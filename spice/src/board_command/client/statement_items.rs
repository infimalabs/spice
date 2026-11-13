use super::*;

/// Apply the item-addressed term edits named by `task modify`.
///
/// Bare `--term` values append. Replacement and removal handles resolve
/// only against the statement head this command witnessed, so argument order
/// cannot make a later edit silently target an item created by an earlier one.
pub(super) fn edited_term(
    matches: &ArgMatches,
    current: &[String],
) -> Result<Option<Vec<String>>, Outcome<CommandAnswer>> {
    let additions = matches
        .get_many::<String>("term")
        .into_iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    let replacements = matches
        .get_many::<String>("replace-term")
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let removals = matches
        .get_many::<String>("remove-term")
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if additions.is_empty() && replacements.is_empty() && removals.is_empty() {
        return Ok(None);
    }

    let items = wire::StatementItem::each(current).collect::<Vec<_>>();
    let mut edits = Vec::<(usize, Option<String>)>::new();
    for replacement in replacements {
        let Some((handle, text)) = replacement.split_once('=') else {
            return Err(refusal(
                "task::statement-item-replacement-shape",
                Some(replacement.clone()),
                "name a replacement as --replace-term HANDLE=TEXT",
            ));
        };
        let index = statement_item_index(handle, &items)?;
        record_statement_item_edit(&mut edits, index, Some(text.to_owned()), &items)?;
    }
    for handle in &removals {
        let index = statement_item_index(handle, &items)?;
        record_statement_item_edit(&mut edits, index, None, &items)?;
    }

    let mut term = Vec::with_capacity(
        current
            .len()
            .saturating_add(additions.len())
            .saturating_sub(removals.len()),
    );
    for (index, text) in current.iter().enumerate() {
        match edits.iter().find(|(target, _)| *target == index) {
            Some((_, Some(replacement))) => term.push(replacement.clone()),
            Some((_, None)) => {}
            None => term.push(text.clone()),
        }
    }
    term.extend(additions);
    Ok(Some(term))
}

/// Resolve one current item handle, refusing every spelling that is not one
/// current identity and listing the handles the caller may use instead.
pub(in crate::board_command) fn statement_item_index(
    handle: &str,
    items: &[wire::StatementItem],
) -> Result<usize, Outcome<CommandAnswer>> {
    let item = wire::StatementItem::resolve(handle, items).map_err(|error| {
        let reason = match error {
            wire::StatementItemPrefixError::Malformed => "task::statement-item-handle-invalid",
            wire::StatementItemPrefixError::Unknown => "task::statement-item-handle-stale",
            wire::StatementItemPrefixError::Ambiguous => "task::statement-item-handle-ambiguous",
        };
        let current = items
            .iter()
            .map(|item| item.handle(items))
            .collect::<Vec<_>>()
            .join(", ");
        refusal(
            reason,
            Some(handle.to_owned()),
            format!(
                "{error}; read the task again and name one current handle: {}",
                if current.is_empty() {
                    "none"
                } else {
                    current.as_str()
                }
            ),
        )
    })?;
    Ok(items
        .iter()
        .position(|current| current == item)
        .expect("the resolved item came from the current statement"))
}

/// One command may mutate an item at most once.
fn record_statement_item_edit(
    edits: &mut Vec<(usize, Option<String>)>,
    index: usize,
    replacement: Option<String>,
    items: &[wire::StatementItem],
) -> Result<(), Outcome<CommandAnswer>> {
    if edits.iter().any(|(target, _)| *target == index) {
        return Err(refusal(
            "task::statement-item-edited-twice",
            Some(items[index].handle(items).to_owned()),
            "replace or remove each statement item at most once per command",
        ));
    }
    edits.push((index, replacement));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modify(
        arguments: &[&str],
        current: &[String],
    ) -> Result<Option<Vec<String>>, Outcome<CommandAnswer>> {
        let matches = crate::cli::command()
            .try_get_matches_from(arguments)
            .expect("task modify arguments parse");
        let task = matches.subcommand_matches("task").expect("task command");
        let modify = task
            .subcommand_matches("modify")
            .expect("task modify command");
        edited_term(modify, current)
    }

    #[test]
    fn item_addressed_term_edits_preserve_unmentioned_rows_and_can_remove_the_last() {
        let current = vec!["first criterion".to_owned(), "second criterion".to_owned()];
        let items = wire::StatementItem::each(&current).collect::<Vec<_>>();
        let first = items[0].handle(&items);
        let second = items[1].handle(&items);
        let replacement = format!("{first}=rewritten criterion");
        let edited = modify(
            &[
                "spice",
                "task",
                "modify",
                "TASK",
                "--replace-term",
                &replacement,
                "--remove-term",
                second,
                "--term",
                "added criterion",
            ],
            &current,
        )
        .expect("current handles resolve");
        assert_eq!(
            edited,
            Some(vec![
                "rewritten criterion".to_owned(),
                "added criterion".to_owned()
            ])
        );

        let only = vec!["only criterion".to_owned()];
        let item = wire::StatementItem::of(&only[0]);
        let removed = modify(
            &[
                "spice",
                "task",
                "modify",
                "TASK",
                "--remove-term",
                item.handle(std::slice::from_ref(&item)),
            ],
            &only,
        )
        .expect("the last item can be removed");
        assert_eq!(removed, Some(Vec::new()));
    }

    #[test]
    fn item_addressed_term_refuses_short_and_stale_handles() {
        let current = vec!["current criterion".to_owned()];
        let item = wire::StatementItem::of(&current[0]);
        let handle = item.handle(std::slice::from_ref(&item));
        let stale_item = wire::StatementItem::of("retired criterion");
        let stale = stale_item
            .handle(std::slice::from_ref(&stale_item))
            .to_owned();
        for (written, reason) in [
            (
                handle[..wire::StatementItem::HANDLE_LEN - 1].to_owned(),
                "task::statement-item-handle-invalid",
            ),
            (stale, "task::statement-item-handle-stale"),
        ] {
            let outcome = modify(
                &["spice", "task", "modify", "TASK", "--remove-term", &written],
                &current,
            );
            let Err(Outcome::Refusal(refusal)) = outcome else {
                panic!("expected {reason} refusal, found {outcome:?}");
            };
            assert_eq!(refusal.reason(), reason);
            assert!(
                refusal.explanation().contains(handle),
                "the refusal must list the current handle: {refusal:?}"
            );
        }
    }
}
