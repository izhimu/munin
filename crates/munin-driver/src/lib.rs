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
