//! Bounded evidence one conversation offers to open Design matching.
//!
//! A session's relevant knowledge may sit in any task it finished or in what
//! it said in ordinary conversation, so a session is matched through every
//! retained piece of evidence rather than through one completion document.
//! Each [`Evidence`] card keeps canonical term counts for a few fixed
//! [`View`]s; an [`EvidenceProfile`] scores a Design as the maximum cosine over
//! all views of all cards, under one shared frequency fit, and names the
//! winning card and view.
//!
//! Conversation evidence is authored prose only. Placed protocol headers are
//! cut by the steering grammar, and block quotes, code blocks, raw HTML blocks,
//! and `spice://` quote links are hidden by Markdown structure, so quoted or
//! pasted material never speaks for the author. Inline code stays: an
//! identifier the author names is part of what they wrote. Callers pass only
//! the session's own settled assistant prose; operator and peer turns never
//! become evidence.

use std::cmp::Ordering;
use std::ops::Range;

use board::similarity::{
    FrequencyProfile, Generation, GenerationMismatch, Prepared, TermCounts, utf8_prefix,
};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use wire::TaskStamp;

/// Most UTF-8 bytes of cleaned prose one conversation card retains.
pub const CONVERSATION_BYTES: usize = 4 * 1024;
/// Fewest distinct terms a paragraph or list item needs to be its own view.
pub const PASSAGE_TERMS: usize = 12;
/// Most passage views one conversation card keeps, in document order.
pub const MESSAGE_PASSAGES: usize = 4;
/// Most distinct successful task cards one lane profile retains.
pub const TASK_CARDS: usize = 32;
/// Most distinct conversation cards one lane profile retains.
pub const CONVERSATION_CARDS: usize = 64;
/// Fewest distinct terms cleaned prose needs before it is evidence at all.
///
/// A short substantive note can be the only place a session explained a
/// subject, and the audit's pilot 24-term rule excluded two such useful notes,
/// so admission uses the same bound as a passage: a message that could not be
/// one coherent passage carries too little to match on. Acknowledgement-only
/// replies fall below it once their headers are cut.
pub const MESSAGE_TERMS: usize = PASSAGE_TERMS;

/// Which rendering of one card a score was computed against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum View {
    /// A task's title, description, and terms.
    Statement,
    /// A task's statement plus its bounded successful validation.
    Completion,
    /// One whole cleaned message.
    Message,
    /// One paragraph or list item of a message, by retained position.
    Passage(u8),
}

/// Retention class of one evidence card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A successful task completion.
    Task,
    /// One settled assistant-authored message.
    Conversation,
}

impl Kind {
    /// Most distinct cards of this class one profile retains.
    #[must_use]
    pub const fn limit(self) -> usize {
        match self {
            Self::Task => TASK_CARDS,
            Self::Conversation => CONVERSATION_CARDS,
        }
    }
}

/// Content identity of one card; equal digests are exact repeats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest([u8; 32]);

impl Digest {
    /// Lowercase hexadecimal spelling suitable for a durable source witness.
    #[must_use]
    pub fn to_hex(self) -> String {
        blake3::Hash::from_bytes(self.0).to_hex().to_string()
    }

    /// Raw BLAKE3 output for binding this process-local digest into a durable
    /// domain-separated identity.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Settled authored prose reduced to what its author wrote, within
/// [`CONVERSATION_BYTES`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    text: String,
    passages: Vec<Range<usize>>,
}

impl Conversation {
    /// Clean and bound one settled assistant message. `None` when what
    /// remains has fewer than [`MESSAGE_TERMS`] distinct terms.
    #[must_use]
    pub fn read(prose: &str) -> Option<Self> {
        let mut text = String::new();
        let mut blocks = Vec::new();
        for block in authored_blocks(prose) {
            let separator = if text.is_empty() { "" } else { "\n\n" };
            let start = text.len() + separator.len();
            if start >= CONVERSATION_BYTES {
                break;
            }
            let kept = utf8_prefix(&block, CONVERSATION_BYTES - start);
            if kept.is_empty() {
                break;
            }
            text.push_str(separator);
            text.push_str(kept);
            blocks.push(start..text.len());
            if kept.len() < block.len() {
                break;
            }
        }
        if TermCounts::from_texts([text.as_str()]).distinct() < MESSAGE_TERMS {
            return None;
        }
        let passages = if blocks.len() > 1 {
            blocks
                .into_iter()
                .filter(|block| {
                    TermCounts::from_texts([&text[block.clone()]]).distinct() >= PASSAGE_TERMS
                })
                .take(MESSAGE_PASSAGES)
                .collect()
        } else {
            Vec::new()
        };
        Some(Self { text, passages })
    }

    /// The cleaned, bounded prose.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The retained paragraph and list-item views, in document order.
    pub fn passages(&self) -> impl Iterator<Item = &str> {
        self.passages.iter().map(|range| &self.text[range.clone()])
    }
}

/// One retained piece of a session's evidence and its canonical views.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence<K> {
    key: K,
    kind: Kind,
    digest: Digest,
    excerpt: String,
    views: Vec<(View, TermCounts)>,
}

impl<K> Evidence<K> {
    /// A finished task's statement and statement-plus-validation views,
    /// excerpting its validation, or its title when the validation is blank.
    #[must_use]
    pub fn task(
        key: K,
        title: &str,
        pitch: &str,
        description: Option<&str>,
        term: &[String],
        validation: &str,
    ) -> Self {
        let validation = utf8_prefix(validation, wire::rooms::COMPLETION_VALIDATION_BYTES);
        let mut hasher = digest_kind(b"task");
        digest_field(&mut hasher, title.as_bytes());
        digest_field(&mut hasher, pitch.as_bytes());
        digest_field(&mut hasher, description.unwrap_or_default().as_bytes());
        hasher.update(&(u64::from(description.is_some())).to_le_bytes());
        hasher.update(&(term.len() as u64).to_le_bytes());
        for item in term {
            digest_field(&mut hasher, item.as_bytes());
        }
        digest_field(&mut hasher, validation.as_bytes());
        let excerpt = match wire::rooms::design_excerpt(validation) {
            excerpt if excerpt.is_empty() => wire::rooms::design_excerpt(title),
            excerpt => excerpt,
        };
        Self {
            key,
            kind: Kind::Task,
            digest: Digest(*hasher.finalize().as_bytes()),
            excerpt,
            views: vec![
                (
                    View::Statement,
                    TermCounts::statement(title, pitch, description, term),
                ),
                (
                    View::Completion,
                    TermCounts::completion(title, pitch, description, term, validation),
                ),
            ],
        }
    }

    /// One admitted message's whole and passage views.
    #[must_use]
    pub fn conversation(key: K, conversation: &Conversation) -> Self {
        let mut hasher = digest_kind(b"conversation");
        digest_field(&mut hasher, conversation.text.as_bytes());
        let views = std::iter::once((View::Message, TermCounts::from_texts([conversation.text()])))
            .chain(conversation.passages().enumerate().map(|(index, passage)| {
                let index = u8::try_from(index).expect("passages are bounded by MESSAGE_PASSAGES");
                (View::Passage(index), TermCounts::from_texts([passage]))
            }))
            .collect();
        Self {
            key,
            kind: Kind::Conversation,
            digest: Digest(*hasher.finalize().as_bytes()),
            excerpt: wire::rooms::design_excerpt(conversation.text()),
            views,
        }
    }

    /// The caller's identity for this card.
    #[must_use]
    pub fn key(&self) -> &K {
        &self.key
    }

    /// Replace only the caller-owned identity of this card.
    #[must_use]
    pub fn map_key<L>(self, map: impl FnOnce(K) -> L) -> Evidence<L> {
        Evidence {
            key: map(self.key),
            kind: self.kind,
            digest: self.digest,
            excerpt: self.excerpt,
            views: self.views,
        }
    }

    /// The card's content identity.
    #[must_use]
    pub fn digest(&self) -> Digest {
        self.digest
    }

    /// The card's authored text as one bounded line.
    #[must_use]
    pub fn excerpt(&self) -> &str {
        &self.excerpt
    }

    /// The retention class of this card.
    #[must_use]
    pub const fn kind(&self) -> Kind {
        self.kind
    }
}

/// One open Design, weighted under the fit and floor it is scored against.
pub struct DesignCandidate<'a, T> {
    identity: &'a TaskStamp,
    value: &'a T,
    vector: &'a Prepared,
    score_floor: f64,
}

impl<'a, T> DesignCandidate<'a, T> {
    #[must_use]
    pub fn new(
        identity: &'a TaskStamp,
        value: &'a T,
        vector: &'a Prepared,
        score_floor: f64,
    ) -> Self {
        Self {
            identity,
            value,
            vector,
            score_floor,
        }
    }
}

/// The Design a profile is most relevant to and the evidence that showed it.
#[derive(Debug, PartialEq)]
pub struct ProfileMatch<'p, 'a, K, T> {
    /// The selected Design.
    pub design: &'a T,
    /// The card whose view scored highest.
    pub evidence: &'p K,
    /// The winning view of that card.
    pub view: View,
    /// Content identity of the winning card.
    pub digest: Digest,
    /// The winning card's bounded authored excerpt.
    pub excerpt: &'p str,
    /// The winning cosine, at least that Design candidate's floor.
    pub score: f64,
    /// The strongest shared terms, sorted.
    pub terms: Vec<String>,
}

/// Strongest retained evidence view for one already-weighted candidate
/// document, without changing the profile's derived-vector cache.
#[derive(Debug, PartialEq)]
pub struct EvidenceMatch<'p, K> {
    /// Winning retained card identity.
    pub evidence: &'p K,
    /// Winning canonical card view.
    pub view: View,
    /// Content identity of the winning card.
    pub digest: Digest,
    /// Winning card's bounded authored excerpt.
    pub excerpt: &'p str,
    /// Strongest finite cosine.
    pub score: f64,
    /// Strongest shared terms, sorted by the production explanation.
    pub terms: Vec<String>,
}

/// One session's retained evidence, stored oldest first, with derived vectors
/// cached per frequency generation.
#[derive(Debug)]
pub struct EvidenceProfile<K> {
    cards: Vec<Card<K>>,
}

#[derive(Debug)]
struct Card<K> {
    evidence: Evidence<K>,
    prepared: Option<(Generation, Vec<Option<Prepared>>)>,
}

impl<K> Default for EvidenceProfile<K> {
    fn default() -> Self {
        Self { cards: Vec::new() }
    }
}

impl<K> EvidenceProfile<K> {
    /// Retain `evidence` as the newest card. An exact repeat of a retained
    /// card is refused and returned, leaving the profile unchanged.
    ///
    /// # Errors
    ///
    /// Returns the refused card when its digest is already retained.
    pub fn insert(&mut self, evidence: Evidence<K>) -> Result<(), Evidence<K>> {
        if self.retains(evidence.digest) {
            return Err(evidence);
        }
        let kind = evidence.kind;
        self.cards.push(Card {
            evidence,
            prepared: None,
        });
        if self.count(kind) > kind.limit() {
            let oldest = self
                .cards
                .iter()
                .position(|card| card.evidence.kind == kind)
                .expect("the overfull evidence class has an oldest card");
            self.cards.remove(oldest);
        }
        Ok(())
    }

    /// Retain `evidence` as older than every retained card of its kind, as a
    /// newest-first history walk finds it.
    ///
    /// # Errors
    ///
    /// Returns the refused card when its digest is already retained or its kind
    /// is [full](Self::is_full): older history can no longer displace any
    /// retained card.
    pub fn insert_older(&mut self, evidence: Evidence<K>) -> Result<(), Evidence<K>> {
        if self.is_full(evidence.kind) || self.retains(evidence.digest) {
            return Err(evidence);
        }
        self.cards.insert(
            0,
            Card {
                evidence,
                prepared: None,
            },
        );
        Ok(())
    }

    /// Whether this profile already retains its limit of `kind` cards.
    #[must_use]
    pub fn is_full(&self, kind: Kind) -> bool {
        self.count(kind) >= kind.limit()
    }

    fn retains(&self, digest: Digest) -> bool {
        self.cards.iter().any(|card| card.evidence.digest == digest)
    }

    fn count(&self, kind: Kind) -> usize {
        self.cards
            .iter()
            .filter(|card| card.evidence.kind == kind)
            .count()
    }

    /// Retained cards, newest first.
    pub fn evidence(&self) -> impl DoubleEndedIterator<Item = &Evidence<K>> {
        self.cards.iter().rev().map(|card| &card.evidence)
    }

    /// Forget one exact retained card whose owning source is no longer
    /// current. Returns whether the card was present.
    pub fn remove(&mut self, digest: Digest) -> bool {
        let Some(index) = self
            .cards
            .iter()
            .position(|card| card.evidence.digest == digest)
        else {
            return false;
        };
        self.cards.remove(index);
        true
    }

    /// The Design this profile is most relevant to under `frequency`.
    ///
    /// A Design's relevance is the maximum cosine over every view of every
    /// card; each Design qualifies against its candidate-owned floor. Ties
    /// prefer the lower Design identity, then the newer card, then the earlier
    /// view, so the same profile and candidates always select the same match.
    ///
    /// # Errors
    ///
    /// Refuses a candidate vector weighted under a generation other than
    /// `frequency`'s.
    pub fn select<'p, 'a, T>(
        &'p mut self,
        frequency: &FrequencyProfile,
        candidates: impl IntoIterator<Item = DesignCandidate<'a, T>>,
    ) -> Result<Option<ProfileMatch<'p, 'a, K, T>>, GenerationMismatch> {
        self.select_where(frequency, candidates, |_, _| true)
    }

    /// [`Self::select`] over only the retained cards that `include` admits
    /// for the candidate Design identity.
    ///
    /// # Errors
    ///
    /// Refuses a candidate vector weighted under a generation other than
    /// `frequency`'s.
    pub fn select_where<'p, 'a, T>(
        &'p mut self,
        frequency: &FrequencyProfile,
        candidates: impl IntoIterator<Item = DesignCandidate<'a, T>>,
        mut include: impl FnMut(&Evidence<K>, &TaskStamp) -> bool,
    ) -> Result<Option<ProfileMatch<'p, 'a, K, T>>, GenerationMismatch> {
        let generation = frequency.generation();
        for card in &mut self.cards {
            card.prepare(frequency);
        }
        let mut best: Option<Best<'a, T>> = None;
        for candidate in candidates {
            if candidate.vector.generation() != generation {
                return Err(GenerationMismatch {
                    left: candidate.vector.generation(),
                    right: generation,
                });
            }
            if !candidate.score_floor.is_finite() {
                continue;
            }
            for (card_index, card) in self.cards.iter().enumerate() {
                if !include(&card.evidence, candidate.identity) {
                    continue;
                }
                let Some((_, vectors)) = &card.prepared else {
                    continue;
                };
                for (view_index, vector) in vectors.iter().enumerate() {
                    let Some(vector) = vector else {
                        continue;
                    };
                    let score = candidate.vector.cosine(vector)?;
                    if !score.is_finite() || score < candidate.score_floor {
                        continue;
                    }
                    let found = Best {
                        identity: candidate.identity,
                        design: candidate.value,
                        vector: candidate.vector,
                        score,
                        card: card_index,
                        view: view_index,
                    };
                    if best.as_ref().is_none_or(|known| found.outranks(known)) {
                        best = Some(found);
                    }
                }
            }
        }
        let Some(best) = best else {
            return Ok(None);
        };
        let card = &self.cards[best.card];
        let (_, vectors) = card
            .prepared
            .as_ref()
            .expect("every card was prepared above");
        let vector = vectors[best.view]
            .as_ref()
            .expect("the winning view has a vector");
        Ok(Some(ProfileMatch {
            design: best.design,
            evidence: &card.evidence.key,
            view: card.evidence.views[best.view].0,
            digest: card.evidence.digest,
            excerpt: &card.evidence.excerpt,
            score: best.score,
            terms: best.vector.explanation(vector),
        }))
    }

    /// Measure one candidate vector against retained card views accepted by
    /// `include`, without changing the profile. Ties use the same newer-card,
    /// earlier-view order as [`Self::select`] for a single candidate.
    ///
    /// # Errors
    ///
    /// Refuses a candidate vector weighted under a generation other than
    /// `frequency`'s.
    pub fn measure_where<'p>(
        &'p self,
        frequency: &FrequencyProfile,
        vector: &Prepared,
        mut include: impl FnMut(&Evidence<K>) -> bool,
    ) -> Result<Option<EvidenceMatch<'p, K>>, GenerationMismatch> {
        let generation = frequency.generation();
        if vector.generation() != generation {
            return Err(GenerationMismatch {
                left: vector.generation(),
                right: generation,
            });
        }
        let mut best: Option<ViewBest> = None;
        for (card_index, card) in self.cards.iter().enumerate() {
            if !include(&card.evidence) {
                continue;
            }
            for (view_index, (_, counts)) in card.evidence.views.iter().enumerate() {
                let Some(evidence) = frequency.prepare(counts) else {
                    continue;
                };
                let score = vector.cosine(&evidence)?;
                let found = ViewBest {
                    score,
                    card: card_index,
                    view: view_index,
                };
                if score.is_finite() && best.as_ref().is_none_or(|known| found.outranks(known)) {
                    best = Some(found);
                }
            }
        }
        let Some(best) = best else {
            return Ok(None);
        };
        let card = &self.cards[best.card];
        let (view, counts) = &card.evidence.views[best.view];
        let evidence = frequency
            .prepare(counts)
            .expect("the winning view prepared during measurement");
        Ok(Some(EvidenceMatch {
            evidence: &card.evidence.key,
            view: *view,
            digest: card.evidence.digest,
            excerpt: &card.evidence.excerpt,
            score: best.score,
            terms: vector.explanation(&evidence),
        }))
    }

    /// Measure one candidate vector against every retained card view without
    /// changing the profile.
    ///
    /// # Errors
    ///
    /// Refuses a candidate vector weighted under a generation other than
    /// `frequency`'s.
    pub fn measure<'p>(
        &'p self,
        frequency: &FrequencyProfile,
        vector: &Prepared,
    ) -> Result<Option<EvidenceMatch<'p, K>>, GenerationMismatch> {
        self.measure_where(frequency, vector, |_| true)
    }

    /// The strongest finite cosine between `vector` and any view of any
    /// retained card; `None` when no view scores.
    ///
    /// # Errors
    ///
    /// Refuses a vector weighted under a generation other than
    /// `frequency`'s.
    pub fn best_score(
        &mut self,
        frequency: &FrequencyProfile,
        vector: &Prepared,
    ) -> Result<Option<f64>, GenerationMismatch> {
        self.best_score_where(frequency, vector, |_| true)
    }

    /// [`best_score`](Self::best_score) over the retained cards `keep`
    /// admits.
    ///
    /// # Errors
    ///
    /// Refuses a vector weighted under a generation other than
    /// `frequency`'s.
    pub fn best_score_where(
        &mut self,
        frequency: &FrequencyProfile,
        vector: &Prepared,
        keep: impl Fn(&Evidence<K>) -> bool,
    ) -> Result<Option<f64>, GenerationMismatch> {
        let generation = frequency.generation();
        if vector.generation() != generation {
            return Err(GenerationMismatch {
                left: vector.generation(),
                right: generation,
            });
        }
        let mut best: Option<f64> = None;
        for card in &mut self.cards {
            if !keep(&card.evidence) {
                continue;
            }
            card.prepare(frequency);
            let Some((_, views)) = &card.prepared else {
                continue;
            };
            for view in views.iter().flatten() {
                let score = vector.cosine(view)?;
                if score.is_finite() && best.is_none_or(|known| score > known) {
                    best = Some(score);
                }
            }
        }
        Ok(best)
    }

    /// Whether any retained card's key satisfies `predicate`.
    #[must_use]
    pub fn holds(&self, predicate: impl Fn(&K) -> bool) -> bool {
        self.cards.iter().any(|card| predicate(&card.evidence.key))
    }

    /// The whole-message view of every retained conversation card.
    pub fn conversation_messages(&self) -> impl Iterator<Item = &TermCounts> {
        self.cards
            .iter()
            .filter(|card| matches!(card.evidence.kind, Kind::Conversation))
            .filter_map(|card| card.evidence.views.first().map(|(_, counts)| counts))
    }
}

impl<K> Card<K> {
    fn prepare(&mut self, frequency: &FrequencyProfile) {
        let generation = frequency.generation();
        if self
            .prepared
            .as_ref()
            .is_some_and(|(prepared, _)| *prepared == generation)
        {
            return;
        }
        let vectors = self
            .evidence
            .views
            .iter()
            .map(|(_, counts)| frequency.prepare(counts))
            .collect();
        self.prepared = Some((generation, vectors));
    }
}

#[derive(Clone, Copy)]
struct ViewBest {
    score: f64,
    card: usize,
    view: usize,
}

impl ViewBest {
    fn outranks(&self, known: &Self) -> bool {
        self.score
            .total_cmp(&known.score)
            .then_with(|| self.card.cmp(&known.card))
            .then_with(|| known.view.cmp(&self.view))
            == Ordering::Greater
    }
}

struct Best<'a, T> {
    identity: &'a TaskStamp,
    design: &'a T,
    vector: &'a Prepared,
    score: f64,
    card: usize,
    view: usize,
}

impl<T> Best<'_, T> {
    fn outranks(&self, known: &Self) -> bool {
        self.score
            .total_cmp(&known.score)
            .then_with(|| known.identity.cmp(self.identity))
            .then_with(|| self.card.cmp(&known.card))
            .then_with(|| known.view.cmp(&self.view))
            == Ordering::Greater
    }
}

/// Authored Markdown blocks, in order: paragraphs, headings, and each list
/// item's own text, with protocol headers cut and quoted or code material
/// hidden.
fn authored_blocks(prose: &str) -> Vec<String> {
    let mut source = String::with_capacity(prose.len());
    let mut cursor = 0;
    for header in steer::placed_headers(prose) {
        if header.start > cursor {
            source.push_str(&prose[cursor..header.start]);
        }
        cursor = cursor.max(header.end);
    }
    source.push_str(&prose[cursor.min(prose.len())..]);

    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut hidden = 0_usize;
    let mut links = Vec::new();
    let mut flush = |current: &mut String| {
        let block = current.trim();
        if !block.is_empty() {
            blocks.push(block.to_owned());
        }
        current.clear();
    };
    for event in Parser::new(&source) {
        match event {
            Event::Start(Tag::BlockQuote(_) | Tag::CodeBlock(_) | Tag::HtmlBlock) => {
                flush(&mut current);
                hidden += 1;
            }
            Event::End(TagEnd::BlockQuote(_) | TagEnd::CodeBlock | TagEnd::HtmlBlock) => {
                hidden = hidden.saturating_sub(1);
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                let quote = dest_url.starts_with("spice://");
                hidden += usize::from(quote);
                links.push(quote);
            }
            Event::End(TagEnd::Link) => {
                if links.pop().unwrap_or(false) {
                    hidden = hidden.saturating_sub(1);
                }
            }
            Event::Start(Tag::Paragraph | Tag::Heading { .. } | Tag::Item)
            | Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item) => {
                flush(&mut current);
            }
            Event::Text(text) | Event::Code(text) if hidden == 0 => current.push_str(&text),
            Event::SoftBreak | Event::HardBreak if hidden == 0 => current.push('\n'),
            _ => {}
        }
    }
    flush(&mut current);
    blocks
}

fn digest_kind(kind: &[u8]) -> blake3::Hasher {
    let mut hasher = blake3::Hasher::new();
    digest_field(&mut hasher, kind);
    hasher
}

fn digest_field(hasher: &mut blake3::Hasher, field: &[u8]) {
    hasher.update(&(field.len() as u64).to_le_bytes());
    hasher.update(field);
}

#[cfg(test)]
mod tests;
