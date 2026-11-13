//! Owner-scoped state for the app's common feedback area.

/// The workflow that owns one independently replaceable feedback slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToastOwner {
    General,
    Task,
}

/// One owner's current message and presentation state.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ToastSlot<T> {
    pub(crate) text: T,
    pub(crate) shown: bool,
}

/// Both independently owned slots rendered by the common lower feedback area.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ToastArea<T> {
    general: ToastSlot<T>,
    task: ToastSlot<T>,
}

impl<T: Default> ToastArea<T> {
    #[must_use]
    pub(crate) const fn slot(&self, owner: ToastOwner) -> &ToastSlot<T> {
        match owner {
            ToastOwner::General => &self.general,
            ToastOwner::Task => &self.task,
        }
    }

    fn slot_mut(&mut self, owner: ToastOwner) -> &mut ToastSlot<T> {
        match owner {
            ToastOwner::General => &mut self.general,
            ToastOwner::Task => &mut self.task,
        }
    }

    pub(crate) fn apply(&mut self, action: ToastAction<T>) {
        match action {
            ToastAction::Show { owner, text } => {
                let slot = self.slot_mut(owner);
                slot.text = text;
                slot.shown = true;
            }
            ToastAction::Hide { owner } => self.slot_mut(owner).shown = false,
            ToastAction::Clear { owner } => *self.slot_mut(owner) = ToastSlot::<T>::default(),
        }
    }
}

/// One state transition scoped to its named owner.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ToastAction<T> {
    Show { owner: ToastOwner, text: T },
    Hide { owner: ToastOwner },
    Clear { owner: ToastOwner },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(owner: ToastOwner, text: &'static str) -> ToastAction<String> {
        ToastAction::Show {
            owner,
            text: text.to_owned(),
        }
    }

    #[test]
    fn each_feedback_owner_replaces_only_its_own_slot() {
        let mut area = ToastArea::<String>::default();
        area.apply(show(ToastOwner::General, "Ordinary action pending"));
        area.apply(show(ToastOwner::Task, "Task save pending"));
        area.apply(show(ToastOwner::Task, "Task save completed"));

        assert_eq!(
            area.slot(ToastOwner::General).text,
            "Ordinary action pending"
        );
        assert!(area.slot(ToastOwner::General).shown);
        assert_eq!(area.slot(ToastOwner::Task).text, "Task save completed");
        assert!(area.slot(ToastOwner::Task).shown);
    }

    #[test]
    fn hiding_or_clearing_one_owner_preserves_the_other() {
        let mut area = ToastArea::<String>::default();
        area.apply(show(ToastOwner::General, "Ordinary result"));
        area.apply(show(ToastOwner::Task, "Task result"));
        area.apply(ToastAction::Hide {
            owner: ToastOwner::General,
        });

        assert!(!area.slot(ToastOwner::General).shown);
        assert_eq!(area.slot(ToastOwner::Task).text, "Task result");
        assert!(area.slot(ToastOwner::Task).shown);

        area.apply(ToastAction::Clear {
            owner: ToastOwner::Task,
        });
        assert_eq!(area.slot(ToastOwner::General).text, "Ordinary result");
        assert!(area.slot(ToastOwner::Task).text.is_empty());
    }
}
