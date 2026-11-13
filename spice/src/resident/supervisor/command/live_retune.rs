//! Correlated live setting changes over one running OMP command stream.

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::PoisonError;
use std::sync::atomic::Ordering;

use listen::Decoded;
use serde::{Deserialize, Serialize};
use wire::CanonicalUuid;

use super::{
    Driver, Kick, RETUNE_TIMEOUT, RuntimePaths, call, door_root, kick_path, reply_name, send_to,
};

/// A live OMP setting change. Exact model parts are required because OMP's
/// runtime command does not admit the CLI's fuzzy selector.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "setting", rename_all = "snake_case")]
pub(in crate::resident) enum LiveRetune {
    Effort { effort: String },
    Model { provider: String, model: String },
    Fast { enabled: bool },
}

impl LiveRetune {
    fn command(&self) -> &'static str {
        match self {
            Self::Effort { .. } => "set_thinking_level",
            Self::Model { .. } => "set_model",
            Self::Fast { .. } => "set_fast_mode",
        }
    }

    fn request(&self, id: &str) -> Vec<u8> {
        match self {
            Self::Effort { effort } => vendor::omp::set_thinking_level_request(id, effort),
            Self::Model { provider, model } => vendor::omp::set_model_request(id, provider, model),
            Self::Fast { enabled } => vendor::omp::set_fast_mode_request(id, *enabled),
        }
    }
}

pub(super) struct PendingRetune {
    command: &'static str,
    reply: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct RetuneReply {
    pub(super) request: String,
    pub(super) result: LiveRetuneResult,
}

/// Confirmed disposition of one attempted live provider retune.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub(in crate::resident) enum LiveRetuneResult {
    Applied,
    Refused { detail: String },
}

impl<W: Write + Send + 'static> Driver<W> {
    /// Submit one live setting change after registering its exact correlated
    /// response. A pipe failure is answered through the same callback path as
    /// a provider refusal.
    pub(super) fn retune(&self, request: String, retune: LiveRetune, reply: PathBuf) {
        let command = retune.command();
        if retune == (LiveRetune::Fast { enabled: true }) {
            self.fast_refused.store(false, Ordering::Relaxed);
        }
        self.retunes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(request.clone(), PendingRetune { command, reply });
        if let Err(error) = self.write(&retune.request(&request))
            && let Some(pending) = self
                .retunes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&request)
        {
            let _sent = send_to(
                &pending.reply,
                &RetuneReply {
                    request,
                    result: LiveRetuneResult::Refused {
                        detail: error.to_string(),
                    },
                },
            );
        }
    }

    /// Settle a pending live retune only from its matching provider response.
    pub(super) fn observe_retune(&self, decoded: &Decoded) {
        let Some(response) = decoded
            .context()
            .and_then(|context| context.response.as_ref())
        else {
            return;
        };
        let Some(request) = response.id.as_ref() else {
            return;
        };
        let Some(pending) = self
            .retunes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(request)
        else {
            return;
        };
        let result = if response.command != pending.command {
            LiveRetuneResult::Refused {
                detail: format!(
                    "provider answered {} with {}",
                    pending.command, response.command
                ),
            }
        } else if response.success {
            LiveRetuneResult::Applied
        } else {
            LiveRetuneResult::Refused {
                detail: response
                    .error
                    .clone()
                    .unwrap_or_else(|| "provider refused without detail".to_owned()),
            }
        };
        let _sent = send_to(
            &pending.reply,
            &RetuneReply {
                request: request.clone(),
                result,
            },
        );
    }
}

/// Apply one setting change to the exact running occupant and wait for OMP's
/// correlated response.
///
/// # Errors
///
/// Returns an I/O error when no matching supervisor can receive the request,
/// its callback cannot be read, or the provider does not answer in time.
pub(in crate::resident) fn retune(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    retune: LiveRetune,
) -> io::Result<LiveRetuneResult> {
    let request = format!(
        "{}{}",
        listen::OMP_RETUNE_REQUEST_ID_PREFIX,
        super::super::super::new_uuid()
    );
    let reply = door_root(paths).join(reply_name(&paths.common, 'r', &request));
    let answer: RetuneReply = call(&kick_path(paths, lane), reply, RETUNE_TIMEOUT, |reply| {
        Kick::Retune {
            occupant,
            request: request.clone(),
            retune,
            reply,
        }
    })?;
    if answer.request != request {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "live retune callback named another request",
        ));
    }
    Ok(answer.result)
}
