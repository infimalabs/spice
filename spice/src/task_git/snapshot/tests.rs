use super::Controls;

#[test]
fn cleanup_can_resume_after_removing_either_owned_control() {
    let captured = Controls {
        message: Some("owned snapshot message".into()),
        auto_merge: Some("owned merge tree".into()),
    };
    for current in [
        captured.clone(),
        Controls {
            message: captured.message.clone(),
            auto_merge: None,
        },
        Controls {
            message: None,
            auto_merge: captured.auto_merge.clone(),
        },
        Controls::default(),
    ] {
        assert!(captured.admits_cleanup(&current));
    }
}

#[test]
fn cleanup_never_adopts_introduced_or_replaced_controls() {
    let captured = Controls {
        message: Some("owned snapshot message".into()),
        auto_merge: Some("owned merge tree".into()),
    };
    for current in [
        Controls {
            message: Some("another operation".into()),
            auto_merge: captured.auto_merge.clone(),
        },
        Controls {
            message: captured.message.clone(),
            auto_merge: Some("another merge tree".into()),
        },
    ] {
        assert!(!captured.admits_cleanup(&current));
    }
    let message_only = Controls {
        message: captured.message.clone(),
        auto_merge: None,
    };
    let tree_only = Controls {
        message: None,
        auto_merge: captured.auto_merge.clone(),
    };
    assert!(!message_only.admits_cleanup(&captured));
    assert!(!tree_only.admits_cleanup(&captured));
}
