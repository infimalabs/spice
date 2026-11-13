use wire::intent::AnswerShape;

/// One successful reply body for every declared answer shape.
///
/// The board task row is a reachable pending workflow with a deferral,
/// alongside claim and lease state. Terminal workflow documents and their
/// completion instants are covered independently in `binding.rs`; combining
/// a live workflow with terminal completion would violate the workflow type.
///
/// Round-tripping alone cannot prove that the row's DTO still names every
/// field. [`the_board_task_row_is_the_struct_this_crate_declares`] builds it as
/// a struct literal and asserts that it encodes to exactly the JSON below, so
/// a field added to the DTO fails this crate's build and a changed spelling
/// fails the equality.
// Principal and document receipt fixtures.
pub(super) const ANSWERS: &[(AnswerShape, &str)] = &[
    (
        AnswerShape::Board,
        r#"{
  "schema": 1,
  "cursor": 1,
  "task_barrier": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "principal": "principal:00000000-0000-0000-0000-000000000000",
  "actor_route": {
    "actor": "principal:00000000-0000-0000-0000-000000000000",
    "origin_thread": "operator"
  },
  "tasks": [
    {
      "origin_thread": "operator",
      "presentation_title": "Bind a board task",
      "statement": {
        "position": 2,
        "body": {
          "handle": "SPICE-1kKRRsDPx",
          "born": "1kKRRsDPx",
          "topic_origin": 2,
          "pitch": "Readers see why a row binds.",
          "description": "One task spelled through every field the shape carries.",
          "term": ["the row decodes", "the row re-encodes unchanged"],
          "resources": {"windows-guest": 1},
          "topic": { "path": "/wire/binding", "named": "/wire/binding" },
          "priority": "high",
          "taste": {
            "matches": [{
              "field": "term",
              "line": 1,
              "taste": "compatibility",
              "finding": {
                "trigger": "compatibility",
                "column": 12,
                "message": "Keep one current path."
              }
            }],
            "rechecks": 2,
            "opened": 7
          },
          "dependencies": ["1kKRRsDQx"],
          "provenance": "task:1kKRRsDPx",
          "creation": { "kind": "unavailable", "reason": "principal-actor" },
          "revision": 2
        }
      },
      "workflow": {
        "position": 3,
        "body": {
          "flow": ["draft", "todo", "review"],
          "phase": "todo",
          "status": "pending",
          "review": "unassigned",
          "deferral": "2026-08-01T07:07:19.000Z"
        }
      },
      "completion": {
        "position": 3,
        "body": {
          "targets": { "review": 2 },
          "round": 3
        }
      },
      "claims": [{
        "claim": {
          "position": 4,
          "body": {
            "actor": "principal:00000000-0000-0000-0000-000000000000",
            "fact": 4,
            "claimed": 4,
            "cell": {
              "topic": "/wire/binding",
              "phase": "todo"
            },
            "lane": "018f82d4-6f59-7d06-92e3-bce9131163c5",
            "occupant": "018f82d4-6f59-7d06-92e3-bce9131163c6",
            "epoch": "018f82d4-6f59-7d06-92e3-bce9131163c3",
            "disposition": "active"
          }
        },
        "lease": {
          "position": 5,
          "body": {
            "claimed": 4,
            "start": "2026-08-01T06:07:19.000Z",
            "renewal": "2026-08-01T06:37:19.000Z",
            "maximum": "2026-08-01T09:07:19.000Z",
            "expiry": "2026-08-01T07:07:19.000Z",
            "epoch": "018f82d4-6f59-7d06-92e3-bce9131163c3"
          }
        }
      }],
      "claim_context": {
        "claimed": 4,
        "lane": "018f82d4-6f59-7d06-92e3-bce9131163c5",
        "occupant": "018f82d4-6f59-7d06-92e3-bce9131163c6",
        "epoch": "018f82d4-6f59-7d06-92e3-bce9131163c3",
        "source": {
          "proven": {
            "heads": [{
              "lane": "018f82d4-6f59-7d06-92e3-bce9131163c5",
              "vendor": "omp",
              "stream": "session",
              "source": "018f82d4-6f59-7d06-92e3-bce9131163c7",
              "next": { "member": 3, "offset": 44 }
            }]
          }
        },
        "recovery": {
          "kind": "transcript",
          "session": "thread-123"
        }
      },
      "git_context": {
        "fact": 6,
        "kind": "completion",
        "boundary": {
          "head": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          "integrated": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          "upstream": "origin/main",
          "upstream_head": "cccccccccccccccccccccccccccccccccccccccc",
          "local_commits": 2
        }
      }
    }
  ],
  "appliance": "0.2.0-dev.0.dTYp0bz3B+bin.fixture",
  "residency": {
    "position": 1,
    "body": {
      "epoch": "018f82d4-6f59-7d06-92e3-bce9131163c3",
      "state": "seated",
      "topology_revision": 0,
      "lanes": [{
        "lane": "018f82d4-6f59-7d06-92e3-bce9131163c2",
        "team": "018f82d4-6f59-7d06-92e3-bce9131163c2",
        "name": "Ava",
        "worktree": "/worktrees/lane-a",
        "life": "drive",
        "vendor": "omp",
        "occupant": "018f82d4-6f59-7d06-92e3-bce9131163c5",
        "session": null,
        "lineage": ["018f82d4-6f59-7d06-92e3-bce9131163c5"],
        "capabilities": ["018f82d4-6f59-7d06-92e3-bce9131163c7"],
        "disposition": "occupied"
      }],
      "subscriptions": [{
        "team": "018f82d4-6f59-7d06-92e3-bce9131163c2",
        "drive": true,
        "drain": false
      }]
    }
  }
}"#,
    ),
    (AnswerShape::Text, r#"{ "text": "graph dependencies\n" }"#),
    (
        AnswerShape::Task,
        r#"{
  "placement": null,
  "ownership": null,
  "task": "SPICE-1kKRRsDPx",
  "status": "done",
  "completion": {
    "operation": "018f82d4-6f59-7d06-92e3-bce9131163c4",
    "state": "running"
  }
}"#,
    ),
    (
        AnswerShape::Claim,
        r#"{ "ownership": null, "task": "SPICE-1kKRRsDPx", "holder": "principal:00000000-0000-0000-0000-000000000000" }"#,
    ),
    (
        AnswerShape::Directive,
        r#"{
  "directive": "018f82d4-6f59-7d06-92e3-bce9131163c4",
  "recipient": "lane:018f82d4-6f59-7d06-92e3-bce9131163c2",
  "key": "1kJJP87Bx",
  "authored": "2026-08-01T06:07:19.000Z",
  "disposition": "acked"
}"#,
    ),
    (
        AnswerShape::SteerLine,
        r#"{
  "input": {
    "directive": "018f82d4-6f59-7d06-92e3-bce9131163c4",
    "principal": "018f82d4-6f59-7d06-92e3-bce9131163c7"
  },
  "effect": {
    "root": "1kJJP87Bx",
    "action": "reaction"
  },
  "body": "unsent"
}"#,
    ),
    (
        AnswerShape::Inbox,
        r#"{
  "epoch": "018f82d4-6f59-7d06-92e3-bce9131163c3",
  "lanes": [{
    "lane": "018f82d4-6f59-7d06-92e3-bce9131163c2",
    "occupant": "018f82d4-6f59-7d06-92e3-bce9131163c5",
    "pending": [{
      "directive": "018f82d4-6f59-7d06-92e3-bce9131163c4",
      "key": "1kJJP87Bx",
      "text": "drain the queue",
      "preview": "drain the queue",
      "from": null,
      "sender": null,
      "authored": "2026-08-01T06:07:19.000Z",
      "stage": "published",
      "expires": "2026-08-01T07:07:19.000Z"
    }]
  }]
}"#,
    ),
    (
        AnswerShape::Consent,
        r#"{ "request": "018f82d4-6f59-7d06-92e3-bce9131163c4", "answer": "granted" }"#,
    ),
    (
        AnswerShape::ConsentQueue,
        r#"{
  "pending": [{
    "request": "018f82d4-6f59-7d06-92e3-bce9131163c4",
    "lane": "018f82d4-6f59-7d06-92e3-bce9131163c2",
    "operation": "shell:git push",
    "rule": "browser",
    "cause": { "kind": "budget-exhausted", "bound": 2 },
    "budget": { "rule": "browser", "bound": 2, "remaining": 0 }
  }],
  "budgets": [{ "rule": "browser", "bound": 2, "remaining": 0 }]
}"#,
    ),
    (
        AnswerShape::ConsentPolicy,
        r#"{
  "posture": "queue",
  "rules": [{
    "name": "browser",
    "tool": "shell:*",
    "answer": "allow",
    "budget": 2,
    "remaining": 1
  }]
}"#,
    ),
    (
        AnswerShape::GlobalSettings,
        r#"{
  "fast_mode": true,
  "principals": [{
    "principal": "018f82d4-6f59-7d06-92e3-bce9131163c3",
    "code": "6wB",
    "name": "C Adams",
    "email": "c@example.invalid",
    "initials": "CA",
    "gravatar": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
  }]
}"#,
    ),
    (
        AnswerShape::VendorCatalog,
        r#"{
  "vendors": [{
    "name": "omp",
    "label": "omp",
    "default_model": "gpt-5.6-sol",
    "default_effort": "high"
  }],
  "efforts": ["low", "medium", "high", "xhigh", "max"]
}"#,
    ),
    (
        AnswerShape::Trust,
        r#"{
  "digest": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  "standing": "granted"
}"#,
    ),
    (
        AnswerShape::Lane,
        r#"{
  "epoch": "018f82d4-6f59-7d06-92e3-bce9131163c3",
  "state": "seated",
  "topology_revision": 9,
  "subscriptions": [{
    "team": "018f82d4-6f59-7d06-92e3-bce9131163c2",
    "drive": true,
    "drain": false
  }, {
    "team": "018f82d4-6f59-7d06-92e3-bce9131163c6",
    "drive": false,
    "drain": false
  }],
  "lanes": [{
    "lane": "018f82d4-6f59-7d06-92e3-bce9131163c2",
    "team": "018f82d4-6f59-7d06-92e3-bce9131163c2",
    "name": "Ava",
    "worktree": "/worktrees/lane-a",
    "speech": {
      "backend": "system",
      "voices": ["Samantha"],
      "words_per_minute": 190
    },
    "life": "drive",
    "vendor": "omp",
    "occupant": "018f82d4-6f59-7d06-92e3-bce9131163c5",
    "lineage": ["018f82d4-6f59-7d06-92e3-bce9131163c5"],
    "bound": true,
    "disposition": "occupied",
    "liveness": "working"
  }, {
    "lane": "018f82d4-6f59-7d06-92e3-bce9131163c6",
    "team": "018f82d4-6f59-7d06-92e3-bce9131163c6",
    "worktree": "/worktrees/lane-b",
    "speech": {
      "backend": "disabled"
    },
    "life": "steer",
    "vendor": "omp",
    "lineage": ["018f82d4-6f59-7d06-92e3-bce9131163c8"],
    "bound": false,
    "disposition": "vacant",
    "liveness": "vacant"
  }]
}"#,
    ),
    (
        AnswerShape::LaneChange,
        r#"{
  "lane": "018f82d4-6f59-7d06-92e3-bce9131163c2",
  "topology_revision": 10,
  "from": "occupied",
  "to": "vacant"
}"#,
    ),
    (
        AnswerShape::LaneRenewal,
        r#"{
  "team": "018f82d4-6f59-7d06-92e3-bce9131163c2",
  "lane": "018f82d4-6f59-7d06-92e3-bce9131163c3",
  "topology_revision": 9,
  "renewal": {
    "team": "018f82d4-6f59-7d06-92e3-bce9131163c2",
    "lane": "018f82d4-6f59-7d06-92e3-bce9131163c3",
    "revision": 9,
    "state": {
      "state": "requested",
      "predecessor": {
        "occupant": "018f82d4-6f59-7d06-92e3-bce9131163c4",
        "session": "session-old",
        "thread": "thread-old"
      },
      "team_slot": 2
    }
  }
}"#,
    ),
    (
        AnswerShape::State,
        r#"{ "from": "seated", "to": "quiescing" }"#,
    ),
    (
        AnswerShape::Recall,
        r#"{
  "barrier": 48213,
  "repository": "/worktrees/repository",
  "residency": {
    "epoch": "018f82d4-6f59-7d06-92e3-bce9131163c3",
    "state": "seated",
    "topology_revision": 9,
    "lanes": [{
      "lane": "018f82d4-6f59-7d06-92e3-bce9131163c2",
      "team": "018f82d4-6f59-7d06-92e3-bce9131163c2",
      "name": "Ava",
      "worktree": "/worktrees/lane-a",
      "speech": {
        "backend": "system",
        "voices": ["Samantha"],
        "words_per_minute": 190
      },
      "life": "drive",
      "vendor": "omp",
      "occupant": "018f82d4-6f59-7d06-92e3-bce9131163c5",
      "lineage": ["018f82d4-6f59-7d06-92e3-bce9131163c5"],
      "bound": true,
      "disposition": "occupied",
      "liveness": "running"
    }],
    "subscriptions": [{
      "team": "018f82d4-6f59-7d06-92e3-bce9131163c2",
      "drive": true,
      "drain": false
    }]
  },
  "recent": [{
    "seq": 48213,
    "kind": "task::added",
    "actor": "principal:00000000-0000-0000-0000-000000000000",
    "origin": "task:WIRE-parent",
    "lane": "018f82d4-6f59-7d06-92e3-bce9131163c2",
    "provenance": {
      "kind": "steering",
      "directive": "018f82d4-6f59-7d06-92e3-bce9131163c4",
      "key": "1kJJP87Bx",
      "disposition": "acked",
      "stage": "overdue",
      "escalations": [{
        "stage": "reminded",
        "at": "2026-08-06T03:00:02.000Z"
      }, {
        "stage": "escalated",
        "at": "2026-08-06T03:01:00.000Z"
      }, {
        "stage": "overdue",
        "at": "2026-08-06T03:05:00.000Z"
      }]
    }
  }]
}"#,
    ),
    (
        AnswerShape::Taste,
        r#"{ "name": "polling", "confirmed": true }"#,
    ),
    (
        AnswerShape::Recognition,
        r#"{
  "key": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
  "fresh": true
}"#,
    ),
];
