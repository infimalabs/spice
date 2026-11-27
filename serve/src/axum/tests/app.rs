//! Immutable app response integration checks.

use super::*;

#[tokio::test(flavor = "current_thread")]
async fn etags_revalidate_without_retransmitting_the_asset() {
    let app = TestApp::new();
    let (server, _control) = Server::bind(generation(&app, Capabilities::default()))
        .await
        .expect("binds");
    let address = server.local_addr().expect("local address");
    let task = tokio::spawn(server.up());

    let initial = request_with_headers(
        address,
        &address.to_string(),
        "/",
        &[("If-None-Match", "\"not-this-asset\"")],
    )
    .await;
    assert!(initial.starts_with(b"HTTP/1.1 200 OK\r\n"));
    let etag = response_header(&initial, "etag")
        .expect("the app response carries an ETag")
        .to_owned();
    let content_type = response_header(&initial, "content-type")
        .expect("the app response carries its media type")
        .to_owned();
    let policy = response_header(&initial, "content-security-policy")
        .expect("the app shell carries its CSP")
        .to_owned();
    assert_eq!(response_header(&initial, "cache-control"), Some("no-cache"));

    let revalidated = request_with_headers(
        address,
        &address.to_string(),
        "/",
        &[("If-None-Match", &etag)],
    )
    .await;
    assert!(revalidated.starts_with(b"HTTP/1.1 304 Not Modified\r\n"));
    assert!(response_body(&revalidated).is_empty());
    assert_eq!(response_header(&revalidated, "etag"), Some(etag.as_str()));
    assert_eq!(
        response_header(&revalidated, "content-type"),
        Some(content_type.as_str())
    );
    assert_eq!(
        response_header(&revalidated, "content-security-policy"),
        Some(policy.as_str())
    );
    assert_eq!(
        response_header(&revalidated, "cache-control"),
        Some("no-cache")
    );

    task.abort();
    assert!(task.await.expect_err("aborted server task").is_cancelled());
}

#[tokio::test(flavor = "current_thread")]
async fn generation_qualified_assets_are_immutable_and_retransmit_when_revalidated() {
    let app = TestApp::new();
    let (server, _control) = Server::bind(generation(&app, Capabilities::default()))
        .await
        .expect("binds");
    let address = server.local_addr().expect("local address");
    let task = tokio::spawn(server.up());

    for file in ["style.css", "app_bg.wasm"] {
        let route = format!(
            "{}/{FIXTURE_GENERATION}/{file}",
            crate::APP_GENERATION_ROUTE
        );
        let initial = request_with_headers(address, &address.to_string(), &route, &[]).await;
        assert!(initial.starts_with(b"HTTP/1.1 200 OK\r\n"));
        assert_eq!(
            response_header(&initial, "cache-control"),
            Some("public, max-age=31536000, immutable")
        );
        assert_eq!(
            response_header(&initial, "content-type"),
            crate::content_type(file)
        );
        let etag = response_header(&initial, "etag")
            .expect("the generation-qualified asset carries an ETag")
            .to_owned();
        let body = response_body(&initial);

        let revalidated = request_with_headers(
            address,
            &address.to_string(),
            &route,
            &[("If-None-Match", &etag)],
        )
        .await;
        assert!(revalidated.starts_with(b"HTTP/1.1 200 OK\r\n"));
        assert_eq!(response_body(&revalidated), body);
        assert_eq!(response_header(&revalidated, "etag"), Some(etag.as_str()));
        assert_eq!(
            response_header(&revalidated, "cache-control"),
            Some("public, max-age=31536000, immutable")
        );
    }

    task.abort();
    assert!(task.await.expect_err("aborted server task").is_cancelled());
}

fn embedded_generation() -> wire::Digest {
    wire::Digest::compute(wire::Domain::DistArtifact, b"embedded generation")
}

/// The build record the fixture archive carries at its root.
const BUILD_RECORD: &[u8] = br#"{"product":"0.1.0","source":{"commit":"0123456789abcdef0123456789abcdef01234567","committed":1700000000}}"#;

/// A complete generation archived the way staging writes it, held for the
/// `'static` lifetime an executable's linked archive has.
static EMBEDDED_ARCHIVE: std::sync::LazyLock<Vec<u8>> =
    std::sync::LazyLock::new(|| embedded_archive_with(Some(BUILD_RECORD)));

/// The fixture generation's rack, carrying `build` as its root build record
/// when one is given.
fn embedded_archive_with(build: Option<&[u8]>) -> Vec<u8> {
    use wire::manifest::renderer::{self, File, Kind, Member};
    let renderer_document = b"<script type=\"importmap\">{}</script>".to_vec();
    let declaration = renderer::encode(&[Member {
        digest: wire::Digest::compute(wire::Domain::InstallArtifact, &renderer_document),
        file: File::new(Kind::Document, 0).expect("index is in bounds"),
    }])
    .expect("declaration encodes");
    let generation = embedded_generation();
    let mut files = APP
        .assets()
        .iter()
        .map(|asset| {
            let bytes = if asset.path() == "/" {
                format!(
                    "<main id=\"app\" data-app-generation=\"{generation}\" {}=\"{declaration}\"></main>{INLINE_THEME_OPEN}theme();{INLINE_MODULE_CLOSE}{INLINE_MODULE_OPEN}start();{INLINE_MODULE_CLOSE}",
                    renderer::ATTRIBUTE
                )
            } else {
                format!("embedded {}", asset.file()).repeat(64)
            };
            (
                format!("{}/{}", crate::canvas::APP_DIRECTORY, asset.file()),
                bytes.into_bytes(),
            )
        })
        .collect::<Vec<_>>();
    files.push((
        format!("{}/renderer-000.html", crate::canvas::APP_DIRECTORY),
        renderer_document,
    ));
    files.extend(build.map(|record| (crate::canvas::BUILD.to_owned(), record.to_vec())));
    crate::canvas::gzip::write(
        files
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice())),
    )
    .expect("fixture archive writes")
}

/// Racks whose build record is absent, has an invalid product or source
/// identity, or carries an undeclared field, each otherwise the complete
/// fixture generation, held `'static` like a linked archive.
static UNBUILT_ARCHIVES: std::sync::LazyLock<[Vec<u8>; 5]> = std::sync::LazyLock::new(|| {
    [
        embedded_archive_with(None),
        embedded_archive_with(Some(b"{\"product\":\"v1\"}")),
        embedded_archive_with(Some(
            b"{\"product\":\"0.1.0\",\"source\":{\"commit\":\"0123\",\"committed\":1700000000}}",
        )),
        embedded_archive_with(Some(
            b"{\"product\":\"0.1.0\",\"source\":{\"commit\":\"0123456789abcdef0123456789abcdef01234567\",\"committed\":-1}}",
        )),
        embedded_archive_with(Some(b"{\"product\":\"0.1.0\",\"extra\":1}")),
    ]
});

/// The fixture generation with another build record: same app, other build.
static REBUILT_ARCHIVE: std::sync::LazyLock<Vec<u8>> =
    std::sync::LazyLock::new(|| embedded_archive_with(Some(b"{\"product\":\"0.1.1\"}")));

/// A linked rack names the build that linked it; one without a parseable
/// build record is refused at that member, however complete its app is.
#[test]
fn an_embedded_archive_carries_its_build_record_or_is_refused() {
    let archive: &'static [u8] = &EMBEDDED_ARCHIVE;
    let build = crate::canvas::EmbeddedArchive::admit(archive)
        .expect("fixture archive admits")
        .build()
        .clone();
    assert_eq!(build.product.as_str(), "0.1.0");
    assert_eq!(
        build.source.map(|source| (source.commit, source.committed)),
        Some((
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            1_700_000_000
        ))
    );
    for (index, bytes) in UNBUILT_ARCHIVES.iter().enumerate() {
        assert!(rack::Rack::admit(bytes).is_ok(), "{index}");
        assert!(
            matches!(
                crate::canvas::EmbeddedArchive::admit(bytes),
                Err(rack::Error::Member { path, .. }) if path == crate::canvas::BUILD
            ),
            "{index}"
        );
    }
}

/// The fixture generation plus an app member whose extension the content-type
/// table does not name.
static UNTYPED_ARCHIVE: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
    let stored = rack::Rack::admit(&EMBEDDED_ARCHIVE).expect("fixture archive admits");
    let name = crate::canvas::gzip::stored_name("app/notes.xyz");
    let gzip = crate::canvas::gzip::compress(b"untyped");
    rack::write(
        stored
            .members()
            .iter()
            .map(|member| (member.path(), member.bytes()))
            .chain([(name.as_str(), gzip.as_slice())]),
    )
    .expect("fixture archive writes")
});

/// Every served member's content type comes from the one table, so an app
/// member whose extension the table does not name refuses indexing.
#[test]
fn an_embedded_archive_refuses_an_app_member_without_a_tabled_content_type() {
    assert!(matches!(
        crate::canvas::EmbeddedArchive::admit(&UNTYPED_ARCHIVE),
        Err(rack::Error::Member { path, .. }) if path == "app/notes.xyz"
    ));
}

/// The linked build record is one bounded HTTP read of the stored member, and
/// its ETag follows the record, not the app generation: a rebuilt record under
/// the same generation is never answered as unmodified.
#[tokio::test(flavor = "current_thread")]
async fn the_build_record_is_served_and_its_etag_follows_the_record() {
    let mut etags = Vec::new();
    for archive in [&*EMBEDDED_ARCHIVE, &*REBUILT_ARCHIVE] {
        let archive: &'static [u8] = archive;
        let stored = rack::Rack::admit(archive).expect("fixture archive admits");
        let app = CanvasAssets::EmbeddedArchive(
            crate::canvas::EmbeddedArchive::admit(archive).expect("fixture archive indexes"),
        );
        assert_eq!(app.generation().expect("generation"), embedded_generation());
        let (server, _control) = Server::bind(Generation::new(
            epoch(),
            Capabilities::default(),
            app_version(&app),
            app,
        ))
        .await
        .expect("binds");
        let address = server.local_addr().expect("local address");
        let host = address.to_string();
        let task = tokio::spawn(server.up());

        let response = request_with_headers(
            address,
            &host,
            crate::canvas::BUILD_ROUTE,
            &[("Accept-Encoding", "gzip")],
        )
        .await;
        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
        let gzip = crate::canvas::gzip::get(&stored, crate::canvas::BUILD)
            .expect("build record is one stream")
            .expect("build record is archived")
            .gzip();
        assert_eq!(response_body(&response), gzip);
        assert_eq!(
            response_header(&response, "content-type"),
            Some("application/json")
        );
        assert_eq!(response_header(&response, "content-encoding"), Some("gzip"));
        let etag = response_header(&response, "etag")
            .expect("the record carries an ETag")
            .to_owned();
        let unchanged = request_with_headers(
            address,
            &host,
            crate::canvas::BUILD_ROUTE,
            &[("Accept-Encoding", "gzip"), ("If-None-Match", &etag)],
        )
        .await;
        assert!(unchanged.starts_with(b"HTTP/1.1 304 Not Modified\r\n"));
        etags.push(etag);
        task.abort();
    }
    assert_ne!(etags[0], etags[1], "a rebuilt record is a new ETag");
}

#[tokio::test(flavor = "current_thread")]
async fn embedded_archive_serves_its_stored_gzip_slices_and_refuses_what_it_cannot() {
    let held = embedded_generation();
    let archive: &'static [u8] = &EMBEDDED_ARCHIVE;
    let stored = rack::Rack::admit(archive).expect("fixture archive admits");
    let app = CanvasAssets::EmbeddedArchive(
        crate::canvas::EmbeddedArchive::admit(archive).expect("fixture archive indexes"),
    );
    let (server, _control) = Server::bind(Generation::new(
        epoch(),
        Capabilities::default(),
        app_version(&app),
        app,
    ))
    .await
    .expect("binds");
    let address = server.local_addr().expect("local address");
    let host = address.to_string();
    let task = tokio::spawn(server.up());

    let wasm = format!("{}/{held}/app_bg.wasm", crate::APP_GENERATION_ROUTE);
    for (route, member, accept) in [
        ("/", wire::manifest::APP_HTML, Some("br, gzip;q=0.5")),
        (wasm.as_str(), "app_bg.wasm", None),
    ] {
        let headers = accept.map(|accept| ("Accept-Encoding", accept));
        let response = request_with_headers(address, &host, route, headers.as_slice()).await;
        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"), "{route}");
        let (index, (_, member)) =
            crate::canvas::gzip::beneath(&stored, crate::canvas::APP_DIRECTORY)
                .map(|entry| entry.expect("member is one stream"))
                .enumerate()
                .find(|(_, (path, _))| *path == member)
                .expect("member is archived");
        let gzip = member.gzip();
        assert_eq!(
            response_body(&response),
            gzip,
            "{route} is the stored slice"
        );
        assert_eq!(
            response_header(&response, "content-length"),
            Some(gzip.len().to_string().as_str())
        );
        assert_eq!(response_header(&response, "content-encoding"), Some("gzip"));
        assert_eq!(response_header(&response, "vary"), Some("accept-encoding"));
        assert_eq!(
            response_header(&response, "etag"),
            Some(format!("\"{held}/{index}\"").as_str())
        );
    }
    let shell = request(address, &host, "/").await;
    assert!(response_header(&shell, "content-security-policy").is_some());

    for refused in ["identity", "gzip;q=0, *", "br"] {
        let response =
            request_with_headers(address, &host, &wasm, &[("Accept-Encoding", refused)]).await;
        assert!(
            response.starts_with(b"HTTP/1.1 406 Not Acceptable\r\n"),
            "{refused}"
        );
    }

    let retired = wire::Digest::compute(wire::Domain::DistArtifact, b"retired generation");
    let gone = request(
        address,
        &host,
        &format!("{}/{retired}/app.module", crate::APP_GENERATION_ROUTE),
    )
    .await;
    assert!(gone.starts_with(b"HTTP/1.1 410 Gone\r\n"));
    assert_eq!(
        response_header(&gone, wire::hello::APP_GENERATION_HEADER),
        Some(held.as_str())
    );
    assert_eq!(response_header(&gone, "cache-control"), Some("no-store"));

    task.abort();
    assert!(task.await.expect_err("aborted server task").is_cancelled());
}

#[tokio::test(flavor = "current_thread")]
async fn development_directory_answers_gone_for_a_generation_it_no_longer_holds() {
    let app = TestApp::new();
    let (server, _control) = Server::bind(generation(&app, Capabilities::default()))
        .await
        .expect("binds");
    let address = server.local_addr().expect("local address");
    let task = tokio::spawn(server.up());

    let retired = wire::Digest::compute(wire::Domain::DistArtifact, b"pruned generation");
    let gone = request(
        address,
        &address.to_string(),
        &format!("{}/{retired}/app_bg.wasm", crate::APP_GENERATION_ROUTE),
    )
    .await;
    assert!(gone.starts_with(b"HTTP/1.1 410 Gone\r\n"));
    assert_eq!(
        response_header(&gone, wire::hello::APP_GENERATION_HEADER),
        Some(FIXTURE_GENERATION)
    );

    task.abort();
    assert!(task.await.expect_err("aborted server task").is_cancelled());
}

#[tokio::test(flavor = "current_thread")]
async fn the_right_to_left_shell_is_the_current_shell_directed_rtl() {
    let root = tempfile::Builder::new()
        .prefix("spice-test-app-")
        .tempdir()
        .expect("fixture temp directory");
    seed_app_generation(root.path(), "<!doctype html><html lang=\"en\">");
    let app = CanvasAssets::DevelopmentDirectory(
        crate::AppSource::resolve(root.path()).expect("fixture app resolves"),
    );
    let (server, _control) = Server::bind(Generation::new(
        epoch(),
        Capabilities::default(),
        app_version(&app),
        app,
    ))
    .await
    .expect("binds");
    let address = server.local_addr().expect("local address");
    let task = tokio::spawn(server.up());
    let host = address.to_string();
    let shell = request(address, &host, "/").await;
    let directed = request(address, &host, RTL_SHELL_ROUTE).await;
    assert!(directed.starts_with(b"HTTP/1.1 200 OK\r\n"));
    let shell_body = std::str::from_utf8(response_body(&shell)).expect("UTF-8 shell");
    assert!(shell_body.starts_with("<!doctype html><html lang=\"en\">"));
    assert_eq!(
        std::str::from_utf8(response_body(&directed)).expect("UTF-8 shell"),
        shell_body.replacen("<html", "<html dir=\"rtl\"", 1),
    );
    assert_eq!(
        response_header(&directed, "content-security-policy"),
        response_header(&shell, "content-security-policy"),
    );
    let directed_etag = response_header(&directed, "etag").expect("directed ETag");
    assert_ne!(Some(directed_etag), response_header(&shell, "etag"));
    let revalidated = request_with_headers(
        address,
        &host,
        RTL_SHELL_ROUTE,
        &[("If-None-Match", directed_etag)],
    )
    .await;
    assert!(revalidated.starts_with(b"HTTP/1.1 304 Not Modified\r\n"));

    task.abort();
    assert!(task.await.expect_err("aborted server task").is_cancelled());
}
