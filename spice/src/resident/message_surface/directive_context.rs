use super::*;

pub(in crate::resident) fn steering_ledger(
    reader: &bus::BusReader,
    decoded: &listen::Decoded,
) -> CommandResult<Option<PresentationDirectives>> {
    let authored = || {
        decoded
            .events()
            .iter()
            .filter_map(listen::Event::assistant_text)
    };
    let acknowledges = authored().any(steer::has_acknowledgement);
    let (packets, addresses) = packet_destinations(authored());
    let mut keys = direction::delivered(decoded.events());
    if !acknowledges && !packets && keys.is_empty() {
        return Ok(None);
    }
    if acknowledges {
        keys.extend(steering_keys(authored()));
    }
    let mut ledger = if keys.is_empty() {
        PresentationDirectives::default()
    } else {
        let presentations = load_steering_presentations(reader, &keys)?;
        PresentationDirectives::from_presentations(
            reader,
            &presentations,
            load_principal_roles(reader)?,
        )
        .map_err(|error| store_fault("serve::message-steering-invalid", &error))?
    };
    ledger.prove_recipients(reader, addresses)?;
    Ok(Some(ledger))
}

pub(super) fn present_steering_acknowledgements(
    text: &str,
    steering: Option<&PresentationDirectives>,
    event: PresentationEvent,
) -> CommandResult<serve::stream::MessagePresentation> {
    let Some(steering) = steering else {
        return Ok(serve::stream::MessagePresentation::plain(text.to_owned()));
    };
    let presented = present_steering_acknowledgements_with_directives(text, steering, event)
        .map_err(|detail| store_fault("serve::message-steering-presentation-invalid", &detail))?;
    let directive_contexts = hydrate_directive_contexts(&presented, steering)
        .map_err(|detail| store_fault("serve::message-steering-presentation-invalid", &detail))?;
    let sends = hydrate_sends(&presented, event)
        .map_err(|detail| store_fault("serve::message-steering-presentation-invalid", &detail))?;
    Ok(serve::stream::MessagePresentation {
        text: presented.text,
        acknowledgement: presented.acknowledgement,
        directive_contexts,
        sends,
    })
}

/// One admitted invitation message as its author's card presents it: one
/// outgoing quote of its body to the lane and room its invitation reached.
pub(super) fn present_invitation_message(
    text: &str,
    invitation: &wire::outbox::Invitation,
    event: PresentationEvent,
) -> CommandResult<serve::stream::MessagePresentation> {
    let presented = present_invitation(text, invitation);
    let sends = hydrate_sends(&presented, event)
        .map_err(|detail| store_fault("serve::message-steering-presentation-invalid", &detail))?;
    Ok(serve::stream::MessagePresentation {
        text: presented.text,
        acknowledgement: None,
        directive_contexts: Vec::new(),
        sends,
    })
}

impl PresentationDirective {
    /// Badge of the resident-generated directive kind, from typed provenance.
    pub(super) fn badge(&self) -> Option<wire::DirectiveBadge> {
        if self.review.is_some() {
            Some(wire::DirectiveBadge::Review)
        } else if self.taste {
            Some(wire::DirectiveBadge::Taste)
        } else {
            None
        }
    }
}

impl PresentationDirectives {
    pub(super) fn new(directives: BTreeMap<steer::Key, PresentationDirective>) -> Self {
        Self {
            directives,
            recipients: BTreeMap::new(),
            roles: wire::principal::PrincipalRoles::default(),
            lines: BTreeMap::new(),
        }
    }

    /// Whether `directive` was sent by an actor whose principal currently
    /// holds the operator role.
    pub(super) fn operator_sent(&self, directive: &PresentationDirective) -> bool {
        self.roles
            .holds(&directive.actor, wire::principal::PrincipalRole::Operator)
    }

    pub(super) fn from_presentations(
        reader: &bus::BusReader,
        rows: &[bus::SteeringPresentation],
        roles: wire::principal::PrincipalRoles,
    ) -> Result<Self, String> {
        let needs_review = rows.iter().any(|row| {
            row.head()
                .body()
                .control
                .as_deref()
                .and_then(wire::parse_review_feedback_control)
                .is_some()
        });
        let snapshot = needs_review
            .then(|| reader.task_snapshot().map_err(|error| error.to_string()))
            .transpose()?;
        let mut directives = BTreeMap::new();
        for row in rows {
            let head = row.head().body();
            let Some(lane) = head.recipient.lane() else {
                continue;
            };
            let key = head
                .key
                .parse::<steer::Key>()
                .map_err(|error| error.to_string())?;
            let retirement = row
                .current()
                .fact
                .read::<wire::body::SteerAcked>()
                .and_then(|body| body.evidence.transcript().map(|(_, locus)| locus))
                .or_else(|| {
                    row.current()
                        .fact
                        .read::<wire::body::SteerDeclined>()
                        .and_then(|body| body.evidence.transcript().map(|(_, locus)| locus))
                });
            let taste = row.created().fact.kind() == wire::FactKind::TasteReminded
                || row
                    .created()
                    .fact
                    .read::<wire::body::TasteJudged>()
                    .is_some_and(|body| {
                        matches!(body.decision, wire::body::TasteDecision::Confirmed { .. })
                    });
            let quote_target = wire::QuoteTarget::Steering {
                key: key.to_string(),
                digest: wire::QuoteTarget::text_digest(&head.text),
            };
            let media_quotes = spice::quote::media_quotes(&quote_target, &head.attachments)
                .map_err(|error| format!("{error:?}"))?;
            let spice::quote::SteeringSource { attribution, body } =
                spice::quote::steering_source(row);
            let text = body.as_deref().unwrap_or(&head.text);
            directives.insert(
                key,
                PresentationDirective {
                    lane,
                    text: wire::media::with_media_payloads(text, &head.attachments)
                        .map_err(|error| error.to_string())?
                        .into_owned(),
                    authored: true,
                    taste,
                    review: review_presentation(snapshot.as_ref(), head.control.as_deref())?,
                    disposition: head.disposition,
                    retirement,
                    quote_target,
                    media_quotes,
                    actor: row.created().actor.clone(),
                    attribution,
                    published: row.created().at,
                    fanout: head.fanout,
                },
            );
        }
        let inputs = rows
            .iter()
            .map(|row| row.head().body())
            .filter(|head| {
                head.control
                    .as_deref()
                    .and_then(wire::parse_peer_line_control)
                    .is_some()
            })
            .cloned()
            .collect::<Vec<_>>();
        let lines = if inputs.is_empty() {
            BTreeMap::new()
        } else {
            reader
                .read(move |read| {
                    let mut lines = BTreeMap::new();
                    for head in &inputs {
                        if let (Ok(key), Some(line)) = (
                            head.key.parse::<steer::Key>(),
                            crate::peer_line::standing(read, head)?,
                        ) {
                            lines.insert(key, line.identity());
                        }
                    }
                    Ok(lines)
                })
                .map_err(|error| error.to_string())?
        };
        Ok(Self {
            roles,
            lines,
            ..Self::new(directives)
        })
    }

    pub(super) fn extend_presentations(
        &mut self,
        reader: &bus::BusReader,
        rows: &[bus::SteeringPresentation],
        roles: wire::principal::PrincipalRoles,
    ) -> Result<(), String> {
        let loaded = Self::from_presentations(reader, rows, roles)?;
        self.directives.extend(loaded.directives);
        self.lines.extend(loaded.lines);
        self.roles = loaded.roles;
        Ok(())
    }

    /// Prove the occupant birth each not-yet-proved address names. An address
    /// no birth proves leaves its packet presented as written.
    pub(super) fn prove_recipients(
        &mut self,
        reader: &bus::BusReader,
        addresses: BTreeSet<wire::OccupantAddress>,
    ) -> CommandResult<()> {
        for address in addresses {
            if self.recipients.contains_key(&address) {
                continue;
            }
            let proof = reader.occupant_birth(address).map_err(|error| {
                store_fault("serve::message-recipient-unreadable", &error.to_string())
            })?;
            if let Ok(birth) = proof {
                self.recipients.insert(
                    address,
                    wire::MessageRecipient {
                        address,
                        lane: birth.lane,
                        occupant: birth.occupant,
                    },
                );
            }
        }
        Ok(())
    }
}

struct AnySteeringKey;

impl steer::Standing for AnySteeringKey {
    fn disposition(&self, _key: &steer::Key) -> Option<wire::Disposition> {
        Some(wire::Disposition::Live)
    }
}

pub(super) fn steering_keys<'a>(texts: impl IntoIterator<Item = &'a str>) -> BTreeSet<steer::Key> {
    texts
        .into_iter()
        .flat_map(|text| steer::scan(text, &AnySteeringKey))
        .filter_map(|recognition| match recognition.outcome {
            steer::Outcome::Retired { key, .. } => Some(key),
            steer::Outcome::Missed { .. } => None,
        })
        .collect()
}

pub(in crate::resident) fn review_presentation(
    snapshot: Option<&bus::TaskSnapshot>,
    control: Option<&str>,
) -> Result<Option<wire::MessageReview>, String> {
    let Some((task, reviewed_at)) = control.and_then(wire::parse_review_feedback_control) else {
        return Ok(None);
    };
    let snapshot = snapshot.ok_or_else(|| "review feedback has no task snapshot".to_owned())?;
    let reviewed = snapshot
        .facts()
        .at(reviewed_at)
        .and_then(|envelope| envelope.fact.read::<wire::body::TaskReviewed>())
        .ok_or_else(|| format!("review feedback names missing review fact {reviewed_at}"))?;
    if reviewed.task != task {
        return Err(format!(
            "review feedback task {task} disagrees with review fact task {}",
            reviewed.task
        ));
    }
    let finding = reviewed
        .finding
        .filter(|finding| !finding.trim().is_empty())
        .ok_or_else(|| format!("review feedback for {task} has no finding"))?;
    let heads = snapshot
        .tasks()
        .binary_search_by(|heads| heads.statement().body().born.cmp(&task))
        .ok()
        .map(|index| &snapshot.tasks()[index])
        .ok_or_else(|| format!("review feedback task {task} has no current statement"))?;
    Ok(Some(wire::MessageReview {
        task: wire::TaskHandle::for_topic(&heads.statement().body().topic.path, &task),
        finding,
        guidance: wire::REVIEW_FEEDBACK_GUIDANCE.to_owned(),
    }))
}

/// Each context carries its directive's own attribution, exactly as a quote
/// of that directive names it; the card presenting it adds nothing.
pub(super) fn hydrate_directive_contexts(
    presented: &Presentation,
    directives: &PresentationDirectives,
) -> Result<Vec<wire::MessageDirective>, String> {
    presented
        .contexts
        .iter()
        .map(|context| {
            let directive = directives.directives.get(&context.key).ok_or_else(|| {
                format!(
                    "canonical acknowledgment presentation lost directive {}",
                    context.key
                )
            })?;
            Ok(wire::MessageDirective {
                start: u32::try_from(context.range.start)
                    .map_err(|_| "directive context start exceeds the wire range".to_owned())?,
                end: u32::try_from(context.range.end)
                    .map_err(|_| "directive context end exceeds the wire range".to_owned())?,
                quote_target: directive.quote_target.clone(),
                media_quotes: directive.media_quotes.clone(),
                attribution: directive.attribution.clone(),
                authored: directive.published,
                badge: directive.badge(),
                review: directive.review.clone(),
                delivery: None,
                line: directives.lines.get(&context.key).cloned(),
            })
        })
        .collect()
}

fn hydrate_sends(
    presented: &Presentation,
    event: PresentationEvent,
) -> Result<Vec<wire::MessageSend>, String> {
    if presented.sends.is_empty() {
        return Ok(Vec::new());
    }
    let sender = event
        .author
        .ok_or_else(|| "presented packet names no authoring occupant".to_owned())?;
    presented
        .sends
        .iter()
        .map(|send| {
            Ok(wire::MessageSend {
                start: u32::try_from(send.range.start)
                    .map_err(|_| "packet quote start exceeds the wire range".to_owned())?,
                end: u32::try_from(send.range.end)
                    .map_err(|_| "packet quote end exceeds the wire range".to_owned())?,
                from: event.lane,
                sender,
                recipients: send.recipients.clone(),
                lanes: send.lanes.clone(),
                operator: send.operator,
                contexts: send.contexts.clone(),
                source: wire::rooms::TextRange {
                    start: u64::try_from(send.source.start)
                        .map_err(|_| "packet source start exceeds the wire range".to_owned())?,
                    end: u64::try_from(send.source.end)
                        .map_err(|_| "packet source end exceeds the wire range".to_owned())?,
                },
                body: send.body.clone(),
            })
        })
        .collect()
}

/// Recognized answers from one presentation scan. Callers select dispositions
/// before collecting distinct keys, so pending cards need no refusal set.
pub(super) fn recognized_answers<'a>(
    text: &str,
    standing: &'a EventDirectives<'_>,
) -> impl Iterator<Item = (steer::Key, wire::Disposition)> + 'a {
    scan_presentations(text, standing)
        .into_iter()
        .flat_map(|packet| packet.recognitions)
        .filter_map(move |recognition| {
            let steer::ack::scan::Outcome::Retired {
                key, disposition, ..
            } = recognition.outcome
            else {
                return None;
            };
            standing
                .presents(&key, disposition, recognition.locus)
                .then_some((key, disposition))
        })
}

impl EventDirectives<'_> {
    pub(super) fn recognizes(
        &self,
        key: &steer::Key,
        disposition: wire::Disposition,
        locus: steer::ack::scan::Locus,
    ) -> bool {
        if self.event.superseded_answers.contains(key) {
            return false;
        }
        let Some(directive) = self.directives.directives.get(key) else {
            return false;
        };
        directive.lane == self.event.lane
            && (directive.disposition == wire::Disposition::Live
                || (directive.disposition == disposition
                    && directive.retirement.is_some_and(|retirement| {
                        retirement_matches(retirement, self.event, locus)
                    })))
    }

    /// The answer the journal retired `key` with for this lane, which display
    /// may repeat without repeating retirement.
    ///
    /// The lane owns that conversation continuity across transcript replacement
    /// and across the records of one message: a key retires at the streamed
    /// text while the card projects from the settled message. The original
    /// retirement locus remains the authority for retirement and automatic
    /// speech; later records only reuse its already-authenticated context for
    /// presentation.
    pub(super) fn answered_for_lane(&self, key: &steer::Key) -> Option<wire::Disposition> {
        if self.event.superseded_answers.contains(key) {
            return None;
        }
        self.directives
            .directives
            .get(key)
            .filter(|directive| {
                directive.lane == self.event.lane
                    && matches!(
                        directive.disposition,
                        wire::Disposition::Acked | wire::Disposition::Declined
                    )
                    && directive.review.is_none()
                    && directive.retirement.is_some()
            })
            .map(|directive| directive.disposition)
    }

    /// The stable seal authenticating steering delivery to this lane.
    ///
    /// It is a presentation target rather than a directive: acknowledging it
    /// never mutates steering standing or gains automatic-speech authority.
    pub(super) fn is_lane_seal(&self, key: &steer::Key) -> bool {
        key == &steer::Key::for_identity(self.event.lane)
    }

    pub(super) fn presents(
        &self,
        key: &steer::Key,
        disposition: wire::Disposition,
        locus: steer::ack::scan::Locus,
    ) -> bool {
        self.recognizes(key, disposition, locus)
            || self.answered_for_lane(key) == Some(disposition)
            || (disposition == wire::Disposition::Acked && self.is_lane_seal(key))
    }

    pub(super) fn recognizes_authored(
        &self,
        key: &steer::Key,
        disposition: wire::Disposition,
        locus: steer::ack::scan::Locus,
    ) -> bool {
        self.recognizes(key, disposition, locus) && self.authored(key)
    }

    pub(super) fn authored(&self, key: &steer::Key) -> bool {
        self.directives
            .directives
            .get(key)
            .is_some_and(|directive| directive.authored)
    }

    /// Whether the journal still holds `key` pending, so an acknowledgement
    /// spoken here awaits the steer::acked fact before it reads as credit.
    pub(super) fn pending(&self, key: &steer::Key) -> bool {
        self.directives
            .directives
            .get(key)
            .is_some_and(|directive| directive.disposition == wire::Disposition::Live)
    }

    pub(super) fn context_state(
        &self,
        key: &steer::Key,
        disposition: wire::Disposition,
    ) -> ContextState {
        match disposition {
            wire::Disposition::Acked if self.pending(key) => ContextState::Pending,
            wire::Disposition::Acked => ContextState::Acknowledged,
            wire::Disposition::Declined => ContextState::Declined,
            _ => ContextState::Unmarked,
        }
    }
}

impl steer::Standing for EventDirectives<'_> {
    fn disposition(&self, key: &steer::Key) -> Option<wire::Disposition> {
        if self.event.superseded_answers.contains(key) {
            return None;
        }
        let directive = self.directives.directives.get(key)?;
        if directive.disposition == wire::Disposition::Live
            || (directive.lane == self.event.lane
                && directive
                    .retirement
                    .is_some_and(|retirement| retirement_record_matches(retirement, self.event)))
        {
            Some(wire::Disposition::Live)
        } else {
            Some(directive.disposition)
        }
    }
}

fn retirement_record_matches(
    retirement: wire::body::TranscriptLocus,
    event: PresentationEvent<'_>,
) -> bool {
    retirement
        .source
        .is_none_or(|expected| event.source == Some(expected))
        && retirement.member == event.locus.member
        && retirement.offset == event.locus.offset
        && retirement.length == event.locus.length
        && retirement.event == event.ordinal
}

fn retirement_matches(
    retirement: wire::body::TranscriptLocus,
    event: PresentationEvent<'_>,
    locus: steer::ack::scan::Locus,
) -> bool {
    retirement_record_matches(retirement, event)
        && retirement.line as usize == locus.line
        && retirement.column as usize == locus.column
}
