use serde::{Deserialize, Serialize};

use crate::color::Color;

/// Where a text-showing operator lived. Later milestones delete or rewrite
/// that operator using `operator_index` and the byte range inside the stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    PageContent,
    FormXObject,
    AnnotationAppearance,
    Type3CharProc,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlyphSource {
    pub kind: SourceKind,
    /// Indirect object id `"number generation"`, when the stream is indirect.
    pub object_id: Option<String>,
    /// Index of this stream inside a page `/Contents` array.
    pub stream_index: u32,
    /// Index of the text-showing operator within that stream.
    pub operator_index: u32,
    pub byte_start: usize,
    pub byte_end: usize,
    /// Resource name (`/F1`, `/Fm1`, annotation index, Type3 glyph name).
    pub resource_name: Option<String>,
}

/// Final states are `rewritten`, `kept_original`, and `non_text`.
/// `pending` is the state extraction leaves every glyph in.
/// `translated_pending_rewrite` records a translation before PDF rewrite exists.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Disposition {
    #[default]
    Pending,
    TranslatedPendingRewrite {
        text: String,
    },
    Rewritten {
        text: String,
    },
    KeptOriginal {
        reason: String,
    },
    NonText {
        reason: String,
    },
}

impl Disposition {
    pub fn is_final(&self) -> bool {
        matches!(
            self,
            Self::Rewritten { .. } | Self::KeptOriginal { .. } | Self::NonText { .. }
        )
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::TranslatedPendingRewrite { .. } => "translated_pending_rewrite",
            Self::Rewritten { .. } => "rewritten",
            Self::KeptOriginal { .. } => "kept_original",
            Self::NonText { .. } => "non_text",
        }
    }
}

/// One extracted glyph. Nothing in this record is optional in the sense of
/// "the glyph was skipped": unmapped, invisible, and clipped glyphs are still
/// present and flagged.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Glyph {
    pub id: u32,
    pub page_index: u32,
    /// Unicode mapping. Empty when `unmapped` is set. May contain several scalars
    /// (ligatures, ToUnicode multi-char mappings).
    pub unicode: String,
    pub unmapped: bool,
    /// Raw character-code bytes for this glyph.
    pub char_code: Vec<u8>,
    pub gid: Option<u32>,
    /// Font resource name as used by `Tf` (for example `F1`).
    pub font_resource: String,
    /// `/BaseFont` name, when the font dictionary has one.
    pub font_name: String,
    pub font_object: Option<String>,
    pub font_size: f32,
    /// Text rendering matrix `Trm` as PDF `[a b c d e f]`.
    /// Maps glyph em space (1 unit = 1 em, y up) into page user space.
    pub matrix: [f32; 6],
    /// Glyph bounding box in page user space: `[x0 y0 x1 y1]`, origin bottom-left.
    /// The box is an em approximation (descent 0.2 em, ascent 0.8 em) times the
    /// glyph width, not the font's exact ink bounds.
    pub bbox: [f32; 4],
    /// User-space advance of the text position after this glyph, `[dx dy]`.
    pub advance: [f32; 2],
    pub fill_color: Color,
    pub stroke_color: Color,
    /// PDF text rendering mode `Tr` (0–7).
    pub render_mode: u8,
    /// `Tr` 3 or 7, or a fully transparent fill on a fill-only mode.
    pub invisible: bool,
    /// The glyph bbox lies completely outside a tracked rectangular clip.
    pub clipped: bool,
    /// A non-rectangular clip is active, so `clipped` may be incomplete.
    pub clip_uncertain: bool,
    pub vertical: bool,
    pub disposition: Disposition,
    pub source: GlyphSource,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PageInfo {
    pub index: u32,
    pub object_id: String,
    /// MediaBox `[llx lly urx ury]` in the default user space.
    pub media_box: [f32; 4],
    /// Page `/Rotate` in degrees. Coordinates are **not** rotated by this value.
    pub rotate: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Diagnostic {
    pub page_index: Option<u32>,
    pub message: String,
}
