//! The connected principal's personal inbox heading.

/// The inbox heading for the connected principal: their initials' inbox once
/// their recorded profile supplies initials, else "Your inbox".
pub(crate) fn heading(identity: Option<&wire::principal::PrincipalDisplay>) -> String {
    match identity.and_then(|identity| identity.initials.as_deref()) {
        Some(initials) => format!("{initials}'s inbox"),
        None => "Your inbox".to_owned(),
    }
}

/// Compact route attribution: immutable code always survives optional profile
/// decoration, while an unresolved principal retains its full actor identity.
pub(crate) fn label(
    principal: wire::CanonicalUuid,
    identity: Option<&wire::principal::PrincipalDisplay>,
) -> String {
    let Some(identity) = identity else {
        return wire::Actor::Principal(principal).to_string();
    };
    identity.initials.as_ref().map_or_else(
        || identity.code.to_string(),
        |initials| format!("{initials} · {}", identity.code),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_heading_names_resolved_initials_and_otherwise_addresses_the_reader() {
        let resolved = wire::principal::PrincipalProfile {
            name: Some("C Anthony Risinger".to_owned()),
            email: None,
        }
        .display(
            "018f82d4-6f59-7d06-92e3-bce913116301"
                .parse()
                .expect("principal"),
            "1".parse().expect("principal code"),
        );
        assert_eq!(heading(Some(&resolved)), "CA's inbox");
        let nameless = wire::principal::PrincipalProfile::default().display(
            "018f82d4-6f59-7d06-92e3-bce913116302"
                .parse()
                .expect("principal"),
            "2".parse().expect("principal code"),
        );
        assert_eq!(heading(Some(&nameless)), "Your inbox");
    }

    #[test]
    fn a_route_label_keeps_the_immutable_code_beside_profile_decoration() {
        let principal = "018f82d4-6f59-7d06-92e3-bce913116301"
            .parse()
            .expect("principal");
        let resolved = wire::principal::PrincipalProfile {
            name: Some("C Anthony Risinger".to_owned()),
            email: None,
        }
        .display(principal, "1".parse().expect("principal code"));
        assert_eq!(label(principal, Some(&resolved)), "CA · 1");

        let nameless = wire::principal::PrincipalProfile::default()
            .display(principal, "2".parse().expect("principal code"));
        assert_eq!(label(principal, Some(&nameless)), "2");
        assert_eq!(
            label(principal, None),
            format!("principal:{principal}"),
            "an unresolved route remains exact"
        );
    }
}
