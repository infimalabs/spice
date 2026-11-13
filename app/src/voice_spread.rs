//! Spread desktop agents across the device's voices when no voice list is
//! configured.
//!
//! Apple mobile browsers instead use the device default: iOS and iPadOS do not
//! publish the macOS tier metadata this pool reads. The assignment reads only
//! the roster and the device's voice list, so every desktop page with the
//! same roster and voices hears the same agent in the same voice. The pool
//! prefers voices carrying macOS premium evidence in the chosen language;
//! without one it takes the enhanced voices of that language, then any
//! speaking voice the device serves in it (never a macOS novelty voice), and
//! only when the language has no such voice does the device default speak.
//! Every step down from premium carries
//! a notice naming the gap, shown once per page. Teams and their members are
//! visited in identity order, so reordering a team or the roster changes
//! nothing. Teammates are what must sound different: within one team no voice
//! repeats until the team outnumbers the pool. An agent keeps the voice its
//! identity hashes to unless a teammate holds it, even when another team holds
//! it too; only an agent a teammate displaced prefers, among the voices its
//! team leaves free, one no other team holds.

use std::collections::BTreeSet;
use std::fmt;

use wire::CanonicalUuid;

mod premium;

pub use premium::{is_enhanced, is_premium, is_speaking};

/// How many premium languages a notice names before eliding the rest.
const MAX_NAMED_LANGUAGES: usize = 8;

/// One published voice as the pool reads it.
#[derive(Clone, Copy, Debug)]
pub struct OfferedVoice<'a> {
    pub name: &'a str,
    /// The browser's voice URI, which Safari fills with the provider's
    /// canonical identifier.
    pub uri: &'a str,
    pub lang: &'a str,
    /// The device serves this voice itself rather than a remote service.
    pub local: bool,
    /// The device marks this voice as its default.
    pub default: bool,
}

/// Browser-level policy for one system-speech request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoicePolicy {
    /// Use the first explicitly configured voice the browser publishes.
    Listed,
    /// Let the browser select its device default voice.
    DeviceDefault,
    /// Spread a roster member over the device's voices, premium first. On a
    /// Mac a pool below premium carries a notice; elsewhere no premium tier
    /// exists to miss, so the same descent runs silently.
    Spread { premium_expected: bool },
}

/// Choose the voice policy before inspecting the browser's published voices.
///
/// Explicit configuration always wins. An unconfigured speaker outside the
/// roster uses the device default everywhere. A roster member spreads over
/// the device's voices except on Apple mobile browsers, which use their
/// device default because their voice records carry no tier evidence and
/// their engines offer one voice per language. Desktop-mode iPadOS presents a
/// Macintosh user agent, so its touch capability distinguishes it from a Mac.
/// Only a Mac is expected to publish premium voices.
#[must_use]
pub fn policy(
    configured: bool,
    roster_member: bool,
    user_agent: &str,
    max_touch_points: i32,
) -> VoicePolicy {
    if configured {
        VoicePolicy::Listed
    } else if !roster_member || apple_mobile(user_agent, max_touch_points) {
        VoicePolicy::DeviceDefault
    } else {
        VoicePolicy::Spread {
            premium_expected: user_agent.contains("Macintosh"),
        }
    }
}

/// Whether this browser runs on an Apple mobile device.
fn apple_mobile(user_agent: &str, max_touch_points: i32) -> bool {
    user_agent.contains("iPhone")
        || user_agent.contains("iPad")
        || user_agent.contains("iPod")
        || (user_agent.contains("Macintosh")
            && (user_agent.contains("Mobile/") || max_touch_points > 1))
}

/// The voice tier a spread landed on, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// macOS premium voices of the chosen language.
    Premium,
    /// macOS enhanced voices of the chosen language.
    Enhanced,
    /// Any voice the device serves in the chosen language.
    Local,
    /// The language has no published voice; the device default speaks.
    DeviceDefault,
}

impl Tier {
    /// What speaks in premium's place, as the gap notice names it.
    const fn substitute(self) -> &'static str {
        match self {
            Self::Premium => "premium voices",
            Self::Enhanced => "its enhanced voices",
            Self::Local => "its other voices",
            Self::DeviceDefault => "the device default voice",
        }
    }
}

/// Why the spread landed below premium, and what speaks instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PremiumGap {
    /// No premium voice is marked default and the device names no language.
    NoLanguage { offered: usize, premium: usize },
    /// No premium voice speaks `language`; `premium_languages` lists the
    /// languages premium voices do speak, in order.
    NoPremium {
        language: String,
        offered: usize,
        premium_languages: Vec<String>,
        substitute: Tier,
    },
}

impl fmt::Display for PremiumGap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLanguage { offered, premium } => write!(
                formatter,
                "automatic voices prefer a macOS premium voice in the device language, but \
                 no language is known; this browser publishes {offered} voices, {premium} \
                 premium, so the device default voice speaks. Set say.voices to name voices \
                 explicitly"
            ),
            Self::NoPremium {
                language,
                offered,
                premium_languages,
                substitute,
            } => {
                write!(
                    formatter,
                    "automatic voices prefer a macOS premium voice for language {language:?}, \
                     but none of the {offered} voices this browser publishes is one"
                )?;
                if !premium_languages.is_empty() {
                    let mut named: Vec<&str> = premium_languages
                        .iter()
                        .take(MAX_NAMED_LANGUAGES)
                        .map(String::as_str)
                        .collect();
                    let elided = premium_languages.len().saturating_sub(MAX_NAMED_LANGUAGES);
                    let more = format!("and {elided} more");
                    if elided > 0 {
                        named.push(&more);
                    }
                    write!(formatter, " (premium voices speak {})", named.join(", "))?;
                }
                write!(
                    formatter,
                    ", so {} speak instead. Download a premium {language} voice in System \
                     Settings > Accessibility > Spoken Content, or set say.voices",
                    substitute.substitute()
                )
            }
        }
    }
}

/// The spread's pool: indices into the offered voices, ordered by name, the
/// tier they came from, and the premium gap when the tier is below premium.
/// An empty pool speaks the device default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spread {
    pub pool: Vec<usize>,
    pub tier: Tier,
    pub gap: Option<PremiumGap>,
}

/// The spread pool over `offered`.
///
/// The language is that of the default premium voice, or `device_language`
/// when no premium voice is marked default. Within it the pool is the premium
/// voices, else the enhanced voices, else every speaking voice the device
/// serves (a macOS novelty voice never speaks for an agent); a
/// name published twice enters once. With no language known, or no voice in
/// the language, the device default speaks. A pool below premium carries the
/// gap only when `premium_expected`: a platform without a premium tier has
/// nothing to download.
#[must_use]
pub fn pool(
    offered: &[OfferedVoice<'_>],
    device_language: Option<&str>,
    premium_expected: bool,
) -> Spread {
    let premium: Vec<usize> = (0..offered.len())
        .filter(|&index| is_premium(&offered[index]))
        .collect();
    let Some(language) = premium
        .iter()
        .map(|&index| offered[index])
        .find(|voice| voice.default)
        .map(|voice| voice.lang)
        .or(device_language)
        .map(primary_language)
    else {
        return Spread {
            pool: Vec::new(),
            tier: Tier::DeviceDefault,
            gap: premium_expected.then_some(PremiumGap::NoLanguage {
                offered: offered.len(),
                premium: premium.len(),
            }),
        };
    };
    let tiers = [
        (Tier::Premium, in_language(offered, &language, is_premium)),
        (Tier::Enhanced, in_language(offered, &language, is_enhanced)),
        (Tier::Local, in_language(offered, &language, is_speaking)),
    ];
    let (tier, mut pool) = tiers
        .into_iter()
        .find(|(_, pool)| !pool.is_empty())
        .unwrap_or((Tier::DeviceDefault, Vec::new()));
    pool.sort_by(|&left, &right| offered[left].name.cmp(offered[right].name));
    pool.dedup_by(|right, left| offered[*left].name == offered[*right].name);
    let gap = (premium_expected && tier != Tier::Premium).then(|| PremiumGap::NoPremium {
        language,
        offered: offered.len(),
        premium_languages: premium
            .iter()
            .map(|&index| primary_language(offered[index].lang))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        substitute: tier,
    });
    Spread { pool, tier, gap }
}

/// Indices of the voices `admits` accepts that speak `language`.
fn in_language(
    offered: &[OfferedVoice<'_>],
    language: &str,
    admits: impl Fn(&OfferedVoice<'_>) -> bool,
) -> Vec<usize> {
    (0..offered.len())
        .filter(|&index| {
            admits(&offered[index]) && primary_language(offered[index].lang) == language
        })
        .collect()
}

/// The primary language subtag, so `en-GB`, `en_US`, and `EN` all read `en`.
fn primary_language(tag: &str) -> String {
    tag.split(['-', '_'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// The pool slot `member` speaks with among `teams`, given `size` voices.
///
/// A member absent from the roster keeps the slot its identity hashes to.
#[must_use]
pub fn voice_slot(
    teams: &[(CanonicalUuid, Vec<CanonicalUuid>)],
    member: CanonicalUuid,
    size: usize,
) -> Option<usize> {
    if size == 0 {
        return None;
    }
    let mut ordered: Vec<(CanonicalUuid, Vec<CanonicalUuid>)> = teams.to_vec();
    ordered.sort_by_key(|(team, _)| *team);
    let mut elsewhere = BTreeSet::new();
    for (_, mut members) in ordered {
        members.sort_unstable();
        let mut held = BTreeSet::new();
        for candidate in members {
            let preferred = preferred_slot(candidate, size);
            let probe = |free: &dyn Fn(usize) -> bool| {
                (0..size)
                    .map(|step| (preferred + step) % size)
                    .find(|&slot| free(slot))
            };
            let slot = if held.contains(&preferred) {
                probe(&|slot| !held.contains(&slot) && !elsewhere.contains(&slot))
                    .or_else(|| probe(&|slot| !held.contains(&slot)))
                    .unwrap_or(preferred)
            } else {
                preferred
            };
            if candidate == member {
                return Some(slot);
            }
            held.insert(slot);
        }
        elsewhere.extend(held);
    }
    Some(preferred_slot(member, size))
}

/// The slot one identity hashes to, mixed so neighbouring identities spread.
fn preferred_slot(member: CanonicalUuid, size: usize) -> usize {
    let bits = member.get().as_u128();
    let mut mixed = (bits >> 64) as u64 ^ bits as u64;
    mixed ^= mixed >> 30;
    mixed = mixed.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed ^= mixed >> 27;
    mixed = mixed.wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^= mixed >> 31;
    (mixed % size as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::{
        OfferedVoice, PremiumGap, Spread, Tier, VoicePolicy, policy, pool, preferred_slot,
        voice_slot,
    };
    use wire::CanonicalUuid;

    fn id(suffix: u16) -> CanonicalUuid {
        format!("00000000-0000-4000-8000-{suffix:012}")
            .parse()
            .expect("canonical test UUID")
    }

    fn slots(
        teams: &[(CanonicalUuid, Vec<CanonicalUuid>)],
        members: &[CanonicalUuid],
        size: usize,
    ) -> Vec<usize> {
        members
            .iter()
            .map(|member| voice_slot(teams, *member, size).expect("pool is nonempty"))
            .collect()
    }

    fn distinct(slots: &[usize]) -> bool {
        let mut seen = slots.to_vec();
        seen.sort_unstable();
        seen.dedup();
        seen.len() == slots.len()
    }

    #[test]
    fn apple_mobile_automatic_voice_uses_default_without_changing_other_boundaries() {
        let mobile_agents = [
            (
                "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15",
                0,
            ),
            (
                "Mozilla/5.0 (iPad; CPU OS 18_0 like Mac OS X) AppleWebKit/605.1.15",
                0,
            ),
            (
                "Mozilla/5.0 (iPod touch; CPU iPhone OS 17_0 like Mac OS X)",
                0,
            ),
            (
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15) AppleWebKit/605.1.15 \
                 Version/18.0 Mobile/15E148 Safari/604.1",
                0,
            ),
            (
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
                 (KHTML, like Gecko) Version/16.0 Safari/605.1.15",
                5,
            ),
        ];
        for (user_agent, max_touch_points) in mobile_agents {
            assert_eq!(
                policy(false, true, user_agent, max_touch_points),
                VoicePolicy::DeviceDefault,
                "{user_agent}"
            );
            assert_eq!(
                policy(true, true, user_agent, max_touch_points),
                VoicePolicy::Listed,
                "explicit voices still win on {user_agent}"
            );
        }
        let macintosh = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
             (KHTML, like Gecko) Version/16.0 Safari/605.1.15";
        assert_eq!(
            policy(false, true, macintosh, 0),
            VoicePolicy::Spread {
                premium_expected: true
            }
        );
        assert_eq!(policy(true, true, macintosh, 0), VoicePolicy::Listed);
        let elsewhere = [
            "Mozilla/5.0 (X11; Linux x86_64) Chrome/140",
            "Mozilla/5.0 (Android 14; Mobile; rv:130.0) Gecko/130.0 Firefox/130.0",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Firefox/130.0",
        ];
        for user_agent in elsewhere {
            assert_eq!(
                policy(false, true, user_agent, 0),
                VoicePolicy::Spread {
                    premium_expected: false
                },
                "{user_agent}"
            );
            assert_eq!(
                policy(false, false, user_agent, 0),
                VoicePolicy::DeviceDefault,
                "{user_agent}"
            );
        }
    }

    #[test]
    fn teammates_hear_distinct_voices_while_the_pool_has_room() {
        for size in 2..=9 {
            for team_size in 1..=size {
                let members: Vec<_> = (0..team_size).map(|n| id(100 + n as u16)).collect();
                let teams = [(id(1), members.clone())];
                assert!(
                    distinct(&slots(&teams, &members, size)),
                    "{team_size} teammates over {size} voices repeated a voice"
                );
            }
        }
    }

    #[test]
    fn an_agent_keeps_its_own_voice_unless_a_teammate_holds_it() {
        let others: Vec<_> = (0..5).map(|n| id(200 + n)).collect();
        for size in [2, 3, 5, 8] {
            for n in 0..6 {
                let alone = id(300 + n);
                let without = voice_slot(&[(id(9), vec![alone])], alone, size);
                let beside = voice_slot(
                    &[(id(1), others.clone()), (id(9), vec![alone])],
                    alone,
                    size,
                );
                assert_eq!(without, Some(preferred_slot(alone, size)));
                assert_eq!(beside, without, "another team moved a lone agent's voice");
            }
        }
    }

    #[test]
    fn a_displaced_teammate_prefers_a_voice_no_other_team_holds() {
        let size = 3;
        let ids: Vec<_> = (0..64).map(|n| id(400 + n)).collect();
        let (first, second) = ids
            .iter()
            .flat_map(|left| ids.iter().map(move |right| (*left, *right)))
            .find(|(left, right)| {
                left < right && preferred_slot(*left, size) == preferred_slot(*right, size)
            })
            .expect("some identities share a slot");
        let shared = preferred_slot(first, size);
        let neighbour = ids
            .iter()
            .copied()
            .find(|other| preferred_slot(*other, size) == (shared + 1) % size)
            .expect("some identity hashes to the next slot");
        let teams = [(id(1), vec![neighbour]), (id(2), vec![second, first])];
        assert_eq!(voice_slot(&teams, first, size), Some(shared));
        assert_eq!(voice_slot(&teams, second, size), Some((shared + 2) % size));
    }

    #[test]
    fn reordering_members_or_teams_keeps_every_voice() {
        let first: Vec<_> = (0..4).map(|n| id(400 + n)).collect();
        let second: Vec<_> = (0..3).map(|n| id(500 + n)).collect();
        let everyone: Vec<_> = first.iter().chain(&second).copied().collect();
        let teams = [(id(1), first.clone()), (id(2), second.clone())];
        let mut reversed_first = first.clone();
        reversed_first.reverse();
        let mut reversed_second = second.clone();
        reversed_second.rotate_left(1);
        let reordered = [(id(2), reversed_second), (id(1), reversed_first)];
        for size in [3, 5, 7, 12] {
            assert_eq!(
                slots(&teams, &everyone, size),
                slots(&reordered, &everyone, size)
            );
        }
    }

    #[test]
    fn a_team_larger_than_the_pool_uses_every_voice_before_sharing() {
        let members: Vec<_> = (0..7).map(|n| id(600 + n)).collect();
        let teams = [(id(1), members.clone())];
        let mut heard = slots(&teams, &members, 4);
        heard.sort_unstable();
        heard.dedup();
        assert_eq!(heard, vec![0, 1, 2, 3]);
    }

    #[test]
    fn an_empty_pool_offers_no_voice() {
        assert_eq!(voice_slot(&[(id(1), vec![id(2)])], id(2), 0), None);
    }

    /// A Safari-style voice: the provider identifier, a plain name.
    fn safari<'a>(name: &'a str, uri: &'a str, lang: &'a str, default: bool) -> OfferedVoice<'a> {
        OfferedVoice {
            name,
            uri,
            lang,
            local: true,
            default,
        }
    }

    /// A Chromium-style voice: the display name doubles as its URI.
    fn chromium<'a>(name: &'a str, lang: &'a str, default: bool) -> OfferedVoice<'a> {
        safari(name, name, lang, default)
    }

    fn names<'a>(offered: &[OfferedVoice<'a>], language: Option<&str>) -> Vec<&'a str> {
        let spread = pool(offered, language, true);
        assert_eq!(spread.tier, Tier::Premium);
        assert_eq!(spread.gap, None, "a premium pool carries no gap");
        spread
            .pool
            .into_iter()
            .map(|index| offered[index].name)
            .collect()
    }

    #[test]
    fn the_pool_keeps_only_premium_voices_of_the_default_premium_language() {
        let offered = [
            chromium("Samantha", "en-US", false),
            chromium("Zoe (Premium)", "en-US", true),
            chromium("Ava (Enhanced)", "en-US", false),
            chromium("Kyoko (Premium)", "ja-JP", false),
            safari(
                "Daniel",
                "com.apple.voice.premium.en_GB.Daniel",
                "en_GB",
                false,
            ),
            safari(
                "Daniel",
                "com.apple.voice.premium.en-GB.Daniel",
                "en-GB",
                false,
            ),
            safari(
                "Karen",
                "com.apple.voice.compact.en-AU.Karen",
                "EN-au",
                false,
            ),
            safari(
                "Bubbles",
                "com.apple.speech.synthesis.voice.Bubbles",
                "en-US",
                false,
            ),
        ];
        assert_eq!(names(&offered, Some("fr-FR")), ["Daniel", "Zoe (Premium)"]);
    }

    #[test]
    fn a_nonpremium_default_leaves_the_device_language_choosing_the_pool() {
        let offered = [
            chromium("Thomas (Premium)", "fr-FR", false),
            chromium("Alex", "en-US", true),
            chromium("Ava (Premium)", "en-US", false),
        ];
        assert_eq!(names(&offered, Some("fr")), ["Thomas (Premium)"]);
        assert_eq!(
            pool(&offered, None, true),
            Spread {
                pool: Vec::new(),
                tier: Tier::DeviceDefault,
                gap: Some(PremiumGap::NoLanguage {
                    offered: 3,
                    premium: 2
                }),
            }
        );
    }

    #[test]
    fn without_a_premium_voice_the_pool_descends_one_tier_at_a_time() {
        let enhanced = [
            chromium("Samantha", "en-US", true),
            chromium("Ava (Enhanced)", "en-US", false),
            safari(
                "Allison",
                "com.apple.voice.enhanced.en-US.Allison",
                "en-US",
                false,
            ),
            chromium("Kyoko (Premium)", "ja-JP", false),
            chromium("Thomas (Premium)", "fr-FR", false),
        ];
        let gap = |offered: usize, substitute: Tier| PremiumGap::NoPremium {
            language: "en".to_owned(),
            offered,
            premium_languages: vec!["fr".to_owned(), "ja".to_owned()],
            substitute,
        };
        assert_eq!(
            pool(&enhanced, Some("en-US"), true),
            Spread {
                pool: vec![2, 1],
                tier: Tier::Enhanced,
                gap: Some(gap(5, Tier::Enhanced)),
            }
        );

        let elsewhere = [
            chromium("Kyoko (Premium)", "ja-JP", false),
            chromium("Thomas (Premium)", "fr-FR", false),
        ];
        assert_eq!(
            pool(&elsewhere, Some("en-US"), true),
            Spread {
                pool: Vec::new(),
                tier: Tier::DeviceDefault,
                gap: Some(gap(2, Tier::DeviceDefault)),
            }
        );
    }

    #[test]
    fn the_local_tier_spreads_webkits_compact_voices_and_never_a_novelty_voice() {
        let gap = |offered: usize, substitute: Tier| PremiumGap::NoPremium {
            language: "en".to_owned(),
            offered,
            premium_languages: vec!["fr".to_owned(), "ja".to_owned()],
            substitute,
        };
        // WebKit publishes only built-in system voices, every one marked
        // default: compact, super-compact, eloquence, novelty, and remote
        // entries, never a tier. The novelty voices outnumber the compact ones
        // and are not speaking voices.
        let novelty = |name: &'static str, uri: &'static str| OfferedVoice {
            name,
            uri,
            lang: "en-US",
            local: true,
            default: true,
        };
        let mut system = vec![
            chromium("Kyoko (Premium)", "ja-JP", false),
            chromium("Thomas (Premium)", "fr-FR", false),
            safari(
                "Samantha",
                "com.apple.voice.compact.en-US.Samantha",
                "en-US",
                true,
            ),
            safari(
                "Daniel",
                "com.apple.voice.compact.en-GB.Daniel",
                "en-GB",
                true,
            ),
            safari(
                "Karen",
                "com.apple.voice.super-compact.en-AU.Karen",
                "en-AU",
                true,
            ),
            safari("Eddy", "com.apple.eloquence.en-US.Eddy", "en-US", true),
            OfferedVoice {
                name: "Google US English",
                uri: "Google US English",
                lang: "en-US",
                local: false,
                default: true,
            },
        ];
        let speaking = system.len();
        system.extend(
            [
                ("Albert", "com.apple.speech.synthesis.voice.Albert"),
                ("Bad News", "com.apple.speech.synthesis.voice.BadNews"),
                ("Bahh", "com.apple.speech.synthesis.voice.Bahh"),
                ("Bells", "com.apple.speech.synthesis.voice.Bells"),
                ("Boing", "com.apple.speech.synthesis.voice.Boing"),
                ("Bubbles", "com.apple.speech.synthesis.voice.Bubbles"),
                ("Cellos", "com.apple.speech.synthesis.voice.Cellos"),
                ("Wobble", "com.apple.speech.synthesis.voice.Deranged"),
                ("Fred", "com.apple.speech.synthesis.voice.Fred"),
                ("Good News", "com.apple.speech.synthesis.voice.GoodNews"),
                ("Jester", "com.apple.speech.synthesis.voice.Hysterical"),
                ("Junior", "com.apple.speech.synthesis.voice.Junior"),
                ("Kathy", "com.apple.speech.synthesis.voice.Kathy"),
                ("Organ", "com.apple.speech.synthesis.voice.Organ"),
                ("Superstar", "com.apple.speech.synthesis.voice.Princess"),
                ("Ralph", "com.apple.speech.synthesis.voice.Ralph"),
                ("Trinoids", "com.apple.speech.synthesis.voice.Trinoids"),
                ("Whisper", "com.apple.speech.synthesis.voice.Whisper"),
                ("Zarvox", "com.apple.speech.synthesis.voice.Zarvox"),
            ]
            .map(|(name, uri)| novelty(name, uri)),
        );
        assert_eq!(system.len(), speaking + 19);
        assert_eq!(
            pool(&system, Some("en-US"), true),
            Spread {
                pool: vec![3, 5, 4, 2],
                tier: Tier::Local,
                gap: Some(gap(26, Tier::Local)),
            },
            "Daniel, Eddy, Karen, Samantha by name; no novelty voice"
        );

        // A language whose only local voices are novelty voices is unspoken.
        let only_novelty = [
            novelty("Zarvox", "com.apple.speech.synthesis.voice.Zarvox"),
            chromium("Thomas (Premium)", "fr-FR", false),
            chromium("Kyoko (Premium)", "ja-JP", false),
        ];
        assert_eq!(
            pool(&only_novelty, Some("en-US"), true),
            Spread {
                pool: Vec::new(),
                tier: Tier::DeviceDefault,
                gap: Some(gap(3, Tier::DeviceDefault)),
            }
        );
    }

    #[test]
    fn a_platform_without_premium_voices_spreads_its_local_voices_silently() {
        // Firefox on Android publishes the engine's voices under their
        // engine identifiers; none carries macOS tier evidence.
        let android = [
            safari(
                "English United States",
                "urn:moz-tts:android:en-us-x-sfg",
                "en-US",
                true,
            ),
            safari(
                "English United Kingdom",
                "urn:moz-tts:android:en-gb-x-gba",
                "en-GB",
                false,
            ),
            safari("Deutsch", "urn:moz-tts:android:de-de-x-nfh", "de-DE", false),
            OfferedVoice {
                name: "English (Cloud)",
                uri: "urn:moz-tts:android:en-us-cloud",
                lang: "en-US",
                local: false,
                default: false,
            },
        ];
        assert_eq!(
            pool(&android, Some("en-US"), false),
            Spread {
                pool: vec![1, 0],
                tier: Tier::Local,
                gap: None,
            }
        );
        assert_eq!(
            pool(&android, None, false),
            Spread {
                pool: Vec::new(),
                tier: Tier::DeviceDefault,
                gap: None,
            }
        );
    }

    #[test]
    fn the_gap_notice_names_the_language_the_substitute_and_the_remedy() {
        let notice = PremiumGap::NoPremium {
            language: "en".to_owned(),
            offered: 68,
            premium_languages: Vec::new(),
            substitute: Tier::Local,
        }
        .to_string();
        assert!(notice.contains("language \"en\""), "{notice}");
        assert!(notice.contains("68 voices"), "{notice}");
        assert!(
            notice.contains("its other voices speak instead"),
            "{notice}"
        );
        assert!(notice.contains("say.voices"), "{notice}");
        assert!(!notice.contains("premium voices speak"), "{notice}");
    }

    #[test]
    fn spreading_over_the_premium_pool_ignores_other_tiers_and_order() {
        let premium = [
            chromium("Ava (Premium)", "en-US", false),
            chromium("Zoe (Premium)", "en-US", false),
            safari("Evan", "com.apple.voice.premium.en-US.Evan", "en-US", false),
        ];
        let mut mixed = vec![
            chromium("Samantha", "en-US", true),
            chromium("Ava (Enhanced)", "en-US", false),
        ];
        mixed.extend(premium.iter().rev().copied());
        let members: Vec<_> = (0..3).map(|n| id(700 + n)).collect();
        let teams = [(id(1), members.clone())];
        let heard = |offered: &[OfferedVoice<'_>]| -> Vec<String> {
            let pool = pool(offered, Some("en-US"), true).pool;
            members
                .iter()
                .map(|member| {
                    let slot = voice_slot(&teams, *member, pool.len()).expect("slot");
                    offered[pool[slot]].name.to_owned()
                })
                .collect()
        };
        let plain = heard(&premium);
        assert_eq!(heard(&mixed), plain, "nonpremium voices moved an agent");
        let mut distinct = plain.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), members.len(), "teammates shared a voice");
    }
}
