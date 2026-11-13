use super::*;

use crate::task_artifact_command::cited_digests;

/// Replace one unfiled pending Draft's image artifacts with exactly the
/// admitted set, in presentation order. Non-image artifacts stay.
///
/// Images live in the artifact store rather than the journal, so nothing is
/// journaled and the reply carries the snapshot the Draft was read at. Every
/// refusal leaves the preceding manifest authoritative.
pub(in crate::board_command) fn task_media(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    media: &wire::intent::TaskMedia,
    repository: Option<Host<'_>>,
) -> Outcome<CommandAnswer> {
    let Some(repository) = repository else {
        return refusal(
            "task::media-repository-required",
            Some(media.task.to_string()),
            "attach Draft images through a resident serving one repository",
        );
    };
    let desired = match admitted_images(&media.media) {
        Ok(desired) => desired,
        Err(outcome) => return outcome,
    };
    let snapshot = match validated_working_snapshot(
        bus,
        Some(repository.repository),
        command.actor().clone(),
        std::slice::from_ref(&media.task),
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let entry = match unfiled_draft(&snapshot, &media.task) {
        Ok(entry) => entry,
        Err(outcome) => return outcome,
    };
    let state = match repository.admit_state() {
        Ok(state) => state,
        Err(error) => return repository_state_failure(&error),
    };
    let store = artifact::Store::new(state.artifacts(), state.artifacts_lock());
    if let Err(outcome) = replace_images(bus, &store, entry.born(), &desired) {
        return outcome;
    }
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(snapshot.cursor),
        body: CommandAnswer::Task(Box::new(wire::intent::TaskAnswer {
            diagnostic: None,
            ownership: Some(snapshot.ownership(entry)),
            placement: None,
            created: Default::default(),
            taste: None,
            completion_contract: None,
            task: entry.handle().clone(),
            status: "pending".to_owned(),
            completion: None,
            claim: None,
            closed: Vec::new(),
            next: None,
        })),
    })
}

/// Admit the whole set under the shared media bounds and decode every member,
/// so a refused set never reaches the store.
fn admitted_images(
    media: &[wire::Media],
) -> Result<Vec<(&wire::Media, Vec<u8>)>, Outcome<CommandAnswer>> {
    let refused = |error: wire::media::MediaError| {
        refusal(
            "task::media-refused",
            Some(error.to_string()),
            "attach recognized images within the shared count and size bounds",
        )
    };
    wire::media::admit_media(media).map_err(refused)?;
    media
        .iter()
        .map(|item| Ok((item, item.decoded_bytes().map_err(refused)?)))
        .collect()
}

fn unfiled_draft<'a>(
    snapshot: &'a ValidatedSnapshot,
    task: &TaskHandle,
) -> Result<&'a Entry, Outcome<CommandAnswer>> {
    let Some(entry) = task_entry(&snapshot.entries, task) else {
        return Err(refusal(
            "task::unknown",
            Some(task.to_string()),
            "read board and name one current task handle",
        ));
    };
    if !entry.is_draft() {
        return Err(refusal(
            "task::media-draft-only",
            Some(entry.handle().to_string()),
            "attach images to filed work with spice task artifact add",
        ));
    }
    if !entry.workflow().status.is_pending() {
        return Err(refusal(
            "task::settled",
            Some(entry.handle().to_string()),
            "replace images only on a pending Draft",
        ));
    }
    Ok(entry)
}

fn replace_images(
    bus: &Bus,
    store: &artifact::Store,
    task: &wire::TaskStamp,
    desired: &[(&wire::Media, Vec<u8>)],
) -> Result<(), Outcome<CommandAnswer>> {
    let cited = cited_digests(&crate::board_command::room::room_publications(
        bus, task, None,
    )?);
    let created_at = current_instant()?;
    let images = desired
        .iter()
        .map(|(media, bytes)| artifact::ImageBytes {
            bytes,
            name: &media.name,
            media_type: &media.content_type,
            retention: artifact::Retention::Permanent,
        })
        .collect::<Vec<_>>();
    store
        .replace_images(artifact::ReplaceImagesRequest {
            task,
            images: &images,
            cited: &cited,
            created_at,
        })
        .map_err(artifact_operation_failure)?;
    Ok(())
}
