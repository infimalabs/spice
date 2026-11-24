//! Portable task-set file export and import.

use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;
use wire::interchange::{
    TASK_SET_MAX_BYTES, TASK_SET_MAX_TASKS, TASK_SET_VERSION, TaskSet, TaskSetTask,
};
use wire::output::Export;
use wire::{Digest, Domain, EnvironmentRequirement, EnvironmentRequirements, Fault, FaultClass};

use super::*;

/// Receipt for one portable task-set file.
#[derive(Debug, Serialize)]
pub(crate) struct TaskSetExport {
    pub(crate) tasks: usize,
    pub(crate) excluded: usize,
    #[serde(flatten)]
    pub(crate) export: Export,
    #[serde(skip)]
    text: String,
}

impl TaskSetExport {
    pub(crate) fn text(&self) -> Option<&str> {
        Some(&self.text)
    }
}

/// One fresh task created for an export-local identity.
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportedTask {
    pub(crate) id: u32,
    pub(crate) task: TaskHandle,
}

/// Receipt for one fully imported portable task set.
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskSetImport {
    pub(crate) input: String,
    pub(crate) digest: Digest,
    pub(crate) tasks: Vec<ImportedTask>,
    #[serde(skip)]
    text: String,
}

impl TaskSetImport {
    pub(crate) fn text(&self) -> Option<&str> {
        Some(&self.text)
    }
}

pub(crate) fn export(matches: &ArgMatches, ambient: &Ambient) -> Outcome<TaskSetExport> {
    let repository = match discover_repository() {
        Ok(repository) => repository,
        Err(outcome) => return failure(outcome),
    };
    let host = Host::new(&repository, ambient);
    let board = match current_board(host, BoardRead::All { family: None }) {
        Ok(board) => board,
        Err(outcome) => return failure(outcome),
    };
    let (set, excluded) = match project(&board) {
        Ok(projected) => projected,
        Err(outcome) => return outcome,
    };
    let bytes = match wire::encode(&set) {
        Ok(bytes) => bytes,
        Err(error) => {
            return fault(
                FaultClass::Protocol,
                "task::export-unencodable",
                error.to_string(),
            );
        }
    };
    if bytes.len() as u64 > TASK_SET_MAX_BYTES {
        return refuse(
            "task::export-too-large",
            Some(bytes.len().to_string()),
            format!("portable task sets admit at most {TASK_SET_MAX_BYTES} encoded bytes"),
        );
    }
    let output = matches
        .get_one::<String>("OUTPUT")
        .expect("the generated task export parser requires OUTPUT");
    let export = match spice::export::export(Path::new(output), &bytes) {
        Ok(export) => export,
        Err(refusal) => return Outcome::Refusal(refusal),
    };
    let tasks = set.tasks.len();
    let text = format!("exported {tasks} tasks ({excluded} excluded); {export}");
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(board.cursor),
        body: TaskSetExport {
            tasks,
            excluded,
            export,
            text,
        },
    })
}

pub(crate) fn import(matches: &ArgMatches, ambient: &Ambient) -> Outcome<TaskSetImport> {
    let input = matches
        .get_one::<String>("INPUT")
        .expect("the generated task import parser requires INPUT");
    let (path, bytes) = match read_input(Path::new(input)) {
        Ok(value) => value,
        Err(outcome) => return outcome,
    };
    let digest = Digest::compute(Domain::CliExport, &bytes);
    let set = match wire::decode::<TaskSet>(&bytes) {
        Ok(set) => set,
        Err(error) => {
            return refuse(
                "task::import-invalid",
                Some(path.display().to_string()),
                error.to_string(),
            );
        }
    };
    let order = match set.creation_order() {
        Ok(order) => order,
        Err(error) => {
            return refuse(
                "task::import-invalid",
                Some(path.display().to_string()),
                error.to_string(),
            );
        }
    };
    for task in &set.tasks {
        if let Err(outcome) = validate_term(&task.term).and_then(|()| validate_pitch(&task.pitch)) {
            return failure(outcome);
        }
    }
    let origin = match parse_import_origin(matches) {
        Ok(origin) => origin,
        Err(outcome) => return outcome,
    };
    let draft = matches.get_flag("draft");
    let repository = match discover_repository() {
        Ok(repository) => repository,
        Err(outcome) => return failure(outcome),
    };
    let host = Host::new(&repository, ambient);
    if origin.is_none()
        && !draft
        && set.tasks.iter().any(|task| !task.draft)
        && let Err(outcome) = require_held_cause(host)
    {
        return outcome;
    }
    submit_import(
        host,
        &set,
        order,
        origin,
        draft,
        path.display().to_string(),
        digest,
    )
}

/// Without `--origin` or `--draft`, a filed task takes its cause from the
/// importer's one held claim. Refuse before the first creation when none is
/// held, so a predictable admission refusal never leaves a partial import.
fn require_held_cause(host: Host<'_>) -> Result<(), Outcome<TaskSetImport>> {
    let board = current_board(
        host,
        BoardRead::Open {
            recent_finished: 0,
            after: None,
        },
    )
    .map_err(failure)?;
    if board
        .tasks
        .iter()
        .any(|task| task.held_by(&board.principal).is_some())
    {
        return Ok(());
    }
    Err(refuse(
        "task::origin-required",
        Some(board.principal.to_string()),
        "supply --origin ack:<steering-key> or --origin task:<parent-born-stamp>, import while holding one active claim, or as the operator import every task as a Draft with --draft",
    ))
}

fn submit_import(
    host: Host<'_>,
    set: &TaskSet,
    order: Vec<usize>,
    origin: Option<wire::Origin>,
    draft: bool,
    input: String,
    digest: Digest,
) -> Outcome<TaskSetImport> {
    let contract =
        crate::cli::contract(&["task", "add"]).expect("the task add contract is registered");
    let mut handles = BTreeMap::new();
    let mut imported = Vec::with_capacity(set.tasks.len());
    let mut committed: Option<SeqRange> = None;
    for index in order {
        let definition = &set.tasks[index];
        let add = match imported_add(definition, &handles, origin.clone(), draft) {
            Ok(add) => add,
            Err(outcome) => return outcome,
        };
        match submit_task_add(host, contract, add) {
            Outcome::Reply(Reply {
                position: ReplyPosition::Committed(range),
                body: CommandAnswer::Task(answer),
            }) => {
                committed = Some(match committed {
                    Some(previous) => SeqRange {
                        first: previous.first,
                        last: range.last,
                    },
                    None => range,
                });
                handles.insert(definition.id, answer.task.clone());
                imported.push(ImportedTask {
                    id: definition.id,
                    task: answer.task,
                });
            }
            Outcome::Reply(reply) => {
                return partial_fault(
                    imported.len(),
                    definition.id,
                    "task add returned another answer or position shape",
                    reply.position,
                );
            }
            Outcome::Refusal(refusal) if imported.is_empty() => {
                return Outcome::Refusal(refusal);
            }
            Outcome::Refusal(refusal) => {
                return refuse(
                    "task::import-partial",
                    Some(format!(
                        "created {} of {} tasks; local task {} refused with {}",
                        imported.len(),
                        set.tasks.len(),
                        definition.id,
                        refusal.reason()
                    )),
                    if origin.is_some() {
                        "the created prefix remains durable; inspect it by the import origin before retrying"
                    } else {
                        "the created prefix remains durable; inspect the created tasks before retrying"
                    },
                );
            }
            Outcome::Fault(fault) if imported.is_empty() => return Outcome::Fault(fault),
            Outcome::Fault(failure) => {
                return fault(
                    failure.class,
                    "task::import-partial",
                    format!(
                        "created {} of {} tasks before local task {} failed with {}: {}",
                        imported.len(),
                        set.tasks.len(),
                        definition.id,
                        failure.reason,
                        failure.detail
                    ),
                );
            }
        }
    }
    imported.sort_by_key(|task| task.id);
    let mut text = format!(
        "imported {} tasks from {input} digest {digest}",
        imported.len()
    );
    for task in &imported {
        text.push_str(&format!("\n{} {}", task.id, task.task));
    }
    Outcome::Reply(Reply {
        position: committed.map_or(ReplyPosition::Direct {}, ReplyPosition::Committed),
        body: TaskSetImport {
            input,
            digest,
            tasks: imported,
            text,
        },
    })
}

fn project(board: &BoardAnswer) -> Result<(TaskSet, usize), Outcome<TaskSetExport>> {
    let (portable, excluded) = portable_tasks(board);
    if portable.len() > TASK_SET_MAX_TASKS {
        return Err(refuse(
            "task::export-too-many",
            Some(portable.len().to_string()),
            format!("portable task sets admit at most {TASK_SET_MAX_TASKS} tasks"),
        ));
    }
    let ids = portable
        .iter()
        .enumerate()
        .map(|(index, task)| {
            (
                task.statement.body.born.clone(),
                u32::try_from(index + 1).expect("task-set bound fits u32"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut tasks = Vec::with_capacity(portable.len());
    for (index, task) in portable.into_iter().enumerate() {
        let statement = &task.statement.body;
        debug_assert!(statement.priority.is_none());
        let depends = statement
            .dependencies
            .iter()
            .map(|dependency| {
                ids.get(dependency).copied().ok_or_else(|| {
                    refuse(
                        "task::export-reference-missing",
                        Some(statement.handle.to_string()),
                        format!("dependency {dependency} is outside the portable task set"),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let preparation = statement
            .preparation
            .as_ref()
            .map(|preparation| {
                ids.get(&preparation.parent).copied().ok_or_else(|| {
                    refuse(
                        "task::export-reference-missing",
                        Some(statement.handle.to_string()),
                        format!(
                            "preparation parent {} is outside the portable task set",
                            preparation.parent
                        ),
                    )
                })
            })
            .transpose()?;
        let requirements = authored_requirements(statement).map_err(|detail| {
            fault(
                FaultClass::Protocol,
                "task::export-requirements-invalid",
                detail,
            )
        })?;
        let topic = statement
            .topic
            .path
            .clone()
            .map(wire::TopicInput::from)
            .or_else(|| statement.topic.named.clone());
        tasks.push(TaskSetTask {
            id: u32::try_from(index + 1).expect("task-set bound fits u32"),
            title: task.presentation_title().to_owned(),
            pitch: statement.pitch.clone(),
            topic,
            description: statement.description.clone(),
            term: statement.term.clone(),
            requirements,
            flow: task.workflow.body.flow.clone(),
            depends,
            intent: statement.intent,
            preparation,
            draft: exported_draft(task),
        });
    }
    let set = TaskSet {
        version: TASK_SET_VERSION,
        tasks,
    };
    set.creation_order().map_err(|error| {
        refuse(
            "task::export-nonportable",
            None,
            format!("current board cannot form a portable task set: {error}"),
        )
    })?;
    Ok((set, excluded))
}

fn portable_tasks(board: &BoardAnswer) -> (Vec<&wire::intent::BoardTask>, usize) {
    let indexes = board
        .tasks
        .iter()
        .enumerate()
        .map(|(index, task)| (task.statement.body.born.clone(), index))
        .collect::<BTreeMap<_, _>>();
    let mut included = board
        .tasks
        .iter()
        .map(intrinsically_portable)
        .collect::<Vec<_>>();
    let mut children = vec![Vec::new(); board.tasks.len()];
    let mut dependency_children = vec![Vec::new(); board.tasks.len()];
    let mut dependency_incoming = vec![0_usize; board.tasks.len()];
    for (index, task) in board.tasks.iter().enumerate() {
        let statement = &task.statement.body;
        for dependency in &statement.dependencies {
            if let Some(parent) = indexes.get(dependency) {
                children[*parent].push(index);
                dependency_children[*parent].push(index);
                dependency_incoming[index] += 1;
            } else {
                included[index] = false;
            }
        }
        if let Some(preparation) = &statement.preparation {
            if let Some(parent) = indexes.get(&preparation.parent) {
                children[*parent].push(index);
            } else {
                included[index] = false;
            }
        }
    }
    let mut ready = dependency_incoming
        .iter()
        .enumerate()
        .filter_map(|(index, incoming)| (*incoming == 0).then_some(index))
        .collect::<VecDeque<_>>();
    let mut advances = vec![false; board.tasks.len()];
    while let Some(index) = ready.pop_front() {
        let task = &board.tasks[index];
        advances[index] = included[index]
            && !exported_draft(task)
            && !task.statement.body.term.is_empty()
            && task.statement.body.dependencies.iter().all(|dependency| {
                indexes
                    .get(dependency)
                    .is_some_and(|parent| advances[*parent])
            });
        for child in dependency_children[index].iter().copied() {
            dependency_incoming[child] -= 1;
            if dependency_incoming[child] == 0 {
                ready.push_back(child);
            }
        }
    }
    for (index, task) in board.tasks.iter().enumerate() {
        if let Some(preparation) = &task.statement.body.preparation {
            let parent = indexes.get(&preparation.parent).copied();
            if !parent
                .is_some_and(|parent| advances[parent] && planning_context(&board.tasks[parent]))
            {
                included[index] = false;
            }
        }
    }
    let mut excluded = included
        .iter()
        .enumerate()
        .filter_map(|(index, included)| (!included).then_some(index))
        .collect::<VecDeque<_>>();
    while let Some(parent) = excluded.pop_front() {
        for child in children[parent].iter().copied() {
            if included[child] {
                included[child] = false;
                excluded.push_back(child);
            }
        }
    }
    let portable = board
        .tasks
        .iter()
        .zip(&included)
        .filter_map(|(task, included)| included.then_some(task))
        .collect::<Vec<_>>();
    let excluded = board.tasks.len() - portable.len();
    (portable, excluded)
}

fn intrinsically_portable(task: &wire::intent::BoardTask) -> bool {
    let statement = &task.statement.body;
    let topic = statement
        .topic
        .path
        .clone()
        .map(wire::TopicInput::from)
        .or_else(|| statement.topic.named.clone());
    statement.priority.is_none()
        && statement.intent != Some(wire::TaskIntent::OpportunisticDesign)
        && !topic.as_ref().is_some_and(|topic| match topic {
            wire::TopicInput::Absolute(topic) => wire::is_oops_topic(topic),
            wire::TopicInput::Private(_) => true,
            wire::TopicInput::Tail(_) => false,
        })
}

fn authored_requirements(
    statement: &wire::intent::BoardStatement,
) -> Result<EnvironmentRequirements, String> {
    let mut requirements = statement.requirements.as_slice().to_vec();
    for (name, shards) in statement.resources.iter() {
        requirements.push(
            EnvironmentRequirement::resource_pool(name.clone(), shards)
                .map_err(|error| error.to_string())?,
        );
    }
    EnvironmentRequirements::new(requirements).map_err(|error| error.to_string())
}

fn exported_draft(task: &wire::intent::BoardTask) -> bool {
    task.workflow.body.is_draft() && !task.workflow.body.submitted
}

fn planning_context(task: &wire::intent::BoardTask) -> bool {
    let statement = &task.statement.body;
    statement.intent.is_some()
        || (statement.intent.is_none()
            && task.workflow.body.flow.contains(&wire::Phase::Plan)
            && task.workflow.body.flow.iter().all(|phase| {
                matches!(
                    phase,
                    wire::Phase::Draft | wire::Phase::Plan | wire::Phase::Review
                )
            }))
        || task
            .workflow
            .body
            .flow
            .get(1)
            .is_some_and(|phase| matches!(phase, wire::Phase::Plan | wire::Phase::Design))
}

fn imported_add(
    task: &TaskSetTask,
    handles: &BTreeMap<u32, TaskHandle>,
    origin: Option<wire::Origin>,
    draft: bool,
) -> Result<TaskAdd, Outcome<TaskSetImport>> {
    let depends = task
        .depends
        .iter()
        .map(|id| {
            handles.get(id).cloned().ok_or_else(|| {
                fault(
                    FaultClass::Protocol,
                    "task::import-order-invalid",
                    format!(
                        "local dependency {id} was not created before task {}",
                        task.id
                    ),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let preparation = task
        .preparation
        .map(|id| {
            handles.get(&id).cloned().ok_or_else(|| {
                fault(
                    FaultClass::Protocol,
                    "task::import-order-invalid",
                    format!(
                        "local preparation {id} was not created before task {}",
                        task.id
                    ),
                )
            })
        })
        .transpose()?;
    Ok(TaskAdd {
        title: task.title.clone(),
        pitch: task.pitch.clone(),
        topic: task.topic.clone(),
        description: task.description.clone(),
        term: task.term.clone(),
        requirements: task.requirements.clone(),
        origin,
        flow: Some(
            task.flow
                .iter()
                .map(|phase| phase.as_str())
                .collect::<Vec<_>>()
                .join(","),
        ),
        depends,
        intent: task.intent,
        preparation,
        claim: false,
        draft: task.draft || draft,
    })
}

fn parse_import_origin(
    matches: &ArgMatches,
) -> Result<Option<wire::Origin>, Outcome<TaskSetImport>> {
    matches
        .get_one::<String>("origin")
        .map(|written| {
            written.parse::<wire::Origin>().map_err(|error| {
                refuse(
                    "task::origin-invalid",
                    Some(written.clone()),
                    error.to_string(),
                )
            })
        })
        .transpose()
}

fn read_input(path: &Path) -> Result<(PathBuf, Vec<u8>), Outcome<TaskSetImport>> {
    if path == Path::new("-") {
        return Err(refuse(
            "task::import-input-refused",
            Some("-".to_owned()),
            "name one regular task-set file; standard input has no stable identity",
        ));
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        fault(
            FaultClass::Environment,
            "task::import-input-unavailable",
            format!("{}: {error}", path.display()),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(refuse(
            "task::import-input-refused",
            Some(path.display().to_string()),
            "name one regular file, not a link or another filesystem object",
        ));
    }
    if metadata.len() > TASK_SET_MAX_BYTES {
        return Err(refuse(
            "task::import-too-large",
            Some(metadata.len().to_string()),
            format!("portable task sets admit at most {TASK_SET_MAX_BYTES} encoded bytes"),
        ));
    }
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        fault(
            FaultClass::Environment,
            "task::import-input-unavailable",
            format!("{}: {error}", path.display()),
        )
    })?;
    let mut file = File::open(&canonical).map_err(|error| {
        fault(
            FaultClass::Environment,
            "task::import-input-unavailable",
            format!("{}: {error}", canonical.display()),
        )
    })?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(TASK_SET_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            fault(
                FaultClass::Environment,
                "task::import-input-unreadable",
                format!("{}: {error}", canonical.display()),
            )
        })?;
    if bytes.len() as u64 > TASK_SET_MAX_BYTES {
        return Err(refuse(
            "task::import-too-large",
            Some(bytes.len().to_string()),
            format!("portable task sets admit at most {TASK_SET_MAX_BYTES} encoded bytes"),
        ));
    }
    Ok((canonical, bytes))
}

fn partial_fault(
    imported: usize,
    task: u32,
    detail: &str,
    position: ReplyPosition,
) -> Outcome<TaskSetImport> {
    fault(
        FaultClass::Protocol,
        "task::import-partial",
        format!(
            "created {imported} tasks before local task {task} failed: {detail}; position={position:?}"
        ),
    )
}

fn failure<T>(outcome: Outcome<CommandAnswer>) -> Outcome<T> {
    match outcome {
        Outcome::Reply(_) => unreachable!("a failure conversion cannot carry a reply"),
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

fn refuse<T>(
    reason: impl Into<String>,
    observed: Option<String>,
    detail: impl Into<String>,
) -> Outcome<T> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(reason, None, None, observed).with_detail(detail),
    ))
}

fn fault<T>(class: FaultClass, reason: impl Into<String>, detail: impl Into<String>) -> Outcome<T> {
    Outcome::Fault(Fault {
        class,
        reason: reason.into(),
        detail: detail.into(),
    })
}
