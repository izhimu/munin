use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;

use crate::FastEngine;

/// 基于 Laya API 服务的极速快引擎实现 (System 1)
///
/// 通过 10~20ms 级轻量级视觉/文本前向推理，完成机械交互、弹窗自愈与达成断言
#[derive(Debug, Clone)]
pub struct LayaFastEngine {
    endpoint: String,
    client: reqwest::Client,
    model: String,
}

impl LayaFastEngine {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self::with_model(endpoint, "multilingual")
    }

    pub fn with_model(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .tcp_nodelay(true)
                .pool_max_idle_per_host(10)
                .build()
                .unwrap_or_default(),
            model: model.into(),
        }
    }

    pub fn with_client(
        endpoint: impl Into<String>,
        model: impl Into<String>,
        client: reqwest::Client,
    ) -> Self {
        Self {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            client,
            model: model.into(),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn model(&self) -> &str {
        &self.model
    }
}

#[async_trait]
impl FastEngine for LayaFastEngine {
    async fn choice(
        &self,
        state: &serde_json::Value,
        instructions: &str,
        criteria: &HashMap<String, String>,
    ) -> Result<(String, f32)> {
        let payload = json!({
            "model": self.model,
            "state": state,
            "questions": {
                "q": {
                    "type": "choice",
                    "instructions": instructions,
                    "criteria": criteria
                }
            }
        });

        let resp: serde_json::Value = self
            .client
            .post(format!("{}/predict", self.endpoint))
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;

        let ans = &resp["answers"]["q"];
        let choice = ans["choice"]
            .as_str()
            .context("Missing 'choice' field in Laya prediction response")?
            .to_string();
        let conf = ans["confidence"].as_f64().unwrap_or(0.0) as f32;
        Ok((choice, conf))
    }

    async fn probe(
        &self,
        state: &serde_json::Value,
        assertion: &str,
    ) -> Result<(bool, f32)> {
        let payload = json!({
            "model": self.model,
            "state": state,
            "questions": {
                "q": {
                    "type": "noul",
                    "instructions": assertion
                }
            }
        });

        let resp: serde_json::Value = self
            .client
            .post(format!("{}/predict", self.endpoint))
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;

        let prob = resp["answers"]["q"]["noul"]
            .as_f64()
            .or_else(|| resp["answers"]["q"]["probability"].as_f64())
            .unwrap_or(0.0) as f32;
        Ok((prob > 0.5, prob))
    }

    async fn score(
        &self,
        state: &serde_json::Value,
        instructions: &str,
        criteria: &[String],
    ) -> Result<(f32, f32)> {
        let payload = json!({
            "model": self.model,
            "state": state,
            "questions": {
                "q": {
                    "type": "score",
                    "instructions": instructions,
                    "criteria": criteria
                }
            }
        });

        let resp: serde_json::Value = self
            .client
            .post(format!("{}/predict", self.endpoint))
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;

        let ans = &resp["answers"]["q"];
        let score = ans["score"].as_f64().unwrap_or(0.0) as f32;
        let conf = ans["confidence"].as_f64().unwrap_or(0.0) as f32;
        Ok((score, conf))
    }
}
