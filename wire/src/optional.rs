//! Canonical decoding for optional wire fields.

use serde::{Deserialize, Deserializer};

/// Decode a field that is present and wrap its value in `Some`.
///
/// DTO fields pair this with `default` and `skip_serializing_if`: omission
/// becomes `None`, a present value becomes `Some`, and `null` is refused by
/// `T`'s decoder instead of becoming a second spelling of absence.
pub(crate) fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// Decode one explicitly carried optional value.
///
/// Pair this with an `Option<T>` field that has semantic absence but whose
/// field is mandatory in the current wire shape. Omission is refused; `null`
/// decodes as `None`.
pub(crate) fn required<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}
