use super::*;

/// The author validation a reviewer must assess, without expanding unrelated
/// history. The most recent completed phase is the one that entered this
/// review round.
pub(super) fn preceding_validation_lines(
    task: &BoardTask,
    history: TaskShowHistory<'_>,
) -> Vec<String> {
    if task.workflow.body.phase != wire::Phase::Review {
        return Vec::new();
    }
    let Some((phase, validation)) = history.steps.iter().rev().find_map(|step| {
        let wire::intent::TaskStep::Done {
            phase, validation, ..
        } = &step.step
        else {
            return None;
        };
        Some((phase, validation.as_str()))
    }) else {
        return Vec::new();
    };
    let mut lines = vec![format!("validation {phase}:")];
    if validation.is_empty() {
        lines.push("  -".to_owned());
    } else {
        lines.extend(validation.lines().map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("  {line}")
            }
        }));
    }
    lines
}
