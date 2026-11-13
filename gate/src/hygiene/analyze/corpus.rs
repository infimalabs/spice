use super::*;

mod resolve;

const GRAPH_SYMBOL_LIMIT: usize = 1_000_000;
const GRAPH_EDGE_LIMIT: usize = 1_000_000;
const GRAPH_WORKLIST_LIMIT: usize = 1_000_000;
const GRAPH_FINDING_LIMIT: usize = 1_000_000;
const GRAPH_DIAGNOSTIC_BYTES_LIMIT: usize = 64 * 1024 * 1024;
const GRAPH_STEP_LIMIT: usize = 10_000_000;
const _: () = assert!(
    GRAPH_SYMBOL_LIMIT > 0
        && GRAPH_EDGE_LIMIT > 0
        && GRAPH_WORKLIST_LIMIT > 0
        && GRAPH_FINDING_LIMIT > 0
        && GRAPH_DIAGNOSTIC_BYTES_LIMIT > 0
        && GRAPH_STEP_LIMIT > 0
);

#[derive(Clone, Copy)]
pub(super) enum GraphFailure {
    Symbols,
    Edges,
    Worklist,
    Findings,
    Diagnostics,
    Steps,
    Deadline,
}

pub(super) struct GraphWork {
    started: Instant,
    deadline_ms: u64,
    edges: usize,
    findings: usize,
    diagnostic_bytes: usize,
    steps: usize,
    failure: Option<GraphFailure>,
}

impl GraphWork {
    pub(super) fn new(started: Instant, deadline_ms: u64) -> Self {
        Self {
            started,
            deadline_ms,
            edges: 0,
            findings: 0,
            diagnostic_bytes: 0,
            steps: 0,
            failure: None,
        }
    }

    pub(super) fn preflight(&mut self, corpus: &Corpus) -> bool {
        let symbols = corpus.modules.values().fold(0_usize, |total, module| {
            total
                .saturating_add(module.imports.len())
                .saturating_add(module.declarations.len())
                .saturating_add(module.references.len())
                .saturating_add(module.tests.len())
                .saturating_add(module.private_couplings.len())
        });
        if symbols > GRAPH_SYMBOL_LIMIT {
            self.failure = Some(GraphFailure::Symbols);
            return false;
        }
        let possible_findings = corpus
            .modules
            .len()
            .saturating_add(
                corpus
                    .modules
                    .values()
                    .fold(0_usize, |total, module| {
                        total.saturating_add(module.declarations.len())
                    })
                    .saturating_mul(2),
            )
            .saturating_add(corpus.modules.values().fold(0_usize, |total, module| {
                total
                    .saturating_add(module.tests.len())
                    .saturating_add(module.private_couplings.len())
            }));
        if possible_findings > GRAPH_FINDING_LIMIT {
            self.failure = Some(GraphFailure::Findings);
            return false;
        }
        self.check()
    }

    pub(super) fn check(&mut self) -> bool {
        if self.failure.is_some() {
            return false;
        }
        if u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX) > self.deadline_ms
        {
            self.failure = Some(GraphFailure::Deadline);
            false
        } else {
            true
        }
    }

    fn step(&mut self) -> bool {
        self.steps(1)
    }

    pub(super) fn steps(&mut self, count: usize) -> bool {
        if !self.check() {
            return false;
        }
        let next = self.steps.saturating_add(count);
        if next > GRAPH_STEP_LIMIT {
            self.failure.get_or_insert(GraphFailure::Steps);
            return false;
        }
        self.steps = next;
        true
    }

    fn admit_edge(&mut self) -> bool {
        if !self.step() || self.edges == GRAPH_EDGE_LIMIT {
            self.failure.get_or_insert(GraphFailure::Edges);
            return false;
        }
        self.edges += 1;
        true
    }

    fn admit_worklist(&mut self, length: usize) -> bool {
        if length >= GRAPH_WORKLIST_LIMIT {
            self.failure = Some(GraphFailure::Worklist);
            false
        } else {
            self.step()
        }
    }

    fn admit_finding(&mut self, bytes: usize) -> bool {
        if !self.step() || self.findings == GRAPH_FINDING_LIMIT {
            self.failure.get_or_insert(GraphFailure::Findings);
            return false;
        }
        let next = self.diagnostic_bytes.saturating_add(bytes);
        if next > GRAPH_DIAGNOSTIC_BYTES_LIMIT {
            self.failure = Some(GraphFailure::Diagnostics);
            return false;
        }
        self.findings += 1;
        self.diagnostic_bytes = next;
        true
    }

    pub(super) const fn failure(&self) -> Option<GraphFailure> {
        self.failure
    }

    pub(super) fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub(super) fn diagnostic(&self) -> String {
        let bound = match self.failure {
            Some(GraphFailure::Symbols) => "symbol",
            Some(GraphFailure::Edges) => "edge",
            Some(GraphFailure::Worklist) => "worklist",
            Some(GraphFailure::Findings) => "finding",
            Some(GraphFailure::Diagnostics) => "diagnostic-byte",
            Some(GraphFailure::Steps) => "graph-step",
            Some(GraphFailure::Deadline) => "deadline",
            None => "completed",
        };
        format!(
            "hygiene graph {bound} bound exhausted after {} steps",
            self.steps
        )
    }
}

#[derive(Default)]
pub(super) struct Corpus {
    modules: BTreeMap<String, Module>,
    pub(super) edges: BTreeSet<(String, String)>,
    production_roots: BTreeSet<String>,
    test_modules: BTreeSet<String>,
    production_reachable: BTreeSet<String>,
    test_reachable: BTreeSet<String>,
}

impl Corpus {
    pub(super) fn load(
        documents: &[HygieneDocument],
        configuration: &HygieneConfiguration,
        started: Instant,
        reached_paths: Option<&BTreeSet<String>>,
    ) -> (u32, Self, Terminal) {
        let documents = documents
            .iter()
            .filter(|document| reached_paths.is_none_or(|reached| reached.contains(&document.path)))
            .collect::<Vec<_>>();
        let selected_files = u32::try_from(documents.len()).unwrap_or(u32::MAX);
        let mut corpus = Self::default();
        let mut ast_work = AstWork::new(started, DEADLINE_MS);
        for (index, document) in documents.iter().enumerate() {
            let path = &document.path;
            let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            if elapsed_ms > DEADLINE_MS {
                return (selected_files, corpus, Terminal::TimedOut { elapsed_ms });
            }
            let source = match std::str::from_utf8(&document.bytes) {
                Ok(source) => source,
                Err(error) => {
                    return (
                        selected_files,
                        corpus,
                        Terminal::ParseFailure {
                            documents: u32::try_from(index).unwrap_or(u32::MAX),
                            path: path.clone(),
                            diagnostic: format!("source is not UTF-8: {error}"),
                        },
                    );
                }
            };
            let suite = match rustpython_ast::Suite::parse(source, path) {
                Ok(suite) => suite,
                Err(error) => {
                    return (
                        selected_files,
                        corpus,
                        Terminal::ParseFailure {
                            documents: u32::try_from(index).unwrap_or(u32::MAX),
                            path: path.clone(),
                            diagnostic: error.to_string(),
                        },
                    );
                }
            };
            let name = module_name(path);
            let is_test = path_is_under_roots(path, &configuration.test_roots);
            let Some(mut module) =
                Module::new(path.clone(), name.clone(), is_test, source, &mut ast_work)
            else {
                return ast_terminal(selected_files, corpus, index, path, &ast_work);
            };
            {
                let mut collector = AstCollector::new(&mut module, &mut ast_work);
                for statement in suite {
                    collector.visit_stmt(statement);
                }
            }
            if ast_work.failure().is_some() {
                return ast_terminal(selected_files, corpus, index, path, &ast_work);
            }
            if let Some(existing_path) = corpus.modules.get(&name).map(|item| item.path.clone()) {
                if existing_path.ends_with(".py") && path.ends_with(".pyi") {
                    continue;
                }
                if !(existing_path.ends_with(".pyi") && path.ends_with(".py")) {
                    return (
                        selected_files,
                        corpus,
                        Terminal::ParseFailure {
                            documents: u32::try_from(index).unwrap_or(u32::MAX),
                            path: path.clone(),
                            diagnostic: format!(
                                concat!(
                                    "selected paths {} and {} resolve to the same ",
                                    "Python module {}"
                                ),
                                existing_path, path, name
                            ),
                        },
                    );
                }
            }
            corpus.modules.insert(name, module);
            let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            if elapsed_ms > DEADLINE_MS {
                return (selected_files, corpus, Terminal::TimedOut { elapsed_ms });
            }
        }
        let mut graph_work = GraphWork::new(started, DEADLINE_MS);
        if !graph_work.preflight(&corpus)
            || !corpus.construct_graphs(configuration, &mut graph_work)
        {
            return graph_terminal(selected_files, corpus, &graph_work);
        }
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let terminal = if elapsed_ms > DEADLINE_MS {
            Terminal::TimedOut { elapsed_ms }
        } else {
            Terminal::Parsed
        };
        (selected_files, corpus, terminal)
    }

    fn construct_graphs(
        &mut self,
        configuration: &HygieneConfiguration,
        work: &mut GraphWork,
    ) -> bool {
        let known: BTreeSet<_> = self.modules.keys().cloned().collect();
        for (name, module) in &self.modules {
            if !work.step() {
                return false;
            }
            if module.is_test {
                self.test_modules.insert(name.clone());
            }
            if !module.is_test && path_is_under_roots(&module.path, &configuration.production_roots)
            {
                self.production_roots.insert(name.clone());
            }
            for import in &module.imports {
                if !work.step() {
                    return false;
                }
                if let Some(target) = import.target_module(&known) {
                    let edge = (name.clone(), target);
                    if !self.edges.contains(&edge) {
                        if !work.admit_edge() {
                            return false;
                        }
                        self.edges.insert(edge);
                    }
                }
            }
        }
        let Some(production_reachable) =
            reachable_bounded(&self.production_roots, &self.edges, work)
        else {
            return false;
        };
        let Some(test_reachable) = reachable_bounded(&self.test_modules, &self.edges, work) else {
            return false;
        };
        self.production_reachable = production_reachable;
        self.test_reachable = test_reachable;
        true
    }

    pub(super) fn applicable(&self, gate: GateId) -> bool {
        let graph = !self.modules.is_empty()
            && !self.production_roots.is_empty()
            && !self.test_modules.is_empty();
        let declarations = self
            .declarations()
            .any(|(_, declaration)| declaration.top_level);
        let tests = self.tests().next().is_some();
        match gate {
            GateId::Reachability => graph,
            GateId::SymbolReachability | GateId::PythonUnused => graph && declarations,
            GateId::AssertionFreeTests => tests,
            GateId::PrivateInternals => tests && !self.production_packages().is_empty(),
            _ => false,
        }
    }

    pub(super) fn traversal_evidence(
        &self,
        traversal: HygieneTraversal,
        work: &mut GraphWork,
    ) -> Option<AnalysisEvidence> {
        let evidence = match traversal {
            HygieneTraversal::ImportGraph => AnalysisEvidence::GraphConstruction {
                production_roots: self.root_paths(&self.production_roots),
                test_roots: self.root_paths(&self.test_modules),
                nodes: u32::try_from(self.modules.len()).unwrap_or(u32::MAX),
                edges: u32::try_from(self.edges.len()).unwrap_or(u32::MAX),
                reach: self.suite_reach(work)?,
            },
            HygieneTraversal::SymbolGraph | HygieneTraversal::PythonDeclarations => {
                let resolved = self.resolved_references(work)?;
                let reachable = self.production_symbols(&resolved, work)?;
                AnalysisEvidence::SymbolExtraction {
                    declarations: u32::try_from(self.declarations().count()).unwrap_or(u32::MAX),
                    production_references: u32::try_from(
                        resolved
                            .iter()
                            .filter(|item| {
                                !item.from_test
                                    && item.source_symbol.as_ref().is_none_or(|symbol| {
                                        reachable
                                            .contains(&(item.source_module.clone(), symbol.clone()))
                                    })
                            })
                            .count(),
                    )
                    .unwrap_or(u32::MAX),
                    test_references: u32::try_from(
                        resolved.iter().filter(|item| item.from_test).count(),
                    )
                    .unwrap_or(u32::MAX),
                }
            }
            HygieneTraversal::TestAssertions => {
                if !work.steps(self.traversal_steps(traversal)) {
                    return None;
                }
                AnalysisEvidence::TestClassification {
                    files: u32::try_from(
                        self.modules
                            .values()
                            .filter(|module| module.is_test)
                            .count(),
                    )
                    .unwrap_or(u32::MAX),
                    tests: u32::try_from(self.tests().count()).unwrap_or(u32::MAX),
                    assertions: u32::try_from(
                        self.tests().filter(|(_, test)| test.assertions > 0).count(),
                    )
                    .unwrap_or(u32::MAX),
                    helpers: Vec::new(),
                }
            }
            HygieneTraversal::PrivateBoundaries => {
                let couplings = self.private_couplings(work)?;
                AnalysisEvidence::PrivateBoundaryResolution {
                    packages: self.production_packages().into_iter().collect(),
                    private_imports: u32::try_from(
                        couplings
                            .iter()
                            .filter(|item| item.kind == "private import")
                            .count(),
                    )
                    .unwrap_or(u32::MAX),
                    private_assertions: u32::try_from(
                        couplings
                            .iter()
                            .filter(|item| item.kind == "private assertion")
                            .count(),
                    )
                    .unwrap_or(u32::MAX),
                }
            }
        };
        work.check().then_some(evidence)
    }

    pub(super) fn findings(
        &self,
        gate: GateId,
        work: &mut GraphWork,
    ) -> Option<Vec<HygieneFinding>> {
        let estimate = match gate {
            GateId::Reachability => self.modules.len().saturating_mul(self.edges.len()),
            GateId::SymbolReachability | GateId::PythonUnused => 0,
            GateId::AssertionFreeTests => self.tests().count(),
            GateId::PrivateInternals => self
                .modules
                .values()
                .map(|module| module.private_couplings.len())
                .sum::<usize>(),
            _ => 0,
        };
        if !work.steps(estimate) {
            return None;
        }
        match gate {
            GateId::Reachability => self.reachability_findings(work),
            GateId::SymbolReachability => self.symbol_reachability_findings(work),
            GateId::PythonUnused => self.python_unused_findings(work),
            GateId::AssertionFreeTests => self.assertion_free_findings(work),
            GateId::PrivateInternals => self.private_findings(work),
            _ => Some(Vec::new()),
        }
    }

    pub(super) fn traversal_steps(&self, traversal: HygieneTraversal) -> usize {
        let declarations = self.declarations().count();
        let references = self.modules.values().fold(0_usize, |total, module| {
            total.saturating_add(module.references.len())
        });
        match traversal {
            HygieneTraversal::ImportGraph => 0,
            HygieneTraversal::SymbolGraph | HygieneTraversal::PythonDeclarations => declarations
                .saturating_mul(references.max(1))
                .saturating_mul(declarations.max(1)),
            HygieneTraversal::TestAssertions => self.tests().count(),
            HygieneTraversal::PrivateBoundaries => self.modules.len().saturating_mul(
                self.modules
                    .values()
                    .fold(0_usize, |total, module| {
                        total.saturating_add(module.private_couplings.len())
                    })
                    .max(1),
            ),
        }
    }

    fn reachability_findings(&self, work: &mut GraphWork) -> Option<Vec<HygieneFinding>> {
        let mut findings = Vec::new();
        for name in self
            .test_reachable
            .difference(&self.production_reachable)
            .filter(|name| !self.test_modules.contains(*name))
        {
            let Some(module) = self.modules.get(name) else {
                continue;
            };
            let importers = || {
                self.edges
                    .iter()
                    .filter(|(from, to)| to == name && self.test_reachable.contains(from))
                    .filter_map(|(from, _)| self.modules.get(from).map(|item| item.path.as_str()))
            };
            let imported_bytes = string_bytes(work, importers())?;
            let bytes = module
                .path
                .len()
                .saturating_add("python".len())
                .saturating_add(name.len())
                .saturating_add(imported_bytes);
            if !work.admit_finding(bytes) {
                return None;
            }
            let imported_by = clone_strings(work, importers())?;
            findings.push(HygieneFinding {
                path: module.path.clone(),
                line: None,
                detail: HygieneFindingDetail::ModuleReachability {
                    provider: "python".to_owned(),
                    subject: name.clone(),
                    imported_by,
                },
            });
        }
        Some(findings)
    }

    fn symbol_reachability_findings(&self, work: &mut GraphWork) -> Option<Vec<HygieneFinding>> {
        let references = self.resolved_references(work)?;
        let production = self.production_symbols(&references, work)?;
        let mut findings = Vec::new();
        for (module, declaration) in self.declarations() {
            let production_reachable =
                production.contains(&(module.name.clone(), declaration.name.clone()));
            let importers = || {
                references
                    .iter()
                    .filter(|reference| {
                        reference.from_test && reference.matches(&module.name, declaration)
                    })
                    .map(|reference| reference.path.as_str())
            };
            if production_reachable || importers().next().is_none() {
                continue;
            }
            let imported_bytes = string_bytes(work, importers())?;
            let bytes = saturating_sum([
                module.path.len(),
                "python".len(),
                module.name.len(),
                declaration.name.len(),
                declaration.kind.len(),
                imported_bytes,
            ]);
            if !work.admit_finding(bytes) {
                return None;
            }
            let mut imported_by = clone_strings(work, importers())?;
            imported_by.sort();
            imported_by.dedup();
            findings.push(HygieneFinding {
                path: module.path.clone(),
                line: Some(declaration.line),
                detail: HygieneFindingDetail::SymbolReachability {
                    provider: "python".to_owned(),
                    module: module.name.clone(),
                    symbol: declaration.name.clone(),
                    symbol_kind: declaration.kind.to_owned(),
                    imported_by,
                },
            });
        }
        Some(findings)
    }

    fn python_unused_findings(&self, work: &mut GraphWork) -> Option<Vec<HygieneFinding>> {
        let references = self.resolved_references(work)?;
        let production = self.production_symbols(&references, work)?;
        let mut findings = Vec::new();
        for (module, declaration) in self.declarations().filter(|(_, row)| row.top_level) {
            if production.contains(&(module.name.clone(), declaration.name.clone())) {
                continue;
            }
            let test_paths = || {
                references
                    .iter()
                    .filter(|reference| {
                        reference.from_test && reference.matches(&module.name, declaration)
                    })
                    .map(|reference| reference.path.as_str())
            };
            let test_only = test_paths().next().is_some();
            let reason = if test_only {
                "references only in tests"
            } else {
                "no production or test references"
            };
            let test_bytes = string_bytes(work, test_paths())?;
            let bytes = saturating_sum([
                module.path.len(),
                module.name.len(),
                declaration.name.len(),
                declaration.kind.len(),
                reason.len(),
                test_bytes,
            ]);
            if !work.admit_finding(bytes) {
                return None;
            }
            let mut test_references = clone_strings(work, test_paths())?;
            test_references.sort();
            test_references.dedup();
            let status = if test_only {
                PythonUnusedStatus::TestOnly
            } else {
                PythonUnusedStatus::CandidateUnused
            };
            findings.push(HygieneFinding {
                path: module.path.clone(),
                line: Some(declaration.line),
                detail: HygieneFindingDetail::PythonUnused {
                    module: module.name.clone(),
                    symbol: declaration.name.clone(),
                    declaration_kind: declaration.kind.to_owned(),
                    status,
                    reason: reason.to_owned(),
                    test_references,
                },
            });
        }
        Some(findings)
    }

    fn assertion_free_findings(&self, work: &mut GraphWork) -> Option<Vec<HygieneFinding>> {
        let mut findings = Vec::new();
        for (module, test) in self.tests().filter(|(_, test)| test.assertions == 0) {
            if !work.admit_finding(module.path.len().saturating_add(test.name.len())) {
                return None;
            }
            findings.push(HygieneFinding {
                path: module.path.clone(),
                line: Some(test.line),
                detail: HygieneFindingDetail::AssertionFree {
                    test_name: test.name.clone(),
                },
            });
        }
        Some(findings)
    }

    fn private_findings(&self, work: &mut GraphWork) -> Option<Vec<HygieneFinding>> {
        let couplings = self.private_couplings(work)?;
        let mut findings = Vec::new();
        for coupling in &couplings {
            if !work.admit_finding(saturating_sum([
                coupling.path.len(),
                coupling.test_name.len(),
                coupling.kind.len(),
                coupling.target.len(),
            ])) {
                return None;
            }
            findings.push(HygieneFinding {
                path: coupling.path.clone(),
                line: Some(coupling.line),
                detail: HygieneFindingDetail::PrivateCoupling {
                    test_name: coupling.test_name.clone(),
                    coupling_kind: coupling.kind.to_owned(),
                    target: coupling.target.clone(),
                },
            });
        }
        work.check().then_some(findings)
    }

    /// Rank the modules the suite reaches by how much of the suite reaches them.
    ///
    /// One traversal per test module rather than the single traversal from all
    /// of them the arms already hold: that union answers whether anything
    /// reaches a module, and this answers how much, which is the number that
    /// separates a module a lane can verify from one whose change leaves the
    /// rest of the suite unrun. Modules nothing reaches are absent rather than
    /// ranked at zero, because a module no test reaches never widens a run.
    fn suite_reach(&self, work: &mut GraphWork) -> Option<Vec<ModuleReach>> {
        let mut reached: BTreeMap<String, u32> = BTreeMap::new();
        let mut imported: BTreeMap<String, u32> = BTreeMap::new();
        for test in &self.test_modules {
            if !work.step() {
                return None;
            }
            let mut direct = BTreeSet::new();
            for (from, to) in &self.edges {
                if !work.step() {
                    return None;
                }
                if from == test {
                    direct.insert(to.clone());
                }
            }
            for module in &direct {
                if !work.step() {
                    return None;
                }
                *imported.entry(module.clone()).or_default() += 1;
            }
            // Seeded with the direct imports rather than the test itself, so a
            // test module never counts as reaching itself, and seeded as a set
            // so one test importing a module twice still counts once.
            for module in reachable_bounded(&direct, &self.edges, work)? {
                if !work.step() {
                    return None;
                }
                *reached.entry(module).or_default() += 1;
            }
        }
        let mut ranked: Vec<_> = reached
            .into_iter()
            .filter(|(name, _)| !self.test_modules.contains(name))
            .filter_map(|(name, reached_by)| {
                let module = self.modules.get(&name)?;
                Some(ModuleReach {
                    path: module.path.clone(),
                    reached_by,
                    imported_by: imported.get(&name).copied().unwrap_or_default(),
                })
            })
            .collect();
        ranked.sort_by(|left, right| {
            right
                .reached_by
                .cmp(&left.reached_by)
                .then_with(|| left.path.cmp(&right.path))
        });
        work.check().then_some(ranked)
    }

    fn root_paths(&self, roots: &BTreeSet<String>) -> Vec<String> {
        roots
            .iter()
            .filter_map(|name| self.modules.get(name).map(|module| module.path.clone()))
            .collect()
    }

    fn declarations(&self) -> impl Iterator<Item = (&Module, &Declaration)> {
        self.modules
            .values()
            .filter(|module| self.production_reachable.contains(&module.name))
            .flat_map(|module| module.declarations.iter().map(move |item| (module, item)))
    }

    fn tests(&self) -> impl Iterator<Item = (&Module, &TestCase)> {
        self.modules
            .values()
            .flat_map(|module| module.tests.iter().map(move |item| (module, item)))
    }

    fn production_packages(&self) -> BTreeSet<String> {
        self.production_reachable
            .iter()
            .filter_map(|module| {
                module
                    .split('.')
                    .next()
                    .filter(|package| !package.is_empty())
                    .map(str::to_owned)
            })
            .collect()
    }
}

fn ast_terminal(
    selected_files: u32,
    corpus: Corpus,
    index: usize,
    path: &str,
    work: &AstWork,
) -> (u32, Corpus, Terminal) {
    let terminal = match work.failure() {
        Some(AstFailure::Deadline) => Terminal::TimedOut {
            elapsed_ms: work.elapsed_ms(),
        },
        Some(AstFailure::Nodes | AstFailure::RetainedText) => Terminal::WorkExhausted {
            documents: u32::try_from(index).unwrap_or(u32::MAX),
            path: path.to_owned(),
            diagnostic: work.diagnostic(),
        },
        None => unreachable!("AST terminal requires one exhausted work bound"),
    };
    (selected_files, corpus, terminal)
}

fn graph_terminal(
    selected_files: u32,
    corpus: Corpus,
    work: &GraphWork,
) -> (u32, Corpus, Terminal) {
    let terminal = match work.failure() {
        Some(GraphFailure::Deadline) => Terminal::TimedOut {
            elapsed_ms: work.elapsed_ms(),
        },
        Some(_) => Terminal::WorkExhausted {
            documents: selected_files,
            path: "<graph>".to_owned(),
            diagnostic: work.diagnostic(),
        },
        None => unreachable!("graph terminal requires one exhausted work bound"),
    };
    (selected_files, corpus, terminal)
}

fn reachable_bounded(
    roots: &BTreeSet<String>,
    edges: &BTreeSet<(String, String)>,
    work: &mut GraphWork,
) -> Option<BTreeSet<String>> {
    if roots.len() > GRAPH_WORKLIST_LIMIT {
        work.failure = Some(GraphFailure::Worklist);
        return None;
    }
    let mut reached = BTreeSet::new();
    let mut pending: Vec<_> = roots.iter().cloned().collect();
    while let Some(next) = pending.pop() {
        if !work.step() {
            return None;
        }
        if !reached.insert(next.clone()) {
            continue;
        }
        for (from, to) in edges {
            if !work.step() {
                return None;
            }
            if from == &next {
                if !work.admit_worklist(pending.len()) {
                    return None;
                }
                pending.push(to.clone());
            }
        }
    }
    Some(reached)
}

fn string_bytes<'a>(
    work: &mut GraphWork,
    values: impl IntoIterator<Item = &'a str>,
) -> Option<usize> {
    let mut bytes = 0_usize;
    for value in values {
        if !work.step() {
            return None;
        }
        bytes = bytes.saturating_add(value.len());
    }
    Some(bytes)
}

fn clone_strings<'a>(
    work: &mut GraphWork,
    values: impl IntoIterator<Item = &'a str>,
) -> Option<Vec<String>> {
    let mut retained = Vec::new();
    for value in values {
        if !work.step() {
            return None;
        }
        retained.push(value.to_owned());
    }
    Some(retained)
}
