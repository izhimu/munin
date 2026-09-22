use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::prelude::*;
use munin_types::MacroStep;
use serde_json::json;

use crate::SlowEngine;

/// 基于 OpenAI / 兼容接口的慢引擎实现 (System 2)
///
/// 负责宏观任务规划 (Plan) 与复杂障碍/低置信度时的多模态仲裁 (Arbitrate)
#[derive(Debug, Clone)]
pub struct OpenAISlowEngine {
    api_key: String,
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl OpenAISlowEngine {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_config(api_key, "https://api.openai.com/v1", "gpt-4o")
    }

    pub fn with_config(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            client: reqwest::Client::new(),
        }
    }

    pub fn with_client(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        client: reqwest::Client,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            client,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn clean_json_markdown(text: &str) -> &str {
        let trimmed = text.trim();
        if let Some(stripped) = trimmed.strip_prefix("```json") {
            if let Some(inner) = stripped.strip_suffix("```") {
                return inner.trim();
            }
        } else if let Some(stripped) = trimmed.strip_prefix("```") {
            if let Some(inner) = stripped.strip_suffix("```") {
                return inner.trim();
            }
        }
        trimmed
    }
}

#[async_trait]
impl SlowEngine for OpenAISlowEngine {
    async fn plan(&self, user_goal: &str, context: &str) -> Result<Vec<MacroStep>> {
        let system_prompt = r#"You are an expert browser automation planner (System 2).
Given a high-level user goal and current context, decompose the goal into a minimal, sequential list of macro steps.
Output strictly a valid JSON array of objects with the following schema:
[
  {
    "step_id": 1,
    "intent": "Brief description of what to do in this phase",
    "expected_outcome": "Condition or indicator that confirms this step succeeded"
  }
]
Do not output any introductory or concluding text. Output JSON only."#;

        let user_prompt = format!("User Goal: {}\nCurrent Context: {}", user_goal, context);

        let payload = json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system_prompt },
                { "role": "user", "content": user_prompt }
            ],
            "temperature": 0.1
        });

        let resp: serde_json::Value = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .context("Failed to send plan request to OpenAI")?
            .json()
            .await
            .context("Failed to parse OpenAI plan response as JSON")?;

        let content = resp["choices"][0]["message"]["content"]
            .as_str()
            .context("Missing choices[0].message.content in OpenAI response")?;

        let cleaned = Self::clean_json_markdown(content);
        let steps: Vec<MacroStep> = serde_json::from_str(cleaned)
            .with_context(|| format!("Failed to deserialize MacroSteps from response: {}", cleaned))?;

        Ok(steps)
    }

    async fn arbitrate(
        &self,
        step_intent: &str,
        current_dom_desc: &str,
        screenshot: Option<&[u8]>,
    ) -> Result<String> {
        let system_prompt = r#"You are an expert browser automation supervisor and arbitrator (System 2).
The fast reflex engine encountered low confidence or stalled on the current step.
Analyze the provided DOM interactive elements and optional screenshot.
Output a concise resolution: state the exact node ID to click or fill, or describe the specific corrective action required."#;

        let user_text = format!(
            "Current Step Intent: {}\nCandidate Elements Description:\n{}",
            step_intent, current_dom_desc
        );

        let user_message_content = if let Some(bytes) = screenshot {
            if !bytes.is_empty() {
                let encoded = BASE64_STANDARD.encode(bytes);
                json!([
                    { "type": "text", "text": user_text },
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:image/png;base64,{}", encoded)
                        }
                    }
                ])
            } else {
                json!(user_text)
            }
        } else {
            json!(user_text)
        };

        let payload = json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system_prompt },
                { "role": "user", "content": user_message_content }
            ],
            "temperature": 0.2
        });

        let resp: serde_json::Value = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .context("Failed to send arbitrate request to OpenAI")?
            .json()
            .await
            .context("Failed to parse OpenAI arbitrate response as JSON")?;

        let content = resp["choices"][0]["message"]["content"]
            .as_str()
            .context("Missing choices[0].message.content in OpenAI response")?;

        Ok(content.trim().to_string())
    }
}
