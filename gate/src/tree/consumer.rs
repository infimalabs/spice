//! Selected-tree possession shared with processes that consume its source.

use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use sill::{Acquisition, Possession, Running, Spec};

use super::workspace::{release_workspace, workspace_lock};

static CONSUMERS: Mutex<BTreeMap<PathBuf, Possession>> = Mutex::new(BTreeMap::new());

#[derive(Debug)]
pub(super) struct ConsumerLease {
    root: PathBuf,
}

impl ConsumerLease {
    pub(super) fn new(root: &Path, possession: Possession) -> Self {
        let root = root.to_path_buf();
        let previous = CONSUMERS.lock().insert(root.clone(), possession);
        assert!(
            previous.is_none(),
            "selected tree already has a consumer lease"
        );
        Self { root }
    }

    pub(super) fn retire(self, workspace: &Path, retained: bool) {
        let possession = CONSUMERS.lock().remove(&self.root);
        drop(possession);
        if !retained {
            // A child may still hold the inherited open description. Reclaim
            // only after the kernel grants a new possession; otherwise the
            // ordinary abandoned-workspace sweep owns eventual cleanup.
            if let Ok(lock) = workspace_lock(workspace)
                && let Ok(Acquisition::Acquired(possession)) = lock.try_acquire()
            {
                let _ = release_workspace(workspace, &possession);
            }
        }
    }
}

pub(crate) fn spawn(spec: Spec, observation: Option<sill::Observation>) -> io::Result<Running> {
    #[cfg(unix)]
    {
        let mut consumers = CONSUMERS.lock();
        if let Some((_, possession)) = consumers
            .iter_mut()
            .find(|(root, _)| spec.cwd.starts_with(root))
        {
            return sill::spawn_retaining(spec, observation, possession);
        }
    }
    match observation {
        Some(observation) => sill::spawn_observed(spec, observation),
        None => sill::spawn(spec),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{Selector, test_repository};
    use std::fs;

    #[test]
    fn consumer_description_keeps_complete_selection_exclusive_after_tree_drop() {
        let fixture = test_repository::Fixture::repository("consumer-selection");
        fixture.write("src/main.rs", b"fn main() { println!(\"first\"); }\n");
        fixture.write("src/unchanged.rs", b"pub const VALUE: u8 = 7;\n");
        test_repository::stage_all(fixture.path());
        let first = Selector::staged().materialize(fixture.path()).unwrap();
        let root = first.root().to_path_buf();
        let unchanged = fs::metadata(root.join("src/unchanged.rs"))
            .unwrap()
            .modified()
            .unwrap();
        let description = {
            let mut consumers = CONSUMERS.lock();
            let possession = consumers.get_mut(&root).unwrap();
            // Duplication models the same kernel open description inherited
            // at fork; no process is launched by this contract check.
            possession.retain_through_child(&mut std::process::Command::new("unused"));
            possession.file().try_clone().unwrap()
        };
        drop(first);
        fixture.write("src/main.rs", b"fn main() { println!(\"second\"); }\n");
        test_repository::stage_all(fixture.path());
        let second = Selector::staged().materialize(fixture.path()).unwrap();
        assert_ne!(second.root(), root);
        assert_eq!(
            fs::read(root.join("src/main.rs")).unwrap(),
            b"fn main() { println!(\"first\"); }\n"
        );
        assert_eq!(
            fs::read(second.root().join("src/main.rs")).unwrap(),
            b"fn main() { println!(\"second\"); }\n"
        );
        assert_eq!(
            fs::read(root.join("src/unchanged.rs")).unwrap(),
            b"pub const VALUE: u8 = 7;\n"
        );
        drop(description);
        let reused = Selector::staged().materialize(fixture.path()).unwrap();
        assert_eq!(reused.root(), root);
        assert_eq!(
            fs::read(root.join("src/main.rs")).unwrap(),
            b"fn main() { println!(\"second\"); }\n"
        );
        assert_eq!(
            fs::metadata(root.join("src/unchanged.rs"))
                .unwrap()
                .modified()
                .unwrap(),
            unchanged
        );
    }
}
