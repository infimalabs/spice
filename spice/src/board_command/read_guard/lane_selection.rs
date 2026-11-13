use super::*;

/// Record in `answer` the lane `code` selects among the residency lanes that
/// `answer` already carries from `observed`, so selection and admission come
/// from one snapshot.
pub(super) fn select_answer_lane(
    observed: &bus::TaskSnapshot,
    answer: &mut BoardAnswer,
    code: wire::OccupantAddress,
) -> Result<(), Outcome<CommandAnswer>> {
    let selection = observed
        .facts()
        .select_lane(&answer.residency.body.lanes, code)
        .map_err(|refused| {
            refusal(
                refused.reason(),
                Some(refused.to_string()),
                "name a lane by any occupant code it has shown; `spice lane list` shows each lane's current code",
            )
        })?;
    answer.selected = Some(selection);
    Ok(())
}
