//! Score a translated PDF against its source.
//!
//! The same function scores every engine, including an identity copy of the
//! source (the ceiling) and, once rewrite exists, RapidPdfTrans output.
//! Metrics are layout and conservation, not a preference for one language:
//!
//! - `line_coverage` / `drop_rate`: source text lines whose box still meets an
//!   output glyph. A line with no output glyph was dropped or moved off its box.
//! - `protected_recall`: URLs, emails, `{braces}`, numbers, and `⟦N⟧`
//!   placeholders from the source plain text that still occur in the output.
//! - `formula_integrity`: math-font runs kept either as the same Unicode or as
//!   glyphs still sitting on that run's box.
//! - `overflow_rate`: output glyphs that extend past the page box.
//! - `style_retention`: output glyphs within 0.8 pt of a source glyph whose
//!   size and bold/italic class match that glyph.
//! - `identity_char_retention`: source characters still present in the output.
//!   This is the reference metric when the shared translator is identity.
//!   A real zh translation lowers it on purpose; do not read it as dropped text.
//! - `reference_byte_identity`: text-showing operators in the detected References
//!   section whose bytes are unchanged in the output. Vacuous (1.0) when the
//!   source has no bibliography. A rewrite that keeps that section original
//!   scores 1.0; editing those operators lowers it.
//! - non-text SSIM: figures and rules, with source glyph boxes masked.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicU64;

static WORK_TICK: AtomicU64 = AtomicU64::new(0);

use rpt_core::{
    citation_end, identical_reference_operators, ExtractOptions, Extraction, Glyph, PageInfo,
    PdfDocument,
};
use serde::Serialize;

use crate::render::compare_renders;

#[derive(Clone, Debug)]
pub struct ScoreOptions {
    pub render_pages: u32,
    pub max_pages: Option<u32>,
}

impl Default for ScoreOptions {
    fn default() -> Self {
        Self {
            render_pages: 1,
            max_pages: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct FontCount {
    pub name: String,
    pub count: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct PairScore {
    pub source_pages: usize,
    pub output_pages: usize,
    pub source_glyphs: usize,
    pub output_glyphs: usize,
    pub source_unmapped: usize,
    pub source_unmapped_ratio: f32,
    pub output_unmapped: usize,
    pub lines: usize,
    pub lines_covered: usize,
    pub line_coverage: f32,
    pub drop_rate: f32,
    pub protected_tokens: usize,
    pub protected_kept: usize,
    pub protected_recall: f32,
    pub formulas: usize,
    pub formulas_kept: usize,
    pub formula_integrity: f32,
    pub formula_unreadable: usize,
    pub overflow_glyphs: usize,
    pub visible_output_glyphs: usize,
    pub overflow_rate: f32,
    pub style_compared: usize,
    pub style_kept: usize,
    pub style_retention: f32,
    pub identity_char_retention: f32,
    pub source_chars: usize,
    pub reference_operators: usize,
    pub reference_operators_identical: usize,
    /// 1.0 when every reference-section text operator is byte-identical, or
    /// when the source has no reference section.
    pub reference_byte_identity: f32,
    pub mean_ssim: Option<f32>,
    pub mean_nontext_ssim: Option<f32>,
    pub render_note: String,
    pub unmapped_fonts: Vec<FontCount>,
    /// False when an embedded translation font is an OpenType wrapper declared
    /// as CIDFontType0. Poppler then paints the wrong outlines while ToUnicode
    /// still extracts the translation, so drop/style scores stay high.
    pub paint_font_ok: bool,
    /// Output lines whose horizontal span sits inside a nearby source line.
    /// A translation drawn past the source column scores below 1.
    pub horizontal_containment: f32,
    /// CJK characters in our extraction that `pdftotext` also returns.
    /// Vacuous (1) when the output has no CJK.
    pub cjk_extract_ratio: f32,
}

struct Line {
    page: u32,
    bbox: [f32; 4],
    glyphs: Vec<Glyph>,
}

struct Formula {
    page: u32,
    bbox: [f32; 4],
    text: String,
    unreadable: bool,
}

pub fn score_pair(source: &Path, output: &Path, opts: &ScoreOptions) -> Result<PairScore, String> {
    let extract = ExtractOptions {
        max_pages: opts.max_pages,
        ..ExtractOptions::default()
    };
    let source_doc = PdfDocument::open(source).map_err(|err| err.to_string())?;
    let output_doc = PdfDocument::open(output).map_err(|err| err.to_string())?;
    let source_ex = source_doc.extract_with(&extract);
    let output_ex = output_doc.extract_with(&extract);
    score_extractions(source, output, &source_ex, &output_ex, opts)
}

pub fn score_extractions(
    source: &Path,
    output: &Path,
    source_ex: &Extraction,
    output_ex: &Extraction,
    opts: &ScoreOptions,
) -> Result<PairScore, String> {
    let lines = lines_of(source_ex);
    let output_glyphs: Vec<&Glyph> = output_ex
        .glyphs
        .iter()
        .filter(|glyph| !glyph.invisible)
        .collect();
    let lines_covered = lines
        .iter()
        .filter(|line| {
            output_glyphs
                .iter()
                .any(|glyph| glyph.page_index == line.page && overlaps(line.bbox, glyph.bbox, 2.0))
        })
        .count();
    let formulas = formulas_of(&lines);
    let output_text = squash(&output_ex.plain_text());
    let formulas_kept = formulas
        .iter()
        .filter(|formula| formula_kept(formula, &output_text, &output_glyphs))
        .count();
    let formula_unreadable = formulas.iter().filter(|formula| formula.unreadable).count();
    let protected = protected_tokens(&source_ex.plain_text());
    let protected_kept = protected
        .iter()
        .filter(|token| output_text.contains(&squash(token)))
        .count();
    let mut overflow_glyphs = 0usize;
    let mut style_compared = 0usize;
    let mut style_kept = 0usize;
    for glyph in &output_glyphs {
        let Some(page) = page_info(output_ex, glyph.page_index) else {
            continue;
        };
        if extends_past_page(glyph, page) {
            overflow_glyphs += 1;
        }
        if let Some(source_glyph) = nearest_source_glyph(&lines, glyph) {
            style_compared += 1;
            if style_matches(glyph, source_glyph) {
                style_kept += 1;
            }
        }
    }
    let render_pages = opts
        .render_pages
        .min(source_ex.pages.len() as u32)
        .min(output_ex.pages.len() as u32);
    let render = if render_pages == 0 {
        None
    } else {
        let n = WORK_TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let work = std::env::temp_dir().join(format!("rpt-bench-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        Some(compare_renders(
            source,
            output,
            source_ex,
            render_pages,
            &work,
        ))
    };
    let (mean_ssim, mean_nontext_ssim, render_note) = match &render {
        None => (None, None, "no pages to render".into()),
        Some(report) if !report.available || report.pages.is_empty() => {
            (None, None, report.note.clone())
        }
        Some(report) => {
            let ssim = mean_opt(report.pages.iter().filter_map(|page| page.ssim));
            let nontext = mean_opt(report.pages.iter().filter_map(|page| page.nontext_ssim));
            (ssim, nontext, report.note.clone())
        }
    };
    let source_unmapped = source_ex
        .glyphs
        .iter()
        .filter(|glyph| glyph.unmapped)
        .count();
    let (identity_char_retention, source_chars) =
        char_retention(&source_ex.plain_text(), &output_ex.plain_text());
    let src_doc = PdfDocument::open(source).map_err(|err| err.to_string())?;
    let out_doc = PdfDocument::open(output).map_err(|err| err.to_string())?;
    let (reference_operators_identical, reference_operators) =
        identical_reference_operators(&src_doc, &out_doc, &source_ex.glyphs);
    let output_lines = lines_of(output_ex);
    let horizontal_containment = horizontal_containment(&lines, &output_lines);
    let paint_font_ok = embedded_translation_font_ok(output);
    let cjk_extract_ratio = cjk_extract_ratio(output, output_ex);
    Ok(PairScore {
        source_pages: source_ex.pages.len(),
        output_pages: output_ex.pages.len(),
        source_glyphs: source_ex.glyphs.len(),
        output_glyphs: output_ex.glyphs.len(),
        source_unmapped,
        source_unmapped_ratio: ratio(source_unmapped, source_ex.glyphs.len()),
        output_unmapped: output_ex
            .glyphs
            .iter()
            .filter(|glyph| glyph.unmapped)
            .count(),
        lines: lines.len(),
        lines_covered,
        line_coverage: ratio_or_one(lines_covered, lines.len()),
        drop_rate: if lines.is_empty() {
            0.0
        } else {
            1.0 - ratio(lines_covered, lines.len())
        },
        protected_tokens: protected.len(),
        protected_kept,
        protected_recall: ratio_or_one(protected_kept, protected.len()),
        formulas: formulas.len(),
        formulas_kept,
        formula_integrity: ratio_or_one(formulas_kept, formulas.len()),
        formula_unreadable,
        overflow_glyphs,
        visible_output_glyphs: output_glyphs.len(),
        overflow_rate: ratio(overflow_glyphs, output_glyphs.len()),
        style_compared,
        style_kept,
        style_retention: ratio_or_one(style_kept, style_compared),
        identity_char_retention,
        source_chars,
        reference_operators,
        reference_operators_identical,
        reference_byte_identity: ratio_or_one(reference_operators_identical, reference_operators),
        mean_ssim,
        mean_nontext_ssim,
        render_note,
        unmapped_fonts: unmapped_fonts(source_ex),
        paint_font_ok,
        horizontal_containment,
        cjk_extract_ratio,
    })
}

fn horizontal_containment(source: &[Line], output: &[Line]) -> f32 {
    if output.is_empty() {
        return 1.0;
    }
    let mut inside = 0usize;
    for line in output {
        let x0 = line.bbox[0].min(line.bbox[2]);
        let x1 = line.bbox[0].max(line.bbox[2]);
        let y = line_baseline(line);
        let size = line
            .glyphs
            .first()
            .map(|glyph| glyph.font_size)
            .unwrap_or(10.0)
            .max(1.0);
        let tol = 3.0f32;
        let mut spans: Vec<(f32, f32)> = source
            .iter()
            .filter(|src| src.page == line.page && (line_baseline(src) - y).abs() <= size * 2.4)
            .map(|src| (src.bbox[0].min(src.bbox[2]), src.bbox[0].max(src.bbox[2])))
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut merged: Vec<(f32, f32)> = Vec::new();
        for (left, right) in spans {
            if let Some(last) = merged.last_mut() {
                // Word gaps on one baseline join. A column gutter stays split.
                if left <= last.1 + 20.0 {
                    last.1 = last.1.max(right);
                    continue;
                }
            }
            merged.push((left, right));
        }
        if merged
            .iter()
            .any(|(left, right)| x0 >= left - tol && x1 <= right + tol)
        {
            inside += 1;
        }
    }
    ratio(inside, output.len())
}

fn embedded_translation_font_ok(pdf: &Path) -> bool {
    let output = match Command::new("pdffonts").arg(pdf).output() {
        Ok(output) if output.status.success() => output,
        _ => return false,
    };
    let listing = String::from_utf8_lossy(&output.stdout);
    for line in listing.lines() {
        if line.contains("RPTCJK") && line.contains("(OT)") {
            return false;
        }
    }
    true
}

fn cjk_extract_ratio(pdf: &Path, extraction: &Extraction) -> f32 {
    let mut expected: HashMap<char, u32> = HashMap::new();
    for glyph in &extraction.glyphs {
        for ch in glyph.unicode.chars().filter(|ch| is_cjk_char(*ch)) {
            *expected.entry(ch).or_insert(0) += 1;
        }
    }
    let total: u32 = expected.values().sum();
    if total == 0 {
        return 1.0;
    }
    let output = match Command::new("pdftotext")
        .args(["-enc", "UTF-8", "-q"])
        .arg(pdf)
        .arg("-")
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return 0.0,
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut found: HashMap<char, u32> = HashMap::new();
    for ch in text.chars().filter(|ch| is_cjk_char(*ch)) {
        *found.entry(ch).or_insert(0) += 1;
    }
    let mut hit = 0u32;
    for (ch, count) in &expected {
        hit += (*count).min(*found.get(ch).unwrap_or(&0));
    }
    hit as f32 / total as f32
}

fn is_cjk_char(ch: char) -> bool {
    ('\u{3400}'..='\u{9FFF}').contains(&ch) || ('\u{F900}'..='\u{FAFF}').contains(&ch)
}

fn lines_of(extraction: &Extraction) -> Vec<Line> {
    let mut glyphs: Vec<&Glyph> = extraction
        .glyphs
        .iter()
        .filter(|glyph| !glyph.invisible)
        .collect();
    glyphs.sort_by(|a, b| {
        a.page_index
            .cmp(&b.page_index)
            .then(b.matrix[5].total_cmp(&a.matrix[5]))
            .then(a.matrix[4].total_cmp(&b.matrix[4]))
    });
    let mut lines: Vec<Line> = Vec::new();
    for glyph in glyphs {
        let attach = lines.last().is_some_and(|line| {
            let line_right = line.bbox[0].max(line.bbox[2]);
            let glyph_left = glyph.bbox[0].min(glyph.bbox[2]);
            let gap = glyph_left - line_right;
            let size = line
                .glyphs
                .last()
                .map(|last| last.font_size)
                .unwrap_or(10.0)
                .max(1.0);
            // Only extend a line to the right. A glyph in the other column has
            // a close baseline but sits far to the left; `left <= right + 24`
            // used to absorb it and score the union as one overflowing line.
            line.page == glyph.page_index
                && (line_baseline(line) - glyph.matrix[5]).abs() <= 2.0
                && (-size..=24.0).contains(&gap)
        });
        if attach {
            let line = lines.last_mut().unwrap();
            line.bbox = union(line.bbox, glyph.bbox);
            line.glyphs.push(glyph.clone());
        } else {
            lines.push(Line {
                page: glyph.page_index,
                bbox: glyph.bbox,
                glyphs: vec![glyph.clone()],
            });
        }
    }
    lines
}

fn line_baseline(line: &Line) -> f32 {
    line.glyphs
        .last()
        .map(|glyph| glyph.matrix[5])
        .unwrap_or(0.0)
}

fn formulas_of(lines: &[Line]) -> Vec<Formula> {
    let mut formulas = Vec::new();
    for line in lines {
        let mut run: Vec<&Glyph> = Vec::new();
        let flush = |run: &mut Vec<&Glyph>, formulas: &mut Vec<Formula>| {
            if run.is_empty() {
                return;
            }
            let text: String = run.iter().map(|glyph| glyph.unicode.as_str()).collect();
            let unreadable = run
                .iter()
                .all(|glyph| glyph.unmapped || glyph.unicode.is_empty());
            let mut bbox = run[0].bbox;
            for glyph in run.iter().skip(1) {
                bbox = union(bbox, glyph.bbox);
            }
            formulas.push(Formula {
                page: run[0].page_index,
                bbox,
                text,
                unreadable,
            });
            run.clear();
        };
        for glyph in &line.glyphs {
            if is_math_font(&glyph.font_name) {
                run.push(glyph);
            } else {
                flush(&mut run, &mut formulas);
            }
        }
        flush(&mut run, &mut formulas);
    }
    formulas
}

fn formula_kept(formula: &Formula, output_text: &str, output_glyphs: &[&Glyph]) -> bool {
    let visual = output_glyphs
        .iter()
        .any(|glyph| glyph.page_index == formula.page && overlaps(formula.bbox, glyph.bbox, 2.0));
    if formula.unreadable {
        return visual;
    }
    let needle = squash(&formula.text);
    (!needle.is_empty() && output_text.contains(&needle)) || visual
}

fn protected_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(end) = take_placeholder(text, i)
            .or_else(|| take_url(text, i))
            .or_else(|| take_email(text, i))
            .or_else(|| take_braces(text, i))
            .or_else(|| citation_end(text, i))
            .or_else(|| take_number(text, i))
        {
            tokens.push(text[i..end].to_string());
            i = end;
            continue;
        }
        i += text[i..]
            .chars()
            .next()
            .map(|ch| ch.len_utf8())
            .unwrap_or(1);
    }
    tokens
}

fn take_placeholder(text: &str, i: usize) -> Option<usize> {
    let rest = text.get(i..)?;
    let rest = rest.strip_prefix('⟦')?;
    let end = rest.find('⟧')?;
    if end > 0 && rest[..end].bytes().all(|b| b.is_ascii_digit()) {
        Some(i + '⟦'.len_utf8() + end + '⟧'.len_utf8())
    } else {
        None
    }
}

fn take_url(text: &str, i: usize) -> Option<usize> {
    let rest = text.get(i..)?;
    let prefix = if rest.starts_with("https://") {
        8
    } else if rest.starts_with("http://") {
        7
    } else {
        return None;
    };
    let bytes = rest.as_bytes();
    let mut end = prefix;
    while end < bytes.len() && !bytes[end].is_ascii_whitespace() && bytes[end] != b'"' {
        end += 1;
    }
    while end > prefix && matches!(bytes[end - 1], b'.' | b',' | b';' | b')') {
        end -= 1;
    }
    (end > prefix).then_some(i + end)
}

fn take_email(text: &str, i: usize) -> Option<usize> {
    let rest = text.get(i..)?;
    let bytes = rest.as_bytes();
    let mut p = 0;
    while p < bytes.len() && is_email_byte(bytes[p]) && bytes[p] != b'@' {
        p += 1;
    }
    if p == 0 || p >= bytes.len() || bytes[p] != b'@' {
        return None;
    }
    p += 1;
    let domain = p;
    while p < bytes.len() && is_email_byte(bytes[p]) {
        p += 1;
    }
    let host = rest.get(domain..p)?;
    let dot = host.rfind('.')?;
    let tld = &host[dot + 1..];
    if tld.len() < 2 || !tld.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    Some(i + p)
}

fn is_email_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn take_braces(text: &str, i: usize) -> Option<usize> {
    let rest = text.get(i..)?;
    if !rest.starts_with('{') {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut p = 1;
    while p < bytes.len() && bytes[p] != b'}' && bytes[p] != b'{' && bytes[p] != b'\n' {
        p += 1;
    }
    (p > 1 && p < bytes.len() && bytes[p] == b'}').then_some(i + p + 1)
}

fn take_number(text: &str, i: usize) -> Option<usize> {
    let rest = text.get(i..)?;
    let bytes = rest.as_bytes();
    if !bytes.first().is_some_and(|b| b.is_ascii_digit()) {
        return None;
    }
    if i > 0 {
        let prev = text[..i].chars().next_back()?;
        if prev.is_ascii_alphanumeric() {
            return None;
        }
    }
    let mut p = 0;
    while p < bytes.len() && bytes[p].is_ascii_digit() {
        p += 1;
    }
    if p < bytes.len() && bytes[p] == b'.' {
        let mut q = p + 1;
        let frac = q;
        while q < bytes.len() && bytes[q].is_ascii_digit() {
            q += 1;
        }
        if q > frac {
            p = q;
        }
    }
    let next = text.get(i + p..).and_then(|s| s.chars().next());
    if next.is_some_and(|ch| ch.is_ascii_alphanumeric()) {
        return None;
    }
    Some(i + p)
}

fn nearest_source_glyph<'a>(lines: &'a [Line], glyph: &Glyph) -> Option<&'a Glyph> {
    lines
        .iter()
        .filter(|line| line.page == glyph.page_index)
        .flat_map(|line| &line.glyphs)
        .filter(|source| glyph_distance(source, glyph) <= 0.8)
        .min_by(|a, b| glyph_distance(a, glyph).total_cmp(&glyph_distance(b, glyph)))
}

fn glyph_distance(a: &Glyph, b: &Glyph) -> f32 {
    let dx = a.matrix[4] - b.matrix[4];
    let dy = a.matrix[5] - b.matrix[5];
    dx.hypot(dy)
}

fn style_matches(glyph: &Glyph, source: &Glyph) -> bool {
    let size_ok = (glyph.font_size - source.font_size).abs() <= source.font_size.abs() * 0.20 + 0.5;
    size_ok && style_class(&glyph.font_name) == style_class(&source.font_name)
}

fn style_class(name: &str) -> (bool, bool) {
    let upper = name.to_ascii_uppercase();
    let bold = ["BOLD", "CMBX", "BLACK", "HEAVY", "SEMIBOLD"]
        .iter()
        .any(|needle| upper.contains(needle));
    let italic = ["ITAL", "OBLIQUE", "CMTI", "CMMI"]
        .iter()
        .any(|needle| upper.contains(needle));
    (bold, italic)
}

fn is_math_font(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "CMMI",
        "CMSY",
        "CMEX",
        "MSAM",
        "MSBM",
        "STIX",
        "CAMBRIAMATH",
        "LATINMODERNMATH",
        "SYMBOL",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
}

fn extends_past_page(glyph: &Glyph, page: &PageInfo) -> bool {
    const MARGIN: f32 = 1.0;
    let box_ = page.media_box;
    let x0 = glyph.bbox[0].min(glyph.bbox[2]);
    let x1 = glyph.bbox[0].max(glyph.bbox[2]);
    let y0 = glyph.bbox[1].min(glyph.bbox[3]);
    let y1 = glyph.bbox[1].max(glyph.bbox[3]);
    x0 < box_[0] - MARGIN || x1 > box_[2] + MARGIN || y0 < box_[1] - MARGIN || y1 > box_[3] + MARGIN
}

fn page_info(extraction: &Extraction, index: u32) -> Option<&PageInfo> {
    extraction.pages.iter().find(|page| page.index == index)
}

fn overlaps(a: [f32; 4], b: [f32; 4], pad: f32) -> bool {
    let (ax0, ay0, ax1, ay1) = corners(a);
    let (bx0, by0, bx1, by1) = corners(b);
    ax0 - pad < bx1 && ax1 + pad > bx0 && ay0 - pad < by1 && ay1 + pad > by0
}

fn corners(box_: [f32; 4]) -> (f32, f32, f32, f32) {
    (
        box_[0].min(box_[2]),
        box_[1].min(box_[3]),
        box_[0].max(box_[2]),
        box_[1].max(box_[3]),
    )
}

fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let (ax0, ay0, ax1, ay1) = corners(a);
    let (bx0, by0, bx1, by1) = corners(b);
    [ax0.min(bx0), ay0.min(by0), ax1.max(bx1), ay1.max(by1)]
}

fn squash(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn char_retention(source: &str, output: &str) -> (f32, usize) {
    let mut counts = HashMap::<char, u32>::new();
    for ch in output.chars().filter(|ch| !ch.is_whitespace()) {
        *counts.entry(ch).or_insert(0) += 1;
    }
    let mut total = 0usize;
    let mut hit = 0usize;
    for ch in source.chars().filter(|ch| !ch.is_whitespace()) {
        total += 1;
        if let Some(left) = counts.get_mut(&ch) {
            if *left > 0 {
                *left -= 1;
                hit += 1;
            }
        }
    }
    (ratio_or_one(hit, total), total)
}

fn unmapped_fonts(extraction: &Extraction) -> Vec<FontCount> {
    let mut counts = HashMap::<String, usize>::new();
    for glyph in extraction.glyphs.iter().filter(|glyph| glyph.unmapped) {
        let name = if glyph.font_name.is_empty() {
            glyph.font_resource.clone()
        } else {
            glyph.font_name.clone()
        };
        *counts.entry(name).or_insert(0) += 1;
    }
    let mut rows: Vec<FontCount> = counts
        .into_iter()
        .map(|(name, count)| FontCount { name, count })
        .collect();
    rows.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
    rows.truncate(8);
    rows
}

fn ratio(part: usize, whole: usize) -> f32 {
    if whole == 0 {
        0.0
    } else {
        part as f32 / whole as f32
    }
}

fn ratio_or_one(part: usize, whole: usize) -> f32 {
    if whole == 0 {
        1.0
    } else {
        part as f32 / whole as f32
    }
}

fn mean_opt(values: impl Iterator<Item = f32>) -> Option<f32> {
    let values: Vec<f32> = values.collect();
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f32>() / values.len() as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Document, Object, Stream};
    use std::path::PathBuf;

    fn write_pdf(path: &std::path::Path, lines: &[(&str, i32)]) {
        let mut doc = Document::with_version("1.4");
        doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        let pages_id = doc.new_object_id();
        let font = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
            "FirstChar" => 32,
            "LastChar" => 122,
            "Widths" => (32..=122).map(|_| Object::Integer(500)).collect::<Vec<_>>(),
        });
        let mut fonts = lopdf::Dictionary::new();
        fonts.set("F1", font);
        let mut resources = lopdf::Dictionary::new();
        resources.set("Font", fonts);
        let mut content = String::from("BT /F1 12 Tf ");
        for (text, y) in lines {
            content.push_str(&format!("1 0 0 1 72 {y} Tm ({text}) Tj "));
        }
        content.push_str("ET");
        let content_id =
            doc.add_object(Stream::new(lopdf::Dictionary::new(), content.into_bytes()));
        let page = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content_id,
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
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn identity_copy_keeps_text_and_a_dropped_line_scores_worse() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/bench-fixtures");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("two-lines.pdf");
        let dropped = dir.join("one-line.pdf");
        write_pdf(
            &source,
            &[("Hello 0.25 https://example.com/a", 700), ("World", 640)],
        );
        write_pdf(&dropped, &[("Hello 0.25 https://example.com/a", 700)]);
        let opts = ScoreOptions {
            render_pages: 1,
            max_pages: Some(1),
        };
        let same = score_pair(&source, &source, &opts).unwrap();
        assert_eq!(same.lines, 2);
        assert_eq!(same.drop_rate, 0.0);
        assert_eq!(same.protected_tokens, 2);
        assert_eq!(same.protected_recall, 1.0);
        assert_eq!(same.overflow_glyphs, 0);
        assert_eq!(same.style_retention, 1.0);
        assert!(same.identity_char_retention > 0.99);
        assert!(same.paint_font_ok);
        assert!(same.horizontal_containment > 0.99);
        assert!(same.cjk_extract_ratio > 0.99);
        if let Some(ssim) = same.mean_nontext_ssim {
            assert!(ssim > 0.99);
        }
        let worse = score_pair(&source, &dropped, &opts).unwrap();
        assert!(worse.drop_rate > same.drop_rate);
        assert!(worse.lines_covered < same.lines_covered);
        assert!(worse.identity_char_retention < same.identity_char_retention);
    }

    #[test]
    fn hello_fixture_against_itself_does_not_drop_text() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/hello.pdf");
        let score = score_pair(
            &path,
            &path,
            &ScoreOptions {
                render_pages: 1,
                max_pages: Some(1),
            },
        )
        .unwrap();
        assert_eq!(score.drop_rate, 0.0);
        assert!(score.identity_char_retention > 0.99);
        assert_eq!(score.overflow_rate, 0.0);
        assert_eq!(score.reference_operators, 0);
        assert_eq!(score.reference_byte_identity, 1.0);
    }

    #[test]
    fn reference_section_operators_are_byte_identical_until_edited() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/bench-fixtures");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("refs-source.pdf");
        let body_edit = dir.join("refs-body-edit.pdf");
        let ref_edit = dir.join("refs-section-edit.pdf");
        let lines = [
            ("HelloBodyText stays in the article.", 720),
            ("References", 680),
            ("[1] Smith, A. A paper about attention 2020.", 660),
            ("Proceedings of the conference 2020.", 640),
            ("Appendix", 600),
            ("Proofs after the bibliography.", 580),
        ];
        write_pdf(&source, &lines);
        std::fs::copy(&source, &body_edit).unwrap();
        std::fs::copy(&source, &ref_edit).unwrap();
        patch_same_len(&body_edit, b"HelloBody", b"HellaBodz");
        patch_same_len(&ref_edit, b"References", b"Referenczz");
        let opts = ScoreOptions {
            render_pages: 0,
            max_pages: Some(1),
        };
        let same = score_pair(&source, &source, &opts).unwrap();
        assert!(same.reference_operators >= 2, "{same:?}");
        assert_eq!(same.reference_operators_identical, same.reference_operators);
        assert_eq!(same.reference_byte_identity, 1.0);
        let body = score_pair(&source, &body_edit, &opts).unwrap();
        assert_eq!(body.reference_byte_identity, 1.0, "{body:?}");
        let edited = score_pair(&source, &ref_edit, &opts).unwrap();
        assert!(edited.reference_byte_identity < 1.0, "{edited:?}");
        assert!(edited.reference_operators_identical < edited.reference_operators);
    }

    fn patch_same_len(path: &std::path::Path, from: &[u8], to: &[u8]) {
        assert_eq!(from.len(), to.len());
        let mut bytes = std::fs::read(path).unwrap();
        let pos = bytes
            .windows(from.len())
            .position(|window| window == from)
            .unwrap_or_else(|| panic!("missing {}", String::from_utf8_lossy(from)));
        bytes[pos..pos + from.len()].copy_from_slice(to);
        std::fs::write(path, bytes).unwrap();
    }
}
