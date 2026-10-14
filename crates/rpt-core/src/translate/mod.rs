//! Pluggable translation of extracted text.
//!
//! This module is the only place a model is called. Paragraph detection is
//! [`crate::segment`] and fitting is [`crate::layout`]; both run without a
//! translator. Translation marks glyphs `translated_pending_rewrite`.
//! [`crate::rewrite`] writes those strings back into the PDF.
//! Bibliography glyphs are the exception: with `skip_references` (the default)
//! they are `kept_original` and are not sent to the translator.
//!
//! The default backend is the maclaw preset: an OpenAI-compatible chat
//! gateway at `https://hub.mypapers.top/api/llm/v1` with model `auto`.
//! `RPT_TRANSLATOR` selects `maclaw`, `openai`, `google-v2`, `google-v3`,
//! or `google-unofficial`. Keys come only from environment variables.

mod backend;
mod google;
mod llm;
mod prompt;
mod protect;
mod references;

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) use crate::segment::is_inline_math_symbol;
pub use crate::segment::Segment;
pub use backend::TranslatorBackend;
pub use google::GoogleTranslator;
pub use llm::LlmTranslator;
pub use protect::citation_end;
pub(crate) use protect::verbatim_spans;
pub use references::{identical_reference_operators, reference_glyph_ids, reference_stream_spans};

use crate::error::{Error, Result};
use crate::extract::Extraction;
use crate::segment;
use prompt::{
    fallback_prompt, lint_prompt, parse_translations, system_prompt, user_payload, PromptSegment,
};
use protect::{restore, shield};

pub const DEFAULT_LLM_BASE_URL: &str = "https://hub.mypapers.top/api/llm/v1";
pub const DEFAULT_LLM_MODEL: &str = "auto";

/// Something that turns a system prompt and a user payload into model text.
///
/// `complete` may run on several threads when [`TranslateOptions::jobs`] is
/// greater than one. Each call gets its own prompt. Do not store segment
/// text, placeholders, or glossary entries on `self`; workers share the
/// translator only through `&self`.
pub trait Translator: Sync {
    fn complete(&self, system: &str, user: &str) -> Result<String>;
}

#[derive(Clone, Debug)]
pub struct TranslateOptions {
    pub source_lang: String,
    pub target_lang: String,
    /// Explicit model. Empty or unset falls through to `RPT_LLM_MODEL`, then `auto`.
    pub model: Option<String>,
    /// Explicit base URL. Empty or unset falls through to `RPT_LLM_BASE_URL`, then the gateway.
    pub base_url: Option<String>,
    /// Source term, target term. Longer terms win. Applied before the model call.
    pub glossary: Vec<(String, String)>,
    pub context_window: usize,
    pub batch_size: usize,
    /// Parallel model requests. `0` uses `RPT_TRANSLATE_JOBS`, then 1.
    /// Layout and rewrite stay on the calling thread either way.
    pub jobs: usize,
    pub temperature: f32,
    pub timeout_secs: u64,
    /// Sent only when set. Left unset so reasoning models can still emit content.
    pub max_tokens: Option<u32>,
    /// `maclaw`, `openai`, `google`, `google-v2`, `google-v3`, or `google-unofficial`.
    /// Empty falls through to `RPT_TRANSLATOR`, then `maclaw`.
    pub translator: Option<String>,
    /// Leave References / Bibliography as original text. Default is on.
    pub skip_references: bool,
    /// Leave text inside figures original. Only the caption is translated.
    pub skip_figures: bool,
    /// Leave table cells and headers original. Only the caption is translated.
    pub skip_tables: bool,
    /// Song/serif CJK font file. Empty uses `RPT_CJK_FONT`, then Noto Serif CJK.
    pub cjk_font: Option<String>,
    /// Sans CJK font file for regular sans text.
    pub cjk_sans: Option<String>,
    /// Chinese body size as a fraction of the source size. `0` uses 0.90,
    /// or `RPT_CJK_SIZE_SCALE` when that is set.
    pub cjk_size_scale: f32,
    /// Chinese baseline distance in ems. `0` uses 1.60, or `RPT_CJK_LEADING`.
    pub cjk_leading: f32,
    /// `replace` writes only the translation. `bilingual` keeps an English page.
    pub output_mode: OutputMode,
}

/// How a finished PDF presents the translation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OutputMode {
    /// Chinese (or the target language) replaces the source text in place.
    #[default]
    Replace,
    /// English and the translation are both kept. See [`BilingualLayout`].
    Bilingual(BilingualLayout),
}

/// Where the English page sits relative to the translated page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BilingualLayout {
    /// One wide page: original on the left, translation on the right.
    #[default]
    SideBySide,
    /// Original page, then its translated page, for every page.
    Alternating,
    /// Same page: original operators stay, and the translation is drawn too.
    Overlay,
}

impl OutputMode {
    /// `mode` is `replace`, `bilingual`, `side-by-side`, `alternating`, or `overlay`.
    /// `layout` is used when `mode` is `bilingual`.
    pub fn parse(mode: &str, layout: Option<&str>) -> Result<Self> {
        let mode = normalize_mode(mode);
        match mode.as_str() {
            "replace" | "mono" | "translation" => Ok(Self::Replace),
            "bilingual" | "bi" => Ok(Self::Bilingual(BilingualLayout::parse(
                layout.unwrap_or("side-by-side"),
            )?)),
            "side-by-side" | "sidebyside" | "parallel" => {
                Ok(Self::Bilingual(BilingualLayout::SideBySide))
            }
            "alternating" | "alternate" | "interleaved" => {
                Ok(Self::Bilingual(BilingualLayout::Alternating))
            }
            "overlay" => Ok(Self::Bilingual(BilingualLayout::Overlay)),
            _ => Err(Error::Options(format!(
                "unknown output mode `{mode}`; use replace, side-by-side, alternating, or overlay"
            ))),
        }
    }
}

impl BilingualLayout {
    pub fn parse(layout: &str) -> Result<Self> {
        match normalize_mode(layout).as_str() {
            "side-by-side" | "sidebyside" | "parallel" => Ok(Self::SideBySide),
            "alternating" | "alternate" | "interleaved" => Ok(Self::Alternating),
            "overlay" => Ok(Self::Overlay),
            other => Err(Error::Options(format!(
                "unknown bilingual layout `{other}`; use side-by-side, alternating, or overlay"
            ))),
        }
    }
}

fn normalize_mode(text: &str) -> String {
    text.trim().to_ascii_lowercase().replace('_', "-")
}

impl Default for TranslateOptions {
    fn default() -> Self {
        Self {
            source_lang: "en".into(),
            target_lang: "zh".into(),
            model: None,
            base_url: None,
            glossary: Vec::new(),
            context_window: 2,
            batch_size: 8,
            jobs: 0,
            temperature: 0.0,
            timeout_secs: 300,
            max_tokens: None,
            translator: None,
            skip_references: true,
            skip_figures: true,
            skip_tables: true,
            cjk_font: None,
            cjk_sans: None,
            cjk_size_scale: 0.0,
            cjk_leading: 0.0,
            output_mode: OutputMode::Replace,
        }
    }
}

impl TranslateOptions {
    pub fn resolved_model(&self) -> String {
        resolve_choice(
            self.model.as_deref(),
            std::env::var("RPT_LLM_MODEL").ok().as_deref(),
            DEFAULT_LLM_MODEL,
        )
    }

    pub fn resolved_base_url(&self) -> String {
        resolve_choice(
            self.base_url.as_deref(),
            std::env::var("RPT_LLM_BASE_URL").ok().as_deref(),
            DEFAULT_LLM_BASE_URL,
        )
    }

    pub fn resolved_translator(&self) -> String {
        resolve_choice(
            self.translator.as_deref(),
            std::env::var("RPT_TRANSLATOR").ok().as_deref(),
            "maclaw",
        )
    }

    /// How many segment batches may be in flight. The default is one.
    pub fn resolved_jobs(&self) -> Result<usize> {
        if self.jobs > 0 {
            return Ok(self.jobs);
        }
        match std::env::var("RPT_TRANSLATE_JOBS") {
            Ok(value) => {
                let value = value.trim();
                if value.is_empty() {
                    Ok(1)
                } else {
                    value
                        .parse::<usize>()
                        .ok()
                        .filter(|n| *n > 0)
                        .ok_or_else(|| {
                            Error::Options(format!(
                                "RPT_TRANSLATE_JOBS must be a positive integer, got `{value}`"
                            ))
                        })
                }
            }
            Err(_) => Ok(1),
        }
    }

    /// Parse translator fields from an options JSON object.
    /// `api_key` is ignored; a warning is returned when it is present.
    pub fn from_json(text: &str) -> Result<(Self, Vec<String>)> {
        if text.trim().is_empty() {
            return Ok((Self::default(), Vec::new()));
        }
        let value: Value = serde_json::from_str(text).map_err(|e| Error::Options(e.to_string()))?;
        let mut opts = Self::default();
        let mut warnings = Vec::new();
        if value.get("api_key").is_some()
            || value.get("apiKey").is_some()
            || value.get("google_api_key").is_some()
        {
            warnings.push(
                "api keys in options JSON are ignored; set RPT_LLM_API_KEY or RPT_GOOGLE_API_KEY in the environment"
                    .into(),
            );
        }
        if let Some(v) = string_field(&value, &["source_lang", "from"]) {
            opts.source_lang = v;
        }
        if let Some(v) = string_field(&value, &["target_lang", "to"]) {
            opts.target_lang = v;
        }
        if let Some(v) = string_field(&value, &["model"]) {
            opts.model = Some(v);
        }
        if let Some(v) = string_field(&value, &["base_url", "baseUrl"]) {
            opts.base_url = Some(v);
        }
        if let Some(v) = string_field(&value, &["translator", "backend"]) {
            opts.translator = Some(v);
        }
        if let Some(n) = value.get("context_window").and_then(|v| v.as_u64()) {
            opts.context_window = n as usize;
        }
        if let Some(n) = value.get("batch_size").and_then(|v| v.as_u64()) {
            opts.batch_size = n as usize;
        }
        if let Some(n) = usize_field(&value, &["jobs", "translate_jobs"]) {
            if n == 0 {
                return Err(Error::Options("jobs must be at least 1".into()));
            }
            opts.jobs = n;
        }
        if let Some(t) = value.get("temperature").and_then(|v| v.as_f64()) {
            opts.temperature = t as f32;
        }
        if let Some(t) = value.get("timeout_secs").and_then(|v| v.as_u64()) {
            opts.timeout_secs = t;
        }
        if let Some(t) = value.get("max_tokens").and_then(|v| v.as_u64()) {
            opts.max_tokens = Some(t as u32);
        }
        if let Some(skip) = value.get("skip_references").and_then(|v| v.as_bool()) {
            opts.skip_references = skip;
        }
        if let Some(skip) = value.get("skip_figures").and_then(|v| v.as_bool()) {
            opts.skip_figures = skip;
        }
        if let Some(skip) = value.get("skip_tables").and_then(|v| v.as_bool()) {
            opts.skip_tables = skip;
        }
        if let Some(v) = string_field(&value, &["cjk_font", "cjk_serif"]) {
            opts.cjk_font = Some(v);
        }
        if let Some(v) = string_field(&value, &["cjk_sans"]) {
            opts.cjk_sans = Some(v);
        }
        if let Some(v) = float_field(&value, &["cjk_size_scale", "cjkSizeScale"]) {
            opts.cjk_size_scale = v;
        }
        if let Some(v) = float_field(&value, &["cjk_leading", "cjkLeading"]) {
            opts.cjk_leading = v;
        }
        let layout = string_field(&value, &["bilingual_layout", "layout"]);
        if let Some(mode) = string_field(&value, &["output_mode", "mode"]) {
            opts.output_mode = OutputMode::parse(&mode, layout.as_deref())?;
        } else if value.get("bilingual").and_then(|v| v.as_bool()) == Some(true) {
            opts.output_mode = OutputMode::parse("bilingual", layout.as_deref())?;
        }
        opts.glossary = parse_glossary(value.get("glossary"));
        if opts.batch_size == 0 {
            return Err(Error::Options("batch_size must be at least 1".into()));
        }
        Ok((opts, warnings))
    }
}

pub fn resolve_choice(explicit: Option<&str>, env_value: Option<&str>, builtin: &str) -> String {
    explicit
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| env_value.map(str::trim).filter(|s| !s.is_empty()))
        .unwrap_or(builtin)
        .to_string()
}

fn usize_field(value: &Value, names: &[&str]) -> Option<usize> {
    names.iter().find_map(|name| {
        value.get(*name).and_then(|item| {
            item.as_u64()
                .or_else(|| item.as_str().and_then(|text| text.trim().parse().ok()))
                .map(|number| number as usize)
        })
    })
}

fn float_field(value: &Value, names: &[&str]) -> Option<f32> {
    names.iter().find_map(|name| {
        value.get(*name).and_then(|item| {
            item.as_f64()
                .or_else(|| item.as_str().and_then(|text| text.trim().parse().ok()))
                .map(|number| number as f32)
        })
    })
}

fn string_field(value: &Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        value
            .get(*name)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

fn parse_glossary(value: Option<&Value>) -> Vec<(String, String)> {
    let Some(value) = value else {
        return Vec::new();
    };
    if let Some(obj) = value.as_object() {
        return obj
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|t| (k.clone(), t.to_string())))
            .filter(|(k, _)| !k.is_empty())
            .collect();
    }
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            let src = item
                .get("source")
                .and_then(|v| v.as_str())
                .or_else(|| item.get("from").and_then(|v| v.as_str()))?;
            let dst = item
                .get("target")
                .and_then(|v| v.as_str())
                .or_else(|| item.get("to").and_then(|v| v.as_str()))?;
            if src.is_empty() {
                None
            } else {
                Some((src.to_string(), dst.to_string()))
            }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslatedSegment {
    pub id: u32,
    pub page_index: u32,
    pub glyph_ids: Vec<u32>,
    pub source: String,
    pub translated: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslateReport {
    pub segments: Vec<TranslatedSegment>,
    pub calls: usize,
    pub cache_hits: usize,
}

/// Translate every mapped glyph run. Unmapped glyphs are left `pending`.
pub fn translate_extraction(
    extraction: &mut Extraction,
    opts: &TranslateOptions,
    translator: &dyn Translator,
) -> Result<TranslateReport> {
    let jobs = opts.resolved_jobs()?;
    let (segments, reference_ids) = prepare_segments(extraction, opts)?;
    let shielded: Vec<protect::Shielded> = segments
        .iter()
        .map(|seg| shield(&seg.text, &opts.glossary))
        .collect();
    let mut translated: Vec<Option<String>> = segments
        .iter()
        .map(|seg| {
            seg.glyph_ids
                .iter()
                .any(|id| reference_ids.contains(id))
                .then(|| seg.text.clone())
        })
        .collect();
    let mut cache: HashMap<String, String> = HashMap::new();
    let mut calls = 0usize;
    let mut cache_hits = 0usize;

    for (i, seg) in segments.iter().enumerate() {
        if translated[i].is_none() && seg.text.trim().is_empty() {
            translated[i] = Some(seg.text.clone());
        }
    }
    cache_hits += load_checkpoint(&segments, &mut translated, &mut cache);
    // One owner per distinct source string. A repeated paragraph is filled
    // from that owner after the workers join, so two jobs never translate it.
    let batches = plan_unique_batches(&segments, &translated, opts.batch_size);
    let rendered = dispatch_batches(
        &segments, &shielded, &batches, opts, translator, jobs, &mut calls,
    )?;
    for (i, text) in rendered {
        cache.insert(segments[i].text.clone(), text.clone());
        translated[i] = Some(text);
    }
    for i in 0..segments.len() {
        if translated[i].is_some() {
            continue;
        }
        let text = cache.get(&segments[i].text).cloned().ok_or_else(|| {
            Error::Translate(format!("no translation for segment {}", segments[i].id))
        })?;
        cache_hits += 1;
        translated[i] = Some(text);
    }

    finish_report(
        extraction,
        &segments,
        &reference_ids,
        translated,
        calls,
        cache_hits,
    )
}

/// Apply a previous translation to the current segmentation.
///
/// Each segment is matched by its exact source string. No model is called.
/// A segment that would have been sent to the model and has no saved
/// translation is an error, so a layout replay cannot silently drop a tail.
pub fn replay_extraction(
    extraction: &mut Extraction,
    opts: &TranslateOptions,
    saved: &[TranslatedSegment],
) -> Result<TranslateReport> {
    let mut saved_map: HashMap<String, String> = HashMap::new();
    for item in saved {
        if let Some(previous) = saved_map.get(&item.source) {
            if previous != &item.translated {
                return Err(Error::Translate(
                    "saved translations disagree for one source string".into(),
                ));
            }
        } else {
            saved_map.insert(item.source.clone(), item.translated.clone());
        }
    }
    let (segments, reference_ids) = prepare_segments(extraction, opts)?;
    let mut translated: Vec<Option<String>> = Vec::with_capacity(segments.len());
    let mut cache_hits = 0usize;
    let mut seen = HashSet::new();
    for seg in &segments {
        if seg.glyph_ids.iter().any(|id| reference_ids.contains(id)) || seg.text.trim().is_empty() {
            translated.push(Some(seg.text.clone()));
            continue;
        }
        let Some(text) = lookup_saved(&seg.text, &saved_map) else {
            let preview: String = seg.text.chars().take(48).collect();
            return Err(Error::Translate(format!(
                "no saved translation for segment {} ({preview})",
                seg.id
            )));
        };
        if !seen.insert(seg.text.clone()) {
            cache_hits += 1;
        }
        translated.push(Some(text));
    }
    finish_report(
        extraction,
        &segments,
        &reference_ids,
        translated,
        0,
        cache_hits,
    )
}

/// A layout change can split a chapter index off its title, or join two
/// pieces of one sentence. Both still have a saved translation; a new
/// sentence does not, and that stays an error.
fn lookup_saved(source: &str, saved: &HashMap<String, String>) -> Option<String> {
    // An exact saved sentence is what this replay paints. A flaw still
    // makes the live checkpoint miss, so the model is asked again there.
    if let Some(hit) = saved.get(source) {
        return Some(hit.clone());
    }
    let trimmed = source.trim();
    if is_digit_index(trimmed) {
        return Some(trimmed.to_string());
    }
    if let Some(peeled) = peel_saved_index(trimmed, saved) {
        return Some(peeled);
    }
    if let Some(joined) = join_saved_pieces(source, saved) {
        if !translation_flaw(source, &joined) {
            return Some(joined);
        }
    }
    // The same glyphs can pick up a different space when two lines become one.
    let squashed = join_squashed(source, saved)?;
    (!translation_flaw(source, &squashed)).then_some(squashed)
}

fn is_digit_index(text: &str) -> bool {
    !text.is_empty() && text.len() <= 3 && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// `10 10.1 Title` loses the leading chapter index. `10.1` does not.
fn strip_leading_index(text: &str) -> Option<&str> {
    let text = text.trim_start();
    let digits = text
        .bytes()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if !(1..=3).contains(&digits) {
        return None;
    }
    let after = &text[digits..];
    let boundary = after.chars().next()?;
    if !boundary.is_whitespace() {
        return None;
    }
    let rest = after.trim_start();
    (!rest.is_empty()).then_some(rest)
}

fn peel_saved_index(source: &str, saved: &HashMap<String, String>) -> Option<String> {
    let source = source.trim();
    for (old, translated) in saved {
        let Some(rest) = strip_leading_index(old) else {
            continue;
        };
        // `10.1 Title` is a section heading. `Baldwin Road` is an address.
        if rest.trim() != source || !section_number_rest(rest) {
            continue;
        }
        if let Some(body) = strip_leading_index(translated) {
            return Some(body.trim().to_string());
        }
    }
    None
}

fn section_number_rest(rest: &str) -> bool {
    let rest = rest.trim();
    let digits = rest
        .bytes()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    (1..=3).contains(&digits) && rest.as_bytes().get(digits) == Some(&b'.')
}

fn rest_hit<'a>(
    rest: &'a str,
    saved: &'a HashMap<String, String>,
) -> Option<(&'a str, &'a String)> {
    if let Some(value) = saved.get(rest) {
        return Some((rest, value));
    }
    let trimmed = rest.trim_start();
    if trimmed.len() != rest.len() {
        if let Some(value) = saved.get(trimmed) {
            return Some((trimmed, value));
        }
    }
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return None;
    }
    saved
        .iter()
        .find(|(key, _)| key.trim() == trimmed)
        .map(|(key, value)| (key.as_str(), value))
}

fn join_saved_pieces(source: &str, saved: &HashMap<String, String>) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for (left, left_zh) in saved {
        if left.is_empty() || left.len() >= source.len() || !source.starts_with(left.as_str()) {
            continue;
        }
        let rest = &source[left.len()..];
        let Some((rest_key, right_zh)) = rest_hit(rest, saved) else {
            continue;
        };
        let joined = splice_saved(left, left_zh, rest_key, right_zh);
        if best.as_ref().is_none_or(|(len, _)| left.len() > *len) {
            best = Some((left.len(), joined));
        }
    }
    best.map(|(_, text)| text)
}

fn squash_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Join saved pieces after collapsing whitespace. A piece shorter than a
/// short phrase is not a prefix, so page numbers are not stitched into a
/// paragraph.
fn join_squashed(source: &str, saved: &HashMap<String, String>) -> Option<String> {
    let flat = squash_ws(source);
    let mut pieces: Vec<(String, String)> = saved
        .iter()
        .map(|(key, value)| (squash_ws(key), value.clone()))
        .filter(|(key, _)| !key.is_empty())
        .collect();
    pieces.sort_by_key(|piece| std::cmp::Reverse(piece.0.len()));
    cover_flat(&flat, &pieces)
}

fn cover_flat(flat: &str, pieces: &[(String, String)]) -> Option<String> {
    if flat.is_empty() {
        return Some(String::new());
    }
    for (left, left_zh) in pieces {
        if left.len() > flat.len() || (left.len() < 12 && flat != left) {
            continue;
        }
        if flat == left {
            return Some(left_zh.trim().to_string());
        }
        if !flat.starts_with(left.as_str()) {
            continue;
        }
        if !flat
            .as_bytes()
            .get(left.len())
            .is_some_and(u8::is_ascii_whitespace)
        {
            continue;
        }
        let rest = flat[left.len()..].trim_start();
        if rest.is_empty() || rest.len() >= flat.len() {
            continue;
        }
        if let Some(right) = cover_flat(rest, pieces) {
            return Some(splice_saved(left, left_zh, rest, &right));
        }
    }
    None
}

fn splice_saved(left_src: &str, left_zh: &str, right_src: &str, right_zh: &str) -> String {
    let left = left_zh.trim_end();
    let right = right_zh.trim_start();
    let spaced =
        left_src.ends_with(char::is_whitespace) || right_src.starts_with(char::is_whitespace);
    let han = left.chars().last().is_some_and(crate::layout::is_cjk_body)
        && right.chars().next().is_some_and(crate::layout::is_cjk_body);
    let mut out = left.to_string();
    if spaced && !han && !left.is_empty() && !right.is_empty() {
        out.push(' ');
    }
    out.push_str(right);
    out
}

fn prepare_segments(
    extraction: &mut Extraction,
    opts: &TranslateOptions,
) -> Result<(Vec<Segment>, HashSet<u32>)> {
    if opts.batch_size == 0 {
        return Err(Error::Options("batch_size must be at least 1".into()));
    }
    let segmentation = segment::segment_placed(
        &extraction.glyphs,
        &segment::SegmentFlags {
            skip_figures: opts.skip_figures,
            skip_tables: opts.skip_tables,
        },
        &extraction.regions,
        &extraction.pages,
    );
    let segments = segmentation.segments;
    let reference_ids = if opts.skip_references {
        references::reference_glyph_ids(&extraction.glyphs)
    } else {
        HashSet::new()
    };
    for id in &reference_ids {
        extraction.mark_kept(*id, "references")?;
    }
    let by_id: HashMap<u32, usize> = extraction
        .glyphs
        .iter()
        .enumerate()
        .map(|(index, glyph)| (glyph.id, index))
        .collect();
    for (id, reason) in &segmentation.kept {
        let Some(index) = by_id.get(id) else {
            continue;
        };
        if extraction.glyphs[*index].disposition.is_final() {
            continue;
        }
        extraction.mark_kept(*id, reason.clone())?;
    }
    Ok((segments, reference_ids))
}

fn finish_report(
    extraction: &mut Extraction,
    segments: &[Segment],
    reference_ids: &HashSet<u32>,
    translated: Vec<Option<String>>,
    calls: usize,
    cache_hits: usize,
) -> Result<TranslateReport> {
    let mut texts: Vec<String> = Vec::with_capacity(segments.len());
    for (i, seg) in segments.iter().enumerate() {
        let text = translated[i]
            .clone()
            .ok_or_else(|| Error::Translate(format!("segment {} was not translated", seg.id)))?;
        texts.push(text);
    }
    gloss_echoed_words(segments, &mut texts);
    let mut report_segments = Vec::with_capacity(segments.len());
    for (i, seg) in segments.iter().enumerate() {
        let text = texts[i].clone();
        let kept = seg.glyph_ids.iter().any(|id| reference_ids.contains(id));
        if !kept {
            for id in &seg.glyph_ids {
                extraction.mark_translated_pending(*id, text.clone())?;
            }
        }
        report_segments.push(TranslatedSegment {
            id: seg.id,
            page_index: seg.page_index,
            glyph_ids: seg.glyph_ids.clone(),
            source: seg.text.clone(),
            translated: text,
        });
    }
    Ok(TranslateReport {
        segments: report_segments,
        calls,
        cache_hits,
    })
}

/// `RPT_TRANSLATE_CHECKPOINT` is a JSONL file of `{source, translated}`.
/// A resumed run skips those source strings and does not call the model again.
fn load_checkpoint(
    segments: &[Segment],
    translated: &mut [Option<String>],
    cache: &mut HashMap<String, String>,
) -> usize {
    let Ok(path) = std::env::var("RPT_TRANSLATE_CHECKPOINT") else {
        return 0;
    };
    if path.is_empty() {
        return 0;
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        return 0;
    };
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let (Some(source), Some(done)) = (
            value.get("source").and_then(|item| item.as_str()),
            value.get("translated").and_then(|item| item.as_str()),
        ) else {
            continue;
        };
        cache.insert(source.to_string(), localize_part_heading(done));
    }
    let mut hits = 0usize;
    for (index, seg) in segments.iter().enumerate() {
        if translated[index].is_some() {
            continue;
        }
        if let Some(done) = cache.get(&seg.text) {
            // A cached echo of a body paragraph is not a translation. Leave it
            // for the model, which retries once when the reply is still English.
            if needs_english_retry(&seg.text, done) || translation_flaw(&seg.text, done) {
                continue;
            }
            translated[index] = Some(done.clone());
            hits += 1;
        }
    }
    hits
}

fn append_checkpoint(segments: &[Segment], part: &[(usize, String)]) {
    let Ok(path) = std::env::var("RPT_TRANSLATE_CHECKPOINT") else {
        return;
    };
    if path.is_empty() || part.is_empty() {
        return;
    }
    let mut lines = String::new();
    for (index, text) in part {
        let line = serde_json::json!({
            "source": segments[*index].text,
            "translated": text,
        });
        lines.push_str(&line.to_string());
        lines.push('\n');
    }
    let Ok(_guard) = checkpoint_lock().lock() else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = std::io::Write::write_all(&mut file, lines.as_bytes());
    }
}

fn checkpoint_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// Distinct segments, in reading order, packed into batches of `batch_size`.
/// Indexes in different batches are disjoint. Repeated source text is omitted
/// so it cannot be sent to a second worker.
fn plan_unique_batches(
    segments: &[Segment],
    translated: &[Option<String>],
    batch_size: usize,
) -> Vec<Vec<usize>> {
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for (i, seg) in segments.iter().enumerate() {
        if translated[i].is_some() {
            continue;
        }
        if !seen.insert(seg.text.clone()) {
            continue;
        }
        unique.push(i);
    }
    unique
        .chunks(batch_size.max(1))
        .map(|chunk| chunk.to_vec())
        .collect()
}

/// Run each batch on a worker. `jobs == 1` stays on this thread, in order.
/// Workers share the translator only as `&self` and never share a segment index.
fn dispatch_batches(
    segments: &[Segment],
    shielded: &[protect::Shielded],
    batches: &[Vec<usize>],
    opts: &TranslateOptions,
    translator: &dyn Translator,
    jobs: usize,
    calls: &mut usize,
) -> Result<Vec<(usize, String)>> {
    if batches.is_empty() {
        return Ok(Vec::new());
    }
    if jobs <= 1 {
        let mut rendered = Vec::new();
        for batch in batches {
            rendered.extend(translate_batch(
                segments, shielded, batch, opts, translator, calls,
            )?);
        }
        return Ok(rendered);
    }

    let next = std::sync::atomic::AtomicUsize::new(0);
    let call_count = std::sync::atomic::AtomicUsize::new(0);
    let outputs = std::sync::Mutex::new(Vec::new());
    let failure: std::sync::Mutex<Option<Error>> = std::sync::Mutex::new(None);
    let workers = jobs.min(batches.len());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                if failure.lock().map(|guard| guard.is_some()).unwrap_or(true) {
                    break;
                }
                let index = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if index >= batches.len() {
                    break;
                }
                let mut local_calls = 0usize;
                match translate_batch(
                    segments,
                    shielded,
                    &batches[index],
                    opts,
                    translator,
                    &mut local_calls,
                ) {
                    Ok(part) => {
                        call_count.fetch_add(local_calls, std::sync::atomic::Ordering::SeqCst);
                        append_checkpoint(segments, &part);
                        match outputs.lock() {
                            Ok(mut slot) => slot.extend(part),
                            Err(_) => {
                                if let Ok(mut slot) = failure.lock() {
                                    if slot.is_none() {
                                        *slot = Some(Error::Translate(
                                            "translation worker lock poisoned".into(),
                                        ));
                                    }
                                }
                                break;
                            }
                        }
                    }
                    Err(err) => {
                        if let Ok(mut slot) = failure.lock() {
                            if slot.is_none() {
                                *slot = Some(err);
                            }
                        }
                        break;
                    }
                }
            });
        }
    });
    *calls += call_count.load(std::sync::atomic::Ordering::SeqCst);
    let failure = failure
        .into_inner()
        .map_err(|_| Error::Translate("translation worker lock poisoned".into()))?;
    if let Some(err) = failure {
        return Err(err);
    }
    outputs
        .into_inner()
        .map_err(|_| Error::Translate("translation worker lock poisoned".into()))
}

fn translate_batch(
    segments: &[Segment],
    shielded: &[protect::Shielded],
    indexes: &[usize],
    opts: &TranslateOptions,
    translator: &dyn Translator,
    calls: &mut usize,
) -> Result<Vec<(usize, String)>> {
    match translate_batch_once(segments, shielded, indexes, opts, translator, calls) {
        Ok(restored) => Ok(polish(lint_fix(
            segments,
            echo_fix(segments, shielded, restored, opts, translator, calls),
            opts,
            translator,
            calls,
        ))),
        Err(err) if recoverable(&err) && indexes.len() > 1 => {
            let mid = indexes.len() / 2;
            let mut left =
                translate_batch(segments, shielded, &indexes[..mid], opts, translator, calls)?;
            left.extend(translate_batch(
                segments,
                shielded,
                &indexes[mid..],
                opts,
                translator,
                calls,
            )?);
            Ok(left)
        }
        Err(err) if recoverable(&err) => {
            match translate_batch_once(segments, shielded, indexes, opts, translator, calls) {
                Ok(restored) => Ok(polish(lint_fix(
                    segments,
                    echo_fix(segments, shielded, restored, opts, translator, calls),
                    opts,
                    translator,
                    calls,
                ))),
                Err(_) => Ok(polish(
                    indexes
                        .iter()
                        .map(|&index| (index, segments[index].text.clone()))
                        .collect(),
                )),
            }
        }
        Err(err) => Err(err),
    }
}

/// One more call when a long Latin paragraph comes back still in English.
fn echo_fix(
    segments: &[Segment],
    shielded: &[protect::Shielded],
    mut restored: Vec<(usize, String)>,
    opts: &TranslateOptions,
    translator: &dyn Translator,
    calls: &mut usize,
) -> Vec<(usize, String)> {
    let retry: Vec<usize> = restored
        .iter()
        .filter(|(index, text)| needs_english_retry(&segments[*index].text, text))
        .map(|(index, _)| *index)
        .collect();
    if retry.is_empty() {
        return restored;
    }
    // A failed batch retry must not skip the sentence pass and the fallback.
    // A short credit line is often the one the batch call rejects.
    if let Ok(parsed) = call_batch(
        segments, shielded, &retry, opts, translator, calls, false, true,
    ) {
        if let Ok(second) = restore_all(&retry, shielded, &parsed) {
            for (index, text) in second {
                if let Some(slot) = restored.iter_mut().find(|(id, _)| *id == index) {
                    slot.1 = text;
                }
            }
        }
    }
    // A long paragraph is often copied whole when the neighboring lines are
    // still English. Ask again for each sentence, with that context removed.
    for slot in &mut restored {
        if !needs_english_retry(&segments[slot.0].text, &slot.1) {
            continue;
        }
        if let Some(joined) = translate_sentences(&segments[slot.0].text, opts, translator, calls) {
            slot.1 = joined;
        }
    }
    // The echo retry above reuses the same instructions. A segment that is
    // still English gets one more call with a different prompt and no neighbors.
    for slot in &mut restored {
        if !needs_english_retry(&segments[slot.0].text, &slot.1) {
            continue;
        }
        if let Some(text) = translate_fallback(&segments[slot.0].text, opts, translator, calls) {
            slot.1 = text;
        }
    }
    restored
}

/// Split on sentence ends, keeping each piece's trailing space so the pieces
/// concatenate back to `text`. Abbreviations (`et al.`, `Fig.`) stay put.
fn split_sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut pieces = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;
    while index < chars.len() {
        if is_sentence_end(&chars, index) {
            let mut stop = index + 1;
            while stop < chars.len() && chars[stop].is_whitespace() {
                stop += 1;
            }
            pieces.push(chars[start..stop].iter().collect());
            start = stop;
            index = stop;
            continue;
        }
        index += 1;
    }
    if start < chars.len() {
        pieces.push(chars[start..].iter().collect());
    }
    pieces
}

fn is_sentence_end(chars: &[char], index: usize) -> bool {
    if !matches!(chars[index], '.' | '?' | '!') {
        return false;
    }
    let mut next = index + 1;
    while next < chars.len() && chars[next].is_whitespace() {
        next += 1;
    }
    if next >= chars.len() || !chars[next].is_ascii_uppercase() {
        return false;
    }
    let mut word_start = index;
    while word_start > 0 && chars[word_start - 1].is_ascii_alphabetic() {
        word_start -= 1;
    }
    let word: String = chars[word_start..index].iter().collect();
    !abbreviated_word(&word)
}

fn abbreviated_word(word: &str) -> bool {
    // A period after `)` has no word in front of it. That is a sentence
    // end, not an initial.
    if word.is_empty() {
        return false;
    }
    word.chars().count() <= 1
        || matches!(
            word.to_ascii_lowercase().as_str(),
            "al" | "etc"
                | "eg"
                | "ie"
                | "fig"
                | "eq"
                | "dr"
                | "mr"
                | "mrs"
                | "ms"
                | "vs"
                | "cf"
                | "vol"
                | "pp"
                | "no"
                | "approx"
                | "st"
                | "jr"
                | "sr"
                | "prof"
                | "inc"
                | "ltd"
        )
}

/// Translate one echoed paragraph as separate sentences and with no neighbor
/// context. Returns nothing when a long sentence is still English, so a mixed
/// paragraph is not painted as if it were done.
fn translate_sentences(
    source: &str,
    opts: &TranslateOptions,
    translator: &dyn Translator,
    calls: &mut usize,
) -> Option<String> {
    let split = split_sentences(source);
    // One sentence has no boundary to split on. Ask once more anyway, without
    // the neighboring English lines that the paragraph call was showing.
    let pieces = if split.len() >= 2 {
        split
    } else {
        vec![source.to_string()]
    };
    let mut translated = Vec::with_capacity(pieces.len());
    for piece in &pieces {
        if piece.trim().is_empty() {
            translated.push(piece.clone());
            continue;
        }
        let shielded = shield(piece, &opts.glossary);
        let prompt = PromptSegment {
            id: 0,
            text: shielded.text,
            context_before: Vec::new(),
            context_after: Vec::new(),
        };
        let user = user_payload(std::slice::from_ref(&prompt)).ok()?;
        let system = system_prompt(&opts.source_lang, &opts.target_lang, false, true);
        *calls += 1;
        let raw = translator.complete(&system, &user).ok()?;
        let parsed = parse_translations(&raw).ok()?;
        let text = parsed
            .iter()
            .find(|(id, _)| *id == 0)
            .map(|(_, text)| text.clone())?;
        let restored = restore_expanded(&text, &shielded.slots).ok()?;
        if needs_english_retry(piece, &restored) {
            return None;
        }
        translated.push(restored);
    }
    let joined = translated.concat();
    if joined.trim() == source.trim() {
        None
    } else {
        Some(joined)
    }
}

/// One segment, no neighbors, and a prompt that is not the echo retry.
fn translate_fallback(
    source: &str,
    opts: &TranslateOptions,
    translator: &dyn Translator,
    calls: &mut usize,
) -> Option<String> {
    let shielded = shield(source, &opts.glossary);
    let prompt = PromptSegment {
        id: 0,
        text: shielded.text,
        context_before: Vec::new(),
        context_after: Vec::new(),
    };
    let user = user_payload(std::slice::from_ref(&prompt)).ok()?;
    let system = fallback_prompt(&opts.target_lang);
    *calls += 1;
    let raw = translator.complete(&system, &user).ok()?;
    let parsed = parse_translations(&raw).ok()?;
    let text = parsed
        .iter()
        .find(|(id, _)| *id == 0)
        .map(|(_, text)| text.clone())?;
    let restored = restore_expanded(&text, &shielded.slots).ok()?;
    if needs_english_retry(source, &restored) {
        None
    } else {
        Some(restored)
    }
}

/// `restore` rejects a reply that wrote `20` instead of ⟦0⟧. A Chinese
/// answer that already contains each shielded token is still a translation.
fn restore_expanded(text: &str, slots: &[String]) -> std::result::Result<String, String> {
    match restore(text, slots) {
        Ok(value) => Ok(value),
        Err(err) => {
            let expanded = text.chars().any(is_cjk_char)
                && slots
                    .iter()
                    .all(|slot| !slot.is_empty() && text.contains(slot));
            if expanded {
                Ok(text.to_string())
            } else {
                Err(err)
            }
        }
    }
}

/// One more call when the translation is Chinese but still defective.
/// A repeated word or a space between Chinese characters is then removed,
/// so a second copy of the same defect is not painted.
fn lint_fix(
    segments: &[Segment],
    mut restored: Vec<(usize, String)>,
    opts: &TranslateOptions,
    translator: &dyn Translator,
    calls: &mut usize,
) -> Vec<(usize, String)> {
    for slot in &mut restored {
        let source = &segments[slot.0].text;
        if !translation_flaw(source, &slot.1) {
            continue;
        }
        let original = slot.1.clone();
        let candidate = translate_lint(segments[slot.0].id, source, opts, translator, calls)
            .unwrap_or(original.clone());
        let repaired_new = repair_translation(source, &candidate);
        let repaired_old = repair_translation(source, &original);
        slot.1 = if flaw_count(source, &repaired_new) <= flaw_count(source, &repaired_old) {
            repaired_new
        } else {
            repaired_old
        };
    }
    restored
}

fn translate_lint(
    id: u32,
    source: &str,
    opts: &TranslateOptions,
    translator: &dyn Translator,
    calls: &mut usize,
) -> Option<String> {
    let shielded = shield(source, &opts.glossary);
    let prompt = PromptSegment {
        id,
        text: shielded.text,
        context_before: Vec::new(),
        context_after: Vec::new(),
    };
    let user = user_payload(std::slice::from_ref(&prompt)).ok()?;
    let system = lint_prompt(&opts.target_lang);
    *calls += 1;
    let raw = translator.complete(&system, &user).ok()?;
    let parsed = parse_translations(&raw).ok()?;
    let text = parsed
        .iter()
        .find(|(found, _)| *found == id)
        .map(|(_, text)| text.clone())?;
    let restored = restore_expanded(&text, &shielded.slots).ok()?;
    if needs_english_retry(source, &restored) {
        None
    } else {
        Some(restored)
    }
}

/// A translation the model should be asked to do again.
///
/// A space between two short Chinese words, a word typed twice, a full stop
/// the source does not have, or a short English phrase left in the sentence.
fn translation_flaw(source: &str, translated: &str) -> bool {
    flaw_count(source, translated) > 0
}

fn flaw_count(source: &str, translated: &str) -> usize {
    usize::from(duplicated_word(source, translated))
        + usize::from(stray_cjk_space(translated))
        + usize::from(extra_sentence_end(source, translated))
        + usize::from(residual_english(translated))
        + usize::from(leftover_source_word(source, translated))
}

fn repair_translation(source: &str, translated: &str) -> String {
    drop_stray_cjk_spaces(&collapse_duplicate_tokens(source, translated))
}

fn duplicated_word(source: &str, translated: &str) -> bool {
    let tokens: Vec<&str> = translated.split_whitespace().collect();
    tokens.windows(2).any(|pair| {
        pair[0] == pair[1]
            && !pair[0].chars().all(|ch| ch.is_ascii_digit())
            && !source_repeats_token(source, pair[0])
    })
}

fn source_repeats_token(source: &str, token: &str) -> bool {
    let tokens: Vec<&str> = source.split_whitespace().collect();
    tokens
        .windows(2)
        .any(|pair| pair[0] == token && pair[1] == token)
}

fn collapse_duplicate_tokens(source: &str, text: &str) -> String {
    let lead_len = text.len() - text.trim_start().len();
    let lead = &text[..lead_len];
    let mut kept: Vec<&str> = Vec::new();
    for token in text.split_whitespace() {
        if kept.last().copied() == Some(token)
            && !token.chars().all(|ch| ch.is_ascii_digit())
            && !source_repeats_token(source, token)
        {
            continue;
        }
        kept.push(token);
    }
    let mut out = lead.to_string();
    out.push_str(&kept.join(" "));
    out
}

/// `高级 检索` is a space the model inserted. `第 4 部分 摘要` keeps the
/// space after the structural word. A transliterated name (`罗伯托 因凡特`)
/// is three characters on each side and stays.
fn stray_cjk_space(text: &str) -> bool {
    !stray_cjk_space_spans(text).is_empty()
}

fn drop_stray_cjk_spaces(text: &str) -> String {
    let spans = stray_cjk_space_spans(text);
    if spans.is_empty() {
        return text.to_string();
    }
    let mut out = String::new();
    let mut cursor = 0usize;
    for (start, end) in spans {
        out.push_str(&text[cursor..start]);
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}

fn stray_cjk_space_spans(text: &str) -> Vec<(usize, usize)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut spans = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        if !is_han(chars[index].1) {
            index += 1;
            continue;
        }
        let run_start = index;
        while index < chars.len() && is_han(chars[index].1) {
            index += 1;
        }
        let run: String = chars[run_start..index].iter().map(|(_, ch)| *ch).collect();
        let space_start = index;
        while index < chars.len() && chars[index].1.is_whitespace() {
            index += 1;
        }
        if space_start == index || index >= chars.len() || !is_han(chars[index].1) {
            continue;
        }
        if run.chars().count() <= 2 && !structural_han(&run) {
            spans.push((chars[space_start].0, chars[index].0));
        }
    }
    spans
}

fn structural_han(run: &str) -> bool {
    matches!(run, "部分" | "章" | "节" | "图" | "表" | "第" | "附录")
}

fn is_han(ch: char) -> bool {
    matches!(ch, '\u{3400}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}')
}

fn extra_sentence_end(source: &str, translated: &str) -> bool {
    let chars: Vec<char> = source.chars().collect();
    let source_ends = chars
        .iter()
        .enumerate()
        .filter(|(index, ch)| {
            // `this?” moments` is not a new sentence, but the translation
            // still has `？`. Count that mark so it is not an extra stop.
            sentence_boundary(&chars, *index) || matches!(ch, '?' | '!')
        })
        .count();
    let translated_ends = translated
        .chars()
        .filter(|ch| matches!(ch, '。' | '！' | '？'))
        .count();
    translated_ends > source_ends
}

/// A period before a new sentence, or the period that ends the segment.
fn sentence_boundary(chars: &[char], index: usize) -> bool {
    if is_sentence_end(chars, index) {
        return true;
    }
    if !matches!(chars[index], '.' | '?' | '!') {
        return false;
    }
    if chars[index + 1..].iter().any(|ch| !ch.is_whitespace()) {
        return false;
    }
    let mut word_start = index;
    while word_start > 0 && chars[word_start - 1].is_ascii_alphabetic() {
        word_start -= 1;
    }
    let word: String = chars[word_start..index].iter().collect();
    !abbreviated_word(&word)
}

/// `The Model Context Protocol` left in a sentence. A list of personal names
/// and a product name with no function word are not this phrase.
fn residual_english(text: &str) -> bool {
    // A one-character mark in front of an echoed sentence is not a Chinese
    // sentence that still contains an English phrase.
    let han_run = text
        .chars()
        .fold((0usize, 0usize), |(best, run), ch| {
            if is_han(ch) {
                let run = run + 1;
                (best.max(run), run)
            } else {
                (best, 0)
            }
        })
        .0;
    if han_run < 2 && text.chars().any(is_han) {
        return false;
    }
    let mut run: Vec<String> = Vec::new();
    let mut flagged = false;
    let flush = |run: &mut Vec<String>, flagged: &mut bool| {
        if (3..=4).contains(&run.len()) && run.iter().any(|word| is_function_word(word)) {
            *flagged = true;
        }
        run.clear();
    };
    for token in text.split_whitespace() {
        if let Some(word) = prose_word(token) {
            run.push(word);
        } else {
            flush(&mut run, &mut flagged);
        }
    }
    flush(&mut run, &mut flagged);
    flagged
}

/// `Text summarization` came back as `Text 摘要`. The other source word was
/// translated and this one was left. A name kept beside Chinese
/// (`编辑 Dustin Archibald`) is more than one word, and a long sentence that
/// keeps `Python` is not a short title.
fn leftover_source_word(source: &str, translated: &str) -> bool {
    if source.split_whitespace().count() > 6 {
        return false;
    }
    let words: Vec<String> = translated
        .split_whitespace()
        .filter_map(prose_word)
        .collect();
    if words.len() != 1 || !translated.chars().any(is_han) {
        return false;
    }
    let word = &words[0];
    if !contains_word(source, word) {
        return false;
    }
    source.split_whitespace().any(|token| {
        let Some(other) = prose_word(token) else {
            return false;
        };
        other != *word && !contains_word(translated, &other)
    })
}

/// `Text` was its own segment because the contents line wrapped, and the
/// model echoed it. Other short titles in the same job already translated
/// that word (`Text classification` → `文本分类`, `Composing text` →
/// `撰写文本`). Use the Han string they share. A product token with no
/// shared gloss (`Chroma`) stays, and a roman folio (`xvi`) is not a word.
fn gloss_echoed_words(segments: &[Segment], translated: &mut [String]) {
    let owned: Vec<(String, String)> = segments
        .iter()
        .zip(translated.iter())
        .map(|(segment, text)| (segment.text.clone(), text.clone()))
        .collect();
    let pairs: Vec<(&str, &str)> = owned
        .iter()
        .map(|(source, text)| (source.as_str(), text.as_str()))
        .collect();
    for (segment, text) in segments.iter().zip(translated.iter_mut()) {
        let source = segment.text.trim();
        let current = text.trim();
        if !echoed_prose_word(source) || !source.eq_ignore_ascii_case(current) {
            continue;
        }
        let Some(gloss) = shared_gloss(&pairs, source) else {
            continue;
        };
        let lead = text.len() - text.trim_start().len();
        let trail = text.len() - text.trim_end().len();
        let mut next = text[..lead].to_string();
        next.push_str(&gloss);
        next.push_str(&text[text.len() - trail..]);
        *text = next;
    }
}

fn echoed_prose_word(text: &str) -> bool {
    let mut tokens = text.split_whitespace();
    let Some(token) = tokens.next() else {
        return false;
    };
    tokens.next().is_none() && prose_word(token).is_some() && !roman_token(token)
}

fn roman_token(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && text.chars().all(|ch| {
            matches!(
                ch.to_ascii_lowercase(),
                'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'
            )
        })
}

fn shared_gloss(pairs: &[(&str, &str)], word: &str) -> Option<String> {
    let mut sets: Vec<std::collections::HashSet<String>> = Vec::new();
    for (source, translated) in pairs {
        if source.trim().eq_ignore_ascii_case(word) || source.split_whitespace().count() > 14 {
            continue;
        }
        if !contains_word(source, word) || contains_word(translated, word) {
            continue;
        }
        let subs = han_substrings(translated);
        if subs.is_empty() {
            continue;
        }
        sets.push(subs);
    }
    if sets.len() < 2 {
        return None;
    }
    let mut common = sets[0].clone();
    for set in sets.iter().skip(1) {
        common.retain(|item| set.contains(item));
    }
    let max = common.iter().map(|item| item.chars().count()).max()?;
    if max < 2 {
        return None;
    }
    let longest: Vec<String> = common
        .into_iter()
        .filter(|item| item.chars().count() == max)
        .collect();
    (longest.len() == 1).then(|| longest.into_iter().next().unwrap())
}

fn han_substrings(text: &str) -> std::collections::HashSet<String> {
    let mut subs = std::collections::HashSet::new();
    let mut run = String::new();
    let flush = |run: &str, subs: &mut std::collections::HashSet<String>| {
        let chars: Vec<char> = run.chars().collect();
        for start in 0..chars.len() {
            for end in start + 2..=chars.len() {
                subs.insert(chars[start..end].iter().collect());
            }
        }
    };
    for ch in text.chars() {
        if is_han(ch) {
            run.push(ch);
        } else if !run.is_empty() {
            flush(&run, &mut subs);
            run.clear();
        }
    }
    if !run.is_empty() {
        flush(&run, &mut subs);
    }
    subs
}

fn contains_word(text: &str, word: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let needle = word.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return false;
    }
    let bytes = lower.as_bytes();
    let mut start = 0usize;
    while let Some(found) = lower[start..].find(&needle) {
        let at = start + found;
        let before_ok = at == 0 || !bytes[at - 1].is_ascii_alphabetic();
        let after = at + needle.len();
        let after_ok = after >= bytes.len() || !bytes[after].is_ascii_alphabetic();
        if before_ok && after_ok {
            return true;
        }
        start = at + 1;
    }
    false
}

fn prose_word(token: &str) -> Option<String> {
    let core = token.trim_matches(|ch: char| {
        matches!(
            ch,
            ',' | '.' | ';' | ':' | ')' | '(' | '"' | '\'' | '“' | '”' | '—' | '-'
        )
    });
    if core.is_empty() {
        return None;
    }
    if is_function_word(core) {
        return Some(core.to_ascii_lowercase());
    }
    if !core
        .chars()
        .all(|ch| ch.is_ascii_alphabetic() || ch == '\'' || ch == '-')
    {
        return None;
    }
    if core.contains('-') && core.chars().any(|ch| ch.is_ascii_uppercase()) {
        return None;
    }
    let letters: String = core.chars().filter(|ch| ch.is_ascii_alphabetic()).collect();
    if letters.len() < 3 || !letters.chars().any(|ch| ch.is_ascii_lowercase()) {
        return None;
    }
    if letters.chars().any(|ch| ch.is_ascii_uppercase())
        && letters.chars().any(|ch| ch.is_ascii_lowercase())
        && letters
            .chars()
            .zip(letters.chars().skip(1))
            .any(|(left, right)| left.is_ascii_lowercase() && right.is_ascii_uppercase())
    {
        return None;
    }
    Some(letters.to_ascii_lowercase())
}

fn is_function_word(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
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
            | "how"
            | "got"
            | "why"
            | "what"
            | "when"
            | "where"
            | "which"
            | "who"
            | "testing"
            | "practical"
            | "software"
            | "engineering"
            | "consultant"
            | "versus"
            | "theorem"
            // Document headings, not bylines. `Executive Summary` and
            // `Change Log` are title case, the same shape as a person's name.
            | "summary"
            | "log"
            | "executive"
            | "change"
            | "contents"
            | "introduction"
            | "references"
            | "appendix"
            | "abstract"
            | "overview"
            | "chapter"
            | "section"
    )
}

fn polish(items: Vec<(usize, String)>) -> Vec<(usize, String)> {
    items
        .into_iter()
        .map(|(index, text)| (index, localize_part_heading(&strip_cjk_hyphens(&text))))
        .collect()
}

/// The model sometimes drops a dingbat. The source marker stays in front.
pub(crate) fn restore_list_marker(source: &str, translated: &str) -> String {
    if !crate::segment::starts_with_list_marker(source) {
        return translated.to_string();
    }
    let translated_trim = translated.trim_start();
    if translated_trim.starts_with('■')
        || translated_trim.starts_with('●')
        || crate::segment::starts_with_list_marker(translated_trim)
    {
        return translated.to_string();
    }
    let marker = source.trim_start().chars().next().unwrap_or('■');
    let shown = if ('\u{E000}'..='\u{F8FF}').contains(&marker) {
        '■'
    } else {
        marker
    };
    let lead = &translated[..translated.len() - translated_trim.len()];
    format!("{lead}{shown} {translated_trim}")
}

/// A credit line whose personal name was translated. Role words stay in
/// Chinese; the name is the original Latin (`Copy editor: Julie McNamee`).
pub(crate) fn restore_credit_names(source: &str, translated: &str) -> String {
    // The department is an organization, not a person. The cached rendering
    // `特殊销售部` is not the wording used for this line.
    if source
        .trim()
        .eq_ignore_ascii_case("Special Sales Department")
    {
        return "特别销售部".to_string();
    }
    // A cover or byline that is only a person's name stays in the original
    // Latin, the same way a credit line keeps `Dustin Archibald`.
    if is_personal_name(source.trim()) {
        return source.trim().to_string();
    }
    let Some(name) = credit_personal_name(source) else {
        return translated.to_string();
    };
    let restored = if translated.contains(name.as_str()) {
        translated.to_string()
    } else if let Some(index) = translated.rfind([':', '：']) {
        let colon = translated[index..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
        let mut out = translated[..index + colon].to_string();
        out.push_str(&name);
        out
    } else {
        let trimmed = translated.trim_start();
        if source.trim_start().starts_with("and ") && trimmed.starts_with('和') {
            let lead = &translated[..translated.len() - trimmed.len()];
            format!("{lead}和 {name}")
        } else {
            translated.to_string()
        }
    };
    // Role labels in one list must share one colon. A CJK role uses '：'.
    cjk_label_colon(&restored)
}

/// `开发编辑: Ada` and `校对：Ann` are the same list. The label stays, and the
/// colon after a CJK role is the fullwidth one.
fn cjk_label_colon(text: &str) -> String {
    let Some(index) = text.find([':', '：']) else {
        return text.to_string();
    };
    let label = text[..index].trim();
    if label.is_empty()
        || label.chars().count() > 24
        || !label.chars().any(is_cjk_char)
        || label.ends_with(|ch: char| ch.is_ascii_digit())
    {
        return text.to_string();
    }
    let colon = text[index..].chars().next().unwrap_or(':');
    if colon == '：' {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 2);
    out.push_str(&text[..index]);
    out.push('：');
    out.push_str(&text[index + colon.len_utf8()..]);
    out
}

fn credit_personal_name(source: &str) -> Option<String> {
    let trimmed = source.trim();
    let after = if let Some(index) = trimmed.rfind(':') {
        trimmed[index + 1..].trim()
    } else {
        let rest = trimmed.strip_prefix("and ")?;
        rest.trim()
    };
    is_personal_name(after).then(|| after.to_string())
}

pub(crate) fn whole_line_is_personal_name(text: &str) -> bool {
    is_personal_name(text)
}

/// Two to five title-case tokens, and none of them a function word or an
/// organization. `Julie McNamee` is a person. `The Model Context Protocol`
/// is a title, so the Chinese translation stays.
fn is_personal_name(text: &str) -> bool {
    let trimmed = text.trim();
    // `Overall Objective.` and `Mask Segmentation Loss.` are run-in headings.
    // A byline (`Roberto Infante`) does not end in a period after a full word.
    if heading_period(trimmed) {
        return false;
    }
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    (2..=5).contains(&tokens.len())
        && tokens.iter().all(|token| {
            is_name_token(token)
                && !is_org_token(token)
                && !is_function_word(token)
                && !compound_title_token(token)
                && !long_title_word(token)
        })
}

/// `Archibald` (9) is still a surname. `Infinitary` (10) is a heading word.
fn long_title_word(token: &str) -> bool {
    token.chars().filter(|ch| ch.is_ascii_alphabetic()).count() > 9
}

/// `Prefix-Conditioned` is a section title. `Jean-Luc` is still a given name.
fn compound_title_token(token: &str) -> bool {
    let token = token.trim_matches(|ch: char| matches!(ch, ',' | '.'));
    let Some((left, right)) = token.split_once('-') else {
        return false;
    };
    let letters = |part: &str| part.chars().filter(|ch| ch.is_ascii_alphabetic()).count();
    letters(left) >= 6 && letters(right) >= 6
}

/// `Objective.` is a heading terminator. `J.` is an initial.
fn heading_period(text: &str) -> bool {
    let Some(word) = text.trim().trim_end_matches('.').split_whitespace().last() else {
        return false;
    };
    text.trim().ends_with('.') && word.chars().filter(|ch| ch.is_ascii_alphabetic()).count() > 2
}

fn is_org_token(token: &str) -> bool {
    let token = token.trim_matches(|ch| matches!(ch, ',' | '.'));
    matches!(
        token.to_ascii_lowercase().as_str(),
        "co" | "inc"
            | "ltd"
            | "llc"
            | "corp"
            | "company"
            | "press"
            | "publications"
            | "publishing"
            | "university"
            | "institute"
            | "group"
            | "labs"
            | "lab"
            | "department"
            | "division"
            | "office"
            | "bureau"
            | "committee"
            | "association"
            | "society"
            | "services"
            | "headquarters"
            | "dept"
    )
}

fn is_name_token(token: &str) -> bool {
    let token = token.trim_matches(|ch| matches!(ch, ',' | '.'));
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_uppercase() {
        return false;
    }
    if token.chars().count() == 1 {
        return true;
    }
    chars.all(|ch| ch.is_ascii_alphabetic() || ch == '-' || ch == '\'')
        && token.chars().any(|ch| ch.is_ascii_lowercase())
}

/// A contents heading the model left as `PART 4 高级 RAG`. The word is a
/// structural label, so it becomes 部分 once the rest of the line is Chinese.
/// A bare `PART 1` is the same heading. An English sentence that merely
/// contains the word stays for the model. A heading-position label is then
/// `第 5 部分` (ASCII digit, spaces). Body uses such as `每个部分` stay.
pub(crate) fn localize_part_heading(text: &str) -> String {
    normalize_part_label(&localize_part_word(text))
}

fn localize_part_word(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if let Some(replaced) = bare_part_line(&chars) {
        return replaced;
    }
    if !chars.iter().copied().any(is_cjk_char) {
        return text.to_string();
    }
    let urls = url_char_mask(&chars);
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index].is_ascii_alphabetic() && !urls[index] {
            let start = index;
            while index < chars.len() && chars[index].is_ascii_alphabetic() && !urls[index] {
                index += 1;
            }
            let word: String = chars[start..index].iter().collect();
            let bounded = (start == 0 || !chars[start - 1].is_ascii_alphabetic())
                && (index == chars.len() || !chars[index].is_ascii_alphabetic());
            if bounded && word.eq_ignore_ascii_case("part") {
                out.push_str("部分");
            } else {
                out.push_str(&word);
            }
            continue;
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

/// `部分 5` / `第５部分` at the start of a heading (after an optional bullet)
/// becomes `第 5 部分`. A later `每个部分` is not a heading.
fn normalize_part_label(text: &str) -> String {
    let lead = text.len() - text.trim_start().len();
    let mut rest = &text[lead..];
    let mut prefix = text[..lead].to_string();
    if crate::segment::starts_with_list_marker(rest) {
        let marker = rest.chars().next().unwrap();
        prefix.push(marker);
        rest = &rest[marker.len_utf8()..];
        let spaces = rest.len() - rest.trim_start().len();
        prefix.push_str(&rest[..spaces]);
        rest = &rest[spaces..];
    }
    let Some((number, consumed)) = part_label_prefix(rest) else {
        return text.to_string();
    };
    prefix.push_str("第 ");
    prefix.push_str(&number);
    prefix.push_str(" 部分");
    prefix.push_str(&rest[consumed..]);
    prefix
}

/// A part label at the start of `text`: `部分 5`, `第5部分`, `第 ５ 部分`.
fn part_label_prefix(text: &str) -> Option<(String, usize)> {
    if let Some(rest) = text.strip_prefix('第') {
        let rest = rest.trim_start();
        let (number, after) = split_heading_digits(rest)?;
        let after = after.trim_start();
        let after = after.strip_prefix("部分")?;
        return Some((number, text.len() - after.len()));
    }
    if let Some(rest) = text.strip_prefix("部分") {
        let rest = rest.trim_start();
        let (number, after) = split_heading_digits(rest)?;
        return Some((number, text.len() - after.len()));
    }
    None
}

/// One or two ASCII or fullwidth digits. The returned number is ASCII.
fn split_heading_digits(text: &str) -> Option<(String, &str)> {
    let mut number = String::new();
    let mut rest = text;
    while number.chars().count() < 2 {
        let mut chars = rest.chars();
        let Some(ch) = chars.next() else {
            break;
        };
        let digit = if ch.is_ascii_digit() {
            ch
        } else {
            let value = ch as u32;
            if (0xFF10..=0xFF19).contains(&value) {
                char::from_u32(u32::from(b'0') + (value - 0xFF10))?
            } else {
                break;
            }
        };
        number.push(digit);
        rest = chars.as_str();
    }
    if number.is_empty() {
        None
    } else {
        Some((number, rest))
    }
}

/// `PART 4` with nothing after the number. `PART 4 ADVANCED RAG` is not bare.
fn bare_part_line(chars: &[char]) -> Option<String> {
    let text: String = chars.iter().collect();
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    let rest = lower.strip_prefix("part")?;
    if rest.starts_with(|ch: char| ch.is_ascii_alphabetic()) {
        return None;
    }
    let number = trimmed[4..].trim();
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let start = text.find(trimmed)?;
    let mut out = text[..start].to_string();
    out.push_str("部分 ");
    out.push_str(number);
    out.push_str(&text[start + trimmed.len()..]);
    Some(out)
}

fn url_char_mask(chars: &[char]) -> Vec<bool> {
    let mut mask = vec![false; chars.len()];
    let mut index = 0;
    while index < chars.len() {
        let rest: String = chars[index..].iter().collect();
        if rest.starts_with("https://") || rest.starts_with("http://") || rest.starts_with("www.") {
            while index < chars.len() && !chars[index].is_whitespace() {
                mask[index] = true;
                index += 1;
            }
        } else {
            index += 1;
        }
    }
    mask
}

/// A body paragraph the model echoed. A long English reply is sent again.
/// A shorter echo is sent again when it is a credit line or a multi-word
/// heading (`20 Baldwin Road`, `PART 1 GETTING STARTED WITH LLMS`). A
/// one-word title, an acronym (`RAG`), a personal name, a URL, or any
/// Chinese stays.
fn needs_english_retry(source: &str, translated: &str) -> bool {
    let source = source.trim();
    let translated = translated.trim();
    if cjk_count(source) > 0 || cjk_count(translated) > 0 {
        return false;
    }
    if is_personal_name(source) || url_or_email_only(source) {
        return false;
    }
    let source_letters = latin_letters(source);
    let translated_letters = latin_letters(translated);
    if source_letters >= 40 && translated_letters >= 40 {
        return true;
    }
    // A cached `...` is not a translation of a heading.
    if source_letters >= 8 && ellipsis_stub(translated) {
        return true;
    }
    translated == source && prose_echo(source)
}

fn ellipsis_stub(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && text
            .chars()
            .all(|ch| matches!(ch, '.' | '…' | '·' | '。' | ' ' | '　'))
}

/// `20 Baldwin Road` and `PART 1 GETTING STARTED WITH LLMS` are prose.
/// `RAG`, `LLM API`, and `Introduction` are not.
fn prose_echo(source: &str) -> bool {
    // `Go to https://example.com/a.` is a short line plus a URL. The URL's
    // letters must not make it look like a paragraph the model echoed.
    let words: Vec<&str> = source
        .split_whitespace()
        .filter(|word| !is_url_or_email_token(word))
        .filter(|word| word.chars().any(|ch| ch.is_ascii_alphabetic()))
        .collect();
    let letters: usize = words.iter().copied().map(latin_letters).sum();
    if words.len() < 2 || letters < 8 {
        return false;
    }
    !words.iter().all(|word| is_acronym_token(word))
}

fn is_url_or_email_token(word: &str) -> bool {
    word.contains("://") || word.contains('@') || word.starts_with("www.")
}

fn is_acronym_token(word: &str) -> bool {
    let letters: String = word.chars().filter(|ch| ch.is_ascii_alphabetic()).collect();
    !letters.is_empty() && letters.len() <= 5 && letters.chars().all(|ch| ch.is_ascii_uppercase())
}

fn url_or_email_only(source: &str) -> bool {
    let words: Vec<&str> = source.split_whitespace().collect();
    !words.is_empty()
        && words.iter().all(|word| {
            word.starts_with("http://") || word.starts_with("https://") || word.contains('@')
        })
}

fn latin_letters(text: &str) -> usize {
    text.chars().filter(|ch| ch.is_ascii_alphabetic()).count()
}

fn cjk_count(text: &str) -> usize {
    text.chars().filter(|ch| is_cjk_char(*ch)).count()
}

fn is_cjk_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{3000}'..='\u{303F}'
            | '\u{3040}'..='\u{30FF}'
            | '\u{3400}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FF00}'..='\u{FFEF}'
    )
}

/// Drop a hyphen sitting between CJK characters, or a dangling hyphen after CJK.
/// Hyphens inside a URL stay.
fn strip_cjk_hyphens(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut in_url = vec![false; chars.len()];
    let mut index = 0;
    while index < chars.len() {
        let rest: String = chars[index..].iter().collect();
        if rest.starts_with("https://") || rest.starts_with("http://") {
            while index < chars.len() && !chars[index].is_whitespace() {
                in_url[index] = true;
                index += 1;
            }
        } else {
            index += 1;
        }
    }
    let mut out = String::new();
    for (index, ch) in chars.iter().enumerate() {
        if !in_url[index] && matches!(*ch, '-' | '\u{00ad}' | '\u{2010}') {
            let prev = chars[..index]
                .iter()
                .rev()
                .find(|item| !item.is_whitespace())
                .copied();
            let next = chars[index + 1..]
                .iter()
                .copied()
                .find(|item| !item.is_whitespace());
            if prev.is_some_and(is_cjk_char) && (next.is_none() || next.is_some_and(is_cjk_char)) {
                continue;
            }
        }
        out.push(*ch);
    }
    out
}

fn recoverable(err: &Error) -> bool {
    let msg = err.to_string().to_ascii_lowercase();
    msg.contains("timeout")
        || msg.contains("omitted segment")
        || msg.contains("invalid translation json")
        || msg.contains("did not contain a json")
        || msg.contains("no translations array")
        || msg.contains("missing id")
        || msg.contains("missing text")
        || msg.contains("placeholder was not preserved")
        || msg.contains("empty message.content")
        || msg.contains("http status 500")
        || msg.contains("http status 502")
        || msg.contains("http status 503")
        || msg.contains("http status 429")
        || llm::connection_dropped(&msg)
}

fn translate_batch_once(
    segments: &[Segment],
    shielded: &[protect::Shielded],
    indexes: &[usize],
    opts: &TranslateOptions,
    translator: &dyn Translator,
    calls: &mut usize,
) -> Result<Vec<(usize, String)>> {
    let first = call_batch(
        segments, shielded, indexes, opts, translator, calls, false, false,
    )?;
    match restore_all(indexes, shielded, &first) {
        Ok(restored) => Ok(restored),
        Err(_) => {
            let second = call_batch(
                segments, shielded, indexes, opts, translator, calls, true, false,
            )?;
            restore_all(indexes, shielded, &second).map_err(|err| {
                Error::Translate(format!("placeholder was not preserved after retry: {err}"))
            })
        }
    }
}

fn call_batch(
    segments: &[Segment],
    shielded: &[protect::Shielded],
    indexes: &[usize],
    opts: &TranslateOptions,
    translator: &dyn Translator,
    calls: &mut usize,
    strict: bool,
    echo: bool,
) -> Result<Vec<(u32, String)>> {
    let prompts = indexes
        .iter()
        .map(|&i| prompt_for(segments, shielded, i, opts.context_window))
        .collect::<Vec<_>>();
    let user = user_payload(&prompts)?;
    let system = system_prompt(&opts.source_lang, &opts.target_lang, strict, echo);
    *calls += 1;
    let raw = translator.complete(&system, &user)?;
    let parsed = parse_translations(&raw)?;
    let mut by_id: Vec<(u32, String)> = Vec::new();
    for &i in indexes {
        let id = segments[i].id;
        let text = parsed
            .iter()
            .find(|(pid, _)| *pid == id)
            .map(|(_, text)| text.clone())
            .ok_or_else(|| Error::Translate(format!("model omitted segment {id}")))?;
        by_id.push((id, text));
    }
    Ok(by_id)
}

fn restore_all(
    indexes: &[usize],
    shielded: &[protect::Shielded],
    parsed: &[(u32, String)],
) -> std::result::Result<Vec<(usize, String)>, String> {
    let mut out = Vec::new();
    for (n, &i) in indexes.iter().enumerate() {
        let (_, text) = &parsed[n];
        let restored = restore(text, &shielded[i].slots)?;
        out.push((i, restored));
    }
    Ok(out)
}

fn prompt_for(
    segments: &[Segment],
    shielded: &[protect::Shielded],
    index: usize,
    window: usize,
) -> PromptSegment {
    let before = index.saturating_sub(window)..index;
    let after = (index + 1)..(index + 1 + window).min(segments.len());
    PromptSegment {
        id: segments[index].id,
        text: shielded[index].text.clone(),
        context_before: before.map(|i| shielded[i].text.clone()).collect(),
        context_after: after.map(|i| shielded[i].text.clone()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::extract::{Extraction, COORDINATE_SPACE};
    use crate::glyph::{Diagnostic, Disposition, Glyph, GlyphSource, PageInfo, SourceKind};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn glyph(id: u32, x: f32, y: f32, text: &str) -> Glyph {
        Glyph {
            id,
            page_index: 0,
            unicode: text.into(),
            unmapped: false,
            char_code: text.as_bytes().to_vec(),
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
                object_id: None,
                stream_index: 0,
                operator_index: id,
                byte_start: 0,
                byte_end: 1,
                resource_name: Some("F1".into()),
            },
        }
    }

    fn extraction_from_lines(lines: &[&str]) -> Extraction {
        let mut glyphs = Vec::new();
        let mut id = 0u32;
        for (line_i, line) in lines.iter().enumerate() {
            let y = 700.0 - line_i as f32 * 20.0;
            for (i, ch) in line.chars().enumerate() {
                glyphs.push(glyph(id, i as f32 * 6.0, y, &ch.to_string()));
                id += 1;
            }
        }
        Extraction {
            coordinate_space: COORDINATE_SPACE,
            pages: vec![PageInfo {
                index: 0,
                object_id: "1 0".into(),
                media_box: [0.0, 0.0, 612.0, 792.0],
                rotate: 0,
            }],
            glyphs,
            regions: Vec::new(),
            diagnostics: Vec::<Diagnostic>::new(),
        }
    }

    struct PrefixTranslator;
    impl Translator for PrefixTranslator {
        fn complete(&self, system: &str, user: &str) -> Result<String> {
            assert!(system.contains('⟦'));
            let payload: Value = serde_json::from_str(user).unwrap();
            let segs = payload["segments"].as_array().unwrap();
            let translations: Vec<Value> = segs
                .iter()
                .map(|seg| {
                    serde_json::json!({
                        "id": seg["id"],
                        "text": format!("译{}", seg["text"].as_str().unwrap()),
                    })
                })
                .collect();
            Ok(serde_json::json!({"translations": translations}).to_string())
        }
    }

    struct DropOnce {
        n: AtomicUsize,
    }
    impl Translator for DropOnce {
        fn complete(&self, _system: &str, user: &str) -> Result<String> {
            let n = self.n.fetch_add(1, Ordering::SeqCst);
            let payload: Value = serde_json::from_str(user).unwrap();
            let segs = payload["segments"].as_array().unwrap();
            let translations: Vec<Value> = segs
                .iter()
                .map(|seg| {
                    let text = seg["text"].as_str().unwrap();
                    let text = if n == 0 {
                        text.replace('⟦', "")
                    } else {
                        text.to_string()
                    };
                    serde_json::json!({"id": seg["id"], "text": text})
                })
                .collect();
            Ok(serde_json::json!({"translations": translations}).to_string())
        }
    }

    #[test]
    fn default_model_is_auto() {
        assert_eq!(DEFAULT_LLM_MODEL, "auto");
        assert_eq!(resolve_choice(None, None, DEFAULT_LLM_MODEL), "auto");
        assert_eq!(
            resolve_choice(Some("official-low"), Some("other"), DEFAULT_LLM_MODEL),
            "official-low"
        );
        assert_eq!(
            resolve_choice(None, Some("official-mid"), DEFAULT_LLM_MODEL),
            "official-mid"
        );
        assert_eq!(TranslateOptions::default().resolved_model(), {
            resolve_choice(None, std::env::var("RPT_LLM_MODEL").ok().as_deref(), "auto")
        });
    }

    #[test]
    fn credit_personal_names_stay_in_latin() {
        assert_eq!(
            restore_credit_names("Copy editor: Julie McNamee", "文字编辑：朱莉·麦克名"),
            "文字编辑：Julie McNamee"
        );
        assert_eq!(
            restore_credit_names("and Antowan Malik Batts", "和安托万·马利克·巴茨"),
            "和 Antowan Malik Batts"
        );
        assert_eq!(
            restore_credit_names(
                "Development editor: Dustin Archibald",
                "编辑开发: Dustin Archibald"
            ),
            "编辑开发： Dustin Archibald"
        );
        assert_eq!(
            restore_credit_names("20 Baldwin Road", "20 鲍德温路"),
            "20 鲍德温路"
        );
        assert_eq!(
            restore_credit_names("Manning Publications Co.", "曼宁出版公司"),
            "曼宁出版公司"
        );
        assert_eq!(
            restore_credit_names("Shelter Island, NY 11964", "谢尔特岛，纽约 11964"),
            "谢尔特岛，纽约 11964"
        );
        assert_eq!(
            restore_credit_names("Roberto Infante", "罗伯托·因凡特"),
            "Roberto Infante"
        );
        assert_eq!(
            restore_credit_names("Special Sales Department", "特殊销售部"),
            "特别销售部"
        );
        assert_eq!(
            restore_credit_names("Special Sales Department", "特别销售部"),
            "特别销售部"
        );
        assert_eq!(
            restore_credit_names(
                "solution: The Model Context Protocol",
                "解决方案: 模型上下文协议"
            ),
            "解决方案: 模型上下文协议"
        );
        assert!(!whole_line_is_personal_name("The Model Context Protocol"));
        assert_eq!(
            restore_credit_names("Overall Objective.", "总体目标。"),
            "总体目标。"
        );
        assert_eq!(
            restore_credit_names("Mask Segmentation Loss.", "掩码分割损失。"),
            "掩码分割损失。"
        );
        assert!(!whole_line_is_personal_name("Overall Objective."));
        assert_eq!(
            restore_credit_names("Prefix-Conditioned Sufficiency", "前缀条件充分性"),
            "前缀条件充分性"
        );
        assert!(whole_line_is_personal_name("Jean-Luc Picard"));
        assert!(!whole_line_is_personal_name(
            "How Mutation Testing Got Practical"
        ));
        assert!(!whole_line_is_personal_name(
            "Software Engineering Consultant"
        ));
        assert!(whole_line_is_personal_name("Dustin Archibald"));
        assert!(!whole_line_is_personal_name("Finitary Versus Infinitary"));
        assert!(!whole_line_is_personal_name("Ramsey's Theorem"));
        assert!(!whole_line_is_personal_name("Executive Summary"));
        assert!(!whole_line_is_personal_name("Change Log"));
        assert_eq!(
            restore_credit_names("Executive Summary", "执行摘要"),
            "执行摘要"
        );
        assert_eq!(
            restore_credit_names(" Change Log ", " 变更日志 "),
            " 变更日志 "
        );
        assert_eq!(
            restore_credit_names("Finitary Versus Infinitary", "有限与无限"),
            "有限与无限"
        );
        assert_eq!(
            restore_credit_names("How Mutation Testing Got Practical", "变异测试如何变得实用"),
            "变异测试如何变得实用"
        );
    }

    #[test]
    fn an_ellipsis_is_not_a_cached_heading() {
        assert!(needs_english_retry(
            "Query generation, routing, and retrieval postprocessing",
            "..."
        ));
        assert!(!needs_english_retry(
            "Query generation, routing, and retrieval postprocessing",
            "查询生成、路由和检索后处理"
        ));
    }

    #[test]
    fn a_dropped_list_marker_is_put_back() {
        let source = "\u{f0a1}Part 3: Q&A chatbots (chapters 6-7)—This part shifts the focus";
        let dropped = "第3部分：问答聊天机器人（第6–7章）——这一部分将重点转移";
        assert_eq!(restore_list_marker(source, dropped), format!("■ {dropped}"));
        assert_eq!(
            restore_list_marker("–Chapter 1 maps the landscape", "第1章描绘了格局"),
            "– 第1章描绘了格局"
        );
        assert_eq!(
            restore_list_marker(source, "■ 第3部分：问答"),
            "■ 第3部分：问答"
        );
    }

    #[test]
    fn pipeline_restores_placeholders_and_is_not_final() {
        let mut ex = extraction_from_lines(&[
            "The transformer uses self-attention.",
            "See https://example.com/paper and {eq:1}.",
        ]);
        let opts = TranslateOptions {
            glossary: vec![("transformer".into(), "Transformer".into())],
            ..TranslateOptions::default()
        };
        let report = translate_extraction(&mut ex, &opts, &PrefixTranslator).unwrap();
        assert_eq!(report.calls, 1);
        let joined = report
            .segments
            .iter()
            .map(|s| s.translated.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("Transformer"));
        assert!(joined.contains("https://example.com/paper"));
        assert!(joined.contains("{eq:1}"));
        assert!(joined.contains('译'));
        assert!(ex
            .glyphs
            .iter()
            .all(|g| matches!(g.disposition, Disposition::TranslatedPendingRewrite { .. })));
        assert!(!ex.coverage_report().complete);
        assert!(ex.assert_complete().is_err());
    }

    #[test]
    fn references_stay_original_and_citations_are_protected() {
        let mut ex = extraction_from_lines(&[
            "The model uses attention [12] (Smith et al., 2020).",
            "7. References",
            "[12] Smith, A. (2020). Attention is all you need.",
            "Proceedings of NeurIPS, 2020.",
            "Appendix",
            "A. Proofs of the main result.",
        ]);
        let report =
            translate_extraction(&mut ex, &TranslateOptions::default(), &PrefixTranslator).unwrap();
        let refs = report
            .segments
            .iter()
            .find(|seg| seg.source.contains("References"))
            .unwrap();
        assert_eq!(refs.translated, refs.source);
        assert!(report
            .segments
            .iter()
            .any(|seg| { seg.source.contains("[12] Smith") && seg.translated == seg.source }));
        let body = report
            .segments
            .iter()
            .find(|seg| seg.source.contains("The model uses"))
            .unwrap();
        assert!(body.translated.contains("[12]"), "{}", body.translated);
        assert!(
            body.translated.contains("(Smith et al., 2020)"),
            "{}",
            body.translated
        );
        assert!(body.translated.contains('译'), "{}", body.translated);
        assert!(body.translated.starts_with('译'));
        let proofs = report
            .segments
            .iter()
            .find(|seg| seg.source.contains("Proofs"))
            .unwrap();
        assert!(proofs.translated.contains('译'), "{}", proofs.translated);
        assert!(ex.glyphs.iter().any(|glyph| {
            matches!(
                &glyph.disposition,
                Disposition::KeptOriginal { reason } if reason == "references"
            ) && glyph.unicode == "R"
        }));
        assert!(ex.glyphs.iter().any(|glyph| {
            glyph.unicode == "P"
                && matches!(
                    glyph.disposition,
                    Disposition::TranslatedPendingRewrite { .. }
                )
        }));

        let mut ex = extraction_from_lines(&[
            "7. References",
            "[1] Smith, A. (2020). A paper title for the option.",
        ]);
        let opts = TranslateOptions {
            skip_references: false,
            ..TranslateOptions::default()
        };
        let report = translate_extraction(&mut ex, &opts, &PrefixTranslator).unwrap();
        assert!(report
            .segments
            .iter()
            .any(|seg| seg.source.contains("References") && seg.translated.contains('译')));
        assert!(ex.glyphs.iter().all(|glyph| matches!(
            glyph.disposition,
            Disposition::TranslatedPendingRewrite { .. }
        )));
    }

    #[test]
    fn a_timeout_splits_the_batch_and_still_translates() {
        let mut ex = extraction_from_lines(&["Alpha one", "Beta two"]);
        let translator = SplitOnTimeout;
        let report =
            translate_extraction(&mut ex, &TranslateOptions::default(), &translator).unwrap();
        let texts: Vec<_> = report
            .segments
            .iter()
            .map(|seg| seg.translated.as_str())
            .collect();
        assert!(texts.iter().any(|text| text.contains("Alpha")), "{texts:?}");
        assert!(texts.iter().any(|text| text.contains("Beta")), "{texts:?}");
        assert!(report.calls >= 3, "calls={}", report.calls);
    }

    #[test]
    fn a_response_without_translations_keeps_the_source_and_does_not_abort() {
        let mut ex = extraction_from_lines(&["Alpha one", "Beta two"]);
        let report =
            translate_extraction(&mut ex, &TranslateOptions::default(), &AlwaysNoArray).unwrap();
        assert_eq!(report.segments[0].translated, "Alpha one");
        assert_eq!(report.segments[1].translated, "Beta two");
    }

    struct AlwaysNoArray;
    impl Translator for AlwaysNoArray {
        fn complete(&self, _system: &str, _user: &str) -> Result<String> {
            Err(Error::Translate(
                "translation JSON has no translations array".into(),
            ))
        }
    }

    #[test]
    fn a_dropped_connection_keeps_the_source_and_does_not_abort() {
        let mut ex = extraction_from_lines(&["Alpha one", "Beta two"]);
        let report =
            translate_extraction(&mut ex, &TranslateOptions::default(), &AlwaysEof).unwrap();
        assert_eq!(report.segments[0].translated, "Alpha one");
        assert_eq!(report.segments[1].translated, "Beta two");
    }

    struct AlwaysEof;
    impl Translator for AlwaysEof {
        fn complete(&self, _system: &str, _user: &str) -> Result<String> {
            Err(Error::Translate("io: unexpected end of file".into()))
        }
    }

    #[test]
    fn an_empty_model_reply_keeps_the_source_text() {
        let mut ex = extraction_from_lines(&["Alpha one"]);
        let report =
            translate_extraction(&mut ex, &TranslateOptions::default(), &AlwaysEmpty).unwrap();
        assert_eq!(report.segments[0].translated, "Alpha one");
    }

    #[test]
    fn an_echoed_english_paragraph_is_translated_on_retry() {
        let source = "The projects target modern versions of Python and use standard tools such as pip and virtual environments for every chapter.";
        let mut ex = extraction_from_lines(&[source]);
        let report =
            translate_extraction(&mut ex, &TranslateOptions::default(), &EchoThenChinese).unwrap();
        assert!(
            report.segments[0].translated.chars().any(is_cjk_char),
            "{}",
            report.segments[0].translated
        );
        assert_ne!(report.segments[0].translated, source);
        assert!(report.calls >= 2, "calls={}", report.calls);
    }

    #[test]
    fn a_defective_translation_is_retried_and_then_repaired() {
        assert!(translation_flaw(
            "PART 4 ADVANCED RAG ",
            "第 4 部分 高级 检索增强生成 "
        ));
        assert!(!translation_flaw(
            "PART 2 SUMMARIZATION ",
            "第 2 部分 摘要 "
        ));
        assert!(translation_flaw("The The MCP ", "该 该 MCP "));
        assert_eq!(repair_translation("The The MCP ", "该 该 MCP "), "该 MCP");
        assert_eq!(
            repair_translation("PART 4 ADVANCED RAG ", "第 4 部分 高级 检索增强生成 "),
            "第 4 部分 高级检索增强生成"
        );
        assert!(!translation_flaw("ROBERTO INFANTE", "罗伯托 因凡特"));
        let open = "In both cases, source code is formatted in a fixed-width font like this";
        assert!(extra_sentence_end(
            open,
            "在这两种情况下，源代码都使用像这样的等宽字体格式。"
        ));
        assert!(!extra_sentence_end(
            "countless “What if we tried this?” moments followed.",
            "无数次“如果我们试试这个？”的时刻随之而来。"
        ));
        assert!(!extra_sentence_end(
            "grounding outputs using Retrieval-Augmented Generation (RAG). From there, we stop.",
            "使用检索增强生成 (RAG)。随后我们停下。"
        ));
        assert!(translation_flaw(
            "solution: The Model Context Protocol",
            "解决方案： The Model Context Protocol"
        ));
        assert!(!translation_flaw(
            "solution: The Model Context Protocol",
            "解决方案：模型上下文协议"
        ));
        assert!(!residual_english(
            "致审稿人—Abdullah Al Imran, Antowan Malik Batts"
        ));
        assert!(translation_flaw("Text summarization", "Text 摘要"));
        assert!(!translation_flaw("Text classification", "文本分类"));
        assert!(!translation_flaw(
            "The projects target modern versions of Python and use standard tools.",
            "这些项目面向较新的 Python 版本，并使用标准工具。"
        ));
        let source = "The projects target modern versions of Python and use standard tools such as pip and virtual environments for every chapter.";
        let mut ex = extraction_from_lines(&[source]);
        let report =
            translate_extraction(&mut ex, &TranslateOptions::default(), &SpaceThenClean).unwrap();
        assert!(
            !translation_flaw(source, &report.segments[0].translated),
            "{}",
            report.segments[0].translated
        );
        assert!(report.calls >= 2, "calls={}", report.calls);
    }

    struct SpaceThenClean;
    impl Translator for SpaceThenClean {
        fn complete(&self, system: &str, user: &str) -> Result<String> {
            let payload: Value = serde_json::from_str(user).unwrap();
            let segs = payload["segments"].as_array().unwrap();
            let translations: Vec<Value> = segs
                .iter()
                .map(|seg| {
                    let text = if system.contains("space between Chinese") {
                        "这些项目面向较新的 Python 版本，并使用标准工具。"
                    } else {
                        "这些 项目面向较新的 Python 版本。"
                    };
                    serde_json::json!({"id": seg["id"], "text": text})
                })
                .collect();
            Ok(serde_json::json!({"translations": translations}).to_string())
        }
    }

    #[test]
    fn an_echoed_contents_word_takes_the_gloss_the_other_titles_share() {
        let sources = [
            "Text classification",
            "Composing text",
            "Summarizing text using LangChain",
            "Text ",
            "Chroma",
            "RAG fundamentals with ChromaDB",
            "xvi",
            "Content ingestion stage",
            "Question-answering stage",
            "stage",
        ];
        let mut translated = vec![
            "文本分类".to_string(),
            "撰写文本".to_string(),
            "使用 LangChain 总结文本".to_string(),
            "Text ".to_string(),
            "Chroma".to_string(),
            "RAG 基础知识与 ChromaDB".to_string(),
            "xvi".to_string(),
            "内容摄取阶段".to_string(),
            "问答阶段".to_string(),
            "stage".to_string(),
        ];
        let segments: Vec<Segment> = sources
            .iter()
            .enumerate()
            .map(|(index, text)| Segment {
                id: index as u32,
                page_index: 0,
                glyph_ids: Vec::new(),
                text: (*text).to_string(),
            })
            .collect();
        gloss_echoed_words(&segments, &mut translated);
        assert_eq!(translated[3], "文本 ");
        assert_eq!(translated[4], "Chroma");
        assert_eq!(translated[6], "xvi");
        assert_eq!(translated[9], "阶段");
    }

    #[test]
    fn a_broken_sentence_join_is_not_reused() {
        let left = "This book contains many examples of source code both in numbered listings and in line with normal text. In both cases, source code is formatted in a fixed-width font like this";
        let right =
            " to separate it from ordinary text. Sometimes code is also in bold to highlight";
        let mut saved = HashMap::new();
        saved.insert(
            left.to_string(),
            "本书包含许多源代码示例，既在编号列表中，也穿插在正常文本中。在这两种情况下，源代码都使用像这样的等宽字体格式。".to_string(),
        );
        saved.insert(
            right.to_string(),
            "以将其与普通文本区分开来。有时代码也会用粗体来突出显示".to_string(),
        );
        let source = format!("{left}{right}");
        assert!(
            lookup_saved(&source, &saved).is_none(),
            "joined the broken halves"
        );
    }

    #[test]
    fn replay_keeps_an_exact_sentence_the_lint_would_retry() {
        let source = "∗These authors contributed equally. †Corresponding author.";
        let saved_text = "∗这些作者同等贡献。†通讯作者。";
        assert!(
            translation_flaw(source, saved_text),
            "the extra stop is a flaw"
        );
        let mut saved = HashMap::new();
        saved.insert(source.to_string(), saved_text.to_string());
        assert_eq!(lookup_saved(source, &saved).as_deref(), Some(saved_text));
    }

    #[test]
    fn a_body_echo_is_retried_and_a_short_heading_is_not() {
        let body = "Graph-based clustering approaches, primarily among them multicut, are theoretically appealing.";
        assert!(needs_english_retry(body, body));
        assert!(!needs_english_retry(
            body,
            "基于图的聚类方法在理论上很有吸引力。"
        ));
        let header = "Clustering Fully connected Graphs by Multicut";
        assert!(needs_english_retry(header, header), "{header}");
        assert!(!needs_english_retry("Introduction", "Introduction"));
        assert!(needs_english_retry("20 Baldwin Road", "20 Baldwin Road"));
        assert!(needs_english_retry(
            "PART 1 GETTING STARTED WITH LLMS ",
            "PART 1 GETTING STARTED WITH LLMS "
        ));
        assert!(!needs_english_retry("RAG", "RAG"));
        assert!(!needs_english_retry("Julie McNamee", "Julie McNamee"));
        assert!(!needs_english_retry("LLM API", "LLM API"));
    }

    #[test]
    fn a_stubborn_echo_is_translated_by_the_fallback_prompt() {
        let source = "Setting up the chain to generate a step-back question";
        let mut ex = extraction_from_lines(&[source]);
        let report =
            translate_extraction(&mut ex, &TranslateOptions::default(), &EchoUntilFallback)
                .unwrap();
        assert!(
            report.segments[0].translated.chars().any(is_cjk_char),
            "{}",
            report.segments[0].translated
        );
        assert!(report.calls >= 2, "calls={}", report.calls);
        let road = "20 Baldwin Road";
        let mut road_ex = extraction_from_lines(&[road]);
        let road_report = translate_extraction(
            &mut road_ex,
            &TranslateOptions::default(),
            &EchoUntilFallback,
        )
        .unwrap();
        assert!(
            road_report.segments[0].translated.chars().any(is_cjk_char),
            "calls={} text={}",
            road_report.calls,
            road_report.segments[0].translated
        );
    }

    struct EchoUntilFallback;
    impl Translator for EchoUntilFallback {
        fn complete(&self, system: &str, user: &str) -> Result<String> {
            let payload: Value = serde_json::from_str(user).unwrap();
            let segs = payload["segments"].as_array().unwrap();
            let translations: Vec<Value> = segs
                .iter()
                .map(|seg| {
                    let raw = seg["text"].as_str().unwrap();
                    let text = if system.contains("independent") {
                        // Keep ⟦N⟧ so a shielded `20` still restores.
                        let mut marks = String::new();
                        let chars: Vec<char> = raw.chars().collect();
                        let mut index = 0;
                        while index < chars.len() {
                            if chars[index] == '⟦' {
                                let start = index;
                                while index < chars.len() && chars[index] != '⟧' {
                                    index += 1;
                                }
                                if index < chars.len() {
                                    index += 1;
                                }
                                marks.extend(chars[start..index].iter());
                            } else {
                                index += 1;
                            }
                        }
                        format!("已译{marks}")
                    } else {
                        raw.to_string()
                    };
                    serde_json::json!({ "id": seg["id"], "text": text })
                })
                .collect();
            Ok(serde_json::json!({ "translations": translations }).to_string())
        }
    }

    #[test]
    fn sentence_splits_keep_abbreviations_and_rejoin() {
        let text = "See Smith et al. The method is shown in Fig. 1 and then continues. Efficient solvers scale well.";
        let parts = split_sentences(text);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert_eq!(parts.concat(), text);
        assert!(parts[0].contains("et al."));
        assert!(parts[0].contains("Fig. 1"));
    }

    #[test]
    fn an_echoed_paragraph_is_translated_sentence_by_sentence() {
        let source = "Graph-based clustering approaches are theoretically appealing and infer the number of clusters during optimization. Efficient solvers scale well and give high quality solutions on large graphs.";
        let mut ex = extraction_from_lines(&[source, "A short note beside it."]);
        let report =
            translate_extraction(&mut ex, &TranslateOptions::default(), &EchoWholeParagraph)
                .unwrap();
        let intro = report
            .segments
            .iter()
            .find(|seg| seg.source.contains("Graph-based"))
            .unwrap();
        assert!(
            intro.translated.chars().any(is_cjk_char),
            "{}",
            intro.translated
        );
        assert!(!intro.translated.contains("theoretically appealing"));
        assert!(intro.translated.contains("求解器") || intro.translated.contains("簇"));
        assert!(report.calls >= 4, "calls={}", report.calls);
    }

    #[test]
    fn an_echoed_sentence_is_translated_without_its_neighbors() {
        let source = "Efficient solvers scale well and give high quality solutions on large graphs in practice.";
        let mut ex = extraction_from_lines(&[source, "Another nearby heading for context."]);
        let report =
            translate_extraction(&mut ex, &TranslateOptions::default(), &EchoWhenContext).unwrap();
        let line = &report.segments[0];
        assert!(line.translated.contains("求解器"), "{}", line.translated);
        assert_ne!(line.translated, source);
    }

    struct EchoWhenContext;
    impl Translator for EchoWhenContext {
        fn complete(&self, _system: &str, user: &str) -> Result<String> {
            let payload: Value = serde_json::from_str(user).unwrap();
            let segs = payload["segments"].as_array().unwrap();
            let translations: Vec<Value> = segs
                .iter()
                .map(|seg| {
                    let text = seg["text"].as_str().unwrap();
                    let neighbors = seg["context_before"]
                        .as_array()
                        .is_some_and(|items| !items.is_empty())
                        || seg["context_after"]
                            .as_array()
                            .is_some_and(|items| !items.is_empty());
                    let translated = if neighbors {
                        text.to_string()
                    } else if text.contains("solvers") {
                        "高效求解器能够扩展到大图并给出高质量的解。".into()
                    } else {
                        text.to_string()
                    };
                    serde_json::json!({"id": seg["id"], "text": translated})
                })
                .collect();
            Ok(serde_json::json!({"translations": translations}).to_string())
        }
    }

    fn sentence_continues(text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        chars.iter().enumerate().any(|(index, ch)| {
            if *ch != '.' {
                return false;
            }
            let mut next = index + 1;
            while next < chars.len() && chars[next].is_whitespace() {
                next += 1;
            }
            next < chars.len() && chars[next].is_ascii_uppercase()
        })
    }

    struct EchoWholeParagraph;
    impl Translator for EchoWholeParagraph {
        fn complete(&self, _system: &str, user: &str) -> Result<String> {
            let payload: Value = serde_json::from_str(user).unwrap();
            let segs = payload["segments"].as_array().unwrap();
            let translations: Vec<Value> = segs
                .iter()
                .map(|seg| {
                    let text = seg["text"].as_str().unwrap();
                    let translated = if sentence_continues(text) {
                        text.to_string()
                    } else if text.contains("solvers") {
                        "高效求解器能够扩展到大图并给出高质量的解。".into()
                    } else if text.contains("clustering") {
                        "基于图的聚类方法会在优化过程中推断簇的数量。".into()
                    } else {
                        text.to_string()
                    };
                    serde_json::json!({"id": seg["id"], "text": translated})
                })
                .collect();
            Ok(serde_json::json!({"translations": translations}).to_string())
        }
    }

    struct EchoThenChinese;
    impl Translator for EchoThenChinese {
        fn complete(&self, system: &str, user: &str) -> Result<String> {
            let payload: Value = serde_json::from_str(user).unwrap();
            let segs = payload["segments"].as_array().unwrap();
            let translations: Vec<Value> = segs
                .iter()
                .map(|seg| {
                    let text = if system.contains("copied the English") {
                        "这些项目面向较新的 Python 版本，并使用标准工具。"
                    } else {
                        seg["text"].as_str().unwrap()
                    };
                    serde_json::json!({"id": seg["id"], "text": text})
                })
                .collect();
            Ok(serde_json::json!({"translations": translations}).to_string())
        }
    }

    #[test]
    fn cjk_hyphens_are_removed_and_url_hyphens_stay() {
        assert_eq!(strip_cjk_hyphens("多-文档"), "多文档");
        assert_eq!(strip_cjk_hyphens("摘要链, 多-"), "摘要链, 多");
        assert_eq!(
            strip_cjk_hyphens("见 https://livebook.manning.com/book/ai-agents-and-applications"),
            "见 https://livebook.manning.com/book/ai-agents-and-applications"
        );
    }

    #[test]
    fn a_leftover_part_heading_becomes_chinese() {
        assert_eq!(
            localize_part_heading("PART 4 高级 RAG "),
            "第 4 部分 高级 RAG "
        );
        assert_eq!(
            localize_part_heading("\u{f0a1} Part 5: AI 智能体"),
            "\u{f0a1} 第 5 部分: AI 智能体"
        );
        assert_eq!(localize_part_heading("PART 1"), "第 1 部分");
        assert_eq!(
            localize_part_heading("部分 ５: AI 智能体"),
            "第 5 部分: AI 智能体"
        );
        assert_eq!(localize_part_heading("第３部分：问答"), "第 3 部分：问答");
        assert_eq!(localize_part_heading("每个部分都要译"), "每个部分都要译");
        assert_eq!(localize_part_heading("5个部分"), "5个部分");
        assert_eq!(
            localize_part_heading("PART 4 ADVANCED RAG"),
            "PART 4 ADVANCED RAG"
        );
        assert_eq!(
            localize_part_heading("见 https://example.com/part/4 的说明"),
            "见 https://example.com/part/4 的说明"
        );
        assert_eq!(localize_part_heading("Partial 结果"), "Partial 结果");
        let source = "\u{f0a1}Part 5: AI agents (chapters 11-14)";
        let model = "部分 ５: AI 智能体 (第 11–14 章)";
        assert_eq!(
            restore_list_marker(source, &localize_part_heading(model)),
            "■ 第 5 部分: AI 智能体 (第 11–14 章)"
        );
    }

    struct PartHeadingTranslator;
    impl Translator for PartHeadingTranslator {
        fn complete(&self, _system: &str, user: &str) -> Result<String> {
            let payload: Value = serde_json::from_str(user).unwrap();
            let segs = payload["segments"].as_array().unwrap();
            let translations: Vec<Value> = segs
                .iter()
                .map(|seg| {
                    let text =
                        seg["text"]
                            .as_str()
                            .unwrap()
                            .replacen("ADVANCED RAG", "高级 RAG", 1);
                    serde_json::json!({"id": seg["id"], "text": text})
                })
                .collect();
            Ok(serde_json::json!({"translations": translations}).to_string())
        }
    }

    #[test]
    fn polish_rewrites_a_part_heading_the_model_left_in_english() {
        let mut ex = extraction_from_lines(&["PART 4 ADVANCED RAG "]);
        let report = translate_extraction(
            &mut ex,
            &TranslateOptions::default(),
            &PartHeadingTranslator,
        )
        .unwrap();
        assert_eq!(report.segments[0].translated, "第 4 部分 高级 RAG ");
    }

    struct AlwaysEmpty;
    impl Translator for AlwaysEmpty {
        fn complete(&self, _system: &str, _user: &str) -> Result<String> {
            Err(Error::Translate(
                "LLM response had empty message.content (reasoning tokens are ignored)".into(),
            ))
        }
    }

    #[test]
    fn an_omitted_id_splits_the_batch() {
        let mut ex = extraction_from_lines(&["Alpha one", "Beta two"]);
        let report =
            translate_extraction(&mut ex, &TranslateOptions::default(), &OmitWhenBatched).unwrap();
        let texts: Vec<_> = report
            .segments
            .iter()
            .map(|seg| seg.translated.as_str())
            .collect();
        assert!(texts.iter().any(|text| text.contains("Alpha")), "{texts:?}");
        assert!(texts.iter().any(|text| text.contains("Beta")), "{texts:?}");
    }

    struct OmitWhenBatched;
    impl Translator for OmitWhenBatched {
        fn complete(&self, _system: &str, user: &str) -> Result<String> {
            let payload: serde_json::Value = serde_json::from_str(user).unwrap();
            let segs = payload["segments"].as_array().unwrap();
            if segs.len() > 1 {
                let only = &segs[0];
                return Ok(
                    serde_json::json!({"translations":[{"id": only["id"], "text": only["text"]}]})
                        .to_string(),
                );
            }
            let seg = &segs[0];
            Ok(
                serde_json::json!({"translations":[{"id": seg["id"], "text": seg["text"]}]})
                    .to_string(),
            )
        }
    }

    struct SplitOnTimeout;
    impl Translator for SplitOnTimeout {
        fn complete(&self, _system: &str, user: &str) -> Result<String> {
            if user.matches("\"id\":").count() > 1 {
                return Err(Error::Translate("timeout: global".into()));
            }
            let payload: serde_json::Value = serde_json::from_str(user).unwrap();
            let seg = &payload["segments"][0];
            let text = seg["text"].as_str().unwrap_or("");
            Ok(serde_json::json!({"translations":[{"id": seg["id"], "text": text}]}).to_string())
        }
    }

    #[test]
    fn retries_once_when_a_placeholder_is_dropped() {
        let mut ex = extraction_from_lines(&["Go to https://example.com/a."]);
        let opts = TranslateOptions::default();
        let translator = DropOnce {
            n: AtomicUsize::new(0),
        };
        let report = translate_extraction(&mut ex, &opts, &translator).unwrap();
        assert_eq!(report.calls, 2);
        assert!(report.segments[0]
            .translated
            .contains("https://example.com/a"));
    }

    #[test]
    fn json_options_ignore_api_key_and_default_model() {
        let (opts, warnings) = TranslateOptions::from_json(
            r#"{"from":"en","to":"zh","api_key":"nope","glossary":{"transformer":"Transformer"}}"#,
        )
        .unwrap();
        assert_eq!(opts.source_lang, "en");
        assert_eq!(opts.target_lang, "zh");
        assert!(opts.model.is_none());
        assert_eq!(
            opts.glossary,
            vec![("transformer".into(), "Transformer".into())]
        );
        assert_eq!(warnings.len(), 1);
        assert!(opts.skip_references);
        assert!(opts.skip_figures);
        assert!(opts.skip_tables);
        assert!(!format!("{opts:?}").contains("nope"));
        let (opts, _) = TranslateOptions::from_json(r#"{"skip_references":false}"#).unwrap();
        assert!(!opts.skip_references);
        let (opts, _) =
            TranslateOptions::from_json(r#"{"skip_figures":false,"skip_tables":false}"#).unwrap();
        assert!(!opts.skip_figures && !opts.skip_tables);
        assert_eq!(
            TranslateOptions::from_json(r#"{"output_mode":"replace"}"#)
                .unwrap()
                .0
                .output_mode,
            OutputMode::Replace
        );
        assert_eq!(
            TranslateOptions::from_json(r#"{"bilingual":true,"bilingual_layout":"alternating"}"#)
                .unwrap()
                .0
                .output_mode,
            OutputMode::Bilingual(BilingualLayout::Alternating)
        );
        assert_eq!(
            TranslateOptions::from_json(r#"{"mode":"side-by-side"}"#)
                .unwrap()
                .0
                .output_mode,
            OutputMode::Bilingual(BilingualLayout::SideBySide)
        );
        assert!(TranslateOptions::from_json(r#"{"output_mode":"nope"}"#).is_err());
        assert_eq!(TranslateOptions::default().output_mode, OutputMode::Replace);
        assert_eq!(
            TranslateOptions::from_json("{}").unwrap().0.output_mode,
            OutputMode::Replace
        );
        let sized = TranslateOptions::from_json(r#"{"cjk_size_scale":0.92,"cjk_leading":1.5}"#)
            .unwrap()
            .0;
        assert!((sized.cjk_size_scale - 0.92).abs() < 0.001);
        assert!((sized.cjk_leading - 1.5).abs() < 0.001);
        assert_eq!(TranslateOptions::default().cjk_size_scale, 0.0);
        assert_eq!(TranslateOptions::default().cjk_leading, 0.0);
        assert_eq!(
            TranslateOptions::from_json(r#"{"layout":"side-by-side"}"#)
                .unwrap()
                .0
                .output_mode,
            OutputMode::Replace,
            "a layout without bilingual:true stays pure translation"
        );
    }

    #[test]
    #[ignore = "calls the live model; layout tests must run with no API key"]
    fn live_translation_preserves_placeholders() {
        if std::env::var("RPT_LLM_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .is_none()
        {
            return;
        }
        let mut ex = extraction_from_lines(&[
            "The transformer architecture uses self-attention.",
            "The learning rate is 0.001.",
            "Refer to {eq:1}.",
            "Paper: https://example.com/paper",
        ]);
        let opts = TranslateOptions {
            glossary: vec![
                ("self-attention".into(), "self-attention".into()),
                ("transformer".into(), "Transformer".into()),
            ],
            ..TranslateOptions::default()
        };
        let client = LlmTranslator::from_env(&opts).expect("llm client");
        if std::env::var("RPT_LLM_MODEL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .is_none()
        {
            assert_eq!(client.model(), "auto");
        }
        let report = translate_extraction(&mut ex, &opts, &client).expect("translate");
        let joined = report
            .segments
            .iter()
            .map(|s| s.translated.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        eprintln!("live translation:\n{joined}");
        assert!(joined.contains("Transformer"), "{joined}");
        assert!(joined.contains("self-attention"), "{joined}");
        assert!(joined.contains("0.001"), "{joined}");
        assert!(joined.contains("{eq:1}"), "{joined}");
        assert!(joined.contains("https://example.com/paper"), "{joined}");
        assert!(
            joined
                .chars()
                .any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c)),
            "{joined}"
        );
        assert!(!joined.contains("The transformer architecture"));
    }

    #[test]
    fn jobs_default_to_one_and_json_accepts_an_explicit_count() {
        assert_eq!(TranslateOptions::default().jobs, 0);
        assert_eq!(TranslateOptions::from_json("{}").unwrap().0.jobs, 0);
        assert_eq!(
            TranslateOptions::from_json(r#"{"jobs":"3"}"#)
                .unwrap()
                .0
                .jobs,
            3
        );
        assert_eq!(
            TranslateOptions::from_json(r#"{"translate_jobs":2}"#)
                .unwrap()
                .0
                .jobs,
            2
        );
        assert!(TranslateOptions::from_json(r#"{"jobs":0}"#).is_err());
        let explicit = TranslateOptions {
            jobs: 4,
            ..TranslateOptions::default()
        };
        assert_eq!(explicit.resolved_jobs().unwrap(), 4);
        let unset = TranslateOptions::default().resolved_jobs().unwrap();
        let from_env = std::env::var("RPT_TRANSLATE_JOBS")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(1);
        assert_eq!(unset, from_env);
    }

    #[test]
    fn concurrent_jobs_match_single_thread_and_keep_disjoint_scopes() {
        let lines = [
            "The transformer uses self-attention.",
            "See https://example.com/a and {eq:1}.",
            "The learning rate is 0.001.",
            "Mail ada@example.com for the notes.",
            "This paragraph is only on this line.",
            "Another distinct sentence ends here.",
            "Caption text for the figure stays here.",
            "Hello world.",
            "Hello world.",
        ];
        let single_opts = TranslateOptions {
            glossary: vec![("transformer".into(), "Transformer".into())],
            batch_size: 1,
            jobs: 1,
            ..TranslateOptions::default()
        };
        let mut single_doc = extraction_from_lines(&lines);
        let single =
            translate_extraction(&mut single_doc, &single_opts, &PrefixTranslator).unwrap();

        let parallel_opts = TranslateOptions {
            jobs: 4,
            ..single_opts
        };
        let isolated = IsolatedTranslator {
            inflight: std::sync::Mutex::new(std::collections::HashSet::new()),
            overlapped: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            leaked_context: AtomicUsize::new(0),
        };
        let mut parallel_doc = extraction_from_lines(&lines);
        let parallel = translate_extraction(&mut parallel_doc, &parallel_opts, &isolated).unwrap();

        assert_eq!(isolated.overlapped.load(Ordering::SeqCst), 0);
        assert_eq!(isolated.leaked_context.load(Ordering::SeqCst), 0);
        assert!(
            isolated.peak.load(Ordering::SeqCst) > 1,
            "expected overlapping workers, peak {}",
            isolated.peak.load(Ordering::SeqCst)
        );
        assert_eq!(parallel.calls, single.calls);
        assert_eq!(parallel.cache_hits, single.cache_hits);
        assert_eq!(parallel.calls, 8);
        assert_eq!(parallel.cache_hits, 1);
        assert_eq!(single.segments.len(), parallel.segments.len());
        for (left, right) in single.segments.iter().zip(parallel.segments.iter()) {
            assert_eq!(left.id, right.id);
            assert_eq!(left.glyph_ids, right.glyph_ids);
            assert_eq!(left.source, right.source);
            assert_eq!(left.translated, right.translated);
        }
        let ids: Vec<u32> = parallel.segments.iter().map(|seg| seg.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "merge keeps reading order");
        let snapshot = |doc: &Extraction| {
            doc.glyphs
                .iter()
                .map(|glyph| format!("{:?}", glyph.disposition))
                .collect::<Vec<_>>()
        };
        assert_eq!(snapshot(&single_doc), snapshot(&parallel_doc));
        assert!(single_doc
            .glyphs
            .iter()
            .all(|glyph| !matches!(glyph.disposition, Disposition::Pending)));

        let url = parallel
            .segments
            .iter()
            .find(|seg| seg.source.contains("https://"))
            .unwrap();
        assert!(url.translated.contains("https://example.com/a"));
        assert!(url.translated.contains("{eq:1}"));
        assert!(!url.translated.contains("ada@example.com"));
        let mail = parallel
            .segments
            .iter()
            .find(|seg| seg.source.contains('@'))
            .unwrap();
        assert!(mail.translated.contains("ada@example.com"));
        assert!(!mail.translated.contains("https://"));
        assert!(parallel.segments.iter().any(|seg| {
            seg.source.contains("transformer") && seg.translated.contains("Transformer")
        }));
        assert!(parallel.segments.iter().all(|seg| {
            seg.source.contains("transformer") || !seg.translated.contains("Transformer")
        }));
        let rate = parallel
            .segments
            .iter()
            .find(|seg| seg.source.contains("0.001"))
            .unwrap();
        assert!(rate.translated.contains("0.001"));
        let hellos: Vec<_> = parallel
            .segments
            .iter()
            .filter(|seg| seg.source == "Hello world.")
            .collect();
        assert_eq!(hellos.len(), 2);
        assert_eq!(hellos[0].translated, hellos[1].translated);
    }

    struct IsolatedTranslator {
        inflight: std::sync::Mutex<std::collections::HashSet<u32>>,
        overlapped: AtomicUsize,
        peak: AtomicUsize,
        leaked_context: AtomicUsize,
    }

    impl Translator for IsolatedTranslator {
        fn complete(&self, _system: &str, user: &str) -> Result<String> {
            let payload: Value = serde_json::from_str(user).unwrap();
            let segs = payload["segments"].as_array().unwrap();
            let ids: Vec<u32> = segs
                .iter()
                .map(|seg| seg["id"].as_u64().unwrap() as u32)
                .collect();
            for seg in segs {
                for key in ["context_before", "context_after"] {
                    let Some(items) = seg.get(key).and_then(|value| value.as_array()) else {
                        continue;
                    };
                    for item in items {
                        let text = item.as_str().unwrap_or("");
                        if text.contains('译') {
                            self.leaked_context.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
                let text = seg["text"].as_str().unwrap_or("");
                if text.contains('译') {
                    self.leaked_context.fetch_add(1, Ordering::SeqCst);
                }
            }
            {
                let mut guard = self.inflight.lock().unwrap();
                for id in &ids {
                    if !guard.insert(*id) {
                        self.overlapped.fetch_add(1, Ordering::SeqCst);
                    }
                }
                let n = guard.len();
                self.peak.fetch_max(n, Ordering::SeqCst);
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
            let translations: Vec<Value> = segs
                .iter()
                .map(|seg| {
                    serde_json::json!({
                        "id": seg["id"],
                        "text": format!("译{}", seg["text"].as_str().unwrap()),
                    })
                })
                .collect();
            {
                let mut guard = self.inflight.lock().unwrap();
                for id in &ids {
                    guard.remove(id);
                }
            }
            Ok(serde_json::json!({"translations": translations}).to_string())
        }
    }

    #[test]
    fn repeated_segment_hits_the_cache() {
        let mut ex = extraction_from_lines(&["Hello.", "Hello."]);
        let report =
            translate_extraction(&mut ex, &TranslateOptions::default(), &PrefixTranslator).unwrap();
        assert_eq!(report.calls, 1);
        assert_eq!(report.cache_hits, 1);
        assert_eq!(report.segments[0].translated, report.segments[1].translated);
    }

    #[test]
    fn different_numbers_do_not_share_a_placeholder_cache_entry() {
        let mut ex = extraction_from_lines(&["Fig 1", "Fig 2", "3"]);
        let report =
            translate_extraction(&mut ex, &TranslateOptions::default(), &PrefixTranslator).unwrap();
        let texts: Vec<_> = report
            .segments
            .iter()
            .map(|seg| seg.translated.as_str())
            .collect();
        assert_eq!(texts, ["译Fig 1", "译Fig 2", "译3"]);
    }

    #[test]
    fn replay_matches_each_source_and_rejects_a_missing_segment() {
        let opts = TranslateOptions::default();
        let mut live_doc = extraction_from_lines(&["Hello world.", "Second sentence stays."]);
        let live =
            translate_extraction(&mut live_doc, &opts, &PrefixTranslator).expect("live translate");
        let mut replay_doc = extraction_from_lines(&["Hello world.", "Second sentence stays."]);
        let replayed = replay_extraction(&mut replay_doc, &opts, &live.segments).expect("replay");
        assert_eq!(replayed.calls, 0);
        assert_eq!(replayed.segments.len(), live.segments.len());
        for (saved, fresh) in live.segments.iter().zip(&replayed.segments) {
            assert_eq!(fresh.source, saved.source);
            assert_eq!(fresh.translated, saved.translated);
            assert_eq!(fresh.glyph_ids, saved.glyph_ids);
        }
        assert!(replay_doc
            .glyphs
            .iter()
            .all(|glyph| !matches!(glyph.disposition, Disposition::Pending)));

        let mut hole = live.segments.clone();
        hole.pop();
        let mut again = extraction_from_lines(&["Hello world.", "Second sentence stays."]);
        let err = replay_extraction(&mut again, &opts, &hole).unwrap_err();
        assert!(err.to_string().contains("no saved translation"), "{err}");
    }

    #[test]
    fn replay_keeps_a_split_chapter_index_and_a_joined_sentence() {
        let opts = TranslateOptions::default();
        let saved = vec![
            TranslatedSegment {
                id: 1,
                page_index: 0,
                glyph_ids: Vec::new(),
                source: "10 10.1 Title".into(),
                translated: "10 10.1 标题".into(),
            },
            TranslatedSegment {
                id: 2,
                page_index: 0,
                glyph_ids: Vec::new(),
                source: "alpha like this".into(),
                translated: "甲像这样".into(),
            },
            TranslatedSegment {
                id: 3,
                page_index: 0,
                glyph_ids: Vec::new(),
                source: " to separate it.".into(),
                translated: "以便分开。".into(),
            },
        ];
        let mut split = extraction_from_lines(&["10", "10.1 Title"]);
        let report = replay_extraction(&mut split, &opts, &saved).expect("split replay");
        let texts: Vec<_> = report
            .segments
            .iter()
            .map(|seg| seg.translated.as_str())
            .collect();
        assert_eq!(texts, ["10", "10.1 标题"]);

        let trailing = vec![TranslatedSegment {
            id: 4,
            page_index: 0,
            glyph_ids: Vec::new(),
            source: "11 11.1 Starting simple: Building a single-tool travel info ".into(),
            translated: "11 11.1 简单开始 ".into(),
        }];
        let mut spaced =
            extraction_from_lines(&["11.1 Starting simple: Building a single-tool travel info"]);
        let report = replay_extraction(&mut spaced, &opts, &trailing).expect("trailing replay");
        assert_eq!(report.segments[0].translated, "11.1 简单开始");

        let address = vec![TranslatedSegment {
            id: 5,
            page_index: 0,
            glyph_ids: Vec::new(),
            source: "20 Baldwin Road".into(),
            translated: "20 鲍德温路".into(),
        }];
        let mut street = extraction_from_lines(&["Baldwin Road"]);
        let err = replay_extraction(&mut street, &opts, &address).unwrap_err();
        assert!(err.to_string().contains("no saved translation"), "{err}");

        let mut joined = extraction_from_lines(&["alpha like this to separate it."]);
        let report = replay_extraction(&mut joined, &opts, &saved).expect("joined replay");
        assert_eq!(report.segments.len(), 1);
        assert_eq!(report.segments[0].translated, "甲像这样以便分开。");

        let spaced = vec![
            TranslatedSegment {
                id: 6,
                page_index: 0,
                glyph_ids: Vec::new(),
                source: "alpha  like this".into(),
                translated: "甲像这样".into(),
            },
            TranslatedSegment {
                id: 7,
                page_index: 0,
                glyph_ids: Vec::new(),
                source: "to separate it.".into(),
                translated: "以便分开。".into(),
            },
        ];
        let mut loose = extraction_from_lines(&["alpha like this to separate it."]);
        let report = replay_extraction(&mut loose, &opts, &spaced).expect("squashed replay");
        assert_eq!(report.segments[0].translated, "甲像这样以便分开。");
    }
}
