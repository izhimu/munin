use anyhow::{anyhow, Result};
use munin_driver::BrowserDriver;
use munin_engine::{FastEngine, SlowEngine};
use munin_perception::{CandidatePruner, OverlayBuster, StateProbe};
use munin_types::{Action, ExecutionReport, StepExecutionResult};
use serde_json::json;
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
    pub step_delay_ms: u64,
}

pub type Supervisor<D, F, S> = BiSystemSupervisor<D, F, S>;

impl<D: BrowserDriver, F: FastEngine, S: SlowEngine> BiSystemSupervisor<D, F, S> {
    /// 创建并装配协调器
    pub fn new(driver: D, fast: F, slow: S, threshold: f32) -> Self {
        Self {
            driver,
            fast,
            slow,
            confidence_threshold: threshold,
            max_attempts_per_step: 5,
            overlay_buster: OverlayBuster::default().with_threshold(0.80),
            candidate_pruner: CandidatePruner::default(),
            state_probe: StateProbe::default().with_threshold(threshold),
            step_delay_ms: 100,
        }
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
        let _ = self
            .overlay_buster
            .bust_overlays(&self.driver, &self.fast)
            .await?;
        Ok(())
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
        let mut step_results = Vec::new();
        let mut overall_success = true;

        for step in steps {
            info!("▶ [Step {}] 阶段任务: {}", step.step_id, step.intent);
            let mut completed = false;
            let mut attempts = 0;
            let mut actions_taken = Vec::new();
            let mut last_confidence = 0.0;

            while !completed && attempts < self.max_attempts_per_step {
                // 1. 毫秒级自愈环境（清理弹窗干扰）
                self.heal_overlays().await?;

                // 2. 状态探针：检测是否已达成阶段目标
                let (achieved, prob) = self
                    .state_probe
                    .probe_outcome(&self.driver, &self.fast, &step.expected_outcome)
                    .await?;

                if achieved && prob >= self.confidence_threshold {
                    info!(
                        "✔ [System 1] 达成断言成功 (置信度: {:.1}%)",
                        prob * 100.0
                    );
                    completed = true;
                    last_confidence = prob;
                    break;
                }

                // 3. 提取候选交互元素并执行语义修剪 (Top-K)
                let elements = self.driver.get_interactive_elements().await?;
                let criteria_map = self.candidate_pruner.prune_to_map(&elements, &step.intent);

                if criteria_map.is_empty() {
                    warn!("⚠️ 未发现与当前阶段意图相关的可交互候选元素，尝试唤醒慢引擎介入...");
                    let screenshot = self.driver.take_screenshot().await.ok();
                    let fallback_action = self
                        .slow
                        .arbitrate(&step.intent, "No candidate elements visible", screenshot.as_deref())
                        .await?;
                    info!("🧠 [System 2] 慢引擎应急仲裁指令: {}", fallback_action);
                    attempts += 1;
                    if self.step_delay_ms > 0 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(self.step_delay_ms)).await;
                    }
                    continue;
                }

                // 4. 快引擎 10ms 锁定操作目标
                let (target_id, conf) = self
                    .fast
                    .choice(
                        &json!({ "goal": step.intent }),
                        "哪个元素是推进当前任务目标最匹配的交互项？",
                        &criteria_map,
                    )
                    .await?;

                last_confidence = conf;

                // 5. 置信度门控仲裁
                if conf >= self.confidence_threshold {
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
                    let screenshot = self.driver.take_screenshot().await.ok();
                    let fallback_action = self
                        .slow
                        .arbitrate(
                            &step.intent,
                            &format!("{:?}", criteria_map),
                            screenshot.as_deref(),
                        )
                        .await?;

                    info!("🧠 [System 2] 慢引擎仲裁指令: {}", fallback_action);
                    // 若慢引擎返回具体存在的候选节点 ID，则执行对应交互
                    let arbitrated_target = fallback_action.trim().trim_matches('"');
                    if criteria_map.contains_key(arbitrated_target) {
                        self.driver.click(arbitrated_target).await?;
                        actions_taken.push(Action::Click {
                            target_id: arbitrated_target.to_string(),
                        });
                    } else {
                        // 记录自定义仲裁动作
                        actions_taken.push(Action::Custom {
                            name: "slow_arbitrate".to_string(),
                            payload: json!({ "directive": fallback_action }),
                        });
                    }
                }

                attempts += 1;
                if self.step_delay_ms > 0 {
                    tokio::time::sleep(tokio::time::Duration::from_millis(self.step_delay_ms)).await;
                }
            }

            if completed {
                step_results.push(StepExecutionResult::success(
                    step.step_id,
                    last_confidence,
                    actions_taken,
                ));
            } else {
                overall_success = false;
                step_results.push(StepExecutionResult::failure(
                    step.step_id,
                    format!("Max attempts ({}) exceeded without achieving expected outcome", self.max_attempts_per_step),
                ));
                break;
            }
        }

        let total_duration_ms = start_time.elapsed().as_millis() as u64;
        Ok(ExecutionReport {
            goal: goal.to_string(),
            steps: step_results,
            total_duration_ms,
            overall_success,
        })
    }
}
