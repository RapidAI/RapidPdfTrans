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
        // A wide gap on one baseline is a column gutter, not a missing space.
        // A jump back to the left is the other column on a nearby baseline.
        let left = glyph.bbox[0].min(glyph.bbox[2]);
        let column_gap = current.last().is_some_and(|prev| {
            let right = prev.bbox[0].max(prev.bbox[2]);
            left - right > 24.0
        });
        let jumped_back = current.last().is_some_and(|prev| {
            let prev_left = prev.bbox[0].min(prev.bbox[2]);
            prev_left - left > 24.0
        });
        if (column_gap || jumped_back) && !current.is_empty() {
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
        let mut previous: Option<&Glyph> = None;
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
            if let Some(prev) = previous {
                if word_space(prev, glyph) {
                    buf.push(' ');
                }
            }
            previous = Some(glyph);
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
    join_line_break_hyphens(glyphs, segments)
}

/// LaTeX line-break hyphens (`tra-` / `jectories`) are separate lines. Join them
/// so the translator sees the whole word. A following capital, or a line in the
/// other column, stays separate.
fn join_line_break_hyphens(glyphs: &[Glyph], segments: Vec<Segment>) -> Vec<Segment> {
    if segments.len() < 2 {
        return segments;
    }
    let by_id: std::collections::HashMap<u32, &Glyph> =
        glyphs.iter().map(|glyph| (glyph.id, glyph)).collect();
    let mut consumed = vec![false; segments.len()];
    let mut out = Vec::new();
    for i in 0..segments.len() {
        if consumed[i] {
            continue;
        }
        consumed[i] = true;
        let mut seg = segments[i].clone();
        while let Some(next_index) = hyphen_continuation(&seg, &segments, &consumed, &by_id) {
            let next = &segments[next_index];
            let rest = next.text.trim_start();
            let Some(stem) = soft_hyphen_stem(&seg.text) else {
                break;
            };
            if !rest.starts_with(|ch: char| ch.is_ascii_lowercase()) {
                break;
            }
            consumed[next_index] = true;
            seg.text = format!("{stem}{rest}");
            seg.glyph_ids.extend(next.glyph_ids.iter().copied());
        }
        out.push(seg);
    }
    for (id, seg) in out.iter_mut().enumerate() {
        seg.id = id as u32;
    }
    out
}

fn soft_hyphen_stem(text: &str) -> Option<&str> {
    let trimmed = text.trim_end();
    let mut chars = trimmed.chars();
    let last = chars.next_back()?;
    let prev = chars.next_back()?;
    if last == '-' && prev.is_ascii_alphabetic() {
        Some(&trimmed[..trimmed.len() - '-'.len_utf8()])
    } else {
        None
    }
}

fn hyphen_continuation(
    segment: &Segment,
    segments: &[Segment],
    consumed: &[bool],
    by_id: &std::collections::HashMap<u32, &Glyph>,
) -> Option<usize> {
    let head_id = *segment.glyph_ids.first()?;
    let tail_id = *segment.glyph_ids.last()?;
    let head = by_id.get(&head_id)?;
    let tail = by_id.get(&tail_id)?;
    let mut best: Option<(usize, f32)> = None;
    for (index, other) in segments.iter().enumerate() {
        if consumed[index] || other.page_index != segment.page_index {
            continue;
        }
        let first_id = *other.glyph_ids.first()?;
        let first = by_id.get(&first_id)?;
        let size = tail.font_size.max(1.0);
        let dy = tail.matrix[5] - first.matrix[5];
        if dy < size * 0.45 || dy > size * 2.4 {
            continue;
        }
        if (first.matrix[4] - head.matrix[4]).abs() > 36.0 {
            continue;
        }
        if best.is_none_or(|(_, best_dy)| dy < best_dy) {
            best = Some((index, dy));
        }
    }
    best.map(|(index, _)| index)
}

/// A gap that is a word space in the original drawing, not a character in the stream.
fn word_space(prev: &Glyph, next: &Glyph) -> bool {
    if prev.unicode.chars().all(char::is_whitespace)
        || next.unicode.chars().all(char::is_whitespace)
    {
        return false;
    }
    if prev.unicode.chars().any(is_cjk) && next.unicode.chars().any(is_cjk) {
        return false;
    }
    let right = prev.bbox[0].max(prev.bbox[2]);
    let left = next.bbox[0].min(next.bbox[2]);
    let gap = left - right;
    let size = prev.font_size.max(1.0);
    gap > size * 0.18 && gap < 24.0
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch,
        '\u{3000}'..='\u{303F}'
            | '\u{3040}'..='\u{30FF}'
            | '\u{3400}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FF00}'..='\u{FFEF}'
    )
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

    #[test]
    fn a_nearby_baseline_in_the_other_column_is_its_own_line() {
        let glyphs = vec![
            glyph(0, 320.0, 648.0, "I", false),
            glyph(1, 326.0, 648.0, "n", false),
            glyph(2, 55.0, 643.0, "R", false),
            glyph(3, 61.0, 643.0, "e", false),
        ];
        let segs = segment_glyphs(&glyphs);
        let texts: Vec<_> = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["In", "Re"]);
    }

    #[test]
    fn a_word_gap_becomes_a_space_and_cjk_does_not() {
        let mut glyphs = vec![
            glyph(0, 0.0, 700.0, "A", false),
            glyph(1, 12.0, 700.0, "B", false),
        ];
        glyphs[1].bbox = [12.0, 700.0, 18.0, 710.0];
        let segs = segment_glyphs(&glyphs);
        assert_eq!(segs[0].text, "A B");
        assert_eq!(segs[0].glyph_ids, vec![0, 1]);

        let mut cjk = vec![
            glyph(0, 0.0, 700.0, "中", false),
            glyph(1, 14.0, 700.0, "文", false),
        ];
        cjk[1].bbox = [14.0, 700.0, 26.0, 710.0];
        let segs = segment_glyphs(&cjk);
        assert_eq!(segs[0].text, "中文");
    }

    #[test]
    fn a_line_break_hyphen_joins_the_next_line_in_the_same_column() {
        let glyphs = vec![
            glyph(0, 72.0, 700.0, "through long tra-", false),
            glyph(1, 72.0, 686.0, "jectories of code", false),
            glyph(2, 320.0, 686.0, "other column", false),
        ];
        let segs = segment_glyphs(&glyphs);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("trajectories of code")),
            "{texts:?}"
        );
        assert!(texts.contains(&"other column"), "{texts:?}");
        assert!(
            !texts.iter().any(|text| text.ends_with("tra-")),
            "{texts:?}"
        );
    }
}
