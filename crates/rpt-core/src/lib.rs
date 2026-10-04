//! RapidPdfTrans core.
//!
//! This crate is the milestone-1 foundation: a PDF content-stream interpreter
//! that records every glyph, and a coverage checker that refuses to treat the
//! document as finished while any glyph still lacks a final disposition.
//!
//! # Coordinates
//!
//! Glyph positions are in PDF user space. The origin is the bottom-left of the
//! page's default user space and y increases upward. Page `/Rotate` is reported
//! and is not baked into the matrices.
//!
//! # What is deliberately not here yet
//!
//! Rewriting content streams, shaping, font subsetting, and layout analysis
//! belong to later milestones. The glyph source location (stream, operator
//! index, byte range) is the hook those steps will use to delete the original
//! text-showing operator instead of dropping text.
//!
//! [`translate`] sends extracted text to a pluggable translator. The default
//! model name is `auto`. Translation marks glyphs
//! `translated_pending_rewrite` and does not rewrite PDF operators.

#![allow(clippy::too_many_arguments)]

pub mod coverage;
pub mod error;
pub mod extract;
pub mod glyph;
pub mod translate;

mod color;
mod content;
mod font;
mod geom;
mod pdfutil;
mod resources;

pub use color::Color;
pub use coverage::{report as coverage_report, CoverageReport};
pub use error::{Error, Result};
pub use extract::{ExtractOptions, Extraction, OpenOptions, PdfDocument, COORDINATE_SPACE};
pub use font::minimal_ttf;
pub use geom::{Matrix, Rect};
pub use glyph::{Diagnostic, Disposition, Glyph, GlyphSource, PageInfo, SourceKind};
pub use translate::{
    translate_extraction, LlmTranslator, TranslateOptions, TranslateReport, TranslatedSegment,
    Translator, DEFAULT_LLM_BASE_URL, DEFAULT_LLM_MODEL,
};

#[cfg(test)]
mod coverage_api {
    use super::*;

    #[test]
    fn garbage_bytes_are_an_error_not_a_panic() {
        assert!(PdfDocument::open_bytes(b"this is not a pdf").is_err());
        assert!(PdfDocument::open_bytes(b"%PDF-1.4\ntrailer <<>>\n%%EOF").is_err());
    }
}
