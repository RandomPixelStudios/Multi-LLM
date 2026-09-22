/* Multi LLM Web-Verwaltung - Providers (Liste, Picker, Editor, Fetch). */
(function () {
  "use strict";
  const A = window.App;

  async function reload() {
    await A.refreshProviders();
    render();
  }

  function render() {
    const v = document.getElementById("view");
    const provs = A.state.providers;
    let cards = "";
    for (const p of provs) {
      const en = p.models.filter(function (m) { return m.enabled; }).length;
      const dot = !p.status ? "warn" : (p.status === "ok" ? "ok" : "err");
      cards += '<div class="mcard" data-p="' + A.esc(p.id) + '">' +
        '<div class="name">' + A.esc(p.id) + '</div>' +
        '<div class="prov"><span class="dot ' + dot + '"></span><span>' + en + " / " + p.models.length + " models enabled</span></div>" +
        '<div class="prov"><span class="mono">' + A.esc(p.baseUrl) + " · " + A.esc(p.apiFormat) + '</span></div>' +
        (p.hasKey ? "" : '<div style="margin-top:6px;"><span class="badge warn">no API key</span></div>') +
        '<div class="foot"><button class="iconbtn ed" title="Edit">✎</button>' +
        '<button class="iconbtn danger del" title="Delete (click twice)">🗑</button></div></div>';
    }
    v.innerHTML = '<div class="toolbar"><div><h2>Providers</h2>' +
      '<p class="sub">Connect OpenAI-compatible providers and enable their models.</p></div>' +
      '<button class="btn accent" id="p-add">+ Add Provider</button></div>' +
      (provs.length ? '<div class="grid-cards">' + cards + "</div>"
        : '<div class="empty"><b>No providers yet</b><br/>Click "Add Provider" to connect one.</div>');
    document.getElementById("p-add").addEventListener("click", openPicker);
    v.querySelectorAll(".mcard").forEach(function (card) {
      const p = provs.find(function (x) { return x.id === card.dataset.p; });
      card.querySelector(".ed").addEventListener("click", function (e) { e.stopPropagation(); openEditor(p); });
      const del = card.querySelector(".del");
      let armed = false, t = 0;
      del.addEventListener("click", async function (e) {
        e.stopPropagation();
        if (!armed) {
          armed = true; del.style.color = "var(--bad)";
          del.title = "Really delete? Click again.";
          t = setTimeout(function () { armed = false; del.style.color = ""; }, 2600);
          return;
        }
        clearTimeout(t);
        try {
          await A.api("/api/providers/" + encodeURIComponent(p.id), { method: "DELETE" });
          toast0("Provider deleted");
          reload();
        } catch (err) { A.toast(A.errText(err), true); }
      });
      card.addEventListener("click", function () { openEditor(p); });
    });
    function toast0(m) { A.toast(m); }
  }

  /* ---- Preset-Picker ---- */
  function openPicker() {
    const presets = (window.PROVIDER_PRESETS || []).slice().sort(function (a, b) {
      return a.name.localeCompare(b.name);
    });
    const ov = A.openModal("Select provider",
      '<div class="field"><input id="pk-q" type="search" placeholder="Search…" /></div>' +
      '<div class="presetlist" id="pk-list"></div>',
      '<button data-close>Cancel</button>');
    const list = ov.querySelector("#pk-list");
    const q = ov.querySelector("#pk-q");
    function paint() {
      const s = q.value.trim().toLowerCase();
      list.innerHTML = "";
      const mk = function (name, sub, fn) {
        const b = document.createElement("button");
        b.className = "preset";
        b.innerHTML = "<span><b>" + A.esc(name) + '</b><br/><span class="u">' + A.esc(sub) + "</span></span>";
        b.addEventListener("click", fn);
        list.appendChild(b);
      };
      if (!s || "custom".indexOf(s) >= 0) {
        mk("Custom", "Enter everything manually", function () { A.closeModal(); openEditor(null, null); });
      }
      for (const pr of presets) {
        if (s && (pr.name + " " + pr.url).toLowerCase().indexOf(s) < 0) { continue; }
        (function (p) {
          mk(p.name, p.url, function () { A.closeModal(); openEditor(null, p); });
        })(pr);
      }
      if (!list.children.length) { list.innerHTML = '<p class="hint">No matches.</p>'; }
    }
    q.addEventListener("input", paint);
    paint();
    setTimeout(function () { q.focus(); }, 40);
  }

  /* ---- Editor ---- */
  function slugify(name) {
    return name.toLowerCase().replace(/\(local\)/g, "").replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "") || "provider";
  }

  function openEditor(existing, preset) {
    const isEdit = !!existing;
    let models = isEdit
      ? existing.models.map(function (m) { return { id: m.id, name: m.name || m.id, enabled: !!m.enabled, contextLength: m.contextLength }; })
      : [];
    const initId = isEdit ? existing.id : (preset ? slugify(preset.name) : "");
    const initUrl = isEdit ? existing.baseUrl : (preset ? preset.url : "");
    const initFmt = isEdit ? existing.apiFormat : (preset && preset.format === "anthropic" ? "anthropic" : "openai");
    const ov = A.openModal(isEdit ? "Edit provider: " + existing.id : "Add provider",
      (isEdit ? "" : '<div class="field"><label>Provider ID (lowercase)</label><input id="pe-id" type="text" class="mono" /></div>') +
      '<div class="field"><label>Endpoint type</label><select id="pe-fmt">' +
      '<option value="openai">OpenAI-compatible</option><option value="anthropic">Anthropic-compatible</option></select></div>' +
      '<div class="field"><label>Base URL</label><input id="pe-url" type="text" class="mono" /></div>' +
      '<div class="field"><label>API Key' + (isEdit ? " (saved – leave empty to keep)" : "") + '</label>' +
      '<input id="pe-key" type="password" class="mono" autocomplete="off" /></div>' +
      '<div class="secttl">Models</div>' +
      '<div class="searchrow"><input id="pe-mid" type="text" class="mono" placeholder="Add model by ID…" />' +
      '<button id="pe-addm" title="Add">+</button>' +
      '<button id="pe-fetch">Fetch</button></div>' +
      '<div class="searchrow"><input id="pe-q" type="search" placeholder="Filter models…" />' +
      '<button class="btn sm" id="pe-all">All on</button><button class="btn sm" id="pe-none">All off</button></div>' +
      '<div class="err" id="pe-err"></div>' +
      '<div id="pe-grid"></div>',
      '<button data-close>Cancel</button><button class="btn accent" id="pe-save">Save</button>');
    if (!isEdit) { ov.querySelector("#pe-id").value = initId; }
    ov.querySelector("#pe-url").value = initUrl;
    ov.querySelector("#pe-fmt").value = initFmt;
    const grid = ov.querySelector("#pe-grid");
    const err = ov.querySelector("#pe-err");
    const q = ov.querySelector("#pe-q");

    function paintGrid() {
      const s = q.value.trim().toLowerCase();
      grid.innerHTML = "";
      const vis = models.filter(function (m) { return !s || (m.id + " " + m.name).toLowerCase().indexOf(s) >= 0; });
      if (!models.length) { grid.innerHTML = '<p class="hint">No models yet – fetch or add by ID.</p>'; return; }
      if (!vis.length) { grid.innerHTML = '<p class="hint">No matches.</p>'; return; }
      vis.forEach(function (m) {
        const idx = models.indexOf(m);
        const row = document.createElement("div");
        row.className = "mrow";
        const ctx = m.contextLength ? '<span class="badge">' + ctxFmt(m.contextLength) + "</span>" : "";
        row.innerHTML = '<input type="checkbox" class="switch"' + (m.enabled ? " checked" : "") + " />" +
          '<span class="t">' + A.esc(m.name) + '</span><span class="badge mono">' + A.esc(m.id) + "</span>" + ctx +
          '<button class="iconbtn danger" title="Remove">✕</button>';
        row.querySelector("input").addEventListener("change", function (e) { models[idx].enabled = e.target.checked; });
        row.querySelector("button").addEventListener("click", function () { models.splice(idx, 1); paintGrid(); });
        grid.appendChild(row);
      });
    }
    function ctxFmt(n) {
      if (!n) { return ""; }
      return (n >= 1000 ? (Math.round(n / 100) / 10) + "K" : String(n)) + " ctx";
    }
    q.addEventListener("input", paintGrid);
    ov.querySelector("#pe-all").addEventListener("click", function () { models.forEach(function (m) { m.enabled = true; }); paintGrid(); });
    ov.querySelector("#pe-none").addEventListener("click", function () { models.forEach(function (m) { m.enabled = false; }); paintGrid(); });
    function addManual() {
      const inp = ov.querySelector("#pe-mid");
      const id = inp.value.trim();
      if (!id) { err.textContent = "Enter a model ID first."; return; }
      err.textContent = "";
      if (!models.some(function (m) { return m.id === id; })) {
        models.push({ id: id, name: id, enabled: true });
      }
      inp.value = ""; paintGrid();
    }
    ov.querySelector("#pe-addm").addEventListener("click", addManual);
    ov.querySelector("#pe-mid").addEventListener("keydown", function (e) { if (e.key === "Enter") { addManual(); } });

    ov.querySelector("#pe-fetch").addEventListener("click", async function (e) {
      const btn = e.target;
      const base = ov.querySelector("#pe-url").value.trim();
      if (!base) { err.textContent = "Enter the base URL first."; return; }
      err.textContent = "";
      btn.disabled = true;
      try {
        const list = await A.api("/api/providers/fetch", {
          method: "POST",
          body: {
            baseUrl: base,
            apiKey: ov.querySelector("#pe-key").value.trim() || null,
            providerId: isEdit ? existing.id : null,
            apiFormat: ov.querySelector("#pe-fmt").value
          }
        });
        let added = 0;
        for (const f of (list || [])) {
          if (!models.some(function (m) { return m.id === f.id; })) {
            models.push({ id: f.id, name: f.name || f.id, enabled: true, contextLength: f.contextLength });
            added++;
          }
        }
        paintGrid();
        A.toast(added + " new model(s), " + (list || []).length + " total");
      } catch (ex) { err.textContent = A.errText(ex); }
      btn.disabled = false;
    });

    ov.querySelector("#pe-save").addEventListener("click", async function (e) {
      const btn = e.target;
      let id = isEdit ? existing.id : ov.querySelector("#pe-id").value.trim().toLowerCase();
      if (!/^[a-z0-9_-]+$/.test(id)) { err.textContent = "ID: only a-z, 0-9, - and _."; return; }
      const base = ov.querySelector("#pe-url").value.trim();
      if (!/^https?:\/\//.test(base)) { err.textContent = "Base URL must start with http(s)://."; return; }
      const seen = {};
      const clean = models.filter(function (m) {
        const t = (m.id || "").trim();
        if (!t || seen[t]) { return false; }
        seen[t] = 1; return true;
      }).map(function (m) {
        return { id: m.id.trim(), name: (m.name || "").trim() || m.id.trim(), enabled: !!m.enabled, contextLength: m.contextLength || null };
      });
      btn.disabled = true;
      try {
        await A.api("/api/providers", {
          method: "POST",
          body: {
            originalId: isEdit ? existing.id : null,
            id: id, baseUrl: base,
            apiKey: ov.querySelector("#pe-key").value.trim() || null,
            apiFormat: ov.querySelector("#pe-fmt").value,
            models: clean, logo: null
          }
        });
        A.closeModal();
        A.toast(isEdit ? "Provider saved" : "Provider added");
        reload();
      } catch (ex) { err.textContent = A.errText(ex); }
      btn.disabled = false;
    });
    paintGrid();
  }

  window.UI_providers = render;
})();
