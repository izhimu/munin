use anyhow::{anyhow, Context, Result};
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

const HELP: &str = r#"Munin CLI - Reflex-Driven Browser Automation Framework

USAGE:
  munin open <url> [options]
  munin run <goal> --url <url> [options]
  munin test <scenario.yaml> [options]
  munin serve [options]
  munin demo
  munin help

COMMANDS:
  open <url>                 Open browser and navigate to URL
  run <goal> --url <url>     Execute natural language goal with Dual-Engine
  test <scenario.yaml>       Execute declarative test scenario internally (0 LLM overhead)
  serve                      Start JSON-RPC 2.0 service for external slow models/agents
  demo                       Run standalone reflex demonstration
  help                       Show this help message

OPTIONS:
  --config <path>            Path to munin.toml configuration file (default: munin.toml)
  --url <url>                Target webpage URL (required for 'run')
  --listen <addr>            Listening host and port for RPC server (default: 127.0.0.1:9090)
  --mock                     Use mock engines (zero external API keys or services required)
  --headed                   Show browser window while running
  --headless                 Run browser in background without window
  --cdp <url>                Connect to existing CDP endpoint (e.g. http://127.0.0.1:9222)
  --threshold <float>        Confidence threshold (default: 0.85 or from config)

CONFIGURATION FILE:
  munin.toml                 Configures fast model (Laya), slow model RPC/OpenAI, and server

ENVIRONMENT VARIABLES:
  OPENAI_API_KEY             API key for System 2 slow macro planner (if using OpenAI)
  OPENAI_BASE_URL            Base URL for OpenAI-compatible endpoint
  OPENAI_MODEL               Model name for slow planner (default: gpt-4o)
  LAYA_ENDPOINT              Endpoint for System 1 fast reflexes (default: http://127.0.0.1:8000)
  RUST_LOG                   Log level filter (e.g. info, debug)
"#;
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        println!("{}", HELP);
        return Ok(());
    }

    let command = args[1].as_str();
    let res = tokio::select! {
        res = async {
            match command {
                "help" | "-h" | "--help" => {
                    println!("{}", HELP);
                    Ok(())
                }
                "open" => cmd_open(&args[2..]).await,
                "run" => cmd_run(&args[2..]).await,
                "serve" => cmd_serve(&args[2..]).await,
                "test" => cmd_test(&args[2..]).await,
                "demo" => cmd_demo().await,
                unknown => {
                    eprintln!("Unknown command: '{}'\n", unknown);
                    println!("{}", HELP);
                    std::process::exit(1);
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
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init();
}

async fn cmd_open(args: &[String]) -> Result<()> {
    init_logger().await;

    let mut url = None;
    let mut headless = false; // default headed for `open`
    let mut cdp_endpoint = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--headless" => headless = true,
            "--headed" => headless = false,
            "--cdp" => {
                i += 1;
                if i < args.len() {
                    cdp_endpoint = Some(args[i].clone());
                } else {
                    return Err(anyhow!("Missing value for --cdp"));
                }
            }
            val if !val.starts_with('-') && url.is_none() => {
                url = Some(val.to_string());
            }
            other => {
                return Err(anyhow!("Unexpected argument: {other}"));
            }
        }
        i += 1;
    }

    let target_url = url.ok_or_else(|| anyhow!("URL is required. Usage: munin open <url>"))?;
    let normalized_url = if !target_url.starts_with("http://") && !target_url.starts_with("https://") {
        format!("https://{}", target_url)
    } else {
        target_url
    };

    println!("🚀 Launching browser...");
    let mut driver = match cdp_endpoint {
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

async fn cmd_run(args: &[String]) -> Result<()> {
    init_logger().await;

    let mut goal = None;
    let mut url = None;
    let mut headless = None;
    let mut mock = false;
    let mut cdp_endpoint = None;
    let mut threshold_override = None;
    let mut config_path = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--headless" => headless = Some(true),
            "--headed" => headless = Some(false),
            "--mock" => mock = true,
            "--config" => {
                i += 1;
                if i < args.len() {
                    config_path = Some(args[i].clone());
                } else {
                    return Err(anyhow!("Missing value for --config"));
                }
            }
            "--url" => {
                i += 1;
                if i < args.len() {
                    url = Some(args[i].clone());
                } else {
                    return Err(anyhow!("Missing value for --url"));
                }
            }
            "--cdp" => {
                i += 1;
                if i < args.len() {
                    cdp_endpoint = Some(args[i].clone());
                } else {
                    return Err(anyhow!("Missing value for --cdp"));
                }
            }
            "--threshold" => {
                i += 1;
                if i < args.len() {
                    threshold_override = Some(
                        args[i]
                            .parse()
                            .context("Failed to parse --threshold as float")?,
                    );
                } else {
                    return Err(anyhow!("Missing value for --threshold"));
                }
            }
            val if !val.starts_with('-') && goal.is_none() => {
                goal = Some(val.to_string());
            }
            other => {
                return Err(anyhow!("Unexpected argument: {other}"));
            }
        }
        i += 1;
    }

    let config = MuninConfig::load_or_default(config_path.as_deref());
    let user_goal = goal.ok_or_else(|| anyhow!("Goal description is required. Usage: munin run <goal> --url <url>"))?;
    let target_url = url.ok_or_else(|| anyhow!("--url is required. Usage: munin run <goal> --url <url>"))?;
    let normalized_url = if !target_url.starts_with("http://") && !target_url.starts_with("https://") {
        format!("https://{}", target_url)
    } else {
        target_url
    };

    let threshold = threshold_override.unwrap_or(config.fast_engine.confidence_threshold);
    let is_headless = headless.unwrap_or(config.browser.headless);
    let cdp = cdp_endpoint.or(config.browser.cdp_endpoint);

    println!("================================================================");
    println!("  🦅 Munin Dual-Engine Execution Pipeline");
    println!("================================================================");
    println!("• Goal:      {}", user_goal);
    println!("• Target:    {}", normalized_url);
    println!("• Threshold: {:.2}", threshold);
    println!("• Fast Eng:  {} ({})", config.fast_engine.provider, config.fast_engine.endpoint);
    println!("• Slow Eng:  {} ({})", config.slow_engine.provider, config.slow_engine.endpoint);
    println!("• Engine:    {}", if mock { "Mock Engines" } else { "Active Engines" });
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

    if mock {
        let fast_engine = MockFastEngine::new().with_probe_sequence(vec![
            (false, 0.20),
            (true, 0.95),
        ]);
        let slow_engine = MockSlowEngine::new().with_steps(vec![
            MacroStep::new(1, &user_goal, "任务完成"),
        ]);

        let mut supervisor = BiSystemSupervisor::new(driver, fast_engine, slow_engine, threshold);
        supervisor.execute_goal(&user_goal).await?;
    } else {
        let fast_endpoint = env::var("LAYA_ENDPOINT").unwrap_or_else(|_| config.fast_engine.endpoint.clone());
        let fast_engine = LayaFastEngine::with_model(fast_endpoint, &config.fast_engine.model);

        if config.slow_engine.provider == "rpc" {
            let slow_engine = RpcSlowEngine::with_timeout(
                &config.slow_engine.endpoint,
                Duration::from_millis(config.slow_engine.timeout_ms),
            );
            let mut supervisor = BiSystemSupervisor::new(driver, fast_engine, slow_engine, threshold);
            supervisor.execute_goal(&user_goal).await?;
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

            let mut supervisor = BiSystemSupervisor::new(driver, fast_engine, slow_engine, threshold);
            supervisor.execute_goal(&user_goal).await?;
        }
    }

    println!("\n✔ Task execution complete.");
    Ok(())
}

async fn cmd_serve(args: &[String]) -> Result<()> {
    init_logger().await;

    let mut config_path = None;
    let mut listen_override = None;
    let mut mock = false;
    let mut headless = None;
    let mut cdp_endpoint = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--mock" => mock = true,
            "--headless" => headless = Some(true),
            "--headed" => headless = Some(false),
            "--config" => {
                i += 1;
                if i < args.len() {
                    config_path = Some(args[i].clone());
                } else {
                    return Err(anyhow!("Missing value for --config"));
                }
            }
            "--listen" => {
                i += 1;
                if i < args.len() {
                    listen_override = Some(args[i].clone());
                } else {
                    return Err(anyhow!("Missing value for --listen"));
                }
            }
            "--cdp" => {
                i += 1;
                if i < args.len() {
                    cdp_endpoint = Some(args[i].clone());
                } else {
                    return Err(anyhow!("Missing value for --cdp"));
                }
            }
            other => {
                return Err(anyhow!("Unexpected argument: {other}"));
            }
        }
        i += 1;
    }

    let config = MuninConfig::load_or_default(config_path.as_deref());
    let listen_addr = listen_override.unwrap_or(config.rpc_server.listen);
    let is_headless = headless.unwrap_or(config.browser.headless);
    let cdp = cdp_endpoint.or(config.browser.cdp_endpoint);

    println!("================================================================");
    println!("  🦅 Munin JSON-RPC 2.0 Dual-Engine Server");
    println!("================================================================");
    println!("• RPC Listen: http://{}", listen_addr);
    println!("• Fast Eng:   {} ({})", config.fast_engine.provider, config.fast_engine.endpoint);
    println!("• Slow Eng:   {} ({})", config.slow_engine.provider, config.slow_engine.endpoint);
    println!("• Engine:     {}", if mock { "Mock Engines" } else { "Laya + Browser" });
    println!("----------------------------------------------------------------\n");
    println!("💡 Available JSON-RPC 2.0 Methods:");
    println!("  - execute_goal(goal, [url])");
    println!("  - navigate(url)");
    println!("  - get_interactive_elements([prune], [goal])");
    println!("  - click(node_id)");
    println!("  - fill(node_id, text)");
    println!("  - screenshot()");
    println!("  - bust_overlays()");
    println!("  - probe(assertion, [state])");
    println!("  - evaluate_js(script)");
    println!("  - ping()\n");

    if mock {
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
            let supervisor = BiSystemSupervisor::new(
                driver,
                fast_engine,
                slow_engine,
                config.fast_engine.confidence_threshold,
            );
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
            );
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
async fn cmd_test(args: &[String]) -> Result<()> {
    init_logger().await;
    let mut scenario_path = None;
    let mut config_path = None;
    let mut mock = false;
    let mut headless = None;
    let mut cdp_endpoint = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--mock" => mock = true,
            "--headless" => headless = Some(true),
            "--headed" => headless = Some(false),
            "--config" => {
                i += 1;
                if i < args.len() {
                    config_path = Some(args[i].clone());
                }
            }
            "--cdp" => {
                i += 1;
                if i < args.len() {
                    cdp_endpoint = Some(args[i].clone());
                }
            }
            val if !val.starts_with('-') && scenario_path.is_none() => {
                scenario_path = Some(val.to_string());
            }
            _ => {}
        }
        i += 1;
    }

    let target_file = scenario_path.ok_or_else(|| anyhow!("Scenario file path is required. Usage: munin test <scenario.yaml>"))?;
    let scenario = Scenario::from_file(&target_file)?;
    let config = MuninConfig::load_or_default(config_path.as_deref());
    let is_headless = headless.unwrap_or(config.browser.headless);
    let cdp = cdp_endpoint.or(config.browser.cdp_endpoint);

    println!("================================================================");
    println!("  🦅 Munin Internal Scenario Test Runner");
    println!("================================================================");
    println!("• Scenario:   {}", scenario.name);
    println!("• File:       {}", target_file);
    println!("• Base URL:   {}", scenario.base_url.as_deref().unwrap_or("Defined in steps"));
    println!("• Steps:      {} step(s)", scenario.steps.len());
    println!("• Engine:     {}", if mock { "Mock Driver" } else { "CDP Browser + Laya" });
    println!("----------------------------------------------------------------\n");

    let report = if mock {
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
