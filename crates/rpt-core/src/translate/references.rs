//! Bibliography regions that must stay original text.
//!
//! A reference section starts at a heading (`References`, `Bibliography`,
//! `参考文献`, `Literatur`, `Références`, …) or at a run of numbered /
//! author-year entries. It continues across columns and pages in reading
//! order (left column top-to-bottom, then the next column) and stops at the
//! next section heading (`Appendix`, `附录`, `A. Proofs`, …). Those glyphs
//! are `kept_original` with reason `references`.

use std::collections::{HashMap, HashSet};

use crate::extract::PdfDocument;
use crate::glyph::Glyph;

use crate::segment::{segment_glyphs, Segment};

/// One text-showing operator that belongs to the reference section.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StreamSpan {
    pub object_id: String,
    pub byte_start: usize,
    pub byte_end: usize,
}

struct Placed {
    seg_index: usize,
    page: u32,
    x: f32,
    y: f32,
    text: String,
}

const REF_HEADINGS: &[&str] = &[
    "references",
    "reference",
    "bibliography",
    "bibliographies",
    "works cited",
    "work cited",
    "literature cited",
    "cited literature",
    "cited references",
    "references and notes",
    "notes and references",
    "selected references",
    "selected bibliography",
    "bibliographic references",
    "bibliographical references",
    "references cited",
    "参考文献",
    "引用文献",
    "参考资料",
    "參考文獻",
    "參考資料",
    "文献",
    "文献一覧",
    "literatur",
    "literaturverzeichnis",
    "literaturangaben",
    "schriftenverzeichnis",
    "quellenverzeichnis",
    "bibliographie",
    "bibliografie",
    "références",
    "références bibliographiques",
    "ouvrages cités",
    "bibliografía",
    "referencias",
    "referencias bibliográficas",
    "obras citadas",
    "referências",
    "bibliografia",
    "riferimenti bibliografici",
    "riferimenti",
    "литература",
    "список литературы",
    "библиография",
    "literatuur",
    "referenties",
    "piśmiennictwo",
];

const END_HEADINGS: &[&str] = &[
    "appendix",
    "appendices",
    "annex",
    "annexes",
    "supplementary",
    "supplementary material",
    "supplementary materials",
    "supplemental material",
    "supplemental materials",
    "supporting information",
    "acknowledgments",
    "acknowledgements",
    "acknowledgment",
    "acknowledgement",
    "funding",
    "author contributions",
    "authors contributions",
    "conflict of interest",
    "conflicts of interest",
    "competing interests",
    "data availability",
    "code availability",
    "ethics statement",
    "附录",
    "附錄",
    "致谢",
    "致謝",
    "謝辭",
    "补充材料",
    "補充材料",
    "anhang",
    "danksagung",
    "annexe",
    "remerciements",
    "agradecimientos",
    "agradecimentos",
    "ringraziamenti",
    "приложение",
    "приложения",
];

const NOT_SURNAMES: &[&str] = &[
    "Figure",
    "Fig",
    "Table",
    "Tab",
    "Section",
    "Equation",
    "Eq",
    "Chapter",
    "Appendix",
    "Algorithm",
    "Theorem",
    "Lemma",
    "Page",
    "Volume",
    "Vol",
];

/// Glyphs that belong to a bibliography and must not be translated.
pub fn reference_glyph_ids(glyphs: &[Glyph]) -> HashSet<u32> {
    let segments = segment_glyphs(glyphs);
    let placed = place(&segments, glyphs);
    let order = reading_order(&placed);
    let tops = page_tops(&placed);
    let mut selected: HashSet<usize> = HashSet::new();
    let mut in_refs = false;
    let mut streak_from: Option<usize> = None;
    let mut streak_entries = 0usize;
    let mut streak_gap = 0usize;

    for (pos, &placed_index) in order.iter().enumerate() {
        let line = &placed[placed_index];
        if is_running_header(&placed, &tops, line) {
            continue;
        }
        let text = line.text.as_str();
        if !in_refs {
            if is_reference_heading(text) {
                in_refs = true;
                selected.insert(line.seg_index);
                streak_from = None;
                streak_entries = 0;
                streak_gap = 0;
                continue;
            }
            if is_reference_entry(text) {
                if streak_from.is_none() {
                    streak_from = Some(pos);
                    streak_entries = 1;
                    streak_gap = 0;
                } else {
                    streak_entries += 1;
                    streak_gap = 0;
                }
                if streak_entries >= 2 {
                    in_refs = true;
                    let start = streak_from.unwrap_or(pos);
                    for &idx in &order[start..=pos] {
                        let earlier = &placed[idx];
                        if !is_running_header(&placed, &tops, earlier) {
                            selected.insert(earlier.seg_index);
                        }
                    }
                    streak_from = None;
                }
            } else if streak_from.is_some() {
                streak_gap += 1;
                if streak_gap > 2 || is_section_break(text) {
                    streak_from = None;
                    streak_entries = 0;
                    streak_gap = 0;
                }
            }
        } else if is_section_break(text) {
            in_refs = false;
            streak_from = None;
            streak_entries = 0;
            streak_gap = 0;
        } else {
            selected.insert(line.seg_index);
        }
    }

    let mut ids = HashSet::new();
    for seg_index in &selected {
        ids.extend(segments[*seg_index].glyph_ids.iter().copied());
    }
    expand_mates(glyphs, &mut ids);
    for seg in &segments {
        if seg.glyph_ids.iter().any(|id| ids.contains(id)) {
            ids.extend(seg.glyph_ids.iter().copied());
        }
    }
    ids
}

/// Text-showing operators whose bytes must stay identical when references are kept.
pub fn reference_stream_spans(glyphs: &[Glyph]) -> Vec<StreamSpan> {
    let ids = reference_glyph_ids(glyphs);
    let mut seen = HashSet::new();
    let mut spans = Vec::new();
    for glyph in glyphs {
        if !ids.contains(&glyph.id) {
            continue;
        }
        let Some(object_id) = glyph.source.object_id.clone() else {
            continue;
        };
        if glyph.source.byte_end <= glyph.source.byte_start {
            continue;
        }
        let span = StreamSpan {
            object_id,
            byte_start: glyph.source.byte_start,
            byte_end: glyph.source.byte_end,
        };
        if seen.insert(span.clone()) {
            spans.push(span);
        }
    }
    spans
}

/// How many reference operators are byte-identical in `output`.
/// The pair is `(identical, total)`. `total == 0` means no reference section.
pub fn identical_reference_operators(
    source: &PdfDocument,
    output: &PdfDocument,
    glyphs: &[Glyph],
) -> (usize, usize) {
    let spans = reference_stream_spans(glyphs);
    let mut identical = 0usize;
    for span in &spans {
        let Some(src) = source.plain_stream(&span.object_id) else {
            continue;
        };
        let Some(dst) = output.plain_stream(&span.object_id) else {
            continue;
        };
        if span.byte_end <= src.len()
            && span.byte_end <= dst.len()
            && src[span.byte_start..span.byte_end] == dst[span.byte_start..span.byte_end]
        {
            identical += 1;
        }
    }
    (identical, spans.len())
}

fn place(segments: &[Segment], glyphs: &[Glyph]) -> Vec<Placed> {
    let by_id: HashMap<u32, &Glyph> = glyphs.iter().map(|glyph| (glyph.id, glyph)).collect();
    segments
        .iter()
        .enumerate()
        .filter_map(|(seg_index, seg)| {
            let glyph = by_id.get(seg.glyph_ids.first()?)?;
            Some(Placed {
                seg_index,
                page: seg.page_index,
                x: glyph.matrix[4],
                y: glyph.matrix[5],
                text: seg.text.clone(),
            })
        })
        .collect()
}

fn page_tops(placed: &[Placed]) -> HashMap<u32, f32> {
    let mut tops = HashMap::new();
    for line in placed {
        let slot = tops.entry(line.page).or_insert(line.y);
        if line.y > *slot {
            *slot = line.y;
        }
    }
    tops
}

fn reading_order(placed: &[Placed]) -> Vec<usize> {
    let mut pages: Vec<u32> = placed.iter().map(|line| line.page).collect();
    pages.sort_unstable();
    pages.dedup();
    let mut order = Vec::with_capacity(placed.len());
    for page in pages {
        let indexes: Vec<usize> = (0..placed.len())
            .filter(|&i| placed[i].page == page)
            .collect();
        let mut barriers = centered_barriers(placed, &indexes);
        barriers.sort_by(|&a, &b| {
            placed[b]
                .y
                .total_cmp(&placed[a].y)
                .then(placed[a].x.total_cmp(&placed[b].x))
        });
        let mut consumed = HashSet::new();
        let mut ceiling = f32::INFINITY;
        for barrier in &barriers {
            emit_band(
                &mut order,
                placed,
                &indexes,
                ceiling,
                placed[*barrier].y,
                &mut consumed,
            );
            if consumed.insert(*barrier) {
                order.push(*barrier);
            }
            ceiling = placed[*barrier].y;
        }
        emit_band(
            &mut order,
            placed,
            &indexes,
            ceiling,
            f32::NEG_INFINITY,
            &mut consumed,
        );
    }
    order
}

/// A heading set toward the center of the page, not in the left column.
/// Column-major order would otherwise read the appendix body before "Appendix".
fn centered_barriers(placed: &[Placed], indexes: &[usize]) -> Vec<usize> {
    let mut body_left = f32::MAX;
    for &index in indexes {
        if !is_heading(&placed[index].text) {
            body_left = body_left.min(placed[index].x);
        }
    }
    if body_left == f32::MAX {
        return Vec::new();
    }
    indexes
        .iter()
        .copied()
        .filter(|&index| is_heading(&placed[index].text) && placed[index].x - body_left > 36.0)
        .collect()
}

fn is_heading(text: &str) -> bool {
    is_reference_heading(text) || is_end_heading(text) || looks_like_section_title(text)
}

fn emit_band(
    order: &mut Vec<usize>,
    placed: &[Placed],
    indexes: &[usize],
    y_above: f32,
    y_below: f32,
    consumed: &mut HashSet<usize>,
) {
    let mut band: Vec<usize> = indexes
        .iter()
        .copied()
        .filter(|&index| {
            if consumed.contains(&index) {
                return false;
            }
            let y = placed[index].y;
            y <= y_above && y > y_below
        })
        .collect();
    if band.is_empty() {
        return;
    }
    band.sort_by(|&a, &b| placed[a].x.total_cmp(&placed[b].x));
    let mut columns: Vec<Vec<usize>> = Vec::new();
    for index in band {
        if let Some(column) = columns.iter_mut().find(|column| {
            let anchor = placed[column[0]].x;
            (placed[index].x - anchor).abs() <= 36.0
        }) {
            column.push(index);
        } else {
            columns.push(vec![index]);
        }
    }
    columns.sort_by(|a, b| placed[a[0]].x.total_cmp(&placed[b[0]].x));
    for column in &mut columns {
        column.sort_by(|&a, &b| placed[b].y.total_cmp(&placed[a].y));
        for index in column.iter().copied() {
            if consumed.insert(index) {
                order.push(index);
            }
        }
    }
}

fn is_running_header(placed: &[Placed], tops: &HashMap<u32, f32>, line: &Placed) -> bool {
    let norm = collapse(&line.text).to_lowercase();
    if norm.chars().count() < 4 {
        return false;
    }
    let Some(top) = tops.get(&line.page) else {
        return false;
    };
    if *top - line.y > 30.0 {
        return false;
    }
    placed.iter().any(|other| {
        other.page != line.page
            && tops
                .get(&other.page)
                .is_some_and(|other_top| *other_top - other.y <= 30.0)
            && collapse(&other.text).to_lowercase() == norm
    })
}

fn expand_mates(glyphs: &[Glyph], ids: &mut HashSet<u32>) {
    let seeds: Vec<&Glyph> = glyphs
        .iter()
        .filter(|glyph| ids.contains(&glyph.id))
        .collect();
    for glyph in glyphs {
        if ids.contains(&glyph.id) {
            continue;
        }
        if seeds.iter().any(|seed| same_line(seed, glyph)) {
            ids.insert(glyph.id);
        }
    }
    let ops: HashSet<(String, usize, usize)> = glyphs
        .iter()
        .filter(|glyph| ids.contains(&glyph.id))
        .filter_map(|glyph| {
            Some((
                glyph.source.object_id.clone()?,
                glyph.source.byte_start,
                glyph.source.byte_end,
            ))
        })
        .collect();
    for glyph in glyphs {
        let Some(object_id) = &glyph.source.object_id else {
            continue;
        };
        if ops.contains(&(
            object_id.clone(),
            glyph.source.byte_start,
            glyph.source.byte_end,
        )) {
            ids.insert(glyph.id);
        }
    }
}

fn same_line(seed: &Glyph, glyph: &Glyph) -> bool {
    if seed.page_index != glyph.page_index {
        return false;
    }
    let size = seed.font_size.max(1.0);
    if (seed.matrix[5] - glyph.matrix[5]).abs() > size * 0.55 {
        return false;
    }
    let left = seed.bbox[0].min(seed.bbox[2]) - 8.0;
    let right = seed.bbox[0].max(seed.bbox[2]) + 8.0;
    let x = glyph.matrix[4];
    x >= left && x <= right
}

pub(crate) fn is_reference_heading(text: &str) -> bool {
    let norm = normalize_heading(text);
    REF_HEADINGS.iter().any(|heading| norm == *heading)
}

fn is_end_heading(text: &str) -> bool {
    if text.trim().chars().count() > 48 {
        return false;
    }
    let norm = normalize_heading(text);
    END_HEADINGS
        .iter()
        .any(|heading| heading_match(&norm, heading))
}

fn heading_match(norm: &str, heading: &str) -> bool {
    norm == heading
        || norm
            .strip_prefix(heading)
            .is_some_and(|rest| rest.starts_with(' ') || rest.starts_with('　'))
}

fn is_section_break(text: &str) -> bool {
    if is_reference_heading(text) || is_reference_entry(text) {
        return false;
    }
    is_end_heading(text) || looks_like_section_title(text)
}

fn looks_like_section_title(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.chars().count() > 60 || trimmed.chars().count() < 3 {
        return false;
    }
    if contains_year(trimmed) || is_reference_entry(trimmed) {
        return false;
    }
    let Some(rest) = strip_section_prefix(trimmed) else {
        return false;
    };
    let words: Vec<&str> = rest.split_whitespace().collect();
    if words.is_empty() || words.len() > 8 {
        return false;
    }
    let Some(first) = words[0].chars().next() else {
        return false;
    };
    if first.is_ascii_lowercase() || words[0].ends_with(',') {
        return false;
    }
    true
}

pub(crate) fn is_reference_entry(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.chars().count() < 8 {
        return false;
    }
    if starts_bracket_marker(trimmed) {
        return true;
    }
    if starts_numbered_marker(trimmed) && (contains_year(trimmed) || looks_author_list(trimmed)) {
        return true;
    }
    looks_author_year_start(trimmed) && contains_year(trimmed)
}

fn starts_bracket_marker(text: &str) -> bool {
    let rest = text.trim_start();
    let after = if let Some(after) = rest.strip_prefix('[') {
        after
    } else if let Some(after) = rest.strip_prefix('［') {
        after
    } else {
        return false;
    };
    after.chars().next().is_some_and(|ch| ch.is_ascii_digit())
}

fn starts_numbered_marker(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.first().is_none_or(|b| !b.is_ascii_digit()) {
        return false;
    }
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    (1..=4).contains(&i) && i < bytes.len() && matches!(bytes[i], b'.' | b')')
}

fn looks_author_list(text: &str) -> bool {
    let Some(rest) = strip_section_prefix(text.trim()) else {
        return false;
    };
    looks_author_year_start(rest.trim())
}

fn looks_author_year_start(text: &str) -> bool {
    let text = text.trim_start();
    let Some((word, len)) = take_capital_word(text) else {
        return false;
    };
    if NOT_SURNAMES.contains(&word) {
        return false;
    }
    let after = text[len..].trim_start();
    after.starts_with(',') || after.starts_with('，')
}

fn contains_year(text: &str) -> bool {
    find_year(text).is_some()
}

fn find_year(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if bytes[i].is_ascii_digit() && (i == 0 || !bytes[i - 1].is_ascii_digit()) {
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j - i == 4 {
                if let Ok(year) = text[i..j].parse::<u32>() {
                    if (1900..2100).contains(&year) {
                        return Some(i);
                    }
                }
            }
            i = j;
            continue;
        }
        i += 1;
    }
    None
}

fn normalize_heading(text: &str) -> String {
    let trimmed = text.trim();
    let stripped = strip_section_prefix(trimmed).unwrap_or(trimmed).trim();
    let stripped = stripped.trim_matches(|ch: char| matches!(ch, ':' | '.' | '：' | '．'));
    collapse(stripped).to_lowercase()
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_section_prefix(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut i = 0;
    if bytes[0].is_ascii_digit() {
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
        } else if i < bytes.len() && bytes[i] == b')' {
            i += 1;
        }
    } else if bytes[0].is_ascii_alphabetic() {
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let word = &text[start..i];
        let single = i - start == 1;
        if !single && !is_roman(word) {
            return None;
        }
        if i < bytes.len() && matches!(bytes[i], b'.' | b')') {
            i += 1;
        } else if !is_roman(word) {
            return None;
        }
    } else {
        return None;
    }
    if i == 0 || i >= bytes.len() {
        return None;
    }
    if bytes[i].is_ascii_whitespace() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
    } else if !text[i..].chars().next().is_some_and(|ch| ch.is_uppercase()) {
        return None;
    }
    (i < bytes.len()).then_some(&text[i..])
}

fn is_roman(word: &str) -> bool {
    let bytes = word.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 6
        && bytes.iter().all(|b| {
            matches!(
                b,
                b'I' | b'V'
                    | b'X'
                    | b'L'
                    | b'C'
                    | b'D'
                    | b'M'
                    | b'i'
                    | b'v'
                    | b'x'
                    | b'l'
                    | b'c'
                    | b'd'
                    | b'm'
            )
        })
}

fn take_capital_word(text: &str) -> Option<(&str, usize)> {
    let mut chars = text.chars();
    let first = chars.next()?;
    if !first.is_uppercase() {
        return None;
    }
    let mut len = first.len_utf8();
    for ch in chars {
        if ch.is_alphabetic() || matches!(ch, '\'' | '’' | '-' | '‐') {
            len += ch.len_utf8();
        } else {
            break;
        }
    }
    if len == first.len_utf8() {
        return None;
    }
    Some((&text[..len], len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::glyph::{Disposition, Glyph, GlyphSource, SourceKind};

    fn glyph_at(id: u32, page: u32, x: f32, y: f32, text: &str) -> Glyph {
        Glyph {
            id,
            page_index: page,
            unicode: text.into(),
            unmapped: false,
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
                object_id: Some("5 0".into()),
                stream_index: 0,
                operator_index: id,
                byte_start: id as usize * 10,
                byte_end: id as usize * 10 + 8,
                resource_name: Some("F1".into()),
            },
        }
    }

    fn line(glyphs: &mut Vec<Glyph>, id: &mut u32, page: u32, x: f32, y: f32, text: &str) {
        for (i, ch) in text.chars().enumerate() {
            glyphs.push(glyph_at(*id, page, x + i as f32 * 6.0, y, &ch.to_string()));
            *id += 1;
        }
    }

    fn text_of(glyphs: &[Glyph], ids: &HashSet<u32>) -> String {
        let mut ordered: Vec<&Glyph> = glyphs
            .iter()
            .filter(|glyph| ids.contains(&glyph.id))
            .collect();
        ordered.sort_by(|a, b| {
            a.page_index
                .cmp(&b.page_index)
                .then(b.matrix[5].total_cmp(&a.matrix[5]))
                .then(a.matrix[4].total_cmp(&b.matrix[4]))
        });
        ordered.iter().map(|glyph| glyph.unicode.as_str()).collect()
    }

    #[test]
    fn heading_stops_at_appendix_and_skips_the_body() {
        let mut glyphs = Vec::new();
        let mut id = 0;
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            700.0,
            "The model uses attention.",
        );
        line(&mut glyphs, &mut id, 0, 72.0, 680.0, "7. References");
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            660.0,
            "[12] Smith, A. (2020). Attention is all you need.",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            84.0,
            640.0,
            "Proceedings of NeurIPS, 2020.",
        );
        line(&mut glyphs, &mut id, 0, 72.0, 600.0, "Appendix");
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            580.0,
            "A. Proofs of the claim.",
        );
        let ids = reference_glyph_ids(&glyphs);
        let kept = text_of(&glyphs, &ids);
        assert!(kept.contains("References"), "{kept}");
        assert!(kept.contains("[12] Smith"), "{kept}");
        assert!(kept.contains("Proceedings of NeurIPS"), "{kept}");
        assert!(!kept.contains("The model uses attention"), "{kept}");
        assert!(!kept.contains("Appendix"), "{kept}");
        assert!(!kept.contains("Proofs"), "{kept}");
    }

    #[test]
    fn columns_and_the_next_page_stay_in_the_section() {
        let mut glyphs = Vec::new();
        let mut id = 0;
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            700.0,
            "Body text in the left column.",
        );
        line(&mut glyphs, &mut id, 0, 72.0, 660.0, "References");
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            640.0,
            "[1] Smith, A. (2020). A paper about testing methods.",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            320.0,
            700.0,
            "[2] Jones, B. (2021). Another paper about testing.",
        );
        line(
            &mut glyphs,
            &mut id,
            1,
            72.0,
            760.0,
            "Attention Is All You Need",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            760.0,
            "Attention Is All You Need",
        );
        line(
            &mut glyphs,
            &mut id,
            1,
            72.0,
            700.0,
            "Doe, J. (2019). Continued reference entry title.",
        );
        line(&mut glyphs, &mut id, 1, 72.0, 660.0, "附录");
        line(&mut glyphs, &mut id, 1, 72.0, 640.0, "Proofs live here.");
        let ids = reference_glyph_ids(&glyphs);
        let kept = text_of(&glyphs, &ids);
        assert!(kept.contains("[2] Jones"), "{kept}");
        assert!(kept.contains("Doe, J."), "{kept}");
        assert!(!kept.contains("Body text"), "{kept}");
        assert!(!kept.contains("Attention Is All You Need"), "{kept}");
        assert!(!kept.contains("Proofs live here"), "{kept}");
        assert!(!kept.contains("附录"), "{kept}");
    }

    #[test]
    fn entries_without_a_heading_and_localized_headings() {
        let mut glyphs = Vec::new();
        let mut id = 0;
        line(&mut glyphs, &mut id, 0, 72.0, 700.0, "参考文献");
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            680.0,
            "[1] 作者甲。一篇论文。2020。",
        );
        line(&mut glyphs, &mut id, 0, 72.0, 660.0, "附录");
        let ids = reference_glyph_ids(&glyphs);
        let kept = text_of(&glyphs, &ids);
        assert!(kept.contains("参考文献"), "{kept}");
        assert!(kept.contains("作者甲"), "{kept}");
        assert!(!kept.contains("附录"), "{kept}");

        let mut glyphs = Vec::new();
        let mut id = 0;
        line(&mut glyphs, &mut id, 0, 72.0, 700.0, "See the notes below.");
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            660.0,
            "Müller, A. (2020). Ein Titel der Arbeit.",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            640.0,
            "Schmidt, B. (2021). Noch ein Titel der Arbeit.",
        );
        line(&mut glyphs, &mut id, 0, 72.0, 600.0, "Anhang");
        let ids = reference_glyph_ids(&glyphs);
        let kept = text_of(&glyphs, &ids);
        assert!(kept.contains("Müller"), "{kept}");
        assert!(kept.contains("Schmidt"), "{kept}");
        assert!(!kept.contains("See the notes"), "{kept}");
        assert!(!kept.contains("Anhang"), "{kept}");
    }

    #[test]
    fn a_centered_appendix_ends_the_section_before_its_body() {
        let mut glyphs = Vec::new();
        let mut id = 0;
        line(&mut glyphs, &mut id, 0, 72.0, 700.0, "References");
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            680.0,
            "[1] Smith, A. (2020). A paper about testing methods.",
        );
        line(&mut glyphs, &mut id, 1, 250.0, 720.0, "Appendix");
        line(
            &mut glyphs,
            &mut id,
            1,
            72.0,
            680.0,
            "A. Time complexity analysis stays outside.",
        );
        let ids = reference_glyph_ids(&glyphs);
        let kept = text_of(&glyphs, &ids);
        assert!(kept.contains("[1] Smith"), "{kept}");
        assert!(!kept.contains("Time complexity"), "{kept}");
        assert!(!kept.contains("Appendix"), "{kept}");
    }

    #[test]
    fn ci_papers_keep_the_bibliography_and_stop_before_the_appendix() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/ci");
        let attention = PdfDocument::open(root.join("neurips-2017-attention.pdf")).unwrap();
        let extracted = attention.extract();
        let ids = reference_glyph_ids(&extracted.glyphs);
        let kept = text_of(&extracted.glyphs, &ids);
        assert!(kept.contains("References"), "heading missing");
        assert!(
            kept.contains("[1]") || kept.contains("[10]"),
            "entries missing: {}",
            kept.chars().take(200).collect::<String>()
        );
        assert!(
            !kept.contains("The dominant sequence transduction"),
            "body was pulled into the bibliography"
        );

        let abbas = PdfDocument::open(root.join("pmlr-v202-abbas23a.pdf")).unwrap();
        let extracted = abbas.extract();
        let ids = reference_glyph_ids(&extracted.glyphs);
        let kept = text_of(&extracted.glyphs, &ids);
        assert!(kept.contains("References"), "heading missing");
        assert!(kept.contains("Abbas"), "entries missing");
        assert!(
            !kept.contains("We are grateful"),
            "acknowledgements before the heading were kept"
        );
        assert!(!kept.contains("Time complexity"), "appendix was kept");
    }

    #[test]
    fn numbered_steps_are_not_a_bibliography() {
        let mut glyphs = Vec::new();
        let mut id = 0;
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            700.0,
            "1. Collect the training data.",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            680.0,
            "2. Train the model on it.",
        );
        let ids = reference_glyph_ids(&glyphs);
        assert!(ids.is_empty(), "{}", text_of(&glyphs, &ids));
    }
}
