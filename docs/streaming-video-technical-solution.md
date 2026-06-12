# Streaming Video Technical Solution

This document describes the proposed direction for improving simulator
streaming so `simx` can target high-quality 60 fps browser playback over real
networks.

## Problem

The current browser stream is optimized for a simple local-first workflow:

```text
Simulator IOSurface
-> CoreImage CGImage
-> ImageIO JPEG encode
-> binary WebSocket frame
-> browser Blob URL
-> <img> source swap
```

That design is easy to inspect and works well enough on localhost, but it is a
poor fit for high-quality internet streaming.

The main issues are:

- Each frame is encoded as a standalone full-frame JPEG.
- The encoder cannot reuse information from earlier frames.
- JPEG encode work can consume meaningful CPU at 60 fps.
- WebSocket runs over reliable ordered delivery, so network loss can delay newer
  frames behind stale frame data.
- The browser repeatedly allocates Blob URLs and swaps an image element instead
  of using a native video playback pipeline.
- The current implementation starts the native frame source per WebSocket
  client, so multiple viewers can multiply capture and encode work.

The target experience is interactive simulator control with clear text,
responsive input, and sustained 60 fps when host and network conditions allow.

## Analysis

Simulator UI behaves more like screen sharing or cloud gaming than a photo feed.
Most frames are similar to the frames immediately before them. A video encoder
can exploit that temporal similarity, while JPEG cannot.

For local streams, the current path can hide inefficiency because bandwidth is
high and round-trip time is low. Over the internet, the same design becomes
sensitive to:

- full-frame bandwidth cost
- encode latency
- TCP head-of-line blocking
- browser image decode and DOM update overhead
- backpressure when the client cannot keep up

To hit a real 60 fps target, the stream must be measured as an end-to-end
pipeline:

```text
capture -> encode -> transport -> decode -> render
```

Average fps is not enough. The stream can average 60 fps while still feeling
laggy if frames arrive in bursts, old frames queue up, or input-to-visual
latency grows over time.

`encode_latency_ms_p50` and `encode_latency_ms_p95` should measure native
encoder duration, not source-to-WebSocket delivery time. Delivery latency should
be reported separately so benchmark failures point to the right pipeline stage.

The solution should prefer dropping stale frames over building latency. For an
interactive simulator, the newest frame is usually more valuable than guaranteed
delivery of every old frame.

## Proposed Solution

Move the primary internet streaming path from JPEG-over-WebSocket to
hardware-encoded video.

The proposed target pipeline is:

```text
Simulator IOSurface / CVPixelBuffer
-> VideoToolbox hardware H.264 encode
-> real-time video transport
-> browser native hardware decode
-> <video> or canvas presentation
```

### Encoder

Use Apple's VideoToolbox `VTCompressionSession` on macOS for native hardware
encoding.

Start with H.264 because it has the broadest browser hardware decode support.
HEVC can be considered later for Apple-only or explicitly negotiated clients,
but it should not be the default web compatibility path.

The initial encoder should target:

- 60 fps
- real-time encoding mode
- required hardware acceleration when the host supports it
- low-latency rate control
- low-latency settings
- no avoidable frame reordering
- zero queued frame delay
- bounded keyframe interval, likely 1-2 seconds
- configurable bitrate and quality targets
- generated SPS/PPS metadata for decoder setup

### Transport

Prefer WebRTC for the long-term internet transport because it is designed for
real-time media, network adaptation, jitter handling, and browser playback.

Keep the existing JPEG-over-WebSocket path as a local/debug fallback while the
new path matures.

A staged implementation can be:

1. Refactor serve to use one shared frame producer per leased simulator. Done in
   the first implementation slice.
2. Add one shared VideoToolbox H.264 encoder per served simulator. Native and
   Rust source primitives are in place.
3. Add a local encoded-frame transport for development validation. An
   experimental `/<slug>/h264-stream` WebSocket route and `?transport=h264`
   WebCodecs viewer mode are in place.
4. Add WebRTC signaling and media delivery.
5. Keep the existing WebSocket HID channel, or move control messages onto a
   WebRTC data channel after the video path is stable.

If WebRTC is too large for the first implementation step, an intermediate
WebCodecs transport may be useful:

```text
VideoToolbox H.264
-> WebSocket or WebTransport encoded chunks
-> WebCodecs VideoDecoder
-> canvas rendering
```

That is still not the final network story, because simx would own packetization,
loss behavior, keyframe recovery, and congestion control. It is useful as a
bridge for validating encoder quality and browser decode behavior.

The experimental H.264 WebSocket route uses:

```text
WS /<slug>/h264-stream
GET /<slug>?transport=h264
```

Server-to-client messages are:

- JSON `h264Config` messages with an `avc1` codec string and base64 AVC decoder
  configuration bytes.
- Binary `SXH1` frame messages containing keyframe, generation, timestamp, byte
  length, and the encoded H.264 sample in one WebSocket message.

Client-to-server HID/control messages use the existing JSON text message shape.
This route is for local validation and benchmark development. It is not the
final internet transport; WebRTC remains the target for production-quality WAN
streaming.

### Producer And Fanout

The serve process should own one capture/encode producer for the active lease.
Clients should subscribe to the latest encoded stream rather than starting their
own native frame sources.

This reduces CPU, avoids duplicate native private API registrations, and makes
stats easier to reason about.

### Backpressure

The stream should never build an unbounded queue of old frames.

When a client falls behind:

- keep the latest frame
- drop stale delta frames
- request or send a new keyframe when recovery is needed
- lower bitrate before allowing latency to grow
- expose drops and queue depth in stats

The experimental H.264 route also coalesces adjacent unacknowledged touch
`moved` messages before HID delivery. This keeps pointer bursts from starving
frame delivery while preserving `began`, `ended`, non-touch messages, and
messages that request acknowledgements.

The WebSocket sender loop should avoid artificial per-frame read waits. The
implementation uses nonblocking reads after the WebSocket upgrade and bounded
write retries for short backpressure. This keeps input handling from adding a
fixed delay before every frame and preserves the target 16.67 ms frame budget.

H.264/WebCodecs clients can request decoder recovery with:

```json
{"type":"requestKeyframe"}
```

The experimental H.264 route requests a keyframe when a client connects or
resumes, and the viewer requests one when it is waiting for a keyframe or sees a
decoder error. The native VideoToolbox bridge fulfills the request with
`kVTEncodeFrameOptionKey_ForceKeyFrame` on the next encoded frame.

### Compatibility

The current JPEG stream should remain available during the transition.

Suggested CLI direction:

```sh
simx serve --slug browser --port 8080 --transport jpeg
simx serve --slug browser --port 8080 --transport h264
simx serve --slug browser --port 8080 --transport webrtc
```

The exact CLI should be finalized when implementation begins. Any stable CLI,
JSON, WebSocket, WebRTC, or HID contract changes must update the relevant docs
and reference `docs/api-stability.md`.

## Benchmarking

Benchmark the stream as an end-to-end interactive system. The goal is not only
to send 60 frames per second from the server, but to render a smooth 60 fps
stream in the browser with bounded latency.

### Primary Success Metrics

The benchmarked 60 fps H.264 mode should pass when all of the following are true
for a 60 second run. The current success profile uses a 70 fps source target to
absorb browser and host scheduling jitter while requiring at least 60 rendered
fps:

- viewer render rate is at least 60 fps
- p95 frame interval is at most 21 ms
- p99 frame interval is at most 33 ms
- server source FPS over the final 5 second window is at least 60 fps
- server sent FPS over the final 5 second window is at least 60 fps
- p95 native H.264 encode latency is at most 12 ms
- p95 server delivery latency is at most 120 ms on LAN
- p95 browser decode plus render latency is at most 8 ms
- browser console issues are zero

Raw server drop rate is diagnostic, not a hard failure, because low-latency
interactive video intentionally drops stale surplus source frames when the
source produces more than the target presentation cadence.
- latency does not creep upward over the run
- server CPU is at least 40% lower than the current JPEG path at comparable
  visual quality

The strict local benchmark runner should fail when browser metrics pass but the
server is not actually producing and sending frames at the target rate. It checks
render FPS, p95/p99 frame interval, decode/render p95, source FPS, sent FPS,
target miss rate, native encode p95, and console health. Raw server drop rate is
still reported as a diagnostic, but it is not a strict failure by itself because
low-latency streaming intentionally drops surplus stale producer frames when the
source runs faster than the target send rate.

Glass-to-glass latency means the time from a simulator visual change to that
change being visible in the browser.

### Required Measurements

Each frame should carry a frame id and timestamps for:

- surface callback received
- encode submitted
- encode completed
- server send started
- server send completed
- client received
- decode completed
- rendered to screen

The stats endpoint should expose rolling and lifetime values for:

- target fps
- source fps
- encoded fps
- sent fps
- rendered fps, reported by the browser
- encode p50/p95/p99
- transport p50/p95/p99 when measurable
- decode/render p50/p95/p99
- frame interval p50/p95/p99
- dropped frames by reason
- keyframe count and keyframe interval
- bitrate
- bytes per second
- connected clients
- per-client send queue depth or latest-frame lag

### Benchmark Network Profiles

Run the same benchmark suite in at least these profiles:

- `local`: localhost or LAN, validates pipeline overhead.
- `wan-good`: about 50 ms RTT, low packet loss, enough bandwidth for the target
  bitrate.
- `wan-rough`: about 100 ms RTT, around 1% packet loss, constrained bandwidth.

The benchmark runner records the selected profile with
`SIMX_BENCH_NETWORK_PROFILE`. It does not change system network settings.
`local` is measured directly with no shaping. `wan-good` and `wan-rough` are
externally shaped profiles: apply the shaping with Network Link Conditioner, a
relay, or another repeatable testbed before running the command, then record the
tool in `SIMX_BENCH_NETWORK_SHAPER`.

Run the direct local profile:

```sh
SIMX_BENCH_NETWORK_PROFILE=local \
SIMX_BENCH_AUTO_LEASE=1 \
SIMX_BENCH_STRICT=1 \
SIMX_BENCH_DURATION_MS=60000 \
node scripts/benchmark-h264-viewer.js
```

Run the good WAN profile after applying about 50 ms RTT, 0% packet loss, and at
least 20 Mbps of available bandwidth:

```sh
SIMX_BENCH_NETWORK_PROFILE=wan-good \
SIMX_BENCH_NETWORK_SHAPER="Network Link Conditioner: 50 ms RTT, 0% loss, 20 Mbps" \
SIMX_BENCH_AUTO_LEASE=1 \
SIMX_BENCH_STRICT=1 \
SIMX_BENCH_DURATION_MS=60000 \
node scripts/benchmark-h264-viewer.js
```

Run the rough WAN profile after applying about 100 ms RTT, 1% packet loss, and
about 8 Mbps of available bandwidth:

```sh
SIMX_BENCH_NETWORK_PROFILE=wan-rough \
SIMX_BENCH_NETWORK_SHAPER="Network Link Conditioner: 100 ms RTT, 1% loss, 8 Mbps" \
SIMX_BENCH_AUTO_LEASE=1 \
SIMX_BENCH_STRICT=1 \
SIMX_BENCH_DURATION_MS=60000 \
node scripts/benchmark-h264-viewer.js
```

Optional measured values can be included in the report with
`SIMX_BENCH_OBSERVED_RTT_MS`, `SIMX_BENCH_OBSERVED_LOSS_PERCENT`,
`SIMX_BENCH_OBSERVED_DOWNLINK_MBPS`, `SIMX_BENCH_OBSERVED_UPLINK_MBPS`, and
free-form `SIMX_BENCH_NETWORK_NOTES`.

### Test Content

Use simulator scenes that stress different parts of the pipeline:

- mostly static screen with taps
- smooth scrolling list
- keyboard text entry
- animation-heavy screen
- full-screen color or gradient motion
- text-heavy UI where compression artifacts are easy to see

Each scenario should record the same metrics so the JPEG path and new video path
can be compared directly.

### Acceptance Report

Every benchmark run should produce a machine-readable report, for example:

```json
{
  "transport": "h264-websocket-webcodecs",
  "timestamp": "2026-06-12T10:00:00.000Z",
  "durationMs": 15000,
  "network": {
    "name": "local",
    "measuredDirectly": true,
    "target": {
      "rttMs": 0,
      "packetLossPercent": 0,
      "bandwidthMbps": null
    }
  },
  "environment": {
    "host": {
      "model": "Mac16,10",
      "macOS": {
        "productVersion": "26.5.1",
        "buildVersion": "25F80"
      }
    },
    "browser": {
      "name": "chromium",
      "channel": "chrome",
      "version": "..."
    },
    "simulator": {
      "udid": "...",
      "runtime": "com.apple.CoreSimulator.SimRuntime.iOS-26-5"
    }
  },
  "scenarioNames": ["static-taps", "smooth-scroll"],
  "lease": {
    "command": "target/debug/simx lease --slug h264-pacing-bench --ttl 5m --wait-timeout 5s --serve --port 8097 --fps 70 --transport h264 --control-mode single-controller --idle-timeout 2m --json"
  },
  "thresholds": {
    "renderedFps": 60,
    "frameIntervalP95Ms": 21,
    "frameIntervalP99Ms": 33,
    "decodeRenderP95Ms": 8,
    "serverSourceFps5s": 60,
    "serverSentFps5s": 60,
    "serverEncodeP95Ms": 12,
    "serverDeliveryP95Ms": 120
  },
  "metrics": {
    "renderedFps": 68.1,
    "frameIntervalP95Ms": 19.8,
    "decodeRenderP95Ms": 1.4
  },
  "serverStats": {
    "encode_latency_ms_p95": 10,
    "delivery_latency_ms_p95": 79
  },
  "ok": true
}
```

The report should include the host model, macOS version, Xcode version, browser
name/version/channel when available, simulator UDID/runtime when available, the
selected network profile, exact `simx` auto-lease command when the runner starts
the stream, benchmark duration, scenarios, and timestamp.

### Local Runner

During development, use the script runner in auto-lease strict mode when a pool
device is available:

```sh
SIMX_BENCH_AUTO_LEASE=1 \
SIMX_BENCH_STRICT=1 \
SIMX_BENCH_DURATION_MS=15000 \
node scripts/benchmark-h264-viewer.js
```

Auto-lease mode serves with `--control-mode single-controller` by default so
the stress scenes can send touch and keyboard input to the simulator. Override
that with `SIMX_BENCH_CONTROL_MODE` when testing another control mode.

To measure an already-served H.264 viewer instead, provide `SIMX_VIEWER_URL`:

```sh
SIMX_VIEWER_URL="http://127.0.0.1:8092/h264-browser-bench?transport=h264" \
SIMX_BENCH_DURATION_MS=15000 \
node scripts/benchmark-h264-viewer.js
```

The script opens the viewer in Playwright, waits for H.264 frames to render,
drives the configured benchmark scenes, reads the viewer's hidden `#metrics`
report after each scene, fetches `/<slug>/stats`, and prints one JSON object.

By default the runner executes every stress scene:

- `static-taps`
- `smooth-scroll`
- `keyboard-entry`
- `animation-heavy`
- `full-motion`
- `text-heavy`

Use `SIMX_BENCH_SCENARIOS` to run a subset:

```sh
SIMX_BENCH_AUTO_LEASE=1 \
SIMX_BENCH_STRICT=1 \
SIMX_BENCH_DURATION_MS=15000 \
SIMX_BENCH_SCENARIOS=static-taps,smooth-scroll,full-motion \
node scripts/benchmark-h264-viewer.js
```

The output includes aggregate `ok`, `thresholds`, `scenarioNames`, `failures`,
and a `scenarioResults` array. Each scenario result includes its name,
description, duration, checks, failures, derived server drop/target-miss rates,
browser metrics, server stats, console issues observed during that scene, and
per-scene `ok`.

The runner starts a local scene server and opens each deterministic scene inside
the leased simulator before measuring. When measuring an already-served viewer
without `SIMX_BENCH_AUTO_LEASE=1`, set `SIMX_BENCH_UDID` if the script should
open those scenes in the simulator automatically.

Set `SIMX_BENCH_STRICT=1` to make the script exit non-zero when the current
metrics miss the 60 fps thresholds. Keep strict mode off while the experimental
transport is still below target so CI can collect comparable reports without
failing the whole check.
