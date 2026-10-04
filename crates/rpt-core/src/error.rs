use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to open PDF: {0}")]
    Pdf(String),
    #[error("invalid options: {0}")]
    Options(String),
    #[error("glyph {0} not found")]
    MissingGlyph(u32),
    #[error("glyph {0} already has a final disposition ({1})")]
    AlreadyFinal(u32, String),
    #[error("coverage incomplete: {unresolved} glyph(s) have no final state")]
    CoverageIncomplete { unresolved: usize, ids: Vec<u32> },
    #[error("translation failed: {0}")]
    Translate(String),
    #[error("{0}")]
    Message(String),
}
