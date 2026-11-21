#!/usr/bin/env bash
# Measures the CPU use of AIchemist Desktop Buddy on macOS over a window of
# time (docs/perf.md), summed over the app process and the WebKit processes
# that serve it (com.apple.WebKit.WebContent / .Networking / .GPU).
#
# Per process we read the kernel's exact CPU time counters (user + system,
# proc_pid_rusage) at the start and the end of the window and take the
# difference. `ps -o time` and `top` round to 10 ms or worse, which hides
# the short bursts of an animation. Wake-ups (package idle + interrupt
# wake-ups from the same counters) are reported too.
#
# WebKit helpers are attributed to the app through the kernel's "responsible
# process", as in tools/measure-memory.sh, which needs the app launched with
# `open` (or from Finder). A tiny C helper is compiled on the fly (Xcode
# command line tools).
#
# Usage:
#   tools/measure-cpu.sh [--app PATH.app | --pid PID] [--settle S] [--seconds S]
#     --app      launch this .app bundle with `open -n`, measure, then quit it
#     --pid      measure an already running app process
#     default    measure the running process named aichemist-desktop-buddy
#     --settle   wait before the window starts (default 30)
#     --seconds  length of the window (default 60)
set -euo pipefail

APP=""
PID=""
SETTLE=30
SECONDS_WINDOW=60
NAME="aichemist-desktop-buddy"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --app) APP="$2"; shift 2 ;;
    --pid) PID="$2"; shift 2 ;;
    --settle) SETTLE="$2"; shift 2 ;;
    --seconds) SECONDS_WINDOW="$2"; shift 2 ;;
    -h|--help) sed -n '2,24p' "$0"; exit 0 ;;
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

# Prints "pid responsible_pid cpu_ns wakeups" per pid (cpu_ns -1 if unreadable).
cat > "$WORK/probe.c" <<'EOF'
#include <libproc.h>
#include <mach/mach_time.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/resource.h>
#include <sys/types.h>
pid_t responsibility_get_pid_responsible_for_pid(pid_t);
int main(int argc, char **argv) {
  mach_timebase_info_data_t tb;
  mach_timebase_info(&tb);
  for (int i = 1; i < argc; i++) {
    pid_t p = (pid_t)atoi(argv[i]);
    struct rusage_info_v3 ri;
    pid_t r = responsibility_get_pid_responsible_for_pid(p);
    if (proc_pid_rusage(p, RUSAGE_INFO_V3, (rusage_info_t *)&ri) == 0) {
      /* User and system time are in mach time units (ns on Intel). */
      unsigned long long ns = (ri.ri_user_time + ri.ri_system_time) * tb.numer / tb.denom;
      unsigned long long wk = ri.ri_pkg_idle_wkups + ri.ri_interrupt_wkups;
      printf("%d %d %llu %llu\n", p, r, ns, wk);
    } else {
      printf("%d %d -1 -1\n", p, r);
    }
  }
  return 0;
}
EOF
cc -o "$WORK/probe" "$WORK/probe.c" 2>/dev/null || { echo "could not compile the probe helper (install Xcode command line tools)" >&2; exit 1; }

if [[ -n "$APP" ]]; then
  exe="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$APP/Contents/Info.plist")"
  open -n "$APP"
  LAUNCHED=1
  for _ in $(seq 1 50); do
    PID="$(pgrep -nf "$APP/Contents/MacOS/$exe" || true)"
    [[ -n "$PID" ]] && break
    sleep 0.2
  done
  [[ -n "$PID" ]] || { echo "app process not found" >&2; exit 1; }
  echo "Root PID $PID; settling for ${SETTLE}s..."
  sleep "$SETTLE"
elif [[ -z "$PID" ]]; then
  PID="$(pgrep -n -x "$NAME" || true)"
fi
[[ -n "$PID" ]] || { echo "app process not found" >&2; exit 1; }
kill -0 "$PID" 2>/dev/null || { echo "process $PID exited before measurement" >&2; exit 1; }

app_resp="$("$WORK/probe" "$PID" | awk '{print $2}')"
if [[ "$app_resp" != "$PID" ]]; then
  echo "warning: app is not its own responsible process (launched from a terminal?)." >&2
  echo "         WebKit processes of other apps launched by the same parent may be counted." >&2
fi

# The app plus every WebKit process the app is responsible for.
tree() {
  local webkit
  webkit="$(ps -axo pid=,comm= | awk '$2 ~ /com\.apple\.WebKit\./ {print $1}')"
  echo "$PID"
  if [[ -n "$webkit" ]]; then
    # shellcheck disable=SC2086
    "$WORK/probe" $webkit | awk -v r="$app_resp" '$2 == r {print $1}'
  fi
}

# shellcheck disable=SC2046
"$WORK/probe" $(tree) > "$WORK/before.txt"
start_ns="$(perl -MTime::HiRes=time -e 'printf "%.0f", time * 1e9')"
sleep "$SECONDS_WINDOW"
# Processes started during the window count from zero.
# shellcheck disable=SC2046
"$WORK/probe" $( (awk '{print $1}' "$WORK/before.txt"; tree) | sort -un) > "$WORK/after.txt"
end_ns="$(perl -MTime::HiRes=time -e 'printf "%.0f", time * 1e9')"
logical="$(sysctl -n hw.logicalcpu)"

printf "%-8s %-36s %10s %10s\n" PID PROCESS CPU_MS WAKEUPS
awk -v wall_ns="$((end_ns - start_ns))" -v cpus="$logical" -v app="$PID" '
  FNR == NR { if ($3 >= 0) { b[$1] = $3; bw[$1] = $4 } next }
  $3 >= 0 {
    ms = ($3 - (($1 in b) ? b[$1] : 0)) / 1e6
    wk = $4 - (($1 in bw) ? bw[$1] : 0)
    cmd = "ps -p " $1 " -o comm= 2>/dev/null"; name = "?"; cmd | getline name; close(cmd)
    n = split(name, parts, "/"); name = parts[n]
    if ($1 == app) name = name " (app)"
    printf "%-8s %-36s %10.1f %10d\n", $1, name, ms, wk
    total += ms; wakeups += wk; count++
  }
  END {
    wall = wall_ns / 1e9
    printf "\nTOTAL  processes=%d  window=%.1f s  cpu=%.1f ms  one_core=%.3f %%  machine=%.4f %% (%d logical CPUs)  wakeups=%.1f/s\n",
      count, wall, total, total / (wall * 1000) * 100, total / (wall * 1000 * cpus) * 100, cpus, wakeups / wall
  }' "$WORK/before.txt" "$WORK/after.txt"
