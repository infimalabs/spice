//! Exact admission of one allocator-materialized merge resolution.

use std::fs;

use super::landing::{Drift, drift, integration_refusal};
use super::transaction::{
    git_index_spelling, git_state_path, materialize_integration, materialize_resolution,
};
use super::*;

const INTEGRATION_INDEX: &str = "spice-integration-index";

#[derive(Debug, Eq, PartialEq)]
struct Seal {
    agent: String,
    upstream: String,
    tree: String,
    head_ref: String,
    configuration: String,
    message: String,
}

pub(super) struct Preflight(Seal);

pub(super) struct Parents<'a> {
    pub(super) agent: &'a str,
    pub(super) upstream: &'a str,
    pub(super) preflight: Preflight,
}

pub(super) fn has_state(root: Worktree<'_>) -> Result<bool, Error> {
    // ORIG_HEAD is durable Git history and commonly survives completed merges;
    // only the three in-progress controls select resolution admission.
    for name in ["MERGE_HEAD", "MERGE_MODE", "MERGE_MSG"] {
        let path = git_state_path(root, name)?;
        match fs::metadata(&path) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(Error::fault(
                    "git::resolution-state-unreadable",
                    format!("{}: {error}", path.display()),
                ));
            }
        }
    }
    Ok(false)
}

/// Commit an admitted resolution, publishing only one inside the footprint.
///
/// A resolved tree that drifts still becomes the lane's integration, with every
/// drifted path held as the baseline has it, and the refusal that follows
/// leaves its resolved content staged for the claimant to own or drop.
pub(super) fn complete(
    root: Worktree<'_>,
    meta: Metadata<'_>,
    active: &board::Active<'_>,
    parents: Parents<'_>,
    message: &str,
) -> Result<String, Error> {
    let Parents {
        agent,
        upstream,
        preflight,
    } = parents;
    require_claim(meta, active)?;
    let seal = preflight.complete(root, agent, upstream, message)?;
    let drifted = drift(root, upstream, &seal.tree, agent)?;
    seal.verify(root)?;
    let tree = if drifted.is_empty() {
        seal.tree.clone()
    } else {
        integration_tree(root, &seal.tree, upstream, &drifted)?
    };
    let created = create_merge_commit(root, &tree, &seal.upstream, &seal.agent, &seal.message)?;
    if !created.success() || created.stdout.trim().is_empty() {
        return Err(Error::fault(
            "git::resolution-commit-failed",
            created.detail("create admitted task resolution"),
        ));
    }
    let commit = created.stdout.trim().to_owned();
    seal.verify(root)?;
    if drifted.is_empty() {
        materialize_resolution(root, &commit, &seal.agent, &seal.tree, meta.label)?;
        return Ok(commit);
    }
    materialize_integration(root, &commit, &seal.agent, meta.label)?;
    Err(integration_refusal(meta.label, upstream, &commit, &drifted))
}

/// The sealed tree with every drifted path held as the baseline has it.
///
/// Built in a private index so the lane's own index keeps the sealed
/// resolution, which is what leaves the drifted content staged afterwards.
fn integration_tree(
    root: Worktree<'_>,
    tree: &str,
    upstream: &str,
    drifted: &[(String, Drift)],
) -> Result<String, Error> {
    let index = git_state_path(root, INTEGRATION_INDEX)?;
    let built = git_index_spelling(&index, "git::integration-index-path")
        .and_then(|path| build_integration_tree(root, &path, tree, upstream, drifted));
    match fs::remove_file(&index) {
        Ok(()) => built,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => built,
        Err(error) => built.and(Err(Error::fault(
            "git::integration-index-cleanup",
            format!("{}: {error}", index.display()),
        ))),
    }
}

fn build_integration_tree(
    root: Worktree<'_>,
    index: &str,
    tree: &str,
    upstream: &str,
    drifted: &[(String, Drift)],
) -> Result<String, Error> {
    indexed(root, index, &["read-tree", tree], "read sealed resolution")?;
    let adds = drifted
        .iter()
        .filter(|(_, drift)| *drift == Drift::AddsToBaseline)
        .map(|(path, _)| path.as_str())
        .collect::<Vec<_>>();
    if !adds.is_empty() {
        let mut args = vec!["update-index", "--force-remove", "--"];
        args.extend(adds);
        indexed(root, index, &args, "drop paths the baseline lacks")?;
    }
    let mut entries = Vec::new();
    for (path, _) in drifted
        .iter()
        .filter(|(_, drift)| *drift == Drift::RewritesBaseline)
    {
        let listed = required_read(
            root,
            &["ls-tree", upstream, "--", path],
            "read baseline entry",
        )?;
        let mut fields = listed.split_whitespace();
        let (Some(mode), Some(_), Some(object)) = (fields.next(), fields.next(), fields.next())
        else {
            return Err(Error::fault(
                "git::integration-entry-invalid",
                format!("baseline {upstream} lists {path:?} as {listed:?}"),
            ));
        };
        entries.push(format!("{mode},{object},{path}"));
    }
    if !entries.is_empty() {
        let mut args = vec!["update-index", "--add"];
        for entry in &entries {
            args.extend(["--cacheinfo", entry.as_str()]);
        }
        indexed(root, index, &args, "hold paths at the baseline")?;
    }
    indexed(root, index, &["write-tree"], "write integration tree")
}

fn indexed(root: Worktree<'_>, index: &str, args: &[&str], action: &str) -> Result<String, Error> {
    let output = run_with_environment(root, args, &[("GIT_INDEX_FILE", index)])?;
    if output.success() {
        Ok(output.stdout.trim().to_owned())
    } else {
        Err(Error::fault(
            "git::integration-index-failed",
            output.detail(action),
        ))
    }
}

impl Preflight {
    pub(super) fn capture(root: Worktree<'_>) -> Result<Self, Error> {
        let upstream = read_state_value(root, "MERGE_HEAD")?;
        let agent = read_state_value(root, "ORIG_HEAD")?;
        require_state_file(root, "MERGE_MODE", "no-ff\n")?;
        let message = read_state_text(root, "MERGE_MSG")?
            .strip_suffix('\n')
            .ok_or_else(|| {
                Error::refusal(
                    "git::resolution-state-mismatch",
                    "MERGE_MSG has no canonical final newline",
                    "do not edit Git merge control files; abort and retry from the current baseline",
                )
            })?
            .to_owned();
        require_resolved_index(root)?;
        require_equal_read(root, &["rev-parse", "HEAD"], "HEAD", &agent)?;
        let tree = required_read(root, &["write-tree"], "seal resolved index tree")?;
        let head_ref = required_read(
            root,
            &["symbolic-ref", "--quiet", "HEAD"],
            "read resolution HEAD ref",
        )?;
        let configuration = configuration(root)?;
        Ok(Self(Seal {
            agent,
            upstream,
            tree,
            head_ref,
            configuration,
            message,
        }))
    }

    fn complete(
        self,
        root: Worktree<'_>,
        agent: &str,
        upstream: &str,
        message: &str,
    ) -> Result<Seal, Error> {
        let seal = Seal::capture(root, agent, upstream, message)?;
        self.require_unchanged(seal)
    }

    fn require_unchanged(self, seal: Seal) -> Result<Seal, Error> {
        if seal == self.0 {
            Ok(seal)
        } else {
            Err(Error::refusal(
                "git::resolution-state-moved",
                "resolution controls, index, HEAD, or Git configuration moved during completion",
                "restore the exact sealed resolution and retry task completion",
            ))
        }
    }
}

impl Seal {
    fn capture(
        root: Worktree<'_>,
        agent: &str,
        upstream: &str,
        message: &str,
    ) -> Result<Self, Error> {
        require_state_file(root, "MERGE_HEAD", &format!("{upstream}\n"))?;
        require_state_file(root, "ORIG_HEAD", &format!("{agent}\n"))?;
        require_state_file(root, "MERGE_MODE", "no-ff\n")?;
        require_state_file(root, "MERGE_MSG", &format!("{message}\n"))?;
        require_resolved_index(root)?;
        let head = required_read(root, &["rev-parse", "HEAD"], "read resolution HEAD")?;
        if head != agent {
            return Err(moved("HEAD", agent, &head));
        }
        let tree = required_read(root, &["write-tree"], "seal resolved index tree")?;
        let head_ref = required_read(
            root,
            &["symbolic-ref", "--quiet", "HEAD"],
            "read resolution HEAD ref",
        )?;
        let configuration = configuration(root)?;
        Ok(Self {
            agent: agent.to_owned(),
            upstream: upstream.to_owned(),
            tree,
            head_ref,
            configuration,
            message: message.to_owned(),
        })
    }

    fn verify(&self, root: Worktree<'_>) -> Result<(), Error> {
        require_state_file(root, "MERGE_HEAD", &format!("{}\n", self.upstream))?;
        require_state_file(root, "ORIG_HEAD", &format!("{}\n", self.agent))?;
        require_state_file(root, "MERGE_MODE", "no-ff\n")?;
        require_state_file(root, "MERGE_MSG", &format!("{}\n", self.message))?;
        require_resolved_index(root)?;
        require_equal_read(root, &["rev-parse", "HEAD"], "HEAD", &self.agent)?;
        require_equal_read(
            root,
            &["symbolic-ref", "--quiet", "HEAD"],
            "HEAD ref",
            &self.head_ref,
        )?;
        require_equal_read(root, &["write-tree"], "resolved index", &self.tree)?;
        let observed = configuration(root)?;
        if observed != self.configuration {
            return Err(Error::refusal(
                "git::resolution-configuration-moved",
                "Git configuration changed during resolution completion",
                "restore the intended repository configuration and retry task completion",
            ));
        }
        Ok(())
    }
}

fn require_claim(meta: Metadata<'_>, active: &board::Active<'_>) -> Result<(), Error> {
    if active.handle().as_str() == meta.label && active.actor().to_string() == meta.actor {
        return Ok(());
    }
    Err(Error::refusal(
        "git::resolution-claim-mismatch",
        format!(
            "resolution names task {} actor {}, but active proof names task {} actor {}",
            meta.label,
            meta.actor,
            active.handle(),
            active.actor(),
        ),
        "retry through the lane capability that owns this exact active task phase",
    ))
}

fn require_state_file(root: Worktree<'_>, name: &str, expected: &str) -> Result<(), Error> {
    let observed = read_state_text(root, name)?;
    if observed == expected {
        return Ok(());
    }
    Err(Error::refusal(
        "git::resolution-state-mismatch",
        format!("{name} does not match the allocator-materialized merge state"),
        "do not edit Git merge control files; abort and retry from the current baseline",
    ))
}

fn read_state_text(root: Worktree<'_>, name: &str) -> Result<String, Error> {
    let path = git_state_path(root, name)?;
    let observed = match fs::read_to_string(&path) {
        Ok(observed) => observed,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::refusal(
                "git::resolution-state-missing",
                format!("required {name} is absent at {}", path.display()),
                "restore the allocator-materialized merge state before retrying task completion",
            ));
        }
        Err(error) => {
            return Err(Error::fault(
                "git::resolution-state-unreadable",
                error.to_string(),
            ));
        }
    };
    Ok(observed)
}

fn read_state_value(root: Worktree<'_>, name: &str) -> Result<String, Error> {
    let text = read_state_text(root, name)?;
    let Some(value) = text.strip_suffix('\n') else {
        return Err(Error::refusal(
            "git::resolution-state-mismatch",
            format!("{name} is not one canonical line"),
            "do not edit Git merge control files; abort and retry from the current baseline",
        ));
    };
    if value.is_empty() || value.contains('\n') {
        return Err(Error::refusal(
            "git::resolution-state-mismatch",
            format!("{name} is not one canonical line"),
            "do not edit Git merge control files; abort and retry from the current baseline",
        ));
    }
    Ok(value.to_owned())
}

fn require_resolved_index(root: Worktree<'_>) -> Result<(), Error> {
    let unresolved = required_read(
        root,
        &["ls-files", "-u", "-z"],
        "inspect unresolved index stages",
    )?;
    if !unresolved.is_empty() {
        return Err(Error::refusal(
            "git::resolution-unresolved",
            "the task resolution still contains unresolved index stages",
            "resolve every conflicted path, git add each resolution, then retry task completion",
        ));
    }
    let unstaged = run(root, &["diff", "--quiet", "--exit-code"])?;
    if !unstaged.success() {
        return Err(Error::refusal(
            "git::resolution-unstaged",
            "the task resolution contains tracked worktree changes absent from the index",
            "git add every intended resolution before retrying task completion",
        ));
    }
    let untracked = required_read(
        root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        "inspect resolution untracked paths",
    )?;
    if !untracked.is_empty() {
        return Err(Error::refusal(
            "git::resolution-untracked",
            "the task resolution contains untracked paths outside the sealed index",
            "add or remove every untracked path before retrying task completion",
        ));
    }
    Ok(())
}

fn configuration(root: Worktree<'_>) -> Result<String, Error> {
    required_read(
        root,
        &[
            "config",
            "--null",
            "--show-origin",
            "--show-scope",
            "--list",
        ],
        "capture Git resolution configuration",
    )
}

fn require_equal_read(
    root: Worktree<'_>,
    args: &[&str],
    name: &str,
    expected: &str,
) -> Result<(), Error> {
    let observed = required_read(root, args, &format!("recheck resolution {name}"))?;
    if observed == expected {
        Ok(())
    } else {
        Err(moved(name, expected, &observed))
    }
}

fn moved(name: &str, expected: &str, observed: &str) -> Error {
    Error::refusal(
        "git::resolution-state-moved",
        format!("{name} moved from {expected} to {observed} while evidence was running"),
        "inspect the moved state and retry task completion from a fresh conflict handoff",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seal(configuration: &str) -> Seal {
        Seal {
            agent: "agent".to_owned(),
            upstream: "upstream".to_owned(),
            tree: "tree".to_owned(),
            head_ref: "refs/heads/main".to_owned(),
            configuration: configuration.to_owned(),
            message: "resolved task".to_owned(),
        }
    }

    #[test]
    fn preflight_refuses_configuration_movement_during_completion() {
        let error = Preflight(seal("before"))
            .require_unchanged(seal("after"))
            .expect_err("moved configuration is refused");
        assert_eq!(error.reason(), "git::resolution-state-moved");
    }

    #[test]
    fn preflight_admits_the_exact_sealed_state() {
        let expected = seal("unchanged");
        assert_eq!(
            Preflight(seal("unchanged"))
                .require_unchanged(expected)
                .expect("exact seal is admitted"),
            seal("unchanged")
        );
    }
}
