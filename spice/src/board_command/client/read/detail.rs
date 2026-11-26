//! The bounded detail page of `task show`: context, closure, workflow steps,
//! and room rows admitted under one budget, with the cursor that resumes it.

use super::*;

fn task_show_context_lines(
    task: &BoardTask,
    history: TaskShowHistory<'_>,
    origin_context: Option<&str>,
    budget: wire::output::Budget,
) -> Vec<String> {
    let statement = &task.statement.body;
    let workflow = &task.workflow.body;
    let mut lines = vec![
        format!("origin {}", statement.provenance),
        task_flow_line(&workflow.flow),
        format!("review {}", workflow.review_str()),
        format!("version {}", statement.revision),
        format!("claim_thread {}", claim_thread(task)),
    ];
    lines.extend(
        acknowledgment_lines(&statement.provenance, history.acknowledgments)
            .into_iter()
            .map(|line| budget.clip(&line).to_string()),
    );
    if let Some(origin_context) = origin_context {
        let clipped = budget.clip(origin_context);
        lines.extend(clipped.text.lines().map(str::to_owned));
        if let Some(withheld) = clipped.withheld_bytes {
            lines.push(format!(
                "origin_context withheld {withheld} bytes; recover with the session context commands below"
            ));
        }
    }
    lines.extend(creation_context_lines(statement));
    lines.extend(claim_context_lines(task));
    lines.push(format!(
        "spice doc ledger {} --output {}-ledger.md",
        statement.handle, statement.handle
    ));
    lines.extend(git_context_lines(task));
    lines.extend(parked_commit_lines(task));
    lines
}

#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TaskShowDetailSection {
    Context,
    Closure,
    Step,
    Room,
}

#[derive(serde::Serialize)]
pub(super) struct TaskShowDetailRow {
    pub(super) cursor: String,
    pub(super) section: TaskShowDetailSection,
    pub(super) text: String,
    #[serde(skip)]
    pub(super) step: Option<wire::intent::TaskDetailStep>,
}

pub(super) fn bounded_detail_lines(
    board: &BoardAnswer,
    task: &BoardTask,
    history: TaskShowHistory<'_>,
    origin_context: Option<&str>,
    room: Option<TaskShowRoom>,
    options: &TaskShowOptions,
    budget: wire::output::Budget,
) -> Result<(Vec<String>, Vec<wire::intent::TaskDetailStep>), Outcome<CommandAnswer>> {
    let mut rows = Vec::new();
    if options.context {
        rows.extend(
            task_show_context_lines(task, history, origin_context, budget)
                .into_iter()
                .enumerate()
                .map(|(index, text)| TaskShowDetailRow {
                    cursor: format!("context:{index}"),
                    section: TaskShowDetailSection::Context,
                    text,
                    step: None,
                }),
        );
    }
    if options.history {
        if let Some(closure) = history.closure {
            rows.push(TaskShowDetailRow {
                cursor: format!("closure:{}", closure.position),
                section: TaskShowDetailSection::Closure,
                text: closure_lines_bounded(board, closure, history.principals, budget).join("\n"),
                step: None,
            });
        }
        let mut steps = history.steps.iter().collect::<Vec<_>>();
        steps.sort_by_key(|step| std::cmp::Reverse(step.position));
        rows.extend(steps.into_iter().map(|step| TaskShowDetailRow {
            cursor: format!("step:{}", step.position),
            section: TaskShowDetailSection::Step,
            text: render_task_step(board, step, history.principals, budget),
            step: Some(step.clone()),
        }));
    }
    if resumes_in_room(options) {
        rows.clear();
    } else if let Some(after) = &options.after {
        let Some(index) = rows.iter().position(|row| row.cursor == *after) else {
            return Err(refusal(
                "task::show-after-unknown",
                Some(after.clone()),
                "name a detail cursor present in the selected context or history",
            ));
        };
        rows = rows.split_off(index);
    }

    let mut admission = wire::output::Admission::new(budget);
    for row in rows {
        admission
            .offer(row)
            .expect("a task detail text row always serializes");
    }
    let room_empty = room.as_ref().is_some_and(TaskShowRoom::is_empty);
    if let Some(room) = room {
        room.offer(&mut admission);
    }
    let page = admission
        .finish(|resume| task_show_continuation(&task.statement.body.handle, options, resume));
    let mut lines = Vec::new();
    let mut steps = Vec::new();
    let mut steps_started = false;
    let mut room_started = false;
    for row in page.rows {
        if matches!(row.section, TaskShowDetailSection::Step) && !steps_started {
            lines.push("workflow:".to_owned());
            steps_started = true;
        }
        if matches!(row.section, TaskShowDetailSection::Room) && !room_started {
            lines.push("room:".to_owned());
            room_started = true;
        }
        lines.push(row.text);
        steps.extend(row.step);
    }
    if room_empty {
        lines.extend(["room:".to_owned(), "  -".to_owned()]);
    }
    if let Some(omission) = page.omission {
        lines.push(omission.to_string());
    }
    Ok((lines, steps))
}

fn render_task_step(
    board: &BoardAnswer,
    step: &wire::intent::TaskDetailStep,
    principals: Option<&PrincipalDirectory>,
    budget: wire::output::Budget,
) -> String {
    let author = actor_label(board, principals, &step.author);
    let mut rendered = format!("  {}", step_heading(step, &author));
    if let Some(text) = step_text(step) {
        let text = budget.clip(text);
        push_indented(&mut rendered, &text.text);
        if let Some(withheld) = text.withheld_bytes {
            rendered.push_str(&format!("\n    … [+{withheld} bytes]"));
        }
    }
    if let Some(disposition) = step_disposition(step, |stamp| stamp_name(board, stamp)) {
        push_indented(&mut rendered, &disposition);
    }
    if let Some(followups) = step_followups(step, |stamp| stamp_name(board, stamp)) {
        push_indented(&mut rendered, &followups);
    }
    rendered
}

/// Append `text` under a history row's heading, four columns in.
fn push_indented(rendered: &mut String, text: &str) {
    for line in text.lines() {
        rendered.push('\n');
        if !line.is_empty() {
            rendered.push_str("    ");
            rendered.push_str(line);
        }
    }
}

fn closure_lines_bounded(
    board: &BoardAnswer,
    closure: &wire::intent::TaskDetailClosure,
    principals: Option<&PrincipalDirectory>,
    budget: wire::output::Budget,
) -> Vec<String> {
    let displaced = closure
        .claimed
        .as_ref()
        .map_or_else(String::new, |actor| format!(" claim-freed={actor}"));
    let mut lines = vec![format!(
        "closed {} {} {} phase={}{displaced}",
        closure.position,
        closure.at,
        actor_label(board, principals, &closure.author),
        closure.phase
    )];
    let validation = budget.clip(&closure.validation);
    lines.extend(validation.text.lines().map(|line| {
        if line.is_empty() {
            String::new()
        } else {
            format!("    {line}")
        }
    }));
    if let Some(withheld) = validation.withheld_bytes {
        lines.push(format!("    … [+{withheld} bytes]"));
    }
    lines
}

pub(super) fn task_show_continuation(
    handle: &TaskHandle,
    options: &TaskShowOptions,
    resume: wire::output::Resume<'_, TaskShowDetailRow>,
) -> wire::SpiceCommand {
    let (row, widened) = match resume {
        wire::output::Resume::At(row) => (row, None),
        wire::output::Resume::Widened { record, bytes } => (record, Some(bytes)),
        wire::output::Resume::Export(row) => (row, Some(wire::output::INTERACTIVE.maximum().bytes)),
    };
    let limits = options.budget().resume();
    let mut arguments = vec!["show".to_owned(), handle.to_string()];
    if options.full {
        arguments.push("--full".to_owned());
    }
    if options.context {
        arguments.push("--context".to_owned());
    }
    if options.history {
        arguments.push("--history".to_owned());
    }
    arguments.extend([
        "--limit".to_owned(),
        limits.rows.to_string(),
        "--max-bytes".to_owned(),
        widened.unwrap_or(limits.bytes).to_string(),
        "--max-text".to_owned(),
        limits.text.to_string(),
        "--after".to_owned(),
        row.cursor.clone(),
    ]);
    wire::SpiceCommand::new("task", arguments)
        .expect("task show continuation has structural arguments")
}
