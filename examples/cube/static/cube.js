// Fill each face by picking the font-size that makes the grid span the face
// width, once per face. Font-size (not transform) keeps rendering native, so
// the spinning cube stays smooth. Runs on .fit (never morphed), so it survives
// datastar's per-frame swap of #grid-n. Cell count is fixed, so we recompute
// only when it changes or on resize -- no per-frame work.
function fit(wrap){
  const g = wrap.firstElementChild;
  if(!g || !g.dataset.cols) return;                 // no real frame yet
  const key = g.dataset.cols + 'x' + g.dataset.rows;
  if(wrap.dataset.key === key) return;              // already fitted
  wrap.style.fontSize = '';                         // measure at inherited size
  // scrollWidth, not offsetWidth: the scroll face pins the wrapper's width, so
  // offsetWidth reports the container, not the grid's content
  const nw = Math.max(g.scrollWidth, g.offsetWidth);
  if(!nw) return;
  const face = wrap.parentElement;
  const cur = parseFloat(getComputedStyle(g).fontSize);
  wrap.style.fontSize = (cur * face.clientWidth / nw) + 'px';
  wrap.dataset.key = key;
}
const fits = [...document.querySelectorAll('.fit')];
fits.forEach(w => new MutationObserver(() => fit(w))
  .observe(w, {childList:true, subtree:true, attributes:true}));
addEventListener('resize', () => { fits.forEach(w => { delete w.dataset.key; fit(w); }); });

// The scrollback face follows the tail like a terminal. Follow breaks on
// scroll-back INTENT (an upward wheel or drag), not on position: deriving it
// from position races the pin -- the first ticks of a gesture are still near
// the bottom, so a frame landing mid-gesture would snap the user back.
// Returning to the bottom re-arms it.
document.querySelectorAll('.fit.scroll').forEach(w => {
  let follow = true;
  w.addEventListener('wheel', e => { if (e.deltaY < 0) follow = false; }, {passive: true});
  let touchY = 0;
  w.addEventListener('touchstart', e => { touchY = e.touches[0].clientY; }, {passive: true});
  w.addEventListener('touchmove', e => {
    if (e.touches[0].clientY > touchY) follow = false;
    touchY = e.touches[0].clientY;
  }, {passive: true});
  w.addEventListener('scroll', () => {
    if (w.scrollTop + w.clientHeight >= w.scrollHeight - 8) follow = true;
  });
  new MutationObserver(() => { if (follow) w.scrollTop = w.scrollHeight; })
    .observe(w, {childList: true, subtree: true, characterData: true});
});

// Live per-face metrics in the corner: patches applied and DOM churn over a
// 1s window, measured at the point of truth for client cost -- what this
// browser actually morphs. fps counts mutation batches (~= patch events, so a
// diff frame's patch+append+remove counts as up to 3); kb sums added/changed
// node bytes.
const stats = fits.map(() => ({frames: 0, bytes: 0}));
fits.forEach((w, i) => {
  new MutationObserver(muts => {
    stats[i].frames++;
    for (const m of muts) {
      for (const n of m.addedNodes) stats[i].bytes += (n.outerHTML || n.textContent || '').length;
      if (m.type === 'characterData') stats[i].bytes += m.target.length;
      if (m.type === 'attributes') stats[i].bytes += 16;
    }
  }).observe(w, {childList: true, subtree: true, characterData: true, attributes: true});
});
const mline = document.getElementById('metrics');
setInterval(() => {
  mline.textContent = stats
    .map((s, i) => `f${i} ${s.frames}/s ${(s.bytes / 1024).toFixed(0)}kb`)
    .join('  ');
  stats.forEach(s => { s.frames = 0; s.bytes = 0; });
}, 1000);

// Keystrokes -> the interactive (front) face. POST /input appends a pty0.send
// frame, which the duplex service feeds to that ptyZZZ's stdin. Sends the real
// terminal bytes for as many keys as the browser lets us intercept: control
// chars, Alt/ESC-prefix, arrows, and nav keys.
const NAV = {ArrowUp:'\x1b[A',ArrowDown:'\x1b[B',ArrowRight:'\x1b[C',
  ArrowLeft:'\x1b[D',Home:'\x1b[H',End:'\x1b[F',PageUp:'\x1b[5~',
  PageDown:'\x1b[6~',Insert:'\x1b[2~',Delete:'\x1b[3~',
  Enter:'\r',Backspace:'\x7f',Tab:'\t',Escape:'\x1b'};
addEventListener('keydown', e => {
  if(e.metaKey) return;                       // leave Cmd/Win to the OS
  const k = e.key;
  let b;
  if(e.ctrlKey && k.length===1){
    // Ctrl+char -> control byte (Ctrl-a..z = 0x01..0x1a, etc.)
    const c = k.toLowerCase().charCodeAt(0);
    if(c>=97 && c<=122) b = String.fromCharCode(c-96);
    else if(k===' '||k==='@') b = '\x00';
    else if(k==='[') b = '\x1b';
    else if(k===']') b = '\x1d';
    else if(k==='\\') b = '\x1c';
    else return;
  } else if(e.altKey && k.length===1){
    b = '\x1b' + k;                          // Alt/Meta -> ESC prefix
  } else if(k in NAV){
    b = NAV[k];
  } else if(k.length===1){
    b = k;                                    // printable
  } else return;
  e.preventDefault();
  fetch('/input',{method:'POST',body:b});
});
