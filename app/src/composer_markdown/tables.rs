//! Lay out markdown tables for a narrow, scrollable message column.
//!
//! pulldown-cmark writes an aligned cell as `style="text-align: …"`, and the
//! app `style-src 'self'` policy refuses every inline style attribute, so
//! the alignment is written here as `data-align` instead and the writer never
//! sees one.
//!
//! Each table is wrapped in a `.markdown-table` scroll box, the way fenced code
//! scrolls inside its own block. Cells keep ordinary words whole, so a table
//! that cannot fit scrolls rather than splitting every word letter by letter.
//! Whether a column may squeeze is decided here, once per table, from its text:
//! a prose column keeps a readable floor width, and a column carrying a token
//! too long to keep whole (a path, hash or URL) may break that token instead of
//! forcing the whole table wide. The verdict reaches the stylesheet as
//! `data-measure` on every cell of the column.

use pulldown_cmark::{Alignment, CowStr, Event, Tag, TagEnd};

/// Longest word, in characters, a cell keeps unbroken.
const LONG_TOKEN: usize = 24;
/// Longest cell, in characters, that never needs a prose floor.
const SHORT_CELL: usize = 24;

const TABLE_OPEN: &str = "<div class=\"markdown-table\">";
const TABLE_CLOSE: &str = "</div>";

/// How one column may give up width before its table scrolls.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Measure {
    /// Short cells: the column is as narrow as its longest word.
    #[default]
    Plain,
    /// Running text: wraps at word boundaries above a readable floor.
    Prose,
    /// A token too long to keep whole: breaks it above a readable floor.
    Token,
}

impl Measure {
    const fn attribute(self) -> Option<&'static str> {
        match self {
            Self::Plain => None,
            Self::Prose => Some("prose"),
            Self::Token => Some("token"),
        }
    }
}

/// Text extent one column has shown so far.
#[derive(Clone, Copy, Default)]
struct Extent {
    longest_word: usize,
    longest_cell: usize,
    spaced: bool,
}

impl Extent {
    fn cell(&mut self, text: &str) {
        for word in text.split_whitespace() {
            self.longest_word = self.longest_word.max(word.chars().count());
        }
        let text = text.trim();
        self.longest_cell = self.longest_cell.max(text.chars().count());
        self.spaced |= text.contains(char::is_whitespace);
    }

    const fn measure(self) -> Measure {
        if self.longest_word > LONG_TOKEN {
            Measure::Token
        } else if self.spaced && self.longest_cell > SHORT_CELL {
            Measure::Prose
        } else {
            Measure::Plain
        }
    }
}

pub(super) fn lay_out_tables<'a>(
    events: impl IntoIterator<Item = Event<'a>>,
) -> impl Iterator<Item = Event<'a>> {
    TableLayout {
        events: events.into_iter(),
        table: Vec::new().into_iter(),
    }
}

struct TableLayout<'a, I> {
    events: I,
    /// The laid-out events of the table being replayed.
    table: std::vec::IntoIter<Event<'a>>,
}

impl<'a, I: Iterator<Item = Event<'a>>> Iterator for TableLayout<'a, I> {
    type Item = Event<'a>;

    fn next(&mut self) -> Option<Event<'a>> {
        if let Some(event) = self.table.next() {
            return Some(event);
        }
        match self.events.next()? {
            Event::Start(Tag::Table(alignments)) => {
                let mut body = Vec::new();
                for event in self.events.by_ref() {
                    if matches!(event, Event::End(TagEnd::Table)) {
                        break;
                    }
                    body.push(event);
                }
                self.table = lay_out(&alignments, body).into_iter();
                self.table.next()
            }
            event => Some(event),
        }
    }
}

/// Measure every column of one table, then write it with its cell attributes.
fn lay_out<'a>(alignments: &[Alignment], body: Vec<Event<'a>>) -> Vec<Event<'a>> {
    let measures = measure_columns(&body);
    let mut events = Vec::with_capacity(body.len() + 3);
    events.push(Event::Html(CowStr::Borrowed(TABLE_OPEN)));
    events.push(Event::Start(Tag::Table(Vec::new())));
    let mut head = false;
    let mut column = 0usize;
    let mut open = None::<&'static str>;
    for event in body {
        events.push(match event {
            Event::Start(Tag::TableHead) => {
                head = true;
                column = 0;
                event
            }
            Event::End(TagEnd::TableHead) => {
                head = false;
                event
            }
            Event::Start(Tag::TableRow) => {
                column = 0;
                event
            }
            Event::Start(Tag::TableCell) => {
                let align = match alignments.get(column) {
                    Some(Alignment::Left) => Some("left"),
                    Some(Alignment::Center) => Some("center"),
                    Some(Alignment::Right) => Some("right"),
                    Some(Alignment::None) | None => None,
                };
                let measure = measures
                    .get(column)
                    .copied()
                    .unwrap_or_default()
                    .attribute();
                if align.is_none() && measure.is_none() {
                    event
                } else {
                    let cell = if head { "th" } else { "td" };
                    open = Some(cell);
                    let mut tag = format!("<{cell}");
                    if let Some(align) = align {
                        tag.push_str(&format!(" data-align=\"{align}\""));
                    }
                    if let Some(measure) = measure {
                        tag.push_str(&format!(" data-measure=\"{measure}\""));
                    }
                    tag.push('>');
                    Event::Html(CowStr::from(tag))
                }
            }
            Event::End(TagEnd::TableCell) => {
                column += 1;
                match open.take() {
                    Some(cell) => Event::Html(CowStr::from(format!("</{cell}>"))),
                    None => event,
                }
            }
            event => event,
        });
    }
    events.push(Event::End(TagEnd::Table));
    events.push(Event::Html(CowStr::Borrowed(TABLE_CLOSE)));
    events
}

fn measure_columns(body: &[Event<'_>]) -> Vec<Measure> {
    let mut extents = Vec::<Extent>::new();
    let mut column = 0usize;
    // GFM table text only occurs inside cells, so one buffer serves them all.
    let mut cell = String::new();
    for event in body {
        match event {
            Event::Start(Tag::TableHead | Tag::TableRow) => column = 0,
            Event::Start(Tag::TableCell) => cell.clear(),
            Event::End(TagEnd::TableCell) => {
                if extents.len() <= column {
                    extents.resize_with(column + 1, Extent::default);
                }
                extents[column].cell(&cell);
                column += 1;
            }
            Event::Text(text) | Event::Code(text) | Event::InlineMath(text) => {
                cell.push_str(text);
            }
            Event::SoftBreak | Event::HardBreak => cell.push(' '),
            _ => {}
        }
    }
    extents.into_iter().map(Extent::measure).collect()
}

#[cfg(test)]
mod tests;
