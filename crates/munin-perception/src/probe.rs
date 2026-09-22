use anyhow::Result;
use munin_driver::BrowserDriver;
use munin_engine::FastEngine;
use tracing::info;

/// 页面状态与目标断言探针 (StateProbe)
///
/// 替代传统的固定 sleep 等待，利用快引擎的语义断言能力，
/// 10ms 级快速探测页面是否已达到期望的目标状态。
#[derive(Debug, Clone)]
pub struct StateProbe {
    pub max_text_length: usize,
    pub default_confidence_threshold: f32,
}

impl Default for StateProbe {
    fn default() -> Self {
        Self {
            max_text_length: 1000,
            default_confidence_threshold: 0.85,
        }
    }
}

impl StateProbe {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.default_confidence_threshold = threshold;
        self
    }

    /// 提取当前页面文本快照
    pub async fn snapshot_text<D: BrowserDriver>(&self, driver: &D) -> Result<String> {
        let script = format!(
            "(() => {{ const t = (document.body && document.body.innerText) || ''; return t.slice(0, {}); }})()",
            self.max_text_length
        );
        let val = driver.evaluate_js(&script).await?;
        Ok(val.as_str().unwrap_or("").to_string())
    }

    /// 探测页面是否已达成期望目标
    pub async fn probe_outcome<D: BrowserDriver, F: FastEngine>(
        &self,
        driver: &D,
        fast: &F,
        expected_outcome: &str,
    ) -> Result<(bool, f32)> {
        let text_snapshot = self.snapshot_text(driver).await?;
        let assertion = format!("页面内容是否已表明：{}？", expected_outcome);

        let (achieved, prob) = fast
            .probe(&serde_json::json!({ "page_text": text_snapshot }), &assertion)
            .await?;

        if achieved && prob >= self.default_confidence_threshold {
            info!(
                "✔ [StateProbe] 达成断言成功 (置信度: {:.1}%): {}",
                prob * 100.0,
                expected_outcome
            );
        }

        Ok((achieved, prob))
    }
}
