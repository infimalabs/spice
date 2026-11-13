use super::*;

impl Fixture {
    /// Create a linked worktree that shares this fixture's installed appliance.
    fn linked_repository(&self, name: &str) -> Repository {
        let admin = self.0.join(".git/worktrees").join(name);
        let worktree = self.0.join("linked").join(name);
        fs::create_dir_all(&admin).expect("linked worktree administration is created");
        fs::create_dir_all(&worktree).expect("linked worktree is created");
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .expect("linked worktree git pointer writes");
        fs::write(admin.join("commondir"), "../..\n").expect("linked common pointer writes");
        fs::write(admin.join("HEAD"), "ref: refs/heads/main\n")
            .expect("linked worktree HEAD writes");
        Repository::discover(worktree).expect("linked repository resolves")
    }
}

/// Publications from linked worktrees must not displace the live generation's
/// command authority with an intermediate installed image: the restarting
/// refresh stops the serving generation through that retained authority, starts
/// the installed successor, and retires the retained copy once it is ready.
#[test]
fn publications_retain_one_shared_resident_authority() {
    let fixture = Fixture::new("linked-worktree-resident-authority");
    let original = fixture.repository();
    let lane_b = fixture.linked_repository("lane-b");
    let lane_c = fixture.linked_repository("lane-c");
    let lane_z = fixture.linked_repository("lane-z");
    let discovery_path = original
        .admit_state()
        .expect("fixture state is admitted")
        .serve()
        .to_path_buf();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bootstrap listener binds");
    let address = listener.local_addr().expect("bootstrap address reads");
    let endpoint = format!("http://{address}");
    let predecessor = place(&original, b"original executable\n");
    let discovery = fixture.publish(&endpoint);
    let successor_b = fixture.binary("spice-lane-b", b"lane b executable\n");
    let successor_c = fixture.binary("spice-lane-c", b"lane c executable\n");
    let successor_z = fixture.binary("spice-lane-z", b"lane z executable\n");
    let server = serve_bootstraps(
        listener,
        discovery,
        vec![
            predecessor.clone(),
            predecessor.clone(),
            predecessor.clone(),
        ],
    );
    let mut host = Scripted::new({
        let endpoint = endpoint.clone();
        move |issued: &Issued| {
            if issued.is_down() {
                fs::remove_file(&discovery_path).expect("the stopped resident withdraws");
                stopped()
            } else {
                ready(&endpoint)
            }
        }
    });
    let no_resident = RefreshRequest {
        restart: false,
        ..RefreshRequest::default()
    };
    let mut output = Vec::new();

    install_built(
        &mut host,
        &lane_b,
        successor_b.clone(),
        &no_resident,
        &mut output,
    )
    .expect("lane b publishes without restarting");
    install_built(&mut host, &lane_c, successor_c, &no_resident, &mut output)
        .expect("lane c publishes without restarting");

    let common = workspace(&lane_z).expect("common refresh workspace exists");
    let retained = common.join(RESIDENT_ROLLBACK);
    assert_eq!(
        executable_digest(&retained).expect("resident authority hashes"),
        predecessor,
        "later publications displaced the still-serving executable",
    );
    assert_eq!(
        executable_digest(&common.join(ROLLBACK)).expect("transaction rollback hashes"),
        executable_digest(&successor_b).expect("lane b executable hashes"),
        "the immediate install rollback did not follow the installed generation",
    );

    let request = RefreshRequest {
        restart: true,
        ..RefreshRequest::default()
    };
    let report = install_built(&mut host, &lane_z, successor_z, &request, &mut output)
        .expect("lane z replaces the original resident");
    server.join().expect("bootstrap server joins");

    let installed = install::installed_binary(&lane_z);
    let port = address.port().to_string();
    assert_eq!(
        host.issued,
        [
            Issued::down(&retained),
            Issued::up(&installed, &["--host", "127.0.0.1", "--port", &port]),
        ],
        "the serving generation was not replaced through its retained authority"
    );
    assert_eq!(
        fs::read(&installed).expect("installed successor reads"),
        b"lane z executable\n"
    );
    assert_eq!(report.endpoint.as_deref(), Some(endpoint.as_str()));
    assert!(
        !retained.exists(),
        "retired resident authority survived successor readiness",
    );
}
