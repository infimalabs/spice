//! Browser discovery admission and platform launch.

use super::*;

// There is deliberately no `open_app(path)` convenience that admits and
// launches in one step. Both commands that can open the app gate the launch on
// an explicit flag, and a helper that hard-wires `system_browser` is exactly
// the shape that made `spice demo` seize the operator's browser on every run in
// an agent's hot loop. Callers pass their own already-gated launcher.

/// Admit the browser surface named by the current owner-only discovery file,
/// and hand its endpoint to `launch`.
///
/// This deliberately shares [`DiscoveryRead`] with resident commands. App
/// acquisition therefore has one file-identity, ownership, mode, wire-shape,
/// and loopback-origin admission path rather than a browser-specific reader.
/// The resident announcing its own freshly published discovery goes through
/// here too, so the endpoint an operator is told to open is one that already
/// passed the admission a client would apply to it.
pub(crate) fn open_app_with(
    discovery_path: &Path,
    before_current_check: impl FnOnce(),
    launch: impl FnOnce(&str) -> Result<(), String>,
) -> Outcome<wire::AppOpen> {
    match open_app_result_with(discovery_path, before_current_check, launch) {
        Ok(body) => Outcome::Reply(Reply {
            position: ReplyPosition::Direct {},
            body,
        }),
        Err(fault) => Outcome::Fault(fault),
    }
}

/// Admit and launch the app through the fault-only discovery boundary.
pub(crate) fn open_app_result_with(
    discovery_path: &Path,
    before_current_check: impl FnOnce(),
    launch: impl FnOnce(&str) -> Result<(), String>,
) -> Result<wire::AppOpen, Fault> {
    let discovery = DiscoveryRead::open_fault(discovery_path, &resident_lock_path(discovery_path))?;
    dialable_endpoint(&discovery.value.endpoint)?;
    before_current_check();
    discovery.verify_current_fault()?;
    discovery.require_live_publisher()?;
    if let Err(detail) = launch(&discovery.value.endpoint) {
        return Err(Fault {
            class: FaultClass::Environment,
            reason: "serve::launch-failed".to_owned(),
            detail,
        });
    }
    Ok(wire::AppOpen {
        endpoint: discovery.value.endpoint,
        epoch: discovery.value.epoch,
    })
}

pub(crate) fn system_browser(endpoint: &str) -> Result<(), String> {
    let mut command = browser_command(endpoint)?;
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("the platform browser launcher did not start: {error}"))?;
    if !status.success() {
        return Err(format!(
            "the platform browser launcher did not accept the current endpoint: {status}"
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) fn browser_command(endpoint: &str) -> Result<ProcessCommand, String> {
    let mut command = ProcessCommand::new("/usr/bin/open");
    command.arg("-u").arg(endpoint);
    Ok(command)
}

#[cfg(target_os = "linux")]
pub(super) fn browser_command(endpoint: &str) -> Result<ProcessCommand, String> {
    let mut command = ProcessCommand::new("/usr/bin/xdg-open");
    command.arg(endpoint);
    Ok(command)
}

/// The URL protocol handler `rundll32` exposes opens the default browser
/// without a shell, so no `cmd` metacharacter in the URL is ever interpreted.
#[cfg(windows)]
pub(super) fn browser_command(endpoint: &str) -> Result<ProcessCommand, String> {
    let mut command = ProcessCommand::new(r"C:\Windows\System32\rundll32.exe");
    command.arg("url.dll,FileProtocolHandler").arg(endpoint);
    Ok(command)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(super) fn browser_command(_endpoint: &str) -> Result<ProcessCommand, String> {
    Err("browser launch is unsupported on this build target".to_owned())
}
