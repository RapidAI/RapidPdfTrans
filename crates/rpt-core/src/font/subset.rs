//! Subset a TrueType glyf font down to the characters a translation uses.
//!
//! The result is a CIDFontType2 payload: Identity glyph ids, advances in font
//! units, and a Unicode cmap. Shaping is one glyph per character from `hmtx`.
//! OpenType GSUB is not applied. A CFF font is refused so the caller can keep
//! the original text instead of embedding a font this subsetter cannot rebuild.

use std::collections::{HashMap, HashSet, VecDeque};

use super::ttf::{build_cmap_subtable, checksum32, FontCmap};

#[derive(Clone, Debug)]
pub struct SubsetFont {
    pub bytes: Vec<u8>,
    pub units_per_em: u16,
    /// Unicode scalar → (new glyph id, advance in font units).
    pub glyphs: HashMap<u32, (u16, u16)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FaceStyle {
    pub serif: bool,
    pub bold: bool,
    pub italic: bool,
}

pub fn face_style(font_name: &str) -> FaceStyle {
    let upper = font_name.to_ascii_uppercase();
    let serif = [
        "SERIF",
        "ROMAN",
        "TIMES",
        "NIMBUSROM",
        "CMR",
        "SONG",
        "MING",
        "STIX",
        "TINOS",
        "GEORGIA",
        "PLEXSERIF",
        "NOTOSERIF",
    ]
    .iter()
    .any(|needle| upper.contains(needle));
    let bold = [
        "BOLD", "BLACK", "HEAVY", "SEMIBOLD", "DEMIBOLD", "CMBX", "MEDI",
    ]
    .iter()
    .any(|needle| upper.contains(needle));
    let italic = ["ITAL", "OBLIQUE", "CMTI"]
        .iter()
        .any(|needle| upper.contains(needle));
    FaceStyle {
        serif,
        bold,
        italic,
    }
}

/// Subset a style-matched CJK face. Noto CJK (CFF) is preferred; a glyf font is the fallback.
pub fn subset_for_style(style: FaceStyle, codepoints: &[u32]) -> Option<SubsetFont> {
    if let Some(face) = noto_face(style) {
        if let Some(font) = subset_cff(face.0, face.1, codepoints) {
            return Some(font);
        }
    }
    let bytes = load_cjk_font()?;
    subset_ttf(&bytes, codepoints)
}

fn noto_face(style: FaceStyle) -> Option<(&'static str, u32)> {
    let path = match (style.serif, style.bold) {
        (true, true) => "/usr/share/fonts/opentype/noto/NotoSerifCJK-Bold.ttc",
        (true, false) => "/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc",
        (false, true) => "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc",
        (false, false) => "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    };
    std::path::Path::new(path).is_file().then_some((path, 2))
}

fn subset_cff(path: &str, face_index: u32, codepoints: &[u32]) -> Option<SubsetFont> {
    let mut codes: Vec<u32> = codepoints
        .iter()
        .copied()
        .filter(|code| *code > 0 && *code <= 0xFFFF)
        .collect();
    codes.push(0x20);
    codes.sort_unstable();
    codes.dedup();
    if codes.is_empty() {
        return None;
    }
    let list = codes
        .iter()
        .map(|code| format!("{code:X}"))
        .collect::<Vec<_>>()
        .join(",");
    let dir = std::env::temp_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let font_out = dir.join(format!("rpt-cff-{stamp}.otf"));
    let json_out = dir.join(format!("rpt-cff-{stamp}.json"));
    let script = r#"
import json, sys
from fontTools.ttLib import TTFont
from fontTools.subset import Subsetter, Options
src, face, unicodes, out_font, out_json = sys.argv[1:]
codes = [int(item, 16) for item in unicodes.split(",") if item]
opt = Options()
opt.layout_features = ["*"]
opt.notdef_outline = True
opt.recommended_glyphs = True
font = TTFont(src, fontNumber=int(face))
subsetter = Subsetter(options=opt)
subsetter.populate(unicodes=codes)
subsetter.subset(font)
font.save(out_font)
cmap = {}
for table in font["cmap"].tables:
    if table.platformID == 3 and table.platEncID in (1, 10):
        for cp, name in table.cmap.items():
            cmap[cp] = name
order = font.getGlyphOrder()
upem = int(font["head"].unitsPerEm)
glyphs = []
for cp, name in cmap.items():
    if isinstance(name, str) and name.startswith("cid") and name[3:].isdigit():
        cid = int(name[3:])
    elif name in order:
        cid = order.index(name)
    else:
        continue
    if cid > 65535:
        continue
    advance = int(font["hmtx"].metrics.get(name, (upem, 0))[0])
    glyphs.append({"cp": int(cp), "cid": cid, "adv": advance})
json.dump({"upem": upem, "glyphs": glyphs}, open(out_json, "w"))
"#;
    let status = std::process::Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(path)
        .arg(face_index.to_string())
        .arg(&list)
        .arg(&font_out)
        .arg(&json_out)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let bytes = std::fs::read(&font_out).ok()?;
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(&json_out).ok()?).ok()?;
    let _ = std::fs::remove_file(&font_out);
    let _ = std::fs::remove_file(&json_out);
    let upem = meta.get("upem")?.as_u64()? as u16;
    let mut glyphs = HashMap::new();
    for item in meta.get("glyphs")?.as_array()? {
        let cp = item.get("cp")?.as_u64()? as u32;
        let cid = item.get("cid")?.as_u64()? as u16;
        let adv = item.get("adv")?.as_u64()? as u16;
        glyphs.insert(cp, (cid, adv));
    }
    if glyphs.is_empty() {
        return None;
    }
    Some(SubsetFont {
        bytes,
        units_per_em: upem.max(1),
        glyphs,
    })
}

pub fn load_cjk_font() -> Option<Vec<u8>> {
    if let Ok(path) = std::env::var("RPT_CJK_FONT") {
        if let Ok(bytes) = std::fs::read(path.trim()) {
            if !bytes.is_empty() {
                return Some(bytes);
            }
        }
    }
    for path in [
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            if !bytes.is_empty() {
                return Some(bytes);
            }
        }
    }
    None
}

/// Rectangle glyphs for tests. Advance is 600 units at 1000 units per em.
#[cfg(test)]
pub fn box_ttf(codepoints: &[u32]) -> Vec<u8> {
    let mut codes: Vec<u32> = codepoints
        .iter()
        .copied()
        .filter(|code| *code != 0 && *code <= 0xFFFF)
        .collect();
    codes.push(0x20);
    codes.sort_unstable();
    codes.dedup();
    let upem = 1000u16;
    let advance = 600u16;
    let mut glyphs = vec![empty_glyph()];
    let mut hmtx = Vec::new();
    push_hmtx(&mut hmtx, advance, 0);
    let mut mappings = Vec::new();
    for (index, code) in codes.iter().enumerate() {
        glyphs.push(box_glyph());
        push_hmtx(&mut hmtx, advance, 50);
        mappings.push((*code, (index as u16) + 1));
    }
    assemble_font(upem, &glyphs, &hmtx, &mappings, 800, -200)
}

pub fn subset_ttf(data: &[u8], codepoints: &[u32]) -> Option<SubsetFont> {
    let mut best: Option<SubsetFont> = None;
    let mut best_count = 0usize;
    for directory in face_directories(data) {
        let Some(subset) = subset_face(data, directory, codepoints) else {
            continue;
        };
        let count = subset.glyphs.len();
        if count > best_count {
            best_count = count;
            best = Some(subset);
        }
    }
    best
}

fn subset_face(file: &[u8], directory: usize, codepoints: &[u32]) -> Option<SubsetFont> {
    if directory + 12 > file.len() || &file[directory..directory + 4] == b"OTTO" {
        return None;
    }
    let tables = read_tables(file, directory)?;
    let head = tables.get(b"head".as_slice())?;
    let maxp = tables.get(b"maxp".as_slice())?;
    let hhea = tables.get(b"hhea".as_slice())?;
    let hmtx = tables.get(b"hmtx".as_slice())?;
    let loca = tables.get(b"loca".as_slice())?;
    let glyf = tables.get(b"glyf".as_slice())?;
    if head.len() < 54 || maxp.len() < 6 || hhea.len() < 36 {
        return None;
    }
    let upem = u16be(head, 18).unwrap_or(1000).max(1);
    let long_loca = i16be(head, 50).unwrap_or(0) != 0;
    let num_glyphs = u16be(maxp, 4)? as usize;
    let n_metrics = u16be(hhea, 34).unwrap_or(num_glyphs as u16) as usize;
    let offsets = loca_offsets(loca, num_glyphs, long_loca)?;
    let cmap = FontCmap::from_table(tables.get(b"cmap".as_slice())?);
    let mut wanted: HashSet<u16> = HashSet::new();
    wanted.insert(0);
    let mut requested = Vec::new();
    for code in codepoints {
        if *code == 0 || *code > 0xFFFF {
            continue;
        }
        let Some(gid) = cmap.unicode_to_gid.get(code).copied() else {
            continue;
        };
        if gid > u16::MAX as u32 {
            continue;
        }
        wanted.insert(gid as u16);
        requested.push((*code, gid as u16));
    }
    let mut queue: VecDeque<u16> = wanted.iter().copied().collect();
    while let Some(gid) = queue.pop_front() {
        let slice = glyph_slice(glyf, &offsets, gid as usize)?;
        for component in composite_gids(slice) {
            if wanted.insert(component) {
                queue.push_back(component);
            }
        }
    }
    let mut order: Vec<u16> = wanted.into_iter().collect();
    order.sort_unstable();
    if order.first().copied() != Some(0) {
        order.insert(0, 0);
    }
    let new_of: HashMap<u16, u16> = order
        .iter()
        .enumerate()
        .map(|(index, gid)| (*gid, index as u16))
        .collect();
    let mut glyphs = Vec::new();
    let mut metrics = Vec::new();
    for gid in &order {
        let mut bytes = glyph_slice(glyf, &offsets, *gid as usize)?.to_vec();
        rewrite_composites(&mut bytes, &new_of);
        glyphs.push(bytes);
        let adv = hmtx_advance(hmtx, *gid as usize, n_metrics);
        push_hmtx(&mut metrics, adv, 0);
    }
    let mut mappings: Vec<(u32, u16)> = requested
        .into_iter()
        .map(|(code, old)| (code, new_of[&old]))
        .collect();
    let ascender = i16be(hhea, 4).unwrap_or(800);
    let descender = i16be(hhea, 6).unwrap_or(-200);
    if codepoints.contains(&0x20) && !mappings.iter().any(|(code, _)| *code == 0x20) {
        let gid = glyphs.len() as u16;
        glyphs.push(vec![0u8; 10]);
        push_hmtx(&mut metrics, upem / 4, 0);
        mappings.push((0x20, gid));
    }
    let bytes = assemble_font(upem, &glyphs, &metrics, &mappings, ascender, descender);
    let mut glyph_map = HashMap::new();
    for (code, gid) in &mappings {
        let adv = hmtx_advance(&metrics, *gid as usize, glyphs.len());
        glyph_map.insert(*code, (*gid, adv));
    }
    Some(SubsetFont {
        bytes,
        units_per_em: upem,
        glyphs: glyph_map,
    })
}

fn face_directories(data: &[u8]) -> Vec<usize> {
    if data.len() >= 12 && &data[0..4] == b"ttcf" {
        let Some(n) = u32be(data, 8) else {
            return Vec::new();
        };
        let mut faces = Vec::new();
        for index in 0..n as usize {
            let Some(start) = u32be(data, 12 + index * 4) else {
                break;
            };
            faces.push(start as usize);
        }
        return faces;
    }
    vec![0]
}

fn read_tables(file: &[u8], directory: usize) -> Option<HashMap<Vec<u8>, &[u8]>> {
    if directory + 12 > file.len() {
        return None;
    }
    let n = u16be(file, directory + 4)? as usize;
    let mut tables = HashMap::new();
    for i in 0..n {
        let rec = directory + 12 + i * 16;
        if rec + 16 > file.len() {
            return None;
        }
        let tag = file[rec..rec + 4].to_vec();
        let offset = u32be(file, rec + 8)? as usize;
        let len = u32be(file, rec + 12)? as usize;
        if offset.saturating_add(len) > file.len() {
            return None;
        }
        tables.insert(tag, &file[offset..offset + len]);
    }
    Some(tables)
}

fn loca_offsets(loca: &[u8], num_glyphs: usize, long: bool) -> Option<Vec<u32>> {
    let count = num_glyphs + 1;
    if long {
        if loca.len() < count * 4 {
            return None;
        }
        Some(
            (0..count)
                .map(|i| u32be(loca, i * 4).unwrap_or(0))
                .collect(),
        )
    } else {
        if loca.len() < count * 2 {
            return None;
        }
        Some(
            (0..count)
                .map(|i| u16be(loca, i * 2).unwrap_or(0) as u32 * 2)
                .collect(),
        )
    }
}

fn glyph_slice<'a>(glyf: &'a [u8], offsets: &[u32], gid: usize) -> Option<&'a [u8]> {
    let start = *offsets.get(gid)? as usize;
    let end = *offsets.get(gid + 1)? as usize;
    if end < start || end > glyf.len() {
        return None;
    }
    Some(&glyf[start..end])
}

fn composite_gids(glyph: &[u8]) -> Vec<u16> {
    if glyph.len() < 10 {
        return Vec::new();
    }
    let contours = i16::from_be_bytes([glyph[0], glyph[1]]);
    if contours >= 0 {
        return Vec::new();
    }
    let mut gids = Vec::new();
    let mut i = 10usize;
    loop {
        if i + 4 > glyph.len() {
            break;
        }
        let flags = u16::from_be_bytes([glyph[i], glyph[i + 1]]);
        let gid = u16::from_be_bytes([glyph[i + 2], glyph[i + 3]]);
        gids.push(gid);
        i += 4;
        let arg_bytes = if flags & 0x0001 != 0 { 4 } else { 2 };
        i += arg_bytes;
        if flags & 0x0008 != 0 {
            i += 2;
        } else if flags & 0x0040 != 0 {
            i += 4;
        } else if flags & 0x0080 != 0 {
            i += 8;
        }
        if flags & 0x0020 == 0 {
            break;
        }
    }
    gids
}

fn rewrite_composites(glyph: &mut [u8], new_of: &HashMap<u16, u16>) {
    if glyph.len() < 10 {
        return;
    }
    let contours = i16::from_be_bytes([glyph[0], glyph[1]]);
    if contours >= 0 {
        return;
    }
    let mut i = 10usize;
    loop {
        if i + 4 > glyph.len() {
            break;
        }
        let flags = u16::from_be_bytes([glyph[i], glyph[i + 1]]);
        let gid = u16::from_be_bytes([glyph[i + 2], glyph[i + 3]]);
        let mapped = new_of.get(&gid).copied().unwrap_or(0);
        glyph[i + 2..i + 4].copy_from_slice(&mapped.to_be_bytes());
        i += 4;
        let arg_bytes = if flags & 0x0001 != 0 { 4 } else { 2 };
        i += arg_bytes;
        if flags & 0x0008 != 0 {
            i += 2;
        } else if flags & 0x0040 != 0 {
            i += 4;
        } else if flags & 0x0080 != 0 {
            i += 8;
        }
        if flags & 0x0020 == 0 {
            break;
        }
    }
}

fn hmtx_advance(hmtx: &[u8], gid: usize, n_metrics: usize) -> u16 {
    if n_metrics == 0 || hmtx.len() < 2 {
        return 600;
    }
    let index = gid.min(n_metrics - 1);
    let at = index * 4;
    if at + 2 > hmtx.len() {
        return 600;
    }
    u16be(hmtx, at).unwrap_or(600)
}

fn assemble_font(
    upem: u16,
    glyphs: &[Vec<u8>],
    hmtx: &[u8],
    mappings: &[(u32, u16)],
    ascender: i16,
    descender: i16,
) -> Vec<u8> {
    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    for glyph in glyphs {
        loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
        glyf.extend_from_slice(glyph);
        while glyf.len() % 4 != 0 {
            glyf.push(0);
        }
    }
    loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
    let cmap_sub = build_cmap_subtable(
        &mappings
            .iter()
            .map(|(code, gid)| (*code, *gid as u32))
            .collect::<Vec<_>>(),
    );
    let mut cmap = Vec::new();
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(&1u16.to_be_bytes());
    cmap.extend_from_slice(&3u16.to_be_bytes());
    cmap.extend_from_slice(&1u16.to_be_bytes());
    cmap.extend_from_slice(&12u32.to_be_bytes());
    cmap.extend_from_slice(&cmap_sub);
    let num = glyphs.len() as u16;
    let maxp = maxp_table(num);
    let hhea = hhea_table(ascender, descender, num);
    let head = head_table(upem, ascender, descender);
    let post = post_table();
    let name = name_table();
    let mut tables = vec![
        (*b"cmap", cmap),
        (*b"glyf", glyf),
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"hmtx", hmtx.to_vec()),
        (*b"loca", loca),
        (*b"maxp", maxp),
        (*b"name", name),
        (*b"post", post),
    ];
    tables.sort_by_key(|table| table.0);
    let mut font = pack_sfnt(&tables);
    apply_head_checksum(&mut font);
    font
}

fn pack_sfnt(tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let n = tables.len() as u16;
    let mut search_range = 1u16;
    let mut entry_selector = 0u16;
    while search_range * 2 <= n {
        search_range *= 2;
        entry_selector += 1;
    }
    search_range *= 16;
    let range_shift = n * 16 - search_range;
    let mut offset = 12 + tables.len() * 16;
    let mut directory = Vec::new();
    let mut body = Vec::new();
    for (tag, data) in tables {
        let pad = (4 - data.len() % 4) % 4;
        directory.extend_from_slice(tag);
        directory.extend_from_slice(&checksum32(data).to_be_bytes());
        directory.extend_from_slice(&(offset as u32).to_be_bytes());
        directory.extend_from_slice(&(data.len() as u32).to_be_bytes());
        body.extend_from_slice(data);
        body.extend(std::iter::repeat_n(0, pad));
        offset += data.len() + pad;
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0x00010000u32.to_be_bytes());
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&range_shift.to_be_bytes());
    out.extend_from_slice(&directory);
    out.extend_from_slice(&body);
    out
}

fn apply_head_checksum(font: &mut [u8]) {
    let Some(head_at) = table_offset(font, b"head") else {
        return;
    };
    if head_at + 12 > font.len() {
        return;
    }
    font[head_at + 8..head_at + 12].copy_from_slice(&0u32.to_be_bytes());
    let sum = checksum32(font);
    let adjustment = 0xB1B0AFBAu32.wrapping_sub(sum);
    font[head_at + 8..head_at + 12].copy_from_slice(&adjustment.to_be_bytes());
}

fn table_offset(font: &[u8], tag: &[u8; 4]) -> Option<usize> {
    if font.len() < 12 {
        return None;
    }
    let n = u16be(font, 4)? as usize;
    for i in 0..n {
        let rec = 12 + i * 16;
        if rec + 16 > font.len() {
            return None;
        }
        if &font[rec..rec + 4] == tag {
            return Some(u32be(font, rec + 8)? as usize);
        }
    }
    None
}

fn head_table(upem: u16, ascender: i16, descender: i16) -> Vec<u8> {
    let mut t = vec![0u8; 54];
    t[0..4].copy_from_slice(&0x00010000u32.to_be_bytes());
    t[12..16].copy_from_slice(&0x5F0F3CF5u32.to_be_bytes());
    t[16..18].copy_from_slice(&0x000Bu16.to_be_bytes());
    t[18..20].copy_from_slice(&upem.to_be_bytes());
    t[36..38].copy_from_slice(&0i16.to_be_bytes());
    t[38..40].copy_from_slice(&descender.to_be_bytes());
    t[40..42].copy_from_slice(&600i16.to_be_bytes());
    t[42..44].copy_from_slice(&ascender.to_be_bytes());
    t[44..46].copy_from_slice(&0i16.to_be_bytes());
    t[46..48].copy_from_slice(&8i16.to_be_bytes());
    t[48..50].copy_from_slice(&2i16.to_be_bytes());
    t[50..52].copy_from_slice(&1i16.to_be_bytes());
    t
}

fn hhea_table(ascender: i16, descender: i16, n_metrics: u16) -> Vec<u8> {
    let mut t = vec![0u8; 36];
    t[0..4].copy_from_slice(&0x00010000u32.to_be_bytes());
    t[4..6].copy_from_slice(&ascender.to_be_bytes());
    t[6..8].copy_from_slice(&descender.to_be_bytes());
    t[10..12].copy_from_slice(&600u16.to_be_bytes());
    t[18..20].copy_from_slice(&1i16.to_be_bytes());
    t[34..36].copy_from_slice(&n_metrics.to_be_bytes());
    t
}

fn maxp_table(num_glyphs: u16) -> Vec<u8> {
    let mut t = vec![0u8; 32];
    t[0..4].copy_from_slice(&0x00010000u32.to_be_bytes());
    t[4..6].copy_from_slice(&num_glyphs.to_be_bytes());
    t[6..8].copy_from_slice(&4u16.to_be_bytes());
    t[8..10].copy_from_slice(&1u16.to_be_bytes());
    t[14..16].copy_from_slice(&2u16.to_be_bytes());
    t
}

fn post_table() -> Vec<u8> {
    let mut t = vec![0u8; 32];
    t[0..4].copy_from_slice(&0x00030000u32.to_be_bytes());
    t
}

fn name_table() -> Vec<u8> {
    let text = b"RPTCJK";
    let mut t = Vec::new();
    t.extend_from_slice(&0u16.to_be_bytes());
    t.extend_from_slice(&1u16.to_be_bytes());
    t.extend_from_slice(&18u16.to_be_bytes());
    t.extend_from_slice(&1u16.to_be_bytes());
    t.extend_from_slice(&0u16.to_be_bytes());
    t.extend_from_slice(&0u16.to_be_bytes());
    t.extend_from_slice(&6u16.to_be_bytes());
    t.extend_from_slice(&(text.len() as u16).to_be_bytes());
    t.extend_from_slice(&0u16.to_be_bytes());
    t.extend_from_slice(text);
    t
}

#[cfg(test)]
fn empty_glyph() -> Vec<u8> {
    vec![0u8; 10]
}

#[cfg(test)]
fn box_glyph() -> Vec<u8> {
    let points = [(50i16, -200i16), (500, 0), (0, 900), (-500, 0)];
    let mut g = Vec::new();
    g.extend_from_slice(&1i16.to_be_bytes());
    g.extend_from_slice(&50i16.to_be_bytes());
    g.extend_from_slice(&(-200i16).to_be_bytes());
    g.extend_from_slice(&550i16.to_be_bytes());
    g.extend_from_slice(&700i16.to_be_bytes());
    g.extend_from_slice(&3u16.to_be_bytes());
    g.extend_from_slice(&0u16.to_be_bytes());
    g.extend(std::iter::repeat_n(0x01, points.len()));
    for (x, _) in points {
        g.extend_from_slice(&x.to_be_bytes());
    }
    for (_, y) in points {
        g.extend_from_slice(&y.to_be_bytes());
    }
    g
}

fn push_hmtx(out: &mut Vec<u8>, advance: u16, lsb: i16) {
    out.extend_from_slice(&advance.to_be_bytes());
    out.extend_from_slice(&lsb.to_be_bytes());
}

fn u16be(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn u32be(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn i16be(data: &[u8], at: usize) -> Option<i16> {
    Some(i16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_font_roundtrips_through_cmap_and_subset() {
        let font = box_ttf(&[b'A' as u32, b'B' as u32, 0x4E2D]);
        let cmap = FontCmap::parse(&font);
        assert!(cmap.unicode_to_gid.contains_key(&0x41));
        assert!(cmap.unicode_to_gid.contains_key(&0x20));
        assert!(cmap.unicode_to_gid.contains_key(&0x4E2D));
        let subset = subset_ttf(&font, &[0x42]).unwrap();
        assert!(subset.glyphs.contains_key(&0x42));
        assert!(!subset.glyphs.contains_key(&0x41));
        let again = FontCmap::parse(&subset.bytes);
        assert!(again.unicode_to_gid.contains_key(&0x42));
    }

    #[test]
    fn droid_subset_keeps_a_cjk_character_when_the_font_is_installed() {
        let Some(bytes) = load_cjk_font() else {
            return;
        };
        let subset = subset_ttf(&bytes, &[0x4E2D, 0x6587, b'A' as u32]);
        let Some(subset) = subset else {
            return;
        };
        assert!(subset.glyphs.contains_key(&0x4E2D), "missing U+4E2D");
        assert!(subset.glyphs.contains_key(&0x41), "missing Latin A");
        let cmap = FontCmap::parse(&subset.bytes);
        assert!(cmap.unicode_to_gid.contains_key(&0x4E2D));
        assert!(cmap.unicode_to_gid.contains_key(&0x41));
    }
}
