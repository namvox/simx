# API And CLI Stability

This document defines the public `simx` contract for `v0.3.0`.

`simx` follows semantic versioning:

- Patch releases fix bugs without breaking stable CLI, JSON, or protocol contracts.
- Minor releases may add commands, flags, JSON fields, or protocol messages.
- Major releases may make breaking changes.

Deprecated stable flags, fields, or messages should remain for at least one
minor release when practical.

## Stable Commands

The following commands are stable in `v0.3.0`:

```sh
simx init
simx status
simx lease
simx renew
simx serve
simx release
simx clean
simx doctor
simx run
simx install
simx screenshot
simx record-video
simx update
simx control
```

Human-readable text output may change. Agents should use JSON output where
available.

## Experimental Commands

`simx preview` is an experimental SwiftUI-preview hot-reload command:

```sh
simx preview --slug browser --package Package.swift --package-target App
simx preview --slug browser --package Package.swift --package-target App --preview-filter StatusRow
simx preview --slug browser --package Package.swift --package-target App --once --json
```

It requires an active lease, generates a disposable host project outside the
package, installs that host on the leased simulator, and watches Swift Package
source changes by default. Hot reload rebuilds a preview dylib, copies it into
the running host app's data container, and notifies the host process without
relaunching it.

This command is not a stable contract in `v0.3.0`. Supported package shapes,
generated host internals, JSON fields, watch behavior, and reload diagnostics
may change in minor releases while the workflow is validated.

## Stable Agent JSON

The following machine-readable outputs are stable in `v0.3.0`:

```sh
simx status --json
simx lease --json
simx renew --json
simx doctor --json
simx run --json
simx install --json
simx screenshot --json
simx record-video --json
simx update --json
simx control snapshot --json
simx control tap --json
simx control touch --json
simx control swipe --json
simx control drag --json
simx control key --json
simx control paste --json
simx control button --json
simx --json-errors ...
```

Existing JSON fields will not be removed or renamed without a major release. New
fields may be added in minor releases, so agents should ignore unknown fields.

Agents should pass `--json-errors` when parsing failures.

When a newer release is known, agent-facing JSON commands may include an
additive `update` object:

```json
{
  "update": {
    "available": true,
    "current_version": "0.3.0",
    "latest_version": "0.3.1",
    "command": "simx update"
  }
}
```

Human-readable commands may print the same update hint to stderr. The passive
release check is cached and can be disabled with the global
`--no-update-check` flag.

## Lease Semantics

Lease ownership is keyed by `slug`.

- The same slug reuses the same active lease.
- Calling `simx lease --slug <slug>` for the same active slug extends the TTL.
- A different slug receives an idle simulator when one is available.
- If no simulator is available, a different slug waits until `--wait-timeout`,
  then exits with a pool-full error.
- Expired leases are reaped before status, lease, renew, and serve decisions.
- Non-booted unserved leases are reclaimed before status and lease decisions.
- `simx release --slug <slug>` clears ownership and keeps the simulator booted.
- `simx clean` shuts down and deletes all pool devices.

Default durations:

```text
lease --ttl: 30m
lease --wait-timeout: 60s
renew --ttl: 30m
```

There is no maximum TTL in `v0.3.0`.

## Streaming Contract

The serve CLI shape is stable:

```sh
simx serve --slug browser-agent --port 8080
simx lease --slug browser-agent --serve --port 8080
```

Default streaming options:

```text
--host 127.0.0.1
--port 8080
--quality 0.7
--fps 60
--transport jpeg
--control-mode read-only
--idle-timeout 5m
```

`--fps 120` remains supported as a configurable target. Actual frame rate is
host-dependent and may vary with macOS, Xcode, Simulator behavior, encoding
cost, browser performance, and backpressure.

Stable route shape:

```text
GET /<slug>
GET /<slug>/stats
WS  /<slug>/stream
```

Experimental H.264 route shape:

```text
GET /<slug>?transport=h264
WS  /<slug>/h264-stream
```

WebRTC prototype route shape:

```text
GET /<slug>?transport=webrtc
GET /<slug>/webrtc
POST /<slug>/webrtc-offer
```

The `--transport h264` serve option and H.264/WebCodecs route are experimental
transport surfaces for VideoToolbox/WebCodecs validation. They are not stable
production transport contracts yet. Until WAN-shaped benchmark evidence is
strong enough to promote them, the following H.264 details may change in minor
releases:

- route shape: `GET /<slug>?transport=h264` and `WS /<slug>/h264-stream`
- message envelope: `h264Config`, `SXH1` binary frames, keyframe recovery, and
  related WebSocket message ordering
- tuning defaults: encoded-size caps, source FPS targets, bitrate, keyframe
  cadence, delivery-age caps, queue bounds, and WebCodecs recovery behavior
- discovery details: H.264-specific URLs or transport metadata exposed in
  machine-readable output

The stable JPEG fallback remains `WS /<slug>/stream`. The current measured
local-loopback 60 fps browser success profile uses `--transport h264 --fps 70`
and a 640 px encoded-width cap; this is an experimental tuning detail, not a
stable API guarantee or WAN readiness claim.

The `--transport webrtc` serve option and WebRTC routes are an experimental
local loopback-video surface. `GET /<slug>/webrtc` returns a JSON descriptor for
the prototype. `POST /<slug>/webrtc-offer` accepts a browser SDP offer with a
video m-line and `hid: "websocket"`, then returns an SDP answer on the happy
path. HID/control remains on the existing WebSocket contract for this prototype.
Trickle ICE, TURN, full RTCP feedback, bitrate adaptation, WAN-shaped benchmark
evidence, and production readiness are not stable contracts.

Stable transport:

- Server-to-client simulator frames are binary JPEG WebSocket messages.
- Client-to-server HID/control messages are JSON text WebSocket messages.
- Server-to-client status, role, pause/resume, and acknowledgement messages are
  JSON text WebSocket messages.

Control mode is selected explicitly when serving:

- `--control-mode read-only` is the default. Clients receive frames but HID input
  is rejected with an acknowledgement when requested.
- `--control-mode single-controller` preserves the original controller contract:
  the first WebSocket client may send HID input and later clients are
  viewer-only until the controller disconnects and they reconnect.
- `--control-mode claim` lets any connected WebSocket client claim HID write
  permission by sending `type: "claimControl"`. A later claim transfers write
  permission to that client.
- `--control-mode shared` allows every connected WebSocket client to send HID
  input.

Streaming uses private Apple Simulator APIs and remains compatibility-sensitive
even though the CLI and route shape are stable.

Stable agent control commands:

```text
simx control snapshot --slug <slug> --json
simx control snapshot --slug <slug> --output <path> --json
simx control tap --slug <slug> --nx <0..1> --ny <0..1> --json
simx control touch --slug <slug> --phase <began|moved|ended|cancelled> --nx <0..1> --ny <0..1> --json
simx control swipe --slug <slug> --from-nx <0..1> --from-ny <0..1> --to-nx <0..1> --to-ny <0..1> --json
simx control drag --slug <slug> --from-nx <0..1> --from-ny <0..1> --to-nx <0..1> --to-ny <0..1> --json
simx control key --slug <slug> --code <KeyboardEvent.code> --json
simx control paste --slug <slug> --text <text> --json
simx control button --slug <slug> home --json
simx control button --slug <slug> soft-keyboard --json
```

These commands operate on the active lease directly through a short-lived native
SimulatorKit session. They do not require `simx serve`, do not use
`WS /<slug>/stream`, and do not create a separate HTTP control API.
`simx control snapshot --json` is intentionally metadata-only unless `--output`
or `--inline-base64` is requested. WebSocket control modes and `claimControl`
apply only to ordinary browser/WebSocket stream clients, not to local CLI
control commands.

Stable media capture commands:

```text
simx screenshot --slug <slug> --output <path> --json
simx record-video --slug <slug> --output <path> --duration <duration> --json
```

These commands require an active lease and boot the leased simulator if needed.
`simx screenshot` writes a PNG file through `xcrun simctl io screenshot`.
`simx record-video` writes a bounded MP4 file through `xcrun simctl io
recordVideo`, stops recording after `--duration`, and waits for the file to
finalize. Both commands reject existing output files unless `--force` is set.

Their JSON output includes stable `slug`, `udid`, `output`, and `bytes` fields.
`simx record-video --json` also includes `duration_seconds`.

Experimental agent control commands:

```text
simx control tree --slug <slug> --json
```

The accessibility tree command is reserved for a future provider and is not a
stable data contract yet.

## HID Message Core

The core HID/control message families from `v0.1.0` remain stable:

- Touch: `type: "touch"`.
- Keyboard: `type: "key"`.
- Buttons: `type: "button"` with `button: "home"` or `button: "softKeyboard"`.
- Resume: `type: "resume"`.

Additive fields and new message types may be added in minor releases. See
[hid-contract.md](hid-contract.md) for the wire format.

`v0.1.1` added an additive long-press scroll helper:

- Long-press scroll: `type: "longPressScroll"`.

`v0.2.0` added additive CLI control commands for snapshots and native HID
actions. These commands did not change the WebSocket HID message core.
