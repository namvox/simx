# Private Apple API Disclosure

Browser streaming uses private Apple Simulator APIs through
`native/src/simx_bridge.m`.

These APIs are not documented or supported by Apple. They may change or break
across macOS, Xcode, or iOS Simulator updates.

`simx` is intended for local development and agent automation. It is not
designed for production public network exposure.

Streaming binds to `127.0.0.1` by default. If you choose a non-local host, you
are responsible for network isolation and access control. The current browser
stream is unauthenticated.

`--fps 120` is supported as a configurable target, but actual frame rate depends
on Simulator capture, JPEG or H.264 encode, browser decode and render,
transport backpressure, and host machine performance.

`simx` is not affiliated with, endorsed by, or sponsored by Apple Inc. Apple,
iOS, macOS, Xcode, and Simulator-related names are trademarks of Apple Inc.
