//! Typed task-document graph values.

use std::fmt;

/// The reserved root used when a document has several top-level nodes.
pub const DOCUMENT_ROOT_TITLE: &str = "Document root";
pub const DOCUMENT_ROOT_SLUG: &str = "document-root";

/// The Markdown structure that introduced a node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    Heading,
    Item,
    SyntheticRoot,
}

/// A task priority admitted by the current dialect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Priority {
    Critical,
    High,
    Medium,
    Low,
    None,
}

impl Priority {
    /// Every admitted priority, strongest first.
    pub const ALL: [Self; 5] = [
        Self::Critical,
        Self::High,
        Self::Medium,
        Self::Low,
        Self::None,
    ];

    /// Read one authored priority, ignoring surrounding space and case.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "critical" => Some(Self::Critical),
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::None => "none",
        }
    }
}

impl fmt::Display for Priority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A workflow phase admitted by a task document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Design,
    Plan,
    Todo,
    Verify,
    Review,
}

impl Phase {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "design" => Some(Self::Design),
            "plan" => Some(Self::Plan),
            "todo" => Some(Self::Todo),
            "verify" => Some(Self::Verify),
            "review" => Some(Self::Review),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Design => "design",
            Self::Plan => "plan",
            Self::Todo => "todo",
            Self::Verify => "verify",
            Self::Review => "review",
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One node in the authored work graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node {
    pub id: usize,
    pub kind: NodeKind,
    pub title: String,
    pub slug: String,
    pub line: usize,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub term: Vec<String>,
    pub priority: Option<Priority>,
    pub flow: Vec<Phase>,
    pub due: Option<String>,
    pub description: String,
    pub annotations: Vec<String>,
    pub(crate) after: Vec<(String, usize)>,
}

impl Node {
    pub(crate) fn new(
        id: usize,
        kind: NodeKind,
        title: String,
        line: usize,
        parent: Option<usize>,
    ) -> Self {
        Self {
            id,
            kind,
            title,
            slug: String::new(),
            line,
            parent,
            children: Vec::new(),
            term: Vec::new(),
            priority: None,
            flow: Vec::new(),
            due: None,
            description: String::new(),
            annotations: Vec::new(),
            after: Vec::new(),
        }
    }
}

/// Why a dependency edge exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EdgeKind {
    Containment,
    Explicit,
}

/// A dependency points from a goal toward a prerequisite.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Edge {
    pub source: usize,
    pub target: usize,
    pub kind: EdgeKind,
}

/// A classified document and its complete work graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Document {
    pub origin: Option<String>,
    pub nodes: Vec<Node>,
    pub root: usize,
    pub edges: Vec<Edge>,
}

impl Document {
    /// Resolve a node by its final, unambiguous slug.
    pub fn node(&self, slug: &str) -> Option<&Node> {
        self.nodes.iter().find(|node| node.slug == slug)
    }
}

/// Produce the natural ASCII identity for a title.
///
/// URL targets are excluded while visible link text remains. The function is
/// intentionally small and deterministic so the grammar does not inherit a
/// locale or Unicode-version dependency.
pub fn slugify(title: &str) -> String {
    let visible = strip_link_targets_and_urls(title);
    let mut words = Vec::new();
    let mut current = String::new();
    for byte in visible.bytes() {
        if byte.is_ascii_alphanumeric() {
            current.push(char::from(byte.to_ascii_lowercase()));
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words.join("-")
}

fn strip_link_targets_and_urls(title: &str) -> String {
    let bytes = title.as_bytes();
    let mut output = String::with_capacity(title.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'['
            && let Some(close_text) = title[index + 1..].find(']')
        {
            let close_text = index + 1 + close_text;
            if bytes.get(close_text + 1) == Some(&b'(')
                && let Some(close_target) = title[close_text + 2..].find(')')
            {
                output.push_str(&title[index + 1..close_text]);
                index = close_text + 3 + close_target;
                continue;
            }
        }
        if title[index..].starts_with("http://") || title[index..].starts_with("https://") {
            index += title[index..]
                .find(char::is_whitespace)
                .unwrap_or(title.len() - index);
            output.push(' ');
            continue;
        }
        let ch = title[index..]
            .chars()
            .next()
            .expect("index is on a char boundary");
        output.push(ch);
        index += ch.len_utf8();
    }
    output
}
