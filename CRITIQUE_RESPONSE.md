# Critique Response

Maps each recommendation in `spice-critiques/01_the_battery.md` to its task
handle, completion status, and outcome. Tasks without a linked handle are
deferred with a reason.

---

## Tier 0 — Do Not Touch

Tier 0 items are load-bearing invariants. No task is opened for them; the
appropriate response is protection, not modification.

| Item | Status |
| --- | --- |
| Two axioms + no-privileged-channel rule | Protected — no change |
| Inbox-ACK semantic receipt (forgiving grammar + honest no-op feedback) | Protected — no change |
| Constitution applied to itself (`policy.py` + flex/sticky hysteresis) | Protected — no change |
| Conscience cost-asymmetry design (EAFTP, shuffled judges, dup suppression) | Protected — no change |
| Maxim-can't-resurrect-the-dead safety seam | Protected; duplication fixed (see Tier 2.7 below) |
| Git-shadow noise suppression (per-process, env-only) | Protected — no change |
| Allocator leaning on Taskwarrior | Protected — no change |
| `prepare_say_text` compaction-for-the-ears | Protected — no change |

---

## Tier 1 — Preserve, but Make Visible

| # | Recommendation | Task | Status |
| --- | --- | --- | --- |
| 1.1 | Lead with worldview, not commands | `DOCS-20260624T033219703869Z` | done |
| 1.2 | Surface 0.79:1 test-to-code ratio | `DOCS-20260624T033224027358Z` | done |
| 1.3 | Name the honest-feedback principle | `DOCS-20260624T033229386529Z` | done |
| 1.4 | Document `procs.py` as public API | `DOCS-20260624T033234295547Z` | done |
| 1.5 | Document graceful degradation as designed property | `DOCS-20260624T033239240780Z` | done |

---

## Tier 2 — Sand the Edge

| # | Recommendation | Task | Status |
| --- | --- | --- | --- |
| 2.1 | Formalize `SpeechBackend` seam for non-Mac TTS | `PLATFOR-20260624T033244580527Z` | done |
| 2.2 | Reframe maxim infallibility; write maxim-authoring guide | `DOCS-20260624T033249408087Z`, `DOCS-20260624T033332865679Z` | done |
| 2.3a | Glossary + 400-word mental model page | `DOCS-20260624T033254152847Z` | done |
| 2.3b | Fix Steer/Drive/Drain vs Renew/Steer/Drive mismatch | `RELEASE-20260624T033304579790Z` | done |
| 2.4 | UI invariants doc for frameworkless maintainability | `DOCS-20260624T033300075483Z` | done |
| 2.5 | CHANGELOG note for v0.9.0 skip + tag discipline | `RELEASE-20260624T033308663953Z` | done |
| 2.6 | Stability table — settled vs. moving APIs | `DOCS-20260624T033312891746Z` | done |
| 2.7 | Fix resurrection-invariant duplication (`inbox.py` vs `agentapi.py`) | `REFACTO-20260624T033318285645Z` | done |

---

## Tier 3 — Make Malleable

| # | Recommendation | Task | Status |
| --- | --- | --- | --- |
| 3.1 | Document every `policy.py` constant in CONFIG.md | `DOCS-20260624T033324065260Z` | done |
| 3.2 | Maxim set extensibility + authoring guide | `DOCS-20260624T033332865679Z` (shared with 2.2) | done |
| 3.3 | Wrappers / command rewrite seam documentation | `DOCS-20260624T035104359146Z` | todo |
| 3.4 | Agent personality / driver seam as front-of-house | deferred — driver seam is documented; deeper exposure waits on driver API stability |
| 3.5 | Expose `defaultAgentLifetime` as documented config knob | `CONFIG-20260624T033340586941Z` | done |

---

## Tier 4 — Open Provocations

| # | Provocation | Task | Status |
| --- | --- | --- | --- |
| 4.1 | Embrace conscience's unfalsifiability as a feature rather than soften it? | deferred — framing in Tier 2.2 response (honest-deps reframe) is the chosen path; no further spike opened |
| 4.2 | Semantic-ACK protocol as a standalone published standard? | `DISCOVE-20260624T033346968460Z` | done (spike: too early; protocol depends on spice invariants not yet separate-stable) |
| 4.3 | No-privileged-channel purity under multi-human operators? | `DISCOVE-20260624T033351795407Z` | done (spike: axiom survives; attribution answered by inbox key authorship; conflict answered by ordering) |
| 4.4 | Quest hardware autonomous control as public lead demo? | `DISCOVE-20260624T033356656690Z`, `DOCS-20260624T033448208443Z` | done (decision: not yet; sanitized sidebar added to README) |

---

## Exhaust-problem recommendations (from `03_the_exhaust_problem.md`)

| Recommendation | Task | Status |
| --- | --- | --- |
| Differential reachability study (`spice study reachability`) | `TOOLING-20260624T033403390673Z` | done |
| Ratchet constant `REACHABILITY_TEST_ONLY_LIMIT` in `policy.py` + pre-commit gate | `GOVERNA-20260624T033427350378Z` | review |
| Test-quality maxims for just-in-time redirection | `GOVERNA-20260624T033432741954Z` | todo |
| Exhaust burn-down task board from reachability analysis | `GOVERNA-20260624T033438069529Z` | todo |
| Mutation testing for test effectiveness | `QUALITY-20260624T033408394092Z` | todo |
| Coverage subsumption detector | `QUALITY-20260624T033413059614Z` | todo |
| Assertion-freeness detector | `QUALITY-20260624T033417295896Z` | todo |
| Private-internal coupling detector | `QUALITY-20260624T033422249123Z` | todo |

---

## Surfacing guide (`02_surfacing_the_best.md`)

| Recommendation | Task | Status |
| --- | --- | --- |
| Battle-testing proof point (Quest hardware sidebar) | `DOCS-20260624T033448208443Z` | done |
| Validate critique against current codebase | `META-20260624T033503749433Z` | todo |

---

## Open fixes identified during implementation

| Item | Task | Status |
| --- | --- | --- |
| UI real-time task creation feed (tasks don't appear until refresh) | `UI-20260624T033337577838Z` | done |
| Configure Claude driver to always disable Co-Authored-By trailers | `CLAUDE-20260624T034022561407Z` | todo |
| Ensure latest `claude-sonnet-4-6` when user picks Sonnet model | `CLAUDE-20260624T034933030809Z` | todo |
| Correct UI invariant playback test pointer | `DOCS-20260624T033934936072Z` | todo |
| Correct DESIGN lifetime vocabulary | `DOCS-20260624T034019261028Z` | todo |
| Make `spice.procs` README example self-contained | `DOCS-20260624T034308863716Z` | todo |
| Correct graceful degradation docs for judge and speech backend | `DOCS-20260624T034348171013Z` | todo |
| Complete STABILITY public library seam row | `DOCS-20260624T034819216736Z` | todo |
| Correct CONFIG wrapper common group description | `DOCS-20260624T035104359146Z` | todo |
| Finish default lifetime config tests and docs | `CONFIG-20260624T035316313728Z` | todo |
| Fix reachability scanner so test roots are parsed | `TOOLING-20260624T035757264997Z` | todo |
