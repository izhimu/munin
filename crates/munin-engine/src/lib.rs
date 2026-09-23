use anyhow::Result;
use async_trait::async_trait;
use munin_types::{MacroStep, MemoryEntry, RecoveryVerdict};
use std::collections::HashMap;

pub mod fast;
pub mod slow;

pub use fast::{LayaFastEngine, MockFastEngine};
pub use slow::{MockSlowEngine, OpenAISlowEngine, RpcSlowEngine};

/// 快引擎接口契约 (System 1)：低延迟、确定性输出、置信度统计
#[async_trait]
pub trait FastEngine: Send + Sync {
    /// 单选决策：分类/目标点击选择
    async fn choice(
        &self,
        state: &serde_json::Value,
        instructions: &str,
        criteria: &HashMap<String, String>,
    ) -> Result<(String, f32)>;

    /// 序数评分决策：等级评估/情绪/危害打分
    async fn score(
        &self,
        state: &serde_json::Value,
        instructions: &str,
        criteria: &[String],
    ) -> Result<(f32, f32)>;

    /// 布尔断言决策 (noul/probe)：真假/达成概率探测
    async fn probe(
        &self,
        state: &serde_json::Value,
        assertion: &str,
    ) -> Result<(bool, f32)>;

    /// 单请求融合探测与选择：一次前向推理同时完成状态断言与目标锁定
    ///
    /// 默认实现为顺序调用 `probe` + `choice`（两次请求）；
    /// 支持批量问答的引擎（如 Laya）应覆盖此方法合并为单次请求以降低往返延迟。
    async fn probe_and_choice(
        &self,
        state: &serde_json::Value,
        assertion: &str,
        instructions: &str,
        criteria: &HashMap<String, String>,
    ) -> Result<((bool, f32), (String, f32))> {
        let probe = self.probe(state, assertion).await?;
        let choice = self.choice(state, instructions, criteria).await?;
        Ok((probe, choice))
    }

    /// 对比式布尔判定：正反双探针抵消 noul 顺从偏置
    ///
    /// 正向断言 pos 与反向断言 neg 同批推理；
    /// 采纳条件：pos ≥ 阈值 且 neg ≤ (1 - 阈值)；冲突时返回保守值 false。
    /// 返回 (判定结果, 置信度 = min(pos, 1-neg))。
    async fn contrastive_probe(
        &self,
        state: &serde_json::Value,
        positive: &str,
        negative: &str,
        threshold: f32,
    ) -> Result<(bool, f32)> {
        let (pos_r, pos_c) = self.probe(state, positive).await?;
        let (neg_r, neg_c) = self.probe(state, negative).await?;
        let pos = if pos_r { pos_c } else { 1.0 - pos_c };
        let neg = if neg_r { neg_c } else { 1.0 - neg_c };
        let decided = pos >= threshold && neg <= (1.0 - threshold);
        Ok((decided, pos.min(1.0 - neg)))
    }

    /// 单次请求完成恢复图推理：达成判定 + 阻塞判定 + 故障分类 + 恢复策略 + 目标锁定
    ///
    /// 默认实现顺序调用对比探针与 choice（多次请求）；
    /// Laya 等支持批量问答的引擎应覆盖为单次前向推理。
    /// `criteria` 非空时附带目标锁定；空时 target 为 None。
    async fn recovery_graph(
        &self,
        state: &serde_json::Value,
        expected_outcome: &str,
        criteria: &HashMap<String, String>,
    ) -> Result<RecoveryVerdict> {
        let achieved = self
            .contrastive_probe(
                state,
                &format!("当前页面状态已表明：{}（结果已达成）", expected_outcome),
                &format!("当前页面仍未达成：{}（结果未出现）", expected_outcome),
                0.85,
            )
            .await?;
        let blocked = self
            .contrastive_probe(
                state,
                "当前页面存在阻止目标交互的遮挡层、模态弹窗、加载中或权限墙",
                "当前页面完全无遮挡，所有元素可正常交互",
                0.80,
            )
            .await?;

        use munin_types::{BlockerClass, RecoveryKind};
        let blocker_map: HashMap<String, String> = BlockerClass::all()
            .iter()
            .map(|(k, _, d)| (k.to_string(), d.to_string()))
            .collect();
        let (bk, bc) = self
            .choice(state, "当前阻塞属于哪一类别？", &blocker_map)
            .await?;
        let blocker = (
            BlockerClass::all()
                .iter()
                .find(|(k, _, _)| *k == bk)
                .map(|(_, v, _)| *v)
                .unwrap_or(BlockerClass::None),
            bc,
        );

        let recovery_map: HashMap<String, String> = RecoveryKind::all()
            .iter()
            .map(|(k, _, d)| (k.to_string(), d.to_string()))
            .collect();
        let (rk, rc) = self
            .choice(state, "应采取何种恢复动作解除阻塞并推进目标？", &recovery_map)
            .await?;
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
            Some(
                self.choice(state, "哪个元素是推进当前任务目标最匹配的交互项？", criteria)
                    .await?,
            )
        };

        let recovery_target = if criteria.is_empty() {
            None
        } else {
            Some(
                self.choice(state, "恢复动作（如关闭弹窗）应作用于哪个元素？", criteria)
                    .await?,
            )
        };

        Ok(RecoveryVerdict {
            achieved,
            blocked,
            blocker,
            recovery,
            target,
            recovery_target,
        })
    }

    /// 对比式记忆匹配：对全部历史条目批量正反探针判定，返回最佳命中条目索引与置信度
    async fn match_memory(
        &self,
        state: &serde_json::Value,
        entries: &[MemoryEntry],
    ) -> Result<Option<(usize, f32)>> {
        let mut best: Option<(usize, f32)> = None;
        for (idx, entry) in entries.iter().enumerate() {
            let (hit, conf) = self
                .contrastive_probe(state, &entry.applicable_desc, &entry.inapplicable_desc, 0.80)
                .await?;
            if hit && entry.success_rate() >= 0.5
                && best.map(|(_, c)| conf > c).unwrap_or(true) {
                    best = Some((idx, conf));
                }
        }
        Ok(best)
    }
}

/// 慢引擎接口契约 (System 2)：宏观分解、策略调整、疑难仲裁
#[async_trait]
pub trait SlowEngine: Send + Sync {
    /// 宏观目标拆解为执行步骤流
    async fn plan(&self, user_goal: &str, context: &str) -> Result<Vec<MacroStep>>;

    /// 异常仲裁：在快引擎置信度偏低或执行受阻时介入决策
    async fn arbitrate(
        &self,
        step_intent: &str,
        current_dom_desc: &str,
        screenshot: Option<&[u8]>,
    ) -> Result<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_fast_engine_contract() -> Result<()> {
        let engine = MockFastEngine::new()
            .with_choice("node-click-me", 0.96)
            .with_probe(true, 0.98);

        let mut crit = HashMap::new();
        crit.insert("node-click-me".to_string(), "Button [Click Me]".to_string());

        let (chosen, conf) = engine
            .choice(
                &serde_json::json!({ "goal": "submit form" }),
                "Which button to click?",
                &crit,
            )
            .await?;
        assert_eq!(chosen, "node-click-me");
        assert!(conf > 0.90);

        let (probe_ok, prob) = engine
            .probe(
                &serde_json::json!("Success page"),
                "Is the form successfully submitted?",
            )
            .await?;
        assert!(probe_ok);
        assert!(prob > 0.90);

        Ok(())
    }

    #[tokio::test]
    async fn test_mock_slow_engine_contract() -> Result<()> {
        let engine = MockSlowEngine::new().with_steps(vec![
            MacroStep::new(1, "Navigate to login", "Login page loaded"),
            MacroStep::new(2, "Enter credentials", "Dashboard visible"),
        ]);

        let steps = engine.plan("Login to system", "Clean session").await?;
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].step_id, 1);
        assert_eq!(steps[1].expected_outcome, "Dashboard visible");

        let arbitration = engine.arbitrate("Stuck on captcha", "DOM snippet", None).await?;
        assert_eq!(arbitration, "btn-confirm");

        Ok(())
    }
}
