//! Read-only app access to documentation in the resident's linked rack.

use bus::Bus;
use wire::intent::{CommandAnswer, RackRead};
use wire::{Outcome, Refusal, Reply, ReplyPosition};

use super::store_fault;

/// Read one bounded docs directory or member at the current journal horizon.
///
/// The reply cursor and the rack's admission code come from one bus read, so
/// the answer's identity is the generation that horizon admits and no second
/// journal connection opens.
pub(super) fn read(bus: &Bus, request: &RackRead) -> Outcome<CommandAnswer> {
    let (linked, archive) = match crate::resource::linked_for_app() {
        Ok(linked) => linked,
        Err(error) => return refused(request, error),
    };
    let (cursor, generation) = match bus
        .read(move |read| Ok((read.cursor()?, spice::rack_admission::code(read, archive)?)))
    {
        Ok((Some(cursor), generation)) => (cursor, generation),
        Ok((None, _)) => {
            return store_fault(
                "rack::journal-empty",
                "a running resident has no startup authority fact",
            );
        }
        Err(error) => {
            return store_fault("rack::journal-unreadable", &error.to_string());
        }
    };
    match crate::resource::read_app_document(linked.rack(), archive.clone(), generation, request) {
        Ok(answer) => Outcome::Reply(Reply {
            position: ReplyPosition::Snapshot(cursor),
            body: CommandAnswer::Rack(Box::new(answer)),
        }),
        Err(error) => refused(request, error),
    }
}

fn refused(request: &RackRead, error: String) -> Outcome<CommandAnswer> {
    Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
        "rack::read-refused".to_owned(),
        Some(request.path.clone()),
        Some("a normalized supported member beneath docs".to_owned()),
        Some(error),
    )))
}
