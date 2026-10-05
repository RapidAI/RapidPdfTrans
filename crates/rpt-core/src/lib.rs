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
//! [`rewrite`] deletes original text-showing operators by replacing them with
//! the same number of spaces, then draws the translation with a subset CID
//! font. Advances come from `hmtx` (not OpenType GSUB). Reference-section
//! operators are left byte-identical.
//!
//! [`translate`] sends extracted text to a pluggable translator. The default
//! model name is `auto`.

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
mod rewrite;

pub use color::Color;
pub use coverage::{report as coverage_report, CoverageReport};
pub use error::{Error, Result};
pub use extract::{ExtractOptions, Extraction, OpenOptions, PdfDocument, COORDINATE_SPACE};
pub use font::minimal_ttf;
pub use geom::{Matrix, Rect};
pub use glyph::{Diagnostic, Disposition, Glyph, GlyphSource, PageInfo, SourceKind};
pub use rewrite::{rewrite_translation, RewriteOptions};
pub use translate::{
    citation_end, identical_reference_operators, reference_glyph_ids, translate_extraction,
    BilingualLayout, LlmTranslator, OutputMode, TranslateOptions, TranslateReport,
    TranslatedSegment, Translator, TranslatorBackend, DEFAULT_LLM_BASE_URL, DEFAULT_LLM_MODEL,
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
