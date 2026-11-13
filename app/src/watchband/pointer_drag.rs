//! Pointer ownership shared by composer, whole-team, roster and design lifts.

use wire::CanonicalUuid;

const POINTER_DRAG_LIFT_PX: u32 = 6;

/// One pointer holding one lifted source, keyed by what the source names: a
/// lane or team by default, or a task for design lifts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PointerDrag<S = CanonicalUuid> {
    pub(crate) source: S,
    pub(crate) pointer_id: i32,
    pub(crate) start_x: i32,
    pub(crate) start_y: i32,
    pub(crate) dragging: bool,
    topology_revision: u64,
}

#[derive(Debug)]
pub(crate) struct PointerDragSession<S = CanonicalUuid> {
    active: Option<PointerDrag<S>>,
}

impl<S> Default for PointerDragSession<S> {
    fn default() -> Self {
        Self { active: None }
    }
}

impl<S: Clone + PartialEq> PointerDragSession<S> {
    pub(crate) fn begin(
        &mut self,
        enabled: bool,
        source: S,
        pointer_id: i32,
        start_x: i32,
        start_y: i32,
        topology_revision: u64,
    ) -> bool {
        if !enabled || self.active.is_some() {
            return false;
        }
        self.active = Some(PointerDrag {
            source,
            pointer_id,
            start_x,
            start_y,
            dragging: false,
            topology_revision,
        });
        true
    }

    pub(crate) fn movement(
        &mut self,
        source: &S,
        pointer_id: i32,
        client_x: i32,
        client_y: i32,
        topology_revision: u64,
    ) -> Option<&mut PointerDrag<S>> {
        let active = self.active.as_mut().filter(|active| {
            active.source == *source
                && active.pointer_id == pointer_id
                && active.topology_revision == topology_revision
        })?;
        if !active.dragging
            && client_x.abs_diff(active.start_x) < POINTER_DRAG_LIFT_PX
            && client_y.abs_diff(active.start_y) < POINTER_DRAG_LIFT_PX
        {
            return None;
        }
        Some(active)
    }

    pub(crate) fn finish(
        &mut self,
        source: &S,
        pointer_id: i32,
        topology_revision: u64,
    ) -> Option<PointerDrag<S>> {
        self.active
            .as_ref()
            .is_some_and(|active| {
                active.source == *source
                    && active.pointer_id == pointer_id
                    && active.topology_revision == topology_revision
            })
            .then(|| self.active.take())
            .flatten()
    }

    pub(crate) fn cancel(&mut self, source: &S, pointer_id: i32) -> bool {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.source == *source && active.pointer_id == pointer_id)
        {
            self.active = None;
            true
        } else {
            false
        }
    }

    pub(crate) fn reconcile(&mut self, enabled: bool, topology_revision: Option<u64>) -> bool {
        if self
            .active
            .as_ref()
            .is_some_and(|active| !enabled || Some(active.topology_revision) != topology_revision)
        {
            self.active = None;
            true
        } else {
            false
        }
    }

    pub(crate) fn clear(&mut self) -> bool {
        self.active.take().is_some()
    }
    pub(crate) fn source(&self) -> Option<S> {
        self.active.as_ref().map(|active| active.source.clone())
    }

    pub(crate) fn dragging_source(&self) -> Option<S> {
        self.active
            .as_ref()
            .filter(|active| active.dragging)
            .map(|active| active.source.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::PointerDragSession;

    fn id(suffix: u8) -> wire::CanonicalUuid {
        format!("00000000-0000-4000-8000-{suffix:012}")
            .parse()
            .expect("fixture identity is canonical")
    }

    #[test]
    fn pointer_drag_session_survives_same_topology_and_cancels_stale_context() {
        let mut session = PointerDragSession::default();
        assert!(session.begin(true, id(1), 17, 100, 200, 7));
        assert!(session.movement(&id(1), 17, 105, 205, 7).is_none());
        assert!(!session.reconcile(true, Some(7)));

        let lifted = session
            .movement(&id(1), 17, 106, 200, 7)
            .expect("the lift threshold starts the same drag after a rerender");
        lifted.dragging = true;
        assert_eq!(session.dragging_source(), Some(id(1)));
        assert!(!session.reconcile(true, Some(7)));
        assert!(
            session
                .finish(&id(1), 17, 7)
                .expect("the surviving drag finishes")
                .dragging
        );

        assert!(session.begin(true, id(1), 18, 0, 0, 7));
        assert!(session.reconcile(true, Some(8)));
        assert!(session.finish(&id(1), 18, 8).is_none());
        assert!(session.begin(true, id(1), 19, 0, 0, 8));
        assert_eq!(session.source(), Some(id(1)));
        assert!(!session.cancel(&id(2), 19));
        assert!(session.cancel(&id(1), 19));
        assert!(session.begin(true, id(1), 20, 0, 0, 8));
        assert!(session.clear());
        assert!(session.begin(true, id(1), 21, 0, 0, 8));
        assert!(session.reconcile(false, Some(8)));
        assert_eq!(session.source(), None);
        assert!(!session.begin(false, id(1), 22, 0, 0, 8));
    }
}
