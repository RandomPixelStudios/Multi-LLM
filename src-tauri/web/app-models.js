/* Multi LLM Web-Verwaltung - Models & Virtual Models. */
(function () {
  "use strict";
  const A = window.App;

  function allEnabled() {
    const out = [];
    for (const p of A.state.providers) {
      for (const m of p.models) {
        if (m.enabled) { out.push({ p: p, m: m }); }
      }
    }
    return out;
  }

  /* ---------- Models ---------- */
  function renderModels() {
    const v = document.getElementById("view");
    const rows = allEnabled();
    v.innerHTML = '<div class="toolbar"><div><h2>Models</h2>' +
      '<p class="sub" id="m-sub">' + rows.length + " models enabled</p></div></div>" +
      '<div class="searchrow"><input id="m-q" type="search" placeholder="Search models…" />' +
      '<select id="m-sort"><option value="name">Sort: Name</option>' +
      '<option value="provider">Sort: Provider</option>' +
      '<option value="starred">Sort: Starred</option></select></div>' +
      '<div class="grid-cards" id="m-grid"></div>' +
      '<div class="empty hidden" id="m-empty"><b>No models yet</b><br/>Enable models on a provider in the Providers tab.</div>';
    const grid = v.querySelector("#m-grid");
    const q = v.querySelector("#m-q"), sort = v.querySelector("#m-sort");
    function paint() {
      const s = q.value.trim().toLowerCase();
      let list = rows.filter(function (r) {
        return !s || ((r.m.name || "") + " " + r.m.id + " " + r.p.id).toLowerCase().indexOf(s) >= 0;
      });
      if (sort.value === "provider") {
        list.sort(function (a, b) { return a.p.id === b.p.id ? a.m.id.localeCompare(b.m.id) : a.p.id.localeCompare(b.p.id); });
      } else if (sort.value === "starred") {
        list.sort(function (a, b) { return ((b.m.starred ? 1 : 0) - (a.m.starred ? 1 : 0)) || a.m.id.localeCompare(b.m.id); });
      } else {
        list.sort(function (a, b) { return (a.m.name || a.m.id).localeCompare(b.m.name || b.m.id); });
      }
      grid.innerHTML = "";
      v.querySelector("#m-empty").classList.toggle("hidden", rows.length > 0);
      v.querySelector("#m-sub").textContent = s
        ? list.length + " of " + rows.length + " match"
        : rows.length + " models enabled";
      for (const r of list) {
        const ctx = r.m.contextLength ? '<span class="hint mono">' + ctxFmt(r.m.contextLength) + "</span>" : "";
        const card = document.createElement("div");
        card.className = "mcard";
        card.innerHTML = '<button class="star' + (r.m.starred ? " on" : "") + '" title="Star">' + (r.m.starred ? "★" : "☆") + "</button>" +
          '<div class="name">' + A.esc(r.m.name || r.m.id) + '</div>' +
          '<div class="prov"><span class="dot"></span><span>' + A.esc(r.p.id) + '</span></div>' +
          '<div class="foot">' + ctx +
          '<button class="iconbtn danger mdel" title="Remove model">🗑</button></div>';
        card.querySelector(".star").addEventListener("click", async function () {
          try {
            await A.api("/api/models/star", { method: "POST", body: { providerId: r.p.id, modelId: r.m.id, starred: !r.m.starred } });
            await A.refreshProviders();
            renderModels();
          } catch (e) { A.toast(A.errText(e), true); }
        });
        card.querySelector(".mdel").addEventListener("click", function (e) {
          const btn = e.currentTarget;
          if (!btn.dataset.armed) {
            btn.dataset.armed = "1"; btn.title = "Really remove? Click again.";
            setTimeout(function () { delete btn.dataset.armed; }, 2600);
            return;
          }
          (async function () {
            try {
              await A.api("/api/models/delete", { method: "POST", body: { providerId: r.p.id, modelId: r.m.id } });
              A.toast("Model removed");
              await A.refreshProviders();
              renderModels();
            } catch (ex) { A.toast(A.errText(ex), true); }
          })();
        });
        grid.appendChild(card);
      }
    }
    function ctxFmt(n) {
      return (n >= 1000 ? (Math.round(n / 100) / 10) + "K" : String(n)) + " ctx";
    }
    q.addEventListener("input", paint);
    sort.addEventListener("change", paint);
    paint();
  }

  /* ---------- Virtual Models ---------- */
  async function renderVirtual() {
    const v = document.getElementById("view");
    let list = [];
    try {
      const j = await A.api("/api/virtual");
      list = j.virtualModels || [];
    } catch (e) { A.toast(A.errText(e), true); }
    let cards = "";
    for (const it of list) {
      cards += '<div class="mcard" data-v="' + A.esc(it.id) + '">' +
        '<div class="name">' + A.esc(it.id) + "</div>" +
        '<div class="prov"><span>' + it.models.length + " models bundled</span></div>" +
        '<div class="foot"><button class="iconbtn ed" title="Edit">✎</button>' +
        '<button class="iconbtn danger del" title="Delete (click twice)">🗑</button></div></div>';
    }
    v.innerHTML = '<div class="toolbar"><div><h2>Virtual Models</h2>' +
      '<p class="sub">Bundle multiple models under one API name.</p></div>' +
      '<button class="btn accent" id="v-add">+ Create Virtual Model</button></div>' +
      (list.length ? '<div class="grid-cards">' + cards + "</div>"
        : '<div class="empty"><b>No virtual models yet</b><br/>Bundle models under a custom API name.</div>');
    document.getElementById("v-add").addEventListener("click", function () { openVirtual(null); });
    v.querySelectorAll(".mcard").forEach(function (card) {
      const it = list.find(function (x) { return x.id === card.dataset.v; });
      card.querySelector(".ed").addEventListener("click", function (e) { e.stopPropagation(); openVirtual(it); });
      const del = card.querySelector(".del");
      let armed = false;
      del.addEventListener("click", async function (e) {
        e.stopPropagation();
        if (!armed) {
          armed = true; del.style.color = "var(--bad)";
          setTimeout(function () { armed = false; del.style.color = ""; }, 2600);
          return;
        }
        try {
          await A.api("/api/virtual/" + encodeURIComponent(it.id), { method: "DELETE" });
          A.toast("Virtual model deleted");
          renderVirtual();
        } catch (err) { A.toast(A.errText(err), true); }
      });
      card.addEventListener("click", function () { openVirtual(it); });
    });
  }

  function openVirtual(existing) {
    const isEdit = !!existing;
    let selected = isEdit ? existing.models.slice() : [];
    const all = [];
    for (const p of A.state.providers) {
      for (const m of p.models) {
        if (m.enabled) { all.push({ key: p.id + "::" + m.id, label: m.name || m.id, prov: p.id }); }
      }
    }
    const ov = A.openModal(isEdit ? "Edit virtual model" : "Create virtual model",
      '<div class="field"><label>Model ID (for API requests)</label>' +
      '<input id="vm-id" type="text" class="mono" value="' + (isEdit ? A.esc(existing.id) : "") + '" /></div>' +
      '<div class="secttl">Bundled models (<span id="vm-n">' + selected.length + "</span>)</div>" +
      '<div class="searchrow"><input id="vm-q" type="search" placeholder="Search…" />' +
      '<button class="btn sm" id="vm-all">All</button><button class="btn sm" id="vm-clear">None</button></div>' +
      '<div class="err" id="vm-err"></div><div id="vm-grid"></div>' +
      '<div class="secttl">Routing (optional)</div>' +
      '<div class="field"><label>Strategy override</label><select id="vm-strat">' +
      '<option value="">Global default</option><option value="weighted">Weighted random</option>' +
      '<option value="round_robin">Round robin</option><option value="priority">Priority</option>' +
      '<option value="latency">Lowest latency</option><option value="fastest">Fastest</option>' +
      '<option value="sticky">Sticky sessions</option></select></div>',
      '<button data-close>Cancel</button><button class="btn accent" id="vm-save">Save</button>');
    const grid = ov.querySelector("#vm-grid");
    const q = ov.querySelector("#vm-q");
    const err = ov.querySelector("#vm-err");
    if (isEdit && existing.policy && existing.policy.strategy) {
      ov.querySelector("#vm-strat").value = existing.policy.strategy;
    }
    function paint() {
      const s = q.value.trim().toLowerCase();
      grid.innerHTML = "";
      ov.querySelector("#vm-n").textContent = String(selected.length);
      if (!all.length) { grid.innerHTML = '<p class="hint">No enabled models – enable provider models first.</p>'; return; }
      for (const e of all) {
        if (s && (e.label + " " + e.key).toLowerCase().indexOf(s) < 0) { continue; }
        const row = document.createElement("div");
        row.className = "mrow";
        row.innerHTML = '<input type="checkbox" class="switch"' + (selected.indexOf(e.key) >= 0 ? " checked" : "") + " />" +
          '<span class="t">' + A.esc(e.label) + '</span><span class="badge">' + A.esc(e.prov) + "</span>";
        row.querySelector("input").addEventListener("change", function (ev) {
          if (ev.target.checked) {
            if (selected.indexOf(e.key) < 0) { selected.push(e.key); }
          } else {
            selected = selected.filter(function (k) { return k !== e.key; });
          }
          ov.querySelector("#vm-n").textContent = String(selected.length);
        });
        grid.appendChild(row);
      }
    }
    q.addEventListener("input", paint);
    ov.querySelector("#vm-all").addEventListener("click", function () {
      for (const e of all) { if (selected.indexOf(e.key) < 0) { selected.push(e.key); } }
      paint();
    });
    ov.querySelector("#vm-clear").addEventListener("click", function () { selected = []; paint(); });
    ov.querySelector("#vm-save").addEventListener("click", async function (e) {
      const btn = e.target;
      const id = ov.querySelector("#vm-id").value.trim().toLowerCase();
      if (!/^[a-z0-9_-]+$/.test(id)) { err.textContent = "ID: only a-z, 0-9, - and _."; return; }
      btn.disabled = true;
      try {
        await A.api("/api/virtual", {
          method: "POST",
          body: {
            originalId: isEdit ? existing.id : null, id: id, models: selected, logo: null,
            tiers: null, strategyOverride: ov.querySelector("#vm-strat").value || null
          }
        });
        A.closeModal();
        A.toast(isEdit ? "Virtual model saved" : "Virtual model created");
        renderVirtual();
      } catch (ex) { err.textContent = A.errText(ex); }
      btn.disabled = false;
    });
    paint();
  }

  window.UI_models = renderModels;
  window.UI_virtual = renderVirtual;
})();
