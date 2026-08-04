#!/usr/bin/env bash
# e2e.sh: full-stack benchmarks against the real ptyZZZ binary.
#
# Each scenario spawns ptyZZZ, drives a workload through the pty, and
# consumes the JSONL frames, so it exercises the whole path: pty read ->
# wezterm parse -> damage scan -> row render -> serde -> stdout.
#
# Workloads are deterministic: scroll24 is generated, aqua24/vim24 replay
# corpora recorded by bench/record.sh (run it once first). The replayer
# slices each corpus into 24 chunks/sec, so every scenario runs at 24 fps
# regardless of the source app's own draw rate.
#
# Usage:
#   bench/e2e.sh [firehose|scroll24|aqua24|vim24|latency|all]
#
# A/B two builds by pointing BIN at each:
#   BIN=./target/release/ptyZZZ bench/e2e.sh all
#   BIN=/tmp/ptyZZZ-before     bench/e2e.sh all
#
# Every throughput scenario reports the same row:
#   fps, frames (screen/diff split), JSONL MB, avg B/frame, ptyZZZ cpu ms.
# Trust cpu and bytes, not wall time: wall time is fixed by the pacing.
# latency is the odd one out by nature: it reports round-trip percentiles.

set -euo pipefail
cd "$(dirname "$0")/.."

BIN=${BIN:-./target/release/ptyZZZ}
SECS=${SECS:-10}
LAT_N=${LAT_N:-100}
HZ=24
HZ_SLEEP=0.038 # ~24 Hz once per-event overhead is added

header() {
    printf '%-9s %6s %8s %14s %8s %9s %8s\n' \
        scenario fps frames screen/diff MB B/frame cpu_ms
}

# report <name> <secs> <cpu_ms> < frames.jsonl
report() {
    awk -v name="$1" -v secs="$2" -v cpu="$3" \
        '{n++; b+=length($0)+1}
         /"t":"screen"/ {kf++} /"t":"diff"/ {df++}
         END {printf "%-9s %6.1f %8d %14s %8.2f %9d %8s\n",
              name, (secs > 0 ? n/secs : 0), n, kf+0 "/" df+0,
              b/1048576, (n ? b/n : 0), cpu}'
}

# Sample /proc/<pid>/stat while <pid> runs; echo total cpu ms when it exits.
sample_cpu() {
    local pid=$1 s cpu="0 0"
    while kill -0 "$pid" 2>/dev/null; do
        s=$(cut -d' ' -f14,15 "/proc/$pid/stat" 2>/dev/null) && cpu=$s
        sleep 0.2
    done
    set -- $cpu
    echo $((($1 + $2) * 10)) # clock ticks (100 Hz) -> ms
}

# run_pty <name> <run-args...>: run under ptyZZZ, print a report row.
# fps in the report is realized: frames / measured wall time.
run_pty() {
    local name=$1 out pid cpu t0 t1
    shift
    out=$(mktemp)
    t0=$(date +%s%N)
    "$BIN" run "$@" </dev/null >"$out" &
    pid=$!
    cpu=$(sample_cpu "$pid")
    wait "$pid" 2>/dev/null || true
    t1=$(date +%s%N)
    report "$name" "$(awk -v ns=$((t1 - t0)) 'BEGIN {print ns / 1e9}')" "$cpu" <"$out"
    rm -f "$out"
}

firehose() {
    # Throughput ceiling: output arrives as fast as the pty drains, and
    # coalesce=41ms caps emission at ~24 fps. cpu is still the headline
    # metric; fps shows whether emission kept up.
    run_pty firehose --coalesce 41 -- sh -c "seq 1 ${FIREHOSE_LINES:-2000000}"
}

scroll24() {
    run_pty scroll24 -- sh -c '
        i=0
        while [ $i -lt '"$((SECS * HZ))"' ]; do
            seq $((i*20)) $((i*20+19))
            i=$((i+1))
            sleep '"$HZ_SLEEP"'
        done'
}

# replay <name> <corpus>: slice the corpus into 24 chunks/sec over SECS.
replay() {
    local name=$1 corpus=$2 chunk
    [ -s "$corpus" ] || {
        echo "$name: missing $corpus (run bench/record.sh first)" >&2
        return
    }
    chunk=$((($(wc -c <"$corpus") + SECS * HZ - 1) / (SECS * HZ)))
    run_pty "$name" -- sh -c '
        i=0
        while [ $i -lt '"$((SECS * HZ))"' ]; do
            dd if='"$corpus"' bs='"$chunk"' skip=$i count=1 2>/dev/null
            i=$((i+1))
            sleep '"$HZ_SLEEP"'
        done'
}

latency() {
    echo
    echo "latency: $LAT_N keystroke echoes at ~24 Hz via cat (--coalesce 0)"
    local m t0 t1 line
    coproc PTY { "$BIN" run --coalesce 0 -- cat; }
    local lat=()
    for i in $(seq 1 "$LAT_N"); do
        m=$(printf 'mk%04d' "$i")
        t0=${EPOCHREALTIME/./} # builtin us clock; date(1) would cost a fork
        printf '{"t":"input","b":"%s\\r"}\n' "$m" >&"${PTY[1]}"
        while read -r -u "${PTY[0]}" line; do
            [[ $line == *"$m"* ]] && break
        done
        t1=${EPOCHREALTIME/./}
        lat+=($((t1 - t0)))
        sleep "$HZ_SLEEP"
    done
    eval "exec ${PTY[1]}>&-"
    printf '%s\n' "${lat[@]}" | sort -n | awk \
        '{a[NR]=$1}
         END {printf "  n=%d p50=%dus p90=%dus p99=%dus max=%dus\n",
              NR, a[int(NR*.5)], a[int(NR*.9)], a[int(NR*.99)], a[NR]}'
}

case "${1:-all}" in
firehose)
    header
    firehose
    ;;
scroll24)
    header
    scroll24
    ;;
aqua24)
    header
    replay aqua24 bench/corpus/aqua.raw
    ;;
vim24)
    header
    replay vim24 bench/corpus/vim24.raw
    ;;
latency) latency ;;
all)
    header
    firehose
    scroll24
    replay aqua24 bench/corpus/aqua.raw
    replay vim24 bench/corpus/vim24.raw
    latency
    ;;
*)
    echo "usage: bench/e2e.sh [firehose|scroll24|aqua24|vim24|latency|all]" >&2
    exit 1
    ;;
esac
