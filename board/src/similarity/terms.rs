//! Preserve spelling and token boundaries until technical-term admission.

/// Yield canonical terms without allocating rejected runs or their context.
pub(super) fn terms(text: &str) -> impl Iterator<Item = String> + '_ {
    let mut offset = 0;
    text.split(|c: char| !c.is_alphanumeric())
        .filter_map(move |run| {
            let start = offset;
            offset += run.len();
            let suffix = &text[offset..];
            offset += suffix.chars().next().map_or(0, char::len_utf8);
            let mut chars = run.chars();
            let first = chars.next()?;
            let second = chars.next()?;
            let third = chars.next()?;
            let fourth = chars.next();
            if option_prefix(&text[..start], suffix) {
                return None;
            }
            let Some(fourth) = fourth else {
                if !short_jargon([first, second, third])
                    // Reject common ASCII runs before allocating their lowercase form.
                    || super::STOP.iter().any(|stop| run.eq_ignore_ascii_case(stop))
                {
                    return None;
                }
                return canonical_jargon(run);
            };
            let mut has_numeric = [first, second, third, fourth]
                .into_iter()
                .any(char::is_numeric);
            for (index, character) in chars.enumerate() {
                if index >= super::MAX_TERM_CHARS - 4 {
                    return None;
                }
                has_numeric |= character.is_numeric();
            }
            if first.is_alphabetic() && has_numeric {
                return canonical_jargon(run);
            }
            super::normalize(run)
        })
        // Every admitted spelling, including folded longer words, uses this
        // authority. Session-control markers do not describe a task's subject.
        .filter(|word| {
            !super::STOP.contains(&word.as_str())
                && !matches!(word.as_str(), "syn" | "ack" | "nak" | "fin")
        })
}

fn canonical_jargon(run: &str) -> Option<String> {
    canonical_lower_jargon(run.to_lowercase())
}

pub(super) fn canonical_lower_jargon(word: String) -> Option<String> {
    word.chars()
        .all(|c| c.is_alphanumeric() && !c.is_uppercase())
        .then_some(word)
}

pub(super) fn short_jargon(chars: [char; 3]) -> bool {
    let [first, second, third] = chars;
    if !first.is_alphabetic() {
        return false;
    }
    chars.iter().any(|c| c.is_numeric())
        || chars.iter().all(|c| c.is_uppercase())
        || (chars.iter().any(|c| c.is_uppercase())
            && chars.iter().any(|c| c.is_lowercase())
            && !(first.is_uppercase() && second.is_lowercase() && third.is_lowercase()))
        || chars
            .iter()
            .all(|c| c.is_ascii_lowercase() && !matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'y'))
}

fn option_prefix(prefix: &str, suffix: &str) -> bool {
    if suffix.starts_with(['-', '_', '/']) {
        return false;
    }
    let Some(before_dash) = prefix
        .strip_suffix("--")
        .or_else(|| prefix.strip_suffix('-'))
    else {
        return false;
    };
    before_dash
        .chars()
        .next_back()
        .is_none_or(|c| c.is_whitespace() || matches!(c, '`' | '(' | '[' | '{' | '"' | '\''))
}

#[cfg(test)]
mod tests {
    use crate::similarity::{MAX_TERM_CHARS, TermCounts};

    #[test]
    fn session_control_markers_do_not_become_topical_evidence() {
        let subject = "Cargo dependency cache CSS CLI iOS";
        assert_eq!(
            TermCounts::from_texts([
                subject,
                "SYN ACK NAK FIN syn ack nak fin SYNs ACKs NAKs FINs SYN-ACK/NAK",
            ])
            .0,
            TermCounts::from_texts([subject]).0,
        );
        let meaningful = TermCounts::from_texts(["synapse acknowledgment finance final"]);
        assert_eq!(
            meaningful.0.into_keys().collect::<Vec<_>>(),
            ["acknowledgment", "final", "finance", "synapse"]
        );
    }

    #[test]
    fn technical_jargon_keeps_case_evidence_until_admission() {
        for (input, canonical) in [
            ("CGI", "cgi"),
            ("aDS", "ads"),
            ("Iq3", "iq3"),
            ("CSS", "css"),
            ("css", "css"),
            ("x86", "x86"),
            ("p95", "p95"),
            ("iOS", "ios"),
            ("cmd", "cmd"),
            ("éDS", "éds"),
            ("a²b", "a²b"),
            ("p999", "p999"),
            ("utf8", "utf8"),
            ("K8S2", "k8s2"),
            ("WebView2", "webview2"),
            ("wasm32", "wasm32"),
            ("SHA256", "sha256"),
        ] {
            assert_eq!(
                TermCounts::from_texts([input]).0,
                [(canonical.to_owned(), 1)].into(),
                "{input}"
            );
        }
        for input in [
            "Ray",
            "Mrs",
            "Cmd",
            "03Z",
            "6px",
            "123",
            "1234",
            "6px2",
            "cli",
            "api",
            "sky",
            "aB",
            "x8",
            "İOS",
            "𝐂𝐒𝐒",
        ] {
            assert!(TermCounts::from_texts([input]).0.is_empty(), "{input}");
        }
    }

    #[test]
    fn excessive_runs_never_enter_sparse_vocabulary() {
        let ascii_limit = format!("a{}1", "b".repeat(MAX_TERM_CHARS - 2));
        let ascii_over = format!("a{}1", "b".repeat(MAX_TERM_CHARS - 1));
        let unicode_limit = format!("é{}1", "é".repeat(MAX_TERM_CHARS - 2));
        let unicode_over = format!("é{}1", "é".repeat(MAX_TERM_CHARS - 1));
        let sha1 = format!("a{}1", "b".repeat(38));
        let sha256 = format!("a{}1", "b".repeat(62));
        let counts = TermCounts::from_texts([
            ascii_limit.as_str(),
            ascii_over.as_str(),
            unicode_limit.as_str(),
            unicode_over.as_str(),
            sha1.as_str(),
            sha256.as_str(),
        ]);

        assert_eq!(counts.0.get(&ascii_limit), Some(&1));
        assert_eq!(counts.0.get(&unicode_limit), Some(&1));
        for rejected in [ascii_over, unicode_over, sha1, sha256] {
            assert!(!counts.0.contains_key(&rejected), "{rejected}");
        }

        let identifiers = TermCounts::from_texts(["BOARD-dY1WG7gcT e58a09b17"]);
        assert_eq!(identifiers.0.get("dy1wg7gct"), Some(&1));
        assert_eq!(identifiers.0.get("e58a09b17"), Some(&1));
    }

    #[test]
    fn punctuation_excludes_flags_without_erasing_compound_terms() {
        let flags = "-ciE --cfg=value `-vvv` (-CGI) [-CSS] {-LSP} \"--IDF\" '-UTF' --json -build --p999 \u{2003}--META";
        assert_eq!(
            TermCounts::from_texts([flags]).0,
            [("value".to_owned(), 1)].into()
        );
        assert_eq!(
            TermCounts::from_texts([
                "TF-IDF UTF-8 UTF8-mode x86-64 p999/path -tmp- CSS/css —CLI\u{2003}css",
            ])
            .0,
            [
                ("cli".to_owned(), 1),
                ("css".to_owned(), 3),
                ("idf".to_owned(), 1),
                ("tmp".to_owned(), 1),
                ("utf".to_owned(), 1),
                ("utf8".to_owned(), 1),
                ("mode".to_owned(), 1),
                ("p999".to_owned(), 1),
                ("path".to_owned(), 1),
                ("x86".to_owned(), 1),
            ]
            .into()
        );
        // Reject only the specified option boundary, not every preceding dash.
        assert_eq!(
            TermCounts::from_texts(["---cfg a-cfg --cfg_tail --cfg/path"]).0["cfg"],
            4
        );
    }

    #[test]
    fn admitted_occurrences_merge_with_longer_jargon_and_option_filtering() {
        assert_eq!(
            TermCounts::from_texts(["CSS css --CSS css", "CLI cli"]).0,
            [("css".to_owned(), 3), ("cli".to_owned(), 1)].into()
        );
    }

    #[test]
    fn ordinary_plurals_reuse_three_character_singular_admission() {
        assert_eq!(
            TermCounts::from_texts([
                "CSS CSSs --CSSs API APIs -APIs CLI CLIs iOS iOSs cmd cmds éDS éDSs",
            ])
            .0,
            [
                ("api".to_owned(), 2),
                ("cli".to_owned(), 2),
                ("cmd".to_owned(), 2),
                ("css".to_owned(), 2),
                ("ios".to_owned(), 2),
                ("éds".to_owned(), 2),
            ]
            .into()
        );
        assert!(
            TermCounts::from_texts(["ANDS apis clis runs sets uses ways"])
                .0
                .is_empty()
        );
    }
}
