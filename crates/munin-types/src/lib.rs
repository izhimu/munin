pub mod config;
pub mod scenario;
pub use config::{
    BrowserProfileConfig, FastEngineConfig, MuninConfig, RpcServerConfig, SlowEngineConfig,
};
pub use scenario::{
    FillAction, Scenario, ScenarioAction, ScenarioReport, ScenarioStep, StepResult,
};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 轻量化 DOM 交互节点（剥离无关冗余样式，聚焦交互语义）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DOMElementNode {
    pub node_id: String,
    pub tag: String,
    pub text: String,
    #[serde(default)]
    pub attributes: HashMap<String, String>,
}

impl DOMElementNode {
    pub fn new(node_id: impl Into<String>, tag: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            tag: tag.into(),
            text: text.into(),
            attributes: HashMap::new(),
        }
    }

    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }

    pub fn get_attribute(&self, key: &str) -> Option<&str> {
        self.attributes.get(key).map(|s| s.as_str())
    }

    pub fn is_clickable(&self) -> bool {
        let tag = self.tag.to_lowercase();
        if tag == "button" || tag == "a" {
            return true;
        }
        if let Some(role) = self.get_attribute("role") {
            if role.eq_ignore_ascii_case("button") || role.eq_ignore_ascii_case("link") {
                return true;
            }
        }
        self.attributes.contains_key("onclick")
    }

    pub fn is_input(&self) -> bool {
        let tag = self.tag.to_lowercase();
        tag == "input" || tag == "textarea" || tag == "select"
    }

    pub fn semantic_summary(&self) -> String {
        let tag = self.tag.to_lowercase();
        let text = self.text.trim();
        if text.is_empty() {
            if let Some(aria) = self.get_attribute("aria-label") {
                format!("<{} aria='{}'>", tag, aria)
            } else if let Some(ph) = self.get_attribute("placeholder") {
                format!("<{} placeholder='{}'>", tag, ph)
            } else {
                format!("<{}>", tag)
            }
        } else {
            format!("<{}> {}", tag, text)
        }
    }
}

/// 慢引擎规划的宏观步骤
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MacroStep {
    pub step_id: usize,
    pub intent: String,
    pub expected_outcome: String,
    /// 达成判定方式：`visual`（默认，Laya 反射探针）| `dom`（DOM 变更检测兜底）
    #[serde(default)]
    pub assertion_type: Option<String>,
    /// 步骤级置信度阈值，覆盖全局阈值
    #[serde(default)]
    pub threshold: Option<f32>,
    /// 候选命中线索：约束快引擎反射范围（text 模糊匹配 / selector CSS 子树）
    #[serde(default)]
    pub hint: Option<StepHint>,
}

/// 步骤候选命中线索
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepHint {
    /// 文本模糊匹配（如 "在岗"），候选元素 text/aria/placeholder 含此词时加权
    #[serde(default)]
    pub text: Option<String>,
    /// CSS 选择器约束（如 ".statistic-card"），仅其子孙节点参与反射
    #[serde(default)]
    pub selector: Option<String>,
}

impl MacroStep {
    pub fn new(
        step_id: usize,
        intent: impl Into<String>,
        expected_outcome: impl Into<String>,
    ) -> Self {
        Self {
            step_id,
            intent: intent.into(),
            expected_outcome: expected_outcome.into(),
            assertion_type: None,
            threshold: None,
            hint: None,
        }
    }

    pub fn with_assertion_type(mut self, assertion_type: impl Into<String>) -> Self {
        self.assertion_type = Some(assertion_type.into());
        self
    }

    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.threshold = Some(threshold);
        self
    }

    pub fn with_hint(mut self, text: Option<String>, selector: Option<String>) -> Self {
        self.hint = Some(StepHint { text, selector });
        self
    }
}

/// 快引擎决策输出原语
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FastDecision {
    Choice { target_key: String, confidence: f32 },
    Score { score: f32, confidence: f32 },
    Probe { result: bool, probability: f32 },
}

/// 浏览器交互原语动作
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Click { target_id: String },
    Fill { target_id: String, text: String },
    Scroll { x: i32, y: i32 },
    Wait { duration_ms: u64 },
    Navigate { url: String },
    Evaluate { script: String },
    Custom { name: String, payload: serde_json::Value },
}

/// 单个宏观步骤执行结果
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepExecutionResult {
    pub step_id: usize,
    pub success: bool,
    pub actions_taken: Vec<Action>,
    pub final_confidence: f32,
    pub message: Option<String>,
    /// 失败时刻诊断快照：页面 URL、可见文本截断、Top 候选元素、最终置信度
    #[serde(default)]
    pub diagnostics: Option<serde_json::Value>,
}

impl StepExecutionResult {
    pub fn success(step_id: usize, confidence: f32, actions: Vec<Action>) -> Self {
        Self {
            step_id,
            success: true,
            actions_taken: actions,
            final_confidence: confidence,
            message: None,
            diagnostics: None,
        }
    }

    pub fn failure(step_id: usize, message: impl Into<String>) -> Self {
        Self {
            step_id,
            success: false,
            actions_taken: Vec::new(),
            final_confidence: 0.0,
            message: Some(message.into()),
            diagnostics: None,
        }
    }

    /// 附加失败诊断快照
    pub fn with_diagnostics(mut self, diagnostics: serde_json::Value) -> Self {
        self.diagnostics = Some(diagnostics);
        self
    }
}

/// 整体目标执行报告
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReport {
    pub goal: String,
    pub steps: Vec<StepExecutionResult>,
    pub total_duration_ms: u64,
    pub overall_success: bool,
}

/// 阻塞类别（故障分类）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BlockerClass {
    /// 营销弹窗或遮罩层
    Overlay,
    /// 页面/数据加载中
    Loading,
    /// 需要登录或权限
    Auth,
    /// 验证码挑战
    Captcha,
    /// 付费墙
    Paywall,
    /// 元素过期/页面无响应
    Stale,
    /// 无阻塞
    None,
}

impl BlockerClass {
    /// 全量候选（供快引擎 choice criteria 构造）
    pub fn all() -> &'static [(&'static str, BlockerClass, &'static str)] {
        &[
            ("overlay", BlockerClass::Overlay, "营销弹窗或遮罩层"),
            ("loading", BlockerClass::Loading, "页面或数据仍在加载中"),
            ("auth", BlockerClass::Auth, "需要登录或权限不足"),
            ("captcha", BlockerClass::Captcha, "出现验证码挑战"),
            ("paywall", BlockerClass::Paywall, "付费墙阻断内容"),
            ("stale", BlockerClass::Stale, "元素过期或页面无响应"),
            ("none", BlockerClass::None, "无任何阻塞"),
        ]
    }
}

/// 恢复动作类别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RecoveryKind {
    /// 重试上一动作
    Retry,
    /// 滚动页面暴露元素
    Scroll,
    /// 等待加载完成
    Wait,
    /// 刷新页面
    Refresh,
    /// 关闭弹窗/遮罩
    Dismiss,
    /// 后退一页
    Back,
    /// 上报慢引擎
    Escalate,
}

impl RecoveryKind {
    pub fn all() -> &'static [(&'static str, RecoveryKind, &'static str)] {
        &[
            ("retry", RecoveryKind::Retry, "重试上一交互动作"),
            ("scroll", RecoveryKind::Scroll, "滚动页面以暴露目标元素"),
            ("wait", RecoveryKind::Wait, "等待页面加载完成"),
            ("refresh", RecoveryKind::Refresh, "刷新当前页面"),
            ("dismiss", RecoveryKind::Dismiss, "关闭弹窗或遮罩"),
            ("back", RecoveryKind::Back, "后退到上一页面"),
            ("escalate", RecoveryKind::Escalate, "无法自愈，上报慢引擎"),
        ]
    }
}

/// 单次恢复图推理的完整判定结果
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecoveryVerdict {
    /// 目标达成判定（对比式：pos 高且 neg 低才为 true）
    pub achieved: (bool, f32),
    /// 阻塞判定（对比式）
    pub blocked: (bool, f32),
    /// 阻塞类别与置信度
    pub blocker: (BlockerClass, f32),
    /// 建议恢复动作与置信度
    pub recovery: (RecoveryKind, f32),
    /// 交互目标锁定（有候选时）：目标节点 ID 与置信度
    pub target: std::option::Option<(String, f32)>,
    /// 恢复动作作用目标（如弹窗关闭按钮）：目标节点 ID 与置信度
    pub recovery_target: std::option::Option<(String, f32)>,
}

/// 对比式记忆条目：故障的正反双向描述 + 有效恢复动作
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// 适用场景描述："当页面出现 ant-design Modal 遮罩且关闭按钮可见时"
    pub applicable_desc: String,
    /// 不适用边界描述："当页面无遮罩或弹窗为验证码时"
    pub inapplicable_desc: String,
    /// 历史验证有效的恢复动作类别
    pub recovery: RecoveryKind,
    /// 恢复动作作用目标（元素 ID 或 "page"）
    pub target: String,
    pub success_count: u32,
    pub failure_count: u32,
}

impl MemoryEntry {
    pub fn success_rate(&self) -> f32 {
        let total = self.success_count + self.failure_count;
        if total == 0 {
            return 0.0;
        }
        self.success_count as f32 / total as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dom_node_basics() {
        let node = DOMElementNode::new("btn-1", "button", "Submit")
            .with_attribute("role", "button")
            .with_attribute("type", "submit");

        assert_eq!(node.node_id, "btn-1");
        assert!(node.is_clickable());
        assert!(!node.is_input());
        assert_eq!(node.semantic_summary(), "<button> Submit");
    }

    #[test]
    fn test_macro_step_serialization() {
        let step = MacroStep::new(1, "Click submit", "Page redirects to dashboard");
        let json = serde_json::to_string(&step).unwrap();
        let deserialized: MacroStep = serde_json::from_str(&json).unwrap();
        assert_eq!(step, deserialized);
    }

    #[test]
    fn test_fast_decision_variants() {
        let c = FastDecision::Choice {
            target_key: "node-42".to_string(),
            confidence: 0.95,
        };
        let p = FastDecision::Probe {
            result: true,
            probability: 0.92,
        };
        let s = FastDecision::Score {
            score: 4.5,
            confidence: 0.88,
        };
        assert!(matches!(c, FastDecision::Choice { .. }));
        assert!(matches!(p, FastDecision::Probe { .. }));
        assert!(matches!(s, FastDecision::Score { .. }));
    }
}
