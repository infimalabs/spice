use super::*;

impl Published {
    /// The packet judgment of a newly staged publication whose admitted
    /// routes became `branches`.
    pub(super) fn outcome(
        &self,
        branches: Vec<wire::outbox::Branch>,
    ) -> wire::outbox::PacketOutcome {
        match self {
            Self::Sent {
                lanes,
                queued,
                resuming,
                redirected,
                unreached,
                spent,
                rooms,
                principals: _,
                lines,
                ..
            } => wire::outbox::PacketOutcome::Messaged {
                lanes: lanes.clone(),
                queued: queued.clone(),
                resuming: resuming.clone(),
                redirected: redirected.clone(),
                unreached: unreached.clone(),
                spent: spent.clone(),
                rooms: rooms.clone(),
                lines: Some(*lines),
                branches,
            },
            Self::Already => unreachable!("only a newly staged publication has a new judgment"),
        }
    }
}

pub(super) fn publish_packet_judged(
    bus: &Bus,
    epoch: CanonicalUuid,
    config: &steer::Config,
    packet: &Packet<'_>,
    view: &bus::TaskSnapshot,
    judgment: Option<outbox::Judgment>,
) -> Result<Published, Unpublished> {
    refuse_key_recipients(bus, packet)?;
    let resolved_packet;
    let routes = if packet
        .routes
        .iter()
        .any(|route| route.audience == Some(Audience::Input))
    {
        let ResolvedInputRoutes {
            packet,
            parents,
            refused,
        } = resolve_input_routes(bus, packet, view)?;
        resolved_packet = packet;
        PublicationRoutes {
            packet: &resolved_packet,
            parents,
            refused,
        }
    } else {
        PublicationRoutes {
            packet,
            parents: vec![None; packet.routes.len()],
            refused: Vec::new(),
        }
    };
    let conversation = routes
        .packet
        .routes
        .iter()
        .any(|route| route.audience == Some(Audience::Input));
    if conversation {
        publish_conversation(bus, epoch, config, view, judgment, routes)
    } else {
        publish_message(bus, epoch, config, view, judgment, routes)
    }
}

/// Refuse a packet that writes an input key where an occupant belongs.
///
/// `SYN dYNb4g1kj: BODY` parses its selector as an occupant address, yet a
/// key-shaped spelling no occupant was ever born at is an input key meant
/// for `#`. Delivering the rest of the packet would guess at the author's
/// intent, so the whole packet refuses with the spelling that reaches that
/// input's sender. An address some occupant's birth records routes as written,
/// whatever its shape.
fn refuse_key_recipients(bus: &Bus, packet: &Packet<'_>) -> Result<(), Unpublished> {
    for route in &packet.routes {
        let Some(Audience::Occupant(address)) = route.audience else {
            continue;
        };
        let written = address.to_string();
        if !steer::Key::shaped(&written) {
            continue;
        }
        if let Err(bus::AddressRefusal::Unrecorded { .. } | bus::AddressRefusal::NotBirth { .. }) =
            occupant_birth(bus, address)?
        {
            return Err(Unpublished::Refused {
                reason: steer::ack::scan::message::KEY_RECIPIENT.to_owned(),
                detail: notice::key_recipient(&written),
            });
        }
    }
    Ok(())
}

/// Resolve every `#INPUT` to its current authorized counterpart.
///
/// Any input this lane received links, whatever its disposition: a live,
/// seen, answered, declined, or closed-line input each continues through a
/// fresh linked line to the peer lane or principal that sent it.
fn resolve_input_routes<'a>(
    bus: &Bus,
    packet: &Packet<'a>,
    view: &bus::TaskSnapshot,
) -> Result<ResolvedInputRoutes<'a>, Unpublished> {
    let mut resolved = packet.clone();
    let mut parents = vec![None; resolved.routes.len()];
    let mut refused = Vec::new();
    let mut index = 0;
    while index < resolved.routes.len() {
        if resolved.routes[index].audience != Some(Audience::Input) {
            index += 1;
            continue;
        }
        let key = resolved.routes[index]
            .direction
            .clone()
            .expect("an input audience carries its exact key");
        let written = key.to_string();
        let resolution = (|| {
            let head = bus
                .read({
                    let written = written.clone();
                    move |read| read.steering_key(&written)
                })
                .map_err(|error| {
                    Unpublished::Failed(store_fault("steer::input-unreadable", &error.to_string()))
                })?
                .ok_or_else(|| Unpublished::Refused {
                    reason: "syn::input-unknown".to_owned(),
                    detail: format!("#{written} names no input"),
                })?;
            let head = head.body();
            if head.recipient != wire::Actor::Lane(packet.lane) {
                return Err(Unpublished::Refused {
                    reason: "syn::input-unauthorized".to_owned(),
                    detail: format!("#{written} is not an input this lane received"),
                });
            }
            let Some(peer) = head.sender.lane() else {
                if head.fanout.is_some() {
                    return Ok(None);
                }
                return Err(Unpublished::Refused {
                    reason: "syn::input-counterpart-missing".to_owned(),
                    detail: format!("#{written} has no authorized counterpart"),
                });
            };
            let parent = head
                .control
                .as_deref()
                .and_then(wire::parse_peer_line_control)
                .map(|(line, _, _)| line.root)
                .ok_or_else(|| Unpublished::Refused {
                    reason: "syn::input-not-line".to_owned(),
                    detail: format!("#{written} is not a peer-line input"),
                })?;
            let residency = view
                .residency()
                .and_then(|residency| residency.body().lanes.iter().find(|row| row.lane == peer))
                .ok_or_else(|| Unpublished::Refused {
                    reason: "syn::input-counterpart-unavailable".to_owned(),
                    detail: format!("#{written}'s counterpart lane is unavailable"),
                })?;
            let occupant = residency
                .occupant
                .or_else(|| view.facts().latest_lane_occupant(peer))
                .ok_or_else(|| Unpublished::Refused {
                    reason: "syn::input-counterpart-unproved".to_owned(),
                    detail: format!("#{written}'s counterpart has no proved generation"),
                })?;
            let address = view
                .facts()
                .occupant_address(peer, occupant)
                .map_err(|error| Unpublished::Refused {
                    reason: "syn::input-counterpart-unproved".to_owned(),
                    detail: error.to_string(),
                })?;
            Ok(Some((address, parent)))
        })();
        match resolution {
            Ok(Some((address, parent))) => {
                resolved.routes[index].audience = Some(Audience::Occupant(address));
                parents[index] = Some(parent);
                index += 1;
            }
            Ok(None) => index += 1,
            Err(Unpublished::Refused { reason, detail }) => {
                refused.push(RouteRefusal {
                    target: format!("#{written}"),
                    reason,
                    detail,
                });
                resolved.routes.remove(index);
                parents.remove(index);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(ResolvedInputRoutes {
        packet: resolved,
        parents,
        refused,
    })
}

/// Outbox effects and their immutable judgments share the native transaction.
pub(in crate::resident::steering) fn publish_judged(
    bus: &Bus,
    epoch: CanonicalUuid,
    config: &steer::Config,
    packet: &Packet<'_>,
    judgment: outbox::Judgment,
) -> Result<Option<Published>, Outcome<CommandAnswer>> {
    let mut attempt = 1;
    let outcome = loop {
        let view = bus
            .task_snapshot()
            .map_err(|error| store_fault("steer::residency-unreadable", &error.to_string()))?;
        match publish_packet_judged(bus, epoch, config, packet, &view, Some(judgment)) {
            Err(Unpublished::Refused { reason, .. })
                if attempt < PUBLISH_ATTEMPTS && moved(&reason) =>
            {
                attempt += 1
            }
            outcome => break outcome,
        }
    };
    match outcome {
        Ok(published) => Ok(Some(published)),
        Err(Unpublished::Failed(outcome)) => Err(outcome),
        Err(
            Unpublished::Refused { reason, detail }
            | Unpublished::Recorded { reason, detail }
            | Unpublished::Withheld { reason, detail },
        ) => {
            let at = current_instant()?;
            let mut write = JournalWrite::new(
                Actor::Spice,
                resident_origin(epoch).map_err(typed_outcome)?,
                at,
                Producer::AckScanner,
            );
            let withheld = write
                .fact(
                    wire::body::SteerWithheld {
                        from: packet.lane,
                        sender: packet.author,
                        locus: packet.locus,
                        reason: reason.clone(),
                        detail: detail.clone(),
                    },
                    FactCause::Root,
                )
                .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
            judgment.append(
                &mut write,
                wire::outbox::PacketOutcome::Refused { reason, detail },
                FactCause::Pending(withheld),
            )?;
            commit(bus, write).map_err(typed_outcome)?;
            Ok(None)
        }
    }
}
fn publish_message(
    bus: &Bus,
    epoch: CanonicalUuid,
    config: &steer::Config,
    view: &bus::TaskSnapshot,
    judgment: Option<outbox::Judgment>,
    routes: PublicationRoutes<'_, '_>,
) -> Result<Published, Unpublished> {
    let PublicationRoutes {
        packet,
        parents,
        mut refused,
    } = routes;
    let directed = occupant_routes(&packet.routes, &parents);
    let identities = identities(packet, &directed);
    let mut minted_micros = None;
    for directive in &identities {
        let (existing, minted) = keyspace(bus, *directive).map_err(Unpublished::Failed)?;
        if existing.is_some() {
            return Ok(Published::Already);
        }
        minted_micros = minted;
    }
    let sender = proved_sender(view, packet)?;
    let residency = current(view, epoch)?;
    let Some(record) = resolve_rooms(bus, packet, view, sender)? else {
        return Ok(Published::Already);
    };
    let ResolvedTargets {
        targets,
        unreached,
        unlaunchable,
        refused: route_refusals,
    } = resolve_targets(
        bus,
        &residency.body().lanes,
        view,
        packet.lane,
        &directed,
        &identities,
    )?;
    refused.extend(route_refusals);
    let at = current_instant().map_err(Unpublished::Failed)?;
    let (targets, spent) = admit_idle(bus, packet.lane, targets, at)?;
    let (unreached, spent) = finalize_routes(
        !targets.is_empty() || !record.rooms.is_empty(),
        unreached,
        unlaunchable,
        spent,
        refused,
    )?;
    let expires = expiry(at, config.expiry().get()).map_err(Unpublished::Failed)?;
    let mut proposal = lane_write(epoch, packet, at)?;
    let keys = mint_keys(&mut minted_micros, targets.len())?;
    let sending = Sending {
        epoch,
        residency: residency.position(),
        packet,
        at,
        expires,
    };
    let branches = sending.stage(&mut proposal, &targets, keys)?;
    stage_rooms(
        &mut proposal,
        residency.position(),
        &record,
        &targets,
        moment(minted_micros),
    )?;
    admit_send_envelope(&proposal).map_err(unpublished)?;
    let held_room = held_room(&mut proposal, &record.rooms, view, packet.lane);
    let resuming = queued(&targets);
    let published = Published::Sent {
        lanes: targets
            .iter()
            .filter(|target| target.occupant.is_some())
            .map(|target| target.lane)
            .collect(),
        queued: queued(&targets),
        resuming,
        redirected: targets
            .iter()
            .filter_map(|target| target.redirected.as_ref().map(Redirected::note))
            .collect(),
        unreached,
        spent,
        held_room,
        rooms: record.rooms.into_iter().map(|(handle, _)| handle).collect(),
        principals: 0,
        lines: packet.body.lines().count(),
    };
    if let Some(judgment) = judgment {
        judgment
            .append(
                &mut proposal,
                published.outcome(branches),
                FactCause::Existing(residency.position()),
            )
            .map_err(Unpublished::Failed)?;
    }
    commit(bus, proposal).map_err(|outcome| unpublished(typed_outcome(outcome)))?;
    Ok(published)
}

/// Preserve each refused route beside any branch or room that can publish.
/// Without such a success, retain the native single-route reason and otherwise
/// report the packet's aggregate availability boundary.
fn finalize_routes(
    publishes: bool,
    mut unreached: Vec<String>,
    unlaunchable: Vec<String>,
    spent: Vec<String>,
    mut refused: Vec<RouteRefusal>,
) -> Result<(Vec<String>, Vec<String>), Unpublished> {
    if publishes {
        unreached.extend(unlaunchable);
        unreached.extend(refused.into_iter().map(RouteRefusal::notice));
        return Ok((unreached, spent));
    }
    if unreached.is_empty() && unlaunchable.is_empty() && spent.is_empty() && refused.len() == 1 {
        let refused = refused.pop().expect("one route refused");
        return Err(Unpublished::Refused {
            reason: refused.reason,
            detail: refused.detail,
        });
    }
    let reason = if unreached.is_empty()
        && spent.is_empty()
        && refused.is_empty()
        && !unlaunchable.is_empty()
    {
        UNLAUNCHABLE
    } else if unreached.is_empty() && unlaunchable.is_empty() && refused.is_empty() {
        admission::SPENT
    } else {
        UNAVAILABLE
    };
    unreached.extend(unlaunchable);
    unreached.extend(spent);
    unreached.extend(refused.into_iter().map(RouteRefusal::notice));
    Err(Unpublished::Refused {
        reason: reason.to_owned(),
        detail: unreached.join("; "),
    })
}
/// Each occupant-addressed route, in route order, with its own context and
/// exact linked parent.
pub(super) fn occupant_routes(
    routes: &[Route],
    parents: &[Option<CanonicalUuid>],
) -> Vec<Directed> {
    routes
        .iter()
        .zip(parents)
        .filter_map(|(route, parent)| match route.audience {
            Some(Audience::Occupant(address)) => Some((address, route.context.clone(), *parent)),
            Some(Audience::Input) | None => None,
        })
        .collect()
}

/// The one directive identity each directed route can own.
pub(super) fn identities(packet: &Packet<'_>, directed: &[Directed]) -> Vec<CanonicalUuid> {
    directed
        .iter()
        .map(|(recipient, _, _)| message_identity(&packet.locus, *recipient))
        .collect()
}

/// The residency head this resident generation publishes against.
pub(super) fn current(
    view: &bus::TaskSnapshot,
    epoch: CanonicalUuid,
) -> Result<&journal::JournalHead<journal::crossing::ResidencyHead>, Unpublished> {
    view.residency()
        .filter(|residency| residency.body().epoch == epoch)
        .ok_or_else(|| {
            Unpublished::Failed(store_fault(
                "steer::residency-missing",
                "no current residency names this resident generation",
            ))
        })
}

pub(super) fn proved_sender(
    view: &bus::TaskSnapshot,
    packet: &Packet<'_>,
) -> Result<OccupantAddress, Unpublished> {
    view.facts()
        .occupant_address(packet.lane, packet.author)
        .map_err(|refusal| Unpublished::Refused {
            reason: "syn::sender-unproved".to_owned(),
            detail: refusal.to_string(),
        })
}

/// The address a message to `lane` names: its seated occupant's, or its
/// latest occupant's while the lane stands vacant. `None` when that occupant
/// has no proved address, so nothing can name the lane.
pub(in crate::resident::steering) fn lane_address(
    view: &bus::TaskSnapshot,
    lane: CanonicalUuid,
) -> Option<OccupantAddress> {
    let seated = view
        .residency()?
        .body()
        .lanes
        .iter()
        .find(|row| row.lane == lane)?
        .occupant;
    let occupant = seated.or_else(|| view.facts().latest_lane_occupant(lane))?;
    view.facts().occupant_address(lane, occupant).ok()
}

/// Every room the packet names and, when it names any, the packet's exact
/// transcript hunk; `None` when the packet already recorded these bytes.
///
/// Callers resolve directed routes separately; a packet with only room routes
/// still uses its room record as the proof that it published.
fn resolve_rooms(
    bus: &Bus,
    packet: &Packet<'_>,
    view: &bus::TaskSnapshot,
    sender: OccupantAddress,
) -> Result<Option<RoomRecord>, Unpublished> {
    let contexts = packet
        .routes
        .iter()
        .filter_map(|route| route.context.as_ref())
        .collect();
    let rooms = packet_rooms(contexts, view)?;
    let hunk = match rooms.first() {
        None => None,
        Some((_, room)) => {
            let hunk = packet_hunk(bus, packet)?;
            if recorded(bus, room, sender, &hunk)? {
                return Ok(None);
            }
            Some(hunk)
        }
    };
    Ok(Some(RoomRecord {
        rooms,
        hunk,
        author: packet.author,
    }))
}

/// Each directed route's target, in route order, and why each invalid,
/// unproved, removed, or expired address receives nothing. A proved address
/// whose birth lane is currently vacant opens a lane-scoped queued directive;
/// one whose birth lane now seats a later proved occupant delivers to that
/// occupant, keeping the named address beside the route. A route resolving
/// onto the author's own lane refuses independently. The first route to a
/// seat wins and each later alias of that seat refuses independently.
pub(super) fn resolve_targets(
    bus: &Bus,
    residency: &[wire::LaneResidency],
    view: &bus::TaskSnapshot,
    author: CanonicalUuid,
    directed: &[Directed],
    identities: &[CanonicalUuid],
) -> Result<ResolvedTargets, Unpublished> {
    let lanes = seats(residency, view);
    let seated = lanes
        .iter()
        .filter_map(|(lane, seat)| {
            let (occupant, address) = (*seat)?;
            Some((address?, (*lane, occupant)))
        })
        .collect::<BTreeMap<_, _>>();
    let mut targets: Vec<Target> = Vec::with_capacity(directed.len());
    let mut unreached = Vec::new();
    let mut unlaunchable = Vec::new();
    let mut refused = Vec::new();
    for ((recipient, context, parent), directive) in directed.iter().zip(identities) {
        let room = context_room(context.as_ref());
        let target = |lane, occupant, redirected| Target {
            lane,
            occupant,
            recipient: *recipient,
            directive: *directive,
            parent: *parent,
            room: room.clone(),
            redirected,
            admission: None,
        };
        let candidate = if let Some((lane, occupant)) = seated.get(recipient) {
            target(*lane, Some(*occupant), None)
        } else {
            let Ok(birth) = occupant_birth(bus, *recipient)? else {
                unreached.push(unavailable(bus, &lanes, view, *recipient)?);
                continue;
            };
            let vacant = lanes.get(&birth.lane).is_some_and(Option::is_none)
                && view.facts().latest_lane_occupant(birth.lane) == Some(birth.occupant);
            if vacant {
                let row = residency
                    .iter()
                    .find(|row| row.lane == birth.lane)
                    .expect("a resolved vacant lane remains in its residency");
                if let Err(failure) = lane_capability(row) {
                    let (_, detail) = describe(&typed_outcome(failure));
                    unlaunchable.push(format!("{recipient} {UNLAUNCHABLE} ({detail})"));
                    continue;
                }
                target(birth.lane, None, None)
            } else if let Some(successor) = successor_seat(bus, &lanes, &birth)? {
                let redirected = Redirected {
                    addressed: birth,
                    successor,
                };
                target(birth.lane, Some(successor.occupant), Some(redirected))
            } else {
                unreached.push(unavailable(bus, &lanes, view, *recipient)?);
                continue;
            }
        };
        if candidate.lane == author {
            let detail = if candidate.redirected.is_some() {
                format!(
                    "{} is an earlier address of the authoring occupant's own lane",
                    candidate.recipient
                )
            } else {
                format!(
                    "{} is the authoring occupant's own address",
                    candidate.recipient
                )
            };
            refused.push(RouteRefusal {
                target: candidate.recipient.to_string(),
                reason: "syn::recipient-self".to_owned(),
                detail,
            });
            continue;
        }
        let repeated = candidate.occupant.and_then(|occupant| {
            targets
                .iter()
                .find(|target| target.lane == candidate.lane && target.occupant == Some(occupant))
        });
        if let Some(first) = repeated {
            let current = [first, &candidate]
                .into_iter()
                .find_map(|named| named.redirected.map(|route| route.successor.address))
                .unwrap_or(candidate.recipient);
            refused.push(RouteRefusal {
                target: candidate.recipient.to_string(),
                reason: "syn::recipient-same-seat".to_owned(),
                detail: format!(
                    "{} and {} both name the seat {current} holds",
                    first.recipient, candidate.recipient
                ),
            });
            continue;
        }
        targets.push(candidate);
    }
    Ok(ResolvedTargets {
        targets,
        unreached,
        unlaunchable,
        refused,
    })
}

/// The proved occupant now seated on an expired address's birth lane, when the
/// residency seats one with a proved address of its own.
fn successor_seat(
    bus: &Bus,
    lanes: &Seats,
    birth: &bus::OccupantBirth,
) -> Result<Option<bus::OccupantBirth>, Unpublished> {
    let Some(Some((occupant, Some(address)))) = lanes.get(&birth.lane) else {
        return Ok(None);
    };
    if *occupant == birth.occupant {
        return Ok(None);
    }
    Ok(occupant_birth(bus, *address)?
        .ok()
        .filter(|successor| successor.lane == birth.lane && successor.occupant == *occupant))
}
/// Stage one `room::messaged` record of the packet in each room, naming the
/// directives its delivered or queued routes into that room opened.
pub(super) fn stage_rooms(
    proposal: &mut JournalWrite,
    residency: wire::Seq,
    record: &RoomRecord,
    targets: &[Target],
    moment: i64,
) -> Result<(), Unpublished> {
    let Some(hunk) = &record.hunk else {
        return Ok(());
    };
    for ((_, room), offset) in record.rooms.iter().zip(0_usize..) {
        let publication = conversation_stamp(moment, offset)?;
        proposal
            .fact(
                RoomMessaged {
                    room: room.clone(),
                    publication,
                    hunk: hunk.clone(),
                    author: record.author,
                    directed: targets
                        .iter()
                        .filter(|target| target.room.as_ref() == Some(room))
                        .map(|target| target.directive)
                        .collect(),
                },
                FactCause::Existing(residency),
            )
            .map_err(fact_invalid)?;
    }
    Ok(())
}
/// Every distinct room `contexts` name, in source order, each resolved by
/// birth stamp against `view`; a context naming no task refuses the whole
/// packet.
pub(super) fn packet_rooms(
    contexts: Vec<&TaskHandle>,
    view: &bus::TaskSnapshot,
) -> Result<Rooms, Unpublished> {
    let mut rooms = Rooms::new();
    for handle in contexts {
        let born = handle
            .born()
            .ok()
            .filter(|born| view.task_heads(born).is_some())
            .ok_or_else(|| Unpublished::Refused {
                reason: "syn::context-unknown".to_owned(),
                detail: format!("{handle} names no task"),
            })?;
        if rooms.iter().all(|(_, known)| *known != born) {
            rooms.push((handle.clone(), born));
        }
    }
    Ok(rooms)
}
/// The packet body's exact bytes in its author's accounted transcript.
///
/// Publication follows the sweep that accounted the record, so a record the
/// lane's accounted generation does not yet cover is undecided, not refused:
/// a later read publishes it.
pub(super) fn packet_hunk(bus: &Bus, packet: &Packet<'_>) -> Result<TranscriptHunk, Unpublished> {
    let locus = &packet.locus;
    let unaccounted = || {
        Unpublished::Failed(store_fault(
            "syn::transcript-unaccounted",
            &format!(
                "lane {} record {}:{} has no accounted transcript generation",
                packet.lane, locus.member, locus.offset
            ),
        ))
    };
    let source = locus.source.ok_or_else(unaccounted)?;
    let lane = packet.lane;
    let head = bus
        .read(move |read| read.transcript_source_heads(&lane))
        .map_err(|error| {
            Unpublished::Failed(store_fault(
                "steer::transcript-unreadable",
                &error.to_string(),
            ))
        })?
        .into_iter()
        .map(journal::JournalHead::into_body)
        .find(|head| {
            let end = locus.offset + locus.length;
            head.source == source
                && (locus.member < head.next.member
                    || (locus.member == head.next.member && end <= head.next.offset))
        })
        .ok_or_else(unaccounted)?;
    let offset = |at: usize| {
        u64::try_from(at).map_err(|error| {
            Unpublished::Failed(protocol_fault(
                "steer::transcript-locus-invalid",
                &error.to_string(),
            ))
        })
    };
    Ok(TranscriptHunk {
        head,
        locus: wire::MessageLocus {
            member: locus.member,
            offset: locus.offset,
            length: locus.length,
            event: locus.event,
        },
        text: TextRange {
            start: offset(packet.text.start)?,
            end: offset(packet.text.end)?,
        },
    })
}
