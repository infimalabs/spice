//! Operator CLI over durable principals.

use std::collections::BTreeSet;

use super::*;

use wire::principal::{
    PrincipalAdmit, PrincipalDeauthorize, PrincipalEdit, PrincipalProfile, PrincipalRead,
    PrincipalRole, PrincipalRoleRef,
};

/// Route one `principal` leaf.
pub(super) fn dispatch_principal_client(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Option<Outcome<CommandAnswer>> {
    let outcome = match contract.path {
        ["principal", "list"] => crate::command_client::execute_typed(
            repository,
            contract,
            CommandKind::PrincipalRead,
            Vec::new(),
            &PrincipalRead {},
        ),
        ["principal", "admit"] => crate::command_client::execute_typed(
            repository,
            contract,
            CommandKind::PrincipalAdmit,
            Vec::new(),
            &PrincipalAdmit {
                name: matches.get_one::<String>("name").cloned(),
                email: matches.get_one::<String>("email").cloned(),
                roles: if matches.get_flag("operator") {
                    BTreeSet::from([PrincipalRole::Operator])
                } else {
                    BTreeSet::new()
                },
            },
        ),
        ["principal", "profile"] => match principal(matches) {
            Ok(principal) => crate::command_client::execute_typed(
                repository,
                contract,
                CommandKind::PrincipalEdit,
                Vec::new(),
                &PrincipalEdit {
                    principal,
                    name: matches.get_one::<String>("name").cloned(),
                    email: matches.get_one::<String>("email").cloned(),
                },
            ),
            Err(outcome) => outcome,
        },
        ["principal", "role", verb @ ("grant" | "revoke")] => {
            match principal(matches).and_then(|principal| Ok((principal, role(matches)?))) {
                Ok((principal, role)) => crate::command_client::execute_typed(
                    repository,
                    contract,
                    if *verb == "grant" {
                        CommandKind::PrincipalGrant
                    } else {
                        CommandKind::PrincipalRevoke
                    },
                    Vec::new(),
                    &PrincipalRoleRef { principal, role },
                ),
                Err(outcome) => outcome,
            }
        }
        ["principal", "deauthorize"] => match principal(matches) {
            Ok(principal) => crate::command_client::execute_typed(
                repository,
                contract,
                CommandKind::PrincipalDeauthorize,
                Vec::new(),
                &PrincipalDeauthorize { principal },
            ),
            Err(outcome) => outcome,
        },
        _ => return None,
    };
    Some(render(outcome))
}

fn principal(matches: &ArgMatches) -> Result<wire::CanonicalUuid, Outcome<CommandAnswer>> {
    let Some(written) = matches.get_one::<String>("PRINCIPAL") else {
        return Err(refusal(
            "principal::identity-missing",
            None,
            "name the principal by its identity from `spice principal list`",
        ));
    };
    written.parse().map_err(|error: wire::IdentError| {
        refusal(
            "principal::identity-invalid",
            Some(written.clone()),
            error.to_string(),
        )
    })
}

fn role(matches: &ArgMatches) -> Result<PrincipalRole, Outcome<CommandAnswer>> {
    let written = matches.get_one::<String>("ROLE").map_or("", String::as_str);
    written.parse().map_err(|error: String| {
        refusal("principal::role-invalid", Some(written.to_owned()), &error)
    })
}

fn render(outcome: Outcome<CommandAnswer>) -> Outcome<CommandAnswer> {
    let Outcome::Reply(Reply { position, body }) = outcome else {
        return outcome;
    };
    let text = match body {
        CommandAnswer::Principals(answer) => {
            let mut text = String::new();
            if answer.principals.is_empty() {
                text.push_str("no principals\n");
            }
            for listing in &answer.principals {
                let head = &listing.head;
                text.push_str(&format!(
                    "{} code {} born {} admitted {} {} {}\n",
                    head.principal,
                    wire::principal::PrincipalCode::of(head),
                    head.born,
                    listing.admitted,
                    roles(&head.roles),
                    profile(&head.profile)
                ));
            }
            text
        }
        CommandAnswer::Principal(standing) => {
            let mut text = format!(
                "{} {} {}\n",
                standing.principal,
                roles(&standing.roles),
                profile(&standing.profile)
            );
            if let Some(capability) = standing.capability {
                text.push_str(&format!(
                    "capability {capability} (shown once; open the app with it as the URL \
                     fragment)\n"
                ));
            }
            text
        }
        body => {
            return Outcome::Reply(Reply { position, body });
        }
    };
    Outcome::Reply(Reply {
        position,
        body: CommandAnswer::Text(TextAnswer { text }),
    })
}

fn roles(roles: &BTreeSet<PrincipalRole>) -> String {
    if roles.is_empty() {
        return "no-role".to_owned();
    }
    roles
        .iter()
        .map(|role| role.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

fn profile(profile: &PrincipalProfile) -> String {
    match (&profile.name, &profile.email) {
        (Some(name), Some(email)) => format!("{name} <{email}>"),
        (Some(name), None) => name.clone(),
        (None, Some(email)) => format!("<{email}>"),
        (None, None) => "(no profile)".to_owned(),
    }
}
