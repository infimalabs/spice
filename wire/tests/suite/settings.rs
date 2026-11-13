//! Global settings carry fast mode only; principal profiles originate from Git.
use wire::body;

#[test]
fn a_settings_change_records_fast_mode_only() {
    assert_eq!(
        serde_json::to_value(body::GlobalSettingsChanged::new(true)).expect("body encodes"),
        serde_json::json!({ "fast_mode": true })
    );
}
