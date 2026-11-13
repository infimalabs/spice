use super::*;
use std::future;

#[tokio::test(flavor = "current_thread")]
async fn continuously_ready_sources_share_every_three_turns() {
    let mut lead = Lead::Supervisor;

    for _ in 0..8 {
        let first = next(
            &mut lead,
            future::ready(()),
            future::ready(()),
            future::ready(()),
        )
        .await;
        let second = next(
            &mut lead,
            future::ready(()),
            future::ready(()),
            future::ready(()),
        )
        .await;
        let third = next(
            &mut lead,
            future::ready(()),
            future::ready(()),
            future::ready(()),
        )
        .await;

        assert!(matches!(
            (first, second, third),
            (Input::Supervisor(()), Input::Control(()), Input::Record(()))
        ));
    }
}
