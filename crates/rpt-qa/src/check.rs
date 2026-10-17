//! Compare two extractions and cross-check text against Poppler when it is installed.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use rpt_core::{Extraction, Glyph};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct AlignReport {
    pub text_match: bool,
    pub glyph_count_left: usize,
    pub glyph_count_right: usize,
    pub position_max_delta: f32,
    pub style_mismatches: usize,
    pub first_text_mismatch: Option<usize>,
}

pub fn align(left: &Extraction, right: &Extraction) -> AlignReport {
    let mut position_max_delta = 0.0f32;
    let mut style_mismatches = 0usize;
    let mut first_text_mismatch = None;
    let shared = left.glyphs.len().min(right.glyphs.len());
    for index in 0..shared {
        let a = &left.glyphs[index];
        let b = &right.glyphs[index];
        if first_text_mismatch.is_none() && a.unicode != b.unicode {
            first_text_mismatch = Some(index);
        }
        let dx = (a.matrix[4] - b.matrix[4]).abs();
        let dy = (a.matrix[5] - b.matrix[5]).abs();
        position_max_delta = position_max_delta.max(dx).max(dy);
        if style_differs(a, b) {
            style_mismatches += 1;
        }
    }
    if left.glyphs.len() != right.glyphs.len() && first_text_mismatch.is_none() {
        first_text_mismatch = Some(shared);
    }
    let text_match =
        left.plain_text() == right.plain_text() && left.glyphs.len() == right.glyphs.len();
    AlignReport {
        text_match,
        glyph_count_left: left.glyphs.len(),
        glyph_count_right: right.glyphs.len(),
        position_max_delta,
        style_mismatches,
        first_text_mismatch,
    }
}

fn style_differs(a: &Glyph, b: &Glyph) -> bool {
    if (a.font_size - b.font_size).abs() > 0.05
        || a.font_name != b.font_name
        || a.render_mode != b.render_mode
    {
        return true;
    }
    if a.fill_color.space != b.fill_color.space
        || a.fill_color.components.len() != b.fill_color.components.len()
    {
        return true;
    }
    a.fill_color
        .components
        .iter()
        .zip(&b.fill_color.components)
        .any(|(l, r)| (l - r).abs() > 0.01)
}

#[derive(Clone, Debug, Serialize)]
pub struct TextCrossCheck {
    pub tool: String,
    pub available: bool,
    pub ours_in_reference: Option<f32>,
    pub reference_in_ours: Option<f32>,
    pub our_chars: usize,
    pub reference_chars: usize,
    pub note: String,
}

pub fn poppler_cross_check(
    pdf: &Path,
    extraction: &Extraction,
    last_page: Option<u32>,
) -> TextCrossCheck {
    let mut command = Command::new("pdftotext");
    command.args(["-enc", "UTF-8", "-q"]);
    if let Some(last) = last_page {
        command.args(["-f", "1", "-l", &last.to_string()]);
    }
    command.arg(pdf).arg("-");
    match command.output() {
        Err(err) => TextCrossCheck {
            tool: "pdftotext".into(),
            available: false,
            ours_in_reference: None,
            reference_in_ours: None,
            our_chars: 0,
            reference_chars: 0,
            note: format!("pdftotext unavailable ({err})"),
        },
        Ok(output) if !output.status.success() => TextCrossCheck {
            tool: "pdftotext".into(),
            available: true,
            ours_in_reference: None,
            reference_in_ours: None,
            our_chars: 0,
            reference_chars: 0,
            note: format!(
                "pdftotext failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        },
        Ok(output) => {
            let reference = String::from_utf8_lossy(&output.stdout);
            let ours = letter_counts(&extraction.plain_text());
            let refs = letter_counts(&reference);
            let (ours_in_reference, our_chars) = coverage(&ours, &refs);
            let (reference_in_ours, reference_chars) = coverage(&refs, &ours);
            TextCrossCheck {
                tool: "pdftotext".into(),
                available: true,
                ours_in_reference: Some(ours_in_reference),
                reference_in_ours: Some(reference_in_ours),
                our_chars,
                reference_chars,
                note: "whitespace-insensitive character multiset; PDFium is not installed".into(),
            }
        }
    }
}

fn letter_counts(text: &str) -> HashMap<char, u32> {
    let mut counts = HashMap::new();
    for ch in text.chars().filter(|ch| !ch.is_whitespace()) {
        *counts.entry(ch).or_insert(0) += 1;
    }
    counts
}

fn coverage(left: &HashMap<char, u32>, right: &HashMap<char, u32>) -> (f32, usize) {
    let mut total = 0usize;
    let mut hit = 0usize;
    for (ch, count) in left {
        let count = *count as usize;
        total += count;
        let available = right.get(ch).copied().unwrap_or(0) as usize;
        hit += count.min(available);
    }
    if total == 0 {
        (1.0, 0)
    } else {
        (hit as f32 / total as f32, total)
    }
}

pub fn observed_tags(extraction: &Extraction) -> Vec<String> {
    let mut tags = Vec::new();
    if extraction.glyphs.is_empty() {
        tags.push("no-glyphs".into());
        return tags;
    }
    if is_two_column(extraction) {
        tags.push("two-column".into());
    } else {
        tags.push("single-column".into());
    }
    if extraction.glyphs.iter().any(|glyph| {
        glyph.unicode.chars().any(|ch| {
            ('\u{3400}'..='\u{9FFF}').contains(&ch) || ('\u{F900}'..='\u{FAFF}').contains(&ch)
        })
    }) {
        tags.push("cjk".into());
    }
    if extraction
        .glyphs
        .iter()
        .any(|glyph| matches!(glyph.source.kind, rpt_core::SourceKind::Type3CharProc))
    {
        tags.push("type3".into());
    }
    let math_glyphs = extraction
        .glyphs
        .iter()
        .filter(|glyph| is_math_font(&glyph.font_name) || has_math(&glyph.unicode))
        .count();
    if math_glyphs >= 8 {
        tags.push("formulas".into());
    }
    let mono_glyphs = extraction
        .glyphs
        .iter()
        .filter(|glyph| is_mono(&glyph.font_name))
        .count();
    if mono_glyphs >= 12 {
        tags.push("code".into());
    }
    if has_footnotes(extraction) {
        tags.push("footnotes".into());
    }
    if extraction
        .glyphs
        .iter()
        .any(|glyph| !is_black(&glyph.fill_color))
    {
        tags.push("color".into());
    }
    if extraction
        .glyphs
        .iter()
        .any(|glyph| is_bold(&glyph.font_name))
    {
        tags.push("bold".into());
    }
    if extraction
        .glyphs
        .iter()
        .any(|glyph| is_italic(&glyph.font_name))
    {
        tags.push("italic".into());
    }
    let pages = extraction.pages.len().max(1);
    if extraction.glyphs.len() / pages < 15 {
        tags.push("sparse-text".into());
    }
    let plain = extraction.plain_text();
    if plain.contains("Table") || plain.contains("TABLE") {
        tags.push("tables".into());
    }
    if plain.contains("Figure") || plain.contains("Fig.") {
        tags.push("figures".into());
    }
    tags
}

fn is_two_column(extraction: &Extraction) -> bool {
    // A column gutter is a gap that stays at nearly the same x on many lines.
    // A page-wide x histogram hides it: titles and page numbers sit in the
    // middle, and the gutter itself is often only ~15pt. A table of contents
    // also repeats a gap, but the right-hand side is a page number, not a column.
    extraction
        .pages
        .iter()
        .any(|page| page_has_stable_gutter(extraction, page))
}

fn page_has_stable_gutter(extraction: &Extraction, page: &rpt_core::PageInfo) -> bool {
    let width = (page.media_box[2] - page.media_box[0]).abs();
    if width < 100.0 {
        return false;
    }
    let left = page.media_box[0];
    let mut lines: HashMap<i32, Vec<f32>> = HashMap::new();
    for glyph in extraction.glyphs.iter().filter(|glyph| {
        glyph.page_index == page.index && glyph.matrix[4].is_finite() && glyph.matrix[5].is_finite()
    }) {
        lines
            .entry(glyph.matrix[5].round() as i32)
            .or_default()
            .push(glyph.matrix[4]);
    }
    let mut splits = Vec::new();
    for xs in lines.values_mut() {
        if xs.len() < 16 {
            continue;
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut best_gap = 0.0f32;
        let mut split = left + width / 2.0;
        for pair in xs.windows(2) {
            let gap = pair[1] - pair[0];
            if gap > best_gap {
                best_gap = gap;
                split = (pair[0] + pair[1]) / 2.0;
            }
        }
        let from_left = (split - left) / width;
        if best_gap < 14.0 || !(0.30..=0.70).contains(&from_left) {
            continue;
        }
        let left_count = xs.iter().filter(|x| **x < split).count();
        let right_count = xs.len() - left_count;
        if left_count >= 8 && right_count >= 8 {
            splits.push(split);
        }
    }
    if splits.len() < 8 {
        return false;
    }
    splits.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = splits[splits.len() / 2];
    let mut devs: Vec<f32> = splits.iter().map(|split| (split - median).abs()).collect();
    devs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    devs[devs.len() / 2] < 8.0
}

fn has_footnotes(extraction: &Extraction) -> bool {
    let sizes: Vec<f32> = extraction
        .glyphs
        .iter()
        .map(|glyph| glyph.font_size)
        .filter(|size| size.is_finite() && *size > 0.0)
        .collect();
    if sizes.len() < 20 {
        return false;
    }
    let mut sorted = sizes.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];
    extraction.pages.iter().any(|page| {
        let height = (page.media_box[3] - page.media_box[1]).abs();
        let floor = page.media_box[1] + height * 0.16;
        extraction
            .glyphs
            .iter()
            .filter(|glyph| {
                glyph.page_index == page.index
                    && glyph.font_size <= median * 0.78
                    && glyph.matrix[5] < floor
            })
            .count()
            >= 8
    })
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

fn has_math(text: &str) -> bool {
    text.chars().any(|ch| {
        ('\u{2200}'..='\u{22FF}').contains(&ch)
            || ('\u{0370}'..='\u{03FF}').contains(&ch)
            || ('\u{1D400}'..='\u{1D7FF}').contains(&ch)
    })
}

fn is_mono(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "COUR",
        "CMTT",
        "CONSOL",
        "MONO",
        "INCONSOL",
        "COURIER",
        "LUCIDACONSOLE",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
}

fn is_bold(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.contains("BOLD")
        || upper.contains("BLACK")
        || upper.contains("SEMIBOLD")
        || upper.contains("CMB")
}

fn is_italic(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.contains("ITALIC") || upper.contains("OBLIQUE") || upper.contains("ITAL")
}

#[derive(Clone, Debug, Serialize)]
pub struct TocFragment {
    pub page: u32,
    pub text: String,
    pub reason: String,
}

/// Contents rows that lost a title, glued two entries together, or left a
/// one-character scrap. `page` is the 1-based PDF page.
pub fn toc_row_fragments(extraction: &Extraction) -> Vec<TocFragment> {
    let mut lines: Vec<TocLine> = Vec::new();
    let mut glyphs: Vec<&Glyph> = extraction
        .glyphs
        .iter()
        .filter(|glyph| !glyph.invisible && !glyph.unicode.trim().is_empty())
        .collect();
    glyphs.sort_by(|left, right| {
        left.page_index.cmp(&right.page_index).then(
            right.matrix[5]
                .total_cmp(&left.matrix[5])
                .then(left.matrix[4].total_cmp(&right.matrix[4])),
        )
    });
    for glyph in glyphs {
        let attach = lines.last().is_some_and(|line| {
            line.page == glyph.page_index && (line.y - glyph.matrix[5]).abs() <= 2.0
        });
        if attach {
            let line = lines.last_mut().expect("line just matched");
            line.text.push_str(&glyph.unicode);
            line.x = line.x.min(glyph.matrix[4]);
            line.size = line.size.max(glyph.font_size);
        } else {
            lines.push(TocLine {
                page: glyph.page_index,
                y: glyph.matrix[5],
                x: glyph.matrix[4],
                size: glyph.font_size,
                text: glyph.unicode.clone(),
            });
        }
    }
    let mut pages: HashMap<u32, Vec<usize>> = HashMap::new();
    for (index, line) in lines.iter().enumerate() {
        pages.entry(line.page).or_default().push(index);
    }
    let mut flags = Vec::new();
    for (page, indices) in pages {
        if !page_is_contents(&lines, &indices) {
            continue;
        }
        for index in indices {
            let line = &lines[index];
            if let Some(reason) = fragment_reason(line) {
                flags.push(TocFragment {
                    page: page + 1,
                    text: line.text.clone(),
                    reason,
                });
            }
        }
    }
    flags.sort_by(|left, right| {
        left.page
            .cmp(&right.page)
            .then(left.text.cmp(&right.text))
            .then(left.reason.cmp(&right.reason))
    });
    flags
}

struct TocLine {
    page: u32,
    y: f32,
    x: f32,
    size: f32,
    text: String,
}

fn page_is_contents(lines: &[TocLine], indices: &[usize]) -> bool {
    let numbered = indices
        .iter()
        .filter(|index| line_ends_with_page_number(&lines[**index].text))
        .count();
    if numbered < 4 {
        return false;
    }
    let mut lengths: Vec<usize> = indices
        .iter()
        .map(|index| lines[*index].text.chars().count())
        .collect();
    lengths.sort_unstable();
    lengths[lengths.len() / 2] < 90
}

fn line_ends_with_page_number(text: &str) -> bool {
    let text = text.trim();
    let Some(token) = text.split_whitespace().last() else {
        return false;
    };
    page_token(token) || text.chars().last().is_some_and(|ch| ch.is_ascii_digit())
}

fn page_token(text: &str) -> bool {
    if text.is_empty() || text.chars().count() > 6 {
        return false;
    }
    if text.bytes().all(|byte| byte.is_ascii_digit()) {
        return (1..=4).contains(&text.len());
    }
    text.chars().all(|ch| {
        matches!(
            ch.to_ascii_lowercase(),
            'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'
        )
    })
}

fn fragment_reason(line: &TocLine) -> Option<String> {
    let text = line.text.trim();
    if text.is_empty() {
        return None;
    }
    let compact: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    if compact.chars().count() >= 2
        && compact
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic())
        && compact.chars().skip(1).all(|ch| ch.is_ascii_digit())
        && compact.chars().filter(|ch| ch.is_ascii_digit()).count() >= 2
    {
        return Some("latin-stub".into());
    }
    let body = strip_toc_chrome(text);
    if body.starts_with('录') && !body.contains('附') {
        return Some("split-appendix".into());
    }
    if let Some(at) = body.find("附录") {
        let before = body[..at].trim();
        let before = before.trim_start_matches(|ch: char| {
            ch.is_ascii_digit() || ch == '.' || ch == '■' || ch == ' '
        });
        if before
            .chars()
            .any(|ch| !ch.is_ascii_digit() && ch != '■' && !ch.is_whitespace())
        {
            return Some("run-on".into());
        }
    }
    let leftover = strip_trailing_page(&body);
    if leftover.chars().count() == 1 && line.size < 16.0 && line.y > 80.0 {
        return Some("orphan-char".into());
    }
    if leftover.is_empty()
        && page_token(compact.as_str())
        && line.size < 16.0
        && line.x > 280.0
        && line.y > 90.0
    {
        return Some("detached-number".into());
    }
    None
}

fn strip_toc_chrome(text: &str) -> String {
    let mut out = text.trim().to_string();
    while out.starts_with(['■', '•', '▪', ' ']) {
        out = out[out.chars().next().map(char::len_utf8).unwrap_or(1)..]
            .trim_start()
            .to_string();
    }
    out
}

fn strip_trailing_page(text: &str) -> String {
    let trimmed = text.trim();
    let Some(token) = trimmed.split_whitespace().last() else {
        return String::new();
    };
    if page_token(token) && trimmed.split_whitespace().count() >= 2 {
        let end = trimmed.len() - token.len();
        return trimmed[..end].trim().to_string();
    }
    if trimmed.chars().last().is_some_and(|ch| ch.is_ascii_digit()) {
        let stem = trimmed
            .trim_end_matches(|ch: char| ch.is_ascii_digit())
            .trim();
        if stem.chars().count() < trimmed.chars().count() {
            return stem.to_string();
        }
    }
    trimmed.to_string()
}

#[derive(Clone, Debug, Serialize)]
pub struct BoundaryCross {
    pub page: u32,
    pub text: String,
    pub edge: f32,
}

/// Output text that crosses a filled path or image edge the matching source
/// line did not. A full-bleed image is the page itself, not a panel edge.
pub fn paint_boundary_crossings(source: &Extraction, output: &Extraction) -> Vec<BoundaryCross> {
    let edges = vertical_edges(source);
    if edges.is_empty() {
        return Vec::new();
    }
    let source_lines = paint_lines(&source.glyphs);
    let mut flags = Vec::new();
    for line in paint_lines(&output.glyphs) {
        let Some(matched) = source_lines
            .iter()
            .filter(|row| {
                row.page == line.page && (row.y - line.y).abs() <= line.size.max(row.size) * 0.8
            })
            .min_by(|left, right| (left.y - line.y).abs().total_cmp(&(right.y - line.y).abs()))
        else {
            continue;
        };
        for edge in edges.iter().filter(|edge| edge.page == line.page) {
            if !line_meets_edge(&line, edge) || !crosses(&line, edge.x) || crosses(matched, edge.x)
            {
                continue;
            }
            flags.push(BoundaryCross {
                page: line.page + 1,
                text: line.text.clone(),
                edge: edge.x,
            });
        }
    }
    flags.sort_by(|left, right| {
        left.page
            .cmp(&right.page)
            .then(left.edge.total_cmp(&right.edge))
            .then(left.text.cmp(&right.text))
    });
    flags
}

struct PaintLine {
    page: u32,
    y: f32,
    min_x: f32,
    max_x: f32,
    size: f32,
    text: String,
}

struct PanelEdge {
    page: u32,
    x: f32,
    y0: f32,
    y1: f32,
}

fn paint_lines(glyphs: &[Glyph]) -> Vec<PaintLine> {
    let mut ordered: Vec<&Glyph> = glyphs
        .iter()
        .filter(|glyph| !glyph.invisible && !glyph.unicode.trim().is_empty())
        .collect();
    ordered.sort_by(|left, right| {
        left.page_index.cmp(&right.page_index).then(
            right.matrix[5]
                .total_cmp(&left.matrix[5])
                .then(left.matrix[4].total_cmp(&right.matrix[4])),
        )
    });
    let mut lines: Vec<PaintLine> = Vec::new();
    for glyph in ordered {
        let left = glyph.matrix[4].min(glyph.bbox[0]);
        let right = glyph.bbox[2].max(glyph.matrix[4] + glyph.advance[0].max(0.0));
        let attach = lines.last().is_some_and(|line| {
            line.page == glyph.page_index && (line.y - glyph.matrix[5]).abs() <= 2.0
        });
        if attach {
            let line = lines.last_mut().expect("line just matched");
            line.text.push_str(&glyph.unicode);
            line.min_x = line.min_x.min(left);
            line.max_x = line.max_x.max(right);
            line.size = line.size.max(glyph.font_size);
        } else {
            lines.push(PaintLine {
                page: glyph.page_index,
                y: glyph.matrix[5],
                min_x: left,
                max_x: right,
                size: glyph.font_size,
                text: glyph.unicode.clone(),
            });
        }
    }
    lines
}

fn vertical_edges(extraction: &Extraction) -> Vec<PanelEdge> {
    let mut edges = Vec::new();
    for region in &extraction.regions {
        let [x0, y0, x1, y1] = region.bbox;
        let width = (x1 - x0).abs();
        let height = (y1 - y0).abs();
        if width < 8.0 || height < 20.0 {
            continue;
        }
        let media = extraction
            .pages
            .iter()
            .find(|page| page.index == region.page_index)
            .map(|page| page.media_box)
            .unwrap_or([0.0, 0.0, 612.0, 792.0]);
        let page_w = (media[2] - media[0]).abs().max(1.0);
        let page_h = (media[3] - media[1]).abs().max(1.0);
        if width > page_w * 0.92 && height > page_h * 0.92 {
            continue;
        }
        let (lo, hi) = (y0.min(y1), y0.max(y1));
        let (left, right) = (x0.min(x1), x0.max(x1));
        edges.push(PanelEdge {
            page: region.page_index,
            x: left,
            y0: lo,
            y1: hi,
        });
        edges.push(PanelEdge {
            page: region.page_index,
            x: right,
            y0: lo,
            y1: hi,
        });
    }
    edges
}

fn crosses(line: &PaintLine, edge: f32) -> bool {
    line.min_x < edge - 1.0 && line.max_x > edge + 1.0
}

fn line_meets_edge(line: &PaintLine, edge: &PanelEdge) -> bool {
    let pad = line.size.max(4.0);
    line.y + pad >= edge.y0 && line.y - pad <= edge.y1
}

#[derive(Clone, Debug, Serialize)]
pub struct ContrastFlag {
    pub page: u32,
    pub text: String,
    pub ratio: f32,
}

/// WCAG contrast of a fill against a background RGB sample in 0..1.
/// Text at 18pt or larger uses the 3:1 floor. Smaller text uses 4.5:1.
pub fn low_contrast_lines(
    glyphs: &[Glyph],
    mut sample: impl FnMut(u32, f32, f32) -> Option<[f32; 3]>,
) -> Vec<ContrastFlag> {
    let mut flags = Vec::new();
    for line in paint_lines(glyphs) {
        let Some(fill) = line_fill(glyphs, &line) else {
            continue;
        };
        let probe_y = line.y + line.size * 1.15;
        let Some(background) = sample(line.page, line.min_x, probe_y) else {
            continue;
        };
        let ratio = contrast_ratio(&fill, background);
        let floor = if line.size >= 18.0 { 3.0 } else { 4.5 };
        if ratio + 0.05 < floor {
            flags.push(ContrastFlag {
                page: line.page + 1,
                text: line.text.clone(),
                ratio,
            });
        }
    }
    flags
}

fn line_fill(glyphs: &[Glyph], line: &PaintLine) -> Option<rpt_core::Color> {
    glyphs.iter().find_map(|glyph| {
        if glyph.page_index != line.page || (glyph.matrix[5] - line.y).abs() > 2.0 {
            return None;
        }
        let left = glyph.matrix[4].min(glyph.bbox[0]);
        if (left - line.min_x).abs() > 1.5 {
            return None;
        }
        Some(glyph.fill_color.clone())
    })
}

pub fn contrast_ratio(foreground: &rpt_core::Color, background: [f32; 3]) -> f32 {
    let Some(rgb) = color_rgb(foreground) else {
        return 21.0;
    };
    let lighter = relative_luminance(rgb).max(relative_luminance(background));
    let darker = relative_luminance(rgb).min(relative_luminance(background));
    (lighter + 0.05) / (darker + 0.05)
}

fn color_rgb(color: &rpt_core::Color) -> Option<[f32; 3]> {
    match color.space.as_str() {
        "DeviceGray" => {
            let g = *color.components.first()?;
            Some([g, g, g])
        }
        "DeviceRGB" if color.components.len() >= 3 => Some([
            color.components[0],
            color.components[1],
            color.components[2],
        ]),
        "DeviceCMYK" if color.components.len() >= 4 => {
            let (c, m, y, k) = (
                color.components[0],
                color.components[1],
                color.components[2],
                color.components[3],
            );
            Some([
                (1.0 - c) * (1.0 - k),
                (1.0 - m) * (1.0 - k),
                (1.0 - y) * (1.0 - k),
            ])
        }
        _ => None,
    }
}

fn relative_luminance(rgb: [f32; 3]) -> f32 {
    let channel = |value: f32| {
        let value = value.clamp(0.0, 1.0);
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(rgb[0]) + 0.7152 * channel(rgb[1]) + 0.0722 * channel(rgb[2])
}

#[derive(Clone, Debug, Serialize)]
pub struct ShortBodyLine {
    pub page: u32,
    pub text: String,
    pub width_ratio: f32,
}

/// A non-final line of a body paragraph whose ink is under 85% of the
/// paragraph width. The last line of a paragraph may be short. Backing up
/// to `、` or `，` leaves a line in this range while the next characters
/// still fitted.
pub fn short_paragraph_lines(glyphs: &[Glyph]) -> Vec<ShortBodyLine> {
    let lines = paint_lines(glyphs);
    let mut flags = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let mut end = index + 1;
        while end < lines.len() && paragraph_continues(&lines[end - 1], &lines[end]) {
            end += 1;
        }
        if end - index >= 2 {
            let measure = lines[index..end]
                .iter()
                .map(|line| line.max_x - line.min_x)
                .fold(0.0f32, f32::max)
                .max(1.0);
            for line in &lines[index..end - 1] {
                let width = line.max_x - line.min_x;
                let ratio = width / measure;
                let letters = line.chars_of_text();
                if ratio < 0.85 && letters >= 2 && width > 1.0 {
                    flags.push(ShortBodyLine {
                        page: line.page + 1,
                        text: line.text.clone(),
                        width_ratio: ratio,
                    });
                }
            }
        }
        index = end;
    }
    flags
}

fn paragraph_continues(upper: &PaintLine, lower: &PaintLine) -> bool {
    if upper.page != lower.page {
        return false;
    }
    let size = upper.size.max(lower.size).max(1.0);
    if (upper.size - lower.size).abs() > size * 0.25 {
        return false;
    }
    let dy = upper.y - lower.y;
    if dy < size * 0.8 || dy > size * 2.2 {
        return false;
    }
    // The last line of a paragraph may be short. An indent change after a
    // finished sentence is the next paragraph, not a continuation.
    if ends_sentence(&upper.text) && (upper.min_x - lower.min_x).abs() > size * 0.45 {
        return false;
    }
    // `10` above `10.1 …` is a contents heading, not a one-word body line.
    if number_only(&upper.text) || number_only(&lower.text) {
        return false;
    }
    let shift = (upper.min_x - lower.min_x).abs();
    shift <= size * 2.4
}

#[derive(Clone, Debug, Serialize)]
pub struct BaselineDrift {
    pub page: u32,
    pub text: String,
    pub rise: f32,
}

/// A digit after `=` belongs on that baseline. Inline math (`N = 6`) and a
/// body-size formula digit must not be painted above the line.
pub fn inline_math_baseline_drift(glyphs: &[Glyph]) -> Vec<BaselineDrift> {
    let visible: Vec<&Glyph> = glyphs
        .iter()
        .filter(|glyph| !glyph.invisible && !glyph.unicode.trim().is_empty())
        .collect();
    let mut flags = Vec::new();
    for glyph in &visible {
        let text = glyph.unicode.trim();
        if text.chars().count() != 1 || !text.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
            continue;
        }
        let Some(equals) = visible
            .iter()
            .filter(|equals| {
                if equals.page_index != glyph.page_index || equals.unicode.trim() != "=" {
                    return false;
                }
                let size = equals.font_size.max(1.0);
                let dx = glyph.matrix[4] - equals.matrix[4];
                if dx <= 0.2 || dx > size * 3.0 {
                    return false;
                }
                !visible.iter().any(|between| {
                    if between.id == equals.id || between.id == glyph.id {
                        return false;
                    }
                    if between.page_index != glyph.page_index {
                        return false;
                    }
                    let text = between.unicode.trim();
                    if text.is_empty() || text.chars().all(|ch| ch.is_whitespace()) {
                        return false;
                    }
                    let x = between.matrix[4];
                    x > equals.matrix[4] + 0.2 && x < glyph.matrix[4] - 0.2
                })
            })
            .min_by(|left, right| {
                (left.matrix[5] - glyph.matrix[5])
                    .abs()
                    .total_cmp(&(right.matrix[5] - glyph.matrix[5]).abs())
            })
        else {
            continue;
        };
        let rise = glyph.matrix[5] - equals.matrix[5];
        if rise > equals.font_size.max(1.0) * 0.15 {
            flags.push(BaselineDrift {
                page: glyph.page_index + 1,
                text: format!("={text}"),
                rise,
            });
        }
    }
    for glyph in &visible {
        let text = glyph.unicode.trim();
        if text.chars().count() != 1 || !text.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
            continue;
        }
        if !math_face(&glyph.font_name) {
            continue;
        }
        let size = glyph.font_size.max(1.0);
        let Some(host) = visible
            .iter()
            .filter(|host| {
                host.page_index == glyph.page_index
                    && host.id != glyph.id
                    && !math_face(&host.font_name)
                    && host.font_size > size * 0.85
                    && (host.matrix[4] - glyph.matrix[4]).abs() < size * 6.0
            })
            .min_by(|left, right| {
                (left.matrix[5] - glyph.matrix[5])
                    .abs()
                    .total_cmp(&(right.matrix[5] - glyph.matrix[5]).abs())
            })
        else {
            continue;
        };
        if glyph.font_size <= host.font_size * 0.85 {
            continue;
        }
        let rise = glyph.matrix[5] - host.matrix[5];
        if rise > host.font_size * 0.20 {
            flags.push(BaselineDrift {
                page: glyph.page_index + 1,
                text: text.to_string(),
                rise,
            });
        }
    }
    flags
}

#[derive(Clone, Debug, Serialize)]
pub struct TitleAxisDrift {
    pub page: u32,
    pub text: String,
    pub delta: f32,
}

/// A display title between two horizontal rules, or a short page-centered
/// title, whose ink midpoint has left the rule or page axis. Redrawing a
/// wider face from the source left edge does that.
pub fn title_axis_drift(extraction: &Extraction) -> Vec<TitleAxisDrift> {
    let lines = paint_lines(&extraction.glyphs);
    let mut flags = Vec::new();
    for line in &lines {
        let letters = line.chars_of_text();
        if line.size < 14.0 || !(4..=80).contains(&letters) {
            continue;
        }
        let media = extraction
            .pages
            .iter()
            .find(|info| info.index == line.page)
            .map(|info| info.media_box)
            .unwrap_or([0.0, 0.0, 612.0, 792.0]);
        let page_left = media[0];
        let page_right = media[2];
        let page_width = (page_right - page_left).max(1.0);
        let page_mid = (page_left + page_right) * 0.5;
        let mid = (line.min_x + line.max_x) * 0.5;
        let axis =
            bracketing_rules(extraction, line.page, line.y, line.size).unwrap_or_else(|| {
                let margin = (page_width * 0.12).max(line.size * 3.0);
                let centered = line.min_x - page_left >= margin
                    && page_right - line.max_x >= margin
                    && (mid - page_mid).abs() <= (line.size * 1.6).max(28.0)
                    && (line.max_x - line.min_x) < page_width * 0.8;
                if centered {
                    page_mid
                } else {
                    f32::NAN
                }
            });
        if !axis.is_finite() {
            continue;
        }
        let delta = (mid - axis).abs();
        if delta > (line.size * 0.22).max(4.0) {
            flags.push(TitleAxisDrift {
                page: line.page + 1,
                text: line.text.clone(),
                delta,
            });
        }
    }
    flags
}

#[derive(Clone, Debug, Serialize)]
pub struct OrphanRule {
    pub page: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
}

/// A short horizontal path with no glyph beside it. A radical vinculum or
/// fraction bar left at the English position after the formula moved is
/// this. A bar that still crosses its radicand is not.
pub fn orphan_rules(extraction: &Extraction) -> Vec<OrphanRule> {
    let mut flags = Vec::new();
    for region in &extraction.regions {
        if region.kind != "path" {
            continue;
        }
        let x0 = region.bbox[0].min(region.bbox[2]);
        let x1 = region.bbox[0].max(region.bbox[2]);
        let y0 = region.bbox[1].min(region.bbox[3]);
        let y1 = region.bbox[1].max(region.bbox[3]);
        let width = x1 - x0;
        let height = y1 - y0;
        if height > 1.6 || !(2.0..=40.0).contains(&width) {
            continue;
        }
        let y = (y0 + y1) * 0.5;
        let beside = extraction.glyphs.iter().any(|glyph| {
            if glyph.page_index != region.page_index
                || glyph.invisible
                || glyph.unicode.trim().is_empty()
            {
                return false;
            }
            let gx0 = glyph.bbox[0].min(glyph.bbox[2]) - 16.0;
            let gx1 = glyph.bbox[0].max(glyph.bbox[2]) + 16.0;
            let gy0 = glyph.bbox[1].min(glyph.bbox[3]) - 6.0;
            let gy1 = glyph.bbox[1].max(glyph.bbox[3]) + 6.0;
            y >= gy0 && y <= gy1 && x1 >= gx0 && x0 <= gx1
        });
        if beside {
            continue;
        }
        flags.push(OrphanRule {
            page: region.page_index + 1,
            x: x0,
            y,
            width,
        });
    }
    flags
}

/// Shared midpoint of the horizontal rules above and below `y`.
fn bracketing_rules(extraction: &Extraction, page: u32, y: f32, size: f32) -> Option<f32> {
    let mut rules = Vec::new();
    for region in &extraction.regions {
        if region.page_index != page || region.kind != "path" {
            continue;
        }
        let width = (region.bbox[2] - region.bbox[0]).abs();
        let height = (region.bbox[3] - region.bbox[1]).abs();
        if width < 120.0 || height > 6.0 {
            continue;
        }
        let left = region.bbox[0].min(region.bbox[2]);
        let right = region.bbox[0].max(region.bbox[2]);
        let rule_y = (region.bbox[1] + region.bbox[3]) * 0.5;
        rules.push((left, right, rule_y));
    }
    let above = rules
        .iter()
        .filter(|rule| rule.2 > y + size * 0.15 && rule.2 < y + size * 4.5)
        .min_by(|left, right| left.2.total_cmp(&right.2))?;
    let below = rules
        .iter()
        .filter(|rule| rule.2 < y - size * 0.05 && rule.2 > y - size * 4.5)
        .max_by(|left, right| left.2.total_cmp(&right.2))?;
    let overlap = above.1.min(below.1) - above.0.max(below.0);
    let shorter = (above.1 - above.0).min(below.1 - below.0);
    if overlap < shorter * 0.6 {
        return None;
    }
    Some((above.0 + above.1 + below.0 + below.1) * 0.25)
}

fn math_face(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("cmm") || name.contains("cmr") || name.contains("cmsy") || name.contains("math")
}

fn ends_sentence(text: &str) -> bool {
    text.trim_end().ends_with(['.', '!', '?', '。', '！', '？'])
}

fn number_only(text: &str) -> bool {
    let text = text.trim().trim_end_matches('.');
    !text.is_empty()
        && text.len() <= 4
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
}

impl PaintLine {
    fn chars_of_text(&self) -> usize {
        self.text.chars().filter(|ch| !ch.is_whitespace()).count()
    }
}

fn is_black(color: &rpt_core::Color) -> bool {
    color.components.iter().all(|component| *component <= 0.02)
        || (color.space == "DeviceGray" && color.components.first().is_some_and(|g| *g <= 0.02))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpt_core::{
        Color, Disposition, Extraction, Glyph, GlyphSource, PageInfo, SourceKind, COORDINATE_SPACE,
    };

    fn glyph(id: u32, x: f32, y: f32) -> Glyph {
        Glyph {
            id,
            page_index: 0,
            unicode: "a".into(),
            unmapped: false,
            char_code: vec![b'a'],
            gid: None,
            font_resource: "F1".into(),
            font_name: "CMR10".into(),
            font_object: None,
            font_size: 10.0,
            matrix: [10.0, 0.0, 0.0, 10.0, x, y],
            bbox: [x, y, x + 5.0, y + 8.0],
            advance: [5.0, 0.0],
            fill_color: Color::black(),
            stroke_color: Color::black(),
            render_mode: 0,
            invisible: false,
            clipped: false,
            clip_uncertain: false,
            vertical: false,
            disposition: Disposition::Pending,
            source: GlyphSource {
                kind: SourceKind::PageContent,
                object_id: None,
                stream_index: 0,
                operator_index: id,
                byte_start: 0,
                byte_end: 1,
                resource_name: None,
            },
        }
    }

    fn page_with(glyphs: Vec<Glyph>) -> Extraction {
        Extraction {
            coordinate_space: COORDINATE_SPACE,
            pages: vec![PageInfo {
                index: 0,
                object_id: "1 0".into(),
                media_box: [0.0, 0.0, 612.0, 792.0],
                rotate: 0,
            }],
            glyphs,
            regions: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn column_gutter_is_detected_per_line() {
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        for line in 0..8 {
            let y = 500.0 - line as f32 * 14.0;
            for col in 0..10 {
                glyphs.push(glyph(id, 72.0 + col as f32 * 6.0, y));
                id += 1;
                glyphs.push(glyph(id, 340.0 + col as f32 * 6.0, y));
                id += 1;
            }
        }
        let tags = observed_tags(&page_with(glyphs));
        assert!(tags.iter().any(|tag| tag == "two-column"), "{tags:?}");
    }

    #[test]
    fn a_single_text_column_is_not_two_column() {
        let mut glyphs = Vec::new();
        for line in 0..8 {
            let y = 500.0 - line as f32 * 14.0;
            for col in 0..24 {
                glyphs.push(glyph((line * 24 + col) as u32, 72.0 + col as f32 * 6.0, y));
            }
        }
        let tags = observed_tags(&page_with(glyphs));
        assert!(tags.iter().any(|tag| tag == "single-column"), "{tags:?}");
    }

    #[test]
    fn cached_samples_match_known_layouts() {
        let cases = [
            ("2024-acl-long-1", "two-column"),
            ("arxiv-1706.03762", "single-column"),
            (
                "cvpr-2024-liu-programmable-motion-generation-for-open-set-",
                "two-column",
            ),
            ("open-logic", "single-column"),
            ("nist-sp-800-63-3", "single-column"),
        ];
        let mut ran = 0;
        for (id, expect) in cases {
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../corpus/cache/{id}.pdf"));
            if !path.exists() {
                continue;
            }
            ran += 1;
            let bytes = std::fs::read(&path).unwrap();
            let doc = rpt_core::PdfDocument::open_bytes(&bytes).unwrap();
            let extraction = doc.extract_with(&rpt_core::ExtractOptions {
                max_pages: Some(4),
                ..rpt_core::ExtractOptions::default()
            });
            let tags = observed_tags(&extraction);
            assert!(
                tags.iter().any(|tag| tag == expect),
                "{id} expected {expect} in {tags:?}"
            );
        }
        if ran == 0 {
            eprintln!("corpus cache absent; layout samples skipped");
        }
    }

    fn text_glyph(id: u32, x: f32, y: f32, text: &str) -> Glyph {
        let mut item = glyph(id, x, y);
        item.unicode = text.into();
        item.advance = [text.chars().count() as f32 * 8.0, 0.0];
        item.bbox[2] = x + item.advance[0];
        item
    }

    #[test]
    fn toc_fragments_are_flagged_and_clean_rows_are_not() {
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        let push = |glyphs: &mut Vec<Glyph>, id: &mut u32, y: f32, text: &str| {
            glyphs.push(text_glyph(*id, 120.0, y, text));
            *id += 1;
        };
        push(&mut glyphs, &mut id, 500.0, "1.1 构建应用 4");
        push(&mut glyphs, &mut id, 480.0, "1.2 介绍框架 11");
        push(&mut glyphs, &mut id, 460.0, "附录 A 试用 LangChain 351");
        push(&mut glyphs, &mut id, 440.0, "提示工程 20");
        push(&mut glyphs, &mut id, 420.0, "n351");
        push(&mut glyphs, &mut id, 400.0, "录 C 选择 LLM 360");
        push(
            &mut glyphs,
            &mut id,
            380.0,
            "查询生成、路由和检索后处理 附录 A 试用 LangChain 228",
        );
        push(&mut glyphs, &mut id, 360.0, "四");
        glyphs.push(text_glyph(id, 400.0, 200.0, "375"));
        let flags = toc_row_fragments(&page_with(glyphs));
        let reasons: Vec<_> = flags.iter().map(|flag| flag.reason.as_str()).collect();
        assert!(reasons.contains(&"latin-stub"), "{flags:?}");
        assert!(reasons.contains(&"split-appendix"), "{flags:?}");
        assert!(reasons.contains(&"run-on"), "{flags:?}");
        assert!(reasons.contains(&"orphan-char"), "{flags:?}");
        assert!(reasons.contains(&"detached-number"), "{flags:?}");
        assert!(
            !flags.iter().any(|flag| flag.text.contains("1.1")),
            "{flags:?}"
        );
        assert!(
            !flags.iter().any(|flag| flag.text.starts_with("附录 A")),
            "{flags:?}"
        );
    }

    fn span_line(id: u32, x: f32, y: f32, width: f32, text: &str) -> Glyph {
        let mut item = text_glyph(id, x, y, text);
        item.font_size = 12.0;
        item.matrix[0] = 12.0;
        item.matrix[3] = 12.0;
        item.advance = [width, 0.0];
        item.bbox = [x, y - 2.0, x + width, y + 10.0];
        item
    }

    #[test]
    fn text_that_crosses_a_new_panel_edge_is_flagged() {
        let panel = rpt_core::glyph::PaintedRegion {
            page_index: 0,
            bbox: [0.0, 100.0, 200.0, 500.0],
            kind: "path".into(),
        };
        let bleed = rpt_core::glyph::PaintedRegion {
            page_index: 0,
            bbox: [0.0, 0.0, 612.0, 792.0],
            kind: "image".into(),
        };
        let mut source = page_with(vec![
            span_line(1, 40.0, 300.0, 140.0, "With LangChain"),
            span_line(2, 40.0, 260.0, 220.0, "already across"),
        ]);
        source.regions = vec![panel.clone(), bleed];
        let output = page_with(vec![
            span_line(1, 40.0, 300.0, 220.0, "使用 LangChain、LangGraph 和 MCP"),
            span_line(2, 40.0, 280.0, 100.0, "曼宁"),
            span_line(3, 40.0, 260.0, 220.0, "already across"),
        ]);
        let flags = paint_boundary_crossings(&source, &output);
        assert!(
            flags
                .iter()
                .any(|flag| { flag.text.contains("LangGraph") && (flag.edge - 200.0).abs() < 0.1 }),
            "{flags:?}"
        );
        assert!(
            !flags.iter().any(|flag| flag.text.contains("曼宁")),
            "{flags:?}"
        );
        assert!(
            !flags.iter().any(|flag| flag.text.contains("already")),
            "{flags:?}"
        );
        let only_bleed = page_with(vec![span_line(1, 40.0, 300.0, 400.0, "full width")]);
        let mut bleed_source = only_bleed.clone();
        bleed_source.regions = vec![rpt_core::glyph::PaintedRegion {
            page_index: 0,
            bbox: [0.0, 0.0, 612.0, 792.0],
            kind: "image".into(),
        }];
        assert!(paint_boundary_crossings(&bleed_source, &only_bleed).is_empty());
    }

    #[test]
    fn black_on_navy_fails_contrast_and_light_gray_does_not() {
        let navy = [29.0 / 255.0, 48.0 / 255.0, 70.0 / 255.0];
        let mut dark = span_line(1, 43.0, 330.0, 160.0, "使用 LangChain、");
        dark.fill_color = Color::black();
        dark.font_size = 9.5;
        let mut light = span_line(2, 43.0, 300.0, 160.0, "LangGraph 和 MCP");
        light.fill_color = Color::cmyk(0.0, 0.0, 0.0, 0.3);
        light.font_size = 18.0;
        let mut body = span_line(3, 72.0, 200.0, 300.0, "正文");
        body.fill_color = Color::black();
        let flags = low_contrast_lines(&[dark, light.clone(), body], |_page, _x, y| {
            if y > 250.0 {
                Some(navy)
            } else {
                Some([1.0, 1.0, 1.0])
            }
        });
        assert!(
            flags.iter().any(|flag| flag.text.contains("LangChain")),
            "{flags:?}"
        );
        assert!(
            !flags.iter().any(|flag| flag.text.contains("LangGraph")),
            "{flags:?}"
        );
        assert!(
            !flags.iter().any(|flag| flag.text.contains("正文")),
            "{flags:?}"
        );
        let _ = light;
    }

    #[test]
    fn a_short_line_in_the_middle_of_a_paragraph_is_flagged() {
        let glyphs = vec![
            span_line(1, 66.0, 400.0, 320.0, "当那些标识出现在书中，并且"),
            span_line(2, 66.0, 386.0, 54.0, "Manning"),
            span_line(3, 84.0, 372.0, 300.0, "Publications 知悉了商标声明"),
            span_line(4, 66.0, 340.0, 80.0, "印刷。"),
        ];
        let flags = short_paragraph_lines(&glyphs);
        assert!(
            flags.iter().any(|flag| flag.text.contains("Manning")),
            "{flags:?}"
        );
        assert!(
            !flags.iter().any(|flag| flag.text.contains("印刷")),
            "{flags:?}"
        );
        let ending = short_paragraph_lines(&[
            span_line(5, 93.0, 500.0, 70.0, "挑战相结合。"),
            span_line(6, 112.0, 486.0, 340.0, "致审稿人开始下一段。"),
        ]);
        assert!(
            ending.iter().all(|flag| !flag.text.contains("挑战")),
            "{ending:?}"
        );
        let heading = short_paragraph_lines(&[
            span_line(7, 113.0, 251.0, 12.0, "10"),
            span_line(8, 93.0, 240.0, 180.0, "10.1 内容数据库查询生成"),
        ]);
        assert!(heading.is_empty(), "{heading:?}");
        let ragged = short_paragraph_lines(&[
            span_line(9, 66.0, 400.0, 400.0, "完整的一行排到栏宽"),
            span_line(10, 66.0, 386.0, 320.0, "这一行大约只有八成"),
            span_line(11, 66.0, 372.0, 370.0, "这一行超过八成五所以留下"),
            span_line(12, 66.0, 358.0, 80.0, "结束。"),
        ]);
        assert!(
            ragged.iter().any(|flag| flag.text.contains("八成")),
            "{ragged:?}"
        );
        assert!(
            ragged.iter().all(|flag| !flag.text.contains("八成五")),
            "{ragged:?}"
        );
    }

    #[test]
    fn an_equation_digit_raised_off_the_equals_baseline_is_flagged() {
        let mut equals = glyph(1, 180.0, 220.0);
        equals.unicode = "=".into();
        equals.font_size = 9.0;
        equals.matrix = [9.0, 0.0, 0.0, 9.0, 180.0, 220.0];
        equals.advance = [6.0, 0.0];
        equals.bbox = [180.0, 218.0, 186.0, 228.0];
        let mut raised = glyph(2, 196.0, 225.5);
        raised.unicode = "6".into();
        raised.font_size = 9.0;
        raised.matrix = [9.0, 0.0, 0.0, 9.0, 196.0, 225.5];
        raised.advance = [5.0, 0.0];
        raised.bbox = [196.0, 223.0, 201.0, 233.0];
        let mut seated = glyph(3, 196.0, 180.0);
        seated.unicode = "6".into();
        seated.font_size = 9.0;
        seated.matrix = [9.0, 0.0, 0.0, 9.0, 196.0, 180.0];
        seated.advance = [5.0, 0.0];
        seated.bbox = [196.0, 178.0, 201.0, 188.0];
        let mut equals_low = equals.clone();
        equals_low.id = 4;
        equals_low.matrix[4] = 180.0;
        equals_low.matrix[5] = 180.0;
        equals_low.bbox = [180.0, 178.0, 186.0, 188.0];
        let flags = inline_math_baseline_drift(&[equals, raised, seated, equals_low]);
        assert!(
            flags
                .iter()
                .any(|flag| flag.text.contains('6') && flag.rise > 4.0),
            "{flags:?}"
        );
        assert_eq!(flags.len(), 1, "{flags:?}");
        let mut math = glyph(5, 210.0, 230.0);
        math.unicode = "6".into();
        math.font_name = "CMR10".into();
        math.font_size = 10.0;
        math.matrix = [10.0, 0.0, 0.0, 10.0, 210.0, 230.0];
        let mut host = glyph(6, 200.0, 220.0);
        host.unicode = "层".into();
        host.font_name = "RPTCJK".into();
        host.font_size = 10.0;
        host.matrix = [10.0, 0.0, 0.0, 10.0, 200.0, 220.0];
        let lifted = inline_math_baseline_drift(&[math, host]);
        assert!(lifted.iter().any(|flag| flag.text == "6"), "{lifted:?}");
    }

    #[test]
    fn a_title_between_rules_must_share_their_axis() {
        let mut title = glyph(1, 212.0, 630.0);
        title.unicode = "Attention Is All You Need".into();
        title.font_size = 17.0;
        title.matrix = [17.0, 0.0, 0.0, 17.0, 212.0, 630.0];
        title.bbox = [212.0, 620.0, 422.0, 646.0];
        title.advance = [210.0, 0.0];
        let mut body = glyph(2, 108.0, 400.0);
        body.unicode = "正文行保持左对齐而不被当成标题".into();
        body.font_size = 10.0;
        body.matrix = [10.0, 0.0, 0.0, 10.0, 108.0, 400.0];
        body.bbox = [108.0, 392.0, 300.0, 408.0];
        let mut page = page_with(vec![title, body]);
        page.regions = vec![
            rpt_core::glyph::PaintedRegion {
                page_index: 0,
                bbox: [108.0, 659.8, 504.0, 663.8],
                kind: "path".into(),
            },
            rpt_core::glyph::PaintedRegion {
                page_index: 0,
                bbox: [108.0, 613.9, 504.0, 613.9],
                kind: "path".into(),
            },
        ];
        let off = title_axis_drift(&page);
        assert!(
            off.iter().any(|flag| flag.text.contains("Attention")),
            "{off:?}"
        );
        assert!(
            !off.iter().any(|flag| flag.text.contains("正文")),
            "{off:?}"
        );
        page.glyphs[0].bbox = [201.0, 620.0, 411.0, 646.0];
        page.glyphs[0].matrix[4] = 201.0;
        let centered = title_axis_drift(&page);
        assert!(
            !centered.iter().any(|flag| flag.text.contains("Attention")),
            "{centered:?}"
        );
    }

    #[test]
    fn a_short_rule_with_no_glyph_is_an_orphan() {
        let mut radicand = glyph(1, 126.0, 259.0);
        radicand.unicode = "d".into();
        radicand.bbox = [126.0, 257.0, 132.0, 266.0];
        let mut page = page_with(vec![radicand]);
        page.regions = vec![
            rpt_core::glyph::PaintedRegion {
                page_index: 0,
                bbox: [120.0, 265.4, 135.0, 265.8],
                kind: "path".into(),
            },
            rpt_core::glyph::PaintedRegion {
                page_index: 0,
                bbox: [260.0, 220.0, 275.0, 220.4],
                kind: "path".into(),
            },
            rpt_core::glyph::PaintedRegion {
                page_index: 0,
                bbox: [108.0, 94.0, 251.0, 94.4],
                kind: "path".into(),
            },
        ];
        let flags = orphan_rules(&page);
        assert!(
            flags.iter().any(|flag| (flag.x - 260.0).abs() < 0.1),
            "the bar with no glyph was kept: {flags:?}"
        );
        assert!(
            !flags.iter().any(|flag| (flag.x - 120.0).abs() < 0.1),
            "the vinculum on d was flagged: {flags:?}"
        );
        assert!(
            !flags.iter().any(|flag| flag.width > 40.0),
            "a footnote rule was flagged: {flags:?}"
        );
    }
}
