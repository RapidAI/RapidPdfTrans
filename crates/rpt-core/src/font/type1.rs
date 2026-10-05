//! Encoding vector stored in a Type 1 font program.
//!
//! pdfTeX embeds Computer Modern as a Type 1 `FontFile` with `/Encoding` left
//! unset and the symbolic flag set. The glyph names then live in the cleartext
//! header (`dup 65 /A put`), not in a PDF `/Differences` array and not in a
//! Unicode cmap. Poppler reads that header. Without it, those glyphs stay
//! unmapped and formula text never reaches the translator.

use super::encoding::{glyph_name_to_unicode, BaseEncoding, SimpleEncoding};
use super::texmath::tex_math_name;

pub fn apply_type1_encoding(simple: &mut SimpleEncoding, font_program: &[u8]) {
    let text = cleartext(font_program);
    let pairs = encoding_pairs(&text);
    if pairs.is_empty() {
        if text.contains("/Encoding StandardEncoding") {
            simple.fill_missing_from_base(BaseEncoding::Standard);
        }
        return;
    }
    for (code, name) in pairs {
        if simple.map(code).is_some() {
            continue;
        }
        if glyph_name_to_unicode(&name).is_some() {
            simple.apply_difference(code, &name);
        } else if let Some(text) = tex_math_name(&name) {
            simple.set_mapped(code, text);
        }
    }
}

fn cleartext(bytes: &[u8]) -> String {
    let ascii = pfb_ascii(bytes).unwrap_or(bytes);
    let end = ascii
        .windows(5)
        .position(|window| window == b"eexec")
        .unwrap_or(ascii.len());
    let end = end.min(512 * 1024);
    String::from_utf8_lossy(&ascii[..end]).into_owned()
}

fn pfb_ascii(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.len() < 6 || bytes[0] != 0x80 || bytes[1] != 0x01 {
        return None;
    }
    let len = u32::from_le_bytes(bytes[2..6].try_into().ok()?) as usize;
    let end = 6usize.saturating_add(len).min(bytes.len());
    Some(&bytes[6..end])
}

fn encoding_pairs(text: &str) -> Vec<(u8, String)> {
    let bytes = text.as_bytes();
    let mut pairs = Vec::new();
    let mut i = 0;
    while i + 6 < bytes.len() {
        if bytes[i..].starts_with(b"dup") && (i == 0 || !is_name_byte(bytes[i - 1])) {
            if let Some((code, name, next)) = parse_dup(&text[i..]) {
                pairs.push((code, name));
                i += next;
                continue;
            }
        }
        i += 1;
    }
    pairs
}

fn parse_dup(text: &str) -> Option<(u8, String, usize)> {
    let rest = text.strip_prefix("dup")?;
    let rest = rest.trim_start_matches([' ', '\t', '\r', '\n']);
    let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let code: u32 = rest[..digits].parse().ok()?;
    if code > 255 {
        return None;
    }
    let rest = rest[digits..].trim_start_matches([' ', '\t', '\r', '\n']);
    let rest = rest.strip_prefix('/')?;
    let name_len = rest.bytes().take_while(|b| is_name_byte(*b)).count();
    if name_len == 0 {
        return None;
    }
    let name = rest[..name_len].to_string();
    let after = rest[name_len..].trim_start_matches([' ', '\t', '\r', '\n']);
    if !after.starts_with("put") {
        return None;
    }
    let consumed = text.len() - after.len();
    Some((code as u8, name, consumed))
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'*')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PdfDocument;
    use lopdf::{dictionary, Document, Object, Stream};

    #[test]
    fn pfa_and_pfb_headers_map_glyph_names() {
        let pfa = b"%!PS-AdobeFont-1.0
/Encoding 256 array
0 1 255 {1 index exch /.notdef put} for
dup 72 /H put
dup 101 /e put
dup 108 /l put
dup 111 /o put
readonly def
currentfile eexec
";
        let mut enc = SimpleEncoding::from_base(BaseEncoding::Standard);
        for code in 0..=255 {
            enc.apply_difference(code, ".notdef");
        }
        apply_type1_encoding(&mut enc, pfa);
        assert_eq!(enc.map(b'H').as_deref(), Some("H"));
        assert_eq!(enc.map(b'e').as_deref(), Some("e"));
        assert!(enc.map(b'A').is_none());

        let mut pfb = vec![0x80, 0x01];
        pfb.extend_from_slice(&(pfa.len() as u32).to_le_bytes());
        pfb.extend_from_slice(pfa);
        pfb.extend_from_slice(&[0x80, 0x02, 0, 0, 0, 0]);
        let mut enc = SimpleEncoding::from_base(BaseEncoding::Standard);
        for code in 0..=255 {
            enc.apply_difference(code, ".notdef");
        }
        apply_type1_encoding(&mut enc, &pfb);
        assert_eq!(enc.map(b'o').as_deref(), Some("o"));
    }

    #[test]
    fn symbolic_type1_without_pdf_encoding_uses_the_font_program() {
        let program = b"%!PS-AdobeFont-1.0
dup 72 /H put
dup 101 /e put
dup 108 /l put
dup 111 /o put
currentfile eexec
";
        let mut doc = Document::with_version("1.4");
        doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        let pages_id = doc.new_object_id();
        let file = doc.add_object(Stream::new(dictionary! {}, program.to_vec()));
        let descriptor = doc.add_object(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "CMR10",
            "Flags" => 4,
            "FontBBox" => vec![0.into(), (-200).into(), 1000.into(), 800.into()],
            "FontFile" => file,
        });
        let font = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "FUIULY+CMR10",
            "FirstChar" => 32,
            "LastChar" => 122,
            "Widths" => (32..=122).map(|_| Object::Integer(600)).collect::<Vec<_>>(),
            "FontDescriptor" => descriptor,
        });
        let mut fonts = lopdf::Dictionary::new();
        fonts.set("F1", font);
        let mut resources = lopdf::Dictionary::new();
        resources.set("Font", fonts);
        let content = doc.add_object(Stream::new(
            lopdf::Dictionary::new(),
            b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET".to_vec(),
        ));
        let page = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content,
            "Resources" => resources,
        });
        doc.set_object(
            pages_id,
            dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page.into()],
                "Count" => 1,
            },
        );
        let catalog = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        let opened = PdfDocument::open_bytes(&bytes).unwrap();
        let extraction = opened.extract();
        assert_eq!(extraction.plain_text().trim(), "Hello");
        assert!(extraction.glyphs.iter().all(|glyph| !glyph.unmapped));
    }

    #[test]
    fn attention_paper_computer_modern_is_mostly_mapped() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/ci/neurips-2017-attention.pdf");
        if !path.exists() {
            return;
        }
        let opened = PdfDocument::open(&path).unwrap();
        let extraction = opened.extract();
        let unmapped = extraction
            .glyphs
            .iter()
            .filter(|glyph| glyph.unmapped)
            .count();
        let ratio = unmapped as f32 / extraction.glyphs.len().max(1) as f32;
        assert!(
            extraction.plain_text().contains("Attention"),
            "plain text missing title: {}",
            extraction
                .plain_text()
                .chars()
                .take(200)
                .collect::<String>()
        );
        assert!(
            ratio < 0.01,
            "unmapped {unmapped} / {} ({ratio:.3})",
            extraction.glyphs.len()
        );
    }
}
