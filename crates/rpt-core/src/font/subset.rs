//! Subset a CJK font down to the characters a translation uses.
//!
//! Glyf faces become a CIDFontType2 payload with Identity glyph ids. The
//! subset is a standalone TrueType file: `maxp` maxima cover every outline
//! (a short `maxContours` makes Windows/OTS drop multi-contour glyphs such as
//! a, e, o, 0, and most Han), and `OS/2` is present. fontTools writes that
//! file when Python is available; the in-process subsetter is the fallback.
//! CFF faces (Noto CJK) are subset with fontTools. The saved bytes are an
//! OpenType file whose `CFF ` table is what a CIDFontType0 FontFile3 must
//! embed. Glyph ids in `glyphs` are identity charset CIDs (glyph index, not
//! the source `cidNNNNN`). A viewer that paints CID as GID, and one that
//! caches fonts by BaseFont, both then see the same outlines. Shaping is one
//! glyph per character. OpenType GSUB is not applied.

use std::collections::{HashMap, HashSet, VecDeque};

use super::ttf::{build_cmap_subtable, checksum32, FontCmap};

#[derive(Clone, Debug)]
pub struct SubsetFont {
    pub bytes: Vec<u8>,
    pub units_per_em: u16,
    /// Unicode scalar → (new glyph id, advance in font units).
    pub glyphs: HashMap<u32, (u16, u16)>,
    /// Built-in outlines for scalars this face does not cover.
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
    // Medium (NimbusRom-Medi) stays regular. SemiBold and Bold use the Bold
    // weight of the selected CJK family. A sans source face is not Bold.
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

/// Built-in CJK family. Sans is Source Han Sans / Noto Sans CJK SC (Heiti).
/// Serif is Source Han Serif / Noto Serif CJK SC (Songti).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CjkStyle {
    #[default]
    Sans,
    Serif,
}

impl CjkStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sans => "sans",
            Self::Serif => "serif",
        }
    }

    /// Name used in a missing-glyph warning.
    pub fn label(self) -> &'static str {
        match self {
            Self::Sans => "Noto Sans CJK",
            Self::Serif => "Noto Serif CJK",
        }
    }

    /// `sans` or `serif`, ignoring ASCII case and surrounding space.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "sans" => Ok(Self::Sans),
            "serif" => Ok(Self::Serif),
            other => Err(format!("unknown cjk_style `{other}`; use sans or serif")),
        }
    }

    /// An explicit value wins. Otherwise `RPT_CJK_STYLE`, then Sans.
    pub fn resolve(explicit: Option<&str>) -> Result<Self, String> {
        if let Some(raw) = explicit.map(str::trim).filter(|text| !text.is_empty()) {
            return Self::parse(raw);
        }
        match std::env::var("RPT_CJK_STYLE") {
            Ok(value) if !value.trim().is_empty() => Self::parse(value.trim()),
            _ => Ok(Self::Sans),
        }
    }
}

/// Optional faces that replace the installed Noto CJK files.
#[derive(Clone, Debug, Default)]
pub struct FontSources {
    /// Regular face for body text (`RPT_CJK_FONT` when unset).
    /// This path wins over [`Self::style`].
    pub serif: Option<std::path::PathBuf>,
    /// Regular sans source text only. This path wins over `serif` for that
    /// source style.
    pub sans: Option<std::path::PathBuf>,
    /// Bold face for titles and bold source text (`RPT_CJK_BOLD_FONT` when
    /// unset). This path wins over [`Self::style`].
    pub bold: Option<std::path::PathBuf>,
    /// Family used when a path above is unset. `None` reads `RPT_CJK_STYLE`,
    /// then Sans (Noto Sans CJK SC).
    pub style: Option<CjkStyle>,
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
/// The built-in family is Noto Sans CJK SC unless `sources.style` or
/// `RPT_CJK_STYLE` selects Serif. Body text uses Regular. Titles and other
/// bold source faces use Bold. `--cjk-font` and `--cjk-bold-font` replace
/// those two weights and win over the family. A character the user face
/// does not cover is taken from the same built-in family and weight, and
/// named in `notice`.
pub fn subset_for_style(
    style: FaceStyle,
    codepoints: &[u32],
    sources: &FontSources,
) -> Option<SubsetFont> {
    let family = match sources.style {
        Some(family) => family,
        None => CjkStyle::resolve(None).ok()?,
    };
    // An explicit sans file still replaces regular sans source text only.
    if !style.bold && !style.serif {
        if let Some(spec) = sources.sans.clone().or_else(|| env_font("RPT_CJK_SANS")) {
            if let Some(font) = subset_user_with_fallback(&spec, codepoints, family, false) {
                return Some(font);
            }
        }
    }
    if style.bold {
        if let Some(spec) = sources
            .bold
            .clone()
            .or_else(|| env_font("RPT_CJK_BOLD_FONT"))
        {
            if let Some(font) = subset_user_with_fallback(&spec, codepoints, family, true) {
                return Some(font);
            }
        }
        if let Some(font) = noto_subset(family, true, codepoints) {
            return Some(font);
        }
    } else {
        if let Some(spec) = sources.serif.clone().or_else(|| env_font("RPT_CJK_FONT")) {
            if let Some(font) = subset_user_with_fallback(&spec, codepoints, family, false) {
                return Some(font);
            }
        }
        if let Some(font) = noto_subset(family, false, codepoints) {
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

    /// Advance as a fraction of an em, from this face or its built-in fallback.
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

fn noto_paths(family: CjkStyle, bold: bool) -> &'static [(&'static str, u32)] {
    match (family, bold) {
        (CjkStyle::Sans, true) => &[
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc", 2),
            ("/usr/share/fonts/truetype/noto/NotoSansCJK-Bold.ttc", 2),
        ],
        (CjkStyle::Sans, false) => &[
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 2),
            ("/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc", 2),
        ],
        (CjkStyle::Serif, true) => &[
            ("/usr/share/fonts/opentype/noto/NotoSerifCJK-Bold.ttc", 2),
            ("/usr/share/fonts/truetype/noto/NotoSerifCJK-Bold.ttc", 2),
        ],
        (CjkStyle::Serif, false) => &[
            ("/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc", 2),
            ("/usr/share/fonts/truetype/noto/NotoSerifCJK-Regular.ttc", 2),
        ],
    }
}

fn noto_subset(family: CjkStyle, bold: bool, codepoints: &[u32]) -> Option<SubsetFont> {
    // Song body still prefers an installed Light face. Heiti body is Regular.
    if family == CjkStyle::Serif && !bold {
        if let Some(font) = light_song_subset(codepoints) {
            return Some(font);
        }
    }
    // Debian ships Regular and Bold. Face 2 of each collection is SC.
    for (path, face) in noto_paths(family, bold) {
        if std::path::Path::new(path).is_file() {
            if let Some(font) = subset_cff(path, *face, codepoints) {
                return Some(font);
            }
        }
    }
    // A machine without the system package downloads the SC OTF once.
    // The bytes are not stored in the wheel or the SDK.
    let cached = ensure_noto(family, bold)?;
    let path = cached.to_str()?;
    subset_cff(path, 0, codepoints)
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
    family: CjkStyle,
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
        let mut builtin = noto_subset(family, bold, codepoints)?;
        builtin.notice = Some(format!(
            "CJK font {}{} could not be subset; using {}",
            parsed.path.display(),
            face_note,
            family.label()
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
    let Some(fallback) = noto_subset(family, bold, &missing) else {
        return Some(font);
    };
    let mut covered: Vec<u32> = missing
        .iter()
        .copied()
        .filter(|cp| fallback.glyphs.contains_key(cp))
        .collect();
    covered.sort_unstable();
    covered.dedup();
    if covered.is_empty() {
        return Some(font);
    }
    font.notice = Some(missing_glyph_notice(&parsed.path, &covered, family));
    font.fallback = Some(Box::new(fallback));
    Some(font)
}

fn missing_glyph_notice(path: &std::path::Path, covered: &[u32], family: CjkStyle) -> String {
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
        "CJK font {} has no glyph for {}{}; those characters use {}",
        path.display(),
        shown.join(" "),
        more,
        family.label()
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
    let mut command = python_command()?;
    let status = command
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

fn python_command() -> Option<std::process::Command> {
    for name in ["python3", "python"] {
        let Ok(output) = std::process::Command::new(name).arg("--version").output() else {
            continue;
        };
        if output.status.success() {
            return Some(std::process::Command::new(name));
        }
    }
    None
}

fn next_stamp() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

thread_local! {
    static SKIP_FONTTOOLS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `body` with the in-process glyf assembler. Python fontTools is not called.
#[cfg(test)]
pub struct RustGlyfOnly {
    previous: bool,
}

#[cfg(test)]
impl RustGlyfOnly {
    pub fn enter() -> Self {
        let previous = SKIP_FONTTOOLS.with(|flag| flag.replace(true));
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for RustGlyfOnly {
    fn drop(&mut self) {
        SKIP_FONTTOOLS.with(|flag| flag.set(self.previous));
    }
}

enum GlyfAttempt {
    Ready(SubsetFont),
    /// Python or a glyf `OS/2` face is not available. The Rust assembler runs.
    Absent,
    /// fontTools started and failed. Do not hide that behind the assembler.
    Failed,
}

/// Subset a glyf face with fontTools. The result is one TrueType file, not a
/// collection. Glyph ids are the subset's glyph indexes, so CIDToGIDMap
/// Identity addresses the outline that was packed.
fn subset_glyf_fonttools(data: &[u8], face: Option<u32>, codepoints: &[u32]) -> GlyfAttempt {
    if SKIP_FONTTOOLS.with(std::cell::Cell::get)
        || is_cff_font(data)
        || !font_has_table(data, face, b"OS/2")
    {
        return GlyfAttempt::Absent;
    }
    let mut codes: Vec<u32> = codepoints
        .iter()
        .copied()
        .filter(|code| *code > 0 && *code <= 0xFFFF)
        .collect();
    codes.push(0x20);
    codes.sort_unstable();
    codes.dedup();
    if codes.is_empty() {
        return GlyfAttempt::Absent;
    }
    let list = codes
        .iter()
        .map(|code| format!("{code:X}"))
        .collect::<Vec<_>>()
        .join(",");
    let stamp = next_stamp();
    let dir = std::env::temp_dir();
    let src = dir.join(format!("rpt-glyf-src-{}-{stamp}.bin", std::process::id()));
    let font_out = dir.join(format!("rpt-glyf-{}-{stamp}.ttf", std::process::id()));
    let json_out = dir.join(format!("rpt-glyf-{}-{stamp}.json", std::process::id()));
    if std::fs::write(&src, data).is_err() {
        return GlyfAttempt::Absent;
    }
    let face_arg = face
        .map(|face| face.to_string())
        .unwrap_or_else(|| "-".to_string());
    let script = r#"
import json, struct, sys
from fontTools.ttLib import TTFont
from fontTools.ttLib.tables._g_l_y_f import Glyph as RawGlyph
from fontTools.subset import Subsetter, Options
src, face, unicodes, out_font, out_json = sys.argv[1:]
codes = [int(item, 16) for item in unicodes.split(",") if item]
raw = open(src, "rb").read(12)
if raw[:4] == b"ttcf":
    count = struct.unpack(">I", open(src, "rb").read(12)[8:12])[0]
else:
    count = 1
wanted = [int(face)] if face != "-" else list(range(min(count, 32)))

def cover(font):
    cmap = font.getBestCmap() or {}
    return sum(1 for code in codes if code in cmap)

best_face = None
best_count = -1
for index in wanted:
    try:
        font = TTFont(src, fontNumber=index)
    except Exception:
        continue
    if "glyf" not in font:
        continue
    covered = cover(font)
    if covered > best_count:
        best_count = covered
        best_face = index
if best_face is None or best_count <= 0:
    raise SystemExit(2)
opt = Options()
# "*" pulls every GSUB alternate. Some of those outlines make this
# fontTools build crash in glyf.trim, and the page does not need them.
# Component glyphs are still closed over.
opt.layout_features = []
opt.notdef_outline = True
opt.recommended_glyphs = True
opt.hinting = True
font = TTFont(src, fontNumber=best_face)
subsetter = Subsetter(options=opt)
subsetter.populate(unicodes=codes)
subsetter.subset(font)
font.save(out_font)
font = TTFont(out_font)
if font.sfntVersion != "\x00\x01\x00\x00":
    raise SystemExit("subset is not a standalone TrueType file")
for table in ("cmap", "glyf", "head", "hhea", "hmtx", "loca", "maxp", "name", "post"):
    if table not in font:
        raise SystemExit(f"missing {table}")
maxp = font["maxp"]
glyf = font["glyf"]
order = font.getGlyphOrder()
for name in order:
    raw_glyph = glyf.glyphs[name]
    raw = bytes(getattr(raw_glyph, "data", b"") or b"")
    try:
        RawGlyph(raw).trim(remove_hinting=False)
    except Exception as exc:
        raise SystemExit(f"{name} glyf record is not trimmable: {exc}")
for name in order:
    glyph = glyf[name]
    contours = int(glyph.numberOfContours)
    if contours > int(getattr(maxp, "maxContours", 0) or 0) and contours >= 0:
        raise SystemExit(f"{name} has {contours} contours, maxp allows {maxp.maxContours}")
    if glyph.isComposite():
        elements = len(glyph.components)
        if elements > int(getattr(maxp, "maxComponentElements", 0) or 0):
            raise SystemExit(f"{name} components exceed maxp")
        for component in glyph.components:
            if component.glyphName not in glyf:
                raise SystemExit(f"{name} missing component {component.glyphName}")
cmap = font.getBestCmap() or {}
upem = int(font["head"].unitsPerEm)
glyphs = []
for cp, name in cmap.items():
    if name not in order:
        continue
    gid = order.index(name)
    if gid > 65535:
        continue
    advance = int(font["hmtx"].metrics.get(name, (upem, 0))[0])
    glyphs.append({"cp": int(cp), "cid": gid, "adv": min(advance, 65535)})
if not glyphs:
    raise SystemExit("empty cmap")
json.dump({"upem": upem, "glyphs": glyphs}, open(out_json, "w"))
"#;
    let Some(mut command) = python_command() else {
        let _ = std::fs::remove_file(&src);
        return GlyfAttempt::Absent;
    };
    let output = command
        .arg("-c")
        .arg(script)
        .arg(&src)
        .arg(face_arg)
        .arg(&list)
        .arg(&font_out)
        .arg(&json_out)
        .output();
    let cleanup = || {
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&font_out);
        let _ = std::fs::remove_file(&json_out);
    };
    let Ok(output) = output else {
        cleanup();
        return GlyfAttempt::Absent;
    };
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let uncovered = output.status.code() == Some(2) && !err.contains("Traceback");
        cleanup();
        if uncovered {
            return GlyfAttempt::Absent;
        }
        let message = err.trim();
        if !message.is_empty() {
            eprintln!("fontTools glyf subset failed:\n{message}");
        }
        return GlyfAttempt::Failed;
    }
    let bytes = match std::fs::read(&font_out) {
        Ok(bytes) => bytes,
        Err(_) => {
            cleanup();
            return GlyfAttempt::Failed;
        }
    };
    let meta: serde_json::Value = match std::fs::read(&json_out)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    {
        Some(meta) => meta,
        None => {
            cleanup();
            return GlyfAttempt::Failed;
        }
    };
    cleanup();
    let Some(upem) = meta.get("upem").and_then(|value| value.as_u64()) else {
        return GlyfAttempt::Failed;
    };
    let upem = upem as u16;
    let Some(items) = meta.get("glyphs").and_then(|value| value.as_array()) else {
        return GlyfAttempt::Failed;
    };
    let mut glyphs = HashMap::new();
    for item in items {
        let Some(cp) = item.get("cp").and_then(|value| value.as_u64()) else {
            return GlyfAttempt::Failed;
        };
        let Some(cid) = item.get("cid").and_then(|value| value.as_u64()) else {
            return GlyfAttempt::Failed;
        };
        let Some(adv) = item.get("adv").and_then(|value| value.as_u64()) else {
            return GlyfAttempt::Failed;
        };
        if cid > u64::from(u16::MAX) {
            return GlyfAttempt::Failed;
        }
        glyphs.insert(cp as u32, (cid as u16, adv.min(u64::from(u16::MAX)) as u16));
    }
    if glyphs.is_empty() {
        return GlyfAttempt::Failed;
    }
    GlyfAttempt::Ready(SubsetFont {
        bytes,
        units_per_em: upem.max(1),
        glyphs,
        fallback: None,
        notice: None,
    })
}

struct CachedFace {
    file: &'static str,
    url: &'static str,
    sha: &'static str,
}

const NOTO_SANS_REGULAR: CachedFace = CachedFace {
    file: "NotoSansCJKsc-Regular.otf",
    url: "https://raw.githubusercontent.com/notofonts/noto-cjk/Sans2.004/Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Regular.otf",
    sha: "2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b",
};
const NOTO_SANS_BOLD: CachedFace = CachedFace {
    file: "NotoSansCJKsc-Bold.otf",
    url: "https://raw.githubusercontent.com/notofonts/noto-cjk/Sans2.004/Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Bold.otf",
    sha: "b5f0d1a190a7f9b43c310a8850630af12553df32c4c050543f9059732d9b4c0a",
};
const NOTO_SERIF_REGULAR: CachedFace = CachedFace {
    file: "NotoSerifCJKsc-Regular.otf",
    url: "https://raw.githubusercontent.com/notofonts/noto-cjk/Serif2.003/Serif/OTF/SimplifiedChinese/NotoSerifCJKsc-Regular.otf",
    sha: "2a2eae2628df83556c54018c41e20fa532c1b862c5256ae8b3f23feb918d12ca",
};
const NOTO_SERIF_BOLD: CachedFace = CachedFace {
    file: "NotoSerifCJKsc-Bold.otf",
    url: "https://raw.githubusercontent.com/notofonts/noto-cjk/Serif2.003/Serif/OTF/SimplifiedChinese/NotoSerifCJKsc-Bold.otf",
    sha: "8af07d4b6c2e82bcc72a30e066eaf295f11b9424f4aad2eaa9fe0e9c3b38fc73",
};

fn cached_face(family: CjkStyle, bold: bool) -> &'static CachedFace {
    match (family, bold) {
        (CjkStyle::Sans, false) => &NOTO_SANS_REGULAR,
        (CjkStyle::Sans, true) => &NOTO_SANS_BOLD,
        (CjkStyle::Serif, false) => &NOTO_SERIF_REGULAR,
        (CjkStyle::Serif, true) => &NOTO_SERIF_BOLD,
    }
}

/// Directory for the downloaded Noto CJK SC faces.
///
/// `RPT_FONT_CACHE` wins. Otherwise this is `%LOCALAPPDATA%\rapidpdftrans`
/// on Windows, `~/Library/Caches/rapidpdftrans` on macOS, and
/// `$XDG_CACHE_HOME/rapidpdftrans` or `~/.cache/rapidpdftrans` elsewhere.
fn font_cache_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("RPT_FONT_CACHE") {
        if !dir.is_empty() {
            return std::path::PathBuf::from(dir);
        }
    }
    if cfg!(windows) {
        if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
            if !dir.is_empty() {
                return std::path::PathBuf::from(dir).join("rapidpdftrans");
            }
        }
    }
    if cfg!(target_os = "macos") {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home)
                .join("Library")
                .join("Caches")
                .join("rapidpdftrans");
        }
    }
    if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
        if !dir.is_empty() {
            return std::path::PathBuf::from(dir).join("rapidpdftrans");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return std::path::PathBuf::from(home)
            .join(".cache")
            .join("rapidpdftrans");
    }
    std::env::temp_dir().join("rapidpdftrans")
}

/// Noto CJK SC Regular or Bold for `family`, from the cache, downloading it
/// once when the file is absent. The OFL text is written beside the faces.
fn ensure_noto(family: CjkStyle, bold: bool) -> Option<std::path::PathBuf> {
    use std::sync::Mutex;
    static FETCH: Mutex<()> = Mutex::new(());
    let _guard = FETCH.lock().ok()?;
    let face = cached_face(family, bold);
    let dir = font_cache_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let ofl = dir.join("OFL.txt");
    if !ofl.is_file() {
        let _ = std::fs::write(&ofl, include_str!("../../../../assets/fonts/OFL.txt"));
    }
    let path = dir.join(face.file);
    if path.is_file() && file_sha256_matches(&path, face.sha) {
        return Some(path);
    }
    let bytes = download_font(face.url)?;
    if !sha256_hex(&bytes).eq_ignore_ascii_case(face.sha) || !bytes.starts_with(b"OTTO") {
        return None;
    }
    let tmp = dir.join(format!("{}.partial", face.file));
    std::fs::write(&tmp, &bytes).ok()?;
    std::fs::rename(&tmp, &path).ok()?;
    Some(path)
}

fn download_font(url: &str) -> Option<Vec<u8>> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(180)))
        .build();
    let agent: ureq::Agent = config.into();
    let mut response = agent
        .get(url)
        .header("User-Agent", "rapidpdftrans")
        .call()
        .ok()?;
    response
        .body_mut()
        .with_config()
        .limit(40 * 1024 * 1024)
        .read_to_vec()
        .ok()
}

fn file_sha256_matches(path: &std::path::Path, expect: &str) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    sha256_hex(&bytes).eq_ignore_ascii_case(expect)
}

fn sha256_hex(bytes: &[u8]) -> String {
    // Compact SHA-256 so the cache check does not need another crate.
    sha256(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn sha256(message: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (message.len() as u64).saturating_mul(8);
    let mut data = message.to_vec();
    data.push(0x80);
    while (data.len() % 64) != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in data.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut a = state[0];
        let mut b = state[1];
        let mut c = state[2];
        let mut d = state[3];
        let mut e = state[4];
        let mut f = state[5];
        let mut g = state[6];
        let mut h = state[7];
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }
    let mut out = [0u8; 32];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
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
    match subset_glyf_fonttools(data, face, codepoints) {
        GlyfAttempt::Ready(font) => return Some(font),
        // A crash used to fall through to the assembler, so the IndexError
        // was only a log line and the test still passed.
        GlyfAttempt::Failed => return None,
        GlyfAttempt::Absent => {}
    }
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
        let mut bytes = outline_bytes(glyph_slice(glyf, &offsets, *gid as usize)?);
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
        // A 10-byte zero header plus loca padding makes fontTools trim() read
        // yMax as an end point and then walk off the flag array. A zero-length
        // glyph is the empty outline.
        glyphs.push(Vec::new());
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

fn font_has_table(data: &[u8], face: Option<u32>, tag: &[u8; 4]) -> bool {
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
    selected.iter().any(|directory| {
        read_tables(data, *directory)
            .and_then(|tables| tables.get(tag.as_slice()).copied())
            .is_some()
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

/// Bytes copied into the subset. A zero-contour header is not a legal simple
/// glyph: `Glyph.trim` reads `yMax` as an end point and then indexes the flag
/// array. The empty outline is a zero-length glyf record.
fn outline_bytes(glyph: &[u8]) -> Vec<u8> {
    if glyph.len() < 10 {
        return Vec::new();
    }
    let contours = i16::from_be_bytes([glyph[0], glyph[1]]);
    if contours == 0 {
        return Vec::new();
    }
    glyph.to_vec()
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
    let maxp = maxp_from_glyphs(glyphs);
    let hhea = hhea_table(ascender, descender, num);
    let head = head_table(upem, ascender, descender);
    let post = post_table();
    let name = name_table();
    let os2 = os2_table(upem, ascender, descender);
    let mut tables = vec![
        (*b"OS/2", os2),
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

/// `maxp` version 1.0. The maxima come from the outlines that were packed.
/// A hard-coded `maxContours` of 1 is what made Windows and OTS drop every
/// glyph with a counter (a, e, o, 0, B, R, and almost every Han character)
/// while single-contour letters such as T, r, and E still painted.
fn maxp_from_glyphs(glyphs: &[Vec<u8>]) -> Vec<u8> {
    let mut max_points = 0u16;
    let mut max_contours = 0u16;
    let mut max_instructions = 0u16;
    let mut max_elements = 0u16;
    let mut points = vec![0u16; glyphs.len()];
    let mut contours = vec![0u16; glyphs.len()];
    let mut components = Vec::with_capacity(glyphs.len());
    let mut unparsed = false;
    for (index, glyph) in glyphs.iter().enumerate() {
        let Some(parsed) = parse_glyph_header(glyph) else {
            unparsed = true;
            components.push(Vec::new());
            continue;
        };
        if parsed.composite {
            max_elements =
                max_elements.max(parsed.components.len().min(usize::from(u16::MAX)) as u16);
            components.push(parsed.components);
        } else {
            max_points = max_points.max(parsed.points);
            max_contours = max_contours.max(parsed.contours);
            max_instructions = max_instructions.max(parsed.instructions);
            points[index] = parsed.points;
            contours[index] = parsed.contours;
            components.push(Vec::new());
        }
    }
    if unparsed {
        max_points = max_points.max(4096);
        max_contours = max_contours.max(64);
        max_instructions = max_instructions.max(512);
        max_elements = max_elements.max(16);
    }
    let mut max_composite_points = 0u32;
    let mut max_composite_contours = 0u32;
    let mut max_depth = 0u16;
    for index in 0..glyphs.len() {
        if components[index].is_empty() {
            continue;
        }
        let mut stack = Vec::new();
        let (sum_points, sum_contours, depth) =
            expanded_composite(index, &components, &points, &contours, &mut stack);
        max_composite_points = max_composite_points.max(sum_points);
        max_composite_contours = max_composite_contours.max(sum_contours);
        max_depth = max_depth.max(depth);
    }
    let hinted = max_instructions > 0;
    let mut t = vec![0u8; 32];
    t[0..4].copy_from_slice(&0x00010000u32.to_be_bytes());
    t[4..6].copy_from_slice(&(glyphs.len() as u16).to_be_bytes());
    t[6..8].copy_from_slice(&max_points.max(1).to_be_bytes());
    t[8..10].copy_from_slice(&max_contours.max(1).to_be_bytes());
    t[10..12].copy_from_slice(&(max_composite_points.min(u16::MAX as u32) as u16).to_be_bytes());
    t[12..14].copy_from_slice(&(max_composite_contours.min(u16::MAX as u32) as u16).to_be_bytes());
    t[14..16].copy_from_slice(&2u16.to_be_bytes());
    let twilight = if hinted { 16u16 } else { 0 };
    let storage = if hinted { 256u16 } else { 0 };
    let functions = if hinted { 256u16 } else { 0 };
    let stack = if hinted { max_instructions.max(512) } else { 0 };
    t[16..18].copy_from_slice(&twilight.to_be_bytes());
    t[18..20].copy_from_slice(&storage.to_be_bytes());
    t[20..22].copy_from_slice(&functions.to_be_bytes());
    t[24..26].copy_from_slice(&stack.to_be_bytes());
    t[26..28].copy_from_slice(&max_instructions.to_be_bytes());
    t[28..30].copy_from_slice(&max_elements.to_be_bytes());
    t[30..32].copy_from_slice(&max_depth.to_be_bytes());
    t
}

struct GlyphHeader {
    composite: bool,
    points: u16,
    contours: u16,
    instructions: u16,
    components: Vec<u16>,
}

fn parse_glyph_header(glyph: &[u8]) -> Option<GlyphHeader> {
    if glyph.len() < 10 {
        return Some(GlyphHeader {
            composite: false,
            points: 0,
            contours: 0,
            instructions: 0,
            components: Vec::new(),
        });
    }
    let contour_count = i16::from_be_bytes([glyph[0], glyph[1]]);
    if contour_count < 0 {
        return Some(GlyphHeader {
            composite: true,
            points: 0,
            contours: 0,
            instructions: 0,
            components: composite_gids(glyph),
        });
    }
    if contour_count == 0 {
        return Some(GlyphHeader {
            composite: false,
            points: 0,
            contours: 0,
            instructions: 0,
            components: Vec::new(),
        });
    }
    let count = contour_count as usize;
    let end_at = 10 + (count - 1) * 2;
    if end_at + 2 > glyph.len() {
        return None;
    }
    let last = u16::from_be_bytes([glyph[end_at], glyph[end_at + 1]]);
    let instr_at = 10 + count * 2;
    let instructions = if instr_at + 2 <= glyph.len() {
        u16::from_be_bytes([glyph[instr_at], glyph[instr_at + 1]])
    } else {
        0
    };
    Some(GlyphHeader {
        composite: false,
        points: last.saturating_add(1),
        contours: contour_count as u16,
        instructions,
        components: Vec::new(),
    })
}

fn expanded_composite(
    index: usize,
    components: &[Vec<u16>],
    points: &[u16],
    contours: &[u16],
    stack: &mut Vec<usize>,
) -> (u32, u32, u16) {
    if stack.contains(&index) || index >= components.len() {
        return (0, 0, 0);
    }
    if components[index].is_empty() {
        return (u32::from(points[index]), u32::from(contours[index]), 0);
    }
    stack.push(index);
    let mut sum_points = 0u32;
    let mut sum_contours = 0u32;
    let mut depth = 0u16;
    for gid in &components[index] {
        let (child_points, child_contours, child_depth) =
            expanded_composite(*gid as usize, components, points, contours, stack);
        sum_points = sum_points.saturating_add(child_points);
        sum_contours = sum_contours.saturating_add(child_contours);
        depth = depth.max(child_depth);
    }
    stack.pop();
    (sum_points, sum_contours, depth.saturating_add(1))
}

fn os2_table(upem: u16, ascender: i16, descender: i16) -> Vec<u8> {
    let mut t = vec![0u8; 96];
    let put_u16 = |t: &mut [u8], at: usize, value: u16| {
        t[at..at + 2].copy_from_slice(&value.to_be_bytes());
    };
    let put_i16 = |t: &mut [u8], at: usize, value: i16| {
        t[at..at + 2].copy_from_slice(&value.to_be_bytes());
    };
    put_u16(&mut t, 0, 4);
    put_i16(&mut t, 2, (upem / 2) as i16);
    put_u16(&mut t, 4, 400);
    put_u16(&mut t, 6, 5);
    put_i16(&mut t, 10, (upem / 2) as i16);
    put_i16(&mut t, 12, (upem / 2) as i16);
    put_i16(&mut t, 16, (upem / 5) as i16);
    put_i16(&mut t, 18, (upem / 2) as i16);
    put_i16(&mut t, 20, (upem / 2) as i16);
    put_i16(&mut t, 22, (upem / 5) as i16);
    put_i16(&mut t, 26, upem as i16 / 20);
    put_i16(&mut t, 28, upem as i16 / 4);
    // Basic Latin through Latin Extended-B, plus CJK Unified Ideographs.
    t[42..46].copy_from_slice(&0x0000_00FFu32.to_be_bytes());
    t[46..50].copy_from_slice(&(1u32 << 27).to_be_bytes());
    t[50..54].copy_from_slice(&(1u32 << 4).to_be_bytes());
    t[58..62].copy_from_slice(b"RPT ");
    // Regular (bit 6) and USE_TYPO_METRICS (bit 7).
    put_u16(&mut t, 62, 0x00C0);
    put_u16(&mut t, 64, 0x0020);
    put_u16(&mut t, 66, 0xFFFF);
    put_i16(&mut t, 68, ascender);
    put_i16(&mut t, 70, descender);
    let win_ascent = ascender.max(0) as u16;
    let win_descent = descender.min(0).unsigned_abs();
    put_u16(&mut t, 74, win_ascent);
    put_u16(&mut t, 76, win_descent);
    // Latin 1, Japanese, Simplified Chinese, Korean, Traditional Chinese.
    t[78..82]
        .copy_from_slice(&(1u32 | (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20)).to_be_bytes());
    put_i16(&mut t, 86, (upem / 2) as i16);
    put_i16(&mut t, 88, (upem as i16 * 7) / 10);
    put_u16(&mut t, 92, 0x0020);
    t
}

fn post_table() -> Vec<u8> {
    let mut t = vec![0u8; 32];
    t[0..4].copy_from_slice(&0x00030000u32.to_be_bytes());
    t
}

fn name_table() -> Vec<u8> {
    let records = [
        (1u16, "RPT CJK"),
        (2, "Regular"),
        (4, "RPT CJK"),
        (6, "RPTCJK"),
    ];
    let mut storage = Vec::new();
    let mut entries = Vec::new();
    for (name_id, text) in records {
        let wide: Vec<u8> = text
            .encode_utf16()
            .flat_map(|unit| unit.to_be_bytes())
            .collect();
        entries.extend_from_slice(&3u16.to_be_bytes());
        entries.extend_from_slice(&1u16.to_be_bytes());
        entries.extend_from_slice(&0x0409u16.to_be_bytes());
        entries.extend_from_slice(&name_id.to_be_bytes());
        entries.extend_from_slice(&(wide.len() as u16).to_be_bytes());
        entries.extend_from_slice(&(storage.len() as u16).to_be_bytes());
        storage.extend_from_slice(&wide);
    }
    let count = records.len() as u16;
    let string_offset = 6 + count * 12;
    let mut t = Vec::new();
    t.extend_from_slice(&0u16.to_be_bytes());
    t.extend_from_slice(&count.to_be_bytes());
    t.extend_from_slice(&string_offset.to_be_bytes());
    t.extend_from_slice(&entries);
    t.extend_from_slice(&storage);
    t
}

#[cfg(test)]
fn empty_glyph() -> Vec<u8> {
    Vec::new()
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
    fn cjk_style_accepts_only_sans_or_serif() {
        assert_eq!(CjkStyle::parse("sans").unwrap(), CjkStyle::Sans);
        assert_eq!(CjkStyle::parse(" Serif ").unwrap(), CjkStyle::Serif);
        assert_eq!(CjkStyle::parse("SANS").unwrap().as_str(), "sans");
        let err = CjkStyle::parse("song").unwrap_err();
        assert!(err.contains("sans or serif"), "{err}");
        assert!(CjkStyle::parse("").is_err());
    }

    #[test]
    fn default_style_is_noto_sans_regular_and_bold() {
        let regular = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";
        let bold_path = "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc";
        if !std::path::Path::new(regular).is_file() || !std::path::Path::new(bold_path).is_file() {
            return;
        }
        let codes = [0x4E2Du32, 0x6587];
        let sources = FontSources {
            style: Some(CjkStyle::Sans),
            ..FontSources::default()
        };
        let bold = subset_for_style(
            FaceStyle {
                serif: true,
                bold: true,
                italic: false,
            },
            &codes,
            &sources,
        )
        .expect("bold");
        let sans = subset_for_style(
            FaceStyle {
                serif: false,
                bold: false,
                italic: false,
            },
            &codes,
            &sources,
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
            face_name_contains(&bold.bytes, "Noto Sans CJK SC Bold"),
            "title face is not Heiti Bold"
        );
        assert!(
            face_name_contains(&sans.bytes, "Noto Sans CJK SC"),
            "regular sans source is not Heiti"
        );
        assert!(
            !face_name_contains(&sans.bytes, "Bold"),
            "regular sans source used Bold"
        );
        assert!(face_name_contains(&body.bytes, "Noto Sans CJK SC"));
        assert!(!face_name_contains(&body.bytes, "Bold"));
        assert!(!face_name_contains(&body.bytes, "Noto Serif"));
        assert!(bold.fallback.is_none() && bold.notice.is_none());
    }

    #[test]
    fn serif_style_uses_noto_serif_cjk_sc() {
        let bold_path = "/usr/share/fonts/opentype/noto/NotoSerifCJK-Bold.ttc";
        let regular = "/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc";
        if !std::path::Path::new(bold_path).is_file() || !std::path::Path::new(regular).is_file() {
            return;
        }
        let sources = FontSources {
            style: Some(CjkStyle::Serif),
            ..FontSources::default()
        };
        let codes = [0x4E2Du32, 0x6587];
        let bold = subset_for_style(
            FaceStyle {
                serif: false,
                bold: true,
                italic: false,
            },
            &codes,
            &sources,
        )
        .expect("serif bold");
        let body = subset_for_style(
            FaceStyle {
                serif: true,
                bold: false,
                italic: false,
            },
            &codes,
            &sources,
        )
        .expect("serif body");
        assert!(
            face_name_contains(&bold.bytes, "Noto Serif CJK SC"),
            "serif bold is not Song"
        );
        assert!(face_name_contains(&bold.bytes, "Bold"));
        assert!(face_name_contains(&body.bytes, "Noto Serif CJK"));
        assert!(!face_name_contains(&body.bytes, "Noto Sans"));
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
    fn a_user_font_missing_a_character_falls_back_to_the_selected_family() {
        let sans = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";
        let serif = "/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc";
        if !std::path::Path::new(sans).is_file() || !std::path::Path::new(serif).is_file() {
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
        let style = FaceStyle {
            serif: true,
            bold: false,
            italic: false,
        };
        let codes = [b'A' as u32, 0x4E2D, 0x4E2D];
        for (family, needle) in [
            (CjkStyle::Sans, "Noto Sans CJK"),
            (CjkStyle::Serif, "Noto Serif CJK"),
        ] {
            let sources = FontSources {
                serif: Some(path.clone()),
                style: Some(family),
                ..FontSources::default()
            };
            let font = subset_for_style(style, &codes, &sources).expect("user font");
            assert!(font.glyphs.contains_key(&(b'A' as u32)));
            assert!(!font.glyphs.contains_key(&0x4E2D));
            let fallback = font.fallback.as_ref().expect("builtin fallback");
            assert!(fallback.glyphs.contains_key(&0x4E2D));
            assert!(face_name_contains(&fallback.bytes, needle), "{needle}");
            let note = font.notice.as_deref().expect("missing-glyph warning");
            assert_eq!(note.matches("U+4E2D").count(), 1, "{note}");
            assert!(note.contains(needle), "{note}");
            assert!(font.has(0x4E2D));
            assert!(font.em_advance(0x4E2D).is_some());
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sha256_matches_known_digests() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn rust_glyf_subset_maxp_covers_multi_contour_letters() {
        let path = "/usr/share/fonts/truetype/liberation/LiberationSerif-Regular.ttf";
        if !std::path::Path::new(path).is_file() {
            return;
        }
        let bytes = std::fs::read(path).unwrap();
        let codes = [0x61u32, 0x65, 0x6f, 0x30, 0x42, 0x52, 0x54, 0x45];
        let font = subset_face(&bytes, 0, &codes).expect("rust subset");
        assert!(font.bytes.starts_with(&[0, 1, 0, 0]), "standalone TTF");
        assert!(font.glyphs.contains_key(&0x61));
        assert_maxp_covers_glyf(&font.bytes);
        assert!(
            table_present(&font.bytes, b"OS/2"),
            "Windows loaders require OS/2"
        );
    }

    #[test]
    fn uming_collection_subset_keeps_contours_and_composites() {
        let path = "/usr/share/fonts/truetype/arphic/uming.ttc";
        if !std::path::Path::new(path).is_file() {
            return;
        }
        let codes = [
            0x61u32, 0x65, 0x6f, 0x30, 0x42, 0x52, 0x4E2D, 0x6587, 0x8BED,
        ];
        let font = subset_for_style(
            FaceStyle {
                serif: true,
                bold: false,
                italic: false,
            },
            &codes,
            &FontSources {
                serif: Some(format!("{path}#0").into()),
                bold: Some(format!("{path}#0").into()),
                style: Some(CjkStyle::Sans),
                ..FontSources::default()
            },
        )
        .expect("uming");
        assert!(font.glyphs.contains_key(&0x4E2D), "missing 中");
        assert!(font.glyphs.contains_key(&0x61), "missing a");
        assert!(font.glyphs.contains_key(&0x30), "missing 0");
        assert!(
            font.bytes.starts_with(&[0, 1, 0, 0]),
            "FontFile2 must be a TTF, not a TTC: {:02x?}",
            &font.bytes[..4.min(font.bytes.len())]
        );
        assert_maxp_covers_glyf(&font.bytes);
        assert!(
            !face_name_contains(&font.bytes, "Noto Sans CJK SC"),
            "an explicit TrueType collection lost to the style"
        );
    }

    #[test]
    fn an_explicit_bold_collection_wins_over_style() {
        let path = "/usr/share/fonts/truetype/arphic/uming.ttc";
        let sans = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";
        if !std::path::Path::new(path).is_file() || !std::path::Path::new(sans).is_file() {
            return;
        }
        let sources = FontSources {
            bold: Some(format!("{path}#0").into()),
            style: Some(CjkStyle::Serif),
            ..FontSources::default()
        };
        let codes = [0x4E2Du32, 0x61, 0x30];
        let body = subset_for_style(
            FaceStyle {
                serif: true,
                bold: false,
                italic: false,
            },
            &codes,
            &sources,
        )
        .expect("body");
        let bold = subset_for_style(
            FaceStyle {
                serif: true,
                bold: true,
                italic: false,
            },
            &codes,
            &sources,
        )
        .expect("bold");
        assert!(
            face_name_contains(&body.bytes, "Noto Serif CJK"),
            "unset regular face ignored the serif style"
        );
        assert!(
            bold.bytes.starts_with(&[0, 1, 0, 0]),
            "explicit bold TTC was not embedded as a TTF"
        );
        assert!(!face_name_contains(&bold.bytes, "Noto Serif CJK SC"));
    }

    #[test]
    fn cached_noto_serves_a_clean_machine() {
        if std::env::var("RPT_FONT_SMOKE").ok().as_deref() != Some("1") {
            return;
        }
        let faces = [
            (CjkStyle::Sans, false, "Noto Sans CJK SC"),
            (CjkStyle::Sans, true, "Noto Sans CJK SC"),
            (CjkStyle::Serif, false, "Noto Serif CJK SC"),
            (CjkStyle::Serif, true, "Noto Serif CJK SC"),
        ];
        for (family, bold, needle) in faces {
            let path = ensure_noto(family, bold).expect("download");
            let font = subset_cff(path.to_str().unwrap(), 0, &[0x4E2D, 0x61, 0x30, 0x6587])
                .expect("subset");
            assert!(font.glyphs.contains_key(&0x4E2D), "{}", path.display());
            assert!(font.glyphs.contains_key(&0x61), "{}", path.display());
            assert!(font.glyphs.contains_key(&0x30), "{}", path.display());
            assert!(
                face_name_contains(&font.bytes, needle),
                "{} is not {needle}",
                path.display()
            );
            if bold {
                assert!(
                    face_name_contains(&font.bytes, "Bold"),
                    "{} is not Bold",
                    path.display()
                );
            }
        }
        assert!(font_cache_dir().join("OFL.txt").is_file());
    }

    fn table_present(font: &[u8], tag: &[u8; 4]) -> bool {
        table_offset(font, tag).is_some()
    }

    fn assert_maxp_covers_glyf(font: &[u8]) {
        let path = std::env::temp_dir().join(format!(
            "rpt-maxp-{}-{}.ttf",
            std::process::id(),
            next_stamp()
        ));
        std::fs::write(&path, font).unwrap();
        let mut command = python_command().expect("python");
        let output = command
            .arg("-c")
            .arg(
                r#"
import sys
from fontTools.ttLib import TTFont
from fontTools.ttLib.tables._g_l_y_f import Glyph as RawGlyph
font = TTFont(sys.argv[1])
if font.sfntVersion not in ("\x00\x01\x00\x00",):
    raise SystemExit(f"not a TTF: {font.sfntVersion!r}")
required = ["cmap", "glyf", "head", "hhea", "hmtx", "loca", "maxp", "name", "OS/2", "post"]
missing = [name for name in required if name not in font]
if missing:
    raise SystemExit("missing " + ",".join(missing))
maxp = font["maxp"]
glyf = font["glyf"]
for name in font.getGlyphOrder():
    # glyf[name] expands the outline and trim() then skips the raw bytes.
    # prune_post_subset trims the compact record, which is where the
    # IndexError was raised.
    raw = bytes(getattr(glyf.glyphs[name], "data", b"") or b"")
    try:
        RawGlyph(raw).trim(remove_hinting=False)
    except Exception as exc:
        raise SystemExit(f"{name} glyf record is not trimmable: {exc}")
    glyph = glyf[name]
    contours = glyph.numberOfContours
    if contours >= 0 and contours > int(maxp.maxContours or 0):
        raise SystemExit(f"{name} contours {contours} > maxp {maxp.maxContours}")
    if contours > 0:
        data = bytes(getattr(glyph, "data", b"") or b"")
        if len(data) >= 10 + contours * 2:
            last = int.from_bytes(data[8 + contours * 2 : 10 + contours * 2], "big")
            if last + 1 > int(maxp.maxPoints or 0):
                raise SystemExit(f"{name} points {last + 1} > maxp {maxp.maxPoints}")
    if glyph.isComposite():
        if len(glyph.components) > int(maxp.maxComponentElements or 0):
            raise SystemExit(f"{name} components {len(glyph.components)} > maxp")
        for component in glyph.components:
            if component.glyphName not in glyf:
                raise SystemExit(f"{name} dropped {component.glyphName}")
"#,
            )
            .arg(&path)
            .output()
            .expect("python");
        let _ = std::fs::remove_file(&path);
        let detail = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "maxp does not cover the glyf outlines: {detail}"
        );
    }

    #[test]
    fn fonttools_glyf_subset_keeps_the_source_name() {
        let path = "/usr/share/fonts/truetype/liberation/LiberationSerif-Regular.ttf";
        if !std::path::Path::new(path).is_file() || python_command().is_none() {
            return;
        }
        let bytes = std::fs::read(path).unwrap();
        let font = subset_ttf(&bytes, &[0x61, 0x65, 0x6F, 0x30, 0x42])
            .expect("fontTools glyf subset failed; a crash must not fall back to the assembler");
        assert!(
            face_name_contains(&font.bytes, "Liberation"),
            "glyf subset lost the source name, so the Rust assembler ran"
        );
        assert_maxp_covers_glyf(&font.bytes);
    }

    #[test]
    fn rust_glyf_subset_of_a_collection_has_legal_flags() {
        let path = "/usr/share/fonts/truetype/arphic/uming.ttc";
        if !std::path::Path::new(path).is_file() {
            return;
        }
        let bytes = std::fs::read(path).unwrap();
        let _guard = RustGlyfOnly::enter();
        let font = subset_ttf_face(&bytes, Some(0), &[0x4E2D, 0x6587, 0x61, 0x65, 0x6F, 0x30])
            .expect("rust glyf subset");
        drop(_guard);
        assert!(
            face_name_contains(&font.bytes, "RPT CJK"),
            "Rust assembler did not replace the name"
        );
        assert!(
            !face_name_contains(&font.bytes, "UMing"),
            "collection subset still carries the source name"
        );
        assert!(font.glyphs.contains_key(&0x4E2D), "missing 中");
        if python_command().is_some() {
            assert_maxp_covers_glyf(&font.bytes);
        }
    }
}
