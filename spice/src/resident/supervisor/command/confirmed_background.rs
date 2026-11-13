//! Confirmed delivery of one private background notice to an exact occupant.

use std::io;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{Kick, call, reply_name, send_to};
use crate::resident::design_input::BackgroundNotice;
use crate::resident::new_uuid;

const REPLY_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug, Deserialize, Serialize)]
struct Reply {
    request: String,
    accepted: bool,
}

/// Send `notice` through the kick door at `door`, retained for `owner` when
/// one is named, awaiting the answer at a reply door beside it.
pub(super) fn send(
    door: &Path,
    identity_source: &Path,
    notice: BackgroundNotice,
    owner: Option<super::ServiceClaim>,
) -> io::Result<bool> {
    let request = new_uuid().to_string();
    let reply = door.with_file_name(reply_name(identity_source, 'b', &request));
    let answer: Reply = call(door, reply, REPLY_TIMEOUT, |reply| {
        Kick::BackgroundConfirmed {
            notice,
            owner,
            request: request.clone(),
            reply,
        }
    })?;
    if answer.request != request {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "background callback named another request",
        ));
    }
    Ok(answer.accepted)
}

pub(super) fn answer(reply: &Path, request: String, accepted: bool) {
    let _sent = send_to(reply, &Reply { request, accepted });
}
