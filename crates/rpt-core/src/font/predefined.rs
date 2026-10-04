//! Predefined CJK CMaps.
//!
//! Unicode comes from the encoding the CMap name implies (UTF-16 code points,
//! GBK, Shift-JIS, Big5, EUC-KR, EUC-JP). Official Adobe-GB1/Japan1/Korea1/CNS1
//! CID tables are not embedded: for non-Identity CMaps the character code is
//! used as the CID when looking up `/W`, and `/DW` applies when that code is
//! absent. Identity-H/V uses the numeric code as the CID, which matches `/W`.

use encoding_rs::Encoding;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitKind {
    Utf16,
    Utf32,
    Gbk,
    GbEuc,
    ShiftJis,
    EucKr,
    Big5,
    EucJp,
}

#[derive(Clone, Copy, Debug)]
pub enum Predefined {
    Utf16 {
        vertical: bool,
    },
    Utf32 {
        vertical: bool,
    },
    Legacy {
        encoding: &'static Encoding,
        split: SplitKind,
        vertical: bool,
    },
}

impl Predefined {
    pub fn vertical(self) -> bool {
        match self {
            Self::Utf16 { vertical } | Self::Utf32 { vertical } => vertical,
            Self::Legacy { vertical, .. } => vertical,
        }
    }

    pub fn split_bytes(self, bytes: &[u8]) -> Vec<(u32, &[u8])> {
        let kind = match self {
            Self::Utf16 { .. } => SplitKind::Utf16,
            Self::Utf32 { .. } => SplitKind::Utf32,
            Self::Legacy { split, .. } => split,
        };
        split_by(kind, bytes)
    }

    pub fn decode(self, code_bytes: &[u8]) -> Option<String> {
        match self {
            Self::Utf16 { .. } => decode_utf16_unit(code_bytes),
            Self::Utf32 { .. } => {
                if code_bytes.len() != 4 {
                    return None;
                }
                let u = u32::from_be_bytes([
                    code_bytes[0],
                    code_bytes[1],
                    code_bytes[2],
                    code_bytes[3],
                ]);
                char::from_u32(u).map(|c| c.to_string())
            }
            Self::Legacy { encoding, .. } => {
                let (text, _, had_errors) = encoding.decode(code_bytes);
                if had_errors || text.is_empty() {
                    None
                } else {
                    Some(text.into_owned())
                }
            }
        }
    }
}

pub fn from_name(name: &str) -> Option<Predefined> {
    let vertical = name.ends_with("-V");
    let base = name
        .strip_suffix("-H")
        .or_else(|| name.strip_suffix("-V"))
        .unwrap_or(name);
    let utf16 = matches!(
        base,
        "UniGB-UCS2"
            | "UniGB-UTF16"
            | "UniJIS-UCS2"
            | "UniJIS-UTF16"
            | "UniKS-UCS2"
            | "UniKS-UTF16"
            | "UniCNS-UCS2"
            | "UniCNS-UTF16"
    );
    if utf16 {
        return Some(Predefined::Utf16 { vertical });
    }
    if matches!(
        base,
        "UniGB-UTF32" | "UniJIS-UTF32" | "UniKS-UTF32" | "UniCNS-UTF32"
    ) {
        return Some(Predefined::Utf32 { vertical });
    }
    let (encoding, split) = match base {
        "GBK-EUC" | "GBKp-EUC" | "GBK2K" => (encoding_rs::GBK, SplitKind::Gbk),
        "GB-EUC" | "GBpc-EUC" => (encoding_rs::GBK, SplitKind::GbEuc),
        "90ms-RKSJ" | "90msp-RKSJ" | "90pv-RKSJ" | "Add-RKSJ" | "Ext-RKSJ" => {
            (encoding_rs::SHIFT_JIS, SplitKind::ShiftJis)
        }
        "KSCms-UHC" | "KSCms-UHC-HW" | "KSCpc-EUC" => (encoding_rs::EUC_KR, SplitKind::EucKr),
        "ETen-B5" | "ETenms-B5" | "B5pc" | "HKscs-B5" => (encoding_rs::BIG5, SplitKind::Big5),
        "EUC" => (encoding_rs::EUC_JP, SplitKind::EucJp),
        _ => return None,
    };
    Some(Predefined::Legacy {
        encoding,
        split,
        vertical,
    })
}

pub fn split_by(kind: SplitKind, bytes: &[u8]) -> Vec<(u32, &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let len = match kind {
            SplitKind::Utf16 => {
                if i + 1 < bytes.len() {
                    2
                } else {
                    1
                }
            }
            SplitKind::Utf32 => {
                if i + 3 < bytes.len() {
                    4
                } else {
                    1
                }
            }
            SplitKind::Gbk | SplitKind::EucKr | SplitKind::Big5 => {
                let b = bytes[i];
                if (0x81..=0xFE).contains(&b) && i + 1 < bytes.len() {
                    2
                } else {
                    1
                }
            }
            SplitKind::GbEuc => {
                let b = bytes[i];
                if (0xA1..=0xFE).contains(&b) && i + 1 < bytes.len() {
                    2
                } else {
                    1
                }
            }
            SplitKind::ShiftJis => {
                let b = bytes[i];
                if ((0x81..=0x9F).contains(&b) || (0xE0..=0xFC).contains(&b)) && i + 1 < bytes.len()
                {
                    2
                } else {
                    1
                }
            }
            SplitKind::EucJp => {
                let b = bytes[i];
                if b == 0x8E && i + 1 < bytes.len() {
                    2
                } else if b == 0x8F && i + 2 < bytes.len() {
                    3
                } else if (0xA1..=0xFE).contains(&b) && i + 1 < bytes.len() {
                    2
                } else {
                    1
                }
            }
        };
        let slice = &bytes[i..i + len];
        let mut code = 0u32;
        for b in slice {
            code = (code << 8) | *b as u32;
        }
        out.push((code, slice));
        i += len;
    }
    out
}

fn decode_utf16_unit(code_bytes: &[u8]) -> Option<String> {
    if code_bytes.len() == 2 {
        let u = u16::from_be_bytes([code_bytes[0], code_bytes[1]]);
        if (0xD800..=0xDFFF).contains(&u) {
            return None;
        }
        char::from_u32(u as u32).map(|c| c.to_string())
    } else if code_bytes.len() == 4 {
        let hi = u16::from_be_bytes([code_bytes[0], code_bytes[1]]);
        let lo = u16::from_be_bytes([code_bytes[2], code_bytes[3]]);
        char::decode_utf16([hi, lo])
            .next()
            .and_then(|r| r.ok())
            .map(|c| c.to_string())
    } else if code_bytes.len() == 1 {
        char::from_u32(code_bytes[0] as u32).map(|c| c.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gbk_and_ucs2_and_shiftjis() {
        let gbk = from_name("GBK-EUC-H").unwrap();
        // 中 is GBK D6 D0.
        let bytes = [0xD6, 0xD0];
        let parts = gbk.split_bytes(&bytes);
        assert_eq!(parts.len(), 1);
        assert_eq!(gbk.decode(parts[0].1).as_deref(), Some("中"));

        let ucs = from_name("UniGB-UCS2-H").unwrap();
        assert_eq!(ucs.decode(&[0x4E, 0x2D]).as_deref(), Some("中"));

        let sjis = from_name("90ms-RKSJ-H").unwrap();
        // あ is Shift-JIS 82 A0.
        assert_eq!(sjis.decode(&[0x82, 0xA0]).as_deref(), Some("あ"));
        assert!(from_name("Identity-H").is_none());
        assert!(from_name("UniJIS-UCS2-V").unwrap().vertical());
    }
}
