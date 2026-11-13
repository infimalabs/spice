//! The vendor choices an app seat may select.
//!
//! Every row is projected from the two authorities a launch already reads:
//! the fixed compiled vendor table that owns which vendors exist, and the
//! admitted vendor configuration that owns what each one launches with. The
//! surface adds no vocabulary of its own, so a choice this answer offers is
//! one a later `lane::seat` admits.

use bus::Bus;
use spice::repository::Repository;
use wire::intent::{CommandAnswer, VendorCatalogAnswer, VendorChoice};
use wire::{Outcome, Refusal, Reply, ReplyPosition};

use super::store_fault;

/// Read the seatable vendors and the closed effort vocabulary.
pub(super) fn catalog(bus: &Bus, repository: &Repository) -> Outcome<CommandAnswer> {
    let cursor = match bus.read(|read| read.cursor()) {
        Ok(Some(cursor)) => cursor,
        Ok(None) => {
            return store_fault(
                "vendor::journal-empty",
                "a running resident has no startup authority fact",
            );
        }
        Err(error) => {
            return store_fault("vendor::journal-unreadable", &error.to_string());
        }
    };
    let resolved = match spice::config::load_repository(repository, &[]) {
        Ok(resolved) => resolved,
        Err(error) => {
            return Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
                "vendor::configuration-refused".to_owned(),
                Some("effective-configuration".to_owned()),
                Some("one admitted effective repository configuration".to_owned()),
                Some(error.diagnostic()),
            )));
        }
    };
    let root = resolved.root();
    let declared = spice::extension::VENDORS;
    let mut vendors = Vec::with_capacity(declared.len());
    for entry in declared {
        let vendor = entry.vendor;
        let Ok(name) = vendor.name().parse::<wire::Name>() else {
            return store_fault(
                "vendor::name-invalid",
                &format!(
                    "declared vendor {} does not spell a wire name",
                    vendor.name()
                ),
            );
        };
        let launch = vendor.launch_values(root);
        vendors.push(VendorChoice {
            name,
            label: vendor.label().to_owned(),
            default_model: launch.model.to_owned(),
            default_effort: launch.effort.as_str().to_owned(),
        });
    }
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(cursor),
        body: CommandAnswer::VendorCatalog(VendorCatalogAnswer {
            vendors,
            efforts: vendor::Effort::ALL
                .iter()
                .map(|effort| effort.as_str().to_owned())
                .collect(),
        }),
    })
}
