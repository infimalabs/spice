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
| `spice/agent/judgeadapter.py:101` | Judge execution has a configured timeout and reports expiry. | Bounded |
| `spice/studies/mutations.py:249` | The mutation command receives its configured timeout. | Bounded |
| `spice/tasks/ops.py:112` | Optional RTK gain measurement has a short timeout and degrades to no gain data. | Bounded |
| `spice/gitprocess.py:41`; wrapper at `spice/tasks/gitsync.py:65` | The shared git runner applies a configurable 120-second default; task `fetch` and `push` retain their tighter 30-second limit plus noninteractive SSH connect timeout. | Bounded, implemented under `GITSYNC-1kCzJQCl`; task-lock coordination remains in `RELIABI-1kCzJljJ` |
| `spice/agent/lifecycle.py:431,701`; `spice/agent/watchdog.py:112` | These `Popen` calls create the supervisor, agent, or watchdog process whose unbounded runtime is the product. Startup publication is separately deadline-bound; shutdown uses process groups. | Lifetime-bound |
| `spice/cli/entry.py:128`; `spice/cli/mounts.py:132`; `spice/resourcelocks.py:501` | Self-exec, mounted commands, and `lock run` children are foreground commands. They inherit terminal/parent cancellation and intentionally run until the selected command exits. | Lifetime-bound; policy documentation and representative cancellation coverage in `RELIABI-1kCzJtSj` |
| `spice/agent/cli.py:196`; `spice/agent/judgeadapter.py:85`; `spice/hooks/refguard.py:37`; `spice/tasks/cli.py:848,1018`; `spice/tasks/taskdoc.py:12,14` | Foreground CLI, hook, and document reads intentionally wait for the invoking pipe or terminal to deliver EOF; the operator or parent process owns cancellation. | Lifetime-bound foreground input |
| `spice/agent/driver.py:1047`; `spice/agent/lifecycle.py:520,928`; `spice/procs.py:137,150,169`; `spice/agent/shadow.py:226` | Appearance lookup, supervisor git probes, process-liveness helpers, and git-shadow reads now carry explicit deadlines and return their documented conservative fallbacks on expiry. | Bounded, implemented under `RELIABI-1kCzJcnr` |
| `spice/config.py:540,555`; `spice/flexstate.py:54`; `spice/paths.py:21,46,62` | General configuration and repository discovery calls still sit on CLI paths where a hung binary can retain the command. | Actionable shared runner work: `RELIABI-1kCzJtSj` |
| `spice/tasks/tw.py:51`; task-local Git callers in `spice/tasks/config.py`, `tw.py`, and `sizing.py` route through `spice/gitprocess.py:41` | Taskwarrior commands have a named 120-second deadline; all task-local Git network and local operations share the configured Git deadline while fetch and push retain their tighter limit. | Bounded, implemented under `RELIABI-1kCzJljJ` |
| `spice/sessions/briefingpressure.py:258-279,295-306,594-610`; `spice/studies/complexity.py:172-179`; command guard at `spice/sessions/cli.py:310-339` | Briefing Git and complexity providers run in dedicated process groups with named 15-second phase deadlines; standalone complexity collection has a 30-second deadline. Briefing and sweep additionally have a configurable 30-second end-to-end render deadline whose typed diagnostic names the action and transcript inputs. | Bounded, implemented under `RELIABI-1kCzJgmj` |
| `spice/serve/audio.py`; `spice/procs.py:241-278` | Configurable and macOS speech run in dedicated process groups with a named 30-second deadline; expiry terminates the group and reports the backend phase and input size. | Bounded, implemented under `RELIABI-1kCzJpcb` |
| `spice/serve/typecheck.py:97` | Serve typechecking is a synchronous foreground child with no declared deadline. | Actionable policy: `RELIABI-1kCzJtSj` |
| `spice/hooks/doctor.py:286,460,738`; `spice/hooks/install.py:119`; `spice/hooks/precommit.py:483,507,605,612,619,622`; `spice/hooks/refguard.py:125` | Hook and gate subprocesses inherit the invoking commit or diagnostic command. They are synchronously interruptible but have no declared per-tool deadline, so CI can remain retained by a stuck child. | Actionable policy: `RELIABI-1kCzJtSj` |
| `spice/release.py:461,777,837`; `spice/serve/demo.py:211,212` | Release, GitHub CLI, packaging, and demo git commands are foreground work with parent cancellation but no per-operation subprocess deadline. | Actionable policy: `RELIABI-1kCzJtSj` |
| `spice/studies/links.py:117`; `spice/studies/mutations.py:78,262`; `spice/studies/reachability.py:352`; `spice/studies/typecheck.py:84,152`; `spice/studies/walk.py:226,254,267,296,314,326` | Study providers and git walks are foreground analysis. Their input size can legitimately vary widely, but a wedged tool is indistinguishable from useful work and has no named cancellation policy. | Actionable policy: `RELIABI-1kCzJtSj` |

## Locks, threads, process waits, watchers, and sockets

| Call sites | Caller impact and current cancellation contract | Classification |
| --- | --- | --- |
| `spice/locking.py:115`; bounded callers `spice/agent/lifecycle.py:833-837`, `spice/tasks/config.py:391-395`, `spice/mail/inbox.py:757-761,853-857`; immediate resource locks `spice/resourcelocks.py:400,430` | Coordination locks use nonblocking acquisition: agent ensure, task bootstrap, and inbox publication retry only until their named deadlines, while resource locks report contention immediately. Errors name the caller action and lock path. | Bounded, implemented under `RELIABI-1kCzJljJ` |
| `spice/resourcelocks.py:393,423` | Resource-lock acquisition uses nonblocking mode and reports contention immediately. | Bounded |
| `spice/agent/lifecycle.py:449-474,802-810` | Supervisor-state and session-id startup polling have monotonic deadlines. | Bounded |
| `spice/agent/lifecycle.py:607` | Lane watch waits 45 seconds at a time and exits through its stop event or child exit. | Bounded |
| `spice/agent/lifecycle.py:666,669,670,768`; `spice/agent/wrap.py:201` | The supervisor, daemon reaper, and wrapped foreground child waits intentionally match the agent or command lifetime. Cleanup joins are capped at one second. | Lifetime-bound |
| `spice/procs.py:122,129,137,150,169` | Windows termination waits and process-liveness subprocesses have explicit deadlines and escalate or return conservative liveness fallbacks. | Bounded |
| `spice/procs.py:244,253-263` | The shared bounded-provider runner starts a dedicated process group, applies the named provider deadline, terminates the complete group on expiry, and caps its final reap before returning a typed phase/input diagnostic. | Bounded |
| `spice/agent/sidechannel.py:105` | Listener accept has a 100 ms socket timeout and observes the server stop event. | Bounded |
| `spice/agent/sidechannel.py:134,147,356,368,374`; `spice/agent/sidechannelnotify.py:60,61`; `spice/agent/wrap.py:566,567` | Server hello reads and replies, notifier connect/send, and agent-run connect/send all inherit explicit socket deadlines. Silent or wedged peers therefore release handler, publication, and launch paths. | Bounded, implemented under `RELIABI-1kCzJcnr` |
| `spice/agent/sidechannel.py:201,232,328,337,346`; `spice/agent/wrap.py:587,592` | Established stream selection, wake-socket reads/writes, and relayed reads intentionally stay open until parent exit, peer close, or server wake/stop. The bounded handshake timeout is cleared before this stream begins. | Lifetime-bound after connection |
| `spice/agent/sidechannel.py:94`; `spice/agent/wrap.py:547` | Side-channel thread joins are capped at one second. | Bounded |
| `spice/mail/watch.py:65-91` | ACK watch is explicitly an operator-facing wait until ACK/NACK or `KeyboardInterrupt`, with periodic resend progress. | Lifetime-bound |
| `spice/serve/app.py:245,252,897`; `spice/serve/websocket.py:300` | HTTP serving and accepted-request reads intentionally run until shutdown, `KeyboardInterrupt`, peer completion, or the LiveBus read timeout; watcher cleanup join is bounded. | Lifetime-bound server/read path with bounded watcher cleanup |
| `spice/serve/filewatch.py:38-45,139`; `spice/serve/livebus.py:514-565,907-915` | Serve file-watch and lane-watcher activation, initial-payload release, and payload futures have named deadlines. Expiry releases gates and emits a path- or lane-qualified operator/bus error, allowing the subscription worker to continue with later batches. | Bounded, implemented under `RELIABI-1kCzJpcb` |
| `spice/serve/livebus.py:663` | Pending detached reads are joined through `concurrent.futures.wait`; the only production caller supplies the teardown timeout. | Bounded |
| `spice/serve/livebus.py:261,265,273,492,806,847,878` | Subscribe, follow-up, and metric workers block on queues until close sends sentinels; every cleanup join has the watcher join timeout. | Lifetime-bound workers with bounded cleanup |
| `spice/serve/livebus.py:1077`; bounded native loops at `spice/serve/livebus.py:1092-1106,1162-1226` | The kqueue and watchfiles waits use bounded native wakeups and observe the subscription stop event; activation is signaled after the native watch is armed, and callers impose the named activation deadline above. | Bounded |
| `spice/serve/websocket.py:46-120`; timeout and read at `spice/serve/livebus.py:230-234` | WebSocket reads are blocking by design after a connection is accepted, but LiveBus applies a read timeout so silent peers are reaped. | Bounded |
| `spice/serve/livebus.py:189`; acquisition and write at `spice/serve/livebus.py:308-333` | The per-session send mutex is held across the WebSocket write. One stalled peer write can therefore retain every later outbound frame for that session; telemetry records lock wait and hold duration but does not impose a deadline. | Actionable diagnosis: `LIVEBUS-1kCzHL0m` |
| `spice/agent/sidechannel.py:58,61`; `spice/agent/sidechannelnotify.py:25`; `spice/serve/app.py:119`; `spice/serve/livebus.py:174,191-194,225`; `spice/serve/messages.py:146`; `spice/serve/submissions.py:88`; `spice/serve/team/store.py:122`; `spice/serve/websocket.py:45` | In-process mutexes protect short critical sections. Side-channel notification I/O is outside its notice mutex; these LiveBus telemetry, background-dirty, subscription-state, and read-chain sections perform no network or child-process waits while held. | Lifetime invariant documented by code shape; intentionally unbounded mutex acquisition |
| `spice/agent/maxims.py:1002` | Both parallel maxim judges have bounded command attempts; collecting their futures can wait only for those configured attempt bounds. | Bounded |

## Network and database calls

| Call sites | Caller impact and current cancellation contract | Classification |
| --- | --- | --- |
| `spice/release.py:802` | PyPI lookup passes a 20-second URL timeout. | Bounded |
| `spice/tasks/gitsync.py:65` | Git fetch/push use a 30-second subprocess timeout, disable terminal prompts, and configure a five-second SSH connect timeout. | Bounded for network operations |
| `spice/agent/sidechannel.py`, `sidechannelnotify.py`, `wrap.py` | Unix-domain connect and hello operations have explicit deadlines; established streams intentionally retain stop/parent/peer lifetime cancellation. | Mixed: Bounded handshake and lifetime-bound stream |
| `spice/agent/driver.py:367`; `spice/agent/maximmetrics.py:141,164,181,228`; `spice/mail/ackstate.py:104,131`; `spice/serve/team/store.py:136,168`; `spice/studies/subsumption.py:50`; team/directive/filter/metric store `connect()` callers | SQLite connect and lock waits use Python's finite default timeout; the team store also sets an explicit busy timeout. They can delay callers but are not indefinite. | Bounded |

## Result

The intentionally open-ended surfaces all correspond to an explicit lifetime:
foreground child command, supervised agent, server, ACK watcher, socket stream,
or sentinel-driven worker. They already have an external cancellation owner and
should not receive arbitrary wall-clock caps.

Every remaining surface without such an owner is assigned to one of two concrete tasks. Agent side-channel handshakes and helper probes, session rehydration providers, and task backend coordination were bounded under `RELIABI-1kCzJcnr`, `RELIABI-1kCzJgmj`, and `RELIABI-1kCzJljJ`:

- `RELIABI-1kCzJpcb` — serve watcher activation and speech workers;
- `RELIABI-1kCzJtSj` — shared deadlines for synchronous tool runners.

Those remaining tasks require deterministic stalled-process and
watcher-activation coverage for the serve and synchronous-tool paths named by
this audit.

`tests/test_waitsurfaceaudit.py` AST-scans the production tree and requires every
direct subprocess, process wait, thread join, event/future wait, queue wait,
mutex factory, watcher, socket operation, network read, and database connection
surface it recognizes to retain a file-and-line entry in this matrix. A newly
introduced unclassified surface therefore fails the test with the exact missing
reference instead of silently drifting beyond the audit.
