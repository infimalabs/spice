//! A packet that sends earns its author one private receipt, so a packet the
//! author never sees receipted did not send.

use std::time::Duration;

use super::*;
use crate::resident::supervisor::command::{KICK_BYTES, Kick, kick_path};

/// Every kick the door holds now.
fn kicks(door: &door::Door) -> Vec<Kick> {
    std::iter::from_fn(|| door.recv(Some(Duration::ZERO)).ok())
        .map(|bytes| serde_json::from_slice(&bytes).expect("a kick decodes"))
        .collect()
}

#[test]
fn a_packet_reaching_only_peers_receipts_its_author_once() {
    let lanes = Lanes::new("syn-receipt");
    let (first, second) = (lanes.address(FIRST), lanes.address(SECOND));
    let door = door::Door::bind(&kick_path(&lanes.paths, test_uuid(SENDER)), KICK_BYTES)
        .expect("the author's kick door binds");
    lanes.author(SENDER, &format!("SYN {first} {second}: rebase onto main"));
    let author = test_uuid(SENDER + 1);

    let (outcomes, diagnostics) = lanes.hear_with(SENDER, author, steering::Refusals::Notify);
    assert_eq!(outcomes, [Ok(sent(&[FIRST, SECOND], &[]))]);
    assert!(
        diagnostics.is_empty(),
        "plain delivery adds no resident diagnostic: {diagnostics}"
    );
    let [Kick::MessageRecorded(receipt)] =
        <[Kick; 1]>::try_from(kicks(&door)).expect("plain delivery earns exactly one receipt")
    else {
        panic!("the receipt is a recorded-message notice");
    };
    assert_eq!(receipt.occupant, author);
    assert_eq!(receipt.header, format!("SYN {first} {second}:"));
    assert_eq!(receipt.reached, 2);
    assert!(receipt.rooms.is_empty() && receipt.queued.is_empty());
    assert!(receipt.redirected.is_empty());

    let (reread, _) = lanes.hear_with(SENDER, author, steering::Refusals::Notify);
    assert_eq!(reread, [Ok(steering::Published::Already)]);
    assert_eq!(
        kicks(&door),
        [],
        "a reread sends nothing and earns no receipt"
    );
    drop(door);
    lanes.finish();
}
