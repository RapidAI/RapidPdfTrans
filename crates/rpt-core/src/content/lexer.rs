//! Content-stream lexer with byte offsets.
//!
//! Offsets cover each operator together with its operands so a later milestone
//! can delete exactly that text-showing instruction. Inline images (`BI`/`ID`/`EI`)
//! are consumed as a single non-text operator and never split the following
//! content, even when the image dictionary is incomplete.

use std::collections::HashSet;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct ContentOp {
    pub operands: Vec<Operand>,
    pub operator: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum Operand {
    Number(f32),
    Name(String),
    String(Vec<u8>),
    Array(Vec<Operand>),
    Dict(Vec<(String, Operand)>),
    Bool(bool),
    Null,
}

#[derive(Clone, Debug, Default)]
pub struct LexResult {
    pub ops: Vec<ContentOp>,
    pub diagnostics: Vec<String>,
}

pub fn lex(data: &[u8]) -> LexResult {
    let mut out = LexResult::default();
    let mut i = 0;
    let mut operands = Vec::new();
    let mut operand_start: Option<usize> = None;
    while i < data.len() {
        skip_ws_and_comments(data, &mut i);
        if i >= data.len() {
            break;
        }
        if let Some(keyword) = peek_keyword(data, i) {
            if is_operator(keyword) {
                let op_start = i;
                i += keyword.len();
                let start = operand_start.unwrap_or(op_start);
                if keyword == "BI" {
                    if let Err(msg) = consume_inline_image(data, &mut i) {
                        out.diagnostics.push(msg);
                    }
                    out.ops.push(ContentOp {
                        operands: Vec::new(),
                        operator: "BI".into(),
                        start,
                        end: i,
                    });
                } else {
                    out.ops.push(ContentOp {
                        operands: std::mem::take(&mut operands),
                        operator: keyword.to_string(),
                        start,
                        end: i,
                    });
                }
                operand_start = None;
                continue;
            }
        }
        let value_start = i;
        match read_operand(data, &mut i) {
            Ok(operand) => {
                if operand_start.is_none() {
                    operand_start = Some(value_start);
                }
                operands.push(operand);
            }
            Err(msg) => {
                out.diagnostics.push(msg);
                if i == value_start && i < data.len() {
                    i += 1;
                }
            }
        }
    }
    if !operands.is_empty() {
        out.diagnostics
            .push(format!("trailing operands at byte {i}"));
    }
    out
}

fn is_operator(name: &str) -> bool {
    static OPS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    OPS.get_or_init(|| {
        [
            "q", "Q", "cm", "w", "J", "j", "M", "d", "ri", "i", "gs", "CS", "cs", "SC", "SCN",
            "sc", "scn", "G", "g", "RG", "rg", "K", "k", "m", "l", "c", "v", "y", "h", "re", "S",
            "s", "f", "F", "f*", "B", "B*", "b", "b*", "n", "W", "W*", "BT", "ET", "Tc", "Tw",
            "Tz", "TL", "Tf", "Tr", "Ts", "Td", "TD", "Tm", "T*", "Tj", "TJ", "'", "\"", "Do",
            "BMC", "BDC", "EMC", "MP", "DP", "BX", "EX", "d0", "d1", "BI", "ID", "EI", "sh",
        ]
        .into_iter()
        .collect()
    })
    .contains(name)
}

fn peek_keyword(data: &[u8], i: usize) -> Option<&str> {
    if i >= data.len() || is_number_start(data, i) {
        return None;
    }
    let b = data[i];
    if is_delim(b) || b.is_ascii_whitespace() {
        return None;
    }
    let end = scan_regular(data, i);
    std::str::from_utf8(&data[i..end]).ok()
}

fn scan_regular(data: &[u8], i: usize) -> usize {
    let mut j = i;
    while j < data.len() && !data[j].is_ascii_whitespace() && !is_delim(data[j]) {
        j += 1;
    }
    j
}

fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn is_ws(b: u8) -> bool {
    matches!(b, 0x00 | 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

fn skip_ws_and_comments(data: &[u8], i: &mut usize) {
    while *i < data.len() {
        if is_ws(data[*i]) {
            *i += 1;
            continue;
        }
        if data[*i] == b'%' {
            *i += 1;
            while *i < data.len() && data[*i] != b'\n' && data[*i] != b'\r' {
                *i += 1;
            }
            continue;
        }
        break;
    }
}

fn is_number_start(data: &[u8], i: usize) -> bool {
    let b = data[i];
    if b.is_ascii_digit() || b == b'.' {
        return true;
    }
    if (b == b'+' || b == b'-') && i + 1 < data.len() {
        let n = data[i + 1];
        return n.is_ascii_digit() || n == b'.';
    }
    false
}

fn read_operand(data: &[u8], i: &mut usize) -> Result<Operand, String> {
    if *i >= data.len() {
        return Err("unexpected end of content stream".into());
    }
    let b = data[*i];
    if is_number_start(data, *i) {
        return Ok(Operand::Number(read_number(data, i)?));
    }
    match b {
        b'/' => Ok(Operand::Name(read_name(data, i))),
        b'(' => Ok(Operand::String(read_literal(data, i)?)),
        b'<' => {
            if *i + 1 < data.len() && data[*i + 1] == b'<' {
                Ok(Operand::Dict(read_dict(data, i)?))
            } else {
                Ok(Operand::String(read_hex(data, i)?))
            }
        }
        b'[' => Ok(Operand::Array(read_array(data, i)?)),
        _ => {
            let end = scan_regular(data, *i);
            let word = String::from_utf8_lossy(&data[*i..end]).into_owned();
            *i = end.max(*i + 1);
            match word.as_str() {
                "true" => Ok(Operand::Bool(true)),
                "false" => Ok(Operand::Bool(false)),
                "null" => Ok(Operand::Null),
                other => Err(format!("unexpected token '{other}'")),
            }
        }
    }
}

fn read_number(data: &[u8], i: &mut usize) -> Result<f32, String> {
    let start = *i;
    if data[*i] == b'+' || data[*i] == b'-' {
        *i += 1;
    }
    while *i < data.len() && data[*i].is_ascii_digit() {
        *i += 1;
    }
    if *i < data.len() && data[*i] == b'.' {
        *i += 1;
        while *i < data.len() && data[*i].is_ascii_digit() {
            *i += 1;
        }
    }
    let text = std::str::from_utf8(&data[start..*i]).unwrap_or("");
    text.parse::<f32>()
        .map_err(|_| format!("bad number '{text}'"))
}

fn read_name(data: &[u8], i: &mut usize) -> String {
    *i += 1; // skip '/'
    let mut raw = Vec::new();
    while *i < data.len() && !data[*i].is_ascii_whitespace() && !is_delim(data[*i]) {
        if data[*i] == b'#' && *i + 2 < data.len() && is_hex(data[*i + 1]) && is_hex(data[*i + 2]) {
            raw.push((hex_val(data[*i + 1]) << 4) | hex_val(data[*i + 2]));
            *i += 3;
        } else {
            raw.push(data[*i]);
            *i += 1;
        }
    }
    String::from_utf8_lossy(&raw).into_owned()
}

fn read_literal(data: &[u8], i: &mut usize) -> Result<Vec<u8>, String> {
    *i += 1;
    let mut out = Vec::new();
    let mut depth = 1i32;
    while *i < data.len() && depth > 0 {
        let b = data[*i];
        if b == b'\\' {
            *i += 1;
            if *i >= data.len() {
                break;
            }
            match data[*i] {
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b't' => out.push(b'\t'),
                b'b' => out.push(0x08),
                b'f' => out.push(0x0C),
                b'(' => out.push(b'('),
                b')' => out.push(b')'),
                b'\\' => out.push(b'\\'),
                b'\n' => {}
                b'\r' => {
                    if *i + 1 < data.len() && data[*i + 1] == b'\n' {
                        *i += 1;
                    }
                }
                c if (b'0'..=b'7').contains(&c) => {
                    let mut val = (c - b'0') as u16;
                    *i += 1;
                    for _ in 0..2 {
                        if *i < data.len() && (b'0'..=b'7').contains(&data[*i]) {
                            val = (val << 3) | (data[*i] - b'0') as u16;
                            *i += 1;
                        } else {
                            break;
                        }
                    }
                    out.push((val & 0xFF) as u8);
                    continue;
                }
                c => out.push(c),
            }
            *i += 1;
            continue;
        }
        if b == b'(' {
            depth += 1;
            out.push(b);
            *i += 1;
            continue;
        }
        if b == b')' {
            depth -= 1;
            if depth > 0 {
                out.push(b);
            }
            *i += 1;
            continue;
        }
        out.push(b);
        *i += 1;
    }
    if depth != 0 {
        return Err("unterminated literal string".into());
    }
    Ok(out)
}

fn read_hex(data: &[u8], i: &mut usize) -> Result<Vec<u8>, String> {
    *i += 1;
    let mut nibbles = Vec::new();
    while *i < data.len() && data[*i] != b'>' {
        let b = data[*i];
        *i += 1;
        if is_ws(b) {
            continue;
        }
        if !is_hex(b) {
            return Err("bad hex string".into());
        }
        nibbles.push(hex_val(b));
    }
    if *i >= data.len() {
        return Err("unterminated hex string".into());
    }
    *i += 1;
    if nibbles.len() % 2 == 1 {
        nibbles.push(0);
    }
    Ok(nibbles.chunks(2).map(|c| (c[0] << 4) | c[1]).collect())
}

fn read_array(data: &[u8], i: &mut usize) -> Result<Vec<Operand>, String> {
    *i += 1;
    let mut items = Vec::new();
    loop {
        skip_ws_and_comments(data, i);
        if *i >= data.len() {
            return Err("unterminated array".into());
        }
        if data[*i] == b']' {
            *i += 1;
            break;
        }
        items.push(read_operand(data, i)?);
    }
    Ok(items)
}

fn read_dict(data: &[u8], i: &mut usize) -> Result<Vec<(String, Operand)>, String> {
    *i += 2;
    let mut items = Vec::new();
    loop {
        skip_ws_and_comments(data, i);
        if *i >= data.len() {
            return Err("unterminated dictionary".into());
        }
        if data[*i] == b'>' && *i + 1 < data.len() && data[*i + 1] == b'>' {
            *i += 2;
            break;
        }
        if data[*i] != b'/' {
            return Err("dictionary key is not a name".into());
        }
        let key = read_name(data, i);
        skip_ws_and_comments(data, i);
        let value = read_operand(data, i)?;
        items.push((key, value));
    }
    Ok(items)
}

fn consume_inline_image(data: &[u8], i: &mut usize) -> Result<(), String> {
    let mut dict: Vec<(String, Operand)> = Vec::new();
    loop {
        skip_ws_and_comments(data, i);
        if *i >= data.len() {
            return Err("unterminated inline image".into());
        }
        if peek_keyword(data, *i) == Some("ID") {
            *i += 2;
            break;
        }
        if data[*i] != b'/' {
            return Err("inline image expected a name or ID".into());
        }
        let key = read_name(data, i);
        skip_ws_and_comments(data, i);
        let value = read_operand(data, i)?;
        dict.push((key, value));
    }
    if *i < data.len() && is_ws(data[*i]) {
        *i += 1;
    }
    if let Some(len) = inline_image_length(&dict) {
        if *i + len > data.len() {
            *i = data.len();
            return Err("truncated inline image".into());
        }
        *i += len;
        skip_ws_and_comments(data, i);
        if data.get(*i..).is_some_and(|s| s.starts_with(b"EI")) {
            *i += 2;
            return Ok(());
        }
    }
    scan_ei(data, i)
}

fn inline_image_length(dict: &[(String, Operand)]) -> Option<usize> {
    let get = |name: &str| dict.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    // Filtered images are ASCII-encoded; scanning for EI is safer.
    if get("F").is_some() || get("Filter").is_some() {
        return None;
    }
    let width = number_operand(get("W")?)? as usize;
    let height = number_operand(get("H")?)? as usize;
    let bpc = get("BPC").and_then(number_operand).unwrap_or(8.0) as usize;
    let comps = match get("CS")? {
        Operand::Name(n) => match n.as_str() {
            "G" | "DeviceGray" | "Gray" => 1,
            "RGB" | "DeviceRGB" => 3,
            "CMYK" | "DeviceCMYK" => 4,
            _ => return None,
        },
        _ => return None,
    };
    if bpc == 0 {
        return None;
    }
    let row = width.saturating_mul(comps).saturating_mul(bpc).div_ceil(8);
    Some(row.saturating_mul(height))
}

fn number_operand(op: &Operand) -> Option<f32> {
    match op {
        Operand::Number(n) => Some(*n),
        _ => None,
    }
}

fn scan_ei(data: &[u8], i: &mut usize) -> Result<(), String> {
    let start = *i;
    while *i + 1 < data.len() {
        if is_ws(data[*i]) && data.get(*i + 1..).is_some_and(|s| s.starts_with(b"EI")) {
            let after = *i + 3;
            if after >= data.len() || is_ws(data[after]) || is_delim(data[after]) {
                *i = after;
                return Ok(());
            }
        }
        *i += 1;
    }
    *i = data.len();
    Err(format!("inline image EI not found after byte {start}"))
}

fn is_hex(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_operator_byte_range_and_escapes() {
        let data = b"BT /F1 12 Tf 100 200 Td (H\\105llo) Tj ET";
        let lexed = lex(data);
        assert!(lexed.diagnostics.is_empty(), "{:?}", lexed.diagnostics);
        let tj = lexed.ops.iter().find(|op| op.operator == "Tj").unwrap();
        let Operand::String(s) = &tj.operands[0] else {
            panic!("string");
        };
        assert_eq!(s, b"HEllo");
        assert_eq!(&data[tj.start..tj.end], b"(H\\105llo) Tj");
    }

    #[test]
    fn tj_array_and_inline_image() {
        let mut data = Vec::from(&b"BT (Before) Tj ET\n"[..]);
        data.extend_from_slice(b"BI /W 2 /H 2 /CS /G /BPC 8 ID ");
        data.extend_from_slice(&[1, 2, 3, 4]);
        data.extend_from_slice(b"\nEI\nBT (After) Tj ET");
        let lexed = lex(&data);
        assert!(lexed.diagnostics.is_empty(), "{:?}", lexed.diagnostics);
        let strings: Vec<_> = lexed
            .ops
            .iter()
            .filter(|op| op.operator == "Tj")
            .map(|op| match &op.operands[0] {
                Operand::String(s) => String::from_utf8_lossy(s).into_owned(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(strings, vec!["Before".to_string(), "After".to_string()]);
        assert!(lexed.ops.iter().any(|op| op.operator == "BI"));
        let tj = lexed.ops.iter().find(|op| op.operator == "TJ");
        let _ = tj;
        let kern = lex(b"[ (A) -120.5 (B) ] TJ");
        let op = &kern.ops[0];
        assert_eq!(op.operator, "TJ");
        let Operand::Array(items) = &op.operands[0] else {
            panic!("array");
        };
        assert!(matches!(items[1], Operand::Number(n) if (n + 120.5).abs() < 1e-3));
    }
}
