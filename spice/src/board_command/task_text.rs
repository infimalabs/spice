//! A task's description or terms as one text that a whole replacement
//! rewrites through the `task modify` intent.
//!
//! The text carries a [`Domain::TaskText`] digest. A replacement names the
//! digest of the text it read; the edit admits it only when that digest still
//! names the statement the edit witnesses. A replacement therefore never lands
//! over text its writer did not see.

use wire::intent::BoardStatement;

use super::*;

/// One replaceable text of a task statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskText {
    Description,
    Term,
}

impl TaskText {
    /// The text's name, as its resource path and its refusals spell it.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Description => "description",
            Self::Term => "terms",
        }
    }

    /// The current text: the description verbatim, or each term
    /// as a `- ` line whose continuation lines are indented two spaces.
    pub(crate) fn current(self, statement: &BoardStatement) -> String {
        match self {
            Self::Description => statement.description.clone().unwrap_or_default(),
            Self::Term => statement
                .term
                .iter()
                .map(|item| {
                    let mut lines = item.lines();
                    let first = lines.next().unwrap_or_default();
                    std::iter::once(format!("- {first}"))
                        .chain(lines.map(|line| {
                            if line.is_empty() {
                                String::new()
                            } else {
                                format!("  {line}")
                            }
                        }))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    /// The digest a replacement of the current text names.
    pub(crate) fn digest(self, statement: &BoardStatement) -> Digest {
        Digest::compute(Domain::TaskText, self.current(statement).as_bytes())
    }
}

/// `description` collapsed to one line: its line count and the resource that
/// expands exactly it, or `-` when there is none.
pub(crate) fn collapsed_description(handle: &TaskHandle, description: Option<&str>) -> String {
    let lines = description
        .filter(|text| !text.trim().is_empty())
        .map_or(0, |text| text.lines().count());
    collapsed("description", lines, "line", handle, TaskText::Description)
}

/// `term` collapsed to one line under `label` (`questions` for a
/// Design): its item count and the resource that expands exactly it.
pub(crate) fn collapsed_term(handle: &TaskHandle, label: &str, term: &[String]) -> String {
    let items = term.iter().filter(|item| !item.trim().is_empty()).count();
    collapsed(label, items, "item", handle, TaskText::Term)
}

fn collapsed(label: &str, count: usize, unit: &str, handle: &TaskHandle, text: TaskText) -> String {
    match count {
        0 => format!("{label}: -"),
        1 => format!("{label}: 1 {unit} spice://task/{handle}/{}", text.name()),
        count => format!(
            "{label}: {count} {unit}s spice://task/{handle}/{}",
            text.name()
        ),
    }
}

/// The terms a written text names, in order.
///
/// A line beginning `- ` (or a lone `-`) opens an item; every other line
/// continues the open one, less a two-space indent. Trailing blank lines of
/// an item are not part of it.
pub(crate) fn term_items(text: &str) -> Result<Vec<String>, String> {
    let mut items: Vec<Vec<&str>> = Vec::new();
    for line in text.lines() {
        if let Some(first) = line.strip_prefix("- ").or((line == "-").then_some("")) {
            items.push(vec![first]);
        } else if let Some(item) = items.last_mut() {
            item.push(line.strip_prefix("  ").unwrap_or(line));
        } else if !line.trim().is_empty() {
            return Err(format!(
                "term text opens each item with `- `; the first line is `{line}`"
            ));
        }
    }
    Ok(items
        .into_iter()
        .map(|mut lines| {
            while lines.len() > 1 && lines.last().is_some_and(|line| line.trim().is_empty()) {
                lines.pop();
            }
            lines.join("\n")
        })
        .collect())
}

/// Map a replacement of `text` onto the `task modify` intent for `task`, or
/// refuse it when `named` is not the digest of the text the edit replaces.
pub(crate) fn text_edit(
    task: &BoardTask,
    text: TaskText,
    named: Option<&Digest>,
    replacement: &str,
) -> Result<(CommandKind, Vec<Witness>, serde_json::Value), Outcome<CommandAnswer>> {
    let statement = &task.statement.body;
    let current = text.digest(statement);
    if named != Some(&current) {
        let (reason, detail) = match named {
            Some(named) => (
                "task::text-digest-moved",
                format!(
                    "the {} changed since the read that named digest {named}; its current digest is {current}, so read it again and replace what it says now",
                    text.name()
                ),
            ),
            None => (
                "task::text-digest-missing",
                format!(
                    "a replacement of the {} names the digest of the text it replaces, currently {current}; read it first",
                    text.name()
                ),
            ),
        };
        return Err(refusal(reason, Some(current.to_string()), &detail));
    }
    let replacement = replacement.trim_end_matches(['\n', '\r']);
    let (description, term) = match text {
        TaskText::Description => (Some(replacement.to_owned()), None),
        TaskText::Term => {
            let items = term_items(replacement)
                .map_err(|detail| refusal("task::term-shape", None, &detail))?;
            if items.iter().all(|item| item.trim().is_empty()) {
                return Err(refusal(
                    "task::term-empty",
                    None,
                    "a replacement names at least one `- ` item; terms are replaced, never cleared",
                ));
            }
            (None, Some(items))
        }
    };
    Ok(statement_edit(task, description, term))
}

/// The one current term `written` names, as its current handle and its text.
///
/// A handle is derived from the term's own text, so it names exactly the text
/// a replacement at that handle replaces: a term reworded since it was read
/// answers to another handle, and the old one refuses as stale.
pub(crate) fn term_text(
    statement: &BoardStatement,
    written: &str,
) -> Result<(String, String), Outcome<CommandAnswer>> {
    let items = wire::StatementItem::each(&statement.term).collect::<Vec<_>>();
    let index = statement_item_index(written, &items)?;
    Ok((
        items[index].handle(&items).to_owned(),
        statement.term[index].clone(),
    ))
}

/// Map a replacement of the one term `written` names onto the `task modify`
/// intent for `task`, leaving every other term and the order as they stand.
pub(crate) fn term_edit(
    task: &BoardTask,
    written: &str,
    replacement: &str,
) -> Result<(CommandKind, Vec<Witness>, serde_json::Value), Outcome<CommandAnswer>> {
    let statement = &task.statement.body;
    let items = wire::StatementItem::each(&statement.term).collect::<Vec<_>>();
    let index = statement_item_index(written, &items)?;
    let replacement = replacement.trim_end_matches(['\n', '\r']);
    if replacement.trim().is_empty() {
        return Err(refusal(
            "task::term-empty",
            None,
            "a replacement of one term is its new text; a term is dropped by replacing the whole list without it",
        ));
    }
    let mut term = statement.term.clone();
    term[index] = replacement.to_owned();
    Ok(statement_edit(task, None, Some(term)))
}

/// The `task modify` intent replacing `description` or `term` of `task`,
/// witnessing the statement head the replaced text was read from.
fn statement_edit(
    task: &BoardTask,
    description: Option<String>,
    term: Option<Vec<String>>,
) -> (CommandKind, Vec<Witness>, serde_json::Value) {
    (
        CommandKind::TaskEdit,
        vec![statement_witness(task)],
        serde_json::to_value(TaskEdit {
            intent: None,
            task: task.statement.body.handle.clone(),
            title: None,
            pitch: None,
            description,
            term,
            requirements: None,
            priority: None,
            topic: None,
            flow: None,
            related: Vec::new(),
        })
        .expect("typed intent encodes"),
    )
}

/// The current evidence of one task, read as an edit of it reads it.
pub(crate) fn current_task(
    repository: Host<'_>,
    task: &TaskHandle,
) -> Result<BoardTask, Outcome<CommandAnswer>> {
    let board = current_board(repository, single_task_read(task))?;
    task_heads(&board, task).cloned()
}

/// Replace `text` of `task` through the `task modify` intent, as the CLI's
/// actor, when `named` is the digest of the text it replaces.
pub(crate) fn replace_text(
    repository: Host<'_>,
    task: &TaskHandle,
    text: TaskText,
    named: Option<&Digest>,
    replacement: &str,
) -> Outcome<CommandAnswer> {
    let contract =
        crate::cli::contract(&["task", "modify"]).expect("task modify contract is registered");
    mutate_from_board(repository, contract, single_task_read(task), |board| {
        text_edit(task_heads(&board, task)?, text, named, replacement)
    })
}

/// Replace the one term `written` names through the `task modify` intent, as
/// the CLI's actor.
pub(crate) fn replace_term(
    repository: Host<'_>,
    task: &TaskHandle,
    written: &str,
    replacement: &str,
) -> Outcome<CommandAnswer> {
    let contract =
        crate::cli::contract(&["task", "modify"]).expect("task modify contract is registered");
    mutate_from_board(repository, contract, single_task_read(task), |board| {
        term_edit(task_heads(&board, task)?, written, replacement)
    })
}

fn single_task_read(task: &TaskHandle) -> BoardRead {
    BoardRead::Tasks {
        claims: false,
        tasks: vec![
            task.born()
                .expect("a parsed handle carries a canonical task stamp"),
        ],
        dependencies: false,
        history: false,
    }
}
