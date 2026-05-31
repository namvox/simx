# Lease Contract

`simx` leases are agent claims on simulator pool devices. A claim is identified by `slug`.

## Core Rule

The same `slug` is the same owner.

```sh
simx lease --slug agent-a
simx lease --slug agent-a --ttl 10m --json
```

Both commands operate on the same claim. If `agent-a` already has an active lease, the second command returns the same UDID and extends the lease TTL instead of treating the simulator as busy.

Different slugs are competing owners.

## Lease

```sh
simx lease --slug <slug> [--ttl <duration>] [--wait-timeout <duration>] [--json]
```

Defaults:

- `--ttl 30m`
- `--wait-timeout 60s`

Behavior:

- If `<slug>` owns an active lease, return the same simulator and renew `lease_expires_at`.
- If `<slug>` owns an expired lease, the expired lease is reclaimed first, then normal leasing runs.
- If another slug owns an active lease, that simulator is busy.
- If another slug owns an expired lease, that simulator is reclaimed and can be leased.
- If the pool is full, wait until `--wait-timeout`, then fail.
- Allocated simulators are booted if needed.

Plain output puts the UDID first for copy/paste compatibility, then prints lease metadata and a serve command.

Machine-readable output:

```sh
simx lease --slug agent-a --ttl 10m --json
```

```json
{
  "slug": "agent-a",
  "udid": "1DF0F390-70FB-402D-BC19-47DA36F3F1F9",
  "device_name": "simx-pool-002",
  "lease_expires_at": 1780239000,
  "ttl_seconds": 600,
  "serve": {
    "command": "simx lease --slug agent-a --serve --host 127.0.0.1 --port 8080",
    "url": "http://127.0.0.1:8080/agent-a",
    "stream": "ws://127.0.0.1:8080/agent-a/stream",
    "stats": "http://127.0.0.1:8080/agent-a/stats"
  }
}
```

## Renew

```sh
simx renew --slug <slug> [--ttl <duration>] [--json]
```

Defaults:

- `--ttl 30m`

Behavior:

- Extends only an active lease for the same slug.
- Fails if the slug is missing.
- Fails if the slug's lease has already expired.
- Returns the same output shape as `lease`.

Agents can use either command as a heartbeat:

```sh
simx lease --slug agent-a --ttl 10m --json
```

or:

```sh
simx renew --slug agent-a --ttl 10m --json
```

Use `lease` when the agent is allowed to reclaim a simulator if its previous lease expired. Use `renew` when expiration should be treated as a hard ownership loss.

## Serve

```sh
simx lease --slug <slug> --serve [--ttl <duration>]
```

Behavior:

- Claims or reuses the lease.
- Starts the HTTP/WebSocket viewer.
- Stops serving when the lease is released or expires.

## Release

```sh
simx release --slug <slug>
```

Behavior:

- Clears the active lease for the slug.
- Keeps the simulator booted.
- Stops a matching `--serve` process on its next lease check.

## Status

```sh
simx status
```

Behavior:

- Reaps expired leases before printing.
- Prints each device with owner and expiry. Idle devices show `idle -`.

## Clean

```sh
simx clean
```

Behavior:

- Shuts down and deletes all pool devices.
- Removes the pool state file.
