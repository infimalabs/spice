//! The operator work leaving this document would discard, named item by item.
//!
//! The leave guard holds the page exactly when this inventory is non-empty,
//! and the page then lists these same items, so a guard never fires without
//! naming what it protects and where that work lives.

use wire::TaskStamp;

use crate::composer_drafts::DraftHandoff;

/// Characters of draft text an item quotes so the operator recognizes it.
const EXCERPT_CHARS: usize = 80;

/// One piece of unsent operator work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UnsentWork {
    /// A composer's unsent text, quoted replies, or images, keyed by the
    /// composer target its draft belongs to.
    Composer {
        target: String,
        excerpt: String,
        quotes: usize,
        images: usize,
    },
    /// An observation drafted on a task.
    Note { task: TaskStamp, excerpt: String },
    /// Messages the resident has not yet accepted, queued or in flight. No
    /// later document can learn their fate.
    Awaiting { count: usize },
    /// Annotation marks not yet applied to the named draft image.
    Annotation { image: String },
}

/// Everything leaving now would discard: the drafts a reload would hand off,
/// sent messages still awaiting their result, and unapplied annotation marks.
pub(crate) fn inventory(
    drafts: &DraftHandoff,
    awaiting: usize,
    annotation: Option<&str>,
) -> Vec<UnsentWork> {
    let composers = drafts.composers.iter().filter_map(|composer| {
        let quotes = composer.quotes.len();
        let images = composer.attachments.len()
            + composer
                .quotes
                .iter()
                .map(|quote| quote.attachments.len())
                .sum::<usize>();
        let text = std::iter::once(composer.body.as_str())
            .chain(std::iter::once(composer.continuation.as_str()))
            .chain(composer.quotes.iter().map(|quote| quote.body.as_str()));
        let excerpt = text.map(excerpt).find(|line| !line.is_empty());
        (excerpt.is_some() || quotes > 0 || images > 0).then(|| UnsentWork::Composer {
            target: composer.target.clone(),
            excerpt: excerpt.unwrap_or_default(),
            quotes,
            images,
        })
    });
    let notes = drafts.notes.iter().filter_map(|note| {
        let excerpt = excerpt(&note.text);
        (!excerpt.is_empty()).then(|| UnsentWork::Note {
            task: note.task.clone(),
            excerpt,
        })
    });
    composers
        .chain(notes)
        .chain((awaiting > 0).then_some(UnsentWork::Awaiting { count: awaiting }))
        .chain(annotation.map(|image| UnsentWork::Annotation {
            image: image.to_owned(),
        }))
        .collect()
}

/// The first non-blank line of `text`, cut to [`EXCERPT_CHARS`].
fn excerpt(text: &str) -> String {
    let Some(line) = text.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return String::new();
    };
    match line.char_indices().nth(EXCERPT_CHARS) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer_drafts::{HandedComposer, HandedNote, HandedQuote};
    use wire::Media;

    fn composer(target: &str, body: &str) -> HandedComposer {
        HandedComposer {
            target: target.to_owned(),
            body: body.to_owned(),
            continuation: String::new(),
            quotes: Vec::new(),
            attachments: Vec::new(),
        }
    }

    fn quote(body: &str) -> HandedQuote {
        HandedQuote {
            identity: "quote".to_owned(),
            source_id: "source".to_owned(),
            preview: "preview".to_owned(),
            display_millis: 0,
            body: body.to_owned(),
            attachments: Vec::new(),
            order: 0,
        }
    }

    fn image() -> Media {
        Media::from_base64("shot.png", "image/png", "iVBORw0KGgo=")
            .expect("attachment fixture decodes")
    }

    fn task() -> TaskStamp {
        "dWgYktsvQ".parse().expect("task stamp fixture parses")
    }

    #[test]
    fn nothing_unsent_leaves_the_guard_disarmed() {
        let drafts = DraftHandoff {
            composers: vec![composer("lane", " \n\t")],
            notes: vec![HandedNote {
                task: task(),
                text: "\n".to_owned(),
            }],
        };
        assert_eq!(inventory(&drafts, 0, None), Vec::new());
    }

    #[test]
    fn each_kind_of_unsent_work_is_one_named_item() {
        let mut quoted = composer("design-group-dWgYktsvQ", "");
        quoted.quotes.push(quote("\n  reply line\nmore"));
        quoted.quotes[0].attachments.push(image());
        let mut pictured = composer("lane-b", "");
        pictured.attachments.push(image());
        let drafts = DraftHandoff {
            composers: vec![
                composer("lane-a", "\n  hello there  \nsecond"),
                quoted,
                pictured,
            ],
            notes: vec![HandedNote {
                task: task(),
                text: "observed".to_owned(),
            }],
        };
        assert_eq!(
            inventory(&drafts, 2, Some("shot.png")),
            vec![
                UnsentWork::Composer {
                    target: "lane-a".to_owned(),
                    excerpt: "hello there".to_owned(),
                    quotes: 0,
                    images: 0,
                },
                UnsentWork::Composer {
                    target: "design-group-dWgYktsvQ".to_owned(),
                    excerpt: "reply line".to_owned(),
                    quotes: 1,
                    images: 1,
                },
                UnsentWork::Composer {
                    target: "lane-b".to_owned(),
                    excerpt: String::new(),
                    quotes: 0,
                    images: 1,
                },
                UnsentWork::Note {
                    task: task(),
                    excerpt: "observed".to_owned(),
                },
                UnsentWork::Awaiting { count: 2 },
                UnsentWork::Annotation {
                    image: "shot.png".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn a_long_line_is_cut_at_a_character_boundary() {
        let long = "é".repeat(EXCERPT_CHARS + 5);
        assert_eq!(excerpt(&long), format!("{}…", "é".repeat(EXCERPT_CHARS)));
        assert_eq!(
            excerpt(&"é".repeat(EXCERPT_CHARS)),
            "é".repeat(EXCERPT_CHARS)
        );
    }
}
