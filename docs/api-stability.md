# API And CLI Stability

This document defines the public `simx` contract for `v0.1.0`.

`simx` follows semantic versioning:

- Patch releases fix bugs without breaking stable CLI, JSON, or protocol contracts.
- Minor releases may add commands, flags, JSON fields, or protocol messages.
- Major releases may make breaking changes.

Deprecated stable flags, fields, or messages should remain for at least one
minor release when practical.

## Stable Commands

The following commands are stable in `v0.1.0`:

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
simx update
```

Human-readable text output may change. Agents should use JSON output where
available.

## Stable Agent JSON

The following machine-readable outputs are stable in `v0.1.0`:

```sh
simx status --json
simx lease --json
simx renew --json
simx doctor --json
simx run --json
simx install --json
simx update --json
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
    "current_version": "0.1.0",
    "latest_version": "0.1.1",
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

There is no maximum TTL in `v0.1.0`.

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

Experimental route shape:

```text
GET /<slug>?transport=h264
WS  /<slug>/h264-stream
```

The `--transport h264` serve option and H.264 route are active development
transport surfaces for VideoToolbox/WebCodecs validation. Their message envelope,
viewer behavior, and JSON discovery fields may change before they are promoted
to a stable contract. The current measured 60 fps browser success profile uses
`--transport h264 --fps 70` and a 640 px encoded-width cap; this is an
experimental tuning detail, not a stable API guarantee.

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
```

These commands operate on the active lease directly through a short-lived native
SimulatorKit session. They do not require `simx serve`, do not use
`WS /<slug>/stream`, and do not create a separate HTTP control API.
`simx control snapshot --json` is intentionally metadata-only unless `--output`
or `--inline-base64` is requested. WebSocket control modes and `claimControl`
apply only to ordinary browser/WebSocket stream clients, not to local CLI
control commands.

Experimental agent control commands:

```text
simx control tree --slug <slug> --json
```

The accessibility tree command is reserved for a future provider and is not a
stable data contract yet.

## HID Message Core

The core HID/control message families are frozen in `v0.1.0`:

- Touch: `type: "touch"`.
- Keyboard: `type: "key"`.
- Home: `type: "button"` with `button: "home"`.
- Resume: `type: "resume"`.

Additive fields and new message types may be added in minor releases. See
[hid-contract.md](hid-contract.md) for the wire format.

`v0.1.1` adds an additive long-press scroll helper:

- Long-press scroll: `type: "longPressScroll"`.
