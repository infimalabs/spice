//! Private local projections of the steering media one exact occupant is told
//! about.
//!
//! An interjection names each attachment by reference; the occupant can only
//! look at the bytes through a readable file. Each occupant publishes into its
//! own tree under the lane, so no read for one occupant can publish over or
//! remove a path another occupant was shown.
//!
//! Every inbox read runs in its own process, so what this occupant has been
//! shown lives in a state file beside its supervisor state, and that state
//! path is the read's occupant identity. A missing file is an occupant nothing
//! was read for yet.
//!
//! A supervisor reads before the resident seats its occupant, and replays its
//! inbox into a compacted context before the resident records that
//! compaction's anchor. So:
//! - Until this occupant is first seen seated, a read publishes into its tree
//!   and retires nothing.
//! - Its first seated read retires every other occupant's tree, once.
//! - Each later context anchor retires what the occupant was shown before the
//!   generation that just ended, and keeps everything shown since, since the
//!   replay into the new context ran under the old anchor.
//! - Once seen seated, a read finding the lane vacant or seated by another
//!   occupant refuses before it touches any file.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use wire::CanonicalUuid;

use crate::steering_command::media_projection::{self, Projection, Record};

/// Bytes a projection state file may hold before it is refused as corrupt.
const STATE_BYTES: u64 = 1 << 20;

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    /// The context anchor this occupant last projected under; absent only
    /// before its first projection.
    compaction: Option<u64>,
    /// Everything shown since that anchor was first seen.
    retained: Vec<Record>,
    /// What was shown during the generation before it, which the compaction
    /// opening this one may have replayed into the current context.
    prior: Vec<Record>,
}

/// Encoded projection lines keyed by directive and attachment ordinal.
pub(super) type Projected = BTreeMap<(CanonicalUuid, u8), String>;

/// Why a read projected nothing.
#[derive(Debug)]
pub(super) enum Refusal {
    /// This occupant no longer holds the lane; the read must not answer.
    Stale(String),
    /// The media could not be projected; the text still stands.
    Failed(String),
}

/// The occupant whose projection state `state` is: exactly
/// `<lanes>/<lane>/jobs/<occupant>/<file>` beneath an existing occupant
/// directory.
pub(super) fn occupant(
    supervisors: &Path,
    lane: CanonicalUuid,
    state: &Path,
    file: &str,
) -> Result<CanonicalUuid, String> {
    let malformed = || {
        format!(
            "projection state {} is not an occupant's state beneath lane {lane}",
            state.display(),
        )
    };
    let occupant = state
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .and_then(|name| name.parse::<CanonicalUuid>().ok())
        .ok_or_else(malformed)?;
    let directory = spice::repository::lane_occupants(supervisors, lane).join(occupant.to_string());
    if state != directory.join(file) {
        return Err(malformed());
    }
    match std::fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(occupant),
        Ok(_) => Err(malformed()),
        Err(error) => Err(format!(
            "occupant {occupant} has no supervisor state: {error}"
        )),
    }
}

/// Publish every pending attachment into `occupant`'s tree under `root` and
/// name where each one is readable, carrying what this occupant was shown in
/// the file at `state`.
pub(super) fn project(
    root: &Path,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    state: &Path,
    inbox: &wire::intent::LaneInbox,
) -> Result<Projected, Refusal> {
    if inbox.occupant != Some(occupant) {
        return Err(Refusal::Stale(match inbox.occupant {
            Some(current) => {
                format!("lane {lane} is seated by occupant {current}, not {occupant}")
            }
            None => format!("lane {lane} seats no occupant, not {occupant}"),
        }));
    }
    let mut shown = match crate::resident::supervisor::read_json::<State>(state, STATE_BYTES) {
        Ok(shown) => shown,
        Err(error) if error.kind() == io::ErrorKind::NotFound => State::default(),
        Err(error) => {
            return Err(Refusal::Failed(format!(
                "steering media state is unreadable: {error}"
            )));
        }
    };
    let anchor = inbox.compaction.ok_or_else(|| {
        Refusal::Failed(format!(
            "lane {lane} seats occupant {occupant} without a context anchor"
        ))
    })?;
    if let Some(seen) = shown.compaction.filter(|seen| anchor < *seen) {
        return Err(Refusal::Failed(format!(
            "lane {lane} context anchor {anchor} precedes anchor {seen} occupant {occupant} already projected under"
        )));
    }
    let (projected, desired) =
        media_projection::publish(lane, occupant, &inbox.pending, root).map_err(Refusal::Failed)?;
    match shown.compaction {
        None => media_projection::retire_other_occupants(root, lane, occupant)
            .map_err(Refusal::Failed)?,
        Some(seen) if anchor > seen => {
            let ended = std::mem::take(&mut shown.retained);
            media_projection::retire(
                root,
                lane,
                occupant,
                shown
                    .prior
                    .iter()
                    .filter(|record| !ended.contains(record) && !desired.contains(record)),
            )
            .map_err(Refusal::Failed)?;
            shown.prior = ended
                .into_iter()
                .filter(|record| !desired.contains(record))
                .collect();
        }
        Some(_) => {}
    }
    shown.compaction = Some(anchor);
    for record in desired {
        if !shown.retained.contains(&record) {
            shown.retained.push(record);
        }
    }
    crate::resident::supervisor::publish_json(state, &shown).map_err(|error| {
        Refusal::Failed(format!("steering media state was not recorded: {error}"))
    })?;
    projected
        .values()
        .flatten()
        .map(|projection: &Projection| {
            let encoded = String::from_utf8(projection.encoded().map_err(Refusal::Failed)?)
                .map_err(|_| {
                    Refusal::Failed("an encoded media projection is UTF-8 JSON".to_owned())
                })?;
            let reference = projection.reference();
            Ok(((reference.directive(), reference.ordinal()), encoded))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::super::PROJECTION_STATE;
    use super::*;

    const LANE: &str = "0a2f5b6c-1d3e-4f50-8a91-b2c3d4e5f607";
    const PREDECESSOR: &str = "3d5b8e9f-4061-4273-9d24-e5f607182930";
    const SUCCESSOR: &str = "4e6c9fa0-5172-4384-8e35-f60718293a41";
    const FIRST: &str = "1b3f6c7d-2e4f-4051-9b02-c3d4e5f60718";
    const SECOND: &str = "2c4a7d8e-3f50-4162-8c13-d4e5f6071829";
    const PNG_DATA_URL: &str = "data:image/png;base64,iVBORw0KGgo=";
    const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n";

    #[test]
    fn an_interjected_attachment_names_a_private_file_holding_its_exact_bytes() {
        let fixture = Fixture::new();
        let inbox = inbox(Some(SUCCESSOR), Some(40), &[FIRST]);
        let projected = fixture
            .read(SUCCESSOR, &inbox)
            .expect("pending media projects");
        let rendered = steer::inbox::render_pending(
            &steer::Key::for_identity(fixture.lane),
            &inbox.pending,
            "2026-08-30T09:00:00.000Z".parse().expect("fixture now"),
            |directive, ordinal| projected.get(&(directive, ordinal)).cloned(),
        );

        let reference = rendered
            .lines()
            .position(|line| line.starts_with("media-reference="))
            .expect("the interjection names the attachment");
        let projection: serde_json::Value = rendered
            .lines()
            .nth(reference + 1)
            .and_then(|line| line.strip_prefix("media-projection="))
            .map(|line| serde_json::from_str(line).expect("projection is JSON"))
            .expect("the projection follows its reference");
        let cited = &wire::media::agent_media_references(
            inbox.pending[0].directive,
            &inbox.pending[0].attachments,
        )
        .expect("fixture media cites")[0];
        assert_eq!(
            projection["digest"],
            serde_json::to_value(cited.digest()).expect("digest encodes"),
        );
        assert_eq!(projection["bytes"], cited.bytes());
        assert_eq!(projection["content_type"], "image/png");
        let path = projection["path"]
            .as_str()
            .expect("projection names a path");
        let metadata = fs::symlink_metadata(path).expect("the named file exists");
        assert!(metadata.file_type().is_file());
        assert_eq!(metadata.len(), cited.bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(fs::read(path).expect("the named file reads"), PNG_BYTES);
    }

    #[test]
    fn a_predecessor_reading_after_its_successor_touches_none_of_the_successors_media() {
        let fixture = Fixture::new();
        let earlier = only_path(
            &fixture
                .read(PREDECESSOR, &inbox(Some(PREDECESSOR), Some(10), &[FIRST]))
                .expect("the predecessor's media projects"),
        );
        let shown = only_path(
            &fixture
                .read(SUCCESSOR, &inbox(Some(SUCCESSOR), Some(20), &[FIRST]))
                .expect("the successor's media projects"),
        );
        assert_ne!(earlier, shown, "occupants never share a projection path");
        assert!(
            fs::symlink_metadata(&earlier).is_err(),
            "the seated successor retires its predecessor's media",
        );
        // Staleness outranks whatever the predecessor's own state holds.
        fs::write(fixture.state(PREDECESSOR), b"{}").expect("predecessor state rewrites");

        for answer in [
            inbox(Some(SUCCESSOR), Some(20), &[FIRST, SECOND]),
            inbox(None, None, &[FIRST, SECOND]),
        ] {
            assert!(matches!(
                fixture.read(PREDECESSOR, &answer),
                Err(Refusal::Stale(_)),
            ));
        }
        assert_eq!(fs::read(&shown).expect("successor media reads"), PNG_BYTES);
        assert!(
            fs::symlink_metadata(fixture.occupant_tree(PREDECESSOR)).is_err(),
            "a stale read publishes nothing",
        );
    }

    #[test]
    fn a_new_occupant_projects_nothing_before_its_start_anchor_and_keeps_its_media_through_ack() {
        let fixture = Fixture::new();
        let predecessor = only_path(
            &fixture
                .read(PREDECESSOR, &inbox(Some(PREDECESSOR), Some(10), &[SECOND]))
                .expect("the predecessor's media projects"),
        );

        // The successor's supervisor reads before the resident commits its
        // start: the lane still names its predecessor, or nobody.
        for unseated in [
            inbox(Some(PREDECESSOR), Some(10), &[FIRST]),
            inbox(None, None, &[FIRST]),
        ] {
            assert!(matches!(
                fixture.read(SUCCESSOR, &unseated),
                Err(Refusal::Stale(_)),
            ));
        }
        assert!(
            fs::symlink_metadata(fixture.occupant_tree(SUCCESSOR)).is_err(),
            "an unseated read publishes nothing",
        );
        assert!(
            fs::symlink_metadata(fixture.state(SUCCESSOR)).is_err(),
            "an unseated read records no projection state",
        );
        assert_eq!(
            fs::read(&predecessor).expect("nothing retires before seating"),
            PNG_BYTES,
        );

        // The read the start commit kicks delivers the directive, and every
        // reread after its acknowledgment keeps what it was shown.
        let received = only_path(
            &fixture
                .read(SUCCESSOR, &inbox(Some(SUCCESSOR), Some(953_349), &[FIRST]))
                .expect("the started occupant receives its media"),
        );
        for _ in 0..2 {
            fixture
                .read(SUCCESSOR, &inbox(Some(SUCCESSOR), Some(953_349), &[]))
                .expect("the seated occupant rereads");
            assert_eq!(
                fs::read(&received).expect("acknowledged media stays readable"),
                PNG_BYTES,
            );
        }
        assert!(
            fs::symlink_metadata(&predecessor).is_err(),
            "the successor's first seated read retires its predecessor's media",
        );
    }

    #[test]
    fn an_unusable_anchor_or_state_refuses_before_publishing() {
        let fixture = Fixture::new();
        let shown = only_path(
            &fixture
                .read(SUCCESSOR, &inbox(Some(SUCCESSOR), Some(30), &[FIRST]))
                .expect("the seated occupant's media projects"),
        );
        let recorded = fs::read(fixture.state(SUCCESSOR)).expect("projection state records");

        for unusable in [
            inbox(Some(SUCCESSOR), Some(10), &[SECOND]),
            inbox(Some(SUCCESSOR), None, &[SECOND]),
        ] {
            assert!(matches!(
                fixture.read(SUCCESSOR, &unusable),
                Err(Refusal::Failed(_)),
            ));
        }
        assert_eq!(
            fs::read(fixture.state(SUCCESSOR)).expect("projection state reads"),
            recorded,
            "a refused read leaves the projection state unchanged",
        );
        assert_eq!(
            fs::read_dir(fixture.occupant_tree(SUCCESSOR))
                .expect("the occupant tree lists")
                .count(),
            1,
            "a refused read publishes no other directive",
        );
        assert_eq!(fs::read(&shown).expect("shown media reads"), PNG_BYTES);

        fs::write(
            fixture.state(SUCCESSOR),
            br#"{"compaction":30,"retained":[]}"#,
        )
        .expect("a state without its prior generation writes");
        assert!(matches!(
            fixture.read(SUCCESSOR, &inbox(Some(SUCCESSOR), Some(30), &[SECOND])),
            Err(Refusal::Failed(_)),
        ));
        assert_eq!(
            fs::read_dir(fixture.occupant_tree(SUCCESSOR))
                .expect("the occupant tree lists")
                .count(),
            1,
            "a state missing a field publishes nothing",
        );
    }

    #[test]
    fn a_compaction_keeps_what_its_replay_showed_and_retires_the_generation_before_it() {
        let fixture = Fixture::new();
        let read = |anchor: u64, pending: &[&str]| {
            fixture
                .read(SUCCESSOR, &inbox(Some(SUCCESSOR), Some(anchor), pending))
                .expect("the seated occupant reads")
        };
        let first = only_path(&read(10, &[FIRST]));
        read(10, &[]);
        assert_eq!(fs::read(&first).expect("answered media reads"), PNG_BYTES);

        // The replay into a compacted context runs before its anchor lands.
        let second = only_path(&read(10, &[SECOND]));
        read(30, &[]);
        assert_eq!(
            fs::read(&second).expect("the replayed context keeps its media"),
            PNG_BYTES,
        );

        read(50, &[]);
        for retired in [&first, &second] {
            assert!(
                fs::symlink_metadata(retired).is_err(),
                "the next context retires what only an earlier one was shown",
            );
        }
    }

    #[test]
    fn only_an_occupants_own_state_path_names_it() {
        let fixture = Fixture::new();
        let supervisors = fixture.root.clone();
        let state = fixture.state(SUCCESSOR);
        assert_eq!(
            occupant(&supervisors, fixture.lane, &state, PROJECTION_STATE).ok(),
            Some(SUCCESSOR.parse().expect("fixture occupant")),
        );
        let lane = spice::repository::lane_occupants(&supervisors, fixture.lane);
        for malformed in [
            spice::repository::lane_jobs(&supervisors, fixture.lane)
                .join(SUCCESSOR)
                .join(PROJECTION_STATE),
            lane.join(SUCCESSOR).join("state.json"),
            lane.join("not-an-occupant").join(PROJECTION_STATE),
            supervisors
                .join(FIRST)
                .join(SUCCESSOR)
                .join(PROJECTION_STATE),
            fixture
                .root
                .join(LANE)
                .join(SUCCESSOR)
                .join(PROJECTION_STATE),
            lane.join(PREDECESSOR).join(PROJECTION_STATE),
        ] {
            assert!(
                occupant(&supervisors, fixture.lane, &malformed, PROJECTION_STATE).is_err(),
                "{} names no occupant of this lane",
                malformed.display(),
            );
        }
    }

    struct Fixture {
        root: PathBuf,
        lane: CanonicalUuid,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                root: std::env::temp_dir().join(format!(
                    "spice-supervisor-media-{}-{}",
                    std::process::id(),
                    uuid::Uuid::new_v4().simple(),
                )),
                lane: LANE.parse().expect("fixture lane"),
            }
        }

        fn media(&self) -> PathBuf {
            self.root.clone()
        }

        fn occupant_tree(&self, occupant: &str) -> PathBuf {
            spice::repository::lane_media(&self.root, self.lane).join(occupant)
        }

        fn state(&self, occupant: &str) -> PathBuf {
            let directory = spice::repository::lane_occupants(&self.root, self.lane).join(occupant);
            fs::create_dir_all(&directory).expect("supervisor directory");
            directory.join(PROJECTION_STATE)
        }

        fn read(
            &self,
            occupant: &str,
            inbox: &wire::intent::LaneInbox,
        ) -> Result<Projected, Refusal> {
            project(
                &self.media(),
                self.lane,
                occupant.parse().expect("fixture occupant"),
                &self.state(occupant),
                inbox,
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn only_path(projected: &Projected) -> PathBuf {
        let [encoded] = projected.values().collect::<Vec<_>>()[..] else {
            panic!("exactly one projection");
        };
        let projection: serde_json::Value = serde_json::from_str(encoded).expect("projection");
        PathBuf::from(projection["path"].as_str().expect("path"))
    }

    fn inbox(
        occupant: Option<&str>,
        compaction: Option<u64>,
        directives: &[&str],
    ) -> wire::intent::LaneInbox {
        let mut row = serde_json::json!({
            "lane": LANE,
            "pending": directives
                .iter()
                .map(|directive| serde_json::json!({
                    "directive": directive,
                    "key": "1kMdbx1xx",
                    "text": "inspect the capture",
                    "attachments": [{
                        "name": "capture.png",
                        "content_type": "image/png",
                        "data_url": PNG_DATA_URL,
                    }],
                    "from": null,
                    "sender": null,
                    "authored": "2026-08-30T09:00:00.000Z",
                    "stage": "published",
                    "expires": "2026-08-30T10:00:00.000Z",
                }))
                .collect::<Vec<_>>(),
        });
        if let Some(occupant) = occupant {
            row["occupant"] = occupant.into();
        }
        if let Some(compaction) = compaction {
            row["compaction"] = compaction.into();
        }
        serde_json::from_value(row).expect("inbox fixture admits")
    }
}
