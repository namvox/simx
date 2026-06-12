# simx

`simx` is a macOS-only Rust CLI for managing a stable pool of iOS Simulator
devices. It is designed for agent automation first: an agent leases a named
simulator, renews the lease while it works, installs or runs an app, and releases
the simulator when finished. Human iOS developers can use the same commands for
repeatable local workflows.

Browser streaming's stable contract includes the `simx serve` CLI, the default
JPEG-over-WebSocket stream, stats endpoint, WebSocket HID/control protocol, and
control modes. The implementation still uses private Apple CoreSimulator and
SimulatorKit APIs, so runtime compatibility can break across macOS, Xcode, or
iOS Simulator updates, and the browser server is unauthenticated.

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
  build from source on a best-effort basis for now.
- Latest stable Xcode on latest stable macOS is recommended. Recent Xcode
  versions with installed iOS Simulator runtimes are best effort.
- Private Simulator APIs used by streaming may break across macOS, Xcode, and
  iOS Simulator versions.

Check the local machine:

```sh
simx doctor
simx doctor --json
xcode-select -p
xcrun simctl list runtimes
```

See [docs/compatibility.md](docs/compatibility.md) for compatibility details.

## Stability

`simx` uses semantic versioning. The stable surface includes the pool, lease,
serve, release, clean, doctor, run, install, screenshot, record-video, update,
and control commands, plus JSON output for agent-facing commands. Browser streaming's stable surface
includes the serve CLI, default JPEG/WebSocket stream, stats endpoint,
WebSocket HID/control protocol, and control modes. `simx preview` is an
experimental SwiftUI-preview hot-reload workflow. See
[docs/api-stability.md](docs/api-stability.md) for the stable CLI, JSON, lease,
streaming, and HID contracts.

## Install

Install from GitHub with Cargo:

```sh
cargo install --git https://github.com/namvox/simx.git
```

Install from a local checkout:

```sh
cargo install --path .
```

Install from GitHub Releases with curl:

```sh
curl -fsSL https://github.com/namvox/simx/releases/latest/download/install.sh | sh
```

GitHub Releases provide the Apple Silicon binary and install script. See
[docs/release.md](docs/release.md) for the release process.

Check for or install the latest release binary:

```sh
simx update --check
simx update
```

When a newer release is known, normal human-readable commands print a stderr
hint such as:

```text
simx 0.2.1 is available; current version is 0.2.0. Run `simx update` to upgrade.
```

The check is cached for 24 hours and can be disabled with `--no-update-check`
for CI or hermetic agent runs.

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

## SwiftUI Preview Hot Reload

`simx preview` renders Swift Package-backed SwiftUI previews in an active leased
simulator and watches Swift source changes by default:

```sh
simx lease --slug feature-preview --ttl 15m --json
simx preview --slug feature-preview --package Package.swift --package-target App
```

To watch the same simulator in a browser, run `simx serve --slug
feature-preview --port 8080` in another terminal while `simx preview` watches
for source changes.

The command generates a disposable host project under the system temporary
directory, builds and installs that host on the leased simulator, and discovers
`PreviewProvider` and `#Preview` declarations from the selected package target.
On each source edit, it rebuilds a preview dylib, copies it into the running
host app's data container, and notifies the host without relaunching the app.

Useful options:

```sh
simx preview --slug feature-preview --package Package.swift --package-target App --preview-filter StatusRow
simx preview --slug feature-preview --package Package.swift --package-target App --once --json
```

Preview hot reload is experimental. It supports importable Swift Package library
targets and does not edit the package manifest, Xcode project, schemes, or build
settings.

## Browser Streaming

Streaming serves a browser viewer and WebSocket stream for an active lease:

```sh
simx lease --slug browser-preview --ttl 10m --serve --port 8080
```

![simx browser streaming demo](docs/assets/simx-browser-streaming.gif)

Open:

```text
http://127.0.0.1:8080/browser-preview
ws://127.0.0.1:8080/browser-preview/stream
http://127.0.0.1:8080/browser-preview/stats
```

### Stable Default Stream And Control

The stable browser-streaming contract covers the `simx serve` CLI, the default
JPEG-over-WebSocket binary stream at `/<slug>/stream`, `/<slug>/stats`, the
WebSocket HID/control protocol, and `--control-mode` behavior. The default
stream remains compatibility-sensitive because it is backed by private Simulator
APIs, and it is unauthenticated like the rest of the local browser server.

The public default is `--fps 60`. `--fps` is configurable and sets the target
frame pacing used by the server; `--fps 120` remains supported as a
host-dependent target. Actual source and sent frame rates depend on Simulator
behavior, host load, encoding cost, and client/network backpressure. Check
`/<slug>/stats` for current `target_fps`, frame counts, dropped frames, latency,
`source_fps`, and `sent_fps`.

Streams default to `--control-mode read-only`, so browser clients can view the
simulator but cannot send touch, keyboard, or Home input. Start serving with an
explicit write mode when control is required:

```sh
simx serve --slug browser-preview --port 8080 --control-mode single-controller
simx serve --slug browser-preview --port 8080 --control-mode claim
simx serve --slug browser-preview --port 8080 --control-mode shared
```

`single-controller` preserves the original behavior where the first WebSocket
client controls HID and later clients are viewer-only. `claim` lets any client
explicitly claim HID write permission from the viewer. `shared` allows every
connected client to send HID input.

Agent commands can observe and control an active lease without starting
`simx serve`:

```sh
simx control snapshot --slug browser-preview --json
simx control snapshot --slug browser-preview --output snapshot.jpg --json
simx control tap --slug browser-preview --nx 0.5 --ny 0.5 --json
simx control swipe --slug browser-preview --from-nx 0.5 --from-ny 0.8 --to-nx 0.5 --to-ny 0.2 --json
simx control key --slug browser-preview --code KeyA --json
simx control paste --slug browser-preview --text "hello" --json
simx control button --slug browser-preview home --json
simx screenshot --slug browser-preview --output screenshot.png --json
simx record-video --slug browser-preview --output demo.mp4 --duration 10s --json
```

`simx control snapshot --json` is token-efficient by default: it returns frame
metadata, dimensions when available, a hash, and estimated inline-image token
cost without printing base64 image bytes. Use `--output` to write the JPEG frame
or `--inline-base64` only when an inline image payload is required.

Use `simx screenshot` for a one-shot PNG file from `xcrun simctl io screenshot`.
Use `simx record-video` for a bounded MP4 recording; simx stops recording after
`--duration` and waits for `simctl` to finalize the file. Both commands require
an active lease, boot the leased simulator if needed, support `--force` for
overwriting output files, and return file metadata with `--json`.

`simx control` opens a short-lived native SimulatorKit session for the leased
simulator. It does not use the served WebSocket stream, and it does not send
`claimControl`. Stream control modes apply only to browser/WebSocket clients.
`simx control tree` is reserved for a future accessibility snapshot provider and
currently reports that no provider is implemented.

You can also serve an existing active lease:

```sh
simx serve --slug browser-preview --port 8080
```

### Experimental H.264

The hardware H.264/WebCodecs path is experimental and can be served directly
with:

```sh
simx lease --slug browser-preview --ttl 10m --serve --port 8080 --transport h264
```

Open:

```text
http://127.0.0.1:8080/browser-preview?transport=h264
ws://127.0.0.1:8080/browser-preview/h264-stream
```

Treat `--transport h264`, `?transport=h264`, and `/<slug>/h264-stream` as
active-development surfaces until WAN-shaped benchmark evidence is strong. The
route shape, WebSocket message envelope, tuning defaults, and H.264 discovery
details may change before the transport is promoted to a stable contract.

The experimental H.264 path caps encoded width at 640 px to keep VideoToolbox
tail latency bounded for browser streaming. The measured local 60 fps success
profile uses `--transport h264 --fps 70`, which gives the browser enough source
cadence to render at least 60 fps with p95 frame interval at or below 21 ms.
This is a local-loopback tuning profile, not a production transport guarantee.

### WebRTC Prototype

The WebRTC prototype validates browser signaling without replacing the stable
media or HID paths yet:

```sh
simx lease --slug browser-preview --ttl 10m --serve --port 8080 --transport webrtc
```

Open:

```text
http://127.0.0.1:8080/browser-preview?transport=webrtc
http://127.0.0.1:8080/browser-preview/webrtc
POST http://127.0.0.1:8080/browser-preview/webrtc-offer
```

The WebRTC viewer creates an SDP offer and posts it to the signaling endpoint.
For now, valid offers receive a structured `501 Not Implemented` response
because SDP answers, ICE/DTLS/SRTP ownership, H.264 RTP packetization, and RTCP
feedback are not implemented. HID/control remains on the existing WebSocket
stream contract while video transport is evaluated.

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
    "h264_url": "http://127.0.0.1:8080/checkout-tests?transport=h264",
    "h264_stream": "ws://127.0.0.1:8080/checkout-tests/h264-stream",
    "webrtc_url": "http://127.0.0.1:8080/checkout-tests?transport=webrtc",
    "webrtc_signaling": "http://127.0.0.1:8080/checkout-tests/webrtc-offer",
    "stats": "http://127.0.0.1:8080/checkout-tests/stats",
    "control_mode": "read-only"
  },
  "update": {
    "available": true,
    "current_version": "0.2.0",
    "latest_version": "0.2.1",
    "command": "simx update"
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

See [docs/private-apis.md](docs/private-apis.md) for the full private API and
network exposure disclosure.

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
