use anyhow::{anyhow, Context, Result};
use base64::prelude::*;
use munin_driver::BrowserDriver;
use munin_engine::{FastEngine, SlowEngine};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::BiSystemSupervisor;

/// Munin RPC 服务端
///
/// 对外暴露基于 HTTP 的标准 JSON-RPC 2.0 服务接口，
/// 允许外部慢模型 (如 Python / LangChain / 自定义 AI Agent) 通过 RPC 控制浏览器驱动、
/// 调用本地 Laya 极速模型、获取页面交互元素及执行宏观目标。
pub struct MuninRpcServer<D: BrowserDriver + 'static, F: FastEngine + 'static, S: SlowEngine + 'static> {
    supervisor: Arc<Mutex<BiSystemSupervisor<D, F, S>>>,
    listen_addr: String,
}

impl<D: BrowserDriver + 'static, F: FastEngine + 'static, S: SlowEngine + 'static> MuninRpcServer<D, F, S> {
    pub fn new(supervisor: BiSystemSupervisor<D, F, S>, listen_addr: impl Into<String>) -> Self {
        Self {
            supervisor: Arc::new(Mutex::new(supervisor)),
            listen_addr: listen_addr.into(),
        }
    }

    pub fn listen_addr(&self) -> &str {
        &self.listen_addr
    }

    /// 启动 RPC 服务监听并持续处理请求
    pub async fn run(&self) -> Result<()> {
        let listener = TcpListener::bind(&self.listen_addr)
            .await
            .with_context(|| format!("Failed to bind RPC server to {}", self.listen_addr))?;

        info!("🚀 Munin JSON-RPC 2.0 Server listening on http://{}", self.listen_addr);

        loop {
            let (stream, remote_addr) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    warn!("Failed to accept incoming RPC connection: {e}");
                    continue;
                }
            };

            let supervisor = Arc::clone(&self.supervisor);
            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, supervisor).await {
                    error!("Error handling RPC connection from {remote_addr}: {e}");
                }
            });
        }
    }

    /// 分发并执行单个 JSON-RPC 请求
    pub async fn dispatch(
        supervisor: &Arc<Mutex<BiSystemSupervisor<D, F, S>>>,
        req: &Value,
    ) -> Value {
        let id = req.get("id").cloned().unwrap_or(json!(null));
        let method = match req.get("method").and_then(|m| m.as_str()) {
            Some(m) => m,
            None => {
                return json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32600, "message": "Invalid Request: missing method"},
                    "id": id
                });
            }
        };

        let params = req.get("params").cloned().unwrap_or(json!({}));

        let mut sup = supervisor.lock().await;

        let result = match method {
            "execute_goal" => {
                let goal = params.get("goal").and_then(|g| g.as_str()).unwrap_or("");
                if goal.is_empty() {
                    Err(anyhow!("Missing 'goal' parameter in execute_goal"))
                } else {
                    if let Some(url) = params.get("url").and_then(|u| u.as_str()) {
                        let _ = sup.driver.goto(url).await;
                    }
                    match sup.execute_goal_with_report(goal).await {
                        Ok(report) => Ok(serde_json::to_value(&report).unwrap_or(json!({"success": true}))),
                        Err(e) => Err(anyhow!("Goal execution error: {e}")),
                    }
                }
            }
            "navigate" => {
                let url = params.get("url").and_then(|u| u.as_str()).unwrap_or("");
                if url.is_empty() {
                    Err(anyhow!("Missing 'url' parameter in navigate"))
                } else {
                    sup.driver.goto(url).await.map(|_| json!({"navigated": true, "url": url}))
                }
            }
            "get_interactive_elements" => {
                match sup.driver.get_interactive_elements().await {
                    Ok(mut elements) => {
                        let prune = params.get("prune").and_then(|p| p.as_bool()).unwrap_or(false);
                        if prune {
                            let goal = params.get("goal").and_then(|g| g.as_str()).unwrap_or("");
                            elements = sup.candidate_pruner.prune(&elements, goal).into_iter().cloned().collect();
                        }
                        Ok(json!({
                            "count": elements.len(),
                            "elements": elements
                        }))
                    }
                    Err(e) => Err(e),
                }
            }
            "click" => {
                let node_id = params.get("node_id").and_then(|n| n.as_str()).unwrap_or("");
                if node_id.is_empty() {
                    Err(anyhow!("Missing 'node_id' in click params"))
                } else {
                    sup.driver.click(node_id).await.map(|_| json!({"clicked": true, "node_id": node_id}))
                }
            }
            "fill" => {
                let node_id = params.get("node_id").and_then(|n| n.as_str()).unwrap_or("");
                let text = params.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if node_id.is_empty() {
                    Err(anyhow!("Missing 'node_id' in fill params"))
                } else {
                    sup.driver.fill(node_id, text).await.map(|_| json!({"filled": true, "node_id": node_id}))
                }
            }
            "screenshot" => {
                match sup.driver.take_screenshot().await {
                    Ok(bytes) => Ok(json!({"base64": BASE64_STANDARD.encode(&bytes)})),
                    Err(e) => Err(e),
                }
            }
            "bust_overlays" => {
                match sup.heal_overlays().await {
                    Ok(_) => Ok(json!({"healed": true})),
                    Err(e) => Err(e),
                }
            }
            "evaluate_js" => {
                let script = params.get("script").and_then(|s| s.as_str()).unwrap_or("");
                sup.driver.evaluate_js(script).await
            }
            "probe" => {
                let assertion = params.get("assertion").and_then(|a| a.as_str()).unwrap_or("");
                let default_state = json!({});
                let state = params.get("state").unwrap_or(&default_state);
                match sup.fast.probe(state, assertion).await {
                    Ok((passed, conf)) => Ok(json!({"passed": passed, "confidence": conf})),
                    Err(e) => Err(e),
                }
            }
            "ping" => Ok(json!({"pong": true, "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()})),
            other => Err(anyhow!("Method '{}' not found", other)),
        };

        match result {
            Ok(res_val) => json!({
                "jsonrpc": "2.0",
                "result": res_val,
                "id": id
            }),
            Err(e) => {
                let err_msg = e.to_string();
                let code = if err_msg.contains("not found") { -32601 } else { -32000 };
                json!({
                    "jsonrpc": "2.0",
                    "error": {"code": code, "message": err_msg},
                    "id": id
                })
            }
        }
    }
}

async fn handle_connection<D: BrowserDriver + 'static, F: FastEngine + 'static, S: SlowEngine + 'static>(
    mut stream: TcpStream,
    supervisor: Arc<Mutex<BiSystemSupervisor<D, F, S>>>,
) -> Result<()> {
    let mut buffer = Vec::with_capacity(4096);
    let mut temp = [0u8; 2048];

    loop {
        let n = stream.read(&mut temp).await?;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&temp[..n]);

        // 检查是否包含 HTTP 请求头结束标志
        if let Some(header_end) = find_subsequence(&buffer, b"\r\n\r\n") {
            let headers_part = String::from_utf8_lossy(&buffer[..header_end]);
            let content_len = parse_content_length(&headers_part);

            let body_start = header_end + 4;
            if buffer.len() < body_start + content_len {
                // 等待继续读取完整 body
                continue;
            }

            let body_slice = &buffer[body_start..body_start + content_len];
            let req_json: Value = serde_json::from_slice(body_slice)
                .unwrap_or(json!({"jsonrpc": "2.0", "error": {"code": -32700, "message": "Parse error"}}));

            let resp_json = if let Some(arr) = req_json.as_array() {
                // Batch requests
                let mut resp_arr = Vec::new();
                for item in arr {
                    resp_arr.push(MuninRpcServer::dispatch(&supervisor, item).await);
                }
                Value::Array(resp_arr)
            } else {
                MuninRpcServer::dispatch(&supervisor, &req_json).await
            };

            let resp_bytes = serde_json::to_vec(&resp_json)?;
            let http_response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                resp_bytes.len()
            );

            stream.write_all(http_response.as_bytes()).await?;
            stream.write_all(&resp_bytes).await?;
            stream.flush().await?;

            // 清理已消费数据
            buffer.drain(..body_start + content_len);
        }
    }

    Ok(())
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn parse_content_length(headers: &str) -> usize {
    for line in headers.lines() {
        if line.to_ascii_lowercase().starts_with("content-length:") {
            if let Some(val) = line.split(':').nth(1) {
                if let Ok(num) = val.trim().parse::<usize>() {
                    return num;
                }
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use munin_driver::mock::MockDriver;
    use munin_engine::fast::mock::MockFastEngine;
    use munin_engine::slow::mock::MockSlowEngine;
    use munin_types::{DOMElementNode, MacroStep};

    #[tokio::test]
    async fn test_rpc_server_dispatch() -> Result<()> {
        let mut driver = MockDriver::new();
        driver.launch(true).await?;
        driver.add_element(DOMElementNode::new("btn-submit", "button", "提交"));

        let fast = MockFastEngine::new().with_choice("btn-submit", 0.95);
        let slow = MockSlowEngine::new().with_steps(vec![MacroStep::new(1, "点击提交", "成功")]);

        let supervisor = BiSystemSupervisor::new(driver, fast, slow, 0.85);
        let server = MuninRpcServer::new(supervisor, "127.0.0.1:0");

        // 1. 测试 ping
        let req_ping = json!({"jsonrpc": "2.0", "method": "ping", "id": 10});
        let resp_ping = MuninRpcServer::dispatch(&server.supervisor, &req_ping).await;
        assert_eq!(resp_ping["result"]["pong"], true);
        assert_eq!(resp_ping["id"], 10);

        // 2. 测试 navigate
        let req_nav = json!({"jsonrpc": "2.0", "method": "navigate", "params": {"url": "http://test.local"}, "id": 11});
        let resp_nav = MuninRpcServer::dispatch(&server.supervisor, &req_nav).await;
        assert_eq!(resp_nav["result"]["navigated"], true);

        // 3. 测试 get_interactive_elements
        let req_elem = json!({"jsonrpc": "2.0", "method": "get_interactive_elements", "id": 12});
        let resp_elem = MuninRpcServer::dispatch(&server.supervisor, &req_elem).await;
        assert_eq!(resp_elem["result"]["count"], 1);

        // 4. 测试 click
        let req_click = json!({"jsonrpc": "2.0", "method": "click", "params": {"node_id": "btn-submit"}, "id": 13});
        let resp_click = MuninRpcServer::dispatch(&server.supervisor, &req_click).await;
        assert_eq!(resp_click["result"]["clicked"], true);

        // 5. 测试未知方法
        let req_unknown = json!({"jsonrpc": "2.0", "method": "unknown_foo", "id": 14});
        let resp_unknown = MuninRpcServer::dispatch(&server.supervisor, &req_unknown).await;
        assert_eq!(resp_unknown["error"]["code"], -32601);

        Ok(())
    }
}
