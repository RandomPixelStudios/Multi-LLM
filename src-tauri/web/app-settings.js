/* Multi LLM Web-Verwaltung - Settings (Routing, Kompression, Export/Import). */
(function () {
  "use strict";
  const A = window.App;

  async function render() {
    const v = document.getElementById("view");
    let s = {};
    try { s = await A.api("/api/settings"); } catch (e) { A.toast(A.errText(e), true); }
    const routing = s.routing || { strategy: "weighted" };
    const compress = s.compress || { enabled: false, strength: 3 };
    v.innerHTML = '<div class="toolbar"><div><h2>Settings</h2><p class="sub">App behavior.</p></div></div>' +
      '<div class="card"><div class="setrow"><div class="tx"><b>Routing strategy</b>' +
      '<p>How "multillm" and bundles pick a model per request.</p></div>' +
      '<select id="s-strat">' +
      '<option value="weighted">Weighted random (starred ×4)</option>' +
      '<option value="round_robin">Round robin</option>' +
      '<option value="priority">Priority (provider order)</option>' +
      '<option value="latency">Lowest average latency</option>' +
      '<option value="fastest">Fastest (latency × health)</option>' +
      '<option value="sticky">Sticky sessions</option></select></div>' +
      '<div class="setrow"><div class="tx"><b>Token compression</b><p>Shrink prompts before sending – saves tokens.</p></div>' +
      '<input type="checkbox" class="switch" id="s-comp"' + (compress.enabled ? " checked" : "") + " /></div>" +
      '<div class="setrow"><div class="tx"><b>Compression strength (1–10)</b>' +
      "<p>1–3 safe · 4–6 stronger · 7–10 aggressive.</p></div>" +
      '<input id="s-strength" type="number" min="1" max="10" style="width:90px;" value="' + (compress.strength || 3) + '" /></div>' +
      '<div class="err" id="s-err"></div>' +
      '<div class="rowactions" style="justify-content:flex-start;"><button class="btn accent" id="s-save">Save</button></div></div>' +
      '<div class="card"><div class="setrow"><div class="tx"><b>Export config</b><p>Providers, models and settings as JSON.</p></div>' +
      '<button id="s-exp">Export</button></div>' +
      '<div class="setrow"><div class="tx"><b>Import config</b><p>Restore an exported JSON file (overwrites same IDs).</p></div>' +
      '<button id="s-imp">Import</button><input type="file" id="s-file" accept="application/json" class="hidden" /></div>' +
      '<div class="setrow"><div class="tx"><b>Export usage</b><p>Token usage as JSON.</p></div>' +
      '<button id="s-expu">Export</button></div></div>';
    v.querySelector("#s-strat").value = routing.strategy || "weighted";

    v.querySelector("#s-save").addEventListener("click", async function (e) {
      const btn = e.target, err = v.querySelector("#s-err");
      const strength = Math.min(10, Math.max(1, parseInt(v.querySelector("#s-strength").value, 10) || 3));
      err.textContent = "";
      btn.disabled = true;
      try {
        await A.api("/api/settings", {
          method: "POST",
          body: {
            routing: { strategy: v.querySelector("#s-strat").value, latencyWindow: routing.latencyWindow || 20 },
            compress: { enabled: v.querySelector("#s-comp").checked, strength: strength }
          }
        });
        A.toast("Saved");
        render();
      } catch (ex) { err.textContent = A.errText(ex); }
      btn.disabled = false;
    });

    function download(text, name) {
      const a = document.createElement("a");
      a.href = URL.createObjectURL(new Blob([text], { type: "application/json" }));
      a.download = name;
      a.click();
      setTimeout(function () { URL.revokeObjectURL(a.href); }, 2000);
    }
    v.querySelector("#s-exp").addEventListener("click", async function () {
      try {
        const t = await A.api("/api/export/config");
        download(typeof t === "string" ? t : JSON.stringify(t, null, 2), "multillm-config.json");
        A.toast("Config exported");
      } catch (e) { A.toast(A.errText(e), true); }
    });
    v.querySelector("#s-expu").addEventListener("click", async function () {
      try {
        const t = await A.api("/api/export/usage");
        download(typeof t === "string" ? t : JSON.stringify(t, null, 2), "multillm-usage.json");
        A.toast("Usage exported");
      } catch (e) { A.toast(A.errText(e), true); }
    });
    const file = v.querySelector("#s-file");
    v.querySelector("#s-imp").addEventListener("click", function () { file.click(); });
    file.addEventListener("change", async function () {
      const f = file.files[0];
      if (!f) { return; }
      const text = await f.text();
      try {
        await A.api("/api/import/config", { method: "POST", body: { json: text } });
        A.toast("Config imported");
        await A.refreshProviders();
        render();
      } catch (e) { A.toast(A.errText(e), true); }
      file.value = "";
    });
  }

  window.UI_settings = render;
})();
