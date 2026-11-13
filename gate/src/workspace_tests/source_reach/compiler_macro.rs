use super::*;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::workspace_tests) enum CompilerMacroKind {
    Println,
    Format,
    Env,
    OptionEnv,
    Concat,
    Include,
    IncludeStr,
    IncludeBytes,
    Cfg,
    File,
    Line,
    Column,
    ModulePath,
    Stringify,
}

#[derive(Clone, Copy)]
pub(in crate::workspace_tests) enum CompilerMacroPath {
    Unqualified(CompilerMacroKind),
    Qualified {
        namespace: QualifiedNamespace,
        kind: CompilerMacroKind,
        leading_global: bool,
    },
}

impl CompilerMacroKind {
    pub(in crate::workspace_tests) const ALL: [Self; 14] = [
        Self::Println,
        Self::Format,
        Self::Env,
        Self::OptionEnv,
        Self::Concat,
        Self::Include,
        Self::IncludeStr,
        Self::IncludeBytes,
        Self::Cfg,
        Self::File,
        Self::Line,
        Self::Column,
        Self::ModulePath,
        Self::Stringify,
    ];

    pub(in crate::workspace_tests) const fn index(self) -> usize {
        self as usize
    }

    pub(in crate::workspace_tests) fn from_name(name: &str) -> Option<Self> {
        match name {
            "println" => Some(Self::Println),
            "format" => Some(Self::Format),
            "env" => Some(Self::Env),
            "option_env" => Some(Self::OptionEnv),
            "concat" => Some(Self::Concat),
            "include" => Some(Self::Include),
            "include_str" => Some(Self::IncludeStr),
            "include_bytes" => Some(Self::IncludeBytes),
            "cfg" => Some(Self::Cfg),
            "file" => Some(Self::File),
            "line" => Some(Self::Line),
            "column" => Some(Self::Column),
            "module_path" => Some(Self::ModulePath),
            "stringify" => Some(Self::Stringify),
            _ => None,
        }
    }

    pub(in crate::workspace_tests) const fn name(self) -> &'static str {
        match self {
            Self::Println => "println",
            Self::Format => "format",
            Self::Env => "env",
            Self::OptionEnv => "option_env",
            Self::Concat => "concat",
            Self::Include => "include",
            Self::IncludeStr => "include_str",
            Self::IncludeBytes => "include_bytes",
            Self::Cfg => "cfg",
            Self::File => "file",
            Self::Line => "line",
            Self::Column => "column",
            Self::ModulePath => "module_path",
            Self::Stringify => "stringify",
        }
    }

    pub(in crate::workspace_tests) const fn file_kind(self) -> Option<FileMacroKind> {
        match self {
            Self::Include => Some(FileMacroKind::Include),
            Self::IncludeStr => Some(FileMacroKind::IncludeStr),
            Self::IncludeBytes => Some(FileMacroKind::IncludeBytes),
            _ => None,
        }
    }

    pub(in crate::workspace_tests) const fn available_in(
        self,
        namespace: QualifiedNamespace,
    ) -> bool {
        !matches!(
            (self, namespace),
            (Self::Println | Self::Format, QualifiedNamespace::Core)
        )
    }

    pub(in crate::workspace_tests) fn classify_path(path: &syn::Path) -> Option<CompilerMacroPath> {
        let segments = path.segments.iter().collect::<Vec<_>>();
        if segments
            .iter()
            .any(|segment| !matches!(segment.arguments, syn::PathArguments::None))
        {
            return None;
        }
        match segments.as_slice() {
            [name] if path.leading_colon.is_none() => {
                Self::from_name(&ident_name(&name.ident)).map(CompilerMacroPath::Unqualified)
            }
            [namespace, name] => {
                let namespace = qualified_namespace(&ident_name(&namespace.ident))?;
                let kind = Self::from_name(&ident_name(&name.ident))?;
                Some(CompilerMacroPath::Qualified {
                    namespace,
                    kind,
                    leading_global: path.leading_colon.is_some(),
                })
            }
            _ => None,
        }
    }
}

impl From<FileMacroKind> for CompilerMacroKind {
    fn from(value: FileMacroKind) -> Self {
        match value {
            FileMacroKind::Include => Self::Include,
            FileMacroKind::IncludeStr => Self::IncludeStr,
            FileMacroKind::IncludeBytes => Self::IncludeBytes,
        }
    }
}

impl FileMacroKind {
    pub(super) fn from_name(name: &str) -> Option<Self> {
        match name {
            "include" => Some(Self::Include),
            "include_str" => Some(Self::IncludeStr),
            "include_bytes" => Some(Self::IncludeBytes),
            _ => None,
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Include => "include!",
            Self::IncludeStr => "include_str!",
            Self::IncludeBytes => "include_bytes!",
        }
    }

    pub(super) fn classify_path(path: &syn::Path) -> Option<FileMacroPath> {
        let segments = path.segments.iter().collect::<Vec<_>>();
        if segments
            .iter()
            .any(|segment| !matches!(segment.arguments, syn::PathArguments::None))
        {
            return None;
        }
        match segments.as_slice() {
            [name] if path.leading_colon.is_none() => {
                Self::from_name(&ident_name(&name.ident)).map(FileMacroPath::Unqualified)
            }
            [namespace, name]
                if matches!(ident_name(&namespace.ident).as_str(), "std" | "core") =>
            {
                let namespace = qualified_namespace(&ident_name(&namespace.ident))?;
                Self::from_name(&ident_name(&name.ident)).map(|kind| FileMacroPath::Qualified {
                    namespace,
                    kind,
                    leading_global: path.leading_colon.is_some(),
                })
            }
            _ => None,
        }
    }

    pub(in crate::workspace_tests) fn from_path(path: &syn::Path) -> Option<Self> {
        match Self::classify_path(path)? {
            FileMacroPath::Unqualified(kind) | FileMacroPath::Qualified { kind, .. } => Some(kind),
        }
    }
}

impl FileMacroScope {
    pub(in crate::workspace_tests) fn resolve_compiler_macro(
        &self,
        path: &syn::Path,
    ) -> Option<(CompilerMacroKind, FileMacroResolution)> {
        match CompilerMacroKind::classify_path(path)? {
            CompilerMacroPath::Unqualified(kind) => Some((kind, self.macros.resolve(kind))),
            CompilerMacroPath::Qualified {
                namespace,
                kind,
                leading_global,
            } => {
                let resolution = match self.resolve(namespace, leading_global) {
                    NamespaceResolution::Compiler if kind.available_in(namespace) => {
                        FileMacroResolution::Compiler(kind)
                    }
                    NamespaceResolution::Shadowed => FileMacroResolution::Shadowed,
                    NamespaceResolution::Compiler | NamespaceResolution::Unresolved => {
                        FileMacroResolution::Unresolved
                    }
                };
                Some((kind, resolution))
            }
        }
    }
}
