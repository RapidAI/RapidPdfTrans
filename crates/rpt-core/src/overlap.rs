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

/// How a source block sits in its column or on the page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BlockAlign {
    Start,
    /// `axis` is the source midpoint, which already lines up with the page
    /// or the column.
    Center {
        axis: f32,
    },
    /// Lines share `edge` and their left edges do not.
    End {
        edge: f32,
    },
}

/// One source line, in user space. `left` and `right` are the ink edges.
#[derive(Clone, Copy, Debug)]
pub struct AlignLine {
    pub left: f32,
    pub right: f32,
    pub size: f32,
}

/// Centered when every line's midpoint meets the page center or the column
/// center. Right-aligned when the right edges meet and the midpoints do not.
/// A justified column meets both edges and stays [`BlockAlign::Start`].
pub fn block_alignment(
    lines: &[AlignLine],
    media: [f32; 4],
    column: Option<(f32, f32)>,
) -> BlockAlign {
    let lines: Vec<&AlignLine> = lines
        .iter()
        .filter(|line| {
            let size = line.size.max(1.0);
            line.right - line.left >= size * 1.5
        })
        .collect();
    if lines.is_empty() {
        return BlockAlign::Start;
    }
    let size = lines
        .iter()
        .map(|line| line.size)
        .fold(1.0, f32::max)
        .max(1.0);
    let page_left = media[0];
    let page_right = media[2];
    let page_width = (page_right - page_left).max(1.0);
    let page_mid = (page_left + page_right) * 0.5;
    let tol = (size * 0.9).max(10.0);
    // A body line's margin is about a tenth of the page. A centered title
    // sits further in than that on both sides.
    let margin_floor = (page_width * 0.16).max(size * 4.0);
    let mid = |line: &AlignLine| (line.left + line.right) * 0.5;
    let mids: Vec<f32> = lines.iter().copied().map(mid).collect();
    let left_min = lines.iter().map(|line| line.left).fold(f32::MAX, f32::min);
    let left_max = lines.iter().map(|line| line.left).fold(f32::MIN, f32::max);
    let right_min = lines.iter().map(|line| line.right).fold(f32::MAX, f32::min);
    let right_max = lines.iter().map(|line| line.right).fold(f32::MIN, f32::max);
    let flush_both = lines.len() >= 2 && left_max - left_min <= 2.5 && right_max - right_min <= 2.5;
    let flush_left = lines
        .iter()
        .filter(|line| (line.left - left_min).abs() <= 2.5)
        .count();
    // A justified body column shares the left margin. Four full lines of
    // that column also share the page center, and centering them slides the
    // short last line into the middle of the measure. A notice sits further
    // in than a tenth of the page (`124` vs a body at `108`).
    let body_flush =
        flush_left * 2 >= lines.len() && left_min - page_left < (page_width * 0.19).max(size * 8.0);
    // Two full lines of a centered header share both edges, the same way a
    // justified column does. The margin keeps that column out: a body line
    // sits closer to the page edge than a title.
    let page_centered = lines.len() <= 4
        && lines.iter().all(|line| {
            (mid(line) - page_mid).abs() <= tol
                && line.left - page_left >= margin_floor
                && page_right - line.right >= margin_floor
        });
    if page_centered && !body_flush {
        return BlockAlign::Center {
            axis: median(&mids),
        };
    }
    let axis = median(&mids);
    let mids_agree = mids.iter().all(|item| (item - axis).abs() <= tol);
    let lefts_wander = left_max - left_min > size * 0.6;
    let rights_wander = right_max - right_min > size * 0.6;
    // A hanging list also wanders a little on both edges. It is centered only
    // when that shared midpoint is the page or the column, not the list itself.
    let on_column = column.is_some_and(|(left, right)| {
        let col_mid = (left + right) * 0.5;
        (axis - col_mid).abs() <= tol
    });
    if lines.len() >= 2
        && mids_agree
        && lefts_wander
        && rights_wander
        && !flush_both
        && !body_flush
        && ((axis - page_mid).abs() <= tol || on_column)
    {
        return BlockAlign::Center { axis };
    }
    if let Some((col_left, col_right)) = column {
        let col_mid = (col_left + col_right) * 0.5;
        let col_width = col_right - col_left;
        let in_column = col_width > size * 8.0
            && lines.iter().all(|line| {
                (mid(line) - col_mid).abs() <= tol
                    && line.left >= col_left + size * 0.8
                    && line.right <= col_right - size * 0.8
            });
        if in_column && !flush_both && !body_flush {
            return BlockAlign::Center {
                axis: median(&mids),
            };
        }
    }
    let rights_agree = right_max - right_min <= 2.5;
    let left_slop: f32 = if size >= 16.0 { 3.0 } else { 8.0 };
    let flush_left = lines
        .iter()
        .filter(|line| (line.left - left_min).abs() <= 3.0)
        .count();
    // A justified column shares the right edge too. An indent is a couple of
    // ems; a right-aligned title steps much further, and most of its lines
    // are not flush with the leftmost one.
    let ragged_enough = left_max - left_min > left_slop.max(size * 3.0);
    let mostly_flush = flush_left * 2 > lines.len();
    if lines.len() >= 2 && rights_agree && ragged_enough && !mids_agree && !mostly_flush {
        return BlockAlign::End { edge: right_max };
    }
    BlockAlign::Start
}

fn median(values: &[f32]) -> f32 {
    let mut values = values.to_vec();
    values.retain(|value| value.is_finite());
    if values.is_empty() {
        return f32::NAN;
    }
    values.sort_by(|left, right| left.total_cmp(right));
    values[values.len() / 2]
}

/// Translated lines that left a centered source on the left edge, or a
/// right-aligned source short of its edge.
pub fn find_alignment_drift(source: &Extraction, translated: &Extraction) -> Vec<OverlapHit> {
    let mut hits = Vec::new();
    let mut pages: Vec<u32> = source.glyphs.iter().map(|glyph| glyph.page_index).collect();
    pages.sort_unstable();
    pages.dedup();
    for page in pages {
        let media = source
            .pages
            .iter()
            .find(|info| info.index == page)
            .map(|info| info.media_box)
            .unwrap_or([0.0, 0.0, 612.0, 792.0]);
        let source_lines = positioned_lines(
            &source
                .glyphs
                .iter()
                .filter(|glyph| glyph.page_index == page && !glyph.invisible)
                .collect::<Vec<_>>(),
        );
        let translated_lines = positioned_lines(
            &translated
                .glyphs
                .iter()
                .filter(|glyph| glyph.page_index == page && !glyph.invisible)
                .collect::<Vec<_>>(),
        );
        let translated_lines: Vec<_> = translated_lines
            .into_iter()
            .filter(|line| line.translated)
            .collect();
        let column = column_span(&source_lines);
        let mut seen = Vec::new();
        for (index, line) in source_lines.iter().enumerate() {
            let window: Vec<AlignLine> = source_lines
                .iter()
                .filter(|other| {
                    (other.y - line.y).abs() <= line.size.max(1.0) * 2.4
                        && ((other.right - line.right).abs() <= 3.0
                            || (other.mid() - line.mid()).abs() <= line.size.max(1.0))
                })
                .map(|other| other.align_line())
                .collect();
            let align = block_alignment(&window, media, column);
            let (kind, axis, edge) = match align {
                BlockAlign::Center { axis } => ("center", axis, 0.0),
                BlockAlign::End { edge } => ("right", 0.0, edge),
                BlockAlign::Start => continue,
            };
            let size = line.size.max(1.0);
            for other in &translated_lines {
                let below = line.y - other.y;
                if below < -size * 0.45 || below > size * 1.45 {
                    continue;
                }
                let key = (kind, index, (other.y * 10.0).round() as i32);
                if seen.contains(&key) {
                    continue;
                }
                let off = match align {
                    BlockAlign::Center { .. } => {
                        (other.mid() - axis).abs() > (size * 0.45).max(6.0)
                    }
                    BlockAlign::End { .. } => other.right + 6.0 < edge,
                    BlockAlign::Start => false,
                };
                if off {
                    seen.push(key);
                    hits.push(OverlapHit {
                        page_index: page,
                        kind,
                        detail: format!(
                            "translated {:?} left the source {:?} {}",
                            snippet(&other.text),
                            snippet(&line.text),
                            if kind == "center" {
                                format!("axis {axis:.0}")
                            } else {
                                format!("edge {edge:.0}")
                            }
                        ),
                    });
                }
            }
        }
    }
    hits
}

struct PositionedLine {
    text: String,
    translated: bool,
    y: f32,
    left: f32,
    right: f32,
    size: f32,
}

impl PositionedLine {
    fn mid(&self) -> f32 {
        (self.left + self.right) * 0.5
    }

    fn align_line(&self) -> AlignLine {
        AlignLine {
            left: self.left,
            right: self.right,
            size: self.size,
        }
    }
}

fn positioned_lines(glyphs: &[&Glyph]) -> Vec<PositionedLine> {
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
            (group[0].matrix[5] - glyph.matrix[5]).abs() <= size * 0.35
        });
        if same {
            groups.last_mut().unwrap().push(glyph);
        } else {
            groups.push(vec![glyph]);
        }
    }
    groups
        .into_iter()
        .filter_map(|group| {
            let text: String = group.iter().map(|glyph| glyph.unicode.as_str()).collect();
            if text.trim().is_empty() {
                return None;
            }
            let translated = group.iter().any(|glyph| {
                glyph.font_name.contains("RPTCJK")
                    || glyph
                        .unicode
                        .chars()
                        .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch))
            });
            let left = group
                .iter()
                .map(|glyph| glyph.bbox[0].min(glyph.bbox[2]).min(glyph.matrix[4]))
                .fold(f32::MAX, f32::min);
            let right = group
                .iter()
                .map(|glyph| {
                    glyph.bbox[0]
                        .max(glyph.bbox[2])
                        .max(glyph.matrix[4] + glyph.advance[0].max(0.0))
                })
                .fold(left, f32::max);
            let size = group
                .iter()
                .map(|glyph| glyph.font_size)
                .fold(1.0, f32::max)
                .max(1.0);
            let y = group[group.len() / 2].matrix[5];
            Some(PositionedLine {
                text,
                translated,
                y,
                left,
                right,
                size,
            })
        })
        .collect()
}

fn column_span(lines: &[PositionedLine]) -> Option<(f32, f32)> {
    let mut spans: Vec<(f32, f32)> = lines
        .iter()
        .filter(|line| line.right - line.left >= line.size * 12.0)
        .map(|line| (line.left, line.right))
        .collect();
    if spans.len() < 3 {
        return None;
    }
    spans.sort_by(|left, right| left.0.total_cmp(&right.0));
    let left = spans[spans.len() / 2].0;
    spans.sort_by(|left, right| left.1.total_cmp(&right.1));
    let right = spans[spans.len() / 2].1;
    (right - left > 80.0).then_some((left, right))
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

    fn placed(id: u32, x: f32, y: f32, right: f32, size: f32, text: &str, font: &str) -> Glyph {
        let mut glyph = glyph(id, x, y, text, font);
        glyph.font_size = size;
        glyph.matrix = [size, 0.0, 0.0, size, x, y];
        glyph.bbox = [x, y - size * 0.2, right, y + size * 0.8];
        glyph.advance = [right - x, 0.0];
        glyph
    }

    #[test]
    fn a_title_midpoint_on_the_page_center_is_centered() {
        let media = [0.0, 0.0, 612.0, 792.0];
        let title = AlignLine {
            left: 153.4,
            right: 441.8,
            size: 14.3,
        };
        match block_alignment(&[title], media, None) {
            BlockAlign::Center { axis } => assert!((axis - 297.6).abs() < 0.2, "{axis}"),
            other => panic!("title was {other:?}"),
        }
        let body = AlignLine {
            left: 50.0,
            right: 286.0,
            size: 10.0,
        };
        assert!(
            matches!(block_alignment(&[body], media, None), BlockAlign::Start),
            "a column line is not a centered title"
        );
        let header = [
            AlignLine {
                left: 124.0,
                right: 488.0,
                size: 10.0,
            },
            AlignLine {
                left: 124.3,
                right: 487.9,
                size: 10.0,
            },
        ];
        match block_alignment(&header, media, None) {
            BlockAlign::Center { axis } => assert!((axis - 306.0).abs() < 2.0, "{axis}"),
            other => panic!("equal centered lines were {other:?}"),
        }
    }

    #[test]
    fn a_justified_body_paragraph_is_not_centered() {
        let media = [0.0, 0.0, 612.0, 792.0];
        let body = [
            AlignLine {
                left: 108.0,
                right: 504.0,
                size: 10.0,
            },
            AlignLine {
                left: 108.0,
                right: 504.0,
                size: 10.0,
            },
            AlignLine {
                left: 108.0,
                right: 504.0,
                size: 10.0,
            },
            AlignLine {
                left: 108.0,
                right: 459.0,
                size: 10.0,
            },
        ];
        assert!(
            matches!(block_alignment(&body, media, None), BlockAlign::Start),
            "a body column that shares the left margin is not a centered notice"
        );
    }

    #[test]
    fn a_short_title_left_on_the_english_edge_is_drift() {
        let source = extraction(vec![placed(
            0,
            153.4,
            700.0,
            441.8,
            14.3,
            "Deep Residual Learning for Image Recognition",
            "NimbusRom",
        )]);
        let shifted = extraction(vec![placed(
            1,
            153.4,
            700.0,
            340.0,
            13.5,
            "深度残差学习用于图像识别",
            "RPTCJK-RPT0",
        )]);
        let hits = find_alignment_drift(&source, &shifted);
        assert!(hits.iter().any(|hit| hit.kind == "center"), "{hits:?}");
        let centered = extraction(vec![placed(
            1,
            201.0,
            700.0,
            394.0,
            13.5,
            "深度残差学习用于图像识别",
            "RPTCJK-RPT0",
        )]);
        let clean = find_alignment_drift(&source, &centered);
        assert!(clean.is_empty(), "{clean:?}");
    }

    #[test]
    fn a_right_aligned_block_that_stays_left_is_drift() {
        let media = [0.0, 0.0, 612.0, 792.0];
        let lines = [
            AlignLine {
                left: 266.0,
                right: 500.0,
                size: 18.0,
            },
            AlignLine {
                left: 374.0,
                right: 500.0,
                size: 18.0,
            },
        ];
        assert!(
            matches!(
                block_alignment(&lines, media, None),
                BlockAlign::End { edge } if (edge - 500.0).abs() < 0.1
            ),
            "{:?}",
            block_alignment(&lines, media, None)
        );
        let source = extraction(vec![
            placed(
                0,
                266.0,
                520.0,
                500.0,
                18.0,
                "AI Agents and Applications",
                "Times",
            ),
            placed(1, 374.0, 480.0, 500.0, 18.0, "First printing", "Times"),
        ]);
        let shifted = extraction(vec![placed(
            2,
            374.0,
            480.0,
            420.0,
            17.0,
            "初版",
            "RPTCJK-RPT0",
        )]);
        let hits = find_alignment_drift(&source, &shifted);
        assert!(hits.iter().any(|hit| hit.kind == "right"), "{hits:?}");
    }
}
