use super::*;

#[derive(Clone)]
pub(super) struct Change {
    requirements: Option<wire::EnvironmentRequirements>,
    trust_blocked: bool,
    task: Option<TaskStamp>,
    lease: Option<LeaseHead>,
}

impl Change {
    pub(super) const fn lease(&self) -> Option<&LeaseHead> {
        self.lease.as_ref()
    }

    pub(super) fn guard(&self, write: &mut JournalWrite) {
        let Some(requirements) = self.requirements.clone() else {
            return;
        };
        let expected = self.trust_blocked;
        write.guard(JournalGuard::new(move |read| {
            let environment = read.environment(&requirements)?;
            if requirements.has_unsatisfied_trust_grant(&environment) == expected {
                Ok(())
            } else {
                Err(GuardError::refused("task::require-trust-standing-moved"))
            }
        }));
    }

    pub(super) fn write_lease(self, write: &mut JournalWrite, edited: PendingFact) {
        let Some(lease) = self.lease else {
            return;
        };
        let task = self
            .task
            .expect("a prepared lease transition retains its task");
        write.lease_head(task, edited, move |_| lease);
    }
}

pub(super) fn prepare(
    entry: &Entry,
    command: &wire::AdmittedCommand,
    edit: &TaskEdit,
    requirements: &wire::EnvironmentRequirements,
    resources: &wire::ResourceRequirements,
    environment: &wire::Environment,
    at: Instant,
) -> Result<Change, Outcome<CommandAnswer>> {
    let Some(_) = edit.requirements else {
        return Ok(Change {
            requirements: None,
            trust_blocked: false,
            task: None,
            lease: None,
        });
    };
    if resources != &entry.statement().resources && entry.is_held() {
        return Err(refusal(
            "task::resource-edit-claimed",
            Some(entry.handle().to_string()),
            "release the active claim before replacing its resource map",
        ));
    }
    let trust_blocked = requirements.has_unsatisfied_trust_grant(environment);
    let lease = lease_transition(entry, command, trust_blocked, at)?;
    Ok(Change {
        requirements: Some(requirements.clone()),
        trust_blocked,
        task: lease.as_ref().map(|_| entry.born().clone()),
        lease,
    })
}

fn lease_transition(
    entry: &Entry,
    command: &wire::AdmittedCommand,
    trust_blocked: bool,
    at: Instant,
) -> Result<Option<LeaseHead>, Outcome<CommandAnswer>> {
    let Some(held) = entry.claim_of(command.actor()) else {
        return Ok(None);
    };
    let lease = held.lease().ok_or_else(|| {
        protocol_fault(
            "task::require-lease-missing",
            "an active claim has no lease while editing environmental requirements",
        )
    })?;
    let indefinite = spice::claim_lease::is_indefinite(lease);
    let transition = if trust_blocked && !indefinite {
        board::Active::admit(entry, command.actor(), at).map_err(|error| {
            refusal(
                "task::require-claim-inactive",
                Some(error.to_string()),
                "run task next to refresh the active claim before attaching its trust requirement",
            )
        })?;
        Some(spice::claim_lease::iced(lease))
    } else {
        None
    };
    if transition.is_some()
        && !command.expected().iter().any(|witness| {
            matches!(
                witness,
                Witness::Lease { task, claimed, .. }
                    if task == entry.born() && *claimed == held.claimed()
            )
        })
    {
        return Err(protocol_fault(
            "task::require-lease-witness-missing",
            "a requirement edit changing an active lease must witness that lease head",
        ));
    }
    Ok(transition)
}
