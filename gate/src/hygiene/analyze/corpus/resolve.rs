use super::*;

impl Corpus {
    pub(super) fn resolved_references(
        &self,
        work: &mut GraphWork,
    ) -> Option<Vec<ResolvedReference>> {
        let mut known = BTreeSet::new();
        for name in self.modules.keys() {
            if !work.step() {
                return None;
            }
            known.insert(name.clone());
        }
        let mut declarations = BTreeSet::new();
        for (module, row) in self.declarations() {
            if !work.step() {
                return None;
            }
            declarations.insert((module.name.clone(), row.name.clone()));
        }
        let mut output = BTreeSet::new();
        for module in self.modules.values() {
            if !work.step() {
                return None;
            }
            let from_test = self.test_reachable.contains(&module.name)
                && !self.production_reachable.contains(&module.name);
            if !from_test && !self.production_reachable.contains(&module.name) {
                continue;
            }
            for reference in &module.references {
                if !work.step() {
                    return None;
                }
                if let Some((target_module, symbol)) =
                    module.resolve_reference(&reference.name, &known, &declarations)
                {
                    output.insert(ResolvedReference {
                        target_module,
                        symbol,
                        source_module: module.name.clone(),
                        source_symbol: reference.owner.clone(),
                        path: module.path.clone(),
                        from_test,
                    });
                }
            }
        }
        work.check().then(|| output.into_iter().collect())
    }

    pub(super) fn production_symbols(
        &self,
        references: &[ResolvedReference],
        work: &mut GraphWork,
    ) -> Option<BTreeSet<(String, String)>> {
        let mut declarations = BTreeSet::new();
        let mut reached = BTreeSet::new();
        for (module, row) in self.declarations() {
            if !work.step() {
                return None;
            }
            let symbol = (module.name.clone(), row.name.clone());
            if row.top_level && self.production_roots.contains(&module.name) {
                reached.insert(symbol.clone());
            }
            declarations.insert(symbol);
        }
        for reference in references
            .iter()
            .filter(|row| !row.from_test && row.source_symbol.is_none())
        {
            if !work.step() {
                return None;
            }
            let target = (reference.target_module.clone(), reference.symbol.clone());
            if declarations.contains(&target) {
                reached.insert(target);
            }
        }
        loop {
            let before = reached.len();
            for reference in references.iter().filter(|row| !row.from_test) {
                if !work.step() {
                    return None;
                }
                let Some(source_symbol) = &reference.source_symbol else {
                    continue;
                };
                if !reached.contains(&(reference.source_module.clone(), source_symbol.clone())) {
                    continue;
                }
                let target = (reference.target_module.clone(), reference.symbol.clone());
                if declarations.contains(&target) {
                    reached.insert(target);
                }
            }
            if reached.len() == before {
                break;
            }
        }
        work.check().then_some(reached)
    }

    pub(super) fn private_couplings(&self, work: &mut GraphWork) -> Option<Vec<PrivateCoupling>> {
        let packages = self.production_packages();
        let mut known = BTreeSet::new();
        for name in self.modules.keys() {
            if !work.step() {
                return None;
            }
            known.insert(name.clone());
        }
        let mut output = BTreeSet::new();
        for module in self.modules.values().filter(|module| module.is_test) {
            if !work.step() {
                return None;
            }
            for coupling in &module.private_couplings {
                if !work.step() {
                    return None;
                }
                let target = module
                    .resolve_reference(&coupling.target, &known, &BTreeSet::new())
                    .map_or_else(
                        || coupling.target.clone(),
                        |(name, symbol)| {
                            if symbol.is_empty() {
                                name
                            } else {
                                format!("{name}.{symbol}")
                            }
                        },
                    );
                if target
                    .split('.')
                    .next()
                    .is_some_and(|package| packages.contains(package))
                    && has_private_component(&target)
                {
                    output.insert(PrivateCoupling {
                        path: module.path.clone(),
                        line: coupling.line,
                        test_name: coupling.test_name.clone(),
                        kind: coupling.kind,
                        target,
                    });
                }
            }
        }
        work.check().then(|| output.into_iter().collect())
    }
}
