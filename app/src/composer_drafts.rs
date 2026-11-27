//! Target-keyed state for the plural lane composer.
//!
//! The browser owns elements and focus, but target routing, quote identity,
//! Markdown payloads, and optimistic restore are ordinary state transitions.
//! Keeping those transitions here makes the shipped plural behavior reachable
//! from native tests instead of testing a second, simplified implementation.

use std::borrow::{Borrow, Cow};
use std::collections::BTreeMap;
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use wire::Media;
use wire::{AgentMessage, CanonicalUuid, QuoteTarget};

use crate::composer_markdown::QuotedReply;
use crate::image_annotation::Shape;
use crate::runtime::LaneMember;

/// Shared context for the global Message / Task / Plan / Design composer.
pub(crate) const NEW_DRAFT_KEY: &str = "spice-draft:new";

/// The global composer's persistent longer-context element.
#[cfg(any(target_arch = "wasm32", rust_analyzer))]
pub(crate) const CREATOR_CONTEXT_ID: &str = "app-compose-context";

/// Where one Quote control sends its shard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QuoteDestination {
    /// The composer of the quoted source's own lane or room.
    Composer,
    /// The global composer in Task mode, whatever composer the source belongs to.
    NewDraft,
}

impl QuoteDestination {
    /// The composer key a quote offered to `composer` lands under.
    pub(crate) fn key(self, composer: &str) -> &str {
        match self {
            Self::Composer => composer,
            Self::NewDraft => NEW_DRAFT_KEY,
        }
    }
}

/// Original pixels and marks remain local; only `media` crosses the socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DraftImage {
    pub(crate) id: u64,
    pub(crate) revision: u64,
    pub(crate) original: Rc<Media>,
    pub(crate) media: Rc<Media>,
    pub(crate) shapes: Rc<Vec<Shape>>,
}

impl DraftImage {
    pub(crate) fn new(id: u64, media: Media) -> Self {
        let media = Rc::new(media);
        Self {
            id,
            revision: 0,
            original: media.clone(),
            media,
            shapes: Rc::default(),
        }
    }
}

impl Borrow<Media> for DraftImage {
    fn borrow(&self) -> &Media {
        &self.media
    }
}

/// Every draft belonging to one independently steerable target.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ComposerDraft {
    pub(crate) body: String,
    /// Longer primary text, shown separately from the global composer's title line.
    pub(crate) continuation: String,
    pub(crate) quotes: Vec<QuoteDraft>,
    pub(crate) attachments: Vec<DraftImage>,
}

impl ComposerDraft {
    fn is_empty(&self) -> bool {
        self.body.is_empty()
            && self.continuation.is_empty()
            && self.quotes.is_empty()
            && self.attachments.is_empty()
    }
}

/// One canonical quoted source and the reply being composed beneath it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QuoteDraft {
    pub(crate) identity: String,
    pub(crate) source_id: String,
    pub(crate) preview: String,
    pub(crate) display_millis: i64,
    pub(crate) body: String,
    pub(crate) attachments: Vec<DraftImage>,
    order: u64,
}

/// Whether adding a quote created a band or found its existing identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QuotePlacement {
    Added,
    Existing,
}

/// The exact target-specific payload detached for one optimistic send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedSubmission {
    pub(crate) message: String,
    pub(crate) attachments: Vec<Media>,
    pub(crate) snapshot: ComposerDraft,
}
/// How prepared image bytes are represented in the composed text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MediaPlacement {
    /// Lane messages carry authenticated Markdown media references.
    Markdown,
    /// Task Drafts persist the bytes as artifacts beside their description.
    Artifacts,
}

/// Prepare one owned composer snapshot for any transport. Text composition,
/// quote and primary attachment ordering, and whole-set media admission stay
/// one invariant; only the destination's explicit media placement differs.
pub(crate) fn prepare_submission(
    snapshot: ComposerDraft,
    placement: MediaPlacement,
) -> Result<PreparedSubmission, wire::media::MediaError> {
    let attachments = snapshot
        .quotes
        .iter()
        .flat_map(|quote| quote.attachments.iter())
        .chain(snapshot.attachments.iter())
        .map(|image| (*image.media).clone())
        .collect::<Vec<_>>();
    wire::media::admit_media(&attachments)?;
    let message = compose_for(&snapshot, placement);
    Ok(PreparedSubmission {
        message,
        attachments,
        snapshot,
    })
}

/// Stable composer state keyed by the lane target rather than render order.
///
/// The last field keeps what each target drawn from admitted content was
/// drawn, redrawn, or saved as. A draft still equal to it is no operator
/// work: leaving discards nothing, so it is neither handed off nor listed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DraftStore(
    BTreeMap<String, ComposerDraft>,
    u64,
    BTreeMap<String, ComposerDraft>,
);

impl DraftStore {
    /// Not reset by clearing a draft: a late export must never own a later paste.
    pub(crate) fn issue_image(&mut self) -> u64 {
        self.1 += 1;
        self.1
    }

    pub(crate) fn image(&self, lane: &str, quote: Option<&str>, id: u64) -> Option<&DraftImage> {
        let draft = self.get(lane)?;
        let images = match quote {
            None => &draft.attachments,
            Some(identity) => {
                &draft
                    .quotes
                    .iter()
                    .find(|quote| quote.identity == identity)?
                    .attachments
            }
        };
        images.iter().find(|image| image.id == id)
    }

    pub(crate) fn apply_image(
        &mut self,
        lane: &str,
        quote: Option<&str>,
        id: u64,
        revision: u64,
        media: Media,
        shapes: Rc<Vec<Shape>>,
    ) -> bool {
        let Some(draft) = self.0.get_mut(lane) else {
            return false;
        };
        let images = match quote {
            None => &mut draft.attachments,
            Some(identity) => {
                let Some(quote) = draft
                    .quotes
                    .iter_mut()
                    .find(|quote| quote.identity == identity)
                else {
                    return false;
                };
                &mut quote.attachments
            }
        };
        let Some(image) = images
            .iter_mut()
            .find(|image| image.id == id && image.revision == revision)
        else {
            return false;
        };
        image.media = Rc::new(media);
        image.shapes = shapes;
        image.revision += 1;
        true
    }
    pub(crate) fn get(&self, target: &str) -> Option<&ComposerDraft> {
        self.0.get(target)
    }

    pub(crate) fn draft_mut(&mut self, target: String) -> &mut ComposerDraft {
        self.0.entry(target).or_default()
    }

    /// Set `target`'s text. A target drawn from admitted content is being
    /// redrawn from a newer revision, so what it was drawn as moves too.
    pub(crate) fn set_body(&mut self, target: String, body: String) {
        if let Some(drawn) = self.2.get_mut(&target) {
            drawn.body.clone_from(&body);
        }
        self.draft_mut(target).body = body;
    }

    /// Record that admitted content now equals `saved`, as `target` held it
    /// when a landed save read it.
    pub(crate) fn saved(&mut self, target: String, saved: ComposerDraft) {
        self.2.insert(target, saved);
    }

    /// Draw `target` from admitted content, unless it already holds a draft:
    /// the body and each image, under fresh image identities, in order. A
    /// draft the operator kept, such as text a closed editor handed back,
    /// stays theirs.
    pub(crate) fn hydrate(&mut self, target: String, body: String, media: Vec<Media>) {
        if self.0.contains_key(&target) {
            return;
        }
        let attachments = media
            .into_iter()
            .map(|media| DraftImage::new(self.issue_image(), media))
            .collect();
        let drawn = ComposerDraft {
            body,
            continuation: String::new(),
            quotes: Vec::new(),
            attachments,
        };
        self.2.insert(target.clone(), drawn.clone());
        self.0.insert(target, drawn);
    }

    pub(crate) fn add_quote(
        &mut self,
        lane: String,
        quote_target: QuoteTarget,
        source_id: String,
        order: u64,
        display_millis: i64,
        preview: String,
    ) -> QuotePlacement {
        let identity = quote_target.to_string();
        let draft = self.draft_mut(lane);
        if draft
            .quotes
            .iter()
            .any(|candidate| candidate.identity == identity)
        {
            return QuotePlacement::Existing;
        }
        draft.quotes.push(QuoteDraft {
            identity,
            source_id,
            preview,
            display_millis,
            body: String::new(),
            attachments: Vec::new(),
            order,
        });
        sort_quotes(&mut draft.quotes);
        QuotePlacement::Added
    }

    pub(crate) fn remove_quote(&mut self, target: String, identity: &str) {
        self.draft_mut(target)
            .quotes
            .retain(|quote| quote.identity != identity);
    }

    #[cfg(test)]
    pub(crate) fn set_quote_body(&mut self, target: String, identity: &str, body: String) {
        if let Some(quote) = self
            .draft_mut(target)
            .quotes
            .iter_mut()
            .find(|quote| quote.identity == identity)
        {
            quote.body = body;
        }
    }

    pub(crate) fn clear(&mut self, target: &str) {
        self.0.remove(target);
        self.2.remove(target);
    }

    /// Keep text typed into a composer element the page is discarding.
    ///
    /// Typed text lives only in its element until submission, so an element
    /// rebuilt elsewhere, such as a lane moved into another team, redraws from
    /// the store and would lose it. The text is kept only while the store still
    /// holds what the element was drawn from: a draft cleared, restored, or
    /// removed since then replaced that element on purpose.
    pub(crate) fn keep_detached(
        &mut self,
        target: String,
        quote: Option<&str>,
        drawn: &str,
        live: String,
    ) {
        if live == drawn {
            return;
        }
        let draft = self.draft_mut(target);
        let body = match quote {
            None => &mut draft.body,
            Some(identity) => match draft
                .quotes
                .iter_mut()
                .find(|quote| quote.identity == identity)
            {
                Some(quote) => &mut quote.body,
                None => return,
            },
        };
        if body == drawn {
            *body = live;
        }
    }

    /// Build one request from one target without observing sibling drafts.
    /// An empty composition prepares nothing. A media set the resident would
    /// refuse, as a restored refused send can produce, is refused here and the
    /// draft keeps every attachment.
    pub(crate) fn prepare(
        &self,
        target: &str,
    ) -> Result<Option<PreparedSubmission>, wire::media::MediaError> {
        let Some(draft) = self.get(target) else {
            return Ok(None);
        };
        let prepared = prepare_submission(draft.clone(), MediaPlacement::Markdown)?;
        Ok((!prepared.message.trim().is_empty()).then_some(prepared))
    }

    /// Detach only the primary body and its direct attachments for broadcast,
    /// under the same admission as [`Self::prepare`].
    pub(crate) fn prepare_primary(
        &self,
        target: &str,
    ) -> Result<Option<PreparedSubmission>, wire::media::MediaError> {
        let Some(draft) = self
            .get(target)
            .filter(|draft| !draft.body.trim().is_empty() || !draft.continuation.trim().is_empty())
        else {
            return Ok(None);
        };
        let snapshot = ComposerDraft {
            body: draft.body.clone(),
            continuation: draft.continuation.clone(),
            quotes: Vec::new(),
            attachments: draft.attachments.clone(),
        };
        prepare_submission(snapshot, MediaPlacement::Markdown).map(Some)
    }

    /// Remove exactly the content a durably accepted send carried: the body,
    /// and the sent quotes and images by identity. A broadcast sends only the
    /// primary body and images, so its quote shards stay; anything drafted
    /// beside a send while it was in flight stays too.
    pub(crate) fn clear_sent(&mut self, target: &str, sent: &ComposerDraft) {
        let remove = self.0.get_mut(target).is_some_and(|draft| {
            draft.body.clear();
            draft.continuation.clear();
            draft
                .attachments
                .retain(|image| !sent.attachments.iter().any(|item| item.id == image.id));
            draft.quotes.retain(|quote| {
                !sent
                    .quotes
                    .iter()
                    .any(|item| item.identity == quote.identity)
            });
            draft.is_empty()
        });
        if remove {
            self.clear(target);
        }
    }

    /// Every draft this store and its live composer elements hold, apart from
    /// those still as drawn, in the form the next document of this tab adopts.
    ///
    /// Typed text lives in its element until it is sent, so `live` supplies
    /// each element's current text and wins over the body it was drawn from.
    pub(crate) fn hand_off(&self, live: Vec<LiveText>) -> Vec<HandedComposer> {
        let mut store = self.clone();
        for LiveText {
            target,
            quote,
            continuation,
            text,
        } in live
        {
            let draft = store.draft_mut(target);
            if continuation {
                draft.continuation = text;
                continue;
            }
            match quote {
                None => draft.body = text,
                Some(identity) => {
                    if let Some(quote) = draft
                        .quotes
                        .iter_mut()
                        .find(|quote| quote.identity == identity)
                    {
                        quote.body = text;
                    }
                }
            }
        }
        let media = |images: Vec<DraftImage>| {
            images
                .into_iter()
                .map(|image| (*image.media).clone())
                .collect()
        };
        let Self(drafts, _, drawn) = store;
        drafts
            .into_iter()
            .filter(|(target, draft)| !draft.is_empty() && drawn.get(target) != Some(draft))
            .map(|(target, draft)| HandedComposer {
                target,
                body: draft.body,
                continuation: draft.continuation,
                quotes: draft
                    .quotes
                    .into_iter()
                    .map(|quote| HandedQuote {
                        identity: quote.identity,
                        source_id: quote.source_id,
                        preview: quote.preview,
                        display_millis: quote.display_millis,
                        body: quote.body,
                        attachments: media(quote.attachments),
                        order: quote.order,
                    })
                    .collect(),
                attachments: media(draft.attachments),
            })
            .collect()
    }

    /// The store a document starts with when the previous document of its tab
    /// handed these drafts off.
    pub(crate) fn adopt(composers: Vec<HandedComposer>) -> Self {
        let mut store = Self::default();
        for composer in composers {
            let attachments = composer
                .attachments
                .into_iter()
                .map(|media| DraftImage::new(store.issue_image(), media))
                .collect();
            let mut quotes = Vec::with_capacity(composer.quotes.len());
            for quote in composer.quotes {
                quotes.push(QuoteDraft {
                    identity: quote.identity,
                    source_id: quote.source_id,
                    preview: quote.preview,
                    display_millis: quote.display_millis,
                    body: quote.body,
                    attachments: quote
                        .attachments
                        .into_iter()
                        .map(|media| DraftImage::new(store.issue_image(), media))
                        .collect(),
                    order: quote.order,
                });
            }
            sort_quotes(&mut quotes);
            store.0.insert(
                composer.target,
                ComposerDraft {
                    body: composer.body,
                    continuation: composer.continuation,
                    quotes,
                    attachments,
                },
            );
        }
        store
    }
}

/// The current text of one composer element: a target's primary composer, or
/// the reply beneath one of its quotes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveText {
    pub(crate) target: String,
    pub(crate) quote: Option<String>,
    pub(crate) continuation: bool,
    pub(crate) text: String,
}

/// Unsent drafts one document of a browser tab hands to the next.
///
/// The next document may be a newer app generation, so this shape is the
/// contract between generations: a change needs a new storage name that the
/// newer reader also accepts. Each attachment travels as the media it would
/// send, so annotation marks arrive flattened into the image.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DraftHandoff {
    pub(crate) composers: Vec<HandedComposer>,
    pub(crate) notes: Vec<HandedNote>,
}

/// One target's handed-off primary text, quotes, and attachments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HandedComposer {
    pub(crate) target: String,
    pub(crate) body: String,
    #[serde(default)]
    pub(crate) continuation: String,
    pub(crate) quotes: Vec<HandedQuote>,
    pub(crate) attachments: Vec<Media>,
}

/// One handed-off quote band and the reply composed beneath it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HandedQuote {
    pub(crate) identity: String,
    pub(crate) source_id: String,
    pub(crate) preview: String,
    pub(crate) display_millis: i64,
    pub(crate) body: String,
    pub(crate) attachments: Vec<Media>,
    pub(crate) order: u64,
}

/// One task's handed-off observation text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HandedNote {
    pub(crate) task: wire::TaskStamp,
    pub(crate) text: String,
}

/// The lane a quote taken from `message` composes into.
///
/// A quote answers the agent that wrote it, so a seated producer wins even when
/// it sits in a different member than the card: an agent that moved seats still
/// receives replies to prose it wrote from the old one.
///
/// Prose outlives the seat that produced it, so the author is not always seated.
/// That case resolves to the lane the card is rendered in rather than to a
/// member drawn out of the team: quoting a departed agent composes into the
/// lane the operator is already looking at, and no unrelated agent is handed
/// direction it never earned. One occupant seated in two members at once is not
/// a state the resident reports; were it ever reported the answer stays the
/// card's own lane, so this decision reads the same under every ordering of
/// `members` and nothing in it depends on which member sorts first.
pub(crate) fn quote_lane(members: &[LaneMember], message: &AgentMessage) -> CanonicalUuid {
    let mut seated = members
        .iter()
        .filter(|member| member.occupant == Some(message.author));
    match (seated.next(), seated.next()) {
        (Some(producer), None) => producer.lane,
        _ => message.lane,
    }
}

fn sort_quotes(quotes: &mut [QuoteDraft]) {
    quotes.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.identity.cmp(&right.identity))
    });
}

fn compose_for(draft: &ComposerDraft, placement: MediaPlacement) -> String {
    let body = match (draft.body.trim(), draft.continuation.trim()) {
        (body, "") => Cow::Borrowed(body),
        ("", continuation) => Cow::Borrowed(continuation),
        (body, continuation) => Cow::Owned(format!("{body}\n\n{continuation}")),
    };
    let quotes = draft
        .quotes
        .iter()
        .map(|quote| QuotedReply {
            target: &quote.identity,
            body: &quote.body,
            attachments: &quote.attachments,
        })
        .collect::<Vec<_>>();
    match placement {
        MediaPlacement::Markdown => {
            crate::composer_markdown::compose(&body, &quotes, &draft.attachments)
        }
        MediaPlacement::Artifacts => {
            crate::composer_markdown::compose_without_media(&body, &quotes, &draft.attachments)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longer_context_survives_reload_and_is_sent_once() {
        for (header, expected) in [
            ("", "Follow the detailed instructions"),
            (
                "Short request",
                "Short request\n\nFollow the detailed instructions",
            ),
        ] {
            let live = vec![
                LiveText {
                    target: NEW_DRAFT_KEY.to_owned(),
                    quote: None,
                    continuation: false,
                    text: header.to_owned(),
                },
                LiveText {
                    target: NEW_DRAFT_KEY.to_owned(),
                    quote: None,
                    continuation: true,
                    text: "Follow the detailed instructions".to_owned(),
                },
            ];
            let saved = DraftStore::default().hand_off(live);
            let json = serde_json::to_string(&saved).unwrap();
            let mut restored = DraftStore::adopt(serde_json::from_str(&json).unwrap());
            let message = restored.prepare(NEW_DRAFT_KEY).unwrap().unwrap();
            let broadcast = restored.prepare_primary(NEW_DRAFT_KEY).unwrap().unwrap();
            assert_eq!(message.message, expected);
            assert_eq!(broadcast.message, expected);
            assert_eq!(
                message.snapshot.continuation,
                "Follow the detailed instructions"
            );
            restored.clear_sent(NEW_DRAFT_KEY, &broadcast.snapshot);
            assert!(restored.get(NEW_DRAFT_KEY).is_none());
        }
    }

    const PNG: &str = "iVBORw0KGgo=";

    fn attachment(name: &str) -> DraftImage {
        DraftImage::new(
            0,
            Media::from_base64(name, "image/png", PNG).expect("fixture attachment admits"),
        )
    }

    fn target(key: &str) -> QuoteTarget {
        QuoteTarget::Steering {
            key: key.to_owned(),
            digest: QuoteTarget::text_digest(key),
        }
    }

    fn identity(key: &str) -> String {
        target(key).to_string()
    }

    fn source_id(key: &str) -> String {
        format!("message-{key}")
    }

    #[test]
    fn target_keys_preserve_one_draft_per_member_across_reordering() {
        let mut store = DraftStore::default();
        store.set_body("agent-a".to_owned(), "draft a".to_owned());
        store.set_body("agent-b".to_owned(), "draft b".to_owned());

        let rerendered = ["agent-b", "agent-a"];
        assert_eq!(store.get(rerendered[0]).unwrap().body, "draft b");
        assert_eq!(store.get(rerendered[1]).unwrap().body, "draft a");
    }

    #[test]
    fn a_handoff_carries_exactly_what_each_composer_would_send() {
        let mut store = DraftStore::default();
        store.add_quote(
            "agent-a".to_owned(),
            target("source"),
            source_id("source"),
            7,
            1_000,
            "quoted prose".to_owned(),
        );
        let draft = store.draft_mut("agent-a".to_owned());
        draft.attachments.push(attachment("direct.png"));
        draft.quotes[0].attachments.push(attachment("quoted.png"));
        store.set_body("agent-b".to_owned(), "drawn body".to_owned());
        store.set_body("agent-c".to_owned(), "sent".to_owned());
        store.clear("agent-c");
        let live = |target: &str, quote: Option<String>, text: &str| LiveText {
            target: target.to_owned(),
            quote,
            continuation: false,
            text: text.to_owned(),
        };
        let handed = store.hand_off(vec![
            live("agent-a", None, "typed beside the quote"),
            live("agent-a", Some(identity("source")), "typed reply"),
            live("agent-b", None, "typed over the drawn body"),
            live("agent-d", None, "typed only in its element"),
            live("agent-e", None, ""),
        ]);

        let mut typed = store.clone();
        typed.set_body("agent-a".to_owned(), "typed beside the quote".to_owned());
        typed.set_quote_body(
            "agent-a".to_owned(),
            &identity("source"),
            "typed reply".to_owned(),
        );
        typed.set_body("agent-b".to_owned(), "typed over the drawn body".to_owned());
        typed.set_body("agent-d".to_owned(), "typed only in its element".to_owned());
        let document = serde_json::to_string(&DraftHandoff {
            composers: handed,
            notes: Vec::new(),
        })
        .expect("handoff serializes");
        let adopted = DraftStore::adopt(
            serde_json::from_str::<DraftHandoff>(&document)
                .expect("the next document reads the handoff")
                .composers,
        );
        let sent = |store: &DraftStore, target: &str| {
            store
                .prepare(target)
                .expect("fixture media admits")
                .map(|prepared| (prepared.message, prepared.attachments))
        };
        for target in ["agent-a", "agent-b", "agent-d"] {
            assert!(sent(&typed, target).is_some());
            assert_eq!(sent(&adopted, target), sent(&typed, target), "{target}");
        }
        assert!(
            adopted.get("agent-c").is_none() && adopted.get("agent-e").is_none(),
            "an empty composer hands off nothing"
        );
        let adopted_a = adopted.get("agent-a").expect("quoted draft adopted");
        assert_ne!(
            adopted_a.attachments[0].id, adopted_a.quotes[0].attachments[0].id,
            "each adopted image stays individually addressable"
        );
    }

    #[test]
    fn a_composer_still_as_drawn_saved_or_redrawn_is_no_unsent_work() {
        let key = "spice-draft:dWgYktsvQ";
        let live = |text: &str| {
            vec![LiveText {
                target: key.to_owned(),
                quote: None,
                continuation: false,
                text: text.to_owned(),
            }]
        };
        let mut store = DraftStore::default();
        let image = (*attachment("admitted.png").media).clone();
        store.hydrate(key.to_owned(), "admitted".to_owned(), vec![image]);
        assert!(store.hand_off(live("admitted")).is_empty(), "as drawn");
        assert_eq!(store.hand_off(live("edited")).len(), 1, "an unsaved edit");
        let mut removed = store.clone();
        removed.draft_mut(key.to_owned()).attachments.clear();
        assert_eq!(
            removed.hand_off(live("admitted")).len(),
            1,
            "a removed image"
        );

        store.set_body(key.to_owned(), "newer revision".to_owned());
        assert!(
            store.hand_off(live("newer revision")).is_empty(),
            "as redrawn"
        );
        let mut saved = store.get(key).cloned().expect("drawn draft");
        saved.body = "saved".to_owned();
        store.saved(key.to_owned(), saved);
        assert!(store.hand_off(live("saved")).is_empty(), "as last saved");
        assert_eq!(store.hand_off(live("saved, then typed")).len(), 1);

        store.clear(key);
        store.set_body(key.to_owned(), "saved".to_owned());
        assert_eq!(
            store.hand_off(Vec::new()).len(),
            1,
            "clearing forgets what the composer was drawn as"
        );
    }

    const LANE_A: &str = "11111111-1111-4111-8111-111111111111";
    const LANE_B: &str = "22222222-2222-4222-8222-222222222222";
    const SEATED_A: &str = "33333333-3333-4333-8333-333333333333";
    const SEATED_B: &str = "44444444-4444-4444-8444-444444444444";
    const DEPARTED: &str = "55555555-5555-4555-8555-555555555555";

    fn uuid(text: &str) -> CanonicalUuid {
        text.parse().expect("the fixture spells a canonical uuid")
    }

    fn seat(lane: &str, occupant: Option<&str>) -> LaneMember {
        LaneMember {
            lane: uuid(lane),
            name: None,
            worktree: format!("worktree-{lane}"),
            speech: wire::SpeechPlayback::Disabled {},
            life: "steer".to_owned(),
            vendor: "omp".parse().expect("the fixture spells a vendor name"),
            model: Some("sonnet".to_owned()),
            seat_model: None,
            effort: Some("high".to_owned()),
            seat_effort: None,
            fast_mode: false,
            fast_refusal: None,
            seat_fast_mode: false,
            occupant: occupant.map(uuid),
            session: Some("session-fixture".to_owned()),
            disposition: "seated".to_owned(),
            liveness: wire::intent::LaneLiveness::Working,
            blocked_on: None,
            bound: true,
            wakeable: true,
            jobs: Vec::new(),
            parked: None,
        }
    }

    fn message(lane: &str, author: &str) -> AgentMessage {
        AgentMessage {
            prose_origin: None,
            lane: uuid(lane),
            author: uuid(author),
            locus: wire::stream::MessageLocus {
                member: 1,
                offset: 0,
                length: 8,
                event: 0,
            },
            authored: "2030-04-12T18:44:09.271Z".parse().expect("fixture instant"),
            authored_synthetic: false,
            observed: None,
            activity: wire::AgentActivityKind::Prose,
            compaction_method: None,
            final_answer: false,
            acknowledgement: None,
            text: "quoted prose".to_owned(),
            provider_error: None,
            quote_target: None,
            directive_contexts: Vec::new(),
            sends: Vec::new(),
            quotes: Vec::new(),
            automatic_speech: None,
            media: Vec::new(),
            media_quotes: Vec::new(),
            markdown_images: Vec::new(),
            context_bytes: 0,
        }
    }

    /// A seated author receives the quote even from another member's card.
    #[test]
    fn a_seated_author_receives_the_quote_from_whichever_card_carries_it() {
        let members = vec![seat(LANE_A, Some(SEATED_A)), seat(LANE_B, Some(SEATED_B))];
        // The card sits in lane A while the prose was written by the occupant
        // that now sits in lane B, which is the seat-change case the preference
        // exists for: the reply follows the agent, not the tile.
        assert_eq!(
            quote_lane(&members, &message(LANE_A, SEATED_B)),
            uuid(LANE_B),
            "a seated author elsewhere in the team still receives its replies"
        );
        assert_eq!(
            quote_lane(&members, &message(LANE_B, SEATED_B)),
            uuid(LANE_B),
            "the ordinary case targets the author's own seat"
        );
    }

    /// An unseated author's quote stays in the lane whose card carries it.
    ///
    /// The first member is deliberately vacant and the card is deliberately the
    /// second one: with a single member, or with the card in the first member,
    /// every candidate rule returns the same lane and the fixture would pass
    /// against the row-order pick this replaced. Both orderings are asserted
    /// because a rule that reads member order can only be caught by permuting
    /// the members it reads.
    #[test]
    fn an_unseated_author_composes_into_the_card_lane_whatever_the_member_order() {
        let vacant_first = vec![seat(LANE_A, None), seat(LANE_B, Some(SEATED_B))];
        let vacant_last = vec![seat(LANE_B, Some(SEATED_B)), seat(LANE_A, None)];
        for members in [vacant_first, vacant_last] {
            assert_eq!(
                quote_lane(&members, &message(LANE_B, DEPARTED)),
                uuid(LANE_B),
                "a departed author's quote belongs to the lane being looked at"
            );
            assert_eq!(
                quote_lane(&members, &message(LANE_A, DEPARTED)),
                uuid(LANE_A),
                "including when that lane is the vacant one"
            );
        }
    }

    /// One occupant claimed by two seats resolves without reading their order.
    #[test]
    fn an_ambiguous_author_resolves_to_the_card_lane_rather_than_a_seat() {
        let duplicated = vec![seat(LANE_A, Some(SEATED_B)), seat(LANE_B, Some(SEATED_B))];
        let reversed = vec![seat(LANE_B, Some(SEATED_B)), seat(LANE_A, Some(SEATED_B))];
        for members in [duplicated, reversed] {
            assert_eq!(
                quote_lane(&members, &message(LANE_A, SEATED_B)),
                uuid(LANE_A),
                "an unrepresentable team still answers the card's own lane"
            );
        }
    }

    #[test]
    fn quote_identity_deduplicates_without_conflating_equal_text_and_sorts_oldest_first() {
        let mut store = DraftStore::default();
        assert_eq!(
            store.add_quote(
                "agent-a".to_owned(),
                target("message-old"),
                source_id("message-old"),
                10,
                1_000,
                "Old quote".to_owned(),
            ),
            QuotePlacement::Added
        );
        assert_eq!(
            store.add_quote(
                "agent-a".to_owned(),
                target("message-new"),
                source_id("message-new"),
                30,
                3_000,
                "New quote".to_owned(),
            ),
            QuotePlacement::Added
        );
        assert_eq!(
            store.add_quote(
                "agent-a".to_owned(),
                target("message-old"),
                source_id("message-old"),
                10,
                1_000,
                "Changed preview is ignored".to_owned(),
            ),
            QuotePlacement::Existing
        );

        let identities = store
            .get("agent-a")
            .unwrap()
            .quotes
            .iter()
            .map(|quote| quote.identity.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            identities,
            [identity("message-old"), identity("message-new")]
        );
        assert_eq!(
            store
                .get("agent-a")
                .unwrap()
                .quotes
                .iter()
                .map(|quote| quote.source_id.as_str())
                .collect::<Vec<_>>(),
            ["message-message-old", "message-message-new"]
        );

        store.remove_quote("agent-a".to_owned(), &identity("message-old"));
        assert_eq!(store.get("agent-a").unwrap().quotes.len(), 1);
    }

    #[test]
    fn preparation_keeps_images_with_their_text_blocks_and_target() {
        let mut store = DraftStore::default();
        let quote_attachment = attachment("quote.png");
        let primary_attachment = attachment(r"a](one)\雪.png");
        store.set_body("agent-a".to_owned(), "body".to_owned());
        store.add_quote(
            "agent-a".to_owned(),
            target("message"),
            source_id("message"),
            1,
            1_000,
            "Quoted source".to_owned(),
        );
        store.set_quote_body(
            "agent-a".to_owned(),
            &identity("message"),
            "reply".to_owned(),
        );
        store
            .draft_mut("agent-a".to_owned())
            .quotes
            .first_mut()
            .expect("the quote exists")
            .attachments
            .push(quote_attachment.clone());
        store
            .draft_mut("agent-a".to_owned())
            .attachments
            .push(primary_attachment.clone());
        store
            .draft_mut("agent-b".to_owned())
            .attachments
            .push(attachment("b"));

        let prepared = store.prepare("agent-a").unwrap().unwrap();
        assert!(
            prepared
                .attachments
                .iter()
                .all(|media| !prepared.message.contains(&media.data_url)),
            "directive prose must not duplicate its attachment payloads",
        );
        assert_eq!(
            prepared.message,
            format!(
                "{}\n\nreply\n\n{}\n\nbody\n\n{}",
                target("message").markdown(),
                quote_attachment
                    .media
                    .reference_markdown()
                    .expect("fixture Markdown renders"),
                primary_attachment
                    .media
                    .reference_markdown()
                    .expect("fixture Markdown renders"),
            ),
        );
        assert_eq!(
            prepared.attachments,
            [
                (*quote_attachment.media).clone(),
                (*primary_attachment.media).clone()
            ]
        );
        let artifact_prepared =
            prepare_submission(prepared.snapshot, MediaPlacement::Artifacts).unwrap();
        assert_eq!(
            artifact_prepared.message,
            format!("{}\n\nreply\n\nbody", target("message").markdown())
        );
        assert_eq!(
            artifact_prepared.attachments,
            [
                (*quote_attachment.media).clone(),
                (*primary_attachment.media).clone()
            ]
        );
    }

    #[test]
    fn clearing_a_send_removes_exactly_its_content_from_only_its_target() {
        let mut store = DraftStore::default();
        store.set_body("agent-a".to_owned(), "sent".to_owned());
        store.set_body("agent-b".to_owned(), "untouched".to_owned());
        store
            .draft_mut("agent-a".to_owned())
            .attachments
            .push(attachment("sent"));
        store.add_quote(
            "agent-a".to_owned(),
            target("sent-quote"),
            source_id("sent-quote"),
            1,
            1_000,
            "Sent quote".to_owned(),
        );
        store.set_quote_body(
            "agent-a".to_owned(),
            &identity("sent-quote"),
            "sent reply".to_owned(),
        );
        let snapshot = store.prepare("agent-a").unwrap().unwrap().snapshot;

        // Drafted beside the send while it awaited acceptance.
        let mut newer = attachment("newer");
        newer.id = 99;
        store
            .draft_mut("agent-a".to_owned())
            .attachments
            .push(newer);
        store.add_quote(
            "agent-a".to_owned(),
            target("newer-quote"),
            source_id("newer-quote"),
            3,
            3_000,
            "Newer quote".to_owned(),
        );
        store.clear_sent("agent-a", &snapshot);

        let remaining = store.get("agent-a").unwrap();
        assert_eq!(remaining.body, "");
        assert_eq!(
            remaining
                .attachments
                .iter()
                .map(|item| item.media.name.as_str())
                .collect::<Vec<_>>(),
            ["newer"]
        );
        assert_eq!(
            remaining
                .quotes
                .iter()
                .map(|quote| quote.identity.as_str())
                .collect::<Vec<_>>(),
            [identity("newer-quote")]
        );
        assert_eq!(store.get("agent-b").unwrap().body, "untouched");

        store.set_body("agent-b".to_owned(), "only".to_owned());
        let only = store.prepare("agent-b").unwrap().unwrap().snapshot;
        store.clear_sent("agent-b", &only);
        assert!(store.get("agent-b").is_none(), "a fully sent draft leaves");
    }

    #[test]
    fn broadcast_preparation_detaches_only_primary_content_and_clears_only_it() {
        let mut store = DraftStore::default();
        store.set_body("agent-a".to_owned(), "broadcast body".to_owned());
        store
            .draft_mut("agent-a".to_owned())
            .attachments
            .push(attachment("primary"));
        store.add_quote(
            "agent-a".to_owned(),
            target("quote"),
            source_id("quote"),
            1,
            1_000,
            "Quoted source".to_owned(),
        );
        store.set_quote_body(
            "agent-a".to_owned(),
            &identity("quote"),
            "local reply".to_owned(),
        );
        store
            .draft_mut("agent-a".to_owned())
            .quotes
            .first_mut()
            .expect("the quote exists")
            .attachments
            .push(attachment("quoted"));

        let prepared = store
            .prepare_primary("agent-a")
            .expect("primary media admits")
            .expect("primary body prepares");
        assert!(prepared.message.starts_with("broadcast body"));
        assert!(prepared.message.contains("![primary]"));
        assert!(!prepared.message.contains("quoted source"));
        assert!(!prepared.message.contains("local reply"));
        assert_eq!(prepared.attachments.len(), 1);

        store.clear_sent("agent-a", &prepared.snapshot);
        let remaining = store.get("agent-a").expect("quote draft remains");
        assert!(remaining.body.is_empty());
        assert!(remaining.attachments.is_empty());
        assert_eq!(remaining.quotes.len(), 1);
        assert_eq!(remaining.quotes[0].body, "local reply");
        assert_eq!(remaining.quotes[0].attachments.len(), 1);
    }

    #[test]
    fn drafts_past_the_media_bound_refuse_preparation_and_keep_the_draft() {
        let mut store = DraftStore::default();
        let typed = store.draft_mut("lane".to_owned());
        typed.body = "typed".to_owned();
        typed.attachments = (0..=wire::media::MAX_MEDIA_ITEMS)
            .map(|index| attachment(&format!("image-{index}")))
            .collect();
        let restored = store.get("lane").expect("typed draft").clone();

        let over = wire::media::MediaError::Count {
            observed: wire::media::MAX_MEDIA_ITEMS + 1,
            maximum: wire::media::MAX_MEDIA_ITEMS,
        };
        assert_eq!(store.prepare("lane").err(), Some(over.clone()));
        assert_eq!(store.prepare_primary("lane").err(), Some(over));
        assert_eq!(store.get("lane"), Some(&restored));

        let draft = store.draft_mut("lane".to_owned());
        draft.attachments.pop();
        let prepared = store
            .prepare("lane")
            .expect("the bound admits once one image leaves")
            .expect("the draft prepares");
        assert_eq!(prepared.attachments.len(), wire::media::MAX_MEDIA_ITEMS);
    }

    #[test]
    fn handoff_carries_every_authored_draft() {
        let mut store = DraftStore::default();
        assert!(store.hand_off(Vec::new()).is_empty());
        store.set_body("agent-a".to_owned(), "typed after send".to_owned());
        assert_eq!(store.hand_off(Vec::new()).len(), 1);

        store.clear("agent-a");
        store.add_quote(
            "agent-a".to_owned(),
            target("source"),
            source_id("source"),
            1,
            1_000,
            "Quoted source".to_owned(),
        );
        assert_eq!(
            store.hand_off(Vec::new()).len(),
            1,
            "an untouched quote is still authored intent"
        );
    }

    #[test]
    fn detached_composer_text_is_kept_only_over_the_text_it_was_drawn_from() {
        let mut store = DraftStore::default();
        store.add_quote(
            "agent-a".to_owned(),
            target("source"),
            source_id("source"),
            1,
            1_000,
            "Quoted source".to_owned(),
        );
        store.keep_detached("agent-a".to_owned(), None, "", "typed".to_owned());
        store.keep_detached(
            "agent-a".to_owned(),
            Some(&identity("source")),
            "",
            "reply".to_owned(),
        );
        let kept = store.get("agent-a").expect("moved draft stays on its lane");
        assert_eq!(kept.body, "typed");
        assert_eq!(kept.quotes[0].body, "reply");

        store.set_body("agent-a".to_owned(), "replaced".to_owned());
        store.keep_detached("agent-a".to_owned(), None, "typed", "stale".to_owned());
        assert_eq!(
            store.get("agent-a").unwrap().body,
            "replaced",
            "an element drawn from replaced text does not overwrite it"
        );

        store.remove_quote("agent-a".to_owned(), &identity("source"));
        store.keep_detached(
            "agent-a".to_owned(),
            Some(&identity("source")),
            "reply",
            "reply and more".to_owned(),
        );
        assert!(store.get("agent-a").unwrap().quotes.is_empty());
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;
    use crate::image_annotation::{Color, Kind, Point, Route};

    #[test]
    fn edits_own_one_paste_and_revision_and_end_with_their_image() {
        let mut store = DraftStore::default();
        let original = Media::from_base64("original.png", "image/png", "iVBORw0KGgo=").unwrap();
        let marked = Media::from_base64("original.png", "image/png", "iVBORw0KGgoAAAAA").unwrap();
        let first = store.issue_image();
        let second = store.issue_image();
        store.draft_mut("lane".into()).attachments = vec![
            DraftImage::new(first, original.clone()),
            DraftImage::new(second, original.clone()),
        ];
        let shapes = Rc::new(vec![Shape::new(
            Kind::Rectangle,
            Route::Straight,
            Color::Red,
            Point { x: 2, y: 3 },
            Point { x: 20, y: 30 },
        )]);
        assert!(store.apply_image("lane", None, second, 0, marked.clone(), shapes.clone()));
        assert_eq!(*store.image("lane", None, first).unwrap().media, original);
        assert_eq!(
            *store.image("lane", None, second).unwrap().original,
            original
        );
        assert!(!store.apply_image("lane", None, second, 0, original.clone(), Rc::default()));
        store.set_body("lane".into(), "send".into());
        let sent = store.prepare("lane").unwrap().unwrap();
        assert_eq!(sent.attachments, [original.clone(), marked.clone()]);
        store.clear("lane");
        assert!(!store.apply_image("lane", None, second, 1, original.clone(), Rc::default()));
    }
}

#[cfg(test)]
mod quote_image_tests {
    use super::*;

    #[test]
    fn image_application_requires_the_owning_quote_and_live_attachment() {
        let mut store = DraftStore::default();
        let media = Media::from_base64("image.png", "image/png", "iVBORw0KGgo=").unwrap();
        let marked = Media::from_base64("image.png", "image/png", "iVBORw0KGgoAAAAA").unwrap();
        let target = QuoteTarget::Steering {
            key: "quoted".into(),
            digest: QuoteTarget::text_digest("source"),
        };
        let identity = target.to_string();
        store.add_quote(
            "lane".into(),
            target,
            "source".into(),
            1,
            1,
            "source".into(),
        );
        let id = store.issue_image();
        store.draft_mut("lane".into()).quotes[0]
            .attachments
            .push(DraftImage::new(id, media.clone()));
        assert!(!store.apply_image("lane", None, id, 0, marked.clone(), Rc::default()));
        assert!(!store.apply_image(
            "other-lane",
            Some(&identity),
            id,
            0,
            marked.clone(),
            Rc::default()
        ));
        assert_eq!(
            *store.image("lane", Some(&identity), id).unwrap().media,
            media
        );
        assert!(store.apply_image(
            "lane",
            Some(&identity),
            id,
            0,
            marked.clone(),
            Rc::default()
        ));
        assert_eq!(
            *store.image("lane", Some(&identity), id).unwrap().media,
            marked
        );
        store.remove_quote("lane".into(), &identity);
        assert!(!store.apply_image("lane", Some(&identity), id, 1, media, Rc::default()));
    }
}
