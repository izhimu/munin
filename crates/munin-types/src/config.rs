use serde::{Deserialize, Serialize};
use std::path::Path;

fn default_fast_provider() -> String {
    "laya".to_string()
}

fn default_laya_endpoint() -> String {
    "http://127.0.0.1:8000".to_string()
}

fn default_laya_model() -> String {
    "multilingual".to_string()
}

fn default_confidence_threshold() -> f32 {
    0.85
}

fn default_fast_timeout_ms() -> u64 {
    5000
}

fn default_slow_provider() -> String {
    "rpc".to_string()
}

fn default_slow_endpoint() -> String {
    "http://127.0.0.1:50051".to_string()
}

fn default_slow_timeout_ms() -> u64 {
    60000
}

fn default_true() -> bool {
    true
}

fn default_window_width() -> u32 {
    1920
}

fn default_window_height() -> u32 {
    1080
}

fn default_rpc_listen() -> String {
    "127.0.0.1:9090".to_string()
}

/// 快引擎配置 (System 1: Laya 极速推理服务)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FastEngineConfig {
    /// 驱动提供者名称 (如 "laya", "mock")
    #[serde(default = "default_fast_provider")]
    pub provider: String,
    /// Laya 服务的 HTTP 根地址
    #[serde(default = "default_laya_endpoint")]
    pub endpoint: String,
    /// 使用的模型名称
    #[serde(default = "default_laya_model")]
    pub model: String,
    /// 决策通过所需的最低置信度阈值 (默认 0.85)
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f32,
    /// 请求超时时间 (毫秒)
    #[serde(default = "default_fast_timeout_ms")]
    pub timeout_ms: u64,
}

impl Default for FastEngineConfig {
    fn default() -> Self {
        Self {
            provider: default_fast_provider(),
            endpoint: default_laya_endpoint(),
            model: default_laya_model(),
            confidence_threshold: default_confidence_threshold(),
            timeout_ms: default_fast_timeout_ms(),
        }
    }
}

/// 慢引擎配置 (System 2: 宏观规划与疑难仲裁大模型)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlowEngineConfig {
    /// 提供者类型: "rpc" (标准 JSON-RPC 2.0) 或 "openai" (兼容 OpenAI REST 接口)
    #[serde(default = "default_slow_provider")]
    pub provider: String,
    /// RPC 服务地址 (当 provider 为 "rpc" 时使用)
    #[serde(default = "default_slow_endpoint")]
    pub endpoint: String,
    /// 请求超时时间 (毫秒)
    #[serde(default = "default_slow_timeout_ms")]
    pub timeout_ms: u64,
    /// OpenAI 鉴权密钥 (当 provider 为 "openai" 时使用)
    pub api_key: Option<String>,
    /// OpenAI API 基础地址 (当 provider 为 "openai" 时使用)
    pub base_url: Option<String>,
    /// OpenAI 模型名称 (例如 "gpt-4o", "deepseek-chat")
    pub model: Option<String>,
}

impl Default for SlowEngineConfig {
    fn default() -> Self {
        Self {
            provider: default_slow_provider(),
            endpoint: default_slow_endpoint(),
            timeout_ms: default_slow_timeout_ms(),
            api_key: None,
            base_url: None,
            model: None,
        }
    }
}

/// 本项目对外暴露的 RPC 服务配置 (供外部大模型作为 Client 与本项目交互)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcServerConfig {
    /// 是否开启 RPC 服务监听
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// RPC 监听地址与端口
    #[serde(default = "default_rpc_listen")]
    pub listen: String,
}

impl Default for RpcServerConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            listen: default_rpc_listen(),
        }
    }
}

/// 浏览器基础运行配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserProfileConfig {
    /// 是否以无头模式运行
    #[serde(default = "default_true")]
    pub headless: bool,
    /// 窗口宽度 (默认 1920)
    #[serde(default = "default_window_width")]
    pub window_width: u32,
    /// 窗口高度 (默认 1080)
    #[serde(default = "default_window_height")]
    pub window_height: u32,
    /// 外部已有 CDP 调试端口 (可选)
    pub cdp_endpoint: Option<String>,
}

impl Default for BrowserProfileConfig {
    fn default() -> Self {
        Self {
            headless: default_true(),
            window_width: default_window_width(),
            window_height: default_window_height(),
            cdp_endpoint: None,
        }
    }
}

/// Munin 全局主配置
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MuninConfig {
    #[serde(default)]
    pub fast_engine: FastEngineConfig,
    #[serde(default)]
    pub slow_engine: SlowEngineConfig,
    #[serde(default)]
    pub rpc_server: RpcServerConfig,
    #[serde(default)]
    pub browser: BrowserProfileConfig,
}

impl MuninConfig {
    /// 从 TOML 字符串解析配置
    pub fn from_toml_str(content: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(content)
    }

    /// 从指定文件路径读取配置
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let content = std::fs::read_to_string(path)?;
        Self::from_toml_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// 尝试加载配置文件，优先使用用户指定路径，其次检查当前目录的 munin.toml / config.toml，否则返回默认配置
    pub fn load_or_default(custom_path: Option<&str>) -> Self {
        if let Some(path) = custom_path {
            if let Ok(cfg) = Self::from_file(path) {
                return cfg;
            }
        }
        for candidate in &["munin.toml", "config.toml"] {
            if Path::new(candidate).exists() {
                if let Ok(cfg) = Self::from_file(candidate) {
                    return cfg;
                }
            }
        }
        Self::default()
    }

    /// 将配置序列化为标准 TOML 格式
    pub fn to_toml_string(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = MuninConfig::default();
        assert_eq!(cfg.fast_engine.provider, "laya");
        assert_eq!(cfg.fast_engine.endpoint, "http://127.0.0.1:8000");
        assert_eq!(cfg.fast_engine.confidence_threshold, 0.85);
        assert_eq!(cfg.slow_engine.provider, "rpc");
        assert_eq!(cfg.slow_engine.endpoint, "http://127.0.0.1:50051");
        assert!(cfg.rpc_server.enabled);
        assert_eq!(cfg.rpc_server.listen, "127.0.0.1:9090");
    }

    #[test]
    fn test_parse_toml_string() {
        let toml_str = r#"
[fast_engine]
provider = "laya"
endpoint = "http://192.168.1.100:8000"
model = "fast-v2"
confidence_threshold = 0.90
timeout_ms = 3000

[slow_engine]
provider = "rpc"
endpoint = "http://127.0.0.1:60000"
timeout_ms = 45000

[rpc_server]
enabled = true
listen = "0.0.0.0:8888"

[browser]
headless = false
cdp_endpoint = "http://localhost:9222"
"#;
        let cfg = MuninConfig::from_toml_str(toml_str).expect("parse toml failed");
        assert_eq!(cfg.fast_engine.endpoint, "http://192.168.1.100:8000");
        assert_eq!(cfg.fast_engine.model, "fast-v2");
        assert_eq!(cfg.fast_engine.confidence_threshold, 0.90);
        assert_eq!(cfg.slow_engine.provider, "rpc");
        assert_eq!(cfg.slow_engine.endpoint, "http://127.0.0.1:60000");
        assert_eq!(cfg.rpc_server.listen, "0.0.0.0:8888");
        assert!(!cfg.browser.headless);
        assert_eq!(cfg.browser.cdp_endpoint.as_deref(), Some("http://localhost:9222"));
    }

    #[test]
    fn test_serialization_roundtrip() {
        let cfg = MuninConfig::default();
        let s = cfg.to_toml_string().expect("to toml failed");
        let parsed = MuninConfig::from_toml_str(&s).expect("from toml failed");
        assert_eq!(cfg, parsed);
    }
}
