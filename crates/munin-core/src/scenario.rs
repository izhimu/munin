use anyhow::{anyhow, Result};
use munin_driver::BrowserDriver;
use munin_engine::FastEngine;
pub use munin_types::scenario::{
    FillAction, Scenario, ScenarioAction, ScenarioReport, ScenarioStep, StepResult,
};
use std::path::Path;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// 通用浏览器流程与场景执行器
pub struct ScenarioRunner<'a, D: BrowserDriver, F: FastEngine> {
    pub driver: &'a mut D,
    pub fast: &'a F,
}

impl<'a, D: BrowserDriver, F: FastEngine> ScenarioRunner<'a, D, F> {
    pub fn new(driver: &'a mut D, fast: &'a F) -> Self {
        Self { driver, fast }
    }

    /// 执行完整场景流程
    pub async fn run(&mut self, scenario: &Scenario) -> Result<ScenarioReport> {
        let total_start = Instant::now();
        info!("▶ 开始执行通用浏览器脚本: '{}'", scenario.name);

        // 1. 若配置初始 base_url 且首个步骤未以 Navigate 开头，则预先跳转
        if let Some(target_url) = &scenario.base_url {
            let first_step_navigates = scenario
                .steps
                .first()
                .map(|s| {
                    s.navigate.is_some()
                        || s.actions
                            .first()
                            .map(|a| matches!(a, ScenarioAction::Navigate { .. }))
                            .unwrap_or(false)
                })
                .unwrap_or(false);

            if !first_step_navigates {
                info!("🔗 导航至基础入口: {}", target_url);
                self.driver.goto(target_url).await?;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }

        let mut step_results = Vec::new();
        let mut passed_steps = 0;
        let mut failed_steps = 0;

        // 2. 依次执行所有步骤 (登录、查询、操作均为标准步骤)
        for (idx, step) in scenario.steps.iter().enumerate() {
            let step_start = Instant::now();
            info!("  [步骤 {}/{}] 执行: {}", idx + 1, scenario.steps.len(), step.name);

            match self.execute_step(step, &scenario.variables, scenario.base_url.as_deref()).await {
                Ok(_) => {
                    let elapsed = step_start.elapsed().as_millis();
                    info!("  ✔ 步骤成功 (耗时: {}ms)", elapsed);
                    passed_steps += 1;
                    step_results.push(StepResult {
                        step_name: step.name.clone(),
                        success: true,
                        elapsed_ms: elapsed,
                        error: None,
                    });
                }
                Err(e) => {
                    let elapsed = step_start.elapsed().as_millis();
                    warn!("  ✖ 步骤失败: {} (耗时: {}ms)", e, elapsed);
                    failed_steps += 1;
                    step_results.push(StepResult {
                        step_name: step.name.clone(),
                        success: false,
                        elapsed_ms: elapsed,
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        let total_elapsed = total_start.elapsed().as_millis();
        info!(
            "🎉 脚本执行完毕! 通过: {} / {}, 失败: {}, 总耗时: {}ms",
            passed_steps,
            scenario.steps.len(),
            failed_steps,
            total_elapsed
        );

        Ok(ScenarioReport {
            scenario_name: scenario.name.clone(),
            total_steps: scenario.steps.len(),
            passed_steps,
            failed_steps,
            elapsed_ms: total_elapsed,
            step_results,
        })
    }

    /// 执行单个测试步骤
    async fn execute_step(
        &mut self,
        step: &ScenarioStep,
        variables: &std::collections::HashMap<String, String>,
        base_url: Option<&str>,
    ) -> Result<()> {
        let mut actions = step.actions.clone();

        // 组合扁平快捷定义动作
        if let Some(nav) = &step.navigate {
            actions.push(ScenarioAction::Navigate { navigate: nav.clone() });
        }
        if let Some(fill) = &step.fill {
            actions.push(ScenarioAction::Fill { fill: fill.clone() });
        }
        if let Some(click_target) = &step.click {
            actions.push(ScenarioAction::Click {
                click: click_target.clone(),
            });
        }
        if let Some(wf) = &step.wait_for {
            actions.push(ScenarioAction::WaitFor {
                wait_for: wf.clone(),
            });
        }
        if let Some(text) = &step.assert_text {
            actions.push(ScenarioAction::AssertText {
                assert_text: text.clone(),
            });
        }
        if let Some(sel) = &step.assert_exists {
            actions.push(ScenarioAction::AssertExists {
                assert_exists: sel.clone(),
            });
        }
        if let Some(ms) = step.sleep_ms {
            actions.push(ScenarioAction::Sleep { sleep: ms });
        }

        if actions.is_empty() {
            return Err(anyhow!("步骤 '{}' 未定义任何动作", step.name));
        }

        for act in actions {
            info!("       -> 正在执行动作: {:?}", act);
            self.execute_action(&act, variables, base_url).await?;
        }

        Ok(())
    }

    /// 执行原子动作原语
    async fn execute_action(
        &mut self,
        action: &ScenarioAction,
        variables: &std::collections::HashMap<String, String>,
        base_url: Option<&str>,
    ) -> Result<()> {
        match action {
            ScenarioAction::Navigate { navigate } => {
                let mut url = self.interpolate(navigate, variables, base_url);
                if url.starts_with('/') {
                    if let Some(b) = base_url {
                        url = format!("{}{}", b.trim_end_matches('/'), url);
                    }
                }
                info!("       -> 正在导航至: {}", url);
                self.driver.goto(&url).await?;
                tokio::time::sleep(Duration::from_millis(1000)).await;
            }
            ScenarioAction::Click { click } => {
                let target = self.interpolate(click, variables, base_url);
                let node_id = self.find_node(&target).await?;
                self.driver.click(&node_id).await?;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            ScenarioAction::Fill { fill } => {
                let target = self.interpolate(&fill.target, variables, base_url);
                let value = self.interpolate(&fill.value, variables, base_url);
                let node_id = self.find_node(&target).await?;
                self.driver.fill(&node_id, &value).await?;
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            ScenarioAction::Hover { hover } => {
                let target = self.interpolate(hover, variables, base_url);
                let node_id = self.find_node(&target).await?;
                let script = format!(
                    r#"(() => {{
                        const el = document.querySelector('[data-munin-id="{}"]') || document.getElementById('{}');
                        if (el) {{
                            el.dispatchEvent(new MouseEvent('mouseenter', {{ bubbles: true }}));
                            el.dispatchEvent(new MouseEvent('mouseover', {{ bubbles: true }}));
                            return true;
                        }}
                        return false;
                    }})()"#,
                    node_id, node_id
                );
                self.driver.evaluate_js(&script).await?;
            }
            ScenarioAction::Clear { clear } => {
                let target = self.interpolate(clear, variables, base_url);
                let node_id = self.find_node(&target).await?;
                self.driver.fill(&node_id, "").await?;
            }
            ScenarioAction::PressKey { press_key } => {
                let script = format!(
                    r#"document.activeElement.dispatchEvent(new KeyboardEvent('keydown', {{ key: {}, bubbles: true }}))"#,
                    serde_json::to_string(press_key)?
                );
                self.driver.evaluate_js(&script).await?;
            }
            ScenarioAction::WaitFor { wait_for } => {
                let target = self.interpolate(wait_for, variables, base_url);
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut ready = false;
                while Instant::now() < deadline {
                    if self.find_node_once(&target).await.is_ok() {
                        ready = true;
                        break;
                    }
                    let script = format!(
                        r#"document.body && document.body.innerText.includes({})"#,
                        serde_json::to_string(&target)?
                    );
                    if let Ok(res) = self.driver.evaluate_js(&script).await {
                        if res.as_bool().unwrap_or(false) {
                            ready = true;
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                if !ready {
                    return Err(anyhow!("等待超时：未能等到目标出现 '{}'", target));
                }
            }
            ScenarioAction::Sleep { sleep } => {
                tokio::time::sleep(Duration::from_millis(*sleep)).await;
            }
            ScenarioAction::Evaluate { evaluate } => {
                let script = self.interpolate(evaluate, variables, base_url);
                self.driver.evaluate_js(&script).await?;
            }
            ScenarioAction::AssertText { assert_text } => {
                let text = self.interpolate(assert_text, variables, base_url);
                let script = format!(
                    r#"document.body.innerText.includes({})"#,
                    serde_json::to_string(&text)?
                );
                let deadline = Instant::now() + Duration::from_secs(4);
                let mut found = false;
                while Instant::now() < deadline {
                    if let Ok(res) = self.driver.evaluate_js(&script).await {
                        if res.as_bool().unwrap_or(false) {
                            found = true;
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                if !found {
                    return Err(anyhow!("断言失败：页面未出现预期文本 '{}'", text));
                }
            }
            ScenarioAction::AssertNotText { assert_not_text } => {
                let text = self.interpolate(assert_not_text, variables, base_url);
                let script = format!(
                    r#"document.body.innerText.includes({})"#,
                    serde_json::to_string(&text)?
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
                let found = self
                    .driver
                    .evaluate_js(&script)
                    .await?
                    .as_bool()
                    .unwrap_or(false);
                if found {
                    return Err(anyhow!("断言失败：页面意外包含了不应出现的文本 '{}'", text));
                }
            }
            ScenarioAction::AssertExists { assert_exists } => {
                let sel = self.interpolate(assert_exists, variables, base_url);
                let script = format!(
                    r#"document.querySelector({}) !== null"#,
                    serde_json::to_string(&sel)?
                );
                let deadline = Instant::now() + Duration::from_secs(4);
                let mut exists = false;
                while Instant::now() < deadline {
                    if let Ok(res) = self.driver.evaluate_js(&script).await {
                        if res.as_bool().unwrap_or(false) {
                            exists = true;
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                if !exists {
                    return Err(anyhow!("断言失败：未检索到符合选择器的 DOM 元素 '{}'", sel));
                }
            }
            ScenarioAction::AssertNotExists { assert_not_exists } => {
                let sel = self.interpolate(assert_not_exists, variables, base_url);
                let script = format!(
                    r#"document.querySelector({}) !== null"#,
                    serde_json::to_string(&sel)?
                );
                let exists = self
                    .driver
                    .evaluate_js(&script)
                    .await?
                    .as_bool()
                    .unwrap_or(false);
                if exists {
                    return Err(anyhow!("断言失败：页面不应存在选择器元素 '{}'", sel));
                }
            }
            ScenarioAction::AssertTitle { assert_title } => {
                let title = self.interpolate(assert_title, variables, base_url);
                let current_title = self.driver.evaluate_js("document.title").await?
                    .as_str().unwrap_or("").to_string();
                if !current_title.contains(&title) {
                    return Err(anyhow!("断言失败：页面标题 '{}' 未包含预期文本 '{}'", current_title, title));
                }
            }
            ScenarioAction::AssertUrl { assert_url } => {
                let expected = self.interpolate(assert_url, variables, base_url);
                let current_url = self.driver.evaluate_js("window.location.href").await?
                    .as_str().unwrap_or("").to_string();
                if !current_url.contains(&expected) {
                    return Err(anyhow!("断言失败：当前 URL '{}' 未包含预期文本 '{}'", current_url, expected));
                }
            }
            ScenarioAction::BustOverlays { .. } => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            ScenarioAction::Screenshot { screenshot } => {
                let bytes = self.driver.take_screenshot().await?;
                let file_path = screenshot.as_deref().unwrap_or("target/screenshot.png");
                if let Some(parent) = Path::new(file_path).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::write(file_path, bytes)?;
                info!("📸 截图已留存: {}", file_path);
            }
        }
        Ok(())
    }

    /// 变量插值替换 (如将 ${base_url} 替换为实际值)
    fn interpolate(
        &self,
        input: &str,
        variables: &std::collections::HashMap<String, String>,
        base_url: Option<&str>,
    ) -> String {
        let mut result = input.to_string();
        if let Some(b) = base_url {
            result = result.replace("${base_url}", b.trim_end_matches('/'));
        }
        for (k, v) in variables {
            let pattern = format!("${{{}}}", k);
            result = result.replace(&pattern, v);
        }
        result
    }
    /// 通用节点定位器 (轮询等待，支持精确 ID、全等文本、placeholder、包含文本及 CSS 选择器)
    async fn find_node(&self, target: &str) -> Result<String> {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match self.find_node_once(target).await {
                Ok(id) => return Ok(id),
                Err(e) => {
                    if Instant::now() >= deadline {
                        return Err(e);
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }

    async fn find_node_once(&self, target: &str) -> Result<String> {
        let target_clean = target.trim();
        let elements = self.driver.get_interactive_elements().await?;

        // 1. node_id 精确匹配
        if let Some(el) = elements.iter().find(|e| e.node_id == target_clean) {
            return Ok(el.node_id.clone());
        }

        // 2. 文本完全匹配
        if let Some(el) = elements.iter().find(|e| e.text.trim() == target_clean) {
            return Ok(el.node_id.clone());
        }

        // 3. placeholder 完全匹配
        if let Some(el) = elements.iter().find(|e| {
            e.attributes.get("placeholder").map(|p| p.trim()) == Some(target_clean)
        }) {
            return Ok(el.node_id.clone());
        }

        // 4. 文本模糊包含
        if let Some(el) = elements.iter().find(|e| e.text.contains(target_clean)) {
            return Ok(el.node_id.clone());
        }

        // 5. placeholder 模糊包含
        if let Some(el) = elements.iter().find(|e| {
            e.attributes
                .get("placeholder")
                .map(|p| p.contains(target_clean))
                .unwrap_or(false)
        }) {
            return Ok(el.node_id.clone());
        }

        // 6. CSS 选择器回退匹配
        let script = format!(
            r#"(() => {{
                try {{
                    const el = document.querySelector({});
                    if (el) {{
                        return el.getAttribute('data-munin-id') || el.id || '';
                    }}
                }} catch (e) {{}}
                return '';
            }})()"#,
            serde_json::to_string(target_clean)?
        );

        if let Ok(res) = self.driver.evaluate_js(&script).await {
            if let Some(id) = res.as_str() {
                if !id.is_empty() {
                    return Ok(id.to_string());
                }
            }
        }

        Err(anyhow!(
            "未能定位目标交互元素: '{}' (已检索语义文本、Placeholder、ID 与 CSS 选择器)",
            target
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use munin_driver::mock::MockDriver;
    use munin_engine::fast::mock::MockFastEngine;
    use munin_types::DOMElementNode;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_universal_scenario_runner() -> Result<()> {
        let mut driver = MockDriver::new();
        driver.launch(true).await?;
        driver.add_element(DOMElementNode::new("btn-submit", "button", "Submit"));
        driver.set_default_js_result(serde_json::json!(true));

        let fast = MockFastEngine::new();
        let mut runner = ScenarioRunner::new(&mut driver, &fast);

        let scenario = Scenario {
            version: "1.0".to_string(),
            name: "Universal Browser Script Test".to_string(),
            description: None,
            base_url: Some("https://example.com".to_string()),
            variables: HashMap::new(),
            steps: vec![
                ScenarioStep {
                    name: "Click Submit".to_string(),
                    actions: vec![ScenarioAction::Click {
                        click: "Submit".to_string(),
                    }],
                    navigate: None,
                    click: None,
                    fill: None,
                    wait_for: None,
                    assert_text: None,
                    assert_exists: None,
                    sleep_ms: None,
                },
                ScenarioStep {
                    name: "Assert Exists".to_string(),
                    actions: vec![ScenarioAction::AssertExists {
                        assert_exists: ".container".to_string(),
                    }],
                    navigate: None,
                    click: None,
                    fill: None,
                    wait_for: None,
                    assert_text: None,
                    assert_exists: None,
                    sleep_ms: None,
                },
            ],
        };

        let report = runner.run(&scenario).await?;
        assert_eq!(report.total_steps, 2);
        assert_eq!(report.passed_steps, 2);
        assert_eq!(report.failed_steps, 0);

        Ok(())
    }
}
