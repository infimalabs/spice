//! The model provider a lane answers with, read from the model its seat
//! recorded, and the emblem and label a composer paints for it.
//!
//! omp is the only harness, so naming it says nothing; the provider and model
//! say which architecture answers.

/// Model provider family with a known name and glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Anthropic's Claude models.
    Anthropic,
    /// OpenAI's GPT models.
    OpenAi,
}

impl Provider {
    /// The provider's name as an operator reads it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::OpenAi => "OpenAI",
        }
    }

    /// Stable lower-case key for styling and data attributes.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
        }
    }
}

/// Provider behind an omp model spelling.
///
/// omp names a model as `provider/model` or by the bare id it resolves
/// itself; both forms carry the family, and any other family stays unnamed.
#[must_use]
pub fn model_provider(model: &str) -> Option<Provider> {
    match model.split_once('/') {
        Some(("anthropic", _)) => Some(Provider::Anthropic),
        Some(("openai" | "openai-codex", _)) => Some(Provider::OpenAi),
        Some(_) => None,
        None if model.starts_with("claude") => Some(Provider::Anthropic),
        None if model.starts_with("gpt-") || model.starts_with("codex") => Some(Provider::OpenAi),
        None => None,
    }
}

/// The model a seat recorded as it reads beside its provider's emblem: a
/// known provider's prefix is dropped because the emblem already names it,
/// any other model stays as recorded, and an unrecorded model is nothing.
#[must_use]
pub fn model_name(model: Option<&str>) -> Option<&str> {
    let model = model.filter(|model| !model.is_empty())?;
    Some(match model_provider(model) {
        Some(_) => model.split_once('/').map_or(model, |(_, id)| id),
        None => model,
    })
}

/// Operator label for a recorded model: `Provider / model` for a known
/// provider, the model as recorded for any other, and nothing when the seat
/// recorded no model.
///
/// A known provider's prefix is not repeated after its name, so
/// `anthropic/claude-opus-5` and `claude-opus-5` read alike.
#[must_use]
pub fn model_label(model: Option<&str>) -> Option<String> {
    let name = model_name(model)?;
    Some(model.and_then(model_provider).map_or_else(
        || name.to_owned(),
        |provider| format!("{} / {name}", provider.name()),
    ))
}

/// What a display names when the seat recorded no model, rather than
/// borrowing the harness or leaving the label empty.
pub const MODEL_UNRECORDED: &str = "model unrecorded";

/// [`model_label`] for display, naming an unrecorded model outright.
#[must_use]
pub fn model_display(model: Option<&str>) -> String {
    model_label(model).unwrap_or_else(|| MODEL_UNRECORDED.to_owned())
}

/// What a composer paints in its emblem position.
///
/// The emblem says which model and architecture answers, not which harness
/// drives it. A model of a known provider wears that provider's glyph with
/// omp's pi shrunk to a badge over its corner; a model of no known provider,
/// or a seat that recorded none, keeps the pi as its emblem with no badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emblem {
    /// The provider's glyph, badged with omp's pi.
    Provider(Provider),
    /// omp's pi alone.
    Pi,
}

/// Emblem for a composer, from the model its seat recorded.
#[must_use]
pub fn emblem(model: Option<&str>) -> Emblem {
    model
        .and_then(model_provider)
        .map_or(Emblem::Pi, Emblem::Provider)
}

#[cfg(test)]
mod tests {
    use super::{Emblem, Provider, emblem, model_display, model_label, model_name, model_provider};

    #[test]
    fn a_provider_is_named_from_either_model_spelling() {
        for model in [
            "anthropic/claude-opus-5",
            "claude-opus-5",
            "claude-sonnet-5",
        ] {
            assert_eq!(model_provider(model), Some(Provider::Anthropic), "{model}");
        }
        for model in [
            "openai/gpt-5.3",
            "openai-codex/gpt-5.3-codex",
            "gpt-5.3-codex",
            "codex-mini-latest",
        ] {
            assert_eq!(model_provider(model), Some(Provider::OpenAi), "{model}");
        }
        for model in [
            "",
            "google/gemini-3-pro",
            "gemini-3-pro",
            "Claude-opus-5",
            "/claude",
        ] {
            assert_eq!(model_provider(model), None, "{model}");
        }
    }

    #[test]
    fn a_label_names_provider_and_model_or_the_model_alone() {
        assert_eq!(
            model_label(Some("anthropic/claude-opus-5")).as_deref(),
            Some("Anthropic / claude-opus-5")
        );
        assert_eq!(
            model_label(Some("claude-opus-5")).as_deref(),
            Some("Anthropic / claude-opus-5")
        );
        assert_eq!(
            model_label(Some("openai-codex/gpt-5.3-codex")).as_deref(),
            Some("OpenAI / gpt-5.3-codex")
        );
        assert_eq!(
            model_label(Some("google/gemini-3-pro")).as_deref(),
            Some("google/gemini-3-pro")
        );
        assert_eq!(model_label(Some("")), None);
        assert_eq!(model_label(None), None);
        assert_eq!(model_display(None), "model unrecorded");
    }

    #[test]
    fn a_name_drops_only_a_known_providers_prefix() {
        assert_eq!(
            model_name(Some("anthropic/claude-opus-5")),
            Some("claude-opus-5")
        );
        assert_eq!(model_name(Some("claude-opus-5")), Some("claude-opus-5"));
        assert_eq!(
            model_name(Some("openai-codex/gpt-5.3-codex")),
            Some("gpt-5.3-codex")
        );
        assert_eq!(
            model_name(Some("google/gemini-3-pro")),
            Some("google/gemini-3-pro")
        );
        assert_eq!(model_name(Some("")), None);
        assert_eq!(model_name(None), None);
    }

    #[test]
    fn the_emblem_is_the_provider_glyph_or_the_pi_alone() {
        assert_eq!(
            emblem(Some("anthropic/claude-opus-5")),
            Emblem::Provider(Provider::Anthropic)
        );
        assert_eq!(
            emblem(Some("codex-mini-latest")),
            Emblem::Provider(Provider::OpenAi)
        );
        for model in [Some("gemini-3-pro"), Some(""), None] {
            assert_eq!(emblem(model), Emblem::Pi, "{model:?}");
        }
    }
}
