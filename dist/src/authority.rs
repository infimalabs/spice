//! One tracked Git authority for external Spice releases.

use std::fs;
use std::path::{Path, PathBuf};

use wire::manifest::ProductVersion;

use crate::version::TrackedVersionError;

const REMOTE: &str = "origin";
const SOURCE_REF: &str = "refs/heads/main";
const PACKAGE_REF: &str = "refs/spice/packages/vVERSION";
const TAG_REF: &str = "refs/tags/vVERSION";
const ANNOUNCEMENT_REF: &str = "refs/spice/releases/vVERSION";
const VERSION_SLOT: &str = "VERSION";
const ASCII_DELETE: u8 = 0x7f;

/// Expanded immutable refs for one release version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseRefs {
    source: String,
    package: String,
    tag: String,
    announcement: String,
}

impl ReleaseRefs {
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    #[must_use]
    pub fn tag(&self) -> &str {
        &self.tag
    }

    #[must_use]
    pub fn announcement(&self) -> &str {
        &self.announcement
    }
}

/// The sole admitted external release authority: the one fixed Git topology.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseAuthority(());

impl ReleaseAuthority {
    #[must_use]
    pub fn remote(&self) -> &str {
        REMOTE
    }

    #[must_use]
    pub fn refs(&self, version: &ProductVersion) -> ReleaseRefs {
        ReleaseRefs {
            source: SOURCE_REF.to_owned(),
            package: expand(PACKAGE_REF, version),
            tag: expand(TAG_REF, version),
            announcement: expand(ANNOUNCEMENT_REF, version),
        }
    }

    /// Rehearse the exact guarded package publication without changing refs.
    ///
    /// # Errors
    ///
    /// Refuses a noncanonical Git object spelling.
    pub fn package_rehearsal(
        &self,
        version: &ProductVersion,
        object: &str,
    ) -> Result<GitCommand, AuthorityError> {
        object_id(object)?;
        let reference = self.refs(version).package;
        Ok(push_command(REMOTE, object, &reference, Some(None), true))
    }

    /// Observe the configured remote through the authenticated Git transport.
    #[must_use]
    pub fn remote_visibility(&self) -> GitCommand {
        visibility_command(REMOTE, None)
    }

    /// Observe one exact ref through the authenticated Git transport.
    ///
    /// # Errors
    ///
    /// Refuses a noncanonical Git reference spelling.
    pub fn reference_visibility(&self, reference: &str) -> Result<GitCommand, AuthorityError> {
        validate_ref(reference)?;
        Ok(visibility_command(REMOTE, Some(reference)))
    }

    /// Converge the package ref without overwriting another release object.
    ///
    /// # Errors
    ///
    /// Refuses a noncanonical object or an observed different object.
    pub fn package_push(
        &self,
        version: &ProductVersion,
        object: &str,
        state: RefState<'_>,
    ) -> Result<GitMutation, AuthorityError> {
        self.converge(self.refs(version).package, object, state)
    }

    /// Converge the immutable version tag.
    ///
    /// # Errors
    ///
    /// Refuses a noncanonical object or an observed different object.
    pub fn tag_push(
        &self,
        version: &ProductVersion,
        object: &str,
        state: RefState<'_>,
    ) -> Result<GitMutation, AuthorityError> {
        self.converge(self.refs(version).tag, object, state)
    }

    /// Converge the release announcement ref.
    ///
    /// # Errors
    ///
    /// Refuses a noncanonical object or an observed different object.
    pub fn announcement_push(
        &self,
        version: &ProductVersion,
        object: &str,
        state: RefState<'_>,
    ) -> Result<GitMutation, AuthorityError> {
        self.converge(self.refs(version).announcement, object, state)
    }

    fn converge(
        &self,
        reference: String,
        object: &str,
        state: RefState<'_>,
    ) -> Result<GitMutation, AuthorityError> {
        object_id(object)?;
        match state {
            RefState::Absent => Ok(GitMutation::Command(push_command(
                REMOTE,
                object,
                &reference,
                Some(None),
                false,
            ))),
            RefState::Present(observed) => {
                object_id(observed)?;
                if observed == object {
                    Ok(GitMutation::Converged)
                } else {
                    Err(AuthorityError::RefMoved {
                        reference,
                        expected: object.to_owned(),
                        observed: observed.to_owned(),
                    })
                }
            }
        }
    }
}

/// Observed state of one immutable release ref.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefState<'a> {
    Absent,
    Present(&'a str),
}

/// Whether one release mutation already converged or needs one command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitMutation {
    Converged,
    Command(GitCommand),
}

/// One exact Git invocation constructed by the release authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitCommand {
    arguments: Vec<String>,
}

impl GitCommand {
    fn new(arguments: impl IntoIterator<Item = String>) -> Self {
        Self {
            arguments: arguments.into_iter().collect(),
        }
    }

    #[must_use]
    pub const fn program(&self) -> &'static str {
        "git"
    }

    #[must_use]
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }
}

/// Admit the fixed release authority for this workspace.
///
/// # Errors
///
/// Refuses when any workspace crate could be published without stating so.
pub fn current_authority(workspace: &Path) -> Result<ReleaseAuthority, AuthorityError> {
    admit_workspace_publish_policy(workspace)?;
    Ok(ReleaseAuthority(()))
}

/// Prove no workspace package can be sent to a Cargo registry unless its own
/// manifest says so.
///
/// # Errors
///
/// Refuses an unreadable or malformed manifest, an externally publishable
/// workspace default, or any member whose manifest states no `publish`, so
/// that a crate published by default rather than by its own declaration is
/// refused.
fn admit_workspace_publish_policy(root: &Path) -> Result<(), AuthorityError> {
    let root_manifest = read_manifest(&root.join("Cargo.toml"))?;
    let workspace = root_manifest
        .get("workspace")
        .and_then(toml::Value::as_table)
        .ok_or(AuthorityError::WorkspacePublish)?;
    let package = workspace
        .get("package")
        .and_then(toml::Value::as_table)
        .ok_or(AuthorityError::WorkspacePublish)?;
    if package.get("publish").and_then(toml::Value::as_bool) != Some(false) {
        return Err(AuthorityError::WorkspacePublish);
    }
    let members = workspace
        .get("members")
        .and_then(toml::Value::as_array)
        .ok_or(AuthorityError::WorkspacePublish)?;
    for member in members {
        let member = member.as_str().ok_or(AuthorityError::WorkspacePublish)?;
        let path = root.join(member).join("Cargo.toml");
        let manifest = read_manifest(&path)?;
        admit_member_publish_policy(&path, &manifest)?;
    }
    Ok(())
}

fn admit_member_publish_policy(path: &Path, manifest: &toml::Value) -> Result<(), AuthorityError> {
    let stated = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .is_some_and(|package| package.contains_key("publish"));
    if stated {
        Ok(())
    } else {
        Err(AuthorityError::MemberPublish(path.to_path_buf()))
    }
}

fn read_manifest(path: &Path) -> Result<toml::Value, AuthorityError> {
    let bytes = fs::read(path).map_err(|source| AuthorityError::ManifestRead {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_slice(&bytes).map_err(|source| AuthorityError::ManifestDecode {
        path: path.to_path_buf(),
        detail: source.to_string(),
    })
}

pub(crate) fn tracked_text_line<'a>(
    text: &'a str,
    label: &'static str,
) -> Result<&'a str, AuthorityError> {
    let line = text
        .strip_suffix('\n')
        .ok_or(AuthorityError::TrackedLine(label))?;
    if line.contains('\n') || line.contains('\r') {
        Err(AuthorityError::TrackedLine(label))
    } else {
        Ok(line)
    }
}

fn expand(template: &str, version: &ProductVersion) -> String {
    template.replace(VERSION_SLOT, version.as_str())
}

fn validate_ref(reference: &str) -> Result<(), AuthorityError> {
    let invalid = !reference.starts_with("refs/")
        || reference.ends_with(['/', '.'])
        || reference.contains("..")
        || reference.contains("@{")
        || reference
            .split('/')
            .any(|part| part.is_empty() || part.ends_with(".lock"))
        || reference.bytes().any(|byte| {
            byte <= b' '
                || byte == ASCII_DELETE
                || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        });
    if invalid {
        Err(AuthorityError::Reference(reference.to_owned()))
    } else {
        Ok(())
    }
}

fn object_id(object: &str) -> Result<(), AuthorityError> {
    let canonical = matches!(object.len(), 40 | 64)
        && object
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if canonical {
        Ok(())
    } else {
        Err(AuthorityError::Object(object.to_owned()))
    }
}

fn push_command(
    remote: &str,
    object: &str,
    reference: &str,
    lease: Option<Option<&str>>,
    dry_run: bool,
) -> GitCommand {
    let mut arguments = vec!["push".to_owned()];
    if dry_run {
        arguments.push("--dry-run".to_owned());
    }
    arguments.push("--porcelain".to_owned());
    if let Some(expected) = lease {
        arguments.push(format!(
            "--force-with-lease={reference}:{}",
            expected.unwrap_or_default()
        ));
    }
    arguments.push(remote.to_owned());
    arguments.push(format!("{object}:{reference}"));
    GitCommand::new(arguments)
}

fn visibility_command(remote: &str, reference: Option<&str>) -> GitCommand {
    let mut arguments = vec![
        "ls-remote".to_owned(),
        "--refs".to_owned(),
        remote.to_owned(),
    ];
    if let Some(reference) = reference {
        arguments.push(reference.to_owned());
    }
    GitCommand::new(arguments)
}

/// Why the external release authority was refused.
#[derive(Debug, thiserror::Error)]
pub enum AuthorityError {
    #[error("tracked {0} must be exactly one LF-terminated line")]
    TrackedLine(&'static str),
    #[error(transparent)]
    Version(#[from] TrackedVersionError),
    #[error("release ref is not canonical: {0}")]
    Reference(String),
    #[error("Git object is not one canonical lowercase object id: {0}")]
    Object(String),
    #[error("release ref {reference} already names {observed}, not {expected}")]
    RefMoved {
        reference: String,
        expected: String,
        observed: String,
    },
    #[error("workspace package policy is not publish=false")]
    WorkspacePublish,
    #[error("workspace member states no publish policy: {0}")]
    MemberPublish(PathBuf),
    #[error("cannot read Cargo manifest {path}: {source}")]
    ManifestRead {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot decode Cargo manifest {path}: {detail}")]
    ManifestDecode { path: PathBuf, detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    const OBJECT: &str = "1111111111111111111111111111111111111111";

    fn workspace() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("dist is one workspace member")
    }

    #[test]
    fn tracked_text_is_exactly_one_lf_terminated_line() {
        assert_eq!(tracked_text_line("1\n", "version").ok(), Some("1"));
        for refused in ["1", "1\n\n", "1\r\n"] {
            assert!(
                tracked_text_line(refused, "version").is_err(),
                "{refused:?}"
            );
        }
    }

    #[test]
    fn versions_and_ref_templates_have_one_canonical_spelling() {
        for accepted in ["0.0.0", "1.2.3", "1.2.3-rc.1", "1.2.3+build.01"] {
            ProductVersion::parse(accepted).expect("canonical semantic version");
        }
        for refused in ["", "v1.2.3", "01.2.3", "1.2", "1.2.3-01", "1.2.3+"] {
            assert!(ProductVersion::parse(refused).is_err(), "{refused}");
        }

        let authority = current_authority(workspace()).expect("tracked authority");
        let version = ProductVersion::parse("1.2.3").expect("version");
        let refs = authority.refs(&version);
        assert_eq!(
            (
                authority.remote(),
                refs.source(),
                refs.package(),
                refs.tag(),
                refs.announcement(),
            ),
            (
                "origin",
                "refs/heads/main",
                "refs/spice/packages/v1.2.3",
                "refs/tags/v1.2.3",
                "refs/spice/releases/v1.2.3",
            )
        );
    }

    #[test]
    fn rehearsal_visibility_and_convergence_construct_exact_git_operations() {
        let authority = current_authority(workspace()).expect("tracked authority");
        let version = ProductVersion::parse("1.2.3").expect("version");
        assert_eq!(
            authority
                .package_rehearsal(&version, OBJECT)
                .expect("rehearsal")
                .arguments(),
            [
                "push",
                "--dry-run",
                "--porcelain",
                "--force-with-lease=refs/spice/packages/v1.2.3:",
                "origin",
                "1111111111111111111111111111111111111111:refs/spice/packages/v1.2.3",
            ]
        );
        assert_eq!(
            authority.remote_visibility().arguments(),
            ["ls-remote", "--refs", "origin"]
        );
        assert_eq!(authority.remote_visibility().program(), "git");
        assert_eq!(
            authority
                .reference_visibility("refs/tags/v1.2.3")
                .expect("reference visibility")
                .arguments(),
            ["ls-remote", "--refs", "origin", "refs/tags/v1.2.3"]
        );
        assert_eq!(
            authority
                .package_push(&version, OBJECT, RefState::Present(OBJECT))
                .expect("matching ref converges"),
            GitMutation::Converged
        );
        let error = authority
            .tag_push(
                &version,
                OBJECT,
                RefState::Present("2222222222222222222222222222222222222222"),
            )
            .expect_err("mismatched immutable tag refuses");
        assert!(matches!(error, AuthorityError::RefMoved { .. }));

        let GitMutation::Command(announcement) = authority
            .announcement_push(&version, OBJECT, RefState::Absent)
            .expect("absent announcement can converge")
        else {
            panic!("absent announcement must require one command");
        };
        assert!(
            announcement
                .arguments()
                .iter()
                .any(|argument| argument.ends_with(":refs/spice/releases/v1.2.3"))
        );
        assert!(matches!(
            authority.package_rehearsal(&version, "111111"),
            Err(AuthorityError::Object(_))
        ));
    }

    #[test]
    fn ref_grammar_refuses_noncanonical_spellings() {
        for reference in [
            "main",
            "refs/heads/release candidate",
            "refs/heads/release..candidate",
            "refs/heads//release",
            "refs/heads/release~candidate",
            "refs/heads/candidate.lock",
        ] {
            assert!(validate_ref(reference).is_err(), "{reference}");
        }
    }

    #[test]
    fn only_a_member_stating_its_publish_policy_admits() {
        let manifest = |package: &str| -> toml::Value {
            toml::from_str(&format!(
                "[package]\nname = \"member\"\nversion = \"1.0.0\"\n{package}"
            ))
            .expect("fixture manifest")
        };
        let path = Path::new("member/Cargo.toml");
        assert!(matches!(
            admit_member_publish_policy(path, &manifest("")),
            Err(AuthorityError::MemberPublish(refused)) if refused == path
        ));
        for stated in [
            "publish.workspace = true",
            "publish = true",
            "publish = false",
        ] {
            assert!(
                admit_member_publish_policy(path, &manifest(stated)).is_ok(),
                "{stated}"
            );
        }
    }
}
