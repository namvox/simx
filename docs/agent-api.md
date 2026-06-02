# Agent API

This is the stable command surface intended for agents.

## JSON Output

```sh
simx lease --slug agent-a --ttl 10m --json
simx renew --slug agent-a --ttl 10m --json
simx install --slug agent-a --app path/to/App.app --json
simx run --slug agent-a --json
simx status --json
simx doctor --json
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
- `serve.stats`

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
- `project`
- `scheme`
- `configuration`
- `derived_data_path`
- `app`
- `bundle_id`
- `launched`

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

## Serve Lifecycle

Agents should lease first, then serve:

```sh
simx lease --slug agent-a --ttl 10m --json
simx serve --slug agent-a --host 127.0.0.1 --port 8080
```

`simx serve` requires an active lease. It records its PID in pool state. `simx release --slug agent-a` clears the lease and sends `SIGTERM` to the tracked serve process.

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

`simx run` requires an active lease. It validates the current directory has exactly one `.xcodeproj` unless `--project` is provided. It builds the project with `xcodebuild`, targeting the leased simulator UDID, then installs the built `.app` on that simulator, writes `.simx/run.json`, and launches it by default.

Defaults:

- `--scheme`: project file stem, for example `Lumi` from `Lumi.xcodeproj`.
- `--configuration`: `Debug`.
- `--derived-data-path`: `.simx/DerivedData/<slug>`.
- `--bundle-id`: inferred from the built app bundle's `Info.plist`.

Use `--no-launch` to build and install without launching.

`.simx/run.json` is temporary worktree-local state. It records the last run's slug, simulator UDID, project, scheme, derived data path, app bundle, bundle id, launch flag, and update timestamp. Projects should ignore `.simx/` in git.

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
