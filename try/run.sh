#!/usr/bin/env bash
# Runs the application on a local Core, grants the Plugin what its Manifest declares, and follows the unit
# from the channel `panel`: every question's answer, every stream and every event, one line each. An
# interrupt stops everything. yk-qshell's notification centre speaks to it.
#
# Usage: run.sh [--read-only] [--seconds N]
#   --read-only   grants the questions, the streams and the occurrences, and no command
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
plugin=com.yoke.notifications unit=notifications
manifest="$here/../plugin/$plugin.yaml"
read_only="" follow_args=()
while (( $# )); do
  case "$1" in
    --read-only) read_only=1 ;;
    --seconds) follow_args+=(--seconds "$2"); shift ;;
    *) echo "run.sh: unknown argument $1" >&2; exit 2 ;;
  esac
  shift
done

# The Plugin as the application carries it, built from ../plugin whenever it changed.
cargo build --release --quiet --manifest-path "$here/../plugin/Cargo.toml"
if ! cmp -s "$here/../plugin/target/release/$unit" "$here/components/plugins/$unit"; then
  cp "$here/../plugin/target/release/$unit" "$here/components/plugins/$unit"
  cp "$manifest" "$here/manifests/"
fi
yoke-compose set "units.$unit.digest" "sha256:$(sha256sum "$here/components/plugins/$unit" | cut -d' ' -f1)" --dir "$here" > /dev/null
cargo build --release --quiet --manifest-path "$here/follow/Cargo.toml"

log="$(mktemp)"
yoke app run --from "$here" > "$log" 2>&1 &
run=$!
trap 'kill -INT "$run" 2>/dev/null; wait "$run" 2>/dev/null; rm -f "$log"' EXIT
until grep -q 'is observable' "$log"; do
  kill -0 "$run" 2>/dev/null || { cat "$log" >&2; exit 1; }
  sleep 0.3
done
instance="$(sed -n '1s/ runs .*//p' "$log")"
export YOKE_INSTANCE="$instance"

# A run's instance starts with nothing granted; a grant reaches the unit at its next admission.
for capability in $(sed -n 's/^  - name: //p' "$manifest"); do
  [[ -n "$read_only" && "$capability" == command.* ]] && continue
  yoke plugin grant "$plugin" "$capability" --instance "$instance" > /dev/null
done
yoke unit restart "$unit" --instance "$instance" > /dev/null
sleep 1
# Every question and every stream the Manifest declares.
section() { awk -v s="$1:" '$0 == s {on=1; next} /^[a-z]/ {on=0} on && /^  - id: / {print $3}' "$manifest"; }
for q in $(section queries); do follow_args+=(--ask "$q"); done
for s in $(section streams); do follow_args+=(--stream "$s"); done
echo "run.sh: $instance runs; commands go with: YOKE_INSTANCE=$instance yoke unit command $unit <type> --input -" >&2
"$here/follow/target/release/follow" --instance "$instance" --channel panel --unit "$unit" "${follow_args[@]}"
