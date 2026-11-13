//! Browser reducer adapting the target-neutral composer store to Yew.
//!
//! Work switches and speech policy belong to `controls`; this module owns only
//! draft state. Runtime work policy always settles against resident authority.

use std::rc::Rc;

use wire::Media;
use yew::prelude::*;

use super::MAX_ATTACHMENTS;
use crate::composer_drafts::{ComposerDraft, DraftImage, DraftStore, QuoteDestination};
use crate::image_annotation::Shape;

#[derive(Clone, Default, PartialEq)]
pub(in crate::browser) struct ComposerDrafts(
    pub(super) DraftStore,
    Option<ComposerIssue>,
    Option<CreatorSummons>,
);

/// The latest composer refusal. Its serial distinguishes a repeat of the same
/// text, so every refusal is announced again.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ComposerIssue {
    serial: u64,
    text: String,
}

/// The latest quote sent to the global New Draft creator. Its serial
/// distinguishes a repeat of the same source, so each one opens the creator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::browser) struct CreatorSummons {
    pub(in crate::browser) serial: u64,
    /// The quote shard identity the creator focuses.
    pub(in crate::browser) identity: String,
}

pub(in crate::browser) enum ComposerAction {
    /// Add a quote shard to `lane`'s composer or, by `destination`, to the
    /// global New Draft creator.
    AddQuote {
        lane: String,
        destination: QuoteDestination,
        target: wire::QuoteTarget,
        source_id: String,
        order: u64,
        display_millis: i64,
        preview: String,
    },
    RemoveQuote {
        lane: String,
        identity: String,
    },
    AddAttachment {
        lane: String,
        quote: Option<String>,
        attachment: Media,
    },
    RemoveAttachment {
        lane: String,
        quote: Option<String>,
        id: u64,
    },
    ApplyImage {
        lane: String,
        quote: Option<String>,
        id: u64,
        revision: u64,
        media: Media,
        shapes: Rc<Vec<Shape>>,
    },
    /// Announce a refusal of composer input through the shared toast.
    Report {
        issue: String,
    },
    Clear {
        lane: String,
    },
    /// Remove the content a durably accepted send carried.
    ClearSent {
        lane: String,
        sent: Rc<ComposerDraft>,
    },
    /// Text typed into a composer element that is being discarded.
    KeepDetached {
        lane: String,
        quote: Option<String>,
        drawn: String,
        live: String,
    },
    /// Draw a Draft composer from its admitted description and images unless
    /// it already holds a draft.
    Hydrate {
        lane: String,
        body: String,
        media: Vec<Media>,
    },
    /// Redraw a composer's text from a newer admitted revision.
    SetBody {
        lane: String,
        body: String,
    },
    /// A save of `draft`, read from a Draft composer, landed.
    Saved {
        lane: String,
        draft: ComposerDraft,
    },
}

impl Reducible for ComposerDrafts {
    type Action = ComposerAction;

    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        let mut next = (*self).clone();
        match action {
            ComposerAction::AddQuote {
                lane,
                destination,
                target,
                source_id,
                order,
                display_millis,
                preview,
            } => {
                let identity = target.to_string();
                let key = destination.key(&lane).to_owned();
                next.0
                    .add_quote(key, target, source_id, order, display_millis, preview);
                if destination == QuoteDestination::NewDraft {
                    next.summon_creator(identity);
                }
            }
            ComposerAction::RemoveQuote { lane, identity } => {
                next.0.remove_quote(lane, &identity);
            }
            ComposerAction::AddAttachment {
                lane,
                quote,
                attachment,
            } => {
                let id = next.0.issue_image();
                let draft = next.0.draft_mut(lane);
                let attachment_count = draft.attachments.len()
                    + draft
                        .quotes
                        .iter()
                        .map(|quote| quote.attachments.len())
                        .sum::<usize>();
                let media_bytes =
                    draft_media_data_url_bytes(draft).saturating_add(attachment.data_url.len());
                if attachment_count >= MAX_ATTACHMENTS {
                    next.report("Maximum images attached.".to_owned());
                } else if media_bytes > wire::media::MAX_MEDIA_DATA_URL_BYTES {
                    next.report("Attached images exceed the submission limit.".to_owned());
                } else if let Some(attachments) = attachment_state_mut(draft, quote.as_deref()) {
                    attachments.push(DraftImage::new(id, attachment));
                }
            }
            ComposerAction::RemoveAttachment { lane, quote, id } => {
                let draft = next.0.draft_mut(lane);
                if let Some(attachments) = attachment_state_mut(draft, quote.as_deref()) {
                    attachments.retain(|image| image.id != id);
                }
            }
            ComposerAction::ApplyImage {
                lane,
                quote,
                id,
                revision,
                media,
                shapes,
            } => {
                let exceeds_limit = next.0.get(&lane).is_some_and(|draft| {
                    let previous = attachment_state(draft, quote.as_deref())
                        .and_then(|attachments| {
                            attachments
                                .iter()
                                .find(|image| image.id == id && image.revision == revision)
                        })
                        .map_or(0, |image| image.media.data_url.len());
                    draft_media_data_url_bytes(draft)
                        .saturating_sub(previous)
                        .saturating_add(media.data_url.len())
                        > wire::media::MAX_MEDIA_DATA_URL_BYTES
                });
                if exceeds_limit {
                    next.report("Annotated image exceeds the submission limit.".to_owned());
                } else if !next
                    .0
                    .apply_image(&lane, quote.as_deref(), id, revision, media, shapes)
                {
                    return self;
                }
            }
            ComposerAction::Report { issue } => next.report(issue),
            ComposerAction::Clear { lane } => {
                next.0.clear(&lane);
            }
            ComposerAction::ClearSent { lane, sent } => {
                next.0.clear_sent(&lane, &sent);
            }
            ComposerAction::KeepDetached {
                lane,
                quote,
                drawn,
                live,
            } => next.0.keep_detached(lane, quote.as_deref(), &drawn, live),
            ComposerAction::Hydrate { lane, body, media } => next.0.hydrate(lane, body, media),
            ComposerAction::SetBody { lane, body } => next.0.set_body(lane, body),
            ComposerAction::Saved { lane, draft } => next.0.saved(lane, draft),
        }
        Rc::new(next)
    }
}

pub(super) fn draft_media_data_url_bytes(draft: &ComposerDraft) -> usize {
    draft
        .quotes
        .iter()
        .flat_map(|quote| &quote.attachments)
        .chain(&draft.attachments)
        .map(|image| image.media.data_url.len())
        .sum()
}

fn attachment_state<'a>(
    draft: &'a ComposerDraft,
    quote_identity: Option<&str>,
) -> Option<&'a [DraftImage]> {
    match quote_identity {
        None => Some(&draft.attachments),
        Some(identity) => draft
            .quotes
            .iter()
            .find(|quote| quote.identity == identity)
            .map(|quote| quote.attachments.as_slice()),
    }
}

fn attachment_state_mut<'a>(
    draft: &'a mut ComposerDraft,
    quote_identity: Option<&str>,
) -> Option<&'a mut Vec<DraftImage>> {
    match quote_identity {
        None => Some(&mut draft.attachments),
        Some(identity) => draft
            .quotes
            .iter_mut()
            .find(|quote| quote.identity == identity)
            .map(|quote| &mut quote.attachments),
    }
}

impl ComposerDrafts {
    /// The drafts a document starts with: those the previous document of its
    /// tab handed off, or none with the refusal announced when they could
    /// not be read.
    pub(in crate::browser) fn adopt(
        handoff: &Result<crate::composer_drafts::DraftHandoff, String>,
    ) -> Self {
        match handoff {
            Ok(handoff) => Self(DraftStore::adopt(handoff.composers.clone()), None, None),
            Err(issue) => {
                let mut drafts = Self::default();
                drafts.report(issue.clone());
                drafts
            }
        }
    }

    pub(in crate::browser) fn store(&self) -> &DraftStore {
        &self.0
    }

    fn report(&mut self, text: String) {
        let serial = self
            .1
            .as_ref()
            .map_or(0, |issue| issue.serial.wrapping_add(1));
        self.1 = Some(ComposerIssue { serial, text });
    }

    fn summon_creator(&mut self, identity: String) {
        let serial = self
            .2
            .as_ref()
            .map_or(0, |summons| summons.serial.wrapping_add(1));
        self.2 = Some(CreatorSummons { serial, identity });
    }

    /// The latest quote sent to the global New Draft creator.
    pub(in crate::browser) fn creator_summons(&self) -> Option<&CreatorSummons> {
        self.2.as_ref()
    }

    fn issue(&self) -> Option<&ComposerIssue> {
        self.1.as_ref()
    }

    pub(in crate::browser) fn image(
        &self,
        lane: &str,
        quote: Option<&str>,
        id: u64,
    ) -> Option<&DraftImage> {
        self.0.image(lane, quote, id)
    }
    pub(in crate::browser) fn get(&self, lane: &str) -> Option<&ComposerDraft> {
        self.0.get(lane)
    }

    /// Whether `lane` holds a quote or an image. Typed text lives in its
    /// composer element until sent, so the element alone knows of it.
    pub(in crate::browser) fn holds_quotes_or_images(&self, lane: &str) -> bool {
        self.0
            .get(lane)
            .is_some_and(|draft| !draft.quotes.is_empty() || !draft.attachments.is_empty())
    }
}

/// Announce each composer refusal through the root toast, so the refusal
/// fades on its own instead of shifting the composer it concerns.
#[hook]
pub(in crate::browser) fn use_composer_issue_toast(drafts: &UseReducerHandle<ComposerDrafts>) {
    let toaster = crate::browser::toast::use_toaster();
    use_effect_with(drafts.issue().cloned(), move |issue| {
        if let Some(issue) = issue {
            toaster.settled(issue.text.clone());
        }
    });
}
