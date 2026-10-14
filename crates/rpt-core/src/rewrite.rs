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
//!
//! The input is a finished [`crate::translate::TranslateReport`]. This module
//! does not call a model. Bilingual page layouts are composed here from the
//! two PDFs.

use std::collections::{HashMap, HashSet};

use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

use crate::color::Color;
use crate::error::{Error, Result};
use crate::extract::Extraction;
use crate::font::{face_style, subset_for_style, subset_ttf, FaceStyle, FontSources, SubsetFont};
use crate::glyph::{Disposition, Glyph, GlyphSource, SourceKind};
use crate::layout::{
    char_widths, cids_of, cjk_indent_ems, fit_cjk_block, heading_level, justify_gaps, measure,
    pack_into_slots, pack_narrow_tail, scale_for_heading, spread_to_lines, wrap_text, CjkMeasure,
    HeadingLevel, LineSlots,
};
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
    /// Chinese body size as a fraction of the source size. `0` uses 0.90,
    /// or `RPT_CJK_SIZE_SCALE` when that is set. Titles stay a little larger.
    pub cjk_size_scale: f32,
    /// Baseline distance in ems of the Chinese size. `0` uses 1.60,
    /// or `RPT_CJK_LEADING` when that is set. This is wider than English leading.
    pub cjk_leading: f32,
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
    text: String,
    /// Source glyphs this draw replaces. Used to drop a draw when its operator
    /// cannot be blanked.
    glyph_ids: Vec<u32>,
    /// User-space advance of each character.
    widths: Vec<f32>,
    /// Extra user-space gap after each character except the last.
    /// Only CJK–CJK gaps are filled, so a Latin citation stays tight.
    gaps: Vec<f32>,
    /// True when this character keeps a source superscript (author marks).
    supers: Vec<bool>,
    /// Superscript size as a fraction of `size`, from the source glyphs.
    sup_scale: f32,
    /// Superscript baseline rise as a fraction of `size`, from the source glyphs.
    sup_rise: f32,
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
    // An echoed column that shares a TJ with a translation. The operator is
    // blanked for the translation; these glyphs are painted back in place.
    let mut paint_original: HashSet<u32> = HashSet::new();
    let owners = operator_owners(&extraction.glyphs);
    let mut toc_redraw: HashSet<u32> = HashSet::new();
    // Two credit columns often share one text operator. Each column is its
    // own segment. Blank the operator when every visible glyph in it belongs
    // to a segment that will be drawn.
    let translatable: HashSet<u32> = report
        .segments
        .iter()
        .filter(|segment| {
            let translated = crate::translate::restore_list_marker(
                &segment.source,
                &crate::translate::localize_part_heading(&segment.translated),
            );
            let translated = crate::translate::restore_credit_names(&segment.source, &translated);
            !segment.glyph_ids.is_empty()
                && !segment.source.trim().is_empty()
                && !translated.trim().is_empty()
                && translated != segment.source
        })
        .flat_map(|segment| segment.glyph_ids.iter().copied())
        .collect();
    // An identity pass echoes every segment. Leftover English is a QA failure
    // only when this job actually produced Chinese for some other segment.
    let job_translates = report
        .segments
        .iter()
        .any(|segment| segment.translated.chars().any(is_han_char));
    for (index, segment) in report.segments.iter().enumerate() {
        if segment.glyph_ids.is_empty() {
            continue;
        }
        let segment_glyphs: Vec<&Glyph> = segment
            .glyph_ids
            .iter()
            .filter_map(|id| extraction.glyphs.get(by_id[id]))
            .collect();
        // `c_{ij} > 0` inside a sentence is still that sentence. A display
        // equation has more math glyphs than words and stays original.
        let prose_math = prose_carries_inline_math(&segment_glyphs);
        if segment.glyph_ids.iter().any(|id| {
            extraction.glyphs.get(by_id[id]).is_some_and(|glyph| {
                if glyph.disposition.is_final() {
                    return true;
                }
                match keep.get(id).map(String::as_str) {
                    // `$9.7\times$` is a prose line. Redraw the operator with
                    // the body font instead of leaving the English in place.
                    Some("formula")
                        if prose_math
                            || crate::translate::is_inline_math_symbol(&glyph.unicode) =>
                    {
                        false
                    }
                    Some(_) => true,
                    None => false,
                }
            })
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
        // A dingbat between contents entries is redrawn with the row. Translating
        // the marker alone paints a stray word beside the bullet.
        let marker_only = glyphs
            .iter()
            .any(|glyph| !glyph.unicode.trim().is_empty() && is_subentry_bullet(glyph))
            && glyphs
                .iter()
                .all(|glyph| glyph.unicode.trim().is_empty() || is_subentry_bullet(glyph));
        if marker_only {
            continue;
        }
        let translated = crate::translate::restore_list_marker(
            &segment.source,
            &crate::translate::localize_part_heading(&segment.translated),
        );
        let translated = crate::translate::restore_list_number(&segment.source, &translated);
        let translated = crate::translate::restore_credit_names(&segment.source, &translated);
        let translated = crate::translate::repair_body_translation(&segment.source, &translated);
        let will_draw = translated != segment.source;
        if !operators_are_private(&glyphs, &owners) {
            // A contents title shares its text operator with the leader dots and
            // the page number. Blank the operator, draw the translation in the
            // title box, and put the dots and page number back where they were.
            if let Some(companions) =
                toc_companion_ids(&glyphs, &owners, extraction, &by_id, &translatable)
            {
                toc_redraw.extend(companions);
            } else if !co_translated_operator(&glyphs, &owners, &translatable, extraction, &by_id) {
                // One column echoed. Draw the translation and put the echo
                // back in its own box instead of leaving both columns English.
                if will_draw {
                    if let Some(ids) = plain_echo_siblings(
                        &glyphs,
                        &owners,
                        &translatable,
                        extraction,
                        &by_id,
                        &keep,
                    ) {
                        paint_original.extend(ids);
                    } else {
                        for id in &segment.glyph_ids {
                            keep.insert(*id, "shared-operator".into());
                        }
                        continue;
                    }
                } else if sibling_will_draw(&glyphs, &owners, &translatable, extraction, &by_id) {
                    paint_original.extend(segment.glyph_ids.iter().copied());
                } else {
                    for id in &segment.glyph_ids {
                        keep.insert(*id, "shared-operator".into());
                    }
                    continue;
                }
            } else if !will_draw {
                paint_original.extend(segment.glyph_ids.iter().copied());
            }
        }
        if translated == segment.source {
            let reason = if job_translates && english_body_source(&segment.source) {
                "english-body"
            } else {
                "unchanged"
            };
            for id in &segment.glyph_ids {
                keep.insert(*id, reason.into());
            }
            continue;
        }
        for id in &segment.glyph_ids {
            if keep.get(id).is_some_and(|reason| reason == "formula") {
                let inline = prose_math
                    || extraction.glyphs.get(by_id[id]).is_some_and(|glyph| {
                        crate::translate::is_inline_math_symbol(&glyph.unicode)
                    });
                if inline {
                    keep.remove(id);
                }
            }
        }
        planned.push((index, translated));
        rewrite_ids.extend(segment.glyph_ids.iter().copied());
    }

    let heading_wraps = if overlay {
        HashMap::new()
    } else {
        find_heading_wraps(&planned, report, extraction, &by_id)
    };
    let heading_chars: String = heading_wraps
        .values()
        .map(|wrap| wrap.text.as_str())
        .collect();
    let metrics = CjkMeasure::resolve(opts.cjk_size_scale, opts.cjk_leading);
    // Unchanged titles in a bullet row (`RAG`) are not planned, but the row
    // is redrawn as one flow and still needs those characters.
    let subentry_chars = subentry_font_chars(report, extraction, &by_id);
    let mut drawn = Vec::new();
    let mut succeeded: HashSet<u32> = HashSet::new();
    let mut embedded: Vec<(String, SubsetFont)> = Vec::new();
    let mut font_styles: HashMap<String, FaceStyle> = HashMap::new();
    // A leader row can be dots and a page number with no title glyph in
    // that operator. Those marks are not companions of the title, but the
    // subset still has to draw them once the page number blanks the line.
    let toc_chars: String = extraction
        .glyphs
        .iter()
        .filter(|glyph| {
            toc_redraw.contains(&glyph.id)
                || matches!(
                    &glyph.disposition,
                    Disposition::KeptOriginal { reason } if reason == "toc"
                )
        })
        .map(|glyph| glyph.unicode.as_str())
        .collect();
    let echo_chars: String = paint_original
        .iter()
        .filter_map(|id| extraction.glyphs.get(by_id[id]))
        .map(|glyph| glyph.unicode.as_str())
        .collect();
    // An unchanged continuation (`GoDaddy” (https://…)`) is redrawn so a
    // shorter paragraph above can pull it up. The subset has to contain
    // those Latin letters (`X` in the URL) or the line stays on the
    // English baseline and the hole reopens.
    let unchanged_body_chars: String = extraction
        .glyphs
        .iter()
        .filter(|glyph| {
            keep.get(&glyph.id)
                .is_some_and(|reason| reason == "unchanged")
        })
        .map(|glyph| glyph.unicode.as_str())
        .collect();
    let mut heading_shifts = Vec::new();
    if let Some(font_bytes) = opts.font_bytes.clone() {
        let chars = cover_with_fallbacks(
            planned
                .iter()
                .flat_map(|(index, text)| {
                    text.chars().chain(report.segments[*index].source.chars())
                })
                .chain(toc_chars.chars())
                .chain(echo_chars.chars())
                .chain(unchanged_body_chars.chars())
                .chain(subentry_chars.chars())
                .chain(heading_chars.chars()),
        );
        if let Some(font) = subset_ttf(&font_bytes, &chars) {
            place_segments(
                &planned,
                &font,
                "RPTF",
                0.0,
                metrics,
                report,
                extraction,
                &by_id,
                overlay,
                &mut succeeded,
                &mut drawn,
                &mut keep,
                &heading_wraps,
                &mut heading_shifts,
            );
            font_styles.insert(
                "RPTF".into(),
                FaceStyle {
                    serif: true,
                    bold: false,
                    italic: false,
                },
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
            let glyphs: Vec<&Glyph> = report.segments[*index]
                .glyph_ids
                .iter()
                .filter_map(|id| extraction.glyphs.get(by_id[id]))
                .collect();
            let mut style = paragraph_style(&glyphs);
            if heading_wants_bold(&glyphs, text) {
                style.bold = true;
            }
            groups
                .entry(style)
                .or_default()
                .push((*index, text.clone()));
        }
        let mut slot = 0u32;
        for (style, group) in groups {
            let chars = cover_with_fallbacks(
                group
                    .iter()
                    .flat_map(|(index, text)| {
                        text.chars().chain(report.segments[*index].source.chars())
                    })
                    .chain(toc_chars.chars())
                    .chain(echo_chars.chars())
                    .chain(unchanged_body_chars.chars())
                    .chain(subentry_chars.chars())
                    .chain(heading_chars.chars()),
            );
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
                metrics,
                report,
                extraction,
                &by_id,
                overlay,
                &mut succeeded,
                &mut drawn,
                &mut keep,
                &heading_wraps,
                &mut heading_shifts,
            );
            font_styles.insert(resource.clone(), style);
            embedded.push((resource, font));
        }
    }
    if !heading_wraps.is_empty() {
        let pending: Vec<(usize, String)> = heading_wraps
            .values()
            .filter(|wrap| {
                report.segments[wrap.continuation]
                    .glyph_ids
                    .iter()
                    .any(|id| !succeeded.contains(id))
            })
            .filter_map(|wrap| {
                planned
                    .iter()
                    .find(|(index, _)| *index == wrap.continuation)
                    .cloned()
            })
            .collect();
        if let Some((resource, font)) = embedded.first() {
            if !pending.is_empty() {
                place_segments(
                    &pending,
                    font,
                    resource,
                    0.0,
                    metrics,
                    report,
                    extraction,
                    &by_id,
                    overlay,
                    &mut succeeded,
                    &mut drawn,
                    &mut keep,
                    &HashMap::new(),
                    &mut heading_shifts,
                );
            }
        }
    }

    // A bullet-separated contents row is one flow (`title page ▪ title
    // page`), not a stack of English line breaks. Do this before the toc
    // pass so those page numbers are not also drawn at the old x.
    let mut subentry_shifts = if overlay {
        Vec::new()
    } else {
        let joined_continuations: HashSet<usize> = heading_wraps
            .values()
            .filter(|wrap| {
                report.segments[wrap.continuation]
                    .glyph_ids
                    .iter()
                    .all(|id| succeeded.contains(id))
            })
            .map(|wrap| wrap.continuation)
            .collect();
        reflow_inline_subentries(
            &mut drawn,
            &mut succeeded,
            &mut keep,
            &mut toc_redraw,
            report,
            extraction,
            &by_id,
            &embedded,
            &font_styles,
            metrics,
            &joined_continuations,
        )
    };
    // A joined chapter title that used fewer (or more) lines than the source
    // wrap moves the rows under that wrap by the same leading as a sub-entry.
    subentry_shifts.extend(heading_shifts);
    // A contents mark is redrawn by the toc pass. Painting it again stacks
    // a second `4` on the inline sub-entry.
    // The inline flow already redrew these page numbers. Painting the
    // original digits again leaves a second copy at the source x.
    paint_original.retain(|id| !toc_redraw.contains(id) && !succeeded.contains(id));
    let heading_page_ids = heading_wrap_page_ids(&heading_wraps, extraction, &succeeded);
    commit_toc_operators(
        &owners,
        &toc_redraw,
        &heading_page_ids,
        &paint_original,
        &mut succeeded,
        &mut keep,
        &mut drawn,
        extraction,
        &by_id,
        &embedded,
        &font_styles,
    );
    // After the shared-operator pass, so an in-place echo is not dropped
    // for lacking a succeeded id.
    drawn.extend(draw_original_glyphs(
        &paint_original,
        extraction,
        &by_id,
        &embedded,
    ));
    // `1.4.1` is one glyph per draw. Hole-closing then lifts the title and
    // leaves the digits on the English baseline, on top of the next paragraph.
    coalesce_section_indexes(&mut drawn);
    adopt_unchanged_body_lines(
        &mut drawn,
        &mut succeeded,
        &mut keep,
        extraction,
        &by_id,
        &embedded,
        &owners,
    );
    close_column_holes(&mut drawn, extraction, &by_id);
    apply_subentry_shifts(&mut drawn, &subentry_shifts);
    // A 27pt chapter index is kept in the source font (`(3) Tj`). The rows
    // around it move with a taller or shorter sub-entry, and the numeral has
    // to move by the same amount or it lands on the next section.
    shift_kept_chapter_indexes(
        &mut drawn,
        &mut succeeded,
        &mut keep,
        &subentry_shifts,
        extraction,
        &embedded,
        &font_styles,
    );
    // A contents page that a taller block pushed past the text band is
    // scaled as one stack. Rows stay in order and do not overlap.
    fit_contents_stack(&mut drawn, extraction);
    // A contents title can move up to close a hole. An inline page number
    // that was redrawn on the old baseline would otherwise sit alone.
    seat_inline_page_numbers(&mut drawn, extraction, &by_id);
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
        retarget_link_annotations(doc, extraction, &drawn, &draw_ids);
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
    let leftover_reason = if job_translates {
        "untranslated"
    } else {
        "kept"
    };
    for id in leftover {
        extraction.mark_kept(id, leftover_reason)?;
    }
    let _ = spans;
    if let (Some(layout), Some(original)) = (compose, original) {
        compose_bilingual(doc, &original, layout)?;
    }
    // Side-by-side imports the original page, including any OpenType font
    // stored under a CIDFontType0C subtype. Unwrap after compose so both
    // the translated file and the imported copy lose the mismatch.
    unwrap_opentype_cid_fonts(doc);
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

/// A sentence that contains a symbol (`edge ij with c > 0`) is body text.
/// A display equation has more math glyphs than words. A one-line equation
/// that also says "and define" or "for some f" is still that equation.
/// A subscript on that same baseline (`Y_k`, `B_{<k}`) is inline math inside
/// the sentence, so `Applying this with A = Y_k` still translates.
fn prose_carries_inline_math(glyphs: &[&Glyph]) -> bool {
    let mut letters = 0usize;
    let mut formula = 0usize;
    let mut other = 0usize;
    for glyph in glyphs {
        if inherent_keep(glyph) == Some("formula") {
            formula += 1;
        } else {
            other += 1;
            letters += glyph
                .unicode
                .chars()
                .filter(|ch| ch.is_ascii_alphabetic())
                .count();
        }
    }
    // `to (X, B_{≤k}), this becomes` is 13 letters. The 16-letter floor is
    // for a lead-in with no script (`for some f`). A roman identifier with
    // a subscript (`Lbbox`, `Lgiou`) is still the equation.
    let sentence = shares_subscript_baseline(glyphs) && has_prose_function_word(glyphs);
    let floor = if sentence { 12 } else { 16 };
    if letters < floor || other <= formula {
        return false;
    }
    if !sentence && formula >= 8 && letters < 40 && one_baseline(glyphs) {
        return false;
    }
    true
}

/// `with`, `the`, `to`: the line is a sentence, not a string of identifiers.
/// PDF text often has no space glyph, so a word gap ends the word too.
fn has_prose_function_word(glyphs: &[&Glyph]) -> bool {
    let mut ordered: Vec<&Glyph> = glyphs.to_vec();
    ordered.sort_by(|left, right| left.matrix[4].total_cmp(&right.matrix[4]));
    let mut word = String::new();
    let mut found = false;
    let mut previous: Option<&Glyph> = None;
    let flush = |word: &mut String, found: &mut bool| {
        if matches!(
            word.as_str(),
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
        ) {
            *found = true;
        }
        word.clear();
    };
    for glyph in ordered {
        if let Some(prev) = previous {
            let right = prev.bbox[0].max(prev.bbox[2]);
            let left = glyph.bbox[0].min(glyph.bbox[2]);
            if left - right > prev.font_size.max(1.0) * 0.18 {
                flush(&mut word, &mut found);
            }
        }
        previous = Some(glyph);
        if inherent_keep(glyph) == Some("formula") {
            flush(&mut word, &mut found);
            continue;
        }
        for ch in glyph.unicode.chars() {
            if ch.is_ascii_alphabetic() {
                word.push(ch.to_ascii_lowercase());
            } else {
                flush(&mut word, &mut found);
            }
        }
    }
    flush(&mut word, &mut found);
    found
}

/// A smaller glyph sitting just under the body baseline is a subscript.
fn shares_subscript_baseline(glyphs: &[&Glyph]) -> bool {
    let body = glyphs
        .iter()
        .map(|glyph| glyph.font_size)
        .fold(0.0f32, f32::max);
    if body < 1.0 {
        return false;
    }
    let mut body_ys: Vec<f32> = glyphs
        .iter()
        .filter(|glyph| glyph.font_size >= body * 0.85)
        .map(|glyph| glyph.matrix[5])
        .collect();
    if body_ys.is_empty() {
        return false;
    }
    body_ys.sort_by(|left, right| left.total_cmp(right));
    let body_y = body_ys[body_ys.len() / 2];
    glyphs.iter().any(|glyph| {
        let drop = body_y - glyph.matrix[5];
        glyph.font_size <= body * 0.80 && drop >= body * 0.08 && drop <= body * 0.45
    })
}

fn one_baseline(glyphs: &[&Glyph]) -> bool {
    let mut ys: Vec<f32> = glyphs.iter().map(|glyph| glyph.matrix[5]).collect();
    if ys.is_empty() {
        return false;
    }
    ys.sort_by(|left, right| left.total_cmp(right));
    let size = glyphs
        .iter()
        .map(|glyph| glyph.font_size)
        .fold(1.0f32, f32::max);
    ys[ys.len() - 1] - ys[0] <= size * 0.7
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
    if glyph.vertical || glyph.sideways() {
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

/// Glyphs that share an operator with `segment` and are contents marks
/// (leader dots or a page number) already kept as `toc`.
fn toc_companion_ids(
    segment: &[&Glyph],
    owners: &HashMap<(u32, u16, usize, usize), Vec<u32>>,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
    translatable: &HashSet<u32>,
) -> Option<Vec<u32>> {
    let inside: HashSet<u32> = segment.iter().map(|glyph| glyph.id).collect();
    let mut foreign = Vec::new();
    for glyph in segment {
        let span = span_of(&glyph.source)?;
        let key = (span.object_id.0, span.object_id.1, span.start, span.end);
        let ids = owners.get(&key)?;
        for id in ids {
            if !inside.contains(id) && !foreign.contains(id) {
                foreign.push(*id);
            }
        }
    }
    if foreign.is_empty() {
        return None;
    }
    let mut companions = Vec::new();
    for id in &foreign {
        let glyph = extraction.glyphs.get(by_id[id])?;
        let toc = matches!(
            &glyph.disposition,
            Disposition::KeptOriginal { reason } if reason == "toc"
        );
        if toc {
            companions.push(*id);
        } else if glyph.unicode.trim().is_empty() || translatable.contains(id) {
            // Another title in this TJ (`engines 4  Chatbots 7`) is drawn too.
            continue;
        } else {
            return None;
        }
    }
    if companions.is_empty() {
        None
    } else {
        Some(companions)
    }
}

/// The other ink in this operator is text from a segment that will be drawn.
/// A formula, a page number, or an untranslated column stays put.
fn co_translated_operator(
    segment: &[&Glyph],
    owners: &HashMap<(u32, u16, usize, usize), Vec<u32>>,
    translatable: &HashSet<u32>,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) -> bool {
    let inside: HashSet<u32> = segment.iter().map(|glyph| glyph.id).collect();
    let mut foreign = false;
    for glyph in segment {
        let Some(span) = span_of(&glyph.source) else {
            return false;
        };
        let key = (span.object_id.0, span.object_id.1, span.start, span.end);
        let Some(ids) = owners.get(&key) else {
            return false;
        };
        for id in ids {
            if inside.contains(id) {
                continue;
            }
            let Some(other) = extraction.glyphs.get(by_id[id]) else {
                return false;
            };
            if other.unicode.trim().is_empty() {
                continue;
            }
            if !translatable.contains(id) {
                return false;
            }
            foreign = true;
        }
    }
    foreign
}

/// Foreign ink that is plain text the model left unchanged. A formula or a
/// contents mark stays in the operator instead of being redrawn in the body face.
fn plain_echo_siblings(
    segment: &[&Glyph],
    owners: &HashMap<(u32, u16, usize, usize), Vec<u32>>,
    translatable: &HashSet<u32>,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
    keep: &HashMap<u32, String>,
) -> Option<Vec<u32>> {
    let inside: HashSet<u32> = segment.iter().map(|glyph| glyph.id).collect();
    let mut echoes = Vec::new();
    for glyph in segment {
        let span = span_of(&glyph.source)?;
        let key = (span.object_id.0, span.object_id.1, span.start, span.end);
        let ids = owners.get(&key)?;
        for id in ids {
            if inside.contains(id) || echoes.contains(id) {
                continue;
            }
            let other = extraction.glyphs.get(by_id[id])?;
            if other.unicode.trim().is_empty() || translatable.contains(id) {
                continue;
            }
            if glyph_stays_with_operator(other, keep) {
                return None;
            }
            echoes.push(*id);
        }
    }
    (!echoes.is_empty()).then_some(echoes)
}

fn sibling_will_draw(
    segment: &[&Glyph],
    owners: &HashMap<(u32, u16, usize, usize), Vec<u32>>,
    translatable: &HashSet<u32>,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) -> bool {
    let inside: HashSet<u32> = segment.iter().map(|glyph| glyph.id).collect();
    for glyph in segment {
        let Some(span) = span_of(&glyph.source) else {
            continue;
        };
        let key = (span.object_id.0, span.object_id.1, span.start, span.end);
        let Some(ids) = owners.get(&key) else {
            continue;
        };
        for id in ids {
            if inside.contains(id) {
                continue;
            }
            let Some(other) = extraction.glyphs.get(by_id[id]) else {
                continue;
            };
            if other.unicode.trim().is_empty() {
                continue;
            }
            if translatable.contains(id) {
                return true;
            }
        }
    }
    false
}

fn glyph_stays_with_operator(glyph: &Glyph, keep: &HashMap<u32, String>) -> bool {
    // Segmentation records `toc` on the glyph. The rewrite `keep` map does
    // not copy that reason, and a page number is not a plain echo.
    let reason = keep
        .get(&glyph.id)
        .map(String::as_str)
        .or(match &glyph.disposition {
            Disposition::KeptOriginal { reason } => Some(reason.as_str()),
            _ => None,
        });
    if reason.is_some_and(|reason| {
        matches!(
            reason,
            "formula" | "toc" | "margin" | "reference" | "figure" | "table"
        )
    }) {
        return true;
    }
    let upper = glyph.font_name.to_ascii_uppercase();
    ["CMMI", "CMSY", "CMEX", "MSAM", "MSBM"]
        .iter()
        .any(|needle| upper.contains(needle))
        || upper.contains("MATH")
}

fn draw_original_glyphs(
    ids: &HashSet<u32>,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
    embedded: &[(String, SubsetFont)],
) -> Vec<Drawn> {
    let mut drawn = Vec::new();
    let mut ordered: Vec<u32> = ids.iter().copied().collect();
    ordered.sort_unstable();
    for id in ordered {
        let Some(&index) = by_id.get(&id) else {
            continue;
        };
        let glyph = &extraction.glyphs[index];
        let text = glyph.unicode.clone();
        if text.trim().is_empty() {
            continue;
        }
        let Some((resource, font)) = embedded.iter().find(|(_, font)| {
            text.chars()
                .all(|ch| ch.is_whitespace() || font.glyphs.contains_key(&(ch as u32)))
        }) else {
            continue;
        };
        let size = glyph.font_size.max(1.0);
        let skew = if face_style(&glyph.font_name).italic {
            0.25
        } else {
            0.0
        };
        drawn.push(Drawn {
            page: glyph.page_index,
            x: glyph.matrix[4],
            y: glyph.matrix[5],
            size,
            color: glyph.fill_color.clone(),
            cids: cids_of(&text, font),
            resource: resource.clone(),
            skew,
            widths: char_widths(&text, size, font),
            gaps: vec![0.0; text.chars().count().saturating_sub(1)],
            text,
            supers: Vec::new(),
            sup_scale: 1.0,
            sup_rise: 0.0,
            glyph_ids: vec![glyph.id],
        });
    }
    drawn
}

/// Drop draws whose operator still contains ink we are not redrawing, then
/// paint kept contents marks back at their source positions.
fn commit_toc_operators(
    owners: &HashMap<(u32, u16, usize, usize), Vec<u32>>,
    toc_redraw: &HashSet<u32>,
    heading_page_ids: &HashSet<u32>,
    paint_original: &HashSet<u32>,
    succeeded: &mut HashSet<u32>,
    keep: &mut HashMap<u32, String>,
    drawn: &mut Vec<Drawn>,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
    embedded: &[(String, SubsetFont)],
    font_styles: &HashMap<String, FaceStyle>,
) {
    let mut blocked = HashSet::new();
    for ids in owners.values() {
        let live: Vec<u32> = ids
            .iter()
            .copied()
            .filter(|id| succeeded.contains(id) && !keep.contains_key(id))
            .collect();
        if live.is_empty() {
            continue;
        }
        let toc_ok = ids.iter().filter(|id| toc_redraw.contains(*id)).all(|id| {
            extraction.glyphs.get(by_id[id]).is_some_and(|glyph| {
                embedded
                    .iter()
                    .any(|(_, font)| glyph_chars_covered(glyph, font))
            })
        });
        // A space between the leader dots and the page number is not a segment.
        // Blanking it with the operator does not drop visible text.
        // An echoed column is painted after this pass. It is not leftover
        // ink, so it must not cancel the translated column.
        let ignorable = |id: &u32| {
            toc_redraw.contains(id)
                || paint_original.contains(id)
                || extraction
                    .glyphs
                    .get(by_id[id])
                    .is_some_and(|glyph| glyph.unicode.trim().is_empty())
        };
        let unsafe_ink = !toc_ok || ids.iter().any(|id| !live.contains(id) && !ignorable(id));
        if unsafe_ink {
            blocked.extend(live);
        }
    }
    for id in &blocked {
        succeeded.remove(id);
        keep.insert(*id, "shared-operator".into());
    }
    drawn.retain(|item| {
        item.glyph_ids
            .iter()
            .any(|id| succeeded.contains(id) && !keep.contains_key(id))
    });
    let mut live_ops = HashSet::new();
    for glyph in &extraction.glyphs {
        if succeeded.contains(&glyph.id) && !keep.contains_key(&glyph.id) {
            if let Some(span) = span_of(&glyph.source) {
                live_ops.insert((span.object_id.0, span.object_id.1, span.start, span.end));
            }
        }
    }
    let mut restored = Vec::new();
    // A sub-entry row already drew its page numbers upright beside the titles.
    // Those glyph ids are in `succeeded`. Drawing them again from the toc
    // disposition stacks a second italic number on the line.
    let runs = toc_page_number_runs(extraction, toc_redraw, succeeded);
    let mut placed = HashSet::new();
    for run in &runs {
        // The source puts the page number one gap after the title. Only a
        // part row, whose dots already end at the number, keeps that number
        // on the right edge. A shorter Chinese title must not slide `4` or
        // `xvi` out to that edge. The digit often shares the title's TJ, so
        // it is drawn again once that operator is blanked.
        // A heading wrap (`tool 283`) keeps its number on a separate
        // operator. `l` is also a roman digit, so the tight-gap test skips
        // the word and the number is never redrawn. Draw it anyway; seating
        // then puts it on the joined title.
        let forced =
            !run.is_empty() && run.iter().all(|glyph| heading_page_ids.contains(&glyph.id));
        let keep_put = forced
            || run_follows_leaders(run, extraction, toc_redraw)
            || shares_baseline_with_another_number(run, &runs)
            || title_follows_number(run, extraction)
            || tight_subentry_number(run, extraction);
        if keep_put {
            // `RAG 20` is one TJ. Redrawing the page number blanks that
            // operator, and an unchanged title is not drawn again.
            if operator_has_unredrawn_text(run, extraction, succeeded) {
                continue;
            }
            let source_right = run
                .iter()
                .map(|glyph| glyph_ink_right(glyph))
                .fold(0.0f32, f32::max);
            if let Some((resource, font, skew)) =
                font_for_page(run[0].page_index, run[0], embedded, drawn, font_styles)
            {
                if let Some(item) = draw_toc_run(run, source_right, font, resource, skew) {
                    placed.extend(run.iter().map(|glyph| glyph.id));
                    succeeded.extend(item.glyph_ids.iter().copied());
                    restored.push(item);
                }
            }
            continue;
        }
        if run.iter().any(|glyph| !toc_redraw.contains(&glyph.id)) {
            continue;
        }
        let glyph = run[0];
        let live = run.iter().all(|glyph| {
            span_of(&glyph.source).is_some_and(|span| {
                live_ops.contains(&(span.object_id.0, span.object_id.1, span.start, span.end))
            })
        });
        if !live {
            continue;
        }
        let Some((resource, font, skew)) =
            font_for_page(glyph.page_index, glyph, embedded, drawn, font_styles)
        else {
            continue;
        };
        let source_right = run
            .iter()
            .map(|glyph| glyph_ink_right(glyph))
            .fold(0.0f32, f32::max);
        if let Some(item) = draw_toc_run(run, source_right, font, resource, skew) {
            placed.extend(run.iter().map(|glyph| glyph.id));
            succeeded.extend(item.glyph_ids.iter().copied());
            restored.push(item);
        }
    }
    let mut leaders: Vec<&Glyph> = Vec::new();
    // `.......  119` is one operator and the title is another. The dots are
    // not companions of the title, so they never enter `toc_redraw`. Drawing
    // `119` still blanks that operator. Put the dots back with the number.
    let mut redraw_ids: Vec<u32> = toc_redraw.iter().copied().collect();
    let mut seen_redraw: HashSet<u32> = toc_redraw.clone();
    for glyph in &extraction.glyphs {
        if seen_redraw.contains(&glyph.id)
            || !is_toc_leader_glyph(glyph)
            || !is_toc_mark(glyph, toc_redraw)
        {
            continue;
        }
        let Some(span) = span_of(&glyph.source) else {
            continue;
        };
        let key = (span.object_id.0, span.object_id.1, span.start, span.end);
        let shares_redraw = owners.get(&key).is_some_and(|ids| {
            ids.iter()
                .any(|id| succeeded.contains(id) && !keep.contains_key(id))
        });
        if !shares_redraw {
            continue;
        }
        seen_redraw.insert(glyph.id);
        redraw_ids.push(glyph.id);
        live_ops.insert(key);
    }
    for id in &redraw_ids {
        if placed.contains(id) {
            continue;
        }
        let Some(glyph) = extraction.glyphs.get(by_id[id]) else {
            continue;
        };
        let Some(span) = span_of(&glyph.source) else {
            continue;
        };
        let key = (span.object_id.0, span.object_id.1, span.start, span.end);
        if !live_ops.contains(&key) {
            continue;
        }
        if is_toc_leader_glyph(glyph) {
            leaders.push(glyph);
            continue;
        }
        let Some((resource, font, skew)) =
            font_for_page(glyph.page_index, glyph, embedded, drawn, font_styles)
        else {
            continue;
        };
        if let Some(item) = draw_toc_glyph(glyph, font, resource, skew) {
            succeeded.extend(item.glyph_ids.iter().copied());
            restored.push(item);
        }
    }
    leaders.sort_by(|left, right| {
        left.page_index.cmp(&right.page_index).then(
            left.matrix[5]
                .total_cmp(&right.matrix[5])
                .then(left.matrix[4].total_cmp(&right.matrix[4])),
        )
    });
    let mut leader_groups: Vec<Vec<&Glyph>> = Vec::new();
    for glyph in leaders {
        let start_new = match leader_groups.last().and_then(|group| group.last().copied()) {
            Some(prev) => {
                let size = prev.font_size.max(glyph.font_size).max(1.0);
                glyph.page_index != prev.page_index
                    || (glyph.matrix[5] - prev.matrix[5]).abs() > size * 0.45
            }
            None => true,
        };
        if start_new {
            leader_groups.push(vec![glyph]);
        } else if let Some(group) = leader_groups.last_mut() {
            group.push(glyph);
        }
    }
    let mut replaced_baselines: Vec<(u32, f32, f32)> = Vec::new();
    for group in &leader_groups {
        let Some((resource, font, _)) =
            font_for_page(group[0].page_index, group[0], embedded, drawn, font_styles)
        else {
            continue;
        };
        if let Some(item) = place_leader_run(group, drawn, extraction, toc_redraw, font, resource) {
            replaced_baselines.push((item.page, item.y, item.size));
            succeeded.extend(item.glyph_ids.iter().copied());
            restored.push(item);
        } else {
            for glyph in group {
                if let Some(item) = draw_toc_glyph(glyph, font, resource, 0.0) {
                    succeeded.extend(item.glyph_ids.iter().copied());
                    restored.push(item);
                }
            }
        }
    }
    // A dot run can be several Tj chunks. The replacement paints one line.
    // Chunks that were not in that group still sit on the English baseline
    // after the row moves.
    for glyph in &extraction.glyphs {
        if !is_toc_leader_glyph(glyph) {
            continue;
        }
        let covered = replaced_baselines.iter().any(|(page, y, size)| {
            glyph.page_index == *page && (glyph.matrix[5] - *y).abs() <= (*size).max(1.0) * 0.45
        });
        if covered {
            keep.remove(&glyph.id);
            succeeded.insert(glyph.id);
        }
    }
    // These marks were `kept` as toc, then drawn again. Leave them in `keep`
    // and the source dots stay under the shifted row.
    for item in &restored {
        for id in &item.glyph_ids {
            keep.remove(id);
        }
    }
    drawn.extend(restored);
    strip_ink_inside_leaders(drawn);
}

/// A small-caps letter (`S` in `LLMS`) sits just before the leader dots.
/// Stretching the dots across the shorter Chinese title runs through that
/// x. The letter belongs to the title, which is already drawn, so a second
/// copy here is a mark in the middle of the dots.
fn strip_ink_inside_leaders(drawn: &mut Vec<Drawn>) {
    let spans: Vec<(u32, f32, f32, f32, f32)> = drawn
        .iter()
        .filter(|item| draw_is_leader(item))
        .map(|item| {
            (
                item.page,
                item.y,
                item.size.max(1.0),
                item.x,
                drawn_ink_right(item),
            )
        })
        .collect();
    if spans.is_empty() {
        return;
    }
    drawn.retain(|item| {
        if draw_is_leader(item) || bare_page_label(&item.text) {
            return true;
        }
        let letters = item.text.trim().chars().count();
        if letters == 0 || letters > 2 {
            return true;
        }
        let right = drawn_ink_right(item);
        let inside = spans.iter().any(|(page, y, size, left, lead_right)| {
            item.page == *page
                && (item.y - *y).abs() <= (*size).max(item.size.max(1.0)) * 0.7
                && item.x > *left + 0.8
                && right < *lead_right - 0.4
        });
        !inside
    });
}

fn bare_page_label(text: &str) -> bool {
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

/// `12` in `architecture 12  core object`. A title (or the bullet before it)
/// sits to the right, so the digit is not the row's column edge.
fn title_follows_number(run: &[&Glyph], extraction: &Extraction) -> bool {
    let Some(first) = run.first() else {
        return false;
    };
    let size = first.font_size.max(1.0);
    let right = run
        .iter()
        .map(|glyph| glyph_ink_right(glyph))
        .fold(0.0f32, f32::max);
    extraction.glyphs.iter().any(|glyph| {
        if glyph.page_index != first.page_index || run.iter().any(|item| item.id == glyph.id) {
            return false;
        }
        if (glyph.matrix[5] - first.matrix[5]).abs() > size * 0.45 {
            return false;
        }
        let left = glyph_ink_left(glyph);
        if left < right + size * 0.15 {
            return false;
        }
        let text = glyph.unicode.trim();
        if text.is_empty() || is_toc_leader_glyph(glyph) {
            return false;
        }
        // The next digit on this row is another page number, not a title.
        if toc_page_char(text) && left - right < size * 1.2 {
            return false;
        }
        true
    })
}

/// `4`, `7`, and `8` on one contents row. Moving them to one x stacks the digits.
fn shares_baseline_with_another_number(run: &[&Glyph], runs: &[Vec<&Glyph>]) -> bool {
    let Some(first) = run.first() else {
        return false;
    };
    let size = first.font_size.max(1.0);
    runs.iter().any(|other| {
        let Some(head) = other.first() else {
            return false;
        };
        head.id != first.id
            && head.page_index == first.page_index
            && (head.matrix[5] - first.matrix[5]).abs() <= size * 0.45
    })
}

/// Page-number glyphs already kept as contents marks, grouped into one run
/// per number (`27`, `103`). Leader dots are not part of a run.
fn toc_page_number_runs<'a>(
    extraction: &'a Extraction,
    toc_redraw: &HashSet<u32>,
    drawn_ids: &HashSet<u32>,
) -> Vec<Vec<&'a Glyph>> {
    let mut glyphs: Vec<&Glyph> = extraction
        .glyphs
        .iter()
        .filter(|glyph| {
            toc_page_char(&glyph.unicode)
                && is_toc_mark(glyph, toc_redraw)
                && !drawn_ids.contains(&glyph.id)
        })
        .collect();
    glyphs.sort_by(|left, right| {
        left.page_index.cmp(&right.page_index).then(
            left.matrix[5]
                .total_cmp(&right.matrix[5])
                .then(left.matrix[4].total_cmp(&right.matrix[4])),
        )
    });
    let mut runs: Vec<Vec<&Glyph>> = Vec::new();
    for glyph in glyphs {
        let start_new = match runs.last().and_then(|run| run.last().copied()) {
            Some(prev) => {
                let size = prev.font_size.max(glyph.font_size).max(1.0);
                let gap = glyph_ink_left(glyph) - glyph_ink_right(prev);
                // A tiny baseline difference sorts `8` before `4` even though
                // `4` sits further left. A negative gap is that jump backward,
                // not the next digit of the same number.
                glyph.page_index != prev.page_index
                    || (glyph.matrix[5] - prev.matrix[5]).abs() > size * 0.45
                    || gap > size * 0.45
                    || glyph_ink_left(glyph) + 0.4 < glyph_ink_left(prev)
            }
            None => true,
        };
        if start_new {
            runs.push(vec![glyph]);
        } else if let Some(run) = runs.last_mut() {
            run.push(glyph);
        }
    }
    runs
}

fn is_toc_mark(glyph: &Glyph, toc_redraw: &HashSet<u32>) -> bool {
    toc_redraw.contains(&glyph.id)
        || matches!(&glyph.disposition, Disposition::KeptOriginal { reason } if reason == "toc")
}

fn toc_page_char(text: &str) -> bool {
    let text = text.trim();
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) => {
            ch.is_ascii_digit()
                || matches!(
                    ch.to_ascii_lowercase(),
                    'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'
                )
        }
        _ => false,
    }
}

fn glyph_ink_left(glyph: &Glyph) -> f32 {
    glyph.matrix[4].min(glyph.bbox[0]).min(glyph.bbox[2])
}

fn glyph_ink_right(glyph: &Glyph) -> f32 {
    glyph.bbox[0].max(glyph.bbox[2]).max(glyph.matrix[4])
}

/// A contents page number that sits against leader dots is already at the
/// row's right edge. Moving it would leave a gap in the dots.
fn run_follows_leaders(run: &[&Glyph], extraction: &Extraction, toc_redraw: &HashSet<u32>) -> bool {
    let Some(first) = run.first() else {
        return false;
    };
    let size = first.font_size.max(1.0);
    let left = glyph_ink_left(first);
    extraction.glyphs.iter().any(|glyph| {
        if glyph.page_index != first.page_index || !is_toc_leader_glyph(glyph) {
            return false;
        }
        if !is_toc_mark(glyph, toc_redraw) {
            return false;
        }
        if (glyph.matrix[5] - first.matrix[5]).abs() > size * 0.45 {
            return false;
        }
        let right = glyph_ink_right(glyph);
        left - right < size * 0.8 && right <= left + 1.0
    })
}

fn is_toc_leader_glyph(glyph: &Glyph) -> bool {
    let text = glyph.unicode.trim();
    !text.is_empty()
        && text
            .chars()
            .all(|ch| matches!(ch, '.' | '·' | '…' | '•' | '⋅' | '‧' | '․'))
}

/// A title still lives in this page-number's TJ (`RAG` beside `20`).
/// Blanking the operator to redraw the digit would drop that title.
fn operator_has_unredrawn_text(
    run: &[&Glyph],
    extraction: &Extraction,
    succeeded: &HashSet<u32>,
) -> bool {
    let spans: Vec<Span> = run
        .iter()
        .filter_map(|glyph| span_of(&glyph.source))
        .collect();
    if spans.is_empty() {
        return false;
    }
    let run_ids: HashSet<u32> = run.iter().map(|glyph| glyph.id).collect();
    extraction.glyphs.iter().any(|glyph| {
        if run_ids.contains(&glyph.id)
            || glyph.unicode.trim().is_empty()
            || is_toc_leader_glyph(glyph)
        {
            return false;
        }
        if succeeded.contains(&glyph.id) {
            return false;
        }
        let Some(span) = span_of(&glyph.source) else {
            return false;
        };
        spans.iter().any(|other| {
            other.object_id == span.object_id && other.start == span.start && other.end == span.end
        })
    })
}

/// Draw one page number so its right edge meets `target_right`.
/// Contents rows pass the source right edge. A part row is already there,
/// against its leader dots. `seat_inline_page_numbers` then closes a gap
/// after a shorter title when the row has no leaders.
fn draw_toc_run(
    run: &[&Glyph],
    target_right: f32,
    font: &SubsetFont,
    resource: &str,
    skew: f32,
) -> Option<Drawn> {
    let text: String = run.iter().map(|glyph| glyph.unicode.trim()).collect();
    if text.is_empty() || run.iter().any(|glyph| !glyph_chars_covered(glyph, font)) {
        return None;
    }
    let size = run
        .iter()
        .map(|glyph| glyph.font_size)
        .fold(1.0f32, f32::max);
    let supers = vec![false; text.chars().count()];
    let widths = widths_with_superscripts(&text, size, font, &supers, None);
    let total: f32 = widths.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let source_right = run
        .iter()
        .map(|glyph| glyph_ink_right(glyph))
        .fold(0.0f32, f32::max);
    let x = source_right + (target_right - source_right).max(0.0) - total;
    Some(Drawn {
        page: run[0].page_index,
        x,
        y: run[0].matrix[5],
        size,
        color: run[0].fill_color.clone(),
        cids: cids_of(&text, font),
        resource: resource.to_string(),
        skew,
        widths,
        gaps: vec![0.0; text.chars().count().saturating_sub(1)],
        text,
        supers,
        sup_scale: 1.0,
        sup_rise: 0.0,
        glyph_ids: run.iter().map(|glyph| glyph.id).collect(),
    })
}

fn glyph_chars_covered(glyph: &Glyph, font: &SubsetFont) -> bool {
    let text = glyph.unicode.trim();
    !text.is_empty()
        && text
            .chars()
            .all(|ch| font.glyphs.contains_key(&(ch as u32)))
}

fn font_for_page<'a>(
    page: u32,
    glyph: &Glyph,
    embedded: &'a [(String, SubsetFont)],
    drawn: &[Drawn],
    styles: &HashMap<String, FaceStyle>,
) -> Option<(&'a str, &'a SubsetFont, f32)> {
    let want = face_style(&glyph.font_name);
    let mut best: Option<(&str, &SubsetFont, i32, f32)> = None;
    for (resource, font) in embedded {
        if !glyph_chars_covered(glyph, font) {
            continue;
        }
        let style = styles.get(resource).copied().unwrap_or(FaceStyle {
            serif: true,
            bold: false,
            italic: false,
        });
        let mut score = 0;
        if style.bold == want.bold {
            score += 4;
        }
        if style.italic == want.italic {
            score += 2;
        }
        if style.serif == want.serif {
            score += 1;
        }
        // Italic page numbers shear the regular face when the page has no
        // italic subset. A bold face is not sheared into an italic.
        let skew = if style.italic || (want.italic && !style.bold) {
            0.25
        } else {
            0.0
        };
        if best
            .as_ref()
            .is_none_or(|(_, _, best_score, _)| score > *best_score)
        {
            best = Some((resource.as_str(), font, score, skew));
        }
    }
    if best.is_some() {
        return best.map(|(resource, font, _, skew)| (resource, font, skew));
    }
    // A single test font may not record a style. Use the page's drawn face.
    let preferred = drawn
        .iter()
        .find(|item| item.page == page)
        .map(|item| item.resource.as_str());
    if let Some(name) = preferred {
        if let Some((resource, font)) = embedded.iter().find(|(resource, _)| resource == name) {
            if glyph_chars_covered(glyph, font) {
                let skew = if styles.get(resource).is_some_and(|style| style.italic) {
                    0.25
                } else {
                    0.0
                };
                return Some((resource.as_str(), font, skew));
            }
        }
    }
    embedded
        .iter()
        .find(|(_, font)| glyph_chars_covered(glyph, font))
        .map(|(resource, font)| (resource.as_str(), font, 0.0))
}

/// `model 15`: the digit sits against the word. It stays with that title
/// instead of being treated as the row's right-edge number.
fn tight_subentry_number(run: &[&Glyph], extraction: &Extraction) -> bool {
    let text: String = run.iter().map(|glyph| glyph.unicode.trim()).collect();
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let first = run[0];
    let size = first.font_size.max(1.0);
    let left = glyph_ink_left(first);
    extraction.glyphs.iter().any(|glyph| {
        if glyph.page_index != first.page_index || run.iter().any(|item| item.id == glyph.id) {
            return false;
        }
        if (glyph.matrix[5] - first.matrix[5]).abs() > size * 0.45 {
            return false;
        }
        let right = glyph_ink_right(glyph);
        if right > left + 0.5 {
            return false;
        }
        let gap = left - right;
        if !(0.0..=size * 0.8).contains(&gap) {
            return false;
        }
        let neighbor = glyph.unicode.trim();
        !neighbor.is_empty() && !is_toc_leader_glyph(glyph) && !toc_page_char(neighbor)
    })
}

/// A shorter Chinese title leaves a hole before leader dots that still start
/// where the English title ended. Stretch the dots from just after the title
/// to just before the page number.
fn place_leader_run(
    glyphs: &[&Glyph],
    drawn: &[Drawn],
    extraction: &Extraction,
    toc_redraw: &HashSet<u32>,
    font: &SubsetFont,
    resource: &str,
) -> Option<Drawn> {
    let first = *glyphs.first()?;
    let size = glyphs
        .iter()
        .map(|glyph| glyph.font_size)
        .fold(1.0f32, f32::max);
    let y = first.matrix[5];
    let leader_left = glyphs
        .iter()
        .map(|glyph| glyph_ink_left(glyph))
        .fold(f32::MAX, f32::min);
    let title_right = drawn
        .iter()
        .filter(|item| {
            item.page == first.page_index
                && (item.y - y).abs() <= size * 0.7
                && item.x < leader_left
        })
        .map(|item| item.x + item.widths.iter().sum::<f32>() + item.gaps.iter().sum::<f32>())
        .fold(None, |best: Option<f32>, right| {
            Some(best.map(|value| value.max(right)).unwrap_or(right))
        })?;
    let number_left = extraction
        .glyphs
        .iter()
        .filter(|glyph| {
            glyph.page_index == first.page_index
                && is_toc_mark(glyph, toc_redraw)
                && toc_page_char(&glyph.unicode)
                && (glyph.matrix[5] - y).abs() <= size * 0.45
                && glyph_ink_left(glyph) + 1.0 >= leader_left
        })
        .map(glyph_ink_left)
        .fold(None, |best: Option<f32>, left| {
            Some(best.map(|value| value.min(left)).unwrap_or(left))
        })?;
    let start = title_right + size * 0.75;
    let end = number_left - size * 0.35;
    // The title still ends where the dots began. Leave them.
    if start + size >= leader_left || end - start < size * 2.0 {
        return None;
    }
    let dot = glyphs.iter().find_map(|glyph| {
        glyph
            .unicode
            .chars()
            .find(|ch| matches!(*ch, '.' | '·' | '…' | '•' | '⋅' | '‧' | '․'))
    })?;
    if !font.glyphs.contains_key(&(dot as u32)) {
        return None;
    }
    let sample = dot.to_string();
    let one = widths_with_superscripts(&sample, size, font, &[false], None);
    let dot_width = one.first().copied().unwrap_or(0.0);
    if dot_width <= 0.2 {
        return None;
    }
    let span = end - start;
    let count = (span / dot_width).floor().max(1.0) as usize;
    if count < 4 {
        return None;
    }
    let text = sample.repeat(count);
    let widths = widths_with_superscripts(&text, size, font, &vec![false; count], None);
    let ink: f32 = widths.iter().sum();
    let extra = (span - ink).max(0.0);
    let gaps = if count > 1 {
        vec![extra / (count - 1) as f32; count - 1]
    } else {
        Vec::new()
    };
    Some(Drawn {
        page: first.page_index,
        x: start,
        y,
        size,
        color: first.fill_color.clone(),
        cids: cids_of(&text, font),
        resource: resource.to_string(),
        skew: 0.0,
        widths,
        gaps,
        text,
        supers: vec![false; count],
        sup_scale: 1.0,
        sup_rise: 0.0,
        glyph_ids: glyphs.iter().map(|glyph| glyph.id).collect(),
    })
}

fn draw_toc_glyph(glyph: &Glyph, font: &SubsetFont, resource: &str, skew: f32) -> Option<Drawn> {
    let text: String = glyph
        .unicode
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    if text.is_empty() || !glyph_chars_covered(glyph, font) {
        return None;
    }
    let supers = vec![false; text.chars().count()];
    let size = glyph.font_size.max(1.0);
    Some(Drawn {
        page: glyph.page_index,
        x: glyph.matrix[4],
        y: glyph.matrix[5],
        size,
        color: glyph.fill_color.clone(),
        cids: cids_of(&text, font),
        resource: resource.to_string(),
        skew,
        widths: widths_with_superscripts(&text, size, font, &supers, None),
        gaps: vec![0.0; text.chars().count().saturating_sub(1)],
        text,
        supers,
        sup_scale: 1.0,
        sup_rise: 0.0,
        glyph_ids: vec![glyph.id],
    })
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

fn glyph_span_width(glyphs: &[&Glyph]) -> f32 {
    let left = glyphs
        .iter()
        .map(|glyph| glyph.matrix[4].min(glyph.bbox[0]).min(glyph.bbox[2]))
        .fold(f32::MAX, f32::min);
    let right = glyphs
        .iter()
        .map(|glyph| glyph.bbox[0].max(glyph.bbox[2]).max(glyph.matrix[4]))
        .fold(left, f32::max);
    right - left
}

fn is_folio_segment(glyphs: &[&Glyph]) -> bool {
    if glyphs.is_empty() {
        return false;
    }
    let text: String = glyphs.iter().map(|glyph| glyph.unicode.as_str()).collect();
    let text = text.trim();
    let digits = !text.is_empty() && text.chars().all(|ch| ch.is_ascii_digit()) && text.len() <= 3;
    let roman = text.chars().count() >= 2
        && text.chars().count() <= 6
        && text.chars().all(|ch| {
            matches!(
                ch.to_ascii_lowercase(),
                'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'
            )
        });
    if !digits && !roman {
        return false;
    }
    let left = glyphs
        .iter()
        .map(|glyph| glyph.matrix[4].min(glyph.bbox[0]).min(glyph.bbox[2]))
        .fold(f32::MAX, f32::min);
    let right = glyphs
        .iter()
        .map(|glyph| glyph.bbox[0].max(glyph.bbox[2]).max(glyph.matrix[4]))
        .fold(left, f32::max);
    right - left <= 48.0
}

/// A header or footer page token (`ii`, `xiv`). A contents number such as
/// `31` sits in the text block and is not a folio.
fn is_margin_folio(glyphs: &[&Glyph], extraction: &Extraction) -> bool {
    if !is_folio_segment(glyphs) {
        return false;
    }
    let page = glyphs[0].page_index;
    let y = glyphs
        .iter()
        .map(|glyph| glyph.matrix[5])
        .fold(0.0f32, f32::max);
    let media = extraction
        .pages
        .iter()
        .find(|info| info.index == page)
        .map(|info| info.media_box)
        .unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let height = (media[3] - media[1]).max(1.0);
    let band = height * 0.12;
    media[3] - y < band || y - media[1] < band
}

struct HeadingLine {
    index: usize,
    page: u32,
    y: f32,
    left: f32,
    right: f32,
    size: f32,
    source: String,
}

fn heading_line(
    index: usize,
    report: &TranslateReport,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) -> Option<HeadingLine> {
    let segment = &report.segments[index];
    let mut glyphs = Vec::new();
    for id in &segment.glyph_ids {
        let glyph = extraction.glyphs.get(by_id[id])?;
        if glyph.unicode.trim().is_empty() {
            continue;
        }
        glyphs.push(glyph);
    }
    if glyphs.is_empty() {
        return None;
    }
    let size = glyphs
        .iter()
        .map(|glyph| glyph.font_size)
        .fold(1.0f32, f32::max)
        .max(1.0);
    let y = glyphs
        .iter()
        .map(|glyph| glyph.matrix[5])
        .fold(f32::MIN, f32::max);
    let low = glyphs
        .iter()
        .map(|glyph| glyph.matrix[5])
        .fold(f32::MAX, f32::min);
    // A segment that already spans two baselines was joined by the segmenter.
    if y - low > size * 0.45 {
        return None;
    }
    Some(HeadingLine {
        index,
        page: glyphs[0].page_index,
        y,
        left: glyphs
            .iter()
            .map(|glyph| glyph_ink_left(glyph))
            .fold(f32::MAX, f32::min),
        right: glyphs
            .iter()
            .map(|glyph| glyph_ink_right(glyph))
            .fold(0.0f32, f32::max),
        size,
        source: segment.source.clone(),
    })
}

fn source_ends_with_page_number(text: &str) -> bool {
    text.split_whitespace().last().is_some_and(|token| {
        !token.is_empty() && token.len() <= 3 && token.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn baseline_has_toc_page(extraction: &Extraction, page: u32, y: f32, size: f32) -> bool {
    let limit = size.max(1.0) * 0.55;
    extraction.glyphs.iter().any(|glyph| {
        glyph.page_index == page
            && (glyph.matrix[5] - y).abs() <= limit
            && is_inline_page_glyph(glyph)
    })
}

/// A contents heading whose page number sits on the next line. `beyond 327`
/// is indented about an em under the chapter title. `component 302`,
/// `agent 268`, and `tool 283` sit at the sub-entry x under a section
/// heading that has no number of its own. The segmenter keeps each as its
/// own line. Join the wrap back onto the heading so the number stays with it.
fn find_heading_wraps(
    planned: &[(usize, String)],
    report: &TranslateReport,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) -> HashMap<usize, HeadingWrap> {
    let lines: Vec<HeadingLine> = planned
        .iter()
        .filter_map(|(index, _)| heading_line(*index, report, extraction, by_id))
        .collect();
    let mut column_count: HashMap<(u32, i32), usize> = HashMap::new();
    for line in &lines {
        let key = (line.page, (line.left / 4.0).round() as i32);
        *column_count.entry(key).or_default() += 1;
    }
    let text_of: HashMap<usize, &str> = planned
        .iter()
        .map(|(index, text)| (*index, text.as_str()))
        .collect();
    let mut taken_hosts = HashSet::new();
    let mut wraps = HashMap::new();
    for cont in &lines {
        let source = cont.source.trim_start();
        let Some(first) = source.chars().next() else {
            continue;
        };
        if !first.is_ascii_lowercase() || source.split_whitespace().count() > 4 {
            continue;
        }
        if source.chars().any(|ch| matches!(ch, '■' | '•' | '▪')) {
            continue;
        }
        let column = column_count
            .get(&(cont.page, (cont.left / 4.0).round() as i32))
            .copied()
            .unwrap_or(0);
        // A sub-entry column shares its x across the page. A chapter wrap
        // like `beyond` does not. A section heading can still carry its page
        // number on that shared x.
        let shared_column = column >= 4;
        if !baseline_has_toc_page(extraction, cont.page, cont.y, cont.size)
            || line_has_bullet(cont.page, cont.y, extraction)
        {
            continue;
        }
        let Some(host) = lines
            .iter()
            .filter(|host| {
                if host.page != cont.page
                    || host.index == cont.index
                    || taken_hosts.contains(&host.index)
                {
                    return false;
                }
                if host.source.trim().chars().count() < 12
                    || source_ends_with_page_number(&host.source)
                {
                    return false;
                }
                if host.source.chars().any(|ch| matches!(ch, '■' | '•' | '▪')) {
                    return false;
                }
                if baseline_has_toc_page(extraction, host.page, host.y, host.size) {
                    return false;
                }
                let size = host.size.max(cont.size).max(1.0);
                let dy = host.y - cont.y;
                if dy < size * 0.65 || dy > size * 1.85 {
                    return false;
                }
                let indent = cont.left - host.left;
                // `beyond` is indented about an em from the chapter title.
                let modest = indent >= size * 0.30
                    && indent <= size * 1.85
                    && cont.left < host.right - size * 0.4;
                // `12.3 Handling…` / `component 302` is the heading's own
                // page number, even though it uses the sub-entry x.
                let section_wrap = section_heading_source(&host.source)
                    && cont.left > host.left + size * 0.30
                    && cont.left < host.right - size * 0.2;
                if shared_column {
                    section_wrap
                } else {
                    modest || section_wrap
                }
            })
            .min_by(|left, right| (left.y - cont.y).total_cmp(&(right.y - cont.y)))
        else {
            continue;
        };
        let Some(host_text) = text_of.get(&host.index) else {
            continue;
        };
        let Some(cont_text) = text_of.get(&cont.index) else {
            continue;
        };
        let text = join_wrapped_title(host_text.trim(), cont_text.trim());
        if text.trim().is_empty() {
            continue;
        }
        taken_hosts.insert(host.index);
        wraps.insert(
            host.index,
            HeadingWrap {
                continuation: cont.index,
                text,
                extra_ids: report.segments[cont.index].glyph_ids.clone(),
                host_y: host.y,
                cont_y: cont.y,
                page: host.page,
            },
        );
    }
    wraps
}

/// Page-number glyphs on a heading wrap that was actually joined (`tool 283`
/// under `11.7 … forecast`). They are redrawn even when the word they follow
/// is a roman letter, then seated on the joined title.
fn heading_wrap_page_ids(
    wraps: &HashMap<usize, HeadingWrap>,
    extraction: &Extraction,
    succeeded: &HashSet<u32>,
) -> HashSet<u32> {
    let mut ids = HashSet::new();
    for wrap in wraps.values() {
        let joined = wrap.extra_ids.iter().any(|id| succeeded.contains(id));
        if !joined {
            continue;
        }
        for glyph in &extraction.glyphs {
            if glyph.page_index != wrap.page || !is_inline_page_glyph(glyph) {
                continue;
            }
            if (glyph.matrix[5] - wrap.cont_y).abs() > 1.6 {
                continue;
            }
            ids.insert(glyph.id);
        }
    }
    ids
}

/// Fit the joined title on the host line when a slight shrink reaches, and
/// otherwise wrap every line at the host x. The continuation indent is not
/// a second column. `extra` is the signed leading followers must absorb:
/// negative closes an unused wrap line, positive pushes a taller title down.
fn layout_joined_contents_title(
    host: &[&Glyph],
    continuation_ids: &[u32],
    host_y: f32,
    cont_y: f32,
    text: &str,
    font: &SubsetFont,
    extraction: &Extraction,
    resource: &str,
    skew: f32,
    metrics: CjkMeasure,
) -> Option<(Vec<Drawn>, f32)> {
    let text = text.trim();
    if host.is_empty() || text.is_empty() {
        return None;
    }
    if text
        .chars()
        .any(|ch| !ch.is_whitespace() && !font.glyphs.contains_key(&(ch as u32)))
    {
        return None;
    }
    let title_x = host
        .iter()
        .map(|glyph| glyph_ink_left(glyph))
        .fold(f32::MAX, f32::min);
    if !title_x.is_finite() {
        return None;
    }
    let mut sizes: Vec<f32> = host.iter().map(|glyph| glyph.font_size).collect();
    sizes.sort_by(|left, right| left.total_cmp(right));
    let title_size = sizes[sizes.len() / 2].max(1.0);
    let bold = host.iter().all(|glyph| face_style(&glyph.font_name).bold);
    let level = heading_level(1, title_size, bold, text);
    let scale = scale_for_heading(level, title_size, metrics);
    let start = (title_size * scale).max(1.0);
    let floor = (start * 0.85).max(1.0);
    let page = host[0].page_index;
    let media = extraction
        .pages
        .iter()
        .find(|info| info.index == page)
        .map(|info| info.media_box)
        .unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let room = (media[2] - title_x - start * 3.0).max(start * 6.0);
    let mut size = start;
    let mut one_line = None;
    while size + 0.01 >= floor {
        if measure(text, size, font) <= room + 0.6 {
            one_line = Some(size);
            break;
        }
        let next = (size * 0.96).max(floor);
        if (next - size).abs() < 0.01 {
            break;
        }
        size = next;
    }
    let (lines, size) = if let Some(size) = one_line {
        (vec![text.to_string()], size)
    } else {
        let size = floor;
        let wrapped = wrap_text(text, size, room, room, font)?;
        if wrapped.is_empty() || wrapped.len() > 4 {
            return None;
        }
        (wrapped, size)
    };
    let source_step = (host_y - cont_y).abs();
    let step = source_step.clamp(size * 0.92, size * 1.45);
    let ys: Vec<f32> = (0..lines.len())
        .map(|index| host_y - index as f32 * step)
        .collect();
    let last = ys[ys.len() - 1];
    let extra = cont_y - last;
    let mut ids: Vec<u32> = host.iter().map(|glyph| glyph.id).collect();
    ids.extend(continuation_ids.iter().copied());
    ids.sort_unstable();
    ids.dedup();
    let color = host[0].fill_color.clone();
    let drawn = lines
        .into_iter()
        .zip(ys)
        .map(|(line, y)| {
            let count = line.chars().count();
            Drawn {
                page,
                x: title_x,
                y,
                size,
                color: color.clone(),
                cids: cids_of(&line, font),
                resource: resource.to_string(),
                skew,
                widths: char_widths(&line, size, font),
                gaps: vec![0.0; count.saturating_sub(1)],
                text: line,
                supers: vec![false; count],
                sup_scale: 1.0,
                sup_rise: 0.0,
                glyph_ids: ids.clone(),
            }
        })
        .collect();
    Some((drawn, extra))
}

fn place_segments(
    planned: &[(usize, String)],
    font: &SubsetFont,
    resource: &str,
    skew: f32,
    metrics: CjkMeasure,
    report: &TranslateReport,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
    bilingual: bool,
    succeeded: &mut HashSet<u32>,
    drawn: &mut Vec<Drawn>,
    keep: &mut HashMap<u32, String>,
    wraps: &HashMap<usize, HeadingWrap>,
    shifts: &mut Vec<SubentryShift>,
) {
    let skip_continuation: HashSet<usize> = wraps.values().map(|wrap| wrap.continuation).collect();
    for (index, text) in planned {
        if skip_continuation.contains(index) {
            continue;
        }
        let segment = &report.segments[*index];
        let glyphs: Vec<&Glyph> = segment
            .glyph_ids
            .iter()
            .filter_map(|id| extraction.glyphs.get(by_id[id]))
            .collect();
        if let Some(wrap) = wraps.get(index) {
            if let Some((lines, extra)) = layout_joined_contents_title(
                &glyphs,
                &wrap.extra_ids,
                wrap.host_y,
                wrap.cont_y,
                &wrap.text,
                font,
                extraction,
                resource,
                skew,
                metrics,
            ) {
                succeeded.extend(segment.glyph_ids.iter().copied());
                succeeded.extend(wrap.extra_ids.iter().copied());
                drawn.extend(lines);
                if extra.abs() > 0.4 {
                    let mut own_ids = HashSet::new();
                    own_ids.extend(segment.glyph_ids.iter().copied());
                    own_ids.extend(wrap.extra_ids.iter().copied());
                    shifts.push(SubentryShift {
                        page: wrap.page,
                        below_y: wrap.cont_y,
                        delta: extra,
                        own_ids,
                    });
                }
                continue;
            }
        }
        // `ii` translated to `二` fits the folio box and would replace the
        // printed page number. A folio stays the source token.
        if is_margin_folio(&glyphs, extraction) {
            for id in &segment.glyph_ids {
                keep.insert(*id, "folio".into());
            }
            continue;
        }
        match layout_segment(
            &glyphs, text, font, extraction, bilingual, resource, skew, metrics,
        ) {
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
                } else if is_folio_segment(&glyphs) {
                    // `xvi` translated to 十六 does not fit the folio box.
                    // Leave the printed page number.
                    "folio"
                } else if glyph_span_width(&glyphs) <= 44.0 {
                    // A contents crumb narrower than the translation. Leaving
                    // the English is not an unfinished body paragraph.
                    "tight"
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

fn segment_crosses_column(glyphs: &[&Glyph], neighbors: &[&Glyph], size: f32) -> bool {
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
    gaps.iter().enumerate().any(|(index, gap)| {
        if *gap <= trigger {
            return false;
        }
        // A contents bullet leaves ~14pt between a page number and the next
        // entry. That is not a second column.
        let left = &ordered[..=index];
        if *gap <= hard && crate::segment::piece_ends_with_page_number(left) {
            return false;
        }
        // An inline formula leaves a hole on this baseline where the
        // subscript sits. A column gutter is empty above and below too.
        let right_edge = ordered[index].bbox[0].max(ordered[index].bbox[2]);
        let left_edge = ordered[index + 1].bbox[0].min(ordered[index + 1].bbox[2]);
        let y = ordered[index].matrix[5];
        let filled = neighbors.iter().any(|glyph| {
            let x = (glyph.bbox[0] + glyph.bbox[2]) * 0.5;
            let dy = (glyph.matrix[5] - y).abs();
            x > right_edge + 0.5 && x < left_edge - 0.5 && dy > size * 0.12 && dy < size * 1.25
        });
        !filled
    })
}

struct InkLine<'a> {
    glyphs: Vec<&'a Glyph>,
    y: f32,
    left: f32,
    right: f32,
}

/// Extra codepoints to embed so a missing symbol can be drawn as a covered one.
fn cover_with_fallbacks(chars: impl Iterator<Item = char>) -> Vec<u32> {
    let mut codes = Vec::new();
    for ch in chars {
        codes.push(ch as u32);
        for alt in stand_ins(ch) {
            codes.push(alt as u32);
        }
    }
    codes
}

fn stand_ins(ch: char) -> Vec<char> {
    let mut out: Vec<char> = coverage_fallbacks(ch).to_vec();
    if let Some(base) = latin_base(ch) {
        out.push(base);
    }
    if let Some(plain) = math_plain(ch) {
        out.push(plain);
        if let Some(ascii) = greek_ascii(plain) {
            out.push(ascii);
        }
    }
    out
}

/// Symbols academic PDFs use that a CJK face often lacks. The original is kept
/// when none of these stand-ins are in the subset, so a mark is never dropped.
fn coverage_fallbacks(ch: char) -> &'static [char] {
    match ch {
        '\u{2217}' | '\u{204E}' | '\u{FE61}' | '\u{FF0A}' | '\u{22C6}' => &['*'],
        // Inner products and a combining slash. The Song face has neither.
        '\u{27E8}' => &['<'],
        '\u{27E9}' => &['>'],
        '\u{0338}' => &['/'],
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => &['-'],
        '\u{00D7}' | '\u{2715}' | '\u{2A2F}' => &['x'],
        // Floor and ceiling brackets. The CJK face has the ASCII brackets.
        '\u{2308}' | '\u{230A}' => &['['],
        '\u{2309}' | '\u{230B}' => &[']'],
        // `67.4 ∼ 118%`. The Song face has no tilde operator.
        '\u{223C}' | '\u{223D}' => &['~'],
        '\u{2018}' | '\u{2019}' | '\u{201A}' => &['\''],
        '\u{201C}' | '\u{201D}' | '\u{201E}' => &['"'],
        '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2009}' => &[' '],
        // FrameMaker dingbat bullets and the line-continuation arrow.
        '\u{27A2}' | '\u{27A4}' | '\u{27A5}' | '\u{2794}' | '\u{279C}' | '\u{279E}' => &['→', '>'],
        '\u{2705}' | '\u{2714}' => &['✓', '*'],
        '\u{274C}' | '\u{274E}' => &['×', 'x'],
        '\u{E000}'..='\u{F8FF}' => &['■', '•', '*'],
        _ if pictograph(ch) => &['•', '*'],
        _ => &[],
    }
}

/// Emoji and clock pictographs. A CJK body face has none of them, and one
/// missing icon used to leave the whole slide line in English.
fn pictograph(ch: char) -> bool {
    matches!(
        ch,
        '\u{2300}'..='\u{23FF}'
            | '\u{2600}'..='\u{27BF}'
            | '\u{1F000}'..='\u{1FAFF}'
    )
}

fn expand_ligature(ch: char) -> Option<&'static str> {
    match ch {
        '\u{FB00}' => Some("ff"),
        '\u{FB01}' => Some("fi"),
        '\u{FB02}' => Some("fl"),
        '\u{FB03}' => Some("ffi"),
        '\u{FB04}' => Some("ffl"),
        _ => None,
    }
}

/// Base letter for a precomposed Latin character the CJK face does not cover.
/// `Jastrzębski` can then be drawn, instead of leaving the paragraph in English.
/// Mathematical alphanumeric symbols, primes, and script digits become a
/// letter or digit the CJK face can draw. One missing `𝜆` used to leave the
/// whole paragraph in English.
fn math_plain(ch: char) -> Option<char> {
    if let Some(digit) = script_digit(ch) {
        return Some(digit);
    }
    if let Some(letter) = subscript_letter(ch) {
        return Some(letter);
    }
    let code = ch as u32;
    const LATIN: &[(u32, u32)] = &[
        (0x1D400, 0x1D41A),
        (0x1D434, 0x1D44E),
        (0x1D468, 0x1D482),
        (0x1D5A0, 0x1D5BA),
        (0x1D5D4, 0x1D5EE),
        (0x1D608, 0x1D622),
        (0x1D63C, 0x1D656),
        (0x1D670, 0x1D68A),
    ];
    for &(upper, lower) in LATIN {
        if (upper..upper + 26).contains(&code) {
            return char::from_u32(u32::from('A') + (code - upper));
        }
        if (lower..lower + 26).contains(&code) {
            return char::from_u32(u32::from('a') + (code - lower));
        }
    }
    const GREEK_SMALL: &[u32] = &[0x1D6C2, 0x1D6FC, 0x1D736, 0x1D770, 0x1D7AA];
    // After the 25 letters: partial, then epsilon/theta/kappa/phi/rho/pi symbols.
    // `𝜙` (U+1D719) used to leave the whole sentence in English.
    const GREEK_SYMBOL: &[char] = &['d', 'ε', 'θ', 'κ', 'φ', 'ρ', 'π'];
    for &start in GREEK_SMALL {
        if (start..start + 25).contains(&code) {
            return char::from_u32(0x03B1 + (code - start));
        }
        if (start + 25..start + 32).contains(&code) {
            return Some(GREEK_SYMBOL[(code - (start + 25)) as usize]);
        }
    }
    const GREEK_CAP: &[u32] = &[0x1D6A8, 0x1D6E2, 0x1D71C, 0x1D756, 0x1D790];
    for &start in GREEK_CAP {
        if (start..start + 17).contains(&code) {
            return char::from_u32(0x0391 + (code - start));
        }
        if code == start + 17 {
            return Some('Θ');
        }
        if (start + 18..start + 25).contains(&code) {
            return char::from_u32(0x03A3 + (code - (start + 18)));
        }
    }
    match ch {
        '\u{2032}' | '\u{2035}' => Some('\''),
        '\u{2033}' => Some('"'),
        // `𝑑ℎ` uses the Planck constant, not mathematical italic h.
        '\u{210E}' | '\u{210F}' => Some('h'),
        '\u{2113}' => Some('l'),
        '\u{212F}' => Some('e'),
        '\u{2134}' => Some('o'),
        _ => None,
    }
}

fn greek_ascii(ch: char) -> Option<char> {
    Some(match ch {
        'α' | 'Α' => 'a',
        'β' | 'Β' => 'b',
        'γ' | 'Γ' => 'g',
        'δ' | 'Δ' => 'd',
        'ε' | 'Ε' => 'e',
        'ζ' | 'Ζ' => 'z',
        'η' | 'Η' => 'h',
        'θ' | 'Θ' => 't',
        'ι' | 'Ι' => 'i',
        'κ' | 'Κ' => 'k',
        'λ' | 'Λ' => 'l',
        'μ' | 'Μ' => 'm',
        'ν' | 'Ν' => 'n',
        'ξ' | 'Ξ' => 'x',
        'ο' | 'Ο' => 'o',
        'π' | 'Π' => 'p',
        'ρ' | 'Ρ' => 'r',
        'σ' | 'ς' | 'Σ' => 's',
        'τ' | 'Τ' => 't',
        'υ' | 'Υ' => 'u',
        'φ' | 'Φ' => 'f',
        'χ' | 'Χ' => 'c',
        'ψ' | 'Ψ' => 'y',
        'ω' | 'Ω' => 'w',
        _ => return None,
    })
}

fn script_digit(ch: char) -> Option<char> {
    let code = ch as u32;
    match ch {
        '\u{2080}'..='\u{2089}' => char::from_u32(u32::from('0') + (code - 0x2080)),
        '\u{2070}' => Some('0'),
        '\u{00B9}' => Some('1'),
        '\u{00B2}' => Some('2'),
        '\u{00B3}' => Some('3'),
        '\u{2074}'..='\u{2079}' => char::from_u32(u32::from('4') + (code - 0x2074)),
        _ => None,
    }
}

fn subscript_letter(ch: char) -> Option<char> {
    Some(match ch {
        '\u{1D62}' => 'i',
        '\u{1D63}' => 'r',
        '\u{1D64}' => 'u',
        '\u{1D65}' => 'v',
        '\u{2090}' => 'a',
        '\u{2091}' => 'e',
        '\u{2092}' => 'o',
        '\u{2093}' => 'x',
        '\u{2095}' => 'h',
        '\u{2096}' => 'k',
        '\u{2097}' => 'l',
        '\u{2098}' => 'm',
        '\u{2099}' => 'n',
        '\u{209A}' => 'p',
        '\u{209B}' => 's',
        '\u{209C}' => 't',
        _ => return None,
    })
}

fn latin_base(ch: char) -> Option<char> {
    Some(match ch {
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ā' | 'Ă' | 'Ą' => 'A',
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'Ç' | 'Ć' | 'Ĉ' | 'Ċ' | 'Č' => 'C',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'È' | 'É' | 'Ê' | 'Ë' | 'Ē' | 'Ĕ' | 'Ė' | 'Ę' | 'Ě' => 'E',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'Ì' | 'Í' | 'Î' | 'Ï' | 'Ī' | 'Ĭ' | 'Į' => 'I',
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ĭ' | 'į' => 'i',
        'Ñ' | 'Ń' | 'Ņ' | 'Ň' => 'N',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'Ř' => 'R',
        'ř' => 'r',
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ō' | 'Ŏ' | 'Ő' => 'O',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ō' | 'ŏ' | 'ő' => 'o',
        'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ū' | 'Ŭ' | 'Ů' | 'Ű' | 'Ų' => 'U',
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'Ý' | 'Ÿ' | 'Ŷ' => 'Y',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'Ś' | 'Ŝ' | 'Ş' | 'Š' => 'S',
        'ś' | 'ŝ' | 'ş' | 'š' => 's',
        'Ź' | 'Ż' | 'Ž' => 'Z',
        'ź' | 'ż' | 'ž' => 'z',
        'Ł' => 'L',
        'ł' => 'l',
        'Đ' => 'D',
        'đ' => 'd',
        'Ø' => 'O',
        'ø' => 'o',
        _ => return None,
    })
}

fn substitute_covered(text: &str, font: &SubsetFont) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if font.glyphs.contains_key(&(ch as u32)) {
            out.push(ch);
            continue;
        }
        if let Some(ligature) = expand_ligature(ch) {
            if ligature
                .chars()
                .all(|part| font.glyphs.contains_key(&(part as u32)))
            {
                out.push_str(ligature);
                continue;
            }
        }
        if let Some(alt) = coverage_fallbacks(ch)
            .iter()
            .copied()
            .find(|alt| font.glyphs.contains_key(&(*alt as u32)))
        {
            out.push(alt);
            continue;
        }
        if let Some(base) = latin_base(ch) {
            if font.glyphs.contains_key(&(base as u32)) {
                out.push(base);
                continue;
            }
        }
        if let Some(plain) = math_plain(ch) {
            if font.glyphs.contains_key(&(plain as u32)) {
                out.push(plain);
                continue;
            }
            if let Some(ascii) = greek_ascii(plain) {
                if font.glyphs.contains_key(&(ascii as u32)) {
                    out.push(ascii);
                    continue;
                }
            }
        }
        out.push(ch);
    }
    out
}

/// A centered title is only as wide as the English ink. Chinese can use the
/// page measure on both sides. A column heading sits off the page center and
/// keeps its own column.
fn centered_heading_span(
    level: HeadingLevel,
    source_size: f32,
    media: [f32; 4],
    block_left: f32,
    block_right: f32,
) -> Option<(f32, f32)> {
    if level != HeadingLevel::Title {
        return None;
    }
    let page_left = media[0];
    let page_right = media[2];
    let page_width = page_right - page_left;
    if page_width < 200.0 {
        return None;
    }
    let mid = (block_left + block_right) * 0.5;
    let page_mid = (page_left + page_right) * 0.5;
    if (mid - page_mid).abs() > 28.0 {
        return None;
    }
    if block_left - page_left < 36.0 || page_right - block_right < 36.0 {
        return None;
    }
    let margin = (page_width * 0.11).clamp(54.0, 78.0);
    let left = page_left + margin;
    let right = page_right - margin;
    if right - left < (block_right - block_left) + source_size * 4.0 {
        return None;
    }
    Some((left, right))
}

fn ink_line_text(line: &InkLine<'_>) -> String {
    let mut glyphs = line.glyphs.to_vec();
    glyphs.sort_by(|left, right| left.matrix[4].total_cmp(&right.matrix[4]));
    glyphs.iter().map(|glyph| glyph.unicode.as_str()).collect()
}

struct NumberedItem {
    marker: String,
    marker_x: f32,
    marker_size: f32,
    marker_color: Color,
    text_left: f32,
    text_color: Color,
}

/// A `1` / `2` item whose wrapped lines share a hanging indent.
fn numbered_hanging(ink: &[InkLine<'_>]) -> Option<NumberedItem> {
    if ink.len() < 2 {
        return None;
    }
    let mut glyphs = ink[0].glyphs.clone();
    glyphs.sort_by(|left, right| left.matrix[4].total_cmp(&right.matrix[4]));
    let mut marker = String::new();
    let mut marker_color = None;
    let mut marker_size = 1.0f32;
    let mut marker_x = ink[0].left;
    let mut text_x = None;
    for glyph in &glyphs {
        let token = glyph.unicode.trim();
        if token.is_empty() {
            continue;
        }
        let marker_char = token
            .chars()
            .all(|ch| ch.is_ascii_digit() || ch == '.' || ch == ')');
        if text_x.is_none() && marker_char {
            if marker.is_empty() {
                marker_x = glyph.matrix[4];
                marker_size = glyph.font_size.max(1.0);
                marker_color = Some(glyph.fill_color.clone());
            }
            marker.push_str(token);
            continue;
        }
        if marker.is_empty() {
            return None;
        }
        text_x = Some(glyph.matrix[4]);
        break;
    }
    let text_x = text_x?;
    let digits = marker.chars().filter(|ch| ch.is_ascii_digit()).count();
    if digits == 0 || digits > 2 || marker_has_minor_number(&marker) {
        return None;
    }
    let text_left = ink[1..]
        .iter()
        .map(|line| line.left)
        .fold(f32::MAX, f32::min);
    let size = marker_size.max(1.0);
    if (text_left - text_x).abs() > (size * 0.55).max(4.0) {
        return None;
    }
    let shift = text_left - marker_x;
    if !(size * 0.35..=size * 3.0).contains(&shift) {
        return None;
    }
    let marker_color = marker_color?;
    let text_color = ink[1..]
        .iter()
        .flat_map(|line| line.glyphs.iter())
        .find(|glyph| glyph.unicode.chars().any(|ch| ch.is_ascii_alphabetic()))
        .map(|glyph| glyph.fill_color.clone())
        .unwrap_or_else(Color::black);
    Some(NumberedItem {
        marker,
        marker_x,
        marker_size,
        marker_color,
        text_left,
        text_color,
    })
}

/// `1.1` is a section index. `1.` is a list number with a closing dot.
fn marker_has_minor_number(marker: &str) -> bool {
    let bytes = marker.as_bytes();
    bytes.iter().enumerate().any(|(index, byte)| {
        *byte == b'.'
            && bytes
                .get(index + 1)
                .is_some_and(|next| next.is_ascii_digit())
    })
}

/// Two source title lines. Keep a short Latin lead (`AI`) on the first
/// line so the Chinese word that follows stays whole.
fn split_display_title(text: &str, count: usize) -> Vec<String> {
    let trimmed = text.trim();
    if count == 2 {
        if let Some((lead, rest)) = trimmed.split_once(' ') {
            let rest = rest.trim();
            if !lead.is_empty()
                && lead.chars().count() <= 12
                && rest.chars().filter(|ch| !ch.is_whitespace()).count() >= 3
            {
                return vec![lead.to_string(), rest.to_string()];
            }
        }
    }
    spread_to_lines(vec![trimmed.to_string()], count)
}

/// A tall narrow path in the right margin: the giant chapter numeral.
fn decorative_numeral_left(extraction: &Extraction, page: u32, y: f32, size: f32) -> Option<f32> {
    let page_width = extraction
        .pages
        .iter()
        .find(|info| info.index == page)
        .map(|info| info.media_box[2] - info.media_box[0])
        .unwrap_or(600.0);
    let mut best: Option<f32> = None;
    for region in extraction
        .regions
        .iter()
        .filter(|region| region.page_index == page && region.kind == "path")
    {
        let x0 = region.bbox[0].min(region.bbox[2]);
        let x1 = region.bbox[0].max(region.bbox[2]);
        let y0 = region.bbox[1].min(region.bbox[3]);
        let y1 = region.bbox[1].max(region.bbox[3]);
        let width = x1 - x0;
        let height = y1 - y0;
        if !(40.0..=140.0).contains(&width) || height < 100.0 || height < width * 1.2 {
            continue;
        }
        if x0 < page_width * 0.55 {
            continue;
        }
        let top = y + size;
        let bottom = y - size * 2.2;
        if y1 < bottom || y0 > top {
            continue;
        }
        best = Some(best.map_or(x0, |current| current.min(x0)));
    }
    best
}

struct BulletItem {
    marker: String,
    marker_x: f32,
    marker_size: f32,
    marker_color: Color,
    text_left: f32,
    text_color: Color,
}

/// A dingbat item, one line or wrapped. The mark keeps the source glyph,
/// colour, size, and x. Every line of the words starts at the hanging indent.
/// A dash item (`- Part`) stays on `list_outdent`.
fn single_line_bullet(ink: &[InkLine<'_>]) -> Option<BulletItem> {
    if ink.is_empty() {
        return None;
    }
    let mut glyphs = ink[0].glyphs.clone();
    glyphs.sort_by(|left, right| left.matrix[4].total_cmp(&right.matrix[4]));
    let mut marker_ids = Vec::new();
    let mut marker_x = 0.0f32;
    let mut marker_size = 1.0f32;
    let mut marker_color = None;
    let mut marker_char = None;
    let mut text_x = None;
    let mut text_color = None;
    for glyph in &glyphs {
        let token = glyph.unicode.trim();
        if token.is_empty() {
            continue;
        }
        if text_x.is_none()
            && marker_ids.is_empty()
            && token.chars().count() == 1
            && is_graphic_bullet(token.chars().next().unwrap_or('\0'))
        {
            marker_ids.push(glyph.id);
            marker_x = glyph.matrix[4];
            marker_size = glyph.font_size.max(1.0);
            marker_color = Some(glyph.fill_color.clone());
            marker_char = token.chars().next();
            continue;
        }
        if marker_ids.is_empty() {
            return None;
        }
        if text_x.is_none() {
            text_x = Some(glyph.matrix[4]);
            text_color = Some(glyph.fill_color.clone());
        }
    }
    // The first line may be only the mark. The hanging indent is the next line.
    if text_x.is_none() {
        let next = ink.get(1)?;
        text_x = Some(next.left);
        text_color = next
            .glyphs
            .iter()
            .find(|glyph| !glyph.unicode.trim().is_empty())
            .map(|glyph| glyph.fill_color.clone());
    }
    let mut text_x = text_x?;
    if ink.len() >= 2 {
        let hanging = ink[1..]
            .iter()
            .map(|line| line.left)
            .fold(f32::MAX, f32::min);
        // Wrapped lines already share the indent. A first line that starts
        // in the gutter moves out to that same edge.
        if hanging.is_finite() && (hanging - text_x).abs() > 0.8 && hanging > marker_x {
            text_x = hanging;
        }
    }
    let marker_char = marker_char?;
    let shift = text_x - marker_x;
    if !(marker_size * 0.35..=marker_size * 3.5).contains(&shift) {
        return None;
    }
    let _ = marker_ids;
    let marker = if ('\u{E000}'..='\u{F8FF}').contains(&marker_char) {
        "■".to_string()
    } else {
        marker_char.to_string()
    };
    Some(BulletItem {
        marker,
        marker_x,
        marker_size,
        marker_color: marker_color?,
        text_left: text_x,
        text_color: text_color?,
    })
}

/// Drop a leading dingbat the model kept or that was restored in front of
/// the words. An em dash that opens the sentence stays.
fn bullet_body_text(text: &str) -> &str {
    let rest = text.trim_start();
    let Some(first) = rest.chars().next() else {
        return rest;
    };
    let bullet = matches!(first, '•' | '●' | '◦' | '▪' | '■' | '‣' | '∙' | '·' | '‧')
        || ('\u{E000}'..='\u{F8FF}').contains(&first)
        || (matches!(first, '-' | '–')
            && rest[first.len_utf8()..].starts_with(char::is_whitespace));
    if !bullet {
        return rest;
    }
    rest[first.len_utf8()..].trim_start()
}

/// How far a list's wrapped lines sit to the right of the marker.
fn list_outdent(ink: &[InkLine<'_>], size: f32) -> Option<f32> {
    if ink.len() < 2 || size <= 0.0 {
        return None;
    }
    if !crate::segment::starts_with_list_marker(&ink_line_text(&ink[0])) {
        return None;
    }
    let body = ink[1..]
        .iter()
        .map(|line| line.left)
        .fold(f32::MAX, f32::min);
    let shift = body - ink[0].left;
    (size * 0.45..=size * 2.8).contains(&shift).then_some(shift)
}

/// The next baseline under `y`, and the tallest face on that baseline.
fn next_line_below(
    extraction: &Extraction,
    page: u32,
    y: f32,
    size: f32,
    own: &[&Glyph],
) -> Option<(f32, f32)> {
    let mut best_y: Option<f32> = None;
    for glyph in &extraction.glyphs {
        if glyph.page_index != page || own.iter().any(|item| item.id == glyph.id) {
            continue;
        }
        if glyph.unicode.trim().is_empty() {
            continue;
        }
        let below = glyph.matrix[5];
        if below > y - size * 0.35 {
            continue;
        }
        if best_y.is_none_or(|current| below > current) {
            best_y = Some(below);
        }
    }
    let best_y = best_y?;
    let mut next_size = 0.0f32;
    for glyph in &extraction.glyphs {
        if glyph.page_index != page || own.iter().any(|item| item.id == glyph.id) {
            continue;
        }
        if (glyph.matrix[5] - best_y).abs() > size * 0.45 {
            continue;
        }
        next_size = next_size.max(glyph.font_size);
    }
    (next_size > 0.0).then_some((best_y, next_size))
}

/// Markers redrawn between inline contents items. The source dingbat is a
/// separate operator; the flow paints one of these instead.
const SUBENTRY_BULLETS: &str = "■•▪";

fn subentry_font_chars(
    report: &TranslateReport,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) -> String {
    let mut chars = Vec::new();
    for run in find_subentry_runs(report, extraction, by_id, &HashSet::new()) {
        for item in &run.items {
            chars.extend(item.title.chars());
            chars.extend(item.page.chars());
        }
        chars.extend(SUBENTRY_BULLETS.chars());
    }
    chars.into_iter().collect()
}

struct SubentryShift {
    page: u32,
    below_y: f32,
    delta: f32,
    own_ids: HashSet<u32>,
}

/// A contents title whose page number sits on an indented wrap (`beyond 327`).
/// The two source lines are separate segments. They are painted as one title
/// at the first line's x.
struct HeadingWrap {
    continuation: usize,
    text: String,
    extra_ids: Vec<u32>,
    host_y: f32,
    cont_y: f32,
    page: u32,
}

#[derive(Clone)]
struct SubentryItem {
    title: String,
    page: String,
    title_ids: Vec<u32>,
    page_ids: Vec<u32>,
    italic: bool,
    color: Color,
    /// Highest source baseline, so a wrapped title sorts before the next item.
    y: f32,
    x: f32,
}

struct SubentryRun {
    page: u32,
    left: f32,
    right: f32,
    /// Source baselines, top to bottom.
    baselines: Vec<f32>,
    size: f32,
    items: Vec<SubentryItem>,
    bullet_ids: Vec<u32>,
    bullet_color: Color,
}

#[derive(Clone)]
struct FlowPiece {
    kind: FlowKind,
    text: String,
    italic: bool,
    color: Color,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FlowKind {
    Title,
    Page,
    Bullet,
}

/// Reflow each `title page ▪ title page` row inside the source column.
///
/// Wrapping keeps a page number on the same line as the end of its title and
/// refuses a one-character line. A run that needs more lines than the English
/// pushes later entries down. Inline page numbers stay upright, one word-space
/// after the title.
fn reflow_inline_subentries(
    drawn: &mut Vec<Drawn>,
    succeeded: &mut HashSet<u32>,
    keep: &mut HashMap<u32, String>,
    toc_redraw: &mut HashSet<u32>,
    report: &TranslateReport,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
    embedded: &[(String, SubsetFont)],
    font_styles: &HashMap<String, FaceStyle>,
    metrics: CjkMeasure,
    skip: &HashSet<usize>,
) -> Vec<SubentryShift> {
    // A heading wrap (`component` under `12.3`) already painted its page
    // number on the heading. Leaving it in the bullet run draws that number
    // a second time and the heading keeps neither.
    let runs = find_subentry_runs(report, extraction, by_id, skip);
    let mut shifts = Vec::new();
    for run in runs {
        let Some((flow, flow_size)) = flow_subentry(&run, embedded, font_styles, metrics) else {
            continue;
        };
        let mut own_ids: HashSet<u32> = HashSet::new();
        for item in &run.items {
            own_ids.extend(item.title_ids.iter().copied());
            own_ids.extend(item.page_ids.iter().copied());
            for id in &item.title_ids {
                keep.remove(id);
                succeeded.insert(*id);
            }
            for id in &item.page_ids {
                keep.remove(id);
                succeeded.insert(*id);
                toc_redraw.remove(id);
            }
        }
        for id in &run.bullet_ids {
            keep.remove(id);
            succeeded.insert(*id);
            own_ids.insert(*id);
        }
        drawn.retain(|item| item.glyph_ids.iter().any(|id| !own_ids.contains(id)));
        let source_bottom = run.baselines.last().copied().unwrap_or(run.baselines[0]);
        let (lines, extra) = place_subentry_lines(&run, &flow, flow_size);
        let ids: Vec<u32> = own_ids.iter().copied().collect();
        for line in &lines {
            drawn.extend(paint_flow_line(
                run.page,
                run.left,
                line,
                &ids,
                embedded,
                font_styles,
            ));
        }
        if extra.abs() > 0.4 {
            shifts.push(SubentryShift {
                page: run.page,
                below_y: source_bottom,
                delta: extra,
                own_ids,
            });
        }
    }
    shifts
}

fn apply_subentry_shifts(drawn: &mut [Drawn], shifts: &[SubentryShift]) {
    // Membership uses the source baseline. An earlier lift must not carry a
    // row above a later block's `below_y` and make that block skip it, or the
    // section title and its sub-entry move by different amounts.
    let original: Vec<f32> = drawn.iter().map(|line| line.y).collect();
    for (index, line) in drawn.iter_mut().enumerate() {
        let y0 = original[index];
        let mut delta = 0.0f32;
        for shift in shifts {
            if shift.page != line.page || y0 >= shift.below_y - 0.5 {
                continue;
            }
            if line.glyph_ids.iter().any(|id| shift.own_ids.contains(id)) {
                continue;
            }
            delta += shift.delta;
        }
        line.y -= delta;
    }
}

fn subentry_delta_at(shifts: &[SubentryShift], page: u32, y: f32) -> f32 {
    shifts
        .iter()
        .filter(|shift| shift.page == page && y < shift.below_y - 0.5)
        .map(|shift| shift.delta)
        .sum()
}

/// Chapter indexes stay in the source operator when the translation is the
/// same digit. `apply_subentry_shifts` only sees redrawn lines, so a shorter
/// block used to slide the title up and leave `3` on the English baseline.
fn shift_kept_chapter_indexes(
    drawn: &mut Vec<Drawn>,
    succeeded: &mut HashSet<u32>,
    keep: &mut HashMap<u32, String>,
    shifts: &[SubentryShift],
    extraction: &Extraction,
    embedded: &[(String, SubsetFont)],
    font_styles: &HashMap<String, FaceStyle>,
) {
    if shifts.is_empty() {
        return;
    }
    let painted: HashSet<u32> = drawn
        .iter()
        .flat_map(|item| item.glyph_ids.iter().copied())
        .collect();
    let mut extra = Vec::new();
    for glyph in &extraction.glyphs {
        if painted.contains(&glyph.id) || succeeded.contains(&glyph.id) {
            continue;
        }
        let text = glyph.unicode.trim();
        if glyph.font_size < 16.0
            || text.is_empty()
            || text.len() > 3
            || !text.bytes().all(|byte| byte.is_ascii_digit())
        {
            continue;
        }
        let delta = subentry_delta_at(shifts, glyph.page_index, glyph.matrix[5]);
        if delta.abs() <= 0.4 {
            continue;
        }
        let Some((resource, font, skew)) =
            font_for_page(glyph.page_index, glyph, embedded, drawn, font_styles)
        else {
            continue;
        };
        let Some(mut item) = draw_toc_glyph(glyph, font, resource, skew) else {
            continue;
        };
        item.y -= delta;
        keep.remove(&glyph.id);
        succeeded.insert(glyph.id);
        extra.push(item);
    }
    drawn.extend(extra);
}

fn find_subentry_runs(
    report: &TranslateReport,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
    skip: &HashSet<usize>,
) -> Vec<SubentryRun> {
    let bullets: Vec<&Glyph> = extraction
        .glyphs
        .iter()
        .filter(|glyph| is_subentry_bullet(glyph))
        .collect();
    if bullets.is_empty() {
        // A contents row with no bullet still keeps an italic page number
        // beside its title (`Implementing a prompt template … 32`).
        let mut seg_of: HashMap<u32, usize> = HashMap::new();
        for (index, segment) in report.segments.iter().enumerate() {
            for id in &segment.glyph_ids {
                seg_of.insert(*id, index);
            }
        }
        let mut runs = solo_italic_subentries(report, extraction, by_id, &seg_of, skip);
        share_subentry_column(&mut runs);
        return runs;
    }
    let mut seg_of: HashMap<u32, usize> = HashMap::new();
    for (index, segment) in report.segments.iter().enumerate() {
        for id in &segment.glyph_ids {
            seg_of.insert(*id, index);
        }
    }
    let count = report.segments.len();
    let mut parent: Vec<usize> = (0..count).collect();
    let mut bullet_roots: HashSet<usize> = HashSet::new();
    for bullet in &bullets {
        let near: Vec<usize> = segments_near_bullet(bullet, report, extraction, by_id)
            .into_iter()
            .filter(|index| !skip.contains(index))
            .collect();
        let Some(first) = near.first().copied() else {
            continue;
        };
        bullet_roots.insert(find_root(&mut parent, first));
        for index in near.iter().skip(1) {
            union_root(&mut parent, first, *index);
            bullet_roots.insert(*index);
        }
    }
    // A wrapped tail (`model` under `core object`) shares the indent and the
    // next baseline. A section heading is further left and stays out.
    let mut grew = true;
    while grew {
        grew = false;
        for left in 0..count {
            if skip.contains(&left) || !bullet_component(&parent, &bullet_roots, left) {
                continue;
            }
            for right in 0..count {
                if left == right || skip.contains(&right) || same_root(&parent, left, right) {
                    continue;
                }
                if segments_share_subentry(
                    &report.segments[left],
                    &report.segments[right],
                    extraction,
                    by_id,
                ) {
                    union_root(&mut parent, left, right);
                    grew = true;
                }
            }
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for index in 0..count {
        if skip.contains(&index) || !bullet_component(&parent, &bullet_roots, index) {
            continue;
        }
        let root = find_root(&mut parent, index);
        groups.entry(root).or_default().push(index);
    }
    let mut used = HashSet::new();
    let mut runs = Vec::new();
    for mut indices in groups.into_values() {
        // A 27pt chapter index can sit inside the gap window of the row
        // above. It is not an entry, and absorbing it blanks the big numeral.
        indices.retain(|index| {
            !skip.contains(index) && !is_chapter_index(&report.segments[*index], extraction, by_id)
        });
        if let Some(run) =
            build_subentry_run(&indices, &bullets, report, extraction, by_id, &seg_of)
        {
            for index in &indices {
                if !segment_spans_pages(&report.segments[*index], extraction, by_id) {
                    used.insert(*index);
                }
            }
            runs.push(run);
        }
    }
    used.extend(skip.iter().copied());
    runs.extend(solo_italic_subentries(
        report, extraction, by_id, &seg_of, &used,
    ));
    share_subentry_column(&mut runs);
    runs.sort_by(|left, right| {
        left.page
            .cmp(&right.page)
            .then(right.baselines[0].total_cmp(&left.baselines[0]))
    });
    runs
}

/// Sub-entry rows on one contents page share the source indent and the
/// source wrap edge. That edge is where the English runs end (`Q&A`,
/// `Minimal`), not the part-row page number at the page margin. A short
/// line such as `PromptTemplate 33` may use the wider sibling edge, and
/// wraps only once that column is full. Continuation lines use the same x.
/// Front matter sits further left (`preface` vs `Engines`); it keeps its
/// own column. Odd and even pages differ in the source and stay apart.
fn share_subentry_column(runs: &mut [SubentryRun]) {
    let mut by_page: HashMap<u32, Vec<usize>> = HashMap::new();
    for (index, run) in runs.iter().enumerate() {
        by_page.entry(run.page).or_default().push(index);
    }
    for indices in by_page.values() {
        let mut ordered = indices.clone();
        ordered.sort_by(|left, right| runs[*left].left.total_cmp(&runs[*right].left));
        let mut clusters: Vec<Vec<usize>> = Vec::new();
        for index in ordered {
            let left = runs[index].left;
            let size = runs[index].size.max(1.0);
            let join = clusters.last().is_some_and(|cluster| {
                let anchor = runs[cluster[0]].left;
                // Sub-entry blocks on one page share the source indent.
                // Front matter sits more than two ems further left and stays
                // its own column.
                (left - anchor).abs() <= size * 2.2
            });
            if join {
                clusters.last_mut().unwrap().push(index);
            } else {
                clusters.push(vec![index]);
            }
        }
        for cluster in clusters {
            let mut lefts: Vec<f32> = cluster.iter().map(|index| runs[*index].left).collect();
            lefts.sort_by(|left, right| left.total_cmp(right));
            let left = lefts[lefts.len() / 2];
            let ink = cluster
                .iter()
                .map(|index| runs[*index].right)
                .fold(0.0f32, f32::max);
            let size = cluster
                .iter()
                .map(|index| runs[*index].size)
                .fold(1.0f32, f32::max);
            // A bullet row's ink is the source column (`Q&A`, `Minimal`).
            // An italic row with no bullet (`preface xvi`) has no such
            // column; give it room for a longer Chinese title.
            let bulleted = cluster
                .iter()
                .any(|index| !runs[*index].bullet_ids.is_empty());
            let right = if bulleted {
                ink
            } else {
                ink.max(left + size * 16.0)
            };
            let unify_left = cluster.len() >= 2;
            for index in cluster {
                if unify_left {
                    runs[index].left = left;
                }
                if right > runs[index].right {
                    runs[index].right = right;
                }
            }
        }
    }
}

fn is_subentry_bullet(glyph: &Glyph) -> bool {
    let text = glyph.unicode.trim();
    if text.chars().count() == 1 && crate::segment::starts_with_list_marker(text) {
        // A hyphen inside `LLM-based` is its own glyph. It is not the bullet
        // between contents entries.
        if text == "-" {
            return false;
        }
        return true;
    }
    let font = glyph.font_name.to_ascii_uppercase();
    (font.contains("DINGBAT") || font.contains("WINGDING")) && text.chars().count() == 1
}

fn segments_near_bullet(
    bullet: &Glyph,
    report: &TranslateReport,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) -> Vec<usize> {
    let mut hits = Vec::new();
    for (index, segment) in report.segments.iter().enumerate() {
        let near = segment.glyph_ids.iter().any(|id| {
            extraction.glyphs.get(by_id[id]).is_some_and(|glyph| {
                glyph.page_index == bullet.page_index
                    && (glyph.matrix[5] - bullet.matrix[5]).abs() <= 4.0
                    && !is_subentry_bullet(glyph)
            })
        });
        if near
            && !section_heading_source(&segment.source)
            && !is_chapter_index(segment, extraction, by_id)
            && !segment_spans_pages(segment, extraction, by_id)
        {
            hits.push(index);
        }
    }
    hits
}

/// A contents chapter index (`5`, `10`) is one to three digits at display
/// size. A page number is the same shape at body size, so size is the split.
fn is_chapter_index(
    segment: &crate::translate::TranslatedSegment,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) -> bool {
    let text: String = segment
        .source
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    if text.is_empty() || text.len() > 3 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    segment.glyph_ids.iter().any(|id| {
        extraction
            .glyphs
            .get(by_id[id])
            .is_some_and(|glyph| !glyph.unicode.trim().is_empty() && glyph.font_size >= 16.0)
    })
}

fn segments_share_subentry(
    left: &crate::translate::TranslatedSegment,
    right: &crate::translate::TranslatedSegment,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) -> bool {
    if section_heading_source(&left.source) || section_heading_source(&right.source) {
        return false;
    }
    // A 27pt chapter index is not the next bullet entry. Its size makes the
    // gap to the row above look like one line.
    if is_chapter_index(left, extraction, by_id) || is_chapter_index(right, extraction, by_id) {
        return false;
    }
    if segment_spans_pages(left, extraction, by_id) || segment_spans_pages(right, extraction, by_id)
    {
        return false;
    }
    let left_box = segment_box(left, extraction, by_id);
    let right_box = segment_box(right, extraction, by_id);
    let (Some((page, l_left, l_top, l_bot, l_size)), Some((page2, r_left, r_top, r_bot, r_size))) =
        (left_box, right_box)
    else {
        return false;
    };
    if page != page2 {
        return false;
    }
    let size = l_size.max(r_size).max(1.0);
    // `Retrieving content` sits on the right of the previous entry and
    // continues at the left indent (`with the rewritten query`). The left
    // edges do not match, and the next entry's bullet shares the lower line.
    let right_wrap = right_side_title_wrap(
        left, right, page, l_left, l_top, r_left, r_top, size, extraction, by_id,
    );
    if !right_wrap && (l_left - r_left).abs() > 14.0 {
        return false;
    }
    let gap = l_bot.max(r_bot) - l_top.min(r_top);
    // Overlap is negative. A continuation sits about one line below.
    if gap >= size * 1.45 || gap <= -size * 4.0 {
        return false;
    }
    // `1 ■ Introduction 3` and `2 ■ Executing 27` are two entries. A wrapped
    // tail (`model 15`) has no bullet of its own and still joins.
    if gap > size * 0.45 && !right_wrap {
        let upper_y = l_top.max(r_top);
        let lower_y = l_bot.min(r_bot);
        if line_has_bullet(page, upper_y, extraction) && line_has_bullet(page, lower_y, extraction)
        {
            return false;
        }
    }
    true
}

/// A contents title that starts beside the previous entry and finishes on the
/// next line at the column indent. Both faces are italic, the lower line has
/// no bullet of its own, and the fragment is at least 8em to the right.
fn right_side_title_wrap(
    left: &crate::translate::TranslatedSegment,
    right: &crate::translate::TranslatedSegment,
    page: u32,
    l_left: f32,
    l_top: f32,
    r_left: f32,
    r_top: f32,
    size: f32,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) -> bool {
    if !segment_is_italic(left, extraction, by_id) || !segment_is_italic(right, extraction, by_id) {
        return false;
    }
    if left.source.chars().count() > 80 || right.source.chars().count() > 80 {
        return false;
    }
    let (upper_left, lower_left, upper_y, lower_y) = if l_top >= r_top {
        (l_left, r_left, l_top, r_top)
    } else {
        (r_left, l_left, r_top, l_top)
    };
    let dy = upper_y - lower_y;
    if dy <= size * 0.55 || dy >= size * 1.65 {
        return false;
    }
    if lower_left + size * 8.0 >= upper_left {
        return false;
    }
    !bullet_before(page, lower_left, lower_y, size, extraction)
}

fn segment_is_italic(
    segment: &crate::translate::TranslatedSegment,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) -> bool {
    let mut italic = 0usize;
    let mut total = 0usize;
    for id in &segment.glyph_ids {
        let Some(glyph) = extraction.glyphs.get(by_id[id]) else {
            continue;
        };
        if glyph.unicode.trim().is_empty() || is_subentry_bullet(glyph) {
            continue;
        }
        total += 1;
        if face_style(&glyph.font_name).italic {
            italic += 1;
        }
    }
    total > 0 && italic * 2 >= total
}

fn bullet_before(page: u32, x: f32, y: f32, size: f32, extraction: &Extraction) -> bool {
    extraction.glyphs.iter().any(|glyph| {
        glyph.page_index == page
            && is_subentry_bullet(glyph)
            && (glyph.matrix[5] - y).abs() <= 4.0
            && glyph.matrix[4] < x
            && x - glyph.matrix[4] <= size * 3.0
    })
}

fn line_has_bullet(page: u32, y: f32, extraction: &Extraction) -> bool {
    extraction.glyphs.iter().any(|glyph| {
        glyph.page_index == page && is_subentry_bullet(glyph) && (glyph.matrix[5] - y).abs() <= 4.0
    })
}

fn segment_spans_pages(
    segment: &crate::translate::TranslatedSegment,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) -> bool {
    let mut page = None;
    for id in &segment.glyph_ids {
        let Some(glyph) = extraction.glyphs.get(by_id[id]) else {
            continue;
        };
        if glyph.unicode.trim().is_empty() {
            continue;
        }
        if page.is_some_and(|seen| seen != glyph.page_index) {
            return true;
        }
        page = Some(glyph.page_index);
    }
    false
}

fn segment_box(
    segment: &crate::translate::TranslatedSegment,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) -> Option<(u32, f32, f32, f32, f32)> {
    let mut page = None;
    let mut left = f32::MAX;
    let mut top = f32::MIN;
    let mut bottom = f32::MAX;
    let mut size = 1.0f32;
    for id in &segment.glyph_ids {
        let glyph = extraction.glyphs.get(by_id[id])?;
        if glyph.unicode.trim().is_empty() || is_subentry_bullet(glyph) {
            continue;
        }
        page = Some(glyph.page_index);
        left = left.min(glyph.matrix[4]);
        top = top.max(glyph.matrix[5]);
        bottom = bottom.min(glyph.matrix[5]);
        size = size.max(glyph.font_size);
    }
    Some((page?, left, top, bottom, size))
}

fn section_heading_source(text: &str) -> bool {
    let Some(token) = text.split_whitespace().next() else {
        return false;
    };
    let core = token.trim_end_matches('.');
    core.contains('.') && core.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
}

fn bullet_component(parent: &[usize], roots: &HashSet<usize>, index: usize) -> bool {
    let mut cursor = index;
    let mut guard = 0;
    while parent[cursor] != cursor && guard < parent.len() {
        cursor = parent[cursor];
        guard += 1;
    }
    roots.contains(&cursor) || roots.contains(&index)
}

fn find_root(parent: &mut [usize], index: usize) -> usize {
    let mut cursor = index;
    while parent[cursor] != cursor {
        cursor = parent[cursor];
    }
    let mut back = index;
    while parent[back] != cursor {
        let next = parent[back];
        parent[back] = cursor;
        back = next;
    }
    cursor
}

fn union_root(parent: &mut [usize], left: usize, right: usize) {
    let left = find_root(parent, left);
    let right = find_root(parent, right);
    if left != right {
        parent[right] = left;
    }
}

fn same_root(parent: &[usize], left: usize, right: usize) -> bool {
    let mut a = left;
    let mut b = right;
    let mut guard = 0;
    while parent[a] != a && guard < parent.len() {
        a = parent[a];
        guard += 1;
    }
    guard = 0;
    while parent[b] != b && guard < parent.len() {
        b = parent[b];
        guard += 1;
    }
    a == b
}

/// An italic sub-entry with no bullet (`Implementing a prompt template … 32`,
/// `preface xvi`, `appendix A … 351`). The page number stays one space after
/// the title. A part row's number follows leader dots and is not this path.
fn solo_italic_subentries(
    report: &TranslateReport,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
    seg_of: &HashMap<u32, usize>,
    used: &HashSet<usize>,
) -> Vec<SubentryRun> {
    let mut runs = Vec::new();
    for (index, segment) in report.segments.iter().enumerate() {
        if used.contains(&index)
            || section_heading_source(&segment.source)
            || segment_spans_pages(segment, extraction, by_id)
        {
            continue;
        }
        if solo_page_token(segment.source.trim()) {
            continue;
        }
        if let Some(run) = build_solo_italic(segment, report, extraction, by_id, seg_of) {
            runs.push(run);
        }
    }
    runs
}

fn build_solo_italic(
    segment: &crate::translate::TranslatedSegment,
    report: &TranslateReport,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
    seg_of: &HashMap<u32, usize>,
) -> Option<SubentryRun> {
    let glyphs: Vec<&Glyph> = segment
        .glyph_ids
        .iter()
        .filter_map(|id| extraction.glyphs.get(by_id[id]))
        .filter(|glyph| !glyph.unicode.trim().is_empty() && !is_subentry_bullet(glyph))
        .collect();
    if glyphs.len() < 2 {
        return None;
    }
    let italic = glyphs
        .iter()
        .filter(|glyph| face_style(&glyph.font_name).italic)
        .count()
        * 2
        >= glyphs.len();
    if !italic {
        return None;
    }
    // A BoldItalic chapter title (`AI 代理和应用简介`) is a heading. Folding
    // it into this path paints a light italic. Its page number is seated
    // after the title by the contents pass, in the source face.
    let bold = glyphs
        .iter()
        .filter(|glyph| face_style(&glyph.font_name).bold)
        .count()
        * 2
        >= glyphs.len();
    if bold {
        return None;
    }
    let page = glyphs[0].page_index;
    if glyphs.iter().any(|glyph| glyph.page_index != page) {
        return None;
    }
    let size = {
        let mut sizes: Vec<f32> = glyphs.iter().map(|glyph| glyph.font_size).collect();
        sizes.sort_by(|a, b| a.total_cmp(b));
        sizes[sizes.len() / 2].max(1.0)
    };
    let mut ys: Vec<f32> = glyphs.iter().map(|glyph| glyph.matrix[5]).collect();
    ys.sort_by(|a, b| b.total_cmp(a));
    let mut baselines = Vec::new();
    for y in ys {
        if baselines
            .last()
            .is_none_or(|prev: &f32| (prev - y).abs() > 1.6)
        {
            baselines.push(y);
        }
    }
    let numbers = page_numbers_near(&baselines, page, size, extraction);
    let toc_page = page_text_for_item(&glyphs, &numbers, seg_of, size);
    let (page_label, page_ids) = if !toc_page.text.is_empty() && solo_page_token(&toc_page.text) {
        (toc_page.text, toc_page.ids)
    } else {
        adjacent_page_segment(&glyphs, size, report, extraction, by_id)?
    };
    if number_follows_leaders(&page_ids, extraction, size) {
        return None;
    }
    let title = strip_list_marker(&display_title(segment));
    if title.is_empty() {
        return None;
    }
    let left = glyphs
        .iter()
        .map(|glyph| glyph.matrix[4])
        .fold(f32::MAX, f32::min);
    let ink_right = glyphs
        .iter()
        .map(|glyph| glyph_ink_right(glyph))
        .fold(left, f32::max);
    let right = page_ids
        .iter()
        .filter_map(|id| {
            by_id
                .get(id)
                .and_then(|index| extraction.glyphs.get(*index))
        })
        .map(glyph_ink_right)
        .fold(ink_right, f32::max);
    let y = glyphs
        .iter()
        .map(|glyph| glyph.matrix[5])
        .fold(f32::MIN, f32::max);
    Some(SubentryRun {
        page,
        left,
        right,
        baselines,
        size,
        items: vec![SubentryItem {
            title,
            page: page_label,
            title_ids: segment.glyph_ids.clone(),
            page_ids,
            italic: true,
            color: glyphs[0].fill_color.clone(),
            y,
            x: left,
        }],
        bullet_ids: Vec::new(),
        bullet_color: glyphs[0].fill_color.clone(),
    })
}

/// A page number that column splitting pulled off the title (`function` / `32`).
/// It is still the same row when it sits within two ems on that baseline.
fn adjacent_page_segment(
    title: &[&Glyph],
    size: f32,
    report: &TranslateReport,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) -> Option<(String, Vec<u32>)> {
    if title.is_empty() {
        return None;
    }
    let right = title
        .iter()
        .map(|glyph| glyph_ink_right(glyph))
        .fold(f32::MIN, f32::max);
    let y = title
        .iter()
        .map(|glyph| glyph.matrix[5])
        .fold(f32::MIN, f32::max);
    let page = title[0].page_index;
    let mut best: Option<(f32, String, Vec<u32>)> = None;
    for segment in &report.segments {
        let token = segment.source.trim();
        if !solo_page_token(token) {
            continue;
        }
        let glyphs: Vec<&Glyph> = segment
            .glyph_ids
            .iter()
            .filter_map(|id| {
                by_id
                    .get(id)
                    .and_then(|index| extraction.glyphs.get(*index))
            })
            .filter(|glyph| !glyph.unicode.trim().is_empty())
            .collect();
        if glyphs.is_empty() || glyphs.iter().any(|glyph| glyph.page_index != page) {
            continue;
        }
        let number_y = glyphs
            .iter()
            .map(|glyph| glyph.matrix[5])
            .fold(f32::MIN, f32::max);
        if (number_y - y).abs() > 2.0 {
            continue;
        }
        let left = glyphs
            .iter()
            .map(|glyph| glyph_ink_left(glyph))
            .fold(f32::MAX, f32::min);
        if left + 0.5 < right {
            continue;
        }
        let gap = left - right;
        if gap > size * 2.0 {
            continue;
        }
        if best.as_ref().is_none_or(|(best_gap, _, _)| gap < *best_gap) {
            let ids = glyphs.iter().map(|glyph| glyph.id).collect();
            best = Some((gap, token.to_string(), ids));
        }
    }
    best.map(|(_, text, ids)| (text, ids))
}

fn solo_page_token(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > 6 {
        return false;
    }
    if text.bytes().all(|byte| byte.is_ascii_digit()) {
        return true;
    }
    text.chars().all(|ch| {
        matches!(
            ch.to_ascii_lowercase(),
            'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'
        )
    })
}

fn number_follows_leaders(ids: &[u32], extraction: &Extraction, size: f32) -> bool {
    let Some(first_id) = ids.first() else {
        return false;
    };
    let Some(first) = extraction.glyphs.iter().find(|glyph| glyph.id == *first_id) else {
        return false;
    };
    let left = glyph_ink_left(first);
    extraction.glyphs.iter().any(|glyph| {
        glyph.page_index == first.page_index
            && is_toc_leader_glyph(glyph)
            && (glyph.matrix[5] - first.matrix[5]).abs() <= size * 0.45
            && left - glyph_ink_right(glyph) < size * 0.8
            && glyph_ink_right(glyph) <= left + 1.0
    })
}

fn build_subentry_run(
    indices: &[usize],
    bullets: &[&Glyph],
    report: &TranslateReport,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
    seg_of: &HashMap<u32, usize>,
) -> Option<SubentryRun> {
    if indices.len() < 2 {
        return None;
    }
    let mut glyphs: Vec<&Glyph> = Vec::new();
    for index in indices {
        if segment_spans_pages(&report.segments[*index], extraction, by_id) {
            continue;
        }
        for id in &report.segments[*index].glyph_ids {
            if let Some(glyph) = extraction.glyphs.get(by_id[id]) {
                if !glyph.unicode.trim().is_empty() && !is_subentry_bullet(glyph) {
                    glyphs.push(glyph);
                }
            }
        }
    }
    if glyphs.is_empty() {
        return None;
    }
    let page = {
        let mut counts: HashMap<u32, usize> = HashMap::new();
        for glyph in &glyphs {
            *counts.entry(glyph.page_index).or_default() += 1;
        }
        counts
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(page, _)| page)
            .unwrap_or(glyphs[0].page_index)
    };
    glyphs.retain(|glyph| glyph.page_index == page);
    let size = {
        let mut sizes: Vec<f32> = glyphs.iter().map(|glyph| glyph.font_size).collect();
        sizes.sort_by(|a, b| a.total_cmp(b));
        sizes[sizes.len() / 2].max(1.0)
    };
    let mut ys: Vec<f32> = glyphs.iter().map(|glyph| glyph.matrix[5]).collect();
    ys.sort_by(|a, b| b.total_cmp(a));
    let mut baselines = Vec::new();
    for y in ys {
        if baselines
            .last()
            .is_none_or(|prev: &f32| (prev - y).abs() > 1.6)
        {
            baselines.push(y);
        }
    }
    let left = glyphs
        .iter()
        .map(|glyph| glyph.matrix[4])
        .fold(f32::MAX, f32::min);
    let mut right = glyphs
        .iter()
        .map(|glyph| glyph_ink_right(glyph))
        .fold(left, f32::max);
    let run_bullets: Vec<&Glyph> = bullets
        .iter()
        .copied()
        .filter(|glyph| {
            glyph.page_index == page && baselines.iter().any(|y| (glyph.matrix[5] - y).abs() <= 4.0)
        })
        .collect();
    if run_bullets.is_empty() {
        return None;
    }
    for bullet in &run_bullets {
        right = right.max(glyph_ink_right(bullet));
    }
    let numbers = page_numbers_near(&baselines, page, size, extraction);
    for number in &numbers {
        right = right.max(glyph_ink_right(number.last().copied()?));
    }
    let mut items = Vec::new();
    for index in indices {
        let segment = &report.segments[*index];
        if segment_spans_pages(segment, extraction, by_id) {
            continue;
        }
        let title = strip_list_marker(&display_title(segment));
        if title.is_empty() {
            continue;
        }
        let item_glyphs: Vec<&Glyph> = segment
            .glyph_ids
            .iter()
            .filter_map(|id| extraction.glyphs.get(by_id[id]))
            .filter(|glyph| !glyph.unicode.trim().is_empty() && !is_subentry_bullet(glyph))
            .collect();
        if item_glyphs.is_empty() {
            continue;
        }
        let y = item_glyphs
            .iter()
            .map(|glyph| glyph.matrix[5])
            .fold(f32::MIN, f32::max);
        let x = item_glyphs
            .iter()
            .map(|glyph| glyph.matrix[4])
            .fold(f32::MAX, f32::min);
        let italic = item_glyphs
            .iter()
            .filter(|glyph| face_style(&glyph.font_name).italic)
            .count()
            * 2
            >= item_glyphs.len();
        let page_text = page_text_for_item(&item_glyphs, &numbers, seg_of, size);
        items.push(SubentryItem {
            title,
            page: page_text.text,
            // Spaces stay in the segment. A draw that still carries them must
            // be dropped with the title, or the English line is painted twice.
            title_ids: segment.glyph_ids.clone(),
            page_ids: page_text.ids,
            italic,
            color: item_glyphs[0].fill_color.clone(),
            y,
            x,
        });
    }
    if items.len() < 2 || items.iter().all(|item| item.page.is_empty()) {
        return None;
    }
    items.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x.total_cmp(&b.x)));
    merge_wrapped_titles(&mut items, size);
    Some(SubentryRun {
        page,
        left,
        right,
        baselines,
        size,
        items,
        bullet_ids: run_bullets.iter().map(|glyph| glyph.id).collect(),
        bullet_color: run_bullets[0].fill_color.clone(),
    })
}

struct PageText {
    text: String,
    ids: Vec<u32>,
}

fn page_text_for_item(
    item: &[&Glyph],
    numbers: &[Vec<&Glyph>],
    seg_of: &HashMap<u32, usize>,
    size: f32,
) -> PageText {
    let mut best: Option<(f32, usize)> = None;
    for (index, number) in numbers.iter().enumerate() {
        let Some(first) = number.first() else {
            continue;
        };
        if seg_of.contains_key(&first.id) {
            continue;
        }
        let y = first.matrix[5];
        let x = glyph_ink_left(first);
        let on_line = item
            .iter()
            .filter(|glyph| (glyph.matrix[5] - y).abs() <= 2.0);
        let mut right = f32::MIN;
        let mut any = false;
        for glyph in on_line {
            right = right.max(glyph_ink_right(glyph));
            any = true;
        }
        if !any || x + 0.5 < right {
            continue;
        }
        let gap = x - right;
        if gap > size * 2.0 {
            continue;
        }
        if best.is_none_or(|(best_gap, _)| gap < best_gap) {
            best = Some((gap, index));
        }
    }
    let Some((_, index)) = best else {
        return PageText {
            text: String::new(),
            ids: Vec::new(),
        };
    };
    let number = &numbers[index];
    PageText {
        text: number.iter().map(|glyph| glyph.unicode.trim()).collect(),
        ids: number.iter().map(|glyph| glyph.id).collect(),
    }
}

fn page_numbers_near<'a>(
    baselines: &[f32],
    page: u32,
    size: f32,
    extraction: &'a Extraction,
) -> Vec<Vec<&'a Glyph>> {
    let mut digits: Vec<&Glyph> = extraction
        .glyphs
        .iter()
        .filter(|glyph| {
            glyph.page_index == page
                && is_inline_page_glyph(glyph)
                && baselines.iter().any(|y| (glyph.matrix[5] - y).abs() <= 3.0)
        })
        .collect();
    digits.sort_by(|a, b| {
        b.matrix[5]
            .total_cmp(&a.matrix[5])
            .then(a.matrix[4].total_cmp(&b.matrix[4]))
    });
    let mut groups: Vec<Vec<&Glyph>> = Vec::new();
    for glyph in digits {
        let start_new = groups.last().is_none_or(|group| {
            let prev = group[group.len() - 1];
            (prev.matrix[5] - glyph.matrix[5]).abs() > 1.5
                || glyph_ink_left(glyph) - glyph_ink_right(prev) > size * 0.65
        });
        if start_new {
            groups.push(vec![glyph]);
        } else if let Some(group) = groups.last_mut() {
            group.push(glyph);
        }
    }
    groups
}

fn is_inline_page_glyph(glyph: &Glyph) -> bool {
    let text = glyph.unicode.trim();
    if text.chars().count() != 1 {
        return false;
    }
    let Some(ch) = text.chars().next() else {
        return false;
    };
    let page_char = ch.is_ascii_digit()
        || matches!(
            ch.to_ascii_lowercase(),
            'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'
        );
    page_char
        && matches!(
            &glyph.disposition,
            Disposition::KeptOriginal { reason } if reason == "toc"
        )
}

/// `core object` / `model 15` is one title that wrapped. The page number
/// belongs to the whole title, and there is no bullet between the lines.
fn merge_wrapped_titles(items: &mut Vec<SubentryItem>, size: f32) {
    let mut index = 0;
    while index < items.len() {
        if !items[index].page.is_empty() {
            index += 1;
            continue;
        }
        let upper_y = items[index].y;
        let upper_x = items[index].x;
        let Some(lower_at) = items.iter().enumerate().position(|(other, item)| {
            other != index
                && !item.page.is_empty()
                && upper_y > item.y
                && upper_y - item.y > size * 0.55
                && upper_y - item.y < size * 1.65
                && ((upper_x - item.x).abs() <= 8.0 || item.x + 8.0 < upper_x)
        }) else {
            index += 1;
            continue;
        };
        let lower = items[lower_at].clone();
        items[index].title = join_wrapped_title(&items[index].title, &lower.title);
        items[index].page = lower.page;
        items[index].page_ids = lower.page_ids;
        items[index].title_ids.extend(lower.title_ids);
        items.remove(lower_at);
        if lower_at < index {
            index -= 1;
        }
    }
}

fn join_wrapped_title(upper: &str, lower: &str) -> String {
    let upper = upper.trim_end();
    let lower = lower.trim_start();
    if upper.is_empty() {
        return lower.to_string();
    }
    if lower.is_empty() {
        return upper.to_string();
    }
    // `组合成` + `一个` is one phrase. A space belongs between Latin words
    // (`LangChain` + `architecture`), not between Han characters.
    let han_join = upper.chars().next_back().is_some_and(is_han_char)
        && lower.chars().next().is_some_and(is_han_char);
    if han_join {
        format!("{upper}{lower}")
    } else {
        format!("{upper} {lower}")
    }
}

fn display_title(segment: &crate::translate::TranslatedSegment) -> String {
    let translated = crate::translate::restore_list_marker(
        &segment.source,
        &crate::translate::localize_part_heading(&segment.translated),
    );
    let translated = crate::translate::restore_credit_names(&segment.source, &translated);
    if translated.trim().is_empty() {
        segment.source.clone()
    } else {
        translated
    }
}

fn strip_list_marker(text: &str) -> String {
    let trimmed = text.trim();
    if !crate::segment::starts_with_list_marker(trimmed) {
        return trimmed.to_string();
    }
    trimmed
        .chars()
        .skip(1)
        .collect::<String>()
        .trim()
        .to_string()
}

struct FlowLine {
    y: f32,
    pieces: Vec<FlowPiece>,
    size: f32,
}

fn measure_font<'a>(
    run: &SubentryRun,
    embedded: &'a [(String, SubsetFont)],
    styles: &HashMap<String, FaceStyle>,
) -> Option<&'a SubsetFont> {
    let want_italic = run.items.iter().any(|item| item.italic);
    let mut best: Option<(&SubsetFont, i32)> = None;
    for (resource, font) in embedded {
        if !run_text_covered(run, font) {
            continue;
        }
        let style = styles.get(resource).copied().unwrap_or(FaceStyle {
            serif: true,
            bold: false,
            italic: false,
        });
        let mut score = 0;
        if style.italic == want_italic {
            score += 4;
        }
        if !style.bold {
            score += 2;
        }
        if style.serif {
            score += 1;
        }
        if best
            .as_ref()
            .is_none_or(|(_, best_score)| score > *best_score)
        {
            best = Some((font, score));
        }
    }
    best.map(|(font, _)| font)
}

fn flow_subentry(
    run: &SubentryRun,
    embedded: &[(String, SubsetFont)],
    styles: &HashMap<String, FaceStyle>,
    metrics: CjkMeasure,
) -> Option<(Vec<Vec<FlowPiece>>, f32)> {
    let width = run.right - run.left;
    if width < run.size * 8.0 || embedded.is_empty() {
        return None;
    }
    let sample = measure_font(run, embedded, styles)?;
    let preferred = run.size * metrics.body_scale.clamp(0.85, 1.05);
    let floor = run.size * 0.85;
    let at_preferred = layout_subentry_flow(run, width, preferred, sample)?;
    let (chosen, size) = if at_preferred.len() > run.baselines.len() {
        let at_floor = layout_subentry_flow(run, width, floor, sample)?;
        if at_floor.len() < at_preferred.len() && at_floor.len() <= run.baselines.len() {
            (at_floor, floor)
        } else {
            (at_preferred, preferred)
        }
    } else {
        (at_preferred, preferred)
    };
    Some((chosen, size))
}

fn run_text_covered(run: &SubentryRun, font: &SubsetFont) -> bool {
    let needs_bullet = !run.bullet_ids.is_empty() || run.items.len() > 1;
    run.items.iter().all(|item| {
        text_covered(&item.title, font) && (item.page.is_empty() || text_covered(&item.page, font))
    }) && (!needs_bullet || text_covered("■", font))
}

fn text_covered(text: &str, font: &SubsetFont) -> bool {
    text.chars()
        .all(|ch| ch.is_whitespace() || font.glyphs.contains_key(&(ch as u32)))
}

fn layout_subentry_flow(
    run: &SubentryRun,
    width: f32,
    size: f32,
    font: &SubsetFont,
) -> Option<Vec<Vec<FlowPiece>>> {
    let bullet_gap = size * 0.45;
    let bullet_w = (size * 0.42).max(2.5);
    let mut lines: Vec<Vec<FlowPiece>> = vec![Vec::new()];
    let mut used = 0.0f32;
    for (index, item) in run.items.iter().enumerate() {
        let page_gap = page_number_gap(size, item.italic);
        let title_lines =
            title_lines_with_page(&item.title, &item.page, width, size, font, page_gap)?;
        let first_width = piece_width(&title_lines, item, size, font, page_gap);
        if index > 0 {
            let lead = bullet_gap + bullet_w + bullet_gap;
            let fits = used > 0.0 && used + lead + first_width <= width + 0.4;
            if fits {
                lines.last_mut()?.push(bullet_piece(run));
                used += lead;
            } else if used > 0.0 {
                // The source draws ■ only between two items that share a
                // line. A wrap starts the next item at the column: no
                // trailing ■ on this line, and no ■ opening the next one.
                lines.push(Vec::new());
                used = 0.0;
            }
        }
        for (line_index, line) in title_lines.iter().enumerate() {
            if line_index > 0 {
                lines.push(Vec::new());
                used = 0.0;
            }
            lines.last_mut()?.push(FlowPiece {
                kind: FlowKind::Title,
                text: line.clone(),
                italic: item.italic,
                color: item.color.clone(),
            });
            used += measure(line, size, font);
            if line_index + 1 == title_lines.len() && !item.page.is_empty() {
                lines.last_mut()?.push(FlowPiece {
                    kind: FlowKind::Page,
                    text: item.page.clone(),
                    italic: false,
                    color: item.color.clone(),
                });
                used += page_gap + measure(&item.page, size, font);
            }
        }
    }
    lines.retain(|line| !line.is_empty());
    if lines.is_empty() {
        None
    } else {
        Some(lines)
    }
}

fn bullet_piece(run: &SubentryRun) -> FlowPiece {
    FlowPiece {
        kind: FlowKind::Bullet,
        text: "■".to_string(),
        italic: false,
        color: run.bullet_color.clone(),
    }
}

fn piece_width(
    title_lines: &[String],
    item: &SubentryItem,
    size: f32,
    font: &SubsetFont,
    gap: f32,
) -> f32 {
    let Some(first) = title_lines.first() else {
        return 0.0;
    };
    let mut width = measure(first, size, font);
    if title_lines.len() == 1 && !item.page.is_empty() {
        width += gap + measure(&item.page, size, font);
    }
    width
}

/// Title lines. The page number stays on the last line, and that line is
/// never a single leftover character when the title is longer.
/// Half an em after the title. An italic shear hangs about 0.2em past the
/// advance, so the page number starts further right and the ink gap stays
/// half an em.
fn page_number_gap(size: f32, italic: bool) -> f32 {
    size * 0.50 + if italic { size * 0.20 } else { 0.0 }
}

fn title_lines_with_page(
    title: &str,
    page: &str,
    width: f32,
    size: f32,
    font: &SubsetFont,
    gap: f32,
) -> Option<Vec<String>> {
    let page_w = if page.is_empty() {
        0.0
    } else {
        gap + measure(page, size, font)
    };
    if measure(title, size, font) + page_w <= width + 0.4 {
        return Some(vec![title.to_string()]);
    }
    let last_budget = (width - page_w).max(size * 2.0);
    let mut lines = wrap_text(title, size, width, width, font)?;
    let mut guard = 0;
    while guard < 8 {
        guard += 1;
        let last = lines.last()?.clone();
        if measure(&last, size, font) <= last_budget + 0.4 {
            break;
        }
        // Break on a phrase or a word. A raw character cut turns
        // `Protocol` into `Protoc` / `ol`.
        let budget = last_budget.max(size);
        let Some(rewrapped) = wrap_text(&last, size, budget, budget, font) else {
            break;
        };
        if rewrapped.len() <= 1 {
            break;
        }
        lines.pop();
        lines.extend(rewrapped);
    }
    absorb_orphan_tails(&mut lines);
    if lines.iter().any(|line| line.is_empty()) {
        return None;
    }
    Some(lines)
}

fn absorb_orphan_tails(lines: &mut Vec<String>) {
    let mut index = 0;
    while index < lines.len() {
        let alone = lines[index].chars().count() == 1;
        if !alone || lines.len() < 2 {
            index += 1;
            continue;
        }
        if index > 0 {
            let prev: Vec<char> = lines[index - 1].chars().collect();
            if prev.len() >= 2 {
                let moved = prev[prev.len() - 1];
                lines[index - 1] = prev[..prev.len() - 1].iter().collect();
                let tail = lines[index].clone();
                lines[index] = format!("{moved}{tail}");
            } else {
                let tail = lines[index].clone();
                lines[index - 1] = format!("{}{tail}", lines[index - 1]);
                lines.remove(index);
                continue;
            }
        } else if lines.len() > 1 {
            let next: Vec<char> = lines[1].chars().collect();
            if let Some(first) = next.first() {
                lines[0] = format!("{}{first}", lines[0]);
                lines[1] = next[1..].iter().collect();
                if lines[1].is_empty() {
                    lines.remove(1);
                }
            }
        }
        index += 1;
    }
}

fn place_subentry_lines(
    run: &SubentryRun,
    flow: &[Vec<FlowPiece>],
    size: f32,
) -> (Vec<FlowLine>, f32) {
    let step = if run.baselines.len() >= 2 {
        let mut gaps: Vec<f32> = run
            .baselines
            .windows(2)
            .map(|pair| pair[0] - pair[1])
            .filter(|gap| *gap > 0.5)
            .collect();
        if gaps.is_empty() {
            run.size * 1.20
        } else {
            gaps.sort_by(|a, b| a.total_cmp(b));
            gaps[gaps.len() / 2].max(run.size * 1.10)
        }
    } else {
        run.size * 1.20
    };
    let mut ys = Vec::with_capacity(flow.len());
    for (index, _) in flow.iter().enumerate() {
        if index < run.baselines.len() {
            ys.push(run.baselines[index]);
        } else {
            let prev = ys[index - 1];
            ys.push(prev - step);
        }
    }
    // Positive when the Chinese block is taller than the source: followers
    // move down. Negative when it is shorter: followers move up by the
    // unused source lines, so the gap under the block stays the source gap.
    let extra = if ys.is_empty() || run.baselines.is_empty() {
        0.0
    } else {
        run.baselines[run.baselines.len() - 1] - ys[ys.len() - 1]
    };
    let lines = flow
        .iter()
        .enumerate()
        .map(|(index, pieces)| FlowLine {
            y: ys[index],
            pieces: pieces.clone(),
            size,
        })
        .collect();
    (lines, extra)
}

fn paint_flow_line(
    page: u32,
    left: f32,
    line: &FlowLine,
    ids: &[u32],
    embedded: &[(String, SubsetFont)],
    styles: &HashMap<String, FaceStyle>,
) -> Vec<Drawn> {
    let bullet_gap = line.size * 0.45;
    let bullet_size = (line.size * 0.42).max(2.5);
    // Every line of the block starts at `left`. A separator sits between
    // two items on that same line. It does not hang in the gutter, open
    // the next line, or dangle after the last item of a wrap.
    let mut x = left;
    let mut previous_bullet = false;
    let mut previous_italic = false;
    let mut out = Vec::new();
    for piece in &line.pieces {
        let Some((resource, font)) = font_for_flow_piece(piece, embedded, styles) else {
            continue;
        };
        let (draw_size, skew, advance) = match piece.kind {
            FlowKind::Bullet => {
                if x > left {
                    x += bullet_gap;
                }
                (bullet_size, 0.0, bullet_size)
            }
            FlowKind::Page => {
                x += page_number_gap(line.size, previous_italic);
                (line.size, 0.0, measure(&piece.text, line.size, font))
            }
            FlowKind::Title => {
                if previous_bullet {
                    x += bullet_gap;
                }
                (
                    line.size,
                    if piece.italic { 0.25 } else { 0.0 },
                    measure(&piece.text, line.size, font),
                )
            }
        };
        let y = if piece.kind == FlowKind::Bullet {
            line.y + line.size * 0.12
        } else {
            line.y
        };
        let count = piece.text.chars().count();
        out.push(Drawn {
            page,
            x,
            y,
            size: draw_size,
            color: piece.color.clone(),
            cids: cids_of(&piece.text, font),
            resource: resource.to_string(),
            skew,
            widths: char_widths(&piece.text, draw_size, font),
            gaps: vec![0.0; count.saturating_sub(1)],
            text: piece.text.clone(),
            supers: vec![false; count],
            sup_scale: 1.0,
            sup_rise: 0.0,
            glyph_ids: ids.to_vec(),
        });
        x += advance;
        previous_italic = piece.kind == FlowKind::Title && piece.italic;
        previous_bullet = piece.kind == FlowKind::Bullet;
    }
    out
}

/// Titles keep the source italic face. Page numbers and bullets stay roman,
/// so a bold body face that happens to contain the same characters is not used.
fn font_for_flow_piece<'a>(
    piece: &FlowPiece,
    embedded: &'a [(String, SubsetFont)],
    styles: &HashMap<String, FaceStyle>,
) -> Option<(&'a str, &'a SubsetFont)> {
    let want_italic = piece.kind == FlowKind::Title && piece.italic;
    let mut best: Option<(&str, &SubsetFont, i32)> = None;
    for (resource, font) in embedded {
        if !text_covered(&piece.text, font) {
            continue;
        }
        let style = styles.get(resource).copied().unwrap_or(FaceStyle {
            serif: true,
            bold: false,
            italic: false,
        });
        let mut score = 0;
        if style.italic == want_italic {
            score += 4;
        }
        if !style.bold {
            score += 2;
        }
        if style.serif {
            score += 1;
        }
        if best
            .as_ref()
            .is_none_or(|(_, _, best_score)| score > *best_score)
        {
            best = Some((resource.as_str(), font, score));
        }
    }
    best.map(|(resource, font, _)| (resource, font))
}

struct TextPiece<'a> {
    y: f32,
    left: f32,
    right: f32,
    size: f32,
    glyphs: Vec<&'a Glyph>,
}

struct ColonSplit {
    y: f32,
    left: f32,
    size: f32,
    colon_right: f32,
    value_x: f32,
}

struct LabelAxis {
    colon_right: f32,
    value_x: f32,
    continuation: bool,
}

fn page_text_pieces(extraction: &Extraction, page: u32) -> Vec<TextPiece<'_>> {
    let mut glyphs: Vec<&Glyph> = extraction
        .glyphs
        .iter()
        .filter(|glyph| glyph.page_index == page && !glyph.invisible && !glyph.unicode.is_empty())
        .collect();
    glyphs.sort_by(|left, right| {
        right.matrix[5]
            .total_cmp(&left.matrix[5])
            .then(left.matrix[4].total_cmp(&right.matrix[4]))
    });
    let mut rows: Vec<Vec<&Glyph>> = Vec::new();
    for glyph in glyphs {
        let same = rows
            .last()
            .is_some_and(|row: &Vec<&Glyph>| (row[0].matrix[5] - glyph.matrix[5]).abs() <= 0.8);
        if same {
            rows.last_mut().unwrap().push(glyph);
        } else {
            rows.push(vec![glyph]);
        }
    }
    let mut pieces = Vec::new();
    for row in rows {
        if row.is_empty() {
            continue;
        }
        let size = crate::segment::representative_size(&row).max(1.0);
        let hard = (size * 1.5).max(12.0);
        let mut current = vec![row[0]];
        for glyph in row.into_iter().skip(1) {
            let prev = current[current.len() - 1];
            let right = prev.bbox[2].max(prev.matrix[4]);
            let left = glyph.matrix[4].min(glyph.bbox[0]);
            if left - right > hard {
                pieces.push(text_piece(current));
                current = vec![glyph];
            } else {
                current.push(glyph);
            }
        }
        pieces.push(text_piece(current));
    }
    pieces
}

fn text_piece(glyphs: Vec<&Glyph>) -> TextPiece<'_> {
    let left = glyphs
        .iter()
        .map(|glyph| glyph.matrix[4])
        .fold(f32::MAX, f32::min);
    let right = glyphs
        .iter()
        .map(|glyph| glyph.bbox[2].max(glyph.matrix[4]))
        .fold(left, f32::max);
    let size = crate::segment::representative_size(&glyphs).max(1.0);
    let y = glyphs[0].matrix[5];
    TextPiece {
        y,
        left,
        right,
        size,
        glyphs,
    }
}

/// A colon that introduces a name, not `https://` or `Email: orders@`.
fn colon_split(piece: &TextPiece<'_>) -> Option<ColonSplit> {
    let mut colon_at = None;
    for (index, glyph) in piece.glyphs.iter().enumerate() {
        let mark = glyph.unicode.trim();
        if mark != ":" && mark != "：" {
            continue;
        }
        let next = piece
            .glyphs
            .get(index + 1)
            .map(|glyph| glyph.unicode.as_str())
            .unwrap_or("");
        if next.starts_with('/') {
            continue;
        }
        colon_at = Some(index);
    }
    let index = colon_at?;
    let colon = piece.glyphs[index];
    let value = piece
        .glyphs
        .iter()
        .skip(index + 1)
        .find(|glyph| !glyph.unicode.trim().is_empty())?;
    let value_text = glyphs_text(&piece.glyphs[index + 1..]);
    let value_trim = value_text.trim();
    if !value_trim
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_uppercase())
        || value_trim.chars().count() > 48
    {
        return None;
    }
    let label = glyphs_text(&piece.glyphs[..index]);
    if label.trim().is_empty() || label.trim().chars().count() > 40 {
        return None;
    }
    if piece.right - piece.left > piece.size * 36.0 {
        return None;
    }
    Some(ColonSplit {
        y: piece.y,
        left: piece.left,
        size: piece.size,
        colon_right: colon.bbox[2].max(colon.matrix[4]),
        value_x: value.matrix[4],
    })
}

fn median_f32(values: impl Iterator<Item = f32>) -> f32 {
    let mut values: Vec<f32> = values.collect();
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|left, right| left.total_cmp(right));
    values[values.len() / 2]
}

/// Right-aligned role labels share a colon axis. Names, and a continuation
/// such as `and Ada`, share the value's left edge.
fn label_axis_for(extraction: &Extraction, line: &InkLine<'_>) -> Option<LabelAxis> {
    let page = line.glyphs.first()?.page_index;
    let pieces = page_text_pieces(extraction, page);
    let mut colons: Vec<ColonSplit> = pieces.iter().filter_map(colon_split).collect();
    colons.sort_by(|left, right| left.value_x.total_cmp(&right.value_x));
    let mut groups: Vec<Vec<ColonSplit>> = Vec::new();
    for item in colons {
        let fits = groups.last().is_some_and(|group| {
            (item.value_x - group[0].value_x).abs() <= 3.0
                && (item.colon_right - group[0].colon_right).abs() <= 5.0
                && (item.size - group[0].size).abs() <= item.size.max(group[0].size) * 0.25
        });
        if fits {
            groups.last_mut().unwrap().push(item);
        } else {
            groups.push(vec![item]);
        }
    }
    let mut clusters = Vec::new();
    for mut group in groups {
        group.sort_by(|left, right| right.y.total_cmp(&left.y));
        let mut run: Vec<ColonSplit> = Vec::new();
        for item in group {
            if let Some(prev) = run.last() {
                // One continuation (`and Ada`) sits between two role lines.
                if prev.y - item.y > prev.size.max(item.size) * 3.4 {
                    if run.len() >= 2 {
                        clusters.push(std::mem::take(&mut run));
                    } else {
                        run.clear();
                    }
                }
            }
            run.push(item);
        }
        if run.len() >= 2 {
            clusters.push(run);
        }
    }
    let matched = clusters.iter().find(|cluster| {
        cluster
            .iter()
            .any(|item| (item.y - line.y).abs() <= 0.9 && (item.left - line.left).abs() <= 8.0)
    });
    if let Some(cluster) = matched {
        return Some(LabelAxis {
            colon_right: median_f32(cluster.iter().map(|item| item.colon_right)),
            value_x: median_f32(cluster.iter().map(|item| item.value_x)),
            continuation: false,
        });
    }
    let source = glyphs_text(&{
        let mut ordered = line.glyphs.clone();
        ordered.sort_by(|left, right| left.matrix[4].total_cmp(&right.matrix[4]));
        ordered
    });
    if !source.trim_start().starts_with("and ") {
        return None;
    }
    let cluster = clusters.iter().find(|cluster| {
        let value_x = median_f32(cluster.iter().map(|item| item.value_x));
        let size = cluster[0].size.max(1.0);
        (line.left - value_x).abs() <= 4.0
            && line.right - line.left <= size * 36.0
            && cluster
                .iter()
                .any(|item| (item.y - line.y).abs() <= size * 1.8)
    })?;
    Some(LabelAxis {
        colon_right: median_f32(cluster.iter().map(|item| item.colon_right)),
        value_x: median_f32(cluster.iter().map(|item| item.value_x)),
        continuation: true,
    })
}

fn split_translated_label(text: &str) -> Option<(String, String)> {
    let index = text.find([':', '：'])?;
    let label = text[..index].trim();
    if label.is_empty() || label.chars().count() > 24 || !label.chars().any(is_han_char) {
        return None;
    }
    let colon = text[index..].chars().next()?;
    let value = text[index + colon.len_utf8()..].trim();
    if value.is_empty() {
        return None;
    }
    Some((label.to_string(), value.to_string()))
}

fn plain_draw(
    page: u32,
    x: f32,
    y: f32,
    size: f32,
    color: Color,
    text: &str,
    font: &SubsetFont,
    resource: &str,
    skew: f32,
    glyph_ids: Vec<u32>,
) -> Drawn {
    let count = text.chars().count();
    Drawn {
        page,
        x,
        y,
        size,
        color,
        cids: cids_of(text, font),
        resource: resource.to_string(),
        skew,
        widths: char_widths(text, size, font),
        gaps: vec![0.0; count.saturating_sub(1)],
        text: text.to_string(),
        supers: vec![false; count],
        sup_scale: 1.0,
        sup_rise: 0.0,
        glyph_ids,
    }
}

fn layout_label_value_list(
    glyphs: &[&Glyph],
    ink: &[InkLine<'_>],
    text: &str,
    font: &SubsetFont,
    extraction: &Extraction,
    resource: &str,
    skew: f32,
    source_size: f32,
    metrics: CjkMeasure,
    media_left: f32,
) -> Option<Vec<Drawn>> {
    if ink.len() != 1 {
        return None;
    }
    let axis = label_axis_for(extraction, &ink[0])?;
    let scale = scale_for_heading(HeadingLevel::Body, source_size, metrics);
    let mut size = (source_size * scale).max(1.0);
    let floor = (source_size * 0.80).min(size);
    let y = ink[0].y;
    let page = ink[0].glyphs[0].page_index;
    let color = ink[0].glyphs[0].fill_color.clone();
    let ids: Vec<u32> = glyphs.iter().map(|glyph| glyph.id).collect();
    if axis.continuation {
        let text = text.trim();
        if text.is_empty()
            || text.chars().count() > 48
            || text
                .chars()
                .any(|ch| !font.glyphs.contains_key(&(ch as u32)))
        {
            return None;
        }
        return Some(vec![plain_draw(
            page,
            axis.value_x,
            y,
            size,
            color,
            text,
            font,
            resource,
            skew,
            ids,
        )]);
    }
    let (label, value) = split_translated_label(text)?;
    let colon = if label.chars().any(is_han_char) && font.glyphs.contains_key(&('：' as u32)) {
        "："
    } else {
        ":"
    };
    let label_text = format!("{label}{colon}");
    if label_text
        .chars()
        .chain(value.chars())
        .any(|ch| !font.glyphs.contains_key(&(ch as u32)))
    {
        return None;
    }
    loop {
        let width = measure(&label_text, size, font);
        if axis.colon_right - width >= media_left || size <= floor + 0.01 {
            break;
        }
        let next = (size * 0.94).max(floor);
        if (next - size).abs() < 0.01 {
            break;
        }
        size = next;
    }
    let width = measure(&label_text, size, font);
    let x = axis.colon_right - width;
    Some(vec![
        plain_draw(
            page,
            x,
            y,
            size,
            color.clone(),
            &label_text,
            font,
            resource,
            skew,
            ids.clone(),
        ),
        plain_draw(
            page,
            axis.value_x,
            y,
            size,
            color,
            &value,
            font,
            resource,
            skew,
            ids,
        ),
    ])
}

/// The widest row in a left-aligned stack. A short address line uses that
/// width so its translation stays on one baseline at the shared left edge.
fn shared_left_stack_right(extraction: &Extraction, line: &InkLine<'_>) -> Option<f32> {
    let page = line.glyphs.first()?.page_index;
    let size = crate::segment::representative_size(&line.glyphs).max(1.0);
    let pieces = page_text_pieces(extraction, page);
    let mut siblings: Vec<&TextPiece<'_>> = pieces
        .iter()
        .filter(|piece| {
            (piece.left - line.left).abs() <= 1.5
                && (piece.size - size).abs() <= size * 0.2
                && piece.right - piece.left <= size * 16.0
        })
        .collect();
    if siblings.len() < 2 {
        return None;
    }
    siblings.sort_by(|left, right| right.y.total_cmp(&left.y));
    let index = siblings
        .iter()
        .position(|piece| (piece.y - line.y).abs() <= 0.8)?;
    let mut low = index;
    let mut high = index;
    while low > 0 {
        let gap = siblings[low - 1].y - siblings[low].y;
        if gap > 0.0 && gap <= size * 1.8 {
            low -= 1;
        } else {
            break;
        }
    }
    while high + 1 < siblings.len() {
        let gap = siblings[high].y - siblings[high + 1].y;
        if gap > 0.0 && gap <= size * 1.8 {
            high += 1;
        } else {
            break;
        }
    }
    // Two left-aligned lines are often a title and the next entry. An address
    // block is three or more short rows, and the short one may use that width.
    if high - low + 1 < 3 {
        return None;
    }
    let right = siblings[low..=high]
        .iter()
        .map(|piece| piece.right)
        .fold(line.right, f32::max);
    (right > line.right + size * 0.4).then_some(right)
}

/// Lines that share a right edge and do not share a left edge are a
/// right-aligned cluster (a title page). A justified column shares both edges,
/// and a contents digit column is only a few ems wide.
/// `WITH LANGCHAIN` is 19.98pt initials and 15.96pt small caps. Chinese has
/// no small-caps face, so the line is set at the cap size.
fn small_caps_cap_size(sizes: &[f32]) -> Option<f32> {
    if sizes.len() < 6 {
        return None;
    }
    let mut clusters: Vec<Vec<f32>> = Vec::new();
    for size in sizes {
        if let Some(cluster) = clusters.last_mut() {
            let anchor = cluster[0];
            if (size - anchor).abs() <= anchor * 0.05 {
                cluster.push(*size);
                continue;
            }
        }
        clusters.push(vec![*size]);
    }
    if clusters.len() != 2 {
        return None;
    }
    let small = clusters[0][clusters[0].len() / 2];
    let large = clusters[1][clusters[1].len() / 2];
    if small < 1.0 {
        return None;
    }
    let ratio = large / small;
    if !(1.18..=1.35).contains(&ratio) || clusters[1].len() * 2 >= sizes.len() {
        return None;
    }
    Some(large)
}

fn shared_right_edge(extraction: &Extraction, line: &InkLine<'_>) -> Option<f32> {
    let page = line.glyphs.first()?.page_index;
    let size = crate::segment::representative_size(&line.glyphs).max(1.0);
    let pieces = page_text_pieces(extraction, page);
    let cluster: Vec<&TextPiece<'_>> = pieces
        .iter()
        .filter(|piece| {
            (piece.right - line.right).abs() <= 2.5
                && piece.right - piece.left >= piece.size * 0.8
                && piece
                    .glyphs
                    .iter()
                    .any(|glyph| !glyph.unicode.trim().is_empty())
        })
        .collect();
    if cluster.len() < 3 {
        return None;
    }
    let left_min = cluster
        .iter()
        .map(|piece| piece.left)
        .fold(f32::MAX, f32::min);
    let left_max = cluster
        .iter()
        .map(|piece| piece.left)
        .fold(f32::MIN, f32::max);
    if left_max - left_min <= size * 4.0 {
        return None;
    }
    let flush_left = cluster
        .iter()
        .filter(|piece| (piece.left - left_min).abs() <= 3.0)
        .count();
    if flush_left * 2 > cluster.len() {
        return None;
    }
    Some(line.right)
}

/// A contents baseline has a kept page number or leader dots. Title-page
/// right alignment must not slide that row.
fn contents_baseline(extraction: &Extraction, line: &InkLine<'_>) -> bool {
    let Some(page) = line.glyphs.first().map(|glyph| glyph.page_index) else {
        return false;
    };
    let size = crate::segment::representative_size(&line.glyphs).max(1.0);
    extraction.glyphs.iter().any(|glyph| {
        glyph.page_index == page
            && (glyph.matrix[5] - line.y).abs() <= size * 0.55
            && (is_toc_leader_glyph(glyph) || is_inline_page_glyph(glyph))
    })
}

/// `1.1` / `2.1` occupy their own column. The title starts at the source
/// title x, so every section title on the page shares that x, and the index
/// stays at the source index size rather than growing with a display face.
fn layout_indexed_contents_heading(
    glyphs: &[&Glyph],
    line: &InkLine<'_>,
    text: &str,
    font: &SubsetFont,
    extraction: &Extraction,
    resource: &str,
    skew: f32,
    metrics: CjkMeasure,
) -> Option<Vec<Drawn>> {
    if !contents_baseline(extraction, line) {
        return None;
    }
    let (index_glyphs, title_glyphs) = split_contents_index(glyphs)?;
    let source_index: String = index_glyphs
        .iter()
        .map(|glyph| glyph.unicode.trim())
        .collect();
    let (index_text, title_text) = match split_indexed_label(text) {
        Some((index, title)) if index == source_index => (index.to_string(), title.to_string()),
        Some(_) => return None,
        None => (source_index.clone(), text.trim().to_string()),
    };
    if title_text.chars().count() > 80 || ends_with_sentence(&title_text) {
        return None;
    }
    if !text_covered(&index_text, font) || !text_covered(&title_text, font) {
        return None;
    }
    let title_size = {
        let mut sizes: Vec<f32> = title_glyphs.iter().map(|glyph| glyph.font_size).collect();
        sizes.sort_by(|left, right| left.total_cmp(right));
        sizes[sizes.len() / 2].max(1.0)
    };
    let bold = title_glyphs
        .iter()
        .all(|glyph| face_style(&glyph.font_name).bold);
    let level = heading_level(1, title_size, bold, text);
    let scale = scale_for_heading(level, title_size, metrics);
    let size = title_size * scale;
    let index_x = index_glyphs
        .iter()
        .map(|glyph| glyph_ink_left(glyph))
        .fold(f32::MAX, f32::min);
    let title_x = title_glyphs
        .iter()
        .map(|glyph| glyph_ink_left(glyph))
        .fold(f32::MAX, f32::min);
    let page = index_glyphs[0].page_index;
    let media = extraction
        .pages
        .iter()
        .find(|info| info.index == page)
        .map(|info| info.media_box)
        .unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let room = media[2] - title_x - size * 3.0;
    if measure(&title_text, size, font) > room.max(size * 4.0) {
        return None;
    }
    let y = line.y;
    let color = title_glyphs[0].fill_color.clone();
    let index_ids: Vec<u32> = index_glyphs.iter().map(|glyph| glyph.id).collect();
    let title_ids: Vec<u32> = title_glyphs.iter().map(|glyph| glyph.id).collect();
    let piece = |x: f32, drawn_text: &str, ids: Vec<u32>| Drawn {
        page,
        x,
        y,
        size,
        color: color.clone(),
        cids: cids_of(drawn_text, font),
        resource: resource.to_string(),
        skew,
        widths: char_widths(drawn_text, size, font),
        gaps: vec![0.0; drawn_text.chars().count().saturating_sub(1)],
        text: drawn_text.to_string(),
        supers: vec![false; drawn_text.chars().count()],
        sup_scale: 1.0,
        sup_rise: 0.0,
        glyph_ids: ids,
    };
    Some(vec![
        piece(index_x, &index_text, index_ids),
        piece(title_x, &title_text, title_ids),
    ])
}

fn split_contents_index<'a>(glyphs: &[&'a Glyph]) -> Option<(Vec<&'a Glyph>, Vec<&'a Glyph>)> {
    let mut ordered: Vec<&Glyph> = glyphs
        .iter()
        .copied()
        .filter(|glyph| !glyph.unicode.trim().is_empty())
        .collect();
    ordered.sort_by(|left, right| glyph_ink_left(left).total_cmp(&glyph_ink_left(right)));
    let mut index = Vec::new();
    let mut cursor = 0;
    while cursor < ordered.len() {
        let token = ordered[cursor].unicode.trim();
        // `1.1` is three glyphs. The dot has no digit, but it still belongs
        // to the index when another digit follows it. A sentence period
        // (`1. The`) does not.
        let mark = !token.is_empty() && token.chars().all(|ch| ch.is_ascii_digit() || ch == '.');
        let digit = token.chars().any(|ch| ch.is_ascii_digit());
        let dot_bridge = !digit
            && token.chars().all(|ch| ch == '.')
            && !index.is_empty()
            && ordered.get(cursor + 1).is_some_and(|glyph| {
                let next = glyph.unicode.trim();
                !next.is_empty()
                    && next.chars().any(|ch| ch.is_ascii_digit())
                    && next.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
            });
        if !(mark && (digit || dot_bridge)) {
            break;
        }
        index.push(ordered[cursor]);
        cursor += 1;
    }
    if index.is_empty() || cursor >= ordered.len() {
        return None;
    }
    let index_text: String = index.iter().map(|glyph| glyph.unicode.trim()).collect();
    if !index_text.contains('.') {
        return None;
    }
    let index_right = index
        .iter()
        .map(|glyph| glyph_ink_right(glyph))
        .fold(0.0f32, f32::max);
    let title_left = glyph_ink_left(ordered[cursor]);
    let size = index
        .iter()
        .map(|glyph| glyph.font_size)
        .fold(1.0f32, f32::max)
        .max(1.0);
    if title_left - index_right < size * 0.35 {
        return None;
    }
    Some((index, ordered[cursor..].to_vec()))
}

fn split_indexed_label(text: &str) -> Option<(&str, &str)> {
    let text = text.trim();
    let bytes = text.as_bytes();
    let mut end = 0;
    while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
        end += 1;
    }
    if end == 0 || !text[..end].contains('.') {
        return None;
    }
    let rest = text[end..].trim_start();
    if rest.is_empty() {
        return None;
    }
    Some((&text[..end], rest))
}

fn ends_with_sentence(text: &str) -> bool {
    text.ends_with('.')
        || text.ends_with('。')
        || text.ends_with('!')
        || text.ends_with('?')
        || text.ends_with('？')
        || text.ends_with('！')
}

fn layout_segment(
    glyphs: &[&Glyph],
    text: &str,
    font: &SubsetFont,
    extraction: &Extraction,
    bilingual: bool,
    resource: &str,
    skew: f32,
    metrics: CjkMeasure,
) -> Option<Vec<Drawn>> {
    if glyphs.is_empty() || text.trim().is_empty() {
        return None;
    }
    let covered = substitute_covered(text, font);
    let raw_mask = superscript_mask(&covered);
    let (normalized, body_mask) = fullwidth_han_digits(&covered, &raw_mask, font);
    let (normalized, body_mask) = if font.glyphs.contains_key(&0x20) {
        crate::layout::space_cjk_latin_mask(&normalized, &body_mask)
    } else {
        (normalized, body_mask)
    };
    let text = normalized.as_str();
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
    let pages = pages_of(glyphs);
    if pages.len() > 1 {
        return layout_across_pages(glyphs, text, font, extraction, resource, skew, metrics);
    }
    if !bilingual && ink.len() == 1 {
        if let Some(drawn) = layout_indexed_contents_heading(
            glyphs, &ink[0], text, font, extraction, resource, skew, metrics,
        ) {
            return Some(drawn);
        }
    }
    let size0 = ink
        .iter()
        .map(|line| crate::segment::representative_size(&line.glyphs))
        .fold(1.0f32, f32::max);
    if ink
        .iter()
        .any(|line| segment_crosses_column(&line.glyphs, glyphs, size0))
    {
        return None;
    }
    let mut sizes: Vec<f32> = ink
        .iter()
        .flat_map(|line| line.glyphs.iter().map(|glyph| glyph.font_size))
        .collect();
    sizes.sort_by(|a, b| a.total_cmp(b));
    // Small caps store the initials at the cap size and the rest smaller.
    // The median is the small size, which paints the Chinese under the caps.
    let source_size = if ink.len() == 1 {
        small_caps_cap_size(&sizes).unwrap_or(sizes[sizes.len() / 2])
    } else {
        sizes[sizes.len() / 2]
    }
    .max(1.0);
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
    // An English first line may sit to the right of the column. Chinese
    // indent is measured from the body edge, not added on top of that gap.
    if spans.len() >= 2 {
        let body_left = spans[1..]
            .iter()
            .map(|(left, _)| *left)
            .fold(f32::MAX, f32::min);
        if spans[0].0 > body_left + source_size * 0.6 {
            block_left = body_left;
        }
    }
    block_right = block_right.min(media[2] - 1.0);
    block_left = block_left.max(media[0]);
    // A role:name list is right-aligned in the source. Fit each translation
    // to that colon axis before a paragraph reflow slides the labels left.
    if !bilingual {
        if let Some(drawn) = layout_label_value_list(
            glyphs,
            &ink,
            text,
            font,
            extraction,
            resource,
            skew,
            source_size,
            metrics,
            media[0],
        ) {
            return Some(drawn);
        }
    }
    // Short rows that share a left edge (an address) use the stack's width.
    // Fitting each row to its own ink shrinks or wraps the longest translation.
    if !bilingual && ink.len() == 1 {
        if let Some(right) = shared_left_stack_right(extraction, &ink[0]) {
            block_right = block_right.max(right).min(media[2] - 1.0);
        }
    }
    // `RAG` is three letters beside `1.4.2`. Fitting the translation to that
    // ink leaves `检索增强生成` marked tight and the English title in place.
    // The title column runs to the body margin.
    if !bilingual && ink.len() == 1 && index_sits_left_of(extraction, &ink[0]) {
        let room = media[2] - 36.0;
        if room > block_right + source_size {
            block_right = room;
        }
    }
    // A cover line changes fill where the panel ends. Fit the translation
    // inside that primary run instead of the English tail on the artwork.
    if ink.len() == 1 && source_size >= 16.0 && text.chars().count() <= 48 {
        if let Some(panel) = display_panel_right(&ink[0]) {
            let limit = panel.max(block_left + source_size * 4.0);
            block_right = block_right.min(limit);
        }
    }
    let available = (top_y - bottom_y).max(0.0);
    let bold = paragraph_is_bold(&ink);
    let numbered = numbered_hanging(&ink);
    let mut level = heading_level(ink.len(), source_size, bold, text);
    if numbered.is_some() {
        level = HeadingLevel::Body;
    }
    let bullet = if numbered.is_some() {
        None
    } else {
        single_line_bullet(&ink)
    };
    let hanging = if numbered.is_some() || bullet.is_some() {
        None
    } else {
        list_outdent(&ink, source_size)
    };
    let mut indent_ems = cjk_indent_ems(ink.len(), source_size, bold, text);
    // A list marker hangs left of the wrapped lines. Continuations share one
    // indent; the marker line outdents by that same amount.
    if let Some(shift) = hanging {
        block_left = (block_left + shift).min(block_right - source_size.max(1.0));
        indent_ems = -(shift / source_size.max(1.0));
    }
    // A numbered item keeps the source number in the margin. Every line of
    // the words shares the hanging indent, with no first-line indent.
    let body_text;
    let text = if let Some(item) = &numbered {
        block_left = item.text_left.min(block_right - source_size.max(1.0));
        indent_ems = 0.0;
        body_text = crate::segment::numbered_item_body(text)
            .unwrap_or(text)
            .to_string();
        body_text.as_str()
    } else if let Some(item) = &bullet {
        // The words share the source hanging indent, with no first-line
        // indent. The mark is drawn again on this baseline.
        block_left = item.text_left.min(block_right - source_size.max(1.0));
        indent_ems = 0.0;
        body_text = bullet_body_text(text).to_string();
        body_text.as_str()
    } else {
        text
    };
    // A giant chapter numeral is a path on the right. The title stops before
    // it instead of running through the stem.
    let numeral_left = if source_size >= 14.0 {
        decorative_numeral_left(extraction, page, top_y, source_size)
    } else {
        None
    };
    if let Some(limit) = numeral_left {
        let clearance = limit - source_size * 0.35;
        if clearance > block_left + source_size * 4.0 {
            block_right = block_right.min(clearance);
        }
    }
    let scale = scale_for_heading(level, source_size, metrics);
    let fit_box = |left: f32, right: f32| {
        let width = (right - left).max(source_size);
        fit_cjk_block(
            text,
            source_size,
            scale,
            width,
            indent_ems,
            available,
            font,
            metrics,
        )
    };
    let (block_left, block_right, lines, mut size, leading, indent) =
        if let Some((lines, size, leading, indent)) = fit_box(block_left, block_right) {
            (block_left, block_right, lines, size, leading, indent)
        } else if let Some((left, right)) =
            centered_heading_span(level, source_size, media, block_left, block_right)
        {
            let (lines, size, leading, indent) = fit_box(left, right)?;
            (left, right, lines, size, leading, indent)
        } else {
            return None;
        };
    let width = (block_right - block_left).max(source_size);
    let first_width = (width - indent).max(size * 0.5);
    let sup = superscript_metrics(glyphs);
    let mask = if sup.is_some() {
        body_mask
    } else {
        vec![false; text.chars().count()]
    };
    // A short Chinese paragraph used to sit on the first baselines and leave
    // the rest of the English lines empty. Spreading it back onto those
    // breaks cuts words (`API`) and starts a line on `；`. A full column
    // keeps the wrap and covers the source lines by absorbing them.
    let reflow = column_reflow(level, ink.len(), width, source_size, hanging.is_some());
    let mut absorbed: Vec<u32> = Vec::new();
    // A narrow contents tail (`engines` beside page number `4`) is packed
    // before a full-column reflow. Reflow would keep `引擎` on the wide line
    // and leave `4` as the first mark on the next baseline.
    let lines = if bilingual || hanging.is_some() {
        // A dash item is one paragraph. Spreading it onto the source line
        // breaks leaves each continuation at a different indent.
        lines
    } else if let Some(packed) = pack_narrow_tail(text, size, indent, &spans, font) {
        for (index, line) in packed.iter().enumerate() {
            if line.trim().is_empty() {
                absorbed.extend(ink[index].glyphs.iter().map(|glyph| glyph.id));
            }
        }
        packed
            .into_iter()
            .filter(|line| !line.trim().is_empty())
            .collect()
    } else if reflow || level == HeadingLevel::Title {
        // A chapter title that fits one line stays one line. Spreading it
        // onto the English baselines splits 智能体. A title that only fit by
        // shrinking into a chapter numeral is split back onto the source
        // baselines at the source size, and the right edge stays clear of
        // that numeral.
        let start_size = (source_size * scale).max(1.0);
        if numeral_left.is_some() && ink.len() >= 2 && lines.len() == 1 && size + 0.4 < start_size {
            size = start_size;
            // Break after the Latin lead (`AI` / `智能体及应用简介`) so a
            // midpoint split does not cut 智能体.
            split_display_title(&lines[0], ink.len())
        } else {
            lines
        }
    } else {
        spread_to_lines(lines, ink.len())
    };
    if hanging.is_some() || reflow || numbered.is_some() {
        // Reflow replaces the source lines. A stray cover glyph at the source
        // size is the mixed-size glitch inside the paragraph.
        absorbed.extend(glyphs.iter().map(|glyph| glyph.id));
    }
    let line_masks = split_superscripts(text, &lines, &mask);
    let mut origin_y = top_y;
    if bilingual {
        let above = top_y + size * 1.2;
        if above + size <= media[3] {
            origin_y = above;
        } else {
            origin_y = top_y - size * 1.2;
        }
    }
    let ys: Vec<f32> = if bilingual {
        (0..lines.len())
            .map(|index| origin_y - index as f32 * leading)
            .collect()
    } else if ink.len() == 1 && lines.len() > 1 {
        // A one-line contents title whose Chinese wraps. Keep the second line
        // above the next heading; there is no second source baseline to share.
        let mut lead = leading.max(size * 0.92);
        if let Some((next, next_size)) = next_line_below(extraction, page, top_y, size, glyphs) {
            // Clear the next heading's ascent, this line's descent, and a
            // 2pt gap. A 0.92em lead sets `天机器人` flush against `1.2`.
            let clearance = next_size * 0.80 + size * 0.20 + 2.0;
            let room = top_y - next - clearance;
            let step = room / (lines.len() - 1) as f32;
            let packed = step.max(size * 0.72);
            if packed < lead {
                lead = packed;
            }
        }
        (0..lines.len())
            .map(|index| top_y - index as f32 * lead)
            .collect()
    } else {
        baselines_for(&ink, lines.len())
    };
    let last_y = ys.iter().copied().fold(origin_y, f32::min);
    if last_y < media[1] - 0.5 || origin_y > media[3] {
        return None;
    }
    let text_color = if let Some(item) = &numbered {
        item.text_color.clone()
    } else if let Some(item) = &bullet {
        item.text_color.clone()
    } else {
        list_run_colors(&ink)
            .map(|(_, text_color)| text_color)
            .unwrap_or_else(|| ink[0].glyphs[0].fill_color.clone())
    };
    let count = lines.len();
    let glyph_ids: Vec<u32> = glyphs.iter().map(|glyph| glyph.id).collect();
    let mut drawn: Vec<Drawn> = lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let limit = if index == 0 { first_width } else { width };
            let justify = index + 1 != count;
            let supers = line_masks.get(index).cloned().unwrap_or_default();
            Drawn {
                page,
                x: block_left + if index == 0 { indent } else { 0.0 },
                y: ys[index],
                size,
                color: text_color.clone(),
                cids: cids_of(&line, font),
                resource: resource.to_string(),
                skew,
                widths: widths_with_superscripts(&line, size, font, &supers, sup.as_ref()),
                gaps: justify_gaps(&line, size, limit, font, justify),
                text: line,
                supers,
                sup_scale: sup.as_ref().map(|item| item.scale).unwrap_or(1.0),
                sup_rise: sup.as_ref().map(|item| item.rise).unwrap_or(0.0),
                glyph_ids: glyph_ids.clone(),
            }
        })
        .collect();
    if let Some(item) = &bullet {
        if let Some(host) = drawn.first() {
            // Same glyph ids as the words, so a later column shift moves the
            // mark with the line. A marker left in the source stream stays on
            // the English baseline after that shift.
            let marker = item.marker.clone();
            let widths = char_widths(&marker, item.marker_size, font);
            let glyph_ids = host.glyph_ids.clone();
            drawn.insert(
                0,
                Drawn {
                    page,
                    x: item.marker_x,
                    y: host.y,
                    size: item.marker_size,
                    color: item.marker_color.clone(),
                    cids: cids_of(&marker, font),
                    resource: resource.to_string(),
                    skew: 0.0,
                    widths,
                    gaps: Vec::new(),
                    text: marker,
                    supers: Vec::new(),
                    sup_scale: 1.0,
                    sup_rise: 0.0,
                    glyph_ids,
                },
            );
        }
    }
    // A title page is right-aligned. Chinese is shorter than the English, so
    // anchoring at the source left leaves a ragged right edge. A contents
    // row shares that right-looking edge only by coincidence (a page number
    // or the end of a long title). Those rows stay at the source left.
    // A justified body column also shares a right edge. Sliding a short body
    // line onto it (`“来自一线的 LLM…` at x=270) opens a hole the column
    // closer cannot cross, because the line no longer shares a left edge.
    if !bilingual && source_size >= 14.0 && !drawn.is_empty() {
        let ink_edge = ink.iter().map(|line| line.right).fold(f32::MIN, f32::max);
        let shared = ink.iter().all(|line| (line.right - ink_edge).abs() <= 2.0);
        let left_min = ink.iter().map(|line| line.left).fold(f32::MAX, f32::min);
        let left_max = ink.iter().map(|line| line.left).fold(f32::MIN, f32::max);
        let ragged_left = ink.len() >= 2 && left_max - left_min > 8.0;
        let page_edge =
            if ink.len() == 1 && drawn.len() == 1 && !contents_baseline(extraction, &ink[0]) {
                shared_right_edge(extraction, &ink[0])
            } else {
                None
            };
        if shared && (ragged_left || page_edge.is_some()) {
            let mut edge = page_edge.unwrap_or(ink_edge);
            if let Some(limit) = numeral_left {
                let clearance = limit - source_size * 0.35;
                if clearance > media[0] + 36.0 {
                    edge = edge.min(clearance);
                }
            }
            for item in &mut drawn {
                let advance = item.widths.iter().sum::<f32>() + item.gaps.iter().sum::<f32>();
                if advance <= 0.0 {
                    continue;
                }
                let x = edge - advance;
                // A title line that still crosses the chapter numeral moves
                // left onto that cleared edge. Other lines only move right,
                // so a short line is not pulled off its column.
                let crosses_numeral = numeral_left.is_some() && item.x + advance > edge + 0.4;
                if (x + 0.4 >= item.x || crosses_numeral) && x >= media[0] + 24.0 {
                    item.x = x;
                }
            }
        }
    }
    if let Some(item) = &numbered {
        if let Some(host) = drawn.first() {
            let marker = item.marker.clone();
            let widths = char_widths(&marker, item.marker_size, font);
            drawn.insert(
                0,
                Drawn {
                    page,
                    x: item.marker_x,
                    y: host.y,
                    size: item.marker_size,
                    color: item.marker_color.clone(),
                    cids: cids_of(&marker, font),
                    resource: resource.to_string(),
                    skew: 0.0,
                    widths,
                    gaps: Vec::new(),
                    text: marker,
                    supers: Vec::new(),
                    sup_scale: 1.0,
                    sup_rise: 0.0,
                    glyph_ids: glyph_ids.clone(),
                },
            );
        }
    }
    let mut drawn = restyle_list_lines(drawn, &ink, font);
    if !bilingual {
        cover_unhit_clusters(
            &mut drawn,
            glyphs,
            font,
            resource,
            skew,
            &text_color,
            &glyph_ids,
            &absorbed,
        );
    }
    Some(drawn)
}

fn column_reflow(
    level: HeadingLevel,
    ink_len: usize,
    width: f32,
    size: f32,
    hanging: bool,
) -> bool {
    !hanging && ink_len >= 2 && matches!(level, HeadingLevel::Body) && width >= size * 16.0
}

/// The bullet keeps its own colour. The words use the source text colour.
/// An italic lead (through the em dash) is a shear; the explanation is roman.
fn restyle_list_lines(lines: Vec<Drawn>, ink: &[InkLine<'_>], font: &SubsetFont) -> Vec<Drawn> {
    let Some((marker_color, text_color)) = list_run_colors(ink) else {
        return lines;
    };
    let italic_lead = italic_lead_before_emdash(ink);
    if marker_color == text_color && !italic_lead {
        return lines;
    }
    let mut out = Vec::new();
    let mut before_emdash = italic_lead;
    for line in lines {
        out.extend(split_styled_line(
            line,
            &marker_color,
            &text_color,
            &mut before_emdash,
            font,
        ));
    }
    out
}

fn list_run_colors(ink: &[InkLine<'_>]) -> Option<(Color, Color)> {
    if ink.is_empty() || !crate::segment::starts_with_list_marker(&ink_line_text(&ink[0])) {
        return None;
    }
    let mut glyphs = ink[0].glyphs.clone();
    glyphs.sort_by(|left, right| left.matrix[4].total_cmp(&right.matrix[4]));
    let mut marker = None;
    let mut body = None;
    let mut skipped = false;
    for glyph in &glyphs {
        let text = glyph.unicode.trim();
        if text.is_empty() {
            continue;
        }
        if !skipped && crate::segment::starts_with_list_marker(text) {
            marker = Some(glyph.fill_color.clone());
            skipped = true;
            continue;
        }
        skipped = true;
        if body.is_none() {
            body = Some(glyph.fill_color.clone());
        }
    }
    if body.is_none() {
        for line in ink.iter().skip(1) {
            for glyph in &line.glyphs {
                if !glyph.unicode.trim().is_empty() {
                    body = Some(glyph.fill_color.clone());
                    break;
                }
            }
            if body.is_some() {
                break;
            }
        }
    }
    Some((
        marker.unwrap_or_else(Color::black),
        body.unwrap_or_else(Color::black),
    ))
}

fn italic_lead_before_emdash(ink: &[InkLine<'_>]) -> bool {
    if ink.is_empty() {
        return false;
    }
    let mut glyphs = ink[0].glyphs.clone();
    glyphs.sort_by(|left, right| left.matrix[4].total_cmp(&right.matrix[4]));
    let mut skipped_marker = false;
    let mut italic = 0usize;
    let mut roman = 0usize;
    for glyph in &glyphs {
        let text = glyph.unicode.trim();
        if text.is_empty() {
            continue;
        }
        if !skipped_marker && crate::segment::starts_with_list_marker(text) {
            skipped_marker = true;
            continue;
        }
        skipped_marker = true;
        if text.chars().any(|ch| ch == '\u{2014}' || ch == '\u{2015}') {
            break;
        }
        if text.chars().any(|ch| ch.is_ascii_alphabetic()) {
            if face_style(&glyph.font_name).italic {
                italic += 1;
            } else {
                roman += 1;
            }
        }
    }
    italic > 0 && italic >= roman
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PieceKind {
    Marker,
    Lead,
    Body,
}

fn is_bullet_char(ch: char) -> bool {
    matches!(
        ch,
        '-' | '–' | '•' | '●' | '◦' | '▪' | '■' | '‣' | '∙' | '·' | '‧'
    ) || ('\u{E000}'..='\u{F8FF}').contains(&ch)
}

/// A dingbat, not a dash that opens a sentence (`——本部分`).
fn is_graphic_bullet(ch: char) -> bool {
    matches!(ch, '•' | '●' | '◦' | '▪' | '■' | '‣' | '∙' | '·' | '‧')
        || ('\u{E000}'..='\u{F8FF}').contains(&ch)
}

fn lone_bullet_mark(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(is_graphic_bullet) && chars.next().is_none()
}

fn split_styled_line(
    line: Drawn,
    marker_color: &Color,
    text_color: &Color,
    before_emdash: &mut bool,
    font: &SubsetFont,
) -> Vec<Drawn> {
    let chars: Vec<char> = line.text.chars().collect();
    if chars.is_empty() {
        return vec![line];
    }
    // An em dash ends the italic lead. It is not the gold bullet.
    let opens = chars.first().copied().is_some_and(is_bullet_char);
    let mut kinds = Vec::with_capacity(chars.len());
    for (index, ch) in chars.iter().enumerate() {
        if index == 0 && opens {
            kinds.push(PieceKind::Marker);
            continue;
        }
        if *before_emdash && *ch != '\u{2014}' && *ch != '\u{2015}' {
            kinds.push(PieceKind::Lead);
        } else {
            *before_emdash = false;
            kinds.push(PieceKind::Body);
        }
    }
    let widths = if line.widths.len() == chars.len() {
        line.widths.clone()
    } else {
        char_widths(&line.text, line.size, font)
    };
    let mut pieces = Vec::new();
    let mut cursor = line.x;
    let mut start = 0usize;
    while start < chars.len() {
        let kind = kinds[start];
        let mut end = start + 1;
        while end < chars.len() && kinds[end] == kind {
            end += 1;
        }
        let text: String = chars[start..end].iter().collect();
        let piece_widths = widths[start..end].to_vec();
        let piece_gaps: Vec<f32> = if end > start + 1 {
            (start..end - 1)
                .map(|index| line.gaps.get(index).copied().unwrap_or(0.0))
                .collect()
        } else {
            Vec::new()
        };
        let bridge = if end < chars.len() {
            line.gaps.get(end - 1).copied().unwrap_or(0.0)
        } else {
            0.0
        };
        let cids = if line.cids.len() == chars.len() {
            line.cids[start..end].to_vec()
        } else {
            cids_of(&text, font)
        };
        let supers = if line.supers.len() == chars.len() {
            line.supers[start..end].to_vec()
        } else {
            Vec::new()
        };
        let (color, skew) = match kind {
            PieceKind::Marker => (marker_color.clone(), 0.0),
            PieceKind::Lead => (text_color.clone(), 0.25),
            PieceKind::Body => (text_color.clone(), 0.0),
        };
        let advance = piece_widths.iter().sum::<f32>() + piece_gaps.iter().sum::<f32>() + bridge;
        pieces.push(Drawn {
            page: line.page,
            x: cursor,
            y: line.y,
            size: line.size,
            color,
            cids,
            resource: line.resource.clone(),
            skew,
            widths: piece_widths,
            gaps: piece_gaps,
            text,
            supers,
            sup_scale: line.sup_scale,
            sup_rise: line.sup_rise,
            glyph_ids: line.glyph_ids.clone(),
        });
        cursor += advance;
        start = end;
    }
    pieces
}

struct ColumnBlock {
    indices: Vec<usize>,
    top: f32,
    bottom: f32,
    left: f32,
    source_top: f32,
    source_bottom: f32,
    size: f32,
}

/// A contents title shares its baseline with a kept page number. Body text
/// does not, so closing a hole in a paragraph still runs.
fn block_is_contents_row(
    block: &ColumnBlock,
    drawn: &[Drawn],
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) -> bool {
    let page = drawn[block.indices[0]].page;
    let size = block.size.max(1.0);
    let mut ids = Vec::new();
    for index in &block.indices {
        ids.extend(drawn[*index].glyph_ids.iter().copied());
    }
    if ids.iter().any(|id| {
        by_id.get(id).and_then(|index| extraction.glyphs.get(*index)).is_some_and(|glyph| {
            matches!(&glyph.disposition, Disposition::KeptOriginal { reason } if reason == "toc")
        })
    }) {
        return true;
    }
    if extraction.glyphs.iter().any(|glyph| {
        glyph.page_index == page
            && (glyph.matrix[5] - block.source_top).abs() <= size * 0.8
            && matches!(&glyph.disposition, Disposition::KeptOriginal { reason } if reason == "toc")
    }) {
        return true;
    }
    // `12.3 Handling…` and `Productionizing…` have no page number on the
    // first line (it sits on the wrap). A toc glyph within 0.8em of
    // `source_top` misses them, and hole-closing then pulls the heading
    // by a second leading on top of `apply_subentry_shifts`.
    let Some((low, high)) = toc_vertical_span(extraction, page) else {
        return false;
    };
    let pad = size * 3.2;
    block.source_bottom <= high + pad && block.source_top >= low - pad
}

/// Pages that carry several kept contents marks. Detection is the marks,
/// not a fixed page range.
fn is_toc_mark_glyph(glyph: &Glyph) -> bool {
    !glyph.unicode.trim().is_empty()
        && matches!(&glyph.disposition, Disposition::KeptOriginal { reason } if reason == "toc")
}

fn contents_page_indexes(extraction: &Extraction) -> Vec<u32> {
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for glyph in &extraction.glyphs {
        if is_toc_mark_glyph(glyph) {
            *counts.entry(glyph.page_index).or_default() += 1;
        }
    }
    let mut pages: Vec<u32> = counts
        .into_iter()
        .filter(|(_, count)| *count >= 3)
        .map(|(page, _)| page)
        .collect();
    pages.sort_unstable();
    pages
}

fn toc_vertical_span(extraction: &Extraction, page: u32) -> Option<(f32, f32)> {
    let mut low = f32::MAX;
    let mut high = f32::MIN;
    let mut count = 0usize;
    for glyph in &extraction.glyphs {
        if glyph.page_index != page || !is_toc_mark_glyph(glyph) {
            continue;
        }
        count += 1;
        low = low.min(glyph.matrix[5]);
        high = high.max(glyph.matrix[5]);
    }
    (count >= 3).then_some((low, high))
}

/// A taller contents block can push the last row into the footer. Scale that
/// page's stack from its top so the leading stays even and rows still clear
/// one another. A page that already fits is left on the source baselines.
fn fit_contents_stack(drawn: &mut [Drawn], extraction: &Extraction) {
    for page in contents_page_indexes(extraction) {
        let media = extraction
            .pages
            .iter()
            .find(|info| info.index == page)
            .map(|info| info.media_box)
            .unwrap_or([0.0, 0.0, 612.0, 792.0]);
        let floor = media[1] + 32.0;
        let ceiling = media[3] - 28.0;
        let rows: Vec<usize> = drawn
            .iter()
            .enumerate()
            .filter(|(_, line)| {
                line.page == page
                    && !line.text.trim().is_empty()
                    && line.y >= floor
                    && line.y <= ceiling
            })
            .map(|(index, _)| index)
            .collect();
        if rows.len() < 4 {
            continue;
        }
        let top = rows
            .iter()
            .map(|index| drawn[*index].y)
            .fold(f32::MIN, f32::max);
        let bottom = rows
            .iter()
            .map(|index| {
                let line = &drawn[*index];
                line.y - line.size * 0.25
            })
            .fold(f32::MAX, f32::min);
        if bottom >= floor {
            continue;
        }
        let span = (top - bottom).max(1.0);
        let room = (top - floor).max(1.0);
        let scale = (room / span).clamp(0.72, 1.0);
        if scale > 0.995 {
            continue;
        }
        for index in rows {
            let line = &mut drawn[index];
            line.y = top - (top - line.y) * scale;
            line.size *= scale;
            for width in &mut line.widths {
                *width *= scale;
            }
            for gap in &mut line.gaps {
                *gap *= scale;
            }
        }
    }
}

fn draw_ink_right(item: &Drawn) -> f32 {
    item.x + item.widths.iter().sum::<f32>() + item.gaps.iter().sum::<f32>()
}

fn is_section_atom(item: &Drawn) -> bool {
    let text = item.text.trim();
    text.chars().count() == 1 && text.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
}

/// `1`, `.`, `4`, `.`, `1` are separate echo draws. Join them so the section
/// index is one block and moves with its title.
fn coalesce_section_indexes(drawn: &mut Vec<Drawn>) {
    let mut order: Vec<usize> = (0..drawn.len())
        .filter(|&index| is_section_atom(&drawn[index]))
        .collect();
    order.sort_by(|&left, &right| {
        drawn[left]
            .page
            .cmp(&drawn[right].page)
            .then_with(|| drawn[right].y.total_cmp(&drawn[left].y))
            .then_with(|| drawn[left].x.total_cmp(&drawn[right].x))
    });
    let mut drop = Vec::new();
    let mut merged = Vec::new();
    let mut cursor = 0;
    while cursor < order.len() {
        let mut run = vec![order[cursor]];
        let mut next = cursor + 1;
        while next < order.len() {
            let prev = &drawn[*run.last().unwrap()];
            let item = &drawn[order[next]];
            if item.page != prev.page || (item.y - prev.y).abs() > 0.8 {
                break;
            }
            // Echoed digits are wider than the source advance, so the next
            // atom can start inside the previous ink (`1.` ends at 66 and
            // the following `1` starts at 65). They are still one index
            // when each origin steps forward by less than an em.
            let step = item.x - prev.x;
            let gap = item.x - draw_ink_right(prev);
            if step < prev.size * 0.15 || step > prev.size * 1.6 || gap > prev.size * 1.2 {
                break;
            }
            run.push(order[next]);
            next += 1;
        }
        let text: String = run.iter().map(|&index| drawn[index].text.trim()).collect();
        let first = &drawn[run[0]];
        let heading = is_section_index_text(&text)
            && drawn.iter().any(|item| {
                item.page == first.page
                    && (item.y - first.y).abs() < 1.6
                    && item.x > first.x + first.size
                    && item.text.chars().any(is_han_char)
            });
        if heading {
            let mut widths = Vec::new();
            let mut gaps = Vec::new();
            let mut cids = Vec::new();
            let mut ids = Vec::new();
            for (step, &index) in run.iter().enumerate() {
                let item = &drawn[index];
                if step > 0 {
                    let bridge = (item.x - draw_ink_right(&drawn[run[step - 1]])).max(0.0);
                    if let Some(last) = gaps.last_mut() {
                        *last += bridge;
                    } else {
                        gaps.push(bridge);
                    }
                }
                widths.extend(item.widths.iter().copied());
                gaps.extend(item.gaps.iter().copied());
                cids.extend(item.cids.iter().copied());
                ids.extend(item.glyph_ids.iter().copied());
            }
            let mut item = Drawn {
                page: first.page,
                x: first.x,
                y: first.y,
                size: first.size,
                color: first.color.clone(),
                cids,
                resource: first.resource.clone(),
                skew: first.skew,
                text,
                glyph_ids: ids,
                widths,
                gaps,
                supers: Vec::new(),
                sup_scale: first.sup_scale,
                sup_rise: first.sup_rise,
            };
            if item.gaps.len() + 1 != item.widths.len() && item.widths.len() > 1 {
                item.gaps = vec![0.0; item.widths.len() - 1];
            }
            drop.extend(run.iter().copied());
            merged.push(item);
        }
        cursor = next;
    }
    if drop.is_empty() {
        return;
    }
    let mut kept = Vec::new();
    for (index, item) in drawn.drain(..).enumerate() {
        if !drop.contains(&index) {
            kept.push(item);
        }
    }
    kept.extend(merged);
    *drawn = kept;
}

/// A shorter translation used to leave the next item on its English baseline,
/// which opened a hole. Pull the next same-column block up until the gap
/// matches the source gap. A footer, whose source gap is larger, stays put.
/// A line the model left in Latin (`GoDaddy” (https://…)`) is not a drawn
/// block, so the column closer stops and the quote sits a line below the
/// title that moved up. Redraw that one body line. Section numbers, folios,
/// and short code tokens stay where they are.
fn adopt_unchanged_body_lines(
    drawn: &mut Vec<Drawn>,
    succeeded: &mut HashSet<u32>,
    keep: &mut HashMap<u32, String>,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
    embedded: &[(String, SubsetFont)],
    owners: &HashMap<(u32, u16, usize, usize), Vec<u32>>,
) {
    let already: HashSet<u32> = drawn
        .iter()
        .flat_map(|item| item.glyph_ids.iter().copied())
        .collect();
    let mut ids: Vec<u32> = keep
        .iter()
        .filter(|(id, reason)| reason.as_str() == "unchanged" && !already.contains(id))
        .map(|(id, _)| *id)
        .collect();
    ids.sort_unstable();
    let mut lines: Vec<Vec<usize>> = Vec::new();
    for id in ids {
        let Some(&index) = by_id.get(&id) else {
            continue;
        };
        let glyph = &extraction.glyphs[index];
        let y = glyph.matrix[5];
        if let Some(line) = lines.iter_mut().find(|line| {
            let other = &extraction.glyphs[line[0]];
            other.page_index == glyph.page_index && (other.matrix[5] - y).abs() < 1.2
        }) {
            line.push(index);
        } else {
            lines.push(vec![index]);
        }
    }
    for mut line in lines {
        line.sort_by(|&left, &right| {
            extraction.glyphs[left].matrix[4].total_cmp(&extraction.glyphs[right].matrix[4])
        });
        let text: String = line
            .iter()
            .map(|&index| extraction.glyphs[index].unicode.as_str())
            .collect();
        let trimmed = text.trim();
        if trimmed.chars().count() < 12
            || is_section_index_text(trimmed)
            || bare_page_label(trimmed)
            || english_body_source(trimmed)
        {
            continue;
        }
        let sample = &extraction.glyphs[line[0]];
        let page = sample.page_index;
        let page_has_han = drawn
            .iter()
            .any(|item| item.page == page && item.text.chars().any(is_han_char));
        if !page_has_han {
            continue;
        }
        let (media_bottom, media_top) = extraction
            .pages
            .iter()
            .find(|info| info.index == page)
            .map(|info| (info.media_box[1], info.media_box[3]))
            .unwrap_or((0.0, 800.0));
        let y = sample.matrix[5];
        if y > media_top - 48.0 || y < media_bottom + 36.0 {
            continue;
        }
        let glyphs: Vec<&Glyph> = line
            .iter()
            .map(|&index| &extraction.glyphs[index])
            .collect();
        if !operators_are_private(&glyphs, owners) {
            continue;
        }
        let Some((resource, font)) = embedded.iter().find(|(_, font)| {
            text.chars()
                .all(|ch| ch.is_whitespace() || font.glyphs.contains_key(&(ch as u32)))
        }) else {
            continue;
        };
        let size = glyphs
            .iter()
            .map(|glyph| glyph.font_size)
            .fold(1.0f32, f32::max);
        let skew = if glyphs
            .iter()
            .any(|glyph| face_style(&glyph.font_name).italic)
        {
            0.25
        } else {
            0.0
        };
        let glyph_ids: Vec<u32> = glyphs.iter().map(|glyph| glyph.id).collect();
        for id in &glyph_ids {
            keep.remove(id);
            succeeded.insert(*id);
        }
        drawn.push(Drawn {
            page,
            x: sample.matrix[4],
            y,
            size,
            color: sample.fill_color.clone(),
            cids: cids_of(&text, font),
            resource: resource.clone(),
            skew,
            widths: char_widths(&text, size, font),
            gaps: vec![0.0; text.chars().count().saturating_sub(1)],
            text,
            supers: Vec::new(),
            sup_scale: 1.0,
            sup_rise: 0.0,
            glyph_ids,
        });
    }
}

fn close_column_holes(drawn: &mut [Drawn], extraction: &Extraction, by_id: &HashMap<u32, usize>) {
    let mut pages: Vec<u32> = drawn.iter().map(|line| line.page).collect();
    pages.sort_unstable();
    pages.dedup();
    for page in pages {
        let mut groups: HashMap<Vec<u32>, Vec<usize>> = HashMap::new();
        for (index, line) in drawn.iter().enumerate() {
            if line.page != page || line.text.trim().is_empty() {
                continue;
            }
            let mut key = line.glyph_ids.clone();
            key.sort_unstable();
            key.dedup();
            if key.is_empty() {
                continue;
            }
            groups.entry(key).or_default().push(index);
        }
        let mut blocks = Vec::new();
        for indices in groups.into_values() {
            if let Some(block) = column_block(drawn, &indices, page, extraction, by_id) {
                blocks.push(block);
            }
        }
        // `1.3` and `Typical LLM use cases` are two draws of one row. The
        // index is short, so closing the hole above it used to lift `1.3`
        // onto the previous line and leave the title behind.
        merge_section_heading_blocks(&mut blocks, drawn);
        blocks.sort_by(|left, right| right.top.total_cmp(&left.top));
        let mut shift = 0.0f32;
        let mut prev: Option<ColumnBlock> = None;
        for block in blocks {
            // A column page number sits to the right of its title. It is not
            // the next paragraph, and treating it as one used to drop the
            // pull before the following section. Leader dots are the same
            // row: a hanging bullet changes the block above by a few points
            // and used to chain the dots onto that block, leaving `171`
            // behind. `seat_inline_page_numbers` moves both with the title.
            let page_number = block
                .indices
                .iter()
                .all(|index| draw_is_bare_page_number(&drawn[*index], extraction, by_id));
            let leaders = block
                .indices
                .iter()
                .all(|index| draw_is_leader(&drawn[*index]));
            if page_number || leaders || block_is_contents_row(&block, drawn, extraction, by_id) {
                // Contents rows keep the source baseline. A taller or shorter
                // sub-entry block moves them in `apply_subentry_shifts`, by
                // one source leading, instead of closing each hole differently.
                if !page_number && !leaders {
                    shift = 0.0;
                    prev = Some(block);
                }
                continue;
            }
            let mut extra = 0.0f32;
            if let Some(prev) = &prev {
                let source_gap = prev.source_bottom - block.source_top;
                let size = prev.size.max(block.size).max(1.0);
                let same_column = (prev.left - block.left).abs() < size * 4.0;
                // A shorter paragraph used to leave the next block on the
                // English baseline. Same-baseline marks (a page number beside
                // its title) have no positive source gap, so they are not pulled.
                let chained = same_column && source_gap > 0.0 && source_gap < size * 2.6;
                if chained {
                    // `shift` is the pull already applied above this block.
                    // Only a hole that is still open after that pull is closed.
                    let drawn_gap = prev.bottom - (block.top + shift);
                    if drawn_gap > source_gap + 1.0 {
                        extra = drawn_gap - source_gap;
                    }
                } else {
                    shift = 0.0;
                }
            }
            shift += extra;
            for index in &block.indices {
                drawn[*index].y += shift;
            }
            let mut placed = block;
            placed.top += shift;
            placed.bottom += shift;
            prev = Some(placed);
        }
    }
}

fn merge_section_heading_blocks(blocks: &mut Vec<ColumnBlock>, drawn: &[Drawn]) {
    let mut drop = Vec::new();
    for index in 0..blocks.len() {
        if drop.contains(&index) || !block_is_section_index(&blocks[index], drawn) {
            continue;
        }
        let Some(mate) = (0..blocks.len()).find(|&other| {
            other != index
                && !drop.contains(&other)
                && !block_is_section_index(&blocks[other], drawn)
                && !block_is_page_or_leaders(&blocks[other], drawn)
                && (blocks[other].source_top - blocks[index].source_top).abs() < 1.6
                && blocks[other].left + 1.0 >= blocks[index].left
        }) else {
            continue;
        };
        let other = &blocks[mate];
        let other_indices = other.indices.clone();
        let other_top = other.top;
        let other_bottom = other.bottom;
        let other_left = other.left;
        let other_source_top = other.source_top;
        let other_source_bottom = other.source_bottom;
        let other_size = other.size;
        let block = &mut blocks[index];
        block.indices.extend(other_indices);
        block.top = block.top.max(other_top);
        block.bottom = block.bottom.min(other_bottom);
        block.left = block.left.min(other_left);
        block.source_top = block.source_top.max(other_source_top);
        block.source_bottom = block.source_bottom.min(other_source_bottom);
        block.size = block.size.max(other_size);
        drop.push(mate);
    }
    if drop.is_empty() {
        return;
    }
    let mut kept = Vec::new();
    for (index, block) in blocks.drain(..).enumerate() {
        if !drop.contains(&index) {
            kept.push(block);
        }
    }
    *blocks = kept;
}

fn block_is_section_index(block: &ColumnBlock, drawn: &[Drawn]) -> bool {
    block
        .indices
        .iter()
        .any(|index| is_section_index_text(&drawn[*index].text))
        && block.indices.iter().all(|index| {
            let text = drawn[*index].text.trim();
            text.is_empty() || text == "■" || is_section_index_text(text)
        })
}

/// `1.4.2` sits in the margin and `RAG` is the title. The number is not
/// part of the title segment, so the title's own ink is only three letters.
fn index_sits_left_of(extraction: &Extraction, line: &InkLine<'_>) -> bool {
    let Some(sample) = line.glyphs.first() else {
        return false;
    };
    let size = crate::segment::representative_size(&line.glyphs).max(1.0);
    let mut marks: Vec<&Glyph> = extraction
        .glyphs
        .iter()
        .filter(|glyph| {
            let token = glyph.unicode.trim();
            !token.is_empty()
                && glyph.page_index == sample.page_index
                && (glyph.matrix[5] - line.y).abs() <= size * 0.45
                && glyph_ink_right(glyph) < line.left - size * 0.15
                && token.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
        })
        .collect();
    if marks.is_empty() {
        return false;
    }
    marks.sort_by(|left, right| glyph_ink_left(left).total_cmp(&glyph_ink_left(right)));
    let text: String = marks.iter().map(|glyph| glyph.unicode.trim()).collect();
    is_section_index_text(text.trim())
}

fn is_section_index_text(text: &str) -> bool {
    let text = text.trim();
    text.contains('.')
        && text.chars().any(|ch| ch.is_ascii_digit())
        && text.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
}

fn block_is_page_or_leaders(block: &ColumnBlock, drawn: &[Drawn]) -> bool {
    !block.indices.is_empty()
        && block.indices.iter().all(|index| {
            let text = drawn[*index].text.trim();
            bare_page_label(text) || draw_is_leader(&drawn[*index])
        })
}

fn column_block(
    drawn: &[Drawn],
    indices: &[usize],
    page: u32,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) -> Option<ColumnBlock> {
    if indices.is_empty() {
        return None;
    }
    let mut top = f32::MIN;
    let mut bottom = f32::MAX;
    let mut left = f32::MAX;
    let mut size = 1.0f32;
    for index in indices {
        let line = &drawn[*index];
        top = top.max(line.y);
        bottom = bottom.min(line.y);
        // A continuation marker hangs in the gutter. It is not the column edge.
        if !lone_bullet_mark(&line.text) {
            left = left.min(line.x);
        }
        size = size.max(line.size);
    }
    if left == f32::MAX {
        left = drawn[indices[0]].x;
    }
    let mut source_ys = Vec::new();
    for id in &drawn[indices[0]].glyph_ids {
        let Some(&index) = by_id.get(id) else {
            continue;
        };
        let glyph = &extraction.glyphs[index];
        if glyph.page_index != page || glyph.unicode.trim().is_empty() {
            continue;
        }
        source_ys.push(glyph.matrix[5]);
    }
    if source_ys.is_empty() {
        return None;
    }
    source_ys.sort_by(|left, right| left.total_cmp(right));
    Some(ColumnBlock {
        indices: indices.to_vec(),
        top,
        bottom,
        left,
        source_top: source_ys[source_ys.len() - 1],
        source_bottom: source_ys[0],
        size,
    })
}

fn drawn_ink_right(item: &Drawn) -> f32 {
    item.x + item.widths.iter().sum::<f32>() + item.gaps.iter().sum::<f32>()
}

/// A page number left where the English title ended, then left behind when
/// its title moved up to close a hole. Put it on the title's last line, one
/// gap after the words. A leader-dot row keeps its source x and only follows
/// the title vertically. Front matter, chapters, and sections all sit inline;
/// the source does not park those numbers on a right edge.
fn seat_inline_page_numbers(
    drawn: &mut [Drawn],
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) {
    let targets: Vec<usize> = drawn
        .iter()
        .enumerate()
        .filter(|(_, item)| draw_is_bare_page_number(item, extraction, by_id))
        .map(|(index, _)| index)
        .collect();
    let mut seats = Vec::new();
    let mut leader_shifts = Vec::new();
    for index in targets {
        let Some((source_y, source_left)) = bare_number_source(&drawn[index], extraction, by_id)
        else {
            continue;
        };
        let page = drawn[index].page;
        let size = drawn[index].size.max(1.0);
        let Some((title_ids, title_source_right)) =
            title_before_number(drawn, extraction, by_id, page, source_y, source_left)
        else {
            continue;
        };
        let Some(title_index) = lowest_title_line(drawn, &title_ids, page) else {
            continue;
        };
        let title = &drawn[title_index];
        let title_right =
            title.x + title.widths.iter().sum::<f32>() + title.gaps.iter().sum::<f32>();
        let title_y = title.y;
        let gap = source_left - title_source_right;
        let leaders = leaders_between(extraction, page, source_y, title_source_right, source_left);
        let inline = gap >= -1.0 && gap < size * 3.0 && !leaders;
        let x = if inline {
            title_right + page_number_gap(size, false)
        } else {
            drawn[index].x
        };
        let old_y = drawn[index].y;
        seats.push((index, x, title_y));
        if !inline && (title_y - old_y).abs() > 0.4 {
            let dy = title_y - old_y;
            for (leader_index, item) in drawn.iter().enumerate() {
                if item.page != page || !draw_is_leader(item) {
                    continue;
                }
                let on_row = item.glyph_ids.iter().any(|id| {
                    by_id.get(id).is_some_and(|glyph_index| {
                        let glyph = &extraction.glyphs[*glyph_index];
                        glyph.page_index == page && (glyph.matrix[5] - source_y).abs() <= 1.6
                    })
                });
                if on_row {
                    leader_shifts.push((leader_index, dy));
                }
            }
        }
    }
    for (index, x, y) in seats {
        drawn[index].x = x;
        drawn[index].y = y;
    }
    for (index, dy) in leader_shifts {
        drawn[index].y += dy;
    }
}

fn draw_is_bare_page_number(
    item: &Drawn,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) -> bool {
    if !bare_page_label(&item.text) {
        return false;
    }
    !item.glyph_ids.is_empty()
        && item.glyph_ids.iter().all(|id| {
            by_id.get(id).is_some_and(|index| {
                let token = extraction.glyphs[*index].unicode.trim();
                token.is_empty() || bare_page_label(token) || toc_page_char(token)
            })
        })
}

fn bare_number_source(
    item: &Drawn,
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
) -> Option<(f32, f32)> {
    let mut y = f32::MIN;
    let mut left = f32::MAX;
    let mut saw = false;
    for id in &item.glyph_ids {
        let Some(&index) = by_id.get(id) else {
            continue;
        };
        let glyph = &extraction.glyphs[index];
        if glyph.page_index != item.page || glyph.unicode.trim().is_empty() {
            continue;
        }
        saw = true;
        y = y.max(glyph.matrix[5]);
        left = left.min(glyph_ink_left(glyph));
    }
    saw.then_some((y, left))
}

/// The title whose source ink ends closest to this page number, still on its
/// left. A row that continues past the number (`engines 4 chatbots 7`) is not
/// that title.
fn title_before_number(
    drawn: &[Drawn],
    extraction: &Extraction,
    by_id: &HashMap<u32, usize>,
    page: u32,
    source_y: f32,
    number_left: f32,
) -> Option<(Vec<u32>, f32)> {
    let mut best: Option<(Vec<u32>, f32)> = None;
    for item in drawn {
        if item.page != page
            || item.text.trim().is_empty()
            || draw_is_bare_page_number(item, extraction, by_id)
            // Dots end just before the number, closer than the title. They
            // are not the row the number should follow.
            || draw_is_leader(item)
        {
            continue;
        }
        let glyphs = source_glyphs_on_baseline(&item.glyph_ids, extraction, by_id, page, source_y);
        if glyphs.is_empty()
            || glyphs
                .iter()
                .any(|glyph| glyph_ink_left(glyph) > number_left + 0.5)
        {
            continue;
        }
        let right = glyphs
            .iter()
            .map(|glyph| glyph_ink_right(glyph))
            .fold(0.0f32, f32::max);
        if right > number_left + 1.0 {
            continue;
        }
        let closer = best
            .as_ref()
            .is_none_or(|(_, best_right)| right > *best_right + 0.4);
        if closer {
            best = Some((item.glyph_ids.clone(), right));
        }
    }
    best
}

fn source_glyphs_on_baseline<'a>(
    ids: &[u32],
    extraction: &'a Extraction,
    by_id: &HashMap<u32, usize>,
    page: u32,
    y: f32,
) -> Vec<&'a Glyph> {
    let mut glyphs = Vec::new();
    for id in ids {
        let Some(&index) = by_id.get(id) else {
            continue;
        };
        let glyph = &extraction.glyphs[index];
        if glyph.page_index != page || glyph.unicode.trim().is_empty() {
            continue;
        }
        if (glyph.matrix[5] - y).abs() <= 1.6 {
            glyphs.push(glyph);
        }
    }
    glyphs
}

fn lowest_title_line(drawn: &[Drawn], ids: &[u32], page: u32) -> Option<usize> {
    drawn
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item.page == page
                && same_glyph_ids(&item.glyph_ids, ids)
                && !item.text.trim().is_empty()
                && !item.text.trim().chars().all(|ch| ch.is_ascii_digit())
        })
        .min_by(|(_, left), (_, right)| left.y.total_cmp(&right.y))
        .map(|(index, _)| index)
}

fn same_glyph_ids(left: &[u32], right: &[u32]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut left = left.to_vec();
    let mut right = right.to_vec();
    left.sort_unstable();
    right.sort_unstable();
    left == right
}

fn draw_is_leader(item: &Drawn) -> bool {
    let text = item.text.trim();
    !text.is_empty()
        && text
            .chars()
            .all(|ch| matches!(ch, '.' | '·' | '…' | '•' | '⋅' | '‧' | '․'))
}

fn leaders_between(extraction: &Extraction, page: u32, y: f32, left: f32, right: f32) -> bool {
    extraction.glyphs.iter().any(|glyph| {
        glyph.page_index == page
            && (glyph.matrix[5] - y).abs() <= 1.6
            && is_toc_leader_glyph(glyph)
            && glyph_ink_left(glyph) + 0.5 >= left
            && glyph_ink_right(glyph) <= right + 1.0
    })
}

/// Every source baseline gets a drawn line when there are enough pieces.
/// Extra pieces sit in the largest gaps so a longer translation still covers
/// the English lines instead of walking off the bottom of the paragraph.
fn baselines_for(ink: &[InkLine<'_>], count: usize) -> Vec<f32> {
    let src: Vec<f32> = ink.iter().map(|line| line.y).collect();
    if src.is_empty() || count == 0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![src[0]];
    }
    if count == src.len() {
        return src;
    }
    if count < src.len() {
        return src.into_iter().take(count).collect();
    }
    let extra = count - src.len();
    let mut weight: Vec<f32> = src
        .windows(2)
        .map(|pair| (pair[0] - pair[1]).abs())
        .collect();
    let mut inserts = vec![0usize; src.len() - 1];
    for _ in 0..extra {
        let slot = weight
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .map(|(index, _)| index)
            .unwrap_or(0);
        inserts[slot] += 1;
        weight[slot] = (src[slot] - src[slot + 1]).abs() / (inserts[slot] + 1) as f32;
    }
    let mut ys = Vec::with_capacity(count);
    for index in 0..src.len() {
        ys.push(src[index]);
        if index + 1 == src.len() {
            break;
        }
        let added = inserts[index];
        for step in 1..=added {
            let t = step as f32 / (added + 1) as f32;
            ys.push(src[index] + (src[index + 1] - src[index]) * t);
        }
    }
    ys
}

fn cover_unhit_clusters(
    drawn: &mut Vec<Drawn>,
    glyphs: &[&Glyph],
    font: &SubsetFont,
    resource: &str,
    skew: f32,
    color: &crate::color::Color,
    glyph_ids: &[u32],
    skip: &[u32],
) {
    if glyphs.is_empty() {
        return;
    }
    for cluster in marker_clusters(glyphs) {
        if cluster.iter().all(|glyph| skip.contains(&glyph.id)) {
            continue;
        }
        let page = cluster[0].page_index;
        let bbox = cluster_bbox(&cluster);
        if drawn_hits(drawn, font, page, bbox) {
            continue;
        }
        let (x, y, size, text) = if cluster_width(&cluster) <= 24.0 {
            let text = cluster_source(&cluster, font);
            if text.is_empty() {
                continue;
            }
            let y = cluster.iter().map(|glyph| glyph.matrix[5]).sum::<f32>() / cluster.len() as f32;
            let x = cluster
                .iter()
                .map(|glyph| glyph.matrix[4].min(glyph.bbox[0]).min(glyph.bbox[2]))
                .fold(f32::MAX, f32::min);
            let size = cluster
                .iter()
                .map(|glyph| glyph.font_size)
                .fold(1.0f32, f32::max)
                .max(1.0);
            (x, y, size, text)
        } else {
            let Some(ch) = drawn.iter().rev().find_map(|line| {
                line.text
                    .chars()
                    .rev()
                    .find(|ch| !ch.is_whitespace() && font.glyphs.contains_key(&(*ch as u32)))
            }) else {
                continue;
            };
            let y = cluster.iter().map(|glyph| glyph.matrix[5]).sum::<f32>() / cluster.len() as f32;
            let x = cluster
                .iter()
                .map(|glyph| glyph.matrix[4].min(glyph.bbox[0]).min(glyph.bbox[2]))
                .fold(f32::MAX, f32::min);
            let size = cluster
                .iter()
                .map(|glyph| glyph.font_size)
                .fold(1.0f32, f32::max)
                .max(1.0);
            (x, y, size, ch.to_string())
        };
        drawn.push(Drawn {
            page,
            x,
            y,
            size,
            color: color.clone(),
            cids: cids_of(&text, font),
            resource: resource.to_string(),
            skew,
            widths: char_widths(&text, size, font),
            gaps: vec![0.0; text.chars().count().saturating_sub(1)],
            text,
            supers: Vec::new(),
            sup_scale: 1.0,
            sup_rise: 0.0,
            glyph_ids: glyph_ids.to_vec(),
        });
    }
}

/// The same grouping the drop-rate scorer uses: a glyph more than 24pt to the
/// right is its own line, so a spaced affiliation mark is not covered by text
/// that starts at the left of the author line.
fn marker_clusters<'a>(glyphs: &[&'a Glyph]) -> Vec<Vec<&'a Glyph>> {
    let mut ordered: Vec<&Glyph> = glyphs.to_vec();
    ordered.sort_by(|left, right| {
        right.matrix[5]
            .total_cmp(&left.matrix[5])
            .then(left.matrix[4].total_cmp(&right.matrix[4]))
    });
    let mut clusters: Vec<Vec<&Glyph>> = Vec::new();
    for glyph in ordered {
        let attach = clusters.last().is_some_and(|cluster| {
            let right = cluster
                .iter()
                .map(|item| item.bbox[0].max(item.bbox[2]))
                .fold(f32::MIN, f32::max);
            let left = glyph.bbox[0].min(glyph.bbox[2]);
            let gap = left - right;
            let size = cluster
                .last()
                .map(|item| item.font_size)
                .unwrap_or(10.0)
                .max(1.0);
            let anchor = cluster
                .last()
                .map(|item| item.matrix[5])
                .unwrap_or(glyph.matrix[5]);
            glyph.page_index == cluster[0].page_index
                && (anchor - glyph.matrix[5]).abs() <= 2.0
                && (-size..=24.0).contains(&gap)
        });
        if attach {
            clusters.last_mut().unwrap().push(glyph);
        } else {
            clusters.push(vec![glyph]);
        }
    }
    clusters
}

fn cluster_bbox(cluster: &[&Glyph]) -> [f32; 4] {
    let mut bbox = cluster[0].bbox;
    for glyph in cluster.iter().skip(1) {
        let x0 = bbox[0].min(bbox[2]).min(glyph.bbox[0]).min(glyph.bbox[2]);
        let y0 = bbox[1].min(bbox[3]).min(glyph.bbox[1]).min(glyph.bbox[3]);
        let x1 = bbox[0].max(bbox[2]).max(glyph.bbox[0]).max(glyph.bbox[2]);
        let y1 = bbox[1].max(bbox[3]).max(glyph.bbox[1]).max(glyph.bbox[3]);
        bbox = [x0, y0, x1, y1];
    }
    bbox
}

fn cluster_width(cluster: &[&Glyph]) -> f32 {
    let bbox = cluster_bbox(cluster);
    bbox[0].max(bbox[2]) - bbox[0].min(bbox[2])
}

fn cluster_source(cluster: &[&Glyph], font: &SubsetFont) -> String {
    let mut text = String::new();
    for glyph in cluster {
        for ch in glyph.unicode.chars() {
            if let Some(paint) = paintable(ch, font) {
                text.push(paint);
            }
        }
    }
    text
}

fn paintable(ch: char, font: &SubsetFont) -> Option<char> {
    if font.glyphs.contains_key(&(ch as u32)) {
        return Some(ch);
    }
    coverage_fallbacks(ch)
        .iter()
        .copied()
        .find(|alt| font.glyphs.contains_key(&(*alt as u32)))
        .or_else(|| latin_base(ch).filter(|alt| font.glyphs.contains_key(&(*alt as u32))))
}

fn drawn_hits(drawn: &[Drawn], font: &SubsetFont, page: u32, bbox: [f32; 4]) -> bool {
    drawn
        .iter()
        .any(|line| line.page == page && boxes_overlap(drawn_bbox(line, font), bbox, 2.0))
}

fn drawn_bbox(line: &Drawn, font: &SubsetFont) -> [f32; 4] {
    let width = measure(&line.text, line.size, font).max(line.size * 0.4);
    [
        line.x,
        line.y - line.size * 0.3,
        line.x + width,
        line.y + line.size * 0.9,
    ]
}

fn boxes_overlap(a: [f32; 4], b: [f32; 4], pad: f32) -> bool {
    let ax0 = a[0].min(a[2]);
    let ay0 = a[1].min(a[3]);
    let ax1 = a[0].max(a[2]);
    let ay1 = a[1].max(a[3]);
    let bx0 = b[0].min(b[2]);
    let by0 = b[1].min(b[3]);
    let bx1 = b[0].max(b[2]);
    let by1 = b[1].max(b[3]);
    ax0 - pad < bx1 && ax1 + pad > bx0 && ay0 - pad < by1 && ay1 + pad > by0
}

fn pages_of(glyphs: &[&Glyph]) -> Vec<u32> {
    let mut pages: Vec<u32> = glyphs.iter().map(|glyph| glyph.page_index).collect();
    pages.sort_unstable();
    pages.dedup();
    pages
}

/// The face of a paragraph is the majority of its prose letters. A short
/// italic citation or a bold label that stayed on the sentence must not
/// paint the roman body in heiti or with a skew.
fn paragraph_style(glyphs: &[&Glyph]) -> FaceStyle {
    let mut serif = 0usize;
    let mut letters = 0usize;
    let mut bold = 0usize;
    let mut italic = 0usize;
    for glyph in glyphs {
        if !style_vote(glyph) {
            continue;
        }
        let n = glyph
            .unicode
            .chars()
            .filter(|ch| ch.is_ascii_alphabetic())
            .count();
        let style = face_style(&glyph.font_name);
        letters += n;
        if style.serif {
            serif += n;
        }
        if style.bold {
            bold += n;
        }
        if style.italic {
            italic += n;
        }
    }
    if letters == 0 {
        return glyphs
            .first()
            .map(|glyph| face_style(&glyph.font_name))
            .unwrap_or(FaceStyle {
                serif: true,
                bold: false,
                italic: false,
            });
    }
    FaceStyle {
        serif: serif * 2 >= letters,
        bold: bold * 2 > letters,
        italic: italic * 2 > letters,
    }
}

fn style_vote(glyph: &Glyph) -> bool {
    if !glyph.unicode.chars().any(|ch| ch.is_ascii_alphabetic()) {
        return false;
    }
    let upper = glyph.font_name.to_ascii_uppercase();
    let math = ["CMMI", "CMSY", "CMEX", "MSAM", "MSBM"]
        .iter()
        .any(|needle| upper.contains(needle))
        || upper.contains("MATH");
    !math
}

fn heading_wants_bold(glyphs: &[&Glyph], text: &str) -> bool {
    if glyphs.is_empty() {
        return false;
    }
    let mut sizes: Vec<f32> = glyphs.iter().map(|glyph| glyph.font_size).collect();
    sizes.sort_by(|left, right| left.total_cmp(right));
    let size = sizes[sizes.len() / 2];
    let mut rows: Vec<i32> = glyphs
        .iter()
        .map(|glyph| (glyph.matrix[5] * 2.0) as i32)
        .collect();
    rows.sort_unstable();
    rows.dedup();
    let bold = glyphs.iter().all(|glyph| face_style(&glyph.font_name).bold);
    matches!(
        heading_level(rows.len().max(1), size, bold, text),
        HeadingLevel::Subsection | HeadingLevel::Section
    )
}

struct PagePlan {
    page: u32,
    left: f32,
    width: f32,
    ys: Vec<f32>,
    lines: Vec<String>,
}

/// Place one translated paragraph into the source boxes on each page it
/// crosses. Shrinking is allowed. A tail that still does not fit keeps the
/// original paragraph instead of painting a prefix.
fn layout_across_pages(
    glyphs: &[&Glyph],
    text: &str,
    font: &SubsetFont,
    extraction: &Extraction,
    resource: &str,
    skew: f32,
    metrics: CjkMeasure,
) -> Option<Vec<Drawn>> {
    let pages = pages_of(glyphs);
    let mut boxes = Vec::new();
    let mut ink_pages = Vec::new();
    for page in pages {
        let subset: Vec<&Glyph> = glyphs
            .iter()
            .copied()
            .filter(|glyph| glyph.page_index == page)
            .collect();
        let ink = ink_lines(&subset);
        if ink.is_empty() {
            continue;
        }
        let size0 = ink
            .iter()
            .flat_map(|line| line.glyphs.iter().map(|glyph| glyph.font_size))
            .fold(1.0f32, f32::max);
        if ink
            .iter()
            .any(|line| segment_crosses_column(&line.glyphs, &subset, size0))
        {
            return None;
        }
        let spans: Vec<(f32, f32)> = ink.iter().map(|line| line_span(line, size0)).collect();
        let left = spans.iter().map(|(left, _)| *left).fold(f32::MAX, f32::min);
        let right = spans.iter().map(|(_, right)| *right).fold(left, f32::max);
        boxes.push(LineSlots {
            width: (right - left).max(size0),
            slots: ink.len(),
        });
        ink_pages.push((page, ink, left, right));
    }
    if boxes.len() < 2 {
        return None;
    }
    let mut sizes: Vec<f32> = glyphs.iter().map(|glyph| glyph.font_size).collect();
    sizes.sort_by(|left, right| left.total_cmp(right));
    let source_size = sizes[sizes.len() / 2].max(1.0);
    let line_count: usize = boxes.iter().map(|slot| slot.slots).sum();
    let bold = glyphs.iter().all(|glyph| face_style(&glyph.font_name).bold);
    let level = heading_level(line_count, source_size, bold, text);
    let hanging = ink_pages
        .first()
        .and_then(|(_, ink, _, _)| list_outdent(ink, source_size));
    let mut indent_ems = cjk_indent_ems(line_count, source_size, bold, text);
    // The marker hangs on the first page. Later pages are already at the
    // continuation edge, so they do not take a body first-line indent.
    if let Some(shift) = hanging {
        indent_ems = -(shift / source_size);
        if let Some(slot) = boxes.first_mut() {
            slot.width = (slot.width - shift).max(source_size);
        }
    }
    let scale = scale_for_heading(level, source_size, metrics);
    let start = (source_size * scale).max(1.0);
    let floor = (start * 0.78).max(source_size * 0.62).min(start);
    let mut size = start;
    let packed = loop {
        let indent = size * indent_ems;
        if let Some(packed) = pack_into_slots(text, size, indent, &boxes, font) {
            let flat: String = packed
                .iter()
                .flatten()
                .flat_map(|line| line.chars())
                .filter(|ch| !ch.is_whitespace())
                .collect();
            let wanted: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
            if flat == wanted {
                break packed;
            }
        }
        if size <= floor + 0.01 {
            return None;
        }
        let next = (size * 0.94).max(floor);
        if (next - size).abs() < 0.01 {
            return None;
        }
        size = next;
    };
    let color = glyphs[0].fill_color.clone();
    let indent = size * indent_ems;
    let sup = superscript_metrics(glyphs);
    let mask = sup
        .as_ref()
        .map(|_| superscript_mask(text))
        .unwrap_or_default();
    // Spread before assigning superscripts. A mask built on the packed lines
    // would land on the wrong character once a line is split onto the next
    // source baseline.
    let mut plans: Vec<PagePlan> = Vec::new();
    for (box_index, ((page, ink, left, _), lines)) in ink_pages.iter().zip(packed).enumerate() {
        if lines.is_empty() {
            continue;
        }
        let top = ink[0].y;
        let bottom = ink[ink.len() - 1].y;
        let page_width = boxes[box_index].width;
        let reflow = column_reflow(level, ink.len(), page_width, source_size, hanging.is_some());
        let lines = if hanging.is_some() || reflow || level == HeadingLevel::Title {
            lines
        } else {
            spread_to_lines(lines, ink.len())
        };
        let gaps = lines.len().saturating_sub(1) as f32;
        let leading = if gaps == 0.0 {
            size * metrics.leading_ratio
        } else {
            let room = (top - bottom) / gaps;
            room.min(size * metrics.leading_ratio).max(size * 1.05)
        };
        let last_y = top - gaps * leading;
        let media = extraction
            .pages
            .iter()
            .find(|info| info.index == *page)
            .map(|info| info.media_box)
            .unwrap_or([0.0, 0.0, 612.0, 792.0]);
        if last_y < media[1] - 0.5 || top > media[3] {
            return None;
        }
        let ys = baselines_for(ink, lines.len());
        let draw_left = if box_index == 0 {
            hanging.map(|shift| left + shift).unwrap_or(*left)
        } else {
            *left
        };
        plans.push(PagePlan {
            page: *page,
            left: draw_left,
            width: boxes[box_index].width,
            ys,
            lines,
        });
    }
    let flat_lines: Vec<String> = plans
        .iter()
        .flat_map(|plan| plan.lines.iter().cloned())
        .collect();
    let line_masks = split_superscripts(text, &flat_lines, &mask);
    let mut mask_index = 0usize;
    let mut drawn = Vec::new();
    let mut first_line = true;
    for PagePlan {
        page,
        left,
        width,
        ys,
        lines,
    } in plans
    {
        for (index, line) in lines.into_iter().enumerate() {
            let line_indent = if first_line { indent } else { 0.0 };
            let limit = (width - line_indent).max(size * 0.5);
            let supers = line_masks.get(mask_index).cloned().unwrap_or_default();
            mask_index += 1;
            drawn.push(Drawn {
                page,
                x: left + line_indent,
                y: ys[index],
                size,
                color: color.clone(),
                cids: cids_of(&line, font),
                resource: resource.to_string(),
                skew,
                widths: widths_with_superscripts(&line, size, font, &supers, sup.as_ref()),
                gaps: justify_gaps(&line, size, limit, font, false),
                text: line,
                supers,
                sup_scale: sup.as_ref().map(|item| item.scale).unwrap_or(1.0),
                sup_rise: sup.as_ref().map(|item| item.rise).unwrap_or(0.0),
                glyph_ids: glyphs.iter().map(|glyph| glyph.id).collect(),
            });
            first_line = false;
        }
    }
    let painted: String = drawn
        .iter()
        .flat_map(|line| line.text.chars())
        .filter(|ch| !ch.is_whitespace())
        .collect();
    let wanted: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    if painted != wanted {
        return None;
    }
    let first_ink = ink_pages
        .first()
        .map(|(_, ink, _, _)| ink.as_slice())
        .unwrap_or(&[]);
    let mut drawn = restyle_list_lines(drawn, first_ink, font);
    let cover_color = list_run_colors(first_ink)
        .map(|(_, text_color)| text_color)
        .unwrap_or(color);
    let reflowed = ink_pages.iter().enumerate().any(|(index, (_, ink, _, _))| {
        column_reflow(
            level,
            ink.len(),
            boxes.get(index).map(|slot| slot.width).unwrap_or(0.0),
            source_size,
            hanging.is_some(),
        )
    });
    let skip: Vec<u32> = if hanging.is_some() || reflowed {
        glyphs.iter().map(|glyph| glyph.id).collect()
    } else {
        Vec::new()
    };
    cover_unhit_clusters(
        &mut drawn,
        glyphs,
        font,
        resource,
        skew,
        &cover_color,
        &glyphs.iter().map(|glyph| glyph.id).collect::<Vec<_>>(),
        &skip,
    );
    Some(drawn)
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
        let same = groups.last().is_some_and(|group| {
            let anchor = group[0];
            // A drop cap on this baseline must not swallow the next line.
            let limit = crate::segment::representative_size(group);
            (anchor.matrix[5] - glyph.matrix[5]).abs() <= limit * 0.35
        });
        if same {
            groups.last_mut().unwrap().push(glyph);
        } else {
            groups.push(vec![glyph]);
        }
    }
    absorb_script_baselines(&mut groups);
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

/// `N×N` drawn a few points above the words is not a line of its own.
/// Painting a full-size character there stacks it on the next baseline
/// (`得` through `到`).
fn absorb_script_baselines(groups: &mut Vec<Vec<&Glyph>>) {
    let mut index = 0;
    while index < groups.len() {
        let Some(host) = script_baseline_host(index, groups) else {
            index += 1;
            continue;
        };
        let scripts = groups.remove(index);
        let host = if host > index { host - 1 } else { host };
        groups[host].extend(scripts);
    }
}

fn script_baseline_host(index: usize, groups: &[Vec<&Glyph>]) -> Option<usize> {
    let group = &groups[index];
    if !is_script_baseline(group) {
        return None;
    }
    let y = group_anchor_y(group);
    let (left, right) = group_span(group);
    let mut best: Option<(usize, f32)> = None;
    for (other_index, other) in groups.iter().enumerate() {
        if other_index == index || is_script_baseline(other) {
            continue;
        }
        let size = crate::segment::representative_size(other).max(1.0);
        let dy = (y - group_anchor_y(other)).abs();
        if dy < size * 0.08 || dy > size * 0.55 {
            continue;
        }
        let (other_left, other_right) = group_span(other);
        if right < other_left - size || left > other_right + size {
            continue;
        }
        if best.is_none_or(|(_, best_dy)| dy < best_dy) {
            best = Some((other_index, dy));
        }
    }
    best.map(|(host, _)| host)
}

fn is_script_baseline(group: &[&Glyph]) -> bool {
    (1..=6).contains(&group.len())
        && group.iter().any(|glyph| is_math_font(&glyph.font_name))
        && group.iter().all(|glyph| {
            is_math_font(&glyph.font_name)
                || !glyph.unicode.chars().any(|ch| ch.is_ascii_alphabetic())
        })
}

fn group_anchor_y(group: &[&Glyph]) -> f32 {
    let mut ys: Vec<f32> = group.iter().map(|glyph| glyph.matrix[5]).collect();
    ys.sort_by(|left, right| left.total_cmp(right));
    ys[ys.len() / 2]
}

fn group_span(group: &[&Glyph]) -> (f32, f32) {
    let left = group
        .iter()
        .map(|glyph| glyph.matrix[4].min(glyph.bbox[0]).min(glyph.bbox[2]))
        .fold(f32::MAX, f32::min);
    let right = group
        .iter()
        .map(|glyph| glyph.bbox[0].max(glyph.bbox[2]).max(glyph.matrix[4]))
        .fold(left, f32::max);
    (left, right)
}

fn colors_close(left: &Color, right: &Color) -> bool {
    left.space == right.space
        && left.components.len() == right.components.len()
        && left
            .components
            .iter()
            .zip(&right.components)
            .all(|(a, b)| (a - b).abs() <= 0.08)
}

/// Right edge of the primary fill on one display line. The cover subtitle
/// switches from the panel ink to a darker tail where the illustration starts.
fn display_panel_right(line: &InkLine<'_>) -> Option<f32> {
    let mut glyphs: Vec<&Glyph> = line.glyphs.to_vec();
    glyphs.sort_by(|left, right| left.matrix[4].total_cmp(&right.matrix[4]));
    if glyphs.len() < 6 {
        return None;
    }
    struct Run {
        start: f32,
        right: f32,
        count: usize,
        color: Color,
    }
    let mut runs: Vec<Run> = Vec::new();
    for glyph in glyphs {
        let left = glyph.matrix[4].min(glyph.bbox[0]);
        let right = glyph.bbox[2].max(glyph.matrix[4] + glyph.advance[0].max(0.0));
        if let Some(run) = runs.last_mut() {
            let gap = left - run.right;
            if colors_close(&run.color, &glyph.fill_color) && gap < glyph.font_size.max(1.0) {
                run.right = run.right.max(right);
                run.count += 1;
                continue;
            }
        }
        runs.push(Run {
            start: left,
            right,
            count: 1,
            color: glyph.fill_color.clone(),
        });
    }
    if runs.len() < 2 {
        return None;
    }
    let primary = &runs[0];
    let rest_start = runs[1].start;
    let rest_right = runs.last().map(|run| run.right).unwrap_or(rest_start);
    let rest_count: usize = runs[1..].iter().map(|run| run.count).sum();
    let primary_width = primary.right - primary.start;
    let rest_width = (rest_right - rest_start).max(0.0);
    if primary.count >= 4
        && rest_count >= 2
        && rest_start + 0.5 >= primary.right - 1.0
        && primary_width > rest_width
    {
        Some(primary.right)
    } else {
        None
    }
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

fn paragraph_is_bold(ink: &[InkLine<'_>]) -> bool {
    let mut saw = false;
    for line in ink {
        for glyph in &line.glyphs {
            saw = true;
            if !face_style(&glyph.font_name).bold {
                return false;
            }
        }
    }
    saw
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
                "0 Tc /{} {} Tf {} 1 0 {} 1 {} {} Tm {}\n",
                line.resource,
                pdf_num(line.size),
                color_ops(&line.color),
                pdf_num(line.skew),
                pdf_num(line.x),
                pdf_num(line.y),
                show_text(line)
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
    // A page can leave the CTM translated (a table or a form that never
    // restored `Q`). Content streams share that state, so text appended
    // afterwards is shifted onto the table. Save the initial CTM, let the
    // page run, then restore before drawing.
    let saved = doc.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
    let restored = doc.add_object(Stream::new(Dictionary::new(), b"Q\n".to_vec()));
    let contents = doc
        .get_dictionary(page_id)
        .ok()
        .and_then(|dict| dict.get(b"Contents").ok().cloned());
    let replacement = match contents {
        None => Object::Reference(stream_id),
        Some(Object::Reference(id)) => match doc.get_object(id) {
            Ok(Object::Array(array)) => {
                let mut wrapped = vec![Object::Reference(saved)];
                wrapped.extend(array.iter().cloned());
                wrapped.push(Object::Reference(restored));
                wrapped.push(Object::Reference(stream_id));
                Object::Array(wrapped)
            }
            _ => Object::Array(vec![
                Object::Reference(saved),
                Object::Reference(id),
                Object::Reference(restored),
                Object::Reference(stream_id),
            ]),
        },
        Some(Object::Array(array)) => {
            let mut wrapped = vec![Object::Reference(saved)];
            wrapped.extend(array.iter().cloned());
            wrapped.push(Object::Reference(restored));
            wrapped.push(Object::Reference(stream_id));
            Object::Array(wrapped)
        }
        Some(other) => Object::Array(vec![
            Object::Reference(saved),
            other,
            Object::Reference(restored),
            Object::Reference(stream_id),
        ]),
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
    } else if color.space == "DeviceCMYK" && color.components.len() >= 4 {
        format!(
            "{} {} {} {} k",
            pdf_num(color.components[0]),
            pdf_num(color.components[1]),
            pdf_num(color.components[2]),
            pdf_num(color.components[3])
        )
    } else {
        "0 g".into()
    }
}

fn hex_cids(cids: &[u16]) -> String {
    cids.iter().map(|cid| format!("{cid:04X}")).collect()
}

struct SupMetrics {
    scale: f32,
    rise: f32,
}

/// Source affiliation marks are smaller and, when `Ts` raised them, sit above
/// the body baseline. Body digits such as a year stay on the baseline.
fn superscript_metrics(glyphs: &[&Glyph]) -> Option<SupMetrics> {
    let mut body_sizes = Vec::new();
    let mut body_ys = Vec::new();
    for glyph in glyphs {
        if affil_marker(&glyph.unicode) {
            continue;
        }
        body_sizes.push(glyph.font_size);
        body_ys.push(glyph.matrix[5]);
    }
    if body_sizes.is_empty() {
        return None;
    }
    body_sizes.sort_by(|left, right| left.total_cmp(right));
    body_ys.sort_by(|left, right| left.total_cmp(right));
    let body = body_sizes[body_sizes.len() / 2].max(1.0);
    let base = body_ys[body_ys.len() / 2];
    let mut scales = Vec::new();
    let mut rises = Vec::new();
    for glyph in glyphs {
        if !affil_marker(&glyph.unicode) {
            continue;
        }
        let smaller = glyph.font_size <= body * 0.85;
        let raised = glyph.matrix[5] >= base + body * 0.12;
        if !smaller && !raised {
            continue;
        }
        scales.push((glyph.font_size / body).clamp(0.45, 1.0));
        let rise = ((glyph.matrix[5] - base) / body).max(0.0);
        rises.push(rise.clamp(0.0, 0.6));
    }
    if scales.is_empty() {
        return None;
    }
    scales.sort_by(|left, right| left.total_cmp(right));
    rises.sort_by(|left, right| left.total_cmp(right));
    Some(SupMetrics {
        scale: scales[scales.len() / 2],
        rise: rises[rises.len() / 2],
    })
}

fn affil_marker(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && text.chars().count() <= 2
        && text
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '*' | '∗' | '†' | '‡' | '§' | '¶' | '⋆'))
}

/// Author marks follow a name (`张轩1`). Affiliation marks lead a name (`1新加坡`).
/// A run of three or more digits is a year or a count and stays on the baseline.
fn superscript_mask(text: &str) -> Vec<bool> {
    let chars: Vec<char> = text.chars().collect();
    let mut mask = vec![false; chars.len()];
    let mut index = 0;
    while index < chars.len() {
        // A star and the digit after it are separate marks: `Zhang∗1`.
        if is_mark_star(chars[index]) {
            let prev = index.checked_sub(1).map(|slot| chars[slot]);
            let word = chars
                .iter()
                .skip(index + 1)
                .take_while(|ch| ch.is_alphabetic())
                .count();
            let after_name = prev.is_some_and(char::is_alphabetic);
            let before_name = word >= 2
                && prev
                    .is_none_or(|ch| ch.is_whitespace() || matches!(ch, ',' | '，' | ';' | '；'));
            if after_name || before_name {
                mask[index] = true;
            }
            index += 1;
            continue;
        }
        if !chars[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len() && chars[index].is_ascii_digit() && index - start < 2 {
            index += 1;
        }
        if index < chars.len() && chars[index].is_ascii_digit() {
            while index < chars.len() && chars[index].is_ascii_digit() {
                index += 1;
            }
            continue;
        }
        // `第4章`, `第4 章`, `第1–2章`, and `章节3–5` are body text.
        // `张轩1` has no Han after it and is not a range.
        if han_wraps_number(&chars, start, index) || chapter_range_digit(&chars, start) {
            continue;
        }
        let prev = start.checked_sub(1).map(|slot| chars[slot]);
        let run = index - start;
        // One digit after a name, or one digit before an institution word.
        // `16K` is a body token: two digits glued to a single letter.
        let word = chars
            .iter()
            .skip(index)
            .take_while(|ch| ch.is_alphabetic())
            .count();
        // `8.18`, `0.001`, `n ≥ 2`, and `1)` are body text. `LayoutLMv3`
        // is a version token. `Zhang1` is still an author mark.
        let body_digit = decimal_digit(&chars, start)
            || enumeration_digit(&chars, index)
            || follows_operator(&chars, start)
            || version_token(&chars, start);
        let after_name = !body_digit
            && run == 1
            && prev.is_some_and(|ch| {
                ch.is_alphabetic() || (is_mark_star(ch) && start > 0 && mask[start - 1])
            });
        let before_name = run == 1
            && word >= 2
            && prev.is_none_or(|ch| {
                ch.is_whitespace() || matches!(ch, ',' | '，' | ';' | '；' | '*' | '∗')
            });
        // `第4章` is a body phrase. `张轩1` is an author mark: nothing follows the digit.
        let followed_by_han = chars.get(index).copied().is_some_and(is_han_char);
        if (after_name && !followed_by_han) || before_name {
            for flag in mask.iter_mut().take(index).skip(start) {
                *flag = true;
            }
        }
    }
    // `Zhang1,2` and `1,2Singapore`: a short mark beside a comma follows its neighbor.
    let mut again = true;
    while again {
        again = false;
        index = 0;
        while index < chars.len() {
            if mask[index] || !chars[index].is_ascii_digit() {
                index += 1;
                continue;
            }
            let start = index;
            while index < chars.len() && chars[index].is_ascii_digit() && index - start < 2 {
                index += 1;
            }
            if index < chars.len() && chars[index].is_ascii_digit() {
                while index < chars.len() && chars[index].is_ascii_digit() {
                    index += 1;
                }
                continue;
            }
            let before = start.checked_sub(1).is_some_and(|slot| {
                matches!(chars[slot], ',' | '，') && slot > 0 && mask[slot - 1]
            });
            let after = chars.get(index).is_some_and(|ch| matches!(ch, ',' | '，'))
                && chars
                    .get(index + 1)
                    .is_some_and(|ch| ch.is_ascii_digit() && mask.get(index + 1) == Some(&true));
            if before || after {
                for flag in mask.iter_mut().take(index).skip(start) {
                    if !*flag {
                        *flag = true;
                        again = true;
                    }
                }
            }
        }
    }
    mask
}

fn is_dash_char(ch: char) -> bool {
    matches!(ch, '-' | '–' | '—' | '−')
}

/// A chapter index sitting between Han characters, allowing a space or a
/// range dash (`第4章`, `第 4 章`, `第1–2章`).
fn han_wraps_number(chars: &[char], start: usize, end: usize) -> bool {
    let before = (0..start).rev().find(|&slot| {
        let ch = chars[slot];
        !ch.is_whitespace() && !is_dash_char(ch)
    });
    if !before.is_some_and(|slot| is_han_char(chars[slot])) {
        return false;
    }
    let mut index = end;
    loop {
        let mut across_dash = false;
        while index < chars.len() && (chars[index].is_whitespace() || is_dash_char(chars[index])) {
            if is_dash_char(chars[index]) {
                across_dash = true;
            }
            index += 1;
        }
        // `1–2` is one chapter range. `1 1新加坡` is two affiliation marks.
        if across_dash && index < chars.len() && chars[index].is_ascii_digit() {
            while index < chars.len() && chars[index].is_ascii_digit() {
                index += 1;
            }
            continue;
        }
        break;
    }
    index < chars.len() && is_han_char(chars[index])
}

fn is_mark_star(ch: char) -> bool {
    matches!(ch, '*' | '∗' | '†' | '‡' | '§' | '¶' | '⋆')
}

/// `8.18` and `0.001`: the digit is the integer part of a decimal.
fn decimal_digit(chars: &[char], start: usize) -> bool {
    chars
        .get(start + 1)
        .is_some_and(|ch| matches!(ch, '.' | '．'))
        && chars.get(start + 2).is_some_and(|ch| ch.is_ascii_digit())
}

/// `1)` starts a list item. An author mark is not followed by a parenthesis.
fn enumeration_digit(chars: &[char], end: usize) -> bool {
    chars.get(end).is_some_and(|ch| matches!(ch, ')' | '）'))
}

/// `n ≥ 2` and `x > 1`. The digit belongs to the relation, not to a name.
fn follows_operator(chars: &[char], start: usize) -> bool {
    let prev = (0..start)
        .rev()
        .map(|slot| chars[slot])
        .find(|ch| !ch.is_whitespace());
    prev.is_some_and(|ch| {
        matches!(
            ch,
            '≥' | '≤' | '≈' | '>' | '<' | '=' | '+' | '−' | '-' | '×' | '/' | '＞' | '＜'
        )
    })
}

/// `LayoutLMv3` and `GPT4` carry a version. `Zhang1` has one capital.
fn version_token(chars: &[char], start: usize) -> bool {
    let mut index = start;
    let mut uppers = 0usize;
    while index > 0 && chars[index - 1].is_ascii_alphabetic() {
        index -= 1;
        if chars[index].is_ascii_uppercase() {
            uppers += 1;
        }
    }
    uppers >= 2
}

fn english_body_source(text: &str) -> bool {
    // An HTML row echoed by the model is markup, not a paragraph that failed
    // to translate. Four tags is a table fragment; a sentence is not.
    let tags = text.matches('<').count();
    if tags >= 4 && text.matches('>').count() >= 4 {
        return false;
    }
    let latin = text.chars().filter(|ch| ch.is_ascii_alphabetic()).count();
    if latin < 40 || text.chars().any(is_han_char) {
        return false;
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    !words.is_empty()
        && !words.iter().all(|word| {
            word.starts_with("http://")
                || word.starts_with("https://")
                || word.contains('@')
                || word.chars().all(|ch| ch.is_ascii_digit())
        })
}

fn is_han_char(ch: char) -> bool {
    matches!(ch, '\u{3400}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}')
}

/// Digits stay ASCII, including a chapter index against Han (`第4章`).
/// Mixing fullwidth and ASCII (`第８–10章`) makes the same number two widths.
/// Author marks (`张1`) stay ASCII and can still rise.
fn fullwidth_han_digits(text: &str, supers: &[bool], _font: &SubsetFont) -> (String, Vec<bool>) {
    let mask = text
        .chars()
        .enumerate()
        .map(|(index, _)| supers.get(index).copied().unwrap_or(false))
        .collect();
    (text.to_string(), mask)
}

/// `第8–10章` and `章节3–5`: the digits are one chapter range, not author marks.
fn chapter_range_digit(chars: &[char], index: usize) -> bool {
    let Some((start, end)) = dash_range(chars, index) else {
        return false;
    };
    let prev = (0..start)
        .rev()
        .map(|slot| chars[slot])
        .find(|ch| !ch.is_whitespace());
    let next = chars[end..].iter().copied().find(|ch| !ch.is_whitespace());
    prev.is_some_and(is_han_char) || next.is_some_and(is_han_char)
}

/// Digits joined by a dash (`3–5`, `8–10`), including spaces around the dash.
fn dash_range(chars: &[char], index: usize) -> Option<(usize, usize)> {
    if !chars.get(index).is_some_and(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let mut start = index;
    while start > 0 && chars[start - 1].is_ascii_digit() {
        start -= 1;
    }
    let mut end = index + 1;
    while end < chars.len() && chars[end].is_ascii_digit() {
        end += 1;
    }
    let mut saw_dash = false;
    loop {
        let mut cursor = end;
        let mut dash = false;
        while cursor < chars.len() && (chars[cursor].is_whitespace() || is_dash_char(chars[cursor]))
        {
            if is_dash_char(chars[cursor]) {
                dash = true;
            }
            cursor += 1;
        }
        if !dash || cursor >= chars.len() || !chars[cursor].is_ascii_digit() {
            break;
        }
        saw_dash = true;
        while cursor < chars.len() && chars[cursor].is_ascii_digit() {
            cursor += 1;
        }
        end = cursor;
    }
    loop {
        let mut cursor = start;
        let mut dash = false;
        while cursor > 0 {
            let ch = chars[cursor - 1];
            if !(ch.is_whitespace() || is_dash_char(ch)) {
                break;
            }
            if is_dash_char(ch) {
                dash = true;
            }
            cursor -= 1;
        }
        if !dash || cursor == 0 || !chars[cursor - 1].is_ascii_digit() {
            break;
        }
        saw_dash = true;
        while cursor > 0 && chars[cursor - 1].is_ascii_digit() {
            cursor -= 1;
        }
        start = cursor;
    }
    saw_dash.then_some((start, end))
}

fn split_superscripts(text: &str, lines: &[String], mask: &[bool]) -> Vec<Vec<bool>> {
    let chars: Vec<char> = text.chars().collect();
    let mut cursor = 0usize;
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let mut line_mask = Vec::new();
        for ch in line.chars() {
            while cursor < chars.len() && chars[cursor] != ch && chars[cursor].is_whitespace() {
                cursor += 1;
            }
            if cursor < chars.len() && chars[cursor] == ch {
                line_mask.push(mask.get(cursor).copied().unwrap_or(false));
                cursor += 1;
            } else {
                line_mask.push(false);
            }
        }
        out.push(line_mask);
    }
    out
}

fn widths_with_superscripts(
    text: &str,
    size: f32,
    font: &SubsetFont,
    supers: &[bool],
    metrics: Option<&SupMetrics>,
) -> Vec<f32> {
    let mut widths = char_widths(text, size, font);
    let Some(metrics) = metrics else {
        return widths;
    };
    for (width, sup) in widths.iter_mut().zip(supers) {
        if *sup {
            *width *= metrics.scale;
        }
    }
    widths
}

fn show_text(line: &Drawn) -> String {
    let count = line.text.chars().count();
    let raised = line.cids.len() == count
        && line.supers.len() == count
        && line.supers.iter().any(|flag| *flag);
    if !raised {
        if line.cids.len() != count || line.gaps.iter().all(|gap| *gap == 0.0) || line.size <= 0.0 {
            return format!("<{}> Tj", hex_cids(&line.cids));
        }
        return show_run(&line.cids, &line.gaps, 0, line.size);
    }
    let mut out = String::new();
    let mut index = 0usize;
    while index < line.cids.len() {
        let sup = line.supers[index];
        let mut end = index + 1;
        while end < line.cids.len() && line.supers[end] == sup {
            end += 1;
        }
        let size = if sup {
            (line.size * line.sup_scale).max(0.5)
        } else {
            line.size
        };
        let rise = if sup { line.size * line.sup_rise } else { 0.0 };
        out.push_str(&format!(
            "{} Ts /{} {} Tf {} ",
            pdf_num(rise),
            line.resource,
            pdf_num(size),
            show_run(&line.cids[index..end], &line.gaps, index, size)
        ));
        index = end;
    }
    out.push_str("0 Ts ");
    out
}

fn show_run(cids: &[u16], gaps: &[f32], start: usize, size: f32) -> String {
    let internal = (start..start + cids.len())
        .any(|index| index > 0 && gaps.get(index - 1).is_some_and(|gap| *gap != 0.0));
    if cids.is_empty() || !internal || size <= 0.0 {
        return format!("<{}> Tj", hex_cids(cids));
    }
    let mut parts = Vec::with_capacity(cids.len() * 2);
    for (offset, cid) in cids.iter().enumerate() {
        let index = start + offset;
        if index > 0 {
            let extra = gaps.get(index - 1).copied().unwrap_or(0.0);
            let adj = -((extra / size) * 1000.0).round() as i32;
            if adj != 0 {
                parts.push(adj.to_string());
            }
        }
        parts.push(format!("<{cid:04X}>"));
    }
    format!("[{}] TJ", parts.join(" "))
}

#[derive(Clone)]
struct LinkRef {
    id: ObjectId,
    rect: [f32; 4],
}

/// Move link rectangles onto the rewritten citation, or drop them when the
/// original words no longer have a single place on the page.
fn retarget_link_annotations(
    doc: &mut Document,
    extraction: &Extraction,
    drawn: &[Drawn],
    rewritten: &HashSet<u32>,
) {
    for page in &extraction.pages {
        let Some(page_id) = parse_id(&page.object_id) else {
            continue;
        };
        let Some((holder, items)) = load_annots(doc, page_id) else {
            continue;
        };
        let links: Vec<LinkRef> = items.iter().filter_map(|obj| link_ref(doc, obj)).collect();
        if links.is_empty() {
            continue;
        }
        let mut updates: HashMap<ObjectId, [f32; 4]> = HashMap::new();
        let mut drop_ids: HashSet<ObjectId> = HashSet::new();
        let mut clones: Vec<ObjectId> = Vec::new();
        for cluster in cluster_links(&links) {
            match place_cluster(&cluster, page.index, extraction, drawn, rewritten) {
                ClusterPlace::Keep => {}
                ClusterPlace::Drop(ids) => drop_ids.extend(ids),
                ClusterPlace::Move { ids, rects } => {
                    let count = ids.len().min(rects.len());
                    for (id, rect) in ids.iter().zip(rects.iter()).take(count) {
                        updates.insert(*id, *rect);
                    }
                    if ids.len() > rects.len() {
                        drop_ids.extend(ids.iter().skip(rects.len()).copied());
                    }
                    let Some(source) = ids.first().copied() else {
                        continue;
                    };
                    for rect in rects.iter().skip(ids.len()) {
                        if let Some(id) = clone_link(doc, source, *rect) {
                            clones.push(id);
                        }
                    }
                }
            }
        }
        if updates.is_empty() && drop_ids.is_empty() && clones.is_empty() {
            continue;
        }
        for (id, rect) in &updates {
            set_annot_rect(doc, *id, *rect);
        }
        let mut kept = Vec::new();
        for item in items {
            let drop = item
                .as_reference()
                .ok()
                .is_some_and(|id| drop_ids.contains(&id));
            if !drop {
                kept.push(item);
            }
        }
        for id in clones {
            kept.push(Object::Reference(id));
        }
        store_annots(doc, page_id, holder, kept);
    }
}

enum ClusterPlace {
    Keep,
    Drop(Vec<ObjectId>),
    Move {
        ids: Vec<ObjectId>,
        rects: Vec<[f32; 4]>,
    },
}

fn place_cluster(
    cluster: &[LinkRef],
    page: u32,
    extraction: &Extraction,
    drawn: &[Drawn],
    rewritten: &HashSet<u32>,
) -> ClusterPlace {
    let union = union_rect(cluster);
    let mut glyphs: Vec<&Glyph> = extraction
        .glyphs
        .iter()
        .filter(|glyph| glyph.page_index == page && glyph_under_link(glyph, union))
        .collect();
    if glyphs.is_empty() || !glyphs.iter().any(|glyph| rewritten.contains(&glyph.id)) {
        return ClusterPlace::Keep;
    }
    glyphs.sort_by(|a, b| {
        a.matrix[4]
            .total_cmp(&b.matrix[4])
            .then(a.matrix[5].total_cmp(&b.matrix[5]))
    });
    let needle = squash_text(&glyphs_text(&glyphs));
    let ids: Vec<ObjectId> = cluster.iter().map(|link| link.id).collect();
    if needle.chars().count() < 2 {
        return ClusterPlace::Drop(ids);
    }
    let lines: Vec<&Drawn> = drawn.iter().filter(|line| line.page == page).collect();
    let Some(hit) = unique_citation_hit(&lines, &needle) else {
        return ClusterPlace::Drop(ids);
    };
    let rects = if hit.len() == 1 && cluster.len() > 1 {
        split_span(hit[0], cluster)
    } else {
        hit
    };
    ClusterPlace::Move { ids, rects }
}

fn unique_citation_hit(lines: &[&Drawn], needle: &str) -> Option<Vec<[f32; 4]>> {
    let mut ordered = lines.to_vec();
    ordered.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x.total_cmp(&b.x)));
    let mut hits: Vec<Vec<[f32; 4]>> = Vec::new();
    for line in &ordered {
        if let Some(rect) = line_hit(line, needle) {
            hits.push(vec![rect]);
        }
    }
    for pair in ordered.windows(2) {
        if !lines_are_adjacent(pair[0], pair[1]) {
            continue;
        }
        if let Some(rects) = pair_hit(pair[0], pair[1], needle) {
            hits.push(rects);
        }
    }
    if hits.len() == 1 {
        hits.pop()
    } else {
        None
    }
}

fn line_hit(line: &Drawn, needle: &str) -> Option<[f32; 4]> {
    let (flat, map) = squash_map(&line.text);
    let start = unique_index(&flat, needle)?;
    let end = start + needle.chars().count();
    let first = *map.get(start)?;
    let last = *map.get(end - 1)?;
    Some(span_rect(line, first, last + 1))
}

fn pair_hit(top: &Drawn, bottom: &Drawn, needle: &str) -> Option<Vec<[f32; 4]>> {
    let (top_flat, top_map) = squash_map(&top.text);
    let (bottom_flat, bottom_map) = squash_map(&bottom.text);
    if top_flat.is_empty() || bottom_flat.is_empty() {
        return None;
    }
    let flat = format!("{top_flat}{bottom_flat}");
    let start = unique_index(&flat, needle)?;
    let end = start + needle.chars().count();
    if start >= top_flat.chars().count() || end <= top_flat.chars().count() {
        return None;
    }
    let split = top_flat.chars().count();
    let top_first = *top_map.get(start)?;
    let top_last = *top_map.last()?;
    let bottom_first = *bottom_map.first()?;
    let bottom_last = *bottom_map.get(end - split - 1)?;
    Some(vec![
        span_rect(top, top_first, top_last + 1),
        span_rect(bottom, bottom_first, bottom_last + 1),
    ])
}

fn lines_are_adjacent(top: &Drawn, bottom: &Drawn) -> bool {
    let dy = top.y - bottom.y;
    let size = top.size.max(bottom.size).max(1.0);
    dy > size * 0.4 && dy < size * 2.2 && (top.x - bottom.x).abs() < 40.0
}

fn unique_index(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let first = haystack.find(needle)?;
    if haystack[first + needle.len()..].contains(needle) {
        return None;
    }
    Some(haystack[..first].chars().count())
}

fn span_rect(line: &Drawn, start: usize, end: usize) -> [f32; 4] {
    let x0 = char_x(line, start);
    let x1 = char_x(line, end).max(x0 + line.size * 0.3);
    [x0, line.y - line.size * 0.22, x1, line.y + line.size * 0.85]
}

fn char_x(line: &Drawn, index: usize) -> f32 {
    let mut x = line.x;
    let limit = index.min(line.widths.len());
    for i in 0..limit {
        x += line.widths[i];
        if i < line.gaps.len() {
            x += line.gaps[i];
        }
    }
    x
}

fn split_span(span: [f32; 4], links: &[LinkRef]) -> Vec<[f32; 4]> {
    let union = union_rect(links);
    let width = (union[2] - union[0]).max(0.1);
    let span_w = span[2] - span[0];
    links
        .iter()
        .map(|link| {
            let t0 = ((link.rect[0] - union[0]) / width).clamp(0.0, 1.0);
            let t1 = ((link.rect[2] - union[0]) / width).clamp(t0, 1.0);
            [
                span[0] + t0 * span_w,
                span[1],
                (span[0] + t1 * span_w).max(span[0] + t0 * span_w + 1.0),
                span[3],
            ]
        })
        .collect()
}

fn glyphs_text(glyphs: &[&Glyph]) -> String {
    let mut out = String::new();
    let mut prev: Option<&Glyph> = None;
    for glyph in glyphs {
        if let Some(prev) = prev {
            let gap = glyph.bbox[0].min(glyph.matrix[4]) - prev.bbox[2].max(prev.matrix[4]);
            let size = prev.font_size.max(glyph.font_size).max(1.0);
            let blank = glyph.unicode.chars().all(|ch| ch.is_whitespace());
            if gap > size * 0.18 && !blank {
                out.push(' ');
            }
        }
        out.push_str(&glyph.unicode);
        prev = Some(glyph);
    }
    out
}

fn squash_text(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn squash_map(text: &str) -> (String, Vec<usize>) {
    let mut flat = String::new();
    let mut map = Vec::new();
    for (index, ch) in text.chars().enumerate() {
        if ch.is_whitespace() {
            continue;
        }
        flat.push(ch);
        map.push(index);
    }
    (flat, map)
}

fn cluster_links(links: &[LinkRef]) -> Vec<Vec<LinkRef>> {
    let mut ordered = links.to_vec();
    ordered.sort_by(|a, b| {
        b.rect[1]
            .total_cmp(&a.rect[1])
            .then(a.rect[0].total_cmp(&b.rect[0]))
    });
    let mut clusters: Vec<Vec<LinkRef>> = Vec::new();
    for link in ordered {
        let join = clusters.last().is_some_and(|cluster| {
            let anchor = cluster.last().unwrap();
            let cy = (link.rect[1] + link.rect[3]) * 0.5;
            let ay = (anchor.rect[1] + anchor.rect[3]) * 0.5;
            (cy - ay).abs() < 3.0 && link.rect[0] <= anchor.rect[2] + 18.0
        });
        if join {
            clusters.last_mut().unwrap().push(link);
        } else {
            clusters.push(vec![link]);
        }
    }
    clusters
}

fn union_rect(links: &[LinkRef]) -> [f32; 4] {
    let mut rect = links[0].rect;
    for link in &links[1..] {
        rect[0] = rect[0].min(link.rect[0]);
        rect[1] = rect[1].min(link.rect[1]);
        rect[2] = rect[2].max(link.rect[2]);
        rect[3] = rect[3].max(link.rect[3]);
    }
    rect
}

/// Link rectangles are about one em tall and nearly touch the line below.
/// A raw bbox overlap mixes those lines and the citation no longer matches.
fn glyph_under_link(glyph: &Glyph, rect: [f32; 4]) -> bool {
    let size = glyph.font_size.max(1.0);
    let y = glyph.matrix[5];
    if y < rect[1] - size * 0.35 || y > rect[3] + size * 0.15 {
        return false;
    }
    let left = glyph.bbox[0].min(glyph.matrix[4]);
    let right = glyph.bbox[2].max(glyph.matrix[4]);
    left < rect[2] + 1.0 && right > rect[0] - 1.0
}

fn link_ref(doc: &Document, obj: &Object) -> Option<LinkRef> {
    let id = obj.as_reference().ok()?;
    let dict = doc.get_dictionary(id).ok()?;
    let subtype = dict
        .get(b"Subtype")
        .ok()
        .and_then(|obj| obj.as_name().ok())?;
    if subtype != b"Link" {
        return None;
    }
    let rect = rect_from_object(doc, dict.get(b"Rect").ok()?)?;
    Some(LinkRef { id, rect })
}

fn load_annots(doc: &Document, page_id: ObjectId) -> Option<(Option<ObjectId>, Vec<Object>)> {
    let obj = doc.get_dictionary(page_id).ok()?.get(b"Annots").ok()?;
    match obj {
        Object::Reference(id) => {
            let arr = doc.get_object(*id).ok()?.as_array().ok()?.clone();
            Some((Some(*id), arr))
        }
        Object::Array(arr) => Some((None, arr.clone())),
        _ => None,
    }
}

fn store_annots(doc: &mut Document, page_id: ObjectId, holder: Option<ObjectId>, arr: Vec<Object>) {
    if let Some(id) = holder {
        if let Ok(object) = doc.get_object_mut(id) {
            if let Ok(existing) = object.as_array_mut() {
                *existing = arr;
                return;
            }
        }
    }
    let Ok(page) = doc.get_object_mut(page_id) else {
        return;
    };
    let Ok(dict) = page.as_dict_mut() else {
        return;
    };
    if arr.is_empty() {
        dict.remove(b"Annots");
    } else {
        dict.set("Annots", arr);
    }
}

fn set_annot_rect(doc: &mut Document, id: ObjectId, rect: [f32; 4]) {
    let Ok(object) = doc.get_object_mut(id) else {
        return;
    };
    let Ok(dict) = object.as_dict_mut() else {
        return;
    };
    dict.set("Rect", rect_objects(rect));
    dict.remove(b"QuadPoints");
}

fn clone_link(doc: &mut Document, source: ObjectId, rect: [f32; 4]) -> Option<ObjectId> {
    let mut dict = doc.get_object(source).ok()?.as_dict().ok()?.clone();
    dict.set("Rect", rect_objects(rect));
    dict.remove(b"QuadPoints");
    Some(doc.add_object(dict))
}

/// `CIDFontType0C` must be a bare CFF program. An OpenType (`OTTO`) wrapper
/// makes Poppler warn "Mismatch between font type and embedded font file".
/// RPTCJK is already stored as bare CFF; this rewrites source fonts such as
/// the arXiv margin stamp, including copies imported for a bilingual page.
fn unwrap_opentype_cid_fonts(doc: &mut Document) {
    let ids: Vec<ObjectId> = doc.objects.keys().copied().collect();
    for id in ids {
        let Some(bytes) = doc.get_object(id).ok().and_then(|object| {
            let stream = object.as_stream().ok()?;
            let subtype = stream
                .dict
                .get(b"Subtype")
                .ok()
                .and_then(|obj| obj.as_name().ok());
            if subtype != Some(b"CIDFontType0C".as_slice()) {
                return None;
            }
            Some(
                stream
                    .decompressed_content()
                    .unwrap_or_else(|_| stream.content.clone()),
            )
        }) else {
            continue;
        };
        if bytes.len() < 12 || &bytes[0..4] != b"OTTO" {
            continue;
        }
        let Some(cff) = sfnt_table(&bytes, b"CFF ") else {
            continue;
        };
        if cff.len() < 4 || cff[0] != 1 {
            continue;
        }
        let Some(stream) = doc
            .get_object_mut(id)
            .ok()
            .and_then(|object| object.as_stream_mut().ok())
        else {
            continue;
        };
        stream.dict.set("Subtype", "CIDFontType0C");
        stream.set_plain_content(cff);
    }
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
    use crate::layout::{
        cjk_indent_ems, fit_cjk_block, is_cjk_body, justify_gaps, measure, wrap_text, CjkMeasure,
    };
    use crate::translate::{
        translate_extraction, BilingualLayout, OutputMode, TranslateOptions, TranslateReport,
        TranslatedSegment, Translator,
    };
    use lopdf::dictionary;

    #[test]
    fn a_planck_constant_stands_in_for_italic_h() {
        assert_eq!(math_plain('\u{210E}'), Some('h'));
        assert_eq!(math_plain('𝑑'), Some('d'));
        assert_eq!(math_plain('\u{1D719}'), Some('φ'));
        assert_eq!(math_plain('\u{1D715}'), Some('d'));
        assert_eq!(latin_base('ř'), Some('r'));
        assert_eq!(coverage_fallbacks('\u{2308}'), &['[']);
        assert_eq!(coverage_fallbacks('\u{2309}'), &[']']);
    }

    #[test]
    fn an_ocw_aside_with_a_year_lays_out() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/cache/ocw-18-225-lec01.pdf");
        if !path.is_file() {
            return;
        }
        let pdf = PdfDocument::open(&path).unwrap();
        let extraction = pdf.extract();
        let seg = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let segment = seg
            .segments
            .iter()
            .find(|item| item.text.contains("Additive combinatorics describes"))
            .expect("paragraph");
        let translated = "加性组合学描述了一个快速发展的数学领域，其动机源于关于整数加法和乘法的简单问题。（“加性组合学”这一名称在2000年代变得流行，当时该领域因高尔斯、格林、陶哲轩等人的开创性工作而迅速爆发；此前该领域更常被称为“组合数论”。）加性组合学中的问题和方法深刻且影响深远，连接了数学的许多不同领域，如图论、调和分析、遍历理论、离散几何和模型论。";
        let mut points: Vec<u32> = translated.chars().map(|ch| ch as u32).collect();
        points.sort_unstable();
        points.dedup();
        let font = subset_for_style(
            FaceStyle {
                serif: true,
                bold: false,
                italic: false,
            },
            &points,
            &FontSources::default(),
        )
        .expect("font");
        let glyphs: Vec<&Glyph> = segment
            .glyph_ids
            .iter()
            .filter_map(|id| extraction.glyphs.iter().find(|glyph| glyph.id == *id))
            .collect();
        let drawn = layout_segment(
            &glyphs,
            translated,
            &font,
            &extraction,
            false,
            "RPT",
            0.0,
            CjkMeasure::resolve(0.0, 0.0),
        )
        .expect("paragraph fits");
        assert!(
            drawn.len() <= 8,
            "aside still exploded into {} lines",
            drawn.len()
        );
    }

    #[test]
    fn a_similar_to_tilde_has_an_ascii_stand_in() {
        assert_eq!(coverage_fallbacks('\u{223C}'), &['~']);
    }

    struct InkRow {
        y: f32,
        text: String,
        glyphs: Vec<InkSpot>,
    }

    struct InkSpot {
        text: String,
        x: f32,
        advance: f32,
        skew: f32,
    }

    fn ink_glyph(id: u32, x: f32, y: f32, size: f32, text: &str) -> Glyph {
        Glyph {
            id,
            page_index: 0,
            unicode: text.into(),
            unmapped: false,
            char_code: vec![b'A'],
            gid: None,
            font_resource: "F1".into(),
            font_name: "Times-Roman".into(),
            font_object: None,
            font_size: size,
            matrix: [size, 0.0, 0.0, size, x, y],
            bbox: [x, y - size * 0.2, x + size * 0.5, y + size * 0.8],
            advance: [size * 0.5, 0.0],
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
    fn cmyk_fill_is_written_as_cmyk() {
        let ops = color_ops(&Color::cmyk(0.0, 0.0, 0.0, 0.3));
        assert_eq!(ops, "0.000 0.000 0.000 0.300 k");
    }

    #[test]
    fn a_cover_subtitle_clips_to_the_panel_fill() {
        let text = "With LangChain, LangGraph and MCP";
        let primary = "With LangChain, LangGraph ";
        let mut glyphs = Vec::new();
        let mut x = 43.2f32;
        let mut panel = 0.0f32;
        for (index, ch) in text.chars().enumerate() {
            let width = if ch == ' ' { 3.6 } else { 6.6 };
            let mut item = ink_glyph(index as u32, x, 330.5, 18.5, &ch.to_string());
            item.bbox = [x, 326.0, x + width, 345.0];
            item.advance = [width, 0.0];
            if index < primary.chars().count() {
                item.fill_color = Color::cmyk(0.0, 0.0, 0.0, 0.3);
                panel = x + width;
            } else {
                item.fill_color = Color::cmyk(0.0, 0.0, 0.0, 0.5);
            }
            x += width;
            glyphs.push(item);
        }
        let refs: Vec<&Glyph> = glyphs.iter().collect();
        let lines = ink_lines(&refs);
        assert_eq!(lines.len(), 1);
        let edge = display_panel_right(&lines[0]).expect("panel edge");
        assert!((edge - panel).abs() < 0.2, "edge {edge} panel {panel}");
        let same: Vec<Glyph> = glyphs
            .iter()
            .map(|glyph| {
                let mut copy = glyph.clone();
                copy.fill_color = Color::cmyk(0.0, 0.0, 0.0, 0.3);
                copy
            })
            .collect();
        let refs: Vec<&Glyph> = same.iter().collect();
        let lines = ink_lines(&refs);
        assert!(display_panel_right(&lines[0]).is_none());
    }

    #[test]
    fn a_subentry_wrap_keeps_a_latin_word_intact() {
        let text = "The Model Context Protocol";
        let mut glyphs = std::collections::HashMap::new();
        for (id, ch) in (1u16..).zip(text.chars().chain("310".chars())) {
            glyphs.insert(ch as u32, (id, 600));
        }
        let font = crate::font::SubsetFont {
            bytes: Vec::new(),
            units_per_em: 1000,
            glyphs,
        };
        let lines = title_lines_with_page(text, "310", 72.0, 10.0, &font, 5.0).expect("wrap");
        let joined = lines.join("\n");
        assert!(
            !joined.contains("Protoc\n") && !lines.iter().any(|line| line == "ol"),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("Protocol")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_drop_cap_does_not_merge_the_next_baseline() {
        let mut glyphs = vec![ink_glyph(0, 126.0, 402.0, 48.0, "T")];
        let mut x = 154.0;
        for (index, ch) in "his opening part".chars().enumerate() {
            let width = if ch == ' ' { 3.0 } else { 5.2 };
            let mut item = ink_glyph(1 + index as u32, x, 402.0, 10.0, &ch.to_string());
            item.bbox = [x, 400.0, x + width, 410.0];
            x += width;
            glyphs.push(item);
        }
        let mut x = 126.0;
        let base = glyphs.len() as u32;
        for (index, ch) in "explore the rest".chars().enumerate() {
            let width = if ch == ' ' { 3.0 } else { 5.2 };
            let mut item = ink_glyph(base + index as u32, x, 389.0, 10.0, &ch.to_string());
            item.bbox = [x, 387.0, x + width, 397.0];
            x += width;
            glyphs.push(item);
        }
        let refs: Vec<&Glyph> = glyphs.iter().collect();
        let lines = ink_lines(&refs);
        assert_eq!(
            lines.len(),
            2,
            "{:?}",
            lines.iter().map(|line| line.y).collect::<Vec<_>>()
        );
        assert!((lines[0].y - 402.0).abs() < 0.5);
        assert!((lines[1].y - 389.0).abs() < 0.5);
    }

    #[test]
    fn a_script_cluster_above_the_line_is_not_its_own_baseline() {
        let mut glyphs = Vec::new();
        let mut x = 70.0f32;
        for (index, ch) in "bedding space and the head dimension".chars().enumerate() {
            let width = if ch == ' ' { 3.0 } else { 5.4 };
            let mut item = ink_glyph(index as u32, x, 538.0, 10.9, &ch.to_string());
            item.bbox = [x, 536.0, x + width, 547.0];
            x += width;
            glyphs.push(item);
        }
        let base = glyphs.len() as u32;
        for (index, (ch, sx)) in [('N', x - 28.0), ('×', x - 20.0), ('N', x - 12.0)]
            .into_iter()
            .enumerate()
        {
            let mut item = ink_glyph(base + index as u32, sx, 542.0, 7.3, &ch.to_string());
            item.font_name = "XCharterMathMI".into();
            item.bbox = [sx, 540.5, sx + 6.0, 548.0];
            glyphs.push(item);
        }
        let refs: Vec<&Glyph> = glyphs.iter().collect();
        let lines = ink_lines(&refs);
        assert_eq!(
            lines.len(),
            1,
            "script ys {:?}",
            lines.iter().map(|line| line.y).collect::<Vec<_>>()
        );
        assert!((lines[0].y - 538.0).abs() < 1.0, "{}", lines[0].y);
    }

    #[test]
    fn affiliation_marks_are_superscripts_and_years_are_not() {
        let author = superscript_mask("Zhang1,2 Lee");
        assert!(author[5] && author[7], "{author:?}");
        assert!(author.iter().filter(|flag| **flag).count() == 2);
        let both = superscript_mask("1,2Singapore");
        assert!(both[0] && both[2], "{both:?}");
        let cjk = superscript_mask("张轩1 1新加坡");
        assert!(cjk[2] && cjk[4], "{cjk:?}");
        let chapter = superscript_mask("第4章从线性链过渡");
        assert!(chapter.iter().all(|flag| !flag), "{chapter:?}");
        let spaced = superscript_mask("第4 章构建");
        assert!(spaced.iter().all(|flag| !flag), "{spaced:?}");
        let range = superscript_mask("第1–2章概述");
        assert!(range.iter().all(|flag| !flag), "{range:?}");
        assert!(superscript_mask("Anno 2024").iter().all(|flag| !flag));
        assert!(superscript_mask("9.2% and 16K").iter().all(|flag| !flag));
        let star = superscript_mask("*Equal Contribution");
        assert!(star[0], "{star:?}");
        assert!(star.iter().skip(1).all(|flag| !flag));
        let raised = superscript_mask("Zhang∗1,2 Lee");
        assert!(raised[5] && raised[6] && raised[8], "{raised:?}");
        assert_eq!(raised.iter().filter(|flag| **flag).count(), 3);
        let cjk_star = superscript_mask("张轩∗1");
        assert!(cjk_star[2] && cjk_star[3], "{cjk_star:?}");
        assert!(superscript_mask("定理8.18").iter().all(|flag| !flag));
        assert!(superscript_mask("学习率0.001").iter().all(|flag| !flag));
        assert!(superscript_mask("n ≥ 2").iter().all(|flag| !flag));
        assert!(superscript_mask("研究1) 初始化").iter().all(|flag| !flag));
        assert!(superscript_mask("LayoutLMv3").iter().all(|flag| !flag));
    }

    #[test]
    fn author_marks_keep_the_source_rise_and_a_year_stays_on_the_baseline() {
        let mut doc = Document::with_version("1.4");
        doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        let pages_id = doc.new_object_id();
        let body = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        });
        let mut fonts = lopdf::Dictionary::new();
        fonts.set("F1", body);
        let mut resources = lopdf::Dictionary::new();
        resources.set("Font", fonts);
        let mut ops = String::from("BT\n");
        let mut x = 72.0f32;
        for ch in ['Z', 'h', 'a', 'n', 'g'] {
            ops.push_str(&format!("/F1 10 Tf 1 0 0 1 {x:.1} 700 Tm ({ch}) Tj\n"));
            x += 8.0;
        }
        ops.push_str(&format!("/F1 6 Tf 1 0 0 1 {x:.1} 703.5 Tm (1) Tj\n"));
        x += 8.0;
        for ch in ['L', 'e', 'e'] {
            ops.push_str(&format!("/F1 10 Tf 1 0 0 1 {x:.1} 700 Tm ({ch}) Tj\n"));
            x += 8.0;
        }
        x = 72.0;
        for ch in "Anno 2024".chars() {
            ops.push_str(&format!("/F1 10 Tf 1 0 0 1 {x:.1} 660 Tm ({ch}) Tj\n"));
            x += 8.0;
        }
        ops.push_str("ET\n");
        let content = ops.into_bytes();
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

        let mut pdf = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = pdf.extract();
        let author_ids: Vec<u32> = extraction
            .glyphs
            .iter()
            .filter(|glyph| glyph.matrix[5] > 690.0)
            .map(|glyph| glyph.id)
            .collect();
        let year_ids: Vec<u32> = extraction
            .glyphs
            .iter()
            .filter(|glyph| glyph.matrix[5] < 690.0)
            .map(|glyph| glyph.id)
            .collect();
        assert!(author_ids.len() > 5 && year_ids.len() > 4);
        let report = TranslateReport {
            segments: vec![
                TranslatedSegment {
                    id: 0,
                    page_index: 0,
                    glyph_ids: author_ids,
                    source: "Zhang1 Lee".into(),
                    translated: "Author1 Lee".into(),
                },
                TranslatedSegment {
                    id: 1,
                    page_index: 0,
                    glyph_ids: year_ids,
                    source: "Anno 2024".into(),
                    translated: "Year 2024".into(),
                },
            ],
            calls: 0,
            cache_hits: 0,
        };
        let font = box_ttf(
            &"Author1 LeeYear 2024X"
                .chars()
                .map(|ch| ch as u32)
                .collect::<Vec<_>>(),
        );
        pdf.rewrite(
            &mut extraction,
            &report,
            &RewriteOptions {
                font_bytes: Some(font),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let saved = pdf.save_bytes().unwrap();
        let again = PdfDocument::open_bytes(&saved).unwrap().extract();
        let dumped: Vec<_> = again
            .glyphs
            .iter()
            .map(|glyph| {
                format!(
                    "{} s{:.2} y{:.1}",
                    glyph.unicode, glyph.font_size, glyph.matrix[5]
                )
            })
            .collect();
        let digit = again
            .glyphs
            .iter()
            .find(|glyph| glyph.unicode == "1")
            .unwrap_or_else(|| panic!("author mark missing in {dumped:?}"));
        let name = again
            .glyphs
            .iter()
            .find(|glyph| glyph.unicode == "A")
            .expect("author name");
        assert!(
            digit.font_size < name.font_size * 0.85,
            "mark {} should be smaller than name {}",
            digit.font_size,
            name.font_size
        );
        assert!(
            digit.matrix[5] > name.matrix[5] + name.font_size * 0.2,
            "mark y {} should rise above name y {}",
            digit.matrix[5],
            name.matrix[5]
        );
        let year: Vec<_> = again
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode.chars().all(|ch| ch.is_ascii_digit()))
            .filter(|glyph| glyph.matrix[5] < name.matrix[5] - 5.0)
            .collect();
        assert!(!year.is_empty(), "year digits missing");
        assert!(
            year.iter()
                .all(|glyph| (glyph.font_size - name.font_size).abs() < 0.4),
            "year should stay at body size: {:?}",
            year.iter().map(|glyph| glyph.font_size).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_star_operator_uses_an_ascii_asterisk_when_the_face_lacks_it() {
        let font = box_ttf(&[0x4E2D, b'*' as u32]);
        let subset = subset_ttf(&font, &cover_with_fallbacks("中∗".chars())).unwrap();
        assert_eq!(substitute_covered("中∗", &subset), "中*");
        assert_eq!(substitute_covered("中†", &subset), "中†");
        let star = subset_ttf(&font, &cover_with_fallbacks("中⋆".chars())).unwrap();
        assert_eq!(substitute_covered("中⋆", &star), "中*");
        assert!(cover_with_fallbacks("∗".chars()).contains(&('*' as u32)));
    }

    #[test]
    fn a_missing_name_mark_or_bullet_uses_a_covered_stand_in() {
        let mut glyphs = HashMap::new();
        for (index, ch) in "中文e■→Mly1".chars().enumerate() {
            glyphs.insert(ch as u32, (index as u16, 500));
        }
        let font = SubsetFont {
            bytes: Vec::new(),
            units_per_em: 1000,
            glyphs,
        };
        assert_eq!(substitute_covered("中ę文", &font), "中e文");
        assert_eq!(substitute_covered("\u{f0a1}中", &font), "■中");
        assert_eq!(substitute_covered("（\u{27a5}）", &font), "（→）");
        assert_eq!(substitute_covered("X\u{1D440} \u{1D706}", &font), "XM l");
        assert_eq!(substitute_covered("y\u{2081}", &font), "y1");
        assert_eq!(coverage_fallbacks('✅'), &['✓', '*']);
        assert_eq!(coverage_fallbacks('👽'), &['•', '*']);
        let mut icons = HashMap::new();
        for (index, ch) in "已杀死•✓×*file".chars().enumerate() {
            icons.insert(ch as u32, (index as u16, 500));
        }
        let icons = SubsetFont {
            bytes: Vec::new(),
            units_per_em: 1000,
            glyphs: icons,
        };
        assert_eq!(substitute_covered("✅已杀死👽", &icons), "✓已杀死•");
        assert_eq!(substitute_covered("ﬁle", &icons), "file");
    }

    #[test]
    fn a_chapter_number_between_han_stays_ascii_and_is_not_a_superscript() {
        let mut glyphs = HashMap::new();
        for (index, ch) in "第4章构建".chars().enumerate() {
            glyphs.insert(ch as u32, (index as u16, 1000));
        }
        let font = SubsetFont {
            bytes: Vec::new(),
            units_per_em: 1000,
            glyphs,
        };
        let mask = superscript_mask("第4章构建");
        let (text, flags) = fullwidth_han_digits("第4章构建", &mask, &font);
        assert_eq!(text, "第4章构建");
        assert!(flags.iter().all(|flag| !flag), "{flags:?}");
        let wide = "邮政信箱761©2026至少15%第11–14章20 鲍德温路";
        let (kept, _) = fullwidth_han_digits(wide, &vec![false; wide.chars().count()], &font);
        assert_eq!(kept, wide, "digits stay ASCII");
        for sample in ["第8–10章", "章节3–5）", "第1–2章", "第 4 章"] {
            let mask = superscript_mask(sample);
            assert!(
                mask.iter().all(|flag| !flag),
                "{sample} raised a digit: {mask:?}"
            );
            let (drawn, _) = fullwidth_han_digits(sample, &mask, &font);
            assert_eq!(drawn, sample, "{sample}");
            assert!(
                !drawn
                    .chars()
                    .any(|ch| ('\u{FF10}'..='\u{FF19}').contains(&ch)),
                "{drawn}"
            );
        }
        let author = superscript_mask("张轩1");
        let (name, name_flags) = fullwidth_han_digits("张轩1", &author, &font);
        assert_eq!(name, "张轩1");
        assert!(name_flags[2], "{name_flags:?}");
    }

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

    fn math_glyph(id: u32, x: f32, y: f32, text: &str, math: bool) -> Glyph {
        Glyph {
            id,
            page_index: 0,
            unicode: text.into(),
            unmapped: false,
            char_code: vec![b'A'],
            gid: None,
            font_resource: "F1".into(),
            font_name: if math {
                "CMMI10".into()
            } else {
                "NimbusRomNo9L-Regu".into()
            },
            font_object: None,
            font_size: 10.0,
            matrix: [10.0, 0.0, 0.0, 10.0, x, y],
            bbox: [x, y, x + 6.0, y + 8.0],
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
    fn a_short_italic_prefix_does_not_skew_the_paragraph() {
        let mut mixed = Vec::new();
        for (index, ch) in "Note".chars().enumerate() {
            let mut glyph = math_glyph(
                index as u32,
                index as f32 * 6.0,
                400.0,
                &ch.to_string(),
                false,
            );
            glyph.font_name = "Times-Italic".into();
            mixed.push(glyph);
        }
        for (index, ch) in "showed that the bound holds".chars().enumerate() {
            let mut glyph = math_glyph(
                20 + index as u32,
                40.0 + index as f32 * 6.0,
                400.0,
                &ch.to_string(),
                false,
            );
            glyph.font_name = "Times-Roman".into();
            mixed.push(glyph);
        }
        let refs: Vec<&Glyph> = mixed.iter().collect();
        let style = paragraph_style(&refs);
        assert!(!style.italic && !style.bold, "{style:?}");

        let mut italic_only = Vec::new();
        for (index, ch) in "The bound is strict".chars().enumerate() {
            if ch == ' ' {
                continue;
            }
            let mut glyph = math_glyph(
                index as u32,
                index as f32 * 6.0,
                400.0,
                &ch.to_string(),
                false,
            );
            glyph.font_name = "Times-Italic".into();
            italic_only.push(glyph);
        }
        let refs: Vec<&Glyph> = italic_only.iter().collect();
        assert!(
            paragraph_style(&refs).italic,
            "{:?}",
            paragraph_style(&refs)
        );
    }

    #[test]
    fn a_subscript_in_a_formula_hole_is_not_a_second_column() {
        let line = vec![
            math_glyph(0, 72.0, 400.0, "L", false),
            math_glyph(1, 80.0, 400.0, "=", false),
            math_glyph(2, 88.0, 400.0, "E", false),
            math_glyph(3, 160.0, 400.0, "v", false),
        ];
        let refs: Vec<&Glyph> = line.iter().collect();
        assert!(
            segment_crosses_column(&refs, &refs, 10.0),
            "an empty hole is still a gap"
        );
        let sub = math_glyph(4, 120.0, 396.0, "t", true);
        let mut all = line.clone();
        all.push(sub);
        let refs: Vec<&Glyph> = all.iter().collect();
        let baseline: Vec<&Glyph> = refs[..4].to_vec();
        assert!(
            !segment_crosses_column(&baseline, &refs, 10.0),
            "the subscript fills the formula hole"
        );
    }

    #[test]
    fn an_empty_gutter_is_still_a_second_column() {
        let line = [
            math_glyph(0, 72.0, 400.0, "w", false),
            math_glyph(1, 80.0, 400.0, "e", false),
            math_glyph(2, 320.0, 400.0, "t", false),
            math_glyph(3, 328.0, 400.0, "o", false),
        ];
        let refs: Vec<&Glyph> = line.iter().collect();
        assert!(segment_crosses_column(&refs, &refs, 10.0));
    }

    #[test]
    fn a_one_line_equation_with_a_few_words_stays_a_formula() {
        let mut glyphs = Vec::new();
        let words = "and define the bound";
        for (index, ch) in words.chars().enumerate() {
            glyphs.push(math_glyph(
                index as u32,
                72.0 + index as f32 * 6.0,
                400.0,
                &ch.to_string(),
                false,
            ));
        }
        for index in 0..10 {
            glyphs.push(math_glyph(
                100 + index,
                220.0 + index as f32 * 8.0,
                400.0,
                "x",
                true,
            ));
        }
        assert!(
            !prose_carries_inline_math(&glyphs.iter().collect::<Vec<_>>()),
            "the equation should stay original"
        );
        let mut sentence = Vec::new();
        let mut x = 72.0f32;
        let mut id = 0u32;
        for word in ["Applying", "this", "with"] {
            for ch in word.chars() {
                sentence.push(math_glyph(id, x, 500.0, &ch.to_string(), false));
                id += 1;
                x += 5.0;
            }
            x += 3.2;
        }
        let base = sentence.len() as u32;
        for (index, ch) in ["A", "Y", "B", "C"].into_iter().enumerate() {
            sentence.push(math_glyph(
                base + index as u32,
                200.0 + index as f32 * 12.0,
                500.0,
                ch,
                true,
            ));
        }
        for index in 0..4 {
            let mut sub = math_glyph(
                base + 10 + index,
                206.0 + index as f32 * 12.0,
                498.4,
                "k",
                true,
            );
            sub.font_size = 7.0;
            sentence.push(sub);
        }
        assert!(
            prose_carries_inline_math(&sentence.iter().collect::<Vec<_>>()),
            "a sentence that shares its baseline with subscripts is still prose"
        );
        let mut tail = Vec::new();
        for (index, ch) in "to this becomes".chars().enumerate() {
            tail.push(math_glyph(
                index as u32,
                72.0 + index as f32 * 5.0,
                460.0,
                &ch.to_string(),
                false,
            ));
        }
        let mut sub = math_glyph(40, 160.0, 458.5, "k", true);
        sub.font_size = 7.0;
        tail.push(sub);
        assert!(
            prose_carries_inline_math(&tail.iter().collect::<Vec<_>>()),
            "the short continuation with a subscript is still prose"
        );
        let mut loss = Vec::new();
        for (index, ch) in "Lbbox Lgiou GIoU".chars().enumerate() {
            loss.push(math_glyph(
                index as u32,
                72.0 + index as f32 * 6.0,
                300.0,
                &ch.to_string(),
                false,
            ));
        }
        let mut sub = math_glyph(80, 90.0, 298.4, "i", true);
        sub.font_size = 7.0;
        loss.push(sub);
        assert!(
            !prose_carries_inline_math(&loss.iter().collect::<Vec<_>>()),
            "identifier subscripts stay with the equation"
        );
        let mut sentence = Vec::new();
        let body = "The method uses a bound and then compares the two estimates carefully.";
        for (index, ch) in body.chars().enumerate() {
            sentence.push(math_glyph(
                index as u32,
                72.0 + (index % 40) as f32 * 6.0,
                500.0 - (index / 40) as f32 * 14.0,
                &ch.to_string(),
                false,
            ));
        }
        sentence.push(math_glyph(400, 200.0, 500.0, "x", true));
        assert!(
            prose_carries_inline_math(&sentence.iter().collect::<Vec<_>>()),
            "a sentence with one symbol is still prose"
        );
    }

    #[test]
    fn a_sentence_with_one_math_glyph_is_still_translated() {
        let Some(font) = crate::font::load_cjk_font() else {
            return;
        };
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
        let content = b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (The method is ) Tj /F2 12 Tf 1 0 0 1 156 700 Tm (x) Tj /F1 12 Tf 1 0 0 1 170 700 Tm (similar to our approach.) Tj /F2 12 Tf 1 0 0 1 72 640 Tm (xy) Tj ET".to_vec();
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

        struct PaintSentence;
        impl Translator for PaintSentence {
            fn complete(&self, _system: &str, user: &str) -> Result<String> {
                let payload: serde_json::Value = serde_json::from_str(user).unwrap();
                let translations: Vec<serde_json::Value> = payload["segments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|seg| {
                        let text = seg["text"].as_str().unwrap_or("");
                        let translated = if text.contains("method") {
                            "该方法与我们的做法相似。"
                        } else {
                            text
                        };
                        serde_json::json!({"id": seg["id"], "text": translated})
                    })
                    .collect();
                Ok(serde_json::json!({"translations": translations}).to_string())
            }
        }

        let mut opened = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = opened.extract();
        let report = translate_extraction(
            &mut extraction,
            &TranslateOptions::default(),
            &PaintSentence,
        )
        .unwrap();
        opened
            .rewrite(
                &mut extraction,
                &report,
                &RewriteOptions {
                    font_bytes: Some(font),
                    ..RewriteOptions::default()
                },
            )
            .unwrap();
        let method = extraction
            .glyphs
            .iter()
            .find(|glyph| glyph.unicode == "m")
            .unwrap();
        assert!(
            matches!(method.disposition, Disposition::Rewritten { .. }),
            "{:?}",
            method.disposition
        );
        let display = extraction
            .glyphs
            .iter()
            .find(|glyph| glyph.matrix[5] < 660.0)
            .unwrap();
        assert!(
            matches!(&display.disposition, Disposition::KeptOriginal { reason } if reason == "formula" || reason == "kept"),
            "{:?}",
            display.disposition
        );
    }

    #[test]
    fn rewrite_paints_a_hand_built_report_with_no_translator() {
        let bytes = sample_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let hello = crate::segment::segment_glyphs(&extraction.glyphs)
            .into_iter()
            .find(|seg| seg.text.contains("Hello"))
            .expect("hello paragraph");
        let report = TranslateReport {
            segments: vec![TranslatedSegment {
                id: hello.id,
                page_index: hello.page_index,
                glyph_ids: hello.glyph_ids,
                source: hello.text,
                translated: "AB".into(),
            }],
            calls: 0,
            cache_hits: 0,
        };
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
        let text = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract()
            .plain_text();
        assert!(text.contains("AB"), "{text}");
        assert!(!text.contains("Hello"), "{text}");
        assert!(text.contains("References"), "{text}");
    }

    fn prose_pdf() -> Vec<u8> {
        let mut doc = Document::with_version("1.4");
        doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        let pages_id = doc.new_object_id();
        let body = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        });
        let mut fonts = lopdf::Dictionary::new();
        fonts.set("F1", body);
        let mut resources = lopdf::Dictionary::new();
        resources.set("Font", fonts);
        let content = b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Hello) Tj 1 0 0 1 72 660 Tm (RAG) Tj 1 0 0 1 72 600 Tm (This paragraph explains how agents plan work across tools and should become Chinese.) Tj ET".to_vec();
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

    fn prose_report(extraction: &Extraction, translate_hello: bool) -> TranslateReport {
        let segments = crate::segment::segment_glyphs(&extraction.glyphs)
            .into_iter()
            .map(|seg| {
                let translated = if translate_hello && seg.text.contains("Hello") {
                    "甲".into()
                } else {
                    seg.text.clone()
                };
                TranslatedSegment {
                    id: seg.id,
                    page_index: seg.page_index,
                    glyph_ids: seg.glyph_ids,
                    source: seg.text,
                    translated,
                }
            })
            .collect();
        TranslateReport {
            segments,
            calls: 0,
            cache_hits: 0,
        }
    }

    #[test]
    fn html_table_markup_is_not_an_english_body() {
        let html = "<tr><td></td><td></td><td colspan=\"4\"></td><td></td></tr><tr><td></td></tr>";
        assert!(!english_body_source(html));
        assert!(english_body_source(
            "The table structure language is usually adopted as is for the sequence tokenization."
        ));
        assert!(!english_body_source(
            "4 https://huggingface.co/datasets/piushorn/wikipedia-latex-formulas-319k"
        ));
    }

    #[test]
    fn an_echoed_english_body_fails_when_other_prose_was_translated() {
        let bytes = prose_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let report = prose_report(&extraction, true);
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
        let coverage = extraction.coverage_report();
        assert!(!coverage.complete, "{coverage:?}");
        assert!(coverage.english_body > 0, "{coverage:?}");
        let echoed = extraction.glyphs.iter().any(|glyph| {
            glyph.unicode == "T"
                && matches!(
                    &glyph.disposition,
                    Disposition::KeptOriginal { reason } if reason == "english-body"
                )
        });
        assert!(echoed, "the body paragraph should be flagged english-body");
        let proper = extraction
            .glyphs
            .iter()
            .find(|glyph| glyph.unicode == "G")
            .expect("RAG");
        assert!(
            matches!(&proper.disposition, Disposition::KeptOriginal { reason } if reason == "unchanged"),
            "a short proper noun stays unchanged: {:?}",
            proper.disposition
        );
    }

    #[test]
    fn an_identity_echo_of_english_prose_stays_complete() {
        let bytes = prose_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let report = prose_report(&extraction, false);
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
        assert!(extraction.glyphs.iter().all(|glyph| {
            !matches!(
                &glyph.disposition,
                Disposition::KeptOriginal { reason } if reason == "english-body"
            )
        }));
    }

    #[test]
    fn a_citation_stays_on_the_line_with_the_following_cjk() {
        let text = "见(Rashid et al.,2025)上评估AutoCompact分别取得通过率";
        let mut glyphs = HashMap::new();
        for (id, ch) in (1u16..).zip(text.chars()) {
            glyphs.insert(ch as u32, (id, 1000));
        }
        let font = SubsetFont {
            bytes: Vec::new(),
            units_per_em: 1000,
            glyphs,
        };
        // 22em ends at "上". The old wrapper jumped back to the space in "et al.".
        let lines = wrap_text(text, 10.0, 220.0, 220.0, &font).unwrap();
        assert!(
            lines[0].contains("et al.,2025)") && lines[0].ends_with('上'),
            "{lines:?}"
        );
        let gaps = justify_gaps(lines[0].as_str(), 10.0, 400.0, &font, true);
        assert!(
            gaps.iter().all(|gap| *gap == 0.0),
            "a short citation line must not be letter-spaced: {gaps:?}"
        );
        let cjk = "中文正文需要两端对齐";
        let mut cjk_glyphs = HashMap::new();
        for (id, ch) in (1u16..).zip(cjk.chars()) {
            cjk_glyphs.insert(ch as u32, (id, 1000));
        }
        let cjk_font = SubsetFont {
            bytes: Vec::new(),
            units_per_em: 1000,
            glyphs: cjk_glyphs,
        };
        let count = cjk.chars().count();
        let width = count as f32 * 10.0 + 2.0;
        let cjk_gaps = justify_gaps(cjk, 10.0, width, &cjk_font, true);
        assert!(
            cjk_gaps.iter().all(|gap| *gap > 0.0),
            "pure CJK slack should spread across every ideograph gap: {cjk_gaps:?}"
        );
        let mixed = "中文(Rashid)后续";
        let mut mixed_glyphs = HashMap::new();
        for (id, ch) in (1u16..).zip(mixed.chars()) {
            mixed_glyphs.insert(ch as u32, (id, 1000));
        }
        let mixed_font = SubsetFont {
            bytes: Vec::new(),
            units_per_em: 1000,
            glyphs: mixed_glyphs,
        };
        let mixed_width = measure(mixed, 10.0, &mixed_font) + 0.6;
        let mixed_gaps = justify_gaps(mixed, 10.0, mixed_width, &mixed_font, true);
        let chars: Vec<char> = mixed.chars().collect();
        for (index, gap) in mixed_gaps.iter().enumerate() {
            let latin = !is_cjk_body(chars[index]) || !is_cjk_body(chars[index + 1]);
            if latin {
                assert_eq!(*gap, 0.0, "gap {index} in {mixed}");
            }
        }
        assert!(mixed_gaps.iter().any(|gap| *gap > 0.0), "{mixed_gaps:?}");
    }

    #[test]
    fn latin_words_are_not_split_across_lines() {
        let mut glyphs = HashMap::new();
        for (id, ch) in (1u16..).zip("包括SelfCompact、SWE-Compressor和变体".chars()) {
            glyphs.insert(ch as u32, (id, 1000));
        }
        let font = SubsetFont {
            bytes: Vec::new(),
            units_per_em: 1000,
            glyphs,
        };
        // 14em fits "包括SelfCompact、" (14 chars) but not the following name.
        let lines = wrap_text(
            "包括SelfCompact、SWE-Compressor和变体",
            10.0,
            145.0,
            145.0,
            &font,
        )
        .unwrap();
        assert!(
            lines.iter().any(|line| line.contains("SWE-Compressor")),
            "{lines:?}"
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("SWE-") && !line.contains("Compressor")),
            "{lines:?}"
        );
    }

    #[test]
    fn an_unhit_mark_on_the_next_page_stays_on_that_page() {
        let font = uniform_font("中3");
        let mut home = math_glyph(0, 80.0, 500.0, "A", false);
        home.page_index = 0;
        let mut mark = math_glyph(1, 80.0, 200.0, "3", false);
        mark.page_index = 1;
        mark.bbox = [80.0, 200.0, 86.0, 210.0];
        let mut drawn = vec![Drawn {
            page: 0,
            x: 80.0,
            y: 500.0,
            size: 10.0,
            color: Color::black(),
            cids: vec![],
            resource: "RPT0".into(),
            skew: 0.0,
            text: "中".into(),
            glyph_ids: vec![0],
            widths: vec![10.0],
            gaps: vec![],
            supers: vec![],
            sup_scale: 1.0,
            sup_rise: 0.0,
        }];
        let glyphs = [&home, &mark];
        cover_unhit_clusters(
            &mut drawn,
            &glyphs,
            &font,
            "RPT0",
            0.0,
            &Color::black(),
            &[0, 1],
            &[],
        );
        let marks: Vec<_> = drawn
            .iter()
            .filter(|line| line.text.contains('3'))
            .map(|line| line.page)
            .collect();
        assert_eq!(marks, vec![1], "{marks:?}");
    }

    fn uniform_font(text: &str) -> SubsetFont {
        let mut glyphs = HashMap::new();
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
    fn a_centered_title_may_use_the_page_measure() {
        let media = [0.0, 0.0, 595.0, 842.0];
        let (left, right) = centered_heading_span(HeadingLevel::Title, 14.35, media, 180.0, 416.0)
            .expect("centered title");
        assert!(right - left > 400.0, "{left}..{right}");
        assert!(
            centered_heading_span(HeadingLevel::Title, 14.35, media, 88.0, 274.0).is_none(),
            "a left column is not a page-centered title"
        );
        assert!(
            centered_heading_span(HeadingLevel::Body, 14.35, media, 180.0, 416.0).is_none(),
            "body text keeps the ink box"
        );
    }

    #[test]
    fn chinese_body_is_smaller_with_more_open_leading() {
        let metrics = CjkMeasure::resolve(0.90, 1.60);
        assert!((metrics.body_scale - 0.90).abs() < 1e-4);
        assert!((metrics.leading_ratio - 1.60).abs() < 1e-4);
        assert!(metrics.min_leading_ratio >= 1.40);
        assert!(metrics.heading_scale > metrics.body_scale && metrics.heading_scale <= 1.0);

        let text = "中文正文应当使用更小的字号并且配上更舒适的行距才像论文";
        let font = uniform_font(text);
        // 22 ideographs. At 9pt a 90pt column holds 10, so this is three lines.
        // Two English gaps of 20pt each leave room for a 1.6em Chinese leading.
        let (lines, size, leading, _) = fit_cjk_block(
            text,
            10.0,
            metrics.body_scale,
            90.0,
            0.0,
            40.0,
            &font,
            metrics,
        )
        .unwrap();
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(
            (size - 9.0).abs() < 0.05,
            "body should start at 90% of 10pt, got {size}"
        );
        assert!(
            (leading / size - 1.60).abs() < 0.02,
            "leading {leading} / size {size}"
        );

        let (tight_lines, tight_size, tight_leading, _) = fit_cjk_block(
            text,
            10.0,
            metrics.body_scale,
            90.0,
            0.0,
            22.0,
            &font,
            metrics,
        )
        .unwrap();
        assert!(tight_lines.len() >= 2, "{tight_lines:?}");
        assert!(
            tight_size < size - 0.4,
            "a short block shrinks the size before crushing the leading: {tight_size}"
        );
        assert!(
            tight_leading / tight_size >= metrics.min_leading_ratio - 0.05,
            "leading {tight_leading} size {tight_size}"
        );

        let title = "论文标题";
        let title_font = uniform_font(title);
        let (_, title_size, _, _) = fit_cjk_block(
            title,
            16.0,
            metrics.heading_scale,
            200.0,
            0.0,
            0.0,
            &title_font,
            metrics,
        )
        .unwrap();
        assert!(
            (title_size - 16.0 * metrics.heading_scale).abs() < 0.05,
            "{title_size}"
        );

        let author = "作者甲";
        let author_font = uniform_font(author);
        let (_, author_size, _, _) = fit_cjk_block(
            author,
            10.0,
            metrics.body_scale,
            200.0,
            0.0,
            0.0,
            &author_font,
            metrics,
        )
        .unwrap();
        assert!((author_size - 9.0).abs() < 0.05, "{author_size}");
    }

    #[test]
    fn body_paragraphs_indent_two_characters_and_headings_do_not() {
        let body = "编程智能体通过长轨迹解决仓库级任务。随着任务推进，早期探索会过时。";
        assert_eq!(cjk_indent_ems(5, 10.0, false, body), 2.0);
        assert_eq!(
            cjk_indent_ems(
                1,
                10.0,
                false,
                "这是单独成段的一句完整正文，句末有句号，所以仍然应当首行缩进。",
            ),
            2.0
        );
        assert_eq!(cjk_indent_ems(1, 14.0, true, "1 引言"), 0.0);
        assert_eq!(cjk_indent_ems(1, 11.0, true, "实验设置"), 0.0);
        assert_eq!(
            cjk_indent_ems(2, 16.0, false, "AutoCompact: 学习何时压缩"),
            0.0
        );
        assert_eq!(cjk_indent_ems(3, 9.0, false, "图1：压缩流程示意"), 0.0);
        assert_eq!(
            cjk_indent_ems(1, 10.0, false, "{xuanzhang826, ltzheng01}@gmail.com"),
            0.0
        );
        assert_eq!(
            cjk_indent_ems(
                1,
                10.0,
                false,
                "■ 您知道如何创建 Python Jupyter Notebook 环境。",
            ),
            0.0
        );

        let font = uniform_font(body);
        let metrics = CjkMeasure::resolve(0.90, 1.60);
        let (lines, size, _, indent) = fit_cjk_block(
            body,
            10.0,
            metrics.body_scale,
            180.0,
            2.0,
            80.0,
            &font,
            metrics,
        )
        .unwrap();
        assert!((indent - size * 2.0).abs() < 0.05, "{indent} {size}");
        assert!(lines.len() >= 2, "{lines:?}");
        let first = measure(lines[0].as_str(), size, &font);
        let second = measure(lines[1].as_str(), size, &font);
        assert!(
            first + indent <= 180.0 + 1.0,
            "first line {first} plus indent {indent}"
        );
        assert!(
            first + size < second || first <= 180.0 - indent,
            "the first line should be the narrower one: {lines:?}"
        );
    }

    #[test]
    fn citations_and_emails_are_not_split_or_letter_spaced() {
        let text = "前文(Jimenez et al., 2024)后文继续";
        let font = uniform_font(text);
        let lines = wrap_text(text, 10.0, 220.0, 220.0, &font).unwrap();
        assert_eq!(lines[0], "前文", "{lines:?}");
        assert!(lines[1].starts_with("(Jimenez et al., 2024)"), "{lines:?}");
        let email = "联系{xuanzhang826, ltzheng01}@gmail.com谢谢";
        let email_font = uniform_font(email);
        let email_lines = wrap_text(email, 10.0, 200.0, 360.0, &email_font).unwrap();
        assert!(
            email_lines
                .iter()
                .any(|line| line.contains("{xuanzhang826, ltzheng01}@gmail.com")),
            "{email_lines:?}"
        );
        assert!(
            !email_lines
                .iter()
                .any(|line| line.contains('@') && !line.contains("gmail.com")),
            "{email_lines:?}"
        );

        let hung = "中文正文。后续";
        let hung_font = uniform_font(hung);
        let hung_lines = wrap_text(hung, 10.0, 45.0, 45.0, &hung_font).unwrap();
        assert_eq!(hung_lines[0], "中文正文。", "{hung_lines:?}");

        let cited = "评估(Rashid et al.,2025)结果很好";
        let cited_font = uniform_font(cited);
        let cited_width = measure(cited, 10.0, &cited_font) + 1.5;
        let cited_gaps = justify_gaps(cited, 10.0, cited_width, &cited_font, true);
        assert!(
            cited_gaps.iter().all(|gap| *gap == 0.0),
            "a citation line stays tight: {cited_gaps:?}"
        );
        let addr = "邮箱user@lab.org谢谢";
        let addr_font = uniform_font(addr);
        let addr_width = measure(addr, 10.0, &addr_font) + 1.5;
        let addr_gaps = justify_gaps(addr, 10.0, addr_width, &addr_font, true);
        assert!(
            addr_gaps.iter().all(|gap| *gap == 0.0),
            "an email line stays tight: {addr_gaps:?}"
        );
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
        // Paint OCR needs poppler and a chi_sim tessdata. Hosts that only
        // have the text tools still check ToUnicode and the CID font above.
        if tool_missing("pdftoppm") || tool_missing("tesseract") {
            return;
        }
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

    fn tool_missing(name: &str) -> bool {
        match std::process::Command::new(name).arg("--version").output() {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
            Err(_) => false,
            Ok(_) => false,
        }
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

    #[test]
    fn a_citation_link_moves_onto_the_rewritten_glyphs() {
        let translated = "参见(Smith et al., 2020)的结果说明";
        let bytes = citation_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let report =
            translate_extraction(&mut extraction, &TranslateOptions::default(), &MoveCitation)
                .unwrap();
        let font = box_ttf(&translated.chars().map(|ch| ch as u32).collect::<Vec<_>>());
        doc.rewrite(
            &mut extraction,
            &report,
            &RewriteOptions {
                font_bytes: Some(font),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let saved = doc.save_bytes().unwrap();
        let loaded = Document::load_mem(&saved).unwrap();
        let page_id = loaded.get_pages().into_values().next().unwrap();
        let annots = loaded
            .get_dictionary(page_id)
            .unwrap()
            .get(b"Annots")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(
            annots.len(),
            1,
            "the link over the replaced prose is removed"
        );
        let id = annots[0].as_reference().unwrap();
        let rect = rect_from_object(
            &loaded,
            loaded.get_dictionary(id).unwrap().get(b"Rect").unwrap(),
        )
        .unwrap();
        // Source citation sat near x=367. Body text is 90% of the 12pt source,
        // and the box font advance matches the 600-unit source widths, so
        // "参见" is two 6.48pt glyphs and the citation ends near x=210.
        assert!(
            rect[0] > 80.0 && rect[0] < 95.0,
            "link should sit on the citation, not the old English x: {rect:?}"
        );
        assert!(
            rect[2] > 205.0 && rect[2] < 230.0,
            "link should cover the citation and not the following Chinese: {rect:?}"
        );
        let text = PdfDocument::open_bytes(&saved)
            .unwrap()
            .extract()
            .plain_text();
        assert!(text.contains("(Smith et al., 2020)"), "{text}");
        assert!(text.contains("参见"), "{text}");
    }

    fn otto_cid_streams(doc: &Document) -> usize {
        doc.objects
            .keys()
            .filter(|id| {
                let Ok(object) = doc.get_object(**id) else {
                    return false;
                };
                let Ok(stream) = object.as_stream() else {
                    return false;
                };
                let subtype = stream
                    .dict
                    .get(b"Subtype")
                    .ok()
                    .and_then(|obj| obj.as_name().ok());
                if subtype != Some(b"CIDFontType0C".as_slice()) {
                    return false;
                }
                let bytes = stream
                    .decompressed_content()
                    .unwrap_or_else(|_| stream.content.clone());
                bytes.len() >= 4 && &bytes[0..4] == b"OTTO"
            })
            .count()
    }

    #[test]
    fn lesser_stamp_font_is_unwrapped() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/cache/arxiv-2610.03702.pdf");
        if !path.exists() {
            return;
        }
        let mut doc = Document::load(&path).unwrap();
        let before = otto_cid_streams(&doc);
        unwrap_opentype_cid_fonts(&mut doc);
        let after = otto_cid_streams(&doc);
        assert!(before > 0, "expected an OTTO CID font in the source");
        assert_eq!(after, 0, "OTTO CID fonts left: {after}");
        let mut saved = Vec::new();
        doc.save_to(&mut saved).unwrap();
        let out = std::env::temp_dir().join("rpt-unwrapped-lesser.pdf");
        std::fs::write(&out, &saved).unwrap();
        let fonts = std::process::Command::new("pdffonts")
            .arg(&out)
            .output()
            .expect("pdffonts");
        let stderr = String::from_utf8_lossy(&fonts.stderr);
        assert!(!stderr.contains("Mismatch between font type"), "{stderr}");
    }

    #[test]
    fn opentype_cid_fontfile_is_unwrapped_without_a_viewer_warning() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/ci/arxiv-2610.02163.pdf");
        if !path.exists() {
            return;
        }
        let mut doc = Document::load(&path).unwrap();
        unwrap_opentype_cid_fonts(&mut doc);
        let mut saved = Vec::new();
        doc.save_to(&mut saved).unwrap();
        let out = std::env::temp_dir().join("rpt-unwrapped-stamp.pdf");
        std::fs::write(&out, &saved).unwrap();
        let fonts = std::process::Command::new("pdffonts")
            .arg(&out)
            .output()
            .expect("pdffonts");
        let listing = String::from_utf8_lossy(&fonts.stdout);
        let stderr = String::from_utf8_lossy(&fonts.stderr);
        assert!(!stderr.contains("Mismatch between font type"), "{stderr}");
        assert!(
            !listing.contains("(OT)"),
            "CIDFontType0C must be bare CFF:\n{listing}"
        );
        assert!(
            listing.contains("NimbusRoman") && listing.contains("CID Type 0C"),
            "{listing}"
        );
        let text = std::process::Command::new("pdftotext")
            .args(["-f", "1", "-l", "1", "-enc", "UTF-8", "-q"])
            .arg(&out)
            .arg("-")
            .output()
            .expect("pdftotext");
        let extracted = String::from_utf8_lossy(&text.stdout);
        let err = String::from_utf8_lossy(&text.stderr);
        assert!(!err.contains("Mismatch between font type"), "{err}");
        assert!(
            extracted.contains("arXiv:2610.02163"),
            "unwrapping the stamp font dropped its text: {extracted}"
        );
    }

    fn citation_pdf() -> Vec<u8> {
        let mut doc = Document::with_version("1.4");
        doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        let pages_id = doc.new_object_id();
        let widths: Vec<Object> = (0..256).map(|_| Object::Integer(600)).collect();
        let font = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
            "FirstChar" => 0,
            "LastChar" => 255,
            "Widths" => widths,
        });
        let mut fonts = lopdf::Dictionary::new();
        fonts.set("F1", font);
        let mut resources = lopdf::Dictionary::new();
        resources.set("Font", fonts);
        let prose = b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX (Smith et al., 2020)) Tj ET".to_vec();
        let content_id = doc.add_object(Stream::new(dictionary! {}, prose));
        let cite = doc.add_object(dictionary! {
            "Type" => "Annot",
            "Subtype" => "Link",
            "Rect" => vec![366.into(), 688.into(), 520.into(), 716.into()],
            "Border" => vec![0.into(), 0.into(), 1.into()],
            "C" => vec![0.into(), 1.into(), 0.into()],
        });
        let filler = doc.add_object(dictionary! {
            "Type" => "Annot",
            "Subtype" => "Link",
            "Rect" => vec![72.into(), 688.into(), 300.into(), 716.into()],
            "Border" => vec![0.into(), 0.into(), 1.into()],
            "C" => vec![0.into(), 1.into(), 0.into()],
        });
        let page = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content_id,
            "Resources" => resources,
            "Annots" => vec![cite.into(), filler.into()],
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

    struct MoveCitation;
    impl Translator for MoveCitation {
        fn complete(&self, _system: &str, user: &str) -> Result<String> {
            let payload: serde_json::Value = serde_json::from_str(user).unwrap();
            let translations: Vec<serde_json::Value> = payload["segments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|seg| {
                    let text = seg["text"].as_str().unwrap_or("");
                    let translated = if text.contains('⟦') {
                        let mut marks = String::new();
                        let mut rest = text;
                        while let Some(start) = rest.find('⟦') {
                            let Some(end) = rest[start..].find('⟧') else {
                                break;
                            };
                            let stop = start + end + '⟧'.len_utf8();
                            marks.push_str(&rest[start..stop]);
                            rest = &rest[stop..];
                        }
                        format!("参见{marks}的结果说明")
                    } else {
                        text.to_string()
                    };
                    serde_json::json!({"id": seg["id"], "text": translated})
                })
                .collect();
            Ok(serde_json::json!({"translations": translations}).to_string())
        }
    }

    #[test]
    fn a_contents_title_is_translated_and_the_page_number_stays() {
        let bytes = toc_row_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let title = segmentation
            .segments
            .iter()
            .find(|seg| seg.text.contains("Introduction"))
            .expect("title");
        assert!(
            !title.text.contains('3') && !title.text.contains('.'),
            "{}",
            title.text
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let report = TranslateReport {
            segments: vec![TranslatedSegment {
                id: title.id,
                page_index: title.page_index,
                glyph_ids: title.glyph_ids.clone(),
                source: title.text.clone(),
                translated: "简介".into(),
            }],
            calls: 0,
            cache_hits: 0,
        };
        let font = box_ttf(
            &"简介3."
                .chars()
                .map(|ch| ch as u32)
                .chain(std::iter::once('.' as u32))
                .collect::<Vec<_>>(),
        );
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
        let saved = doc.save_bytes().unwrap();
        let text = PdfDocument::open_bytes(&saved)
            .unwrap()
            .extract()
            .plain_text();
        assert!(text.contains("简介"), "{text}");
        assert!(text.contains('3'), "{text}");
        assert!(!text.contains("Introduction"), "{text}");
        let painted = PdfDocument::open_bytes(&saved).unwrap().extract();
        let dot_x = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode.contains('.'))
            .map(|glyph| glyph.matrix[4])
            .fold(f32::MAX, f32::min);
        assert!(
            dot_x < 160.0,
            "leader dots stayed out by the old English title at x={dot_x}"
        );
    }

    #[test]
    fn toc_section_page_numbers_sit_after_the_title() {
        let bytes = toc_column_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let texts: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Agents") && !text.contains('2')),
            "short title should drop its page number: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("postprocessing") && !text.contains("228")),
            "long title should drop its page number: {texts:?}"
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .filter(|seg| seg.text.contains("Agents") || seg.text.contains("postprocessing"))
            .map(|seg| TranslatedSegment {
                id: seg.id,
                page_index: seg.page_index,
                glyph_ids: seg.glyph_ids.clone(),
                source: seg.text.clone(),
                translated: if seg.text.contains("Agents") {
                    "代理".into()
                } else {
                    "查询生成与检索后处理".into()
                },
            })
            .collect();
        let report = TranslateReport {
            segments,
            calls: 0,
            cache_hits: 0,
        };
        let font = box_ttf(
            &"代理查询生成与检索后处理278"
                .chars()
                .map(|ch| ch as u32)
                .collect::<Vec<_>>(),
        );
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
        let saved = doc.save_bytes().unwrap();
        let painted = PdfDocument::open_bytes(&saved).unwrap().extract();
        let mut rows: Vec<(f32, f32, f32)> = Vec::new();
        for glyph in painted.glyphs.iter().filter(|glyph| {
            glyph.unicode.chars().all(|ch| ch.is_ascii_digit()) && !glyph.unicode.is_empty()
        }) {
            let y = glyph.matrix[5];
            let left = glyph.matrix[4];
            let right = left + glyph.advance[0].max(0.0);
            if let Some(row) = rows.iter_mut().find(|row| (row.0 - y).abs() < 1.0) {
                row.1 = row.1.min(left);
                row.2 = row.2.max(right);
            } else {
                rows.push((y, left, right));
            }
        }
        assert_eq!(rows.len(), 2, "page-number rows: {rows:?}");
        rows.sort_by(|left, right| right.0.total_cmp(&left.0));
        let short = rows[0];
        let long = rows[1];
        let title_right = |needle: char, y: f32| {
            painted
                .glyphs
                .iter()
                .filter(|glyph| glyph.unicode.contains(needle) && (glyph.matrix[5] - y).abs() < 2.0)
                .map(glyph_ink_right)
                .fold(0.0f32, f32::max)
        };
        let short_title = title_right('理', short.0);
        let long_title = title_right('理', long.0);
        assert!(
            short_title > 0.0 && (short.1 - short_title) < 18.0 && short.1 + 1.0 >= short_title,
            "short page number is not inline after 代理: title_right={short_title} number={short:?}"
        );
        assert!(
            long_title > 0.0 && (long.1 - long_title) < 24.0 && long.1 + 1.0 >= long_title,
            "long page number is not inline after its title: title_right={long_title} number={long:?}"
        );
        assert!(
            long.2 - short.2 > 24.0,
            "section numbers collapsed onto one right edge: {rows:?}"
        );
    }

    #[test]
    fn inline_contents_numbers_stay_next_to_their_titles() {
        let bytes = inline_numbers_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| {
                let translated = if seg.text.contains("engines") {
                    "引擎"
                } else if seg.text.contains("chatbots") {
                    "聊天机器人"
                } else if seg.text.contains("agents") {
                    "代理"
                } else {
                    "简介"
                };
                TranslatedSegment {
                    id: seg.id,
                    page_index: seg.page_index,
                    glyph_ids: seg.glyph_ids.clone(),
                    source: seg.text.clone(),
                    translated: translated.into(),
                }
            })
            .collect();
        let font = box_ttf(
            &"引擎聊天机器人代理简介3478"
                .chars()
                .map(|ch| ch as u32)
                .collect::<Vec<_>>(),
        );
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(font),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let painted = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract();
        let mut xs: Vec<f32> = painted
            .glyphs
            .iter()
            .filter(|glyph| matches!(glyph.unicode.as_str(), "4" | "7" | "8"))
            .map(|glyph| glyph.matrix[4])
            .collect();
        xs.sort_by(|left, right| left.total_cmp(right));
        assert_eq!(xs.len(), 3, "inline numbers: {xs:?}");
        assert!(
            xs[1] - xs[0] > 12.0 && xs[2] - xs[1] > 12.0,
            "inline page numbers stacked: {xs:?}"
        );
        let section: Vec<f32> = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode == "3")
            .map(|glyph| glyph.matrix[4])
            .collect();
        assert_eq!(section.len(), 1, "section page number missing: {section:?}");
        let title_right = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode == "简")
            .map(glyph_ink_right)
            .fold(0.0f32, f32::max);
        assert!(
            section[0] + 1.0 >= title_right && section[0] - title_right < 18.0,
            "section page number x={} is not inline after 简介 at {title_right}",
            section[0]
        );
        let title_y: Vec<f32> = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode == "简")
            .map(|glyph| glyph.matrix[5])
            .collect();
        let section_y: Vec<f32> = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode == "3")
            .map(|glyph| glyph.matrix[5])
            .collect();
        assert_eq!(title_y.len(), 1, "title baseline missing: {title_y:?}");
        assert!(
            (title_y[0] - section_y[0]).abs() < 1.5,
            "section number left the title baseline: title={title_y:?} number={section_y:?}"
        );
    }

    #[test]
    fn a_bold_italic_chapter_title_keeps_a_bold_face() {
        if !std::path::Path::new("/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc").is_file() {
            return;
        }
        let bytes = contents_pdf_named(
            b"BT /F1 13 Tf \
1 0 0 1 120 520 Tm [(Introduction to AI agents) -900 (3)] TJ \
/F1 11 Tf \
1 0 0 1 120 490 Tm [(Building agents) -900 (4)] TJ \
ET",
            "NewBaskerville-BoldItalic",
        );
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| TranslatedSegment {
                id: seg.id,
                page_index: seg.page_index,
                glyph_ids: seg.glyph_ids.clone(),
                source: seg.text.clone(),
                translated: if seg.text.contains("Introduction") {
                    "AI代理和应用简介".into()
                } else if seg.text.contains("Building") {
                    "构建代理".into()
                } else {
                    seg.text.clone()
                },
            })
            .collect();
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions::default(),
        )
        .unwrap();
        let saved = doc.save_bytes().unwrap();
        let saved_text = String::from_utf8_lossy(&saved);
        assert!(
            saved_text.contains("NotoSansCJKjp-Bold") || saved_text.contains("NotoSansCJK-Bold"),
            "chapter title was not painted with the bold face"
        );
        let painted = PdfDocument::open_bytes(&saved).unwrap().extract();
        let title: Vec<_> = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode.chars().any(is_han_char))
            .filter(|glyph| glyph.unicode.contains('简') || glyph.unicode.contains('代'))
            .collect();
        assert!(
            !title.is_empty(),
            "chapter title missing: {}",
            painted.plain_text()
        );
        assert!(
            title.iter().any(|glyph| glyph.matrix[2].abs() > 0.15),
            "bold-italic chapter title lost its italic shear"
        );
        let number_left = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode.trim() == "3")
            .map(|glyph| glyph.matrix[4])
            .fold(f32::MAX, f32::min);
        let title_right = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode.contains('简'))
            .map(glyph_ink_right)
            .fold(0.0f32, f32::max);
        assert!(
            number_left.is_finite()
                && title_right > 0.0
                && number_left + 1.0 >= title_right
                && number_left - title_right < 20.0,
            "chapter page number x={number_left} is not inline after the title at {title_right}"
        );
    }

    #[test]
    fn a_wrapped_subentry_does_not_open_a_line_with_the_separator() {
        let chars: Vec<u32> = "问答引擎摘要聊天机器人应用智能体练习补充说明甲乙丙48■"
            .chars()
            .map(|ch| ch as u32)
            .collect();
        let font = subset_ttf(&box_ttf(&chars), &chars).expect("font");
        let color = Color::black();
        let item = |title: &str, page: &str| SubentryItem {
            title: title.to_string(),
            page: page.to_string(),
            title_ids: Vec::new(),
            page_ids: Vec::new(),
            italic: true,
            color: color.clone(),
            y: 500.0,
            x: 180.0,
        };
        let run = SubentryRun {
            page: 0,
            left: 180.0,
            right: 360.0,
            baselines: vec![500.0, 488.0],
            size: 10.0,
            items: vec![
                item("问答引擎摘要问答引擎摘要", "4"),
                item("聊天机器人应用聊天机器人", "8"),
            ],
            bullet_ids: vec![1],
            bullet_color: color.clone(),
        };
        let lines = layout_subentry_flow(&run, run.right - run.left, 10.0, &font).expect("flow");
        assert!(lines.len() >= 2, "the run stayed on one line");
        for line in &lines {
            assert!(
                line.first()
                    .is_none_or(|piece| piece.kind != FlowKind::Bullet),
                "a separator opens the line"
            );
        }
        for line in &lines {
            assert!(
                line.last()
                    .is_none_or(|piece| piece.kind != FlowKind::Bullet),
                "a wrap left a trailing separator"
            );
        }
        let short_run = SubentryRun {
            page: run.page,
            left: run.left,
            right: run.right,
            baselines: vec![500.0],
            size: run.size,
            items: vec![item("问答", "4"), item("引擎", "8")],
            bullet_ids: run.bullet_ids.clone(),
            bullet_color: run.bullet_color.clone(),
        };
        let short = layout_subentry_flow(&short_run, short_run.right - short_run.left, 10.0, &font)
            .expect("short flow");
        assert_eq!(
            short.len(),
            1,
            "two short items wrapped into {}",
            short.len()
        );
        let kinds: Vec<_> = short[0].iter().map(|piece| piece.kind).collect();
        assert!(
            kinds.contains(&FlowKind::Bullet),
            "items on one line lost the separator between them"
        );
        assert!(kinds[0] != FlowKind::Bullet, "the separator opens the line");
        assert!(
            *kinds.last().unwrap() != FlowKind::Bullet,
            "the separator trails the line"
        );
        let (placed, extra_tall) = place_subentry_lines(&run, &lines, 10.0);
        let step = placed[0].y - placed[1].y;
        assert!(
            (step - 12.0).abs() < 0.2,
            "wrap leading {step} left the source 12pt step"
        );
        if lines.len() > run.baselines.len() {
            assert!(
                extra_tall > 8.0,
                "a taller block did not push the rows below: {extra_tall}"
            );
        }
        let short = vec![lines[0].clone()];
        let (_, extra_short) = place_subentry_lines(&run, &short, 10.0);
        assert!(
            extra_short < -8.0,
            "a shorter block did not pull the rows below: {extra_short}"
        );
        let painted = paint_flow_line(
            0,
            run.left,
            &placed[0],
            &[1],
            &[("F1".into(), font)],
            &HashMap::new(),
        );
        let text_left = painted
            .iter()
            .filter(|item| item.text != "■")
            .map(|item| item.x)
            .fold(f32::MAX, f32::min);
        assert!(
            (text_left - run.left).abs() < 0.2,
            "first line text x={text_left} left the column {}",
            run.left
        );
        assert!(
            painted
                .iter()
                .filter(|item| item.text == "■")
                .all(|item| item.x > run.left + 1.0),
            "a separator sits left of the column"
        );
    }

    #[test]
    fn a_joined_contents_title_stays_on_the_title_x() {
        let chars: Vec<u32> = "将代理投入生产内存安全防护和超越"
            .chars()
            .map(|ch| ch as u32)
            .collect();
        let font = subset_ttf(&box_ttf(&chars), &chars).expect("font");
        let host = [
            ink_glyph(1, 129.0, 500.0, 12.0, "P"),
            ink_glyph(2, 141.0, 500.0, 12.0, "a"),
        ];
        let host_refs: Vec<&Glyph> = host.iter().collect();
        let extraction = Extraction {
            coordinate_space: "user",
            pages: vec![crate::glyph::PageInfo {
                index: 0,
                object_id: "1 0".into(),
                media_box: [0.0, 0.0, 612.0, 792.0],
                rotate: 0,
            }],
            glyphs: Vec::new(),
            regions: Vec::new(),
            diagnostics: Vec::new(),
        };
        let metrics = CjkMeasure::resolve(0.0, 0.0);
        let (lines, extra) = layout_joined_contents_title(
            &host_refs,
            &[9],
            500.0,
            485.0,
            "将代理投入生产内存安全防护和超越",
            &font,
            &extraction,
            "F",
            0.0,
            metrics,
        )
        .expect("title");
        assert_eq!(lines.len(), 1, "the title wrapped at full page width");
        assert!((lines[0].x - 129.0).abs() < 0.2, "title x {}", lines[0].x);
        assert!(
            extra < -8.0,
            "the unused wrap line did not pull the rows below: {extra}"
        );
        let narrow = Extraction {
            pages: vec![crate::glyph::PageInfo {
                index: 0,
                object_id: "1 0".into(),
                media_box: [0.0, 0.0, 250.0, 792.0],
                rotate: 0,
            }],
            ..extraction
        };
        let (wrapped, _) = layout_joined_contents_title(
            &host_refs,
            &[9],
            500.0,
            485.0,
            "将代理投入生产内存安全防护和超越",
            &font,
            &narrow,
            "F",
            0.0,
            metrics,
        )
        .expect("wrapped title");
        assert!(wrapped.len() >= 2, "a narrow title stayed one line");
        let lefts: Vec<f32> = wrapped.iter().map(|line| line.x).collect();
        assert!(
            lefts.iter().all(|x| (*x - 129.0).abs() < 0.2),
            "wrap left the title x: {lefts:?}"
        );
    }

    #[test]
    fn contents_pages_have_no_overlapping_text() {
        // Page 0, not 8–17: contents pages are every page with toc marks.
        let bytes = contents_pdf(
            b"BT \
/F1 10 Tf \
1 0 0 1 177 640 Tm (Designing the router agent) Tj \
1 0 0 1 310 640 Tm (298) Tj \
1 0 0 1 340 640 Tm (Routing logic) Tj \
1 0 0 1 430 640 Tm (298) Tj \
1 0 0 1 177 628 Tm (Building the multi agent graph) Tj \
1 0 0 1 340 628 Tm (300) Tj \
1 0 0 1 370 628 Tm (Trying out the) Tj \
1 0 0 1 177 616 Tm (router agent) Tj \
1 0 0 1 240 616 Tm (301) Tj \
/F1 4 Tf \
1 0 0 1 328 641 Tm (\x95) Tj \
1 0 0 1 358 629 Tm (\x95) Tj \
/F1 10 Tf \
1 0 0 1 120 596 Tm (12.3 Handling multi agent requests) Tj \
1 0 0 1 177 580 Tm (component) Tj \
1 0 0 1 240 580 Tm (302) Tj \
1 0 0 1 177 568 Tm (Supervisor pattern agents) Tj \
1 0 0 1 310 568 Tm (302) Tj \
1 0 0 1 177 520 Tm (Preparing the travel agent for live weather data) Tj \
1 0 0 1 420 520 Tm (323) Tj \
1 0 0 1 450 520 Tm (Updating the agent chat loop now) Tj \
1 0 0 1 177 508 Tm (remote tools) Tj \
1 0 0 1 250 508 Tm (324) Tj \
/F1 4 Tf \
1 0 0 1 438 521 Tm (\x95) Tj \
/F1 27 Tf \
1 0 0 1 93 508 Tm (14) Tj \
/F1 12 Tf \
1 0 0 1 129 492 Tm (Productionizing agents memory guardrails and) Tj \
1 0 0 1 141 477 Tm (beyond) Tj \
1 0 0 1 186 477 Tm (327) Tj \
/F1 10 Tf \
1 0 0 1 120 456 Tm (14.1 Memory) Tj \
1 0 0 1 210 456 Tm (328) Tj \
ET",
        );
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let pages = contents_page_indexes(&extraction);
        assert!(
            pages.contains(&0),
            "toc marks on page 0 were ignored: {pages:?}"
        );
        assert!(
            pages.iter().any(|page| *page < 8 || *page > 17),
            "contents detection was limited to pages 8-17: {pages:?}"
        );
        let sources: Vec<String> = segmentation
            .segments
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| {
                let translated = if seg.text.contains("Designing") {
                    "设计"
                } else if seg.text.contains("Routing") {
                    "逻辑"
                } else if seg.text.contains("Building") {
                    "构图"
                } else if seg.text.contains("Trying") || seg.text.contains("router agent") {
                    "试用"
                } else if seg.text.contains("12.3") {
                    "12.3 处理多代理请求"
                } else if seg.text.contains("component") {
                    "组件"
                } else if seg.text.contains("Supervisor") {
                    "监督"
                } else if seg.text.contains("Preparing") {
                    "为实时天气数据准备旅行代理并接入远程工具的完整说明文字需要换行"
                } else if seg.text.contains("Updating") {
                    "更新智能体聊天循环并结合本地与远程工具进行验证"
                } else if seg.text.contains("remote") {
                    "远程"
                } else if seg.text.contains("Productionizing") {
                    "将代理投入生产内存安全防护和"
                } else if seg.text.contains("beyond") {
                    "超越"
                } else if seg.text.contains("14.1") {
                    "14.1 内存"
                } else {
                    seg.text.as_str()
                };
                TranslatedSegment {
                    id: seg.id,
                    page_index: seg.page_index,
                    glyph_ids: seg.glyph_ids.clone(),
                    source: seg.text.clone(),
                    translated: translated.into(),
                }
            })
            .collect();
        let font_chars: String = segments.iter().map(|seg| seg.translated.as_str()).collect();
        let codes: Vec<u32> = format!("{font_chars}■")
            .chars()
            .map(|ch| ch as u32)
            .collect();
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(box_ttf(&codes)),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let painted = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract();
        let overlap = overlapping_text(&painted, &pages);
        assert!(overlap.is_none(), "contents text overlaps: {overlap:?}");
        let heading = painted
            .glyphs
            .iter()
            .find(|glyph| glyph.unicode.contains('处'))
            .expect("heading");
        let component = painted
            .glyphs
            .iter()
            .find(|glyph| glyph.unicode.contains('组'))
            .expect("component");
        let gap = heading.matrix[5] - component.matrix[5];
        assert!(
            (12.0..22.0).contains(&gap),
            "12.3 and its sub-entry gap is {gap} (heading {} component {})",
            heading.matrix[5],
            component.matrix[5]
        );
        let title_ys: Vec<f32> = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode.contains('超') || glyph.unicode.contains('投'))
            .map(|glyph| glyph.matrix[5])
            .collect();
        assert!(!title_ys.is_empty(), "chapter title missing");
        let top = title_ys.iter().copied().fold(f32::MIN, f32::max);
        let low = title_ys.iter().copied().fold(f32::MAX, f32::min);
        assert!(
            top - low < 1.5,
            "chapter title did not fit on one line: {title_ys:?}"
        );
        let title_left = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode.contains('将'))
            .map(|glyph| glyph.matrix[4])
            .fold(f32::MAX, f32::min);
        assert!(
            (title_left - 129.0).abs() < 2.0,
            "chapter title x={title_left} left the source title x; sources {sources:?}"
        );
        for glyph in &painted.glyphs {
            if glyph.page_index != 0 || glyph.unicode != "■" {
                continue;
            }
            let line: Vec<_> = painted
                .glyphs
                .iter()
                .filter(|other| {
                    other.page_index == glyph.page_index
                        && (other.matrix[5] - glyph.matrix[5]).abs() < 3.0
                        && other.unicode.trim().chars().any(|ch| !is_bullet_char(ch))
                })
                .collect();
            let before = line
                .iter()
                .any(|other| other.matrix[4] < glyph.matrix[4] - 0.5);
            let after = line
                .iter()
                .any(|other| other.matrix[4] > glyph.matrix[4] + 0.5);
            assert!(
                before && after,
                "■ at x={} y={} is not between two items",
                glyph.matrix[4],
                glyph.matrix[5]
            );
        }
        let source_labels = page_label_tokens(&extraction);
        let painted_labels = page_label_tokens(&painted);
        assert_eq!(
            painted_labels, source_labels,
            "a contents row lost or duplicated its page number"
        );
    }

    #[test]
    fn contents_rows_keep_source_page_numbers() {
        // Page 0 is a contents page because of its toc marks, not because
        // it is pages 8–17. Rows cover an inline number, three headings
        // whose number sits on the next source line, a same-x sub-entry
        // wrap, and the chapter title wrap.
        let bytes = contents_pdf(
            b"BT \
/F1 10 Tf \
1 0 0 1 133 730 Tm (12.2 Building a router assistant) Tj \
1 0 0 1 300 730 Tm (298) Tj \
1 0 0 1 142 710 Tm (11.1 Starting simple: Building a single-tool travel info) Tj \
1 0 0 1 162 696 Tm (agent) Tj \
1 0 0 1 193 696 Tm (268) Tj \
1 0 0 1 162 682 Tm (Project setup) Tj \
1 0 0 1 233 682 Tm (268) Tj \
1 0 0 1 162 668 Tm (Loading variables) Tj \
1 0 0 1 253 668 Tm (269) Tj \
/F1 4 Tf \
1 0 0 1 360 683 Tm (\x95) Tj \
1 0 0 1 360 669 Tm (\x95) Tj \
/F1 10 Tf \
1 0 0 1 133 630 Tm (12.3 Handling multi-agent requests with a Supervisor) Tj \
1 0 0 1 162 614 Tm (component) Tj \
1 0 0 1 213 614 Tm (302) Tj \
1 0 0 1 162 600 Tm (Supervisor pattern) Tj \
1 0 0 1 258 600 Tm (302) Tj \
1 0 0 1 162 586 Tm (Return ticket) Tj \
1 0 0 1 233 586 Tm (304) Tj \
/F1 4 Tf \
1 0 0 1 360 601 Tm (\x95) Tj \
1 0 0 1 360 587 Tm (\x95) Tj \
/F1 10 Tf \
1 0 0 1 162 540 Tm (Designing the router) Tj \
1 0 0 1 268 540 Tm (298) Tj \
1 0 0 1 162 526 Tm (Trying out the) Tj \
1 0 0 1 162 512 Tm (router agent) Tj \
1 0 0 1 228 512 Tm (301) Tj \
/F1 4 Tf \
1 0 0 1 360 541 Tm (\x95) Tj \
1 0 0 1 360 513 Tm (\x95) Tj \
/F1 10 Tf \
1 0 0 1 142 470 Tm (11.7 Expanding your agent: Adding a weather forecast) Tj \
1 0 0 1 162 456 Tm (tool) Tj \
1 0 0 1 188 456 Tm (283) Tj \
1 0 0 1 162 442 Tm (Mock weather service) Tj \
1 0 0 1 268 442 Tm (283) Tj \
/F1 4 Tf \
1 0 0 1 360 443 Tm (\x95) Tj \
/F1 27 Tf \
1 0 0 1 93 400 Tm (14) Tj \
/F1 12 Tf \
1 0 0 1 129 378 Tm (Productionizing agents memory guardrails and) Tj \
1 0 0 1 141 363 Tm (beyond) Tj \
1 0 0 1 186 363 Tm (327) Tj \
/F1 10 Tf \
1 0 0 1 133 340 Tm (14.1 Memory) Tj \
1 0 0 1 195 340 Tm (328) Tj \
ET",
        );
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let pages = contents_page_indexes(&extraction);
        assert!(
            pages.contains(&0),
            "toc marks were not detected: {pages:?} kept {:?}",
            segmentation
                .kept
                .iter()
                .map(|(_, reason)| reason.as_str())
                .collect::<Vec<_>>()
        );
        let sources: Vec<String> = segmentation
            .segments
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        let source_labels = page_label_tokens(&extraction);
        assert!(
            source_labels.len() >= 10,
            "fixture lost source page numbers before rewrite: {source_labels:?} sources {sources:?}"
        );
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| {
                let translated = if seg.text.contains("12.2") {
                    "12.2 路由助手"
                } else if seg.text.contains("11.1") {
                    "11.1 从简出发"
                } else if seg.text.contains("12.3") {
                    "12.3 处理多代理请求"
                } else if seg.text.contains("11.7") {
                    "11.7 扩展预报"
                } else if seg.text.contains("14.1") {
                    "14.1 内存"
                } else if seg.text.contains("component") {
                    "组件"
                } else if seg.text.trim() == "agent" {
                    "代理"
                } else if seg.text.contains("Project") {
                    "项目设置"
                } else if seg.text.contains("Loading") {
                    "加载变量"
                } else if seg.text.contains("Supervisor") {
                    "监督模式"
                } else if seg.text.contains("Return") {
                    "往返交互"
                } else if seg.text.contains("Designing") {
                    "设计路由"
                } else if seg.text.contains("Trying") && seg.text.contains("router") {
                    "尝试使用路由代理"
                } else if seg.text.contains("Trying") {
                    "尝试使用"
                } else if seg.text.contains("router agent") {
                    "路由代理"
                } else if seg.text.trim() == "tool" {
                    "工具"
                } else if seg.text.contains("Mock") {
                    "模拟天气"
                } else if seg.text.contains("Productionizing") {
                    "将代理投入生产"
                } else if seg.text.contains("beyond") {
                    "超越"
                } else {
                    seg.text.as_str()
                };
                TranslatedSegment {
                    id: seg.id,
                    page_index: seg.page_index,
                    glyph_ids: seg.glyph_ids.clone(),
                    source: seg.text.clone(),
                    translated: translated.into(),
                }
            })
            .collect();
        let font_chars: String = segments.iter().map(|seg| seg.translated.as_str()).collect();
        let codes: Vec<u32> = format!("{font_chars}■0123456789")
            .chars()
            .map(|ch| ch as u32)
            .collect();
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(box_ttf(&codes)),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let painted = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract();
        let painted_labels = page_label_tokens(&painted);
        assert_eq!(
            painted_labels, source_labels,
            "a contents row lost or duplicated its page number; sources {sources:?}"
        );
        let y_of = |marker: char| {
            painted
                .glyphs
                .iter()
                .find(|glyph| glyph.unicode.contains(marker))
                .unwrap_or_else(|| panic!("missing {marker}; sources {sources:?}"))
                .matrix[5]
        };
        let heading = y_of('处');
        let wrap = y_of('组');
        assert!(
            (heading - wrap).abs() < 1.5,
            "组件 was not joined onto 12.3 (heading {heading} wrap {wrap})"
        );
        assert!(
            page_label_on_baseline(&painted, heading, "302"),
            "12.3 lost 302"
        );
        assert!(
            page_label_on_baseline(&painted, y_of('监'), "302"),
            "the sub-entry under 12.3 lost its own 302"
        );
        assert!(
            page_label_on_baseline(&painted, y_of('简'), "268"),
            "11.1 lost 268; sources {sources:?}"
        );
        assert!(
            page_label_on_baseline(&painted, y_of('扩'), "283"),
            "11.7 lost 283; sources {sources:?}"
        );
        assert!(
            page_label_on_baseline(&painted, y_of('拟'), "283"),
            "the sub-entry under 11.7 lost its own 283"
        );
        assert!(
            page_label_on_baseline(&painted, y_of('将'), "327")
                || page_label_on_baseline(&painted, y_of('超'), "327"),
            "chapter 14 lost 327"
        );
        assert!(
            (y_of('将') - y_of('超')).abs() < 1.5,
            "chapter 14 title wrapped off the title line"
        );
        assert!(
            page_label_on_baseline(&painted, y_of('内'), "328"),
            "14.1 lost 328"
        );
        assert!(
            page_label_on_baseline(&painted, y_of('助'), "298"),
            "inline 12.2 lost 298"
        );
        assert!(
            source_labels.iter().any(|label| label == "301"),
            "fixture dropped the wrapped sub-entry page number"
        );
        let overlap = overlapping_text(&painted, &pages);
        assert!(overlap.is_none(), "contents text overlaps: {overlap:?}");
    }

    fn page_label_tokens(extraction: &Extraction) -> Vec<String> {
        let mut glyphs: Vec<&Glyph> = extraction
            .glyphs
            .iter()
            .filter(|glyph| !glyph.unicode.trim().is_empty())
            .collect();
        glyphs.sort_by(|left, right| {
            left.page_index.cmp(&right.page_index).then(
                right.matrix[5]
                    .total_cmp(&left.matrix[5])
                    .then(left.matrix[4].total_cmp(&right.matrix[4])),
            )
        });
        let mut labels = Vec::new();
        let mut index = 0;
        while index < glyphs.len() {
            let page = glyphs[index].page_index;
            let y = glyphs[index].matrix[5];
            let mut line = Vec::new();
            while index < glyphs.len()
                && glyphs[index].page_index == page
                && (glyphs[index].matrix[5] - y).abs() <= 2.0
            {
                line.push(glyphs[index]);
                index += 1;
            }
            line.sort_by(|left, right| left.matrix[4].total_cmp(&right.matrix[4]));
            let mut token: Vec<&Glyph> = Vec::new();
            for glyph in line {
                if let Some(prev) = token.last() {
                    let size = prev.font_size.max(glyph.font_size).max(1.0);
                    if glyph_ink_left(glyph) - glyph_ink_right(prev) > size * 0.35 {
                        push_page_label(&token, &mut labels);
                        token.clear();
                    }
                }
                token.push(glyph);
            }
            push_page_label(&token, &mut labels);
        }
        labels.sort();
        labels
    }

    fn push_page_label(token: &[&Glyph], labels: &mut Vec<String>) {
        if token.is_empty() {
            return;
        }
        let size = token
            .iter()
            .map(|glyph| glyph.font_size)
            .fold(0.0f32, f32::max);
        let text: String = token.iter().map(|glyph| glyph.unicode.trim()).collect();
        if size < 16.0 && bare_page_label(&text) {
            labels.push(text);
        }
    }

    fn page_label_on_baseline(extraction: &Extraction, y: f32, label: &str) -> bool {
        let mut glyphs: Vec<&Glyph> = extraction
            .glyphs
            .iter()
            .filter(|glyph| (glyph.matrix[5] - y).abs() <= 2.0 && !glyph.unicode.trim().is_empty())
            .collect();
        glyphs.sort_by(|left, right| left.matrix[4].total_cmp(&right.matrix[4]));
        let mut token: Vec<&Glyph> = Vec::new();
        let mut found = false;
        for glyph in glyphs {
            if let Some(prev) = token.last() {
                let size = prev.font_size.max(glyph.font_size).max(1.0);
                if glyph_ink_left(glyph) - glyph_ink_right(prev) > size * 0.35 {
                    if token_text(&token) == label {
                        found = true;
                    }
                    token.clear();
                }
            }
            token.push(glyph);
        }
        found || token_text(&token) == label
    }

    fn token_text(token: &[&Glyph]) -> String {
        token.iter().map(|glyph| glyph.unicode.trim()).collect()
    }

    fn overlapping_text(extraction: &Extraction, pages: &[u32]) -> Option<String> {
        for page in pages {
            let glyphs: Vec<&Glyph> = extraction
                .glyphs
                .iter()
                .filter(|glyph| {
                    glyph.page_index == *page
                        && !glyph.unicode.trim().is_empty()
                        && !glyph
                            .unicode
                            .trim()
                            .chars()
                            .all(|ch| matches!(ch, '.' | '·' | '…'))
                })
                .collect();
            for left in 0..glyphs.len() {
                for right in (left + 1)..glyphs.len() {
                    let a = glyphs[left];
                    let b = glyphs[right];
                    let dy = (a.matrix[5] - b.matrix[5]).abs();
                    let size = a.font_size.min(b.font_size).max(1.0);
                    if dy < size * 0.45 {
                        continue;
                    }
                    if boxes_overlap(a.bbox, b.bbox, -0.6) {
                        return Some(format!(
                            "p{page} {:?} @{} vs {:?} @{}",
                            a.unicode, a.matrix[5], b.unicode, b.matrix[5]
                        ));
                    }
                }
            }
        }
        None
    }

    #[test]
    fn subentry_blocks_on_one_page_share_the_source_indent() {
        let color = Color::black();
        let run = |page, left, bullets: bool| SubentryRun {
            page,
            left,
            right: left + 200.0,
            baselines: vec![400.0],
            size: 10.0,
            items: Vec::new(),
            bullet_ids: if bullets { vec![1] } else { Vec::new() },
            bullet_color: color.clone(),
        };
        // 16pt apart is more than the old 1.5em join and still one source column.
        // Front matter sits 36pt further left and keeps its own x.
        let mut runs = vec![
            run(9, 141.2, false),
            run(9, 170.0, true),
            run(9, 186.2, true),
        ];
        share_subentry_column(&mut runs);
        assert!(
            (runs[0].left - 141.2).abs() < 0.2,
            "front matter joined the sub-entry column: {}",
            runs[0].left
        );
        assert!(
            (runs[1].left - runs[2].left).abs() < 0.2,
            "sub-entry blocks kept different indents: {} {}",
            runs[1].left,
            runs[2].left
        );
    }

    #[test]
    fn a_bullet_run_fills_the_contents_column_before_wrapping() {
        // The English row wraps inside x=300. A section number at the
        // contents column is further right. Short Chinese entries fill that
        // column on one line instead of breaking under an empty first line.
        let bytes = contents_pdf(
            b"BT /F1 10 Tf \
1 0 0 1 72 640 Tm (Building applications) Tj \
1 0 0 1 450 640 Tm (19) Tj \
1 0 0 1 72 600 Tm (LLM-based applications: Summarization and Q and A engines) Tj \
1 0 0 1 72 586 Tm [(Chatbots) -700 (7) -1600 (Agents) -700 (8)] TJ \
/F1 4 Tf \
1 0 0 1 150 588 Tm (\x95) Tj \
ET",
        );
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| {
                let translated = if seg.text.contains("Summarization") {
                    "问答引擎"
                } else if seg.text.contains("Chatbots") {
                    "聊天机器人"
                } else if seg.text.contains("Agents") {
                    "智能体"
                } else if seg.text.contains("Building") {
                    "构建应用"
                } else {
                    seg.text.as_str()
                };
                TranslatedSegment {
                    id: seg.id,
                    page_index: seg.page_index,
                    glyph_ids: seg.glyph_ids.clone(),
                    source: seg.text.clone(),
                    translated: translated.into(),
                }
            })
            .collect();
        let font = box_ttf(
            &"问答引擎聊天机器人智能体构建应用■78"
                .chars()
                .map(|ch| ch as u32)
                .collect::<Vec<_>>(),
        );
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(font),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let painted = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract();
        let mut ys = Vec::new();
        for glyph in painted.glyphs.iter().filter(|glyph| {
            glyph.unicode.contains('问')
                || glyph.unicode.contains('聊')
                || glyph.unicode.contains('智')
        }) {
            let y = glyph.matrix[5];
            if ys.iter().all(|other: &f32| (other - y).abs() > 2.0) {
                ys.push(y);
            }
        }
        assert_eq!(
            ys.len(),
            1,
            "bullet run wrapped before the column was full: {}",
            painted.plain_text()
        );
        let left = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode.contains('问') || glyph.unicode.contains('聊'))
            .map(|glyph| glyph.matrix[4])
            .fold(f32::MAX, f32::min);
        assert!(
            left < 90.0,
            "packed run left the sub-entry indent at x={left}"
        );
    }

    #[test]
    fn contents_rows_follow_the_source_geometry() {
        // Source geometry, measured from the fixture, not from a column rule:
        // front matter, chapter, and section numbers sit one gap after the
        // title; only the part number meets the leader edge. Section indexes
        // share an x and section titles share a later x. Bullet rows wrap at
        // the source run's right edge and continue at that run's left.
        let bytes = contents_row_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let source_part = extraction
            .glyphs
            .iter()
            .filter(|glyph| glyph.matrix[4] > 400.0 && glyph.unicode.trim() == "1")
            .map(glyph_ink_right)
            .fold(0.0f32, f32::max);
        assert!(source_part > 400.0, "part number missing: {source_part}");
        let source_x = |needle: &str| {
            extraction
                .glyphs
                .iter()
                .find(|glyph| glyph.unicode == needle)
                .map(|glyph| glyph.matrix[4])
                .unwrap_or(f32::NAN)
        };
        let index_x = extraction
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode == "1" && glyph.matrix[4] < 140.0)
            .map(|glyph| glyph.matrix[4])
            .fold(f32::MAX, f32::min);
        let title_x = source_x("B");
        let subentry_x = source_x("E");
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| {
                let translated = if seg.text.contains("preface") {
                    "前言"
                } else if seg.text.contains("Introduction") {
                    "代理和应用简介"
                } else if seg.text.contains("PART") || seg.text == "S" {
                    "第1部分入门"
                } else if seg.text.contains("1.1") && seg.text.contains("Building") {
                    "1.1 构建应用"
                } else if seg.text.trim() == "1.1" {
                    "1.1"
                } else if seg.text.contains("Building") {
                    "构建应用"
                } else if seg.text.contains("2.1") && seg.text.contains("Running") {
                    "2.1 运行提示"
                } else if seg.text.trim() == "2.1" {
                    "2.1"
                } else if seg.text.contains("Running") {
                    "运行提示"
                } else if seg.text.contains("Engines") {
                    "问答引擎摘要问答引擎摘要问答引擎摘要问答引擎摘要"
                } else if seg.text.contains("Chatbots") {
                    "聊天机器人应用聊天机器人应用聊天机器人应用"
                } else if seg.text.contains("Agents") {
                    "章节练习补充说明文字章节练习补充说明文字"
                } else if seg.text.contains("PromptTemplate") {
                    "使用模板长标题使用模板长标题使用模板长标题使用模板长标题尾"
                } else {
                    seg.text.as_str()
                };
                TranslatedSegment {
                    id: seg.id,
                    page_index: seg.page_index,
                    glyph_ids: seg.glyph_ids.clone(),
                    source: seg.text.clone(),
                    translated: translated.into(),
                }
            })
            .collect();
        let font = box_ttf(
            &"前言代理和应用简介第1部分入门构建应用运行提示问答引擎摘要聊天机器人章节练习补充说明文字使用模板长标题尾■"
                .chars()
                .chain("xvi.123456789".chars())
                .map(|ch| ch as u32)
                .collect::<Vec<_>>(),
        );
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(font),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let painted = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract();
        let part_right = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode.trim() == "1" && glyph.matrix[4] > 300.0)
            .map(glyph_ink_right)
            .fold(0.0f32, f32::max);
        assert!(
            (part_right - source_part).abs() < 1.5,
            "part number right {part_right} left the source edge {source_part}"
        );
        let preface = painted
            .glyphs
            .iter()
            .find(|glyph| glyph.unicode == "前")
            .expect("front matter");
        assert!(
            painted
                .glyphs
                .iter()
                .any(|glyph| glyph.unicode == "x" || glyph.unicode == "xvi"),
            "xvi missing: {}",
            painted.plain_text()
        );
        let preface_right = painted
            .glyphs
            .iter()
            .filter(|glyph| {
                (glyph.matrix[5] - preface.matrix[5]).abs() < 1.5
                    && glyph.unicode != "x"
                    && glyph.unicode != "v"
                    && glyph.unicode != "i"
                    && glyph.unicode != "xvi"
            })
            .map(glyph_ink_right)
            .fold(0.0f32, f32::max);
        let xvi_left = painted
            .glyphs
            .iter()
            .filter(|glyph| {
                (glyph.matrix[5] - preface.matrix[5]).abs() < 1.5
                    && matches!(glyph.unicode.as_str(), "x" | "v" | "i" | "xvi")
            })
            .map(|glyph| glyph.matrix[4])
            .fold(f32::MAX, f32::min);
        assert!(
            xvi_left.is_finite() && xvi_left - preface_right < 16.0 && part_right - xvi_left > 40.0,
            "xvi x={xvi_left} is not inline after 前言 at {preface_right}; part edge {part_right}"
        );
        let index_draws: Vec<&Glyph> = painted
            .glyphs
            .iter()
            .filter(|glyph| {
                matches!(glyph.unicode.as_str(), "1" | "2")
                    && (glyph.matrix[4] - index_x).abs() < 3.0
            })
            .collect();
        assert!(
            index_draws.len() >= 2,
            "section indexes left the source column {index_x}: {}",
            painted.plain_text()
        );
        let title_draws: Vec<f32> = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode == "构" || glyph.unicode == "运")
            .map(|glyph| glyph.matrix[4])
            .collect();
        assert_eq!(
            title_draws.len(),
            2,
            "section titles: {title_draws:?} text={}",
            painted.plain_text()
        );
        for x in &title_draws {
            assert!(
                (x - title_x).abs() < 2.0,
                "section title x={x} left the source title column {title_x}"
            );
        }
        let mut sizes: Vec<f32> = painted
            .glyphs
            .iter()
            .filter(|glyph| {
                glyph.unicode == "构"
                    || glyph.unicode == "运"
                    || (matches!(glyph.unicode.as_str(), "1" | "2")
                        && (glyph.matrix[4] - index_x).abs() < 3.0)
            })
            .map(|glyph| glyph.font_size)
            .collect();
        sizes.sort_by(|left, right| left.total_cmp(right));
        assert!(
            sizes.len() >= 4 && sizes[sizes.len() - 1] - sizes[0] < 0.4,
            "section index and title sizes diverged: {sizes:?}"
        );
        assert!(
            !painted.glyphs.iter().any(|glyph| glyph.unicode == "S"),
            "small-caps S was painted into the part row: {}",
            painted.plain_text()
        );
        let mut bullet_lines: Vec<(f32, f32)> = Vec::new();
        for glyph in painted.glyphs.iter().filter(|glyph| {
            glyph.unicode.contains('问')
                || glyph.unicode.contains('聊')
                || glyph.unicode.contains('章')
        }) {
            let y = glyph.matrix[5];
            if let Some(row) = bullet_lines.iter_mut().find(|row| (row.0 - y).abs() < 1.5) {
                row.1 = row.1.min(glyph.matrix[4]);
            } else {
                bullet_lines.push((y, glyph.matrix[4]));
            }
        }
        bullet_lines.sort_by(|left, right| right.0.total_cmp(&left.0));
        assert!(
            bullet_lines.len() >= 2,
            "bullet run did not wrap: {} {bullet_lines:?}",
            painted.plain_text()
        );
        let first_x = bullet_lines[0].1;
        for (_, x) in bullet_lines.iter().skip(1) {
            assert!(
                (x - first_x).abs() < 0.2,
                "continuation x={x} left the first line x={first_x}"
            );
        }
        let inline: Vec<f32> = painted
            .glyphs
            .iter()
            .filter(|glyph| {
                matches!(glyph.unicode.as_str(), "7" | "8")
                    && bullet_lines
                        .iter()
                        .any(|row| (row.0 - glyph.matrix[5]).abs() < 2.0)
            })
            .map(|glyph| glyph.matrix[4] + glyph.advance[0].max(0.0))
            .collect();
        assert_eq!(
            inline.len(),
            2,
            "inline bullet numbers: {inline:?} lines={bullet_lines:?} text={}",
            painted.plain_text()
        );
        assert!(
            inline.iter().all(|right| part_right - right > 20.0),
            "bullet-run numbers left their titles for the part edge {part_right}: {inline:?}"
        );
        assert!(
            (first_x - subentry_x).abs() < 2.0,
            "bullet run x={first_x} left the source sub-entry column {subentry_x}"
        );
        let bullet_right = painted
            .glyphs
            .iter()
            .filter(|glyph| {
                glyph.unicode.contains('问')
                    || glyph.unicode.contains('聊')
                    || glyph.unicode.contains('章')
            })
            .map(glyph_ink_right)
            .fold(0.0f32, f32::max);
        assert!(
            part_right - bullet_right > 20.0,
            "bullet run reached the part edge {part_right}: ink {bullet_right}"
        );
        let prompt_ys: Vec<f32> = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode == "使" || glyph.unicode == "尾")
            .map(|glyph| glyph.matrix[5])
            .collect();
        assert!(
            prompt_ys.len() >= 2 && prompt_ys.iter().all(|y| (y - prompt_ys[0]).abs() < 1.5),
            "PromptTemplate wrapped with column room left: {prompt_ys:?} {}",
            painted.plain_text()
        );
    }

    #[test]
    fn glued_part_leaders_are_redrawn_with_the_page_number() {
        // Part 3 puts the dots and `  119` in one TJ, with no title glyph in
        // that operator. Parts 1 and 2 start the dot string with a space, so
        // the title still shares the operator. Blanking `119` used to drop
        // the dots and leave the gap.
        let bytes = glued_part_leader_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let title = segmentation
            .segments
            .iter()
            .find(|seg| seg.text.contains("CHATBOTS"))
            .expect("part title");
        assert!(
            !title.text.contains('.'),
            "leaders stayed in the title: {}",
            title.text
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let font = box_ttf(
            &"第3部分问答聊天机器人.119"
                .chars()
                .map(|ch| ch as u32)
                .collect::<Vec<_>>(),
        );
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments: vec![TranslatedSegment {
                    id: title.id,
                    page_index: title.page_index,
                    glyph_ids: title.glyph_ids.clone(),
                    source: title.text.clone(),
                    translated: "第3部分问答聊天机器人".into(),
                }],
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(font),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let painted = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract();
        let dots: Vec<f32> = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode == ".")
            .map(|glyph| glyph.matrix[4])
            .collect();
        assert!(
            dots.len() >= 4,
            "glued leaders were not redrawn: {}",
            painted.plain_text()
        );
        let title_right = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode == "人")
            .map(glyph_ink_right)
            .fold(0.0f32, f32::max);
        let number_left = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode == "1" || glyph.unicode == "9")
            .map(|glyph| glyph.matrix[4])
            .fold(f32::MAX, f32::min);
        assert!(
            title_right > 0.0 && number_left.is_finite() && number_left - title_right > 20.0,
            "119 is not past the title: title_right={title_right} number_left={number_left}"
        );
        assert!(
            dots.iter().any(|x| *x > title_right && *x < number_left),
            "no leader between the title and 119: dots={dots:?} title_right={title_right} number={number_left}"
        );
    }

    fn glued_part_leader_pdf() -> Vec<u8> {
        let mut doc = Document::with_version("1.4");
        doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        let pages_id = doc.new_object_id();
        let widths: Vec<lopdf::Object> = (0..256).map(|_| lopdf::Object::Integer(500)).collect();
        let mut font_dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "Encoding" => "WinAnsiEncoding",
            "FirstChar" => 0,
            "LastChar" => 255,
            "Widths" => widths,
        };
        font_dict.set("BaseFont", lopdf::Object::Name(b"Helvetica".to_vec()));
        let font = doc.add_object(font_dict);
        let mut fonts = lopdf::Dictionary::new();
        fonts.set("F1", font);
        let mut resources = lopdf::Dictionary::new();
        resources.set("Font", fonts);
        let content = b"BT \
/F1 13.5 Tf \
1 0 0 1 57 400 Tm (PART 3 QA CHATBOTS) Tj \
13.5 0 0 13.5 250 400 Tm [(............)-147(  119)] TJ \
ET"
        .to_vec();
        let content_id = doc.add_object(Stream::new(dictionary! {}, content));
        let page = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 531.into(), 666.into()],
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

    fn contents_row_pdf() -> Vec<u8> {
        let mut doc = Document::with_version("1.4");
        doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        let pages_id = doc.new_object_id();
        let widths: Vec<lopdf::Object> = (0..256).map(|_| lopdf::Object::Integer(500)).collect();
        let mut fonts = lopdf::Dictionary::new();
        for (name, base) in [
            ("F1", "NewBaskerville-Italic"),
            ("F2", "NewBaskerville-BoldItalic"),
            ("F3", "Helvetica"),
        ] {
            let mut font_dict = dictionary! {
                "Type" => "Font",
                "Subtype" => "Type1",
                "Encoding" => "WinAnsiEncoding",
                "FirstChar" => 0,
                "LastChar" => 255,
                "Widths" => widths.clone(),
            };
            font_dict.set("BaseFont", lopdf::Object::Name(base.as_bytes().to_vec()));
            let font = doc.add_object(font_dict);
            fonts.set(name, font);
        }
        let mut resources = lopdf::Dictionary::new();
        resources.set("Font", fonts);
        // Part `1` is the only right-edge number. Section indexes sit in
        // their own column; titles share a later x. The bullet run ends
        // well short of that part number. `S` is the small-caps tail of
        // the part title, a few points before the dots.
        let content = b"BT \
/F1 10 Tf 1 0 0 1 72 700 Tm (preface) Tj \
1 0 0 1 125 700 Tm (xvi) Tj \
/F2 13 Tf 1 0 0 1 90 640 Tm (Introduction to AI agents) Tj \
1 0 0 1 280 640 Tm (3) Tj \
/F3 12 Tf 1 0 0 1 40 600 Tm (PART LLM) Tj \
/F3 9.6 Tf 1 0 0 1 96 600 Tm (S) Tj \
/F3 12 Tf 1 0 0 1 220 600 Tm (........) Tj \
1 0 0 1 450 600 Tm (1) Tj \
/F3 11 Tf 1 0 0 1 110 560 Tm [(1.1)-1200(Building applications)-900(19)] TJ \
1 0 0 1 110 530 Tm [(2.1)-1200(Running prompts)-900(28)] TJ \
/F3 10 Tf \
1 0 0 1 180 470 Tm (Engines) Tj \
1 0 0 1 230 470 Tm (4) Tj \
/F3 4 Tf 1 0 0 1 248 472 Tm (\x95) Tj \
/F3 10 Tf \
1 0 0 1 258 470 Tm (Chatbots) Tj \
1 0 0 1 305 470 Tm (7) Tj \
/F3 4 Tf 1 0 0 1 322 472 Tm (\x95) Tj \
/F3 10 Tf \
1 0 0 1 332 470 Tm (Agents) Tj \
1 0 0 1 370 470 Tm (8) Tj \
/F1 10 Tf 1 0 0 1 180 420 Tm (PromptTemplate) Tj \
1 0 0 1 270 420 Tm (33) Tj \
ET"
        .to_vec();
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
    fn part_leaders_follow_the_title_not_the_row_above() {
        // The block above is one drawn line whose source still reaches the
        // part row, so closing the hole would pull a nearby left edge up.
        // Leader dots sit in that left-edge band. They and `171` belong on
        // the part title.
        let size = 11.0;
        let mut glyphs = vec![
            ink_glyph(1, 168.0, 300.0, size, "7"),
            ink_glyph(2, 168.0, 220.0, size, "5"),
            ink_glyph(10, 66.0, 200.0, size, "P"),
            ink_glyph(11, 120.0, 200.0, size, "4"),
        ];
        let mut leader_ids = Vec::new();
        for index in 0..8 {
            let id = 20 + index as u32;
            leader_ids.push(id);
            glyphs.push(ink_glyph(id, 209.0 + index as f32 * 28.0, 200.0, size, "."));
        }
        glyphs.push(ink_glyph(30, 449.0, 200.0, size, "1"));
        glyphs.push(ink_glyph(31, 456.0, 200.0, size, "7"));
        glyphs.push(ink_glyph(32, 463.0, 200.0, size, "1"));
        let extraction = Extraction {
            coordinate_space: "user",
            pages: vec![],
            glyphs,
            regions: vec![],
            diagnostics: vec![],
        };
        let by_id = glyph_index(&extraction.glyphs);
        // The part title sits below the dots. Closing the hole above used to
        // drag the dots up, and the number then followed the dots.
        let mut drawn = vec![
            test_draw(168.0, 300.0, size, "7.5 追踪", vec![1, 2]),
            test_draw(66.0, 180.0, size, "第4部分", vec![10, 11]),
            test_draw(209.0, 200.0, size, "............", leader_ids),
            test_draw(446.0, 200.0, size, "171", vec![30, 31, 32]),
        ];
        close_column_holes(&mut drawn, &extraction, &by_id);
        seat_inline_page_numbers(&mut drawn, &extraction, &by_id);
        let title_y = drawn
            .iter()
            .find(|item| item.text.starts_with('第'))
            .unwrap()
            .y;
        let leader_y = drawn
            .iter()
            .find(|item| item.text.chars().all(|ch| ch == '.'))
            .unwrap()
            .y;
        let number_y = drawn.iter().find(|item| item.text == "171").unwrap().y;
        assert!(
            (leader_y - title_y).abs() < 1.5,
            "leaders y={leader_y} left the part title y={title_y}"
        );
        assert!(
            (number_y - title_y).abs() < 1.5,
            "171 y={number_y} left the part title y={title_y}"
        );
    }

    #[test]
    fn a_section_index_stays_on_its_title_when_the_hole_above_closes() {
        // The line above is one drawn row whose source still reaches the
        // section. Closing that hole must move `2.2` and the title together.
        // The index used to be its own block and landed on the line above.
        let size = 11.0;
        let glyphs = vec![
            ink_glyph(1, 180.0, 296.0, size, "E"),
            ink_glyph(2, 186.0, 296.0, size, "n"),
            ink_glyph(10, 148.0, 280.0, size, "2"),
            ink_glyph(11, 154.0, 280.0, size, "."),
            ink_glyph(12, 158.0, 280.0, size, "2"),
            ink_glyph(13, 174.0, 280.0, size, "R"),
            ink_glyph(14, 180.0, 280.0, size, "u"),
        ];
        let extraction = Extraction {
            coordinate_space: "user",
            pages: vec![],
            glyphs,
            regions: vec![],
            diagnostics: vec![],
        };
        let by_id = glyph_index(&extraction.glyphs);
        let mut drawn = vec![
            test_draw(180.0, 340.0, size, "环境", vec![1, 2]),
            test_draw(148.0, 280.0, size, "2.2", vec![10, 11, 12]),
            test_draw(174.0, 280.0, size, "运行提示", vec![13, 14]),
        ];
        close_column_holes(&mut drawn, &extraction, &by_id);
        let index_y = drawn.iter().find(|item| item.text == "2.2").unwrap().y;
        let title_y = drawn.iter().find(|item| item.text == "运行提示").unwrap().y;
        assert!(
            (index_y - title_y).abs() < 0.4,
            "2.2 y={index_y} left the title y={title_y}"
        );
        assert!(
            index_y > 280.0 + 1.0,
            "the hole above the section stayed open: y={index_y}"
        );
    }

    #[test]
    fn split_section_digits_move_with_the_title() {
        // Echoed `1.4.1` is one draw per glyph. Joining them lets the hole
        // closer treat the index and the title as one row.
        let size = 10.5;
        let glyphs = vec![
            ink_glyph(1, 90.0, 320.0, size, "P"),
            ink_glyph(10, 70.0, 300.0, size, "1"),
            ink_glyph(11, 78.0, 300.0, size, "."),
            ink_glyph(12, 86.0, 300.0, size, "4"),
            ink_glyph(13, 94.0, 300.0, size, "."),
            ink_glyph(14, 102.0, 300.0, size, "1"),
            ink_glyph(20, 120.0, 300.0, size, "P"),
        ];
        let extraction = Extraction {
            coordinate_space: "user",
            pages: vec![],
            glyphs,
            regions: vec![],
            diagnostics: vec![],
        };
        let by_id = glyph_index(&extraction.glyphs);
        let mut drawn = vec![
            test_draw(90.0, 360.0, size, "上一节", vec![1]),
            test_draw(70.0, 300.0, size, "1", vec![10]),
            test_draw(78.0, 300.0, size, ".", vec![11]),
            test_draw(86.0, 300.0, size, "4", vec![12]),
            test_draw(94.0, 300.0, size, ".", vec![13]),
            test_draw(102.0, 300.0, size, "1", vec![14]),
            test_draw(120.0, 300.0, size, "提示工程", vec![20]),
        ];
        coalesce_section_indexes(&mut drawn);
        assert!(
            drawn.iter().any(|item| item.text == "1.4.1"),
            "digits stayed split: {:?}",
            drawn
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>()
        );
        close_column_holes(&mut drawn, &extraction, &by_id);
        let index_y = drawn.iter().find(|item| item.text == "1.4.1").unwrap().y;
        let title_y = drawn.iter().find(|item| item.text == "提示工程").unwrap().y;
        assert!(
            (index_y - title_y).abs() < 0.4,
            "1.4.1 y={index_y} left 提示工程 y={title_y}"
        );
        assert!(
            index_y > 300.0 + 1.0,
            "the index stayed on the English baseline y={index_y}"
        );
    }

    #[test]
    fn overlapping_section_atoms_still_join() {
        // The echo face is wider than the source advance, so `1` of `1.1`
        // starts inside the ink of `1.`. The old gap test split the index
        // and left `1.1` on the English baseline.
        let size = 10.8;
        let mut drawn = vec![
            test_draw(57.2, 507.8, size, "1", vec![1]),
            test_draw(62.0, 507.8, size, ".", vec![2]),
            test_draw(65.5, 507.8, size, "1", vec![3]),
            test_draw(70.4, 507.8, size, ".", vec![4]),
            test_draw(74.0, 507.8, size, "1", vec![5]),
            test_draw(93.2, 507.8, size, "基于大语言模型的应用", vec![6]),
        ];
        for item in &mut drawn[..5] {
            item.widths = vec![size * 0.55];
        }
        coalesce_section_indexes(&mut drawn);
        assert!(
            drawn.iter().any(|item| item.text == "1.1.1"),
            "wide digits stayed split: {:?}",
            drawn
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            drawn.iter().filter(|item| item.text.contains('1')).count(),
            1
        );
    }

    #[test]
    fn a_chapter_title_stops_before_the_giant_numeral() {
        // Both source lines share a right edge past a numeral whose left
        // sits inside the full-size Chinese second line. The title has to
        // move left to clear it.
        let bytes = contents_pdf(
            b"0.8 0.8 0.8 rg 350 480 80 160 re f \
BT /F1 24 Tf \
1 0 0 1 310 600 Tm (Introduction to AI) Tj \
1 0 0 1 250 568 Tm (agents and applications) Tj \
ET",
        );
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| TranslatedSegment {
                id: seg.id,
                page_index: seg.page_index,
                glyph_ids: seg.glyph_ids.clone(),
                source: seg.text.clone(),
                translated: "AI 智能体及应用简介".into(),
            })
            .collect();
        let font = box_ttf(
            &"AI 智能体及应用简介"
                .chars()
                .map(|ch| ch as u32)
                .collect::<Vec<_>>(),
        );
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(font),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let painted = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract();
        let mut han = 0;
        for glyph in painted.glyphs.iter().filter(|glyph| {
            glyph.unicode.chars().any(is_han_char) || glyph.font_name.contains("RPT")
        }) {
            han += 1;
            let right = glyph.bbox[0].max(glyph.bbox[2]);
            assert!(
                right < 350.0,
                "title ink enters the numeral: {:?} x={:.1} right={right:.1}",
                glyph.unicode,
                glyph.matrix[4]
            );
        }
        assert!(han > 0, "the title was not drawn");
        assert!(
            painted.plain_text().contains("智能体"),
            "the title split 智能体: {}",
            painted.plain_text()
        );
        let zhi = painted
            .glyphs
            .iter()
            .find(|glyph| glyph.unicode == "智")
            .expect("智");
        assert!(
            zhi.matrix[4] < 250.0,
            "the long title line stays on the source left and crosses the numeral: x={}",
            zhi.matrix[4]
        );
    }

    #[test]
    fn a_numbered_item_paints_its_number_once() {
        let bytes = contents_pdf(
            b"BT /F1 10 Tf \
0.8 0.651 0.349 rg \
1 0 0 1 117.2 220 Tm (1 The agent sends a prompt) Tj \
0.15 g \
1 0 0 1 127.2 207 Tm (tools for the request.) Tj \
ET",
        );
        let translated = "1 智能体向大语言模型发送一段提示，要求其选择最相关的工具和假日数据库。";
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| TranslatedSegment {
                id: seg.id,
                page_index: seg.page_index,
                glyph_ids: seg.glyph_ids.clone(),
                source: seg.text.clone(),
                translated: if seg.text.contains("The agent") {
                    translated.into()
                } else {
                    seg.text.clone()
                },
            })
            .collect();
        assert!(
            segments
                .iter()
                .any(|seg| seg.source.contains("The agent") && seg.source.contains("tools")),
            "the hanging item was split: {:?}",
            segments
                .iter()
                .map(|seg| seg.source.as_str())
                .collect::<Vec<_>>()
        );
        let font = box_ttf(&translated.chars().map(|ch| ch as u32).collect::<Vec<_>>());
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(font),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let painted = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract();
        let ones: Vec<_> = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode == "1")
            .collect();
        assert_eq!(ones.len(), 1, "the item number was repeated: {ones:?}");
        assert!(
            (ones[0].matrix[4] - 117.2).abs() < 1.0,
            "number x {}",
            ones[0].matrix[4]
        );
        assert_eq!(ones[0].fill_color.components[0], 0.8);
        let han: Vec<_> = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode.chars().any(is_han_char))
            .collect();
        assert!(!han.is_empty(), "the item body was not drawn");
        let mut baselines: Vec<f32> = Vec::new();
        for glyph in &han {
            assert!(
                glyph.fill_color.space == "DeviceGray",
                "body kept the number colour: {:?}",
                glyph.fill_color
            );
            if !baselines.iter().any(|y| (y - glyph.matrix[5]).abs() < 0.6) {
                baselines.push(glyph.matrix[5]);
            }
        }
        assert!(baselines.len() >= 2, "the item stayed on one line");
        for y in baselines {
            let left = han
                .iter()
                .filter(|glyph| (glyph.matrix[5] - y).abs() < 0.6)
                .map(|glyph| glyph.matrix[4])
                .fold(f32::MAX, f32::min);
            assert!(
                (left - 127.2).abs() < 2.0,
                "line at y={y:.1} starts at {left:.1}, not the hanging indent"
            );
        }
    }

    fn test_draw(x: f32, y: f32, size: f32, text: &str, glyph_ids: Vec<u32>) -> Drawn {
        let count = text.chars().count();
        Drawn {
            page: 0,
            x,
            y,
            size,
            color: Color::black(),
            cids: vec![],
            resource: "F".into(),
            skew: 0.0,
            widths: vec![size * 0.5; count],
            gaps: vec![0.0; count.saturating_sub(1)],
            text: text.into(),
            supers: vec![false; count],
            sup_scale: 1.0,
            sup_rise: 0.0,
            glyph_ids,
        }
    }

    #[test]
    fn han_title_wrap_joins_without_a_space() {
        assert_eq!(join_wrapped_title("组合成", "一个"), "组合成一个");
        assert_eq!(
            join_wrapped_title("LangChain", "architecture"),
            "LangChain architecture"
        );
    }

    #[test]
    fn a_margin_folio_keeps_its_roman_token() {
        let bytes = contents_pdf(b"BT /F1 12 Tf 1 0 0 1 280 760 Tm (ii) Tj ET");
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| TranslatedSegment {
                id: seg.id,
                page_index: seg.page_index,
                glyph_ids: seg.glyph_ids.clone(),
                source: seg.text.clone(),
                translated: "二".into(),
            })
            .collect();
        assert!(
            segments.iter().any(|seg| seg.source.contains("ii")),
            "folio was not a segment: {segments:?}"
        );
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(box_ttf(&['二' as u32, 'i' as u32])),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let painted = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract();
        let text = painted.plain_text();
        assert!(text.contains("ii"), "{text}");
        assert!(!text.contains('二'), "{text}");
    }

    #[test]
    fn a_right_aligned_cluster_keeps_a_shared_right_edge() {
        let bytes = right_aligned_title_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| {
                let translated = if seg.text.contains("Applications") {
                    "智能体与应用"
                } else if seg.text.contains("LangChain") {
                    "使用 LangChain"
                } else {
                    "初版"
                };
                TranslatedSegment {
                    id: seg.id,
                    page_index: seg.page_index,
                    glyph_ids: seg.glyph_ids.clone(),
                    source: seg.text.clone(),
                    translated: translated.into(),
                }
            })
            .collect();
        let font = box_ttf(
            &"智能体与应用使用 LangChain初版"
                .chars()
                .map(|ch| ch as u32)
                .collect::<Vec<_>>(),
        );
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(font),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let painted = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract();
        let mut rows: Vec<(f32, f32, f32)> = Vec::new();
        for glyph in painted.glyphs.iter().filter(|glyph| {
            glyph.font_name.contains("RPT") || glyph.unicode.chars().any(is_han_char)
        }) {
            let y = glyph.matrix[5];
            let left = glyph.matrix[4];
            let right = left + glyph.advance[0].max(0.0);
            if let Some(row) = rows.iter_mut().find(|row| (row.0 - y).abs() < 1.0) {
                row.1 = row.1.min(left);
                row.2 = row.2.max(right);
            } else {
                rows.push((y, left, right));
            }
        }
        assert_eq!(rows.len(), 3, "title-page rows: {rows:?}");
        let edge = rows.iter().map(|row| row.2).fold(0.0f32, f32::max);
        for row in &rows {
            assert!((row.2 - edge).abs() < 1.5, "right edges diverged: {rows:?}");
        }
        let name = rows
            .iter()
            .find(|row| row.2 - row.1 < 80.0)
            .expect("short author line");
        assert!(
            name.1 > 400.0,
            "author stayed left-anchored at x={}: {rows:?}",
            name.1
        );
    }

    #[test]
    fn small_caps_cap_size_is_the_initial_not_the_median() {
        let mut sizes = Vec::new();
        sizes.extend(std::iter::repeat_n(15.96, 20));
        sizes.extend(std::iter::repeat_n(19.98, 9));
        let cap = small_caps_cap_size(&sizes).unwrap();
        assert!((cap - 19.98).abs() < 0.01, "{cap}");
        assert!(small_caps_cap_size(&[10.0; 12]).is_none());
        let mut majority_large = vec![10.0; 4];
        majority_large.extend(std::iter::repeat_n(12.5, 8));
        assert!(small_caps_cap_size(&majority_large).is_none());
    }

    #[test]
    fn a_small_caps_line_paints_at_the_cap_size() {
        let bytes = small_caps_line_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let texts: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert_eq!(texts.len(), 1, "small caps split: {texts:?}");
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| TranslatedSegment {
                id: seg.id,
                page_index: seg.page_index,
                glyph_ids: seg.glyph_ids.clone(),
                source: seg.text.clone(),
                translated: "使用链接".into(),
            })
            .collect();
        let font = box_ttf(&"使用链接".chars().map(|ch| ch as u32).collect::<Vec<_>>());
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(font),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let painted = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract();
        let size = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode.chars().any(is_han_char))
            .map(|glyph| glyph.font_size)
            .fold(0.0f32, f32::max);
        assert!(
            size > 10.5,
            "small-caps Chinese used the small median, size={size}"
        );
    }

    #[test]
    fn a_chapter_index_is_not_absorbed_into_the_subentry_row() {
        let bytes = chapter_index_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| {
                let translated = if seg.text.contains("Search") {
                    "搜索链"
                } else if seg.text.contains("Research") {
                    "研究链"
                } else {
                    seg.text.trim()
                };
                TranslatedSegment {
                    id: seg.id,
                    page_index: seg.page_index,
                    glyph_ids: seg.glyph_ids.clone(),
                    source: seg.text.clone(),
                    translated: translated.into(),
                }
            })
            .collect();
        let font = box_ttf(
            &"搜索链研究链"
                .chars()
                .map(|ch| ch as u32)
                .collect::<Vec<_>>(),
        );
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(font),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let painted = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract();
        let fives: Vec<_> = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode.trim() == "5")
            .collect();
        assert!(!fives.is_empty(), "chapter 5 disappeared");
        let big = fives
            .iter()
            .find(|glyph| glyph.font_size >= 16.0)
            .expect("chapter 5 was painted at body size");
        let zh_y = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode.chars().any(is_han_char))
            .map(|glyph| glyph.matrix[5])
            .fold(f32::MIN, f32::max);
        assert!(
            (big.matrix[5] - zh_y).abs() > 8.0,
            "chapter 5 sat on the subentry line at y={} (row y={zh_y})",
            big.matrix[5]
        );
    }

    #[test]
    fn a_shorter_subentry_moves_the_chapter_index_with_the_rows_below() {
        // The 27pt index is an unchanged digit, so it never enters `drawn`.
        // A shorter block still has to lift it by the same unused leading as
        // the title, or `3` stays on the English baseline.
        let chars: Vec<u32> = "524".chars().map(|ch| ch as u32).collect();
        let font = subset_ttf(&box_ttf(&chars), &chars).expect("font");
        let glyph = |id, y, size, text: &str| Glyph {
            id,
            page_index: 0,
            unicode: text.into(),
            unmapped: false,
            char_code: text.as_bytes().to_vec(),
            gid: None,
            font_resource: "F1".into(),
            font_name: "Times-Bold".into(),
            font_object: None,
            font_size: size,
            matrix: [size, 0.0, 0.0, size, 40.0, y],
            bbox: [40.0, y, 50.0, y + size],
            advance: [size * 0.6, 0.0],
            fill_color: Color::black(),
            stroke_color: Color::black(),
            render_mode: 0,
            invisible: false,
            clipped: false,
            clip_uncertain: false,
            vertical: false,
            disposition: Disposition::KeptOriginal {
                reason: "unchanged".into(),
            },
            source: GlyphSource {
                kind: SourceKind::PageContent,
                object_id: Some("1 0".into()),
                stream_index: 0,
                operator_index: id,
                byte_start: 0,
                byte_end: 1,
                resource_name: Some("F1".into()),
            },
        };
        let extraction = Extraction {
            coordinate_space: "user",
            pages: Vec::new(),
            glyphs: vec![
                glyph(1, 442.0, 27.0, "5"),
                glyph(2, 600.0, 27.0, "2"),
                glyph(3, 400.0, 11.0, "4"),
            ],
            regions: Vec::new(),
            diagnostics: Vec::new(),
        };
        let mut drawn = Vec::new();
        let mut succeeded = HashSet::new();
        let mut keep = HashMap::from([(1, "unchanged".into())]);
        let shifts = vec![SubentryShift {
            page: 0,
            below_y: 508.0,
            delta: -12.0,
            own_ids: HashSet::new(),
        }];
        shift_kept_chapter_indexes(
            &mut drawn,
            &mut succeeded,
            &mut keep,
            &shifts,
            &extraction,
            &[("F1".into(), font)],
            &HashMap::new(),
        );
        assert_eq!(
            drawn.len(),
            1,
            "body digits or the index above the block moved"
        );
        assert_eq!(drawn[0].text, "5");
        assert!(
            (drawn[0].y - 454.0).abs() < 0.2,
            "chapter index y={} was not lifted by the unused 12pt",
            drawn[0].y
        );
        assert!(succeeded.contains(&1));
        assert!(!keep.contains_key(&1));
        assert!(!succeeded.contains(&2));
        assert!(!succeeded.contains(&3));
    }

    #[test]
    fn stacked_subentry_shifts_use_the_source_baseline() {
        let line = |y, ids: Vec<u32>| Drawn {
            page: 0,
            x: 0.0,
            y,
            size: 11.0,
            color: Color::black(),
            cids: Vec::new(),
            resource: "F".into(),
            skew: 0.0,
            text: "行".into(),
            glyph_ids: ids,
            widths: Vec::new(),
            gaps: Vec::new(),
            supers: Vec::new(),
            sup_scale: 1.0,
            sup_rise: 0.0,
        };
        // Upper block lifts everything below 500 by 12. The lower block's
        // source bottom is 400. A row at 390 is below both; applying the
        // first lift before the test used to carry it above 400 and drop
        // the second.
        let mut drawn = vec![line(390.0, vec![9]), line(380.0, vec![7])];
        let shifts = vec![
            SubentryShift {
                page: 0,
                below_y: 500.0,
                delta: -12.0,
                own_ids: HashSet::new(),
            },
            SubentryShift {
                page: 0,
                below_y: 400.0,
                delta: -12.0,
                own_ids: HashSet::from([7]),
            },
        ];
        apply_subentry_shifts(&mut drawn, &shifts);
        assert!(
            (drawn[0].y - 414.0).abs() < 0.2,
            "row under both blocks y={}",
            drawn[0].y
        );
        assert!(
            (drawn[1].y - 392.0).abs() < 0.2,
            "the block's own line took its own shift y={}",
            drawn[1].y
        );
    }

    #[test]
    fn subentry_rows_keep_each_title_with_its_page_number() {
        let bytes = subentry_rows_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| {
                let translated = if seg.text.trim().chars().all(|ch| ch.is_ascii_digit()) {
                    seg.text.as_str()
                } else if seg.text.contains("Q and A") && seg.text.contains("engines") {
                    "基于LLM的应用：摘要与问答引擎"
                } else if seg.text.contains("Q and A") {
                    "基于LLM的应用：摘要与问答"
                } else if seg.text.to_ascii_lowercase().contains("engines") {
                    "引擎"
                } else if seg.text.to_ascii_lowercase().contains("chatbots") {
                    "基于LLM的聊天机器人"
                } else if seg.text.to_ascii_lowercase().contains("agents") {
                    "AI智能体"
                } else if seg.text.contains("architecture") {
                    "LangChain架构"
                } else if seg.text.contains("object") && seg.text.contains("model") {
                    "LangChain的核心对象模型"
                } else if seg.text.contains("object") {
                    "LangChain的核心对象"
                } else if seg.text.contains("model") {
                    "模型"
                } else if seg.text.contains("1.2") || seg.text.contains("LangChain") {
                    "介绍LangChain"
                } else {
                    "简介"
                };
                TranslatedSegment {
                    id: seg.id,
                    page_index: seg.page_index,
                    glyph_ids: seg.glyph_ids.clone(),
                    source: seg.text.clone(),
                    translated: translated.into(),
                }
            })
            .collect();
        let font_chars: String = segments.iter().map(|seg| seg.translated.as_str()).collect();
        let codes: Vec<u32> = format!("{font_chars}■456781215")
            .chars()
            .map(|ch| ch as u32)
            .collect();
        let font = box_ttf(&codes);
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(font),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let painted = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract();
        let mut rows: Vec<InkRow> = Vec::new();
        for glyph in painted
            .glyphs
            .iter()
            .filter(|glyph| !glyph.unicode.trim().is_empty())
        {
            let y = glyph.matrix[5];
            let spot = InkSpot {
                text: glyph.unicode.clone(),
                x: glyph.matrix[4],
                advance: glyph.advance[0],
                skew: glyph.matrix[2],
            };
            if let Some(row) = rows.iter_mut().find(|row| (row.y - y).abs() < 2.4) {
                row.text.push_str(&glyph.unicode);
                row.glyphs.push(spot);
            } else {
                rows.push(InkRow {
                    y,
                    text: glyph.unicode.clone(),
                    glyphs: vec![spot],
                });
            }
        }
        for row in &rows {
            let chars = row.text.chars().filter(|ch| !ch.is_whitespace()).count();
            assert!(chars != 1, "orphan line: {}", row.text);
        }
        let fours: Vec<&InkSpot> = rows
            .iter()
            .flat_map(|row| row.glyphs.iter())
            .filter(|glyph| glyph.text == "4")
            .collect();
        assert_eq!(fours.len(), 1, "page number 4 was drawn more than once");
        let with_four = rows
            .iter()
            .find(|row| row.text.contains('4'))
            .expect("page 4");
        assert!(
            with_four.text.contains('答') && with_four.text.contains('擎'),
            "title split off its page number: {}",
            with_four.text
        );
        assert!(
            with_four.text.contains('机') || !with_four.text.contains('人'),
            "chatbots title orphaned: {}",
            with_four.text
        );
        let with_model = rows
            .iter()
            .find(|row| row.text.contains('模') && row.text.contains('5'))
            .expect("model 15");
        assert!(
            with_model.text.contains('象'),
            "核心对象 split from 模型 15: {}",
            with_model.text
        );
        let digit = with_four
            .glyphs
            .iter()
            .find(|item| item.text == "4")
            .expect("digit 4");
        let before = with_four
            .glyphs
            .iter()
            .filter(|item| item.x < digit.x)
            .max_by(|a, b| a.x.total_cmp(&b.x))
            .expect("title before 4");
        let gap = digit.x - (before.x + before.advance);
        assert!(
            gap < digit_size_limit(with_four),
            "wide gap {gap} before page number on {}",
            with_four.text
        );
        assert!(
            digit.skew.abs() < 0.4,
            "inline page number is italic, skew {}",
            digit.skew
        );
        let title = with_four
            .glyphs
            .iter()
            .find(|item| item.text == "答")
            .expect("答");
        assert!(
            title.skew.abs() > 1.0,
            "italic source title lost its shear, skew {}",
            title.skew
        );
        let lefts: Vec<f32> = rows
            .iter()
            .filter(|row| {
                row.text.contains('答') || row.text.contains('聊') || row.text.contains('架')
            })
            .map(|row| {
                row.glyphs
                    .iter()
                    .filter(|item| item.text != "■" && item.text != "•")
                    .map(|item| item.x)
                    .fold(f32::MAX, f32::min)
            })
            .collect();
        let min = lefts.iter().copied().fold(f32::MAX, f32::min);
        assert!(
            lefts.iter().all(|x| (x - min).abs() < 1.5),
            "sub-entry lines do not share the source indent: {lefts:?}"
        );
    }

    #[test]
    fn bulleted_chapter_lines_are_not_one_subentry_run() {
        let bytes = subentry_pdf(
            b"BT /F1 10 Tf \
1 0 0 1 72 500 Tm (1) Tj \
1 0 0 1 100 500 Tm [(Introduction) -800 (3)] TJ \
/F1 4 Tf \
1 0 0 1 90 502.4 Tm (\x95) Tj \
/F1 10 Tf \
1 0 0 1 72 490 Tm (2) Tj \
1 0 0 1 100 490 Tm [(Executing) -800 (27)] TJ \
/F1 4 Tf \
1 0 0 1 90 492.4 Tm (\x95) Tj \
ET",
        );
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| {
                let translated = if seg.text.trim().chars().all(|ch| ch.is_ascii_digit()) {
                    seg.text.as_str()
                } else if seg.text.contains("Introduction") {
                    "人工智能体与应用简介"
                } else if seg.text.contains("Executing") {
                    "执行"
                } else {
                    seg.text.as_str()
                };
                TranslatedSegment {
                    id: seg.id,
                    page_index: seg.page_index,
                    glyph_ids: seg.glyph_ids.clone(),
                    source: seg.text.clone(),
                    translated: translated.into(),
                }
            })
            .collect();
        let codes: Vec<u32> = "人工智能体与应用简介执行■12327"
            .chars()
            .map(|ch| ch as u32)
            .collect();
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(box_ttf(&codes)),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let painted = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract();
        let mut rows: Vec<(f32, String)> = Vec::new();
        for glyph in painted
            .glyphs
            .iter()
            .filter(|glyph| !glyph.unicode.trim().is_empty())
        {
            let y = glyph.matrix[5];
            if let Some(row) = rows.iter_mut().find(|row| (row.0 - y).abs() < 2.4) {
                row.1.push_str(&glyph.unicode);
            } else {
                rows.push((y, glyph.unicode.clone()));
            }
        }
        let intro = rows.iter().find(|row| row.1.contains('简')).expect("intro");
        assert!(
            !intro.1.contains('执') && !intro.1.contains('2'),
            "chapter 2 joined the previous entry: {}",
            intro.1
        );
        let next = rows
            .iter()
            .find(|row| row.1.contains('执'))
            .expect("chapter 2");
        assert!(
            next.1.contains('2') && next.1.contains('7'),
            "chapter 2 lost its number or page: {}",
            next.1
        );
    }

    fn digit_size_limit(row: &InkRow) -> f32 {
        let size = row
            .glyphs
            .iter()
            .find(|item| item.text == "4")
            .map(|item| item.advance)
            .unwrap_or(8.0);
        // An italic title adds 0.2em so the shear still leaves half an em.
        size.max(6.0) + 2.2
    }

    fn subentry_rows_pdf() -> Vec<u8> {
        // The bullet sits a point above the text, as a dingbat does, so the
        // title line is `engines 4 chatbots 7` and the marker is not a token.
        subentry_pdf(
            b"BT /F1 10 Tf \
1 0 0 1 72 520 Tm (LLM-based applications: Q and A) Tj \
1 0 0 1 72 508 Tm [(engines) -700 (4) -1800 (Chatbots) -700 (7) -1800 (Agents) -700 (8)] TJ \
/F1 4 Tf \
1 0 0 1 90 510.6 Tm (\x95) Tj \
1 0 0 1 190 510.6 Tm (\x95) Tj \
/F1 10 Tf \
1 0 0 1 72 488 Tm [(1.2 LangChain) -2800 (11)] TJ \
1 0 0 1 72 464 Tm [(LangChain architecture) -700 (12) -1800 (LangChain core object)] TJ \
/F1 4 Tf \
1 0 0 1 210 466.6 Tm (\x95) Tj \
/F1 10 Tf \
1 0 0 1 72 452 Tm [(model) -700 (15)] TJ \
ET",
        )
    }

    fn subentry_pdf(content: &[u8]) -> Vec<u8> {
        contents_pdf_named(content, "NewBaskerville-Italic")
    }

    #[test]
    fn an_italic_subentry_keeps_its_page_number_upright() {
        // -1400 at 10pt is about 14pt, wider than the tight-gap keep, so the
        // old path slid the number to the column edge and sheared it.
        let bytes = subentry_pdf(
            b"BT /F1 10 Tf \
1 0 0 1 72 500 Tm [(Implementing a prompt template with a Python function) -1400 (32)] TJ \
ET",
        );
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| {
                let translated = if seg.text.contains("Python") {
                    "使用Python函数实现提示词模板"
                } else {
                    seg.text.as_str()
                };
                TranslatedSegment {
                    id: seg.id,
                    page_index: seg.page_index,
                    glyph_ids: seg.glyph_ids.clone(),
                    source: seg.text.clone(),
                    translated: translated.into(),
                }
            })
            .collect();
        let font = box_ttf(
            &"使用Python函数实现提示词模板32"
                .chars()
                .map(|ch| ch as u32)
                .collect::<Vec<_>>(),
        );
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(font),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let painted = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract();
        let text: String = painted
            .glyphs
            .iter()
            .filter(|glyph| (glyph.matrix[5] - 500.0).abs() < 8.0 || glyph.unicode == "板")
            .map(|glyph| glyph.unicode.as_str())
            .collect();
        assert!(
            text.contains('板') && text.contains('3'),
            "title split from its page number: {text}"
        );
        let digit = painted
            .glyphs
            .iter()
            .find(|glyph| glyph.unicode == "3")
            .expect("page digit");
        let title_end = painted
            .glyphs
            .iter()
            .find(|glyph| glyph.unicode.contains('板'))
            .expect("title end");
        let gap = digit.matrix[4] - (title_end.matrix[4] + title_end.advance[0]);
        assert!(gap < 8.0, "page number gap {gap} is wider than one space");
        assert!(
            gap > 5.5,
            "italic title needs a half-em before the page number, gap {gap}"
        );
        assert!(
            digit.matrix[2].abs() < 0.4,
            "page number is italic, skew {}",
            digit.matrix[2]
        );
        assert_eq!(
            painted
                .glyphs
                .iter()
                .filter(|glyph| glyph.unicode == "3")
                .count(),
            1,
            "page digit drawn twice"
        );
    }

    #[test]
    fn an_unchanged_title_beside_its_page_number_is_not_blanked() {
        let bytes = rag_row_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| {
                let translated = if seg.text.contains("Prompt") {
                    "提示工程"
                } else if seg.text.contains("Fine") {
                    "微调"
                } else {
                    seg.text.as_str()
                };
                TranslatedSegment {
                    id: seg.id,
                    page_index: seg.page_index,
                    glyph_ids: seg.glyph_ids.clone(),
                    source: seg.text.clone(),
                    translated: translated.into(),
                }
            })
            .collect();
        let font = box_ttf(
            &"提示工程微调RAG20"
                .chars()
                .map(|ch| ch as u32)
                .collect::<Vec<_>>(),
        );
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(font),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let text = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract()
            .plain_text();
        assert!(text.contains("提示工程"), "{text}");
        assert!(text.contains("微调"), "{text}");
        assert!(text.contains("RAG"), "{text}");
    }

    #[test]
    fn a_wrapped_contents_title_clears_the_next_heading() {
        let bytes = wrapped_title_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| {
                let translated = if seg.text.contains("chatbots") {
                    "基于大语言模型的聊天机器人"
                } else {
                    seg.text.as_str()
                };
                TranslatedSegment {
                    id: seg.id,
                    page_index: seg.page_index,
                    glyph_ids: seg.glyph_ids.clone(),
                    source: seg.text.clone(),
                    translated: translated.into(),
                }
            })
            .collect();
        let font = box_ttf(
            &"基于大语言模型的聊天机器人1.2LangChain"
                .chars()
                .map(|ch| ch as u32)
                .collect::<Vec<_>>(),
        );
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(font),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let painted = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract();
        let mut cjk: Vec<&Glyph> = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode.chars().any(is_han_char))
            .collect();
        assert!(
            cjk.len() > 4,
            "wrapped title missing: {:?}",
            painted.plain_text()
        );
        cjk.sort_by(|left, right| right.matrix[5].total_cmp(&left.matrix[5]));
        let top = cjk[0].matrix[5];
        let lower: Vec<_> = cjk
            .iter()
            .copied()
            .filter(|glyph| top - glyph.matrix[5] > 1.0)
            .collect();
        assert!(!lower.is_empty(), "title stayed on one line: {top}");
        let wrap_bottom = lower
            .iter()
            .map(|glyph| glyph.bbox[1].min(glyph.bbox[3]))
            .fold(f32::MAX, f32::min);
        let heading_top = painted
            .glyphs
            .iter()
            .filter(|glyph| glyph.unicode == "L" || glyph.unicode == "1")
            .map(|glyph| glyph.bbox[1].max(glyph.bbox[3]))
            .fold(0.0f32, f32::max);
        assert!(
            wrap_bottom > heading_top + 1.0,
            "wrap bottom {wrap_bottom} does not clear heading top {heading_top}"
        );
    }

    #[test]
    fn a_label_value_list_shares_one_colon_axis_and_one_name_column() {
        let bytes = contents_pdf(
            b"BT /F1 10 Tf \
1 0 0 1 66 700 Tm (For online information and ordering of this and other Manning books, please visit the office today.) Tj \
1 0 0 1 66 686 Tm (The publisher offers discounts on this book when ordered in quantity for a class or a team.) Tj \
1 0 0 1 66 672 Tm (For more information please contact the sales desk before the end of the current month.) Tj \
1 0 0 1 72 520 Tm (Manning Co.) Tj \
1 0 0 1 240 520 Tm (Development editor: Ada Lovelace) Tj \
1 0 0 1 72 506 Tm (20 Road St) Tj \
1 0 0 1 275 506 Tm (Copy editor: Ann Lee) Tj \
1 0 0 1 72 492 Tm (PO 761) Tj \
1 0 0 1 340 492 Tm (and Lin Mei) Tj \
1 0 0 1 185 478 Tm (Typesetter and cover designer: Mo Yan) Tj \
ET",
        );
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| {
                let translated = if seg.text.contains("Development editor") {
                    "开发编辑: Ada Lovelace"
                } else if seg.text.contains("Copy editor") {
                    "校对: Ann Lee"
                } else if seg.text.contains("Typesetter") {
                    "排版师与封面设计师: Mo Yan"
                } else if seg.text.contains("and Lin") {
                    "和 Lin Mei"
                } else if seg.text.contains("Manning Co") {
                    "曼宁出版公司"
                } else if seg.text.contains("20 Road") {
                    "20 鲍德温路"
                } else if seg.text.contains("PO 761") {
                    "邮政信箱 761"
                } else {
                    seg.text.as_str()
                };
                TranslatedSegment {
                    id: seg.id,
                    page_index: seg.page_index,
                    glyph_ids: seg.glyph_ids.clone(),
                    source: seg.text.clone(),
                    translated: translated.to_string(),
                }
            })
            .collect();
        let mut chars: Vec<u32> = segments
            .iter()
            .flat_map(|seg| seg.translated.chars().chain(seg.source.chars()))
            .map(|ch| ch as u32)
            .collect();
        chars.extend("： ".chars().map(|ch| ch as u32));
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(box_ttf(&chars)),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let painted = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract();
        let mut glyphs: Vec<&Glyph> = painted
            .glyphs
            .iter()
            .filter(|glyph| {
                glyph.font_name.contains("RPT") || glyph.unicode.chars().any(is_han_char)
            })
            .collect();
        glyphs.sort_by(|left, right| {
            right.matrix[5]
                .total_cmp(&left.matrix[5])
                .then(left.matrix[4].total_cmp(&right.matrix[4]))
        });
        let mut rows: Vec<Vec<&Glyph>> = Vec::new();
        for glyph in glyphs {
            let same = rows.last().is_some_and(|row: &Vec<&Glyph>| {
                let prev = row[row.len() - 1];
                (prev.matrix[5] - glyph.matrix[5]).abs() < 1.5
                    && glyph.matrix[4] - prev.bbox[2] < 14.0
            });
            if same {
                rows.last_mut().unwrap().push(glyph);
            } else {
                rows.push(vec![glyph]);
            }
        }
        let texts: Vec<String> = rows
            .iter()
            .map(|row| row.iter().map(|glyph| glyph.unicode.as_str()).collect())
            .collect();
        let row_text = |needles: &[&str]| -> Vec<&Glyph> {
            rows.iter()
                .find(|row| {
                    let text: String = row.iter().map(|glyph| glyph.unicode.as_str()).collect();
                    needles.iter().all(|needle| text.contains(needle))
                })
                .unwrap_or_else(|| panic!("missing {needles:?} in {texts:?}"))
                .clone()
        };
        let colon_right = |row: &[&Glyph]| -> f32 {
            let colon = row
                .iter()
                .find(|glyph| glyph.unicode == "：")
                .unwrap_or_else(|| panic!("missing fullwidth colon in {row:?}"));
            colon.bbox[2].max(colon.matrix[4])
        };
        let name_x = |row: &[&Glyph], name: &str| -> f32 {
            let needle = name.chars().next().unwrap();
            row.iter()
                .find(|glyph| glyph.unicode.starts_with(needle))
                .map(|glyph| glyph.matrix[4])
                .unwrap_or_else(|| panic!("{name} missing in {row:?}"))
        };
        let dev = row_text(&["开发编辑", "Ada"]);
        let copy = row_text(&["校对", "Ann"]);
        let set = row_text(&["排版师", "Mo"]);
        let cont = row_text(&["和", "Lin"]);
        let rights = [colon_right(&dev), colon_right(&copy), colon_right(&set)];
        let names = [
            name_x(&dev, "Ada"),
            name_x(&copy, "Ann"),
            name_x(&set, "Mo"),
            name_x(&cont, "和"),
        ];
        let colon_span = rights.iter().copied().fold(rights[0], f32::max)
            - rights.iter().copied().fold(rights[0], f32::min);
        assert!(colon_span < 1.5, "colons are not on one axis: {rights:?}");
        let name_span = names.iter().copied().fold(names[0], f32::max)
            - names.iter().copied().fold(names[0], f32::min);
        assert!(
            name_span < 1.5,
            "names and the continuation are not on one column: {names:?}"
        );
        assert!(
            names[0] > rights[0] + 1.0,
            "name overlaps the colon: colon {rights:?} name {names:?}"
        );
        let company = row_text(&["曼宁出版公司"]);
        let street = row_text(&["鲍德温"]);
        let post = row_text(&["邮政信箱"]);
        let lefts = [company[0].matrix[4], street[0].matrix[4], post[0].matrix[4]];
        let left_span = lefts.iter().copied().fold(lefts[0], f32::max)
            - lefts.iter().copied().fold(lefts[0], f32::min);
        assert!(
            left_span < 1.0,
            "address rows do not share a left edge: {lefts:?}"
        );
        let post_text: String = post.iter().map(|glyph| glyph.unicode.as_str()).collect();
        assert_eq!(post_text, "邮政信箱 761", "{post_text}");
        let sizes = [company[0].font_size, street[0].font_size, post[0].font_size];
        let size_span = sizes.iter().copied().fold(sizes[0], f32::max)
            - sizes.iter().copied().fold(sizes[0], f32::min);
        assert!(size_span < 0.6, "address rows are not one size: {sizes:?}");
    }

    #[test]
    fn side_by_side_credits_that_share_an_operator_are_both_translated() {
        let bytes = shared_credit_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        let texts: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| seg.text.clone())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("Manning"))
                && texts.iter().any(|text| text.contains("Development")),
            "credits should be two segments: {texts:?}"
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| {
                let translated = if seg.text.contains("Manning") {
                    "曼宁出版公司"
                } else {
                    "开发编辑：Ada"
                };
                TranslatedSegment {
                    id: seg.id,
                    page_index: seg.page_index,
                    glyph_ids: seg.glyph_ids.clone(),
                    source: seg.text.clone(),
                    translated: translated.into(),
                }
            })
            .collect();
        let font = box_ttf(
            &"曼宁出版公司开发编辑：Ada"
                .chars()
                .chain("Manning Publications Co. Development editor: Ada".chars())
                .map(|ch| ch as u32)
                .collect::<Vec<_>>(),
        );
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(font),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let text = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract()
            .plain_text();
        assert!(text.contains("曼宁出版公司"), "{text}");
        assert!(text.contains("开发编辑"), "{text}");
        assert!(!text.contains("Manning"), "{text}");
        assert!(!text.contains("Development"), "{text}");
    }

    #[test]
    fn an_echoed_credit_column_does_not_keep_its_neighbor_english() {
        let bytes = shared_credit_pdf();
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .map(|seg| {
                let translated = if seg.text.contains("Development") {
                    "开发编辑：Ada".to_string()
                } else {
                    seg.text.clone()
                };
                TranslatedSegment {
                    id: seg.id,
                    page_index: seg.page_index,
                    glyph_ids: seg.glyph_ids.clone(),
                    source: seg.text.clone(),
                    translated,
                }
            })
            .collect();
        let font = box_ttf(
            &"开发编辑：AdaManning Publications Co."
                .chars()
                .map(|ch| ch as u32)
                .collect::<Vec<_>>(),
        );
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(font),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        let text = PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract()
            .plain_text();
        assert!(text.contains("开发编辑"), "{text}");
        assert!(text.contains("Manning"), "{text}");
        assert!(!text.contains("Development"), "{text}");
    }

    #[test]
    fn a_body_paragraph_reflows_without_splitting_api_or_a_semicolon() {
        let bytes = contents_pdf(
            b"BT /F1 10 Tf \
1 0 0 1 72 700 Tm (You may be an engineer who wants to move past a simple) Tj \
1 0 0 1 72 686 Tm (API call and turn a prototype into something reliable) Tj \
1 0 0 1 72 672 Tm (for a team that ships products every single week.) Tj \
ET",
        );
        let zh = "您可能是一位希望超越简单 API 调用的工程师，并将原型转化为可靠的产品；团队每周都会发布产品。";
        let painted = paint_translation(&bytes, &[(0, zh.to_string())]);
        let lines = glyph_lines(&painted);
        let joined = lines.concat();
        assert!(joined.contains("API"), "{lines:?}");
        assert!(
            lines.iter().all(
                |line| !line.starts_with('；') && !line.contains("AP I") && {
                    let chars: Vec<char> = line.chars().collect();
                    !chars
                        .windows(2)
                        .any(|pair| pair == ['A', 'P'] && !line.contains("API"))
                }
            ),
            "{lines:?}"
        );
        assert!(lines.iter().any(|line| line.contains("API")), "{lines:?}");
        assert!(
            lines
                .iter()
                .all(|line| !line.ends_with("AP") && !line.starts_with('I')),
            "{lines:?}"
        );
    }

    #[test]
    fn a_bullet_list_shares_one_marker_and_one_text_edge() {
        let bytes = contents_pdf(
            b"BT /F1 10 Tf \
0.8 0.651 0.349 rg \
1 0 0 1 108.8 420 Tm (\x95) Tj \
0.148 g \
1 0 0 1 120.8 420 Tm (You have an OpenAI API key.) Tj \
0.8 0.651 0.349 rg \
1 0 0 1 108.8 406 Tm (\x95) Tj \
0.148 g \
1 0 0 1 120.8 406 Tm (You know how to create a notebook.) Tj \
ET",
        );
        let painted = paint_translation(
            &bytes,
            &[
                (0, " 您拥有 OpenAI API 密钥。".into()),
                (1, "\u{f0a1} 您知道如何创建笔记本环境。".into()),
            ],
        );
        let markers: Vec<_> = painted
            .glyphs
            .iter()
            .filter(|glyph| {
                glyph.unicode == "\u{2022}" || glyph.unicode == "•" || glyph.unicode == "■"
            })
            .collect();
        assert_eq!(markers.len(), 2, "markers: {markers:?}");
        let first = &markers[0];
        for marker in &markers {
            assert!((marker.matrix[4] - first.matrix[4]).abs() < 0.4, "marker x");
            assert!(
                (marker.font_size - first.font_size).abs() < 0.2,
                "marker size"
            );
            assert_eq!(marker.fill_color, first.fill_color, "marker colour");
            assert_eq!(marker.unicode, first.unicode, "marker glyph");
            assert!(
                marker.fill_color.components.first().copied().unwrap_or(0.0) > 0.5,
                "marker lost the source colour: {:?}",
                marker.fill_color
            );
        }
        let mut rows: Vec<(f32, f32)> = Vec::new();
        for glyph in &painted.glyphs {
            if !glyph.unicode.chars().any(is_han_char) {
                continue;
            }
            if let Some(row) = rows
                .iter_mut()
                .find(|(y, _)| (*y - glyph.matrix[5]).abs() < 0.8)
            {
                row.1 = row.1.min(glyph.matrix[4]);
            } else {
                rows.push((glyph.matrix[5], glyph.matrix[4]));
            }
        }
        assert!(rows.len() >= 2, "both items should draw: {rows:?}");
        let text_x = rows[0].1;
        for (_, x) in &rows {
            assert!(
                (*x - text_x).abs() < 0.6,
                "item text x {x:.1} vs {text_x:.1}"
            );
            assert!(
                (*x - 120.8).abs() < 1.5,
                "text left the source hanging indent: {x:.1}"
            );
        }
        for marker in &markers {
            assert!(
                rows.iter()
                    .any(|(y, _)| (*y - marker.matrix[5]).abs() < 0.8),
                "marker y {:.1} is not on an item baseline: {rows:?}",
                marker.matrix[5]
            );
        }
    }

    #[test]
    fn a_wrapped_bullet_matches_its_one_line_sibling() {
        let bytes = contents_pdf(
            b"BT \
/F1 8.5 Tf \
0.278 0.42 0.522 rg \
1 0 0 1 96.2 340 Tm (\x95) Tj \
/F1 10 Tf \
0.148 g \
1 0 0 1 108.2 340 Tm (Core challenges in building applications) Tj \
1 0 0 1 108.2 326 Tm (powered by large language models.) Tj \
/F1 8.5 Tf \
0.278 0.42 0.522 rg \
1 0 0 1 96.2 312 Tm (\x95) Tj \
/F1 10 Tf \
0.148 g \
1 0 0 1 108.2 312 Tm (Patterns for engines and agents.) Tj \
ET",
        );
        let painted = paint_translation(
            &bytes,
            &[
                (0, "构建由大语言模型驱动的应用程序所面临的核心挑战".into()),
                (1, "引擎、聊天机器人和智能体的模式".into()),
            ],
        );
        let markers: Vec<_> = painted
            .glyphs
            .iter()
            .filter(|glyph| {
                glyph.unicode == "•" || glyph.unicode == "\u{2022}" || glyph.unicode == "■"
            })
            .collect();
        assert_eq!(markers.len(), 2, "markers: {markers:?}");
        for marker in &markers {
            assert!((marker.matrix[4] - 96.2).abs() < 0.6, "marker x");
            assert!(
                (marker.font_size - 8.5).abs() < 0.3,
                "marker used the body size: {}",
                marker.font_size
            );
            assert!(
                marker.fill_color.components.first().copied().unwrap_or(0.0) > 0.2
                    && marker.fill_color.components.first().copied().unwrap_or(0.0) < 0.4,
                "marker colour {:?}",
                marker.fill_color
            );
            assert_eq!(marker.unicode, markers[0].unicode);
        }
        let mut rows: Vec<(f32, f32)> = Vec::new();
        for glyph in &painted.glyphs {
            if !glyph.unicode.chars().any(is_han_char) {
                continue;
            }
            if let Some(row) = rows
                .iter_mut()
                .find(|(y, _)| (*y - glyph.matrix[5]).abs() < 0.8)
            {
                row.1 = row.1.min(glyph.matrix[4]);
            } else {
                rows.push((glyph.matrix[5], glyph.matrix[4]));
            }
        }
        assert!(rows.len() >= 2, "{rows:?}");
        for (y, x) in &rows {
            assert!((*x - 108.2).abs() < 1.5, "text x {x:.1} on y {y:.1}");
        }
    }

    #[test]
    fn a_gold_bullet_does_not_paint_the_part_text_gold() {
        let bytes = contents_pdf_named(
            b"BT /F1 10 Tf \
0.8 0.651 0.349 rg \
1 0 0 1 72 500 Tm (- ) Tj \
0.148 g \
1 0 0 1 78 500 Tm (Part 1: Getting started with LLMs today) Tj \
1 0 0 1 96 486 Tm (lays the foundation for the later chapters.) Tj \
ET",
            "NewBaskerville-Italic",
        );
        let zh = "第 1 部分：入门（第 1–2 章）——本部分奠定基础。";
        let painted = paint_translation(&bytes, &[(0, zh.to_string())]);
        let mut gold = 0;
        let mut text = 0;
        for glyph in &painted.glyphs {
            if glyph.font_name != "RPTCJK"
                && !glyph.unicode.chars().any(is_han_char)
                && glyph.unicode != "■"
                && glyph.unicode != "-"
            {
                continue;
            }
            let rgb = glyph.fill_color.components.first().copied().unwrap_or(0.0);
            let goldish = glyph.fill_color.components.first().copied().unwrap_or(0.0) > 0.5;
            if glyph.unicode == "■" || glyph.unicode == "-" {
                if goldish {
                    gold += 1;
                }
            } else if glyph.unicode.chars().any(is_han_char) {
                text += 1;
                assert!(
                    rgb < 0.4,
                    "han {} is gold {:?}",
                    glyph.unicode,
                    glyph.fill_color
                );
            }
        }
        assert!(gold >= 1, "marker lost its colour");
        assert!(text > 4, "part text missing");
        let italic = painted
            .glyphs
            .iter()
            .any(|glyph| glyph.unicode.chars().any(is_han_char) && glyph.matrix[2].abs() > 0.1);
        let roman = painted
            .glyphs
            .iter()
            .any(|glyph| glyph.unicode == "本" && glyph.matrix[2].abs() < 0.05);
        assert!(italic, "part lead should keep the italic shear");
        assert!(roman, "explanation after the em dash should be roman");
    }

    #[test]
    fn a_short_list_item_does_not_leave_a_hole_before_the_next() {
        let bytes = contents_pdf(
            b"BT /F1 10 Tf \
1 0 0 1 72 500 Tm (- Part one is a long item that wraps) Tj \
1 0 0 1 90 486 Tm (onto a second line of the same item) Tj \
1 0 0 1 90 472 Tm (and finishes on a third line here.) Tj \
1 0 0 1 72 458 Tm (- Chapter two follows immediately.) Tj \
ET",
        );
        let painted = paint_translation(
            &bytes,
            &[
                (0, "第 1 部分很短。".into()),
                (1, "第 2 章紧随其后。".into()),
            ],
        );
        let mut rows: Vec<(f32, String)> = Vec::new();
        for glyph in &painted.glyphs {
            if !glyph.unicode.chars().any(is_han_char)
                && glyph.unicode != "■"
                && glyph.unicode != "-"
            {
                continue;
            }
            if let Some(row) = rows
                .iter_mut()
                .find(|(y, _)| (*y - glyph.matrix[5]).abs() < 1.0)
            {
                row.1.push_str(&glyph.unicode);
            } else {
                rows.push((glyph.matrix[5], glyph.unicode.clone()));
            }
        }
        rows.sort_by(|left, right| right.0.total_cmp(&left.0));
        assert!(rows.len() >= 2, "{rows:?}");
        let gap = rows[0].0 - rows[1].0;
        assert!(gap < 24.0, "list hole is {gap}pt between {rows:?}");
    }

    fn paint_translation(bytes: &[u8], which: &[(usize, String)]) -> Extraction {
        let mut doc = PdfDocument::open_bytes(bytes).unwrap();
        let mut extraction = doc.extract();
        let segmentation = crate::segment::segment_placed(
            &extraction.glyphs,
            &crate::segment::SegmentFlags::default(),
            &extraction.regions,
            &extraction.pages,
        );
        for (id, reason) in &segmentation.kept {
            extraction.mark_kept(*id, reason.clone()).unwrap();
        }
        let mut chars: Vec<u32> = which
            .iter()
            .flat_map(|(_, text)| text.chars().map(|ch| ch as u32))
            .collect();
        chars.extend("■—–API ：".chars().map(|ch| ch as u32));
        for seg in &segmentation.segments {
            chars.extend(seg.text.chars().map(|ch| ch as u32));
        }
        let segments: Vec<_> = segmentation
            .segments
            .iter()
            .enumerate()
            .map(|(index, seg)| {
                let translated = which
                    .iter()
                    .find(|(slot, _)| *slot == index)
                    .map(|(_, text)| text.clone())
                    .unwrap_or_else(|| seg.text.clone());
                TranslatedSegment {
                    id: seg.id,
                    page_index: seg.page_index,
                    glyph_ids: seg.glyph_ids.clone(),
                    source: seg.text.clone(),
                    translated,
                }
            })
            .collect();
        doc.rewrite(
            &mut extraction,
            &TranslateReport {
                segments,
                calls: 0,
                cache_hits: 0,
            },
            &RewriteOptions {
                font_bytes: Some(box_ttf(&chars)),
                ..RewriteOptions::default()
            },
        )
        .unwrap();
        PdfDocument::open_bytes(&doc.save_bytes().unwrap())
            .unwrap()
            .extract()
    }

    fn glyph_lines(extraction: &Extraction) -> Vec<String> {
        let mut glyphs: Vec<&Glyph> = extraction
            .glyphs
            .iter()
            .filter(|glyph| {
                glyph.font_name.contains("RPT") || glyph.unicode.chars().any(is_han_char)
            })
            .collect();
        glyphs.sort_by(|left, right| {
            right.matrix[5]
                .total_cmp(&left.matrix[5])
                .then(left.matrix[4].total_cmp(&right.matrix[4]))
        });
        let mut lines: Vec<Vec<&Glyph>> = Vec::new();
        for glyph in glyphs {
            let same = lines.last().is_some_and(|line: &Vec<&Glyph>| {
                (line[0].matrix[5] - glyph.matrix[5]).abs() < 1.5
            });
            if same {
                lines.last_mut().unwrap().push(glyph);
            } else {
                lines.push(vec![glyph]);
            }
        }
        lines
            .into_iter()
            .map(|line| line.iter().map(|glyph| glyph.unicode.as_str()).collect())
            .collect()
    }

    fn toc_column_pdf() -> Vec<u8> {
        let mut doc = Document::with_version("1.4");
        doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        let pages_id = doc.new_object_id();
        let widths: Vec<lopdf::Object> = (0..256).map(|_| lopdf::Object::Integer(500)).collect();
        let font = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
            "FirstChar" => 0,
            "LastChar" => 255,
            "Widths" => widths,
        });
        let mut fonts = lopdf::Dictionary::new();
        fonts.set("F1", font);
        let mut resources = lopdf::Dictionary::new();
        resources.set("Font", fonts);
        // One TJ per row, matching a contents line whose page number shares the
        // title's operator. -1000 is a 12pt gap at 12pt: wide enough to detach
        // the number, narrow enough that it is not a second column.
        let content = b"BT /F1 12 Tf \
1 0 0 1 72 700 Tm [(Agents) -1000 (27)] TJ \
1 0 0 1 72 670 Tm [(Query generation, routing, and retrieval postprocessing) -1000 (228)] TJ \
ET"
        .to_vec();
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

    fn small_caps_line_pdf() -> Vec<u8> {
        // Initials at 12pt, the rest at 9.6pt. Width 500 makes 6pt and 4.8pt.
        contents_pdf(
            b"BT \
/F1 12 Tf 1 0 0 1 200 500 Tm (W) Tj \
/F1 9.6 Tf 1 0 0 1 206 500 Tm (ITH ) Tj \
/F1 12 Tf 1 0 0 1 225.2 500 Tm (L) Tj \
/F1 9.6 Tf 1 0 0 1 231.2 500 Tm (ANG ) Tj \
/F1 12 Tf 1 0 0 1 250.4 500 Tm (C) Tj \
/F1 9.6 Tf 1 0 0 1 256.4 500 Tm (HAIN) Tj \
ET",
        )
    }

    fn chapter_index_pdf() -> Vec<u8> {
        // The 27pt chapter index sits 30pt under the bullet row. That gap is
        // inside 27pt * 1.45, which used to pull the numeral into the row.
        contents_pdf(
            b"BT /F1 11 Tf \
1 0 0 1 72 520 Tm (Search chain) Tj \
1 0 0 1 160 520 Tm (91) Tj \
1 0 0 1 190 520 Tm (\x95) Tj \
1 0 0 1 210 520 Tm (Research chain) Tj \
1 0 0 1 310 520 Tm (99) Tj \
/F1 27 Tf \
1 0 0 1 105 490 Tm (5) Tj \
ET",
        )
    }

    fn right_aligned_title_pdf() -> Vec<u8> {
        // Width 500 at 18pt is 9pt per character. Each line ends at x=500.
        contents_pdf(
            b"BT /F1 18 Tf \
1 0 0 1 266 520 Tm (AI Agents and Applications) Tj \
1 0 0 1 302 480 Tm (with LangChain and MCP) Tj \
1 0 0 1 374 440 Tm (First printing) Tj \
ET",
        )
    }

    fn inline_numbers_pdf() -> Vec<u8> {
        let mut doc = Document::with_version("1.4");
        doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        let pages_id = doc.new_object_id();
        let widths: Vec<lopdf::Object> = (0..256).map(|_| lopdf::Object::Integer(500)).collect();
        let font = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
            "FirstChar" => 0,
            "LastChar" => 255,
            "Widths" => widths,
        });
        let mut fonts = lopdf::Dictionary::new();
        fonts.set("F1", font);
        let mut resources = lopdf::Dictionary::new();
        resources.set("Font", fonts);
        // -700 is a tight page-number gap. -2200 separates the next entry.
        // The second row's number is the only one on its baseline.
        let content = b"BT /F1 12 Tf \
1 0 0 1 72 700 Tm [(engines) -700 (4) -2200 (chatbots) -700 (7) -2200 (agents) -700 (8)] TJ \
1 0 0 1 72 670 Tm [(Introduction) -1000 (3)] TJ \
ET"
        .to_vec();
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

    fn rag_row_pdf() -> Vec<u8> {
        contents_pdf(
            b"BT /F1 12 Tf \
1 0 0 1 72 500 Tm [(Prompt engineering) -900 (20)] TJ \
1 0 0 1 250 500 Tm [(RAG) -700 (20)] TJ \
1 0 0 1 340 500 Tm [(Fine-tuning) -900 (22)] TJ \
ET",
        )
    }

    fn wrapped_title_pdf() -> Vec<u8> {
        contents_pdf(
            b"BT /F1 10 Tf \
1 0 0 1 72 400 Tm (chatbots) Tj \
1 0 0 1 72 383 Tm (1.2 LangChain) Tj \
ET",
        )
    }

    #[test]
    fn a_leaked_page_transform_does_not_move_the_translation() {
        if !std::path::Path::new("/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc")
            .is_file()
        {
            return;
        }
        let bytes = contents_pdf(
            b"1 0 0 1 80 40 cm\nBT /F1 12 Tf 1 0 0 1 72 500 Tm (This sentence is long enough to count as ordinary body text in the column today.) Tj ET",
        );
        let mut doc = PdfDocument::open_bytes(&bytes).unwrap();
        let mut extraction = doc.extract();
        let source_y = extraction
            .glyphs
            .iter()
            .find(|glyph| glyph.unicode == "T")
            .map(|glyph| glyph.matrix[5])
            .unwrap();
        struct ToChinese;
        impl Translator for ToChinese {
            fn complete(&self, _system: &str, user: &str) -> Result<String> {
                let payload: serde_json::Value = serde_json::from_str(user).unwrap();
                let translations: Vec<serde_json::Value> = payload["segments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|seg| {
                        serde_json::json!({"id": seg["id"], "text": "这句正文应当留在原来的基线上而不要跟着页面变换走。"})
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
        let again = PdfDocument::open_bytes(&saved).unwrap().extract();
        let painted = again
            .glyphs
            .iter()
            .find(|glyph| glyph.unicode.contains('应'))
            .unwrap_or_else(|| panic!("missing translation: {}", again.plain_text()));
        assert!(
            (painted.matrix[5] - source_y).abs() < 3.0,
            "painted y {} source y {source_y}",
            painted.matrix[5]
        );
    }

    fn contents_pdf(content: &[u8]) -> Vec<u8> {
        contents_pdf_named(content, "Helvetica")
    }

    fn contents_pdf_named(content: &[u8], font_name: &str) -> Vec<u8> {
        let mut doc = Document::with_version("1.4");
        doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        let pages_id = doc.new_object_id();
        let widths: Vec<lopdf::Object> = (0..256).map(|_| lopdf::Object::Integer(500)).collect();
        let mut font_dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "Encoding" => "WinAnsiEncoding",
            "FirstChar" => 0,
            "LastChar" => 255,
            "Widths" => widths,
        };
        font_dict.set(
            "BaseFont",
            lopdf::Object::Name(font_name.as_bytes().to_vec()),
        );
        let font = doc.add_object(font_dict);
        let mut fonts = lopdf::Dictionary::new();
        fonts.set("F1", font);
        let mut resources = lopdf::Dictionary::new();
        resources.set("Font", fonts);
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.to_vec()));
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

    fn shared_credit_pdf() -> Vec<u8> {
        let mut doc = Document::with_version("1.4");
        doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        let pages_id = doc.new_object_id();
        let widths: Vec<lopdf::Object> = (0..256).map(|_| lopdf::Object::Integer(500)).collect();
        let font = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
            "FirstChar" => 0,
            "LastChar" => 255,
            "Widths" => widths,
        });
        let mut fonts = lopdf::Dictionary::new();
        fonts.set("F1", font);
        let mut resources = lopdf::Dictionary::new();
        resources.set("Font", fonts);
        // One TJ, two columns. -8000 at 10pt is an 80pt gutter.
        let content = b"BT /F1 10 Tf \
1 0 0 1 72 400 Tm [(Manning Publications Co.) -8000 (Development editor: Ada)] TJ \
ET"
        .to_vec();
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

    fn toc_row_pdf() -> Vec<u8> {
        let mut doc = Document::with_version("1.4");
        doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        let pages_id = doc.new_object_id();
        let widths: Vec<lopdf::Object> = (0..256).map(|_| lopdf::Object::Integer(500)).collect();
        let font = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
            "FirstChar" => 0,
            "LastChar" => 255,
            "Widths" => widths,
        });
        let mut fonts = lopdf::Dictionary::new();
        fonts.set("F1", font);
        let mut resources = lopdf::Dictionary::new();
        resources.set("Font", fonts);
        let content =
            b"BT /F1 12 Tf 72 700 Td [(Introduction to AI agents) -4000 (........) -80 ( ) -80 (3)] TJ ET"
                .to_vec();
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
}
