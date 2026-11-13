use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

#[test]
fn absent_tracked_policy_is_exactly_the_packaged_document() {
    let root = std::env::temp_dir().join(format!(
        "spice-gate-packaged-policy-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("create isolated generic repository");

    let generic = Contract::load_generic(&root).expect("generic package policy loads");
    let packaged = parse_tracked_policy(PACKAGED_POLICY_SOURCE, packaged_policy_text())
        .expect("the rack document compiles through the tracked parser");
    assert_eq!(generic.rule_policy().document(), packaged.document());

    let declared: toml::Value =
        toml::from_str(packaged_policy_text()).expect("packaged policy remains TOML");
    let expected_skip = declared["gate"]["walk"]["skip"]
        .as_array()
        .expect("packaged walk skip is a list")
        .iter()
        .map(|value| value.as_str().expect("packaged skip is text"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert_eq!(generic.policy().ignore().render(), expected_skip);
    assert_eq!(
        generic.shape().name_cluster_threshold(),
        usize::try_from(
            declared["policy"]["name_cluster_threshold"]
                .as_integer()
                .expect("packaged threshold is an integer")
        )
        .expect("packaged threshold fits usize")
    );

    fs::remove_dir_all(root).expect("remove isolated generic repository");
}
