//! Full-history replay authority folded from immutable facts alone.

use std::collections::BTreeMap;

use wire::{
    Actor, AnyFact, CanonicalUuid, Instant, Seq, TaskPreparation, TaskStamp, TranscriptSourceHead,
};

use super::{Authority, Change, Projection, Refusal, claims, dependencies, fold};

/// Every task, transcript generation, and operator direction
/// the history has introduced, beside the projection it derives.
#[derive(Default)]
pub(crate) struct History {
    projection: Projection,
    tasks: BTreeMap<String, Option<TaskPreparation>>,
    task_intents: BTreeMap<String, Option<wire::TaskIntent>>,
    dependencies: dependencies::ReplayDependencies,
    task_statements: BTreeMap<String, Seq>,
    active_claims: BTreeMap<String, Vec<claims::ActiveClaim>>,
    acknowledgements: claims::Acknowledgements,
    transcripts: BTreeMap<String, TranscriptSourceHead>,
    /// Operator direction key to the lane it addresses.
    operator_directions: BTreeMap<String, CanonicalUuid>,
}

impl History {
    /// Fold the complete canonical history, collecting every refusal by
    /// position instead of stopping at the first.
    pub(crate) fn rebuild<'a>(
        facts: impl IntoIterator<Item = (Seq, &'a Actor, Instant, &'a AnyFact)>,
    ) -> (Projection, Vec<(Seq, Refusal)>) {
        let mut history = Self::default();
        let mut refusals = Vec::new();
        for (seq, actor, at, fact) in facts {
            if let Some((task, holder)) = history.acknowledgements.observe(actor, at, fact)
                && let Some(claim) = history
                    .active_claims
                    .get_mut(task.as_str())
                    .and_then(|claims| claims.iter_mut().find(|claim| claim.actor == holder))
            {
                claim.position = seq;
            }
            if let Err(refusal) = fold(&mut history, seq, actor, fact) {
                refusals.push((seq, refusal));
            }
        }
        (history.projection, refusals)
    }
}

impl Authority for History {
    fn task(&self, task: &TaskStamp) -> Result<Option<Option<TaskPreparation>>, Refusal> {
        Ok(self.tasks.get(task.as_str()).cloned())
    }

    fn set_task(&mut self, task: &TaskStamp, preparation: Option<TaskPreparation>) {
        self.tasks.insert(task.as_str().to_owned(), preparation);
    }
    fn task_intent(&self, task: &TaskStamp) -> Result<Option<Option<wire::TaskIntent>>, Refusal> {
        Ok(self.task_intents.get(task.as_str()).cloned())
    }

    fn set_task_intent(&mut self, task: &TaskStamp, intent: Option<wire::TaskIntent>) {
        self.task_intents.insert(task.to_string(), intent);
    }
    fn task_dependencies(&self, task: &TaskStamp) -> Result<Option<Vec<TaskStamp>>, Refusal> {
        Ok(self.dependencies.get(task))
    }

    fn set_task_dependencies(&mut self, task: &TaskStamp, dependencies: Vec<TaskStamp>) {
        self.dependencies.set(task, dependencies);
    }
    fn set_task_birth_dependencies(&mut self, added: &wire::body::TaskAdded) {
        self.dependencies.birth(added);
    }
    fn set_task_edit_dependencies(&mut self, task: &TaskStamp, dependencies: Vec<TaskStamp>) {
        self.dependencies.edit(task, dependencies);
    }
    fn task_statement(&self, task: &TaskStamp) -> Result<Option<Seq>, Refusal> {
        Ok(self.task_statements.get(task.as_str()).copied())
    }

    fn set_task_statement(&mut self, task: &TaskStamp, statement: Seq) {
        self.task_statements
            .insert(task.as_str().to_owned(), statement);
    }

    fn active_claims(&self, task: &TaskStamp) -> Result<Vec<claims::ActiveClaim>, Refusal> {
        Ok(self
            .active_claims
            .get(task.as_str())
            .cloned()
            .unwrap_or_default())
    }

    fn set_active_claims(&mut self, task: &TaskStamp, claims: Vec<claims::ActiveClaim>) {
        if claims.is_empty() {
            self.active_claims.remove(task.as_str());
        } else {
            self.active_claims.insert(task.to_string(), claims);
        }
    }

    fn account(&mut self, head: &TranscriptSourceHead) {
        self.transcripts
            .insert(head.key().into_string(), head.clone());
    }

    fn transcript(
        &self,
        head: &TranscriptSourceHead,
    ) -> Result<Option<TranscriptSourceHead>, Refusal> {
        Ok(self.transcripts.get(head.key().as_str()).cloned())
    }

    fn published(&self, room: &str, publication: &str) -> Result<bool, Refusal> {
        Ok(self
            .projection
            .publications
            .contains_key(&(room.to_owned(), publication.to_owned())))
    }
    fn publication_at(&self, seq: u64) -> Result<bool, Refusal> {
        Ok(self
            .projection
            .publications
            .values()
            .any(|published| *published == seq))
    }
    fn request_at(&self, seq: u64) -> Result<bool, Refusal> {
        Ok(self
            .projection
            .notices
            .keys()
            .any(|(_, noticed)| *noticed == seq))
    }

    fn cursor(&self, lane: &str) -> Result<Option<Vec<u8>>, Refusal> {
        Ok(self
            .projection
            .cursors
            .get(lane)
            .map(|(_, body)| body.clone()))
    }

    fn oops_generation(&self) -> Result<Option<Vec<u8>>, Refusal> {
        Ok(self
            .projection
            .oops_generation
            .as_ref()
            .map(|(_, body)| body.clone()))
    }

    fn proposal(&self, subject: &str) -> Result<Option<Vec<u8>>, Refusal> {
        Ok(self
            .projection
            .proposals
            .get(subject)
            .map(|row| row.body.clone()))
    }

    fn decision_authorized(&self, decision: &str) -> Result<bool, Refusal> {
        Ok(self
            .projection
            .authorizations
            .values()
            .any(|row| row.decision == decision))
    }

    fn operator_direction(&self, key: &str) -> Result<Option<CanonicalUuid>, Refusal> {
        Ok(self.operator_directions.get(key).copied())
    }

    fn set_operator_direction(&mut self, key: &str, lane: CanonicalUuid) {
        self.operator_directions.insert(key.to_owned(), lane);
    }

    fn conversation(&self, publication: &str) -> Result<Option<Vec<u8>>, Refusal> {
        Ok(self
            .projection
            .conversation
            .publications
            .get(publication)
            .map(|(_, body)| body.clone()))
    }

    fn change(&mut self, change: Change) -> Result<(), Refusal> {
        self.projection.apply(change)
    }
}
