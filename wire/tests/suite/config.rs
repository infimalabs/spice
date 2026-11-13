use std::time::Duration;

use serde::{Deserialize, Serialize};
use wire::{ConfigSource, DurationWord, DurationWordError, Provenance};

fn duration(word: &str) -> DurationWord {
    word.parse().expect("canonical duration word")
}

#[test]
fn every_duration_unit_preserves_its_word_and_checked_value() {
    for (word, seconds) in [
        ("0s", 0),
        ("15s", 15),
        ("15m", 15 * 60),
        ("24h", 24 * 60 * 60),
        ("00s", 0),
    ] {
        let parsed = duration(word);
        assert_eq!(parsed.to_string(), word);
        assert_eq!(parsed.get(), Duration::from_secs(seconds));
    }
    assert_ne!(duration("60s"), duration("1m"));
}

#[test]
fn duration_boundaries_are_checked_in_seconds() {
    let maximum_seconds = format!("{}s", u64::MAX);
    assert_eq!(
        duration(&maximum_seconds).get(),
        Duration::from_secs(u64::MAX)
    );

    for (unit, multiplier) in [('m', 60_u64), ('h', 60 * 60)] {
        let maximum = u64::MAX / multiplier;
        let admitted = format!("{maximum}{unit}");
        assert_eq!(
            duration(&admitted).get(),
            Duration::from_secs(maximum * multiplier)
        );

        let overflow = format!("{}{unit}", maximum + 1);
        assert!(matches!(
            overflow.parse::<DurationWord>(),
            Err(DurationWordError::Overflow(word)) if word == overflow
        ));
    }

    let decimal_overflow = "18446744073709551616s";
    assert!(matches!(
        decimal_overflow.parse::<DurationWord>(),
        Err(DurationWordError::Overflow(word)) if word == decimal_overflow
    ));
}

#[test]
fn every_non_word_duration_class_refuses() {
    for malformed in [
        "", "s", "1", " 1s", "1s ", "+1s", "-1s", "1.0s", "1ms", "1d", "1S", "one", "١s",
    ] {
        assert!(
            matches!(
                malformed.parse::<DurationWord>(),
                Err(DurationWordError::Grammar(word)) if word == malformed
            ),
            "{malformed:?} crossed the duration grammar"
        );
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct EffectiveDuration {
    value: DurationWord,
    provenance: Provenance<DurationWord>,
}

#[test]
fn duration_and_typed_provenance_round_trip_through_json_and_toml() {
    let effective = EffectiveDuration {
        value: duration("90s"),
        provenance: Provenance::Flag {
            builtin: Some(duration("1m")),
        },
    };

    let json = wire::encode(&effective).expect("JSON encodes");
    assert_eq!(
        String::from_utf8(json.clone()).expect("JSON is UTF-8"),
        r#"{"value":"90s","provenance":{"source":"flag","builtin":"1m"}}"#
    );
    assert_eq!(
        wire::decode::<EffectiveDuration>(&json).expect("JSON decodes"),
        effective
    );

    let toml = toml::to_string(&effective).expect("TOML encodes");
    assert!(toml.contains("value = \"90s\""));
    assert!(toml.contains("source = \"flag\""));
    assert!(toml.contains("builtin = \"1m\""));
    assert_eq!(
        toml::from_str::<EffectiveDuration>(&toml).expect("TOML decodes"),
        effective
    );
}

#[test]
fn configuration_sources_are_closed_and_ordered_by_precedence() {
    assert_eq!(
        ConfigSource::ALL,
        &[
            ConfigSource::Builtin,
            ConfigSource::Tracked,
            ConfigSource::Local,
            ConfigSource::Worktree,
            ConfigSource::Flag,
        ]
    );
    for (source, spelling) in ConfigSource::ALL
        .iter()
        .copied()
        .zip(["builtin", "tracked", "local", "worktree", "flag"])
    {
        assert_eq!(source.as_str(), spelling);
        assert_eq!(source.to_string(), spelling);
        assert_eq!(
            wire::encode(&source).expect("source encodes"),
            format!("\"{spelling}\"").as_bytes()
        );
    }
    assert!(ConfigSource::ALL.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(wire::decode::<ConfigSource>(br#""remote""#).is_err());
}

#[test]
fn every_provenance_variant_names_its_winner_and_typed_builtin() {
    let builtin = duration("15m");
    let provenances = [
        Provenance::Builtin {},
        Provenance::Tracked {
            builtin: Some(builtin.clone()),
        },
        Provenance::Local { builtin: None },
        Provenance::Worktree {
            builtin: Some(builtin.clone()),
        },
        Provenance::Flag {
            builtin: Some(builtin),
        },
    ];

    for (provenance, source) in provenances.into_iter().zip(ConfigSource::ALL) {
        assert_eq!(provenance.source(), *source);
        let bytes = wire::encode(&provenance).expect("provenance encodes");
        assert_eq!(
            wire::decode::<Provenance<DurationWord>>(&bytes).expect("provenance decodes"),
            provenance
        );
    }
}

#[test]
fn provenance_refuses_unknown_variants_fields_and_null_builtin_values() {
    for malformed in [
        br#"{}"#.as_slice(),
        br#"{"source":"remote"}"#.as_slice(),
        br#"{"source":"builtin","extra":true}"#.as_slice(),
        br#"{"source":"tracked","builtin":"1s","extra":true}"#.as_slice(),
        br#"{"source":"local","builtin":null}"#.as_slice(),
    ] {
        assert!(
            wire::decode::<Provenance<DurationWord>>(malformed).is_err(),
            "malformed provenance decoded: {}",
            String::from_utf8_lossy(malformed)
        );
    }
}
