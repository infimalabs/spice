# Surface mechanics — Constants

[Surface mechanics](../mechanics.md) · [Spice product model](../README.md)
## Constants that are contracts

| Constant | Value | Why it is load-bearing |
| --- | --- | --- |
| heartbeat / liveness | 15s / 35s | liveness must exceed 2 heartbeats |
| reconnect backoff | 500ms to 10s | reset on open |
| bus read timeout (server) | 45s | > client heartbeat interval |
| background coalesce | 250ms | one aggregate frame, not one per line |
| watcher activation / initial payload | 5s / 15s | deadlines, not retries |
| payload workers | 8 | per-session fan-out bound |
| initial / paged / retained messages | 25 / 50 / 400 | retained grows with hydration |
| accent slots | 6 | every index reduced mod 6 at its source |
| grid tracks | 12 | legal base spans 2,3,4,6,12 |
| minLane | 17rem | a floor, **never** a divisor |
| gap / module line height | 0.75rem / 1.25rem | M = round(lines, lineHeight) |
| tall threshold / wide factor | 230px / twice the width | measured height only, never class |
| freeze depth | 2 | burial depth latch |
| plane anchor K | 4096 | unchanged cards get no write |
| card tween | 340ms | plane and cards never share it |
| settle quiet / resize debounce | 500ms / 200ms | reveal bypasses the debounce |
| min render width | 24px | below it, defer and touch nothing |
| width epsilon / backfill epsilon | 0.5px / 2px | below these a difference is measurement noise |
| near-top threshold | 80px | jump-and-compensate boundary |
| fuse gutter / drag threshold | 0.2 / 6px | outer fifths fuse; under the threshold it is a click |
| submissions kept | 50 client / 200 server | bounded so a long session cannot grow without limit |
| latency samples | 25 | ring buffer |
| lane / global transient | 2.5s / 3s | global outlives lane, so it is never overwritten first |
| activity decay | 60s / 300s | active, then active-ish, then inactive |
| time rule bucket | 1h | absolute label, never relative |
| attachments | 8 items / 8 MB | a draft stays a draft rather than becoming a payload |
| speech cursors | 500 agents | bounded so the persisted record cannot grow without limit |
