# Simulator HID WebSocket Contract

This document describes the JSON text messages a browser client sends over the `simx` stream WebSocket to control the leased simulator.

The core `v0.1.0` control families are stable: touch, keyboard, Home, and
resume. On the wire these are represented as `type: "touch"`, `type: "key"`,
`type: "button"` with `button: "home"`, and `type: "resume"`. Additive fields
and new message types may be added in minor releases.

## Endpoint

```text
ws://<host>:<port>/<slug>/stream
```

Example:

```text
ws://127.0.0.1:8080/browser-preview/stream
```

The same WebSocket is used for both directions:

- Server to client: binary JPEG frame messages and JSON status messages.
- Client to server: JSON text messages for HID input and stream resume.

All client-to-server messages must be UTF-8 JSON objects with a string `type` field. Unknown message types are ignored.

## Resume

The server can pause frame delivery after the configured idle timeout. A client resumes by sending:

```json
{
  "type": "resume"
}
```

Server responses:

```json
{
  "type": "paused",
  "reason": "idle_timeout"
}
```

```json
{
  "type": "resumed"
}
```

Any non-resume input message also counts as client activity.

## Touch

Touch messages use normalized screen coordinates.

```json
{
  "type": "touch",
  "phase": "began",
  "id": 0,
  "nx": 0.5,
  "ny": 0.5,
  "pressure": 1
}
```

Fields:

- `type`: must be `"touch"`.
- `phase`: `"began"`, `"moved"`, `"ended"`, or `"cancelled"`.
- `id`: pointer identifier. Currently accepted for compatibility; the native bridge sends a single active touch stream.
- `nx`: normalized horizontal coordinate from `0.0` left to `1.0` right.
- `ny`: normalized vertical coordinate from `0.0` top to `1.0` bottom.
- `pressure`: optional pressure value. Currently accepted for compatibility; the native bridge only uses down/up state.

Behavior:

- `began` and `moved` send touch-down/move state.
- `ended` and `cancelled` send touch-up state.
- Coordinates are clamped to `0.0..=1.0`.

Tap example:

```json
{ "type": "touch", "phase": "began", "id": 1, "nx": 0.85, "ny": 0.46, "pressure": 1 }
```

```json
{ "type": "touch", "phase": "ended", "id": 1, "nx": 0.85, "ny": 0.46, "pressure": 0 }
```

## Drag And Swipe

`drag` and `swipe` are high-level helpers that expand into touch-down, move, and touch-up HID messages.

```json
{
  "type": "swipe",
  "id": "gesture-1",
  "ack": true,
  "from": { "nx": 0.5, "ny": 0.8 },
  "to": { "nx": 0.5, "ny": 0.2 },
  "steps": 12
}
```

Fields:

- `type`: `"drag"` or `"swipe"`.
- `from`: object with `nx` and `ny` start coordinates.
- `to`: object with `nx` and `ny` end coordinates.
- `steps`: optional number of move steps, clamped to `2..=60`.

## Keyboard

Keyboard messages use browser `KeyboardEvent.code` values. `simx` maps those codes to USB HID usage IDs before sending them through SimulatorKit.

```json
{
  "type": "key",
  "phase": "down",
  "key": "a",
  "code": "KeyA",
  "repeat": false,
  "modifiers": {
    "shift": false,
    "control": false,
    "option": false,
    "command": false
  }
}
```

Fields:

- `type`: must be `"key"`.
- `phase`: `"down"` or `"up"`.
- `code`: browser physical key code. This is the authoritative field used by the server.
- `key`: browser character value. Currently accepted for compatibility and debugging; not used for mapping.
- `repeat`: browser repeat flag. Currently accepted for compatibility; repeat behavior is not special-cased.
- `modifiers`: current modifier state. Currently accepted for compatibility; modifier auto-chording is not implemented yet.
- `ack`: optional boolean. When `true`, the server replies with an acknowledgement.

Supported `code` values:

```text
KeyA..KeyZ
Digit0..Digit9
Enter
Escape
Backspace
Tab
Space
Minus
Equal
BracketLeft
BracketRight
Backslash
Semicolon
Quote
Backquote
Comma
Period
Slash
ArrowRight
ArrowLeft
ArrowDown
ArrowUp
```

Typing `m` example:

```json
{ "type": "key", "phase": "down", "key": "m", "code": "KeyM", "repeat": false }
```

```json
{ "type": "key", "phase": "up", "key": "m", "code": "KeyM", "repeat": false }
```

Modifier example:

```json
{
  "type": "key",
  "id": "shift-a",
  "ack": true,
  "phase": "down",
  "key": "A",
  "code": "KeyA",
  "modifiers": { "shift": true }
}
```

## Paste Text

Paste expands supported characters into key down/up events.

```json
{
  "type": "paste",
  "id": "paste-1",
  "ack": true,
  "text": "maps"
}
```

Supported pasted characters currently include ASCII letters, digits, space, newline, and common punctuation: `- _ = + , < . > / ?`.

## Hardware Buttons

Home button:

```json
{
  "type": "button",
  "button": "home"
}
```

Only `home` is currently supported. The native bridge tries the same SimulatorKit HID strategies used by the MindStone SimStream reference:

- Consumer-control Menu usage.
- Consumer-control Home usage.
- Legacy hardware button fallback targets.

## Acknowledgements

Messages with `"ack": true` receive a JSON text response:

```json
{
  "type": "ack",
  "id": "paste-1",
  "ok": true,
  "message": "ok"
}
```

If a viewer-only client sends input while another client owns control, the server replies:

```json
{
  "type": "ack",
  "id": "paste-1",
  "ok": false,
  "message": "client is viewer-only"
}
```

## Multi-Client Control

Multiple clients can connect to one stream. The first WebSocket client becomes the controller and may send HID input. Later clients are viewers and receive frames but cannot control the simulator until the controller disconnects and they reconnect.

The server sends a role message on connect:

```json
{ "type": "client", "role": "controller" }
```

or:

```json
{ "type": "client", "role": "viewer" }
```

## Error Handling

The server currently logs malformed JSON, unsupported key codes, or native HID failures to stderr and keeps the WebSocket open when possible.

The server does not currently send per-input success or error acknowledgements to the browser. Clients should treat the stream as best-effort input transport and use the incoming JPEG frames to observe effects.

## Compatibility Notes

- The contract is slug scoped; a client must connect to the same `<slug>` route that was used for `simx lease --slug <slug> --serve`.
- HID delivery depends on macOS, Xcode, CoreSimulator, and SimulatorKit private API compatibility.
- The WebSocket stream sends simulator frames as binary JPEG messages. Client JSON messages must be text frames, not binary frames.
- Do not use `simctl io screenshot` as part of the streaming path.
