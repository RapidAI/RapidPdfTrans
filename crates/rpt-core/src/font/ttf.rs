//! Minimal TrueType / OpenType `cmap` reader.
//!
//! Used as the last-resort Unicode source: glyph id → Unicode (reversed from
//! a Unicode cmap) and, when the character code itself is a Unicode scalar in
//! the cmap, code → glyph id. Malformed fonts yield an empty map rather than
//! a panic. Format 0, 4, 6, and 12 subtables are read. TTC collections use
//! the first font that contains a Unicode cmap.

use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub struct FontCmap {
    pub unicode_to_gid: HashMap<u32, u32>,
    pub gid_to_unicode: HashMap<u32, String>,
}

impl FontCmap {
    pub fn parse(data: &[u8]) -> Self {
        if data.len() >= 12 && &data[0..4] == b"ttcf" {
            return parse_ttc(data);
        }
        parse_sfnt(data).unwrap_or_default()
    }
}

fn parse_ttc(data: &[u8]) -> FontCmap {
    let mut r = Reader::new(data);
    let _ = r.u32();
    let Some(num_fonts) = r.u32() else {
        return FontCmap::default();
    };
    for _ in 0..num_fonts {
        let Some(offset) = r.u32() else {
            break;
        };
        let start = offset as usize;
        if start >= data.len() {
            continue;
        }
        if let Some(cmap) = parse_sfnt(&data[start..]) {
            if !cmap.unicode_to_gid.is_empty() {
                return cmap;
            }
        }
    }
    FontCmap::default()
}

fn parse_sfnt(data: &[u8]) -> Option<FontCmap> {
    if data.len() < 12 {
        return None;
    }
    let mut r = Reader::new(data);
    let _scaler = r.u32()?;
    let num_tables = r.u16()? as usize;
    let _ = r.u16();
    let _ = r.u16();
    let _ = r.u16();
    let mut cmap_off = None;
    let mut cmap_len = None;
    for _ in 0..num_tables {
        let tag = r.bytes(4)?;
        let _checksum = r.u32()?;
        let offset = r.u32()? as usize;
        let length = r.u32()? as usize;
        if tag == b"cmap" {
            cmap_off = Some(offset);
            cmap_len = Some(length);
        }
    }
    let off = cmap_off?;
    let len = cmap_len?;
    if off.saturating_add(len) > data.len() || len < 4 {
        return None;
    }
    Some(parse_cmap_table(&data[off..off + len]))
}

fn parse_cmap_table(data: &[u8]) -> FontCmap {
    let mut best = FontCmap::default();
    let mut best_score = -1i32;
    let mut r = Reader::new(data);
    let Some(_) = r.u16() else {
        return best;
    };
    let Some(num) = r.u16() else {
        return best;
    };
    for _ in 0..num {
        let Some(platform) = r.u16() else {
            break;
        };
        let Some(encoding) = r.u16() else {
            break;
        };
        let Some(offset) = r.u32() else {
            break;
        };
        let start = offset as usize;
        if start >= data.len() {
            continue;
        }
        let score = match (platform, encoding) {
            (3, 10) => 4, // Windows full Unicode
            (0, _) => 3,  // Unicode
            (3, 1) => 2,  // Windows BMP
            (1, 0) => 1,  // Mac Roman
            (3, 0) => 0,  // Symbol
            _ => -1,
        };
        if score < 0 || score < best_score {
            continue;
        }
        let parsed = parse_subtable(&data[start..]);
        if !parsed.unicode_to_gid.is_empty() || score > best_score {
            best = parsed;
            best_score = score;
        }
    }
    best
}

fn parse_subtable(data: &[u8]) -> FontCmap {
    if data.len() < 4 {
        return FontCmap::default();
    }
    let format = u16::from_be_bytes([data[0], data[1]]);
    match format {
        0 => parse_format0(data),
        4 => parse_format4(data),
        6 => parse_format6(data),
        12 => parse_format12(data),
        _ => FontCmap::default(),
    }
}

fn parse_format0(data: &[u8]) -> FontCmap {
    let mut map = FontCmap::default();
    if data.len() < 6 + 256 {
        return map;
    }
    for (code, gid) in data[6..6 + 256].iter().copied().enumerate() {
        insert(&mut map, code as u32, gid as u32);
    }
    map
}

fn parse_format4(data: &[u8]) -> FontCmap {
    let mut map = FontCmap::default();
    if data.len() < 14 {
        return map;
    }
    let seg_count = u16::from_be_bytes([data[6], data[7]]) as usize / 2;
    if seg_count == 0 {
        return map;
    }
    let end_off = 14;
    let start_off = end_off + seg_count * 2 + 2;
    let delta_off = start_off + seg_count * 2;
    let offset_off = delta_off + seg_count * 2;
    if offset_off + seg_count * 2 > data.len() {
        return map;
    }
    for i in 0..seg_count {
        let end = u16::from_be_bytes([data[end_off + i * 2], data[end_off + i * 2 + 1]]);
        let start = u16::from_be_bytes([data[start_off + i * 2], data[start_off + i * 2 + 1]]);
        let delta = i16::from_be_bytes([data[delta_off + i * 2], data[delta_off + i * 2 + 1]]);
        let range_off =
            u16::from_be_bytes([data[offset_off + i * 2], data[offset_off + i * 2 + 1]]);
        if start == 0xFFFF && end == 0xFFFF {
            continue;
        }
        let mut code = start;
        while code <= end {
            let gid = if range_off == 0 {
                (code as i32 + delta as i32) as u16
            } else {
                let glyph_index_off =
                    offset_off + i * 2 + range_off as usize + (code - start) as usize * 2;
                if glyph_index_off + 2 > data.len() {
                    break;
                }
                let glyph = u16::from_be_bytes([data[glyph_index_off], data[glyph_index_off + 1]]);
                if glyph == 0 {
                    0
                } else {
                    (glyph as i32 + delta as i32) as u16
                }
            };
            insert(&mut map, code as u32, gid as u32);
            if code == u16::MAX {
                break;
            }
            code += 1;
        }
    }
    map
}

fn parse_format6(data: &[u8]) -> FontCmap {
    let mut map = FontCmap::default();
    if data.len() < 10 {
        return map;
    }
    let first = u16::from_be_bytes([data[6], data[7]]) as u32;
    let count = u16::from_be_bytes([data[8], data[9]]) as usize;
    if 10 + count * 2 > data.len() {
        return map;
    }
    for i in 0..count {
        let gid = u16::from_be_bytes([data[10 + i * 2], data[11 + i * 2]]);
        insert(&mut map, first + i as u32, gid as u32);
    }
    map
}

fn parse_format12(data: &[u8]) -> FontCmap {
    let mut map = FontCmap::default();
    if data.len() < 16 {
        return map;
    }
    let n_groups = u32::from_be_bytes([data[12], data[13], data[14], data[15]]) as usize;
    let mut off = 16;
    for _ in 0..n_groups {
        if off + 12 > data.len() {
            break;
        }
        let start = u32::from_be_bytes(data[off..off + 4].try_into().unwrap());
        let end = u32::from_be_bytes(data[off + 4..off + 8].try_into().unwrap());
        let gid = u32::from_be_bytes(data[off + 8..off + 12].try_into().unwrap());
        off += 12;
        let mut code = start;
        let mut g = gid;
        while code <= end {
            insert(&mut map, code, g);
            if code == u32::MAX {
                break;
            }
            code += 1;
            g = g.saturating_add(1);
            // Guard against a corrupt range that would allocate millions of entries.
            if g - gid > 100_000 {
                break;
            }
        }
    }
    map
}

fn insert(map: &mut FontCmap, unicode: u32, gid: u32) {
    if gid == 0 || char::from_u32(unicode).is_none() {
        return;
    }
    map.unicode_to_gid.entry(unicode).or_insert(gid);
    map.gid_to_unicode
        .entry(gid)
        .or_insert_with(|| char::from_u32(unicode).unwrap().to_string());
}

struct Reader<'a> {
    data: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, i: 0 }
    }

    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.i + n > self.data.len() {
            return None;
        }
        let s = &self.data[self.i..self.i + n];
        self.i += n;
        Some(s)
    }

    fn u16(&mut self) -> Option<u16> {
        let b = self.bytes(2)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        let b = self.bytes(4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// Build a one-table TrueType font whose cmap maps the given Unicode scalars
/// to glyph ids. Test helper, also usable by the corpus.
pub fn minimal_ttf(mappings: &[(u32, u32)]) -> Vec<u8> {
    let cmap = build_cmap_subtable(mappings);
    let mut table = Vec::new();
    table.extend_from_slice(&0u16.to_be_bytes());
    table.extend_from_slice(&1u16.to_be_bytes());
    table.extend_from_slice(&3u16.to_be_bytes()); // platform Windows
    table.extend_from_slice(&1u16.to_be_bytes()); // Unicode BMP
    table.extend_from_slice(&12u32.to_be_bytes()); // offset of subtable
    table.extend_from_slice(&cmap);
    wrap_sfnt(b"cmap", &table)
}

fn build_cmap_subtable(mappings: &[(u32, u32)]) -> Vec<u8> {
    let mut pairs: Vec<(u16, u16)> = mappings
        .iter()
        .filter(|(u, _)| *u <= 0xFFFF)
        .map(|(u, g)| (*u as u16, *g as u16))
        .collect();
    pairs.sort_by_key(|(u, _)| *u);
    pairs.dedup_by_key(|(u, _)| *u);
    // One segment per code, plus the required 0xFFFF sentinel.
    let seg_count = pairs.len() + 1;
    let seg_x2 = (seg_count * 2) as u16;
    let mut search_range = 1u16;
    let mut entry_selector = 0u16;
    while (search_range as usize) * 2 <= seg_count {
        search_range *= 2;
        entry_selector += 1;
    }
    search_range *= 2;
    let range_shift = seg_x2 - search_range;

    let mut end_codes = Vec::new();
    let mut start_codes = Vec::new();
    let mut deltas = Vec::new();
    for (code, gid) in &pairs {
        end_codes.extend_from_slice(&code.to_be_bytes());
        start_codes.extend_from_slice(&code.to_be_bytes());
        let delta = (*gid as i32 - *code as i32) as i16;
        deltas.extend_from_slice(&delta.to_be_bytes());
    }
    end_codes.extend_from_slice(&0xFFFFu16.to_be_bytes());
    start_codes.extend_from_slice(&0xFFFFu16.to_be_bytes());
    deltas.extend_from_slice(&1i16.to_be_bytes());
    let range_offsets = vec![0u8; seg_count * 2];

    let length = (16 + seg_count * 8) as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&4u16.to_be_bytes());
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&seg_x2.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&range_shift.to_be_bytes());
    out.extend_from_slice(&end_codes);
    out.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
    out.extend_from_slice(&start_codes);
    out.extend_from_slice(&deltas);
    out.extend_from_slice(&range_offsets);
    out
}

fn wrap_sfnt(tag: &[u8; 4], table: &[u8]) -> Vec<u8> {
    let offset = 12 + 16;
    let checksum = checksum32(table);
    let mut out = Vec::new();
    out.extend_from_slice(&0x00010000u32.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&16u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(tag);
    out.extend_from_slice(&checksum.to_be_bytes());
    out.extend_from_slice(&(offset as u32).to_be_bytes());
    out.extend_from_slice(&(table.len() as u32).to_be_bytes());
    out.extend_from_slice(table);
    out
}

fn checksum32(data: &[u8]) -> u32 {
    let mut sum = 0u32;
    let mut i = 0;
    while i < data.len() {
        let mut word = [0u8; 4];
        for b in &mut word {
            if i < data.len() {
                *b = data[i];
                i += 1;
            }
        }
        sum = sum.wrapping_add(u32::from_be_bytes(word));
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_minimal_cmap() {
        let font = minimal_ttf(&[(0x41, 1), (0x42, 2), (0x4E2D, 7)]);
        let cmap = FontCmap::parse(&font);
        assert_eq!(cmap.unicode_to_gid.get(&0x41), Some(&1));
        assert_eq!(cmap.gid_to_unicode.get(&2).map(String::as_str), Some("B"));
        assert_eq!(cmap.gid_to_unicode.get(&7).map(String::as_str), Some("中"));
    }
}
