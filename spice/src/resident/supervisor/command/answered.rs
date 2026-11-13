//! Keys an occupant answered in tape-backed assistant text before the journal retired them.

use std::collections::BTreeMap;

/// Each key the occupant's own tape-backed assistant text answered, with the
/// stage its telling named when they did.
///
/// The resident retires a key only once the answer reaches its journal, and
/// until then every inbox read still lists it. A reminder, replay, or briefing
/// composed from such a read would ask again for an answer already given, so
/// composition holds these keys back. The hold ends when the inbox stops
/// listing the key, or lists it at another stage: the resident climbed its
/// resend ladder after the answer, so the answer retired nothing and the key
/// is owed again.
#[derive(Debug, Default)]
pub(super) struct Answered(BTreeMap<String, String>);

impl Answered {
    /// Hold `key`, answered while `rendered` listed it. A key the rendering
    /// does not list is the journal's to judge and holds nothing.
    pub(super) fn record(&mut self, rendered: &str, key: &str) {
        if let Some(stage) = steer::inbox::stage(rendered, key) {
            self.0.insert(key.to_owned(), stage.to_owned());
        }
    }

    /// Release every key `rendered` no longer lists at its answered stage.
    pub(super) fn settle(&mut self, rendered: &str) {
        self.0
            .retain(|key, stage| steer::inbox::stage(rendered, key) == Some(stage.as_str()));
    }

    pub(super) fn holds(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }
}

#[cfg(test)]
mod tests {
    use super::Answered;

    fn rendering(stage: &str) -> String {
        format!(
            "<1kJK6N3Rx>\ndirective=d1 key=1kJK6M0Vx stage={stage} expires=never\nship it\n\
             </1kJK6N3Rx>\n"
        )
    }

    #[test]
    fn an_answer_holds_its_key_while_the_journal_still_lists_it_at_that_stage() {
        let mut answered = Answered::default();
        answered.record(&rendering("published"), "1kJK6M0Vx");
        answered.settle(&rendering("published"));
        assert!(answered.holds("1kJK6M0Vx"));

        answered.settle("");
        assert!(
            !answered.holds("1kJK6M0Vx"),
            "a retired key is no longer held"
        );
    }

    #[test]
    fn a_later_stage_releases_the_key_the_answer_did_not_retire() {
        let mut answered = Answered::default();
        answered.record(&rendering("published"), "1kJK6M0Vx");
        answered.settle(&rendering("reminded"));
        assert!(!answered.holds("1kJK6M0Vx"));
    }

    #[test]
    fn an_answer_to_a_key_the_rendering_never_listed_holds_nothing() {
        let mut answered = Answered::default();
        answered.record(&rendering("published"), "1kJK6N3Rx");
        assert!(!answered.holds("1kJK6N3Rx"));
    }
}
