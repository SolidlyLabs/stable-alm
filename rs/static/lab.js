// Strategy lab: pick or build a strategy (simple rules, or a node graph with indicators), backtest
// it on the pool's real swaps against strategies you choose from the library, save it under a name,
// and pick it for a pool in the Pools view. The backtest runs in the app (src/backtest.rs) with the same decision code
// as the worker (custom.rs, graph.rs); every change re-runs it.
// Uses the dashboard's helpers ($, api, esc, fmt, toast, chainOf, chains, pchain, show, S, MARKET, refresh).
(function () {
  const css = document.createElement("style");
  css.textContent = `
  .lab-top{display:flex;flex-wrap:wrap;gap:12px 16px;align-items:flex-end}
  .lab-top .grow{flex:1;min-width:240px}
  .seg{display:inline-flex;border:1px solid var(--line2);border-radius:10px;overflow:hidden;background:var(--card2)}
  .seg button{background:none;color:var(--mute);border-radius:0;padding:7px 11px;font-weight:600;border-right:1px solid var(--line)}
  .seg button:last-child{border-right:0}.seg button.on{background:var(--acc-soft);color:var(--fg)}
  .lab-main{display:grid;grid-template-columns:minmax(0,1fr) 352px;gap:14px;align-items:start}
  .lab-main>.mid{min-width:0}
  .res{position:sticky;top:110px;max-height:calc(100vh - 124px);overflow:auto}
  @media (max-width:900px){.lab-main{grid-template-columns:1fr}.res{position:static;max-height:none}}
  .res{padding:14px}
  .res .rh{display:flex;align-items:baseline;gap:8px;margin-bottom:4px}.res .rh h2{margin:0}
  .rc2{border:1px solid var(--line);border-radius:11px;padding:9px 11px;margin-bottom:8px;background:var(--card2)}
  .rc2.main{border-color:color-mix(in srgb,var(--acc) 45%,var(--line2))}
  .rc2 .t{display:flex;align-items:center;gap:7px;font-weight:650;font-size:13px}.rc2 .t i{width:9px;height:9px;border-radius:3px;flex:none}
  .rc2 .t span.n{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .rc2 .big{font:650 17px var(--mono);text-align:right}.rc2 .apr{font-size:11px;color:var(--mute);text-align:right}
  .rc2 .g{display:grid;grid-template-columns:repeat(4,1fr);gap:6px 8px;margin-top:7px}
  .rc2 .g div{font-size:10.5px;color:var(--mute)}.rc2 .g b{display:block;font:550 12px var(--mono);color:var(--fg2)}
  .rc2 .g b.best,.rc2 .big.best{color:var(--ok)}
  .of{margin-top:9px;padding-top:8px;border-top:1px dashed var(--line)}
  .of .oh{display:flex;align-items:center;gap:8px;font-size:11px;color:var(--mute);text-transform:uppercase;letter-spacing:.06em}
  .of .ofs{margin-left:auto;display:inline-flex;align-items:baseline;gap:6px;cursor:help;padding:2px 8px;border-radius:999px;border:1px solid currentColor;text-transform:none;letter-spacing:0}
  .of .ofs b{font:650 14px var(--mono)}.of .ofs span{font-size:11px}
  .of .ofs.good{color:var(--ok)}.of .ofs.mid{color:var(--warn)}.of .ofs.badv{color:var(--bad)}.of .ofs.none{color:var(--mute)}
  .of .g{grid-template-columns:repeat(2,1fr)}
  .of .spin{display:inline-block;width:10px;height:10px;border-radius:50%;border:2px solid var(--line2);border-top-color:var(--acc);animation:spin .8s linear infinite;align-self:center}
  .of .bars{display:flex;align-items:center;gap:3px;height:34px;margin-top:8px}
  .of .bars i{flex:1;position:relative;height:100%}
  .of .bars i::after{content:"";position:absolute;left:0;right:0;border-radius:2px}
  .of .bars i.bp::after{bottom:50%;height:var(--h);background:var(--ok)}.of .bars i.bn::after{top:50%;height:var(--h);background:var(--bad)}
  .of .bars{background:linear-gradient(var(--line),var(--line)) center/100% 1px no-repeat}
  .of .foot{font-size:10.5px;color:var(--mute);margin-top:5px}
  #labTip{position:fixed;z-index:60;max-width:330px;padding:11px 13px;border-radius:10px;background:var(--card);border:1px solid var(--line);box-shadow:0 10px 30px rgba(0,0,0,.28);
    font-size:12px;line-height:1.5;color:var(--fg2);pointer-events:none;opacity:0;transform:translateY(-3px);transition:opacity .12s,transform .12s}
  #labTip.on{opacity:1;transform:none}#labTip h4{margin:0 0 5px;font-size:12.5px;color:var(--fg)}#labTip p{margin:0 0 6px}#labTip p:last-child{margin:0}
  #labTip code{font-size:11px}
  .rc2 .sub{font-size:11px;color:var(--mute);font-weight:400}
  .rc2[data-drag]{cursor:grab;position:relative;transition:transform .12s,opacity .12s}
  .rc2 .grip{color:var(--mute);font-size:11px;letter-spacing:-2px;margin:1px 2px 0 -4px;opacity:.45;cursor:grab;user-select:none}
  .rc2[data-drag]:hover .grip{opacity:1}
  .rc2.dragging{opacity:.35;transform:scale(.98)}
  .rc2.drop-b::before,.rc2.drop-a::after{content:"";position:absolute;left:6px;right:6px;height:3px;border-radius:2px;background:var(--acc);box-shadow:0 0 10px var(--acc)}
  .rc2.drop-b::before{top:-6px}.rc2.drop-a::after{bottom:-6px}
  .rc2 .cx{align-self:flex-start;margin:-4px -5px 0 2px;padding:0 5px;border-radius:6px;color:var(--mute);cursor:pointer;font-size:15px;line-height:1.3}
  .rc2 .cx:hover{color:var(--bad);background:var(--card)}
  .res .pick{margin:2px 0 12px}.res .pick button{width:100%;justify-content:center}
  .lib h3{font:650 10.5px/1.2 var(--sans);letter-spacing:1px;text-transform:uppercase;color:var(--mute);margin:14px 6px 6px}
  .lib h3:first-of-type{margin-top:6px}
  .lib .it{display:block;width:100%;text-align:left;background:none;color:var(--fg2);border:1px solid transparent;border-radius:9px;padding:7px 9px;font-weight:550;white-space:normal}
  .lib .it:hover{background:var(--card2);filter:none}.lib .it.on{background:var(--acc-soft);border-color:color-mix(in srgb,var(--acc) 40%,var(--line2));color:var(--fg)}
  .lib .it small{display:block;color:var(--mute);font-weight:400;font-size:11.5px;line-height:1.35;margin-top:2px;display:-webkit-box;-webkit-line-clamp:2;-webkit-box-orient:vertical;overflow:hidden}
  .lib .tag{font:600 9.5px var(--sans);letter-spacing:.5px;text-transform:uppercase;padding:1px 6px;border-radius:99px;margin-left:6px;vertical-align:1px;background:var(--card2);color:var(--mute)}
  .ed-head{display:flex;flex-wrap:wrap;gap:8px 10px;align-items:center;margin-bottom:12px}
  .ed-head input.nm{font:650 15px var(--sans);background:transparent;border:1px solid transparent;padding:4px 6px;width:auto;min-width:200px;flex:1}
  .ed-head input.nm:hover,.ed-head input.nm:focus{border-color:var(--line2);background:var(--card2)}
  .ed-desc{font-size:12.5px;color:var(--mute);margin:-4px 0 12px}
  .dirty{color:var(--warn);font-size:12px}
  .grp{border-top:1px solid var(--line);padding:12px 0 4px}.grp:first-of-type{border-top:0;padding-top:0}
  .grp>.gh{display:flex;align-items:center;gap:8px;margin-bottom:10px}
  .grp>.gh b{font:650 11px/1.2 var(--sans);letter-spacing:1px;text-transform:uppercase;color:var(--fg2)}
  .grp>.gh .sw{margin-left:auto}
  .grp .key{width:10px;height:10px;border-radius:3px;flex:none}
  .lf{display:grid;grid-template-columns:repeat(auto-fill,minmax(170px,1fr));gap:10px 12px}
  .lf .full{grid-column:1/-1}
  .lf label.f{display:flex;justify-content:space-between;gap:6px}.lf label.f i{font-style:normal;color:var(--fg2);font-family:var(--mono);font-size:11px}
  .lf input[type=range]{width:100%;margin-top:6px}
  .grp.off .lf{opacity:.4;pointer-events:none}
  /* node editor */
  .ne{display:block;border:1px solid var(--line);border-radius:12px;overflow:hidden;background:var(--bg2)}
  .ne .cv{position:relative;overflow:hidden;height:600px;cursor:default;touch-action:none;
    background-image:radial-gradient(color-mix(in srgb,var(--fg) 9%,transparent) 1px,transparent 1px);background-size:18px 18px}
  .ne .cv:focus{outline:none}
  .ne .area{position:absolute;left:0;top:0;transform-origin:0 0}
  .ne .cv.pan{cursor:grabbing}.ne .cv.pan .nd{pointer-events:none}
  .ne .mm{position:absolute;right:10px;bottom:10px;width:168px;height:108px;border-radius:9px;border:1px solid var(--line2);
    background:color-mix(in srgb,var(--card) 88%,transparent);backdrop-filter:blur(4px);cursor:pointer;touch-action:none;overflow:hidden}
  .ne .mm svg{width:100%;height:100%;display:block}
  .ne .zoom{display:none;position:absolute;right:10px;bottom:126px;display:flex;flex-direction:column;border:1px solid var(--line2);border-radius:9px;overflow:hidden;background:var(--card)}
  .ne .zoom button{background:none;color:var(--fg2);border-radius:0;padding:4px 0;width:30px;font-weight:650;border-bottom:1px solid var(--line)}
  .ne .zoom button:last-child{border-bottom:0;font-size:10.5px}.ne .zoom button:hover{background:var(--card2);filter:none}
  .ne svg.wires{position:absolute;inset:0;pointer-events:none;overflow:visible}
  .ne svg.wires path{fill:none;stroke-width:2.2;pointer-events:stroke;cursor:pointer}
  .ne svg.wires path:hover{stroke-width:4;filter:brightness(1.3)}
  .nd{position:absolute;width:176px;background:var(--card);border:1px solid var(--line2);border-radius:11px;box-shadow:0 10px 28px -14px rgba(0,0,0,.6);font-size:12px;user-select:none}
  .nd.sel{border-color:var(--acc);box-shadow:0 0 0 3px var(--acc-soft),0 10px 28px -14px rgba(0,0,0,.6)}
  .nd.bad{border-color:var(--bad)}
  .nd .hd{display:flex;align-items:center;gap:6px;padding:7px 9px;border-bottom:1px solid var(--line);cursor:grab;border-radius:11px 11px 0 0;touch-action:none}
  .nd .hd i{width:8px;height:8px;border-radius:2px;flex:none}.nd .hd b{flex:1;font-weight:650;font-size:12px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .nd .hd .x{color:var(--mute);cursor:pointer;padding:0 2px;font-size:14px;line-height:1}.nd .hd .x:hover{color:var(--bad)}
  .nd .bd{padding:6px 0 8px}
  .nd .pr{display:flex;align-items:center;justify-content:space-between;height:22px;position:relative;padding:0 12px;color:var(--fg2)}
  .nd .pr .dot{position:absolute;width:12px;height:12px;border-radius:50%;border:2px solid var(--card);top:5px;cursor:crosshair}
  .nd .pr .in{left:-7px}.nd .pr .out{right:-7px}
  .nd .pr .dot:hover{transform:scale(1.3)}
  .nd .pr.opt span:first-child{color:var(--mute);font-style:italic}
  .nd .pm{display:grid;grid-template-columns:1fr 74px;gap:4px 6px;align-items:center;padding:4px 10px 0;color:var(--mute)}
  .nd .pm input,.nd .pm select{padding:3px 6px;font-size:12px;border-radius:6px}
  .ne .hint{position:absolute;left:12px;bottom:10px;right:190px;font-size:11.5px;color:var(--mute);pointer-events:none}
  @media (max-width:700px){.ne .cv{height:460px}.ne .mm{display:none}.ne .hint{display:none}}
  .cm{position:fixed;z-index:60;min-width:200px;max-width:260px;background:var(--card);border:1px solid var(--line2);border-radius:11px;
    box-shadow:0 18px 40px -16px rgba(0,0,0,.7);padding:5px;font-size:12.5px}
  .cm .ci{display:flex;align-items:center;gap:8px;padding:6px 9px;border-radius:7px;color:var(--fg2);cursor:pointer;white-space:nowrap}
  .cm .ci:hover,.cm .ci.on{background:var(--acc-soft);color:var(--fg)}.cm .ci i{width:8px;height:8px;border-radius:2px;flex:none}
  .cm .ci kbd{margin-left:auto;font:10.5px var(--mono);color:var(--mute)}.cm .ci.bad{color:var(--bad)}
  .cm .sep{height:1px;background:var(--line);margin:4px 2px}
  .cm .gl{font:650 9.5px var(--sans);letter-spacing:.8px;text-transform:uppercase;color:var(--mute);padding:7px 9px 3px}
  .cm input{width:100%;padding:6px 8px;font-size:12.5px;margin-bottom:4px}
  .cm .list{max-height:300px;overflow:auto}
  .cm .ttl{padding:6px 9px 4px;font-weight:650;color:var(--fg)}
  /* toolbox floating in the canvas */
  .ne .tb{position:absolute;left:10px;top:10px;z-index:3;display:flex;gap:2px;padding:3px;border:1px solid var(--line2);border-radius:10px;
    background:color-mix(in srgb,var(--card) 92%,transparent);backdrop-filter:blur(4px);box-shadow:0 8px 22px -12px rgba(0,0,0,.6)}
  .ne .tb button{background:none;color:var(--fg2);padding:5px 9px;border-radius:7px;font-weight:600;font-size:12px}
  .ne .tb button:hover{background:var(--card2);color:var(--fg);filter:none}.ne .tb button.pri{color:var(--acc)}
  .ne .tb .div{width:1px;background:var(--line);margin:3px 2px}
  /* strategy browser */
  .sbx{position:fixed;inset:0;z-index:55;background:rgba(0,0,0,.45);display:flex;align-items:flex-start;justify-content:center;padding:70px 16px 16px}
  .sbx.hide{display:none}
  .sbw{width:min(860px,100%);max-height:calc(100vh - 90px);display:flex;flex-direction:column;background:var(--card);border:1px solid var(--line2);border-radius:16px;
    box-shadow:0 30px 70px -20px rgba(0,0,0,.75);overflow:hidden}
  .sbw .sh{display:flex;flex-wrap:wrap;gap:8px 10px;align-items:center;padding:14px 16px;border-bottom:1px solid var(--line)}
  .sbw .sh h2{margin:0}.sbw .sh input{flex:1;min-width:160px}.sbw .sh select{width:auto}
  .sbw .st{padding:8px 16px;font-size:12px;color:var(--mute);border-bottom:1px solid var(--line);display:flex;gap:8px;align-items:center}
  .sbw .bd{overflow:auto;padding:6px 8px 10px}
  .sbw .gh{font:650 10px var(--sans);letter-spacing:1px;text-transform:uppercase;color:var(--mute);padding:12px 10px 4px}
  .sr{display:grid;grid-template-columns:minmax(0,1fr) 92px 120px auto;gap:12px;align-items:center;padding:9px 10px;border-radius:10px;cursor:pointer;border:1px solid transparent}
  .sr:hover{background:var(--card2)}.sr.on{border-color:color-mix(in srgb,var(--acc) 45%,var(--line2));background:var(--acc-soft)}
  .sr .nm2{font-weight:650;font-size:13px}.sr .nm2 .tag{font:600 9.5px var(--sans);letter-spacing:.5px;text-transform:uppercase;padding:1px 6px;border-radius:99px;margin-left:6px;background:var(--card2);color:var(--mute);vertical-align:1px}
  .sr .ds{font-size:11.5px;color:var(--mute);line-height:1.35;margin-top:2px;display:-webkit-box;-webkit-line-clamp:2;-webkit-box-orient:vertical;overflow:hidden}
  .sr .apr{font:650 15px var(--mono);text-align:right}.sr .apr small{display:block;font:400 10.5px var(--sans);color:var(--mute)}
  .sr .ex{font-size:11px;color:var(--mute);line-height:1.5}.sr .ex b{font-family:var(--mono);font-weight:500;color:var(--fg2)}
  .sr .ac{display:flex;gap:4px;align-items:center}
  .sr .ac button{padding:3px 8px;font-size:11px}
  .sr .ac button.cmpon{background:var(--acc-soft);color:var(--fg);border-color:var(--acc)}
  .sr .spin{display:inline-block;width:12px;height:12px;border-radius:50%;border:2px solid var(--line2);border-top-color:var(--acc);animation:spin .8s linear infinite}
  @media (max-width:700px){.sr{grid-template-columns:minmax(0,1fr) 80px}.sr .ex,.sr .ac{grid-column:1/-1}}
  .ed-tools{display:flex;gap:6px;align-items:center;flex-wrap:wrap}
  .ne .ghost{position:fixed;z-index:50;pointer-events:none;padding:6px 10px;border-radius:8px;background:var(--card);border:1px solid var(--acc);font-size:12px}
  /* chart */
  .rc{position:relative;user-select:none;touch-action:none}
  .rc svg{width:100%;height:380px;display:block;overflow:visible}
  .rc .yl{font:11px var(--mono);fill:var(--mute)}
  .rc .hd{cursor:ns-resize}.rc .hd rect{transition:filter .12s}.rc .hd:hover rect,.rc .hd.drag rect{filter:brightness(1.25)}
  .rc .hd text{font:600 10.5px var(--sans);pointer-events:none}
  .rc .tipline{font:11px var(--mono);fill:var(--fg2)}
  .ov{display:flex;flex-wrap:wrap;gap:6px;margin-top:8px}
  .ov button{background:var(--card2);color:var(--mute);border:1px solid var(--line2);padding:3px 9px;font-size:11.5px;border-radius:99px;display:inline-flex;align-items:center;gap:6px}
  .ov button i{width:12px;height:3px;border-radius:2px}.ov button.on{color:var(--fg)}
  .lab-status{display:flex;align-items:center;gap:8px;font-size:12px;color:var(--mute);min-height:20px}
  .lab-status .spin{width:12px;height:12px;border-radius:50%;border:2px solid var(--line2);border-top-color:var(--acc);animation:spin .8s linear infinite}
  @keyframes spin{to{transform:rotate(360deg)}}
  /* pool picker: a button showing the pool, a searchable list of yours (with what you hold) and the market's */
  .pp{position:relative}
  .pp-btn{width:100%;display:flex;align-items:center;gap:10px;min-height:44px;padding:6px 12px 6px 9px;border-radius:10px;border:1px solid var(--line2);background:var(--card2);color:var(--fg);cursor:pointer;text-align:left;font:inherit}
  .pp-btn:hover,.pp.open .pp-btn{border-color:var(--acc)}
  .pp-btn .nm{font-weight:650;font-size:14px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .pp-btn .sub,.pp-row .sub{font-size:11px;color:var(--mute);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .pp-btn .car{margin-left:auto;color:var(--mute);transition:transform .15s}.pp.open .pp-btn .car{transform:rotate(180deg)}
  .pp-btn .val,.pp-row .val{margin-left:auto;text-align:right;white-space:nowrap}
  .pp-btn .val b,.pp-row .val b{font:650 13px var(--mono);display:block}
  .pp-pop{position:absolute;z-index:40;top:calc(100% + 6px);left:0;width:min(560px,calc(100vw - 32px));max-height:min(560px,70vh);display:flex;flex-direction:column;
    background:var(--card);border:1px solid var(--line2);border-radius:12px;box-shadow:0 18px 50px rgba(0,0,0,.4);overflow:hidden;animation:ppin .14s ease-out}
  @keyframes ppin{from{opacity:0;transform:translateY(-4px)}}
  .pp-head{padding:10px;border-bottom:1px solid var(--line);display:flex;flex-direction:column;gap:8px}
  .pp-head input{width:100%}
  .pp-chains{display:flex;gap:5px;flex-wrap:wrap}
  .pp-chains button{font-size:11.5px;padding:3px 9px;border-radius:999px;border:1px solid var(--line);background:transparent;color:var(--mute);cursor:pointer}
  .pp-chains button.on{color:var(--fg);border-color:var(--acc);background:color-mix(in srgb,var(--acc) 12%,transparent)}
  .pp-list{overflow:auto;padding:4px 6px 6px}
  .pp-gl{display:flex;justify-content:space-between;font-size:10.5px;text-transform:uppercase;letter-spacing:.07em;color:var(--mute);padding:9px 8px 4px}
  .pp-row{display:flex;align-items:center;gap:10px;padding:7px 8px;border-radius:9px;cursor:pointer;border:1px solid transparent}
  .pp-row:hover,.pp-row.kb{background:var(--card2)}.pp-row.cur{border-color:color-mix(in srgb,var(--acc) 45%,transparent)}
  .pp-row .nm{font-weight:600;font-size:13px;display:flex;align-items:center;gap:6px}
  .pp-row .mid{min-width:0;flex:1}
  .pp-row .fee,.pp-btn .fee{font:500 10.5px var(--mono);color:var(--mute);border:1px solid var(--line);border-radius:5px;padding:0 4px}
  .pp-row.mine{background:linear-gradient(90deg,color-mix(in srgb,var(--acc) 7%,transparent),transparent 60%)}
  .pp-row.mine:hover,.pp-row.mine.kb{background:var(--card2)}
  .pp-dot{width:7px;height:7px;border-radius:50%;flex:none;display:inline-block}.pp-dot.in{background:var(--ok);box-shadow:0 0 0 3px color-mix(in srgb,var(--ok) 20%,transparent)}.pp-dot.out{background:var(--warn)}.pp-dot.off{background:var(--mute)}
  .pp-row .val small{font-size:10.5px;color:var(--mute)}
  .pp-bar{height:3px;border-radius:2px;background:var(--line);margin-top:4px;overflow:hidden;width:72px;margin-left:auto}.pp-bar i{display:block;height:100%;background:var(--acc)}
  .pp-other{border-top:1px solid var(--line);padding:9px 14px;font-size:12.5px;color:var(--acc);cursor:pointer}.pp-other:hover,.pp-other.kb{background:var(--card2)}
  .pp-empty{padding:18px;text-align:center;color:var(--mute);font-size:12.5px}
  .pp .logos .logo{width:22px;height:22px}.pp-btn .logos .logo{width:26px;height:26px}
  /* skeletons while a backtest runs: a soft sweep over placeholder bars */
  .skl{display:inline-block;vertical-align:middle;height:.95em;width:var(--w,60%);border-radius:5px;
    background:linear-gradient(100deg,color-mix(in srgb,var(--line) 80%,transparent) 30%,color-mix(in srgb,var(--fg2) 16%,var(--line)) 50%,color-mix(in srgb,var(--line) 80%,transparent) 70%) 0 0/300% 100%;
    animation:skm 1.4s ease-in-out infinite}
  @keyframes skm{from{background-position:100% 0}to{background-position:0 0}}
  @media (prefers-reduced-motion:reduce){.skl{animation:none}}
  .rc2.ld .big .skl{height:17px;width:92px}.rc2.ld .apr .skl{height:9px;width:58px;margin-top:5px}
  .rc2.ld .g b .skl{height:11px;width:var(--w,70%);margin-top:3px}
  .rc2.ld{animation:skfade .25s ease-out}@keyframes skfade{from{opacity:.4}}
  .sr .apr .skl{height:14px;width:58px}.sr .apr .skl+.skl{display:block;height:8px;width:34px;margin:5px 0 0 auto}
  .sr .ex .skl{display:block;height:9px;width:110px;margin:3px 0}.sr .ex .skl+.skl{width:86px}
  .pick{display:flex;flex-wrap:wrap;gap:6px;align-items:center;margin-bottom:12px}
  .res .pick select{flex:1;min-width:150px}
  .pick .c{display:inline-flex;align-items:center;gap:7px;padding:5px 6px 5px 10px;border-radius:99px;border:1px solid var(--line2);background:var(--card2);font-size:12.5px}
  .pick .c i{width:9px;height:9px;border-radius:3px}.pick .c .x{color:var(--mute);cursor:pointer;padding:0 4px}.pick .c .x:hover{color:var(--bad)}
  .pick select{width:auto;padding:5px 8px;font-size:12.5px}
  .cmp td,.cmp th{padding:8px 10px}.cmp td:first-child{white-space:nowrap}.cmp tr.sum td{border-top:1px solid var(--line2)}
  .cmp .best{color:var(--ok)}
  .cmp th .sw2{display:inline-block;width:9px;height:9px;border-radius:3px;margin-right:6px;vertical-align:middle}
  .eq svg{width:100%;height:150px;display:block}
  .moves{max-height:260px;overflow:auto;font-size:12px}
  .moves div{display:grid;grid-template-columns:130px 70px 1fr;gap:10px;padding:5px 0;border-bottom:1px solid var(--line)}
  .lab-legend{display:flex;flex-wrap:wrap;gap:6px 14px;font-size:12px;color:var(--mute);margin-top:8px}
  .lab-legend span{display:inline-flex;align-items:center;gap:6px}.lab-legend i{width:14px;height:3px;border-radius:2px;display:inline-block}
  .lab-note{font-size:12px;color:var(--mute);margin:8px 0 0}
  .rearm{display:flex;align-items:center;gap:10px;padding:10px 12px;border-radius:10px;border:1px solid color-mix(in srgb,var(--warn) 45%,var(--line2));background:color-mix(in srgb,var(--warn) 8%,var(--card));margin-bottom:12px;font-size:13px}`;
  document.head.appendChild(css);

  const COL = {range: "var(--acc)", buffer: "var(--info)", cutoff: "var(--warn)", exit: "var(--bad)"};
  const RUNCOL = ["var(--acc)", "var(--c1)", "var(--c3)", "var(--c4)", "var(--c6)"];
  const L = {
    chain: null, pool: null, sym: ["", ""], d: [null, null], spacing: 1, pegged: true, num1: false,
    days: 30, capital: 10000, costs: {swap_bps: 0.3, gas_usd: 0.02, fee_haircut: 0.82, emission_haircut: 0.5},
    lib: null, cur: null, dirty: false, compare: ["b:ladder"], hold: true, ov: {},
    res: null, tick: null, job: null, pending: false, step: "", err: "", other: false, sel: null,
  };
  window.LAB = L;

  // ---------- tick math (src/custom.rs) ----------
  const sp = () => Math.max(1, L.spacing || 1);
  const price = t => Math.pow(1.0001, t) * Math.pow(10, (L.d[0] ?? 0) - (L.d[1] ?? 0));
  const pfmt = t => { const p = price(t); return p >= 1000 ? fmt(p, 2) : p >= 1 ? fmt(p, p >= 100 ? 3 : 5) : p.toPrecision(5); };
  const pct = ticks => (Math.pow(1.0001, ticks) - 1) * 100;
  const pctFmt = ticks => (ticks >= 0 ? "+" : "−") + fmt(Math.abs(pct(ticks)), Math.abs(pct(ticks)) < 1 ? 3 : 2) + "%";
  const fdiv = (a, b) => Math.floor(a / b);
  function rangeFor(c, r) {
    const s = sp(), w = Math.ceil(Math.max(r.width, s) / s) * s;
    if (r.placement === "follow") { const tl = fdiv(c - Math.trunc(w / 2), s) * s; return [tl, tl + w]; }
    const below = Math.round(w * (r.skew ?? 0.5));
    let tl = fdiv(c - below, s) * s;
    if (!(tl <= c && c < tl + w)) tl = fdiv(c, s) * s - Math.trunc(w / s / 2) * s;
    return [tl, tl + w];
  }
  const band = (tl, tu, r) => { const lo = tl - (r.buffer_lo || 0), hi = tu + (r.buffer_hi || 0); return lo < hi ? [lo, hi] : [Math.trunc((tl + tu) / 2), Math.trunc((tl + tu) / 2) + 1]; };

  // ---------- node kinds: names, groups and help (ports and params come from the app) ----------
  const GROUPS = [["src", "Sources", "var(--fg2)"], ["ind", "Indicators", "var(--c4)"], ["math", "Maths", "var(--info)"], ["cond", "Conditions", "var(--warn)"],
    ["pos", "Position", "var(--c6)"], ["out", "Outputs", "var(--acc)"]];
  const K = {
    price: ["Price", "src", "The pool price at the end of each 5-minute bar, in ticks (1 tick = 0.01%)."],
    high_low: ["Bar high / low", "src", "The highest and lowest swap price inside each 5-minute bar."],
    fees: ["Fees per bar", "src", "Swap fees paid in each bar (in the numeraire): how busy the pool is."],
    const: ["Number", "src", "A fixed value. Widths and distances can be given in ticks, tick spacings or percent."],
    sma: ["Average (SMA)", "ind", "Simple moving average of its input over the period."],
    ema: ["Average (EMA)", "ind", "Exponential moving average: recent bars weigh more."],
    median: ["Median", "ind", "Median over the period: ignores single spikes."],
    stdev: ["Std deviation", "ind", "How much the input varies over the period (in its units)."],
    bollinger: ["Bollinger bands", "ind", "Average ± k standard deviations of the input over the period."],
    atr: ["ATR", "ind", "Average true range (Wilder): the typical size of a 5-minute bar's move, in ticks."],
    keltner: ["Keltner channel", "ind", "EMA of the price ± k average true ranges."],
    donchian: ["Donchian channel", "ind", "Highest high and lowest low of the period's bars before this one, so a close above the upper line is a breakout."],
    rsi: ["RSI", "ind", "Relative strength index (0–100, Wilder): above 70 the price rose steadily, below 30 it fell. 50 when the price did not move."],
    macd: ["MACD", "ind", "Fast EMA minus slow EMA (in ticks), its signal line (an EMA of it) and the histogram (line minus signal). Above zero: the short-term average is above the long one."],
    stochastic: ["Stochastic", "ind", "Where the close sits in the period's high–low range (0–100), %K, and its average %D. 50 while the range is flat."],
    stoch_rsi: ["Stochastic RSI", "ind", "Where the RSI sits in its own recent range (0–100): a faster, twitchier RSI."],
    cci: ["CCI", "ind", "Commodity channel index: how far the typical price is from its average, in units of its mean deviation (±100 is the usual band). 0 while flat."],
    adx: ["ADX / DI", "ind", "Trend strength (ADX, 0–100: above 25 a clear trend) and direction (+DI above −DI: up). Wilder smoothing; 0 while the price does not move."],
    trix: ["TRIX", "ind", "The change per bar of a triple EMA, in ticks: a smooth rate of change that ignores small wiggles."],
    psar: ["Parabolic SAR", "ind", "A trailing stop that follows the trend and accelerates; it flips to the other side when the price crosses it."],
    ichimoku: ["Ichimoku", "ind", "Conversion and base lines (midpoints of the high and low over 9 and 26 hours) and the cloud (span A and B) as drawn at each bar, computed 'shift' hours earlier. The lagging span is left out: it would look ahead."],
    heikin_ashi: ["Heikin-Ashi", "ind", "Smoothed bars: each close is the bar's average price and each open the midpoint of the previous smoothed bar. Good for spotting runs."],
    gaussian: ["Gaussian channel", "ind", "A centre line from four chained averages (close to a Gaussian filter) ± k deviations of the price around it."],
    trend: ["Trend score", "ind", "One number from −100 (down) to +100 (up): EMA 20 vs 50, MACD, ADX and RSI combined, every period counted in 'unit hours'. Above 10 leans up, below −10 down."],
    momentum: ["Momentum z-score", "ind", "How far the typical price is from its previous short average, compared with the last 'window' hours (a z-score), smoothed. Above 0.25 upward momentum, below −0.25 downward."],
    zscore: ["Z-score", "ind", "How many standard deviations the input is from its average over the period. 0 while flat."],
    percentile: ["Percentile", "ind", "The q-th quantile of the input over the period (q 0.5 is the median, 0.9 the value 90% stay under)."],
    highest: ["Highest", "ind", "The highest value of the input over the period, this bar included."],
    lowest: ["Lowest", "ind", "The lowest value of the input over the period, this bar included."],
    change: ["Change", "math", "The input minus its value the given hours ago. On prices: the move in ticks (1 tick = 0.01%)."],
    lag: ["Earlier value", "math", "The input as it was the given hours ago."],
    add: ["a + b", "math", "Adds two values."], sub: ["a − b", "math", "Subtracts b from a."], mul: ["a × b", "math", "Multiplies two values."],
    scale: ["× k", "math", "Multiplies its input by k."], offset: ["+ value", "math", "Adds a fixed number of ticks (or spacings, or percent)."],
    min: ["Min", "math", "The smaller of a and b."], max: ["Max", "math", "The larger of a and b."], abs: ["Abs", "math", "The size of a value, without its sign."],
    around: ["Range around", "math", "Lower and upper edges around a centre with a width; 'below' is the share of the width under the centre."],
    switch: ["Switch", "math", "a when the condition is true, b otherwise."],
    gt: ["a > b", "cond", "True while a is above b."], lt: ["a < b", "cond", "True while a is below b."],
    cross_above: ["Crosses above", "cond", "True on the bar where a goes from below b to above it."],
    cross_below: ["Crosses below", "cond", "True on the bar where a goes from above b to below it."],
    between: ["Between", "cond", "True while the input is at or above lower and below upper."],
    distance_gt: ["Distance >", "cond", "True while a and b are further apart than the value."],
    and: ["And", "cond", "True when both inputs are."], or: ["Or", "cond", "True when either input is."], not: ["Not", "cond", "True when its input is false."],
    out_of_range: ["Price out of range", "pos", "True while the price is outside the current position's range, widened by the buffers (negative buffers trigger inside)."],
    range_shift: ["Range shifted", "pos", "True when the wanted range's middle is further than the value from the current position's middle."],
    range: ["Range", "out", "The range to hold: its lower and upper edges, rounded out to the tick spacing. Required."],
    rebalance: ["Rebalance when", "out", "Moves to the wanted range when its condition is true (default: the price left the range), after the delay and not within the cooldown."],
    exit: ["Exit when", "out", "Withdraws everything while its condition is true (after the delay). Deposits again when 're-enter' is true (default: when the exit condition is false)."],
    hold: ["Hold when", "out", "While true, nothing is done at all: a freeze, like the depeg guard."],
  };
  const BOOL = new Set(["gt", "lt", "cross_above", "cross_below", "between", "distance_gt", "and", "or", "not", "out_of_range", "range_shift"]);
  const PNAME = {hours: "hours", k: "k", value: "value", below: "below", above: "above", delay_min: "delay (min)", cooldown_min: "cooldown (min)",
    fast: "fast (h)", slow: "slow (h)", signal: "signal (h)", d_hours: "%D (h)", stoch_hours: "stoch (h)", af: "start step", step: "step", max_af: "max step",
    tenkan: "conversion (h)", kijun: "base (h)", span_b: "span B (h)", shift: "shift (h)", unit_hours: "unit (h)", window: "window (h)", smooth: "smooth (h)", q: "q"};
  const kcat = k => (L.lib && L.lib.catalog || []).find(x => x.kind === k);
  const gcol = k => (GROUPS.find(g => g[0] === (K[k] || [])[1]) || [, , "var(--mute)"])[2];

  // ---------- markup ----------
  function shell() {
    const sec = document.createElement("section");
    sec.dataset.tab = "lab";
    sec.className = "grid hide";
    sec.innerHTML = `
      <div class="card" data-help="lab">
        <div class="lab-top">
          <div class="grow" data-help="lab-pool"><label class="f">Pool</label><div class="pp" id="labPool"></div></div>
          <div id="labOther" class="row hide" style="flex:2;min-width:320px;gap:8px"><select id="labChain" style="width:140px"></select>
            <input id="labAddr" placeholder="pool address 0x…" spellcheck="false" style="flex:1"><button class="sec" id="labGo">Load</button></div>
          <div data-help="lab-period"><label class="f">Period</label><div class="seg" id="labDays">${[7, 14, 30, 60, 90, 180, 365].map(d => `<button data-d="${d}">${d}d</button>`).join("")}</div></div>
          <div class="mini" data-help="lab-capital"><label class="f">Capital (<span id="labNum">USD</span>)</label><input id="labCap" type="number" min="1" step="any"></div>
        </div>
      </div>
      <div class="lab-main">
        <div class="grid mid">
          <div class="card" id="labEd"></div>
          <div class="card" data-help="lab-chart"><div class="row" style="margin-bottom:6px"><h2 style="margin:0">Price &amp; strategy</h2><span class="sp"></span>
            <span class="mute small" id="labNow"></span></div>
            <div class="rc" id="labRc"><svg id="labSvg"></svg></div>
            <div class="ov" id="labOv"></div>
            <div class="lab-legend" id="labLegend"></div></div>
          <div class="card" data-help="lab-moves"><h2>Moves &amp; exits</h2><div class="moves" id="labMoves"><span class="mute">Run a backtest to list them.</span></div></div>
        </div>
        <div class="card res" data-help="lab-results">
          <div class="rh"><h2>Backtest</h2></div>
          <div class="lab-status" id="labStatus"></div>
          <div class="mute small" id="labMeta" style="margin:2px 0 10px"></div>
          <div id="labRearm"></div>
          <div id="labCmp"></div>
          <div class="pick" id="labPick" data-help="lab-compare" style="margin-top:4px"></div>
          <div class="eq"><svg id="labEq" viewBox="0 0 800 150" preserveAspectRatio="none"></svg></div>
          <div class="mute small" id="labEqAxis" style="margin-top:6px;line-height:1.5"></div>
          <p class="lab-note" id="labNotes"></p>
        </div>
      </div>`;
    document.querySelector("main").appendChild(sec);
    const nav = document.getElementById("tabs");
    const b = document.createElement("button");
    b.dataset.t = "lab"; b.textContent = "Strategy lab";
    nav.insertBefore(b, nav.querySelector('[data-t="settings"]'));
    b.onclick = () => show("lab");
    poolPickerEvents();
    $("labGo").onclick = () => { const a = $("labAddr").value.trim().toLowerCase(); if (!/^0x[0-9a-f]{40}$/.test(a)) return toast("Not a pool address", true); L.other = false; $("labOther").classList.add("hide"); select(+$("labChain").value, a); };
    $("labDays").onclick = e => { const d = e.target.closest("button"); if (!d) return; L.days = +d.dataset.d; remember(); renderTop(); schedule(0); };
    $("labCap").oninput = e => { const v = parseFloat(e.target.value); if (v > 0) { L.capital = v; schedule(); } };
    const sb = document.createElement("div");
    sb.className = "sbx hide"; sb.id = "labSb";
    sb.innerHTML = `<div class="sbw" data-help="lab-library"><div class="sh"><h2>Strategies</h2><input id="sbQ" placeholder="Search by name or description…" spellcheck="false">
        <div class="seg" id="sbF"><button data-f="all" class="on">All</button><button data-f="builtin">Built in</button><button data-f="mine">Mine</button></div>
        <select id="sbSort"><option value="apr">Best APR first</option><option value="name">By name</option><option value="cat">By group</option></select>
        <button class="sec sm" id="sbClose" title="close (Esc)">✕</button></div>
      <div class="st" id="sbSt"></div><div class="bd" id="sbList"></div></div>`;
    document.body.appendChild(sb);
    sb.onclick = e => {
      if (e.target === sb) return closeBrowser();
      const cmp = e.target.closest("[data-cmp]"), dl = e.target.closest("[data-sbdel]"), row = e.target.closest("[data-lib]");
      if (cmp) { e.stopPropagation(); toggleCompare(cmp.dataset.cmp); return; }
      if (dl) { e.stopPropagation(); delItem(dl.dataset.sbdel); return; }
      if (row) { if (loadItem(row.dataset.lib) !== false) closeBrowser(); }
    };
    $("sbClose").onclick = closeBrowser;
    $("sbQ").oninput = renderLib;
    $("sbSort").onchange = renderLib;
    $("sbF").onclick = e => { const b = e.target.closest("button"); if (!b) return; L.sbf = b.dataset.f; document.querySelectorAll("#sbF button").forEach(x => x.classList.toggle("on", x === b)); renderLib(); };
    document.addEventListener("keydown", e => {
      if (e.key === "Escape" && !menu && !$("labSb").classList.contains("hide")) closeBrowser();
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "o" && tab === "lab") { e.preventDefault(); openBrowser(); }
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s" && tab === "lab" && L.cur) { e.preventDefault(); quickSave(); }
    });
    $("labPick").onclick = e => { const b = e.target.closest("#labAddCmp"); if (b) addCompareMenu(b); };
    // removing a comparison takes its card away at once; the next backtest runs without it
    cardDrag();
    $("labCmp").onclick = e => {
      const x = e.target.closest("[data-unpick]"), h = e.target.closest("[data-unhold]");
      if (x) L.compare = L.compare.filter(id => id !== x.dataset.unpick);
      else if (h) L.hold = false;
      else return;
      remember(); renderPick(); renderResults(); renderLib();
    };
    $("labOv").onclick = e => { const b = e.target.closest("[data-ov]"); if (!b) return; const l = L.res.overlays.lines.find(x => x.name === b.dataset.ov); L.ov[b.dataset.ov] = !ovOn(l); renderOverlayChips(); chartRange(); };
    window.addEventListener("resize", () => { chartRange(); chartEq(); drawWires(); drawMinimap(); });
    tipEvents();
  }
  function remember() {
    try { localStorage.setItem("alm.lab", JSON.stringify({chain: L.chain, pool: L.pool, days: L.days, cur: L.cur && L.cur.id, compare: L.compare, hold: L.hold})); } catch (e) {}
  }

  // ---------- pool ----------
  // every pool the picker offers: yours first (with what you hold in it), then the market's by TVL
  function poolOptions() {
    const out = [], acct = ((S && S.worker && S.worker.account) || {}).pools || {};
    for (const p of (S && S.pools) || []) {
      const cid = pchain(p), k = `${cid}:${p.pool.toLowerCase()}`, m = (MARKET || []).find(x => `${x.chain_id}:${x.pool}` === k), a = acct[p.name] || {};
      const pos = a.positions || [];
      out.push({k, cid, mine: true, name: p.name, sym: m ? [m.token0.symbol, m.token1.symbol] : (a.symbols || p.symbols || [p.name]), t: m ? [m.token0, m.token1] : null,
        fee: m ? m.fee_pips : a.fee_pips, deployed: a.deployed_usd, ranges: pos.length, inRange: pos.filter(x => x.in_range).length, alloc: a.allocation_pct ?? p.allocation_pct,
        enabled: p.enabled !== false, strat: p.strategy && p.strategy.type, tvl: m && m.tvl_usd, apr: m && m.processor_apr_24h});
    }
    for (const m of MARKET || []) {
      const k = `${m.chain_id}:${m.pool}`;
      if (!out.some(o => o.k === k) && m.tvl_usd >= 1000) out.push({k, cid: m.chain_id, sym: [m.token0.symbol, m.token1.symbol], t: [m.token0, m.token1], fee: m.fee_pips,
        tvl: m.tvl_usd, vol: m.volume_24h, apr: m.processor_apr_24h, pegged: m.pegged});
    }
    const mine = out.filter(o => o.mine), rest = out.filter(o => !o.mine).sort((a, b) => (b.tvl || 0) - (a.tvl || 0));
    return mine.concat(rest);
  }
  const feeTxt = f => f == null ? "" : `${fmt(f / 100, f % 100 ? 2 : 0)} bp`;
  const pairLogos = o => `<span class="logos">${o.t ? logo(o.t[0]) + logo(o.t[1]) : logo({symbol: o.sym[0]}) + logo({symbol: o.sym[1] || "?"})}</span>`;
  const big$ = x => x == null ? "–" : x >= 1e6 ? "$" + fmt(x / 1e6, 2) + "M" : x >= 1e4 ? "$" + fmt(x / 1e3, 1) + "k" : usd(x, x < 100 ? 2 : 0);
  function poolRow(o, cur) {
    const st = !o.mine ? "" : !o.enabled ? "off" : !o.ranges ? "off" : o.inRange ? "in" : "out";
    const stTxt = !o.mine ? "" : !o.enabled ? "paused" : !o.ranges ? "no position" : o.inRange ? `in range · ${o.ranges} range${o.ranges === 1 ? "" : "s"}` : `out of range · ${o.ranges} range${o.ranges === 1 ? "" : "s"}`;
    return `<div class="pp-row ${o.mine ? "mine" : ""} ${o.k === cur ? "cur" : ""}" data-pk="${esc(o.k)}" data-q="${esc((o.sym.join(" ") + " " + (o.name || "") + " " + chainOf(o.cid).name).toLowerCase())}" role="option">
      ${pairLogos(o)}
      <div class="mid"><div class="nm">${esc(o.sym.join(" / "))}${o.fee != null ? `<span class="fee">${feeTxt(o.fee)}</span>` : ""}</div>
        <div class="sub">${esc(chainOf(o.cid).name)}${o.mine ? ` · ${st ? `<span class="pp-dot ${st}"></span> ` : ""}${esc(stTxt)}${o.strat ? " · " + esc(o.strat) : ""}` : ` · TVL ${big$(o.tvl)} · 24h vol ${big$(o.vol)}`}</div></div>
      ${o.mine ? `<div class="val" title="your liquidity in this pool"><b>${o.deployed != null ? usd(o.deployed, 0) : "–"}</b>${o.alloc != null ? `<small>${fmt(o.alloc, 0)}% of the account</small><div class="pp-bar"><i style="width:${Math.min(100, o.alloc)}%"></i></div>` : ""}</div>`
        : `<div class="val"><b>${o.apr != null ? fmt(o.apr, o.apr < 10 ? 1 : 0) + "%" : "–"}</b><small>APR 24h</small></div>`}
    </div>`;
  }
  function renderTop() {
    const opts = poolOptions(), cur = L.pool ? `${L.chain}:${L.pool}` : "", box = $("labPool");
    let o = opts.find(x => x.k === cur);
    if (!o && cur) o = {k: cur, cid: L.chain, sym: L.sym.length && L.sym[0] ? L.sym : [cur.slice(cur.indexOf(":") + 1, cur.indexOf(":") + 9) + "…"]};
    const btn = !o ? `<span class="nm" style="color:var(--mute)">${L.other ? "Another pool by address" : "Choose a pool…"}</span>`
      : `${pairLogos(o)}<span style="min-width:0"><span class="nm">${esc(o.sym.join(" / "))}</span>${o.fee != null ? ` <span class="fee">${feeTxt(o.fee)}</span>` : ""}
          <div class="sub">${esc(chainOf(o.cid).name)}${o.mine ? ` · yours${o.ranges ? ` · <span class="pp-dot ${o.inRange ? "in" : "out"}"></span> ${o.inRange ? "in range" : "out of range"}` : ""}` : o.tvl != null ? ` · TVL ${big$(o.tvl)}` : ""}</div></span>
         ${o.mine && o.deployed != null ? `<span class="val"><b>${usd(o.deployed, 0)}</b><span class="sub">your liquidity</span></span>` : ""}`;
    const open = box.classList.contains("open");
    if (!open) box.innerHTML = `<button class="pp-btn" type="button" aria-haspopup="listbox">${btn}<span class="car" ${o && o.mine && o.deployed != null ? 'style="margin-left:6px"' : ""}>▾</span></button>`;
    if (!$("labChain").matches(":focus")) $("labChain").innerHTML = chains().map(c => `<option value="${c.id}" ${c.id === (L.chain || 8453) ? "selected" : ""}>${esc(c.name)}</option>`).join("");
    document.querySelectorAll("#labDays button").forEach(b => b.classList.toggle("on", +b.dataset.d === L.days));
    if (document.activeElement !== $("labCap")) $("labCap").value = L.capital;
    $("labNum").textContent = "USD";
  }
  function poolPickerEvents() {
    const box = $("labPool");
    let chainF = 0, kb = -1;
    const close = () => { box.classList.remove("open"); const p = box.querySelector(".pp-pop"); if (p) p.remove(); renderTop(); };
    const rows = () => [...box.querySelectorAll(".pp-row:not([hidden]), .pp-other")];
    const mark = i => { const r = rows(); r.forEach(x => x.classList.remove("kb")); kb = Math.max(0, Math.min(r.length - 1, i)); if (r[kb]) { r[kb].classList.add("kb"); r[kb].scrollIntoView({block: "nearest"}); } };
    function list() {
      const opts = poolOptions(), cur = L.pool ? `${L.chain}:${L.pool}` : "", q = (box.querySelector(".pp-head input") || {}).value || "";
      const ok = o => (!chainF || o.cid === chainF) && (!q || o.sym.join(" ").concat(" ", o.name || "", " ", chainOf(o.cid).name).toLowerCase().includes(q.trim().toLowerCase()));
      const mine = opts.filter(o => o.mine && ok(o)), rest = opts.filter(o => !o.mine && ok(o));
      const tot = mine.reduce((a, o) => a + (o.deployed || 0), 0);
      box.querySelector(".pp-list").innerHTML =
        (mine.length ? `<div class="pp-gl"><span>Your pools</span><span>${usd(tot, 0)} deployed</span></div>` + mine.map(o => poolRow(o, cur)).join("") : "") +
        (rest.length ? `<div class="pp-gl"><span>All pools</span><span>by TVL</span></div>` + rest.map(o => poolRow(o, cur)).join("") : "") +
        (!mine.length && !rest.length ? `<div class="pp-empty">${MARKET ? "No pool matches." : "Loading pools…"}</div>` : "");
      kb = -1;
      if (q) mark(0);
    }
    function open() {
      box.classList.add("open");
      const ids = [...new Set(poolOptions().map(o => o.cid))];
      box.insertAdjacentHTML("beforeend", `<div class="pp-pop" role="listbox"><div class="pp-head"><input placeholder="Search by token, pool or chain…" spellcheck="false">
        <div class="pp-chains"><button data-cf="0" class="${chainF ? "" : "on"}">All chains</button>${ids.map(id => `<button data-cf="${id}" class="${chainF === id ? "on" : ""}">${esc(chainOf(id).name)}</button>`).join("")}</div></div>
        <div class="pp-list"></div><div class="pp-other">＋ Another pool by address…</div></div>`);
      list();
      box.querySelector(".pp-head input").focus();
      const c = box.querySelector(".pp-row.cur"); if (c) c.scrollIntoView({block: "center"});
    }
    const choose = el => {
      if (el.classList.contains("pp-other")) { close(); L.other = true; $("labOther").classList.remove("hide"); $("labAddr").focus(); renderTop(); return; }
      close(); pickKey(el.dataset.pk);
    };
    box.addEventListener("click", e => {
      if (e.target.closest(".pp-btn")) { box.classList.contains("open") ? close() : open(); return; }
      const cf = e.target.closest("[data-cf]");
      if (cf) { chainF = +cf.dataset.cf; box.querySelectorAll("[data-cf]").forEach(b => b.classList.toggle("on", b === cf)); list(); box.querySelector(".pp-head input").focus(); return; }
      const r = e.target.closest(".pp-row, .pp-other");
      if (r) choose(r);
    });
    box.addEventListener("input", e => { if (e.target.matches(".pp-head input")) list(); });
    box.addEventListener("keydown", e => {
      if (!box.classList.contains("open")) { if (["ArrowDown", "Enter", " "].includes(e.key) && e.target.closest(".pp-btn")) { e.preventDefault(); open(); } return; }
      if (e.key === "Escape") { e.preventDefault(); close(); box.querySelector(".pp-btn").focus(); }
      else if (e.key === "ArrowDown") { e.preventDefault(); mark(kb + 1); }
      else if (e.key === "ArrowUp") { e.preventDefault(); mark(kb - 1); }
      else if (e.key === "Enter") { e.preventDefault(); const r = rows()[kb]; if (r) choose(r); }
    });
    document.addEventListener("pointerdown", e => { if (box.classList.contains("open") && !box.contains(e.target)) close(); });
  }
  function pickKey(k) { const [c, p] = k.split(":"); L.other = false; $("labOther").classList.add("hide"); select(+c, p); }
  const cfgOf = () => ((S && S.pools) || []).find(p => pchain(p) === L.chain && p.pool.toLowerCase() === L.pool);
  // The capital a backtest runs with: what you would really have in the pool. Fees do not depend
  // on it (they are the pool's fee per unit of liquidity), but emissions dilute and gas is a fixed
  // cost per move, so a smaller or larger amount than you would deploy shows a different APR. Your
  // pool: what is deployed in it now; another pool: an equal share of the account (what adding it
  // as one more pool would give it). In USD: you bring USDC and the backtest buys the pair with it.
  function defaultCapital(cfg, m) {
    const acct = (S && S.worker && S.worker.account) || {}, mine = acct.pools || {};
    const dep = cfg && mine[cfg.name] && mine[cfg.name].deployed_usd;
    const x = dep > 0 ? dep : acct.total_usd > 0 ? acct.total_usd / (Object.keys(mine).length + 1) : 10000;
    // two significant figures: a round number that reads as an amount, not a measurement
    const k = Math.pow(10, Math.floor(Math.log10(x)) - 1);
    return Math.round(x / k) * k;
  }

  function select(cid, pool) {
    L.chain = cid; L.pool = pool.toLowerCase(); L.res = null; L.tick = null;
    const cfg = cfgOf(), m = (MARKET || []).find(x => x.chain_id === cid && x.pool === L.pool);
    L.sym = m ? [m.token0.symbol, m.token1.symbol] : cfg ? (cfg.symbols || ["", ""]) : ["", ""];
    L.d = m ? [m.token0.decimals, m.token1.decimals] : cfg ? [cfg.decimals, cfg.decimals1] : [null, null];
    L.spacing = m ? m.tick_spacing : cfg ? cfg.tick_spacing : 1;
    L.pegged = m ? m.pegged : cfg ? cfg.pegged !== false : true;
    // the numeraire as the backtest picks it (a pool's setting, else WETH against a non-stable)
    L.num1 = cfg ? cfg.numeraire === 1 : (["WETH", "ETH"].includes((L.sym[1] || "").toUpperCase()) && !["USDC", "USDT"].includes((L.sym[0] || "").toUpperCase()));
    L.capital = defaultCapital(cfg, m);
    if (cfg) {
      // the pool's own strategy opens first
      if (!L.cur || L.cur.fromPool !== L.pool) setCur({id: null, fromPool: L.pool, name: `${cfg.name}: current strategy`, description: "What this pool runs now.",
        strategy: JSON.parse(JSON.stringify(cfg.strategy))});
    }
    if (!L.cur) loadItem("b:bollinger", true);
    remember(); renderTop(); renderLib(); renderEd(); chartRange(); renderResults(); loadCustomState(); schedule(0);
  }
  window.labOpen = (cid, pool) => { show("lab"); L.cur = null; select(cid, pool); };

  // ---------- library ----------
  async function loadLib() {
    try { L.lib = await api("/api/strategies"); } catch (e) { L.lib = {builtin: [], mine: [], catalog: []}; toast("strategy library: " + e.message, true); }
    renderLib(); renderPick();
    // the Pools view's strategy pickers list the library
    if (typeof renderPools === "function" && typeof poolsEdit !== "undefined" && poolsEdit) renderPools();
    // the help panel explains each block from its description and the app's catalog
    if (window.HELP) for (const c of L.lib.catalog) if (K[c.kind]) window.HELP["node-" + c.kind] = [K[c.kind][0] + " block", `<p>${esc(K[c.kind][2])}</p>` +
      (c.inputs.length ? `<p><b>Inputs</b>: ${c.inputs.map(p => esc(p) + (c.optional.includes(p) ? " (optional)" : "")).join(", ")}</p>` : "") +
      (c.outputs.length ? `<p><b>Outputs</b>: ${c.outputs.map(esc).join(", ")}${BOOL.has(c.kind) ? " (true/false)" : c.price ? " (a price, in ticks)" : ""}</p>` : "") +
      (c.params.length ? `<p><b>Settings</b>: ${c.params.map(p => `${esc(PNAME[p.name] || p.name)} (default ${p.default}, ${p.min} to ${p.max}${p.ticks ? ", in the block's unit" : ""})`).join("; ")}</p>` : "")];
  }
  const allItems = () => L.lib ? [...L.lib.builtin, ...L.lib.mine] : [];
  const itemById = id => allItems().find(x => x.id === id);
  // ---------- strategy browser (Open…): every strategy with its APR over the chosen period ----------
  const CATS = ["Mine", "Indicators", "Simple rules", "Ladder"];
  function openBrowser() {
    if (!L.lib) return;
    $("labSb").classList.remove("hide");
    $("sbQ").value = ""; renderLib();
    setTimeout(() => $("sbQ").focus(), 0);
    runApr();
  }
  function closeBrowser() { $("labSb").classList.add("hide"); }
  const aprKey = () => JSON.stringify([L.chain, L.pool, L.days, L.capital, L.costs, allItems().map(x => [x.id, x.strategy])]);
  // backtest every library strategy on the current pool and period (the app runs 4 at a time)
  async function runApr() {
    if (!L.pool || !L.lib) return renderLib();
    const key = aprKey();
    if (L.apr && L.apr.key === key && (L.apr.done || L.apr.job)) return renderLib();
    L.apr = {key, rows: {}, job: null, done: false, err: "", step: "starting"};
    renderLib();
    const items = allItems().filter(x => !x.draft).map(x => ({id: x.id, strategy: body(x.strategy)}));
    let id;
    while (true) {
      try { id = (await api("/api/backtest", {chain_id: L.chain, pool: L.pool, days: L.days, capital_usd: L.capital, costs: L.costs, strategy: {type: "ladder"}, batch: items})).id; break; }
      catch (e) {
        if (!/already running/.test(e.message)) { L.apr.err = e.message; return renderLib(); }
        await new Promise(r => setTimeout(r, 1200));
        if (!L.apr || L.apr.key !== key) return;
      }
    }
    L.apr.job = id;
    while (L.apr && L.apr.key === key) {
      await new Promise(r => setTimeout(r, 400));
      let j;
      try { j = await api("/api/backtest/" + id); } catch (e) { L.apr.err = e.message; break; }
      if (j.state === "running") { L.apr.step = j.step; for (const r of j.partial || []) L.apr.rows[r.id] = r; renderLib(); continue; }
      if (j.state === "error") { L.apr.err = j.error; break; }
      for (const r of j.result.batch) L.apr.rows[r.id] = r;
      L.apr.done = true; L.apr.meta = j.result;
      break;
    }
    if (L.apr && L.apr.key === key) L.apr.job = null;
    renderLib(); renderPick();
  }
  const aprOf = id => L.apr && L.apr.key === aprKey() ? L.apr.rows[id] : null;
  const aprTxt = a => a == null ? "" : (a >= 0 ? "+" : "−") + fmt(Math.abs(a), Math.abs(a) < 10 ? 2 : 1) + "%";
  function renderLib() {
    const box = $("sbList");
    if (!box || $("labSb").classList.contains("hide")) return;
    if (!L.lib) { box.innerHTML = `<div class="empty">Loading the library…</div>`; return; }
    const q = $("sbQ").value.trim().toLowerCase(), f = L.sbf || "all", sort = $("sbSort").value;
    let xs = allItems().filter(x => (f === "all" || (f === "mine") === x.id.startsWith("u:")) && (!q || (x.name + " " + (x.description || "")).toLowerCase().includes(q)));
    const apr = x => { const r = aprOf(x.id); return r && r.apr != null ? r.apr : -1e9; };
    if (sort === "apr") xs.sort((a, b) => apr(b) - apr(a));
    if (sort === "name") xs.sort((a, b) => a.name.localeCompare(b.name));
    const row = x => {
      const r = aprOf(x.id), mine = x.id.startsWith("u:"), inCmp = L.compare.includes(x.id);
      return `<div class="sr ${L.cur && L.cur.id === x.id ? "on" : ""}" data-lib="${esc(x.id)}" title="open this strategy in the editor">
        <div><div class="nm2">${esc(x.name)}<span class="tag">${mine ? "mine" : x.category}</span></div><div class="ds">${esc(x.description || typeLabel(x.strategy))}</div></div>
        <div class="apr">${x.draft ? `<span class="small" style="color:var(--warn)" title="${esc(x.draft)}">unfinished</span>` : r ? r.error ? `<span class="bad small" title="${esc(r.error)}">error</span>` : `<span class="${r.apr >= 0 ? "pos" : "neg"}">${aprTxt(r.apr)}</span><small>a year</small>`
          : L.pool ? `<span class="skl"></span><span class="skl"></span>` : ""}</div>
        <div class="ex">${r && !r.error ? `vs hold <b>${(r.vs_hodl >= 0 ? "+" : "−") + fmt(Math.abs(r.vs_hodl), 2)}</b><br>moves <b>${r.actions}</b> · in range <b>${fmt(r.time_in_range * 100, 0)}%</b>` : !r && !x.draft && L.pool ? `<span class="skl"></span><span class="skl"></span>` : ""}</div>
        <div class="ac"><button class="sec ${inCmp ? "cmpon" : ""}" data-cmp="${esc(x.id)}" title="${inCmp ? "remove from" : "add to"} the backtest comparison">${inCmp ? "✓ compared" : "compare"}</button>
          ${mine ? `<button class="sec" data-sbdel="${esc(x.id)}" title="delete from your strategies">✕</button>` : ""}</div></div>`;
    };
    box.innerHTML = !xs.length ? `<div class="empty">No strategy matches.</div>` : sort === "cat"
      ? CATS.map(c => { const g = xs.filter(x => (x.id.startsWith("u:") ? "Mine" : x.category) === c); return g.length ? `<div class="gh">${c === "Mine" ? "Saved by you" : c}</div>` + g.map(row).join("") : ""; }).join("")
      : xs.map(row).join("");
    const a = L.apr, sym = L.res ? L.res.numeraire_symbol : "";
    $("sbSt").innerHTML = !L.pool ? "Choose a pool to see what each strategy would have earned."
      : a && a.err ? `<span class="bad">${esc(a.err)}</span>`
      : `${a && !a.done ? `<span class="spin" style="width:11px;height:11px;border-radius:50%;border:2px solid var(--line2);border-top-color:var(--acc);display:inline-block;animation:spin .8s linear infinite"></span> ${esc(a.step || "")} · ` : ""}
        APR from a backtest of the last ${L.days} days on ${esc(L.sym.join(" / "))} with $${fmt(L.capital, 0)}, after costs, 4 strategies at a time.${a && a.done && a.meta ? ` Took ${fmt(a.meta.elapsed_ms / 1000, 1)} s.` : ""}`;
  }
  function toggleCompare(id) {
    if (L.compare.includes(id)) L.compare = L.compare.filter(x => x !== id);
    else if (L.compare.length >= 4) return toast("At most four comparisons: remove one first.", true);
    else L.compare.push(id);
    remember(); renderLib(); renderPick(); compareChanged();
  }
  async function delItem(id) {
    const x = itemById(id);
    if (!x || !confirm(`Delete "${x.name}" from your strategies?`)) return;
    try {
      await api("/api/strategies", {action: "delete", id});
      L.compare = L.compare.filter(c => c !== id);
      if (L.cur && L.cur.id === id) L.cur.id = null;
      await loadLib(); renderEd(); toast("Deleted.");
    } catch (e) { toast(e.message, true); }
  }
  const typeLabel = s => s.type === "graph" ? "node strategy" : s.type === "custom" ? "simple rules" : "ladder";
  // a built-in simple-rules strategy in spacings becomes ticks of this pool, so the form shows ticks
  function forPool(s) {
    const x = JSON.parse(JSON.stringify(s));
    if (x.type === "custom" && x.unit === "spacings") {
      for (const k of ["width", "offset_up", "offset_down", "buffer_lo", "buffer_hi"]) if (x[k] != null) x[k] = Math.round(x[k] * sp());
      delete x.unit;
    }
    if (x.type === "custom") x.width = Math.max(x.width ?? sp(), sp());
    return x;
  }
  function setCur(item) {
    L.cur = {id: item.id, fromPool: item.fromPool || null, name: item.name, description: item.description || "", builtin: !!(item.id && item.id.startsWith("b:")),
      strategy: forPool(item.strategy)};
    L.dirty = false; L.sel = null;
  }
  function loadItem(id, quiet) {
    const x = itemById(id);
    if (!x) return;
    if (L.dirty && !quiet && !confirm("Discard the changes to the strategy you are editing?")) return false;
    setCur(x); remember(); renderLib(); renderEd(); chartRange(); schedule(0);
  }
  function newStrategy(kind) {
    if (L.dirty && !confirm("Discard the changes to the strategy you are editing?")) return;
    if (kind === "rules") setCur({id: null, name: "New rules", description: "", strategy: {type: "custom", width: 6 * sp(), placement: "center", skew: 0.5, buffer_lo: 0, buffer_hi: 0, delay_min: 0, cooldown_min: 60}});
    else if (kind === "ladder") setCur({id: null, name: "My ladder", description: "", strategy: {type: "ladder"}});
    else newGraph();
    L.dirty = true;
    renderLib(); renderEd(); chartRange(); schedule(0);
  }
  function newGraph() {
    setCur({id: null, name: "New strategy", description: "", strategy: {type: "graph", nodes: [
      {id: "p", kind: "price", x: 20, y: 40, p: {}, in: {}},
      {id: "w", kind: "const", unit: "spacings", x: 20, y: 170, p: {value: 6}, in: {}},
      {id: "ar", kind: "around", x: 260, y: 60, p: {below: 0.5}, in: {center: ["p", "tick"], width: ["w", "value"]}},
      {id: "r", kind: "range", x: 500, y: 60, p: {}, in: {lower: ["ar", "lower"], upper: ["ar", "upper"]}},
      {id: "move", kind: "rebalance", x: 500, y: 220, p: {delay_min: 0, cooldown_min: 60}, in: {}}]}});
  }
  // Ctrl+S / Save: overwrite your own strategy, or ask for a name the first time
  function quickSave() { if (L.cur.id && L.cur.id.startsWith("u:")) save(false); else saveAs($("labSaveBtn")); }
  function saveAs(btn) {
    const r = btn.getBoundingClientRect(), taken = new Set(allItems().map(x => x.name.toLowerCase()));
    let nm = L.cur.name;
    if (taken.has(nm.toLowerCase())) { let i = 2; while (taken.has(`${L.cur.name} ${i}`.toLowerCase())) i++; nm = `${L.cur.name} ${i}`; }
    openMenu(r.left, r.bottom + 6, `<div class="ttl">Save as a new strategy</div><input data-rename id="saName" value="${esc(nm)}" maxlength="60" placeholder="name" spellcheck="false">
      <input data-rename id="saDesc" value="${esc(L.cur.description || "")}" maxlength="400" placeholder="description (optional)" spellcheck="false">
      <div class="row" style="padding:4px 2px 2px"><span class="sp"></span><button class="sm" id="saGo">Save</button></div>`, m => {
      const go = () => { const n = $("saName").value.trim(); if (!n) return toast("Give the strategy a name", true); L.cur.name = n; L.cur.description = $("saDesc").value.trim(); closeMenu(); save(true); };
      $("saName").focus(); $("saName").select();
      m.querySelectorAll("input").forEach(i => i.onkeydown = e => { if (e.key === "Enter") { e.preventDefault(); go(); } });
      $("saGo").onclick = go;
    });
  }
  async function save(asNew) {
    const name = ($("labName") && $("labName").value.trim()) || L.cur.name;
    if (!name) return toast("Give the strategy a name", true);
    const id = !asNew && L.cur.id && L.cur.id.startsWith("u:") ? L.cur.id : undefined;
    try {
      const it = await api("/api/strategies", {action: "save", id, name, description: L.cur.description, strategy: L.cur.strategy});
      const v = L.cur.view;
      await loadLib(); setCur(it); L.cur.view = v; renderLib(); renderEd(); remember();
      toast(it.draft ? `Saved "${it.name}" as unfinished: ${it.draft}. Finish it before applying it.` : `Saved "${it.name}" in your strategies.`, !!it.draft);
    } catch (e) { toast(e.message, true); }
  }
  async function del() {
    if (!L.cur.id || !L.cur.id.startsWith("u:") || !confirm(`Delete "${L.cur.name}" from your strategies?`)) return;
    try { await api("/api/strategies", {action: "delete", id: L.cur.id}); L.cur.id = null; await loadLib(); renderEd(); toast("Deleted."); }
    catch (e) { toast(e.message, true); }
  }
  function changed() { L.dirty = true; const d = $("labDirty"); if (d) d.textContent = "unsaved changes"; schedule(); }

  // ---------- editor ----------
  function renderEd() {
    const box = $("labEd");
    if (!L.cur) { box.innerHTML = `<div class="empty">Choose a pool and a strategy.</div>`; return; }
    const s = L.cur.strategy, mine = L.cur.id && L.cur.id.startsWith("u:");
    box.innerHTML = `<div class="ed-head" data-help="lab-save"><div class="ed-tools"><button class="sec sm" id="labOpen" title="browse every strategy with its APR (Ctrl+O)">📂 Select strategy</button>
        <button class="sec sm" id="labNewBtn" title="start a new strategy">＋ Create new strategy</button></div>
        <input class="nm" id="labName" value="${esc(L.cur.name)}" aria-label="strategy name" title="rename">
        <span class="chip">${typeLabel(s)}</span><span class="dirty" id="labDirty">${L.dirty ? "unsaved changes" : ""}</span><span class="sp"></span>
        <div class="ed-tools">${mine ? `<button class="sec sm" id="labDel">Delete</button>` : ""}
        <button class="sec sm" id="labSaveNew">Save as…</button><button class="sm" id="labSaveBtn" title="Ctrl+S">Save</button></div></div>
      <input class="ed-desc" id="labDesc" value="${esc(L.cur.description || "")}" placeholder="a short description (optional)" maxlength="400" spellcheck="false" title="describe the strategy" style="width:100%;margin:-4px 0 ${L.cur.builtin ? 4 : 12}px;font-size:12.5px">
      ${L.cur.builtin && !mine ? `<p class="mute small" style="margin:0 0 10px"><i>Built in: saving keeps a copy under your strategies.</i></p>` : ""}
      <div id="labEdBody"></div>`;
    $("labName").oninput = e => { L.cur.name = e.target.value; };
    if ($("labDesc")) $("labDesc").oninput = e => { L.cur.description = e.target.value; L.dirty = true; $("labDirty").textContent = "unsaved changes"; };
    $("labSaveNew").onclick = e => saveAs(e.currentTarget);
    $("labSaveBtn").onclick = quickSave;
    $("labOpen").onclick = openBrowser;
    $("labNewBtn").onclick = e => {
      const r = e.currentTarget.getBoundingClientRect();
      openMenu(r.left, r.bottom + 6, `<div class="ttl">New strategy</div>${item("graph", "Node strategy", "<kbd>blocks</kbd>")}${item("rules", "Simple rules", "<kbd>one range</kbd>")}${item("ladder", "Ladder", "<kbd>the app's own</kbd>")}`,
        m => { m.onclick = ev => { const a = ev.target.closest("[data-act]"); if (a) { closeMenu(); newStrategy(a.dataset.act); } }; });
    };
    if ($("labDel")) $("labDel").onclick = del;
    const body = $("labEdBody");
    if (s.type === "graph") renderGraph(body);
    else if (s.type === "custom") renderRules(body);
    else renderLadder(body);
  }

  // simple rules form
  function num(r, k, label, opts = {}) {
    const v = r[k];
    return `<div class="${opts.full ? "full" : ""}" data-help="lab-${opts.help || k}"><label class="f"><span>${label}</span><i data-hint="${k}">${opts.hint ? opts.hint(v) : ""}</i></label>
      <input type="number" data-r="${k}" value="${v ?? ""}" step="${opts.step ?? sp()}" ${opts.min != null ? `min="${opts.min}"` : ""} placeholder="${opts.ph || ""}"></div>`;
  }
  const tickHint = v => v == null || v === "" ? "" : pctFmt(+v);
  const absHint = v => v == null || v === "" || L.tick == null ? "" : `${pfmt(+v)} · ${pctFmt(+v - L.tick)}`;
  function grp(id, title, color, toggle, body) {
    const on = toggle ? toggle.on : true;
    return `<div class="grp ${on ? "" : "off"}" data-help="lab-${id}"><div class="gh"><span class="key" style="background:${color}"></span><b>${title}</b>
      ${toggle ? `<label class="sw"><input type="checkbox" data-tog="${id}" ${on ? "checked" : ""}><span class="t"></span></label>` : ""}</div><div class="lf">${body}</div></div>`;
  }
  function renderRules(box) {
    const r = L.cur.strategy;
    box.innerHTML = `<p class="mute small" style="margin-top:0">One range, moved by rules. Drag the handles on the chart below, or set the numbers here.</p>` +
      grp("range", "Range", COL.range, null,
        num(r, "width", "Width (ticks)", {min: sp(), hint: v => "≈ " + fmt(pct(+v || 0), 3) + "%"}) +
        `<div data-help="lab-placement"><label class="f">Placement</label><select data-r="placement"><option value="center" ${r.placement !== "follow" ? "selected" : ""}>Centred on the price</option>
          <option value="follow" ${r.placement === "follow" ? "selected" : ""}>Follow from the crossed edge</option></select></div>` +
        (r.placement === "follow"
          ? num(r, "offset_up", "After a move up: ticks above", {min: 0, hint: tickHint, help: "offset"}) + num(r, "offset_down", "After a move down: ticks below", {min: 0, hint: tickHint, help: "offset"})
          : `<div class="full" data-help="lab-skew"><label class="f"><span>Share of the range below the price</span><i>${fmt((r.skew ?? 0.5) * 100, 0)}%</i></label>
              <input type="range" data-r="skew" min="0" max="1" step="0.05" value="${r.skew ?? 0.5}"></div>`)) +
      grp("trigger", "Trigger", COL.buffer, null,
        num(r, "buffer_lo", "Buffer below (ticks)", {hint: tickHint, help: "buffer"}) + num(r, "buffer_hi", "Buffer above (ticks)", {hint: tickHint, help: "buffer"}) +
        num(r, "delay_min", "Delay (min)", {step: 1, min: 0, help: "delay"}) + num(r, "cooldown_min", "Cooldown (min)", {step: 5, min: 0, help: "cooldown"})) +
      grp("cutoff", "Cutoffs", COL.cutoff, {on: r.cutoff_lo != null || r.cutoff_hi != null},
        num(r, "cutoff_lo", "Don't rebalance below (tick)", {hint: absHint, ph: "none", help: "cutoff"}) + num(r, "cutoff_hi", "Don't rebalance above (tick)", {hint: absHint, ph: "none", help: "cutoff"})) +
      grp("exit", "Exit", COL.exit, {on: r.exit_lo != null || r.exit_hi != null},
        num(r, "exit_lo", "Withdraw below (tick)", {hint: absHint, ph: "none", help: "exitp"}) + num(r, "exit_hi", "Withdraw above (tick)", {hint: absHint, ph: "none", help: "exitp"}) +
        num(r, "exit_delay_min", "Exit delay (min)", {step: 1, min: 0, help: "exitdelay"}) + exitToSel(r.exit_to) +
        `<div class="full" data-help="lab-reenter"><label class="sw"><input type="checkbox" data-r="reenter" ${r.reenter ? "checked" : ""}><span class="t"></span>Re-enter when the price is back between the exits</label></div>`) +
      grp("guard", "Depeg guard", "var(--c4)", {on: r.guard != null},
        num(r, "guard", "Freeze when this many ticks from the 7-day median", {step: 1, min: 1, ph: "off", full: true, help: "guard"})) + costsBox();
    box.oninput = box.onchange = onRules;
  }
  function exitToSel(v) {
    return `<div data-help="lab-exitto"><label class="f">Withdraw into</label><select data-r="exit_to">${[["keep", "both tokens, as withdrawn"], ["token0", "all " + esc(L.sym[0] || "token0")], ["token1", "all " + esc(L.sym[1] || "token1")]]
      .map(([x, l]) => `<option value="${x}" ${(v || "keep") === x ? "selected" : ""}>${l}</option>`).join("")}</select></div>`;
  }
  function costsBox() {
    return `<details class="grp" data-help="lab-costs"><summary class="mute small">Costs &amp; haircuts: swap ${fmt(L.costs.swap_bps, 2)} bp · gas $${fmt(L.costs.gas_usd, 3)} · fees ×${fmt(L.costs.fee_haircut, 2)} · emissions ×${fmt(L.costs.emission_haircut, 2)}</summary>
      <div class="lf" style="margin-top:10px">${[["swap_bps", "Swap cost (bp)"], ["gas_usd", "Gas per tx ($)"], ["fee_haircut", "Fees received (×)"], ["emission_haircut", "Emissions value (×)"]]
        .map(([k, l]) => `<div><label class="f">${l}</label><input type="number" step="any" min="0" data-c="${k}" value="${L.costs[k]}"></div>`).join("")}</div></details>`;
  }
  function onCost(el) { const v = parseFloat(el.value); if (v >= 0) { L.costs[el.dataset.c] = v; schedule(); } }
  function onRules(e) {
    const el = e.target, r = L.cur.strategy;
    if (el.dataset.c) return onCost(el);
    if (el.dataset.tog && e.type === "change") {
      const g = el.dataset.tog, on = el.checked, c = L.tick ?? 0, d = Math.max(L.pegged ? 20 : 400, 10 * sp());
      if (g === "cutoff") { r.cutoff_lo = on ? c - d : null; r.cutoff_hi = on ? c + d : null; }
      if (g === "exit") { r.exit_lo = on ? c - 2 * d : null; r.exit_hi = on ? c + 2 * d : null; }
      if (g === "guard") r.guard = on ? (L.pegged ? 20 : 300) : null;
      renderEd(); chartRange(); changed(); return;
    }
    const k = el.dataset.r;
    if (!k) return;
    if (el.type === "checkbox") { if (e.type !== "change") return; r[k] = el.checked; }
    else if (el.tagName === "SELECT") { if (e.type !== "change") return; r[k] = el.value; if (k === "placement") { renderEd(); chartRange(); changed(); return; } }
    else if (el.value === "") r[k] = ["cutoff_lo", "cutoff_hi", "exit_lo", "exit_hi", "guard"].includes(k) ? null : 0;
    else r[k] = parseFloat(el.value);
    syncInputs(); chartRange(); changed();
  }
  function syncInputs() {
    const r = L.cur && L.cur.strategy;
    if (!r || r.type !== "custom") return;
    document.querySelectorAll("#labEdBody [data-r]").forEach(el => {
      const k = el.dataset.r;
      if (el.type === "checkbox" || el.tagName === "SELECT") return;
      if (document.activeElement !== el) el.value = r[k] ?? "";
      const h = document.querySelector(`#labEdBody [data-hint="${k}"]`);
      if (h) h.textContent = ["cutoff_lo", "cutoff_hi", "exit_lo", "exit_hi"].includes(k) ? absHint(r[k]) : k === "width" ? "≈ " + fmt(pct(+r[k] || 0), 3) + "%" : k === "guard" ? "" : tickHint(r[k]);
      if (k === "skew") el.previousElementSibling.querySelector("i").textContent = fmt((r.skew ?? .5) * 100, 0) + "%";
    });
  }
  // ladder form
  function renderLadder(box) {
    const s = L.cur.strategy, d = S.ladder_defaults || {};
    const f = (k, l, step) => `<div data-help="param-${k}"><label class="f">${l}</label><input type="number" step="${step}" data-l="${k}" value="${s[k] ?? ""}" placeholder="${k === "span" ? "pool default" : d[k] ?? ""}"></div>`;
    box.innerHTML = `<p class="mute small" style="margin-top:0">The app's own strategy. Empty fields use the pool's defaults.</p><div class="lf">
      ${f("lookback", "Lookback (s)", 3600)}${f("span", "Span (ticks each side)", 1)}${f("every", "Re-solve every (bars of 5 min)", 1)}${f("turnover", "Move when this share changes", 0.05)}${f("guard", "Depeg guard (ticks; empty = off)", 1)}</div>` + costsBox();
    box.oninput = e => {
      const el = e.target;
      if (el.dataset.c) return onCost(el);
      if (!el.dataset.l) return;
      if (el.value === "") delete s[el.dataset.l]; else s[el.dataset.l] = parseFloat(el.value);
      changed();
    };
  }

  // ---------- node editor ----------
  let nodeDrag = null, wireDrag = null;
  function renderGraph(box) {
    const cat = (L.lib && L.lib.catalog) || [];
    box.innerHTML = `<div class="ne" data-help="lab-nodes">
      <div class="cv" id="neCv"><div class="area" id="neArea"><svg class="wires" id="neWires"></svg></div>
      <div class="hint">＋ Blocks or a right-click adds a block (drag it from the list to place it); right-click a block or wire to edit it.
        Hold the middle button (or drag the background) to move around; the wheel zooms.</div>
      <div class="tb" data-help="lab-toolbox"><button class="pri" id="neAdd" title="add a block: indicators, maths, conditions, outputs">＋ Blocks</button><span class="div"></span>
        <button id="neTidy" title="lay the blocks out in columns, sources left and outputs right">Tidy</button><span class="div"></span>
        <button id="neZout" title="zoom out">−</button><button id="neFit" title="fit every block in view">100%</button><button id="neZin" title="zoom in">+</button></div>
      <div class="mm" id="neMm" title="minimap: click or drag to move the view, right click to see every block"><svg id="neMmSvg"></svg></div></div></div>
      <div class="row" style="margin-top:10px">${exitToSel(L.cur.strategy.exit_to).replace("<div ", '<div style="min-width:220px" ')}<span class="sp"></span></div>` + costsBox();
    box.oninput = box.onchange = e => {
      const el = e.target;
      if (el.dataset.c) return onCost(el);
      if (el.dataset.r === "exit_to" && e.type === "change") { L.cur.strategy.exit_to = el.value; changed(); return; }
      const nd = el.closest(".nd");
      if (!nd) return;
      const n = node(nd.dataset.id);
      if (el.dataset.p) { const v = parseFloat(el.value); if (isFinite(v)) { n.p = n.p || {}; n.p[el.dataset.p] = v; changed(); } }
      if (el.dataset.unit && e.type === "change") { n.unit = el.value; changed(); }
      if (el.dataset.label) { n.label = el.value; changed(); }
    };
    drawNodes();
    if (overlapping()) { tidy(); drawNodes(); }
    if (!L.cur.view) fit(); else applyView();
    $("neTidy").onclick = () => { tidy(); L.dirty = true; drawNodes(); fit(); const d = $("labDirty"); if (d) d.textContent = "unsaved changes"; };
    $("neAdd").onclick = e => {
      const r = e.currentTarget.getBoundingClientRect(), cv = $("neCv"), v = view();
      addMenu(r.left, r.bottom + 6, [(cv.clientWidth / 2 - v.x) / v.z, (cv.clientHeight / 2 - v.y) / v.z]);
    };
    viewEvents();
    menuEvents();
  }
  const node = id => L.cur.strategy.nodes.find(n => n.id === id);
  // blocks change size after they are drawn (fonts arriving, a select filling in): the wires follow
  let wireRO = null;
  function drawNodes() {
    const area = $("neArea");
    if (!area) return;
    if (window.ResizeObserver && (!wireRO || wireRO.area !== area)) {
      if (wireRO) wireRO.disconnect();
      let raf = 0;
      wireRO = new ResizeObserver(() => { cancelAnimationFrame(raf); raf = requestAnimationFrame(() => { drawWires(); drawMinimap(); }); });
      wireRO.area = area;
    }
    const nodes = L.cur.strategy.nodes;
    area.querySelectorAll(".nd").forEach(e => e.remove());
    const errs = graphErrors();
    let W = 900, H = 520;
    for (const n of nodes) {
      const c = kcat(n.kind);
      if (!c) continue;
      const el = document.createElement("div");
      el.className = "nd" + (L.sel === n.id ? " sel" : "") + (errs[n.id] ? " bad" : "");
      el.dataset.id = n.id;
      el.dataset.help = "node-" + n.kind;
      el.style.left = (n.x || 0) + "px"; el.style.top = (n.y || 0) + "px";
      const ticks = c.params.some(p => p.ticks);
      el.innerHTML = `<div class="hd" title="${esc(errs[n.id] || K[n.kind][2])}"><i style="background:${gcol(n.kind)}"></i><b>${esc(n.label || K[n.kind][0])}</b><span class="x" data-del="${esc(n.id)}" title="remove">×</span></div>
        <div class="bd">${c.inputs.map(p => `<div class="pr ${c.optional.includes(p) ? "opt" : ""}"><span>${p}${c.optional.includes(p) ? " (optional)" : ""}</span><span class="dot in" data-in="${p}" style="background:${n.in && n.in[p] ? "var(--fg2)" : "var(--line2)"}"></span></div>`).join("")}
        ${c.outputs.map(p => `<div class="pr" style="justify-content:flex-end"><span>${p}</span><span class="dot out" data-out="${p}" style="background:${BOOL.has(n.kind) ? "var(--warn)" : c.price ? "var(--acc)" : "var(--info)"}"></span></div>`).join("")}
        ${c.params.length || ticks ? `<div class="pm">${c.params.map(p => `<span>${PNAME[p.name] || p.name}</span><input type="number" step="any" data-p="${p.name}" value="${(n.p || {})[p.name] ?? p.default}" min="${p.min}" max="${p.max}">`).join("")}
          ${ticks ? `<span>unit</span><select data-unit="1">${["ticks", "spacings", "percent"].map(u => `<option ${(n.unit || "ticks") === u ? "selected" : ""}>${u}</option>`).join("")}</select>` : ""}</div>` : ""}</div>`;
      area.appendChild(el);
      W = Math.max(W, (n.x || 0) + 260); H = Math.max(H, (n.y || 0) + 260);
    }
    area.style.width = W + "px"; area.style.height = H + "px";
    drawWires(); drawMinimap();
    if (wireRO) area.querySelectorAll(".nd").forEach(n => wireRO.observe(n));
    requestAnimationFrame(() => drawWires());
    area.onpointerdown = ndDown;
  }
  function portXY(id, dir, port) {
    const area = $("neArea"), el = area && area.querySelector(`.nd[data-id="${CSS.escape(id)}"] .dot[data-${dir}="${port}"]`);
    if (!el) return null;
    // the scale the area is really drawn at (not the one asked for: before the first fit, or
    // while another strategy's view is still applied, they differ and the wires land beside the dots)
    const a = area.getBoundingClientRect(), r = el.getBoundingClientRect(), z = area.offsetWidth ? a.width / area.offsetWidth : view().z;
    return [(r.left - a.left + r.width / 2) / z, (r.top - a.top + r.height / 2) / z];
  }
  const curve = (a, b) => { const dx = Math.max(40, Math.abs(b[0] - a[0]) / 2); return `M${a[0]},${a[1]} C${a[0] + dx},${a[1]} ${b[0] - dx},${b[1]} ${b[0]},${b[1]}`; };
  function drawWires(temp) {
    const svg = $("neWires");
    if (!svg || !L.cur || L.cur.strategy.type !== "graph") return;
    let h = "";
    for (const n of L.cur.strategy.nodes) for (const [port, src] of Object.entries(n.in || {})) {
      const a = portXY(src[0], "out", src[1]), b = portXY(n.id, "in", port);
      if (!a || !b) continue;
      const s = node(src[0]), col = s && BOOL.has(s.kind) ? "var(--warn)" : s && kcat(s.kind) && kcat(s.kind).price ? "var(--acc)" : "var(--info)";
      h += `<path d="${curve(a, b)}" stroke="${col}" data-wire="${esc(n.id)}|${esc(port)}"/>`;
    }
    if (temp) h += `<path d="${curve(temp[0], temp[1])}" stroke="var(--fg2)" stroke-dasharray="5 4"/>`;
    svg.innerHTML = h;
    svg.onclick = e => { const w = e.target.dataset && e.target.dataset.wire; if (!w) return; const [id, port] = w.split("|"); delete node(id).in[port]; changed(); drawNodes(); };
  }
  function areaXY(e) { const a = $("neArea").getBoundingClientRect(), z = view().z; return [(e.clientX - a.left) / z, (e.clientY - a.top) / z]; }

  // ---- view: pan (middle button, or dragging the background), wheel zoom, minimap ----
  const view = () => L.cur.view || {x: 0, y: 0, z: 1};
  const ZMIN = 0.25, ZMAX = 2;
  function applyView() {
    const a = $("neArea"), v = view();
    closeMenu();
    if (!a) return;
    a.style.transform = `translate(${v.x}px,${v.y}px) scale(${v.z})`;
    drawWires();
    const zb = $("neFit");
    if (zb) zb.textContent = Math.round(v.z * 100) + "%";
    drawMinimap();
  }
  // the blocks' bounding box in area coordinates
  function bbox() {
    const els = [...document.querySelectorAll("#neArea .nd")];
    if (!els.length) return [0, 0, 400, 300];
    const xs = els.map(e => [e.offsetLeft, e.offsetTop, e.offsetLeft + e.offsetWidth, e.offsetTop + e.offsetHeight]);
    return [Math.min(...xs.map(r => r[0])), Math.min(...xs.map(r => r[1])), Math.max(...xs.map(r => r[2])), Math.max(...xs.map(r => r[3]))];
  }
  function fit() {
    const cv = $("neCv");
    if (!cv) return;
    const [x0, y0, x1, y1] = bbox(), pad = 28, W = cv.clientWidth, H = cv.clientHeight;
    if (!W || !H) { L.cur.view = null; return; } // hidden: fitted when the lab is shown
    const z = Math.min(1, Math.max(ZMIN, Math.min((W - 2 * pad) / (x1 - x0 || 1), (H - 2 * pad - 30) / (y1 - y0 || 1))));
    L.cur.view = {z, x: (W - (x1 - x0) * z) / 2 - x0 * z, y: Math.max(pad, (H - 30 - (y1 - y0) * z) / 2) - y0 * z};
    applyView();
  }
  function zoomAt(f, cx, cy) {
    const v = view(), z = Math.min(ZMAX, Math.max(ZMIN, v.z * f));
    v.x = cx - (cx - v.x) * z / v.z; v.y = cy - (cy - v.y) * z / v.z; v.z = z;
    L.cur.view = v; applyView();
  }
  function drawMinimap() {
    const svg = $("neMmSvg"), cv = $("neCv");
    if (!svg || !cv) return;
    const v = view(), W = cv.clientWidth, H = cv.clientHeight;
    // the visible part of the area, in area coordinates
    const vis = [-v.x / v.z, -v.y / v.z, (W - v.x) / v.z, (H - v.y) / v.z];
    const b = bbox(), all = [Math.min(b[0], vis[0]), Math.min(b[1], vis[1]), Math.max(b[2], vis[2]), Math.max(b[3], vis[3])];
    const mw = 168, mh = 108, pad = 6, k = Math.min((mw - 2 * pad) / (all[2] - all[0] || 1), (mh - 2 * pad) / (all[3] - all[1] || 1));
    const ox = pad + ((mw - 2 * pad) - (all[2] - all[0]) * k) / 2 - all[0] * k, oy = pad + ((mh - 2 * pad) - (all[3] - all[1]) * k) / 2 - all[1] * k;
    L.mm = {k, ox, oy};
    svg.setAttribute("viewBox", `0 0 ${mw} ${mh}`);
    svg.innerHTML = [...document.querySelectorAll("#neArea .nd")].map(e => {
      const n = node(e.dataset.id);
      return `<rect x="${ox + e.offsetLeft * k}" y="${oy + e.offsetTop * k}" width="${Math.max(2, e.offsetWidth * k)}" height="${Math.max(2, e.offsetHeight * k)}" rx="1.5"
        fill="${n ? gcol(n.kind) : "var(--mute)"}" opacity="${L.sel === e.dataset.id ? 1 : .55}"/>`;
    }).join("") + `<rect x="${ox + vis[0] * k}" y="${oy + vis[1] * k}" width="${(vis[2] - vis[0]) * k}" height="${(vis[3] - vis[1]) * k}" rx="2"
        fill="color-mix(in srgb,var(--acc) 10%,transparent)" stroke="var(--acc)" stroke-width="1.2"/>`;
  }
  function viewEvents() {
    const cv = $("neCv"), mm = $("neMm");
    // no browser autoscroll on the middle button
    cv.addEventListener("mousedown", e => { if (e.button === 1) e.preventDefault(); });
    cv.addEventListener("auxclick", e => { if (e.button === 1) e.preventDefault(); });
    cv.addEventListener("wheel", e => {
      e.preventDefault();
      const r = cv.getBoundingClientRect(), d = e.deltaMode === 1 ? e.deltaY * 33 : e.deltaY;
      zoomAt(Math.exp(-d * 0.0015), e.clientX - r.left, e.clientY - r.top);
    }, {passive: false});
    const pts = new Map();
    let pan = null, pinch = null;
    cv.addEventListener("pointerdown", e => {
      if (e.target.closest(".mm,.zoom,.tb")) return;
      const bg = !e.target.closest(".nd") && !(e.target.dataset && e.target.dataset.wire);
      if (e.button === 1 || (e.button === 0 && bg)) {
        e.preventDefault(); e.stopPropagation();
        pts.set(e.pointerId, [e.clientX, e.clientY]);
        cv.setPointerCapture(e.pointerId);
        if (pts.size === 2) {
          const [a, b] = [...pts.values()], v = view();
          pinch = {d: Math.hypot(a[0] - b[0], a[1] - b[1]) || 1, z: v.z};
          pan = null;
        } else {
          const v = view();
          pan = {x: e.clientX, y: e.clientY, vx: v.x, vy: v.y};
          cv.classList.add("pan");
        }
      }
    }, true);
    cv.addEventListener("pointermove", e => {
      if (!pts.has(e.pointerId)) return;
      pts.set(e.pointerId, [e.clientX, e.clientY]);
      const v = view();
      if (pinch && pts.size === 2) {
        const [a, b] = [...pts.values()], r = cv.getBoundingClientRect();
        zoomAt(pinch.z * Math.hypot(a[0] - b[0], a[1] - b[1]) / pinch.d / v.z, (a[0] + b[0]) / 2 - r.left, (a[1] + b[1]) / 2 - r.top);
      } else if (pan) {
        v.x = pan.vx + e.clientX - pan.x; v.y = pan.vy + e.clientY - pan.y;
        L.cur.view = v; applyView();
      }
    });
    const end = e => {
      if (!pts.delete(e.pointerId)) return;
      if (pts.size < 2) pinch = null;
      if (!pts.size) { pan = null; cv.classList.remove("pan"); }
    };
    cv.addEventListener("pointerup", end);
    cv.addEventListener("pointercancel", end);
    // minimap: centre the view where it is clicked or dragged
    const goto = e => {
      const r = mm.getBoundingClientRect(), m = L.mm, v = view();
      if (!m) return;
      const ax = ((e.clientX - r.left) * 168 / r.width - m.ox) / m.k, ay = ((e.clientY - r.top) * 108 / r.height - m.oy) / m.k;
      v.x = cv.clientWidth / 2 - ax * v.z; v.y = cv.clientHeight / 2 - ay * v.z;
      L.cur.view = v; applyView();
    };
    mm.addEventListener("pointerdown", e => { if (e.button !== 0) return; e.preventDefault(); e.stopPropagation(); mm.setPointerCapture(e.pointerId); mm.dragging = true; goto(e); });
    // right click: every block in view
    mm.addEventListener("contextmenu", e => { e.preventDefault(); e.stopPropagation(); fit(); });
    mm.addEventListener("pointermove", e => { if (mm.dragging) goto(e); });
    mm.addEventListener("pointerup", () => { mm.dragging = false; });
    $("neZin").onclick = () => zoomAt(1.2, cv.clientWidth / 2, cv.clientHeight / 2);
    $("neZout").onclick = () => zoomAt(1 / 1.2, cv.clientWidth / 2, cv.clientHeight / 2);
    $("neFit").onclick = fit;
  }
  function ndDown(e) {
    const del = e.target.closest("[data-del]");
    if (del) { removeNode(del.dataset.del); return; }
    const dot = e.target.closest(".dot");
    const nd = e.target.closest(".nd");
    if (dot && dot.dataset.out) {
      e.preventDefault();
      wireDrag = {from: [nd.dataset.id, dot.dataset.out], a: portXY(nd.dataset.id, "out", dot.dataset.out)};
      $("neArea").setPointerCapture(e.pointerId);
    } else if (dot && dot.dataset.in) {
      // pick up an existing wire from its input end
      const n = node(nd.dataset.id), src = n.in && n.in[dot.dataset.in];
      if (src) { e.preventDefault(); delete n.in[dot.dataset.in]; wireDrag = {from: src, a: portXY(src[0], "out", src[1])}; $("neArea").setPointerCapture(e.pointerId); drawWires(); }
    } else if (nd && e.target.closest(".hd")) {
      e.preventDefault();
      const n = node(nd.dataset.id), [x, y] = areaXY(e);
      nodeDrag = {id: n.id, dx: x - (n.x || 0), dy: y - (n.y || 0), moved: false};
      L.sel = n.id;
      document.querySelectorAll("#neArea .nd").forEach(el => el.classList.toggle("sel", el.dataset.id === n.id));
      $("neArea").setPointerCapture(e.pointerId);
    }
    const area = $("neArea");
    area.onpointermove = ev => {
      if (nodeDrag) {
        const [x, y] = areaXY(ev), n = node(nodeDrag.id);
        n.x = Math.round((x - nodeDrag.dx) / 6) * 6; n.y = Math.round((y - nodeDrag.dy) / 6) * 6;
        const el = area.querySelector(`.nd[data-id="${CSS.escape(n.id)}"]`);
        el.style.left = n.x + "px"; el.style.top = n.y + "px"; nodeDrag.moved = true;
        drawWires(); drawMinimap();
      } else if (wireDrag) drawWires([wireDrag.a, areaXY(ev)]);
    };
    area.onpointerup = ev => {
      if (wireDrag) {
        const t = document.elementFromPoint(ev.clientX, ev.clientY), dot = t && t.closest(".dot.in"), nd2 = t && t.closest(".nd");
        if (dot && nd2 && nd2.dataset.id !== wireDrag.from[0]) {
          const n = node(nd2.dataset.id);
          n.in = n.in || {};
          n.in[dot.dataset.in] = wireDrag.from;
        }
        wireDrag = null; changed(); drawNodes();
      }
      if (nodeDrag) { if (nodeDrag.moved) L.dirty = true; nodeDrag = null; drawNodes(); }
    };
  }
  // Tidy: a layered layout. Each block goes one column right of its furthest input (blocks without
  // inputs sit just left of what they feed), the outputs share the last column; the order within a
  // column follows the average height of the neighbours (a few sweeps both ways, fewer crossings),
  // and every column is centred on the tallest one.
  function tidy() {
    const s = L.cur.strategy, nodes = s.nodes, by = Object.fromEntries(nodes.map(n => [n.id, n]));
    const SINKS = ["range", "rebalance", "exit", "hold"];
    const ins = n => Object.values(n.in || {}).map(src => by[src[0]]).filter(Boolean);
    const outs = {};
    nodes.forEach(n => ins(n).forEach(m => (outs[m.id] = outs[m.id] || []).push(n)));
    const depth = {};
    const d = (n, seen = new Set()) => {
      if (depth[n.id] != null) return depth[n.id];
      if (seen.has(n.id)) return 0;
      seen.add(n.id);
      return depth[n.id] = Math.max(0, ...ins(n).map(m => d(m, seen) + 1));
    };
    nodes.forEach(n => d(n));
    const last = Math.max(1, ...nodes.filter(n => !SINKS.includes(n.kind)).map(n => depth[n.id] + 1));
    nodes.forEach(n => { if (SINKS.includes(n.kind)) depth[n.id] = last; });
    // inputless blocks next to their first consumer
    for (let pass = 0; pass < 2; pass++) nodes.forEach(n => {
      if (!ins(n).length && !SINKS.includes(n.kind) && (outs[n.id] || []).length) depth[n.id] = Math.max(0, Math.min(...outs[n.id].map(m => depth[m.id])) - 1);
    });
    const ncol = Math.max(...nodes.map(n => depth[n.id])) + 1;
    const cols = Array.from({length: ncol}, () => []);
    nodes.forEach(n => cols[depth[n.id]].push(n));
    const order = {};
    cols.forEach(c => { c.sort((a, b) => (a.y || 0) - (b.y || 0)); c.forEach((n, i) => order[n.id] = i); });
    cols[ncol - 1].sort((a, b) => SINKS.indexOf(a.kind) - SINKS.indexOf(b.kind));
    const bary = (n, nb) => { const xs = nb.map(m => order[m.id]).filter(v => v != null); return xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : order[n.id]; };
    for (let sweep = 0; sweep < 4; sweep++) {
      const range = sweep % 2 ? [...cols.keys()].reverse() : [...cols.keys()];
      for (const k of range) {
        if (k === ncol - 1) continue; // outputs keep range, rebalance, exit, hold
        const nb = n => sweep % 2 ? (outs[n.id] || []) : ins(n);
        cols[k].sort((a, b) => bary(a, nb(a)) - bary(b, nb(b)));
        cols[k].forEach((n, i) => order[n.id] = i);
      }
    }
    const h = n => { const el = document.querySelector(`#neArea .nd[data-id="${CSS.escape(n.id)}"]`); return el ? el.offsetHeight : 120; };
    const GAP = 26, COLW = 176 + 70;
    const heights = cols.map(c => c.reduce((a, n) => a + h(n) + GAP, -GAP));
    const tall = Math.max(...heights);
    cols.forEach((c, k) => {
      let y = 20 + (tall - heights[k]) / 2;
      for (const n of c) { n.x = 20 + k * COLW; n.y = Math.round(y / 6) * 6; y += h(n) + GAP; }
    });
  }
  function overlapping() {
    const r = [...document.querySelectorAll("#neArea .nd")].map(e => [e.offsetLeft, e.offsetTop, e.offsetWidth, e.offsetHeight]);
    return r.some((a, i) => r.some((b, j) => j > i && a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]));
  }
  // ---- context menu: add blocks (right-click the canvas), edit a block or a wire (right-click it) ----
  let menu = null;
  function closeMenu() { if (menu) { menu.remove(); menu = null; document.removeEventListener("pointerdown", outside, true); document.removeEventListener("keydown", menuKey, true); } }
  function outside(e) { if (menu && !menu.contains(e.target)) closeMenu(); }
  function menuKey(e) {
    if (e.key === "Escape") { closeMenu(); e.preventDefault(); return; }
    const items = [...menu.querySelectorAll(".ci")].filter(x => x.offsetParent);
    let i = items.findIndex(x => x.classList.contains("on"));
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      items.forEach(x => x.classList.remove("on"));
      i = e.key === "ArrowDown" ? (i + 1) % items.length : (i - 1 + items.length) % items.length;
      if (items[i]) { items[i].classList.add("on"); items[i].scrollIntoView({block: "nearest"}); }
    } else if (e.key === "Enter" && items.length && document.activeElement.dataset.rename == null) {
      e.preventDefault(); (items[Math.max(0, i)]).click();
    }
  }
  function openMenu(x, y, html, bind) {
    closeMenu();
    menu = document.createElement("div");
    menu.className = "cm";
    menu.innerHTML = html;
    document.body.appendChild(menu);
    const r = menu.getBoundingClientRect();
    menu.style.left = Math.max(6, Math.min(x, innerWidth - r.width - 6)) + "px";
    menu.style.top = Math.max(6, Math.min(y, innerHeight - r.height - 6)) + "px";
    menu.oncontextmenu = e => e.preventDefault();
    bind(menu);
    setTimeout(() => { document.addEventListener("pointerdown", outside, true); document.addEventListener("keydown", menuKey, true); });
  }
  const item = (act, label, extra = "") => `<div class="ci ${act === "delete" ? "bad" : ""}" data-act="${act}">${label}${extra}</div>`;
  function addMenu(cx, cy, at) {
    const cat = (L.lib && L.lib.catalog) || [];
    const list = () => GROUPS.map(([g, name, col]) => {
      const ks = cat.filter(c => (K[c.kind] || [])[1] === g);
      return ks.length ? `<div class="gl" data-g="${g}">${name}</div>` + ks.map(c => `<div class="ci" data-add="${c.kind}" data-q="${esc((K[c.kind][0] + " " + c.kind + " " + name).toLowerCase())}"
        title="${esc(K[c.kind][2])}"><i style="background:${col}"></i>${esc(K[c.kind][0])}</div>`).join("") : "";
    }).join("");
    openMenu(cx, cy, `<input placeholder="Add a block: type to search…" spellcheck="false"><div class="list">${list()}</div>
      <div class="sep"></div>${item("paste", "Paste block", L.clip ? "" : " <kbd>nothing copied</kbd>")}${item("tidy", "Tidy blocks")}${item("fit", "Fit everything in view")}`, m => {
      const inp = m.querySelector("input");
      inp.focus();
      inp.oninput = () => {
        const q = inp.value.trim().toLowerCase();
        m.querySelectorAll("[data-add]").forEach(x => { x.style.display = !q || x.dataset.q.includes(q) ? "" : "none"; x.classList.remove("on"); });
        m.querySelectorAll(".gl").forEach(g => { g.style.display = [...m.querySelectorAll(`[data-add]`)].some(x => x.style.display !== "none" && x.dataset.q.endsWith(g.textContent.toLowerCase())) ? "" : "none"; });
        const first = [...m.querySelectorAll("[data-add]")].find(x => x.style.display !== "none");
        if (first && q) first.classList.add("on");
      };
      m.addEventListener("pointerdown", e => { const a = e.target.closest("[data-add]"); if (a && e.button === 0) blockDrag(e, a.dataset.add, at); });
      m.onclick = e => {
        const a = e.target.closest("[data-add]"), act = e.target.closest("[data-act]");
        if (a) { if (e.detail === 0) { closeMenu(); addNode(a.dataset.add, at[0] - 20, at[1] - 14); } return; } // keyboard; the pointer goes through blockDrag
        if (!act) return;
        closeMenu();
        if (act.dataset.act === "paste" && L.clip) pasteNode(at);
        if (act.dataset.act === "tidy") $("neTidy").click();
        if (act.dataset.act === "fit") fit();
      };
    });
  }
  function nodeMenu(cx, cy, id) {
    const n = node(id), c = kcat(n.kind), ticks = c && c.params.some(p => p.ticks);
    L.sel = id;
    document.querySelectorAll("#neArea .nd").forEach(el => el.classList.toggle("sel", el.dataset.id === id));
    const ins = Object.keys(n.in || {}).length, outs = L.cur.strategy.nodes.some(x => Object.values(x.in || {}).some(src => src[0] === id));
    openMenu(cx, cy, `<div class="ttl">${esc(n.label || K[n.kind][0])}</div>
      <input data-rename value="${esc(n.label || "")}" placeholder="${esc(K[n.kind][0])} (name it…)" maxlength="40" spellcheck="false">
      ${c && c.params.length ? item("reset", "Reset settings to defaults") : ""}
      ${ticks ? `<div class="gl">Unit</div>` + ["ticks", "spacings", "percent"].map(u => item("unit:" + u, ((n.unit || "ticks") === u ? "● " : "○ ") + u)).join("") + `<div class="sep"></div>` : ""}
      ${item("dup", "Duplicate", "<kbd>Ctrl+D</kbd>")}${item("copy", "Copy", "<kbd>Ctrl+C</kbd>")}
      ${ins ? item("unin", "Disconnect its inputs") : ""}${outs ? item("unout", "Disconnect its outputs") : ""}
      ${item("help", "What does it do?")}<div class="sep"></div>${item("delete", "Delete block", "<kbd>Del</kbd>")}`, m => {
      const inp = m.querySelector("[data-rename]");
      inp.onkeydown = e => { if (e.key === "Enter") { e.preventDefault(); n.label = inp.value.trim(); closeMenu(); changed(); drawNodes(); } };
      inp.onchange = () => { n.label = inp.value.trim(); changed(); drawNodes(); };
      m.onclick = e => {
        const act = e.target.closest("[data-act]");
        if (!act) return;
        const a = act.dataset.act;
        closeMenu();
        if (a === "delete") return removeNode(id);
        if (a === "dup") return dupNode(id);
        if (a === "copy") { L.clip = JSON.parse(JSON.stringify(n)); return toast(`Copied ${n.label || K[n.kind][0]}: right-click the canvas to paste.`); }
        if (a === "unin") n.in = {};
        if (a === "unout") for (const x of L.cur.strategy.nodes) for (const [p, src] of Object.entries(x.in || {})) if (src[0] === id) delete x.in[p];
        if (a === "reset") for (const p of c.params) n.p[p.name] = p.default;
        if (a.startsWith("unit:")) n.unit = a.slice(5);
        if (a === "help") { const el = document.querySelector(`#neArea .nd[data-id="${CSS.escape(id)}"]`); if (el) el.dispatchEvent(new MouseEvent("mouseover", {bubbles: true})); return; }
        changed(); drawNodes();
      };
    });
  }
  function wireMenu(cx, cy, w) {
    const [id, port] = w.split("|"), n = node(id), src = n.in[port], s = node(src[0]);
    openMenu(cx, cy, `<div class="ttl">${esc(s.label || K[s.kind][0])} ${esc(src[1])} → ${esc(n.label || K[n.kind][0])} ${esc(port)}</div>${item("delete", "Remove this connection")}`, m => {
      m.onclick = e => { if (e.target.closest("[data-act]")) { closeMenu(); delete n.in[port]; changed(); drawNodes(); } };
    });
  }
  function freeId(kind) {
    let i = 1;
    while (L.cur.strategy.nodes.some(n => n.id === kind.slice(0, 3) + i)) i++;
    return kind.slice(0, 3) + i;
  }
  function dupNode(id) {
    const n = node(id);
    if (["range", "rebalance", "exit", "hold"].includes(n.kind)) return toast(`There can be only one ${K[n.kind][0]} block.`, true);
    const c = JSON.parse(JSON.stringify(n));
    c.id = freeId(n.kind); c.x = (n.x || 0) + 24; c.y = (n.y || 0) + 24;
    L.cur.strategy.nodes.push(c); L.sel = c.id; changed(); drawNodes();
  }
  function pasteNode(at) {
    const c = JSON.parse(JSON.stringify(L.clip));
    if (["range", "rebalance", "exit", "hold"].includes(c.kind) && L.cur.strategy.nodes.some(n => n.kind === c.kind)) return toast(`There can be only one ${K[c.kind][0]} block.`, true);
    c.id = freeId(c.kind); c.x = Math.round(at[0]); c.y = Math.round(at[1]);
    // keep inputs that still exist
    for (const [p, src] of Object.entries(c.in || {})) if (!node(src[0])) delete c.in[p];
    L.cur.strategy.nodes.push(c); L.sel = c.id; changed(); drawNodes();
  }
  function menuEvents() {
    const cv = $("neCv");
    cv.addEventListener("contextmenu", e => {
      e.preventDefault();
      if (e.target.closest(".mm,.zoom,.tb")) return;
      const nd = e.target.closest(".nd"), w = e.target.dataset && e.target.dataset.wire;
      if (w) return wireMenu(e.clientX, e.clientY, w);
      if (nd) return nodeMenu(e.clientX, e.clientY, nd.dataset.id);
      addMenu(e.clientX, e.clientY, areaXY(e));
    });
    // keys for the selected block, while the pointer is over the canvas
    cv.tabIndex = 0;
    cv.addEventListener("pointerdown", e => { if (!e.target.closest("input,select")) cv.focus({preventScroll: true}); });
    cv.addEventListener("keydown", e => {
      if (e.target !== cv || !L.sel || !node(L.sel)) return;
      const k = e.key.toLowerCase();
      if (k === "delete" || k === "backspace") { e.preventDefault(); removeNode(L.sel); L.sel = null; }
      else if ((e.ctrlKey || e.metaKey) && k === "d") { e.preventDefault(); dupNode(L.sel); }
      else if ((e.ctrlKey || e.metaKey) && k === "c") { L.clip = JSON.parse(JSON.stringify(node(L.sel))); toast("Copied: right-click the canvas to paste."); }
      else if ((e.ctrlKey || e.metaKey) && k === "v" && L.clip) { const v = view(); pasteNode([(cv.clientWidth / 2 - v.x) / v.z, (cv.clientHeight / 2 - v.y) / v.z]); }
    });
  }
  function removeNode(id) {
    const s = L.cur.strategy;
    s.nodes = s.nodes.filter(n => n.id !== id);
    for (const n of s.nodes) for (const [p, src] of Object.entries(n.in || {})) if (src[0] === id) delete n.in[p];
    changed(); drawNodes();
  }
  // a block picked in the add menu: a click adds it at `at`, a drag drops it where the pointer is let go
  function blockDrag(e, kind, at) {
    e.preventDefault();
    const x0 = e.clientX, y0 = e.clientY;
    let g = null;
    const move = ev => {
      if (!g && Math.abs(ev.clientX - x0) + Math.abs(ev.clientY - y0) > 5) {
        g = document.createElement("div"); g.className = "ghost"; g.textContent = K[kind][0]; document.body.appendChild(g);
        if (menu) menu.style.opacity = ".35";
      }
      if (g) { g.style.left = ev.clientX + 8 + "px"; g.style.top = ev.clientY + 8 + "px"; }
    };
    const done = () => { document.removeEventListener("pointermove", move); document.removeEventListener("pointerup", up); document.removeEventListener("pointercancel", cancel); if (g) g.remove(); closeMenu(); };
    const cancel = () => done();
    const up = ev => {
      const dragged = !!g;
      done();
      if (!dragged) return addNode(kind, at[0] - 20, at[1] - 14);
      const cv = $("neCv"), r = cv.getBoundingClientRect();
      if (ev.clientX >= r.left && ev.clientX <= r.right && ev.clientY >= r.top && ev.clientY <= r.bottom) { const [x, y] = areaXY(ev); addNode(kind, x - 20, y - 14); }
    };
    document.addEventListener("pointermove", move);
    document.addEventListener("pointerup", up);
    document.addEventListener("pointercancel", cancel);
  }
  function addNode(kind, x, y) {
    const s = L.cur.strategy, c = kcat(kind);
    if (["range", "rebalance", "exit", "hold"].includes(kind) && s.nodes.some(n => n.kind === kind)) return toast(`There is already a ${K[kind][0]} block.`, true);
    const n = {id: freeId(kind), kind, x: Math.round(x / 6) * 6, y: Math.round(y / 6) * 6, p: {}, in: {}};
    for (const p of c.params) n.p[p.name] = p.default;
    s.nodes.push(n);
    L.sel = n.id;
    changed(); drawNodes();
  }
  // what the app would refuse, shown on the blocks before a run
  function graphErrors() {
    const out = {}, s = L.cur.strategy;
    for (const n of s.nodes) {
      const c = kcat(n.kind);
      if (!c) continue;
      const miss = c.inputs.filter(p => !c.optional.includes(p) && !(n.in && n.in[p]));
      if (miss.length) out[n.id] = "connect: " + miss.join(", ");
    }
    return out;
  }

  // ---------- chart ----------
  let drag = null;
  const isRules = () => L.cur && L.cur.strategy.type === "custom";
  function geom() {
    const svg = $("labSvg"), W = svg.clientWidth || 800, H = 380;
    const pts = (L.res && L.res.price) || [];
    const c = L.tick;
    if (c == null || !L.cur) return null;
    const r = L.cur.strategy, rules = isRules();
    const lines = [c];
    let tl, tu, blo, bhi;
    if (rules) {
      [tl, tu] = rangeFor(c, r); [blo, bhi] = band(tl, tu, r);
      lines.push(tl, tu, blo, bhi);
      for (const k of ["cutoff_lo", "cutoff_hi", "exit_lo", "exit_hi"]) if (r[k] != null) lines.push(r[k]);
    }
    const ticks = pts.map(p => p[1]);
    let lo = Math.min(...lines, ...ticks), hi = Math.max(...lines, ...ticks);
    const pad = Math.max((hi - lo) * 0.08, 2 * sp());
    if (!drag) L.yr = [lo - pad, hi + pad]; else L.yr = [Math.min(L.yr[0], lo - pad / 2), Math.max(L.yr[1], hi + pad / 2)];
    const [y0, y1] = L.yr;
    const left = 64, right = rules ? 176 : 16, top = 10, bot = 26;
    const x0 = pts.length ? pts[0][0] : 0, x1 = pts.length ? pts[pts.length - 1][0] : 1;
    const X = t => left + (t - x0) / (x1 - x0 || 1) * (W - left - right);
    const Y = t => top + (y1 - t) / (y1 - y0 || 1) * (H - top - bot);
    const T = y => y1 - (y - top) / (H - top - bot) * (y1 - y0);
    if (rules && (!drag || !L.zs)) { const hs = Math.max(c - blo, bhi - c, 2 * sp()) * 1.9; L.zs = [c - hs, c + hs]; }
    const [s0, s1] = L.zs || [0, 1];
    const Ys = t => top + 22 + (s1 - t) / (s1 - s0) * (H - top - bot - 44);
    const Ts = y => s1 - (y - top - 22) / (H - top - bot - 44) * (s1 - s0);
    return {svg, W, H, pts, r, c, tl, tu, blo, bhi, X, Y, T, Ys, Ts, left, right, top, bot, x0, x1, rules};
  }
  const OVC = ["var(--c4)", "var(--c3)", "var(--c6)", "var(--c1)", "var(--c5)", "var(--info)"];
  // indicator lines start shown, intermediate maths (offsets, switches…) start hidden
  function ovOn(l) {
    if (L.ov[l.name] != null) return L.ov[l.name];
    const n = L.cur && L.cur.strategy.nodes && L.cur.strategy.nodes.find(x => x.id === l.node);
    return !n || (K[n.kind] || [])[1] === "ind";
  }
  function renderOverlayChips() {
    const ov = L.res && L.res.overlays;
    $("labOv").innerHTML = ov && ov.lines.length ? ov.lines.map((l, i) => `<button data-ov="${esc(l.name)}" class="${ovOn(l) ? "on" : ""}"><i style="background:${OVC[i % OVC.length]}"></i>${esc(l.name)}</button>`).join("") : "";
  }
  function chartRange() {
    const g = geom(), svg = $("labSvg");
    if (!g) { svg.innerHTML = `<text x="50%" y="50%" text-anchor="middle" class="yl">${L.pool ? (L.job ? "loading the pool's history…" : "no price yet") : "choose a pool"}</text>`; $("labNow").textContent = ""; $("labLegend").innerHTML = ""; return; }
    const {W, H, pts, r, c, tl, tu, blo, bhi, X, Y, Ys, left, right, top, bot, rules} = g;
    const xr = W - right, strip = [xr + 8, W - 4];
    let h = "";
    for (let i = 0; i <= 5; i++) {
      const t = L.yr[0] + (L.yr[1] - L.yr[0]) * i / 5, y = Y(t);
      h += `<line x1="${left}" x2="${xr}" y1="${y}" y2="${y}" stroke="var(--line)"/><text class="yl" x="${left - 6}" y="${y + 4}" text-anchor="end">${pfmt(t)}</text>`;
    }
    const main = L.res && L.res.runs.main;
    h += ((main && main.segments) || []).map(s => `<rect x="${X(s[0])}" width="${Math.max(1, X(s[1]) - X(s[0]))}" y="${Y(s[3])}" height="${Math.max(1, Y(s[2]) - Y(s[3]))}" fill="var(--acc)" opacity=".16"/>`).join("");
    if (rules) {
      const zone = (t, up, col, op) => up ? `<rect x="${left}" width="${xr - left}" y="${top}" height="${Math.max(0, Y(t) - top)}" fill="${col}" opacity="${op}"/>`
        : `<rect x="${left}" width="${xr - left}" y="${Y(t)}" height="${Math.max(0, H - bot - Y(t))}" fill="${col}" opacity="${op}"/>`;
      if (r.exit_hi != null) h += zone(r.exit_hi, true, COL.exit, .07);
      if (r.exit_lo != null) h += zone(r.exit_lo, false, COL.exit, .07);
      if (r.cutoff_hi != null) h += zone(r.cutoff_hi, true, COL.cutoff, .06);
      if (r.cutoff_lo != null) h += zone(r.cutoff_lo, false, COL.cutoff, .06);
    }
    if (pts.length > 1) h += `<polyline fill="none" stroke="var(--fg2)" stroke-width="1.4" points="${pts.map(p => `${X(p[0]).toFixed(1)},${Y(p[1]).toFixed(1)}`).join(" ")}"/>`;
    // node strategies: the wanted range and the indicator lines
    const ov = L.res && L.res.overlays;
    if (ov && !rules && pts.length) {
      const seg = (vals, f) => { let d = "", on = false; vals.forEach((v, i) => { if (v == null || i >= pts.length) { on = false; return; } d += `${on ? "L" : "M"}${X(pts[i][0]).toFixed(1)},${Y(f(v)).toFixed(1)}`; on = true; }); return d; };
      h += `<path d="${seg(ov.wanted, v => v[0])}" fill="none" stroke="var(--acc)" stroke-width="1.2" stroke-dasharray="5 4" opacity=".9"/><path d="${seg(ov.wanted, v => v[1])}" fill="none" stroke="var(--acc)" stroke-width="1.2" stroke-dasharray="5 4" opacity=".9"/>`;
      ov.lines.forEach((l, i) => { if (ovOn(l)) h += `<path d="${seg(l.v, v => v)}" fill="none" stroke="${OVC[i % OVC.length]}" stroke-width="1.3" opacity=".9"/>`; });
    }
    const ev = (main && main.events) || [];
    h += ev.map(e => `<line x1="${X(e.ts)}" x2="${X(e.ts)}" y1="${H - bot}" y2="${H - bot - 6}" stroke="${e.kind === "exit" ? COL.exit : COL.range}" stroke-width="1.5"/>`).join("");
    if (rules) {
      const sw = strip[1] - strip[0];
      h += `<rect x="${strip[0]}" width="${sw}" y="${top}" height="${H - top - bot}" rx="8" fill="var(--card2)" stroke="var(--line)"/>`;
      h += `<text class="yl" x="${strip[0] + sw / 2}" y="${top + 15}" text-anchor="middle">today, zoomed</text>`;
      h += `<rect x="${strip[0] + 6}" width="${sw - 12}" y="${Ys(bhi)}" height="${Math.max(1, Ys(blo) - Ys(bhi))}" rx="6" fill="color-mix(in srgb,var(--info) 6%,transparent)" stroke="${COL.buffer}" stroke-dasharray="4 3"/>`;
      h += `<rect x="${strip[0] + 18}" width="${sw - 36}" y="${Ys(tu)}" height="${Math.max(2, Ys(tl) - Ys(tu))}" rx="4" fill="var(--acc)" opacity=".3"/>`;
      h += `<line x1="${xr}" x2="${strip[0]}" y1="${Y(c)}" y2="${Ys(c)}" stroke="var(--fg)" stroke-dasharray="2 3" opacity=".35"/>`;
      h += `<line x1="${strip[0] + 18}" x2="${strip[1] - 18}" y1="${Ys(c)}" y2="${Ys(c)}" stroke="var(--fg)" stroke-width="1.5"/>`;
      h += `<text class="tipline" x="${strip[0] + sw / 2}" y="${H - bot - 8}" text-anchor="middle">now ${pfmt(c)}</text>`;
      const hs = [["tu", tu, COL.range, "top " + pfmt(tu)], ["tl", tl, COL.range, "bottom " + pfmt(tl)], ["bhi", bhi, COL.buffer, "moves ≥ " + pfmt(bhi)], ["blo", blo, COL.buffer, "moves < " + pfmt(blo)]];
      for (const [k, l, col] of [["cutoff_hi", "cutoff", COL.cutoff], ["cutoff_lo", "cutoff", COL.cutoff], ["exit_hi", "exit", COL.exit], ["exit_lo", "exit", COL.exit]])
        if (r[k] != null) hs.push([k, r[k], col, l, true]);
      for (const [k, t, col, label, full] of hs) {
        const y = full ? Y(t) : Ys(t);
        if (full) h += `<line x1="${left}" x2="${xr}" y1="${y}" y2="${y}" stroke="${col}" stroke-width="1.5" stroke-dasharray="${k.startsWith("exit") ? "" : "6 4"}"/>`;
        const hx = full ? xr - 74 : strip[0] + (k.startsWith("b") ? 4 : 16), hw = full ? 70 : sw - (k.startsWith("b") ? 8 : 32);
        h += `<g class="hd ${drag && drag.k === k ? "drag" : ""}" data-h="${k}"><rect x="${hx}" y="${y - 8}" width="${hw}" height="16" rx="8" fill="${col}" opacity="${full ? .9 : k.startsWith("b") ? .25 : .85}"/>
          <rect x="${hx}" y="${y - 12}" width="${hw}" height="24" fill="transparent"/>
          <text x="${hx + hw / 2}" y="${y + 3.5}" text-anchor="middle" fill="${k.startsWith("b") ? "var(--fg)" : "#06110c"}">${label}</text></g>`;
      }
      $("labNow").innerHTML = `range <b class=mono>${pfmt(tl)} – ${pfmt(tu)}</b> · ${tu - tl} ticks · moves outside <span class=mono>${pfmt(blo)} – ${pfmt(bhi)}</span>`;
      $("labLegend").innerHTML = `<span><i style="background:${COL.range}"></i>range at today's price (drag its edges)</span><span><i style="background:${COL.buffer}"></i>trigger band</span>
        <span><i style="background:${COL.cutoff}"></i>cutoffs</span><span><i style="background:${COL.exit}"></i>exits</span><span><i style="background:color-mix(in srgb,var(--acc) 35%,transparent);height:9px"></i>backtested ranges</span>`;
    } else {
      const w = ov && ov.wanted && ov.wanted[ov.wanted.length - 1];
      $("labNow").innerHTML = w ? `wanted now <b class=mono>${pfmt(w[0])} – ${pfmt(w[1])}</b> · ${w[1] - w[0]} ticks · price ${pfmt(c)}` : `price ${pfmt(c)}`;
      $("labLegend").innerHTML = `<span><i style="background:var(--fg2)"></i>price</span><span><i style="background:var(--acc);opacity:.8"></i>┅ range the strategy wants</span>
        <span><i style="background:color-mix(in srgb,var(--acc) 35%,transparent);height:9px"></i>backtested ranges</span>${ov && ov.lines.length ? "<span>indicator lines: toggle them above</span>" : ""}`;
    }
    const {x0, x1} = g;
    if (pts.length) h += [0, .5, 1].map(f => `<text class="yl" x="${left + f * (xr - left)}" y="${H - 6}" text-anchor="${f === 0 ? "start" : f === 1 ? "end" : "middle"}">${new Date((x0 + (x1 - x0) * f) * 1000).toLocaleDateString(undefined, {month: "short", day: "numeric"})}</text>`).join("");
    svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
    svg.innerHTML = h;
  }
  function onDown(e) {
    const hd = e.target.closest(".hd");
    if (!hd || !isRules()) return;
    e.preventDefault();
    drag = {k: hd.dataset.h};
    $("labSvg").setPointerCapture(e.pointerId);
  }
  function onMove(e) {
    if (!drag) return;
    const g = geom();
    if (!g) return;
    const rect = g.svg.getBoundingClientRect();
    const y = (e.clientY - rect.top) * (g.H / rect.height);
    const inStrip = ["tu", "tl", "bhi", "blo"].includes(drag.k);
    const t = Math.round(inStrip ? g.Ts(y) : g.T(y));
    const r = L.cur.strategy, s = sp(), c = g.c;
    const snap = v => Math.round(v / s) * s;
    if (drag.k === "tu" || drag.k === "tl") {
      let tl = g.tl, tu = g.tu;
      if (drag.k === "tu") tu = Math.max(snap(t), fdiv(c, s) * s + s); else tl = Math.min(snap(t), fdiv(c, s) * s);
      if (tu - tl < s) return;
      r.width = tu - tl;
      if (r.placement !== "follow") r.skew = Math.min(1, Math.max(0, (c - tl) / (tu - tl)));
    } else if (drag.k === "bhi") r.buffer_hi = Math.round(t - g.tu);
    else if (drag.k === "blo") r.buffer_lo = Math.round(g.tl - t);
    else {
      r[drag.k] = t;
      const pair = {cutoff_lo: "cutoff_hi", cutoff_hi: "cutoff_lo", exit_lo: "exit_hi", exit_hi: "exit_lo"}[drag.k];
      if (r[pair] != null && (drag.k.endsWith("lo") ? r[drag.k] >= r[pair] : r[drag.k] <= r[pair])) r[drag.k] = drag.k.endsWith("lo") ? r[pair] - 1 : r[pair] + 1;
    }
    L.dirty = true;
    chartRange(); syncInputs();
  }
  function onUp() { if (!drag) return; drag = null; chartRange(); changed(); }

  // ---------- comparisons ----------
  function renderPick() {
    const full = L.compare.length >= 4;
    $("labPick").innerHTML = `<button class="sec sm" id="labAddCmp" ${full && L.hold ? "disabled" : ""} title="${full ? "at most four comparisons" : "compare with another strategy"}">＋ Add a strategy</button>`;
  }
  // the add button's list: library strategies (with their APR when the strategy list has run) and holding
  function addCompareMenu(btn) {
    const r = btn.getBoundingClientRect(), full = L.compare.length >= 4;
    const items = allItems().filter(x => !x.draft && !L.compare.includes(x.id) && !(L.cur && L.cur.id === x.id));
    const row = x => { const a = aprOf(x.id); return `<div class="ci" data-add-cmp="${esc(x.id)}" data-q="${esc((x.name + " " + (x.description || "")).toLowerCase())}">${esc(x.name)}${a && a.apr != null ? `<kbd>${aprTxt(a.apr)}</kbd>` : ""}</div>`; };
    openMenu(r.left, r.bottom + 6, `<input placeholder="Compare with… type to search" spellcheck="false">
      <div class="list">${L.hold ? "" : `<div class="ci" data-add-hold>Hold 50/50<kbd>no range</kbd></div>`}
      ${full ? `<div class="mute small" style="padding:6px 9px">Four strategies already: remove one to add another.</div>` : CATS.map(c => {
        const xs = items.filter(x => (x.id.startsWith("u:") ? "Mine" : x.category) === c);
        return xs.length ? `<div class="gl">${c === "Mine" ? "Saved by you" : c}</div>` + xs.map(row).join("") : "";
      }).join("")}</div>`, m => {
      const inp = m.querySelector("input");
      inp.focus();
      inp.oninput = () => {
        const q = inp.value.trim().toLowerCase();
        m.querySelectorAll("[data-add-cmp]").forEach(x => { x.style.display = !q || x.dataset.q.includes(q) ? "" : "none"; x.classList.remove("on"); });
        const first = [...m.querySelectorAll("[data-add-cmp]")].find(x => x.style.display !== "none");
        if (first && q) first.classList.add("on");
      };
      m.onclick = e => {
        const a = e.target.closest("[data-add-cmp]"), hd = e.target.closest("[data-add-hold]");
        if (!a && !hd) return;
        closeMenu();
        if (hd) L.hold = true; else L.compare.push(a.dataset.addCmp);
        remember(); renderPick(); renderLib(); compareChanged();
      };
    });
  }
  // ---------- running ----------
  let timer = null;
  function schedule(ms = 450) {
    if (!L.pool || !L.cur) return;
    clearTimeout(timer);
    timer = setTimeout(run, ms);
  }
  function status() {
    const el = $("labStatus");
    if (!el) return;
    el.innerHTML = L.job || L.filling ? `<span class="spin"></span>${esc(L.step || "running")}…${L.pending ? " (another run queued)" : ""}`
      : L.err ? `<span class=bad>${esc(L.err)}</span>` : L.res ? `Backtested ${L.res.swaps.toLocaleString()} swaps in ${fmt(L.res.elapsed_ms / 1000, 1)} s` +
        (L.res.history_requests ? ` · downloaded ${L.res.history_requests} history page${L.res.history_requests === 1 ? "" : "s"}` : " · history from the local store") : "";
  }
  function body(s) {
    const r = JSON.parse(JSON.stringify(s));
    if (r.type === "custom") for (const k of ["cutoff_lo", "cutoff_hi", "exit_lo", "exit_hi", "guard"]) if (r[k] == null || r[k] === "") delete r[k];
    return r;
  }
  async function run() {
    if (L.job) { L.pending = true; status(); return; }
    if (L.cur.strategy.type === "graph") {
      const errs = Object.entries(graphErrors());
      if (errs.length) { L.err = `Block ${errs[0][0]}: ${errs[0][1]}`; status(); return; }
    }
    L.err = ""; L.step = "starting";
    // an edit to the strategy (saved or not) reruns only it: the comparisons on the same pool,
    // period, capital and costs keep their results, and any missing one is filled in afterwards
    const ctx = JSON.stringify([L.chain, L.pool, L.days, L.capital, L.costs]), prev = L.res && L.resCtx === ctx ? L.res : null;
    const compare = prev ? [] : L.compare.map(id => itemById(id)).filter(Boolean).map(x => ({id: x.id, name: x.name, strategy: body(x.strategy)}));
    const req = {chain_id: L.chain, pool: L.pool, days: L.days, capital_usd: L.capital, strategy: body(L.cur.strategy), costs: L.costs, compare, hold: prev ? false : L.hold};
    let id;
    try { id = (await api("/api/backtest", req)).id; }
    catch (e) { if (/already running/.test(e.message)) { setTimeout(run, 1500); return; } L.err = e.message; status(); return; }
    L.job = id; status(); renderResults();
    const key = `${L.chain}:${L.pool}`, t0 = Date.now();
    while (true) {
      await new Promise(r => setTimeout(r, Date.now() - t0 < 2000 ? 120 : 600));
      let j;
      try { j = await api("/api/backtest/" + id); } catch (e) { L.err = e.message; break; }
      if (j.state === "running") { L.step = j.step; status(); continue; }
      if (j.state === "error") { L.err = j.error; break; }
      if (key === `${L.chain}:${L.pool}`) {
        L.res = j.result; L.err = ""; L.resCtx = ctx;
        if (prev) { L.res.runs.compare = prev.runs.compare || []; if (prev.runs.hold) L.res.runs.hold = prev.runs.hold; }
        L.d = L.res.decimals; L.sym = L.res.symbols; L.spacing = L.res.tick_spacing;
        L.tick = L.res.price.length ? L.res.price[L.res.price.length - 1][1] : null;
      }
      break;
    }
    L.job = null;
    status(); renderTop(); renderOverlayChips(); chartRange(); renderResults(); syncInputs();
    if (L.pending) { L.pending = false; run(); return; }
    fillCompare();
    if (L.res && key === `${L.chain}:${L.pool}`) overfit({chain_id: req.chain_id, pool: req.pool, days: req.days, capital_usd: req.capital_usd, strategy: req.strategy, costs: req.costs, walkforward: true});
  }

  // Comparison cards rearranged by drag and drop; the strategy being edited stays on top and
  // holding at the bottom. The order is the comparison list's, so colours and the chart follow it.
  function cardDrag() {
    const box = $("labCmp");
    let from = null;
    const clear = () => box.querySelectorAll(".drop-a,.drop-b,.dragging").forEach(x => x.classList.remove("drop-a", "drop-b", "dragging"));
    box.addEventListener("dragstart", e => {
      const c = e.target.closest && e.target.closest("[data-drag]");
      if (!c) return;
      from = c.dataset.drag;
      e.dataTransfer.effectAllowed = "move";
      try { e.dataTransfer.setData("text/plain", from); } catch (x) {}
      requestAnimationFrame(() => c.classList.add("dragging"));
    });
    const target = e => {
      const c = e.target.closest && e.target.closest("[data-drag]");
      if (!c || !from) return null;
      const r = c.getBoundingClientRect();
      return {c, after: e.clientY > r.top + r.height / 2};
    };
    box.addEventListener("dragover", e => {
      if (!from) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";
      const t = target(e);
      box.querySelectorAll(".drop-a,.drop-b").forEach(x => x.classList.remove("drop-a", "drop-b"));
      if (t && t.c.dataset.drag !== from) t.c.classList.add(t.after ? "drop-a" : "drop-b");
    });
    box.addEventListener("drop", e => {
      if (!from) return;
      e.preventDefault();
      const t = target(e);
      clear();
      if (t && t.c.dataset.drag !== from) {
        const ids = L.compare.filter(id => id !== from);
        let k = ids.indexOf(t.c.dataset.drag);
        if (t.after) k++;
        ids.splice(k, 0, from);
        L.compare = ids;
        remember(); renderResults(); renderLib();
      }
      from = null;
    });
    box.addEventListener("dragend", () => { from = null; clear(); });
  }
  // Comparisons added or removed: a removed card just goes; an added strategy (or holding) is
  // backtested on its own and joins the cards, without running the others again.
  function compareChanged() {
    renderResults();
    if (!L.res) return schedule(0);
    if (!L.job) fillCompare();
  }
  async function fillCompare() {
    const res = L.res;
    if (!res || L.filling) return;
    const have = new Set((res.runs.compare || []).map(r => r.id));
    const add = L.compare.filter(id => !have.has(id)).map(id => itemById(id)).filter(Boolean);
    const hold = L.hold && !res.runs.hold;
    if (!add.length && !hold) return;
    L.filling = true;
    L.step = `backtesting ${add.length ? add.map(x => x.name).join(", ") : "holding"}`;
    const req = {chain_id: L.chain, pool: L.pool, days: L.days, capital_usd: L.capital, strategy: body(L.cur.strategy), costs: L.costs,
      compare: add.map(x => ({id: x.id, name: x.name, strategy: body(x.strategy)})), hold, main: false};
    status();
    try {
      let id;
      for (;;) {
        try { id = (await api("/api/backtest", req)).id; break; }
        catch (e) { if (!/already running/.test(e.message)) throw e; await new Promise(r => setTimeout(r, 1200)); }
      }
      for (;;) {
        await new Promise(r => setTimeout(r, 250));
        const j = await api("/api/backtest/" + id);
        if (j.state === "running") continue;
        if (j.state === "error") throw new Error(j.error);
        if (L.res !== res) break; // a full run replaced the results meanwhile
        const got = j.result.runs;
        res.runs.compare = (res.runs.compare || []).filter(r => !(got.compare || []).some(g => g.id === r.id)).concat(got.compare || []);
        if (got.hold) res.runs.hold = got.hold;
        break;
      }
    } catch (e) {
      // taken off the list again, or its card would wait for ever
      toast("Comparison: " + e.message, true);
      L.compare = L.compare.filter(id => !add.some(x => x.id === id));
      if (hold) L.hold = false;
      remember(); renderPick(); renderLib();
    }
    L.filling = false;
    L.step = "";
    status(); renderResults();
    if (L.res === res) fillCompare(); // anything added while this one ran
  }

  // ---------- overfit check: the second step after a full backtest ----------
  // The strategy and 24 neighbours (settings 1/2 to 2x) on the same replay, cut into 8 time slices:
  // walk-forward efficiency, PBO, stability, consistency and a 0-100 score (overfit.rs). One check
  // runs at a time; a newer request waits for it and a result for an older strategy is dropped.
  L.of = null;
  let ofQueued = null;
  async function overfit(req) {
    const key = JSON.stringify(req);
    if (L.of && L.of.key === key && L.of.state !== "error") return; // already checked or checking
    if (L.of && L.of.state === "running") { ofQueued = req; L.of.stale = true; drawOf(); return; }
    L.of = {key, state: "running", step: "starting"};
    drawOf();
    let id;
    for (;;) {
      try { id = (await api("/api/backtest", req)).id; break; }
      catch (e) {
        if (!/already running/.test(e.message)) { L.of = {key, state: "error", error: e.message}; drawOf(); return next(); }
        L.of.step = "waiting for a free slot"; drawOf();
        await new Promise(r => setTimeout(r, 1500));
        if (ofQueued) return next();
      }
    }
    for (;;) {
      await new Promise(r => setTimeout(r, 700));
      let j;
      try { j = await api("/api/backtest/" + id); } catch (e) { L.of = {key, state: "error", error: e.message}; break; }
      if (j.state === "running") { if (!ofQueued) { L.of.step = j.step; drawOf(); } continue; }
      L.of = j.state === "error" ? {key, state: "error", error: j.error} : {key, state: "done", r: j.result.walkforward};
      break;
    }
    if (!ofQueued) drawOf();
    next();
    function next() { const q = ofQueued; ofQueued = null; if (q) { L.of = null; overfit(q); } }
  }
  // the check is for the strategy on screen: the request it would send now
  function ofCurrent() {
    if (!L.of || !L.cur || !L.pool) return false;
    try { const k = JSON.parse(L.of.key); return k.pool === L.pool && k.days === L.days && JSON.stringify(k.strategy) === JSON.stringify(body(L.cur.strategy)); } catch (e) { return false; }
  }
  function drawOf() { const el = $("labOf"); if (el) el.innerHTML = overfitHtml(); }
  const ofPct = x => x == null ? "–" : fmt(x * 100, 0) + "%";
  function overfitHtml() {
    const o = L.of, head = badge => `<div class="oh">Overfit check${badge}</div>`;
    if (!o || !ofCurrent()) return head("") + `<div class="foot">Runs after the backtest: the same strategy with its settings nudged, period cut into slices.</div>`;
    if (o.state === "running") return head(`<span class="ofs none"><span class="spin"></span><span>${esc(o.step || "running")}</span></span>`) +
      `<div class="g">${["walk-forward efficiency", "overfit probability (PBO)", "neighbours holding up", "slices beating hold", "better than neighbours", "neighbours' median"].map((l, i) => `<div>${l}<b><span class="skl" style="--w:${[40, 34, 46, 52, 38, 44][i]}%"></span></b></div>`).join("")}</div>
      <div class="bars">${[38, 70, 22, 55, 80, 30, 62, 45].map(h => `<span class="skl" style="flex:1;height:${h * .34}px;width:auto"></span>`).join("")}</div>
      <div class="foot">About 25 extra backtests on the same swaps, four at a time.</div>`;
    if (o.state === "error") return head(`<span class="ofs none"><span>not available</span></span>`) + `<div class="foot bad">${esc(o.error)}</div>`;
    const r = o.r, cls = r.score == null ? "none" : r.score >= 70 ? "good" : r.score >= 45 ? "mid" : "badv";
    const sl = r.strategy_slices || [], mx = Math.max(1e-9, ...sl.map(Math.abs));
    const pos = sl.filter(x => x > 0).length, sliceDays = (r.span_days || r.days) / (r.slices || 8);
    const g = (label, val, help, t) => `<div data-help="${help}" title="${esc(t)}">${label}<b>${val}</b></div>`;
    return head(`<span class="ofs ${cls}" data-tip="overfit" data-help="lab-overfit"><b>${r.score == null ? "–" : r.score}</b><span>${r.score == null ? esc(r.verdict) : "/ 100 · " + esc(r.verdict)}</span></span>`) +
      `<div class="g">${g("walk-forward efficiency", r.wfe == null ? "–" : fmt(r.wfe, 2), "lab-overfit", "out-of-sample return of the in-sample winner ÷ its in-sample return (1 = held up)")}
        ${g("overfit probability (PBO)", ofPct(r.pbo), "lab-overfit", "how often the best settings on half the slices land in the bottom half on the other half")}
        ${g("neighbours holding up", ofPct(r.stability), "lab-overfit", "share of nudged settings keeping at least half of this strategy's edge over holding")}
        ${g("slices beating hold", `${pos} of ${sl.length}`, "lab-overfit", "time slices in which this strategy beat holding")}
        ${g("better than neighbours", ofPct(r.rank), "lab-overfit", "share of the nudged variants this strategy beats over the whole period (very high can mean a lucky peak)")}
        ${g("neighbours' median", r.neighbours_median == null ? "–" : fmt(r.neighbours_median, 1) + "%", "lab-overfit", "median edge over holding of the nudged variants, % a year (this strategy: " + fmt(r.strategy, 1) + "%)")}</div>
      <div class="bars" title="this strategy's return over holding in each slice, oldest first">${sl.map(x => `<i class="${x >= 0 ? "bp" : "bn"}" style="--h:${(Math.abs(x) / mx * 50).toFixed(1)}%" title="${fmt(x, 1)}% a year"></i>`).join("")}</div>
      <div class="foot">${r.variants} runs · ${r.slices} slices of ${fmt(sliceDays, sliceDays < 2 ? 1 : 0)} day${sliceDays === 1 ? "" : "s"} · ${fmt(r.elapsed_ms / 1000, 1)} s</div>`;
  }
  // the score's tooltip
  const TIP = `<h4>What overfitting is</h4>
    <p>A backtest can look good because the settings happen to fit the quirks of this stretch of history, not because the idea works. Tuned that way, a strategy often does worse live than in the backtest.</p>
    <h4>How it is checked</h4>
    <p>The strategy is run again with every setting nudged between half and double (24 neighbours), and the period is cut into 8 time slices. Each run's return over holding is measured per slice.</p>
    <p><b>Walk-forward efficiency</b>: pick the best settings on the slices so far, then measure them on the next slice. 1 means they held up; 0 or less means the edge vanished out of sample.</p>
    <p><b>PBO</b> (probability of backtest overfitting): over all 70 ways to split the slices in two halves, how often the best settings of one half fall in the bottom half of the other.</p>
    <p><b>Stability</b>: share of neighbours that keep at least half the edge. <b>Consistency</b>: share of slices that beat holding.</p>
    <h4>The score</h4>
    <p><code>35% × (1 − PBO) + 25% × efficiency + 25% × stability + 15% × consistency</code>, 0 to 100. 70 and above: robust; 45 to 70: some risk; below 45: likely overfit. Short periods give noisy slices: a week tells less than three months.</p>`;
  function tipEvents() {
    let tip = $("labTip");
    if (!tip) { tip = document.createElement("div"); tip.id = "labTip"; document.body.appendChild(tip); }
    const hide = () => tip.classList.remove("on");
    document.addEventListener("mouseover", e => {
      const t = e.target.closest && e.target.closest("[data-tip]");
      if (!t) return hide();
      tip.innerHTML = TIP;
      const r = t.getBoundingClientRect(), w = Math.min(330, innerWidth - 24);
      tip.style.width = w + "px";
      const h = tip.offsetHeight;
      tip.style.left = Math.max(12, Math.min(innerWidth - w - 12, r.right - w)) + "px";
      tip.style.top = (r.bottom + 8 + h < innerHeight ? r.bottom + 8 : Math.max(12, r.top - 8 - h)) + "px";
      tip.classList.add("on");
    });
    window.addEventListener("scroll", hide, true);
  }

  // ---------- results ----------
  // a card still being backtested: its name, and shimmering bars where the figures will be
  function skelCard(name, sub, col, main, x) {
    const g = ["vs hold", "fees", "emissions", "costs", "IL", "moves", "in range", "end value"].map((l, i) => `<div>${l}<b><span class="skl" style="--w:${[72, 58, 64, 50, 46, 30, 52, 80][i]}%"></span></b></div>`).join("");
    return `<div class="rc2 ld ${main ? "main" : ""}"><div class="t"><i style="background:${col}"></i><span class="n">${esc(name)}<br><span class="sub">${esc(sub)}</span></span>${x || ""}
      <span><div class="big"><span class="skl"></span></div><div class="apr"><span class="skl"></span></div></span></div><div class="g">${g}</div>
      ${main ? `<div class="of" id="labOf">${overfitHtml()}</div>` : ""}</div>`;
  }
  function renderResults() {
    const res = L.res, cfg = cfgOf();
    if (!res) {
      $("labCmp").innerHTML = !L.pool ? `<div class="empty">Choose a pool.</div>` : !L.cur ? `<div class="empty">Waiting for the first backtest.</div>`
        : skelCard(L.cur.name, "being edited", RUNCOL[0], true) + L.compare.map(id => itemById(id)).filter(Boolean).map((x, i) => skelCard(x.name, typeLabel(x.strategy), RUNCOL[i + 1])).join("")
          + (L.hold ? skelCard("Hold 50/50", "no range, no moves", "var(--mute)") : "");
      $("labEq").innerHTML = ""; $("labMeta").textContent = ""; $("labNotes").textContent = ""; return;
    }
    const sym = res.numeraire_symbol;
    // the strategy being edited first, then the comparisons in the order you put them (drag to
    // rearrange); one still being backtested is a skeleton in its place
    const all = [{r: res.runs.main, name: L.cur ? L.cur.name : "This strategy", sub: "being edited", col: RUNCOL[0], main: true}]
      .concat(L.compare.map((id, i) => {
        const r = (res.runs.compare || []).find(x => x.id === id), x = itemById(id);
        if (!r && !x) return null;
        return {r, skel: !r, id, name: r ? r.name : x.name, sub: typeLabel((x || {strategy: {}}).strategy), col: RUNCOL[i + 1]};
      }).filter(Boolean));
    if (L.hold) all.push({r: res.runs.hold, skel: !res.runs.hold, name: "Hold 50/50", sub: "no range, no moves", col: "var(--mute)", hold: true});
    const cols = all.filter(c => !c.skel);
    // the replay counts in the pool's numeraire; the cards show dollars at its price today
    const U = res.numeraire_usd > 0 ? res.numeraire_usd : 1, dec = 2;
    const sg = x => x == null ? "–" : `<span class="${x >= 0 ? "pos" : "neg"}">${x >= 0 ? "+" : "−"}$${fmt(Math.abs(x * U), dec)}</span>`;
    // the best of each figure across the runs is green
    const best = f => { const xs = cols.map(f).filter(x => x != null); return xs.length > 1 ? Math.max(...xs) : null; };
    const B = {pnl: best(c => c.r.pnl), hodl: best(c => c.hold ? null : c.r.vs_hodl), fees: best(c => c.r.fees), emis: best(c => c.r.emissions),
      costs: best(c => -(c.r.swap_costs + c.r.gas)), il: best(c => c.hold ? null : c.r.il)};
    const cell = (label, val, key, help, txt) => `<div data-help="${help}">${label}<b class="${key && val != null && val === B[key] ? "best" : ""}">${txt ?? sg(val)}</b></div>`;
    const xb = attr => `<span class="cx" ${attr} title="remove from the comparison">×</span>`;
    const dragAttr = c => c.id ? `draggable="true" data-drag="${esc(c.id)}"` : "";
    const grip = c => c.id ? `<span class="grip" title="drag to rearrange">⋮⋮</span>` : "";
    $("labCmp").innerHTML = all.map(c => c.skel ? skelCard(c.name, c.sub, c.col, false, xb(c.hold ? "data-unhold" : `data-unpick="${esc(c.id)}"`)).replace('<div class="rc2 ld ', `<div ${dragAttr(c)} class="rc2 ld `).replace('<div class="t">', `<div class="t">${grip(c)}`)
      : c.main && L.job ? skelCard(c.name, "backtesting your changes…", c.col, true) : `<div class="rc2 ${c.main ? "main" : ""}" ${dragAttr(c)} title="${esc(c.r.label)}">
        <div class="t">${grip(c)}<i style="background:${c.col}"></i><span class="n">${esc(c.name)}<br><span class="sub">${esc(c.sub)}</span></span>
          ${c.main ? "" : `<span class="cx" ${c.hold ? "data-unhold" : `data-unpick="${esc(c.r.id)}"`} title="remove from the comparison">×</span>`}
          <span data-help="lab-r-pnl"><div class="big ${c.r.pnl === B.pnl ? "best" : ""}">${sg(c.r.pnl)}</div><div class="apr">${fmt(c.r.pnl / res.capital * 100 * 365 / (res.span_days || res.days), 2)}% a year</div></span></div>
        <div class="g">${cell("vs hold", c.hold ? null : c.r.vs_hodl, "hodl", "lab-r-hodl")}${cell("fees", c.r.fees, "fees", "lab-r-fees")}${cell("emissions", c.r.emissions, "emis", "lab-r-emis")}${cell("costs", -(c.r.swap_costs + c.r.gas), "costs", "lab-r-costs")}
          ${cell("IL", c.hold ? null : c.r.il, "il", "lab-r-il")}${cell("moves", null, null, "lab-r-moves", String(c.r.actions))}${cell("in range", null, null, "lab-r-tir", c.hold ? "–" : fmt(c.r.time_in_range * 100, 1) + "%")}${cell("end value", null, null, "lab-r-value", "$" + fmt(c.r.value * U, dec))}</div>
        ${c.main ? `<div class="of" id="labOf">${overfitHtml()}</div>` : ""}</div>`).join("") +
      `<div class="mute small" style="margin:-2px 0 8px">Amounts in USD${sym && !/^(USDC|USDT|USDC\.E|USDBC|DAI)$/i.test(sym) ? ` (counted in ${esc(sym)} at $${fmt(U, U > 100 ? 0 : 4)})` : ""}. Green is the best of the runs.</div>`;
    $("labMeta").innerHTML = `${new Date(res.from * 1000).toLocaleDateString()} – ${new Date(res.to * 1000).toLocaleDateString()} · ${res.swaps.toLocaleString()} swaps · capital $${fmt(res.capital_usd ?? res.capital * U, 0)}${res.entry_cost > 0 ? ` (buying ${esc(sym)} with it cost $${fmt(res.entry_cost * U, 2)})` : ""}`;
    const short = res.span_days && res.span_days < res.days - 1;
    $("labNotes").textContent = (short ? `The pool's history covers only ${fmt(res.span_days, 0)} of the ${res.days} days asked for; yearly rates use the days covered. ` : "") + res.notes.join(" ") + (res.runs.main.guard_bars ? ` The strategy held still (guard or hold) for ${fmt(res.runs.main.guard_bars * 5 / 60, 1)} hours.` : "")
      + elasticityNote(res.elasticity)
      + (res.runs.main.flow_capped > 0.005 * Math.max(res.runs.main.fees, 1e-9) ? ` This capital is large for the pool: it would draw more than twice the real flow, and the cap held back $${fmt(res.runs.main.flow_capped * (res.numeraire_usd || 1), 2)} of fees.` : "");
    chartEq(cols);
    const ev = res.runs.main.events || [];
    $("labMoves").innerHTML = ev.length ? ev.map(e => `<div><span class="mono">${new Date(e.ts * 1000).toLocaleString(undefined, {month: "short", day: "numeric", hour: "2-digit", minute: "2-digit"})}</span>
      <span class="${e.kind === "exit" ? "bad" : "ok"}">${e.kind}</span><span>${e.kind === "move" ? `<span class=mono>${pfmt(e.tl)} – ${pfmt(e.tu)}</span>${e.ranges > 1 ? ` (${e.ranges} ranges)` : ""} · ` : ""}<span class=mute>${esc(e.reason)}</span></span></div>`).join("")
      : `<span class="mute">No moves in the period.</span>`;
  }
  // how far the pool's flow follows its depth, as measured (elasticity.rs) and as used
  function elasticityNote(e) {
    if (!e) return "";
    const b = fmt(e.beta, 2);
    if (e.method === "set") return ` Flow follows depth by β = ${b} (set).`;
    if (e.method === "prior") return ` Flow follows depth by β = ${b}, a prior: ${e.why}.`;
    const how = e.method === "instrument" ? "with your positions' share of the pool as the instrument" : "by regression (your positions here are too small or too steady to act as an instrument)";
    return ` Flow follows depth by β = ${b}: the last ${e.days} days measure ${fmt(e.raw, 2)} ± ${fmt(e.se, 2)} over ${e.hours} hours ${how}, weighted ${fmt(e.weight_of_data * 100, 0)}% against a prior of 0.5.`;
  }
  let lastCols = null;
  function chartEq(cols) {
    cols = cols || lastCols; lastCols = cols;
    const res = L.res, svg = $("labEq");
    if (!res || !cols) return;
    const all = cols.flatMap(c => c.r.series.map(p => p[1])).concat(res.runs.main.series.map(p => p[2]));
    const lo = Math.min(...all), hi = Math.max(...all), span = Math.max(hi - lo, Math.abs(hi) * 1e-4, 1e-9);
    const H = 150, y0 = lo - span * .1, dy = span * 1.2;
    const s = res.runs.main.series, x0 = s[0][0], x1 = s[s.length - 1][0];
    const X = t => (t - x0) / (x1 - x0 || 1) * 800, Y = val => H - 6 - (val - y0) / dy * (H - 12);
    const line = (pts, i, col, dash) => `<polyline fill="none" stroke="${col}" stroke-width="${dash ? 1.2 : 2}" ${dash ? 'stroke-dasharray="5 4"' : ""} vector-effect="non-scaling-stroke" points="${pts.map(p => `${X(p[0]).toFixed(1)},${Y(p[i]).toFixed(1)}`).join(" ")}"/>`;
    svg.innerHTML = [0, .5, 1].map(f => `<line x1="0" x2="800" y1="${6 + f * (H - 12)}" y2="${6 + f * (H - 12)}" stroke="var(--line)" vector-effect="non-scaling-stroke"/>`).join("") +
      line(res.runs.main.series, 2, "var(--fg2)", true) + cols.slice().reverse().map(c => line(c.r.series, 1, c.col)).join("");
    $("labEqAxis").innerHTML = `Value over time, $${fmt(lo * (res.numeraire_usd || 1), 2)} to $${fmt(hi * (res.numeraire_usd || 1), 2)} · <span style="color:var(--fg2)">┅</span> holding the first deposit`;
  }

  // ---------- re-arm ----------
  async function loadCustomState() {
    const box = $("labRearm");
    box.innerHTML = "";
    const cfg = cfgOf();
    if (!cfg || !cfg.strategy || !["custom", "graph"].includes(cfg.strategy.type)) return;
    try {
      const st = await api(`/api/custom/state/${L.chain}/${L.pool}`);
      const s = S.settings.execute ? st.live : st.dry;
      if (s.exited) box.innerHTML = `<div class="rearm" data-help="lab-rearm"><span>● ${esc(cfg.name)} exited and stays out until its re-entry condition holds.</span><span class="sp"></span><button class="sm" id="labRearmBtn">Re-arm</button></div>`;
      const b = $("labRearmBtn");
      if (b) b.onclick = async () => { try { await api("/api/custom/rearm", {chain_id: L.chain, pool: L.pool}); toast("Re-armed: the next cycle deposits again."); loadCustomState(); } catch (e) { toast(e.message, true); } };
    } catch (e) { /* no state yet */ }
  }

  // ---------- the library for the Pools view's strategy picker ----------
  // a library strategy as a pool's strategy: simple rules in spacings become this pool's ticks, a
  // ladder gets the app's defaults for the pool (as adding the pool would), a node strategy its name
  function forPoolCfg(st, spacing, pegged, name) {
    const s = JSON.parse(JSON.stringify(st)), spc = Math.max(1, spacing || 1);
    if (s.type === "custom") {
      if (s.unit === "spacings") { for (const k of ["width", "offset_up", "offset_down", "buffer_lo", "buffer_hi"]) if (s[k] != null) s[k] = Math.round(s[k] * spc); delete s.unit; }
      s.width = Math.max(s.width ?? spc, spc);
      for (const k of ["cutoff_lo", "cutoff_hi", "exit_lo", "exit_hi", "guard"]) if (s[k] == null || s[k] === "") delete s[k];
      return s;
    }
    if (s.type === "ladder") {
      const d = Object.assign({}, (S && S.ladder_defaults) || {}), own = Object.assign({}, s);
      delete own.type;
      return Object.assign({type: "ladder"}, d, pegged ? {span: Math.max(d.span || 3, 3 * spc)} : {span: 60 * spc, guard: null}, own);
    }
    if (s.type === "graph") s.name = name || s.name || "";
    return s;
  }
  const css2 = document.createElement("style");
  css2.textContent = `.sgh{font:650 10.5px var(--sans);letter-spacing:.8px;text-transform:uppercase;color:var(--mute);margin:12px 0 6px}
    .sblocks{display:grid;grid-template-columns:repeat(auto-fill,minmax(250px,1fr));gap:4px 16px}.sblock .fields{margin:0}`;
  document.head.appendChild(css2);
  window.LABLIB = {
    ready: () => !!L.lib,
    items: () => allItems(),
    item: id => itemById(id),
    catalog: () => (L.lib && L.lib.catalog) || [],
    kindName: k => (K[k] || [k])[0],
    paramName: k => PNAME[k] || k,
    forPool: forPoolCfg,
    reload: () => loadLib(),
  };

  // ---------- boot ----------
  shell();
  const svg = $("labSvg");
  svg.addEventListener("pointerdown", onDown);
  svg.addEventListener("pointermove", onMove);
  svg.addEventListener("pointerup", onUp);
  svg.addEventListener("pointercancel", onUp);
  let booted = false;
  setInterval(async () => {
    if (!S) return;
    if (!booted) {
      booted = true;
      let saved = null;
      try { saved = JSON.parse(localStorage.getItem("alm.lab") || "null"); } catch (e) {}
      if (saved) { L.days = saved.days || 30; if (Array.isArray(saved.compare)) L.compare = saved.compare; if (saved.hold != null) L.hold = saved.hold; }
      await loadLib();
      if (saved && saved.cur) { const x = itemById(saved.cur); if (x) setCur(x); }
      if (saved && saved.pool) { L.chain = saved.chain; L.pool = saved.pool; }
      renderTop(); renderLib(); renderEd(); chartRange(); renderResults();
      if (tab === "lab" && L.pool) select(L.chain, L.pool);
      return;
    }
    if (tab === "lab") {
      if (!$("labPool").classList.contains("open")) renderTop();
      if (L.cur && L.cur.strategy.type === "graph" && !L.cur.view && $("neCv")) fit();
      if (L.pool && !L.res && !L.job && !L.err) select(L.chain, L.pool);
      if (!MARKET) loadMarket();
    }
  }, 1000);
})();
