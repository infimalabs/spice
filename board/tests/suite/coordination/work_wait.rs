use super::*;

#[test]
fn a_lapsed_claim_is_transparent_to_dependency_wait_classification() {
    let stale = admitted(
        statement("WAIT-stale"),
        workflow("todo"),
        Some(claim(ClaimDisposition::Lapsed)),
        Some(lease()),
    )
    .expect("stale prerequisite");
    let mut behind_stale = statement("WAIT-behind-stale");
    behind_stale.dependencies.push(stamp("WAIT-stale"));
    let behind_stale =
        admitted(behind_stale, workflow("todo"), None, None).expect("wait behind stale work");

    let mut deferred_workflow = workflow("todo");
    deferred_workflow.deferral = Some(instant("2026-08-01T07:00:00.000Z"));
    let stale_deferred = admitted(
        statement("WAIT-stale-deferred"),
        deferred_workflow,
        Some(claim(ClaimDisposition::Lapsed)),
        Some(lease()),
    )
    .expect("stale deferred prerequisite");
    let mut behind_stale_deferred = statement("WAIT-behind-stale-deferred");
    behind_stale_deferred
        .dependencies
        .push(stamp("WAIT-stale-deferred"));
    let behind_stale_deferred = admitted(behind_stale_deferred, workflow("todo"), None, None)
        .expect("blocked behind stale deferred work");

    let entries = [stale, behind_stale, stale_deferred, behind_stale_deferred];
    let waits = board::work_waits(&entries, &environment());

    assert_eq!(
        waits.get(&stamp("WAIT-stale")),
        None,
        "a stale task keeps its distinct standing"
    );
    assert_eq!(
        waits.get(&stamp("WAIT-behind-stale")),
        Some(&board::WorkWait::Workflow),
        "a dependency on claimable stale work is waiting its turn"
    );
    assert_eq!(
        waits.get(&stamp("WAIT-stale-deferred")),
        None,
        "a stale task does not borrow its underlying gate's standing"
    );
    assert_eq!(
        waits.get(&stamp("WAIT-behind-stale-deferred")),
        Some(&board::WorkWait::Blocked),
        "a human-actionable wall behind stale work still blocks its dependent"
    );
}
