//! Admission of lane tape records written before any resident runs.
//!
//! A seed that authors lane transcripts, such as the demo, admits each record
//! through the steps a live capture sweep takes once a record lands: account
//! the lane's tape, publish every packet the accounted records carry as their
//! author's, and hear the steering keys their prose heads. No line, room or
//! delivery fact is formed here, so seeded traffic meets exactly the admission
//! rules live traffic meets, and its keys are the ones admission minted.

use std::io;
use std::path::Path;

use bus::Bus;
use spice::repository::Repository;
use wire::{CanonicalUuid, OccupantAddress, Outcome, Refusal};

use super::directive_context::steering_keys;
use crate::resident::{
    CommandResult, RuntimePaths, environment_fault, ingest_transcripts, steering, store_fault,
};

/// The one journal writer, runtime paths and configuration a seeded
/// repository's records are admitted under.
pub(crate) struct TapeAdmission {
    bus: Bus,
    paths: RuntimePaths,
    epoch: CanonicalUuid,
    steering: steer::Config,
    repository: Repository,
    ambient: spice::ambient::Ambient,
    resources: crate::board_command::resources::ResourceClaims,
}

impl TapeAdmission {
    /// Take the journal writer of the repository at `root`, whose residency
    /// `epoch` names. Nothing else may write that journal until [`Self::close`].
    pub(crate) fn open(root: &Path, epoch: CanonicalUuid) -> CommandResult<Self> {
        let repository = Repository::discover(root).map_err(|error| {
            environment_fault(spice::repository::ResolveError::REASON, &error.to_string())
        })?;
        let paths = RuntimePaths::from_repository(&repository)?;
        let config = spice::config::load_repository(&repository, &[])
            .map_err(|error| environment_fault("config::unloadable", &error.to_string()))?;
        let journal = journal::Journal::open(&paths.journal)
            .map_err(|error| store_fault("resident::journal-unavailable", &error.to_string()))?;
        let bus = Bus::start(journal)
            .map_err(|error| store_fault("resident::bus-unavailable", &error.to_string()))?;
        let resources = crate::board_command::resources::ResourceClaims::new(
            sill::LockTopology::new(repository.worktree_root(), config.root().lock()),
        );
        resources
            .restore(&bus, Some(&repository))
            .map_err(crate::typed_outcome)?;
        Ok(Self {
            bus,
            paths,
            epoch,
            steering: config.root().steer().clone(),
            repository,
            ambient: spice::ambient::Ambient::default(),
            resources,
        })
    }

    /// The address `occupant` was born with on `lane`.
    pub(crate) fn address(
        &self,
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
    ) -> CommandResult<OccupantAddress> {
        self.bus
            .task_snapshot()
            .map_err(|error| store_fault("steer::residency-unreadable", &error.to_string()))?
            .facts()
            .occupant_address(lane, occupant)
            .map_err(|error| store_fault("address::unrecorded", &format!("{error:?}")))
    }

    /// Keys of the inputs pending on `lane` that `from` told it as a peer, in
    /// admission order.
    pub(crate) fn peer_keys(
        &self,
        lane: CanonicalUuid,
        from: CanonicalUuid,
    ) -> CommandResult<Vec<String>> {
        let pending = self
            .bus
            .read(move |read| crate::steering_read::pending(read, Some(lane)))
            .map_err(|error| store_fault("steer::index-unreadable", &error.to_string()))?;
        Ok(pending
            .into_iter()
            .filter(|head| head.body().sender == wire::Actor::Lane(from))
            .map(|head| head.body().key.clone())
            .collect())
    }

    /// Land `text` as one complete assistant block that `occupant` of the omp
    /// lane `lane` wrote at `at`, streamed and then settled as omp records
    /// it, and admit it as a live sweep would.
    ///
    /// A packet the block holds that publishes nothing refuses with its
    /// reason; a delivery that misses some addresses still admits.
    pub(crate) fn say(
        &self,
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        at: &str,
        text: &str,
        diagnostic: &mut dyn io::Write,
    ) -> CommandResult<()> {
        let vendor: wire::Name = "omp".parse().expect("omp vendor name is valid");
        let tape = tape::Tape::open(self.paths.lane_tape(lane).join(vendor.as_str()))
            .map_err(|error| store_fault("tape::unwritable", &error.to_string()))?;
        let timestamp = at
            .parse::<wire::Instant>()
            .map_err(|error| store_fault("tape::invalid-time", &error.to_string()))?
            .get()
            .as_millisecond();
        let started = serde_json::json!({
            "type": "message_start",
            "messageId": at,
            "message": {"role": "assistant", "timestamp": timestamp},
        });
        let streamed = serde_json::json!({
            "type": "message_update",
            "messageId": at,
            "message": {"role": "assistant"},
            "assistantMessageEvent": {"type": "text_end", "contentIndex": 0, "content": text},
        });
        let settled = serde_json::json!({
            "timestamp": at,
            "type": "message_end",
            "messageId": at,
            "message": {
                "role": "assistant",
                "timestamp": timestamp,
                "content": [{"type": "text", "text": text}],
                "stopReason": "stop",
            },
        });
        for record in [started, streamed, settled] {
            tape.tee(format!("{record}\n").as_bytes())
                .map_err(|error| store_fault("tape::unwritable", &error.to_string()))?;
        }
        self.admit(lane, occupant, &vendor, diagnostic)
    }

    /// Account what `lane`'s tape gained, publish its packets as `occupant`'s,
    /// and hear the keys its prose heads.
    fn admit(
        &self,
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        vendor: &wire::Name,
        diagnostic: &mut dyn io::Write,
    ) -> CommandResult<()> {
        let (observations, _) = ingest_transcripts(
            &self.bus,
            &self.paths,
            Some(lane),
            lane::ingest::Bound::IMMEDIATE,
            lane::ingest::Coalescing::new(),
            diagnostic,
        )?;
        let mut texts = Vec::new();
        for observation in observations
            .iter()
            .filter(|observation| observation.batch.lane == lane)
        {
            for record in &observation.records {
                let listen::RecordPayload::Events(sourced) = &record.payload else {
                    continue;
                };
                let events = sourced
                    .iter()
                    .map(|sourced| sourced.event.clone())
                    .collect::<Vec<_>>();
                for outcome in steering::publish_authored(
                    &self.bus,
                    &self.paths,
                    self.epoch,
                    &self.steering,
                    &steering::AuthoredRecord {
                        lane,
                        author: occupant,
                        source: observation.batch.source,
                        locus: record.locus,
                        events: &events,
                    },
                    steering::Refusals::Record,
                    diagnostic,
                ) {
                    outcome.map_err(unadmitted)?;
                }
                texts.extend(
                    events
                        .iter()
                        .filter_map(listen::Event::assistant_text)
                        .map(str::to_owned),
                );
            }
        }
        let keys = steering_keys(texts.iter().map(String::as_str));
        if !keys.is_empty() {
            steering::dispose_heard(
                &self.bus,
                &self.paths,
                (&self.steering, self.epoch, self.joining()),
                lane,
                vendor,
                &keys,
                diagnostic,
            );
        }
        Ok(())
    }

    fn joining(&self) -> crate::board_command::Joining<'_> {
        crate::board_command::Joining {
            host: crate::command_client::Host {
                repository: &self.repository,
                ambient: &self.ambient,
            },
            resources: &self.resources,
        }
    }

    /// Verify the journal and release its writer.
    pub(crate) fn close(self) -> CommandResult<()> {
        self.bus
            .verify()
            .map_err(|error| store_fault("journal::invalid", &error.to_string()))?;
        self.bus
            .shutdown()
            .map_err(|error| store_fault("resident::bus-unavailable", &error.to_string()))
    }
}

/// The refusal a packet that published nothing names.
fn unadmitted(unpublished: steering::Unpublished) -> Outcome<()> {
    match unpublished {
        steering::Unpublished::Refused { reason, detail }
        | steering::Unpublished::Recorded { reason, detail }
        | steering::Unpublished::Withheld { reason, detail } => Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(reason, Some("message".to_owned()), None, None)
                .with_detail(&detail),
        )),
        steering::Unpublished::Failed(outcome) => crate::typed_outcome(outcome),
    }
}
