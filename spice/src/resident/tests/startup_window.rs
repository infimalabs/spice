//! What a client finds between the resident lock and the surface clear.

use super::*;

/// A client standing in the startup window refuses the record it finds there.
///
/// The window is two statements wide. `up` takes the resident lock, and only
/// then removes the discovery records its predecessor left behind, so between
/// those two acts the kernel reports a live holder over an endpoint that
/// belongs to a resident which is already gone. This drives that interval by
/// standing in it: the two production calls run here in order, with a client
/// in between. Nothing is simulated and nothing races — the window is entered
/// by executing its first statement and pausing before its second.
///
/// The predecessor here was killed rather than shut down, which is the only
/// way its record outlives it. That is also what leaves the lock free for the
/// acquisition below to succeed.
///
/// Three arms run against one fixture and one set of published bytes, and the
/// only thing that changes between them is what the lock says about its
/// holder. The middle arm is what makes the first mean something: without it,
/// a client that refused every record would pass. The last arm is what makes
/// the first mean something *else* — a refusal that named the predecessor's
/// death would be reporting a resident that is alive and starting as a corpse,
/// so the two states have to answer differently, from the same record.
#[test]
fn a_client_inside_the_startup_window_refuses_the_predecessors_record() {
    let root = fixture("startup-window");
    fs::create_dir_all(root.join(".git")).expect("git directory");
    let paths = RuntimePaths::discover(&root).expect("runtime paths resolve");
    fs::create_dir_all(paths.serve.parent().expect("serve record has a parent"))
        .expect("runtime directory");
    let stale = serve::Discovery {
        endpoint: "http://127.0.0.1:48250".to_owned(),
        capability: test_uuid(1),
        epoch: test_uuid(2),
    };
    stale
        .publish(&paths.serve)
        .expect("the killed resident published this and never removed it");

    let resident_lock = ResidentLock::acquire(&paths).expect("a killed holder leaves no lock");

    let mut launched = Vec::new();
    let inside = open_app(&paths.serve, &mut launched);
    let Outcome::Fault(fault) = inside else {
        panic!("a record inside the startup window must fault: {inside:?}");
    };
    assert_eq!(fault.reason, "serve::publisher-replaced");
    assert!(
        launched.is_empty(),
        "a record no live resident published must not reach the launcher: {launched:?}"
    );
    assert!(!fault.detail.contains(&stale.capability.to_string()));

    // The holder now claims the very record under test, and nothing else about
    // the fixture moves: same bytes, same file, same lock, same process. The
    // generation the holder names is the whole variable.
    resident_lock
        .set_epoch(stale.epoch)
        .expect("the holder names the generation it published");
    let claimed = open_app(&paths.serve, &mut launched);
    assert!(
        matches!(claimed, Outcome::Reply(_)),
        "a record its holder claims must open: {claimed:?}"
    );
    assert_eq!(launched, [stale.endpoint.as_str()]);

    // No holder at all is the older fault, and it keeps its own name.
    drop(resident_lock);
    let orphaned = open_app(&paths.serve, &mut launched);
    let Outcome::Fault(orphaned) = orphaned else {
        panic!("a record whose publisher is gone must fault: {orphaned:?}");
    };
    assert_eq!(orphaned.reason, "serve::publisher-gone");
    assert_eq!(
        launched,
        [stale.endpoint.as_str()],
        "the endpoint must not reach the launcher a second time"
    );

    // The window's far edge: the statement `up` runs next takes the record out
    // of reach entirely, so the interval under test is bounded on both sides.
    paths
        .clear_stale_surfaces()
        .expect("stale surfaces clear the way up clears them");
    assert!(!paths.serve.exists());
    fs::remove_dir_all(&root).expect("startup window fixture cleanup");
}

/// Admit the published record through the client every command shares.
///
/// The launcher records rather than dials, so an admission is observable
/// without a socket and a refusal is observably not one.
fn open_app(path: &Path, launched: &mut Vec<String>) -> Outcome<wire::AppOpen> {
    crate::command_client::open_app_with(
        path,
        || {},
        |endpoint| {
            launched.push(endpoint.to_owned());
            Ok(())
        },
    )
}
