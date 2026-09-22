/* Multi LLM Web-Verwaltung - Kern: API-Client, Auth, Shell. */
(function () {
  "use strict";

  let toastTimer = 0;
  function toast(msg, isErr) {
    const t = document.getElementById("toast");
    t.textContent = msg;
    t.classList.toggle("err", !!isErr);
    t.classList.add("show");
    clearTimeout(toastTimer);
    toastTimer = setTimeout(function () { t.classList.remove("show"); }, 3200);
  }

  function errText(e) {
    if (e && e.message) { return e.message; }
    return String(e);
  }

  // fetch mit Admin-Header (wie dashboard.js) + Cookies (Session).
  async function api(path, opts) {
    opts = opts || {};
    const headers = Object.assign({ "x-multillm-admin": "1" }, opts.headers || {});
    if (opts.body !== undefined && !headers["Content-Type"]) {
      headers["Content-Type"] = "application/json";
    }
    const r = await fetch(path, {
      method: opts.method || "GET",
      headers: headers,
      credentials: "same-origin",
      body: opts.body === undefined ? undefined : (typeof opts.body === "string" ? opts.body : JSON.stringify(opts.body))
    });
    if (r.status === 401) {
      const err = new Error("unauthorized");
      err.code = 401;
      try { err.detail = await r.json(); } catch (e) { /* ignore */ }
      throw err;
    }
    if (!r.ok) {
      let msg = "HTTP " + r.status;
      try {
        const j = await r.json();
        if (j && j.error && j.error.message) { msg = j.error.message; }
      } catch (e) { /* ignore */ }
      throw new Error(msg);
    }
    const ct = r.headers.get("content-type") || "";
    if (ct.indexOf("application/json") >= 0) { return r.json(); }
    return r.text();
  }

  function avatar(name) {
    return esc((String(name || "?").trim().charAt(0) || "?").toUpperCase());
  }

  /* ---------- Modal ---------- */
  function openModal(title, bodyHtml, footHtml) {
    closeModal();
    const root = document.getElementById("modal-root");
    const ov = document.createElement("div");
    ov.className = "overlay";
    ov.innerHTML = '<div class="dialog" role="dialog" aria-modal="true">' +
      '<div class="body"><h3>' + esc(title) + '</h3><div class="mbody"></div></div>' +
      '<div class="footer"></div></div>';
    ov.querySelector(".mbody").innerHTML = bodyHtml;
    ov.querySelector(".footer").innerHTML = footHtml || '<button data-close>Close</button>';
    root.appendChild(ov);
    ov.addEventListener("mousedown", function (e) { if (e.target === ov) { closeModal(); } });
    ov.querySelectorAll("[data-close]").forEach(function (b) {
      b.addEventListener("click", closeModal);
    });
    document.addEventListener("keydown", escClose);
    return ov;
  }
  function escClose(e) { if (e.key === "Escape") { closeModal(); } }
  function closeModal() {
    document.getElementById("modal-root").innerHTML = "";
    document.removeEventListener("keydown", escClose);
  }

  /* ---------- State ---------- */
  const S = {
    user: null,       // {username, port, is_admin, oauthProvider, email, createdAt}
    providers: [],
    status: null,
    tab: "models"
  };

  function renderUser() {
    const chip = document.getElementById("userchip");
    const lo = document.getElementById("btn-logout");
    if (!S.user) {
      chip.classList.add("hidden");
      lo.classList.add("hidden");
    } else {
      chip.classList.remove("hidden");
      lo.classList.remove("hidden");
      document.getElementById("uavatar").textContent = (S.user.username || "?").trim().charAt(0).toUpperCase();
      document.getElementById("uname").textContent = S.user.username;
    }
    // Den Benutzer-Tab sieht nur der erste Benutzer (Admin).
    const usersBtn = document.querySelector('#sidenav [data-tab="users"]');
    if (usersBtn) { usersBtn.classList.toggle("hidden", !(S.user && S.user.is_admin)); }
  }

  async function refreshStatus() {
    try {
      const st = await api("/api/status");
      S.status = st;
      const pill = document.getElementById("state-pill");
      if (st.running) { pill.textContent = "running"; pill.className = "pill on"; }
      else { pill.textContent = "stopped"; pill.className = "pill off"; }
    } catch (e) {
      const pill = document.getElementById("state-pill");
      pill.textContent = "offline";
      pill.className = "pill off";
    }
  }

  async function refreshProviders() {
    try {
      const j = await api("/api/providers/full");
      S.providers = j.providers || [];
    } catch (e) {
      if (e && e.code === 401) { throw e; }
      S.providers = [];
    }
  }

  /* ---------- Tabs ---------- */
  const TABS = ["models", "providers", "virtual", "api", "usage", "settings", "users"];
  function showTab(which) {
    // Benutzer-Tab ist Admins vorbehalten (wird sonst gar nicht angezeigt).
    if (which === "users" && !(S.user && S.user.is_admin)) { which = "models"; }
    S.tab = which;
    document.querySelectorAll("#sidenav button").forEach(function (b) {
      b.classList.toggle("active", b.dataset.tab === which);
    });
    const fn = {
      models: window.UI_models, providers: window.UI_providers, virtual: window.UI_virtual,
      api: window.UI_api, usage: window.UI_usage, settings: window.UI_settings, users: window.UI_users
    }[which];
    if (fn) { fn(); }
  }

  function wireShell() {
    document.querySelectorAll("#sidenav button").forEach(function (b) {
      b.addEventListener("click", function () { showTab(b.dataset.tab); });
    });
    document.getElementById("btn-logout").addEventListener("click", doLogout);
    document.getElementById("userchip").addEventListener("click", function () { showTab("users"); });
  }

  /* ---------- Auth: Anmeldung auf /login (eigene Seite). ---------- */
  async function doLogout() {
    try { await api("/api/auth/logout", { method: "POST", body: {} }); } catch (e) { /* ignore */ }
    S.user = null;
    location.href = "/login";
  }


  /* ---------- Start ---------- */
  async function boot() {
    await refreshStatus();
    try {
      await refreshProviders();
    } catch (e) {
      if (e && e.code === 401) { location.replace("/login"); return; }
      S.providers = [];
    }
    showTab(S.tab || "models");
    // Status alle 5 s aktualisieren (kein Abmelden bei Fehlern hier).
    if (!boot._t) { boot._t = setInterval(refreshStatus, 5000); }
  }

  async function init() {
    wireShell();
    renderUser();
    // Schon eingeloggt (Cookie vorhanden)? Sonst ab zu /login (eigene Seite).
    try {
      const me = await api("/api/auth/me");
      if (me && me.user) { S.user = me.user; renderUser(); await boot(); return; }
    } catch (e) { /* nicht eingeloggt */ }
    location.replace("/login");
  }

  /* ---------- Benutzer-Tab ---------- */
  async function renderUsers() {
    const v = document.getElementById("view");
    let users = [];
    try { users = await api("/api/auth/users"); } catch (e) { toast(errText(e), true); }
    const rows = users.map(function (u) {
      const self = S.user && S.user.username.toLowerCase() === u.username.toLowerCase();
      const sub = (u.oauthProvider ? u.oauthProvider + " · " : "") + (u.email ? u.email + " · " : "") + (u.is_admin ? "Admin" : "User") + (self ? " · logged in" : "");
      return '<div class="mrow" data-u="' + esc(u.username) + '">' +
        '<span class="avatar">' + avatar(u.username) + '</span>' +
        '<span class="t">' + esc(u.username) + '<br/><span class="hint">' + esc(sub) + "</span></span>" +
        (self ? "" : '<button class="iconbtn danger del" title="Delete">🗑</button>') + "</div>";
    }).join("");
    v.innerHTML = '<div class="toolbar"><div><h2>Users</h2>' +
      '<p class="sub">Only for the first user (admin). Everyone has their own providers, models and API keys.</p></div>' +
      '<button class="btn accent" id="u-add">+ New user</button></div>' +
      (users.length ? rows : '<div class="empty">No users.</div>');
    document.getElementById("u-add").addEventListener("click", function () { location.href = "/login?signup=1"; });
    v.querySelectorAll(".mrow").forEach(function (row) {
      const name = row.dataset.u;
      const del = row.querySelector(".del");
      if (!del) { return; }
      let armed = false;
      del.addEventListener("click", function () {
        if (!armed) {
          armed = true; del.style.color = "var(--bad)";
          toast('Click again to delete "' + name + '"');
          setTimeout(function () { armed = false; del.style.color = ""; }, 2600);
          return;
        }
        (async function () {
          try {
            // No password needed as a logged-in user.
            await api("/api/auth/users/" + encodeURIComponent(name), { method: "DELETE", body: {} });
            toast("User deleted");
            if (S.user && S.user.username.toLowerCase() === name.toLowerCase()) {
              S.user = null; location.href = "/login"; return;
            }
            renderUsers();
          } catch (e) { toast(errText(e), true); }
        })();
      });
    });
  }
  window.UI_users = renderUsers;

  // Export for the sub-modules.
  window.App = {
    api: api, esc: esc, toast: toast, errText: errText, avatar: avatar,
    openModal: openModal, closeModal: closeModal,
    state: S, showTab: showTab,
    refreshStatus: refreshStatus, refreshProviders: refreshProviders, doLogout: doLogout
  };
  document.addEventListener("DOMContentLoaded", init);
})();
