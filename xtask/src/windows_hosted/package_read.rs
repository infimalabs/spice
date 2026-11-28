//! Proof, before any hosted dispatch, that this coordinator's `gh` identity
//! can read the repository's prepared GHCR package.
//!
//! GHCR, not the credential's advertised scopes, decides. Its token endpoint
//! issues a pull token to any authenticated GitHub user, so the check spends
//! that token on one registry read of the prepared package, the same
//! authorization a prepared-bundle fetch needs. Classic, fine-grained, and app
//! credentials pass or refuse alike on that answer.

use serde::Deserialize;
use std::path::Path;
use std::time::Duration;

use super::{GHCR_USERNAME, WindowsHostedError, command, command_text, prepared_repository};

const REGISTRY: &str = "ghcr.io";
const HTTP_OK: &str = "200";
const PREPARED_READ_RUNTIME: Duration = Duration::from_secs(2 * 60);

#[derive(Deserialize)]
struct RegistryToken {
    token: String,
}

#[derive(Deserialize)]
struct RegistryErrors {
    errors: Vec<RegistryError>,
}

#[derive(Deserialize)]
struct RegistryError {
    code: String,
    message: String,
}

/// Refuse unless the selected `gh` credential reads the repository's prepared
/// package on GHCR.
///
/// # Errors
///
/// Refuses when `gh` holds no credential, GHCR cannot be reached, or the
/// registry denies the read, naming GHCR's answer and the package authority
/// to grant.
pub(crate) fn require_prepared_read(
    environment: &sill::Environment,
    cwd: &Path,
) -> Result<(), WindowsHostedError> {
    let repository = windows_execution::hosted::client::require_public_attestation_repository()?;
    let reference = prepared_repository(&repository);
    let package = reference
        .strip_prefix(&format!("{REGISTRY}/"))
        .expect("the prepared repository lives on GHCR");
    let token = command_text(
        "GitHub registry token lookup",
        PREPARED_READ_RUNTIME,
        environment,
        cwd,
        "gh",
        &["auth", "token"],
    )
    .map_err(WindowsHostedError::ProviderResponse)?;
    let (status, body) = get(
        &format!("https://{REGISTRY}/token?scope=repository:{package}:pull&service={REGISTRY}"),
        &format!(
            "user = \"{}\"",
            quoted(&format!("{GHCR_USERNAME}:{}", token.trim()))
        ),
        environment,
        cwd,
    )?;
    read_verdict(&status, &body).map_err(|answer| denied(&reference, &answer))?;
    let pull: RegistryToken = serde_json::from_str(&body).map_err(|error| {
        denied(
            &reference,
            &format!("token endpoint returned no pull token: {error}"),
        )
    })?;
    let (status, body) = get(
        &format!("https://{REGISTRY}/v2/{package}/tags/list?n=1"),
        &format!("header = \"Authorization: Bearer {}\"", quoted(&pull.token)),
        environment,
        cwd,
    )?;
    read_verdict(&status, &body).map_err(|answer| denied(&reference, &answer))
}

/// GHCR's answer to one read: only success admits; any other status refuses
/// with the registry's own error codes.
fn read_verdict(status: &str, body: &str) -> Result<(), String> {
    if status == HTTP_OK {
        return Ok(());
    }
    let detail = serde_json::from_str::<RegistryErrors>(body).map_or_else(
        |_| body.trim().to_owned(),
        |answer| {
            answer
                .errors
                .iter()
                .map(|error| format!("{} {}", error.code, error.message))
                .collect::<Vec<_>>()
                .join("; ")
        },
    );
    Err(format!("HTTP {status} {detail}"))
}

fn denied(reference: &str, answer: &str) -> WindowsHostedError {
    WindowsHostedError::ProviderResponse(format!(
        "the selected gh credential cannot read the prepared package {reference}, so no hosted stage was dispatched: GHCR answered {}. Grant package read before releasing: a classic token needs read:packages (gh auth refresh -h github.com -s read:packages); a fine-grained or app credential needs read access to that package.",
        answer.trim_end_matches('.')
    ))
}

/// One curl config string value, escaped for its double quotes.
fn quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// GET `url` with curl, passing `config` (which carries the credential) on
/// standard input rather than the command line. Returns the HTTP status and
/// body.
fn get(
    url: &str,
    config: &str,
    environment: &sill::Environment,
    cwd: &Path,
) -> Result<(String, String), WindowsHostedError> {
    let output = command::run(
        "GHCR prepared package read",
        PREPARED_READ_RUNTIME,
        "curl",
        [
            "--silent",
            "--show-error",
            "--config",
            "-",
            "--write-out",
            "\n%{http_code}",
            url,
        ],
        environment,
        cwd,
        Some(config.as_bytes()),
    )
    .map_err(WindowsHostedError::ProviderResponse)?;
    let text = output
        .stdout_text("GHCR prepared package read")
        .map_err(WindowsHostedError::ProviderResponse)?;
    let (body, status) = text.rsplit_once('\n').unwrap_or(("", &text));
    Ok((status.trim().to_owned(), body.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::read_verdict;

    #[test]
    fn only_a_successful_registry_read_admits() {
        assert!(read_verdict("200", "{\"tags\":[]}").is_ok());
        let denied = read_verdict(
            "403",
            r#"{"errors":[{"code":"DENIED","message":"permission_denied: The token provided does not match expected scopes."}]}"#,
        )
        .unwrap_err();
        assert!(denied.contains("403") && denied.contains("DENIED"));
        assert!(read_verdict("401", "").is_err());
        assert!(read_verdict("404", "not json").is_err());
    }
}
