# Release Process

`simx` uses GitHub Releases for prebuilt binaries and source installs through
Cargo.

## Install Paths

Source install:

```sh
cargo install --git https://github.com/boncasa/simx.git
```

Release binary install:

```sh
curl -fsSL https://github.com/boncasa/simx/releases/latest/download/install.sh | sh
```

`v0.1.0` release binaries are Apple Silicon first. Homebrew support is not
documented until it exists.

## v0.1.0 Checklist

Before tagging `v0.1.0`:

- MIT `LICENSE` is present.
- `README.md` documents install, compatibility, security, and private API risk.
- `SECURITY.md` names the disclosure contact.
- CI is present for Linux non-simulator checks and macOS checks.
- Secret/history scans have been run.
- `raw/MindStone` is not present.
- `CHANGELOG.md` has a `v0.1.0` section.
- The browser streaming demo GIF exists at
  `docs/assets/simx-browser-streaming.gif`.
- Checks pass:

```sh
make check
make release-dry-run
simx doctor --json
gitleaks detect --source .
rg -n "token|secret|password|api[_-]?key|PRIVATE KEY|BEGIN .*KEY|ghp_|sk-"
```

## Tagging

After the checklist passes:

```sh
git tag v0.1.0
git push origin v0.1.0
```

Pushing the tag runs `.github/workflows/release.yml`, which builds and uploads:

```text
simx-aarch64-apple-darwin.tar.gz
checksums.txt
install.sh
```

## Demo

The current demo asset is:

```text
docs/assets/simx-browser-streaming.gif
```

It should show the browser viewer connected to a local simulator stream. A later
release can replace it with a richer app-screen demo.
