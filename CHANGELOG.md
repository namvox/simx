# Changelog

## Unreleased

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
