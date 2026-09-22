use anyhow::Result;
use async_trait::async_trait;
use munin_types::MacroStep;
use std::collections::HashMap;

pub mod fast;
pub mod slow;

pub use fast::{LayaFastEngine, MockFastEngine};
pub use slow::{MockSlowEngine, OpenAISlowEngine};

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
