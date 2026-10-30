# ptyZZZ experiment: ptys owned by xs *services*, shown over one /sse.
#
# Run:
#   ~/http-nu/target/release/http-nu --dev --datastar --services --store ./store \
#     :5111 ~/ptyZZZ/serve.nu
#
# Panes: PTYZZZ_BINS is a comma-separated list of name=path pairs, one pty
# pane per entry, rendered side by side. Unset, it serves one pane backed by
# this repo's release binary if present, otherwise ptyZZZ on PATH. Two engines
# head to head:
#   PTYZZZ_BINS="wezterm=/a/ptyZZZ,rio=/b/ptyZZZ" http-nu ... serve.nu
#
# Flow, per pane <name>:
#   POST /input?pane=<name> -> append `pty-<name>.send` -> service stdin
#   ptyZZZ stdout -> service closure fans JSONL into frames:
#     screen (keyframe) -> `pty-<name>.screen` (ttl last:1)    the join point
#     diff              -> `pty-<name>.diff`   (ttl ephemeral) changed rows
#
# Diffs carry their payload in frame meta ({body: ...}), not the CAS. An
# ephemeral frame is broadcast and never stored, so writing its content to the
# CAS would be a disk write with no reader benefit -- benched at ~30us/frame
# via CAS vs ~0.4us via meta, plus it saves a CAS read per subscriber per
# frame on /sse. Keyframes stay in the CAS: stored (last:1), large, rare.
#   GET  /sse     -> follow every pane's topics, patch by element id
#
# A joiner replays each pane's stored keyframe, asks each pane for a fresh one
# ({t:screen}) once xs signals the follow is live, and applies live diffs on
# top; any missed diffs or delta bugs heal at the next keyframe. Diffs are
# ephemeral, so the request must go out after the follow is open: xs emits
# `xs.threshold` after the replay, and the request rides on that.
#
# The client routes keystrokes to the focused pane (click to focus) and fits
# each pane's pty to its own box via {t:resize}.

use http-nu/datastar *
use http-nu/router *

const HERE = (path self | path dirname)
const LOCAL_BIN = ($HERE | path join "target" "release" "ptyZZZ")

def resolve-ptyzzz [] {
  if ($LOCAL_BIN | path exists) { $LOCAL_BIN } else {
    let found = which ptyZZZ
    if ($found | is-empty) {
      error make {msg: "serve: no ptyZZZ binary (cargo build --release, or put ptyZZZ on PATH)"}
    } else {
      $found.0.path
    }
  }
}

let panes = (
  (if ($env.PTYZZZ_BINS? | default "" | is-empty) {
    $"local=(resolve-ptyzzz)"
  } else {
    $env.PTYZZZ_BINS
  })
  | split row ","
  | each {|p|
      let kv = $p | split row "=" | each { str trim }
      {name: $kv.0, bin: $kv.1}
    }
)

# Fail at load with a clear message rather than registering a service whose
# spawn dies silently (the page would just sit at "connecting...").
for p in $panes {
  if not ($p.bin | path exists) {
    error make {msg: $"serve: missing binary for pane ($p.name): ($p.bin)"}
  }
}

# Register each pane's pty service idempotently (needs --store + --services):
# append the create frame only if the stored definition is missing or changed.
# Create frames are kept `forever` -- the runtime keeps the last known-good
# create as its hot-replace fallback (lifecycle I3), and an already-confirmed
# service resumes on every boot on its own (I2), so re-appending an identical
# create each boot would just pile up cruft.
def register-service [topic: string, config: string] {
  let last = (.last $topic)
  let current = if ($last | is-empty) { null } else { .cas $last.hash }
  if $current != $config { $config | .append $topic | ignore }
}

const SVC = '{
  run: {||
    ^PTYBIN run --die-with-parent --target TARGET -- nu
    | lines | each {|l|
        let e = try { $l | from json } catch { null }
        if $e == null { return }
        match $e.t {
          "screen" => ( $e.html | .append "PFX.screen" --ttl last:1 )
          "diff"   => ( null | .append "PFX.diff" --ttl ephemeral --meta {body: $l} )
          "exit"   => ( {code: $e.code} | to json | .append "PFX.exit" --ttl last:1 )
          _ => null
        }
      } | ignore
  }
  duplex: true
}'

if ($HTTP_NU.store? | default null) != null and ($HTTP_NU.services? | default false) {
  for p in $panes {
    let closure = $SVC
      | str replace --all "PTYBIN" $p.bin
      | str replace --all "PFX" $"pty-($p.name)"
      | str replace --all "TARGET" $"grid-($p.name)"
    register-service $"xs.service.pty-($p.name).create" $closure
  }
}

let topics = $panes | get name | each {|n| [$"pty-($n).screen" $"pty-($n).diff"] } | flatten
let pane_names = $panes | get name

const TMPL = r#'<!doctype html>
<html><head><meta charset=utf-8>
<script type=module src="/static/vendor/datastar-rocket@1.0.4.js"></script>
<script type=module src="/static/pty-term.js"></script>
<link rel=stylesheet href="/static/pty-term.css">
<style>
  /* The layout. The terminal itself is styled by static/pty-term.css. */
  body{background:#000;color:#ddd;margin:0;font:14px var(--term-font);overflow:hidden}
  #panes{display:flex;height:100vh}
  #panes.stacked{flex-direction:column}
  /* min-width/height:0: flex items otherwise refuse to shrink below their
     content (min-size:auto), and a tall grid shoves its sibling offscreen
     in stacked mode; the scroll box must shrink so it scrolls instead. */
  .pane{flex:1;display:flex;flex-direction:column;background:var(--term-bg);min-width:0;min-height:0}
  .pane+.pane{border-left:1px solid #444}
  #panes.stacked .pane+.pane{border-left:none;border-top:1px solid #444}
  #split{position:fixed;top:0;right:6px;z-index:2;color:#888;background:none;border:none;font:12px var(--term-font);cursor:pointer;padding:2px 4px}
  #split:hover{color:#fff}
  .pane:focus-within .label{color:#fff;background:#333}
  .label{flex:none;font-size:11px;color:#888;background:#1a1a1a;padding:2px 8px}
</style></head>
<body data-init="@get('/sse')">
  <div id=panes>__PANES_HTML__</div>
  <button id=split title="toggle split direction"></button>
  <script type=module>
    // The layout: panes side by side or stacked, and which pane holds focus
    // at load. Everything inside a pane is <pty-term> (static/pty-term.js).
    const params = new URLSearchParams(location.search);
    const term = (name) => document.querySelector(`pty-term[pane="${name}"]`) || document.querySelector("pty-term");
    await customElements.whenDefined("pty-term");
    // The element's park() is defined once it has rendered.
    const park = (name) => requestAnimationFrame(() => term(name)?.park?.());
    park();
    // ?drive replays a key/paste script from the URL hash (base64 JSON, the
    // bench/keyprobe.json shape) as synthesized events on the pane's
    // composition box, exercising the element's real keydown/paste
    // listeners end to end. Test harness affordance, like ?nofit; see
    // bench/keytest.nu.
    if (params.has("drive")) (async () => {
      // atob yields latin1 code units; decode the underlying UTF-8 bytes
      // properly or non-ASCII compose steps arrive double-encoded.
      const spec = JSON.parse(new TextDecoder().decode(
        Uint8Array.from(atob(location.hash.slice(1)), c => c.charCodeAt(0))));
      await new Promise(r => setTimeout(r, spec.startDelay ?? 2000));
      const el = term(spec.pane).querySelector(".compose");
      for (const s of spec.steps) {
        if (s.pause) await new Promise(r => setTimeout(r, s.pause));
        if (s.key !== undefined) {
          el.dispatchEvent(new KeyboardEvent("keydown", {
            key: s.key, code: s.code ?? "",
            shiftKey: !!(s.mods & 1), altKey: !!(s.mods & 2), ctrlKey: !!(s.mods & 4), metaKey: !!(s.mods & 8),
            bubbles: true, cancelable: true,
          }));
        }
        if (s.paste !== undefined) {
          const dt = new DataTransfer();
          dt.setData("text/plain", s.paste);
          el.dispatchEvent(new ClipboardEvent("paste", {clipboardData: dt, bubbles: true, cancelable: true}));
        }
        if (s.compose !== undefined) {
          // Replay a composition session the way a browser fires one:
          // start, provisional keydowns flagged isComposing (which the
          // element must drop), then the finished text on compositionend.
          el.dispatchEvent(new CompositionEvent("compositionstart", {bubbles: true}));
          el.dispatchEvent(new KeyboardEvent("keydown", {key: "Dead", isComposing: true, bubbles: true, cancelable: true}));
          for (const ch of s.compose) {
            el.dispatchEvent(new KeyboardEvent("keydown", {key: ch, isComposing: true, bubbles: true, cancelable: true}));
          }
          el.dispatchEvent(new CompositionEvent("compositionend", {data: s.compose, bubbles: true}));
        }
        await new Promise(r => setTimeout(r, s.gap ?? 60));
      }
    })();
    // Split direction: side by side (row) or stacked. Sticky via
    // localStorage; ?split=stacked/row overrides (handy for links and
    // headless checks). Each pane refits itself when its box changes.
    const panesEl = document.getElementById("panes");
    const splitBtn = document.getElementById("split");
    function setSplit(stacked) {
      panesEl.classList.toggle("stacked", stacked);
      splitBtn.textContent = stacked ? "=" : "||";
      try { localStorage.setItem("split", stacked ? "stacked" : "row"); } catch {}
    }
    splitBtn.addEventListener("click", () => {
      setSplit(!panesEl.classList.contains("stacked"));
      park(document.activeElement?.closest("pty-term")?.getAttribute("pane"));
    });
    const splitParam = params.get("split");
    setSplit(splitParam ? splitParam === "stacked"
      : (() => { try { return localStorage.getItem("split") === "stacked"; } catch { return false; } })());
  </script>
</body></html>'#

let PAGE = (
  $TMPL
  | str replace "__PANES_HTML__" (
      $panes | each {|p|
        $"<div class=pane data-pane=($p.name)><div class=label>($p.name)</div><pty-term pane=($p.name)><div class=scroll><div id=grid-($p.name)>connecting...</div></div></pty-term></div>"
      } | str join ""
    )
)

{|req|
  dispatch $req [
    (route {method: "GET", path: "/"} {|req ctx|
      $PAGE | metadata set --content-type "text/html"
    })

    # One stream for every pane: each stored keyframe replays first
    # (last:1), then live frames. A keyframe is one morph of its pane's
    # grid div. A diff expands to up to three patch events: changed rows +
    # cursor (morph by id), appended rows (append into the pane's grid),
    # trimmed rows (remove by id).
    #
    # The follow starts at the oldest pane's keyframe, as in the panes
    # example, so a connect replays only what came after it rather than the
    # whole store. With no keyframe yet it starts at the head; xs.threshold
    # still comes, and the keyframe request rides on it.
    (route {method: "GET", path: "/sse"} {|req ctx|
      let seeds = $pane_names | each {|n| .last $"pty-($n).screen" } | compact
      let head = .cat --last 1 | get 0?.id?
      # Piped straight from the if: bound to a variable, the follow would be
      # collected and never end.
      if ($seeds | is-not-empty) { .cat --follow --from ($seeds | get id | sort | first) } else if $head != null { .cat --follow --after $head } else { .cat --follow }
      | where topic in $topics or topic == "xs.threshold"
      | each {|f|
          if $f.topic == "xs.threshold" {
            # The replay is done and this follow is live, so a keyframe
            # requested now lands on this stream, with no diffs lost between
            # it and the seed. The stale seed already painted; this replaces
            # it within one coalesce window, instead of at the next heal.
            for p in $pane_names {
              '{"t":"screen"}' + "\n" | .append $"pty-($p).send" --ttl ephemeral | ignore
            }
            []
          } else if ($f.topic | str ends-with ".screen") {
            [(.cas $f.hash | to datastar-patch-elements)]
          } else {
            let d = $f.meta.body | from json
            [
              (if ($d.patch | is-not-empty) { $d.patch | to datastar-patch-elements })
              (if ($d.append | is-not-empty) {
                $d.append | to datastar-patch-elements --mode append --selector $"#($d.target)"
              })
              (if ($d.trim | is-not-empty) {
                "" | to datastar-patch-elements --mode remove --selector ($d.trim | each {|id| $"#($id)"} | str join ",")
              })
            ] | compact
          }
        }
      | flatten
      | to sse
      | metadata set --content-type "text/event-stream"
    })

    # The body is one or more ptyZZZ command frames as NDJSON ({t:key},
    # {t:paste}, {t:input}, {t:resize}), passed through verbatim to the
    # pane's service; the client batches queued frames into one POST.
    (route {method: "POST", path: "/input"} {|req ctx|
      let body = $in | into string | str trim --right --char "\n"
      let pane = $req.query?.pane? | default $pane_names.0
      if $pane in $pane_names {
        # ephemeral: the duplex service reads a keystroke the moment it is
        # written and never again, so storing it only grows the store.
        $body + "\n" | .append $"pty-($pane).send" --ttl ephemeral | ignore
        null | metadata set { merge {'http.response': {status: 204}} }
      } else {
        "unknown pane" | metadata set { merge {'http.response': {status: 400}} }
      }
    })

    # The pty-term element, its styles and font, and the vendored Datastar
    # Rocket bundle.
    (route {method: "GET", path-matches: "/static/:file"} {|req ctx|
      .static ($HERE | path join "static") $"/($ctx.file)"
    })
    (route {method: "GET", path-matches: "/static/vendor/:file"} {|req ctx|
      .static ($HERE | path join "static" "vendor") $"/($ctx.file)"
    })
    (route {method: "GET", path-matches: "/static/fonts/:file"} {|req ctx|
      .static ($HERE | path join "static" "fonts") $"/($ctx.file)"
    })

    # Vendored terminal font. .static sets the content-type.
    (route {method: "GET", path-matches: "/fonts/:file"} {|req ctx|
      .static ($HERE | path join "static" "fonts") $"/($ctx.file)"
    })

    # Probe helper: a pane's current keyframe html as text/plain.
    (route {method: "GET", path: "/snap"} {|req ctx|
      let pane = $req.query?.pane? | default $pane_names.0
      let f = .last $"pty-($pane).screen"
      if ($f | is-empty) { "no screen yet" } else { .cas $f.hash } | metadata set --content-type "text/plain"
    })
  ]
}
