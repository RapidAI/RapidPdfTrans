//! Select a translator from `RPT_TRANSLATOR` or [`TranslateOptions::translator`].
//!
//! An explicit name wins. A configured LLM (`RPT_LLM_API_KEY`,
//! `RPT_LLM_BASE_URL`, `RPT_LLM_MODEL`, or a model/base URL option) is next.
//! Otherwise an official Google key or credential is used. With none of those
//! set, a short probe of the unofficial Google endpoint selects it when the
//! probe succeeds.
//!
//! `maclaw` is the OpenAI-compatible chat preset (base
//! `https://hub.mypapers.top/api/llm/v1`, model `auto`). The MaClawSrv agent
//! API at RapidAI/MaClaw speaks that same chat-completions wire; this crate
//! does not drive its instance/message agent loop.

use crate::error::{Error, Result};
use crate::translate::google::GoogleTranslator;
use crate::translate::llm::LlmTranslator;
use crate::translate::{TranslateOptions, Translator, DEFAULT_LLM_BASE_URL};

pub const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub const OPENAI_DEFAULT_MODEL: &str = "gpt-4o-mini";

#[allow(clippy::large_enum_variant)]
pub enum TranslatorBackend {
    OpenAi(LlmTranslator),
    Google(GoogleTranslator),
}

impl TranslatorBackend {
    /// Explicit translator, then a configured LLM, then Google (official key
    /// or a successful unofficial probe), then the missing-key error.
    pub fn from_env(opts: &TranslateOptions) -> Result<Self> {
        match choose(
            explicit_translator(opts).as_deref(),
            llm_configured(opts),
            google_v2_configured(),
            google_v3_configured(),
        ) {
            BackendChoice::Named(name) => named(opts, &name),
            BackendChoice::Llm => Ok(Self::OpenAi(LlmTranslator::from_env(opts)?)),
            BackendChoice::GoogleV2 => Ok(Self::Google(google_v2(opts)?)),
            BackendChoice::GoogleV3 => Ok(Self::Google(google_v3(opts)?)),
            BackendChoice::Probe => {
                if let Some(client) =
                    GoogleTranslator::probe_unofficial(&opts.source_lang, &opts.target_lang)
                {
                    Ok(Self::Google(with_timeout(client, opts)))
                } else {
                    Err(Error::Translate(
                        "no translator is configured and Google Translate is unreachable; set RPT_LLM_API_KEY or RPT_TRANSLATOR"
                            .into(),
                    ))
                }
            }
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::OpenAi(client) => {
                if client.base_url().trim_end_matches('/') == DEFAULT_LLM_BASE_URL {
                    "maclaw"
                } else {
                    "openai"
                }
            }
            Self::Google(client) => client.label(),
        }
    }

    pub fn model(&self) -> &str {
        match self {
            Self::OpenAi(client) => client.model(),
            Self::Google(client) => client.label(),
        }
    }

    pub fn endpoint(&self) -> &str {
        match self {
            Self::OpenAi(client) => client.base_url(),
            Self::Google(client) => client.endpoint(),
        }
    }
}

impl Translator for TranslatorBackend {
    fn complete(&self, system: &str, user: &str) -> Result<String> {
        match self {
            Self::OpenAi(client) => client.complete(system, user),
            Self::Google(client) => client.complete(system, user),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum BackendChoice {
    Named(String),
    Llm,
    GoogleV2,
    GoogleV3,
    Probe,
}

/// Selection order: explicit name, configured LLM, official Google, probe.
fn choose(explicit: Option<&str>, llm: bool, google_key: bool, google_v3: bool) -> BackendChoice {
    if let Some(name) = explicit.map(str::trim).filter(|name| !name.is_empty()) {
        return BackendChoice::Named(name.to_ascii_lowercase());
    }
    if llm {
        return BackendChoice::Llm;
    }
    if google_key {
        return BackendChoice::GoogleV2;
    }
    if google_v3 {
        return BackendChoice::GoogleV3;
    }
    BackendChoice::Probe
}

fn explicit_translator(opts: &TranslateOptions) -> Option<String> {
    if let Some(name) = opts
        .translator
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        return Some(name.to_string());
    }
    std::env::var("RPT_TRANSLATOR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_set(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn llm_configured(opts: &TranslateOptions) -> bool {
    env_set("RPT_LLM_API_KEY")
        || env_set("RPT_LLM_BASE_URL")
        || env_set("RPT_LLM_MODEL")
        || opts
            .model
            .as_deref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
        || opts
            .base_url
            .as_deref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
}

fn google_v2_configured() -> bool {
    env_set("RPT_GOOGLE_API_KEY")
}

fn google_v3_configured() -> bool {
    env_set("RPT_GOOGLE_ACCESS_TOKEN")
        || env_set("RPT_GOOGLE_CREDENTIALS")
        || env_set("RPT_GOOGLE_CREDENTIALS_JSON")
}

fn named(opts: &TranslateOptions, name: &str) -> Result<TranslatorBackend> {
    match name {
        "maclaw" | "llm" => Ok(TranslatorBackend::OpenAi(LlmTranslator::from_env(opts)?)),
        "openai" => {
            let mut opts = opts.clone();
            if opts.base_url.as_deref().map(str::trim).unwrap_or("").is_empty()
                && std::env::var("RPT_LLM_BASE_URL")
                    .ok()
                    .map(|v| v.trim().is_empty())
                    .unwrap_or(true)
            {
                opts.base_url = Some(OPENAI_DEFAULT_BASE_URL.into());
            }
            if opts.model.as_deref().map(str::trim).unwrap_or("").is_empty()
                && std::env::var("RPT_LLM_MODEL")
                    .ok()
                    .map(|v| v.trim().is_empty())
                    .unwrap_or(true)
            {
                opts.model = Some(OPENAI_DEFAULT_MODEL.into());
            }
            Ok(TranslatorBackend::OpenAi(LlmTranslator::from_env(&opts)?))
        }
        "google" | "google-v2" => Ok(TranslatorBackend::Google(google_v2(opts)?)),
        "google-v3" => Ok(TranslatorBackend::Google(google_v3(opts)?)),
        "google-unofficial" | "google-web" => Ok(TranslatorBackend::Google(with_timeout(
            GoogleTranslator::unofficial(&opts.source_lang, &opts.target_lang),
            opts,
        ))),
        other => Err(Error::Translate(format!(
            "unknown translator `{other}`; expected maclaw, openai, google-v2, google-v3, or google-unofficial"
        ))),
    }
}

fn google_v2(opts: &TranslateOptions) -> Result<GoogleTranslator> {
    let key = std::env::var("RPT_GOOGLE_API_KEY").map_err(|_| {
        Error::Translate(
            "RPT_GOOGLE_API_KEY is not set; the Google API key is accepted only from that environment variable"
                .into(),
        )
    })?;
    let mut client = GoogleTranslator::v2(&opts.source_lang, &opts.target_lang, key)?;
    if let Ok(base) = std::env::var("RPT_GOOGLE_BASE_URL") {
        if !base.trim().is_empty() {
            client = client.with_base_url(base);
        }
    }
    Ok(with_timeout(client, opts))
}

fn with_timeout(client: GoogleTranslator, opts: &TranslateOptions) -> GoogleTranslator {
    client.with_timeout(std::time::Duration::from_secs(opts.timeout_secs.max(1)))
}

fn google_v3(opts: &TranslateOptions) -> Result<GoogleTranslator> {
    let project = std::env::var("RPT_GOOGLE_PROJECT").unwrap_or_default();
    let location = std::env::var("RPT_GOOGLE_LOCATION").unwrap_or_else(|_| "global".into());
    if let Ok(token) = std::env::var("RPT_GOOGLE_ACCESS_TOKEN") {
        if !token.trim().is_empty() {
            let mut client = GoogleTranslator::v3_access_token(
                &opts.source_lang,
                &opts.target_lang,
                project,
                location,
                token,
            )?;
            if let Ok(base) = std::env::var("RPT_GOOGLE_BASE_URL") {
                if !base.trim().is_empty() {
                    client = client.with_base_url(base);
                }
            }
            return Ok(with_timeout(client, opts));
        }
    }
    let json = if let Ok(raw) = std::env::var("RPT_GOOGLE_CREDENTIALS_JSON") {
        if !raw.trim().is_empty() {
            raw
        } else {
            String::new()
        }
    } else if let Ok(path) = std::env::var("RPT_GOOGLE_CREDENTIALS") {
        if path.trim().is_empty() {
            String::new()
        } else {
            std::fs::read_to_string(&path)
                .map_err(|err| Error::Translate(format!("read RPT_GOOGLE_CREDENTIALS: {err}")))?
        }
    } else {
        String::new()
    };
    if json.is_empty() {
        return Err(Error::Translate(
            "Google Translation v3 needs RPT_GOOGLE_ACCESS_TOKEN or RPT_GOOGLE_CREDENTIALS".into(),
        ));
    }
    let mut client = GoogleTranslator::v3_service_account(
        &opts.source_lang,
        &opts.target_lang,
        project,
        location,
        &json,
    )?;
    if let Ok(base) = std::env::var("RPT_GOOGLE_BASE_URL") {
        if !base.trim().is_empty() {
            client = client.with_base_url(base);
        }
    }
    Ok(with_timeout(client, opts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_order_is_explicit_then_llm_then_google_then_probe() {
        assert_eq!(
            choose(Some("maclaw"), true, true, true),
            BackendChoice::Named("maclaw".into())
        );
        assert_eq!(
            choose(Some(" Google-Unofficial "), false, false, false),
            BackendChoice::Named("google-unofficial".into())
        );
        assert_eq!(choose(None, true, true, true), BackendChoice::Llm);
        assert_eq!(choose(None, false, true, true), BackendChoice::GoogleV2);
        assert_eq!(choose(None, false, false, true), BackendChoice::GoogleV3);
        assert_eq!(choose(None, false, false, false), BackendChoice::Probe);
        assert_eq!(
            choose(Some("  "), false, false, false),
            BackendChoice::Probe
        );
    }

    #[test]
    fn an_explicit_unofficial_translator_does_not_need_a_key() {
        let opts = TranslateOptions {
            translator: Some("google-unofficial".into()),
            ..TranslateOptions::default()
        };
        let client = TranslatorBackend::from_env(&opts).unwrap();
        assert_eq!(client.label(), "google-unofficial");
        assert!(client.endpoint().contains("translate.googleapis.com"));
    }
}
