use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::{Browser, BrowserConfig, Page};
use futures::StreamExt;
use munin_types::DOMElementNode;
use std::sync::Arc;

use crate::BrowserDriver;

/// 基于 Chrome DevTools Protocol (CDP) 的底层浏览器驱动
pub struct CdpDriver {
    browser: Option<Arc<Browser>>,
    page: Option<Page>,
    _handler_task: Option<tokio::task::JoinHandle<()>>,
}

impl CdpDriver {
    /// 连接到已运行的 Chromium/Chrome 实例（如 http://127.0.0.1:9222）
    pub async fn connect(url: impl Into<String>) -> Result<Self> {
        let endpoint = url.into();
        let (browser, mut handler) = Browser::connect(&endpoint)
            .await
            .with_context(|| format!("Failed to connect to CDP at {}", endpoint))?;

        let handler_task = tokio::spawn(async move {
            while let Some(_event) = handler.next().await {}
        });

        let page = browser.new_page("about:blank").await?;
        Ok(Self {
            browser: Some(Arc::new(browser)),
            page: Some(page),
            _handler_task: Some(handler_task),
        })
    }

    /// 启动本地 Chromium/Chrome 实例
    pub async fn launch_headless(headless: bool) -> Result<Self> {
        for lock in &["SingletonLock", "SingletonSocket", "SingletonCookie"] {
            let p = std::path::Path::new("/tmp/chromiumoxide-runner").join(lock);
            if p.exists() {
                let _ = std::fs::remove_file(p);
            }
        }
        let mut builder = BrowserConfig::builder()
            .viewport(None)
            .window_size(1920, 1080)
            .arg("--start-maximized")
            .arg("--no-default-browser-check");
        if !headless {
            builder = builder.with_head();
        }
        let config = builder
            .build()
            .map_err(|e| anyhow!("Failed to build browser config: {e}"))?;

        let (browser, mut handler) = Browser::launch(config)
            .await
            .context("Failed to launch chromium instance")?;

        let handler_task = tokio::spawn(async move {
            while let Some(_event) = handler.next().await {}
        });

        let page = browser.new_page("about:blank").await?;
        Ok(Self {
            browser: Some(Arc::new(browser)),
            page: Some(page),
            _handler_task: Some(handler_task),
        })
    }

    /// 优先连接已有调试端口，若不可用则启动新无头浏览器
    pub async fn connect_or_launch(addr_or_url: &str) -> Result<Self> {
        match Self::connect(addr_or_url).await {
            Ok(driver) => Ok(driver),
            Err(e) => {
                tracing::warn!(
                    "CDP 调试端口连接失败 ({:?})，尝试自启动本地无头浏览器...",
                    e
                );
                Self::launch_headless(true).await
            }
        }
    }

    fn active_page(&self) -> Result<&Page> {
        self.page
            .as_ref()
            .ok_or_else(|| anyhow!("Browser page is not initialized. Call launch or goto first."))
    }
}

#[async_trait]
impl BrowserDriver for CdpDriver {
    async fn launch(&mut self, headless: bool) -> Result<()> {
        if self.browser.is_none() {
            let instance = Self::launch_headless(headless).await?;
            self.browser = instance.browser;
            self.page = instance.page;
            self._handler_task = instance._handler_task;
        }
        Ok(())
    }

    async fn goto(&mut self, url: &str) -> Result<()> {
        if self.page.is_none() {
            self.launch(true).await?;
        }
        let page = self.active_page()?;
        page.goto(url).await?;
        Ok(())
    }

    async fn get_interactive_elements(&self) -> Result<Vec<DOMElementNode>> {
        let page = self.active_page()?;
        let script = r#"
            (() => {
                const selector = 'button, a, input, select, textarea, [role="button"], [role="link"], [onclick]';
                const elements = Array.from(document.querySelectorAll(selector));
                return elements.map((el, idx) => {
                    let id = el.getAttribute('data-munin-id') || el.id;
                    if (!id) {
                        id = 'munin-node-' + idx;
                        el.setAttribute('data-munin-id', id);
                    }
                    const attrs = {};
                    for (const attr of el.attributes) {
                        attrs[attr.name] = attr.value;
                    }
                    const text = (el.innerText || el.textContent || el.value || el.placeholder || '').trim();
                    return {
                        node_id: id,
                        tag: el.tagName.toLowerCase(),
                        text: text,
                        attributes: attrs
                    };
                });
            })()
        "#;

        let eval_fut = page.evaluate(script);
        let eval_res = tokio::time::timeout(std::time::Duration::from_secs(3), eval_fut)
            .await
            .map_err(|_| anyhow!("CDP evaluate timed out waiting for execution context"))??;
        let value = eval_res.into_value::<serde_json::Value>()?;
        let nodes: Vec<DOMElementNode> = serde_json::from_value(value)?;
        Ok(nodes)
    }

    async fn click(&self, node_id: &str) -> Result<()> {
        let page = self.active_page()?;
        let escaped_id = serde_json::to_string(node_id)?;
        let script = format!(
            r#"
            (() => {{
                const id = {escaped_id};
                const el = document.querySelector('[data-munin-id="' + id + '"]') || document.getElementById(id);
                if (el) {{
                    el.scrollIntoView({{ behavior: 'instant', block: 'center' }});
                    el.click();
                    return true;
                }}
                return false;
            }})()
            "#
        );

        let eval_res = page.evaluate(script).await?;
        let clicked = eval_res
            .value()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !clicked {
            return Err(anyhow!("Failed to click element with node_id '{}': element not found", node_id));
        }
        Ok(())
    }

    async fn fill(&self, node_id: &str, text: &str) -> Result<()> {
        let page = self.active_page()?;
        let escaped_id = serde_json::to_string(node_id)?;
        let escaped_text = serde_json::to_string(text)?;
        let script = format!(
            r#"
            (() => {{
                const id = {escaped_id};
                const text = {escaped_text};
                const el = document.querySelector('[data-munin-id="' + id + '"]') || document.getElementById(id);
                if (el) {{
                    el.focus();
                    el.value = text;
                    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    return true;
                }}
                return false;
            }})()
            "#
        );

        let eval_res = page.evaluate(script).await?;
        let filled = eval_res
            .value()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !filled {
            return Err(anyhow!("Failed to fill element with node_id '{}': element not found", node_id));
        }
        Ok(())
    }

    async fn evaluate_js(&self, script: &str) -> Result<serde_json::Value> {
        let page = self.active_page()?;
        let trimmed = script.trim();
        let wrapped = if trimmed.starts_with("(() =>") || trimmed.starts_with("(function") {
            trimmed.to_string()
        } else {
            format!("(() => {{ try {{ return ({}); }} catch(e) {{ {}; return null; }} }})()", trimmed, trimmed)
        };
        let eval_fut = page.evaluate(wrapped);
        let eval_res = tokio::time::timeout(std::time::Duration::from_secs(3), eval_fut)
            .await
            .map_err(|_| anyhow!("CDP evaluate timed out waiting for execution context"))??;
        Ok(eval_res.value().cloned().unwrap_or(serde_json::Value::Null))
    }
    async fn take_screenshot(&self) -> Result<Vec<u8>> {
        let page = self.active_page()?;
        let params = ScreenshotParams::builder().build();
        let bytes = page.screenshot(params).await?;
        Ok(bytes)
    }
}
