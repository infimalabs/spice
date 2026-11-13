//! Browser bootstrap discovery, socket opening, and principal hydration.

use super::*;

/// Establish every prerequisite of the resident event loop as one boundary.
pub(super) async fn establish_connection(
    connection: &UseStateHandle<Connection>,
    surface: &UseStateHandle<SurfaceData>,
    identity: &UseStateHandle<Option<AppIdentity>>,
    app_update: &UseStateHandle<Option<AppUpdate>>,
    served_app: &mut Option<wire::Digest>,
    data: &mut SurfaceData,
    signal: &web_sys::AbortSignal,
) -> Result<EstablishedConnection, String> {
    let (session, mut writer, mut reader) =
        open_socket(identity, app_update, served_app, signal).await?;
    let (principal, hydrating) = admit_runtime(
        &session,
        &mut writer,
        &mut reader,
        connection,
        surface,
        data,
    )
    .await?;
    Ok(EstablishedConnection {
        session,
        writer,
        reader,
        principal,
        hydrating,
    })
}

/// Discover the resident endpoint and open the socket its bootstrap names.
///
/// `served_app` names the generation this resident serves whenever it is not
/// the loaded document's, even when the attempt then fails. Such a document
/// reloads into it before any session opens; one that cannot leave keeps its
/// update offer.
async fn open_socket(
    identity: &UseStateHandle<Option<AppIdentity>>,
    app_update: &UseStateHandle<Option<AppUpdate>>,
    served_app: &mut Option<wire::Digest>,
    signal: &web_sys::AbortSignal,
) -> Result<(Session, SocketWriter, SocketReader), String> {
    let response = Request::get(&authenticated_path(
        "/bootstrap",
        browser_auth_token()?.as_deref(),
    ))
    .abort_signal(Some(signal))
    .send()
    .await
    .map_err(|error| format!("bootstrap request failed: {error}"))?;
    let app_protocol = response
        .headers()
        .get(wire::hello::APP_PROTOCOL_VERSION_HEADER)
        .map(|value| {
            value
                .parse::<wire::AppProtocolVersion>()
                .map_err(|error| format!("bootstrap app protocol header was invalid: {error}"))
        })
        .transpose()?;
    let generation = response
        .headers()
        .get(wire::hello::APP_GENERATION_HEADER)
        .ok_or_else(|| "bootstrap carried no app generation".to_owned())?
        .parse::<wire::Digest>()
        .map_err(|error| format!("bootstrap app generation was invalid: {error}"))?;
    let loaded_generation = loaded_app_generation()?;
    let update = app_protocol.and_then(|version| {
        (!version.is_current() || generation != loaded_generation).then(|| AppUpdate {
            version,
            generation: generation.clone(),
            loaded_generation: loaded_generation.clone(),
        })
    });
    *served_app = update.as_ref().map(|update| update.generation.clone());
    app_update.set(update);
    if let Some(served) = served_app.as_ref()
        && *served != loaded_generation
        && super::leave_for_served_app(served)
    {
        return Err(format!("reloading into served app generation {served}"));
    }
    if let Err(fault) = Session::admit_app_protocol(app_protocol) {
        return Err(format!(
            "{}: {}; explicit app update required",
            fault.reason, fault.detail
        ));
    }
    let discovery: wire::AppBootstrap = response
        .json()
        .await
        .map_err(|error| format!("bootstrap response was invalid: {error}"))?;
    let app_protocol = app_protocol.expect("current app admission requires its version header");
    identity.set(Some(admit_bootstrap_identity(
        &discovery,
        app_protocol,
        &generation,
        loaded_generation,
    )?));
    let session = Session::open(discovery.capability, discovery.epoch);
    let endpoint = discovery
        .endpoint
        .strip_prefix("http://")
        .ok_or_else(|| "bootstrap endpoint was not the current HTTP origin".to_owned())?;
    let socket = WebSocket::open(&format!(
        "ws://{endpoint}/socket?auth={}",
        discovery.capability
    ))
    .map_err(|error| format!("socket did not open: {error}"))?;
    let (mut writer, reader) = socket.split();
    writer
        .send(Message::Bytes(
            wire::encode(&session.hello()).map_err(|error| error.to_string())?,
        ))
        .await
        .map_err(websocket_error)?;
    Ok((session, writer, reader))
}

fn admit_bootstrap_identity(
    discovery: &wire::AppBootstrap,
    app_protocol: wire::AppProtocolVersion,
    generation: &wire::Digest,
    loaded_app: wire::Digest,
) -> Result<AppIdentity, String> {
    if discovery.version.app_protocol != app_protocol {
        return Err("bootstrap body app protocol disagreed with its header".to_owned());
    }
    if &discovery.version.app != generation {
        return Err("bootstrap body app generation disagreed with its header".to_owned());
    }
    if discovery.version.wire != wire::WireVersion::CURRENT {
        return Err("bootstrap body crossing wire generation is not current".to_owned());
    }
    Ok(AppIdentity {
        version: discovery.version.clone(),
        loaded_app,
    })
}

pub(in crate::browser) fn loaded_app_generation() -> Result<wire::Digest, String> {
    web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id("app"))
        .and_then(|root| root.get_attribute("data-app-generation"))
        .ok_or_else(|| "loaded app carried no generation identity".to_owned())?
        .parse()
        .map_err(|error| format!("loaded app generation was invalid: {error}"))
}

/// Admit the acceptance frame and publish the runtime that frame names.
async fn admit_runtime(
    session: &Session,
    writer: &mut SocketWriter,
    reader: &mut SocketReader,
    connection: &UseStateHandle<Connection>,
    surface: &UseStateHandle<SurfaceData>,
    data: &mut SurfaceData,
) -> Result<(Principal, bool), String> {
    let accepted_bytes = binary_message(
        reader
            .next()
            .await
            .ok_or_else(|| "socket closed before acceptance".to_owned())?
            .map_err(websocket_error)?,
    )?;
    let accepted = session
        .admit_accepted(&accepted_bytes)
        .map_err(|fault| format!("{}: {}", fault.reason, fault.detail))?;
    let (initial_principal, hydrating) = data.admit(&accepted);
    let principal = bootstrap_principal(session, writer, reader, data, &initial_principal).await?;
    surface.set(data.publication());
    if !hydrating {
        connection.set(Connection::Online {
            epoch: principal.epoch().to_string(),
        });
    }
    Ok((principal, hydrating))
}

async fn bootstrap_principal(
    session: &Session,
    writer: &mut SocketWriter,
    reader: &mut SocketReader,
    data: &SurfaceData,
    initial: &Principal,
) -> Result<Principal, String> {
    let command = SteerPane::over(initial)
        .command(
            random_uuid()?,
            CommandKind::PrincipalDirectoryRead,
            vec![],
            wire::principal::PrincipalDirectoryRead {},
        )
        .map_err(|error| error.to_string())?;
    writer
        .send(Message::Bytes(
            wire::encode(&command).map_err(|error| error.to_string())?,
        ))
        .await
        .map_err(websocket_error)?;
    let bytes = binary_message(
        reader
            .next()
            .await
            .ok_or_else(|| "socket closed before principal bootstrap".to_owned())?
            .map_err(websocket_error)?,
    )?;
    let admitted = admit_response::<wire::principal::PrincipalDirectoryAnswer>(session, &bytes)?;
    match &admitted.get().outcome {
        wire::Outcome::Reply(_) => {}
        wire::Outcome::Refusal(refusal) => {
            return Err(format!(
                "principal bootstrap refused ({}): {}",
                refusal.reason(),
                refusal.explanation()
            ));
        }
        wire::Outcome::Fault(fault) => {
            return Err(format!(
                "principal bootstrap failed ({}): {}",
                fault.reason, fault.detail
            ));
        }
    }
    let mut runtime = data.write();
    let runtime = runtime
        .as_mut()
        .ok_or_else(|| "accepted connection created no runtime".to_owned())?;
    runtime.apply_principals(&admitted);
    Ok(runtime.principal().clone())
}
