use anyhow::Result;
use munin_driver::BrowserDriver;
use munin_engine::FastEngine;
use munin_types::DOMElementNode;
use serde_json::json;
use std::collections::HashMap;
use tracing::info;

/// 毫秒级干扰自愈探针 (OverlayBuster)
///
/// 针对现代 Web 页面突发的 Cookie 授权、广告弹窗、新手引导遮罩层，
/// 在 100ms 内自动识别并进行条件反射式无感消解。
#[derive(Debug, Clone)]
pub struct OverlayBuster {
    pub dismiss_keywords: Vec<String>,
    pub confidence_threshold: f32,
}

impl Default for OverlayBuster {
    fn default() -> Self {
        Self {
            dismiss_keywords: vec![
                "关闭".into(),
                "跳过".into(),
                "拒绝".into(),
                "我知道了".into(),
                "同意".into(),
                "确定".into(),
                "取消".into(),
                "暂不".into(),
                "Close".into(),
                "Dismiss".into(),
                "Accept".into(),
                "Reject".into(),
                "Cancel".into(),
                "Skip".into(),
                "Got it".into(),
                "Later".into(),
            ],
            confidence_threshold: 0.80,
        }
    }
}

impl OverlayBuster {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_keywords(mut self, keywords: Vec<String>) -> Self {
        self.dismiss_keywords = keywords;
        self
    }

    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.confidence_threshold = threshold;
        self
    }

    /// 从当前候选元素中筛选潜在的弹窗消解按钮
    pub fn find_overlay_candidates<'a>(
        &self,
        elements: &'a [DOMElementNode],
    ) -> Vec<&'a DOMElementNode> {
        elements
            .iter()
            .filter(|e| {
                let text = e.text.trim();
                let aria = e.get_attribute("aria-label").unwrap_or("");
                self.dismiss_keywords.iter().any(|k| {
                    text.contains(k)
                        || aria.contains(k)
                        || text.eq_ignore_ascii_case(k)
                        || aria.eq_ignore_ascii_case(k)
                })
            })
            .collect()
    }

    /// 执行弹窗自愈探针检测与自动清理
    pub async fn bust_overlays<D: BrowserDriver, F: FastEngine>(
        &self,
        driver: &D,
        fast: &F,
    ) -> Result<Option<String>> {
        let elements = driver.get_interactive_elements().await?;
        let candidates = self.find_overlay_candidates(&elements);

        if candidates.is_empty() {
            return Ok(None);
        }

        let mut criteria = HashMap::new();
        for c in &candidates {
            criteria.insert(
                c.node_id.clone(),
                format!("{} [{}]", c.tag, c.text.trim()),
            );
        }

        let (target_id, conf) = fast
            .choice(
                &json!({ "context": "Modal overlay or popup present" }),
                "Which button closes or dismisses the overlay?",
                &criteria,
            )
            .await?;

        if conf >= self.confidence_threshold {
            info!(
                "⚡ [OverlayBuster] 弹窗自愈：自动清理遮罩层 -> {} (置信度: {:.1}%)",
                target_id,
                conf * 100.0
            );
            driver.click(&target_id).await?;
            Ok(Some(target_id))
        } else {
            Ok(None)
        }
    }
}
