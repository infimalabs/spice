# Unbounded Wait Surface Audit

Status: implementation audit, 2026-07-12. Deliverable for
`RELIABI-1kCzDltf`. Line numbers refer to the integrated tree at the audit
commit. Every line number in a call-site cell is a distinct call.

## Classification

- **Bounded**: the call has a timeout, nonblocking mode, polling deadline, or
  stop event that is part of its current contract.
- **Lifetime-bound**: blocking is intentional because the call *is* the
  foreground command, supervised process lifetime, server lifetime, or worker
  lifetime. Cancellation comes from parent exit, `KeyboardInterrupt`, a stop
  event, a sentinel, socket closure, or process-group termination.
- **Actionable**: a stalled dependency can retain a control-plane command,
  daemon thread, lock, subscription queue, or request worker without a named
  deadline. The row names its acceptance-bearing follow-up task.

## Subprocess inventory

Only two direct `subprocess.run` sites currently pass `timeout=` syntactically:
the judge adapter and one mutation runner. Task git sync adds its timeout
dynamically for `fetch` and `push`. All direct production `run` and `Popen`
sites are inventoried below.

| Call sites | Caller impact and current cancellation contract | Classification |
| --- | --- | --- |
| `spice/agent/judgeadapter.py:99` | Judge execution has a configured timeout and reports expiry. | Bounded |
| `spice/studies/mutations.py:249` | The mutation command receives its configured timeout. | Bounded |
| `spice/tasks/ops.py:112` | Optional RTK gain measurement has a short timeout and degrades to no gain data. | Bounded |
| `spice/tasks/gitsync.py:65` | `fetch` and `push` receive a 30-second subprocess timeout plus noninteractive SSH connect timeout; local git operations do not. | Actionable: `RELIABI-1kCzJljJ` |
| `spice/agent/lifecycle.py:431,690`; `spice/agent/watchdog.py:112` | These `Popen` calls create the supervisor, agent, or watchdog process whose unbounded runtime is the product. Startup publication is separately deadline-bound; shutdown uses process groups. | Lifetime-bound |
| `spice/cli/entry.py:128`; `spice/cli/mounts.py:125`; `spice/resourcelocks.py:494` | Self-exec, mounted commands, and `lock run` children are foreground commands. They inherit terminal/parent cancellation and intentionally run until the selected command exits. | Lifetime-bound; policy documentation and representative cancellation coverage in `RELIABI-1kCzJtSj` |
| `spice/agent/driver.py:1038`; `spice/agent/lifecycle.py:512,919`; `spice/agent/procs.py:129,141,159`; `spice/agent/shadow.py:217` | Appearance lookup, supervisor git probes, process-liveness helpers, and git-shadow reads can stall activation, a watcher, or liveness decisions without a deadline. | Actionable: `RELIABI-1kCzJcnr` |
| `spice/config.py:360,375`; `spice/flexstate.py:54`; `spice/paths.py:21,46,62`; `spice/tasks/config.py:345,357`; `spice/tasks/sizing.py:258`; `spice/tasks/tw.py:46,103`; `spice/worktrees.py:54,100,123` | Configuration, repository discovery, Taskwarrior, task sizing, and worktree calls sit on allocator and task CLI paths. A hung binary can retain the entire command and task claim. | Actionable: `RELIABI-1kCzJljJ` and shared runner work `RELIABI-1kCzJtSj` |
| `spice/sessions/briefingpressure.py:286,584,594`; `spice/studies/complexity.py:165` | Briefing pressure reads git baselines and invokes complexity collection. A stall can make rehydration silent indefinitely; this was reproduced twice during this audit with `spice session briefing`. | Actionable: `RELIABI-1kCzJgmj` |
| `spice/serve/audio.py:85,206`; `spice/serve/typecheck.py:97` | Configurable speech, macOS `say`, and serve typechecking occupy request or worker capacity until their child exits. | Actionable: `RELIABI-1kCzJpcb`; shared runner policy in `RELIABI-1kCzJtSj` |
| `spice/hooks/doctor.py:286,460,738`; `spice/hooks/install.py:119`; `spice/hooks/precommit.py:474,498,596,603,610,613`; `spice/hooks/refguard.py:125` | Hook and gate subprocesses inherit the invoking commit or diagnostic command. They are synchronously interruptible but have no declared per-tool deadline, so CI can remain retained by a stuck child. | Actionable policy: `RELIABI-1kCzJtSj` |
| `spice/release.py:461,777,837`; `spice/serve/demo.py:211,212` | Release, GitHub CLI, packaging, and demo git commands are foreground work with parent cancellation but no per-operation subprocess deadline. | Actionable policy: `RELIABI-1kCzJtSj` |
| `spice/studies/links.py:117`; `spice/studies/mutations.py:78,262`; `spice/studies/reachability.py:352`; `spice/studies/typecheck.py:83,151`; `spice/studies/walk.py:221,249,262,291,309,321` | Study providers and git walks are foreground analysis. Their input size can legitimately vary widely, but a wedged tool is indistinguishable from useful work and has no named cancellation policy. | Actionable policy: `RELIABI-1kCzJtSj` |

## Locks, threads, process waits, watchers, and sockets

| Call sites | Caller impact and current cancellation contract | Classification |
| --- | --- | --- |
| `spice/locking.py:78`; callers `spice/agent/lifecycle.py:825`, `spice/tasks/config.py:399`, `spice/mail/inbox.py:756,848` | POSIX blocking `flock` is explicitly documented as indefinite. Agent ensure, task bootstrap, and inbox publication can therefore retain CLI/control-plane work forever while a live holder is wedged. Inbox notification currently occurs inside the publication lock. | Actionable: `RELIABI-1kCzJljJ`; side-channel portion in `RELIABI-1kCzJcnr` |
| `spice/resourcelocks.py:393,423` | Resource-lock acquisition uses nonblocking mode and reports contention immediately. | Bounded |
| `spice/agent/lifecycle.py:449-474,802-810` | Supervisor-state and session-id startup polling have monotonic deadlines. | Bounded |
| `spice/agent/lifecycle.py:596` | Lane watch waits 45 seconds at a time and exits through its stop event or child exit. | Bounded |
| `spice/agent/lifecycle.py:655,757`; joins at `658,659` | The supervisor wait and daemon reaper intentionally match the agent process lifetime. Cleanup joins are capped at one second. | Lifetime-bound |
| `spice/agent/procs.py:103-123` | Windows termination waits have explicit deadlines and escalate to forceful tree termination or kill. | Bounded |
| `spice/agent/sidechannel.py:99` | Listener accept has a 100 ms socket timeout and observes the server stop event. | Bounded |
| `spice/agent/sidechannel.py:339,351`; `spice/agent/sidechannelnotify.py:54` | A client can connect without completing the newline hello, or a notifier connect/send can stall. Handler threads are daemonized but have no read deadline; publication can call notification while holding its file lock. | Actionable: `RELIABI-1kCzJcnr` |
| `spice/agent/sidechannel.py:186`; `spice/agent/wrap.py:560,576-582` | Established stream selection intentionally stays open until parent exit, peer close, or server wake/stop. The stream has cancellation, but initial connect has no deadline. | Lifetime-bound after connection; actionable connect deadline in `RELIABI-1kCzJcnr` |
| `spice/agent/sidechannel.py:88`; `spice/agent/wrap.py:542` | Side-channel thread joins are capped at one second. | Bounded |
| `spice/mail/watch.py:65-91` | ACK watch is explicitly an operator-facing wait until ACK/NACK or `KeyboardInterrupt`, with periodic resend progress. | Lifetime-bound |
| `spice/serve/app.py:244,251` | HTTP serving intentionally runs until shutdown or `KeyboardInterrupt`; watcher cleanup join is bounded. | Lifetime-bound |
| `spice/serve/filewatch.py:37,39,133` | Serve startup waits indefinitely for watcher activation, and the error join is unbounded. The active native watch itself observes `stop_event` and emits readiness every one second. | Actionable: `RELIABI-1kCzJpcb` |
| `spice/serve/livebus.py:399,702` and payload `Future.result()` at `410` | Watcher activation, initial-payload release, and payload computation have no deadline. The single subscribe-completion worker can head-of-line block later subscribe batches. | Actionable: `RELIABI-1kCzJpcb` |
| `spice/serve/livebus.py:377,601,642`; joins at `201,205,213,673` | Subscribe, follow-up, and metric workers block on queues until close sends sentinels; cleanup joins are bounded. | Lifetime-bound |
| `spice/serve/livebus.py:863,891-1007` | Native kqueue/watchfiles loops observe subscription stop events; activation is signaled or a startup error is recorded. | Bounded internally; callers still need the activation deadlines above |
| `spice/serve/websocket.py:46-120`; `spice/serve/livebus.py:179` | WebSocket reads are blocking by design after a connection is accepted, but LiveBus applies a read timeout so silent peers are reaped. | Bounded |
| `spice/serve/app.py:118`; `spice/serve/livebus.py:144,172`; `spice/serve/submissions.py:88`; `spice/serve/team/store.py:122` | In-process mutexes protect short, non-awaiting critical sections. No critical section performs network or child-process waits. | Lifetime invariant documented by code shape; intentionally unbounded mutex acquisition |

## Network and database calls

| Call sites | Caller impact and current cancellation contract | Classification |
| --- | --- | --- |
| `spice/release.py:802` | PyPI lookup passes a 20-second URL timeout. | Bounded |
| `spice/tasks/gitsync.py:65` | Git fetch/push use a 30-second subprocess timeout, disable terminal prompts, and configure a five-second SSH connect timeout. | Bounded for network operations |
| `spice/agent/sidechannel.py`, `sidechannelnotify.py`, `wrap.py` | Unix-domain sockets are local but can still stall at connect or handshake; established streams have stop/parent cancellation. | Mixed; actionable under `RELIABI-1kCzJcnr` |
| `spice/agent/driver.py:366`; `spice/agent/maximmetrics.py:141,164,181,228`; `spice/mail/ackstate.py:104,131`; `spice/serve/team/store.py:136,168`; team/directive/filter/metric store `connect()` callers | SQLite connect and lock waits use Python's finite default timeout; the team store also sets an explicit busy timeout. They can delay callers but are not indefinite. | Bounded |

## Result

The intentionally open-ended surfaces all correspond to an explicit lifetime:
foreground child command, supervised agent, server, ACK watcher, socket stream,
or sentinel-driven worker. They already have an external cancellation owner and
should not receive arbitrary wall-clock caps.

Every surface without such an owner is assigned to one of five concrete tasks:

- `RELIABI-1kCzJcnr` — agent side-channel handshakes and helper probes;
- `RELIABI-1kCzJgmj` — session briefing and sweep completion;
- `RELIABI-1kCzJljJ` — task backend commands and coordination locks;
- `RELIABI-1kCzJpcb` — serve watcher activation and speech workers;
- `RELIABI-1kCzJtSj` — shared deadlines for synchronous tool runners.

Those tasks require deterministic stalled-peer, stalled-process, held-lock, and
watcher-activation coverage, including the highest-risk agent, session, task,
and serve paths named by this audit.
