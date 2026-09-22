(function () {
  // No key prompt here anymore: without a session the server redirects to
  // /login, with a session the browser sends the cookie automatically.
  // A 401 therefore means "not logged in" -> go to the login page.
  function esc(s) { return String(s == null ? "" : s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;"); }
  async function getJSON(path) {
    const r = await fetch(path, { headers: { "x-multillm-admin": "1" } });
    if (r.status === 401) { location.href = "/login"; throw new Error("unauthorized"); }
    if (!r.ok) { throw new Error("HTTP " + r.status); }
    return r.json();
  }
  const fmt = function (n) { return Number(n || 0).toLocaleString("en-US"); };

  function renderChart(days) {
    const host = document.getElementById("chart");
    const has = Array.isArray(days) && days.length && days.some(function (d) { return d.inputTokens + d.outputTokens > 0; });
    if (!has) { host.innerHTML = "<p class=\"muted\" style=\"padding:10px 4px;\">No activity in the last 14 days.</p>"; return; }
    // Chart palette follows the OS color scheme (the SVG is built by hand).
    const light = window.matchMedia && window.matchMedia("(prefers-color-scheme: light)").matches;
    const cc = light
      ? { axis: "#d5d5d5", out: "#2b2b2b", inp: "#9a9a9a", label: "#767676", last: "#1b1b1b" }
      : { axis: "#383838", out: "#ededed", inp: "#8f8f8f", label: "#808080", last: "#ffffff" };
    const W = Math.max(560, host.clientWidth - 34), H = 170, top = 14, baseY = 138, plotH = baseY - top;
    const n = days.length, slotW = (W - 16) / n, barW = Math.min(30, Math.max(6, slotW * 0.55));
    let maxT = 1;
    days.forEach(function (d) { maxT = Math.max(maxT, d.inputTokens + d.outputTokens); });
    let svg = "<svg width=\"" + W + "\" height=\"" + H + "\" xmlns=\"http://www.w3.org/2000/svg\">";
    svg += "<line x1=\"8\" y1=\"" + baseY + "\" x2=\"" + (W - 8) + "\" y2=\"" + baseY + "\" stroke=\"" + cc.axis + "\"/>";
    days.forEach(function (d, i) {
      const total = d.inputTokens + d.outputTokens, cx = 8 + slotW * i + slotW / 2;
      const tip = esc(d.day) + " \u00b7 " + fmt(d.inputTokens) + " in / " + fmt(d.outputTokens) + " out";
      svg += "<rect x=\"" + (cx - slotW / 2) + "\" y=\"" + top + "\" width=\"" + slotW + "\" height=\"" + plotH + "\" fill=\"transparent\"><title>" + tip + "</title></rect>";
      if (total > 0) {
        let hI = Math.round(plotH * d.inputTokens / maxT), hO = Math.round(plotH * d.outputTokens / maxT);
        if (hI + hO > plotH) { const k = plotH / (hI + hO); hI = Math.floor(hI * k); hO = Math.floor(hO * k); }
        if (d.inputTokens > 0 && hI < 2) hI = 2;
        if (d.outputTokens > 0 && hO < 2) hO = 2;
        if (hI + hO > plotH) { if (hI > hO) { hI -= hI + hO - plotH; } else { hO -= hI + hO - plotH; } }
        if (hO > 0) svg += "<rect x=\"" + (cx - barW / 2) + "\" y=\"" + (baseY - hI - hO) + "\" width=\"" + barW + "\" height=\"" + hO + "\" rx=\"2\" fill=\"" + cc.out + "\"><title>" + tip + "</title></rect>";
        if (hI > 0) svg += "<rect x=\"" + (cx - barW / 2) + "\" y=\"" + (baseY - hI) + "\" width=\"" + barW + "\" height=\"" + hI + "\" rx=\"2\" fill=\"" + cc.inp + "\"><title>" + tip + "</title></rect>";
      }
      if (i % 2 === 0 || i === n - 1) {
        svg += "<text x=\"" + cx + "\" y=\"158\" text-anchor=\"middle\" font-size=\"9.5\" fill=\"" + (i === n - 1 ? cc.last : cc.label) + "\" font-family=\"Segoe UI\">" + esc(d.day.slice(8)) + "." + esc(d.day.slice(5, 7)) + ".</text>";
      }
    });
    host.innerHTML = svg + "</svg>";
  }

  async function loadAll() {
    try {
      const st = await getJSON("/api/status");
      const pill = document.getElementById("state-pill");
      if (st.running) { pill.textContent = "running"; pill.className = "pill on"; }
      else { pill.textContent = "stopped"; pill.className = "pill off"; }
      document.getElementById("s-port").textContent = st.port || "-";
      document.getElementById("s-base").textContent = st.baseUrl || "-";
      document.getElementById("s-lan").textContent = st.lanUrl ? st.lanUrl : "localhost only";
      document.getElementById("s-models").textContent = st.modelsEnabled + " / " + st.modelsTotal;
    } catch (e) { document.getElementById("state-pill").textContent = "offline"; }

    try {
      const m = await getJSON("/api/models");
      const wrap = document.getElementById("providers");
      wrap.innerHTML = m.providers.length ? "" : "<span class=\"muted\">No providers configured yet.</span>";
      m.providers.forEach(function (p) {
        const row = document.createElement("div"); row.className = "prov";
        let chips = "";
        p.models.forEach(function (mo) { chips += "<span class=\"chip" + (mo.enabled ? "" : " dis") + "\">" + esc(mo.id) + "</span>"; });
        row.innerHTML = "<span class=\"pid\">" + esc(p.id) + "</span><span class=\"purl\">" + esc(p.baseUrl) + "</span>" +
          (p.apiFormat === "anthropic" ? "<span class=\"chip\">Anthropic</span>" : "") + chips + (p.hasKey ? "" : "<span class=\"nokey\">no API key</span>");
        wrap.appendChild(row);
      });
    } catch (e) { document.getElementById("providers").innerHTML = "<span class=\"muted\">Could not load models.</span>"; }

    try {
      const u = await getJSON("/api/usage?range=all");
      document.getElementById("u-ok").textContent = fmt(u.requestsOk);
      document.getElementById("u-fail").textContent = fmt(u.requestsFailed);
      document.getElementById("u-in").textContent = fmt(u.inputTokens);
      document.getElementById("u-out").textContent = fmt(u.outputTokens);
      const tb = document.getElementById("usage-body");
      tb.innerHTML = "";
      document.getElementById("usage-empty").style.display = u.models.length ? "none" : "block";
      u.models.forEach(function (e) {
        const tr = document.createElement("tr");
        tr.innerHTML = "<td><b>" + esc(e.modelId) + "</b> <span class=\"muted\">(" + esc(e.providerId) + ")</span></td>" +
          "<td class=\"num\">" + fmt(e.requestsOk) + "</td>" +
          "<td class=\"num\">" + (e.requestsFailed ? fmt(e.requestsFailed) : "-") + "</td>" +
          "<td class=\"num\">" + fmt(e.inputTokens) + "</td>" +
          "<td class=\"num\">" + fmt(e.outputTokens) + "</td>";
        tb.appendChild(tr);
      });
    } catch (e) { const em = document.getElementById("usage-empty"); em.textContent = "Could not load usage."; em.style.display = "block"; }

    try {
      const s = await getJSON("/api/usage/daily?days=14");
      renderChart(s.days);
    } catch (e) { document.getElementById("chart").innerHTML = "<p class=\"muted\" style=\"padding:10px 4px;\">Could not load activity chart.</p>"; }
  }

  document.getElementById("btn-refresh").addEventListener("click", function () {
    loadAll();
  });
  loadAll();
  window.setInterval(loadAll, 10000);
})();
