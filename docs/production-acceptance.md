# Production Acceptance Checklist

Use this checklist before tagging a release that claims production readiness for
the current stable agent, lease, streaming, HID, update, and install contracts.
It turns the production milestone into pass/fail evidence that can be attached
to the release PR, GitHub Release notes, or an internal release record.

The safe default command runs only local Rust checks and writes logs under
`.simx/production-acceptance/<timestamp>/`:

```sh
make production-acceptance
```

For a release-candidate host where real simulator mutation and packaging checks
are acceptable, run:

```sh
scripts/production-acceptance.sh --all-local --manual-plan
```

The script is intentionally conservative. It never runs `simx clean`, deletes
simulator devices, tags a release, or pushes to GitHub. Real-simulator tests,
doctor checks, and packaging checks require explicit flags.

## Evidence Rules

Each checklist row must have observable evidence before the release is marked
accepted:

- Exact command or browser action used.
- Expected pass condition.
- Output location, usually the script evidence directory or a linked PR comment.
- Any skipped item, with the reason and the follow-up owner.

The acceptance summary belongs in the PR template's Verification, Simulator And
Streaming Notes, Screenshots Or Demo, and Release Notes sections. The PR Release
Notes section must say whether `CHANGELOG.md` was updated. For stable CLI, JSON,
WebSocket, or HID changes, also reference [api-stability.md](api-stability.md)
and update the matching contract docs in the same PR.

## Automated Local Checks

Run on every release PR and before every tag.

| Area | Command | Expected pass condition | Evidence |
| --- | --- | --- | --- |
| Formatting | `cargo fmt --check` | Exits 0 with no diff required. | `cargo-fmt-check.stdout.log` and `cargo-fmt-check.stderr.log`. |
| Unit and integration tests | `cargo test` | Exits 0. | `cargo-test.stdout.log` and `cargo-test.stderr.log`. |
| Lints | `cargo clippy -- -D warnings` | Exits 0 with no warnings. | `cargo-clippy.stdout.log` and `cargo-clippy.stderr.log`. |
| Combined local check | `make check` | Exits 0 and covers formatting, tests, and clippy. | Use when not running the three commands separately; record terminal output or CI link. |

`make production-acceptance` runs the first three commands separately so each
check has its own log file.

## Gated Real-Simulator Checks

Run on a macOS host where creating, booting, leasing, and releasing simulator
pool devices is acceptable. These checks are gated because they mutate local
CoreSimulator state.

| Area | Command | Expected pass condition | Evidence |
| --- | --- | --- | --- |
| Real pool lifecycle | `SIMX_REAL_SIM_TESTS=1 cargo test --test real_pool` | Exits 0. Pool lifecycle behavior passes against a real simulator runtime. | `real-pool.stdout.log` and `real-pool.stderr.log`. |
| Host compatibility | `simx doctor --json` | Exits 0 and reports aggregate `ok: true`. | `simx-doctor-json.stdout.log`; attach JSON or summarize failing checks. |

Use:

```sh
scripts/production-acceptance.sh --real-sim --doctor
```

If the release candidate binary is not the first `simx` on `PATH`, pass it
explicitly:

```sh
scripts/production-acceptance.sh --doctor --simx-bin target/release/simx
```

## Agent API

Run these checks whenever the release touches agent-facing CLI or JSON behavior.
For unchanged agent API releases, link to passing automated tests and state that
the contract was unchanged.

| Area | Command | Expected pass condition | Evidence |
| --- | --- | --- | --- |
| Lease JSON | `simx lease --slug acceptance-api --ttl 10m --json` | JSON includes slug, UDID, expiry fields, and serve URLs. | Save stdout JSON. |
| Renew JSON | `simx renew --slug acceptance-api --ttl 10m --json` | JSON extends the active same-slug lease. | Save stdout JSON. |
| Status JSON | `simx status --json` | JSON reports pool and per-device lease/serve state. | Save stdout JSON. |
| Install JSON | `simx install --slug acceptance-api --app path/to/App.app --json` | Installs and launches the app, inferring `CFBundleIdentifier` when omitted. | Save stdout JSON and app bundle path. |
| Run JSON | `simx run --slug acceptance-api --json` | Builds quietly, writes a build log under `.simx/logs/`, installs, writes `.simx/run.json`, and launches by default. | Save stdout JSON, `.simx/run.json`, and build log path. |
| Preview JSON | `simx preview --slug acceptance-api --package Package.swift --package-target App --once --json` | Builds and launches the generated preview host for a Swift Package target and reports host/session metadata. | Save stdout JSON and preview build log paths. |
| Update check | `simx update --check --json` | Reports current/latest version and whether an update is available. | Save stdout JSON. |
| JSON errors | `simx --json-errors <failing-command>` | Runtime and argument errors use `{ ok, code, message }` and documented exit codes. | Save stdout/stderr and exit code. |

Release the slug when finished:

```sh
simx release --slug acceptance-api
```

## Lease And Serve Lifecycle

Run when lease, serve, release, TTL, process tracking, or reaping behavior
changes.

| Area | Command or action | Expected pass condition | Evidence |
| --- | --- | --- | --- |
| Idempotent lease | Run `simx lease --slug acceptance-lifecycle --ttl 10m --json` twice. | Same slug reuses and renews the active lease. | Save both JSON outputs. |
| Expiry and reap | Lease with a short TTL, wait for expiry, then run `simx status --json`. | Expired leases are reaped before status, lease, renew, and serve decisions. | Save commands and status JSON. |
| Serve existing lease | `simx serve --slug acceptance-lifecycle --port 8080` | Existing active lease serves viewer, stream, and stats endpoints. | Save serve stderr/stdout and endpoint checks. |
| Serve process tracking | `simx status --json` while serving. | Serve PID, host, and port are tracked in pool state. | Save status JSON. |
| Release stops serve | `simx release --slug acceptance-lifecycle` | Tracked serve process stops promptly. | Save release output and process check. |
| Serve expiry | `simx lease --slug acceptance-expiry --ttl 30s --serve --port 8081` | Serve exits when the lease expires. | Save command output and exit status. |

## Stream Stats

Run for every streaming release and whenever frame pacing, transport, or stats
fields change.

| Area | Command or action | Expected pass condition | Evidence |
| --- | --- | --- | --- |
| Stats endpoint | `curl -fsS http://127.0.0.1:8080/acceptance-smoke/stats` | JSON includes target FPS, lifetime source/sent frames, rolling 1s/5s source FPS, rolling 1s/5s sent FPS, rolling 1s/5s bytes per second, dropped frames, connected clients, controller connected, latest frame age, latest send age, and p50/p95 delivery latency. | Save stats JSON. |
| Viewer endpoint | Open `http://127.0.0.1:8080/acceptance-smoke`. | Browser viewer renders the simulator stream UI. | Screenshot or short screen recording. |
| JPEG stream | Connect to `ws://127.0.0.1:8080/acceptance-smoke/stream`. | WebSocket emits binary JPEG frames. | Browser devtools, script output, or test log proving binary frames. |
| Capture implementation | `pgrep -af "simctl.*io.*screenshot"` | No `simctl io screenshot` polling process is running. | Save command output; no matches is a pass. |

The default production stream remains JPEG-over-WebSocket. Treat H.264 evidence
as experimental until WAN-shaped profiles and stress-scene benchmark results are
recorded separately.

JPEG-over-WebSocket is the stable browser-stream fallback for this milestone.
`--transport h264`, `?transport=h264`, and `/<slug>/h264-stream` remain
experimental until WAN-shaped benchmark runs show the transport is ready for
promotion.

## Multi-Client Behavior

Run when WebSocket connection handling or control modes change.

| Area | Command or action | Expected pass condition | Evidence |
| --- | --- | --- | --- |
| Multiple viewers | Open two clients to the same stream. | Both clients receive frames. | Screenshot or client logs. |
| Read-only default | Start serve without `--control-mode`; send HID with `ack: true`. | Input is rejected with a negative acknowledgement. | Save WebSocket acknowledgement. |
| Single controller | Start with `--control-mode single-controller`; connect two clients. | First client controls HID; later clients are viewer-only. | Save acknowledgements or screen recording. |
| Claim mode | Start with `--control-mode claim`; send `type: "claimControl"`. | Claiming client receives HID write permission. | Save claim acknowledgement and input result. |
| Shared mode | Start with `--control-mode shared`. | Every connected client can send HID input. | Save acknowledgements or screen recording. |

## HID v2

Run when browser input, `simx control`, or HID mapping changes.

| Area | Command or action | Expected pass condition | Evidence |
| --- | --- | --- | --- |
| Legacy messages | Send existing touch, key, and Home messages. | Simulator responds as in the stable contract. | Save WebSocket acknowledgements or screen recording. |
| Keyboard modifiers | Send a modified keyboard event. | Modifier key down/up events are delivered correctly. | Save target app behavior or logs. |
| Paste | Paste supported text. | Text expands to key events in the simulator. | Screenshot or target app text field state. |
| Drag/swipe | Send drag or swipe input. | Input expands to touch down/move/up and affects the simulator. | Screen recording or viewer verification. |
| Long-press scroll | Send long-press scroll input. | Held touch, directional move, and touch up are delivered. | Screen recording or viewer verification. |
| Acknowledgements | Send messages with `ack: true`. | Success and failure acknowledgements match the HID contract. | Save WebSocket messages. |

## Doctor

Run before claiming simulator or streaming support on a release host.

| Area | Command | Expected pass condition | Evidence |
| --- | --- | --- | --- |
| Human-readable doctor | `simx doctor` | Reports Xcode, `xcrun simctl`, private framework paths, runtime listing, and state path checks. | Save terminal output when useful. |
| JSON doctor | `simx doctor --json` | Exits 0 and returns all checks plus aggregate `ok: true`. | Save JSON output. |

## Packaging And Release

Run before tagging a release. These checks are explicit because they create
release artifacts under `dist/`.

| Area | Command | Expected pass condition | Evidence |
| --- | --- | --- | --- |
| Package dry run | `make release-dry-run` | Exits 0 and creates `dist/simx-aarch64-apple-darwin.tar.gz` plus `dist/checksums.txt`. | `release-dry-run.stdout.log`, `release-dry-run.stderr.log`, and artifact listing. |
| Release metadata | Compare `Cargo.toml`, `Cargo.lock`, and `CHANGELOG.md`. | Versions agree and `CHANGELOG.md` has a dated section for the release. | Link diff or recorded command output. |
| Required repo files | Check `LICENSE`, `README.md`, and `SECURITY.md`. | Required files exist and match release expectations. | Link files or recorded command output. |
| Secret/history scan | `gitleaks detect --source .` and `rg -n "token|secret|password|api[_-]?key|PRIVATE KEY|BEGIN .*KEY|ghp_|sk-"` | No unresolved secret findings. | Save command output or explain unavailable tooling. |
| Release workflow | Inspect `.github/workflows/release.yml`. | Tagged release builds and uploads `simx-aarch64-apple-darwin.tar.gz`, `checksums.txt`, and `install.sh`. | Link workflow and release dry-run evidence. |
| Installer | Inspect or smoke `scripts/install.sh` / `install.sh`. | Installer selects a writable bin directory and installs the Apple Silicon binary. | Link diff, script output, or release artifact smoke. |
| Update command | `simx update --check` and, when safe, `simx update`. | Update checks latest release and verifies `checksums.txt` when available. | Save command output. |

Use:

```sh
scripts/production-acceptance.sh --release-dry-run
```

If packaging does not apply to a PR, say so in the PR Release Notes section and
explain why `CHANGELOG.md` did or did not change.

## Manual Smoke

Before marking production acceptance complete for a streaming release, manually
verify locally:

```sh
simx doctor --json
simx lease --slug acceptance-smoke --ttl 10m --serve --port 8080 --control-mode single-controller
curl -fsS http://127.0.0.1:8080/acceptance-smoke/stats
pgrep -af "simctl.*io.*screenshot"
simx release --slug acceptance-smoke
```

The browser smoke must confirm:

- `http://127.0.0.1:8080/acceptance-smoke` renders the viewer.
- `ws://127.0.0.1:8080/acceptance-smoke/stream` emits binary JPEG frames.
- `/acceptance-smoke/stats` reports target FPS, frame counts, drops, connected
  clients, controller state, frame age, send age, and latency.
- No `simctl io screenshot` capture process is running.
- Home, touch, keyboard, paste, drag/swipe, long-press scroll, and acked failure
  cases affect or report simulator state as expected.

If the smoke needs an app-specific surface, install or run it after leasing:

```sh
simx install --slug acceptance-smoke --app path/to/App.app --json
simx run --slug acceptance-smoke --json
```
