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
        let intent_tokens: Vec<&str> = current_intent
            .split_whitespace()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();

        let mut scored: Vec<(i32, &'a DOMElementNode)> = elements
            .iter()
            .filter(|e| {
                if self.prioritize_clickable_and_inputs {
                    e.is_clickable() || e.is_input() || !e.text.trim().is_empty()
                } else {
                    true
                }
            })
            .map(|e| {
                let mut score = 0;
                if e.is_clickable() {
                    score += 5;
                }
                if e.is_input() {
                    score += 5;
                }

                let text_lower = e.text.to_lowercase();
                for token in &intent_tokens {
                    let token_lower = token.to_lowercase();
                    if text_lower.contains(&token_lower) {
                        score += 15;
                    }
                    if let Some(id) = e.attributes.get("id") {
                        if id.to_lowercase().contains(&token_lower) {
                            score += 10;
                        }
                    }
                    if let Some(name) = e.attributes.get("name") {
                        if name.to_lowercase().contains(&token_lower) {
                            score += 10;
                        }
                    }
                    if let Some(ph) = e.attributes.get("placeholder") {
                        if ph.to_lowercase().contains(&token_lower) {
                            score += 10;
                        }
                    }
                    if let Some(aria) = e.attributes.get("aria-label") {
                        if aria.to_lowercase().contains(&token_lower) {
                            score += 10;
                        }
                    }
                }
                (score, e)
            })
            .collect();

        // 按得分降序排序，相同得分保持顺序稳定
        scored.sort_by(|a, b| b.0.cmp(&a.0));

        scored
            .into_iter()
            .take(self.max_candidates)
            .map(|(_, e)| e)
            .collect()
    }
}
