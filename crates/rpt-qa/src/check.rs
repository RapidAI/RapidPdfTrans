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
}
