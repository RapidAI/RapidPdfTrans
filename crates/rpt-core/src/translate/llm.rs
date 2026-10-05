//! Generic OpenAI-compatible chat client.
//!
//! The maclaw preset is this client with base URL
//! `https://hub.mypapers.top/api/llm/v1` and model `auto`. The official
//! MaClawSrv REST API (RapidAI/MaClaw) is an agent control plane, not a
//! document translator. Its own connectivity check reports `protocol: openai`
//! and `wire_api: chat_completions`, so translation uses that wire.
//!
//! The API key is read only from `RPT_LLM_API_KEY`. It is never taken from
//! options JSON, and [`Debug`] prints it as `redacted`.

use std::time::Duration;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::translate::{TranslateOptions, Translator};

pub struct LlmTranslator {
    agent: ureq::Agent,
    base_url: String,
    model: String,
    api_key: String,
    temperature: f32,
    max_tokens: Option<u32>,
    timeout: Duration,
}

impl std::fmt::Debug for LlmTranslator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmTranslator")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &"redacted")
            .field("temperature", &self.temperature)
            .finish()
    }
}

impl LlmTranslator {
    pub fn from_env(opts: &TranslateOptions) -> Result<Self> {
        let key = std::env::var("RPT_LLM_API_KEY").map_err(|_| {
            Error::Translate(
                "RPT_LLM_API_KEY is not set; the API key is accepted only from that environment variable"
                    .into(),
            )
        })?;
        if key.trim().is_empty() {
            return Err(Error::Translate("RPT_LLM_API_KEY is empty".into()));
        }
        Self::with_key(opts, key)
    }

    pub fn with_key(opts: &TranslateOptions, api_key: impl Into<String>) -> Result<Self> {
        let api_key = api_key.into();
        let timeout = Duration::from_secs(opts.timeout_secs.max(1));
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .proxy(None)
            .build();
        let agent: ureq::Agent = config.into();
        Ok(Self {
            agent,
            base_url: opts.resolved_base_url(),
            model: opts.resolved_model(),
            api_key,
            temperature: opts.temperature,
            max_tokens: opts.max_tokens,
            timeout,
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl Translator for LlmTranslator {
    fn complete(&self, system: &str, user: &str) -> Result<String> {
        let mut delay = std::time::Duration::from_secs(2);
        let mut last = None;
        for attempt in 0..4 {
            match self.complete_once(system, user) {
                Ok(text) => return Ok(text),
                Err(err) if http_status_retryable(&err) && attempt < 3 => {
                    std::thread::sleep(delay);
                    delay *= 2;
                    last = Some(err);
                }
                Err(err) => return Err(err),
            }
        }
        Err(last.unwrap_or_else(|| Error::Translate("LLM request failed".into())))
    }
}

impl LlmTranslator {
    fn complete_once(&self, system: &str, user: &str) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut body = json!({
            "model": self.model,
            "temperature": self.temperature,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ]
        });
        if let Some(max_tokens) = self.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }
        // ureq's socket timeout does not always interrupt a stalled TLS read.
        // A thread deadline makes `timeout: global` real so a batch can split.
        let agent = self.agent.clone();
        let api_key = self.api_key.clone();
        let timeout = self.timeout;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(post_chat(&agent, &url, &api_key, body));
        });
        match rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                Err(Error::Translate("timeout: global".into()))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(Error::Translate("LLM request failed".into()))
            }
        }
    }
}

fn post_chat(agent: &ureq::Agent, url: &str, api_key: &str, body: Value) -> Result<String> {
    let response = agent
        .post(url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send_json(&body)
        .map_err(|err| http_error(err, api_key))?;
    let parsed: Value = response
        .into_body()
        .read_json()
        .map_err(|err| Error::Translate(scrub_secrets(&err.to_string(), api_key)))?;
    parsed
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            Error::Translate(
                "LLM response had empty message.content (reasoning tokens are ignored)".into(),
            )
        })
}

fn http_error(err: ureq::Error, api_key: &str) -> Error {
    let msg = match err {
        ureq::Error::StatusCode(code) => format!("LLM HTTP status {code}"),
        other => other.to_string(),
    };
    Error::Translate(scrub_secrets(&msg, api_key))
}

fn http_status_retryable(err: &Error) -> bool {
    let msg = err.to_string();
    ["500", "502", "503", "429"]
        .iter()
        .any(|code| msg.contains(&format!("HTTP status {code}")))
}

pub fn scrub_secrets(message: &str, secret: &str) -> String {
    let mut out = if secret.is_empty() {
        message.to_string()
    } else {
        message.replace(secret, "[redacted]")
    };
    let mut replaced = String::new();
    let mut rest = out.as_str();
    while let Some(idx) = rest.find("Bearer ") {
        replaced.push_str(&rest[..idx]);
        replaced.push_str("Bearer [redacted]");
        let after = &rest[idx + "Bearer ".len()..];
        let skip = after
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(after.len());
        rest = &after[skip..];
    }
    replaced.push_str(rest);
    if !replaced.is_empty() {
        out = replaced;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translate::TranslateOptions;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    fn read_request(stream: &mut std::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = stream.read(&mut tmp).unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(header_end) = find_subsequence(&buf, b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&buf[..header_end]);
                let length = content_length(&headers);
                if buf.len() >= header_end + 4 + length {
                    break;
                }
            }
            if buf.len() > 2_000_000 {
                break;
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    fn find_subsequence(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }

    fn content_length(headers: &str) -> usize {
        headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.eq_ignore_ascii_case("content-length") {
                    value.trim().parse().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0)
    }

    struct Mock {
        body: Arc<Mutex<String>>,
        base: String,
    }

    impl Mock {
        fn start(status: u16, response_body: String) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let body = Arc::new(Mutex::new(String::new()));
            let captured = body.clone();
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let req = read_request(&mut stream);
                *captured.lock().unwrap() = req.clone();
                let resp = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                    response_body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            });
            Self {
                body,
                base: format!("http://127.0.0.1:{port}/v1"),
            }
        }
    }

    #[test]
    fn posts_model_auto_and_reads_message_content_only() {
        let response = r#"{"choices":[{"message":{"reasoning_content":"hidden","content":"{\"translations\":[{\"id\":0,\"text\":\"pong\"}]}"}}],"completion_thinking_tokens":3}"#;
        let mock = Mock::start(200, response.into());
        let opts = TranslateOptions {
            base_url: Some(mock.base.clone()),
            ..TranslateOptions::default()
        };
        let client = LlmTranslator::with_key(&opts, "test-key-not-real").unwrap();
        assert_eq!(client.model(), "auto");
        let text = client.complete("sys", "user").unwrap();
        assert!(text.contains("pong"));
        assert!(!text.contains("hidden"));
        let captured = mock.body.lock().unwrap().clone();
        let json_start = captured.find("\r\n\r\n").unwrap() + 4;
        let posted: Value = serde_json::from_str(&captured[json_start..]).unwrap();
        assert_eq!(posted["model"], "auto");
        assert_eq!(posted["temperature"], 0.0);
        assert!(posted.get("max_tokens").is_none());
        let lower = captured.to_ascii_lowercase();
        assert!(
            lower.contains("authorization: bearer test-key-not-real"),
            "request headers: {}",
            captured.lines().take(12).collect::<Vec<_>>().join(" | ")
        );
        assert!(captured.contains("/v1/chat/completions"));
        let debug = format!("{client:?}");
        assert!(debug.contains("redacted"));
        assert!(!debug.contains("test-key-not-real"));
    }

    #[test]
    fn explicit_model_overrides_the_default() {
        let response = r#"{"choices":[{"message":{"content":"ok"}}]}"#;
        let mock = Mock::start(200, response.into());
        let opts = TranslateOptions {
            base_url: Some(mock.base),
            model: Some("official-high".into()),
            ..TranslateOptions::default()
        };
        let client = LlmTranslator::with_key(&opts, "k").unwrap();
        let _ = client.complete("s", "u").unwrap();
        let captured = mock.body.lock().unwrap().clone();
        let json_start = captured.find("\r\n\r\n").unwrap() + 4;
        let posted: Value = serde_json::from_str(&captured[json_start..]).unwrap();
        assert_eq!(posted["model"], "official-high");
    }

    #[test]
    fn empty_content_is_an_error() {
        let response =
            r#"{"choices":[{"message":{"content":"","reasoning_content":"only thinking"}}]}"#;
        let mock = Mock::start(200, response.into());
        let opts = TranslateOptions {
            base_url: Some(mock.base),
            ..TranslateOptions::default()
        };
        let client = LlmTranslator::with_key(&opts, "k").unwrap();
        let err = client.complete("s", "u").unwrap_err();
        assert!(err.to_string().contains("empty message.content"));
    }

    #[test]
    fn scrub_removes_bearer_and_secret() {
        let msg = "failed Bearer sk-secret more sk-secret";
        let cleaned = scrub_secrets(msg, "sk-secret");
        assert!(!cleaned.contains("sk-secret"));
        assert!(cleaned.contains("Bearer [redacted]"));
    }
}
