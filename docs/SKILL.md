# Munin - AI Agent 交互指南

Rust 反射驱动双引擎浏览器自动化框架。System 1（Laya 10ms 反射）+ System 2（宏观规划 LLM/RPC）。

## 调用原则

MUST 优先使用 JSON-RPC 2.0 API（默认 `http://127.0.0.1:9090`）。
降级路径：确定性批量回归用 `munin test`；独立单次命令用 CLI。

## 首选工作流：长流程规划 + 批量执行

反模式（MUST NOT）：逐动作 RPC 往返（click → 执行 → 截图 → LLM 分析 → 再发）。每动作 2~5s LLM 延迟，快模型优势归零。

推荐模式：LLM 两次调用完成任务。

```
1. POST get_action_map {"goal": "..."}
   → 返回可操作元素表 + 页面文本（LLM 规划所需全部上下文）
2. LLM 内部规划 N 步（无 RPC）
3. POST execute_steps {"steps": [...]}
   → 快引擎本地闭环执行（达成判定/弹窗自愈/置信度门控），返回全部步骤报告
```

## 调用模板（curl）

```bash
# 1. 拉取规划视图（唯一需要 LLM 决策的输入）
curl -s http://127.0.0.1:9090 -d '{"jsonrpc":"2.0","method":"get_action_map","params":{"goal":"<完整任务目标>"},"id":1}'

# 2. 批量执行（LLM 基于上一步 action_map 一次性规划全部步骤）
curl -s http://127.0.0.1:9090 -d '{"jsonrpc":"2.0","method":"execute_steps","params":{"steps":[{"step_id":1,"intent":"...","expected_outcome":"..."}]},"id":2}'
```

MUST NOT 调用 `click`/`fill`/`probe` 逐步试探。这些仅供开发者调试。

## 场景选择

| 场景 | 方法 |
| :--- | :--- |
| 已知页面结构，执行一串操作 | `get_action_map` + `execute_steps` |
| 完全未知的开放目标 | `execute_goal` |
| 单点调试 | `click` / `fill` / `probe` |
| 批量确定性回归 | `munin test scenario.yaml` |

## RPC API

请求头：`Content-Type: application/json`

### `get_action_map`
拉取修剪后可操作元素表 + 页面文本。响应含 `next_action` 字段，指示 LLM 下一步必须执行的动作。
```json
{"jsonrpc": "2.0", "method": "get_action_map", "params": {"goal": "登录并查询车牌湘A90010", "max": 30}, "id": 1}
```
响应：
```json
{
  "goal": "...",
  "page_text": "页面可见文本...",
  "count": 4,
  "action_map": [
    {"node_id": "inp-user", "tag": "input", "text": "", "attributes": {"placeholder": "用户名"}},
    {"node_id": "btn-login", "tag": "button", "text": "登录", "attributes": {}}
  ],
  "next_action": "MANDATORY: Plan ALL steps NOW using action_map + page_text. Call execute_steps ONCE. DO NOT call click/fill/probe individually."
}
```

### `execute_steps`
批量执行 LLM 规划的宏观步骤。某步失败即中止，`overall_success=false`，`error` 字段含原因。
```json
{
  "jsonrpc": "2.0",
  "method": "execute_steps",
  "params": {
    "steps": [
      {"step_id": 1, "intent": "填写用户名 admin", "expected_outcome": "用户名已填入"},
      {"step_id": 2, "intent": "点击登录按钮", "expected_outcome": "跳转到主页"}
    ]
  },
  "id": 2
}
```
响应：
```json
{
  "overall_success": true,
  "total_duration_ms": 2140,
  "steps": [{"step_id": 1, "success": true, "confidence": 0.96, "actions_taken": [], "error": null}]
}
```

### `execute_goal`
委托 Munin 双引擎全权闭环执行自然语言目标。
```json
{"jsonrpc": "2.0", "method": "execute_goal", "params": {"goal": "...", "url": "..."}, "id": 3}
```

### `navigate`
```json
{"jsonrpc": "2.0", "method": "navigate", "params": {"url": "..."}, "id": 4}
```

### `get_interactive_elements`
提取交互 DOM。`prune: true` 时按 goal 裁剪 Top-K。
```json
{"jsonrpc": "2.0", "method": "get_interactive_elements", "params": {"prune": true, "goal": "..."}, "id": 5}
```

### `click` / `fill`
```json
{"jsonrpc": "2.0", "method": "click", "params": {"node_id": "btn-search"}, "id": 6}
{"jsonrpc": "2.0", "method": "fill", "params": {"node_id": "inp-plate", "text": "湘A90010"}, "id": 7}
```

### `probe`
语义断言，返回 `{"passed": bool, "confidence": float}`。
```json
{"jsonrpc": "2.0", "method": "probe", "params": {"assertion": "页面是否显示目标内容？"}, "id": 8}
```

### `screenshot`
返回 `{"base64": "..."}` 视口截图。
```json
{"jsonrpc": "2.0", "method": "screenshot", "params": {}, "id": 9}
```

### `bust_overlays`
识别并消解弹窗/遮罩/Cookie 授权。
```json
{"jsonrpc": "2.0", "method": "bust_overlays", "params": {}, "id": 10}
```

### `ping`
```json
{"jsonrpc": "2.0", "method": "ping", "id": 11}
```

### `evaluate_js`
```json
{"jsonrpc": "2.0", "method": "evaluate_js", "params": {"script": "..."}, "id": 12}
```

## Munin 作为 RPC 客户端

`munin.toml` 配置 `[slow_engine] provider = "rpc"` 时，外部服务 MUST 实现：
- `plan` — 输入 `{"user_goal", "context"}`，输出 `[{"step_id", "intent", "expected_outcome"}]`
- `arbitrate` — 输入阻塞与 DOM，输出 `{"node_id": "..."}`

## CLI 完整命令参考（MUST NOT 再执行 `--help`）

```
munin <COMMAND>

Commands:
  open     打开浏览器并导航至 URL
  run      双引擎执行自然语言目标
  test     执行声明式 YAML 场景（0 LLM 开销）
  serve    启动 JSON-RPC 2.0 服务
  demo     独立反射演示
  install  交互式初始化配置 + 安装 Agent Skill
```

### munin serve
```
munin serve [OPTIONS]
  --listen <LISTEN>    RPC 监听地址 (default: 127.0.0.1:9090)
  --config <CONFIG>    配置文件路径 (default: munin.toml)
  --mock               使用 Mock 引擎（无外部依赖）
  --headless           无头模式
  --headed             有头模式
  --cdp <CDP>          连接已有 CDP 端点 (e.g. http://127.0.0.1:9222)
```

### munin test
```
munin test [OPTIONS] <scenario.yaml>
  --config <CONFIG>    配置文件路径
  --mock               使用 Mock 引擎
  --headless / --headed
  --cdp <CDP>          连接已有 CDP 端点
```

### munin run
```
munin run [OPTIONS] --url <URL> <GOAL>
  --url <URL>              目标页面 URL（必需）
  --config <CONFIG>        配置文件路径
  --mock                   使用 Mock 引擎
  --headless / --headed
  --cdp <CDP>              连接已有 CDP 端点
  --threshold <THRESHOLD>  置信度阈值 (default: 0.85)
```

### munin open
```
munin open [OPTIONS] <URL>
  --headless / --headed
  --cdp <CDP>          连接已有 CDP 端点
```

### munin install
```
munin install [OPTIONS]
  -y, --yes                跳过交互，全量默认值
  --config <CONFIG>        配置写入路径 (default: ~/.munin/munin.toml)
  --skill-dir <SKILL_DIR>  Skill 安装目录 (default: ~/.agents/skills/munin)
```

### 环境变量
| 变量 | 说明 | 默认 |
| :--- | :--- | :--- |
| `LAYA_ENDPOINT` | 快引擎 Laya 端点 | `http://127.0.0.1:8000` |
| `OPENAI_API_KEY` | 慢引擎 API Key | - |
| `OPENAI_BASE_URL` | OpenAI 兼容端点 | `https://api.openai.com/v1` |
| `OPENAI_MODEL` | 慢引擎模型名 | `gpt-4o` |
| `RUST_LOG` | 日志级别 | `info` |

## Agent Loop

### 首选（生产）

```
1. POST ping（失败则后台执行 munin serve）
2. POST navigate {"url": "..."}
3. POST get_action_map {"goal": "完整任务目标"}
   → 读取响应中 next_action 字段（MANDATORY 指令）
4. LLM 内部规划 steps（无 RPC）
5. POST execute_steps {"steps": [...]}
   → 全部成功：任务结束
   → 某步失败：读取 error，重新 get_action_map 局部重规划
```

### 降级（调试用）

```
1. ping → navigate
2. POST get_interactive_elements {"prune": true, "goal": "..."}
3. POST fill / click
4. POST probe {"assertion": "..."}
   → 未达成：循环 2~4
```

MUST NOT 在生产环境使用降级模式。
