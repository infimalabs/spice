//! Configuration declaration and consumer inventories from one selected tree.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use syn::parse::{Parse, ParseStream};
use syn::visit::{self, Visit};
use syn::{
    Expr, FnArg, GenericParam, Ident, Item, ItemFn, ItemMacro, Lit, LitStr, Pat, PathArguments,
    Signature, Stmt, Token, Type, UseTree, braced,
};

use super::acquire::CoreSource;
use super::{ConfigInventoryEvidence, CoreEvidence, CoreFinding, CoreInputError, CoreObservation};

const CONFIGURATION_ROOT: &str = "spice/src/config.rs";
const CONFIGURATION_REGISTRY_MODULE: &str = "registry";
const CONFIGURATION_REGISTRY: &str = "spice/src/config/registry.rs";

/// Parsed production Rust sources shared by configuration inventory providers.
pub(super) struct ConfigurationInventorySource {
    files: Vec<ParsedSource>,
    spice_surface: bool,
}

struct ParsedSource {
    path: String,
    syntax: syn::File,
}

impl ConfigurationInventorySource {
    /// Parse every admitted production Rust source from the selected tree once.
    pub(super) fn capture(source: &CoreSource) -> Result<Self, CoreInputError> {
        let entries = source.jurisdiction().entries();
        let spice_surface = entries.iter().any(|entry| production_rust(entry.path()));
        let mut files = Vec::new();
        for entry in entries
            .iter()
            .filter(|entry| entry.measurable() && production_rust(entry.path()))
        {
            let path = entry.path().to_owned();
            let text = fs::read_to_string(source.root().join(&path)).map_err(|error| {
                CoreInputError::new(format!(
                    "configuration inventory cannot read {path}: {error}"
                ))
            })?;
            let syntax = syn::parse_file(&text).map_err(|error| {
                CoreInputError::new(format!(
                    "configuration inventory cannot parse {path}: {error}"
                ))
            })?;
            files.push(ParsedSource { path, syntax });
        }
        Ok(Self {
            files,
            spice_surface,
        })
    }

    /// Gather false-default registry declarations and their typed consumers.
    pub(super) fn false_disable(&self) -> Result<CoreObservation, CoreInputError> {
        let declared: BTreeSet<_> = self
            .registry_rows()?
            .into_iter()
            .filter(|row| row.false_builtin)
            .map(|row| row.path)
            .collect();

        let mut observed = BTreeSet::new();
        for source in &self.files {
            let mut consumer = ConsumerVisitor::new(&declared);
            consumer.visit_file(&source.syntax);
            observed.append(&mut consumer.observed);
        }
        let evidence = ConfigInventoryEvidence::new(
            declared.into_iter().collect(),
            observed.into_iter().collect(),
            Vec::new(),
        )?;
        Ok(CoreObservation::Measured {
            evidence: CoreEvidence::ConfigFalseDisable(evidence),
        })
    }

    /// Gather policy-class registry paths and classify their source authority.
    pub(super) fn tracked_trust(&self) -> Result<CoreObservation, CoreInputError> {
        let mut declared = BTreeSet::new();
        let mut observed = BTreeSet::new();
        let mut violations = Vec::new();
        for row in self.registry_rows()?.into_iter().filter(|row| row.policy) {
            declared.insert(row.path.clone());
            if row.tracked_source() {
                observed.insert(row.path);
            } else {
                let path = row.path.join(".");
                violations.push(CoreFinding::new(
                    "config-tracked-trust::untracked-source",
                    None::<String>,
                    format!(
                        "trust-bearing configuration path {path} admits untracked layer {}",
                        row.layers
                    ),
                )?);
            }
        }
        let evidence = ConfigInventoryEvidence::new(
            declared.into_iter().collect(),
            observed.into_iter().collect(),
            violations,
        )?;
        Ok(CoreObservation::Measured {
            evidence: CoreEvidence::ConfigTrackedTrust(evidence),
        })
    }

    fn registry_rows(&self) -> Result<Vec<RegistryRow>, CoreInputError> {
        if !self.spice_surface {
            return Ok(Vec::new());
        }
        let root = self.required_source(CONFIGURATION_ROOT, "root")?;
        let mut declarations = inspect_registry(root)?;
        if declares_external_module(root, CONFIGURATION_REGISTRY_MODULE) {
            let registry = self.required_source(CONFIGURATION_REGISTRY, "split module")?;
            declarations.extend(inspect_registry(registry)?);
        }
        if declarations.is_empty() {
            return Err(CoreInputError::new(format!(
                "configuration inventory found no authoritative configuration_registry! \
                 declaration in {CONFIGURATION_ROOT} or its declared registry module"
            )));
        }
        if declarations.len() > 1 {
            return Err(CoreInputError::new(format!(
                "configuration inventory found multiple authoritative configuration_registry! \
                 declarations in {CONFIGURATION_ROOT} and its declared registry module"
            )));
        }
        let rows = declarations
            .pop()
            .expect("one authoritative registry declaration remains");
        if rows.is_empty() {
            return Err(CoreInputError::new(format!(
                "configuration inventory authoritative configuration_registry! declaration in \
                 {CONFIGURATION_ROOT} contains no rows"
            )));
        }
        Ok(rows)
    }

    fn required_source(&self, path: &str, role: &str) -> Result<&ParsedSource, CoreInputError> {
        self.files
            .iter()
            .find(|source| source.path == path)
            .ok_or_else(|| {
                CoreInputError::new(format!(
                    "configuration inventory authoritative {role} {path} is absent"
                ))
            })
    }
}

fn inspect_registry(source: &ParsedSource) -> Result<Vec<Vec<RegistryRow>>, CoreInputError> {
    let mut visitor = DeclarationVisitor::default();
    visitor.visit_file(&source.syntax);
    visitor.error.map_or(Ok(visitor.declarations), |error| {
        Err(CoreInputError::new(format!(
            "configuration inventory cannot inspect {} registry: {error}",
            source.path
        )))
    })
}

fn declares_external_module(source: &ParsedSource, name: &str) -> bool {
    source.syntax.items.iter().any(|item| {
        matches!(item, Item::Mod(module) if module.ident == name && module.content.is_none())
    })
}

fn production_rust(path: &str) -> bool {
    path.starts_with("spice/src/")
        && path.ends_with(".rs")
        && !path.ends_with("/tests.rs")
        && !path.contains("/tests/")
}

#[derive(Default)]
struct DeclarationVisitor {
    declarations: Vec<Vec<RegistryRow>>,
    error: Option<syn::Error>,
}

impl<'ast> Visit<'ast> for DeclarationVisitor {
    fn visit_item_macro(&mut self, node: &'ast ItemMacro) {
        if self.error.is_none()
            && node
                .mac
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "configuration_registry")
        {
            match syn::parse2::<RegistryRows>(node.mac.tokens.clone()) {
                Ok(rows) => self.declarations.push(rows.0),
                Err(error) => self.error = Some(error),
            }
        }
        visit::visit_item_macro(self, node);
    }
}

struct RegistryRows(Vec<RegistryRow>);

struct RegistryRow {
    path: Vec<String>,
    false_builtin: bool,
    layers: String,
    policy: bool,
}

impl RegistryRow {
    fn tracked_source(&self) -> bool {
        matches!(self.layers.as_str(), "TRACKED" | "TRACKED_FLAG")
    }
}

impl Parse for RegistryRows {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut rows = Vec::new();
        while !input.is_empty() {
            let _: Ident = input.parse()?;
            let path: LitStr = input.parse()?;
            let body;
            braced!(body in input);
            let mut false_builtin = false;
            let mut layers = None;
            let mut policy = None;
            while !body.is_empty() {
                let field: Ident = body.parse()?;
                body.parse::<Token![:]>()?;
                let value: Expr = body.parse()?;
                body.parse::<Token![,]>()?;
                if field == "builtin" {
                    false_builtin = is_false_builtin(&value);
                } else if field == "layers" {
                    layers = Some(required_identifier(&value, "registry layers")?);
                } else if field == "class" {
                    policy = Some(required_policy_class(&value)?);
                }
            }
            rows.push(RegistryRow {
                path: path.value().split('.').map(str::to_owned).collect(),
                false_builtin,
                layers: layers.ok_or_else(|| {
                    syn::Error::new(path.span(), "registry row requires a layers field")
                })?,
                policy: policy.ok_or_else(|| {
                    syn::Error::new(path.span(), "registry row requires a class field")
                })?,
            });
        }
        Ok(Self(rows))
    }
}

fn required_identifier(expression: &Expr, label: &str) -> syn::Result<String> {
    let Expr::Path(path) = ungroup(expression) else {
        return Err(syn::Error::new_spanned(
            expression,
            format!("{label} must be one identifier"),
        ));
    };
    if path.path.segments.len() != 1 {
        return Err(syn::Error::new_spanned(
            expression,
            format!("{label} must be one identifier"),
        ));
    }
    Ok(path.path.segments[0].ident.to_string())
}

fn required_policy_class(expression: &Expr) -> syn::Result<bool> {
    let class = required_identifier(expression, "registry policy class")?;
    match class.as_str() {
        "Policy" => Ok(true),
        "WorktreeLocal" | "MachineLocal" => Ok(false),
        _ => Err(syn::Error::new_spanned(
            expression,
            format!("unknown registry policy class {class}"),
        )),
    }
}

fn is_false_builtin(expression: &Expr) -> bool {
    let Some(boolean) = sole_call_argument(expression, "Some") else {
        return false;
    };
    let Some(value) = sole_call_argument(boolean, "Boolean") else {
        return false;
    };
    matches!(
        ungroup(value),
        Expr::Lit(literal) if matches!(&literal.lit, Lit::Bool(value) if !value.value)
    )
}

fn sole_call_argument<'a>(expression: &'a Expr, name: &str) -> Option<&'a Expr> {
    let Expr::Call(call) = ungroup(expression) else {
        return None;
    };
    let Expr::Path(function) = ungroup(&call.func) else {
        return None;
    };
    if call.args.len() != 1
        || !function
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == name)
    {
        return None;
    }
    call.args.first()
}

fn ungroup(expression: &Expr) -> &Expr {
    match expression {
        Expr::Group(group) => ungroup(&group.expr),
        Expr::Paren(parenthesized) => ungroup(&parenthesized.expr),
        _ => expression,
    }
}

/// Method chains rooted in an authenticated configuration binding.
///
/// A binding is seeded only by a parameter of the authoritative root type or by
/// an initializer that already carries a path. Every pattern that rebinds a name
/// evicts it for the construct that scopes the pattern; a let-expression pattern
/// evicts to the end of its block, which can only lose a chain, never invent one.
///
/// A type parameter withdraws the root name it spells for as long as it is in
/// scope, whether it is declared by the signature reached or by the impl block
/// around it, because a generic receiver is any type at all and never the one
/// configuration root.
struct ConsumerVisitor<'a> {
    declared: &'a BTreeSet<Vec<String>>,
    roots: BTreeSet<String>,
    observed: BTreeSet<Vec<String>>,
    bindings: BTreeMap<String, Vec<String>>,
}

impl<'a> ConsumerVisitor<'a> {
    fn new(declared: &'a BTreeSet<Vec<String>>) -> Self {
        Self {
            declared,
            roots: BTreeSet::new(),
            observed: BTreeSet::new(),
            bindings: BTreeMap::new(),
        }
    }

    /// Open one body whose only bindings are its own authenticated parameters.
    ///
    /// Returns the caller's frame for the visitor to restore once the body ends;
    /// every function form seeds a binding by this one rule.
    fn enter_body(&mut self, signature: &Signature) -> BTreeMap<String, Vec<String>> {
        let outer = std::mem::take(&mut self.bindings);
        let mut roots = self.roots.clone();
        for parameter in &signature.generics.params {
            if let GenericParam::Type(parameter) = parameter {
                roots.remove(&parameter.ident.to_string());
            }
        }
        for argument in &signature.inputs {
            let FnArg::Typed(argument) = argument else {
                continue;
            };
            if root_type(&argument.ty, &roots)
                && let Pat::Ident(binding) = &*argument.pat
            {
                self.bindings.insert(binding.ident.to_string(), Vec::new());
            }
        }
        outer
    }

    /// Drop every name one pattern rebinds, ending the path it used to carry.
    ///
    /// A rebound name holds an unauthenticated value, so it carries no path at
    /// all until some authenticated parameter or initializer gives it one.
    fn shadow(&mut self, pattern: &Pat) {
        let mut names = PatternNames::default();
        names.visit_pat(pattern);
        for name in names.names {
            self.bindings.remove(&name);
        }
    }
}

/// Every name one pattern binds, nested and shorthand forms included.
#[derive(Default)]
struct PatternNames {
    names: Vec<String>,
}

impl<'ast> Visit<'ast> for PatternNames {
    fn visit_pat_ident(&mut self, node: &'ast syn::PatIdent) {
        self.names.push(node.ident.to_string());
        visit::visit_pat_ident(self, node);
    }
}

impl<'ast> Visit<'ast> for ConsumerVisitor<'_> {
    fn visit_file(&mut self, node: &'ast syn::File) {
        let outer = std::mem::replace(&mut self.roots, authoritative_root_imports(&node.items));
        visit::visit_file(self, node);
        self.roots = outer;
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let Some((_, items)) = &node.content else {
            return;
        };
        let outer = std::mem::replace(&mut self.roots, authoritative_root_imports(items));
        let outer_bindings = std::mem::take(&mut self.bindings);
        for item in items {
            self.visit_item(item);
        }
        self.bindings = outer_bindings;
        self.roots = outer;
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        let outer = self.enter_body(&node.sig);
        visit::visit_item_fn(self, node);
        self.bindings = outer;
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let outer = self.roots.clone();
        for parameter in &node.generics.params {
            if let GenericParam::Type(parameter) = parameter {
                self.roots.remove(&parameter.ident.to_string());
            }
        }
        visit::visit_item_impl(self, node);
        self.roots = outer;
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let outer = self.enter_body(&node.sig);
        visit::visit_impl_item_fn(self, node);
        self.bindings = outer;
    }

    fn visit_block(&mut self, node: &'ast syn::Block) {
        let outer = self.bindings.clone();
        let outer_roots = self.roots.clone();
        for statement in &node.stmts {
            let Stmt::Item(item) = statement else {
                continue;
            };
            match item {
                Item::Use(import) => {
                    apply_root_import(&import.tree, &mut Vec::new(), &mut self.roots);
                }
                Item::Enum(item) => {
                    self.roots.remove(&item.ident.to_string());
                }
                Item::Struct(item) => {
                    self.roots.remove(&item.ident.to_string());
                }
                Item::Trait(item) => {
                    self.roots.remove(&item.ident.to_string());
                }
                Item::TraitAlias(item) => {
                    self.roots.remove(&item.ident.to_string());
                }
                Item::Type(item) => {
                    self.roots.remove(&item.ident.to_string());
                }
                Item::Union(item) => {
                    self.roots.remove(&item.ident.to_string());
                }
                _ => {}
            }
        }
        visit::visit_block(self, node);
        self.roots = outer_roots;
        self.bindings = outer;
    }

    fn visit_local(&mut self, node: &'ast syn::Local) {
        let bound = simple_binding(&node.pat).map(ToString::to_string);
        let path = node
            .init
            .as_ref()
            .and_then(|initial| method_path(&initial.expr, &self.bindings));
        visit::visit_local(self, node);
        self.shadow(&node.pat);
        if let (Some(name), Some(path)) = (bound, path) {
            self.bindings.insert(name, path);
        }
    }

    fn visit_expr_closure(&mut self, node: &'ast syn::ExprClosure) {
        let outer = self.bindings.clone();
        for input in &node.inputs {
            self.shadow(input);
        }
        visit::visit_expr_closure(self, node);
        self.bindings = outer;
    }

    fn visit_arm(&mut self, node: &'ast syn::Arm) {
        let outer = self.bindings.clone();
        self.shadow(&node.pat);
        visit::visit_arm(self, node);
        self.bindings = outer;
    }

    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        let outer = self.bindings.clone();
        self.visit_expr(&node.expr);
        self.shadow(&node.pat);
        self.visit_block(&node.body);
        self.bindings = outer;
    }

    fn visit_expr_let(&mut self, node: &'ast syn::ExprLet) {
        visit::visit_expr_let(self, node);
        self.shadow(&node.pat);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        if let Some(path) = method_call_path(node, &self.bindings)
            && self.declared.contains(&path)
        {
            self.observed.insert(path);
        }
        visit::visit_expr_method_call(self, node);
    }
}

/// The one name a let pattern binds outright, seen through a type annotation.
///
/// Any other pattern binds by destructuring, which carries no path forward.
fn simple_binding(pattern: &Pat) -> Option<&Ident> {
    match pattern {
        Pat::Type(annotated) => simple_binding(&annotated.pat),
        Pat::Ident(binding) if binding.subpat.is_none() => Some(&binding.ident),
        _ => None,
    }
}

fn method_path(expression: &Expr, bindings: &BTreeMap<String, Vec<String>>) -> Option<Vec<String>> {
    match ungroup(expression) {
        Expr::MethodCall(call) => method_call_path(call, bindings),
        Expr::Path(path) if path.path.segments.len() == 1 => {
            let name = path.path.segments.first()?.ident.to_string();
            bindings.get(&name).cloned()
        }
        Expr::Reference(reference) => method_path(&reference.expr, bindings),
        _ => None,
    }
}

fn method_call_path(
    call: &syn::ExprMethodCall,
    bindings: &BTreeMap<String, Vec<String>>,
) -> Option<Vec<String>> {
    let mut path = method_path(&call.receiver, bindings)?;
    path.push(call.method.to_string());
    Some(path)
}

fn authoritative_root_imports(items: &[Item]) -> BTreeSet<String> {
    let mut roots = BTreeSet::new();
    for item in items {
        if let Item::Use(import) = item {
            apply_root_import(&import.tree, &mut Vec::new(), &mut roots);
        }
    }
    roots
}

fn apply_root_import(tree: &UseTree, prefix: &mut Vec<String>, roots: &mut BTreeSet<String>) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            apply_root_import(&path.tree, prefix, roots);
            prefix.pop();
        }
        UseTree::Name(name) => {
            prefix.push(name.ident.to_string());
            if authoritative_root_path(prefix.iter().map(String::as_str)) {
                roots.insert(name.ident.to_string());
            } else {
                roots.remove(&name.ident.to_string());
            }
            prefix.pop();
        }
        UseTree::Rename(rename) => {
            prefix.push(rename.ident.to_string());
            if authoritative_root_path(prefix.iter().map(String::as_str)) {
                roots.insert(rename.rename.to_string());
            } else {
                roots.remove(&rename.rename.to_string());
            }
            prefix.pop();
        }
        UseTree::Group(group) => {
            for item in &group.items {
                apply_root_import(item, prefix, roots);
            }
        }
        UseTree::Glob(_) => {}
    }
}

fn root_type(kind: &Type, roots: &BTreeSet<String>) -> bool {
    match kind {
        Type::Group(group) => root_type(&group.elem, roots),
        Type::Paren(parenthesized) => root_type(&parenthesized.elem, roots),
        Type::Path(path)
            if path.qself.is_none()
                && path.path.leading_colon.is_none()
                && path
                    .path
                    .segments
                    .iter()
                    .all(|segment| matches!(segment.arguments, PathArguments::None)) =>
        {
            let segments: Vec<_> = path
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect();
            authoritative_root_path(segments.iter().map(String::as_str))
                || matches!(segments.as_slice(), [name] if roots.contains(name))
        }
        Type::Reference(reference) => root_type(&reference.elem, roots),
        _ => false,
    }
}

fn authoritative_root_path<'a>(segments: impl Iterator<Item = &'a str>) -> bool {
    segments.eq(["crate", "config", "Root"])
}
