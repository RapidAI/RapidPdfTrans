//! Select a translator from `RPT_TRANSLATOR` or [`TranslateOptions::translator`].
//!
//! `maclaw` is the default. It is the OpenAI-compatible chat preset
//! (base `https://hub.mypapers.top/api/llm/v1`, model `auto`). The MaClawSrv
//! agent API at RapidAI/MaClaw speaks that same chat-completions wire; this
//! crate does not drive its instance/message agent loop.

use crate::error::{Error, Result};
use crate::translate::google::GoogleTranslator;
use crate::translate::llm::LlmTranslator;
use crate::translate::{TranslateOptions, Translator, DEFAULT_LLM_BASE_URL};

pub const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub const OPENAI_DEFAULT_MODEL: &str = "gpt-4o-mini";

pub enum TranslatorBackend {
    OpenAi(LlmTranslator),
    Google(GoogleTranslator),
}

impl TranslatorBackend {
    pub fn from_env(opts: &TranslateOptions) -> Result<Self> {
        let name = opts.resolved_translator().to_ascii_lowercase();
        match name.as_str() {
            "maclaw" | "llm" => Ok(Self::OpenAi(LlmTranslator::from_env(opts)?)),
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
                Ok(Self::OpenAi(LlmTranslator::from_env(&opts)?))
            }
            "google" | "google-v2" => Ok(Self::Google(google_v2(opts)?)),
            "google-v3" => Ok(Self::Google(google_v3(opts)?)),
            "google-unofficial" | "google-web" => Ok(Self::Google(GoogleTranslator::unofficial(
                &opts.source_lang,
                &opts.target_lang,
            ))),
            other => Err(Error::Translate(format!(
                "unknown translator `{other}`; expected maclaw, openai, google-v2, google-v3, or google-unofficial"
            ))),
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
    Ok(client)
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
            return Ok(client);
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
    Ok(client)
}
