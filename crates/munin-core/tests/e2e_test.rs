use anyhow::Result;
use munin_core::BiSystemSupervisor;
use munin_driver::mock::MockDriver;
use munin_driver::BrowserDriver;
use munin_engine::fast::mock::MockFastEngine;
use munin_engine::slow::mock::MockSlowEngine;
use munin_types::{Action, DOMElementNode, MacroStep};

#[tokio::test]
async fn test_patrol_dispatch_flow_e2e_mock() -> Result<()> {
    // 1. 装配驱动与双引擎
    let mut driver = MockDriver::new();
    driver.launch(true).await?;

    // 初始页面元素：包含一个干扰弹窗和业务按钮
    driver.add_element(
        DOMElementNode::new("btn-dismiss-notice", "button", "我知道了")
            .with_attribute("class", "modal-close-btn"),
    );
    driver.add_element(
        DOMElementNode::new("btn-dispatch-team", "button", "派发特种抢修队")
            .with_attribute("role", "button"),
    );
    driver.add_element(
        DOMElementNode::new("btn-confirm-order", "button", "确认提交工单")
            .with_attribute("type", "submit"),
    );

    // 配置快引擎响应：
    // - 弹窗自愈选择: btn-dismiss-notice
    // - 业务选择: btn-dispatch-team (置信度 0.95)
    // - 探针序列: 初始未完成 -> 完成
    let fast_engine = MockFastEngine::new().with_probe_sequence(vec![
        (false, 0.10), // 轮 1：弹窗阻塞中，未达成
        (false, 0.20), // 轮 2：弹窗已消解，未达成 → 点击业务按钮
        (true, 0.95),  // 轮 3：点击后达成
    ]);
    // 第一轮：存在弹窗阻塞 → 消解；消解后页面通畅
    fast_engine.set_blocked_sequence(vec![(true, 0.95), (false, 0.95)]);
    fast_engine.set_recovery(munin_types::RecoveryKind::Dismiss, 0.95);
    fast_engine.map_choice_by_instruction("closes or dismisses", "btn-dismiss-notice", 0.95);
    fast_engine.map_choice_by_instruction("恢复动作", "btn-dismiss-notice", 0.95);
    fast_engine.map_choice_by_instruction("推进当前任务目标", "btn-dispatch-team", 0.95);

    // 配置慢引擎规划：
    let slow_engine = MockSlowEngine::new().with_steps(vec![MacroStep::new(
        1,
        "提交护栏损毁报告并指派特种抢修队",
        "工单已成功派发并进入流转状态",
    )]);

    let mut supervisor = BiSystemSupervisor::new(driver, fast_engine, slow_engine, 0.85).with_in_memory_recovery()
        .with_step_delay_ms(0);

    // 2. 访问智慧养护工单系统
    supervisor
        .driver
        .goto("http://192.168.2.109:8088/dispatch")
        .await?;

    // 3. 执行自然语言业务流（自动消解弹窗、完成目标交互）
    supervisor
        .execute_goal("以巡检员身份登录，提交一起K12处护栏损毁报告并指派给特种抢修队")
        .await?;

    // 4. 快引擎原生语义断言（探针已在执行中验证）
    let actions = supervisor.driver.recorded_actions();

    // 验证弹窗自愈发生：跳过了广告/通知
    assert_eq!(
        actions[1],
        Action::Click {
            target_id: "btn-dismiss-notice".to_string()
        }
    );

    // 验证核心业务点击执行
    assert!(actions.contains(&Action::Click {
        target_id: "btn-dispatch-team".to_string()
    }));

    Ok(())
}

#[tokio::test]
async fn test_cdp_driver_type_export() -> Result<()> {
    // 验证在 feature = "cdp" 下，CdpDriver 类型与 API 结构可正常引用
    #[cfg(feature = "cdp")]
    {
        use munin_driver::cdp::CdpDriver;
        use munin_engine::fast::laya::LayaFastEngine;
        use munin_engine::slow::openai::OpenAISlowEngine;

        let _fast = LayaFastEngine::new("http://127.0.0.1:8000");
        let _slow = OpenAISlowEngine::new("test-api-key");
        assert_eq!(_fast.endpoint(), "http://127.0.0.1:8000");
        assert_eq!(_slow.base_url(), "https://api.openai.com/v1");
        // CdpDriver type is usable
        let _ = std::mem::size_of::<CdpDriver>();
    }
    Ok(())
}
