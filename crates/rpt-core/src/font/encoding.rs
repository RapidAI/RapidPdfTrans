//! Simple-font encodings and the Adobe Glyph List.
//!
//! Base encodings (Standard, WinAnsi, MacRoman, MacExpert, PDFDoc, Symbol)
//! map a byte to one Unicode scalar. `/Differences` overrides those bytes by
//! glyph name. Glyph names are resolved through the Adobe Glyph List
//! (`data/glyphlist.txt`, BSD), then `uniXXXX` / `uXXXXXX` forms. `.notdef`
//! and unknown names stay unmapped.

use std::collections::HashMap;
use std::sync::OnceLock;

use super::encoding_tables::{
    MAC_EXPERT_ENCODING, MAC_ROMAN_ENCODING, PDF_DOC_ENCODING, STANDARD_ENCODING, SYMBOL_ENCODING,
    WIN_ANSI_ENCODING,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseEncoding {
    Standard,
    WinAnsi,
    MacRoman,
    MacExpert,
    PdfDoc,
    Symbol,
}

impl BaseEncoding {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "StandardEncoding" => Some(Self::Standard),
            "WinAnsiEncoding" => Some(Self::WinAnsi),
            "MacRomanEncoding" => Some(Self::MacRoman),
            "MacExpertEncoding" => Some(Self::MacExpert),
            "PDFDocEncoding" => Some(Self::PdfDoc),
            "SymbolEncoding" => Some(Self::Symbol),
            _ => None,
        }
    }

    fn table(self) -> &'static [u32; 256] {
        match self {
            Self::Standard => &STANDARD_ENCODING,
            Self::WinAnsi => &WIN_ANSI_ENCODING,
            Self::MacRoman => &MAC_ROMAN_ENCODING,
            Self::MacExpert => &MAC_EXPERT_ENCODING,
            Self::PdfDoc => &PDF_DOC_ENCODING,
            Self::Symbol => &SYMBOL_ENCODING,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SimpleEncoding {
    /// Per-byte Unicode. `None` means the code is unmapped (not dropped).
    codes: Vec<Option<String>>,
}

impl SimpleEncoding {
    pub fn from_base(base: BaseEncoding) -> Self {
        let mut codes = Vec::with_capacity(256);
        for cp in base.table() {
            codes.push(unicode_from_scalar(*cp));
        }
        Self { codes }
    }

    pub fn apply_difference(&mut self, code: u8, glyph_name: &str) {
        self.codes[code as usize] = glyph_name_to_unicode(glyph_name);
    }

    pub fn map(&self, code: u8) -> Option<String> {
        self.codes.get(code as usize).and_then(|c| c.clone())
    }

    /// Fill codes that are still unmapped from a base encoding.
    /// Existing `/Differences` entries are left alone.
    pub fn fill_missing_from_base(&mut self, base: BaseEncoding) {
        let other = Self::from_base(base);
        for (slot, filled) in self.codes.iter_mut().zip(other.codes) {
            if slot.is_none() {
                *slot = filled;
            }
        }
    }
}

fn unicode_from_scalar(cp: u32) -> Option<String> {
    if cp == 0 {
        None
    } else {
        char::from_u32(cp).map(|c| c.to_string())
    }
}

pub fn glyph_name_to_unicode(name: &str) -> Option<String> {
    if name.is_empty() || name == ".notdef" {
        return None;
    }
    if let Some(text) = agl().get(name) {
        return Some(text.clone());
    }
    if let Some(hex) = name.strip_prefix("uni") {
        return unicode_from_hex_groups(hex);
    }
    if let Some(hex) = name.strip_prefix('u') {
        if hex.len() >= 4 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return u32::from_str_radix(hex, 16)
                .ok()
                .and_then(unicode_from_scalar);
        }
    }
    None
}

fn unicode_from_hex_groups(hex: &str) -> Option<String> {
    if hex.is_empty() || !hex.len().is_multiple_of(4) || !hex.chars().all(|c| c.is_ascii_hexdigit())
    {
        return None;
    }
    let mut text = String::new();
    for chunk in hex.as_bytes().chunks(4) {
        let s = std::str::from_utf8(chunk).ok()?;
        let cp = u32::from_str_radix(s, 16).ok()?;
        text.push(char::from_u32(cp)?);
    }
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn agl() -> &'static HashMap<String, String> {
    static AGL: OnceLock<HashMap<String, String>> = OnceLock::new();
    AGL.get_or_init(|| {
        let mut map = HashMap::new();
        for line in include_str!("../../data/glyphlist.txt").lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((name, rest)) = line.split_once(';') else {
                continue;
            };
            let mut text = String::new();
            for part in rest.split_whitespace() {
                if let Ok(cp) = u32::from_str_radix(part, 16) {
                    if let Some(ch) = char::from_u32(cp) {
                        text.push(ch);
                    }
                }
            }
            if !text.is_empty() {
                map.insert(name.to_string(), text);
            }
        }
        map
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winansi_differences_and_uni_names() {
        let enc = SimpleEncoding::from_base(BaseEncoding::WinAnsi);
        assert_eq!(enc.map(b'A').as_deref(), Some("A"));
        assert_eq!(enc.map(0xA9).as_deref(), Some("\u{00A9}"));
        assert_eq!(enc.map(0x80).as_deref(), Some("\u{20AC}"));

        let mut enc = SimpleEncoding::from_base(BaseEncoding::Standard);
        enc.apply_difference(65, "copyright");
        enc.apply_difference(66, "fi");
        enc.apply_difference(67, "TotallyUnknownGlyph");
        enc.apply_difference(68, "uni4E2D");
        assert_eq!(enc.map(65).as_deref(), Some("\u{00A9}"));
        assert_eq!(enc.map(66).as_deref(), Some("\u{FB01}"));
        assert!(enc.map(67).is_none());
        assert_eq!(enc.map(68).as_deref(), Some("中"));

        let mac = SimpleEncoding::from_base(BaseEncoding::MacRoman);
        assert_eq!(mac.map(0x80).as_deref(), Some("\u{00C4}"));
    }
}
