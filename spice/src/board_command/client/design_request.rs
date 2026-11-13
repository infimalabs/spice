//! The private Design request `spice task next` answers before allocating.

use super::*;

pub(super) fn design_request_reply(
    board: &BoardAnswer,
    request: &wire::intent::DesignRequest,
) -> Reply<CommandAnswer> {
    let text = spice::rooms::awareness::request_text(request);
    Reply {
        position: ReplyPosition::Snapshot(board.cursor),
        body: CommandAnswer::Text(TextAnswer { text }),
    }
}
