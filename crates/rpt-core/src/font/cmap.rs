//! CMap parser for ToUnicode and CID mappings.
//!
//! Supports `codespacerange`, `bfchar`, `bfrange` (incremental and array form,
//! including multi-character UTF-16BE destinations), `cidchar`, and `cidrange`.
//! Character codes are 1–4 bytes, selected by the codespace (longest match).

use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct CodeSpace {
    pub len: u8,
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug)]
struct UniRange {
    start: u32,
    end: u32,
    dst: Vec<u8>,
}

#[derive(Clone, Debug)]
struct CidRange {
    start: u32,
    end: u32,
    cid0: u32,
}

#[derive(Clone, Debug, Default)]
pub struct CMap {
    pub codespaces: Vec<CodeSpace>,
    exact_uni: HashMap<u32, String>,
    ranges_uni: Vec<UniRange>,
    exact_cid: HashMap<u32, u32>,
    ranges_cid: Vec<CidRange>,
}

impl CMap {
    pub fn parse(data: &[u8]) -> Self {
        let tokens = tokenize(data);
        let mut cmap = CMap::default();
        let mut i = 0;
        while i < tokens.len() {
            match &tokens[i] {
                Tok::Word(w) if w == "begincodespacerange" => {
                    i += 1;
                    while i < tokens.len() {
                        if matches!(&tokens[i], Tok::Word(w) if w == "endcodespacerange") {
                            i += 1;
                            break;
                        }
                        if let (Some(a), Some(b)) = (
                            tokens.get(i).and_then(Tok::hex),
                            tokens.get(i + 1).and_then(Tok::hex),
                        ) {
                            if let (Some(start), Some(end)) = (hex_code(&a), hex_code(&b)) {
                                let len = a.len() as u8;
                                if (1..=4).contains(&len) && b.len() == a.len() {
                                    cmap.codespaces.push(CodeSpace { len, start, end });
                                }
                            }
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                }
                Tok::Word(w) if w == "beginbfchar" => {
                    i += 1;
                    while i < tokens.len() {
                        if matches!(&tokens[i], Tok::Word(w) if w == "endbfchar") {
                            i += 1;
                            break;
                        }
                        if let Some(src) = tokens.get(i).and_then(Tok::hex) {
                            if let Some(dst) = tokens.get(i + 1).and_then(Tok::bytes) {
                                if let Some(code) = hex_code(&src) {
                                    if let Some(text) = decode_utf16_bytes(&dst) {
                                        cmap.exact_uni.insert(code, text);
                                    }
                                }
                                i += 2;
                            } else {
                                i += 1;
                            }
                        } else {
                            i += 1;
                        }
                    }
                }
                Tok::Word(w) if w == "beginbfrange" => {
                    i += 1;
                    while i < tokens.len() {
                        if matches!(&tokens[i], Tok::Word(w) if w == "endbfrange") {
                            i += 1;
                            break;
                        }
                        let Some(src_a) = tokens.get(i).and_then(Tok::hex) else {
                            i += 1;
                            continue;
                        };
                        let Some(src_b) = tokens.get(i + 1).and_then(Tok::hex) else {
                            i += 1;
                            continue;
                        };
                        let Some(start) = hex_code(&src_a) else {
                            i += 1;
                            continue;
                        };
                        let Some(end) = hex_code(&src_b) else {
                            i += 1;
                            continue;
                        };
                        match tokens.get(i + 2) {
                            Some(Tok::Hex(dst)) | Some(Tok::Literal(dst)) => {
                                cmap.ranges_uni.push(UniRange {
                                    start,
                                    end,
                                    dst: dst.clone(),
                                });
                                i += 3;
                            }
                            Some(Tok::Array(items)) => {
                                for (offset, item) in items.iter().enumerate() {
                                    let code = start.saturating_add(offset as u32);
                                    if code > end {
                                        break;
                                    }
                                    if let Some(bytes) = item.bytes() {
                                        if let Some(text) = decode_utf16_bytes(&bytes) {
                                            cmap.exact_uni.insert(code, text);
                                        }
                                    }
                                }
                                i += 3;
                            }
                            _ => i += 1,
                        }
                    }
                }
                Tok::Word(w) if w == "begincidchar" => {
                    i += 1;
                    while i < tokens.len() {
                        if matches!(&tokens[i], Tok::Word(w) if w == "endcidchar") {
                            i += 1;
                            break;
                        }
                        if let (Some(src), Some(Tok::Int(cid))) =
                            (tokens.get(i).and_then(Tok::hex), tokens.get(i + 1))
                        {
                            if let Some(code) = hex_code(&src) {
                                cmap.exact_cid.insert(code, *cid as u32);
                            }
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                }
                Tok::Word(w) if w == "begincidrange" => {
                    i += 1;
                    while i < tokens.len() {
                        if matches!(&tokens[i], Tok::Word(w) if w == "endcidrange") {
                            i += 1;
                            break;
                        }
                        if let (Some(a), Some(b), Some(Tok::Int(cid))) = (
                            tokens.get(i).and_then(Tok::hex),
                            tokens.get(i + 1).and_then(Tok::hex),
                            tokens.get(i + 2),
                        ) {
                            if let (Some(start), Some(end)) = (hex_code(&a), hex_code(&b)) {
                                cmap.ranges_cid.push(CidRange {
                                    start,
                                    end,
                                    cid0: *cid as u32,
                                });
                            }
                            i += 3;
                        } else {
                            i += 1;
                        }
                    }
                }
                _ => i += 1,
            }
        }
        cmap
    }

    pub fn is_empty(&self) -> bool {
        self.exact_uni.is_empty()
            && self.ranges_uni.is_empty()
            && self.exact_cid.is_empty()
            && self.ranges_cid.is_empty()
    }

    pub fn unicode(&self, code: u32) -> Option<String> {
        if let Some(text) = self.exact_uni.get(&code) {
            return Some(text.clone());
        }
        for range in &self.ranges_uni {
            if code >= range.start && code <= range.end {
                let mut dst = range.dst.clone();
                add_offset(&mut dst, code - range.start);
                return decode_utf16_bytes(&dst);
            }
        }
        None
    }

    pub fn cid(&self, code: u32) -> Option<u32> {
        if let Some(cid) = self.exact_cid.get(&code) {
            return Some(*cid);
        }
        for range in &self.ranges_cid {
            if code >= range.start && code <= range.end {
                return Some(range.cid0.saturating_add(code - range.start));
            }
        }
        None
    }

    /// Split a content-stream string into character codes using the codespace.
    /// Unmatched bytes are emitted one at a time so they are never dropped.
    pub fn split<'a>(&self, bytes: &'a [u8]) -> Vec<(u32, &'a [u8])> {
        if self.codespaces.is_empty() {
            return bytes
                .iter()
                .enumerate()
                .map(|(i, b)| (*b as u32, &bytes[i..i + 1]))
                .collect();
        }
        let mut spaces = self.codespaces.clone();
        spaces.sort_by(|a, b| b.len.cmp(&a.len).then(a.start.cmp(&b.start)));
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let mut matched = None;
            for space in &spaces {
                let len = space.len as usize;
                if i + len > bytes.len() {
                    continue;
                }
                let mut code = 0u32;
                for b in &bytes[i..i + len] {
                    code = (code << 8) | *b as u32;
                }
                if code >= space.start && code <= space.end {
                    matched = Some((code, len));
                    break;
                }
            }
            if let Some((code, len)) = matched {
                out.push((code, &bytes[i..i + len]));
                i += len;
            } else {
                out.push((bytes[i] as u32, &bytes[i..i + 1]));
                i += 1;
            }
        }
        out
    }
}

fn add_offset(bytes: &mut [u8], mut offset: u32) {
    for byte in bytes.iter_mut().rev() {
        let sum = *byte as u32 + (offset & 0xff);
        *byte = (sum & 0xff) as u8;
        offset = (offset >> 8) + (sum >> 8);
        if offset == 0 {
            break;
        }
    }
}

fn decode_utf16_bytes(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let bytes = if bytes.len() % 2 == 1 {
        let mut padded = bytes.to_vec();
        padded.insert(0, 0);
        padded
    } else {
        bytes.to_vec()
    };
    let (chunks, _) = bytes.as_chunks::<2>();
    let units: Vec<u16> = chunks.iter().copied().map(u16::from_be_bytes).collect();
    let text: String = char::decode_utf16(units)
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect();
    if text.is_empty() || text.chars().all(|c| c == '\u{FFFD}') {
        None
    } else {
        Some(text)
    }
}

fn hex_code(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || bytes.len() > 4 {
        return None;
    }
    let mut code = 0u32;
    for b in bytes {
        code = (code << 8) | *b as u32;
    }
    Some(code)
}

#[derive(Clone, Debug)]
enum Tok {
    Hex(Vec<u8>),
    Literal(Vec<u8>),
    Word(String),
    Int(i64),
    Array(Vec<Tok>),
}

impl Tok {
    fn hex(&self) -> Option<Vec<u8>> {
        match self {
            Tok::Hex(b) | Tok::Literal(b) => Some(b.clone()),
            _ => None,
        }
    }

    fn bytes(&self) -> Option<Vec<u8>> {
        self.hex()
    }
}

fn tokenize(data: &[u8]) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b == b'%' {
            while i < data.len() && data[i] != b'\n' && data[i] != b'\r' {
                i += 1;
            }
            continue;
        }
        if b == b'<' {
            let start = i + 1;
            i += 1;
            while i < data.len() && data[i] != b'>' {
                i += 1;
            }
            let hex = parse_hex(&data[start..i]);
            if i < data.len() {
                i += 1;
            }
            out.push(Tok::Hex(hex));
            continue;
        }
        if b == b'(' {
            i += 1;
            let mut buf = Vec::new();
            let mut depth = 1;
            while i < data.len() && depth > 0 {
                match data[i] {
                    b'\\' if i + 1 < data.len() => {
                        buf.push(data[i + 1]);
                        i += 2;
                    }
                    b'(' => {
                        depth += 1;
                        buf.push(b'(');
                        i += 1;
                    }
                    b')' => {
                        depth -= 1;
                        if depth > 0 {
                            buf.push(b')');
                        }
                        i += 1;
                    }
                    c => {
                        buf.push(c);
                        i += 1;
                    }
                }
            }
            out.push(Tok::Literal(buf));
            continue;
        }
        if b == b'[' {
            i += 1;
            let mut depth = 1;
            let start = i;
            while i < data.len() && depth > 0 {
                match data[i] {
                    b'[' => depth += 1,
                    b']' => depth -= 1,
                    _ => {}
                }
                if depth > 0 {
                    i += 1;
                }
            }
            let inner = tokenize(&data[start..i]);
            if i < data.len() {
                i += 1;
            }
            out.push(Tok::Array(inner));
            continue;
        }
        if b == b'/' {
            i += 1;
            let start = i;
            while i < data.len() && !is_cmap_delim(data[i]) {
                i += 1;
            }
            let name = String::from_utf8_lossy(&data[start..i]).into_owned();
            out.push(Tok::Word(name));
            continue;
        }
        if b == b'+' || b == b'-' || b.is_ascii_digit() {
            let start = i;
            if b == b'+' || b == b'-' {
                i += 1;
            }
            while i < data.len() && data[i].is_ascii_digit() {
                i += 1;
            }
            if let Ok(n) = std::str::from_utf8(&data[start..i])
                .unwrap_or("")
                .parse::<i64>()
            {
                out.push(Tok::Int(n));
            }
            continue;
        }
        let start = i;
        while i < data.len() && !is_cmap_delim(data[i]) && data[i] != b'<' && data[i] != b'[' {
            i += 1;
        }
        if start == i {
            i += 1;
            continue;
        }
        let word = String::from_utf8_lossy(&data[start..i]).into_owned();
        out.push(Tok::Word(word));
    }
    out
}

fn is_cmap_delim(b: u8) -> bool {
    b.is_ascii_whitespace()
        || matches!(
            b,
            b'<' | b'>' | b'[' | b']' | b'(' | b')' | b'{' | b'}' | b'/' | b'%'
        )
}

fn parse_hex(data: &[u8]) -> Vec<u8> {
    let mut nibbles = Vec::new();
    for b in data {
        let v = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => continue,
        };
        nibbles.push(v);
    }
    if nibbles.len() % 2 == 1 {
        nibbles.push(0);
    }
    nibbles.chunks(2).map(|c| (c[0] << 4) | c[1]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bfchar_bfrange_and_array_form() {
        let src = br#"
            1 begincodespacerange
            <0000> <FFFF>
            endcodespacerange
            2 beginbfchar
            <0001> <0048>
            <0002> <00660069>
            endbfchar
            2 beginbfrange
            <0010> <0012> <0041>
            <0020> <0021> [<0043> <00440045>]
            endbfrange
        "#;
        let map = CMap::parse(src);
        assert_eq!(map.unicode(1).as_deref(), Some("H"));
        assert_eq!(map.unicode(2).as_deref(), Some("fi"));
        assert_eq!(map.unicode(0x10).as_deref(), Some("A"));
        assert_eq!(map.unicode(0x11).as_deref(), Some("B"));
        assert_eq!(map.unicode(0x12).as_deref(), Some("C"));
        assert_eq!(map.unicode(0x20).as_deref(), Some("C"));
        assert_eq!(map.unicode(0x21).as_deref(), Some("DE"));
        let bytes = [0x00, 0x01, 0x00, 0x02];
        let parts = map.split(&bytes);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].0, 1);
        assert_eq!(parts[1].0, 2);
    }

    #[test]
    fn cid_range() {
        let src = br#"
            1 begincodespacerange <00> <FF> endcodespacerange
            1 begincidrange
            <41> <43> 10
            endcidrange
        "#;
        let map = CMap::parse(src);
        assert_eq!(map.cid(0x41), Some(10));
        assert_eq!(map.cid(0x43), Some(12));
    }
}
