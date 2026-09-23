use anyhow::{anyhow, Result};
use munin_driver::BrowserDriver;
use munin_engine::{FastEngine, SlowEngine};
use munin_perception::{CandidatePruner, OverlayBuster, RecoveryMemory, StateProbe};
use munin_types::{Action, DOMElementNode, ExecutionReport, MacroStep, RecoveryKind, StepExecutionResult};
use serde_json::json;
use std::collections::HashMap;
use tracing::{info, warn};

/// 双引擎编排协调器 (BiSystemSupervisor / Supervisor)
///
/// 利用静态泛型单态化消除虚表开销，管理 System 1 (快引擎) 与 System 2 (慢引擎)
/// 的协同编排、置信度门控仲裁与干扰自愈。
pub struct BiSystemSupervisor<D: BrowserDriver, F: FastEngine, S: SlowEngine> {
    pub driver: D,
    pub fast: F,
    pub slow: S,
    pub confidence_threshold: f32,
    pub max_attempts_per_step: usize,
    pub overlay_buster: OverlayBuster,
    pub candidate_pruner: CandidatePruner,
    pub state_probe: StateProbe,
    pub recovery_memory: RecoveryMemory,
    pub step_delay_ms: u64,
    /// 同一步骤内连续恢复失败上限，超过则上报慢引擎
    pub max_recovery_failures: usize,
    /// 慢引擎仲裁失败时是否启用 heuristic 降级（取首个候选元素）
    pub slow_fallback_heuristic: bool,
}

pub type Supervisor<D, F, S> = BiSystemSupervisor<D, F, S>;

impl<D: BrowserDriver, F: FastEngine, S: SlowEngine> BiSystemSupervisor<D, F, S> {
    /// 创建并装配协调器
    pub fn new(driver: D, fast: F, slow: S, threshold: f32) -> Self {
        let mut memory = RecoveryMemory::new();
        // 记忆库加载失败不阻断启动（降级为空库，等价冷启动）
        if let Err(e) = memory.load() {
            warn!("⚠️ 恢复记忆库加载失败（冷启动）: {e}");
        }
        Self {
            driver,
            fast,
            slow,
            confidence_threshold: threshold,
            max_attempts_per_step: 5,
            overlay_buster: OverlayBuster::default().with_threshold(0.80),
            candidate_pruner: CandidatePruner::default(),
            state_probe: StateProbe::default().with_threshold(threshold),
            recovery_memory: memory,
            step_delay_ms: 100,
            max_recovery_failures: 3,
            slow_fallback_heuristic: false,
        }
    }

    pub fn with_slow_fallback_heuristic(mut self, enabled: bool) -> Self {
        self.slow_fallback_heuristic = enabled;
        self
    }

    /// 使用纯内存记忆库（测试/CI 场景，不读写磁盘）
    pub fn with_in_memory_recovery(mut self) -> Self {
        self.recovery_memory = RecoveryMemory::in_memory();
        self
    }

    pub fn with_max_attempts(mut self, attempts: usize) -> Self {
        self.max_attempts_per_step = attempts;
        self
    }

    pub fn with_step_delay_ms(mut self, delay_ms: u64) -> Self {
        self.step_delay_ms = delay_ms;
        self
    }

    /// 毫秒级弹窗干扰自愈逻辑
    pub async fn heal_overlays(&self) -> Result<()> {
        if let Some(target) = self
            .overlay_buster
            .bust_overlays(&self.driver, &self.fast)
            .await?
        {
            info!("🧹 [OverlayBuster] 已清理干扰弹窗 -> {}", target);
        }
        Ok(())
    }

    /// 从慢引擎仲裁文本中解析目标节点 ID
    ///
    /// 依次尝试：JSON 提取 node_id、整体去引号精确匹配、按候选 ID 子串模糊匹配
    fn resolve_arbitrated_target<'a>(
        directive: &str,
        criteria_map: &'a HashMap<String, String>,
    ) -> Option<&'a str> {
        let trimmed = directive.trim();
        // 1. 慢引擎可能输出 JSON（含 ```json 包裹）：提取 node_id 字段
        let json_candidate = trimmed
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        if let Some(start) = json_candidate.find('{') {
            if let Some(val) = serde_json::from_str::<serde_json::Value>(&json_candidate[start..])
                .ok()
                .and_then(|v| {
                    v.get("node_id")
                        .or_else(|| v.get("target_id"))
                        .and_then(|n| n.as_str().map(|s| s.to_string()))
                })
            {
                if let Some(key) = criteria_map.keys().find(|k| k.as_str() == val) {
                    return Some(key.as_str());
                }
            }
        }
        // 2. 精确匹配（去引号）
        let exact = trimmed.trim_matches('"');
        if let Some(key) = criteria_map.keys().find(|k| k.as_str() == exact) {
            return Some(key.as_str());
        }
        // 3. 子串模糊匹配：候选 ID 出现在仲裁文本中（取最长 ID 避免前缀误匹配）
        criteria_map
            .keys()
            .filter(|k| trimmed.contains(k.as_str()))
            .max_by_key(|k| k.len())
            .map(|k| k.as_str())
    }

    /// 步骤间隔等待（0 时跳过）
    async fn sleep_between_attempts(&self) {
        if self.step_delay_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(self.step_delay_ms)).await;
        }
    }

    /// 导航/刷新后轮询 document.readyState 直至页面就绪（上限 5s）
    async fn wait_page_ready(&self) {
        for _ in 0..25 {
            if let Ok(val) = self.driver.evaluate_js("document.readyState").await {
                if val.as_str() == Some("complete") {
                    return;
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        }
    }

    /// 上报慢引擎仲裁，解析并执行指令
    ///
    /// 返回 Ok(true) 表示慢引擎判定目标已达成；Ok(false) 表示已执行恢复指令但未确认达成。
    async fn escalate_to_slow(
        &self,
        step_intent: &str,
        criteria_map: &HashMap<String, String>,
        actions_taken: &mut Vec<Action>,
    ) -> Result<bool> {
        let screenshot = self.driver.take_screenshot().await.ok();
        let arbitrate_result = self
            .slow
            .arbitrate(
                step_intent,
                &format!("{:?}", criteria_map),
                screenshot.as_deref(),
            )
            .await;

        let fallback_action = match arbitrate_result {
            Ok(action) => action,
            Err(e) => {
                warn!("慢引擎仲裁失败: {e}");
                // 降级策略：heuristic 时取第一个候选元素作为仲裁目标
                if self.use_heuristic_fallback() {
                    if let Some((first_id, _)) = criteria_map.iter().next() {
                        warn!("启用 heuristic 降级: 点击首个候选元素 '{}'", first_id);
                        let target = first_id.clone();
                        self.driver.click(&target).await?;
                        actions_taken.push(Action::Click { target_id: target });
                        let state = json!({ "goal": step_intent });
                        let (achieved, _) = self
                            .fast
                            .probe(&state, &format!("当前页面状态已表明：{}（结果已达成）", step_intent))
                            .await
                            .unwrap_or((false, 0.0));
                        return Ok(achieved);
                    }
                }
                return Err(e);
            }
        };
        info!("🧠 [System 2] 慢引擎仲裁指令: {}", fallback_action);

        if let Some(arbitrated_target) = Self::resolve_arbitrated_target(&fallback_action, criteria_map)
        {
            let target = arbitrated_target.to_string();
            self.driver.click(&target).await?;
            actions_taken.push(Action::Click { target_id: target });
            // 仲裁点击后立刻快引擎探针判定，避免强制等下一轮循环造成死循环
            let state = json!({ "goal": step_intent });
            let (achieved, _) = self
                .fast
                .probe(&state, &format!("当前页面状态已表明：{}（结果已达成）", step_intent))
                .await
                .unwrap_or((false, 0.0));
            return Ok(achieved);
        } else {
            actions_taken.push(Action::Custom {
                name: "slow_arbitrate".to_string(),
                payload: json!({ "directive": fallback_action }),
            });
        }
        Ok(false)
    }

    /// 是否启用 heuristic 降级
    fn use_heuristic_fallback(&self) -> bool {
        self.slow_fallback_heuristic
    }

    /// 执行恢复动作，返回是否产生 DOM 变化（动作生效）
    async fn execute_recovery(
        &self,
        kind: RecoveryKind,
        target: &str,
        elements: &[munin_types::DOMElementNode],
        actions_taken: &mut Vec<Action>,
    ) -> Result<bool> {
        let before_count = elements.len();
        match kind {
            RecoveryKind::Retry => {
                // 重试语义：不执行动作，效果由下一轮恢复图重新评估。
                // 返回 true 避免计为恢复失败（否则 3 次 Retry 误触发上报）
                info!("🔁 [Recovery] 重试上一动作");
                Ok(true)
            }
            RecoveryKind::Scroll => {
                info!("📜 [Recovery] 滚动页面暴露元素");
                self.driver
                    .evaluate_js("window.scrollBy({top: 400, behavior: 'instant'})")
                    .await?;
                actions_taken.push(Action::Scroll { x: 0, y: 400 });
                Ok(true)
            }
            RecoveryKind::Wait => {
                info!("⏳ [Recovery] 等待页面加载");
                tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;
                actions_taken.push(Action::Wait { duration_ms: 1500 });
                Ok(true)
            }
            RecoveryKind::Refresh => {
                info!("🔄 [Recovery] 刷新页面");
                self.driver.evaluate_js("location.reload()").await?;
                actions_taken.push(Action::Evaluate {
                    script: "location.reload()".to_string(),
                });
                self.wait_page_ready().await;
                Ok(true)
            }
            RecoveryKind::Dismiss => {
                info!("🧹 [Recovery] 消解弹窗/遮罩 -> {}", target);
                if target != "page" && elements.iter().any(|e| e.node_id == target) {
                    self.driver.click(target).await?;
                    actions_taken.push(Action::Click {
                        target_id: target.to_string(),
                    });
                    Ok(true)
                } else {
                    // 目标不在当前候选中，走通用弹窗消解
                    let healed = self
                        .overlay_buster
                        .bust_overlays_with(&self.driver, &self.fast, elements)
                        .await?;
                    Ok(healed.is_some())
                }
            }
            RecoveryKind::Back => {
                info!("⬅️ [Recovery] 后退一页");
                self.driver.evaluate_js("history.back()").await?;
                actions_taken.push(Action::Evaluate {
                    script: "history.back()".to_string(),
                });
                self.wait_page_ready().await;
                Ok(true)
            }
            RecoveryKind::Escalate => {
                let _ = before_count;
                Ok(false)
            }
        }
    }

    /// 执行通用自然语言任务目标（兼容 README 核心规范）
    pub async fn execute_goal(&mut self, goal: &str) -> Result<()> {
        let report = self.execute_goal_with_report(goal).await?;
        if !report.overall_success {
            return Err(anyhow!(
                "Goal execution failed for goal '{}'. Incomplete steps recorded.",
                goal
            ));
        }
        Ok(())
    }

    /// 执行通用自然语言任务目标并产出结构化审计报告
    pub async fn execute_goal_with_report(&mut self, goal: &str) -> Result<ExecutionReport> {
        let start_time = std::time::Instant::now();
        info!("🧠 [System 2] 慢引擎进行宏观目标分解: '{}'", goal);

        let steps = self.slow.plan(goal, "Browser ready").await?;
        let (steps_results, overall_success) = self.execute_steps(steps).await?;

        Ok(ExecutionReport {
            goal: goal.to_string(),
            steps: steps_results,
            total_duration_ms: start_time.elapsed().as_millis() as u64,
            overall_success,
        })
    }

    /// 批量执行宏观步骤列表：每步由快引擎本地闭环（达成判定/自愈/点击），直至全部完成或某步失败
    ///
    /// LLM 可先通过 `get_action_map` 拉取修剪后的可操作元素，一次性规划多步后调用本方法，
    /// 避免逐动作 RPC 往返，充分发挥快引擎 10ms 级反射速度。
    pub async fn execute_steps(
        &mut self,
        steps: Vec<MacroStep>,
    ) -> Result<(Vec<StepExecutionResult>, bool)> {
        let mut step_results = Vec::new();
        let mut overall_success = true;

        for step in steps {
            info!("▶ [Step {}] 阶段任务: {}", step.step_id, step.intent);
            let mut completed = false;
            let mut attempts = 0;
            let mut actions_taken = Vec::new();
            let mut last_confidence = 0.0;
            let mut recovery_failures = 0;
            // DOM 变更兜底：记录初始元素指纹
            let initial_dom_hash = {
                let els = self.driver.get_interactive_elements().await?;
                els.iter().map(|e| e.node_id.as_str()).collect::<Vec<_>>().join(",")
            };
            let step_threshold = step.threshold.unwrap_or(self.confidence_threshold);
            let assertion_dom = step.assertion_type.as_deref() == Some("dom");

            while !completed && attempts < self.max_attempts_per_step {
                // 1. 单次抓取交互元素快照 + 页面文本（本轮全部判定共用）
                let mut elements = self.driver.get_interactive_elements().await?;
                let page_text = self.state_probe.snapshot_text(&self.driver).await?;
                // hint 约束：selector 子树过滤 + text 加权注入 intent
                let elements_ref: &[DOMElementNode] = &elements;
                let filtered;
                let intent_str = match &step.hint {
                    Some(hint) => {
                        if let Some(sel) = &hint.selector {
                            let sel_attr = sel.to_lowercase();
                            filtered = elements_ref
                                .iter()
                                .filter(|e| {
                                    // 元素自身或属性命中 selector 关键词（类名/id 模糊包含）
                                    let hay = format!(
                                        "{} {} {}",
                                        e.tag,
                                        e.attributes
                                            .values()
                                            .cloned()
                                            .collect::<Vec<_>>()
                                            .join(" "),
                                        e.node_id
                                    )
                                    .to_lowercase();
                                    sel_attr
                                        .trim_start_matches('.')
                                        .trim_start_matches('#')
                                        .split_whitespace()
                                        .any(|tok| !tok.is_empty() && hay.contains(tok))
                                })
                                .cloned()
                                .collect::<Vec<_>>();
                            if !filtered.is_empty() {
                                elements = filtered;
                            }
                        }
                        match &hint.text {
                            Some(t) if !t.trim().is_empty() => {
                                format!("{} {}", step.intent, t.trim())
                            }
                            _ => step.intent.clone(),
                        }
                    }
                    None => step.intent.clone(),
                };
                let criteria_map = self.candidate_pruner.prune_to_map(&elements, &intent_str);
                let state = json!({
                    "goal": step.intent,
                    "page_text": page_text,
                    "elements": elements.iter().map(|e| e.semantic_summary()).collect::<Vec<_>>(),
                });

                // 2. 单次恢复图推理：对比式达成/阻塞判定 + 故障分类 + 恢复策略 + 目标锁定
                let verdict = self
                    .fast
                    .recovery_graph(&state, &step.expected_outcome, &criteria_map)
                    .await?;

                // 3. 达成判定（对比式已内置阈值裁决）
                if verdict.achieved.0 {
                    info!(
                        "✔ [System 1] 达成断言成功 (置信度: {:.1}%)",
                        verdict.achieved.1 * 100.0
                    );
                    completed = true;
                    last_confidence = verdict.achieved.1;
                    break;
                }

                // 3a. DOM 变更兜底：点击查询等微交互无画面剧变时，DOM 结构变化即视为达成
                if assertion_dom {
                    let current_dom_hash = elements
                        .iter()
                        .map(|e| e.node_id.as_str())
                        .collect::<Vec<_>>()
                        .join(",");
                    if current_dom_hash != initial_dom_hash {
                        info!("✔ [System 1] DOM 变更检测达成 (元素指纹变化)");
                        completed = true;
                        last_confidence = step_threshold;
                        break;
                    }
                }

                // 4. 域外自检：目标锁定与恢复策略置信度双低 = 模型对当前状态无判断能力
                let target_conf = verdict.target.as_ref().map(|(_, c)| *c).unwrap_or(0.0);
                let out_of_domain = target_conf < 0.30 && verdict.recovery.1 < 0.30;
                if out_of_domain {
                    warn!(
                        "⚠️ 快引擎对当前状态无域内判断（target={:.2}, recovery={:.2}），直接上报慢引擎...",
                        target_conf, verdict.recovery.1
                    );
                    match self
                        .escalate_to_slow(&step.intent, &criteria_map, &mut actions_taken)
                        .await
                    {
                        Ok(true) => {
                            completed = true;
                            break;
                        }
                        Ok(false) => {}
                        Err(e) => warn!("慢引擎仲裁失败: {e}"),
                    }
                    attempts += 1;
                    self.sleep_between_attempts().await;
                    continue;
                }

                // 5. 阻塞路径：对比式判定确认存在阻塞
                if verdict.blocked.0 {
                    info!(
                        "🚧 [System 1] 检测到阻塞 (类别: {:?}, 置信度: {:.1}%)",
                        verdict.blocker.0,
                        verdict.blocked.1 * 100.0
                    );

                    // 5a. 优先查对比式记忆库
                    let memory_hit = self
                        .fast
                        .match_memory(&state, &self.recovery_memory.entries)
                        .await?;

                    let (kind, target) = if let Some((idx, conf)) = memory_hit {
                        let entry = &self.recovery_memory.entries[idx];
                        info!(
                            "🧠 [Memory] 命中历史恢复方案 (置信度: {:.1}%): {:?} -> {}",
                            conf * 100.0,
                            entry.recovery,
                            entry.target
                        );
                        (entry.recovery, entry.target.clone())
                    } else {
                        // 5b. 未命中 → 采纳恢复图推理结果（恢复目标独立于业务目标）
                        let target = verdict
                            .recovery_target
                            .as_ref()
                            .map(|(t, _)| t.clone())
                            .unwrap_or_else(|| "page".to_string());
                        (verdict.recovery.0, target)
                    };

                    // 5c. 执行恢复动作
                    let recovered = self
                        .execute_recovery(kind, &target, &elements, &mut actions_taken)
                        .await?;

                    // 5d. 记录结果回写记忆库
                    if recovered {
                        recovery_failures = 0;
                        if let Some((idx, _)) = memory_hit {
                            self.recovery_memory.mark_success(idx);
                        } else if kind != RecoveryKind::Escalate {
                            let app_desc = format!(
                                "当执行「{}」时遇到{:?}阻塞",
                                step.intent, verdict.blocker.0
                            );
                            let inapp_desc = format!(
                                "当执行「{}」时页面无阻塞或阻塞为验证码/付费墙",
                                step.intent
                            );
                            self.recovery_memory.record(
                                app_desc,
                                inapp_desc,
                                kind,
                                &target,
                                true,
                            );
                        }
                    } else {
                        recovery_failures += 1;
                        if let Some((idx, _)) = memory_hit {
                            self.recovery_memory.mark_failure(idx);
                        }
                        warn!(
                            "⚠️ 恢复动作未生效 ({}/{} 次失败)",
                            recovery_failures, self.max_recovery_failures
                        );
                    }

                    // 5e. 连续失败超阈值 → 上报慢引擎
                    if recovery_failures >= self.max_recovery_failures {
                        warn!("🆘 快引擎自愈能力耗尽，上报慢引擎...");
                        match self
                            .escalate_to_slow(&step.intent, &criteria_map, &mut actions_taken)
                            .await
                        {
                            Ok(true) => {
                                completed = true;
                                break;
                            }
                            Ok(false) => recovery_failures = 0,
                            Err(e) => warn!("慢引擎仲裁失败: {e}"),
                        }
                    }

                    attempts += 1;
                    self.sleep_between_attempts().await;
                    continue;
                }

                // 6. 无阻塞正常路径：目标锁定 + 置信度门控
                let Some((target_id, conf)) = verdict.target.clone() else {
                    warn!("⚠️ 无候选元素且未达成，上报慢引擎...");
                    let _ = self
                        .escalate_to_slow(&step.intent, &criteria_map, &mut actions_taken)
                        .await;
                    attempts += 1;
                    self.sleep_between_attempts().await;
                    continue;
                };
                last_confidence = conf;

                if conf >= step_threshold {
                    info!(
                        "⚡ [System 1] 执行点击 -> {} (置信度: {:.1}%)",
                        target_id,
                        conf * 100.0
                    );
                    self.driver.click(&target_id).await?;
                    actions_taken.push(Action::Click {
                        target_id: target_id.clone(),
                    });
                } else {
                    warn!(
                        "⚠️ 快引擎置信度偏低 ({:.1}%)，唤醒慢引擎介入仲裁...",
                        conf * 100.0
                    );
                    let _ = self
                        .escalate_to_slow(&step.intent, &criteria_map, &mut actions_taken)
                        .await;
                }

                attempts += 1;
                self.sleep_between_attempts().await;
            }

            if completed {
                step_results.push(StepExecutionResult::success(
                    step.step_id,
                    last_confidence,
                    actions_taken,
                ));
            } else {
                overall_success = false;
                // 失败诊断快照：URL + 页面文本截断 + 最终置信度 + Top 候选，便于定位脱靶原因
                let page_url = self
                    .driver
                    .evaluate_js("location.href")
                    .await
                    .ok()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                let page_text = self
                    .state_probe
                    .snapshot_text(&self.driver)
                    .await
                    .unwrap_or_default();
                let text_preview: String = page_text.chars().take(500).collect();
                let last_elements = self.driver.get_interactive_elements().await.unwrap_or_default();
                let top_candidates: Vec<serde_json::Value> = self
                    .candidate_pruner
                    .prune(&last_elements, &step.intent)
                    .iter()
                    .take(10)
                    .map(|e| json!({"node_id": e.node_id, "summary": e.semantic_summary()}))
                    .collect();
                let diagnostics = json!({
                    "page_url": page_url,
                    "page_text_preview": text_preview,
                    "final_confidence": last_confidence,
                    "top_candidates": top_candidates,
                    "reason": format!("快引擎置信度未达阈值或慢引擎仲裁失败，{} 次尝试后仍未达成预期", self.max_attempts_per_step),
                });
                step_results.push(
                    StepExecutionResult::failure(
                        step.step_id,
                        format!("Max attempts ({}) exceeded without achieving expected outcome", self.max_attempts_per_step),
                    )
                    .with_diagnostics(diagnostics),
                );
                break;
            }
        }

        // 目标执行结束，持久化恢复记忆库（失败仅告警，不影响执行结果）
        if let Err(e) = self.recovery_memory.save() {
            warn!("⚠️ 恢复记忆库持久化失败: {e}");
        }
        Ok((step_results, overall_success))
    }
}
