/* Multi LLM Web-Verwaltung - Usage. */
(function () {
  "use strict";
  const A = window.App;

  function fmt(n) { return Number(n || 0).toLocaleString("en-US"); }
  function compact(n) {
    n = Number(n || 0);
    if (Math.abs(n) >= 1e9) { return (n / 1e9).toFixed(2).replace(/\.?0+$/, "") + "B"; }
    if (Math.abs(n) >= 1e6) { return (n / 1e6).toFixed(2).replace(/\.?0+$/, "") + "M"; }
    if (Math.abs(n) >= 1e3) { return (n / 1e3).toFixed(1).replace(/\.0$/, "") + "K"; }
    return fmt(n);
  }

  async function render() {
    const v = document.getElementById("view");
    v.innerHTML = '<div class="toolbar"><div><h2>Usage</h2><p class="sub">Token usage per model.</p></div>' +
      '<div><select id="u-range" style="width:auto;"><option value="all">All Time</option>' +
      '<option value="month">Month</option><option value="today">Today</option></select></div></div>' +
      '<div class="tilerow" style="grid-template-columns:repeat(auto-fit,minmax(150px,1fr));">' +
      '<div class="tile"><b id="u-ok">0</b><p>Successful requests</p></div>' +
      '<div class="tile"><b id="u-fail">0</b><p>Failed attempts</p></div>' +
      '<div class="tile"><b id="u-in">0</b><p>Input tokens</p></div>' +
      '<div class="tile"><b id="u-out">0</b><p>Output tokens</p></div></div>' +
      '<div class="card"><table class="tbl"><thead><tr><th>Model</th><th>Provider</th>' +
      '<th class="num">Requests</th><th class="num">Failed</th><th class="num">Input</th>' +
      '<th class="num">Output</th><th></th></tr></thead><tbody id="u-body"></tbody></table>' +
      '<p class="hint hidden" id="u-empty" style="padding:8px 4px;">No usage yet – send a request through the proxy.</p></div>';
    const range = v.querySelector("#u-range");
    range.addEventListener("change", load);
    async function load() {
      let j = null;
      try { j = await A.api("/api/usage?range=" + range.value); }
      catch (e) { A.toast(A.errText(e), true); return; }
      v.querySelector("#u-ok").textContent = fmt(j.requestsOk);
      v.querySelector("#u-fail").textContent = fmt(j.requestsFailed);
      v.querySelector("#u-in").textContent = fmt(j.inputTokens);
      v.querySelector("#u-out").textContent = fmt(j.outputTokens);
      const body = v.querySelector("#u-body");
      body.innerHTML = "";
      const models = (j.models || []).slice().sort(function (a, b) {
        return (b.inputTokens + b.outputTokens) - (a.inputTokens + a.outputTokens);
      });
      v.querySelector("#u-empty").classList.toggle("hidden", models.length > 0);
      for (const m of models) {
        const tr = document.createElement("tr");
        tr.innerHTML = "<td><b>" + A.esc(m.modelId) + "</b></td><td>" + A.esc(m.providerId) + "</td>" +
          '<td class="num">' + fmt(m.requestsOk) + '</td><td class="num">' + fmt(m.requestsFailed) + "</td>" +
          '<td class="num">' + compact(m.inputTokens) + '</td><td class="num">' + compact(m.outputTokens) + "</td>" +
          '<td class="num"><button class="iconbtn danger" title="Delete data">🗑</button></td>';
        tr.querySelector("button").addEventListener("click", async function (e) {
          const btn = e.currentTarget;
          if (!btn.dataset.armed) {
            btn.dataset.armed = "1";
            setTimeout(function () { delete btn.dataset.armed; }, 2600);
            A.toast("Click again to delete");
            return;
          }
          try {
            await A.api("/api/usage/delete", { method: "POST", body: { providerId: m.providerId, modelId: m.modelId } });
            A.toast("Usage data deleted");
            load();
          } catch (ex) { A.toast(A.errText(ex), true); }
        });
        body.appendChild(tr);
      }
    }
    load();
  }

  window.UI_usage = render;
})();
