//! Subset a CJK font down to the characters a translation uses.
//!
//! Glyf faces become a CIDFontType2 payload with Identity glyph ids. CFF faces
//! (Noto CJK) are subset with fontTools; the saved bytes are an OpenType file
//! whose `CFF ` table is what a CIDFontType0 FontFile3 must embed. Glyph ids in
//! `glyphs` are identity charset CIDs (glyph index, not the source `cidNNNNN`).
//! A viewer that paints CID as GID, and one that caches fonts by BaseFont, both
//! then see the same outlines. Shaping is one glyph per character. OpenType
//! GSUB is not applied.

use std::collections::{HashMap, HashSet, VecDeque};

use super::ttf::{build_cmap_subtable, checksum32, FontCmap};

#[derive(Clone, Debug)]
pub struct SubsetFont {
    pub bytes: Vec<u8>,
    pub units_per_em: u16,
    /// Unicode scalar → (new glyph id, advance in font units).
    pub glyphs: HashMap<u32, (u16, u16)>,
    /// Built-in Song outlines for scalars this face does not cover.
    /// Drawn with a second CID font so a missing character is not tofu.
    pub fallback: Option<Box<SubsetFont>>,
    /// Shown when a user face lacked glyphs, or could not be subset.
    pub notice: Option<String>,
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
        "CMTI",
        "CMBX",
        "SONG",
        "MING",
        "STIX",
        "TINOS",
        "GEORGIA",
        "PLEXSERIF",
        "NOTOSERIF",
        "BASKERVILLE",
    ]
    .iter()
    .any(|needle| upper.contains(needle));
    // Medium (NimbusRom-Medi) stays a regular Song face. SemiBold and Bold
    // use Song Bold, the same family as the body.
    let bold = ["BOLD", "BLACK", "HEAVY", "SEMIBOLD", "DEMIBOLD", "CMBX"]
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

/// Optional faces that replace the installed Noto Serif CJK files.
#[derive(Clone, Debug, Default)]
pub struct FontSources {
    /// Song face for regular body text (`RPT_CJK_FONT` when unset).
    pub serif: Option<std::path::PathBuf>,
    /// Optional sans file. When unset, a sans source face uses Song Bold.
    pub sans: Option<std::path::PathBuf>,
    /// Song Bold for titles, section heads, and sans source faces
    /// (`RPT_CJK_BOLD_FONT` when unset).
    pub bold: Option<std::path::PathBuf>,
}

/// A font path, plus an optional collection face (`simsun.ttc#0`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontSpec {
    pub path: std::path::PathBuf,
    pub face: Option<u32>,
}

/// `path#N` selects face N of a TTC or OTC. A file whose name itself
/// contains `#` is left unchanged.
pub fn parse_font_spec(raw: &str) -> FontSpec {
    let raw = raw.trim();
    if let Some((path, index)) = raw.rsplit_once('#') {
        if !index.is_empty() && index.chars().all(|ch| ch.is_ascii_digit()) {
            if let Ok(face) = index.parse::<u32>() {
                let bare = std::path::PathBuf::from(path);
                if bare.is_file() || !std::path::Path::new(raw).is_file() {
                    return FontSpec {
                        path: bare,
                        face: Some(face),
                    };
                }
            }
        }
    }
    FontSpec {
        path: std::path::PathBuf::from(raw),
        face: None,
    }
}

/// Subset a style-matched CJK face.
///
/// Body serif uses the configured Song face, then Noto Serif CJK Regular
/// (Light when that SC file is installed). Bold source faces, and sans
/// source faces with no `--cjk-sans`, use Noto Serif CJK Bold so the page
/// is one Song family. A character the user face does not cover is taken
/// from that built-in Song face and named in `notice`.
pub fn subset_for_style(
    style: FaceStyle,
    codepoints: &[u32],
    sources: &FontSources,
) -> Option<SubsetFont> {
    let song_bold = style.bold || !style.serif;
    if !style.bold && !style.serif {
        if let Some(spec) = sources.sans.clone().or_else(|| env_font("RPT_CJK_SANS")) {
            if let Some(font) = subset_user_with_fallback(&spec, codepoints, true) {
                return Some(font);
            }
        }
    }
    if song_bold {
        if let Some(spec) = sources
            .bold
            .clone()
            .or_else(|| env_font("RPT_CJK_BOLD_FONT"))
        {
            if let Some(font) = subset_user_with_fallback(&spec, codepoints, true) {
                return Some(font);
            }
        }
        if let Some(font) = noto_subset(true, true, codepoints) {
            return Some(font);
        }
    } else {
        let spec = sources.serif.clone().or_else(|| env_font("RPT_CJK_FONT"));
        if let Some(spec) = spec {
            if let Some(font) = subset_user_with_fallback(&spec, codepoints, false) {
                return Some(font);
            }
        }
        if let Some(font) = noto_subset(true, false, codepoints) {
            return Some(font);
        }
    }
    let bytes = load_cjk_font()?;
    subset_ttf(&bytes, codepoints)
}

impl SubsetFont {
    pub fn has(&self, cp: u32) -> bool {
        self.cid(cp).is_some()
    }

    /// Advance as a fraction of an em, from this face or its Song fallback.
    pub fn em_advance(&self, cp: u32) -> Option<f32> {
        if let Some((_, advance)) = self.glyphs.get(&cp) {
            return Some(*advance as f32 / self.units_per_em.max(1) as f32);
        }
        let fallback = self.fallback.as_ref()?;
        let (_, advance) = fallback.glyphs.get(&cp)?;
        Some(*advance as f32 / fallback.units_per_em.max(1) as f32)
    }

    /// Glyph id, and whether it lives in the fallback face.
    pub fn cid(&self, cp: u32) -> Option<(u16, bool)> {
        if let Some((gid, _)) = self.glyphs.get(&cp) {
            return Some((*gid, false));
        }
        self.fallback
            .as_ref()?
            .glyphs
            .get(&cp)
            .map(|(gid, _)| (*gid, true))
    }
}

fn env_font(name: &str) -> Option<std::path::PathBuf> {
    std::env::var(name).ok().and_then(|path| {
        let path = path.trim();
        if path.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(path))
        }
    })
}

fn noto_subset(serif: bool, bold: bool, codepoints: &[u32]) -> Option<SubsetFont> {
    if serif && !bold {
        if let Some(font) = light_song_subset(codepoints) {
            return Some(font);
        }
    }
    // Bold and sans share Song Bold. The Debian Noto CJK package has no
    // separate SemiBold file; Bold is the installed heavy weight.
    let (path, face) = if bold || !serif {
        ("/usr/share/fonts/opentype/noto/NotoSerifCJK-Bold.ttc", 2)
    } else {
        ("/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc", 2)
    };
    std::path::Path::new(path)
        .is_file()
        .then(|| subset_cff(path, face, codepoints))
        .flatten()
}

/// Source Han Serif / Noto Serif CJK SC Light, when it is installed.
/// Regular is the fallback. A dedicated SC OTF is face 0.
fn light_song_subset(codepoints: &[u32]) -> Option<SubsetFont> {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(
            std::path::PathBuf::from(home).join(".local/share/fonts/NotoSerifCJKsc-Light.otf"),
        );
    }
    paths.push("/usr/local/share/fonts/NotoSerifCJKsc-Light.otf".into());
    paths.push("/usr/share/fonts/opentype/noto/NotoSerifCJKsc-Light.otf".into());
    for path in paths {
        if !path.is_file() {
            continue;
        }
        let path = path.to_str()?;
        if let Some(font) = subset_cff(path, 0, codepoints) {
            return Some(font);
        }
    }
    None
}

fn subset_user_with_fallback(
    spec: &std::path::Path,
    codepoints: &[u32],
    bold: bool,
) -> Option<SubsetFont> {
    let parsed = parse_font_spec(&spec.to_string_lossy());
    if !parsed.path.is_file() {
        return None;
    }
    let face_note = parsed
        .face
        .map(|face| format!(" face {face}"))
        .unwrap_or_default();
    let Some(mut font) = subset_user_font(&parsed.path, parsed.face, codepoints) else {
        let mut builtin = noto_subset(true, bold, codepoints)?;
        builtin.notice = Some(format!(
            "CJK font {}{} could not be subset; using Noto Serif CJK",
            parsed.path.display(),
            face_note
        ));
        return Some(builtin);
    };
    let missing: Vec<u32> = codepoints
        .iter()
        .copied()
        .filter(|cp| (1..=0xFFFF).contains(cp) && !font.glyphs.contains_key(cp))
        .collect();
    if missing.is_empty() {
        return Some(font);
    }
    let Some(fallback) = noto_subset(true, bold, &missing) else {
        return Some(font);
    };
    let covered: Vec<u32> = missing
        .iter()
        .copied()
        .filter(|cp| fallback.glyphs.contains_key(cp))
        .collect();
    if covered.is_empty() {
        return Some(font);
    }
    font.notice = Some(missing_glyph_notice(&parsed.path, &covered));
    font.fallback = Some(Box::new(fallback));
    Some(font)
}

fn missing_glyph_notice(path: &std::path::Path, covered: &[u32]) -> String {
    let shown: Vec<String> = covered
        .iter()
        .take(8)
        .map(|cp| format!("U+{cp:04X}"))
        .collect();
    let extra = covered.len().saturating_sub(shown.len());
    let more = if extra > 0 {
        format!(" and {extra} more")
    } else {
        String::new()
    };
    format!(
        "CJK font {} has no glyph for {}{}; those characters use Noto Serif CJK",
        path.display(),
        shown.join(" "),
        more
    )
}

fn subset_user_font(
    path: &std::path::Path,
    face: Option<u32>,
    codepoints: &[u32],
) -> Option<SubsetFont> {
    let bytes = cached_font_bytes(path)?;
    if is_cff_font(&bytes) {
        let path = path.to_str()?;
        if let Some(face) = face {
            return subset_cff(path, face, codepoints);
        }
        let mut best: Option<SubsetFont> = None;
        let mut best_count = 0usize;
        for face in 0..6 {
            let Some(font) = subset_cff(path, face, codepoints) else {
                continue;
            };
            let count = font.glyphs.len();
            if count > best_count {
                best_count = count;
                best = Some(font);
            }
        }
        best
    } else {
        subset_ttf_face(&bytes, face, codepoints)
    }
}

fn is_cff_font(bytes: &[u8]) -> bool {
    if bytes.starts_with(b"OTTO") {
        return true;
    }
    if bytes.starts_with(b"ttcf") && bytes.len() >= 16 {
        let count = u32::from_be_bytes(bytes[8..12].try_into().ok().unwrap_or([0; 4]));
        if count == 0 {
            return false;
        }
        let offset = u32::from_be_bytes(bytes[12..16].try_into().ok().unwrap_or([0; 4])) as usize;
        return bytes.get(offset..offset + 4) == Some(b"OTTO");
    }
    false
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
    // cargo test runs cases on many threads. A clock stamp collides, and the
    // later call deletes the file the earlier call is still reading.
    let stamp = {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    };
    let font_out = dir.join(format!("rpt-cff-{}-{stamp}.otf", std::process::id()));
    let json_out = dir.join(format!("rpt-cff-{}-{stamp}.json", std::process::id()));
    let script = r#"
import json, sys, types
from io import BytesIO
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
# Source Han keeps Adobe-Identity CIDs in the glyph name (`cid00066` is "a",
# `cid00017` is "0"). Those values are also Standard Encoding slots, and the
# subset packs the outlines at glyph index 0..N. Identity-H must address the
# glyph index. Rewriting cmap/hmtx to match breaks fontTools; splice a CFF
# whose charset CID equals the glyph index into the subset instead.
buf = BytesIO()
font.save(buf)
top = font["CFF "].cff.topDictIndex[0]
if "ROS" in getattr(top, "rawDict", {}):
    mut = TTFont(BytesIO(buf.getvalue()))
    top = mut["CFF "].cff.topDictIndex[0]
    old = list(top.charset)
    new = [".notdef" if i == 0 else f"cid{i:05d}" for i in range(len(old))]
    cs = top.CharStrings
    rebuilt = {new[i]: cs.charStrings[name] for i, name in enumerate(old) if name in cs.charStrings}
    cs.charStrings.clear()
    cs.charStrings.update(rebuilt)
    top.charset = list(new)
    top.rawDict["CIDCount"] = len(new)
    raw = bytes(mut["CFF "].compile(mut))
    out = TTFont(BytesIO(buf.getvalue()))
    def compile_raw(self, ttFont, payload=raw):
        return payload
    out["CFF "].compile = types.MethodType(compile_raw, out["CFF "])
    out.save(out_font)
else:
    font.save(out_font)
font = TTFont(out_font)
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
        fallback: None,
        notice: None,
    })
}

pub fn load_cjk_font() -> Option<Vec<u8>> {
    if let Ok(path) = std::env::var("RPT_CJK_FONT") {
        if let Some(bytes) = cached_font_bytes(std::path::Path::new(path.trim())) {
            return Some(bytes.as_ref().clone());
        }
    }
    for path in [
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    ] {
        if let Some(bytes) = cached_font_bytes(std::path::Path::new(path)) {
            return Some(bytes.as_ref().clone());
        }
    }
    None
}

/// Font files are re-read for every subset. Keep the bytes for the process.
fn cached_font_bytes(path: &std::path::Path) -> Option<std::sync::Arc<Vec<u8>>> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<std::path::PathBuf, std::sync::Arc<Vec<u8>>>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().ok()?;
    if let Some(hit) = guard.get(path) {
        return Some(std::sync::Arc::clone(hit));
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let bytes = std::sync::Arc::new(bytes);
    guard.insert(path.to_path_buf(), std::sync::Arc::clone(&bytes));
    Some(bytes)
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
    subset_ttf_face(data, None, codepoints)
}

fn subset_ttf_face(data: &[u8], face: Option<u32>, codepoints: &[u32]) -> Option<SubsetFont> {
    let directories = face_directories(data);
    let selected: Vec<usize> = if let Some(index) = face {
        directories
            .get(index as usize)
            .copied()
            .into_iter()
            .collect()
    } else {
        directories
    };
    let mut best: Option<SubsetFont> = None;
    let mut best_count = 0usize;
    for directory in selected {
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
        fallback: None,
        notice: None,
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
    fn font_bytes_are_cached_for_a_path() {
        let path = std::env::temp_dir().join(format!(
            "rpt-font-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::write(&path, b"not-a-font-but-cached").unwrap();
        let first = cached_font_bytes(&path).unwrap();
        std::fs::write(&path, b"rewritten-after-cache").unwrap();
        let second = cached_font_bytes(&path).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &second));
        assert_eq!(&first[..], b"not-a-font-but-cached");
        let _ = std::fs::remove_file(&path);
    }

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
        let wanted = [0x4E2D, 0x6587, b'A' as u32];
        let subset = subset_ttf(&bytes, &wanted);
        let Some(subset) = subset else {
            return;
        };
        assert!(subset.glyphs.contains_key(&0x4E2D), "missing U+4E2D");
        let cmap = FontCmap::parse(&subset.bytes);
        assert!(cmap.unicode_to_gid.contains_key(&0x4E2D));
        // Droid Sans Fallback and some CJK faces have no Latin. Require A
        // only when the face `subset_ttf` would pick actually maps it.
        if winning_face_maps(&bytes, &wanted, 0x41) {
            assert!(subset.glyphs.contains_key(&0x41), "missing Latin A");
            assert!(cmap.unicode_to_gid.contains_key(&0x41));
        }
    }

    /// True when the face with the most requested mappings contains `code`.
    fn winning_face_maps(bytes: &[u8], codepoints: &[u32], code: u32) -> bool {
        let mut best_count = 0usize;
        let mut best_has = false;
        for directory in face_directories(bytes) {
            let Some(tables) = read_tables(bytes, directory) else {
                continue;
            };
            let Some(cmap_bytes) = tables.get(b"cmap".as_slice()) else {
                continue;
            };
            let cmap = FontCmap::from_table(cmap_bytes);
            let count = codepoints
                .iter()
                .filter(|cp| cmap.unicode_to_gid.contains_key(*cp))
                .count();
            if count > best_count {
                best_count = count;
                best_has = cmap.unicode_to_gid.contains_key(&code);
            }
        }
        best_has
    }

    #[test]
    fn medium_body_is_serif_and_real_bold_is_heiti() {
        let medium = face_style("NimbusRomNo9L-Medi");
        assert!(medium.serif);
        assert!(!medium.bold);
        let regular = face_style("NimbusRomNo9L-Regu");
        assert!(regular.serif && !regular.bold);
        let bold = face_style("NimbusRomNo9L-Bold");
        assert!(bold.serif && bold.bold);
        let semi = face_style("IBMPlexMono-SemiBold");
        assert!(semi.bold);
        let italic = face_style("CMTI10");
        assert!(italic.serif && italic.italic && !italic.bold);
        let cm_bold = face_style("CMBX10");
        assert!(cm_bold.bold);
        let baskerville = face_style("NewBaskerville-Roman");
        assert!(baskerville.serif && !baskerville.bold && !baskerville.italic);
        let baskerville_italic = face_style("NewBaskerville-Italic");
        assert!(baskerville_italic.serif && baskerville_italic.italic && !baskerville_italic.bold);
    }

    #[test]
    fn cff_subset_cids_are_glyph_indexes() {
        let path = "/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc";
        if !std::path::Path::new(path).is_file() {
            return;
        }
        let codes = [0x61u32, 0x65, 0x6f, 0x30, 0x4e2d, 0x6587];
        let Some(font) = subset_cff(path, 2, &codes) else {
            return;
        };
        // Source Han's original CIDs for these letters are the Standard
        // Encoding slots. Identity-H must not keep them.
        assert_ne!(font.glyphs[&0x61].0, 66, "a");
        assert_ne!(font.glyphs[&0x65].0, 70, "e");
        assert_ne!(font.glyphs[&0x6f].0, 80, "o");
        assert_ne!(font.glyphs[&0x30].0, 17, "0");
        let otf = std::env::temp_dir().join(format!("rpt-ident-{}.otf", std::process::id()));
        std::fs::write(&otf, &font.bytes).unwrap();
        let check = std::process::Command::new("python3")
            .arg("-c")
            .arg(
                r#"
import sys
from fontTools.ttLib import TTFont
font = TTFont(sys.argv[1])
order = font.getGlyphOrder()
bad = []
for index, name in enumerate(order):
    expect = ".notdef" if index == 0 else f"cid{index:05d}"
    if name != expect:
        bad.append(f"{index}:{name}")
if bad:
    raise SystemExit("charset is not identity: " + ", ".join(bad[:8]))
cmap = {}
for table in font["cmap"].tables:
    if table.platformID == 3 and table.platEncID in (1, 10):
        cmap.update(table.cmap)
for code in (0x61, 0x65, 0x6F, 0x30, 0x4E2D):
    name = cmap.get(code)
    if name is None or not name.startswith("cid"):
        raise SystemExit(f"missing cmap for U+{code:04X}")
    if int(name[3:]) != order.index(name):
        raise SystemExit(f"U+{code:04X} cid {name} is not its glyph index")
"#,
            )
            .arg(&otf)
            .status()
            .expect("python3");
        let _ = std::fs::remove_file(&otf);
        assert!(check.success(), "CFF charset CIDs are not glyph indexes");
    }

    fn face_name_contains(bytes: &[u8], needle: &str) -> bool {
        let wide: Vec<u8> = needle
            .encode_utf16()
            .flat_map(|unit| unit.to_be_bytes())
            .collect();
        bytes
            .windows(wide.len())
            .any(|window| window == wide.as_slice())
            || bytes
                .windows(needle.len())
                .any(|window| window == needle.as_bytes())
    }

    #[test]
    fn bold_and_sans_use_noto_serif_cjk_sc() {
        let bold_path = "/usr/share/fonts/opentype/noto/NotoSerifCJK-Bold.ttc";
        if !std::path::Path::new(bold_path).is_file() {
            return;
        }
        let codes = [0x4E2Du32, 0x6587];
        let bold = subset_for_style(
            FaceStyle {
                serif: true,
                bold: true,
                italic: false,
            },
            &codes,
            &FontSources::default(),
        )
        .expect("serif bold");
        let sans = subset_for_style(
            FaceStyle {
                serif: false,
                bold: false,
                italic: false,
            },
            &codes,
            &FontSources::default(),
        )
        .expect("sans");
        let body = subset_for_style(
            FaceStyle {
                serif: true,
                bold: false,
                italic: false,
            },
            &codes,
            &FontSources::default(),
        )
        .expect("body");
        assert!(
            face_name_contains(&bold.bytes, "Noto Serif CJK SC"),
            "bold title face is not Song"
        );
        assert!(
            face_name_contains(&sans.bytes, "Noto Serif CJK SC"),
            "sans source face is not Song"
        );
        assert!(face_name_contains(&body.bytes, "Noto Serif CJK"));
        assert!(!face_name_contains(&bold.bytes, "Noto Sans"));
        assert!(bold.fallback.is_none() && bold.notice.is_none());
    }

    #[test]
    fn a_collection_face_index_selects_that_face() {
        let path = "/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc";
        if !std::path::Path::new(path).is_file() {
            return;
        }
        let spec = parse_font_spec(&format!("{path}#0"));
        assert_eq!(spec.face, Some(0));
        assert!(spec.path.ends_with("NotoSerifCJK-Regular.ttc"));
        let missing = parse_font_spec("/no/such/simsun.ttc#1");
        assert_eq!(missing.face, Some(1));
        assert!(missing.path.ends_with("simsun.ttc"));
        let sources = FontSources {
            serif: Some(format!("{path}#0").into()),
            ..FontSources::default()
        };
        let font = subset_for_style(
            FaceStyle {
                serif: true,
                bold: false,
                italic: false,
            },
            &[0x4E2D],
            &sources,
        )
        .expect("jp face");
        assert!(
            face_name_contains(&font.bytes, "Noto Serif CJK JP"),
            "face 0 was not selected"
        );
        assert!(font.fallback.is_none());
    }

    #[test]
    fn a_user_font_missing_a_character_falls_back_to_noto_serif() {
        let bold_path = "/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc";
        if !std::path::Path::new(bold_path).is_file() {
            return;
        }
        let path = std::env::temp_dir().join(format!(
            "rpt-user-font-{}-{}.ttf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::write(&path, box_ttf(&[b'A' as u32])).unwrap();
        let sources = FontSources {
            serif: Some(path.clone()),
            ..FontSources::default()
        };
        let font = subset_for_style(
            FaceStyle {
                serif: true,
                bold: false,
                italic: false,
            },
            &[b'A' as u32, 0x4E2D],
            &sources,
        )
        .expect("user font");
        assert!(font.glyphs.contains_key(&(b'A' as u32)));
        assert!(!font.glyphs.contains_key(&0x4E2D));
        let fallback = font.fallback.as_ref().expect("song fallback");
        assert!(fallback.glyphs.contains_key(&0x4E2D));
        assert!(face_name_contains(&fallback.bytes, "Noto Serif CJK"));
        let note = font.notice.as_deref().expect("missing-glyph warning");
        assert!(note.contains("U+4E2D"), "{note}");
        assert!(note.contains("Noto Serif CJK"), "{note}");
        assert!(font.has(0x4E2D));
        assert!(font.em_advance(0x4E2D).is_some());
        let _ = std::fs::remove_file(&path);
    }
}
