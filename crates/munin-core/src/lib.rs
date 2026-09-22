pub mod supervisor;

pub use supervisor::{BiSystemSupervisor, Supervisor};

#[cfg(test)]
mod tests {
    use super::*;
    use munin_driver::mock::MockDriver;
    use munin_driver::BrowserDriver;
    use munin_engine::fast::mock::MockFastEngine;
    use munin_engine::slow::mock::MockSlowEngine;
    use munin_types::{Action, DOMElementNode, MacroStep};

    #[tokio::test]
    async fn test_supervisor_fast_path_flow() -> anyhow::Result<()> {
        let mut driver = MockDriver::new();
        driver.launch(true).await?;
        driver.goto("https://test.example.com").await?;

        driver.add_element(DOMElementNode::new("btn-submit", "button", "Submit"));

        let fast = MockFastEngine::new()
            .with_choice("btn-submit", 0.95)
            .with_probe_sequence(vec![(false, 0.10), (true, 0.96)]);
        let slow = MockSlowEngine::new().with_steps(vec![MacroStep::new(
            1,
            "Click submit button",
            "页面显示操作成功",
        )]);

        let mut supervisor = Supervisor::new(driver, fast, slow, 0.85).with_step_delay_ms(0);

        supervisor.execute_goal("Submit the form successfully").await?;

        let actions = supervisor.driver.recorded_actions();
        assert!(actions.contains(&Action::Click {
            target_id: "btn-submit".to_string()
        }));

        Ok(())
    }

    #[tokio::test]
    async fn test_supervisor_confidence_gating_and_escalation() -> anyhow::Result<()> {
        let mut driver = MockDriver::new();
        driver.launch(true).await?;
        driver.add_element(DOMElementNode::new("btn-ambiguous", "button", "Option A"));
        driver.add_element(DOMElementNode::new("btn-target", "button", "Option B"));

        // Fast engine has low confidence (0.60 < 0.85)
        let fast = MockFastEngine::new()
            .with_choice("btn-ambiguous", 0.60)
            .with_probe_sequence(vec![(false, 0.10), (true, 0.92)]);
        // Slow engine intervenes with arbitration to choose btn-target
        let slow = MockSlowEngine::new();
        slow.set_default_steps(vec![MacroStep::new(1, "Pick correct option", "已确认选择")]);
        slow.set_default_arbitration("btn-target");

        let mut supervisor = Supervisor::new(driver, fast, slow, 0.85).with_step_delay_ms(0);

        supervisor.execute_goal("Pick target option").await?;

        let actions = supervisor.driver.recorded_actions();
        // btn-target was clicked via slow engine arbitration
        assert!(actions.contains(&Action::Click {
            target_id: "btn-target".to_string()
        }));

        Ok(())
    }

    #[tokio::test]
    async fn test_supervisor_overlay_healing_in_flight() -> anyhow::Result<()> {
        let mut driver = MockDriver::new();
        driver.launch(true).await?;
        // Popup overlay present
        driver.add_element(DOMElementNode::new("btn-dismiss-ad", "button", "跳过广告"));
        driver.add_element(DOMElementNode::new("btn-next", "button", "下一页"));

        let fast = MockFastEngine::new()
            .with_probe_sequence(vec![(false, 0.10), (true, 0.91)]);
        fast.map_choice_by_instruction("closes or dismisses", "btn-dismiss-ad", 0.95);
        fast.map_choice_by_instruction("推进当前任务目标", "btn-next", 0.90);
        let slow = MockSlowEngine::new().with_steps(vec![MacroStep::new(
            1,
            "Navigate to next page",
            "已进入下一页",
        )]);

        let mut supervisor = Supervisor::new(driver, fast, slow, 0.85).with_step_delay_ms(0);

        supervisor.execute_goal("Go to next page").await?;

        let actions = supervisor.driver.recorded_actions();
        // Overlay dismiss button should have been clicked first!
        assert_eq!(
            actions[0],
            Action::Click {
                target_id: "btn-dismiss-ad".to_string()
            }
        );

        Ok(())
    }
}
