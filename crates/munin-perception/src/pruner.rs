use munin_types::DOMElementNode;
use std::collections::HashMap;

/// 交互候选元素极速修剪器 (Top-K Pruner)
///
/// 消除整页 DOM 树冗余，只保留与当前任务意图最相关的高价值交互节点，
/// 大幅降低 Token 消耗与推理延迟。
#[derive(Debug, Clone)]
pub struct CandidatePruner {
    pub max_candidates: usize,
    pub prioritize_clickable_and_inputs: bool,
}

impl Default for CandidatePruner {
    fn default() -> Self {
        Self {
            max_candidates: 30,
            prioritize_clickable_and_inputs: true,
        }
    }
}

impl CandidatePruner {
    pub fn new(max_candidates: usize) -> Self {
        Self {
            max_candidates,
            prioritize_clickable_and_inputs: true,
        }
    }

    /// 根据当前阶段意图打分并修剪候选元素，返回 Top-K 映射列表
    pub fn prune_to_map(
        &self,
        elements: &[DOMElementNode],
        current_intent: &str,
    ) -> HashMap<String, String> {
        let pruned = self.prune(elements, current_intent);
        let mut map = HashMap::with_capacity(pruned.len());
        for el in pruned {
            map.insert(el.node_id.clone(), el.semantic_summary());
        }
        map
    }

    /// 修剪并按相关性降序排列候选元素
    pub fn prune<'a>(
        &self,
        elements: &'a [DOMElementNode],
        current_intent: &str,
    ) -> Vec<&'a DOMElementNode> {
        let intent_tokens: Vec<String> = current_intent
            .split_whitespace()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();

        // 浮层激活时硬剔除 buried：Top-K 配额全量分配给激活层，避免底层元素挤占截断预算
        let has_active_layer = elements
            .iter()
            .any(|e| e.attributes.get("data-munin-layer").map(|s| s.as_str()) == Some("active"));

        let mut scored: Vec<(i32, &'a DOMElementNode)> = elements
            .iter()
            .filter(|e| {
                if has_active_layer
                    && e.attributes.get("data-munin-layer").map(|s| s.as_str()) == Some("buried")
                {
                    return false;
                }
                if self.prioritize_clickable_and_inputs {
                    e.is_clickable() || e.is_input() || !e.text.trim().is_empty()
                } else {
                    true
                }
            })
            .map(|e| {
                let mut score = 0;
                // 活动图层优先：弹窗/抽屉内元素大幅加权，被遮罩元素降权
                match e.attributes.get("data-munin-layer").map(|s| s.as_str()) {
                    Some("active") => score += 100,
                    Some("buried") => score -= 50,
                    _ => {}
                }
                if e.is_clickable() {
                    score += 5;
                }
                if e.is_input() {
                    score += 5;
                }

                let text_lower = e.text.to_lowercase();
                // CJK 双汉字按钮空格归一化（"确 定" → "确定"）
                let text_squash: String = text_lower.chars().filter(|c| !c.is_whitespace()).collect();
                let id_l = e.attributes.get("id").map(|v| v.to_lowercase());
                let name_l = e.attributes.get("name").map(|v| v.to_lowercase());
                let ph_l = e.attributes.get("placeholder").map(|v| v.to_lowercase());
                let aria_l = e.attributes.get("aria-label").map(|v| v.to_lowercase());
                for token in &intent_tokens {
                    if text_lower.contains(token.as_str()) {
                        score += 15;
                    } else {
                        let token_squash: String = token.chars().filter(|c| !c.is_whitespace()).collect();
                        if !token_squash.is_empty() && text_squash.contains(&token_squash) {
                            score += 15;
                        }
                    }
                    if let Some(id) = &id_l {
                        if id.contains(token.as_str()) {
                            score += 10;
                        }
                    }
                    if let Some(name) = &name_l {
                        if name.contains(token.as_str()) {
                            score += 10;
                        }
                    }
                    if let Some(ph) = &ph_l {
                        if ph.contains(token.as_str()) {
                            score += 10;
                        }
                    }
                    if let Some(aria) = &aria_l {
                        if aria.contains(token.as_str()) {
                            score += 10;
                        }
                    }
                }
                (score, e)
            })
            .collect();

        // 按得分降序排序，相同得分保持顺序稳定
        scored.sort_by_key(|s| std::cmp::Reverse(s.0));

        scored
            .into_iter()
            .take(self.max_candidates)
            .map(|(_, e)| e)
            .collect()
    }
}
