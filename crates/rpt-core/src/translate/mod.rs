//! Pluggable translation of extracted text.
//!
//! Translation marks glyphs `translated_pending_rewrite`. [`crate::rewrite`]
//! is what writes those strings back into the PDF.
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
mod segment;

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use backend::TranslatorBackend;
pub use google::GoogleTranslator;
pub use llm::LlmTranslator;
pub use protect::citation_end;
pub use references::{identical_reference_operators, reference_glyph_ids, reference_stream_spans};
pub use segment::Segment;

use crate::error::{Error, Result};
use crate::extract::Extraction;
use prompt::{parse_translations, system_prompt, user_payload, PromptSegment};
use protect::{restore, shield};

pub const DEFAULT_LLM_BASE_URL: &str = "https://hub.mypapers.top/api/llm/v1";
pub const DEFAULT_LLM_MODEL: &str = "auto";

/// Something that turns a system prompt and a user payload into model text.
pub trait Translator {
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
    pub temperature: f32,
    pub timeout_secs: u64,
    /// Sent only when set. Left unset so reasoning models can still emit content.
    pub max_tokens: Option<u32>,
    /// `maclaw`, `openai`, `google`, `google-v2`, `google-v3`, or `google-unofficial`.
    /// Empty falls through to `RPT_TRANSLATOR`, then `maclaw`.
    pub translator: Option<String>,
    /// Leave References / Bibliography as original text. Default is on.
    pub skip_references: bool,
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
            temperature: 0.0,
            timeout_secs: 300,
            max_tokens: None,
            translator: None,
            skip_references: true,
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
        let layout = string_field(&value, &["bilingual_layout", "layout"]);
        if let Some(mode) = string_field(&value, &["output_mode", "mode"]) {
            opts.output_mode = OutputMode::parse(&mode, layout.as_deref())?;
        } else if value.get("bilingual").and_then(|v| v.as_bool()) == Some(true) {
            opts.output_mode = OutputMode::parse("bilingual", layout.as_deref())?;
        } else if let Some(layout) = layout {
            opts.output_mode = OutputMode::parse("bilingual", Some(&layout))?;
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
    if opts.batch_size == 0 {
        return Err(Error::Options("batch_size must be at least 1".into()));
    }
    let segments = segment::segment_glyphs(&extraction.glyphs);
    let reference_ids = if opts.skip_references {
        references::reference_glyph_ids(&extraction.glyphs)
    } else {
        HashSet::new()
    };
    for id in &reference_ids {
        extraction.mark_kept(*id, "references")?;
    }
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

    let mut cursor = 0usize;
    while cursor < segments.len() {
        let mut batch: Vec<usize> = Vec::new();
        while cursor < segments.len() && batch.len() < opts.batch_size {
            let i = cursor;
            cursor += 1;
            if translated[i].is_some() {
                continue;
            }
            if segments[i].text.trim().is_empty() {
                translated[i] = Some(segments[i].text.clone());
                continue;
            }
            if let Some(hit) = cache.get(&segments[i].text) {
                translated[i] = Some(hit.clone());
                cache_hits += 1;
                continue;
            }
            if batch.iter().any(|&j| segments[j].text == segments[i].text) {
                batch.push(i);
                continue;
            }
            batch.push(i);
        }
        if batch.is_empty() {
            continue;
        }
        let unique: Vec<usize> = {
            let mut seen: Vec<usize> = Vec::new();
            for &i in &batch {
                if !seen.iter().any(|&j| segments[j].text == segments[i].text) {
                    seen.push(i);
                }
            }
            seen
        };
        let rendered =
            translate_batch(&segments, &shielded, &unique, opts, translator, &mut calls)?;
        for (i, text) in rendered {
            cache.insert(segments[i].text.clone(), text.clone());
            translated[i] = Some(text);
        }
        for &i in &batch {
            if translated[i].is_none() {
                let text = cache.get(&segments[i].text).cloned().ok_or_else(|| {
                    Error::Translate(format!("no translation for segment {}", segments[i].id))
                })?;
                cache_hits += 1;
                translated[i] = Some(text);
            }
        }
    }

    let mut report_segments = Vec::with_capacity(segments.len());
    for (i, seg) in segments.iter().enumerate() {
        let text = translated[i]
            .clone()
            .ok_or_else(|| Error::Translate(format!("segment {} was not translated", seg.id)))?;
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

fn translate_batch(
    segments: &[Segment],
    shielded: &[protect::Shielded],
    indexes: &[usize],
    opts: &TranslateOptions,
    translator: &dyn Translator,
    calls: &mut usize,
) -> Result<Vec<(usize, String)>> {
    match translate_batch_once(segments, shielded, indexes, opts, translator, calls) {
        Ok(restored) => Ok(restored),
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
            translate_batch_once(segments, shielded, indexes, opts, translator, calls)
        }
        Err(err) => Err(err),
    }
}

fn recoverable(err: &Error) -> bool {
    let msg = err.to_string().to_ascii_lowercase();
    msg.contains("timeout")
        || msg.contains("omitted segment")
        || msg.contains("invalid translation json")
        || msg.contains("did not contain a json")
}

fn translate_batch_once(
    segments: &[Segment],
    shielded: &[protect::Shielded],
    indexes: &[usize],
    opts: &TranslateOptions,
    translator: &dyn Translator,
    calls: &mut usize,
) -> Result<Vec<(usize, String)>> {
    let first = call_batch(segments, shielded, indexes, opts, translator, calls, false)?;
    match restore_all(indexes, shielded, &first) {
        Ok(restored) => Ok(restored),
        Err(_) => {
            let second = call_batch(segments, shielded, indexes, opts, translator, calls, true)?;
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
) -> Result<Vec<(u32, String)>> {
    let prompts = indexes
        .iter()
        .map(|&i| prompt_for(segments, shielded, i, opts.context_window))
        .collect::<Vec<_>>();
    let user = user_payload(&prompts)?;
    let system = system_prompt(&opts.source_lang, &opts.target_lang, strict);
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
        assert!(!format!("{opts:?}").contains("nope"));
        let (opts, _) = TranslateOptions::from_json(r#"{"skip_references":false}"#).unwrap();
        assert!(!opts.skip_references);
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
    }

    #[test]
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
}
