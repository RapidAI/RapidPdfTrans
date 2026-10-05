//! Group extracted glyphs into paragraphs.
//!
//! Detection is a pure function of glyph positions and font names. It does
//! not call a model. A segment is one paragraph: visual lines in the same
//! column, joined so a later rewrite can reflow the block inside that box.
//! Superscripts and footnote marks are attached to the line they sit on.
//! Figure interiors and table cells are not segments; only their captions
//! are. Unmapped glyphs stay out of every segment.

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
        let new_line = match (last_page, last_y) {
            (Some(page), Some(y)) => {
                glyph.page_index != page || (glyph.matrix[5] - y).abs() > last_size * 0.5
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
        let line_size = current
            .iter()
            .map(|item| item.font_size)
            .fold(last_size, f32::max)
            .max(1.0);
        // A line number in the gutter is lower and smaller than this line,
        // but still inside the baseline tolerance. Gluing it to the hyphen
        // makes the line look like a contents entry, so the word never joins.
        let gutter_mark =
            glyph.font_size <= line_size * 0.8 && left > line_right + line_size * 0.35;
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
        if (jumped_back || gutter_mark) && !current.is_empty() {
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
    split_columns(line)
}

/// Split one baseline where a gap is a column gutter rather than a word space.
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
    let size = line
        .iter()
        .map(|glyph| glyph.font_size)
        .fold(1.0f32, f32::max);
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
    let mut parts = Vec::new();
    let mut current = vec![line[0]];
    for (index, glyph) in line.iter().copied().enumerate().skip(1) {
        let gap = gaps[index - 1];
        // A detailed-contents bullet is a drawing, so the title and the next
        // entry look like a ~14pt gutter. The left piece is a narrow crumb
        // (`client 83`). A real column is much wider, and a short last line
        // that does not end in a page number still splits.
        let crumb = gap <= hard && narrow_page_crumb(&current);
        let prev_size = current
            .iter()
            .map(|item| item.font_size)
            .fold(1.0f32, f32::max);
        // A smaller line number just past the hyphen is inside the column
        // trigger. Leaving it here glues the digit to the word, and the line
        // is then read as a contents entry. A small index such as `(i)` has
        // body text tight on its right, so it stays in the sentence.
        let gutter_digit = glyph.font_size <= prev_size * 0.8
            && gap > prev_size * 0.5
            && trailing_gutter_mark(&line[index..], prev_size);
        if (gap > trigger || gutter_digit) && !current.is_empty() && !crumb {
            parts.push(std::mem::take(&mut current));
        }
        current.push(glyph);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// The next body-sized glyph is another column, or nothing follows.
/// A parenthetical index has the sentence continuing immediately.
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RunFace {
    Bold,
    Italic,
}

fn run_face(name: &str) -> Option<RunFace> {
    let style = crate::font::face_style(name);
    if style.bold {
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
    if tail_letters < 8 || has_year(&tail) {
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

fn assemble(
    raw: Vec<Vec<&Glyph>>,
    flags: &SegmentFlags,
    regions: &[PaintedRegion],
    pages: &[PageInfo],
) -> Segmentation {
    let mut lines: Vec<VisualLine> = raw.into_iter().map(visual_line).collect();
    attach_markers(&mut lines);
    attach_math_scripts(&mut lines);
    let mut kept = detach_toc_marks(&mut lines);
    kept.extend(interior_glyphs(&lines, flags));
    kept.extend(region_interiors(&lines, flags, regions, pages));
    kept.extend(margin_stamps(&lines));
    release_hyphen_continuations(&lines, &mut kept);
    let kept_ids: std::collections::HashSet<u32> = kept.iter().map(|(id, _)| *id).collect();
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
            if let Some((label, rest)) = styled_run_in(first) {
                // "Definition 1.1." is bold and "Proof." is italic. The sentence
                // after either label keeps the body face.
                buf = line_text(&label);
                buf_ids.extend(label.iter().map(|glyph| glyph.id));
                flush(&mut segments, &mut buf_ids, &mut buf);
                buf = line_text(&rest);
                buf_ids.extend(rest.iter().map(|glyph| glyph.id));
            } else {
                let split_lead = styled_lead_line(first).is_some_and(|face| {
                    lines.clone().next().is_some_and(|next| {
                        majority_run(next) != Some(face)
                            && !has_year(&first.text)
                            && !has_year(&next.text)
                    })
                });
                buf = first.text.clone();
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
                buf = line.text.clone();
                buf_ids.extend(line.glyphs.iter().map(|glyph| glyph.id));
                continue;
            }
            let piece = if url_continues(&buf, &line.text) {
                format!("{}{}", buf.trim_end(), line.text.trim_start())
            } else if let Some(stem) = soft_hyphen_stem(&buf) {
                let rest = line.text.trim_start();
                if rest.starts_with(|ch: char| ch.is_ascii_alphanumeric()) {
                    join_hyphenated_word(stem, rest)
                } else {
                    format!("{} {}", buf.trim_end(), line.text.trim_start())
                }
            } else {
                format!("{} {}", buf.trim_end(), line.text.trim_start())
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
    for (id, seg) in segments.iter_mut().enumerate() {
        seg.id = id as u32;
    }
    Segmentation { segments, kept }
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
    }
}

/// A contents row is `title | leaders | page`. The leaders and the page
/// number stay on their original baselines. Each title between page numbers
/// is its own line, so a later rewrite cannot pull the next entry across.
fn detach_toc_marks(lines: &mut Vec<VisualLine<'_>>) -> Vec<(u32, String)> {
    let mut kept = Vec::new();
    let mut expanded = Vec::with_capacity(lines.len());
    for line in lines.drain(..) {
        let marks = toc_mark_glyphs(&line);
        if marks.is_empty() {
            expanded.push(line);
            continue;
        }
        let mark_ids: std::collections::HashSet<u32> = marks.iter().copied().collect();
        for id in &marks {
            kept.push((*id, "toc".into()));
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

fn toc_mark_glyphs(line: &VisualLine<'_>) -> Vec<u32> {
    let tokens = line_tokens(&line.glyphs, line.size);
    let mut keep = Vec::new();
    for (index, token) in tokens.iter().enumerate().skip(1) {
        if !is_toc_page_token(&token.text) {
            continue;
        }
        let prev = &tokens[index - 1];
        let gap = token.left - prev.right;
        let leaders = is_leader_text(&prev.text);
        if !leaders && gap < line.size * 0.8 {
            continue;
        }
        if leaders {
            keep.extend(prev.ids.iter().copied());
        }
        keep.extend(token.ids.iter().copied());
    }
    // `.......1` is one token when the page number sits against the dots.
    for id in toc_leader_suffix(&line.glyphs) {
        if !keep.contains(&id) {
            keep.push(id);
        }
    }
    keep
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
    // its page number on this edge.
    if dot_chars < 4 || !page_ok {
        return Vec::new();
    }
    let end = page_end;
    ordered[dots_start..end]
        .iter()
        .filter(|glyph| !glyph.unicode.trim().is_empty())
        .map(|glyph| glyph.id)
        .collect()
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

fn line_bounds(glyphs: &[&Glyph]) -> Option<(f32, f32, f32, f32)> {
    let first = *glyphs.first()?;
    let left = glyphs
        .iter()
        .map(|glyph| glyph.matrix[4].min(glyph.bbox[0]).min(glyph.bbox[2]))
        .fold(f32::MAX, f32::min);
    let right = glyphs
        .iter()
        .map(|glyph| glyph.bbox[0].max(glyph.bbox[2]).max(glyph.matrix[4]))
        .fold(left, f32::max);
    let mut ys: Vec<f32> = glyphs.iter().map(|glyph| glyph.matrix[5]).collect();
    ys.sort_by(|a, b| a.total_cmp(b));
    let y = ys[ys.len() / 2];
    let size = glyphs
        .iter()
        .map(|glyph| glyph.font_size)
        .fold(first.font_size, f32::max)
        .max(1.0);
    Some((left, right, y, size))
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

fn fonts_can_join(upper: &VisualLine<'_>, lower: &VisualLine<'_>) -> bool {
    if same_font_family(&upper.font, &lower.font) {
        return true;
    }
    let families = |line: &VisualLine<'_>| {
        line.glyphs
            .iter()
            .map(|glyph| font_family(&glyph.font_name))
            .collect::<std::collections::HashSet<_>>()
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
            ch.is_ascii_digit() || matches!(ch, '*' | '∗' | '†' | '‡' | '§' | '¶' | '⋆' | '#')
        })
}

/// Glyphs drawn inside a figure or a table, excluding the caption paragraph.
fn interior_glyphs(lines: &[VisualLine<'_>], flags: &SegmentFlags) -> Vec<(u32, String)> {
    if !flags.skip_figures && !flags.skip_tables {
        return Vec::new();
    }
    let order = reading_order(lines);
    let mut body_fonts: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
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
        for index in figure_side(&block, lines, body_font, &caption_line) {
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
    let kept_ids: std::collections::HashSet<u32> = kept.iter().map(|(id, _)| *id).collect();
    let mut release = std::collections::HashSet::new();
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
            if (upper.left - lower.left).abs() > 36.0 || !continues_paragraph(upper, lower) {
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
    let mut by_page: std::collections::HashMap<u32, Vec<&PaintedRegion>> =
        std::collections::HashMap::new();
    for region in regions {
        by_page.entry(region.page_index).or_default().push(region);
    }
    let mut kept = Vec::new();
    for (page, regs) in by_page {
        let (width, height) = page_size(pages, page);
        let unions = figure_unions(&regs, width, height);
        for line in lines.iter().filter(|line| line.page == page) {
            if !line_in_figure(line, &unions) {
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
    let mut groups: std::collections::HashMap<usize, (Rect, bool, bool, usize)> =
        std::collections::HashMap::new();
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

fn line_in_figure(line: &VisualLine<'_>, unions: &[Rect]) -> bool {
    if unions.is_empty()
        || caption_kind(&line.text).is_some()
        || is_running_header(&line.text)
        || is_footer_line(&line.text)
        || is_page_folio_text(&line.text)
        || is_body_shape(line)
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
        let above = line.y > rect.y1
            && line.y <= rect.y1 + line.size * 2.4
            && line.left >= rect.x0 - 8.0
            && line.right <= rect.x1 + 24.0;
        if inside * 2 <= total && !above {
            continue;
        }
        let width = line.right - line.left;
        if above || width <= rect.width() + 24.0 {
            return true;
        }
    }
    false
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
        let Some(rest) = rest.strip_prefix(|ch: char| ch.is_ascii_digit()) else {
            continue;
        };
        let rest = rest.trim_start_matches(|ch: char| ch.is_ascii_digit());
        let rest = rest.trim_start();
        // "Figure 1:" and "Table 2." are captions. A long "Figure 1 Overview of …"
        // is too. "Fig 1" and "Table 1 shows" are ordinary sentences.
        if rest.starts_with([':', '.']) {
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
        if lines[index].page != lines[start].page || caption_kind(&lines[index].text).is_some() {
            break;
        }
        let prev = *block.last().unwrap_or(&start);
        if !continues_paragraph(&lines[prev], &lines[index]) {
            break;
        }
        let width = lines[index].right - lines[index].left;
        if width < cap_w * 0.35 && lines[index].text.chars().count() < 24 {
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
    let above = walk_interior(top, 1.0, page, block, lines, body_font, caption_line);
    let below = walk_interior(bottom, -1.0, page, block, lines, body_font, caption_line);
    // A figure caption sits under the drawing; a table caption sits above the cells.
    if above.len() >= below.len() && !above.is_empty() {
        above
    } else {
        below
    }
}

fn walk_interior(
    origin_y: f32,
    direction: f32,
    page: u32,
    block: &[usize],
    lines: &[VisualLine<'_>],
    body_font: &str,
    caption_line: &[bool],
) -> Vec<usize> {
    let anchor = &lines[block[0]];
    let mut cursor = origin_y;
    let mut picked = Vec::new();
    loop {
        let mut band: Vec<(usize, f32)> = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if line.page != page || block.contains(&index) || picked.contains(&index) {
                continue;
            }
            let dy = (line.y - cursor) * direction;
            if dy <= 0.4 || !x_overlaps(anchor, line) {
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
        if nearest > 140.0 || (cursor - origin_y).abs() > 420.0 {
            break;
        }
        let size = lines[band[0].0].size.max(1.0);
        let group: Vec<usize> = band
            .into_iter()
            .filter(|(_, dy)| *dy <= nearest + size * 0.5)
            .map(|(index, _)| index)
            .collect();
        if group.iter().any(|&index| {
            caption_line.get(index).copied().unwrap_or(false)
                || stops_interior(&lines[index], body_font)
                || is_running_header(&lines[index].text)
        }) {
            break;
        }
        for index in &group {
            picked.push(*index);
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
    lower == "preprint"
        || lower == "published"
        || lower.starts_with("under review")
        || lower.starts_with("proceedings of")
}

fn stops_interior(line: &VisualLine<'_>, body_font: &str) -> bool {
    caption_kind(&line.text).is_some()
        || is_body_barrier(line, body_font)
        || is_section_heading(line, body_font)
}

fn x_overlaps(anchor: &VisualLine<'_>, line: &VisualLine<'_>) -> bool {
    let left = anchor.left.max(line.left);
    let right = anchor.right.min(line.right);
    let overlap = (right - left).max(0.0);
    let narrow = (anchor.right - anchor.left)
        .min(line.right - line.left)
        .max(1.0);
    overlap >= narrow * 0.3
}

fn dominant_body_font(lines: &[VisualLine<'_>], page: u32) -> String {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for line in lines.iter().filter(|line| line.page == page) {
        if is_body_shape(line) {
            *counts.entry(line.font.as_str()).or_insert(0) += line.text.len();
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(font, _)| font.to_string())
        .unwrap_or_default()
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
fn is_section_heading(line: &VisualLine<'_>, body_font: &str) -> bool {
    if !body_font.is_empty() && line.font != body_font {
        return false;
    }
    let text = line.text.trim();
    if text.chars().count() > 48 || line.right - line.left > 260.0 {
        return false;
    }
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    let digits = index;
    if digits == 0 || digits > 2 {
        return false;
    }
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
    }
    if index >= bytes.len() || bytes[index] != b' ' {
        return false;
    }
    text[index..]
        .trim()
        .chars()
        .filter(|ch| ch.is_alphabetic())
        .count()
        >= 4
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
    let mut seen = std::collections::HashSet::new();
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
    let mut by_page: std::collections::HashMap<u32, Vec<&Glyph>> = std::collections::HashMap::new();
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
            if (glyph.matrix[4] - anchor).abs() <= 4.0 && seen.insert(glyph.id) {
                kept.push((glyph.id, "margin".into()));
            }
        }
    }
    kept
}

fn stamp_glyph(glyph: &Glyph) -> bool {
    let left = glyph.matrix[4].min(glyph.bbox[0]).min(glyph.bbox[2]);
    let right = glyph.bbox[0].max(glyph.bbox[2]).max(glyph.matrix[4]);
    left <= 56.0 && right - left <= 24.0 && glyph.font_size >= 14.0
}

fn join_paragraphs(lines: Vec<VisualLine<'_>>) -> Vec<Vec<VisualLine<'_>>> {
    // A left-edge stamp between two body baselines must not split the paragraph.
    // Join each column on its own. The anchor is that column's median left edge,
    // so a chain of small indents cannot pull the other column in.
    let mut columns: Vec<Vec<VisualLine>> = Vec::new();
    for line in lines {
        let slot = columns.iter().position(|column| {
            column_anchor(column, line.page).is_some_and(|anchor| {
                let distance = (anchor - line.left).abs();
                // A wrapped word can start a list-indented line and finish
                // back on the column margin. That line sits to the right of
                // the margin. A body line to the left of a centered title is
                // a different column, even when a hyphen makes the gap look
                // like an indent.
                let indented = line.left + 1.0 >= anchor;
                distance <= 28.0
                    || (distance <= 64.0 && indented && soft_hyphen_stem(&line.text).is_some())
            }) || column.last().is_some_and(|prev| hyphen_pull(prev, &line))
        });
        if let Some(slot) = slot {
            columns[slot].push(line);
        } else {
            columns.push(vec![line]);
        }
    }
    let mut paragraphs = Vec::new();
    for mut column in columns {
        column.sort_by(|a, b| {
            a.page
                .cmp(&b.page)
                .then(b.y.total_cmp(&a.y))
                .then(a.left.total_cmp(&b.left))
        });
        let mut current: Vec<VisualLine> = Vec::new();
        for line in column {
            if current
                .last()
                .is_some_and(|prev| continues_paragraph(prev, &line))
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
            let Some(next) = find_reading_continuation(&current, &paragraphs, &used) else {
                break;
            };
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
) -> Option<usize> {
    if let Some(next) = find_column_continuation(current, paragraphs, used) {
        return Some(next);
    }
    if closes_the_page(current, paragraphs, used) {
        return find_page_continuation(current, paragraphs, used);
    }
    None
}

fn find_column_continuation(
    current: &[VisualLine<'_>],
    paragraphs: &[Vec<VisualLine<'_>>],
    used: &[bool],
) -> Option<usize> {
    if !closes_the_column(current, paragraphs, used) {
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

fn closes_the_column(
    current: &[VisualLine<'_>],
    paragraphs: &[Vec<VisualLine<'_>>],
    used: &[bool],
) -> bool {
    let Some(upper) = current.last() else {
        return false;
    };
    for (index, para) in paragraphs.iter().enumerate() {
        if used[index] || para.is_empty() || is_page_bridge(para) {
            continue;
        }
        let lower = &para[0];
        if lower.page != upper.page || (lower.left - upper.left).abs() > 36.0 {
            continue;
        }
        if lower.y >= upper.y - 0.5 {
            continue;
        }
        if substantial_paragraph(para) {
            return false;
        }
    }
    true
}

fn find_page_continuation(
    current: &[VisualLine<'_>],
    paragraphs: &[Vec<VisualLine<'_>>],
    used: &[bool],
) -> Option<usize> {
    let upper = current.last()?;
    // Vector order follows column reading order, so a later paragraph in the
    // next page can appear first. The continuation is the top of the left column.
    let mut best: Option<(usize, f32, f32)> = None;
    for (index, para) in paragraphs.iter().enumerate() {
        if used[index] || para.is_empty() || is_page_bridge(para) {
            continue;
        }
        let lower = &para[0];
        if lower.page != upper.page + 1 {
            continue;
        }
        let replace = match best {
            None => true,
            Some((_, left, y)) => {
                lower.left < left - 28.0 || ((lower.left - left).abs() <= 28.0 && lower.y > y)
            }
        };
        if replace {
            best = Some((index, lower.left, lower.y));
        }
    }
    let index = best?.0;
    page_continuation(current, &paragraphs[index]).then_some(index)
}

/// True when nothing but a footer, caption, or header sits below this paragraph.
fn closes_the_page(
    current: &[VisualLine<'_>],
    paragraphs: &[Vec<VisualLine<'_>>],
    used: &[bool],
) -> bool {
    let Some(upper) = current.last() else {
        return false;
    };
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
            if substantial_paragraph(para) && !is_margin_strip(para) {
                return false;
            }
            continue;
        }
        if (lower.left - upper.left).abs() > 36.0 || lower.y >= upper.y - 0.5 {
            continue;
        }
        if substantial_paragraph(para) {
            return false;
        }
    }
    true
}

/// A real column, not a table cell such as `t [s]` or `Method`.
fn votes_as_column(para: &[VisualLine<'_>]) -> bool {
    let letters: usize = para
        .iter()
        .map(|line| line.text.chars().filter(|ch| ch.is_alphabetic()).count())
        .sum();
    letters >= 16 || para.len() >= 2
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
    // The right column ends on this page and the sentence continues at the
    // top of the next page's left column. A new paragraph, or a centered
    // running title, does not: the continuation has to finish the hyphen
    // in lowercase.
    let finishes_hyphen = soft_hyphen_stem(&upper.text).is_some()
        && lower
            .text
            .trim_start()
            .starts_with(|ch: char| ch.is_ascii_lowercase());
    if (upper.left - lower.left).abs() > 36.0 && !finishes_hyphen {
        return false;
    }
    prose_continues(upper, lower)
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
    // A line beside this one is the other column's matching row, not the
    // continuation. The next column's text starts near the top of the page,
    // not a folio a few lines above a footer.
    let size = upper.size.max(lower.size).max(1.0);
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
    start.starts_with(|ch: char| ch.is_ascii_lowercase())
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

fn continues_paragraph(upper: &VisualLine<'_>, lower: &VisualLine<'_>) -> bool {
    if upper.page != lower.page || upper.toc || lower.toc {
        return false;
    }
    let next = lower.text.trim_start();
    // `GPT-` / `2` and `MetaMath-` / `7B` are one token. A contents row is
    // `83 Generating` only when the line above is not finishing that token.
    let hyphen = soft_hyphen_stem(&upper.text).is_some()
        && next.starts_with(|ch: char| ch.is_ascii_alphanumeric());
    // A detailed-contents line already holds several entries (`83 Generating`).
    // Joining the wrap makes one block the Chinese leading cannot fit.
    if !hyphen && (inline_contents_entry(&upper.text) || inline_contents_entry(&lower.text)) {
        return false;
    }
    // A display equation keeps its own segment. Joining it onto the prose
    // around it would leave that prose untranslated. A calligraphic letter
    // or a subscript inside a sentence does not.
    if !hyphen && (line_has_formula(upper) || line_has_formula(lower)) {
        return false;
    }
    let size = upper.size.max(lower.size).max(1.0);
    let dy = upper.y - lower.y;
    if dy < size * 0.7 || dy > size * 1.5 {
        return false;
    }
    if (upper.size - lower.size).abs() > size * 0.2 {
        return false;
    }
    // A bold or italic run-in is the same paragraph as the regular line under it.
    // A monospace phrase at the start of a line is not a new paragraph.
    if !hyphen && !fonts_can_join(upper, lower) {
        return false;
    }
    if (upper.left - lower.left).abs() > 36.0 && !hyphen {
        return false;
    }
    // A first-line indent, with the same leading and no extra gap, is a new
    // paragraph. A hyphenated word's continuation is indented the other way
    // and is handled above.
    if !hyphen && lower.left > upper.left + size * 0.55 {
        return false;
    }
    if hyphen {
        return true;
    }
    if toc_entry_boundary(upper, lower) {
        return false;
    }
    let upper_w = upper.right - upper.left;
    let lower_w = lower.right - lower.left;
    if upper.right + size * 2.2 < lower.right {
        return false;
    }
    if lower_w < upper_w * 0.5
        && lower.text.chars().count() < 32
        && !lower.text.trim_end().ends_with('.')
    {
        return false;
    }
    let overlap = (upper.right.min(lower.right) - upper.left.max(lower.left)).max(0.0);
    let narrow = upper_w.min(lower_w).max(1.0);
    overlap >= narrow * 0.35 || (upper.left - lower.left).abs() <= 8.0
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
    // equation has more math glyphs than prose letters.
    math > 0 && (letters < 16 || math >= letters)
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
        if letters < 16 {
            continue;
        }
        let dy = (line.y - other.y).abs();
        let size = other.size.max(1.0);
        if dy < size * 0.08 || dy > size * 0.7 {
            continue;
        }
        if line.left < other.left - size || line.right > other.right + size {
            continue;
        }
        if best.is_none_or(|(_, best_dy)| dy < best_dy) {
            best = Some((other_index, dy));
        }
    }
    best.map(|(host, _)| host)
}

fn is_math_script(line: &VisualLine<'_>) -> bool {
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
    if is_hard_prefix(stem_word) && next_word.len() >= 3 && !is_hyphen_suffix(next_word) {
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

/// A hanging indent that finishes `multi-` / `per-` belongs to that column
/// even when the column's median left edge is the body margin.
fn hyphen_pull(prev: &VisualLine<'_>, line: &VisualLine<'_>) -> bool {
    let shift = line.left - prev.left;
    prev.page == line.page
        && soft_hyphen_stem(&prev.text).is_some()
        && line
            .text
            .trim_start()
            .starts_with(|ch: char| ch.is_ascii_alphabetic())
        // The continuation may hang a little to the right, or step back
        // from an indented hyphen line onto the column margin.
        && (-64.0..=36.0).contains(&shift)
        && prev.y > line.y
        && prev.y - line.y < prev.size.max(line.size).max(1.0) * 1.6
}

/// `83 Generating` on one baseline is the next contents entry, not a sentence.
/// A bare page number only: `1:` and `(3)` stay with the prose around them.
fn inline_contents_entry(text: &str) -> bool {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    tokens.windows(2).any(|pair| {
        (1..=3).contains(&pair[0].len())
            && pair[0].bytes().all(|byte| byte.is_ascii_digit())
            && pair[1].chars().count() >= 3
            && pair[1].starts_with(|ch: char| ch.is_ascii_uppercase())
    })
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
    fn a_narrow_contents_crumb_stays_with_the_rest_of_its_line() {
        // The bullet between "83" and the next entry is a drawing, so the
        // text gap is ~14pt. That must not peel "client 83" off the line.
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
        let segs = segment_glyphs(&glyphs);
        let texts: Vec<_> = segs.iter().map(|segment| segment.text.as_str()).collect();
        assert!(!texts.contains(&"client 83 results 83"), "{texts:?}");
        assert!(
            texts.iter().any(|text| {
                text.contains("client") && text.contains("Generating") && !text.contains("Scraping")
            }),
            "{texts:?}"
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
        let segs = segment_glyphs(&[lead, rest, other]);
        let texts: Vec<_> = segs.iter().map(|seg| seg.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("all evaluated proactive methods")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("mono label") && !text.contains("Proactive")),
            "{texts:?}"
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
            wide(4, 72.0, 500.0, body, 360.0, 10.0),
        ];
        let seg = segment_with(&glyphs, &SegmentFlags::default());
        let texts: Vec<_> = seg.segments.iter().map(|item| item.text.as_str()).collect();
        assert!(
            texts.iter().any(|text| text.starts_with("Table 1:")),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("Method")),
            "{texts:?}"
        );
        let kept: Vec<_> = seg.kept.iter().map(|(id, _)| *id).collect();
        assert!(kept.contains(&1) && kept.contains(&2), "{:?}", seg.kept);
        assert!(seg.kept.iter().all(|(_, reason)| reason == "table"));
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
        let kept: std::collections::HashSet<u32> = seg.kept.iter().map(|(id, _)| *id).collect();
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
        assert!(kept.contains(&cell.id), "table header should stay original");
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
        let margin_ids: std::collections::HashSet<u32> = seg
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
        let intro_pages: std::collections::HashSet<u32> = intro
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
    fn roman_page_numbers_are_folios() {
        assert!(is_roman_numeral("xvi"));
        assert!(is_roman_numeral("XVIII"));
        assert!(is_roman_numeral("xxvi"));
        assert!(!is_roman_numeral("did"));
        assert!(!is_roman_numeral("civil"));
    }
}
