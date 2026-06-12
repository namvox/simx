# Release Process

`simx` uses GitHub Releases for prebuilt binaries and source installs through
Cargo.

## Install Paths

Source install:

```sh
cargo install --git https://github.com/namvox/simx.git
```

Release binary install:

```sh
curl -fsSL https://github.com/namvox/simx/releases/latest/download/install.sh | sh
```

Release binary update:

```sh
simx update --check
simx update
```

Release binaries are Apple Silicon first. Homebrew support is not documented
until it exists.

## Release Checklist

Before tagging a release:

- MIT `LICENSE` is present.
- `README.md` documents install, compatibility, security, and private API risk.
- `SECURITY.md` names the disclosure contact.
- CI is present for Linux non-simulator checks and macOS checks.
- Secret/history scans have been run.
- `raw/MindStone` is not present.
- `Cargo.toml`, `Cargo.lock`, and `CHANGELOG.md` agree on the release version.
- `CHANGELOG.md` has a dated release section for the version being tagged.
- The browser streaming demo GIF exists at
  `docs/assets/simx-browser-streaming.gif`.
- Release notes do not describe H.264/WebCodecs as stable unless
  [api-stability.md](api-stability.md) has promoted `--transport h264`,
  `?transport=h264`, and `/<slug>/h264-stream` from experimental status.
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
git tag v0.2.0
git push origin v0.2.0
```

Pushing the tag runs `.github/workflows/release.yml`, which builds and uploads:

```text
simx-aarch64-apple-darwin.tar.gz
checksums.txt
install.sh
```

`simx update` expects the release archive and `checksums.txt` names above. If
the artifact names change, update the command implementation and
[docs/api-stability.md](api-stability.md) in the same PR.

## Demo

The current demo asset is:

```text
docs/assets/simx-browser-streaming.gif
```

It should show the browser viewer connected to a local simulator stream. A later
release can replace it with a richer app-screen demo.
