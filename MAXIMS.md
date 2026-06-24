# Maxim Authoring

Maxims are not lint rules and they are not facts. They are curated,
near-universal operator preferences whose false positives are usually still
useful reinforcement. A good maxim teaches the agent how this repo wants work
shaped while the agent is still speaking.

## What Makes A Maxim Good

A maxim belongs in the conscience when all of these are true:

- It reflects a stable taste, not a temporary task preference.
- It applies across many files, tasks, and agents.
- It is worth hearing even when the specific mention was harmless.
- It points toward an action the agent can take immediately.
- Its false-positive cost is low: the reminder reinforces a real preference
  instead of derailing the current work.

If a maxim needs many exceptions, it is too contextual. Put that guidance in a
task, design doc, or review comment instead.

## Specificity Is The Enemy

Specific maxims feel precise, but they age badly. "Do not add polling to wait
for UI state" is narrower than the real taste: do not hide missing signals with
busy waits. The broader maxim catches CLI loops, browser waits, supervisor
retries, and future shapes the author did not imagine.

Specificity also makes false positives expensive. If a maxim fires only in one
subsystem, the agent has to stop and ask whether the subsystem is in scope. If
it fires on a repo-wide preference, the agent can apply the reminder cheaply:
prefer the signal, delete the shim, remove the fallback, name the seam.

## Trigger-Word Bags

Trigger bags should fire often enough to catch drift, but not anchor the judge
to one accidental phrase. Use whole words and small phrase families:

```toml
[tool.spice.maxims.polling]
words = ["poll", "polling", "sleep", "retry loop"]
message = "DO NOT add polling, busy-waits, or retry loops to paper over timing; react to the real signal or restructure the flow so the wait is unnecessary."
```

Guidelines:

- Include common inflections: `fallback`, `fallbacks`, `falls back`.
- Include the words agents naturally use when proposing the bad shape.
- Avoid project nouns unless the preference truly belongs only to that noun.
- Avoid overloaded words that would fire on unrelated ordinary prose.
- Keep the bag name boring; it is an identifier, not the instruction.
- Keep the message imperative and portable across contexts.

The trigger only chooses candidates. The judge still receives the full maxim and
the full assistant statement, so the message must carry the real policy.

## Curation

Review maxims like production API. Add one when repeated steering proves the
preference is durable. Remove or narrow one when false positives stop being
cheap reinforcement. Merge overlapping bags when they teach the same action.

A healthy maxim set is short, blunt, and boring. It should make the conscience
feel like the operator's taste arriving early, not like a second task queue.
