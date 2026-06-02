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
simx run --slug browser
simx doctor --json
simx release --slug browser
simx clean
```

Streaming:

```sh
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
- `raw/MindStone` is tracked as a submodule reference source for SimStream behavior.
- Streaming uses CoreSimulator/SimulatorKit private APIs through `native/src/simx_bridge.m`.
- The stream path sends JPEG frames as binary WebSocket messages.
- Browser input sends JSON text messages for touch, keyboard, resume, and Home.
- `simx run --slug ...` validates the current folder has one `.xcodeproj`, builds it for the active lease, installs the built `.app`, infers `CFBundleIdentifier` from `Info.plist`, writes `.simx/run.json`, and launches it by default.
- Do not reintroduce `simctl io screenshot` polling for streaming.

## Development Rules

- Keep the CLI namespace flat: use `simx lease --slug ... --serve`, not `simx pool` or `simx simstream`.
- Prefer focused tests for pool state transitions and WebSocket protocol behavior.
- Before claiming streaming works, manually verify:
  - `cargo test` passes.
  - `simx doctor --json` passes.
  - `http://127.0.0.1:<port>/<slug>` serves the viewer.
  - `ws://127.0.0.1:<port>/<slug>/stream` emits binary JPEG frames.
  - `/<slug>/stats` reports target FPS, frame counts, drops, and latency.
  - No `simctl io screenshot` capture process is running.
  - Touch, keyboard, and Home input affect the simulator.
