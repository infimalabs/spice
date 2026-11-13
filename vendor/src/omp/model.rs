//! OMP model spelling resolution and launch-time selection.

use super::{
    EFFORT_REQUEST_ID, MODEL_REQUEST_ID, set_fast_mode_request, set_model_request,
    set_thinking_level_request,
};

/// The model, reasoning effort, and fast-mode disposition a launch selects
/// over the command stream once OMP has listed the models it serves.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelSelection {
    spelling: String,
    effort: String,
    fast_mode: bool,
}

impl ModelSelection {
    /// Select the model `spelling` names at `effort`, fast or not.
    #[must_use]
    pub fn new(spelling: &str, effort: crate::launch::Effort, fast_mode: bool) -> Self {
        Self {
            spelling: spelling.to_owned(),
            effort: effort.as_str().to_owned(),
            fast_mode,
        }
    }

    /// The model as the lane spells it: `provider/id` or a bare identity.
    #[must_use]
    pub fn spelling(&self) -> &str {
        &self.spelling
    }

    /// `set_model` for the one served model this spelling names, then
    /// `set_thinking_level` with the lane's effort, then `set_fast_mode` with
    /// the lane's disposition.
    ///
    /// A slash qualifies the spelling as `provider/id`; it never falls through
    /// to a served raw identity containing the whole spelling. A slashless
    /// spelling matches a unique raw identity. Comparisons ignore ASCII case,
    /// as OMP's own selector does. The effort follows the model because OMP's
    /// `set_model` replaces the thinking level with the new model's default;
    /// stating it next is also what replaces the level a resumed session
    /// restores. Fast mode follows the model because OMP states it for the
    /// current model's tier family only, and the launch states no tier: omp's
    /// settings or a resumed session's record would otherwise stand. The
    /// opening admits only OMP's exact current no-tier refusal under
    /// [`listen::OMP_FAST_REQUEST_ID`]; another unsuccessful response closes
    /// before the first prompt.
    ///
    /// # Errors
    ///
    /// Refuses a spelling no served model answers to, and one several served
    /// models answer to, rather than run on a model the lane did not name.
    pub fn request(&self, served: &[listen::ServedModel]) -> Result<Vec<u8>, SelectionError> {
        let model = resolve_model(&self.spelling, served)?;
        let mut bytes = set_model_request(MODEL_REQUEST_ID, &model.provider, &model.id);
        bytes.extend(set_thinking_level_request(EFFORT_REQUEST_ID, &self.effort));
        bytes.extend(set_fast_mode_request(
            listen::OMP_FAST_REQUEST_ID,
            self.fast_mode,
        ));
        Ok(bytes)
    }
}

/// Resolve one operator model spelling against the models OMP currently
/// serves.
///
/// A qualified spelling matches only its exact provider and remaining model
/// identity. A bare spelling matches only when one served provider carries
/// that identity.
///
/// # Errors
///
/// Refuses an absent spelling and a bare identity shared by several providers.
pub fn resolve_model<'a>(
    spelling: &str,
    served: &'a [listen::ServedModel],
) -> Result<&'a listen::ServedModel, SelectionError> {
    let normalized = spelling.trim();
    let qualified = normalized.split_once('/');
    let mut named = served.iter().filter(|model| match qualified {
        Some((provider, id)) => {
            model.provider.eq_ignore_ascii_case(provider) && model.id.eq_ignore_ascii_case(id)
        }
        None => model.id.eq_ignore_ascii_case(normalized),
    });
    let Some(model) = named.next() else {
        return Err(SelectionError::Unserved {
            spelling: spelling.to_owned(),
        });
    };
    let others: Vec<String> = named
        .map(|other| format!("{}/{}", other.provider, other.id))
        .collect();
    if !others.is_empty() {
        let mut candidates = vec![format!("{}/{}", model.provider, model.id)];
        candidates.extend(others);
        return Err(SelectionError::Ambiguous {
            spelling: spelling.to_owned(),
            candidates,
        });
    }
    Ok(model)
}

/// Why a launch's model spelling selects no served model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectionError {
    /// No model OMP serves answers to the spelling.
    Unserved {
        /// The lane's spelling.
        spelling: String,
    },
    /// Several served models answer to the spelling.
    Ambiguous {
        /// The lane's spelling.
        spelling: String,
        /// Every served `provider/id` it names, in OMP's order.
        candidates: Vec<String>,
    },
}

impl std::fmt::Display for SelectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unserved { spelling } => {
                write!(formatter, "omp serves no model named {spelling}")
            }
            Self::Ambiguous {
                spelling,
                candidates,
            } => write!(
                formatter,
                "omp serves several models named {spelling}: {}; spell the lane model as provider/id",
                candidates.join(", ")
            ),
        }
    }
}

impl std::error::Error for SelectionError {}
