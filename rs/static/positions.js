// Positions & history (Pools tab, My pools): measured APR per position (positions.rs) and a timeline of what happened to them.
// Uses the page's helpers: $, api, fmt, usd, esc, toast, COLORS, chainOf, cc.
(function () {
  // cache: the last answer per period, read in the background (preload) so opening the tab or
  // switching the period shows data at once; a page with nothing yet shows a skeleton of itself
  const P = {data: null, days: 7, closed: false, focus: null, kinds: null, cache: {}, at: {}, pending: {}, retry: null};
  const FRESH = 60000, BACKGROUND = 120000;
  const KINDS = {
    mint: ["Added", "var(--ok)", "+"], burn: ["Removed", "var(--bad)", "−"], claim: ["Claimed", "var(--warn)", "◆"],
    swap: ["Swap", "var(--info)", "⇄"], tx: ["Transaction", "var(--mute)", "·"], decision: ["Rebalance", "var(--c4)", "↻"],
    range_exit: ["Left range", "var(--bad)", "↗"], range_enter: ["Back in range", "var(--ok)", "↘"],
    error: ["Error", "var(--bad)", "!"], park: ["Parked", "var(--c6)", "P"], park_handover: ["Handed over", "var(--c6)", "H"],
    capital: ["Capital", "var(--c6)", "$"], solid_lp: ["SOLID LP", "var(--warn)", "S"]};
  const kindOf = k => KINDS[k] || [k, "var(--mute)", "·"];
  const posKey = p => p.id + ":" + p.episode;
  // ranges as prices or as ticks: one switch for the table, the legend and the chart's tooltip, kept by
  // the app (config/ui.json) so it survives a restart and a change of port; browser storage only paints
  // the first frame before the app answers
  let UNITS = (() => { try { return localStorage.getItem("alm.units") === "tick" ? "tick" : "price"; } catch (e) { return "price"; } })();
  const keepUnits = () => { try { localStorage.setItem("alm.units", UNITS); } catch (e) {} };
  let prefsRead = false;
  function readPrefs() {
    if (prefsRead) return;
    prefsRead = true;
    api("/api/ui_prefs").then(pr => { if (pr.range_units && pr.range_units !== UNITS) { UNITS = pr.range_units; keepUnits(); redraw(); } }).catch(() => { prefsRead = false; });
  }
  function setUnits(u) {
    if (u === UNITS) return;
    UNITS = u; keepUnits(); redraw();
    api("/api/ui_prefs", {range_units: u}).catch(e => toast("Range units not saved: " + e.message, true));
  }
  function redraw() { if (P.data && $("posTable")) { const ps = shown(); chart(ps); table(ps); } }
  // enough decimals that a range's two ends differ (1.0012 – 1.0013 shows as 1.00120 – 1.00130)
  const pxDigits = (lo, hi) => Math.min(8, Math.max(2, Math.ceil(-Math.log10(Math.abs(hi - lo) || 1e-4)) + 1));
  const tickFmt = x => String(Math.round(x));
  const rangeTxt = p => UNITS === "tick" || !p.prices ? `[${p.tl}, ${p.tu})`
    : `${fmt(p.prices[0], pxDigits(...p.prices))} – ${fmt(p.prices[1], pxDigits(...p.prices))}`;
  const tickTxt = rangeTxt;
  // the pool's current tick: a whole number, the tick the price is in (as the pool reports it; else
  // from the price, rounded down as the pool does)
  const tickNow = p => p.tick_now != null ? p.tick_now
    : p.price_now != null && p.prices && p.prices[0] > 0 ? p.tl + Math.floor(Math.log(p.price_now / p.prices[0]) / Math.log(1.0001) + 1e-9) : null;
  const pname = p => p.name || (p.symbols ? p.symbols.join("/") : (p.pool || "").slice(0, 10));
  const when = ts => new Date(ts * 1000).toLocaleString(undefined, {month: "short", day: "numeric", hour: "2-digit", minute: "2-digit"});
  const dayOf = ts => new Date(ts * 1000).toLocaleDateString(undefined, {weekday: "short", month: "short", day: "numeric"});
  const dur = s => s >= 86400 ? fmt(s / 86400, 1) + " d" : s >= 3600 ? fmt(s / 3600, 1) + " h" : Math.max(1, Math.round(s / 60)) + " min";
  const pct = (x, d = 2) => x == null || isNaN(x) ? "–" : fmt(x, x >= 100 ? 0 : d) + "%";
  const explorer = (cid, tx) => { const c = (P.data.chains || []).find(c => c.chain_id === cid); return c && c.explorer && tx ? `${c.explorer.replace(/\/$/, "")}/tx/${tx}` : null; };

  const onTab = () => typeof tab !== "undefined" && tab === "pools" && (typeof POOLVIEW === "undefined" || POOLVIEW === "mine") && !(typeof isAll === "function" && isAll());
  // a switch of wallet starts a new generation: answers for the wallet shown before are dropped
  let GEN = 0;

  // one request per period at a time; callers share it
  function fetchDays(days) {
    const g = GEN;
    if (!P.pending[days]) P.pending[days] = api("/api/positions/" + days)
      .then(d => { if (g !== GEN) throw new Error("wallet switched"); P.cache[days] = d; P.at[days] = Date.now(); kpis(d); return d; })
      .finally(() => { delete P.pending[days]; busy(); });
    busy();
    return P.pending[days];
  }
  function busy() {
    const b = $("posReload");
    if (!b) return;
    const on = !!P.pending[P.days];
    b.disabled = on; b.classList.toggle("busy", on); b.textContent = on ? "Updating…" : "Refresh";
  }
  function put(d) { P.data = d; if (onTab()) render(); }

  // the tab opened, the period changed or Refresh: what is cached shows at once, a newer answer replaces it
  async function load(force) {
    const days = P.days, have = P.cache[days];
    clearTimeout(P.retry);
    readPrefs();
    if (have) { if (P.shown !== have || !$("posTable")) put(have); if (!force && Date.now() - P.at[days] < FRESH) return; }
    else skeleton();
    try { const d = await fetchDays(days); if (P.days === days) put(d); }
    catch (e) {
      if (P.days !== days || /wallet switched/.test(e.message)) return;
      // the server is still reading them (first start): keep the skeleton and ask again shortly
      if (/still loading/i.test(e.message)) { P.retry = setTimeout(() => load(true), 3000); return; }
      if (have) toast("positions: " + e.message, true);
      else $("posTop").innerHTML = "", $("posBody").innerHTML = `<div class="card bad">${esc(e.message)} <button class="sec sm" id="posRetry" style="margin-left:8px">Try again</button></div>`, $("posRetry").onclick = () => load(true);
    }
  }

  // in the background, from page load on: the shown period every 2 minutes, the others once
  // each then and again when one is opened after going stale
  async function preload() {
    if (!document.hidden) {
      const days = P.days;
      try { const d = await fetchDays(days); if (P.days === days) put(d); } catch (e) {}
      for (const n of [7, 1, 30, 90]) if (!P.cache[n]) { try { await fetchDays(n); } catch (e) { break; } }
    }
    setTimeout(preload, BACKGROUND);
  }
  setTimeout(preload, 1500);

  // a skeleton of the page itself: KPI cards, the chart, table rows and the timeline
  function skeleton() {
    const kpi = (m, i) => `<div class="card kpi${m ? " main" : ""}"><div class="l">${skl(["46%", "52%", "38%", "30%"][i])}</div>
      <div class="v">${skl(["58%", "44%", "50%", "36%"][i], "26px")}</div><div class="s">${skl(["82%", "60%", "72%", "54%"][i])}</div></div>`;
    const bars = Array.from({length: 36}, (_, i) => `<span class="skl" style="height:${Math.round(28 + 34 * (1 + Math.sin(i / 3.1)) + 18 * Math.sin(i * 1.7))}%;animation-delay:${(i % 12) * 60}ms"></span>`).join("");
    const ev = i => `<div class="pos-ev"><span class="pos-time">${skl("38px")}</span><span class="skl" style="width:18px;height:18px;border-radius:50%"></span><span class="pos-txt">${skl(40 + (i * 23) % 45 + "%")}</span><span></span></div>`;
    $("posBody").innerHTML = `
      <div class="card"><div class="row" style="align-items:center;margin-bottom:12px"><h2 style="margin:0">APR per position</h2><span class="sp"></span>${skl("220px", "22px")}</div>
        <div class="pos-skchart">${bars}</div><div class="legend">${[90, 120, 104].map(w => skl(w + "px")).join("")}</div></div>
      <div class="card"><h2>Timeline</h2><div class="pos-tl"><div class="pos-day">${skl("120px")}</div>${[0, 1, 2, 3, 4].map(ev).join("")}</div></div>`;
    $("posTop").innerHTML = `<div class="card"><div class="row" style="align-items:center;margin-bottom:10px"><h2 style="margin:0">Positions</h2><span class="sp"></span>${skl("132px", "28px")}</div><div class="scroll"><table><tbody>${skRows(8, 4)}</tbody></table></div></div>`;
  }

  // the overview's fee cards: live positions only, so any period's answer serves (the newest wins)
  function kpiSkeleton() {
    const box = $("ovFees");
    if (!box) return;
    box.innerHTML = [1, 0, 0, 0].map((m, i) => `<div class="card kpi${m ? " main" : ""}"><div class="l">${skl(["46%", "52%", "38%", "30%"][i])}</div>
      <div class="v">${skl(["58%", "44%", "50%", "36%"][i], "26px")}</div><div class="s">${skl(["82%", "60%", "72%", "54%"][i])}</div></div>`).join("");
  }
  function kpis(d) {
    const box = $("ovFees");
    if (!box || !d) return;
    const live = (d.chains || []).flatMap(c => c.positions || []).filter(p => !p.closed);
    const sum = k => live.reduce((s, p) => s + (p[k] || 0), 0);
    const value = sum("usd"), inR = live.filter(p => p.in_range);
    const f24 = sum("fees_24h"), f7 = sum("fees_7d"), f30 = sum("fees_30d");
    box.innerHTML = `
        <div class="card kpi main"><div class="l">Fee APR · last 24 h</div><div class="v">${pct(value ? f24 / value * 365 * 100 : null)}</div>
          <div class="s">on ${usd(value, 0)} in ${live.length} live position${live.length === 1 ? "" : "s"} · LP share of fees (80%)</div></div>
        <div class="card kpi"><div class="l">Fee APR · last 7 days</div><div class="v">${pct(value ? f7 / value * 365 / 7 * 100 : null)}</div><div class="s">at today's position values</div></div>
        <div class="card kpi"><div class="l">Fees earned</div><div class="v">${usd(f24, 2)}</div>
          <div class="s"><span class=it>last 24 h</span><span class=it>7 d ${usd(f7, 2)}</span><span class=it>30 d ${usd(f30, 2)}</span></div></div>
        <div class="card kpi"><div class="l">In range</div><div class="v">${inR.length} / ${live.length}</div>
          <div class="s">${usd(inR.reduce((s, p) => s + (p.usd || 0), 0), 0)} earning now</div></div>`;
  }
  kpiSkeleton();

  function all() {
    const out = [];
    for (const c of P.data.chains || []) for (const p of c.positions || []) out.push({...p, chain_id: c.chain_id});
    return out;
  }
  function shown() {
    // live positions, and closed ones that earned something (the many seconds-long episodes of a re-mint are noise)
    const ps = all().filter(p => !p.closed || (P.closed && p.fees_usd > 0.001));
    ps.sort((a, b) => (a.closed - b.closed) || (b.usd || b.avg_capital_usd || 0) - (a.usd || a.avg_capital_usd || 0));
    ps.forEach((p, i) => p.color = COLORS[i % COLORS.length]);
    return ps;
  }

  function render() {
    P.shown = P.data;
    const d = P.data, ps = shown();
    const errs = (d.chains || []).filter(c => c.error).map(c => `${esc(chainOf(c.chain_id).name)}: ${esc(c.error)}`);
    const samples = Math.max(0, ...(d.chains || []).map(c => c.samples || 0));
    $("posBody").innerHTML = `
      ${errs.length ? `<div class="card bad small">${errs.join("<br>")}</div>` : ""}
      <div class="card">
        <div class="row" style="align-items:center;margin-bottom:6px"><h2 style="margin:0" data-help="pos-chart">APR per position</h2><span class="sp"></span>
          <label class="small mute" style="display:flex;gap:6px;align-items:center"><input type="checkbox" id="posClosed" ${P.closed ? "checked" : ""}> closed positions</label>
          <div class="pos-seg">${[1, 7, 30, 90].map(n => `<button class="sec sm ${P.days === n ? "on" : ""}" data-days="${n}">${n === 1 ? "24 h" : n + " d"}</button>`).join("")}</div>
          <button class="sec sm" id="posReload" title="read again from the Solidly UI">Refresh</button></div>
        <div class="pos-chart"><svg id="posSvg" viewBox="0 0 900 260"></svg><div id="posTip" class="pos-tip hide"></div></div>
        <div class="legend" id="posLegend"></div>
        <p class="small mute" style="margin:8px 0 0">Solid lines: the APR the Solidly UI measured for each position between our samples (every 15 minutes, ${samples} so far in this period).
          Dashed steps: the earlier history from its trailing fee sums (last hour, 1–2 h, 2–4 h … 7–30 days). Dotted: SOLID rewards per pool. Marks along the bottom are the events below.</p>
      </div>
      <div class="card"><div class="row" style="align-items:center"><h2 style="margin:0">Timeline</h2><span class="sp"></span><div id="posKinds" class="pos-kinds"></div></div>
        <div id="posEvents" class="pos-tl"></div></div>`;
    $("posTop").innerHTML = `<div class="card"><div class="row" style="align-items:center;margin-bottom:10px"><h2 data-help="pos-table" style="margin:0">Positions</h2><span class="sp"></span><div id="posUnits"></div></div>
      <div class="scroll"><table id="posTable"></table></div></div>`;
    chart(ps);
    table(ps);
    timeline();
    document.querySelectorAll("#posBody [data-days]").forEach(b => b.onclick = () => { P.days = +b.dataset.days; load(); });
    $("posClosed").onchange = e => { P.closed = e.target.checked; render(); };
    $("posReload").onclick = () => load(true);
    busy();
  }

  function chart(ps) {
    // the drawing is as wide as the box, so text keeps its shape at any width
    const svg = $("posSvg"), W = Math.max(300, Math.round(svg.getBoundingClientRect().width) || 900), H = 260, now = P.data.now, x0 = now - P.days * 86400;
    svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
    const lines = ps.map(p => ({p, pts: (p.series || []).map(([t, a]) => [t, a]), steps: p.trailing || []}));
    const rw = [];
    for (const c of P.data.chains || []) for (const [pool, pts] of Object.entries(c.rewards || {})) {
      if (!pts.length) continue;
      const any = ps.find(p => p.pool === pool && p.chain_id === c.chain_id);
      rw.push({name: (any && pname(any)) || pool.slice(0, 10), color: any ? any.color : "var(--mute)", pts});
    }
    const vals = [...lines.flatMap(l => [...l.pts.map(x => x[1]), ...l.steps.map(s => s[2])]), ...rw.flatMap(r => r.pts.map(x => x[1]))].filter(v => isFinite(v) && v >= 0).sort((a, b) => a - b);
    // the y axis to the 97th percentile: one spike in a quiet hour should not flatten every other line
    const top = vals.length ? Math.max(1, vals[Math.min(vals.length - 1, Math.floor(vals.length * .97))] * 1.15) : 10;
    const X = t => (t - x0) / (now - x0) * W, Y = v => H - 26 - Math.min(v, top) / top * (H - 40);
    const grid = [0, .25, .5, .75, 1].map(f => { const v = top * f; return `<line x1="0" x2="${W}" y1="${Y(v)}" y2="${Y(v)}" stroke="var(--line)" vector-effect="non-scaling-stroke"/>
      <text x="4" y="${Y(v) - 3}" class="pos-ax">${fmt(v, v < 10 ? 1 : 0)}%</text>`; }).join("");
    const ticks = []; const nt = W < 520 ? 3 : 6; for (let i = 0; i <= nt; i++) { const t = x0 + (now - x0) * i / nt; ticks.push(`<text x="${Math.min(W - 40, Math.max(2, X(t) - 20))}" y="${H - 4}" class="pos-ax">${P.days <= 1 ? new Date(t * 1000).toLocaleTimeString(undefined, {hour: "2-digit", minute: "2-digit"}) : new Date(t * 1000).toLocaleDateString(undefined, {month: "short", day: "numeric"})}</text>`); }
    const path = (pts, color, dash, w = 2) => pts.length ? `<polyline points="${pts.map(([t, v]) => `${X(Math.max(t, x0)).toFixed(1)},${Y(v).toFixed(1)}`).join(" ")}" fill="none" stroke="${color}" stroke-width="${w}" ${dash ? `stroke-dasharray="${dash}"` : ""} vector-effect="non-scaling-stroke" stroke-linejoin="round"/>` : "";
    const dim = l => P.focus && P.focus !== posKey(l.p) ? ' opacity=".18"' : "";
    const body = lines.map(l => `<g${dim(l)}>${path(l.steps.flatMap(([a, b, v]) => [[a, v], [b, v]]), l.p.color, "5 4", 1.5)}${path(l.pts, l.p.color, "", 2.2)}
      ${l.pts.length === 1 ? `<circle cx="${X(l.pts[0][0])}" cy="${Y(l.pts[0][1])}" r="3" fill="${l.p.color}"/>` : ""}</g>`).join("")
      + rw.map(r => path(r.pts, r.color, "1 4", 2)).join("");
    const evs = (P.data.events || []).filter(e => e.ts >= x0 && ["mint", "burn", "claim", "decision", "range_exit", "range_enter", "error"].includes(e.kind));
    const marks = evs.map(e => `<circle cx="${X(e.ts).toFixed(1)}" cy="${H - 18}" r="3.2" fill="${kindOf(e.kind)[1]}" opacity=".85"><title>${esc(kindOf(e.kind)[0])} · ${esc(e.name || "")} · ${esc(when(e.ts))}</title></circle>`).join("");
    const empty = !vals.length ? `<text x="${W / 2}" y="${H / 2}" text-anchor="middle" class="pos-ax" style="font-size:13px">No fees measured in this period yet: the chart fills in as samples are taken.</text>` : "";
    svg.innerHTML = grid + ticks.join("") + `<line x1="0" x2="${W}" y1="${H - 18}" y2="${H - 18}" stroke="var(--line2)" vector-effect="non-scaling-stroke"/>` + body + marks + empty + `<line id="posCross" x1="0" x2="0" y1="0" y2="${H - 22}" stroke="var(--fg2)" stroke-dasharray="2 3" vector-effect="non-scaling-stroke" opacity="0"/>`;
    $("posLegend").innerHTML = ps.map(p => `<span class="pos-leg ${P.focus === posKey(p) ? "on" : ""}" data-k="${esc(posKey(p))}"><span class="swc" style="background:${p.color}"></span>${esc(pname(p))} <span class="mono">${tickTxt(p)}</span>${p.closed ? ' <span class="mute">closed</span>' : ""}</span>`).join("")
      + rw.map(r => `<span class="mute"><span class="swc" style="background:none;border:2px dotted ${r.color}"></span>${esc(r.name)} rewards</span>`).join("");
    document.querySelectorAll("#posLegend .pos-leg").forEach(el => el.onclick = () => { P.focus = P.focus === el.dataset.k ? null : el.dataset.k; chart(shown()); table(shown()); });
    // hover: each line's value at the nearest time
    const tip = $("posTip"), cross = $("posCross");
    svg.onmousemove = ev => {
      const r = svg.getBoundingClientRect(), t = x0 + (ev.clientX - r.left) / r.width * (now - x0);
      const at = l => { let best = null; for (const [ts, v] of l.pts) if (Math.abs(ts - t) < 3 * 3600 && (!best || Math.abs(ts - t) < Math.abs(best[0] - t))) best = [ts, v];
        if (!best) for (const [a, b, v] of l.steps) if (t >= a && t <= b) best = [t, v]; return best; };
      const rows = lines.map(l => [l, at(l)]).filter(x => x[1]);
      if (!rows.length) { tip.classList.add("hide"); cross.setAttribute("opacity", 0); return; }
      cross.setAttribute("x1", X(t)); cross.setAttribute("x2", X(t)); cross.setAttribute("opacity", .6);
      tip.innerHTML = `<b>${esc(when(t))}</b>` + rows.map(([l, v]) => `<div><span class="swc" style="background:${l.p.color}"></span>${esc(pname(l.p))} ${tickTxt(l.p)} <b class="mono">${pct(v[1])}</b></div>`).join("");
      tip.classList.remove("hide");
      const left = ev.clientX - r.left;
      tip.style.left = (left > r.width - 260 ? left - 250 : left + 14) + "px";
    };
    svg.onmouseleave = () => { tip.classList.add("hide"); cross.setAttribute("opacity", 0); };
  }

  // the range against the price now, as the Solidly UI's position cards draw it: the range as a filled
  // segment on an axis wide enough to hold the price, a marker at the price, both ends and "now" below
  function rangeCell(p) {
    const tick = UNITS === "tick";
    const [lo, hi] = tick ? [p.tl, p.tu] : p.prices || [null, null];
    const cur = p.closed || lo == null ? null : tick ? tickNow(p) : p.price_now;
    // in or out by the price itself when there is one (a range is [lower, upper)), so the chip, the
    // marker and the distance always agree
    const known = cur != null && isFinite(cur), inr = known ? cur >= lo && cur < hi : p.in_range;
    const chip = p.closed ? `<span class="rchip">closed</span>` : inr == null ? "" : `<span class="rchip ${inr ? "in" : "out"}">${inr ? "In range" : "Out of range"}</span>`;
    if (lo == null) return chip;
    const dg = pxDigits(lo, hi), f = x => tick ? tickFmt(x) : fmt(x, dg);
    if (!known) return `<div class="rcell">${chip}<div class="rlab"><span>${f(lo)}</span><span class="mute">${p.closed ? "" : "price not read yet"}</span><span>${f(hi)}</span></div></div>`;
    // in ticks the price is somewhere inside tick `cur`, so its marker sits in the middle of it
    const mark = tick ? cur + 0.5 : cur;
    const out = !inr, low = Math.min(lo, mark), high = Math.max(hi, mark), span = high - low;
    const pad = span > 0 ? span * 0.12 : Math.abs(cur) * 0.12 || 1, a0 = low - pad, w = high + pad - a0;
    const at = v => Math.min(100, Math.max(0, (v - a0) / w * 100));
    const L = at(lo), R = at(hi);
    // how far outside, in the switch's units: the nearer end
    // ticks: how many ticks lie between the price's tick and the range ([lower, upper): tick `upper` is
    // the first one above it)
    const ticks = n => `${n} tick${n === 1 ? "" : "s"}`;
    const away = !out ? "" : cur >= hi ? (tick ? `${ticks(cur - hi + 1)} above` : `${fmt((cur / hi - 1) * 100, 3)}% above`)
      : (tick ? `${ticks(lo - cur)} below` : `${fmt((1 - cur / lo) * 100, 3)}% below`);
    const asOf = p.price_ts ? ` (pool price at the last cycle, ${when(p.price_ts)})` : "";
    return `<div class="rcell" title="${esc(`range ${f(lo)} – ${f(hi)}, now ${f(cur)}${asOf}`)}">
      <div class="rtop">${chip}${away ? `<span class="raway">${away}</span>` : ""}</div>
      <div class="rbar"><span class="rseg" style="left:${L}%;width:${Math.max(2, R - L)}%;--rc:${p.color}"></span><span class="rnow ${out ? "out" : ""}" style="left:${at(mark)}%"></span></div>
      <div class="rlab"><span>${f(lo)}</span><span class="rnowt ${out ? "out" : ""}">now ${f(cur)}</span><span>${f(hi)}</span></div></div>`;
  }

  function unitsSwitch() {
    const b = (u, glyph, label, tip) => `<button role="radio" aria-checked="${UNITS === u}" class="${UNITS === u ? "on" : ""}" data-u="${u}" title="${tip}"><span class="ug">${glyph}</span>${label}</button>`;
    return `<div class="useg" role="radiogroup" aria-label="Show ranges as" data-v="${UNITS}">
      ${b("price", "1.0", "Price", "ranges and the price now as prices")}${b("tick", "#", "Tick", "ranges and the price now as ticks")}</div>`;
  }

  function table(ps) {
    const now = P.data.now;
    const sw = $("posUnits");
    if (sw) {
      sw.innerHTML = unitsSwitch();
      sw.querySelectorAll("button[data-u]").forEach(b => b.onclick = () => setUnits(b.dataset.u));
    }
    const unit = p => UNITS === "tick" ? `${p.tu - p.tl} tick${p.tu - p.tl === 1 ? "" : "s"} wide` : p.symbols ? `${p.symbols[1]} per ${p.symbols[0]}` : "";
    const row = p => `<tr class="${P.focus === posKey(p) ? "pos-on" : ""}" data-k="${esc(posKey(p))}" style="cursor:pointer">
        <td style="white-space:nowrap"><span class="swc" style="background:${p.color}"></span><b>${esc(pname(p))}</b> <span class="mono">[${p.tl}, ${p.tu})</span><div class="small mute">${esc(unit(p))}</div></td>
        <td class="rtd">${rangeCell(p)}</td>
        <td class="num">${p.closed ? `<span class="mute">${usd(p.avg_capital_usd, 0)} avg</span>` : usd(p.usd, 2)}</td>
        <td class="num">${usd(p.fees_usd, p.fees_usd < 1 ? 4 : 2)}</td>
        <td class="num">${p.closed ? "–" : pct(p.apr_24h)}</td><td class="num">${p.closed ? "–" : pct(p.apr_7d)}</td><td class="num"><b>${pct(p.apr)}</b></td>
        <td class="num small">${p.start ? esc(when(p.start)) : "–"}<div class="mute">${p.start ? dur((p.end || now) - p.start) : ""}</div></td></tr>`;
    $("posTable").innerHTML = `<thead><tr><th>Position</th><th>Range · ${UNITS === "tick" ? "tick" : "price"} now</th><th class="num">Value</th><th class="num">Fees earned</th><th class="num">APR 24 h</th><th class="num">APR 7 d</th><th class="num" title="over the position's life">APR life</th><th class="num">Opened</th></tr></thead>
      <tbody>${ps.map(row).join("") || `<tr><td colspan="8" class="mute empty">No positions in this period.</td></tr>`}</tbody>`;
    document.querySelectorAll("#posTable tbody tr[data-k]").forEach(tr => tr.onclick = () => { P.focus = P.focus === tr.dataset.k ? null : tr.dataset.k; chart(shown()); table(shown()); });
  }

  function evText(e) {
    const amt = (a, s) => a ? `${fmt(a, a < 1 ? 6 : 2)} ${esc(s || "")}` : "";
    const name = `<b>${esc(e.name || (e.pool || "").slice(0, 10))}</b>`;
    const moves = ms => (ms || []).map(m => `<span class="${m.amount >= 0 ? "pos" : "neg"} mono">${m.amount >= 0 ? "+" : "−"}${fmt(Math.abs(m.amount), Math.abs(m.amount) < 1 ? 6 : 2)} ${esc(m.symbol)}</span>${m.usd != null ? ` <span class="mute">(${usd(Math.abs(m.usd), 2)})</span>` : ""}`).join(" ");
    switch (e.kind) {
      case "mint": case "burn": {
        const s = e.symbols || [];
        const parts = [amt(e.amount0, s[0]), amt(e.amount1, s[1])].filter(Boolean).join(" + ");
        return `${e.kind === "mint" ? "Added liquidity to" : "Removed liquidity from"} ${name} <span class="mono">[${e.tl}, ${e.tu})</span>${parts ? ` · <span class="mono">${parts}</span>` : ""}`;
      }
      case "claim": return `Claimed rewards ${moves(e.moves)}`;
      case "swap": return `${esc(e.what || "Swap")}${e.name ? " for " + name : ""} ${moves(e.moves)}`;
      case "tx": return `${esc(e.what || "Transaction")}${e.name ? " · " + name : ""}`;
      case "decision": return `Rebalance of ${name} into ${(e.ranges || []).length} range${(e.ranges || []).length === 1 ? "" : "s"}${(e.ranges || []).length ? ` <span class="mono mute">${e.ranges.map(r => `[${r[0]}, ${r[1]})`).join(" ")}</span>` : ""}`;
      case "range_exit": return `${name} left its range <span class="mute">at tick ${esc(e.tick)}</span>`;
      case "range_enter": return `${name} is back in range <span class="mute">at tick ${esc(e.tick)}</span>`;
      case "error": return `${name ? name + ": " : ""}<span class="bad">${esc(e.error || e.what || "error")}</span>`;
      default: return `${esc(kindOf(e.kind)[0])}${e.name ? " · " + name : ""} <span class="mute">${esc(e.what || e.reason || "")}</span>`;
    }
  }

  function timeline() {
    const evs = P.data.events || [];
    const counts = {};
    for (const e of evs) counts[e.kind] = (counts[e.kind] || 0) + 1;
    if (!P.kinds) P.kinds = new Set(Object.keys(counts).filter(k => k !== "tx"));
    $("posKinds").innerHTML = Object.keys(counts).sort((a, b) => counts[b] - counts[a]).map(k => `<button class="pchip ${P.kinds.has(k) ? "on" : ""}" data-kind="${esc(k)}" style="--kc:${kindOf(k)[1]}"><span class="pos-glyph sm" style="background:${kindOf(k)[1]}">${kindOf(k)[2]}</span>${esc(kindOf(k)[0])} <span class="mute">${counts[k]}</span></button>`).join("");
    document.querySelectorAll("#posKinds [data-kind]").forEach(b => b.onclick = () => { const k = b.dataset.kind; P.kinds.has(k) ? P.kinds.delete(k) : P.kinds.add(k); timeline(); });
    let html = "", day = null;
    for (const e of evs.filter(e => P.kinds.has(e.kind))) {
      const d = dayOf(e.ts);
      if (d !== day) { html += `<div class="pos-day">${esc(d)}</div>`; day = d; }
      const link = explorer(e.chain_id, e.tx);
      html += `<div class="pos-ev"><span class="pos-time mono">${new Date(e.ts * 1000).toLocaleTimeString(undefined, {hour: "2-digit", minute: "2-digit", hourCycle: "h23"})}</span>
        <span class="pos-glyph" style="background:${kindOf(e.kind)[1]}">${kindOf(e.kind)[2]}</span>
        <span class="pos-txt">${evText(e)}${e.ok === false ? ' <span class="bad">· reverted</span>' : ""}</span>
        ${link ? `<a class="small" href="${esc(link)}" target="_blank" rel="noopener">tx ↗</a>` : "<span></span>"}</div>`;
    }
    $("posEvents").innerHTML = html || `<p class="mute">Nothing in this period.</p>`;
  }

  function reset() {
    GEN++;
    clearTimeout(P.retry);
    P.cache = {}; P.at = {}; P.pending = {}; P.data = null; P.shown = null; P.focus = null;
    kpiSkeleton();
    if (onTab() && $("posBody")) skeleton();
  }

  window.POSITIONS = {load, reset};
})();
