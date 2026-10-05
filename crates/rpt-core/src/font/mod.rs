//! Font decoding.
//!
//! Unicode fallback order, per character code:
//! 1. ToUnicode CMap
//! 2. predefined CJK CMap (when `/Encoding` names one)
//! 3. simple encoding (Standard / WinAnsi / MacRoman / PDFDoc / Symbol + `/Differences`),
//!    then glyph names from a Type 1 `FontFile` cleartext encoding for codes still unmapped
//! 4. embedded TrueType/OpenType `cmap` (GID → Unicode, then code → Unicode)
//!
//! A code that survives all four steps is returned with `unicode: None` and
//! must still be recorded by the interpreter.

mod cmap;
mod encoding;
mod encoding_tables;
mod predefined;
mod ttf;
mod type1;
mod widths;

use std::collections::HashMap;

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::geom::Matrix;
use crate::pdfutil::{array_of, as_f32, as_name, deref, dict_of, object_id_string, stream_bytes};
use crate::resources::Resources;

pub use cmap::CMap;
pub use predefined::Predefined;
pub use ttf::{minimal_ttf, FontCmap};

use encoding::{glyph_name_to_unicode, BaseEncoding, SimpleEncoding};
use widths::{VerticalMap, WidthMap};

const STREAM_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontKind {
    Type1,
    TrueType,
    Type0,
    Type3,
    Other,
}

#[derive(Clone, Debug)]
pub enum CidToGid {
    None,
    Identity,
    Map(Vec<u16>),
}

#[derive(Clone, Debug)]
pub struct Type3 {
    pub matrix: Matrix,
    /// Character code → char-proc content stream bytes.
    pub procs: HashMap<u32, Vec<u8>>,
    pub resources: Resources,
}

#[derive(Clone, Debug)]
pub struct DecodedGlyph {
    pub code: u32,
    pub code_bytes: Vec<u8>,
    pub unicode: Option<String>,
    pub gid: Option<u32>,
    /// Horizontal displacement in text space (already through FontMatrix).
    pub w0: f32,
    pub vertical_metrics: Option<VerticalGlyph>,
}

#[derive(Clone, Copy, Debug)]
pub struct VerticalGlyph {
    /// Vertical displacement in text space.
    pub w1: f32,
    /// Position-vector components in text space.
    pub vx: f32,
    pub vy: f32,
}

#[derive(Clone, Debug)]
pub struct Font {
    pub resource_name: String,
    pub base_name: String,
    #[allow(dead_code)]
    pub kind: FontKind,
    pub object_id: Option<ObjectId>,
    pub vertical: bool,
    pub font_matrix: Matrix,
    widths: WidthMap,
    vertical_widths: VerticalMap,
    to_unicode: Option<CMap>,
    cid_map: Option<CMap>,
    predefined: Option<Predefined>,
    simple: Option<SimpleEncoding>,
    identity_two_byte: bool,
    is_cid: bool,
    cid_to_gid: CidToGid,
    cmap: FontCmap,
    pub type3: Option<Type3>,
}

impl Font {
    pub fn object_id_string(&self) -> Option<String> {
        self.object_id.map(object_id_string)
    }

    pub fn decode(&self, bytes: &[u8]) -> Vec<DecodedGlyph> {
        let parts = self.split(bytes);
        let mut out = Vec::with_capacity(parts.len());
        let mut i = 0;
        while i < parts.len() {
            let (code, slice) = parts[i];
            // Merge a UTF-16 surrogate pair into one glyph so it is not dropped.
            if let Some(Predefined::Utf16 { .. }) = self.predefined {
                if slice.len() == 2 && i + 1 < parts.len() && parts[i + 1].1.len() == 2 {
                    let hi = u16::from_be_bytes([slice[0], slice[1]]);
                    let lo_b = parts[i + 1].1;
                    let lo = u16::from_be_bytes([lo_b[0], lo_b[1]]);
                    if (0xD800..=0xDBFF).contains(&hi) && (0xDC00..=0xDFFF).contains(&lo) {
                        let mut code_bytes = slice.to_vec();
                        code_bytes.extend_from_slice(lo_b);
                        let combined = ((hi as u32) << 16) | lo as u32;
                        out.push(self.finish(combined, code_bytes));
                        i += 2;
                        continue;
                    }
                }
            }
            out.push(self.finish(code, slice.to_vec()));
            i += 1;
        }
        out
    }

    fn split<'a>(&self, bytes: &'a [u8]) -> Vec<(u32, &'a [u8])> {
        if let Some(map) = self
            .to_unicode
            .as_ref()
            .filter(|m| !m.codespaces.is_empty())
        {
            return map.split(bytes);
        }
        if let Some(map) = self.cid_map.as_ref().filter(|m| !m.codespaces.is_empty()) {
            return map.split(bytes);
        }
        if let Some(pre) = self.predefined {
            return pre.split_bytes(bytes);
        }
        if self.identity_two_byte {
            return split_fixed(bytes, 2);
        }
        bytes
            .iter()
            .enumerate()
            .map(|(i, b)| (*b as u32, &bytes[i..i + 1]))
            .collect()
    }

    fn finish(&self, code: u32, code_bytes: Vec<u8>) -> DecodedGlyph {
        let cid = if let Some(map) = &self.cid_map {
            map.cid(code).unwrap_or(code)
        } else {
            code
        };
        let mut unicode = self.to_unicode.as_ref().and_then(|m| m.unicode(code));
        if unicode.is_none() {
            if let Some(pre) = self.predefined {
                unicode = pre.decode(&code_bytes);
            }
        }
        if unicode.is_none() {
            if code_bytes.len() == 1 {
                if let Some(simple) = &self.simple {
                    unicode = simple.map(code_bytes[0]);
                }
            } else if let Some(simple) = &self.simple {
                if code <= 0xFF {
                    unicode = simple.map(code as u8);
                }
            }
        }
        let mut gid = match &self.cid_to_gid {
            CidToGid::Identity => Some(cid),
            CidToGid::Map(table) => table
                .get(cid as usize)
                .copied()
                .map(|g| g as u32)
                .filter(|g| *g != 0),
            CidToGid::None => None,
        };
        if unicode.is_none() {
            if let Some(g) = gid {
                if let Some(text) = self.cmap.gid_to_unicode.get(&g) {
                    unicode = Some(text.clone());
                }
            }
        }
        if unicode.is_none() {
            if let Some(g) = self.cmap.unicode_to_gid.get(&code) {
                if let Some(ch) = char::from_u32(code) {
                    unicode = Some(ch.to_string());
                    gid = Some(*g);
                }
            }
        }
        if gid.is_none() {
            if let Some(text) = &unicode {
                if let Some(ch) = text.chars().next() {
                    if let Some(g) = self.cmap.unicode_to_gid.get(&(ch as u32)) {
                        gid = Some(*g);
                    }
                }
            }
        }
        let width_units = self.widths.get(if self.is_cid { cid } else { code });
        let (w0x, _) = self.font_matrix.transform_vector(width_units, 0.0);
        let vertical_metrics = if self.vertical {
            let vm = self.vertical_widths.get(cid, width_units);
            let (_, w1) = self.font_matrix.transform_vector(0.0, vm.w1);
            let (vx, _) = self.font_matrix.transform_vector(vm.vx, 0.0);
            let (_, vy) = self.font_matrix.transform_vector(0.0, vm.vy);
            Some(VerticalGlyph { w1, vx, vy })
        } else {
            None
        };
        DecodedGlyph {
            code,
            code_bytes,
            unicode,
            gid,
            w0: w0x,
            vertical_metrics,
        }
    }
}

fn split_fixed(bytes: &[u8], width: usize) -> Vec<(u32, &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let len = if i + width <= bytes.len() { width } else { 1 };
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

pub fn load_font(doc: &Document, resource_name: &str, obj: &Object) -> Font {
    let object_id = match obj {
        Object::Reference(id) => Some(*id),
        _ => None,
    };
    let Some(dict) = dict_of(doc, obj) else {
        return missing(resource_name, object_id);
    };
    let subtype = dict
        .get(b"Subtype")
        .ok()
        .and_then(|o| as_name(deref(doc, o)));
    let kind = match subtype.as_deref() {
        Some("Type0") => FontKind::Type0,
        Some("Type1") | Some("MMType1") => FontKind::Type1,
        Some("TrueType") => FontKind::TrueType,
        Some("Type3") => FontKind::Type3,
        _ => FontKind::Other,
    };
    let base_name = dict
        .get(b"BaseFont")
        .ok()
        .and_then(|o| as_name(deref(doc, o)))
        .unwrap_or_else(|| resource_name.to_string());

    if kind == FontKind::Type0 {
        return load_type0(doc, resource_name, object_id, dict, &base_name);
    }
    load_simple(doc, resource_name, object_id, dict, kind, &base_name)
}

fn missing(resource_name: &str, object_id: Option<ObjectId>) -> Font {
    Font {
        resource_name: resource_name.into(),
        base_name: resource_name.into(),
        kind: FontKind::Other,
        object_id,
        vertical: false,
        font_matrix: Matrix::scale(0.001, 0.001),
        widths: WidthMap::constant(1000.0),
        vertical_widths: VerticalMap::default(),
        to_unicode: None,
        cid_map: None,
        predefined: None,
        simple: None,
        identity_two_byte: false,
        is_cid: false,
        cid_to_gid: CidToGid::None,
        cmap: FontCmap::default(),
        type3: None,
    }
}

fn load_simple(
    doc: &Document,
    resource_name: &str,
    object_id: Option<ObjectId>,
    dict: &Dictionary,
    kind: FontKind,
    base_name: &str,
) -> Font {
    let descriptor = dict
        .get(b"FontDescriptor")
        .ok()
        .and_then(|o| dict_of(doc, o));
    let flags = descriptor
        .and_then(|d| d.get(b"Flags").ok())
        .and_then(|o| o.as_i64().ok())
        .unwrap_or(0);
    let symbolic = flags & 4 != 0;
    let missing_width = descriptor
        .and_then(|d| d.get(b"MissingWidth").ok())
        .and_then(|o| as_f32(deref(doc, o)))
        .unwrap_or(0.0);

    let (mut simple, differences_names) =
        load_simple_encoding(doc, dict, kind, symbolic, base_name);
    if let Some(desc) = descriptor {
        if let Ok(obj) = desc.get(b"FontFile") {
            if let Some((_, bytes, _)) = stream_bytes(doc, obj, STREAM_LIMIT) {
                type1::apply_type1_encoding(&mut simple, &bytes);
            }
        }
    }
    let to_unicode = load_tounicode(doc, dict);
    let cmap = descriptor
        .map(|d| load_cmap_from_descriptor(doc, d))
        .unwrap_or_default();

    let first = dict
        .get(b"FirstChar")
        .ok()
        .and_then(|o| as_f32(deref(doc, o)))
        .map(|v| v as u32)
        .unwrap_or(0);
    let widths = if let Some(arr) = dict.get(b"Widths").ok().and_then(|o| array_of(doc, o)) {
        let values: Vec<f32> = arr.iter().filter_map(|o| as_f32(deref(doc, o))).collect();
        WidthMap::from_simple(first, &values, missing_width)
    } else {
        WidthMap::constant(if missing_width == 0.0 {
            0.0
        } else {
            missing_width
        })
    };

    let mut font_matrix = Matrix::scale(0.001, 0.001);
    let mut type3 = None;
    if kind == FontKind::Type3 {
        if let Some(arr) = dict.get(b"FontMatrix").ok().and_then(|o| array_of(doc, o)) {
            if arr.len() >= 6 {
                let n = |i: usize| {
                    arr.get(i)
                        .and_then(|o| as_f32(deref(doc, o)))
                        .unwrap_or(0.0)
                };
                font_matrix = Matrix::new(n(0), n(1), n(2), n(3), n(4), n(5));
            }
        }
        let mut info = load_type3(doc, dict, &differences_names, &simple);
        info.matrix = font_matrix;
        type3 = Some(info);
    }

    Font {
        resource_name: resource_name.into(),
        base_name: base_name.into(),
        kind,
        object_id,
        vertical: false,
        font_matrix,
        widths,
        vertical_widths: VerticalMap::default(),
        to_unicode,
        cid_map: None,
        predefined: None,
        simple: Some(simple),
        identity_two_byte: false,
        is_cid: false,
        cid_to_gid: CidToGid::None,
        cmap,
        type3,
    }
}

fn load_simple_encoding(
    doc: &Document,
    dict: &Dictionary,
    kind: FontKind,
    symbolic: bool,
    base_name: &str,
) -> (SimpleEncoding, HashMap<u32, String>) {
    let default_base = if base_name.contains("Symbol") {
        BaseEncoding::Symbol
    } else if symbolic || kind == FontKind::Type1 {
        BaseEncoding::Standard
    } else {
        BaseEncoding::WinAnsi
    };
    let mut names: HashMap<u32, String> = HashMap::new();
    let enc_obj = dict.get(b"Encoding").ok();
    let Some(enc_obj) = enc_obj else {
        let simple = if symbolic && !base_name.contains("Symbol") {
            // No encoding on a symbolic font: leave bytes unmapped so the cmap can win.
            empty_encoding()
        } else {
            SimpleEncoding::from_base(default_base)
        };
        return (simple, names);
    };
    let enc = deref(doc, enc_obj);
    if let Some(name) = as_name(enc) {
        let base = BaseEncoding::from_name(&name).unwrap_or(default_base);
        return (SimpleEncoding::from_base(base), names);
    }
    if let Ok(edict) = enc.as_dict() {
        let base = edict
            .get(b"BaseEncoding")
            .ok()
            .and_then(|o| as_name(deref(doc, o)))
            .and_then(|n| BaseEncoding::from_name(&n))
            .unwrap_or(default_base);
        let mut simple = SimpleEncoding::from_base(base);
        if let Some(diffs) = edict
            .get(b"Differences")
            .ok()
            .and_then(|o| array_of(doc, o))
        {
            let mut code: Option<u32> = None;
            for item in diffs {
                let item = deref(doc, item);
                if let Some(n) = as_f32(item) {
                    code = Some(n as u32);
                } else if let Some(name) = as_name(item) {
                    if let Some(c) = code {
                        if c <= 255 {
                            simple.apply_difference(c as u8, &name);
                            names.insert(c, name);
                        }
                        code = Some(c.saturating_add(1));
                    }
                }
            }
        }
        return (simple, names);
    }
    (SimpleEncoding::from_base(default_base), names)
}

fn empty_encoding() -> SimpleEncoding {
    let mut enc = SimpleEncoding::from_base(BaseEncoding::Standard);
    for i in 0..=255 {
        enc.apply_difference(i, ".notdef");
    }
    enc
}

fn load_type0(
    doc: &Document,
    resource_name: &str,
    object_id: Option<ObjectId>,
    dict: &Dictionary,
    base_name: &str,
) -> Font {
    let to_unicode = load_tounicode(doc, dict);
    let mut vertical = false;
    let mut predefined = None;
    let mut cid_map = None;
    let mut identity_two_byte = false;

    if let Ok(enc) = dict.get(b"Encoding") {
        let enc = deref(doc, enc);
        if let Some(name) = as_name(enc) {
            if name == "Identity-H" || name == "Identity-V" {
                identity_two_byte = true;
                vertical = name.ends_with("-V");
            } else if let Some(pre) = predefined::from_name(&name) {
                predefined = Some(pre);
                vertical = pre.vertical();
            } else {
                // Unknown named CMap: still consume bytes as 2-byte codes.
                identity_two_byte = true;
            }
        } else if let Some((_, bytes, _)) = stream_bytes(doc, enc, STREAM_LIMIT) {
            if bytes.windows(8).any(|w| w == b"/WMode 1") {
                vertical = true;
            }
            let parsed = CMap::parse(&bytes);
            if !parsed.is_empty() || !parsed.codespaces.is_empty() {
                cid_map = Some(parsed);
            }
        }
    } else {
        identity_two_byte = true;
    }

    let descendant = dict
        .get(b"DescendantFonts")
        .ok()
        .and_then(|o| array_of(doc, o))
        .and_then(|a| a.first())
        .and_then(|o| dict_of(doc, o));

    let mut widths = WidthMap::constant(1000.0);
    let mut vertical_widths = VerticalMap::default();
    let mut cid_to_gid = CidToGid::None;
    let mut cmap = FontCmap::default();
    let mut cid_base = base_name.to_string();

    if let Some(cid) = descendant {
        if let Some(name) = cid
            .get(b"BaseFont")
            .ok()
            .and_then(|o| as_name(deref(doc, o)))
        {
            cid_base = name;
        }
        let dw = cid
            .get(b"DW")
            .ok()
            .and_then(|o| as_f32(deref(doc, o)))
            .unwrap_or(1000.0);
        widths = if let Ok(w) = cid.get(b"W") {
            WidthMap::parse_w(doc, w, dw)
        } else {
            WidthMap::constant(dw)
        };
        vertical_widths = VerticalMap::parse(doc, cid.get(b"DW2").ok(), cid.get(b"W2").ok());
        if let Ok(map_obj) = cid.get(b"CIDToGIDMap") {
            cid_to_gid = load_cid_to_gid(doc, map_obj);
        } else if cid
            .get(b"Subtype")
            .ok()
            .and_then(|o| as_name(deref(doc, o)))
            .as_deref()
            == Some("CIDFontType2")
        {
            // CIDFontType2 defaults to identity when the map is absent.
            cid_to_gid = CidToGid::Identity;
        }
        if let Some(desc) = cid
            .get(b"FontDescriptor")
            .ok()
            .and_then(|o| dict_of(doc, o))
        {
            cmap = load_cmap_from_descriptor(doc, desc);
        }
    }

    Font {
        resource_name: resource_name.into(),
        base_name: cid_base,
        kind: FontKind::Type0,
        object_id,
        vertical,
        font_matrix: Matrix::scale(0.001, 0.001),
        widths,
        vertical_widths,
        to_unicode,
        cid_map,
        predefined,
        simple: None,
        identity_two_byte,
        is_cid: true,
        cid_to_gid,
        cmap,
        type3: None,
    }
}

fn load_cid_to_gid(doc: &Document, obj: &Object) -> CidToGid {
    let obj = deref(doc, obj);
    if as_name(obj).as_deref() == Some("Identity") {
        return CidToGid::Identity;
    }
    if let Some((_, bytes, _)) = stream_bytes(doc, obj, STREAM_LIMIT) {
        let mut table = Vec::with_capacity(bytes.len() / 2);
        for chunk in bytes.chunks(2) {
            if chunk.len() == 2 {
                table.push(u16::from_be_bytes([chunk[0], chunk[1]]));
            }
        }
        return CidToGid::Map(table);
    }
    CidToGid::None
}

fn load_tounicode(doc: &Document, dict: &Dictionary) -> Option<CMap> {
    let obj = dict.get(b"ToUnicode").ok()?;
    let (_, bytes, _) = stream_bytes(doc, obj, STREAM_LIMIT)?;
    let map = CMap::parse(&bytes);
    if map.is_empty() && map.codespaces.is_empty() {
        None
    } else {
        Some(map)
    }
}

fn load_cmap_from_descriptor(doc: &Document, desc: &Dictionary) -> FontCmap {
    for key in [
        b"FontFile2".as_slice(),
        b"FontFile3".as_slice(),
        b"FontFile".as_slice(),
    ] {
        if let Ok(obj) = desc.get(key) {
            if let Some((_, bytes, _)) = stream_bytes(doc, obj, STREAM_LIMIT) {
                let cmap = FontCmap::parse(&bytes);
                if !cmap.gid_to_unicode.is_empty() || !cmap.unicode_to_gid.is_empty() {
                    return cmap;
                }
            }
        }
    }
    FontCmap::default()
}

fn load_type3(
    doc: &Document,
    dict: &Dictionary,
    diff_names: &HashMap<u32, String>,
    simple: &SimpleEncoding,
) -> Type3 {
    let resources = dict
        .get(b"Resources")
        .ok()
        .and_then(|o| dict_of(doc, o))
        .map(|d| Resources::from_dict(doc, d))
        .unwrap_or_default();
    let mut procs = HashMap::new();
    let mut name_to_bytes: HashMap<String, Vec<u8>> = HashMap::new();
    if let Some(charprocs) = dict.get(b"CharProcs").ok().and_then(|o| dict_of(doc, o)) {
        for (name, obj) in charprocs.iter() {
            let name = String::from_utf8_lossy(name).into_owned();
            if let Some((_, bytes, _)) = stream_bytes(doc, obj, STREAM_LIMIT) {
                name_to_bytes.insert(name, bytes);
            }
        }
    }
    // Map character codes onto procs via Differences names first, then AGL
    // reverse through the simple encoding's unicode (best-effort by name).
    if diff_names.is_empty() {
        for (name, bytes) in &name_to_bytes {
            // Common case: WinAnsi code equals the byte of a single ASCII name.
            if name.len() == 1 {
                let code = name.as_bytes()[0] as u32;
                procs.insert(code, bytes.clone());
            } else if let Some(text) = glyph_name_to_unicode(name) {
                if text.chars().count() == 1 {
                    // Search the encoding for this unicode.
                    if let Some(code) =
                        (0u32..256).find(|c| simple.map(*c as u8).as_deref() == Some(text.as_str()))
                    {
                        procs.insert(code, bytes.clone());
                    }
                }
            }
        }
    } else {
        for (code, name) in diff_names {
            if let Some(bytes) = name_to_bytes.get(name) {
                procs.insert(*code, bytes.clone());
            }
        }
        for (name, bytes) in &name_to_bytes {
            if procs.values().any(|b| b == bytes) {
                continue;
            }
            if let Some(text) = glyph_name_to_unicode(name) {
                if let Some(code) =
                    (0u32..256).find(|c| simple.map(*c as u8).as_deref() == Some(text.as_str()))
                {
                    procs.entry(code).or_insert_with(|| bytes.clone());
                }
            }
        }
    }
    Type3 {
        matrix: Matrix::scale(0.001, 0.001),
        procs,
        resources,
    }
}
