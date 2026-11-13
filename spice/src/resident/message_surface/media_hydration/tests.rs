use super::*;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: std::path::PathBuf,
    tape: tape::Tape,
    scope: Scope,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "spice-captured-media-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let tape = tape::Tape::open(&root).expect("capture opens");
        let scope = Scope {
            lane: uuid("e5"),
            occupant: uuid("ef"),
            source: tape.source_id().unwrap().as_str().parse().unwrap(),
        };
        Self { root, tape, scope }
    }

    fn observe(&self, hydration: &mut MediaHydration, value: serde_json::Value) -> listen::Locus {
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        let locus = self.tape.tee(&bytes).unwrap();
        self.replay(hydration, locus);
        locus
    }

    fn replay(&self, hydration: &mut MediaHydration, locus: listen::Locus) {
        let bytes = self.tape.read(locus).unwrap();
        let decoded = listen::decode_record(None, &bytes, locus).unwrap().decoded;
        assert!(decoded.residue().is_none());
        hydration.enter(self.scope);
        for (ordinal, event) in decoded.events().iter().enumerate() {
            if let listen::Event::Tool(tool) = event {
                hydration.observe_tool(self.scope, tool, ordinal as u32);
            }
        }
    }

    fn call(
        &self,
        hydration: &mut MediaHydration,
        id: &str,
        name: &str,
        path: &str,
    ) -> listen::Locus {
        self.observe(
            hydration,
            serde_json::json!({
                "type": "message_end",
                "message": {"role": "assistant", "content": [{
                    "type": "toolCall", "id": id, "name": name,
                    "arguments": {"path": path},
                }]},
            }),
        )
    }

    fn result(
        &self,
        hydration: &mut MediaHydration,
        id: &str,
        media: &[wire::Media],
        failed: bool,
    ) -> listen::Locus {
        let content = media
            .iter()
            .map(|media| {
                serde_json::json!({
                    "type": "image", "name": media.name, "data_url": media.data_url,
                })
            })
            .collect::<Vec<_>>();
        self.observe(
            hydration,
            serde_json::json!({
                "type": "message_end",
                "message": {"role": "toolResult", "toolCallId": id,
                    "content": content, "isError": failed},
            }),
        )
    }

    fn correlate(
        &self,
        hydration: &mut MediaHydration,
        id: &str,
        media: &wire::Media,
    ) -> (listen::Locus, listen::Locus) {
        let call = self.call(hydration, id, "read", "/tmp/capture.png");
        let result = self.result(hydration, id, std::slice::from_ref(media), false);
        (call, result)
    }

    fn bindings(
        &self,
        hydration: &MediaHydration,
        markdown: &str,
    ) -> Vec<wire::MarkdownImageBinding> {
        hydration.bindings(self.scope, markdown, &self.root)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn uuid(suffix: &str) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-bce9131163{suffix}")
        .parse()
        .unwrap()
}

fn media(suffix: u8) -> wire::Media {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.push(suffix);
    wire::Media::from_bytes("capture.png", "image/png", &bytes).unwrap()
}

#[test]
fn one_source_bound_read_hydrates_only_an_exact_image_destination() {
    let fixture = Fixture::new();
    let media = media(1);
    let mut hydration = MediaHydration::default();
    fixture.correlate(&mut hydration, "image", &media);
    assert_eq!(
        fixture.bindings(
            &hydration,
            "![authored alt](/tmp/capture.png \"Authored title\")"
        ),
        [wire::MarkdownImageBinding {
            destination: "/tmp/capture.png".to_owned(),
            media,
        }]
    );
    assert!(
        fixture
            .bindings(&hydration, "[ordinary link](/tmp/capture.png)")
            .is_empty()
    );
}

#[test]
fn only_successful_single_image_absolute_reads_correlate() {
    let fixture = Fixture::new();
    let mut hydration = MediaHydration::default();
    for (id, name, path, failed, images) in [
        (
            "web",
            "read",
            "https://example.com/image.png",
            false,
            vec![media(1)],
        ),
        ("relative", "read", "image.png", false, vec![media(1)]),
        ("failed", "read", "/tmp/failed.png", true, vec![media(1)]),
        (
            "multiple",
            "read",
            "/tmp/multiple.png",
            false,
            vec![media(1), media(2)],
        ),
        (
            "other-tool",
            "write",
            "/tmp/written.png",
            false,
            vec![media(1)],
        ),
    ] {
        fixture.call(&mut hydration, id, name, path);
        fixture.result(&mut hydration, id, &images, failed);
        assert!(
            fixture
                .bindings(&hydration, &format!("![]({path})"))
                .is_empty()
        );
    }
}

#[test]
fn scopes_isolate_paths_and_conflicting_content_stays_ambiguous() {
    let fixture = Fixture::new();
    let mut hydration = MediaHydration::default();
    fixture.correlate(&mut hydration, "first", &media(1));
    let mut other = fixture.scope;
    other.lane = uuid("e6");
    assert!(
        hydration
            .bindings(other, "![](/tmp/capture.png)", &fixture.root)
            .is_empty()
    );
    fixture.correlate(&mut hydration, "same", &media(1));
    assert_eq!(
        fixture.bindings(&hydration, "![](/tmp/capture.png)")[0].media,
        media(1)
    );
    fixture.correlate(&mut hydration, "different", &media(2));
    assert!(
        fixture
            .bindings(&hydration, "![](/tmp/capture.png)")
            .is_empty()
    );
    other = fixture.scope;
    other.occupant = uuid("ee");
    hydration.enter(other);
    assert!(
        fixture
            .bindings(&hydration, "![](/tmp/capture.png)")
            .is_empty()
    );
}

#[test]
fn image_binding_survives_sealing_and_reconstruction_without_provider_storage() {
    let fixture = Fixture::new();
    let image = media(7);
    let mut hydration = MediaHydration::default();
    let (call, result) = fixture.correlate(&mut hydration, "image", &image);
    fixture.tape.seal().unwrap();
    let expected = vec![wire::MarkdownImageBinding {
        destination: "/tmp/capture.png".into(),
        media: image,
    }];
    assert_eq!(
        fixture.bindings(&hydration, "![](/tmp/capture.png)"),
        expected
    );
    let mut reconstructed = MediaHydration::default();
    fixture.replay(&mut reconstructed, call);
    fixture.replay(&mut reconstructed, result);
    assert_eq!(
        fixture.bindings(&reconstructed, "![](/tmp/capture.png)"),
        expected
    );
}

#[test]
fn replacement_capture_cannot_satisfy_an_old_image_binding() {
    let fixture = Fixture::new();
    let replacement = Fixture::new();
    let mut hydration = MediaHydration::default();
    fixture.correlate(&mut hydration, "image", &media(1));
    replacement.correlate(&mut MediaHydration::default(), "image", &media(1));
    assert!(
        hydration
            .bindings(fixture.scope, "![](/tmp/capture.png)", &replacement.root)
            .is_empty()
    );
    fs::remove_dir_all(&fixture.root).unwrap();
    fs::create_dir(&fixture.root).unwrap();
    assert!(
        fixture
            .bindings(&hydration, "![](/tmp/capture.png)")
            .is_empty()
    );
}

#[test]
fn altered_capture_content_cannot_replace_an_admitted_image() {
    use std::io::{Seek, SeekFrom, Write};

    let fixture = Fixture::new();
    let mut hydration = MediaHydration::default();
    let (_, locus) = fixture.correlate(&mut hydration, "image", &media(1));
    let original = fixture.tape.read(locus).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
    value["message"]["content"][0]["data_url"] = media(2).data_url.into();
    let mut altered = serde_json::to_vec(&value).unwrap();
    altered.push(b'\n');
    assert_eq!(altered.len(), original.len());
    let member = fs::read_dir(&fixture.root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            fs::read(path)
                .is_ok_and(|bytes| bytes.get(locus.offset as usize..) == Some(original.as_slice()))
        })
        .expect("the captured range belongs to one active member");
    let mut file = fs::OpenOptions::new().write(true).open(member).unwrap();
    file.seek(SeekFrom::Start(locus.offset)).unwrap();
    file.write_all(&altered).unwrap();
    assert!(
        fixture
            .bindings(&hydration, "![](/tmp/capture.png)")
            .is_empty()
    );
}
