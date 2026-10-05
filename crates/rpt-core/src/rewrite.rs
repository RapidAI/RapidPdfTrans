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
use crate::font::{face_style, subset_for_style, subset_ttf, FaceStyle, FontSources, SubsetFont};
use crate::glyph::{Disposition, Glyph, GlyphSource, SourceKind};
use crate::pdfutil::{dict_of, object_id_string};
use crate::translate::{BilingualLayout, OutputMode, TranslateReport};

#[derive(Clone, Debug, Default)]
pub struct RewriteOptions {
    /// `replace` is pure target-language text. Bilingual layouts keep English too.
    pub mode: OutputMode,
    /// Font bytes to subset. `None` searches `RPT_CJK_FONT` and common CJK fonts.
    pub font_bytes: Option<Vec<u8>>,
    /// Song/serif CJK file used for regular body text.
    pub cjk_serif: Option<std::path::PathBuf>,
    /// Sans CJK file used for regular sans text. Bold text stays Noto Sans CJK.
    pub cjk_sans: Option<std::path::PathBuf>,
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
    resource: String,
    skew: f32,
    /// Extra advance after each glyph, in unscaled text space (`Tc`).
    tracking: f32,
}

/// Rewrite `extraction` using `report` and remember the dispositions on the glyphs.
pub fn rewrite_translation(
    doc: &mut Document,
    extraction: &mut Extraction,
    report: &TranslateReport,
    opts: &RewriteOptions,
) -> Result<()> {
    let compose = match opts.mode {
        OutputMode::Bilingual(
            layout @ (BilingualLayout::SideBySide | BilingualLayout::Alternating),
        ) => Some(layout),
        _ => None,
    };
    let original = compose.map(|_| doc.clone());
    let overlay = matches!(opts.mode, OutputMode::Bilingual(BilingualLayout::Overlay));
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
    let owners = operator_owners(&extraction.glyphs);
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
        if !operators_are_private(&glyphs, &owners) {
            for id in &segment.glyph_ids {
                keep.insert(*id, "shared-operator".into());
            }
            continue;
        }
        if segment.translated == segment.source {
            for id in &segment.glyph_ids {
                keep.insert(*id, "unchanged".into());
            }
            continue;
        }
        planned.push((index, segment.translated.clone()));
        rewrite_ids.extend(segment.glyph_ids.iter().copied());
    }

    let mut drawn = Vec::new();
    let mut succeeded: HashSet<u32> = HashSet::new();
    let mut embedded: Vec<(String, SubsetFont)> = Vec::new();
    if let Some(font_bytes) = opts.font_bytes.clone() {
        let chars = planned
            .iter()
            .flat_map(|(_, text)| text.chars().map(|ch| ch as u32))
            .collect::<Vec<_>>();
        if let Some(font) = subset_ttf(&font_bytes, &chars) {
            place_segments(
                &planned,
                &font,
                "RPTF",
                0.0,
                report,
                extraction,
                &by_id,
                overlay,
                &mut succeeded,
                &mut drawn,
                &mut keep,
            );
            embedded.push(("RPTF".into(), font));
        } else {
            for id in &rewrite_ids {
                keep.insert(*id, "missing-glyph".into());
            }
        }
    } else {
        let mut groups: HashMap<FaceStyle, Vec<(usize, String)>> = HashMap::new();
        for (index, text) in &planned {
            let style = report.segments[*index]
                .glyph_ids
                .first()
                .and_then(|id| extraction.glyphs.get(by_id[id]))
                .map(|glyph| face_style(&glyph.font_name))
                .unwrap_or(FaceStyle {
                    serif: true,
                    bold: false,
                    italic: false,
                });
            groups
                .entry(style)
                .or_default()
                .push((*index, text.clone()));
        }
        let mut slot = 0u32;
        for (style, group) in groups {
            let chars = group
                .iter()
                .flat_map(|(_, text)| text.chars().map(|ch| ch as u32))
                .collect::<Vec<_>>();
            let Some(font) = subset_for_style(
                style,
                &chars,
                &FontSources {
                    serif: opts.cjk_serif.clone(),
                    sans: opts.cjk_sans.clone(),
                },
            ) else {
                for (index, _) in &group {
                    for id in &report.segments[*index].glyph_ids {
                        keep.insert(*id, "no-font".into());
                    }
                }
                continue;
            };
            let resource = format!("RPT{slot}");
            slot += 1;
            let skew = if style.italic { 0.25 } else { 0.0 };
            place_segments(
                &group,
                &font,
                &resource,
                skew,
                report,
                extraction,
                &by_id,
                overlay,
                &mut succeeded,
                &mut drawn,
                &mut keep,
            );
            embedded.push((resource, font));
        }
    }

    let mut spans = Vec::new();
    if !overlay {
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
    if !draw_ids.is_empty() && !embedded.is_empty() {
        embed_and_draw(doc, extraction, &drawn, &embedded)?;
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
    if let (Some(layout), Some(original)) = (compose, original) {
        compose_bilingual(doc, &original, layout)?;
    }
    Ok(())
}

/// Place each original page beside, or just before, its translated page.
fn compose_bilingual(
    translated: &mut Document,
    original: &Document,
    layout: BilingualLayout,
) -> Result<()> {
    let source_ids: Vec<ObjectId> = original.get_pages().into_values().collect();
    let target_ids: Vec<ObjectId> = translated.get_pages().into_values().collect();
    if source_ids.len() != target_ids.len() {
        return Err(Error::Pdf(
            "bilingual compose saw different page counts".into(),
        ));
    }
    let source_pages: Vec<PageSnap> = source_ids
        .iter()
        .map(|id| snap_page(original, *id))
        .collect();
    let target_pages: Vec<PageSnap> = target_ids
        .iter()
        .map(|id| snap_page(translated, *id))
        .collect();
    let mut imported = HashMap::new();
    let mut kids = Vec::new();
    let root = pages_root(translated)?;
    for (src, dst) in source_pages.into_iter().zip(target_pages) {
        let src_resources = remap_dict(translated, original, src.resources, &mut imported)?;
        let src_form = make_form(translated, src.content, src_resources, src.media);
        let dst_form = make_form(translated, dst.content, dst.resources, dst.media);
        match layout {
            BilingualLayout::SideBySide => {
                let width = dst.media[2] - dst.media[0];
                let media = [
                    dst.media[0],
                    dst.media[1],
                    dst.media[0] + width * 2.0,
                    dst.media[3],
                ];
                let mut xobjects = Dictionary::new();
                xobjects.set("RPTSrc", src_form);
                xobjects.set("RPTDst", dst_form);
                let content = format!(
                    "q /RPTSrc Do Q\nq 1 0 0 1 {} 0 cm /RPTDst Do Q\n",
                    pdf_num(width)
                );
                kids.push(make_page(
                    translated, root, media, dst.rotate, xobjects, content,
                ));
            }
            BilingualLayout::Alternating => {
                kids.push(single_page(
                    translated, root, src.media, src.rotate, src_form,
                ));
                kids.push(single_page(
                    translated, root, dst.media, dst.rotate, dst_form,
                ));
            }
            BilingualLayout::Overlay => {}
        }
    }
    let count = kids.len() as i64;
    let pages = translated
        .get_object_mut(root)
        .map_err(|err| Error::Pdf(err.to_string()))?
        .as_dict_mut()
        .map_err(|err| Error::Pdf(err.to_string()))?;
    pages.set("Kids", kids);
    pages.set("Count", count);
    Ok(())
}

struct PageSnap {
    content: Vec<u8>,
    resources: Dictionary,
    media: [f32; 4],
    rotate: i32,
}

fn snap_page(doc: &Document, page_id: ObjectId) -> PageSnap {
    PageSnap {
        content: doc.get_page_content(page_id),
        resources: merged_resources(doc, page_id),
        media: inherited_box(doc, page_id).unwrap_or([0.0, 0.0, 612.0, 792.0]),
        rotate: inherited_rotate(doc, page_id),
    }
}

fn single_page(
    doc: &mut Document,
    parent: ObjectId,
    media: [f32; 4],
    rotate: i32,
    form: ObjectId,
) -> Object {
    let mut xobjects = Dictionary::new();
    xobjects.set("RPTPage", form);
    make_page(
        doc,
        parent,
        media,
        rotate,
        xobjects,
        "q /RPTPage Do Q\n".into(),
    )
}

fn make_form(
    doc: &mut Document,
    content: Vec<u8>,
    resources: Dictionary,
    bbox: [f32; 4],
) -> ObjectId {
    let mut dict = Dictionary::new();
    dict.set("Type", "XObject");
    dict.set("Subtype", "Form");
    dict.set("FormType", 1);
    dict.set("BBox", rect_objects(bbox));
    dict.set("Resources", resources);
    doc.add_object(Stream::new(dict, content))
}

fn make_page(
    doc: &mut Document,
    parent: ObjectId,
    media: [f32; 4],
    rotate: i32,
    xobjects: Dictionary,
    content: String,
) -> Object {
    let mut resources = Dictionary::new();
    resources.set("XObject", xobjects);
    let stream_id = doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
    let mut page = Dictionary::new();
    page.set("Type", "Page");
    page.set("Parent", parent);
    page.set("MediaBox", rect_objects(media));
    if rotate != 0 {
        page.set("Rotate", i64::from(rotate));
    }
    page.set("Resources", resources);
    page.set("Contents", stream_id);
    Object::Reference(doc.add_object(page))
}

fn rect_objects(rect: [f32; 4]) -> Vec<Object> {
    rect.into_iter().map(Object::Real).collect()
}

fn pages_root(doc: &Document) -> Result<ObjectId> {
    let catalog_id = doc
        .trailer
        .get(b"Root")
        .map_err(|err| Error::Pdf(err.to_string()))?
        .as_reference()
        .map_err(|err| Error::Pdf(err.to_string()))?;
    doc.get_dictionary(catalog_id)
        .map_err(|err| Error::Pdf(err.to_string()))?
        .get(b"Pages")
        .map_err(|err| Error::Pdf(err.to_string()))?
        .as_reference()
        .map_err(|err| Error::Pdf(err.to_string()))
}

fn inherited_box(doc: &Document, page_id: ObjectId) -> Option<[f32; 4]> {
    let mut id = page_id;
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(id) {
            return None;
        }
        let dict = doc.get_dictionary(id).ok()?;
        if let Ok(value) = dict.get(b"MediaBox") {
            if let Some(rect) = rect_from_object(doc, value) {
                return Some(rect);
            }
        }
        id = dict
            .get(b"Parent")
            .ok()
            .and_then(|obj| obj.as_reference().ok())?;
    }
}

fn inherited_rotate(doc: &Document, page_id: ObjectId) -> i32 {
    let mut id = page_id;
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(id) {
            return 0;
        }
        let Ok(dict) = doc.get_dictionary(id) else {
            return 0;
        };
        if let Ok(value) = dict.get(b"Rotate") {
            if let Some(angle) = number_of(doc, value) {
                return angle as i32;
            }
        }
        let Some(parent) = dict
            .get(b"Parent")
            .ok()
            .and_then(|obj| obj.as_reference().ok())
        else {
            return 0;
        };
        id = parent;
    }
}

fn rect_from_object(doc: &Document, object: &Object) -> Option<[f32; 4]> {
    let array = match object {
        Object::Array(items) => items,
        Object::Reference(id) => {
            return doc
                .get_object(*id)
                .ok()
                .and_then(|obj| rect_from_object(doc, obj));
        }
        _ => return None,
    };
    if array.len() < 4 {
        return None;
    }
    Some([
        number_of(doc, &array[0])?,
        number_of(doc, &array[1])?,
        number_of(doc, &array[2])?,
        number_of(doc, &array[3])?,
    ])
}

fn number_of(doc: &Document, object: &Object) -> Option<f32> {
    match object {
        Object::Integer(value) => Some(*value as f32),
        Object::Real(value) => Some(*value),
        Object::Reference(id) => doc.get_object(*id).ok().and_then(|obj| number_of(doc, obj)),
        _ => None,
    }
}

fn remap_dict(
    dst: &mut Document,
    src: &Document,
    dict: Dictionary,
    map: &mut HashMap<ObjectId, ObjectId>,
) -> Result<Dictionary> {
    let mut out = Dictionary::new();
    for (key, value) in dict.into_iter() {
        out.set(key, remap_object(dst, src, value, map)?);
    }
    Ok(out)
}

fn remap_object(
    dst: &mut Document,
    src: &Document,
    object: Object,
    map: &mut HashMap<ObjectId, ObjectId>,
) -> Result<Object> {
    match object {
        Object::Reference(id) => Ok(Object::Reference(import_object(dst, src, id, map)?)),
        Object::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(remap_object(dst, src, item, map)?);
            }
            Ok(Object::Array(out))
        }
        Object::Dictionary(dict) => Ok(Object::Dictionary(remap_dict(dst, src, dict, map)?)),
        Object::Stream(stream) => {
            let dict = remap_dict(dst, src, stream.dict, map)?;
            Ok(Object::Stream(Stream {
                dict,
                content: stream.content,
                allows_compression: stream.allows_compression,
                start_position: None,
            }))
        }
        other => Ok(other),
    }
}

fn import_object(
    dst: &mut Document,
    src: &Document,
    id: ObjectId,
    map: &mut HashMap<ObjectId, ObjectId>,
) -> Result<ObjectId> {
    if let Some(existing) = map.get(&id) {
        return Ok(*existing);
    }
    let new_id = dst.new_object_id();
    map.insert(id, new_id);
    let object = src
        .get_object(id)
        .map_err(|err| Error::Pdf(err.to_string()))?
        .clone();
    let mapped = remap_object(dst, src, object, map)?;
    dst.set_object(new_id, mapped);
    Ok(new_id)
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
    if is_math_font(&glyph.font_name) && !is_footnote_symbol(&glyph.unicode) {
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

fn is_footnote_symbol(text: &str) -> bool {
    matches!(text.trim(), "*" | "∗" | "†" | "‡" | "§" | "¶" | "⋆" | "#")
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

fn operator_owners(glyphs: &[Glyph]) -> HashMap<(u32, u16, usize, usize), Vec<u32>> {
    let mut owners: HashMap<(u32, u16, usize, usize), Vec<u32>> = HashMap::new();
    for glyph in glyphs {
        let Some(span) = span_of(&glyph.source) else {
            continue;
        };
        owners
            .entry((span.object_id.0, span.object_id.1, span.start, span.end))
            .or_default()
            .push(glyph.id);
    }
    owners
}

fn operators_are_private(
    segment: &[&Glyph],
    owners: &HashMap<(u32, u16, usize, usize), Vec<u32>>,
) -> bool {
    let inside: HashSet<u32> = segment.iter().map(|glyph| glyph.id).collect();
    for glyph in segment {
        let Some(span) = span_of(&glyph.source) else {
            return false;
        };
        let key = (span.object_id.0, span.object_id.1, span.start, span.end);
        let Some(ids) = owners.get(&key) else {
            return false;
        };
        if ids.iter().any(|id| !inside.contains(id)) {
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

fn place_segments(
    planned: &[(usize, String)],
    font: &SubsetFont,
    resource: &str,
    skew: f32,
    report: &TranslateReport,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
    bilingual: bool,
    succeeded: &mut HashSet<u32>,
    drawn: &mut Vec<Drawn>,
    keep: &mut HashMap<u32, String>,
) {
    for (index, text) in planned {
        let segment = &report.segments[*index];
        let glyphs: Vec<&Glyph> = segment
            .glyph_ids
            .iter()
            .filter_map(|id| extraction.glyphs.get(by_id[id]))
            .collect();
        match layout_segment(&glyphs, text, font, extraction, bilingual, resource, skew) {
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
}

fn segment_crosses_column(glyphs: &[&Glyph], size: f32) -> bool {
    if glyphs.len() < 4 {
        return false;
    }
    let mut ordered: Vec<&Glyph> = glyphs.to_vec();
    ordered.sort_by(|a, b| a.matrix[4].total_cmp(&b.matrix[4]));
    let mut gaps = Vec::with_capacity(ordered.len() - 1);
    for pair in ordered.windows(2) {
        let right = pair[0].bbox[0].max(pair[0].bbox[2]);
        let left = pair[1].bbox[0].min(pair[1].bbox[2]);
        gaps.push(left - right);
    }
    let mut sorted = gaps.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let median = sorted[sorted.len() / 2].max(0.0);
    let hard = (size * 2.0).max(24.0);
    let trigger = (median * 3.5).max(size * 1.25).min(hard);
    gaps.iter().any(|gap| *gap > trigger)
}

struct InkLine<'a> {
    glyphs: Vec<&'a Glyph>,
    y: f32,
    left: f32,
    right: f32,
}

fn layout_segment(
    glyphs: &[&Glyph],
    text: &str,
    font: &SubsetFont,
    extraction: &Extraction,
    bilingual: bool,
    resource: &str,
    skew: f32,
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
    let ink = ink_lines(glyphs);
    if ink.is_empty() {
        return None;
    }
    let size0 = ink
        .iter()
        .map(|line| {
            line.glyphs
                .iter()
                .map(|glyph| glyph.font_size)
                .fold(1.0f32, f32::max)
        })
        .fold(1.0f32, f32::max);
    if ink
        .iter()
        .any(|line| segment_crosses_column(&line.glyphs, size0))
    {
        return None;
    }
    let mut sizes: Vec<f32> = ink
        .iter()
        .flat_map(|line| line.glyphs.iter().map(|glyph| glyph.font_size))
        .collect();
    sizes.sort_by(|a, b| a.total_cmp(b));
    let source_size = sizes[sizes.len() / 2].max(1.0);
    let top_y = ink[0].y;
    let bottom_y = ink[ink.len() - 1].y;
    let spans: Vec<(f32, f32)> = ink
        .iter()
        .map(|line| line_span(line, source_size))
        .collect();
    let mut block_left = spans.iter().map(|(left, _)| *left).fold(f32::MAX, f32::min);
    let mut block_right = spans
        .iter()
        .map(|(_, right)| *right)
        .fold(block_left, f32::max);
    let mut indent = 0.0f32;
    if spans.len() >= 2 {
        let body_left = spans[1..]
            .iter()
            .map(|(left, _)| *left)
            .fold(f32::MAX, f32::min);
        if spans[0].0 > body_left + source_size * 0.6 {
            indent = spans[0].0 - body_left;
            block_left = body_left;
        }
    }
    block_right = block_right.min(media[2] - 1.0);
    block_left = block_left.max(media[0]);
    let width = (block_right - block_left).max(source_size);
    let first_width = (width - indent).max(source_size * 0.5);
    let deltas: Vec<f32> = ink
        .windows(2)
        .map(|pair| pair[0].y - pair[1].y)
        .filter(|dy| *dy > 0.0)
        .collect();
    let source_leading = if deltas.is_empty() {
        source_size * 1.15
    } else {
        let mut ordered = deltas;
        ordered.sort_by(|a, b| a.total_cmp(b));
        ordered[ordered.len() / 2]
    };
    let floor = source_size * 0.55;
    let mut size = source_size;
    let fitted = loop {
        let leading = (source_leading * size / source_size).clamp(size * 1.0, size * 1.35);
        let lines = wrap_text(text, size, first_width, width, font)?;
        let available = (top_y - bottom_y).max(0.0);
        let need = (lines.len().saturating_sub(1) as f32) * leading;
        let within = lines.iter().enumerate().all(|(index, line)| {
            let limit = if index == 0 { first_width } else { width };
            measure(line, size, font) <= limit + 1.0
        });
        if within && need <= available + 0.8 {
            break Some((lines, leading));
        }
        if size <= floor + 0.01 {
            break None;
        }
        size = (size * 0.92).max(floor);
    };
    let (lines, leading) = fitted?;
    let mut origin_y = top_y;
    if bilingual {
        let above = top_y + size * 1.2;
        if above + size <= media[3] {
            origin_y = above;
        } else {
            origin_y = top_y - size * 1.2;
        }
    }
    let last_y = origin_y - (lines.len().saturating_sub(1) as f32) * leading;
    if last_y < media[1] - 0.5 || origin_y > media[3] {
        return None;
    }
    let color = ink[0].glyphs[0].fill_color.clone();
    let count = lines.len();
    Some(
        lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                let limit = if index == 0 { first_width } else { width };
                Drawn {
                    page,
                    x: block_left + if index == 0 { indent } else { 0.0 },
                    y: origin_y - index as f32 * leading,
                    size,
                    color: color.clone(),
                    cids: cids_of(&line, font),
                    resource: resource.to_string(),
                    skew,
                    tracking: line_tracking(&line, size, limit, font, index + 1 != count),
                }
            })
            .collect(),
    )
}

fn ink_lines<'a>(glyphs: &[&'a Glyph]) -> Vec<InkLine<'a>> {
    let mut ordered: Vec<&Glyph> = glyphs.to_vec();
    ordered.sort_by(|a, b| {
        b.matrix[5]
            .total_cmp(&a.matrix[5])
            .then(a.matrix[4].total_cmp(&b.matrix[4]))
    });
    let mut groups: Vec<Vec<&Glyph>> = Vec::new();
    for glyph in ordered {
        let size = glyph.font_size.max(1.0);
        let same = groups.last().is_some_and(|group| {
            let anchor = group[0];
            (anchor.matrix[5] - glyph.matrix[5]).abs() <= anchor.font_size.max(size) * 0.35
        });
        if same {
            groups.last_mut().unwrap().push(glyph);
        } else {
            groups.push(vec![glyph]);
        }
    }
    groups.retain(|group| {
        !group
            .iter()
            .all(|glyph| is_attached_mark(&glyph.unicode, glyph.font_size))
    });
    groups
        .into_iter()
        .map(|group| {
            let left = group
                .iter()
                .map(|glyph| glyph.matrix[4].min(glyph.bbox[0]).min(glyph.bbox[2]))
                .fold(f32::MAX, f32::min);
            let right = group
                .iter()
                .map(|glyph| glyph.bbox[0].max(glyph.bbox[2]).max(glyph.matrix[4]))
                .fold(left, f32::max);
            let mut ys: Vec<f32> = group.iter().map(|glyph| glyph.matrix[5]).collect();
            ys.sort_by(|a, b| a.total_cmp(b));
            let y = ys[ys.len() / 2];
            InkLine {
                glyphs: group,
                y,
                left,
                right,
            }
        })
        .collect()
}

fn line_span(line: &InkLine<'_>, size: f32) -> (f32, f32) {
    let visual = line.right - line.left;
    if visual >= size * 0.25 {
        return (line.left, line.right);
    }
    // Some Type 1 fonts omit widths, so every glyph sits on the same point.
    // Estimate from advances, then from a half-em per glyph, for this line only.
    let advance: f32 = line.glyphs.iter().map(|glyph| glyph.advance[0].abs()).sum();
    let width = if advance >= size * 0.25 {
        advance
    } else {
        (line.glyphs.len() as f32 * size * 0.5).max(size)
    };
    (line.left, line.left + width)
}

fn is_attached_mark(text: &str, size: f32) -> bool {
    if is_footnote_symbol(text) {
        return true;
    }
    let text = text.trim();
    size <= 8.0
        && !text.is_empty()
        && text.chars().count() <= 2
        && text.chars().all(|ch| ch.is_ascii_digit())
}

fn line_tracking(text: &str, size: f32, width: f32, font: &SubsetFont, justify: bool) -> f32 {
    if !justify {
        return 0.0;
    }
    let count = text.chars().count();
    if count < 2 || size <= 0.0 {
        return 0.0;
    }
    let slack = width - measure(text, size, font);
    if !(0.4..width * 0.22).contains(&slack) {
        return 0.0;
    }
    slack / ((count - 1) as f32 * size)
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

fn wrap_text(
    text: &str,
    size: f32,
    first_width: f32,
    max_width: f32,
    font: &SubsetFont,
) -> Option<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
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
        let mut last_space = None;
        while end < chars.len() {
            let advance = measure(&chars[end].to_string(), size, font);
            if end > start && width + advance > limit {
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
        if lines.len() > 48 {
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
    fonts: &[(String, SubsetFont)],
) -> Result<()> {
    if drawn.is_empty() {
        return Ok(());
    }
    let mut font_ids: HashMap<String, ObjectId> = HashMap::new();
    for (name, font) in fonts {
        font_ids.insert(name.clone(), embed_font(doc, font));
    }
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
        // Concatenated content streams keep the previous text state. A leftover
        // Tc/Tz from the page would move every new glyph off the line box.
        let mut stream = String::from("BT\n0 Tc 0 Tw 100 Tz 0 TL 0 Ts\n");
        for line in lines {
            stream.push_str(&format!(
                "{} Tc /{} {} Tf {} 1 0 {} 1 {} {} Tm <{}> Tj\n",
                pdf_num(line.tracking),
                line.resource,
                pdf_num(line.size),
                color_ops(&line.color),
                pdf_num(line.skew),
                pdf_num(line.x),
                pdf_num(line.y),
                hex_cids(&line.cids)
            ));
        }
        stream.push_str("ET\n");
        let stream_id = doc.add_object(Stream::new(Dictionary::new(), stream.into_bytes()));
        attach_fonts(doc, page_id, &font_ids)?;
        append_contents(doc, page_id, stream_id)?;
    }
    Ok(())
}

/// Bare CFF for CIDFontType0, otherwise the original glyf font bytes.
fn cff_or_glyf(bytes: &[u8]) -> (Vec<u8>, bool) {
    if bytes.len() >= 12 && &bytes[0..4] == b"OTTO" {
        if let Some(cff) = sfnt_table(bytes, b"CFF ") {
            if cff.len() >= 4 && cff[0] == 1 {
                return (cff, true);
            }
        }
    }
    if bytes.len() >= 4 && bytes[0] == 1 && bytes[1] == 0 {
        return (bytes.to_vec(), true);
    }
    (bytes.to_vec(), false)
}

fn sfnt_table(font: &[u8], tag: &[u8; 4]) -> Option<Vec<u8>> {
    if font.len() < 12 {
        return None;
    }
    let n = u16::from_be_bytes(font.get(4..6)?.try_into().ok()?) as usize;
    for index in 0..n {
        let rec = 12 + index * 16;
        let header = font.get(rec..rec + 16)?;
        if &header[0..4] != tag {
            continue;
        }
        let offset = u32::from_be_bytes(header[8..12].try_into().ok()?) as usize;
        let len = u32::from_be_bytes(header[12..16].try_into().ok()?) as usize;
        return Some(font.get(offset..offset + len)?.to_vec());
    }
    None
}

fn embed_font(doc: &mut Document, font: &SubsetFont) -> ObjectId {
    // A CIDFontType0 FontFile3 must be a bare CFF program. Noto's subset is an
    // OpenType (OTTO) wrapper around that CFF. Embedding the wrapper makes
    // Poppler report "Mismatch between font type and embedded font file" and
    // paint the Identity-H CID bytes through the Unicode cmap, so the page
    // shows Latin garbage while ToUnicode still extracts the translation.
    let (file_bytes, cff) = cff_or_glyf(&font.bytes);
    let file_id = if cff {
        let mut file = Dictionary::new();
        file.set("Subtype", "CIDFontType0C");
        doc.add_object(Stream::new(file, file_bytes))
    } else {
        doc.add_object(Stream::new(Dictionary::new(), file_bytes))
    };
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
    if cff {
        descriptor.set("FontFile3", file_id);
    } else {
        descriptor.set("FontFile2", file_id);
    }
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
    cid.set("Subtype", if cff { "CIDFontType0" } else { "CIDFontType2" });
    cid.set("BaseFont", "RPTCJK");
    cid.set("CIDSystemInfo", system);
    cid.set("FontDescriptor", descriptor_id);
    cid.set("DW", 1000);
    cid.set("W", widths);
    if !cff {
        cid.set("CIDToGIDMap", "Identity");
    }
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

fn attach_fonts(
    doc: &mut Document,
    page_id: ObjectId,
    font_ids: &HashMap<String, ObjectId>,
) -> Result<()> {
    let mut resources = merged_resources(doc, page_id);
    let mut fonts = resources
        .get(b"Font")
        .ok()
        .and_then(|obj| dict_of(doc, obj))
        .cloned()
        .unwrap_or_default();
    for (name, id) in font_ids {
        fonts.set(name.clone(), *id);
    }
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
    use crate::extract::Extraction;
    use crate::extract::PdfDocument;
    use crate::font::box_ttf;
    use crate::translate::{
        translate_extraction, BilingualLayout, OutputMode, TranslateOptions, Translator,
    };
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
                font_bytes: Some(font),
                ..RewriteOptions::default()
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

    #[test]
    fn a_changed_line_is_drawn_in_a_cjk_face() {
        if !std::path::Path::new("/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc")
            .is_file()
        {
            return;
        }
        let bytes = sample_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        struct ToChinese;
        impl Translator for ToChinese {
            fn complete(&self, _system: &str, user: &str) -> Result<String> {
                let payload: serde_json::Value = serde_json::from_str(user).unwrap();
                let translations: Vec<serde_json::Value> = payload["segments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|seg| {
                        let text = seg["text"].as_str().unwrap_or("");
                        let translated = if text.contains("Hello") {
                            "你好"
                        } else {
                            text
                        };
                        serde_json::json!({"id": seg["id"], "text": translated})
                    })
                    .collect();
                Ok(serde_json::json!({"translations": translations}).to_string())
            }
        }
        let report =
            translate_extraction(&mut extraction, &TranslateOptions::default(), &ToChinese)
                .unwrap();
        doc.rewrite(&mut extraction, &report, &RewriteOptions::default())
            .unwrap();
        let saved = doc.save_bytes().unwrap();
        let text = PdfDocument::open_bytes(&saved)
            .unwrap()
            .extract()
            .plain_text();
        let squashed: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert!(squashed.contains("你好"), "{text}");
        assert!(text.contains("References"), "{text}");
        assert!(
            text.lines().any(|line| line.contains("你好")),
            "CJK replacement should stay on one line: {text}"
        );
        let font_program = embedded_cff(&saved).expect("CIDFontType0C font program");
        assert_ne!(&font_program[0..4], b"OTTO", "FontFile3 must be bare CFF");
        assert_eq!(font_program[0], 1, "CFF major version");
        assert_painted_nihao(&saved);
    }

    fn embedded_cff(pdf: &[u8]) -> Option<Vec<u8>> {
        let doc = Document::load_mem(pdf).ok()?;
        for object in doc.objects.values() {
            let Ok(stream) = object.as_stream() else {
                continue;
            };
            let subtype = stream
                .dict
                .get(b"Subtype")
                .ok()
                .and_then(|obj| obj.as_name().ok());
            if subtype != Some(b"CIDFontType0C".as_slice()) {
                continue;
            }
            let bytes = stream
                .decompressed_content()
                .unwrap_or_else(|_| stream.content.clone());
            if !bytes.is_empty() {
                return Some(bytes);
            }
        }
        None
    }

    fn assert_painted_nihao(pdf: &[u8]) {
        let path = std::env::temp_dir().join("rpt-nihao-paint.pdf");
        std::fs::write(&path, pdf).unwrap();
        let text = std::process::Command::new("pdftotext")
            .args(["-enc", "UTF-8", "-q"])
            .arg(&path)
            .arg("-")
            .output()
            .expect("pdftotext");
        let extracted = String::from_utf8_lossy(&text.stdout);
        assert!(
            extracted.contains("你好"),
            "pdftotext missed the translation: {extracted}"
        );
        let fonts = std::process::Command::new("pdffonts")
            .arg(&path)
            .output()
            .expect("pdffonts");
        let listing = String::from_utf8_lossy(&fonts.stdout);
        let stderr = String::from_utf8_lossy(&fonts.stderr);
        assert!(
            listing.contains("RPTCJK") && listing.contains("CID Type 0C"),
            "{listing}"
        );
        assert!(
            !listing.contains("CID Type 0C (OT)"),
            "OpenType wrapper is not a CIDFontType0 program:\n{listing}"
        );
        assert!(!stderr.contains("Mismatch between font type"), "{stderr}");
        let dir = std::env::temp_dir().join("rpt-nihao-paint");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rendered = std::process::Command::new("pdftoppm")
            .args(["-png", "-r", "144", "-f", "1", "-l", "1"])
            .arg(&path)
            .arg(dir.join("page"))
            .status()
            .expect("pdftoppm");
        assert!(rendered.success(), "pdftoppm failed");
        let png = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .find(|path| path.extension().and_then(|ext| ext.to_str()) == Some("png"))
            .expect("pdftoppm wrote no png");
        let ocr = std::process::Command::new("tesseract")
            .args([
                png.to_str().unwrap(),
                "stdout",
                "-l",
                "chi_sim+eng",
                "--psm",
                "6",
            ])
            .output()
            .expect("tesseract");
        let seen = String::from_utf8_lossy(&ocr.stdout);
        assert!(
            seen.contains("你好"),
            "rendered page is not 你好 (ToUnicode is not used): {seen}"
        );
    }

    #[test]
    fn bilingual_layouts_keep_english_and_the_translation() {
        let bytes = sample_pdf();
        let font = box_ttf(&[b'A' as u32, b'B' as u32]);
        let side = rewrite_mode(
            &bytes,
            &font,
            OutputMode::Bilingual(BilingualLayout::SideBySide),
        );
        assert_eq!(side.pages.len(), 1);
        let width = side.pages[0].media_box[2] - side.pages[0].media_box[0];
        assert!((width - 1224.0).abs() < 1.0, "width={width}");
        let text = side.plain_text();
        assert!(text.contains("Hello"), "{text}");
        assert!(text.contains("AB"), "{text}");
        assert!(text.contains("References"), "{text}");

        let pages = rewrite_mode(
            &bytes,
            &font,
            OutputMode::Bilingual(BilingualLayout::Alternating),
        );
        assert_eq!(pages.pages.len(), 2, "alternating should add a page");
        let english = page_text(&pages, 0);
        let chinese = page_text(&pages, 1);
        assert!(english.contains("Hello"), "{english}");
        assert!(!english.contains("AB"), "{english}");
        assert!(chinese.contains("AB"), "{chinese}");
        assert!(!chinese.contains("Hello"), "{chinese}");
        assert!(english.contains("References") && chinese.contains("References"));

        let overlay = rewrite_mode(
            &bytes,
            &font,
            OutputMode::Bilingual(BilingualLayout::Overlay),
        );
        assert_eq!(overlay.pages.len(), 1);
        let overlay_width = overlay.pages[0].media_box[2] - overlay.pages[0].media_box[0];
        assert!((overlay_width - 612.0).abs() < 1.0, "width={overlay_width}");
        let overlay_text = overlay.plain_text();
        assert!(
            overlay_text.contains("Hello") && overlay_text.contains("AB"),
            "{overlay_text}"
        );
    }

    fn rewrite_mode(bytes: &[u8], font: &[u8], mode: OutputMode) -> Extraction {
        let mut doc = PdfDocument::open_bytes(bytes).unwrap();
        let mut extraction = doc.extract();
        let report =
            translate_extraction(&mut extraction, &TranslateOptions::default(), &MapHello).unwrap();
        doc.rewrite(
            &mut extraction,
            &report,
            &RewriteOptions {
                mode,
                font_bytes: Some(font.to_vec()),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        extraction.assert_complete().unwrap();
        let saved = doc.save_bytes().unwrap();
        PdfDocument::open_bytes(&saved).unwrap().extract()
    }

    fn page_text(extraction: &Extraction, page: u32) -> String {
        extraction
            .glyphs
            .iter()
            .filter(|glyph| glyph.page_index == page)
            .map(|glyph| glyph.unicode.as_str())
            .collect()
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
                font_bytes: Some(font),
                ..RewriteOptions::default()
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
        assert_eq!(
            rewritten, 0,
            "identity text should keep the source drawing, rewritten={rewritten} kept={reasons:?}"
        );
        assert!(
            reasons.get("unchanged").copied().unwrap_or(0) > 20,
            "kept={reasons:?}"
        );
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
            let squashed: String = after.chars().filter(|ch| !ch.is_whitespace()).collect();
            assert!(
                squashed.contains("2123") && squashed.contains("123"),
                "affiliation numbers were rewritten into the wrong digits"
            );
        }
        assert!(after.chars().filter(|ch| !ch.is_whitespace()).count() > 100);
        let _ = (before, out);
    }
}
