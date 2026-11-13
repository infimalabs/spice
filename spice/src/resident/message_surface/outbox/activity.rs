//! Outbox transport is status prose only when native admission proves its body.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::super::*;

#[derive(Debug, Default)]
pub(in crate::resident::message_surface) struct Activity {
    authority: Option<(bus::BusReader, PathBuf)>,
    calls: BTreeMap<(CanonicalUuid, String), Call>,
}

#[derive(Debug)]
struct Call {
    author: CanonicalUuid,
    source: Option<CanonicalUuid>,
    text: String,
}

pub(in crate::resident::message_surface) struct Authored {
    pub text: String,
    pub quote: wire::QuoteTarget,
}

impl Activity {
    pub(in crate::resident::message_surface) fn bind(&mut self, bus: &bus::BusReader, tape: &Path) {
        if self.authority.is_none() {
            self.authority = Some((bus.clone(), tape.to_owned()));
        }
    }

    pub(in crate::resident::message_surface) fn clear_lane(&mut self, lane: CanonicalUuid) {
        self.calls.retain(|(owner, _), _| *owner != lane);
    }

    pub(in crate::resident::message_surface) fn authored(
        &mut self,
        event: &AgentEvent<'_>,
    ) -> CommandResult<Option<Authored>> {
        let listen::Event::Tool(tool) = event.decoded else {
            return Ok(None);
        };
        if self.authority.is_none() {
            return Ok(None);
        }
        let key = (event.lane, tool.call_id.clone());
        match &tool.moment {
            listen::ToolMoment::Call {
                name, arguments, ..
            } => {
                let Some(text) = write_text(name, arguments) else {
                    self.calls.remove(&key);
                    return Ok(None);
                };
                let authored = self.admitted(event.lane, event.author, &text)?;
                self.calls.insert(
                    key,
                    Call {
                        author: event.author,
                        source: event.source,
                        text,
                    },
                );
                Ok(authored)
            }
            listen::ToolMoment::Result(_) => {
                let Some(call) = self.calls.remove(&key) else {
                    return Ok(None);
                };
                if call.author != event.author || call.source != event.source {
                    return Ok(None);
                }
                self.admitted(event.lane, event.author, &call.text)
            }
            listen::ToolMoment::Update { .. } => {
                let Some(call) = self.calls.get(&key) else {
                    return Ok(None);
                };
                if call.author != event.author || call.source != event.source {
                    return Ok(None);
                }
                self.admitted(event.lane, event.author, &call.text)
            }
        }
    }

    fn admitted(
        &self,
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        text: &str,
    ) -> CommandResult<Option<Authored>> {
        let Some((bus, tape)) = &self.authority else {
            return Ok(None);
        };
        let Some(submission) = bus
            .outbox(bus::OutboxQuery::Body {
                lane,
                occupant,
                digest: wire::QuoteTarget::text_digest(text),
            })
            .map_err(|error| store_fault("outbox::index-unreadable", &format!("{error:?}")))?
            .pop()
        else {
            return Ok(None);
        };
        let accepted = &submission.body;
        let decoded = spice::outbox_source::read(tape, accepted)
            .map_err(|error| store_fault("outbox::source-unreadable", &error))?;
        let listen::Decoded::Events { mut events, .. } = decoded else {
            unreachable!("outbox materialization returns authored events");
        };
        let Some(listen::Event::Prose(prose)) = events.pop() else {
            unreachable!("outbox materialization returns authored prose");
        };
        if prose.text != text {
            return Ok(None);
        }
        Ok(Some(Authored {
            text: prose.text,
            quote: wire::QuoteTarget::Tape {
                lane: accepted.lane,
                source: accepted.hunk.head.source,
                locus: accepted.hunk.locus,
                digest: accepted.digest.clone(),
            },
        }))
    }
}

fn write_text(name: &str, arguments: &str) -> Option<String> {
    if !matches!(name, "write" | "functions.write") {
        return None;
    }
    let serde_json::Value::Object(mut fields) = serde_json::from_str(arguments).ok()? else {
        return None;
    };
    if fields.get("path")?.as_str()? != "spice://outbox" {
        return None;
    }
    let serde_json::Value::String(text) = fields.remove("content")? else {
        return None;
    };
    (text.len() <= wire::outbox::BODY_BYTES_MAX).then_some(text)
}
