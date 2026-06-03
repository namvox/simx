# Production Milestone Acceptance

This milestone is complete only when all checks below pass.

## Agent API

- `simx lease --slug <slug> --ttl 10m --json` returns slug, UDID, expiry, and serve URLs.
- `simx renew --slug <slug> --ttl 10m --json` renews an active lease.
- `simx install --slug <slug> --app <path-to.app> --json` installs and launches an app on the active lease.
- `simx run --slug <slug> --json` validates the current Xcode project, builds it quietly, returns a build log path, installs the app, writes `.simx/run.json`, and launches it on the active lease.
- `simx status --json` returns pool and per-device lease/serve state.
- `--json-errors` returns `{ ok, code, message }` for runtime and argument errors.
- Exit codes match `docs/agent-api.md`.

## Lease And Serve Lifecycle

- Same-slug lease calls are idempotent and renew TTL.
- Expired leases are reaped before status, lease, renew, and serve checks.
- `simx serve --slug <slug>` serves an existing active lease.
- Serve PID, host, and port are tracked in pool state.
- `simx release --slug <slug>` stops a tracked serve process promptly.
- `--serve` exits when the lease expires.

## Stream Stats

`/<slug>/stats` reports:

- target FPS
- lifetime source/sent frames
- rolling 1s/5s source FPS
- rolling 1s/5s sent FPS
- rolling 1s/5s bytes per second
- dropped frames
- connected clients
- controller connected
- latest frame age
- latest send age
- p50/p95 delivery latency

## Multi-Client Behavior

- Multiple clients may connect to the same stream.
- The first WebSocket client is controller.
- Later WebSocket clients are viewer-only.
- Viewer-only input with `ack: true` receives a negative acknowledgement.

## HID v2

- Existing touch/key/Home messages still work.
- Keyboard modifiers are sent as HID modifier key down/up events.
- Paste expands supported text to key events.
- Drag/swipe expands to touch down/move/up.
- Messages with `ack: true` receive success or failure acknowledgements.

## Doctor

- `simx doctor` checks Xcode, `xcrun simctl`, private framework paths, runtime listing, and state path.
- `simx doctor --json` returns all checks and aggregate `ok`.

## Tests And Packaging

- `cargo fmt -- --check` passes.
- `cargo test` passes.
- `SIMX_REAL_SIM_TESTS=1 cargo test --test real_pool` is available for gated local real-simulator testing.
- GitHub Actions runs Linux and macOS checks.
- Tagged releases build and upload the Apple Silicon `simx-aarch64-apple-darwin.tar.gz` artifact.
- `install.sh` installs the latest Apple Silicon release binary into an auto-detected writable bin directory.

## Manual Smoke

Before marking the milestone complete, manually verify locally:

```sh
simx doctor --json
simx init --size 1
simx lease --slug smoke --ttl 2m --json
simx install --slug smoke --app path/to/App.app
simx run --slug smoke
simx serve --slug smoke --port 8080
curl http://127.0.0.1:8080/smoke/stats
simx release --slug smoke
simx clean
```

The browser smoke should confirm:

- `http://127.0.0.1:8080/smoke` renders.
- `ws://127.0.0.1:8080/smoke/stream` emits JPEG binary frames.
- Home, touch, keyboard, paste, and swipe input affect the simulator.
