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

/// How Chinese body text is sized relative to the English it replaces.
///
/// CJK glyphs fill the em square, so the same point size looks heavier than
/// Latin, and the English baseline gap then feels cramped. Body text starts
/// smaller and uses a Chinese academic leading; the fit loop may shrink the
/// size further, but it keeps that leading ratio.
#[derive(Clone, Copy, Debug)]
pub struct CjkMeasure {
    pub body_scale: f32,
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
            heading_scale: (body + 0.06).min(1.0),
            leading_ratio: leading,
            min_leading_ratio: (leading * 0.88).max(1.40).min(leading),
        }
    }
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
    let indent_ems = cjk_indent_ems(line_count, source_size, bold, text);
    let scale = if indent_ems == 0.0 && line_count < 2 && source_size >= 12.5 {
        metrics.heading_scale
    } else {
        metrics.body_scale
    };
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
    if is_cjk_caption(text) || is_cjk_heading(line_count, source_size, bold, text) {
        0.0
    } else if line_count >= 2 || is_one_line_prose(text, source_size, bold) {
        2.0
    } else {
        0.0
    }
}

fn is_cjk_heading(line_count: usize, source_size: f32, bold: bool, text: &str) -> bool {
    let trimmed = text.trim();
    let chars = trimmed.chars().count();
    if source_size >= 12.5 && line_count <= 3 && chars < 120 {
        return true;
    }
    if line_count == 1 && bold && chars < 80 {
        return true;
    }
    line_count == 1 && chars < 40 && !ends_sentence(trimmed)
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
    let start = (source_size * scale).max(1.0);
    let floor = (start * 0.78).max(source_size * 0.62).min(start);
    let mut size = start;
    loop {
        let indent = size * indent_ems;
        let first_width = (width - indent).max(size * 0.5);
        let lines = wrap_text(text, size, first_width, width, font)?;
        let gaps = lines.len().saturating_sub(1);
        let within = lines.iter().enumerate().all(|(index, line)| {
            let limit = if index == 0 { first_width } else { width };
            fit_measure(line, size, font) <= limit + 1.0
        });
        let single_source = available <= 0.5;
        if within && (!single_source || lines.len() == 1) {
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
            if within && !single_source && gaps > 0 {
                let room = available / gaps as f32;
                if room >= size * 1.05 {
                    return Some((lines, size, room, indent));
                }
            }
            if within && lines.len() == 1 {
                return Some((lines, size, size * metrics.leading_ratio, indent));
            }
            return None;
        }
        let next = (size * 0.94).max(floor);
        if (next - size).abs() < 0.01 {
            return None;
        }
        size = next;
    }
}

/// Extra space goes only between CJK characters. A single `Tc` would letter-space
/// the Latin citation on the same line.
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
    if !(0.4..width * 0.12).contains(&slack) {
        return gaps;
    }
    let extra = slack / slots.len() as f32;
    // A larger gap reads as letter-spacing. Leave the line ragged instead.
    if extra > size * 0.03 {
        return gaps;
    }
    for index in slots {
        gaps[index] = extra;
    }
    gaps
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

pub(crate) fn wrap_text(
    text: &str,
    size: f32,
    first_width: f32,
    max_width: f32,
    font: &SubsetFont,
) -> Option<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let sticky = sticky_after(&chars);
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
        while end < chars.len() {
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
        } else if end < chars.len() {
            // Break at the rightmost legal point. Jumping back to the previous
            // ASCII space throws away CJK that already fit after a citation.
            // A sticky span longer than the line falls through to one character.
            while end > start + 1 && !can_break_at(&chars, &sticky, end) {
                end -= 1;
            }
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
        if lines.len() > 48 {
            return None;
        }
    }
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
    sticky
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
            let digits = inside.chars().filter(|ch| ch.is_ascii_digit()).count();
            if inside.to_ascii_lowercase().contains("et al") || digits >= 4 {
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

fn can_break(prev: char, next: char) -> bool {
    // Keep Latin words and hyphenated names on one line. CJK still breaks per character.
    if prev.is_ascii_alphanumeric() && (next.is_ascii_alphanumeric() || next == '-' || next == '\'')
    {
        return false;
    }
    if (prev == '-' || prev == '\'') && next.is_ascii_alphanumeric() {
        return false;
    }
    break_after(prev) && break_before(next)
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
    !matches!(ch, '《' | '「' | '『' | '【' | '(' | '[' | '{')
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
}
