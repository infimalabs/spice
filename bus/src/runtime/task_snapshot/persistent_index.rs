use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

/// One level's batch, absent while the counter's bit is clear.
type Level<K, V> = Option<Arc<HashMap<K, V>>>;

/// Immutable logarithmic map: each occupied level owns a successively older
/// batch, and inserting carries through occupied levels like a binary counter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PersistentIndex<K: Eq + Hash, V> {
    levels: Arc<[Level<K, V>]>,
}

impl<K: Eq + Hash, V> Default for PersistentIndex<K, V> {
    fn default() -> Self {
        Self {
            levels: Arc::from([]),
        }
    }
}

impl<K, V> PersistentIndex<K, V>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    pub(super) fn pushed(&self, mut carry: HashMap<K, V>) -> Self {
        let mut levels = self.levels.to_vec();
        let mut level = 0;
        loop {
            if level == levels.len() {
                levels.push(Some(Arc::new(carry)));
                break;
            }
            let Some(existing) = levels[level].take() else {
                levels[level] = Some(Arc::new(carry));
                break;
            };
            let mut merged = HashMap::with_capacity(existing.len() + carry.len());
            merged.extend(
                existing
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
            // Lower levels are newer, so the carried batch wins on a repeated key.
            merged.extend(carry);
            carry = merged;
            level += 1;
        }
        Self {
            levels: Arc::from(levels),
        }
    }

    pub(super) fn get(&self, key: &K) -> Option<&V> {
        self.levels
            .iter()
            .find_map(|level| level.as_ref().and_then(|entries| entries.get(key)))
    }
}
