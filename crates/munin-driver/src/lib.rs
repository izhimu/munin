use anyhow::Result;
use async_trait::async_trait;
use munin_types::DOMElementNode;

pub mod mock;
pub use mock::MockDriver;

#[cfg(feature = "cdp")]
pub mod cdp;
#[cfg(feature = "cdp")]
pub use cdp::CdpDriver;

/// 统一浏览器驱动抽象层契约
#[async_trait]
pub trait BrowserDriver: Send + Sync {
    /// 启动或连接浏览器实例
    async fn launch(&mut self, headless: bool) -> Result<()>;

    /// 页面跳转
    async fn goto(&mut self, url: &str) -> Result<()>;

    /// 提取当前视口具备语义的可交互节点列表
    async fn get_interactive_elements(&self) -> Result<Vec<DOMElementNode>>;

    /// 模拟点击动作
    async fn click(&self, node_id: &str) -> Result<()>;

    /// 模拟输入文本动作
    async fn fill(&self, node_id: &str, text: &str) -> Result<()>;

    /// 执行页面 JS 脚本并返回 JSON 值
    async fn evaluate_js(&self, script: &str) -> Result<serde_json::Value>;

    /// 视口截图（供慢引擎多模态或故障排查使用）
    async fn take_screenshot(&self) -> Result<Vec<u8>>;

    /// 等待 DOM 稳定（无新增/删除节点、无 CSS transitions 运行中），防止抓取幽灵节点
    async fn wait_for_stable(&self) -> Result<()> {
        // 默认空实现，CDP 驱动覆盖
        Ok(())
    }

    /// AntD DatePicker 高阶语义操作：选择日期
    async fn pick_date(&self, node_id: &str, date: &str) -> Result<()> {
        let _ = (node_id, date);
        Err(anyhow::anyhow!("pick_date not implemented for this driver"))
    }

    /// AntD Select 高阶语义操作：选择下拉项
    async fn select_dropdown(&self, node_id: &str, label: &str) -> Result<()> {
        let _ = (node_id, label);
        Err(anyhow::anyhow!("select_dropdown not implemented for this driver"))
    }

    /// 等待指定 selector 的元素达到目标状态（默认实现：基于 evaluate_js 轮询）
    ///
    /// `state`: "visible" | "hidden" | "attached" | "detached"
    async fn wait_for(&self, selector: &str, state: &str, timeout_ms: u64) -> Result<()> {
        let sel_json = serde_json::to_string(selector)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            let script = format!(
                r#"(() => {{
                    const el = document.querySelector({sel_json});
                    const visible = !!el && (el.offsetWidth > 0 || el.offsetHeight > 0) && getComputedStyle(el).visibility !== 'hidden';
                    return {{ attached: !!el, visible: visible }};
                }})()"#
            );
            let res = self.evaluate_js(&script).await.unwrap_or(serde_json::Value::Null);
            let attached = res.get("attached").and_then(|v| v.as_bool()).unwrap_or(false);
            let visible = res.get("visible").and_then(|v| v.as_bool()).unwrap_or(false);
            let done = match state {
                "visible" => visible,
                "hidden" => attached && !visible || !attached,
                "attached" => attached,
                "detached" => !attached,
                other => return Err(anyhow::anyhow!("wait_for: unknown state '{other}'")),
            };
            if done {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(anyhow::anyhow!(
                    "wait_for timeout after {timeout_ms}ms: selector '{selector}' never reached state '{state}'"
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// 等待网络空闲：连续 idle_ms 无新增资源加载（默认实现：轮询 performance 资源计数）
    async fn wait_for_network_idle(&self, timeout_ms: u64, idle_ms: u64) -> Result<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        let mut last_count = -1i64;
        let mut quiet_since = std::time::Instant::now();
        loop {
            let res = self
                .evaluate_js("performance.getEntriesByType('resource').length")
                .await
                .unwrap_or(serde_json::Value::Null);
            let count = res.as_i64().unwrap_or(-1);
            if count != last_count {
                last_count = count;
                quiet_since = std::time::Instant::now();
            } else if quiet_since.elapsed().as_millis() as u64 >= idle_ms {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(anyhow::anyhow!("wait_for_network_idle timeout after {timeout_ms}ms"));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use munin_types::Action;

    #[tokio::test]
    async fn test_mock_driver_flow() -> Result<()> {
        let mut driver = MockDriver::new();
        driver.launch(true).await?;
        assert!(driver.is_launched());

        driver.goto("https://example.com").await?;
        assert_eq!(driver.current_url(), "https://example.com");

        let node = DOMElementNode::new("btn-submit", "button", "Submit");
        driver.add_element(node);

        let elements = driver.get_interactive_elements().await?;
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].node_id, "btn-submit");

        driver.click("btn-submit").await?;
        driver.fill("input-username", "admin").await?;

        let actions = driver.recorded_actions();
        assert_eq!(actions.len(), 3);
        assert_eq!(
            actions[0],
            Action::Navigate {
                url: "https://example.com".to_string()
            }
        );
        assert_eq!(
            actions[1],
            Action::Click {
                target_id: "btn-submit".to_string()
            }
        );
        assert_eq!(
            actions[2],
            Action::Fill {
                target_id: "input-username".to_string(),
                text: "admin".to_string()
            }
        );

        driver.set_js_result("document.title", serde_json::json!("Example Domain"));
        let title = driver.evaluate_js("return document.title;").await?;
        assert_eq!(title, "Example Domain");

        let screenshot = driver.take_screenshot().await?;
        assert!(!screenshot.is_empty());

        Ok(())
    }
}
