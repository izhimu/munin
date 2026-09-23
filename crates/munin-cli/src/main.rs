use anyhow::{anyhow, Result};
use clap::{Args, CommandFactory, Parser, Subcommand};
use munin_core::{BiSystemSupervisor, MuninRpcServer, Scenario, ScenarioRunner};
use munin_driver::cdp::CdpDriver;
use munin_driver::mock::MockDriver;
use munin_driver::BrowserDriver;
use munin_engine::fast::laya::LayaFastEngine;
use munin_engine::fast::mock::MockFastEngine;
use munin_engine::slow::mock::MockSlowEngine;
use munin_engine::slow::openai::OpenAISlowEngine;
use munin_engine::slow::rpc::RpcSlowEngine;
use munin_types::config::MuninConfig;
use munin_types::{DOMElementNode, MacroStep};
use std::env;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

const AFTER_HELP: &str = r#"CONFIGURATION FILE:
  munin.toml                 Configures fast model (Laya), slow model RPC/OpenAI, and server

ENVIRONMENT VARIABLES:
  OPENAI_API_KEY             API key for System 2 slow macro planner (if using OpenAI)
  OPENAI_BASE_URL            Base URL for OpenAI-compatible endpoint
  OPENAI_MODEL               Model name for slow planner (default: gpt-4o)
  LAYA_ENDPOINT              Endpoint for System 1 fast reflexes (default: http://127.0.0.1:8000)
  RUST_LOG                   Log level filter (e.g. info, debug)"#;

/// Munin CLI - Reflex-Driven Browser Automation Framework
#[derive(Parser)]
#[command(name = "munin", version, after_help = AFTER_HELP)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Open browser and navigate to URL
    Open(OpenArgs),
    /// Execute natural language goal with Dual-Engine
    Run(RunArgs),
    /// Execute declarative test scenario internally (0 LLM overhead)
    Test(TestArgs),
    /// Start JSON-RPC 2.0 service for external slow models/agents
    Serve(ServeArgs),
    /// Stop a running 'munin serve' instance
    Stop(StopArgs),
    /// Restart a running 'munin serve' instance with its original arguments
    Restart(StopArgs),
    /// Run standalone reflex demonstration
    Demo,
    /// Interactively initialize configuration and install Agent Skill
    Install(InstallArgs),
}

#[derive(Args)]
struct OpenArgs {
    /// Target webpage URL
    url: String,
    /// Run browser in background without window
    #[arg(long, conflicts_with = "headed")]
    headless: bool,
    /// Show browser window while running
    #[arg(long)]
    headed: bool,
    /// Connect to existing CDP endpoint (e.g. http://127.0.0.1:9222)
    #[arg(long)]
    cdp: Option<String>,
}

#[derive(Args)]
struct RunArgs {
    /// Natural language goal to execute
    goal: String,
    /// Target webpage URL (required for 'run')
    #[arg(long)]
    url: String,
    /// Path to munin.toml configuration file (default: munin.toml)
    #[arg(long)]
    config: Option<String>,
    /// Use mock engines (zero external API keys or services required)
    #[arg(long)]
    mock: bool,
    /// Run browser in background without window
    #[arg(long, conflicts_with = "headed")]
    headless: bool,
    /// Show browser window while running
    #[arg(long)]
    headed: bool,
    /// Connect to existing CDP endpoint (e.g. http://127.0.0.1:9222)
    #[arg(long)]
    cdp: Option<String>,
    /// Confidence threshold (default: 0.85 or from config)
    #[arg(long)]
    threshold: Option<f32>,
}

#[derive(Args)]
struct TestArgs {
    /// Declarative scenario file (YAML)
    #[arg(name = "scenario.yaml")]
    scenario: String,
    /// Path to munin.toml configuration file (default: munin.toml)
    #[arg(long)]
    config: Option<String>,
    /// Use mock engines (zero external API keys or services required)
    #[arg(long)]
    mock: bool,
    /// Run browser in background without window
    #[arg(long, conflicts_with = "headed")]
    headless: bool,
    /// Show browser window while running
    #[arg(long)]
    headed: bool,
    /// Connect to existing CDP endpoint (e.g. http://127.0.0.1:9222)
    #[arg(long)]
    cdp: Option<String>,
}

#[derive(Args)]
struct StopArgs {
    /// Listening host and port of the instance to stop (default: 127.0.0.1:9090)
    #[arg(long)]
    listen: Option<String>,
}

#[derive(Args)]
struct ServeArgs {
    /// Listening host and port for RPC server (default: 127.0.0.1:9090)
    #[arg(long)]
    listen: Option<String>,
    /// Path to munin.toml configuration file (default: munin.toml)
    #[arg(long)]
    config: Option<String>,
    /// Use mock engines (zero external API keys or services required)
    #[arg(long)]
    mock: bool,
    /// Run browser in background without window
    #[arg(long, conflicts_with = "headed")]
    headless: bool,
    /// Show browser window while running
    #[arg(long)]
    headed: bool,
    /// Connect to existing CDP endpoint (e.g. http://127.0.0.1:9222)
    #[arg(long)]
    cdp: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let res = tokio::select! {
        res = async {
            match cli.command {
                Some(Commands::Open(args)) => cmd_open(args).await,
                Some(Commands::Run(args)) => cmd_run(args).await,
                Some(Commands::Serve(args)) => cmd_serve(args).await,
                Some(Commands::Stop(args)) => cmd_stop(args).await,
                Some(Commands::Restart(args)) => cmd_restart(args).await,
                Some(Commands::Test(args)) => cmd_test(args).await,
                Some(Commands::Demo) => cmd_demo().await,
                Some(Commands::Install(args)) => cmd_install(args).await,
                None => {
                    Cli::command().print_help()?;
                    println!();
                    Ok(())
                }
            }
        } => res,
        _ = tokio::signal::ctrl_c() => {
            println!("\n🛑 Received Ctrl+C, exiting...");
            Ok(())
        }
    };

    match res {
        Ok(()) => std::process::exit(0),
        Err(e) => Err(e),
    }
}

async fn init_logger() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                // chromiumoxide 对新版 Chromium CDP 扩展字段反序列化失败，WS Invalid message 刷屏
                EnvFilter::new("info,chromiumoxide::handler=error")
            }),
        )
        .try_init();
}

/// 归一化目标 URL：缺少协议头时补全 https://
fn normalize_url(url: String) -> String {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        format!("https://{}", url)
    } else {
        url
    }
}

/// 解析 --headless/--headed 互斥标志为可选覆盖值
fn headless_opt(headless: bool, headed: bool) -> Option<bool> {
    if headed {
        Some(false)
    } else if headless {
        Some(true)
    } else {
        None
    }
}

async fn cmd_open(args: OpenArgs) -> Result<()> {
    init_logger().await;

    // open 默认 headed，仅 --headless 显式开启无头模式
    let headless = headless_opt(args.headless, args.headed).unwrap_or(false);
    let normalized_url = normalize_url(args.url);

    println!("🚀 Launching browser...");
    let mut driver = match args.cdp {
        Some(endpoint) => {
            println!("🔌 Connecting to CDP at {}...", endpoint);
            CdpDriver::connect(endpoint).await?
        }
        None => {
            println!(
                "🌐 Launching local Chromium (mode: {})...",
                if headless { "headless" } else { "headed" }
            );
            CdpDriver::launch_headless(headless).await?
        }
    };

    println!("🔗 Navigating to {}...", normalized_url);
    driver.goto(&normalized_url).await?;

    println!("✔ Browser is open at: {}", normalized_url);
    println!("💡 Press Enter or Ctrl+C to close browser and exit.");

    wait_for_exit().await;
    drop(driver);
    Ok(())
}

async fn cmd_run(args: RunArgs) -> Result<()> {
    init_logger().await;

    let config = MuninConfig::load_or_default(args.config.as_deref());
    let normalized_url = normalize_url(args.url);

    let threshold = args.threshold.unwrap_or(config.fast_engine.confidence_threshold);
    let is_headless = headless_opt(args.headless, args.headed).unwrap_or(config.browser.headless);
    let cdp = args.cdp.or(config.browser.cdp_endpoint);

    println!("================================================================");
    println!("  🦅 Munin Dual-Engine Execution Pipeline");
    println!("================================================================");
    println!("• Goal:      {}", args.goal);
    println!("• Target:    {}", normalized_url);
    println!("• Threshold: {:.2}", threshold);
    println!("• Fast Eng:  {} ({})", config.fast_engine.provider, config.fast_engine.endpoint);
    println!("• Slow Eng:  {} ({})", config.slow_engine.provider, config.slow_engine.endpoint);
    println!("• Engine:    {}", if args.mock { "Mock Engines" } else { "Active Engines" });
    println!("----------------------------------------------------------------\n");

    let mut driver = match cdp {
        Some(endpoint) => {
            println!("🔌 Connecting to CDP at {}...", endpoint);
            CdpDriver::connect(endpoint).await?
        }
        None => {
            println!(
                "🌐 Launching Chromium (mode: {})...",
                if is_headless { "headless" } else { "headed" }
            );
            CdpDriver::launch_headless(is_headless).await?
        }
    };

    println!("🔗 Navigating to {}...", normalized_url);
    driver.goto(&normalized_url).await?;

    if args.mock {
        let fast_engine = MockFastEngine::new().with_probe_sequence(vec![
            (false, 0.20),
            (true, 0.95),
        ]);
        let slow_engine = MockSlowEngine::new().with_steps(vec![
            MacroStep::new(1, &args.goal, "任务完成"),
        ]);

        let mut supervisor = BiSystemSupervisor::new(driver, fast_engine, slow_engine, threshold);
        supervisor.execute_goal(&args.goal).await?;
    } else {
        let fast_endpoint = env::var("LAYA_ENDPOINT").unwrap_or_else(|_| config.fast_engine.endpoint.clone());
        let fast_engine = LayaFastEngine::with_model(fast_endpoint, &config.fast_engine.model);

        if config.slow_engine.provider == "rpc" {
            let slow_engine = RpcSlowEngine::with_timeout(
                &config.slow_engine.endpoint,
                Duration::from_millis(config.slow_engine.timeout_ms),
            );
            if let Err(e) = slow_engine.ping().await {
                eprintln!("⚠️  [WARN] 慢引擎 RPC 服务不可达 ({}): {}。execute_goal 将因仲裁失败而重试超限。", config.slow_engine.endpoint, e);
            }
            let mut supervisor = BiSystemSupervisor::new(driver, fast_engine, slow_engine, threshold)
                .with_slow_fallback_heuristic(config.slow_engine.fallback.as_deref() == Some("heuristic"));
            supervisor.execute_goal(&args.goal).await?;
        } else {
            let api_key = env::var("OPENAI_API_KEY")
                .ok()
                .or(config.slow_engine.api_key)
                .ok_or_else(|| {
                    anyhow!("OPENAI_API_KEY is not set in env or munin.toml. Pass --mock or configure RPC.")
                })?;
            let base_url = env::var("OPENAI_BASE_URL")
                .ok()
                .or(config.slow_engine.base_url)
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            let model = env::var("OPENAI_MODEL")
                .ok()
                .or(config.slow_engine.model)
                .unwrap_or_else(|| "gpt-4o".to_string());
            let slow_engine = OpenAISlowEngine::with_config(api_key, base_url, model);

            let mut supervisor = BiSystemSupervisor::new(driver, fast_engine, slow_engine, threshold)
                .with_slow_fallback_heuristic(config.slow_engine.fallback.as_deref() == Some("heuristic"));
            supervisor.execute_goal(&args.goal).await?;
        }
    }

    println!("\n✔ Task execution complete.");
    Ok(())
}

/// serve 实例记录文件路径（按监听端口区分多实例）
fn serve_record_path(listen: &str) -> std::path::PathBuf {
    let port = listen.rsplit(':').next().unwrap_or("9090");
    std::env::temp_dir().join(format!("munin-serve-{port}.json"))
}

/// 写入 serve 实例记录：PID + 原始 CLI 参数（供 restart 复现）
fn write_serve_record(listen: &str) {
    let record = serde_json::json!({
        "pid": std::process::id(),
        "args": std::env::args().skip(1).collect::<Vec<_>>(),
        "listen": listen,
    });
    let _ = std::fs::write(
        serve_record_path(listen),
        serde_json::to_string_pretty(&record).unwrap_or_default(),
    );
}

/// 读取并校验 serve 实例记录（PID 存活才返回）
fn read_live_record(listen: &str) -> Option<(u32, Vec<String>)> {
    let path = serve_record_path(listen);
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let pid = v.get("pid")?.as_u64()? as u32;
    let args: Vec<String> = v
        .get("args")?
        .as_array()?
        .iter()
        .filter_map(|a| a.as_str().map(|s| s.to_string()))
        .collect();
    // 校验 PID 存活（kill -0 语义）
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if alive {
        Some((pid, args))
    } else {
        let _ = std::fs::remove_file(&path);
        None
    }
}

async fn cmd_stop(args: StopArgs) -> Result<()> {
    let listen = args.listen.unwrap_or_else(|| "127.0.0.1:9090".to_string());
    match read_live_record(&listen) {
        Some((pid, _)) => {
            let status = std::process::Command::new("kill")
                .arg(pid.to_string())
                .status()?;
            if status.success() {
                let _ = std::fs::remove_file(serve_record_path(&listen));
                println!("🛑 已停止 munin serve (pid {pid}, listen {listen})");
                Ok(())
            } else {
                Err(anyhow!("kill {pid} failed"))
            }
        }
        None => Err(anyhow!("未找到运行中的 munin serve 实例 (listen {listen})")),
    }
}

async fn cmd_restart(args: StopArgs) -> Result<()> {
    let listen = args.listen.unwrap_or_else(|| "127.0.0.1:9090".to_string());
    let Some((pid, serve_args)) = read_live_record(&listen) else {
        return Err(anyhow!("未找到运行中的 munin serve 实例 (listen {listen})"));
    };
    // 停旧实例
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .status();
    let _ = std::fs::remove_file(serve_record_path(&listen));
    // 等端口释放
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    // 以原参数后台拉起新实例（脱离父进程，输出重定向到日志）
    let exe = std::env::current_exe()?;
    let log_path = std::env::temp_dir().join(format!(
        "munin-serve-{}.log",
        listen.rsplit(':').next().unwrap_or("9090")
    ));
    let log = std::fs::File::create(&log_path)?;
    let log_err = log.try_clone()?;
    let child = std::process::Command::new(exe)
        .args(&serve_args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err))
        .spawn()?;
    println!(
        "🔄 已重启 munin serve (旧 pid {pid} → 新 pid {}, listen {listen}), 日志: {}",
        child.id(),
        log_path.display()
    );
    Ok(())
}

async fn cmd_serve(args: ServeArgs) -> Result<()> {
    init_logger().await;

    let config = MuninConfig::load_or_default(args.config.as_deref());
    let listen_addr = args.listen.unwrap_or(config.rpc_server.listen);
    let is_headless = headless_opt(args.headless, args.headed).unwrap_or(config.browser.headless);
    let cdp = args.cdp.or(config.browser.cdp_endpoint);

    // 记录 PID 与原始参数，供 `munin stop` / `munin restart` 管理
    write_serve_record(&listen_addr);

    println!("================================================================");
    println!("  🦅 Munin JSON-RPC 2.0 Dual-Engine Server");
    println!("================================================================");
    println!("• RPC Listen: http://{}", listen_addr);
    println!("• Fast Eng:   {} ({})", config.fast_engine.provider, config.fast_engine.endpoint);
    println!("• Slow Eng:   {} ({})", config.slow_engine.provider, config.slow_engine.endpoint);
    println!("• Engine:     {}", if args.mock { "Mock Engines" } else { "Laya + Browser" });
    println!("----------------------------------------------------------------\n");
    println!("💡 Available JSON-RPC 2.0 Methods:");
    println!("  - get_action_map(goal, [max])        # LLM 规划视图（首选入口）");
    println!("  - execute_steps(steps[])            # 批量宏观步骤闭环");
    println!("  - execute_goal(goal, [url])");
    println!("  - navigate(url)");
    println!("  - get_interactive_elements([prune], [goal])");
    println!("  - click(node_id)");
    println!("  - fill(node_id, text)");
    println!("  - screenshot()");
    println!("  - bust_overlays()");
    println!("  - probe(assertion, [state])");
    println!("  - evaluate_js(script)");
    println!("  - pick_date(node_id, date)       # AntD DatePicker 宏");
    println!("  - select_option(node_id, label) # 统一下拉宏（原生 select + AntD/Element）");
    println!("  - wait_for(selector, [state], [timeout_ms])");
    println!("  - wait_for_network_idle([timeout_ms], [idle_ms])");
    println!("  - wait_for_stable()");
    println!("  - ping()\n");

    if args.mock {
        let mut driver = MockDriver::new();
        driver.launch(true).await?;
        let fast_engine = MockFastEngine::new().with_probe_sequence(vec![(true, 0.95)]);
        let slow_engine = MockSlowEngine::new();
        let supervisor = BiSystemSupervisor::new(
            driver,
            fast_engine,
            slow_engine,
            config.fast_engine.confidence_threshold,
        );
        let server = MuninRpcServer::new(supervisor, listen_addr);
        server.run().await?;
    } else {
        let mut driver = match cdp {
            Some(endpoint) => CdpDriver::connect(endpoint).await?,
            None => CdpDriver::launch_headless(is_headless).await?,
        };
        driver.launch(is_headless).await?;

        let fast_endpoint = env::var("LAYA_ENDPOINT").unwrap_or_else(|_| config.fast_engine.endpoint.clone());
        let fast_engine = LayaFastEngine::with_model(fast_endpoint, &config.fast_engine.model);

        if config.slow_engine.provider == "rpc" {
            let slow_engine = RpcSlowEngine::with_timeout(
                &config.slow_engine.endpoint,
                Duration::from_millis(config.slow_engine.timeout_ms),
            );
            // 启动时探测慢引擎可达性，避免僵死进程导致 execute_steps 硬编码失败
            if let Err(e) = slow_engine.ping().await {
                eprintln!("⚠️  [WARN] 慢引擎 RPC 服务不可达 ({}): {}。execute_steps 将因仲裁失败而重试超限。", config.slow_engine.endpoint, e);
                eprintln!("       请启动真实 LLM 仲裁服务，或改用 --mock / provider = \"openai\"。");
            }
            let supervisor = BiSystemSupervisor::new(
                driver,
                fast_engine,
                slow_engine,
                config.fast_engine.confidence_threshold,
            )
            .with_slow_fallback_heuristic(config.slow_engine.fallback.as_deref() == Some("heuristic"));
            let server = MuninRpcServer::new(supervisor, listen_addr);
            server.run().await?;
        } else {
            let api_key = env::var("OPENAI_API_KEY")
                .ok()
                .or(config.slow_engine.api_key)
                .ok_or_else(|| {
                    anyhow!("OPENAI_API_KEY is not set in env or munin.toml. Pass --mock or configure RPC.")
                })?;
            let base_url = env::var("OPENAI_BASE_URL")
                .ok()
                .or(config.slow_engine.base_url)
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            let model = env::var("OPENAI_MODEL")
                .ok()
                .or(config.slow_engine.model)
                .unwrap_or_else(|| "gpt-4o".to_string());
            let slow_engine = OpenAISlowEngine::with_config(api_key, base_url, model);

            let supervisor = BiSystemSupervisor::new(
                driver,
                fast_engine,
                slow_engine,
                config.fast_engine.confidence_threshold,
            )
            .with_slow_fallback_heuristic(config.slow_engine.fallback.as_deref() == Some("heuristic"));
            let server = MuninRpcServer::new(supervisor, listen_addr);
            server.run().await?;
        }
    }

    Ok(())
}

async fn cmd_demo() -> Result<()> {
    init_logger().await;

    println!("=================================================================");
    println!("  🦅 Munin (奥丁灵鸦) —— Rust 快慢双引擎演示");
    println!("=================================================================\n");

    let mut driver = MockDriver::new();
    driver.launch(true).await?;
    driver.goto("http://demo.local/patrol/dispatch").await?;

    driver.add_element(
        DOMElementNode::new("btn-ad-close", "button", "✖ 残忍拒绝并关闭窗口")
            .with_attribute("class", "modal-close-btn"),
    );
    driver.add_element(
        DOMElementNode::new("btn-clean-team", "button", "🌱 绿化保洁班组")
            .with_attribute("role", "button"),
    );
    driver.add_element(
        DOMElementNode::new("btn-emergency-team", "button", "🚒 高速特种应急抢修大队")
            .with_attribute("role", "button"),
    );
    driver.set_default_js_result(serde_json::json!(
        "现场巡检K45险情上报成功，特种应急抢修大队已接单调度！"
    ));

    let fast_engine = MockFastEngine::new().with_probe_sequence(vec![
        (false, 0.10),
        (true, 0.96),
    ]);
    fast_engine.map_choice_by_instruction("closes or dismisses", "btn-ad-close", 0.98);
    fast_engine.map_choice_by_instruction("最匹配的交互项", "btn-emergency-team", 0.94);

    let slow_engine = MockSlowEngine::new().with_steps(vec![MacroStep::new(
        1,
        "研判现场重大险情并指派特种应急大队",
        "特种应急抢修大队已接单调度",
    )]);

    let mut supervisor = BiSystemSupervisor::new(driver, fast_engine, slow_engine, 0.85)
        .with_step_delay_ms(50);

    supervisor
        .execute_goal("处理现场巡检上报的严重护栏撞毁事故，并指派给特种应急大队")
        .await?;

    println!("\n✔ 驱动记录的操作流水：");
    for (i, act) in supervisor.driver.recorded_actions().iter().enumerate() {
        println!("  [{}] {:?}", i + 1, act);
    }
    println!("\n🎉 演示闭环成功！");
    Ok(())
}

async fn cmd_test(args: TestArgs) -> Result<()> {
    init_logger().await;

    let scenario = Scenario::from_file(&args.scenario)?;
    let config = MuninConfig::load_or_default(args.config.as_deref());
    let is_headless = headless_opt(args.headless, args.headed).unwrap_or(config.browser.headless);
    let cdp = args.cdp.or(config.browser.cdp_endpoint);

    println!("================================================================");
    println!("  🦅 Munin Internal Scenario Test Runner");
    println!("================================================================");
    println!("• Scenario:   {}", scenario.name);
    println!("• File:       {}", args.scenario);
    println!("• Base URL:   {}", scenario.base_url.as_deref().unwrap_or("Defined in steps"));
    println!("• Steps:      {} step(s)", scenario.steps.len());
    println!("• Engine:     {}", if args.mock { "Mock Driver" } else { "CDP Browser + Laya" });
    println!("----------------------------------------------------------------\n");

    let report = if args.mock {
        let mut driver = MockDriver::new();
        driver.launch(true).await?;
        let fast_engine = MockFastEngine::new();
        let mut runner = ScenarioRunner::new(&mut driver, &fast_engine);
        runner.run(&scenario).await?
    } else {
        let mut driver = match cdp {
            Some(endpoint) => CdpDriver::connect(endpoint).await?,
            None => CdpDriver::launch_headless(is_headless).await?,
        };
        let fast_endpoint = env::var("LAYA_ENDPOINT").unwrap_or_else(|_| config.fast_engine.endpoint.clone());
        let fast_engine = LayaFastEngine::with_model(fast_endpoint, &config.fast_engine.model);

        let mut runner = ScenarioRunner::new(&mut driver, &fast_engine);
        runner.run(&scenario).await?
    };

    println!("\n================================================================");
    println!("  📋 测试用例执行摘要");
    println!("================================================================");
    println!("• 总用例数: {}", report.total_steps);
    println!("• 通过步骤: {}", report.passed_steps);
    println!("• 失败步骤: {}", report.failed_steps);
    println!("• 总耗时:   {}ms", report.elapsed_ms);
    println!("----------------------------------------------------------------");
    for (i, res) in report.step_results.iter().enumerate() {
        if res.success {
            println!("  [{}] ✔ {} ({}ms)", i + 1, res.step_name, res.elapsed_ms);
        } else {
            println!("  [{}] ✖ {} ({}ms) -> 错误: {}", i + 1, res.step_name, res.elapsed_ms, res.error.as_deref().unwrap_or(""));
        }
    }
    println!("================================================================\n");

    if report.failed_steps > 0 {
        Err(anyhow!("Scenario test suite failed: {} step(s) failed", report.failed_steps))
    } else {
        println!("🎉 测试流程全量通过！");
        Ok(())
    }
}

async fn wait_for_exit() {
    use tokio::io::AsyncBufReadExt;
    let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
    let mut line = String::new();
    if let Ok(n) = reader.read_line(&mut line).await {
        if n == 0 {
            std::future::pending::<()>().await;
        }
    }
    println!("🛑 Closing browser...");
}

const SKILL_MD_RAW: &str = include_str!("../../../docs/SKILL.md");

const SKILL_FRONTMATTER: &str = r#"---
name: munin
description: |
  Guide AI agents and LLMs on controlling and integrating with Munin: PREFER JSON-RPC 2.0 API mode for interactive browser control, macro execution, DOM perception, and semantic assertions. Fallback to CLI commands and YAML scenarios.
metadata:
  version: "0.1.0"
---

"#;

#[derive(Args)]
struct InstallArgs {
    /// Skip interactive prompts and accept all defaults
    #[arg(short, long)]
    yes: bool,
    /// Destination path for configuration file (default: ~/.munin/munin.toml)
    #[arg(long)]
    config: Option<String>,
    /// Destination directory for AI Agent skill (default: ~/.agents/skills/munin)
    #[arg(long)]
    skill_dir: Option<String>,
}

fn prompt_line(prompt: &str, default: &str) -> String {
    use std::io::{stdout, Write};
    print!("{} [{}]: ", prompt, default);
    let _ = stdout().flush();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_ok() {
        let trimmed = input.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    default.to_string()
}

async fn cmd_install(args: InstallArgs) -> Result<()> {
    use std::path::PathBuf;

    println!("================================================================");
    println!("  🦅 Munin (奥丁灵鸦) —— 安装与环境初始化向导");
    println!("================================================================\n");

    let home = env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    let target_config_path = args.config.map(PathBuf::from).unwrap_or_else(|| {
        home.join(".munin").join("munin.toml")
    });
    let target_skill_dir = args.skill_dir.map(PathBuf::from).unwrap_or_else(|| {
        home.join(".agents").join("skills").join("munin")
    });

    let mut config = MuninConfig::default();

    if args.yes {
        println!("⚡ 已指定 --yes，使用标准默认配置自动生成...\n");
    } else {
        println!("📋 [1/2] 交互式生成运行时配置文件 (直接回车保留默认值)");
        println!("----------------------------------------------------------------");

        let fast_endpoint = prompt_line("• 快引擎 (Laya) 服务端点", &config.fast_engine.endpoint);
        config.fast_engine.endpoint = fast_endpoint;

        let fast_model = prompt_line("• 快引擎推理模型", &config.fast_engine.model);
        config.fast_engine.model = fast_model;

        let threshold_str = prompt_line(
            "• 置信度门控仲裁阈值 (0.0 ~ 1.0)",
            &format!("{:.2}", config.fast_engine.confidence_threshold),
        );
        if let Ok(t) = threshold_str.parse::<f32>() {
            config.fast_engine.confidence_threshold = t;
        }

        let slow_provider = prompt_line("• 慢引擎提供者 (rpc / openai)", &config.slow_engine.provider);
        config.slow_engine.provider = slow_provider.clone();

        if slow_provider == "openai" {
            let api_key = prompt_line("• OpenAI API Key", config.slow_engine.api_key.as_deref().unwrap_or(""));
            if !api_key.is_empty() {
                config.slow_engine.api_key = Some(api_key);
            }
            let base_url = prompt_line(
                "• OpenAI Base URL",
                config.slow_engine.base_url.as_deref().unwrap_or("https://api.openai.com/v1"),
            );
            config.slow_engine.base_url = Some(base_url);
            let model = prompt_line(
                "• OpenAI 模型名称",
                config.slow_engine.model.as_deref().unwrap_or("gpt-4o"),
            );
            config.slow_engine.model = Some(model);
        } else {
            let slow_endpoint = prompt_line("• 慢引擎 RPC 服务端点", &config.slow_engine.endpoint);
            config.slow_engine.endpoint = slow_endpoint;
        }

        let rpc_listen = prompt_line("• 本地 JSON-RPC 2.0 监听地址", &config.rpc_server.listen);
        config.rpc_server.listen = rpc_listen;

        let headless_str = prompt_line(
            "• 浏览器默认后台无头模式 (true / false)",
            if config.browser.headless { "true" } else { "false" },
        );
        config.browser.headless = headless_str.trim().eq_ignore_ascii_case("true");
        println!();
    }

    // 1. 写入配置文件
    if let Some(parent) = target_config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let toml_content = config.to_toml_string().map_err(|e| anyhow!("序列化配置失败: {e}"))?;
    std::fs::write(&target_config_path, toml_content)?;
    println!("✔ [配置就绪] 默认全局配置文件写入: {}", target_config_path.display());

    // 2. 写入 AI Agent 统一规范 SKILL 文件 (~/.agents/skills/munin/SKILL.md)
    println!("\n📋 [2/2] 安装 AI Agent Skill 标准指令");
    println!("----------------------------------------------------------------");
    std::fs::create_dir_all(&target_skill_dir)?;
    let skill_path = target_skill_dir.join("SKILL.md");
    let full_skill_content = format!("{}{}", SKILL_FRONTMATTER, SKILL_MD_RAW);
    std::fs::write(&skill_path, &full_skill_content)?;
    println!("✔ [Skill 就绪] 业界统一规范已安装: {}", skill_path.display());

    // 3. 若存在 ~/.omp/agent/skills，同时兼容写入
    let omp_skills_dir = home.join(".omp").join("agent").join("skills").join("munin");
    if home.join(".omp").exists() && std::fs::create_dir_all(&omp_skills_dir).is_ok() {
        let _ = std::fs::write(omp_skills_dir.join("SKILL.md"), &full_skill_content);
        println!("✔ [兼容就绪] 已同步安装至 Oh-My-Pi: {}", omp_skills_dir.join("SKILL.md").display());
    }

    println!("\n================================================================");
    println!("🎉 Munin 全局环境初始化完成！");
    println!("================================================================");
    println!("• 任意终端启动服务:     munin serve");
    println!("• 任意终端运行测试:     munin test <scenario.yaml> --headed");
    println!("• AI Agent 技能库已同步加载至 ~/.agents/skills/munin/");
    println!("================================================================\n");

    Ok(())
}
