//! Group extracted glyphs into paragraphs.
//!
//! Detection is a pure function of glyph positions and font names. It does
//! not call a model. A segment is one paragraph: visual lines in the same
//! column, joined so a later rewrite can reflow the block inside that box.
//! Superscripts and footnote marks are attached to the line they sit on.
//! Figure interiors and table cells are not segments. A table caption and
//! its column header are. Unmapped glyphs stay out of every segment.

use crate::geom::Rect;
use crate::glyph::{Glyph, PageInfo, PaintedRegion};

const LONG_LINE: usize = 1600;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub id: u32,
    pub page_index: u32,
    pub glyph_ids: Vec<u32>,
    pub text: String,
}

/// Figure interiors and table cells stay original unless the caller opts in.
#[derive(Clone, Debug)]
pub struct SegmentFlags {
    pub skip_figures: bool,
    pub skip_tables: bool,
}

impl Default for SegmentFlags {
    fn default() -> Self {
        Self {
            skip_figures: true,
            skip_tables: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Segmentation {
    pub segments: Vec<Segment>,
    /// Glyphs that belong to a figure interior or a table body, with a reason.
    pub kept: Vec<(u32, String)>,
}

pub fn segment_glyphs(glyphs: &[Glyph]) -> Vec<Segment> {
    segment_with(glyphs, &SegmentFlags::default()).segments
}

pub fn segment_with(glyphs: &[Glyph], flags: &SegmentFlags) -> Segmentation {
    segment_placed(glyphs, flags, &[], &[])
}

/// Like [`segment_with`], and also keeps labels inside painted boxes and images.
pub fn segment_placed(
    glyphs: &[Glyph],
    flags: &SegmentFlags,
    regions: &[PaintedRegion],
    pages: &[PageInfo],
) -> Segmentation {
    let lines = raw_lines(glyphs);
    assemble(lines, flags, regions, pages)
}

fn raw_lines(glyphs: &[Glyph]) -> Vec<Vec<&Glyph>> {
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
            // The gutter often holds an unmapped drawing. Flushing the line
            // without a column split glues the equation to the prose.
            if !current.is_empty() {
                lines.extend(finish_line(std::mem::take(&mut current)));
            }
            last_page = None;
            last_y = None;
            continue;
        }
        // A 20pt margin stamp between two body lines used to set the window
        // from that stamp. The next line, one leading down, stayed on this
        // line, and x-sorting then interleaved the two sentences. The anchor
        // stays the body baseline already collected.
        let incoming = glyph.font_size.max(1.0);
        let (anchor_y, tolerance) = if let Some((y, size)) = body_anchor(&current) {
            let mismatched = incoming > size * 1.8 || size > incoming * 1.8;
            let tolerance = if mismatched {
                size.min(incoming) * 0.5
            } else {
                size * 0.5
            };
            // A display delimiter (`(` from CMEX) is drawn from a baseline
            // well off its text. Half an em glued the formula's big parens
            // into the heading above it (`Defini(tion 3 (Fisher Infor)mation`).
            let prose_letters = current
                .iter()
                .filter(|item| !math_font_glyph(item))
                .filter(|item| item.unicode.chars().any(char::is_alphabetic))
                .count();
            let big_delimiter =
                prose_letters >= 16 && glyph.font_name.to_ascii_uppercase().contains("CMEX");
            let tolerance = if big_delimiter {
                tolerance.min(size * 0.3)
            } else {
                tolerance
            };
            (y, tolerance)
        } else {
            (last_y.unwrap_or(glyph.matrix[5]), last_size * 0.5)
        };
        let new_line = match (last_page, last_y) {
            (Some(page), Some(_)) => {
                glyph.page_index != page || (glyph.matrix[5] - anchor_y).abs() > tolerance
            }
            _ => !current.is_empty(),
        };
        if new_line && !current.is_empty() {
            lines.extend(finish_line(std::mem::take(&mut current)));
        }
        // A jump back to the left margin is the other column on a nearby
        // baseline. A subscript under the middle of this line is also left
        // of the last glyph, and it stays. Same-baseline columns are split
        // after the line is collected: a fixed gap would also cut justified
        // word spaces.
        let left = glyph.bbox[0].min(glyph.bbox[2]);
        let line_left = current
            .iter()
            .map(|item| item.bbox[0].min(item.bbox[2]))
            .fold(f32::MAX, f32::min);
        let line_right = current
            .iter()
            .map(|item| item.bbox[0].max(item.bbox[2]))
            .fold(line_left, f32::max);
        // A drop cap is one glyph several times the letters beside it.
        // The body size is what a gutter digit and the next column use.
        let line_size = if current.is_empty() {
            last_size
        } else {
            representative_size(&current)
        };
        // A line number in the gutter is lower and smaller than this line,
        // but still inside the baseline tolerance. Gluing it to the hyphen
        // makes the line look like a contents entry, so the word never joins.
        // `LLMS` ends in a small-caps `S` a few points before the leader
        // dots. That letter is the last of the word, not a gutter mark.
        let gap_after = left - line_right;
        let small_caps_letter = glyph.unicode.trim().chars().count() == 1
            && glyph
                .unicode
                .trim()
                .chars()
                .all(|ch| ch.is_ascii_alphabetic())
            && gap_after < line_size * 1.25;
        let gutter_mark = glyph.font_size <= line_size * 0.8
            && gap_after > line_size * 0.35
            && !small_caps_letter;
        // A subscript a few points inside the left edge is not the other
        // column. A body-sized glyph that returns to this margin is: the
        // line walked onto the other column through a mark in between.
        let jumped_back = current.last().is_some_and(|prev| {
            let prev_left = prev.bbox[0].min(prev.bbox[2]);
            let back = prev_left - left > 24.0;
            let at_margin = left <= line_left + 1.0;
            let body_return = left <= line_left + 8.0 && glyph.font_size > line_size * 0.85;
            back && (at_margin || body_return)
        });
        // A 0.0002pt baseline difference sorts the right-hand glyphs first.
        // The left-hand word then looks like a jump to the other column, and
        // one sentence is cut in half (`like this` / `to separate`).
        let same_baseline = last_y.is_some_and(|y| (glyph.matrix[5] - y).abs() <= 0.6);
        // A chapter index (`10` at 27pt) sits a few points above the section
        // title. The large size keeps that title inside the baseline window.
        let chapter_below = !current.is_empty()
            && current.len() <= 3
            && current
                .iter()
                .all(|item| item.font_size >= glyph.font_size * 1.8)
            && (glyph.matrix[5] - current[0].matrix[5]).abs() > glyph.font_size * 0.55;
        if ((jumped_back && !same_baseline) || gutter_mark || chapter_below) && !current.is_empty()
        {
            lines.extend(finish_line(std::mem::take(&mut current)));
        }
        last_page = Some(glyph.page_index);
        last_y = Some(glyph.matrix[5]);
        last_size = glyph.font_size.max(1.0);
        current.push(glyph);
    }
    if !current.is_empty() {
        lines.extend(finish_line(current));
    }
    lines
}

/// Reading order reaches a lower subscript after the rest of the line.
/// Put the line back in x order before measuring gaps or building text.
fn finish_line(mut line: Vec<&Glyph>) -> Vec<Vec<&Glyph>> {
    line.sort_by(|a, b| {
        a.bbox[0]
            .min(a.bbox[2])
            .total_cmp(&b.bbox[0].min(b.bbox[2]))
    });
    peel_stacked_labels(line)
        .into_iter()
        .flat_map(split_columns)
        .collect()
}

/// A chart annotation shares the body baseline and is drawn on top of a word
/// (`Physics,` under `Highest`, `83.75%` over `Statistics`). Sorting by x
/// then writes `PhysicsH,ighest`. The label is a different size, overlaps the
/// word, and sits a fraction of an em off its baseline. A subscript is one
/// or two glyphs, so a run shorter than four stays in the line.
fn peel_stacked_labels(line: Vec<&Glyph>) -> Vec<Vec<&Glyph>> {
    if line.len() < 8 {
        return vec![line];
    }
    let mut sizes: Vec<f32> = line
        .iter()
        .map(|glyph| glyph.font_size)
        .filter(|size| *size >= 1.0)
        .collect();
    if sizes.is_empty() {
        return vec![line];
    }
    sizes.sort_by(|left, right| left.total_cmp(right));
    let body = sizes[sizes.len() / 2].max(1.0);
    let prose: Vec<&Glyph> = line
        .iter()
        .copied()
        .filter(|glyph| {
            let ratio = glyph.font_size / body;
            (0.85..=1.15).contains(&ratio)
        })
        .collect();
    if prose.len() < 8 {
        return vec![line];
    }
    let off_size = |glyph: &Glyph| {
        let ratio = glyph.font_size / body;
        ratio >= 1.2 || ratio <= 0.72
    };
    let overlaps_body = |glyph: &Glyph| {
        prose.iter().any(|other| {
            let left = glyph_left(glyph).max(glyph_left(other));
            let right = glyph_right(glyph).min(glyph_right(other));
            right - left > 0.4 && (glyph.matrix[5] - other.matrix[5]).abs() > body * 0.1
        })
    };
    let mut candidates: Vec<usize> = line
        .iter()
        .enumerate()
        .filter(|(_, glyph)| off_size(glyph))
        .map(|(index, _)| index)
        .collect();
    candidates.sort_by(|&left, &right| glyph_left(line[left]).total_cmp(&glyph_left(line[right])));
    let mut stacked = vec![false; line.len()];
    let mut index = 0usize;
    while index < candidates.len() {
        let mut end = index + 1;
        while end < candidates.len() {
            let gap = glyph_left(line[candidates[end]]) - glyph_right(line[candidates[end - 1]]);
            if gap > body * 1.25 {
                break;
            }
            end += 1;
        }
        let overlapping = candidates[index..end]
            .iter()
            .filter(|slot| overlaps_body(line[**slot]))
            .count();
        if overlapping >= 4 {
            for slot in &candidates[index..end] {
                stacked[*slot] = true;
            }
        }
        index = end;
    }
    if !stacked.iter().any(|marked| *marked) {
        return vec![line];
    }
    let prose_line: Vec<&Glyph> = line
        .iter()
        .enumerate()
        .filter(|(index, _)| !stacked[*index])
        .map(|(_, glyph)| *glyph)
        .collect();
    let labels: Vec<&Glyph> = line
        .iter()
        .enumerate()
        .filter(|(index, _)| stacked[*index])
        .map(|(_, glyph)| *glyph)
        .collect();
    if prose_line.len() < 8 || labels.is_empty() {
        return vec![line];
    }
    vec![prose_line, labels]
}

/// Split one baseline where a gap is a column gutter rather than a word space.
/// `All   three   attacks   succeeded   on   gemma-4-12b,` is one justified
/// line in a narrow column: a long token left every word space at ~1.4em.
/// Three or more wide gaps of about the same width between lowercase words
/// are word spaces, not table cells or a gutter.
fn justified_spread_line(line: &[&Glyph], gaps: &[f32], trigger: f32, hard: f32) -> bool {
    let wide: Vec<usize> = gaps
        .iter()
        .enumerate()
        .filter(|(_, gap)| **gap > trigger && **gap <= hard)
        .map(|(index, _)| index)
        .collect();
    if wide.len() < 3 || gaps.iter().any(|gap| *gap > hard) {
        return false;
    }
    let low = wide.iter().map(|i| gaps[*i]).fold(f32::MAX, f32::min);
    let high = wide.iter().map(|i| gaps[*i]).fold(0.0f32, f32::max);
    if high > low * 1.35 {
        return false;
    }
    let lowercase_starts = wide
        .iter()
        .filter(|i| {
            line[**i + 1]
                .unicode
                .chars()
                .next()
                .is_some_and(|ch| ch.is_lowercase())
        })
        .count();
    lowercase_starts * 2 > wide.len()
}

fn split_columns(line: Vec<&Glyph>) -> Vec<Vec<&Glyph>> {
    if line.len() < 2 {
        return vec![line];
    }
    let mut gaps = Vec::with_capacity(line.len() - 1);
    for pair in line.windows(2) {
        let right = pair[0].bbox[0].max(pair[0].bbox[2]);
        let left = pair[1].bbox[0].min(pair[1].bbox[2]);
        gaps.push(left - right);
    }
    let size = representative_size(&line);
    // Wider than a justified word space. A fixed 18pt floor misses a column
    // whose left line runs close to the right column (the remaining gutter is
    // ~17pt). Short lines still split only on the hard gap.
    let hard = (size * 2.0).max(24.0);
    let trigger = if line.len() >= 4 {
        let mut ordered = gaps.clone();
        ordered.sort_by(|a, b| a.total_cmp(b));
        let median = ordered[ordered.len() / 2].max(0.0);
        (median * 3.5).max(size * 1.25).min(hard)
    } else {
        hard
    };
    let mut ranked = gaps.clone();
    ranked.sort_by(|left, right| right.total_cmp(left));
    let largest = ranked.first().copied().unwrap_or(0.0);
    let second = ranked.get(1).copied().unwrap_or(0.0);
    let span = piece_width(&line);
    let header_labels = title_case_header_line(&line);
    let justified_spread = justified_spread_line(&line, &gaps, trigger, hard);
    let mut parts = Vec::new();
    let mut current = vec![line[0]];
    for (index, glyph) in line.iter().copied().enumerate().skip(1) {
        let gap = gaps[index - 1];
        // A detailed-contents bullet is a drawing, so the title and the next
        // entry look like a ~14pt gutter. The left piece is a narrow crumb
        // (`client 83`). A real column is much wider, and a short last line
        // that does not end in a page number still splits.
        let crumb = gap <= hard && narrow_page_crumb(&current);
        let prev_size = representative_size(&current);
        // A smaller line number just past the hyphen is inside the column
        // trigger. Leaving it here glues the digit to the word, and the line
        // is then read as a contents entry. A small index such as `(i)` has
        // body text tight on its right, so it stays in the sentence.
        // `LLMS` ends in a small-caps `S` a few points before the leader
        // dots. The size drop looks like a gutter digit, but the letter is
        // the last of the word. Splitting it leaves that `S` in the dots.
        let small_caps_letter = glyph.unicode.trim().chars().count() == 1
            && glyph
                .unicode
                .trim()
                .chars()
                .all(|ch| ch.is_ascii_alphabetic())
            && gap < prev_size * 1.25;
        let gutter_digit = glyph.font_size <= prev_size * 0.8
            && gap > prev_size * 0.5
            && trailing_gutter_mark(&line[index..], prev_size)
            && !small_caps_letter;
        let left_w = piece_width(&current);
        let right_w = piece_width(&line[index..]);
        let both_wide = left_w >= size * 12.0 && right_w >= size * 12.0;
        // A justified space before a few last words (`are used to`) exceeds
        // the median trigger. The tail is short and the head already fills
        // the column. A short left token (`Note`) still splits from the line
        // that follows it.
        // `racy.` is the next column's hyphen tail, 1.4pt off this baseline.
        // A justified last word shares the baseline. The offset is the gutter.
        let line_y = {
            let mut ys: Vec<f32> = current.iter().map(|item| item.matrix[5]).collect();
            ys.sort_by(|left, right| left.total_cmp(right));
            ys[ys.len() / 2]
        };
        let off_baseline = (glyph.matrix[5] - line_y).abs() > size * 0.1;
        let trailing_word =
            gap <= hard && right_w < size * 8.0 && left_w >= size * 12.0 && !off_baseline;
        let stray_column_fragment = off_baseline
            && left_w >= size * 12.0
            && right_w >= size * 0.8
            && gap >= size
            && gap + 0.05 >= largest;
        // `conversa-` then `data.` 1.4em later: a word broken at the line end
        // is not followed by another word of the same line. That tail is the
        // next column's last line.
        let hyphen_break = gap >= size * 1.0
            && left_w >= size * 12.0
            && !off_baseline
            && current.len() >= 2
            && current[current.len() - 1].unicode.trim() == "-"
            && current[current.len() - 2]
                .unicode
                .chars()
                .all(|ch| ch.is_alphabetic())
            && glyph.unicode.chars().all(|ch| ch.is_alphanumeric());
        // "Table 1." then a one-em space is the caption sentence, not the
        // other column. The hard gap still splits a label from a real column.
        let caption_label = gap_inside_caption(&current, gap, size);
        let column_gap = (gap > hard || gap > trigger)
            && !trailing_word
            && !caption_label
            && !(justified_spread && gap <= hard);
        // Two full columns on one baseline can leave only ~1.2em. Em boxes
        // overlap, so the median gap is 0 and the trigger rises to 1.25em,
        // just above that gutter. A line wider than one column, with the
        // gap standing above the next word space, is still two columns.
        // Character glyphs, not a handful of pre-grouped blocks. A subscript
        // in the middle of one prose line is three boxes and must stay one line.
        // `divide each by √d_k, and apply` leaves about one em where the
        // radical sits. That gap is the largest on a justified line, and it
        // is not the other column: the sentence continues in the text face.
        let inline_math_gap = continues_through_inline_math(&line, index, size);
        let tight_column = !inline_math_gap
            && line.len() >= 40
            && span >= size * 38.0
            && both_wide
            && gap + 0.05 >= largest
            && gap >= size * 1.0
            && (second <= 0.05 || gap >= second * 1.45);
        // A table header on the same baseline as the other column
        // (`Document layout` | `XML category`) is the second gap, under the
        // 1.25em trigger. Each cell is still wider than a short word tail.
        let cell_wide = left_w >= size * 4.0 && right_w >= size * 4.0;
        let word_median = {
            let mut ordinary: Vec<f32> = gaps.iter().copied().filter(|gap| *gap < size).collect();
            if ordinary.is_empty() {
                0.0
            } else {
                ordinary.sort_by(|left, right| left.total_cmp(right));
                ordinary[ordinary.len() / 2]
            }
        };
        let table_cell = line.len() >= 40
            && span >= size * 38.0
            && cell_wide
            && gap >= size * 1.15
            && (word_median <= 0.05 || gap >= word_median * 3.0)
            && !caption_label;
        // A short note in the other column (`where α is 32.`) can sit just
        // under the 24pt hard gap, and it is too narrow to be a full column.
        // Two ems, and clearly the largest gap, is still that gutter.
        let column_stub = line.len() >= 40
            && left_w >= size * 18.0
            && right_w >= size * 4.0
            && gap >= size * 2.0
            && gap + 0.05 >= largest
            && (second <= 0.05 || gap >= second * 2.0);
        // The last line of each column shares a baseline. The right piece is
        // `with 44,617 tables.`, `challenges:`, or the caption wrap
        // `columns.`, and the gutter is ~16pt, so it used to look like a
        // justified tail of the left column. One column is about 20em, so a
        // 24em span already includes the gutter.
        // `2.5 Data` sits 17.6pt past `self-attention`. A justified space on
        // that line is 10.9pt, so the gutter is not twice the next gap and
        // the heading was read as the tail of the left column.
        let two_column_tail = line.len() >= 40
            && span >= size * 24.0
            && left_w >= size * 16.0
            && right_w >= size * 3.0
            && gap >= size * 1.25
            && gap + 0.05 >= largest
            && (second <= 0.05
                || gap >= second * 2.0
                || (section_index_stub(&line[index..]) && gap >= second * 1.25));
        // `references column, {` The brace is the left edge of a listing in
        // the next column. A one-glyph tail used to look like a justified space.
        let lone_mark = gap >= size * 1.15
            && left_w >= size * 8.0
            && line[index..].iter().all(|glyph| {
                let token = glyph.unicode.trim();
                !token.is_empty()
                    && token
                        .chars()
                        .all(|ch| matches!(ch, '{' | '}' | '[' | ']' | '(' | ')'))
            });
        // `Trigger Optimization Context` sits in one header row. The gutters
        // are about an em, under the 24pt column split, so the three words
        // used to become one string `触发器优化上下文`.
        let header_word = header_labels
            && gap >= size * 0.55
            && glyph
                .unicode
                .trim()
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_uppercase())
            && current
                .iter()
                .any(|item| item.unicode.chars().any(|ch| ch.is_ascii_alphabetic()));
        if (column_gap
            || tight_column
            || table_cell
            || column_stub
            || two_column_tail
            || lone_mark
            || gutter_digit
            || stray_column_fragment
            || hyphen_break
            || header_word)
            && !current.is_empty()
            && !crumb
        {
            parts.push(std::mem::take(&mut current));
        }
        current.push(glyph);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// `Trigger Optimization Context`: two to six title-case words, no sentence
/// punctuation. A prose title (`Full-history baseline`) has a lowercase word
/// and stays one segment.
fn title_case_header_line(line: &[&Glyph]) -> bool {
    if line.len() < 4 || line.len() > 48 {
        return false;
    }
    let text: String = line.iter().map(|glyph| glyph.unicode.as_str()).collect();
    if text
        .chars()
        .any(|ch| matches!(ch, ',' | '.' | ':' | ';' | '。' | '，' | '?' | '!'))
    {
        return false;
    }
    let size = representative_size(line).max(1.0);
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    for (index, glyph) in line.iter().enumerate() {
        let spaced = glyph.unicode.chars().all(|ch| ch.is_whitespace());
        let gap = if index == 0 {
            0.0
        } else {
            let right = line[index - 1].bbox[0].max(line[index - 1].bbox[2]);
            let left = glyph.bbox[0].min(glyph.bbox[2]);
            left - right
        };
        if index > 0 && (spaced || gap >= size * 0.55) {
            let word = current.trim().to_string();
            if !word.is_empty() {
                words.push(word);
            }
            current.clear();
        }
        if !spaced {
            current.push_str(&glyph.unicode);
        }
    }
    let word = current.trim().to_string();
    if !word.is_empty() {
        words.push(word);
    }
    if !(2..=6).contains(&words.len()) {
        return false;
    }
    words.iter().all(|word| {
        word.chars()
            .find(|ch| ch.is_ascii_alphabetic())
            .is_some_and(|ch| ch.is_ascii_uppercase())
    })
}

/// The next body-sized glyph is another column, or nothing follows.
/// A parenthetical index has the sentence continuing immediately.
fn section_index_stub(rest: &[&Glyph]) -> bool {
    let text: String = rest.iter().map(|glyph| glyph.unicode.as_str()).collect();
    let text = text.trim();
    let bytes = text.as_bytes();
    if bytes.first().is_none_or(|byte| !byte.is_ascii_digit()) {
        return false;
    }
    let mut index = 0usize;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if !(1..=2).contains(&index) {
        return false;
    }
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
        let start = index;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
            index += 1;
        }
        if index == start {
            return false;
        }
    }
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    text[index..]
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_uppercase())
}

fn trailing_gutter_mark(rest: &[&Glyph], body_size: f32) -> bool {
    let Some(first) = rest.first() else {
        return false;
    };
    let mut prev_right = glyph_right(first);
    for glyph in rest.iter().skip(1) {
        let gap = glyph_left(glyph) - prev_right;
        if glyph.font_size > body_size * 0.8 {
            return gap > body_size * 0.8;
        }
        prev_right = prev_right.max(glyph_right(glyph));
    }
    true
}

/// Left side of a contents bullet: a short title ending in its page number.
/// Wider than this is a column, so a 17pt gutter still splits.
fn narrow_page_crumb(piece: &[&Glyph]) -> bool {
    if piece.len() < 2 || piece_width(piece) > 140.0 {
        return false;
    }
    piece_ends_with_page_number(piece)
}

/// A word space inside `Table 2: …` is not the other column. The hard gap
/// still splits a short label from a real column.
pub(crate) fn gap_inside_caption(left: &[&Glyph], gap: f32, size: f32) -> bool {
    if left.is_empty() {
        return false;
    }
    let hard = (size * 2.0).max(24.0);
    gap <= hard && piece_width(left) < size * 12.0 && caption_kind(&line_text(left)).is_some()
}

fn piece_width(piece: &[&Glyph]) -> f32 {
    let left = piece
        .iter()
        .map(|glyph| glyph.bbox[0].min(glyph.bbox[2]))
        .fold(f32::MAX, f32::min);
    let right = piece
        .iter()
        .map(|glyph| glyph.bbox[0].max(glyph.bbox[2]))
        .fold(left, f32::max);
    right - left
}

pub(crate) fn piece_ends_with_page_number(piece: &[&Glyph]) -> bool {
    let mut index = piece.len();
    let mut token = String::new();
    while index > 0 {
        let glyph = piece[index - 1];
        if index < piece.len() {
            let right = glyph.bbox[0].max(glyph.bbox[2]);
            let left = piece[index].bbox[0].min(piece[index].bbox[2]);
            let size = glyph.font_size.max(piece[index].font_size).max(1.0);
            if left - right > size * 0.18 {
                break;
            }
        }
        let chunk = glyph.unicode.trim();
        if chunk.is_empty() {
            if !token.is_empty() {
                break;
            }
            index -= 1;
            continue;
        }
        token.insert_str(0, chunk);
        index -= 1;
        if token.len() > 3 {
            return false;
        }
    }
    !token.is_empty()
        && token.len() <= 3
        && token.bytes().all(|byte| byte.is_ascii_digit())
        && index > 0
}

struct VisualLine<'a> {
    glyphs: Vec<&'a Glyph>,
    text: String,
    page: u32,
    y: f32,
    left: f32,
    right: f32,
    size: f32,
    font: String,
    /// A contents row. Its page number and leader dots stay in place, and it
    /// does not join the next row.
    toc: bool,
    /// No other title sits to the right on this baseline. A lowercase word
    /// under it is this title's wrap (`Q&A` / `engines`), not the next entry.
    sole_on_baseline: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RunFace {
    Bold,
    Italic,
}

fn run_face(name: &str) -> Option<RunFace> {
    let style = crate::font::face_style(name);
    // NimbusRom-Medi is LaTeX bold. It is not FaceStyle::bold, so a running
    // header in that face is not enlarged, but a run-in heading still splits.
    if style.bold || crate::font::medium_weight(name) {
        Some(RunFace::Bold)
    } else if style.italic {
        Some(RunFace::Italic)
    } else {
        None
    }
}

fn tail_starts_upper(text: &str) -> bool {
    text.trim_start()
        .chars()
        .find(|ch| ch.is_ascii_alphabetic())
        .is_some_and(|ch| ch.is_ascii_uppercase())
}

pub(crate) fn is_run_in_label(text: &str) -> bool {
    known_run_in(text)
}

fn known_run_in(label: &str) -> bool {
    let word = label
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|ch: char| !ch.is_ascii_alphabetic())
        .to_ascii_lowercase();
    matches!(
        word.as_str(),
        "proof"
            | "definition"
            | "theorem"
            | "lemma"
            | "proposition"
            | "corollary"
            | "remark"
            | "example"
            | "note"
            | "claim"
            | "observation"
            | "assumption"
    )
}

fn prose_letter(glyph: &Glyph) -> bool {
    glyph.unicode.chars().any(|ch| ch.is_ascii_alphabetic()) && !math_font_glyph(glyph)
}

fn letter_count(glyph: &Glyph) -> usize {
    glyph
        .unicode
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .count()
}

/// `SIAM Publications, 1993.` names the year in the first few words. A year
/// later in the line (`Qwen Team, 2026` or `Lloyd, 1982`) is part of the
/// sentence that follows a run-in heading.
fn year_in_prefix(text: &str, chars: usize) -> bool {
    let prefix: String = text.chars().take(chars).collect();
    has_year(&prefix)
}

fn has_year(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 4 {
        return false;
    }
    for index in 0..=chars.len() - 4 {
        let window = &chars[index..index + 4];
        if !window.iter().all(|ch| ch.is_ascii_digit()) {
            continue;
        }
        let before = index > 0 && chars[index - 1].is_ascii_digit();
        let after = index + 4 < chars.len() && chars[index + 4].is_ascii_digit();
        if before || after {
            continue;
        }
        let year: String = window.iter().collect();
        if year.starts_with("19") || year.starts_with("20") {
            return true;
        }
    }
    false
}

fn label_terminator(label: &str) -> Option<LabelEnd> {
    let trimmed = label.trim_end();
    if trimmed.ends_with('→') || trimmed.ends_with('➔') || trimmed.ends_with("->") {
        return Some(LabelEnd::Arrow);
    }
    if trimmed.ends_with(':') {
        return Some(LabelEnd::Colon);
    }
    if trimmed.ends_with('.') || trimmed.ends_with('?') {
        return Some(LabelEnd::Period);
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LabelEnd {
    Period,
    Colon,
    Arrow,
}

/// Most letters of `glyphs` are in an italic face.
fn italic_majority(glyphs: &[&Glyph]) -> bool {
    let letters: Vec<&&Glyph> = glyphs.iter().filter(|glyph| prose_letter(glyph)).collect();
    let italic = letters
        .iter()
        .filter(|glyph| crate::font::face_style(&glyph.font_name).italic)
        .count();
    !letters.is_empty() && italic * 2 > letters.len()
}

/// A styled label at the start of a line (`Definition 1.1.`, `Proof.`,
/// `Ratings →`) and the sentence that follows it. Punctuation and a math
/// letter inside the label do not end the run. The label keeps its own face
/// so the body does not become heiti or italic.
fn styled_run_in<'a>(line: &VisualLine<'a>) -> Option<(Vec<&'a Glyph>, Vec<&'a Glyph>)> {
    let mut ordered = line.glyphs.clone();
    ordered.sort_by(|left, right| glyph_left(left).total_cmp(&glyph_left(right)));
    let kind = ordered.iter().find_map(|glyph| {
        if prose_letter(glyph) {
            run_face(&glyph.font_name)
        } else {
            None
        }
    })?;
    let mut split = 0;
    let mut label_letters = 0usize;
    while split < ordered.len() {
        let glyph = ordered[split];
        if prose_letter(glyph) {
            if run_face(&glyph.font_name) != Some(kind) {
                break;
            }
            label_letters += letter_count(glyph);
        }
        split += 1;
    }
    if !(3..160).contains(&label_letters) || split == ordered.len() {
        return None;
    }
    let label = line_text(&ordered[..split]);
    let tail = line_text(&ordered[split..]);
    let tail_letters = tail.chars().filter(|ch| ch.is_ascii_alphabetic()).count();
    // An italic title plus `SIAM Publications, 1993.` is one citation. A bold
    // run-in still splits when the sentence names a year (`K-Means: The
    // K-means problem (Lloyd, 1982) is`).
    if tail_letters < 8 || (kind == RunFace::Italic && year_in_prefix(&tail, 26)) {
        return None;
    }
    let end = label_terminator(&label)?;
    // "Smith et al. showed" is a citation, not a label. "Proof. Let" is.
    if kind == RunFace::Italic
        && end == LabelEnd::Period
        && !tail_starts_upper(&tail)
        && !known_run_in(&label)
    {
        return None;
    }
    Some((ordered[..split].to_vec(), ordered[split..].to_vec()))
}

/// The whole first line is one bold or italic sentence (`Obstacle 1: … .`).
/// The following line is a different face, so the sentence must not set the
/// body face.
fn styled_lead_line(line: &VisualLine<'_>) -> Option<RunFace> {
    let mut face: Option<RunFace> = None;
    let mut letters = 0usize;
    for glyph in &line.glyphs {
        if !prose_letter(glyph) {
            continue;
        }
        let kind = run_face(&glyph.font_name)?;
        match face {
            None => face = Some(kind),
            Some(prev) if prev != kind => return None,
            Some(_) => {}
        }
        letters += letter_count(glyph);
    }
    if !(3..160).contains(&letters) || label_terminator(&line.text).is_none() {
        return None;
    }
    face
}

fn majority_run(line: &VisualLine<'_>) -> Option<RunFace> {
    let mut bold = 0usize;
    let mut italic = 0usize;
    let mut roman = 0usize;
    for glyph in &line.glyphs {
        if !prose_letter(glyph) {
            continue;
        }
        let n = letter_count(glyph);
        match run_face(&glyph.font_name) {
            Some(RunFace::Bold) => bold += n,
            Some(RunFace::Italic) => italic += n,
            None => roman += n,
        }
    }
    if bold > italic && bold > roman {
        Some(RunFace::Bold)
    } else if italic > bold && italic > roman {
        Some(RunFace::Italic)
    } else {
        None
    }
}

/// A scanned pamphlet paints a column of rules (`§`, `|`, `1`) in the margin
/// on the same baseline as the text. OCR glues a letter onto the rule
/// (`j§`, `jj|`, `||`) and sometimes leaves a stray `S` in the same column.
/// The gap to the sentence is wider than a word space, and the same x repeats
/// down the page. Those marks are not part of the sentence.
fn detach_border_rules(lines: &mut Vec<VisualLine<'_>>) -> Vec<(u32, String)> {
    struct Edge {
        id: u32,
        page: u32,
        x: f32,
        y: f32,
        /// A rule character. A stray letter is dropped only when it sits on
        /// a column that already has rules.
        rule: bool,
    }
    let mut edges = Vec::new();
    for line in lines.iter() {
        let mut glyphs = line.glyphs.clone();
        glyphs.sort_by(|left, right| left.matrix[4].total_cmp(&right.matrix[4]));
        let size = line.size.max(1.0);
        let content: Vec<&&Glyph> = glyphs
            .iter()
            .filter(|glyph| !glyph.unicode.trim().is_empty())
            .collect();
        if content.is_empty() {
            continue;
        }
        // A column split leaves the rule on a line of its own (`||`, `S`).
        if content.len() == 1 {
            let glyph = content[0];
            let piece = glyph.unicode.trim().chars().count();
            if (1..=2).contains(&piece) {
                edges.push(Edge {
                    id: glyph.id,
                    page: line.page,
                    x: glyph.matrix[4],
                    y: line.y,
                    rule: border_token(&glyph.unicode),
                });
            }
            continue;
        }
        if content.len() <= 4
            && content.iter().all(|glyph| ornament_piece(&glyph.unicode))
            && content.iter().any(|glyph| border_token(&glyph.unicode))
        {
            let x = content
                .iter()
                .map(|glyph| glyph.matrix[4])
                .fold(f32::MAX, f32::min);
            for glyph in &content {
                edges.push(Edge {
                    id: glyph.id,
                    page: line.page,
                    x,
                    y: line.y,
                    rule: true,
                });
            }
            continue;
        }
        for left_end in [true, false] {
            let Some(run) = margin_run(&content, left_end, size) else {
                continue;
            };
            let rule = run.iter().any(|&slot| border_token(&content[slot].unicode));
            let anchor = run[run.len() - 1];
            for slot in run {
                edges.push(Edge {
                    id: content[slot].id,
                    page: line.page,
                    x: content[anchor].matrix[4],
                    y: line.y,
                    rule,
                });
            }
        }
    }
    let mut drop = crate::detmap::HashSet::default();
    let mut columns: Vec<(u32, f32)> = Vec::new();
    let mut used = vec![false; edges.len()];
    for index in 0..edges.len() {
        if used[index] || !edges[index].rule {
            continue;
        }
        let mut cluster = vec![index];
        for other in index + 1..edges.len() {
            if edges[other].rule
                && edges[other].page == edges[index].page
                && (edges[other].x - edges[index].x).abs() < 4.0
            {
                cluster.push(other);
            }
        }
        let mut ys: Vec<i32> = cluster
            .iter()
            .map(|&slot| (edges[slot].y * 2.0) as i32)
            .collect();
        ys.sort_unstable();
        ys.dedup();
        if ys.len() >= 4 {
            columns.push((edges[index].page, edges[index].x));
            for slot in cluster {
                used[slot] = true;
                drop.insert(edges[slot].id);
            }
        }
    }
    for edge in &edges {
        if edge.rule {
            continue;
        }
        if columns
            .iter()
            .any(|(page, x)| *page == edge.page && (edge.x - x).abs() < 4.0)
        {
            drop.insert(edge.id);
        }
    }
    drop_glyphs(lines, drop)
}

/// Glyphs of one tight ornament, from the outer edge inward. The last index
/// is the glyph nearest the sentence.
fn margin_run(content: &[&&Glyph], left_end: bool, size: f32) -> Option<Vec<usize>> {
    if content.len() < 2 {
        return None;
    }
    let mut index = if left_end { 0 } else { content.len() - 1 };
    let step: isize = if left_end { 1 } else { -1 };
    let mut run = vec![index];
    while run.len() < 4 {
        let next = index as isize + step;
        if next < 0 || next >= content.len() as isize {
            break;
        }
        let next_i = next as usize;
        let gap = if left_end {
            glyph_span_gap(content[index], content[next_i])
        } else {
            glyph_span_gap(content[next_i], content[index])
        };
        if gap > size * 0.45 || !ornament_piece(&content[next_i].unicode) {
            break;
        }
        run.push(next_i);
        index = next_i;
    }
    let body = index as isize + step;
    if body < 0 || body >= content.len() as isize {
        return None;
    }
    let body_i = body as usize;
    let gap = if left_end {
        glyph_span_gap(content[index], content[body_i])
    } else {
        glyph_span_gap(content[body_i], content[index])
    };
    if gap < size * 0.9 {
        return None;
    }
    let rule = run.iter().any(|&slot| border_token(&content[slot].unicode));
    // A lone letter does not open a column. It is claimed later when a rule
    // stack already occupies this x (`S` beside `|`).
    if !rule && run.len() != 1 {
        return None;
    }
    Some(run)
}

fn glyph_span_gap(left: &Glyph, right: &Glyph) -> f32 {
    right.bbox[0].min(right.bbox[2]) - left.bbox[0].max(left.bbox[2])
}

/// OCR of a printed rule: a long string of `l` and `|` with no spaces.
fn detach_rule_noise(lines: &mut Vec<VisualLine<'_>>) -> Vec<(u32, String)> {
    let mut drop = crate::detmap::HashSet::default();
    for line in lines.iter() {
        if !ocr_rule_noise(&line.text) {
            continue;
        }
        for glyph in &line.glyphs {
            drop.insert(glyph.id);
        }
    }
    drop_glyphs(lines, drop)
}

fn ocr_rule_noise(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().any(|ch| ch.is_whitespace()) {
        return false;
    }
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() < 24 {
        return false;
    }
    let marks = chars
        .iter()
        .filter(|ch| {
            matches!(
                ch,
                'l' | 'I' | 'i' | '|' | '1' | '!' | 'f' | 'F' | '[' | ']' | '^'
            )
        })
        .count();
    marks * 5 >= chars.len() * 4
}

/// A page of OCR with no sentence and several garbage tokens. `TOILETTE`
/// and `PEFARTMENT` look like words; `POBUtfttCO` and `3jOV£9rOIit` do not.
/// Two of those, and no sentence, means the page is the scan image.
/// A born-digital page can embed a screenshot as a form figure that carries
/// its own invisible text layer (`Goal prompt for GuideEvolve`). The pixels
/// stay English, so painting that layer in Chinese stacked two texts on the
/// figure. A scanned page has no visible text of its own and keeps its layer.
fn detach_figure_text_layers(lines: &mut Vec<VisualLine<'_>>) -> Vec<(u32, String)> {
    let mut visible: crate::detmap::HashMap<u32, usize> = crate::detmap::HashMap::default();
    for line in lines.iter() {
        let count = line
            .glyphs
            .iter()
            .filter(|glyph| !glyph.invisible && !glyph.unicode.trim().is_empty())
            .count();
        *visible.entry(line.page).or_default() += count;
    }
    let mut drop = crate::detmap::HashSet::default();
    for line in lines.iter() {
        if visible.get(&line.page).copied().unwrap_or(0) < 20 {
            continue;
        }
        for glyph in &line.glyphs {
            if glyph.invisible && matches!(glyph.source.kind, crate::glyph::SourceKind::FormXObject)
            {
                drop.insert(glyph.id);
            }
        }
    }
    drop_glyphs_as(lines, drop, "figure")
}

fn detach_junk_ocr_pages(lines: &mut Vec<VisualLine<'_>>) -> Vec<(u32, String)> {
    let mut pages: Vec<u32> = lines.iter().map(|line| line.page).collect();
    pages.sort_unstable();
    pages.dedup();
    let mut drop = crate::detmap::HashSet::default();
    for page in pages {
        let page_lines: Vec<&VisualLine> = lines.iter().filter(|line| line.page == page).collect();
        if page_lines.is_empty() || page_has_prose(&page_lines) {
            continue;
        }
        let junk: usize = page_lines
            .iter()
            .map(|line| junk_token_count(&line.text))
            .sum();
        if junk < 2 {
            continue;
        }
        for line in page_lines {
            for glyph in &line.glyphs {
                drop.insert(glyph.id);
            }
        }
    }
    drop_glyphs_as(lines, drop, "ocr")
}

/// Two centered lines of a comma-separated list. A left-aligned sentence
/// that ends with a comma does not share this midpoint test: its next line
/// starts at the same left edge, which the ordinary join already accepts.
fn centered_comma_continuation(upper: &VisualLine<'_>, lower: &VisualLine<'_>) -> bool {
    if upper.page != lower.page {
        return false;
    }
    let size = upper.size.max(lower.size).max(1.0);
    if (upper.size - lower.size).abs() > size * 0.2 {
        return false;
    }
    let upper_mid = (upper.left + upper.right) * 0.5;
    let lower_mid = (lower.left + lower.right) * 0.5;
    if (upper_mid - lower_mid).abs() > size * 3.0 {
        return false;
    }
    let upper_text = upper.text.trim();
    let lower_text = lower.text.trim();
    // A left-aligned address, heading, or sentence shares a left edge. A
    // centered list does not: each line is centered, so the edges wander.
    if (upper.left - lower.left).abs() <= size * 1.2 {
        return false;
    }
    // A full body line shares the page midpoint with a short centered
    // equation (`where head_i = Attention(...)`). The edges differ, so the
    // comma test used to claim the sentence under that equation.
    let upper_w = upper.right - upper.left;
    let lower_w = lower.right - lower.left;
    if lower_w > upper_w * 1.45 && lower.left + size * 2.0 < upper.left {
        return false;
    }
    if upper_text.ends_with('.') || upper_text.ends_with('?') || upper_text.ends_with('!') {
        return false;
    }
    let commas = upper_text.matches(',').count();
    commas >= 2
        && upper_text.split_whitespace().count() >= 3
        && lower_text.chars().any(|ch| ch.is_ascii_alphabetic())
}

/// The paragraph is already a centered comma list, and this line sits on the
/// same axis. Two lines of similar length have left edges within an em, which
/// is also how a first-line indent looks, so the ordinary join drops the line.
/// The lines already in the paragraph wander, and their midpoints do not.
fn centered_list_keeps(current: &[VisualLine<'_>], lower: &VisualLine<'_>) -> bool {
    if !centered_comma_block(current) {
        return false;
    }
    let Some(upper) = current.last() else {
        return false;
    };
    if upper.page != lower.page {
        return false;
    }
    let size = upper.size.max(lower.size).max(1.0);
    if (upper.size - lower.size).abs() > size * 0.2 {
        return false;
    }
    let dy = upper.y - lower.y;
    if dy < size * 0.7 || dy > size * 1.5 {
        return false;
    }
    let upper_text = upper.text.trim_end();
    if upper_text.ends_with('.') || upper_text.ends_with('?') || upper_text.ends_with('!') {
        return false;
    }
    let axis = centered_block_axis(current);
    let lower_mid = (lower.left + lower.right) * 0.5;
    (lower_mid - axis).abs() <= size * 2.5 && lower.text.chars().any(|ch| ch.is_ascii_alphabetic())
}

/// Midpoints agree and left edges wander. A left-aligned paragraph fails the
/// left-edge test, so an indented sentence is not pulled in.
fn centered_comma_block(lines: &[VisualLine<'_>]) -> bool {
    if lines.len() < 2 {
        return false;
    }
    let commas: usize = lines
        .iter()
        .map(|line| line.text.matches(',').count())
        .sum();
    if commas < 4 {
        return false;
    }
    let size = lines
        .iter()
        .map(|line| line.size)
        .fold(1.0f32, f32::max)
        .max(1.0);
    let mut left_min = f32::MAX;
    let mut left_max = f32::MIN;
    let mut mid_min = f32::MAX;
    let mut mid_max = f32::MIN;
    let mut right_min = f32::MAX;
    let mut right_max = f32::MIN;
    for line in lines {
        left_min = left_min.min(line.left);
        left_max = left_max.max(line.left);
        right_min = right_min.min(line.right);
        right_max = right_max.max(line.right);
        let mid = (line.left + line.right) * 0.5;
        mid_min = mid_min.min(mid);
        mid_max = mid_max.max(mid);
    }
    // A first-line indent moves only the opening line. The rest of a justified
    // paragraph shares the left margin. A centered list keeps moving that edge.
    if lines.len() >= 3 {
        let mut body_min = f32::MAX;
        let mut body_max = f32::MIN;
        for line in lines.iter().skip(1) {
            body_min = body_min.min(line.left);
            body_max = body_max.max(line.left);
        }
        if body_max - body_min <= size * 0.45 {
            return false;
        }
    }
    // Two justified lines share the right edge. The indent is about an em, so
    // the left edges differ by less than the wander of a centered list.
    if right_max - right_min <= size * 0.6 && left_max - left_min <= size * 2.5 {
        return false;
    }
    left_max - left_min > size * 1.2 && mid_max - mid_min <= size * 3.0
}

fn centered_block_axis(lines: &[VisualLine<'_>]) -> f32 {
    let mut mids: Vec<f32> = lines
        .iter()
        .map(|line| (line.left + line.right) * 0.5)
        .collect();
    mids.sort_by(|left, right| left.total_cmp(right));
    mids[mids.len() / 2]
}

/// A short centered tail under a wide centered sentence (`scholarly works.`
/// under `…journalistic or`). The left edges do not match, so the ordinary
/// margin join starts a new paragraph and the translation keeps the source
/// line break.
fn centered_prose_continuation(upper: &VisualLine<'_>, lower: &VisualLine<'_>) -> bool {
    if upper.page != lower.page {
        return false;
    }
    let size = upper.size.max(lower.size).max(1.0);
    if (upper.size - lower.size).abs() > size * 0.25 {
        return false;
    }
    let dy = upper.y - lower.y;
    if dy < size * 0.7 || dy > size * 1.6 {
        return false;
    }
    let upper_mid = (upper.left + upper.right) * 0.5;
    let lower_mid = (lower.left + lower.right) * 0.5;
    if (upper_mid - lower_mid).abs() > size * 2.0 {
        return false;
    }
    // A wrapped body line already shares the margin. This path is the tail
    // whose edges wander because the line is centered.
    if (upper.left - lower.left).abs() <= size * 1.5
        && (upper.right - lower.right).abs() <= size * 1.5
    {
        return false;
    }
    let upper_text = upper.text.trim();
    let lower_text = lower.text.trim();
    if upper_text.ends_with(['.', '?', '!', '。', '？', '！']) {
        return false;
    }
    if crate::translate::has_email_address(upper_text)
        || crate::translate::has_email_address(lower_text)
    {
        return false;
    }
    if crate::translate::is_institution_line(upper_text)
        || crate::translate::is_institution_line(lower_text)
    {
        return false;
    }
    let upper_width = upper.right - upper.left;
    let lower_width = lower.right - lower.left;
    if upper_width < size * 18.0 || lower_width > upper_width * 0.72 {
        return false;
    }
    // `scholarly` continues the sentence. A title's second line is capitalized.
    lower_text.starts_with(|ch: char| ch.is_ascii_lowercase())
        && lower_text.chars().any(|ch| ch.is_ascii_alphabetic())
}

fn centered_prose_keeps(current: &[VisualLine<'_>], lower: &VisualLine<'_>) -> bool {
    current
        .last()
        .is_some_and(|upper| centered_prose_continuation(upper, lower))
}

/// Per-block OCR score. A low-quality block is not translated and not
/// painted. A title page whose scan is only partly read (a real title plus
/// `3jOV£9rOIit`) keeps the photograph: every text block on that page goes.
fn detach_low_quality_ocr(
    lines: &mut Vec<VisualLine<'_>>,
    regions: &[PaintedRegion],
    pages: &[PageInfo],
) -> Vec<(u32, String)> {
    let mut pages_present: Vec<u32> = lines.iter().map(|line| line.page).collect();
    pages_present.sort_unstable();
    pages_present.dedup();
    let mut drop = crate::detmap::HashSet::default();
    for page in pages_present {
        let page_lines: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.page == page)
            .map(|(index, _)| index)
            .collect();
        if page_lines.is_empty() {
            continue;
        }
        let low: Vec<usize> = page_lines
            .iter()
            .copied()
            .filter(|&index| {
                let body = page_body_size(lines, lines[index].page);
                // `d_model×d_k` is a superscript of the sentence under it.
                // The multiply sign makes the line look like scan garbage.
                ocr_line_low_quality(&lines[index].text)
                    && !is_extended_formula_script(&lines[index], body)
            })
            .collect();
        if low.is_empty() {
            continue;
        }
        let drop_page = title_scan_with_partial_ocr(page, &page_lines, &low, lines, regions, pages);
        let victims: &[usize] = if drop_page { &page_lines } else { &low };
        for index in victims {
            for glyph in &lines[*index].glyphs {
                drop.insert(glyph.id);
            }
        }
    }
    drop_glyphs_as(lines, drop, "ocr")
}

fn title_scan_with_partial_ocr(
    page: u32,
    page_lines: &[usize],
    low: &[usize],
    lines: &[VisualLine<'_>],
    regions: &[PaintedRegion],
    pages: &[PageInfo],
) -> bool {
    if low.is_empty() || page_lines.len() > 24 {
        return false;
    }
    let prose = page_lines
        .iter()
        .filter(|&&index| {
            !ocr_line_low_quality(&lines[index].text) && prose_ocr_sentence(&lines[index].text)
        })
        .count();
    if prose >= 4 {
        return false;
    }
    let Some(info) = pages.iter().find(|info| info.index == page) else {
        return false;
    };
    let page_area = (info.media_box[2] - info.media_box[0]).abs()
        * (info.media_box[3] - info.media_box[1]).abs();
    if page_area < 1.0 {
        return false;
    }
    regions.iter().any(|region| {
        if region.page_index != page || region.kind != "image" {
            return false;
        }
        let width = (region.bbox[2] - region.bbox[0]).abs();
        let height = (region.bbox[3] - region.bbox[1]).abs();
        width * height >= page_area * 0.45
    })
}

/// Dictionary-word ratio and garbage-character ratio for one OCR block.
/// `The ideas are expressed` is readable. `3jOV£9rOIit` and `POBUtfttCO` are not.
fn ocr_line_low_quality(text: &str) -> bool {
    if text.contains("://") || text.contains('@') || text.contains("www.") {
        return false;
    }
    // A born-digital formula (`µ(G) = min`, `−d(x,y)`, `1≤i`) is not a scan
    // misread. Dropping it left the glyphs in place while the sentence
    // around them was translated on top.
    if formula_line(text) {
        return false;
    }
    let tokens: Vec<&str> = text
        .split_whitespace()
        .filter(|token| token.chars().any(|ch| ch.is_alphanumeric()))
        .collect();
    if tokens.is_empty() {
        return false;
    }
    let mut chars = 0usize;
    let mut garbage = 0usize;
    for ch in text.chars() {
        if ch.is_whitespace() {
            continue;
        }
        chars += 1;
        // A slide bullet (`●FOSDEM`, `○IPv6`) is one glyph in the word. It is
        // not a misread the way `£` inside `3jOV£9rOIit` is. Counting it made
        // a 7-character line fail the garbage ratio and dropped the page.
        if junk_ocr_symbol(ch) && !list_mark(ch) {
            garbage += 1;
        }
    }
    if chars >= 6 && garbage * 8 >= chars {
        return true;
    }
    let junk = tokens
        .iter()
        .filter(|token| ocr_garbage_token(token))
        .count();
    let dictionary = tokens
        .iter()
        .filter(|token| dictionary_token(token))
        .count();
    let ratio = dictionary as f32 / tokens.len() as f32;
    if tokens.len() == 1 && junk == 1 && tokens[0].chars().count() >= 4 {
        return true;
    }
    tokens.len() >= 2 && junk > 0 && ratio < 0.5
}

/// `3jOV£9rOIit` and `POBUtfttCO` are misreads. `PromptTemplate` and a URL
/// are not: a camel-case hump is a real token, not a consonant smear.
/// A junk symbol or a digit glued into a non-word (`3j`, `9r`) wins over
/// that exemption. `log2` and `O(log2` stay: the letters are a plain word.
fn ocr_garbage_token(token: &str) -> bool {
    let token = strip_leading_mark(token);
    if token.is_empty() {
        return false;
    }
    if version_token(token) {
        return false;
    }
    let chars: Vec<char> = token.chars().collect();
    let letters: Vec<char> = chars
        .iter()
        .copied()
        .filter(|ch| ch.is_ascii_alphabetic())
        .collect();
    if chars.iter().copied().any(junk_ocr_symbol) {
        return true;
    }
    // `GKP24`, `BBN10`, `log2`, and `3z` are citations and formulas: one
    // capitalisation plus a digit. `3jOV9rOIit` is not that shape.
    // `junk_ocr_token` would flag every digit-letter join, including these.
    if digit_touches_letter(&chars) {
        return !plain_word_case(&letters);
    }
    if camel_identifier(&letters) {
        return false;
    }
    junk_ocr_token(token)
}

/// `Prompt|Template` has a capital followed by a lowercase hump that contains
/// a vowel. `Iit` inside `3jOV£9rOIit` is only two letters, and `POBUtfttCO`
/// has a lowercase smear (`tftt`) with no vowel.
fn camel_identifier(letters: &[char]) -> bool {
    if letters.len() < 4 {
        return false;
    }
    let vowel = |ch: char| matches!(ch.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u');
    let mut index = 0;
    while index < letters.len() {
        if letters[index].is_ascii_uppercase() {
            let start = index + 1;
            let mut end = start;
            while end < letters.len() && letters[end].is_ascii_lowercase() {
                end += 1;
            }
            if end - start >= 3 && letters[start..end].iter().any(|ch| vowel(*ch)) {
                return true;
            }
            index = end.max(index + 1);
            continue;
        }
        index += 1;
    }
    false
}

fn dictionary_token(token: &str) -> bool {
    if junk_ocr_token(token) {
        return false;
    }
    let bare: String = token
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .collect();
    if bare.len() < 2 {
        return false;
    }
    let lower = bare.to_ascii_lowercase();
    if COMMON_WORDS.contains(&lower.as_str()) {
        return true;
    }
    let chars: Vec<char> = bare.chars().collect();
    chars
        .iter()
        .any(|ch| matches!(ch.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u' | 'y'))
        && consonant_run(&chars) < 5
}

const COMMON_WORDS: &[&str] = &[
    "the", "and", "are", "is", "was", "were", "this", "that", "with", "for", "from", "using",
    "may", "can", "not", "also", "their", "these", "those", "of", "in", "on", "to", "a", "an",
    "or", "as", "at", "by", "be", "it", "its", "we", "our", "you", "your", "which", "when",
    "where", "what", "who", "how", "but", "if", "then", "than", "into", "over", "under", "about",
    "between", "after", "before", "each", "other", "such", "only", "more", "most", "some", "any",
    "all", "no", "yes", "has", "have", "had", "been", "being", "will", "would", "should", "could",
];

fn page_has_prose(lines: &[&VisualLine<'_>]) -> bool {
    let joined = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    prose_ocr_sentence(&joined)
}

fn prose_ocr_sentence(text: &str) -> bool {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 6 {
        return false;
    }
    const FUNCTION: &[&str] = &[
        "the", "and", "are", "is", "was", "were", "this", "that", "with", "for", "from", "using",
        "may", "can", "not", "also", "their", "these", "those", "of", "in", "on", "to", "a", "an",
    ];
    words.iter().any(|word| {
        let bare: String = word
            .chars()
            .filter(|ch| ch.is_ascii_alphabetic())
            .collect::<String>()
            .to_ascii_lowercase();
        FUNCTION.contains(&bare.as_str())
    })
}

fn junk_token_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|token| junk_ocr_token(token))
        .count()
}

/// A token OCR did not read as a word. A normal English word, including
/// `LaFollette` and `Constraints`, is not junk.
fn junk_ocr_token(token: &str) -> bool {
    let token = strip_leading_mark(token);
    if token.is_empty() || version_token(token) {
        return false;
    }
    let chars: Vec<char> = token.chars().filter(|ch| !ch.is_whitespace()).collect();
    if chars.len() < 3 {
        return false;
    }
    if chars.iter().copied().any(junk_ocr_symbol) {
        return true;
    }
    if digit_touches_letter(&chars) {
        return true;
    }
    consonant_run(&chars) >= 4 && !plain_word_case(&chars)
}

/// Greek, a micro sign, or a math operator. Scan garbage uses `£`, not `µ`.
/// The operator itself (`−`, `≤`) is not a junk symbol.
fn formula_line(text: &str) -> bool {
    let math = text.chars().any(math_formula_char);
    // A space is not scan garbage. `and W ∈ R` is still that formula.
    let junk = text
        .chars()
        .any(|ch| !ch.is_whitespace() && junk_ocr_symbol(ch) && !math_formula_char(ch));
    math && !junk
}

fn math_formula_char(ch: char) -> bool {
    ch == 'µ'
        || ch == 'μ'
        || ch == '×'
        || ch == '÷'
        // Angle brackets in ⟨x, y⟩ are math, not a scan misread. Treating
        // them as junk dropped `A × B = {⟨x,y⟩ : x ∈ A and y ∈ B}` and the
        // Let/Since line as OCR, so they never became a segment.
        || ch == '⟨'
        || ch == '⟩'
        || ch == '⟦'
        || ch == '⟧'
        || ch == '〈'
        || ch == '〉'
        || ch == '\u{0338}'
        || ('\u{0370}'..='\u{03FF}').contains(&ch)
        || ('\u{2200}'..='\u{22FF}').contains(&ch)
        || ('\u{2070}'..='\u{209F}').contains(&ch)
}

fn junk_ocr_symbol(ch: char) -> bool {
    if ch.is_alphanumeric() {
        return false;
    }
    // Brackets, CJK stops, and `*` are ordinary text. `£` inside `3jOV£9rOIit`
    // is a misread and is not in this set.
    !".,;:!?\"'`’“”‘—–-()[]{}<>/\\+*=&@#%^_|~…·•、。《》「」『』【】（），：；！？".contains(ch)
}

/// `●All` and `○IPv6-only` keep the bullet in the same glyph run. The
/// bullet is not a misread; the word after it is what the scorer judges.
fn list_mark(ch: char) -> bool {
    matches!(
        ch,
        '●' | '○'
            | '•'
            | '◦'
            | '▪'
            | '▫'
            | '▸'
            | '►'
            | '‣'
            | '∙'
            | '◉'
            | '◎'
            | '◆'
            | '◇'
            | '■'
            | '★'
            | '☆'
            | '·'
    )
}

fn strip_leading_mark(token: &str) -> &str {
    token.trim_start_matches(list_mark)
}

/// `IPv4`, `IPv6`, and `IPv6-only`. The `v` is lowercase, so the token is
/// not an all-caps citation, but it is still a real word.
fn version_token(token: &str) -> bool {
    let letters: String = token
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .collect();
    if letters.len() < 3 || letters.len() > 12 || !token.chars().any(|ch| ch.is_ascii_digit()) {
        return false;
    }
    let lower = letters.to_ascii_lowercase();
    lower == "ipv" || lower == "ipvonly"
}

fn digit_touches_letter(chars: &[char]) -> bool {
    chars.windows(2).any(|pair| {
        (pair[0].is_ascii_digit() && pair[1].is_ascii_alphabetic())
            || (pair[1].is_ascii_digit() && pair[0].is_ascii_alphabetic())
    })
}

fn consonant_run(chars: &[char]) -> usize {
    let mut best = 0usize;
    let mut run = 0usize;
    for ch in chars {
        if ch.is_ascii_alphabetic()
            && !matches!(ch.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u')
        {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

/// All lower, all upper, or one leading capital. `POBUtfttCO` and `TttH` are
/// neither. `LaFollette` is not this shape either, and it has no consonant run.
fn plain_word_case(chars: &[char]) -> bool {
    let letters: Vec<char> = chars
        .iter()
        .copied()
        .filter(|ch| ch.is_ascii_alphabetic())
        .collect();
    if letters.is_empty() {
        return true;
    }
    if letters.iter().all(|ch| ch.is_ascii_lowercase())
        || letters.iter().all(|ch| ch.is_ascii_uppercase())
    {
        return true;
    }
    letters[0].is_ascii_uppercase() && letters[1..].iter().all(|ch| ch.is_ascii_lowercase())
}

fn drop_glyphs(
    lines: &mut Vec<VisualLine<'_>>,
    drop: crate::detmap::HashSet<u32>,
) -> Vec<(u32, String)> {
    drop_glyphs_as(lines, drop, "margin")
}

fn drop_glyphs_as(
    lines: &mut Vec<VisualLine<'_>>,
    drop: crate::detmap::HashSet<u32>,
    reason: &str,
) -> Vec<(u32, String)> {
    if drop.is_empty() {
        return Vec::new();
    }
    let mut kept = Vec::new();
    let mut rebuilt = Vec::new();
    for line in lines.drain(..) {
        let mut body = Vec::new();
        for glyph in line.glyphs {
            if drop.contains(&glyph.id) {
                kept.push((glyph.id, reason.into()));
            } else {
                body.push(glyph);
            }
        }
        if !body.is_empty() {
            rebuilt.push(visual_line(body));
        }
    }
    *lines = rebuilt;
    kept
}

fn border_token(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty()
        && trimmed.chars().count() <= 2
        && trimmed.chars().all(|ch| {
            matches!(
                ch,
                '|' | '¦' | '§' | '│' | '┃' | '║' | '‖' | '1' | 'I' | '='
            ) || ('\u{2500}'..='\u{257F}').contains(&ch)
        })
}

/// A glyph OCR stuck onto a rule (`j` in `j§`, `S` in the same column).
fn ornament_piece(text: &str) -> bool {
    if border_token(text) {
        return true;
    }
    let trimmed = text.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    (1..=2).contains(&chars.len())
        && chars
            .iter()
            .all(|ch| matches!(ch, 'j' | 'J' | 'S' | 's' | 'l' | 'i' | 'f' | 'F' | '^'))
}

fn assemble(
    raw: Vec<Vec<&Glyph>>,
    flags: &SegmentFlags,
    regions: &[PaintedRegion],
    pages: &[PageInfo],
) -> Segmentation {
    // A rotated margin footer shares a baseline with the sentence beside it.
    // It stays original, and it is not part of that sentence.
    let mut kept = Vec::new();
    let mut lines: Vec<VisualLine> = Vec::new();
    for glyphs in raw {
        let mut body = Vec::new();
        for glyph in glyphs {
            if glyph.sideways() {
                kept.push((glyph.id, "margin".into()));
            } else {
                body.push(glyph);
            }
        }
        if !body.is_empty() {
            lines.push(visual_line(body));
        }
    }
    kept.extend(detach_border_rules(&mut lines));
    kept.extend(detach_rule_noise(&mut lines));
    // A scanned cover whose OCR is `3jOV£9rOIit` has no sentence. Painting a
    // translation covers the photograph with gibberish. The image stays.
    kept.extend(detach_figure_text_layers(&mut lines));
    kept.extend(detach_junk_ocr_pages(&mut lines));
    kept.extend(detach_low_quality_ocr(&mut lines, regions, pages));
    attach_markers(&mut lines);
    // The body line is split where a superscript (`d_model×d_k`) leaves a
    // hole. Join the shards while those scripts are still their own lines.
    // Attaching first parks the scripts on the left shard, the hole no
    // longer has a line over it, and equation (1) loses its closing `)V`.
    bridge_formula_holes(&mut lines);
    attach_math_scripts(&mut lines);
    attach_formula_scripts(&mut lines);
    kept.extend(detach_toc_marks(&mut lines));
    kept.extend(interior_glyphs(&lines, flags));
    kept.extend(region_interiors(&lines, flags, regions, pages));
    let ruled = ruled_table_rows(&lines, &kept, regions);
    kept.extend(ruled);
    // A booktabs table of words (`Factor  Setting  Applies to`) has no
    // numeric cell for the grid detector. Its rules and columns mark it.
    let worded = worded_ruled_tables(&lines, &kept, regions);
    kept.extend(worded);
    // A contents row can sit in a table: Word paints "Table 1: … …. 5"
    // as a caption, and a boxed contents list looks like a figure. The
    // title is still translated. Leaders and page numbers stay.
    release_toc_titles(&lines, &mut kept);
    // A table is kept glyph for glyph, column headers included: a header
    // row redrawn in another script no longer lines up with its cells.
    release_table_group_labels(&lines, &mut kept);
    // `ᵃ Visible if EXIF …` under the grid is the table's own note. It keeps
    // its marker's script, so it stays glyph for glyph with the table.
    let notes = table_note_glyphs(&lines, &kept);
    kept.extend(notes);
    kept.extend(margin_stamps(&lines));
    release_hyphen_continuations(&lines, &mut kept);
    // A theorem frame is a box, so a short wrap (`version. We write…`,
    // `to x + y = z.`) was kept as a figure label. It is the next line of
    // the body paragraph above it.
    release_body_wraps(&lines, &mut kept);
    // A listing is a monospace line. Inline code shares its line with the
    // sentence, so those words stay in the paragraph. The listing itself is
    // not translated.
    kept.extend(listing_glyphs(&lines));
    kept.extend(algorithm_glyphs(&lines));
    kept.extend(keep_low_prose_math(&lines));
    let kept_ids: crate::detmap::HashSet<u32> = kept.iter().map(|(id, _)| *id).collect();
    if !kept_ids.is_empty() {
        lines.retain(|line| {
            line.glyphs
                .iter()
                .any(|glyph| !kept_ids.contains(&glyph.id))
        });
        for line in &mut lines {
            line.glyphs.retain(|glyph| !kept_ids.contains(&glyph.id));
            line.text = line_text(&line.glyphs);
            if let Some(bounds) = line_bounds(&line.glyphs) {
                line.left = bounds.0;
                line.right = bounds.1;
                line.y = bounds.2;
                line.size = bounds.3;
            }
        }
        lines.retain(|line| !line.glyphs.is_empty());
    }
    let paragraphs = stitch_page_continuations(join_paragraphs(lines));
    let mut segments = Vec::new();
    for para in paragraphs {
        let page = para[0].page;
        let mut buf_ids: Vec<u32> = Vec::new();
        let mut buf = String::new();
        let flush = |segments: &mut Vec<Segment>, buf_ids: &mut Vec<u32>, buf: &mut String| {
            if buf_ids.is_empty() || buf.trim().is_empty() {
                buf_ids.clear();
                buf.clear();
                return;
            }
            segments.push(Segment {
                id: segments.len() as u32,
                page_index: page,
                glyph_ids: std::mem::take(buf_ids),
                text: std::mem::take(buf),
            });
        };
        let mut lines = para.iter();
        if let Some(first) = lines.next() {
            // `• Semantic signals (rᵢ): item review text…` is one list item.
            // Splitting its bolder lead off an italic body seats the label
            // apart from the wrapped body, and the two collide. A bold lead
            // over a roman body still splits.
            // A paragraph that continues at the top of the next column on
            // the same page keeps its label too: the body is seated beside
            // the label, so its second-column lines landed in the first one.
            let next_column = para
                .windows(2)
                .any(|pair| pair[0].page == pair[1].page && pair[1].y > pair[0].y + 1.0);
            let run_in = (!next_column)
                .then(|| styled_run_in(first))
                .flatten()
                .filter(|(_, rest)| {
                    !(starts_with_list_marker(&first.text) && italic_majority(rest))
                });
            if let Some((label, rest)) = run_in {
                // "Definition 1.1." is bold and "Proof." is italic. The sentence
                // after either label keeps the body face.
                buf = line_text(&label);
                buf_ids.extend(label.iter().map(|glyph| glyph.id));
                flush(&mut segments, &mut buf_ids, &mut buf);
                buf = line_text(&rest);
                buf_ids.extend(rest.iter().map(|glyph| glyph.id));
            } else {
                // A list item's italic lead (`Part 1: …—`) is the same paragraph
                // as the roman lines under it. An obstacle sentence is not.
                let split_lead = !starts_with_list_marker(&first.text)
                    && styled_lead_line(first).is_some_and(|face| {
                        lines.clone().next().is_some_and(|next| {
                            majority_run(next) != Some(face)
                                && !has_year(&first.text)
                                && !has_year(&next.text)
                        })
                    });
                buf = trim_slide_pad(&first.text).to_string();
                buf_ids.extend(first.glyphs.iter().map(|glyph| glyph.id));
                // "Obstacle 1: … inconsistent." is its own italic line. The
                // roman explanation under it is the body.
                if split_lead {
                    flush(&mut segments, &mut buf_ids, &mut buf);
                }
            }
        }
        for line in lines {
            if buf.trim().is_empty() {
                buf = trim_slide_pad(&line.text).to_string();
                buf_ids.extend(line.glyphs.iter().map(|glyph| glyph.id));
                continue;
            }
            let incoming = trim_slide_pad(line.text.trim_start());
            let piece = if url_continues(&buf, &line.text) {
                format!("{}{}", buf.trim_end(), incoming)
            } else if let Some(stem) = soft_hyphen_stem(&buf) {
                if incoming.starts_with(|ch: char| ch.is_ascii_alphanumeric()) {
                    join_hyphenated_word(stem, incoming)
                } else {
                    format!("{} {}", buf.trim_end(), incoming)
                }
            } else {
                format!("{} {}", buf.trim_end(), incoming)
            };
            if piece.chars().count() > LONG_LINE && sentence_end(&buf) {
                flush(&mut segments, &mut buf_ids, &mut buf);
                buf = line.text.clone();
            } else {
                buf = piece;
            }
            buf_ids.extend(line.glyphs.iter().map(|glyph| glyph.id));
        }
        flush(&mut segments, &mut buf_ids, &mut buf);
    }
    // `Email addresses: a@x.de (A. Name), b@y.uk (B. Name)` is a contact
    // list. Addresses are not translated, and a translated list reflows
    // the addresses into each other.
    segments.retain(|seg| {
        if !contact_list(&seg.text) {
            return true;
        }
        kept.extend(seg.glyph_ids.iter().map(|id| (*id, "contact".to_string())));
        false
    });
    for (id, seg) in segments.iter_mut().enumerate() {
        seg.id = id as u32;
    }
    Segmentation { segments, kept }
}

/// Two or more email addresses with only names in parentheses and a short
/// label (`Email addresses:`) around them.
fn contact_list(text: &str) -> bool {
    let is_email = |word: &str| {
        let word = word.trim_matches(|ch: char| matches!(ch, ',' | ';' | '.' | '(' | ')'));
        word.split_once('@').is_some_and(|(user, host)| {
            !user.is_empty() && host.contains('.') && !host.starts_with('.')
        })
    };
    let emails = text
        .split_whitespace()
        .filter(|word| is_email(word))
        .count();
    if emails < 2 {
        return false;
    }
    let mut depth = 0usize;
    let mut outside = String::new();
    for ch in text.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => outside.push(ch),
            _ => {}
        }
    }
    let rest: String = outside
        .split_whitespace()
        .filter(|word| !is_email(word))
        .collect::<Vec<_>>()
        .join(" ");
    let label_end = rest
        .find(':')
        .filter(|end| rest[..*end].split_whitespace().count() <= 3);
    let rest = label_end.map_or(rest.as_str(), |end| &rest[end + 1..]);
    rest.chars().filter(|ch| ch.is_alphabetic()).count() <= 12
}

/// A line set entirely in a monospace face is a listing. A dingbat or an
/// arrow in front of that line is the marker, not body text. A sentence that
/// only borrows a few monospace words keeps those words.
fn listing_glyphs(lines: &[VisualLine<'_>]) -> Vec<(u32, String)> {
    let prose = mono_prose_block(lines);
    let mut kept = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if prose[index] || !listing_line(line) {
            continue;
        }
        for glyph in &line.glyphs {
            kept.push((glyph.id, "code".into()));
        }
    }
    kept
}

fn listing_line(line: &VisualLine<'_>) -> bool {
    let mut mono_letters = 0usize;
    let mut mono_marks = 0usize;
    let mut other = 0usize;
    for glyph in &line.glyphs {
        if glyph.unicode.trim().is_empty() {
            continue;
        }
        let letters = letter_count(glyph);
        if mono_face(&glyph.font_name) {
            if letters > 0 {
                mono_letters += letters;
            } else {
                // A closing bracket is the last line of the listing. It has
                // no letters, and leaving it out lets that line stay behind
                // when the block above it moves.
                mono_marks += 1;
            }
        } else if letters > 0 {
            other += letters;
        }
    }
    other == 0 && (mono_letters >= 2 || (mono_letters == 0 && mono_marks >= 1))
}

/// Several stacked monospace lines of ordinary sentences are a license or a
/// preface, not a listing. One mono sentence stays code.
fn mono_prose_block(lines: &[VisualLine<'_>]) -> Vec<bool> {
    let mut prose = vec![false; lines.len()];
    let mut index = 0usize;
    while index < lines.len() {
        if !mono_prose_line(&lines[index]) {
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        while index < lines.len()
            && mono_prose_line(&lines[index])
            && mono_lines_stack(&lines[index - 1], &lines[index])
        {
            index += 1;
        }
        if index - start >= 3 {
            for flag in &mut prose[start..index] {
                *flag = true;
            }
        }
    }
    prose
}

fn mono_prose_line(line: &VisualLine<'_>) -> bool {
    if !listing_line(line) {
        return false;
    }
    let letters = line
        .text
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .count();
    let words = line.text.split_whitespace().count();
    let symbols = line
        .text
        .chars()
        .filter(|ch| {
            matches!(
                ch,
                '{' | '}'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | ';'
                    | '='
                    | '<'
                    | '>'
                    | '#'
                    | '`'
                    | '|'
                    | '\\'
                    | '/'
                    | '_'
                    | '$'
                    | '%'
                    | '&'
            )
        })
        .count();
    letters >= 25 && words >= 5 && symbols * 4 <= letters.max(1)
}

fn mono_lines_stack(above: &VisualLine<'_>, below: &VisualLine<'_>) -> bool {
    if above.page != below.page {
        return false;
    }
    let size = above.size.max(below.size).max(1.0);
    (above.y - below.y).abs() <= size * 2.4 && (above.left - below.left).abs() <= size * 3.0
}

pub(crate) fn mono_face(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "MONO",
        "COURIER",
        "CONSOLAS",
        "MENLO",
        "INCONSOLATA",
        "CASCADIA",
        "JETBRAINS",
        "TYPEWRITER",
        "CMTT",
        "LMTT",
        "NIMBUSMON",
        "OCRA",
        "OCRB",
        "OCR-A",
        "OCR-B",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
}

fn visual_line(glyphs: Vec<&Glyph>) -> VisualLine<'_> {
    let (left, right, y, size) = line_bounds(&glyphs).unwrap_or((0.0, 0.0, 0.0, 12.0));
    let font = font_key(&glyphs[0].font_name);
    VisualLine {
        text: line_text(&glyphs),
        page: glyphs[0].page_index,
        y,
        left,
        right,
        size,
        font,
        glyphs,
        toc: false,
        sole_on_baseline: true,
    }
}

/// A contents row is `title | leaders | page`. The leaders and the page
/// number stay on their original baselines. Each title between page numbers
/// is its own line, so a later rewrite cannot pull the next entry across.
fn detach_toc_marks(lines: &mut Vec<VisualLine<'_>>) -> Vec<(u32, String)> {
    let mut kept = Vec::new();
    let mut expanded = Vec::with_capacity(lines.len());
    for line in lines.drain(..) {
        let (marks, scripts) = toc_mark_glyphs(&line);
        if marks.is_empty() && scripts.is_empty() {
            expanded.push(line);
            continue;
        }
        let mark_ids: crate::detmap::HashSet<u32> =
            marks.iter().chain(scripts.iter()).copied().collect();
        for id in &marks {
            kept.push((*id, "toc".into()));
        }
        // `x_0` and `x_1` share a subscript baseline. The gap between `0` and
        // `1` looks like a contents page number, but blanking `1` and seating
        // it on the equation deletes the subscript. Keep the digit in place.
        for id in &scripts {
            kept.push((*id, "formula".into()));
        }
        let mut ordered = line.glyphs.clone();
        ordered.sort_by(|left, right| glyph_left(left).total_cmp(&glyph_left(right)));
        let mut title: Vec<&Glyph> = Vec::new();
        for glyph in ordered {
            if mark_ids.contains(&glyph.id) {
                push_toc_title(&mut expanded, &mut title);
            } else {
                title.push(glyph);
            }
        }
        push_toc_title(&mut expanded, &mut title);
    }
    *lines = expanded;
    kept
}

fn push_toc_title<'a>(expanded: &mut Vec<VisualLine<'a>>, title: &mut Vec<&'a Glyph>) {
    if title.is_empty() {
        return;
    }
    let mut part = visual_line(std::mem::take(title));
    part.toc = true;
    expanded.push(part);
}

/// Real contents page numbers, and digits that only share that gap.
/// The second list is a subscript (`0` then `1` under `x_0, x_1`), not a
/// page number. Both leave the line so the remaining token stays put.
fn toc_mark_glyphs(line: &VisualLine<'_>) -> (Vec<u32>, Vec<u32>) {
    let tokens = line_tokens(&line.glyphs, line.size);
    let mut pages = Vec::new();
    let mut scripts = Vec::new();
    for (index, token) in tokens.iter().enumerate().skip(1) {
        if !is_toc_page_token(&token.text) {
            continue;
        }
        let prev = &tokens[index - 1];
        let gap = token.left - prev.right;
        let leaders = is_leader_text(&prev.text);
        // A detailed-contents row packs several entries on one baseline:
        // `engines 4  LLM-based chatbots 7  AI agents 8`. The page number
        // sits just past the title, closer than the usual leader gap, and
        // the next entry starts about an em later. A trailing `model 15`
        // has the same tight gap and no following entry.
        let next_entry = separates_next_entry(&tokens, index, line.size);
        let trailing =
            index + 1 == tokens.len() && gap > line.size * 0.45 && gap <= line.size * 0.9;
        if !leaders && gap < line.size * 0.8 && !next_entry && !trailing {
            continue;
        }
        if leaders {
            pages.extend(prev.ids.iter().copied());
        }
        // The token before a page number is a title or a run of dots.
        // `0` an em before `1` is the other subscript, not a title.
        let titled = prev.text.chars().any(|ch| ch.is_alphabetic());
        if !leaders && !titled {
            scripts.extend(token.ids.iter().copied());
            continue;
        }
        pages.extend(token.ids.iter().copied());
    }
    // `.......1` is one token when the page number sits against the dots.
    for id in toc_leader_suffix(&line.glyphs) {
        if !pages.contains(&id) && !scripts.contains(&id) {
            pages.push(id);
        }
    }
    (pages, scripts)
}

/// Leader dots at the right edge, plus a page number glued to them.
fn toc_leader_suffix(glyphs: &[&Glyph]) -> Vec<u32> {
    let mut ordered = glyphs.to_vec();
    ordered.sort_by(|left, right| glyph_left(left).total_cmp(&glyph_left(right)));
    let mut index = ordered.len();
    while index > 0 && ordered[index - 1].unicode.trim().is_empty() {
        index -= 1;
    }
    let page_end = index;
    let mut page = String::new();
    let mut page_start = index;
    while page_start > 0 {
        let piece = ordered[page_start - 1].unicode.trim();
        if piece.is_empty() {
            break;
        }
        let next = format!("{piece}{page}");
        if is_page_prefix(&next) {
            page = next;
            page_start -= 1;
            continue;
        }
        break;
    }
    let page_ok = is_toc_page_token(&page);
    let dots_end = if page_ok { page_start } else { page_end };
    let mut dots_start = dots_end;
    let mut dot_chars = 0usize;
    while dots_start > 0 {
        let text = ordered[dots_start - 1].unicode.trim();
        if text.is_empty() {
            dots_start -= 1;
            continue;
        }
        if text.chars().all(is_leader_dot) {
            dot_chars += text.chars().filter(|ch| is_leader_dot(*ch)).count();
            dots_start -= 1;
            continue;
        }
        break;
    }
    // Dots alone are an ellipsis (`tion.......`). A contents row also has
    // its page number on this edge, or spaced leaders (`. . . .`) whose
    // number the column split put on its own line.
    if dot_chars < 4 || !(page_ok || spaced_leaders(&ordered[dots_start..dots_end])) {
        return Vec::new();
    }
    let end = page_end;
    ordered[dots_start..end]
        .iter()
        .filter(|glyph| !glyph.unicode.trim().is_empty())
        .map(|glyph| glyph.id)
        .collect()
}

/// At least eight dots set an em apart: TeX's `\\dotfill` leaders. An
/// ellipsis packs its dots together.
fn spaced_leaders(glyphs: &[&Glyph]) -> bool {
    let dots: Vec<&&Glyph> = glyphs
        .iter()
        .filter(|glyph| {
            glyph.unicode.trim().chars().all(is_leader_dot) && !glyph.unicode.trim().is_empty()
        })
        .collect();
    if dots.len() < 8 {
        return false;
    }
    let first = glyph_left(dots[0]);
    let last = glyph_left(dots[dots.len() - 1]);
    let step = (last - first) / (dots.len() - 1) as f32;
    let width = dots
        .iter()
        .map(|glyph| (glyph.bbox[2] - glyph.bbox[0]).abs())
        .fold(0.0f32, f32::max);
    step > width * 2.0
}

fn is_page_prefix(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > 6 {
        return false;
    }
    if text.bytes().all(|byte| byte.is_ascii_digit()) {
        return text.len() <= 3;
    }
    text.chars().all(|ch| {
        matches!(
            ch.to_ascii_lowercase(),
            'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'
        )
    })
}

struct LineToken {
    ids: Vec<u32>,
    left: f32,
    right: f32,
    text: String,
}

fn line_tokens(glyphs: &[&Glyph], size: f32) -> Vec<LineToken> {
    let mut ordered: Vec<&Glyph> = glyphs.to_vec();
    ordered.sort_by(|left, right| glyph_left(left).total_cmp(&glyph_left(right)));
    let mut tokens = Vec::new();
    let mut current: Vec<&Glyph> = Vec::new();
    for glyph in ordered {
        if let Some(prev) = current.last() {
            if glyph_left(glyph) - glyph_right(prev) > size * 0.45 {
                tokens.push(token_of(std::mem::take(&mut current)));
            }
        }
        current.push(glyph);
    }
    if !current.is_empty() {
        tokens.push(token_of(current));
    }
    merge_leader_tokens(tokens)
}

fn token_of(glyphs: Vec<&Glyph>) -> LineToken {
    LineToken {
        left: glyphs
            .iter()
            .map(|glyph| glyph_left(glyph))
            .fold(f32::MAX, f32::min),
        right: glyphs
            .iter()
            .map(|glyph| glyph_right(glyph))
            .fold(0.0, f32::max),
        text: glyphs.iter().map(|glyph| glyph.unicode.as_str()).collect(),
        ids: glyphs.iter().map(|glyph| glyph.id).collect(),
    }
}

fn merge_leader_tokens(tokens: Vec<LineToken>) -> Vec<LineToken> {
    let mut merged: Vec<LineToken> = Vec::new();
    for token in tokens {
        if is_leader_text(&token.text) {
            if let Some(prev) = merged.last_mut() {
                if is_leader_text(&prev.text) && token.left - prev.right < 14.0 {
                    prev.right = token.right;
                    prev.text.push_str(&token.text);
                    prev.ids.extend(token.ids);
                    continue;
                }
            }
        }
        merged.push(token);
    }
    merged
}

fn is_leader_text(text: &str) -> bool {
    let dots = text.chars().filter(|ch| is_leader_dot(*ch)).count();
    dots >= 4
        && text
            .chars()
            .all(|ch| is_leader_dot(ch) || ch.is_whitespace())
}

fn is_leader_dot(ch: char) -> bool {
    matches!(ch, '.' | '·' | '…' | '•' | '⋅' | '‧' | '․')
}

/// The next token is another contents title, not the next word of this one.
fn separates_next_entry(tokens: &[LineToken], index: usize, size: f32) -> bool {
    let Some(next) = tokens.get(index + 1) else {
        return false;
    };
    let gap = next.left - tokens[index].right;
    if gap < size * 1.15 || gap > size * 2.2 {
        return false;
    }
    let text = next.text.trim();
    let Some(ch) = text.chars().next() else {
        return false;
    };
    ch.is_ascii_uppercase() || (ch.is_ascii_digit() && text.contains('.'))
}

fn is_toc_page_token(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    if text.len() <= 3 && text.bytes().all(|byte| byte.is_ascii_digit()) {
        return true;
    }
    text.chars().count() >= 2 && is_roman_numeral(text)
}

fn glyph_left(glyph: &Glyph) -> f32 {
    glyph.matrix[4].min(glyph.bbox[0]).min(glyph.bbox[2])
}

fn glyph_right(glyph: &Glyph) -> f32 {
    glyph.bbox[0].max(glyph.bbox[2]).max(glyph.matrix[4])
}

/// Leading and trailing spaces do not move the line's edges. A line that is
/// only spaces keeps every glyph, so an empty pad still has a position.
fn ink_glyphs<'a>(glyphs: &[&'a Glyph]) -> Vec<&'a Glyph> {
    let mut start = 0usize;
    let mut end = glyphs.len();
    while start < end && glyph_is_space(glyphs[start]) {
        start += 1;
    }
    while end > start && glyph_is_space(glyphs[end - 1]) {
        end -= 1;
    }
    if start == end {
        glyphs.to_vec()
    } else {
        glyphs[start..end].to_vec()
    }
}

fn glyph_is_space(glyph: &Glyph) -> bool {
    !glyph.unicode.is_empty() && glyph.unicode.chars().all(char::is_whitespace)
}

/// A slides box pads the last line with spaces out to the frame. One trailing
/// space is a word space and stays, so a saved sentence still matches.
fn trim_slide_pad(text: &str) -> &str {
    let end = text.trim_end();
    if text.len() - end.len() > 2 {
        end
    } else {
        text
    }
}

fn line_bounds(glyphs: &[&Glyph]) -> Option<(f32, f32, f32, f32)> {
    if glyphs.is_empty() {
        return None;
    }
    // A slides text box pads the last line with spaces. Those advances run
    // past the page and pull the midpoint off the centered axis, so the line
    // looks like a new paragraph and the topics on it are painted separately.
    let ink = ink_glyphs(glyphs);
    let left = ink
        .iter()
        .map(|glyph| glyph.matrix[4].min(glyph.bbox[0]).min(glyph.bbox[2]))
        .fold(f32::MAX, f32::min);
    let right = ink
        .iter()
        .map(|glyph| glyph.bbox[0].max(glyph.bbox[2]).max(glyph.matrix[4]))
        .fold(left, f32::max);
    let mut ys: Vec<f32> = glyphs.iter().map(|glyph| glyph.matrix[5]).collect();
    ys.sort_by(|a, b| a.total_cmp(b));
    let y = ys[ys.len() / 2];
    let size = representative_size(glyphs);
    Some((left, right, y, size))
}

/// Size used for leading and column joins.
///
/// A drop cap is one glyph several times the letters it sits in. Using that
/// size as the line size refuses the body underneath it. The median is the
/// body size once the line is more than a couple of glyphs.
/// Baseline and size of the body glyphs on this line. A single oversized
/// margin stamp does not move the anchor.
fn body_anchor(glyphs: &[&Glyph]) -> Option<(f32, f32)> {
    if glyphs.is_empty() {
        return None;
    }
    let size = representative_size(glyphs);
    let mut ys: Vec<f32> = glyphs
        .iter()
        .filter(|glyph| {
            let ratio = glyph.font_size.max(1.0) / size;
            (0.65..=1.8).contains(&ratio)
        })
        .map(|glyph| glyph.matrix[5])
        .collect();
    if ys.is_empty() {
        ys = glyphs.iter().map(|glyph| glyph.matrix[5]).collect();
    }
    ys.sort_by(|left, right| left.total_cmp(right));
    Some((ys[ys.len() / 2], size))
}

pub(crate) fn representative_size(glyphs: &[&Glyph]) -> f32 {
    let mut sizes: Vec<f32> = glyphs
        .iter()
        .map(|glyph| glyph.font_size)
        .filter(|size| *size >= 1.0)
        .collect();
    if sizes.is_empty() {
        return 1.0;
    }
    sizes.sort_by(|a, b| a.total_cmp(b));
    let median = sizes[sizes.len() / 2];
    let max = sizes[sizes.len() - 1];
    if sizes.len() >= 4 && max > median * 1.8 {
        // A drop cap is one or two glyphs. A longer run at the large size is
        // the body: small labels on the same baseline pulled the median down,
        // and a word space then looks like a column gutter.
        let large: Vec<f32> = sizes
            .iter()
            .copied()
            .filter(|size| *size > median * 1.8)
            .collect();
        if large.len() >= 4 {
            large[large.len() / 2]
        } else {
            median
        }
    } else {
        max.max(1.0)
    }
}

fn line_text(glyphs: &[&Glyph]) -> String {
    let mut buf = String::new();
    let mut previous: Option<&Glyph> = None;
    for glyph in glyphs {
        if let Some(prev) = previous {
            if word_space(prev, glyph) && !is_superscript_marker(glyph, prev) {
                buf.push(' ');
            }
        }
        buf.push_str(&glyph.unicode);
        previous = Some(glyph);
    }
    buf
}

fn font_key(name: &str) -> String {
    name.rsplit('+').next().unwrap_or(name).to_string()
}

fn same_font_family(a: &str, b: &str) -> bool {
    font_family(a) == font_family(b)
}

/// Computer Modern bold, `CMBX12`. A semibold diagram face is not a section.
fn bold_face(name: &str) -> bool {
    font_key(name).to_ascii_lowercase().contains("cmbx")
}

/// `Related Work` is a heading. `When the task reaches a point` is a sentence.
fn title_case_heading(rest: &str) -> bool {
    let words: Vec<&str> = rest.split_whitespace().collect();
    if words.is_empty() || words.len() > 6 || rest.contains([',', ';', ':']) {
        return false;
    }
    words.iter().all(|word| {
        let word = word.trim_matches(|ch: char| !ch.is_ascii_alphabetic());
        word.is_empty()
            || word
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_uppercase())
    })
}

fn fonts_can_join(upper: &VisualLine<'_>, lower: &VisualLine<'_>) -> bool {
    if same_font_family(&upper.font, &lower.font) {
        return true;
    }
    let families = |line: &VisualLine<'_>| {
        line.glyphs
            .iter()
            .map(|glyph| font_family(&glyph.font_name))
            .collect::<crate::detmap::HashSet<_>>()
    };
    let left = families(upper);
    families(lower).iter().any(|family| left.contains(family))
}

fn font_family(name: &str) -> String {
    let base = name.rsplit('+').next().unwrap_or(name).to_ascii_lowercase();
    for suffix in [
        "-bolditalic",
        "-boldital",
        "-reguital",
        "-semibold",
        "-demibold",
        "-italic",
        "-regular",
        "-medium",
        "-roman",
        "-light",
        "-black",
        "-bold",
        "-ital",
        "-medi",
        "-regu",
        "-book",
    ] {
        if let Some(stripped) = base.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    base
}

fn attach_markers(lines: &mut Vec<VisualLine<'_>>) {
    let mut drop = vec![false; lines.len()];
    let hosts: Vec<Option<usize>> = (0..lines.len())
        .map(|index| marker_host(index, lines))
        .collect();
    for (index, host) in hosts.into_iter().enumerate() {
        let Some(host) = host else {
            continue;
        };
        let markers = std::mem::take(&mut lines[index].glyphs);
        lines[host].glyphs.extend(markers);
        lines[host]
            .glyphs
            .sort_by(|a, b| a.matrix[4].total_cmp(&b.matrix[4]));
        lines[host].text = line_text(&lines[host].glyphs);
        if let Some((left, right, y, size)) = line_bounds(&lines[host].glyphs) {
            lines[host].left = left;
            lines[host].right = right;
            lines[host].y = y;
            lines[host].size = size;
        }
        drop[index] = true;
    }
    let mut index = 0;
    lines.retain(|_| {
        let keep = !drop[index];
        index += 1;
        keep
    });
}

fn marker_host(index: usize, lines: &[VisualLine<'_>]) -> Option<usize> {
    let line = &lines[index];
    if !is_marker_line(line) {
        return None;
    }
    let mut best: Option<(usize, f32)> = None;
    for (other_index, other) in lines.iter().enumerate() {
        if other_index == index || other.page != line.page || is_marker_line(other) {
            continue;
        }
        let dy = line.y - other.y;
        let size = other.size.max(1.0);
        if dy < size * 0.12 || dy > size * 0.9 {
            continue;
        }
        if line.size > size * 0.85 {
            continue;
        }
        let span = other.right - other.left;
        if line.left < other.left - size || line.right > other.right + size {
            continue;
        }
        if span < size {
            continue;
        }
        let near = line.glyphs.iter().all(|mark| {
            other.glyphs.iter().any(|glyph| {
                (mark.matrix[4] - glyph.bbox[0].max(glyph.bbox[2])).abs() <= size * 1.6
                    || (mark.matrix[4] >= glyph.matrix[4] - size
                        && mark.matrix[4] <= glyph.bbox[0].max(glyph.bbox[2]) + size)
            })
        });
        if !near {
            continue;
        }
        if best.is_none_or(|(_, best_dy)| dy < best_dy) {
            best = Some((other_index, dy));
        }
    }
    best.map(|(host, _)| host)
}

fn is_marker_line(line: &VisualLine<'_>) -> bool {
    !line.glyphs.is_empty()
        && line.glyphs.len() <= 16
        && line
            .glyphs
            .iter()
            .all(|glyph| is_marker_text(&glyph.unicode))
}

/// A footnote mark is smaller than the glyph it follows. A body-size `#`
/// in a monospace span is a character, so it keeps the word space in front.
fn is_superscript_marker(glyph: &Glyph, prev: &Glyph) -> bool {
    is_marker_text(&glyph.unicode) && glyph.font_size <= prev.font_size * 0.85
}

fn is_marker_text(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && text.chars().count() <= 2
        && text.chars().all(|ch| {
            ch.is_ascii_digit() || matches!(ch, '*' | '∗' | '†' | '‡' | '§' | '¶' | '⋆' | '#' | '+')
        })
}

/// Glyphs drawn inside a figure or a table, excluding the caption paragraph.
fn interior_glyphs(lines: &[VisualLine<'_>], flags: &SegmentFlags) -> Vec<(u32, String)> {
    if !flags.skip_figures && !flags.skip_tables {
        return Vec::new();
    }
    let order = reading_order(lines);
    let mut body_fonts: crate::detmap::HashMap<u32, String> = crate::detmap::HashMap::default();
    let mut caption_line = vec![false; lines.len()];
    let mut kept = Vec::new();
    let mut claimed = vec![false; lines.len()];
    let mut seen = vec![false; lines.len()];
    for &start in &order {
        if seen[start] {
            continue;
        }
        let Some(kind) = caption_kind(&lines[start].text) else {
            continue;
        };
        // "Table 2: Practices …… 24" is a contents row, not a caption.
        if lines[start].toc {
            continue;
        }
        if (kind == "figure" && !flags.skip_figures) || (kind == "table" && !flags.skip_tables) {
            continue;
        }
        let block = caption_block(start, lines, &order);
        for index in &block {
            seen[*index] = true;
            caption_line[*index] = true;
        }
        let page = lines[start].page;
        let body_font = body_fonts
            .entry(page)
            .or_insert_with(|| dominant_body_font(lines, page));
        for index in figure_side(
            &block,
            lines,
            body_font,
            &caption_line,
            kind == "table",
            &claimed,
        ) {
            if claimed[index] || caption_line[index] {
                continue;
            }
            claimed[index] = true;
            for glyph in &lines[index].glyphs {
                kept.push((glyph.id, kind.to_string()));
            }
        }
    }
    kept
}

/// A box can cover the second half of a line-break hyphen (`over-` / `all`).
/// The stem is still body prose, so that continuation is body prose too.
/// A label that does not finish the word stays inside the drawing.
fn release_hyphen_continuations(lines: &[VisualLine<'_>], kept: &mut Vec<(u32, String)>) {
    let kept_ids: crate::detmap::HashSet<u32> = kept.iter().map(|(id, _)| *id).collect();
    let mut release = crate::detmap::HashSet::default();
    for upper in lines {
        if soft_hyphen_stem(&upper.text).is_none() {
            continue;
        }
        let upper_kept = upper
            .glyphs
            .iter()
            .filter(|glyph| kept_ids.contains(&glyph.id))
            .count();
        if upper.glyphs.is_empty() || upper_kept * 2 > upper.glyphs.len() {
            continue;
        }
        for lower in lines {
            // The other column can sit one leading lower. Only the same
            // column is the rest of this word.
            if (upper.left - lower.left).abs() > 36.0 || !continues_paragraph(upper, lower, 0.0) {
                continue;
            }
            let lower_kept = lower
                .glyphs
                .iter()
                .filter(|glyph| kept_ids.contains(&glyph.id))
                .count();
            if lower.glyphs.is_empty() || lower_kept * 2 <= lower.glyphs.len() {
                continue;
            }
            for glyph in &lower.glyphs {
                release.insert(glyph.id);
            }
        }
    }
    if !release.is_empty() {
        kept.retain(|(id, _)| !release.contains(id));
    }
}

/// A short line inside or just above a frame stays a figure label unless it
/// is the next line of a body paragraph (`version. We write [N]…`).
fn release_body_wraps(lines: &[VisualLine<'_>], kept: &mut Vec<(u32, String)>) {
    let kept_ids: crate::detmap::HashSet<u32> = kept.iter().map(|(id, _)| *id).collect();
    let mut release = crate::detmap::HashSet::default();
    for lower in lines {
        let lower_kept = lower
            .glyphs
            .iter()
            .filter(|glyph| kept_ids.contains(&glyph.id))
            .count();
        if lower.glyphs.is_empty() || lower_kept * 2 <= lower.glyphs.len() {
            continue;
        }
        let figure = lower.glyphs.iter().any(|glyph| {
            kept.iter()
                .any(|(id, reason)| *id == glyph.id && reason == "figure")
        });
        let table = !figure
            && lower.glyphs.iter().any(|glyph| {
                kept.iter()
                    .any(|(id, reason)| *id == glyph.id && reason == "table")
            });
        if !(figure || table) || is_panel_label(&lower.text) {
            continue;
        }
        for upper in lines {
            if upper.page != lower.page {
                continue;
            }
            let upper_kept = upper
                .glyphs
                .iter()
                .filter(|glyph| kept_ids.contains(&glyph.id))
                .count();
            if upper.glyphs.is_empty() || upper_kept * 2 > upper.glyphs.len() {
                continue;
            }
            if table && !table_body_wrap(upper, lower) {
                continue;
            }
            if continues_paragraph(upper, lower, 0.0) {
                for glyph in &lower.glyphs {
                    release.insert(glyph.id);
                }
                break;
            }
        }
    }
    if !release.is_empty() {
        kept.retain(|(id, _)| !release.contains(id));
    }
}

/// `… recommendations across` / `all test conversations.` sits just above a
/// table's top rule, so the grid claimed it. It is the body paragraph's last
/// line: the same face and size as the line above, which does not end its
/// sentence, and it starts lowercase. A table cell is smaller than the body.
fn table_body_wrap(upper: &VisualLine<'_>, lower: &VisualLine<'_>) -> bool {
    let open = !upper
        .text
        .trim_end()
        .ends_with(['.', '?', '!', ':', '。', '？', '！', '：']);
    let lowercase = lower
        .text
        .trim_start()
        .starts_with(|ch: char| ch.is_lowercase());
    open && lowercase
        && upper.font == lower.font
        && (upper.size - lower.size).abs() <= 0.3
        && (upper.left - lower.left).abs() <= 2.0
}

/// Labels inside a cluster of boxes, or on an image, stay original.
/// A "Figure N" caption is not required. Body lines and captions are not kept.
fn region_interiors(
    lines: &[VisualLine<'_>],
    flags: &SegmentFlags,
    regions: &[PaintedRegion],
    pages: &[PageInfo],
) -> Vec<(u32, String)> {
    if !flags.skip_figures || regions.is_empty() {
        return Vec::new();
    }
    let mut by_page: crate::detmap::HashMap<u32, Vec<&PaintedRegion>> =
        crate::detmap::HashMap::default();
    for region in regions {
        by_page.entry(region.page_index).or_default().push(region);
    }
    let mut kept = Vec::new();
    for (page, regs) in by_page {
        let (width, height) = page_size(pages, page);
        let unions = figure_unions(&regs, width, height);
        // Class boxes sit in two columns. A prose callout in the gutter is
        // inside the diagram but not inside any one box.
        let body_font = dominant_body_font(lines, page);
        let span = diagram_span(&unions);
        for line in lines.iter().filter(|line| line.page == page) {
            if !line_in_figure(line, &unions, &body_font)
                && !in_diagram_gutter(line, span, &body_font)
            {
                continue;
            }
            for glyph in &line.glyphs {
                kept.push((glyph.id, "figure".to_string()));
            }
        }
    }
    kept
}

fn page_size(pages: &[PageInfo], page: u32) -> (f32, f32) {
    pages
        .iter()
        .find(|info| info.index == page)
        .map(|info| {
            (
                info.media_box[2] - info.media_box[0],
                info.media_box[3] - info.media_box[1],
            )
        })
        .unwrap_or((0.0, 0.0))
}

fn figure_unions(regions: &[&PaintedRegion], page_w: f32, page_h: f32) -> Vec<Rect> {
    struct Item {
        rect: Rect,
        image: bool,
        fat: bool,
    }
    let mut items = Vec::new();
    for region in regions {
        let rect = Rect::new(
            region.bbox[0],
            region.bbox[1],
            region.bbox[2],
            region.bbox[3],
        );
        let width = rect.width();
        let height = rect.height();
        // A short hairline is an underline. A long thin edge is a box border.
        let thin = width < 1.5 || height < 1.5;
        let short = width < 24.0 && height < 24.0;
        if (width < 4.0 && height < 4.0) || (thin && short) {
            continue;
        }
        if page_w > 1.0 && page_h > 1.0 && width > page_w * 0.85 && height > page_h * 0.70 {
            continue;
        }
        items.push(Item {
            rect,
            image: region.kind == "image",
            fat: width >= 6.0 && height >= 6.0,
        });
    }
    let count = items.len();
    if count == 0 {
        return Vec::new();
    }
    let mut parent: Vec<usize> = (0..count).collect();
    for left in 0..count {
        for right in left + 1..count {
            if rects_near(items[left].rect, items[right].rect, 8.0) {
                unite(&mut parent, left, right);
            }
        }
    }
    let mut groups: crate::detmap::HashMap<usize, (Rect, bool, bool, usize)> =
        crate::detmap::HashMap::default();
    for (index, item) in items.iter().enumerate() {
        let root = find_root(&mut parent, index);
        let entry = groups.entry(root).or_insert((item.rect, false, false, 0));
        entry.0 = union_rect(entry.0, item.rect);
        entry.1 |= item.image;
        entry.2 |= item.fat;
        entry.3 += 1;
    }
    groups
        .into_values()
        .filter(|(_, image, fat, boxes)| *image || (*fat && *boxes >= 2))
        .map(|(rect, _, _, _)| rect)
        .collect()
}

fn find_root(parent: &mut [usize], mut index: usize) -> usize {
    while parent[index] != index {
        parent[index] = parent[parent[index]];
        index = parent[index];
    }
    index
}

fn unite(parent: &mut [usize], left: usize, right: usize) {
    let left = find_root(parent, left);
    let right = find_root(parent, right);
    if left != right {
        parent[right] = left;
    }
}

fn union_rect(a: Rect, b: Rect) -> Rect {
    Rect {
        x0: a.x0.min(b.x0),
        y0: a.y0.min(b.y0),
        x1: a.x1.max(b.x1),
        y1: a.y1.max(b.y1),
    }
}

fn rects_near(a: Rect, b: Rect, gap: f32) -> bool {
    a.x1 + gap >= b.x0 && b.x1 + gap >= a.x0 && a.y1 + gap >= b.y0 && b.y1 + gap >= a.y0
}

/// Several boxes on one page are one diagram. The span is the drawing,
/// including the gutter between the columns of boxes.
fn diagram_span(unions: &[Rect]) -> Option<Rect> {
    if unions.len() < 4 {
        return None;
    }
    let mut span = unions[0];
    for rect in &unions[1..] {
        span = union_rect(span, *rect);
    }
    (span.width() >= 160.0 && span.height() >= 100.0).then_some(span)
}

/// A callout set in a diagram face, sitting between the boxes. Body text
/// in the column face is not a label, and neither is the caption.
fn in_diagram_gutter(line: &VisualLine<'_>, span: Option<Rect>, body_font: &str) -> bool {
    let Some(span) = span else {
        return false;
    };
    if line.toc
        || caption_kind(&line.text).is_some()
        || is_running_header(&line.text)
        || is_footer_line(&line.text)
        || is_page_folio_text(&line.text)
        || is_body_barrier(line, body_font)
        || is_display_heading(line)
        || is_section_heading(line, body_font)
        || starts_run_in(&line.text)
    {
        return false;
    }
    // No body sample on the page: do not guess. A caption that set the
    // body face stays out because it shares that face.
    if body_font.is_empty() || same_font_family(&line.font, body_font) {
        return false;
    }
    let center_x = (line.left + line.right) * 0.5;
    center_x >= span.x0 - 2.0
        && center_x <= span.x1 + 2.0
        && line.y >= span.y0 - 2.0
        && line.y <= span.y1 + 2.0
}

/// `DEFINITION` and `NOTE` are body callouts. A box around the figure must
/// not keep the first line in English while the rest of the callout is translated.
fn starts_run_in(text: &str) -> bool {
    let trimmed = text
        .trim()
        .trim_start_matches(|ch: char| !ch.is_ascii_alphabetic());
    // `example, a splitter` is a sentence inside a diagram. `DEFINITION`
    // is the callout label. A lowercase start is not that label.
    if !trimmed
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_uppercase())
    {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    [
        "definition",
        "note",
        "theorem",
        "lemma",
        "proof",
        "example",
        "remark",
        "tip",
        "warning",
    ]
    .iter()
    .any(|word| {
        // `notebook` and `examples` are not the callout labels.
        // `Example, a splitter` is still a sentence.
        let Some(rest) = lower.strip_prefix(word) else {
            return false;
        };
        rest.chars()
            .next()
            .is_none_or(|ch| !ch.is_ascii_alphabetic())
            && !rest.starts_with(',')
    })
}

fn line_in_figure(line: &VisualLine<'_>, unions: &[Rect], _body_font: &str) -> bool {
    if unions.is_empty()
        || line.toc
        || caption_kind(&line.text).is_some()
        || is_running_header(&line.text)
        || is_footer_line(&line.text)
        || is_page_folio_text(&line.text)
        || is_display_heading(line)
        || (is_body_shape(line) && !is_panel_label(&line.text))
        || starts_run_in(&line.text)
    {
        return false;
    }
    let total = line.glyphs.len();
    if total == 0 {
        return false;
    }
    for rect in unions {
        let inside = line
            .glyphs
            .iter()
            .filter(|glyph| {
                let x = (glyph.bbox[0] + glyph.bbox[2]) * 0.5;
                let y = (glyph.bbox[1] + glyph.bbox[3]) * 0.5;
                x >= rect.x0 - 6.0 && x <= rect.x1 + 6.0 && y >= rect.y0 - 6.0 && y <= rect.y1 + 6.0
            })
            .count();
        // An 11pt icon under a lead-in (`Below` over a bullet image) is not
        // the figure the sentence belongs to. A caption above a real drawing
        // is still inside that drawing.
        let tiny = rect.width() < 24.0 && rect.height() < 24.0;
        let above = !tiny
            && line.y > rect.y1
            && line.y <= rect.y1 + line.size * 2.4
            && line.left >= rect.x0 - 8.0
            && line.right <= rect.x1 + 24.0;
        // `(d) Following vs. ignoring summaries` sits just under panel (d),
        // outside the stroked box. It is still the figure's own label.
        let below = !tiny
            && line.y < rect.y0
            && rect.y0 - line.y <= line.size * 2.4
            && line.left >= rect.x0 - 8.0
            && line.right <= rect.x1 + 24.0;
        let panel = is_panel_label(&line.text)
            && line.y >= rect.y0 - line.size * 2.4
            && line.y <= rect.y1 + line.size * 2.4
            && line.left >= rect.x0 - 12.0
            && line.left <= rect.x1 + 8.0
            && line.right <= rect.x1 + 36.0;
        if inside * 2 <= total && !above && !below && !panel {
            continue;
        }
        let width = line.right - line.left;
        if above || below || panel || width <= rect.width() + 24.0 {
            return true;
        }
    }
    false
}

/// `(d) Following vs. ignoring summaries`. A figure panel's own title.
/// `Figure 3:` is a caption, not this label.
pub(crate) fn is_panel_label(text: &str) -> bool {
    let text = text.trim();
    let Some(rest) = text.strip_prefix('(') else {
        return false;
    };
    let Some(close) = rest.find(')') else {
        return false;
    };
    if !(1..=3).contains(&close) {
        return false;
    }
    let index = &rest[..close];
    if !index.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return false;
    }
    let label = rest[close + 1..].trim();
    !label.is_empty()
        && label.chars().count() <= 48
        && label.chars().any(|ch| ch.is_ascii_alphabetic())
}

fn caption_kind(text: &str) -> Option<&'static str> {
    let lower = text.trim().to_ascii_lowercase();
    if caption_prefix(&lower, &["figure", "fig.", "fig"]) {
        Some("figure")
    } else if caption_prefix(&lower, &["table", "tab.", "tab"]) {
        Some("table")
    } else {
        None
    }
}

/// `Table 2` and `TABLE II`. A following word such as `in` is not an index.
fn strip_caption_index(rest: &str) -> Option<&str> {
    if let Some(rest) = rest.strip_prefix(|ch: char| ch.is_ascii_digit()) {
        let rest = rest.trim_start_matches(|ch: char| ch.is_ascii_digit());
        return Some(rest);
    }
    let end = rest
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_alphabetic())
        .map(|(index, _)| index)
        .unwrap_or(rest.len());
    if end == 0 || end > 8 {
        return None;
    }
    if !is_roman_numeral(&rest[..end]) {
        return None;
    }
    Some(&rest[end..])
}

fn caption_prefix(lower: &str, prefixes: &[&str]) -> bool {
    for prefix in prefixes {
        let Some(rest) = lower.strip_prefix(prefix) else {
            continue;
        };
        if (*prefix == "fig" || *prefix == "tab")
            && rest.starts_with(|ch: char| ch.is_ascii_alphabetic())
        {
            continue;
        }
        let rest = rest.trim_start().trim_start_matches('.');
        let rest = rest.trim_start();
        let Some(rest) = strip_caption_index(rest) else {
            continue;
        };
        let rest = rest.trim_start();
        // "Figure 1:" and "Table 2." are captions. A long "Figure 1 Overview of …"
        // is too. "Figure 2 | The unified…" uses a bar. "Fig 1" and "Table 1 shows"
        // are ordinary sentences.
        if rest.starts_with([':', '.', '|']) {
            return true;
        }
        if rest.starts_with(|ch: char| ch.is_ascii_uppercase()) && lower.chars().count() >= 24 {
            return true;
        }
    }
    false
}

fn caption_block(start: usize, lines: &[VisualLine<'_>], order: &[usize]) -> Vec<usize> {
    let Some(pos) = order.iter().position(|index| *index == start) else {
        return vec![start];
    };
    let mut block = vec![start];
    let cap_w = (lines[start].right - lines[start].left).max(1.0);
    for &index in &order[pos + 1..] {
        if lines[index].page != lines[start].page {
            break;
        }
        // The other column sits between a caption and its wrap in reading
        // order. Treating that line as the end of the caption left the wrap
        // inside the figure, still in English. A second caption beside this
        // one (`Table 2` next to `Table 1`) is that other column, not the
        // next caption below.
        if caption_kind(&lines[index].text).is_some() {
            if x_overlaps(&lines[start], &lines[index]) {
                break;
            }
            continue;
        }
        if !x_overlaps(&lines[start], &lines[index]) {
            continue;
        }
        let prev = *block.last().unwrap_or(&start);
        if !continues_paragraph(&lines[prev], &lines[index], 0.0) {
            break;
        }
        let width = lines[index].right - lines[index].left;
        // `per-word perplexities.` and `of WSJ)` are the short last line of
        // a caption. They start in lowercase and the line above does not end
        // the sentence. A short cell under a finished caption still stops.
        let sentence_tail = !sentence_end(lines[prev].text.trim_end())
            && lines[index]
                .text
                .trim_start()
                .starts_with(|ch: char| ch.is_ascii_lowercase());
        if !sentence_tail && width < cap_w * 0.35 && lines[index].text.chars().count() < 24 {
            break;
        }
        block.push(index);
        if width + lines[index].size * 2.0 < cap_w {
            break;
        }
    }
    block
}

fn figure_side(
    block: &[usize],
    lines: &[VisualLine<'_>],
    body_font: &str,
    caption_line: &[bool],
    table: bool,
    claimed: &[bool],
) -> Vec<usize> {
    let page = lines[block[0]].page;
    let top = block
        .iter()
        .map(|index| lines[*index].y)
        .fold(f32::MIN, f32::max);
    let bottom = block
        .iter()
        .map(|index| lines[*index].y)
        .fold(f32::MAX, f32::min);
    let above = walk_interior(
        top,
        1.0,
        page,
        block,
        lines,
        body_font,
        caption_line,
        table,
        claimed,
    );
    let below = walk_interior(
        bottom,
        -1.0,
        page,
        block,
        lines,
        body_font,
        caption_line,
        table,
        claimed,
    );
    // A figure caption sits under the drawing. A table caption may sit above
    // the cells or below them. The adjacent side is the table. A later grid
    // can contain more numbers and is not this caption's interior.
    if table {
        let above_d = nearest_distance(&above, lines, top);
        let below_d = nearest_distance(&below, lines, bottom);
        let size = lines[block[0]].size.max(1.0);
        if above.is_empty() {
            return below;
        }
        if below.is_empty() {
            return above;
        }
        let above_grid = grid_line_count(&above, lines);
        let below_grid = grid_line_count(&below, lines);
        // A running head can sit closer than the grid. The grid still wins
        // when the other side has no rows. A second table further away does
        // not: that side has rows too, so the nearer side is chosen.
        if above_grid > 0 && below_grid == 0 && above_d < 160.0 {
            return above;
        }
        if below_grid > 0 && above_grid == 0 && below_d < 160.0 {
            return below;
        }
        if (above_d - below_d).abs() > size * 0.5 {
            return if above_d < below_d { above } else { below };
        }
        if above_grid != below_grid {
            return if above_grid > below_grid {
                above
            } else {
                below
            };
        }
    }
    if above.len() >= below.len() && !above.is_empty() {
        above
    } else {
        below
    }
}

fn nearest_distance(ids: &[usize], lines: &[VisualLine<'_>], origin: f32) -> f32 {
    ids.iter()
        .map(|index| (lines[*index].y - origin).abs())
        .fold(f32::MAX, f32::min)
}

/// A header or a numeric row. A callout title has neither a column gap nor
/// a pile of digits, so the tight band limit still stops in front of it.
fn is_table_row_line(line: &VisualLine<'_>) -> bool {
    let letters = line
        .text
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .count();
    let digits = line.text.chars().filter(|ch| ch.is_ascii_digit()).count();
    if column_gap(line) >= line.size.max(1.0) * 1.55 || (digits >= 2 && digits >= letters) {
        return true;
    }
    // `4K` and `512` are cells. `Pos.` and a prose wrap are not.
    let text = line.text.trim();
    let chars = text.chars().count();
    chars > 0 && chars <= 6 && digits >= 1 && letters <= 2
}

/// Lines that are a header or a numeric row, not a sentence beside the grid.
fn grid_line_count(ids: &[usize], lines: &[VisualLine<'_>]) -> usize {
    ids.iter()
        .filter(|&&index| is_table_row_line(&lines[index]))
        .count()
}

fn walk_interior(
    origin_y: f32,
    direction: f32,
    page: u32,
    block: &[usize],
    lines: &[VisualLine<'_>],
    body_font: &str,
    caption_line: &[bool],
    table: bool,
    claimed: &[bool],
) -> Vec<usize> {
    let anchor = &lines[block[0]];
    let mut span_left = block
        .iter()
        .map(|index| lines[*index].left)
        .fold(anchor.left, f32::min);
    let mut span_right = block
        .iter()
        .map(|index| lines[*index].right)
        .fold(anchor.right, f32::max);
    let mut cursor = origin_y;
    let mut picked = Vec::new();
    loop {
        let mut band: Vec<(usize, f32)> = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if line.page != page
                || block.contains(&index)
                || picked.contains(&index)
                || claimed.get(index).copied().unwrap_or(false)
            {
                continue;
            }
            let dy = (line.y - cursor) * direction;
            // The caption is often only as wide as "Table 2.1…". A bullet in
            // the example column starts further right and misses that box,
            // so the cell was translated. Once a row is in, later rows overlap
            // the table's accumulated span.
            let overlaps = if table && !picked.is_empty() {
                span_overlaps(span_left, span_right, line)
            } else {
                x_overlaps(anchor, line)
            };
            if dy <= 0.4 || !overlaps {
                continue;
            }
            band.push((index, dy));
        }
        if band.is_empty() {
            break;
        }
        band.sort_by(|a, b| a.1.total_cmp(&b.1));
        let nearest = band[0].1;
        // A blank band this wide is the next region, not more of the drawing.
        // A table's cells sit on consecutive leadings. A shaded callout a
        // few lines below the last cell (`Beyond Chain of Thought`) is the
        // next region, not another row. Figures keep the wider band.
        // The first row may sit well under a wrapped caption. Later rows
        // stay within about four leadings. A shaded callout 42pt under the
        // last 10.5pt row is the next region, not another cell. A wrapped
        // example row (`4K` then the next task) is further than that and
        // still a row, so a column gap keeps the wider limit.
        let size = lines[band[0].0].size.max(1.0);
        // A wrapped cell (`nesota to form…`) has no column gap. The `2K`
        // on the row above it is still this table, a little further up.
        let rowish = table
            && band
                .iter()
                .any(|(index, dy)| *dy <= size * 12.0 && is_table_row_line(&lines[*index]));
        let band_limit = if table && !picked.is_empty() {
            if rowish {
                size * 12.0
            } else {
                size * 4.05
            }
        } else {
            140.0
        };
        if nearest > band_limit || (cursor - origin_y).abs() > 420.0 {
            break;
        }
        let group: Vec<usize> = band
            .into_iter()
            .filter(|(_, dy)| *dy <= nearest + size * 0.5)
            .map(|(index, _)| index)
            .collect();
        let span_w = (span_right - span_left).max(anchor.right - anchor.left);
        let stopping: Vec<bool> = group
            .iter()
            .map(|&index| {
                stops_table_band(
                    &lines[index],
                    index,
                    lines,
                    body_font,
                    caption_line,
                    table,
                    span_w,
                )
            })
            .collect();
        if stopping.iter().any(|stop| *stop) {
            let rows: Vec<usize> = group
                .iter()
                .copied()
                .enumerate()
                .filter(|(slot, _)| !stopping[*slot])
                .map(|(_, index)| index)
                .collect();
            // `2K` sits on the same baseline as the example sentence. The
            // sentence is wide enough to look like the next paragraph, and
            // stopping there left the rows above it translated. A section
            // heading on that baseline is still the next region.
            let row_band = table && group.iter().any(|&index| is_table_row_line(&lines[index]));
            let hard_stop = group.iter().any(|&index| {
                let line = &lines[index];
                caption_kind(&line.text).is_some()
                    || is_running_header(&line.text)
                    || indexed_title_line(index, lines)
                    || (is_section_heading(line, body_font) && !short_row_label(line, span_w))
                    || (is_display_heading(line) && !short_row_label(line, span_w))
            });
            if row_band && !hard_stop {
                for index in &group {
                    picked.push(*index);
                    span_left = span_left.min(lines[*index].left);
                    span_right = span_right.max(lines[*index].right);
                }
                cursor = group
                    .iter()
                    .map(|&index| lines[index].y)
                    .fold(cursor, |acc, y| {
                        if direction > 0.0 {
                            acc.max(y)
                        } else {
                            acc.min(y)
                        }
                    });
                continue;
            }
            if !rows.is_empty() {
                for index in &rows {
                    picked.push(*index);
                }
                break;
            }
            // A one-line caption note (`the number of clusters was specified`)
            // sits between the caption and the header. The rows below it stay
            // cells. A paragraph that continues is the next section.
            if table
                && caption_note_before_a_row(
                    lines,
                    page,
                    &group,
                    direction,
                    span_left,
                    span_right,
                    body_font,
                    caption_line,
                    span_w,
                )
            {
                cursor = group
                    .iter()
                    .map(|&index| lines[index].y)
                    .fold(cursor, |acc, y| {
                        if direction > 0.0 {
                            acc.max(y)
                        } else {
                            acc.min(y)
                        }
                    });
                continue;
            }
            break;
        }
        for index in &group {
            picked.push(*index);
            span_left = span_left.min(lines[*index].left);
            span_right = span_right.max(lines[*index].right);
        }
        cursor = group
            .iter()
            .map(|&index| lines[index].y)
            .fold(cursor, |acc, y| {
                if direction > 0.0 {
                    acc.max(y)
                } else {
                    acc.min(y)
                }
            });
    }
    picked
}

/// A page running header sits above a figure on some pages. It is not a label
/// inside the drawing, so the interior walk stops instead of keeping it.
fn is_running_header(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    if lower == "preprint"
        || lower == "published"
        || lower.starts_with("published as ")
        || lower.starts_with("published in ")
        || lower.starts_with("under review")
        || lower.starts_with("proceedings of")
    {
        return true;
    }
    // `CHAPTER 1 Introduction…`. A page number may sit on the same line.
    // `In this chapter` is a sentence, not the running head.
    let rest = lower.trim_start_matches(|ch: char| ch.is_ascii_digit() || ch.is_whitespace());
    rest.starts_with("chapter ") || rest == "chapter"
}

/// A chapter title (`Introduction to AI`) is display type. A rule under it
/// can union with a drawing and the bounding box then covers the title.
/// Diagram labels are body size or smaller.
fn is_display_heading(line: &VisualLine<'_>) -> bool {
    let letters = line
        .text
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .count();
    // A 56pt slide title (`You!`) has fewer than four letters. A diagram
    // label at body size (`GPU`) does not.
    if line.size >= 28.0 && letters >= 2 {
        return true;
    }
    line.size >= 16.0 && letters >= 4
}

/// Both lines end on the same right edge and are display size. Body lines
/// share a left margin; a right-aligned title does not.
fn right_aligned_display_wrap(upper: &VisualLine<'_>, lower: &VisualLine<'_>) -> bool {
    let size = upper.size.max(lower.size);
    if size < 16.0 || (upper.size - lower.size).abs() > size * 0.05 {
        return false;
    }
    if !fonts_can_join(upper, lower) {
        return false;
    }
    (upper.right - lower.right).abs() <= 2.0 && (upper.left - lower.left).abs() > 8.0
}

/// `2.5` and `Reasoning in detail` share a baseline, but the gutter splits
/// them. The title is not `2.5 Reasoning…`, and `in` is lowercase, so neither
/// piece is a section heading. It still is not a table cell.
fn indexed_title_line(index: usize, lines: &[VisualLine<'_>]) -> bool {
    let line = &lines[index];
    let text = line.text.trim();
    if dotted_index_only(text) {
        return lines.iter().enumerate().any(|(other, title)| {
            other != index
                && title.page == line.page
                && title.size >= 11.0
                && baseline_neighbors(line, title)
                && title.left + 2.0 >= line.right
                && short_heading_title(&title.text)
        });
    }
    line.size >= 11.0
        && short_heading_title(text)
        && lines.iter().enumerate().any(|(other, index_line)| {
            other != index
                && index_line.page == line.page
                && baseline_neighbors(index_line, line)
                && index_line.right <= line.left + 2.0
                && dotted_index_only(&index_line.text)
        })
}

fn baseline_neighbors(left: &VisualLine<'_>, right: &VisualLine<'_>) -> bool {
    let size = left.size.max(right.size).max(1.0);
    (left.y - right.y).abs() <= size * 0.35 && right.left - left.right <= size * 3.0
}

fn short_heading_title(text: &str) -> bool {
    let text = text.trim();
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() || words.len() > 8 || text.contains([',', ';', ':']) {
        return false;
    }
    if text.ends_with(['.', '?', '!', '。']) {
        return false;
    }
    text.chars().filter(|ch| ch.is_alphabetic()).count() >= 4
}

/// `2.5` and `2.5.1`. A bare chapter number is not this mark.
fn dotted_index_only(text: &str) -> bool {
    let text = text.trim().trim_end_matches('.');
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let mut index = 0;
    let mut groups = 0;
    loop {
        if groups > 0 {
            if index >= bytes.len() || bytes[index] != b'.' {
                break;
            }
            index += 1;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index == start || index - start > 2 {
            return false;
        }
        groups += 1;
        if groups > 4 {
            return false;
        }
        if index >= bytes.len() {
            break;
        }
    }
    groups >= 2 && index == bytes.len()
}

/// A table walk keeps every cell, header, and row label. A numeric row used
/// to look like a contents entry (`512` across a column gap), and the whole
/// grid after it was translated. A prose sentence under the grid still stops
/// the walk. A cell with a column gap is not that sentence.
/// The barrier is a caption footnote, and a real row (a column gap) follows
/// before another sentence.
fn caption_note_before_a_row(
    lines: &[VisualLine<'_>],
    page: u32,
    group: &[usize],
    direction: f32,
    span_left: f32,
    span_right: f32,
    body_font: &str,
    caption_line: &[bool],
    span_w: f32,
) -> bool {
    let note_y = group.iter().map(|&index| lines[index].y).fold(
        if direction > 0.0 { f32::MIN } else { f32::MAX },
        |acc, y| {
            if direction > 0.0 {
                acc.max(y)
            } else {
                acc.min(y)
            }
        },
    );
    let size = lines[group[0]].size.max(1.0);
    let mut saw_row = false;
    for (index, line) in lines.iter().enumerate() {
        if line.page != page || group.contains(&index) {
            continue;
        }
        let dy = (line.y - note_y) * direction;
        if dy <= 0.4 || dy > size * 5.0 || !span_overlaps(span_left, span_right, line) {
            continue;
        }
        if stops_table_band(line, index, lines, body_font, caption_line, true, span_w) {
            return false;
        }
        if column_gap(line) >= line.size.max(1.0) * 1.55 {
            saw_row = true;
        }
    }
    saw_row
}

fn stops_table_band(
    line: &VisualLine<'_>,
    index: usize,
    lines: &[VisualLine<'_>],
    body_font: &str,
    caption_line: &[bool],
    table: bool,
    span_w: f32,
) -> bool {
    // `3 Bits` is a row stub. `5.3 Optimizer` is the next section.
    let row_label = table && short_row_label(line, span_w);
    if caption_line.get(index).copied().unwrap_or(false)
        || caption_kind(&line.text).is_some()
        || is_running_header(&line.text)
        || (!row_label && is_display_heading(line))
        || indexed_title_line(index, lines)
        || (!row_label && is_section_heading(line, body_font))
    {
        return true;
    }
    if !table {
        return stops_interior(line, body_font);
    }
    // `512` across a column gap is not a contents row, and
    // `positional embedding instead of sinusoids` is a cell whose numbers
    // sit in the next column. A full-measure sentence under the grid stops.
    if line.toc || column_gap(line) >= line.size.max(1.0) * 1.55 {
        return false;
    }
    let width = line.right - line.left;
    let full_measure = width >= span_w.max(1.0) * 0.82;
    full_measure && (is_body_barrier(line, body_font) || starts_run_in(&line.text))
}

/// `3 Bits` and `2 Bits` share the body face, so they look like `1 Introduction`.
/// A row stub is one short word and much narrower than the grid. A real
/// heading (`1 Introduction`, `5.3 Optimizer`) is not.
fn short_row_label(line: &VisualLine<'_>, span_w: f32) -> bool {
    let width = line.right - line.left;
    if width >= span_w.max(1.0) * 0.5 {
        return false;
    }
    let Some((dotted, rest)) = section_heading_index(line.text.trim()) else {
        return false;
    };
    if dotted {
        return false;
    }
    let words: Vec<&str> = rest.split_whitespace().collect();
    let letters = rest.chars().filter(|ch| ch.is_alphabetic()).count();
    words.len() == 1 && letters > 0 && letters <= 6
}

/// The widest hole between ink on one baseline. A table column is wider than
/// a word space.
fn column_gap(line: &VisualLine<'_>) -> f32 {
    let mut glyphs: Vec<&Glyph> = line.glyphs.to_vec();
    glyphs.sort_by(|left, right| glyph_left(left).total_cmp(&glyph_left(right)));
    let mut best = 0.0f32;
    let mut prev_right: Option<f32> = None;
    for glyph in glyphs {
        if glyph.unicode.trim().is_empty() {
            continue;
        }
        let left = glyph_left(glyph);
        if let Some(prev) = prev_right {
            best = best.max(left - prev);
        }
        let right = glyph_right(glyph);
        prev_right = Some(prev_right.map(|prev| prev.max(right)).unwrap_or(right));
    }
    best
}

fn stops_interior(line: &VisualLine<'_>, body_font: &str) -> bool {
    // `DEFINITION Retrieval-Augmented…` uses a label face, so it is not a
    // body barrier, and the walk kept that first line inside the figure.
    // A wide line in another face is still a diagram annotation; stopping
    // on width alone cut the sample paper's page-break sentence.
    line.toc
        || caption_kind(&line.text).is_some()
        || is_body_barrier(line, body_font)
        || is_section_heading(line, body_font)
        || starts_run_in(&line.text)
}

/// Titles on a contents row are translated even when a table or a box
/// claimed them. Leaders and page numbers are not on these lines.
/// A numeric cell under a real table caption is not a contents title.
/// `Table 1: Practices …… 5` still releases its title.
fn toc_line_is_table_cell(line: &VisualLine<'_>, lines: &[VisualLine<'_>]) -> bool {
    if caption_kind(&line.text).is_some()
        || !(under_table_caption(line, lines) || above_table_caption(line, lines))
    {
        return false;
    }
    if line.text.chars().filter(|ch| is_leader_dot(*ch)).count() >= 4 {
        return false;
    }
    let letters = line
        .text
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .count();
    let digits = line.text.chars().filter(|ch| ch.is_ascii_digit()).count();
    column_gap(line) >= line.size.max(1.0) * 1.55 || digits > letters || letters <= 6
}

fn release_toc_titles(lines: &[VisualLine<'_>], kept: &mut Vec<(u32, String)>) {
    let mut titles = crate::detmap::HashSet::default();
    for line in lines {
        if !line.toc || toc_line_is_table_cell(line, lines) {
            continue;
        }
        for glyph in &line.glyphs {
            titles.insert(glyph.id);
        }
    }
    if titles.is_empty() {
        return;
    }
    kept.retain(|(id, reason)| !(titles.contains(id) && (reason == "table" || reason == "figure")));
}
/// Notes under a kept table: each starts with a raised one-letter marker
/// (`a`, `b`, `*`, `†`) and sits directly below the grid or the note above.
/// A wrapped note line in the same type continues its note.
fn table_note_glyphs(lines: &[VisualLine<'_>], kept: &[(u32, String)]) -> Vec<(u32, String)> {
    let table: crate::detmap::HashSet<u32> = kept
        .iter()
        .filter(|(_, reason)| reason == "table")
        .map(|(id, _)| *id)
        .collect();
    if table.is_empty() {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..lines.len()).collect();
    order.sort_by(|&a, &b| {
        lines[a]
            .page
            .cmp(&lines[b].page)
            .then(lines[b].y.total_cmp(&lines[a].y))
    });
    let mut out = Vec::new();
    // Table rows and note lines seen so far: page, y, left, right, size, note.
    let mut anchors: Vec<(u32, f32, f32, f32, f32, bool)> = Vec::new();
    for &index in &order {
        let line = &lines[index];
        if line.glyphs.iter().any(|g| table.contains(&g.id)) {
            anchors.push((line.page, line.y, line.left, line.right, line.size, false));
            continue;
        }
        let below = anchors
            .iter()
            .rev()
            .find(|(page, y, left, right, size, _)| {
                *page == line.page
                    && line.y < *y
                    && *y - line.y <= 3.0 * size.max(line.size)
                    && line.left < *right
                    && line.right > *left
            });
        let Some(&(_, _, _, _, anchor_size, after_note)) = below else {
            continue;
        };
        let wrap = after_note && (line.size - anchor_size).abs() <= anchor_size * 0.1;
        if !table_note_marker(line) && !wrap {
            continue;
        }
        out.extend(line.glyphs.iter().map(|g| (g.id, "table".to_string())));
        anchors.push((line.page, line.y, line.left, line.right, line.size, true));
    }
    out
}

fn table_note_marker(line: &VisualLine<'_>) -> bool {
    let mut ink = line.glyphs.iter().filter(|g| !g.unicode.trim().is_empty());
    let (Some(first), Some(second)) = (ink.next(), ink.next()) else {
        return false;
    };
    let mut chars = first.unicode.chars();
    let (Some(ch), None) = (chars.next(), chars.next()) else {
        return false;
    };
    let symbol = ch.is_ascii_lowercase() || matches!(ch, '*' | '†' | '‡' | '§' | '¶');
    let raised = first.matrix[5] - second.matrix[5] > line.size * 0.15;
    symbol && raised && second.matrix[4] > first.matrix[4]
}

/// `3: for H ∈ P in parallel do` is a numbered step of an algorithm block.
/// `Require:` and `Data:` open one. algorithm2e numbers without the colon
/// (`1 while c ≥ 0 do`); such a step only counts next to an opening key.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AlgorithmLine {
    Key,
    Numbered,
    Plain,
}

fn algorithm_step(text: &str) -> Option<AlgorithmLine> {
    let text = text.trim_start();
    for key in [
        "Require:", "Ensure:", "Input:", "Output:", "Data:", "Result:",
    ] {
        if text.starts_with(key) {
            return Some(AlgorithmLine::Key);
        }
    }
    let digits = text.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if !(1..=3).contains(&digits) {
        return None;
    }
    let rest = &text[digits..];
    if rest.starts_with(':') && rest[1..].chars().next().is_none_or(char::is_whitespace) {
        return Some(AlgorithmLine::Numbered);
    }
    if rest.trim().is_empty() {
        return Some(AlgorithmLine::Plain);
    }
    let word = rest.trim_start();
    (rest.starts_with(char::is_whitespace)
        && word
            .chars()
            .next()
            .is_some_and(|ch| !ch.is_uppercase() && !ch.is_ascii_digit()))
    .then_some(AlgorithmLine::Plain)
}

/// An algorithm block stays as drawn: code, not prose. Its numbered steps
/// and the wraps between them are kept; the `Algorithm 1 …` caption is not.
fn algorithm_glyphs(lines: &[VisualLine<'_>]) -> Vec<(u32, String)> {
    struct Block {
        page: u32,
        top: f32,
        bottom: f32,
        left: f32,
        right: f32,
        size: f32,
        numbered: usize,
        keys: usize,
        plain: usize,
        lines: usize,
    }
    let mut steps: Vec<(&VisualLine<'_>, AlgorithmLine)> = lines
        .iter()
        .filter_map(|line| algorithm_step(&line.text).map(|kind| (line, kind)))
        .collect();
    steps.sort_by(|a, b| a.0.page.cmp(&b.0.page).then(b.0.y.total_cmp(&a.0.y)));
    let mut blocks: Vec<Block> = Vec::new();
    for (line, kind) in steps {
        let near = blocks.iter_mut().rev().find(|block| {
            block.page == line.page
                && line.left < block.right + 2.0 * block.size
                && line.right > block.left - 2.0 * block.size
                && block.bottom - line.y <= 4.0 * block.size.max(line.size)
        });
        match near {
            Some(block) => {
                block.bottom = block.bottom.min(line.y);
                block.left = block.left.min(line.left);
                block.right = block.right.max(line.right);
                block.numbered += usize::from(kind == AlgorithmLine::Numbered);
                block.keys += usize::from(kind == AlgorithmLine::Key);
                block.plain += usize::from(kind == AlgorithmLine::Plain);
                block.lines += 1;
            }
            None => blocks.push(Block {
                page: line.page,
                top: line.y,
                bottom: line.y,
                left: line.left,
                right: line.right,
                size: line.size,
                numbered: usize::from(kind == AlgorithmLine::Numbered),
                keys: usize::from(kind == AlgorithmLine::Key),
                plain: usize::from(kind == AlgorithmLine::Plain),
                lines: 1,
            }),
        }
    }
    let mut out = Vec::new();
    for block in blocks.iter().filter(|block| {
        block.lines >= 3 && (block.numbered >= 2 || (block.keys >= 1 && block.plain >= 2))
    }) {
        for line in lines {
            let inside = line.page == block.page
                && line.y <= block.top + block.size * 0.5
                && line.y >= block.bottom - block.size * 0.5
                && line.left >= block.left - block.size
                && line.left < block.right;
            if inside && !line.text.trim_start().starts_with("Algorithm") {
                if std::env::var_os("RPT_ALG_DEBUG").is_some() {
                    eprintln!("ALG p{} {}", line.page, line.text);
                }
                out.extend(line.glyphs.iter().map(|g| (g.id, "algorithm".to_string())));
            }
        }
    }
    out
}

/// A text-only table caught under its caption by the cells that overlap the
/// caption. The first and last columns sit wider than the caption, so they
/// were translated while the middle columns stayed. The table's own rules
/// span every column: a line between them, inside their width, is a cell.
fn ruled_table_rows(
    lines: &[VisualLine<'_>],
    kept: &[(u32, String)],
    regions: &[PaintedRegion],
) -> Vec<(u32, String)> {
    let table_ids: crate::detmap::HashSet<u32> = kept
        .iter()
        .filter(|(_, reason)| reason == "table")
        .map(|(id, _)| *id)
        .collect();
    if table_ids.is_empty() {
        return Vec::new();
    }
    let all_ids: crate::detmap::HashSet<u32> = kept.iter().map(|(id, _)| *id).collect();
    let rules: Vec<&PaintedRegion> = regions
        .iter()
        .filter(|region| {
            region.kind == "path"
                && (region.bbox[3] - region.bbox[1]).abs() < 1.5
                && region.bbox[2] - region.bbox[0] >= 100.0
        })
        .collect();
    let mut out = Vec::new();
    let mut done = crate::detmap::HashSet::default();
    for seed in lines {
        if !seed
            .glyphs
            .iter()
            .all(|glyph| table_ids.contains(&glyph.id))
        {
            continue;
        }
        let page_rules: Vec<&&PaintedRegion> = rules
            .iter()
            .filter(|rule| {
                rule.page_index == seed.page
                    && rule.bbox[0] <= seed.left + 1.0
                    && rule.bbox[2] >= seed.right - 1.0
            })
            .collect();
        let reach = seed.size * 3.0;
        let (mut lo, mut hi) = (seed.y, seed.y);
        let (mut left, mut right) = (f32::MAX, f32::MIN);
        let mut used = vec![false; page_rules.len()];
        loop {
            let mut grew = false;
            for (index, rule) in page_rules.iter().enumerate() {
                let y = rule.bbox[1];
                if used[index] || y < lo - reach || y > hi + reach {
                    continue;
                }
                used[index] = true;
                lo = lo.min(y);
                hi = hi.max(y);
                left = left.min(rule.bbox[0]);
                right = right.max(rule.bbox[2]);
                grew = true;
            }
            // The kept cells carry the band down to the bottom rule.
            for line in lines {
                if line.page != seed.page
                    || line.y >= lo - reach * 0.5 && line.y <= hi + reach * 0.5
                    || line.y < lo - reach
                    || line.y > hi + reach
                    || !line
                        .glyphs
                        .iter()
                        .all(|glyph| table_ids.contains(&glyph.id))
                {
                    continue;
                }
                lo = lo.min(line.y);
                hi = hi.max(line.y);
                grew = true;
            }
            if !grew {
                break;
            }
        }
        let above = used
            .iter()
            .zip(&page_rules)
            .any(|(u, rule)| *u && rule.bbox[1] > seed.y);
        let below = used
            .iter()
            .zip(&page_rules)
            .any(|(u, rule)| *u && rule.bbox[1] < seed.y);
        if !above || !below || !done.insert((seed.page, lo.to_bits(), hi.to_bits())) {
            continue;
        }
        for line in lines {
            if line.page != seed.page
                || line.y <= lo
                || line.y >= hi
                || line.left < left - 1.0
                || line.right > right + 1.0
                || caption_kind(&line.text).is_some()
                // A caption's wrap or a paragraph inside the rules runs most
                // of their width. A cell is a column's share of it.
                || line.right - line.left > (right - left) * 0.4
            {
                continue;
            }
            for glyph in &line.glyphs {
                if !all_ids.contains(&glyph.id) {
                    out.push((glyph.id, "table".to_string()));
                }
            }
        }
    }
    out
}

/// A table whose cells are all words sits between three or more rules of one
/// width, and most of its baselines carry two or more cells apart by a gap.
/// A paragraph between rules has one line per baseline.
fn worded_ruled_tables(
    lines: &[VisualLine<'_>],
    kept: &[(u32, String)],
    regions: &[PaintedRegion],
) -> Vec<(u32, String)> {
    let all_ids: crate::detmap::HashSet<u32> = kept.iter().map(|(id, _)| *id).collect();
    let mut rules: Vec<&PaintedRegion> = regions
        .iter()
        .filter(|region| {
            region.kind == "path"
                && (region.bbox[3] - region.bbox[1]).abs() < 1.5
                && region.bbox[2] - region.bbox[0] >= 100.0
        })
        .collect();
    rules.sort_by(|a, b| {
        (a.page_index, -a.bbox[1])
            .partial_cmp(&(b.page_index, -b.bbox[1]))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut out = Vec::new();
    let mut start = 0;
    while start < rules.len() {
        let first = rules[start];
        let same = |rule: &PaintedRegion| {
            rule.page_index == first.page_index
                && (rule.bbox[0] - first.bbox[0]).abs() < 2.0
                && (rule.bbox[2] - first.bbox[2]).abs() < 2.0
                && first.bbox[1] - rule.bbox[1] <= 350.0
        };
        let mut end = start + 1;
        while end < rules.len() && same(rules[end]) {
            end += 1;
        }
        let count = end - start;
        let (top, bottom) = (first.bbox[1], rules[end - 1].bbox[1]);
        let (left, right) = (first.bbox[0], first.bbox[2]);
        start = end;
        if count < 3 {
            continue;
        }
        let inside: Vec<&VisualLine<'_>> = lines
            .iter()
            .filter(|line| {
                line.page == first.page_index
                    && line.y < top
                    && line.y > bottom
                    && line.left >= left - 1.0
                    && line.right <= right + 1.0
                    && caption_kind(&line.text).is_none()
                    && !continues_caption(line, lines)
            })
            .collect();
        let mut baselines: Vec<Vec<&VisualLine<'_>>> = Vec::new();
        for line in &inside {
            match baselines
                .iter_mut()
                .find(|row| (row[0].y - line.y).abs() < line.size * 0.3)
            {
                Some(row) => row.push(line),
                None => baselines.push(vec![line]),
            }
        }
        for row in &mut baselines {
            row.sort_by(|a, b| {
                a.left
                    .partial_cmp(&b.left)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        let celled = baselines
            .iter()
            .filter(|row| {
                row.windows(2)
                    .any(|pair| pair[1].left - pair[0].right >= pair[0].size * 2.0)
            })
            .count();
        if celled < 3 || celled * 2 < baselines.len() {
            continue;
        }
        for line in inside {
            for glyph in &line.glyphs {
                if !all_ids.contains(&glyph.id) {
                    out.push((glyph.id, "table".to_string()));
                }
            }
        }
    }
    out
}

/// The line wraps a caption: a chain of left-aligned lines one leading
/// apart leads up to a caption line.
fn continues_caption(line: &VisualLine<'_>, lines: &[VisualLine<'_>]) -> bool {
    let mut current = line;
    for _ in 0..6 {
        let Some(above) = lines.iter().find(|other| {
            other.page == current.page
                && (other.left - current.left).abs() < current.size
                && other.y > current.y
                && other.y - current.y < current.size * 1.8
        }) else {
            return false;
        };
        if caption_kind(&above.text).is_some() {
            return true;
        }
        current = above;
    }
    false
}

fn release_table_group_labels(lines: &[VisualLine<'_>], kept: &mut Vec<(u32, String)>) {
    let mut release = crate::detmap::HashSet::default();
    for line in lines {
        // `(d) Following vs. ignoring summaries` is bold and sits under the
        // table caption on the same page. It is the figure's own title.
        if is_panel_label(&line.text) {
            continue;
        }
        if !line.sole_on_baseline || !under_table_caption(line, lines) {
            continue;
        }
        let letters = line
            .text
            .chars()
            .filter(|ch| ch.is_ascii_alphabetic())
            .count();
        let digits = line.text.chars().filter(|ch| ch.is_ascii_digit()).count();
        let words = line.text.split_whitespace().count();
        if digits != 0 || letters < 12 || !(1..=8).contains(&words) {
            continue;
        }
        let emphasized = line.glyphs.iter().any(|glyph| {
            let style = crate::font::face_style(&glyph.font_name);
            style.bold || crate::font::medium_weight(&glyph.font_name)
        });
        if !emphasized {
            continue;
        }
        // `Demonstration  Objective  Technique (model)` is a header row. A
        // group label has no other bold cell on its baseline. A bold run-in
        // has plain body text beside it and is still released.
        let run_in = line.text.trim_end().ends_with(['.', ':']);
        let row = !run_in
            && lines.iter().any(|other| {
                !std::ptr::eq(other, line)
                    && other.page == line.page
                    && (other.y - line.y).abs() < line.size * 0.3
                    && (other.left >= line.right + line.size * 2.0
                        || other.right + line.size * 2.0 <= line.left)
                    && other.glyphs.iter().all(|glyph| {
                        let style = crate::font::face_style(&glyph.font_name);
                        style.bold || crate::font::medium_weight(&glyph.font_name)
                    })
            });
        if row {
            continue;
        }
        for glyph in &line.glyphs {
            release.insert(glyph.id);
        }
    }
    if release.is_empty() {
        return;
    }
    kept.retain(|(id, reason)| {
        !(release.contains(id) && (reason == "table" || reason == "figure"))
    });
}

fn x_overlaps(anchor: &VisualLine<'_>, line: &VisualLine<'_>) -> bool {
    span_overlaps(anchor.left, anchor.right, line)
}

fn span_overlaps(left: f32, right: f32, line: &VisualLine<'_>) -> bool {
    let overlap_left = left.max(line.left);
    let overlap_right = right.min(line.right);
    let overlap = (overlap_right - overlap_left).max(0.0);
    let narrow = (right - left).min(line.right - line.left).max(1.0);
    overlap >= narrow * 0.3
}

fn dominant_body_font(lines: &[VisualLine<'_>], page: u32) -> String {
    let mut counts: crate::detmap::HashMap<&str, usize> = crate::detmap::HashMap::default();
    let mut fallback: crate::detmap::HashMap<&str, usize> = crate::detmap::HashMap::default();
    for line in lines.iter().filter(|line| line.page == page) {
        if !is_body_shape(line) {
            continue;
        }
        *fallback.entry(line.font.as_str()).or_insert(0) += line.text.len();
        // A long cell under `Table 2.1` is Franklin, and the paragraph above
        // is New Baskerville. Counting the cell made Franklin the body face,
        // so the paragraph was kept and the cell was translated.
        if under_table_caption(line, lines) {
            continue;
        }
        *counts.entry(line.font.as_str()).or_insert(0) += line.text.len();
    }
    if counts.is_empty() {
        counts = fallback;
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(font, _)| font.to_string())
        .unwrap_or_default()
}

/// A cell, bullet, or wrapped row sitting under a table caption. The example
/// column starts to the right of a short caption, so the test is wider than
/// the caption's own box.
fn under_table_caption(line: &VisualLine<'_>, lines: &[VisualLine<'_>]) -> bool {
    table_caption_near(line, lines, true)
}

/// A numeric cell above a caption that sits under its grid. A paragraph above
/// that caption is not a cell, so the body face is still taken from it.
fn above_table_caption(line: &VisualLine<'_>, lines: &[VisualLine<'_>]) -> bool {
    table_caption_near(line, lines, false)
}

fn table_caption_near(line: &VisualLine<'_>, lines: &[VisualLine<'_>], below: bool) -> bool {
    lines.iter().any(|caption| {
        let dy = if below {
            caption.y - line.y
        } else {
            line.y - caption.y
        };
        caption_kind(&caption.text) == Some("table")
            && caption.page == line.page
            && (1.0..=320.0).contains(&dy)
            && line.left < caption.right + 240.0
            && line.right > caption.left - 16.0
    })
}

fn is_body_shape(line: &VisualLine<'_>) -> bool {
    let letters = line.text.chars().filter(|ch| ch.is_alphabetic()).count();
    // The last line of a paragraph is shorter than the lines above it. 24
    // letters still beats a figure label, which is a few words in a narrow box.
    letters >= 24 && line.right - line.left >= 160.0
}

fn is_body_barrier(line: &VisualLine<'_>, body_font: &str) -> bool {
    if !is_body_shape(line) {
        return false;
    }
    // Medium and regular are the same text face. A running header set in
    // Medi must stop a figure walk the same way a Regu body line does.
    body_font.is_empty() || same_font_family(&line.font, body_font)
}

/// "1 Introduction" is a section heading, not a label inside a figure.
/// "5.3 Qualitative Results" is too: the minor number used to leave the
/// heading, and the paragraph under it, inside the table.
fn is_section_heading(line: &VisualLine<'_>, body_font: &str) -> bool {
    let text = line.text.trim();
    if text.chars().count() > 48 || line.right - line.left > 260.0 {
        return false;
    }
    let Some((dotted, rest)) = section_heading_index(text) else {
        return false;
    };
    if rest.chars().filter(|ch| ch.is_alphabetic()).count() < 4 {
        return false;
    }
    // A diagram token in a display face is not a section. A bold title is:
    // LaTeX sets `1 Introduction` in CMBX under the caption while the body
    // stays CMR. A sentence in that face (`1 When the task reaches…`) is a
    // callout inside the drawing. A dotted heading is bold too.
    if !dotted
        && !body_font.is_empty()
        && line.font != body_font
        && !same_font_family(&line.font, body_font)
        && !(bold_face(&line.font) && title_case_heading(rest))
    {
        return false;
    }
    true
}

/// `(true, rest)` when the index has a minor number (`5.3`, `5.3.1`).
fn section_heading_index(text: &str) -> Option<(bool, &str)> {
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut groups = 0;
    loop {
        if groups > 0 {
            if index >= bytes.len() || bytes[index] != b'.' {
                break;
            }
            let after_dot = index + 1;
            if after_dot >= bytes.len() || !bytes[after_dot].is_ascii_digit() {
                break;
            }
            index = after_dot;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index == start || index - start > 2 {
            return None;
        }
        groups += 1;
        if groups > 4 {
            return None;
        }
    }
    if groups == 0 {
        return None;
    }
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
    }
    if index >= bytes.len() || bytes[index] != b' ' {
        return None;
    }
    Some((groups >= 2, text[index..].trim()))
}

fn reading_order(lines: &[VisualLine<'_>]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..lines.len()).collect();
    order.sort_by(|a, b| {
        lines[*a]
            .page
            .cmp(&lines[*b].page)
            .then(lines[*b].y.total_cmp(&lines[*a].y))
            .then(lines[*a].left.total_cmp(&lines[*b].left))
    });
    order
}

fn margin_stamps(lines: &[VisualLine<'_>]) -> Vec<(u32, String)> {
    let mut kept = Vec::new();
    let mut seen = crate::detmap::HashSet::default();
    for line in lines {
        // A stamp that stayed on its own baseline: one or two large glyphs at the left edge.
        if line.left <= 56.0
            && line.glyphs.len() <= 2
            && line.right - line.left <= 22.0
            && line.size >= 14.0
        {
            for glyph in &line.glyphs {
                if seen.insert(glyph.id) {
                    kept.push((glyph.id, "margin".into()));
                }
            }
        }
    }
    // The same stamp often shares a baseline with the body. The line is then too
    // wide to match above, and the size-20 glyph raises the column-split gap so
    // the letter stays inside the paragraph. A real margin is many large glyphs
    // stacked on one x.
    let mut by_page: crate::detmap::HashMap<u32, Vec<&Glyph>> = crate::detmap::HashMap::default();
    for line in lines {
        for glyph in &line.glyphs {
            if stamp_glyph(glyph) {
                by_page.entry(glyph.page_index).or_default().push(glyph);
            }
        }
    }
    for glyphs in by_page.values() {
        if glyphs.len() < 8 {
            continue;
        }
        let mut xs: Vec<f32> = glyphs.iter().map(|glyph| glyph.matrix[4]).collect();
        xs.sort_by(|a, b| a.total_cmp(b));
        let anchor = xs[xs.len() / 2];
        for glyph in glyphs {
            if (glyph.matrix[4] - anchor).abs() > 4.0 || !seen.insert(glyph.id) {
                continue;
            }
            // The first letter of a long centered line can sit on one x.
            // It is the start of the word, not a stamp, when the next letter
            // is on the same baseline.
            if word_continues_right(glyph, lines) {
                seen.remove(&glyph.id);
                continue;
            }
            kept.push((glyph.id, "margin".into()));
        }
    }
    kept
}

/// The next letter of this word, within about an em on the same baseline.
fn word_continues_right(glyph: &Glyph, lines: &[VisualLine<'_>]) -> bool {
    let size = glyph.font_size.max(1.0);
    for line in lines {
        if line.page != glyph.page_index || (line.y - glyph.matrix[5]).abs() > size * 0.45 {
            continue;
        }
        for other in &line.glyphs {
            if other.id == glyph.id {
                continue;
            }
            let dx = other.matrix[4] - glyph.matrix[4];
            if dx > 0.3 && dx <= size * 1.25 && other.unicode.chars().any(|ch| ch.is_alphanumeric())
            {
                return true;
            }
        }
    }
    false
}

fn stamp_glyph(glyph: &Glyph) -> bool {
    let left = glyph.matrix[4].min(glyph.bbox[0]).min(glyph.bbox[2]);
    let right = glyph.bbox[0].max(glyph.bbox[2]).max(glyph.matrix[4]);
    left <= 56.0 && right - left <= 24.0 && glyph.font_size >= 14.0
}

fn big_delimiter_line(line: &VisualLine<'_>) -> bool {
    !line.glyphs.is_empty()
        && line.glyphs.len() <= 2
        && line
            .glyphs
            .iter()
            .all(|glyph| glyph.font_name.to_ascii_uppercase().contains("CMEX"))
}

fn join_paragraphs(lines: Vec<VisualLine<'_>>) -> Vec<Vec<VisualLine<'_>>> {
    // A left-edge stamp between two body baselines must not split the paragraph.
    // Join each column on its own. The anchor is that column's median left edge,
    // so a chain of small indents cannot pull the other column in.
    let measures = page_measures(&lines);
    let mut columns: Vec<Vec<VisualLine>> = Vec::new();
    // A display delimiter split off a heading (`(` at x 99) is no margin.
    // As a column it took the hanging bullet line `histories, …` and cut the
    // bullet in two. It stays a paragraph of its own.
    let mut loose: Vec<Vec<VisualLine>> = Vec::new();
    for line in lines {
        if big_delimiter_line(&line) {
            loose.push(vec![line]);
            continue;
        }
        // A math gap can park the hyphen on the right half of the baseline.
        // That fragment has to claim the next line before the left-hand piece,
        // which shares the body margin, takes it.
        let hyphen_slot = columns
            .iter()
            .position(|column| column.last().is_some_and(|prev| hyphen_pull(prev, &line)));
        // A full line that happens to end in a hyphen can sit within 64pt of
        // an earlier, narrower column. The first match used to claim it
        // (`thou-` left the definition and joined a label column). Prefer the
        // closest margin that actually fits.
        // The last short line of a centered list (`Matrix.org Foundation`)
        // shares the title's left edge. That match used to claim it before
        // the comma-list test, so the line was translated on its own and the
        // topics above it stayed a different paragraph.
        let list_slot = columns.iter().position(|column| {
            column
                .last()
                .is_some_and(|prev| centered_comma_continuation(prev, &line))
        });
        // `scholarly works.` is centered under the permission notice. Its left
        // edge is far from the full lines, so the margin test starts a new
        // paragraph and the translation keeps that short tail.
        let prose_slot = columns.iter().position(|column| {
            column
                .last()
                .is_some_and(|prev| centered_prose_continuation(prev, &line))
        });
        // `2.4 Pipeline: From … CRS` / `Training Data`: the wrap hangs under
        // the title words, which can be closer to another column's margin.
        let heading_slot = columns.iter().position(|column| {
            column.last().is_some_and(|prev| {
                let size = prev.size.max(line.size).max(1.0);
                let dy = prev.y - line.y;
                prev.page == line.page
                    && (size * 0.7..=size * 1.5).contains(&dy)
                    && (prev.size - line.size).abs() <= size * 0.2
                    && section_number_body(&prev.text).is_some()
                    && hanging_numbered_continuation(prev, &line)
            })
        });
        let slot = hyphen_slot
            .or(heading_slot)
            .or(list_slot)
            .or(prose_slot)
            .or_else(|| {
                let mut best: Option<(usize, f32)> = None;
                for (index, column) in columns.iter().enumerate() {
                    let Some(anchor) = column_anchor(column, line.page) else {
                        continue;
                    };
                    let distance = (anchor - line.left).abs();
                    // A wrapped word can start a list-indented line and finish
                    // back on the column margin. That line sits to the right of
                    // the margin. A body line to the left of a centered title is
                    // a different column, even when a hyphen makes the gap look
                    // like an indent.
                    let indented = line.left + 1.0 >= anchor;
                    let fits = distance <= 28.0
                        || (distance <= 64.0 && indented && soft_hyphen_stem(&line.text).is_some());
                    if fits && best.is_none_or(|(_, previous)| distance < previous) {
                        best = Some((index, distance));
                    }
                }
                best.map(|(index, _)| index).or_else(|| {
                    columns.iter().position(|column| {
                        column.last().is_some_and(|prev| {
                            hanging_pull(prev, &line) || right_aligned_display_wrap(prev, &line)
                        })
                    })
                })
            });
        if let Some(slot) = slot {
            columns[slot].push(line);
        } else {
            columns.push(vec![line]);
        }
    }
    attach_caption_wraps(&mut columns);
    for column in &mut columns {
        column.sort_by(|a, b| {
            a.page
                .cmp(&b.page)
                .then(b.y.total_cmp(&a.y))
                .then(a.left.total_cmp(&b.left))
        });
    }
    mark_sole_baselines(&mut columns);
    let mut paragraphs = Vec::new();
    for column in columns {
        let mut current: Vec<VisualLine> = Vec::new();
        for line in column {
            let measure = measures.get(&line.page).copied().unwrap_or(0.0);
            if current
                .last()
                .is_some_and(|prev| continues_paragraph(prev, &line, measure))
                || centered_list_keeps(&current, &line)
                || centered_prose_keeps(&current, &line)
            {
                current.push(line);
            } else {
                if !current.is_empty() {
                    paragraphs.push(std::mem::take(&mut current));
                }
                current.push(line);
            }
        }
        if !current.is_empty() {
            paragraphs.push(current);
        }
    }
    paragraphs.extend(loose);
    paragraphs
}

/// A sentence that ends at the bottom of a page continues in the next page's
/// first body paragraph, including after a figure caption or a running header.
fn stitch_page_continuations<'a>(
    mut paragraphs: Vec<Vec<VisualLine<'a>>>,
) -> Vec<Vec<VisualLine<'a>>> {
    // Reading order: finish the left column before the right one, so a sentence
    // at the bottom of a column can claim the top of the next column. The next
    // page is still later, so a page-break join is unchanged.
    paragraphs.sort_by(|left, right| {
        left[0]
            .page
            .cmp(&right[0].page)
            .then(left[0].left.total_cmp(&right[0].left))
            .then(right[0].y.total_cmp(&left[0].y))
    });
    let floor = page_body_floor(&paragraphs);
    let heads = column_heads(&paragraphs);
    let mut used = vec![false; paragraphs.len()];
    let mut stitched = Vec::new();
    for index in 0..paragraphs.len() {
        if used[index] {
            continue;
        }
        used[index] = true;
        let mut current = std::mem::take(&mut paragraphs[index]);
        let mut joins = 0;
        while joins < 4 {
            let Some(next) =
                find_reading_continuation(&current, &paragraphs, &used, &floor, &heads)
            else {
                break;
            };
            // `Baselines. Qwen3.5-2B …` opens with a bold run-in heading. It
            // starts its own paragraph, even after a sentence that ends in a
            // colon above a list.
            if styled_run_in(&paragraphs[next][0]).is_some() {
                break;
            }
            used[next] = true;
            current.extend(std::mem::take(&mut paragraphs[next]));
            joins += 1;
        }
        if !current.is_empty() {
            stitched.push(current);
        }
    }
    // Callers and tests read top-to-bottom. Reading order above is only how a
    // column bottom claims the next column before that column is consumed.
    stitched.sort_by(|left, right| {
        left[0]
            .page
            .cmp(&right[0].page)
            .then(right[0].y.total_cmp(&left[0].y))
            .then(left[0].left.total_cmp(&right[0].left))
    });
    stitched
}

/// Column bottom first, then the bottom of the page. A later column on this
/// page blocks a jump to the next page, so the right column is not skipped.
fn find_reading_continuation(
    current: &[VisualLine<'_>],
    paragraphs: &[Vec<VisualLine<'_>>],
    used: &[bool],
    floor: &crate::detmap::HashMap<u32, f32>,
    heads: &[ColumnHead],
) -> Option<usize> {
    if let Some(next) = find_column_continuation(current, paragraphs, used, heads) {
        return Some(next);
    }
    if closes_the_page(current, paragraphs, used, floor) {
        return find_page_continuation(current, paragraphs, used);
    }
    None
}

/// Lowest body baseline on each page. A line that still has body text under
/// it is not the end of the page, even after that lower paragraph was already
/// taken into an earlier column.
fn page_body_floor(paragraphs: &[Vec<VisualLine<'_>>]) -> crate::detmap::HashMap<u32, f32> {
    let mut floor = crate::detmap::HashMap::default();
    for para in paragraphs {
        if para.is_empty()
            || is_page_bridge(para)
            || is_margin_strip(para)
            || marked_footnote(para)
            || !substantial_paragraph(para)
        {
            continue;
        }
        for line in para {
            let slot = floor.entry(line.page).or_insert(line.y);
            *slot = (*slot).min(line.y);
        }
    }
    floor
}

fn find_column_continuation(
    current: &[VisualLine<'_>],
    paragraphs: &[Vec<VisualLine<'_>>],
    used: &[bool],
    heads: &[ColumnHead],
) -> Option<usize> {
    if !closes_the_column(current, heads) {
        return None;
    }
    let upper = current.last()?;
    // A table cell sitting a little to the right of its header is not the
    // next column. Only a real paragraph votes for that column's left edge.
    let mut lefts = Vec::new();
    for (index, para) in paragraphs.iter().enumerate() {
        if used[index] || para.is_empty() || is_page_bridge(para) || !votes_as_column(para) {
            continue;
        }
        let lower = &para[0];
        if lower.page == upper.page
            && lower.left > upper.left + 36.0
            && lower.left < upper.left + 360.0
        {
            lefts.push(lower.left);
        }
    }
    if lefts.is_empty() {
        return None;
    }
    lefts.sort_by(|a, b| a.total_cmp(b));
    let next_left = lefts[lefts.len() / 2];
    let mut best: Option<(usize, f32)> = None;
    for (index, para) in paragraphs.iter().enumerate() {
        if used[index] || para.is_empty() || is_page_bridge(para) {
            continue;
        }
        let lower = &para[0];
        if lower.page != upper.page || (lower.left - next_left).abs() > 28.0 {
            continue;
        }
        if best.is_none_or(|(_, y)| lower.y > y) {
            best = Some((index, lower.y));
        }
    }
    let index = best?.0;
    column_continuation(current, &paragraphs[index]).then_some(index)
}

/// First line of every paragraph before any stitching. Paragraphs are
/// consumed in left-edge order, so a heading at 53.8pt is taken before the
/// indented paragraph at 63.8pt above it. Asking the live list whether the
/// column still has body text below then said no, and `bloated context:`
/// jumped to the middle of the right column (`Throwback. …`).
#[derive(Clone, Copy)]
struct ColumnHead {
    page: u32,
    left: f32,
    y: f32,
    body: bool,
}

fn column_heads(paragraphs: &[Vec<VisualLine<'_>>]) -> Vec<ColumnHead> {
    paragraphs
        .iter()
        .filter(|para| !para.is_empty() && !is_page_bridge(para))
        .map(|para| ColumnHead {
            page: para[0].page,
            left: para[0].left,
            y: para[0].y,
            body: substantial_paragraph(para) && !marked_footnote(para),
        })
        .collect()
}

fn closes_the_column(current: &[VisualLine<'_>], heads: &[ColumnHead]) -> bool {
    let Some(upper) = current.last() else {
        return false;
    };
    // `˚Work partially done…` sits under the column. It is not the next
    // sentence, and it must not keep the paragraph from continuing on the
    // next page.
    !heads.iter().any(|head| {
        head.body
            && head.page == upper.page
            && (head.left - upper.left).abs() <= 36.0
            && head.y < upper.y - 0.5
    })
}

fn find_page_continuation(
    current: &[VisualLine<'_>],
    paragraphs: &[Vec<VisualLine<'_>>],
    used: &[bool],
) -> Option<usize> {
    let upper = current.last()?;
    let mut next_page = Vec::new();
    for (index, para) in paragraphs.iter().enumerate() {
        if used[index] || para.is_empty() || is_page_bridge(para) {
            continue;
        }
        let lower = &para[0];
        if lower.page != upper.page + 1 {
            continue;
        }
        next_page.push((index, lower.left, lower.y));
    }
    if next_page.is_empty() {
        return None;
    }
    // A gutter index (`3.1.1`) sits left of the body. It is not the column
    // the right-hand sentence continues into.
    let body_edge = next_page
        .iter()
        .filter(|(index, _, _)| {
            let para = &paragraphs[*index];
            votes_as_column(para) && !is_page_bridge(para) && !is_margin_strip(para)
        })
        .map(|(_, left, _)| *left)
        .fold(f32::MAX, f32::min);
    let left_edge = if body_edge.is_finite() {
        body_edge
    } else {
        next_page
            .iter()
            .map(|(_, left, _)| *left)
            .fold(f32::MAX, f32::min)
    };
    // The right column's next line is the next page's left column.
    // The next page's right column is a new column, even when it starts
    // in lowercase (`school Chinese students` under a table).
    if upper.left > left_edge + 36.0 {
        return leftmost_page_continuation(current, paragraphs, &next_page);
    }
    // A hyphenated right column finishes at the top of the next page's left
    // column (`de-` / `tecting`). That column can sit far from this margin.
    if soft_hyphen_stem(&upper.text).is_some() {
        if let Some(index) = leftmost_page_continuation(current, paragraphs, &next_page) {
            return Some(index);
        }
    }
    // The sentence keeps this margin. A gutter index (`3.1.1`, `2.5`) is
    // further left and must not hide `word.` or `explain`.
    let mut band: Vec<(usize, f32)> = next_page
        .iter()
        .copied()
        .filter(|(_, left, _)| (left - upper.left).abs() <= 36.0)
        .map(|(index, _, y)| (index, y))
        .collect();
    band.sort_by(|left, right| right.1.total_cmp(&left.1));
    first_body_continuation(current, paragraphs, &band)
}

fn leftmost_page_continuation(
    current: &[VisualLine<'_>],
    paragraphs: &[Vec<VisualLine<'_>>],
    next_page: &[(usize, f32, f32)],
) -> Option<usize> {
    let mut best: Option<(usize, f32, f32)> = None;
    for &(index, left, y) in next_page {
        let replace = match best {
            None => true,
            Some((_, best_left, best_y)) => {
                left < best_left - 28.0 || ((left - best_left).abs() <= 28.0 && y > best_y)
            }
        };
        if replace {
            best = Some((index, left, y));
        }
    }
    let (_, anchor, _) = best?;
    let mut column: Vec<(usize, f32)> = next_page
        .iter()
        .filter(|(_, left, _)| (left - anchor).abs() <= 28.0)
        .map(|(index, _, y)| (*index, *y))
        .collect();
    column.sort_by(|left, right| right.1.total_cmp(&left.1));
    first_body_continuation(current, paragraphs, &column)
}

/// Top of the column first. A running head or a section title may sit above
/// the lowercase continuation. A new paragraph stops the search.
fn first_body_continuation(
    current: &[VisualLine<'_>],
    paragraphs: &[Vec<VisualLine<'_>>],
    ordered: &[(usize, f32)],
) -> Option<usize> {
    for (index, _) in ordered {
        if page_continuation(current, &paragraphs[*index]) {
            return Some(*index);
        }
        if paragraph_is_page_head(&paragraphs[*index]) {
            continue;
        }
        break;
    }
    None
}

/// A running title, a dotted section heading, or a gutter index. The sentence
/// from the previous page continues in the body under it.
fn paragraph_is_page_head(para: &[VisualLine<'_>]) -> bool {
    if para.is_empty() || para.len() > 2 {
        return false;
    }
    if para.len() == 1 && dotted_index_only(para[0].text.trim()) {
        return true;
    }
    let mut text = para
        .iter()
        .map(|line| line.text.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    text = strip_trailing_folio(&text).to_string();
    if text.chars().count() > 90 || sentence_end(text.trim_end()) {
        return false;
    }
    if is_running_header(&text) {
        return true;
    }
    let Some((dotted, rest)) = section_heading_index(&text) else {
        return false;
    };
    if !dotted {
        return false;
    }
    let words = rest.split_whitespace().count();
    (1..=14).contains(&words) && !rest.contains([',', ';', ':'])
}

fn strip_trailing_folio(text: &str) -> &str {
    let trimmed = text.trim_end();
    let bytes = trimmed.as_bytes();
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1].is_ascii_digit() {
        end -= 1;
    }
    if end < bytes.len()
        && end > 0
        && bytes[end - 1].is_ascii_whitespace()
        && bytes.len() - end <= 4
    {
        return trimmed[..end].trim_end();
    }
    trimmed
}

/// True when nothing but a footer, caption, or header sits below this paragraph.
fn closes_the_page(
    current: &[VisualLine<'_>],
    paragraphs: &[Vec<VisualLine<'_>>],
    used: &[bool],
    floor: &crate::detmap::HashMap<u32, f32>,
) -> bool {
    let Some(upper) = current.last() else {
        return false;
    };
    if let Some(bottom) = floor.get(&upper.page) {
        let size = upper.size.max(1.0);
        if upper.y > bottom + size * 2.0 {
            return false;
        }
    }
    for (index, para) in paragraphs.iter().enumerate() {
        if used[index] || para.is_empty() || is_page_bridge(para) {
            continue;
        }
        let lower = &para[0];
        if lower.page != upper.page {
            continue;
        }
        // A real second column is later in reading order even when its top
        // sits higher on the page. A centered heading or a margin stamp is not
        // that column, and must not block a page-break join.
        if lower.left > upper.left + 36.0 && lower.left < upper.left + 360.0 {
            // A display equation sits in the middle of a single column. It is
            // not the other column, and it must not keep the paragraph under
            // it from continuing on the next page.
            // The first page's abstract sits in that column while the
            // introduction continues on the next page (`shapes (Cai` /
            // `et al., 2020)`). A column that does not continue this
            // sentence must not block that join.
            if column_blocks_page(para) && !unrelated_sidebar(current, para) {
                return false;
            }
            continue;
        }
        // A math gap can leave the hyphen 100pt to the right of the margin.
        // The next line of that sentence is still on this page, so the next
        // page's lowercase line is not the continuation.
        let size = upper.size.max(1.0);
        if (lower.left - upper.left).abs() > size * 12.0 || lower.y >= upper.y - 0.5 {
            continue;
        }
        if substantial_paragraph(para) && !marked_footnote(para) {
            return false;
        }
    }
    true
}

/// A bottom credit (`* Work partially done…`, `˚Work partially…`).
fn marked_footnote(para: &[VisualLine<'_>]) -> bool {
    para.first().is_some_and(|line| footnote_line(&line.text))
}

fn footnote_line(text: &str) -> bool {
    let trimmed = text.trim_start();
    if matches!(
        trimmed.chars().next(),
        Some('*' | '∗' | '⋆' | '†' | '‡' | '§' | '¶' | '˚' | '\u{2042}')
    ) {
        return true;
    }
    // `4To illustrate why…` is a numbered note. The digit is glued to a
    // word. `7B with`, `3.2.2 Multi-Head`, and `1 The agent` are not notes.
    let digits = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if !(1..=2).contains(&digits) {
        return false;
    }
    let word: String = trimmed[digits..]
        .chars()
        .take_while(|ch| ch.is_ascii_alphabetic())
        .collect();
    word.len() >= 2
        && word
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_uppercase())
}

/// The other column is an abstract or a heading, not the rest of this sentence.
fn unrelated_sidebar(current: &[VisualLine<'_>], para: &[VisualLine<'_>]) -> bool {
    let Some(upper) = current.last() else {
        return false;
    };
    if sentence_end(upper.text.trim_end()) {
        return false;
    }
    let Some(lower) = para.first() else {
        return false;
    };
    !prose_continues(upper, lower)
}

/// A real column, not a table cell such as `t [s]` or `Method`.
fn votes_as_column(para: &[VisualLine<'_>]) -> bool {
    let letters: usize = para
        .iter()
        .map(|line| line.text.chars().filter(|ch| ch.is_alphabetic()).count())
        .sum();
    letters >= 16 || para.len() >= 2
}

/// A later column, not a centered equation. The column has a sentence and
/// at least one line wide enough to be body text.
fn column_blocks_page(para: &[VisualLine<'_>]) -> bool {
    if is_margin_strip(para) {
        return false;
    }
    let letters: usize = para
        .iter()
        .map(|line| line.text.chars().filter(|ch| ch.is_alphabetic()).count())
        .sum();
    letters >= 40
}

fn substantial_paragraph(para: &[VisualLine<'_>]) -> bool {
    let letters: usize = para
        .iter()
        .map(|line| line.text.chars().filter(|ch| ch.is_alphabetic()).count())
        .sum();
    letters >= 40 || para.len() >= 2
}

fn is_page_bridge(para: &[VisualLine<'_>]) -> bool {
    let Some(line) = para.first() else {
        return false;
    };
    let text = line.text.trim();
    if text.is_empty() || is_running_header(text) || caption_kind(text).is_some() {
        return true;
    }
    if is_footer_line(text) || is_narrow_folio(line) {
        return true;
    }
    text.chars()
        .all(|ch| ch.is_ascii_digit() || ch.is_whitespace())
}

fn page_continuation(prev: &[VisualLine<'_>], next: &[VisualLine<'_>]) -> bool {
    let Some(upper) = prev.last() else {
        return false;
    };
    if is_footer_line(&upper.text) || is_narrow_folio(upper) {
        return false;
    }
    let Some(lower) = next.first() else {
        return false;
    };
    if lower.page != upper.page + 1 {
        return false;
    }
    // A numbered note at the bottom of the page is not the sentence that
    // continues above it. `4To illustrate…` must not claim `output values`.
    if marked_footnote(prev) {
        return false;
    }
    // `(5)` is the equation's tag. The next page's `where w` defines that
    // equation, but gluing the tag on makes one segment in two coordinate
    // spaces and the definition stays in English.
    if equation_tag(&upper.text) {
        return false;
    }
    // A centered devroom list fills its own slide. The next slide's list is
    // not the rest of this sentence, even when a line starts with `and`.
    // A schedule bullet (`● 1052 events`) is not the start of that list either.
    if centered_comma_block(prev) || centered_comma_block(next) {
        return false;
    }
    // A contents row already carries its page number. The next page's next
    // entry (`appendix A` under chapter 10) is a new row, not the rest of
    // this title.
    if upper.toc {
        return false;
    }
    // The right column ends on this page and the sentence continues at the
    // top of the next page's left column. A new paragraph, or a centered
    // running title, does not: the continuation has to finish the hyphen
    // in lowercase.
    let finishes_hyphen = soft_hyphen_stem(&upper.text).is_some()
        && lower
            .text
            .trim_start()
            .starts_with(|ch: char| ch.is_ascii_lowercase());
    // `presented` at the right margin continues as `mixed results` on the
    // next page's left margin. A citation list continues as `HellaSwag`.
    // A finished sentence does not.
    let later_column = upper.left > lower.left + 36.0
        && !sentence_end(upper.text.trim_end())
        && (lower
            .text
            .trim_start()
            .starts_with(|ch: char| ch.is_ascii_lowercase())
            || (ends_with_list_mark(upper.text.trim_end())
                && lower
                    .text
                    .trim_start()
                    .starts_with(|ch: char| ch.is_ascii_uppercase())));
    if (upper.left - lower.left).abs() > 36.0 && !finishes_hyphen && !later_column {
        return false;
    }
    prose_continues(upper, lower) || list_carries_over(prev, lower)
}

fn column_continuation(prev: &[VisualLine<'_>], next: &[VisualLine<'_>]) -> bool {
    let Some(upper) = prev.last() else {
        return false;
    };
    let Some(lower) = next.first() else {
        return false;
    };
    if lower.page != upper.page {
        return false;
    }
    // An indented contents sub-entry is not the next column. Joining it
    // pulls `our travel assistant` onto `appendix D`.
    if upper.toc {
        return false;
    }
    if is_footer_line(&upper.text)
        || is_footer_line(&lower.text)
        || is_narrow_folio(upper)
        || is_narrow_folio(lower)
    {
        return false;
    }
    if lower.left <= upper.left + 36.0 || lower.left >= upper.left + 360.0 {
        return false;
    }
    // An 8pt footnote (`jan.gruber@kit.edu (Jan Gruber),`) at the foot of
    // the left column does not run into the 10pt body of the right column.
    // A sentence that changes column keeps its type size.
    if (upper.size - lower.size).abs() > upper.size.max(lower.size) * 0.15 {
        return false;
    }
    // A full-width line already covers the other column (`For SQuAD …` running
    // into `by the LM perplexity`). Its continuation is the next line under
    // it, or the next page, not that column's top.
    let size = upper.size.max(lower.size).max(1.0);
    if prev.iter().any(|line| line.right > lower.left + size * 0.5) {
        return false;
    }
    // A line beside this one is the other column's matching row, not the
    // continuation. The next column's text starts near the top of the page,
    // not a folio a few lines above a footer.
    if lower.y < upper.y + size * 8.0 {
        return false;
    }
    if is_margin_strip(next) {
        return false;
    }
    prose_continues(upper, lower)
}

fn is_margin_strip(para: &[VisualLine<'_>]) -> bool {
    let left = para.iter().map(|line| line.left).fold(f32::MAX, f32::min);
    let right = para.iter().map(|line| line.right).fold(0.0f32, f32::max);
    let width = right - left;
    width <= 30.0 && (left <= 64.0 || left >= 500.0)
}

/// The next block keeps this sentence: it starts lowercase, or it finishes a
/// hyphenated word. A sentence that already ended stays in its own paragraph.
/// A display equation at the top of the next column, or at the bottom of this
/// page, is not that continuation.
fn prose_continues(upper: &VisualLine<'_>, lower: &VisualLine<'_>) -> bool {
    if line_has_formula(upper) || line_has_formula(lower) {
        return false;
    }
    let size = upper.size.max(lower.size).max(1.0);
    if (upper.size - lower.size).abs() > size * 0.35 {
        return false;
    }
    let tail = upper.text.trim_end();
    let hyphen = soft_hyphen_stem(tail).is_some();
    if sentence_end(tail) && !hyphen {
        return false;
    }
    let start = lower.text.trim_start();
    if hyphen {
        return start.starts_with(|ch: char| ch.is_ascii_alphanumeric());
    }
    if start.starts_with(|ch: char| ch.is_ascii_lowercase()) {
        return true;
    }
    // `SIQA (Sap et al., 2019),` / `HellaSwag`. The next name is capitalized.
    // A short heading is not that name.
    ends_with_list_mark(tail)
        && start.starts_with(|ch: char| ch.is_ascii_uppercase())
        && lower.text.chars().filter(|ch| ch.is_alphabetic()).count() >= 12
}

/// `(5)` or `[12]`. A display equation's number, not a sentence.
fn equation_tag(text: &str) -> bool {
    let text = text.trim();
    let bytes = text.as_bytes();
    if bytes.len() < 3 || bytes.len() > 6 {
        return false;
    }
    let (open, close) = (bytes[0], bytes[bytes.len() - 1]);
    if !matches!((open, close), (b'(', b')') | (b'[', b']')) {
        return false;
    }
    bytes[1..bytes.len() - 1]
        .iter()
        .all(|byte| byte.is_ascii_digit())
}

fn ends_with_list_mark(text: &str) -> bool {
    let trimmed = text.trim_end().trim_end_matches(|ch: char| {
        matches!(ch, ')' | '）' | ']' | '"' | '\'' | '\u{2019}' | '\u{201D}')
    });
    trimmed.ends_with([',', ';', ':'])
}

/// `construct LangChain` / `Expression Language` across a page. A new
/// paragraph, or the next dash item, does not.
fn list_carries_over(prev: &[VisualLine<'_>], lower: &VisualLine<'_>) -> bool {
    let Some(upper) = prev.last() else {
        return false;
    };
    if !prev.iter().any(|line| starts_with_list_marker(&line.text))
        || starts_with_list_marker(&lower.text)
    {
        return false;
    }
    let tail = upper.text.trim_end();
    if sentence_end(tail) || soft_hyphen_stem(tail).is_some() {
        return false;
    }
    // A bullet that is only a link is the whole item. The next page's
    // heading (`Summary`) is not the rest of that URL.
    if tail.contains("://") || tail.contains("www.") {
        return false;
    }
    let size = upper.size.max(lower.size).max(1.0);
    if (upper.size - lower.size).abs() > size * 0.35 {
        return false;
    }
    // A slide title is a larger face. `If you need help` at 36pt is not the
    // rest of a 24pt bullet. Comparing against the list line keeps a title
    // from slipping under the band that is measured on the larger size.
    if lower.size > upper.size * 1.2 {
        return false;
    }
    let body_left = prev.iter().map(|line| line.left).fold(f32::MAX, f32::min);
    let shift = lower.left - body_left;
    (-size..=size * 4.0).contains(&shift)
        && lower
            .text
            .trim_start()
            .starts_with(|ch: char| ch.is_ascii_alphabetic())
}

fn column_anchor(column: &[VisualLine<'_>], page: u32) -> Option<f32> {
    let mut xs: Vec<f32> = column
        .iter()
        .filter(|line| line.page == page)
        .map(|line| line.left)
        .collect();
    if xs.is_empty() {
        return None;
    }
    xs.sort_by(|a, b| a.total_cmp(b));
    Some(xs[xs.len() / 2])
}

/// Upper quartile of line widths on one page. A banner does not set it, and
/// a stack of short address rows does not either when the page has body text.
fn page_measures(lines: &[VisualLine<'_>]) -> crate::detmap::HashMap<u32, f32> {
    let mut widths: crate::detmap::HashMap<u32, Vec<f32>> = crate::detmap::HashMap::default();
    for line in lines {
        let width = line.right - line.left;
        if width > 1.0 {
            widths.entry(line.page).or_default().push(width);
        }
    }
    widths
        .into_iter()
        .map(|(page, mut samples)| {
            samples.sort_by(|a, b| a.total_cmp(b));
            let index = samples.len() * 3 / 4;
            (page, samples[index.min(samples.len() - 1)])
        })
        .collect()
}

/// A one-line column that only exists because a caption wrap is centered
/// belongs on the caption, not in a column of its own.
fn attach_caption_wraps(columns: &mut Vec<Vec<VisualLine<'_>>>) {
    let mut host_of: Vec<Vec<Option<usize>>> = columns
        .iter()
        .map(|column| vec![None; column.len()])
        .collect();
    for (index, column) in columns.iter().enumerate() {
        for (line_index, line) in column.iter().enumerate() {
            if caption_kind(&line.text).is_some() {
                continue;
            }
            for (host, other) in columns.iter().enumerate() {
                if host == index {
                    continue;
                }
                if other
                    .iter()
                    .any(|prev| caption_kind(&prev.text).is_some() && caption_line_wrap(prev, line))
                {
                    host_of[index][line_index] = Some(host);
                    break;
                }
            }
        }
    }
    if host_of
        .iter()
        .all(|column| column.iter().all(Option::is_none))
    {
        return;
    }
    let mut next: Vec<Vec<VisualLine<'_>>> = columns.iter().map(|_| Vec::new()).collect();
    for (index, column) in columns.drain(..).enumerate() {
        for (line_index, line) in column.into_iter().enumerate() {
            let dest = host_of[index][line_index].unwrap_or(index);
            next[dest].push(line);
        }
    }
    next.retain(|column| !column.is_empty());
    *columns = next;
}

/// The next line finishes a caption. It sits one leading below and starts
/// in lowercase. A body sentence under the caption is further away.
fn caption_line_wrap(upper: &VisualLine<'_>, lower: &VisualLine<'_>) -> bool {
    if upper.page != lower.page {
        return false;
    }
    let size = upper.size.max(lower.size).max(1.0);
    let dy = upper.y - lower.y;
    if dy < size * 0.6 || dy > size * 1.8 {
        return false;
    }
    if (upper.size - lower.size).abs() > size * 0.35 {
        return false;
    }
    // `columns.` is the caption's own wrap. The other column's body line
    // (`in the types of aspects…`) is one leading lower and also starts in
    // lowercase, but it sits on the left margin. A line in the column to the
    // right (`expensive as a single…` beside `Table 5:`) misses the caption
    // entirely. A centered wrap still overlaps the caption line.
    if lower.left + size * 0.4 < upper.left - size {
        return false;
    }
    let overlap = (upper.right.min(lower.right) - upper.left.max(lower.left)).max(0.0);
    if overlap < size * 0.5 {
        return false;
    }
    let next = lower.text.trim_start();
    !sentence_end(upper.text.trim_end())
        && next.starts_with(|ch: char| ch.is_ascii_lowercase())
        && lower.text.chars().filter(|ch| ch.is_alphabetic()).count() >= 6
}

fn continues_paragraph(upper: &VisualLine<'_>, lower: &VisualLine<'_>, measure: f32) -> bool {
    if upper.page != lower.page {
        return false;
    }
    // `†…Brain.` and `‡…Research.` are two notes. Joining them wraps 工作
    // across the two short source lines. A starred note's next line starts
    // with a word, so that paragraph still joins.
    if footnote_line(&lower.text) {
        return false;
    }
    // `ᵃChair …, Germany` / `ᵇKASTEL …, Germany`: a line led by a raised
    // marker is its own affiliation or note. Joining them reflows the
    // block and the longer Chinese runs into the next affiliation.
    if table_note_marker(lower) {
        return false;
    }
    // `Figure 2 | … segmentation` / `and reading order prediction.` The wrap
    // is centered, so it does not share the caption's left edge.
    if caption_kind(&upper.text).is_some() && caption_line_wrap(upper, lower) {
        return true;
    }
    let next = lower.text.trim_start();
    // `GPT-` / `2` and `MetaMath-` / `7B` are one token. A contents row is
    // `83 Generating` only when the line above is not finishing that token.
    // A prompt answer hangs a quote on the wrap (`palin-` / `“ dromes`).
    let hyphen = soft_hyphen_stem(&upper.text).is_some()
        && (next.starts_with(|ch: char| ch.is_ascii_alphanumeric()) || quoted_hyphen_tail(next));
    // Two contents titles stay apart. A lowercase wrap under a title that
    // has the baseline to itself (`Q&A` / `engines`) is still that title.
    // A right-hand title returning to the left indent is a different entry.
    if upper.toc || lower.toc {
        return toc_same_indent_wrap(upper, lower);
    }
    // A detailed-contents line already holds several entries (`83 Generating`).
    // Joining the wrap makes one block the Chinese leading cannot fit.
    // `1 The agent…` looks like a contents row (`1 The`). A hanging wrap
    // under that number is the rest of the item, not the next contents line.
    if !hyphen
        && !hanging_numbered_continuation(upper, lower)
        && (inline_contents_entry(&upper.text) || inline_contents_entry(&lower.text))
    {
        return false;
    }
    // A display equation keeps its own segment. Joining it onto the prose
    // around it would leave that prose untranslated. A wrapped clause that
    // starts with a word and then a formula (`version. We write [N] = …`,
    // `to x + y = z`) is still the sentence above it.
    let prose_tail = prose_formula_tail(upper, lower);
    if !hyphen && line_has_formula(upper) {
        return false;
    }
    if !hyphen && line_has_formula(lower) && !prose_tail {
        return false;
    }
    let size = upper.size.max(lower.size).max(1.0);
    let dy = upper.y - lower.y;
    if dy < size * 0.7 || dy > size * 1.5 {
        return false;
    }
    // A centered devroom list breaks the left edge on every line (`Energy,`
    // / `Legal and Policy,`). The shared axis is the line's midpoint.
    if centered_comma_continuation(upper, lower) {
        return true;
    }
    // `2 Guided…` is the next numbered item, not the tail of item 1.
    if !hyphen && numbered_item_body(&lower.text).is_some() {
        return false;
    }
    // A broken word stays one paragraph when the next line's math is a
    // little larger. A heading and a footnote are still different lines.
    let size_gap = (upper.size - lower.size).abs();
    if hyphen {
        if size_gap > size * 0.45 {
            return false;
        }
    } else if size_gap > size * 0.2 {
        return false;
    }
    // The lead line is a bold coloured face (`1 The agent…`) and the wrap
    // is body roman, one hanging step to the right. They are one item.
    if !hyphen && hanging_numbered_continuation(upper, lower) {
        return true;
    }
    // A bold or italic run-in is the same paragraph as the regular line under it.
    // A monospace phrase at the start of a line is not a new paragraph.
    if !hyphen && !fonts_can_join(upper, lower) {
        return false;
    }
    // A chapter title is right-aligned. The second line is longer, so its
    // left edge sits further left (`Introduction to AI` / `agents and applications`).
    if !hyphen && right_aligned_display_wrap(upper, lower) {
        return true;
    }
    if (upper.left - lower.left).abs() > 36.0 && !hyphen {
        return false;
    }
    // A first-line indent, with the same leading and no extra gap, is a new
    // paragraph. A hyphenated word's continuation is indented the other way
    // and is handled above.
    // A dash or dingbat item hangs: the marker is left of the wrapped lines.
    // A new item starts with its own marker and stays a separate paragraph.
    if !hyphen && starts_with_list_marker(&lower.text) {
        return false;
    }
    if !hyphen && hanging_list_continuation(upper, lower) {
        return true;
    }
    if !hyphen && lower.left > upper.left + size * 0.55 {
        return false;
    }
    if hyphen {
        return true;
    }
    // An address or a credits row is a stack of short lines under a full
    // measure. Joining them makes one paragraph, and the Chinese then puts
    // a single character on each source line. A wrapped sentence still joins:
    // its upper line fills the column, and a short tail starts lowercase.
    if stacked_short_items(upper, lower, measure) {
        return false;
    }
    if toc_entry_boundary(upper, lower) {
        return false;
    }
    let upper_w = upper.right - upper.left;
    let lower_w = lower.right - lower.left;
    // A short heading above a full line stays apart. A body wrap can end
    // early by a whole word (`and Manning` / `Publications was…`) and is
    // still the same paragraph.
    if upper.right + size * 2.2 < lower.right && !wrapped_body_continuation(upper, lower, upper_w) {
        return false;
    }
    // A short tail that does not finish a sentence is a new item (`20 Baldwin`).
    // A lowercase tail is the rest of the sentence (`you bridge any gaps:`).
    let lower_starts_lower = lower
        .text
        .trim_start()
        .starts_with(|ch: char| ch.is_ascii_lowercase())
        // `relative to the current harness` / `𝐻𝑡, let` continues with an
        // inline symbol, which is not a capital.
        || (lower.text.trim_start().starts_with(is_math_alphanumeric)
            && !sentence_end(upper.text.trim_end()));
    if lower_w < upper_w * 0.5
        && lower.text.chars().count() < 32
        && !lower.text.trim_end().ends_with('.')
        && !lower_starts_lower
    {
        return false;
    }
    let overlap = (upper.right.min(lower.right) - upper.left.max(lower.left)).max(0.0);
    let narrow = upper_w.min(lower_w).max(1.0);
    overlap >= narrow * 0.35 || (upper.left - lower.left).abs() <= 8.0
}

fn mark_sole_baselines(columns: &mut [Vec<VisualLine<'_>>]) {
    let mut flat: Vec<(u32, f32, f32, f32, f32, usize, usize)> = Vec::new();
    for (ci, column) in columns.iter().enumerate() {
        for (li, line) in column.iter().enumerate() {
            flat.push((line.page, line.y, line.left, line.right, line.size, ci, li));
        }
    }
    for (page, y, _left, right, size, ci, li) in &flat {
        let limit = size.max(1.0) * 0.35;
        let sibling = flat.iter().any(|(page2, y2, left2, _, _, ci2, li2)| {
            (*ci2, *li2) != (*ci, *li)
                && page2 == page
                && (y2 - y).abs() <= limit
                && *left2 > right + size.max(1.0) * 0.3
        });
        if sibling {
            columns[*ci][*li].sole_on_baseline = false;
        }
    }
}

fn is_math_alphanumeric(ch: char) -> bool {
    ('\u{1D400}'..='\u{1D7FF}').contains(&ch)
}

/// `engines` under `Summarization and Q&A` continues that title. The next
/// entry on the same baseline (`chatbots`) does not.
fn toc_same_indent_wrap(upper: &VisualLine<'_>, lower: &VisualLine<'_>) -> bool {
    // A title that already has its page number is finished. `appendix A` must
    // not absorb `appendix B` just because both start in lowercase.
    if upper.toc || !upper.sole_on_baseline {
        return false;
    }
    let next = lower.text.trim_start();
    if !next.starts_with(|ch: char| ch.is_ascii_lowercase()) {
        return false;
    }
    if (upper.left - lower.left).abs() > 8.0 {
        return false;
    }
    let size = upper.size.max(lower.size).max(1.0);
    let dy = upper.y - lower.y;
    dy >= size * 0.7 && dy <= size * 1.5 && (upper.size - lower.size).abs() <= size * 0.2
}

/// The upper line is a wrapped body line, not a heading or a contents title.
/// `visit` / `www…` continues in lowercase. `Manning` / `Publications`
/// continues a proper name.
fn wrapped_body_continuation(upper: &VisualLine<'_>, lower: &VisualLine<'_>, upper_w: f32) -> bool {
    if sentence_end(upper.text.trim_end()) {
        return false;
    }
    let size = upper.size.max(lower.size).max(1.0);
    // A contents title is about twenty ems. A wrapped body line is longer.
    if upper_w < size * 26.0 {
        return false;
    }
    let next = lower.text.trim_start();
    if next.starts_with(|ch: char| ch.is_ascii_lowercase()) {
        return true;
    }
    ends_with_capitalized_word(&upper.text)
        && starts_with_capitalized_word(next)
        && upper.text.chars().any(|ch| ch.is_ascii_lowercase())
}

fn ends_with_capitalized_word(text: &str) -> bool {
    text.split_whitespace()
        .last()
        .is_some_and(is_capitalized_word)
}

fn starts_with_capitalized_word(text: &str) -> bool {
    text.split_whitespace()
        .next()
        .is_some_and(is_capitalized_word)
}

fn is_capitalized_word(token: &str) -> bool {
    let token =
        token.trim_matches(|ch: char| matches!(ch, ',' | '.' | ':' | ';' | ')' | '(' | '“' | '”'));
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_uppercase() {
        return false;
    }
    let rest: Vec<char> = chars.collect();
    !rest.is_empty()
        && rest.iter().any(|ch| ch.is_ascii_lowercase())
        && rest
            .iter()
            .all(|ch| ch.is_ascii_alphabetic() || matches!(ch, '-' | '\'' | '’'))
}

/// A bullet, dash, or dingbat at the start of a list item.
pub(crate) fn starts_with_list_marker(text: &str) -> bool {
    let trimmed = text.trim_start();
    let mut chars = trimmed.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !is_list_marker(first) {
        return false;
    }
    // `-and-applications` is a wrapped hyphen. `- Chapter` is a bullet.
    // An en dash is a bullet even when the word follows immediately (`–Chapter`).
    if first == '-' {
        return chars.next().is_none_or(char::is_whitespace);
    }
    true
}

fn is_list_marker(ch: char) -> bool {
    matches!(
        ch,
        '-' | '–'
            | '—'
            | '•'
            | '●'
            | '○'
            | '◯'
            | '◎'
            | '◉'
            | '◦'
            | '▪'
            | '■'
            | '‣'
            | '∙'
            | '·'
            | '‧'
    ) || ('\u{E000}'..='\u{F8FF}').contains(&ch)
}

/// `1 The agent sends…` and `1 智能体向…` are numbered items. `1.1 Building`
/// is a section index. A list number is one or two digits, an optional
/// closing `.` or `)`, then a word in any script.
pub(crate) fn numbered_item_body(text: &str) -> Option<&str> {
    let text = text.trim_start();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index == 0 || index > 2 {
        return None;
    }
    if index < bytes.len() && (bytes[index] == b'.' || bytes[index] == b')') {
        let after = index + 1;
        if after < bytes.len() && bytes[after].is_ascii_digit() {
            return None;
        }
        index = after;
    }
    if index >= bytes.len() || bytes[index] != b' ' {
        return None;
    }
    let rest = text[index..].trim_start();
    rest.chars()
        .next()
        .filter(|ch| ch.is_alphabetic())
        .map(|_| rest)
}

/// The words after a hanging number share one left edge with the lines below.
/// A section heading's title after a dotted number (`2.4 Pipeline…`,
/// `A.1.2 Setup`).
fn section_number_body(text: &str) -> Option<&str> {
    let text = text.trim_start();
    let (number, rest) = text.split_once(' ')?;
    let mut parts = number.trim_end_matches('.').split('.');
    let head = parts.next()?;
    let head_ok = (!head.is_empty() && head.len() <= 2 && head.bytes().all(|b| b.is_ascii_digit()))
        || (head.len() == 1 && head.bytes().all(|b| b.is_ascii_uppercase()));
    let tail: Vec<&str> = parts.collect();
    let tail_ok = !tail.is_empty()
        && tail.iter().all(|part| {
            !part.is_empty() && part.len() <= 2 && part.bytes().all(|b| b.is_ascii_digit())
        });
    let rest = rest.trim_start();
    (head_ok && tail_ok && rest.starts_with(char::is_alphabetic)).then_some(rest)
}

fn hanging_numbered_continuation(upper: &VisualLine<'_>, lower: &VisualLine<'_>) -> bool {
    let numbered =
        |text: &str| numbered_item_body(text).is_some() || section_number_body(text).is_some();
    if !numbered(&upper.text) || numbered(&lower.text) {
        return false;
    }
    if starts_with_list_marker(&lower.text) {
        return false;
    }
    let Some(text_x) = text_after_number_x(upper) else {
        return false;
    };
    let size = upper.size.max(lower.size).max(1.0);
    (lower.left - text_x).abs() <= (size * 0.55).max(4.0)
}

/// Left edge of the first word after the item number.
fn text_after_number_x(line: &VisualLine<'_>) -> Option<f32> {
    let mut glyphs = line.glyphs.clone();
    glyphs.sort_by(|left, right| left.matrix[4].total_cmp(&right.matrix[4]));
    let mut saw_number = false;
    for glyph in &glyphs {
        let token = glyph.unicode.trim();
        if token.is_empty() {
            continue;
        }
        if token
            .chars()
            .all(|ch| ch.is_ascii_digit() || ch == '.' || ch == ')')
        {
            saw_number = true;
            continue;
        }
        if !saw_number {
            return None;
        }
        return Some(glyph.matrix[4]);
    }
    None
}

/// The marker sits left of the wrapped lines (` Part 1` / `exploring…`,
/// `– Chapter 1` / `semantic search`). A new item has its own marker.
fn hanging_list_continuation(upper: &VisualLine<'_>, lower: &VisualLine<'_>) -> bool {
    if !starts_with_list_marker(&upper.text) || starts_with_list_marker(&lower.text) {
        return false;
    }
    let size = upper.size.max(lower.size).max(1.0);
    let shift = lower.left - upper.left;
    (size * 0.45..=size * 2.8).contains(&shift)
}

/// Both lines are well under the page's column measure, and the next one
/// starts a new item (`Manning Publications`, `20 Baldwin Road`).
fn stacked_short_items(upper: &VisualLine<'_>, lower: &VisualLine<'_>, measure: f32) -> bool {
    let size = upper.size.max(lower.size).max(1.0);
    if measure < size * 18.0 {
        return false;
    }
    let cap = measure * 0.5;
    let upper_w = upper.right - upper.left;
    let lower_w = lower.right - lower.left;
    if upper_w >= cap || lower_w >= cap {
        return false;
    }
    let trimmed = lower.text.trim_start();
    trimmed.starts_with(|ch: char| ch.is_ascii_uppercase() || ch.is_ascii_digit())
        || crate::translate::has_email_address(trimmed)
        || trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
}

/// The lower line opens with a word, so the formula on it is inline.
fn prose_formula_tail(upper: &VisualLine<'_>, lower: &VisualLine<'_>) -> bool {
    if sentence_end(upper.text.trim_end()) {
        return false;
    }
    let next = lower.text.trim_start();
    // `For a candidate 𝐻′ relative to the current harness` / `𝐻𝑡, let`: the
    // wrap opens with the symbol the sentence was naming and goes on in words.
    if next.starts_with(is_math_alphanumeric) {
        let rest = next.trim_start_matches(|ch: char| {
            is_math_alphanumeric(ch) || matches!(ch, '′' | '\'' | ',' | ' ' | '(' | ')')
        });
        let operator = next
            .chars()
            .any(|ch| matches!(ch, '=' | '<' | '>' | '+' | '≤' | '≥' | '∈'));
        return !operator
            && rest.split_whitespace().next().is_some_and(|word| {
                word.len() >= 2 && word.chars().all(|ch| ch.is_ascii_lowercase())
            });
    }
    if !next.starts_with(|ch: char| ch.is_ascii_lowercase()) {
        return false;
    }
    let letters = lower.text.chars().filter(|ch| ch.is_alphabetic()).count();
    if letters >= 8 {
        return true;
    }
    let word = next
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|ch: char| !ch.is_ascii_alphabetic());
    matches!(
        word,
        "to" | "of"
            | "in"
            | "an"
            | "the"
            | "and"
            | "or"
            | "for"
            | "with"
            | "by"
            | "as"
            | "on"
            | "at"
            | "from"
            | "into"
            | "than"
    )
}

fn line_has_formula(line: &VisualLine<'_>) -> bool {
    let mut math = 0usize;
    let mut letters = 0usize;
    for glyph in &line.glyphs {
        if math_font_glyph(glyph) {
            math += 1;
        } else {
            letters += glyph
                .unicode
                .chars()
                .filter(|ch| ch.is_ascii_alphabetic())
                .count();
        }
    }
    // One math letter inside a sentence is still that sentence. A display
    // equation has more math glyphs than prose letters. `Attention(Q,K,V) =
    // softmax(...)` is that equation even though the function names are roman.
    (math > 0 && (letters < 16 || math >= letters)) || display_equation_glyphs(&line.glyphs)
}

/// The right-hand piece opens with a math glyph and the sentence continues
/// (`√d_k, and apply`). A column gutter does not.
fn continues_through_inline_math(line: &[&Glyph], index: usize, size: f32) -> bool {
    let open = line[index];
    if !math_font_glyph(open) {
        return false;
    }
    let origin = glyph_left(open);
    line[index + 1..].iter().any(|glyph| {
        glyph_left(glyph) - origin <= size * 8.0
            && !math_font_glyph(glyph)
            && glyph.unicode.chars().any(|ch| ch.is_ascii_lowercase())
    })
}

/// A display equation: math-font glyphs, an operator, and no prose words.
/// Roman function names (`Attention`, `softmax`) stay in the equation.
/// `divide each by √d_k, and apply` is a sentence, so it is not.
pub(crate) fn display_equation_glyphs(glyphs: &[&Glyph]) -> bool {
    if glyphs.len() < 4 {
        return false;
    }
    let body = glyphs
        .iter()
        .map(|glyph| glyph.font_size)
        .fold(1.0f32, f32::max);
    let mut anchor_ys: Vec<f32> = glyphs
        .iter()
        .filter(|glyph| glyph.font_size > body * 0.84)
        .map(|glyph| glyph.matrix[5])
        .collect();
    if anchor_ys.is_empty() {
        anchor_ys = glyphs.iter().map(|glyph| glyph.matrix[5]).collect();
    }
    anchor_ys.sort_by(|left, right| left.total_cmp(right));
    let anchor_y = anchor_ys[anchor_ys.len() / 2];
    let mut math = 0usize;
    let mut letters = 0usize;
    let mut operators = 0usize;
    for glyph in glyphs {
        if math_font_glyph(glyph) {
            math += 1;
        } else {
            letters += glyph
                .unicode
                .chars()
                .filter(|ch| ch.is_ascii_alphabetic())
                .count();
        }
        // A superscript `×` in `d_k` sits above the line. The `=` under a
        // summation is smaller than the sentence and is still that formula's
        // operator.
        if is_display_operator(&glyph.unicode) {
            let raised = glyph.matrix[5] > anchor_y + glyph.font_size * 0.25;
            if glyph.font_size > body * 0.84 || !raised {
                operators += 1;
            }
        }
    }
    if math < 3 || operators < 1 || letters >= 48 {
        return false;
    }
    let (function_words, content_words) = prose_word_counts(glyphs);
    // `q_i k_i, has mean 0 and variance d_k` is a footnote clause. The `=`
    // under the summation and one function word (`and`) are not enough to
    // keep the whole line in English.
    if variance_clause(glyphs) {
        return false;
    }
    // `Attention(Q, K, V) = softmax(...)` has no function word. `where head_i
    // = Attention(Q W^Q_i, ...)` is still that equation: one lead-in word.
    function_words == 0 || (function_words <= 1 && content_words <= 3)
}

/// `has mean 0 and variance d_k`. The words are the sentence; the product
/// is the formula in front of them.
fn variance_clause(glyphs: &[&Glyph]) -> bool {
    let mut ordered = glyphs.to_vec();
    ordered.sort_by(|left, right| left.matrix[4].total_cmp(&right.matrix[4]));
    let mut words = Vec::new();
    let mut word = String::new();
    let mut previous: Option<&Glyph> = None;
    for glyph in &ordered {
        if let Some(prev) = previous {
            if word_space(prev, glyph) && !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        }
        previous = Some(glyph);
        if math_font_glyph(glyph) {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            continue;
        }
        for ch in glyph.unicode.chars() {
            if ch.is_ascii_alphabetic() {
                word.push(ch.to_ascii_lowercase());
            } else if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words.iter().any(|item| item == "mean") && words.iter().any(|item| item == "variance")
}

fn prose_word_counts(glyphs: &[&Glyph]) -> (usize, usize) {
    let mut ordered = glyphs.to_vec();
    ordered.sort_by(|left, right| left.matrix[4].total_cmp(&right.matrix[4]));
    let mut word = String::new();
    let mut function_words = 0usize;
    let mut content_words = 0usize;
    let mut previous: Option<&Glyph> = None;
    let flush = |word: &mut String, function_words: &mut usize, content_words: &mut usize| {
        if word.len() >= 3 {
            if is_prose_function_word(word) {
                *function_words += 1;
            } else {
                *content_words += 1;
            }
        } else if is_prose_function_word(word) {
            *function_words += 1;
        }
        word.clear();
    };
    for glyph in ordered {
        if let Some(prev) = previous {
            if word_space(prev, glyph) {
                flush(&mut word, &mut function_words, &mut content_words);
            }
        }
        previous = Some(glyph);
        if math_font_glyph(glyph) {
            flush(&mut word, &mut function_words, &mut content_words);
            continue;
        }
        for ch in glyph.unicode.chars() {
            if ch.is_ascii_alphabetic() {
                word.push(ch.to_ascii_lowercase());
            } else {
                flush(&mut word, &mut function_words, &mut content_words);
            }
        }
    }
    flush(&mut word, &mut function_words, &mut content_words);
    (function_words, content_words)
}

fn is_display_operator(text: &str) -> bool {
    matches!(
        text.trim(),
        "=" | "+"
            | "-"
            | "−"
            | "×"
            | "·"
            | "/"
            | "√"
            | "∑"
            | "∏"
            | "∫"
            | "<"
            | ">"
            | "≤"
            | "≥"
            | "≈"
            | "≠"
    )
}

fn is_prose_function_word(word: &str) -> bool {
    matches!(
        word,
        "the"
            | "of"
            | "and"
            | "for"
            | "with"
            | "from"
            | "to"
            | "by"
            | "on"
            | "in"
            | "a"
            | "an"
            | "or"
            | "as"
            | "at"
            | "this"
            | "is"
            | "we"
            | "are"
            | "be"
            | "that"
            | "where"
            | "which"
            | "each"
            | "into"
            | "than"
    )
}

fn math_font_glyph(glyph: &Glyph) -> bool {
    let upper = glyph.font_name.to_ascii_uppercase();
    let math = ["CMMI", "CMSY", "CMEX", "MSAM", "MSBM", "STIX"]
        .iter()
        .any(|needle| upper.contains(needle))
        || upper.contains("MATH");
    math && !matches!(
        glyph.unicode.trim(),
        "*" | "∗" | "†" | "‡" | "§" | "¶" | "⋆" | "#"
    ) && !is_inline_math_symbol(&glyph.unicode)
}

/// A subscript or superscript (`f_i`, `N_i^+`) drawn on its own baseline.
/// It belongs to the prose line it sits on, not between two paragraphs.
fn attach_math_scripts(lines: &mut Vec<VisualLine<'_>>) {
    let mut drop = vec![false; lines.len()];
    let hosts: Vec<Option<usize>> = (0..lines.len())
        .map(|index| math_script_host(index, lines))
        .collect();
    for (index, host) in hosts.into_iter().enumerate() {
        let Some(host) = host else {
            continue;
        };
        let scripts = std::mem::take(&mut lines[index].glyphs);
        lines[host].glyphs.extend(scripts);
        lines[host]
            .glyphs
            .sort_by(|a, b| a.matrix[4].total_cmp(&b.matrix[4]));
        lines[host].text = line_text(&lines[host].glyphs);
        if let Some((left, right, _, _)) = line_bounds(&lines[host].glyphs) {
            lines[host].left = left;
            lines[host].right = right;
        }
        drop[index] = true;
    }
    let mut index = 0;
    lines.retain(|_| {
        let keep = !drop[index];
        index += 1;
        keep
    });
}

fn math_script_host(index: usize, lines: &[VisualLine<'_>]) -> Option<usize> {
    let line = &lines[index];
    if !is_math_script(line) {
        return None;
    }
    let mut best: Option<(usize, f32)> = None;
    for (other_index, other) in lines.iter().enumerate() {
        if other_index == index || other.page != line.page || is_math_script(other) {
            continue;
        }
        let letters = other
            .text
            .chars()
            .filter(|ch| ch.is_ascii_alphabetic())
            .count();
        let dy = (line.y - other.y).abs();
        let size = other.size.max(1.0);
        // `W^O` is a one-glyph superscript. The prose line above also covers
        // that x, but the base is the math letter on the short line below.
        let on_math = script_sits_on_math(line, other, size);
        if letters < 16 && !display_equation_glyphs(&other.glyphs) && !on_math {
            continue;
        }
        // A radical sits about one em above its radicand. The line above can
        // be closer in y (`a√nd`) while the math letter it belongs to is below.
        if dy < size * 0.08 || dy > size * 1.05 {
            continue;
        }
        // A prose subscript drops well under half an em. Marks most of an em
        // below a prose line (`⊤ −1` of the formula under `Definition 3`)
        // belong to that formula, and gluing them in read `Info⊤r)m−a1tion`.
        // A limit variable (`n` under a sum) still names its sentence.
        if line.y < other.y
            && !on_math
            && dy > size * 0.5
            && line.size < size * 0.85
            && !line.text.chars().any(char::is_alphabetic)
        {
            continue;
        }
        if line.left < other.left - size || line.right > other.right + size {
            continue;
        }
        let mut score = dy;
        if on_math {
            score -= size;
        }
        if line.y > other.y && radical_meets_base(line, other) {
            score -= size;
        }
        if best.is_none_or(|(_, best_score)| score < best_score) {
            best = Some((other_index, score));
        }
    }
    best.map(|(host, _)| host)
}

/// The radical's right edge meets a math glyph on the line below it.
fn radical_meets_base(script: &VisualLine<'_>, host: &VisualLine<'_>) -> bool {
    let size = host.size.max(1.0);
    script.glyphs.iter().any(|mark| {
        is_display_operator(&mark.unicode) || mark.font_name.to_ascii_uppercase().contains("CMSY")
    }) && host.glyphs.iter().any(|glyph| {
        math_font_glyph(glyph) && {
            let dx = glyph_left(glyph) - script.right;
            dx >= -size * 0.45 && dx <= size * 0.65
        }
    })
}

/// A prime or a spacing accent (`𝐻′`, `𝑆̂` drawn as `ˆ` over `𝑆`) that the
/// extractor left on its own raised line.
fn is_math_mark(text: &str) -> bool {
    matches!(
        text,
        "′" | "″" | "‴" | "ˆ" | "˜" | "¯" | "˙" | "¨" | "ˇ" | "˘" | "´" | "`" | "^" | "~"
    ) || (!text.is_empty()
        && text
            .chars()
            .all(|ch| ('\u{0300}'..='\u{036f}').contains(&ch)))
}

fn is_math_script(line: &VisualLine<'_>) -> bool {
    if line.glyphs.len() == 1 && is_math_mark(line.text.trim()) {
        return true;
    }
    let width = line.right - line.left;
    let symbol = |glyph: &Glyph| math_font_glyph(glyph) || vector_symbol_glyph(glyph);
    width <= line.size.max(1.0) * 6.0
        && (1..=6).contains(&line.glyphs.len())
        && line.glyphs.iter().any(|glyph| symbol(glyph))
        && line
            .glyphs
            .iter()
            .all(|glyph| symbol(glyph) || !glyph.unicode.chars().any(|ch| ch.is_ascii_alphabetic()))
}

/// `W_i^Q ∈ R` then a wide hole then `, W_i^K`. The hole is the superscript
/// `d_model×d_k`, which is on the baseline above. It is not a second column.
fn bridge_formula_holes(lines: &mut Vec<VisualLine<'_>>) {
    let mut index = 0usize;
    while index < lines.len() {
        let Some(next) = next_formula_shard(lines, index) else {
            index += 1;
            continue;
        };
        let taken = std::mem::take(&mut lines[next].glyphs);
        lines[index].glyphs.extend(taken);
        lines[index]
            .glyphs
            .sort_by(|left, right| left.matrix[4].total_cmp(&right.matrix[4]));
        lines[index].text = line_text(&lines[index].glyphs);
        if let Some((left, right, y, size)) = line_bounds(&lines[index].glyphs) {
            lines[index].left = left;
            lines[index].right = right;
            lines[index].y = y;
            lines[index].size = size;
        }
        lines.remove(next);
    }
}

fn next_formula_shard(lines: &[VisualLine<'_>], index: usize) -> Option<usize> {
    let left = &lines[index];
    if left.glyphs.is_empty() {
        return None;
    }
    let size = left.size.max(1.0);
    let body = page_body_size(lines, left.page);
    let body_glyph = |glyph: &Glyph| glyph.font_size > body * 0.84;
    if !left.glyphs.iter().any(|glyph| body_glyph(glyph)) {
        return None;
    }
    let next = (index + 1..lines.len().min(index + 8)).find(|&other| {
        let right = &lines[other];
        right.page == left.page
            && !right.glyphs.is_empty()
            && right.glyphs.iter().any(|glyph| body_glyph(glyph))
            && (right.y - left.y).abs() <= size * 0.45
            && right.left >= left.right - 1.0
    })?;
    let right = &lines[next];
    // Equation (1) is already a display equation. Bridging its fraction
    // hole pulls `)V` and the transpose into the segment, and the formula
    // is then redrawn off its baseline.
    // A shard that reads `, with z = 1 when edit j` is the sentence going
    // on after the script, not a displayed formula.
    let prose_words = right
        .text
        .split_whitespace()
        .filter(|word| word.len() >= 3 && word.chars().all(|ch| ch.is_ascii_alphabetic()))
        .count();
    if display_equation_glyphs(&left.glyphs)
        || (display_equation_glyphs(&right.glyphs) && prose_words < 2)
    {
        return None;
    }
    let gap = right.left - left.right;
    if gap < size * 0.35 || gap > size * 8.0 {
        return None;
    }
    let right_letters = right
        .glyphs
        .iter()
        .filter(|glyph| {
            glyph.font_size > body * 0.84
                && !math_font_glyph(glyph)
                && glyph.unicode.chars().any(|ch| ch.is_ascii_alphabetic())
        })
        .count();
    if right_letters > 12 {
        return None;
    }
    let bridged = lines.iter().any(|script| {
        script.page == left.page
            && (is_extended_formula_script(script, body) || is_math_script(script))
            && script.left < left.right + size
            && script.right > right.left - size
            && {
                let dy = (script.y - left.y).abs();
                dy >= size * 0.05 && dy <= size * 0.95
            }
    });
    bridged.then_some(next)
}

/// A raised or lowered formula run of any width, including the roman
/// second-level word in `d_{model}`. Body-size prose is not this run.
/// Joined onto its base only when the sentence is math-dense, so a prose
/// line that already has a saved translation keeps its wording.
fn attach_formula_scripts(lines: &mut Vec<VisualLine<'_>>) {
    let hosts: Vec<Option<usize>> = (0..lines.len())
        .map(|index| {
            let body = page_body_size(lines, lines[index].page);
            formula_script_host(index, lines, body)
        })
        .collect();
    let mut incoming: Vec<Vec<usize>> = vec![Vec::new(); lines.len()];
    for (index, host) in hosts.into_iter().enumerate() {
        let Some(host) = host else {
            continue;
        };
        incoming[host].push(index);
    }
    let mut drop = vec![false; lines.len()];
    for (host, scripts) in incoming.iter().enumerate() {
        if scripts.is_empty() || drop[host] {
            continue;
        }
        let mut combined = lines[host].glyphs.clone();
        for script in scripts {
            combined.extend(lines[*script].glyphs.iter().copied());
        }
        if !low_prose_math_sentence(&combined) {
            continue;
        }
        for script in scripts {
            let taken = std::mem::take(&mut lines[*script].glyphs);
            lines[host].glyphs.extend(taken);
            drop[*script] = true;
        }
        lines[host]
            .glyphs
            .sort_by(|left, right| left.matrix[4].total_cmp(&right.matrix[4]));
        lines[host].text = line_text(&lines[host].glyphs);
        if let Some((left, right, y, size)) = line_bounds(&lines[host].glyphs) {
            lines[host].left = left;
            lines[host].right = right;
            lines[host].y = y;
            lines[host].size = size;
        }
    }
    let mut index = 0;
    lines.retain(|_| {
        let keep = !drop[index];
        index += 1;
        keep
    });
}

fn formula_script_host(index: usize, lines: &[VisualLine<'_>], body: f32) -> Option<usize> {
    let line = &lines[index];
    if !is_extended_formula_script(line, body) {
        return None;
    }
    let mut best: Option<(usize, f32)> = None;
    for (other_index, other) in lines.iter().enumerate() {
        if other_index == index || other.page != line.page {
            continue;
        }
        let other_body = page_body_size(lines, other.page);
        if is_math_script(other) || is_extended_formula_script(other, other_body) {
            continue;
        }
        if display_equation_glyphs(&other.glyphs) {
            continue;
        }
        let size = other.size.max(body).max(1.0);
        let dy = (line.y - other.y).abs();
        if dy < size * 0.08 || dy > size * 1.05 {
            continue;
        }
        if !script_sits_on_math(line, other, size) {
            continue;
        }
        if best.is_none_or(|(_, score)| dy < score) {
            best = Some((other_index, dy));
        }
    }
    best.map(|(host, _)| host)
}

fn is_extended_formula_script(line: &VisualLine<'_>, body: f32) -> bool {
    if line.glyphs.len() < 2 || is_math_script(line) {
        return false;
    }
    let ceiling = body.max(1.0) * 0.84;
    let mut math = false;
    for glyph in &line.glyphs {
        if glyph.unicode.trim().is_empty() {
            continue;
        }
        if glyph.font_size > ceiling {
            return false;
        }
        if math_font_glyph(glyph) || vector_symbol_glyph(glyph) {
            math = true;
        }
    }
    // A footnote in a slightly smaller face (`has mean 0 and variance`)
    // is prose. `d_model` has no function word.
    let (function_words, _) = prose_word_counts(&line.glyphs);
    math && function_words == 0
}

fn page_body_size(lines: &[VisualLine<'_>], page: u32) -> f32 {
    let mut sizes = Vec::new();
    for line in lines.iter().filter(|line| line.page == page) {
        let roman = line
            .glyphs
            .iter()
            .filter(|glyph| {
                glyph.font_size >= 8.0
                    && !math_font_glyph(glyph)
                    && glyph.unicode.chars().any(|ch| ch.is_ascii_alphabetic())
            })
            .count();
        if roman >= 8 {
            sizes.push(representative_size(&line.glyphs));
        }
    }
    if sizes.is_empty() {
        return lines
            .iter()
            .filter(|line| line.page == page)
            .map(|line| line.size)
            .fold(1.0f32, f32::max);
    }
    sizes.sort_by(|left, right| left.total_cmp(right));
    sizes[sizes.len() / 2].max(1.0)
}

/// One glyph of the script sits on a math letter of the host (`d` on `R`,
/// `O` on `W`). A prose word at the same x is not that base.
fn script_sits_on_math(script: &VisualLine<'_>, host: &VisualLine<'_>, size: f32) -> bool {
    let size = size.max(1.0);
    script.glyphs.iter().any(|mark| {
        host.glyphs.iter().any(|base| {
            math_font_glyph(base) && {
                let dx = mark.matrix[4] - base.matrix[4];
                dx >= -size * 0.45 && dx <= size * 1.45
            }
        })
    })
}

/// `Where the projections are parameter matrices W_i^Q ∈ R^{d_model×d_k} …`.
/// The formula is most of the line. Translating the words and reflowing the
/// bases leaves the superscripts at the English x.
fn keep_low_prose_math(lines: &[VisualLine<'_>]) -> Vec<(u32, String)> {
    let dense: Vec<bool> = lines
        .iter()
        .map(|line| low_prose_math_sentence(&line.glyphs))
        .collect();
    let mut kept = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if !dense[index] && !formula_sentence_tail(index, lines, &dense) {
            continue;
        }
        // `Where the projections are parameter matrices … and W^O …` is a
        // sentence. The formula runs are placeholders; the words translate.
        // A line that is only the formula stays in the source.
        if prose_letter_count(&line.glyphs) >= 18 || follows_prose_sentence(index, lines) {
            continue;
        }
        for glyph in &line.glyphs {
            kept.push((glyph.id, "math-block".into()));
        }
    }
    kept
}

fn prose_letter_count(glyphs: &[&Glyph]) -> usize {
    let body = math_body_size(glyphs);
    let mut letters = 0usize;
    for glyph in glyphs {
        if glyph.unicode.trim().is_empty() {
            continue;
        }
        let script = glyph.font_size <= body * 0.84;
        if math_font_glyph(glyph) || vector_symbol_glyph(glyph) || script {
            continue;
        }
        letters += glyph
            .unicode
            .chars()
            .filter(|ch| ch.is_ascii_alphabetic())
            .count();
    }
    letters
}

/// `and W^O ∈ R^{…}` sits under the projection sentence. Keeping it leaves
/// the conjunction in English after the sentence above it is translated.
fn follows_prose_sentence(index: usize, lines: &[VisualLine<'_>]) -> bool {
    let line = &lines[index];
    // A raised script (`2i/d_model`) is part of the equation above it. Only a
    // line that still has a function word (`and W^O`) follows the sentence.
    let (function_words, _) = prose_word_counts(&line.glyphs);
    if function_words == 0 {
        return false;
    }
    let size = line.size.max(1.0);
    lines.iter().enumerate().any(|(other, host)| {
        other != index
            && host.page == line.page
            && host.y > line.y
            && host.y - line.y <= size * 2.5
            && (line.left - host.left).abs() <= size * 4.0
            && prose_letter_count(&host.glyphs) >= 18
    })
}

fn formula_sentence_tail(index: usize, lines: &[VisualLine<'_>], dense: &[bool]) -> bool {
    let line = &lines[index];
    if dense[index] || display_equation_glyphs(&line.glyphs) {
        return false;
    }
    let letters = line
        .text
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .count();
    if !(1..=8).contains(&letters) || !line.glyphs.iter().any(|glyph| math_font_glyph(glyph)) {
        return false;
    }
    let size = line.size.max(1.0);
    lines.iter().enumerate().any(|(other, host)| {
        dense[other]
            && host.page == line.page
            && host.y > line.y
            && host.y - line.y <= size * 2.5
            && (line.left - host.left).abs() <= size * 4.0
    })
}

/// Prose share is the width of body words over body words plus the formula.
/// High means the line is still a sentence (`In this work we employ h = 8`).
/// A display equation is not this decision; it already stays original.
fn low_prose_math_sentence(glyphs: &[&Glyph]) -> bool {
    if glyphs.len() < 4 || display_equation_glyphs(glyphs) {
        return false;
    }
    let body = math_body_size(glyphs);
    let mut prose = 0.0f32;
    let mut formula = 0.0f32;
    let mut prose_letters = 0usize;
    for glyph in glyphs {
        if glyph.unicode.trim().is_empty() {
            continue;
        }
        let width = glyph_ink_width(glyph).max(body * 0.15);
        let letters = glyph
            .unicode
            .chars()
            .filter(|ch| ch.is_ascii_alphabetic())
            .count();
        let script = glyph.font_size <= body * 0.84;
        if math_font_glyph(glyph) || vector_symbol_glyph(glyph) || script {
            formula += width;
        } else if letters > 0 {
            prose += width;
            prose_letters += letters;
        }
    }
    if prose_letters < 3 || formula < body * 4.0 {
        return false;
    }
    prose / (prose + formula) < 0.60
}

fn math_body_size(glyphs: &[&Glyph]) -> f32 {
    let max_size = glyphs
        .iter()
        .map(|glyph| glyph.font_size)
        .fold(1.0f32, f32::max);
    let mut sizes: Vec<f32> = glyphs
        .iter()
        .map(|glyph| glyph.font_size)
        .filter(|size| *size >= max_size * 0.8)
        .collect();
    if sizes.is_empty() {
        return max_size.max(1.0);
    }
    sizes.sort_by(|left, right| left.total_cmp(right));
    sizes[sizes.len() / 2].max(1.0)
}

fn glyph_ink_width(glyph: &Glyph) -> f32 {
    let box_w = (glyph.bbox[2] - glyph.bbox[0]).abs();
    box_w.max(glyph.advance[0].abs())
}

/// A TeX vector arrow (`#»`) drawn a little under the word it marks.
/// It is not a text font, so the math-font check does not see it.
fn vector_symbol_glyph(glyph: &Glyph) -> bool {
    glyph.font_name.to_ascii_uppercase().contains("VECT")
        && !glyph.unicode.chars().any(|ch| ch.is_ascii_alphabetic())
}

/// Operators and digits that a CJK body font can redraw. They show up in
/// TeX as one-glyph math fonts (`$9.7\times$`) inside a prose line.
pub(crate) fn is_inline_math_symbol(text: &str) -> bool {
    let mut chars = text.trim().chars();
    let Some(ch) = chars.next() else {
        return false;
    };
    if chars.next().is_some() {
        return false;
    }
    matches!(
        ch,
        '.' | ','
            | ':'
            | ';'
            | '+'
            | '-'
            | '='
            | '/'
            | '('
            | ')'
            | '['
            | ']'
            | '×'
            | '·'
            | '±'
            | '≤'
            | '≥'
            | '≈'
            | '≠'
            | '∞'
            | '°'
            | '%'
            | '<'
            | '>'
            | '−'
            | '–'
            | '—'
            | '0'..='9'
    )
}

/// `“ dromes` finishes `palin-`. The quote is the prompt mark, not a new sentence.
fn quoted_hyphen_tail(text: &str) -> bool {
    let rest = text.trim_start();
    let mut chars = rest.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !matches!(first, '“' | '”' | '"' | '‘' | '’') {
        return false;
    }
    chars
        .find(|ch| !ch.is_whitespace())
        .is_some_and(|ch| ch.is_ascii_alphanumeric())
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

/// A wrapped URL continues on the next line (`and` + `-applications`,
/// `manning` + `.com/...`, or `www.../ai-agents` + `-and-applications`).
/// The join keeps the hyphen so the shield sees one token.
fn url_continues(buf: &str, next: &str) -> bool {
    let buf = buf.trim_end();
    let next = next.trim_start();
    let Some(token) = buf.split_whitespace().last() else {
        return false;
    };
    if !looks_like_url_token(token) {
        return false;
    }
    let mut chars = next.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let Some(second) = chars.next() else {
        return false;
    };
    second.is_ascii_alphanumeric()
        && matches!(
            first,
            '-' | '.' | '/' | '%' | '?' | '&' | '=' | '#' | '_' | '~'
        )
}

fn join_hyphenated_word(stem: &str, rest: &str) -> String {
    // `GPT-2`, `round-to-nearest`, and `https://...` keep the hyphen.
    // An ordinary break (`transfor-` / `mation`) does not.
    let next_token = rest.split_whitespace().next().unwrap_or("");
    if line_ends_with_url(stem)
        || rest
            .trim_start()
            .starts_with(|ch: char| ch.is_ascii_digit())
        || next_token.contains('-')
    {
        return format!("{stem}-{rest}");
    }
    let stem_word = stem.split_whitespace().last().unwrap_or(stem);
    let next_word = rest
        .split_whitespace()
        .next()
        .unwrap_or(rest)
        .trim_matches(|ch: char| !ch.is_ascii_alphabetic());
    // `Human-in-the-` / `loop` already has hyphens in the stem. The break
    // hyphen is part of the compound, not a soft hyphen inside a word.
    if stem_word.contains('-')
        || (is_hard_prefix(stem_word) && next_word.len() >= 3 && !is_hyphen_suffix(next_word))
    {
        format!("{stem}-{rest}")
    } else {
        format!("{stem}{rest}")
    }
}

fn is_hard_prefix(word: &str) -> bool {
    // Prefixes that almost always start a compound (`multi-document`).
    // `pre`, `over`, and `inter` are omitted: papers break ordinary words
    // after them (`pre-serve`, `over-flow`, `inter-mediate`).
    const PREFIXES: &[&str] = &[
        "multi", "self", "semi", "cross", "meta", "pseudo", "ultra", "micro", "macro", "anti",
        "non",
    ];
    let word = word.to_ascii_lowercase();
    PREFIXES.contains(&word.as_str())
}

fn line_ends_with_url(text: &str) -> bool {
    text.split_whitespace()
        .last()
        .is_some_and(looks_like_url_token)
}

fn looks_like_url_token(token: &str) -> bool {
    if token.starts_with("https://") || token.starts_with("http://") || token.starts_with("www.") {
        return true;
    }
    token.contains('.') && token.contains('/') && !token.contains('@')
}

fn is_hyphen_suffix(word: &str) -> bool {
    let word = word.to_ascii_lowercase();
    const SUFFIXES: &[&str] = &[
        "ed", "ing", "tion", "sion", "ified", "ally", "ment", "ness", "able", "ible", "ence",
        "ance", "ous", "ive", "ers", "ly", "es", "er", "al", "ity", "or", "ions", "ted", "ned",
        "red", "ies", "ability", "ibility", "ation", "ition", "ful", "less", "ship", "hood",
    ];
    SUFFIXES.contains(&word.as_str())
}

/// The body margin stays the column anchor, so a list continuation that
/// sits one hanging step further right (`grounded systems.`) used to start
/// a new column unless the line above ended in a hyphen.
fn hanging_pull(prev: &VisualLine<'_>, line: &VisualLine<'_>) -> bool {
    if prev.page != line.page {
        return false;
    }
    let size = prev.size.max(line.size).max(1.0);
    let dy = prev.y - line.y;
    if dy < size * 0.7 || dy > size * 1.6 {
        return false;
    }
    let shift = line.left - prev.left;
    (-(size * 1.6)..=size * 2.8).contains(&shift)
}

/// A hanging indent that finishes `multi-` / `per-` belongs to that column
/// even when the column's median left edge is the body margin.
fn hyphen_pull(prev: &VisualLine<'_>, line: &VisualLine<'_>) -> bool {
    let shift = line.left - prev.left;
    let size = prev.size.max(line.size).max(1.0);
    let tail = line.text.trim_start();
    prev.page == line.page
        && soft_hyphen_stem(&prev.text).is_some()
        && (tail.starts_with(|ch: char| ch.is_ascii_alphabetic()) || quoted_hyphen_tail(tail))
        // The continuation may hang a little to the right, or step back
        // onto the column margin. A wide math gap can leave the hyphen on
        // the right half of the line, so the step back is more than 64pt.
        // The other column is still further left than 12em.
        && (-(size * 12.0)..=36.0).contains(&shift)
        && prev.y > line.y
        && prev.y - line.y < size * 1.6
}

/// `83 Generating` on one baseline is the next contents entry, not a sentence.
/// A bare page number only: `1:` and `(3)` stay with the prose around them.
/// A quantity in a sentence is not a page number: `2 BLEU`, `8 NVIDIA`, and
/// `8 P100` used to match, and the line was translated as its own paragraph.
fn inline_contents_entry(text: &str) -> bool {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    tokens.windows(2).any(|pair| {
        (1..=3).contains(&pair[0].len())
            && pair[0].bytes().all(|byte| byte.is_ascii_digit())
            && contents_title_word(pair[1])
    })
}

/// `Generating` and `The`, not an acronym or a model name (`BLEU`, `NVIDIA`,
/// `P100`, `GPUs`). The word after a contents page number is title case.
fn contents_title_word(token: &str) -> bool {
    let token =
        token.trim_matches(|ch: char| matches!(ch, ',' | '.' | ':' | ';' | ')' | '(' | '“' | '”'));
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_uppercase() || token.chars().count() < 3 {
        return false;
    }
    let rest: Vec<char> = chars.collect();
    rest.len() >= 2 && rest.iter().all(|ch| ch.is_ascii_lowercase())
}

fn toc_entry_boundary(upper: &VisualLine<'_>, lower: &VisualLine<'_>) -> bool {
    if contents_marker_line(&upper.text)
        && (contents_marker_line(&lower.text) || starts_new_contents_entry(&lower.text))
    {
        return true;
    }
    // "1.1. Notation" and "1.2. Sturmian" are two contents rows. The page
    // number may already have been split into the right margin.
    if dotted_section_heading(&upper.text) && dotted_section_heading(&lower.text) {
        return true;
    }
    ends_with_page_number(&upper.text) && starts_new_contents_entry(&lower.text)
}

fn dotted_section_heading(text: &str) -> bool {
    let Some(token) = text.split_whitespace().next() else {
        return false;
    };
    let trimmed = token.trim_end_matches('.');
    trimmed.contains('.') && is_section_number_token(trimmed)
}

fn contents_marker_line(text: &str) -> bool {
    let mut core = String::new();
    let mut bullet = false;
    for ch in text.chars() {
        if ch.is_whitespace() || is_toc_bullet(ch) {
            bullet |= is_toc_bullet(ch);
            continue;
        }
        core.push(ch);
    }
    if core.is_empty() {
        return bullet;
    }
    is_section_number_token(&core)
}

fn is_toc_bullet(ch: char) -> bool {
    matches!(ch, '■' | '▪' | '●' | '•' | '◦' | '·')
}

fn is_section_number_token(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let mut index = 0;
    let mut groups = 0;
    while index < bytes.len() {
        if groups > 0 {
            if bytes[index] != b'.' {
                return false;
            }
            index += 1;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index == start || index - start > 2 {
            return false;
        }
        groups += 1;
        if groups > 4 {
            return false;
        }
    }
    groups >= 1
}

fn starts_new_contents_entry(text: &str) -> bool {
    let text = text.trim_start();
    let Some(first) = text.chars().next() else {
        return false;
    };
    if is_toc_bullet(first) || starts_with_section_number(text) {
        return true;
    }
    first.is_ascii_uppercase()
}

fn starts_with_section_number(text: &str) -> bool {
    let Some(token) = text.split_whitespace().next() else {
        return false;
    };
    // "1.1." is a section number. A bare "1." is a list item, not one.
    let trimmed = token.trim_end_matches('.');
    if trimmed.contains('.') {
        return is_section_number_token(trimmed);
    }
    is_section_number_token(token)
}

fn ends_with_page_number(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    if lower.starts_with("figure")
        || lower.starts_with("fig.")
        || lower.starts_with("fig ")
        || lower.starts_with("table")
        || lower.starts_with("tab.")
        || lower.starts_with("tab ")
    {
        return false;
    }
    let text = text.trim_end();
    let Some(token) = text.split_whitespace().last() else {
        return false;
    };
    if token.is_empty() || token.len() > 3 || !token.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    text.split_whitespace().count() >= 2
}

fn is_footer_line(text: &str) -> bool {
    text.trim().to_ascii_lowercase().starts_with("licensed to")
}

fn is_narrow_folio(line: &VisualLine<'_>) -> bool {
    let width = line.right - line.left;
    width <= 48.0 && is_page_folio_text(&line.text)
}

fn is_page_folio_text(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > 6 {
        return false;
    }
    text.chars().all(|ch| ch.is_ascii_digit()) || is_roman_numeral(text)
}

fn is_roman_numeral(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    if lower.is_empty() || lower.len() > 8 {
        return false;
    }
    let bytes = lower.as_bytes();
    let mut index = 0;
    let mut ems = 0;
    while index < bytes.len() && bytes[index] == b'm' && ems < 4 {
        index += 1;
        ems += 1;
    }
    index = eat_roman_group(bytes, index, b'c', b'd', b'm');
    index = eat_roman_group(bytes, index, b'x', b'l', b'c');
    index = eat_roman_group(bytes, index, b'i', b'v', b'x');
    index == bytes.len()
}

fn eat_roman_group(bytes: &[u8], index: usize, one: u8, five: u8, ten: u8) -> usize {
    if index >= bytes.len() {
        return index;
    }
    if bytes[index] == one
        && index + 1 < bytes.len()
        && (bytes[index + 1] == five || bytes[index + 1] == ten)
    {
        return index + 2;
    }
    let mut index = index;
    if index < bytes.len() && bytes[index] == five {
        index += 1;
    }
    let mut count = 0;
    while index < bytes.len() && bytes[index] == one && count < 3 {
        index += 1;
        count += 1;
    }
    index
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
    fn a_same_baseline_column_gutter_is_its_own_segment() {
        let mut glyphs = Vec::new();
        for (index, ch) in ["L", "e", "f", "t"].iter().enumerate() {
            glyphs.push(glyph(
                index as u32,
                72.0 + index as f32 * 8.0,
                500.0,
                ch,
                false,
            ));
        }
        for (index, ch) in ["R", "i", "g", "h", "t"].iter().enumerate() {
            glyphs.push(glyph(
                10 + index as u32,
                340.0 + index as f32 * 8.0,
                500.0,
                ch,
                false,
            ));
        }
        let segs = segment_glyphs(&glyphs);
        let texts: Vec<_> = segs.iter().map(|segment| segment.text.as_str()).collect();
        assert_eq!(texts, ["Left", "Right"]);
    }

    #[test]
    fn a_title_case_header_row_splits_on_each_word() {
        let words = ["Trigger", "Optimization", "Context"];
        let mut glyphs = Vec::new();
        let mut id = 1u32;
        let mut x = 100.0f32;
        for word in words {
            for ch in word.chars() {
                glyphs.push(glyph(id, x, 500.0, &ch.to_string(), false));
                id += 1;
                x += 6.0;
            }
            x += 10.0;
        }
        let segs = segment_glyphs(&glyphs);
        let texts: Vec<_> = segs.iter().map(|segment| segment.text.as_str()).collect();
        assert_eq!(texts, ["Trigger", "Optimization", "Context"]);
    }

    #[test]
    fn a_table_group_label_leaves_the_grid_and_a_method_cell_stays() {
        let mut caption = glyph(1, 72.0, 700.0, "Table 1. Scores.", false);
        caption.font_name = "NimbusRomNo9L-Regu".into();
        let mut label = glyph(2, 72.0, 640.0, "Full-history baseline", false);
        label.font_name = "NimbusRomNo9L-Medi".into();
        let mut method = glyph(3, 72.0, 620.0, "Fixed Compaction", false);
        method.font_name = "NimbusRomNo9L-Medi".into();
        let score = glyph(4, 280.0, 620.0, "42.0", false);
        let glyphs = [caption, label, method, score];
        let line = |index: usize, y: f32, sole: bool| VisualLine {
            glyphs: vec![&glyphs[index]],
            text: glyphs[index].unicode.clone(),
            page: 0,
            y,
            left: glyphs[index].matrix[4],
            right: glyphs[index].matrix[4] + 180.0,
            size: 12.0,
            font: glyphs[index].font_name.clone(),
            toc: false,
            sole_on_baseline: sole,
        };
        let lines = vec![
            line(0, 700.0, true),
            line(1, 640.0, true),
            line(2, 620.0, false),
            line(3, 620.0, false),
        ];
        let mut kept = vec![
            (2, "table".into()),
            (3, "table".into()),
            (4, "table".into()),
        ];
        release_table_group_labels(&lines, &mut kept);
        assert!(
            !kept.iter().any(|(id, _)| *id == 2),
            "group label stayed in the table: {kept:?}"
        );
        assert!(
            kept.iter().any(|(id, _)| *id == 3),
            "method cell was released: {kept:?}"
        );
    }

    #[test]
    fn a_bold_header_row_stays_in_the_table() {
        let mut caption = glyph(1, 72.0, 700.0, "Table 3. Demonstrations.", false);
        caption.font_name = "NimbusRomNo9L-Regu".into();
        let mut first = glyph(2, 72.0, 680.0, "Demonstration", false);
        first.font_name = "NimbusRomNo9L-Medi".into();
        let mut second = glyph(3, 300.0, 680.0, "Location (text body)", false);
        second.font_name = "NimbusRomNo9L-Medi".into();
        let glyphs = [caption, first, second];
        let line = |index: usize, y: f32, width: f32| VisualLine {
            glyphs: vec![&glyphs[index]],
            text: glyphs[index].unicode.clone(),
            page: 0,
            y,
            left: glyphs[index].matrix[4],
            right: glyphs[index].matrix[4] + width,
            size: 12.0,
            font: glyphs[index].font_name.clone(),
            toc: false,
            sole_on_baseline: true,
        };
        let lines = vec![
            line(0, 700.0, 180.0),
            line(1, 680.0, 90.0),
            line(2, 680.0, 120.0),
        ];
        let mut kept = vec![(2, "table".into()), (3, "table".into())];
        release_table_group_labels(&lines, &mut kept);
        assert_eq!(kept.len(), 2, "header cells were released: {kept:?}");
    }

    #[test]
    fn a_table_of_words_between_three_rules_is_kept() {
        let texts = [
            (1, 60.0, 720.0, "Factor"),
            (2, 300.0, 720.0, "Applies to"),
            (3, 60.0, 700.0, "Metric"),
            (4, 300.0, 700.0, "Our scored comparisons"),
            (5, 60.0, 685.0, "Backbone"),
            (6, 300.0, 685.0, "Direct pair"),
            (7, 60.0, 670.0, "Reference answers"),
            (8, 300.0, 670.0, "Our agent evaluations"),
            (9, 60.0, 600.0, "A paragraph between other rules."),
        ];
        let glyphs: Vec<Glyph> = texts
            .iter()
            .map(|(id, x, y, text)| glyph(*id, *x, *y, text, false))
            .collect();
        let lines: Vec<VisualLine<'_>> = glyphs
            .iter()
            .map(|g| VisualLine {
                glyphs: vec![g],
                text: g.unicode.clone(),
                page: 0,
                y: g.matrix[5],
                left: g.matrix[4],
                right: g.matrix[4] + 80.0,
                size: 9.0,
                font: g.font_name.clone(),
                toc: false,
                sole_on_baseline: false,
            })
            .collect();
        let rule = |y: f32| PaintedRegion {
            page_index: 0,
            bbox: [50.0, y, 550.0, y],
            kind: "path".into(),
        };
        let regions = vec![rule(730.0), rule(712.0), rule(660.0), rule(620.0)];
        let added = worded_ruled_tables(&lines, &[], &regions);
        let mut ids: Vec<_> = added.iter().map(|(id, _)| *id).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2, 3, 4, 5, 6, 7, 8], "{added:?}");
    }

    #[test]
    fn a_cell_wider_than_the_caption_stays_between_the_rules() {
        let caption = glyph(1, 200.0, 760.0, "Table 3. Demonstrations.", false);
        let middle = glyph(2, 240.0, 718.0, "Influence the result", false);
        let low = glyph(3, 240.0, 700.0, "T5 injection", false);
        let edge = glyph(4, 47.0, 718.0, "Exp. 1", false);
        let below = glyph(5, 47.0, 600.0, "Body text after the table.", false);
        let glyphs = [caption, middle, low, edge, below];
        let line = |index: usize, y: f32, width: f32| VisualLine {
            glyphs: vec![&glyphs[index]],
            text: glyphs[index].unicode.clone(),
            page: 0,
            y,
            left: glyphs[index].matrix[4],
            right: glyphs[index].matrix[4] + width,
            size: 9.0,
            font: glyphs[index].font_name.clone(),
            toc: false,
            sole_on_baseline: false,
        };
        let lines = vec![
            line(0, 760.0, 200.0),
            line(1, 718.0, 80.0),
            line(2, 700.0, 80.0),
            line(3, 718.0, 30.0),
            line(4, 600.0, 300.0),
        ];
        let rule = |y: f32| PaintedRegion {
            page_index: 0,
            bbox: [37.0, y, 557.0, y],
            kind: "path".into(),
        };
        let regions = vec![rule(745.0), rule(729.0), rule(680.0)];
        let kept = vec![(2, "table".to_string()), (3, "table".to_string())];
        let added = ruled_table_rows(&lines, &kept, &regions);
        let ids: Vec<_> = added.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![4], "{added:?}");
    }

    #[test]
    fn a_panel_title_under_a_table_caption_stays_in_the_figure() {
        let mut caption = glyph(
            1,
            108.0,
            700.0,
            "Table 1: Pass rate versus inference budget.",
            false,
        );
        caption.font_name = "NimbusRomNo9L-Regu".into();
        let mut title = glyph(
            2,
            328.0,
            500.0,
            "(d) Following vs. ignoring summaries",
            false,
        );
        title.font_name = "InterTight-SemiBold".into();
        let glyphs = [caption, title];
        let line = |index: usize, y: f32, right: f32| VisualLine {
            glyphs: vec![&glyphs[index]],
            text: glyphs[index].unicode.clone(),
            page: 0,
            y,
            left: glyphs[index].matrix[4],
            right,
            size: 9.0,
            font: glyphs[index].font_name.clone(),
            toc: false,
            sole_on_baseline: true,
        };
        let lines = vec![line(0, 700.0, 400.0), line(1, 500.0, 451.0)];
        let mut kept = vec![(2, "figure".into())];
        release_table_group_labels(&lines, &mut kept);
        assert!(
            kept.iter()
                .any(|(id, reason)| *id == 2 && reason == "figure"),
            "panel title was translated with the group labels: {kept:?}"
        );
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
    fn a_float_sized_baseline_stays_one_line() {
        // The right-hand glyphs sit 0.0002pt higher, so they sort first.
        // The left-hand word then looks like a jump to the other column.
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        let mut paint = |text: &str, x: f32, y: f32, size: f32| {
            let mut cursor = x;
            for ch in text.chars() {
                let mut item = glyph(id, cursor, y, &ch.to_string(), false);
                id += 1;
                item.font_size = size;
                item.matrix = [size, 0.0, 0.0, size, cursor, y];
                let width = if ch == ' ' { size * 0.3 } else { size * 0.5 };
                item.bbox = [cursor, y - 2.0, cursor + width, y + size * 0.8];
                item.advance = [width, 0.0];
                cursor += width;
                glyphs.push(item);
            }
        };
        paint("like", 102.0, 262.3198, 8.1);
        // A word space, not a column gutter. The back-jump is the last glyph.
        paint(" to separate", 122.0, 262.32, 10.0);
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(
            texts[0].contains("like") && texts[0].contains("separate"),
            "{texts:?}"
        );
    }

    #[test]
    fn a_chapter_index_stays_above_the_section_title() {
        let mut glyphs = Vec::new();
        for (index, ch) in ["1", "0"].iter().enumerate() {
            let x = 93.0 + index as f32 * 14.0;
            let mut item = glyph(index as u32, x, 251.2, ch, false);
            item.font_size = 27.0;
            item.matrix = [27.0, 0.0, 0.0, 27.0, x, 251.2];
            item.bbox = [x, 251.2, x + 13.0, 278.0];
            item.advance = [13.0, 0.0];
            glyphs.push(item);
        }
        let mut cursor = 133.0f32;
        for (id, ch) in (2u32..).zip("10.1 Title".chars()) {
            let mut item = glyph(id, cursor, 241.7, &ch.to_string(), false);
            item.font_size = 11.0;
            item.matrix = [11.0, 0.0, 0.0, 11.0, cursor, 241.7];
            let width = if ch == ' ' { 3.0 } else { 5.5 };
            item.bbox = [cursor, 239.0, cursor + width, 250.0];
            item.advance = [width, 0.0];
            cursor += width;
            glyphs.push(item);
        }
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.trim() == "10"),
            "chapter index merged into the section: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("10.1") && text.contains("Title")),
            "{texts:?}"
        );
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
    fn a_hyphen_tail_off_the_baseline_is_not_a_justified_word() {
        // `accu-` wraps as `racy.` one point below the other column. The gap
        // is a gutter, not the last word of `WinoGrande.`
        let mut glyphs = Vec::new();
        for index in 0..36 {
            let x = 70.0 + index as f32 * 6.0;
            let mut item = glyph(index, x, 597.0, "a", false);
            item.font_size = 10.0;
            item.matrix = [10.0, 0.0, 0.0, 10.0, x, 597.0];
            item.bbox = [x, 597.0, x + 5.5, 607.0];
            glyphs.push(item);
        }
        let left_right = 70.0 + 35.0 * 6.0 + 5.5;
        for (index, ch) in "racy.".chars().enumerate() {
            let x = left_right + 15.0 + index as f32 * 5.0;
            let mut item = glyph(100 + index as u32, x, 595.6, &ch.to_string(), false);
            item.font_size = 10.0;
            item.matrix = [10.0, 0.0, 0.0, 10.0, x, 595.6];
            item.bbox = [x, 595.6, x + 4.5, 605.6];
            glyphs.push(item);
        }
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains('a') && !text.contains("racy")),
            "{texts:?}"
        );
        assert!(texts.iter().any(|text| text.contains("racy")), "{texts:?}");
    }

    #[test]
    fn a_right_column_list_continues_on_the_next_left_column() {
        // LLaMA's right column ends `SIQA (Sap et al., 2019),`. The next page's
        // left column is `HellaSwag`, and `school Chinese students` is the
        // right column under the table. Reading order takes the left column.
        let mut upper = glyph(
            0,
            306.0,
            71.0,
            "PIQA (Bisk et al., 2020), SIQA (Sap et al., 2019),",
            false,
        );
        upper.font_size = 10.0;
        let mut left = glyph(
            1,
            70.0,
            760.0,
            "HellaSwag (Zellers et al., 2019), WinoGrande and OpenBookQA.",
            false,
        );
        left.page_index = 1;
        left.font_size = 10.0;
        let mut right = glyph(
            2,
            306.0,
            560.0,
            "school Chinese students. We follow the evaluation setup.",
            false,
        );
        right.page_index = 1;
        right.font_size = 10.0;
        let texts: Vec<_> = segment_glyphs(&[upper, left, right])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("SIQA") && text.contains("HellaSwag")),
            "{texts:?}"
        );
        assert!(
            !texts
                .iter()
                .any(|text| text.contains("SIQA") && text.contains("school Chinese")),
            "{texts:?}"
        );
    }

    #[test]
    fn an_abstract_beside_an_unfinished_introduction_does_not_block_the_next_page() {
        let mut intro = glyph(0, 108.0, 81.0, "and shapes (Cai", false);
        intro.font_size = 10.0;
        let mut abstract_body = glyph(
            1,
            320.0,
            507.0,
            "Creating noise from data is easy and this abstract stands on its own.",
            false,
        );
        abstract_body.font_size = 10.0;
        let mut next = glyph(
            2,
            108.0,
            563.0,
            "et al., 2020). To enable new sampling methods and further models.",
            false,
        );
        next.page_index = 1;
        next.font_size = 10.0;
        let texts: Vec<_> = segment_glyphs(&[intro, abstract_body, next])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Cai") && text.contains("et al.")),
            "{texts:?}"
        );
        assert!(
            !texts
                .iter()
                .any(|text| text.contains("Cai") && text.contains("Creating noise")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_staggered_column_with_a_17pt_gutter_is_split() {
        // The left line sits 2.5pt above the right line, inside the same-line
        // tolerance, and the gutter left between them is only 17pt.
        let mut glyphs = Vec::new();
        for index in 0..12 {
            let x = 70.0 + index as f32 * 18.0;
            let mut item = glyph(index, x, 304.0, "a", false);
            item.font_size = 10.0;
            item.bbox = [x, 304.0, x + 16.0, 314.0];
            glyphs.push(item);
        }
        for index in 0..12 {
            let x = 301.0 + index as f32 * 18.0;
            let mut item = glyph(20 + index, x, 301.5, "b", false);
            item.font_size = 10.0;
            item.bbox = [x, 301.5, x + 16.0, 311.5];
            glyphs.push(item);
        }
        let segs = segment_glyphs(&glyphs);
        assert_eq!(
            segs.len(),
            2,
            "{:?}",
            segs.iter().map(|s| &s.text).collect::<Vec<_>>()
        );
        assert!(segs
            .iter()
            .any(|seg| seg.text.chars().all(|ch| ch == 'a' || ch == ' ')));
        assert!(segs
            .iter()
            .any(|seg| seg.text.chars().all(|ch| ch == 'b' || ch == ' ')));
    }

    #[test]
    fn a_12pt_gutter_on_a_full_two_column_line_is_split() {
        // PubLayNet's abstract shares a baseline. The gutter is 12pt and the
        // em boxes overlap, so the median gap is ~0 and the 1.25em trigger
        // sits just above the gutter.
        let mut glyphs = Vec::new();
        for index in 0..60 {
            let (start, slot) = if index < 30 {
                (50.0, index)
            } else {
                // Left ink ends at 50+29*8.2+8 = 295.8. Right starts 12pt later.
                (307.8, index - 30)
            };
            let x = start + slot as f32 * 8.2;
            let mut item = glyph(index as u32, x, 565.0, "a", false);
            item.font_size = 10.0;
            item.matrix = [10.0, 0.0, 0.0, 10.0, x, 565.0];
            item.bbox = [x, 565.0, x + 8.0, 573.0];
            glyphs.push(item);
        }
        let segs = segment_glyphs(&glyphs);
        assert_eq!(
            segs.len(),
            2,
            "{:?}",
            segs.iter().map(|segment| &segment.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_hyphen_broken_line_does_not_take_the_next_columns_tail() {
        // `…music-domain conversa-` then `data.` 15pt later on one baseline.
        let mut glyphs = Vec::new();
        let mut x = 70.0f32;
        let mut id = 0u32;
        for ch in "synthesizemusicdomainconversa-".chars() {
            let mut item = glyph(id, x, 638.0, &ch.to_string(), false);
            item.font_size = 10.9;
            item.matrix = [10.9, 0.0, 0.0, 10.9, x, 638.0];
            item.bbox = [x, 636.0, x + 5.0, 646.0];
            glyphs.push(item);
            x += 7.3;
            id += 1;
        }
        x += 15.0;
        for ch in "data.".chars() {
            let mut item = glyph(id, x, 638.0, &ch.to_string(), false);
            item.font_size = 10.9;
            item.matrix = [10.9, 0.0, 0.0, 10.9, x, 638.0];
            item.bbox = [x, 636.0, x + 5.0, 646.0];
            glyphs.push(item);
            x += 5.0;
            id += 1;
        }
        let segs = segment_glyphs(&glyphs);
        assert!(
            segs.iter()
                .all(|segment| !segment.text.contains("-d") && !segment.text.contains("- d")),
            "{:?}",
            segs.iter().map(|segment| &segment.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_wide_word_space_at_the_end_of_one_column_stays_in_the_line() {
        // `the same document from PubMed Central are used to`: a 14pt
        // justified space is not a column. The tail is only a few words.
        let mut glyphs = Vec::new();
        for index in 0..34 {
            let (x, text) = if index < 24 {
                (312.0 + index as f32 * 6.0, "a")
            } else {
                // 24 steps land one point past the last ink. The extra 13pt
                // makes a 14pt word space before the short tail.
                (312.0 + 24.0 * 6.0 + 13.0 + (index - 24) as f32 * 6.0, "b")
            };
            let mut item = glyph(index as u32, x, 174.0, text, false);
            item.font_size = 10.0;
            item.matrix = [10.0, 0.0, 0.0, 10.0, x, 174.0];
            item.bbox = [x, 174.0, x + 5.0, 182.0];
            glyphs.push(item);
        }
        let segs = segment_glyphs(&glyphs);
        assert_eq!(
            segs.len(),
            1,
            "{:?}",
            segs.iter().map(|s| &s.text).collect::<Vec<_>>()
        );
        assert!(segs[0].text.contains('b'));
    }

    #[test]
    fn a_lone_brace_at_the_column_edge_splits_off() {
        // `references column, {` The brace is the listing in the next column.
        let mut glyphs = Vec::new();
        for index in 0..30 {
            let x = 70.0 + index as f32 * 5.5;
            let mut item = glyph(index as u32, x, 611.0, "a", false);
            item.font_size = 10.0;
            item.matrix = [10.0, 0.0, 0.0, 10.0, x, 611.0];
            item.bbox = [x, 611.0, x + 4.8, 619.0];
            glyphs.push(item);
        }
        let x = 70.0 + 30.0 * 5.5 + 15.0;
        let mut brace = glyph(100, x, 611.0, "{", false);
        brace.font_size = 10.0;
        brace.matrix = [10.0, 0.0, 0.0, 10.0, x, 611.0];
        brace.bbox = [x, 611.0, x + 5.0, 619.0];
        glyphs.push(brace);
        let segs = segment_glyphs(&glyphs);
        assert!(
            segs.iter()
                .any(|segment| segment.text.contains('{') && !segment.text.contains('a')),
            "{:?}",
            segs.iter().map(|segment| &segment.text).collect::<Vec<_>>()
        );
        assert!(
            segs.iter()
                .any(|segment| segment.text.contains('a') && !segment.text.contains('{')),
            "{:?}",
            segs.iter().map(|segment| &segment.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn digest_page4_chart_labels_leave_the_prose() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/cache/arxiv-2410.22360.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(&path).unwrap();
        let extraction = doc.extract_with(&crate::extract::ExtractOptions::default());
        let seg = segment_placed(
            &extraction.glyphs,
            &SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let hits: Vec<_> = seg
            .segments
            .iter()
            .filter(|item| item.page_index == 3 && item.text.contains("Physics"))
            .map(|item| item.text.clone())
            .collect();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(
            !hits[0].contains("igQ") && !hits[0].contains("H,ig") && !hits[0].contains("83.75"),
            "{}",
            hits[0]
        );
        assert!(
            hits[0].contains("Physics") && hits[0].contains("Quantitative")
                || hits[0].contains("Quantita"),
            "{}",
            hits[0]
        );
        let caption = seg
            .segments
            .iter()
            .find(|item| item.text.contains("Table 2:"))
            .expect("table 2 caption");
        assert!(
            !caption.text.contains("types of aspects represented"),
            "{}",
            caption.text
        );
        assert!(
            caption.text.contains("columns"),
            "caption wrap dropped: {}",
            caption.text
        );
        assert!(!hits[0].contains("Table 2"), "{}", hits[0]);
    }

    #[test]
    fn a_chart_label_on_the_prose_does_not_interleave() {
        // `Highest` is painted over `Physics,`. One line used to read
        // `PhysicsH,ighest`.
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        let mut cursor = 300.0f32;
        for ch in "Physics, Quantita".chars() {
            let mut item = glyph(id, cursor, 400.0, &ch.to_string(), false);
            id += 1;
            item.font_size = 10.0;
            item.matrix = [10.0, 0.0, 0.0, 10.0, cursor, 400.0];
            let width = if ch == ' ' { 3.0 } else { 5.0 };
            item.bbox = [cursor, 398.0, cursor + width, 408.0];
            glyphs.push(item);
            cursor += width;
        }
        cursor = 332.0;
        for ch in "Highest".chars() {
            let mut item = glyph(id, cursor, 401.2, &ch.to_string(), false);
            id += 1;
            item.font_size = 13.0;
            item.matrix = [13.0, 0.0, 0.0, 13.0, cursor, 401.2];
            item.bbox = [cursor, 399.0, cursor + 7.0, 412.0];
            glyphs.push(item);
            cursor += 7.0;
        }
        let segs = segment_glyphs(&glyphs);
        let texts: Vec<_> = segs.iter().map(|segment| segment.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Physics") && !text.contains('H')),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Highest") && !text.contains("Physics")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_one_word_right_column_splits_on_a_17pt_gutter() {
        // `can be` | `challenges:` The right piece is under 6em, and the
        // line still spans both columns.
        let mut glyphs = Vec::new();
        for index in 0..42 {
            let x = 71.0 + index as f32 * 5.2;
            let mut item = glyph(index as u32, x, 471.0, "a", false);
            item.font_size = 10.0;
            item.matrix = [10.0, 0.0, 0.0, 10.0, x, 471.0];
            item.bbox = [x, 471.0, x + 4.6, 479.0];
            glyphs.push(item);
        }
        let right = 71.0 + 42.0 * 5.2 + 17.0;
        for index in 0..11 {
            let x = right + index as f32 * 5.0;
            let mut item = glyph(100 + index as u32, x, 471.0, "b", false);
            item.font_size = 10.0;
            item.matrix = [10.0, 0.0, 0.0, 10.0, x, 471.0];
            item.bbox = [x, 471.0, x + 4.5, 479.0];
            glyphs.push(item);
        }
        let segs = segment_glyphs(&glyphs);
        assert_eq!(
            segs.len(),
            2,
            "{:?}",
            segs.iter().map(|segment| &segment.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_short_right_column_tail_splits_on_a_16pt_gutter() {
        // `collected a` | `with 44,617 tables.` The gutter is 16pt and the
        // right piece is shorter than a column, but the line spans both.
        let mut glyphs = Vec::new();
        for index in 0..42 {
            let x = 71.0 + index as f32 * 5.2;
            let mut item = glyph(index as u32, x, 69.0, "a", false);
            item.font_size = 10.0;
            item.matrix = [10.0, 0.0, 0.0, 10.0, x, 69.0];
            item.bbox = [x, 69.0, x + 4.6, 77.0];
            glyphs.push(item);
        }
        let right = 71.0 + 42.0 * 5.2 + 16.0;
        for index in 0..14 {
            let x = right + index as f32 * 5.2;
            let mut item = glyph(100 + index as u32, x, 69.0, "b", false);
            item.font_size = 10.0;
            item.matrix = [10.0, 0.0, 0.0, 10.0, x, 69.0];
            item.bbox = [x, 69.0, x + 4.6, 77.0];
            glyphs.push(item);
        }
        let segs = segment_glyphs(&glyphs);
        assert_eq!(
            segs.len(),
            2,
            "{:?}",
            segs.iter().map(|segment| &segment.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_table_header_gap_does_not_merge_the_two_cells() {
        // PubLayNet Table I: `Document layout` and `XML category` share a
        // baseline with a 12pt gap at 8pt. The wrapped word `category` sits
        // under the first cell only.
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        let paint = |glyphs: &mut Vec<Glyph>, id: &mut u32, text: &str, x: f32, y: f32| {
            let mut cursor = x;
            for ch in text.chars() {
                let mut item = glyph(*id, cursor, y, &ch.to_string(), false);
                *id += 1;
                item.font_size = 8.0;
                item.matrix = [8.0, 0.0, 0.0, 8.0, cursor, y];
                let width = 4.0;
                item.bbox = [cursor, y, cursor + width, y + 6.4];
                item.advance = [width, 0.0];
                glyphs.push(item);
                cursor += if ch == ' ' { 3.6 } else { width };
            }
        };
        paint(&mut glyphs, &mut id, "Document", 325.0, 447.7);
        paint(&mut glyphs, &mut id, "layout", 360.6, 447.7);
        // 12pt between the two header cells, matching the real table.
        paint(&mut glyphs, &mut id, "XML", 396.6, 447.7);
        paint(&mut glyphs, &mut id, "category", 412.0, 447.7);
        paint(&mut glyphs, &mut id, "category", 325.0, 438.7);
        let segs = segment_glyphs(&glyphs);
        let texts: Vec<_> = segs.iter().map(|segment| segment.text.as_str()).collect();
        assert!(
            !texts
                .iter()
                .any(|text| text.contains("XML") && text.contains("Document")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_table_cell_gap_beside_another_column_still_splits() {
        // The other column's gutter is the largest gap, so the 1.25em trigger
        // sits just above the 12pt header cells. Those cells stay apart.
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        for index in 0..45 {
            let x = 40.0 + index as f32 * 5.0;
            let mut item = glyph(id, x, 447.7, "a", false);
            id += 1;
            item.font_size = 10.0;
            item.matrix = [10.0, 0.0, 0.0, 10.0, x, 447.7];
            item.bbox = [x, 447.7, x + 4.5, 455.7];
            glyphs.push(item);
        }
        let mut cursor = 40.0 + 45.0 * 5.0 + 26.0;
        for ch in "Document".chars() {
            let mut item = glyph(id, cursor, 447.7, &ch.to_string(), false);
            id += 1;
            item.font_size = 10.0;
            item.matrix = [10.0, 0.0, 0.0, 10.0, cursor, 447.7];
            item.bbox = [cursor, 447.7, cursor + 5.0, 455.7];
            glyphs.push(item);
            cursor += 5.0;
        }
        cursor += 3.0;
        for ch in "layout".chars() {
            let mut item = glyph(id, cursor, 447.7, &ch.to_string(), false);
            id += 1;
            item.font_size = 10.0;
            item.matrix = [10.0, 0.0, 0.0, 10.0, cursor, 447.7];
            item.bbox = [cursor, 447.7, cursor + 5.0, 455.7];
            glyphs.push(item);
            cursor += 5.0;
        }
        cursor += 12.0;
        for ch in "XMLcategory".chars() {
            let mut item = glyph(id, cursor, 447.7, &ch.to_string(), false);
            id += 1;
            item.font_size = 10.0;
            item.matrix = [10.0, 0.0, 0.0, 10.0, cursor, 447.7];
            item.bbox = [cursor, 447.7, cursor + 5.0, 455.7];
            glyphs.push(item);
            cursor += 5.0;
        }
        let segs = segment_glyphs(&glyphs);
        let texts: Vec<_> = segs.iter().map(|segment| segment.text.as_str()).collect();
        assert!(
            !texts
                .iter()
                .any(|text| text.contains("layout") && text.contains("XML")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_short_note_in_the_other_column_still_splits() {
        // LayoutLMv3: the body runs up to a 23.6pt gutter, just under the
        // 24pt hard gap, then `where α is 32.` The note is not a word tail.
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        for index in 0..50 {
            let x = 54.0 + index as f32 * 4.6;
            let mut item = glyph(id, x, 473.6, "b", false);
            id += 1;
            item.font_size = 9.0;
            item.matrix = [9.0, 0.0, 0.0, 9.0, x, 473.6];
            item.bbox = [x, 473.6, x + 4.2, 481.0];
            glyphs.push(item);
        }
        let mut cursor = 54.0 + 50.0 * 4.6 + 23.6;
        for ch in "whereais32.".chars() {
            let mut item = glyph(id, cursor, 473.6, &ch.to_string(), false);
            id += 1;
            item.font_size = 9.0;
            item.matrix = [9.0, 0.0, 0.0, 9.0, cursor, 473.6];
            item.bbox = [cursor, 473.6, cursor + 4.2, 481.0];
            glyphs.push(item);
            cursor += 4.5;
        }
        let segs = segment_glyphs(&glyphs);
        let texts: Vec<_> = segs.iter().map(|segment| segment.text.as_str()).collect();
        assert!(
            !texts
                .iter()
                .any(|text| text.contains('b') && text.contains('w')),
            "{texts:?}"
        );
    }

    #[test]
    fn small_labels_on_a_baseline_do_not_split_body_words() {
        // Figure labels at ~2.6pt share the body baseline and outnumber it.
        // The median size used to become 2.6pt, so a 4.5pt word space was a
        // column gutter and each word was translated on its own.
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        let mut cursor = 49.0f32;
        for word in 0..6 {
            if word > 0 {
                cursor += 4.5;
            }
            for _ in 0..6 {
                let mut item = glyph(id, cursor, 319.4, "a", false);
                id += 1;
                item.font_size = 10.0;
                item.matrix = [10.0, 0.0, 0.0, 10.0, cursor, 319.4];
                item.bbox = [cursor, 319.4, cursor + 4.6, 327.4];
                glyphs.push(item);
                cursor += 4.6;
            }
        }
        let mut tiny = 370.0f32;
        for _ in 0..48 {
            let mut item = glyph(id, tiny, 319.4, "b", false);
            id += 1;
            item.font_size = 2.6;
            item.matrix = [2.6, 0.0, 0.0, 2.6, tiny, 319.4];
            item.bbox = [tiny, 319.4, tiny + 1.4, 321.5];
            glyphs.push(item);
            tiny += 1.5;
        }
        let segs = segment_glyphs(&glyphs);
        let texts: Vec<_> = segs.iter().map(|segment| segment.text.as_str()).collect();
        assert_eq!(
            texts.iter().filter(|text| text.contains('a')).count(),
            1,
            "{texts:?}"
        );
        assert!(
            !texts
                .iter()
                .any(|text| text.contains('a') && text.contains('b')),
            "{texts:?}"
        );
    }

    #[test]
    fn a_narrow_contents_crumb_stays_with_the_rest_of_its_line() {
        // The bullet between "83" and the next entry is a drawing, so the
        // text gap is ~14pt. Each entry keeps its own title; the page number
        // stays a contents mark instead of riding into the next title.
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        let push =
            |glyphs: &mut Vec<Glyph>, id: &mut u32, x: f32, y: f32, text: &str, width: f32| {
                glyphs.push(wide(*id, x, y, text, width, 10.0));
                *id += 1;
            };
        push(&mut glyphs, &mut id, 177.0, 500.0, "client", 21.0);
        push(&mut glyphs, &mut id, 203.0, 500.0, "83", 10.0);
        push(&mut glyphs, &mut id, 227.0, 500.0, "Generating", 40.0);
        push(&mut glyphs, &mut id, 270.0, 500.0, "the", 16.0);
        push(&mut glyphs, &mut id, 289.0, 500.0, "web", 18.0);
        push(&mut glyphs, &mut id, 310.0, 500.0, "searches", 30.0);
        push(&mut glyphs, &mut id, 177.0, 488.0, "results", 25.0);
        push(&mut glyphs, &mut id, 207.0, 488.0, "83", 10.0);
        push(&mut glyphs, &mut id, 231.0, 488.0, "Scraping", 34.0);
        push(&mut glyphs, &mut id, 268.0, 488.0, "the", 16.0);
        push(&mut glyphs, &mut id, 287.0, 488.0, "web", 18.0);
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect();
        assert!(!texts.contains(&"client 83 results 83"), "{texts:?}");
        assert!(
            texts
                .iter()
                .any(|text| text.contains("client") && !text.contains("Generating")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Generating") && !text.contains("client")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|text| text.contains("Scraping")),
            "{texts:?}"
        );
        assert!(
            seg.kept.iter().any(|(_, reason)| reason == "toc"),
            "{:?}",
            seg.kept
        );
    }

    #[test]
    fn a_sentence_quantity_stays_in_the_paragraph() {
        // `2 BLEU` matched the contents crumb `83 Generating`, so the abstract
        // line was its own block: a short line ending at 包括, then a centered
        // fragment, then a new indent.
        let lines = [
            "to-German translation task, improving over the existing best results, including",
            "ensembles, by over 2 BLEU. On the WMT 2014 English-to-French translation task,",
            "our model establishes a new single-model state-of-the-art BLEU score of 41.8 after",
            "training for 3.5 days on eight GPUs, a small fraction of the training costs of the",
        ];
        let glyphs: Vec<Glyph> = lines
            .iter()
            .enumerate()
            .map(|(index, text)| {
                wide(
                    index as u32,
                    143.9,
                    283.8 - index as f32 * 10.9,
                    text,
                    324.0,
                    10.0,
                )
            })
            .collect();
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(
            texts[0].contains("including ensembles") && texts[0].contains("41.8"),
            "{texts:?}"
        );
    }

    #[test]
    fn each_footnote_marker_starts_its_own_line() {
        let star = [
            wide(
                0,
                108.0,
                180.0,
                "∗Equal contribution. Listing order is random. Jakob proposed replacing RNNs.",
                380.0,
                9.0,
            ),
            wide(
                1,
                108.0,
                169.0,
                "the effort to evaluate this idea across several weeks of work.",
                396.0,
                9.0,
            ),
        ];
        let joined: Vec<_> = segment_glyphs(&star)
            .into_iter()
            .map(|seg| seg.text)
            .collect();
        assert_eq!(joined.len(), 1, "{joined:?}");
        assert!(joined[0].contains("the effort"), "{joined:?}");

        // A page that is only these two notes looks like OCR. The body line
        // above them is far enough that it does not join either note.
        let notes = [
            wide(
                0,
                108.0,
                220.0,
                "The model was trained on the full parallel corpus for three days.",
                396.0,
                9.0,
            ),
            wide(
                1,
                124.0,
                95.0,
                "†Work performed while at Google Brain.",
                143.0,
                9.0,
            ),
            wide(
                2,
                124.0,
                84.0,
                "‡Work performed while at Google Research.",
                156.0,
                9.0,
            ),
        ];
        let split: Vec<_> = segment_glyphs(&notes)
            .into_iter()
            .map(|seg| seg.text)
            .collect();
        assert_eq!(split.len(), 3, "{split:?}");
        assert!(split.iter().any(|text| text.starts_with('†')), "{split:?}");
        assert!(split.iter().any(|text| text.starts_with('‡')), "{split:?}");
        assert!(
            split
                .iter()
                .all(|text| !text.contains('†') || !text.contains('‡')),
            "the two notes were joined: {split:?}"
        );
    }

    #[test]
    fn a_short_left_column_line_without_a_page_number_still_splits() {
        let mut glyphs = vec![wide(0, 72.0, 400.0, "Note", 28.0, 10.0)];
        for index in 0..8 {
            let x = 117.0 + index as f32 * 10.0;
            glyphs.push(wide(1 + index, x, 400.0, "b", 8.0, 10.0));
        }
        let segs = segment_glyphs(&glyphs);
        let texts: Vec<_> = segs.iter().map(|segment| segment.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("Note"))
                && texts.iter().any(|text| text.contains('b')),
            "{texts:?}"
        );
        assert_eq!(segs.len(), 2, "{texts:?}");
    }

    fn box_region(x0: f32, y0: f32, x1: f32, y1: f32) -> crate::glyph::PaintedRegion {
        crate::glyph::PaintedRegion {
            page_index: 0,
            bbox: [x0, y0, x1, y1],
            kind: "path".into(),
        }
    }

    fn wide(id: u32, x: f32, y: f32, text: &str, width: f32, size: f32) -> Glyph {
        let mut item = glyph(id, x, y, text, false);
        item.font_size = size;
        item.bbox = [x, y, x + width, y + size];
        item
    }

    #[test]
    fn a_superscript_joins_the_name_and_is_not_its_own_segment() {
        let mut name = wide(0, 72.0, 100.0, "Zhang", 38.0, 10.0);
        name.font_name = "NimbusRomNo9L-Medi".into();
        let mut mark = wide(1, 112.0, 103.6, "1", 4.0, 7.0);
        mark.font_name = "CMR7".into();
        let mut star = wide(2, 108.0, 103.6, "∗", 4.0, 7.0);
        star.font_name = "CMSY7".into();
        let segs = segment_glyphs(&[name, star, mark]);
        assert_eq!(
            segs.len(),
            1,
            "{:?}",
            segs.iter().map(|s| &s.text).collect::<Vec<_>>()
        );
        assert!(segs[0].text.contains('1'), "{}", segs[0].text);
        assert!(segs[0].text.contains('∗'), "{}", segs[0].text);
        assert!(segs[0].glyph_ids.contains(&1));
        assert!(segs[0].glyph_ids.contains(&2));
    }

    #[test]
    fn a_wide_superscript_stays_on_its_base_and_the_math_line_is_kept() {
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        let mut put = |text: &str, x: f32, y: f32, size: f32, font: &str, width: f32| -> f32 {
            let mut at = x;
            for ch in text.chars() {
                let mut item = glyph(id, at, y, &ch.to_string(), false);
                id += 1;
                item.font_name = font.into();
                item.font_size = size;
                item.matrix = [size, 0.0, 0.0, size, at, y];
                item.advance = [width, 0.0];
                item.bbox = [at, y - size * 0.2, at + width, y + size * 0.8];
                at += width;
                glyphs.push(item);
            }
            at
        };
        let mut x = put(
            "Where the matrices ",
            108.0,
            200.0,
            10.0,
            "NimbusRomNo9L-Regu",
            4.2,
        );
        let w = x;
        x = put("W", x, 200.0, 10.0, "CMMI10", 8.0);
        x = put("R", x, 200.0, 10.0, "MSBM10", 7.0);
        // Hole under the superscript, then the next base. The hole is not a column.
        x += 28.0;
        x = put("W", x, 200.0, 10.0, "CMMI10", 8.0);
        put("R", x, 200.0, 10.0, "MSBM10", 7.0);
        put("d", w + 6.0, 204.8, 7.0, "CMMI7", 4.2);
        put("model", w + 11.0, 203.6, 5.0, "NimbusRomNo9L-Regu", 3.4);
        put("d", w + 30.0, 204.8, 7.0, "CMMI7", 4.2);
        put("k", w + 35.0, 203.6, 5.0, "CMMI5", 3.2);
        let mut base = put("and ", 108.0, 186.0, 10.0, "NimbusRomNo9L-Regu", 5.0);
        let w2 = base;
        base = put("W", base, 186.0, 10.0, "CMMI10", 8.0);
        put("R.", base, 186.0, 10.0, "MSBM10", 7.0);
        put("O", w2 + 6.0, 190.6, 7.0, "CMMI7", 6.0);
        put("h", w2 + 16.0, 190.6, 7.0, "CMMI7", 4.0);
        put("d", w2 + 21.0, 190.6, 7.0, "CMMI7", 4.0);
        put("model", w2 + 26.0, 189.4, 5.0, "NimbusRomNo9L-Regu", 3.2);
        put(
            "In this work we employ parallel attention layers for the model.",
            108.0,
            164.0,
            10.0,
            "NimbusRomNo9L-Regu",
            4.6,
        );
        let o = glyphs
            .iter()
            .find(|glyph| glyph.unicode == "O")
            .map(|glyph| glyph.id)
            .expect("O");
        let full = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = full.segments.iter().map(|item| item.text.clone()).collect();
        assert!(
            texts.iter().all(|text| !text.contains("Where")),
            "the math-dense line was translated: {texts:?}"
        );
        assert!(
            texts.iter().all(|text| !text.contains('O')),
            "the superscript O was split onto a prose line: {texts:?}"
        );
        assert!(
            texts.iter().any(|text| text.contains("employ")),
            "a prose sentence was kept: {texts:?}"
        );
        let kept: crate::detmap::HashSet<u32> =
            full.kept.iter().map(|(glyph_id, _)| *glyph_id).collect();
        assert!(kept.contains(&o), "O was not kept with its base");
        assert!(
            full.segments
                .iter()
                .all(|item| !item.glyph_ids.contains(&o)),
            "O was redrawn in a segment"
        );
    }

    #[test]
    fn a_margin_stamp_does_not_split_a_hyphenated_paragraph() {
        let mut upper = wide(0, 144.0, 560.0, "through long tra-", 320.0, 10.0);
        upper.font_name = "NimbusRomNo9L-Regu".into();
        let mut stamp = wide(1, 38.0, 550.0, "6", 12.0, 20.0);
        stamp.font_name = "NimbusRoman-Regular".into();
        let mut lower = wide(2, 144.0, 549.0, "jectories of code", 320.0, 10.0);
        lower.font_name = "NimbusRomNo9L-Regu".into();
        let seg = segment_with(&[upper, stamp, lower], &SegmentFlags::default());
        assert!(
            seg.segments
                .iter()
                .any(|item| item.text.contains("trajectories")),
            "{:?}",
            seg.segments
                .iter()
                .map(|item| &item.text)
                .collect::<Vec<_>>()
        );
        assert!(seg
            .kept
            .iter()
            .any(|(id, reason)| *id == 1 && reason == "margin"));
        assert!(!seg.segments.iter().any(|item| item.glyph_ids.contains(&1)));
    }

    #[test]
    fn a_margin_stamp_does_not_interleave_two_body_lines() {
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        let mut paint = |text: &str, x: f32, y: f32, size: f32| {
            let mut cursor = x;
            for ch in text.chars() {
                let mut item = glyph(id, cursor, y, &ch.to_string(), false);
                id += 1;
                item.font_size = size;
                item.matrix = [size, 0.0, 0.0, size, cursor, y];
                let width = if ch == ' ' { size * 0.33 } else { size * 0.48 };
                item.bbox = [cursor, y - 1.5, cursor + width, y + size * 0.75];
                cursor += width;
                glyphs.push(item);
            }
        };
        paint("with human judgment compared", 108.0, 406.9, 9.0);
        // Offset by half a letter so a merged x-sort cannot keep either phrase.
        paint("ing CDM and text similarity", 110.2, 395.9, 9.0);
        paint("aligns parsed formulas well", 108.0, 385.0, 9.0);
        for (index, y) in [402.1, 399.0, 393.0, 390.5, 387.0, 381.0, 378.0, 375.0]
            .into_iter()
            .enumerate()
        {
            let ch = ['c', '[', ' ', ' ', '3', 'v', '1', ']'][index];
            let mut stamp = glyph(id, 37.9, y, &ch.to_string(), false);
            id += 1;
            stamp.font_size = 20.0;
            stamp.matrix = [20.0, 0.0, 0.0, 20.0, 37.9, y];
            stamp.bbox = [37.9, y - 2.0, 50.0, y + 16.0];
            glyphs.push(stamp);
        }
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|segment| segment.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("with human judgment")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("aligns parsed formulas")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .all(|text| !text.contains("iwnigth") && !text.contains("pajurdsge")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_bold_run_in_stays_with_the_following_regular_line() {
        let mut lead = wide(
            0,
            72.0,
            500.0,
            "Proactive compaction outperforms full-history execution. Under the setting, all evaluated",
            360.0,
            10.0,
        );
        lead.font_name = "NimbusRomNo9L-Medi".into();
        let mut rest = wide(
            1,
            72.0,
            489.0,
            "proactive methods continue the same paragraph.",
            360.0,
            10.0,
        );
        rest.font_name = "NimbusRomNo9L-Regu".into();
        let mut other = wide(
            2,
            72.0,
            478.0,
            "A mono label is not the same family.",
            360.0,
            10.0,
        );
        other.font_name = "IBMPlexMono-SemiBold".into();
        let segs = segment_with(&[lead, rest, other], &SegmentFlags::default());
        let texts: Vec<_> = segs.segments.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("all evaluated proactive methods")),
            "{texts:?}"
        );
        // A line set entirely in a monospace face is a listing. It stays
        // original, and it does not join the roman paragraph above it.
        assert!(
            texts.iter().all(|text| !text.contains("mono label")),
            "{texts:?}"
        );
        assert!(
            segs.kept
                .iter()
                .any(|(id, reason)| *id == 2 && reason == "code"),
            "{:?}",
            segs.kept
        );
    }

    #[test]
    fn a_paragraph_is_one_segment_and_a_short_heading_stays_apart() {
        let body =
            "Coding agents solve repository level tasks through long trajectories of inspection.";
        let lines = vec![
            wide(0, 72.0, 500.0, body, 320.0, 10.0),
            wide(
                1,
                72.0,
                489.0,
                "The next line continues the same paragraph without a break.",
                320.0,
                10.0,
            ),
            wide(2, 72.0, 470.0, "1 Introduction", 90.0, 12.0),
        ];
        let segs = segment_glyphs(&lines);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("next line continues")),
            "{texts:?}"
        );
        assert!(texts.contains(&"1 Introduction"), "{texts:?}");
        assert_eq!(
            segs.iter()
                .filter(|seg| seg.text.contains("Coding"))
                .count(),
            1
        );
    }

    #[test]
    fn figure_labels_are_kept_and_the_caption_is_translated() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today.";
        let glyphs = vec![
            wide(0, 72.0, 700.0, body, 360.0, 10.0),
            wide(1, 90.0, 640.0, "USER PROMPT", 55.0, 9.0),
            wide(2, 230.0, 640.0, "REASONING", 50.0, 9.0),
            wide(
                3,
                72.0,
                560.0,
                "Figure 1: A diagram of the system and its parts.",
                360.0,
                10.0,
            ),
            wide(4, 72.0, 480.0, body, 360.0, 10.0),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.starts_with("Figure 1:")),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("USER PROMPT")),
            "{texts:?}"
        );
        let kept: Vec<_> = seg.kept.iter().map(|(id, _)| *id).collect();
        assert!(kept.contains(&1) && kept.contains(&2), "{:?}", seg.kept);
        assert!(seg.kept.iter().all(|(_, reason)| reason == "figure"));
    }

    #[test]
    fn a_medium_running_header_above_a_figure_is_still_translated() {
        let mut header = wide(
            0,
            180.0,
            738.0,
            "Clustering Fully connected Graphs by Multicut",
            180.0,
            9.0,
        );
        header.font_name = "NimbusRomNo9L-Medi".into();
        let mut body = wide(
            4,
            72.0,
            500.0,
            "This sentence is long enough to count as ordinary body text in the column today.",
            360.0,
            10.0,
        );
        body.font_name = "NimbusRomNo9L-Regu".into();
        let glyphs = vec![
            header,
            wide(1, 90.0, 640.0, "USER PROMPT", 55.0, 9.0),
            wide(2, 230.0, 640.0, "REASONING", 50.0, 9.0),
            wide(
                3,
                72.0,
                560.0,
                "Figure 1: A diagram of the system and its parts.",
                360.0,
                10.0,
            ),
            body,
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Clustering Fully connected")),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("USER PROMPT")),
            "{texts:?}"
        );
        assert!(
            seg.kept.iter().all(|(id, _)| *id != 0),
            "header was claimed as a figure: {:?}",
            seg.kept
        );
    }

    #[test]
    fn a_preprint_header_above_a_figure_is_still_translated() {
        let glyphs = vec![
            wide(0, 108.0, 756.0, "Preprint", 40.0, 9.0),
            wide(1, 90.0, 640.0, "USER PROMPT", 55.0, 9.0),
            wide(2, 230.0, 640.0, "REASONING", 50.0, 9.0),
            wide(
                3,
                72.0,
                520.0,
                "Figure 1: A diagram of the system and its parts.",
                360.0,
                10.0,
            ),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(texts.contains(&"Preprint"), "{texts:?}");
        assert!(
            !texts.iter().any(|text| text.contains("USER PROMPT")),
            "{texts:?}"
        );
        let kept: Vec<_> = seg.kept.iter().map(|(id, _)| *id).collect();
        assert!(kept.contains(&1) && kept.contains(&2), "{:?}", seg.kept);
        assert!(
            !kept.contains(&0),
            "header must not be figure interior: {:?}",
            seg.kept
        );
    }

    #[test]
    fn a_table_note_under_the_grid_stays_original() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today.";
        let mut header = wide(1, 72.0, 600.0, "Method", 50.0, 10.0);
        header.font_name = "NimbusRomNo9L-Regu".into();
        let mut score = wide(2, 220.0, 600.0, "Score", 40.0, 10.0);
        score.font_name = "NimbusRomNo9L-Regu".into();
        let glyphs = vec![
            wide(0, 72.0, 700.0, body, 360.0, 10.0),
            wide(
                3,
                72.0,
                640.0,
                "Table 1: Pass rates for each method on the benchmark.",
                360.0,
                10.0,
            ),
            header,
            score,
            wide(5, 72.0, 580.0, "30.4", 28.0, 10.0),
            wide(6, 72.0, 573.0, "a", 3.0, 6.0),
            wide(
                7,
                76.0,
                570.0,
                "Visible if the metadata is extracted as plain text; unreachable if the pipeline passes only image data.",
                356.0,
                8.0,
            ),
            wide(4, 72.0, 500.0, body, 360.0, 10.0),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().all(|text| !text.contains("Visible")),
            "the table note left the grid: {texts:?}"
        );
        assert_eq!(
            texts
                .iter()
                .filter(|text| text.contains("ordinary body"))
                .count(),
            2,
            "{texts:?}"
        );
    }

    #[test]
    fn affiliations_led_by_raised_markers_stay_apart() {
        let glyphs = vec![
            wide(0, 95.5, 676.7, "a", 3.0, 6.0),
            wide(1, 99.0, 673.8, "Chair for Cybersecurity, Institute of Computer Science, University of Augsburg, Universitätsstr. 2, 86159 Augsburg, Germany", 397.0, 8.0),
            wide(2, 107.3, 667.2, "b", 3.0, 6.0),
            wide(3, 110.8, 664.3, "KASTEL Security Research Labs, Karlsruhe Institute of Technology, Am Fasanengarten 5, 76131 Karlsruhe, Germany", 374.0, 8.0),
            wide(4, 94.7, 657.7, "c", 3.0, 6.0),
            wide(5, 97.8, 654.8, "Fraunhofer Institute for Communication, Information Processing and Ergonomics FKIE, Zanderstr. 5, 53177, Bonn, Germany", 399.0, 8.0),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .all(|text| !(text.contains("Chair") && text.contains("KASTEL"))),
            "two affiliations joined: {texts:?}"
        );
    }

    #[test]
    fn an_algorithm_block_stays_original_and_its_caption_is_translated() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today.";
        let glyphs = vec![
            wide(0, 72.0, 740.0, body, 360.0, 10.0),
            wide(
                1,
                72.0,
                700.0,
                "Algorithm 1 The proposal side of the search.",
                200.0,
                10.0,
            ),
            wide(
                2,
                72.0,
                688.0,
                "Require: the history of earlier rounds and a budget",
                220.0,
                10.0,
            ),
            wide(
                3,
                72.0,
                676.0,
                "1: for each round of the search do",
                160.0,
                10.0,
            ),
            wide(
                4,
                84.0,
                664.0,
                "2: draft a pool of atomic edits to the harness",
                200.0,
                10.0,
            ),
            wide(5, 72.0, 652.0, "3: end for", 50.0, 10.0),
            wide(6, 72.0, 600.0, body, 360.0, 10.0),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.contains(&"Algorithm 1 The proposal side of the search."),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .all(|text| !text.contains("Require") && !text.contains("draft")),
            "an algorithm step was translated: {texts:?}"
        );
        assert_eq!(
            texts
                .iter()
                .filter(|text| text.contains("ordinary body"))
                .count(),
            2,
            "{texts:?}"
        );
    }

    #[test]
    fn an_algorithm2e_block_with_gutter_numbers_stays_original() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today.";
        let glyphs = vec![
            wide(0, 72.0, 740.0, body, 360.0, 10.0),
            wide(
                1,
                80.0,
                700.0,
                "Data: a weighted graph with edge costs",
                180.0,
                10.0,
            ),
            wide(
                2,
                80.0,
                688.0,
                "Result: the clusters of the graph",
                150.0,
                10.0,
            ),
            wide(3, 72.0, 676.0, "1", 4.0, 10.0),
            wide(
                4,
                96.0,
                676.0,
                "while the best edge is attractive do",
                170.0,
                10.0,
            ),
            wide(5, 72.0, 664.0, "2", 4.0, 10.0),
            wide(
                6,
                96.0,
                664.0,
                "contract the edge and update the costs",
                175.0,
                10.0,
            ),
            wide(7, 72.0, 600.0, body, 360.0, 10.0),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .all(|text| !text.contains("graph") && !text.contains("contract")),
            "an algorithm step was translated: {texts:?}"
        );
        assert_eq!(
            texts
                .iter()
                .filter(|text| text.contains("ordinary body"))
                .count(),
            2,
            "{texts:?}"
        );
    }

    #[test]
    fn table_cells_are_kept_and_the_caption_is_translated() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today.";
        let mut header = wide(1, 72.0, 600.0, "Method", 50.0, 10.0);
        header.font_name = "NimbusRomNo9L-Regu".into();
        let mut score = wide(2, 220.0, 600.0, "Score", 40.0, 10.0);
        score.font_name = "NimbusRomNo9L-Regu".into();
        let glyphs = vec![
            wide(0, 72.0, 700.0, body, 360.0, 10.0),
            wide(
                3,
                72.0,
                640.0,
                "Table 1: Pass rates for each method on the benchmark.",
                360.0,
                10.0,
            ),
            header,
            score,
            wide(5, 72.0, 580.0, "30.4", 28.0, 10.0),
            wide(4, 72.0, 500.0, body, 360.0, 10.0),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.starts_with("Table 1:")),
            "{texts:?}"
        );
        assert!(
            texts.iter().all(|text| !text.contains("Method")),
            "column header left the grid: {texts:?}"
        );
        assert!(
            texts.iter().all(|text| !text.contains("Score")),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("30.4")),
            "a numeric cell stays in the source language: {texts:?}"
        );
        let kept: Vec<_> = seg.kept.iter().map(|(id, _)| *id).collect();
        assert!(kept.contains(&5), "{:?}", seg.kept);
        assert!(kept.contains(&1) && kept.contains(&2), "{:?}", seg.kept);
        assert!(seg.kept.iter().all(|(_, reason)| reason == "table"));
    }

    #[test]
    fn translate_tables_puts_cell_text_in_a_segment() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today.";
        let mut header = wide(1, 72.0, 600.0, "Method", 50.0, 10.0);
        header.font_name = "NimbusRomNo9L-Regu".into();
        let mut score = wide(2, 220.0, 600.0, "Score", 40.0, 10.0);
        score.font_name = "NimbusRomNo9L-Regu".into();
        let glyphs = vec![
            wide(0, 72.0, 700.0, body, 360.0, 10.0),
            wide(
                3,
                72.0,
                640.0,
                "Table 1: Pass rates for each method on the benchmark.",
                360.0,
                10.0,
            ),
            header,
            score,
            wide(4, 72.0, 500.0, body, 360.0, 10.0),
        ];
        let seg = segment_with(
            &glyphs,
            &SegmentFlags {
                skip_figures: true,
                skip_tables: false,
            },
        );
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("Method")),
            "{texts:?}"
        );
        assert!(texts.iter().any(|text| text.contains("Score")), "{texts:?}");
        assert!(seg.kept.iter().all(|(_, reason)| reason != "table"));
    }

    #[test]
    fn a_short_caption_tail_stays_with_the_caption() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today.";
        let line_a = "Table 3: Variations on the Transformer architecture. Unlisted values are identical to those of the base";
        let line_b = "model. All metrics are on the English-to-German translation development set, newstest2013. Listed";
        let line_c =
            "perplexities are per-wordpiece, according to our byte-pair encoding, and should not be compared to";
        let tail = "per-word perplexities.";
        let glyphs = vec![
            wide(0, 108.0, 720.0, body, 400.0, 9.0),
            wide(1, 108.0, 680.0, line_a, 396.0, 9.0),
            wide(2, 108.0, 668.0, line_b, 396.0, 9.0),
            wide(3, 108.0, 656.0, line_c, 396.0, 9.0),
            wide(4, 108.0, 644.0, tail, 96.0, 9.0),
            wide(5, 125.0, 600.0, "Layer Type", 70.0, 9.0),
            wide(6, 220.0, 600.0, "Complexity", 70.0, 9.0),
            wide(7, 108.0, 500.0, body, 400.0, 9.0),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let caption = seg
            .segments
            .iter()
            .find(|item| item.text.contains("Table 3:"))
            .expect("caption");
        assert!(
            caption.text.contains("per-word perplexities"),
            "{}",
            caption.text
        );
        assert!(
            !seg.kept
                .iter()
                .any(|(id, reason)| *id == 4 && reason == "table"),
            "caption tail was a table cell: {:?}",
            seg.kept
        );
        assert!(
            seg.kept
                .iter()
                .any(|(id, reason)| (*id == 5 || *id == 6) && reason == "table"),
            "column header left the grid: {:?}",
            seg.kept
        );
        assert!(
            seg.segments
                .iter()
                .all(|item| !item.text.contains("Layer Type")),
            "header missing: {:?}",
            seg.segments
                .iter()
                .map(|item| &item.text)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_table_cell_stays_original_and_the_caption_is_one_segment() {
        let mut id = 0u32;
        let mut glyphs = Vec::new();
        let mut put = |text: &str, x: f32, y: f32| {
            let mut at = x;
            for ch in text.chars() {
                let mut item = glyph(id, at, y, &ch.to_string(), false);
                id += 1;
                item.font_size = 10.0;
                item.matrix = [10.0, 0.0, 0.0, 10.0, at, y];
                item.advance = [5.2, 0.0];
                item.bbox = [at, y, at + 5.0, y + 8.0];
                at += 5.2;
                glyphs.push(item);
            }
        };
        put(
            "Table 3: Variations on the Transformer architecture. Unlisted values stay with the base and should not be compared to",
            108.0,
            700.0,
        );
        put("per-word perplexities.", 108.0, 688.0);
        put("Layer", 108.0, 660.0);
        put("Type", 220.0, 660.0);
        put("Score", 360.0, 660.0);
        put("base", 108.0, 648.0);
        put("512", 220.0, 648.0);
        put("24.9", 360.0, 648.0);
        put("(A)", 108.0, 636.0);
        put("1", 220.0, 636.0);
        put("512", 300.0, 636.0);
        put("positional embedding instead of sinusoids", 108.0, 624.0);
        put("4.92", 340.0, 624.0);
        put(
            "In Table 3 rows we vary the number of attention heads and report the development result in the paragraph under the grid.",
            108.0,
            600.0,
        );
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let captions: Vec<_> = seg
            .segments
            .iter()
            .filter(|item| item.text.contains("Table 3:"))
            .collect();
        assert_eq!(
            captions.len(),
            1,
            "caption split: {:?}",
            captions.iter().map(|item| &item.text).collect::<Vec<_>>()
        );
        assert!(
            captions[0].text.contains("per-word perplexities"),
            "caption tail was its own unit: {}",
            captions[0].text
        );
        assert!(
            !captions[0].text.contains("positional embedding"),
            "a cell joined the caption: {}",
            captions[0].text
        );
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().all(|text| !text.contains("Layer")),
            "column header left the grid: {texts:?}"
        );
        for cell in [
            "512",
            "(A)",
            "positional embedding instead of sinusoids",
            "4.92",
        ] {
            assert!(
                !texts.iter().any(|text| text.contains(cell)),
                "{cell} was translated: {texts:?}"
            );
        }
        assert!(
            texts.iter().any(|text| text.contains("development result")),
            "the paragraph under the table was kept: {texts:?}"
        );
    }

    #[test]
    fn arxiv_attention_caption_tails_join_the_caption() {
        let path = std::path::PathBuf::from("/tmp/fix/arxiv-attention.pdf");
        if !path.is_file() {
            return;
        }
        let extracted = crate::extract::PdfDocument::open(&path)
            .unwrap()
            .extract_with(&crate::extract::ExtractOptions::default());
        let seg = segment_placed(
            &extracted.glyphs,
            &SegmentFlags::default(),
            &extracted.regions,
            &extracted.pages,
        );
        let table3 = seg
            .segments
            .iter()
            .find(|item| item.text.contains("should not be compared to"))
            .map(|item| item.text.clone())
            .unwrap_or_default();
        let table4 = seg
            .segments
            .iter()
            .find(|item| item.text.starts_with("Table 4:"))
            .map(|item| item.text.clone())
            .unwrap_or_default();
        assert!(
            table3.contains("per-word perplexities"),
            "table 3 tail missing: {table3}"
        );
        assert!(table4.contains("of WSJ"), "table 4 tail missing: {table4}");
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.clone()).collect();
        assert!(
            texts.iter().any(|text| text.contains("Layer Type")),
            "column header stayed in the grid"
        );
        for cell in [
            "positional embedding instead of sinusoids",
            "Self-Attention (restricted)",
            "Vinyals & Kaiser el al.",
            "300K",
            "(E) big",
        ] {
            assert!(
                !texts.iter().any(|text| text.contains(cell)),
                "{cell} left the table: {}",
                texts
                    .iter()
                    .find(|text| text.contains(cell))
                    .map(|text| text.chars().take(120).collect::<String>())
                    .unwrap_or_default()
            );
        }
        assert!(
            texts.iter().any(|text| text.contains("In Table 3 rows")),
            "the paragraph under table 3 was kept"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Our results in Table 4")),
            "the paragraph under table 4 was kept"
        );
        assert_eq!(
            texts
                .iter()
                .filter(|text| caption_kind(text) == Some("table"))
                .count(),
            4,
            "a table caption was split or dropped: {:?}",
            texts
                .iter()
                .filter(|text| caption_kind(text) == Some("table"))
                .map(|text| text.chars().take(60).collect::<String>())
                .collect::<Vec<_>>()
        );
        let figures: Vec<_> = texts
            .iter()
            .filter(|text| caption_kind(text) == Some("figure"))
            .map(|text| text.chars().take(72).collect::<String>())
            .collect();
        assert_eq!(figures.len(), 5, "a figure caption was split: {figures:?}");
    }

    #[test]
    fn saved_papers_do_not_translate_a_table_cell() {
        let mut paths = Vec::new();
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/ci");
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "pdf") {
                    paths.push(path);
                }
            }
        }
        let arxiv = std::path::PathBuf::from("/tmp/fix/arxiv-attention.pdf");
        if arxiv.is_file() {
            paths.push(arxiv);
        }
        paths.sort();
        let mut leaked = Vec::new();
        for path in &paths {
            let extracted = crate::extract::PdfDocument::open(path)
                .unwrap()
                .extract_with(&crate::extract::ExtractOptions::default());
            let seg = segment_placed(
                &extracted.glyphs,
                &SegmentFlags::default(),
                &extracted.regions,
                &extracted.pages,
            );
            let by_id: crate::detmap::HashMap<u32, &crate::glyph::Glyph> = extracted
                .glyphs
                .iter()
                .map(|glyph| (glyph.id, glyph))
                .collect();
            let captions: Vec<_> = seg
                .segments
                .iter()
                .filter(|item| caption_kind(&item.text) == Some("table"))
                .collect();
            if captions.is_empty() {
                continue;
            }
            for item in &seg.segments {
                if caption_kind(&item.text).is_some() {
                    continue;
                }
                let glyphs: Vec<_> = item
                    .glyph_ids
                    .iter()
                    .filter_map(|id| by_id.get(id).copied())
                    .collect();
                if glyphs.len() < 2 {
                    continue;
                }
                let mut ordered = glyphs.clone();
                ordered.sort_by(|left, right| glyph_left(left).total_cmp(&glyph_left(right)));
                let size = ordered[0].font_size.max(1.0);
                let mut gap = 0.0f32;
                let mut prev_right: Option<f32> = None;
                for glyph in &ordered {
                    if glyph.unicode.trim().is_empty() {
                        continue;
                    }
                    let left = glyph_left(glyph);
                    if let Some(prev) = prev_right {
                        gap = gap.max(left - prev);
                    }
                    let right = glyph_right(glyph);
                    prev_right = Some(prev_right.map(|prev| prev.max(right)).unwrap_or(right));
                }
                let letters = item
                    .text
                    .chars()
                    .filter(|ch| ch.is_ascii_alphabetic())
                    .count();
                let digits = item.text.chars().filter(|ch| ch.is_ascii_digit()).count();
                let row = gap >= size * 1.55 || (letters <= 4 && item.text.chars().count() <= 6);
                // A two-column sentence has one gutter. A cell is short or numeric.
                if !row || (letters >= 24 && digits * 2 < letters) {
                    continue;
                }
                let trimmed = item.text.trim();
                if trimmed.starts_with("Example ") {
                    continue;
                }
                let y = glyphs
                    .iter()
                    .map(|glyph| glyph.bbox[1])
                    .fold(f32::MIN, f32::max);
                let left = glyphs
                    .iter()
                    .map(|glyph| glyph.bbox[0])
                    .fold(f32::MAX, f32::min);
                let right = glyphs
                    .iter()
                    .map(|glyph| glyph.bbox[2])
                    .fold(0.0f32, f32::max);
                let page = glyphs[0].page_index;
                let page_h = extracted
                    .pages
                    .iter()
                    .find(|info| info.index == page)
                    .map(|info| info.media_box[3] - info.media_box[1])
                    .unwrap_or(792.0);
                if (y < 56.0 || y > page_h - 56.0) && is_page_folio_text(trimmed) {
                    continue;
                }
                let under = captions.iter().any(|caption| {
                    let marks: Vec<_> = caption
                        .glyph_ids
                        .iter()
                        .filter_map(|id| by_id.get(id).copied())
                        .collect();
                    if marks.is_empty() || marks[0].page_index != page {
                        return false;
                    }
                    let cap_y = marks
                        .iter()
                        .map(|glyph| glyph.bbox[1])
                        .fold(f32::MIN, f32::max);
                    // The caption may sit above the grid or under it.
                    let dy = (cap_y - y).abs();
                    if dy < 4.0 || dy > 360.0 {
                        return false;
                    }
                    let cap_left = marks
                        .iter()
                        .map(|glyph| glyph.bbox[0])
                        .fold(f32::MAX, f32::min);
                    let cap_right = marks
                        .iter()
                        .map(|glyph| glyph.bbox[2])
                        .fold(0.0f32, f32::max);
                    (right.min(cap_right) - left.max(cap_left)).max(0.0) > 8.0
                });
                if !under {
                    continue;
                }
                // A column header is translated with the caption. A numeric
                // cell, a complexity (`O(n)`), a prose cell, or a one-word
                // body label (`yes`, `Ours`) is not.
                let words = trimmed.split_whitespace().count();
                let letters = trimmed
                    .chars()
                    .filter(|ch| ch.is_ascii_alphabetic())
                    .count();
                let digits = trimmed.chars().filter(|ch| ch.is_ascii_digit()).count();
                let numeric_cell = digits >= 2 && digits * 2 >= letters && !trimmed.contains('×');
                let prose_cell = words >= 5 || letters >= 24;
                let formula_cell = trimmed.trim_start().starts_with("O(");
                let token = trimmed
                    .trim_matches(|ch: char| !ch.is_ascii_alphanumeric())
                    .to_ascii_lowercase();
                let body_label = matches!(token.as_str(), "yes" | "no" | "ours" | "step" | "base");
                if numeric_cell || prose_cell || formula_cell || body_label {
                    leaked.push(format!(
                        "{} p{} {}",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        page + 1,
                        item.text.chars().take(90).collect::<String>()
                    ));
                }
            }
        }
        assert!(
            leaked.is_empty(),
            "table cells would be translated:\n{}",
            leaked.join("\n")
        );
    }

    #[test]
    fn abbas_table_cells_stay_original() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/ci/pmlr-v202-abbas23a.pdf");
        let extracted = crate::extract::PdfDocument::open(&path)
            .unwrap()
            .extract_with(&crate::extract::ExtractOptions::default());
        let seg = segment_placed(
            &extracted.glyphs,
            &SegmentFlags::default(),
            &extracted.regions,
            &extracted.pages,
        );
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        for cell in ["2440", "2488", "Person", "40.0", "Bus", "0.2"] {
            assert!(
                !texts.iter().any(|text| text.contains(cell)),
                "{cell} left the table: {}",
                texts
                    .iter()
                    .find(|text| text.contains(cell))
                    .map(|text| text.chars().take(80).collect::<String>())
                    .unwrap_or_default()
            );
        }
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Algorithms comparison")),
            "the paragraph under the tables was kept"
        );
        assert!(
            texts.iter().any(|text| text.contains("In Table 3")),
            "the paragraph under table 3 was kept"
        );
    }

    #[test]
    fn a_below_caption_keeps_row_labels_and_the_grid() {
        let mut id = 0u32;
        let mut glyphs = Vec::new();
        let mut put = |text: &str, x: f32, y: f32| {
            let mut at = x;
            for ch in text.chars() {
                let mut item = glyph(id, at, y, &ch.to_string(), false);
                id += 1;
                item.font_size = 10.0;
                item.matrix = [10.0, 0.0, 0.0, 10.0, at, y];
                item.advance = [5.2, 0.0];
                item.bbox = [at, y, at + 5.0, y + 8.0];
                at += 5.2;
                glyphs.push(item);
            }
        };
        put("PPL", 200.0, 640.0);
        put("6.45", 320.0, 640.0);
        put("3 Bits", 108.0, 620.0);
        put("NF-Sym", 108.0, 600.0);
        put("38.28", 320.0, 600.0);
        put("2 Bits", 108.0, 560.0);
        put("2.4e5", 320.0, 560.0);
        put(
            "Table 3: Ablation study of asymmetric quantization and clipping on the development set.",
            108.0,
            500.0,
        );
        put(
            "In the paragraph under the grid we discuss the ablation and the development result stays translated.",
            108.0,
            460.0,
        );
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        let captions: Vec<_> = texts
            .iter()
            .filter(|text| text.contains("Table 3:"))
            .copied()
            .collect();
        assert_eq!(captions.len(), 1, "caption split: {captions:?}");
        for cell in [
            "PPL", "6.45", "3 Bits", "NF-Sym", "38.28", "2 Bits", "2.4e5",
        ] {
            assert!(
                !texts.iter().any(|text| text.contains(cell)),
                "{cell} was translated: {texts:?}"
            );
        }
        assert!(
            texts.iter().any(|text| text.contains("development result")),
            "the paragraph under the caption was kept: {texts:?}"
        );
    }

    #[test]
    fn saved_papers_keep_a_caption_tail_with_the_caption() {
        let mut paths = Vec::new();
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/ci");
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "pdf") {
                    paths.push(path);
                }
            }
        }
        let arxiv = std::path::PathBuf::from("/tmp/fix/arxiv-attention.pdf");
        if arxiv.is_file() {
            paths.push(arxiv);
        }
        paths.sort();
        let mut missed = Vec::new();
        for path in &paths {
            let extracted = crate::extract::PdfDocument::open(path)
                .unwrap()
                .extract_with(&crate::extract::ExtractOptions::default());
            let seg = segment_placed(
                &extracted.glyphs,
                &SegmentFlags::default(),
                &extracted.regions,
                &extracted.pages,
            );
            let kept: crate::detmap::HashSet<u32> = seg
                .kept
                .iter()
                .filter(|(_, reason)| reason == "table" || reason == "figure")
                .map(|(id, _)| *id)
                .collect();
            let caption_ids: crate::detmap::HashSet<u32> = seg
                .segments
                .iter()
                .filter(|item| {
                    let text = item.text.trim();
                    text.starts_with("Table")
                        || text.starts_with("Figure")
                        || text.starts_with("Fig.")
                })
                .flat_map(|item| item.glyph_ids.iter().copied())
                .collect();
            let mut by_page: crate::detmap::HashMap<u32, Vec<&crate::glyph::Glyph>> =
                crate::detmap::HashMap::default();
            for glyph in &extracted.glyphs {
                by_page.entry(glyph.page_index).or_default().push(glyph);
            }
            for glyphs in by_page.values_mut() {
                glyphs.sort_by(|a, b| {
                    b.bbox[1]
                        .total_cmp(&a.bbox[1])
                        .then(a.bbox[0].total_cmp(&b.bbox[0]))
                });
                let mut lines: Vec<Vec<&crate::glyph::Glyph>> = Vec::new();
                for glyph in glyphs.iter().copied() {
                    let y = glyph.bbox[1];
                    if let Some(line) = lines.last_mut() {
                        if (line[0].bbox[1] - y).abs() <= 1.5 {
                            line.push(glyph);
                            continue;
                        }
                    }
                    lines.push(vec![glyph]);
                }
                for (index, line) in lines.iter().enumerate() {
                    let text: String = line.iter().map(|glyph| glyph.unicode.as_str()).collect();
                    let trimmed = text.trim();
                    if !trimmed.starts_with(|ch: char| ch.is_ascii_lowercase()) {
                        continue;
                    }
                    if trimmed
                        .chars()
                        .filter(|ch| ch.is_ascii_alphabetic())
                        .count()
                        < 2
                    {
                        continue;
                    }
                    if !line.iter().any(|glyph| kept.contains(&glyph.id)) {
                        continue;
                    }
                    let Some(prev) = lines[..index].iter().rev().find(|prev| {
                        let letters = prev
                            .iter()
                            .filter(|glyph| {
                                glyph.unicode.chars().any(|ch| ch.is_ascii_alphabetic())
                            })
                            .count();
                        letters >= 4 && prev.iter().any(|glyph| caption_ids.contains(&glyph.id))
                    }) else {
                        continue;
                    };
                    if !prev.iter().all(|glyph| caption_ids.contains(&glyph.id)) {
                        continue;
                    }
                    // Only kept glyphs under the caption line count: a table
                    // cell beside a body column is not the caption's tail.
                    let prev_right = prev
                        .iter()
                        .map(|glyph| glyph.bbox[2])
                        .fold(f32::MIN, f32::max);
                    let prev_left = prev
                        .iter()
                        .map(|glyph| glyph.bbox[0])
                        .fold(f32::MAX, f32::min);
                    if !line.iter().any(|glyph| {
                        kept.contains(&glyph.id)
                            && glyph.bbox[0] < prev_right
                            && glyph.bbox[2] > prev_left
                    }) {
                        continue;
                    }
                    let prev_text: String =
                        prev.iter().map(|glyph| glyph.unicode.as_str()).collect();
                    if sentence_end(prev_text.trim_end()) {
                        continue;
                    }
                    let size = line[0].font_size.max(prev[0].font_size).max(1.0);
                    let dy = prev[0].bbox[1] - line[0].bbox[1];
                    if dy < size * 0.6 || dy > size * 1.8 {
                        continue;
                    }
                    let left = line
                        .iter()
                        .map(|glyph| glyph.bbox[0])
                        .fold(f32::MAX, f32::min);
                    let prev_left = prev
                        .iter()
                        .map(|glyph| glyph.bbox[0])
                        .fold(f32::MAX, f32::min);
                    if (left - prev_left).abs() > 36.0 {
                        continue;
                    }
                    missed.push(format!(
                        "{} p{} after {:?}: {:?}",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        line[0].page_index + 1,
                        prev_text.chars().rev().take(48).collect::<String>(),
                        trimmed.chars().take(80).collect::<String>()
                    ));
                }
            }
            if path.ends_with("arxiv-attention.pdf") {
                let table3 = seg
                    .segments
                    .iter()
                    .find(|item| item.text.contains("should not be compared to"))
                    .map(|item| item.text.clone())
                    .unwrap_or_default();
                assert!(
                    table3.contains("per-word perplexities"),
                    "table 3 tail missing: {table3}"
                );
                let table4 = seg
                    .segments
                    .iter()
                    .find(|item| item.text.starts_with("Table 4:"))
                    .map(|item| item.text.clone())
                    .unwrap_or_default();
                assert!(table4.contains("of WSJ"), "table 4 tail missing: {table4}");
            }
        }
        assert!(
            missed.is_empty(),
            "caption tails kept inside a table or figure:\n{}",
            missed.join("\n")
        );
    }

    #[test]
    fn a_callout_below_a_table_is_not_kept_as_a_cell() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today. It continues so the column face stays the body face and the sidebar does not.";
        let mut body_line = wide(0, 72.0, 700.0, body, 360.0, 10.0);
        body_line.font_name = "NewBaskerville-Roman".into();
        let mut caption = wide(
            1,
            102.0,
            360.0,
            "Table 2.2 In-context learning prompts and methods.",
            300.0,
            8.0,
        );
        caption.font_name = "FranklinGothic-Demi".into();
        let mut cell = wide(
            2,
            108.0,
            340.0,
            "Zero-shot learning provides no example in the prompt at all.",
            280.0,
            8.5,
        );
        cell.font_name = "FranklinGothic-Book".into();
        let mut title = wide(3, 114.0, 290.0, "Beyond Chain of Thought", 180.0, 10.5);
        title.font_name = "FranklinGothic-Demi".into();
        let mut prose = wide(
            4,
            114.0,
            276.0,
            "Chain of Thought improves how language models handle complex reasoning by",
            340.0,
            9.5,
        );
        prose.font_name = "FranklinGothic-Book".into();
        let seg = segment_with(
            &[body_line, caption, cell, title, prose],
            &SegmentFlags::default(),
        );
        let kept: Vec<_> = seg.kept.iter().map(|(id, _)| *id).collect();
        assert!(kept.contains(&2), "table cell stays: {:?}", seg.kept);
        assert!(
            !kept.contains(&3) && !kept.contains(&4),
            "callout was swallowed: {:?}",
            seg.kept
        );
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Beyond Chain of Thought")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_table_bullet_beside_a_narrow_caption_stays_in_the_cell() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today. It continues so the column face stays the body face.";
        let mut body_line = wide(0, 93.0, 620.0, body, 370.0, 10.0);
        body_line.font_name = "NewBaskerville-Roman".into();
        let mut caption = wide(
            1,
            93.0,
            500.0,
            "Table 2.1 Prompt types and examples",
            142.0,
            8.0,
        );
        caption.font_name = "FranklinGothic-Demi".into();
        let mut cell = wide(
            2,
            99.0,
            420.0,
            "Composing text Write a piece on the diver watches, mentioning the following facts:",
            320.0,
            8.0,
        );
        cell.font_name = "FranklinGothic-Book".into();
        // The example column starts near x=194. Overlap with the 142pt caption
        // is under 30 percent, so a caption-only hit test dropped the bullet.
        let mut bullet = wide(
            3,
            194.0,
            406.0,
            "■ The Rolex Oyster Perpetual was among the first to be water resistant.",
            250.0,
            8.5,
        );
        bullet.font_name = "FranklinGothic-Book".into();
        let mut wrap = wide(
            4,
            206.0,
            392.0,
            "diver watch was the Seamaster.",
            120.0,
            8.0,
        );
        wrap.font_name = "FranklinGothic-Book".into();
        let mut heading = wide(5, 57.0, 340.0, "2.5 Reasoning in detail", 180.0, 12.5);
        heading.font_name = "FranklinGothic-Demi".into();
        let seg = segment_with(
            &[body_line, caption, cell, bullet, wrap, heading],
            &SegmentFlags::default(),
        );
        let kept: Vec<_> = seg.kept.iter().map(|(id, _)| *id).collect();
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            kept.contains(&2) && kept.contains(&3) && kept.contains(&4),
            "example-column bullets were translated: {:?} texts {texts:?}",
            seg.kept
        );
        assert!(
            !kept.contains(&5),
            "the section heading was swallowed: {:?}",
            seg.kept
        );
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            !texts.iter().any(|text| text.contains("Rolex")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_split_section_index_is_not_a_table_cell() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today. It continues so the column face stays the body face.";
        let mut body_line = wide(0, 93.0, 620.0, body, 370.0, 10.0);
        body_line.font_name = "NewBaskerville-Roman".into();
        let mut caption = wide(
            1,
            93.0,
            500.0,
            "Table 2.1 Prompt types and examples",
            142.0,
            8.0,
        );
        caption.font_name = "FranklinGothic-Demi".into();
        let mut cell = wide(
            2,
            99.0,
            406.0,
            "ReasoningAdd the square numbers and name the result in the prompt.",
            320.0,
            8.5,
        );
        cell.font_name = "FranklinGothic-Book".into();
        // 26pt is past a word space, so the index and the title are two lines.
        // `in` is lowercase, and neither line is `2.5 Reasoning in detail`.
        let mut index = wide(3, 57.0, 374.0, "2.5", 17.0, 12.5);
        index.font_name = "FranklinGothic-DemiItal".into();
        let mut title = wide(4, 100.0, 374.0, "Reasoning in detail", 160.0, 12.5);
        title.font_name = "FranklinGothic-DemiItal".into();
        let seg = segment_with(
            &[body_line, caption, cell, index, title],
            &SegmentFlags::default(),
        );
        let kept: Vec<_> = seg.kept.iter().map(|(id, _)| *id).collect();
        assert!(
            kept.contains(&2),
            "table cell was translated: {:?}",
            seg.kept
        );
        assert!(
            !kept.contains(&3) && !kept.contains(&4),
            "the split heading was swallowed: {:?}",
            seg.kept
        );
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Reasoning in detail")),
            "{texts:?}"
        );
        assert!(texts.contains(&"2.5"), "{texts:?}");
    }

    #[test]
    fn a_contents_row_inside_a_table_keeps_leaders_and_translates_the_title() {
        // Word paints a contents list as table rows, and "Table 2: … …. 24"
        // matches a table caption. The titles are still translated. A real
        // caption with no leaders still keeps its cells. A box around the
        // contents rows does not turn those titles into figure labels.
        let body =
            "This sentence is long enough to count as ordinary body text in the column today.";
        let glyphs = vec![
            block(0, 72.0, 780.0, 360.0, 10.0, body),
            block(1, 230.0, 750.0, 110.0, 12.0, "Table of Contents"),
            block(2, 230.0, 724.0, 80.0, 12.0, "List of Tables"),
            block(3, 72.0, 660.0, 120.0, 10.0, "Executive Summary"),
            block(4, 200.0, 660.0, 280.0, 10.0, "...................."),
            block(5, 490.0, 660.0, 14.0, 10.0, "vi"),
            block(6, 72.0, 640.0, 110.0, 10.0, "1 Introduction"),
            block(7, 190.0, 640.0, 290.0, 10.0, "......................"),
            block(8, 490.0, 640.0, 10.0, 10.0, "1"),
            block(
                9,
                72.0,
                620.0,
                220.0,
                10.0,
                "Table 2: SSDF Practices and Clauses",
            ),
            block(10, 300.0, 620.0, 180.0, 10.0, ".................."),
            block(11, 488.0, 620.0, 16.0, 10.0, "24"),
            block(
                12,
                72.0,
                460.0,
                360.0,
                10.0,
                "Table 1: Pass rates for each method on the benchmark.",
            ),
            block(13, 72.0, 440.0, 50.0, 10.0, "Method"),
            block(14, 220.0, 440.0, 40.0, 10.0, "Score"),
        ];
        let regions = [
            crate::glyph::PaintedRegion {
                page_index: 0,
                bbox: [60.0, 600.0, 520.0, 680.0],
                kind: "path".into(),
            },
            crate::glyph::PaintedRegion {
                page_index: 0,
                bbox: [60.0, 590.0, 78.0, 675.0],
                kind: "path".into(),
            },
        ];
        let seg = segment_placed(&glyphs, &SegmentFlags::default(), &regions, &[]);
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        for title in [
            "Table of Contents",
            "List of Tables",
            "Executive Summary",
            "1 Introduction",
            "Table 2: SSDF Practices",
            "Table 1:",
        ] {
            assert!(
                texts.iter().any(|text| text.contains(title)),
                "missing {title} in {texts:?}"
            );
        }
        assert!(
            texts
                .iter()
                .all(|text| !text.contains("....") && *text != "vi"),
            "{texts:?}"
        );
        assert!(
            texts.iter().all(|text| !text.contains("Method")),
            "column header left the grid: {texts:?}"
        );
        let reason = |id: u32| -> Vec<&str> {
            seg.kept
                .iter()
                .filter(|(glyph, _)| *glyph == id)
                .map(|(_, reason)| reason.as_str())
                .collect()
        };
        for id in [4, 5, 7, 8, 10, 11] {
            let reasons = reason(id);
            assert!(
                reasons.contains(&"toc") && !reasons.iter().any(|item| *item != "toc"),
                "mark {id} should stay a contents mark: {reasons:?}"
            );
        }
        for id in [1, 2, 3, 6, 9] {
            assert!(
                reason(id).is_empty(),
                "title {id} was kept: {:?}",
                reason(id)
            );
        }
        assert!(
            !reason(13).is_empty() && !reason(14).is_empty(),
            "column header left the grid: {:?} {:?}",
            reason(13),
            reason(14)
        );
    }

    #[test]
    fn a_roman_table_caption_keeps_the_cells() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today.";
        let glyphs = vec![
            wide(0, 72.0, 700.0, body, 360.0, 10.0),
            wide(
                1,
                72.0,
                640.0,
                "TABLE II: Statistics of the training and development sets.",
                360.0,
                10.0,
            ),
            wide(2, 72.0, 620.0, "Plain pages", 80.0, 9.0),
            wide(3, 200.0, 620.0, "87608", 40.0, 9.0),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        assert!(seg
            .segments
            .iter()
            .any(|item| item.text.contains("TABLE II")));
        assert!(
            !seg.segments.iter().any(|item| item.text.contains("Plain")),
            "{:?}",
            seg.segments
                .iter()
                .map(|item| &item.text)
                .collect::<Vec<_>>()
        );
        assert!(seg.kept.iter().any(|(_, reason)| reason == "table"));
    }

    #[test]
    fn an_email_under_an_address_stays_its_own_line() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today.";
        let glyphs = vec![
            wide(0, 72.0, 700.0, body, 360.0, 10.0),
            wide(1, 94.0, 624.0, "VIC 3006, Australia", 110.0, 9.0),
            wide(2, 94.0, 612.0, "peter.zhong@au.ibm.com", 120.0, 9.0),
        ];
        let segs = segment_glyphs(&glyphs);
        let texts: Vec<_> = segs.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains('@') && !text.contains("Australia")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_sentence_that_mentions_a_table_is_not_a_caption() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today.";
        let glyphs = vec![
            wide(0, 72.0, 700.0, body, 360.0, 10.0),
            wide(
                1,
                72.0,
                680.0,
                "Table 1 shows that both methods improve the pass rate.",
                360.0,
                10.0,
            ),
            wide(
                2,
                72.0,
                669.0,
                "The next sentence stays in the same paragraph.",
                360.0,
                10.0,
            ),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        assert!(seg.kept.is_empty(), "{:?}", seg.kept);
        assert!(seg
            .segments
            .iter()
            .any(|item| item.text.contains("Table 1 shows")));
    }

    #[test]
    fn a_wrapped_table_caption_is_translated_and_the_cells_stay() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today.";
        let mut glyphs = vec![wide(0, 72.0, 720.0, body, 360.0, 10.0)];
        let mut id = 1u32;
        let mut paint = |glyphs: &mut Vec<Glyph>, text: &str, x: f32, y: f32| {
            let mut cursor = x;
            let size = 9.0f32;
            for ch in text.chars() {
                let mut item = glyph(id, cursor, y, &ch.to_string(), false);
                id += 1;
                item.font_size = size;
                item.matrix = [size, 0.0, 0.0, size, cursor, y];
                let width = if ch == ' ' { 3.0 } else { 4.4 };
                item.bbox = [cursor, y - 1.0, cursor + width * 0.9, y + 7.0];
                cursor += width;
                glyphs.push(item);
            }
            cursor
        };
        let after = paint(&mut glyphs, "Table 1.", 72.0, 640.0);
        paint(
            &mut glyphs,
            "HPO performed in OTSL and HTML representation on the same",
            after + 12.5,
            640.0,
        );
        paint(
            &mut glyphs,
            "transformer based architecture trained only on the public set today.",
            72.0,
            629.0,
        );
        paint(&mut glyphs, "Method", 90.0, 590.0);
        paint(&mut glyphs, "Score", 240.0, 590.0);
        paint(&mut glyphs, "30.4", 240.0, 572.0);
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("transformer based")),
            "{texts:?}"
        );
        assert!(
            texts.iter().all(|text| !text.contains("Method")),
            "column header left the grid: {texts:?}"
        );
        assert!(!texts.iter().any(|text| text.contains("30.4")), "{texts:?}");
        assert!(seg.kept.iter().any(|(_, reason)| reason == "table"));
        assert!(seg.kept.iter().all(|(_, reason)| reason == "table"));
    }

    #[test]
    fn a_dotted_section_stops_the_table_before_the_paragraph() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today.";
        let mut heading = wide(4, 72.0, 540.0, "5.3 Qualitative Results", 150.0, 10.0);
        heading.font_name = "SourceSans-Bold".into();
        let glyphs = vec![
            wide(0, 72.0, 720.0, body, 360.0, 10.0),
            wide(
                1,
                72.0,
                660.0,
                "Table 2. TSR and cell detection results on the public sets.",
                360.0,
                9.0,
            ),
            wide(2, 180.0, 610.0, "0.965", 36.0, 9.0),
            wide(3, 260.0, 610.0, "0.934", 36.0, 9.0),
            heading,
            wide(5, 72.0, 520.0, body, 360.0, 10.0),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("5.3 Qualitative")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("long enough") && !text.contains("Table 2")),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("0.965")),
            "{texts:?}"
        );
        let kept: Vec<_> = seg.kept.iter().map(|(id, _)| *id).collect();
        assert!(kept.contains(&2) && kept.contains(&3), "{:?}", seg.kept);
        assert!(!kept.contains(&4) && !kept.contains(&5), "{:?}", seg.kept);
    }

    #[test]
    fn a_bold_section_under_the_caption_is_not_a_figure_label() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today.";
        let mut body_top = wide(0, 72.0, 720.0, body, 360.0, 10.0);
        body_top.font_name = "CMR10".into();
        let mut heading = wide(4, 72.0, 520.0, "1 Introduction", 110.0, 12.0);
        heading.font_name = "CMBX12".into();
        let mut body_below = wide(5, 72.0, 490.0, body, 360.0, 10.0);
        body_below.font_name = "CMR10".into();
        let glyphs = vec![
            body_top,
            wide(1, 90.0, 640.0, "USER PROMPT", 55.0, 9.0),
            wide(2, 230.0, 640.0, "REASONING", 50.0, 9.0),
            wide(
                3,
                72.0,
                560.0,
                "Figure 1: A diagram of the system and its parts.",
                360.0,
                10.0,
            ),
            heading,
            body_below,
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(texts.contains(&"1 Introduction"), "{texts:?}");
        assert!(
            texts
                .iter()
                .any(|text| text.contains("long enough") && !text.contains("Figure 1")),
            "{texts:?}"
        );
        let kept: Vec<_> = seg.kept.iter().map(|(id, _)| *id).collect();
        assert!(kept.contains(&1) && kept.contains(&2), "{:?}", seg.kept);
        assert!(!kept.contains(&4) && !kept.contains(&5), "{:?}", seg.kept);
    }

    #[test]
    fn translating_figures_is_opt_in() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today.";
        let glyphs = vec![
            wide(0, 72.0, 700.0, body, 360.0, 10.0),
            wide(1, 90.0, 640.0, "USER PROMPT", 55.0, 9.0),
            wide(
                2,
                72.0,
                560.0,
                "Figure 1: A diagram of the system and its parts.",
                360.0,
                10.0,
            ),
        ];
        let seg = segment_with(
            &glyphs,
            &SegmentFlags {
                skip_figures: false,
                skip_tables: true,
            },
        );
        assert!(seg.kept.is_empty(), "{:?}", seg.kept);
        assert!(seg
            .segments
            .iter()
            .any(|item| item.text.contains("USER PROMPT")));
    }

    #[test]
    fn the_sample_paper_keeps_diagram_labels_and_table_cells() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/ci/arxiv-2610.02163.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(&path).unwrap();
        let extraction = doc.extract_with(&crate::extract::ExtractOptions {
            max_pages: Some(6),
            ..crate::extract::ExtractOptions::default()
        });
        let seg = segment_with(&extraction.glyphs, &SegmentFlags::default());
        let kept: crate::detmap::HashSet<u32> = seg.kept.iter().map(|(id, _)| *id).collect();
        let mono: Vec<_> = extraction
            .glyphs
            .iter()
            .filter(|glyph| glyph.page_index == 1 && glyph.font_name.contains("PlexMono"))
            .collect();
        assert!(!mono.is_empty(), "page 2 should contain diagram labels");
        let kept_mono = mono.iter().filter(|glyph| kept.contains(&glyph.id)).count();
        assert!(
            kept_mono * 10 >= mono.len() * 9,
            "kept {} of {} mono labels",
            kept_mono,
            mono.len()
        );
        assert!(seg.segments.iter().any(|item| {
            item.page_index == 1 && item.text.replace(' ', "").starts_with("Figure1:")
        }));
        assert!(seg.segments.iter().any(|item| {
            item.page_index == 5 && item.text.replace(' ', "").starts_with("Table1:")
        }));
        assert!(seg.segments.iter().any(|item| {
            item.page_index == 5 && item.text.replace(' ', "").starts_with("Figure3:")
        }));
        let cell = extraction.glyphs.iter().find(|glyph| {
            glyph.page_index == 5
                && glyph.unicode == "M"
                && glyph.matrix[5] < 660.0
                && glyph.matrix[5] > 630.0
                && glyph.matrix[4] < 140.0
        });
        let cell = cell.expect("table header");
        assert!(kept.contains(&cell.id), "column header left the grid");
        let placed = segment_placed(
            &extraction.glyphs,
            &SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let reasons: Vec<_> = placed
            .kept
            .iter()
            .filter(|(id, _)| *id == cell.id)
            .map(|(_, reason)| reason.clone())
            .collect();
        let in_seg = placed
            .segments
            .iter()
            .any(|item| item.glyph_ids.contains(&cell.id));
        let numeric = extraction.glyphs.iter().find(|glyph| {
            glyph.page_index == 5
                && glyph.unicode.chars().any(|ch| ch.is_ascii_digit())
                && glyph.matrix[5] < 630.0
                && glyph.matrix[5] > 560.0
                && glyph.font_name.contains("Regu")
        });
        let numeric = numeric.expect("table cell");
        assert!(
            kept.contains(&numeric.id),
            "a numeric cell stays original: {} @ {:.1}",
            numeric.unicode,
            numeric.matrix[5]
        );
        let num_reasons: Vec<_> = placed
            .kept
            .iter()
            .filter(|(id, _)| *id == numeric.id)
            .map(|(_, reason)| reason.as_str())
            .collect();
        assert!(
            !in_seg && !reasons.is_empty(),
            "a ruled column header left the grid: reasons={reasons:?} in_segment={in_seg}"
        );
        assert!(
            placed.kept.iter().any(|(id, _)| *id == numeric.id),
            "a numeric cell was released with the header: {num_reasons:?}"
        );
        assert!(seg.kept.iter().any(|(_, reason)| reason == "figure"));
        assert!(seg.kept.iter().any(|(_, reason)| reason == "table"));
        let author = seg
            .segments
            .iter()
            .find(|item| item.page_index == 0 && item.text.contains("Zhang"))
            .expect("author line");
        assert!(
            author.text.contains('1') && author.text.contains('∗'),
            "superscripts should travel with the author line: {}",
            author.text
        );
        let abstract_seg = seg
            .segments
            .iter()
            .find(|item| item.page_index == 0 && item.text.contains("Coding"))
            .expect("abstract");
        assert!(
            abstract_seg.text.contains("trajectories"),
            "abstract lines should join across the margin stamp: {}",
            &abstract_seg.text[..abstract_seg.text.len().min(180)]
        );
        let margin_ids: crate::detmap::HashSet<u32> = seg
            .kept
            .iter()
            .filter(|(_, reason)| reason == "margin")
            .map(|(id, _)| *id)
            .collect();
        assert!(margin_ids.len() >= 8, "margin stamp should be kept");
        assert!(abstract_seg
            .glyph_ids
            .iter()
            .all(|id| !margin_ids.contains(id)));
        assert!(
            seg.segments.iter().any(|item| {
                item.page_index == 5
                    && item.text.contains("outperforms")
                    && !item.text.contains("Under the")
            }),
            "medium run-in heading should split from the body"
        );
        assert!(
            seg.segments.iter().any(|item| {
                item.page_index == 5
                    && item.text.contains("judge-guided")
                    && !item.text.contains("exceeds")
            }),
            "the second medium run-in should split from the body"
        );
    }

    #[test]
    fn corpus_bold_run_ins_split_from_the_following_body() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/ci");
        let Ok(entries) = std::fs::read_dir(&root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("pdf") {
                continue;
            }
            let doc = crate::extract::PdfDocument::open(&path).unwrap();
            let extraction = doc.extract_with(&crate::extract::ExtractOptions {
                max_pages: Some(8),
                ..crate::extract::ExtractOptions::default()
            });
            let seg = segment_glyphs(&extraction.glyphs);
            let by_id: crate::detmap::HashMap<u32, &crate::glyph::Glyph> = extraction
                .glyphs
                .iter()
                .map(|glyph| (glyph.id, glyph))
                .collect();
            for item in &seg {
                if let Some(detail) = mixed_bold_run_in(&item.glyph_ids, &by_id) {
                    panic!(
                        "{} page {} keeps a bold run-in with the body: {detail}",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        item.page_index + 1
                    );
                }
            }
        }
    }

    fn mixed_bold_run_in(
        ids: &[u32],
        by_id: &crate::detmap::HashMap<u32, &crate::glyph::Glyph>,
    ) -> Option<String> {
        let mut glyphs: Vec<&crate::glyph::Glyph> =
            ids.iter().filter_map(|id| by_id.get(id).copied()).collect();
        if glyphs.len() < 6 {
            return None;
        }
        glyphs.sort_by(|left, right| {
            right.matrix[5]
                .total_cmp(&left.matrix[5])
                .then(left.matrix[4].total_cmp(&right.matrix[4]))
        });
        let top = glyphs[0].matrix[5];
        let size = glyphs
            .iter()
            .map(|glyph| glyph.font_size)
            .fold(1.0f32, f32::max)
            .max(1.0);
        let line: Vec<&crate::glyph::Glyph> = glyphs
            .iter()
            .copied()
            .filter(|glyph| (glyph.matrix[5] - top).abs() <= size * 0.45)
            .collect();
        let mut label = String::new();
        let mut saw_emphasis = false;
        let mut body_letters = 0usize;
        let mut tail_upper = false;
        for glyph in &line {
            let letters = glyph
                .unicode
                .chars()
                .filter(|ch| ch.is_ascii_alphabetic())
                .count();
            let math = glyph.font_name.to_ascii_uppercase().contains("CMMI")
                || glyph.font_name.to_ascii_uppercase().contains("CMSY")
                || glyph.font_name.to_ascii_uppercase().contains("MATH");
            if letters == 0 || math {
                // A period later in the sentence is not the heading's stop.
                // A math letter (`k` after `cond.:`) is not the next sentence.
                if body_letters == 0 && !math {
                    label.push_str(&glyph.unicode);
                }
                continue;
            }
            let style = crate::font::face_style(&glyph.font_name);
            let emphasis = style.bold || crate::font::medium_weight(&glyph.font_name);
            if emphasis {
                saw_emphasis = true;
                label.push_str(&glyph.unicode);
            } else if !saw_emphasis {
                return None;
            } else {
                if body_letters == 0 {
                    tail_upper = glyph
                        .unicode
                        .chars()
                        .find(|ch| ch.is_ascii_alphabetic())
                        .is_some_and(|ch| ch.is_ascii_uppercase());
                }
                body_letters += letters;
            }
        }
        let ended = label.trim_end().ends_with('.') || label.trim_end().ends_with(':');
        // `cond.: k passed` continues in lowercase. A run-in heading opens
        // the next sentence with a capital (`execution. Under`).
        if saw_emphasis && ended && body_letters >= 8 && tail_upper {
            let body: String = line.iter().map(|glyph| glyph.unicode.as_str()).collect();
            return Some(body.chars().take(120).collect());
        }
        // The heading is the whole first line. The roman body is the next
        // line of the same segment, so the heading would be set in the body face.
        if saw_emphasis && ended && body_letters == 0 {
            let later = glyphs
                .iter()
                .filter(|glyph| glyph.matrix[5] < top - size * 0.45)
                .filter(|glyph| {
                    let style = crate::font::face_style(&glyph.font_name);
                    glyph.unicode.chars().any(|ch| ch.is_ascii_alphabetic())
                        && !style.bold
                        && !crate::font::medium_weight(&glyph.font_name)
                        && !style.italic
                })
                .count();
            if later >= 8 {
                return Some(label.chars().take(80).collect());
            }
        }
        None
    }

    #[test]
    fn open_logic_bold_labels_split_when_the_book_is_present() {
        let Some(path) = std::env::var_os("RPT_OPEN_LOGIC_PDF") else {
            return;
        };
        let doc = crate::extract::PdfDocument::open(path).unwrap();
        let extraction = doc.extract_with(&crate::extract::ExtractOptions {
            max_pages: Some(40),
            ..crate::extract::ExtractOptions::default()
        });
        let seg = segment_glyphs(&extraction.glyphs);
        let texts: Vec<_> = seg
            .iter()
            .filter(|item| item.page_index == 32)
            .map(|item| item.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| {
                text.contains("Definition 1.24")
                    && text.contains("Cartesian")
                    && !text.contains("Given")
            }),
            "definition label should split: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Proposition 1.27") && !text.contains("elements")),
            "proposition label should split: {texts:?}"
        );
        let by_id: crate::detmap::HashMap<u32, &crate::glyph::Glyph> = extraction
            .glyphs
            .iter()
            .map(|glyph| (glyph.id, glyph))
            .collect();
        for item in seg.iter().filter(|item| item.page_index == 32) {
            if let Some(detail) = mixed_bold_run_in(&item.glyph_ids, &by_id) {
                panic!("open logic page 33 keeps a bold run-in with the body: {detail}");
            }
        }
    }

    #[test]
    fn a_stamp_on_the_body_baseline_is_removed_from_the_paragraph() {
        let mut body = wide(
            0,
            72.0,
            500.0,
            "through long trajectories of code",
            300.0,
            10.0,
        );
        body.font_name = "CMR10".into();
        let mut glyphs = vec![body];
        // The gap to the body is under the size-20 column split, so the letter
        // would otherwise sit inside the line. Eight stacked letters make a stamp.
        for index in 0..8 {
            let mut stamp = wide(
                1 + index,
                37.9,
                500.0 - index as f32 * 14.0,
                "6",
                14.0,
                20.0,
            );
            stamp.font_name = "NimbusRoman-Regular".into();
            glyphs.push(stamp);
        }
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let text = seg
            .segments
            .iter()
            .find(|item| item.text.contains("trajectories"))
            .expect("body");
        assert!(!text.text.contains('6'), "{}", text.text);
        assert!(!text.glyph_ids.contains(&1));
        assert!(seg
            .kept
            .iter()
            .any(|(id, reason)| *id == 1 && reason == "margin"));
    }

    #[test]
    fn a_centered_list_keeps_the_letter_that_starts_each_line() {
        // The longest centered lines share a left edge. That column is the
        // first letter of each topic, not a margin stamp.
        let rows = [
            "Ada, Android, Go,",
            "Energy, Legal and Policy,",
            "Friends, Management, Community,",
            "Kernel, Legal, LibreOffice,",
            "Monitoring, Mozilla, Network,",
            "Open Media, Open Research,",
            "PostgreSQL, Python, Rust,",
            "Security, Swift, WebAssembly,",
        ];
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        for (row, text) in rows.iter().enumerate() {
            let y = 400.0 - row as f32 * 16.0;
            let mut x = 40.0f32;
            for ch in text.chars() {
                let mut glyph = wide(id, x, y, &ch.to_string(), 8.0, 16.0);
                id += 1;
                glyph.font_name = "NimbusRoman-Regular".into();
                glyphs.push(glyph);
                x += if ch == ' ' { 4.0 } else { 8.0 };
            }
        }
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.clone()).collect();
        let joined = texts.join(" ");
        assert!(
            joined.contains("Friends") && joined.contains("Energy") && joined.contains("Ada"),
            "{texts:?}"
        );
        assert!(
            seg.kept.iter().all(|(_, reason)| reason != "margin"),
            "{:?}",
            seg.kept
        );
    }

    #[test]
    fn an_inline_symbol_stays_inside_its_paragraph() {
        let prose = "This sentence is long enough to stand as its own prose line today.";
        let mut above = wide(0, 72.0, 500.0, prose, 320.0, 10.0);
        above.font_name = "CMR10".into();
        let mut words = wide(
            1,
            72.0,
            488.0,
            "where the exponent stays inside the same sentence today.",
            300.0,
            10.0,
        );
        words.font_name = "CMR10".into();
        let mut theta = wide(2, 380.0, 488.0, "θ", 8.0, 10.0);
        theta.font_name = "CMMI10".into();
        let mut below = wide(
            3,
            72.0,
            476.0,
            "The following sentence is also ordinary prose without symbols.",
            320.0,
            10.0,
        );
        below.font_name = "CMR10".into();
        let seg = segment_with(&[above, words, theta, below], &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| {
                text.contains("This sentence") && text.contains('θ') && text.contains("following")
            }),
            "{texts:?}"
        );
    }

    #[test]
    fn a_display_equation_does_not_join_the_prose_around_it() {
        let mut above = wide(
            0,
            72.0,
            500.0,
            "The identity below is used in the proof of the claim.",
            320.0,
            10.0,
        );
        above.font_name = "CMR10".into();
        let mut equation = wide(1, 160.0, 488.0, "a+b=c", 80.0, 10.0);
        equation.font_name = "CMMI10".into();
        let mut below = wide(
            2,
            72.0,
            476.0,
            "The next sentence returns to ordinary prose in the column.",
            320.0,
            10.0,
        );
        below.font_name = "CMR10".into();
        let seg = segment_with(&[above, equation, below], &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.starts_with("The identity") && !text.contains("next sentence")),
            "{texts:?}"
        );
        assert!(texts.iter().any(|text| text.contains("a+b=c")), "{texts:?}");
        assert!(
            texts.iter().any(|text| text.starts_with("The next")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_wrap_that_opens_with_the_named_symbol_stays_in_the_sentence() {
        let mut id = 0u32;
        let mut glyphs = paint_word(
            &mut id,
            72.0,
            500.0,
            "For a candidate relative to the current harness",
            "CMR10",
        );
        glyphs.extend(paint_word(&mut id, 72.0, 488.0, "𝐻𝑡", "CMMI10"));
        glyphs.extend(paint_word(&mut id, 84.0, 488.0, ", let", "CMR10"));
        let seg = segment_glyphs(&glyphs);
        let texts: Vec<_> = seg.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("harness") && text.contains("let")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_wrapped_clause_with_a_formula_stays_in_the_sentence() {
        let mut id = 0u32;
        let mut glyphs = paint_word(
            &mut id,
            72.0,
            500.0,
            "Following is an equivalent finitary",
            "CMR10",
        );
        glyphs.extend(paint_word(
            &mut id,
            72.0,
            488.0,
            "version. We write ",
            "CMR10",
        ));
        glyphs.extend(paint_word(&mut id, 164.0, 488.0, "[N]={1,2,N}.", "CMMI10"));
        glyphs.extend(paint_word(
            &mut id,
            72.0,
            460.0,
            "Theorem 0.1.2 is the finitary form.",
            "CMR10",
        ));
        let seg = segment_glyphs(&glyphs);
        let texts: Vec<_> = seg.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("finitary version")
                && text.contains("[N]={1,2,N}.")
                && !text.contains("Theorem")),
            "{texts:?}"
        );
        let mut id = 100u32;
        let mut tail = paint_word(
            &mut id,
            72.0,
            400.0,
            "then there is a monochromatic solution",
            "CMR10",
        );
        tail.extend(paint_word(&mut id, 72.0, 388.0, "to ", "CMR10"));
        tail.extend(paint_word(&mut id, 86.0, 388.0, "x+y=z.", "CMMI10"));
        let seg = segment_glyphs(&tail);
        let texts: Vec<_> = seg.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("solution to ") && text.contains("x+y=z")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_math_letter_does_not_split_a_hyphenated_sentence() {
        let mut head = wide(0, 72.0, 500.0, "arcset ", 36.0, 10.0);
        head.font_name = "NimbusRomNo9L-Regu".into();
        let mut letter = wide(1, 110.0, 500.0, "A", 8.0, 10.0);
        letter.font_name = "CMSY10".into();
        let mut tail = wide(
            2,
            120.0,
            500.0,
            "will contain all nearest neighbour arcs after contrac-",
            250.0,
            10.0,
        );
        tail.font_name = "NimbusRomNo9L-Regu".into();
        let mut next = wide(
            3,
            72.0,
            488.0,
            "tion. Instead it guarantees the most attractive edge.",
            280.0,
            10.0,
        );
        next.font_name = "NimbusRomNo9L-Regu".into();
        let segs = segment_glyphs(&[head, letter, tail, next]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("contraction") && text.contains('A')),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.ends_with("contrac-")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_midline_subscript_does_not_block_a_line_break_hyphen() {
        // The subscript sits 1.5pt under the middle of the line, so reading
        // order reaches it after the closing hyphen. It is not the other
        // column, and the hyphen still joins the next line.
        let mut lead = block(0, 108.0, 171.0, 90.0, 10.0, "We use a learned d");
        lead.font_name = "NimbusRomNo9L-Regu".into();
        let mut tail = block(1, 250.0, 171.0, 240.0, 10.0, ". The usual linear transfor-");
        tail.font_name = "NimbusRomNo9L-Regu".into();
        let mut model = block(2, 200.0, 169.5, 36.0, 7.0, "model");
        model.font_name = "NimbusRomNo9L-Regu".into();
        let mut next = block(
            3,
            108.0,
            160.0,
            250.0,
            10.0,
            "mation and softmax over the sequence.",
        );
        next.font_name = "NimbusRomNo9L-Regu".into();
        let segs = segment_glyphs(&[lead, tail, model, next]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| {
                let model_at = text.find("model");
                let transfor_at = text.find("transfor");
                text.contains("transformation")
                    && model_at.is_some_and(|at| transfor_at.is_some_and(|word| at < word))
            }),
            "{texts:?}"
        );
    }

    #[test]
    fn a_gutter_line_number_does_not_glue_to_a_hyphen() {
        // The algorithm line is in the same baseline band. Its small line
        // number sits in the gutter and would bridge the column split.
        let mut prose = block(0, 55.0, 220.0, 220.0, 10.0, "We show how to per-");
        prose.font_name = "NimbusRomNo9L-Regu".into();
        let mut number = block(1, 286.0, 216.5, 4.0, 6.0, "3");
        number.font_name = "NimbusRomNo9L-Medi".into();
        let mut algo = block(2, 330.0, 217.0, 48.0, 10.0, "m := (i, j)");
        algo.font_name = "CMR10".into();
        let mut next = block(
            3,
            55.0,
            208.0,
            200.0,
            10.0,
            "form a more efficient contraction.",
        );
        next.font_name = "NimbusRomNo9L-Regu".into();
        let segs = segment_glyphs(&[prose, number, algo, next]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("perform")),
            "{texts:?}"
        );
        assert!(!texts.iter().any(|text| text.contains("per-")), "{texts:?}");
    }

    #[test]
    fn a_small_parenthetical_index_stays_on_the_hyphenated_line() {
        // `(i)` is set smaller than the sentence, with a gap in front of it
        // because a subscript occupies that space. It is not a line number.
        let mut alg = block(0, 307.0, 594.0, 22.0, 10.0, "ALG");
        alg.font_name = "CMR10".into();
        let mut index = block(1, 339.0, 593.6, 12.0, 5.0, "(i)");
        index.font_name = "CMR5".into();
        let mut rest = block(2, 355.0, 594.0, 140.0, 10.0, ". Let Q be the perfor-");
        rest.font_name = "NimbusRomNo9L-Regu".into();
        let mut next = block(
            3,
            307.0,
            582.0,
            200.0,
            10.0,
            "mance of the algorithm stays whole.",
        );
        next.font_name = "NimbusRomNo9L-Regu".into();
        let segs = segment_glyphs(&[alg, index, rest, next]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("performance")),
            "{texts:?}"
        );
    }

    #[test]
    fn an_indented_hyphen_returns_to_the_column_margin() {
        let mut margin = block(
            0,
            46.0,
            320.0,
            220.0,
            10.0,
            "The lines above already use this column margin.",
        );
        margin.font_name = "NimbusRomNo9L-Regu".into();
        let mut head = block(
            1,
            105.0,
            300.0,
            200.0,
            10.0,
            "Combining those ideas, Soundarara-",
        );
        head.font_name = "NimbusRomNo9L-Regu".into();
        let mut next = block(
            2,
            46.0,
            288.0,
            220.0,
            10.0,
            "jan and Young proved the bound.",
        );
        next.font_name = "NimbusRomNo9L-Regu".into();
        let texts: Vec<_> = segment_glyphs(&[margin, head, next])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("Soundararajan")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_full_hyphenated_line_stays_with_its_own_column() {
        // A definition line ends in `thou-` and sits 48pt to the right of a
        // narrow label column. The closest margin is the definition itself.
        let narrow = block(0, 66.0, 400.0, 80.0, 10.0, "be invoked by other systems");
        let head = block(
            1,
            114.0,
            220.0,
            330.0,
            10.0,
            "DEFINITION Embeddings are vector representations of words",
        );
        let mapped = block(
            2,
            114.0,
            208.0,
            330.0,
            10.0,
            "text units such as sentences or document chunks mapped",
        );
        let broken = block(
            3,
            114.0,
            196.0,
            330.0,
            10.0,
            "into a continuous high-dimensional space or thou-",
        );
        let rest = block(4, 114.0, 184.0, 330.0, 10.0, "sands of dimensions follow.");
        let texts: Vec<_> = segment_glyphs(&[narrow, head, mapped, broken, rest])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("thousands")),
            "{texts:?}"
        );
        assert!(
            !texts
                .iter()
                .any(|text| text.contains("invoked") && text.contains("thou")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_centered_title_does_not_steal_a_hyphenated_body_line() {
        // Several centered lines keep that column's median well to the right
        // of the body. A hyphen within 64pt of the title used to join it, and
        // the next plain line then landed in the author column instead.
        let mut title_a = block(0, 190.0, 700.0, 210.0, 14.0, "Sturmian beta-shifts");
        title_a.font_name = "CMBX12".into();
        let mut title_b = block(
            1,
            188.0,
            682.0,
            214.0,
            14.0,
            "and typical periodic optimization",
        );
        title_b.font_name = "CMBX12".into();
        let mut title_c = block(2, 192.0, 664.0, 200.0, 14.0, "of continuous maps");
        title_c.font_name = "CMBX12".into();
        let mut author_a = block(3, 152.0, 630.0, 180.0, 10.0, "Ada Lovelace");
        author_a.font_name = "CMR10".into();
        let mut author_b = block(4, 155.0, 618.0, 170.0, 10.0, "Grace Hopper");
        author_b.font_name = "CMR10".into();
        let mut hyphen = block(
            5,
            128.0,
            570.0,
            230.0,
            10.0,
            "of Lipschitz maps and typical periodic or-",
        );
        hyphen.font_name = "CMR10".into();
        let mut cont = block(
            6,
            128.0,
            559.0,
            230.0,
            10.0,
            "bit contains an example of the shift space.",
        );
        cont.font_name = "CMR10".into();
        let mut plain = block(
            7,
            128.0,
            548.0,
            230.0,
            10.0,
            "beta-shifts whose expansion stays in one paragraph.",
        );
        plain.font_name = "CMR10".into();
        let texts: Vec<_> = segment_glyphs(&[
            title_a, title_b, title_c, author_a, author_b, hyphen, cont, plain,
        ])
        .iter()
        .map(|seg| seg.text.clone())
        .collect();
        assert!(
            texts.iter().any(|text| {
                text.contains("periodic orbit") && text.contains("beta-shifts whose")
            }),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Sturmian") && !text.contains("orbit")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_version_number_keeps_the_hyphen_and_is_not_a_contents_row() {
        let mut head = block(
            0,
            108.0,
            400.0,
            240.0,
            10.0,
            "the baseline published by Ye et al. (2025): GPT-",
        );
        head.font_name = "NimbusRomNo9L-Regu".into();
        let mut next = block(
            1,
            108.0,
            388.0,
            240.0,
            10.0,
            "2 Scratch, Stream-of-Search, LLaMA",
        );
        next.font_name = "NimbusRomNo9L-Regu".into();
        let texts: Vec<_> = segment_glyphs(&[head, next])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("GPT-2 Scratch")),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.trim_end().ends_with("GPT-")),
            "{texts:?}"
        );
    }

    #[test]
    fn an_ellipsis_does_not_make_the_next_line_a_contents_row() {
        let mut head = block(
            0,
            72.0,
            200.0,
            200.0,
            10.0,
            "isolate it on one side of the equa-",
        );
        head.font_name = "NimbusRomNo9L-Regu".into();
        let mut tion = block(1, 72.0, 188.0, 24.0, 10.0, "tion");
        tion.font_name = "NimbusRomNo9L-Regu".into();
        let mut dots = Vec::new();
        for index in 0..6 {
            let x = 100.0 + index as f32 * 6.0;
            let mut dot = block(2 + index, x, 188.0, 4.0, 10.0, ".");
            dot.font_name = "NimbusRomNo9L-Regu".into();
            dots.push(dot);
        }
        let mut glyphs = vec![head, tion];
        glyphs.extend(dots);
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("equation")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_subscript_near_the_left_edge_stays_on_the_hyphen_line() {
        // The subscript is 6pt inside the line's left edge and 1.5pt lower.
        // Reading order reaches it after the hyphen. It is not the other column.
        let mut lead = block(0, 108.0, 363.0, 6.0, 10.0, "s");
        lead.font_name = "NimbusRomNo9L-Regu".into();
        let mut sub = block(1, 114.0, 361.5, 4.0, 7.0, "0");
        sub.font_name = "CMMI7".into();
        let mut tail = block(2, 116.0, 363.0, 200.0, 10.0, "olution ac-");
        tail.font_name = "NimbusRomNo9L-Regu".into();
        let mut next = block(
            3,
            108.0,
            352.0,
            220.0,
            10.0,
            "curacy. As shown in Fig. 3a today.",
        );
        next.font_name = "NimbusRomNo9L-Regu".into();
        let texts: Vec<_> = segment_glyphs(&[lead, sub, tail, next])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("accuracy")),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.trim_end().ends_with("ac-")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_right_column_hyphen_continues_on_the_next_page() {
        let mut upper = glyph(0, 307.0, 76.0, "Any non-", false);
        upper.page_index = 0;
        upper.font_size = 10.0;
        let mut title = glyph(
            1,
            148.0,
            740.0,
            "Expertise Trees Resolve Knowledge Limitations",
            false,
        );
        title.page_index = 1;
        title.font_size = 10.0;
        let mut lower = glyph(
            2,
            55.0,
            710.0,
            "specialized algorithm must incur linear regret.",
            false,
        );
        lower.page_index = 1;
        lower.font_size = 10.0;
        let texts: Vec<_> = segment_glyphs(&[upper, title, lower])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("non-specialized")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Expertise Trees Resolve")),
            "{texts:?}"
        );
        assert!(
            !texts
                .iter()
                .any(|text| text.contains("non-Expertise") || text.contains("nonExpertise")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_numbered_footnote_does_not_take_the_next_pages_sentence() {
        let mut body = wide(
            0,
            108.0,
            140.0,
            "we then perform the attention function in parallel yielding dimensional",
            390.0,
            10.0,
        );
        body.page_index = 0;
        let mut note = wide(
            1,
            108.0,
            72.0,
            "4To illustrate why the dot products get large assume the components",
            360.0,
            8.0,
        );
        note.page_index = 0;
        let mut next = wide(
            2,
            108.0,
            700.0,
            "output values are concatenated and projected again as shown.",
            340.0,
            10.0,
        );
        next.page_index = 1;
        let texts: Vec<_> = segment_glyphs(&[body, note, next])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("yielding") && text.contains("output values")),
            "the body lost its next-page tail: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| { text.contains("4To illustrate") && !text.contains("output values") }),
            "the footnote absorbed the next page: {texts:?}"
        );
    }

    #[test]
    fn a_finished_right_column_does_not_jump_to_the_next_page() {
        let mut upper = glyph(0, 307.0, 76.0, "This paragraph is finished.", false);
        upper.page_index = 0;
        upper.font_size = 10.0;
        let mut lower = glyph(
            1,
            55.0,
            710.0,
            "the next page starts its own paragraph here.",
            false,
        );
        lower.page_index = 1;
        lower.font_size = 10.0;
        let texts: Vec<_> = segment_glyphs(&[upper, lower])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert_eq!(texts.len(), 2, "{texts:?}");
        assert!(texts.iter().any(|text| text.ends_with("finished.")));
        assert!(texts.iter().any(|text| text.starts_with("the next page")));
    }

    #[test]
    fn a_right_column_sentence_continues_on_the_next_page() {
        // BERT page 8 ends the right column with `presented`. Page 9's left
        // column starts `mixed results`, and its right column is a table.
        let mut left = glyph(
            0,
            72.0,
            76.0,
            "significantly improve results on SQuAD, but the",
            false,
        );
        left.font_size = 10.0;
        let mut right_top = glyph(
            1,
            307.0,
            700.0,
            "results are still far worse than those of the pretrained models.",
            false,
        );
        right_top.font_size = 10.0;
        let mut right_bot = glyph(2, 307.0, 76.0, "Peters et al. (2018b) presented", false);
        right_bot.font_size = 10.0;
        let mut next = glyph(
            3,
            72.0,
            710.0,
            "mixed results on the downstream task impact.",
            false,
        );
        next.page_index = 1;
        next.font_size = 10.0;
        let mut table = glyph(4, 317.0, 700.0, "System Dev F1 Test F1", false);
        table.page_index = 1;
        table.font_size = 10.0;
        let texts: Vec<_> = segment_glyphs(&[left, right_top, right_bot, next, table])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("presented") && text.contains("mixed results")),
            "{texts:?}"
        );
        assert!(
            !texts
                .iter()
                .any(|text| text.contains("presented") && text.contains("System Dev")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_conference_header_and_footnote_do_not_block_the_next_page() {
        // ICLR puts the abstract in the right column and a starred footnote
        // under the introduction. The sentence continues under the next
        // page's running header and figure caption.
        let mut intro = glyph(0, 108.0, 90.0, "and shapes (Cai", false);
        intro.font_size = 10.0;
        let mut note = glyph(
            1,
            108.0,
            62.0,
            "˚Work partially done during an internship at Google Brain.",
            false,
        );
        note.font_size = 8.0;
        let mut abstract_body = glyph(
            2,
            320.0,
            500.0,
            "Creating noise from data is easy and this abstract stands on its own.",
            false,
        );
        abstract_body.font_size = 10.0;
        let mut header = glyph(
            3,
            108.0,
            760.0,
            "Published as a conference paper at ICLR 2021",
            false,
        );
        header.page_index = 1;
        header.font_size = 9.0;
        let mut caption = glyph(
            4,
            108.0,
            700.0,
            "Figure 1: Solving a reverse-time SDE yields a score-based model.",
            false,
        );
        caption.page_index = 1;
        caption.font_size = 9.0;
        let mut next = block(
            5,
            108.0,
            500.0,
            220.0,
            10.0,
            "et al., 2020). To enable new sampling methods and further models.",
        );
        next.page_index = 1;
        let texts: Vec<_> = segment_glyphs(&[intro, note, abstract_body, header, caption, next])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Cai") && text.contains("To enable new sampling")),
            "{texts:?}"
        );
        assert!(
            !texts
                .iter()
                .any(|text| text.contains("Cai") && text.contains("Creating noise")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Work partially done")),
            "{texts:?}"
        );
    }

    #[test]
    fn sde_intro_continues_when_the_pdf_is_present() {
        let path = std::path::Path::new("/tmp/rpt-e2e/diverse/sde-12.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(path).unwrap();
        let extraction = doc.extract();
        let placed = segment_placed(
            &extraction.glyphs,
            &SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let merged_hold =
            crate::rewrite::merge_stacked_paragraphs(placed.segments.clone(), &extraction.glyphs);
        let sohl = merged_hold
            .iter()
            .find(|item| item.text.contains("positive noise scales"))
            .expect("noise-scale paragraph");
        assert!(
            !sohl.text.contains("Our goal is to construct"),
            "DDPM column swallowed the next section: {}",
            sohl.text.chars().take(80).collect::<String>()
        );
        let wiener = merged_hold
            .iter()
            .find(|item| item.text.contains("standard Wiener process (a.k.a."))
            .expect("wiener paragraph");
        assert!(
            wiener
                .text
                .contains("where w is the standard Wiener process"),
            "wiener prose was woven into another line: {}",
            wiener.text.chars().take(160).collect::<String>()
        );
        assert!(
            !wiener.text.starts_with("(5)"),
            "the equation number was glued onto the definition: {}",
            wiener.text.chars().take(40).collect::<String>()
        );
        assert!(
            !wiener.text.contains("Sohl-Dickstein"),
            "wiener paragraph absorbed the previous section"
        );
        assert!(
            !merged_hold
                .iter()
                .any(|item| item.text.contains("anNcestral")),
            "ancestral was interleaved with a formula mark"
        );
        let merged = merged_hold;
        let hit = merged
            .iter()
            .find(|item| item.text.contains("and shapes (Cai"))
            .expect("intro fragment");
        assert!(
            hit.text.contains("To enable new sampling"),
            "page 1 citation stayed split ({} chars): {}",
            hit.text.len(),
            hit.text.chars().rev().take(80).collect::<String>()
        );
        assert!(
            !hit.text.contains("Creating noise"),
            "intro joined the abstract: {}",
            hit.text.chars().take(120).collect::<String>()
        );
    }

    #[test]
    fn bert_presented_joins_the_next_page_when_the_pdf_is_present() {
        let path = std::path::Path::new("/tmp/rpt-e2e/pdfs/bert.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(path).unwrap();
        let extraction = doc.extract();
        let placed = segment_placed(
            &extraction.glyphs,
            &SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let merged = crate::rewrite::merge_stacked_paragraphs(placed.segments, &extraction.glyphs);
        let hit = merged
            .iter()
            .find(|item| item.text.contains("Peters et al. (2018b) presented"))
            .expect("presented fragment");
        assert!(
            hit.text.contains("mixed results"),
            "page 8 right column stayed split: {}",
            hit.text.chars().rev().take(80).collect::<String>()
        );
    }

    #[test]
    fn a_body_glyph_returning_to_the_margin_starts_a_new_line() {
        // A mark just under the left line bridges the right column onto it.
        // The next left line comes back to the same margin and stays its own line.
        let mut left = block(
            0,
            55.0,
            259.0,
            220.0,
            10.0,
            "ficial. We develop a model adapted",
        );
        left.font_name = "NimbusRomNo9L-Regu".into();
        let mut arrow = block(1, 371.0, 254.5, 12.0, 10.0, "#");
        arrow.font_name = "CMSY10".into();
        let mut right = block(
            2,
            307.0,
            250.5,
            220.0,
            10.0,
            "the features of this vector stay apart",
        );
        right.font_name = "NimbusRomNo9L-Regu".into();
        let mut next = block(
            3,
            55.0,
            246.8,
            220.0,
            10.0,
            "the setting of the next line stays whole.",
        );
        next.font_name = "NimbusRomNo9L-Regu".into();
        let texts: Vec<_> = segment_glyphs(&[left, arrow, right, next])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("the setting of the next line")
                    && !text.contains("features")),
            "{texts:?}"
        );
    }

    #[test]
    fn clusterfug_prose_with_math_letters_stays_one_paragraph() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/ci/pmlr-v202-abbas23a.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(&path).unwrap();
        let extraction = doc.extract();
        let segs = segment_glyphs(&extraction.glyphs);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            !texts.iter().any(|text| {
                let tail = text.trim_end();
                tail.ends_with("contrac-") || tail.ends_with("per-") || tail.ends_with("oper-")
            }),
            "hyphen split: {:?}",
            texts.iter().find(|text| {
                let tail = text.trim_end();
                tail.ends_with('-')
                    && (tail.contains("contrac") || tail.contains(" per") || tail.contains("oper"))
            })
        );
        assert!(
            texts.iter().any(|text| text.contains("contraction")),
            "contrac- was not joined"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("show how to perform")),
            "per- was not joined"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("contraction by operating")),
            "oper- was not joined"
        );
        assert!(
            !texts.contains(&"neighbours k is set to 1, a larger value does not benefit"),
            "math k split the sentence"
        );
        assert!(
            texts.iter().any(|text| {
                text.contains("fundamental limitation") && text.contains("quadrants")
            }),
            "subscript split the limitation sentence: {:?}",
            texts
                .iter()
                .find(|text| text.contains("fundamental limitation"))
        );
        let headers = texts
            .iter()
            .filter(|text| text.trim() == "Clustering Fully connected Graphs by Multicut")
            .count();
        assert!(
            headers >= 11,
            "running headers were kept inside figures or tables: {headers}"
        );
    }

    #[test]
    fn attention_projection_line_stays_original() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/ci/neurips-2017-attention.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(&path).unwrap();
        let extraction = doc.extract();
        let full = segment_placed(
            &extraction.glyphs,
            &SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let texts: Vec<_> = full.segments.iter().map(|seg| seg.text.clone()).collect();
        let rewritten =
            crate::rewrite::merge_stacked_paragraphs(full.segments.clone(), &extraction.glyphs);
        let _ = rewritten;
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Where the projections are parameter matrices")),
            "the projection sentence was kept as math: {texts:?}"
        );
        assert!(
            texts.iter().all(|text| !text.contains("Where Othe")),
            "the projection sentence was split: {texts:?}"
        );
        assert!(
            texts.iter().any(|text| {
                text.starts_with("In this work we employ h = 8")
                    && text.contains("full dimensionality")
            }),
            "the following prose sentence was kept or merged: {texts:?}"
        );
        let stray: Vec<_> = texts
            .iter()
            .filter(|text| {
                let trimmed = text.trim();
                trimmed.chars().count() < 40
                    && (trimmed.contains("hdv") || trimmed.contains("dmodel×dk"))
            })
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            stray.is_empty(),
            "a superscript run was left as its own segment: {stray:?}"
        );
        let exponent = extraction.glyphs.iter().any(|glyph| {
            glyph.page_index == 4
                && glyph.unicode == "O"
                && (130.0..145.0).contains(&glyph.matrix[4])
                && full
                    .segments
                    .iter()
                    .any(|seg| seg.glyph_ids.contains(&glyph.id))
        });
        assert!(exponent, "W^O was not grouped into the sentence");
    }

    #[test]
    fn e2e_attention_segments_for_replay() {
        let path = std::path::PathBuf::from("/tmp/rpt-e2e/pdfs/attention.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(&path).unwrap();
        let extraction = doc.extract();
        let full = segment_placed(
            &extraction.glyphs,
            &SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let rewritten =
            crate::rewrite::merge_stacked_paragraphs(full.segments.clone(), &extraction.glyphs);
        let employ = rewritten.iter().find(|seg| {
            seg.text
                .starts_with("In this work we employ h = 8 parallel attention")
        });
        let employ = employ.map(|seg| seg.text.as_str()).unwrap_or("");
        assert!(
            employ.contains("⟦0⟧ = ⟦1⟧ = ⟦2⟧⟦3⟧ = 64") && employ.contains("full dimensionality"),
            "employ sentence split: {employ}"
        );
        let projection = rewritten.iter().find(|seg| {
            seg.text
                .contains("Where the projections are parameter matrices")
        });
        let projection = projection.map(|seg| seg.text.as_str()).unwrap_or("");
        assert!(
            projection.contains('⟦'),
            "projection sentence has no formula hole: {projection}"
        );
        let kept_where = extraction.glyphs.iter().any(|glyph| {
            glyph.page_index == 4
                && glyph.unicode == "O"
                && (130.0..145.0).contains(&glyph.matrix[4])
                && rewritten
                    .iter()
                    .any(|seg| seg.glyph_ids.contains(&glyph.id))
        });
        assert!(kept_where, "e2e W^O was not grouped into the sentence");
        assert!(
            rewritten
                .iter()
                .any(|seg| seg.text.contains("has mean 0 and variance ⟦")),
            "the summation footnote was not grouped: {:?}",
            rewritten
                .iter()
                .find(|seg| seg.text.contains("mean 0"))
                .map(|seg| seg.text.as_str())
        );
    }

    #[test]
    fn attention_subscript_does_not_leave_a_line_break_hyphen() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/ci/neurips-2017-attention.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(&path).unwrap();
        let extraction = doc.extract();
        let segs = segment_glyphs(&extraction.glyphs);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("transformation")),
            "transfor- was not joined"
        );
        assert!(
            !texts
                .iter()
                .any(|text| text.trim_end().ends_with("transfor-")),
            "{:?}",
            texts.iter().find(|text| text.contains("transfor"))
        );
    }

    #[test]
    fn acl_hyphen_continuations_are_not_kept_inside_the_figure() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/ci/2024-acl-long-7.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(&path).unwrap();
        let extraction = doc.extract();
        let seg = segment_placed(
            &extraction.glyphs,
            &SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("round-to-nearest")),
            "round- was not joined"
        );
        assert!(
            !texts.iter().any(|text| text.trim_end().ends_with("round-")),
            "round- was left split"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("overall performance")),
            "over- was kept inside the figure"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("MetaMath-7B with domain-specific")),
            "MetaMath- was kept inside the table"
        );
        assert!(
            !texts.iter().any(|text| {
                let tail = text.trim_end();
                tail.ends_with("over-") || tail.ends_with("MetaMath-")
            }),
            "{:?}",
            texts.iter().find(|text| {
                let tail = text.trim_end();
                tail.ends_with("over-") || tail.ends_with("MetaMath-")
            })
        );
    }

    fn paint_word(id: &mut u32, x: f32, y: f32, text: &str, font: &str) -> Vec<Glyph> {
        let mut glyphs = Vec::new();
        let mut cursor = x;
        for ch in text.chars() {
            let mut glyph = glyph(*id, cursor, y, &ch.to_string(), false);
            *id += 1;
            glyph.font_name = font.into();
            glyph.font_size = 10.0;
            glyph.matrix = [10.0, 0.0, 0.0, 10.0, cursor, y];
            let width = if ch == ' ' { 3.0 } else { 5.5 };
            glyph.bbox = [cursor, y, cursor + width, y + 8.0];
            cursor += width;
            glyphs.push(glyph);
        }
        glyphs
    }

    #[test]
    fn a_bold_run_in_label_is_its_own_segment() {
        let mut id = 0u32;
        let mut glyphs = paint_word(&mut id, 46.0, 460.0, "Definition 1.1. ", "CMBX10");
        glyphs.extend(paint_word(
            &mut id,
            132.0,
            460.0,
            "If psi is a Dirichlet character then",
            "CMTI10",
        ));
        glyphs.extend(paint_word(
            &mut id,
            46.0,
            446.0,
            "the pair of numbers is admissible.",
            "CMTI10",
        ));
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("Definition")
                && text.contains("1.1")
                && !text.contains("Dirichlet")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|text| text.contains("Dirichlet")
                && text.contains("admissible")
                && !text.contains("Definition")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_medium_run_in_heading_is_its_own_segment() {
        let mut id = 0u32;
        let mut glyphs = paint_word(
            &mut id,
            72.0,
            400.0,
            "Proactive compaction outperforms full-history execution. ",
            "NimbusRomNo9L-Medi",
        );
        glyphs.extend(paint_word(
            &mut id,
            430.0,
            400.0,
            "Under the setting, all evaluated",
            "NimbusRomNo9L-Regu",
        ));
        glyphs.extend(paint_word(
            &mut id,
            72.0,
            386.0,
            "proactive methods continue the paragraph.",
            "NimbusRomNo9L-Regu",
        ));
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("outperforms")
                && text.contains("execution")
                && !text.contains("Under")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Under") && !text.contains("outperforms")),
            "{texts:?}"
        );
    }

    #[test]
    fn an_italic_proof_label_is_its_own_segment() {
        let mut id = 0u32;
        let mut glyphs = paint_word(&mut id, 46.0, 400.0, "Proof. ", "CMTI10");
        glyphs.extend(paint_word(
            &mut id,
            90.0,
            400.0,
            "Let the field be given here and",
            "CMR10",
        ));
        glyphs.extend(paint_word(
            &mut id,
            46.0,
            386.0,
            "the bound follows from the lemma.",
            "CMR10",
        ));
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Proof") && !text.contains("field")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|text| text.contains("field")
                && text.contains("lemma")
                && !text.contains("Proof")),
            "{texts:?}"
        );
    }

    #[test]
    fn an_italic_citation_stays_inside_the_sentence() {
        let mut id = 0u32;
        let mut glyphs = paint_word(&mut id, 46.0, 400.0, "Smith et al. ", "CMTI10");
        glyphs.extend(paint_word(
            &mut id,
            116.0,
            400.0,
            "showed that the bound holds.",
            "CMR10",
        ));
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Smith") && text.contains("showed")),
            "{texts:?}"
        );
    }

    #[test]
    fn an_arrow_label_is_its_own_segment() {
        let mut id = 0u32;
        let mut glyphs = paint_word(&mut id, 46.0, 400.0, "Ratings → ", "Times-Italic");
        glyphs.extend(paint_word(
            &mut id,
            104.0,
            400.0,
            "score is kept for the ordered scale here.",
            "Times-Roman",
        ));
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Ratings") && !text.contains("score")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("score") && !text.contains("Ratings")),
            "{texts:?}"
        );
    }

    #[test]
    fn an_italic_sentence_above_roman_body_is_its_own_segment() {
        let mut id = 0u32;
        let mut glyphs = paint_word(
            &mut id,
            46.0,
            400.0,
            "Obstacle 1: the predictor is wasteful.",
            "Times-Italic",
        );
        glyphs.extend(paint_word(
            &mut id,
            46.0,
            386.0,
            "The underlying signal stays fixed here.",
            "Times-Roman",
        ));
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Obstacle") && !text.contains("underlying")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("underlying") && !text.contains("Obstacle")),
            "{texts:?}"
        );
    }

    #[test]
    fn two_italic_lines_stay_one_paragraph() {
        let mut id = 0u32;
        let mut glyphs = paint_word(
            &mut id,
            46.0,
            400.0,
            "The bound is strict for this field.",
            "Times-Italic",
        );
        glyphs.extend(paint_word(
            &mut id,
            46.0,
            386.0,
            "It follows from the lemma above.",
            "Times-Italic",
        ));
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(
            texts[0].contains("bound") && texts[0].contains("lemma"),
            "{texts:?}"
        );
    }

    #[test]
    fn a_bibliography_title_stays_with_its_year() {
        let mut id = 0u32;
        let mut glyphs = paint_word(&mut id, 46.0, 400.0, "Algorithms. ", "Times-Italic");
        glyphs.extend(paint_word(
            &mut id,
            112.0,
            400.0,
            "SIAM Publications, 1993.",
            "Times-Roman",
        ));
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Algorithms") && text.contains("1993")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_bold_heading_stays_one_segment() {
        let mut id = 0u32;
        let glyphs = paint_word(&mut id, 46.0, 438.0, "4 Proof of Theorem 1.2.", "CMBX12");
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(
            texts[0].contains("Proof") && texts[0].contains("Theorem"),
            "{texts:?}"
        );
    }

    #[test]
    fn an_italic_email_label_is_its_own_segment() {
        let mut id = 0u32;
        let mut glyphs = paint_word(&mut id, 46.0, 400.0, "Email address", "Times-Italic");
        glyphs.extend(paint_word(&mut id, 115.0, 400.0, ": ", "Times-Roman"));
        glyphs.extend(paint_word(
            &mut id,
            128.0,
            400.0,
            "name@ucl.ac.uk is listed",
            "Times-Roman",
        ));
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Email") && !text.contains("ucl")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("ucl") && !text.contains("Email")),
            "{texts:?}"
        );
    }

    #[test]
    fn glossary_lines_and_obstacle_sentences_leave_the_body() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/ci");
        let cases = [
            (
                "arxiv-2609.36965.pdf",
                "category labels",
                "retain the original",
            ),
            ("arxiv-2610.02193.pdf", "Obstacle 1", "underlying signal"),
            ("arxiv-2610.01998.pdf", "Email address", "@"),
        ];
        for (file, label, body) in cases {
            let path = root.join(file);
            if !path.exists() {
                continue;
            }
            let doc = crate::extract::PdfDocument::open(&path).unwrap();
            let extraction = doc.extract();
            let seg = segment_placed(
                &extraction.glyphs,
                &SegmentFlags::default(),
                &extraction.regions,
                &extraction.pages,
            );
            let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
            assert!(
                texts
                    .iter()
                    .any(|text| text.contains(label) && !text.contains(body)),
                "{file} label stayed on the body"
            );
            assert!(
                texts
                    .iter()
                    .any(|text| text.contains(body) && !text.contains(label)),
                "{file} body still starts with the label"
            );
        }
    }

    #[test]
    fn definition_run_ins_are_split_from_the_theorem_body() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/ci/arxiv-2610.01998.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(&path).unwrap();
        let extraction = doc.extract();
        let seg = segment_placed(
            &extraction.glyphs,
            &SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| {
                let trimmed = text.trim();
                trimmed.starts_with("Definition 1.1") && !trimmed.contains("Dirichlet")
            }),
            "label was not peeled"
        );
        assert!(
            texts.iter().any(
                |text| text.contains("Dirichlet character") && !text.contains("Definition 1.1")
            ),
            "body still contains the label"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Proof of Theorem 1.2")),
            "the heading was split"
        );
        assert!(
            texts.iter().any(|text| {
                let trimmed = text.trim();
                trimmed.starts_with("Proof.") && !trimmed.contains("field")
            }),
            "italic Proof. was left on the body"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Let K be the field") && !text.contains("Proof.")),
            "the proof body still starts with the label"
        );
    }

    #[test]
    fn a_display_equation_is_not_the_next_column() {
        let mut left = glyph(0, 72.0, 80.0, "we run the base agent on", false);
        left.font_size = 10.0;
        let mut formula = glyph(1, 320.0, 700.0, "for any t in the open interval k", false);
        formula.font_name = "CMMI10".into();
        formula.font_size = 10.0;
        let texts: Vec<_> = segment_glyphs(&[left, formula])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("base agent")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("open interval") && !text.contains("base agent")),
            "{texts:?}"
        );
        assert!(
            !texts
                .iter()
                .any(|text| text.contains("base agent") && text.contains("open interval")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_display_equation_does_not_open_the_next_page() {
        let mut formula = glyph(0, 72.0, 70.0, "alpha equals the minimum of both", false);
        formula.page_index = 0;
        formula.font_name = "CMMI10".into();
        formula.font_size = 10.0;
        let mut prose = glyph(
            1,
            72.0,
            700.0,
            "the next page starts the following paragraph.",
            false,
        );
        prose.page_index = 1;
        prose.font_size = 10.0;
        let texts: Vec<_> = segment_glyphs(&[formula, prose])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert_eq!(texts.len(), 2, "{texts:?}");
        assert!(texts.iter().any(|text| text.contains("minimum of both")));
        assert!(texts
            .iter()
            .any(|text| text.contains("following paragraph")));
    }

    #[test]
    fn display_equations_stay_out_of_the_surrounding_prose() {
        let cases = [
            (
                "neurips-2023-00296c0e",
                "triangular inequality",
                "for any t",
            ),
            ("neurips-2023-00296c0e", "baseline method", "α"),
            ("pmlr-v202-abbas23a", "k-nearest neighbours", "⟨fi"),
        ];
        for (name, prose, formula) in cases {
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../corpus/ci/{name}.pdf"));
            if !path.exists() {
                continue;
            }
            let doc = crate::extract::PdfDocument::open(&path).unwrap();
            let extraction = doc.extract();
            let seg = segment_placed(
                &extraction.glyphs,
                &SegmentFlags::default(),
                &extraction.regions,
                &extraction.pages,
            );
            assert!(
                seg.segments.iter().any(|item| item.text.contains(formula)),
                "{name} lost the equation"
            );
            if let Some(item) = seg
                .segments
                .iter()
                .find(|item| item.text.contains(prose) && item.text.contains(formula))
            {
                let sample: String = item.text.chars().take(180).collect();
                panic!("{name} glued {formula:?} into {prose:?}: {sample}");
            }
        }
    }

    #[test]
    fn an_unmapped_gutter_does_not_glue_an_equation_to_round_to_nearest() {
        let mut equation = block(0, 205.0, 535.0, 16.0, 6.0, "|{y}|");
        equation.font_name = "CMR6".into();
        let mut prose = block(
            1,
            306.0,
            535.0,
            220.0,
            10.9,
            "Baselines include vanilla round-",
        );
        prose.font_name = "NimbusRomNo9L-Regu".into();
        let mut hole = block(2, 260.0, 530.0, 4.0, 10.0, "x");
        hole.unicode.clear();
        hole.unmapped = true;
        let mut next = block(
            3,
            306.0,
            521.4,
            220.0,
            10.9,
            "to-nearest (RTN), GPTQ and AWQ.",
        );
        next.font_name = "NimbusRomNo9L-Regu".into();
        let texts: Vec<_> = segment_glyphs(&[equation, prose, hole, next])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("round-to-nearest")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("|{y}|") && !text.contains("round-")),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("roundto")),
            "{texts:?}"
        );
    }

    #[test]
    fn hc_dlm_subscript_does_not_leave_a_line_break_hyphen() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/ci/arxiv-2610.02193.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(&path).unwrap();
        let extraction = doc.extract();
        let segs = segment_glyphs(&extraction.glyphs);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("solution accuracy")),
            "ac- was not joined"
        );
        assert!(
            !texts.iter().any(|text| text.trim_end().ends_with("ac-")),
            "{:?}",
            texts.iter().find(|text| text.contains("ac-"))
        );
    }

    #[test]
    fn abels_hyphens_join_across_a_symbol_and_a_page_break() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/ci/pmlr-v202-abels23a.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(&path).unwrap();
        let extraction = doc.extract();
        let segs = segment_glyphs(&extraction.glyphs);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("Similarly")),
            "Simi- was not joined past the vector arrow"
        );
        assert!(
            !texts.iter().any(|text| text.trim_end().ends_with("Simi-")),
            "Simi- was left split"
        );
        assert!(
            texts.iter().any(|text| text.contains("detecting")),
            "de- was not joined across the page"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("partition the space")),
            "parti- was not joined across the page"
        );
        assert!(
            texts.iter().any(|text| text.contains("non-specialized")),
            "non- was not joined across the page"
        );
        assert!(
            texts.iter().any(|text| text.contains("beneficial")),
            "bene- was split off the next line"
        );
        assert!(
            !texts.iter().any(|text| text.trim_end().ends_with("bene-")),
            "bene- was not joined"
        );
        assert!(
            !texts.iter().any(|text| {
                let tail = text.trim_end();
                tail.ends_with("de-") || tail.ends_with("parti-") || tail.ends_with("non-")
            }),
            "{:?}",
            texts.iter().find(|text| {
                let tail = text.trim_end();
                tail.ends_with("de-") || tail.ends_with("parti-") || tail.ends_with("non-")
            })
        );
    }

    #[test]
    fn a_figure_box_does_not_keep_the_rest_of_a_body_hyphen() {
        let mut upper = block(
            0,
            306.0,
            560.0,
            220.0,
            10.0,
            "verges more rapidly but also delivers superior over-",
        );
        upper.font_name = "NimbusRomNo9L-Regu".into();
        let mut lower = block(
            1,
            306.0,
            546.5,
            158.0,
            10.0,
            "all performance compared to TSLD.",
        );
        lower.font_name = "NimbusRomNo9L-Regu".into();
        let regions = [crate::glyph::PaintedRegion {
            page_index: 0,
            bbox: [300.0, 530.0, 480.0, 560.0],
            kind: "image".into(),
        }];
        let texts: Vec<_> =
            segment_placed(&[upper, lower], &SegmentFlags::default(), &regions, &[])
                .segments
                .iter()
                .map(|seg| seg.text.clone())
                .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("overall performance")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_vector_arrow_between_baselines_does_not_split_the_hyphen() {
        // The arrow sits 5.3pt under the line, just past the same-baseline
        // cutoff, between the hyphen and the next line.
        let mut lead = block(0, 307.0, 601.0, 70.0, 10.0, "matrix of the ");
        lead.font_name = "NimbusRomNo9L-Regu".into();
        let mut hash = block(1, 332.0, 595.7, 6.0, 10.0, "#");
        hash.font_name = "TeX-vect10".into();
        let mut arrow = block(2, 338.0, 595.7, 6.0, 10.0, "»");
        arrow.font_name = "TeX-vect10".into();
        let mut tail = block(3, 400.0, 601.0, 140.0, 10.0, "context at time t. Simi-");
        tail.font_name = "NimbusRomNo9L-Regu".into();
        let mut next = block(
            4,
            307.0,
            589.0,
            230.0,
            10.0,
            "larly, the column denotes this matrix.",
        );
        next.font_name = "NimbusRomNo9L-Regu".into();
        let texts: Vec<_> = segment_glyphs(&[lead, hash, arrow, tail, next])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("Similarly")),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.trim_end().ends_with("Simi-")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_subscript_between_baselines_stays_in_the_paragraph() {
        let mut above = wide(
            0,
            72.0,
            500.0,
            "f and f have one fundamental limitation: Clusters will by",
            300.0,
            10.0,
        );
        above.font_name = "NimbusRomNo9L-Regu".into();
        let mut sub = wide(1, 78.0, 494.0, "i", 4.0, 6.0);
        sub.font_name = "CMMI7".into();
        let mut below = wide(
            2,
            72.0,
            488.0,
            "default occupy whole quadrants whenever the angle is small.",
            300.0,
            10.0,
        );
        below.font_name = "NimbusRomNo9L-Regu".into();
        let segs = segment_glyphs(&[above, sub, below]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| {
                text.contains("fundamental limitation")
                    && text.contains('i')
                    && text.contains("quadrants")
            }),
            "{texts:?}"
        );
    }

    #[test]
    fn a_big_delimiter_under_a_heading_does_not_join_its_words() {
        let mut head = wide(
            0,
            72.0,
            760.1,
            "Definition 3 (Fisher Information Selection). Let",
            240.0,
            10.9,
        );
        head.font_name = "NimbusRomNo9L-Medi".into();
        let mut open = wide(1, 99.0, 755.4, "(", 5.0, 10.9);
        open.font_name = "CMEX10".into();
        let mut close = wide(2, 187.0, 755.4, ")", 5.0, 10.9);
        close.font_name = "CMEX10".into();
        let segs = segment_glyphs(&[head, open, close]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains("Definition 3 (Fisher Information Selection)")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_lone_display_delimiter_does_not_claim_a_hanging_bullet_line() {
        let mut body = wide(
            0,
            70.9,
            520.0,
            "A body paragraph line that sets the left margin of the column.",
            227.0,
            10.0,
        );
        body.font_name = "NimbusRomNo9L-Regu".into();
        let mut open = wide(1, 99.0, 500.0, "(", 5.0, 10.9);
        open.font_name = "CMEX10".into();
        let mut bullet = wide(
            2,
            84.0,
            400.0,
            "\u{2022} Relational signals: user-item collaborative features from interaction",
            214.0,
            10.0,
        );
        bullet.font_name = "NimbusRomNo9L-Regu".into();
        let mut tail = wide(
            3,
            92.7,
            388.0,
            "histories, encoding user preference patterns.",
            160.0,
            10.0,
        );
        tail.font_name = "NimbusRomNo9L-Regu".into();
        let segs = segment_glyphs(&[body, open, bullet, tail]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts.iter().any(|t| t.contains("interaction histories")),
            "{texts:?}"
        );
    }

    #[test]
    fn formula_marks_far_below_a_heading_do_not_join_its_words() {
        let mut head = wide(
            0,
            72.0,
            760.0,
            "Definition 3 (Fisher Information Selection). Let",
            240.0,
            10.9,
        );
        head.font_name = "NimbusRomNo9L-Medi".into();
        let mut top = wide(1, 180.0, 750.5, "\u{22a4}", 5.0, 8.0);
        top.font_name = "CMSY8".into();
        let mut inv = wide(2, 192.0, 752.6, "\u{2212}1", 10.0, 8.0);
        inv.font_name = "CMSY8".into();
        let mut eq = wide(3, 100.0, 745.0, "\u{39b}t = Id +", 60.0, 10.9);
        eq.font_name = "CMMI10".into();
        let segs = segment_glyphs(&[head, top, inv, eq]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts.iter().any(|t| t.contains("Information Selection")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_prose_line_with_a_times_sign_joins_the_paragraph() {
        let mut above = wide(
            0,
            72.0,
            500.0,
            "We implement this as LESSER, a wrapper that reduces",
            400.0,
            10.0,
        );
        above.font_name = "NimbusRomNo9L-Regu".into();
        let mut words = wide(
            1,
            72.0,
            488.0,
            "the feature-extraction FLOP cost by 9.7",
            250.0,
            10.0,
        );
        words.font_name = "NimbusRomNo9L-Regu".into();
        let mut times = wide(2, 320.0, 488.0, "×", 8.0, 10.0);
        times.font_name = "CMSY10".into();
        let mut tail = wide(
            3,
            330.0,
            488.0,
            "for SFT and for RL benchmarks,",
            120.0,
            10.0,
        );
        tail.font_name = "NimbusRomNo9L-Regu".into();
        let mut below = wide(
            4,
            72.0,
            476.0,
            "while tracking full-gradient performance on downstream tasks.",
            380.0,
            10.0,
        );
        below.font_name = "NimbusRomNo9L-Regu".into();
        let seg = segment_with(
            &[above, words, times, tail, below],
            &SegmentFlags::default(),
        );
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("LESSER")
                && text.contains('×')
                && text.contains("downstream")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_short_last_line_above_a_figure_stays_body_text() {
        let body =
            "This sentence is long enough to count as ordinary body text in the column today.";
        let last = "ently, they select batches with aligned gradients.";
        let glyphs = vec![
            wide(0, 72.0, 700.0, body, 360.0, 10.0),
            wide(1, 72.0, 688.0, last, 280.0, 10.0),
            wide(2, 90.0, 640.0, "USER PROMPT", 55.0, 9.0),
            wide(
                3,
                72.0,
                560.0,
                "Figure 1: A diagram of the system and its parts.",
                360.0,
                10.0,
            ),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("aligned gradients")),
            "{texts:?} kept={:?}",
            seg.kept
        );
        assert!(
            !seg.kept.iter().any(|(id, _)| *id == 1),
            "last body line was claimed as a figure: {:?}",
            seg.kept
        );
        assert!(seg
            .kept
            .iter()
            .any(|(id, reason)| *id == 2 && reason == "figure"));
    }

    #[test]
    fn a_word_above_a_tiny_image_is_not_figure_ink() {
        let mut word = glyph(0, 70.87, 96.15, "Below", false);
        word.font_size = 10.91;
        word.bbox = [70.87, 93.0, 96.0, 107.0];
        word.advance = [25.0, 0.0];
        let line = visual_line(vec![&word]);
        let icon = Rect::new(70.87, 80.61, 81.77, 91.52);
        assert!(
            !line_in_figure(&line, &[icon], "Nimbus"),
            "a lead-in over an 11pt icon was kept as a figure"
        );
        let drawing = Rect::new(70.0, 40.0, 280.0, 90.0);
        assert!(
            line_in_figure(&line, &[drawing], "Nimbus"),
            "a short label above a real drawing was translated"
        );
    }

    #[test]
    fn a_panel_title_just_under_the_frame_stays_in_the_figure() {
        let title = wide(
            0,
            328.0,
            376.0,
            "(d) Following vs. ignoring summaries",
            123.0,
            7.5,
        );
        let caption = wide(
            1,
            108.0,
            250.0,
            "Figure 3: Pass rate versus inference budget.",
            360.0,
            9.0,
        );
        let regions = [
            box_region(108.0, 272.0, 504.0, 502.0),
            box_region(328.0, 300.0, 474.0, 371.0),
            box_region(330.0, 302.0, 472.0, 369.0),
        ];
        let seg = segment_placed(&[title, caption], &SegmentFlags::default(), &regions, &[]);
        assert!(
            seg.kept
                .iter()
                .any(|(id, reason)| *id == 0 && reason == "figure"),
            "panel title was translated: {:?}",
            seg.kept
        );
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("Figure 3")),
            "the caption was kept with the panel: {texts:?}"
        );
        assert!(
            texts.iter().all(|text| !text.contains("Following")),
            "figure text was segmented: {texts:?}"
        );
    }

    #[test]
    fn a_justified_word_gap_stays_one_segment() {
        let mut glyphs = Vec::new();
        for index in 0..12 {
            let x = 72.0 + index as f32 * 14.0;
            let mut item = glyph(index, x, 500.0, "w", false);
            item.font_size = 10.0;
            item.bbox = [x, 500.0, x + 6.0, 510.0];
            glyphs.push(item);
        }
        let segs = segment_glyphs(&glyphs);
        assert_eq!(
            segs.len(),
            1,
            "{:?}",
            segs.iter().map(|s| s.text.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_widely_justified_narrow_line_stays_one_segment() {
        let mut glyphs = Vec::new();
        let mut x = 316.6f32;
        let mut id = 0;
        for word in ["All", "three", "attacks", "succeeded", "on", "gemma-4-12b,"] {
            for ch in word.chars() {
                let mut item = glyph(id, x, 600.0, &ch.to_string(), false);
                item.font_size = 10.0;
                item.bbox = [x, 600.0, x + 4.5, 610.0];
                glyphs.push(item);
                x += 4.6;
                id += 1;
            }
            x += 14.0;
        }
        let segs = segment_glyphs(&glyphs);
        assert_eq!(
            segs.len(),
            1,
            "{:?}",
            segs.iter().map(|s| s.text.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_url_bullet_does_not_join_the_next_heading() {
        let mut url = glyph(
            0,
            108.0,
            80.0,
            "\u{f0a1} https://github.com/dair-ai/Prompt-Engineering-Guide",
            false,
        );
        url.page_index = 0;
        let mut summary = glyph(1, 108.0, 560.0, "Summary", false);
        summary.page_index = 1;
        let segs = segment_glyphs(&[url, summary]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("github.com") && !text.contains("Summary")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|text| text.trim() == "Summary"),
            "{texts:?}"
        );
    }

    #[test]
    fn a_page_break_does_not_cut_the_sentence() {
        let mut tail = glyph(0, 72.0, 80.0, "we run the base agent on", false);
        tail.page_index = 0;
        let mut caption = glyph(
            1,
            72.0,
            700.0,
            "Figure 1: length triggered compaction versus AutoCompact.",
            false,
        );
        caption.page_index = 1;
        let mut next = glyph(
            2,
            72.0,
            400.0,
            "training tasks and use a judge to review it.",
            false,
        );
        next.page_index = 1;
        let segs = segment_glyphs(&[tail, caption, next]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("base agent on training tasks")),
            "{texts:?}"
        );
        assert!(!texts.iter().any(|text| text.ends_with(" on")), "{texts:?}");
        assert!(
            texts.iter().any(|text| text.starts_with("Figure")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_callout_sentence_joins_across_a_running_head() {
        // The gutter index `3.1.1` is further left than the body, and the
        // running head sits above `word.`. Neither is the rest of the sentence.
        let mut tail = glyph(
            0,
            105.0,
            80.0,
            "where one token roughly corresponds to one",
            false,
        );
        tail.page_index = 0;
        let mut index = glyph(1, 66.0, 500.0, "3.1.1", false);
        index.page_index = 1;
        let mut head = glyph(
            2,
            157.0,
            700.0,
            "3.1 Summarizing a document bigger than the context window",
            false,
        );
        head.page_index = 1;
        let mut next = glyph(
            3,
            114.0,
            640.0,
            "word. For example the model continues the sentence here.",
            false,
        );
        next.page_index = 1;
        let segs = segment_glyphs(&[tail, index, head, next]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("corresponds to one word. For example")),
            "{texts:?}"
        );
        assert!(texts.iter().any(|text| text.trim() == "3.1.1"), "{texts:?}");
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Summarizing") && !text.contains("word. For example")),
            "{texts:?}"
        );
    }

    #[test]
    fn an_indented_comma_paragraph_is_not_a_centered_list() {
        // A first-line indent of about one em, four commas, and a shared right
        // edge used to look like a centered topic list. The next page then
        // could not finish `hands-on experience`.
        let rows = [
            (
                0u32,
                0u32,
                84.0,
                120.0,
                316.0,
                "In this book, we'll focus on LangGraph, LangChain's framework.",
            ),
            (
                1,
                0,
                72.0,
                106.0,
                328.0,
                "LangGraph provides classes, along with ready-to-use tools, so",
            ),
            (
                2,
                0,
                72.0,
                92.0,
                328.0,
                "you don't have to reinvent the wheel or gain hands-on",
            ),
            (
                3,
                1,
                72.0,
                700.0,
                328.0,
                "experience building advanced agents in practice.",
            ),
        ];
        let mut glyphs = Vec::new();
        for (id, page, x, y, width, text) in rows {
            let mut item = glyph(id, x, y, text, false);
            item.page_index = page;
            item.font_size = 10.0;
            item.matrix = [10.0, 0.0, 0.0, 10.0, x, y];
            item.bbox = [x, y, x + width, y + 10.0];
            item.advance = [width, 0.0];
            glyphs.push(item);
        }
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .into_iter()
            .map(|seg| seg.text)
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("hands-on experience building")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_hyphen_joins_when_the_next_line_opens_with_a_quote() {
        // The prompt mark is 22pt and sits on the wrap. The body median stays
        // 9.5pt, so the broken word is still the next line.
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        let paint =
            |glyphs: &mut Vec<Glyph>, id: &mut u32, x: f32, y: f32, size: f32, text: &str| {
                let mut cursor = x;
                for ch in text.chars() {
                    let width = if ch == ' ' { size * 0.3 } else { size * 0.5 };
                    let mut item = glyph(*id, cursor, y, &ch.to_string(), false);
                    *id += 1;
                    item.font_size = size;
                    item.matrix = [size, 0.0, 0.0, size, cursor, y];
                    item.bbox = [cursor, y, cursor + width, y + size];
                    item.advance = [width, 0.0];
                    cursor += width;
                    glyphs.push(item);
                }
            };
        paint(
            &mut glyphs,
            &mut id,
            117.2,
            157.9,
            9.48,
            "I apologize for the previous oversight. Let's sum the palin-",
        );
        paint(&mut glyphs, &mut id, 102.2, 146.5, 21.96, "“");
        paint(
            &mut glyphs,
            &mut id,
            117.2,
            146.5,
            9.48,
            "dromes in this sequence: 13, 1331, and 7.",
        );
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .into_iter()
            .map(|seg| seg.text)
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("palin-") && text.contains("dromes")),
            "{texts:?}"
        );
        let path =
            std::path::Path::new("/opt/cursor/artifacts/pdfs/manning-body-p31-60-source.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(path).unwrap();
        let extraction = doc.extract();
        let placed = segment_placed(
            &extraction.glyphs,
            &SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let hit = placed
            .segments
            .iter()
            .find(|item| item.text.contains("apologize"))
            .expect("apology");
        assert!(
            hit.text.contains("palin-") && hit.text.contains("dromes in this sequence"),
            "{}",
            hit.text
        );
    }

    #[test]
    fn manning_hands_on_continues_on_the_next_page() {
        let path = std::path::Path::new("/opt/cursor/artifacts/pdfs/manning-body-p1-30-source.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(path).unwrap();
        let extraction = doc.extract_with(&crate::extract::ExtractOptions {
            max_pages: Some(12),
            ..crate::extract::ExtractOptions::default()
        });
        let seg = segment_placed(
            &extraction.glyphs,
            &SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let hit = seg
            .segments
            .iter()
            .find(|item| item.text.contains("hands-on"))
            .expect("hands-on sentence");
        assert!(
            hit.text.contains("experience building"),
            "page break cut the sentence: {}",
            hit.text
        );
    }

    #[test]
    fn a_two_column_prose_line_stays_out_of_the_notation_column() {
        let path = std::path::Path::new("/tmp/enduser/2610.20248.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(path).unwrap();
        let extraction = doc.extract();
        let placed = segment_placed(
            &extraction.glyphs,
            &SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let merged = crate::rewrite::merge_stacked_paragraphs(placed.segments, &extraction.glyphs);
        let hit = merged
            .iter()
            .find(|item| item.text.contains("Despite these advances"))
            .expect("intro paragraph");
        assert!(
            !hit.text.contains("real numbers"),
            "notation column joined the prose:\n{}",
            hit.text.chars().take(400).collect::<String>()
        );
        assert!(
            hit.text.contains("tractable design procedure"),
            "{}",
            hit.text.chars().take(200).collect::<String>()
        );
        let crosses = |needle: &str, foreign: &str| {
            merged
                .iter()
                .any(|item| item.text.contains(needle) && item.text.contains(foreign))
        };
        assert!(
            !crosses("novel methodology that resolves", "Conceptual figure"),
            "page 6 caption joined the left column"
        );
        assert!(
            !crosses("piecewise convexity", "robustly stable"),
            "page 7 proof joined the left column"
        );
        assert!(
            !crosses("Following this construction", "unstructured dynamic"),
            "page 9 uncertainty set joined the algorithm column"
        );
    }

    #[test]
    fn the_manning_definition_is_one_paragraph_across_the_page() {
        let path =
            std::path::Path::new("/opt/cursor/artifacts/pdfs/manning-body-p31-60-source.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(path).unwrap();
        let extraction = doc.extract();
        let segs = segment_glyphs(&extraction.glyphs);
        assert!(
            segs.iter().any(|seg| {
                seg.text.contains("corresponds to one") && seg.text.contains("word. For example")
            }),
            "definition callout stayed split at the page break"
        );
        assert!(segs.iter().any(|seg| {
            seg.text.contains("3.1 Summarizing a document bigger")
                && !seg.text.contains("word. For example")
        }));
        assert!(
            segs.iter().any(|seg| {
                seg.text.contains("as I’ll") && seg.text.contains("explain in the next section")
            }),
            "a sentence that ends on I’ll stayed split at the page break: {}",
            segs.iter()
                .filter(|seg| {
                    seg.text.contains("I’ll") || seg.text.contains("explain in the next")
                })
                .map(|seg| seg.text.chars().take(180).collect::<String>())
                .collect::<Vec<_>>()
                .join(" || ")
        );
    }

    #[test]
    fn a_finished_sentence_does_not_cross_a_heading() {
        let mut tail = glyph(0, 105.0, 80.0, "This paragraph is already finished.", false);
        tail.page_index = 0;
        let mut head = glyph(
            1,
            105.0,
            700.0,
            "3.1 Summarizing a document bigger than the context window",
            false,
        );
        head.page_index = 1;
        let mut next = glyph(
            2,
            114.0,
            640.0,
            "word. For example this starts a new sentence.",
            false,
        );
        next.page_index = 1;
        let segs = segment_glyphs(&[tail, head, next]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.ends_with("finished.")),
            "{texts:?}"
        );
        assert!(
            !texts
                .iter()
                .any(|text| text.contains("finished") && text.contains("word. For example")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_column_break_does_not_cut_the_sentence_or_skip_to_the_next_page() {
        let left = glyph(0, 72.0, 80.0, "we run the base agent on", false);
        let right = glyph(
            1,
            320.0,
            700.0,
            "training tasks and the judge reviews the trace.",
            false,
        );
        let mut next_page = glyph(
            2,
            72.0,
            700.0,
            "another page should stay its own paragraph.",
            false,
        );
        next_page.page_index = 1;
        let segs = segment_glyphs(&[left, right, next_page]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("base agent on training tasks")),
            "{texts:?}"
        );
        assert!(
            !texts
                .iter()
                .any(|text| text.contains("base agent on") && text.contains("another page")),
            "column bottom must not jump over the next column: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("another page should stay")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_table_cell_is_not_the_next_column() {
        let body = glyph(
            0,
            55.0,
            140.0,
            "we evaluate the method on this task and then continue",
            false,
        );
        let cell = glyph(1, 127.0, 638.0, "t [s]", false);
        let right = glyph(
            2,
            310.0,
            700.0,
            "the right column continues the sentence from the left.",
            false,
        );
        let segs = segment_glyphs(&[body, cell, right]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| {
                text.contains("we evaluate")
                    && text.contains("right column")
                    && !text.contains("[s]")
            }),
            "{texts:?}"
        );
        assert!(texts.iter().any(|text| text.contains("[s]")), "{texts:?}");
    }

    #[test]
    fn a_footnote_does_not_run_into_the_next_column_body() {
        let mut note = glyph(0, 37.0, 90.0, "jan.gruber@kit.edu (Jan Gruber), and", false);
        note.font_size = 8.0;
        let right = glyph(
            1,
            316.0,
            700.0,
            "this paper makes three contributions to the field of forensics.",
            false,
        );
        let segs = segment_glyphs(&[note, right]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            !texts
                .iter()
                .any(|text| text.contains("Gruber") && text.contains("contributions")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_finished_column_does_not_swallow_the_next_column() {
        let left = glyph(0, 72.0, 80.0, "This paragraph is finished.", false);
        let right = glyph(
            1,
            320.0,
            700.0,
            "The next column starts a new paragraph with its own sentence.",
            false,
        );
        let segs = segment_glyphs(&[left, right]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert_eq!(texts.len(), 2, "{texts:?}");
        assert!(texts.iter().any(|text| text.ends_with("finished.")));
        assert!(texts.iter().any(|text| text.starts_with("The next column")));
    }

    #[test]
    fn a_colon_above_a_heading_does_not_jump_to_the_middle_of_the_next_column() {
        let lines = [
            (
                63.8,
                400.0,
                "Faster convergence comes from informed search. The guidance should enable",
            ),
            (
                53.8,
                389.0,
                "more efficient exploration compared to search with polluted observations and",
            ),
            (53.8, 378.0, "bloated context:"),
            (53.8, 320.0, "4.4 Empirical Support and Scope"),
            (
                53.8,
                308.0,
                "Our experimental results in the next section are consistent with these",
            ),
            (
                53.8,
                297.0,
                "predictions, and the accuracy patterns across tasks match the expectation.",
            ),
            (
                318.0,
                700.0,
                "in sequence, while slow thinking enforces careful step by step planning for",
            ),
            (
                318.0,
                689.0,
                "operations that are expensive. The details can be found in Appendix A.",
            ),
            (
                327.9,
                611.0,
                "Throwback. The throwback mechanism represents a crucial innovation in the",
            ),
            (
                318.0,
                600.0,
                "evolution control, extending beyond simple temporal rollback functionality.",
            ),
        ];
        let glyphs: Vec<Glyph> = lines
            .iter()
            .enumerate()
            .map(|(id, (x, y, text))| glyph(id as u32, *x, *y, text, false))
            .collect();
        let segs = segment_glyphs(&glyphs);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        let colon = texts
            .iter()
            .find(|text| text.contains("bloated context:"))
            .expect("colon paragraph");
        assert!(!colon.contains("Throwback"), "{texts:?}");
    }

    #[test]
    fn a_page_break_hyphen_is_joined() {
        let mut upper = glyph(0, 72.0, 70.0, "rules were spec-", false);
        upper.page_index = 0;
        let mut lower = glyph(
            1,
            72.0,
            700.0,
            "ified in the prompt and the model continued.",
            false,
        );
        lower.page_index = 1;
        let segs = segment_glyphs(&[upper, lower]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("specified")),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("spec-")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_monospace_phrase_does_not_split_the_sentence_or_swallow_a_hash() {
        let mut generated = glyph(0, 72.0, 500.0, "generated", false);
        let mut hash = glyph(1, 82.0, 500.0, "#", false);
        let mut auto = glyph(2, 92.0, 500.0, "Auto", false);
        let mut context = glyph(3, 72.0, 486.0, "Context Summary ", false);
        context.font_name = "NimbusMonL-Regu".into();
        let mut rest = glyph(4, 80.0, 486.0, "accurately preserves execution, in-", false);
        let mut cont = glyph(
            5,
            72.0,
            472.0,
            "cluding the information needed after compaction.",
            false,
        );
        generated.font_size = 10.0;
        hash.font_size = 10.0;
        auto.font_size = 10.0;
        context.font_size = 10.0;
        rest.font_size = 10.0;
        cont.font_size = 10.0;
        let segs = segment_glyphs(&[generated, hash, auto, context, rest, cont]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        let joined = texts.join("\n");
        assert!(
            joined.contains("generated # Auto"),
            "hash should keep its word space: {texts:?}"
        );
        assert!(
            joined.contains("including"),
            "hyphen should join across the monospace span: {texts:?}"
        );
        assert!(!joined.contains("in-"), "{texts:?}");
    }

    #[test]
    fn a_right_aligned_chapter_title_is_one_segment() {
        let upper = wide(0, 261.0, 524.0, "Introduction to AI", 213.0, 30.0);
        let lower = wide(1, 196.0, 492.0, "agents and applications", 278.0, 30.0);
        let texts: Vec<_> = segment_glyphs(&[upper, lower])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(
            texts[0].contains("Introduction to AI") && texts[0].contains("agents and applications"),
            "{texts:?}"
        );
    }

    #[test]
    fn a_display_chapter_title_is_not_a_figure_label() {
        let title = wide(0, 180.0, 524.0, "Introduction to AI", 260.0, 30.0);
        let label = wide(1, 420.0, 500.0, "LLM", 28.0, 9.0);
        let regions = [
            crate::glyph::PaintedRegion {
                page_index: 0,
                bbox: [178.0, 481.0, 474.0, 481.3],
                kind: "path".into(),
            },
            crate::glyph::PaintedRegion {
                page_index: 0,
                bbox: [395.0, 448.0, 478.0, 600.0],
                kind: "path".into(),
            },
        ];
        let seg = segment_placed(&[title, label], &SegmentFlags::default(), &regions, &[]);
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("Introduction")),
            "{texts:?} kept={:?}",
            seg.kept
        );
        assert!(
            seg.kept
                .iter()
                .any(|(id, reason)| *id == 1 && reason == "figure"),
            "{:?}",
            seg.kept
        );
        assert!(
            !seg.kept.iter().any(|(id, _)| *id == 0),
            "title was kept: {:?}",
            seg.kept
        );
    }

    #[test]
    fn a_diagram_gutter_callout_stays_in_the_figure() {
        let mut callout = wide(0, 110.0, 320.0, "Document loaders parse text", 90.0, 9.0);
        callout.font_name = "HumanistMann521-BoldCondensed".into();
        let mut body = wide(
            1,
            72.0,
            520.0,
            "This sentence is long enough to count as ordinary body text in the column today.",
            360.0,
            10.0,
        );
        body.font_name = "NewBaskerville-Roman".into();
        let mut aside = wide(2, 120.0, 300.0, "A short aside in the column.", 140.0, 10.0);
        aside.font_name = "NewBaskerville-Roman".into();
        // Two strokes per box, far enough apart that the columns do not union.
        // The callout sits in the gutter between them.
        let regions = [
            box_region(40.0, 400.0, 90.0, 430.0),
            box_region(42.0, 402.0, 88.0, 428.0),
            box_region(220.0, 400.0, 270.0, 430.0),
            box_region(222.0, 402.0, 268.0, 428.0),
            box_region(40.0, 200.0, 90.0, 230.0),
            box_region(42.0, 202.0, 88.0, 228.0),
            box_region(220.0, 200.0, 270.0, 230.0),
            box_region(222.0, 202.0, 268.0, 228.0),
        ];
        let seg = segment_placed(
            &[callout, body, aside],
            &SegmentFlags::default(),
            &regions,
            &[],
        );
        assert!(
            seg.kept
                .iter()
                .any(|(id, reason)| *id == 0 && reason == "figure"),
            "callout was translated: {:?}",
            seg.kept
        );
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("ordinary body")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|text| text.contains("short aside")),
            "column text inside the span was kept: {texts:?} {:?}",
            seg.kept
        );
    }

    #[test]
    fn a_chapter_running_header_above_a_figure_is_translated() {
        let mut header = wide(
            0,
            80.0,
            640.0,
            "CHAPTER 1 Introduction to AI agents and applications",
            340.0,
            9.0,
        );
        header.font_name = "FranklinGothic-Demi".into();
        let mut label = wide(1, 120.0, 560.0, "Agent", 40.0, 8.0);
        label.font_name = "ArialMT".into();
        let mut caption = wide(
            2,
            72.0,
            480.0,
            "Figure 1.5 A holiday-planning agent.",
            280.0,
            9.0,
        );
        caption.font_name = "FranklinGothic-Demi".into();
        let mut body = wide(
            3,
            72.0,
            700.0,
            "This sentence is long enough to count as ordinary body text in the column today.",
            360.0,
            10.0,
        );
        body.font_name = "NewBaskerville-Roman".into();
        let seg = segment_with(&[body, header, label, caption], &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("CHAPTER 1")),
            "{texts:?} kept={:?}",
            seg.kept
        );
        assert!(
            seg.kept
                .iter()
                .any(|(id, reason)| *id == 1 && reason == "figure"),
            "{:?}",
            seg.kept
        );
    }

    #[test]
    fn a_monospace_listing_stays_original_and_the_prose_around_it_does_not() {
        let before = glyph(
            0,
            72.0,
            500.0,
            "Enter the following command in the terminal:",
            false,
        );
        let mut listing = glyph(1, 72.0, 484.0, "print(response)", false);
        listing.font_name = "JetBrainsMonoNL".into();
        let after = glyph(
            2,
            72.0,
            468.0,
            "The notebook then shows the model reply.",
            false,
        );
        let seg = segment_with(&[before, listing, after], &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().all(|text| !text.contains("print")),
            "{texts:?}"
        );
        assert!(texts.iter().any(|text| text.contains("terminal")));
        assert!(texts.iter().any(|text| text.contains("notebook")));
        assert!(
            seg.kept
                .iter()
                .any(|(id, reason)| *id == 1 && reason == "code"),
            "{:?}",
            seg.kept
        );
    }

    #[test]
    fn a_monospace_license_block_is_prose_and_a_listing_stays_code() {
        let license = [
            "Project Gutenberg's Alice's Adventures in Wonderland, by Lewis Carroll.",
            "This eBook is for the use of anyone anywhere at no cost and with",
            "almost no restrictions whatsoever. You may copy it, give it away or",
        ];
        let mut glyphs = Vec::new();
        for (index, line) in license.iter().enumerate() {
            let mut item = wide(
                index as u32,
                72.0,
                700.0 - index as f32 * 14.0,
                line,
                460.0,
                11.0,
            );
            item.font_name = "Courier-Oblique".into();
            glyphs.push(item);
        }
        let mut listing = wide(10, 72.0, 640.0, "print(response)", 140.0, 11.0);
        listing.font_name = "Courier".into();
        glyphs.push(listing);
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.contains("anyone anywhere")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|text| text.contains("Project Gutenberg")),
            "{texts:?}"
        );
        assert!(
            texts.iter().all(|text| !text.contains("print")),
            "{texts:?}"
        );
        assert!(
            seg.kept
                .iter()
                .any(|(id, reason)| *id == 10 && reason == "code"),
            "{:?}",
            seg.kept
        );
        assert!(
            seg.kept.iter().all(|(id, _)| *id == 10),
            "license glyphs were kept as code: {:?}",
            seg.kept
        );
    }

    #[test]
    fn the_sample_paper_does_not_cut_a_sentence_at_the_page_bottom() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/ci/arxiv-2610.02163.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(&path).unwrap();
        let extraction = doc.extract_with(&crate::extract::ExtractOptions {
            max_pages: Some(4),
            ..crate::extract::ExtractOptions::default()
        });
        let seg = segment_with(&extraction.glyphs, &SegmentFlags::default());
        let intro = seg.segments.iter().find(|item| {
            item.text
                .contains("what to preserve, and how to continue as part of its policy")
        });
        let intro = intro.expect("intro paragraph");
        assert!(
            intro.text.contains("training tasks"),
            "page 1 stopped mid-sentence: {}",
            intro.text.chars().rev().take(120).collect::<String>()
        );
        assert!(
            seg.segments
                .iter()
                .any(|item| item.text.contains("specified")),
            "spec- was left at the page break"
        );
        assert!(
            seg.segments
                .iter()
                .any(|item| item.text.contains("including")),
            "in- was left in the working-state sentence"
        );
        assert!(seg.segments.iter().any(|item| {
            item.text.contains("generated # Auto") || item.text.contains("by # Auto")
        }));
        let intro_pages: crate::detmap::HashSet<u32> = intro
            .glyph_ids
            .iter()
            .filter_map(|id| {
                extraction
                    .glyphs
                    .iter()
                    .find(|glyph| glyph.id == *id)
                    .map(|glyph| glyph.page_index)
            })
            .collect();
        assert!(
            intro_pages.len() > 1,
            "intro should cross the page break, pages {intro_pages:?}"
        );
        for item in &seg.segments {
            let tail = item.text.trim_end();
            assert!(
                soft_hyphen_stem(tail).is_none(),
                "soft hyphen at a page or column boundary p{}: {tail}",
                item.page_index
            );
            assert!(
                !tail.ends_with(" on"),
                "mid-sentence segment p{}: {tail}",
                item.page_index
            );
        }
    }

    #[test]
    fn a_hyphen_still_joins_when_the_next_line_carries_larger_math() {
        // Inline math on the continuation lifts the line size past the 20%
        // body tolerance. The broken word is still the next line, not the
        // lowercase sentence at the top of the next page.
        let upper = block(
            0,
            70.0,
            548.0,
            450.0,
            10.0,
            "matrices that map queries to a relational em-",
        );
        let lower = block(
            1,
            70.0,
            534.0,
            450.0,
            12.6,
            "bedding space, and the head dimension follows.",
        );
        let mut jumped = block(
            2,
            70.0,
            750.0,
            420.0,
            10.0,
            "operates on pairs after the page break.",
        );
        jumped.page_index = 1;
        let texts: Vec<_> = segment_glyphs(&[upper, lower, jumped])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("embedding space")),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("emoperates")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_section_index_across_a_justified_gutter_stays_in_its_column() {
        // Measured on a two-column conference page: body size 10.9, justified
        // space 10.9, gutter 17.6 before `2.5 Data` (46.9pt wide).
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        let mut x = 72.0f32;
        let left = "with L layers. Each block uses A self-attention";
        let wide_at = left.find(" uses ").unwrap();
        for (index, ch) in left.chars().enumerate() {
            let advance: f32 = if index == wide_at { 15.3 } else { 4.5 };
            glyphs.push(block(id, x, 276.0, 4.4, 10.9, &ch.to_string()));
            x += advance;
            id += 1;
        }
        let gutter = x + 17.6;
        x = gutter;
        for ch in "2.5 Data".chars() {
            let advance: f32 = if ch == ' ' { 11.3 } else { 5.0 };
            glyphs.push(block(id, x, 276.4, advance, 10.9, &ch.to_string()));
            x += advance;
            id += 1;
        }
        glyphs.push(block(
            id,
            72.0,
            262.0,
            140.0,
            10.9,
            "heads and hidden dimension H.",
        ));
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .all(|text| !(text.contains("self-attention") && text.contains("2.5"))),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("2.5") && text.contains("Data")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|text| text.contains("heads and hidden")),
            "{texts:?}"
        );
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

    fn block(id: u32, x: f32, y: f32, w: f32, size: f32, text: &str) -> Glyph {
        let mut item = glyph(id, x, y, text, false);
        item.font_size = size;
        item.matrix = [size, 0.0, 0.0, size, x, y];
        item.bbox = [x, y, x + w, y + size];
        item
    }

    #[test]
    fn subscript_digits_on_one_baseline_are_not_page_numbers() {
        // `x_0` and `x_1` put `0` and `1` on the subscript baseline, an em
        // apart. That gap used to mark `1` as a contents page number.
        let glyphs = vec![
            block(0, 417.0, 449.0, 4.0, 7.0, "0"),
            block(1, 432.0, 449.0, 4.0, 7.0, "1"),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        assert!(
            seg.kept
                .iter()
                .any(|(id, reason)| *id == 1 && reason == "formula"),
            "{:?}",
            seg.kept
        );
        assert!(
            !seg.kept.iter().any(|(_, reason)| reason == "toc"),
            "{:?}",
            seg.kept
        );
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.contains(&"0"),
            "the other subscript stays its own token: {texts:?}"
        );
        assert!(
            !seg.segments.iter().any(|item| item.glyph_ids.contains(&1)),
            "the page-number gap must not pull the subscript into a title"
        );
    }

    #[test]
    fn a_contents_entry_does_not_merge_with_the_next_page_number() {
        let glyphs = vec![
            block(
                0,
                153.0,
                378.0,
                220.0,
                10.0,
                "Introduction to AI agents and applications 3",
            ),
            block(
                1,
                153.0,
                364.0,
                210.0,
                10.0,
                "Executing prompts programmatically 27",
            ),
            block(
                2,
                133.0,
                604.0,
                280.0,
                10.0,
                "4.4 Enhancing the architecture 76",
            ),
            block(3, 133.0, 590.0, 220.0, 10.0, "4.5 Prompt engineering 78"),
        ];
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text == "Introduction to AI agents and applications 3"),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text == "Executing prompts programmatically 27"),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text == "4.4 Enhancing the architecture 76"),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|text| text == "4.5 Prompt engineering 78"),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains(" 3 Executing")),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("76 4.5")),
            "{texts:?}"
        );
    }

    #[test]
    fn several_contents_entries_on_one_baseline_keep_their_page_numbers() {
        // `engines4 LLM-based chatbots7 AI agents8` and a wrapped `model15`.
        let glyphs = vec![
            block(0, 177.0, 248.0, 33.0, 10.0, "engines"),
            block(1, 215.0, 248.0, 6.0, 10.0, "4"),
            block(2, 235.0, 248.0, 78.0, 10.0, "LLM-based chatbots"),
            block(3, 318.0, 248.0, 6.0, 10.0, "7"),
            block(4, 338.0, 248.0, 36.0, 10.0, "AI agents"),
            block(5, 379.0, 248.0, 6.0, 10.0, "8"),
            block(6, 177.0, 203.0, 40.0, 10.0, "model"),
            block(7, 222.0, 203.0, 12.0, 10.0, "15"),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.clone()).collect();
        assert!(texts.iter().any(|text| text == "engines"), "{texts:?}");
        assert!(
            texts.iter().any(|text| text == "LLM-based chatbots"),
            "{texts:?}"
        );
        assert!(texts.iter().any(|text| text == "AI agents"), "{texts:?}");
        assert!(texts.iter().any(|text| text == "model"), "{texts:?}");
        assert!(
            texts
                .iter()
                .all(|text| !text.chars().any(|ch| ch.is_ascii_digit())),
            "{texts:?}"
        );
        let kept: Vec<u32> = seg
            .kept
            .iter()
            .filter(|(_, reason)| reason == "toc")
            .map(|(id, _)| *id)
            .collect();
        assert!([1, 3, 5, 7].iter().all(|id| kept.contains(id)), "{kept:?}");
    }

    #[test]
    fn a_normal_word_space_before_a_number_is_not_a_contents_mark() {
        let glyphs = vec![
            block(0, 72.0, 400.0, 70.0, 10.0, "the value is"),
            block(1, 145.0, 400.0, 12.0, 10.0, "92"),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let text = seg
            .segments
            .iter()
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("92"), "{text}");
        assert!(
            !seg.kept.iter().any(|(_, reason)| reason == "toc"),
            "{:?}",
            seg.kept
        );
    }

    #[test]
    fn a_contents_hyphen_that_returns_to_the_left_indent_stays_two_titles() {
        // Joining these paints one block from the right-hand title back to
        // the left indent and smashes the neighboring entries.
        let glyphs = vec![
            block(
                0,
                177.0,
                210.0,
                124.0,
                10.0,
                "Ingestion: Metadata enrichment",
            ),
            block(1, 306.0, 210.0, 18.0, 10.0, "232"),
            block(2, 336.0, 210.0, 110.0, 10.0, "Q&A on a metadata-"),
            block(3, 177.0, 198.0, 70.0, 10.0, "enriched collection"),
            block(4, 252.0, 198.0, 18.0, 10.0, "234"),
            block(
                5,
                186.0,
                515.0,
                150.0,
                10.0,
                "Long-term user and application memory",
            ),
            block(6, 345.0, 515.0, 18.0, 10.0, "346"),
            block(7, 375.0, 515.0, 70.0, 10.0, "Human-in-the-"),
            block(10, 348.0, 509.0, 8.0, 10.0, "■"),
            block(8, 186.0, 503.0, 22.0, 10.0, "loop"),
            block(9, 213.0, 503.0, 18.0, 10.0, "346"),
        ];
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .all(|text| !text.contains("metadata-enriched")
                    && !text.contains("Human-in-the-loop")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|text| text.contains("Q&A on a metadata-")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("enriched collection") && !text.contains("Q&A")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Human-in-the-") && !text.contains("loop")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("loop") && !text.contains("Human")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_body_compound_keeps_hyphens_already_in_the_stem() {
        let glyphs = vec![
            block(0, 72.0, 400.0, 90.0, 10.0, "Human-in-the-"),
            block(1, 72.0, 386.0, 24.0, 10.0, "loop"),
        ];
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("Human-in-the-loop")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_lowercase_contents_wrap_stays_with_the_title_above_it() {
        // `engines` is the rest of the title. `chatbots` is the next entry.
        let glyphs = vec![
            block(
                0,
                177.0,
                400.0,
                203.0,
                10.0,
                "LLM-based applications: Summarization and Q&A",
            ),
            block(1, 177.0, 388.0, 28.0, 10.0, "engines"),
            block(2, 211.0, 388.0, 6.0, 10.0, "4"),
            block(3, 230.0, 388.0, 78.0, 10.0, "LLM-based chatbots"),
            block(4, 313.0, 388.0, 6.0, 10.0, "7"),
        ];
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Summarization") && text.contains("engines")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("chatbots") && !text.contains("engines")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_wrapped_contents_title_still_joins() {
        let glyphs = vec![
            block(0, 153.0, 400.0, 240.0, 10.0, "Executing prompts"),
            block(1, 153.0, 386.0, 220.0, 10.0, "programmatically 27"),
        ];
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .into_iter()
            .map(|seg| seg.text)
            .collect();
        assert_eq!(texts, ["Executing prompts programmatically 27"]);
    }

    #[test]
    fn dotted_section_headings_stay_on_their_own_rows() {
        let glyphs = vec![
            block(
                0,
                72.0,
                400.0,
                235.0,
                10.0,
                "1.1. Notation, and typical periodic optimization",
            ),
            block(
                1,
                72.0,
                386.0,
                220.0,
                10.0,
                "1.2. Sturmian beta-shifts stay separate",
            ),
        ];
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("1.1.") && !text.contains("1.2.")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("1.2.") && !text.contains("1.1.")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_body_tail_above_a_table_is_the_paragraph_not_a_cell() {
        let upper = glyph(0, 70.9, 530.0, "recommendations across", false);
        let tail = glyph(1, 70.9, 517.0, "all test conversations.", false);
        let cell = glyph(2, 70.9, 517.0, "Model", false);
        let line = |g: &Glyph, size: f32| VisualLine {
            glyphs: vec![],
            text: g.unicode.clone(),
            page: 0,
            y: g.matrix[5],
            left: g.matrix[4],
            right: g.matrix[4] + 100.0,
            size,
            font: g.font_name.clone(),
            toc: false,
            sole_on_baseline: true,
        };
        assert!(table_body_wrap(&line(&upper, 10.9), &line(&tail, 10.9)));
        // A smaller cell row, or a capitalised header, stays in the table.
        assert!(!table_body_wrap(&line(&upper, 10.9), &line(&tail, 8.0)));
        assert!(!table_body_wrap(&line(&upper, 10.9), &line(&cell, 10.9)));
        let mut closed = line(&upper, 10.9);
        closed.text = "as shown below.".into();
        assert!(!table_body_wrap(&closed, &line(&tail, 10.9)));
    }

    #[test]
    fn a_numbered_heading_joins_its_hanging_wrap() {
        // arXiv 2504.15476: `2.4 Pipeline: From Domain Signals to CRS` /
        // `Training Data`. The wrap hangs under the title words.
        let glyphs = vec![
            block(0, 70.9, 432.5, 13.0, 10.9, "2.4"),
            block(1, 89.0, 432.5, 3.0, 10.9, " "),
            block(
                2,
                95.0,
                432.5,
                190.0,
                10.9,
                "Pipeline: From Domain Signals to CRS",
            ),
            block(3, 95.0, 419.0, 60.0, 10.9, "Training Data"),
        ];
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert_eq!(texts.len(), 1, "{texts:?}");
    }

    #[test]
    fn a_dotted_heading_still_joins_its_wrap() {
        let glyphs = vec![
            block(0, 72.0, 400.0, 240.0, 10.0, "1.1. Notation, and typical"),
            block(
                1,
                72.0,
                386.0,
                220.0,
                10.0,
                "periodic optimization of the shift",
            ),
        ];
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(texts[0].contains("Notation") && texts[0].contains("periodic"));
    }

    #[test]
    fn spaced_leaders_without_a_page_number_stay_off_the_title() {
        let mut glyphs = vec![block(
            0,
            90.0,
            400.0,
            150.0,
            10.0,
            "A.1 Terminal benchmark results",
        )];
        for index in 0..20u32 {
            glyphs.push(block(
                1 + index,
                250.0 + 8.0 * index as f32,
                400.0,
                2.5,
                10.0,
                ".",
            ));
        }
        let seg = segment_glyphs(&glyphs);
        let texts: Vec<_> = seg.iter().map(|item| item.text.trim()).collect();
        assert_eq!(texts, ["A.1 Terminal benchmark results"], "{texts:?}");
    }

    #[test]
    fn a_sentence_after_a_superscript_hole_joins_its_line() {
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        let mut run = |x: f32, y: f32, text: &str, size: f32, font: &str, out: &mut Vec<Glyph>| {
            let mut x = x;
            for ch in text.chars() {
                let width = if ch == ' ' { size * 0.3 } else { size * 0.5 };
                let mut item = wide(id, x, y, &ch.to_string(), width, size);
                item.font_name = font.into();
                out.push(item);
                id += 1;
                x += width;
            }
            x
        };
        let body = "This sentence is long enough to count as ordinary body text in the column.";
        run(62.0, 200.0, body, 10.0, "Times-Roman", &mut glyphs);
        let x = run(
            62.0,
            186.0,
            "a subset of the pool. Write this subset as ",
            10.0,
            "Times-Roman",
            &mut glyphs,
        );
        let x = run(x, 186.0, "𝑧", 10.0, "LibertinusMath", &mut glyphs);
        let x = run(
            x + 2.0,
            186.0,
            "∈ {0, 1}",
            10.0,
            "LibertinusMath",
            &mut glyphs,
        );
        let split_left = x + 0.1;
        let x = run(x + 0.5, 190.0, "|𝐸|", 7.0, "LibertinusMath", &mut glyphs);
        let split_right = x + 0.5;
        let x = run(x + 1.0, 186.0, ", with ", 10.0, "Times-Roman", &mut glyphs);
        let x = run(x, 186.0, "𝑧 = 1", 10.0, "LibertinusMath", &mut glyphs);
        let x = run(
            x + 2.0,
            186.0,
            " when edit ",
            10.0,
            "Times-Roman",
            &mut glyphs,
        );
        run(x, 186.0, "𝑗", 10.0, "LibertinusMath", &mut glyphs);
        run(62.0, 172.0, body, 10.0, "Times-Roman", &mut glyphs);
        let refs: Vec<&Glyph> = glyphs.iter().collect();
        let on = |y: f32, from: f32, to: f32| {
            visual_line(
                refs.iter()
                    .copied()
                    .filter(|g| g.matrix[5] == y && g.matrix[4] >= from && g.matrix[4] < to)
                    .collect(),
            )
        };
        let lines = vec![
            on(200.0, 0.0, 999.0),
            on(186.0, 0.0, split_left),
            on(190.0, 0.0, 999.0),
            on(186.0, split_right, 999.0),
            on(172.0, 0.0, 999.0),
        ];
        assert_eq!(
            next_formula_shard(&lines, 1),
            Some(3),
            "the sentence after the script split off: {}",
            lines[3].text
        );
    }

    #[test]
    fn a_contact_list_stays_original() {
        assert!(contact_list(
            "Email addresses: frank.b@uni-a.de (Frank Breitinger), jan.gruber@kit.edu (Jan Gruber), c.h@cs.ox.ac.uk (Christopher Hargreaves)"
        ));
        assert!(contact_list(
            "{alice, bob}@lab.org carol@lab.org dave@lab.org"
        ));
        assert!(!contact_list(
            "Write to alice@lab.org or bob@lab.org if the released code does not build on your machine."
        ));
        assert!(!contact_list("Correspondence: alice@lab.org"));
    }

    #[test]
    fn a_contents_row_keeps_its_leaders_and_page_number() {
        let glyphs = vec![
            block(
                0,
                90.0,
                400.0,
                170.0,
                10.0,
                "PART 1 GETTING STARTED WITH LLMS",
            ),
            block(1, 272.0, 400.0, 140.0, 10.0, "...................."),
            block(2, 424.0, 400.0, 12.0, 10.0, "1"),
            block(
                3,
                153.0,
                370.0,
                200.0,
                10.0,
                "Introduction to AI agents and applications",
            ),
            block(4, 365.0, 370.0, 12.0, 10.0, "3"),
            block(
                5,
                153.0,
                356.0,
                190.0,
                10.0,
                "Executing prompts programmatically",
            ),
            block(6, 355.0, 356.0, 16.0, 10.0, "27"),
            block(
                7,
                113.0,
                200.0,
                170.0,
                10.0,
                "appendix A Trying out LangChain",
            ),
            block(8, 295.0, 200.0, 18.0, 10.0, "351"),
            block(
                9,
                113.0,
                186.0,
                200.0,
                10.0,
                "appendix B Setting up a Jupyter Notebook environment",
            ),
            block(10, 325.0, 186.0, 18.0, 10.0, "357"),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.clone()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("GETTING STARTED") && !text.contains('.')),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Introduction to AI") && !text.contains('3')),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|text| text.contains("Executing prompts")
                && !text.contains("Introduction")
                && !text.contains("27")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("appendix A") && !text.contains("351")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("appendix B") && !text.contains("appendix A")),
            "{texts:?}"
        );
        let glued = segment_with(
            &[
                block(
                    0,
                    90.0,
                    400.0,
                    170.0,
                    10.0,
                    "PART 1 GETTING STARTED WITH LLMS",
                ),
                block(1, 272.0, 400.0, 140.0, 10.0, "...................."),
                block(2, 414.0, 400.0, 8.0, 10.0, "1"),
            ],
            &SegmentFlags::default(),
        );
        let glued_text: Vec<_> = glued
            .segments
            .iter()
            .map(|item| item.text.clone())
            .collect();
        assert!(
            glued_text
                .iter()
                .any(|text| text.contains("GETTING STARTED")
                    && !text.contains('.')
                    && !text.ends_with('1')),
            "{glued_text:?}"
        );
        assert!(glued.kept.iter().any(|(id, _)| *id == 1));
        assert!(glued.kept.iter().any(|(id, _)| *id == 2));
        let kept: Vec<_> = seg.kept.iter().map(|(id, _)| *id).collect();
        for id in [1, 2, 4, 6, 8, 10] {
            assert!(
                kept.contains(&id),
                "page or leaders {id} stayed in a segment: {texts:?} {kept:?}"
            );
        }
    }

    #[test]
    fn an_indented_paragraph_without_a_gap_stays_separate() {
        let glyphs = vec![
            block(
                0,
                72.0,
                400.0,
                330.0,
                10.0,
                "The line above ends a sentence and fills the measure.",
            ),
            block(
                1,
                84.0,
                387.0,
                300.0,
                10.0,
                "My own journey starts a new paragraph.",
            ),
        ];
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .into_iter()
            .map(|seg| seg.text)
            .collect();
        assert_eq!(texts.len(), 2, "{texts:?}");
        assert!(texts[0].contains("sentence") || texts[1].contains("sentence"));
        assert!(texts.iter().any(|text| text.starts_with("My own")));
    }

    #[test]
    fn a_flush_line_after_an_indent_stays_in_the_paragraph() {
        let glyphs = vec![
            block(
                0,
                84.0,
                400.0,
                280.0,
                10.0,
                "My own journey starts here and",
            ),
            block(
                1,
                72.0,
                387.0,
                290.0,
                10.0,
                "continues on the flush line underneath.",
            ),
        ];
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .into_iter()
            .map(|seg| seg.text)
            .collect();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(texts[0].contains("journey") && texts[0].contains("underneath"));
    }

    #[test]
    fn a_page_folio_stays_off_the_license_line() {
        let glyphs = vec![
            block(
                0,
                166.0,
                19.0,
                200.0,
                8.0,
                "Licensed to THIAGO BANDEIRA <thiago@lar.ifce.edu.br>",
            ),
            block(1, 255.0, 60.0, 16.0, 9.0, "xvi"),
        ];
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .into_iter()
            .map(|seg| seg.text)
            .collect();
        assert!(texts.iter().any(|text| text == "xvi"), "{texts:?}");
        assert!(
            texts
                .iter()
                .any(|text| text.starts_with("Licensed") && !text.contains("xvi")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_license_footer_does_not_steal_the_next_page() {
        let mut body = block(
            0,
            132.0,
            120.0,
            280.0,
            10.0,
            "pipelines that preserve key ideas and then evolve",
        );
        let license = block(
            1,
            166.0,
            19.0,
            200.0,
            8.0,
            "Licensed to THIAGO BANDEIRA <thiago@lar.ifce.edu.br>",
        );
        let mut next = block(
            2,
            142.0,
            640.0,
            280.0,
            10.0,
            "from a single tool into a full agent with tracing.",
        );
        next.page_index = 1;
        body.page_index = 0;
        let texts: Vec<_> = segment_glyphs(&[body, license, next])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("evolve from a single")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.starts_with("Licensed") && !text.contains("evolve")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_url_broken_after_a_hyphen_stays_one_token() {
        let glyphs = vec![
            block(
                0,
                72.0,
                500.0,
                360.0,
                10.0,
                "See https://livebook.manning.com/book/ai-agents-and",
            ),
            block(1, 72.0, 487.0, 120.0, 10.0, "-applications."),
            block(
                2,
                72.0,
                400.0,
                320.0,
                10.0,
                "visit https://livebook.manning",
            ),
            block(
                3,
                72.0,
                387.0,
                280.0,
                10.0,
                ".com/book/ai-agents-and-applications/discussion.",
            ),
            block(
                4,
                72.0,
                360.0,
                340.0,
                10.0,
                "from Manning at www.manning.com/books/ai-agents",
            ),
            block(
                5,
                72.0,
                347.0,
                180.0,
                10.0,
                "-and-applications and mirrored",
            ),
        ];
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| {
                text.contains("https://livebook.manning.com/book/ai-agents-and-applications")
                    && !text.contains("and -")
            }),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|text| {
                text.contains(
                    "https://livebook.manning.com/book/ai-agents-and-applications/discussion",
                )
            }),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|text| {
                text.contains("www.manning.com/books/ai-agents-and-applications")
                    && !text.contains("agents -")
            }),
            "{texts:?}"
        );
    }

    #[test]
    fn a_hanging_hyphen_joins_back_into_its_column() {
        let glyphs = vec![
            block(
                0,
                102.0,
                400.0,
                340.0,
                10.0,
                "This book is divided into chapters across five parts.",
            ),
            block(
                1,
                102.0,
                387.0,
                340.0,
                10.0,
                "Each part builds on the previous one in order.",
            ),
            block(
                2,
                130.0,
                200.0,
                250.0,
                10.0,
                "summarization chains for documents, multi-",
            ),
            block(
                3,
                142.0,
                187.0,
                250.0,
                10.0,
                "document corpora stay together here.",
            ),
            block(4, 130.0, 160.0, 180.0, 10.0, "covering per-"),
            block(
                5,
                142.0,
                147.0,
                220.0,
                10.0,
                "sona and context stay one word.",
            ),
        ];
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("multi-document")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|text| text.contains("persona")),
            "{texts:?}"
        );
        let fused = segment_glyphs(&[
            block(10, 72.0, 400.0, 300.0, 10.0, "avoiding over-"),
            block(11, 72.0, 387.0, 280.0, 10.0, "flow in the window."),
            block(12, 72.0, 360.0, 300.0, 10.0, "what to pre-"),
            block(13, 72.0, 347.0, 280.0, 10.0, "serve in the policy."),
            block(14, 72.0, 320.0, 300.0, 10.0, "when inter-"),
            block(15, 72.0, 307.0, 280.0, 10.0, "mediate evidence remains."),
        ]);
        let fused: Vec<_> = fused.iter().map(|seg| seg.text.clone()).collect();
        let fused_text = fused.join("\n");
        assert!(
            fused_text.contains("overflow") && !fused_text.contains("over-"),
            "{fused:?}"
        );
        assert!(
            fused_text.contains("preserve") && !fused_text.contains("pre-"),
            "{fused:?}"
        );
        assert!(
            fused_text.contains("intermediate") && !fused_text.contains("inter-"),
            "{fused:?}"
        );
        let proper = segment_glyphs(&[
            block(20, 72.0, 400.0, 280.0, 10.0, "memory via Lang-"),
            block(
                21,
                72.0,
                387.0,
                220.0,
                10.0,
                "Graph checkpoints stay one word.",
            ),
        ]);
        let proper: Vec<_> = proper.iter().map(|seg| seg.text.clone()).collect();
        assert!(
            proper.iter().any(|text| text.contains("LangGraph")),
            "{proper:?}"
        );
        assert!(
            !proper.iter().any(|text| text.contains("Lang-")),
            "{proper:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("per-sona")),
            "{texts:?}"
        );
    }

    #[test]
    fn boxed_diagram_labels_stay_original_without_a_figure_caption() {
        let label = block(0, 140.0, 450.0, 70.0, 10.0, "Retriever");
        let caption = block(
            1,
            80.0,
            250.0,
            360.0,
            10.0,
            "Retrieval-Augmented Generation stage overview of the pipeline",
        );
        let regions = vec![
            crate::glyph::PaintedRegion {
                page_index: 0,
                bbox: [100.0, 420.0, 300.0, 600.0],
                kind: "path".into(),
            },
            crate::glyph::PaintedRegion {
                page_index: 0,
                bbox: [100.0, 320.0, 300.0, 430.0],
                kind: "path".into(),
            },
        ];
        let pages = vec![crate::glyph::PageInfo {
            index: 0,
            object_id: "1 0".into(),
            media_box: [0.0, 0.0, 612.0, 792.0],
            rotate: 0,
        }];
        let seg = segment_placed(
            &[label, caption],
            &SegmentFlags::default(),
            &regions,
            &pages,
        );
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Retrieval-Augmented")),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("Retriever")),
            "{texts:?}"
        );
        assert!(
            seg.kept.iter().any(|(_, reason)| reason == "figure"),
            "{:?}",
            seg.kept
        );
    }

    #[test]
    fn a_sideways_margin_footer_stays_out_of_the_line() {
        let mut line = paint_word(
            &mut 0,
            126.0,
            573.0,
            "Added numerous examples suggested via public comments",
            "TimesNewRomanPSMT",
        );
        let mut stamp = glyph(100, 18.0, 575.0, "h", false);
        stamp.matrix = [0.0, -9.0, 9.0, 0.0, 18.0, 575.0];
        stamp.font_size = 9.0;
        line.push(stamp);
        let texts: Vec<_> = segment_glyphs(&line)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("numerous examples")),
            "{texts:?}"
        );
        assert!(!texts.iter().any(|text| text.contains('h')), "{texts:?}");
    }

    #[test]
    fn an_icon_font_bullet_keeps_each_slide_item_separate() {
        let items = [
            block(
                0,
                48.0,
                315.0,
                540.0,
                30.0,
                "\u{e902}Why we need to understand our tests",
            ),
            block(
                1,
                48.0,
                272.0,
                370.0,
                30.0,
                "\u{e902}What mutation testing is",
            ),
            block(
                2,
                48.0,
                230.0,
                720.0,
                30.0,
                "\u{e902}How mutation testing got to practical applicability",
            ),
        ];
        let texts: Vec<_> = segment_glyphs(&items)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert_eq!(texts.len(), 3, "{texts:?}");
        assert!(
            texts.iter().all(|text| text.starts_with('\u{e902}')),
            "{texts:?}"
        );
    }

    #[test]
    fn a_scanned_margin_rule_does_not_join_the_sentence() {
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        let words = [
            "Follow the campaign",
            "pages of a paper",
            "Your ideas matter",
            "And the ideas of",
        ];
        for (row, word) in words.iter().enumerate() {
            let y = 400.0 - row as f32 * 14.0;
            let mut left = glyph(id, 30.0, y, if row % 2 == 0 { "§" } else { "|" }, false);
            id += 1;
            left.font_size = 10.0;
            left.bbox = [30.0, y, 36.0, y + 10.0];
            glyphs.push(left);
            // The rule and the words are one line. A wide space glyph fills
            // the margin, so a column split does not see a gap.
            let mut space = glyph(id, 36.0, y, " ", false);
            id += 1;
            space.font_size = 10.0;
            space.bbox = [36.0, y, 80.0, y + 10.0];
            space.advance = [44.0, 0.0];
            glyphs.push(space);
            let mut body = glyph(id, 80.0, y, word, false);
            id += 1;
            body.font_size = 10.0;
            body.bbox = [80.0, y, 220.0, y + 10.0];
            body.advance = [140.0, 0.0];
            glyphs.push(body);
            let mut right = glyph(id, 300.0, y, "1", false);
            id += 1;
            right.font_size = 10.0;
            right.bbox = [300.0, y, 306.0, y + 10.0];
            glyphs.push(right);
        }
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .all(|text| !text.contains('§') && !text.contains('|')),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Follow the campaign")),
            "{texts:?}"
        );
        assert!(
            seg.kept.iter().any(|(_, reason)| reason == "margin"),
            "{:?}",
            seg.kept
        );
    }

    #[test]
    fn a_figure_text_layer_on_a_born_digital_page_is_not_translated() {
        let mut glyphs = Vec::new();
        let words = [
            "The",
            "method",
            "improves",
            "accuracy",
            "on",
            "every",
            "benchmark",
            "we",
            "tried",
        ];
        let mut id = 0u32;
        for row in 0..3 {
            for (col, word) in words.iter().enumerate() {
                glyphs.push(glyph(
                    id,
                    72.0 + col as f32 * 50.0,
                    700.0 - row as f32 * 12.0,
                    word,
                    false,
                ));
                id += 1;
            }
        }
        let mut hidden = glyph(
            id,
            72.0,
            400.0,
            "Complete Autonomy: unrestricted access",
            false,
        );
        hidden.invisible = true;
        hidden.source.kind = crate::glyph::SourceKind::FormXObject;
        glyphs.push(hidden);
        let segments = segment_glyphs(&glyphs);
        assert!(segments.iter().all(|seg| !seg.glyph_ids.contains(&id)));
        assert!(segments.iter().any(|seg| seg.text.contains("benchmark")));
    }

    #[test]
    fn a_junk_ocr_cover_stays_out_of_the_segments() {
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        let cover = [
            "TOILETTE",
            "*ILLUSION*",
            "3jOV£9rOIit",
            "POBUtfttCO BV TttH",
            "LITERATURE PEFARTMENT",
            "1113 W. WASHINGTON ST.",
        ];
        for (row, text) in cover.iter().enumerate() {
            let mut item = glyph(id, 72.0, 700.0 - row as f32 * 16.0, text, false);
            item.page_index = 0;
            item.invisible = true;
            glyphs.push(item);
            id += 1;
        }
        let mut sentence = glyph(
            id,
            72.0,
            700.0,
            "The ideas are expressed every day in the pages.",
            false,
        );
        sentence.page_index = 1;
        glyphs.push(sentence);
        id += 1;
        let mut title = glyph(id, 72.0, 700.0, "Constraints", false);
        title.page_index = 2;
        glyphs.push(title);
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().all(|text| !text.contains("POBUtfttCO")
                && !text.contains("3jOV")
                && !text.contains("TOILETTE")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("expressed every day")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|text| text.contains("Constraints")),
            "{texts:?}"
        );
        assert!(
            seg.kept.iter().any(|(_, reason)| reason == "ocr"),
            "{:?}",
            seg.kept
        );
    }

    #[test]
    fn a_citation_superscript_plus_joins_the_key() {
        let mut host = glyph(0, 72.0, 100.0, "[FKL 91]", false);
        host.font_size = 10.0;
        host.bbox = [72.0, 98.0, 150.0, 110.0];
        host.advance = [78.0, 0.0];
        let mut plus = glyph(1, 112.0, 104.5, "+", false);
        plus.font_size = 7.5;
        plus.bbox = [112.0, 104.0, 118.0, 112.0];
        let texts: Vec<_> = segment_glyphs(&[host, plus])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains('+') && text.contains("FKL")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_centered_permission_notice_is_one_paragraph() {
        // Attention page 1. The two full lines share a margin. `scholarly
        // works.` is centered on the same axis, so its left edge used to
        // start a new segment and the translation kept that short line.
        let rows = [
            (
                124.0,
                710.0,
                364.0,
                "Provided proper attribution is provided, Google hereby grants permission to",
            ),
            (
                124.3,
                696.0,
                363.6,
                "reproduce the tables and figures in this paper solely for use in journalistic or",
            ),
            (267.6, 682.0, 78.9, "scholarly works."),
        ];
        let mut glyphs = Vec::new();
        for (id, (x, y, width, text)) in rows.iter().enumerate() {
            let mut item = glyph(id as u32, *x, *y, text, false);
            item.font_size = 12.0;
            item.bbox = [*x, *y, x + width, y + 12.0];
            item.advance = [*width, 0.0];
            glyphs.push(item);
        }
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(
            texts[0].contains("journalistic or") && texts[0].contains("scholarly works."),
            "{texts:?}"
        );
    }

    #[test]
    fn a_centered_comma_list_is_one_paragraph() {
        let rows = [
            (180.0, 500.0, 240.0, "Ada, Android, Go, Rust,"),
            (140.0, 486.0, 320.0, "Energy, Legal and Policy, LLVM,"),
            (160.0, 472.0, 280.0, "Matrix.org Foundation and Community"),
        ];
        let mut glyphs = Vec::new();
        for (id, (x, y, width, text)) in rows.iter().enumerate() {
            let mut item = glyph(id as u32, *x, *y, text, false);
            item.font_size = 10.0;
            item.bbox = [*x, *y, x + width, y + 10.0];
            item.advance = [*width, 0.0];
            glyphs.push(item);
        }
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(
            texts[0].contains("Energy") && texts[0].contains("Matrix.org"),
            "{texts:?}"
        );
    }

    #[test]
    fn a_centered_list_stays_one_paragraph_when_a_line_barely_shifts() {
        // FOSDEM page 14. Consecutive centered lines of similar length sit
        // within an em, which the indent test used to treat as a new paragraph.
        // The next page's list must not be stitched on because it starts with
        // `and`.
        let rows = [
            (
                0u32,
                0u32,
                40.0,
                500.0,
                640.0,
                "Microkernel and OS, Modern Email, Monitoring and,",
            ),
            (
                1,
                0,
                28.0,
                476.0,
                664.0,
                "Observability, Mozilla, MySQL, Network, Nix and NixOS,",
            ),
            (
                2,
                0,
                20.0,
                452.0,
                680.0,
                "Open Media, Open Research, Open Source Design,",
            ),
            (
                3,
                0,
                8.0,
                428.0,
                704.0,
                "PostgreSQL, Python, Quantum Computing, Radio, Railways",
            ),
            (
                4,
                0,
                20.0,
                404.0,
                680.0,
                "and Open Transport, Real Time Communications (RTC), RISC-V,",
            ),
            (
                5,
                0,
                12.0,
                380.0,
                696.0,
                "Robotics and Simulation, Rust, Security, (SBOM), Swift,",
            ),
            (
                6,
                1,
                22.0,
                500.0,
                676.0,
                "and Open Transport, RISC-V, Rust, Security, Swift,",
            ),
        ];
        let mut glyphs = Vec::new();
        for (id, page, x, y, width, text) in rows {
            let mut item = glyph(id, x, y, text, false);
            item.page_index = page;
            item.font_size = 20.0;
            item.bbox = [x, y, x + width, y + 16.0];
            item.advance = [width, 0.0];
            glyphs.push(item);
        }
        let segs = segment_glyphs(&glyphs);
        let page0: Vec<_> = segs.iter().filter(|seg| seg.page_index == 0).collect();
        assert_eq!(
            page0.len(),
            1,
            "{:?}",
            page0.iter().map(|seg| &seg.text).collect::<Vec<_>>()
        );
        assert!(
            page0[0].text.contains("RISC-V") && page0[0].text.contains("Railways"),
            "{}",
            page0[0].text
        );
        let page1: Vec<_> = segs.iter().filter(|seg| seg.page_index == 1).collect();
        assert_eq!(page1.len(), 1, "the next slide was stitched onto this list");
        assert!(page1[0].text.contains("RISC-V"), "{}", page1[0].text);
    }

    #[test]
    fn a_schedule_bullet_does_not_absorb_the_next_slides_list() {
        // FOSDEM page 12 ends on a bullet. Page 13 is a centered topic list.
        // Carrying the bullet onto that list painted `Ada` on the schedule.
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        let push = |glyphs: &mut Vec<_>, id: &mut u32, page, x, y, width, text: &str| {
            let mut item = glyph(*id, x, y, text, false);
            item.page_index = page;
            item.font_size = 18.0;
            item.bbox = [x, y, x + width, y + 16.0];
            item.advance = [width, 0.0];
            glyphs.push(item);
            *id += 1;
        };
        push(
            &mut glyphs,
            &mut id,
            0,
            40.0,
            300.0,
            220.0,
            "● 1052 events (+190)",
        );
        push(
            &mut glyphs,
            &mut id,
            0,
            40.0,
            270.0,
            240.0,
            "● 933 speakers (+146)",
        );
        push(
            &mut glyphs,
            &mut id,
            0,
            56.0,
            240.0,
            360.0,
            "○ plus all the lightning talk",
        );
        push(
            &mut glyphs,
            &mut id,
            1,
            40.0,
            320.0,
            640.0,
            "Ada, Android, Go, Rust, Energy,",
        );
        push(
            &mut glyphs,
            &mut id,
            1,
            28.0,
            296.0,
            664.0,
            "Legal and Policy, LLVM, RISC-V, PostgreSQL,",
        );
        push(
            &mut glyphs,
            &mut id,
            1,
            80.0,
            272.0,
            560.0,
            "Matrix.org Foundation and Community",
        );
        let segs = segment_glyphs(&glyphs);
        let page0: Vec<_> = segs.iter().filter(|seg| seg.page_index == 0).collect();
        assert!(
            page0.iter().all(|seg| !seg.text.contains("Ada")),
            "schedule absorbed the list: {:?}",
            page0.iter().map(|seg| &seg.text).collect::<Vec<_>>()
        );
        let page1: Vec<_> = segs.iter().filter(|seg| seg.page_index == 1).collect();
        assert_eq!(
            page1.len(),
            1,
            "{:?}",
            page1.iter().map(|seg| &seg.text).collect::<Vec<_>>()
        );
        assert!(page1[0].text.contains("RISC-V") && page1[0].text.contains("Ada"));
    }

    #[test]
    fn a_padded_last_line_and_a_short_centered_tail_stay_in_the_list() {
        // FOSDEM page 14. The last line is padded with spaces out past the
        // page, and page 13's last item sits on the title's left edge.
        let rows = [
            (
                0u32,
                40.0,
                500.0,
                460.0,
                "Microkernel and OS, Modern Email, Monitoring and,",
            ),
            (
                1,
                28.0,
                476.0,
                444.0,
                "Observability, Mozilla, MySQL, Network, Nix and NixOS,",
            ),
            (
                2,
                8.0,
                452.0,
                484.0,
                "PostgreSQL, Python, Quantum Computing, Radio, Railways",
            ),
            (
                3,
                8.0,
                428.0,
                484.0,
                "(SBOM), Software Defined Storage, Swift, Tool the",
            ),
            (
                4,
                26.0,
                404.0,
                448.0,
                "Docs, Virtualization and Cloud, WebAssembly,",
            ),
        ];
        let mut glyphs = Vec::new();
        let mut next = 0u32;
        for (id, x, y, width, text) in rows {
            let mut item = glyph(id, x, y, text, false);
            item.font_size = 20.0;
            item.bbox = [x, y, x + width, y + 16.0];
            item.advance = [width, 0.0];
            glyphs.push(item);
            next = id + 1;
        }
        for index in 0..12 {
            let x = 500.0 + index as f32 * 40.0;
            let mut space = glyph(next, x, 404.0, " ", false);
            space.font_size = 20.0;
            space.bbox = [x, 404.0, x + 36.0, 420.0];
            space.advance = [36.0, 0.0];
            glyphs.push(space);
            next += 1;
        }
        let mut title = glyph(next, 199.0, 640.0, "Schedule - Devrooms", false);
        title.font_size = 36.0;
        title.bbox = [199.0, 640.0, 520.0, 676.0];
        title.advance = [321.0, 0.0];
        glyphs.push(title);
        next += 1;
        let mut tail = glyph(
            next,
            196.0,
            380.0,
            "Matrix.org Foundation and Community",
            false,
        );
        tail.font_size = 20.0;
        // Same midpoint as the list (250), and the same left edge as the title.
        tail.bbox = [196.0, 380.0, 304.0, 400.0];
        tail.advance = [108.0, 0.0];
        glyphs.push(tail);
        let segs = segment_glyphs(&glyphs);
        let body: Vec<_> = segs
            .iter()
            .filter(|seg| seg.text.contains("Railways") || seg.text.contains("Matrix.org"))
            .collect();
        assert_eq!(
            body.len(),
            1,
            "{:?}",
            segs.iter().map(|seg| seg.text.as_str()).collect::<Vec<_>>()
        );
        assert!(body[0].text.contains("WebAssembly"), "{}", body[0].text);
        assert!(body[0].text.contains("Matrix.org"), "{}", body[0].text);
        assert!(!body[0].text.ends_with("   "), "{}", body[0].text);
    }

    #[test]
    fn a_partial_ocr_title_keeps_the_image_and_a_bad_block_is_dropped() {
        let mut glyphs = Vec::new();
        let mut title = glyph(0, 80.0, 500.0, "Constraints of Public Utilities", false);
        title.page_index = 0;
        title.font_size = 18.0;
        title.bbox = [80.0, 500.0, 400.0, 520.0];
        glyphs.push(title);
        let mut junk = glyph(1, 80.0, 460.0, "3jOV£9rOIit", false);
        junk.page_index = 0;
        junk.bbox = [80.0, 460.0, 200.0, 474.0];
        glyphs.push(junk);
        let mut prose = glyph(
            2,
            72.0,
            700.0,
            "The ideas are expressed every day in the pages.",
            false,
        );
        prose.page_index = 1;
        prose.bbox = [72.0, 700.0, 400.0, 712.0];
        glyphs.push(prose);
        let mut noise = glyph(3, 72.0, 680.0, "POBUtfttCO BV TttH", false);
        noise.page_index = 1;
        noise.bbox = [72.0, 680.0, 220.0, 692.0];
        glyphs.push(noise);
        let pages = vec![
            crate::glyph::PageInfo {
                index: 0,
                object_id: "1 0".into(),
                media_box: [0.0, 0.0, 612.0, 792.0],
                rotate: 0,
            },
            crate::glyph::PageInfo {
                index: 1,
                object_id: "2 0".into(),
                media_box: [0.0, 0.0, 612.0, 792.0],
                rotate: 0,
            },
        ];
        let regions = vec![crate::glyph::PaintedRegion {
            page_index: 0,
            bbox: [0.0, 0.0, 612.0, 792.0],
            kind: "image".into(),
        }];
        assert!(
            ocr_line_low_quality("3jOV£9rOIit"),
            "scorer missed the garbage token"
        );
        assert!(
            ocr_line_low_quality("POBUtfttCO BV TttH"),
            "scorer missed the smear"
        );
        assert!(
            !ocr_line_low_quality("GKP24, CP26].") && !ocr_line_low_quality("[BBN10]"),
            "a citation key was scored as OCR"
        );
        assert!(
            !ocr_line_low_quality("µ(G) = min ν(det(g)). The term")
                && !ocr_line_low_quality("−d(x,y)")
                && !ocr_line_low_quality("1≤i<···<i"),
            "a formula line was scored as OCR"
        );
        assert!(
            !ocr_line_low_quality("A×B={⟨x,y⟩:x∈A and y∈B}.")
                && !ocr_line_low_quality(
                    "A×B.Let Bx={⟨x,y⟩:y∈B}.Since whenever x≠x2,⟨x1,y⟩≠⟨x2,y⟩,"
                ),
            "an angle-bracket formula was scored as OCR"
        );
        let seg = segment_placed(&glyphs, &SegmentFlags::default(), &regions, &pages);
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .all(|text| !text.contains("3jOV") && !text.contains("Constraints")),
            "title scan was painted: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("expressed every day")),
            "{texts:?}"
        );
        assert!(
            texts.iter().all(|text| !text.contains("POBUtfttCO")),
            "{texts:?}"
        );
        assert!(seg.kept.iter().any(|(_, reason)| reason == "ocr"));
    }

    #[test]
    fn a_glued_margin_ornament_stays_out_of_the_sentence() {
        // `||`, `j§`, and a stray `S` sit in one margin column. The gap is
        // inside the run, so the last glyph alone is not a wide-spaced rule.
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        let rows: &[(&str, &str, &[&str])] = &[
            ("=", "are expressed every day in the pages of", &["|", "|"]),
            ("1", "your candidate!", &["j", "§"]),
            ("§", "Follow the campaign through the", &["1"]),
            ("I", "And the ideas of everyone", &["1"]),
            ("|", "Outside of Chicago months", &["S"]),
        ];
        for (row, (left, body, right)) in rows.iter().enumerate() {
            let y = 400.0 - row as f32 * 14.0;
            let mut mark = glyph(id, 30.0, y, left, false);
            id += 1;
            mark.font_size = 9.0;
            mark.bbox = [30.0, y, 36.0, y + 9.0];
            glyphs.push(mark);
            let mut word = glyph(id, 80.0, y, body, false);
            id += 1;
            word.font_size = 9.0;
            word.bbox = [80.0, y, 220.0, y + 9.0];
            glyphs.push(word);
            let mut cursor = 300.0f32;
            for piece in *right {
                let width = 3.0;
                let mut item = glyph(id, cursor, y, piece, false);
                id += 1;
                item.font_size = 9.0;
                item.bbox = [cursor, y, cursor + width, y + 9.0];
                glyphs.push(item);
                cursor += width;
            }
        }
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .all(|text| { !text.contains('|') && !text.contains('§') && !text.contains('S') }),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("pages of") && !text.contains('j')),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|text| text.contains("your candidate!")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_born_digital_slide_is_not_dropped_as_ocr() {
        let pages = vec![crate::glyph::PageInfo {
            index: 0,
            object_id: "1 0".into(),
            media_box: [0.0, 0.0, 720.0, 405.0],
            rotate: 0,
        }];
        let regions = vec![crate::glyph::PaintedRegion {
            page_index: 0,
            bbox: [150.0, 0.0, 570.0, 405.0],
            kind: "image".into(),
        }];
        let slides: &[&[&str]] = &[
            &[
                "Network",
                "●FOSDEM",
                "○IPv6-only - first non-network large-scale conference network to be so",
                "○NAT64 & DNS64",
                "○Fix yo bugs!",
                "●FOSDEM-Dualstack",
                "○IPv4 & IPv6",
                "These are open networks – Encrypt your traffic",
            ],
            &[
                "Schedule",
                "●1052 events (+190)",
                "●933 speakers (+146)",
                "●70 tracks/devrooms (+3)",
                "●43 BoFs",
                "○plus all the lightning lightning talk",
            ],
            &[
                "Thanks",
                "●All volunteers",
                "●All devroom managers",
                "●All speakers",
                "●All staff",
            ],
        ];
        for lines in slides {
            for line in *lines {
                assert!(!ocr_line_low_quality(line), "low: {line}");
            }
            let mut glyphs = Vec::new();
            for (index, text) in lines.iter().enumerate() {
                let y = 360.0 - index as f32 * 28.0;
                // The mascot image starts at x=150. The line stays to its left,
                // which is how the slide's text sits beside the picture.
                glyphs.push(block(index as u32, 40.0, y, 100.0, 16.0, text));
            }
            let seg = segment_placed(&glyphs, &SegmentFlags::default(), &regions, &pages);
            let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
            assert!(
                texts.iter().any(|text| text.contains(lines[1])),
                "slide line was dropped as OCR: {texts:?}"
            );
            assert!(
                seg.kept.iter().all(|(_, reason)| reason != "ocr"),
                "born-digital slide was kept as OCR: {:?}",
                seg.kept
            );
        }
        assert!(ocr_line_low_quality("3jOV£9rOIit"));
        assert!(!ocr_line_low_quality("○IPv4 & IPv6"));
        assert!(!ocr_line_low_quality("●All volunteers"));
        assert!(!ocr_line_low_quality("●1052 events (+190)"));
    }

    #[test]
    fn an_ocr_rule_of_ells_is_not_a_sentence() {
        let noise = block(
            0,
            30.0,
            500.0,
            300.0,
            6.0,
            "fllHlllilllllllflllilllllillllllllllilllllilll",
        );
        let body = block(1, 70.0, 400.0, 200.0, 9.0, "Your ideas matter here.");
        let seg = segment_with(&[noise, body], &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(texts.iter().all(|text| !text.contains("fllH")), "{texts:?}");
        assert!(
            texts.iter().any(|text| text.contains("ideas matter")),
            "{texts:?}"
        );
        assert!(
            seg.kept.iter().any(|(_, reason)| reason == "margin"),
            "{:?}",
            seg.kept
        );
    }

    #[test]
    fn a_white_circle_sub_bullet_stays_its_own_item() {
        // A slide's ○ item sits one leading under the ● line. It is the next
        // item, not the tail of "Sunday evening".
        let parent = block(
            0,
            40.0,
            300.0,
            520.0,
            18.0,
            "● As is tradition: If you help clean up Sunday evening",
        );
        let child = block(
            1,
            72.0,
            274.0,
            420.0,
            18.0,
            "○ Keep all network cable you remove under",
        );
        let texts: Vec<_> = segment_glyphs(&[parent, child])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert_eq!(texts.len(), 2, "{texts:?}");
        assert!(
            texts[0].starts_with('●') && !texts[0].contains('○'),
            "{texts:?}"
        );
        assert!(texts[1].starts_with('○'), "{texts:?}");
    }

    #[test]
    fn a_large_slide_title_on_a_photo_is_not_a_figure_label() {
        let mut title = block(0, 310.0, 168.0, 100.0, 56.0, "You!");
        title.font_name = "Signika-Regular".into();
        let regions = [crate::glyph::PaintedRegion {
            page_index: 0,
            bbox: [150.0, -7.0, 570.0, 412.0],
            kind: "image".into(),
        }];
        let pages = [crate::glyph::PageInfo {
            index: 0,
            object_id: "1 0".into(),
            media_box: [0.0, 0.0, 720.0, 405.0],
            rotate: 0,
        }];
        let seg = segment_placed(&[title], &SegmentFlags::default(), &regions, &pages);
        assert!(
            seg.kept.iter().all(|(_, reason)| reason != "figure"),
            "slide title kept as a figure: {:?}",
            seg.kept
        );
        assert!(
            seg.segments.iter().any(|item| item.text.contains("You!")),
            "{:?}",
            seg.segments
                .iter()
                .map(|item| &item.text)
                .collect::<Vec<_>>()
        );
        let mut label = block(1, 200.0, 200.0, 36.0, 12.0, "GPU");
        label.font_name = "Signika-Regular".into();
        let small = segment_placed(&[label], &SegmentFlags::default(), &regions, &pages);
        assert!(
            small.kept.iter().any(|(_, reason)| reason == "figure"),
            "diagram label was translated: {:?}",
            small.kept
        );
    }

    #[test]
    fn a_short_wrap_beside_a_theorem_frame_stays_in_the_paragraph() {
        let upper = block(
            0,
            115.0,
            235.0,
            380.0,
            10.5,
            "Following is an equivalent finitary",
        );
        let wrap = block(1, 115.0, 222.0, 170.0, 10.5, "version. We write the set.");
        let label = block(2, 200.0, 190.0, 70.0, 10.0, "Retriever");
        let theorem = block(
            3,
            123.0,
            171.0,
            360.0,
            10.5,
            "then there is a monochromatic solution",
        );
        let tail = block(4, 123.0, 158.0, 90.0, 10.5, "to x + y = z.");
        let regions = vec![
            crate::glyph::PaintedRegion {
                page_index: 0,
                bbox: [115.0, 149.0, 498.0, 213.0],
                kind: "path".into(),
            },
            crate::glyph::PaintedRegion {
                page_index: 0,
                bbox: [116.0, 150.0, 497.0, 212.0],
                kind: "path".into(),
            },
        ];
        let pages = vec![crate::glyph::PageInfo {
            index: 0,
            object_id: "1 0".into(),
            media_box: [0.0, 0.0, 612.0, 792.0],
            rotate: 0,
        }];
        let seg = segment_placed(
            &[upper, wrap, label, theorem, tail],
            &SegmentFlags::default(),
            &regions,
            &pages,
        );
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("finitary version. We write")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("solution to x + y = z.")),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("Retriever")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_drop_cap_joins_the_body_under_it() {
        let mut glyphs = Vec::new();
        let mut cap = glyph(0, 126.0, 402.0, "T", false);
        cap.font_size = 48.0;
        cap.matrix = [48.0, 0.0, 0.0, 48.0, 126.0, 402.0];
        cap.bbox = [126.0, 392.0, 158.0, 440.0];
        glyphs.push(cap);
        let mut id = 1u32;
        let mut paint = |text: &str, x: f32, y: f32| {
            let mut cursor = x;
            for ch in text.chars() {
                let mut item = glyph(id, cursor, y, &ch.to_string(), false);
                id += 1;
                item.font_size = 10.0;
                item.matrix = [10.0, 0.0, 0.0, 10.0, cursor, y];
                let width = if ch == ' ' { 3.0 } else { 5.2 };
                item.bbox = [cursor, y - 2.0, cursor + width, y + 8.0];
                cursor += width;
                glyphs.push(item);
            }
        };
        paint(
            "his opening part lays the foundation for the book.",
            154.0,
            402.0,
        );
        paint(
            "explore why the rest of this opening part matters.",
            126.0,
            389.0,
        );
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("opening") && text.contains("explore")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_lowercase_sentence_tail_stays_with_the_line_above() {
        let mut id = 0u32;
        let mut glyphs = paint_word(
            &mut id,
            102.0,
            400.0,
            "cross-references throughout the text will help",
            "Times-Roman",
        );
        glyphs.extend(paint_word(
            &mut id,
            102.0,
            386.0,
            "you bridge any gaps:",
            "Times-Roman",
        ));
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("cross-references") && text.contains("bridge any gaps")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_hanging_dash_item_is_one_paragraph_and_the_next_item_is_not() {
        let mut id = 0u32;
        let mut glyphs = paint_word(
            &mut id,
            117.8,
            400.0,
            "– Chapter 1 maps the landscape from summarization and",
            "Times-Roman",
        );
        glyphs.extend(paint_word(
            &mut id,
            129.8,
            386.0,
            "semantic search to chatbots and agents across the book.",
            "Times-Roman",
        ));
        glyphs.extend(paint_word(
            &mut id,
            117.8,
            370.0,
            "– Chapter 2 offers a separate introduction here.",
            "Times-Roman",
        ));
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| {
                text.contains("Chapter 1")
                    && text.contains("semantic")
                    && !text.contains("Chapter 2")
            }),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Chapter 2") && !text.contains("semantic")),
            "{texts:?}"
        );
    }

    #[test]
    fn an_italic_part_bullet_joins_its_roman_continuation() {
        let mut id = 0u32;
        let mut glyphs = paint_word(&mut id, 117.8, 400.0, "\u{f0a1}", "Wingdings2");
        glyphs.extend(paint_word(
            &mut id,
            129.8,
            400.0,
            "Part 1: Getting started with LLMs (chapters 1-2)",
            "Times-Italic",
        ));
        glyphs.extend(paint_word(
            &mut id,
            360.0,
            400.0,
            "—This part lays the foundation by",
            "Times-Roman",
        ));
        glyphs.extend(paint_word(
            &mut id,
            129.8,
            386.0,
            "exploring where LLMs excel and where they struggle today.",
            "Times-Roman",
        ));
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| {
                text.contains("Part 1") && text.contains("exploring") && text.contains("foundation")
            }),
            "{texts:?}"
        );
    }

    #[test]
    fn a_hanging_tail_stays_with_the_list_when_the_body_sets_the_margin() {
        let mut id = 0u32;
        let mut glyphs = Vec::new();
        for (index, line) in [
            "This book is divided into chapters and each part builds on the previous one.",
            "You can read it from start to finish or focus on the sections you need most.",
            "Cross references throughout the text will help you bridge any gaps today.",
        ]
        .into_iter()
        .enumerate()
        {
            glyphs.extend(paint_word(
                &mut id,
                72.0,
                520.0 - index as f32 * 14.0,
                line,
                "Times-Roman",
            ));
        }
        glyphs.extend(paint_word(
            &mut id,
            100.0,
            478.0,
            "–Chapter 1 maps the landscape from summarization and",
            "Times-Roman",
        ));
        glyphs.extend(paint_word(
            &mut id,
            112.0,
            464.0,
            "semantic search to chatbots and then examines common chal-",
            "Times-Roman",
        ));
        glyphs.extend(paint_word(
            &mut id,
            112.0,
            450.0,
            "lenges. The pieces fit together and you will use the",
            "Times-Roman",
        ));
        glyphs.extend(paint_word(
            &mut id,
            112.0,
            436.0,
            "LangChain templates generate the prompts.",
            "Times-Roman",
        ));
        glyphs.extend(paint_word(
            &mut id,
            112.0,
            422.0,
            "grounded systems.",
            "Times-Roman",
        ));
        glyphs.extend(paint_word(
            &mut id,
            100.0,
            408.0,
            "–Chapter 2 offers a separate introduction to prompting.",
            "Times-Roman",
        ));
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| {
                text.contains("Chapter 1")
                    && text.contains("grounded systems")
                    && text.contains("LangChain templates")
                    && text.contains("challenges")
                    && !text.contains("Chapter 2")
            }),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Chapter 2") && !text.contains("grounded")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_list_item_continues_onto_the_next_page() {
        let mut id = 0u32;
        let mut glyphs = paint_word(
            &mut id,
            100.0,
            90.0,
            "–Chapter 3 builds summarization chains and then you construct LangChain",
            "Times-Roman",
        );
        glyphs.extend(paint_word(
            &mut id,
            112.0,
            76.0,
            "Expression Language pipelines that keep the key points",
            "Times-Roman",
        ));
        for glyph in &mut glyphs {
            glyph.page_index = 0;
        }
        let mut next = paint_word(
            &mut id,
            112.0,
            700.0,
            "Expression stays with the chapter that began on the previous page.",
            "Times-Roman",
        );
        for glyph in &mut next {
            glyph.page_index = 1;
        }
        glyphs.extend(next);
        let mut chapter = paint_word(
            &mut id,
            100.0,
            680.0,
            "–Chapter 4 develops a research summarization engine for the web.",
            "Times-Roman",
        );
        for glyph in &mut chapter {
            glyph.page_index = 1;
        }
        glyphs.extend(chapter);
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| {
                text.contains("Chapter 3")
                    && text.contains("previous page")
                    && !text.contains("Chapter 4")
            }),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Chapter 4") && !text.contains("Chapter 3")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_larger_slide_title_does_not_continue_the_bullet() {
        let mut id = 0u32;
        let mut paint = |x, y, size, text: &str| {
            let mut glyphs = paint_word(&mut id, x, y, text, "Signika-Regular");
            let mut cursor = x;
            for glyph in &mut glyphs {
                let ch = glyph.unicode.chars().next().unwrap_or(' ');
                let width = if ch == ' ' { size * 0.33 } else { size * 0.52 };
                glyph.font_size = size;
                glyph.matrix = [size, 0.0, 0.0, size, cursor, y];
                glyph.bbox = [cursor, y, cursor + width, y + size];
                glyph.advance = [width, 0.0];
                cursor += width;
            }
            glyphs
        };
        let mut bullet = paint(72.0, 140.0, 24.0, "○ Dinner for all teardown volunteers on");
        let mut wrap = paint(
            104.0,
            112.0,
            24.0,
            "Sunday night (same as Friday night buildup)",
        );
        for glyph in bullet.iter_mut().chain(wrap.iter_mut()) {
            glyph.page_index = 0;
        }
        let mut title = paint(109.0, 700.0, 36.0, "If you need help / who does what");
        for glyph in &mut title {
            glyph.page_index = 1;
        }
        bullet.extend(wrap);
        bullet.extend(title);
        let texts: Vec<_> = segment_glyphs(&bullet)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| {
                text.contains("Dinner")
                    && text.contains("buildup")
                    && !text.contains("who does what")
            }),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("who does what") && !text.contains("Dinner")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_name_split_across_body_lines_stays_one_paragraph() {
        let glyphs = vec![
            block(
                0,
                66.0,
                405.0,
                363.0,
                9.0,
                "Many of the designations used by manufacturers and sellers to distinguish their products are",
            ),
            block(
                1,
                66.0,
                394.0,
                329.0,
                9.0,
                "claimed as trademarks. Where those designations appear in the book, and Manning",
            ),
            block(
                2,
                66.0,
                383.0,
                369.0,
                9.0,
                "Publications was aware of a trademark claim, the designations have been printed in initial caps",
            ),
            block(3, 66.0, 372.0, 42.0, 9.0, "or all caps."),
        ];
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .into_iter()
            .map(|seg| seg.text)
            .collect();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(
            texts[0].contains("and Manning Publications was aware"),
            "{texts:?}"
        );
        assert!(texts[0].contains("or all caps."), "{texts:?}");
    }

    #[test]
    fn a_contents_title_shorter_than_the_next_stays_apart() {
        let glyphs = vec![
            block(
                0,
                177.0,
                360.0,
                221.0,
                10.0,
                "Setting up the chain to generate a step-back question",
            ),
            block(
                1,
                177.0,
                347.0,
                264.0,
                10.0,
                "Incorporating step-back question generation into the RAG chain",
            ),
        ];
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .into_iter()
            .map(|seg| seg.text)
            .collect();
        assert_eq!(texts.len(), 2, "{texts:?}");
    }

    #[test]
    fn a_finished_sentence_above_a_longer_line_stays_apart() {
        let glyphs = vec![
            block(0, 66.0, 400.0, 180.0, 9.0, "or all caps."),
            block(
                1,
                66.0,
                389.0,
                360.0,
                9.0,
                "Recognizing the importance of preserving what has been written, it is policy.",
            ),
        ];
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .into_iter()
            .map(|seg| seg.text)
            .collect();
        assert_eq!(texts.len(), 2, "{texts:?}");
    }

    #[test]
    fn short_address_rows_stay_separate_under_a_full_measure() {
        let mut id = 0u32;
        let mut glyphs = paint_word(
            &mut id,
            66.0,
            560.0,
            "For online information and ordering of this and other Manning books, please visit",
            "Times-Roman",
        );
        glyphs.extend(paint_word(
            &mut id,
            90.0,
            420.0,
            "Special Sales Department",
            "Times-Roman",
        ));
        glyphs.extend(paint_word(
            &mut id,
            90.0,
            409.0,
            "Manning Publications Co.",
            "Times-Roman",
        ));
        glyphs.extend(paint_word(
            &mut id,
            90.0,
            398.0,
            "20 Baldwin Road",
            "Times-Roman",
        ));
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(
                |text| text.contains("Special Sales") && !text.contains("Manning Publications")
            ),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Manning Publications") && !text.contains("Baldwin")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Baldwin") && !text.contains("Manning")),
            "{texts:?}"
        );
    }

    #[test]
    fn roman_page_numbers_are_folios() {
        assert!(is_roman_numeral("xvi"));
        assert!(is_roman_numeral("XVIII"));
        assert!(is_roman_numeral("xxvi"));
        assert!(!is_roman_numeral("did"));
        assert!(!is_roman_numeral("civil"));
    }

    #[test]
    fn a_finished_contents_row_does_not_absorb_the_next_entry() {
        let chapter = block(
            0,
            120.0,
            160.0,
            200.0,
            10.0,
            "Query generation, routing, and retrieval postprocessing",
        );
        let chapter_page = block(1, 340.0, 160.0, 24.0, 10.0, "228");
        let mut appendix = block(
            2,
            110.0,
            700.0,
            210.0,
            10.0,
            "appendix A Trying out LangChain",
        );
        appendix.page_index = 1;
        let mut appendix_page = block(3, 340.0, 700.0, 24.0, 10.0, "351");
        appendix_page.page_index = 1;
        let texts: Vec<_> = segment_glyphs(&[chapter, chapter_page, appendix, appendix_page])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("Query generation")
                && !text.to_ascii_lowercase().contains("appendix")),
            "chapter 10 absorbed the next page: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("appendix A") && !text.contains("Query")),
            "appendix A was not its own row: {texts:?}"
        );

        let lower = block(
            4,
            112.0,
            200.0,
            180.0,
            10.0,
            "appendix D Installing SQLite on Windows",
        );
        let lower_page = block(5, 310.0, 200.0, 24.0, 10.0, "375");
        let upper = block(6, 186.0, 640.0, 150.0, 10.0, "our travel assistant");
        let upper_page = block(7, 350.0, 640.0, 24.0, 10.0, "331");
        let column: Vec<_> = segment_glyphs(&[lower, lower_page, upper, upper_page])
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            column
                .iter()
                .any(|text| text.contains("appendix D") && !text.contains("travel")),
            "appendix joined the indented sub-entry: {column:?}"
        );
        assert!(
            column
                .iter()
                .any(|text| text.contains("travel assistant") && !text.contains("appendix")),
            "sub-entry missing: {column:?}"
        );
    }

    #[test]
    fn a_table_caption_does_not_take_the_other_column() {
        // BERT Table 5: the caption's next line is its own wrap. The other
        // column's lowercase line sits one leading lower and used to be read
        // as that wrap, so the caption was painted across the gutter.
        let glyphs = vec![
            wide(
                0,
                72.0,
                400.0,
                "Table 5: Ablation over the pre-training tasks using the",
                218.0,
                9.0,
            ),
            wide(
                1,
                307.0,
                392.0,
                "expensive as a single bidirectional model and more",
                210.0,
                9.0,
            ),
            wide(
                2,
                72.0,
                388.0,
                "architecture remains in the caption column here today.",
                218.0,
                9.0,
            ),
        ];
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .into_iter()
            .map(|item| item.text)
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("Table 5")
                && text.contains("architecture remains")
                && !text.contains("expensive")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("expensive") && !text.contains("Table 5")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_full_width_line_does_not_claim_the_other_column() {
        let bottom = wide(
            0,
            72.0,
            80.0,
            "but the line already spans both columns here",
            450.0,
            10.0,
        );
        let top = wide(
            1,
            320.0,
            700.0,
            "results are still far worse than the baseline.",
            200.0,
            10.0,
        );
        let texts: Vec<_> = segment_glyphs(&[bottom, top])
            .into_iter()
            .map(|item| item.text)
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("both columns") && !text.contains("results are still")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("results are still") && !text.contains("both columns")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_caption_wrap_is_not_swallowed_by_the_other_column() {
        let glyphs = vec![
            wide(
                0,
                50.0,
                400.0,
                "Figure 4: Loss convergence curves of fine-tuning the ablated",
                240.0,
                9.0,
            ),
            wide(
                1,
                320.0,
                390.0,
                "This sentence is long enough to count as ordinary body text in the column today.",
                240.0,
                9.0,
            ),
            wide(
                2,
                50.0,
                389.0,
                "models of LayoutLMv3 on PubLayNet dataset and the loss of model two.",
                240.0,
                9.0,
            ),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Figure 4") && text.contains("models of LayoutLMv3")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("ordinary body") && !text.contains("Figure 4")),
            "{texts:?}"
        );
        assert!(
            seg.kept.iter().all(|(_, reason)| reason != "figure"),
            "{:?}",
            seg.kept
        );
    }

    #[test]
    fn a_math_gap_does_not_send_the_hyphen_to_the_next_page() {
        // `where W ∈ R` and `are learnable … em-` are one baseline. The gap
        // after the math is just over 1.25em, so the hyphen lands on a line
        // whose left edge is 110pt to the right. `bedding` steps back to the
        // margin. It is that word, not `operates` at the top of the next page.
        let size = 10.91;
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        let mut place = |glyphs: &mut Vec<Glyph>, x: f32, y: f32, page: u32, text: &str| {
            let mut cursor = x;
            for ch in text.chars() {
                let width = if ch == ' ' { size * 0.3 } else { size * 0.48 };
                let mut item = glyph(id, cursor, y, &ch.to_string(), false);
                id += 1;
                item.font_size = size;
                item.page_index = page;
                item.matrix = [size, 0.0, 0.0, size, cursor, y];
                item.bbox = [cursor, y, cursor + width, y + size];
                cursor += width;
                glyphs.push(item);
            }
            cursor
        };
        let math_end = place(&mut glyphs, 70.4, 551.7, 0, "where W q in R ");
        let _ = math_end;
        place(
            &mut glyphs,
            math_end + 14.6,
            551.7,
            0,
            "are learnable projection matrices that map queries to a relational em-",
        );
        place(
            &mut glyphs,
            70.9,
            538.1,
            0,
            "bedding space and the head dimension follows.",
        );
        place(
            &mut glyphs,
            70.9,
            745.9,
            1,
            "operates on pairs after the page break.",
        );
        let texts: Vec<_> = segment_glyphs(&glyphs)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("embedding space")),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("emoperates")),
            "{texts:?}"
        );
    }

    #[test]
    fn the_rtdoc_math_gap_hyphen_stays_on_the_page() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/cache/arxiv-2606.23344.pdf");
        if !path.exists() {
            return;
        }
        let doc = crate::extract::PdfDocument::open(&path).unwrap();
        let extraction = doc.extract_with(&crate::extract::ExtractOptions::default());
        let seg = segment_placed(
            &extraction.glyphs,
            &SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let embedding = seg
            .segments
            .iter()
            .find(|item| item.text.contains("embedding space"));
        let embedding = embedding.expect("embedding sentence");
        assert!(
            !embedding.text.contains("operates"),
            "next page joined over the rest of this page: {}",
            embedding.text.chars().take(220).collect::<String>()
        );
        assert!(
            seg.segments
                .iter()
                .any(|item| item.text.contains("inherently anti-symmetric")),
            "the following line should stay on this page"
        );
        let caption = seg
            .segments
            .iter()
            .find(|item| item.text.contains("Figure 2"))
            .expect("figure caption");
        assert!(
            caption.text.contains("reading order prediction"),
            "caption wrap missing: {}",
            caption.text
        );
        assert!(
            seg.segments.iter().any(|item| {
                item.text.contains("order loss") && item.text.contains("operates on")
            }),
            "page break should finish the loss sentence"
        );
        assert!(
            !seg.segments.iter().any(|item| item.text.contains("where K")
                && item.text.contains("reading order prediction")),
            "caption wrap joined the loss paragraph"
        );
    }

    #[test]
    fn a_centered_caption_wrap_does_not_finish_the_column() {
        let caption = wide(
            0,
            71.0,
            600.0,
            "Figure 2 | The unified architecture of RT-DocLayout, featuring parallel heads for segmentation",
            450.0,
            10.0,
        );
        let wrap = wide(
            1,
            226.0,
            586.0,
            "and reading order prediction.",
            180.0,
            10.0,
        );
        let loss = wide(
            2,
            71.0,
            120.0,
            "the order loss receives a higher weight for two reasons",
            450.0,
            10.0,
        );
        let mut operates = wide(
            3,
            71.0,
            760.0,
            "operates on pairs after the page break and keeps going.",
            420.0,
            10.0,
        );
        operates.page_index = 1;
        let texts: Vec<_> = segment_glyphs(&[caption, wrap, loss, operates])
            .into_iter()
            .map(|item| item.text)
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Figure 2") && text.contains("reading order prediction")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("order loss") && text.contains("operates on")),
            "{texts:?}"
        );
        assert!(
            !texts
                .iter()
                .any(|text| text.contains("where") && text.contains("reading order prediction")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_numbered_hanging_item_stays_one_paragraph() {
        let mut id = 0u32;
        let mut lead = paint_word(
            &mut id,
            117.2,
            220.0,
            "1 The agent sends a prompt to the LLM asking it to choose the most relevant",
            "FranklinGothic-Demi",
        );
        let text_x = lead
            .iter()
            .find(|glyph| glyph.unicode == "T")
            .map(|glyph| glyph.matrix[4])
            .unwrap();
        let mut wrap = paint_word(
            &mut id,
            text_x,
            207.0,
            "tools for the request and the holiday deals databases.",
            "NewBaskerville-Roman",
        );
        let mut next = paint_word(
            &mut id,
            117.2,
            180.0,
            "2 Guided by a developer prompt that lists the available tools.",
            "FranklinGothic-Demi",
        );
        for glyph in lead
            .iter_mut()
            .chain(wrap.iter_mut())
            .chain(next.iter_mut())
        {
            glyph.font_size = 10.0;
            glyph.matrix[0] = 10.0;
            glyph.matrix[3] = 10.0;
        }
        lead.extend(wrap);
        lead.extend(next);
        let texts: Vec<_> = segment_glyphs(&lead)
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| {
                text.contains("1 The agent")
                    && text.contains("holiday deals")
                    && !text.contains("2 Guided")
            }),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("2 Guided") && !text.contains("holiday deals")),
            "{texts:?}"
        );
        let joined = texts
            .iter()
            .find(|text| text.contains("holiday deals"))
            .expect("item 1");
        assert_eq!(
            crate::layout::heading_level(2, 10.0, true, joined),
            crate::layout::HeadingLevel::Body,
            "a numbered item with a hanging continuation is not a heading"
        );
        assert_eq!(
            crate::layout::heading_level(
                2,
                10.0,
                true,
                "1 The agent sends a prompt asking it to choose tools",
            ),
            crate::layout::HeadingLevel::Body,
            "the first line of a numbered item is not a heading once a continuation follows"
        );
        assert_eq!(
            numbered_item_body("1 智能体向大语言模型发送提示"),
            Some("智能体向大语言模型发送提示")
        );
        assert!(numbered_item_body("1.1 构建基于大语言模型的应用").is_none());
    }
}
