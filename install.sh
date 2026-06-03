#!/usr/bin/env bash
set -euo pipefail

repo="namvox/simx"
asset="simx-aarch64-apple-darwin.tar.gz"
base_url="https://github.com/${repo}/releases/latest/download"

fail() {
  echo "simx install error: $*" >&2
  exit 1
}

case "$(uname -s)" in
  Darwin) ;;
  *) fail "unsupported OS $(uname -s); simx release binaries currently support macOS Apple Silicon only" ;;
esac

case "$(uname -m)" in
  arm64) ;;
  *) fail "unsupported architecture $(uname -m); simx release binaries currently support aarch64-apple-darwin only" ;;
esac

choose_install_dir() {
  if [[ -n "${SIMX_INSTALL_DIR:-}" ]]; then
    echo "$SIMX_INSTALL_DIR"
    return
  fi

  local local_bin="${HOME}/.local/bin"
  if mkdir -p "$local_bin" 2>/dev/null && [[ -w "$local_bin" ]]; then
    echo "$local_bin"
    return
  fi

  if [[ -w "/usr/local/bin" ]]; then
    echo "/usr/local/bin"
    return
  fi

  return 1
}

install_dir="$(choose_install_dir || true)"
if [[ -z "$install_dir" ]]; then
  fail "no writable install directory found. Set SIMX_INSTALL_DIR to a writable bin directory, or manually install the simx binary from ${base_url}/${asset}."
fi

mkdir -p "$install_dir"
[[ -w "$install_dir" ]] || fail "install directory is not writable: $install_dir"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

curl -fsSL "${base_url}/${asset}" -o "${tmpdir}/${asset}"

if curl -fsSL "${base_url}/checksums.txt" -o "${tmpdir}/checksums.txt"; then
  (
    cd "$tmpdir"
    grep " ${asset}$" checksums.txt | shasum -a 256 -c -
  )
else
  echo "warning: checksums.txt was unavailable; installing without checksum verification" >&2
fi

tar -xzf "${tmpdir}/${asset}" -C "$tmpdir"
[[ -x "${tmpdir}/simx" ]] || fail "release archive did not contain an executable simx binary"

cp "${tmpdir}/simx" "${install_dir}/simx"
chmod +x "${install_dir}/simx"

installed_path="${install_dir}/simx"
echo "installed simx to ${installed_path}"

case ":${PATH}:" in
  *":${install_dir}:"*) ;;
  *) echo "PATH hint: add ${install_dir} to PATH to run simx from any shell." ;;
esac
