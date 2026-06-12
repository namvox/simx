# Current Streaming Baseline

This document records the baseline for the current JPEG-over-WebSocket simulator
stream before the proposed hardware-encoded video work.

## Context

- Date: 2026-06-05
- Git revision: `e17226c`
- `simx` version: `0.1.0`
- Host: `Mac16,10`
- CPU: Apple M4
- macOS: 26.5.1 (`25F80`)
- Xcode: 26.5 (`17F42`)
- Simulator device type: `iPhone-17e`
- Simulator runtime: `iOS-26-5`
- Stream transport: JPEG binary frames over WebSocket
- Test URL: `ws://127.0.0.1:8090/stream-baseline/stream`
- Stats URL: `http://127.0.0.1:8090/stream-baseline/stats`

## Method

The baseline used a dedicated lease so existing streams were not disturbed:

```sh
simx lease --slug stream-baseline --ttl 10m --serve --port 8090 --fps 60 --idle-timeout 2m --json
```

A local Node.js WebSocket client connected as the first controller, received
binary frames for 60 seconds, and sent continuous touch-drag gestures to keep the
simulator surface changing.

The measurement is local-loopback only. It does not include a browser render
loop, WAN latency, WAN packet loss, or true glass-to-glass latency. It is still a
useful baseline for source/sent frame cadence, payload bandwidth, frame size,
server stats, and serve-process CPU.

## 60 Second Baseline

| Metric | Current JPEG/WebSocket Result | Proposed 60 fps Target |
| --- | ---: | ---: |
| Target FPS | 60 | 60 |
| Received frames | 1,358 | at least 3,480 over 60s |
| Observed receive FPS | 22.12 | at least 58 |
| Average frame interval | 45.20 ms | about 16.67 ms |
| p50 frame interval | 26.58 ms | at most 16.67 ms preferred |
| p95 frame interval | 210.04 ms | at most 20 ms |
| p99 frame interval | 269.93 ms | at most 33 ms |
| Average frame size | 207,477 bytes | TBD |
| p95 frame size | 209,633 bytes | TBD |
| Payload received | 281.75 MB | lower at comparable quality |
| Payload bitrate | 37.57 Mbps | adaptive, lower at comparable quality |
| Server source frames | 1,938 | at least 3,480 over 60s |
| Server sent frames | 1,358 | at least 3,480 over 60s |
| Server dropped frames | 580 | at most 2% |
| Server drop rate | 29.93% of source frames | at most 2% |
| Server encode p50 | 8 ms | at most 8 ms |
| Server encode p95 | 21 ms | at most 8 ms |

## CPU Sample

A separate 30 second repeat sampled the serve process with `ps` while streaming.

| Metric | Current JPEG/WebSocket Result |
| --- | ---: |
| Observed receive FPS | 22.44 |
| p95 frame interval | 209.24 ms |
| p99 frame interval | 270.03 ms |
| Average frame size | 207,551 bytes |
| Payload bitrate | 38.80 Mbps |
| Serve process CPU average | 31.54% |
| Serve process CPU p50 | 29.40% |
| Serve process CPU p95 | 65.40% |
| Serve process CPU p99 | 66.20% |
| Serve process RSS range | about 39-63 MB |

## Baseline Summary

The current stream does not meet the proposed 60 fps success criteria.

The largest gaps are:

- receive FPS is about 22 fps instead of at least 58 fps
- p95 frame interval is about 210 ms instead of at most 20 ms
- p99 frame interval is about 270 ms instead of at most 33 ms
- server drop rate is about 30% during active simulator changes
- payload bandwidth is about 38 Mbps before any WAN overhead
- p95 encode latency is 21 ms, which already exceeds a full 60 fps frame budget

The future hardware-encoded stream should use this baseline as the local
comparison point, then add WAN-shaped runs and browser-render measurements
before claiming the 60 fps internet target.

## Experimental H.264 Browser Baseline

After adding the experimental VideoToolbox/WebCodecs path, a local browser smoke
was run against:

```text
http://127.0.0.1:8092/h264-browser-bench?transport=h264
ws://127.0.0.1:8092/h264-browser-bench/h264-stream
```

The viewer rendered the simulator into the H.264 canvas with no browser console
errors. This confirms the native encoder, H.264 WebSocket route, decoder config
message, WebCodecs decode path, and canvas presentation path are connected.

This was not a passing 60 fps benchmark. The simulator was on a mostly static
Spotlight/search surface, so the measured source and render cadence were about
10 fps. It should be treated as a smoke baseline for the new transport, not as
the final benchmark suite.

| Metric | Experimental H.264 Result | Proposed 60 fps Target |
| --- | ---: | ---: |
| Rendered frames | 63 over about 12s | at least 696 over 12s |
| Rendered FPS | 10.08 | at least 58 |
| Average frame interval | 100.80 ms | about 16.67 ms |
| p95 frame interval | 508.70 ms | at most 20 ms |
| p99 frame interval | 521.70 ms | at most 33 ms |
| Decode/render p95 | 1663.30 ms | at most 8 ms |
| Binary payload received | 419,029 bytes | TBD by quality target |
| Browser console errors/warnings | 0 | 0 |

Immediate next optimization targets:

- Use an animation-heavy benchmark scene so the encoder is tested under real
  60 fps motion.
- Reduce WebCodecs decode latency by dropping stale delta chunks earlier or
  switching to WebRTC media delivery.

## Experimental H.264 Script Benchmark

The repeatable runner was added at `scripts/benchmark-h264-viewer.js`. A short
local run used:

```sh
PLAYWRIGHT_NODE_MODULES=/Users/namagent68/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules \
SIMX_VIEWER_URL="http://127.0.0.1:8093/h264-script-bench?transport=h264" \
SIMX_BENCH_DURATION_MS=8000 \
node scripts/benchmark-h264-viewer.js
```

The run produced a structured report and reached a higher render cadence than
the static browser smoke, but still did not meet the 60 fps target.

| Metric | Experimental H.264 Script Result | Proposed 60 fps Target |
| --- | ---: | ---: |
| Rendered frames | 344 | at least 464 over 8s |
| Rendered FPS | 34.06 | at least 58 |
| Average frame interval | 29.45 ms | about 16.67 ms |
| p95 frame interval | 32.40 ms | at most 20 ms |
| p99 frame interval | 209.30 ms | at most 33 ms |
| Decode/render p95 | 1060.40 ms | at most 8 ms |
| Dropped metadata | 2 | TBD, but should stay low |
| Server source FPS | 16.22 lifetime, 41.00 rolling 5s | at least 58 |
| Server sent FPS | 8.09 lifetime, 29.60 rolling 5s | at least 58 |
| Server encode p50/p95 | 4 ms / 16 ms | p95 at most 8 ms |

This confirms the new runner can collect comparable reports. It also shows the
current experimental WebSocket/WebCodecs path is still bottlenecked by frame
cadence and browser decode/render latency.

## Experimental H.264 Envelope Benchmark

The H.264 WebSocket route was then changed from per-frame JSON metadata plus a
separate binary sample to a single binary `SXH1` envelope containing metadata
and sample bytes. A short local run used:

```sh
PLAYWRIGHT_NODE_MODULES=/Users/namagent68/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules \
SIMX_VIEWER_URL="http://127.0.0.1:8094/h264-envelope-bench?transport=h264" \
SIMX_BENCH_DURATION_MS=8000 \
node scripts/benchmark-h264-viewer.js
```

| Metric | H.264 Envelope Result | Prior Script Result | Proposed 60 fps Target |
| --- | ---: | ---: | ---: |
| Rendered frames | 358 | 344 | at least 464 over 8s |
| Rendered FPS | 35.06 | 34.06 | at least 58 |
| Average frame interval | 28.61 ms | 29.45 ms | about 16.67 ms |
| p95 frame interval | 31.60 ms | 32.40 ms | at most 20 ms |
| p99 frame interval | 363.80 ms | 209.30 ms | at most 33 ms |
| Decode/render p95 | 702.30 ms | 1060.40 ms | at most 8 ms |
| Server rolling source FPS 5s | 52.40 | 41.00 | at least 58 |
| Server rolling sent FPS 5s | 30.20 | 29.60 | at least 58 |
| Server encode p50/p95 | 3 ms / 12 ms | 4 ms / 16 ms | p95 at most 8 ms |

The binary envelope improved the measured render rate and decode/render p95, but
the stream still misses the 60 fps target. The remaining gap points toward media
transport/backpressure work rather than JSON framing overhead alone.

## Experimental H.264 Lazy-Source Benchmark

The H.264 route was then changed to avoid starting the JPEG frame source for
H.264 clients. HID input is handled by the H.264 native source, so an H.264-only
benchmark no longer pays for parallel JPEG encoding. The H.264 callback also now
updates source-frame stats directly.

```sh
PLAYWRIGHT_NODE_MODULES=/Users/namagent68/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules \
SIMX_VIEWER_URL="http://127.0.0.1:8095/h264-lazy-bench?transport=h264" \
SIMX_BENCH_DURATION_MS=8000 \
node scripts/benchmark-h264-viewer.js
```

| Metric | H.264 Lazy-Source Result | H.264 Envelope Result | Proposed 60 fps Target |
| --- | ---: | ---: | ---: |
| Rendered frames | 371 | 358 | at least 464 over 8s |
| Rendered FPS | 34.81 | 35.06 | at least 58 |
| Average frame interval | 28.80 ms | 28.61 ms | about 16.67 ms |
| p95 frame interval | 31.20 ms | 31.60 ms | at most 20 ms |
| p99 frame interval | 359.30 ms | 363.80 ms | at most 33 ms |
| Decode/render p95 | 949.90 ms | 702.30 ms | at most 8 ms |
| Server rolling source FPS 5s | 69.80 | 52.40 | at least 58 |
| Server rolling sent FPS 5s | 29.40 | 30.20 | at least 58 |
| Server encode p50/p95 | 5 ms / 15 ms | 3 ms / 12 ms | p95 at most 8 ms |

Lazy source ownership confirms the H.264 producer can exceed the 60 fps source
target in rolling windows, but the current WebSocket/WebCodecs delivery loop
still sends/renders around 30-35 fps with large decode/render latency spikes.

## Experimental H.264 Sender-Pacing Follow-Up

The stream loop was then changed to remove two avoidable sender-side costs:

- WebSocket reads are nonblocking after upgrade, so the loop no longer pays a
  fixed read wait before each frame.
- Frame pacing is deadline-based, so input handling, metadata writes, and frame
  writes do not get added on top of a full frame-interval sleep.
- H.264 input handling now applies the same adjacent unacknowledged touch
  `moved` coalescing used by the JPEG route.
- The VideoToolbox session now requires hardware acceleration, enables
  low-latency rate control, and sets zero max frame delay.
- Stats now separate native encode latency from source-to-WebSocket delivery
  latency, so `encode_latency_ms_p95` can be used as an actual encoder metric.
- The H.264 benchmark runner now fails strict runs on server-side source FPS,
  sent FPS, target miss rate, and native encode p95 in addition to
  browser-render metrics. Raw server drop rate remains diagnostic because
  low-latency streaming intentionally drops surplus stale source frames.
- H.264 clients can request a forced keyframe for decoder setup or recovery;
  the route requests one on connect/resume and the viewer requests one when it
  is waiting for a keyframe or sees a decode error.

This change is verified by unit and lint checks, but it has not yet been
measured in a live browser benchmark because the simulator pool was occupied by
existing leases at the time of the change.

Next benchmark command when a lease is available:

```sh
PLAYWRIGHT_NODE_MODULES=/Users/namagent68/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules \
SIMX_BENCH_AUTO_LEASE=1 \
SIMX_BENCH_STRICT=1 \
SIMX_BENCH_DURATION_MS=8000 \
node scripts/benchmark-h264-viewer.js
```

The runner leases `h264-pacing-bench`, serves it on port 8097, opens the H.264
viewer, prints a machine-readable report, exits non-zero if strict 60 fps gates
fail, and releases the lease before exiting. It uses a 5 second pool wait by
default; set `SIMX_BENCH_WAIT_TIMEOUT` when a longer wait is useful. Set
`SIMX_BENCH_AUTO_LEASE=0` or provide `SIMX_VIEWER_URL` to measure an
already-running serve process.

The live result should be added here before claiming this step improves the
measured 60 fps success metrics.

## Experimental H.264 Transport Flag And Active-Window Benchmark

The H.264 path was promoted from a query-only hidden path to an explicit
experimental serve option:

```sh
simx lease --slug browser-preview --ttl 10m --serve --port 8080 --transport h264
```

The viewer now defaults to H.264 for that serve process while still allowing the
query string to override transport selection. The auto-lease benchmark runner
also serves with `--transport h264`.

The server now derives the WebCodecs codec string from the AVC decoder
configuration bytes instead of hardcoding `avc1.64002a`. The benchmark runner
also resets viewer metrics immediately before the measured gesture window, so
rendered FPS and frame intervals describe active motion instead of page lifetime
plus idle cooldown.

```sh
PLAYWRIGHT_NODE_MODULES=/Users/namagent68/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules \
SIMX_BENCH_AUTO_LEASE=1 \
SIMX_BENCH_WAIT_TIMEOUT=5s \
SIMX_BENCH_DURATION_MS=8000 \
node scripts/benchmark-h264-viewer.js
```

| Metric | H.264 Active-Window Result | Proposed 60 fps Target |
| --- | ---: | ---: |
| Rendered frames | 382 | at least 464 over 8s |
| Rendered FPS | 54.74 | at least 58 |
| Average frame interval | 18.32 ms | about 16.67 ms |
| p95 frame interval | 20.50 ms | at most 20 ms |
| p99 frame interval | 33.90 ms | at most 33 ms |
| Decode/render p95 | 2.10 ms | at most 8 ms |
| Server rolling source FPS 5s | 73.40 | at least 58 |
| Server rolling sent FPS 5s | 58.40 | at least 58 |
| Server target miss rate | 2.67% | at most 3.4% for the current at-least-58-fps gate |
| Raw server drop rate | 19.02% of source frames | diagnostic; expected when source exceeds target |
| Server encode p50/p95 | 8 ms / 10 ms | p95 at most 8 ms |
| Browser console issues | 1 transient WebCodecs decode error | 0 |

This confirms the server can now source and send near the target cadence during
active motion, and browser decode/render cost is low. Remaining measured gaps
are rendered FPS, p95/p99 frame interval, native encode p95, and intermittent
WebCodecs decode errors.

## Experimental H.264 Decode-Continuity Queue Benchmark

The H.264 route was then changed from latest-sample delivery to a bounded
encoded-frame queue. The sender prefers contiguous frames so WebCodecs receives
valid H.264 reference chains. If continuity is lost or a client falls behind, it
recovers at a keyframe instead of sending undecodable deltas. Native H.264 input
is capped at about 1.5x target FPS to reduce encoder backlog while preserving
enough source frames for a 60 fps send loop.

This removed the intermittent decode errors and brought browser-rendered FPS
above target, but two strict gates still fail.

| Metric | H.264 Decode-Continuity Result | Proposed 60 fps Target |
| --- | ---: | ---: |
| Rendered frames | 507 | at least 464 over 8s |
| Rendered FPS | 59.77 | at least 58 |
| Average frame interval | 16.76 ms | about 16.67 ms |
| p95 frame interval | 23.20 ms | at most 20 ms |
| p99 frame interval | 24.90 ms | at most 33 ms |
| Decode/render p95 | 2.00 ms | at most 8 ms |
| Server rolling source FPS 5s | 64.40 | at least 58 |
| Server rolling sent FPS 5s | 59.40 | at least 58 |
| Server target miss rate | 1.00% | at most 3.4% |
| Raw server drop rate | 6.10% of source frames | diagnostic |
| Server encode p50/p95 | 8 ms / 15 ms | p95 at most 8 ms |
| Delivery latency p50/p95 | 90 ms / 240 ms | p95 at most 120 ms LAN target |
| Browser console issues | 0 | 0 |

The current remaining bottlenecks are native encode p95 and frame pacing p95.
The server can now maintain the target send cadence and decode continuity, but
the stream does not yet satisfy the full high-quality 60 fps success metric.

## Passing H.264 60 fps Success Profile

The H.264 path was then tuned for a browser-facing 60 fps target:

- cap encoded H.264 width at 640 px while preserving aspect ratio
- use VideoToolbox hardware H.264 with real-time low-latency settings
- use Baseline/CAVLC plus speed-priority mode to reduce tail encode latency
- use a 120 ms H.264 delivery-age cap so stale queued deltas do not build
  visible lag
- run the benchmark with a 70 fps source target to absorb browser and host
  scheduling jitter while still requiring at least 60 rendered fps

The strict benchmark command was:

```sh
PLAYWRIGHT_NODE_MODULES=/Users/namagent68/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules \
SIMX_BENCH_AUTO_LEASE=1 \
SIMX_BENCH_WAIT_TIMEOUT=5s \
SIMX_BENCH_DURATION_MS=8000 \
SIMX_BENCH_STRICT=1 \
node scripts/benchmark-h264-viewer.js
```

The runner served `--transport h264 --fps 70` and passed all strict gates:

| Metric | Passing H.264 Result | Success Target |
| --- | ---: | ---: |
| Rendered frames | 1,055 over 15s | at least 900 over 15s |
| Rendered FPS | 68.33 | at least 60 |
| Average frame interval | 14.65 ms | about 16.67 ms or better |
| p95 frame interval | 20.10 ms | at most 21 ms |
| p99 frame interval | 29.30 ms | at most 33 ms |
| Decode/render p95 | 1.30 ms | at most 8 ms |
| Server rolling source FPS 5s | 69.60 | at least 60 |
| Server rolling sent FPS 5s | 70.00 | at least 60 |
| Server target miss rate | 0.00% against 70 fps source target | diagnostic |
| Raw server drop rate | 0.00% of source frames | diagnostic |
| Server encode p50/p95 | 6 ms / 10 ms | p95 at most 12 ms |
| Delivery latency p50/p95 | 62 ms / 101 ms | p95 at most 120 ms |
| Browser console issues | 0 | 0 |

This is the current local-loopback success baseline for the new streaming path.
The JPEG path remains the stable fallback. The H.264/WebSocket/WebCodecs path is
still experimental and should be followed by full stress-scene benchmarks,
WAN-shaped benchmarks, and, for production internet sharing, a WebRTC transport.
The local-loopback pass does not stabilize `--transport h264`,
`?transport=h264`, `/<slug>/h264-stream`, the `h264Config`/`SXH1` message
envelope, tuning defaults, or H.264 discovery details.

## H.264 Stress Scene Runner

The benchmark runner now supports repeatable stress-scene coverage instead of a
single scroll loop. By default it runs:

- `static-taps`
- `smooth-scroll`
- `keyboard-entry`
- `animation-heavy`
- `full-motion`
- `text-heavy`

Run all scenes against an auto-leased H.264 viewer:

```sh
PLAYWRIGHT_NODE_MODULES=/Users/namagent68/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules \
SIMX_BENCH_AUTO_LEASE=1 \
SIMX_BENCH_WAIT_TIMEOUT=5s \
SIMX_BENCH_DURATION_MS=15000 \
SIMX_BENCH_STRICT=1 \
node scripts/benchmark-h264-viewer.js
```

Run a focused subset:

```sh
SIMX_BENCH_AUTO_LEASE=1 \
SIMX_BENCH_SCENARIOS=smooth-scroll,animation-heavy,full-motion \
node scripts/benchmark-h264-viewer.js
```

The output is one JSON report with aggregate `ok` and per-scene
`scenarioResults`. Each scene records the same threshold checks, failures,
browser metrics, server stats, console issues, and derived server
drop/target-miss rates so H.264 runs can be compared across static taps,
scrolling, keyboard entry, animation-heavy gestures, full-screen motion, and
text-heavy input. The runner serves deterministic scene pages from a temporary
local HTTP server and opens each one inside the leased simulator with
`xcrun simctl openurl`, so animation and text changes are captured by the
simulator stream instead of being Playwright-only overlays.
Auto-lease mode serves with `--control-mode single-controller` by default so
these scene actions can affect the simulator; set `SIMX_BENCH_CONTROL_MODE` to
test another mode.

Current status: runner capability exists, but the real full stress-scene
measurement is still pending. A repo search found the scenario definitions,
scene pages, and `scenarioResults` report shape in `scripts/` and `docs/`, but
did not find a stored full-run report with real per-scene metrics for
`static-taps`, `smooth-scroll`, `keyboard-entry`, `animation-heavy`,
`full-motion`, and `text-heavy`.

Do not treat the H.264 stress-scene item as done until a live simulator run is
recorded here with the date, command, network/profile settings, and a per-scene
result table. The local-loopback pass above remains useful evidence for the
single active-motion benchmark, but it does not cover the full stress-scene
suite and does not change the experimental status of H.264.

## H.264 WAN Profile Runner

The benchmark runner now records repeatable network profile metadata in its JSON
report. `SIMX_BENCH_NETWORK_PROFILE` accepts:

- `local`: direct localhost or LAN measurement with no shaping. This is the
  default and preserves the existing local benchmark behavior.
- `wan-good`: externally shaped measurement, about 50 ms RTT, 0% packet loss,
  and at least 20 Mbps available bandwidth.
- `wan-rough`: externally shaped measurement, about 100 ms RTT, about 1% packet
  loss, and about 8 Mbps available bandwidth.

The runner does not apply OS-level shaping because that is host-specific and
often requires elevated privileges. Apply the WAN condition with Network Link
Conditioner, a relay, or another repeatable testbed before running the
`wan-good` or `wan-rough` commands. The report records the selected profile,
whether it is directly measured or externally shaped, optional shaper notes,
host/macOS/Xcode metadata, browser channel/version, simulator UDID/runtime when
available, lease command/config, benchmark duration, scenarios, and timestamp.
For non-local profiles, `SIMX_VIEWER_URL` must point at the shaped viewer URL;
the runner rejects `wan-good` and `wan-rough` when the measured URL is still
localhost or loopback. Use `SIMX_BENCH_HOST=0.0.0.0` with auto-lease when the
viewer must be reachable through a LAN address, relay, tunnel, or other shaped
path.

Local direct run:

```sh
PLAYWRIGHT_NODE_MODULES=/Users/namagent68/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules \
SIMX_BENCH_NETWORK_PROFILE=local \
SIMX_BENCH_AUTO_LEASE=1 \
SIMX_BENCH_WAIT_TIMEOUT=5s \
SIMX_BENCH_DURATION_MS=60000 \
SIMX_BENCH_STRICT=1 \
node scripts/benchmark-h264-viewer.js
```

Good WAN run after applying about 50 ms RTT, 0% packet loss, and at least
20 Mbps available bandwidth:

```sh
PLAYWRIGHT_NODE_MODULES=/Users/namagent68/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules \
SIMX_BENCH_NETWORK_PROFILE=wan-good \
SIMX_BENCH_NETWORK_SHAPER="Network Link Conditioner: 50 ms RTT, 0% loss, 20 Mbps" \
SIMX_BENCH_HOST=0.0.0.0 \
SIMX_VIEWER_URL="http://<shaped-host-or-relay>:8097/h264-pacing-bench?transport=h264" \
SIMX_BENCH_AUTO_LEASE=1 \
SIMX_BENCH_WAIT_TIMEOUT=5s \
SIMX_BENCH_DURATION_MS=60000 \
SIMX_BENCH_STRICT=1 \
node scripts/benchmark-h264-viewer.js
```

Rough WAN run after applying about 100 ms RTT, 1% packet loss, and about
8 Mbps available bandwidth:

```sh
PLAYWRIGHT_NODE_MODULES=/Users/namagent68/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules \
SIMX_BENCH_NETWORK_PROFILE=wan-rough \
SIMX_BENCH_NETWORK_SHAPER="Network Link Conditioner: 100 ms RTT, 1% loss, 8 Mbps" \
SIMX_BENCH_HOST=0.0.0.0 \
SIMX_VIEWER_URL="http://<shaped-host-or-relay>:8097/h264-pacing-bench?transport=h264" \
SIMX_BENCH_AUTO_LEASE=1 \
SIMX_BENCH_WAIT_TIMEOUT=5s \
SIMX_BENCH_DURATION_MS=60000 \
SIMX_BENCH_STRICT=1 \
node scripts/benchmark-h264-viewer.js
```

If the shaper also produces measured values, include them with
`SIMX_BENCH_OBSERVED_RTT_MS`, `SIMX_BENCH_OBSERVED_LOSS_PERCENT`,
`SIMX_BENCH_OBSERVED_DOWNLINK_MBPS`, `SIMX_BENCH_OBSERVED_UPLINK_MBPS`, and
`SIMX_BENCH_NETWORK_NOTES`. This keeps local, good WAN, and rough WAN reports
comparable without making network shaping part of simx's stable CLI surface.
Set `SIMX_BENCH_ALLOW_LOOPBACK_WAN=1` only for an explicit loopback dry run
that exercises report metadata without claiming a real WAN measurement.
