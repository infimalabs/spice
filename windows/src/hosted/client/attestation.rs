use std::ffi::OsString;
use std::path::Path;

use super::{HostedRun, WindowsHostedError, attach, command, source};

const BUILD_PROVENANCE_PREDICATE: &str = "https%3A%2F%2Fslsa.dev%2Fprovenance%2Fv1";

/// Require the provider to expose a build-provenance attestation before
/// cryptographic verification. A 404 is the one propagation state callers may
/// resume; once a bundle exists, verification failure is a terminal refusal.
pub(super) fn verify_attestation(
    run: &HostedRun<'_>,
    workflow: &str,
    run_id: u64,
    path: &Path,
) -> Result<String, WindowsHostedError> {
    let workflow_commit = attach::workflow_commit(run, workflow, run_id)?;
    require_attestation_bundle(run, path)?;
    let verification = command(
        run.root,
        "gh",
        [
            OsString::from("attestation"),
            OsString::from("verify"),
            path.as_os_str().to_owned(),
            OsString::from("--repo"),
            OsString::from(run.repository),
            OsString::from("--signer-workflow"),
            OsString::from(format!("{}/{workflow}", run.repository)),
            OsString::from("--signer-digest"),
            OsString::from(&workflow_commit),
            OsString::from("--source-digest"),
            OsString::from(&workflow_commit),
            OsString::from("--deny-self-hosted-runners"),
            OsString::from("--format=json"),
        ],
    )?;
    if verification.status.success() {
        return Ok(workflow_commit);
    }
    Err(WindowsHostedError::AttestationRefused {
        path: path.to_path_buf(),
        code: verification.status.code(),
        stdout: String::from_utf8_lossy(&verification.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&verification.stderr).into_owned(),
    })
}

fn require_attestation_bundle(run: &HostedRun<'_>, path: &Path) -> Result<(), WindowsHostedError> {
    let digest = source::file_sha256(path)?;
    let endpoint = format!(
        "repos/{}/attestations/sha256:{digest}?per_page=1&predicate_type={BUILD_PROVENANCE_PREDICATE}",
        run.repository
    );
    let lookup = command(
        run.root,
        "gh",
        [
            OsString::from("api"),
            OsString::from("--include"),
            OsString::from(endpoint),
        ],
    )?;
    if lookup.status.success() {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&lookup.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&lookup.stderr).into_owned();
    if provider_http_status(&stdout) == Some(404) {
        return Err(WindowsHostedError::AttestationPending {
            path: path.to_path_buf(),
        });
    }
    Err(WindowsHostedError::Command {
        operation: "artifact attestation lookup",
        code: lookup.status.code(),
        stdout,
        stderr,
    })
}

fn provider_http_status(response: &str) -> Option<u16> {
    let mut fields = response.lines().next()?.split_ascii_whitespace();
    if !fields.next()?.starts_with("HTTP/") {
        return None;
    }
    fields.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::provider_http_status;

    #[test]
    fn provider_attestation_lookup_reads_the_exact_http_status() {
        assert_eq!(
            provider_http_status("HTTP/2.0 404 Not Found\nContent-Type: application/json\n"),
            Some(404)
        );
        assert_eq!(
            provider_http_status("HTTP/2.0 200 OK\nContent-Type: application/json\n"),
            Some(200)
        );
        assert_eq!(provider_http_status("gh: network unavailable"), None);
    }
}
