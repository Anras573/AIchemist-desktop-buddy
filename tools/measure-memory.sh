#!/usr/bin/env bash
# Measures the idle private memory of AIchemist Desktop Buddy on macOS.
#
# Metric (owner decision 2026-11-16, see docs/perf.md): memory the app uses on
# its own, summed over the app process and the WebKit processes that serve it
# (com.apple.WebKit.WebContent / .Networking / .GPU). Per process we read the
# physical footprint (phys_footprint), the number Activity Monitor shows as
# "Memory" and `footprint` reports. Shared memory is excluded.
#
# WebKit helpers are XPC services: their parent is launchd, not the app. We
# attribute them through the kernel's "responsible process", which is the app
# when it is launched with `open` (or from Finder). A tiny C helper is compiled
# on the fly to read it (needs the Xcode command line tools, which a Tauri
# build needs anyway).
#
# Usage:
#   tools/measure-memory.sh [--app PATH.app | --pid PID] [--settle SECONDS]
#     --app     launch this .app bundle with `open -n`, measure, then quit it
#     --pid     measure an already running app process
#     default   measure the running process named aichemist-desktop-buddy
#     --settle  idle time before measuring (default 30)
set -euo pipefail

APP=""
PID=""
SETTLE=30
NAME="aichemist-desktop-buddy"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --app) APP="$2"; shift 2 ;;
    --pid) PID="$2"; shift 2 ;;
    --settle) SETTLE="$2"; shift 2 ;;
    -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

WORK="$(mktemp -d)"
LAUNCHED=0
cleanup() {
  if [[ "$LAUNCHED" == 1 && -n "$PID" ]]; then kill "$PID" 2>/dev/null || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT

if [[ -n "$APP" ]]; then
  exe="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$APP/Contents/Info.plist")"
  open -n "$APP"
  LAUNCHED=1
  for _ in $(seq 1 50); do
    PID="$(pgrep -nf "$APP/Contents/MacOS/$exe" || true)"
    [[ -n "$PID" ]] && break
    sleep 0.2
  done
elif [[ -z "$PID" ]]; then
  PID="$(pgrep -n -x "$NAME" || true)"
fi
[[ -n "$PID" ]] || { echo "app process not found" >&2; exit 1; }

echo "Root PID $PID; settling for ${SETTLE}s..."
sleep "$SETTLE"
kill -0 "$PID" 2>/dev/null || { echo "process $PID exited before measurement" >&2; exit 1; }

cat > "$WORK/resp.c" <<'EOF'
#include <stdio.h>
#include <stdlib.h>
#include <sys/types.h>
pid_t responsibility_get_pid_responsible_for_pid(pid_t);
int main(int argc, char **argv) {
  for (int i = 1; i < argc; i++) {
    pid_t p = (pid_t)atoi(argv[i]);
    printf("%d %d\n", p, responsibility_get_pid_responsible_for_pid(p));
  }
  return 0;
}
EOF
cc -o "$WORK/resp" "$WORK/resp.c" 2>/dev/null || { echo "could not compile the responsibility helper (install Xcode command line tools)" >&2; exit 1; }

app_resp="$("$WORK/resp" "$PID" | awk '{print $2}')"
if [[ "$app_resp" != "$PID" ]]; then
  echo "warning: app is not its own responsible process (launched from a terminal?)." >&2
  echo "         WebKit processes of other apps launched by the same parent may be counted." >&2
fi

pids=("$PID")
webkit="$(ps -axo pid=,comm= | awk '$2 ~ /com\.apple\.WebKit\./ {print $1}')"
if [[ -n "$webkit" ]]; then
  # shellcheck disable=SC2086
  while read -r p r; do
    if [[ "$r" == "$app_resp" ]]; then
      pids+=("$p")
    else
      echo "not counted: $(basename "$(ps -p "$p" -o comm=)") [$p], responsible pid $r" >&2
    fi
  done < <("$WORK/resp" $webkit)
fi
[[ ${#pids[@]} -gt 1 ]] || echo "warning: no WebKit processes attributed to the app" >&2

# top's MEM column is phys_footprint. Two samples; the second is the stable one.
top_args=()
for p in "${pids[@]}"; do top_args+=(-pid "$p"); done
top -l 2 -s 1 -stats pid,command,mem "${top_args[@]}" > "$WORK/top.txt"

to_mb() {
  awk -v v="$1" 'BEGIN {
    n = v + 0; u = substr(v, length(v));
    if (u == "K") n /= 1024; else if (u == "G") n *= 1024; else if (u == "B") n /= 1048576;
    printf "%.1f", n }'
}

# Keep only the last sample block.
awk '/^PID/ {blk = ""; inb = 1; next} inb && NF {blk = blk $0 "\n"} END {printf "%s", blk}' "$WORK/top.txt" > "$WORK/last.txt"

printf "%-8s %-36s %12s\n" PID PROCESS FOOTPRINT_MB
total=0
while read -r pid rest; do
  [[ -n "$pid" ]] || continue
  mem="${rest##* }"
  mb="$(to_mb "${mem%[+-]}")"
  cmd="$(basename "$(ps -p "$pid" -o comm= 2>/dev/null || echo '?')")"
  printf "%-8s %-36s %12s\n" "$pid" "$cmd" "$mb"
  total="$(awk -v a="$total" -v b="$mb" 'BEGIN {printf "%.1f", a + b}')"
done < "$WORK/last.txt"

echo
echo "TOTAL  processes=${#pids[@]}  footprint=${total} MB"

if command -v footprint >/dev/null 2>&1; then
  echo
  echo "--- footprint (cross-check) ---"
  footprint "${pids[@]}" 2>&1 | grep -iE 'footprint' || true
fi
