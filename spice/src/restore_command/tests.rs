use std::collections::BTreeMap;

use wire::WireVersion;
use wire::manifest::{JOURNAL_BACKUP_VERSION, JournalBackupManifest, JournalBackupMember};

use super::*;

const AUTHORITY: &str = "spicejournal.sqlite3";

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        // A short base keeps the special-file socket under the Unix path limit.
        #[cfg(unix)]
        let base = PathBuf::from("/tmp");
        #[cfg(not(unix))]
        let base = std::env::temp_dir();
        let root = base.join(format!(
            "spice-restore-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir(&root).expect("exclusive scratch root is created");
        Self(root)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for directory in [
            self.0.clone(),
            self.0.join("backup"),
            self.0.join("backup/tape"),
        ] {
            let _ = authority::set_owner_mode(&directory, 0o700);
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One complete, manifest-bound backup bundle inside its own scratch root.
struct Bundle {
    scratch: Scratch,
    bundle: PathBuf,
}

impl Bundle {
    fn new(label: &str) -> Self {
        let scratch = Scratch::new(label);
        let bundle = scratch.0.join("backup");
        fs::create_dir_all(bundle.join("tape")).expect("bundle directories");
        drop(journal::Journal::open(bundle.join(AUTHORITY)).expect("finalized authority opens"));
        fs::write(bundle.join("tape/lane-a"), b"sealed transcript member")
            .expect("tape member writes");
        let fixture = Self { scratch, bundle };
        fixture.write_manifest(JOURNAL_BACKUP_VERSION, journal::SCHEMA_VERSION);
        fixture
    }

    fn root(&self) -> &Path {
        &self.scratch.0
    }

    fn write_manifest(&self, version: u16, journal_schema: u32) {
        self.write_manifest_versions(version, WireVersion::CURRENT, journal_schema);
    }

    fn write_manifest_versions(&self, version: u16, wire: WireVersion, journal_schema: u32) {
        let mut members = [AUTHORITY, "tape/lane-a"]
            .into_iter()
            .map(|path| {
                let bytes = fs::read(self.bundle.join(path)).expect("member reads");
                JournalBackupMember {
                    path: path.to_owned(),
                    mode: mode_of(&self.bundle.join(path)),
                    bytes: u64::try_from(bytes.len()).expect("member length crosses"),
                    digest: Digest::compute(Domain::JournalBackupMember, &bytes),
                }
            })
            .collect::<Vec<_>>();
        members.sort_by(|left, right| left.path.cmp(&right.path));
        self.persist_manifest(&JournalBackupManifest {
            version,
            wire,
            journal_schema,
            authority: AUTHORITY.to_owned(),
            members,
        });
    }

    fn rewrite_manifest(&self, rewrite: impl FnOnce(&mut JournalBackupManifest)) {
        let bytes = fs::read(self.bundle.join("manifest.json")).expect("manifest reads");
        let mut manifest = wire::decode::<JournalBackupManifest>(&bytes).expect("manifest decodes");
        rewrite(&mut manifest);
        self.persist_manifest(&manifest);
    }

    fn persist_manifest(&self, manifest: &JournalBackupManifest) {
        fs::write(
            self.bundle.join("manifest.json"),
            wire::encode(manifest).expect("manifest encodes"),
        )
        .expect("manifest writes");
    }

    fn verify(&self) -> Outcome<RestoreVerification> {
        verify_bundle(&self.bundle)
    }

    fn sql(&self, statement: impl FnOnce(&rusqlite::Connection)) {
        let connection =
            rusqlite::Connection::open(self.bundle.join(AUTHORITY)).expect("authority opens");
        statement(&connection);
    }
}

#[test]
fn a_valid_bundle_verifies_its_members_and_digests_without_mutation() {
    let fixture = Bundle::new("valid");
    let before = tree(fixture.root());

    let Outcome::Reply(reply) = fixture.verify() else {
        panic!("a valid bundle must reply");
    };

    assert_eq!(reply.position, ReplyPosition::Direct {});
    assert_eq!(reply.body.wire, WireVersion::CURRENT);
    assert_eq!(reply.body.journal_schema, journal::SCHEMA_VERSION);
    assert_eq!(reply.body.members, 2);
    assert_eq!(
        (
            reply.body.facts,
            reply.body.receipts,
            reply.body.heads,
            reply.body.threads
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(
        reply.body.authority,
        Digest::compute(
            Domain::JournalBackupMember,
            &fs::read(fixture.bundle.join(AUTHORITY)).expect("authority reads")
        )
    );
    assert_eq!(
        reply.body.bundle,
        Digest::compute(
            Domain::JournalBackupManifest,
            &fs::read(fixture.bundle.join("manifest.json")).expect("manifest reads")
        )
    );
    assert_eq!(
        tree(fixture.root()),
        before,
        "verification mutated its inputs"
    );
}

#[test]
fn missing_forward_and_incomplete_bundles_refuse_by_class() {
    let missing = Scratch::new("missing");
    assert_refusal(
        &verify_bundle(&missing.0.join("absent")),
        "restore::bundle-incomplete",
    );

    let forward = Bundle::new("forward");
    forward.write_manifest(JOURNAL_BACKUP_VERSION, journal::SCHEMA_VERSION + 1);
    assert_refusal(&forward.verify(), "restore::schema-forward");

    let retired = Bundle::new("retired-bundle");
    retired.write_manifest(JOURNAL_BACKUP_VERSION + 1, journal::SCHEMA_VERSION);
    assert_refusal(&retired.verify(), "restore::bundle-version");

    let foreign = Bundle::new("foreign-wire");
    foreign.write_manifest_versions(
        JOURNAL_BACKUP_VERSION,
        WireVersion(WireVersion::CURRENT.0 + 1),
        journal::SCHEMA_VERSION,
    );
    assert_refusal(&foreign.verify(), "restore::wire-version");

    let hidden = Bundle::new("hidden-forward");
    hidden.sql(|connection| {
        connection
            .execute(
                "UPDATE journal_schema SET version = ?1 WHERE singleton = 1",
                [journal::SCHEMA_VERSION + 1],
            )
            .expect("authority schema advances");
        connection
            .pragma_update(None, "user_version", journal::SCHEMA_VERSION + 1)
            .expect("authority pragma advances");
    });
    hidden.write_manifest(JOURNAL_BACKUP_VERSION, journal::SCHEMA_VERSION);
    assert_refusal(&hidden.verify(), "restore::schema-forward");

    let incomplete = Bundle::new("incomplete");
    fs::remove_file(incomplete.bundle.join("tape/lane-a")).expect("member removes");
    assert_refusal(&incomplete.verify(), "restore::bundle-incomplete");

    let extra = Bundle::new("extra");
    fs::write(extra.bundle.join("unmanifested"), b"not admitted").expect("extra member writes");
    assert_refusal(&extra.verify(), "restore::bundle-incomplete");

    let duplicate = Bundle::new("duplicate");
    duplicate.rewrite_manifest(|manifest| manifest.members.push(manifest.members[0].clone()));
    assert_refusal(&duplicate.verify(), "restore::bundle-incomplete");

    let unordered = Bundle::new("unordered");
    unordered.rewrite_manifest(|manifest| manifest.members.reverse());
    assert_refusal(&unordered.verify(), "restore::bundle-incomplete");
}

#[test]
fn corrupt_or_inconsistent_bundles_fault_as_store_evidence_without_mutation() {
    let manifest = Bundle::new("manifest-corrupt");
    fs::write(manifest.bundle.join("manifest.json"), b"not a manifest").expect("manifest corrupts");
    assert_store_fault(&manifest.verify());

    let noncanonical = Bundle::new("noncanonical");
    let mut bytes = fs::read(noncanonical.bundle.join("manifest.json")).expect("manifest reads");
    bytes.push(b'\n');
    fs::write(noncanonical.bundle.join("manifest.json"), bytes).expect("manifest rewrites");
    assert_store_fault(&noncanonical.verify());

    let member = Bundle::new("member-corrupt");
    fs::write(member.bundle.join("tape/lane-a"), b"changed after backup").expect("member corrupts");
    assert_store_fault(&member.verify());

    let mode = Bundle::new("mode");
    set_mode(&mode.bundle.join("tape/lane-a"), 0o400);
    assert_store_fault(&mode.verify());

    let declared_mode = Bundle::new("declared-mode");
    declared_mode.rewrite_manifest(|manifest| manifest.members[0].mode = 0o10_000);
    assert_store_fault(&declared_mode.verify());

    let length = Bundle::new("length");
    length.rewrite_manifest(|manifest| manifest.members[0].bytes += 1);
    assert_store_fault(&length.verify());

    let digest = Bundle::new("declared-digest");
    digest.rewrite_manifest(|manifest| {
        manifest.members[0].digest =
            Digest::compute(Domain::JournalBackupMember, b"another generation");
    });
    assert_store_fault(&digest.verify());

    let stale = Bundle::new("stale-schema");
    stale.write_manifest(JOURNAL_BACKUP_VERSION, journal::SCHEMA_VERSION - 1);
    assert_store_fault(&stale.verify());

    let traversal = Bundle::new("traversal");
    traversal.rewrite_manifest(|manifest| manifest.authority = "../authority".to_owned());
    assert_store_fault(&traversal.verify());

    let replay = Bundle::new("replay-corrupt");
    replay.sql(|connection| {
        connection
            .execute("DELETE FROM cursor_head", [])
            .expect("cursor singleton corrupts");
    });
    replay.write_manifest(JOURNAL_BACKUP_VERSION, journal::SCHEMA_VERSION);
    let before = tree(replay.root());
    assert_store_fault(&replay.verify());
    assert_eq!(
        tree(replay.root()),
        before,
        "a failed replay mutated the bundle"
    );
}

#[test]
fn a_manifest_bound_truncated_authority_is_refused_at_captured_byte_admission() {
    let fixture = Bundle::new("truncated-authority");
    let authority = fixture.bundle.join(AUTHORITY);
    fs::OpenOptions::new()
        .write(true)
        .open(&authority)
        .expect("authority opens for truncation")
        .set_len(64)
        .expect("authority truncates");
    fixture.write_manifest(JOURNAL_BACKUP_VERSION, journal::SCHEMA_VERSION);
    let before = tree(fixture.root());

    let Outcome::Fault(fault) = fixture.verify() else {
        panic!("a truncated authority must fault");
    };

    assert_eq!(fault.class, FaultClass::Store);
    assert_eq!(fault.reason, "restore::bundle-invalid");
    assert!(
        fault.detail.contains("backup authority was refused"),
        "member authentication stopped before captured-byte admission: {fault:?}"
    );
    assert_eq!(
        tree(fixture.root()),
        before,
        "failed admission mutated the bundle"
    );
}

#[test]
fn links_inside_a_bundle_are_store_evidence() {
    let linked = Bundle::new("link");
    fs::remove_file(linked.bundle.join("tape/lane-a")).expect("member removes");
    authority::link_directory(&linked.bundle, &linked.bundle.join("tape/lane-a"))
        .expect("link creates");
    assert_store_fault(&linked.verify());
}

#[cfg(unix)]
#[test]
fn special_files_inside_a_bundle_are_store_evidence() {
    use std::os::unix::net::UnixListener;

    let special = Bundle::new("special");
    let _socket =
        UnixListener::bind(special.bundle.join("special.sock")).expect("special socket binds");
    assert_store_fault(&special.verify());
}

#[test]
fn a_read_only_bundle_verifies_without_changing_bytes_or_modes() {
    let fixture = Bundle::new("read-only");
    set_mode(&fixture.bundle.join(AUTHORITY), 0o400);
    set_mode(&fixture.bundle.join("tape/lane-a"), 0o400);
    fixture.write_manifest(JOURNAL_BACKUP_VERSION, journal::SCHEMA_VERSION);
    set_mode(&fixture.bundle.join("manifest.json"), 0o400);
    set_mode(&fixture.bundle.join("tape"), 0o500);
    set_mode(&fixture.bundle, 0o500);
    let before = tree(fixture.root());

    assert!(
        matches!(fixture.verify(), Outcome::Reply(_)),
        "read-only verification must reply"
    );
    assert_eq!(tree(fixture.root()), before, "read-only inputs changed");
}

#[test]
fn interrupted_staging_preserves_the_predecessor_authority() {
    let scratch = Scratch::new("publication");
    let root = &scratch.0;
    let destination = root.join(AUTHORITY);
    let predecessor = b"predecessor authority";
    fs::write(&destination, predecessor).expect("write predecessor authority");
    let mut captured = capture_current_authority(&destination)
        .expect("predecessor capture succeeds")
        .expect("predecessor exists");
    assert_eq!(captured.bytes, predecessor.len() as u64);
    assert_eq!(
        captured.digest,
        Digest::compute(Domain::JournalBackupMember, predecessor)
    );

    let error = publish_authority_with(&destination, |staged| {
        staged.write_all(b"partial replacement")?;
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "injected staging interruption",
        ))
    })
    .expect_err("an interrupted staged write must refuse publication");

    let Failure::Fault(fault) = error else {
        panic!("an interrupted staged write was not an environment fault");
    };
    assert_eq!(fault.class, FaultClass::Environment);
    assert_eq!(fault.reason, "restore::stage-failed");
    assert_eq!(
        fs::read(&destination).expect("read predecessor after interruption"),
        predecessor,
        "interrupted staging replaced the predecessor authority",
    );
    let entries = fs::read_dir(root)
        .expect("read restore publication fixture")
        .map(|entry| entry.expect("restore publication entry").path())
        .collect::<Vec<_>>();
    assert_eq!(
        entries.as_slice(),
        std::slice::from_ref(&destination),
        "interrupted staging left a temporary publication behind",
    );

    fs::write(&destination, b"later authority").expect("replace authority after capture");
    publish_authority_with(&destination, |staged| captured.copy_to(staged).map(|_| ()))
        .expect("captured predecessor republishes");
    assert_eq!(
        fs::read(&destination).expect("read republished predecessor"),
        predecessor,
    );
}

fn assert_refusal(outcome: &Outcome<RestoreVerification>, reason: &str) {
    let Outcome::Refusal(refusal) = outcome else {
        panic!("expected a typed refusal, got {outcome:?}");
    };
    assert_eq!(refusal.reason(), reason);
}

fn assert_store_fault(outcome: &Outcome<RestoreVerification>) {
    let Outcome::Fault(fault) = outcome else {
        panic!("expected a typed fault, got {outcome:?}");
    };
    assert_eq!(fault.class, FaultClass::Store);
    assert_eq!(fault.reason, "restore::bundle-invalid");
}

fn mode_of(path: &Path) -> u32 {
    authority::owner_mode(path).expect("fixture mode reads")
}

fn set_mode(path: &Path, mode: u32) {
    authority::set_owner_mode(path, mode).expect("fixture mode changes");
}

fn tree(root: &Path) -> BTreeMap<String, (u32, Option<Vec<u8>>)> {
    let mut state = BTreeMap::new();
    walk(root, root, &mut state);
    state
}

fn walk(root: &Path, directory: &Path, state: &mut BTreeMap<String, (u32, Option<Vec<u8>>)>) {
    let mut entries = fs::read_dir(directory)
        .expect("snapshot directory reads")
        .collect::<Result<Vec<_>, _>>()
        .expect("snapshot entries read");
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .expect("snapshot entry belongs to root")
            .to_string_lossy()
            .into_owned();
        let mode = mode_of(&path);
        if entry.file_type().expect("snapshot file type").is_dir() {
            state.insert(relative, (mode, None));
            walk(root, &path, state);
        } else {
            state.insert(
                relative,
                (mode, Some(fs::read(path).expect("snapshot file reads"))),
            );
        }
    }
}
