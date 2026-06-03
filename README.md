# simx

`simx` is a macOS-only Rust CLI for managing a stable pool of iOS Simulator
devices. It is designed for agent automation first: an agent leases a named
simulator, renews the lease while it works, installs or runs an app, and releases
the simulator when finished. Human iOS developers can use the same commands for
repeatable local workflows.

Browser streaming is experimental. It uses private Apple CoreSimulator and
SimulatorKit APIs, so behavior can break across macOS, Xcode, or iOS Simulator
updates.

## Agent-First Quickstart

Use descriptive slugs that identify the job or workflow:

```sh
simx init --size 2
simx lease --slug checkout-tests --ttl 10m --json
simx run --slug checkout-tests --json
simx renew --slug checkout-tests --ttl 10m --json
simx release --slug checkout-tests
```

For an already-built app bundle:

```sh
simx lease --slug onboarding-smoke --ttl 10m --json
simx install --slug onboarding-smoke --app build/Debug-iphonesimulator/App.app --json
simx release --slug onboarding-smoke
```

## Requirements

Conservative minimum requirements:

- macOS only.
- Full Xcode installed, not just Command Line Tools.
- An iOS Simulator runtime installed.
- Rust stable toolchain for source installs.
- Release binaries are Apple Silicon first. Other macOS architectures should
  build from source for now.
- Private Simulator APIs used by streaming may break across macOS, Xcode, and
  iOS Simulator versions.

Check the local machine:

```sh
simx doctor
simx doctor --json
```

## Stability

`simx` uses semantic versioning. The `v0.1.0` stable surface includes the pool,
lease, serve, release, clean, doctor, run, and install commands, plus JSON output
for agent-facing commands. See [docs/api-stability.md](docs/api-stability.md)
for the stable CLI, JSON, lease, streaming, and HID contracts.

## Install

Install from GitHub with Cargo:

```sh
cargo install --git https://github.com/boncasa/simx.git
```

Install from a local checkout:

```sh
cargo install --path .
```

Install from GitHub Releases with curl:

```sh
curl -fsSL https://github.com/boncasa/simx/releases/latest/download/install.sh | sh
```

GitHub Releases need to be set up before the curl installer works. The release
should publish an `install.sh` asset and any binaries that script downloads.

## Pool Lifecycle

Initialize a stable simulator pool:

```sh
simx init --size 2
```

Pool devices are named `simx-pool-001`, `simx-pool-002`, and so on. Pool state
lives at:

```text
~/Library/Application Support/simx/pool.json
```

Inspect or clean the pool:

```sh
simx status
simx status --json
simx clean
```

`simx clean` shuts down and deletes pool devices, then removes the pool state.

## Lease Lifecycle

Leases are keyed by `slug`. The same slug is the same owner, so agents should use
human-readable slugs such as `checkout-tests`, `design-review`, or
`onboarding-smoke`.

```sh
simx lease --slug checkout-tests --ttl 10m
simx renew --slug checkout-tests --ttl 10m
simx release --slug checkout-tests
```

Lease behavior:

- `simx lease` returns an existing active lease for the same slug and extends its
  TTL.
- `simx renew` extends an active lease and fails if the lease is missing or
  expired.
- Expired leases are reaped before status, lease, renew, and serve decisions.
- Other slugs cannot use a simulator until the current lease is released,
  expires, or is reaped.
- `simx release` clears the lease and stops any tracked serve process for it.

See [docs/lease-lifecycle.md](docs/lease-lifecycle.md) for the detailed model.

## App Workflow

Build, install, and launch the app in the current Xcode project:

```sh
simx lease --slug feature-preview --ttl 15m --json
simx run --slug feature-preview --json
```

`simx run` requires an active lease. It validates that the current folder has one
`.xcodeproj` unless `--project` is provided, builds quietly for the leased
simulator, writes build logs under `.simx/logs/`, installs the built `.app`,
writes `.simx/run.json`, and launches by default.

Useful options:

```sh
simx run --slug feature-preview --project App.xcodeproj --scheme App --configuration Debug
simx run --slug feature-preview --no-launch
```

Install an existing `.app` bundle:

```sh
simx install --slug feature-preview --app path/to/App.app --json
simx install --slug feature-preview --app path/to/App.app --bundle-id com.example.App --no-launch
```

`simx install` infers `CFBundleIdentifier` from `Info.plist` when `--bundle-id`
is omitted and launches by default.

## Experimental Streaming

Streaming serves a browser viewer and WebSocket stream for an active lease:

```sh
simx lease --slug browser-preview --ttl 10m --serve --port 8080
```

Open:

```text
http://127.0.0.1:8080/browser-preview
ws://127.0.0.1:8080/browser-preview/stream
http://127.0.0.1:8080/browser-preview/stats
```

The public default is `--fps 60`. `--fps` is configurable and sets the target
frame pacing used by the server; `--fps 120` remains supported as a
host-dependent target. Actual source and sent frame rates depend on Simulator
behavior, host load, encoding cost, and client/network backpressure. Check
`/<slug>/stats` for current `target_fps`, frame counts, dropped frames, latency,
`source_fps`, and `sent_fps`.

You can also serve an existing active lease:

```sh
simx serve --slug browser-preview --port 8080
```

## JSON Output For Agents

Lease output:

```sh
simx lease --slug checkout-tests --ttl 10m --json
```

```json
{
  "slug": "checkout-tests",
  "udid": "1DF0F390-70FB-402D-BC19-47DA36F3F1F9",
  "device_name": "simx-pool-001",
  "lease_expires_at": 1780239000,
  "lease_expires_at_rfc3339": "2026-06-03T08:10:00Z",
  "ttl_seconds": 600,
  "serve": {
    "command": "simx serve --slug checkout-tests --host 127.0.0.1 --port 8080",
    "url": "http://127.0.0.1:8080/checkout-tests",
    "stream": "ws://127.0.0.1:8080/checkout-tests/stream",
    "stats": "http://127.0.0.1:8080/checkout-tests/stats"
  }
}
```

Run output:

```sh
simx run --slug checkout-tests --json
```

```json
{
  "slug": "checkout-tests",
  "udid": "1DF0F390-70FB-402D-BC19-47DA36F3F1F9",
  "run_state": ".simx/run.json",
  "log": ".simx/logs/checkout-tests-xcodebuild.log",
  "project": "App.xcodeproj",
  "scheme": "App",
  "configuration": "Debug",
  "derived_data_path": ".simx/DerivedData/checkout-tests",
  "app": ".simx/DerivedData/checkout-tests/Build/Products/Debug-iphonesimulator/App.app",
  "bundle_id": "com.example.App",
  "launched": true
}
```

Use `--json-errors` with any command for machine-readable runtime and argument
errors:

```sh
simx --json-errors lease --slug checkout-tests --ttl 0s --json
```

More details are in [docs/agent-api.md](docs/agent-api.md).

## Security And Private API Notes

`simx` is intended for local development and agent automation. It binds to
`127.0.0.1` by default.

Do not expose `simx` streaming ports to public networks. Browser streaming is
unauthenticated in the current version and can expose simulator screen contents,
typed text, app data, keyboard input, and HID actions.

If you use `--host 0.0.0.0` or any other non-local host, you are responsible for
network isolation.

Streaming uses private Apple CoreSimulator and SimulatorKit APIs through
`native/src/simx_bridge.m`. These APIs are undocumented and may change or break
without notice. Use streaming only where private API use is acceptable for your
workflow.

`simx` is not affiliated with, endorsed by, or sponsored by Apple Inc. Apple,
iOS, macOS, Xcode, and Simulator-related names are trademarks of Apple Inc.

Report vulnerabilities using [SECURITY.md](SECURITY.md).

## Troubleshooting

Start with:

```sh
simx doctor --json
simx status --json
```

Common pointers:

- If Xcode checks fail, install full Xcode and run `sudo xcode-select -s
  /Applications/Xcode.app/Contents/Developer`.
- If no iOS runtime is available, install one from Xcode Settings.
- If the pool looks stale, run `simx status` to reap expired leases or
  `simx clean && simx init --size 2` to recreate the pool.
- If `simx run` fails, inspect the returned `.simx/logs/<slug>-xcodebuild.log`.
- If streaming does not work after an Xcode or macOS update, rerun
  `simx doctor --json` and treat private API compatibility as suspect.

## License

`simx` is licensed under the MIT License. See [LICENSE](LICENSE).
