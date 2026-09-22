/* Multi LLM - eigenständige Anmeldeseite (kein App-UI, keine Sidebar). */
(function () {
  "use strict";

  function esc(s) {
    return String(s == null ? "" : s).replace(/&/g, "&amp;").replace(/</g, "&lt;")
      .replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");
  }

  let toastTimer = 0;
  function toast(msg, isErr) {
    const t = document.getElementById("toast");
    t.textContent = msg;
    t.classList.toggle("err", !!isErr);
    t.classList.add("show");
    clearTimeout(toastTimer);
    toastTimer = setTimeout(function () { t.classList.remove("show"); }, 3200);
  }

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
      body: opts.body === undefined ? undefined : JSON.stringify(opts.body)
    });
    if (!r.ok) {
      let msg = "HTTP " + r.status;
      try {
        const j = await r.json();
        if (j && j.error && j.error.message) { msg = j.error.message; }
      } catch (e) { /* ignore */ }
      const err = new Error(msg);
      err.code = r.status;
      throw err;
    }
    return r.json();
  }

  function avatar(name) {
    return esc((String(name || "?").trim().charAt(0) || "?").toUpperCase());
  }

  function shell(title, sub, bodyHtml) {
    const root = document.getElementById("login-root");
    root.innerHTML = '<div class="authwrap"><div class="card">' +
      '<div class="authlogo">M</div><h2 style="font-size:21px;margin-bottom:4px;">' + esc(title) + '</h2>' +
      '<p class="sub" style="margin-bottom:18px;">' + esc(sub) + '</p>' +
      '<div class="loginbody"></div></div></div>';
    const body = root.querySelector(".loginbody");
    body.innerHTML = bodyHtml;
    return body;
  }

  /* ---------- Login ---------- */
  async function showLogin() {
    let users = [];
    try { users = await api("/api/auth/users"); } catch (e) { users = []; }
    if (!users.length) { showSignup(false); return; }
    let selected = null;
    const rows = users.map(function (u) {
      return '<button class="userrow" data-u="' + esc(u.username) + '">' +
        '<span class="avatar">' + avatar(u.username) + '</span>' +
        '<span class="nm">' + esc(u.username) + '</span></button>';
    }).join("");
    const body = shell("Welcome back",
      "Pick your user, enter your password, done.",
      '<div class="userlist">' + rows + '</div>' +
      '<div class="field"><label>Password</label><input id="li-pw" type="password" autocomplete="current-password" /></div>' +
      '<div class="err" id="li-err"></div>' +
      '<div class="rowactions"><button id="li-add">+ New user</button>' +
      '<span class="flex"></span><button class="btn accent" id="li-go">Log in</button></div>');
    body.querySelectorAll(".userrow").forEach(function (r) {
      r.addEventListener("click", function () {
        body.querySelectorAll(".userrow").forEach(function (x) { x.classList.remove("sel"); });
        r.classList.add("sel");
        selected = r.dataset.u;
        body.querySelector("#li-pw").focus();
      });
    });
    const pw = body.querySelector("#li-pw");
    async function go() {
      const err = body.querySelector("#li-err");
      if (!selected) { err.textContent = "Please select a user first."; return; }
      if (!pw.value) { err.textContent = "Please enter the password."; pw.focus(); return; }
      err.textContent = "";
      try {
        await api("/api/auth/login", { method: "POST", body: { username: selected, password: pw.value } });
        location.href = "/";
      } catch (e) {
        err.textContent = e && e.code === 401 ? "Unknown user or wrong password." : String((e && e.message) || e);
      }
    }
    body.querySelector("#li-go").addEventListener("click", go);
    pw.addEventListener("keydown", function (e) { if (e.key === "Enter") { go(); } });
    body.querySelector("#li-add").addEventListener("click", function () { showSignup(true); });
    setTimeout(function () { pw.focus(); }, 40);
  }

  /* ---------- Konto erstellen (nur Name + Passwort) ---------- */
  function showSignup(backToLogin) {
    const body = shell("Create user",
      "Just username + password. The API key is auto-created and never changes.",
      '<div class="field"><label>Username</label><input id="w-name" type="text" autocomplete="username" placeholder="e.g. anna" /></div>' +
      '<div class="field"><label>Password (min. 10 chars)</label><input id="w-pw" type="password" autocomplete="new-password" /></div>' +
      '<div class="field"><label>Repeat password</label><input id="w-pw2" type="password" autocomplete="new-password" /></div>' +
      '<div class="err" id="w-err"></div>' +
      '<div class="rowactions">' + (backToLogin ? '<button id="w-back">Back</button>' : '') +
      '<span class="flex"></span><button class="btn accent" id="w-go">Create account</button></div>');
    const name = body.querySelector("#w-name"), pw = body.querySelector("#w-pw"), pw2 = body.querySelector("#w-pw2");
    const err = body.querySelector("#w-err");
    async function go() {
      const n = name.value.trim();
      if (n.length < 2) { err.textContent = "Username needs at least 2 characters."; name.focus(); return; }
      if (!/^[A-Za-z0-9_-]+$/.test(n)) { err.textContent = "Only letters, digits, _ and -."; name.focus(); return; }
      if (pw.value.length < 10) { err.textContent = "Password needs at least 10 characters."; pw.focus(); return; }
      if (pw.value !== pw2.value) { err.textContent = "Passwords do not match."; pw2.focus(); return; }
      err.textContent = "";
      try {
        await api("/api/auth/users", { method: "POST", body: { username: n, password: pw.value, oauthProvider: null, email: null } });
        await api("/api/auth/login", { method: "POST", body: { username: n, password: pw.value } });
        location.href = "/";
      } catch (e) { err.textContent = String((e && e.message) || e); }
    }
    body.querySelector("#w-go").addEventListener("click", go);
    [name, pw, pw2].forEach(function (i) { i.addEventListener("keydown", function (e) { if (e.key === "Enter") { go(); } }); });
    if (backToLogin) { body.querySelector("#w-back").addEventListener("click", showLogin); }
    // Mit gültiger Session (z. B. Admin legt an): Rückweg in die App anbieten.
    api("/api/auth/me").then(function (me) {
      if (me && me.user) {
        const back = document.createElement("button");
        back.className = "skip";
        back.textContent = "← Back to app (as " + me.user.username + ")";
        back.addEventListener("click", function () { location.href = "/"; });
        body.appendChild(back);
      }
    }, function () { /* nicht eingeloggt: kein Rückweg nötig */ });
    setTimeout(function () { name.focus(); }, 40);
  }

  /* ---------- Start ---------- */
  async function init() {
    // Bereits angemeldet (Session-Cookie)? Direkt in die App.
    // ?signup=1 erzwingt trotzdem die Erstellung (für Admins mit Session).
    const forceSignup = /[?&]signup=1\b/.test(location.search);
    if (!forceSignup) {
      try {
        const me = await api("/api/auth/me");
        if (me && me.user) { location.replace("/"); return; }
      } catch (e) { /* nicht eingeloggt -> Auswahl zeigen */ }
    }
    showLogin();
  }

  document.addEventListener("DOMContentLoaded", init);
})();
