use anyhow::Result;
use async_trait::async_trait;
use munin_types::MacroStep;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use crate::SlowEngine;

#[derive(Debug, Default)]
struct MockSlowInner {
    default_steps: Vec<MacroStep>,
    plan_map: HashMap<String, Vec<MacroStep>>,
    default_arbitration: String,
    arbitration_map: HashMap<String, String>,
}

/// 内存模拟慢引擎（用于单元测试与确定性回归测试）
#[derive(Debug, Clone, Default)]
pub struct MockSlowEngine {
    inner: Arc<Mutex<MockSlowInner>>,
}

impl MockSlowEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_steps(self, steps: Vec<MacroStep>) -> Self {
        self.set_default_steps(steps);
        self
    }

    pub fn set_default_steps(&self, steps: Vec<MacroStep>) {
        let mut inner = self.inner.lock();
        inner.default_steps = steps;
    }

    pub fn map_plan(&self, goal_substring: &str, steps: Vec<MacroStep>) {
        let mut inner = self.inner.lock();
        inner.plan_map.insert(goal_substring.to_string(), steps);
    }

    pub fn set_default_arbitration(&self, arbitration: impl Into<String>) {
        let mut inner = self.inner.lock();
        inner.default_arbitration = arbitration.into();
    }

    pub fn map_arbitration(&self, intent_substring: &str, arbitration: impl Into<String>) {
        let mut inner = self.inner.lock();
        inner
            .arbitration_map
            .insert(intent_substring.to_string(), arbitration.into());
    }
}

#[async_trait]
impl SlowEngine for MockSlowEngine {
    async fn plan(&self, user_goal: &str, _context: &str) -> Result<Vec<MacroStep>> {
        let inner = self.inner.lock();
        for (k, v) in &inner.plan_map {
            if user_goal.contains(k) {
                return Ok(v.clone());
            }
        }
        if !inner.default_steps.is_empty() {
            return Ok(inner.default_steps.clone());
        }
        // Fallback default step
        Ok(vec![MacroStep::new(
            1,
            user_goal,
            "Target page state achieved",
        )])
    }

    async fn arbitrate(
        &self,
        step_intent: &str,
        _current_dom_desc: &str,
        _screenshot: Option<&[u8]>,
    ) -> Result<String> {
        let inner = self.inner.lock();
        for (k, v) in &inner.arbitration_map {
            if step_intent.contains(k) {
                return Ok(v.clone());
            }
        }
        if !inner.default_arbitration.is_empty() {
            return Ok(inner.default_arbitration.clone());
        }
        Ok("btn-confirm".to_string())
    }
}
