//! Pure word-bag matching over typed assistant prose.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use gate::{Applicability, PolicyContext, PolicyResolveError, RuleId, ScopeSelector};
use listen::{Event, Locus};
use wire::{Digest, Domain, Name};

/// Maximum UTF-8 evidence admitted to one local taste-judge framing.
pub const MAX_JUDGE_SOURCE_BYTES: usize = 1024;

const OMISSION: &str = "…";
const MAX_TRIGGER_BYTES: usize = MAX_JUDGE_SOURCE_BYTES - 2 * OMISSION.len();

/// One admitted candidate selector.
///
/// Matching is case-insensitive over alphabetic words. Whitespace in a phrase
/// is normalized before matching, and every selector word must match one
/// complete prose word exactly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Trigger {
    source: String,
    words: Vec<String>,
}

impl Trigger {
    /// Parse one exact alphabetic word or phrase.
    ///
    /// # Errors
    ///
    /// Refuses empty selectors and every non-alphabetic selector term.
    pub fn parse(source: impl AsRef<str>) -> Result<Self, TriggerError> {
        let source = source.as_ref().trim();
        if source.is_empty() {
            return Err(TriggerError::Empty);
        }
        if source.len() > MAX_TRIGGER_BYTES {
            return Err(TriggerError::TooLong {
                bytes: source.len(),
                maximum: MAX_TRIGGER_BYTES,
            });
        }

        let words = source
            .split_whitespace()
            .map(normalize_selector)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            source: source.to_owned(),
            words,
        })
    }

    /// Selector spelling supplied by tracked configuration.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.source
    }

    fn spans(&self, tokens: &[Option<String>]) -> Vec<Span> {
        tokens
            .windows(self.words.len())
            .enumerate()
            .filter(|(_, window)| {
                window
                    .iter()
                    .zip(&self.words)
                    .all(|(token, word)| token.as_deref() == Some(word))
            })
            .map(|(first, _)| Span {
                first,
                end: first + self.words.len(),
            })
            .collect()
    }
}

fn normalize_selector(word: &str) -> Result<String, TriggerError> {
    if word.is_empty() || !word.chars().all(char::is_alphabetic) {
        return Err(TriggerError::NotWord(word.to_owned()));
    }
    Ok(lowercase(word))
}

fn lowercase(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

/// Canonical authored prose used by durable reminder identity.
///
/// Matching is case-insensitive and whitespace-insensitive, so identity uses
/// the same two equivalences while retaining every non-whitespace character.
/// Punctuation remains significant: this is an identity for the whole prose,
/// not for the smaller token stream selectors happen to inspect.
#[must_use]
pub fn normalize_prose(text: &str) -> String {
    lowercase(&text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Domain-separated identity of one taste firing on one authored prose body.
///
/// The two components are length framed rather than delimiter joined because
/// prose may contain any delimiter. The compaction epoch deliberately is not
/// part of this value: it is carried beside the identity on the reminder fact,
/// allowing the same content to become eligible once in each later epoch.
#[must_use]
pub fn reminder_identity(taste: &Name, prose: &str) -> Digest {
    let normalized = normalize_prose(prose);
    let mut framed = Vec::with_capacity(16 + taste.as_str().len() + normalized.len());
    framed.extend_from_slice(&(taste.as_str().len() as u64).to_be_bytes());
    framed.extend_from_slice(taste.as_str().as_bytes());
    framed.extend_from_slice(&(normalized.len() as u64).to_be_bytes());
    framed.extend_from_slice(normalized.as_bytes());
    Digest::compute(Domain::TasteProse, &framed)
}

/// Canonical identity of one complete ordered configured taste bag.
///
/// Names, selectors, triggers, and vendors are length-framed in their admitted
/// order, and enablement is one fixed byte, so any policy movement produces a
/// new identity without a second serializer.
#[must_use]
pub fn bag_identity(bag: &Bag) -> Digest {
    let mut framed = Vec::new();
    frame_count(&mut framed, bag.tastes.len());
    for taste in &bag.tastes {
        frame_text(&mut framed, taste.name.as_str());
        frame_count(&mut framed, taste.triggers.len());
        for trigger in &taste.triggers {
            frame_text(&mut framed, trigger.as_str());
        }
        frame_text(&mut framed, &taste.message);
        framed.push(u8::from(taste.enabled));
        for values in [
            taste.applicability.selector().paths(),
            taste.applicability.selector().extensions(),
            taste.applicability.selector().vendors(),
            taste.applicability.selector().models(),
            taste.applicability.selector().phases(),
        ] {
            frame_count(&mut framed, values.len());
            for value in values {
                frame_text(&mut framed, value);
            }
        }
    }
    Digest::compute(Domain::TasteBag, &framed)
}

fn frame_count(framed: &mut Vec<u8>, count: usize) {
    framed.extend_from_slice(&(count as u64).to_be_bytes());
}

fn frame_text(framed: &mut Vec<u8>, text: &str) {
    frame_count(framed, text.len());
    framed.extend_from_slice(text.as_bytes());
}

struct ProseTokens {
    words: Vec<Option<String>>,
    loci: Vec<(usize, usize)>,
    ranges: Vec<Range<usize>>,
}

fn prose_tokens(text: &str) -> ProseTokens {
    // A digit attached to letters makes the whole alphanumeric run inert. It
    // remains as a barrier so `fall 2 back` cannot collapse into the phrase
    // `fall back`, while punctuation and whitespace still delimit words.
    let mut tokens = Vec::new();
    let mut loci = Vec::new();
    let mut ranges = Vec::new();
    let mut current = String::new();
    let mut current_start = None;
    let mut alphabetic = true;
    let mut line = 1;
    let mut line_start = 0;

    for (offset, character) in text.char_indices() {
        if character.is_alphanumeric() {
            if current.is_empty() {
                current_start = Some(offset);
                loci.push((line, offset - line_start + 1));
                alphabetic = true;
            }
            current.extend(character.to_lowercase());
            alphabetic &= character.is_alphabetic();
        } else {
            if !current.is_empty() {
                tokens.push(alphabetic.then(|| std::mem::take(&mut current)));
                current.clear();
                ranges.push(current_start.take().expect("a token owns its start")..offset);
            }
            if character == '\n' {
                line += 1;
                line_start = offset + 1;
            }
        }
    }
    if !current.is_empty() {
        tokens.push(alphabetic.then_some(current));
        ranges.push(current_start.expect("a token owns its start")..text.len());
    }

    ProseTokens {
        words: tokens,
        loci,
        ranges,
    }
}

/// Why a trigger spelling was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TriggerError {
    Empty,
    NotWord(String),
    TooLong { bytes: usize, maximum: usize },
}

impl fmt::Display for TriggerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("a taste trigger cannot be empty"),
            Self::NotWord(word) => write!(
                formatter,
                "taste trigger term {word:?} is not an alphabetic word"
            ),
            Self::TooLong { bytes, maximum } => write!(
                formatter,
                "taste trigger is {bytes} UTF-8 bytes; at most {maximum} fit in bounded judge evidence"
            ),
        }
    }
}

impl Error for TriggerError {}

/// One tracked taste definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Taste {
    name: Name,
    triggers: Vec<Trigger>,
    message: String,
    enabled: bool,
    applicability: Applicability,
}

impl Taste {
    /// Build one taste from its tracked word bag and imperative message.
    ///
    /// # Errors
    ///
    /// Refuses an empty bag, an empty message, or any invalid trigger.
    pub fn new<I, S>(name: Name, words: I, message: impl Into<String>) -> Result<Self, BagError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::configured(name, words, message, true, &ScopeSelector::default())
    }

    pub(crate) fn configured<I, S>(
        name: Name,
        words: I,
        message: impl Into<String>,
        enabled: bool,
        scopes: &ScopeSelector,
    ) -> Result<Self, BagError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let triggers = words
            .into_iter()
            .map(Trigger::parse)
            .collect::<Result<Vec<_>, _>>()?;
        if triggers.is_empty() {
            return Err(BagError::EmptyWords(name.to_string()));
        }
        let message = message.into();
        if message.trim().is_empty() {
            return Err(BagError::EmptyMessage(name.to_string()));
        }
        let subject = name
            .as_str()
            .parse::<RuleId>()
            .expect("every admitted taste name is a valid selector subject");
        let applicability = Applicability::compile(subject, scopes)?;
        Ok(Self {
            name,
            triggers,
            message,
            enabled,
            applicability,
        })
    }

    /// Stable one-word taste identity.
    #[must_use]
    pub const fn name(&self) -> &Name {
        &self.name
    }

    /// Candidate selectors in tracked order.
    #[must_use]
    pub fn triggers(&self) -> &[Trigger] {
        &self.triggers
    }

    /// Imperative presented when a candidate becomes a live reminder.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Whether this named tracked row participates in matching.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Canonical shared selector constraining this taste.
    #[must_use]
    pub const fn scopes(&self) -> &ScopeSelector {
        self.applicability.selector()
    }
}

/// A closed configured set of tracked tastes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bag {
    tastes: Vec<Taste>,
}

impl Bag {
    /// Admit a configured set with one row per taste name.
    ///
    /// # Errors
    ///
    /// Refuses a repeated taste identity.
    pub fn new(tastes: Vec<Taste>) -> Result<Self, BagError> {
        let mut names = BTreeSet::new();
        for taste in &tastes {
            if !names.insert(taste.name().as_str()) {
                return Err(BagError::Duplicate(taste.name().to_string()));
            }
        }
        Ok(Self { tastes })
    }

    /// Named tracked tastes in tracked order, including disabled rows.
    #[must_use]
    pub fn tastes(&self) -> &[Taste] {
        &self.tastes
    }

    /// Select applicable configured candidates from one typed listening event.
    ///
    /// Only [`Event::Prose`] is readable here. Reasoning, tools, usage,
    /// compaction, and failure events produce no candidates, and this API has
    /// no vendor-byte input to parse.
    /// # Errors
    ///
    /// Refuses malformed context or any selector axis the caller did not
    /// supply.
    pub fn hits(
        &self,
        event: &Event,
        context: &PolicyContext,
    ) -> Result<Vec<Hit>, PolicyResolveError> {
        let Event::Prose(prose) = event else {
            return Ok(Vec::new());
        };
        let ProseTokens {
            words: tokens,
            loci,
            ranges,
        } = prose_tokens(&prose.text);
        let mut hits = Vec::new();
        for taste in self.tastes.iter().filter(|taste| taste.enabled) {
            if !taste.applicability.matches(context)? {
                continue;
            }
            for trigger in &taste.triggers {
                for span in trigger.spans(&tokens) {
                    let (line, column) = loci[span.first];
                    let match_range = ranges[span.first].start..ranges[span.end - 1].end;
                    if match_range.len() > MAX_TRIGGER_BYTES {
                        // A lexical phrase separated by more source bytes than one
                        // evidence record can retain is not an admissible match.
                        continue;
                    }
                    hits.push(Hit {
                        taste: taste.name.clone(),
                        trigger: trigger.source.clone(),
                        message: taste.message.clone(),
                        span,
                        match_range,
                        context: prose.text.clone(),
                        locus: prose.locus,
                        line,
                        column,
                    });
                }
            }
        }
        Ok(hits)
    }
}

/// Why a taste bag was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BagError {
    Trigger(TriggerError),
    Applicability(gate::PolicyCompileError),
    EmptyWords(String),
    EmptyMessage(String),
    Duplicate(String),
}

impl fmt::Display for BagError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Trigger(error) => error.fmt(formatter),
            Self::Applicability(error) => error.fmt(formatter),
            Self::EmptyWords(name) => write!(formatter, "taste {name} has an empty word bag"),
            Self::EmptyMessage(name) => write!(formatter, "taste {name} has an empty message"),
            Self::Duplicate(name) => write!(formatter, "taste {name} is declared more than once"),
        }
    }
}

impl Error for BagError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Trigger(error) => Some(error),
            Self::Applicability(error) => Some(error),
            Self::EmptyWords(_) | Self::EmptyMessage(_) | Self::Duplicate(_) => None,
        }
    }
}

impl From<TriggerError> for BagError {
    fn from(error: TriggerError) -> Self {
        Self::Trigger(error)
    }
}

impl From<gate::PolicyCompileError> for BagError {
    fn from(error: gate::PolicyCompileError) -> Self {
        Self::Applicability(error)
    }
}

/// Half-open token positions selected by one trigger.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Span {
    pub first: usize,
    pub end: usize,
}

fn bounded_sentence(text: &str, matched: Range<usize>) -> String {
    debug_assert!(matched.start < matched.end);
    debug_assert!(text.is_char_boundary(matched.start));
    debug_assert!(text.is_char_boundary(matched.end));

    let mut start = None;
    let mut end = None;
    for (sentence_start, sentence) in text.split_sentence_bound_indices() {
        let sentence = sentence_start..sentence_start + sentence.len();
        if start.is_none() && sentence.start <= matched.start && matched.start < sentence.end {
            start = Some(sentence.start);
        }
        if sentence.start < matched.end && matched.end <= sentence.end {
            end = Some(sentence.end);
            break;
        }
    }
    let sentence = match (start, end) {
        (Some(start), Some(end)) => start..end,
        _ => matched.clone(),
    };
    let sentence = trim_sentence(text, sentence, &matched);
    let relative = matched.start - sentence.start..matched.end - sentence.start;
    bounded_segment(&text[sentence], relative)
}

fn trim_sentence(text: &str, mut sentence: Range<usize>, matched: &Range<usize>) -> Range<usize> {
    while sentence.start < matched.start {
        let character = text[sentence.start..]
            .chars()
            .next()
            .expect("the sentence start precedes its match");
        if !character.is_whitespace() {
            break;
        }
        sentence.start += character.len_utf8();
    }
    while sentence.end > matched.end {
        let character = text[..sentence.end]
            .chars()
            .next_back()
            .expect("the sentence end follows its match");
        if !character.is_whitespace() {
            break;
        }
        sentence.end -= character.len_utf8();
    }
    sentence
}

fn bounded_segment(sentence: &str, matched: Range<usize>) -> String {
    if sentence.len() <= MAX_JUDGE_SOURCE_BYTES {
        return sentence.to_owned();
    }

    let marker_bytes = 2 * OMISSION.len();
    let content_bytes = MAX_JUDGE_SOURCE_BYTES - marker_bytes;
    let matched_bytes = matched.end - matched.start;
    debug_assert!(
        matched_bytes <= content_bytes,
        "configured triggers leave room for both omission markers"
    );
    let context_bytes = content_bytes.saturating_sub(matched_bytes);
    let left_available = matched.start;
    let right_available = sentence.len() - matched.end;
    let mut left_bytes = left_available.min(context_bytes / 2);
    let mut right_bytes = right_available.min(context_bytes - left_bytes);
    let remaining = context_bytes - left_bytes - right_bytes;
    let extra_left = (left_available - left_bytes).min(remaining);
    left_bytes += extra_left;
    right_bytes += (right_available - right_bytes).min(remaining - extra_left);

    let mut start = matched.start - left_bytes;
    while !sentence.is_char_boundary(start) {
        start += 1;
    }
    let mut end = matched.end + right_bytes;
    while !sentence.is_char_boundary(end) {
        end -= 1;
    }

    let mut bounded = String::with_capacity(MAX_JUDGE_SOURCE_BYTES);
    if start > 0 {
        bounded.push_str(OMISSION);
    }
    bounded.push_str(&sentence[start..end]);
    if end < sentence.len() {
        bounded.push_str(OMISSION);
    }
    debug_assert!(bounded.len() <= MAX_JUDGE_SOURCE_BYTES);
    bounded
}

/// One selected prose candidate.
///
/// A hit is inert evidence and cannot mint a fact itself. Resident recognition
/// combines it with current lane, epoch, steering, and transaction authority
/// before minting the one reminder the runtime law admits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hit {
    taste: Name,
    trigger: String,
    message: String,
    span: Span,
    match_range: Range<usize>,
    context: String,
    locus: Locus,
    line: usize,
    column: usize,
}

impl Hit {
    #[must_use]
    pub const fn taste(&self) -> &Name {
        &self.taste
    }

    #[must_use]
    pub fn trigger(&self) -> &str {
        &self.trigger
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// Exact half-open UTF-8 byte range covered by the matched trigger.
    #[must_use]
    pub fn match_range(&self) -> Range<usize> {
        self.match_range.clone()
    }

    /// Bounded sentence evidence supplied to the optional local judge.
    #[must_use]
    pub fn judge_source(&self) -> String {
        bounded_sentence(&self.context, self.match_range.clone())
    }

    #[must_use]
    pub fn context(&self) -> &str {
        &self.context
    }

    #[must_use]
    pub const fn locus(&self) -> Locus {
        self.locus
    }

    /// One-based prose line at which the selected trigger begins.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }

    /// One-based byte column at which the selected trigger begins.
    #[must_use]
    pub const fn column(&self) -> usize {
        self.column
    }
}
