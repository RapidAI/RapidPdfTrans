//! Group extracted glyphs into translation segments.
//!
//! A segment is one visual line (same page, similar baseline), split further
//! only when the line is long. Unmapped glyphs are not folded into a segment:
//! they stay pending so a failed Unicode mapping cannot vanish inside a
//! translation.

use crate::glyph::Glyph;

const LONG_LINE: usize = 400;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub id: u32,
    pub page_index: u32,
    pub glyph_ids: Vec<u32>,
    pub text: String,
}

pub fn segment_glyphs(glyphs: &[Glyph]) -> Vec<Segment> {
    let mut ordered: Vec<&Glyph> = glyphs.iter().collect();
    ordered.sort_by(|a, b| {
        a.page_index
            .cmp(&b.page_index)
            .then(
                b.matrix[5]
                    .partial_cmp(&a.matrix[5])
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(
                a.matrix[4]
                    .partial_cmp(&b.matrix[4])
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });

    let mut lines: Vec<Vec<&Glyph>> = Vec::new();
    let mut current: Vec<&Glyph> = Vec::new();
    let mut last_page: Option<u32> = None;
    let mut last_y: Option<f32> = None;
    let mut last_size = 12.0f32;
    for glyph in ordered {
        if glyph.unmapped || glyph.unicode.is_empty() {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            last_page = None;
            last_y = None;
            continue;
        }
        let new_line = match (last_page, last_y) {
            (Some(page), Some(y)) => {
                glyph.page_index != page || (glyph.matrix[5] - y).abs() > last_size * 0.5
            }
            _ => !current.is_empty(),
        };
        if new_line && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        last_page = Some(glyph.page_index);
        last_y = Some(glyph.matrix[5]);
        last_size = glyph.font_size.max(1.0);
        current.push(glyph);
    }
    if !current.is_empty() {
        lines.push(current);
    }

    let mut segments = Vec::new();
    for line in lines {
        let page = line[0].page_index;
        let mut buf_ids: Vec<u32> = Vec::new();
        let mut buf = String::new();
        let flush = |segments: &mut Vec<Segment>, buf_ids: &mut Vec<u32>, buf: &mut String| {
            if buf_ids.is_empty() {
                return;
            }
            segments.push(Segment {
                id: segments.len() as u32,
                page_index: page,
                glyph_ids: std::mem::take(buf_ids),
                text: std::mem::take(buf),
            });
        };
        for glyph in line {
            let next_len = buf.len() + glyph.unicode.len();
            let boundary = next_len > LONG_LINE && sentence_end(&buf);
            if boundary {
                flush(&mut segments, &mut buf_ids, &mut buf);
            }
            buf.push_str(&glyph.unicode);
            buf_ids.push(glyph.id);
        }
        flush(&mut segments, &mut buf_ids, &mut buf);
    }
    segments
}

fn sentence_end(text: &str) -> bool {
    let t = text.trim_end();
    t.ends_with(['.', '!', '?', '。', '！', '？']) && t.len() > 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::glyph::{Disposition, Glyph, GlyphSource, SourceKind};

    fn glyph(id: u32, x: f32, y: f32, text: &str, unmapped: bool) -> Glyph {
        Glyph {
            id,
            page_index: 0,
            unicode: if unmapped { String::new() } else { text.into() },
            unmapped,
            char_code: vec![b'A'],
            gid: None,
            font_resource: "F1".into(),
            font_name: "Helvetica".into(),
            font_object: None,
            font_size: 12.0,
            matrix: [12.0, 0.0, 0.0, 12.0, x, y],
            bbox: [x, y, x + 6.0, y + 10.0],
            advance: [6.0, 0.0],
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
                operator_index: 0,
                byte_start: 0,
                byte_end: 1,
                resource_name: Some("F1".into()),
            },
        }
    }

    #[test]
    fn lines_split_on_baseline_and_unmapped_is_excluded() {
        let glyphs = vec![
            glyph(0, 0.0, 700.0, "A", false),
            glyph(1, 6.0, 700.0, "B", false),
            glyph(2, 12.0, 700.0, "", true),
            glyph(3, 18.0, 700.0, "C", false),
            glyph(4, 0.0, 680.0, "D", false),
        ];
        let segs = segment_glyphs(&glyphs);
        let texts: Vec<_> = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["AB", "C", "D"]);
        assert_eq!(segs[0].glyph_ids, vec![0, 1]);
        assert!(!segs.iter().any(|s| s.glyph_ids.contains(&2)));
    }
}
