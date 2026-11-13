use super::*;
use crate::composer_drafts::{DraftStore, NEW_DRAFT_KEY, QuoteDestination};
use crate::work::tests::task;

fn seq(value: u64) -> wire::Seq {
    wire::Seq::new(value).expect("fixture sequence is positive")
}

fn draft(value: i64) -> BoardTask {
    let mut row = task(value, "/spice/app");
    row.presentation_title = format!("Draft {value}");
    row.workflow.body.phase = wire::Phase::Draft;
    row.statement.body.description = Some("Before".to_owned());
    row.statement.body.term = vec!["One".to_owned(), "Two".to_owned()];
    row
}

/// `row` as a later statement fact at `position` left it.
fn revised(row: &BoardTask, position: u64, revise: impl FnOnce(&mut BoardTask)) -> BoardTask {
    let mut next = row.clone();
    next.statement.position = seq(position);
    revise(&mut next);
    next
}

fn save(form: &DraftForm) -> (TaskEdit, Vec<Witness>) {
    let Ok(mut requests) = form.save(Vec::new(), &[]) else {
        panic!("the form revises its Draft");
    };
    let Some(DraftRequest::Edit { request, expected }) = requests.pop() else {
        panic!("the form revises its Draft");
    };
    (request, expected)
}

fn witness(row: &BoardTask, position: u64) -> Vec<Witness> {
    vec![Witness::Task {
        task: row.statement.body.born.clone(),
        head: seq(position),
    }]
}

#[test]
fn an_opened_form_holds_the_admitted_content_and_saves_nothing_untouched() {
    let row = draft(1);
    let form = DraftForm::open(&row);
    let fields = form.fields();
    assert_eq!(
        [
            DraftField::Title,
            DraftField::Intent,
            DraftField::Description
        ]
        .map(|field| fields.get(field)),
        ["Draft 1", "", "Before"],
    );
    assert!(form.save(Vec::new(), &[]).is_err());
}

#[test]
fn refreshes_that_carry_no_newer_revision_keep_every_typed_field() {
    let row = draft(1);
    let mut form = DraftForm::open(&row);
    form.edit(DraftField::Description, "Typed".to_owned());
    form.edit(DraftField::Title, "Typed title".to_owned());
    let typed = form.fields().clone();

    let mut behind = revised(&row, 1, |row| row.presentation_title = "Older".to_owned());
    behind.workflow.position = seq(1);
    for refresh in [&row, &behind] {
        form.refresh(refresh);
    }

    assert_eq!(form.fields(), &typed);
    let (request, expected) = save(&form);
    assert_eq!(request.title.as_deref(), Some("Typed title"));
    assert_eq!(request.description.as_deref(), Some("Typed"));
    assert_eq!(expected, witness(&row, 1));
}

#[test]
fn a_newer_revision_rebases_untouched_fields_and_keeps_typed_ones() {
    let row = draft(1);
    let mut form = DraftForm::open(&row);
    form.edit(DraftField::Description, "Typed".to_owned());

    form.refresh(&revised(&row, 5, |row| {
        row.presentation_title = "Retitled elsewhere".to_owned();
        row.statement.body.description = Some("Elsewhere".to_owned());
    }));

    assert_eq!(form.fields().get(DraftField::Title), "Retitled elsewhere");
    assert_eq!(form.fields().get(DraftField::Description), "Typed");
    let (request, expected) = save(&form);
    assert_eq!(request.title, None);
    assert_eq!(request.description.as_deref(), Some("Typed"));
    // The Draft already holds acceptance; this surface never revises it or
    // the flow, so the plan phase keeps owning the contract.
    assert!(request.term.is_none());
    assert_eq!(request.flow, None);
    assert_eq!(expected, witness(&row, 5));
}

#[test]
fn only_a_landed_save_adopts_the_admitted_spelling_of_what_it_sent() {
    let row = draft(1);
    let admitted = revised(&row, 5, |row| {
        row.presentation_title = "Retitled".to_owned();
    });
    let typing = |form: &mut DraftForm| {
        form.edit(DraftField::Title, "  Retitled ".to_owned());
        let sent = form.fields().clone();
        form.edit(DraftField::Description, "Typed after Save".to_owned());
        sent
    };

    let mut landed = DraftForm::open(&row);
    let sent = typing(&mut landed);
    landed.saved(sent);
    landed.refresh(&admitted);
    assert_eq!(landed.fields().get(DraftField::Title), "Retitled");
    assert_eq!(
        landed.fields().get(DraftField::Description),
        "Typed after Save"
    );

    let mut refused = DraftForm::open(&row);
    typing(&mut refused);
    refused.refresh(&admitted);
    assert_eq!(refused.fields().get(DraftField::Title), "  Retitled ");
    assert_eq!(
        refused.fields().get(DraftField::Description),
        "Typed after Save"
    );

    for form in [&landed, &refused] {
        let (request, expected) = save(form);
        assert_eq!(request.title, None);
        assert_eq!(request.description.as_deref(), Some("Typed after Save"));
        assert_eq!(expected, witness(&row, 5));
    }
}

#[test]
fn another_task_reopens_the_form_on_its_own_latest_content() {
    let first = draft(1);
    let second = draft(2);
    let mut form = DraftForm::open(&first);
    form.edit(DraftField::Title, "Typed into the first".to_owned());

    form.refresh(&second);
    assert_eq!(form.fields(), DraftForm::open(&second).fields());

    let latest = revised(&first, 7, |row| {
        row.presentation_title = "First, revised".to_owned();
    });
    form.edit(DraftField::Description, "Typed into the second".to_owned());
    form.refresh(&latest);
    assert_eq!(form.fields().get(DraftField::Title), "First, revised");
    assert_eq!(form.fields().get(DraftField::Description), "Before");
    assert!(form.save(Vec::new(), &[]).is_err());
}

fn image(name: &str) -> Media {
    Media::from_base64(name, "image/png", "iVBORw0KGgo=").expect("fixture media")
}

/// A Draft composer holding `body` and `media`, as the lane composer's state
/// holds them.
fn composed(body: &str, media: &[Media]) -> ComposerDraft {
    let mut store = DraftStore::default();
    store.hydrate(NEW_DRAFT_KEY.to_owned(), body.to_owned(), media.to_vec());
    store
        .get(NEW_DRAFT_KEY)
        .cloned()
        .expect("the composer holds the draft")
}

fn born() -> TaskStamp {
    TaskStamp::for_micros(1_700_000_000_000_000).expect("fixture stamp")
}

fn new_draft(intent: &str) -> NewDraft {
    NewDraft {
        intent: intent.to_owned(),
    }
}

#[test]
fn task_plan_and_design_drafts_share_lane_preparation_without_embedding_artifact_media() {
    let media = [image("one.png"), image("two.png")];
    let mut lane = DraftStore::default();
    lane.hydrate(
        "lane".to_owned(),
        "Title\n\nIt matters.\n\nContext\n\nin detail".to_owned(),
        media.to_vec(),
    );
    let lane_prepared = lane
        .prepare("lane")
        .expect("the lane media admits")
        .expect("the lane submission prepares");
    assert_eq!(lane_prepared.attachments, media);
    assert!(lane_prepared.message.contains("(attachment:"));
    let context = lane_prepared.snapshot;
    let mut created = Vec::new();
    for (kind, intent) in [
        ("", None),
        ("plan", Some(TaskIntent::Plan)),
        ("design", Some(TaskIntent::Design)),
    ] {
        let creation =
            creation(&new_draft(kind), context.clone(), born()).expect("the Draft is created");
        assert_eq!(creation.media, media);
        assert_eq!(creation.description, "Context\n\nin detail");
        let DraftRequest::Create {
            mut request,
            expected,
        } = creation.create
        else {
            panic!("a new Draft is created");
        };
        assert_eq!(expected, vec![Witness::Absence { task: born() }]);
        assert_eq!(request.title, "Title");
        assert_eq!(request.pitch, "It matters.");
        assert_eq!(request.intent, intent);
        assert_eq!(request.description.as_deref(), Some("Context\n\nin detail"));
        assert!(request.term.is_empty(), "{kind:?} authors no contract");
        assert_eq!(
            request.flow, None,
            "{kind:?} leaves the route to board policy"
        );
        assert!(
            !request
                .description
                .as_deref()
                .is_some_and(|description| description.contains("(attachment:"))
        );
        assert!(request.draft);
        request.intent = None;
        created.push(request);
    }
    assert!(created.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn one_primary_text_decodes_title_pitch_and_description() {
    for (body, pitch, description) in [
        (
            "\n  Ship it  \n\n  Because it matters.  ",
            "Because it matters.",
            None,
        ),
        (
            "Ship it\n\nWhy now\n\n  Typed in details \n\nMore",
            "Why now",
            Some("Typed in details \n\nMore"),
        ),
    ] {
        let creation =
            creation(&new_draft(""), composed(body, &[]), born()).expect("the Draft is created");
        let DraftRequest::Create { request, .. } = creation.create else {
            panic!("a new Draft is created");
        };
        assert_eq!(request.title, "Ship it", "{body:?}");
        assert_eq!(request.pitch, pitch, "{body:?}");
        assert_eq!(request.description.as_deref(), description, "{body:?}");
        assert_eq!(request.topic, None, "{body:?}");
    }
    for body in ["", "Title", "Title\n\n"] {
        assert!(creation(&new_draft(""), composed(body, &[]), born()).is_err());
    }
}

#[test]
fn quotes_sent_to_the_creator_compose_into_the_draft_description_in_source_order() {
    let steering = |key: &str| wire::QuoteTarget::Steering {
        key: key.to_owned(),
        digest: wire::QuoteTarget::text_digest(key),
    };
    let (earlier, later, own) = (steering("earlier"), steering("later"), steering("own"));
    let mut store = DraftStore::default();
    for (destination, target, order) in [
        (QuoteDestination::NewDraft, later.clone(), 2),
        (QuoteDestination::Composer, own.clone(), 3),
        (QuoteDestination::NewDraft, earlier.clone(), 1),
    ] {
        let key = destination.key("lane").to_owned();
        store.add_quote(
            key,
            target,
            "card".to_owned(),
            order,
            0,
            "preview".to_owned(),
        );
    }
    let lane = store
        .get("lane")
        .expect("the lane composer holds its own quote");
    assert_eq!(
        lane.quotes
            .iter()
            .map(|quote| quote.identity.clone())
            .collect::<Vec<_>>(),
        [own.to_string()]
    );
    store.set_quote_body(
        NEW_DRAFT_KEY.to_owned(),
        &earlier.to_string(),
        "Why".to_owned(),
    );
    store.set_body(
        NEW_DRAFT_KEY.to_owned(),
        "Title\n\nIt matters.\n\nPrimary".to_owned(),
    );
    let context = store
        .get(NEW_DRAFT_KEY)
        .cloned()
        .expect("the creator holds the routed quotes");

    let creation = creation(&new_draft("design"), context, born()).expect("the Draft is created");

    let expected = format!(
        "{}\n\nWhy\n\n{}\n\nPrimary",
        earlier.markdown(),
        later.markdown()
    );
    assert_eq!(creation.description, expected);
    let DraftRequest::Create { request, .. } = creation.create else {
        panic!("a new Draft is created");
    };
    assert_eq!(request.description, Some(expected));
}

#[test]
fn a_context_media_admission_refuses_sends_no_creation() {
    let over_count = (0..=wire::media::MAX_MEDIA_ITEMS)
        .map(|index| image(&format!("{index}.png")))
        .collect::<Vec<_>>();
    let mut unrecognized = image("unrecognized.png");
    unrecognized.data_url = "data:image/png;base64,AAAAAAAAAAAA".to_owned();
    for media in [over_count, vec![unrecognized]] {
        let context = composed("Title\n\nIt matters.", &media);
        for kind in ["", "plan", "design"] {
            assert!(creation(&new_draft(kind), context.clone(), born()).is_err());
        }
    }
}

#[test]
fn a_reopened_draft_rehydrates_its_images_and_saves_only_a_changed_set() {
    let row = draft(1);
    let admitted = vec![image("one.png"), image("two.png")];
    let key = "spice-draft:1".to_owned();
    let mut store = DraftStore::default();
    store.hydrate(key.clone(), "Before".to_owned(), admitted.clone());
    let prepared = |store: &DraftStore| {
        prepare_context(
            store
                .get(&key)
                .expect("the composer holds the Draft")
                .clone(),
        )
        .expect("the admitted set prepares")
    };
    let context = prepared(&store);
    assert_eq!(context.attachments, admitted);
    assert_eq!(context.message, "Before");
    let snapshot = context.snapshot.clone();
    let form = DraftForm::open(&row);
    assert!(form.save(context.attachments.clone(), &admitted).is_err());

    store.hydrate(key.clone(), String::new(), Vec::new());
    let rehydrated = prepared(&store);
    assert_eq!(rehydrated.message, context.message);
    assert_eq!(rehydrated.attachments, context.attachments);
    assert_eq!(rehydrated.snapshot, snapshot);

    let trimmed = vec![admitted[1].clone()];
    let Ok(mut requests) = form.save(trimmed, &admitted) else {
        panic!("a changed image set saves");
    };
    let (Some(DraftRequest::Media { request, expected }), true) =
        (requests.pop(), requests.is_empty())
    else {
        panic!("only the image set is saved");
    };
    assert_eq!(request.task, row.statement.body.handle);
    assert_eq!(request.media, vec![admitted[1].clone()]);
    assert_eq!(
        expected,
        vec![Witness::Existence {
            task: row.statement.body.born.clone()
        }]
    );
}
