use async_trait::async_trait;
use tracing::info;

/// Raw audio bytes returned by a TTS provider.
/// Always OGG/Opus so every voice-capable channel can send it directly.
pub type AudioBytes = Vec<u8>;

/// Abstraction over any text-to-speech backend.
#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Convert `text` to speech and return raw audio bytes (OGG/Opus).
    async fn synthesize(&self, text: &str) -> Result<AudioBytes, String>;

    /// Short identifier used in log messages.
    fn name(&self) -> &'static str;
}

// ---------------------------------------------------------------------------
// OpenAI TTS  (tts-1 / tts-1-hd)
// ---------------------------------------------------------------------------

/// Calls the OpenAI `/v1/audio/speech` endpoint.
pub struct OpenAiTts {
    client: reqwest::Client,
    api_key: String,
    model: String,
    voice: String,
}

impl OpenAiTts {
    pub fn new(api_key: String, model: Option<String>, voice: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            model: model.unwrap_or_else(|| "tts-1".to_string()),
            voice: voice.unwrap_or_else(|| "alloy".to_string()),
        }
    }
}

#[async_trait]
impl TtsProvider for OpenAiTts {
    fn name(&self) -> &'static str {
        "openai"
    }

    async fn synthesize(&self, text: &str) -> Result<AudioBytes, String> {
        info!("openai tts: synthesizing {} chars", text.len());
        let resp = self
            .client
            .post("https://api.openai.com/v1/audio/speech")
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": self.model,
                "input": text,
                "voice": self.voice,
                "response_format": "opus",
            }))
            .send()
            .await
            .map_err(|e| format!("openai tts request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("openai tts error {status}: {body}"));
        }

        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| format!("openai tts read body failed: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Kokoro TTS (self-hosted via kokoro-fastapi)
//
// Enable with:  cargo build --features tts-kokoro
//
// Expects a running Kokoro FastAPI server (https://github.com/remsky/Kokoro-FastAPI).
// Default base URL: http://localhost:8880
//
// Config example:
//   voice:
//     tts_provider: kokoro
//     base_url: http://localhost:8880
//     voice: af_heart
//     auto_reply_voice: true
// ---------------------------------------------------------------------------

#[cfg(feature = "tts-kokoro")]
pub struct KokoroTts {
    client: reqwest::Client,
    base_url: String,
    voice: String,
}

#[cfg(feature = "tts-kokoro")]
impl KokoroTts {
    pub fn new(base_url: Option<String>, voice: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url
                .unwrap_or_else(|| "http://localhost:8880".to_string())
                .trim_end_matches('/')
                .to_string(),
            voice: voice.unwrap_or_else(|| "af_heart".to_string()),
        }
    }
}

#[cfg(feature = "tts-kokoro")]
#[async_trait]
impl TtsProvider for KokoroTts {
    fn name(&self) -> &'static str {
        "kokoro"
    }

    async fn synthesize(&self, text: &str) -> Result<AudioBytes, String> {
        info!("kokoro tts: synthesizing {} chars", text.len());
        let url = format!("{}/v1/audio/speech", self.base_url);
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({
                "model": "kokoro",
                "input": text,
                "voice": self.voice,
                "response_format": "opus",
            }))
            .send()
            .await
            .map_err(|e| format!("kokoro tts request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("kokoro tts error {status}: {body}"));
        }

        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| format!("kokoro tts read body failed: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

use std::sync::Arc;

/// Build a `TtsProvider` from config values.
/// Returns `None` if `tts_provider` is not set or unrecognised.
pub fn build_tts_provider(
    tts_provider: Option<&str>,
    api_key: Option<String>,
    model: Option<String>,
    voice: Option<String>,
    tts_base_url: Option<String>,
) -> Option<Arc<dyn TtsProvider>> {
    match tts_provider? {
        "openai" => {
            let key = api_key?;
            Some(Arc::new(OpenAiTts::new(key, model, voice)))
        }
        #[cfg(feature = "tts-kokoro")]
        "kokoro" => Some(Arc::new(KokoroTts::new(tts_base_url, voice))),
        #[cfg(not(feature = "tts-kokoro"))]
        "kokoro" => {
            tracing::warn!(
                "tts_provider 'kokoro' requires the `tts-kokoro` feature flag. \
                 Rebuild with: cargo build --features tts-kokoro"
            );
            None
        }
        other => {
            tracing::warn!("unknown tts_provider '{other}' — ignoring");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Fake OGG/Opus header (4 bytes) — just enough for the "non-empty bytes" check.
    const FAKE_AUDIO: &[u8] = b"OggS";

    // -----------------------------------------------------------------------
    // build_tts_provider
    // -----------------------------------------------------------------------

    #[test]
    fn build_tts_provider_none_when_no_provider() {
        assert!(build_tts_provider(None, None, None, None, None).is_none());
    }

    #[test]
    fn build_tts_provider_none_when_openai_missing_key() {
        assert!(build_tts_provider(Some("openai"), None, None, None, None).is_none());
    }

    #[test]
    fn build_tts_provider_openai_returns_provider() {
        let p = build_tts_provider(Some("openai"), Some("sk-test".into()), None, None, None);
        assert!(p.is_some());
        assert_eq!(p.unwrap().name(), "openai");
    }

    #[test]
    fn build_tts_provider_unknown_returns_none() {
        assert!(build_tts_provider(Some("elevenlabs"), None, None, None, None).is_none());
    }

    // -----------------------------------------------------------------------
    // OpenAiTts
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn openai_tts_success() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .and(header("authorization", "Bearer sk-test"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(FAKE_AUDIO)
                    .insert_header("content-type", "audio/ogg"),
            )
            .mount(&server)
            .await;

        // Point OpenAiTts at the mock server by building it directly and
        // overriding the URL via a custom client with a base-url prefix.
        // Since OpenAiTts hardcodes the URL, we test via a wrapper struct.
        let tts = OpenAiTtsWithBaseUrl::new("sk-test".into(), None, None, server.uri());
        let audio = tts.synthesize("hello world").await.unwrap();
        assert_eq!(audio, FAKE_AUDIO);
    }

    #[tokio::test]
    async fn openai_tts_error_response_returns_err() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"invalid key"}"#))
            .mount(&server)
            .await;

        let tts = OpenAiTtsWithBaseUrl::new("bad-key".into(), None, None, server.uri());
        let err = tts.synthesize("hello").await.unwrap_err();
        assert!(err.contains("401"), "expected 401 in error: {err}");
    }

    // -----------------------------------------------------------------------
    // Helper: OpenAiTts with configurable base URL (test-only)
    // -----------------------------------------------------------------------

    struct OpenAiTtsWithBaseUrl {
        client: reqwest::Client,
        api_key: String,
        model: String,
        voice: String,
        base_url: String,
    }

    impl OpenAiTtsWithBaseUrl {
        fn new(
            api_key: String,
            model: Option<String>,
            voice: Option<String>,
            base_url: String,
        ) -> Self {
            Self {
                client: reqwest::Client::new(),
                api_key,
                model: model.unwrap_or_else(|| "tts-1".to_string()),
                voice: voice.unwrap_or_else(|| "alloy".to_string()),
                base_url: base_url.trim_end_matches('/').to_string(),
            }
        }

        async fn synthesize(&self, text: &str) -> Result<AudioBytes, String> {
            let url = format!("{}/v1/audio/speech", self.base_url);
            let resp = self
                .client
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&serde_json::json!({
                    "model": self.model,
                    "input": text,
                    "voice": self.voice,
                    "response_format": "opus",
                }))
                .send()
                .await
                .map_err(|e| format!("openai tts request failed: {e}"))?;

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(format!("openai tts error {status}: {body}"));
            }

            resp.bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| format!("openai tts read body failed: {e}"))
        }
    }

    // -----------------------------------------------------------------------
    // KokoroTts (only compiled when feature flag is on)
    // -----------------------------------------------------------------------

    #[cfg(feature = "tts-kokoro")]
    #[tokio::test]
    async fn kokoro_tts_success() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(FAKE_AUDIO)
                    .insert_header("content-type", "audio/ogg"),
            )
            .mount(&server)
            .await;

        let tts = KokoroTts::new(Some(server.uri()), None);
        let audio = tts.synthesize("こんにちは").await.unwrap();
        assert_eq!(audio, FAKE_AUDIO);
    }

    #[cfg(feature = "tts-kokoro")]
    #[tokio::test]
    async fn kokoro_tts_error_response_returns_err() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&server)
            .await;

        let tts = KokoroTts::new(Some(server.uri()), None);
        let err = tts.synthesize("test").await.unwrap_err();
        assert!(err.contains("500"), "expected 500 in error: {err}");
    }

    #[cfg(feature = "tts-kokoro")]
    #[tokio::test]
    async fn build_tts_provider_kokoro_returns_provider() {
        let p = build_tts_provider(Some("kokoro"), None, None, Some("af_sky".into()), None);
        assert!(p.is_some());
        assert_eq!(p.unwrap().name(), "kokoro");
    }
}
