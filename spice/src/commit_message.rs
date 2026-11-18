//! Native commit-message policy used by the installed `commit-msg` hook.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The release-one repository law for commit-message prose.
pub const WRAP_LIMIT: usize = 100;

const PLACEHOLDER_SUBJECTS: &[&str] = &[
    "changes",
    "fixup",
    "misc",
    "squash",
    "stuff",
    "temp",
    "temporary",
    "tmp",
    "todo",
    "update",
    "updates",
    "wip",
    "work in progress",
];
const PLACEHOLDER_TRAILING: &[char] = &[' ', '\t', '.', ',', ':', ';', '!', '?', '-', '_', '#'];

/// Apply the repository's commit-message law to one Git message file.
///
/// Long body prose is folded before validation.
/// The return value says whether the file was rewritten.
///
/// # Errors
///
/// Returns an I/O failure, a hygiene refusal, or Gate's frozen
/// `Spice-Origin` grammar refusal.
pub fn apply(path: &Path, policy: &gate::CommitMessagePolicy) -> Result<bool, CommitMessageError> {
    let message = fs::read_to_string(path).map_err(|source| CommitMessageError::Io {
        action: "read",
        path: path.to_path_buf(),
        source,
    })?;
    let folded = fold(&message);
    let changed = folded != message;
    if changed {
        fs::write(path, folded.as_bytes()).map_err(|source| CommitMessageError::Io {
            action: "write",
            path: path.to_path_buf(),
            source,
        })?;
    }
    validate_hygiene(&folded, policy)?;
    gate::validate_commit_message(path.display().to_string(), &committed_text(&folded))
        .map_err(CommitMessageError::Origin)?;
    Ok(changed)
}

fn fold(message: &str) -> String {
    let mut rendered = Vec::new();
    let mut subject_seen = false;
    let mut passthrough = false;

    for raw in message.lines() {
        if passthrough {
            rendered.push(raw.to_owned());
            continue;
        }
        let line = raw.trim_end();
        if line.contains(">8") {
            passthrough = true;
            rendered.push(raw.to_owned());
        } else if line.starts_with('#') {
            rendered.push(raw.to_owned());
        } else if !subject_seen {
            subject_seen = true;
            rendered.push(line.to_owned());
        } else {
            rendered.extend(fold_body_line(line));
        }
    }

    let mut folded = rendered.join("\n");
    if message.ends_with(['\n', '\r']) {
        folded.push('\n');
    }
    folded
}

fn fold_body_line(line: &str) -> Vec<String> {
    if exempt_from_wrap(line) {
        return vec![line.to_owned()];
    }
    fold_prose(line)
}

/// Fold one line of prose at [`WRAP_LIMIT`], continuing a list item under its
/// marker; a word wider than the room left on its line is split at the limit.
#[must_use]
pub fn fold_prose(line: &str) -> Vec<String> {
    if line.chars().count() <= WRAP_LIMIT {
        return vec![line.to_owned()];
    }

    let subsequent_indent = list_marker_width(line).unwrap_or(0);
    let mut wrapped = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;
    let mut words_on_line = 0;
    let mut cursor = 0;

    while cursor < line.len() {
        let separator_start = cursor;
        while line[cursor..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
        {
            cursor += line[cursor..].chars().next().unwrap().len_utf8();
        }
        let separator = &line[separator_start..cursor];
        let word_start = cursor;
        while line[cursor..]
            .chars()
            .next()
            .is_some_and(|character| !character.is_whitespace())
        {
            cursor += line[cursor..].chars().next().unwrap().len_utf8();
        }
        let word = &line[word_start..cursor];
        if word.is_empty() {
            break;
        }
        let word_width = word.chars().count();
        let separator_width = separator.chars().count();
        if words_on_line > 0 && current_width + separator_width + word_width > WRAP_LIMIT {
            wrapped.push(std::mem::replace(
                &mut current,
                " ".repeat(subsequent_indent),
            ));
            current_width = subsequent_indent;
            words_on_line = 0;
        } else {
            current.push_str(separator);
            current_width += separator_width;
        }
        let mut rest = word;
        let mut rest_width = word_width;
        while current_width + rest_width > WRAP_LIMIT {
            let room = WRAP_LIMIT.saturating_sub(current_width).max(1);
            let split = rest
                .char_indices()
                .nth(room)
                .map_or(rest.len(), |(index, _)| index);
            current.push_str(&rest[..split]);
            wrapped.push(std::mem::replace(
                &mut current,
                " ".repeat(subsequent_indent),
            ));
            current_width = subsequent_indent;
            rest = &rest[split..];
            rest_width -= room.min(rest_width);
        }
        current.push_str(rest);
        current_width += rest_width;
        words_on_line += 1;
    }
    if !current.is_empty() {
        wrapped.push(current);
    }
    if wrapped.is_empty() {
        vec![line.to_owned()]
    } else {
        wrapped
    }
}

fn exempt_from_wrap(line: &str) -> bool {
    let stripped = line.trim();
    stripped.is_empty()
        || stripped.starts_with("http://")
        || stripped.starts_with("https://")
        || trailer_key(stripped).is_some()
}

fn trailer_key(line: &str) -> Option<&str> {
    let (key, value) = line.split_once(": ")?;
    (!key.is_empty()
        && !value.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
    .then_some(key)
}

fn list_marker_width(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut cursor = 0;
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    match bytes.get(cursor) {
        Some(b'-' | b'*' | b'+') => cursor += 1,
        Some(byte) if byte.is_ascii_digit() => {
            cursor += 1;
            while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
                cursor += 1;
            }
            if !matches!(bytes.get(cursor), Some(b'.' | b')')) {
                return None;
            }
            cursor += 1;
        }
        _ => return None,
    }
    let whitespace = cursor;
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    (cursor > whitespace).then_some(line[..cursor].chars().count())
}

fn policy_lines(message: &str) -> Vec<(usize, &str)> {
    let mut lines = Vec::new();
    for (index, line) in message.lines().enumerate() {
        if line.contains(">8") {
            break;
        }
        if !line.starts_with('#') {
            lines.push((index + 1, line.trim_end()));
        }
    }
    while lines.last().is_some_and(|(_, line)| line.is_empty()) {
        lines.pop();
    }
    lines
}

fn committed_text(message: &str) -> String {
    let mut lines = Vec::new();
    for line in message.lines() {
        if line.contains(">8") {
            break;
        }
        if !line.starts_with('#') {
            lines.push(line);
        }
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn validate_hygiene(
    message: &str,
    policy: &gate::CommitMessagePolicy,
) -> Result<(), CommitMessageError> {
    let lines = policy_lines(message);
    let Some((subject_line, subject)) = lines.first().copied() else {
        return Err(CommitMessageError::Hygiene(
            "commit message subject must not be empty".to_owned(),
        ));
    };
    if subject.trim().is_empty() {
        return Err(CommitMessageError::Hygiene(
            "commit message subject must not be empty".to_owned(),
        ));
    }

    let mut failures = Vec::new();
    let policy_text = lines
        .iter()
        .map(|(_, line)| *line)
        .collect::<Vec<_>>()
        .join("\n");
    if policy_text.contains("\\n") {
        failures.push(
            "literal '\\n' found; write real newlines with `git commit -F <file>`".to_owned(),
        );
    }
    for (line_number, line) in lines.iter().skip(1) {
        let Some(key) = trailer_key(line.trim()) else {
            continue;
        };
        match policy.trailer_rejection(key) {
            Some(gate::TrailerRejection::Blocked) => {
                let blocked = policy
                    .blocked_trailers()
                    .expect("a blocked verdict names an explicit block set");
                let mut blocked = blocked.to_vec();
                blocked.sort();
                failures.push(format!(
                    "line {line_number} uses blocked trailer {}; blocked trailers: {}",
                    key.to_ascii_lowercase(),
                    blocked.join(", ")
                ));
            }
            Some(gate::TrailerRejection::Disallowed) => {
                let allowed = policy
                    .allowed_trailers()
                    .expect("a disallowed verdict names an explicit allow set");
                let mut allowed = allowed.to_vec();
                allowed.sort();
                failures.push(format!(
                    "line {line_number} uses disallowed trailer {}; allowed trailers: {}",
                    key.to_ascii_lowercase(),
                    if allowed.is_empty() {
                        "none".to_owned()
                    } else {
                        allowed.join(", ")
                    }
                ));
            }
            None => {}
        }
    }

    let placeholder = subject
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let placeholder = placeholder.trim_matches(PLACEHOLDER_TRAILING);
    if PLACEHOLDER_SUBJECTS.contains(&placeholder) {
        failures.push(format!(
            concat!(
                "line {} subject {:?} is a placeholder; ",
                "write a real subject describing the change"
            ),
            subject_line,
            subject.trim()
        ));
    }
    let subject_width = subject.chars().count();
    if subject_width > WRAP_LIMIT {
        failures.push(format!(
            concat!(
                "line {} is {} chars; ",
                "keep the subject at {} or less: {}"
            ),
            subject_line, subject_width, WRAP_LIMIT, subject
        ));
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(CommitMessageError::Hygiene(format!(
            concat!(
                "use a message file for multi-line commits, e.g. ",
                "`git commit -F /tmp/commit-message.txt`; ",
                "commit message hygiene failed:\n- {}"
            ),
            failures.join("\n- ")
        )))
    }
}

/// A native `commit-msg` refusal.
#[derive(Debug)]
pub enum CommitMessageError {
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Hygiene(String),
    Origin(gate::CommitMessageError),
}

impl fmt::Display for CommitMessageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                action,
                path,
                source,
            } => write!(
                formatter,
                "commit-msg could not {action} {}: {source}",
                path.display()
            ),
            Self::Hygiene(detail) => formatter.write_str(detail),
            Self::Origin(error) => error.fmt(formatter),
        }
    }
}

impl Error for CommitMessageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Origin(error) => Some(error),
            Self::Hygiene(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str =
        "Spice-Origin: v1 repo=018f82d4-6f59-7d06-92e3-bce9131163c7 task=HOOKS-1kJQnmG8x";

    #[test]
    fn body_prose_folds_with_list_indentation_and_exempts_trailers() {
        let prose = "alpha ".repeat(24);
        let list = format!("    - {prose}");
        let message = format!("Preserve commit policy\n\n{prose}\n{list}\n{ORIGIN}\n");
        let folded = fold(&message);
        let lines = folded.lines().collect::<Vec<_>>();

        assert!(lines.iter().all(|line| {
            line.chars().count() <= WRAP_LIMIT || trailer_key(line.trim()).is_some()
        }));
        let list = lines
            .iter()
            .position(|line| line.starts_with("    - "))
            .unwrap();
        assert!(lines[list + 1].starts_with("      "));
        assert_eq!(lines.last(), Some(&ORIGIN));
    }

    #[test]
    fn placeholder_literal_newline_and_long_subjects_are_refused_together() {
        let subject = format!("WIP{}", "!".repeat(98));
        let message = format!("{subject}\n\nliteral \\n evidence\n");
        let error = validate_hygiene(&message, &gate::CommitMessagePolicy::default())
            .expect_err("hygiene refuses");
        let detail = error.to_string();

        assert!(detail.contains("literal '\\n'"));
        assert!(detail.contains("placeholder"));
        assert!(detail.contains("101 chars"));
    }

    #[test]
    fn origin_grammar_ignores_comments_and_scissors_but_refuses_committed_evidence() {
        let ignored = concat!(
            "Preserve provenance\n",
            "# Spice-Origin: v2 ignored\n",
            "# ------------------------ >8 ------------------------\n",
            "Spice-Origin: v2 ignored\n",
        );
        gate::validate_commit_message("pending", &committed_text(ignored))
            .expect("non-committed evidence is ignored");

        let malformed = "Preserve provenance\n\nSpice-Origin: v2 evidence\n";
        gate::validate_commit_message("pending", &committed_text(malformed))
            .expect_err("committed malformed provenance refuses");
    }

    #[test]
    fn native_hook_consumes_the_same_case_insensitive_trailer_policy() {
        let policy = serde_json::from_str::<gate::CommitMessagePolicy>(
            r#"{
                "allowed_trailers": ["Spice-Origin", "Reviewed-By"],
                "blocked_trailers": ["Co-Authored-By"]
            }"#,
        )
        .expect("fixture trailer policy admits");
        let message = concat!(
            "Preserve launch policy\n\n",
            "Reviewed-By: Operator <operator@example.test>\n",
            "Co-Authored-By: Agent <agent@example.test>\n",
            "Signed-Off-By: Other <other@example.test>\n",
        );

        let error = validate_hygiene(message, &policy).expect_err("policy refuses");
        let detail = error.to_string();
        assert!(detail.contains("line 4 uses blocked trailer co-authored-by"));
        assert!(detail.contains("line 5 uses disallowed trailer signed-off-by"));
        assert!(!detail.contains("reviewed-by;"));
    }
}
