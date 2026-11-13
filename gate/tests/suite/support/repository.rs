use std::fs;
use std::path::{Path, PathBuf};

use gix::bstr::BStr;
use gix::index::entry::{Flags, Mode, Stat};
use gix::object::tree::EntryKind;

use super::Tree;

const MAIN_BRANCH: &str = "refs/heads/main";
const FIXTURE_STAMP: &str = "1700000000 +0000";

impl Tree {
    pub(crate) fn initialize(&self) {
        gix::ThreadSafeRepository::init_opts(
            &self.root,
            gix::create::Kind::WithWorktree,
            gix::create::Options::default(),
            gix::open::Options::isolated(),
        )
        .expect("initialize fixture repository");
        fs::write(self.root.join(".git/HEAD"), format!("ref: {MAIN_BRANCH}\n"))
            .expect("select fixture main branch");
    }

    pub(crate) fn stage_all(&self) {
        let repository = self.open_repository();
        let mut index = repository
            .index_or_load_from_head_or_empty()
            .expect("load fixture index")
            .into_owned();
        index.remove_entries(|_, _, _| true);
        for path in worktree_files(&self.root) {
            push_worktree_entry(&repository, &mut index, &self.root, &path);
        }
        persist_index(&mut index);
    }

    pub(crate) fn stage(&self, paths: &[&str]) {
        let repository = self.open_repository();
        let mut index = repository
            .index_or_load_from_head_or_empty()
            .expect("load fixture index")
            .into_owned();
        for path in paths {
            index.remove_entries(|_, candidate, _| candidate == path.as_bytes());
            if self.root.join(path).exists() {
                push_worktree_entry(&repository, &mut index, &self.root, path);
            }
        }
        persist_index(&mut index);
    }

    pub(crate) fn stage_submodule(&self, path: &str, commit: &str) {
        let repository = self.open_repository();
        let mut index = repository
            .index_or_load_from_head_or_empty()
            .expect("load fixture index")
            .into_owned();
        index.remove_entries(|_, candidate, _| candidate == path.as_bytes());
        index.dangerously_push_entry(
            Stat::default(),
            commit.parse().expect("parse fixture commit id"),
            Flags::empty(),
            Mode::COMMIT,
            BStr::new(path.as_bytes()),
        );
        persist_index(&mut index);
    }

    pub(crate) fn staged_object(&self, path: &str) -> String {
        let repository = self.open_repository();
        let index = repository.index().expect("open fixture index");
        let entry = index
            .entry_index_by_path(BStr::new(path.as_bytes()))
            .ok()
            .map(|position| index.entry(position))
            .expect("fixture path is staged");
        entry.id.to_string()
    }

    pub(crate) fn create_ambiguous_ref(&self, name: &str) {
        let head = self.head();
        for class in ["heads", "tags"] {
            let path = self.root.join(".git/refs").join(class).join(name);
            fs::create_dir_all(path.parent().expect("fixture ref has a parent"))
                .expect("create fixture ref directory");
            fs::write(path, format!("{head}\n")).expect("write fixture ref");
        }
    }

    pub(crate) fn commit(&self, message: &str) -> String {
        let repository = self.open_repository();
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
        repository
            .commit_as(signature, signature, MAIN_BRANCH, message, tree, parent)
            .expect("commit fixture tree")
            .detach()
            .to_string()
    }

    pub(crate) fn head(&self) -> String {
        self.open_repository()
            .head_id()
            .expect("fixture HEAD resolves")
            .detach()
            .to_string()
    }

    fn open_repository(&self) -> gix::Repository {
        gix::open_opts(&self.root, gix::open::Options::isolated()).expect("open fixture repository")
    }
}

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
        let directory = root.join(&relative);
        for entry in fs::read_dir(&directory).expect("read fixture worktree") {
            let entry = entry.expect("read fixture worktree entry");
            let name = entry.file_name();
            if relative.as_os_str().is_empty() && name == ".git" {
                continue;
            }
            let path = relative.join(name);
            let kind = entry.file_type().expect("read fixture entry kind");
            if kind.is_dir() {
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
