#!/usr/bin/env bash
set -uo pipefail

SCRIPT_NAME="$(basename "$0")"
EVIDENCE_DIR=".simx/production-acceptance/$(date -u +%Y%m%dT%H%M%SZ)"
RUN_AUTOMATED=1
RUN_REAL_SIM=0
RUN_DOCTOR=0
RUN_RELEASE=0
PRINT_MANUAL=0
SIMX_BIN="${SIMX_BIN:-simx}"

usage() {
  cat <<EOF
Usage: $SCRIPT_NAME [OPTIONS]

Run the simx production acceptance checklist and record command evidence.

Safe default:
  $SCRIPT_NAME
    Runs cargo fmt --check, cargo test, and cargo clippy -- -D warnings.

Options:
  --all-local          Run automated, real-simulator, doctor, and release checks.
  --real-sim          Run SIMX_REAL_SIM_TESTS=1 cargo test --test real_pool.
  --doctor            Run "\$SIMX_BIN doctor --json" and record JSON output.
  --release-dry-run   Run make release-dry-run.
  --manual-plan       Print manual browser/streaming evidence to collect.
  --evidence-dir DIR  Write logs and summary under DIR.
  --simx-bin PATH     simx executable for doctor checks (default: simx).
  -h, --help          Show this help.

This script never runs simx clean, deletes simulator devices, tags a release,
or pushes to GitHub. Simulator cleanup and release publishing remain explicit
operator actions.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --all-local)
      RUN_AUTOMATED=1
      RUN_REAL_SIM=1
      RUN_DOCTOR=1
      RUN_RELEASE=1
      ;;
    --real-sim)
      RUN_REAL_SIM=1
      ;;
    --doctor)
      RUN_DOCTOR=1
      ;;
    --release-dry-run)
      RUN_RELEASE=1
      ;;
    --manual-plan)
      PRINT_MANUAL=1
      ;;
    --evidence-dir)
      if [ "$#" -lt 2 ]; then
        echo "--evidence-dir requires a directory" >&2
        exit 2
      fi
      EVIDENCE_DIR="$2"
      shift
      ;;
    --simx-bin)
      if [ "$#" -lt 2 ]; then
        echo "--simx-bin requires an executable path" >&2
        exit 2
      fi
      SIMX_BIN="$2"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

mkdir -p "$EVIDENCE_DIR"
SUMMARY_FILE="$EVIDENCE_DIR/summary.md"
COMMANDS_FILE="$EVIDENCE_DIR/commands.log"
FAILURES=0

write_header() {
  {
    echo "# simx production acceptance evidence"
    echo
    echo "- Started: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "- Evidence directory: $EVIDENCE_DIR"
    echo
    echo "## Results"
  } > "$SUMMARY_FILE"
  : > "$COMMANDS_FILE"
}

shell_quote() {
  printf "%q" "$1"
}

record_command() {
  local name="$1"
  shift
  {
    echo "[$name]"
    printf "\$"
    for arg in "$@"; do
      printf " "
      shell_quote "$arg"
    done
    echo
    echo
  } >> "$COMMANDS_FILE"
}

run_step() {
  local name="$1"
  shift
  local stdout_log="$EVIDENCE_DIR/$name.stdout.log"
  local stderr_log="$EVIDENCE_DIR/$name.stderr.log"

  echo "==> $name"
  record_command "$name" "$@"

  if "$@" > >(tee "$stdout_log") 2> >(tee "$stderr_log" >&2); then
    echo "- [x] \`$name\` passed. Logs: \`$stdout_log\`, \`$stderr_log\`." >> "$SUMMARY_FILE"
  else
    local status=$?
    echo "- [ ] \`$name\` failed with exit $status. Logs: \`$stdout_log\`, \`$stderr_log\`." >> "$SUMMARY_FILE"
    FAILURES=$((FAILURES + 1))
  fi
}

write_manual_plan() {
  cat <<'EOF'

Manual browser/streaming evidence to add to the release checklist:

- Start a served lease without destructive cleanup:
  simx lease --slug acceptance-smoke --ttl 10m --serve --port 8080 --control-mode single-controller
- Open http://127.0.0.1:8080/acceptance-smoke and record that the viewer renders.
- Connect ws://127.0.0.1:8080/acceptance-smoke/stream and record that binary JPEG frames arrive.
- Fetch http://127.0.0.1:8080/acceptance-smoke/stats and save the JSON output.
- Confirm stats include target FPS, source/sent frame counts, dropped frames, connected clients, controller state, frame age, send age, and p50/p95 latency.
- Confirm no simctl io screenshot polling process is running:
  pgrep -af "simctl.*io.*screenshot"
- Verify Home, touch, keyboard, paste, drag/swipe, and acked negative read-only HID behavior.
- Release the lease when finished:
  simx release --slug acceptance-smoke
EOF
}

write_header

if [ "$RUN_AUTOMATED" -eq 1 ]; then
  run_step cargo-fmt-check cargo fmt --check
  run_step cargo-test cargo test
  run_step cargo-clippy cargo clippy -- -D warnings
fi

if [ "$RUN_REAL_SIM" -eq 1 ]; then
  run_step real-pool env SIMX_REAL_SIM_TESTS=1 cargo test --test real_pool
else
  echo "- [ ] \`SIMX_REAL_SIM_TESTS=1 cargo test --test real_pool\` not run. Use \`--real-sim\` on a host where real simulator mutation is acceptable." >> "$SUMMARY_FILE"
fi

if [ "$RUN_DOCTOR" -eq 1 ]; then
  run_step simx-doctor-json "$SIMX_BIN" doctor --json
else
  echo "- [ ] \`simx doctor --json\` not run. Use \`--doctor\` with \`--simx-bin\` if the candidate binary is not on PATH." >> "$SUMMARY_FILE"
fi

if [ "$RUN_RELEASE" -eq 1 ]; then
  run_step release-dry-run make release-dry-run
else
  echo "- [ ] \`make release-dry-run\` not run. Use \`--release-dry-run\` before tagging a release." >> "$SUMMARY_FILE"
fi

if [ "$PRINT_MANUAL" -eq 1 ]; then
  write_manual_plan | tee "$EVIDENCE_DIR/manual-browser-streaming.md"
fi

{
  echo
  echo "## Manual Browser And Streaming"
  echo
  echo "Record the browser, WebSocket, stats, HID, and no-screenshot-polling evidence described in docs/production-acceptance.md."
  echo
  echo "## Finished"
  echo
  echo "- Finished: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
} >> "$SUMMARY_FILE"

echo
echo "Evidence written to $EVIDENCE_DIR"
echo "Summary: $SUMMARY_FILE"
echo "Commands: $COMMANDS_FILE"

if [ "$FAILURES" -ne 0 ]; then
  exit 1
fi
