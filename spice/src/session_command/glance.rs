//! Bounded glances a briefing folds beside held work: the board around a
//! lane, the direction it already retired, and the checkout's working state.
//!
//! Each glance reads only what it can bound. The board walks current task
//! heads page by page up to a fixed ceiling and says when it stopped early;
//! the working state is two read-only `git` queries that take no index lock.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use journal::{FactReadBudget, GuardError, JournalRead, RetiredDirection};

use super::model::{BoardGlanceView, RepositoryGlanceView, RetiredDirectionView};

/// Task-head pages the board glance walks before it reports itself partial.
const BOARD_PAGES: usize = 16;
/// Retired directions a briefing recalls.
pub(super) const DIRECTION_ROWS: usize = 4;
/// Bytes of one retired direction's text.
const DIRECTION_BYTES: usize = 320;
/// Uncommitted paths the working-state glance keeps.
const DIRTY_ROWS: usize = 24;
/// Newest commits the working-state glance keeps.
const COMMIT_ROWS: usize = 8;

/// Pending execution work no actor holds, counted by phase.
///
/// Design roots and unauthorized outputs controlled by a Design stay on their
/// dedicated operator-facing surfaces rather than being presented as a lane's
/// next work.
pub(super) fn board(read: &JournalRead<'_>) -> Result<BoardGlanceView, GuardError> {
    let mut unclaimed = BTreeMap::<String, usize>::new();
    let mut resolved = board::ResolvedTasks::new();
    let mut resolved_authority = BTreeSet::<wire::TaskStamp>::new();
    let mut after = None;
    let mut partial = true;
    for _ in 0..BOARD_PAGES {
        let page = read.task_head_page(after.as_ref(), FactReadBudget::RETAINED)?;
        for heads in &page.rows {
            let statement = heads.statement().body();
            let workflow = heads.workflow().body();
            let held = heads.actively_claimed();
            if held
                || !workflow.status.is_pending()
                || speculative_design_work(
                    read,
                    statement,
                    workflow,
                    &mut resolved,
                    &mut resolved_authority,
                )?
            {
                continue;
            }
            *unclaimed
                .entry(workflow.phase.as_str().to_owned())
                .or_default() += 1;
        }
        match page.continuation {
            Some(next) => after = Some(next),
            None => {
                partial = false;
                break;
            }
        }
    }
    Ok(BoardGlanceView {
        unclaimed: unclaimed.into_iter().collect(),
        partial,
    })
}

/// Resolve the immediate planning authority a prepared statement names. Each
/// exact parent or controller lookup happens at most once during the bounded
/// glance; ordinary rows need no extra read.
fn speculative_design_work(
    read: &JournalRead<'_>,
    statement: &wire::TaskHead,
    workflow: &wire::WorkflowHead,
    resolved: &mut board::ResolvedTasks,
    loaded: &mut BTreeSet<wire::TaskStamp>,
) -> Result<bool, GuardError> {
    if let Some(preparation) = &statement.preparation {
        resolve_authority_task(read, &preparation.parent, resolved, loaded)?;
        resolve_authority_task(read, &preparation.controller, resolved, loaded)?;
    }
    Ok(resolved.speculative_design_work(statement, workflow))
}

fn resolve_authority_task(
    read: &JournalRead<'_>,
    task: &wire::TaskStamp,
    resolved: &mut board::ResolvedTasks,
    loaded: &mut BTreeSet<wire::TaskStamp>,
) -> Result<(), GuardError> {
    if !loaded.insert(task.clone()) {
        return Ok(());
    }
    let Some(heads) = read.task_heads_for(task)? else {
        return Ok(());
    };
    let statement = heads.statement().body();
    let workflow = heads.workflow().body();
    resolved.know(task.clone(), statement.intent);
    if !workflow.status.is_pending() {
        resolved.settle(task.clone(), workflow.concluded_positively());
    }
    Ok(())
}

/// The newest directions this lane retired, as context rather than authority.
pub(super) fn directions(retired: Vec<RetiredDirection>) -> Vec<RetiredDirectionView> {
    retired
        .into_iter()
        .map(|row| RetiredDirectionView {
            key: row.key,
            disposition: format!("{:?}", row.disposition).to_ascii_lowercase(),
            published: row.published.to_string(),
            text: crate::read_page::clip(&row.text, DIRECTION_BYTES),
        })
        .collect()
}

/// Uncommitted paths, the newest commits, the commits the integration
/// baseline lacks, and any unfinished Git operation of one checkout.
pub(super) fn repository(root: &Path, ambient: &spice::ambient::Ambient) -> RepositoryGlanceView {
    let read = || -> Result<RepositoryGlanceView, String> {
        let status = git(
            root,
            &["status", "--porcelain=v1", "--untracked-files=normal"],
        )?;
        let dirty = status
            .lines()
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        let log = git(
            root,
            &["log", "-n", &COMMIT_ROWS.to_string(), "--format=%h %s"],
        )?;
        let unlanded = spice::task_git::unlanded(root, ambient, COMMIT_ROWS)
            .map_err(|error| error.to_string())?;
        let operation = spice::task_git::unfinished_operation(root, ambient)
            .map_err(|error| error.to_string())?;
        Ok(RepositoryGlanceView {
            dirty_total: dirty.len(),
            dirty: dirty
                .into_iter()
                .take(DIRTY_ROWS)
                .map(str::to_owned)
                .collect(),
            commits: log.lines().map(str::to_owned).collect(),
            unlanded_baseline: unlanded
                .baseline
                .filter(|_| unlanded.total > 0)
                .map(|baseline| baseline.chars().take(12).collect()),
            unlanded: unlanded.rows,
            unlanded_total: unlanded.total,
            operation: operation.map(str::to_owned),
            unreadable: None,
        })
    };
    read().unwrap_or_else(|error| RepositoryGlanceView {
        unreadable: Some(error),
        ..RepositoryGlanceView::default()
    })
}

/// The working-state glance of a checkout no briefing reads.
#[cfg(test)]
pub(super) fn unread(_root: &Path, _ambient: &spice::ambient::Ambient) -> RepositoryGlanceView {
    RepositoryGlanceView {
        unreadable: Some("working state is not read in process".to_owned()),
        ..RepositoryGlanceView::default()
    }
}

fn git(root: &Path, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotepath=off"])
        .args(arguments)
        // A glance must never contend with the agent's own index writes.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|error| format!("git {}: {error}", arguments.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} exited {}: {}",
            arguments.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("git {} wrote non-UTF-8: {error}", arguments.join(" ")))
}
