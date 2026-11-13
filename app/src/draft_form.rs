//! An open Draft editor's own fields, and the one preparation every new or
//! open Draft's rich context passes through.
//!
//! What the operator types belongs to the editor: a live projection of the
//! Draft, whatever refreshed it, never rewrites a field they changed. The
//! editor revises the latest admitted revision it has seen and witnesses it,
//! so a Save sends exactly the fields the operator changed against that
//! revision, and a refused Save keeps every typed field for the retry.
//!
//! A Draft's context is authored in the lane composer's draft state. Task,
//! Plan, and Design prepare it the same way: a new Draft's first line is its
//! title, its next paragraph is its pitch, and the rest is its description.
//! Its images, admitted whole under the shared media bounds before anything is
//! sent, become the Draft's image artifacts.
//!
//! This surface never authors a contract. Creation sends no acceptance, no
//! Design questions, and no flow, so board policy routes every kind through
//! its plan phase first, which forms the contract; the editor revises
//! neither, leaving whatever the plan phase recorded untouched.

use wire::intent::{BoardTask, TaskAdd, TaskEdit, TaskMedia};
use wire::pitch::{PITCH_CHARS, PitchRefusal};
use wire::{Media, TaskHandle, TaskIntent, TaskIntentEdit, TaskStamp, Witness};

use crate::client_action::DraftRequest;
use crate::composer_drafts::{
    ComposerDraft, MediaPlacement, PreparedSubmission, prepare_submission,
};

/// Prepare one Draft context through the shared composer path, carrying image
/// bytes separately for task artifacts.
pub(crate) fn prepare_context(context: ComposerDraft) -> Result<PreparedSubmission, String> {
    prepare_submission(context, MediaPlacement::Artifacts)
        .map_err(|error| format!("The Draft's images were refused: {error}"))
}

/// A new Draft's non-text choice. Its visible text stays in the one composer
/// and is decoded by [`composed_statement`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct NewDraft {
    /// The Kind control: empty for a Task, else the intent's name.
    pub(crate) intent: String,
}

/// A new Draft's creation, its image set, and the composed description its
/// own editor resumes from, quote shards included.
#[derive(Debug)]
pub(crate) struct DraftCreation {
    pub(crate) create: DraftRequest,
    pub(crate) media: Vec<Media>,
    pub(crate) description: String,
}

/// Decode one visible composer into the durable statement fields: first line
/// title, next paragraph pitch, and every later paragraph description.
pub(crate) fn composed_statement(body: &str) -> (&str, &str, &str) {
    let body = body.trim_start();
    let (title, rest) = body.split_once('\n').unwrap_or((body, ""));
    let rest = rest.trim();
    let (pitch, description) = rest.split_once("\n\n").unwrap_or((rest, ""));
    (title.trim(), pitch.trim(), description.trim())
}

/// The creation of a Task, Plan, or Design Draft born `born`: the one visible
/// composer decodes into title, pitch and description. Quotes and images join
/// that description through the same context preparation. The Kind only names
/// the intent, and no topic is sent: the agent that works the task places it
/// when its first phase closes.
pub(crate) fn creation(
    fields: &NewDraft,
    mut context: ComposerDraft,
    born: TaskStamp,
) -> Result<DraftCreation, String> {
    let (title, pitch, description) = composed_statement(&context.body);
    if title.is_empty() {
        return Err("A Draft needs a title.".to_owned());
    }
    let title = title.to_owned();
    let pitch = admitted_pitch(pitch)?;
    let intent = if fields.intent.is_empty() {
        None
    } else {
        Some(fields.intent.parse::<TaskIntent>()?)
    };
    context.body = description.to_owned();
    context.continuation.clear();
    let PreparedSubmission {
        message,
        attachments,
        ..
    } = prepare_context(context)?;
    let description = (!message.trim().is_empty()).then(|| message.clone());
    Ok(DraftCreation {
        create: DraftRequest::Create {
            request: TaskAdd {
                title,
                pitch,
                topic: None,
                description,
                term: Vec::new(),
                requirements: wire::EnvironmentRequirements::default(),
                origin: None,
                flow: None,
                depends: Vec::new(),
                intent,
                preparation: None,
                claim: false,
                draft: true,
            },
            expected: vec![Witness::Absence { task: born }],
        },
        media: attachments,
        description: message,
    })
}

/// Replace the image set of the landed Draft `task` with `media`.
pub(crate) fn media_request(task: &TaskHandle, media: Vec<Media>) -> Result<DraftRequest, String> {
    let born = task.born().map_err(|error| error.to_string())?;
    Ok(DraftRequest::Media {
        request: TaskMedia {
            task: task.clone(),
            media,
        },
        expected: vec![Witness::Existence { task: born }],
    })
}

/// One editable control of an open Draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DraftField {
    Title,
    /// One plain sentence saying why the work matters.
    Pitch,
    /// The Kind control: empty for ordinary work, else the intent's name.
    Intent,
    Description,
}

impl DraftField {
    const ALL: [Self; 4] = [Self::Title, Self::Pitch, Self::Intent, Self::Description];
}

/// The text each control of an open Draft holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DraftFields {
    title: String,
    pitch: String,
    intent: String,
    description: String,
}

impl DraftFields {
    fn admitted(row: &BoardTask) -> Self {
        let statement = &row.statement.body;
        Self {
            title: row.presentation_title.clone(),
            pitch: statement.pitch.clone(),
            intent: statement
                .intent
                .map_or_else(String::new, |intent| intent.as_str().to_owned()),
            description: statement.description.clone().unwrap_or_default(),
        }
    }

    pub(crate) fn get(&self, field: DraftField) -> &str {
        match field {
            DraftField::Title => &self.title,
            DraftField::Pitch => &self.pitch,
            DraftField::Intent => &self.intent,
            DraftField::Description => &self.description,
        }
    }

    fn slot(&mut self, field: DraftField) -> &mut String {
        match field {
            DraftField::Title => &mut self.title,
            DraftField::Pitch => &mut self.pitch,
            DraftField::Intent => &mut self.intent,
            DraftField::Description => &mut self.description,
        }
    }

    /// The intent the Kind control names.
    pub(crate) fn intent(&self) -> Result<Option<TaskIntent>, String> {
        if self.intent.is_empty() {
            return Ok(None);
        }
        self.intent.parse().map(Some)
    }
}

/// One mounted Draft editor: the admitted revision it revises and the fields
/// the operator is editing against it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DraftForm {
    basis: BoardTask,
    fields: DraftFields,
    /// The fields of the operator's last landed Save, until the revision it
    /// admitted arrives.
    saved: Option<DraftFields>,
}

impl DraftForm {
    /// An editor opened on `row`'s admitted content.
    pub(crate) fn open(row: &BoardTask) -> Self {
        Self {
            basis: row.clone(),
            fields: DraftFields::admitted(row),
            saved: None,
        }
    }

    pub(crate) fn fields(&self) -> &DraftFields {
        &self.fields
    }

    /// The operator typed `value` into `field`.
    pub(crate) fn edit(&mut self, field: DraftField, value: String) {
        *self.fields.slot(field) = value;
    }

    /// The operator's Save of `fields` landed.
    pub(crate) fn saved(&mut self, fields: DraftFields) {
        self.saved = Some(fields);
    }

    /// A projection of the Draft rendered.
    ///
    /// Another task reopens the editor on it, and a revision no newer than
    /// the basis changes nothing. A newer revision becomes the basis: each
    /// field the operator has not changed from the old basis, or still holds
    /// as their landed Save sent it, takes the newly admitted value, and every
    /// other field keeps their text.
    pub(crate) fn refresh(&mut self, row: &BoardTask) {
        if row.statement.body.born != self.basis.statement.body.born {
            *self = Self::open(row);
            return;
        }
        if !newer(row, &self.basis) {
            return;
        }
        let was = DraftFields::admitted(&self.basis);
        let now = DraftFields::admitted(row);
        let sent = self.saved.take();
        for field in DraftField::ALL {
            let typed = self.fields.get(field);
            if typed == was.get(field) || sent.as_ref().is_some_and(|sent| sent.get(field) == typed)
            {
                now.get(field).clone_into(self.fields.slot(field));
            }
        }
        self.basis = row.clone();
    }

    /// The Save of everything that differs from what the Draft holds, in the
    /// order it is sent: the fields that moved from the basis, witnessing the
    /// basis revision, then the prepared image set when it differs from the
    /// `admitted` set.
    pub(crate) fn save(
        &self,
        media: Vec<Media>,
        admitted: &[Media],
    ) -> Result<Vec<DraftRequest>, String> {
        let edit = self.statement_edit()?;
        let changed = media.as_slice() != admitted;
        let media = changed
            .then(|| media_request(&self.basis.statement.body.handle, media))
            .transpose()?;
        let requests = edit.into_iter().chain(media).collect::<Vec<_>>();
        if requests.is_empty() {
            return Err("Nothing changed in this Draft.".to_owned());
        }
        Ok(requests)
    }

    fn statement_edit(&self) -> Result<Option<DraftRequest>, String> {
        let statement = &self.basis.statement.body;
        let moved = |field: DraftField, current: &str| {
            let value = self.fields.get(field).trim();
            (value != current.trim()).then(|| value.to_owned())
        };
        let intent = self.fields.intent()?;
        let pitch = admitted_pitch(&self.fields.pitch)?;
        let request = TaskEdit {
            task: statement.handle.clone(),
            title: moved(DraftField::Title, &self.basis.presentation_title),
            pitch: (pitch != statement.pitch).then_some(pitch),
            description: moved(
                DraftField::Description,
                statement.description.as_deref().unwrap_or_default(),
            ),
            term: None,
            requirements: None,
            intent: (intent != statement.intent).then_some(match intent {
                Some(intent) => TaskIntentEdit::Set { intent },
                None => TaskIntentEdit::Clear {},
            }),
            priority: None,
            topic: None,
            flow: None,
        };
        if request.title.is_none()
            && request.pitch.is_none()
            && request.description.is_none()
            && request.intent.is_none()
        {
            return Ok(None);
        }
        Ok(Some(DraftRequest::Edit {
            request,
            expected: vec![Witness::Task {
                task: statement.born.clone(),
                head: self.basis.statement.position,
            }],
        }))
    }
}

/// The trimmed pitch, or the sentence the editor shows instead of sending a
/// pitch the appliance would refuse.
pub(crate) fn admitted_pitch(text: &str) -> Result<String, String> {
    match wire::pitch::admit(text) {
        Ok(()) => Ok(text.trim().to_owned()),
        Err(PitchRefusal::Empty) => {
            Err("A Draft needs one plain sentence saying why it matters.".to_owned())
        }
        Err(PitchRefusal::Multiline) => Err("A pitch stays on one line.".to_owned()),
        Err(PitchRefusal::TooLong { chars }) => Err(format!(
            "A pitch stays within {PITCH_CHARS} characters; this one has {chars}."
        )),
    }
}

/// Whether `row` carries a later revision of the statement every field of
/// the editor reads than `basis`.
fn newer(row: &BoardTask, basis: &BoardTask) -> bool {
    row.statement.position > basis.statement.position
}

#[cfg(test)]
mod tests;
