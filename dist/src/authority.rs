//! Admission of tracked single-line distribution text.

use crate::version::TrackedVersionError;

pub(crate) fn tracked_text_line<'a>(
    text: &'a str,
    label: &'static str,
) -> Result<&'a str, AuthorityError> {
    let line = text
        .strip_suffix('\n')
        .ok_or(AuthorityError::TrackedLine(label))?;
    if line.contains('\n') || line.contains('\r') {
        Err(AuthorityError::TrackedLine(label))
    } else {
        Ok(line)
    }
}

/// Why tracked distribution text was refused.
#[derive(Debug, thiserror::Error)]
pub enum AuthorityError {
    #[error("tracked {0} must be exactly one LF-terminated line")]
    TrackedLine(&'static str),
    #[error(transparent)]
    Version(#[from] TrackedVersionError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracked_text_is_exactly_one_lf_terminated_line() {
        assert_eq!(tracked_text_line("1\n", "version").ok(), Some("1"));
        for refused in ["1", "1\n\n", "1\r\n"] {
            assert!(
                tracked_text_line(refused, "version").is_err(),
                "{refused:?}"
            );
        }
    }
}
