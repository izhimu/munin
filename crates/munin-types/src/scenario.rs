use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

fn default_version() -> String {
    "1.0".to_string()
}

/// Munin 通用浏览器操作脚本规范 (Browser Operation Scenario Spec v1.0)
///
/// 声明式、语言无关、无特定业务绑定的通用浏览器交互流程规范。
/// 可用于表达任意业务流程：系统登录、表单录入、RPA 数据同步、端到端测试或健康度巡检。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scenario {
    /// 脚本规范版本 (默认 "1.0")
    #[serde(default = "default_version")]
    pub version: String,
    /// 流程/场景名称
    pub name: String,
    /// 场景描述 (可选)
    #[serde(default)]
    pub description: Option<String>,
    /// 基础 URL 地址 (可选)
    #[serde(default)]
    pub base_url: Option<String>,
    /// 自定义全局变量字典 (可选，供多环境注入参数)
    #[serde(default)]
    pub variables: HashMap<String, String>,
    /// 顺序执行的浏览器步骤流 (登录、导航、操作均一视同仁作为标准步骤)
    #[serde(default)]
    pub steps: Vec<ScenarioStep>,
}

impl Scenario {
    /// 从 YAML、TOML 或 JSON 文件中解析通用脚本
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)?;

        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
            match ext {
                "yaml" | "yml" => serde_yaml::from_str(&content)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                "toml" => toml::from_str(&content)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                "json" => serde_json::from_str(&content)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                _ => serde_yaml::from_str(&content)
                    .or_else(|_| toml::from_str(&content))
                    .or_else(|_| serde_json::from_str(&content))
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            }
        } else {
            serde_yaml::from_str(&content)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        }
    }
}

/// 流程步骤单元
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioStep {
    /// 步骤语义名称
    pub name: String,
    /// 步骤中依次执行的动作序列
    #[serde(default)]
    pub actions: Vec<ScenarioAction>,

    // 扁平快捷语法支持
    #[serde(default)]
    pub navigate: Option<String>,
    #[serde(default)]
    pub click: Option<String>,
    #[serde(default)]
    pub fill: Option<FillAction>,
    #[serde(default)]
    pub wait_for: Option<String>,
    #[serde(default)]
    pub assert_text: Option<String>,
    #[serde(default)]
    pub assert_exists: Option<String>,
    #[serde(default)]
    pub sleep_ms: Option<u64>,
}

/// 表单填充动作定义
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FillAction {
    pub target: String,
    pub value: String,
}

/// 通用浏览器原子动作原语规范 (Browser Action Primitives)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScenarioAction {
    /// 页面导航
    Navigate { navigate: String },
    /// 元素点击 (支持 CSS 选择器、文本、Placeholder、ID 智能匹配)
    Click { click: String },
    /// 文本输入
    Fill { fill: FillAction },
    /// 元素悬停
    Hover { hover: String },
    /// 清空输入框
    Clear { clear: String },
    /// 键盘按键模拟 (如 "Enter", "Tab", "Escape")
    PressKey { press_key: String },
    /// 显式等待元素或文本就绪 (自动轮询)
    WaitFor { wait_for: String },
    /// 毫秒休眠
    Sleep { sleep: u64 },
    /// 执行页面 JS 脚本并获取结果
    Evaluate { evaluate: String },
    /// 断言页面包含文本
    AssertText { assert_text: String },
    /// 断言页面不包含文本
    AssertNotText { assert_not_text: String },
    /// 断言选择器对应的 DOM 存在
    AssertExists { assert_exists: String },
    /// 断言选择器对应的 DOM 不存在
    AssertNotExists { assert_not_exists: String },
    /// 断言页面标题匹配
    AssertTitle { assert_title: String },
    /// 断言当前页面 URL 匹配
    AssertUrl { assert_url: String },
    /// 毫秒级弹窗自愈检查
    BustOverlays { bust_overlays: bool },
    /// 视口截图保存
    Screenshot { screenshot: Option<String> },
}

/// 流程执行报告
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioReport {
    pub scenario_name: String,
    pub total_steps: usize,
    pub passed_steps: usize,
    pub failed_steps: usize,
    pub elapsed_ms: u128,
    pub step_results: Vec<StepResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepResult {
    pub step_name: String,
    pub success: bool,
    pub elapsed_ms: u128,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scenario_yaml_roundtrip() {
        let yaml_str = r##"
version: "1.0"
name: "标准浏览器业务脚本"
description: "演示通用步骤，登录亦作为首个普通步骤"
base_url: "https://example.com"
steps:
  - name: "系统身份认证"
    actions:
      - navigate: "https://example.com/login"
      - fill:
          target: "#username"
          value: "admin"
      - fill:
          target: "#password"
          value: "secret"
      - click: "#btn-submit"
      - wait_for: "欢迎回来"
  - name: "业务数据查询"
    actions:
      - click: "报表中心"
      - assert_text: "统计报表"
      - screenshot: "target/report.png"
"##;
        let scenario: Scenario = serde_yaml::from_str(yaml_str).expect("parse yaml failed");
        assert_eq!(scenario.version, "1.0");
        assert_eq!(scenario.steps.len(), 2);
        assert_eq!(scenario.steps[0].name, "系统身份认证");
        assert_eq!(scenario.steps[1].name, "业务数据查询");
    }
}
