//! Read a durable filing proposal from the seat the tree forecasts. Where
//! neighbours were filed is not consulted. A seat that holds a described
//! topic is that topic; any other is named by the word that carries most
//! weight for it in the fit that formed it, the same way at each scale.
//! Numeric tree paths are display labels only.

use std::collections::BTreeMap;

use bus::TaskSnapshot;
use journal::TopicRoutingNode;
use topic::evidence::Prepared;
use topic::kept::Named;
use wire::{TaskStamp, Topic};

use super::current::{Generation, WindowHead};

pub(super) enum Member<'a> {
    Task(&'a WindowHead),
    Fixed(&'a TopicRoutingNode),
}

impl Generation {
    pub(super) fn member(&self, row: usize) -> Member<'_> {
        match self.fixed.get(row) {
            Some(node) => Member::Fixed(node),
            None => Member::Task(&self.window[row - self.fixed.len()]),
        }
    }
}

pub(in crate::board_command) struct Seat {
    /// None when no written word can name the seat.
    pub(in crate::board_command) destination: Option<Topic>,
    pub(super) path: Vec<usize>,
    pub(in crate::board_command) companions: Vec<TaskStamp>,
    pub(in crate::board_command) lead: u8,
}

/// `authored` is the arriving statement's own text, every field; `nodes` is
/// every public topic, whose spellings a proposal keeps.
pub(in crate::board_command) fn forecast(
    pool: &Generation,
    observed: &TaskSnapshot,
    arriving: &Prepared,
    authored: &[&str],
    nodes: &[TopicRoutingNode],
    measure_lead: bool,
) -> Seat {
    let tree = pool.tree();
    let position = tree.arrived();
    let forecast = tree.forecast(arriving);
    let held = |members: &[usize]| -> Vec<usize> {
        (members.iter().filter(|&&i| i != position))
            .map(|&i| tree.row_index(i))
            .collect()
    };
    // The base holds whatever joined no group; sharing it is not likeness.
    let (path, direct) = forecast
        .iter()
        .find(|(_, members)| members.contains(&position))
        .map(|(path, members)| match path.is_empty() {
            true => (Vec::new(), &[][..]),
            false => (path.clone(), members.as_slice()),
        })
        .expect("every arrival occupies one direct seat");
    let nfixed = direct.partition_point(|&at| at < pool.fixed.len());
    let companions = direct[nfixed..]
        .iter()
        .filter(|&&at| at != position)
        .map(|&at| {
            pool.window[tree.row_index(at) - pool.fixed.len()]
                .id
                .clone()
        })
        .collect();
    let (fixed, lead) = if direct.len() <= 1 || (!measure_lead && nfixed < 2) {
        (direct[..nfixed].first().map(|&at| &pool.fixed[at].path), 0)
    } else {
        let (mut inside, mut outside) = (0_f64, 0_f64);
        let mut best: Option<(f64, &Topic)> = None;
        let compare = |at| measure_lead || direct[..nfixed].binary_search(&at).is_ok();
        for (at, score) in tree.likeness(arriving, &forecast, &path, compare) {
            if direct.binary_search(&at).is_err() {
                outside = outside.max(score);
                continue;
            }
            inside = inside.max(score);
            if let Some(node) = pool.fixed.get(at)
                && best.is_none_or(|(s, path)| score > s || (score == s && node.path < *path))
            {
                best = Some((score, &node.path));
            }
        }
        let scale = f64::from(board::team_policy::FULL_PLACEMENT_LEAD);
        (
            best.map(|(_, path)| path),
            (scale * (inside - outside)) as u8,
        )
    };
    let destination = fixed.cloned().or_else(|| {
        // Each group is spelled by its own members, the arrival among them.
        let texts = |members: &[usize]| {
            let mut texts = authored_texts(pool, observed, &held(members));
            texts.extend_from_slice(authored);
            texts
        };
        named_seat(&tree.seat_words(arriving, &forecast), texts, nodes)
    });
    Seat {
        destination,
        path,
        companions,
        lead,
    }
}

/// A seat no fixed point occupies: the word of each group down its path,
/// outermost first, never spelled a second way beside a topic it already
/// names. No word names two levels of one topic, and a level no written
/// word can name ends the topic at the level above it. A statement nothing
/// held resembles is a group of its own, so its own heaviest word starts a
/// topic.
fn named_seat<'t>(
    depths: &[Named<'_>],
    texts: impl Fn(&[usize]) -> Vec<&'t str>,
    nodes: &[TopicRoutingNode],
) -> Option<Topic> {
    let (first, deeper) = depths.split_first()?;
    let mut topic = named(&first.words, &texts(&first.members), |form, word| {
        Topic::root(twin(nodes, None, form, word).unwrap_or(word))
            .ok()
            .filter(|topic| !wire::is_oops_topic(topic))
    })?;
    for group in deeper {
        let Some(child) = named(&group.words, &texts(&group.members), |form, word| {
            let word = twin(nodes, Some(&topic), form, word).unwrap_or(word);
            (topic.components().all(|taken| taken != word))
                .then(|| topic.child(word).ok())
                .flatten()
        }) else {
            break;
        };
        topic = child;
    }
    Some(topic)
}

/// Another spelling of `form` that names a topic directly beneath `above`,
/// or at the top level, where `word` names none: the first in path order.
/// Spelling the word a second way would split one group across twin topics.
fn twin<'n>(
    nodes: &'n [TopicRoutingNode],
    above: Option<&Topic>,
    form: &str,
    word: &str,
) -> Option<&'n str> {
    let above = above.map_or("", Topic::as_str);
    let mut first: Option<&str> = None;
    for (parent, name) in (nodes.iter())
        .filter(|node| node.path.private_owner().is_none())
        .filter_map(|node| node.path.as_str().rsplit_once('/'))
    {
        if parent != above {
            continue;
        }
        if name == word {
            return None;
        }
        if topic::evidence::names_term(name, form) {
            first = Some(first.map_or(name, |first| first.min(name)));
        }
    }
    first
}

/// Every authored field of the rows' current text.
fn authored_texts<'a>(
    pool: &'a Generation,
    observed: &'a TaskSnapshot,
    rows: &[usize],
) -> Vec<&'a str> {
    let mut texts = Vec::new();
    for &row in rows {
        match pool.member(row) {
            Member::Fixed(node) => texts.push(node.description.as_str()),
            Member::Task(held) => {
                let head = observed
                    .task_heads(&held.id)
                    .expect("admitted member")
                    .statement()
                    .body();
                texts.extend([head.title.as_str(), head.pitch.as_str()]);
                texts.extend(head.description.iter().map(String::as_str));
                texts.extend(head.term.iter().map(String::as_str));
            }
        }
    }
    texts
}

/// The topic of the heaviest word that some member wrote and that makes one,
/// given the word's matching form and its written spelling.
fn named(
    words: &[&str],
    texts: &[&str],
    topic: impl Fn(&str, &str) -> Option<Topic>,
) -> Option<Topic> {
    (words.iter())
        .filter(|word| label_term(word))
        .find_map(|word| topic(word, &written(word, texts)?))
}

fn label_term(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// The word a topic is named by. The engine weighs a matching form, which
/// folds plurals (`canva` for `canvas`); a durable name is the form itself when
/// a member wrote it, otherwise its most common written spelling. A form
/// nobody wrote names nothing.
fn written(form: &str, texts: &[&str]) -> Option<String> {
    let mut spellings = BTreeMap::<String, usize>::new();
    for text in texts {
        for word in topic::evidence::spellings(text, form) {
            *spellings.entry(word).or_default() += 1;
        }
    }
    if spellings.contains_key(form) {
        return Some(form.to_owned());
    }
    spellings
        .into_iter()
        .filter(|(word, _)| label_term(word))
        .max_by(|(a, n), (b, m)| n.cmp(m).then_with(|| b.cmp(a)))
        .map(|(word, _)| word)
}

#[cfg(test)]
mod tests {
    use journal::TopicRoutingNode;
    use wire::Topic;

    use super::{Named, named_seat, written};

    fn level(words: &[&'static str]) -> Named<'static> {
        Named {
            members: Vec::new(),
            words: words.to_vec(),
        }
    }

    #[test]
    fn a_word_that_already_names_a_topic_there_keeps_that_spelling() {
        let name = |wrote: &'static str, depths: &[Named<'_>], topics: &[&str]| {
            let nodes: Vec<TopicRoutingNode> = (topics.iter().zip(1..))
                .map(|(path, id)| TopicRoutingNode {
                    id,
                    path: path.parse::<Topic>().expect("canonical topic"),
                    description: String::new(),
                })
                .collect();
            named_seat(depths, |_| vec![wrote, "Tile storage"], &nodes)
                .map(|topic| topic.as_str().to_owned())
        };
        let api = [level(&["api"])];
        assert_eq!(name("API", &api, &["/apis"]).as_deref(), Some("/apis"));
        assert_eq!(name("APIs", &api, &["/api"]).as_deref(), Some("/api"));
        assert_eq!(name("api", &api, &["/apis"]), None);
        let (singular, plural) = ("Resize the window", "Tile windows");
        let window = [level(&["window"])];
        assert_eq!(name(singular, &window, &[]).as_deref(), Some("/window"));
        assert_eq!(
            name(singular, &window, &["/windows"]).as_deref(),
            Some("/windows"),
            "the plural already names the topic this word would name"
        );
        assert_eq!(
            name(plural, &window, &["/window"]).as_deref(),
            Some("/window"),
            "the singular already names the topic this word would name"
        );
        assert_eq!(
            name(plural, &window, &["/window", "/windows"]).as_deref(),
            Some("/windows"),
            "where the written spelling names a topic, another spelling beside it changes nothing"
        );
        assert_eq!(
            name(singular, &window, &["/storage/windows", "/window-manager"]).as_deref(),
            Some("/window"),
            "a twin elsewhere, or a longer name that contains the word, is no spelling of it here"
        );
        assert_eq!(
            name(
                singular,
                &[level(&["storage"]), level(&["window"])],
                &["/storage/windows"]
            )
            .as_deref(),
            Some("/storage/windows"),
            "a deeper segment keeps the spelling in use beneath its own parent"
        );
        assert_eq!(
            name(singular, &[level(&["cache"])], &["/caches"]),
            None,
            "a topic's spelling does not name a group none of whose members wrote the word"
        );
    }

    #[test]
    fn a_seat_is_named_by_one_written_word_for_each_level_of_its_path() {
        let name = |depths: &[Named<'_>]| {
            named_seat(depths, |_| vec!["Storage disk latency budget"], &[])
                .map(|topic| topic.as_str().to_owned())
        };
        assert_eq!(
            name(&[level(&["storage"]), level(&["disk"]), level(&["latency"])]).as_deref(),
            Some("/storage/disk/latency"),
            "every group on the path contributes its segment, outermost first"
        );
        assert_eq!(
            name(&[
                level(&["storage"]),
                level(&["storage", "disk"]),
                level(&["disk", "storage", "latency"]),
            ])
            .as_deref(),
            Some("/storage/disk/latency"),
            "a word that names a level above yields to the group's next word"
        );
        assert_eq!(
            name(&[level(&["storage"]), level(&["cache"]), level(&["latency"])]).as_deref(),
            Some("/storage"),
            "a deeper word never attaches beneath a group it does not sit in"
        );
    }

    #[test]
    fn a_topic_is_named_by_a_written_word_never_an_unwritten_matching_form() {
        let name = |form, texts: &[&str]| written(form, texts);
        assert_eq!(
            name("canva", &["Pan the Canvas", "Zoom canvas tiles"]).as_deref(),
            Some("canvas"),
            "a word that only looks plural keeps its spelling"
        );
        assert_eq!(
            name("rack", &["Stack racks", "Mirror the rack", "Sort racks"]).as_deref(),
            Some("rack"),
            "the singular names the topic once any member wrote it"
        );
        assert_eq!(
            name("patche", &["Apply patches", "Drop the patches"]).as_deref(),
            Some("patches"),
            "a fold nobody wrote yields to the spelling they did write"
        );
        assert_eq!(name("group", &["Anything else"]), None);
    }
}
