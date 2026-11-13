//! Complete steering views assembled from fixed indexed pages.

pub(crate) fn pending(
    read: &journal::JournalRead<'_>,
    lane: Option<wire::CanonicalUuid>,
) -> Result<Vec<journal::JournalHead<wire::SteeringHead>>, journal::GuardError> {
    let recipient = lane.map(wire::Actor::Lane);
    let mut heads = Vec::new();
    let mut start = journal::FactPageStart::After(None);
    loop {
        let page = read.pending_steering(recipient.as_ref(), start)?;
        let continuation = page.overflow().map(|overflow| overflow.continuation);
        heads.extend(page.into_rows());
        let Some(continuation) = continuation else {
            return Ok(heads);
        };
        start = journal::FactPageStart::At(continuation);
    }
}
