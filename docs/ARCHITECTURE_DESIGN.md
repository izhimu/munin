# BiSystem-Browser：基于 Rust 的快慢双引擎通用浏览器自动化框架设计与开发方案

> **项目代号**：Libellula（蜻蜓）  
> **核心定位**：将毫秒级非自回归决策模型（System 1 快思考）与前沿大模型规划（System 2 慢思考）相融合的通用浏览器自动化与 E2E 自动化测试框架。  
> **开发语言**：Rust (2021 Edition)  
> **核心特性**：浏览器无关（Driver-Agnostic）、引擎可插拔（Engine-Pluggable）、零开销抽象、毫秒级交互反射。

---

## 目录

- [一、项目背景与设计哲学](#一项目背景与设计哲学)
- [二、系统整体架构全景](#二系统整体架构全景)
- [三、技术选型与 Workspace 划分](#三技术选型与-workspace-划分)
- [四、核心数据结构与 Trait 契约定义](#四核心数据结构与-trait-契约定义)
- [五、核心组件实现细节](#五核心组件实现细节)
  - [5.1 Laya 极速快引擎实现 (FastEngine)](#51-laya-极速快引擎实现-fastengine)
  - [5.2 统一协调器与置信度门控 (Supervisor)](#52-统一协调器与置信度门控-supervisor)
  - [5.3 毫秒级干扰自愈探针 (OverlayBuster)](#53-毫秒级干扰自愈探针-overlaybuster)
- [六、典型应用场景：E2E 自动化测试落地](#六典型应用场景e2e-自动化测试落地)
- [七、实施路线图与里程碑规划](#七实施路线图与里程碑规划)

---

## 一、项目背景与设计哲学

### 1.1 现有 Browser Agent 的痛点
以 Browser-Use、Stagehand、Playwright+GPT 为代表的传统大模型浏览器自动化工具，面临三大根本性瓶颈：
1. **速度慢（树懒式操作）**：每一步微小交互（点击按钮、关闭弹窗、页面滚动）均需调用大语言模型进行自回归生成，单步耗时 1.5s ~ 4s，无法满足交互式与高并发自动化测试需求；
2. **Token 与算力成本爆炸**：将整页 DOM 树或高分辨率视口截图逐轮输入 LLM，单次测试消耗数万至数十万 Token；
3. **脆弱性与幻觉**：缺乏严格校准的概率门控，遇到突发弹窗、页面异步加载、微小样式调整时容易陷入死循环或误判。

### 1.2 双系统设计哲学（Dual-System Architecture）
人类在操作浏览器时遵循认知科学的“双系统理论”：
- **System 1（快思考 / 条件反射）**：遇到烦人的 Cookie 授权、广告弹窗、找“下一页”或“提交”按钮、匹配表单字段时，大脑皮层几乎不思考，依赖视觉本能反射在 100ms 内点掉；
- **System 2（慢思考 / 逻辑规划）**：遇到复杂的跨表分析、长文信息汇总、异常排查时，才调动大脑进行严密推理与规划。

`BiSystem-Browser` 在架构上实现这种分工：
- **快引擎（Laya 等 10ms 级模型）**：本地 GPU 常驻，单次前向推理 10~20ms，承包 80% 的机械交互、弹窗自愈、候选元素修剪、达成状态断言；
- **慢引擎（DeepSeek / Claude / GPT）**：仅负责顶层宏观拆解与极端低置信度（< 0.85）时的疑难仲裁；
- **Rust 语言筑基**：依靠 Rust 无 GC、零成本抽象、Tokio 异步高吞吐特性，彻底消除胶水层开销，提供极致的运行速度与单二进制文件部署能力。

---

## 二、系统整体架构全景

```text
┌─────────────────────────────────────────────────────────────────────────┐
│                      BiSystem-Browser 统一架构全景图                    │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  [API / CLI / E2E 测试套件] (Rust 原生测试 / CI/CD 流水线 / YAML 工作流)  │
│                                  │                                      │
│                                  ▼                                      │
│  ┌───────────────────────────────────────────────────────────────────┐  │
│  │                    双引擎编排协调器 (Supervisor)                   │  │
│  │   • 宏观规划跟踪 (Macro Execution)     • 状态机流转 (FSM)          │  │
│  │   • 置信度门控 (Confidence Gating)     • 升级请示机制 (Escalation)  │  │
│  └─────────────────┬───────────────────────────────────▲─────────────┘  │
│                    │                                   │                │
│         下发阶段意图 │                                   │ 异常/低置信度   │
│                    ▼                                   │ 唤醒慢引擎     │
│  ┌───────────────────────────────┐     ┌───────────────┴─────────────┐  │
│  │  【慢引擎 Slow Engine】(规划)   │     │  【快引擎 Fast Engine】(反射) │  │
│  │  外部插拔 Trait: SlowEngine   │     │  外部插拔 Trait: FastEngine │  │
│  │  • DeepSeek / Claude / GPT    │     │  • Laya (30ms 决策, CUDA)   │  │
│  │  • 本地 Ollama / vLLM / Qwen  │     │  • 本地 ONNX 分类头 / 嵌入   │  │
│  └───────────────────────────────┘     └───────────────▲─────────────┘  │
│                                                        │                │
│                                  DOM 紧凑候选 / 探针输入 │ 毫秒级决策结果 │
│                                                        │ (10~20ms)      │
│                                  ▼                     │                │
│  ┌───────────────────────────────────────────────────────────────────┐  │
│  │                   感知、修剪与探针层 (Perception Layer)           │  │
│  │   • 干扰自愈探针 (OverlayBuster)   • 交互候选元素极速修剪 (Top-K)   │  │
│  │   • 目标断言探针 (StateProbe)      • 表单意图自适应对齐器           │  │
│  └─────────────────────────────────┬─────────────────────────────────┘  │
│                                    │                                    │
│                                    ▼                                    │
│  ┌───────────────────────────────────────────────────────────────────┐  │
│  │                  统一浏览器驱动抽象层 (Driver Layer)               │  │
│  │  外部插拔 Trait: BrowserDriver (CDP / Playwright / BiDi)          │  │
│  │  适配范围：Chrome / Edge / Firefox / WebKit / 无头实例 / 远程浏览器  │  │
│  └───────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## 三、技术选型与 Workspace 划分

工程采用标准 Cargo Workspace 组织结构：

```text
bisystem-browser/
├── Cargo.toml
├── crates/
│   ├── bisystem-types/       # 核心通用类型定义 (DOMNode, Step, Action, Decision)
│   ├── bisystem-driver/      # 驱动层 Trait 定义及具体实现 (CDP / BiDi)
│   ├── bisystem-engine/      # FastEngine 与 SlowEngine Trait 及外部适配器
│   ├── bisystem-perception/  # 弹窗自愈、候选修剪、状态探针等中间件
│   ├── bisystem-core/        # 核心协调器 (Supervisor)、状态机与门控逻辑
│   └── bisystem-cli/         # 编译出的独立 CLI 工具 (支持运行脚本或测试)
└── tests/
    └── e2e_test.rs           # 完整端到端自动化测试用例
```

### 关键依赖库选型

- **异步运行时**：`tokio = { version = "1.40", features = ["full"] }`
- **CDP 驱动实现**：`chromiumoxide = "0.6"` 或 `tokio-tungstenite = "0.23"`
- **HTTP 客户端**：`reqwest = { version = "0.12", features = ["json", "rustls-tls"] }`
- **序列化反序列化**：`serde = { version = "1.0", features = ["derive"] }`, `serde_json = "1.0"`
- **异步 Trait 支持**：`async-trait = "0.1"`
- **错误处理**：`thiserror = "1.0"`, `anyhow = "1.0"`
- **结构化日志跟踪**：`tracing = "0.1"`, `tracing-subscriber = "0.3"`

---

## 四、核心数据结构与 Trait 契约定义

### 4.1 核心数据结构 (`bisystem-types/src/lib.rs`)

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 轻量化 DOM 交互节点（剥离无关冗余样式，聚焦交互语义）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DOMElementNode {
    pub node_id: String,
    pub tag: String,
    pub text: String,
    pub attributes: HashMap<String, String>,
}

/// 慢引擎规划的宏观步骤
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacroStep {
    pub step_id: usize,
    pub intent: String,
    pub expected_outcome: String,
}

/// 快引擎决策输出原语
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FastDecision {
    Choice { target_key: String, confidence: f32 },
    Score { score: f32, confidence: f32 },
    Probe { result: bool, probability: f32 },
}
```

### 4.2 浏览器驱动契约 (`bisystem-driver/src/lib.rs`)

```rust
use async_trait::async_trait;
use bisystem_types::DOMElementNode;
use anyhow::Result;

#[async_trait]
pub trait BrowserDriver: Send + Sync {
    /// 启动或连接浏览器实例
    async fn launch(&mut self, headless: bool) -> Result<()>;
    /// 页面跳转
    async fn goto(&mut self, url: &str) -> Result<()>;
    /// 提取当前视口具备语义的可交互节点列表
    async fn get_interactive_elements(&self) -> Result<Vec<DOMElementNode>>;
    /// 模拟交互动作
    async fn click(&self, node_id: &str) -> Result<()>;
    async fn fill(&self, node_id: &str, text: &str) -> Result<()>;
    /// 执行页面 JS 脚本
    async fn evaluate_js(&self, script: &str) -> Result<serde_json::Value>;
    /// 视口截图（供慢引擎多模态使用）
    async fn take_screenshot(&self) -> Result<Vec<u8>>;
}
```

### 4.3 双引擎契约 (`bisystem-engine/src/lib.rs`)

```rust
use async_trait::async_trait;
use bisystem_types::MacroStep;
use anyhow::Result;
use std::collections::HashMap;

/// 快引擎接口 (System 1)：低延迟、确定性输出、置信度统计
#[async_trait]
pub trait FastEngine: Send + Sync {
    /// 单选决策：分类/目标点击选择
    async fn choice(
        &self,
        state: &serde_json::Value,
        instructions: &str,
        criteria: &HashMap<String, String>,
    ) -> Result<(String, f32)>;

    /// 序数评分决策：等级评估/情绪/危害打分
    async fn score(
        &self,
        state: &serde_json::Value,
        instructions: &str,
        criteria: &[String],
    ) -> Result<(f32, f32)>;

    /// 布尔断言决策(noul)：真假/达成概率探测
    async fn probe(
        &self,
        state: &serde_json::Value,
        assertion: &str,
    ) -> Result<(bool, f32)>;
}

/// 慢引擎接口 (System 2)：宏观分解、策略调整、疑难仲裁
#[async_trait]
pub trait SlowEngine: Send + Sync {
    /// 宏观目标拆解为执行流
    async fn plan(&self, user_goal: &str, context: &str) -> Result<Vec<MacroStep>>;

    /// 异常仲裁：在快引擎受阻时介入决策
    async fn arbitrate(
        &self,
        step_intent: &str,
        current_dom_desc: &str,
        screenshot: Option<&[u8]>,
    ) -> Result<String>;
}
```

---

## 五、核心组件实现细节

### 5.1 Laya 极速快引擎实现 (FastEngine)

基于 Rust `reqwest` 连接本地常驻的 Laya API 服务（`127.0.0.1:8000`）。启用连接池与 TCP_NODELAY，网络交互开销控制在 0.5ms 内：

```rust
use async_trait::async_trait;
use serde_json::json;
use crate::FastEngine;
use anyhow::{Context, Result};
use std::collections::HashMap;

pub struct LayaFastEngine {
    endpoint: String,
    client: reqwest::Client,
}

impl LayaFastEngine {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            client: reqwest::Client::builder()
                .tcp_nodelay(true)
                .pool_max_idle_per_host(10)
                .build()
                .unwrap(),
        }
    }
}

#[async_trait]
impl FastEngine for LayaFastEngine {
    async fn choice(
        &self,
        state: &serde_json::Value,
        instructions: &str,
        criteria: &HashMap<String, String>,
    ) -> Result<(String, f32)> {
        let payload = json!({
            "model": "multilingual",
            "state": state,
            "questions": {
                "q": {
                    "type": "choice",
                    "instructions": instructions,
                    "criteria": criteria
                }
            }
        });

        let resp: serde_json::Value = self.client
            .post(format!("{}/predict", self.endpoint))
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;

        let ans = &resp["answers"]["q"];
        let choice = ans["choice"].as_str().context("Missing choice field")?.to_string();
        let conf = ans["confidence"].as_f64().unwrap_or(0.0) as f32;
        Ok((choice, conf))
    }

    async fn probe(
        &self,
        state: &serde_json::Value,
        assertion: &str,
    ) -> Result<(bool, f32)> {
        let payload = json!({
            "model": "multilingual",
            "state": state,
            "questions": {
                "q": {
                    "type": "noul",
                    "instructions": assertion
                }
            }
        });

        let resp: serde_json::Value = self.client
            .post(format!("{}/predict", self.endpoint))
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;

        let prob = resp["answers"]["q"]["noul"].as_f64().unwrap_or(0.0) as f32;
        Ok((prob > 0.5, prob))
    }

    async fn score(
        &self,
        state: &serde_json::Value,
        instructions: &str,
        criteria: &[String],
    ) -> Result<(f32, f32)> {
        let payload = json!({
            "model": "multilingual",
            "state": state,
            "questions": {
                "q": {
                    "type": "score",
                    "instructions": instructions,
                    "criteria": criteria
                }
            }
        });

        let resp: serde_json::Value = self.client
            .post(format!("{}/predict", self.endpoint))
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;

        let ans = &resp["answers"]["q"];
        let score = ans["score"].as_f64().unwrap_or(0.0) as f32;
        let conf = ans["confidence"].as_f64().unwrap_or(0.0) as f32;
        Ok((score, conf))
    }
}
```

### 5.2 统一协调器与置信度门控 (Supervisor)

协调器作为运行中枢，利用静态泛型参数 `D, F, S` 消除虚函数表开销，保障极限执行效率：

```rust
use bisystem_driver::BrowserDriver;
use bisystem_engine::{FastEngine, SlowEngine};
use anyhow::Result;
use tracing::{info, warn};
use std::collections::HashMap;

pub struct BiSystemSupervisor<D: BrowserDriver, F: FastEngine, S: SlowEngine> {
    pub driver: D,
    pub fast: F,
    pub slow: S,
    pub confidence_threshold: f32,
}

impl<D: BrowserDriver, F: FastEngine, S: SlowEngine> BiSystemSupervisor<D, F, S> {
    pub fn new(driver: D, fast: F, slow: S, threshold: f32) -> Self {
        Self {
            driver,
            fast,
            slow,
            confidence_threshold: threshold,
        }
    }

    /// 执行通用自然语言任务目标
    pub async fn execute_goal(&mut self, goal: &str) -> Result<()> {
        info!("🧠 [System 2] 慢引擎进行宏观目标分解: '{}'", goal);
        let steps = self.slow.plan(goal, "Browser ready").await?;

        for step in steps {
            info!("▶ [Step {}] 阶段任务: {}", step.step_id, step.intent);
            let mut completed = false;
            let mut attempts = 0;

            while !completed && attempts < 5 {
                // 1. 毫秒级自愈环境（清理弹窗干扰）
                self.heal_overlays().await?;

                // 2. 状态探针：检测是否已达成阶段目标
                let page_snapshot = self.driver.evaluate_js("document.body.innerText.slice(0, 800)").await?;
                let (achieved, prob) = self.fast.probe(
                    &page_snapshot,
                    &format!("页面内容是否已表明：{}？", step.expected_outcome)
                ).await?;

                if achieved && prob >= self.confidence_threshold {
                    info!("✔ [System 1] 达成断言成功 (置信度: {:.1}%)", prob * 100.0);
                    completed = true;
                    break;
                }

                // 3. 提取候选交互元素
                let elements = self.driver.get_interactive_elements().await?;
                let mut criteria_map = HashMap::new();
                for el in elements.iter().take(30) {
                    criteria_map.insert(el.node_id.clone(), format!("{} [{}]", el.tag, el.text));
                }

                // 4. 快引擎 10ms 锁定操作目标
                let (target_id, conf) = self.fast.choice(
                    &serde_json::json!({ "goal": step.intent }),
                    "哪个元素是推进当前任务目标最匹配的交互项？",
                    &criteria_map
                ).await?;

                // 5. 置信度门控仲裁
                if conf >= self.confidence_threshold {
                    info!("⚡ [System 1] 执行点击 -> {} (置信度: {:.1}%)", target_id, conf * 100.0);
                    self.driver.click(&target_id).await?;
                } else {
                    warn!("⚠️ 快引擎置信度偏低 ({:.1}%)，唤醒慢引擎介入仲裁...", conf * 100.0);
                    let fallback_action = self.slow.arbitrate(
                        &step.intent,
                        &format!("{:?}", criteria_map),
                        None
                    ).await?;
                    // 执行慢引擎给出的纠偏动作...
                }

                attempts += 1;
                tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
            }
        }
        Ok(())
    }

    /// 弹窗自愈逻辑
    pub async fn heal_overlays(&self) -> Result<()> {
        let elements = self.driver.get_interactive_elements().await?;
        let candidates: Vec<_> = elements.into_iter()
            .filter(|e| ["关闭", "跳过", "拒绝", "Close", "Dismiss", "Accept"].iter().any(|&k| e.text.contains(k)))
            .collect();

        if !candidates.is_empty() {
            let mut crit = HashMap::new();
            for c in &candidates {
                crit.insert(c.node_id.clone(), c.text.clone());
            }
            let (target_id, conf) = self.fast.choice(
                &serde_json::json!("Modal overlay present"),
                "Which button closes or dismisses the overlay?",
                &crit
            ).await?;

            if conf >= 0.80 {
                info!("⚡ 弹窗自愈：自动清理遮罩层 -> {}", target_id);
                self.driver.click(&target_id).await?;
            }
        }
        Ok(())
    }
}
```

---

## 六、典型应用场景：E2E 自动化测试落地

利用 Rust 原生单测框架，编写兼具**高执行速度**与**强语义弹性**的端到端自动化测试：

```rust
#[cfg(test)]
mod tests {
    use bisystem_core::BiSystemSupervisor;
    use bisystem_driver::cdp::CdpDriver;
    use bisystem_engine::fast::laya::LayaFastEngine;
    use bisystem_engine::slow::openai::OpenAISlowEngine;

    #[tokio::test]
    async fn test_patrol_dispatch_flow_e2e() -> anyhow::Result<()> {
        tracing_subscriber::fmt::init();

        // 1. 装配驱动与双引擎
        let driver = CdpDriver::connect_or_launch("http://127.0.0.1:9222").await?;
        let fast_engine = LayaFastEngine::new("http://127.0.0.1:8000");
        let slow_engine = OpenAISlowEngine::new(std::env::var("OPENAI_API_KEY")?);

        let mut supervisor = BiSystemSupervisor::new(driver, fast_engine, slow_engine, 0.85);

        // 2. 访问智慧养护工单系统
        supervisor.driver.goto("http://192.168.2.109:8088/dispatch").await?;

        // 3. 执行自然语言业务流（自动消解弹窗、完成表单映射与提交）
        supervisor.execute_goal("以巡检员身份登录，提交一起K12处护栏损毁报告并指派给特种抢修队").await?;

        // 4. 快引擎原生语义断言（10ms 给出概率判断，免除脆弱的 sleep 等待）
        let page_text = supervisor.driver.evaluate_js("document.body.innerText").await?;
        let (assert_success, prob) = supervisor.fast.probe(
            &page_text,
            "页面是否提示工单已成功派发并进入流转状态？"
        ).await?;

        assert!(assert_success, "工单派发断言失败");
        assert!(prob >= 0.90, "断言置信度不足");
        Ok(())
    }
}
```

---

## 七、实施路线图与里程碑规划

| 阶段 | 交付核心 | 周期 | 核心验收指标 |
| :--- | :--- | :--- | :--- |
| **M1: 核心契约与驱动打通** | 完成 `bisystem-types`，通过 `chromiumoxide` 实现 `CdpDriver`，接入 `LayaFastEngine`。 | 1 周 | 纯 Rust 成功连接 Chrome/Edge，并完成 10ms 级别的点击与填表。 |
| **M2: 自愈与感知中间件** | 实现 `OverlayBuster`（弹窗自愈）、`FormMapper`（表单自适应匹配）与状态探针。 | 1 周 | 在包含复杂干扰弹窗的页面上实现 100% 自动无感关闭。 |
| **M3: 协调器与置信度门控** | 交付 `BiSystemSupervisor`、状态机仲裁与慢引擎（OpenAI/Claude）集成。 | 1 周 | 形成“慢拆解 ➜ 快执行 ➜ 低置信度升级 ➜ 达成断言”的完整闭环。 |
| **M4: E2E 框架与 CLI 发布** | 交付 `bisystem-cli`，编写集成测试用例，支持作为独立可执行文件在 CI/CD 中运行。 | 1 周 | 单一无依赖可执行文件，可在 Alpine/Ubuntu 镜像中极速启动测试。 |

---

## 八、方案总结

`BiSystem-Browser` 通过 Rust 语言的底层性能优势，将本地常驻的 Laya（RTX 4060 GPU，10~20ms）与云端大模型的能力解耦并有机融合：
1. **执行提速 10~20 倍**：常规单步交互从 2~3 秒骤降至 10~30 毫秒；
2. **Token 消耗锐减 90%**：绝大多数微观决策在本地免费闭环完成；
3. **架构极具通用性**：上层支持无缝切换浏览器（Chrome/Edge/Firefox）与底层模型，为现代 Web 自动化与测试工程提供划时代的生产力。
