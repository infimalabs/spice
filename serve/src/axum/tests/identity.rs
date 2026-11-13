//! Shared product and app identity for server fixtures.

use super::*;

pub(super) fn app_version(source: &CanvasAssets) -> wire::Version {
    wire::Version {
        product: wire::manifest::ProductVersion::parse("1.2.3").expect("fixture product version"),
        binary: wire::Digest::compute(wire::Domain::DistArtifact, b"fixture native"),
        wire: wire::WireVersion::CURRENT,
        journal: journal::SCHEMA_VERSION,
        app_protocol: wire::AppProtocolVersion::CURRENT,
        app: source.generation().expect("fixture generation reads"),
    }
}

pub(super) async fn bind_test_server(
    epoch: wire::CanonicalUuid,
    app: &TestApp,
    capabilities: Capabilities,
) -> Result<(Server, Control), std::io::Error> {
    Server::bind(Generation::new(
        epoch,
        capabilities,
        app.version(),
        app.source(),
    ))
    .await
}

/// One complete lane-built app whose directory lives exactly as long as its test.
pub(super) struct TestApp {
    source: CanvasAssets,
    root: tempfile::TempDir,
}

impl TestApp {
    pub(super) fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("spice-test-app-")
            .tempdir()
            .expect("fixture temp directory");
        seed_app_generation(root.path(), "test app");
        let source = CanvasAssets::DevelopmentDirectory(
            crate::AppSource::resolve(root.path()).expect("fixture app resolves"),
        );
        Self { source, root }
    }

    pub(super) fn source(&self) -> CanvasAssets {
        self.source.clone()
    }

    pub(super) fn version(&self) -> wire::Version {
        app_version(&self.source)
    }

    pub(super) fn root(&self) -> &std::path::Path {
        self.root.path()
    }
}
