#!/usr/bin/env nu
# keytest.nu: synthetic end-to-end keyboard test.
#
# Boots an isolated http-nu + serve.nu instance, launches examples/keyprobe
# inside each pane's shell, then drives a headless chromium at the page with
# ?drive so bench/keyprobe.json's steps replay as synthesized KeyboardEvents.
# The full path is exercised: browser event -> client keyEvent()/paste ->
# awaited queue POST -> pty-<pane>.send -> ptyZZZ {t:key}/{t:paste} ->
# emulator encoding -> pty -> keyprobe's raw-mode byte assertions.
#
# The verdict is read back from each pane's keyframe: KEYPROBE PASS/FAIL.
#
# Env:
#   CHROME       chromium binary (default: chromium on PATH)
#   HTTP_NU_BIN  http-nu binary (default: ~/http-nu/target/release/http-nu)
#   PTYZZZ_BINS  panes to test, name=path pairs (default: this repo's binary)

const PORT = 6689

const ROOT = path self | path dirname | path dirname

def main [] {
    let root = $ROOT
    cd $root
    ^cargo build --release --example keyprobe
    let chrome = $env.CHROME? | default "chromium"
    let http_nu = $env.HTTP_NU_BIN? | default ("~/http-nu/target/release/http-nu" | path expand)
    let bins = $env.PTYZZZ_BINS? | default $"local=($root)/target/release/ptyZZZ"
    let panes = $bins | split row "," | each { split row "=" | get 0 | str trim }
    let store = mktemp -d

    job spawn {||
        with-env {PTYZZZ_BINS: $bins} {
            do { ^$http_nu --dev --datastar --services --store $store $"127.0.0.1:($PORT)" ./serve.nu } | complete | ignore
        }
    } | ignore

    # wait for the server, then for each pane's pty service to produce a screen
    for _ in 1..50 {
        let up = try { http get $"http://127.0.0.1:($PORT)/" | ignore; true } catch { false }
        if $up { break }
        sleep 200ms
    }
    sleep 2sec

    mut failed = false
    for pane in $panes {
        print $"== pane ($pane) =="
        $'{"t":"input","b":"target/release/examples/keyprobe bench/keyprobe.json\r"}(char nl)'
            | http post --content-type text/plain $"http://127.0.0.1:($PORT)/input?pane=($pane)" $in
        sleep 1sec
        let spec = open bench/keyprobe.json | merge {pane: $pane} | to json --raw | encode base64
        do { ^timeout 15 $chrome --headless=new --disable-gpu --no-sandbox --hide-scrollbars $"http://127.0.0.1:($PORT)/?drive&nofit#($spec)" } | complete | ignore
        sleep 6sec # a healing keyframe carries the verdict into the store
        let snap = http get $"http://127.0.0.1:($PORT)/snap?pane=($pane)"
        if ($snap | str contains "KEYPROBE PASS") {
            print $"($pane): PASS"
        } else {
            $failed = true
            print $"($pane): FAIL"
            print ($snap | str replace --all --regex "<[^>]*>" "" | lines | where {|l| $l =~ "ok |FAIL|KEYPROBE|keyprobe"} | last 10 | str join (char nl))
        }
    }

    # teardown: the server and the pty children it spawned
    let srv = ps --long | where command =~ $"127.0.0.1:($PORT)"
    for p in $srv { kill --force $p.pid }
    sleep 500ms
    for p in (ps --long | where command =~ "keyprobe|ptyZZZ run") {
        let parent_gone = (ps | where pid == $p.ppid | is-empty)
        if $parent_gone { kill --force $p.pid }
    }
    rm -rf $store

    if $failed { error make {msg: "keytest: FAIL"} } else { print "keytest: PASS" }
}
