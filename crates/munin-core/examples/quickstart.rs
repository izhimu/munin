//! Munin 极速上手示例：双引擎浏览器自动化工作流演示
//! 运行命令：cargo run -p munin-core --example quickstart

use anyhow::Result;
use munin_core::BiSystemSupervisor;
use munin_driver::mock::MockDriver;
use munin_driver::BrowserDriver;
use munin_engine::fast::mock::MockFastEngine;
use munin_engine::slow::mock::MockSlowEngine;
use munin_types::{DOMElementNode, MacroStep};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // 1. 初始化控制台日志
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    println!("=================================================================");
    println!("  🦅 Munin (奥丁灵鸦) —— Rust 快慢双引擎浏览器自动化运行演示");
    println!("=================================================================\n");

    // 2. 初始化驱动并注入一个真实的业务场景
    let mut driver = MockDriver::new();
    driver.launch(true).await?;
    driver.goto("http://192.168.2.109:8088/patrol/dispatch").await?;

    // 模拟页面元素：一个干扰弹窗 + 两个业务选项
    driver.add_element(
        DOMElementNode::new("btn-ad-close", "button", "✖ 残忍拒绝并关闭窗口")
            .with_attribute("class", "modal-close-btn"),
    );
    driver.add_element(
        DOMElementNode::new("btn-clean-team", "button", "🌱 绿化环卫保洁班组")
            .with_attribute("role", "button"),
    );
    driver.add_element(
        DOMElementNode::new(
            "btn-emergency-team",
            "button",
            "🚒 高速特种应急抢修与排险大队",
        )
        .with_attribute("role", "button"),
    );

    // 模拟页面在操作后的返回状态文本
    driver.set_default_js_result(serde_json::json!(
        "公路智慧调度中心：现场巡检K45险情上报成功，特种应急抢修大队已接单调度，正在封道排险！"
    ));

    println!("[1/3] 模拟页面已就绪：包含 1 个突发干扰弹窗与 2 个派工候选选项");

    // 3. 快慢双引擎组装
    // 配置模拟快引擎响应（如果直连 Laya 服务也是同理）：
    let fast_engine = MockFastEngine::new().with_probe_sequence(vec![
        (false, 0.10), // Step 1 执行前：未完成
        (true, 0.96),  // Step 1 执行后：探测到目标已完成 (置信度 96%)
    ]);
    fast_engine.map_choice_by_instruction("closes or dismisses", "btn-ad-close", 0.98);
    fast_engine.map_choice_by_instruction("最匹配的交互项", "btn-emergency-team", 0.94);

    // 配置慢引擎宏观规划步骤：
    let slow_engine = MockSlowEngine::new().with_steps(vec![MacroStep::new(
        1,
        "研判现场重大险情并指派特种应急大队",
        "特种应急抢修大队已接单调度",
    )]);

    println!("[2/3] ⚡ 快慢双引擎装配完毕 (置信度门控阈值: 0.85)...");
    let mut supervisor = BiSystemSupervisor::new(driver, fast_engine, slow_engine, 0.85)
        .with_step_delay_ms(50);

    println!("[3/3] 🚀 双系统协同执行高阶目标...");
    supervisor
        .execute_goal("处理现场巡检上报的严重护栏撞毁事故，并指派给特种应急大队")
        .await?;

    println!("\n✔ 执行完成！驱动记录的实际交互操作流水：");
    for (i, act) in supervisor.driver.recorded_actions().iter().enumerate() {
        println!("  [{}] {:?}", i + 1, act);
    }

    println!("\n=================================================================");
    println!("  🎉 演示成功！Munin 完美闭环：弹窗 10ms 消除 ➜ 目标高置信命中 ➜ 探针达成断言！");
    println!("=================================================================");

    Ok(())
}
