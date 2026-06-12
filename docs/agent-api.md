# Agent API

This is the stable command surface intended for agents.

See [api-stability.md](api-stability.md) for the versioning policy, stable
command list, JSON compatibility rules, lease semantics, and WebSocket/HID
stability contract.

## JSON Output

```sh
simx lease --slug agent-a --ttl 10m --json
simx renew --slug agent-a --ttl 10m --json
simx install --slug agent-a --app path/to/App.app --json
simx run --slug agent-a --json
simx status --json
simx doctor --json
simx update --check --json
simx control snapshot --slug agent-a --json
simx control tap --slug agent-a --nx 0.5 --ny 0.5 --json
```

Lease and renew return:

- `slug`
- `udid`
- `device_name`
- `lease_expires_at`
- `lease_expires_at_rfc3339`
- `ttl_seconds`
- `serve.command`
- `serve.url`
- `serve.stream`
- `serve.h264_url`
- `serve.h264_stream`
- `serve.webrtc_url`
- `serve.webrtc_signaling`
- `serve.stats`
- `serve.control_mode`

`serve.stream` is the stable JPEG-over-WebSocket fallback route. H.264-specific
discovery details such as `?transport=h264` and `/<slug>/h264-stream` are
experimental until the transport is promoted in [api-stability.md](api-stability.md).

Status returns:

- pool size
- device type
- runtime
- per-device UDID, slug, expiry, serve PID, and serve URL

Install returns:

- `slug`
- `udid`
- `app`
- `bundle_id`
- `launched`

Run returns:

- `slug`
- `udid`
- `run_state`
- `log`
- `project`
- `scheme`
- `configuration`
- `derived_data_path`
- `app`
- `bundle_id`
- `launched`

Update returns:

- `ok`
- `current_version`
- `latest_version`
- `update_available`
- `installed`
- `installed_version`
- `install_path`
- `asset`
- `checksum_verified`

When a newer release is known, JSON outputs may include an additive `update`
object:

```json
{
  "update": {
    "available": true,
    "current_version": "0.2.0",
    "latest_version": "0.2.1",
    "command": "simx update"
  }
}
```

Agents that need fully hermetic command output can pass the global
`--no-update-check` flag.

## JSON Errors

Add `--json-errors` to any command to receive runtime and argument errors as JSON:

```sh
simx --json-errors lease --slug agent-a --ttl 0s --json
```

Error shape:

```json
{
  "ok": false,
  "code": "invalid_argument",
  "message": "ttl must be greater than zero"
}
```

Exit codes:

- `0`: success
- `1`: internal or unexpected error
- `2`: invalid arguments
- `3`: pool or lease state prevented the operation
- `4`: `simx doctor` found failing checks

Current error codes:

- `invalid_argument`
- `pool_not_initialized`
- `pool_full`
- `lease_not_found`
- `doctor_failed`
- `internal`

Agents should pass `--json-errors` whenever they need to parse command failures.

## Observe And Control

`simx control` is the agent-facing observe/action wrapper for an active lease.
It does not require `simx serve` and does not add a separate HTTP control API.
Each command looks up the active lease by slug, boots the simulator if needed,
opens a short-lived native SimulatorKit session for that UDID, performs the
requested operation, and exits.

`simx control` is local CLI authority. It does not send `claimControl`; claim
ownership remains a WebSocket-only concept for ordinary clients connected to
streams started with `--control-mode claim`.

Snapshot commands:

```sh
simx control snapshot --slug agent-a --json
simx control snapshot --slug agent-a --output snapshot.jpg --json
simx control snapshot --slug agent-a --inline-base64 --json
```

The default JSON snapshot is token-efficient. It returns metadata and cost
estimates without embedding image bytes:

```json
{
  "ok": true,
  "slug": "agent-a",
  "source": "native-snapshot",
  "format": "jpeg",
  "width": 393,
  "height": 852,
  "bytes": 184231,
  "sha1": "...",
  "estimated_base64_chars": 245644,
  "estimated_base64_tokens": 61411,
  "estimated_metadata_tokens": 93
}
```

Input commands:

```sh
simx control tap --slug agent-a --nx 0.5 --ny 0.5 --json
simx control touch --slug agent-a --phase began --nx 0.5 --ny 0.5 --json
simx control swipe --slug agent-a --from-nx 0.5 --from-ny 0.8 --to-nx 0.5 --to-ny 0.2 --json
simx control drag --slug agent-a --from-nx 0.2 --from-ny 0.2 --to-nx 0.8 --to-ny 0.8 --json
simx control key --slug agent-a --code KeyA --json
simx control paste --slug agent-a --text "hello" --json
simx control button --slug agent-a home --json
```

WebSocket control modes do not restrict `simx control`. They only decide which
browser/WebSocket clients may send HID through a served stream.

`simx control tree --slug agent-a --json` is reserved for a future accessibility
snapshot provider. It currently returns an unsupported-provider error instead
of falling back to screenshots or private, undocumented shell probes.

## Serve Lifecycle

Agents should lease first, then serve:

```sh
simx lease --slug agent-a --ttl 10m --json
simx serve --slug agent-a --host 127.0.0.1 --port 8080
```

`simx serve` requires an active lease. It records its PID in pool state. `simx release --slug agent-a` clears the lease and sends `SIGTERM` to the tracked serve process.

Serve defaults to `--control-mode read-only`, where clients receive frames but
cannot send HID input. Agents that need simulator input must pass an explicit
write mode: `--control-mode single-controller` for first-client control, or
`--control-mode claim` when connected clients should explicitly claim write
permission, or `--control-mode shared` when any connected client may send HID
input.

## Install And Launch

Agents should lease first, then install:

```sh
simx lease --slug agent-a --ttl 10m --json
simx install --slug agent-a --app path/to/App.app
```

`simx install` requires an active lease. It installs the `.app` bundle on the leased simulator and launches it by default. If `--bundle-id` is omitted, `simx` reads `CFBundleIdentifier` from the app bundle's `Info.plist`. Use `--no-launch` to install without launching.

## Build, Install, And Launch

Agents should lease first, then run from an app project's root folder:

```sh
simx lease --slug agent-a --ttl 10m --json
simx run --slug agent-a
```

`simx run` requires an active lease. It validates the current directory has exactly one `.xcodeproj` unless `--project` is provided. It builds the project quietly with `xcodebuild`, targeting the leased simulator UDID, writes the build log under `.simx/logs/`, then installs the built `.app` on that simulator, writes `.simx/run.json`, and launches it by default.

Defaults:

- `--scheme`: project file stem, for example `Lumi` from `Lumi.xcodeproj`.
- `--configuration`: `Debug`.
- `--derived-data-path`: `.simx/DerivedData/<slug>`.
- `--bundle-id`: inferred from the built app bundle's `Info.plist`.

Use `--no-launch` to build and install without launching.

`.simx/run.json` is temporary worktree-local state. It records the last run's slug, simulator UDID, project, scheme, derived data path, app bundle, bundle id, build log path, launch flag, and update timestamp. Projects should ignore `.simx/` in git.

`simx run` does not stream build output by default. Agents should inspect the returned `log` path only when they need build details, especially after a failed command.

## Doctor

```sh
simx doctor
simx doctor --json
```

Checks:

- `xcode-select -p`
- `xcrun simctl help`
- CoreSimulator private framework path
- SimulatorKit private framework path
- `xcrun simctl list runtimes -j`
- state directory resolvability

## Update

```sh
simx update --check
simx update
simx update --version v0.2.0
simx update --install-dir ~/.local/bin
simx update --json
```

`simx update` checks GitHub Releases and installs the Apple Silicon release
binary. `--check` reports whether a newer release is available without
installing it. `--version` installs a specific release tag, which also supports
rollback. If `--install-dir` is omitted, `simx update` replaces the currently
running binary when its directory is writable.
