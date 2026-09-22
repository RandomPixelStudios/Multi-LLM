/* Multi LLM Web-Verwaltung - API-Tab (Endpoint, Key, Snippets). */
(function () {
  "use strict";
  const A = window.App;

  async function render() {
    const v = document.getElementById("view");
    let st = {};
    try { st = await A.api("/api/status"); } catch (e) { /* offline */ }
    v.innerHTML = '<div class="toolbar"><div><h2>API</h2>' +
      '<p class="sub">OpenAI-compatible endpoint – point any client at it.</p></div></div>' +
      '<div class="card">' +
      '<div class="field"><label>Base URL</label><div class="urlbox"><span class="dot ' + (st.running ? "ok" : "err") + '"></span>' +
      '<span id="api-base">' + A.esc(st.baseUrl || "–") + '</span></div></div>' +
      '<div class="field" style="max-width:220px;"><label>Port (fixed for everyone)</label>' +
      '<div class="urlbox"><span>' + (st.port || "–") + '</span></div></div>' +
      '<div class="field"><label>API key (auto-created, never changes)</label>' +
      '<div class="searchrow"><input id="api-key" type="password" class="mono" readonly placeholder="loading…" />' +
      '<button id="api-show" title="Show/Hide">👁</button>' +
      '<button id="api-copy" title="Copy">⧉</button></div>' +
      '<p class="hint">Use this key as a Bearer token in all clients – requests are routed to your profile automatically.</p></div>' +
      '<div class="tilerow">' +
      tile("api-enabled", "Enabled", "Serve the OpenAI-compatible API.", !!st.enabled) +
      tile("api-expose", "Expose LAN", "Reachable from the network.", !!st.exposeLan) +
      tile("api-all", "All models", "List all enabled models, not just multillm.", !!st.exposeAllModels) +
      "</div>" +
      '<div class="err" id="api-err"></div>' +
      '<div class="rowactions" style="justify-content:flex-start;">' +
      '<button class="btn accent" id="api-save">Save</button>' +
      '<button id="api-snip">Client config snippets</button></div>' +
      (st.lanUrl ? '<p class="hint" style="margin-top:8px;">LAN: ' + A.esc(st.lanUrl) + "</p>" : "") +
      "</div>";
    function tile(id, t, d, on) {
      return '<div class="tile"><div class="hd"><b>' + t + '</b><input type="checkbox" class="switch" id="' + id + '"' + (on ? " checked" : "") + " /></div><p>" + d + "</p></div>";
    }

    const keyEl = v.querySelector("#api-key");
    v.querySelector("#api-show").addEventListener("click", function () {
      keyEl.type = keyEl.type === "password" ? "text" : "password";
    });
    v.querySelector("#api-copy").addEventListener("click", function () {
      (navigator.clipboard ? navigator.clipboard.writeText(keyEl.value) : Promise.reject()).then(
        function () { A.toast("Copied"); },
        function () { A.toast("Copy failed", true); });
    });
    A.api("/api/api-key").then(
      function (j) { keyEl.value = j.key || ""; },
      function () { keyEl.placeholder = "Could not load key"; });

    v.querySelector("#api-save").addEventListener("click", async function (e) {
      const btn = e.target;
      const err = v.querySelector("#api-err");
      err.textContent = "";
      btn.disabled = true;
      try {
        await A.api("/api/api-settings", {
          method: "POST",
          body: {
            port: st.port || 5000,
            enabled: v.querySelector("#api-enabled").checked,
            exposeLan: v.querySelector("#api-expose").checked,
            exposeAllModels: v.querySelector("#api-all").checked
          }
        });
        A.toast("Saved");
        setTimeout(render, 1200);
      } catch (ex) { err.textContent = A.errText(ex); }
      btn.disabled = false;
    });

    v.querySelector("#api-snip").addEventListener("click", function () {
      const base = (document.getElementById("api-base") || {}).textContent || "";
      const K = "<your-local-key>";
      const code =
        "# PowerShell\n$env:OPENAI_BASE_URL = \"" + base + '"\n$env:OPENAI_API_KEY = "' + K + '"\n\n' +
        "# bash / zsh\nexport OPENAI_BASE_URL=\"" + base + '"\nexport OPENAI_API_KEY="' + K + '"\n\n' +
        "# curl\ncurl " + base + "/chat/completions \\\n" +
        '  -H "Content-Type: application/json" \\\n  -H "Authorization: Bearer ' + K + '" \\\n' +
        "  -d '{\"model\": \"<model-id>\", \"messages\": [{\"role\": \"user\", \"content\": \"Hello!\"}]}'";
      const ov = A.openModal("Client config snippets",
        '<p class="hint" style="margin-bottom:8px;">Replace <span class="mono">&lt;your-local-key&gt;</span> with your key and <span class="mono">&lt;model-id&gt;</span> with an enabled model.</p>' +
        '<pre class="snip" id="snip-pre"></pre>',
        '<button id="snip-copy">Copy</button><button class="btn accent" data-close>Done</button>');
      ov.querySelector("#snip-pre").textContent = code;
      ov.querySelector("#snip-copy").addEventListener("click", function () {
        (navigator.clipboard ? navigator.clipboard.writeText(code) : Promise.reject()).then(
          function () { A.toast("Copied"); },
          function () { A.toast("Copy failed", true); });
      });
      ov.querySelector("[data-close]").addEventListener("click", A.closeModal);
    });
  }

  window.UI_api = render;
})();
