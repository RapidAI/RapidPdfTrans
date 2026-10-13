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
}
