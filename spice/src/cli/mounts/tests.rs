use std::ffi::OsStr;

use crate::config::{Document, Inputs, resolve};

use super::*;

fn commands(document: &str) -> crate::config::Resolved {
    resolve(Inputs::new(
        Some(Document::new("fixture.toml", document)),
        None,
        &[],
    ))
    .expect("fixture commands resolve")
}

#[test]
fn discovery_is_sorted_and_generated_entirely_from_contracts() {
    let resolved = commands(
        r#"
        [commands]
        zebra = ["tool", "zebra"]
        "task.fixture" = ["tool", "task", "fixture"]
        alpha = ["tool", "alpha"]
        "#,
    );
    let registry = MountRegistry::discover(Path::new("/fixture"), resolved.root().commands())
        .expect("novel paths are admitted");

    assert_eq!(
        registry
            .mounts()
            .iter()
            .map(|mount| mount.path().join("."))
            .collect::<Vec<_>>(),
        ["alpha", "task.fixture", "zebra"]
    );

    for contract in contracts() {
        let name = contract.path.join(".");
        let document = format!("[commands]\n{name:?} = ['tool']\n");
        let resolved = commands(&document);
        let error = MountRegistry::discover(Path::new("/fixture"), resolved.root().commands())
            .expect_err("every registered action path must collide");
        assert_eq!(error.collisions(), [contract.path]);
    }
}

#[test]
fn every_generated_branch_collides_but_novel_descendants_are_admitted() {
    let generated = generated_paths();
    for path in generated {
        let name = path.join(".");
        let document = format!("[commands]\n{name:?} = ['tool']\n");
        let resolved = commands(&document);
        let error = MountRegistry::discover(Path::new("/fixture"), resolved.root().commands())
            .expect_err("every generated branch must collide");
        assert_eq!(error.collisions(), [path]);
    }

    let resolved =
        commands("[commands]\n\"task.done.fixture\" = ['tool']\n\"task.fixture\" = ['tool']\n");
    MountRegistry::discover(Path::new("/fixture"), resolved.root().commands())
        .expect("novel descendants beneath generated commands are admitted");
}

#[test]
fn generated_parser_selects_longest_mount_and_preserves_trailing_arguments() {
    let resolved = commands(
        r#"
        [commands]
        report = ["tool", "report"]
        "report.inspect" = ["tool", "report", "inspect"]
        "task.fixture" = ["tool", "task", "fixture"]
        "#,
    );
    let registry = MountRegistry::discover(Path::new("/fixture"), resolved.root().commands())
        .expect("mounts are admitted");
    let matches = registry
        .command()
        .try_get_matches_from(["spice", "report", "inspect", "--limit", "40", "--json", ""])
        .expect("mounted arguments parse verbatim");
    let selected = registry.selected(&matches).expect("nested mount selected");

    assert_eq!(selected.mount().path(), ["report", "inspect"]);
    assert_eq!(
        selected.arguments(),
        [
            OsStr::new("--limit"),
            OsStr::new("40"),
            OsStr::new("--json"),
            OsStr::new("")
        ]
    );

    registry
        .command()
        .try_get_matches_from(["spice", "task", "fixture", "--help"])
        .expect("a mounted --help belongs to the mounted child");
}
