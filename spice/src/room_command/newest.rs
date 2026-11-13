//! A task's room read newest first: the conversation `task show` and a claim
//! answer page under their own row and byte bounds.

use super::*;

/// The newest publications of one room from a starting publication back,
/// with how many the room holds from that point back.
pub(crate) struct Newest {
    pub(crate) population: usize,
    pub(crate) publications: Vec<Publication>,
}

/// [`Newest`] as one journal snapshot records it, before its sources are read.
pub(crate) struct NewestRows {
    population: usize,
    rows: Vec<Row>,
}

/// At most `retain` publications of `room` inside `read`, newest first,
/// starting at `from` inclusive or at the newest; `None` when `from` names no
/// publication of `room`.
pub(crate) fn newest_rows(
    read: &journal::JournalRead<'_>,
    room: &TaskStamp,
    from: Option<&RoomAuthorityStamp>,
    retain: usize,
) -> Result<Option<NewestRows>, journal::GuardError> {
    let total = read.room_publication_count(room, None)?;
    let (population, mut before) = match from {
        None => (total, None),
        Some(publication) => {
            let Some(seq) = read.room_publication_seq(room, publication)? else {
                return Ok(None);
            };
            (
                total - read.room_publication_count(room, Some(seq))?,
                Seq::new(seq.get() + 1),
            )
        }
    };
    let roles = read.principal_roles()?;
    let principals = PrincipalDirectory::codes(&roles);
    let mut rows = Vec::new();
    while rows.len() < retain {
        let budget = journal::FactReadBudget::new(
            (retain - rows.len()).min(journal::FactReadBudget::RETAINED.rows()),
            journal::FactReadBudget::RETAINED.decoded_bytes(),
        )?;
        let page = read.room_publications_newest(room, before, budget)?;
        for envelope in page.rows {
            rows.push(row(read, &principals, envelope).map_err(Failure::guard)?);
        }
        match page.continuation {
            Some(seq) => before = Some(seq),
            None => break,
        }
    }
    Ok(Some(NewestRows { population, rows }))
}

impl NewestRows {
    /// These publications with their bytes read from the sources `owners`
    /// names.
    pub(crate) fn project(self, owners: SourceOwners<'_>, room: &TaskStamp) -> Newest {
        let mut sources = RoomSources::new(Some(owners), room);
        Newest {
            population: self.population,
            publications: self
                .rows
                .into_iter()
                .map(|row| row.project(owners.tape, &mut sources))
                .collect(),
        }
    }
}

impl Publication {
    /// This publication as one detail row: the cursor that resumes at it and
    /// its rendering with its authored text clipped under `budget`.
    pub(crate) fn detail_row(mut self, budget: wire::output::Budget) -> (String, String) {
        if let Content::Text { text } | Content::Contribution { text, .. } = &mut self.content {
            *text = budget.clip(text).to_string();
        }
        (format!("room:{}", self.publication), self.to_string())
    }
}
