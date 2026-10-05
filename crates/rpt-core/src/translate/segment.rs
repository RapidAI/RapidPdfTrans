//! Group extracted glyphs into translation segments.
//!
//! A segment is one paragraph: visual lines in the same column, joined so the
//! translator sees the whole block and the rewrite can reflow it inside that
//! block. Superscripts and footnote marks are attached to the line they sit
//! on. Figure interiors and table cells are not segments; only their captions
//! are. Unmapped glyphs stay out of every segment.

use crate::glyph::Glyph;

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
    let lines = raw_lines(glyphs);
    assemble(lines, flags)
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
            lines.extend(split_columns(std::mem::take(&mut current)));
        }
        // A jump back to the left is the other column on a nearby baseline.
        // Same-baseline columns are split after the line is collected: a fixed
        // gap would also cut justified word spaces.
        let left = glyph.bbox[0].min(glyph.bbox[2]);
        let jumped_back = current.last().is_some_and(|prev| {
            let prev_left = prev.bbox[0].min(prev.bbox[2]);
            prev_left - left > 24.0
        });
        if jumped_back && !current.is_empty() {
            lines.extend(split_columns(std::mem::take(&mut current)));
        }
        last_page = Some(glyph.page_index);
        last_y = Some(glyph.matrix[5]);
        last_size = glyph.font_size.max(1.0);
        current.push(glyph);
    }
    if !current.is_empty() {
        lines.extend(split_columns(current));
    }
    lines
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
        if gaps[index - 1] > trigger && !current.is_empty() {
            parts.push(std::mem::take(&mut current));
        }
        current.push(glyph);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
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
}

fn assemble(raw: Vec<Vec<&Glyph>>, flags: &SegmentFlags) -> Segmentation {
    let mut lines: Vec<VisualLine> = raw.into_iter().map(visual_line).collect();
    attach_markers(&mut lines);
    let mut kept = interior_glyphs(&lines, flags);
    kept.extend(margin_stamps(&lines));
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
    let paragraphs = join_paragraphs(lines);
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
        for (index, line) in para.iter().enumerate() {
            let piece = if index == 0 {
                line.text.clone()
            } else if let Some(stem) = soft_hyphen_stem(&buf) {
                let rest = line.text.trim_start();
                if rest.starts_with(|ch: char| ch.is_ascii_lowercase()) {
                    format!("{stem}{rest}")
                } else {
                    format!("{} {}", buf.trim_end(), line.text.trim_start())
                }
            } else {
                format!("{} {}", buf.trim_end(), line.text.trim_start())
            };
            if index > 0 && piece.chars().count() > LONG_LINE && sentence_end(&buf) {
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
    }
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
            if word_space(prev, glyph) && !is_marker_text(&glyph.unicode) {
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
    body_font.is_empty() || line.font == body_font
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
        if let Some(column) = columns.iter_mut().find(|column| {
            column_anchor(column, line.page)
                .is_some_and(|anchor| (anchor - line.left).abs() <= 28.0)
        }) {
            column.push(line);
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
    if upper.page != lower.page {
        return false;
    }
    // A display formula keeps its whole segment. Joining it onto the prose
    // around it would leave that prose untranslated. A times sign or a
    // decimal point in an otherwise ordinary sentence does not.
    if line_has_formula(upper) || line_has_formula(lower) {
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
    if !same_font_family(&upper.font, &lower.font) {
        return false;
    }
    let hyphen = soft_hyphen_stem(&upper.text).is_some()
        && lower
            .text
            .trim_start()
            .starts_with(|ch: char| ch.is_ascii_lowercase());
    if (upper.left - lower.left).abs() > 36.0 && !hyphen {
        return false;
    }
    if hyphen {
        return true;
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
    line.glyphs.iter().any(|glyph| {
        let upper = glyph.font_name.to_ascii_uppercase();
        let math = ["CMMI", "CMSY", "CMEX", "MSAM", "MSBM", "STIX"]
            .iter()
            .any(|needle| upper.contains(needle))
            || upper.contains("MATH");
        math && !matches!(
            glyph.unicode.trim(),
            "*" | "∗" | "†" | "‡" | "§" | "¶" | "⋆" | "#"
        ) && !is_inline_math_symbol(&glyph.unicode)
    })
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
    fn an_inline_formula_line_does_not_swallow_neighboring_prose() {
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
            texts
                .iter()
                .any(|text| text.starts_with("This sentence") && !text.contains("following")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|text| text.starts_with("The following")),
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
