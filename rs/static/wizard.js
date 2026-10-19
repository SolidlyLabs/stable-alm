// The first-start welcome wizard (server side: rs/src/onboard.rs). Opens by itself until setup was
// finished or skipped once (installs updated from before it too); Settings → "Run the setup wizard" opens it again.
// Steps: welcome → wallet (create, recovery phrase, private key) → chains → parking → ready → done.
(() => {
  const css = `
#wz{position:fixed;inset:0;z-index:200;display:grid;place-items:center;padding:24px;overflow:auto;overflow-x:hidden;
  background:color-mix(in srgb,var(--bg) 80%,transparent);backdrop-filter:blur(14px) saturate(1.3);animation:wzIn .5s ease both}
#wz.out{animation:wzOut .45s ease both}
@keyframes wzIn{from{opacity:0}to{opacity:1}}@keyframes wzOut{to{opacity:0;transform:scale(1.02)}}
/* the same look, cheap to animate: each orb is blurred once in its own layer and moved by the compositor (one blur
   over the whole aurora was redone every frame), and the grain is a noise tile drawn once on a canvas (grainTile)
   instead of an SVG turbulence filter; together they held the first second of animation */
#wz .aur{position:fixed;inset:0;overflow:hidden;pointer-events:none;z-index:-1;opacity:.55}
#wz .aur i{position:absolute;width:46vmax;height:46vmax;border-radius:50%;mix-blend-mode:screen;filter:blur(60px);will-change:transform;backface-visibility:hidden}
#wz .grain{position:fixed;inset:0;pointer-events:none;z-index:-1;opacity:.06;background-size:160px 160px}
#wz .aur i:nth-child(1){background:radial-gradient(circle,color-mix(in srgb,var(--acc) 55%,transparent),transparent 65%);left:5%;top:10%;animation:wzA 22s ease-in-out infinite}
#wz .aur i:nth-child(2){background:radial-gradient(circle,color-mix(in srgb,var(--info) 50%,transparent),transparent 65%);right:0;top:30%;animation:wzA 28s ease-in-out -8s infinite reverse}
#wz .aur i:nth-child(3){background:radial-gradient(circle,color-mix(in srgb,var(--c4) 40%,transparent),transparent 65%);left:30%;bottom:-10%;animation:wzA 34s ease-in-out -15s infinite}
@keyframes wzA{0%,100%{transform:translate(0,0) scale(1)}33%{transform:translate(8vmax,-6vmax) scale(1.15)}66%{transform:translate(-6vmax,5vmax) scale(.9)}}
#wz .panel{width:min(980px,100%);min-height:600px;display:grid;grid-template-columns:250px 1fr;border:1px solid var(--line2);border-radius:22px;overflow:hidden;
  background:color-mix(in srgb,var(--card) 88%,transparent);box-shadow:0 40px 120px -30px rgba(0,0,0,.7),0 0 0 1px rgba(255,255,255,.03) inset;animation:wzRise .7s cubic-bezier(.2,.8,.2,1) both}
@keyframes wzRise{from{opacity:0;transform:translateY(24px) scale(.98)}to{opacity:1;transform:none}}
#wz .rail{padding:28px 22px;border-right:1px solid var(--line);background:color-mix(in srgb,var(--bg2) 70%,transparent);display:flex;flex-direction:column;gap:6px}
#wz .rail .logo{font:700 15px/1 var(--display);display:flex;align-items:center;gap:10px;margin-bottom:26px}
#wz .rail .logo .sl-logo{height:24px;width:auto;flex:none;color:var(--fg)}#wz .rail .logo .sl-logo path{stroke:currentColor}#wz .rail .logo span{font:600 11px/1 var(--sans);letter-spacing:.06em;text-transform:uppercase;color:var(--mute);padding-left:10px;border-left:1px solid var(--line2)}
#wz .st{display:flex;align-items:center;gap:12px;padding:9px 10px;border-radius:10px;color:var(--mute);font:550 13px var(--sans);position:relative;transition:color .3s,background .3s}
#wz .st .n{width:24px;height:24px;border-radius:50%;display:grid;place-items:center;border:1.5px solid var(--line2);font:700 11px var(--mono);transition:all .35s cubic-bezier(.2,.8,.2,1);flex:none}
#wz .st.on{color:var(--fg);background:var(--acc-soft)}#wz .st.on .n{border-color:var(--acc);color:var(--acc);box-shadow:0 0 0 4px var(--acc-soft)}
#wz .st.done{color:var(--fg2)}#wz .st.done .n{background:var(--acc);border-color:var(--acc);color:var(--acc-ink)}
#wz .st:not(:last-of-type)::after{content:"";position:absolute;left:21px;top:34px;width:1.5px;height:14px;background:var(--line2)}
#wz .st.done::after{background:var(--acc)!important}
#wz .rail .foot{margin-top:auto;font-size:12px;color:var(--mute)}#wz .rail .foot a{cursor:pointer}
#wz .body{position:relative;padding:40px 44px 30px;display:flex;flex-direction:column;overflow:hidden}
#wz .view{flex:1;display:flex;flex-direction:column}
#wz .view.in-r{animation:wzR .45s cubic-bezier(.2,.8,.2,1) both}#wz .view.in-l{animation:wzL .45s cubic-bezier(.2,.8,.2,1) both}
@keyframes wzR{from{opacity:0;transform:translateX(36px)}to{opacity:1;transform:none}}@keyframes wzL{from{opacity:0;transform:translateX(-36px)}to{opacity:1;transform:none}}
#wz .view>*{animation:wzUp .5s cubic-bezier(.2,.8,.2,1) both}
#wz .view>*:nth-child(2){animation-delay:.05s}#wz .view>*:nth-child(3){animation-delay:.1s}#wz .view>*:nth-child(4){animation-delay:.15s}#wz .view>*:nth-child(5){animation-delay:.2s}#wz .view>*:nth-child(6){animation-delay:.25s}
@keyframes wzUp{from{opacity:0;transform:translateY(10px)}to{opacity:1;transform:none}}
#wz .kick{font:700 11px/1 var(--sans);letter-spacing:.14em;text-transform:uppercase;color:var(--acc);margin:0 0 12px}
#wz h1{font:700 34px/1.1 var(--display);letter-spacing:-.02em;margin:0 0 12px}#wz h1 em{font-style:normal;background:linear-gradient(90deg,var(--acc),var(--info));-webkit-background-clip:text;background-clip:text;color:transparent}
#wz h2{font:700 24px/1.2 var(--display);letter-spacing:-.01em;margin:0 0 8px}
#wz .lead{color:var(--fg2);font-size:15px;margin:0 0 24px;max-width:56ch}
#wz .feat{display:grid;grid-template-columns:repeat(3,1fr);gap:12px;margin:8px 0 28px}
#wz .feat div{padding:16px;border:1px solid var(--line);border-radius:14px;background:var(--card2)}#wz .feat b{display:block;margin:8px 0 4px}#wz .feat span{color:var(--mute);font-size:12.5px}
#wz .feat svg{color:var(--acc)}
#wz .opts{display:grid;grid-template-columns:repeat(3,1fr);gap:12px}
#wz .opt{white-space:normal;min-width:0;display:flex;flex-direction:column;align-items:flex-start;justify-content:flex-start;text-align:left;padding:20px 18px;border:1px solid var(--line2);border-radius:16px;background:var(--card2);color:var(--fg);cursor:pointer;
  transition:transform .25s cubic-bezier(.2,.8,.2,1),border-color .25s,box-shadow .25s;font:inherit}
#wz .opt:hover{transform:translateY(-3px);border-color:var(--acc);box-shadow:0 18px 40px -20px color-mix(in srgb,var(--acc) 60%,transparent)}
#wz .opt .ic{width:40px;height:40px;border-radius:12px;display:grid;place-items:center;background:var(--acc-soft);color:var(--acc);margin-bottom:14px}
#wz .opt b{display:block;font-size:15px;margin-bottom:4px}#wz .opt span{color:var(--mute);font-size:12.5px}
#wz .nav{display:flex;align-items:center;gap:10px;margin-top:auto;padding-top:24px}
#wz .nav .sp{flex:1}
#wz .btn{border:0;border-radius:11px;padding:11px 20px;font:650 14px var(--sans);cursor:pointer;background:var(--acc);color:var(--acc-ink);transition:transform .15s,filter .2s,opacity .2s;position:relative;overflow:hidden}
#wz .btn:hover{filter:brightness(1.08)}#wz .btn:active{transform:scale(.97)}#wz .btn:disabled{opacity:.4;cursor:not-allowed;filter:none}
#wz .btn.sec{background:var(--card2);color:var(--fg);border:1px solid var(--line2)}#wz .btn.ghost{background:none;color:var(--mute)}
#wz .btn.big{padding:14px 28px;font-size:15px}
#wz .btn.big::after{content:"";position:absolute;inset:0;background:linear-gradient(110deg,transparent 30%,rgba(255,255,255,.35),transparent 70%);transform:translateX(-100%);animation:wzShine 3s ease-in-out 1s infinite}
@keyframes wzShine{0%,60%{transform:translateX(-100%)}100%{transform:translateX(100%)}}
#wz .box{border:1px solid var(--line2);border-radius:14px;background:var(--card2);padding:16px 18px}
#wz .addr{font:600 15px var(--mono);word-break:break-all}
#wz .secret{font:500 14px/1.6 var(--mono);word-break:break-all;filter:blur(7px);transition:filter .4s;user-select:none;cursor:pointer}
#wz .secret.shown{filter:none;user-select:all}
#wz .warn-box{border:1px solid color-mix(in srgb,var(--warn) 45%,transparent);background:color-mix(in srgb,var(--warn) 9%,transparent);border-radius:12px;padding:12px 14px;font-size:13px;color:var(--fg2);display:flex;gap:10px}
#wz .warn-box svg{color:var(--warn);flex:none;margin-top:1px}
#wz label.chk{display:flex;gap:10px;align-items:flex-start;cursor:pointer;font-size:13.5px;color:var(--fg2)}
#wz input[type=text],#wz input[type=password],#wz textarea{width:100%;background:var(--bg2);border:1px solid var(--line2);color:var(--fg);border-radius:11px;padding:12px 14px;font:14px var(--mono);outline:none;transition:border-color .2s,box-shadow .2s}
#wz input:focus,#wz textarea:focus{border-color:var(--acc);box-shadow:0 0 0 4px var(--acc-soft)}
#wz textarea{min-height:92px;resize:vertical;line-height:1.6}
#wz textarea.masked{-webkit-text-security:disc;text-security:disc}
#wz .words{display:flex;flex-wrap:wrap;gap:6px;margin-top:10px;min-height:30px}
#wz .words span{font:12px var(--mono);padding:4px 9px;border-radius:99px;background:var(--acc-soft);color:var(--fg2);animation:wzPop .3s cubic-bezier(.2,1.4,.4,1) both}
#wz .words span i{font-style:normal;color:var(--mute);margin-right:5px}
@keyframes wzPop{from{opacity:0;transform:scale(.6)}to{opacity:1;transform:none}}
#wz .accts{display:grid;gap:8px;margin-top:14px}
#wz .acct{display:flex;align-items:center;gap:12px;padding:11px 14px;border:1px solid var(--line2);border-radius:12px;cursor:pointer;transition:border-color .2s,background .2s;animation:wzUp .35s both}
#wz .acct:hover{border-color:var(--line)}#wz .acct.sel{border-color:var(--acc);background:var(--acc-soft)}
#wz .acct .dot{width:16px;height:16px;border-radius:50%;border:2px solid var(--line2);flex:none;transition:all .2s}#wz .acct.sel .dot{border:5px solid var(--acc)}
#wz .chains{display:grid;grid-template-columns:repeat(auto-fill,minmax(190px,1fr));gap:10px}
#wz .chain{padding:16px;border:1px solid var(--line2);border-radius:14px;cursor:pointer;transition:all .25s cubic-bezier(.2,.8,.2,1);background:var(--card2);position:relative;user-select:none}
#wz .chain:hover{transform:translateY(-2px)}
#wz .chain.on{border-color:var(--cc);box-shadow:0 0 0 3px color-mix(in srgb,var(--cc) 20%,transparent),0 16px 40px -22px var(--cc)}
#wz .chain .cd{width:34px;height:34px;border-radius:10px;background:color-mix(in srgb,var(--cc) 22%,transparent);color:var(--cc);display:grid;place-items:center;font:800 10px var(--sans);letter-spacing:.04em;margin-bottom:10px}
#wz .chain .tg{position:absolute;right:14px;top:16px;width:34px;height:20px;border-radius:99px;background:var(--line2);transition:background .25s}
#wz .chain .tg::after{content:"";position:absolute;left:3px;top:3px;width:14px;height:14px;border-radius:50%;background:#fff;transition:transform .3s cubic-bezier(.2,1.4,.4,1)}
#wz .chain.on .tg{background:var(--cc)}#wz .chain.on .tg::after{transform:translateX(14px)}
#wz .chain small{color:var(--mute)}
#wz .pgroup{margin-bottom:16px}#wz .pgroup h4{display:flex;align-items:center;gap:8px;margin:0 0 8px;font:650 13px var(--sans)}
#wz .pgroup h4 i{width:8px;height:8px;border-radius:50%;background:var(--cc)}
#wz .prow{display:grid;grid-template-columns:22px 1fr auto;gap:12px;align-items:center;padding:10px 14px;border:1px solid var(--line);border-radius:12px;margin-bottom:6px;cursor:pointer;transition:all .2s;animation:wzUp .4s both}
#wz .prow:hover{border-color:var(--line2)}#wz .prow.on{border-color:color-mix(in srgb,var(--acc) 60%,transparent);background:var(--acc-soft)}
#wz .cb{width:20px;height:20px;border-radius:6px;border:2px solid var(--line2);display:grid;place-items:center;transition:all .2s}
#wz .prow.on .cb{background:var(--acc);border-color:var(--acc)}#wz .cb svg{opacity:0;transform:scale(.4);transition:all .25s cubic-bezier(.2,1.4,.4,1);color:var(--acc-ink)}#wz .prow.on .cb svg{opacity:1;transform:none}
#wz .prow .meta{font:12px var(--mono);color:var(--mute);text-align:right}
#wz .skel{height:46px;border-radius:12px;margin-bottom:6px;background:linear-gradient(90deg,var(--card2),var(--line),var(--card2));background-size:200% 100%;animation:wzSk 1.2s linear infinite}
@keyframes wzSk{to{background-position:-200% 0}}
#wz .seg{display:inline-flex;border:1px solid var(--line2);border-radius:11px;padding:3px;gap:3px;background:var(--bg2)}
#wz .seg button{white-space:nowrap;border:0;background:none;color:var(--mute);padding:8px 14px;border-radius:8px;font:600 13px var(--sans);cursor:pointer;transition:all .2s}
#wz .seg button.on{background:var(--card2);color:var(--fg);box-shadow:0 1px 0 rgba(255,255,255,.05) inset,0 4px 12px -6px rgba(0,0,0,.5)}
#wz .todo{display:grid;gap:10px;margin:6px 0 0}
#wz .todo div{display:flex;gap:12px;align-items:flex-start;padding:12px 14px;border:1px solid var(--line);border-radius:12px}
#wz .todo b{display:block}#wz .todo span{color:var(--mute);font-size:12.5px}
#wz .todo .k{width:26px;height:26px;border-radius:8px;display:grid;place-items:center;background:var(--acc-soft);color:var(--acc);font:700 12px var(--mono);flex:none}
#wz .row2{display:flex;gap:10px;align-items:center;flex-wrap:wrap}
#wz .muted{color:var(--mute);font-size:12.5px}
#wz .err{color:var(--bad);font-size:13px;min-height:18px;margin-top:8px}
/* the welcome rings turn as separate layers on the compositor, so they spin from the first frame even while the
   page is still busy painting the overlay (SVG <animateTransform> waited for the main thread) */
#wz .ring{position:relative;width:120px;height:120px;margin:0 0 22px;flex:none}
#wz .ring svg{position:absolute;inset:0;width:100%;height:100%;will-change:transform;animation:wzSpin var(--d) linear infinite var(--dir,normal)}
#wz .ring circle{fill:none;stroke-width:2}
#wz .ring .dot{position:absolute;left:50%;top:50%;width:16px;height:16px;margin:-8px;border-radius:50%;background:var(--acc);will-change:transform;animation:wzPulse 2.4s ease-in-out infinite}
@keyframes wzSpin{to{transform:rotate(360deg)}}@keyframes wzPulse{0%,100%{transform:scale(.62)}50%{transform:scale(1)}}
#wz .done-ic{width:76px;height:76px;border-radius:50%;display:grid;place-items:center;background:var(--acc);color:var(--acc-ink);margin:10px 0 22px;animation:wzPop .6s cubic-bezier(.2,1.6,.4,1) both;box-shadow:0 0 0 0 var(--acc);animation:wzPop .6s cubic-bezier(.2,1.6,.4,1) both,wzGlow 2.4s ease-out .6s infinite}
@keyframes wzGlow{0%{box-shadow:0 0 0 0 color-mix(in srgb,var(--acc) 60%,transparent)}100%{box-shadow:0 0 0 26px transparent}}
#wz .done-ic svg path{stroke-dasharray:40;stroke-dashoffset:40;animation:wzDraw .5s ease .45s forwards}@keyframes wzDraw{to{stroke-dashoffset:0}}
#wz canvas.conf{position:fixed;inset:0;pointer-events:none;z-index:3}
#wz .spin{width:16px;height:16px;border:2px solid currentColor;border-right-color:transparent;border-radius:50%;display:inline-block;vertical-align:-3px;animation:wzSpin .7s linear infinite}
@keyframes wzSpin{to{transform:rotate(360deg)}}
@media (max-width:820px){#wz .panel{grid-template-columns:1fr}#wz .rail{flex-direction:row;overflow-x:auto;padding:14px;border-right:0;border-bottom:1px solid var(--line)}
  #wz .rail .logo,#wz .rail .foot,#wz .st .lbl{display:none}#wz #wzRail{display:flex;gap:4px;justify-content:center;width:100%}#wz .st::after{display:none}#wz .body{padding:26px 22px}#wz .opts,#wz .feat{grid-template-columns:1fr}#wz h1{font-size:28px}}
@media (prefers-reduced-motion:reduce){#wz *,#wz{animation:none!important;transition:none!important}}`;
  const st = document.createElement("style"); st.textContent = css; document.head.appendChild(st);

  const I = {
    plus: '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M12 5v14M5 12h14"/></svg>',
    words: '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M4 6h7M4 12h10M4 18h6M15 6h5M17 18h3"/></svg>',
    key: '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="15" r="4"/><path d="m10.8 12.2 9.2-9.2M17 6l3 3M14 9l2 2"/></svg>',
    warn: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M12 9v4M12 17h.01M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z"/></svg>',
    check: '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3.5" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12l5 5 9-10"/></svg>',
    bigcheck: '<svg width="36" height="36" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12l5 5 9-10"/></svg>',
    auto: '<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M3 12a9 9 0 0 1 15.5-6.3L21 8M21 3v5h-5M21 12a9 9 0 0 1-15.5 6.3L3 16M3 21v-5h5"/></svg>',
    shield: '<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3 4 6v6c0 5 3.5 8 8 9 4.5-1 8-4 8-9V6z"/><path d="m9 12 2 2 4-4"/></svg>',
    park: '<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M3 17l5-5 4 4 8-8M15 8h5v5"/></svg>',
    copy: '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/></svg>',
  };
  const STEPS = ["Welcome", "Wallet", "Chains", "Parking", "Ready"];
  const W = {step: 0, dir: "r", mode: null, gen: null, shown: false, saved: null, accts: null, pick: 0, phrase: "", pass: "", sugg: null, park: new Set(), parkOn: true, interval: null, info: null};
  const e = s => String(s ?? "").replace(/[&<>"']/g, c => ({"&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;"}[c]));
  const chainColor = id => (typeof cc === "function" ? cc(id) : "var(--acc)");
  const $w = id => document.getElementById(id);
  const call = (p, b) => api(p, b);

  // fractal-ish noise, drawn once: two octaves of random grey with random alpha, as the old SVG turbulence looked at 6% opacity
  let GRAIN = null;
  function grainTile() {
    if (GRAIN) return GRAIN;
    try {
      const n = 160, c = document.createElement("canvas"); c.width = c.height = n;
      const g = c.getContext("2d"), img = g.createImageData(n, n), d = img.data;
      const coarse = new Float32Array(41 * 41).map(() => Math.random());
      for (let y = 0; y < n; y++) for (let x = 0; x < n; x++) {
        const v = Math.round(255 * (.6 * Math.random() + .4 * coarse[(y >> 2) * 41 + (x >> 2)])), i = (y * n + x) * 4;
        // see-through like the SVG turbulence it replaces (its alpha was noise too), not a flat grey veil
        d[i] = d[i + 1] = d[i + 2] = v; d[i + 3] = Math.round(255 * Math.random());
      }
      g.putImageData(img, 0, 0); GRAIN = c.toDataURL("image/png");
    } catch (x) { GRAIN = "none"; }
    return GRAIN;
  }
  async function open(force) {
    try { W.info = await call("/api/onboard"); } catch (x) { return; }
    if (!force && !W.info.needed) return;
    if ($w("wz")) return;
    // an install updated from before the wizard keeps its parking as it is: switched as before, its own pools ticked
    const had = ((W.info.parking || {}).pools || []).length > 0;
    Object.assign(W, {step: 0, dir: "r", mode: null, gen: null, shown: false, saved: W.info.signer.stored ? W.info.signer.address : null, accts: null, pick: 0, phrase: "", pass: "",
                      interval: W.info.interval_s, sugg: null, parkOn: had ? !!W.info.parking.enabled : true, hadPark: had});
    // the Solidly wordmark: the dashboard header's own copy, so the logo lives in one place
    const solidlyMark = () => { const m = document.querySelector(".brand .sl-logo"); return m ? m.outerHTML : "<b>Solidly</b>"; };
    const d = document.createElement("div"); d.id = "wz"; d.setAttribute("role", "dialog"); d.setAttribute("aria-modal", "true"); d.setAttribute("aria-label", "Setup");
    d.innerHTML = `<div class="aur"><i></i><i></i><i></i></div><div class="grain" style="background-image:url(${grainTile()})"></div>
      <div class="panel"><aside class="rail"><div class="logo">${solidlyMark()}<span>Stable ALM</span></div><div id="wzRail"></div>
        <div class="foot">You can change all of this later in Settings.<br><a id="wzSkip">Skip setup</a></div></aside>
        <div class="body" id="wzBody"></div></div>`;
    document.body.appendChild(d);
    $w("wzSkip").onclick = () => finish(false);
    document.addEventListener("keydown", onKey);
    render();
  }
  function onKey(ev) { if (ev.key === "Escape" && W.step === 0) finish(false); }
  function go(n) { W.dir = n > W.step ? "r" : "l"; W.step = n; render(); }
  function rail() {
    $w("wzRail").innerHTML = STEPS.map((s, i) => `<div class="st ${i === W.step ? "on" : i < W.step ? "done" : ""}"><span class="n">${i < W.step ? I.check : i + 1}</span><span class="lbl">${s}</span></div>`).join("");
  }
  function view(html) {
    rail();
    $w("wzBody").innerHTML = `<div class="view in-${W.dir}">${html}</div>`;
    wire();
  }
  const nav = (back, next, nextLabel = "Continue", extra = "") => `<div class="nav">${back != null ? `<button class="btn ghost" data-go="${back}">← Back</button>` : ""}${extra}<span class="sp"></span>
    ${next ? `<button class="btn" id="wzNext" ${next === "disabled" ? "disabled" : ""}>${nextLabel}</button>` : ""}</div>`;
  function wire() { document.querySelectorAll("#wz [data-go]").forEach(b => b.onclick = () => go(+b.dataset.go)); }
  function render() {
    [welcome, wallet, chains, parking, ready, done][W.step]();
  }

  // ---- 0 welcome
  function welcome() {
    view(`<div class="ring">${[52, 40, 28].map((r, i) => `<svg viewBox="0 0 120 120" style="--d:${14 + i * 6}s;--dir:${i % 2 ? "reverse" : "normal"}"><circle cx="60" cy="60" r="${r}"
        stroke="${["var(--acc)", "var(--info)", "var(--c4)"][i]}" stroke-opacity="${.9 - i * .2}" stroke-dasharray="${r * 1.2} ${r * 0.6}"/></svg>`).join("")}<span class="dot"></span></div>
      <p class="kick">Welcome</p>
      <h1>Liquidity that <em>looks after itself</em>.</h1>
      <p class="lead">Stable ALM keeps your Solidly positions in range, moves them when the price does, and parks idle tokens where they still earn. Let's set it up: about two minutes.</p>
      <div class="feat"><div>${I.auto}<b>Automatic ranges</b><span>Re-centres and resizes positions on its own schedule.</span></div>
        <div>${I.park}<b>Idle tokens earn</b><span>Waiting tokens sit in deep pegged pools meanwhile.</span></div>
        <div>${I.shield}<b>Dry run first</b><span>Nothing is sent until you switch to live yourself.</span></div></div>
      <div class="nav"><button class="btn ghost" id="wzLater">Not now</button><span class="sp"></span><button class="btn big" id="wzNext">Get started →</button></div>`);
    $w("wzNext").onclick = () => go(1);
    $w("wzLater").onclick = () => finish(false);
  }

  // ---- 1 wallet
  function wallet() {
    if (W.saved && !W.mode) return walletKept();
    if (!W.mode) {
      view(`<p class="kick">Step 1 · Wallet</p><h2>Which wallet should it manage?</h2>
        <p class="lead">The app signs its own transactions, so it needs a key. It is stored encrypted for your Windows account and never leaves this computer.</p>
        <div class="opts">
          <button class="opt" data-m="create"><div class="ic">${I.plus}</div><b>Create a new wallet</b><span>A fresh wallet just for the ALM. Recommended: keep it apart from your main funds.</span></button>
          <button class="opt" data-m="phrase"><div class="ic">${I.words}</div><b>Recovery phrase</b><span>Import from the 12 or 24 words of MetaMask, Rabby or a hardware wallet.</span></button>
          <button class="opt" data-m="key"><div class="ic">${I.key}</div><b>Private key</b><span>Paste a 64-character hex key exported from another wallet.</span></button></div>
        ${nav(0, null)}`);
      document.querySelectorAll("#wz .opt").forEach(b => b.onclick = () => { W.mode = b.dataset.m; W.dir = "r"; render(); });
      return;
    }
    ({create: walletCreate, phrase: walletPhrase, key: walletKey})[W.mode]();
  }
  const backToChoice = `<button class="btn ghost" id="wzChoice">← Other options</button>`;
  function wireChoice() { const b = $w("wzChoice"); if (b) b.onclick = () => { W.mode = null; W.dir = "l"; W.gen = null; W.shown = false; render(); }; }
  function walletKept() {
    view(`<p class="kick">Step 1 · Wallet</p><h2>Your wallet is ready</h2>
      <p class="lead">The app signs with this wallet. Send it the tokens you want managed, plus a little ETH for gas on each chain you use.</p>
      <div class="box"><div class="muted">Managed wallet</div><div class="row2" style="margin-top:6px"><span class="addr" id="wzAddr">${(window.privAddr ? window.privAddr(W.saved) : e(W.saved))}</span>
        <button class="btn sec" id="wzCopyA" style="padding:7px 12px">${I.copy} Copy</button></div></div>
      ${nav(0, true, "Continue", `<button class="btn ghost" id="wzOther">Use another wallet</button>`)}`);
    $w("wzCopyA").onclick = ev => copy(W.saved, ev.currentTarget);
    $w("wzOther").onclick = () => { W.saved = null; W.mode = null; render(); };
    $w("wzNext").onclick = () => go(2);
  }
  async function walletCreate() {
    if (!W.gen) {
      view(`<p class="kick">Step 1 · New wallet</p><h2><span class="spin"></span> Making your wallet…</h2>`);
      try { W.gen = await call("/api/onboard/generate", {}); } catch (x) { view(`<h2>Could not make a key</h2><p class="err">${e(x.message)}</p>${nav(null, null, "", backToChoice)}`); wireChoice(); return; }
    }
    const g = W.gen;
    view(`<p class="kick">Step 1 · New wallet</p><h2>Save your private key</h2>
      <p class="lead">This key <b>is</b> the wallet. Whoever has it controls the funds; if you lose it and this computer, they are gone. Store it in a password manager before you continue.</p>
      <div class="box" style="margin-bottom:12px"><div class="muted">Address</div><div class="addr" style="margin:4px 0 14px">${e(g.address)}</div>
        <div class="row2" style="justify-content:space-between"><span class="muted">Private key</span>
          <span class="row2"><button class="btn sec" id="wzShow" style="padding:7px 12px">${W.shown ? "Hide" : "Reveal"}</button>
          <button class="btn sec" id="wzCopyK" style="padding:7px 12px">${I.copy} Copy key</button></span></div>
        <div class="secret ${W.shown ? "shown" : ""}" id="wzSecret" title="click to reveal" style="margin-top:8px">${e(g.private_key)}</div></div>
      <div class="warn-box">${I.warn}<span>Never paste it into a website or share it with anyone, including support. Clear your clipboard after saving it.</span></div>
      <label class="chk" style="margin-top:14px"><input type="checkbox" id="wzAck"> I have stored the private key somewhere safe.</label>
      <div style="margin-top:12px;max-width:340px"><label class="muted">To check, type the last 4 characters of the key</label><input type="text" id="wzLast" maxlength="4" autocomplete="off" spellcheck="false" style="margin-top:6px"></div>
      <div class="err" id="wzErr"></div>
      ${nav(null, "disabled", "Use this wallet", backToChoice)}`);
    wireChoice();
    const toggle = () => { W.shown = !W.shown; $w("wzSecret").classList.toggle("shown", W.shown); $w("wzShow").textContent = W.shown ? "Hide" : "Reveal"; };
    $w("wzShow").onclick = toggle; $w("wzSecret").onclick = () => { if (!W.shown) toggle(); };
    $w("wzCopyK").onclick = ev => copy(g.private_key, ev.currentTarget);
    const check = () => { $w("wzNext").disabled = !($w("wzAck").checked && $w("wzLast").value.toLowerCase() === g.private_key.slice(-4).toLowerCase()); };
    $w("wzAck").onchange = check; $w("wzLast").oninput = check;
    $w("wzNext").onclick = () => saveKey(g.private_key);
  }
  async function saveKey(k) {
    const b = $w("wzNext"); b.disabled = true; b.innerHTML = `<span class="spin"></span> Saving`;
    try { const r = await call("/api/key", {private_key: k}); W.saved = r.address; W.gen = null; W.mode = null; go(2); }
    catch (x) { $w("wzErr").textContent = x.message; b.disabled = false; b.textContent = "Try again"; }
  }
  function walletKey() {
    view(`<p class="kick">Step 1 · Import a private key</p><h2>Paste the private key</h2>
      <p class="lead">64 hexadecimal characters, with or without 0x. It is checked, then stored encrypted for your Windows account.</p>
      <div class="row2" style="flex-wrap:nowrap"><input type="password" id="wzPk" autocomplete="off" spellcheck="false" placeholder="0x…">
        <button class="btn sec" id="wzEye" style="padding:11px 14px">Show</button></div>
      <div class="muted" id="wzPkInfo" style="margin-top:8px">&nbsp;</div><div class="err" id="wzErr"></div>
      ${nav(null, "disabled", "Import", backToChoice)}`);
    wireChoice();
    const pk = $w("wzPk");
    $w("wzEye").onclick = () => { pk.type = pk.type === "password" ? "text" : "password"; $w("wzEye").textContent = pk.type === "password" ? "Show" : "Hide"; };
    pk.oninput = () => { const v = pk.value.trim().replace(/^0x/i, ""); const ok = /^[0-9a-fA-F]{64}$/.test(v);
      $w("wzNext").disabled = !ok; $w("wzPkInfo").innerHTML = !v ? "&nbsp;" : ok ? `<span style="color:var(--ok)">Looks like a key ✓</span>` : `${v.length} of 64 characters`; };
    $w("wzNext").onclick = () => saveKey(pk.value.trim());
    pk.focus();
  }
  function walletPhrase() {
    view(`<p class="kick">Step 1 · Recovery phrase</p><h2>Enter your recovery phrase</h2>
      <p class="lead">The words in order, separated by spaces. The app derives the standard Ethereum accounts (m/44'/60'/0'/0/n) and shows them so you can pick yours.</p>
      <textarea id="wzPh" class="masked" spellcheck="false" autocomplete="off" placeholder="word1 word2 word3 …">${e(W.phrase)}</textarea>
      <div class="row2" style="justify-content:space-between"><div class="words" id="wzWords"></div><button class="btn ghost" id="wzPhEye" style="padding:6px 10px">Show words</button></div>
      <details style="margin-top:8px"><summary class="muted" style="cursor:pointer">Passphrase (only if you set a 25th word)</summary>
        <input type="password" id="wzPass" autocomplete="off" style="margin-top:8px" value="${e(W.pass)}"></details>
      <div id="wzAccts" class="accts"></div><div class="err" id="wzErr"></div>
      ${nav(null, "disabled", "Find my accounts", backToChoice)}`);
    wireChoice();
    const ta = $w("wzPh");
    $w("wzPhEye").onclick = () => { const m = ta.classList.toggle("masked"); $w("wzPhEye").textContent = m ? "Show words" : "Hide words"; };
    const words = () => ta.value.trim().split(/\s+/).filter(Boolean);
    const draw = () => { const w = words(); $w("wzWords").innerHTML = w.map((x, i) => `<span><i>${i + 1}</i>${"•".repeat(Math.min(x.length, 8))}</span>`).join("");
      const ok = [12, 15, 18, 21, 24].includes(w.length);
      if (!W.accts) { $w("wzNext").disabled = !ok; $w("wzNext").textContent = ok ? "Find my accounts" : `${w.length} words`; } };
    ta.oninput = () => { W.accts = null; $w("wzAccts").innerHTML = ""; draw(); };
    draw();
    $w("wzNext").onclick = async () => {
      if (W.accts) { const b = $w("wzNext"); b.disabled = true; b.innerHTML = `<span class="spin"></span> Saving`;
        try { const r = await call("/api/onboard/phrase_save", {phrase: ta.value, passphrase: $w("wzPass").value, index: W.pick});
          W.saved = r.address; W.accts = null; W.mode = null; W.phrase = ""; W.pass = ""; ta.value = ""; go(2); }
        catch (x) { $w("wzErr").textContent = x.message; b.disabled = false; b.textContent = "Use this account"; } return; }
      const b = $w("wzNext"); b.disabled = true; b.innerHTML = `<span class="spin"></span> Deriving`;
      try { const r = await call("/api/onboard/phrase", {phrase: ta.value, passphrase: $w("wzPass").value, count: 5});
        W.accts = r.addresses; W.pick = 0; $w("wzErr").textContent = "";
        $w("wzAccts").innerHTML = `<div class="muted">Pick the account to manage (a mistyped word gives different addresses: check it is yours):</div>` +
          W.accts.map((a, i) => `<div class="acct ${i === 0 ? "sel" : ""}" data-i="${a.index}" style="animation-delay:${i * .05}s"><span class="dot"></span><span class="mono" style="font-size:13px">${e(a.address)}</span><span class="sp" style="flex:1"></span><span class="muted">#${a.index}</span></div>`).join("");
        document.querySelectorAll("#wz .acct").forEach(el => el.onclick = () => { W.pick = +el.dataset.i; document.querySelectorAll("#wz .acct").forEach(x => x.classList.toggle("sel", x === el)); });
        b.disabled = false; b.textContent = "Use this account"; }
      catch (x) { $w("wzErr").textContent = x.message; b.disabled = false; b.textContent = "Find my accounts"; }
    };
    ta.focus();
  }

  // ---- 2 chains
  async function chains() {
    let list = (W.info && W.info.chains) || [];
    try { list = (await call("/api/chains")).chains; } catch (x) {}
    const names = {1: "ETH", 10: "OP", 8453: "BASE", 42161: "ARB", 146: "S"};
    view(`<p class="kick">Step 2 · Chains</p><h2>Where do you want to provide liquidity?</h2>
      <p class="lead">Each chain you turn on is watched every cycle. Base has the deepest Solidly pools and the cheapest gas; start there if unsure.</p>
      <div class="chains">${list.map((c, i) => `<div class="chain ${c.enabled ? "on" : ""}" data-id="${c.id}" style="--cc:${chainColor(c.id)};animation:wzUp .4s ${i * .05}s both">
        <div class="cd">${names[c.id] || c.id}</div><b>${e(c.name)}</b><br><small>gas: ${e(c.gas_symbol || "ETH")}</small><span class="tg"></span></div>`).join("")}</div>
      <div class="err" id="wzErr"></div>${nav(1, true)}`);
    document.querySelectorAll("#wz .chain").forEach(el => el.onclick = async () => {
      const on = !el.classList.contains("on"); el.classList.toggle("on", on);
      try { await call("/api/chains", {id: +el.dataset.id, enabled: on}); $w("wzErr").textContent = ""; }
      catch (x) { el.classList.toggle("on", !on); $w("wzErr").textContent = x.message; }
    });
    $w("wzNext").onclick = () => { if (!document.querySelector("#wz .chain.on")) { $w("wzErr").textContent = "Turn on at least one chain."; return; } W.sugg = null; go(3); };
  }

  // ---- 3 parking
  async function parking() {
    view(`<p class="kick">Step 3 · Parking</p><h2>Let idle tokens keep earning</h2>
      <p class="lead">While a strategy waits out of the market, its stablecoins can sit in a deep pegged pool and earn fees. These are the most liquid pools for each token on your chains; untick any you don't want.</p>
      <div class="row2" style="margin-bottom:14px"><label class="chk" style="align-items:center"><input type="checkbox" id="wzParkOn" ${W.parkOn ? "checked" : ""}> Park waiting tokens</label></div>
      <div id="wzPark" style="max-height:340px;overflow:auto;padding-right:4px">${"<div class='skel'></div>".repeat(4)}</div>
      <p class="muted" style="margin:8px 0 0">A pool parks only when its strategy has <b>Park</b> on (Pools tab, ⚙). One pool per token and chain.</p>
      <div class="err" id="wzErr"></div>${nav(2, true)}`);
    $w("wzParkOn").onchange = ev => { W.parkOn = ev.target.checked; $w("wzPark").style.opacity = W.parkOn ? 1 : .4; };
    $w("wzNext").onclick = saveParking;
    if (!W.sugg) {
      try { W.sugg = (await call("/api/onboard/parking")).chains; } catch (x) { $w("wzPark").innerHTML = `<p class="err">${e(x.message)}</p>`; return; }
      W.park = new Set(); if (!W.hadPark) W.sugg.forEach(c => c.pools.forEach(p => W.park.add(p.chain_id + ":" + p.token)));
      ((W.info.parking || {}).pools || []).forEach(p => W.park.add(p.chain_id + ":" + p.token));
    }
    if (!$w("wzPark")) return;
    $w("wzPark").innerHTML = W.sugg.map(c => `<div class="pgroup" style="--cc:${chainColor(c.chain_id)}"><h4><i></i>${e(c.chain)}</h4>
      ${c.error ? `<p class="muted">Pool list unavailable: ${e(c.error)}</p>` : !c.pools.length ? `<p class="muted">No deep pegged pool here yet.</p>`
        : c.pools.map((p, i) => { const k = p.chain_id + ":" + p.token; return `<div class="prow ${W.park.has(k) ? "on" : ""}" data-k="${e(k)}" style="animation-delay:${i * .05}s">
          <span class="cb">${I.check}</span><span><b>${e(p.symbol)}</b> <span class="muted">→ ${e(p.pair)}</span></span>
          <span class="meta">TVL ${usd(p.tvl_usd)}<br>${fmt(p.apr || 0, 1)}% APR</span></div>`; }).join("")}</div>`).join("");
    document.querySelectorAll("#wz .prow").forEach(el => el.onclick = () => { const k = el.dataset.k; W.park.has(k) ? W.park.delete(k) : W.park.add(k); el.classList.toggle("on", W.park.has(k)); });
    $w("wzPark").style.opacity = W.parkOn ? 1 : .4;
  }
  async function saveParking() {
    const b = $w("wzNext"); b.disabled = true; b.innerHTML = `<span class="spin"></span> Saving`;
    const old = ((W.info && W.info.parking) || {}).pools || [];
    const pools = [];
    (W.sugg || []).forEach(c => c.pools.forEach(p => { if (W.park.has(p.chain_id + ":" + p.token)) pools.push({chain_id: p.chain_id, token: p.token, pool: p.pool}); }));
    old.forEach(p => { if (W.park.has(p.chain_id + ":" + p.token) && !pools.some(q => q.chain_id === p.chain_id && q.token === p.token)) pools.push(p); });
    try { await call("/api/settings", {parking: {enabled: W.parkOn && pools.length > 0, pools}}); go(4); }
    catch (x) { $w("wzErr").textContent = x.message; b.disabled = false; b.textContent = "Continue"; }
  }

  // ---- 4 ready
  function ready() {
    const iv = W.interval || 300;
    view(`<p class="kick">Step 4 · Ready</p><h2>Almost there</h2>
      <p class="lead">The app starts in <b>dry run</b>: it plans every move and logs it, but sends nothing until you press <b>Go live</b> in the header.</p>
      <div class="row2" style="margin-bottom:18px"><span class="muted">Check the pools every</span><div class="seg" id="wzIv">${[[120, "2 min"], [300, "5 min"], [900, "15 min"]].map(([s, l]) =>
        `<button data-s="${s}" class="${s === iv ? "on" : ""}">${l}</button>`).join("")}</div></div>
      <div class="todo">
        <div><span class="k">1</span><span><b>Fund the wallet</b><span>${W.saved ? `Send tokens and a little gas to <span class="mono">${(window.privAddr ? window.privAddr(W.saved) : e(W.saved))}</span>.` : "Add a key in Settings, then send it tokens and a little gas."}</span></span></div>
        <div><span class="k">2</span><span><b>Add a pool</b><span>Pools tab: pick a pegged pair, give it capital, test its strategy in the Strategy lab.</span></span></div>
        <div><span class="k">3</span><span><b>Watch a few dry-run cycles, then go live</b><span>The Activity tab shows what it would have done.</span></span></div></div>
      <div class="err" id="wzErr"></div>${nav(3, true, "Finish setup")}`);
    document.querySelectorAll("#wzIv button").forEach(b => b.onclick = () => { W.interval = +b.dataset.s; document.querySelectorAll("#wzIv button").forEach(x => x.classList.toggle("on", x === b)); });
    $w("wzNext").onclick = async () => {
      const b = $w("wzNext"); b.disabled = true; b.innerHTML = `<span class="spin"></span> Finishing`;
      try { if (W.interval && W.interval !== W.info.interval_s) await call("/api/settings", {interval_s: W.interval}); await call("/api/onboard/done", {}); go(5); }
      catch (x) { $w("wzErr").textContent = x.message; b.disabled = false; b.textContent = "Finish setup"; }
    };
  }

  // ---- 5 done
  function done() {
    W.dir = "r";
    view(`<div class="done-ic">${I.bigcheck.replace("<path", '<path pathLength="40"')}</div>
      <p class="kick">All set</p><h1>You're <em>ready to go</em>.</h1>
      <p class="lead">Next: add your first pool. The app keeps running in the tray; open the dashboard from its icon any time.</p>
      <div class="nav"><button class="btn sec" id="wzDash">Open the dashboard</button><span class="sp"></span><button class="btn big" id="wzPools">Add a pool →</button></div>`);
    $w("wzRail").querySelectorAll(".st").forEach(s => { s.classList.remove("on"); s.classList.add("done"); s.querySelector(".n").innerHTML = I.check; });
    confetti();
    $w("wzDash").onclick = () => close("overview");
    $w("wzPools").onclick = () => close("pools");
  }

  async function finish(completed) {
    if (!completed) { try { await call("/api/onboard/done", {}); } catch (x) {} }
    close();
  }
  function close(tab) {
    const d = $w("wz"); if (!d) return;
    document.removeEventListener("keydown", onKey);
    W.gen = null; W.phrase = ""; W.pass = "";
    // the dashboard does not refresh behind the wizard (index.html: refresh): catch up once it is gone
    d.classList.add("out"); setTimeout(() => { d.remove(); if (typeof refresh === "function") refresh(); }, 450);
    if (tab && typeof show === "function") show(tab);
  }
  async function copy(text, btn) {
    try { await navigator.clipboard.writeText(text); const t = btn.innerHTML; btn.innerHTML = `${I.check} Copied`; btn.style.color = "var(--ok)";
      setTimeout(() => { btn.innerHTML = t; btn.style.color = ""; }, 1600); }
    catch (x) { if (typeof toast === "function") toast("Copy failed: select the text and copy it by hand.", true); }
  }
  function confetti() {
    if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const c = document.createElement("canvas"); c.className = "conf"; $w("wz").appendChild(c);
    const x = c.getContext("2d"), dpr = devicePixelRatio || 1; c.width = innerWidth * dpr; c.height = innerHeight * dpr; x.scale(dpr, dpr);
    const css = getComputedStyle(document.documentElement), cols = ["--acc", "--info", "--c4", "--warn", "--c6"].map(v => css.getPropertyValue(v).trim() || "#38e0a6");
    const ps = Array.from({length: 160}, () => ({x: innerWidth / 2, y: innerHeight / 2.4, vx: (Math.random() - .5) * 16, vy: -Math.random() * 15 - 4, r: Math.random() * 6 + 3,
      a: Math.random() * 6, va: (Math.random() - .5) * .3, c: cols[Math.random() * cols.length | 0], s: Math.random() > .5}));
    let f = 0;
    (function tick() {
      x.clearRect(0, 0, innerWidth, innerHeight);
      ps.forEach(p => { p.vy += .38; p.vx *= .99; p.x += p.vx; p.y += p.vy; p.a += p.va; x.save(); x.translate(p.x, p.y); x.rotate(p.a); x.fillStyle = p.c;
        x.globalAlpha = Math.max(0, 1 - f / 150); p.s ? x.fillRect(-p.r / 2, -p.r / 4, p.r, p.r / 2) : (x.beginPath(), x.arc(0, 0, p.r / 2.4, 0, 7), x.fill()); x.restore(); });
      if (++f < 150) requestAnimationFrame(tick); else c.remove();
    })();
  }
  window.openWizard = () => open(true);
  // the first status call decides; the dashboard's own sign-in comes first
  setTimeout(() => open(false), 400);
})();
