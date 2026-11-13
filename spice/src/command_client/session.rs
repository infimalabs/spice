//! Opening one authenticated session on the socket a discovery record names.
//!
//! The record is a document; the session is a connection to whoever is still
//! holding the lock behind it. Everything here exists to keep those two apart:
//! the endpoint is accepted only as one literal socket authority, the
//! publisher is proved alive on both sides of the connect, and the capability
//! is written into a request only after that second proof.

use super::*;

pub(super) struct AuthenticatedSession {
    pub(super) socket: WebSocket<DeadlineStream>,
    pub(super) discovery: DiscoveryRead,
    pub(super) accepted: wire::Accepted,
}

pub(super) fn open_session(
    contract: &Contract,
    discovery: DiscoveryRead,
    deadline: Instant,
    bound: Duration,
) -> ClientResult<AuthenticatedSession> {
    open_session_with(contract, discovery, deadline, bound, || {})
}

/// The request that carries the capability, built once the endpoint is known.
///
/// Everything secret about a session is in here: the capability rides in the
/// query and the endpoint rides in `Origin`. Keeping its construction separate
/// from the opening makes the one ordering that matters legible — nothing
/// builds this until the publisher has been proved alive a second time.
pub(super) fn capability_request(
    authority: &str,
    discovery: &DiscoveryRead,
) -> ClientResult<tungstenite::handshake::client::Request> {
    let mut request = match format!(
        "ws://{authority}/socket?auth={}",
        discovery.value.capability
    )
    .into_client_request()
    {
        Ok(request) => request,
        Err(_) => {
            return Err(protocol_fault(
                "serve::request-invalid",
                "app endpoint cannot form the current socket request",
            ));
        }
    };
    let origin = match discovery
        .value
        .endpoint
        .parse::<tungstenite::http::HeaderValue>()
    {
        Ok(origin) => origin,
        Err(_) => {
            return Err(protocol_fault(
                "serve::origin-invalid",
                "app endpoint cannot form the current socket origin",
            ));
        }
    };
    request.headers_mut().insert(ORIGIN, origin);
    Ok(request)
}

/// Open a session, proving the publisher alive on both sides of the connect.
///
/// The check before the connect cannot cover the connect itself. In the
/// interval between proving the lock held and the kernel completing the TCP
/// handshake, the resident can die and anything at all can bind the port it
/// vacates — the socket that comes back is then a stranger's. Re-reading the
/// record does not answer this and never will: the record is still exactly
/// what its dead publisher wrote, byte for byte, which is the whole reason the
/// lock is the signal and the bytes are not.
///
/// So the lock is probed again after the connect, and the capability is not
/// written into a request until it answers. The order is the point. A
/// capability spent on a foreign listener cannot be unspent, and everything
/// past this line puts it on the wire. The earlier check is kept rather than
/// replaced: it refuses a record already known dead without opening a
/// connection to anyone at all.
///
/// `after_connect` is the seam a test uses to lose the publisher inside that
/// interval, which is otherwise too narrow to enter on purpose.
pub(super) fn open_session_with(
    contract: &Contract,
    discovery: DiscoveryRead,
    deadline: Instant,
    bound: Duration,
    after_connect: impl FnOnce(),
) -> ClientResult<AuthenticatedSession> {
    let (authority, address) =
        dialable_endpoint(&discovery.value.endpoint).map_err(Outcome::Fault)?;
    discovery.verify_current()?;
    discovery.require_live_publisher().map_err(Outcome::Fault)?;
    let stream = match DeadlineStream::connect(&address, deadline, bound) {
        Ok(stream) => stream,
        Err(error) => {
            return Err(environment_fault(
                "serve::connect-failed",
                error.to_string(),
            ));
        }
    };
    after_connect();
    discovery.verify_current()?;
    discovery.require_live_publisher().map_err(Outcome::Fault)?;
    let request = capability_request(authority, &discovery)?;
    let opening = OpeningBound {
        bound: stream.bound,
        deadline: stream.deadline(),
    };
    // The resident answers in one unfragmented frame, so the 64 MiB message
    // bound, not tungstenite's 16 MiB frame default, must bound a reply: a
    // full board answer outgrew the frame default.
    let config = tungstenite::protocol::WebSocketConfig::default().max_frame_size(None);
    let (mut socket, _) =
        match tungstenite::client::client_with_config(request, stream, Some(config)) {
            Ok(opened) => opened,
            Err(error) => {
                return Err(opening_fault(opening, error.to_string()));
            }
        };
    discovery.verify_current()?;
    let hello = wire::Hello {
        wire: wire::WireVersion::CURRENT,
        capability: discovery.value.capability,
        epoch: discovery.value.epoch,
    };
    send(&mut socket, &hello)?;
    let opening = receive(&mut socket)?;
    let accepted = decode_opening(&opening, &discovery.value)?;
    if !principal_matches(contract.principal, &accepted.actor) {
        return Err(Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "command::principal-mismatch",
                Some("principal".to_owned()),
                Some(contract.principal.label().to_owned()),
                Some(accepted.actor.to_string()),
            )
            .with_detail(format!(
                "{} requires the {} principal; this caller's resident capability resolves to {}",
                contract.syntax,
                contract.principal.label(),
                accepted.actor
            )),
        )));
    }
    discovery.verify_current()?;
    Ok(AuthenticatedSession {
        socket,
        discovery,
        accepted,
    })
}
pub(super) fn endpoint_authority(endpoint: &str) -> Result<&str, Fault> {
    let Some(authority) = endpoint.strip_prefix("http://") else {
        return Err(protocol_fault_value(
            "serve::endpoint-invalid",
            "app endpoint is not the current HTTP origin",
        ));
    };
    if authority.is_empty() || authority.contains(['/', '?', '#', '@']) {
        return Err(protocol_fault_value(
            "serve::endpoint-invalid",
            "app endpoint contains more than one socket authority",
        ));
    }
    Ok(authority)
}

/// The one socket a published discovery document names.
///
/// Any bound address is dialable: the resident binds the address its operator
/// asked for, and refusing to dial the address it published would leave every
/// local command unable to reach a surface that is running and healthy. What
/// the endpoint must be is one literal socket authority, which is what keeps a
/// corrupted document from steering a client at a name it resolves somewhere
/// else.
pub(super) fn dialable_endpoint(endpoint: &str) -> Result<(&str, SocketAddr), Fault> {
    let authority = endpoint_authority(endpoint)?;
    match authority.parse::<SocketAddr>() {
        Ok(address) => Ok((authority, address)),
        Err(_) => Err(protocol_fault_value(
            "serve::endpoint-invalid",
            "resident discovery does not name one socket authority",
        )),
    }
}

pub(super) fn decode_opening(
    bytes: &[u8],
    discovery: &serve::Discovery,
) -> ClientResult<wire::Accepted> {
    if let Ok(accepted) = wire::decode::<wire::Accepted>(bytes) {
        if accepted.wire.is_current()
            && accepted.epoch == discovery.epoch
            && matches!(accepted.actor, Actor::Lane(_) | Actor::Principal(_))
        {
            return Ok(accepted);
        }
        return Err(protocol_fault(
            "hello::acceptance-invalid",
            "resident acceptance does not bind the current wire, epoch, and principal",
        ));
    }
    if let Ok(refusal) = wire::decode::<Refusal>(bytes) {
        if discloses_capability(bytes, discovery.capability) {
            return Err(capability_disclosed());
        }
        return Err(Outcome::Refusal(refusal));
    }
    if let Ok(fault) = wire::decode::<Fault>(bytes) {
        if discloses_capability(bytes, discovery.capability) {
            return Err(capability_disclosed());
        }
        return Err(Outcome::Fault(fault));
    }
    Err(protocol_fault(
        "hello::answer-undecodable",
        "resident returned neither a current acceptance, refusal, nor fault",
    ))
}
