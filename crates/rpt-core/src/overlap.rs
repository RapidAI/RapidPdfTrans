//! Overlap checks for a translated page.
//!
//! A translation may not paint across a column gutter, and it may not cover
//! text, a table rule, or an image that was left in place.

use crate::extract::Extraction;
use crate::glyph::Glyph;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlapHit {
    pub page_index: u32,
    /// `text`, `rule`, `image`, or `column`.
    pub kind: &'static str,
    pub detail: String,
}

/// Translated ink that crosses another text box, a rule, an image, or a gutter.
pub fn find_overlaps(extraction: &Extraction) -> Vec<OverlapHit> {
    let mut hits = Vec::new();
    let mut pages: Vec<u32> = extraction
        .glyphs
        .iter()
        .map(|glyph| glyph.page_index)
        .collect();
    pages.sort_unstable();
    pages.dedup();
    for page in pages {
        let glyphs: Vec<&Glyph> = extraction
            .glyphs
            .iter()
            .filter(|glyph| glyph.page_index == page && !glyph.invisible)
            .collect();
        let lines = text_lines(&glyphs);
        let translated: Vec<&TextLine> = lines.iter().filter(|line| line.translated).collect();
        let others: Vec<&TextLine> = lines.iter().filter(|line| !line.translated).collect();
        for line in &translated {
            for other in &others {
                if boxes_overlap(line.bbox, other.bbox) {
                    hits.push(OverlapHit {
                        page_index: page,
                        kind: "text",
                        detail: format!(
                            "translated {:?} covers {:?}",
                            snippet(&line.text),
                            snippet(&other.text)
                        ),
                    });
                    break;
                }
            }
            for region in extraction
                .regions
                .iter()
                .filter(|region| region.page_index == page)
            {
                let y0 = region.bbox[1].min(region.bbox[3]);
                let y1 = region.bbox[1].max(region.bbox[3]);
                let x0 = region.bbox[0].min(region.bbox[2]);
                let x1 = region.bbox[0].max(region.bbox[2]);
                let rule = region.kind == "path" && y1 - y0 < 2.5 && x1 - x0 > 30.0;
                let image = region.kind == "image";
                if !rule && !image {
                    continue;
                }
                if boxes_overlap(line.bbox, [x0, y0, x1, y1]) {
                    hits.push(OverlapHit {
                        page_index: page,
                        kind: if image { "image" } else { "rule" },
                        detail: format!(
                            "translated {:?} crosses a {}",
                            snippet(&line.text),
                            region.kind
                        ),
                    });
                }
            }
        }
        if let Some(gutter) = column_gutter(&lines) {
            for line in &translated {
                // A title that fills the page has no neighbor in the other
                // column. A caption that runs into that column does.
                let into_other = line.bbox[0] + 24.0 < gutter && line.bbox[2] > gutter + 4.0;
                let neighbor = lines.iter().any(|other| {
                    other.bbox != line.bbox
                        && other.bbox[0] + 8.0 >= gutter
                        && other.bbox[1] < line.bbox[3] - 1.0
                        && other.bbox[3] > line.bbox[1] + 1.0
                });
                if into_other && neighbor {
                    hits.push(OverlapHit {
                        page_index: page,
                        kind: "column",
                        detail: format!(
                            "translated {:?} crosses the gutter at {gutter:.0}",
                            snippet(&line.text)
                        ),
                    });
                }
            }
        }
    }
    hits
}

struct TextLine {
    text: String,
    translated: bool,
    bbox: [f32; 4],
}

fn text_lines(glyphs: &[&Glyph]) -> Vec<TextLine> {
    let mut ordered: Vec<&Glyph> = glyphs
        .iter()
        .copied()
        .filter(|glyph| !glyph.unicode.trim().is_empty())
        .collect();
    ordered.sort_by(|left, right| {
        right.matrix[5]
            .total_cmp(&left.matrix[5])
            .then(left.matrix[4].total_cmp(&right.matrix[4]))
    });
    let mut groups: Vec<Vec<&Glyph>> = Vec::new();
    for glyph in ordered {
        let same = groups.last().is_some_and(|group: &Vec<&Glyph>| {
            let size = group[0].font_size.max(glyph.font_size).max(1.0);
            (group[0].matrix[5] - glyph.matrix[5]).abs() <= size * 0.45
        });
        if same {
            groups.last_mut().unwrap().push(glyph);
        } else {
            groups.push(vec![glyph]);
        }
    }
    groups
        .into_iter()
        .map(|group| {
            let text: String = group.iter().map(|glyph| glyph.unicode.as_str()).collect();
            let translated = group.iter().any(|glyph| {
                glyph.font_name.contains("RPTCJK")
                    || glyph
                        .unicode
                        .chars()
                        .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch))
            });
            let left = group
                .iter()
                .map(|glyph| glyph.bbox[0].min(glyph.bbox[2]))
                .fold(f32::MAX, f32::min);
            let right = group
                .iter()
                .map(|glyph| glyph.bbox[0].max(glyph.bbox[2]))
                .fold(left, f32::max);
            let bottom = group
                .iter()
                .map(|glyph| glyph.bbox[1].min(glyph.bbox[3]))
                .fold(f32::MAX, f32::min);
            let top = group
                .iter()
                .map(|glyph| glyph.bbox[1].max(glyph.bbox[3]))
                .fold(bottom, f32::max);
            TextLine {
                text,
                translated,
                bbox: [left, bottom, right, top],
            }
        })
        .collect()
}

fn boxes_overlap(a: [f32; 4], b: [f32; 4]) -> bool {
    let x0 = a[0].max(b[0]);
    let y0 = a[1].max(b[1]);
    let x1 = a[2].min(b[2]);
    let y1 = a[3].min(b[3]);
    let width = x1 - x0;
    let height = y1 - y0;
    if width < 1.5 || height < 1.2 {
        return false;
    }
    let area = width * height;
    let aa = ((a[2] - a[0]) * (a[3] - a[1])).max(1.0);
    let ba = ((b[2] - b[0]) * (b[3] - b[1])).max(1.0);
    area / aa.min(ba) > 0.25
}

/// Left edge of the right column, when the page has two columns.
fn column_gutter(lines: &[TextLine]) -> Option<f32> {
    let mut lefts: Vec<f32> = lines
        .iter()
        .map(|line| line.bbox[0])
        .filter(|x| x.is_finite())
        .collect();
    if lefts.len() < 4 {
        return None;
    }
    lefts.sort_by(|left, right| left.total_cmp(right));
    let mut clusters: Vec<(f32, usize)> = Vec::new();
    for x in lefts {
        if let Some(last) = clusters.last_mut() {
            if (x - last.0).abs() < 18.0 {
                last.1 += 1;
                continue;
            }
        }
        clusters.push((x, 1));
    }
    let mut populated: Vec<(f32, usize)> = clusters
        .into_iter()
        .filter(|(_, count)| *count >= 3)
        .collect();
    if populated.len() < 2 {
        return None;
    }
    populated.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.total_cmp(&right.0)));
    let first = populated[0].0;
    let second = populated[1].0;
    let (left, right) = if first < second {
        (first, second)
    } else {
        (second, first)
    };
    // A first-line indent is a few em. Two columns sit on opposite sides.
    (right - left > 80.0).then_some(right)
}

fn snippet(text: &str) -> String {
    text.chars().take(32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyph::{Disposition, Glyph, GlyphSource, SourceKind};
    use crate::Color;

    fn glyph(id: u32, x: f32, y: f32, text: &str, font: &str) -> Glyph {
        let width = text.chars().count() as f32 * 8.0;
        Glyph {
            id,
            page_index: 0,
            unicode: text.into(),
            unmapped: false,
            char_code: Vec::new(),
            gid: None,
            font_resource: "F1".into(),
            font_name: font.into(),
            font_object: None,
            font_size: 10.0,
            matrix: [10.0, 0.0, 0.0, 10.0, x, y],
            bbox: [x, y - 2.0, x + width, y + 8.0],
            advance: [width, 0.0],
            fill_color: Color::gray(0.0),
            stroke_color: Color::gray(0.0),
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
                operator_index: 0,
                byte_start: 0,
                byte_end: 0,
                resource_name: None,
            },
        }
    }

    fn extraction(glyphs: Vec<Glyph>) -> Extraction {
        Extraction {
            coordinate_space: crate::COORDINATE_SPACE,
            pages: Vec::new(),
            glyphs,
            regions: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn a_caption_painted_on_a_kept_row_is_an_overlap() {
        let glyphs = vec![
            glyph(0, 72.0, 400.0, "No NSP", "NimbusRom"),
            glyph(1, 72.0, 394.0, "表5消融", "RPTCJK-RPT0"),
        ];
        let hits = find_overlaps(&extraction(glyphs));
        assert!(hits.iter().any(|hit| hit.kind == "text"), "{hits:?}");
    }

    #[test]
    fn a_translation_that_stays_in_its_column_is_clean() {
        let glyphs = vec![
            glyph(0, 72.0, 500.0, "表5消融实验", "RPTCJK-RPT0"),
            glyph(1, 72.0, 400.0, "No NSP", "NimbusRom"),
            glyph(2, 320.0, 500.0, "右栏正文", "RPTCJK-RPT0"),
        ];
        let hits = find_overlaps(&extraction(glyphs));
        assert!(hits.is_empty(), "{hits:?}");
    }

    #[test]
    fn a_line_that_crosses_the_gutter_is_flagged() {
        let mut glyphs = Vec::new();
        // Two real columns, then one translated line that spans both.
        for index in 0..3 {
            let y = 600.0 - index as f32 * 16.0;
            glyphs.push(glyph(index, 72.0, y, "左栏的一行正文", "RPTCJK-RPT0"));
            glyphs.push(glyph(
                index + 10,
                340.0,
                y - 7.0,
                "右栏的一行正文",
                "RPTCJK-RPT0",
            ));
        }
        let mut span = glyph(30, 72.0, 600.0, "这一行跨过了栏间空白", "RPTCJK-RPT0");
        span.bbox[2] = 420.0;
        glyphs.push(span);
        let hits = find_overlaps(&extraction(glyphs));
        assert!(hits.iter().any(|hit| hit.kind == "column"), "{hits:?}");
    }
}
