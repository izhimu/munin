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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacroStep {
    pub step_id: usize,
    pub intent: String,
    pub expected_outcome: String,
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
        }
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
}

impl StepExecutionResult {
    pub fn success(step_id: usize, confidence: f32, actions: Vec<Action>) -> Self {
        Self {
            step_id,
            success: true,
            actions_taken: actions,
            final_confidence: confidence,
            message: None,
        }
    }

    pub fn failure(step_id: usize, message: impl Into<String>) -> Self {
        Self {
            step_id,
            success: false,
            actions_taken: Vec::new(),
            final_confidence: 0.0,
            message: Some(message.into()),
        }
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
