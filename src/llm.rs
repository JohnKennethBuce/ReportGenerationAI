//! One tiny client for every AI backend.
//!
//! Cloud free tiers (Gemini, Groq, OpenRouter), LM Studio and llama.cpp all speak the
//! OpenAI `/chat/completions` API. Ollama is called through its native API so we can
//! pass `keep_alive` and unload the model from RAM shortly after use.

use crate::{config::Ai, util::truncate};
use serde_json::{json, Value};
use std::time::Duration;

pub struct Msg {
    pub role: &'static str,
    pub content: String,
}

impl Msg {
    pub fn system(c: impl Into<String>) -> Self {
        Self { role: "system", content: c.into() }
    }
    pub fn user(c: impl Into<String>) -> Self {
        Self { role: "user", content: c.into() }
    }
    pub fn assistant(c: impl Into<String>) -> Self {
        Self { role: "assistant", content: c.into() }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Api {
    OpenAi,
    Ollama,
}

pub struct Endpoint {
    api: Api,
    base: String,
    model: String,
    key: String,
    timeout: Duration,
    /// Human-readable, e.g. "Gemini (gemini-2.5-flash)"
    pub label: String,
}

const OLLAMA_DEFAULT: &str = "http://localhost:11434";
/// Small, good-quality local models, best first.
const PREFERRED_LOCAL: &[&str] = &[
    "qwen2.5:1.5b", "llama3.2:1b", "gemma3:1b", "qwen2.5:3b", "llama3.2:3b", "gemma3:4b", "qwen2.5:0.5b",
];

/// Decide which AI to use.
/// `Ok(None)` = no AI (template mode). `Err` = AI requested but misconfigured.
pub fn resolve(cfg: &Ai) -> Result<Option<Endpoint>, String> {
    let provider = cfg.provider.trim().to_lowercase();
    let timeout = Duration::from_secs(cfg.timeout_seconds.clamp(10, 900));
    let model_or = |default: &str| {
        let m = cfg.model.trim();
        if m.is_empty() { default.to_string() } else { m.to_string() }
    };
    let base_or = |default: &str| {
        let b = cfg.base_url.trim().trim_end_matches('/');
        if b.is_empty() { default.to_string() } else { b.to_string() }
    };
    let key = cfg.api_key.trim().to_string();

    let cloud = |name: &str, base: &str, model: &str, key_url: &str| {
        if key.is_empty() {
            return Err(format!("provider \"{}\" needs an api_key - get one free at {key_url}", name.to_lowercase()));
        }
        let model = model_or(model);
        Ok(Some(Endpoint {
            api: Api::OpenAi,
            base: base_or(base),
            label: format!("{name} ({model})"),
            model,
            key: key.clone(),
            timeout,
        }))
    };

    let ollama = |model: String, base: String, key: String| Endpoint {
        api: Api::Ollama,
        label: if key.is_empty() {
            format!("Ollama local ({model})")
        } else {
            format!("Ollama Cloud ({model})")
        },
        base,
        model,
        key,
        timeout,
    };

    match provider.as_str() {
        "" | "template" | "none" | "off" => Ok(None),
        "gemini" => cloud(
            "Gemini",
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "gemini-2.5-flash",
            "https://aistudio.google.com/apikey",
        ),
        "groq" => cloud("Groq", "https://api.groq.com/openai/v1", "llama-3.1-8b-instant", "https://console.groq.com/keys"),
        "openrouter" => cloud(
            "OpenRouter",
            "https://openrouter.ai/api/v1",
            "meta-llama/llama-3.3-70b-instruct:free",
            "https://openrouter.ai/keys",
        ),
        "ollama_cloud" => cloud(
            "Ollama Cloud",
            "https://ollama.com/v1",
            "gpt-oss:20b",
            "https://ollama.com/settings/keys",
        ),
        "openai_compatible" | "openai" => {
            let base = cfg.base_url.trim().trim_end_matches('/').to_string();
            let model = cfg.model.trim().to_string();
            if base.is_empty() || model.is_empty() {
                return Err("provider \"openai_compatible\" needs base_url and model".into());
            }
            Ok(Some(Endpoint {
                api: Api::OpenAi,
                label: format!("{model} @ {base}"),
                base,
                model,
                key,
                timeout,
            }))
        }
        "ollama" => {
            if !key.is_empty() && cfg.base_url.trim().is_empty() {
                return cloud(
                    "Ollama Cloud",
                    "https://ollama.com/v1",
                    "gemma4:31b",
                    "https://ollama.com/settings/keys",
                );
            }
            let base = ollama_base(&base_or(OLLAMA_DEFAULT));
            let model = if cfg.model.trim().is_empty() {
                ollama_models(&base)
                    .and_then(|m| best_local(&m))
                    .unwrap_or_else(|| PREFERRED_LOCAL[0].to_string())
            } else {
                cfg.model.trim().to_string()
            };
            Ok(Some(ollama(model, base, key)))
        }
        "auto" => {
            // If an API key is provided in config.toml, auto-detect its cloud provider first (0 MB RAM!)
            if !key.is_empty() {
                if key.starts_with("AIza") {
                    return cloud(
                        "Gemini",
                        "https://generativelanguage.googleapis.com/v1beta/openai",
                        "gemini-2.5-flash",
                        "https://aistudio.google.com/apikey",
                    );
                } else if key.starts_with("gsk_") {
                    return cloud(
                        "Groq",
                        "https://api.groq.com/openai/v1",
                        "llama-3.1-8b-instant",
                        "https://console.groq.com/keys",
                    );
                } else if key.starts_with("sk-or-") {
                    return cloud(
                        "OpenRouter",
                        "https://openrouter.ai/api/v1",
                        "meta-llama/llama-3.3-70b-instruct:free",
                        "https://openrouter.ai/keys",
                    );
                } else {
                    return cloud(
                        "Ollama Cloud",
                        "https://ollama.com/v1",
                        "gpt-oss:20b",
                        "https://ollama.com/settings/keys",
                    );
                }
            }
            let base = ollama_base(&base_or(OLLAMA_DEFAULT));
            let found = ollama_models(&base).and_then(|models| {
                if cfg.model.trim().is_empty() { best_local(&models) } else { Some(cfg.model.trim().to_string()) }
            });
            Ok(found.map(|model| ollama(model, base, String::new())))
        }
        other => Err(format!("unknown provider \"{other}\" in config.toml")),
    }
}

fn ollama_base(b: &str) -> String {
    b.trim_end_matches('/').trim_end_matches("/v1").to_string()
}

/// Installed Ollama models, or `None` if Ollama isn't running (fast: ~1 s max).
fn ollama_models(base: &str) -> Option<Vec<String>> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(700))
        .timeout(Duration::from_secs(2))
        .build();
    let v: Value = agent.get(&format!("{base}/api/tags")).call().ok()?.into_json().ok()?;
    Some(
        v["models"]
            .as_array()?
            .iter()
            .filter_map(|m| m["name"].as_str().map(String::from))
            .filter(|n| !n.contains("embed"))
            .collect(),
    )
}

fn best_local(models: &[String]) -> Option<String> {
    PREFERRED_LOCAL
        .iter()
        .find(|p| models.iter().any(|m| m == *p))
        .map(|p| p.to_string())
        .or_else(|| models.first().cloned())
}

/// Send a conversation and return the reply text.
pub fn complete(ep: &Endpoint, msgs: &[Msg]) -> Result<String, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(ep.timeout)
        .build();
    let messages: Vec<Value> = msgs
        .iter()
        .map(|m| json!({ "role": m.role, "content": m.content }))
        .collect();

    let (url, body) = match ep.api {
        Api::OpenAi => (
            format!("{}/chat/completions", ep.base),
            json!({ "model": ep.model, "messages": messages, "temperature": 0.3 }),
        ),
        Api::Ollama => (
            format!("{}/api/chat", ep.base),
            json!({
                "model": ep.model,
                "messages": messages,
                "stream": false,
                "keep_alive": "2m",          // free the RAM 2 minutes after the last use
                "options": { "temperature": 0.3, "num_ctx": 8192 }
            }),
        ),
    };

    let mut req = agent.post(&url).set("X-Title", "DayRecap");
    if !ep.key.is_empty() {
        req = req.set("Authorization", &format!("Bearer {}", ep.key));
    }

    let resp: Value = match req.send_json(body) {
        Ok(r) => r
            .into_json()
            .map_err(|e| format!("{} sent an unreadable response ({e})", ep.label))?,
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            return Err(format!(
                "{} returned HTTP {code}{} {}",
                ep.label,
                hint(code),
                truncate(text.trim(), 300)
            ));
        }
        Err(e) => return Err(format!("could not reach {}: {e}", ep.label)),
    };

    let text = match ep.api {
        Api::OpenAi => resp["choices"][0]["message"]["content"].as_str(),
        Api::Ollama => resp["message"]["content"].as_str(),
    }
    .unwrap_or_default();

    let text = strip_think(text);
    if text.is_empty() {
        Err(format!("{} returned an empty answer", ep.label))
    } else {
        Ok(text)
    }
}

fn hint(code: u16) -> &'static str {
    match code {
        400 => " (bad request - check the model name):",
        401 | 403 => " (check your api_key):",
        404 => " (model or URL not found - check model / base_url):",
        429 => " (free-tier rate limit reached - try again in a minute):",
        500..=599 => " (the AI service is having problems - try again later):",
        _ => ":",
    }
}

/// Remove `<think>...</think>` blocks produced by reasoning models.
fn strip_think(text: &str) -> String {
    let mut s = text.to_string();
    while let (Some(a), Some(b)) = (s.find("<think>"), s.find("</think>")) {
        if b < a {
            break;
        }
        s.replace_range(a..b + "</think>".len(), "");
    }
    s.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_reasoning() {
        assert_eq!(strip_think("<think>hmm</think>\nHello"), "Hello");
        assert_eq!(strip_think("plain"), "plain");
    }

    #[test]
    fn template_and_missing_key() {
        let mut ai = Ai { provider: "template".into(), ..Ai::default() };
        assert!(resolve(&ai).unwrap().is_none());
        ai.provider = "gemini".into();
        assert!(resolve(&ai).is_err());
        ai.api_key = "k".into();
        assert_eq!(resolve(&ai).unwrap().unwrap().label, "Gemini (gemini-2.5-flash)");
    }
}
