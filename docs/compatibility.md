# Compatibility

`simx` is a macOS-only CLI.

## Supported Platform

- macOS is required.
- Full Xcode is required. Command Line Tools alone are not enough.
- An installed iOS Simulator runtime is required.
- Rust stable is required for source installs.

Use `simx doctor --json` to check a host before assigning agent work to it.

## Architecture

Release binaries are Apple Silicon first:

```text
aarch64-apple-darwin
```

Intel macOS may build from source, but it is best effort.

## Xcode And macOS

Latest stable Xcode on latest stable macOS is recommended. Recent Xcode versions
with installed iOS Simulator runtimes are best effort.

Streaming uses private Apple Simulator APIs, so compatibility can change across
macOS, Xcode, and iOS Simulator updates even when the CLI still builds.

## Self-Diagnosis

Start with:

```sh
simx doctor
simx doctor --json
```

Useful host checks:

```sh
xcode-select -p
xcrun simctl help
xcrun simctl list runtimes
simx status --json
```

If Xcode is installed but `xcode-select` points elsewhere:

```sh
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
```
