use std::fs;
use std::io::Write as _;
use std::path::Path;

use gix::bstr::BStr;
use gix::index::entry::{Flags, Mode, Stage, Stat};
use gix::object::tree::EntryKind;

const MAIN_BRANCH: &str = "refs/heads/main";
const FIXTURE_STAMP: &str = "1700000000 +0000";

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
        "[core]\n\tfilemode = true\n\tlogAllRefUpdates = true"
    )
    .expect("configure fixture repository");
}

pub(crate) fn stage(root: &Path, paths: &[&str]) {
    let repository = open(root);
    let mut index = repository
        .index_or_load_from_head_or_empty()
        .expect("load fixture index")
        .into_owned();
    stage_paths(&repository, &mut index, root, paths);
    persist_index(&mut index);
}

pub(crate) fn stage_at(root: &Path, index_path: &Path, paths: &[&str]) {
    let repository = open(root);
    let mut index = open_index(&repository, index_path);
    stage_paths(&repository, &mut index, root, paths);
    persist_index(&mut index);
}

pub(crate) fn stage_all(root: &Path) {
    let repository = open(root);
    let mut index = repository
        .index_or_load_from_head_or_empty()
        .expect("load fixture index")
        .into_owned();
    index.remove_entries(|_, _, _| true);
    let files = worktree_files(root);
    let paths = files.iter().map(String::as_str).collect::<Vec<_>>();
    stage_paths(&repository, &mut index, root, &paths);
    persist_index(&mut index);
}

pub(crate) fn commit(root: &Path, message: &str) -> String {
    let repository = open(root);
    let tree = write_index_tree(
        &repository,
        open_index(&repository, &repository.index_path()),
    );
    let parent = repository.head_id().ok().map(gix::Id::detach);
    let signature = gix::actor::SignatureRef {
        name: "Spice Fixture".into(),
        email: "spice@example.invalid".into(),
        time: FIXTURE_STAMP,
    };
    repository
        .commit_as(signature, signature, MAIN_BRANCH, message, tree, parent)
        .expect("commit fixture tree")
        .detach()
        .to_string()
}

pub(crate) fn write_tree(root: &Path, index_path: Option<&Path>) -> String {
    let repository = open(root);
    let index = index_path.map_or_else(
        || open_index(&repository, &repository.index_path()),
        |path| open_index(&repository, path),
    );
    write_index_tree(&repository, index).to_string()
}

#[cfg(unix)]
pub(crate) fn index_mode(root: &Path, path: &str) -> Mode {
    let repository = open(root);
    let index = repository.index().expect("open fixture index");
    index
        .entries()
        .iter()
        .find(|entry| entry.path(&index) == path.as_bytes())
        .expect("fixture index contains path")
        .mode
}

pub(crate) fn create_linked_worktree(root: &Path, linked: &Path, branch: &str) {
    let id = linked.file_name().expect("linked worktree has a name");
    let admin = root.join(".git/worktrees").join(id);
    fs::create_dir_all(&admin).expect("create linked worktree administration");
    fs::create_dir(linked).expect("create linked fixture worktree");
    let reference = format!("refs/heads/{branch}");
    let head = open(root)
        .head_id()
        .expect("fixture HEAD resolves")
        .detach();
    let reference_path = root.join(".git").join(&reference);
    fs::create_dir_all(reference_path.parent().expect("branch parent"))
        .expect("create fixture branch parent");
    fs::write(reference_path, format!("{head}\n")).expect("create fixture branch");
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
    .expect("write linked worktree backlink");
    fs::write(admin.join("HEAD"), format!("ref: {reference}\n"))
        .expect("write linked worktree HEAD");
    fs::copy(root.join(".git/index"), admin.join("index")).expect("copy linked worktree index");
    for path in worktree_files(root) {
        let destination = linked.join(&path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).expect("create linked worktree path parent");
        }
        fs::copy(root.join(path), destination).expect("copy linked worktree path");
    }
}

fn open(root: &Path) -> gix::Repository {
    gix::open_opts(root, gix::open::Options::isolated()).expect("open fixture repository")
}

fn open_index(repository: &gix::Repository, path: &Path) -> gix::index::File {
    gix::index::File::at(
        path,
        repository.object_hash(),
        false,
        gix::index::decode::Options::default(),
    )
    .expect("load alternate fixture index")
}

fn stage_paths(
    repository: &gix::Repository,
    index: &mut gix::index::File,
    root: &Path,
    paths: &[&str],
) {
    for path in paths {
        index.remove_entries(|_, candidate, _| candidate == path.as_bytes());
        let absolute = root.join(path);
        if !absolute.exists() {
            continue;
        }
        let metadata = fs::metadata(&absolute).expect("inspect fixture path");
        let mode = if executable(&metadata) {
            Mode::FILE_EXECUTABLE
        } else {
            Mode::FILE
        };
        let id = repository
            .write_blob(fs::read(&absolute).expect("read fixture path"))
            .expect("write fixture blob")
            .detach();
        index.dangerously_push_entry(
            Stat::default(),
            id,
            Flags::from_stage(Stage::Unconflicted),
            mode,
            BStr::new(path.as_bytes()),
        );
    }
}

fn write_index_tree(repository: &gix::Repository, index: gix::index::File) -> gix::ObjectId {
    let mut editor = repository.empty_tree().edit().expect("edit fixture tree");
    for entry in index.entries() {
        let kind = match entry.mode {
            Mode::FILE => EntryKind::Blob,
            Mode::FILE_EXECUTABLE => EntryKind::BlobExecutable,
            unsupported => panic!("unsupported fixture index mode {unsupported:?}"),
        };
        editor
            .upsert(entry.path(&index), kind, entry.id)
            .expect("insert fixture tree entry");
    }
    editor.write().expect("write fixture tree").detach()
}

fn worktree_files(root: &Path) -> Vec<String> {
    fn collect(root: &Path, current: &Path, files: &mut Vec<String>) {
        for entry in fs::read_dir(current).expect("read fixture directory") {
            let entry = entry.expect("read fixture entry");
            if entry.file_name() == ".git" {
                continue;
            }
            if entry.file_type().expect("inspect fixture entry").is_dir() {
                collect(root, &entry.path(), files);
            } else {
                files.push(
                    entry
                        .path()
                        .strip_prefix(root)
                        .expect("fixture path is relative")
                        .to_str()
                        .expect("fixture path is UTF-8")
                        .to_owned(),
                );
            }
        }
    }

    let mut files = Vec::new();
    collect(root, root, &mut files);
    files.sort();
    files
}

#[cfg(unix)]
fn executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_metadata: &fs::Metadata) -> bool {
    false
}

fn persist_index(index: &mut gix::index::File) {
    index.sort_entries();
    index.remove_tree();
    index
        .write(gix::index::write::Options::default())
        .expect("write fixture index");
}
