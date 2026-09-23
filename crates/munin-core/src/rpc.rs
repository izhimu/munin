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

        // ping 不依赖 supervisor 状态，提前返回，避免被长时间任务（如 execute_goal）持有的锁阻塞
        if method == "ping" {
            return json!({
                "jsonrpc": "2.0",
                "result": {"pong": true, "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()},
                "id": id
            });
        }

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
            // 拉取当前页面可操作语义图 + 页面文本：供 LLM 一次性规划长流程步骤
            "get_action_map" => {
                match sup.driver.get_interactive_elements().await {
                    Ok(elements) => {
                        let goal = params.get("goal").and_then(|g| g.as_str()).unwrap_or("");
                        let max = params.get("max").and_then(|m| m.as_u64()).unwrap_or(30) as usize;
                        let pruned = sup.candidate_pruner.prune(&elements, goal);
                        match sup.state_probe.snapshot_text(&sup.driver).await {
                            Ok(page_text) => Ok(json!({
                                "goal": goal,
                                "page_text": page_text,
                                "count": pruned.len().min(max),
                                "action_map": pruned.iter().take(max).map(|e| json!({
                                    "node_id": e.node_id,
                                    "tag": e.tag,
                                    "text": e.text,
                                    "attributes": e.attributes,
                                })).collect::<Vec<_>>(),
                                // LLM 对工具返回中的指令服从度高于静态文档：在响应内直接下发下一步动作指令
                                "next_action": "MANDATORY: Plan ALL steps for the goal NOW using action_map + page_text above. Then call execute_steps ONCE with the full steps array. DO NOT call click/fill/probe individually — that wastes LLM round trips."
                            })),
                            Err(e) => Err(e),
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            // 批量执行 LLM 规划好的宏观步骤：本地快引擎闭环跑完，避免逐动作 RPC 往返
            "execute_steps" => {
                let steps_val = params.get("steps").and_then(|s| s.as_array()).cloned();
                let Some(arr) = steps_val else {
                    return json!({
                        "jsonrpc": "2.0",
                        "error": {"code": -32602, "message": "Missing 'steps' array in execute_steps"},
                        "id": id
                    });
                };
                let mut steps = Vec::with_capacity(arr.len());
                for item in &arr {
                    match serde_json::from_value::<munin_types::MacroStep>(item.clone()) {
                        Ok(s) => steps.push(s),
                        Err(e) => {
                            return json!({
                                "jsonrpc": "2.0",
                                "error": {"code": -32602, "message": format!("Invalid step: {e}")},
                                "id": id
                            });
                        }
                    }
                }
                let start = std::time::Instant::now();
                match sup.execute_steps(steps).await {
                    Ok((results, overall)) => Ok(json!({
                        "overall_success": overall,
                        "total_duration_ms": start.elapsed().as_millis() as u64,
                        "steps": results
                    })),
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
            "pick_date" => {
                let node_id = params.get("node_id").and_then(|n| n.as_str()).unwrap_or("");
                let date = params.get("date").and_then(|d| d.as_str()).unwrap_or("");
                if node_id.is_empty() || date.is_empty() {
                    Err(anyhow!("Missing 'node_id' or 'date' in pick_date params"))
                } else {
                    sup.driver.pick_date(node_id, date).await.map(|_| json!({"picked": true, "node_id": node_id, "date": date}))
                }
            }
            "select_dropdown" | "select_option" => {
                let node_id = params.get("node_id").and_then(|n| n.as_str()).unwrap_or("");
                let label = params.get("label").and_then(|l| l.as_str()).unwrap_or("");
                if node_id.is_empty() || label.is_empty() {
                    Err(anyhow!("Missing 'node_id' or 'label' in select_dropdown params"))
                } else {
                    sup.driver.select_dropdown(node_id, label).await.map(|_| json!({"selected": true, "node_id": node_id, "label": label}))
                }
            }
            "wait_for_stable" => {
                sup.driver.wait_for_stable().await.map(|_| json!({"stable": true}))
            }
            "wait_for" => {
                let selector = params.get("selector").and_then(|s| s.as_str()).unwrap_or("");
                let state = params.get("state").and_then(|s| s.as_str()).unwrap_or("visible");
                let timeout_ms = params.get("timeout_ms").and_then(|t| t.as_u64()).unwrap_or(5000);
                if selector.is_empty() {
                    Err(anyhow!("Missing 'selector' in wait_for params"))
                } else {
                    sup.driver.wait_for(selector, state, timeout_ms).await.map(|_| json!({"reached": true, "selector": selector, "state": state}))
                }
            }
            "wait_for_network_idle" => {
                let timeout_ms = params.get("timeout_ms").and_then(|t| t.as_u64()).unwrap_or(10000);
                let idle_ms = params.get("idle_ms").and_then(|t| t.as_u64()).unwrap_or(500);
                sup.driver.wait_for_network_idle(timeout_ms, idle_ms).await.map(|_| json!({"idle": true}))
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

    /// 请求体大小上限（16MB）
    const MAX_BODY_SIZE: usize = 16 * 1024 * 1024;
    /// 读取超时时间，避免客户端发送停滞导致任务永久挂起
    const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    loop {
        // 先消费缓冲区中已有的完整请求（支持 HTTP 管线化 / 粘包），耗尽后再阻塞读取
        while let Some(header_end) = find_subsequence(&buffer, b"\r\n\r\n") {
            let headers_part = String::from_utf8_lossy(&buffer[..header_end]);
            let body_start = header_end + 4;

            // Content-Length 缺失或非法：返回 411 并关闭连接，避免将后续 body
            // 字节误解析为新请求头导致流错位
            let Some(content_len) = parse_content_length(&headers_part) else {
                let err_body = serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32600, "message": "Missing or invalid Content-Length"},
                    "id": null
                }))?;
                let http_response = format!(
                    "HTTP/1.1 411 Length Required\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    err_body.len()
                );
                stream.write_all(http_response.as_bytes()).await?;
                stream.write_all(&err_body).await?;
                stream.flush().await?;
                return Ok(());
            };

            if content_len > MAX_BODY_SIZE {
                // 请求体超限，返回 413 并关闭连接
                let err_body = serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32000, "message": "Request too large"},
                    "id": null
                }))?;
                let http_response = format!(
                    "HTTP/1.1 413 Payload Too Large\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    err_body.len()
                );
                stream.write_all(http_response.as_bytes()).await?;
                stream.write_all(&err_body).await?;
                stream.flush().await?;
                return Ok(());
            }

            if buffer.len() < body_start + content_len {
                // 等待继续读取完整 body
                break;
            }

            let body_slice = &buffer[body_start..body_start + content_len];
            let req_json: Value = match serde_json::from_slice(body_slice) {
                Ok(v) => v,
                Err(e) => {
                    // 直接返回真实解析错误（含行列），避免误导为 "missing method"
                    let err_resp = json!({
                        "jsonrpc": "2.0",
                        "error": {"code": -32700, "message": format!("Parse error: {e}")},
                        "id": null
                    });
                    let resp_bytes = serde_json::to_vec(&err_resp)?;
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                        resp_bytes.len()
                    );
                    stream.write_all(header.as_bytes()).await?;
                    stream.write_all(&resp_bytes).await?;
                    continue;
                }
            };

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

        let n = match tokio::time::timeout(READ_TIMEOUT, stream.read(&mut temp)).await {
            Ok(res) => res?,
            Err(_) => {
                // 读取超时，关闭连接
                return Ok(());
            }
        };
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&temp[..n]);
    }

    Ok(())
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn parse_content_length(headers: &str) -> Option<usize> {
    for line in headers.lines() {
        if line.to_ascii_lowercase().starts_with("content-length:") {
            // 取首个冒号后的值（头部值本身不含冒号）
            return line
                .split_once(':')
                .and_then(|(_, v)| v.trim().parse::<usize>().ok());
        }
    }
    None
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

        let supervisor = BiSystemSupervisor::new(driver, fast, slow, 0.85).with_in_memory_recovery();
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

        // 4. 测试 get_action_map（LLM 长流程规划视图；须在 click 前调用，mock 点击会移除节点）
        let req_map = json!({"jsonrpc": "2.0", "method": "get_action_map", "params": {"goal": "提交表单"}, "id": 15});
        let resp_map = MuninRpcServer::dispatch(&server.supervisor, &req_map).await;
        assert_eq!(resp_map["result"]["count"], 1);
        assert_eq!(resp_map["result"]["action_map"][0]["node_id"], "btn-submit");
        assert!(resp_map["result"]["page_text"].is_string());

        // 5. 测试 click
        let req_click = json!({"jsonrpc": "2.0", "method": "click", "params": {"node_id": "btn-submit"}, "id": 13});
        let resp_click = MuninRpcServer::dispatch(&server.supervisor, &req_click).await;
        assert_eq!(resp_click["result"]["clicked"], true);

        // 6. 测试 execute_steps（批量宏观步骤，本地快引擎闭环；点击后元素已消失 → 达成断言由 mock probe 兜底）
        let req_steps = json!({
            "jsonrpc": "2.0", "method": "execute_steps", "id": 16,
            "params": {"steps": [{"step_id": 1, "intent": "点击提交按钮", "expected_outcome": "提交成功"}]}
        });
        let resp_steps = MuninRpcServer::dispatch(&server.supervisor, &req_steps).await;
        assert_eq!(resp_steps["result"]["overall_success"], true);
        assert_eq!(resp_steps["result"]["steps"][0]["step_id"], 1);

        // 7. execute_steps 参数校验
        let req_bad = json!({"jsonrpc": "2.0", "method": "execute_steps", "params": {}, "id": 17});
        let resp_bad = MuninRpcServer::dispatch(&server.supervisor, &req_bad).await;
        assert_eq!(resp_bad["error"]["code"], -32602);

        // 8. 测试未知方法
        let req_unknown = json!({"jsonrpc": "2.0", "method": "unknown_foo", "id": 14});
        let resp_unknown = MuninRpcServer::dispatch(&server.supervisor, &req_unknown).await;
        assert_eq!(resp_unknown["error"]["code"], -32601);

        Ok(())
    }
}
