//! The first turn, held until omp answers the `spice` scheme registration and
//! the model, effort, and fast-mode selection and the resident has seated this
//! occupant, whichever is latest. Only a seat that serves the scheme is told
//! about it and handed URLs to read, and only a seated occupant's commands pass
//! job admission, so a turn written before the start commits would open with
//! refusals. The model is selected once omp lists what it serves, and the
//! effort and fast mode follow it; a turn written before omp answers them
//! would run on whatever model, reasoning level, or service tier omp chose
//! for itself or a resumed session recorded. Fresh and
//! resumed seats read their pending steering only when releasing that first
//! prompt and append it after the briefing or resume notice. Background
//! notices, kept jobs' matches and endings, and later steering wait behind that
//! turn: omp runs whatever reaches an idle session first as a turn of its own,
//! so anything written ahead of the first prompt would leave the prompt's
//! authority stale.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use listen::Decoded;

use super::{Driver, Job, Report};

/// Classify the current OMP generation's opening fast-mode answer. Only its
/// exact no-tier refusal is nonfatal; every other unsuccessful or mismatched
/// response could leave a resumed session on stale recorded authority.
fn opening_fast_refusal(response: &listen::CommandResponse) -> Result<Option<String>, String> {
    if response.command != "set_fast_mode" {
        return Err(format!(
            "omp answered the fast-mode selection with {}",
            response.command
        ));
    }
    if response.success {
        return Ok(None);
    }
    let detail = response
        .error
        .as_deref()
        .ok_or_else(|| "omp refused the fast-mode selection without detail".to_owned())?;
    if vendor::omp::fast_mode_unavailable(detail) {
        Ok(Some(detail.to_owned()))
    } else {
        Err(format!("omp refused the fast-mode selection: {detail}"))
    }
}
#[cfg(test)]
mod tests;

/// The first turn a launch composed, and whether its seat resumes a session.
pub(in crate::resident) enum FirstTurn {
    /// A fresh session's turn, already carrying its startup briefing. Pending
    /// steering current at delivery follows that briefing.
    Fresh(String),
    /// A resumed session's neutral turn. The resume notice and pending
    /// steering join it only as it is delivered, so no launch, however early
    /// or abandoned, freezes that authority.
    Resumed(String),
}

/// Whether omp serves `spice://`, the model the seat runs on, and the first
/// turn awaiting those answers and the occupant's seating.
#[derive(Default)]
pub(super) struct Registration {
    /// Settled by omp's answer; until then nothing has named a read.
    serves: OnceLock<bool>,
    /// The model still to select once omp lists what it serves; taken by the
    /// listing's answer.
    selection: Mutex<Option<vendor::omp::ModelSelection>>,
    /// The catalog this occupant reported at startup, retained as the
    /// authority for later live model spellings.
    pub(super) models: OnceLock<Vec<listen::ServedModel>>,
    /// Set once omp accepts the model selection.
    model_accepted: OnceLock<()>,
    /// Set once omp accepts the effort that follows an accepted model.
    effort_accepted: OnceLock<()>,
    /// Set once omp answers the fast-mode disposition that follows an
    /// accepted effort, or at open when the launch selects none.
    selected: OnceLock<()>,
    /// omp's refusal of the lane's fast mode, raised for the lane row once the
    /// first turn is written and the occupant therefore seated.
    fast_unavailable: Mutex<Option<String>>,
    first_turn: Mutex<Option<FirstTurn>>,
    /// Whether a first turn is still unwritten. Read and cleared only under
    /// the driver's background lock, so a notice is either held for the
    /// first prompt's flush or written after it, never neither.
    holding: AtomicBool,
    /// Kept jobs whose telling waits for the first prompt, in the order they
    /// first asked. Nothing about their telling is claimed meanwhile, so a
    /// seat that closes first leaves them owed to its successor.
    jobs: Mutex<Vec<(Job, Owed)>>,
}

/// What a held job still has to say once the first prompt is written.
enum Owed {
    /// Its unheard match.
    Match,
    /// Its unheard match, then the ending it was followed to.
    Ending(Option<Report>),
}

impl Registration {
    /// Whether background notices must wait for the first prompt.
    pub(super) fn holds_first_turn(&self) -> bool {
        self.holding.load(Ordering::Acquire)
    }
}

impl<W: Write + Send + 'static> Driver<W> {
    /// Write the opening request the vendor contract composed and hold the
    /// first turn for the registration answer and the model selection. omp
    /// answers every command, an unknown one with a refusal, in the order the
    /// opening sent them, so the registration answer always precedes the
    /// session's.
    ///
    /// # Errors
    ///
    /// Returns the pipe's error, or `BrokenPipe` once the stream is closed.
    pub(in crate::resident) fn open(
        &self,
        opening: &[u8],
        selection: Option<vendor::omp::ModelSelection>,
        first_turn: Option<FirstTurn>,
    ) -> io::Result<()> {
        {
            let _background = self
                .background
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            self.registration
                .holding
                .store(first_turn.is_some(), Ordering::Release);
            if selection.is_none() {
                let _ = self.registration.selected.set(());
            }
            *self
                .registration
                .selection
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = selection;
            *self
                .registration
                .first_turn
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = first_turn;
        }
        self.write(opening)
    }

    /// Read omp's answers to the opening's model listing, model, effort, and
    /// fast-mode selection, and scheme registration. Returns omp's words for a
    /// refused fast mode once the first turn is written, since only a seated
    /// occupant's lane row can carry them.
    pub(super) fn observe_opening(&self, decoded: &Decoded) -> Option<String> {
        self.observe_model_selection(decoded);
        self.observe_uri_registration(decoded);
        if self.registration.holds_first_turn() {
            return None;
        }
        self.registration
            .fast_unavailable
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Select the launch's model from omp's listing, then release the first
    /// turn once omp answers the selection, the effort, and the fast-mode
    /// disposition stated after it. A spelling no listed model or several
    /// listed models answer to, a refused listing, and a refused model or
    /// effort each end the command stream instead, so no turn runs on a model
    /// or level the lane did not name. Only OMP's exact no-tier fast-mode
    /// refusal runs the turn without fast mode and is kept for the lane row;
    /// any other fast-mode answer that is not success ends the stream too.
    fn observe_model_selection(&self, decoded: &Decoded) {
        let Some(response) = decoded
            .context()
            .and_then(|context| context.response.as_ref())
        else {
            return;
        };
        match response.id.as_deref() {
            Some(vendor::omp::MODELS_REQUEST_ID) => {
                let served = response.served_models.as_deref();
                if let Some(catalog) = served {
                    let _ = self.registration.models.set(catalog.to_vec());
                }
                let Some(selection) = self
                    .registration
                    .selection
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                else {
                    return;
                };
                let request = served
                    .ok_or_else(|| {
                        format!(
                            "omp refused to list its models: {}",
                            response.error.as_deref().unwrap_or("no reason given")
                        )
                    })
                    .and_then(|served| {
                        selection.request(served).map_err(|error| error.to_string())
                    });
                let written = match request {
                    Ok(bytes) => self.write(&bytes).map_err(|error| error.to_string()),
                    Err(refusal) => Err(refusal),
                };
                if let Err(refusal) = written {
                    eprintln!(
                        "spice supervisor: model {} unselected: {refusal}",
                        selection.spelling()
                    );
                    self.close();
                }
            }
            Some(vendor::omp::MODEL_REQUEST_ID) => {
                if response.command != "set_model" {
                    eprintln!(
                        "spice supervisor: omp answered the model selection's set_model with {}",
                        response.command
                    );
                    self.close();
                } else if response.success {
                    let _ = self.registration.model_accepted.set(());
                } else {
                    eprintln!(
                        "spice supervisor: omp refused the model selection: {}",
                        response.error.as_deref().unwrap_or("no reason given")
                    );
                    self.close();
                }
            }
            Some(vendor::omp::EFFORT_REQUEST_ID) => {
                if response.command != "set_thinking_level" {
                    eprintln!(
                        "spice supervisor: omp answered the effort selection's set_thinking_level with {}",
                        response.command
                    );
                    self.close();
                } else if !response.success {
                    eprintln!(
                        "spice supervisor: omp refused the effort selection: {}",
                        response.error.as_deref().unwrap_or("no reason given")
                    );
                    self.close();
                } else if self.registration.model_accepted.get().is_some() {
                    let _ = self.registration.effort_accepted.set(());
                }
            }
            Some(listen::OMP_FAST_REQUEST_ID) => {
                if self.registration.effort_accepted.get().is_none()
                    || self.registration.selected.get().is_some()
                {
                    return;
                }
                let refusal = match opening_fast_refusal(response) {
                    Ok(refusal) => refusal,
                    Err(detail) => {
                        eprintln!("spice supervisor: {detail}");
                        self.close();
                        return;
                    }
                };
                if self.registration.selected.set(()).is_err() {
                    return;
                }
                if let Some(refusal) = refusal {
                    *self
                        .registration
                        .fast_unavailable
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner) = Some(refusal);
                }
                self.release_first_turn();
            }
            _ => {}
        }
    }

    /// Settle whether this seat serves `spice://` from omp's answer to the
    /// registration, then release the first turn if the occupant is already
    /// seated. The first answer alone settles it.
    fn observe_uri_registration(&self, decoded: &Decoded) {
        let Some(response) = decoded
            .context()
            .and_then(|context| context.response.as_ref())
        else {
            return;
        };
        if response.id.as_deref() != Some(listen::OMP_URI_SCHEMES_REQUEST_ID)
            || self.registration.serves.set(response.success).is_err()
        {
            return;
        }
        self.release_first_turn();
    }

    /// Start the held first turn once omp has answered the registration and
    /// the model, effort, and fast-mode selection and the journal seats this
    /// occupant; returns whether this call started it.
    /// Only one call takes the turn, so the resumed-seat notice and every
    /// first prompt's pending inbox are read once, at this delivery. Background
    /// notices held meanwhile follow the prompt, then held jobs' matches and
    /// endings, then an inbox check delivers anything that arrived after the
    /// startup read.
    pub(super) fn release_first_turn(&self) -> bool {
        if self.registration.serves.get().is_none() || self.registration.selected.get().is_none() {
            return false;
        }
        let turn = {
            let mut held = self
                .registration
                .first_turn
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if held.is_none() || !(self.read_seated)() {
                return false;
            }
            held.take()
        };
        let Some(turn) = turn else {
            return false;
        };
        let prompt = match turn {
            FirstTurn::Fresh(prompt) => prompt,
            FirstTurn::Resumed(mut prompt) => {
                prompt.push_str("\n\n");
                prompt.push_str(&(self.read_resume)());
                prompt
            }
        };
        let mut prompt = self.for_seat(prompt);
        if let Some(inbox) = self.startup_inbox() {
            prompt.push_str("\n\n");
            prompt.push_str(&inbox);
        }
        let prompt = vendor::omp::first_prompt_request(&prompt);
        let queued = self
            .background
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Err(error) = self.write(&prompt) {
            eprintln!("spice supervisor: first turn unwritten: {error}");
        }
        self.registration.holding.store(false, Ordering::Release);
        for (purpose, message) in queued.iter() {
            let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
            if let Err(error) = self.write(&vendor::omp::aside_request(sequence, purpose, message))
            {
                eprintln!("spice supervisor: held notice {purpose} unwritten: {error}");
            }
        }
        drop(queued);
        let held = std::mem::take(
            &mut *self
                .registration
                .jobs
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for (job, owed) in held {
            self.deliver_match(&job);
            if let Owed::Ending(report) = owed {
                self.deliver(&job, report.as_ref());
            }
        }
        self.interject(super::steering::InboxDelivery::Update);
        true
    }

    /// Hold one kept job's telling while the first turn is unwritten, and say
    /// whether it was held. An ending replaces an earlier match-only hold,
    /// since a delivered ending follows the job's match.
    pub(super) fn hold_job(&self, job: &Job, ending: Option<Option<&Report>>) -> bool {
        let _background = self
            .background
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !self.registration.holds_first_turn() {
            return false;
        }
        let owed = ending.map_or(Owed::Match, |report| Owed::Ending(report.cloned()));
        let mut held = self
            .registration
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match held.iter_mut().find(|(held, _)| held.id() == job.id()) {
            Some((_, slot)) => {
                if matches!(owed, Owed::Ending(_)) {
                    *slot = owed;
                }
            }
            None => held.push((job.clone(), owed)),
        }
        true
    }

    /// `text` as this seat should read it: with `spice://` URLs and the scheme
    /// guide when omp serves the scheme, else exactly as composed.
    pub(super) fn for_seat(&self, text: String) -> String {
        if self.registration.serves.get() == Some(&true) {
            crate::resource::for_served_seat(&text)
        } else {
            text
        }
    }
}
