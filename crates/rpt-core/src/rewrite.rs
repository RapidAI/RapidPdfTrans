//! Write a translation back into the PDF.
//!
//! Original text-showing operators are replaced with the same number of spaces,
//! so every other byte offset in the stream stays valid. Replacement text is
//! drawn in a new page content stream with a subset CID font (Identity-H,
//! ToUnicode). A segment that cannot be fitted, or whose operator is shared
//! with a glyph that must stay, keeps the original operators.
//!
//! Reference-section glyphs are already `kept_original`. Their operators are
//! not in the deletion set, so those bytes stay identical.
//!
//! Horizontal advances come from `hmtx`. This is not OpenType GSUB shaping.

use std::collections::{HashMap, HashSet};

use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

use crate::color::Color;
use crate::error::{Error, Result};
use crate::extract::Extraction;
use crate::font::{load_cjk_font, subset_ttf, SubsetFont};
use crate::glyph::{Disposition, Glyph, GlyphSource, SourceKind};
use crate::pdfutil::{dict_of, object_id_string};
use crate::translate::TranslateReport;

#[derive(Clone, Debug, Default)]
pub struct RewriteOptions {
    pub bilingual: bool,
    /// Font bytes to subset. `None` searches `RPT_CJK_FONT` and common CJK fonts.
    pub font_bytes: Option<Vec<u8>>,
}

struct Span {
    object_id: ObjectId,
    start: usize,
    end: usize,
}

struct Drawn {
    page: u32,
    x: f32,
    y: f32,
    size: f32,
    color: Color,
    cids: Vec<u16>,
}

/// Rewrite `extraction` using `report` and remember the dispositions on the glyphs.
pub fn rewrite_translation(
    doc: &mut Document,
    extraction: &mut Extraction,
    report: &TranslateReport,
    opts: &RewriteOptions,
) -> Result<()> {
    let by_id = glyph_index(&extraction.glyphs);
    let mut keep: HashMap<u32, String> = HashMap::new();
    let mut non_text: HashMap<u32, String> = HashMap::new();
    let mut rewrite_ids: HashSet<u32> = HashSet::new();

    for glyph in &extraction.glyphs {
        if glyph.disposition.is_final() {
            continue;
        }
        if glyph.invisible {
            non_text.insert(glyph.id, "invisible".into());
            continue;
        }
        if let Some(reason) = inherent_keep(glyph) {
            keep.insert(glyph.id, reason.into());
        }
    }

    let mut planned: Vec<(usize, String)> = Vec::new();
    for (index, segment) in report.segments.iter().enumerate() {
        if segment.glyph_ids.is_empty() {
            continue;
        }
        if segment.glyph_ids.iter().any(|id| {
            extraction
                .glyphs
                .get(by_id[id])
                .is_some_and(|glyph| glyph.disposition.is_final() || keep.contains_key(id))
        }) {
            for id in &segment.glyph_ids {
                if !extraction.glyphs[by_id[id]].disposition.is_final() {
                    keep.entry(*id).or_insert_with(|| "kept".into());
                }
            }
            continue;
        }
        if segment.source.trim().is_empty() || segment.translated.trim().is_empty() {
            for id in &segment.glyph_ids {
                keep.insert(*id, "whitespace".into());
            }
            continue;
        }
        let glyphs: Vec<&Glyph> = segment
            .glyph_ids
            .iter()
            .filter_map(|id| extraction.glyphs.get(by_id[id]))
            .collect();
        if !operators_are_private(&glyphs, &extraction.glyphs) {
            for id in &segment.glyph_ids {
                keep.insert(*id, "shared-operator".into());
            }
            continue;
        }
        planned.push((index, segment.translated.clone()));
        rewrite_ids.extend(segment.glyph_ids.iter().copied());
    }

    let font_bytes = opts.font_bytes.clone().or_else(load_cjk_font);
    let mut drawn = Vec::new();
    let mut succeeded: HashSet<u32> = HashSet::new();
    let mut embedded: Option<SubsetFont> = None;
    if let Some(font_bytes) = font_bytes {
        let chars = planned
            .iter()
            .flat_map(|(_, text)| text.chars().map(|ch| ch as u32))
            .collect::<Vec<_>>();
        if let Some(font) = subset_ttf(&font_bytes, &chars) {
            for (index, text) in &planned {
                let segment = &report.segments[*index];
                let glyphs: Vec<&Glyph> = segment
                    .glyph_ids
                    .iter()
                    .filter_map(|id| extraction.glyphs.get(by_id[id]))
                    .collect();
                match layout_segment(&glyphs, text, &font, extraction, opts.bilingual) {
                    Some(lines) => {
                        succeeded.extend(segment.glyph_ids.iter().copied());
                        drawn.extend(lines);
                    }
                    None => {
                        let reason = if text
                            .chars()
                            .any(|ch| !font.glyphs.contains_key(&(ch as u32)))
                        {
                            "missing-glyph"
                        } else {
                            "overflow"
                        };
                        for id in &segment.glyph_ids {
                            keep.insert(*id, reason.into());
                        }
                    }
                }
            }
            embedded = Some(font);
        } else {
            for id in &rewrite_ids {
                keep.insert(*id, "missing-glyph".into());
            }
        }
    } else {
        for id in &rewrite_ids {
            keep.insert(*id, "no-font".into());
        }
    }

    let mut spans = Vec::new();
    if !opts.bilingual {
        for id in &succeeded {
            if keep.contains_key(id) {
                continue;
            }
            let glyph = &extraction.glyphs[by_id[id]];
            if let Some(span) = span_of(&glyph.source) {
                if !spans.iter().any(|other: &Span| {
                    other.object_id == span.object_id
                        && other.start == span.start
                        && other.end == span.end
                }) {
                    spans.push(span);
                }
            } else {
                keep.insert(*id, "no-stream".into());
            }
        }
    }
    if let Err(err) = blank_operators(doc, &spans) {
        for id in &succeeded {
            keep.insert(*id, err.clone());
        }
        spans.clear();
    }

    let draw_ids: HashSet<u32> = succeeded
        .iter()
        .copied()
        .filter(|id| !keep.contains_key(id))
        .collect();
    if !draw_ids.is_empty() {
        if let Some(font) = embedded {
            embed_and_draw(doc, extraction, &drawn, &font)?;
        }
    }

    for (id, reason) in non_text {
        if !extraction.glyphs[by_id[&id]].disposition.is_final() {
            extraction.mark_non_text(id, reason)?;
        }
    }
    for (id, reason) in keep {
        if !extraction.glyphs[by_id[&id]].disposition.is_final() {
            extraction.mark_kept(id, reason)?;
        }
    }
    for segment in &report.segments {
        if segment
            .glyph_ids
            .iter()
            .any(|id| draw_ids.contains(id) && !keep_contains(&extraction.glyphs[by_id[id]]))
        {
            for id in &segment.glyph_ids {
                if !extraction.glyphs[by_id[id]].disposition.is_final() {
                    extraction.mark_rewritten(*id, segment.translated.clone())?;
                }
            }
        }
    }
    let leftover: Vec<u32> = extraction
        .glyphs
        .iter()
        .filter(|glyph| !glyph.disposition.is_final())
        .map(|glyph| glyph.id)
        .collect();
    for id in leftover {
        extraction.mark_kept(id, "untranslated")?;
    }
    let _ = spans;
    Ok(())
}

fn keep_contains(glyph: &Glyph) -> bool {
    matches!(glyph.disposition, Disposition::KeptOriginal { .. })
}

fn glyph_index(glyphs: &[Glyph]) -> HashMap<u32, usize> {
    glyphs
        .iter()
        .enumerate()
        .map(|(index, glyph)| (glyph.id, index))
        .collect()
}

fn inherent_keep(glyph: &Glyph) -> Option<&'static str> {
    if glyph.unmapped || glyph.unicode.is_empty() {
        return Some("unmapped");
    }
    if is_math_font(&glyph.font_name) {
        return Some("formula");
    }
    if glyph.vertical {
        return Some("vertical");
    }
    if glyph.clipped && !glyph.clip_uncertain {
        return Some("clipped");
    }
    match glyph.source.kind {
        SourceKind::Type3CharProc => Some("type3"),
        SourceKind::AnnotationAppearance => Some("annotation"),
        SourceKind::PageContent | SourceKind::FormXObject => None,
    }
}

fn is_math_font(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "CMMI",
        "CMSY",
        "CMEX",
        "MSAM",
        "MSBM",
        "STIX",
        "CAMBRIAMATH",
        "LATINMODERNMATH",
        "SYMBOL",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
        || upper.contains("MATH")
}

fn operators_are_private(segment: &[&Glyph], all: &[Glyph]) -> bool {
    for glyph in segment {
        let Some(span) = span_of(&glyph.source) else {
            return false;
        };
        let foreign = all.iter().any(|other| {
            other.id != glyph.id
                && span_of(&other.source).is_some_and(|other_span| {
                    other_span.object_id == span.object_id
                        && other_span.start == span.start
                        && other_span.end == span.end
                })
                && !segment.iter().any(|inside| inside.id == other.id)
        });
        if foreign {
            return false;
        }
    }
    true
}

fn span_of(source: &GlyphSource) -> Option<Span> {
    let id = source.object_id.as_deref()?;
    let mut parts = id.split_whitespace();
    let number: u32 = parts.next()?.parse().ok()?;
    let generation: u16 = parts.next()?.parse().ok()?;
    if source.byte_end <= source.byte_start {
        return None;
    }
    Some(Span {
        object_id: (number, generation),
        start: source.byte_start,
        end: source.byte_end,
    })
}

fn blank_operators(doc: &mut Document, spans: &[Span]) -> std::result::Result<(), String> {
    let mut grouped: HashMap<ObjectId, Vec<(usize, usize)>> = HashMap::new();
    for span in spans {
        grouped
            .entry(span.object_id)
            .or_default()
            .push((span.start, span.end));
    }
    for (id, ranges) in grouped {
        let plain = {
            let object = doc
                .get_object(id)
                .map_err(|err| format!("stream {}: {err}", object_id_string(id)))?;
            let stream = object
                .as_stream()
                .map_err(|err| format!("stream {}: {err}", object_id_string(id)))?;
            stream
                .decompressed_content()
                .unwrap_or_else(|_| stream.content.clone())
        };
        let mut bytes = plain;
        for (start, end) in ranges {
            if end > bytes.len() || start >= end {
                return Err("operator range is outside the content stream".into());
            }
            for byte in &mut bytes[start..end] {
                *byte = b' ';
            }
        }
        let object = doc
            .get_object_mut(id)
            .map_err(|err| format!("stream {}: {err}", object_id_string(id)))?;
        let stream = object
            .as_stream_mut()
            .map_err(|err| format!("stream {}: {err}", object_id_string(id)))?;
        stream.set_plain_content(bytes);
    }
    Ok(())
}

fn layout_segment(
    glyphs: &[&Glyph],
    text: &str,
    font: &SubsetFont,
    extraction: &Extraction,
    bilingual: bool,
) -> Option<Vec<Drawn>> {
    if glyphs.is_empty() || text.trim().is_empty() {
        return None;
    }
    if text
        .chars()
        .any(|ch| !font.glyphs.contains_key(&(ch as u32)))
    {
        return None;
    }
    let page = glyphs[0].page_index;
    let media = extraction
        .pages
        .iter()
        .find(|info| info.index == page)
        .map(|info| info.media_box)
        .unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let x = glyphs
        .iter()
        .map(|glyph| glyph.matrix[4])
        .fold(f32::MAX, f32::min);
    let y = glyphs[0].matrix[5];
    let right = glyphs
        .iter()
        .map(|glyph| glyph.bbox[0].max(glyph.bbox[2]))
        .fold(x, f32::max);
    let line_width = (right - x).max(glyphs[0].font_size);
    let mut sizes: Vec<f32> = glyphs.iter().map(|glyph| glyph.font_size).collect();
    sizes.sort_by(|a, b| a.total_cmp(b));
    let mut size = sizes[sizes.len() / 2].max(1.0);
    let floor = size * 0.55;
    while measure(text, size, font) > line_width && size > floor {
        size *= 0.92;
    }
    let lines = if measure(text, size, font) <= line_width {
        vec![text.to_string()]
    } else {
        wrap_text(text, size, line_width, font)?
    };
    if lines.len() > 6 {
        return None;
    }
    let leading = size * 1.15;
    let mut origin_y = y;
    if bilingual {
        let above = y + size * 1.2;
        if above + size <= media[3] {
            origin_y = above;
        } else {
            origin_y = y - size * 1.2;
        }
    }
    let last_y = origin_y - (lines.len().saturating_sub(1) as f32) * leading;
    if last_y < media[1] || origin_y > media[3] {
        return None;
    }
    let color = glyphs[0].fill_color.clone();
    Some(
        lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| Drawn {
                page,
                x,
                y: origin_y - index as f32 * leading,
                size,
                color: color.clone(),
                cids: cids_of(&line, font),
            })
            .collect(),
    )
}

fn measure(text: &str, size: f32, font: &SubsetFont) -> f32 {
    text.chars()
        .map(|ch| {
            font.glyphs
                .get(&(ch as u32))
                .map(|(_, advance)| *advance as f32 * size / font.units_per_em as f32)
                .unwrap_or(size)
        })
        .sum()
}

fn cids_of(text: &str, font: &SubsetFont) -> Vec<u16> {
    text.chars()
        .filter_map(|ch| font.glyphs.get(&(ch as u32)).map(|(gid, _)| *gid))
        .collect()
}

fn wrap_text(text: &str, size: f32, max_width: f32, font: &SubsetFont) -> Option<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let mut lines = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let mut end = start;
        let mut width = 0.0f32;
        let mut last_space = None;
        while end < chars.len() {
            let advance = measure(&chars[end].to_string(), size, font);
            if end > start && width + advance > max_width {
                break;
            }
            width += advance;
            if chars[end] == ' ' {
                last_space = Some(end);
            }
            end += 1;
        }
        if end == start {
            end = (start + 1).min(chars.len());
        } else if end < chars.len() {
            if let Some(space) = last_space {
                if space > start {
                    end = space;
                }
            }
            while end > start + 1 && (!break_after(chars[end - 1]) || !break_before(chars[end])) {
                end -= 1;
            }
        }
        let line: String = chars[start..end].iter().collect();
        if line.trim().is_empty() && end == start {
            return None;
        }
        lines.push(line);
        start = end;
        if chars.get(start) == Some(&' ') {
            start += 1;
        }
        if lines.len() > 6 {
            return None;
        }
    }
    Some(lines).filter(|lines| !lines.is_empty())
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

fn embed_and_draw(
    doc: &mut Document,
    extraction: &Extraction,
    drawn: &[Drawn],
    font: &SubsetFont,
) -> Result<()> {
    if drawn.is_empty() {
        return Ok(());
    }
    let font_id = embed_font(doc, font);
    let mut by_page: HashMap<u32, Vec<&Drawn>> = HashMap::new();
    for line in drawn {
        by_page.entry(line.page).or_default().push(line);
    }
    for (page_index, lines) in by_page {
        let Some(page) = extraction
            .pages
            .iter()
            .find(|info| info.index == page_index)
        else {
            continue;
        };
        let Some(page_id) = parse_id(&page.object_id) else {
            continue;
        };
        let mut stream = String::from("BT\n");
        for line in lines {
            stream.push_str(&format!(
                "/RPTF {} Tf {} 1 0 0 1 {} {} Tm <{}> Tj\n",
                pdf_num(line.size),
                color_ops(&line.color),
                pdf_num(line.x),
                pdf_num(line.y),
                hex_cids(&line.cids)
            ));
        }
        stream.push_str("ET\n");
        let stream_id = doc.add_object(Stream::new(Dictionary::new(), stream.into_bytes()));
        attach_font(doc, page_id, font_id)?;
        append_contents(doc, page_id, stream_id)?;
    }
    Ok(())
}

fn embed_font(doc: &mut Document, font: &SubsetFont) -> ObjectId {
    let file_id = doc.add_object(Stream::new(Dictionary::new(), font.bytes.clone()));
    let mut descriptor = Dictionary::new();
    descriptor.set("Type", "FontDescriptor");
    descriptor.set("FontName", "RPTCJK");
    descriptor.set("Flags", 4);
    descriptor.set(
        "FontBBox",
        vec![0.into(), (-200).into(), 1000.into(), 800.into()],
    );
    descriptor.set("ItalicAngle", 0);
    descriptor.set("Ascent", 800);
    descriptor.set("Descent", -200);
    descriptor.set("CapHeight", 700);
    descriptor.set("StemV", 80);
    descriptor.set("FontFile2", file_id);
    let descriptor_id = doc.add_object(descriptor);

    let mut widths = Vec::new();
    let mut pairs: Vec<(u16, u16)> = font
        .glyphs
        .values()
        .map(|(gid, advance)| {
            let width = (*advance as u32 * 1000 / font.units_per_em.max(1) as u32) as i64;
            (*gid, width as u16)
        })
        .collect();
    pairs.sort_unstable();
    pairs.dedup();
    for (gid, width) in &pairs {
        widths.push(Object::Integer(*gid as i64));
        widths.push(Object::Array(vec![Object::Integer(*width as i64)]));
    }
    let mut system = Dictionary::new();
    system.set("Registry", Object::string_literal("Adobe"));
    system.set("Ordering", Object::string_literal("Identity"));
    system.set("Supplement", 0);
    let mut cid = Dictionary::new();
    cid.set("Type", "Font");
    cid.set("Subtype", "CIDFontType2");
    cid.set("BaseFont", "RPTCJK");
    cid.set("CIDSystemInfo", system);
    cid.set("FontDescriptor", descriptor_id);
    cid.set("DW", 1000);
    cid.set("W", widths);
    cid.set("CIDToGIDMap", "Identity");
    let cid_id = doc.add_object(cid);
    let tounicode = doc.add_object(Stream::new(
        Dictionary::new(),
        tounicode_cmap(font).into_bytes(),
    ));
    let mut type0 = Dictionary::new();
    type0.set("Type", "Font");
    type0.set("Subtype", "Type0");
    type0.set("BaseFont", "RPTCJK");
    type0.set("Encoding", "Identity-H");
    type0.set("DescendantFonts", vec![Object::Reference(cid_id)]);
    type0.set("ToUnicode", tounicode);
    doc.add_object(type0)
}

fn tounicode_cmap(font: &SubsetFont) -> String {
    let mut pairs: Vec<(u16, u32)> = font
        .glyphs
        .iter()
        .map(|(unicode, (gid, _))| (*gid, *unicode))
        .collect();
    pairs.sort_unstable();
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> def\n/CMapName /RPTCJK def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for chunk in pairs.chunks(100) {
        out.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (gid, unicode) in chunk {
            out.push_str(&format!("<{gid:04X}> <{unicode:04X}>\n"));
        }
        out.push_str("endbfchar\n");
    }
    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out
}

fn attach_font(doc: &mut Document, page_id: ObjectId, font_id: ObjectId) -> Result<()> {
    let mut resources = merged_resources(doc, page_id);
    let mut fonts = resources
        .get(b"Font")
        .ok()
        .and_then(|obj| dict_of(doc, obj))
        .cloned()
        .unwrap_or_default();
    fonts.set("RPTF", font_id);
    resources.set("Font", fonts);
    let page = doc
        .get_object_mut(page_id)
        .map_err(|err| Error::Pdf(err.to_string()))?;
    let page = page
        .as_dict_mut()
        .map_err(|err| Error::Pdf(err.to_string()))?;
    page.set("Resources", resources);
    Ok(())
}

fn merged_resources(doc: &Document, page_id: ObjectId) -> Dictionary {
    let mut chain = Vec::new();
    let mut current = Some(page_id);
    let mut seen = HashSet::new();
    while let Some(id) = current {
        if !seen.insert(id) || chain.len() > 32 {
            break;
        }
        chain.push(id);
        current = doc
            .get_dictionary(id)
            .ok()
            .and_then(|dict| dict.get(b"Parent").ok())
            .and_then(|obj| obj.as_reference().ok());
    }
    let mut merged = Dictionary::new();
    for id in chain.iter().rev() {
        let Ok(dict) = doc.get_dictionary(*id) else {
            continue;
        };
        let Ok(resources) = dict.get(b"Resources") else {
            continue;
        };
        let Some(src) = dict_of(doc, resources) else {
            continue;
        };
        merge_dict(doc, &mut merged, src);
    }
    merged
}

fn merge_dict(doc: &Document, dst: &mut Dictionary, src: &Dictionary) {
    for (key, value) in src.iter() {
        let nested = matches!(
            key.as_slice(),
            b"Font"
                | b"XObject"
                | b"ExtGState"
                | b"ColorSpace"
                | b"Pattern"
                | b"Properties"
                | b"Shading"
        );
        if nested {
            let mut owned = dst
                .get(key)
                .ok()
                .and_then(|obj| dict_of(doc, obj))
                .cloned()
                .unwrap_or_default();
            if let Some(incoming) = dict_of(doc, value) {
                for (sub_key, sub_value) in incoming.iter() {
                    owned.set(sub_key.clone(), sub_value.clone());
                }
            }
            dst.set(key.clone(), owned);
        } else if dst.get(key.as_slice()).is_err() {
            dst.set(key.clone(), value.clone());
        }
    }
}

fn append_contents(doc: &mut Document, page_id: ObjectId, stream_id: ObjectId) -> Result<()> {
    let contents = doc
        .get_dictionary(page_id)
        .ok()
        .and_then(|dict| dict.get(b"Contents").ok().cloned());
    let replacement = match contents {
        None => Object::Reference(stream_id),
        Some(Object::Reference(id)) => match doc.get_object(id) {
            Ok(Object::Array(array)) => {
                let mut array = array.clone();
                array.push(Object::Reference(stream_id));
                Object::Array(array)
            }
            _ => Object::Array(vec![Object::Reference(id), Object::Reference(stream_id)]),
        },
        Some(Object::Array(mut array)) => {
            array.push(Object::Reference(stream_id));
            Object::Array(array)
        }
        Some(other) => Object::Array(vec![other, Object::Reference(stream_id)]),
    };
    let page = doc
        .get_object_mut(page_id)
        .map_err(|err| Error::Pdf(err.to_string()))?;
    let page = page
        .as_dict_mut()
        .map_err(|err| Error::Pdf(err.to_string()))?;
    page.set("Contents", replacement);
    Ok(())
}

fn parse_id(text: &str) -> Option<ObjectId> {
    let mut parts = text.split_whitespace();
    let number = parts.next()?.parse().ok()?;
    let generation = parts.next()?.parse().ok()?;
    Some((number, generation))
}

fn color_ops(color: &Color) -> String {
    if color.space == "DeviceRGB" && color.components.len() >= 3 {
        format!(
            "{} {} {} rg",
            pdf_num(color.components[0]),
            pdf_num(color.components[1]),
            pdf_num(color.components[2])
        )
    } else if color.space == "DeviceGray" && !color.components.is_empty() {
        format!("{} g", pdf_num(color.components[0]))
    } else {
        "0 g".into()
    }
}

fn hex_cids(cids: &[u16]) -> String {
    cids.iter().map(|cid| format!("{cid:04X}")).collect()
}

fn pdf_num(value: f32) -> String {
    format!("{value:.3}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::PdfDocument;
    use crate::font::box_ttf;
    use crate::translate::{translate_extraction, TranslateOptions, Translator};
    use lopdf::dictionary;

    struct MapHello;
    impl Translator for MapHello {
        fn complete(&self, _system: &str, user: &str) -> Result<String> {
            let payload: serde_json::Value = serde_json::from_str(user).unwrap();
            let segs = payload["segments"].as_array().unwrap();
            let translations: Vec<serde_json::Value> = segs
                .iter()
                .map(|seg| {
                    let text = seg["text"].as_str().unwrap_or("");
                    let translated = if text.contains("Hello") { "AB" } else { text };
                    serde_json::json!({"id": seg["id"], "text": translated})
                })
                .collect();
            Ok(serde_json::json!({"translations": translations}).to_string())
        }
    }

    fn sample_pdf() -> Vec<u8> {
        let mut doc = Document::with_version("1.4");
        doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        let pages_id = doc.new_object_id();
        let body = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        });
        let math = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "CMSY10",
            "Encoding" => "WinAnsiEncoding",
        });
        let mut fonts = lopdf::Dictionary::new();
        fonts.set("F1", body);
        fonts.set("F2", math);
        let mut resources = lopdf::Dictionary::new();
        resources.set("Font", fonts);
        let content = b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Hello) Tj /F2 12 Tf 1 0 0 1 72 680 Tm (xy) Tj /F1 12 Tf 1 0 0 1 72 640 Tm (References) Tj /F1 12 Tf 1 0 0 1 72 620 Tm ([1] Smith, A. A paper 2020.) Tj ET".to_vec();
        let content_id = doc.add_object(Stream::new(dictionary! {}, content));
        let page = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content_id,
            "Resources" => resources,
        });
        doc.set_object(
            pages_id,
            dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page.into()],
                "Count" => 1,
            },
        );
        let catalog = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn rewrite_replaces_body_and_leaves_formula_and_references() {
        let bytes = sample_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let report =
            translate_extraction(&mut extraction, &TranslateOptions::default(), &MapHello).unwrap();
        let before = doc.plain_stream(extraction.glyphs[0].source.object_id.as_deref().unwrap());
        let font = box_ttf(&[b'A' as u32, b'B' as u32]);
        doc.rewrite(
            &mut extraction,
            &report,
            &RewriteOptions {
                bilingual: false,
                font_bytes: Some(font),
            },
        )
        .unwrap();
        assert!(
            extraction.assert_complete().is_ok(),
            "{:?}",
            extraction.coverage_report()
        );
        let hello = extraction
            .glyphs
            .iter()
            .find(|glyph| glyph.unicode == "H")
            .unwrap();
        assert!(
            matches!(hello.disposition, Disposition::Rewritten { .. }),
            "{:?}",
            hello.disposition
        );
        let refs = extraction
            .glyphs
            .iter()
            .find(|glyph| glyph.unicode == "R")
            .unwrap();
        assert!(
            matches!(&refs.disposition, Disposition::KeptOriginal { reason } if reason == "references"),
            "{:?}",
            refs.disposition
        );
        let after = doc.plain_stream(hello.source.object_id.as_deref().unwrap());
        let (before, after) = (before.unwrap(), after.unwrap());
        let start = refs.source.byte_start;
        let end = refs.source.byte_end;
        assert_eq!(&before[start..end], &after[start..end]);
        let saved = doc.save_bytes().unwrap();
        let again = PdfDocument::open_bytes(&saved).unwrap().extract();
        let text = again.plain_text();
        assert!(text.contains("AB"), "{text}");
        assert!(text.contains("References"), "{text}");
        assert!(!text.contains("Hello"), "{text}");
    }

    struct Echo;
    impl Translator for Echo {
        fn complete(&self, _system: &str, user: &str) -> Result<String> {
            let payload: serde_json::Value = serde_json::from_str(user).unwrap();
            let translations: Vec<serde_json::Value> = payload["segments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|seg| serde_json::json!({"id": seg["id"], "text": seg["text"]}))
                .collect();
            Ok(serde_json::json!({"translations": translations}).to_string())
        }
    }

    #[test]
    fn first_page_identity_rewrite_finishes_and_keeps_its_text() {
        let path = std::env::var_os("RPT_REWRITE_SAMPLE").map_or_else(
            || {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../corpus/ci/arxiv-2610.02163.pdf")
            },
            std::path::PathBuf::from,
        );
        if !path.exists() {
            return;
        }
        let Some(font) = crate::font::load_cjk_font() else {
            return;
        };
        let mut doc = PdfDocument::open(&path).unwrap();
        let mut extraction = doc.extract_with(&crate::extract::ExtractOptions {
            max_pages: Some(1),
            ..crate::extract::ExtractOptions::default()
        });
        let before = extraction.plain_text();
        let report =
            translate_extraction(&mut extraction, &TranslateOptions::default(), &Echo).unwrap();
        doc.rewrite(
            &mut extraction,
            &report,
            &RewriteOptions {
                bilingual: false,
                font_bytes: Some(font),
            },
        )
        .unwrap();
        extraction.assert_complete().unwrap();
        let rewritten = extraction
            .glyphs
            .iter()
            .filter(|glyph| {
                matches!(
                    glyph.disposition,
                    crate::glyph::Disposition::Rewritten { .. }
                )
            })
            .count();
        let mut reasons = std::collections::BTreeMap::<String, usize>::new();
        for glyph in &extraction.glyphs {
            if let crate::glyph::Disposition::KeptOriginal { reason } = &glyph.disposition {
                *reasons.entry(reason.clone()).or_default() += 1;
            }
        }
        assert!(rewritten > 20, "rewritten={rewritten} kept={reasons:?}");
        let saved = doc.save_bytes().unwrap();
        let out = std::env::temp_dir().join("rpt-identity-page1.pdf");
        std::fs::write(&out, &saved).unwrap();
        let again = PdfDocument::open_bytes(&saved).unwrap().extract_with(
            &crate::extract::ExtractOptions {
                max_pages: Some(1),
                ..crate::extract::ExtractOptions::default()
            },
        );
        let after = again.plain_text();
        if std::env::var_os("RPT_REWRITE_SAMPLE").is_none() {
            assert!(
                after.contains("ABSTRACT") || before.contains("ABSTRACT"),
                "missing heading"
            );
            assert!(
                after.contains("We introduce") || after.contains("Coding agents"),
                "word spaces were not rebuilt"
            );
        }
        assert!(after.chars().filter(|ch| !ch.is_whitespace()).count() > 100);
        let _ = (before, out);
    }
}
