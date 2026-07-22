# Unbounded Wait Surface Audit

Status: implemented contract, 2026-07-15. Deliverable for
`RELIABI-1kCzDltf`. Call sites are named by stable `path::qualified.function#call`
anchors -- file plus the enclosing function and the blocking call -- so the
matrix survives line drift: only a genuinely new blocking surface, not an
unrelated edit that shifts a call, can fall out of sync.

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

The judge adapter is the only direct `subprocess.run` site that passes
`timeout=` syntactically: its deadline is legally disableable, which the
bounded engine cannot express. Task git sync adds its timeout dynamically for
`fetch` and `push`. All direct production `run` and `Popen` sites are
inventoried below.

| Call sites | Caller impact and current cancellation contract | Classification |
| --- | --- | --- |
| `spice/agent/judgeadapter.py::main#run` | Judge execution has a configured timeout and reports expiry. | Bounded |
| `spice/studies/mutations.py::_run_pytest` | The mutation command receives its configured timeout. | Bounded |
| Optional RTK gain measurement at `spice/tasks/ops.py::rtk_usage_nudge` rides the named probe tool policy | The probe deadline bounds the measurement and the caller degrades to no gain data. | Bounded, implemented under `PROCESS-1kF6VWvS` |
| `spice/process/git.py::run_git_command`; wrapper at `spice/tasks/git/plumbing.py::run` | The shared git runner applies a configurable 120-second default; task `fetch` and `push` retain their tighter 30-second limit plus noninteractive SSH connect timeout. | Bounded, implemented under `GITSYNC-1kCzJQCl` and `RELIABI-1kCzJljJ` |
| `spice/agent/lifecycle.py::spawn_agent_supervisor#Popen; spice/agent/lifecycle.py::spawn_agent#Popen`; `spice/agent/watchdog.py::spawn_supervised_agent#Popen` | These `Popen` calls create the supervisor, agent, or watchdog process whose unbounded runtime is the product. Startup publication is separately deadline-bound; shutdown uses process groups. | Lifetime-bound |
| `spice/process/tool.py::run_parent_lifetime_command#run`; callers in `spice/cli/entry.py`, `spice/cli/mounts.py`, and `spice/resourcelocks.py` | Self-exec, mounted commands, and `lock run` children use the explicit parent-lifetime runner. They inherit terminal/parent cancellation and intentionally run until the selected command exits. | Lifetime-bound foreground child |
| `spice/agent/cli.py::_reply_to_steering#read`; `spice/agent/judgeadapter.py::main#read`; `spice/hooks/refguard.py::handle_reference_transaction#read`; `spice/tasks/cli.py::_note#read; spice/tasks/cli.py::_handle_add#read`; `spice/tasks/taskdoc.py::read_document#read` | Foreground CLI, hook, and document reads intentionally wait for the invoking pipe or terminal to deliver EOF; the operator or parent process owns cancellation. | Lifetime-bound foreground input |
| `spice/process/groups.py::_force_windows_process_tree#run; spice/process/groups.py::_posix_process_group_has_live_member#run; spice/process/groups.py::_posix_pid_has_live_state#run` | Process-liveness helpers carry explicit deadlines and return conservative liveness fallbacks on expiry. Appearance lookup (`spice/agent/driver.py::operator_color_scheme`) rides the named probe tool policy; supervisor git probes, git-shadow reads, and tracked-path checks ride the git door probe (`spice/process/git.py::git_probe`) and keep their documented degrade answers. | Bounded, implemented under `RELIABI-1kCzJcnr` and `PROCESS-1kF6VWvS` |
| Configuration, flex-state, and repository-discovery Git calls route through `spice/process/git.py::run_git_command` | General configuration and repository discovery calls use the configured Git deadline and preserve full command identity on timeout. | Bounded, implemented under `RELIABI-1kCzJtSj` |
| `spice/tasks/tw.py::run#run`; task-local Git callers in `spice/tasks/config.py`, `tw.py`, and `sizing.py` route through `spice/process/git.py::run_git_command` | Taskwarrior commands have a named 120-second deadline; all task-local Git network and local operations share the configured Git deadline while fetch and push retain their tighter limit. | Bounded, implemented under `RELIABI-1kCzJljJ` |
| `spice/sessions/briefingpressure.py::_scan_dirty_complexity_pressure; spice/sessions/briefingpressure.py::_materialize_complexity_baseline_paths; spice/sessions/briefingpressure.py::_git_read; spice/sessions/briefingpressure.py::_briefing_repo_root_from_cwd`; `spice/studies/complexity.py::collect_complexity_records`; command guard at `spice/sessions/cli.py::handle_session; spice/sessions/cli.py::handle_session.briefing_output; spice/sessions/cli.py::handle_session.sweep_output` | Briefing Git and complexity providers run in dedicated process groups with named 15-second phase deadlines; standalone complexity collection has a 30-second deadline. Briefing and sweep additionally have a configurable 30-second end-to-end render deadline whose typed diagnostic names the action and transcript inputs. | Bounded, implemented under `RELIABI-1kCzJgmj` |
| `spice/config.py::configured_say_timeout` and `spice/serve/audio.py` use `spice/process/groups.py`; serve typecheck routes through `spice/process/tool.py` | External and macOS speech use layered `say.timeout_seconds` with a 300-second default and process-group cleanup, while serve typecheck uses the named typecheck policy. | Bounded under `AUDIO-1kCzJRGj`, `AUDIO-1kD0BCT4`, and `RELIABI-1kCzJtSj` |
| Hook, extension, Ruff, and typecheck callers route through `spice/process/tool.py`; Git hook operations route through `spice/process/git.py::run_git_command` | Gate subprocesses use named hook, extension, or typecheck deadlines and whole-process-group cleanup. | Bounded, implemented under `RELIABI-1kCzJtSj` |
| Release and demo callers route through `spice/process/tool.py` or `spice/process/git.py::run_git_command` | Release, GitHub CLI, packaging, and demo Git commands use named release or Git deadlines with command identity diagnostics. | Bounded, implemented under `RELIABI-1kCzJtSj` |
| Study providers route through `spice/process/tool.py`; study Git walks route through `spice/process/git.py::run_git_command` | Typecheck, mutation, reachability, and repository-walk providers use named study/typecheck or Git deadlines with command identity and process-group cleanup. | Bounded, implemented under `RELIABI-1kCzJtSj` |

## Locks, threads, process waits, watchers, and sockets

| Call sites | Caller impact and current cancellation contract | Classification |
| --- | --- | --- |
| `spice/locking.py::_lock_fd_posix#flock`; bounded callers `spice/agent/lifecycle.py::agent_ensure_lock`, `spice/tasks/config.py::_bootstrap_lock`, and `spice/mail/inbox.py::write_inbox_item`; immediate resource locks `spice/resourcelocks.py::_metadata_lock; spice/resourcelocks.py::_lock_state` | Coordination locks use nonblocking acquisition: agent ensure, task bootstrap, and inbox publication retry only until their named deadlines, while resource locks report contention immediately. Errors name the caller action and lock path. | Bounded, implemented under `RELIABI-1kCzJljJ` |
| `spice/resourcelocks.py::_metadata_lock; spice/resourcelocks.py::_lock_state` | Resource-lock acquisition uses nonblocking mode and reports contention immediately. | Bounded |
| `spice/agent/lifecycle.py::require_supervisor_started; spice/agent/lifecycle.py::head_text; spice/agent/lifecycle.py::started_agent_thread_id` | Supervisor-state and session-id startup polling have monotonic deadlines. | Bounded |
| `spice/agent/lifecycle.py::_watch_supervised_lane#wait` | Lane watch waits 20 seconds at a time and exits through its stop event or child exit. | Bounded |
| `spice/agent/lifecycle.py::_watch_agent_startup#wait` | The first-activity condition wait is capped at 120 seconds and is notified by either driver-defined assistant activity or child completion; expiry terminates the complete agent process group and persists a `startup-stalled` diagnostic. | Bounded, implemented under `AGENT-1kG0VbFD` |
| `spice/agent/lifecycle.py::run_agent_supervisor#wait; spice/agent/lifecycle.py::run_agent_supervisor#join; spice/agent/lifecycle.py::reap_process_when_done.reap#wait`; `spice/agent/wrap.py::run_agent_command#wait` | The supervisor, daemon reaper, and wrapped foreground child waits intentionally match the agent or command lifetime. Stdout and lane-watch cleanup joins are capped at one second. The non-daemon startup watcher is joined for the process-group helper's named 12.1-second worst-case cleanup bound plus a three-second state-persistence allowance, so terminal outcome recording stays after descendant cleanup and durable startup state. | Lifetime-bound |
| `spice/process/groups.py::_terminate_windows_process_tree#wait; spice/process/groups.py::_force_windows_process_tree#run; spice/process/groups.py::_posix_process_group_has_live_member#run; spice/process/groups.py::_posix_pid_has_live_state#run` | Windows termination waits and process-liveness subprocesses have explicit deadlines and escalate or return conservative liveness fallbacks. | Bounded |
| `spice/process/groups.py::run_bounded_process_group#Popen` | The shared bounded-provider runner starts a dedicated process group, applies the named provider deadline, terminates the complete group on expiry, and caps its final reap before returning a typed phase/input diagnostic. | Bounded |
| `spice/sessions/deadline.py::run_with_rehydration_deadline#join` | Platforms without main-thread `setitimer` run transcript resolution and rendering in a daemon worker and join only for the named end-to-end budget before returning the same typed deadline diagnostic. | Bounded |
| `spice/agent/sidechannel.py::AgentSideChannelServer._serve#accept` | Listener accept has a 100 ms socket timeout and observes the server stop event. | Bounded |
| `spice/agent/sidechannel.py::AgentSideChannelServer._handle_connection#sendall; spice/agent/sidechannel.py::_read_line#recv; spice/agent/sidechannel.py::_echo_connection#recv; spice/agent/sidechannel.py::_echo_connection#sendall`; `spice/agent/sidechannelnotify.py::notify_agent_side_channel#connect; spice/agent/sidechannelnotify.py::notify_agent_side_channel#sendall`; `spice/agent/runwatch.py::watch_agent_side_channel#connect; spice/agent/runwatch.py::watch_agent_side_channel#sendall` | Server hello reads and replies, notifier connect/send, and agent-run connect/send all inherit explicit socket deadlines. Silent or wedged peers therefore release handler, publication, and launch paths. | Bounded, implemented under `RELIABI-1kCzJcnr` |
| `spice/agent/sidechannel.py::AgentSideChannelServer._stream_payloads#select; spice/agent/sidechannel.py::AgentSideChannelServer._wake_streams#sendall; spice/agent/sidechannel.py::_SocketTextWriter.write#sendall; spice/agent/sidechannel.py::_connection_has_closed#recv; spice/agent/sidechannel.py::_drain_wakeup#recv`; `spice/agent/runwatch.py::watch_agent_side_channel#select; spice/agent/runwatch.py::watch_agent_side_channel#recv` | Established stream selection, wake-socket reads/writes, and relayed reads intentionally stay open until parent exit, peer close, or server wake/stop. The bounded handshake timeout is cleared before this stream begins. | Lifetime-bound after connection |
| `spice/agent/sidechannel.py::AgentSideChannelServer.stop#join`; `spice/agent/runwatch.py::join_agent_side_channel_watch#join` | Side-channel thread joins are capped at one second. | Bounded |
| `spice/serve/app.py::run_serve#serve_forever; spice/serve/app.py::run_serve#join; spice/serve/app.py::_ServeHandler._read_payload#read`; `spice/serve/websocket.py::_read_exact#read` | HTTP serving and accepted-request reads intentionally run until shutdown, `KeyboardInterrupt`, peer completion, or the LiveBus read timeout; watcher cleanup join is bounded. | Lifetime-bound server/read path with bounded watcher cleanup |
| `spice/serve/filewatch.py::start_exit_file_watch#wait; spice/serve/filewatch.py::start_exit_file_watch#join; spice/serve/filewatch.py::_watch_target_changes_kqueue#wait`; `spice/serve/livebus.py::LiveBusSession._complete_lanes_subscribe#wait; spice/serve/livebus.py::LiveBusSession._initial_payload_result#result; spice/serve/livebus.py::LiveBusSession._stop_subscription#join; spice/serve/livebus.py::LiveBusSession._run_watch_loop#wait` | Serve file-watch and lane-watcher activation, initial-payload release, and payload futures have named deadlines. Expiry releases gates and emits a path- or lane-qualified operator/bus error, allowing the subscription worker to continue with later batches. | Bounded, implemented under `RELIABI-1kCzJpcb` |
| `spice/serve/livebus.py::LiveBusSession._await_pending_reads#wait` | Pending detached reads are joined through `concurrent.futures.wait`; the only production caller supplies the teardown timeout. | Bounded |
| `spice/serve/livebus.py::LiveBusSession._teardown#join; spice/serve/livebus.py::LiveBusSession._subscribe_completion_loop#get; spice/serve/livebus.py::LiveBusSession._send_followup_loop#get; spice/serve/livebus.py::LiveBusSession._metrics_loop#get` | Subscribe, follow-up, and metric workers block on queues until close sends sentinels; every cleanup join has the watcher join timeout. | Lifetime-bound workers with bounded cleanup |
| `spice/serve/livebus.py::_wait_for_change#wait`; bounded native loops in `spice/serve/livebus.py::_wait_for_change_kqueue` and `spice/serve/livebus.py::_wait_for_change_watchfiles` | The kqueue and watchfiles waits use bounded native wakeups and observe the subscription stop event; activation is signaled after the native watch is armed, and callers impose the named activation deadline above. | Bounded |
| `spice/serve/websocket.py::WebSocketConnection.read_json; spice/serve/websocket.py::WebSocketConnection.read_text; spice/serve/websocket.py::WebSocketConnection.encode_text_frame; spice/serve/websocket.py::WebSocketConnection.send_frame; spice/serve/websocket.py::WebSocketConnection.ping; spice/serve/websocket.py::WebSocketConnection.set_read_timeout; spice/serve/websocket.py::WebSocketConnection.close; spice/serve/websocket.py::WebSocketConnection._read_frame`; timeout and read at `spice/serve/livebus.py::LiveBusSession.run` | WebSocket reads are blocking by design after a connection is accepted, but LiveBus applies a read timeout so silent peers are reaped. | Bounded |
| `spice/serve/livebus.py::LiveBusSession.__init__#Lock`; acquisition and write at `spice/serve/livebus.py::LiveBusSession._send` | The per-session send mutex is held across the WebSocket write; the frame is encoded before the lock is taken, so the critical section is the bare socket write. One stalled peer write can still retain later outbound frames for that session; telemetry records lock wait and hold duration but does not impose a deadline. | Lifetime invariant, diagnosed under `LIVEBUS-1kCzHL0m`: measured total lock waits stayed at or below 0.02 ms and holds at or below 1.65 ms (`docs/design/experimental/serve-livebus-latency-diagnosis.md`), rejecting the send lock as a bottleneck; intentionally unbounded mutex acquisition |
| `spice/serve/agentapi.py::<module>#RLock`; acquisition in `spice/serve/agentapi.py::ensure_agent_for_available_work`, `spice/serve/agentapi.py::forget_available_work_observation`, and `spice/serve/agentapi.py::forget_available_work_observations` | The process-wide reentrant mutex serializes each stopped Drain lane's capacity snapshot, ready-age cache mutation, guarded Taskwarrior claim, and agent startup, so the next concurrent inventory refresh observes the newly started lane before deciding whether backlog permits another expansion. Cache cleanup deliberately re-enters the mutex from the decision path, requiring reentrancy. Contending inventory refreshes and cache cleanup can wait for the whole decision, but Taskwarrior and Git commands have named deadlines, SQLite writes inherit finite busy timeouts, agent-ensure lock acquisition is bounded, and supervisor startup publication has a named deadline. | Bounded by the task-backend, Git, SQLite, agent-ensure, and supervisor-startup deadlines |
| `spice/agent/sidechannel.py::AgentSideChannelServer.__init__#Lock`; `spice/agent/sidechannelnotify.py::<module>#Lock`; `spice/serve/app.py::ServeState.__init__#Lock`; `spice/serve/livebus.py::<module>#field; spice/serve/livebus.py::LiveBusSession.__init__#Lock`; `spice/serve/messages.py::<module>#field`; `spice/serve/submissions.py::SubmissionLifecycleTracker.__init__#Lock`; `spice/serve/team/store.py::<module>#Lock`; `spice/serve/websocket.py::<module>#field` | In-process mutexes protect short critical sections. Side-channel notification I/O is outside its notice mutex; these LiveBus telemetry, background-dirty, subscription-state, and read-chain sections perform no network or child-process waits while held. | Lifetime invariant documented by code shape; intentionally unbounded mutex acquisition |
| `spice/agent/maxims.py::evaluate_maxim_any_violation#result` | Both parallel maxim judges have bounded command attempts; collecting their futures can wait only for those configured attempt bounds. | Bounded |

## Network and database calls

| Call sites | Caller impact and current cancellation contract | Classification |
| --- | --- | --- |
| `spice/release.py::wait_for_pypi#urlopen` | PyPI lookup passes a 20-second URL timeout. | Bounded |
| `spice/tasks/git/plumbing.py::run` | Git fetch/push use a 30-second subprocess timeout, disable terminal prompts, and configure a five-second SSH connect timeout. | Bounded for network operations |
| `spice/agent/sidechannel.py`, `sidechannelnotify.py`, `wrap.py` | Unix-domain connect and hello operations have explicit deadlines; established streams intentionally retain stop/parent/peer lifetime cancellation. | Mixed: Bounded handshake and lifetime-bound stream |
| `spice/tasks/opslog.py::_connect#connect` | Read-only URI connect (`mode=ro`) to the local TaskChampion operations database using Python's finite default timeout; every caller (`task_version`, `claim_baseline_id`, `contract_mutations_since`) runs indexed point queries and closes the connection in a `finally` block. | Bounded |
| `spice/sqliteconnection.py::sqlite_connection#connect`; `spice/serve/team/store.py::ServeTeamStore.connect#connect`; team/directive/filter/metric store `connect()` callers | SQLite connect and lock waits use Python's finite default timeout; short-lived stores (ACK state, maxim metrics, subsumption coverage, driver transcript lookup, team schema init) route through the shared deterministic-close owner, and the team store also sets an explicit busy timeout. They can delay callers but are not indefinite. | Bounded |

## Result

The intentionally open-ended surfaces all correspond to an explicit lifetime:
foreground child command, supervised agent, server, ACK watcher, socket stream,
or sentinel-driven worker. They already have an external cancellation owner and
should not receive arbitrary wall-clock caps.

Every surface that lacked such an owner is now bounded. Agent side-channel
handshakes and helper probes, session rehydration providers, task backend
coordination, and synchronous tool runners were bounded under
`RELIABI-1kCzJcnr`, `RELIABI-1kCzJgmj`, `RELIABI-1kCzJljJ`, and
`RELIABI-1kCzJtSj`; serve watcher activation, initial-payload release, and
payload futures were bounded under `RELIABI-1kCzJpcb` with deterministic
stalled-process and watcher-activation coverage. The LiveBus per-session send
mutex was diagnosed rather than capped: `LIVEBUS-1kCzHL0m` measured the lock
as effectively uncontended, so it remains an intentionally unbounded
in-process mutex alongside the other short-critical-section locks above.

`tests/test_waitsurfaceaudit.py` AST-scans the production tree and requires every
direct subprocess, process wait, thread join, event/future wait, queue wait,
mutex factory, watcher, socket operation, network read, and database connection
surface it recognizes to retain a `path::function#call` anchor in this matrix.
Because the anchor is the enclosing function and the call rather than a line
number, an unrelated edit that shifts a call never breaks the test; a newly
introduced unclassified surface still fails it with the exact missing anchor.
