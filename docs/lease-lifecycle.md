# Lease Lifecycle

`simx` keeps a stable pool of iOS Simulator devices and assigns temporary
ownership with leases. The model is intended for agents first: every agent uses a
descriptive slug, receives a stable simulator UDID, renews while active, and
releases when finished.

## Stable Pool Devices

Pool devices are created with stable names:

```text
simx-pool-001
simx-pool-002
simx-pool-003
```

The size is set when initializing the pool:

```sh
simx init --size 2
```

`simx status` reports each pool device, its UDID, current slug owner, expiry, and
serve process metadata when present.

## State And Locking

Pool state lives at:

```text
~/Library/Application Support/simx/pool.json
```

Reads and writes are protected by a file lock next to the state file. This lets
multiple agent processes call `simx lease`, `simx renew`, `simx status`, and
`simx release` without racing the JSON state update.

## Slug Ownership

Leases are keyed by slug. The same slug is the same owner.

```sh
simx lease --slug checkout-tests --ttl 10m --json
simx lease --slug checkout-tests --ttl 10m --json
```

If `checkout-tests` already owns an active lease, the second command returns the
same simulator and extends the expiry. It does not allocate a second simulator.

Different slugs are competing owners:

```sh
simx lease --slug checkout-tests --ttl 10m
simx lease --slug onboarding-smoke --ttl 10m
```

`onboarding-smoke` cannot take the simulator owned by `checkout-tests` until that
lease is released, expires and is reaped, or the pool has another available
device.

## TTL And Reaping

Every lease has a TTL. The default is 30 minutes.

```sh
simx lease --slug checkout-tests --ttl 10m
simx renew --slug checkout-tests --ttl 10m
```

Expired leases are reaped before status, lease, renew, and serve decisions.
Reaping clears the slug ownership and makes the device available for another
lease.

Non-booted leases with no tracked serve process are also reclaimed before status
and lease decisions. This keeps agents from getting stuck behind a stale claim
when CoreSimulator no longer has the leased device booted.

Use `lease` when an agent is allowed to reacquire a simulator after its previous
lease expired. Use `renew` when expiration should be treated as a hard ownership
loss.

## Release

Release clears ownership for the slug:

```sh
simx release --slug checkout-tests
```

The simulator remains in the pool and may stay booted. If a serve process was
registered for the lease, release sends `SIGTERM` to that process and clears the
serve metadata from pool state.

## Serve Lifecycle

Serving requires an active lease:

```sh
simx lease --slug browser-preview --ttl 10m --json
simx serve --slug browser-preview --port 8080
```

or:

```sh
simx lease --slug browser-preview --ttl 10m --serve --port 8080
```

`simx serve` records its PID, host, and port in pool state. The serve loop checks
lease state and stops when the lease is no longer active.

Per [api-stability.md](api-stability.md), `simx serve`, the default JPEG
viewer/stream route, the stats route, and the WebSocket HID/control message
contract are stable. Streaming still uses private Apple Simulator APIs, is
unauthenticated, and defaults to `127.0.0.1`; use separate network isolation
before binding serve hosts to non-local interfaces. H.264 and WebRTC transport
surfaces remain experimental/prototype-only.

## JSON Shapes

Lease and renew return the same shape with `--json`:

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
  }
}
```

Status returns the pool and per-device lease state:

```sh
simx status --json
```

Use [api-stability.md](api-stability.md) for the stable command and protocol
surface, [agent-api.md](agent-api.md) for the stable machine-readable command
surface, and [lease-contract.md](lease-contract.md) for the concise command
contract.
