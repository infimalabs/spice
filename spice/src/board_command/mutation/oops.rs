use super::*;
fn oops_snapshot(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    add: &TaskOopsAdd,
    repository: Option<&Repository>,
) -> Result<(ValidatedSnapshot, bool, wire::OopsGenerationHead), Outcome<CommandAnswer>> {
    let observed = bus
        .task_snapshot()
        .map_err(|error| store_fault("task::oops-heads-unreadable", error.to_string()))?;
    let (generation_head, witnessed_design) = command
        .expected()
        .iter()
        .find_map(|witness| match witness {
            Witness::OopsGeneration { head, design } => Some((*head, design.clone())),
            _ => None,
        })
        .ok_or_else(|| {
            protocol_fault(
                "task::oops-generation-witness-missing",
                "OOPS intake requires one repository generation witness",
            )
        })?;
    if generation_head.is_none() && witnessed_design.is_some() {
        return Err(protocol_fault(
            "task::oops-generation-witness-invalid",
            "an OOPS generation cannot name a Design before its first head",
        ));
    }
    if witnessed_design
        .as_ref()
        .is_some_and(|design| design != &add.design)
    {
        return Err(protocol_fault(
            "task::oops-generation-design-mismatch",
            "the OOPS request must name the Design carried by its generation witness",
        ));
    }
    let generation = observed
        .oops_generation()
        .filter(|head| {
            Some(head.position()) == generation_head && head.body().design == witnessed_design
        })
        .map(|head| head.body().clone())
        .unwrap_or(wire::OopsGenerationHead {
            revision: 0,
            design: witnessed_design.clone(),
        });
    let design = witnessed_design
        .as_ref()
        .map(|design| {
            observed.task_heads(design).ok_or_else(|| {
                protocol_fault(
                    "task::oops-generation-task-missing",
                    "the witnessed OOPS generation names a task absent from the board",
                )
            })
        })
        .transpose()?;
    if let Some(heads) = design
        && (heads.statement().body().intent != Some(wire::TaskIntent::OpportunisticDesign)
            || !wire::intent::is_open_design(
                heads.statement().body().intent,
                heads.workflow().body(),
            ))
    {
        return Err(protocol_fault(
            "task::oops-generation-invalid",
            "the witnessed OOPS generation does not name one open opportunistic Design",
        ));
    }
    let snapshot = validated_working_snapshot_for_tasks(
        bus,
        repository,
        command.actor().clone(),
        std::slice::from_ref(&add.design),
    )?;
    if design.is_some_and(|heads| heads.statement().body().born != add.design) {
        return Err(protocol_fault(
            "task::oops-generation-design-mismatch",
            "the OOPS request must name the Design carried by its generation witness",
        ));
    }
    if let Some(heads) = design {
        require_oops_design_witnesses(command, heads)?;
    }
    let creates_design = design.is_none();
    if creates_design
        && !command
            .expected()
            .iter()
            .any(|witness| matches!(witness, Witness::Absence { task } if task == &add.design))
    {
        return Err(protocol_fault(
            "task::oops-design-witness-missing",
            "opening an opportunistic OOPS Design requires its intended absent birth stamp",
        ));
    }
    Ok((snapshot, creates_design, generation))
}

pub(in crate::board_command) fn task_oops_add(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    add: &TaskOopsAdd,
    repository: Option<Host<'_>>,
) -> Outcome<CommandAnswer> {
    let (snapshot, creates_design, generation) =
        match oops_snapshot(bus, command, add, repository.map(|host| host.repository)) {
            Ok(snapshot) => snapshot,
            Err(outcome) => return outcome,
        };
    let report_born = command
        .expected()
        .iter()
        .filter_map(|witness| match witness {
            Witness::Absence { task } if task != &add.design => Some(task.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [report_born] = report_born.as_slice() else {
        return protocol_fault(
            "task::oops-report-witness-invalid",
            "OOPS intake requires exactly one intended absent report birth stamp",
        );
    };
    let report_title = wire::title::fit(&add.title);
    let report_pitch = wire::pitch::derive(&report_title, None);
    let default_topic = add.topic.is_none().then(|| {
        wire::OOPS_TOPIC
            .parse::<wire::TopicInput>()
            .expect("fixed OOPS report topic")
    });
    let origin =
        match resolve_creation_origin(add.origin.as_ref(), command.actor(), &snapshot, None, false)
        {
            Ok(origin) => origin,
            Err(outcome) => return outcome,
        };
    let origin_view = match super::super::routing::OriginView::of(bus, &origin.provenance) {
        Ok(view) => view,
        Err(outcome) => return outcome,
    };
    let report_phase = wire::Phase::Draft;
    let placement = match placement::place_oops_creation(
        bus,
        &snapshot,
        command.actor(),
        add.topic.as_ref().or(default_topic.as_ref()),
        super::super::routing::Statement::new(
            &report_title,
            &report_pitch,
            None,
            &[],
            &report_phase,
        ),
        origin_view.clone(),
        None,
    ) {
        Ok(placement) => placement,
        Err(outcome) => return outcome,
    };
    let creation = match task_creation_capture(bus, command, &snapshot) {
        Ok(creation) => creation,
        Err(outcome) => return outcome,
    };
    let design = if creates_design {
        match activated_oops_design(
            bus,
            command,
            &snapshot,
            repository,
            add,
            origin.clone(),
            creation.clone(),
            origin_view,
        ) {
            Ok(design) => Some(design),
            Err(outcome) => return outcome,
        }
    } else {
        None
    };
    let report = match prepare_oops_report(
        add,
        &report_title,
        &report_pitch,
        placement.topic.clone(),
        origin,
        creation,
        report_born,
    ) {
        Ok(report) => report,
        Err(outcome) => return outcome,
    };
    commit_oops_intake(
        bus,
        command,
        add,
        &snapshot,
        OopsRows { design, report },
        generation,
    )
}

/// The Design a first report of a generation opens: prepared under the fixed
/// Design topic, placed, and run through the phase-zero validator.
#[expect(
    clippy::too_many_arguments,
    reason = "one Design birth names every authority the intake witnessed"
)]
fn activated_oops_design(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    snapshot: &ValidatedSnapshot,
    repository: Option<Host<'_>>,
    add: &TaskOopsAdd,
    origin: CreationCause,
    creation: TaskCreationCapture,
    origin_view: Option<super::super::routing::OriginView>,
) -> Result<PreparedTask, Outcome<CommandAnswer>> {
    let design_topic = wire::OOPS_DESIGN_TOPIC
        .parse::<wire::Topic>()
        .map_err(|error| store_fault("task::oops-design-topic-refused", error.to_string()))?;
    let mut design = prepare_oops_design(add, design_topic, origin, creation)?;
    let placed = placement::place_oops_creation(
        bus,
        snapshot,
        command.actor(),
        design.statement.topic.named.as_ref(),
        super::super::routing::Statement::task(&design.statement, &design.workflow.phase),
        origin_view,
        None,
    )?;
    design.activate(snapshot, command, repository, false, placed, &[])?;
    Ok(design)
}

/// The rows one oops intake is born as: its Plan report and, when the
/// current generation has no open Design, the Design it opens.
struct OopsRows {
    design: Option<PreparedTask>,
    report: PreparedTask,
}

fn prepare_oops_design(
    add: &TaskOopsAdd,
    design_topic: wire::Topic,
    cause: CreationCause,
    creation: TaskCreationCapture,
) -> Result<PreparedTask, Outcome<CommandAnswer>> {
    let design_add = TaskAdd {
        preparation: None,
        intent: Some(wire::TaskIntent::OpportunisticDesign),
        title: wire::OOPS_DESIGN_TITLE.to_owned(),
        pitch: wire::OOPS_DESIGN_PITCH.to_owned(),
        topic: Some(wire::TopicInput::Absolute(design_topic.clone())),
        description: Some(wire::OOPS_DESIGN_DESCRIPTION.to_owned()),
        term: wire::OOPS_DESIGN_TERM.map(str::to_owned).to_vec(),
        requirements: wire::EnvironmentRequirements::default(),
        origin: add.origin.clone(),
        flow: Some("design".to_owned()),
        depends: Vec::new(),
        related: Vec::new(),
        claim: false,
        draft: false,
    };
    prepare_task_for(
        &design_add,
        design_topic.into(),
        TaskSeed {
            cause,
            creation,
            flow: vec![
                wire::Phase::Draft,
                "design".parse().expect("design is a phase"),
            ],
        },
        add.design.clone(),
    )
}

/// The report row: a title cut to the bound when the report text runs past
/// it, with the complete report text kept as its description.
fn prepare_oops_report(
    add: &TaskOopsAdd,
    title: &str,
    pitch: &str,
    topic: wire::TaskTopic,
    cause: CreationCause,
    creation: TaskCreationCapture,
    report_born: &wire::TaskStamp,
) -> Result<PreparedTask, Outcome<CommandAnswer>> {
    let report_add = TaskAdd {
        preparation: None,
        intent: Some(wire::TaskIntent::Plan),
        title: title.to_owned(),
        // A friction report must not be refused, so its pitch derives once
        // from the report text instead of being asked for.
        pitch: pitch.to_owned(),
        topic: topic.named.clone(),
        description: (title != add.title).then(|| add.title.clone()),
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        origin: add.origin.clone(),
        flow: Some("plan,review".to_owned()),
        depends: Vec::new(),
        related: Vec::new(),
        claim: false,
        draft: true,
    };
    let mut report = prepare_task_for(
        &report_add,
        topic,
        TaskSeed {
            cause,
            creation,
            flow: vec![wire::Phase::Draft, wire::Phase::Plan, wire::Phase::Review],
        },
        report_born.clone(),
    )?;
    report.statement.preparation = Some(wire::TaskPreparation {
        parent: add.design.clone(),
        controller: add.design.clone(),
        construction: report_born.clone(),
    });
    Ok(report)
}

fn require_oops_design_witnesses(
    command: &wire::AdmittedCommand,
    heads: &journal::TaskHeads,
) -> Result<(), Outcome<CommandAnswer>> {
    planning::require_expected(
        command,
        &Witness::Task {
            task: heads.statement().body().born.clone(),
            head: heads.statement().position(),
        },
    )?;
    planning::require_expected(
        command,
        &Witness::Workflow {
            task: heads.statement().body().born.clone(),
            head: heads.workflow().position(),
            phase: heads.workflow().body().phase,
        },
    )
}
/// Commit one oops intake: the report, the Design it opens when the current
/// generation has none, and the generation advance binding them.
fn commit_oops_intake(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    request_body: &TaskOopsAdd,
    snapshot: &ValidatedSnapshot,
    rows: OopsRows,
    generation: wire::OopsGenerationHead,
) -> Outcome<CommandAnswer> {
    let OopsRows { design, report } = rows;
    let duplicates = match super::duplicate::screen(bus, command, &report.statement, &[]) {
        Ok(screen) => screen,
        Err(outcome) => return outcome,
    };
    let post = prepared_entry(&report);
    let next = match report.diagnostic() {
        Some(_) if report.placement.is_some() => {
            Some(format!("spice task place {}", report.task).into_boxed_str())
        }
        _ => next_action(snapshot, &post),
    };
    let task = report.task.clone();
    let origin = report.origin.clone();
    let at = report.at;
    let diagnostic = report.diagnostic();
    let status = report.workflow.phase.to_string();
    let mut write = JournalWrite::new(command.actor().clone(), origin, at, Producer::CommandDoor);
    guard(&mut write, command.expected().to_vec());
    snapshot.guard_ownership(&mut write);
    report.guard(&mut write);
    duplicates.guard(&mut write);
    if let Some(design) = &design {
        design.guard(&mut write);
    }
    let mut created = std::collections::BTreeMap::new();
    let cause = match design {
        Some(design) => {
            let design_stamp = design.statement.born.clone();
            let activated = design.advances();
            let design_task = design.task.clone();
            let ownership = snapshot.ownership(&prepared_entry(&design));
            let (added, placement) = match append_prepared_task(&mut write, design, FactCause::Root)
            {
                Ok(result) => result,
                Err(outcome) => return outcome,
            };
            created.insert(
                design_task,
                wire::intent::TaskCreated {
                    ownership,
                    placement,
                },
            );
            if !activated {
                FactCause::Pending(added)
            } else {
                let Some(revision) = generation.revision.checked_add(1) else {
                    return store_fault(
                        "task::oops-generation-exhausted",
                        "the OOPS generation revision cannot advance",
                    );
                };
                let advanced = wire::body::OopsGenerationAdvanced {
                    revision,
                    from: generation.design,
                    to: Some(design_stamp),
                };
                match write.fact(advanced, FactCause::Pending(added)) {
                    Ok(fact) => FactCause::Pending(fact),
                    Err(error) => {
                        return store_fault("task::oops-generation-build", error.to_string());
                    }
                }
            }
        }
        None => FactCause::Root,
    };
    let placement = match append_prepared_task(&mut write, report, cause) {
        Ok((_, placement)) => placement,
        Err(outcome) => return outcome,
    };
    let answer = wire::intent::TaskAnswer {
        diagnostic,
        duplicates: duplicates.warnings,
        duplicates_omitted: duplicates.warnings_omitted,
        ownership: Some(snapshot.ownership(&post)),
        placement,
        created,
        taste: None,
        completion_contract: None,
        task,
        status,
        completion: None,
        claim: None,
        closed: Vec::new(),
        next,
    };
    if let Err(error) = write.command_result(
        request(command, request_body.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        return store_fault("task::oops-add-receipt", error.to_string());
    }
    committed(bus, write, CommandAnswer::Task(Box::new(answer)))
}

/// An opportunistic Design leaving draft activates only the still-empty generation.
pub(super) fn activation_generation(
    bus: &Bus,
    statement: &TaskHead,
) -> Result<Option<(Witness, wire::body::OopsGenerationAdvanced)>, Outcome<CommandAnswer>> {
    if statement.intent != Some(wire::TaskIntent::OpportunisticDesign) {
        return Ok(None);
    }
    let observed = bus
        .task_snapshot()
        .map_err(|error| store_fault("task::oops-heads-unreadable", error.to_string()))?;
    let generation = observed.oops_generation();
    if let Some(design) = generation.and_then(|head| head.body().design.as_ref()) {
        return Err(refusal(
            "task::oops-generation-occupied",
            Some(design.to_string()),
            "finish the active opportunistic Design before activating another",
        ));
    }
    let revision = generation
        .map_or(0, |head| head.body().revision)
        .checked_add(1)
        .ok_or_else(|| {
            store_fault(
                "task::oops-generation-exhausted",
                "the OOPS generation revision cannot advance",
            )
        })?;
    Ok(Some((
        Witness::OopsGeneration {
            head: generation.map(|head| head.position()),
            design: None,
        },
        wire::body::OopsGenerationAdvanced {
            revision,
            from: None,
            to: Some(statement.born.clone()),
        },
    )))
}
