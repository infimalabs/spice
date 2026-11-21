//! Deterministic recent-window TF-IDF similarity between task statements,
//! tempered by how ordinary each term is in lane conversation.
//!
//! Documents use canonical lowercase terms from authored task fields. Ordinary
//! words need four letters. Technical runs retain original spelling and option
//! context until admission: selected three-character jargon qualifies by shape,
//! and alpha-first numeric jargon qualifies at any longer length. Pure numbers
//! and common English filler are inert. An ordinary plural that folds to three
//! characters reuses the authored singular's selective jargon admission; other
//! plural and singular folds apply to longer words. Subject vocabulary outside
//! the stop list, such as claim, phase, review, verdict, propose, todo and flow,
//! is left to fitted inverse document frequency over the most recently revised
//! statements and to the conversation [`Background`]. A
//! term's weight is logarithmic term frequency times statement inverse
//! document frequency times the [`CONVERSATION_RARITY_EXPONENT`] power of its
//! conversation inverse document frequency normalized by an unseen term's, so
//! a term in every held message keeps about a third of its weight and an empty
//! background is plain statement TF-IDF. L2-normalized sparse vectors then
//! produce one cosine score.
//!
//! Term counts are canonical and fit-independent. A [`Prepared`] vector is a
//! derived cache weighted under one fitted [`Generation`]; two vectors compare
//! only when they share it. Every cache is process-scoped, so a changed fold or
//! stop word takes effect with the binary that carries it. Every matcher (Design
//! awareness, the wake and `task next` match, and claim overlap) takes its fit
//! and statement vectors from the one shared [`MatchCache`], which counts the
//! background only when the statement window refits.

mod terms;

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BinaryHeap};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use topic::stop_words::ENGLISH as STOP;
use wire::TaskHead;
use wire::rooms::{COMPLETION_VALIDATION_BYTES, DESIGN_NOTICE_SHOWN_TERMS};

/// Fewest letters for ordinary terms and stems; selective short jargon is separate.
pub const MIN_TERM_CHARS: usize = 4;
/// Greatest Unicode-scalar length admitted as one sparse matching term.
pub const MAX_TERM_CHARS: usize = 32;
/// Most recently revised task statements, open or completed, in the frequency
/// corpus.
pub const FREQUENCY_WINDOW: usize = 256;
/// Least cosine at which a wide Design request, over unsteeped items, invites
/// a lane.
pub const WIDE_MATCH_FLOOR: f64 = 0.08;
/// Least cosine at which a narrow Design request, over steeped items, invites
/// a lane.
pub const NARROW_MATCH_FLOOR: f64 = 0.16;
/// Least cosine at which any request for a Design awaiting the operator's
/// decision invites a lane.
pub const DECISION_MATCH_FLOOR: f64 = 0.24;
/// Least cosine at which one quiet question item, read with its Design's
/// pitch, admits a request asking only that item when no whole Design invites
/// the lane.
pub const ITEM_MATCH_FLOOR: f64 = NARROW_MATCH_FLOOR;
/// Least cosine at which two concurrently claimed statements warn of overlap.
pub const CLAIM_OVERLAP_FLOOR: f64 = 0.25;
/// Power on a term's normalized conversation rarity in its weight: a term in
/// every held message keeps about a third of its statement weight, and a term
/// absent from conversation keeps all of it.
pub const CONVERSATION_RARITY_EXPONENT: f64 = 0.5;

/// Normalized term occurrence counts for one document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TermCounts(BTreeMap<String, u32>);

impl TermCounts {
    /// Count terms across arbitrary document fields.
    #[must_use]
    pub fn from_texts<'a>(texts: impl IntoIterator<Item = &'a str>) -> Self {
        let mut counts = BTreeMap::<String, u32>::new();
        for text in texts {
            for term in terms::terms(text) {
                let count = counts.entry(term).or_default();
                *count = count.saturating_add(1);
            }
        }
        Self(counts)
    }

    /// Number of distinct terms counted.
    #[must_use]
    pub fn distinct(&self) -> usize {
        self.0.len()
    }

    /// Count a task statement's title, pitch, description, and terms.
    #[must_use]
    pub fn statement(title: &str, pitch: &str, description: Option<&str>, term: &[String]) -> Self {
        Self::from_texts(
            std::iter::once(title)
                .chain(std::iter::once(pitch))
                .chain(description)
                .chain(term.iter().map(String::as_str)),
        )
    }

    /// Count one task head's authored statement.
    #[must_use]
    pub fn task(statement: &TaskHead) -> Self {
        Self::statement(
            &statement.title,
            &statement.pitch,
            statement.description.as_deref(),
            &statement.term,
        )
    }

    /// Count a completed statement plus its bounded successful validation.
    #[must_use]
    pub fn completion(
        title: &str,
        pitch: &str,
        description: Option<&str>,
        term: &[String],
        validation: &str,
    ) -> Self {
        let validation = utf8_prefix(validation, COMPLETION_VALIDATION_BYTES);
        Self::from_texts(
            std::iter::once(title)
                .chain(std::iter::once(pitch))
                .chain(description)
                .chain(term.iter().map(String::as_str))
                .chain(std::iter::once(validation)),
        )
    }

    /// These counts plus `other`'s, as one document counted from both texts.
    #[must_use]
    pub fn merged(mut self, other: &Self) -> Self {
        for (term, count) in &other.0 {
            let total = self.0.entry(term.clone()).or_default();
            *total = total.saturating_add(*count);
        }
        self
    }

    fn terms(&self) -> impl Iterator<Item = &String> {
        self.0.keys()
    }
}

/// Identity of one fitted pair of document-frequency tables.
///
/// Two fits over the same statements and background share a generation, and
/// any change to either population or any term's document frequency names a
/// different one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Generation([u8; 32]);

impl Generation {
    fn of(
        statements: usize,
        document_frequency: &BTreeMap<String, usize>,
        background: &Background,
    ) -> Self {
        let mut hasher = blake3::Hasher::new();
        for (documents, frequencies) in [
            (statements, document_frequency),
            (background.documents, &background.document_frequency),
        ] {
            hasher.update(&(documents as u64).to_le_bytes());
            hasher.update(&(frequencies.len() as u64).to_le_bytes());
            for (term, frequency) in frequencies {
                hasher.update(&(term.len() as u64).to_le_bytes());
                hasher.update(term.as_bytes());
                hasher.update(&(*frequency as u64).to_le_bytes());
            }
        }
        Self(*hasher.finalize().as_bytes())
    }

    /// Stable bytes recorded with a match that this fit weighted.
    #[must_use]
    pub const fn into_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// A cosine was asked between vectors weighted under different fits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GenerationMismatch {
    /// Generation of the vector the comparison was asked of.
    pub left: Generation,
    /// Generation of the vector it was compared with.
    pub right: Generation,
}

impl fmt::Display for GenerationMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("vectors weighted under different frequency generations do not compare")
    }
}

impl std::error::Error for GenerationMismatch {}

/// One L2-normalized TF-IDF vector and the fit that weighted it.
#[derive(Clone, Debug, PartialEq)]
pub struct Prepared {
    generation: Generation,
    weights: BTreeMap<String, f64>,
}

impl Prepared {
    /// The fit this vector was weighted under.
    #[must_use]
    pub fn generation(&self) -> Generation {
        self.generation
    }

    /// Cosine similarity with another vector of the same generation.
    ///
    /// # Errors
    ///
    /// Refuses vectors weighted under different generations rather than
    /// comparing incommensurable weights.
    pub fn cosine(&self, other: &Self) -> Result<f64, GenerationMismatch> {
        if self.generation != other.generation {
            return Err(GenerationMismatch {
                left: self.generation,
                right: other.generation,
            });
        }
        Ok(dot(&self.weights, &other.weights))
    }

    /// The strongest weighted overlaps with `other`, sorted, as a notice
    /// records them.
    #[must_use]
    pub fn explanation(&self, other: &Self) -> Vec<String> {
        let mut products = self
            .weights
            .iter()
            .filter_map(|(term, weight)| {
                other
                    .weights
                    .get(term)
                    .map(|other_weight| (term, weight * other_weight))
            })
            .collect::<Vec<_>>();
        products.sort_by(|(left_term, left), (right_term, right)| {
            right
                .total_cmp(left)
                .then_with(|| left_term.cmp(right_term))
        });
        let mut terms = products
            .into_iter()
            .take(DESIGN_NOTICE_SHOWN_TERMS)
            .map(|(term, _)| term.clone())
            .collect::<Vec<_>>();
        terms.sort();
        terms
    }

    /// The strongest weighted terms of this vector that `other` lacks, at
    /// most `limit`, strongest first; a missing `other` lacks every term.
    #[must_use]
    pub fn strongest_absent(&self, other: Option<&Self>, limit: usize) -> Vec<String> {
        let mut absent = self
            .weights
            .iter()
            .filter(|(term, _)| !other.is_some_and(|other| other.weights.contains_key(*term)))
            .collect::<Vec<_>>();
        absent.sort_by(|(left_term, left), (right_term, right)| {
            right
                .total_cmp(left)
                .then_with(|| left_term.cmp(right_term))
        });
        absent
            .into_iter()
            .take(limit)
            .map(|(term, _)| term.clone())
            .collect()
    }
}

/// Document frequencies of terms across the conversation lanes currently hold:
/// how ordinary a term is in what lanes say, whatever the board says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Background {
    documents: usize,
    document_frequency: BTreeMap<String, usize>,
}

impl Background {
    /// Count term presence across conversation documents.
    #[must_use]
    pub fn of<'a>(documents: impl IntoIterator<Item = &'a TermCounts>) -> Self {
        let (documents, document_frequency) = presence(documents);
        Self {
            documents,
            document_frequency,
        }
    }

    /// The share of its statement weight `term` keeps: one for a term no held
    /// message uses, or when nothing is held; lowest for a term in every held
    /// message.
    fn rarity(&self, term: &str) -> f64 {
        if self.documents == 0 {
            return 1.0;
        }
        let documents = self.documents as f64;
        let frequency = *self.document_frequency.get(term).unwrap_or(&0) as f64;
        (inverse_frequency(documents, frequency) / inverse_frequency(documents, 0.0))
            .powf(CONVERSATION_RARITY_EXPONENT)
    }
}

/// Bounded statement document frequencies shared by both compared texts,
/// tempered by the conversation background counted when they were fitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrequencyProfile {
    statements: usize,
    document_frequency: BTreeMap<String, usize>,
    background: Background,
    generation: Generation,
}

impl FrequencyProfile {
    /// Fit one profile from statement presence counts alone.
    #[must_use]
    pub fn of<D: std::borrow::Borrow<TermCounts>>(documents: impl IntoIterator<Item = D>) -> Self {
        Self::fitted(documents, Background::default())
    }

    /// Fit one profile from statement presence counts and a conversation
    /// background.
    #[must_use]
    pub fn fitted<D: std::borrow::Borrow<TermCounts>>(
        documents: impl IntoIterator<Item = D>,
        background: Background,
    ) -> Self {
        let (statements, document_frequency) = presence(documents);
        Self {
            generation: Generation::of(statements, &document_frequency, &background),
            statements,
            document_frequency,
            background,
        }
    }

    /// The identity of this fit.
    #[must_use]
    pub fn generation(&self) -> Generation {
        self.generation
    }
}

/// How many documents there are and how many hold each term.
fn presence<D: std::borrow::Borrow<TermCounts>>(
    documents: impl IntoIterator<Item = D>,
) -> (usize, BTreeMap<String, usize>) {
    let mut count = 0_usize;
    let mut document_frequency = BTreeMap::<String, usize>::new();
    for document in documents {
        count = count.saturating_add(1);
        for term in document.borrow().terms() {
            let frequency = document_frequency.entry(term.clone()).or_default();
            *frequency = frequency.saturating_add(1);
        }
    }
    (count, document_frequency)
}

/// Smoothed inverse frequency of a term `frequency` of `documents` hold.
fn inverse_frequency(documents: f64, frequency: f64) -> f64 {
    1.0 + ((documents + 1.0) / (frequency + 1.0)).ln()
}

/// The fitted recent window and the task vectors weighted under it, so every
/// matcher over an unchanged board reuses one fit.
///
/// Keys are content, never journal positions: the fit is keyed by the
/// window's statements and each vector by its statement's counted text, so
/// equal keys always mean equal inputs and one cache may serve any journal.
/// The conversation [`Background`] is counted only when the window refits, so
/// conversation alone never refits the window or drops a vector. Vectors are
/// dropped whole when the window moves; a vector asked under any other fit is
/// weighted afresh and not kept. The lock is a leaf, held only inside these
/// methods and never while a background is counted.
#[derive(Debug, Default)]
pub struct MatchCache(Mutex<FittedWindow>);

/// Content identity of the authored text [`TermCounts::task`] counts.
type StatementKey = [u8; 32];

/// The content key of exactly the texts [`TermCounts::task`] counts, in its
/// order, so equal keys always count equally.
fn statement_key(statement: &TaskHead) -> StatementKey {
    let mut hasher = blake3::Hasher::new();
    for text in std::iter::once(statement.title.as_str())
        .chain(std::iter::once(statement.pitch.as_str()))
        .chain(statement.description.as_deref())
        .chain(statement.term.iter().map(String::as_str))
    {
        hasher.update(&(text.len() as u64).to_le_bytes());
        hasher.update(text.as_bytes());
    }
    *hasher.finalize().as_bytes()
}

#[derive(Debug, Default)]
struct FittedWindow {
    fit: Option<(StatementKey, Arc<FrequencyProfile>)>,
    prepared: BTreeMap<StatementKey, Option<Arc<Prepared>>>,
}

impl MatchCache {
    /// An empty cache.
    #[must_use]
    pub const fn new() -> Self {
        Self(Mutex::new(FittedWindow {
            fit: None,
            prepared: BTreeMap::new(),
        }))
    }

    /// The process's one cache, which every production matcher shares.
    #[must_use]
    pub fn shared() -> &'static Self {
        static SHARED: MatchCache = MatchCache::new();
        &SHARED
    }

    /// The fit over the [`FREQUENCY_WINDOW`] most recently revised task
    /// statements, refitted only when that window's statements changed; a
    /// refit counts `background` then, and only then.
    #[must_use]
    pub fn fit<'a>(
        &self,
        statements: impl Iterator<Item = &'a TaskHead>,
        background: impl FnOnce() -> Background,
    ) -> Arc<FrequencyProfile> {
        self.refit(statements, |_| Some(background()))
    }

    /// [`Self::fit`] for a caller that holds no conversation. A refit keeps
    /// the background the current fit carries; before any fit exists, it
    /// returns an uncached statement-only profile so it cannot freeze an empty
    /// background into the shared window.
    pub fn fit_carried<'a>(
        &self,
        statements: impl Iterator<Item = &'a TaskHead>,
    ) -> Arc<FrequencyProfile> {
        self.refit(statements, |carried| carried.cloned())
    }

    fn refit<'a>(
        &self,
        statements: impl Iterator<Item = &'a TaskHead>,
        background: impl FnOnce(Option<&Background>) -> Option<Background>,
    ) -> Arc<FrequencyProfile> {
        let mut recent = BinaryHeap::with_capacity(FREQUENCY_WINDOW + 1);
        for statement in statements {
            recent.push(Reverse(RecentStatement(statement)));
            if recent.len() > FREQUENCY_WINDOW {
                recent.pop();
            }
        }
        let recent = recent.into_sorted_vec();
        let mut hasher = blake3::Hasher::new();
        for Reverse(RecentStatement(statement)) in &recent {
            hasher.update(&statement_key(statement));
        }
        let key = *hasher.finalize().as_bytes();
        let carried = match &self.window().fit {
            Some((current, frequency)) if *current == key => return Arc::clone(frequency),
            Some((_, frequency)) => Some(Arc::clone(frequency)),
            None => None,
        };
        let Some(background) = background(carried.as_deref().map(|carried| &carried.background))
        else {
            return Arc::new(FrequencyProfile::of(
                recent
                    .into_iter()
                    .map(|Reverse(RecentStatement(statement))| TermCounts::task(statement)),
            ));
        };
        let frequency = Arc::new(FrequencyProfile::fitted(
            recent
                .into_iter()
                .map(|Reverse(RecentStatement(statement))| TermCounts::task(statement)),
            background,
        ));
        *self.window() = FittedWindow {
            fit: Some((key, Arc::clone(&frequency))),
            prepared: BTreeMap::new(),
        };
        frequency
    }

    /// `statement` weighted under `frequency`, reused while `frequency` is
    /// this cache's current fit. `None` when it has no weightable terms.
    #[must_use]
    pub fn prepare(
        &self,
        frequency: &FrequencyProfile,
        statement: &TaskHead,
    ) -> Option<Arc<Prepared>> {
        let weigh = || {
            frequency
                .prepare(&TermCounts::task(statement))
                .map(Arc::new)
        };
        let mut window = self.window();
        if window
            .fit
            .as_ref()
            .is_none_or(|(_, current)| current.generation != frequency.generation)
        {
            return weigh();
        }
        window
            .prepared
            .entry(statement_key(statement))
            .or_insert_with(weigh)
            .clone()
    }

    /// The cached window; a panic mid-update leaves only an empty cache.
    fn window(&self) -> MutexGuard<'_, FittedWindow> {
        self.0.lock().unwrap_or_else(|poisoned| {
            self.0.clear_poison();
            let mut window = poisoned.into_inner();
            *window = FittedWindow::default();
            window
        })
    }
}

impl FrequencyProfile {
    /// Cosine similarity for two documents fitted against this same profile.
    #[must_use]
    pub fn score(&self, left: &TermCounts, right: &TermCounts) -> Option<f64> {
        let score = self.prepare(left)?.cosine(&self.prepare(right)?).ok()?;
        score.is_finite().then_some(score)
    }

    /// Weight one document under this fit. `None` when it has no terms or
    /// cannot be weighted finitely.
    #[must_use]
    pub fn prepare(&self, document: &TermCounts) -> Option<Prepared> {
        Some(Prepared {
            generation: self.generation,
            weights: self.vector(document)?,
        })
    }

    fn vector(&self, document: &TermCounts) -> Option<BTreeMap<String, f64>> {
        let population = self.statements as f64;
        let mut weights = BTreeMap::new();
        let mut squared_norm = 0.0;
        for (term, count) in &document.0 {
            let frequency = *self.document_frequency.get(term).unwrap_or(&0) as f64;
            let tf = 1.0 + f64::from(*count).ln();
            let idf = inverse_frequency(population, frequency);
            let weight = tf * idf * self.background.rarity(term);
            if !weight.is_finite() {
                return None;
            }
            squared_norm += weight * weight;
            weights.insert(term.clone(), weight);
        }
        let norm = squared_norm.sqrt();
        if !norm.is_finite() || norm == 0.0 {
            return None;
        }
        for weight in weights.values_mut() {
            *weight /= norm;
        }
        Some(weights)
    }
}

#[derive(Clone, Copy, Debug)]
struct RecentStatement<'a>(&'a TaskHead);

impl PartialEq for RecentStatement<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.revision == other.0.revision
    }
}

impl Eq for RecentStatement<'_> {}

impl PartialOrd for RecentStatement<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RecentStatement<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.revision.cmp(&other.0.revision)
    }
}

fn normalize(run: &str) -> Option<String> {
    if run.chars().any(char::is_numeric) {
        return None;
    }
    let mut word = run.to_lowercase();
    if STOP.binary_search(&word.as_str()).is_ok() {
        return None;
    }
    if let Some(stem) = word.strip_suffix("ies")
        && stem.chars().count() >= MIN_TERM_CHARS
    {
        word.truncate(word.len() - "ies".len());
        word.push('y');
    } else if let Some(stem) = word.strip_suffix('s')
        && stem.chars().count() >= MIN_TERM_CHARS - 1
    {
        let stem_chars = stem.chars().count();
        if stem_chars == MIN_TERM_CHARS - 1 {
            let authored = run.strip_suffix('s').or_else(|| run.strip_suffix('S'))?;
            let mut chars = authored.chars();
            if !terms::short_jargon([chars.next()?, chars.next()?, chars.next()?]) {
                return None;
            }
            word.pop();
            return terms::canonical_lower_jargon(word);
        }
        if !stem.ends_with('s') {
            word.pop();
        }
    }
    if let Some(stem) = word.strip_suffix("ie")
        && stem.chars().count() >= MIN_TERM_CHARS
    {
        word.truncate(word.len() - "ie".len());
        word.push('y');
    }
    (word.chars().count() >= MIN_TERM_CHARS
        && word.chars().all(|c| c.is_alphabetic() && !c.is_uppercase()))
    .then_some(word)
}

/// The longest prefix of `text` within `bytes` that ends on a UTF-8 boundary.
#[must_use]
pub fn utf8_prefix(text: &str, bytes: usize) -> &str {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn dot(left: &BTreeMap<String, f64>, right: &BTreeMap<String, f64>) -> f64 {
    left.iter()
        .map(|(term, weight)| weight * right.get(term).copied().unwrap_or(0.0))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(text: &str) -> TermCounts {
        TermCounts::from_texts([text])
    }

    fn generated_word(index: usize) -> String {
        let first = char::from(b'a' + ((index / (26 * 26)) % 26) as u8);
        let second = char::from(b'a' + ((index / 26) % 26) as u8);
        let third = char::from(b'a' + (index % 26) as u8);
        format!("term{first}{second}{third}")
    }

    fn close(left: f64, right: f64) {
        assert!((left - right).abs() < 1e-12, "{left} != {right}");
    }

    fn head(revision: u64, title: &str) -> TaskHead {
        let born = wire::TaskStamp::for_micros(revision as i64).expect("fixture stamp");
        let seq = wire::Seq::new(revision).expect("fixture position");
        TaskHead {
            preparation: None,
            intent: None,
            born: born.clone(),
            topic_origin: seq,
            title: title.to_owned(),
            pitch: "Matching reuses one fit.".to_owned(),
            description: None,
            term: Vec::new(),
            requirements: wire::EnvironmentRequirements::default(),
            resources: wire::ResourceRequirements::default(),
            topic: "/board/kernel".parse().expect("fixture topic"),
            priority: None,
            taste: None,
            dependencies: Vec::new(),
            provenance: wire::TaskProvenance::Task(born),
            creation: wire::body::TaskCreationContext::unavailable(
                wire::body::TaskCreationUnavailable::SpiceActor,
            ),
            revision: seq,
        }
    }

    #[test]
    fn a_cache_reuses_one_exact_fit_and_vector_until_its_window_moves() {
        let board = [
            head(1, "Snapshot cursor paging"),
            head(2, "Publication boundary retention"),
            head(3, "Lane team design"),
        ];
        let uncached =
            |statements: &[TaskHead]| FrequencyProfile::of(statements.iter().map(TermCounts::task));
        let cache = MatchCache::default();
        let fit = cache.fit(board.iter(), Background::default);
        assert_eq!(*fit, uncached(&board), "the cached fit is the exact fit");
        assert!(
            Arc::ptr_eq(&fit, &cache.fit(board.iter().rev(), Background::default)),
            "an unchanged window, in any order, reuses its fit"
        );
        let vector = cache.prepare(&fit, &board[0]).expect("weighted statement");
        assert_eq!(
            *vector,
            fit.prepare(&TermCounts::task(&board[0])).expect("weighted")
        );
        assert!(
            Arc::ptr_eq(&vector, &cache.prepare(&fit, &board[0]).expect("weighted")),
            "an unchanged statement reuses its vector"
        );

        // The same positions with only the required problem sentence changed.
        let mut edited = board.clone();
        edited[1].pitch = "Publication boundary retention cursor".to_owned();
        let refit = cache.fit(edited.iter(), Background::default);
        assert_eq!(
            *refit,
            uncached(&edited),
            "a changed statement refits the window"
        );
        assert_ne!(refit.generation(), fit.generation());
        let reweighted = cache
            .prepare(&refit, &edited[0])
            .expect("weighted statement");
        assert_eq!(reweighted.generation(), refit.generation());
        close(
            reweighted
                .cosine(&cache.prepare(&refit, &edited[1]).expect("weighted"))
                .expect("one fit"),
            refit
                .score(&TermCounts::task(&edited[0]), &TermCounts::task(&edited[1]))
                .expect("scored"),
        );
        let stale = cache.prepare(&fit, &board[0]).expect("weighted statement");
        assert_eq!(
            stale.generation(),
            fit.generation(),
            "a vector asked under a superseded fit stays in that fit's generation"
        );
    }

    #[test]
    fn conversation_counts_only_when_the_statement_window_refits() {
        let board = [
            head(1, "Snapshot cursor paging"),
            head(2, "Publication boundary retention"),
        ];
        let exact = |statements: &[TaskHead], held: &[TermCounts]| {
            FrequencyProfile::fitted(
                statements.iter().map(TermCounts::task),
                Background::of(held),
            )
        };
        let held = [document("snapshot cursor review")];
        let cache = MatchCache::default();
        let fit = cache.fit(board.iter(), || Background::of(&held));
        assert_eq!(*fit, exact(&board, &held));
        let vector = cache.prepare(&fit, &board[0]).expect("weighted statement");

        let unchanged = cache.fit(board.iter(), || {
            panic!("conversation alone must not refit the window")
        });
        assert!(Arc::ptr_eq(&fit, &unchanged));
        assert!(Arc::ptr_eq(
            &vector,
            &cache.prepare(&unchanged, &board[0]).expect("weighted")
        ));

        let mut edited = board.clone();
        edited[1] = head(2, "Publication boundary retention cursor");
        let later = [
            document("publication boundary"),
            document("boundary review"),
        ];
        let refit = cache.fit(edited.iter(), || Background::of(&later));
        assert_eq!(
            *refit,
            exact(&edited, &later),
            "a refit takes the current background"
        );
        assert_ne!(refit.generation(), exact(&edited, &[]).generation());

        edited[0] = head(1, "Snapshot cursor paging moved");
        let carried = cache.fit_carried(edited.iter());
        assert_eq!(
            *carried,
            exact(&edited, &later),
            "a refit without conversation keeps the carried background"
        );
    }

    #[test]
    fn a_cold_caller_without_conversation_does_not_freeze_an_empty_background() {
        let board = [
            head(1, "Claim cursor paging"),
            head(2, "Publication boundary retention"),
        ];
        let cache = MatchCache::default();
        let cold = cache.fit_carried(board.iter());
        assert_eq!(
            *cold,
            FrequencyProfile::of(board.iter().map(TermCounts::task)),
            "a caller without profiles can still score against statements"
        );

        let held = [document("claim review"), document("claim room")];
        let fitted = cache.fit(board.iter(), || Background::of(&held));
        assert_eq!(
            *fitted,
            FrequencyProfile::fitted(board.iter().map(TermCounts::task), Background::of(&held),),
            "the first profile-aware caller establishes the shared background"
        );
        assert_ne!(cold.generation(), fitted.generation());
    }

    #[test]
    fn a_term_common_in_conversation_weighs_less_than_one_it_never_uses() {
        let statements = [document("claim cursor"), document("paging snapshot")];
        let weights = |held: &[TermCounts]| {
            FrequencyProfile::fitted(statements.iter().cloned(), Background::of(held))
                .prepare(&document("claim cursor"))
                .expect("weighted")
                .weights
        };

        let plain = weights(&[]);
        close(plain["claim"], std::f64::consts::FRAC_1_SQRT_2);
        close(plain["cursor"], std::f64::consts::FRAC_1_SQRT_2);

        let tempered = weights(&[document("claim lane"), document("claim room")]);
        assert!(tempered["claim"] < tempered["cursor"]);
        close(
            tempered["claim"] / tempered["cursor"],
            (1.0 / (1.0 + 3.0_f64.ln())).powf(CONVERSATION_RARITY_EXPONENT),
        );
    }

    #[test]
    fn term_and_document_frequency_are_logarithmic_presence_counts() {
        let profile = FrequencyProfile::of([document("alpha alpha beta"), document("alpha gamma")]);
        assert_eq!(profile.statements, 2);
        assert_eq!(profile.document_frequency["alpha"], 2);
        assert_eq!(profile.document_frequency["beta"], 1);

        let vector = profile.vector(&document("alpha alpha beta")).unwrap();
        let alpha = (1.0 + 2.0_f64.ln()) * (1.0 + (3.0_f64 / 3.0).ln());
        let beta = (1.0 + 1.0_f64.ln()) * (1.0 + (3.0_f64 / 2.0).ln());
        let norm = (alpha * alpha + beta * beta).sqrt();
        close(vector["alpha"], alpha / norm);
        close(vector["beta"], beta / norm);
        close(vector.values().map(|weight| weight * weight).sum(), 1.0);
    }

    #[test]
    fn empty_nonoverlapping_and_diluted_documents_score_low() {
        let profile = FrequencyProfile::of([document("alpha beta")]);
        assert!(
            profile
                .score(&TermCounts::default(), &document("alpha"))
                .is_none()
        );
        close(
            profile
                .score(&document("alpha"), &document("bravo"))
                .unwrap(),
            0.0,
        );

        let shared = generated_word(0);
        let left = (0..64).map(generated_word).collect::<Vec<_>>().join(" ");
        let right = std::iter::once(shared)
            .chain((64..127).map(generated_word))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(profile.score(&document(&left), &document(&right)).unwrap() < WIDE_MATCH_FLOOR);
    }

    #[test]
    fn explanations_keep_the_strongest_products_then_store_them_sorted() {
        let profile = FrequencyProfile::of([document(
            "alpha bravo charly delta foxtrot gamma hotel india",
        )]);
        let text = "alpha alpha alpha bravo bravo charly delta foxtrot gamma hotel india";
        let left = profile.prepare(&document(text)).unwrap();
        let right = profile.prepare(&document(text)).unwrap();
        let terms = left.explanation(&right);
        assert_eq!(
            terms,
            ["alpha", "bravo", "charly", "delta", "foxtrot", "gamma"]
        );
        assert_eq!(terms.len(), DESIGN_NOTICE_SHOWN_TERMS);
    }

    #[test]
    fn statement_fields_contribute_and_completion_validation_is_only_byte_bounded() {
        let statement = TermCounts::statement(
            "Snapshot proposal",
            "Compiler routing",
            None,
            &["Should retained cursors bound each publication page?".to_owned()],
        );
        assert!(statement.0.contains_key("compiler"));
        assert!(statement.0.contains_key("cursor"));
        assert!(statement.0.contains_key("publication"));
        let distinct = (0..300).map(generated_word).collect::<Vec<_>>();
        let completion = TermCounts::completion(
            "Retained statement marker",
            "Matching problem",
            Some("Statement description"),
            &[],
            &distinct.join(" "),
        );
        assert!(
            completion.0.len() > FREQUENCY_WINDOW,
            "the bounded validation has no distinct-term cap"
        );

        let validation = format!(
            "{}é boundarymarker",
            "x".repeat(COMPLETION_VALIDATION_BYTES - 1)
        );
        let clipped =
            TermCounts::completion("Retained statement marker", "", None, &[], &validation);
        assert!(
            !clipped.0.contains_key("boundarymarker"),
            "validation after a partial UTF-8 boundary contributes no terms"
        );
    }

    #[test]
    fn function_words_short_words_digit_runs_and_unrecordable_words_are_not_terms() {
        assert_eq!(
            document("The lane's v2 work with each task: designs and rooms")
                .0
                .into_keys()
                .collect::<Vec<_>>(),
            ["design", "lane", "room", "task", "work"]
        );
        assert_eq!(
            document("İstanbul 𝐁𝐎𝐋𝐃 builds")
                .0
                .into_keys()
                .collect::<Vec<_>>(),
            ["build"],
            "a word whose lowercase holds a mark or an uppercase letter is never recorded"
        );
    }

    #[test]
    fn short_function_words_are_stopped_in_every_case_without_erasing_jargon() {
        for word in [
            "all", "and", "any", "are", "but", "can", "few", "for", "had", "has", "her", "him",
            "his", "how", "its", "may", "nor", "not", "now", "our", "out", "own", "per", "she",
            "the", "too", "was", "who", "why", "yet", "you",
        ] {
            for mask in 0..8 {
                let spelling: String = word
                    .chars()
                    .enumerate()
                    .map(|(index, c)| {
                        if mask & (1 << index) == 0 {
                            c
                        } else {
                            c.to_ascii_uppercase()
                        }
                    })
                    .collect();
                assert_eq!(
                    document(&format!("{spelling} CSS CLI API iOS x86 p95")).0,
                    document("CSS CLI API iOS x86 p95").0,
                    "{spelling} must not contribute a term"
                );
            }
        }
    }

    #[test]
    fn reference_filler_cannot_create_similarity() {
        let filler = "again already always around because between during either enough even ever \
                      first further here however keep last least made might next once perhaps \
                      rather since still together upon well were whole AND THE AnD tHE CAN FOR \
                      HAS WAS YOU SYSTEM name EMPTY DoNe GET themselves systems names";
        let left = document(&format!("alpha {filler}"));
        let right = document(&format!("bravo {filler}"));
        let technical = document(&format!(
            "CSS CLI API iOS x86 p95 LSP Rust WASM SQLite UUID {filler}"
        ));
        let clean = document("CSS CLI API iOS x86 p95 LSP Rust WASM SQLite UUID");
        let profile = FrequencyProfile::of([&left, &right, &technical, &clean]);
        close(profile.score(&left, &right).expect("documents score"), 0.0);
        close(
            profile.score(&technical, &clean).expect("jargon scores"),
            1.0,
        );
    }

    #[test]
    fn plural_and_ie_folding_share_canonical_terms() {
        assert_eq!(document("cookie cookies cooky").0["cooky"], 3);
        assert_eq!(document("boundary boundaries").0["boundary"], 2);
        assert_eq!(
            document("movie movies").0["movie"],
            2,
            "a short stem keeps the existing singular form"
        );
    }
}
