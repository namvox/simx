# Agent API

This is the stable command surface intended for agents.

## JSON Output

```sh
simx lease --slug agent-a --ttl 10m --json
simx renew --slug agent-a --ttl 10m --json
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
