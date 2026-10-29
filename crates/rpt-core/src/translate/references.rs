//! Bibliography regions that must stay original text.
//!
//! A reference section starts at a heading (`References`, `Bibliography`,
//! `参考文献`, `Literatur`, `Références`, …) or at a run of numbered /
//! author-year entries. It continues across columns and pages in reading
//! order (left column top-to-bottom, then the next column) and stops at the
//! next section heading (`Appendix`, `附录`, `A. Proofs`, …). Those glyphs
//! are `kept_original` with reason `references`.

use crate::detmap::{HashMap, HashSet};

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
    let mut selected: HashSet<usize> = HashSet::default();
    let mut in_refs = false;
    let mut refs_x: Option<f32> = None;
    let mut refs_page: Option<u32> = None;
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
            // A contents row `References …… 20` is a title, not the
            // bibliography. The leaders and the page number sit on this
            // baseline and are not part of the title segment.
            if is_reference_heading(text)
                && !toc_title_beside_leaders(&segments[line.seg_index], glyphs)
            {
                // `References` followed by a sentence is a section about
                // citations, not the bibliography. A real list starts with
                // an entry (`[1] Smith` or `Bai, S. 2025`).
                if bibliography_follows(&order, &placed, &tops, pos) {
                    in_refs = true;
                    refs_x = Some(line.x);
                    refs_page = Some(line.page);
                    selected.insert(line.seg_index);
                    streak_from = None;
                    streak_entries = 0;
                    streak_gap = 0;
                }
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
                    refs_x = Some(placed[order[start]].x);
                    refs_page = Some(placed[order[start]].page);
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
        } else if refs_page.is_some_and(|page| line.page == page)
            && refs_x.is_some_and(|anchor| line.x < anchor - 96.0)
        {
            // The heading is in the right column. Body beside it on that
            // page is not a reference. The next page's left column is the
            // continuation, so the guard does not follow the section there.
        } else if is_section_break(text) {
            in_refs = false;
            refs_x = None;
            refs_page = None;
            streak_from = None;
            streak_entries = 0;
            streak_gap = 0;
        } else {
            selected.insert(line.seg_index);
        }
    }

    let mut ids = HashSet::default();
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
    let mut seen = HashSet::default();
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
    let mut tops = HashMap::default();
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
        let mut consumed = HashSet::default();
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
    // Seeds by page, sorted by baseline. Testing every glyph against every
    // seed was quadratic and took minutes on a long bibliography.
    let mut seeds: HashMap<u32, (Vec<&Glyph>, f32)> = HashMap::default();
    for glyph in glyphs.iter().filter(|glyph| ids.contains(&glyph.id)) {
        let entry = seeds.entry(glyph.page_index).or_insert((Vec::new(), 1.0));
        entry.0.push(glyph);
        entry.1 = entry.1.max(glyph.font_size);
    }
    for (page_seeds, _) in seeds.values_mut() {
        page_seeds.sort_by(|left, right| left.matrix[5].total_cmp(&right.matrix[5]));
    }
    let mut mates = Vec::new();
    for glyph in glyphs {
        if ids.contains(&glyph.id) {
            continue;
        }
        let Some((page_seeds, size)) = seeds.get(&glyph.page_index) else {
            continue;
        };
        let reach = size * 0.55 + 0.01;
        let y = glyph.matrix[5];
        let from = page_seeds.partition_point(|seed| seed.matrix[5] < y - reach);
        let to = page_seeds.partition_point(|seed| seed.matrix[5] <= y + reach);
        if page_seeds[from..to]
            .iter()
            .any(|seed| same_line(seed, glyph))
        {
            mates.push(glyph.id);
        }
    }
    ids.extend(mates);
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

/// The lines after a `References` heading. An entry opens the bibliography.
/// A sentence (`References are placed alongside…`) does not.
fn bibliography_follows(
    order: &[usize],
    placed: &[Placed],
    tops: &HashMap<u32, f32>,
    pos: usize,
) -> bool {
    let heading = &placed[order[pos]];
    let mut looked = 0usize;
    let mut roster = 0usize;
    for &placed_index in order.iter().skip(pos + 1) {
        let line = &placed[placed_index];
        if is_running_header(placed, tops, line) {
            continue;
        }
        // The heading sits in the right column. The left column is body.
        // Reading order emits that column first, and a sentence there would
        // close the section before the author list is seen.
        if line.page == heading.page && (line.x - heading.x).abs() > 96.0 {
            continue;
        }
        let text = line.text.trim();
        if text.chars().count() < 8 {
            // `[1]` is often its own segment, ahead of the author line.
            if starts_bracket_marker(text)
                || starts_numbered_marker(text)
                || starts_alpha_cite_key(text)
            {
                return true;
            }
            continue;
        }
        if is_reference_entry(text)
            || looks_author_year_start(text)
            || initials_author_start(text)
            || organisation_entry(text)
            || roster_bearing_year(text)
        {
            return true;
        }
        // `OpenAI Josh Achiam, …` wraps, and `and` appears on the year line.
        // That is still the bibliography. A sentence is not.
        if author_roster(text) {
            if contains_year(text) {
                return true;
            }
            roster += 1;
            if roster >= 8 {
                return false;
            }
            continue;
        }
        if is_section_break(text) || prose_sentence(text) {
            return false;
        }
        looked += 1;
        if looked >= 4 {
            return false;
        }
    }
    false
}

/// `Ojas Ahuja, Jiacheng Xu, Akshay Gupta, Kevin`. Names, commas, and
/// `and` / `et al.`. A lowercase word is a sentence, not a roster.
fn author_roster(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.chars().count() < 8 {
        return false;
    }
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if words.len() < 3 {
        return false;
    }
    let mut names = 0usize;
    let mut other = 0usize;
    for word in &words {
        let bare: String = word.chars().filter(|ch| ch.is_ascii_alphabetic()).collect();
        if bare.is_empty() {
            continue;
        }
        let lower = bare.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "and" | "et" | "al" | "eds" | "ed" | "jr" | "sr"
        ) {
            continue;
        }
        if bare
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_uppercase())
        {
            names += 1;
        } else {
            other += 1;
        }
    }
    names >= 2 && other == 0 && (trimmed.contains(',') || trimmed.contains('，'))
}

/// `…, and Red Avila et al. 2023. Gpt-4 technical report.`
/// The names stop at the year. The title after it may be lowercase.
fn roster_bearing_year(text: &str) -> bool {
    let Some(at) = find_year(text) else {
        return false;
    };
    let head = text[..at].trim().trim_end_matches(['.', ',', ';', ':']);
    author_roster(head)
}

fn prose_sentence(text: &str) -> bool {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 6 {
        return false;
    }
    const FUNCTION: &[&str] = &[
        "the", "and", "are", "is", "was", "were", "this", "that", "with", "for", "from", "using",
        "may", "can", "not", "also", "their", "these", "those",
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

/// `References …… 20` is a contents row. The title segment is only the word.
fn toc_title_beside_leaders(seg: &Segment, glyphs: &[Glyph]) -> bool {
    let ids: HashSet<u32> = seg.glyph_ids.iter().copied().collect();
    let mine: Vec<&Glyph> = glyphs
        .iter()
        .filter(|glyph| ids.contains(&glyph.id))
        .collect();
    if mine.is_empty() {
        return false;
    }
    let y = mine[0].matrix[5];
    let size = mine
        .iter()
        .map(|glyph| glyph.font_size)
        .fold(1.0f32, f32::max)
        .max(1.0);
    let right = mine
        .iter()
        .map(|glyph| glyph.bbox[0].max(glyph.bbox[2]).max(glyph.matrix[4]))
        .fold(0.0f32, f32::max);
    let page = mine[0].page_index;
    let mut side: Vec<&Glyph> = glyphs
        .iter()
        .filter(|glyph| {
            glyph.page_index == page
                && !ids.contains(&glyph.id)
                && (glyph.matrix[5] - y).abs() <= size * 0.55
                && glyph.matrix[4] >= right - 1.0
        })
        .collect();
    side.sort_by(|left, right| left.matrix[4].total_cmp(&right.matrix[4]));
    let mut dots = 0usize;
    let mut page_label = String::new();
    for glyph in side {
        let text = glyph.unicode.trim();
        if text.is_empty() {
            continue;
        }
        if text.chars().all(is_toc_leader) {
            dots += text.chars().count();
            continue;
        }
        page_label.push_str(text);
    }
    dots >= 4 && is_toc_page_label(&page_label)
}

fn is_toc_leader(ch: char) -> bool {
    matches!(ch, '.' | '·' | '…' | '•' | '⋅' | '‧' | '․')
}

fn is_toc_page_label(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > 6 {
        return false;
    }
    if text.bytes().all(|byte| byte.is_ascii_digit()) {
        return text.len() <= 3;
    }
    text.chars().count() >= 2
        && text.chars().all(|ch| {
            matches!(
                ch.to_ascii_lowercase(),
                'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'
            )
        })
}

fn is_end_heading(text: &str) -> bool {
    if appendix_label_start(text) {
        return true;
    }
    if text.trim().chars().count() > 48 {
        return false;
    }
    let norm = normalize_heading(text);
    END_HEADINGS
        .iter()
        .any(|heading| heading_match(&norm, heading))
}

/// `Appendix A.2.2. File written The write it issued was:` is a heading run
/// into its first sentence. The `Appendix <label>` opening ends the
/// bibliography however long the line is.
fn appendix_label_start(text: &str) -> bool {
    let mut words = text.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    if !matches!(first, "Appendix" | "APPENDIX" | "Appendices") {
        return false;
    }
    let Some(label) = words.next() else {
        return false;
    };
    let label = label.trim_end_matches(['.', ':']);
    let mut chars = label.chars();
    let lead_ok = chars
        .next()
        .is_some_and(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit());
    lead_ok
        && label.len() <= 10
        && chars.all(|ch| ch.is_ascii_digit() || ch == '.' || ch.is_ascii_uppercase())
}

fn heading_match(norm: &str, heading: &str) -> bool {
    norm == heading
        || norm
            .strip_prefix(heading)
            .is_some_and(|rest| rest.starts_with(' ') || rest.starts_with('　'))
}

fn is_section_break(text: &str) -> bool {
    if appendix_label_start(text) {
        return true;
    }
    if is_reference_heading(text) || is_reference_entry(text) {
        return false;
    }
    is_end_heading(text)
        || looks_like_section_title(text)
        || appendix_letter_heading(text)
        || is_caption_heading(text)
}

/// `Figure 3: An example of the attention mechanism` and `Table 2. Results`
/// follow the bibliography when the appendix has no "Appendix" heading.
/// A citation that merely mentions a figure still starts with `[1]` or a year.
fn is_caption_heading(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    let Some(rest) = ["figure", "fig.", "table", "tab.", "fig"]
        .iter()
        .find_map(|prefix| {
            let rest = lower.strip_prefix(prefix)?;
            let boundary = rest.chars().next()?;
            if prefix.chars().all(|ch| ch.is_ascii_alphabetic())
                && !(boundary.is_ascii_whitespace() || boundary == '.' || boundary.is_ascii_digit())
            {
                return None;
            }
            Some(rest)
        })
    else {
        return false;
    };
    let rest = rest.trim_start_matches([' ', '.']);
    let mut digits = 0usize;
    for ch in rest.chars() {
        if ch.is_ascii_digit() {
            digits += 1;
            if digits > 3 {
                return false;
            }
            continue;
        }
        return digits > 0 && matches!(ch, ':' | '.' | ' ' | '：');
    }
    false
}

/// `A Derivations` and `A.1 From Region-Specific Sufficiency`. AAAI letters
/// the appendix without the word Appendix. `A definition of…` is a sentence.
fn appendix_letter_heading(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.chars().count() > 80 {
        return false;
    }
    let bytes = trimmed.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_uppercase() || bytes[1].is_ascii_alphabetic() {
        return false;
    }
    let mut index = 1usize;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let start = index;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
            index += 1;
        }
        if index == start {
            return false;
        }
    }
    if index >= bytes.len() || !bytes[index].is_ascii_whitespace() {
        return false;
    }
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    trimmed[index..]
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_uppercase())
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
    if starts_bracket_marker(trimmed) || starts_alpha_cite_key(trimmed) {
        return true;
    }
    if starts_numbered_marker(trimmed) && (contains_year(trimmed) || looks_author_list(trimmed)) {
        // `2. Skip-Thought (Kiros et al., 2015), a sequence-to-sequence model`
        // cites a paper inside a numbered appendix item. Two of those used to
        // open a bibliography and keep the rest of the appendix in English.
        if numbered_prose_item(trimmed) {
            return false;
        }
        return true;
    }
    ((looks_author_year_start(trimmed) || initials_author_start(trimmed)) && contains_year(trimmed))
        || organisation_entry(trimmed)
}

/// `T. Chen, S. Lu, K. Zhao` and `J.-B. Alayrac and A. Smith`: given-name
/// initials first, then the surname. The comma or `and` after the surname
/// separates it from a sentence such as `A. Note on this`.
fn initials_author_start(text: &str) -> bool {
    let chars: Vec<char> = text.trim_start().chars().collect();
    let mut i = 0usize;
    let mut initials = 0usize;
    // One or more `X.` / `X.-Y.` initials, separated by spaces or hyphens.
    loop {
        if i + 1 < chars.len() && chars[i].is_uppercase() && chars[i + 1] == '.' {
            initials += 1;
            i += 2;
            if i < chars.len() && matches!(chars[i], '-' | '‐') {
                i += 1;
                continue;
            }
            if i < chars.len() && chars[i] == ' ' {
                i += 1;
                continue;
            }
        }
        break;
    }
    if initials == 0 || i >= chars.len() || !chars[i].is_uppercase() {
        return false;
    }
    let start = i;
    while i < chars.len()
        && (chars[i].is_alphabetic() || matches!(chars[i], '\'' | '’' | '-' | '‐'))
    {
        i += 1;
    }
    if i - start < 2 {
        return false;
    }
    let rest: String = chars[i..].iter().collect();
    let rest = rest.trim_start();
    rest.starts_with(',') || rest.starts_with("and ")
}

/// `Anthropic. Introducing claude opus 4.8, 2026a. URL https://…`: an
/// organisation author, a title, a year, and a URL or an arXiv id.
fn organisation_entry(text: &str) -> bool {
    let trimmed = text.trim();
    let Some((word, len)) = take_capital_word(trimmed) else {
        return false;
    };
    if NOT_SURNAMES.contains(&word) || !trimmed[len..].starts_with(". ") {
        return false;
    }
    contains_year(trimmed)
        && (trimmed.contains("URL ") || trimmed.contains("arXiv") || trimmed.contains("http"))
}

/// `1. CBoW, the average of the embeddings` is a sentence. `1. Vaswani, A.`
/// is the start of a bibliography entry.
fn numbered_prose_item(text: &str) -> bool {
    if looks_author_list(text) {
        return false;
    }
    let Some(rest) = strip_section_prefix(text.trim()) else {
        return false;
    };
    let words: Vec<&str> = rest.split_whitespace().collect();
    if words.len() < 6 {
        return false;
    }
    let hits = words
        .iter()
        .filter(|word| {
            let bare = word.trim_matches(|ch: char| !ch.is_ascii_alphabetic());
            matches!(
                bare.to_ascii_lowercase().as_str(),
                "the"
                    | "a"
                    | "an"
                    | "of"
                    | "to"
                    | "for"
                    | "with"
                    | "and"
                    | "we"
                    | "is"
                    | "are"
                    | "in"
                    | "on"
                    | "by"
                    | "that"
                    | "from"
                    | "as"
                    | "or"
            )
        })
        .count();
    hits >= 2
}

/// `[AGTG21]`, `[BE98]`, and `[AHK+ 02]`. A numeric `[1]` is handled
/// separately. `[Note]` has no year digits and is not a key.
fn starts_alpha_cite_key(text: &str) -> bool {
    let rest = text.trim_start();
    let Some(after) = rest.strip_prefix('[').or_else(|| rest.strip_prefix('［')) else {
        return false;
    };
    let bytes = after.as_bytes();
    if bytes.first().is_none_or(|byte| !byte.is_ascii_alphabetic()) {
        return false;
    }
    let mut index = 0usize;
    let mut letters = 0usize;
    let mut digits = 0usize;
    while index < bytes.len() && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'+') {
        if bytes[index].is_ascii_alphabetic() {
            letters += 1;
        }
        if bytes[index].is_ascii_digit() {
            digits += 1;
        }
        index += 1;
        if index > 12 {
            return false;
        }
    }
    if letters < 2 {
        return false;
    }
    if index < bytes.len() && bytes[index] == b' ' {
        index += 1;
        let year = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index - year != 2 {
            return false;
        }
        digits += 2;
    }
    digits > 0
        && after[index..]
            .chars()
            .next()
            .is_some_and(|ch| ch == ']' || ch == '］')
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
    let digits = after.chars().take_while(|ch| ch.is_ascii_digit()).count();
    // [1] and [12, 3]. An ORCID `[0000-0002-3723-6960]` is four digits
    // and must not open a bibliography.
    if digits == 0 || digits > 3 {
        return false;
    }
    matches!(
        after.chars().nth(digits),
        Some(']' | '］' | ',' | '，' | '-' | '–')
    )
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
    let Some(rest) = after.strip_prefix(',').or_else(|| after.strip_prefix('，')) else {
        return false;
    };
    // `CBoW, the average` is a sentence. `Vaswani, A.` and `Smith, 2015` are names.
    rest.trim_start()
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
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

    #[test]
    fn initials_first_and_organisation_entries_open_a_bibliography() {
        assert!(is_reference_entry(
            "T. Chen, S. Lu, K. Zhao, W. Meng, H. Teng, et al. Harnessx: A composable foundry. 2026."
        ));
        assert!(is_reference_entry(
            "J.-B. Alayrac and A. Smith. Flamingo. In NeurIPS, 2022."
        ));
        assert!(is_reference_entry(
            "Anthropic. Introducing claude opus 4.8, 2026a. URL https://www.anthropic.com/news/"
        ));
        assert!(!is_reference_entry(
            "A. Note on the method we used in 2026 for this table."
        ));
        assert!(!is_reference_entry(
            "Figure. The results in 2026 are shown at http://x.y"
        ));
    }
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
    fn a_right_column_heading_does_not_swallow_the_left_column() {
        let mut glyphs = Vec::new();
        let mut id = 0;
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            640.0,
            "The layout stays translated.",
        );
        line(&mut glyphs, &mut id, 0, 320.0, 660.0, "REFERENCES");
        line(
            &mut glyphs,
            &mut id,
            0,
            320.0,
            640.0,
            "[1] Smith, A. (2020). Attention matters.",
        );
        let ids = reference_glyph_ids(&glyphs);
        let kept = text_of(&glyphs, &ids);
        assert!(kept.contains("Smith"), "{kept}");
        assert!(!kept.contains("layout"), "{kept}");
        line(
            &mut glyphs,
            &mut id,
            1,
            72.0,
            700.0,
            "Hope, T. (2022). Scaling creative inspiration.",
        );
        let ids = reference_glyph_ids(&glyphs);
        let kept = text_of(&glyphs, &ids);
        assert!(kept.contains("Hope"), "{kept}");
        assert!(!kept.contains("layout"), "{kept}");
    }

    #[test]
    fn an_author_roster_under_references_includes_the_wrapped_names() {
        // The year sits on a later line, and `and` is in the roster.
        // The left column beside the heading stays translatable.
        let mut glyphs = Vec::new();
        let mut id = 0;
        line(&mut glyphs, &mut id, 0, 72.0, 640.0, "Study stays here.");
        line(&mut glyphs, &mut id, 0, 306.0, 660.0, "References");
        line(
            &mut glyphs,
            &mut id,
            0,
            306.0,
            640.0,
            "OpenAI Josh Achiam, Steven Adler, Sandhini Agarwal,",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            306.0,
            626.0,
            "Shyamal Anadkat, and Red Avila et al. 2023. Gpt-4 technical report.",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            306.0,
            600.0,
            "Ojas Ahuja, Jiacheng Xu, Akshay Gupta, Kevin",
        );
        let ids = reference_glyph_ids(&glyphs);
        let kept = text_of(&glyphs, &ids);
        assert!(kept.contains("Ojas"), "{kept}");
        assert!(kept.contains("OpenAI"), "{kept}");
        assert!(!kept.contains("Study"), "{kept}");
    }

    #[test]
    fn digest_page10_author_list_is_a_reference() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/cache/arxiv-2410.22360.pdf");
        if !path.exists() {
            return;
        }
        let doc = PdfDocument::open(&path).unwrap();
        let extraction = doc.extract_with(&crate::extract::ExtractOptions::default());
        let ids = reference_glyph_ids(&extraction.glyphs);
        let seg = segment_glyphs(&extraction.glyphs);
        let ojas = seg
            .iter()
            .find(|item| item.page_index == 9 && item.text.contains("Ojas Ahuja"))
            .expect("ojas line");
        assert!(
            ojas.glyph_ids.iter().all(|id| ids.contains(id)),
            "not a reference: {}",
            ojas.text
        );
        let appendix = seg
            .iter()
            .find(|item| item.text.contains("XML-formatted tables"));
        if let Some(appendix) = appendix {
            assert!(
                appendix.glyph_ids.iter().all(|id| !ids.contains(id)),
                "appendix swallowed: {}",
                appendix.text
            );
        }
        let body = seg
            .iter()
            .find(|item| item.page_index == 9 && item.text.contains("We only study"));
        if let Some(body) = body {
            assert!(
                body.glyph_ids.iter().all(|id| !ids.contains(id)),
                "left column swallowed: {}",
                body.text
            );
        }
    }

    #[test]
    fn a_references_heading_followed_by_prose_is_not_a_bibliography() {
        let mut glyphs = Vec::new();
        let mut id = 0;
        line(&mut glyphs, &mut id, 0, 72.0, 700.0, "References");
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            680.0,
            "References are placed alongside their citations as sidenotes, as well.",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            660.0,
            "This can be accomplished using the normal cite command.",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            400.0,
            660.0,
            "Edward R. Tufte. Beautiful Evidence. Graphics Press, 2006.",
        );
        line(
            &mut glyphs,
            &mut id,
            1,
            72.0,
            760.0,
            "Full page figures may be placed in a figure environment.",
        );
        let ids = reference_glyph_ids(&glyphs);
        let kept = text_of(&glyphs, &ids);
        assert!(
            !kept.contains("sidenotes") && !kept.contains("figure environment"),
            "{kept}"
        );
    }

    #[test]
    fn a_contents_row_named_references_does_not_start_the_bibliography() {
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
        line(&mut glyphs, &mut id, 0, 72.0, 660.0, "References");
        line(
            &mut glyphs,
            &mut id,
            0,
            200.0,
            660.0,
            "....................",
        );
        line(&mut glyphs, &mut id, 0, 400.0, 660.0, "20");
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            640.0,
            "A software development life cycle is a method.",
        );
        let ids = reference_glyph_ids(&glyphs);
        let kept = text_of(&glyphs, &ids);
        assert!(
            !kept.contains("life cycle") && !kept.contains("References"),
            "{kept}"
        );
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
    fn a_lettered_appendix_stops_the_references() {
        let mut glyphs = Vec::new();
        let mut id = 0;
        line(&mut glyphs, &mut id, 0, 72.0, 700.0, "References");
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            680.0,
            "Bai, S.; Cai, Y. 2025. Qwen technical report.",
        );
        line(&mut glyphs, &mut id, 1, 72.0, 760.0, "A Derivations");
        line(
            &mut glyphs,
            &mut id,
            1,
            72.0,
            740.0,
            "A.1 From Region-Specific Sufficiency to",
        );
        line(
            &mut glyphs,
            &mut id,
            1,
            72.0,
            720.0,
            "We provide a derivation of the implication used in the main text.",
        );
        let ids = reference_glyph_ids(&glyphs);
        let kept = text_of(&glyphs, &ids);
        assert!(kept.contains("Qwen"), "{kept}");
        assert!(!kept.contains("Derivations"), "{kept}");
        assert!(!kept.contains("Region-Specific"), "{kept}");
        assert!(!kept.contains("derivation"), "{kept}");
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
    fn a_figure_caption_after_the_bibliography_is_not_a_reference() {
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
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            660.0,
            "[40] Zhu, Y. Fast and accurate shift-reduce parsing. ACL, 2013.",
        );
        line(
            &mut glyphs,
            &mut id,
            1,
            108.0,
            120.0,
            "Figure 3: An example of the attention mechanism following long-distance dependencies.",
        );
        line(
            &mut glyphs,
            &mut id,
            1,
            108.0,
            100.0,
            "The heads clearly learned to perform different tasks.",
        );
        let ids = reference_glyph_ids(&glyphs);
        let kept = text_of(&glyphs, &ids);
        assert!(kept.contains("[40] Zhu"), "{kept}");
        assert!(!kept.contains("Figure 3"), "{kept}");
        assert!(!kept.contains("different tasks"), "{kept}");
        assert!(!is_caption_heading("fight the long deadline today"));
        assert!(is_caption_heading(
            "Figure 3: An example of the attention mechanism"
        ));
        assert!(is_caption_heading("Table 2. Complexity per layer"));
    }

    #[test]
    fn saved_papers_do_not_keep_a_caption_inside_the_bibliography() {
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
        for path in paths {
            let extracted = PdfDocument::open(&path).unwrap().extract();
            let ids = reference_glyph_ids(&extracted.glyphs);
            let kept = text_of(&extracted.glyphs, &ids);
            for needle in [
                "Figure 1:",
                "Figure 2:",
                "Figure 3:",
                "Figure 4:",
                "Figure 5:",
                "Table 1:",
                "Table 2:",
            ] {
                assert!(
                    !kept.contains(needle),
                    "{} kept a caption {needle}",
                    path.display()
                );
            }
        }
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
    fn an_alpha_bibliography_stays_original_and_stops_at_the_appendix() {
        let mut glyphs = Vec::new();
        let mut id = 0;
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            720.0,
            "The bound holds for every metric space.",
        );
        line(&mut glyphs, &mut id, 0, 72.0, 690.0, "References");
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            670.0,
            "[AGTG21] C. J. Argue, Anupam Gupta, and Guru Guruganesh. Chasing convex bodies",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            90.0,
            656.0,
            "with linear competitive ratio. J. ACM, 68(5):32:1-32:10, 2021.",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            636.0,
            "[AHK+ 02] Eric J. Anderson and Michael E. Saks. On list update.",
        );
        line(&mut glyphs, &mut id, 0, 72.0, 400.0, "A     Omitted proofs");
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            360.0,
            "Proof. By the work function recurrence, we have the bound.",
        );
        let ids = reference_glyph_ids(&glyphs);
        let kept = text_of(&glyphs, &ids);
        assert!(kept.contains("References"), "{kept}");
        assert!(kept.contains("Chasing convex bodies"), "{kept}");
        assert!(kept.contains("Anderson"), "{kept}");
        assert!(kept.contains("competitive ratio"), "{kept}");
        assert!(!kept.contains("metric space"), "{kept}");
        assert!(!kept.contains("Omitted"), "{kept}");
        assert!(!kept.contains("work function"), "{kept}");
        assert!(!starts_alpha_cite_key("[Note]"));
        assert!(!starts_alpha_cite_key("[1]"));
        assert!(starts_alpha_cite_key("[BE98]"));
        assert!(starts_alpha_cite_key("[AHK+ 02]"));
    }

    #[test]
    fn the_k_server_alpha_keys_stay_original_through_the_appendix() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/cache/arxiv-2609.15979.pdf");
        if !path.is_file() {
            return;
        }
        let pdf = PdfDocument::open(&path).unwrap();
        let extracted = pdf.extract();
        let ids = reference_glyph_ids(&extracted.glyphs);
        let kept = text_of(&extracted.glyphs, &ids);
        assert!(
            kept.contains("Chasingconvexbodies"),
            "title was translated or dropped"
        );
        assert!(kept.contains("Onlistupdate"), "entry was dropped");
        assert!(
            !kept.contains("Omitted"),
            "appendix heading was kept with the bibliography"
        );
        assert!(
            !kept.contains("workfunctionrecurrence"),
            "the appendix proof was kept"
        );
    }

    #[test]
    fn a_numbered_model_list_is_not_a_bibliography() {
        let mut glyphs = Vec::new();
        let mut id = 0;
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            700.0,
            "B.3 SENTENCE REPRESENTATION MODELS",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            680.0,
            "1. CBoW, the average of the GloVe embeddings of the tokens in the sentence.",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            660.0,
            "2. Skip-Thought (Kiros et al., 2015), a sequence-to-sequence model trained to generate the next sentence.",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            640.0,
            "3. InferSent (Conneau et al., 2017), a BiLSTM with max-pooling trained on MNLI.",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            600.0,
            "The GLUE website limits users to two submissions per day.",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            520.0,
            "1. Vaswani, A., Shazeer, N., Parmar, N., Uszkoreit, J., Jones, L., Gomez, A. N., Kaiser, L., and Polosukhin, I. Attention is all you need. In NIPS, 2017.",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            500.0,
            "2. Devlin, J., Chang, M., Lee, K., and Toutanova, K. BERT: Pre-training of deep bidirectional transformers. In NAACL, 2019.",
        );
        let ids = reference_glyph_ids(&glyphs);
        let kept = text_of(&glyphs, &ids);
        assert!(!kept.contains("CBoW"), "{kept}");
        assert!(!kept.contains("Skip-Thought"), "{kept}");
        assert!(!kept.contains("GLUE website"), "{kept}");
        assert!(kept.contains("Vaswani"), "{kept}");
        assert!(kept.contains("Devlin"), "{kept}");
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

    #[test]
    fn an_orcid_does_not_swallow_the_introduction() {
        // Two `[0000-0002-…]` author ids used to look like `[1]` entries and
        // keep every paragraph after the byline until the next heading.
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        line(
            &mut glyphs,
            &mut id,
            0,
            200.0,
            700.0,
            "[0000-0002-3723-6960]",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            360.0,
            700.0,
            "[0000-0001-8513-3491]",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            640.0,
            "The introduction explains the method and the data.",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            500.0,
            "[1] Smith, A. (2020). Attention matters.",
        );
        line(
            &mut glyphs,
            &mut id,
            0,
            72.0,
            480.0,
            "[2] Jones, B. (2019). Layout analysis.",
        );
        let ids = reference_glyph_ids(&glyphs);
        let kept = text_of(&glyphs, &ids);
        assert!(!kept.contains("introduction"), "{kept}");
        assert!(kept.contains("Smith"), "{kept}");
    }
    #[test]
    fn an_appendix_label_run_into_its_sentence_ends_the_bibliography() {
        assert!(is_section_break(
            "Appendix A.2.2. File written The write it issued was:"
        ));
        assert!(!appendix_label_start(
            "Appendix material is cited in Smith (2020)."
        ));
    }
}
