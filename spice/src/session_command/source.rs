use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use wire::Refusal;

use super::Environment;

const THREAD_HEX_LEN: usize = 32;

/// One explicit transcript file admitted as forensic evidence.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedSource {
    pub(crate) path: PathBuf,
}

/// Admits each named transcript file in caller order. Inputs are paths only:
/// a transcript Spice captured is read from its tape, and a foreign transcript
/// is named by its file, so no input is ever resolved by searching vendor
/// storage.
pub(crate) fn resolve_inputs(
    values: &[String],
    environment: &Environment,
) -> Result<Vec<ResolvedSource>, Refusal> {
    let mut resolved = Vec::new();
    for value in values {
        resolved.push(resolve_input(value, environment)?);
    }
    deduplicate(resolved)
}

fn resolve_input(value: &str, environment: &Environment) -> Result<ResolvedSource, Refusal> {
    let expanded = expand_home(value, environment);
    if expanded.exists() {
        return admit(expanded);
    }
    Err(refusal(
        "session::transcript-missing",
        Some(expanded.display().to_string()),
        "pass an existing .jsonl or .jsonl.gz path; live briefing, sweep, turns, and commands \
         read repository capture",
    ))
}

fn admit(path: PathBuf) -> Result<ResolvedSource, Refusal> {
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        refusal(
            "session::transcript-unavailable",
            Some(format!("{}: {error}", path.display())),
            "restore a readable regular transcript file and retry",
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(refusal(
            "session::transcript-not-regular",
            Some(path.display().to_string()),
            "pass a real regular .jsonl or .jsonl.gz file, not a link, directory, or device",
        ));
    }
    if !transcript_extension(&path) {
        return Err(refusal(
            "session::transcript-extension",
            Some(path.display().to_string()),
            "pass a transcript ending in .jsonl or .jsonl.gz",
        ));
    }
    let path = path.canonicalize().map_err(|error| {
        refusal(
            "session::transcript-unavailable",
            Some(format!("{}: {error}", path.display())),
            "restore stable read access to the transcript and retry",
        )
    })?;
    Ok(ResolvedSource { path })
}

fn deduplicate(values: Vec<ResolvedSource>) -> Result<Vec<ResolvedSource>, Refusal> {
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    for value in values {
        if seen.insert(value.path.clone()) {
            rows.push(value);
        }
    }
    if rows.is_empty() {
        return Err(refusal(
            "session::transcript-unspecified",
            None,
            "pass at least one distinct transcript path",
        ));
    }
    Ok(rows)
}

fn expand_home(value: &str, environment: &Environment) -> PathBuf {
    let Some(rest) = value.strip_prefix("~/") else {
        return PathBuf::from(value);
    };
    environment
        .home
        .as_ref()
        .map_or_else(|| PathBuf::from(value), |home| home.join(rest))
}

fn transcript_extension(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.ends_with(".jsonl") || text.ends_with(".jsonl.gz")
}

pub(crate) fn canonical_thread(value: &str) -> Option<String> {
    let compact = value.trim().replace('-', "").to_ascii_lowercase();
    if compact.len() != THREAD_HEX_LEN || !compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    uuid::Uuid::parse_str(&compact)
        .ok()
        .map(wire::CanonicalUuid::new)
        .map(|identity| identity.to_string())
}

fn refusal(reason: &str, observed: Option<String>, detail: &str) -> Refusal {
    Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("session-transcript".to_owned()),
            Some("immutable transcript evidence readable as JSONL".to_owned()),
            observed,
        )
        .with_detail(detail),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{Environment, canonical_thread, resolve_inputs};
    use crate::session_command::VendorEnvironment;

    const THREAD: &str = "0123456789abcdef0123456789abcdef";
    const THREAD_DASHED: &str = "01234567-89ab-cdef-0123-456789abcdef";
    const THREAD_UPPER: &str = "01234567-89AB-CDEF-0123-456789ABCDEF";

    #[test]
    fn dashed_and_compact_thread_inputs_share_one_canonical_presentation() {
        let expected = Some(THREAD_DASHED);
        assert_eq!(canonical_thread(THREAD).as_deref(), expected);
        assert_eq!(canonical_thread(THREAD_DASHED).as_deref(), expected);
        assert_eq!(canonical_thread(THREAD_UPPER).as_deref(), expected);
    }

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove session source fixture");
        }
    }

    #[test]
    fn only_an_existing_named_file_is_admitted() {
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "spice-session-source-{}-explicit-only",
            std::process::id()
        )));
        let sessions = scratch.0.join("sessions/-fixture");
        fs::create_dir_all(&sessions).expect("provider session directory");
        fs::write(scratch.0.join("nearby.jsonl"), b"\n").expect("nearby transcript");
        let filed = sessions.join(format!("fixture_{THREAD_DASHED}.jsonl"));
        fs::write(&filed, b"\n").expect("provider-filed transcript");
        let environment = Environment {
            vendors: vec![VendorEnvironment {
                vendor: &spice::extension::OMP,
                identity: Some(THREAD_DASHED.into()),
            }],
            preferred_vendor: None,
            home: Some(scratch.0.clone()),
            cwd: Some(scratch.0.clone()),
            checkout: crate::session_command::glance::unread,
            ambient: spice::ambient::Ambient::default(),
        };

        let unnamed = resolve_inputs(&[], &environment).expect_err("no subject is named");
        assert_eq!(unnamed.reason(), "session::transcript-unspecified");
        for thread in [THREAD, THREAD_DASHED] {
            let named = resolve_inputs(&[thread.to_owned()], &environment)
                .expect_err("a thread identity is not a transcript path");
            assert_eq!(named.reason(), "session::transcript-missing");
        }
        let explicit = filed.to_string_lossy().into_owned();
        let resolved = resolve_inputs(&[explicit.clone(), explicit], &environment)
            .expect("an explicit transcript file is admitted once");
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            resolved[0].path,
            filed.canonicalize().expect("fixture path")
        );
    }
}
