# Changelog

## Unreleased

- Add `simx screenshot` and `simx record-video` for lease-scoped simulator
  media capture with JSON file metadata.
- Add experimental `simx preview` for Swift Package-backed SwiftUI previews with
  host-app hot reload on an active leased simulator.
- Add H.264 stress-scene benchmark runner coverage for simulator-visible
  static taps, scrolling, keyboard entry, animation-heavy, full-motion, and
  text-heavy scenarios.
- Add WAN benchmark profile metadata and a loopback guard for repeatable H.264
  benchmark reporting.
- Add the experimental WebRTC prototype signaling surface while media delivery
  remains incomplete.
- Clarify that H.264 transport work is experimental and remains outside the
  stable contract until stronger WAN evidence exists.
- Add the production acceptance workflow and checklist for release readiness.

## v0.2.0 - 2026-06-12

- Add `simx control` for native agent snapshots and HID commands,
  with metadata-only snapshot JSON as the token-efficient default.
- Add explicit streaming control modes with read-only as the default, plus
  `single-controller`, `claim`, and `shared` write-access modes.
- Add `simx update` to check for and install GitHub Release binaries.
- Add cached latest-version hints so older clients encourage agents to run `simx update`.

## v0.1.2 - 2026-06-03

- Add HID long-press scroll input for upward, downward, leftward, and rightward scroll gestures.
- Fix browser viewer swipes so pointer movement sends continuous HID touch move events and missed releases cancel cleanly.

## v0.1.1 - 2026-06-03

- Update public install URLs and release installer to `namvox/simx`.

## v0.1.0 - 2026-06-03

- Simulator pool lifecycle.
- Slug-based leasing with TTL and renew.
- Browser/WebSocket streaming.
- HID input contract.
- App install/run helpers.
- Doctor/status commands.
- Stable CLI/JSON contract for agent-facing commands.
- Default streaming target FPS set to 60, with 120 FPS supported as a host-dependent target.
