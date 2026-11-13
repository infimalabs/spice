//! Text presented by nested directive Quote and Play controls.

use std::borrow::Cow;

use super::{directive_context_prose, first_nonquoted_prose};

/// Keep ordinary directive controls on their quoted source span, but make a
/// typed review's controls follow the authenticated card instead of its
/// discarded delivery envelope.
pub(crate) fn directive_control_prose<'a>(
    review: Option<&wire::MessageReview>,
    envelope: &'a str,
) -> (Cow<'a, str>, Option<String>) {
    match review {
        Some(review) => (
            Cow::Owned(format!(
                "{}\n\n{}\n\n{}",
                review.task, review.finding, review.guidance
            )),
            first_nonquoted_prose(&review.finding),
        ),
        None => (Cow::Borrowed(envelope), directive_context_prose(envelope)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_review_controls_never_present_the_delivery_envelope() {
        let review = wire::MessageReview {
            task: "APP-1kKRRsDRx".parse().expect("fixture task"),
            finding: "typed missing coverage\n\n> nested typed evidence".to_owned(),
            guidance: "Follow-ups are already boarded; continue the current task.".to_owned(),
        };
        let (text, preview) = directive_control_prose(
            Some(&review),
            "> APP-1kKRRsDRx\n>\n> > stale finding\n>\n> stale guidance",
        );

        assert_eq!(
            text.as_ref(),
            concat!(
                "APP-1kKRRsDRx\n\n",
                "typed missing coverage\n\n> nested typed evidence\n\n",
                "Follow-ups are already boarded; continue the current task."
            )
        );
        assert_eq!(preview.as_deref(), Some("typed missing coverage"));
        assert!(!text.contains("stale"));

        assert_eq!(
            directive_control_prose(None, "> > borrowed\n>\n> authored direction"),
            (
                Cow::Borrowed("> > borrowed\n>\n> authored direction"),
                Some("authored direction".to_owned()),
            )
        );
    }
}
