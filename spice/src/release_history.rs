//! Git bounds and durable task authority for release-note folding.

use std::collections::BTreeSet;
use std::error::Error as StdError;
use std::fmt;
use std::path::Path;
use std::process::Command;

use dist::{Correction, Landing, NotesInput, ReviewStanding};
use wire::ReleaseNotes;

const RECENT_LANDING_LIMIT: usize = 5;

/// Resolve and fold release notes at one repository commit.
pub(crate) fn draft(
    root: &Path,
    reader: &journal::JournalReader,
    revision: Option<&str>,
    requested_version: Option<&str>,
) -> Result<ReleaseNotes, Error> {
    draft_with_release(root, reader, revision, requested_version, None)
}

pub(crate) fn projected_draft(
    root: &Path,
    reader: &journal::JournalReader,
    history_commit: &str,
    requested_version: Option<&str>,
    release_commit: &str,
) -> Result<ReleaseNotes, Error> {
    draft_with_release(
        root,
        reader,
        Some(history_commit),
        requested_version,
        Some(release_commit),
    )
}

fn draft_with_release(
    root: &Path,
    reader: &journal::JournalReader,
    revision: Option<&str>,
    requested_version: Option<&str>,
    projected_release: Option<&str>,
) -> Result<ReleaseNotes, Error> {
    let history_commit = resolve_commit(root, revision.unwrap_or("HEAD"))?;
    let release_commit = projected_release.unwrap_or(&history_commit).to_owned();
    let release_version = match requested_version {
        Some(version) => version.to_owned(),
        None => spice::version::source_version(root, dist::Build::Staged)
            .map_err(Error::Encoding)?
            .to_string(),
    };
    let current_tag = format!("v{release_version}");
    let previous_tag = reachable_tags(root, &history_commit)?
        .into_iter()
        .find(|tag| tag != &current_tag);
    let mut window = first_parent_commits(root, previous_tag.as_deref(), &history_commit, true)?;
    if previous_tag.is_none() {
        exclude_parentless_root(root, &mut window)?;
    }
    let ancestry = first_parent_commits(root, None, &history_commit, false)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let authority = reader
        .read(|read| read.release_note_authority())
        .map_err(|error| Error::Journal(error.to_string()))?;
    let landings = authority
        .landings
        .into_iter()
        .filter(|landing| ancestry.contains(&landing.commit))
        .map(|landing| Landing {
            sequence: landing.seq.get(),
            commit: landing.commit,
            task: landing.task.as_str().to_owned(),
            topic: landing.topic.to_string(),
            title: landing.title,
            contributes: landing.contributes,
            verdict: landing.verdict.map(|verdict| match verdict {
                wire::body::Verdict::Clean => ReviewStanding::Clean,
                wire::body::Verdict::Finding => ReviewStanding::Finding,
            }),
        })
        .collect::<Vec<_>>();
    let relevant_tasks = landings
        .iter()
        .map(|landing| landing.task.as_str())
        .collect::<BTreeSet<_>>();
    let corrections = authority
        .corrections
        .into_iter()
        .filter(|correction| relevant_tasks.contains(correction.correction.as_str()))
        .map(|correction| Correction {
            parent: correction.parent.as_str().to_owned(),
            correction: correction.correction.as_str().to_owned(),
        })
        .collect();
    dist::release_notes(NotesInput {
        release_version,
        release_commit,
        previous_tag,
        window,
        landings,
        corrections,
    })
    .map_err(Error::Fold)
}

fn resolve_commit(root: &Path, revision: &str) -> Result<String, Error> {
    read(
        root,
        &[
            "rev-parse".to_owned(),
            "--verify".to_owned(),
            format!("{revision}^{{commit}}"),
        ],
        "resolve the release commit",
    )
}

fn reachable_tags(root: &Path, release_commit: &str) -> Result<Vec<String>, Error> {
    let raw = read(
        root,
        &[
            "tag".to_owned(),
            "--merged".to_owned(),
            release_commit.to_owned(),
            "--list".to_owned(),
            "v*".to_owned(),
            "--sort=-v:refname".to_owned(),
        ],
        "resolve reachable release tags",
    )?;
    Ok(nonempty_lines(&raw))
}

fn first_parent_commits(
    root: &Path,
    previous_tag: Option<&str>,
    release_commit: &str,
    bounded_without_tag: bool,
) -> Result<Vec<String>, Error> {
    let mut arguments = vec![
        "log".to_owned(),
        "--first-parent".to_owned(),
        "--reverse".to_owned(),
        "--format=%H".to_owned(),
    ];
    arguments.push(previous_tag.map_or_else(
        || release_commit.to_owned(),
        |tag| format!("refs/tags/{tag}..{release_commit}"),
    ));
    let raw = read(root, &arguments, "read first-parent release bounds")?;
    let commits = nonempty_lines(&raw);
    Ok(if bounded_without_tag && previous_tag.is_none() {
        bound_recent(commits)
    } else {
        commits
    })
}

fn nonempty_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn bound_recent(mut commits: Vec<String>) -> Vec<String> {
    let first = commits.len().saturating_sub(RECENT_LANDING_LIMIT);
    drop(commits.drain(..first));
    commits
}

fn exclude_parentless_root(root: &Path, commits: &mut Vec<String>) -> Result<(), Error> {
    let Some(first) = commits.first() else {
        return Ok(());
    };
    let object = read(
        root,
        &["cat-file".to_owned(), "-p".to_owned(), first.to_owned()],
        "read the first release boundary",
    )?;
    if !commit_object_has_parent(&object) {
        commits.remove(0);
    }
    Ok(())
}

fn commit_object_has_parent(object: &str) -> bool {
    object
        .lines()
        .take_while(|line| !line.is_empty())
        .any(|line| line.starts_with("parent "))
}

fn read(root: &Path, arguments: &[String], action: &str) -> Result<String, Error> {
    let output = run(root, arguments)?;
    if output.status == 0 {
        Ok(output.stdout.trim_end_matches('\n').to_owned())
    } else {
        Err(Error::Command(output.detail(action)))
    }
}

fn run(root: &Path, arguments: &[String]) -> Result<GitOutput, Error> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|error| Error::Launch(error.to_string()))?;
    let status = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| Error::Encoding(format!("Git stdout is not UTF-8: {error}")))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| Error::Encoding(format!("Git stderr is not UTF-8: {error}")))?;
    Ok(GitOutput {
        status,
        stdout,
        stderr,
    })
}

struct GitOutput {
    status: i32,
    stdout: String,
    stderr: String,
}

impl GitOutput {
    fn detail(&self, action: &str) -> String {
        let detail = [&self.stdout, &self.stderr]
            .into_iter()
            .map(|part| part.trim())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if detail.is_empty() {
            format!("could not {action} (git exit {})", self.status)
        } else {
            format!("could not {action} (git exit {})\n{detail}", self.status)
        }
    }
}

#[derive(Debug)]
pub(crate) enum Error {
    Launch(String),
    Encoding(String),
    Command(String),
    Journal(String),
    Fold(dist::NotesError),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Launch(detail) => write!(formatter, "could not launch Git: {detail}"),
            Self::Encoding(detail) | Self::Command(detail) | Self::Journal(detail) => {
                formatter.write_str(detail)
            }
            Self::Fold(error) => error.fmt(formatter),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Fold(error) => Some(error),
            Self::Launch(_) | Self::Encoding(_) | Self::Command(_) | Self::Journal(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{bound_recent, commit_object_has_parent};

    #[test]
    fn an_untagged_window_keeps_the_latest_bound_in_first_parent_order() {
        let commits = (1..=7).map(|index| format!("commit-{index}")).collect();

        assert_eq!(
            bound_recent(commits),
            ["commit-3", "commit-4", "commit-5", "commit-6", "commit-7"]
        );
    }

    #[test]
    fn only_a_parent_in_the_commit_header_prevents_the_initial_boundary() {
        assert!(!commit_object_has_parent(
            "tree abc\nauthor A\ncommitter A\n\nparent text in the message\n"
        ));
        assert!(commit_object_has_parent(
            "tree abc\nparent def\nauthor A\ncommitter A\n\nmessage\n"
        ));
    }
}
