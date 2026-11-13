//! Pure admission of the oh-my-pi launch configuration.

use serde::de::{Deserializer, Error as _};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

use crate::Effort;

/// oh-my-pi's compiled model selection, matched fuzzily by omp itself: an
/// OpenAI model served through omp's Codex provider.
pub const DEFAULT_OMP_MODEL: &str = "gpt-5.6-sol";
/// Input tokens beyond which OpenAI bills a request at long-context rates.
const OPENAI_LONG_CONTEXT_TOKENS: u64 = 272_000;
/// The window of the smallest Claude a lane runs: Claude Haiku 4.5 holds
/// 200,000 tokens where every Claude from 4.6 on holds a million. Anthropic
/// bills that million at one rate, so the boundary a Claude must not cross
/// is its own window, not a price.
const CLAUDE_HAIKU_CONTEXT_TOKENS: u64 = 200_000;
/// The least a Claude has been seen to compact short of the window it is
/// told, rounded down: 69 lane compactions against 200,000 landed between
/// 166,418 and 175,600 tokens.
const CLAUDE_LEAST_SHORTFALL_TOKENS: u64 = 20_000;
/// The window oh-my-pi is asked to compact against for a Claude Haiku, or
/// for a spelling that names no model spice knows: what a 200,000-token
/// window tolerates, since omp clamps a threshold beneath the model's window
/// and past that compacts only once a request has overflowed.
pub const DEFAULT_OMP_AUTO_COMPACT_WINDOW_TOKENS: u64 =
    CLAUDE_HAIKU_CONTEXT_TOKENS + CLAUDE_LEAST_SHORTFALL_TOKENS;
/// The window oh-my-pi is asked to compact against for a million-token
/// model, OpenAI's or Anthropic's: OpenAI's boundary, the window omp's own
/// registry declares for the Codex-served GPT models whose long-context
/// price it does know, so a Claude compacts where a GPT does. Left to itself
/// omp compacts against the model's full window, 850,000 tokens into a
/// million-token model.
pub const DEFAULT_OMP_LONG_CONTEXT_AUTO_COMPACT_WINDOW_TOKENS: u64 = OPENAI_LONG_CONTEXT_TOKENS;
/// Provider prefixes omp resolves ahead of the slash in a `provider/model`
/// spelling, and bare model families it matches fuzzily, that reach OpenAI.
const OMP_OPENAI_PROVIDERS: [&str; 2] = ["openai", "openai-codex"];
const OMP_OPENAI_FAMILIES: [&str; 2] = ["gpt", "codex"];
/// The provider prefix and bare families that reach Anthropic, every one a
/// million-token model unless the spelling names a Haiku.
const OMP_ANTHROPIC_PROVIDER: &str = "anthropic";
const OMP_ANTHROPIC_FAMILIES: [&str; 4] = ["claude", "opus", "sonnet", "fable"];
const OMP_ANTHROPIC_HAIKU: &str = "haiku";
/// The share of a window omp holds back for the reply, as omp computes it.
const OMP_RESERVE_PERCENT: u64 = 15;
/// The least reserve omp holds back, whatever the window.
const OMP_RESERVE_FLOOR_TOKENS: u64 = 16_384;

/// A configured executable spelling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Binary(String);

impl Binary {
    /// Exact executable spelling supplied by tracked policy.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn new(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for Binary {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        admitted_string(deserializer, "vendor binary").map(Self)
    }
}

impl Serialize for Binary {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LaunchValue(String);

impl LaunchValue {
    fn as_str(&self) -> &str {
        &self.0
    }

    fn new(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for LaunchValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        admitted_string(deserializer, "vendor launch value").map(Self)
    }
}

impl Serialize for LaunchValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

fn admitted_string<'de, D: Deserializer<'de>>(
    deserializer: D,
    label: &str,
) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.trim().is_empty() {
        return Err(D::Error::custom(format!("{label} must not be empty")));
    }
    if value.contains('\0') {
        return Err(D::Error::custom(format!("{label} must not contain NUL")));
    }
    Ok(value)
}

/// Values the current oh-my-pi RPC launch dialect can honor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct OmpConfig {
    binary: Binary,
    model: LaunchValue,
    effort: Effort,
    fast_mode: bool,
    lsp: bool,
    #[serde(
        deserialize_with = "omp_window",
        skip_serializing_if = "Option::is_none"
    )]
    auto_compact_window_tokens: Option<u64>,
}

impl Default for OmpConfig {
    fn default() -> Self {
        Self {
            binary: Binary::new("omp"),
            model: LaunchValue::new(DEFAULT_OMP_MODEL),
            effort: Effort::High,
            fast_mode: false,
            lsp: true,
            auto_compact_window_tokens: None,
        }
    }
}

impl OmpConfig {
    /// Executable selected for oh-my-pi.
    #[must_use]
    pub const fn binary(&self) -> &Binary {
        &self.binary
    }

    /// Model spelling handed to omp, which resolves it against its own list.
    #[must_use]
    pub fn model(&self) -> &str {
        self.model.as_str()
    }

    /// Thinking level omp is launched with.
    #[must_use]
    pub const fn effort(&self) -> Effort {
        self.effort
    }

    /// Explicit oh-my-pi fast-mode disposition: its priority service tier.
    #[must_use]
    pub const fn fast_mode(&self) -> bool {
        self.fast_mode
    }

    /// Whether omp's own language-server tooling is admitted for this lane.
    ///
    /// On by default: the lane takes the tool and the language servers its
    /// worktree's `.omp/lsp.json` describes; policy declines with
    /// `lsp = false`. Nothing here names a server or a toolchain.
    #[must_use]
    pub const fn lsp(&self) -> bool {
        self.lsp
    }

    /// The window omp is asked to compact against: the one policy names, or
    /// else the boundary of the provider the configured model reaches.
    #[must_use]
    pub fn auto_compact_window_tokens(&self) -> u64 {
        self.auto_compact_window_tokens_for(self.model())
    }

    /// The window omp is asked to compact against when a seat selected
    /// `model` over the configured one: the window policy names still wins,
    /// and otherwise the boundary follows the model the seat really runs.
    #[must_use]
    pub fn auto_compact_window_tokens_for(&self, model: &str) -> u64 {
        self.auto_compact_window_tokens
            .unwrap_or_else(|| omp_default_auto_compact_window_tokens(model))
    }

    /// The fixed token limit omp is handed, derived from the window the way
    /// omp derives its own threshold from a model's window.
    #[must_use]
    pub fn auto_compact_threshold_tokens(&self) -> u64 {
        omp_auto_compact_threshold_tokens(self.auto_compact_window_tokens())
    }

    /// [`Self::auto_compact_threshold_tokens`] for the model a seat selected.
    #[must_use]
    pub fn auto_compact_threshold_tokens_for(&self, model: &str) -> u64 {
        omp_auto_compact_threshold_tokens(self.auto_compact_window_tokens_for(model))
    }
}

/// The launch configurations with landed Rust renderers.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    omp: OmpConfig,
}

impl Config {
    /// Admitted oh-my-pi launch values.
    #[must_use]
    pub const fn omp(&self) -> &OmpConfig {
        &self.omp
    }
}

fn omp_window<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    let value = Option::<u64>::deserialize(deserializer)?;
    if value.is_some_and(|window| omp_auto_compact_threshold_tokens(window) == 0) {
        return Err(D::Error::custom(
            "oh-my-pi auto-compaction window must exceed the reserve omp holds back",
        ));
    }
    Ok(value)
}

/// The window omp is asked to compact against when policy names none: the
/// long-context one for a spelling its fuzzy match carries to a million-token
/// model at OpenAI or Anthropic, or the smaller one for a Claude Haiku and
/// for a spelling that names no model spice knows, so an unfamiliar model
/// crosses no window.
#[must_use]
pub fn omp_default_auto_compact_window_tokens(model: &str) -> u64 {
    let spelling = model.trim().to_ascii_lowercase();
    let (provider, family) = match spelling.split_once('/') {
        Some((provider, family)) => (Some(provider), family),
        None => (None, spelling.as_str()),
    };
    let openai = match provider {
        Some(provider) => OMP_OPENAI_PROVIDERS.contains(&provider),
        None => OMP_OPENAI_FAMILIES
            .iter()
            .any(|known| family.starts_with(known)),
    };
    let anthropic = match provider {
        Some(provider) => provider == OMP_ANTHROPIC_PROVIDER,
        None => OMP_ANTHROPIC_FAMILIES
            .iter()
            .any(|known| family.starts_with(known)),
    };
    if openai || (anthropic && !family.contains(OMP_ANTHROPIC_HAIKU)) {
        DEFAULT_OMP_LONG_CONTEXT_AUTO_COMPACT_WINDOW_TOKENS
    } else {
        DEFAULT_OMP_AUTO_COMPACT_WINDOW_TOKENS
    }
}

/// Derive the threshold omp would compute for a window of this size: the
/// window less the larger of its proportional and floor reserves.
#[must_use]
pub const fn omp_auto_compact_threshold_tokens(window: u64) -> u64 {
    let hundreds = window / 100;
    let remainder = window % 100;
    let proportional = hundreds * OMP_RESERVE_PERCENT + remainder * OMP_RESERVE_PERCENT / 100;
    let reserve = if proportional > OMP_RESERVE_FLOOR_TOKENS {
        proportional
    } else {
        OMP_RESERVE_FLOOR_TOKENS
    };
    window.saturating_sub(reserve)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn omp(auto_compact_window_tokens: u64) -> Result<OmpConfig, serde_json::Error> {
        serde_json::from_value(serde_json::json!({
            "auto_compact_window_tokens": auto_compact_window_tokens,
        }))
    }

    #[test]
    fn omp_threshold_is_the_window_less_the_larger_reserve() {
        // The floor wins for windows under roughly 109,000 tokens.
        assert_eq!(omp_auto_compact_threshold_tokens(200_000), 170_000);
        assert_eq!(omp_auto_compact_threshold_tokens(1_000_000), 850_000);
        assert_eq!(omp_auto_compact_threshold_tokens(100_000), 83_616);
        assert_eq!(omp_auto_compact_threshold_tokens(16_385), 1);
        assert_eq!(omp_auto_compact_threshold_tokens(16_384), 0);
        assert_eq!(omp_auto_compact_threshold_tokens(0), 0);
        assert_eq!(
            omp_auto_compact_threshold_tokens(u64::MAX),
            u64::MAX - u64::MAX / 100 * 15 - u64::MAX % 100 * 15 / 100
        );
        assert_eq!(
            omp(16_385)
                .expect("one usable token admits")
                .auto_compact_threshold_tokens(),
            1
        );
        assert!(omp(16_384).is_err());
        assert_eq!(
            OmpConfig::default().auto_compact_threshold_tokens(),
            231_200
        );
    }

    #[test]
    fn omp_window_follows_the_model_the_spelling_reaches() {
        for spelling in [
            "gpt-5.6-sol",
            "GPT-5.2",
            "codex-mini",
            "openai/gpt-5.2",
            "openai-codex/gpt-5.6-sol",
            "openai/anything",
            "claude-opus-5",
            "opus",
            "Sonnet",
            "fable-5",
            "anthropic/claude-sonnet-5",
            "anthropic/anything",
        ] {
            assert_eq!(
                omp_default_auto_compact_window_tokens(spelling),
                272_000,
                "{spelling}"
            );
        }
        for spelling in [
            "claude-haiku-4-5",
            "haiku",
            "anthropic/claude-haiku-4-5",
            "gemini-2.5-pro",
            "google/gemini-2.5-pro",
            "",
        ] {
            assert_eq!(
                omp_default_auto_compact_window_tokens(spelling),
                220_000,
                "{spelling}"
            );
        }
        let sol: OmpConfig = serde_json::from_value(serde_json::json!({"model": "gpt-5.6-sol"}))
            .expect("an OpenAI model admits");
        assert_eq!(sol.auto_compact_window_tokens(), 272_000);
        assert_eq!(sol.auto_compact_threshold_tokens(), 231_200);
        assert_eq!(
            sol.auto_compact_threshold_tokens_for("claude-haiku-4-5"),
            187_000,
            "a seat's own model selection governs its window"
        );
        assert_eq!(
            OmpConfig::default().auto_compact_threshold_tokens_for("gpt-5.6-sol"),
            231_200
        );
        let pinned: OmpConfig = serde_json::from_value(
            serde_json::json!({"model": "gpt-5.6-sol", "auto_compact_window_tokens": 200_000}),
        )
        .expect("a named window admits");
        assert_eq!(pinned.auto_compact_window_tokens(), 200_000);
        assert_eq!(
            pinned.auto_compact_threshold_tokens_for("claude-haiku-4-5"),
            170_000,
            "a named window wins over every model selection"
        );
        let unset: OmpConfig =
            serde_json::from_value(serde_json::json!({"auto_compact_window_tokens": null}))
                .expect("a null window admits");
        assert_eq!(unset.auto_compact_window_tokens(), 272_000);
    }
}
