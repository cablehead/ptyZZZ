#!/usr/bin/env bash
# record.sh: capture raw pty byte streams into bench/corpus/ for e2e.sh replay.
#
# Recording runs the apps under ptyZZZ itself (--record tees the raw pty
# bytes), so terminal queries get answered the same way they do in
# production; a dumb pty recorder like util-linux script hangs apps that
# wait on query responses.
#
# The corpora are vendored in bench/corpus/ so benches are reproducible
# without vim or the animation binaries installed (ghostty's gen/bench
# split). Re-run this script only to refresh them.
#
# Usage: bench/record.sh [secs]   (default 10)

set -euo pipefail
cd "$(dirname "$0")/.."

SECS=${1:-10}
BIN=${BIN:-./target/release/ptyZZZ}
ANIM=${ANIM:-examples/cube/bin/asciiquarium-$(uname -m)-unknown-linux-gnu}
mkdir -p bench/corpus

# asciiquarium: app-paced repaints, recorded at its natural rate.
if [ -x "$ANIM" ]; then
    "$BIN" run --on-stdin-eof ignore --record bench/corpus/aqua.raw -- \
        timeout "$SECS" "$ANIM" </dev/null >/dev/null
    echo "aqua.raw: $(wc -c <bench/corpus/aqua.raw) bytes"
else
    echo "skip aqua: $ANIM not present" >&2
fi

# vim: 24 Hz single-char inserts (newline every 40), then a clean :q!.
# Keystrokes are fed through ptyZZZ's JSONL input protocol.
if command -v vim >/dev/null; then
    {
        sleep 0.5
        printf '{"t":"input","b":"i"}\n'
        for ((i = 0; i < SECS * 24; i++)); do
            if ((i % 40 == 39)); then
                printf '{"t":"input","b":"\\r"}\n'
            else
                printf '{"t":"input","b":"%d"}\n' "$((i % 10))"
            fi
            sleep 0.038
        done
        printf '{"t":"input","b":"\\u001b:q!\\r"}\n'
        sleep 0.5
    } | "$BIN" run --record bench/corpus/vim24.raw -- \
        vim -u NONE -i NONE -n >/dev/null
    echo "vim24.raw: $(wc -c <bench/corpus/vim24.raw) bytes"
else
    echo "skip vim24: vim not on PATH" >&2
fi
