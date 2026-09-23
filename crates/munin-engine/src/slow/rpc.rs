use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use base64::prelude::*;
use munin_types::MacroStep;
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tracing::debug;

use crate::SlowEngine;

/// 基于 JSON-RPC 2.0 规范的远程慢模型 (System 2) 客户端引擎
///
/// 允许外部宏观规划大模型服务（如 Python、vLLM、Triton、FastAPI 等编写的独立大模型代理）
/// 通过网络 RPC 协议为 Munin 提供任务规划 (plan) 与低置信度异常仲裁 (arbitrate)
#[derive(Debug, Clone)]
pub struct RpcSlowEngine {
    endpoint: String,
    client: Client,
    req_counter: std::sync::Arc<AtomicU64>,
}

impl RpcSlowEngine {
    /// 连接到指定的 JSON-RPC 服务地址 (例如 "http://127.0.0.1:50051/rpc")
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self::with_timeout(endpoint, Duration::from_secs(60))
    }

    /// 指定超时时间创建 RPC 客户端
    pub fn with_timeout(endpoint: impl Into<String>, timeout: Duration) -> Self {
        let client = Client::builder()
            .timeout(timeout)
            .tcp_nodelay(true)
            .build()
            .unwrap_or_default();
        Self {
            endpoint: endpoint.into(),
            client,
            req_counter: std::sync::Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// 健康检查：探测慢引擎 RPC 服务是否可达
    pub async fn ping(&self) -> Result<()> {
        self.call_rpc("ping", serde_json::json!({})).await?;
        Ok(())
    }

    /// 发送底层标准 JSON-RPC 2.0 请求
    async fn call_rpc(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.req_counter.fetch_add(1, Ordering::Relaxed);
        let payload = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id,
        });

        debug!("RPC -> {} call: method={}, id={}", self.endpoint, method, id);

        let response = self
            .client
            .post(&self.endpoint)
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("Failed to send JSON-RPC request to {}", self.endpoint))?;

        let status = response.status();
        if !status.is_success() {
            let err_body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "JSON-RPC server returned HTTP error {}: {}",
                status,
                err_body
            ));
        }

        let resp_json: Value = response
            .json()
            .await
            .context("Failed to parse JSON-RPC response body as JSON")?;

        if let Some(err) = resp_json.get("error") {
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown RPC error");
            return Err(anyhow!("JSON-RPC Server Error (code {}): {}", code, msg));
        }

        resp_json
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow!("Malformed JSON-RPC response: missing 'result' field"))
    }
}

#[async_trait]
impl SlowEngine for RpcSlowEngine {
    async fn plan(&self, user_goal: &str, context: &str) -> Result<Vec<MacroStep>> {
        let params = json!({
            "user_goal": user_goal,
            "context": context,
        });

        let result = self.call_rpc("plan", params).await?;
        // 尝试解析并兼容 step_id, intent/description, expected_outcome
        let parse_steps = |arr: &[Value]| -> Result<Vec<MacroStep>> {
            let mut steps = Vec::new();
            for (idx, item) in arr.iter().enumerate() {
                let step_id = item
                    .get("step_id")
                    .and_then(|id| id.as_u64())
                    .map(|id| id as usize)
                    .unwrap_or(idx + 1);
                let intent = item
                    .get("intent")
                    .or_else(|| item.get("description"))
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                let expected_outcome = item
                    .get("expected_outcome")
                    .and_then(|e| e.as_str())
                    .unwrap_or("")
                    .to_string();
                steps.push(MacroStep::new(step_id, intent, expected_outcome));
            }
            Ok(steps)
        };

        if let Some(arr) = result.as_array() {
            return parse_steps(arr);
        } else if let Some(steps_val) = result.get("steps").and_then(|s| s.as_array()) {
            return parse_steps(steps_val);
        }

        Err(anyhow!(
            "Unexpected RPC plan response: expected array or {{'steps': [...]}}, got: {}",
            result
        ))
    }
    async fn arbitrate(
        &self,
        step_intent: &str,
        current_dom_desc: &str,
        screenshot: Option<&[u8]>,
    ) -> Result<String> {
        let screenshot_b64 = screenshot.map(|bytes| BASE64_STANDARD.encode(bytes));

        let params = json!({
            "step_intent": step_intent,
            "current_dom_desc": current_dom_desc,
            "screenshot": screenshot_b64,
        });

        let result = self.call_rpc("arbitrate", params).await?;

        if let Some(text) = result.as_str() {
            return Ok(text.to_string());
        } else if let Some(action) = result.get("action").and_then(|a| a.as_str()) {
            return Ok(action.to_string());
        }

        Ok(result.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn test_rpc_slow_engine_plan_and_arbitrate() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let endpoint = format!("http://{}", addr);

        tokio::spawn(async move {
            loop {
                let (mut socket, _) = match listener.accept().await {
                    Ok(conn) => conn,
                    Err(_) => break,
                };

                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = socket.read(&mut buf).await.unwrap();
                    let req_str = String::from_utf8_lossy(&buf[..n]);

                    let body = req_str.split("\r\n\r\n").nth(1).unwrap_or("");
                    let req_json: Value = serde_json::from_str(body).unwrap_or(Value::Null);

                    let method = req_json.get("method").and_then(|m| m.as_str()).unwrap_or("");
                    let id = req_json.get("id").cloned().unwrap_or(json!(1));

                    let resp_body = match method {
                        "plan" => json!({
                            "jsonrpc": "2.0",
                            "result": [
                                {
                                    "step_id": 1,
                                    "intent": "打开登录页并输入账号",
                                    "expected_outcome": "登录成功"
                                }
                            ],
                            "id": id
                        }),
                        "arbitrate" => json!({
                            "jsonrpc": "2.0",
                            "result": "click:btn-submit",
                            "id": id
                        }),
                        _ => json!({
                            "jsonrpc": "2.0",
                            "error": {"code": -32601, "message": "Method not found"},
                            "id": id
                        }),
                    };

                    let resp_str = resp_body.to_string();
                    let http_resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        resp_str.len(),
                        resp_str
                    );
                    let _ = socket.write_all(http_resp.as_bytes()).await;
                });
            }
        });

        let engine = RpcSlowEngine::new(endpoint);
        let steps = engine.plan("用户登录测试", "Browser ready").await?;
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].intent, "打开登录页并输入账号");
        let arbitration = engine.arbitrate("点击提交", "按钮处于禁用状态", None).await?;
        assert_eq!(arbitration, "click:btn-submit");

        Ok(())
    }
}
