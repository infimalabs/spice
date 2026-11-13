//! GIT-08, the landing footprint: a task lands only what it wrote.
//!
//! A task lands as a merge whose first parent is the baseline it integrated
//! against. Every path that merge changes against that baseline has to come
//! from one of the task's own commits. Anything else was already on the shared
//! branch before the task started, which makes it a peer's, and overwriting it
//! here is silent: the landing carries no conflict, no warning, and nothing
//! recording that the replaced content was ever there.
//!
//! A machine merge cannot drift, because merging the baseline with the lane
//! changes only what the lane changed. Hand resolution can, and this port
//! reaches hand resolution by design -- [`super::materialize_conflict`] hands a
//! conflicted tree to the agent and takes back whatever it commits. That
//! returning tree is the one landing nothing else inspects, which is what this
//! boundary is for.
//!
//! Allocator-owned resolution does not make this boundary redundant. It proves
//! that the active claimant owns the handoff, seals the merge controls and
//! resolved index around source evidence, and refuses if either moves. Source
//! evidence proves that the sealed tree is valid; it cannot prove that every
//! valid path in that tree was authored for this task. An unrelated baseline
//! path can be rewritten to other valid content without becoming a conflict or
//! failing any source gate. Only the first-parent landing diff compared with
//! non-merge task commits retains that authorship distinction.
//!
//! The refusal names every drifted path and classifies it, because the two
//! classes need opposite recoveries: a path the baseline still holds is put
//! back by restoring its content, and a path the baseline never had is put back
//! by removing it. A recipe that offered only one of those would be wrong half
//! the time.
//!
//! A resolution can need to adapt a baseline path no task commit names, such
//! as a caller of a function the task changed, and Git admits no non-merge
//! commit while the merge stays open. So a drifted resolution still lands
//! nothing, but it does commit the integration to the lane: the resolved tree
//! with every drifted path held as the baseline has it, unpublished, with the
//! resolved content of those paths left staged. A non-merge commit then owns
//! that content inside the footprint, or a restore from `HEAD` drops it.

use std::collections::BTreeSet;
use std::fmt;

use super::{Error, Worktree, required_read, run};
use crate::shell_quote;

/// Where a drifted path stood at the baseline, which decides how it is put back.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Drift {
    /// The baseline holds this path, so the landing rewrote a peer's content.
    RewritesBaseline,
    /// The baseline never held it, so the landing invented it outside the task.
    AddsToBaseline,
}

impl fmt::Display for Drift {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RewritesBaseline => formatter.write_str("rewrites-baseline"),
            Self::AddsToBaseline => formatter.write_str("adds-to-baseline"),
        }
    }
}

/// Refuse a landing whose first-parent diff leaves the task's own footprint.
///
/// `landing` is the merge or tree being judged and `agent_head` the lane work
/// it claims to carry; both are compared against `upstream_head`, the baseline
/// the landing takes as its first parent.
pub(super) fn require_landing_within_footprint(
    root: Worktree<'_>,
    label: &str,
    upstream_head: &str,
    landing: &str,
    agent_head: &str,
) -> Result<(), Error> {
    let drifted = drift(root, upstream_head, landing, agent_head)?;
    if drifted.is_empty() {
        return Ok(());
    }
    Err(drift_refusal(label, upstream_head, &drifted))
}

/// Every path `landing` changes against the baseline that no non-merge task
/// commit names, classified by where it stood at the baseline.
pub(super) fn drift(
    root: Worktree<'_>,
    upstream_head: &str,
    landing: &str,
    agent_head: &str,
) -> Result<Vec<(String, Drift)>, Error> {
    let landed = read_paths(
        root,
        &[
            "diff",
            "--no-renames",
            "--name-only",
            upstream_head,
            landing,
        ],
        "read the landing diff",
    )?;
    let footprint = footprint(&required_read(
        root,
        &[
            "log",
            "--no-renames",
            "--format=%x00%P",
            "--name-only",
            &format!("{upstream_head}..{agent_head}"),
        ],
        "read the task footprint",
    )?);
    let drifted = landed.difference(&footprint).cloned().collect::<Vec<_>>();
    classify(root, upstream_head, &drifted)
}

/// The paths of the task's own non-merge commits since the baseline.
///
/// `log` holds one record per commit, each opening with a NUL and its parent
/// list and followed by the paths that commit names. A merge's record never
/// counts, whatever paths it lists: an integration or resolution merge must not
/// authorize the landing measured against it, since the paths it touched are
/// exactly the set this boundary exists to question. A path a non-merge commit
/// names after such a merge counts like any other task commit's path.
fn footprint(log: &str) -> BTreeSet<String> {
    log.split('\0')
        .filter_map(|record| {
            let mut lines = record.lines();
            let parents = lines.next()?.split_whitespace().count();
            (parents <= 1).then_some(lines)
        })
        .flatten()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn read_paths(root: Worktree<'_>, args: &[&str], action: &str) -> Result<BTreeSet<String>, Error> {
    Ok(required_read(root, args, action)?
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

fn classify(
    root: Worktree<'_>,
    upstream_head: &str,
    drifted: &[String],
) -> Result<Vec<(String, Drift)>, Error> {
    drifted
        .iter()
        .map(|path| {
            let probe = run(
                root,
                &["cat-file", "-e", &format!("{upstream_head}:{path}")],
            )?;
            let drift = if probe.success() {
                Drift::RewritesBaseline
            } else {
                Drift::AddsToBaseline
            };
            Ok((path.clone(), drift))
        })
        .collect()
}

fn drift_refusal(label: &str, upstream_head: &str, classified: &[(String, Drift)]) -> Error {
    Error::refusal(
        "git::landing-out-of-scope",
        format!(
            "landing {label} changes baseline paths outside its own commits:{}\nthose \
             paths carry work already on the shared branch, and publishing would overwrite \
             it without recording that it was there",
            listing(classified),
        ),
        remedy(label, upstream_head, classified),
    )
}

/// Refuse a drifted resolution after its integration moved the lane unpublished.
///
/// `integration` is the resolution committed with every drifted path held as
/// the baseline has it. The lane holds it unpublished while the resolved
/// content of those paths stays staged, so either recovery is one ordinary step
/// on a lane that no longer carries an open merge.
pub(super) fn integration_refusal(
    label: &str,
    upstream_head: &str,
    integration: &str,
    classified: &[(String, Drift)],
) -> Error {
    Error::refusal(
        "git::landing-out-of-scope",
        format!(
            "the resolution of {label} changes baseline paths outside its own commits:{}\n\
             nothing was published: the lane holds baseline {upstream_head} integrated as \
             {integration}, which keeps those paths as the shared branch has them, and the \
             resolved content of each stays staged",
            listing(classified),
        ),
        integration_remedy(label, upstream_head, integration, classified),
    )
}

fn listing(classified: &[(String, Drift)]) -> String {
    classified
        .iter()
        .map(|(path, drift)| format!("\n  {drift} {path}"))
        .collect()
}

/// The recipe's pathspecs, quoted the way the predecessor's `shlex.join` does.
///
/// Only the recipe is quoted, never the listing above it. The listing is prose
/// naming what drifted, and the predecessor leaves it bare for the same reason;
/// the recipe is meant to be pasted, and `diff --name-only` prints a path
/// holding a space verbatim, so leaving it bare there would split one pathspec
/// into two and recover something nobody asked for.
fn quoted(classified: &[(String, Drift)], wanted: Drift) -> String {
    classified
        .iter()
        .filter(|(_, drift)| *drift == wanted)
        .map(|(path, _)| format!(" {}", shell_quote(path)))
        .collect()
}

/// Both recoveries, because the boundary cannot tell which one is meant.
///
/// Drift is equally consistent with a resolution that reached too far and with
/// one that deliberately took the paths over, and only the author knows which.
/// Naming a single recovery would make the refusal decide the question it exists
/// to raise, so it states the branch each recipe belongs to and stops there.
fn remedy(label: &str, upstream_head: &str, classified: &[(String, Drift)]) -> String {
    let rewrites = quoted(classified, Drift::RewritesBaseline);
    let adds = quoted(classified, Drift::AddsToBaseline);
    let mut lines = vec![
        "choose one branch before editing anything".to_owned(),
        "unintended drift, drop the changes: put every path above back as the shared branch \
         has it and commit the restoration"
            .to_owned(),
    ];
    if !rewrites.is_empty() {
        lines.push(format!("git checkout {upstream_head} --{rewrites}"));
    }
    if !adds.is_empty() {
        lines.push(format!("git rm --{adds}"));
    }
    lines.push(format!(
        "deliberate ownership, keep the changes: give every path above a real delta and \
         record them in a new non-merge commit, which is what puts them inside the \
         footprint of {label}"
    ));
    lines.push("then retry task completion".to_owned());
    lines.join("; ")
}

/// The same two branches, from a lane whose integration holds the baseline.
///
/// The recovery opens with the lane's movement, because it is the line every
/// surface prints: `HEAD` already keeps every drifted path as the shared branch
/// has it, so dropping restores from `HEAD`, and owning commits what is staged.
fn integration_remedy(
    label: &str,
    upstream_head: &str,
    integration: &str,
    classified: &[(String, Drift)],
) -> String {
    let rewrites = quoted(classified, Drift::RewritesBaseline);
    let adds = quoted(classified, Drift::AddsToBaseline);
    let mut lines = vec![
        format!(
            "nothing was published; the lane holds baseline {upstream_head} integrated as \
             {integration}, which keeps{rewrites}{adds} as the shared branch has them while \
             the resolved content stays staged"
        ),
        "choose one branch before editing anything".to_owned(),
        format!(
            "deliberate ownership, keep the changes: adapt them as needed and git commit the \
             staged paths through the pre-commit gate, a non-merge commit that puts them \
             inside the footprint of {label}"
        ),
        "unintended drift, drop the changes: put them back as HEAD has them".to_owned(),
    ];
    if !rewrites.is_empty() {
        lines.push(format!("git checkout HEAD --{rewrites}"));
    }
    if !adds.is_empty() {
        lines.push(format!("git rm -f --{adds}"));
    }
    lines.push("then retry task completion".to_owned());
    lines.join("; ")
}

#[cfg(test)]
mod tests {
    use super::footprint;

    const BASE: &str = "1111111111111111111111111111111111111111";
    const TASK: &str = "2222222222222222222222222222222222222222";
    const INTEGRATION: &str = "3333333333333333333333333333333333333333";

    /// `git log --format=%x00%P --name-only`, newest first; a merge lists
    /// paths only under `-m` or `--cc`, which this record shows it doing.
    fn log(records: &[(&[&str], &[&str])]) -> String {
        records
            .iter()
            .map(|(parents, paths)| {
                let names = paths
                    .iter()
                    .map(|path| format!("{path}\n"))
                    .collect::<String>();
                let body = if names.is_empty() {
                    String::new()
                } else {
                    format!("\n{names}")
                };
                format!("\0{}\n{body}", parents.join(" "))
            })
            .collect()
    }

    #[test]
    fn a_path_a_non_merge_commit_names_after_the_integration_counts() {
        let footprint = footprint(&log(&[
            (&[INTEGRATION], &["tests/caller.rs"]),
            (&[BASE, TASK], &["tests/caller.rs", "src/peer.rs"]),
            (&[BASE], &["src/lib.rs"]),
        ]));
        assert_eq!(
            footprint,
            ["src/lib.rs", "tests/caller.rs"].map(str::to_owned).into()
        );
    }

    #[test]
    fn a_path_only_an_integration_merge_changes_stays_outside() {
        let footprint = footprint(&log(&[
            (&[BASE, TASK], &["tests/caller.rs", "src/lib.rs"]),
            (&[BASE], &["src/lib.rs"]),
        ]));
        assert_eq!(footprint, ["src/lib.rs"].map(str::to_owned).into());
    }
}
