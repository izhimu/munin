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
    /// 对比式达成判定阈值（多语言嵌入模型在企业后台域外词汇下分数偏低，0.85 过严）
    achieved_threshold: f32,
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
            achieved_threshold: 0.60,
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
            achieved_threshold: 0.60,
        }
    }

    pub fn with_achieved_threshold(mut self, threshold: f32) -> Self {
        self.achieved_threshold = threshold;
        self
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

    async fn probe_and_choice(
        &self,
        state: &serde_json::Value,
        assertion: &str,
        instructions: &str,
        criteria: &HashMap<String, String>,
    ) -> Result<((bool, f32), (String, f32))> {
        let payload = json!({
            "model": self.model,
            "state": state,
            "questions": {
                "probe": {
                    "type": "noul",
                    "instructions": assertion
                },
                "choice": {
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

        let prob = resp["answers"]["probe"]["noul"]
            .as_f64()
            .or_else(|| resp["answers"]["probe"]["probability"].as_f64())
            .unwrap_or(0.0) as f32;

        let ans = &resp["answers"]["choice"];
        let choice = ans["choice"]
            .as_str()
            .context("Missing 'choice' field in Laya prediction response")?
            .to_string();
        let conf = ans["confidence"].as_f64().unwrap_or(0.0) as f32;

        Ok(((prob > 0.5, prob), (choice, conf)))
    }

    async fn recovery_graph(
        &self,
        state: &serde_json::Value,
        expected_outcome: &str,
        criteria: &HashMap<String, String>,
    ) -> Result<munin_types::RecoveryVerdict> {
        use munin_types::{BlockerClass, RecoveryKind};

        let blocker_map: HashMap<String, String> = BlockerClass::all()
            .iter()
            .map(|(k, _, d)| (k.to_string(), d.to_string()))
            .collect();
        let recovery_map: HashMap<String, String> = RecoveryKind::all()
            .iter()
            .map(|(k, _, d)| (k.to_string(), d.to_string()))
            .collect();

        let mut questions = json!({
            "achieved_pos": {"type": "noul", "instructions": format!("当前页面状态已表明：{}（结果已达成）", expected_outcome)},
            "achieved_neg": {"type": "noul", "instructions": format!("当前页面仍未达成：{}（结果未出现）", expected_outcome)},
            "blocked_pos": {"type": "noul", "instructions": "当前页面存在阻止目标交互的遮挡层、模态弹窗、加载中或权限墙"},
            "blocked_neg": {"type": "noul", "instructions": "当前页面完全无遮挡，所有元素可正常交互"},
            "blocker": {"type": "choice", "instructions": "当前阻塞属于哪一类别？", "criteria": blocker_map},
            "recovery": {"type": "choice", "instructions": "应采取何种恢复动作解除阻塞并推进目标？", "criteria": recovery_map}
        });
        if !criteria.is_empty() {
            questions["target"] = json!({
                "type": "choice",
                "instructions": "哪个元素是推进当前任务目标最匹配的交互项？",
                "criteria": criteria
            });
            questions["recovery_target"] = json!({
                "type": "choice",
                "instructions": "恢复动作（如关闭弹窗、解除遮罩）应作用于哪个元素？",
                "criteria": criteria
            });
        }

        let payload = json!({ "model": self.model, "state": state, "questions": questions });
        let resp: serde_json::Value = self
            .client
            .post(format!("{}/predict", self.endpoint))
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;

        let a = &resp["answers"];
        let noul_val = |key: &str| -> f32 {
            a[key]["noul"]
                .as_f64()
                .or_else(|| a[key]["probability"].as_f64())
                .unwrap_or(0.0) as f32
        };
        let contrastive = |pos: f32, neg: f32, threshold: f32| -> (bool, f32) {
            let decided = pos >= threshold && neg <= (1.0 - threshold);
            (decided, pos.min(1.0 - neg))
        };

        let achieved = contrastive(noul_val("achieved_pos"), noul_val("achieved_neg"), self.achieved_threshold);
        let blocked = contrastive(noul_val("blocked_pos"), noul_val("blocked_neg"), 0.80);

        let parse_choice = |key: &str| -> (String, f32) {
            let ans = &a[key];
            if ans.is_null() {
                tracing::warn!("Laya 响应缺失问题键 '{}'，按零置信处理", key);
                return (String::new(), 0.0);
            }
            let choice = ans["choice"].as_str().unwrap_or("").to_string();
            let conf = ans["confidence"].as_f64().unwrap_or(0.0) as f32;
            (choice, conf)
        };
        let (bk, bc) = parse_choice("blocker");
        let blocker = (
            BlockerClass::all()
                .iter()
                .find(|(k, _, _)| *k == bk)
                .map(|(_, v, _)| *v)
                .unwrap_or(BlockerClass::None),
            bc,
        );
        let (rk, rc) = parse_choice("recovery");
        let recovery = (
            RecoveryKind::all()
                .iter()
                .find(|(k, _, _)| *k == rk)
                .map(|(_, v, _)| *v)
                .unwrap_or(RecoveryKind::Escalate),
            rc,
        );

        let target = if criteria.is_empty() {
            None
        } else {
            let (choice, conf) = parse_choice("target");
            Some((choice, conf))
        };

        let recovery_target = if criteria.is_empty() {
            None
        } else {
            let (choice, conf) = parse_choice("recovery_target");
            Some((choice, conf))
        };

        Ok(munin_types::RecoveryVerdict {
            achieved,
            blocked,
            blocker,
            recovery,
            target,
            recovery_target,
        })
    }

    async fn match_memory(
        &self,
        state: &serde_json::Value,
        entries: &[munin_types::MemoryEntry],
    ) -> Result<Option<(usize, f32)>> {
        if entries.is_empty() {
            return Ok(None);
        }

        // 批量正反探针：每条目两问，单次请求判定全部
        let mut questions = serde_json::Map::new();
        for (idx, entry) in entries.iter().enumerate() {
            questions.insert(
                format!("app_{}", idx),
                json!({"type": "noul", "instructions": entry.applicable_desc}),
            );
            questions.insert(
                format!("inapp_{}", idx),
                json!({"type": "noul", "instructions": entry.inapplicable_desc}),
            );
        }
        let payload = json!({
            "model": self.model,
            "state": state,
            "questions": serde_json::Value::Object(questions)
        });
        let resp: serde_json::Value = self
            .client
            .post(format!("{}/predict", self.endpoint))
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;

        let a = &resp["answers"];
        let noul_val = |key: &str| -> f32 {
            a[key]["noul"]
                .as_f64()
                .or_else(|| a[key]["probability"].as_f64())
                .unwrap_or(0.0) as f32
        };

        let mut best: Option<(usize, f32)> = None;
        for (idx, entry) in entries.iter().enumerate() {
            let pos = noul_val(&format!("app_{}", idx));
            let neg = noul_val(&format!("inapp_{}", idx));
            let hit = pos >= 0.80 && neg <= 0.20;
            let conf = pos.min(1.0 - neg);
            if hit && entry.success_rate() >= 0.5
                && best.map(|(_, c)| conf > c).unwrap_or(true) {
                    best = Some((idx, conf));
                }
        }
        Ok(best)
    }
}
