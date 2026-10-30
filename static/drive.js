// Test harness for bench/keytest.nu, loaded by the root page only with
// ?drive: replays a key/paste script from the URL hash (base64 JSON, the
// bench/keyprobe.json shape) as synthesized events on the terminal's
// composition box, exercising <pty-term>'s real keydown/paste listeners.
// atob yields latin1 code units; decode the underlying UTF-8 bytes
// properly or non-ASCII compose steps arrive double-encoded.
const spec = JSON.parse(new TextDecoder().decode(
  Uint8Array.from(atob(location.hash.slice(1)), c => c.charCodeAt(0))));
await new Promise(r => setTimeout(r, spec.startDelay ?? 2000));
const el = document.querySelector("pty-term .compose");
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
