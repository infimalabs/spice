//! Byte-stable normal-form task-document emission.

use std::collections::BTreeMap;

use crate::node::{Document, EdgeKind, Node, NodeKind};

/// Emit the graph's single canonical Markdown spelling.
pub fn emit(document: &Document) -> String {
    if document.nodes.is_empty() || document.root >= document.nodes.len() {
        return String::new();
    }
    Renderer::new(document).render()
}

struct Renderer<'a> {
    document: &'a Document,
    explicit_after: BTreeMap<usize, Vec<String>>,
    lines: Vec<String>,
}

impl<'a> Renderer<'a> {
    fn new(document: &'a Document) -> Self {
        let mut explicit_after: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        for edge in &document.edges {
            if edge.kind == EdgeKind::Explicit {
                explicit_after
                    .entry(edge.source)
                    .or_default()
                    .push(document.nodes[edge.target].slug.clone());
            }
        }
        for targets in explicit_after.values_mut() {
            targets.sort();
            targets.dedup();
        }
        Self {
            document,
            explicit_after,
            lines: Vec::new(),
        }
    }

    fn render(mut self) -> String {
        let root = &self.document.nodes[self.document.root];
        if root.kind == NodeKind::SyntheticRoot {
            self.emit_description(root, 0);
            for child in ordered_children(self.document, root) {
                self.emit_node(child, 1, 0);
            }
        } else {
            self.emit_node(root.id, 1, 0);
        }
        collapse_lines(&self.lines)
    }

    fn emit_node(&mut self, id: usize, heading_level: usize, item_indent: usize) {
        let node = &self.document.nodes[id];
        let follows_bare_item = node.kind == NodeKind::Item
            && self.is_bare(node)
            && self
                .lines
                .last()
                .is_some_and(|line| line.trim_start().starts_with("- "));
        if !follows_bare_item {
            self.blank();
        }
        let content_col = match node.kind {
            NodeKind::Heading => {
                self.lines.push(format!(
                    "{} {}",
                    "#".repeat(heading_level.min(6)),
                    node.title
                ));
                0
            }
            NodeKind::Item => {
                self.lines
                    .push(format!("{}- {}", " ".repeat(item_indent), node.title));
                item_indent + 2
            }
            NodeKind::SyntheticRoot => 0,
        };
        self.emit_content(node, content_col);
        let (child_heading_level, child_item_indent) = match node.kind {
            NodeKind::Heading => (heading_level + 1, 0),
            NodeKind::Item => (heading_level, item_indent + 2),
            NodeKind::SyntheticRoot => (heading_level, item_indent),
        };
        for child in ordered_children(self.document, node) {
            self.emit_node(child, child_heading_level, child_item_indent);
        }
    }

    fn emit_content(&mut self, node: &Node, content_col: usize) {
        let pad = " ".repeat(content_col);
        for term in &node.term {
            self.lines.push(format!("{pad}Term: {term}"));
        }
        if let Some(targets) = self.explicit_after.get(&node.id) {
            self.lines
                .push(format!("{pad}After: {}", targets.join(", ")));
        }
        if let Some(priority) = node.priority {
            self.lines.push(format!("{pad}Priority: {priority}"));
        }
        if !node.flow.is_empty() {
            let phases = node
                .flow
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            self.lines.push(format!("{pad}Flow: {phases}"));
        }
        if let Some(due) = &node.due {
            self.lines.push(format!("{pad}Due: {due}"));
        }
        self.emit_description(node, content_col);
        for annotation in &node.annotations {
            self.blank();
            for line in annotation.lines() {
                self.lines.push(if line.is_empty() {
                    String::new()
                } else {
                    format!("{pad}{line}")
                });
            }
        }
    }

    fn emit_description(&mut self, node: &Node, content_col: usize) {
        if node.description.is_empty() {
            return;
        }
        self.blank();
        let pad = " ".repeat(content_col);
        for line in node.description.lines() {
            self.lines.push(if line.is_empty() {
                String::new()
            } else {
                format!("{pad}{}", escape_prose(line))
            });
        }
    }

    fn blank(&mut self) {
        if self.lines.last().is_some_and(|line| !line.is_empty()) {
            self.lines.push(String::new());
        }
    }

    fn is_bare(&self, node: &Node) -> bool {
        node.term.is_empty()
            && self.explicit_after.get(&node.id).is_none_or(Vec::is_empty)
            && node.priority.is_none()
            && node.flow.is_empty()
            && node.due.is_none()
            && node.description.is_empty()
            && node.annotations.is_empty()
    }
}

fn ordered_children(document: &Document, node: &Node) -> Vec<usize> {
    let mut items = Vec::new();
    let mut headings = Vec::new();
    for child in &node.children {
        if document.nodes[*child].kind == NodeKind::Item {
            items.push(*child);
        } else {
            headings.push(*child);
        }
    }
    items.extend(headings);
    items
}

fn escape_prose(line: &str) -> String {
    let stripped = line.trim_start();
    let pad = &line[..line.len() - stripped.len()];
    if looks_like_field(stripped) {
        let colon = stripped.find(':').expect("field shape has a colon");
        return format!("{pad}{}\\{}", &stripped[..colon], &stripped[colon..]);
    }
    let structural = stripped.starts_with(['#', '-', '*', '+', '>', '|'])
        || stripped.starts_with("```")
        || stripped.starts_with("~~~")
        || looks_like_ordered_item(stripped);
    if structural {
        format!("{pad}\\{stripped}")
    } else {
        line.to_owned()
    }
}

fn looks_like_ordered_item(line: &str) -> bool {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0
        && matches!(line.as_bytes().get(digits), Some(b'.' | b')'))
        && line.as_bytes().get(digits + 1) == Some(&b' ')
}

fn looks_like_field(line: &str) -> bool {
    let Some(colon) = line.find(':') else {
        return false;
    };
    let label = line[..colon].trim_matches(['*', '_', ' ']);
    !label.is_empty()
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic() || byte == b' ')
}

fn collapse_lines(lines: &[String]) -> String {
    let mut output: Vec<&str> = Vec::new();
    for line in lines {
        if line.is_empty() && output.last().is_none_or(|previous| previous.is_empty()) {
            continue;
        }
        output.push(line.as_str());
    }
    while output.last().is_some_and(|line| line.is_empty()) {
        output.pop();
    }
    if output.is_empty() {
        String::new()
    } else {
        format!("{}\n", output.join("\n"))
    }
}
