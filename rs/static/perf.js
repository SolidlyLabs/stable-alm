// Performance tab: each pool's profit with money put in and taken out kept apart (agent/perf.rs,
// /api/pool_stats/<days>). Profit = capital at the end − at the start − deposits + withdrawals; money a
// pool parked in another pool's strategy stays its own (and the host does not count it), and the extra
// a host pays when a parking claim is paid back is a transfer between them, not profit.
// Uses the page's helpers: $, api, fmt, usd, sgn, cost, esc, ctag, chainOf, isAll, tab.
(function () {
  const F = {days: 30, data: null, fees: null, at: 0, busy: false, gen: 0};
  const PERIODS = [[7, "7 days"], [30, "30 days"], [90, "90 days"], [0, "All"]];
  const LINES = {capital: ["Capital", "var(--acc)"], put_in: ["Money put in", "var(--mute)"], hold: ["Holding instead", "var(--warn)"]};
  const FLOW = {deposit: ["Deposit", "var(--ok)"], withdrawal: ["Withdrawal", "var(--bad)"], "transfer in": ["Transfer in", "var(--c4)"],
    "transfer out": ["Transfer out", "var(--c4)"], "sent out": ["Sent out of the wallet", "var(--bad)"], reset: ["Reset", "var(--mute)"]};
  const pct = (x, d = 2) => x == null || isNaN(x) ? "–" : `<span class="${x >= 0 ? "pos" : "neg"}">${x >= 0 ? "+" : "−"}${fmt(Math.abs(x) * 100, d)}%</span>`;
  const when = ts => new Date(ts * 1000).toLocaleString(undefined, {month: "short", day: "numeric", hour: "2-digit", minute: "2-digit"});
  const onTab = () => typeof tab !== "undefined" && tab === "performance";

  function load(force) {
    if (isAll()) { $("perfBody").innerHTML = `<div class="card empty">Choose a wallet above: performance is per wallet.</div>`; return; }
    if (F.busy || (!force && F.data && Date.now() - F.at < 60000 && F.data.days === F.days)) { if (F.data) render(); return; }
    F.busy = true;
    const g = F.gen, days = F.days;
    if (!F.data) $("perfBody").innerHTML = `<div class="card"><span class="skl" style="--w:40%"></span></div>`;
    $("perfInfo").textContent = "loading…";
    // fees earned by the positions (Solidly's figures), 24 h / 7 d / 30 d, beside what was claimed
    const fees = api("/api/positions/30").then(d => {
      const by = {};
      for (const c of d.chains || []) for (const p of c.positions || []) {
        const k = (p.pool || "").toLowerCase(), o = by[k] || (by[k] = {h24: 0, d7: 0, d30: 0});
        o.h24 += p.fees_24h || 0; o.d7 += p.fees_7d || 0; o.d30 += p.fees_30d || 0;
      }
      return by;
    }).catch(() => null);
    Promise.all([api("/api/pool_stats/" + days), fees]).then(([d, f]) => {
      if (g !== F.gen) return;
      F.data = d; F.fees = f; F.at = Date.now();
      render();
    }).catch(e => { $("perfInfo").textContent = ""; $("perfBody").innerHTML = `<div class="card bad">Performance not loaded: ${esc(e.message)}</div>`; })
      .finally(() => { F.busy = false; });
  }

  function setDays(d) { F.days = d; F.data = null; load(true); renderSeg(); }
  function renderSeg() {
    $("perfSeg").innerHTML = PERIODS.map(([d, l]) => `<button class="${F.days === d ? "on" : ""}" data-pd="${d}">${l}</button>`).join("");
    $("perfSeg").querySelectorAll("button").forEach(b => b.onclick = () => setDays(+b.dataset.pd));
  }

  function render() {
    renderSeg();
    const d = F.data, pools = (d.pools || []).filter(p => p.series && p.series.length > 1);
    $("perfInfo").textContent = d.days ? `last ${d.days} days` : "everything recorded";
    if (!pools.length) {
      $("perfBody").innerHTML = `<div class="card empty">No history yet: it builds up as the worker runs (one point per cycle).</div>`;
      return;
    }
    // all pools together: the sums of each pool's window
    const W = pools.map(p => p.window || {});
    const s = k => W.reduce((a, w) => a + (w[k] || 0), 0);
    const pnl = s("pnl_usd"), base = W.reduce((a, w) => a + (w.pnl_usd != null && w.return ? w.pnl_usd / w.return : 0), 0);
    const vs = W.some(w => w.vs_hold_usd != null) ? s("vs_hold_usd") : null;
    const costs = s("gas_usd") + s("swap_cost_usd");
    $("perfBody").innerHTML = `
      <div class="hero">
        <div class="card kpi main"><div class="l">Profit · all pools</div><div class="v">${sgn(pnl, 2)}</div>
          <div class="s"><span class="it">${pct(base ? pnl / base : null)} on the money in them</span></div></div>
        <div class="card kpi"><div class="l">Deposits</div><div class="v">${usd(s("deposits_usd"), 2)}</div><div class="s">put into the pools</div></div>
        <div class="card kpi"><div class="l">Withdrawals</div><div class="v">${usd(s("withdrawals_usd"), 2)}</div><div class="s">taken out (not counted as loss)</div></div>
        <div class="card kpi"><div class="l">Against holding</div><div class="v">${vs == null ? "–" : sgn(vs, 2)}</div><div class="s">vs keeping each pool's tokens</div></div>
        <div class="card kpi"><div class="l">Gas + swap costs</div><div class="v">${cost(costs)}</div><div class="s">inside the profit already</div></div>
      </div>
      ${pools.map((p, i) => card(p, i)).join("")}
      <p class="mute small">Profit is each pool's capital at the end, less at the start, less money put in, plus money taken out.
        A pool's capital is its positions, idle tokens and what it parked in another pool; money it holds for another pool is not counted.
        Fees count once they are claimed into the pool; fees earned but not claimed yet are shown beside it.
        "Holding instead" keeps the tokens the pool started with (half and half when it started parked in USDC).
        Before 21 Nov 2026 two kinds of flow were not recorded, top-ups from Unallocated and tokens sent out of the wallet; in a period that starts earlier, they show as profit or loss.</p>`;
    pools.forEach((p, i) => { draw(p, i, "v"); draw(p, i, "p"); });
  }

  function card(p, i) {
    const w = p.window || {}, f = (F.fees || {})[p.pool] || null;
    const row = (l, x) => `<tr><td>${l}</td><td class=num>${x && x.complete ? sgn(x.pnl_usd, 2) : "–"}</td><td class=num>${x && x.complete ? pct(x.return) : "–"}</td>
      <td class=num>${x && x.complete && x.vs_hold_usd != null ? sgn(x.vs_hold_usd, 2) : "–"}</td><td class=num>${x && x.complete ? usd(x.fees_claimed_usd, 2) : "–"}</td></tr>`;
    const flows = (p.flows || []).slice().reverse();
    return `<div class="card perfcard">
      <div class="row" style="gap:10px;flex-wrap:wrap"><h2 style="margin:0">${esc(p.name)}</h2>${ctag(p.chain_id)}
        ${p.enabled ? "" : `<span class="chip">off</span>`}
        ${w.parked_share > 0.005 ? `<span class="chip" title="share of the period its money was parked in another pool's strategy">parked ${fmt(w.parked_share * 100, 0)}% of the time</span>` : ""}
        <span class="sp"></span><span class="mute small">capital now</span> <b class="mono">${usd(p.capital_usd, 2)}</b></div>
      <div class="pstats">
        <div><div>Profit</div><b>${w.complete ? sgn(w.pnl_usd, 2) : "–"}</b><span>${w.complete ? pct(w.return) + (w.return_yearly != null ? ` · ${pct(w.return_yearly, 1)}/yr` : "") : esc(w.reason || "")}</span></div>
        <div><div>Against holding</div><b>${w.vs_hold_usd != null ? sgn(w.vs_hold_usd, 2) : "–"}</b><span>${w.vs_hold != null ? pct(w.vs_hold) : ""}</span></div>
        <div><div title="fees and rewards claimed into this pool in the period">Fees claimed</div><b>${usd(w.fees_claimed_usd, 2)}</b>
          <span title="earned by its positions, Solidly's figures (claimed weekly)">${f ? `earned ${usd(f.d7, 2)} 7d · ${usd(f.d30, 2)} 30d` : ""}</span></div>
        <div><div>Gas + swaps</div><b>${cost((w.gas_usd || 0) + (w.swap_cost_usd || 0))}</b><span>gas ${usd(w.gas_usd, 2)} · swaps ${usd(w.swap_cost_usd, 2)}</span></div>
        <div><div>Deposits · withdrawals</div><b>${usd(w.deposits_usd, 2)} · ${usd(w.withdrawals_usd, 2)}</b><span>${Math.abs(w.transfers_usd || 0) >= 0.01 ? `transfers ${sgn(w.transfers_usd, 2)}` : ""}</span></div>
      </div>
      <div class="grid g2" style="margin-top:10px">
        <div><div class="mute small" style="margin-bottom:4px">Value</div>${chartBox(i, "v")}
          <div class="legend">${Object.values(LINES).map(([l, c]) => `<span><i class="swc" style="background:${c}"></i>${l}</span>`).join("")}<span><i class="swc" style="background:var(--c6);opacity:.35"></i>Parked</span></div></div>
        <div><div class="mute small" style="margin-bottom:4px">Profit (deposits and withdrawals taken out)</div>${chartBox(i, "p")}
          <div class="legend"><span><i class="swc" style="background:var(--ok)"></i>Deposit</span><span><i class="swc" style="background:var(--bad)"></i>Withdrawal</span><span><i class="swc" style="background:var(--c4)"></i>Transfer</span></div></div>
      </div>
      <table class="small" style="margin-top:10px"><thead><tr><th>Period</th><th class=num>Profit</th><th class=num>Return</th><th class=num>vs holding</th><th class=num>Fees claimed</th></tr></thead>
        <tbody>${row("24 hours", p.h24)}${row("7 days", p.d7)}${row("30 days", p.d30)}</tbody></table>
      ${flows.length ? `<details style="margin-top:8px"><summary class="small">${flows.length} deposit${flows.length === 1 ? "" : "s"}, withdrawal${flows.length === 1 ? "" : "s"} and transfer${flows.length === 1 ? "" : "s"} in the period</summary>
        <table class="small"><tbody>${flows.map(x => `<tr><td>${when(x.ts)}</td><td><i class="swc" style="background:${(FLOW[x.kind] || ["", "var(--mute)"])[1]}"></i>${esc((FLOW[x.kind] || [x.kind])[0])}</td><td class=num>${sgn(x.usd, 2)}</td></tr>`).join("")}</tbody></table></details>` : ""}
    </div>`;
  }

  const chartBox = (i, k) => `<div class="chartwrap" id="pf${k}${i}"><svg class="chart" viewBox="0 0 800 180" preserveAspectRatio="none" style="height:180px"></svg><div class="ax"></div>
    <div class="cross"></div><div class="tip"></div></div>`;

  // one chart: "v" capital, money put in, holding (parked periods shaded); "p" profit, flows marked
  function draw(p, i, k) {
    const wrap = $(`pf${k}${i}`); if (!wrap) return;
    const svg = wrap.querySelector("svg"), axes = wrap.querySelector(".ax"), tip = wrap.querySelector(".tip"), cross = wrap.querySelector(".cross");
    const pts = p.series.map(r => ({ts: r[0], capital: r[1], put_in: r[2], profit: r[3], hold: r[4], parked: r[5]}));
    const keys = k === "v" ? ["capital", "put_in", "hold"] : ["profit"];
    const vals = pts.flatMap(q => keys.map(x => q[x]).filter(v => v != null));
    if (k === "p") vals.push(0);
    const H = 180, x0 = pts[0].ts, x1 = pts[pts.length - 1].ts;
    const lo = Math.min(...vals), hi = Math.max(...vals), span = Math.max(hi - lo, Math.abs(hi) * 0.002, 1), y0 = lo - span * 0.12, dy = span * 1.24;
    const X = t => (t - x0) / (x1 - x0 || 1) * 800, Y = v => H - 6 - (v - y0) / dy * (H - 12);
    const path = key => { let s = "", on = false;
      for (const q of pts) { if (q[key] == null) { on = false; continue; } s += `${on ? "L" : "M"}${X(q.ts).toFixed(1)},${Y(q[key]).toFixed(1)} `; on = true; }
      return s; };
    // parked periods
    let shade = "";
    if (k === "v") for (let j = 0; j < pts.length - 1; j++) if (pts[j].parked > 1)
      shade += `<rect x="${X(pts[j].ts).toFixed(1)}" width="${Math.max(0.5, X(pts[j + 1].ts) - X(pts[j].ts)).toFixed(1)}" y="0" height="${H}" fill="var(--c6)" opacity=".13"/>`;
    const flows = (p.flows || []).filter(f => f.ts >= x0 && f.ts <= x1);
    const marks = flows.map(f => `<line x1="${X(f.ts)}" x2="${X(f.ts)}" y1="0" y2="${H}" stroke="${(FLOW[f.kind] || ["", "var(--mute)"])[1]}" stroke-dasharray="3 3" stroke-width="1" vector-effect="non-scaling-stroke" opacity=".8"/>`).join("");
    const grid = [0, .5, 1].map(f => `<line x1="0" x2="800" y1="${6 + f * (H - 12)}" y2="${6 + f * (H - 12)}" stroke="var(--line)" stroke-width="1" vector-effect="non-scaling-stroke"/>`).join("");
    const lines = k === "v"
      ? `<path d="${path("put_in")}" fill="none" stroke="var(--mute)" stroke-width="1.5" stroke-dasharray="5 4" vector-effect="non-scaling-stroke"/>
         <path d="${path("hold")}" fill="none" stroke="var(--warn)" stroke-width="1.5" stroke-dasharray="2 3" vector-effect="non-scaling-stroke"/>
         <path d="${path("capital")}" fill="none" stroke="var(--acc)" stroke-width="2" vector-effect="non-scaling-stroke"/>`
      : `<line x1="0" x2="800" y1="${Y(0)}" y2="${Y(0)}" stroke="var(--mute)" stroke-width="1" vector-effect="non-scaling-stroke" opacity=".6"/>
         <path d="${path("profit")}" fill="none" stroke="${pts[pts.length - 1].profit >= 0 ? "var(--ok)" : "var(--bad)"}" stroke-width="2" vector-effect="non-scaling-stroke"/>`;
    svg.innerHTML = shade + grid + marks + lines;
    const tfmt = t => { const d = new Date(t * 1000); return x1 - x0 > 2 * 86400 ? d.toLocaleDateString(undefined, {month: "short", day: "numeric"}) : d.toLocaleTimeString(undefined, {hour: "2-digit", minute: "2-digit"}); };
    axes.innerHTML = [0, .5, 1].map(f => { const yp = 6 + f * (H - 12); return `<div class="yl" style="top:${yp}px">${usd(y0 + (H - 6 - yp) / (H - 12) * dy, Math.abs(span) < 50 ? 2 : 0)}</div>`; }).join("") +
      [0, .5, 1].map(f => `<div class="xl" style="left:calc(62px + (100% - 62px) * ${f})">${tfmt(x0 + (x1 - x0) * f)}</div>`).join("");
    wrap.onmousemove = ev => {
      const r = svg.getBoundingClientRect(), fx = (ev.clientX - r.left) / r.width;
      if (fx < 0 || fx > 1) { tip.style.display = cross.style.display = "none"; return; }
      const t = x0 + fx * (x1 - x0), q = pts.reduce((a, b) => Math.abs(b.ts - t) < Math.abs(a.ts - t) ? b : a);
      const px = r.left - wrap.getBoundingClientRect().left + X(q.ts) / 800 * r.width;
      cross.style.left = px + "px"; cross.style.display = "block";
      const near = flows.filter(f => Math.abs(f.ts - q.ts) <= Math.max(600, (x1 - x0) / 300));
      tip.innerHTML = `<div class="t">${when(q.ts)}</div>
        <div><span>Capital</span><span>${usd(q.capital, 2)}</span></div><div><span>Money put in</span><span>${usd(q.put_in, 2)}</span></div>
        <div><b>Profit</b><b>${sgn(q.profit, 2)}</b></div>${q.hold != null ? `<div><span>Holding instead</span><span>${usd(q.hold, 2)}</span></div>` : ""}
        ${q.parked > 1 ? `<div><span>Parked</span><span>${usd(q.parked, 2)}</span></div>` : ""}
        ${near.map(f => `<div><span>${esc((FLOW[f.kind] || [f.kind])[0])}</span><span>${sgn(f.usd, 2)}</span></div>`).join("")}`;
      tip.style.display = "block";
      const tw = tip.offsetWidth, ww = wrap.clientWidth;
      tip.style.left = (px + 12 + tw > ww ? Math.max(px - tw - 12, 0) : px + 12) + "px";
    };
    wrap.onmouseleave = () => { tip.style.display = cross.style.display = "none"; };
  }

  window.PERF = {
    load: () => load(false),
    reset: () => { F.gen++; F.data = null; F.fees = null; },
    // the status refresh: reload in the background while the tab is open
    tick: () => { if (onTab() && !F.busy && Date.now() - F.at > 120000) load(true); },
  };
})();
