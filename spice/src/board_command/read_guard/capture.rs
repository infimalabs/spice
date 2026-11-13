use super::*;

pub(in crate::board_command) fn guard_capture(
    write: &mut JournalWrite,
    expected: Vec<Witness>,
    cursor: Seq,
    actor: Actor,
) {
    write.guard(JournalGuard::new(move |read| {
        witnesses_hold(read, &expected)?;
        if read.cursor()? != Some(cursor) {
            return Err(GuardError::refused(
                "board::witness-moved: capture projection changed",
            ));
        }
        refuse_active_claim_conflict(read, &actor)
    }));
}
