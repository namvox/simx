# simx

`simx` is a macOS-only Rust CLI for managing a stable iOS Simulator pool and serving a leased simulator through a browser/WebSocket interface.

## Commands

```sh
cargo fmt
cargo test
cargo install --path .
```

Pool lifecycle:

```sh
simx init --size 2
simx status
simx lease --slug browser --ttl 10m
simx lease --slug browser --ttl 10m --json
simx renew --slug browser --ttl 10m
simx serve --slug browser --port 8080
simx install --slug browser --app path/to/App.app
simx run --slug browser
simx doctor --json
simx release --slug browser
simx clean
```

Streaming:

```sh
simx lease --slug browser --serve --port 8080 --idle-timeout 5m
simx lease --slug browser --serve --port 8080 --fps 120 --idle-timeout 5m
```

Open:

```text
http://127.0.0.1:8080/browser
ws://127.0.0.1:8080/browser/stream
http://127.0.0.1:8080/browser/stats
```

## Implementation Notes

- Pool state lives at `~/Library/Application Support/simx/pool.json`.
- Pool state reads/writes use a file lock next to the state file.
- Pool devices are named `simx-pool-001`, `simx-pool-002`, and so on.
- Lease ownership is keyed by `slug`; the same slug can renew and reuse its active lease.
- Leases have TTLs. Expired leases are reaped before status, lease, renew, and serve decisions.
- Non-booted unserved leases are reclaimed before status and lease decisions.
- Streaming uses CoreSimulator/SimulatorKit private APIs through `native/src/simx_bridge.m`.
- Streaming defaults to `--fps 60`; `--fps 120` is supported as a host-dependent target.
- The stream path sends JPEG frames as binary WebSocket messages.
- Browser input sends JSON text messages for touch, keyboard, resume, and Home.
- `simx install --slug ... --app ...` installs a `.app` bundle on the active lease, infers `CFBundleIdentifier` from `Info.plist`, and launches it by default.
- `simx run --slug ...` validates the current folder has one `.xcodeproj`, builds it quietly for the active lease, writes the build log under `.simx/logs/`, installs the built `.app`, infers `CFBundleIdentifier` from `Info.plist`, writes `.simx/run.json`, and launches it by default.
- Do not reintroduce `simctl io screenshot` polling for streaming.

## Development Rules

- Keep the CLI namespace flat: use `simx lease --slug ... --serve`, not `simx pool` or `simx simstream`.
- Prefer focused tests for pool state transitions and WebSocket protocol behavior.
- For every pull request, follow `.github/PULL_REQUEST_TEMPLATE.md`. The PR body should explicitly cover Summary, Type, Stable Contract Impact, Verification, Simulator And Streaming Notes, Screenshots Or Demo, and Release Notes.
- Stable CLI, JSON, WebSocket, and HID changes must reference `docs/api-stability.md` and update the relevant docs in the same PR.
- When changing the command interface, update the relevant `--help` text in the same PR. This includes adding, removing, renaming, or changing commands, flags, defaults, JSON-facing options, server URLs, transport/control modes, and common workflows.
- Agent-facing workflows should be discoverable from `simx --help` or `simx <command> --help`; do not rely only on external docs for basic usage.
- Verification for command interface changes should include checking `simx --help` and each touched subcommand's `--help` output.
- Verification should match the change:
  - Documentation/template-only changes may say no Rust checks were required.
  - Code changes should run `cargo fmt --check`, `cargo test`, and `cargo clippy -- -D warnings`, or `make check`.
  - Release/install changes should also run `make release-dry-run` or explain why it does not apply.
  - Simulator/streaming changes should include `simx doctor --json` and the manual browser checks below when feasible.
- Release Notes in the PR must say whether `CHANGELOG.md` was updated. If not, explain why the change does not need a changelog entry.
- Before claiming streaming works, manually verify:
  - `cargo test` passes.
  - `simx doctor --json` passes.
  - `http://127.0.0.1:<port>/<slug>` serves the viewer.
  - `ws://127.0.0.1:<port>/<slug>/stream` emits binary JPEG frames.
  - `/<slug>/stats` reports target FPS, frame counts, drops, and latency.
  - No `simctl io screenshot` capture process is running.
  - Touch, keyboard, and Home input affect the simulator.
