//! Google Cloud Translation API v2 and v3, plus an opt-in unofficial web endpoint.
//!
//! `google-unofficial` calls `translate.googleapis.com/translate_a/single`.
//! That endpoint is not a documented Google API. It is never selected unless
//! `RPT_TRANSLATOR=google-unofficial`.
//!
//! Keys and tokens are read only from the environment or from constructors
//! used by tests. [`Debug`] redacts them.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::Serialize;
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::translate::llm::scrub_secrets;
use crate::translate::Translator;

const V2_DEFAULT_BASE: &str = "https://translation.googleapis.com";
const V3_SCOPE: &str = "https://www.googleapis.com/auth/cloud-translation";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GoogleApi {
    /// Official Cloud Translation API v2 (`?key=`).
    V2,
    /// Official Cloud Translation API v3 (`projects/*/locations/*:translateText`).
    V3,
    /// Undocumented `translate_a/single?client=gtx` endpoint.
    Unofficial,
}

struct ServiceAccount {
    client_email: String,
    private_key_pem: String,
    token_uri: String,
}

pub struct GoogleTranslator {
    agent: ureq::Agent,
    api: GoogleApi,
    source: String,
    target: String,
    /// v2 key, or empty.
    api_key: String,
    project: String,
    location: String,
    /// Pre-minted bearer token. Empty when v3 should mint one.
    access_token: String,
    account: Option<ServiceAccount>,
    /// Override of the translation host, for tests.
    base_url: String,
    /// Cached minted token and expiry unix seconds.
    minted: std::sync::Mutex<Option<(String, u64)>>,
}

impl std::fmt::Debug for GoogleTranslator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleTranslator")
            .field("api", &self.api)
            .field("source", &self.source)
            .field("target", &self.target)
            .field("project", &self.project)
            .field("api_key", &redact_flag(!self.api_key.is_empty()))
            .field("access_token", &redact_flag(!self.access_token.is_empty()))
            .field("service_account", &redact_flag(self.account.is_some()))
            .finish()
    }
}

fn redact_flag(present: bool) -> &'static str {
    if present {
        "redacted"
    } else {
        "absent"
    }
}

impl GoogleTranslator {
    pub fn v2(
        source: impl Into<String>,
        target: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Self> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(Error::Translate(
                "Google Translation v2 requires RPT_GOOGLE_API_KEY".into(),
            ));
        }
        Ok(Self::build(
            GoogleApi::V2,
            source.into(),
            target.into(),
            api_key,
            String::new(),
            "global".into(),
            String::new(),
            None,
            V2_DEFAULT_BASE.into(),
        ))
    }

    pub fn v3_access_token(
        source: impl Into<String>,
        target: impl Into<String>,
        project: impl Into<String>,
        location: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Result<Self> {
        let project = project.into();
        let access_token = access_token.into();
        if project.trim().is_empty() {
            return Err(Error::Translate(
                "Google Translation v3 requires RPT_GOOGLE_PROJECT".into(),
            ));
        }
        if access_token.trim().is_empty() {
            return Err(Error::Translate(
                "Google Translation v3 access token is empty".into(),
            ));
        }
        let location = {
            let location = location.into();
            if location.trim().is_empty() {
                "global".into()
            } else {
                location
            }
        };
        Ok(Self::build(
            GoogleApi::V3,
            source.into(),
            target.into(),
            String::new(),
            project,
            location,
            access_token,
            None,
            V2_DEFAULT_BASE.into(),
        ))
    }

    pub fn v3_service_account(
        source: impl Into<String>,
        target: impl Into<String>,
        project: impl Into<String>,
        location: impl Into<String>,
        credentials_json: &str,
    ) -> Result<Self> {
        let value: Value = serde_json::from_str(credentials_json)
            .map_err(|err| Error::Translate(format!("service account JSON: {err}")))?;
        let client_email = value
            .get("client_email")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let private_key_pem = value
            .get("private_key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if client_email.is_empty() || private_key_pem.is_empty() {
            return Err(Error::Translate(
                "service account JSON needs client_email and private_key".into(),
            ));
        }
        let mut project = project.into();
        if project.trim().is_empty() {
            project = value
                .get("project_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
        }
        if project.trim().is_empty() {
            return Err(Error::Translate(
                "Google Translation v3 requires a project id".into(),
            ));
        }
        let token_uri = value
            .get("token_uri")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(TOKEN_URL)
            .to_string();
        let location = {
            let location = location.into();
            if location.trim().is_empty() {
                "global".into()
            } else {
                location
            }
        };
        Ok(Self::build(
            GoogleApi::V3,
            source.into(),
            target.into(),
            String::new(),
            project,
            location,
            String::new(),
            Some(ServiceAccount {
                client_email,
                private_key_pem,
                token_uri,
            }),
            V2_DEFAULT_BASE.into(),
        ))
    }

    /// Opt-in unofficial web endpoint. Not a Google Cloud API.
    pub fn unofficial(source: impl Into<String>, target: impl Into<String>) -> Self {
        Self::build(
            GoogleApi::Unofficial,
            source.into(),
            target.into(),
            String::new(),
            String::new(),
            "global".into(),
            String::new(),
            None,
            "https://translate.googleapis.com".into(),
        )
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn label(&self) -> &'static str {
        match self.api {
            GoogleApi::V2 => "google-v2",
            GoogleApi::V3 => "google-v3",
            GoogleApi::Unofficial => "google-unofficial",
        }
    }

    pub fn endpoint(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }

    fn build(
        api: GoogleApi,
        source: String,
        target: String,
        api_key: String,
        project: String,
        location: String,
        access_token: String,
        account: Option<ServiceAccount>,
        base_url: String,
    ) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .proxy(None)
            .build();
        Self {
            agent: config.into(),
            api,
            source,
            target,
            api_key,
            project,
            location,
            access_token,
            account,
            base_url,
            minted: std::sync::Mutex::new(None),
        }
    }

    fn translate_many(&self, texts: &[String]) -> Result<Vec<String>> {
        match self.api {
            GoogleApi::V2 => self.translate_v2(texts),
            GoogleApi::V3 => self.translate_v3(texts),
            GoogleApi::Unofficial => texts
                .iter()
                .map(|text| self.translate_unofficial(text))
                .collect(),
        }
    }

    fn translate_v2(&self, texts: &[String]) -> Result<Vec<String>> {
        let url = format!(
            "{}/language/translate/v2?key={}",
            self.base_url.trim_end_matches('/'),
            percent_encode(&self.api_key)
        );
        let protected: Vec<String> = texts.iter().map(|text| protect_html(text)).collect();
        let body = json!({
            "q": protected,
            "source": self.source,
            "target": self.target,
            "format": "html",
        });
        let response = self
            .agent
            .post(&url)
            .header("Content-Type", "application/json")
            .send_json(&body)
            .map_err(|err| self.http_error(err))?;
        let parsed: Value = response
            .into_body()
            .read_json()
            .map_err(|err| Error::Translate(self.scrub(&err.to_string())))?;
        let items = parsed
            .pointer("/data/translations")
            .and_then(|v| v.as_array())
            .ok_or_else(|| Error::Translate("Google v2 response has no translations".into()))?;
        if items.len() != texts.len() {
            return Err(Error::Translate(format!(
                "Google v2 returned {} translations for {} inputs",
                items.len(),
                texts.len()
            )));
        }
        items
            .iter()
            .map(|item| {
                let text = item
                    .get("translatedText")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        Error::Translate("Google v2 item missing translatedText".into())
                    })?;
                Ok(cleanup_html(text))
            })
            .collect()
    }

    fn translate_v3(&self, texts: &[String]) -> Result<Vec<String>> {
        let token = self.bearer()?;
        let url = format!(
            "{}/v3/projects/{}/locations/{}:translateText",
            self.base_url.trim_end_matches('/'),
            self.project,
            self.location
        );
        let protected: Vec<String> = texts.iter().map(|text| protect_html(text)).collect();
        let body = json!({
            "contents": protected,
            "mimeType": "text/html",
            "sourceLanguageCode": self.source,
            "targetLanguageCode": self.target,
        });
        let response = self
            .agent
            .post(&url)
            .header("Authorization", &format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .send_json(&body)
            .map_err(|err| self.http_error(err))?;
        let parsed: Value = response
            .into_body()
            .read_json()
            .map_err(|err| Error::Translate(self.scrub(&err.to_string())))?;
        let items = parsed
            .get("translations")
            .and_then(|v| v.as_array())
            .ok_or_else(|| Error::Translate("Google v3 response has no translations".into()))?;
        if items.len() != texts.len() {
            return Err(Error::Translate(format!(
                "Google v3 returned {} translations for {} inputs",
                items.len(),
                texts.len()
            )));
        }
        items
            .iter()
            .map(|item| {
                let text = item
                    .get("translatedText")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        Error::Translate("Google v3 item missing translatedText".into())
                    })?;
                Ok(cleanup_html(text))
            })
            .collect()
    }

    fn translate_unofficial(&self, text: &str) -> Result<String> {
        let url = format!(
            "{}/translate_a/single?client=gtx&sl={}&tl={}&dt=t&q={}",
            self.base_url.trim_end_matches('/'),
            percent_encode(&self.source),
            percent_encode(&self.target),
            percent_encode(text)
        );
        let response = self
            .agent
            .get(&url)
            .header("User-Agent", "RapidPdfTrans")
            .call()
            .map_err(|err| self.http_error(err))?;
        let parsed: Value = response
            .into_body()
            .read_json()
            .map_err(|err| Error::Translate(self.scrub(&err.to_string())))?;
        let rows = parsed
            .as_array()
            .and_then(|outer| outer.first())
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                Error::Translate("unofficial Google response did not contain a translation".into())
            })?;
        let mut out = String::new();
        for row in rows {
            if let Some(piece) = row
                .as_array()
                .and_then(|cols| cols.first())
                .and_then(|v| v.as_str())
            {
                out.push_str(piece);
            }
        }
        if out.is_empty() {
            return Err(Error::Translate(
                "unofficial Google response had empty translation text".into(),
            ));
        }
        Ok(out)
    }

    fn bearer(&self) -> Result<String> {
        if !self.access_token.is_empty() {
            return Ok(self.access_token.clone());
        }
        let Some(account) = &self.account else {
            return Err(Error::Translate(
                "Google Translation v3 needs RPT_GOOGLE_ACCESS_TOKEN or RPT_GOOGLE_CREDENTIALS"
                    .into(),
            ));
        };
        let now = unix_now();
        if let Ok(guard) = self.minted.lock() {
            if let Some((token, exp)) = guard.as_ref() {
                if *exp > now + 60 {
                    return Ok(token.clone());
                }
            }
        }
        let assertion = sign_assertion(account, now)?;
        let form = format!(
            "grant_type={}&assertion={}",
            percent_encode("urn:ietf:params:oauth:grant-type:jwt-bearer"),
            percent_encode(&assertion)
        );
        let response = self
            .agent
            .post(&account.token_uri)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send(form)
            .map_err(|err| self.http_error(err))?;
        let parsed: Value = response
            .into_body()
            .read_json()
            .map_err(|err| Error::Translate(self.scrub(&err.to_string())))?;
        let token = parsed
            .get("access_token")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::Translate("Google token response had no access_token".into()))?
            .to_string();
        let expires_in = parsed
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .unwrap_or(3600);
        if let Ok(mut guard) = self.minted.lock() {
            *guard = Some((token.clone(), now + expires_in));
        }
        Ok(token)
    }

    fn http_error(&self, err: ureq::Error) -> Error {
        let msg = match err {
            ureq::Error::StatusCode(code) => format!("Google HTTP status {code}"),
            other => other.to_string(),
        };
        Error::Translate(self.scrub(&msg))
    }

    fn scrub(&self, message: &str) -> String {
        let mut out = scrub_secrets(message, &self.api_key);
        out = scrub_secrets(&out, &self.access_token);
        if let Some(account) = &self.account {
            out = scrub_secrets(&out, &account.private_key_pem);
        }
        out
    }
}

impl Translator for GoogleTranslator {
    fn complete(&self, _system: &str, user: &str) -> Result<String> {
        let value: Value = serde_json::from_str(user).map_err(|_| {
            Error::Translate("Google backend expected the segment JSON payload".into())
        })?;
        let segments = value
            .get("segments")
            .and_then(|v| v.as_array())
            .ok_or_else(|| Error::Translate("Google backend payload has no segments".into()))?;
        let mut ids = Vec::with_capacity(segments.len());
        let mut texts = Vec::with_capacity(segments.len());
        for segment in segments {
            let id = segment
                .get("id")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| Error::Translate("segment missing id".into()))?;
            let text = segment
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Translate(format!("segment {id} missing text")))?;
            ids.push(id as u32);
            texts.push(text.to_string());
        }
        let translated = self.translate_many(&texts)?;
        let items: Vec<Value> = ids
            .iter()
            .zip(translated)
            .map(|(id, text)| json!({"id": id, "text": text}))
            .collect();
        Ok(json!({"translations": items}).to_string())
    }
}

#[derive(Serialize)]
struct AssertionClaims {
    iss: String,
    scope: String,
    aud: String,
    iat: u64,
    exp: u64,
}

fn sign_assertion(account: &ServiceAccount, now: u64) -> Result<String> {
    let claims = AssertionClaims {
        iss: account.client_email.clone(),
        scope: V3_SCOPE.into(),
        aud: account.token_uri.clone(),
        iat: now,
        exp: now + 3600,
    };
    let key = EncodingKey::from_rsa_pem(account.private_key_pem.as_bytes())
        .map_err(|err| Error::Translate(format!("service account key: {err}")))?;
    encode(&Header::new(Algorithm::RS256), &claims, &key)
        .map_err(|err| Error::Translate(format!("service account assertion: {err}")))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn protect_html(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('⟦') {
        out.push_str(&escape_html(&rest[..start]));
        let after = &rest[start..];
        if let Some(rel_end) = after.find('⟧') {
            let token_end = rel_end + '⟧'.len_utf8();
            let token = &after[..token_end];
            if is_placeholder(token) {
                out.push_str("<span translate=\"no\">");
                out.push_str(token);
                out.push_str("</span>");
                rest = &after[rel_end + '⟧'.len_utf8()..];
                continue;
            }
        }
        out.push_str(&escape_html("⟦"));
        rest = &rest[start + '⟦'.len_utf8()..];
    }
    out.push_str(&escape_html(rest));
    out
}

fn is_placeholder(token: &str) -> bool {
    let Some(inner) = token.strip_prefix('⟦').and_then(|s| s.strip_suffix('⟧')) else {
        return false;
    };
    !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_digit())
}

fn escape_html(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn cleanup_html(text: &str) -> String {
    let unescaped = unescape_html(text);
    strip_notranslate(&unescaped)
}

fn strip_notranslate(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::new();
    let mut rest = text;
    let mut rest_lower = lower.as_str();
    while let Some(start) = rest_lower.find("<span") {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let after_lower = &rest_lower[start..];
        let Some(tag_end) = after_lower.find('>') else {
            out.push_str(after);
            return out;
        };
        let tag = &after_lower[..=tag_end];
        if tag.contains("translate") && tag.contains("no") {
            let body = &after[tag_end + 1..];
            let body_lower = &after_lower[tag_end + 1..];
            if let Some(close) = body_lower.find("</span>") {
                out.push_str(&body[..close]);
                rest = &body[close + "</span>".len()..];
                rest_lower = &body_lower[close + "</span>".len()..];
                continue;
            }
        }
        out.push_str(&after[..=tag_end]);
        rest = &after[tag_end + 1..];
        rest_lower = &after_lower[tag_end + 1..];
    }
    out.push_str(rest);
    out
}

fn unescape_html(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let Some(end) = after.find(';') else {
            out.push_str(after);
            return out;
        };
        let entity = &after[1..end];
        let ch = if entity == "amp" {
            Some('&')
        } else if entity == "lt" {
            Some('<')
        } else if entity == "gt" {
            Some('>')
        } else if entity == "quot" {
            Some('"')
        } else if entity == "apos" || entity == "#39" {
            Some('\'')
        } else if let Some(hex) = entity
            .strip_prefix("#x")
            .or_else(|| entity.strip_prefix("#X"))
        {
            u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
        } else if let Some(dec) = entity.strip_prefix('#') {
            dec.parse::<u32>().ok().and_then(char::from_u32)
        } else {
            None
        };
        if let Some(ch) = ch {
            out.push(ch);
            rest = &after[end + 1..];
        } else {
            out.push('&');
            rest = &rest[start + 1..];
        }
    }
    out.push_str(rest);
    out
}

fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    struct Mock {
        body: Arc<Mutex<String>>,
        base: String,
    }

    impl Mock {
        fn start(response_body: String) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let body = Arc::new(Mutex::new(String::new()));
            let captured = body.clone();
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let req = read_request(&mut stream);
                *captured.lock().unwrap() = req;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                    response_body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            });
            Self {
                body,
                base: format!("http://127.0.0.1:{port}"),
            }
        }
    }

    fn read_request(stream: &mut std::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = stream.read(&mut tmp).unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&buf[..header_end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        if name.eq_ignore_ascii_case("content-length") {
                            value.trim().parse::<usize>().ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
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

    fn json_body(raw: &str) -> Value {
        let start = raw.find("\r\n\r\n").unwrap() + 4;
        serde_json::from_str(&raw[start..]).unwrap()
    }

    #[test]
    fn v2_posts_html_and_keeps_placeholders() {
        let response = r#"{"data":{"translations":[{"translatedText":"See &lt;span translate=&quot;no&quot;&gt;⟦0⟧&lt;/span&gt;."}]}}"#;
        let mock = Mock::start(response.into());
        let client = GoogleTranslator::v2("en", "zh", "test-google-key")
            .unwrap()
            .with_base_url(&mock.base);
        let user = r#"{"segments":[{"id":3,"text":"See ⟦0⟧."}]}"#;
        let out = client.complete("ignored", user).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["translations"][0]["id"], 3);
        assert_eq!(parsed["translations"][0]["text"], "See ⟦0⟧.");
        let captured = mock.body.lock().unwrap().clone();
        assert!(captured.contains("key=test-google-key"));
        assert!(captured.contains("/language/translate/v2"));
        let posted = json_body(&captured);
        assert_eq!(posted["source"], "en");
        assert_eq!(posted["target"], "zh");
        assert_eq!(posted["format"], "html");
        let q = posted["q"][0].as_str().unwrap();
        assert!(q.contains("translate=\"no\""));
        assert!(q.contains("⟦0⟧"));
        let debug = format!("{client:?}");
        assert!(debug.contains("redacted"));
        assert!(!debug.contains("test-google-key"));
        let err = client.scrub("failed test-google-key");
        assert!(!err.contains("test-google-key"));
    }

    #[test]
    fn v3_sends_bearer_and_project() {
        let response = r#"{"translations":[{"translatedText":"你好"}]}"#;
        let mock = Mock::start(response.into());
        let client =
            GoogleTranslator::v3_access_token("en", "zh-CN", "proj-1", "global", "ya29-test-token")
                .unwrap()
                .with_base_url(&mock.base);
        let user = r#"{"segments":[{"id":1,"text":"Hello"}]}"#;
        let out = client.complete("", user).unwrap();
        assert!(out.contains("你好"));
        let captured = mock.body.lock().unwrap().clone();
        let lower = captured.to_ascii_lowercase();
        assert!(lower.contains("authorization: bearer ya29-test-token"));
        assert!(captured.contains("/v3/projects/proj-1/locations/global:translateText"));
        let posted = json_body(&captured);
        assert_eq!(posted["mimeType"], "text/html");
        assert_eq!(posted["targetLanguageCode"], "zh-CN");
        let debug = format!("{client:?}");
        assert!(!debug.contains("ya29-test-token"));
    }

    #[test]
    fn v3_service_account_mints_a_token_then_translates() {
        let pem = test_pem();
        let creds = json!({
            "client_email": "translator@test.iam.gserviceaccount.com",
            "private_key": pem,
            "project_id": "from-json",
            "token_uri": "PLACEHOLDER"
        });
        // Two accepts: token, then translate. The mock helper accepts one
        // connection, so chain them on one listener.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        thread::spawn(move || {
            for (i, response) in [
                r#"{"access_token":"minted-token","expires_in":3600}"#,
                r#"{"translations":[{"translatedText":"done"}]}"#,
            ]
            .into_iter()
            .enumerate()
            {
                let (mut stream, _) = listener.accept().unwrap();
                let req = read_request(&mut stream);
                seen2.lock().unwrap().push(req);
                let body = response;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                if i == 1 {
                    break;
                }
            }
        });
        let base = format!("http://127.0.0.1:{port}");
        let mut creds = creds;
        creds["token_uri"] = json!(base.clone());
        let client =
            GoogleTranslator::v3_service_account("en", "zh", "", "global", &creds.to_string())
                .unwrap()
                .with_base_url(&base);
        assert_eq!(client.project, "from-json");
        let out = client
            .complete("", r#"{"segments":[{"id":0,"text":"Hi"}]}"#)
            .unwrap();
        assert!(out.contains("done"));
        let captured = seen.lock().unwrap().clone();
        assert_eq!(captured.len(), 2);
        assert!(captured[0].contains("grant_type="));
        assert!(captured[0].contains("assertion="));
        assert!(!captured[0].contains(&pem));
        assert!(captured[1]
            .to_ascii_lowercase()
            .contains("authorization: bearer minted-token"));
        let debug = format!("{client:?}");
        assert!(debug.contains("redacted"));
        assert!(!debug.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn unofficial_reads_the_gtx_array() {
        let response = r#"[[["你好","Hello",null,null,1]],null,"en"]"#;
        let mock = Mock::start(response.into());
        let client = GoogleTranslator::unofficial("en", "zh").with_base_url(&mock.base);
        assert_eq!(client.label(), "google-unofficial");
        let out = client
            .complete("", r#"{"segments":[{"id":2,"text":"Hello"}]}"#)
            .unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["translations"][0]["text"], "你好");
        let captured = mock.body.lock().unwrap().clone();
        assert!(captured.contains("client=gtx"));
        assert!(captured.contains("translate_a/single"));
        assert!(captured.contains("q=Hello"));
    }

    fn test_pem() -> String {
        let output = std::process::Command::new("openssl")
            .args([
                "genpkey",
                "-algorithm",
                "RSA",
                "-pkeyopt",
                "rsa_keygen_bits:2048",
            ])
            .output()
            .expect("openssl genpkey");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
}
