use rusqlite::OptionalExtension as _;

use super::*;

/// Result of looking up a client command identity in durable authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandReplay {
    /// The identity names this exact admitted request and its committed answer.
    Exact(wire::Outcome<wire::intent::CommandAnswer>),
    /// The identity is durable, but it was bound to another request or principal.
    Conflict,
}

impl JournalRead<'_> {
    /// Recover the durable answer for an exact retry of one admitted command.
    pub fn command_replay(
        &self,
        command: &wire::AdmittedCommand,
    ) -> Result<Option<CommandReplay>, GuardError> {
        let row = self
            .connection
            .query_row(
                "SELECT actor, command_kind, request, response, epoch \
                 FROM command_receipts WHERE command_id = ?1",
                [command.id().to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(GuardError::Store)?;
        let Some((actor, kind, request, response, epoch)) = row else {
            return Ok(None);
        };
        if actor != command.actor().to_string() || kind != command.kind().as_str() {
            return Ok(Some(CommandReplay::Conflict));
        }
        let stored: wire::Command<serde_json::Value> = wire::decode(&request).map_err(|error| {
            GuardError::Invalid(format!("command receipt request is invalid: {error}"))
        })?;
        let current: wire::Command<serde_json::Value> = wire::decode(command.canonical_request())
            .map_err(|error| {
            GuardError::Invalid(format!("admitted command request is invalid: {error}"))
        })?;
        let headers_match = stored.v == current.v
            && stored.id == current.id
            && stored.kind == current.kind
            && stored.expected == current.expected;
        let stored_epoch = stored.epoch;
        let body_matches = request_bodies_match(stored.kind, stored.body, current.body)?;
        if epoch != stored_epoch.to_string() || !headers_match || !body_matches {
            return Ok(Some(CommandReplay::Conflict));
        }
        let response: wire::Response<serde_json::Value> = wire::decode(&response)
            .map_err(|error| GuardError::Invalid(format!("command receipt is invalid: {error}")))?;
        if !response.v.is_current() || response.id != command.id() || response.epoch != stored_epoch
        {
            return Err(GuardError::Invalid(
                "command receipt response identity disagrees with its request".to_owned(),
            ));
        }
        let wire::Outcome::Reply(reply) = response.outcome else {
            return Err(GuardError::Invalid(
                "durable command receipt does not contain a successful reply".to_owned(),
            ));
        };
        if !matches!(reply.position, wire::ReplyPosition::Committed(_)) {
            return Err(GuardError::Invalid(
                "durable command receipt does not name a committed position".to_owned(),
            ));
        }
        let body = wire::encode(&reply.body)
            .and_then(|body| command.kind().binding().answer.decode(&body))
            .map_err(|error| {
                GuardError::Invalid(format!("command receipt answer is invalid: {error}"))
            })?;
        Ok(Some(CommandReplay::Exact(wire::Outcome::Reply(
            wire::Reply {
                position: reply.position,
                body,
            },
        ))))
    }

    /// Recover the committed reply whose transaction contains an admitted
    /// operation fact. The caller must authenticate that fact's operation and
    /// actor before using this retained response.
    pub fn command_reply_containing(
        &self,
        position: wire::Seq,
        kind: wire::CommandKind,
        actor: &wire::Actor,
    ) -> Result<Option<wire::Reply<wire::intent::CommandAnswer>>, GuardError> {
        let indexed = i64::try_from(position.get())
            .map_err(|_| GuardError::Invalid("command reply position exceeds SQLite".to_owned()))?;
        let row = self
            .connection
            .query_row(
                "SELECT command_id, epoch, response FROM command_receipts \
             WHERE first_seq <= ?1 AND last_seq >= ?1 AND command_kind = ?2 AND actor = ?3 \
             ORDER BY first_seq DESC, command_id LIMIT 1",
                rusqlite::params![indexed, kind.as_str(), actor.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(GuardError::Store)?;
        let Some((id, epoch, bytes)) = row else {
            return Ok(None);
        };
        let response: wire::Response<serde_json::Value> = wire::decode(&bytes)
            .map_err(|error| GuardError::Invalid(format!("command reply is invalid: {error}")))?;
        if !response.v.is_current()
            || response.id.to_string() != id
            || response.epoch.to_string() != epoch
        {
            return Err(GuardError::Invalid(
                "command reply identity disagrees with its record".to_owned(),
            ));
        }
        let wire::Outcome::Reply(reply) = response.outcome else {
            return Err(GuardError::Invalid(
                "committed command record has no successful reply".to_owned(),
            ));
        };
        let wire::ReplyPosition::Committed(range) = reply.position else {
            return Err(GuardError::Invalid(
                "command reply has no committed range".to_owned(),
            ));
        };
        if position < range.first || position > range.last {
            return Err(GuardError::Invalid(
                "command reply does not contain its indexed operation".to_owned(),
            ));
        }
        let body = wire::encode(&reply.body)
            .and_then(|body| kind.binding().answer.decode(&body))
            .map_err(|error| {
                GuardError::Invalid(format!("command reply body is invalid: {error}"))
            })?;
        Ok(Some(wire::Reply {
            position: wire::ReplyPosition::Committed(range),
            body,
        }))
    }
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentDigest {
    digest: wire::Digest,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MediaReceipt {
    name: String,
    content_type: String,
    data_url: ContentDigest,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SteerSendReceipt {
    fanout: wire::CanonicalUuid,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    lane: Option<wire::CanonicalUuid>,
    message: ContentDigest,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<MediaReceipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    recipients: Vec<wire::body::SteeringRecipient>,
}

fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

impl SteerSendReceipt {
    fn from_send(send: wire::intent::SteerSend) -> Result<Self, wire::MediaError> {
        let message = wire::QuoteTarget::text_digest(&send.message);
        Ok(Self {
            fanout: send.fanout,
            lane: send.lane,
            message: ContentDigest { digest: message },
            attachments: send
                .attachments
                .into_iter()
                .map(|media| {
                    let digest = media.identity()?;
                    Ok(MediaReceipt {
                        name: media.name,
                        content_type: media.content_type,
                        data_url: ContentDigest { digest },
                    })
                })
                .collect::<Result<_, wire::MediaError>>()?,
            recipients: send.recipients,
        })
    }

    pub(crate) fn matches_sent(&self, sent: &wire::body::SteerSent) -> bool {
        sent.fanout == Some(self.fanout)
            && self
                .lane
                .is_none_or(|lane| sent.recipient == wire::Actor::Lane(lane))
            && self.message.digest == wire::QuoteTarget::text_digest(&sent.text)
            && self.recipients == sent.recipients
            && self.attachments.len() == sent.attachments.len()
            && self
                .attachments
                .iter()
                .zip(&sent.attachments)
                .all(|(stored, fact)| {
                    stored.name == fact.name
                        && stored.content_type == fact.content_type
                        && fact
                            .identity()
                            .is_ok_and(|digest| digest == stored.data_url.digest)
                })
    }
}

pub(crate) fn decode_steer_send_receipt(
    body: serde_json::Value,
) -> Result<SteerSendReceipt, String> {
    serde_json::from_value(body)
        .map_err(|error| format!("steer::send receipt body does not decode: {error}"))
}

pub(crate) fn compact_receipt_request(
    mut command: wire::Command<serde_json::Value>,
) -> Result<(wire::Command<serde_json::Value>, Vec<u8>), String> {
    if command.kind == wire::CommandKind::SteerSend {
        let send = serde_json::from_value(command.body)
            .map_err(|error| format!("steer::send receipt body does not decode: {error}"))?;
        let receipt = SteerSendReceipt::from_send(send)
            .map_err(|error| format!("steer::send receipt media is invalid: {error}"))?;
        command.body = serde_json::to_value(receipt)
            .map_err(|error| format!("steer::send receipt body does not encode: {error}"))?;
    }
    let request = command
        .canonical_json()
        .map_err(|error| format!("command receipt request does not encode: {error}"))?
        .into_bytes();
    Ok((command, request))
}

fn request_bodies_match(
    kind: wire::CommandKind,
    stored: serde_json::Value,
    current: serde_json::Value,
) -> Result<bool, GuardError> {
    if kind != wire::CommandKind::SteerSend {
        return Ok(stored == current);
    }
    let request = serde_json::from_value::<wire::intent::SteerSend>(current).map_err(|error| {
        GuardError::Invalid(format!("admitted steer::send body is invalid: {error}"))
    })?;
    let stored = decode_steer_send_receipt(stored).map_err(GuardError::Invalid)?;
    Ok(SteerSendReceipt::from_send(request).is_ok_and(|current| current == stored))
}
