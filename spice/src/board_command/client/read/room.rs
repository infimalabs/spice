//! A task's room conversation as detail rows, newest first: the `room:`
//! section of `task show` and the room context of a claim answer.

use super::*;
use crate::room_command::{NewestRows, SourceOwners, newest_rows};
use wire::rooms::RoomAuthorityStamp;

/// The cursor prefix of one room publication's detail row.
const ROOM_CURSOR: &str = "room:";

/// The newest publications of a task's room one read admits, rendered, and
/// how many the room holds from the first of them back.
pub(in crate::board_command) struct TaskShowRoom {
    population: usize,
    rows: Vec<TaskShowDetailRow>,
}

impl TaskShowRoom {
    pub(super) fn is_empty(&self) -> bool {
        self.population == 0
    }

    /// Offer these rows after every earlier section and count the
    /// publications past them as withheld. Rows are read one past the row
    /// budget, so any publication left unread follows a closed admission.
    pub(super) fn offer(self, admission: &mut wire::output::Admission<TaskShowDetailRow>) {
        let shown = self.rows.len();
        for row in self.rows {
            admission
                .offer(row)
                .expect("a task detail text row always serializes");
        }
        admission.withhold(self.population - shown);
    }
}

/// Whether `options` resumes inside the room section.
pub(super) fn resumes_in_room(options: &TaskShowOptions) -> bool {
    options
        .after
        .as_deref()
        .is_some_and(|after| after.starts_with(ROOM_CURSOR))
}

/// `task`'s room for one `task show`, read from the local journal from the
/// publication a `room:` cursor names, else from the newest.
pub(super) fn task_show_room(
    repository: Host<'_>,
    task: &BoardTask,
    options: &TaskShowOptions,
) -> Result<TaskShowRoom, Outcome<CommandAnswer>> {
    let unknown = || {
        refusal(
            "task::show-after-unknown",
            options.after.clone(),
            "name a detail cursor present in the selected context or history",
        )
    };
    let from = options
        .after
        .as_deref()
        .and_then(|after| after.strip_prefix(ROOM_CURSOR))
        .map(|written| written.parse::<RoomAuthorityStamp>().map_err(|_| unknown()))
        .transpose()?;
    let statement = &task.statement.body;
    let unreadable = |error: &dyn std::fmt::Display| room_unreadable(&statement.handle, error);
    let state = repository
        .admit_state()
        .map_err(|error| repository_state_failure(&error))?;
    let budget = options.budget();
    let reader =
        journal::JournalReader::open(state.journal()).map_err(|error| unreadable(&error))?;
    let rows = reader
        .read(|read| {
            newest_rows(
                read,
                &statement.born,
                from.as_ref(),
                budget.limits().rows + 1,
            )
        })
        .map_err(|error| unreadable(&error))?
        .ok_or_else(unknown)?;
    let artifacts = artifact_store(&state);
    Ok(TaskShowRoom::new(
        rows,
        SourceOwners {
            tape: state.tape(),
            artifacts: &artifacts,
            git: repository.common_dir(),
        },
        &statement.born,
        budget,
    ))
}

/// `task`'s room conversation for a claim answer: its newest publications
/// under the interactive read bounds and the `task show` command that pages
/// the rest; absent when the room holds none.
pub(in crate::board_command) fn claim_room_context(
    bus: &Bus,
    repository: Host<'_>,
    task: &TaskHandle,
    room: &TaskStamp,
) -> Result<Option<Box<str>>, Outcome<CommandAnswer>> {
    let options = TaskShowOptions {
        full: false,
        context: false,
        history: true,
        limit: None,
        max_bytes: None,
        max_text: None,
        after: None,
    };
    let budget = options.budget();
    let (anchor, retain) = (room.clone(), budget.limits().rows + 1);
    let rows = bus
        .read(move |read| newest_rows(read, &anchor, None, retain))
        .map_err(|error| room_unreadable(task, &error))?
        .expect("a read from the newest publication names no cursor");
    let state = repository
        .admit_state()
        .map_err(|error| repository_state_failure(&error))?;
    let artifacts = artifact_store(&state);
    let room = TaskShowRoom::new(
        rows,
        SourceOwners {
            tape: state.tape(),
            artifacts: &artifacts,
            git: repository.common_dir(),
        },
        room,
        budget,
    );
    if room.is_empty() {
        return Ok(None);
    }
    let mut admission = wire::output::Admission::new(budget);
    room.offer(&mut admission);
    let page = admission.finish(|resume| task_show_continuation(task, &options, resume));
    let mut lines = vec![ROOM_CURSOR.to_owned()];
    lines.extend(page.rows.into_iter().map(|row| row.text));
    lines.extend(page.omission.map(|omission| omission.to_string()));
    Ok(Some(lines.join("\n").into_boxed_str()))
}

fn artifact_store(state: &spice::repository::StatePaths) -> artifact::Store {
    artifact::Store::new(state.artifacts(), state.artifacts_lock())
}

impl TaskShowRoom {
    /// `rows` with their sources read from `owners`, each rendered as one
    /// detail row with its authored text clipped under `budget`.
    pub(in crate::board_command) fn new(
        rows: NewestRows,
        owners: SourceOwners<'_>,
        room: &TaskStamp,
        budget: wire::output::Budget,
    ) -> Self {
        let newest = rows.project(owners, room);
        Self {
            population: newest.population,
            rows: newest
                .publications
                .into_iter()
                .map(|publication| {
                    let (cursor, text) = publication.detail_row(budget);
                    TaskShowDetailRow {
                        cursor,
                        section: TaskShowDetailSection::Room,
                        text: text
                            .lines()
                            .map(|line| format!("  {line}"))
                            .collect::<Vec<_>>()
                            .join("\n"),
                        step: None,
                    }
                })
                .collect(),
        }
    }
}

fn room_unreadable(task: &TaskHandle, error: &dyn std::fmt::Display) -> Outcome<CommandAnswer> {
    store_fault(
        "task::room-unreadable",
        format!("cannot read the room of {task}: {error}"),
    )
}
