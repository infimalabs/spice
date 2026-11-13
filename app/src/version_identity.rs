//! The version panel's prose identity.
//!
//! Three numbers in [`wire::Version`] count different things and must never
//! read as one kind of counter. The crossing wire generation names the outer
//! envelope every request and frame carries; this build speaks exactly one and
//! admits no other, so it stays small and never reads as behind. The journal
//! migration schema counts the stored journal's migrations, and the browser
//! app protocol counts browser/resident contract changes. Only the journal
//! value is a schema.

/// Native binary and the resident's two server-side contract numbers.
fn resident_rows(version: &wire::Version) -> [String; 3] {
    [
        format!("Native binary: {}", version.binary),
        format!("Crossing wire generation: {} (current only)", version.wire),
        format!("Journal migration schema: {}", version.journal),
    ]
}

/// Every row naming the running resident and the app this page loaded.
pub(crate) fn identity_rows(version: &wire::Version, loaded_app: &wire::Digest) -> Vec<String> {
    let mut rows = vec![format!("Product: {}", version.product)];
    rows.extend(resident_rows(version));
    rows.extend([
        format!("Browser app protocol: {}", version.app_protocol),
        format!("Loaded app generation: {loaded_app}"),
    ]);
    if *loaded_app != version.app {
        rows.push(format!("Served app generation: {}", version.app));
    }
    rows
}

/// Every row of the update offer: the current resident, while known, and the
/// app generation that replaces the loaded one.
pub(crate) fn update_rows(
    version: Option<&wire::Version>,
    loaded: &wire::Digest,
    available: wire::AppProtocolVersion,
    served: &wire::Digest,
) -> Vec<String> {
    let product = version.map_or_else(
        || "loading".to_owned(),
        |version| version.product.to_string(),
    );
    let mut rows = vec![format!("Current product: {product}")];
    if let Some(version) = version {
        rows.extend(resident_rows(version));
    }
    rows.extend([
        format!("Loaded app generation: {loaded}"),
        format!("Available browser app protocol: {available}"),
        format!("Served app generation: {served}"),
    ]);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOADED: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const SERVED: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn digest(text: &str) -> wire::Digest {
        text.parse().expect("fixture digest")
    }

    fn version() -> wire::Version {
        serde_json::from_value(serde_json::json!({
            "product": "0.26299.762810",
            "binary": "b230f95dc059c24fa5812a81377965894cadeaff45b7335712cd5cba4cbaa5ca",
            "wire": 1,
            "journal": 99,
            "app_protocol": 18,
            "app": SERVED,
        }))
        .expect("fixture version identity")
    }

    /// The lowercase label of each row whose shown value starts with `value`.
    fn labels_of(rows: &[String], value: &str) -> Vec<String> {
        rows.iter()
            .filter_map(|row| row.split_once(": "))
            .filter(|(_, shown)| shown.split(' ').next() == Some(value))
            .map(|(label, _)| label.to_lowercase())
            .collect()
    }

    /// Wire is a crossing generation, journal the one schema, and the app
    /// value a browser protocol.
    fn assert_distinguished(rows: &[String]) {
        let [wire] = &labels_of(rows, "1")[..] else {
            panic!("one wire row: {rows:?}");
        };
        let [journal] = &labels_of(rows, "99")[..] else {
            panic!("one journal row: {rows:?}");
        };
        let [app] = &labels_of(rows, "18")[..] else {
            panic!("one app protocol row: {rows:?}");
        };
        assert!(
            wire.contains("wire") && wire.contains("generation") && !wire.contains("schema"),
            "{wire}"
        );
        assert!(
            journal.contains("journal") && journal.contains("schema"),
            "{journal}"
        );
        assert!(
            app.contains("browser app protocol") && !app.contains("schema"),
            "{app}"
        );
        let schemas = rows.iter().filter(|row| row.contains("schema")).count();
        assert_eq!(schemas, 1, "only the journal is a schema: {rows:?}");
    }

    #[test]
    fn the_identity_tells_the_crossing_generation_from_the_schema_and_protocol() {
        let version = version();
        assert_distinguished(&identity_rows(&version, &digest(LOADED)));
        assert_distinguished(&update_rows(
            Some(&version),
            &digest(LOADED),
            wire::AppProtocolVersion(18),
            &digest(SERVED),
        ));
    }
}
