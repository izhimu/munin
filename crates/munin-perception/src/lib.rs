pub mod form;
pub mod overlay;
pub mod probe;
pub mod pruner;

pub use form::FormMapper;
pub use overlay::OverlayBuster;
pub use probe::StateProbe;
pub use pruner::CandidatePruner;

#[cfg(test)]
mod tests {
    use super::*;
    use munin_driver::mock::MockDriver;
    use munin_driver::BrowserDriver;
    use munin_engine::fast::mock::MockFastEngine;
    use munin_types::{Action, DOMElementNode};

    #[tokio::test]
    async fn test_overlay_buster() -> anyhow::Result<()> {
        let buster = OverlayBuster::default();

        let mut driver = MockDriver::new();
        driver.launch(true).await?;
        driver.add_element(DOMElementNode::new("btn-close-modal", "button", "关闭弹窗"));
        driver.add_element(DOMElementNode::new("btn-submit", "button", "提交"));

        let fast = MockFastEngine::new().with_choice("btn-close-modal", 0.95);

        let dismissed = buster.bust_overlays(&driver, &fast).await?;
        assert_eq!(dismissed, Some("btn-close-modal".to_string()));

        let actions = driver.recorded_actions();
        assert_eq!(
            actions,
            vec![Action::Click {
                target_id: "btn-close-modal".to_string()
            }]
        );

        Ok(())
    }

    #[test]
    fn test_candidate_pruner() {
        let pruner = CandidatePruner::new(2);

        let elements = vec![
            DOMElementNode::new("btn-irrelevant", "button", "Irrelevant Action"),
            DOMElementNode::new("input-search", "input", "Search Query")
                .with_attribute("placeholder", "Search articles"),
            DOMElementNode::new("btn-search", "button", "Search"),
            DOMElementNode::new("div-footer", "div", "Copyright 2026"),
        ];

        let pruned = pruner.prune(&elements, "Search articles now");
        assert_eq!(pruned.len(), 2);
        // The search elements should score higher than footer or irrelevant
        let ids: Vec<&str> = pruned.iter().map(|e| e.node_id.as_str()).collect();
        assert!(ids.contains(&"input-search") || ids.contains(&"btn-search"));
    }

    #[tokio::test]
    async fn test_state_probe() -> anyhow::Result<()> {
        let probe = StateProbe::new();
        let mut driver = MockDriver::new();
        driver.launch(true).await?;
        driver.set_js_result("document.body", serde_json::json!("Order #12345 has been created successfully!"));

        let fast = MockFastEngine::new().with_probe(true, 0.92);

        let (achieved, conf) = probe
            .probe_outcome(&driver, &fast, "Order created successfully")
            .await?;

        assert!(achieved);
        assert!(conf >= 0.85);

        Ok(())
    }

    #[test]
    fn test_form_mapper_heuristic() {
        let mapper = FormMapper::new();
        let elements = vec![
            DOMElementNode::new("inp-1", "input", "")
                .with_attribute("name", "username")
                .with_attribute("placeholder", "Enter username"),
            DOMElementNode::new("inp-2", "input", "")
                .with_attribute("name", "password"),
        ];

        let matched = mapper.match_field_heuristically(&elements, "username");
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().node_id, "inp-1");
    }
}
