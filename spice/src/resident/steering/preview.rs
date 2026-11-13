//! Producer-owned directive previews for crossing the resident boundary.

/// The first line this directive's author wrote outside quoted context.
///
/// Steering text routinely opens by quoting earlier conversation. The preview
/// belongs to the resident because it has the complete durable text before the
/// wire DTO crosses; clients display the field and never reinterpret the body.
pub(super) fn directive_preview(text: &str) -> Option<String> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    normalized
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('>'))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_is_the_first_line_authored_outside_quoted_context() {
        for text in [
            "> > older reply\n> earlier reply\n\nwhat I actually said\nand another line",
            "> nested\r\nwhat I actually said",
            "> nested\rwhat I actually said",
            "\t> nested\n\twhat I actually said",
        ] {
            assert_eq!(
                directive_preview(text).as_deref(),
                Some("what I actually said"),
                "from {text:?}"
            );
        }
    }

    #[test]
    fn entirely_quoted_or_blank_text_has_no_preview() {
        for text in ["> only\n> quoted\n>\n   > indented too", "   \n\n  ", ""] {
            assert_eq!(directive_preview(text), None, "from {text:?}");
        }
    }
}
