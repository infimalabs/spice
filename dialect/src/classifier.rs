//! Forgiving task-document reader and graph validator.

use std::collections::{BTreeMap, BTreeSet};

use crate::ledger::emit;
use crate::node::{
    DOCUMENT_ROOT_SLUG, DOCUMENT_ROOT_TITLE, Document, Edge, EdgeKind, Node, NodeKind, Phase,
    Priority, slugify,
};
use crate::refuse::{Refusal, RefusalCode};

const FIELD_NAMES: &[(&str, &str)] = &[
    ("term", "term"),
    ("acceptance", "term"),
    ("acceptance criteria", "term"),
    ("success criteria", "term"),
    ("done when", "term"),
    ("definition of done", "term"),
    ("ac", "term"),
    ("after", "after"),
    ("depends on", "after"),
    ("blocked by", "after"),
    ("prerequisites", "after"),
    ("priority", "priority"),
    ("flow", "flow"),
    ("due", "due"),
    ("deadline", "due"),
];

/// Identity supplied by the caller rather than authored inside Markdown.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClassificationContext {
    pub origin: Option<String>,
}

/// Machine-readable explanation for a convertible spelling.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum WarningCode {
    NonCanonicalNewlines,
    NonCanonicalHeading,
    NonCanonicalListMarker,
    CheckedStatusDiscarded,
    FieldAlias,
    RepeatedField,
    ThematicBreakDiscarded,
    UnclosedFence,
    NormalizationRequired,
}

impl WarningCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NonCanonicalNewlines => "noncanonical-newlines",
            Self::NonCanonicalHeading => "noncanonical-heading",
            Self::NonCanonicalListMarker => "noncanonical-list-marker",
            Self::CheckedStatusDiscarded => "checked-status-discarded",
            Self::FieldAlias => "field-alias",
            Self::RepeatedField => "repeated-field",
            Self::ThematicBreakDiscarded => "thematic-break-discarded",
            Self::UnclosedFence => "unclosed-fence",
            Self::NormalizationRequired => "normalization-required",
        }
    }
}

/// One nonblocking classification judgment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Warning {
    pub code: WarningCode,
    pub line: usize,
    pub message: String,
}

impl Warning {
    fn new(code: WarningCode, line: usize, message: impl Into<String>) -> Self {
        Self {
            code,
            line,
            message: message.into(),
        }
    }
}

/// Exactly one outcome for every input document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Classification {
    Current(Document),
    Convertible {
        document: Document,
        warnings: Vec<Warning>,
    },
    Refused {
        refusals: Vec<Refusal>,
        warnings: Vec<Warning>,
    },
}

impl Classification {
    pub fn document(&self) -> Option<&Document> {
        match self {
            Self::Current(document) | Self::Convertible { document, .. } => Some(document),
            Self::Refused { .. } => None,
        }
    }

    pub fn warnings(&self) -> &[Warning] {
        match self {
            Self::Current(_) => &[],
            Self::Convertible { warnings, .. } | Self::Refused { warnings, .. } => warnings,
        }
    }

    pub fn refusals(&self) -> &[Refusal] {
        match self {
            Self::Refused { refusals, .. } => refusals,
            Self::Current(_) | Self::Convertible { .. } => &[],
        }
    }
}

/// Classify task-document text without family identity.
pub fn classify(input: &str) -> Classification {
    classify_with_context(input, ClassificationContext::default())
}

/// Classify task-document text with caller-owned family identity.
pub fn classify_with_context(input: &str, context: ClassificationContext) -> Classification {
    let mut parser = Parser::new(input, context);
    parser.parse()
}

struct Parser {
    original: String,
    text: String,
    context: ClassificationContext,
    nodes: Vec<Node>,
    node_content_cols: Vec<usize>,
    headings: [Option<usize>; 6],
    list_stack: Vec<(usize, usize)>,
    current_heading: Option<usize>,
    last_owner: Option<usize>,
    after_blank: bool,
    preamble: Vec<String>,
    warnings: Vec<Warning>,
    refusals: Vec<Refusal>,
}

impl Parser {
    fn new(input: &str, context: ClassificationContext) -> Self {
        let original = input.to_owned();
        let text = input
            .strip_prefix('\u{feff}')
            .unwrap_or(input)
            .replace("\r\n", "\n");
        let mut warnings = Vec::new();
        if text != input {
            warnings.push(Warning::new(
                WarningCode::NonCanonicalNewlines,
                1,
                "BOM or CRLF spelling will normalize to UTF-8 LF text",
            ));
        }
        Self {
            original,
            text,
            context,
            nodes: Vec::new(),
            node_content_cols: Vec::new(),
            headings: [None; 6],
            list_stack: Vec::new(),
            current_heading: None,
            last_owner: None,
            after_blank: true,
            preamble: Vec::new(),
            warnings,
            refusals: Vec::new(),
        }
    }

    fn parse(&mut self) -> Classification {
        if self.text.trim().is_empty() {
            return Classification::Refused {
                refusals: vec![Refusal::new(
                    RefusalCode::EmptyDocument,
                    None,
                    "task document is empty",
                )],
                warnings: std::mem::take(&mut self.warnings),
            };
        }
        self.validate_context();
        let lines: Vec<String> = self.text.lines().map(str::to_owned).collect();
        self.parse_lines(&lines);
        if self.nodes.is_empty() {
            self.refusals.push(Refusal::new(
                RefusalCode::NoNodes,
                None,
                "task document has no nodes (content but no structure)",
            ));
        }
        self.finish()
    }

    fn parse_lines(&mut self, lines: &[String]) {
        let mut index = 0;
        while index < lines.len() {
            index = self.consume_line(lines, index);
        }
    }

    fn consume_line(&mut self, lines: &[String], index: usize) -> usize {
        let line_number = index + 1;
        let line = &lines[index];
        if line.trim().is_empty() {
            self.push_blank();
            return index + 1;
        }
        if let Some(next) = self.consume_block(lines, index) {
            return next;
        }
        if let Some(item) = list_item(line) {
            self.consume_item(item, line_number);
            return index + 1;
        }
        self.consume_content(line, line_number);
        index + 1
    }

    fn consume_block(&mut self, lines: &[String], index: usize) -> Option<usize> {
        let line_number = index + 1;
        let line = &lines[index];
        if let Some((marker, title)) = setext_heading(lines, index) {
            self.add_heading(marker, title, line_number, true);
            return Some(index + 2);
        }
        if let Some((marker, title, canonical)) = atx_heading(line) {
            self.add_heading(marker, title, line_number, !canonical);
            return Some(index + 1);
        }
        if is_thematic_break(line) {
            self.warnings.push(Warning::new(
                WarningCode::ThematicBreakDiscarded,
                line_number,
                "thematic break carries no task meaning and was discarded",
            ));
            return Some(index + 1);
        }
        let (end, block, closed) = fenced_block(lines, index)?;
        if let Some(owner) = self.owner_for(indent_width(line)) {
            self.nodes[owner].annotations.push(block);
            self.last_owner = Some(owner);
        } else {
            self.preamble.extend(block.lines().map(str::to_owned));
        }
        if !closed {
            self.warnings.push(Warning::new(
                WarningCode::UnclosedFence,
                line_number,
                "unclosed fence was balanced during ledger emission",
            ));
        }
        self.after_blank = false;
        Some(end)
    }

    fn consume_item(&mut self, item: ListItem, line: usize) {
        if let Some((label, value, alias)) = parse_field(&item.title) {
            self.apply_field_to_owner(item.indent, label, value, alias, line);
            if item.noncanonical {
                self.warn_list_marker(line);
            }
        } else {
            self.add_item(item, line);
        }
    }

    fn consume_content(&mut self, line: &str, line_number: usize) {
        let indent = indent_width(line);
        let unescaped = unescape_prose(line);
        if unescaped != line.trim_start() {
            self.push_prose(indent, &unescaped);
            return;
        }
        if let Some((label, value, alias)) = parse_field(line.trim()) {
            self.apply_field_to_owner(indent, label, value, alias, line_number);
            return;
        }
        if let Some(label) = unsupported_field(line.trim()) {
            self.refusals.push(Refusal::new(
                RefusalCode::UnsupportedField,
                Some(line_number),
                format!("unsupported field: {label} (line {line_number})"),
            ));
            return;
        }
        self.push_prose(indent, line.trim_start());
    }

    fn apply_field_to_owner(
        &mut self,
        indent: usize,
        label: &'static str,
        value: &str,
        alias: bool,
        line: usize,
    ) {
        if let Some(owner) = self.owner_for(indent) {
            self.apply_field(owner, label, value, alias, line);
        } else {
            self.refusals.push(Refusal::new(
                RefusalCode::NoNodes,
                Some(line),
                "field appears before any task node",
            ));
        }
    }

    fn validate_context(&mut self) {
        if let Some(origin) = &self.context.origin
            && !valid_origin(origin)
        {
            self.refusals.push(Refusal::new(
                RefusalCode::InvalidOrigin,
                None,
                format!("invalid origin context: {origin}"),
            ));
        }
    }

    fn add_heading(&mut self, level: usize, title: String, line: usize, noncanonical: bool) {
        let parent = (0..level.saturating_sub(1))
            .rev()
            .find_map(|position| self.headings[position]);
        let id = self.add_node(NodeKind::Heading, title, line, parent, 0);
        self.headings[level - 1] = Some(id);
        for heading in &mut self.headings[level..] {
            *heading = None;
        }
        self.current_heading = Some(id);
        self.list_stack.clear();
        self.last_owner = Some(id);
        self.after_blank = false;
        if noncanonical {
            self.warnings.push(Warning::new(
                WarningCode::NonCanonicalHeading,
                line,
                "heading will normalize to an unindented ATX heading",
            ));
        }
    }

    fn add_item(&mut self, item: ListItem, line: usize) {
        while self
            .list_stack
            .last()
            .is_some_and(|(indent, _)| *indent >= item.indent)
        {
            self.list_stack.pop();
        }
        let parent = self
            .list_stack
            .last()
            .map(|(_, id)| *id)
            .or(self.current_heading);
        let content_col = item.indent + 2;
        let id = self.add_node(NodeKind::Item, item.title, line, parent, content_col);
        self.list_stack.push((item.indent, id));
        self.last_owner = Some(id);
        self.after_blank = false;
        if item.noncanonical {
            self.warn_list_marker(line);
        }
        if item.checked {
            self.warnings.push(Warning::new(
                WarningCode::CheckedStatusDiscarded,
                line,
                "checked status was discarded because documents carry intent, not runtime state",
            ));
        }
    }

    fn add_node(
        &mut self,
        kind: NodeKind,
        title: String,
        line: usize,
        parent: Option<usize>,
        content_col: usize,
    ) -> usize {
        let id = self.nodes.len();
        self.nodes.push(Node::new(id, kind, title, line, parent));
        self.node_content_cols.push(content_col);
        if let Some(parent) = parent {
            self.nodes[parent].children.push(id);
        }
        id
    }

    fn warn_list_marker(&mut self, line: usize) {
        self.warnings.push(Warning::new(
            WarningCode::NonCanonicalListMarker,
            line,
            "list item will normalize to a dash marker and two-space nesting",
        ));
    }

    fn owner_for(&self, indent: usize) -> Option<usize> {
        if !self.after_blank
            && let Some(owner) = self.last_owner
        {
            return Some(owner);
        }
        self.list_stack
            .iter()
            .rev()
            .find_map(|(_, id)| (indent >= self.node_content_cols[*id]).then_some(*id))
            .or(self.current_heading)
    }

    fn push_prose(&mut self, indent: usize, text: &str) {
        if let Some(owner) = self.owner_for(indent) {
            let relative = text
                .strip_prefix(&" ".repeat(self.node_content_cols[owner].min(indent)))
                .unwrap_or(text)
                .trim_end();
            append_description(&mut self.nodes[owner].description, relative);
            self.last_owner = Some(owner);
        } else {
            self.preamble.push(text.trim_end().to_owned());
        }
        self.after_blank = false;
    }

    fn push_blank(&mut self) {
        if let Some(owner) = self.last_owner {
            append_description(&mut self.nodes[owner].description, "");
        } else if !self.preamble.is_empty()
            && self.preamble.last().is_some_and(|line| !line.is_empty())
        {
            self.preamble.push(String::new());
        }
        self.after_blank = true;
    }

    fn apply_field(
        &mut self,
        owner: usize,
        field: &'static str,
        value: &str,
        alias: bool,
        line: usize,
    ) {
        if alias {
            self.warnings.push(Warning::new(
                WarningCode::FieldAlias,
                line,
                format!("field alias will normalize to {}", canonical_label(field)),
            ));
        }
        match field {
            "term" => {
                if value.is_empty() {
                    self.refusals.push(Refusal::new(
                        RefusalCode::UnsupportedField,
                        Some(line),
                        format!("empty Term field (line {line})"),
                    ));
                } else if value.contains('|') {
                    self.refusals.push(Refusal::new(
                        RefusalCode::UnsupportedField,
                        Some(line),
                        format!("term contains '|': {value} (line {line})"),
                    ));
                } else {
                    self.nodes[owner].term.push(value.to_owned());
                }
            }
            "after" => self.parse_after(owner, value, line),
            "priority" => {
                if self.nodes[owner].priority.is_some() {
                    self.warn_repeated(line, "Priority");
                }
                if let Some(priority) = Priority::parse(value) {
                    self.nodes[owner].priority = Some(priority);
                } else {
                    self.refusals.push(Refusal::new(
                        RefusalCode::InvalidPriority,
                        Some(line),
                        format!("invalid priority: {value} (line {line})"),
                    ));
                }
            }
            "flow" => {
                if !self.nodes[owner].flow.is_empty() {
                    self.warn_repeated(line, "Flow");
                    self.nodes[owner].flow.clear();
                }
                for value in split_commas(value) {
                    if let Some(phase) = Phase::parse(value) {
                        self.nodes[owner].flow.push(phase);
                    } else {
                        self.refusals.push(Refusal::new(
                            RefusalCode::InvalidFlowPhase,
                            Some(line),
                            format!("invalid flow phase: {value} (line {line})"),
                        ));
                    }
                }
            }
            "due" => {
                if self.nodes[owner].due.is_some() {
                    self.warn_repeated(line, "Due");
                }
                if value.is_empty() {
                    self.refusals.push(Refusal::new(
                        RefusalCode::UnsupportedField,
                        Some(line),
                        format!("empty Due field (line {line})"),
                    ));
                } else {
                    self.nodes[owner].due = Some(value.to_owned());
                }
            }
            _ => unreachable!("field table and parser agree"),
        }
        self.last_owner = Some(owner);
        self.after_blank = false;
    }

    fn parse_after(&mut self, owner: usize, value: &str, line: usize) {
        let parts = value.split(',').map(str::trim).collect::<Vec<_>>();
        if parts.is_empty() || parts.iter().any(|target| target.is_empty()) {
            self.refusals.push(Refusal::new(
                RefusalCode::MalformedDependency,
                Some(line),
                format!("malformed After target: empty target (line {line})"),
            ));
            return;
        }
        for target in parts {
            if !valid_dependency_target(target) {
                self.refusals.push(Refusal::new(
                    RefusalCode::MalformedDependency,
                    Some(line),
                    format!("malformed After target: {target} (line {line})"),
                ));
            } else {
                self.nodes[owner].after.push((target.to_owned(), line));
            }
        }
    }

    fn warn_repeated(&mut self, line: usize, field: &str) {
        self.warnings.push(Warning::new(
            WarningCode::RepeatedField,
            line,
            format!("repeated {field} field; the last value wins"),
        ));
    }

    fn finish(&mut self) -> Classification {
        self.trim_content();
        self.assign_slugs();
        let root = self.add_synthetic_root_if_needed();
        let mut edges = self.containment_edges();
        self.resolve_dependencies(&mut edges);
        self.detect_cycles(&edges);
        self.refusals.sort_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then_with(|| left.code.cmp(&right.code))
                .then_with(|| left.message.cmp(&right.message))
        });
        self.refusals.dedup();
        if !self.refusals.is_empty() || self.nodes.is_empty() {
            return Classification::Refused {
                refusals: std::mem::take(&mut self.refusals),
                warnings: std::mem::take(&mut self.warnings),
            };
        }
        let document = Document {
            origin: self.context.origin.clone(),
            nodes: std::mem::take(&mut self.nodes),
            root,
            edges,
        };
        let canonical = emit(&document);
        if canonical == self.original && self.warnings.is_empty() {
            Classification::Current(document)
        } else {
            if self.warnings.is_empty() {
                self.warnings.push(Warning::new(
                    WarningCode::NormalizationRequired,
                    1,
                    "document uses a recognized noncanonical spelling",
                ));
            }
            Classification::Convertible {
                document,
                warnings: std::mem::take(&mut self.warnings),
            }
        }
    }

    fn trim_content(&mut self) {
        for node in &mut self.nodes {
            node.title = node.title.trim().to_owned();
            node.description = collapse_blank_runs(&node.description);
        }
        while self.preamble.last().is_some_and(String::is_empty) {
            self.preamble.pop();
        }
    }

    fn assign_slugs(&mut self) {
        let naturals: Vec<String> = self.nodes.iter().map(|node| slugify(&node.title)).collect();
        for (node, natural) in self.nodes.iter_mut().zip(&naturals) {
            if natural.is_empty() {
                self.refusals.push(Refusal::new(
                    RefusalCode::TitleWithoutAsciiWords,
                    Some(node.line),
                    format!(
                        "title has no ASCII words: {} (line {})",
                        node.title, node.line
                    ),
                ));
            }
            node.slug.clone_from(natural);
        }
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, slug) in naturals.iter().enumerate() {
            groups.entry(slug.clone()).or_default().push(index);
        }
        for (base, indices) in groups {
            if base.is_empty() || indices.len() == 1 {
                continue;
            }
            let mut candidates = Vec::new();
            for index in &indices {
                let mut ancestors = Vec::new();
                let mut parent = self.nodes[*index].parent;
                while let Some(parent_index) = parent {
                    ancestors.push(naturals[parent_index].clone());
                    parent = self.nodes[parent_index].parent;
                }
                let candidate = ancestors
                    .into_iter()
                    .find(|ancestor| !ancestor.is_empty())
                    .map(|ancestor| format!("{ancestor}--{base}"))
                    .unwrap_or_else(|| base.clone());
                candidates.push(candidate);
            }
            let unique: BTreeSet<_> = candidates.iter().collect();
            if unique.len() == candidates.len() {
                for (index, candidate) in indices.into_iter().zip(candidates) {
                    self.nodes[index].slug = candidate;
                }
            } else {
                let lines = indices
                    .iter()
                    .map(|index| self.nodes[*index].line.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                self.refusals.push(Refusal::new(
                    RefusalCode::DuplicateIdentity,
                    indices.first().map(|index| self.nodes[*index].line),
                    format!("duplicate identity: {base} (lines {lines})"),
                ));
            }
        }
    }

    fn add_synthetic_root_if_needed(&mut self) -> usize {
        let roots: Vec<usize> = self
            .nodes
            .iter()
            .filter(|node| node.parent.is_none())
            .map(|node| node.id)
            .collect();
        if roots.len() == 1 {
            let root = roots[0];
            if !self.preamble.is_empty() {
                let preamble = collapse_blank_runs(&self.preamble.join("\n"));
                if !preamble.is_empty() {
                    let description = &mut self.nodes[root].description;
                    if !description.is_empty() {
                        *description = format!("{preamble}\n\n{description}");
                    } else {
                        *description = preamble;
                    }
                }
            }
            return root;
        }
        if let Some(index) = self
            .nodes
            .iter()
            .position(|node| node.slug == DOCUMENT_ROOT_SLUG)
        {
            self.refusals.push(Refusal::new(
                RefusalCode::SyntheticRootCollision,
                Some(self.nodes[index].line),
                format!(
                    "title collides with the synthetic root: {} (line {}); rename it or use one authored top-level root",
                    self.nodes[index].title, self.nodes[index].line
                ),
            ));
        }
        let id = self.nodes.len();
        let mut root = Node::new(
            id,
            NodeKind::SyntheticRoot,
            DOCUMENT_ROOT_TITLE.to_owned(),
            0,
            None,
        );
        root.slug = DOCUMENT_ROOT_SLUG.to_owned();
        root.children.clone_from(&roots);
        root.description = collapse_blank_runs(&self.preamble.join("\n"));
        for child in &roots {
            self.nodes[*child].parent = Some(id);
        }
        self.nodes.push(root);
        self.node_content_cols.push(0);
        id
    }

    fn containment_edges(&self) -> Vec<Edge> {
        self.nodes
            .iter()
            .filter_map(|node| {
                node.parent.map(|parent| Edge {
                    source: parent,
                    target: node.id,
                    kind: EdgeKind::Containment,
                })
            })
            .collect()
    }

    fn resolve_dependencies(&mut self, edges: &mut Vec<Edge>) {
        let mut final_slugs = BTreeMap::new();
        let mut natural_slugs: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for node in &self.nodes {
            final_slugs.insert(node.slug.clone(), node.id);
            natural_slugs
                .entry(slugify(&node.title))
                .or_default()
                .push(node.id);
        }
        for node in &self.nodes {
            for (raw, line) in &node.after {
                let target = if is_slug_spelling(raw) {
                    raw.to_ascii_lowercase()
                } else {
                    slugify(raw)
                };
                let resolved = if let Some(target_id) = final_slugs.get(&target) {
                    Some(*target_id)
                } else {
                    match natural_slugs.get(&target) {
                        Some(matches) if matches.len() == 1 => matches.first().copied(),
                        Some(_) => {
                            self.refusals.push(Refusal::new(
                                RefusalCode::AmbiguousDependency,
                                Some(*line),
                                format!(
                                    "ambiguous After target: {raw} (line {line}; use a qualified slug)"
                                ),
                            ));
                            None
                        }
                        None => {
                            self.refusals.push(Refusal::new(
                                RefusalCode::UnknownDependency,
                                Some(*line),
                                format!("unknown After target: {raw} (line {line})"),
                            ));
                            None
                        }
                    }
                };
                if let Some(target) = resolved {
                    let edge = Edge {
                        source: node.id,
                        target,
                        kind: EdgeKind::Explicit,
                    };
                    if !edges.contains(&edge) {
                        edges.push(edge);
                    }
                }
            }
        }
    }

    fn detect_cycles(&mut self, edges: &[Edge]) {
        let mut adjacency = vec![Vec::new(); self.nodes.len()];
        for edge in edges {
            adjacency[edge.source].push(edge.target);
        }
        let mut state = vec![0_u8; self.nodes.len()];
        for start in 0..self.nodes.len() {
            if state[start] == 0
                && let Some(cycle) = cycle_from(start, &adjacency, &mut state)
            {
                self.refusals.push(Refusal::new(
                    RefusalCode::DependencyCycle,
                    Some(self.nodes[cycle].line),
                    format!("dependency cycle at {}", self.nodes[cycle].slug),
                ));
                break;
            }
        }
    }
}

#[derive(Debug)]
struct ListItem {
    indent: usize,
    title: String,
    noncanonical: bool,
    checked: bool,
}

fn atx_heading(line: &str) -> Option<(usize, String, bool)> {
    let indent = indent_width(line);
    if indent > 3 {
        return None;
    }
    let stripped = line.trim_start();
    let level = stripped.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&level) || stripped.as_bytes().get(level) != Some(&b' ') {
        return None;
    }
    let raw = stripped[level + 1..].trim();
    let title = raw.trim_end_matches('#').trim_end();
    let canonical = indent == 0 && title == raw;
    Some((level, title.to_owned(), canonical))
}

fn setext_heading(lines: &[String], index: usize) -> Option<(usize, String)> {
    let title = lines.get(index)?.trim();
    let underline = lines.get(index + 1)?.trim();
    if title.is_empty() || atx_heading(title).is_some() {
        return None;
    }
    if underline.len() >= 2 && underline.bytes().all(|byte| byte == b'=') {
        Some((1, title.to_owned()))
    } else if underline.len() >= 2 && underline.bytes().all(|byte| byte == b'-') {
        Some((2, title.to_owned()))
    } else {
        None
    }
}

fn list_item(line: &str) -> Option<ListItem> {
    let indent = indent_width(line);
    let stripped = line.trim_start();
    let (marker_len, marker_is_dash) = if stripped.starts_with("- ") {
        (1, true)
    } else if stripped.starts_with("* ") || stripped.starts_with("+ ") {
        (1, false)
    } else {
        let digits = stripped.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0
            || !matches!(stripped.as_bytes().get(digits), Some(b'.' | b')'))
            || stripped.as_bytes().get(digits + 1) != Some(&b' ')
        {
            return None;
        }
        (digits + 1, false)
    };
    let mut title = stripped[marker_len + 1..].trim().to_owned();
    let mut checked = false;
    let mut checkbox = false;
    if title.len() >= 4 && title.starts_with('[') && title.as_bytes().get(2) == Some(&b']') {
        let state = title.as_bytes()[1];
        if matches!(state, b' ' | b'x' | b'X') && title.as_bytes().get(3) == Some(&b' ') {
            checked = matches!(state, b'x' | b'X');
            checkbox = true;
            title = title[4..].trim().to_owned();
        }
    }
    Some(ListItem {
        indent,
        title,
        noncanonical: !marker_is_dash || checkbox || !indent.is_multiple_of(2),
        checked,
    })
}

fn parse_field(line: &str) -> Option<(&'static str, &str, bool)> {
    for marker in ["***", "___", "**", "__", "*", "_"] {
        if let Some(rest) = line.strip_prefix(marker)
            && let Some(close) = rest.find(marker)
        {
            let emphasized = &rest[..close];
            let value = rest[close + marker.len()..].trim();
            if let Some(label) = emphasized.strip_suffix(':') {
                let normalized = label.trim().to_ascii_lowercase();
                if let Some((_, canonical)) = FIELD_NAMES
                    .iter()
                    .find(|(accepted, _)| normalized == *accepted)
                {
                    return Some((*canonical, value, true));
                }
            }
        }
    }
    let (label, value) = split_field(line)?;
    let bare = label
        .trim()
        .trim_matches('*')
        .trim_matches('_')
        .trim_end_matches(':')
        .trim();
    let normalized = bare.to_ascii_lowercase();
    FIELD_NAMES.iter().find_map(|(accepted, canonical)| {
        (normalized == *accepted).then_some((
            *canonical,
            value,
            normalized != *canonical || bare != canonical_label(canonical),
        ))
    })
}

fn split_field(line: &str) -> Option<(&str, &str)> {
    let colon = line.find(':')?;
    let label = &line[..colon];
    if label.is_empty()
        || !label
            .trim_matches(['*', '_', ' '])
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic() || byte == b' ')
    {
        return None;
    }
    Some((label, line[colon + 1..].trim()))
}

fn unsupported_field(line: &str) -> Option<&str> {
    let (label, _) = split_field(line)?;
    Some(label.trim_matches(['*', '_', ' ']))
}

fn canonical_label(field: &str) -> &'static str {
    match field {
        "term" => "Term",
        "after" => "After",
        "priority" => "Priority",
        "flow" => "Flow",
        "due" => "Due",
        _ => unreachable!("closed field vocabulary"),
    }
}

fn split_commas(value: &str) -> Vec<&str> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect()
}

fn valid_dependency_target(target: &str) -> bool {
    !target.is_empty()
        && target.bytes().any(|byte| byte.is_ascii_alphanumeric())
        && target.bytes().all(valid_dependency_byte)
}

fn valid_dependency_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b" -_./()".contains(&byte)
}

fn is_slug_spelling(target: &str) -> bool {
    !target.is_empty()
        && target
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_origin(origin: &str) -> bool {
    let Some((kind, value)) = origin.split_once(':') else {
        return false;
    };
    matches!(kind, "ack" | "task")
        && !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn indent_width(line: &str) -> usize {
    line.chars()
        .take_while(|ch| ch.is_whitespace() && *ch != '\n' && *ch != '\r')
        .map(|ch| if ch == '\t' { 4 } else { 1 })
        .sum()
}

fn unescape_prose(line: &str) -> String {
    let stripped = line.trim_start();
    if let Some(rest) = stripped.strip_prefix('\\')
        && rest
            .chars()
            .next()
            .is_some_and(|ch| "-*+#>|`=~_[<".contains(ch))
    {
        return rest.to_owned();
    }
    if let Some(colon) = stripped.find("\\:")
        && stripped[..colon]
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic() || byte == b' ')
    {
        let mut output = stripped.to_owned();
        output.remove(colon);
        return output;
    }
    stripped.to_owned()
}

fn append_description(description: &mut String, line: &str) {
    if line.is_empty() {
        if !description.is_empty() && !description.ends_with("\n\n") {
            description.push_str("\n\n");
        }
    } else {
        if !description.is_empty() && !description.ends_with('\n') {
            description.push('\n');
        }
        description.push_str(line);
    }
}

fn collapse_blank_runs(text: &str) -> String {
    let mut output = Vec::new();
    let mut blank = true;
    for line in text.lines() {
        if line.trim().is_empty() {
            if !blank {
                output.push(String::new());
            }
            blank = true;
        } else {
            output.push(line.trim_end().to_owned());
            blank = false;
        }
    }
    while output.last().is_some_and(String::is_empty) {
        output.pop();
    }
    output.join("\n")
}

fn is_thematic_break(line: &str) -> bool {
    let compact: String = line
        .trim()
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    compact.len() >= 3
        && compact
            .chars()
            .all(|ch| ch == '-' || ch == '*' || ch == '_')
        && compact
            .chars()
            .all(|ch| ch == compact.chars().next().unwrap_or('-'))
}

fn fenced_block(lines: &[String], start: usize) -> Option<(usize, String, bool)> {
    let opener_line = lines.get(start)?;
    let opener_indent = indent_width(opener_line);
    let opener = opener_line.trim_start();
    let marker = if opener.starts_with("```") {
        '`'
    } else if opener.starts_with("~~~") {
        '~'
    } else {
        return None;
    };
    let marker_len = opener.chars().take_while(|ch| *ch == marker).count();
    let mut block = vec![lines[start].trim_start().to_owned()];
    for (index, line) in lines.iter().enumerate().skip(start + 1) {
        let trimmed = line.trim();
        block.push(strip_container_indent(line, opener_indent));
        if trimmed.len() >= marker_len && trimmed.chars().all(|ch| ch == marker) {
            return Some((index + 1, block.join("\n"), true));
        }
    }
    block.push(marker.to_string().repeat(marker_len));
    Some((lines.len(), block.join("\n"), false))
}

fn strip_container_indent(line: &str, width: usize) -> String {
    let mut consumed = 0;
    let mut byte_end = 0;
    for (index, ch) in line.char_indices() {
        if consumed >= width || !ch.is_whitespace() || matches!(ch, '\n' | '\r') {
            break;
        }
        let columns = if ch == '\t' { 4 } else { 1 };
        if consumed + columns > width {
            break;
        }
        consumed += columns;
        byte_end = index + ch.len_utf8();
    }
    line[byte_end..].to_owned()
}

fn cycle_from(start: usize, adjacency: &[Vec<usize>], state: &mut [u8]) -> Option<usize> {
    state[start] = 1;
    for target in &adjacency[start] {
        match state[*target] {
            0 => {
                if let Some(cycle) = cycle_from(*target, adjacency, state) {
                    return Some(cycle);
                }
            }
            1 => return Some(*target),
            _ => {}
        }
    }
    state[start] = 2;
    None
}
