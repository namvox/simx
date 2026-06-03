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
simx --json-errors ...
```

Existing JSON fields will not be removed or renamed without a major release. New
fields may be added in minor releases, so agents should ignore unknown fields.

Agents should pass `--json-errors` when parsing failures.

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

Stable transport:

- Server-to-client simulator frames are binary JPEG WebSocket messages.
- Client-to-server HID/control messages are JSON text WebSocket messages.
- Server-to-client status, role, pause/resume, and acknowledgement messages are
  JSON text WebSocket messages.

Streaming uses private Apple Simulator APIs and remains compatibility-sensitive
even though the CLI and route shape are stable.

## HID Message Core

The core HID/control message families are frozen in `v0.1.0`:

- Touch: `type: "touch"`.
- Keyboard: `type: "key"`.
- Home: `type: "button"` with `button: "home"`.
- Resume: `type: "resume"`.

Additive fields and new message types may be added in minor releases. See
[hid-contract.md](hid-contract.md) for the wire format.
