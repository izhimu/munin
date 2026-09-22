use anyhow::{anyhow, Result};
use async_trait::async_trait;
use munin_types::{Action, DOMElementNode};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use crate::BrowserDriver;

#[derive(Debug, Default)]
struct MockInner {
    is_launched: bool,
    current_url: String,
    elements: Vec<DOMElementNode>,
    actions: Vec<Action>,
    js_results: HashMap<String, serde_json::Value>,
    default_js_result: Option<serde_json::Value>,
    screenshot: Vec<u8>,
}

/// 内存模拟浏览器驱动（用于单元测试、CI环境及无GUI环境模拟）
#[derive(Debug, Clone, Default)]
pub struct MockDriver {
    inner: Arc<Mutex<MockInner>>,
}

impl MockDriver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_elements(self, elements: Vec<DOMElementNode>) -> Self {
        self.set_elements(elements);
        self
    }

    pub fn set_elements(&self, elements: Vec<DOMElementNode>) {
        let mut inner = self.inner.lock();
        inner.elements = elements;
    }

    pub fn add_element(&self, element: DOMElementNode) {
        let mut inner = self.inner.lock();
        inner.elements.push(element);
    }

    pub fn recorded_actions(&self) -> Vec<Action> {
        let inner = self.inner.lock();
        inner.actions.clone()
    }

    pub fn clear_actions(&self) {
        let mut inner = self.inner.lock();
        inner.actions.clear();
    }

    pub fn set_js_result(&self, key_contains: &str, result: serde_json::Value) {
        let mut inner = self.inner.lock();
        inner.js_results.insert(key_contains.to_string(), result);
    }

    pub fn set_default_js_result(&self, result: serde_json::Value) {
        let mut inner = self.inner.lock();
        inner.default_js_result = Some(result);
    }

    pub fn set_screenshot(&self, bytes: Vec<u8>) {
        let mut inner = self.inner.lock();
        inner.screenshot = bytes;
    }

    pub fn current_url(&self) -> String {
        let inner = self.inner.lock();
        inner.current_url.clone()
    }

    pub fn is_launched(&self) -> bool {
        let inner = self.inner.lock();
        inner.is_launched
    }
}

#[async_trait]
impl BrowserDriver for MockDriver {
    async fn launch(&mut self, _headless: bool) -> Result<()> {
        let mut inner = self.inner.lock();
        inner.is_launched = true;
        Ok(())
    }

    async fn goto(&mut self, url: &str) -> Result<()> {
        let mut inner = self.inner.lock();
        if !inner.is_launched {
            inner.is_launched = true;
        }
        inner.current_url = url.to_string();
        inner.actions.push(Action::Navigate {
            url: url.to_string(),
        });
        Ok(())
    }

    async fn get_interactive_elements(&self) -> Result<Vec<DOMElementNode>> {
        let inner = self.inner.lock();
        Ok(inner.elements.clone())
    }

    async fn click(&self, node_id: &str) -> Result<()> {
        let mut inner = self.inner.lock();
        let found = inner.elements.iter().any(|e| e.node_id == node_id);
        inner.actions.push(Action::Click {
            target_id: node_id.to_string(),
        });
        if !found && node_id.is_empty() {
            return Err(anyhow!("Cannot click on empty node_id"));
        }
        Ok(())
    }

    async fn fill(&self, node_id: &str, text: &str) -> Result<()> {
        let mut inner = self.inner.lock();
        inner.actions.push(Action::Fill {
            target_id: node_id.to_string(),
            text: text.to_string(),
        });
        Ok(())
    }

    async fn evaluate_js(&self, script: &str) -> Result<serde_json::Value> {
        let inner = self.inner.lock();
        for (k, v) in &inner.js_results {
            if script.contains(k) {
                return Ok(v.clone());
            }
        }
        if let Some(default_val) = &inner.default_js_result {
            return Ok(default_val.clone());
        }
        Ok(serde_json::Value::String(format!("mock_eval_result_for: {}", script)))
    }

    async fn take_screenshot(&self) -> Result<Vec<u8>> {
        let inner = self.inner.lock();
        if inner.screenshot.is_empty() {
            Ok(vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A])
        } else {
            Ok(inner.screenshot.clone())
        }
    }
}
