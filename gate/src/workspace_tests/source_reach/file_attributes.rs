use super::*;

impl SourceGraph {
    pub(super) fn collect_file_attributes(
        &mut self,
        attributes: &[syn::Attribute],
        source: &Path,
        scope: FileMacroScope,
        walk: &SourceWalk,
    ) -> Result<(), WorkspaceTestError> {
        let attribute_walk = SourceWalk {
            scope: Some(scope.clone()),
            ..walk.clone()
        };
        for reference in file_macro_references_in_attributes(
            attributes,
            source,
            scope,
            walk.crate_root,
            &self.exported_macros,
        )? {
            self.collect_file_macro_source(reference, source, &attribute_walk)?;
        }
        Ok(())
    }
}

fn file_macro_references_in_attributes(
    attributes: &[syn::Attribute],
    source: &Path,
    scope: FileMacroScope,
    crate_root: bool,
    exported_macros: &BTreeSet<String>,
) -> Result<Vec<FileMacroReference>, WorkspaceTestError> {
    let mut visitor = FileMacroVisitor {
        source,
        exported_macros,
        scope,
        crate_root,
        references: Vec::new(),
        error: None,
    };
    for attribute in attributes {
        visitor.visit_attribute(attribute);
    }
    match visitor.error {
        Some(error) => Err(error),
        None => Ok(visitor.references),
    }
}
