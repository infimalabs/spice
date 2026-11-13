use super::*;

pub(super) fn run(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    mutate_task_from_board(repository, contract, matches, |_board, task, heads| {
        let apply = matches
            .get_one::<String>("apply")
            .map(|value| value.parse::<Digest>())
            .transpose()
            .map_err(|error| refusal("task::place-digest-invalid", None, error.to_string()))?;
        let expected = vec![statement_witness(heads), workflow_witness(heads)];
        let body = wire::intent::TaskPlace {
            task: task.clone(),
            apply,
        };
        Ok((
            CommandKind::TaskPlace,
            expected,
            serde_json::to_value(body).expect("typed placement intent encodes"),
        ))
    })
}

pub(super) fn dispatch_placement(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    repository: Option<Host<'_>>,
) -> Option<Outcome<CommandAnswer>> {
    Some(match command.body() {
        wire::intent::Intent::TaskAdd(add) => task_add(bus, command, add, repository),
        wire::intent::Intent::TaskFile(file) => task_file(bus, command, file, repository),
        wire::intent::Intent::TaskPlace(place) => {
            super::super::placement::run(bus, command, place, repository)
        }
        _ => return None,
    })
}
