//! Each principal authenticates through its own capability.

use super::*;
use wire::principal::{PrincipalHead, PrincipalProfile, PrincipalRole, PrincipalRoles};

/// A second principal admitted after the owner, also holding the operator role.
fn second() -> wire::CanonicalUuid {
    uuid("018f82d4-6f59-7d06-92e3-bce9131163b1")
}

/// The capability the second principal's admission handed off once.
fn issued() -> wire::CanonicalUuid {
    uuid("018f82d4-6f59-7d06-92e3-bce9131163d9")
}

/// The owner and the second principal, both holding the operator role; the
/// second holds the capability `issued` names when it is present.
fn two_operators(issued: Option<wire::Digest>) -> PrincipalRoles {
    let wire::Actor::Principal(owner) = owner() else {
        unreachable!("the owner is a principal");
    };
    let head = |principal, born, capability| PrincipalHead {
        principal,
        born: seq(born),
        profile: PrincipalProfile::default(),
        roles: [PrincipalRole::Operator].into(),
        capability,
    };
    PrincipalRoles::new([head(owner, 1, None), head(second(), 2, issued)])
}

/// The owner's minted capability and the second principal's issued one, each
/// bound to its own operator-role principal.
fn bound() -> Capabilities {
    let capabilities = operator_capabilities();
    capabilities
        .publish(two_operators(Some(wire::principal::capability_digest(
            issued(),
        ))))
        .expect("the principal heads publish");
    capabilities
}

fn hello(capability: wire::CanonicalUuid) -> wire::Hello {
    wire::Hello {
        wire: wire::WireVersion::CURRENT,
        capability,
        epoch: epoch(),
    }
}

#[test]
fn each_principal_authenticates_as_itself_until_its_capability_is_deauthorized() {
    let capabilities = bound();
    let release = "1.2.3".parse().expect("fixture release");
    let digest = wire::principal::capability_digest(issued());

    let owned = capabilities
        .admit(&hello(operator()), epoch(), &release)
        .expect("the owner authenticates");
    assert_eq!(owned.actor, owner());
    let session = capabilities
        .admit_session(&hello(issued()), epoch(), &release)
        .expect("the issued capability authenticates");
    assert_eq!(session.accepted.actor, wire::Actor::Principal(second()));

    capabilities
        .publish(two_operators(Some(digest)))
        .expect("unchanged heads republish");
    assert!(!session.retired.is_cancelled());
    capabilities
        .publish(two_operators(None))
        .expect("the deauthorized heads publish");
    assert!(session.retired.is_cancelled());
    assert!(matches!(
        &capabilities.admit(&hello(issued()), epoch(), &release),
        Err(OpeningError::Refused(refusal))
            if refusal.reason() == "hello::capability-unknown"
    ));
    assert_eq!(
        capabilities
            .admit(&hello(operator()), epoch(), &release)
            .expect("the owner still authenticates")
            .actor,
        owner()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn discovery_and_bootstrap_serve_each_operator_principal_its_own_capability() {
    let app = TestApp::new();
    let capabilities = bound();
    let lane_capability = identity(0xa2);
    capabilities
        .insert(lane_capability, wire::Actor::Lane(identity(0xa3)))
        .expect("lane capability binds");
    let (server, _control) = Server::bind(generation(&app, capabilities.clone()))
        .await
        .expect("binds");

    for capability in [operator(), issued()] {
        assert_eq!(
            server
                .discovery(capability)
                .expect("an operator-role principal discovers")
                .capability,
            capability
        );
    }
    for capability in [lane_capability, identity(0xa4)] {
        assert_eq!(
            server.discovery(capability).err(),
            Some(CapabilityError::NotPrincipal)
        );
    }

    let address = server.local_addr().expect("local address");
    let host = address.to_string();
    let task = tokio::spawn(server.up());
    for (path, served) in [
        ("/bootstrap".to_owned(), operator()),
        (format!("/bootstrap?auth={}", issued()), issued()),
    ] {
        let response = request(address, &host, &path).await;
        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"), "{path}");
        let bootstrap: wire::AppBootstrap =
            wire::decode(response_body(&response)).expect("bootstrap decodes");
        assert_eq!(bootstrap.capability, served, "{path}");
    }

    capabilities
        .publish(two_operators(None))
        .expect("the second principal is deauthorized");
    for path in [
        format!("/bootstrap?auth={}", issued()),
        format!("/bootstrap?auth={}", identity(0xa4)),
    ] {
        let response = request(address, &host, &path).await;
        assert!(
            response.starts_with(b"HTTP/1.1 401 Unauthorized\r\n"),
            "{path}"
        );
        assert!(
            !String::from_utf8_lossy(&response).contains(&operator().to_string()),
            "an invalid capability must not receive the owner's capability"
        );
    }
    let response = request(address, &host, "/bootstrap").await;
    assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
    let bootstrap: wire::AppBootstrap =
        wire::decode(response_body(&response)).expect("local bootstrap decodes");
    assert_eq!(bootstrap.capability, operator());

    task.abort();
    assert!(task.await.expect_err("aborted server task").is_cancelled());
}
