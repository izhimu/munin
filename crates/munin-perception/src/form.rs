use anyhow::Result;
use munin_engine::FastEngine;
use munin_types::DOMElementNode;
use serde_json::json;
use std::collections::HashMap;

/// 表单意图自适应对齐器 (FormMapper)
///
/// 自动将业务字段（如“用户名”、“手机号”、“搜索关键词”）映射到对应的 input / textarea 交互节点。
#[derive(Debug, Clone, Default)]
pub struct FormMapper;

impl FormMapper {
    pub fn new() -> Self {
        Self
    }

    /// 启发式规则匹配表单输入项
    pub fn match_field_heuristically<'a>(
        &self,
        elements: &'a [DOMElementNode],
        field_intent: &str,
    ) -> Option<&'a DOMElementNode> {
        let field_lower = field_intent.to_lowercase();
        elements.iter().find(|e| {
            if !e.is_input() {
                return false;
            }
            if let Some(id) = e.attributes.get("id") {
                if id.to_lowercase().contains(&field_lower) {
                    return true;
                }
            }
            if let Some(name) = e.attributes.get("name") {
                if name.to_lowercase().contains(&field_lower) {
                    return true;
                }
            }
            if let Some(placeholder) = e.attributes.get("placeholder") {
                if placeholder.to_lowercase().contains(&field_lower) {
                    return true;
                }
            }
            if let Some(aria) = e.attributes.get("aria-label") {
                if aria.to_lowercase().contains(&field_lower) {
                    return true;
                }
            }
            e.text.to_lowercase().contains(&field_lower)
        })
    }

    /// 借助快引擎语义匹配最适表单输入项
    pub async fn match_field_with_engine<F: FastEngine>(
        &self,
        elements: &[DOMElementNode],
        field_intent: &str,
        fast: &F,
    ) -> Result<Option<(String, f32)>> {
        let inputs: Vec<&DOMElementNode> = elements.iter().filter(|e| e.is_input()).collect();
        if inputs.is_empty() {
            return Ok(None);
        }

        let mut criteria = HashMap::new();
        for inp in &inputs {
            criteria.insert(inp.node_id.clone(), inp.semantic_summary());
        }

        let instructions = format!("哪个输入框是用于输入 '{}' 的？", field_intent);
        let (target_id, conf) = fast
            .choice(&json!({ "field": field_intent }), &instructions, &criteria)
            .await?;

        Ok(Some((target_id, conf)))
    }
}
