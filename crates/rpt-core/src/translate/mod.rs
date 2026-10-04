//! Pluggable translation of extracted text.
//!
//! This does not rewrite the PDF. Each translated glyph is marked
//! `translated_pending_rewrite`, which is intentionally not a final coverage
//! state: rewriting the content stream is milestone M3.
//!
//! The default backend is the maclaw/LLM gateway
//! (`https://hub.mypapers.top/api/llm/v1`) with model `auto`. Override the
//! base URL with `RPT_LLM_BASE_URL` or [`TranslateOptions::base_url`], and the
//! model with `RPT_LLM_MODEL` or [`TranslateOptions::model`]. The API key is
//! read only from `RPT_LLM_API_KEY`.

mod llm;
mod prompt;
mod protect;
mod segment;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use llm::LlmTranslator;
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
            timeout_secs: 180,
            max_tokens: None,
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

    /// Parse translator fields from an options JSON object.
    /// `api_key` is ignored; a warning is returned when it is present.
    pub fn from_json(text: &str) -> Result<(Self, Vec<String>)> {
        if text.trim().is_empty() {
            return Ok((Self::default(), Vec::new()));
        }
        let value: Value = serde_json::from_str(text).map_err(|e| Error::Options(e.to_string()))?;
        let mut opts = Self::default();
        let mut warnings = Vec::new();
        if value.get("api_key").is_some() || value.get("apiKey").is_some() {
            warnings.push(
                "api_key in options JSON is ignored; set RPT_LLM_API_KEY in the environment".into(),
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
    let shielded: Vec<protect::Shielded> = segments
        .iter()
        .map(|seg| shield(&seg.text, &opts.glossary))
        .collect();
    let mut translated: Vec<Option<String>> = vec![None; segments.len()];
    let mut cache: HashMap<String, String> = HashMap::new();
    let mut calls = 0usize;
    let mut cache_hits = 0usize;

    let mut cursor = 0usize;
    while cursor < segments.len() {
        let mut batch: Vec<usize> = Vec::new();
        while cursor < segments.len() && batch.len() < opts.batch_size {
            let i = cursor;
            cursor += 1;
            if segments[i].text.trim().is_empty() {
                translated[i] = Some(segments[i].text.clone());
                continue;
            }
            if let Some(hit) = cache.get(&shielded[i].text) {
                translated[i] = Some(hit.clone());
                cache_hits += 1;
                continue;
            }
            if batch.iter().any(|&j| shielded[j].text == shielded[i].text) {
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
                if !seen.iter().any(|&j| shielded[j].text == shielded[i].text) {
                    seen.push(i);
                }
            }
            seen
        };
        let rendered =
            translate_batch(&segments, &shielded, &unique, opts, translator, &mut calls)?;
        for (i, text) in rendered {
            cache.insert(shielded[i].text.clone(), text.clone());
            translated[i] = Some(text);
        }
        for &i in &batch {
            if translated[i].is_none() {
                let text = cache.get(&shielded[i].text).cloned().ok_or_else(|| {
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
        for id in &seg.glyph_ids {
            extraction.mark_translated_pending(*id, text.clone())?;
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
        assert!(!format!("{opts:?}").contains("nope"));
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
}
