//! Open a PDF and extract every glyph.
//!
//! Coordinates are in PDF user space: origin at the bottom left of the page,
//! y increasing upward. Page `/Rotate` is reported on [`PageInfo`] and is not
//! applied to glyph positions. Extraction never fails because one operator is
//! malformed; those become diagnostics, and the glyphs that could be read are
//! still returned.

use std::path::Path;

use lopdf::{Document, LoadOptions};
use serde::{Deserialize, Serialize};

use crate::content::{interpret_document, InterpretOptions};
use crate::coverage::{self, CoverageReport};
use crate::error::{Error, Result};
use crate::glyph::{Diagnostic, Disposition, Glyph, PageInfo};

/// Page user space, origin bottom-left, y up. `/Rotate` is not applied.
pub const COORDINATE_SPACE: &str = "pdf-user-space-origin-bottom-left-y-up";

#[derive(Clone, Debug)]
pub struct OpenOptions {
    pub password: Option<String>,
    /// Reject non-conforming files instead of using lopdf's xref recovery.
    pub strict: bool,
    pub max_decompressed_size: usize,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            password: None,
            strict: false,
            max_decompressed_size: 64 * 1024 * 1024,
        }
    }
}

impl OpenOptions {
    pub fn from_json(text: &str) -> Result<Self> {
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| Error::Options(e.to_string()))?;
        let mut opts = Self::default();
        if let Some(password) = value.get("password").and_then(|v| v.as_str()) {
            opts.password = Some(password.to_string());
        }
        if let Some(strict) = value.get("strict").and_then(|v| v.as_bool()) {
            opts.strict = strict;
        }
        if let Some(limit) = value.get("max_decompressed_size").and_then(|v| v.as_u64()) {
            opts.max_decompressed_size = limit as usize;
        }
        Ok(opts)
    }
}

#[derive(Clone, Debug)]
pub struct ExtractOptions {
    pub max_depth: u32,
    pub max_stream_bytes: usize,
    /// Stop after this many pages. `None` reads the whole document.
    pub max_pages: Option<u32>,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            max_depth: 32,
            max_stream_bytes: 32 * 1024 * 1024,
            max_pages: None,
        }
    }
}

impl ExtractOptions {
    pub fn from_json(text: &str) -> Result<Self> {
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| Error::Options(e.to_string()))?;
        let mut opts = Self::default();
        if let Some(depth) = value.get("max_depth").and_then(|v| v.as_u64()) {
            opts.max_depth = depth as u32;
        }
        if let Some(limit) = value.get("max_stream_bytes").and_then(|v| v.as_u64()) {
            opts.max_stream_bytes = limit as usize;
        }
        if let Some(pages) = value.get("max_pages").and_then(|v| v.as_u64()) {
            opts.max_pages = Some(pages as u32);
        }
        Ok(opts)
    }
}

pub struct PdfDocument {
    inner: Document,
}

impl PdfDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(path, &OpenOptions::default())
    }

    pub fn open_with(path: impl AsRef<Path>, opts: &OpenOptions) -> Result<Self> {
        let doc = Document::load_with_options(path, load_options(opts))
            .map_err(|e| Error::Pdf(e.to_string()))?;
        Ok(Self { inner: doc })
    }

    pub fn open_bytes(data: &[u8]) -> Result<Self> {
        Self::open_bytes_with(data, &OpenOptions::default())
    }

    pub fn open_bytes_with(data: &[u8], opts: &OpenOptions) -> Result<Self> {
        let doc = Document::load_mem_with_options(data, load_options(opts))
            .map_err(|e| Error::Pdf(e.to_string()))?;
        Ok(Self { inner: doc })
    }

    pub fn page_count(&self) -> usize {
        self.inner.get_pages().len()
    }

    pub fn extract(&self) -> Extraction {
        self.extract_with(&ExtractOptions::default())
    }

    /// Decompressed bytes of an indirect stream named `"number generation"`.
    pub fn rewrite(
        &mut self,
        extraction: &mut Extraction,
        report: &crate::translate::TranslateReport,
        opts: &crate::rewrite::RewriteOptions,
    ) -> Result<()> {
        crate::rewrite::rewrite_translation(&mut self.inner, extraction, report, opts)
    }

    pub fn save_bytes(&mut self) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.inner
            .save_to(&mut bytes)
            .map_err(|err| Error::Pdf(err.to_string()))?;
        Ok(bytes)
    }

    pub fn save_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
        self.inner
            .save(path)
            .map(|_| ())
            .map_err(|err| Error::Pdf(err.to_string()))
    }

    pub fn plain_stream(&self, object_id: &str) -> Option<Vec<u8>> {
        let mut parts = object_id.split_whitespace();
        let number: u32 = parts.next()?.parse().ok()?;
        let generation: u16 = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        let obj = self.inner.get_object((number, generation)).ok()?;
        let (_, bytes, _) = crate::pdfutil::stream_bytes(&self.inner, obj, 32 * 1024 * 1024)?;
        Some(bytes)
    }

    pub fn extract_with(&self, opts: &ExtractOptions) -> Extraction {
        let interpreted = interpret_document(
            &self.inner,
            &InterpretOptions {
                max_depth: opts.max_depth,
                max_stream_bytes: opts.max_stream_bytes,
                max_pages: opts.max_pages,
            },
        );
        Extraction {
            coordinate_space: COORDINATE_SPACE,
            pages: interpreted.pages,
            glyphs: interpreted.glyphs,
            diagnostics: interpreted.diagnostics,
        }
    }
}

fn load_options(opts: &OpenOptions) -> LoadOptions {
    LoadOptions {
        password: opts.password.clone(),
        strict: opts.strict,
        max_decompressed_size: Some(opts.max_decompressed_size),
        filter: None,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Extraction {
    pub coordinate_space: &'static str,
    pub pages: Vec<PageInfo>,
    pub glyphs: Vec<Glyph>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Extraction {
    pub fn coverage_report(&self) -> CoverageReport {
        coverage::report(&self.glyphs)
    }

    pub fn assert_complete(&self) -> Result<()> {
        coverage::assert_complete(&self.glyphs)
    }

    pub fn mark_rewritten(&mut self, id: u32, text: impl Into<String>) -> Result<()> {
        coverage::mark(
            &mut self.glyphs,
            id,
            Disposition::Rewritten { text: text.into() },
        )
    }

    pub fn mark_kept(&mut self, id: u32, reason: impl Into<String>) -> Result<()> {
        coverage::mark(
            &mut self.glyphs,
            id,
            Disposition::KeptOriginal {
                reason: reason.into(),
            },
        )
    }

    pub fn mark_non_text(&mut self, id: u32, reason: impl Into<String>) -> Result<()> {
        coverage::mark(
            &mut self.glyphs,
            id,
            Disposition::NonText {
                reason: reason.into(),
            },
        )
    }

    pub fn mark_translated_pending(&mut self, id: u32, text: impl Into<String>) -> Result<()> {
        coverage::mark(
            &mut self.glyphs,
            id,
            Disposition::TranslatedPendingRewrite { text: text.into() },
        )
    }

    /// Reading-order plain text. Spaces that were not in the PDF are not inserted.
    /// A newline is inserted when the baseline changes.
    pub fn plain_text(&self) -> String {
        let mut lines: Vec<Vec<&Glyph>> = Vec::new();
        let mut current: Vec<&Glyph> = Vec::new();
        let mut last_page: Option<u32> = None;
        let mut last_y: Option<f32> = None;
        let mut last_size = 12.0f32;
        let mut ordered: Vec<&Glyph> = self.glyphs.iter().collect();
        ordered.sort_by(|a, b| {
            a.page_index
                .cmp(&b.page_index)
                .then(
                    b.matrix[5]
                        .partial_cmp(&a.matrix[5])
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
                .then(
                    a.matrix[4]
                        .partial_cmp(&b.matrix[4])
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        });
        for glyph in ordered {
            let new_line = match (last_page, last_y) {
                (Some(page), Some(y)) => {
                    glyph.page_index != page || (glyph.matrix[5] - y).abs() > last_size * 0.5
                }
                _ => false,
            };
            if new_line && !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            last_page = Some(glyph.page_index);
            last_y = Some(glyph.matrix[5]);
            last_size = glyph.font_size.max(1.0);
            current.push(glyph);
        }
        if !current.is_empty() {
            lines.push(current);
        }
        lines
            .iter()
            .map(|line| line.iter().map(|g| g.unicode.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn to_json_value(&self) -> Result<serde_json::Value> {
        let mut value = serde_json::to_value(self).map_err(|e| Error::Message(e.to_string()))?;
        if let Some(obj) = value.as_object_mut() {
            obj.insert(
                "coverage".into(),
                serde_json::to_value(self.coverage_report())
                    .map_err(|e| Error::Message(e.to_string()))?,
            );
            obj.insert(
                "plain_text".into(),
                serde_json::Value::String(self.plain_text()),
            );
        }
        Ok(value)
    }

    pub fn to_json_pretty(&self) -> Result<String> {
        serde_json::to_string_pretty(&self.to_json_value()?)
            .map_err(|e| Error::Message(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    #[test]
    fn text_matrix_scale_is_the_font_size() {
        let mut doc = lopdf::Document::with_version("1.4");
        doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        let pages_id = doc.new_object_id();
        let font = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        });
        let mut fonts = lopdf::Dictionary::new();
        fonts.set("F1", font);
        let mut resources = lopdf::Dictionary::new();
        resources.set("Font", fonts);
        let ops = b"BT\n/F1 1 Tf\n9.96 0 0 9.96 72 700 Tm\n(Hi) Tj\n/F1 10 Tf\n1 0 0 1 72 680 Tm\n(A) Tj\nET\n";
        let content_id = doc.add_object(lopdf::Stream::new(dictionary! {}, ops.to_vec()));
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
        let pdf = PdfDocument::open_bytes(&bytes).unwrap();
        let extraction = pdf.extract();
        let hi: Vec<_> = extraction
            .glyphs
            .iter()
            .filter(|glyph| glyph.matrix[5] > 690.0)
            .collect();
        assert!(hi.len() >= 2, "glyphs {}", extraction.glyphs.len());
        for glyph in &hi {
            assert!(
                (glyph.font_size - 9.96).abs() < 0.05,
                "scaled Tf 1 should be 9.96pt, got {}",
                glyph.font_size
            );
        }
        let letter = extraction
            .glyphs
            .iter()
            .find(|glyph| glyph.unicode == "A")
            .expect("A");
        assert!((letter.font_size - 10.0).abs() < 0.05);
    }
}
