use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use gix::bstr::BStr;
use gix::index::entry::{Flags, Mode, Stage, Stat};
use gix::object::tree::EntryKind;

const MAIN_BRANCH: &str = "refs/heads/main";
const FIXTURE_STAMP: &str = "1700000000 +0000";
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

pub(crate) struct Fixture {
    root: PathBuf,
}

impl Fixture {
    pub(crate) fn repository(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "gate-{label}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&root).expect("create Gate test fixture");
        initialize(&root);
        Self { root }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.root
    }

    pub(crate) fn join(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.join(relative)
    }

    pub(crate) fn write(&self, relative: &str, bytes: impl AsRef<[u8]>) {
        let path = self.join(relative);
        fs::create_dir_all(path.parent().expect("fixture path has a parent"))
            .expect("create fixture path parent");
        fs::write(path, bytes).expect("write fixture path");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove Gate test fixture");
    }
}

pub(crate) fn initialize(root: &Path) {
    gix::ThreadSafeRepository::init_opts(
        root,
        gix::create::Kind::WithWorktree,
        gix::create::Options::default(),
        gix::open::Options::isolated(),
    )
    .expect("initialize fixture repository");
    fs::write(root.join(".git/HEAD"), format!("ref: {MAIN_BRANCH}\n"))
        .expect("select fixture main branch");
    writeln!(
        fs::OpenOptions::new()
            .append(true)
            .open(root.join(".git/config"))
            .expect("open fixture repository config"),
        "[core]\n\tlogAllRefUpdates = true"
    )
    .expect("enable fixture reflogs");
}

pub(crate) fn stage(root: &Path, paths: &[&str]) {
    let repository = open(root);
    let mut index = repository
        .index_or_load_from_head_or_empty()
        .expect("load fixture index")
        .into_owned();
    for path in paths {
        index.remove_entries(|_, candidate, _| candidate == path.as_bytes());
        if root.join(path).exists() {
            push_worktree_entry(&repository, &mut index, root, path);
        }
    }
    persist_index(&mut index);
}

pub(crate) fn stage_all(root: &Path) {
    let repository = open(root);
    let mut index = repository
        .index_or_load_from_head_or_empty()
        .expect("load fixture index")
        .into_owned();
    index.remove_entries(|_, _, _| true);
    for path in worktree_files(root) {
        push_worktree_entry(&repository, &mut index, root, &path);
    }
    persist_index(&mut index);
}

pub(crate) fn install_conflict_stages(root: &Path, path: &str) {
    let repository = open(root);
    let mut index = repository
        .index_or_load_from_head_or_empty()
        .expect("load fixture index")
        .into_owned();
    index.remove_entries(|_, candidate, _| candidate == path.as_bytes());
    for (stage, bytes) in [
        (Stage::Base, b"base\n".as_slice()),
        (Stage::Ours, b"ours\n".as_slice()),
        (Stage::Theirs, b"theirs\n".as_slice()),
    ] {
        let id = repository
            .write_blob(bytes)
            .expect("write conflict-stage blob")
            .detach();
        index.dangerously_push_entry(
            Stat::default(),
            id,
            Flags::from_stage(stage),
            Mode::FILE,
            BStr::new(path.as_bytes()),
        );
    }
    persist_index(&mut index);
}

pub(crate) fn commit(root: &Path, message: &str) -> String {
    let repository = open(root);
    let index = repository.index().expect("open fixture index");
    let mut editor = repository.empty_tree().edit().expect("edit fixture tree");
    for entry in index.entries() {
        let kind = match entry.mode {
            Mode::FILE => EntryKind::Blob,
            Mode::FILE_EXECUTABLE => EntryKind::BlobExecutable,
            Mode::SYMLINK => EntryKind::Link,
            unsupported => panic!("unsupported fixture index mode {unsupported:?}"),
        };
        editor
            .upsert(entry.path(&index), kind, entry.id)
            .expect("insert fixture tree entry");
    }
    let tree = editor.write().expect("write fixture tree").detach();
    let parent = repository.head_id().ok().map(gix::Id::detach);
    let signature = gix::actor::SignatureRef {
        name: "Gate Fixture".into(),
        email: "gate@example.invalid".into(),
        time: FIXTURE_STAMP,
    };
    let reference = current_reference(&repository);
    let commit = repository
        .commit_as(signature, signature, reference, message, tree, parent)
        .expect("commit fixture tree")
        .detach();
    append_head_reflog(&repository, parent, commit, &format!("commit: {message}"));
    commit.to_string()
}

pub(crate) fn create_linked_worktree(root: &Path, linked: &Path, branch: &str) {
    let id = linked
        .file_name()
        .expect("linked worktree has a name")
        .to_string_lossy();
    let admin = root.join(".git/worktrees").join(id.as_ref());
    fs::create_dir_all(&admin).expect("create linked worktree administration");
    fs::create_dir(linked).expect("create linked fixture worktree");
    let reference = format!("refs/heads/{branch}");
    create_branch(root, branch);
    fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .expect("write linked worktree gitfile");
    fs::write(admin.join("commondir"), "../..\n").expect("write linked common directory");
    fs::write(
        admin.join("gitdir"),
        format!("{}\n", linked.join(".git").display()),
    )
    .expect("write linked gitfile backlink");
    fs::write(admin.join("HEAD"), format!("ref: {reference}\n"))
        .expect("write linked worktree HEAD");
    fs::copy(root.join(".git/index"), admin.join("index")).expect("copy linked worktree index");

    let repository = open(root);
    let entries = commit_entries(
        &repository,
        repository
            .head_id()
            .expect("fixture HEAD resolves")
            .detach(),
    );
    materialize_entries(linked, &repository, &entries);
}

pub(crate) fn remove_linked_worktree(root: &Path, linked: &Path) {
    let id = linked
        .file_name()
        .expect("linked worktree has a name")
        .to_string_lossy();
    fs::remove_dir_all(linked).expect("remove linked fixture worktree");
    fs::remove_dir_all(root.join(".git/worktrees").join(id.as_ref()))
        .expect("remove linked fixture administration");
}

fn set_ref(root: &Path, reference: &str, id: &str) {
    let path = root.join(".git").join(reference);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture reference directory");
    }
    fs::write(path, format!("{id}\n")).expect("write fixture reference");
}

fn create_branch(root: &Path, branch: &str) {
    set_ref(root, &format!("refs/heads/{branch}"), &head(root));
}

fn head(root: &Path) -> String {
    open(root)
        .head_id()
        .expect("fixture HEAD resolves")
        .detach()
        .to_string()
}

fn open(root: &Path) -> gix::Repository {
    gix::open_opts(root, gix::open::Options::isolated()).expect("open fixture repository")
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IndexEntry {
    id: gix::ObjectId,
    mode: Mode,
}

fn current_reference(repository: &gix::Repository) -> String {
    fs::read_to_string(repository.git_dir().join("HEAD"))
        .expect("read fixture HEAD")
        .strip_prefix("ref: ")
        .expect("fixture HEAD is symbolic")
        .trim()
        .to_owned()
}

fn append_head_reflog(
    repository: &gix::Repository,
    previous: Option<gix::ObjectId>,
    current: gix::ObjectId,
    message: &str,
) {
    let path = repository.git_dir().join("logs/HEAD");
    fs::create_dir_all(path.parent().expect("HEAD reflog has a parent"))
        .expect("create fixture HEAD reflog directory");
    let previous = previous.map_or_else(|| "0".repeat(40), |id| id.to_string());
    writeln!(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open fixture HEAD reflog"),
        "{previous} {current} Gate Fixture <gate@example.invalid> {FIXTURE_STAMP}\t{message}"
    )
    .expect("append fixture HEAD reflog");
}

fn commit_entries(
    repository: &gix::Repository,
    commit: gix::ObjectId,
) -> BTreeMap<String, IndexEntry> {
    let tree = repository
        .find_commit(commit)
        .expect("find fixture commit")
        .tree_id()
        .expect("find fixture commit tree");
    let index = repository
        .index_from_tree(&tree)
        .expect("expand fixture commit tree");
    index
        .entries()
        .iter()
        .map(|entry| {
            (
                entry.path(&index).to_string(),
                IndexEntry {
                    id: entry.id,
                    mode: entry.mode,
                },
            )
        })
        .collect()
}

fn blob(repository: &gix::Repository, id: gix::ObjectId) -> Vec<u8> {
    repository
        .find_blob(id)
        .expect("find fixture blob")
        .data
        .to_vec()
}

fn materialize_entries(
    root: &Path,
    repository: &gix::Repository,
    entries: &BTreeMap<String, IndexEntry>,
) {
    for entry in fs::read_dir(root).expect("read fixture root") {
        let entry = entry.expect("read fixture root entry");
        if entry.file_name() == ".git" {
            continue;
        }
        if entry.file_type().expect("read fixture entry kind").is_dir() {
            fs::remove_dir_all(entry.path()).expect("clear fixture directory");
        } else {
            fs::remove_file(entry.path()).expect("clear fixture file");
        }
    }
    for (relative, entry) in entries {
        let target = root.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).expect("create fixture checkout parent");
        }
        fs::write(&target, blob(repository, entry.id)).expect("write fixture checkout path");
        set_executable(&target, entry.mode == Mode::FILE_EXECUTABLE);
    }
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) {
    use std::os::unix::fs::PermissionsExt as _;

    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .expect("set fixture checkout permissions");
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) {}

fn persist_index(index: &mut gix::index::File) {
    index.sort_entries();
    index.remove_tree();
    index
        .write(gix::index::write::Options::default())
        .expect("write fixture index");
}

fn push_worktree_entry(
    repository: &gix::Repository,
    index: &mut gix::index::File,
    root: &Path,
    relative: &str,
) {
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path).expect("read fixture path metadata");
    let (bytes, mode) = if metadata.file_type().is_symlink() {
        (
            fs::read_link(&path)
                .expect("read fixture symlink")
                .to_string_lossy()
                .into_owned()
                .into_bytes(),
            Mode::SYMLINK,
        )
    } else {
        let mode = executable_mode(&metadata);
        (fs::read(&path).expect("read fixture file"), mode)
    };
    let id = repository
        .write_blob(&bytes)
        .expect("write fixture blob")
        .detach();
    let stat = Stat {
        size: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
        ..Stat::default()
    };
    index.dangerously_push_entry(
        stat,
        id,
        Flags::empty(),
        mode,
        BStr::new(relative.as_bytes()),
    );
}

#[cfg(unix)]
fn executable_mode(metadata: &fs::Metadata) -> Mode {
    use std::os::unix::fs::PermissionsExt as _;

    if metadata.permissions().mode() & 0o111 == 0 {
        Mode::FILE
    } else {
        Mode::FILE_EXECUTABLE
    }
}

#[cfg(not(unix))]
fn executable_mode(_metadata: &fs::Metadata) -> Mode {
    Mode::FILE
}

fn worktree_files(root: &Path) -> Vec<String> {
    let mut pending = vec![PathBuf::new()];
    let mut files = Vec::new();
    while let Some(relative) = pending.pop() {
        for entry in fs::read_dir(root.join(&relative)).expect("read fixture worktree") {
            let entry = entry.expect("read fixture worktree entry");
            let name = entry.file_name();
            if relative.as_os_str().is_empty() && name == ".git" {
                continue;
            }
            let path = relative.join(name);
            if entry.file_type().expect("read fixture entry kind").is_dir() {
                pending.push(path);
            } else {
                files.push(
                    path.to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/"),
                );
            }
        }
    }
    files.sort();
    files
}
