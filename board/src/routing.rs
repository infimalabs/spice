//! One TF-IDF fit and tiered, scored public tree, shared by restricted and
//! unrestricted placement.
//!
//! A route is one source-bound hierarchical decision. The authority filter
//! selects the admitted public subtrees and the declared unmatched topic; the
//! authored statement is then scored against topic descriptions and the live
//! task corpus; only where that pass cannot choose uniquely does the bounded
//! immutable origin view join the query, and only after that does the path
//! tier discriminate: an authored relative tail first, matched leaf-up through
//! contiguous existing components, then public path-name similarity. Descent
//! follows a unique improvement and otherwise stops at the deepest explained
//! parent; disjoint roots that stay unresolved through every tier go to the
//! declared unmatched topic outside scoring, or else leave the route
//! undecided so the task stays unplaced.
//! Nothing else breaks a tie. A tail is composed exactly once below the
//! placement prefix, reusing a unique existing match inside the decided
//! subtree and otherwise folding maximal exact boundary overlap.

use std::fmt;

use wire::intent::{RouteConfidence, RouteConnection, RouteStop, RouteTier};
use wire::{Topic, TopicTail};

use crate::similarity::{FrequencyProfile, Prepared, TermCounts};

/// Most residual statement terms one decision reports.
pub const RESIDUAL_TERMS: usize = 8;

/// Public subtrees selected by the literal roots of drive rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoutingRoots {
    All,
    Selected(Vec<Topic>),
}

impl RoutingRoots {
    #[must_use]
    pub fn admits(&self, topic: &Topic) -> bool {
        match self {
            Self::All => true,
            Self::Selected(roots) => roots
                .iter()
                .any(|root| below(topic.as_str(), root.as_str())),
        }
    }
}

/// The authority filter: admitted roots and the policy-declared destination
/// for disjoint roots that stay unresolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub roots: RoutingRoots,
    pub unmatched: Option<Topic>,
}

/// The authored statement, the bounded immutable origin view consulted only
/// after the statement alone cannot choose, and the authored relative tail
/// consulted only at the path tier.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub statement: TermCounts,
    pub origin: Option<TermCounts>,
    pub tail: Option<TopicTail>,
}

/// A described or task-bearing public node; structural ancestors need no document.
#[derive(Debug)]
pub struct Node {
    pub id: i64,
    pub path: Topic,
    pub terms: TermCounts,
}

/// Stable identity and current spelling of the selected node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pick {
    pub node: i64,
    pub topic: Topic,
}

/// Where one route lands: a scored node or the declared unmatched path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Node(Pick),
    Unmatched(Topic),
}

impl Target {
    #[must_use]
    pub fn topic(&self) -> &Topic {
        match self {
            Self::Node(pick) => &pick.topic,
            Self::Unmatched(topic) => topic,
        }
    }
}

/// One complete routing decision and the evidence that produced it.
#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    /// The placement prefix: the explained node or the unmatched topic.
    pub target: Target,
    /// The final topic, with any authored tail composed exactly once.
    pub destination: Topic,
    /// The scored node already spelled exactly as `destination`.
    pub destination_node: Option<i64>,
    /// Weakest tier any step needed.
    pub tier: RouteTier,
    pub stop: RouteStop,
    /// Strongest cosine any consulted tier found with the placement prefix;
    /// zero for the unmatched topic.
    pub overlap: f64,
    /// Strongest statement terms the placement prefix does not cover.
    pub residual: Vec<String>,
    /// How the authored tail met the tree; absent without a tail.
    pub structure: Option<Structure>,
}

/// Structural comparison of the root-down semantic prefix with the leaf-up
/// tail match, kept apart from every cosine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Structure {
    /// Deepest node the statement or origin tiers chose; absent when nothing
    /// semantic decided.
    pub prefix: Option<Topic>,
    /// The existing node the tail matched leaf-up inside the decided subtree.
    pub matched: Option<Pick>,
    /// Leading tail components the match covered; the rest were appended.
    pub matched_components: usize,
    pub connection: RouteConnection,
    /// Boundary components folded when the tail composed below the prefix
    /// without a match.
    pub composed_overlap: usize,
    /// An existing root-anchored path spelled exactly like the tail that the
    /// decision did not choose.
    pub alternative: Option<Topic>,
}

impl Decision {
    #[must_use]
    pub fn confidence(&self) -> RouteConfidence {
        if matches!(self.target, Target::Unmatched(_))
            || self.overlap <= 0.0
            || matches!(
                self.stop,
                RouteStop::ChildrenEqual | RouteStop::ChildrenTied
            )
        {
            RouteConfidence::Low
        } else if self.tier > RouteTier::Statement {
            RouteConfidence::Secondary
        } else {
            RouteConfidence::Evidenced
        }
    }
}

/// Routing authority is configuration: a drive rule without a public literal
/// root, or an invalid declared unmatched topic, refuses at admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteError {
    InvalidRoot(String),
    InvalidUnmatched(Topic),
    MissingPolicy(String),
}

impl RouteError {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::InvalidRoot(_) => "task::route-invalid-root",
            Self::InvalidUnmatched(_) => "task::route-invalid-unmatched",
            Self::MissingPolicy(_) => "task::route-policy-missing",
        }
    }
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRoot(pattern) => {
                write!(f, "topic rule {pattern:?} has no valid public literal root")
            }
            Self::InvalidUnmatched(topic) => {
                write!(
                    f,
                    "unmatched topic {topic} must be public and outside /oops"
                )
            }
            Self::MissingPolicy(name) => write!(f, "bound team policy {name:?} is not configured"),
        }
    }
}

impl std::error::Error for RouteError {}

struct ScoredNode {
    pick: Pick,
    /// Cosine per tier, indexed by [`RouteTier`] order.
    scores: [f64; 3],
    document: Option<Prepared>,
    parent: Option<usize>,
    children: Vec<usize>,
    /// Leading tail components this node's own path ends with.
    matched: usize,
    /// Strongest `matched` in this node's subtree, itself included.
    reach: usize,
}

/// Scores never change when the authority filter changes.
pub struct Scored {
    nodes: Vec<ScoredNode>,
    statement: Option<Prepared>,
    tiers: Vec<RouteTier>,
    /// Authored tail components, empty without a tail.
    tail: Vec<String>,
}

/// What one level of the tree decided.
enum Choice {
    Chosen(usize, RouteTier),
    /// Candidates improving on the parent, or the roots, stayed tied.
    Tied(Vec<usize>, RouteTier),
    /// No candidate beat its parent on any tier.
    Equal(RouteTier),
    /// Every candidate scored below its parent at this tier.
    Below(RouteTier),
}

impl Scored {
    pub fn new(mut nodes: Vec<Node>, query: &Query) -> Self {
        // UUID nodes are not public targets even if a caller supplied their text.
        nodes.retain(|node| node.path.as_str().starts_with('/'));
        nodes.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));
        let frequency = FrequencyProfile::of(nodes.iter().map(|node| &node.terms));
        let statement = frequency.prepare(&query.statement);
        let origin = query
            .origin
            .as_ref()
            .and_then(|origin| frequency.prepare(&query.statement.clone().merged(origin)));
        let tiers = if query.origin.is_some() {
            vec![RouteTier::Statement, RouteTier::Origin, RouteTier::Path]
        } else {
            vec![RouteTier::Statement, RouteTier::Path]
        };
        let tail = query
            .tail
            .as_ref()
            .map(|tail| {
                tail.as_str()
                    .split('/')
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let cosine = |query: Option<&Prepared>, document: Option<&Prepared>| match (query, document)
        {
            (Some(query), Some(document)) => query.cosine(document).expect("one routing fit"),
            // Empty vectors have no overlap, not a score floor or fallback destination.
            _ => 0.0,
        };
        let mut scored: Vec<ScoredNode> = Vec::with_capacity(nodes.len());
        let mut ancestors: Vec<usize> = Vec::new();
        for node in nodes {
            while ancestors
                .last()
                .is_some_and(|index| !below(node.path.as_str(), scored[*index].pick.topic.as_str()))
            {
                ancestors.pop();
            }
            let parent = ancestors.last().copied();
            let document = frequency.prepare(&node.terms);
            let path = frequency.prepare(&TermCounts::from_texts([node.path.as_str()]));
            let scores = [
                cosine(statement.as_ref(), document.as_ref()),
                cosine(origin.as_ref(), document.as_ref()),
                cosine(statement.as_ref(), path.as_ref()),
            ];
            let matched = matched_components(&node.path, &tail);
            let index = scored.len();
            if let Some(parent) = parent {
                scored[parent].children.push(index);
            }
            scored.push(ScoredNode {
                pick: Pick {
                    node: node.id,
                    topic: node.path,
                },
                scores,
                document,
                parent,
                children: Vec::new(),
                matched,
                reach: matched,
            });
            ancestors.push(index);
        }
        // Parents precede their children, so one reverse pass lifts each
        // subtree's strongest match.
        for index in (0..scored.len()).rev() {
            if let Some(parent) = scored[index].parent {
                let reach = scored[index].reach;
                scored[parent].reach = scored[parent].reach.max(reach);
            }
        }
        Self {
            nodes: scored,
            statement,
            tiers,
            tail,
        }
    }

    /// Route under one authority filter. Disjoint roots that stay unresolved,
    /// or an empty candidate set, land on the scope's declared unmatched topic
    /// outside scoring; without one the route is undecided and `None`.
    #[must_use]
    pub fn pick(&self, scope: &Scope) -> Option<Decision> {
        let candidates = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                (scope.roots.admits(&node.pick.topic)
                    && node
                        .parent
                        .is_none_or(|parent| !scope.roots.admits(&self.nodes[parent].pick.topic)))
                .then_some(index)
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return scope
                .unmatched
                .as_ref()
                .map(|topic| self.unmatched(topic, RouteTier::Statement, &[]));
        }
        let (mut current, mut tier) = match self.choose(candidates, None) {
            Choice::Chosen(index, tier) => (index, tier),
            Choice::Tied(roots, tier) => {
                return scope
                    .unmatched
                    .as_ref()
                    .map(|topic| self.unmatched(topic, tier, &roots));
            }
            Choice::Equal(_) | Choice::Below(_) => unreachable!("roots have no parent to equal"),
        };
        let mut semantic = (tier < RouteTier::Path).then_some(current);
        let stop = loop {
            let children = self.nodes[current].children.clone();
            if children.is_empty() {
                break RouteStop::Leaf;
            }
            match self.choose(children, Some(current)) {
                Choice::Chosen(index, needed) => {
                    tier = tier.max(needed);
                    current = index;
                    if needed < RouteTier::Path {
                        semantic = Some(current);
                    }
                }
                Choice::Below(needed) => {
                    tier = tier.max(needed);
                    break RouteStop::ChildrenBelow;
                }
                Choice::Equal(needed) => {
                    tier = tier.max(needed);
                    break RouteStop::ChildrenEqual;
                }
                Choice::Tied(_, needed) => {
                    tier = tier.max(needed);
                    break RouteStop::ChildrenTied;
                }
            }
        };
        let node = &self.nodes[current];
        let (destination, structure) = self.resolve(current, semantic);
        Some(Decision {
            target: Target::Node(node.pick.clone()),
            destination_node: self.node_at(&destination),
            destination,
            tier,
            stop,
            overlap: node.scores[..=tier as usize]
                .iter()
                .copied()
                .fold(0.0, f64::max),
            residual: self.residual(node.document.as_ref()),
            structure,
        })
    }

    /// Land on the declared unmatched `topic` after a pick's destination was
    /// refused downstream, composing the authored tail there exactly as
    /// [`Self::pick`] would.
    #[must_use]
    pub fn unmatched_at(&self, topic: &Topic) -> Decision {
        self.unmatched(topic, RouteTier::Statement, &[])
    }

    fn unmatched(&self, topic: &Topic, tier: RouteTier, roots: &[usize]) -> Decision {
        let (destination, structure) = if self.tail.is_empty() {
            (topic.clone(), None)
        } else {
            let (destination, composed_overlap) = fold(topic, &self.tail);
            let reach = roots
                .iter()
                .map(|index| self.nodes[*index].reach)
                .max()
                .unwrap_or(0);
            let connection = if reach > 0 {
                RouteConnection::Ambiguous {
                    candidates: roots
                        .iter()
                        .flat_map(|root| self.subtree(*root))
                        .filter(|index| self.nodes[*index].matched == reach)
                        .map(|index| self.nodes[index].pick.topic.clone())
                        .collect(),
                }
            } else {
                RouteConnection::Unresolved
            };
            let structure = Structure {
                prefix: None,
                matched: None,
                matched_components: 0,
                connection,
                composed_overlap,
                alternative: self.alternative(&destination),
            };
            (destination, Some(structure))
        };
        Decision {
            target: Target::Unmatched(topic.clone()),
            destination_node: self.node_at(&destination),
            destination,
            tier,
            stop: RouteStop::Unmatched,
            overlap: 0.0,
            residual: self.residual(None),
            structure,
        }
    }

    /// Compose the authored tail once below the explained node: reuse the one
    /// existing match inside its subtree, otherwise fold boundary overlap.
    fn resolve(&self, current: usize, semantic: Option<usize>) -> (Topic, Option<Structure>) {
        let prefix = &self.nodes[current].pick.topic;
        if self.tail.is_empty() {
            return (prefix.clone(), None);
        }
        let mut structure = Structure {
            prefix: semantic.map(|index| self.nodes[index].pick.topic.clone()),
            matched: None,
            matched_components: 0,
            connection: RouteConnection::Unresolved,
            composed_overlap: 0,
            alternative: None,
        };
        let reach = self.nodes[current].reach;
        if reach > 0 {
            let matches = self
                .subtree(current)
                .filter(|index| self.nodes[*index].matched == reach)
                .collect::<Vec<_>>();
            if let [only] = matches.as_slice() {
                let matched = &self.nodes[*only];
                let destination = append(&matched.pick.topic, &self.tail[reach..]);
                let covered = matched.pick.topic.components().count() - reach;
                let known =
                    semantic.map_or(0, |index| self.nodes[index].pick.topic.components().count());
                structure.connection = if known > covered {
                    RouteConnection::Overlap {
                        overlap: known - covered,
                    }
                } else if known == covered {
                    RouteConnection::Meet
                } else {
                    RouteConnection::Gap {
                        components: matched
                            .pick
                            .topic
                            .components()
                            .skip(known)
                            .take(covered - known)
                            .map(str::to_owned)
                            .collect(),
                    }
                };
                structure.matched = Some(matched.pick.clone());
                structure.matched_components = reach;
                structure.alternative = self.alternative(&destination);
                return (destination, Some(structure));
            }
            structure.connection = RouteConnection::Ambiguous {
                candidates: matches
                    .into_iter()
                    .map(|index| self.nodes[index].pick.topic.clone())
                    .collect(),
            };
        }
        let (destination, composed_overlap) = fold(prefix, &self.tail);
        structure.composed_overlap = composed_overlap;
        structure.alternative = self.alternative(&destination);
        (destination, Some(structure))
    }

    /// The existing root-anchored path spelled like the whole tail, unless the
    /// decision chose it.
    fn alternative(&self, destination: &Topic) -> Option<Topic> {
        if self.tail.is_empty() {
            return None;
        }
        let anchored: Topic = format!("/{}", self.tail.join("/")).parse().ok()?;
        (anchored != *destination && self.node_at(&anchored).is_some()).then_some(anchored)
    }

    fn node_at(&self, topic: &Topic) -> Option<i64> {
        self.nodes
            .iter()
            .find(|node| node.pick.topic == *topic)
            .map(|node| node.pick.node)
    }

    /// Indices of one node and every descendant.
    fn subtree(&self, root: usize) -> impl Iterator<Item = usize> + '_ {
        let mut pending = vec![root];
        std::iter::from_fn(move || {
            let index = pending.pop()?;
            pending.extend(self.nodes[index].children.iter().rev().copied());
            Some(index)
        })
    }

    /// Decide one level: a unique maximum at the earliest tier wins; a tie
    /// narrows the pool for the next tier; without a parent every tier only
    /// discriminates, with one a candidate must first strictly improve on it.
    /// At the path tier the authored tail first narrows the pool to the
    /// subtrees matching it best; path names are compared only after that.
    fn choose(&self, mut pool: Vec<usize>, parent: Option<usize>) -> Choice {
        let mut improved = parent.is_none();
        for &tier in &self.tiers {
            if tier == RouteTier::Path {
                let reach = pool
                    .iter()
                    .map(|index| self.nodes[*index].reach)
                    .max()
                    .unwrap_or(0);
                if reach > 0 {
                    pool.retain(|index| self.nodes[*index].reach == reach);
                    if let [only] = pool.as_slice() {
                        return Choice::Chosen(*only, tier);
                    }
                }
            }
            let at = tier as usize;
            let score = |index: &usize| self.nodes[*index].scores[at];
            let max = pool.iter().map(score).fold(f64::NEG_INFINITY, f64::max);
            match parent.filter(|_| !improved) {
                Some(parent) if max > self.nodes[parent].scores[at] => {
                    pool.retain(|index| score(index) == max);
                    improved = true;
                }
                Some(parent) => {
                    let base = self.nodes[parent].scores[at];
                    pool.retain(|index| score(index) == base);
                    if pool.is_empty() {
                        return Choice::Below(tier);
                    }
                    continue;
                }
                None => pool.retain(|index| score(index) == max),
            }
            if let [only] = pool.as_slice() {
                return Choice::Chosen(*only, tier);
            }
        }
        let tier = *self.tiers.last().expect("at least the statement tier");
        if improved {
            Choice::Tied(pool, tier)
        } else {
            Choice::Equal(tier)
        }
    }

    fn residual(&self, document: Option<&Prepared>) -> Vec<String> {
        self.statement
            .as_ref()
            .map(|statement| statement.strongest_absent(document, RESIDUAL_TERMS))
            .unwrap_or_default()
    }
}

/// Leading tail components `path` ends with, longest first; zero without a
/// contiguous leaf-up match.
fn matched_components(path: &Topic, tail: &[String]) -> usize {
    let components = path.components().collect::<Vec<_>>();
    boundary_overlap(&components, tail)
}

/// Compose `tail` below `prefix`, folding the longest exact run of boundary
/// components the prefix already ends with; a private owner never folds.
fn fold(prefix: &Topic, tail: &[String]) -> (Topic, usize) {
    let components = prefix
        .components()
        .skip(usize::from(prefix.private_owner().is_some()))
        .collect::<Vec<_>>();
    let overlap = boundary_overlap(&components, tail);
    (append(prefix, &tail[overlap..]), overlap)
}

/// Longest `k` such that `components` ends with the first `k` of `tail`.
fn boundary_overlap(components: &[&str], tail: &[String]) -> usize {
    (1..=tail.len().min(components.len()))
        .rev()
        .find(|&k| components[components.len() - k..] == tail[..k])
        .unwrap_or(0)
}

fn append(topic: &Topic, components: &[String]) -> Topic {
    components.iter().fold(topic.clone(), |topic, component| {
        topic
            .child(component)
            .expect("an admitted tail spells valid components")
    })
}

fn below(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|tail| tail.starts_with('/'))
}

#[cfg(test)]
mod tests;
