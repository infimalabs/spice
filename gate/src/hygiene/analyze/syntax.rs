use super::*;

const AST_NODE_LIMIT: usize = 1_000_000;
const AST_RETAINED_TEXT_LIMIT: usize = 64 * 1024 * 1024;
const _: () = assert!(AST_NODE_LIMIT > 0 && AST_RETAINED_TEXT_LIMIT > 0);

#[derive(Clone, Copy)]
pub(super) enum AstFailure {
    Nodes,
    RetainedText,
    Deadline,
}

pub(in crate::hygiene) struct AstWork {
    started: Instant,
    deadline_ms: u64,
    nodes: usize,
    retained_text: usize,
    failure: Option<AstFailure>,
}

impl AstWork {
    pub(in crate::hygiene) fn new(started: Instant, deadline_ms: u64) -> Self {
        Self {
            started,
            deadline_ms,
            nodes: 0,
            retained_text: 0,
            failure: None,
        }
    }

    fn admit_node(&mut self) -> bool {
        if !self.check_deadline() {
            return false;
        }
        let next = self.nodes.saturating_add(1);
        if next > AST_NODE_LIMIT {
            self.failure = Some(AstFailure::Nodes);
            return false;
        }
        self.nodes = next;
        true
    }

    pub(in crate::hygiene) fn retain_text(&mut self, bytes: usize) -> bool {
        if !self.check_deadline() {
            return false;
        }
        let next = self.retained_text.saturating_add(bytes);
        if next > AST_RETAINED_TEXT_LIMIT {
            self.failure = Some(AstFailure::RetainedText);
            return false;
        }
        self.retained_text = next;
        true
    }

    pub(super) fn check_deadline(&mut self) -> bool {
        if self.failure.is_some() {
            return false;
        }
        if u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX) > self.deadline_ms
        {
            self.failure = Some(AstFailure::Deadline);
            false
        } else {
            true
        }
    }

    pub(super) const fn failure(&self) -> Option<AstFailure> {
        self.failure
    }

    pub(super) fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub(in crate::hygiene) fn diagnostic(&self) -> String {
        match self.failure {
            Some(AstFailure::Nodes) => format!(
                "AST traversal exceeded the {AST_NODE_LIMIT}-statement/expression node limit"
            ),
            Some(AstFailure::RetainedText) => format!(
                "AST traversal exceeded the {AST_RETAINED_TEXT_LIMIT}-byte retained syntax limit"
            ),
            Some(AstFailure::Deadline) => format!(
                "AST traversal exceeded the {}ms hygiene deadline",
                self.deadline_ms
            ),
            None => "AST traversal completed".to_owned(),
        }
    }
}

#[derive(Clone)]
pub(super) struct Module {
    pub(super) path: String,
    pub(super) name: String,
    pub(super) is_package_initializer: bool,
    pub(super) is_test: bool,
    line_starts: Vec<usize>,
    pub(super) imports: Vec<ImportRecord>,
    pub(super) declarations: Vec<Declaration>,
    pub(super) references: Vec<Reference>,
    pub(super) tests: Vec<TestCase>,
    pub(super) private_couplings: Vec<RawPrivateCoupling>,
}

impl Module {
    pub(super) fn new(
        path: String,
        name: String,
        is_test: bool,
        source: &str,
        work: &mut AstWork,
    ) -> Option<Self> {
        if !work.retain_text(path.len().saturating_add(name.len())) {
            return None;
        }
        let is_package_initializer = is_package_initializer_path(&path);
        let mut line_starts = vec![0];
        line_starts.extend(
            source
                .bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(index, _)| index + 1),
        );
        Some(Self {
            path,
            name,
            is_package_initializer,
            is_test,
            line_starts,
            imports: Vec::new(),
            declarations: Vec::new(),
            references: Vec::new(),
            tests: Vec::new(),
            private_couplings: Vec::new(),
        })
    }

    fn line(&self, offset: rustpython_ast::text_size::TextSize) -> u32 {
        let offset = usize::try_from(u32::from(offset)).unwrap_or(usize::MAX);
        u32::try_from(self.line_starts.partition_point(|start| *start <= offset))
            .unwrap_or(u32::MAX)
    }

    pub(super) fn resolve_reference(
        &self,
        reference: &str,
        known: &BTreeSet<String>,
        declarations: &BTreeSet<(String, String)>,
    ) -> Option<(String, String)> {
        let (head, tail) = reference
            .split_once('.')
            .map_or((reference, ""), |(head, tail)| (head, tail));
        for import in &self.imports {
            if import.local != head {
                continue;
            }
            if let Some(symbol) = &import.symbol {
                let candidate = format!("{}.{}", import.module, symbol);
                if known.contains(&candidate) {
                    return Some((candidate, tail.to_owned()));
                }
                let base = longest_known_prefix(&import.module, known)?;
                let symbol = if tail.is_empty() {
                    symbol.clone()
                } else {
                    format!("{symbol}.{tail}")
                };
                return Some((base, symbol));
            }
            let combined = if import.explicit_alias {
                if tail.is_empty() {
                    import.module.clone()
                } else {
                    format!("{}.{tail}", import.module)
                }
            } else if tail.is_empty() {
                head.to_owned()
            } else {
                format!("{head}.{tail}")
            };
            let base = longest_known_prefix(&combined, known)?;
            let symbol = combined
                .strip_prefix(&base)
                .unwrap_or_default()
                .trim_start_matches('.')
                .to_owned();
            return Some((base, symbol));
        }
        if declarations.contains(&(self.name.clone(), reference.to_owned())) {
            return Some((self.name.clone(), reference.to_owned()));
        }
        None
    }
}

#[derive(Clone)]
pub(super) struct ImportRecord {
    module: String,
    symbol: Option<String>,
    local: String,
    explicit_alias: bool,
}

impl ImportRecord {
    pub(super) fn target_module(&self, known: &BTreeSet<String>) -> Option<String> {
        if let Some(symbol) = &self.symbol {
            let candidate = format!("{}.{}", self.module, symbol);
            if known.contains(&candidate) {
                return Some(candidate);
            }
        }
        longest_known_prefix(&self.module, known)
    }
}

#[derive(Clone)]
pub(super) struct Declaration {
    pub(super) name: String,
    pub(super) kind: &'static str,
    pub(super) line: u32,
    pub(super) top_level: bool,
}

#[derive(Clone)]
pub(super) struct Reference {
    pub(super) name: String,
    pub(super) owner: Option<String>,
}

#[derive(Clone)]
pub(super) struct TestCase {
    pub(super) name: String,
    pub(super) line: u32,
    pub(super) assertions: u32,
}

#[derive(Clone)]
pub(super) struct RawPrivateCoupling {
    pub(super) line: u32,
    pub(super) test_name: String,
    pub(super) kind: &'static str,
    pub(super) target: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct PrivateCoupling {
    pub(super) path: String,
    pub(super) line: u32,
    pub(super) test_name: String,
    pub(super) kind: &'static str,
    pub(super) target: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct ResolvedReference {
    pub(super) target_module: String,
    pub(super) symbol: String,
    pub(super) source_module: String,
    pub(super) source_symbol: Option<String>,
    pub(super) path: String,
    pub(super) from_test: bool,
}

impl ResolvedReference {
    pub(super) fn matches(&self, module: &str, declaration: &Declaration) -> bool {
        self.target_module == module && self.symbol == declaration.name
    }
}

#[derive(Clone)]
enum ScopeKind {
    Class { is_test: bool },
    Function,
}

#[derive(Clone)]
struct Scope {
    name: String,
    kind: ScopeKind,
    declaration_name: Option<String>,
    test_name: Option<String>,
    assertions: u32,
}

pub(super) struct AstCollector<'a> {
    module: &'a mut Module,
    work: &'a mut AstWork,
    scopes: Vec<Scope>,
    assertion_depth: usize,
}

impl<'a> AstCollector<'a> {
    pub(super) fn new(module: &'a mut Module, work: &'a mut AstWork) -> Self {
        Self {
            module,
            work,
            scopes: Vec::new(),
            assertion_depth: 0,
        }
    }

    fn current_test_name(&self) -> String {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.test_name.clone())
            .unwrap_or_else(|| "<module>".to_owned())
    }

    fn current_declaration_name(&self) -> Option<String> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.declaration_name.clone())
    }

    fn record_assertion(&mut self) {
        if let Some(scope) = self
            .scopes
            .iter_mut()
            .rev()
            .find(|scope| scope.test_name.is_some())
        {
            scope.assertions = scope.assertions.saturating_add(1);
        }
    }

    fn qualify(&self, name: &str) -> String {
        let mut parts: Vec<_> = self
            .scopes
            .iter()
            .map(|scope| scope.name.as_str())
            .collect();
        parts.push(name);
        parts.join(".")
    }

    fn private_coupling(&mut self, line: u32, kind: &'static str, target: String) {
        if self.module.is_test && has_private_component(&target) {
            let test_name = self.current_test_name();
            if !self
                .work
                .retain_text(test_name.len().saturating_add(target.len()))
            {
                return;
            }
            self.module.private_couplings.push(RawPrivateCoupling {
                line,
                test_name,
                kind,
                target,
            });
        }
    }

    fn visit_function(
        &mut self,
        name: String,
        range: rustpython_ast::text_size::TextRange,
        body: impl FnOnce(&mut Self),
    ) {
        let line = self.module.line(range.start());
        let parent_test_class = self
            .scopes
            .last()
            .is_some_and(|scope| matches!(scope.kind, ScopeKind::Class { is_test: true }));
        let is_test = self.module.is_test
            && name.starts_with("test_")
            && (self.scopes.is_empty() || parent_test_class);
        let test_name = is_test.then(|| self.qualify(&name));
        let is_declaration = !self.module.is_test
            && (self.scopes.is_empty()
                || matches!(
                    self.scopes.last().map(|scope| &scope.kind),
                    Some(ScopeKind::Class { .. })
                ));
        let declaration_name = is_declaration.then(|| self.qualify(&name));
        if let Some(declaration_name) = &declaration_name {
            if !self.work.retain_text(declaration_name.len()) {
                return;
            }
            self.module.declarations.push(Declaration {
                name: declaration_name.clone(),
                kind: if self.scopes.is_empty() {
                    "function"
                } else {
                    "method"
                },
                line,
                top_level: self.scopes.is_empty(),
            });
        }
        let scope_bytes = name
            .len()
            .saturating_add(declaration_name.as_ref().map_or(0, String::len))
            .saturating_add(test_name.as_ref().map_or(0, String::len));
        if !self.work.retain_text(scope_bytes) {
            return;
        }
        self.scopes.push(Scope {
            name,
            kind: ScopeKind::Function,
            declaration_name,
            test_name: test_name.clone(),
            assertions: 0,
        });
        body(self);
        let scope = self.scopes.pop().expect("function scope was pushed");
        if let Some(test_name) = test_name {
            if !self.work.retain_text(test_name.len()) {
                return;
            }
            self.module.tests.push(TestCase {
                name: test_name,
                line,
                assertions: scope.assertions,
            });
        }
    }
}

impl Visitor for AstCollector<'_> {
    fn visit_stmt(&mut self, node: rustpython_ast::Stmt) {
        if self.work.admit_node() {
            self.generic_visit_stmt(node);
        }
    }

    fn visit_expr(&mut self, node: rustpython_ast::Expr) {
        if self.work.admit_node() {
            self.generic_visit_expr(node);
        }
    }

    fn visit_stmt_function_def(&mut self, node: rustpython_ast::StmtFunctionDef) {
        let name = node.name.to_string();
        let range = node.range;
        self.visit_function(name, range, |collector| {
            collector.generic_visit_stmt_function_def(node);
        });
    }

    fn visit_stmt_async_function_def(&mut self, node: rustpython_ast::StmtAsyncFunctionDef) {
        let name = node.name.to_string();
        let range = node.range;
        self.visit_function(name, range, |collector| {
            collector.generic_visit_stmt_async_function_def(node);
        });
    }

    fn visit_stmt_class_def(&mut self, node: rustpython_ast::StmtClassDef) {
        let name = node.name.to_string();
        let line = self.module.line(node.range.start());
        let top_level = self.scopes.is_empty();
        if !self.module.is_test && top_level {
            if !self.work.retain_text(name.len()) {
                return;
            }
            self.module.declarations.push(Declaration {
                name: name.clone(),
                kind: "class",
                line,
                top_level: true,
            });
        }
        let is_test = self.module.is_test && top_level && name.starts_with("Test");
        let declaration_name = (!self.module.is_test && top_level).then(|| name.clone());
        if !self.work.retain_text(
            name.len()
                .saturating_add(declaration_name.as_ref().map_or(0, String::len)),
        ) {
            return;
        }
        self.scopes.push(Scope {
            declaration_name,
            name,
            kind: ScopeKind::Class { is_test },
            test_name: None,
            assertions: 0,
        });
        self.generic_visit_stmt_class_def(node);
        self.scopes.pop();
    }

    fn visit_stmt_import(&mut self, node: rustpython_ast::StmtImport) {
        let line = self.module.line(node.range.start());
        for alias in &node.names {
            let imported = alias.name.to_string();
            let local = alias.asname.as_ref().map_or_else(
                || imported.split('.').next().unwrap_or("").to_owned(),
                ToString::to_string,
            );
            if !self
                .work
                .retain_text(imported.len().saturating_add(local.len()))
            {
                return;
            }
            self.module.imports.push(ImportRecord {
                module: imported.clone(),
                symbol: None,
                local,
                explicit_alias: alias.asname.is_some(),
            });
            self.private_coupling(line, "private import", imported);
        }
        self.generic_visit_stmt_import(node);
    }

    fn visit_stmt_import_from(&mut self, node: rustpython_ast::StmtImportFrom) {
        let line = self.module.line(node.range.start());
        let module = resolve_relative_module(
            &self.module.name,
            self.module.is_package_initializer,
            node.level.as_ref().map_or(0, rustpython_ast::Int::to_usize),
            node.module.as_ref().map(ToString::to_string),
        );
        for alias in &node.names {
            let symbol = alias.name.to_string();
            let local = alias
                .asname
                .as_ref()
                .map_or_else(|| symbol.clone(), ToString::to_string);
            let retained = module
                .len()
                .saturating_add(symbol.len())
                .saturating_add(local.len());
            if !self.work.retain_text(retained) {
                return;
            }
            self.module.imports.push(ImportRecord {
                module: module.clone(),
                symbol: Some(symbol.clone()),
                local,
                explicit_alias: true,
            });
            self.private_coupling(line, "private import", format!("{module}.{symbol}"));
        }
        self.generic_visit_stmt_import_from(node);
    }

    fn visit_stmt_assert(&mut self, node: rustpython_ast::StmtAssert) {
        self.record_assertion();
        self.assertion_depth = self.assertion_depth.saturating_add(1);
        self.generic_visit_stmt_assert(node);
        self.assertion_depth = self.assertion_depth.saturating_sub(1);
    }

    fn visit_expr_call(&mut self, node: rustpython_ast::ExprCall) {
        if let Some(path) = expression_path(&node.func) {
            let final_name = path.rsplit('.').next().unwrap_or(&path);
            if final_name.starts_with("assert") {
                self.record_assertion();
            }
        }
        self.generic_visit_expr_call(node);
    }

    fn visit_expr_attribute(&mut self, node: rustpython_ast::ExprAttribute) {
        if node.ctx == ExprContext::Load
            && let Some(base) = expression_path(&node.value)
        {
            let path = format!("{base}.{}", node.attr);
            let owner = self.current_declaration_name();
            if !self.work.retain_text(
                path.len()
                    .saturating_add(owner.as_ref().map_or(0, String::len)),
            ) {
                return;
            }
            self.module.references.push(Reference {
                name: path.clone(),
                owner,
            });
            if self.assertion_depth > 0 {
                let line = self.module.line(node.range.start());
                self.private_coupling(line, "private assertion", path);
            }
        }
        self.generic_visit_expr_attribute(node);
    }

    fn visit_expr_name(&mut self, node: rustpython_ast::ExprName) {
        if node.ctx == ExprContext::Load {
            let owner = self.current_declaration_name();
            let name = node.id.to_string();
            if !self.work.retain_text(
                name.len()
                    .saturating_add(owner.as_ref().map_or(0, String::len)),
            ) {
                return;
            }
            self.module.references.push(Reference { name, owner });
        }
        self.generic_visit_expr_name(node);
    }
}

fn expression_path(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Name(node) => Some(node.id.to_string()),
        Expr::Attribute(node) => {
            expression_path(&node.value).map(|base| format!("{base}.{}", node.attr))
        }
        Expr::Call(node) => expression_path(&node.func),
        _ => None,
    }
}

pub(super) fn resolve_relative_module(
    current: &str,
    is_package_initializer: bool,
    level: usize,
    module: Option<String>,
) -> String {
    if level == 0 {
        return module.unwrap_or_default();
    }
    let mut parts: Vec<_> = if current.is_empty() {
        Vec::new()
    } else {
        current.split('.').map(str::to_owned).collect()
    };
    if !is_package_initializer && !parts.is_empty() {
        parts.pop();
    }
    for _ in 1..level {
        parts.pop();
    }
    if let Some(module) = module {
        parts.extend(module.split('.').map(str::to_owned));
    }
    parts.join(".")
}

fn is_package_initializer_path(path: &str) -> bool {
    matches!(
        path.rsplit('/').next(),
        Some("__init__.py" | "__init__.pyi")
    )
}

pub(super) fn module_name(path: &str) -> String {
    let without_extension = path
        .strip_suffix(".pyi")
        .or_else(|| path.strip_suffix(".py"))
        .unwrap_or(path);
    let mut components: Vec<_> = without_extension.split('/').collect();
    if components.last() == Some(&"__init__") {
        components.pop();
    }
    components.join(".")
}

pub(in crate::hygiene) fn is_python_path(path: &str) -> bool {
    path.ends_with(".py") || path.ends_with(".pyi")
}

pub(super) fn path_is_under_roots(path: &str, roots: &[String]) -> bool {
    roots.iter().any(|root| {
        path == root
            || root.strip_suffix('/').is_some_and(|root| {
                path.strip_prefix(root)
                    .is_some_and(|suffix| suffix.starts_with('/'))
            })
            || (!root.ends_with(".py")
                && path
                    .strip_prefix(root)
                    .is_some_and(|suffix| suffix.starts_with('/')))
    })
}

fn longest_known_prefix(value: &str, known: &BTreeSet<String>) -> Option<String> {
    let mut candidate = value;
    loop {
        if known.contains(candidate) {
            return Some(candidate.to_owned());
        }
        candidate = candidate.rsplit_once('.')?.0;
    }
}

pub(super) fn has_private_component(target: &str) -> bool {
    target
        .split('.')
        .any(|part| part.starts_with('_') && !part.starts_with("__"))
}
