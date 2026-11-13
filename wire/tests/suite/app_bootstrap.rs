use wire::{AppBootstrap, AppProtocolVersion, CanonicalUuid};

const APP_BOOTSTRAP: &str = r#"{
  "endpoint": "http://127.0.0.1:48123",
  "capability": "018f82d4-6f59-7d06-92e3-bce9131163c6",
  "epoch": "018f82d4-6f59-7d06-92e3-bce9131163c3",
  "version": {
    "product": "1.2.3",
    "binary": "1111111111111111111111111111111111111111111111111111111111111111",
    "wire": 1,
    "journal": 20,
    "app_protocol": 10,
    "app": "2222222222222222222222222222222222222222222222222222222222222222"
  }
}"#;

fn uuid(value: &str) -> CanonicalUuid {
    value.parse().expect("fixture UUID is canonical")
}

fn round_trip(document: &str) -> AppBootstrap {
    let decoded: AppBootstrap = wire::decode(document.as_bytes()).expect("bootstrap decodes");
    let reencoded: serde_json::Value =
        serde_json::from_slice(&wire::encode(&decoded).expect("bootstrap encodes"))
            .expect("re-encoded bootstrap is JSON");
    let original: serde_json::Value =
        serde_json::from_str(document).expect("bootstrap fixture is JSON");
    assert_eq!(reencoded, original, "round trip changed the bootstrap");
    decoded
}

#[test]
fn app_bootstrap_has_one_strict_body_shape() {
    let bootstrap = round_trip(APP_BOOTSTRAP);

    assert_eq!(bootstrap.endpoint, "http://127.0.0.1:48123");
    assert_eq!(
        bootstrap.capability,
        uuid("018f82d4-6f59-7d06-92e3-bce9131163c6")
    );
    assert_eq!(
        bootstrap.epoch,
        uuid("018f82d4-6f59-7d06-92e3-bce9131163c3")
    );
    assert_eq!(bootstrap.version.product.as_str(), "1.2.3");
    assert_eq!(bootstrap.version.app.as_str(), "2".repeat(64));

    for field in ["endpoint", "capability", "epoch", "version"] {
        let mut value: serde_json::Value =
            serde_json::from_str(APP_BOOTSTRAP).expect("bootstrap fixture is JSON");
        value
            .as_object_mut()
            .expect("bootstrap is an object")
            .remove(field);
        assert!(
            wire::decode::<AppBootstrap>(&serde_json::to_vec(&value).expect("shape encodes"))
                .is_err(),
            "bootstrap without {field} was admitted"
        );
    }

    for malformed in [
        r#"{"backend":"unknown"}"#,
        r#"{"backend":"disabled","voices":["Zoe"]}"#,
        r#"{"backend":"system","words_per_minute":175}"#,
        r#"{"backend":"system","voice":"Zoe","words_per_minute":175}"#,
        r#"{"backend":"external","content_type":"audio/wav","words_per_minute":175}"#,
    ] {
        assert!(
            wire::decode::<wire::SpeechPlayback>(malformed.as_bytes()).is_err(),
            "malformed speech playback was admitted: {malformed}"
        );
    }
}

#[test]
fn app_protocol_version_has_one_http_header_spelling() {
    assert_eq!(
        wire::hello::APP_PROTOCOL_VERSION_HEADER,
        "x-spice-app-protocol-version"
    );
    assert_eq!(wire::hello::APP_GENERATION_HEADER, "x-spice-app-generation");
    assert_eq!(
        AppProtocolVersion::CURRENT.to_string().parse(),
        Ok(AppProtocolVersion::CURRENT)
    );
    for malformed in ["", "current", "-1", "1.0", "+1", "01", " 1", "1 "] {
        assert!(
            malformed.parse::<AppProtocolVersion>().is_err(),
            "malformed app protocol version was admitted: {malformed:?}"
        );
    }
}
