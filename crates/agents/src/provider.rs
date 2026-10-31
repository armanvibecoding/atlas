//! LLM sağlayıcıları: Anthropic (Messages API), xAI (OpenAI uyumlu chat completions), test sahtesi.
//! Anahtarlar repo'da değil: `ATLAS_ANTHROPIC_KEY`, `ATLAS_XAI_KEY` (/etc/atlas/atlas.env).

use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmResponse {
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

pub trait LlmProvider {
    fn name(&self) -> String;
    fn complete(&self, system: &str, user: &str, max_tokens: u32) -> Result<LlmResponse, String>;
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())
}

pub struct Anthropic {
    key: String,
    model: String,
    client: reqwest::blocking::Client,
}

impl Anthropic {
    pub fn from_env(model: &str) -> Result<Anthropic, String> {
        let key = std::env::var("ATLAS_ANTHROPIC_KEY")
            .map_err(|_| "ATLAS_ANTHROPIC_KEY yok".to_string())?;
        Ok(Anthropic {
            key,
            model: model.to_string(),
            client: client()?,
        })
    }
}

impl LlmProvider for Anthropic {
    fn name(&self) -> String {
        format!("anthropic/{}", self.model)
    }
    fn complete(&self, system: &str, user: &str, max_tokens: u32) -> Result<LlmResponse, String> {
        let r = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.key)
            .header("anthropic-version", "2023-06-01")
            .json(
                &serde_json::json!({"model": self.model, "max_tokens": max_tokens, "system": system,
                "messages": [{"role": "user", "content": user}]}),
            )
            .send()
            .map_err(|e| e.to_string())?;
        let status = r.status();
        let v: serde_json::Value = r.json().map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("HTTP {status}: {v}"));
        }
        let text = v["content"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| c["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        Ok(LlmResponse {
            text,
            input_tokens: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
        })
    }
}

pub struct Xai {
    key: String,
    model: String,
    client: reqwest::blocking::Client,
}

impl Xai {
    pub fn from_env(model: &str) -> Result<Xai, String> {
        let key = std::env::var("ATLAS_XAI_KEY").map_err(|_| "ATLAS_XAI_KEY yok".to_string())?;
        Ok(Xai {
            key,
            model: model.to_string(),
            client: client()?,
        })
    }
}

impl LlmProvider for Xai {
    fn name(&self) -> String {
        format!("xai/{}", self.model)
    }
    fn complete(&self, system: &str, user: &str, max_tokens: u32) -> Result<LlmResponse, String> {
        let r = self
            .client
            .post("https://api.x.ai/v1/chat/completions")
            .bearer_auth(&self.key)
            .json(&serde_json::json!({"model": self.model, "max_tokens": max_tokens,
                "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}]}))
            .send()
            .map_err(|e| e.to_string())?;
        let status = r.status();
        let v: serde_json::Value = r.json().map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("HTTP {status}: {v}"));
        }
        Ok(LlmResponse {
            text: v["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            input_tokens: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
            output_tokens: v["usage"]["completion_tokens"].as_u64().unwrap_or(0),
        })
    }
}

/// Sırayla hazır yanıt döndüren test sağlayıcısı.
pub struct MockProvider {
    replies: Mutex<std::collections::VecDeque<String>>,
    calls: Mutex<usize>,
}

impl MockProvider {
    pub fn new(replies: Vec<String>) -> Self {
        MockProvider {
            replies: Mutex::new(replies.into()),
            calls: Mutex::new(0),
        }
    }
    pub fn calls(&self) -> usize {
        self.calls.lock().map(|g| *g).unwrap_or(0)
    }
}

impl LlmProvider for MockProvider {
    fn name(&self) -> String {
        "mock".into()
    }
    fn complete(&self, _s: &str, user: &str, _m: u32) -> Result<LlmResponse, String> {
        if let Ok(mut c) = self.calls.lock() {
            *c += 1;
        }
        let text = self
            .replies
            .lock()
            .map_err(|e| e.to_string())?
            .pop_front()
            .ok_or("yanıt kalmadı")?;
        Ok(LlmResponse {
            input_tokens: user.len() as u64 / 4 + 100,
            output_tokens: text.len() as u64 / 4 + 1,
            text,
        })
    }
}
