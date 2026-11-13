//! Constant-cost publication of the mutable app runtime to Yew.

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::rc::Rc;

use wire::Accepted;

use super::AppRuntime;
use crate::pane::Principal;
use crate::socket::Admitted;

/// One shared allocation whose handle is the value published to Yew.
///
/// The connection owns the mutation path and the component owns a clone of
/// this handle. Publishing therefore clones one `Rc`, never the retained
/// runtime behind it. `Shared` is generic so its native regression can put
/// clone-counting payloads behind the exact production ownership primitive.
struct Shared<T>(Rc<RefCell<T>>);

impl<T: Default> Default for Shared<T> {
    fn default() -> Self {
        Self(Rc::new(RefCell::new(T::default())))
    }
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}

impl<T> PartialEq for Shared<T> {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl<T> Shared<T> {
    fn read(&self) -> Ref<'_, T> {
        self.0.borrow()
    }

    fn write(&self) -> RefMut<'_, T> {
        self.0.borrow_mut()
    }
}

/// The runtime cell and the constant-cost handle published after a mutation.
///
/// The app stores this in Yew's `use_state`, whose setter always rerenders; it
/// must not move to `use_state_eq`, because two publications intentionally
/// point at the same allocation.
#[derive(Clone, Default)]
pub struct AppSurface {
    runtime: Shared<Option<AppRuntime>>,
    /// A changed-scope replacement that has not yet been published. The
    /// surface on screen still shows the previous scope until it is.
    replacing: Rc<Cell<bool>>,
}

impl PartialEq for AppSurface {
    fn eq(&self, other: &Self) -> bool {
        self.runtime == other.runtime
    }
}

impl AppSurface {
    #[must_use]
    pub fn read(&self) -> Ref<'_, Option<AppRuntime>> {
        self.runtime.read()
    }

    #[must_use]
    pub fn write(&self) -> RefMut<'_, Option<AppRuntime>> {
        self.runtime.write()
    }

    /// Produce the next Yew state value without copying retained runtime data.
    ///
    /// Publishing a replacement ends its hydration: the screen now shows it.
    #[must_use]
    pub fn publication(&self) -> Self {
        self.replacing.set(false);
        self.clone()
    }

    /// Adopt one admitted connection, returning its principal and whether the
    /// surface it builds must stay unpublished until every snapshot settles.
    ///
    /// A changed scope starts a replacement in a new cell, so the published
    /// surface keeps showing the previous scope. A connection that ends before
    /// that replacement is published leaves it pending: the next acceptance
    /// shares its scope, yet still resumes hydration rather than publishing a
    /// surface without lanes, board, or settings in place of the one on screen.
    pub fn admit(&mut self, accepted: &Admitted<Accepted>) -> (Principal, bool) {
        let changed = self
            .read()
            .as_ref()
            .is_some_and(|runtime| !runtime.same_scope(accepted));
        if changed {
            let runtime = AppRuntime::rehydrate(
                accepted,
                self.read()
                    .as_ref()
                    .expect("changed-scope hydration has a prior runtime"),
            );
            *self = Self::default();
            *self.write() = Some(runtime);
            self.replacing.set(true);
        } else {
            let mut runtime = self.write();
            match runtime.as_mut() {
                Some(runtime) => {
                    runtime.reconnect(accepted);
                }
                None => *runtime = Some(AppRuntime::new(accepted)),
            }
        }
        let principal = self
            .read()
            .as_ref()
            .expect("accepted connection creates runtime")
            .principal()
            .clone();
        (principal, self.replacing.get())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::Shared;

    static PAYLOAD_CLONES: AtomicUsize = AtomicUsize::new(0);

    struct RetainedPayload(String);

    impl Clone for RetainedPayload {
        fn clone(&self) -> Self {
            PAYLOAD_CLONES.fetch_add(1, Ordering::Relaxed);
            Self(self.0.clone())
        }
    }

    #[derive(Default)]
    struct RuntimeFixture {
        cursor: usize,
        lanes: Vec<Vec<RetainedPayload>>,
    }

    fn publication_payload_clones(lanes: usize, rows_per_lane: usize) -> usize {
        let retained = (0..lanes)
            .map(|lane| {
                (0..rows_per_lane)
                    .map(|row| RetainedPayload(format!("lane-{lane}-row-{row}")))
                    .collect()
            })
            .collect();
        let runtime = Shared(Rc::new(RefCell::new(RuntimeFixture {
            cursor: 0,
            lanes: retained,
        })));
        PAYLOAD_CLONES.store(0, Ordering::Relaxed);

        // The same one-field frame mutation is applied to both fixtures.
        runtime.write().cursor += 1;
        let published = runtime.clone();

        assert!(Rc::ptr_eq(&runtime.0, &published.0));
        assert_eq!(published.read().cursor, 1);
        assert_eq!(published.read().lanes.len(), lanes);
        PAYLOAD_CLONES.load(Ordering::Relaxed)
    }

    #[test]
    fn publication_clones_no_payload_for_small_or_full_multilane_retention() {
        assert_eq!(publication_payload_clones(1, 1), 0);
        assert_eq!(publication_payload_clones(32, 256), 0);
    }
}
