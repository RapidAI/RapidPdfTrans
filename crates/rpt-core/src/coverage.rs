use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::glyph::{Disposition, Glyph};

/// Conservation report. A document is complete only when every extracted glyph
/// has exactly one final disposition: rewritten, kept-original, or non-text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageReport {
    pub total: usize,
    pub pending: usize,
    pub translated_pending_rewrite: usize,
    pub rewritten: usize,
    pub kept_original: usize,
    pub non_text: usize,
    /// Glyphs left in English because a body paragraph was not painted.
    /// A report with any of these is not complete.
    #[serde(default)]
    pub english_body: usize,
    pub unresolved_ids: Vec<u32>,
    pub complete: bool,
}

pub fn report(glyphs: &[Glyph]) -> CoverageReport {
    let mut pending = 0;
    let mut translated = 0;
    let mut rewritten = 0;
    let mut kept = 0;
    let mut non_text = 0;
    let mut english_body = 0;
    let mut unresolved_ids = Vec::new();
    for g in glyphs {
        match &g.disposition {
            Disposition::Pending => {
                pending += 1;
                unresolved_ids.push(g.id);
            }
            Disposition::TranslatedPendingRewrite { .. } => {
                translated += 1;
                unresolved_ids.push(g.id);
            }
            Disposition::Rewritten { .. } => rewritten += 1,
            Disposition::KeptOriginal { reason } => {
                kept += 1;
                if failed_body_reason(reason) && glyph_is_latin_letter(g) {
                    english_body += 1;
                    unresolved_ids.push(g.id);
                }
            }
            Disposition::NonText { .. } => non_text += 1,
        }
    }
    let complete = unresolved_ids.is_empty();
    CoverageReport {
        total: glyphs.len(),
        pending,
        translated_pending_rewrite: translated,
        rewritten,
        kept_original: kept,
        non_text,
        english_body,
        unresolved_ids,
        complete,
    }
}

fn failed_body_reason(reason: &str) -> bool {
    matches!(
        reason,
        "missing-glyph" | "overflow" | "untranslated" | "no-font" | "no-stream" | "english-body"
    )
}

fn glyph_is_latin_letter(glyph: &Glyph) -> bool {
    glyph.unicode.chars().any(|ch| ch.is_ascii_alphabetic())
}

pub fn assert_complete(glyphs: &[Glyph]) -> Result<()> {
    let rep = report(glyphs);
    if rep.complete {
        Ok(())
    } else {
        Err(Error::CoverageIncomplete {
            unresolved: rep.unresolved_ids.len(),
            english_body: rep.english_body,
            ids: rep.unresolved_ids,
        })
    }
}

pub fn mark(glyphs: &mut [Glyph], id: u32, disposition: Disposition) -> Result<()> {
    let glyph = glyphs
        .iter_mut()
        .find(|g| g.id == id)
        .ok_or(Error::MissingGlyph(id))?;
    if glyph.disposition.is_final() {
        return Err(Error::AlreadyFinal(id, glyph.disposition.name().into()));
    }
    glyph.disposition = disposition;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyph::{GlyphSource, SourceKind};

    fn glyph(id: u32) -> Glyph {
        Glyph {
            id,
            page_index: 0,
            unicode: "A".into(),
            unmapped: false,
            char_code: vec![b'A'],
            gid: None,
            font_resource: "F1".into(),
            font_name: "Helvetica".into(),
            font_object: None,
            font_size: 12.0,
            matrix: [12.0, 0.0, 0.0, 12.0, 0.0, 0.0],
            bbox: [0.0, 0.0, 7.0, 10.0],
            advance: [7.0, 0.0],
            fill_color: crate::color::Color::black(),
            stroke_color: crate::color::Color::black(),
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
                resource_name: None,
            },
        }
    }

    #[test]
    fn pending_is_not_final_and_marking_completes() {
        let mut glyphs = vec![glyph(0), glyph(1)];
        let rep = report(&glyphs);
        assert!(!rep.complete);
        assert_eq!(rep.unresolved_ids, vec![0, 1]);
        assert!(assert_complete(&glyphs).is_err());
        mark(&mut glyphs, 0, Disposition::Rewritten { text: "甲".into() }).unwrap();
        mark(
            &mut glyphs,
            1,
            Disposition::KeptOriginal {
                reason: "number".into(),
            },
        )
        .unwrap();
        assert!(report(&glyphs).complete);
        assert!(assert_complete(&glyphs).is_ok());
        let mut failed = vec![glyph(2)];
        failed[0].unicode = "Part".into();
        mark(
            &mut failed,
            2,
            Disposition::KeptOriginal {
                reason: "missing-glyph".into(),
            },
        )
        .unwrap();
        let failed_report = report(&failed);
        assert!(!failed_report.complete);
        assert_eq!(failed_report.english_body, 1);
        assert!(assert_complete(&failed).is_err());
        let err = mark(
            &mut glyphs,
            0,
            Disposition::NonText {
                reason: "again".into(),
            },
        );
        assert!(matches!(err, Err(Error::AlreadyFinal(0, _))));
    }
}
