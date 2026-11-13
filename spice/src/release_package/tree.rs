use std::collections::BTreeMap;
use std::io::{self, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};

use sha1::{Digest as _, Sha1};

use super::{ReleaseBudget, StreamBudget};

enum Node<'a> {
    Directory(BTreeMap<String, Node<'a>>),
    File(&'a proof::PackageMember),
}

pub(super) fn identity(
    members: &[proof::PackageMember],
    budget: &ReleaseBudget,
) -> Result<String, String> {
    let root = tree(members)?;
    let mut stream = budget.stream();
    let identity = hash_node(&root, &mut stream)?;
    stream.finish()?;
    Ok(hex(&identity))
}

pub(super) fn write(
    root: &Path,
    members: &[proof::PackageMember],
    budget: &ReleaseBudget,
) -> Result<String, String> {
    let tree = tree(members)?;
    let mut stream = budget.stream();
    let identity = write_node(root, &tree, &mut stream)?;
    stream.finish()?;
    Ok(hex(&identity))
}

fn tree(members: &[proof::PackageMember]) -> Result<Node<'_>, String> {
    let mut root = Node::Directory(BTreeMap::new());
    for member in members {
        insert(&mut root, member.path(), member)?;
    }
    Ok(root)
}

fn insert<'a>(
    node: &mut Node<'a>,
    path: &str,
    member: &'a proof::PackageMember,
) -> Result<(), String> {
    let (name, tail) = path
        .split_once('/')
        .map_or((path, None), |(name, tail)| (name, Some(tail)));
    let Node::Directory(children) = node else {
        return Err(format!("package path collides with a file: {path}"));
    };
    if let Some(tail) = tail {
        let child = children
            .entry(name.to_owned())
            .or_insert_with(|| Node::Directory(BTreeMap::new()));
        insert(child, tail, member)
    } else if children
        .insert(name.to_owned(), Node::File(member))
        .is_some()
    {
        Err(format!("package path occurs twice: {path}"))
    } else {
        Ok(())
    }
}

fn hash_node(node: &Node<'_>, budget: &mut StreamBudget<'_>) -> Result<[u8; 20], String> {
    match node {
        Node::File(member) => hash_member(member, budget),
        Node::Directory(children) => {
            tree_body(children, |child| hash_node(child, budget)).map(|body| hash("tree", &body))
        }
    }
}

fn write_node(
    root: &Path,
    node: &Node<'_>,
    budget: &mut StreamBudget<'_>,
) -> Result<[u8; 20], String> {
    match node {
        Node::File(member) => write_member_object(root, member, budget),
        Node::Directory(children) => {
            let body = tree_body(children, |child| write_node(root, child, budget))?;
            write_object(root, "tree", &body)
        }
    }
}

fn tree_body(
    children: &BTreeMap<String, Node<'_>>,
    mut identity: impl FnMut(&Node<'_>) -> Result<[u8; 20], String>,
) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    for (name, child) in children {
        let (mode, object) = match child {
            Node::Directory(_) => ("40000", identity(child)?),
            Node::File(member) if member.mode() == 0o644 => ("100644", identity(child)?),
            Node::File(member) if member.mode() == 0o755 => ("100755", identity(child)?),
            Node::File(member) => {
                return Err(format!(
                    "package member {name} has mode {:o}",
                    member.mode()
                ));
            }
        };
        body.extend_from_slice(mode.as_bytes());
        body.push(b' ');
        body.extend_from_slice(name.as_bytes());
        body.push(0);
        body.extend_from_slice(&object);
    }
    Ok(body)
}

fn hash_member(
    member: &proof::PackageMember,
    budget: &mut StreamBudget<'_>,
) -> Result<[u8; 20], String> {
    budget.admit(member)?;
    let mut hash = Sha1::new();
    hash.update(format!("blob {}\0", member.length()).as_bytes());
    member
        .copy_to(&mut HashWriter(&mut hash))
        .map_err(|error| error.to_string())?;
    Ok(hash.finalize().into())
}

struct HashWriter<'a>(&'a mut Sha1);

impl io::Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn hash(kind: &str, bytes: &[u8]) -> [u8; 20] {
    let mut hash = Sha1::new();
    hash.update(format!("{kind} {}\0", bytes.len()).as_bytes());
    hash.update(bytes);
    hash.finalize().into()
}

fn write_member_object(
    root: &Path,
    member: &proof::PackageMember,
    budget: &mut StreamBudget<'_>,
) -> Result<[u8; 20], String> {
    budget.admit(member)?;
    let mut child = Command::new("git")
        .args(["hash-object", "-w", "-t", "blob", "--stdin"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot launch git hash-object: {error}"))?;
    let mut stdin = child.stdin.take().expect("piped Git stdin exists");
    let mut hash = Sha1::new();
    hash.update(format!("blob {}\0", member.length()).as_bytes());
    member
        .copy_to(&mut CopyHashWriter {
            writer: &mut stdin,
            hash: &mut hash,
        })
        .map_err(|error| error.to_string())?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .map_err(|error| format!("cannot wait for git hash-object: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git hash-object refused: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let observed = String::from_utf8(output.stdout)
        .map_err(|error| format!("Git object id is not UTF-8: {error}"))?;
    let expected: [u8; 20] = hash.finalize().into();
    if observed.trim() != hex(&expected) {
        return Err("Git blob object identity disagrees with preview".to_owned());
    }
    Ok(expected)
}

struct CopyHashWriter<'a> {
    writer: &'a mut dyn io::Write,
    hash: &'a mut Sha1,
}

impl io::Write for CopyHashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.writer.write(bytes)?;
        self.hash.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

fn write_object(root: &Path, kind: &str, bytes: &[u8]) -> Result<[u8; 20], String> {
    let mut child = Command::new("git")
        .args(["hash-object", "-w", "-t", kind, "--stdin"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot launch git hash-object: {error}"))?;
    child
        .stdin
        .take()
        .expect("piped Git stdin exists")
        .write_all(bytes)
        .map_err(|error| format!("cannot write Git {kind} object: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("cannot wait for git hash-object: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git hash-object refused: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let observed = String::from_utf8(output.stdout)
        .map_err(|error| format!("Git object id is not UTF-8: {error}"))?;
    let expected = hash(kind, bytes);
    if observed.trim() != hex(&expected) {
        return Err(format!("Git {kind} object identity disagrees with preview"));
    }
    Ok(expected)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}
