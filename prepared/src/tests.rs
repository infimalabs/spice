use super::*;

mod contract {
    use super::*;

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn manifest() -> PreparedManifest {
        let inputs = BTreeMap::from([
            ("cargo_lock".to_owned(), digest('1')),
            ("source".to_owned(), digest('2')),
        ]);
        let build_key_digest = sha256_bytes(&serde_json::to_vec(&inputs).unwrap());
        PreparedManifest {
            schema: SCHEMA,
            kind: ArtifactKind::Bundle,
            platform: Platform::MacosArm64,
            build_key: BuildKey {
                digest: build_key_digest,
                inputs,
            },
            payload: Payload {
                path: "candidate.tar.gz".to_owned(),
                media_type: "application/gzip".to_owned(),
                bytes: 3,
                digest: digest('3'),
            },
            runtime: None,
            files: vec![FileEntry {
                path: "bin/spice".to_owned(),
                bytes: 4,
                digest: digest('4'),
            }],
        }
    }

    #[test]
    fn common_manifest_admits_bundle_and_image_kinds() {
        let bundle = manifest();
        bundle.admit().unwrap();
        let mut image = bundle.clone();
        image.kind = ArtifactKind::Image;
        image.platform = Platform::LinuxAmd64;
        image.runtime = Some(digest('5'));
        image.admit().unwrap();
    }

    #[test]
    fn build_key_covers_complete_named_input_set() {
        let mut changed = manifest();
        changed
            .build_key
            .inputs
            .insert("vendor".to_owned(), digest('5'));
        assert!(changed.admit().is_err());
    }

    #[test]
    fn inventory_order_and_paths_are_canonical() {
        let mut changed = manifest();
        changed.files.push(FileEntry {
            path: "../escape".to_owned(),
            bytes: 1,
            digest: digest('5'),
        });
        assert!(changed.admit().is_err());
    }

    #[test]
    fn discovery_accepts_only_build_key_tags() {
        assert!(admit_tag(&"a".repeat(64)).is_ok());
        assert!(admit_tag("latest").is_err());
        assert!(
            admit_tag_reference(&format!(
                "ghcr.io/infimalabs/spice/prepared:{}",
                "a".repeat(64)
            ))
            .is_ok()
        );
        assert!(admit_tag_reference("ghcr.io/infimalabs/spice/prepared:latest").is_err());
    }

    #[test]
    fn top_level_descriptor_allows_registry_normalization_but_rejects_conflicts() {
        let descriptor = serde_json::json!({
            "mediaType": OCI_MANIFEST_MEDIA_TYPE,
            "artifactType": ARTIFACT_MEDIA_TYPE,
            "digest": digest('a'),
            "size": 17,
            "annotations": {},
        });
        let bytes = serde_json::to_vec(&descriptor).unwrap();
        assert_eq!(
            admit_manifest_descriptor(&bytes).unwrap().digest,
            digest('a')
        );

        let mut normalized = descriptor.clone();
        normalized.as_object_mut().unwrap().remove("artifactType");
        assert!(admit_manifest_descriptor(&serde_json::to_vec(&normalized).unwrap()).is_ok());

        let mut missing_media_type = descriptor.clone();
        missing_media_type
            .as_object_mut()
            .unwrap()
            .remove("mediaType");
        assert!(
            admit_manifest_descriptor(&serde_json::to_vec(&missing_media_type).unwrap()).is_err()
        );
        let mut wrong_type = descriptor.clone();
        wrong_type["artifactType"] = serde_json::json!("application/vnd.example.wrong");
        assert!(admit_manifest_descriptor(&serde_json::to_vec(&wrong_type).unwrap()).is_err());
        let mut changed = descriptor;
        changed["unknown"] = serde_json::json!("field");
        assert!(admit_manifest_descriptor(&serde_json::to_vec(&changed).unwrap()).is_err());
    }

    #[test]
    fn fetched_manifest_requires_the_provider_spelled_artifact_type() {
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST_MEDIA_TYPE,
            "artifactType": ARTIFACT_MEDIA_TYPE,
            "config": {
                "mediaType": CONFIG_MEDIA_TYPE,
                "digest": digest('a'),
                "size": 17,
            },
            "layers": [{
                "mediaType": "application/vnd.infimalabs.spice.payload.v1",
                "digest": digest('b'),
                "size": 29,
            }],
            "annotations": {},
        });
        assert!(admit_oci_manifest(&serde_json::to_vec(&manifest).unwrap()).is_ok());

        let mut missing = manifest.clone();
        missing.as_object_mut().unwrap().remove("artifactType");
        assert!(admit_oci_manifest(&serde_json::to_vec(&missing).unwrap()).is_err());
        let mut conflicting = manifest.clone();
        conflicting["artifactType"] = serde_json::json!("application/vnd.example.wrong");
        assert!(admit_oci_manifest(&serde_json::to_vec(&conflicting).unwrap()).is_err());
        let mut unknown = manifest;
        unknown["unknown"] = serde_json::json!("field");
        assert!(admit_oci_manifest(&serde_json::to_vec(&unknown).unwrap()).is_err());
    }

    #[test]
    fn optional_descriptor_suppresses_only_canonical_absence() {
        let absent = optional_descriptor(Err(PreparedArtifactError::OrasMissing(
            "oras manifest fetch --descriptor missing".to_owned(),
        )))
        .unwrap();
        assert!(absent.is_none());

        let refused = optional_descriptor(Err(PreparedArtifactError::Invalid(
            "malformed descriptor".to_owned(),
        )));
        assert!(matches!(
            refused,
            Err(PreparedArtifactError::Invalid(message)) if message == "malformed descriptor"
        ));
    }

    #[test]
    fn prepared_bundle_bytes_are_reproducible_and_unpack_to_the_inventory() {
        let scratch = Scratch::new().unwrap();
        let root = scratch.path.join("root");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("candidate"), b"exact bytes").unwrap();
        fs::write(root.join("empty"), b"").unwrap();
        let first = scratch.path.join("first.tar.gz");
        let second = scratch.path.join("second.tar.gz");

        pack_bundle(&root, &first, ArchiveTimestamps::Normalized).unwrap();
        pack_bundle(&root, &second, ArchiveTimestamps::Normalized).unwrap();
        assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());

        let unpacked = scratch.path.join("unpacked");
        fs::create_dir(&unpacked).unwrap();
        let archive = fs::File::open(first).unwrap();
        Archive::new(GzDecoder::new(archive))
            .unpack(&unpacked)
            .unwrap();
        assert_eq!(inventory(&unpacked).unwrap(), inventory(&root).unwrap());
    }

    #[test]
    fn cache_bundle_preserves_fingerprint_ordering() {
        let scratch = Scratch::new().unwrap();
        let root = scratch.path.join("root");
        fs::create_dir(&root).unwrap();
        let dependency = root.join("dependency");
        let dependent = root.join("dependent");
        fs::write(&dependency, b"dependency").unwrap();
        fs::write(&dependent, b"dependent").unwrap();
        let dependency_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        let dependent_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(2_000_000);
        fs::OpenOptions::new()
            .write(true)
            .open(&dependency)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(dependency_time))
            .unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&dependent)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(dependent_time))
            .unwrap();
        let bundle = scratch.path.join("cache.tar.gz");

        pack_bundle(&root, &bundle, ArchiveTimestamps::Preserved).unwrap();
        let unpacked = scratch.path.join("unpacked");
        fs::create_dir(&unpacked).unwrap();
        Archive::new(GzDecoder::new(fs::File::open(bundle).unwrap()))
            .unpack(&unpacked)
            .unwrap();

        assert_eq!(
            fs::metadata(unpacked.join("dependency"))
                .unwrap()
                .modified()
                .unwrap(),
            dependency_time
        );
        assert_eq!(
            fs::metadata(unpacked.join("dependent"))
                .unwrap()
                .modified()
                .unwrap(),
            dependent_time
        );
    }

    #[cfg(feature = "windows-proof")]
    #[test]
    fn a_release_bundle_admits_only_its_source_canvas_and_payload() {
        let scratch = Scratch::new().unwrap();
        let payload = scratch.path.join("windows-cross.tar.gz");
        fs::write(&payload, b"packed bundle").unwrap();
        let source: wire::Digest = "2".repeat(64).parse().unwrap();
        let canvas = "6".repeat(64);
        let mut prepared = manifest();
        prepared.platform = Platform::WindowsAmd64;
        prepared
            .build_key
            .inputs
            .insert("canvas".to_owned(), format!("sha256:{canvas}"));
        prepared.build_key.digest =
            sha256_bytes(&serde_json::to_vec(&prepared.build_key.inputs).unwrap());
        prepared.payload.path = "windows-cross.tar.gz".to_owned();
        prepared.payload.bytes = 13;
        prepared.payload.digest = sha256_file(&payload).unwrap();
        let path = scratch.path.join("prepared.json");
        fs::write(&path, serde_json::to_vec(&prepared).unwrap()).unwrap();

        let admitted = windows_release_bundle(&path, &payload, &source, &canvas).unwrap();
        assert_eq!(admitted.build_key, prepared.build_key.digest);
        assert_eq!(admitted.files["bin/spice"], "4".repeat(64));

        let other: wire::Digest = "7".repeat(64).parse().unwrap();
        assert!(windows_release_bundle(&path, &payload, &other, &canvas).is_err());
        assert!(windows_release_bundle(&path, &payload, &source, &"8".repeat(64)).is_err());
        fs::write(&payload, b"packed bundlf").unwrap();
        assert!(windows_release_bundle(&path, &payload, &source, &canvas).is_err());
    }
}
