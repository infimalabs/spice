//! The tape owns authored text; journal admissions only authorize its interpretation.

use std::path::Path;

use listen::{Decoded, Dialog, Event, Prose, ProseRole};
use wire::outbox::Accepted;

/// The one URL whose written body carries its own packets.
pub const OUTBOX: &str = "spice://outbox";

/// The task and optional recipient an invitation URL spells, unparsed:
/// `spice://task/TASK/invite[/ADDRESS]` with no query.
#[must_use]
pub fn invitation(url: &str) -> Option<(&str, Option<&str>)> {
    let mut segments = url.strip_prefix("spice://task/")?.split('/');
    let task = segments.next().filter(|task| !task.is_empty())?;
    if segments.next()? != "invite" {
        return None;
    }
    let recipient = segments.next();
    (segments.next().is_none()
        && recipient.is_none_or(|address| !address.is_empty())
        && !url.contains(['?', '#']))
    .then_some((task, recipient))
}

/// Whether one host URI operation writes authored text: the outbox, or an
/// invitation whose body is its message.
#[must_use]
pub fn authored_write(operation: &str, url: &str) -> bool {
    operation == "write" && (url == OUTBOX || invitation(url).is_some())
}

/// URL and exact body of one narrowly admitted host URI write.
///
/// Ordinary tool calls, arbitrary arguments, results and quoted examples never
/// acquire authored-message meaning through this function.
#[must_use]
pub fn captured(decoded: &Decoded) -> Option<(&str, &str)> {
    match decoded.context()?.dialog.as_ref()? {
        Dialog::HostUri {
            operation,
            url,
            content: Some(text),
            ..
        } if authored_write(operation, url) => Some((url, text)),
        _ => None,
    }
}

/// Exact body of one narrowly admitted host URI write.
#[must_use]
pub fn captured_text(decoded: &Decoded) -> Option<&str> {
    captured(decoded).map(|(_, text)| text)
}

/// Interpret an admitted captured write as one canonical assistant-prose event.
///
/// The caller supplies the journal admission, not a model-authored declaration.
/// This does not run packets, mutate authority or change the underlying tape.
pub fn materialize(decoded: Decoded, accepted: &Accepted) -> Result<Decoded, &'static str> {
    let (url, text) = captured(&decoded).ok_or("admitted source is not an authored host write")?;
    if accepted.hunk.locus.event != 0
        || accepted.hunk.text.start != 0
        || accepted.hunk.text.end != text.len() as u64
        || accepted.digest != wire::QuoteTarget::text_digest(text)
        || accepted.invitation.is_some() == (url == OUTBOX)
    {
        return Err("admitted authored source no longer agrees with its exact body");
    }
    let Decoded::Events {
        mut context,
        events,
    } = decoded
    else {
        return Err("admitted outbox source is not a decoded host record");
    };
    if !events.is_empty() {
        return Err("an outbox host record already contains unrelated events");
    }
    let Some(Dialog::HostUri {
        content: Some(text),
        ..
    }) = context.dialog.take()
    else {
        return Err("admitted outbox source lost its host operation");
    };
    let locus = accepted.hunk.locus;
    Ok(Decoded::Events {
        context,
        events: vec![Event::Prose(Prose {
            role: ProseRole::Assistant,
            text,
            phase: None,
            media: Vec::new(),
            external_media: Vec::new(),
            locus: listen::Locus::new(locus.member, locus.offset, locus.length),
        })],
    })
}

/// Reconstruct an admitted body's event from its exact durable tape generation.
pub fn read(tape: &Path, accepted: &Accepted) -> Result<Decoded, String> {
    let head = &accepted.hunk.head;
    let dialect = crate::extension::VENDORS
        .iter()
        .find(|entry| entry.vendor.name() == head.vendor.as_str())
        .map(|entry| entry.vendor.dialect())
        .ok_or_else(|| "admitted outbox vendor is unavailable".to_owned())?;
    let root = crate::repository::lane_tape(tape, head.lane).join(head.vendor.as_str());
    let decoded = crate::quote::decode_record(&root, head.source, accepted.hunk.locus, dialect)
        .map_err(|error| format!("admitted outbox tape is unavailable: {error:?}"))?;
    materialize(decoded, accepted).map_err(str::to_owned)
}
