use anyhow::Result;
use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use crate::FastEngine;

#[derive(Debug, Default)]
struct MockFastInner {
    default_choice: Option<(String, f32)>,
    default_probe: Option<(bool, f32)>,
    default_score: Option<(f32, f32)>,
    choice_map: HashMap<String, (String, f32)>,
    probe_map: HashMap<String, (bool, f32)>,
    score_map: HashMap<String, (f32, f32)>,
    probe_sequence: Vec<(bool, f32)>,
    probe_call_count: usize,
    /// 恢复图返回的阻塞判定（None 时默认无阻塞）
    blocked_override: Option<(bool, f32)>,
    /// 阻塞判定序列（按调用次数消费，耗尽后保持最后值；优先级高于 blocked_override）
    blocked_sequence: Vec<(bool, f32)>,
    blocked_call_count: usize,
    /// 恢复图返回的恢复动作类别（None 时默认 Retry）
    recovery_override: Option<(munin_types::RecoveryKind, f32)>,
}
/// 内存模拟快引擎（用于单元测试与确定性验证）
#[derive(Debug, Clone, Default)]
pub struct MockFastEngine {
    inner: Arc<Mutex<MockFastInner>>,
}

impl MockFastEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_choice(self, choice: impl Into<String>, confidence: f32) -> Self {
        self.set_default_choice(choice, confidence);
        self
    }

    pub fn with_probe(self, result: bool, probability: f32) -> Self {
        self.set_default_probe(result, probability);
        self
    }

    pub fn set_default_choice(&self, choice: impl Into<String>, confidence: f32) {
        let mut inner = self.inner.lock();
        inner.default_choice = Some((choice.into(), confidence));
    }

    pub fn set_default_probe(&self, result: bool, probability: f32) {
        let mut inner = self.inner.lock();
        inner.default_probe = Some((result, probability));
    }

    pub fn set_default_score(&self, score: f32, confidence: f32) {
        let mut inner = self.inner.lock();
        inner.default_score = Some((score, confidence));
    }

    pub fn map_choice_by_instruction(
        &self,
        instruction_substring: &str,
        choice: impl Into<String>,
        confidence: f32,
    ) {
        let mut inner = self.inner.lock();
        inner
            .choice_map
            .insert(instruction_substring.to_string(), (choice.into(), confidence));
    }

    pub fn map_probe_by_assertion(
        &self,
        assertion_substring: &str,
        result: bool,
        probability: f32,
    ) {
        let mut inner = self.inner.lock();
        inner
            .probe_map
            .insert(assertion_substring.to_string(), (result, probability));
    }
    pub fn with_probe_sequence(self, sequence: Vec<(bool, f32)>) -> Self {
        let mut inner = self.inner.lock();
        inner.probe_sequence = sequence;
        drop(inner);
        self
    }

    pub fn set_probe_sequence(&self, sequence: Vec<(bool, f32)>) {
        let mut inner = self.inner.lock();
        inner.probe_sequence = sequence;
        inner.probe_call_count = 0;
    }

    /// 强制恢复图返回指定阻塞判定（测试弹窗/恢复路径用）
    pub fn set_blocked(&self, blocked: bool, confidence: f32) {
        self.inner.lock().blocked_override = Some((blocked, confidence));
    }

    /// 强制恢复图返回指定恢复动作类别
    pub fn set_recovery(&self, kind: munin_types::RecoveryKind, confidence: f32) {
        self.inner.lock().recovery_override = Some((kind, confidence));
    }

    /// 阻塞判定按序列消费（如首次 true 后续 false，模拟弹窗消解后恢复通畅）
    pub fn set_blocked_sequence(&self, sequence: Vec<(bool, f32)>) {
        let mut inner = self.inner.lock();
        inner.blocked_sequence = sequence;
        inner.blocked_call_count = 0;
    }
}

#[async_trait]
impl FastEngine for MockFastEngine {
    async fn choice(
        &self,
        _state: &serde_json::Value,
        instructions: &str,
        criteria: &HashMap<String, String>,
    ) -> Result<(String, f32)> {
        let inner = self.inner.lock();
        for (k, v) in &inner.choice_map {
            if instructions.contains(k) {
                return Ok(v.clone());
            }
        }
        if let Some((choice, conf)) = &inner.default_choice {
            return Ok((choice.clone(), *conf));
        }
        // Fallback: pick first key in criteria or empty string
        let first_key = criteria.keys().next().cloned().unwrap_or_default();
        Ok((first_key, 0.90))
    }

    async fn probe(
        &self,
        _state: &serde_json::Value,
        assertion: &str,
    ) -> Result<(bool, f32)> {
        let mut inner = self.inner.lock();
        if !inner.probe_sequence.is_empty() {
            let idx = inner.probe_call_count;
            inner.probe_call_count += 1;
            let val = if idx < inner.probe_sequence.len() {
                inner.probe_sequence[idx]
            } else {
                *inner.probe_sequence.last().unwrap()
            };
            return Ok(val);
        }
        for (k, v) in &inner.probe_map {
            if assertion.contains(k) {
                return Ok(*v);
            }
        }
        if let Some((res, prob)) = inner.default_probe {
            return Ok((res, prob));
        }
        Ok((true, 0.95))
    }

    async fn score(
        &self,
        _state: &serde_json::Value,
        instructions: &str,
        _criteria: &[String],
    ) -> Result<(f32, f32)> {
        let inner = self.inner.lock();
        for (k, v) in &inner.score_map {
            if instructions.contains(k) {
                return Ok(*v);
            }
        }
        if let Some((score, conf)) = &inner.default_score {
            return Ok((*score, *conf));
        }
        Ok((5.0, 0.90))
    }

    async fn recovery_graph(
        &self,
        state: &serde_json::Value,
        expected_outcome: &str,
        criteria: &HashMap<String, String>,
    ) -> Result<munin_types::RecoveryVerdict> {
        // 消费一次 probe 序列驱动 achieved 判定（与旧 probe_and_choice 语义对齐）
        let (achieved_r, achieved_c) = self.probe(state, expected_outcome).await?;
        let achieved = (achieved_r, achieved_c);

        // 阻塞与恢复判定：优先序列消费，其次注入值，否则默认无阻塞 + Retry
        let (blocked, recovery) = {
            let mut inner = self.inner.lock();
            let blocked = if !inner.blocked_sequence.is_empty() {
                let idx = inner.blocked_call_count;
                inner.blocked_call_count += 1;
                if idx < inner.blocked_sequence.len() {
                    inner.blocked_sequence[idx]
                } else {
                    *inner.blocked_sequence.last().unwrap()
                }
            } else {
                inner.blocked_override.unwrap_or((false, 0.95))
            };
            (
                blocked,
                inner
                    .recovery_override
                    .unwrap_or((munin_types::RecoveryKind::Retry, 0.90)),
            )
        };

        let target = if criteria.is_empty() {
            None
        } else {
            Some(
                self.choice(state, "哪个元素是推进当前任务目标最匹配的交互项？", criteria)
                    .await?,
            )
        };

        let recovery_target = if criteria.is_empty() {
            None
        } else {
            Some(
                self.choice(state, "恢复动作（如关闭弹窗、解除遮罩）应作用于哪个元素？", criteria)
                    .await?,
            )
        };

        Ok(munin_types::RecoveryVerdict {
            achieved,
            blocked,
            blocker: (munin_types::BlockerClass::None, 0.95),
            recovery,
            target,
            recovery_target,
        })
    }

    async fn match_memory(
        &self,
        _state: &serde_json::Value,
        _entries: &[munin_types::MemoryEntry],
    ) -> Result<Option<(usize, f32)>> {
        Ok(None)
    }
}
