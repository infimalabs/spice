//! Principal-input SYN publication with its `room::conversed` record.

use wire::rooms::{PrincipalInputRef, RoomAnchor};

use super::*;

/// Publish one principal-input SYN with its conversation record in one write.
///
/// Each admitted lane route is one native peer directive, as for a plain SYN;
/// it is delivered now when seated or queued when its proved lane is vacant.
/// Each principal route mints its own steering input on its own line. The
/// conversation record names the lane directives and exact principal inputs.
/// Every independently admitted route remains effective when another route
/// can neither deliver nor queue.
pub(super) fn publish_conversation(
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
    let residency = current(view, epoch)?;
    let sender = proved_sender(view, packet)?;
    let hunk = packet_hunk(bus, packet)?;
    if conversed(bus, packet.lane, sender, &hunk)? {
        return Ok(Published::Already);
    }
    let directed = occupant_routes(&packet.routes, &parents);
    let contexts = packet
        .routes
        .iter()
        .filter_map(|route| route.context.as_ref())
        .collect();
    let rooms = packet_rooms(contexts, view)?;
    let identities = identities(packet, &directed);
    let ResolvedTargets {
        targets,
        mut unreached,
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
    unreached.extend(unlaunchable);
    unreached.extend(refused.into_iter().map(RouteRefusal::notice));
    let at = current_instant().map_err(Unpublished::Failed)?;
    let (targets, spent) = admit_idle(bus, packet.lane, targets, at)?;
    let mut minted_micros = minted(bus)?;
    let expires = expiry(at, config.expiry().get()).map_err(Unpublished::Failed)?;
    let routes = conversation_routes(bus, packet, &targets, &identities)?;
    let principals = principal_routes(&routes).len();
    let mut keys = mint_keys(&mut minted_micros, targets.len() + principals)?;
    let principal_keys = keys.split_off(targets.len());
    let record = RoomRecord {
        hunk: (!rooms.is_empty()).then(|| hunk.clone()),
        rooms,
        author: packet.author,
    };
    let moment = moment(minted_micros);
    let mut proposal = lane_write(epoch, packet, at)?;
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
        moment,
    )?;
    let conversed = stage_conversed(
        &mut proposal,
        &sending,
        RoomConversed {
            publication: conversation_stamp(moment, record.rooms.len())?,
            hunk,
            operation: RoomPublicationKind::Message {},
            routes,
            directed: targets.iter().map(|target| target.directive).collect(),
            expires,
        },
        principal_keys,
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
        rooms: record.rooms.into_iter().map(|(handle, _)| handle).collect(),
        principals,
        held_room,
        lines: packet.body.lines().count(),
    };
    if let Some(judgment) = judgment {
        judgment
            .append(
                &mut proposal,
                published.outcome(branches),
                FactCause::Pending(conversed),
            )
            .map_err(Unpublished::Failed)?;
    }
    commit(bus, proposal).map_err(|outcome| unpublished(typed_outcome(outcome)))?;
    Ok(published)
}

pub(super) fn conversation_stamp(
    moment: i64,
    offset: usize,
) -> Result<RoomAuthorityStamp, Unpublished> {
    let offset = i64::try_from(offset).unwrap_or(i64::MAX);
    RoomAuthorityStamp::for_micros(moment.saturating_add(offset)).map_err(|error| {
        Unpublished::Failed(store_fault("syn::room-stamp-invalid", &error.to_string()))
    })
}

/// Each principal route's principal with its own context, in header order.
fn principal_routes(routes: &[RoomRoute]) -> Vec<(CanonicalUuid, Option<RoomAnchor>)> {
    routes
        .iter()
        .filter_map(|route| match route.destination {
            RoomDestination::Principal { principal } => Some((principal, route.context.clone())),
            RoomDestination::Lane { .. } | RoomDestination::Task { .. } => None,
        })
        .collect()
}

/// Stage the conversation record, then the steering input each principal
/// route mints on its own line, caused by the record so a reader returns from
/// the input to its publication. Each input's identity is its route's
/// [`PrincipalInputRef`]; `keys` holds one fresh key per principal route.
fn stage_conversed(
    proposal: &mut JournalWrite,
    sending: &Sending<'_, '_>,
    conversed: RoomConversed,
    keys: Vec<steer::Key>,
) -> Result<PendingFact, Unpublished> {
    let origin = resident_origin(sending.epoch)
        .map_err(|outcome| Unpublished::Failed(typed_outcome(outcome)))?;
    let principals = principal_routes(&conversed.routes);
    let publication = conversed.publication.clone();
    let record = proposal
        .fact(conversed, FactCause::Existing(sending.residency))
        .map_err(fact_invalid)?;
    for ((principal, room), key) in principals.into_iter().zip(keys) {
        let directive = PrincipalInputRef::published(&publication, principal).directive;
        proposal.guard(internal_send_guard(directive, key.clone()));
        let line = wire::body::PeerLineRef {
            root: directive,
            parent: None,
            opening: directive,
        };
        let sent = wire::body::SteerSent {
            directive,
            fanout: None,
            key: key.to_string(),
            recipient: Actor::Principal(principal),
            text: sending.packet.body.to_owned(),
            sender: Actor::Lane(sending.packet.lane),
            room,
            control: Some(wire::peer_line_control(
                line,
                wire::body::PeerBodyKind::Open,
                &[],
            )),
            attachments: Vec::new(),
            recipients: Vec::new(),
            expires: sending.expires,
        };
        let pending = proposal
            .fact(sent.clone(), FactCause::Pending(record))
            .map_err(fact_invalid)?;
        let (origin, at) = (origin.clone(), sending.at);
        proposal.steering_head(pending, move |seq| {
            steer::Ledger::sent_head(&sent, origin, at, seq)
                .expect("the principal input builds its authority head")
        });
    }
    Ok(record)
}

/// The routes a conversation record freezes: the principal input, each
/// admitted lane route with its own context, and each room-only route, in
/// header order. A lane route is admitted when it delivers to a seat or queues
/// for a vacant lane; a principal route names an exact input that principal
/// sent this lane.
fn conversation_routes(
    bus: &Bus,
    packet: &Packet<'_>,
    targets: &[Target],
    identities: &[CanonicalUuid],
) -> Result<Vec<RoomRoute>, Unpublished> {
    let mut directed = identities.iter();
    let mut routes = Vec::new();
    for route in &packet.routes {
        let context = context_room(route.context.as_ref());
        let (destination, direction) = match route.audience {
            Some(Audience::Input) => {
                let key = route.direction.as_ref().ok_or_else(|| {
                    Unpublished::Failed(protocol_fault(
                        "syn::direction-missing",
                        "an input route names no direction",
                    ))
                })?;
                let (direction, principal) = principal_direction(bus, packet.lane, key)?;
                (RoomDestination::Principal { principal }, Some(direction))
            }
            Some(Audience::Occupant(_)) => {
                let directive = directed.next().copied();
                let Some(target) = targets
                    .iter()
                    .find(|target| Some(target.directive) == directive)
                else {
                    continue;
                };
                let direction = route
                    .direction
                    .as_ref()
                    .map(|key| principal_direction(bus, packet.lane, key).map(|(key, _)| key))
                    .transpose()?;
                (RoomDestination::Lane { lane: target.lane }, direction)
            }
            None => {
                let task = context.ok_or_else(|| {
                    Unpublished::Failed(protocol_fault(
                        "syn::route-empty",
                        "a room-only route names no task",
                    ))
                })?;
                routes.push(RoomRoute {
                    destination: RoomDestination::Task { task },
                    context: None,
                    direction: None,
                });
                continue;
            }
        };
        routes.push(RoomRoute {
            destination,
            context,
            direction,
        });
    }
    Ok(routes)
}

/// `key` when it names a direction the principal sent `lane`.
fn principal_direction(
    bus: &Bus,
    lane: CanonicalUuid,
    key: &steer::Key,
) -> Result<(String, CanonicalUuid), Unpublished> {
    let key = key.to_string();
    let read_key = key.clone();
    let head = bus
        .read(move |read| read.steering_key(&read_key))
        .map_err(|error| {
            Unpublished::Failed(store_fault("steer::room-unreadable", &error.to_string()))
        })?;
    let refuse = |reason: &str, detail: String| {
        Err(Unpublished::Refused {
            reason: reason.to_owned(),
            detail,
        })
    };
    match head.map(|head| head.into_body()) {
        None => refuse(
            "syn::direction-unknown",
            format!("#{key} names no direction"),
        ),
        Some(head) if head.recipient != wire::Actor::Lane(lane) => refuse(
            "syn::direction-other-lane",
            format!("#{key} addresses another lane"),
        ),
        Some(head) if head.sender.principal().is_none() || head.fanout.is_none() => refuse(
            "syn::direction-not-principal",
            format!("#{key} is not a direction from a principal"),
        ),
        Some(head) => Ok((key, head.sender.principal().expect("checked above"))),
    }
}

/// Whether this lane already admitted a conversation record of these exact
/// bytes since the author's birth.
fn conversed(
    bus: &Bus,
    lane: CanonicalUuid,
    author: OccupantAddress,
    hunk: &TranscriptHunk,
) -> Result<bool, Unpublished> {
    let hunk = hunk.clone();
    bus.read(move |read| {
        let mut after = Some(author.birth());
        loop {
            let page = read.selected_fact_page(
                &[wire::FactKind::RoomConversed],
                after,
                None,
                journal::FactReadBudget::RETAINED,
            )?;
            if page.rows.iter().any(|envelope| {
                envelope
                    .fact
                    .read::<RoomConversed>()
                    .is_some_and(|body| body.hunk.head.lane == lane && body.hunk.same_bytes(&hunk))
            }) {
                return Ok(true);
            }
            match page.continuation {
                Some(continuation) => after = Some(continuation),
                None => return Ok(false),
            }
        }
    })
    .map_err(|error| Unpublished::Failed(store_fault("steer::room-unreadable", &error.to_string())))
}

/// The exact historical address of each route queued for a vacant lane. Every
/// such route was admitted with the start its recorded cause commits.
pub(super) fn queued(targets: &[Target]) -> Vec<OccupantAddress> {
    targets
        .iter()
        .filter(|target| target.occupant.is_none())
        .map(|target| target.recipient)
        .collect()
}

/// Whether any room the packet names is a task the sender holds, read from
/// `view` and re-proved by a guard at commit. When the claim authority behind
/// it moved since `view`, the write refuses as moved and the publication
/// re-resolves, so a receipt never states authority its commit no longer has.
pub(super) fn held_room(
    proposal: &mut JournalWrite,
    rooms: &Rooms,
    view: &bus::TaskSnapshot,
    from: CanonicalUuid,
) -> bool {
    let held: Vec<_> = rooms
        .iter()
        .map(|(_, room)| {
            let claims = view
                .task_heads(room)
                .map_or(&[][..], |heads| heads.claims());
            (room.clone(), holds(claims, from))
        })
        .collect();
    let held_room = held.iter().any(|(_, held)| *held);
    if !held.is_empty() {
        proposal.guard(JournalGuard::new(move |read| {
            for (room, held) in &held {
                if holds(&read.task_claims(room)?, from) != *held {
                    return Err(GuardError::refused(JUDGMENT_MOVED));
                }
            }
            Ok(())
        }));
    }
    held_room
}

/// Whether `lane` holds an active claim among one task's `claims`.
fn holds(claims: &[journal::ClaimHeads], lane: CanonicalUuid) -> bool {
    claims.iter().any(|held| {
        let claim = held.claim().body();
        claim.disposition == wire::ClaimDisposition::Active && claim.lane == lane
    })
}
