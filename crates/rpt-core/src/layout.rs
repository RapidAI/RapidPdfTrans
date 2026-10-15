//! Deterministic CJK typesetting.
//!
//! Paragraph detection is [`crate::segment`]. This module indents, wraps,
//! and justifies text that is already in the target language. It does not
//! open a socket and it does not accept a model client.
//!
//! [`fit_paragraph`] is the entry point: pass the translated string, the
//! source box, and a subset font. [`crate::rewrite_translation`] draws the
//! fitted lines into the PDF, including bilingual page layouts. Both stay
//! callable when no model is configured.

use crate::font::SubsetFont;
use crate::translate::verbatim_spans;

/// How Chinese body text is sized relative to the English it replaces.
///
/// CJK glyphs fill the em square, so the same point size looks heavier than
/// Latin, and the English baseline gap then feels cramped. Body text starts
/// smaller and uses a Chinese academic leading; the fit loop may shrink the
/// size further, but it keeps that leading ratio.
#[derive(Clone, Copy, Debug)]
pub struct CjkMeasure {
    pub body_scale: f32,
    pub subsection_scale: f32,
    pub section_scale: f32,
    pub heading_scale: f32,
    pub leading_ratio: f32,
    pub min_leading_ratio: f32,
}

impl CjkMeasure {
    pub fn resolve(size_scale: f32, leading: f32) -> Self {
        let body = positive_or_env(size_scale, "RPT_CJK_SIZE_SCALE", 0.90).clamp(0.65, 1.05);
        let leading = positive_or_env(leading, "RPT_CJK_LEADING", 1.60).clamp(1.25, 2.20);
        Self {
            body_scale: body,
            // A subsection is often small-caps at the body size. Chinese still
            // needs it larger than the shrunk body, and a section larger than that.
            subsection_scale: (body + 0.12).min(1.05).max(body),
            section_scale: (body + 0.06).clamp(0.96, 1.0).max(body),
            heading_scale: (body + 0.06).min(1.0),
            leading_ratio: leading,
            min_leading_ratio: (leading * 0.88).max(1.40).min(leading),
        }
    }
}

/// How a source line should be ranked once it is set in Chinese.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeadingLevel {
    Body,
    Subsection,
    Section,
    Title,
}

/// Rank a paragraph from its source size and the translated text.
///
/// Numbered heads such as `2.2` stay subsections even when the English face
/// is the same size as the body. A section number at a larger size stays
/// above that. Captions are body for this ranking; indent treats them apart.
pub fn heading_level(line_count: usize, source_size: f32, bold: bool, text: &str) -> HeadingLevel {
    let trimmed = text.trim();
    if trimmed.is_empty() || is_cjk_caption(trimmed) {
        return HeadingLevel::Body;
    }
    // A heading is a title, not the middle of a sentence. `最相关的` and a
    // trailing comma are a list line that was split off its continuation.
    if ends_mid_sentence(trimmed) {
        return HeadingLevel::Body;
    }
    // `1 The agent sends…` plus the hanging lines under it is one body item.
    // `1 Introduction` is a single heading line and stays a section.
    if line_count >= 2 && crate::segment::numbered_item_body(trimmed).is_some() {
        return HeadingLevel::Body;
    }
    let chars = trimmed.chars().count();
    let sentence = ends_sentence(trimmed);
    if source_size >= 14.0 && line_count <= 3 && chars < 160 {
        return HeadingLevel::Title;
    }
    // "1新加坡管理大学2南洋理工大学" is an affiliation line, not section 1.
    let affiliation = affiliation_marks(trimmed) >= 2;
    match numbered_depth(trimmed) {
        // `1 Introduction` is a section. `20 鲍德温路` is a street number at
        // body size; lifting it makes the address larger than the body.
        Some(1)
            if !affiliation
                && line_count <= 2
                && chars < 80
                && !sentence
                && !(street_number(trimmed) && !bold && source_size < 12.0) =>
        {
            return HeadingLevel::Section;
        }
        Some(depth) if depth >= 2 && line_count <= 2 && chars < 100 && !sentence => {
            return HeadingLevel::Subsection;
        }
        _ => {}
    }
    if line_count == 1 && bold && chars < 80 && !sentence {
        return HeadingLevel::Subsection;
    }
    if source_size >= 12.5 && line_count <= 3 && chars < 120 && !sentence {
        return HeadingLevel::Title;
    }
    if line_count == 1 && source_size >= 11.2 && chars < 24 && !sentence {
        return HeadingLevel::Section;
    }
    HeadingLevel::Body
}

pub(crate) fn scale_for_heading(level: HeadingLevel, source_size: f32, metrics: CjkMeasure) -> f32 {
    let source_size = source_size.max(1.0);
    let target = match level {
        HeadingLevel::Body => source_size * metrics.body_scale,
        // Small-caps headings are stored below the body size. Lift them off a
        // 10pt body so a subsection stays larger than Chinese body text.
        HeadingLevel::Subsection => {
            let floor = 10.0 * metrics.subsection_scale;
            floor.max(source_size * metrics.subsection_scale)
        }
        HeadingLevel::Section => {
            if source_size >= 12.0 {
                source_size * metrics.section_scale
            } else {
                let floor = 10.0 * (metrics.subsection_scale + 0.14);
                floor.max(source_size * metrics.section_scale)
            }
        }
        HeadingLevel::Title => source_size * metrics.heading_scale,
    };
    (target / source_size).clamp(metrics.body_scale, 1.35)
}

/// Digit runs glued to a name: `1新加坡` or `张轩1`. Two or more means an
/// affiliation line rather than a numbered heading.
fn affiliation_marks(text: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut marks = 0usize;
    let mut index = 0;
    while index < chars.len() {
        if !chars[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len() && chars[index].is_ascii_digit() {
            index += 1;
        }
        if index - start > 2 {
            continue;
        }
        let before = start > 0 && chars[start - 1].is_alphabetic();
        let after = index < chars.len() && chars[index].is_alphabetic();
        if before || after {
            marks += 1;
        }
    }
    marks
}

/// `20 Baldwin Road` translated as `20 鲍德温路`. A one-digit outline index
/// (`1 Introduction`) is not a street number.
fn street_number(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    index >= 2 && bytes.get(index) == Some(&b' ')
}

/// `1` for `1 Introduction`, `2` for `2.2 Methods`.
fn numbered_depth(text: &str) -> Option<u8> {
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut groups = 0u8;
    loop {
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index == start || index - start > 2 {
            return None;
        }
        groups = groups.saturating_add(1);
        if index < bytes.len() && bytes[index] == b'.' {
            let after = index + 1;
            if after < bytes.len() && bytes[after].is_ascii_digit() {
                index = after;
                continue;
            }
        }
        break;
    }
    if groups > 2 || index > bytes.len() {
        return None;
    }
    if index == bytes.len() {
        return Some(groups);
    }
    if bytes[index] == b' ' {
        return Some(groups);
    }
    // Chinese often drops the space: "2.2裁判引导".
    let rest = text.get(index..)?;
    rest.chars()
        .next()
        .filter(|ch| !ch.is_ascii())
        .map(|_| groups)
}

fn positive_or_env(explicit: f32, key: &str, builtin: f32) -> f32 {
    if explicit > 0.0 {
        return explicit;
    }
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<f32>().ok())
        .filter(|value| *value > 0.0)
        .unwrap_or(builtin)
}

/// One fitted paragraph: wrapped lines, point size, leading, and first-line indent.
#[derive(Clone, Debug, PartialEq)]
pub struct FittedParagraph {
    pub lines: Vec<String>,
    pub size: f32,
    pub leading: f32,
    /// First-line indent in user space. Body text uses two ems of `size`.
    pub indent: f32,
}

/// Fit already-translated text into a paragraph box.
///
/// `line_count` and `bold` describe the source paragraph so a heading stays
/// flush and body text gets a two-em indent. `width` is the column measure
/// and `available` is the source baseline span (top line minus bottom line).
pub fn fit_paragraph(
    text: &str,
    source_size: f32,
    width: f32,
    available: f32,
    line_count: usize,
    bold: bool,
    font: &SubsetFont,
    metrics: CjkMeasure,
) -> Option<FittedParagraph> {
    let level = heading_level(line_count, source_size, bold, text);
    let indent_ems = cjk_indent_ems(line_count, source_size, bold, text);
    let scale = scale_for_heading(level, source_size, metrics);
    let (lines, size, leading, indent) = fit_cjk_block(
        text,
        source_size,
        scale,
        width,
        indent_ems,
        available,
        font,
        metrics,
    )?;
    Some(FittedParagraph {
        lines,
        size,
        leading,
        indent,
    })
}

/// Two ems for a body or abstract paragraph. Zero for a heading or caption.
pub fn cjk_indent_ems(line_count: usize, source_size: f32, bold: bool, text: &str) -> f32 {
    if is_cjk_caption(text)
        || heading_level(line_count, source_size, bold, text) != HeadingLevel::Body
        // A one-line bullet is not a paragraph. Indenting it slides the
        // marker (` You know how to create…` moved 18pt right of its sibling).
        || crate::segment::starts_with_list_marker(text)
    {
        0.0
    } else if line_count >= 2 || is_one_line_prose(text, source_size, bold) {
        2.0
    } else {
        0.0
    }
}

fn is_one_line_prose(text: &str, source_size: f32, bold: bool) -> bool {
    if bold || source_size >= 12.5 || text.contains('@') {
        return false;
    }
    let trimmed = text.trim();
    let letters = trimmed.chars().filter(|ch| ch.is_alphabetic()).count();
    letters >= 24 && ends_sentence(trimmed)
}

fn ends_sentence(text: &str) -> bool {
    text.ends_with('.')
        || text.ends_with('。')
        || text.ends_with('!')
        || text.ends_with('?')
        || text.ends_with('？')
        || text.ends_with('！')
}

/// `的` and `，` close a clause, not a heading.
fn ends_mid_sentence(text: &str) -> bool {
    text.ends_with('的') || text.ends_with('，') || text.ends_with(',')
}

fn is_cjk_caption(text: &str) -> bool {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("figure")
        || lower.starts_with("fig.")
        || lower.starts_with("fig ")
        || lower.starts_with("table")
        || lower.starts_with("tab.")
        || lower.starts_with("tab ")
    {
        return true;
    }
    let mut chars = trimmed.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first != '图' && first != '表' {
        return false;
    }
    let rest: String = chars.collect();
    rest.trim_start()
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_digit() || ch == '：' || ch == ':')
}

/// Fit Chinese into the source block.
///
/// Body text starts below the English point size. Leading stays near a
/// Chinese academic ratio (about 1.6 em) and the size shrinks before that
/// ratio collapses back to the English baseline gap.
pub(crate) fn fit_cjk_block(
    text: &str,
    source_size: f32,
    scale: f32,
    width: f32,
    indent_ems: f32,
    available: f32,
    font: &SubsetFont,
    metrics: CjkMeasure,
) -> Option<(Vec<String>, f32, f32, f32)> {
    fit_cjk_block_with(
        text,
        source_size,
        scale,
        width,
        indent_ems,
        available,
        font,
        metrics,
        None,
    )
}

/// Like [`fit_cjk_block`], but `fixed_indent` is a point offset that does not
/// shrink when the type size does. A callout body starts at the source x.
pub(crate) fn fit_cjk_block_with(
    text: &str,
    source_size: f32,
    scale: f32,
    width: f32,
    indent_ems: f32,
    available: f32,
    font: &SubsetFont,
    metrics: CjkMeasure,
    fixed_indent: Option<f32>,
) -> Option<(Vec<String>, f32, f32, f32)> {
    let start = (source_size * scale).max(1.0);
    // A paragraph that already fits stays at `start`. A narrow column, or a
    // one-line source, may shrink to 0.62 of the English size before the
    // English is left in place. A short one-line title stays at least 0.80
    // and may use a second line. Half the source size is too small to read.
    // Two characters in a folio-sized box (`十四` for `xiv`) must stay one
    // line. Wrapping them at the 0.80 floor leaves a one-character line.
    let chars_n = text.chars().filter(|ch| !ch.is_whitespace()).count();
    let compact = chars_n <= 4;
    // A whole-line personal name cannot break. Shrink it onto one line.
    // Other display text (a cover subtitle) stays near the source size and
    // may use a second line. Nine-point type on a dark panel cannot be read.
    let name_chars: Vec<char> = text.chars().collect();
    let name_sticky = sticky_after(&name_chars);
    let unbreakable = name_chars.len() > 1
        && name_sticky.len() + 1 == name_chars.len()
        && name_sticky.iter().all(|flag| *flag);
    let display = !unbreakable && available <= 0.5 && source_size >= 16.0 && chars_n <= 40;
    let floor_ratio = if unbreakable {
        // The name stays one line. 0.50 still clears the panel; a split
        // `R` / `oberto` is worse than a slightly smaller line.
        0.50
    } else if display && compact {
        // `Hi!` is one short word. `你好!` at 0.80 wraps to one character
        // per line. Half the source size still reads on a title slide.
        0.50
    } else if display {
        0.80
    } else if compact && available <= 0.5 {
        0.62
    } else if available <= 0.5 && width <= source_size * 16.0 {
        0.80
    } else {
        0.62
    };
    let mut floor = (source_size * floor_ratio).min(start);
    let mut size = start;
    // The first two-line fit near the source size. A one-line fit found
    // later still wins. Shrinking a subtitle to one unreadably small line
    // does not.
    let mut kept_display: Option<(Vec<String>, f32, f32, f32)> = None;
    loop {
        let indent = fixed_indent.unwrap_or(size * indent_ems);
        let first_width = (width - indent).max(size * 0.5);
        // A narrow column can need more lines than `wrap_text` will count at
        // the starting size. That is a reason to shrink, not to keep the English.
        let Some(mut lines) = wrap_text(text, size, first_width, width, font) else {
            if size <= floor + 0.01 {
                return None;
            }
            let next = (size * 0.94).max(floor);
            if (next - size).abs() < 0.01 {
                return None;
            }
            size = next;
            continue;
        };
        // A one-character last line (`组` / `件`) is an orphan. Pull one
        // character down so the tail is at least two characters. The last
        // line was one em wide, so two characters still fit the measure.
        absorb_one_char_tail(&mut lines);
        // `使用 LangChain 摘` / `要文本` cuts 摘要 because one more CJK
        // character still fit after the Latin word. A display title moves
        // that short tail down so the word stays whole.
        if source_size >= 16.0 {
            rejoin_cjk_tail_after_latin(&mut lines, size, width, first_width, font);
        }
        let gaps = lines.len().saturating_sub(1);
        let within = lines.iter().enumerate().all(|(index, line)| {
            let limit = if index == 0 { first_width } else { width };
            fit_measure(line, size, font) <= limit + 1.0
        });
        let single_source = available <= 0.5;
        let tail = lines
            .last()
            .map(|line| line.chars().filter(|ch| !ch.is_whitespace()).count())
            .unwrap_or(0);
        // A display title must not end on one character (`提示` / `词`).
        // `display` is only the one-line-source case. A two-line chapter
        // title has leading between the baselines, and the same orphan.
        // Two characters (`示` / `词`) still splits 提示词. A short display
        // title has to stay on one line.
        let short_title_tail = source_size >= 16.0 && chars_n <= 24 && lines.len() > 1 && tail < 3;
        if display && within && lines.len() == 2 && kept_display.is_none() && !short_title_tail {
            kept_display = Some((lines.clone(), size, size * 1.05, indent));
        }
        if within && (!single_source || lines.len() == 1) && !short_title_tail {
            if gaps == 0 {
                return Some((lines, size, size * metrics.leading_ratio, indent));
            }
            let room = available / gaps as f32;
            let min_lead = size * metrics.min_leading_ratio;
            let want = size * metrics.leading_ratio;
            if room + 0.8 >= min_lead {
                return Some((lines, size, want.min(room), indent));
            }
        }
        if size <= floor + 0.01 {
            if let Some(saved) = kept_display {
                return Some(saved);
            }
            if display && within && (1..=3).contains(&lines.len()) {
                let lead = if lines.len() == 1 {
                    size * metrics.leading_ratio
                } else {
                    size * 1.05
                };
                return Some((lines, size, lead, indent));
            }
            // A one-line title that still does not fit at 0.80 wraps once.
            // Shrinking it to half the source size is too small to read.
            // A two-character tail (`澳大` / `利亚`) is not a title wrap: it
            // lands on the next source line. Shrink further and stay on one line.
            let tail = lines
                .last()
                .map(|line| line.chars().filter(|ch| !ch.is_whitespace()).count())
                .unwrap_or(0);
            if within
                && single_source
                && width <= source_size * 16.0
                && lines.len() == 2
                && size + 0.01 >= source_size * 0.80
                && tail >= 4
            {
                // 1.15 em drops the second line into the next heading.
                return Some((lines, size, size * 1.0, indent));
            }
            if within && !single_source && gaps > 0 && !short_title_tail {
                let room = available / gaps as f32;
                if room >= size * 1.05 {
                    return Some((lines, size, room, indent));
                }
            }
            if within && lines.len() == 1 {
                return Some((lines, size, size * metrics.leading_ratio, indent));
            }
            let orphan = within && single_source && lines.len() == 2 && tail < 4;
            if !(orphan && floor > source_size * 0.62 + 0.01) {
                return None;
            }
            floor = source_size * 0.62;
        }
        let next = (size * 0.94).max(floor);
        if (next - size).abs() < 0.01 {
            return None;
        }
        size = next;
    }
}

/// Extra space between two CJK ideographs, as a fraction of the em.
/// A larger cap (`0.15`) still tracked `对于任意` into `对 于 任 意`. A line
/// that cannot be squared inside this hairline stays ragged.
const MAX_JUSTIFY_EM: f32 = 0.05;

/// Extra space goes only between CJK characters. A single `Tc` would letter-space
/// the Latin citation on the same line. The extra never exceeds [`MAX_JUSTIFY_EM`];
/// a line that is still short breaks a long Latin token instead of stretching.
pub(crate) fn justify_gaps(
    text: &str,
    size: f32,
    width: f32,
    font: &SubsetFont,
    justify: bool,
) -> Vec<f32> {
    let chars: Vec<char> = text.chars().collect();
    let mut gaps = vec![0.0f32; chars.len().saturating_sub(1)];
    if !justify || chars.len() < 2 || size <= 0.0 {
        return gaps;
    }
    let slots: Vec<usize> = (0..chars.len() - 1)
        .filter(|&index| is_cjk_body(chars[index]) && is_cjk_body(chars[index + 1]))
        .collect();
    if slots.is_empty() {
        return gaps;
    }
    if line_skips_justification(text) {
        return gaps;
    }
    let slack = width - fit_measure(text, size, font);
    // A line that already fills the measure, or one that is mostly empty
    // (a short label), stays as it is. Anything wider than the cap is a
    // wrap problem: the line should have taken another word or a camelCase
    // break, not opened a river between ideographs.
    // The cap cannot square a line that is still short by more than one
    // sixth of an em per pair. Spacing those characters only opens rivers
    // (`关 系。此 后`) and the tail stays short. Leave the line ragged.
    let cap = slots.len() as f32 * size * MAX_JUSTIFY_EM;
    if slack < 0.4 || slack > cap + 0.4 {
        return gaps;
    }
    let extra = (slack / slots.len() as f32).min(size * MAX_JUSTIFY_EM);
    if extra < 0.05 {
        return gaps;
    }
    for index in slots {
        gaps[index] = extra;
    }
    gaps
}

/// The last line of a paragraph is not justified. The last or only line of an
/// enumerated item is not either: `股票2：` stays on its own line ahead of
/// `股票3：`, and stretching that short line only letter-spaces it.
pub(crate) fn justify_wrapped_line(lines: &[String], index: usize) -> bool {
    let Some(next) = lines.get(index + 1) else {
        return false;
    };
    let chars: Vec<char> = next.chars().collect();
    !enumerated_item_at(&chars, 0)
}

/// A prompt line that still ends short of the measure is a standalone line.
/// The justification cap cannot square it, so the extra space would only
/// pull the characters apart.
pub(crate) fn prompt_line_is_full(text: &str, size: f32, width: f32, font: &SubsetFont) -> bool {
    let slack = width - fit_measure(text, size, font);
    if slack <= 0.4 {
        return true;
    }
    let pairs = cjk_pair_count(text) as f32;
    pairs > 0.0 && slack <= pairs * size * MAX_JUSTIFY_EM + 0.4
}

fn line_skips_justification(text: &str) -> bool {
    if text.contains('@') || text.to_ascii_lowercase().contains("et al") {
        return true;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut sticky = vec![false; chars.len().saturating_sub(1)];
    mark_citations(&chars, &mut sticky);
    sticky.into_iter().any(|gap| gap)
}

/// `组件` must not end as `组` / `件`. One CJK character on the last line
/// moves down with the character before it.
/// A display line that ends `LangChain 摘` with `要文本` on the next line
/// has room for one CJK character and uses it to cut a word. Move that
/// one- or two-character tail onto the next line when both lines still fit.
fn rejoin_cjk_tail_after_latin(
    lines: &mut [String],
    size: f32,
    width: f32,
    first_width: f32,
    font: &SubsetFont,
) {
    if lines.len() < 2 {
        return;
    }
    for index in 0..lines.len() - 1 {
        let chars: Vec<char> = lines[index].chars().collect();
        let mut cut = chars.len();
        while cut > 0 && is_cjk_body(chars[cut - 1]) {
            cut -= 1;
        }
        let run = chars.len() - cut;
        if run == 0 || run > 2 || cut == 0 {
            continue;
        }
        let prev = chars[cut - 1];
        if !(prev.is_whitespace() || prev.is_ascii_alphanumeric()) {
            continue;
        }
        if !lines[index + 1].chars().next().is_some_and(is_cjk_body) {
            continue;
        }
        let moved: String = chars[cut..].iter().collect();
        let left: String = chars[..cut].iter().collect();
        let left = left.trim_end().to_string();
        if left.is_empty() {
            continue;
        }
        let right = format!("{moved}{}", lines[index + 1]);
        let limit = if index == 0 { first_width } else { width };
        if fit_measure(&left, size, font) > limit + 1.0
            || fit_measure(&right, size, font) > width + 1.0
        {
            continue;
        }
        lines[index] = left;
        lines[index + 1] = right;
    }
}

fn absorb_one_char_tail(lines: &mut [String]) {
    if lines.len() < 2 {
        return;
    }
    let last_index = lines.len() - 1;
    let last_cjk = lines[last_index]
        .chars()
        .filter(|ch| is_cjk_body(*ch))
        .count();
    if last_cjk == 0 || last_cjk >= 2 {
        return;
    }
    let prev: Vec<char> = lines[last_index - 1].chars().collect();
    if prev.len() < 3 {
        return;
    }
    let moved = prev[prev.len() - 1];
    if !is_cjk_body(moved) {
        return;
    }
    lines[last_index - 1] = prev[..prev.len() - 1].iter().collect();
    let tail = lines[last_index].clone();
    lines[last_index] = format!("{moved}{tail}");
}

pub(crate) fn is_cjk_body(ch: char) -> bool {
    matches!(
        ch,
        '\u{3040}'..='\u{30FF}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{AC00}'..='\u{D7AF}'
    )
}

pub(crate) fn char_widths(text: &str, size: f32, font: &SubsetFont) -> Vec<f32> {
    text.chars()
        .map(|ch| {
            font.glyphs
                .get(&(ch as u32))
                .map(|(_, advance)| *advance as f32 * size / font.units_per_em as f32)
                .unwrap_or(size)
        })
        .collect()
}

pub(crate) fn measure(text: &str, size: f32, font: &SubsetFont) -> f32 {
    text.chars()
        .map(|ch| {
            font.glyphs
                .get(&(ch as u32))
                .map(|(_, advance)| *advance as f32 * size / font.units_per_em as f32)
                .unwrap_or(size)
        })
        .sum()
}

pub(crate) fn cids_of(text: &str, font: &SubsetFont) -> Vec<u16> {
    text.chars()
        .filter_map(|ch| font.glyphs.get(&(ch as u32)).map(|(gid, _)| *gid))
        .collect()
}

/// A short translation still has to meet every source line. Split the longest
/// wrapped lines until there is one piece per source baseline. Pieces only get
/// shorter, so a line that already fit the measure still does.
pub(crate) fn spread_to_lines(mut lines: Vec<String>, count: usize) -> Vec<String> {
    if count <= lines.len() || lines.is_empty() {
        return lines;
    }
    while lines.len() < count {
        let Some(index) = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| safe_split(line).is_some())
            .max_by_key(|(_, line)| line.chars().count())
            .map(|(index, _)| index)
        else {
            break;
        };
        let (left, right) = safe_split(&lines[index]).expect("filtered");
        lines.splice(index..=index, [left, right]);
    }
    lines
}

/// Split near the middle. Never inside a citation, number, URL, email, or
/// Latin word (`API`), and never so the next piece starts with `；`.
fn safe_split(line: &str) -> Option<(String, String)> {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() < 2 {
        return None;
    }
    let spans = verbatim_spans(line);
    let sticky = sticky_after(&chars);
    let indexed: Vec<(usize, char)> = line.char_indices().collect();
    let mid = chars.len() / 2;
    let mut best: Option<(usize, usize)> = None;
    for (index, (byte, _)) in indexed.iter().enumerate().skip(1) {
        if spans
            .iter()
            .any(|&(start, end)| *byte > start && *byte < end)
        {
            continue;
        }
        if !can_break_at(&chars, &sticky, index) {
            continue;
        }
        let dist = index.abs_diff(mid);
        if best.is_none_or(|(best_dist, _)| dist < best_dist) {
            best = Some((dist, *byte));
        }
    }
    let at = best?.1;
    let (left, right) = line.split_at(at);
    if left.is_empty() || right.is_empty() {
        None
    } else {
        Some((left.to_string(), right.to_string()))
    }
}

pub(crate) fn wrap_text(
    text: &str,
    size: f32,
    first_width: f32,
    max_width: f32,
    font: &SubsetFont,
) -> Option<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let sticky = sticky_after(&chars);
    let item_heads = (0..chars.len())
        .filter(|&index| enumerated_item_at(&chars, index))
        .count();
    let mut lines = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let limit = if lines.is_empty() {
            first_width
        } else {
            max_width
        };
        let mut end = start;
        let mut width = 0.0f32;
        let mut item_break = false;
        while end < chars.len() {
            // `股票2：` is the next enumerated item. It starts its own line
            // even when the previous item still has room on this measure.
            if item_heads >= 2 && end > start && enumerated_item_at(&chars, end) {
                item_break = true;
                break;
            }
            let advance = char_advance(chars[end], size, font);
            if end > start && width + advance > limit {
                // A closing comma or period may hang past the measure once.
                if hanging_punct(chars[end]) && width <= limit + 0.01 {
                    end += 1;
                }
                break;
            }
            width += advance;
            end += 1;
        }
        if end == start {
            end = (start + 1).min(chars.len());
        } else if end < chars.len() && !item_break {
            // Break at the rightmost legal point. Jumping back to the previous
            // ASCII space throws away CJK that already fit after a citation.
            let fitted = end;
            while end > start + 1 && !can_break_at(&chars, &sticky, end) {
                end -= 1;
            }
            if end == start + 1 {
                // The sticky span itself is wider than the measure. Fill the
                // line at an ordinary boundary instead of one character.
                end = fitted;
                while end > start + 1 && !can_break(chars[end - 1], chars[end]) {
                    end -= 1;
                }
            }
            // `FewShotPromptTemplate` did not fit after `包括`, so the line
            // stopped early and justification opened half an em between
            // ideographs. A camelCase boundary that still fits belongs on
            // this line. A URL stays sticky and is not cut.
            end = pull_latin_prefix(&chars, &sticky, start, end, fitted, limit, size, font);
            end = prefer_enumeration_break(&chars, &sticky, start, end);
        }
        let mut line_end = end;
        while line_end > start && chars[line_end - 1].is_whitespace() {
            line_end -= 1;
        }
        if line_end == start {
            start = end.max(start + 1);
            while start < chars.len() && chars[start].is_whitespace() {
                start += 1;
            }
            continue;
        }
        let line: String = chars[start..line_end].iter().collect();
        lines.push(line);
        start = end;
        while start < chars.len() && chars[start].is_whitespace() {
            start += 1;
        }
        // A full page is well under this. The cap only stops a degenerate
        // string; the fit loop shrinks when a real paragraph still exceeds it.
        if lines.len() > 96 {
            return None;
        }
    }
    let lines = move_opening_brackets(lines);
    Some(lines).filter(|lines| !lines.is_empty())
}

fn char_advance(ch: char, size: f32, font: &SubsetFont) -> f32 {
    font.glyphs
        .get(&(ch as u32))
        .map(|(_, advance)| *advance as f32 * size / font.units_per_em as f32)
        .unwrap_or(size)
}

fn fit_measure(text: &str, size: f32, font: &SubsetFont) -> f32 {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() > 1 && chars.last().is_some_and(|ch| hanging_punct(*ch)) {
        let body: String = chars[..chars.len() - 1].iter().collect();
        measure(&body, size, font)
    } else {
        measure(text, size, font)
    }
}

fn hanging_punct(ch: char) -> bool {
    matches!(
        ch,
        '，' | '。' | '、' | '；' | '：' | '！' | '？' | ',' | '.' | ';' | ':' | '!' | '?'
    )
}

fn sticky_after(chars: &[char]) -> Vec<bool> {
    let mut sticky = vec![false; chars.len().saturating_sub(1)];
    mark_citations(chars, &mut sticky);
    mark_emails(chars, &mut sticky);
    mark_urls(chars, &mut sticky);
    mark_dotted_names(chars, &mut sticky);
    mark_latin_name(chars, &mut sticky);
    mark_proper_names(chars, &mut sticky);
    mark_cjk_compounds(chars, &mut sticky);
    sticky
}

/// `词元` and `一个词` stay on one line. They can overlap (`其中一个词元`),
/// so every match is marked. A later break is used when the compound does
/// not fit.
fn mark_cjk_compounds(chars: &[char], sticky: &mut [bool]) {
    for word in CJK_COMPOUNDS {
        let piece: Vec<char> = word.chars().collect();
        if piece.len() < 2 || piece.len() > chars.len() {
            continue;
        }
        let mut index = 0;
        while index + piece.len() <= chars.len() {
            let end = index + piece.len();
            if chars[index..end] != piece[..] {
                index += 1;
                continue;
            }
            for boundary in index..end - 1 {
                if let Some(flag) = sticky.get_mut(boundary) {
                    *flag = true;
                }
            }
            index += 1;
        }
    }
}

/// `Manning Publications` is one name. It may move to the next line, but
/// the two words do not split and leave `Manning` alone.
fn mark_proper_names(chars: &[char], sticky: &mut [bool]) {
    let mut index = 0;
    while index < chars.len() {
        let Some(mut end) = capitalized_word(chars, index) else {
            index += 1;
            continue;
        };
        let start = index;
        let mut words = 1usize;
        loop {
            let mut gap = end;
            while gap < chars.len() && chars[gap].is_whitespace() {
                gap += 1;
            }
            if gap == end || gap - end > 1 {
                break;
            }
            let Some(next) = capitalized_word(chars, gap) else {
                break;
            };
            words += 1;
            end = next;
        }
        if words >= 2 {
            for boundary in start..end.saturating_sub(1) {
                if let Some(flag) = sticky.get_mut(boundary) {
                    *flag = true;
                }
            }
        }
        index = end.max(index + 1);
    }
}

fn capitalized_word(chars: &[char], start: usize) -> Option<usize> {
    let first = chars.get(start).copied()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    let mut end = start + 1;
    let mut lower = false;
    while end < chars.len() {
        let ch = chars[end];
        if ch.is_ascii_lowercase() {
            lower = true;
            end += 1;
            continue;
        }
        if ch.is_ascii_uppercase() {
            end += 1;
            continue;
        }
        if matches!(ch, '-' | '\'' | '’')
            && chars
                .get(end + 1)
                .is_some_and(|next| next.is_ascii_alphabetic())
        {
            end += 1;
            continue;
        }
        break;
    }
    if end - start >= 2 && (lower || chars[start..end].iter().all(|ch| ch.is_ascii_uppercase())) {
        Some(end)
    } else {
        None
    }
}

/// `股票2：` after the digits and the full-width colon. A bare `股票市场` is not an item.
fn enumerated_item_at(chars: &[char], index: usize) -> bool {
    if chars.get(index) != Some(&'股') || chars.get(index + 1) != Some(&'票') {
        return false;
    }
    let mut cursor = index + 2;
    if !chars.get(cursor).is_some_and(|ch| ch.is_ascii_digit()) {
        return false;
    }
    while chars.get(cursor).is_some_and(|ch| ch.is_ascii_digit()) {
        cursor += 1;
    }
    matches!(chars.get(cursor), Some('：'))
}

/// `使用 LangChain、LangGraph 和 MCP` breaks after `、`, not before `和`.
fn prefer_enumeration_break(chars: &[char], sticky: &[bool], start: usize, end: usize) -> usize {
    let mut look = end;
    while look < chars.len() && chars[look].is_whitespace() {
        look += 1;
    }
    if !matches!(chars.get(look).copied(), Some('和' | '与' | '及')) {
        return end;
    }
    let mut alt = end;
    while alt > start + 1 {
        alt -= 1;
        if chars[alt - 1] == '、' && can_break_at(chars, sticky, alt) && alt - start >= 4 {
            return alt;
        }
    }
    end
}

/// A missing space between Latin and CJK (`Publications知悉了`) is inserted.
/// Digits stay glued (`第4章`, `15%`). Superscript marks stay on their character.
pub(crate) fn space_cjk_latin_mask(text: &str, mask: &[bool]) -> (String, Vec<bool>) {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 4);
    let mut out_mask = Vec::with_capacity(chars.len() + 4);
    for (index, ch) in chars.iter().enumerate() {
        if index > 0 && cjk_latin_boundary(chars[index - 1], *ch) {
            out.push(' ');
            out_mask.push(false);
        }
        out.push(*ch);
        out_mask.push(mask.get(index).copied().unwrap_or(false));
    }
    (out, out_mask)
}

fn cjk_latin_boundary(prev: char, next: char) -> bool {
    (is_cjk_body(prev) && next.is_ascii_alphabetic())
        || (prev.is_ascii_alphabetic() && is_cjk_body(next))
}

/// `罗伯托·因凡特` is one name. A break after the dot leaves `凡特` alone.
fn mark_dotted_names(chars: &[char], sticky: &mut [bool]) {
    let mut index = 0;
    while index < chars.len() {
        if !is_name_dot(chars[index]) && !is_name_cjk(chars[index]) {
            index += 1;
            continue;
        }
        let start = index;
        let mut dots = 0usize;
        while index < chars.len() && (is_name_dot(chars[index]) || is_name_cjk(chars[index])) {
            if is_name_dot(chars[index]) {
                dots += 1;
            }
            index += 1;
        }
        if dots > 0 && index - start <= 12 {
            for boundary in start..index.saturating_sub(1) {
                if let Some(flag) = sticky.get_mut(boundary) {
                    *flag = true;
                }
            }
        }
    }
}

fn is_name_dot(ch: char) -> bool {
    matches!(ch, '·' | '•' | '・' | '‧' | '⋅')
}

fn is_name_cjk(ch: char) -> bool {
    ('\u{3400}'..='\u{9FFF}').contains(&ch)
}

/// `Roberto Infante` as the whole line stays on one line, including the space.
fn mark_latin_name(chars: &[char], sticky: &mut [bool]) {
    let text: String = chars.iter().collect();
    if !crate::translate::whole_line_is_personal_name(text.trim()) {
        return;
    }
    for flag in sticky.iter_mut() {
        *flag = true;
    }
}

fn mark_citations(chars: &[char], sticky: &mut [bool]) {
    let mut index = 0;
    while index < chars.len() {
        let close = match chars[index] {
            '(' => ')',
            '（' => '）',
            _ => {
                index += 1;
                continue;
            }
        };
        let Some(rel) = chars[index + 1..].iter().position(|ch| *ch == close) else {
            index += 1;
            continue;
        };
        let end = index + 1 + rel;
        if end - index < 80 {
            let inside: String = chars[index + 1..end].iter().collect();
            if parenthetical_is_citation(&inside) {
                for boundary in index..end {
                    if let Some(flag) = sticky.get_mut(boundary) {
                        *flag = true;
                    }
                }
            }
        }
        index = end + 1;
    }
}

/// `(Smith et al., 2020)` and `（王等，2020）` stay on one line. A year inside
/// a Chinese sentence (`（……在2000年代……）`) is prose and still wraps.
fn parenthetical_is_citation(inside: &str) -> bool {
    if inside.to_ascii_lowercase().contains("et al") {
        return true;
    }
    let digits = inside.chars().filter(|ch| ch.is_ascii_digit()).count();
    if digits < 4 {
        return false;
    }
    let cjk = inside.chars().filter(|ch| is_cjk_body(*ch)).count();
    cjk <= 12
}

fn mark_emails(chars: &[char], sticky: &mut [bool]) {
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '@' {
            index += 1;
            continue;
        }
        let mut left = index;
        while left > 0 && index - left < 80 {
            let prev = chars[left - 1];
            if is_addr_char(prev) || matches!(prev, '{' | '}' | ',' | ' ') {
                left -= 1;
            } else {
                break;
            }
        }
        let mut right = index;
        while right + 1 < chars.len() && right - index < 80 && is_addr_char(chars[right + 1]) {
            right += 1;
        }
        if right > index && left < index {
            for boundary in left..right {
                if let Some(flag) = sticky.get_mut(boundary) {
                    *flag = true;
                }
            }
        }
        index = right + 1;
    }
}

/// `https://mng.bz/rZlZ` stays on one line. A line that cannot finish the
/// URL takes the whole token onto the next line.
fn mark_urls(chars: &[char], sticky: &mut [bool]) {
    // Indexes are characters. A byte offset slips once CJK precedes the URL
    // and leaves `https://` free to break from its host.
    let mut index = 0usize;
    while index < chars.len() {
        let Some(len) = url_len_at(chars, index) else {
            index += 1;
            continue;
        };
        let end = index + len;
        if len > 4 {
            for boundary in index..end.saturating_sub(1) {
                if let Some(flag) = sticky.get_mut(boundary) {
                    *flag = true;
                }
            }
        }
        index = end.max(index + 1);
    }
}

fn url_len_at(chars: &[char], index: usize) -> Option<usize> {
    let rest = &chars[index..];
    let prefix = if has_prefix(rest, "https://") {
        8
    } else if has_prefix(rest, "http://") {
        7
    } else if has_prefix(rest, "www.") {
        4
    } else {
        return None;
    };
    let mut end = prefix;
    while end < rest.len() && is_url_char(rest[end]) {
        end += 1;
    }
    (end > prefix).then_some(end)
}

fn has_prefix(chars: &[char], needle: &str) -> bool {
    let needle: Vec<char> = needle.chars().collect();
    chars.len() >= needle.len()
        && chars[..needle.len()]
            .iter()
            .zip(&needle)
            .all(|(ch, want)| ch.to_ascii_lowercase() == *want)
}

fn is_url_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            ':' | '/' | '.' | '-' | '_' | '~' | '?' | '#' | '%' | '&' | '=' | '+' | '@'
        )
}

fn is_addr_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '+' | '%')
}

fn can_break_at(chars: &[char], sticky: &[bool], end: usize) -> bool {
    if end == 0 || end >= chars.len() {
        return true;
    }
    if sticky.get(end - 1).copied().unwrap_or(false) {
        return false;
    }
    can_break(chars[end - 1], chars[end])
}

/// ASCII letters and accented Latin (`Plücker`, `naïve`, `café`). CJK stays
/// a per-character break. `ü` is not ASCII, and treating it as a break split
/// `Plü` / `cker`.
fn is_latin_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || (ch.is_alphabetic() && !is_cjk_body(ch))
}

fn can_break(prev: char, next: char) -> bool {
    // Keep Latin words and hyphenated names on one line. CJK still breaks per character.
    if is_latin_word_char(prev) && (is_latin_word_char(next) || next == '-' || next == '\'') {
        return false;
    }
    if (prev == '-' || prev == '\'') && is_latin_word_char(next) {
        return false;
    }
    break_after(prev) && break_before(next)
}

/// `Few|Shot`, `Prompt|Template`, and `HTML|Parser`. A run of capitals stays
/// together until the last capital that introduces a lowercase tail.
fn camel_boundary(chars: &[char], index: usize) -> bool {
    if index == 0 || index >= chars.len() {
        return false;
    }
    let prev = chars[index - 1];
    let next = chars[index];
    if !prev.is_ascii_alphabetic() || !next.is_ascii_uppercase() {
        return false;
    }
    if prev.is_ascii_lowercase() {
        return true;
    }
    chars
        .get(index + 1)
        .is_some_and(|ch| ch.is_ascii_lowercase())
}

fn cjk_pair_count(text: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    chars
        .windows(2)
        .filter(|pair| is_cjk_body(pair[0]) && is_cjk_body(pair[1]))
        .count()
}

/// The legal break left the line short of what [`MAX_JUSTIFY_EM`] can close,
/// and the next token is a long camelCase word. Take the longest prefix that
/// still fits. Sticky spans (a URL, a citation) are not entered.
fn pull_latin_prefix(
    chars: &[char],
    sticky: &[bool],
    start: usize,
    end: usize,
    fitted: usize,
    limit: f32,
    size: f32,
    font: &SubsetFont,
) -> usize {
    if end <= start || end >= chars.len() || fitted <= end {
        return end;
    }
    // A URL or a citation is sticky. A camelCase identifier is not: the
    // boundary inside `Jupyter|Notebook` is the break that fills the line.
    // Entering a URL would cut `https://Ex.com/AbCd` at `Ab|Cd`.
    let head: String = chars[start..end].iter().collect();
    let slack = limit - measure(&head, size, font);
    let cap = cjk_pair_count(&head) as f32 * size * MAX_JUSTIFY_EM;
    if slack <= cap + 0.4 {
        return end;
    }
    let room = fitted.min(chars.len());
    let mut best = end;
    let mut width = measure(&head, size, font);
    let mut index = end;
    while index < room {
        if index > end && sticky.get(index - 1).copied().unwrap_or(false) {
            break;
        }
        if index > end && camel_boundary(chars, index) && width <= limit + 0.4 {
            best = index;
        }
        let advance = char_advance(chars[index], size, font);
        if width + advance > limit + 0.4 {
            break;
        }
        width += advance;
        index += 1;
    }
    // `Jupyter|Notebook` can fill the measure on the boundary itself. The
    // loop stops at `fitted`, so that break is not seen from inside.
    if index > end
        && index < chars.len()
        && camel_boundary(chars, index)
        && !sticky.get(index - 1).copied().unwrap_or(false)
        && width <= limit + 0.4
    {
        best = index;
    }
    best
}

fn break_before(ch: char) -> bool {
    !matches!(
        ch,
        '，' | '。'
            | '、'
            | '；'
            | '：'
            | '！'
            | '？'
            | '）'
            | '》'
            | '」'
            | '』'
            | '】'
            | ','
            | '.'
            | ';'
            | ':'
            | '!'
            | '?'
            | ')'
            | ']'
            | '}'
    )
}

fn break_after(ch: char) -> bool {
    !is_opening_bracket(ch)
}

/// An opening bracket is not a line ending. `思维链（` / `CoT）` leaves the
/// bracket hanging, and justification then opens a gap in front of it.
fn is_opening_bracket(ch: char) -> bool {
    matches!(
        ch,
        '（' | '［'
            | '｛'
            | '〈'
            | '《'
            | '「'
            | '『'
            | '【'
            | '〔'
            | '〖'
            | '〘'
            | '〚'
            | '｢'
            | '('
            | '['
            | '{'
            | '<'
    )
}

/// Move a trailing opener onto the next line. A line that is only the opener
/// joins that line instead of standing alone.
fn move_opening_brackets(mut lines: Vec<String>) -> Vec<String> {
    let mut index = 0;
    while index + 1 < lines.len() {
        let chars: Vec<char> = lines[index].chars().collect();
        let take = chars
            .iter()
            .rev()
            .take_while(|ch| is_opening_bracket(**ch))
            .count();
        if take == 0 {
            index += 1;
            continue;
        }
        let keep = chars.len() - take;
        let head: String = chars[..keep].iter().collect();
        let tail: String = chars[keep..].iter().collect();
        if head.trim().is_empty() {
            lines[index + 1] = format!("{tail}{}", lines[index + 1]);
            lines.remove(index);
            continue;
        }
        lines[index] = head;
        lines[index + 1] = format!("{tail}{}", lines[index + 1]);
        index += 1;
    }
    lines
}

/// One page's measure: how many source lines it can hold, and how wide they are.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LineSlots {
    pub width: f32,
    pub slots: usize,
}

/// Wrap `text` across page boxes. Every non-space character is placed, or the
/// fit is refused. A short first box must not keep a prefix and drop the tail.
pub(crate) fn pack_into_slots(
    text: &str,
    size: f32,
    indent: f32,
    boxes: &[LineSlots],
    font: &SubsetFont,
    snap_compounds: bool,
) -> Option<Vec<Vec<String>>> {
    if boxes.is_empty() || size <= 0.0 || text.trim().is_empty() {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut packed = Vec::with_capacity(boxes.len());
    let mut cursor = 0usize;
    let mut first_line = true;
    for (box_index, slot) in boxes.iter().enumerate() {
        while cursor < chars.len() && chars[cursor].is_whitespace() {
            cursor += 1;
        }
        if cursor >= chars.len() {
            packed.push(Vec::new());
            continue;
        }
        let width = slot.width.max(size);
        let first_width = if first_line {
            (width - indent).max(size * 0.5)
        } else {
            width
        };
        let remaining: String = chars[cursor..].iter().collect();
        let wrapped = wrap_text(&remaining, size, first_width, width, font)?;
        let take = wrapped.len().min(slot.slots.max(1));
        if take == 0 {
            return None;
        }
        let mut used = wrapped[..take].to_vec();
        let mut consumed = consumed_chars(&chars[cursor..], &used);
        if consumed == 0 {
            return None;
        }
        // A page break is not a place to cut 一个|词. Pull the rest of the
        // compound onto this page when it fits; otherwise start it on the next.
        if snap_compounds && box_index + 1 < boxes.len() && cursor + consumed < chars.len() {
            let last_limit = if used.len() == 1 && first_line {
                first_width
            } else {
                width
            };
            if retreat_compound_boundary(
                &mut used,
                &chars[cursor + consumed..],
                last_limit,
                size,
                font,
            ) {
                consumed = consumed_chars(&chars[cursor..], &used);
                if consumed == 0 {
                    return None;
                }
            }
        }
        cursor += consumed;
        packed.push(used);
        first_line = false;
    }
    while cursor < chars.len() && chars[cursor].is_whitespace() {
        cursor += 1;
    }
    if cursor < chars.len() {
        return None;
    }
    Some(packed)
}

/// A contents title whose last source line is only the wrapped tail
/// (`engines`, about three ems) must not receive half of the Chinese.
/// Pack one piece per source line, using that line's own width. A short
/// last line of a body paragraph is wider than this and stays on the
/// normal wrap.
pub(crate) fn pack_narrow_tail(
    text: &str,
    size: f32,
    indent: f32,
    spans: &[(f32, f32)],
    font: &SubsetFont,
) -> Option<Vec<String>> {
    if spans.len() < 2 || size <= 0.0 {
        return None;
    }
    let widths: Vec<f32> = spans
        .iter()
        .map(|(left, right)| (*right - *left).max(0.0))
        .collect();
    let widest = widths.iter().copied().fold(0.0f32, f32::max);
    let crumb = widths
        .iter()
        .any(|width| *width + 0.5 < size * 4.0 && *width < widest * 0.5);
    if !crumb {
        return None;
    }
    let boxes: Vec<LineSlots> = widths
        .iter()
        .map(|width| LineSlots {
            width: (*width).max(size),
            slots: 1,
        })
        .collect();
    let packed = pack_into_slots(text, size, indent, &boxes, font, false)?;
    let mut lines: Vec<String> = packed
        .into_iter()
        .map(|slot| slot.into_iter().next().unwrap_or_default())
        .collect();
    // The wide line can hold the whole title, which used to leave the crumb
    // empty and the page number (`4`) as the first ink on that baseline.
    // A wrapped English word is about two CJK characters; put those back.
    donate_crumb_suffix(&mut lines, &widths, size, font);
    if lines.iter().all(|line| line.trim().is_empty()) {
        return None;
    }
    Some(lines)
}

fn donate_crumb_suffix(lines: &mut [String], widths: &[f32], size: f32, font: &SubsetFont) {
    for index in 1..lines.len() {
        if !lines[index].trim().is_empty() {
            continue;
        }
        let width = widths[index];
        if width + 0.5 >= size * 4.0 {
            continue;
        }
        let Some(source) = (0..index)
            .rev()
            .find(|&slot| !lines[slot].trim().is_empty())
        else {
            continue;
        };
        let Some((prefix, suffix)) = peel_cjk_suffix(&lines[source], width, size, font) else {
            continue;
        };
        lines[source] = prefix;
        lines[index] = suffix;
    }
}

/// Up to two trailing CJK characters, when they fit the crumb and leave a prefix.
fn peel_cjk_suffix(
    line: &str,
    width: f32,
    size: f32,
    font: &SubsetFont,
) -> Option<(String, String)> {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() < 3 || width <= 0.0 {
        return None;
    }
    let mut count = 0usize;
    while count < chars.len() && count < 2 && is_cjk_body(chars[chars.len() - 1 - count]) {
        count += 1;
    }
    while count > 0 {
        let suffix: String = chars[chars.len() - count..].iter().collect();
        if measure(&suffix, size, font) <= width + 0.5 {
            break;
        }
        count -= 1;
    }
    if count == 0 {
        return None;
    }
    let prefix: String = chars[..chars.len() - count].iter().collect();
    if prefix.trim().is_empty() {
        return None;
    }
    let suffix: String = chars[chars.len() - count..].iter().collect();
    Some((prefix, suffix))
}

/// Compounds that stay on one side of a page break. Longest match wins, so
/// `一个词` is not cut as `一个` plus `词`.
const CJK_COMPOUNDS: &[&str] = &[
    "一个词",
    "上下文",
    "一个",
    "一种",
    "一些",
    "一次",
    "一位",
    "一条",
    "一本",
    "这个",
    "这种",
    "这些",
    "那个",
    "那种",
    "那些",
    "每个",
    "某个",
    "某种",
    "可以",
    "处理",
    "编排",
    "法律",
    "所有",
    "不是",
    "没有",
    "我们",
    "他们",
    "它们",
    "以及",
    "或者",
    "如果",
    "因为",
    "所以",
    "但是",
    "其中",
    "词元",
];

enum CompoundSnap {
    /// Take this many upcoming characters onto the page that is already open.
    Pull(usize),
    /// Move this many trailing characters onto the next page.
    Push(usize),
}

fn retreat_compound_boundary(
    used: &mut [String],
    upcoming: &[char],
    width: f32,
    size: f32,
    font: &SubsetFont,
) -> bool {
    let Some(last) = used.last().cloned() else {
        return false;
    };
    let Some(snap) = compound_snap(&last, upcoming, width, size, font) else {
        return false;
    };
    let last_chars: Vec<char> = last.chars().collect();
    let next = match snap {
        CompoundSnap::Pull(count) => {
            let mut extra = String::new();
            let mut seen = 0usize;
            for ch in upcoming {
                if ch.is_whitespace() {
                    if seen == 0 {
                        continue;
                    }
                    break;
                }
                extra.push(*ch);
                seen += 1;
                if seen == count {
                    break;
                }
            }
            if extra.chars().count() != count {
                return false;
            }
            let line = format!("{last}{extra}");
            if fit_measure(&line, size, font) > width + 0.6 {
                return false;
            }
            line
        }
        CompoundSnap::Push(count) => {
            if last_chars.len() <= count {
                return false;
            }
            let line: String = last_chars[..last_chars.len() - count].iter().collect();
            let line = line.trim_end().to_string();
            if line.is_empty() {
                return false;
            }
            line
        }
    };
    if let Some(slot) = used.last_mut() {
        *slot = next;
    }
    true
}

fn compound_snap(
    last: &str,
    upcoming: &[char],
    width: f32,
    size: f32,
    font: &SubsetFont,
) -> Option<CompoundSnap> {
    let left: Vec<char> = last.chars().collect();
    if left.is_empty() {
        return None;
    }
    let mut right_at = 0usize;
    while right_at < upcoming.len() && upcoming[right_at].is_whitespace() {
        right_at += 1;
    }
    let right = &upcoming[right_at..];
    if right.is_empty() || !is_cjk_body(*left.last()?) || !is_cjk_body(right[0]) {
        return None;
    }
    if let Some(snap) = dictionary_crossing(&left, right, last, width, size, font) {
        return Some(snap);
    }
    // `词。` would open the next page. Keep that character with the word before it.
    let orphan = right.get(1).is_none_or(|ch| !is_cjk_body(*ch));
    if !orphan {
        return None;
    }
    let pulled = format!("{last}{}", right[0]);
    if fit_measure(&pulled, size, font) <= width + 0.6 {
        return Some(CompoundSnap::Pull(1));
    }
    let mut run = 0usize;
    while run < left.len() && run < 4 && is_cjk_body(left[left.len() - 1 - run]) {
        run += 1;
    }
    (run >= 1 && left.len() > run).then_some(CompoundSnap::Push(run))
}

fn dictionary_crossing(
    left: &[char],
    right: &[char],
    last: &str,
    width: f32,
    size: f32,
    font: &SubsetFont,
) -> Option<CompoundSnap> {
    let take = left.len().min(8);
    let prefix = &left[left.len() - take..];
    let suffix_len = right.len().min(8);
    let mut junction = prefix.to_vec();
    junction.extend_from_slice(&right[..suffix_len]);
    let cut = prefix.len();
    let mut best: Option<(usize, usize)> = None;
    for word in CJK_COMPOUNDS {
        let piece: Vec<char> = word.chars().collect();
        if piece.len() < 2 || piece.len() > junction.len() {
            continue;
        }
        for start in 0..=junction.len() - piece.len() {
            if junction[start..start + piece.len()] != piece[..] {
                continue;
            }
            let end = start + piece.len();
            if start < cut && end > cut && best.is_none_or(|(_, len)| piece.len() > len) {
                best = Some((start, piece.len()));
            }
        }
    }
    let (start, len) = best?;
    let end = start + len;
    let on_left = cut - start;
    let on_right = end - cut;
    let mut pulled = last.to_string();
    for ch in right.iter().take(on_right) {
        pulled.push(*ch);
    }
    if fit_measure(&pulled, size, font) <= width + 0.6 {
        return Some(CompoundSnap::Pull(on_right));
    }
    (on_left > 0 && left.len() > on_left).then_some(CompoundSnap::Push(on_left))
}

fn consumed_chars(src: &[char], lines: &[String]) -> usize {
    let mut index = 0usize;
    for line in lines {
        for ch in line.chars() {
            while index < src.len() && src[index].is_whitespace() && src[index] != ch {
                index += 1;
            }
            if index < src.len() && src[index] == ch {
                index += 1;
            } else {
                return 0;
            }
        }
        while index < src.len() && src[index].is_whitespace() {
            index += 1;
        }
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uniform_font(text: &str) -> SubsetFont {
        let mut glyphs = std::collections::HashMap::new();
        for (id, ch) in (1u16..).zip(text.chars()) {
            glyphs.insert(ch as u32, (id, 1000));
        }
        SubsetFont {
            bytes: Vec::new(),
            units_per_em: 1000,
            glyphs,
        }
    }

    fn cover_font(text: &str) -> SubsetFont {
        let mut glyphs = std::collections::HashMap::new();
        for (id, ch) in (1u16..).zip(text.chars()) {
            let advance = if ch.is_ascii_alphabetic() {
                560
            } else if ch == ' ' {
                258
            } else {
                1000
            };
            glyphs.insert(ch as u32, (id, advance));
        }
        SubsetFont {
            bytes: Vec::new(),
            units_per_em: 1000,
            glyphs,
        }
    }

    #[test]
    fn a_github_url_is_not_left_as_a_scheme() {
        let text = "获取获取获取获取获取获取获取获取获取获取源代码可以从 Manning 在 www.manning.com/books/ai-agents-and-applications 获取，并在 GitHub 上于 https://mng.bz/rZlZ 镜像";
        let metrics = CjkMeasure::resolve(0.90, 1.60);
        let fitted = fit_cjk_block(
            text,
            10.0,
            0.90,
            372.0,
            0.0,
            40.0,
            &mixed_ascii_font(text),
            metrics,
        )
        .expect("paragraph");
        assert!(
            fitted
                .0
                .iter()
                .any(|line| line.contains("https://mng.bz/rZlZ")),
            "{:?}",
            fitted.0
        );
        let chars: Vec<char> = text.chars().collect();
        let sticky = sticky_after(&chars);
        let scheme = chars.iter().position(|ch| *ch == 'h').expect("scheme");
        assert!(
            sticky.get(scheme + 7).copied().unwrap_or(false),
            "CJK before the URL shifted the sticky span off https://"
        );
    }

    #[test]
    fn a_display_title_does_not_split_a_cjk_word_after_latin() {
        let text = "使用 LangChain 摘要文本";
        let font = mixed_ascii_font(text);
        let metrics = CjkMeasure::resolve(1.0, 1.15);
        // Width fits `使用 LangChain 摘` and not the next character, which
        // is how 摘要 was cut on the chapter opener.
        let fitted =
            fit_cjk_block(text, 30.0, 1.0, 260.0, 0.0, 32.0, &font, metrics).expect("title");
        assert!(
            fitted.0.iter().any(|line| line.contains("摘要文本")),
            "{:?}",
            fitted.0
        );
        assert!(
            fitted.0.iter().all(|line| !line.ends_with('摘')),
            "{:?}",
            fitted.0
        );
    }

    fn mixed_ascii_font(text: &str) -> SubsetFont {
        let mut glyphs = std::collections::HashMap::new();
        for (id, ch) in (1u16..).zip(text.chars()) {
            let advance = if ch.is_ascii() { 500 } else { 1000 };
            glyphs.insert(ch as u32, (id, advance));
        }
        SubsetFont {
            bytes: Vec::new(),
            units_per_em: 1000,
            glyphs,
        }
    }

    #[test]
    fn a_url_moves_intact_to_the_next_line() {
        let text = "位于 https://mng.bz/rZlZ 处";
        let metrics = CjkMeasure::resolve(1.0, 1.60);
        let fitted = fit_cjk_block(
            text,
            10.0,
            1.0,
            200.0,
            0.0,
            0.0,
            &uniform_font(text),
            metrics,
        )
        .expect("url paragraph fits");
        assert!(
            fitted
                .0
                .iter()
                .any(|line| line.contains("https://mng.bz/rZlZ")),
            "url was split: {:?}",
            fitted.0
        );
        assert!(
            !fitted
                .0
                .iter()
                .any(|line| line.contains("https://") && !line.contains("mng.bz/rZlZ")),
            "scheme was left behind: {:?}",
            fitted.0
        );
    }

    #[test]
    fn a_one_word_title_stays_on_one_line() {
        let metrics = CjkMeasure::resolve(0.90, 1.60);
        let text = "你好!";
        let fitted = fit_cjk_block(
            text,
            63.0,
            0.90,
            81.0,
            0.0,
            0.0,
            &mixed_ascii_font(text),
            metrics,
        )
        .expect("title fits");
        assert_eq!(fitted.0, vec!["你好!".to_string()], "{:?}", fitted.0);
    }

    #[test]
    fn a_cover_subtitle_breaks_after_the_enumeration_comma() {
        let metrics = CjkMeasure::resolve(0.90, 1.60);
        let text = "使用 LangChain、LangGraph 和 MCP";
        let fitted = fit_cjk_block(
            text,
            18.5,
            0.96,
            164.0,
            0.0,
            0.0,
            &cover_font(text),
            metrics,
        )
        .expect("subtitle fits the panel");
        assert_eq!(fitted.0.len(), 2, "{:?}", fitted.0);
        assert!(
            fitted.1 + 0.05 >= 18.5 * 0.80,
            "subtitle shrunk below a readable size: {}",
            fitted.1
        );
        assert!(fitted.0[0].ends_with('、'), "{:?}", fitted.0);
        assert!(
            fitted.0[1].contains("LangGraph") && fitted.0[1].contains("MCP"),
            "{:?}",
            fitted.0
        );
        assert!(!fitted.0[1].starts_with('和'), "{:?}", fitted.0);

        let forced = wrap_text(text, 12.0, 170.0, 170.0, &cover_font(text)).expect("wrap");
        assert!(
            forced.iter().any(|line| line.ends_with('、')),
            "a wide line must still break after the comma, not before 和: {forced:?}"
        );
        assert!(
            !forced.iter().any(|line| line.starts_with('和')),
            "{forced:?}"
        );

        let name = "Roberto Infante";
        let author = fit_cjk_block(
            name,
            22.0,
            0.96,
            110.0,
            0.0,
            0.0,
            &cover_font(name),
            metrics,
        )
        .expect("author name fits");
        assert_eq!(author.0, vec![name.to_string()]);

        let dotted = "罗伯托·因凡特";
        let narrow = fit_cjk_block(
            dotted,
            22.0,
            0.96,
            40.0,
            0.0,
            0.0,
            &uniform_font(dotted),
            metrics,
        );
        assert!(
            narrow.as_ref().is_none_or(|fitted| fitted.0.len() == 1),
            "{narrow:?}"
        );

        let address = "维多利亚州3006澳大利亚";
        let fitted_address = fit_cjk_block(
            address,
            10.0,
            0.90,
            88.0,
            0.0,
            0.0,
            &uniform_font(address),
            metrics,
        )
        .expect("address fits");
        assert_eq!(fitted_address.0.len(), 1, "{:?}", fitted_address.0);
        assert!(
            fitted_address.1 + 0.05 >= 10.0 * 0.62,
            "address shrunk below the body floor: {}",
            fitted_address.1
        );

        let prose = "并且 Manning Publications 知悉了商标声明";
        let lines = wrap_text(prose, 10.0, 80.0, 220.0, &uniform_font(prose)).expect("name");
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Manning Publications")),
            "the name should stay on one line: {lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.trim() == "Manning"),
            "{lines:?}"
        );
        assert_eq!(
            space_cjk_latin_mask("Publications知悉了商标声明，并且Manning", &[]).0,
            "Publications 知悉了商标声明，并且 Manning"
        );
        assert_eq!(space_cjk_latin_mask("第4章与15%", &[]).0, "第4章与15%");
    }

    #[test]
    fn fit_paragraph_needs_no_model_and_indents_body_text() {
        let body = "编程智能体通过长轨迹解决仓库级任务。随着任务推进，早期探索会过时。";
        let font = uniform_font(body);
        let metrics = CjkMeasure::resolve(0.90, 1.60);
        let fitted = fit_paragraph(body, 10.0, 180.0, 80.0, 5, false, &font, metrics)
            .expect("body fits offline");
        assert!(fitted.lines.len() >= 2, "{:?}", fitted.lines);
        assert!((fitted.indent - fitted.size * 2.0).abs() < 0.05);
        assert!((fitted.size - 9.0).abs() < 0.05);
        assert!((fitted.leading / fitted.size - 1.60).abs() < 0.02);

        let title = fit_paragraph(
            "1 引言",
            14.0,
            200.0,
            0.0,
            1,
            true,
            &uniform_font("1 引言"),
            metrics,
        )
        .expect("title fits");
        assert_eq!(title.indent, 0.0);
        assert!((title.size - 14.0 * metrics.heading_scale).abs() < 0.05);

        let sub_text = "2.2 评判引导的数据收集与SFT";
        let sub = fit_paragraph(
            sub_text,
            10.0,
            360.0,
            0.0,
            1,
            false,
            &uniform_font(sub_text),
            metrics,
        )
        .expect("subsection fits");
        assert_eq!(sub.indent, 0.0);
        assert!(
            sub.size > 9.0 + 0.5,
            "subsection should sit above the 9pt body, got {}",
            sub.size
        );
        let section = fit_paragraph(
            "1 引言",
            12.0,
            360.0,
            0.0,
            1,
            false,
            &uniform_font("1 引言"),
            metrics,
        )
        .expect("section fits");
        assert!(
            section.size > sub.size,
            "section {} should exceed subsection {}",
            section.size,
            sub.size
        );

        let small_caps = "2.2 裁判引导的数据收集与SFT";
        let lifted = fit_paragraph(
            small_caps,
            7.97,
            360.0,
            0.0,
            1,
            false,
            &uniform_font(small_caps),
            metrics,
        )
        .expect("small-cap subsection");
        let body = fit_paragraph(
            "正文应当使用更小的字号并且保持首行缩进两个汉字。",
            9.96,
            360.0,
            40.0,
            3,
            false,
            &uniform_font("正文应当使用更小的字号并且保持首行缩进两个汉字。"),
            metrics,
        )
        .expect("body");
        assert!(
            lifted.size > body.size + 0.6,
            "small-cap subsection {} should exceed body {}",
            lifted.size,
            body.size
        );
        assert_eq!(lifted.indent, 0.0);

        let dense = "中文回归".repeat(185);
        let narrow = fit_paragraph(
            &dense,
            10.91,
            220.0,
            189.7,
            15,
            false,
            &uniform_font(&dense),
            metrics,
        )
        .expect("a narrow column of paint-density Chinese still fits");
        assert!(
            narrow.size + 0.05 >= 10.91 * 0.62,
            "shrunk below the floor: {}",
            narrow.size
        );
        assert!(narrow.lines.len() > 15, "{:?}", narrow.lines.len());
        let abstract_body = "中文回归".repeat(300);
        let column = fit_paragraph(
            &abstract_body,
            9.96,
            186.0,
            310.8,
            27,
            false,
            &uniform_font(&abstract_body),
            metrics,
        );
        assert!(
            column.is_some(),
            "27-line narrow abstract should fit at the 0.62 floor"
        );

        let crumb = "基于大语言模型的聊天机器人";
        let fitted = fit_paragraph(
            crumb,
            10.0,
            78.0,
            0.0,
            1,
            false,
            &uniform_font(crumb),
            metrics,
        )
        .expect("a short contents title wraps instead of shrinking to half");
        assert!(
            fitted.size + 0.05 >= 8.0,
            "contents title shrunk below 0.80: {}",
            fitted.size
        );
        assert_eq!(fitted.lines.len(), 2, "{:?}", fitted.lines);
        let short = fit_paragraph(
            "代理",
            10.0,
            78.0,
            0.0,
            1,
            false,
            &uniform_font("代理"),
            metrics,
        )
        .expect("a title that fits stays one line");
        assert_eq!(short.lines.len(), 1, "{:?}", short.lines);
        let folio = fit_paragraph(
            "十四",
            9.0,
            16.0,
            0.0,
            1,
            false,
            &uniform_font("十四"),
            metrics,
        )
        .expect("a two-character folio stays one line");
        assert_eq!(folio.lines.len(), 1, "{:?}", folio.lines);
        assert!(
            folio.size + 0.05 >= 9.0 * 0.62,
            "folio shrunk below the body floor: {}",
            folio.size
        );
        let wide = "基于大语言模型的聊天机器人".repeat(3);
        assert!(
            fit_paragraph(
                &wide,
                10.0,
                200.0,
                0.0,
                1,
                false,
                &uniform_font(&wide),
                metrics,
            )
            .is_none(),
            "a wide one-line paragraph stays at the 0.62 floor"
        );

        let affil = "1新加坡管理大学2南洋理工大学3哈佛大学";
        assert_eq!(
            heading_level(1, 9.96, false, affil),
            HeadingLevel::Body,
            "affiliation marks are not a section heading"
        );
        let street = "20 鲍德温路";
        assert_eq!(
            heading_level(1, 9.0, false, street),
            HeadingLevel::Body,
            "a street number is not a section heading"
        );
        assert_eq!(
            heading_level(1, 10.0, true, "该智能体向 LLM 发送提示，要求其选择最相关的"),
            HeadingLevel::Body,
            "a heading must not end on 的"
        );
        assert_eq!(
            heading_level(
                1,
                12.0,
                true,
                "该智能体执行查询，收集结果，并发送另一条提示，"
            ),
            HeadingLevel::Body,
            "a heading must not end on a comma"
        );
        assert_eq!(
            heading_level(
                2,
                10.0,
                true,
                "1 智能体向大语言模型发送一段提示，要求其选择最相关的工具"
            ),
            HeadingLevel::Body,
            "a numbered item with a hanging continuation is not a heading"
        );
        let wrapped = "例如思维链（CoT）。LangChain 的一系列提示工程工具";
        let lines = wrap_text(wrapped, 10.0, 60.0, 60.0, &uniform_font(wrapped)).expect("wrap");
        assert!(
            lines.len() >= 2,
            "the opener must wrap onto the next line: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .all(|line| !line.ends_with('（') && !line.ends_with('(') && !line.ends_with('<')),
            "an opening bracket ends a line: {lines:?}"
        );
        let tagged = "</输入> <语气>";
        let tags = wrap_text(tagged, 10.0, 70.0, 70.0, &uniform_font(tagged)).expect("tags");
        assert!(
            tags.iter().all(|line| !line.ends_with('<')),
            "a prompt tag was split: {tags:?}"
        );
        assert!(
            tags.iter().any(|line| line.contains("<语气>")),
            "the tag should stay one token: {tags:?}"
        );
        assert!(
            lines.iter().any(|line| line.starts_with('（')),
            "the opener should move with the following text: {lines:?}"
        );
        assert_eq!(
            heading_level(1, 11.0, true, "1.1 构建基于大语言模型的应用"),
            HeadingLevel::Subsection,
            "a dotted section index stays a heading"
        );
        let street_fit = fit_paragraph(
            street,
            9.0,
            200.0,
            0.0,
            1,
            false,
            &uniform_font(street),
            metrics,
        )
        .expect("street");
        assert!(
            street_fit.size <= 9.0 * 0.95,
            "street address grew past body size: {}",
            street_fit.size
        );
        assert_eq!(street_fit.lines.len(), 1, "{:?}", street_fit.lines);
        let affil_fit = fit_paragraph(
            affil,
            9.96,
            420.0,
            0.0,
            1,
            false,
            &uniform_font(affil),
            metrics,
        )
        .expect("affiliation");
        assert!(
            (affil_fit.size - 9.96 * 0.90).abs() < 0.15,
            "affiliation should stay body size, got {}",
            affil_fit.size
        );
    }

    #[test]
    fn a_short_translation_spreads_onto_every_source_line() {
        let lines = spread_to_lines(
            vec!["我们提出了一种基于完全图的聚类公式并且保留每一行。".into()],
            4,
        );
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(lines.iter().all(|line| !line.is_empty()));
        let joined: String = lines.concat();
        assert_eq!(joined, "我们提出了一种基于完全图的聚类公式并且保留每一行。");
    }

    #[test]
    fn spreading_keeps_a_word_intact_and_off_the_line_start() {
        let line = "简单API调用；下一句继续写完整的说明文字在这里。";
        let lines = spread_to_lines(vec![line.into()], 4);
        assert_eq!(lines.concat(), line);
        assert!(lines.iter().any(|piece| piece.contains("API")), "{lines:?}");
        assert!(
            lines.iter().all(|piece| !piece.starts_with('；')
                && !piece.starts_with('。')
                && !piece.ends_with("AP")
                && !piece.starts_with('I')),
            "{lines:?}"
        );
    }

    #[test]
    fn spreading_keeps_a_citation_on_one_line() {
        let line = "特征向量内积的因子化(Kazi et al., 2022)，但这会显著增加复杂性。";
        let lines = spread_to_lines(vec![line.into()], 6);
        assert!(lines.len() > 1, "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|piece| piece.contains("(Kazi et al., 2022)")),
            "{lines:?}"
        );
        assert_eq!(lines.concat(), line);
        assert_eq!(
            spread_to_lines(vec!["112".into()], 3),
            vec!["112".to_string()]
        );
    }

    #[test]
    fn a_prose_year_wraps_and_a_citation_year_stays_intact() {
        let cited = "前文很长(Smith, 2020)后文继续写完这一句";
        let cited_lines = wrap_text(cited, 10.0, 160.0, 160.0, &uniform_font(cited)).unwrap();
        assert!(
            cited_lines
                .iter()
                .any(|line| line.contains("(Smith, 2020)")),
            "{cited_lines:?}"
        );

        let prose = "加性组合学描述了一个快速发展的数学领域，其动机源于关于整数加法和乘法的简单问题。（“加性组合学”这一名称在2000年代变得流行，当时该领域因高尔斯、格林、陶哲轩等人的开创性工作而迅速爆发；此前该领域更常被称为“组合数论”。）加性组合学中的问题和方法深刻且影响深远，连接了数学的许多不同领域。";
        let prose_lines = wrap_text(prose, 10.0, 360.0, 380.0, &uniform_font(prose)).unwrap();
        assert!(
            prose_lines.len() <= 8,
            "a year inside a Chinese aside wrapped one character at a time: {prose_lines:?}"
        );
        assert!(
            prose_lines.iter().all(|line| line.chars().count() > 1),
            "{prose_lines:?}"
        );

        let long = format!("（{} et al., 2020）", "甲".repeat(30));
        let long_lines = wrap_text(&long, 10.0, 120.0, 120.0, &uniform_font(&long)).unwrap();
        assert!(
            long_lines.len() <= 8 && long_lines.iter().all(|line| line.chars().count() >= 4),
            "a sticky span wider than the line became one character: {long_lines:?}"
        );
    }

    #[test]
    fn enumerated_stock_items_each_start_a_line() {
        let text = "股票2：英伟达：销售额正受到大语言模型消费者需求的推动股票3：GX石油：得益于可再生能源的增长，碳基能源的需求正在下降";
        let font = uniform_font(text);
        let width = measure(text, 10.0, &font);
        let lines = wrap_text(text, 10.0, width, width, &font).expect("wrap");
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].starts_with("股票2：") && !lines[0].contains("股票3"),
            "{lines:?}"
        );
        assert!(lines[1].starts_with("股票3："), "{lines:?}");
        let single = "股票1：苹果：新款iPhone的发布取得了成功";
        let one = wrap_text(single, 10.0, 400.0, 400.0, &uniform_font(single)).unwrap();
        assert_eq!(one, vec![single.to_string()]);
        assert!(
            !justify_wrapped_line(&lines, 0) && !justify_wrapped_line(&lines, 1),
            "an item on its own line was justified: {lines:?}"
        );
        let gaps = justify_gaps(
            &lines[0],
            10.0,
            width,
            &font,
            justify_wrapped_line(&lines, 0),
        );
        assert!(
            gaps.iter().all(|gap| *gap == 0.0),
            "股票2 was letter-spaced: {gaps:?}"
        );
        assert!(
            !prompt_line_is_full(&lines[0], 10.0, width, &font),
            "a short prompt item looked full"
        );

        let long = format!("股票2：{}继续写完这一项的说明。股票3：短", "甲".repeat(40));
        let font = uniform_font(&long);
        let wrapped = wrap_text(&long, 10.0, 180.0, 180.0, &font).unwrap();
        assert!(wrapped.len() >= 3, "{wrapped:?}");
        assert!(
            justify_wrapped_line(&wrapped, 0)
                && prompt_line_is_full(&wrapped[0], 10.0, 180.0, &font),
            "a full item line should still square: {wrapped:?}"
        );
        let tail = wrapped
            .iter()
            .position(|line| line.starts_with("股票3"))
            .unwrap();
        assert!(tail > 0);
        assert!(
            !justify_wrapped_line(&wrapped, tail - 1),
            "the last line of 股票2 was justified: {wrapped:?}"
        );
    }

    #[test]
    fn justification_never_opens_more_than_a_sixth_of_an_em() {
        let text = "的一系列提示工程工具包括后续说明";
        let font = uniform_font(text);
        let natural = measure(text, 10.0, &font);
        let pairs = cjk_pair_count(text) as f32;
        let width = natural + (pairs * 10.0 * MAX_JUSTIFY_EM * 0.5).max(1.0);
        let gaps = justify_gaps(text, 10.0, width, &font, true);
        let cap = 10.0 * MAX_JUSTIFY_EM + 0.01;
        assert!(
            gaps.iter().all(|gap| *gap <= cap),
            "inter-character gap exceeded {cap:.2}: {gaps:?}"
        );
        assert!(gaps.iter().any(|gap| *gap > 0.0), "{gaps:?}");
        let rivers = justify_gaps(text, 10.0, natural + 40.0, &font, true);
        assert!(
            rivers.iter().all(|gap| *gap == 0.0),
            "a line the cap cannot square was letter-spaced: {rivers:?}"
        );
    }

    #[test]
    fn an_accented_latin_word_stays_on_one_line() {
        let text = "赋值拟阵自然地产生于行列式的赋值和 Plücker 关系。此后进一步扩展。";
        let font = uniform_font(text);
        let lines = wrap_text(text, 10.0, 180.0, 180.0, &font).unwrap();
        let glued = lines.join("");
        assert!(
            glued.contains("Plücker"),
            "Plücker was split: {lines:?}"
        );
        assert!(
            lines.iter().all(|line| !line.ends_with("Plü") && !line.starts_with("cker")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_long_camel_case_token_breaks_so_the_line_can_fill() {
        let text = "的一系列提示工程工具包括FewShotPromptTemplate并且继续写完这一句说明。";
        let font = uniform_font(text);
        // `包括FewShot` fits in 200pt. The whole identifier does not, and the
        // line must not stop before `Few` just to keep the token intact.
        let lines = wrap_text(text, 10.0, 200.0, 200.0, &font).unwrap();
        assert!(
            lines[0].contains("FewShot") && !lines[0].contains("PromptTemplate"),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("PromptTemplate")),
            "{lines:?}"
        );
        let slack = 200.0 - measure(&lines[0], 10.0, &font);
        let cap = cjk_pair_count(&lines[0]) as f32 * 10.0 * 0.15 + 0.5;
        assert!(
            slack <= cap,
            "line still short by {slack:.1}pt after the camelCase break: {}",
            lines[0]
        );
        let url = "说明见https://Ex.com/AbCd获取更多";
        // The URL is 190pt at 1em. A 180pt measure cannot hold it, so the
        // line would have to cut the token. A wider measure must keep it whole.
        let url_lines = wrap_text(url, 10.0, 250.0, 250.0, &uniform_font(url)).unwrap();
        assert!(
            url_lines
                .iter()
                .any(|line| line.contains("https://Ex.com/AbCd")),
            "a URL was cut at camelCase: {url_lines:?}"
        );
    }

    #[test]
    fn a_narrow_contents_tail_does_not_take_half_the_title() {
        let text = "基于LLM的应用：摘要生成与问答引擎";
        let font = uniform_font(text);
        // The English wrap `engines` is ~28pt. The title above it is ~200pt.
        let packed = pack_narrow_tail(text, 9.0, 0.0, &[(177.0, 381.0), (177.0, 206.0)], &font)
            .expect("narrow tail packs");
        assert_eq!(packed.len(), 2, "{packed:?}");
        assert_eq!(
            packed[1], "引擎",
            "the crumb should keep the wrapped word, got {packed:?}"
        );
        assert_eq!(packed[0], "基于LLM的应用：摘要生成与问答");
        assert!(
            measure(&packed[1], 9.0, &font) <= 29.0 + 0.5,
            "crumb overflowed the engines box: {packed:?}"
        );
        let body = "我们提出了一种基于完全图的聚类公式并且保留每一行的来源。第二行仍然比较宽。";
        assert!(
            pack_narrow_tail(
                body,
                10.0,
                0.0,
                &[(72.0, 280.0), (72.0, 160.0)],
                &uniform_font(body)
            )
            .is_none(),
            "a normal short last line is not a contents crumb"
        );
    }

    #[test]
    fn pack_into_slots_keeps_the_tail_or_fits_nothing() {
        let text = "为了收集训练数据我们在编码任务上运行基础智能体并使用评审器审查压缩决策";
        let font = uniform_font(text);
        let wide = LineSlots {
            width: 200.0,
            slots: 2,
        };
        let packed =
            pack_into_slots(text, 10.0, 0.0, &[wide, wide], &font, false).expect("two boxes");
        let flat: String = packed.iter().flatten().map(|line| line.as_str()).collect();
        assert_eq!(flat, text, "{packed:?}");
        assert!(packed[0].len() <= 2 && packed[1].len() <= 2, "{packed:?}");
        assert!(
            pack_into_slots(
                text,
                10.0,
                0.0,
                &[LineSlots {
                    width: 200.0,
                    slots: 1,
                }],
                &font,
                false,
            )
            .is_none(),
            "one short line must not keep a prefix"
        );
    }

    #[test]
    fn a_page_break_does_not_split_a_cjk_compound() {
        // Twelve ems end on `一个`. The next character is `词`, which does not
        // fit, so the compound moves intact onto the next page.
        let text = "定义其中词元大致对应一个词。例如模型。";
        let font = uniform_font(text);
        let packed = pack_into_slots(
            text,
            10.0,
            0.0,
            &[
                LineSlots {
                    width: 120.0,
                    slots: 1,
                },
                LineSlots {
                    width: 200.0,
                    slots: 2,
                },
            ],
            &font,
            true,
        )
        .expect("compound fits across the page");
        let page_a = packed[0].concat();
        let page_b = packed[1].concat();
        assert!(
            page_a.contains("一个词") || page_b.contains("一个词"),
            "一个词 was cut: {page_a:?} | {page_b:?}"
        );
        assert!(
            !(page_a.ends_with('个') && page_b.starts_with('词')),
            "page break split 一个|词: {page_a:?} | {page_b:?}"
        );
        assert!(
            !(page_a.ends_with('一') && page_b.starts_with('个')),
            "page break split 一|个: {page_a:?} | {page_b:?}"
        );
        let flat = format!("{page_a}{page_b}");
        assert_eq!(flat, text);
    }

    #[test]
    fn a_line_does_not_split_a_cjk_compound() {
        // `一个词` overlaps `词元` in `其中一个词元`. Neither boundary breaks.
        let text = "不同的模型支持不同的词元限制，其中一个词元。";
        let font = uniform_font(text);
        // Twenty ems end on the 词 of `一个词`, and the following 元 does not
        // fit. `词元` overlaps that `一个词`, so the break moves before both.
        let lines = wrap_text(text, 10.0, 200.0, 200.0, &font).expect("wrap");
        let flat: String = lines.concat();
        assert_eq!(flat, text);
        for pair in lines.windows(2) {
            assert!(
                !(pair[0].ends_with('词') && pair[1].starts_with('元')),
                "词|元: {lines:?}"
            );
            assert!(
                !(pair[0].ends_with('个') && pair[1].starts_with('词')),
                "一个|词: {lines:?}"
            );
        }
    }

    #[test]
    fn typesetting_sources_do_not_name_a_model_client() {
        let files = [
            include_str!("layout.rs"),
            include_str!("segment.rs"),
            include_str!("rewrite.rs"),
        ];
        for src in files {
            let production = src.split("#[cfg(test)]").next().unwrap();
            for needle in ["Translator", "RPT_LLM_API_KEY", "chat/completions", "ureq"] {
                assert!(
                    !production.contains(needle),
                    "typesetting source mentioned {needle}"
                );
            }
        }
    }

    #[test]
    fn a_one_character_tail_is_pulled_onto_two_characters() {
        let text = "模块化架构与组件";
        let font = uniform_font(text);
        // Eight characters at 10pt need 80pt. 70pt leaves one character over.
        let fitted = fit_cjk_block(
            text,
            10.0,
            1.0,
            70.0,
            0.0,
            40.0,
            &font,
            CjkMeasure::resolve(1.0, 1.2),
        )
        .expect("fit");
        let lines = &fitted.0;
        let tail = lines
            .last()
            .unwrap()
            .chars()
            .filter(|ch| is_cjk_body(*ch))
            .count();
        assert!(tail >= 2, "orphan tail: {lines:?}");
        let flat: String = lines.iter().flat_map(|line| line.chars()).collect();
        assert_eq!(flat, text);
    }
}
