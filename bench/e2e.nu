#!/usr/bin/env nu
# e2e.nu: full-stack benchmarks against the real ptyZZZ binary.
#
# Each scenario spawns ptyZZZ, drives a workload through the pty, and
# consumes the JSONL frames, so it exercises the whole path: pty read ->
# wezterm parse -> damage scan -> row render -> serde -> stdout.
#
# Workloads are deterministic: scroll24 is generated, aqua24/vim24 replay
# corpora recorded by bench/record.sh. The replayer slices each corpus
# into 24 chunks/sec. With leading-edge emission a burst splits into an
# immediate frame plus the remainder one window later, so paced scenarios
# emit 24-48 fps depending on burst size (vim24 single-key bursts stay
# ~24). Keep SECS equal to the recording length
# (record.sh default 10): a shorter SECS replays the same bytes in fewer,
# larger chunks, which shifts frames from the diff path to keyframes.
# firehose is unpaced with coalesce=41ms, so its realized fps doubles as
# a keyframe-production-cost signal.
#
# Usage:
#   bench/e2e.nu              # all scenarios
#   bench/e2e.nu latency      # one scenario
#
# A/B two builds by pointing BIN at each:
#   BIN=./target/release/ptyZZZ bench/e2e.nu
#   BIN=/tmp/ptyZZZ-before     bench/e2e.nu
#
# Tunables (env): BIN, SECS (default 10), LAT_N (default 100),
# FIREHOSE_LINES (default 2000000).
#
# Trust cpu and size, not wall time: wall is fixed by the pacing. fps is
# realized (frames / measured wall). The latency scenario reports
# round-trip percentiles instead; it measures time, not volume.

const HZ = 24
const HZ_SLEEP = 38ms # ~24 Hz once per-event overhead is added
const CPU_TAG = 7
const FRAME_TAG = 9

def bin [] { $env.BIN? | default "./target/release/ptyZZZ" }
def secs [] { $env.SECS? | default 10 | into int }

# Background job: find the ptyZZZ child of this nu process, sample its
# cumulative cpu from /proc until it exits, then mail the total (ms) to
# main. /proc/<pid>/stat fields 14/15 are utime/stime in 100 Hz ticks.
def spawn-cpu-sampler [] {
    let bname = bin | path basename
    job spawn {
        mut pid = 0
        for _ in 1..50 {
            let found = ps --long | where ppid == $nu.pid and name == $bname
            if ($found | is-not-empty) {
                $pid = $found | first | get pid
                break
            }
            sleep 50ms
        }
        mut cpu = 0
        loop {
            let stat = try { open --raw $"/proc/($pid)/stat" } catch { null }
            if $stat == null { break }
            let parts = $stat | split row " "
            $cpu = ((($parts | get 13 | into int) + ($parts | get 14 | into int)) * 10)
            sleep 200ms
        }
        $cpu | job send 0 --tag $CPU_TAG
    }
}

# Run ptyZZZ with the given `run` args in the foreground, cpu-sampled from
# a job, and reduce the emitted frames to one metrics record.
def run-pty [name: string, args: list<string>] {
    let b = bin
    let out = mktemp -t
    spawn-cpu-sampler | ignore
    let t0 = date now
    "" | ^$b run --on-stdin-eof ignore ...$args o> $out
    let wall = (date now) - $t0
    let cpu = try { job recv --tag $CPU_TAG --timeout 5sec } catch { -1 }
    let row = report $name $wall $cpu $out
    rm $out
    $row
}

def report [name: string, wall: duration, cpu_ms: int, out: path] {
    let size = ls $out | first | get size
    let types = open --raw $out | lines | each { from json | get t }
    let n = $types | length
    {
        scenario: $name
        fps: (($n / ($wall / 1sec)) | math round --precision 1)
        frames: $n
        screen: ($types | where $it == "screen" | length)
        diff: ($types | where $it == "diff" | length)
        cpu_ms: $cpu_ms
        "b/frame": (if $n > 0 { ($size | into int) / $n | math round } else { 0 })
        size: $size
        wall: (($wall // 10ms) * 10ms)
    }
}

# Throughput ceiling: output arrives as fast as the pty drains, and
# coalesce=41ms caps emission at ~24 fps. cpu is the headline metric;
# realized fps shows whether keyframe production kept up.
def firehose [] {
    let lines = $env.FIREHOSE_LINES? | default 2000000
    run-pty firehose [--coalesce 41 -- sh -c $"seq 1 ($lines)"]
}

# 20-line bursts at 24 Hz: the paced append path. The sh script is built
# from single-quoted pieces so its `$` uses stay literal.
def scroll24 [] {
    let ticks = (secs) * $HZ
    let script = (
        'i=0; while [ $i -lt ' + $"($ticks)" + ' ]; do'
        + ' seq $((i*20)) $((i*20+19)); i=$((i+1)); sleep 0.038; done'
    )
    run-pty scroll24 [-- sh -c $script]
}

# scroll24's shape with URL-rich lines: 20 lines/tick at 24 Hz, each line
# carrying a bare URL, a bracketed URL, an angle-bracketed URL, and a mailto
# (and wrapping past 80 cols, so logical-line scanning is exercised). The
# delta vs scroll24 prices the implicit-link scan plus anchor emission.
def urls24 [] {
    let ticks = (secs) * $HZ
    let script = (
        'i=0; while [ $i -lt ' + $"($ticks)" + ' ]; do j=0; while [ $j -lt 20 ]; do'
        + ' echo "[$i.$j] GET https://api.example.com/v1/items/$i/$j ->'
        + ' (https://cdn.example.com/img/$j.png) see <https://docs.example.com/p/$i>'
        + ' or mail ops@example.com 200"'
        + '; j=$((j+1)); done; i=$((i+1)); sleep 0.038; done'
    )
    run-pty urls24 [-- sh -c $script]
}

# Replay a recorded corpus in 24 byte-slices/sec via dd inside the pty.
def replay [name: string, corpus: path] {
    if not ($corpus | path exists) {
        error make {msg: $"($name): missing ($corpus) \(run bench/record.sh first)"}
    }
    let ticks = (secs) * $HZ
    let size = ls $corpus | first | get size | into int
    let chunk = ($size + $ticks - 1) // $ticks
    let script = (
        'i=0; while [ $i -lt ' + $"($ticks)" + ' ]; do'
        + ' dd if=' + $corpus + ' bs=' + $"($chunk)"
        + ' skip=$i count=1 2>/dev/null; i=$((i+1)); sleep 0.038; done'
    )
    run-pty $name [-- sh -c $script]
}

# Keystroke round trips through a live coprocess: a job runs the ptyZZZ
# pipeline with a mailbox-fed generator at its head (kept lazy by
# `to text`; a bare value stream would be table-rendered and buffered),
# and mails every output frame back with an arrival timestamp. Main paces
# sends at ~24 Hz and joins send/arrival times per marker.
def latency [] {
    let n = $env.LAT_N? | default 100 | into int
    let b = bin
    let jid = job spawn {||
        $in | ignore
        generate {|_|
            let msg = job recv
            if $msg == "" { {} } else { {out: $"($msg)\n", next: true} }
        } true
        | to text
        | ^$b run --coalesce 0 -- cat
        | lines
        | each {|line| {line: $line, ts: (date now)} | job send 0 --tag $FRAME_TAG }
        | ignore
    }
    sleep 300ms # let ptyZZZ and cat spawn
    mut lats = []
    for i in 1..$n {
        let m = $"mk($i | fill --alignment right --character '0' --width 4)"
        let t0 = date now
        $'{"t":"input","b":"($m)\r"}' | job send $jid
        loop {
            let msg = job recv --tag $FRAME_TAG --timeout 5sec
            if ($msg.line? | default "" | str contains $m) {
                $lats = $lats | append ($msg.ts - $t0)
                break
            }
        }
        sleep $HZ_SLEEP
    }
    # JSON \u0004 is ctrl-D: the pty line discipline turns it into EOF for
    # cat, which exits and takes ptyZZZ down cleanly; then the empty-string
    # sentinel stops the generator. Each send may race the job's own exit,
    # so both are best-effort.
    try { '{"t":"input","b":"\u0004"}' | job send $jid }
    try { "" | job send $jid }
    sleep 300ms
    try { job kill $jid }
    let sorted = $lats | sort
    let at = {|p|
        # nearest-rank percentile: ceil(p/100 * n), as a 0-based index
        let idx = ([(($n * $p + 99) // 100) 1] | math max) - 1
        ($sorted | get $idx | $in // 1us) * 1us
    }
    {
        scenario: latency
        n: $n
        p50: (do $at 50)
        p90: (do $at 90)
        p99: (do $at 99)
        max: (do $at 100)
    }
}

def main [scenario: string = "all"] {
    match $scenario {
        "firehose" => [(firehose)]
        "scroll24" => [(scroll24)]
        "urls24" => [(urls24)]
        "aqua24" => [(replay aqua24 bench/corpus/aqua.raw)]
        "vim24" => [(replay vim24 bench/corpus/vim24.raw)]
        "latency" => (latency)
        "all" => {
            let throughput = [
                (firehose)
                (scroll24)
                (urls24)
                (replay aqua24 bench/corpus/aqua.raw)
                (replay vim24 bench/corpus/vim24.raw)
            ]
            print ($throughput | table)
            latency
        }
        _ => {
            error make {msg: $"unknown scenario ($scenario); use firehose|scroll24|urls24|aqua24|vim24|latency|all"}
        }
    }
}
