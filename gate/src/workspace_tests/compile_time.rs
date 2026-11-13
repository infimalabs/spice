//! Closed authority for executable Cargo compile-time producers.

use super::proof::{CargoCompileTimeAuthority, path_text};
use super::*;
use serde::Deserialize;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::visit::Visit;

mod call_authority;

use super::source_reach::macro_scope::{attribute_may_expand_items, exported_macro_rules};
use super::source_reach::{CompilerMacroKind, FileMacroResolution, FileMacroScope};
use call_authority::{builtin_derive, builtin_derives_unshadowed};

const AUTHORITY_VERSION: u64 = 1;

#[cfg(test)]
std::thread_local! {
    static SOURCE_PARSE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_source_parse_count() {
    SOURCE_PARSE_COUNT.set(0);
}

#[cfg(test)]
pub(super) fn source_parse_count() -> usize {
    SOURCE_PARSE_COUNT.get()
}

/// Rust source facts captured once for the lifetime of one Cargo shard plan.
#[derive(Default)]
pub(super) struct CompileTimeSourceAnalysis {
    packages: BTreeMap<PathBuf, PackageSourceFacts>,
    sources: BTreeMap<PathBuf, syn::File>,
}

struct PackageSourceFacts {
    generated_outputs: Vec<String>,
    scans: BTreeMap<TargetIdentity, CompileTimeScan>,
}

impl CompileTimeSourceAnalysis {
    fn package(
        &mut self,
        package: &PackageInventory,
    ) -> Result<&PackageSourceFacts, WorkspaceTestError> {
        if !self.packages.contains_key(&package.root) {
            let generated_outputs = package_generated_outputs(package, &mut self.sources)?;
            let mut scans = BTreeMap::new();
            if package.compile_time_authority.is_some() {
                for target in package
                    .declared_targets
                    .iter()
                    .filter(|target| compile_time_kind(target).is_some())
                {
                    scans.insert(
                        target.clone(),
                        scan_target(package, target, &mut self.sources)?,
                    );
                }
            }
            self.packages.insert(
                package.root.clone(),
                PackageSourceFacts {
                    generated_outputs,
                    scans,
                },
            );
        }
        Ok(self
            .packages
            .get(&package.root)
            .expect("package source facts were inserted"))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAuthority {
    version: u64,
    complete: bool,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    environment: Vec<String>,
    #[serde(default)]
    calls: Vec<String>,
}

pub(super) fn authority_declaration(
    package: &str,
    metadata: &serde_json::Value,
) -> Result<Option<CompileTimeAuthorityDeclaration>, WorkspaceTestError> {
    let Some(raw) = metadata
        .get("spice")
        .and_then(|spice| spice.get("cargo-proof"))
    else {
        return Ok(None);
    };
    let raw: RawAuthority = serde_json::from_value(raw.clone()).map_err(|error| {
        non_cacheable(format!(
            "Cargo package {package:?} has invalid package.metadata.spice.cargo-proof: {error}"
        ))
    })?;
    if raw.version != AUTHORITY_VERSION {
        return Err(non_cacheable(format!(
            "Cargo package {package:?} declares unsupported compile-time authority version {}; expected {AUTHORITY_VERSION}",
            raw.version,
        )));
    }
    if !raw.complete {
        return Err(non_cacheable(format!(
            "Cargo package {package:?} compile-time authority is not declared complete"
        )));
    }
    let mut files = raw.files;
    files.sort();
    files.dedup();
    let mut environment = raw.environment;
    for name in &environment {
        validate_environment_name(package, name)?;
    }
    environment.sort();
    environment.dedup();
    let mut calls = raw.calls;
    for call in &calls {
        if call.is_empty()
            || call.trim() != call
            || call
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte == 0)
        {
            return Err(non_cacheable(format!(
                "Cargo package {package:?} declares invalid compile-time call authority {call:?}",
            )));
        }
    }
    calls.sort();
    calls.dedup();
    Ok(Some(CompileTimeAuthorityDeclaration {
        files,
        environment,
        calls,
    }))
}

pub(super) fn compile_time_authorities(
    inventory: &Inventory,
    selected_root: &Path,
    reached_roots: &BTreeSet<PathBuf>,
    selected_paths: &[String],
    input_paths: &mut BTreeSet<String>,
    source_analysis: &mut CompileTimeSourceAnalysis,
) -> Result<Vec<CargoCompileTimeAuthority>, WorkspaceTestError> {
    let selected = selected_paths
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut authorities = Vec::new();
    for package in inventory
        .packages
        .iter()
        .filter(|package| reached_roots.contains(&package.root))
    {
        let executable_targets = package
            .declared_targets
            .iter()
            .filter(|target| compile_time_kind(target).is_some())
            .collect::<Vec<_>>();
        let source_facts = source_analysis.package(package)?;
        if executable_targets.is_empty() {
            if !source_facts.generated_outputs.is_empty() {
                return Err(non_cacheable(format!(
                    "Cargo package {:?} consumes OUT_DIR generated inputs without a custom-build producer",
                    package.name,
                )));
            }
            continue;
        }
        let declaration = package.compile_time_authority.as_ref().ok_or_else(|| {
            non_cacheable(format!(
                "Cargo package {:?} reaches executable compile-time code but has no complete package.metadata.spice.cargo-proof declaration",
                package.name,
            ))
        })?;

        let mut declared_files = declaration.files.iter().cloned().collect::<BTreeSet<_>>();
        let mut declared_environment = declaration
            .environment
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut scans = Vec::new();
        for target in &executable_targets {
            let scan = source_facts
                .scans
                .get(*target)
                .expect("executable target source facts were captured");
            declared_files.extend(scan.rerun_files.iter().cloned());
            declared_environment.extend(scan.rerun_environment.iter().cloned());
            scans.push((*target, scan));
        }

        if !source_facts.generated_outputs.is_empty()
            && !executable_targets
                .iter()
                .any(|target| compile_time_kind(target) == Some("custom-build"))
        {
            return Err(non_cacheable(format!(
                "Cargo package {:?} consumes OUT_DIR generated inputs without a custom-build producer",
                package.name,
            )));
        }

        let mut relative_files = BTreeSet::new();
        for declared in &declared_files {
            let relative = admit_declared_file(package, selected_root, declared, &selected)?;
            input_paths.insert(relative.clone());
            relative_files.insert(relative);
        }
        for (target, scan) in scans {
            for read in &scan.file_reads {
                if !declared_files.contains(read) {
                    return Err(non_cacheable(format!(
                        "Cargo package {:?} target {} reads undeclared compile-time file {read:?}",
                        package.name,
                        target.display(),
                    )));
                }
            }
            for name in &scan.environment_reads {
                if !declared_environment.contains(name) {
                    return Err(non_cacheable(format!(
                        "Cargo package {:?} target {} reads undeclared ambient environment {name:?}",
                        package.name,
                        target.display(),
                    )));
                }
            }
            authorities.push(CargoCompileTimeAuthority {
                package: package.name.clone(),
                target: target.display(),
                kind: compile_time_kind(target)
                    .expect("compile-time targets were filtered")
                    .to_owned(),
                input_paths: relative_files.iter().cloned().collect(),
                environment: declared_environment.iter().cloned().collect(),
                generated_outputs: if compile_time_kind(target) == Some("custom-build") {
                    source_facts.generated_outputs.clone()
                } else {
                    Vec::new()
                },
            });
        }
    }
    authorities.sort_by(|left, right| {
        (&left.package, &left.kind, &left.target).cmp(&(&right.package, &right.kind, &right.target))
    });
    Ok(authorities)
}

fn compile_time_kind(target: &TargetIdentity) -> Option<&'static str> {
    if target.kind.split('+').any(|kind| kind == "custom-build") {
        Some("custom-build")
    } else if target.kind.split('+').any(|kind| kind == "proc-macro") {
        Some("proc-macro")
    } else {
        None
    }
}

fn scan_target(
    package: &PackageInventory,
    target: &TargetIdentity,
    sources: &mut BTreeMap<PathBuf, syn::File>,
) -> Result<CompileTimeScan, WorkspaceTestError> {
    let mut scan = CompileTimeScan::default();
    let root = parse_rust_source(package, &target.source, sources, SourcePurpose::CompileTime)?;
    let exported_macros = exported_macro_rules(&root.items);
    let root_scope = package.namespace_context.root_scope(&root.attrs);
    let calls = &package
        .compile_time_authority
        .as_ref()
        .expect("executable targets require compile-time authority")
        .calls;
    let authority = ScanAuthority {
        exported_macros: &exported_macros,
        calls,
    };
    scan_compile_time_source(
        package,
        &target.source,
        root,
        root_scope,
        true,
        authority,
        &mut scan,
    )?;
    let mut visited = BTreeSet::new();
    for source in target.declared_path_sources.iter().filter(|source| {
        source.path.extension() == Some(OsStr::new("rs"))
            && source.origins.iter().any(|origin| {
                matches!(
                    origin,
                    DeclaredPathSourceOrigin::IncludedRust | DeclaredPathSourceOrigin::Module
                )
            })
    }) {
        let syntax = parse_rust_source(package, &source.path, sources, SourcePurpose::CompileTime)?;
        for inherited in &source.scopes {
            if visited.insert((source.path.clone(), inherited.clone())) {
                scan_compile_time_source(
                    package,
                    &source.path,
                    syntax,
                    inherited.scope.clone(),
                    inherited.crate_root,
                    authority,
                    &mut scan,
                )?;
            }
        }
    }
    Ok(scan)
}

#[derive(Clone, Copy)]
enum SourcePurpose {
    CompileTime,
    GeneratedOutput,
}

fn parse_rust_source<'a>(
    package: &PackageInventory,
    source: &Path,
    sources: &'a mut BTreeMap<PathBuf, syn::File>,
    purpose: SourcePurpose,
) -> Result<&'a syn::File, WorkspaceTestError> {
    if !sources.contains_key(source) {
        let document = fs::read_to_string(source).map_err(|error| {
            non_cacheable(match purpose {
                SourcePurpose::CompileTime => format!(
                    "read Cargo package {:?} compile-time source {}: {error}",
                    package.name,
                    source.display(),
                ),
                SourcePurpose::GeneratedOutput => format!(
                    "read Cargo package {:?} source {} for OUT_DIR authority: {error}",
                    package.name,
                    source.display(),
                ),
            })
        })?;
        let syntax = syn::parse_file(&document).map_err(|error| {
            non_cacheable(match purpose {
                SourcePurpose::CompileTime => format!(
                    "parse Cargo package {:?} compile-time source {}: {error}",
                    package.name,
                    source.display(),
                ),
                SourcePurpose::GeneratedOutput => format!(
                    "parse Cargo package {:?} source {} for OUT_DIR authority: {error}",
                    package.name,
                    source.display(),
                ),
            })
        })?;
        #[cfg(test)]
        SOURCE_PARSE_COUNT.set(SOURCE_PARSE_COUNT.get() + 1);
        sources.insert(source.to_path_buf(), syntax);
    }
    Ok(sources
        .get(source)
        .expect("successfully parsed Rust source was retained"))
}

#[derive(Clone, Copy)]
struct ScanAuthority<'a> {
    exported_macros: &'a BTreeSet<String>,
    calls: &'a [String],
}

fn scan_compile_time_source(
    package: &PackageInventory,
    source: &Path,
    syntax: &syn::File,
    scope: FileMacroScope,
    crate_root: bool,
    authority: ScanAuthority<'_>,
    scan: &mut CompileTimeScan,
) -> Result<(), WorkspaceTestError> {
    let builtin_derives_unshadowed = builtin_derives_unshadowed(syntax);
    let mut visitor = CompileTimeVisitor {
        package: &package.name,
        source,
        scan,
        scope,
        crate_root,
        exported_macros: authority.exported_macros,
        calls: authority.calls,
        builtin_derives_unshadowed,
        error: None,
    };
    visitor.scan_file(syntax);
    match visitor.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[derive(Default)]
struct CompileTimeScan {
    rerun_files: BTreeSet<String>,
    rerun_environment: BTreeSet<String>,
    file_reads: BTreeSet<String>,
    environment_reads: BTreeSet<String>,
}

struct CompileTimeVisitor<'a> {
    package: &'a str,
    source: &'a Path,
    scan: &'a mut CompileTimeScan,
    scope: FileMacroScope,
    crate_root: bool,
    exported_macros: &'a BTreeSet<String>,
    calls: &'a [String],
    builtin_derives_unshadowed: bool,
    error: Option<WorkspaceTestError>,
}

impl<'ast> Visit<'ast> for CompileTimeVisitor<'_> {
    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        if self.error.is_some() {
            return;
        }
        if invocation.path.is_ident("macro_rules") {
            return;
        }
        let spelling = invocation
            .path
            .segments
            .last()
            .map(|segment| super::source_reach::ident_name(&segment.ident));
        let Some(spelling) = spelling else {
            self.refuse("invokes a macro without a path identity".to_owned());
            return;
        };
        if CompilerMacroKind::from_name(&spelling).is_none() {
            self.refuse(format!("invokes unrecognized macro {spelling}!"));
            return;
        }
        let Some((_, resolution)) = self.scope.resolve_compiler_macro(&invocation.path) else {
            self.refuse(format!(
                "cannot prove compiler macro origin for {}!",
                display_macro_path(&invocation.path),
            ));
            return;
        };
        match resolution {
            FileMacroResolution::Compiler(kind) => self.scan_compiler_macro(kind, invocation),
            FileMacroResolution::Shadowed => self.refuse(format!(
                "resolves {}! to a user-owned macro",
                display_macro_path(&invocation.path),
            )),
            FileMacroResolution::Unresolved => self.refuse(format!(
                "cannot prove compiler macro origin for {}!",
                display_macro_path(&invocation.path),
            )),
        }
    }

    fn visit_attribute(&mut self, attribute: &'ast syn::Attribute) {
        let prelude_control =
            attribute.path().is_ident("no_std") || attribute.path().is_ident("no_implicit_prelude");
        let builtin_derive = self.builtin_derives_unshadowed && builtin_derive(attribute);
        if self.error.is_none()
            && !prelude_control
            && !builtin_derive
            && attribute_may_expand_items(attribute)
        {
            self.refuse(format!(
                "uses unresolved compile-time attribute {}",
                display_macro_path(attribute.path()),
            ));
        } else {
            syn::visit::visit_attribute(self, attribute);
        }
    }

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        for attribute in &item.attrs {
            self.visit_attribute(attribute);
        }
        let Some((_, items)) = &item.content else {
            return;
        };
        let outer = self.scope.clone();
        let crate_root = self.crate_root;
        self.scope = outer.clone().child_module();
        self.crate_root = false;
        self.scan_items(items);
        self.scope = outer;
        self.crate_root = crate_root;
    }

    fn visit_block(&mut self, block: &'ast syn::Block) {
        let outer = self.scope.clone();
        let crate_root = self.crate_root;
        self.scope = self
            .scope
            .clone()
            .child_block()
            .with_block_items(block, self.exported_macros);
        self.crate_root = false;
        let mut textual = self.scope.clone();
        for statement in &block.stmts {
            if let syn::Stmt::Item(item) = statement {
                textual = textual.with_textual_item(item);
            }
            self.scope = textual.clone();
            syn::visit::visit_stmt(self, statement);
        }
        self.scope = outer;
        self.crate_root = crate_root;
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if self.error.is_some() {
            return;
        }
        if let Expr::Path(function) = call.func.as_ref() {
            let path = rust_path(&function.path);
            if matches!(
                path.as_str(),
                "std::env::vars" | "std::env::vars_os" | "env::vars" | "env::vars_os"
            ) {
                self.refuse(format!(
                    "enumerates unbounded ambient environment through {path}"
                ));
                return;
            }
            if matches!(
                path.as_str(),
                "std::env::var" | "std::env::var_os" | "env::var" | "env::var_os"
            ) {
                match literal_call_argument(call) {
                    Some(name) => {
                        if let Err(error) = validate_environment_name(self.package, &name) {
                            self.error = Some(error);
                            return;
                        }
                        if !cargo_derived_environment(&name) {
                            self.scan.environment_reads.insert(name);
                        }
                    }
                    None => self.refuse(format!("uses dynamic ambient environment through {path}")),
                }
                return;
            }
            if process_or_network_call(&path) {
                self.refuse(format!(
                    "uses unbounded process or network dependency {path}"
                ));
                return;
            }
            if filesystem_read_call(&path) {
                match literal_call_argument(call) {
                    Some(path) => {
                        self.scan.file_reads.insert(path);
                    }
                    None => self.refuse(format!("uses dynamic filesystem dependency {path}")),
                }
                return;
            }
            if filesystem_write_call(&path) {
                self.refuse(format!(
                    "emits unbounded compile-time filesystem output through {path}"
                ));
                return;
            }
            if self.calls.binary_search(&path).is_ok() {
                syn::visit::visit_expr_call(self, call);
                return;
            }
            self.refuse(format!("invokes unrecognized executable call {path}"));
            return;
        }
        self.refuse("invokes an indirect callable expression".to_owned());
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if self.error.is_some() {
            return;
        }
        let authority = format!(".{}", call.method);
        if self.calls.binary_search(&authority).is_ok() {
            syn::visit::visit_expr_method_call(self, call);
        } else {
            self.refuse(format!("invokes unbounded method dispatch {authority}()"));
        }
    }
}

impl CompileTimeVisitor<'_> {
    fn scan_file(&mut self, syntax: &syn::File) {
        for attribute in &syntax.attrs {
            self.visit_attribute(attribute);
        }
        self.scan_items(&syntax.items);
    }

    fn scan_items(&mut self, items: &[Item]) {
        let mut textual = self
            .scope
            .clone()
            .with_items(items, self.crate_root, self.exported_macros)
            .with_generated_items(items, self.crate_root);
        for item in items {
            textual = textual.with_textual_item(item);
            self.scope = textual.clone();
            self.visit_item(item);
            if self.error.is_some() {
                return;
            }
        }
    }

    fn scan_compiler_macro(&mut self, kind: CompilerMacroKind, invocation: &syn::Macro) {
        match kind {
            CompilerMacroKind::Println => self.scan_println(invocation),
            CompilerMacroKind::Format => self.scan_expression_macro(invocation),
            CompilerMacroKind::Env | CompilerMacroKind::OptionEnv => {
                self.scan_environment_macro(invocation);
            }
            CompilerMacroKind::Concat => self.scan_concat(invocation),
            CompilerMacroKind::Include
            | CompilerMacroKind::IncludeStr
            | CompilerMacroKind::IncludeBytes => self.scan_file_macro_argument(invocation),
            CompilerMacroKind::File
            | CompilerMacroKind::Line
            | CompilerMacroKind::Column
            | CompilerMacroKind::ModulePath => self.require_empty_macro(kind, invocation),
            CompilerMacroKind::Cfg | CompilerMacroKind::Stringify => {}
        }
    }

    fn scan_concat(&mut self, invocation: &syn::Macro) {
        let parser = Punctuated::<Expr, syn::Token![,]>::parse_terminated;
        let Ok(arguments) = parser.parse2(invocation.tokens.clone()) else {
            self.refuse("uses malformed concat! input".to_owned());
            return;
        };
        for argument in arguments {
            match argument {
                Expr::Lit(_) => {}
                Expr::Macro(expression) => self.visit_macro(&expression.mac),
                _ => {
                    self.refuse("uses computed concat! input".to_owned());
                    return;
                }
            }
        }
    }

    fn scan_expression_macro(&mut self, invocation: &syn::Macro) {
        let parser = Punctuated::<Expr, syn::Token![,]>::parse_terminated;
        let Ok(arguments) = parser.parse2(invocation.tokens.clone()) else {
            self.refuse("uses malformed format! input".to_owned());
            return;
        };
        for argument in arguments {
            self.visit_expr(&argument);
            if self.error.is_some() {
                return;
            }
        }
    }

    fn scan_file_macro_argument(&mut self, invocation: &syn::Macro) {
        let Ok(argument) = syn::parse2::<Expr>(invocation.tokens.clone()) else {
            self.refuse("uses malformed compiler file-macro input".to_owned());
            return;
        };
        match argument {
            Expr::Lit(_) => {}
            Expr::Macro(expression) => self.visit_macro(&expression.mac),
            _ => self.refuse("uses computed compiler file-macro input".to_owned()),
        }
    }

    fn require_empty_macro(&mut self, kind: CompilerMacroKind, invocation: &syn::Macro) {
        if !invocation.tokens.is_empty() {
            self.refuse(format!("uses malformed {}! input", kind.name()));
        }
    }

    fn scan_println(&mut self, invocation: &syn::Macro) {
        let parser = Punctuated::<Expr, syn::Token![,]>::parse_terminated;
        let Ok(arguments) = parser.parse2(invocation.tokens.clone()) else {
            self.refuse("emits computed compile-time output".to_owned());
            return;
        };
        let [Expr::Lit(first)] = arguments.iter().collect::<Vec<_>>().as_slice() else {
            self.refuse("emits computed or interpolated compile-time output".to_owned());
            return;
        };
        let Lit::Str(value) = &first.lit else {
            self.refuse("emits computed compile-time output".to_owned());
            return;
        };
        let value = value.value();
        if value.contains(['{', '}', '\n', '\r']) {
            self.refuse("emits dynamic or multiline compile-time output".to_owned());
            return;
        }
        let Some(directive) = value
            .strip_prefix("cargo::")
            .or_else(|| value.strip_prefix("cargo:"))
        else {
            return;
        };
        let Some((name, value)) = directive.split_once('=') else {
            self.refuse("emits a malformed Cargo directive".to_owned());
            return;
        };
        if value.is_empty() {
            self.refuse("emits an empty Cargo directive".to_owned());
            return;
        }
        match name {
            "rerun-if-changed" => {
                self.scan.rerun_files.insert(value.to_owned());
            }
            "rerun-if-env-changed" => {
                if let Err(error) = validate_environment_name(self.package, value) {
                    self.error = Some(error);
                } else {
                    self.scan.rerun_environment.insert(value.to_owned());
                }
            }
            _ => self.refuse(format!("emits unsupported Cargo directive {name:?}")),
        }
    }

    fn scan_environment_macro(&mut self, invocation: &syn::Macro) {
        let Ok(name) = syn::parse2::<syn::LitStr>(invocation.tokens.clone()) else {
            self.refuse("uses a dynamic compile-time environment macro".to_owned());
            return;
        };
        let name = name.value();
        if let Err(error) = validate_environment_name(self.package, &name) {
            self.error = Some(error);
        } else if !cargo_derived_environment(&name) {
            self.scan.environment_reads.insert(name);
        }
    }

    fn refuse(&mut self, detail: String) {
        self.error = Some(non_cacheable(format!(
            "Cargo package {:?} compile-time source {} {detail}",
            self.package,
            self.source.display(),
        )));
    }
}

fn literal_call_argument(call: &syn::ExprCall) -> Option<String> {
    if call.args.len() != 1 {
        return None;
    }
    let Expr::Lit(argument) = call.args.first()? else {
        return None;
    };
    let Lit::Str(value) = &argument.lit else {
        return None;
    };
    Some(value.value())
}

fn rust_path(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn display_macro_path(path: &syn::Path) -> String {
    let root = if path.leading_colon.is_some() {
        "::"
    } else {
        ""
    };
    format!("{root}{}", rust_path(path))
}

fn filesystem_read_call(path: &str) -> bool {
    matches!(
        path,
        "std::fs::read"
            | "std::fs::read_to_string"
            | "std::fs::read_link"
            | "std::fs::canonicalize"
            | "std::fs::File::open"
            | "fs::read"
            | "fs::read_to_string"
            | "fs::read_link"
            | "fs::canonicalize"
            | "fs::File::open"
            | "File::open"
    )
}

fn filesystem_write_call(path: &str) -> bool {
    matches!(
        path,
        "std::fs::write"
            | "std::fs::File::create"
            | "fs::write"
            | "fs::File::create"
            | "File::create"
    )
}

fn process_or_network_call(path: &str) -> bool {
    matches!(
        path,
        "std::process::Command::new"
            | "process::Command::new"
            | "Command::new"
            | "std::net::TcpStream::connect"
            | "net::TcpStream::connect"
            | "TcpStream::connect"
            | "std::net::UdpSocket::bind"
            | "net::UdpSocket::bind"
            | "UdpSocket::bind"
            | "std::os::unix::net::UnixStream::connect"
            | "UnixStream::connect"
    )
}

fn cargo_derived_environment(name: &str) -> bool {
    name.starts_with("CARGO_")
        || matches!(
            name,
            "OUT_DIR"
                | "TARGET"
                | "HOST"
                | "PROFILE"
                | "OPT_LEVEL"
                | "DEBUG"
                | "NUM_JOBS"
                | "RUSTC"
                | "RUSTDOC"
        )
}

fn package_generated_outputs(
    package: &PackageInventory,
    parsed_sources: &mut BTreeMap<PathBuf, syn::File>,
) -> Result<Vec<String>, WorkspaceTestError> {
    let mut outputs = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for target in &package.declared_targets {
        if compile_time_kind(target).is_some() {
            continue;
        }
        let mut sources = BTreeSet::from([target.source.clone()]);
        sources.extend(
            target
                .declared_path_sources
                .iter()
                .filter(|source| source.path.extension() == Some(OsStr::new("rs")))
                .map(|source| source.path.clone()),
        );
        for source in sources {
            if !visited.insert(source.clone()) {
                continue;
            }
            let syntax = parse_rust_source(
                package,
                &source,
                parsed_sources,
                SourcePurpose::GeneratedOutput,
            )?;
            let mut visitor = GeneratedOutputVisitor {
                source: &source,
                outputs: &mut outputs,
                error: None,
            };
            visitor.visit_file(syntax);
            if let Some(error) = visitor.error {
                return Err(error);
            }
        }
    }
    Ok(outputs.into_iter().collect())
}

struct GeneratedOutputVisitor<'a> {
    source: &'a Path,
    outputs: &'a mut BTreeSet<String>,
    error: Option<WorkspaceTestError>,
}

impl<'ast> Visit<'ast> for GeneratedOutputVisitor<'_> {
    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        if self.error.is_some() {
            return;
        }
        let is_file_macro =
            super::source_reach::FileMacroKind::from_path(&invocation.path).is_some();
        if is_file_macro {
            match out_dir_include(invocation.tokens.clone()) {
                Ok(Some(output)) => {
                    self.outputs.insert(output);
                }
                Ok(None) => {}
                Err(detail) => {
                    self.error = Some(non_cacheable(format!(
                        "Cargo source {} has unbounded OUT_DIR input: {detail}",
                        self.source.display(),
                    )));
                }
            }
        }
        syn::visit::visit_macro(self, invocation);
    }
}

fn out_dir_include(tokens: proc_macro2::TokenStream) -> Result<Option<String>, String> {
    let Ok(expression) = syn::parse2::<Expr>(tokens) else {
        return Ok(None);
    };
    let Expr::Macro(concat) = expression else {
        return Ok(None);
    };
    if !concat.mac.path.is_ident("concat") {
        return Ok(None);
    }
    let parser = Punctuated::<Expr, syn::Token![,]>::parse_terminated;
    let arguments = parser
        .parse2(concat.mac.tokens)
        .map_err(|error| error.to_string())?;
    let [Expr::Macro(environment), Expr::Lit(suffix)] =
        arguments.iter().collect::<Vec<_>>().as_slice()
    else {
        return Err("expected concat!(env!(\"OUT_DIR\"), \"/literal\")".to_owned());
    };
    if !environment.mac.path.is_ident("env") {
        return Err("expected env!(\"OUT_DIR\") as the first concat argument".to_owned());
    }
    let variable = syn::parse2::<syn::LitStr>(environment.mac.tokens.clone())
        .map_err(|error| error.to_string())?;
    let Lit::Str(suffix) = &suffix.lit else {
        return Err("expected a literal OUT_DIR suffix".to_owned());
    };
    if variable.value() != "OUT_DIR" {
        return Ok(None);
    }
    let suffix = suffix.value();
    let relative = suffix.strip_prefix('/').ok_or_else(|| {
        "OUT_DIR suffix must begin with one slash before a relative path".to_owned()
    })?;
    let path = Path::new(relative);
    if relative.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "OUT_DIR suffix {suffix:?} is not a normalized relative file"
        ));
    }
    Ok(Some(relative.replace(std::path::MAIN_SEPARATOR, "/")))
}

fn admit_declared_file(
    package: &PackageInventory,
    selected_root: &Path,
    declared: &str,
    selected_paths: &BTreeSet<&str>,
) -> Result<String, WorkspaceTestError> {
    let relative = Path::new(declared);
    if relative.is_absolute() {
        return Err(non_cacheable(format!(
            "Cargo package {:?} declares external compile-time path {declared:?}",
            package.name,
        )));
    }
    let candidate = package.root.join(relative);
    let metadata = fs::symlink_metadata(&candidate).map_err(|error| {
        non_cacheable(format!(
            "Cargo package {:?} declared compile-time path {} is unavailable: {error}",
            package.name,
            candidate.display(),
        ))
    })?;
    if !metadata.file_type().is_file() {
        return Err(non_cacheable(format!(
            "Cargo package {:?} declared compile-time path {} is not a non-symlink regular file",
            package.name,
            candidate.display(),
        )));
    }
    let canonical = fs::canonicalize(&candidate).map_err(|error| {
        non_cacheable(format!(
            "canonicalize Cargo package {:?} compile-time path {}: {error}",
            package.name,
            candidate.display(),
        ))
    })?;
    let relative = canonical.strip_prefix(selected_root).map_err(|_| {
        non_cacheable(format!(
            "Cargo package {:?} compile-time path {} escapes selected root {}",
            package.name,
            canonical.display(),
            selected_root.display(),
        ))
    })?;
    let relative = path_text(relative)?;
    if !selected_paths.contains(relative.as_str()) {
        return Err(non_cacheable(format!(
            "Cargo package {:?} compile-time path {relative:?} is absent from selected-tree authority",
            package.name,
        )));
    }
    Ok(relative)
}

fn validate_environment_name(package: &str, name: &str) -> Result<(), WorkspaceTestError> {
    if name.is_empty() || name.as_bytes().contains(&b'=') || name.as_bytes().contains(&0) {
        return Err(non_cacheable(format!(
            "Cargo package {package:?} declares invalid compile-time environment name {name:?}"
        )));
    }
    Ok(())
}

fn non_cacheable(detail: impl Into<String>) -> WorkspaceTestError {
    WorkspaceTestError::new(format!(
        "Cargo shard is non-cacheable because executable compile-time authority is incomplete: {}",
        detail.into(),
    ))
}
