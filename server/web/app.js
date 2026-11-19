/* Multi LLM web UI - single file, no build step. */
(function () {
  "use strict";

  /* ================= helpers ================= */
  const $ = (s, r) => (r || document).querySelector(s);
  const $$ = (s, r) => Array.from((r || document).querySelectorAll(s));
  const esc = (v) => String(v == null ? "" : v).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  const initial = (n) => esc((String(n || "?").trim().charAt(0) || "?").toUpperCase());
  const fmt = (n) => {
    n = Number(n || 0);
    if (n >= 1e9) return (n / 1e9).toFixed(2) + " B";
    if (n >= 1e6) return (n / 1e6).toFixed(2) + " M";
    if (n >= 1e4) return (n / 1e3).toFixed(1) + " k";
    return Math.round(n).toLocaleString("de-DE");
  };
  const usd = (n) => "$" + Number(n || 0).toFixed(Number(n) < 1 ? 4 : 2);
  const pct = (n) => (Number(n || 0) * 100).toFixed(1) + " %";
  const ago = (ms) => {
    if (!ms) return "–";
    const s = Math.max(0, (Date.now() - ms) / 1000);
    if (s < 60) return "gerade eben";
    if (s < 3600) return Math.floor(s / 60) + " min";
    if (s < 86400) return Math.floor(s / 3600) + " h";
    return Math.floor(s / 86400) + " d";
  };
  const until = (ms) => {
    if (!ms) return "";
    const s = Math.max(0, (ms - Date.now()) / 1000);
    if (s < 3600) return "in " + Math.ceil(s / 60) + " min";
    if (s < 86400) return "in " + Math.floor(s / 3600) + " h " + Math.floor((s % 3600) / 60) + " min";
    return "in " + Math.floor(s / 86400) + " d";
  };

  let toastT = 0;
  function toast(msg, err) {
    const t = $("#toast");
    t.textContent = msg;
    t.classList.toggle("err", !!err);
    t.classList.add("show");
    clearTimeout(toastT);
    toastT = setTimeout(() => t.classList.remove("show"), 3400);
  }

  async function api(path, opts) {
    opts = opts || {};
    const headers = { "x-multillm-admin": "1" };
    if (opts.body !== undefined) headers["Content-Type"] = "application/json";
    const r = await fetch(path, {
      method: opts.method || (opts.body !== undefined ? "POST" : "GET"),
      headers,
      credentials: "same-origin",
      body: opts.body === undefined ? undefined : JSON.stringify(opts.body),
    });
    let data = null;
    const ct = r.headers.get("content-type") || "";
    if (ct.includes("application/json")) { try { data = await r.json(); } catch (e) { data = null; } }
    else { data = await r.text(); }
    if (!r.ok) {
      const err = new Error((data && data.error && data.error.message) || ("HTTP " + r.status));
      err.status = r.status;
      err.code = data && data.error && data.error.code;
      if (r.status === 401 && !opts.noRedirect) { showLock(); }
      if (err.code === "password_change_required") { showLock(); }
      throw err;
    }
    return data;
  }

  function modal(title, body, foot, wide) {
    closeModal();
    const ov = document.createElement("div");
    ov.className = "overlay";
    ov.innerHTML = '<div class="dialog' + (wide ? " wide" : "") + '" role="dialog" aria-modal="true"><div class="dh"><h2>' + esc(title) +
      '</h2><button class="icon-btn" data-close aria-label="Schließen">&#10005;</button></div><div class="db"></div>' +
      (foot === false ? "" : '<div class="df"></div>') + "</div>";
    $(".db", ov).innerHTML = body;
    if (foot !== false) $(".df", ov).innerHTML = foot || '<button class="btn" data-close>Schließen</button>';
    $("#modal-root").appendChild(ov);
    ov.addEventListener("mousedown", (e) => { if (e.target === ov) closeModal(); });
    $$("[data-close]", ov).forEach((b) => b.addEventListener("click", closeModal));
    const f = $("input,select,textarea", ov); if (f) setTimeout(() => f.focus(), 30);
    return ov;
  }
  function closeModal() { $("#modal-root").innerHTML = ""; if (S.onClose) { const f = S.onClose; S.onClose = null; f(); } }
  document.addEventListener("keydown", (e) => { if (e.key === "Escape" && $("#modal-root").firstChild) closeModal(); });

  function confirmBox(title, text, okLabel, danger) {
    return new Promise((res) => {
      const ov = modal(title, "<p>" + esc(text) + "</p>",
        '<button class="btn" data-close>Abbrechen</button><button class="btn ' + (danger ? "danger" : "primary") + '" id="cf-ok">' + esc(okLabel || "OK") + "</button>");
      S.onClose = () => res(false);
      $("#cf-ok", ov).onclick = () => { S.onClose = null; closeModal(); res(true); };
    });
  }

  // Downloads go through fetch (management endpoints need the admin header).
  document.addEventListener("click", async (e) => {
    const b = e.target.closest("[data-dl]"); if (!b) return;
    try {
      const data = await api(b.dataset.dl);
      const blob = new Blob([typeof data === "string" ? data : JSON.stringify(data, null, 2)], { type: "application/json" });
      const a = document.createElement("a"); a.href = URL.createObjectURL(blob); a.download = b.dataset.name; a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 2000);
    } catch (x) { toast(x.message, true); }
  });

  function copy(text) {
    if (navigator.clipboard && window.isSecureContext) { navigator.clipboard.writeText(text).then(() => toast("Kopiert")); return; }
    const ta = document.createElement("textarea"); ta.value = text; document.body.appendChild(ta); ta.select();
    try { document.execCommand("copy"); toast("Kopiert"); } catch (e) { toast("Kopieren nicht möglich", true); }
    ta.remove();
  }

  /* ---- provider logos ---- */
  const LOGOS = ["302ai","ai21","aihubmix","aimlapi","akash","alephalpha","anthropic","aws","azure","baichuan","bigmodel","cerebras:ico","chutes","cohere:ico","cometapi","crusoe","deepinfra:ico","deepseek","featherless","fireworks:ico","gcp","gemini","gigachat","glama","gpt4all","groq","huawei","huggingface","hunyuan","hyperbolic","iflytek","ionos","jan","kilo:ico","kimi","koboldcpp","krutrim","lingyi","litellm","llamafile","lmstudio","localai","longcat","maritaca","martian","mimo","minimax","mistral","modelscope","nanogpt","naver","nebius","novita","nvidia","ollama","openai","opencode","openrouter","ovh","perplexity","portkey","ppio","qwen","reka","requesty","sakura","sambanova:ico","sarvam","scaleway","siliconflow","stackit","stepfun","tgwebui","together:ico","tokenhub","upstage","venice","vercel","vllm","volcark","xai","yandex","zai"];
  const ALIAS = { moonshot: "kimi", grok: "xai", google: "gemini", vertex: "gcp", bedrock: "aws", copilot: "openai", github: "openai", zhipu: "bigmodel", glm: "zai", "lm studio": "lmstudio" };
  function logoFor(name, url) {
    const hay = (String(name || "") + " " + String(url || "")).toLowerCase();
    for (const a in ALIAS) { if (hay.includes(a)) return logoFile(ALIAS[a]); }
    for (const l of LOGOS) { const id = l.split(":")[0]; if (hay.replace(/[^a-z0-9]/g, "").includes(id)) return logoFile(id); }
    return null;
  }
  function logoFile(id) { const e = LOGOS.find((l) => l.split(":")[0] === id); return e ? "/logos/" + id + "." + (e.includes(":ico") ? "ico" : "png") : null; }
  function logoHtml(name, url) {
    const f = logoFor(name, url);
    return '<div class="plogo">' + (f ? '<img src="' + f + '" alt="" loading="lazy">' : initial(name)) + "</div>";
  }

  /* ================= state ================= */
  const S = { user: null, info: null, page: "overview", providers: [], presets: null, onClose: null, timers: [] };

  /* ================= lock / sign-in ================= */
  let clockT = 0;
  function tick() {
    const d = new Date();
    $("#clock-time").textContent = d.toLocaleTimeString("de-DE", { hour: "2-digit", minute: "2-digit" });
    $("#clock-date").textContent = d.toLocaleDateString("de-DE", { weekday: "long", day: "numeric", month: "long" });
  }

  async function showLock() {
    S.user = null;
    $("#app").classList.add("hidden");
    closeModal();
    const lock = $("#lock");
    lock.classList.remove("hidden", "unlocked");
    $("#signin").classList.add("hidden");
    $("#lock-clock").classList.remove("hidden");
    tick(); clearInterval(clockT); clockT = setInterval(tick, 1000);
    $("#si-host").textContent = location.host;
    const unlock = () => { document.removeEventListener("keydown", unlock); lock.removeEventListener("click", unlock); openSignin(); };
    document.addEventListener("keydown", unlock);
    lock.addEventListener("click", unlock);
  }

  let tiles = [];
  let current = null; // username or "" for "other user"
  async function openSignin() {
    $("#lock").classList.add("unlocked");
    $("#signin").classList.remove("hidden");
    try { tiles = await api("/api/auth/tiles", { noRedirect: true }); } catch (e) { tiles = []; }
    let last = ""; try { last = localStorage.getItem("ml-last-user") || ""; } catch (e) { /* storage blocked */ }
    const box = $("#si-tiles");
    box.innerHTML = tiles.map((t) => '<button class="si-tile" data-u="' + esc(t.username) + '"><span class="avatar">' + initial(t.displayName || t.username) +
      "</span><span>" + esc(t.displayName || t.username) + "</span></button>").join("") +
      '<button class="si-tile" data-u=""><span class="avatar">+</span><span>Anderer Benutzer</span></button>';
    $$(".si-tile", box).forEach((b) => b.onclick = () => pickUser(b.dataset.u));
    const start = tiles.find((t) => t.username === last) || tiles[0];
    pickUser(start ? start.username : "");
  }

  function pickUser(u) {
    current = u;
    resetSigninUi();
    $$(".si-tile").forEach((b) => b.classList.toggle("active", b.dataset.u === u));
    const t = tiles.find((x) => x.username === u);
    $("#si-avatar").innerHTML = u ? initial(t && t.displayName || u) : "&#9787;";
    $("#si-name").textContent = u ? (t && t.displayName || u) : "Anderer Benutzer";
    $("#si-user").classList.toggle("hidden", !!u);
    $("#si-user").value = "";
    $("#si-pass").value = "";
    setTimeout(() => (u ? $("#si-pass") : $("#si-user")).focus(), 30);
  }

  function resetSigninUi() {
    $("#si-form").classList.remove("hidden");
    $("#si-msg").classList.add("hidden");
    $("#si-ok").classList.add("hidden");
    $("#si-change").classList.add("hidden");
    $("#si-spinner").classList.add("hidden");
    $("#si-tiles").classList.remove("hidden");
  }

  function signinError(msg) {
    $("#si-spinner").classList.add("hidden");
    $("#si-form").classList.add("hidden");
    $("#si-user").classList.add("hidden");
    const m = $("#si-msg"); m.textContent = msg; m.classList.remove("hidden");
    const ok = $("#si-ok"); ok.classList.remove("hidden"); ok.focus();
    ok.onclick = () => pickUser(current);
  }

  $("#si-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const username = current || $("#si-user").value.trim();
    const password = $("#si-pass").value;
    if (!username) { $("#si-user").focus(); return; }
    $("#si-form").classList.add("hidden");
    $("#si-user").classList.add("hidden");
    $("#si-spinner").classList.remove("hidden");
    const welcome = $("#si-msg"); welcome.textContent = "Willkommen"; welcome.classList.remove("hidden");
    try {
      const r = await api("/api/auth/login", { body: { username, password }, noRedirect: true });
      try { localStorage.setItem("ml-last-user", r.user.username); } catch (e2) { /* ignore */ }
      if (r.user.mustChangePassword) { askNewPassword(r.user, password); return; }
      enterApp(r.user);
    } catch (err) {
      signinError(err.status === 401 && !/locked/.test(err.message) ? "Das Kennwort ist falsch. Versuchen Sie es erneut." : err.message);
    }
  });

  function strength(pw) {
    let s = 0;
    if (pw.length >= 8) s++; if (pw.length >= 12) s++;
    if (/[A-Z]/.test(pw) && /[a-z]/.test(pw)) s++;
    if (/\d/.test(pw)) s++; if (/[^A-Za-z0-9]/.test(pw)) s++;
    return Math.min(s, 4);
  }
  function bindMeter(input, bar) {
    input.addEventListener("input", () => {
      const s = strength(input.value);
      bar.style.width = (s * 25) + "%";
      bar.style.background = ["#ef4444", "#ef4444", "#f59e0b", "#84cc16", "#22c55e"][s];
    });
  }

  function askNewPassword(user, oldPw) {
    $("#si-spinner").classList.add("hidden");
    $("#si-msg").classList.add("hidden");
    $("#si-tiles").classList.add("hidden");
    const box = $("#si-change"); box.classList.remove("hidden");
    $("#ch-new").value = ""; $("#ch-new2").value = "";
    bindMeter($("#ch-new"), $("#ch-meter"));
    setTimeout(() => $("#ch-new").focus(), 30);
    const go = async () => {
      const a = $("#ch-new").value, b = $("#ch-new2").value;
      if (a !== b) { toast("Die Kennwörter stimmen nicht überein.", true); return; }
      try {
        const r = await api("/api/auth/password", { body: { current: oldPw, new: a }, noRedirect: true });
        toast("Kennwort geändert");
        enterApp(r.user);
      } catch (err) { toast(err.message, true); }
    };
    $("#ch-go").onclick = go;
    $("#ch-new2").onkeydown = (e) => { if (e.key === "Enter") go(); };
  }

  function enterApp(user) {
    S.user = user;
    clearInterval(clockT);
    $("#lock").classList.add("hidden");
    $("#app").classList.remove("hidden");
    $("#me-avatar").innerHTML = initial(user.displayName || user.username);
    $("#me-name").textContent = user.displayName || user.username;
    $("#me-role").textContent = user.is_admin ? "Administrator" : "Benutzer";
    $$(".admin-only").forEach((b) => b.classList.toggle("hidden", !user.is_admin));
    const fromHash = location.hash.replace("#", "");
    go(PAGES[fromHash] ? fromHash : S.page);
  }

  /* ================= navigation ================= */
  $$("#nav button").forEach((b) => b.addEventListener("click", () => go(b.dataset.page)));
  $("#me-btn").addEventListener("click", accountDialog);

  function go(page) {
    if (!PAGES[page]) page = "overview";
    S.page = page;
    history.replaceState(null, "", "#" + page);
    $$("#nav button").forEach((b) => b.classList.toggle("active", b.dataset.page === page));
    S.timers.forEach(clearInterval); S.timers = [];
    $("#main").innerHTML = '<div class="empty">Lädt …</div>';
    PAGES[page]().catch((e) => { if (e.status !== 401) $("#main").innerHTML = '<div class="empty"><b>Fehler</b>' + esc(e.message) + "</div>"; });
  }

  function head(title, sub, actions) {
    return '<div class="page-head"><div><h1>' + esc(title) + "</h1>" + (sub ? "<p>" + sub + "</p>" : "") + '</div><div class="row">' + (actions || "") + "</div></div>";
  }

  /* ================= charts (inline SVG) ================= */
  function areaChart(series, keys, colors, opts) {
    opts = opts || {};
    const W = 800, H = 220, P = { l: 44, r: 10, t: 10, b: 24 };
    const n = series.length; if (!n) return "";
    const tot = series.map((r) => keys.reduce((a, k) => a + Number(r[k] || 0), 0));
    const max = Math.max(1, ...tot);
    const x = (i) => P.l + (n === 1 ? 0 : (i / (n - 1)) * (W - P.l - P.r));
    const y = (v) => H - P.b - (v / max) * (H - P.t - P.b);
    let svg = '<svg class="chart" viewBox="0 0 ' + W + " " + H + '" preserveAspectRatio="none">';
    for (let g = 0; g <= 4; g++) {
      const v = (max * g) / 4, yy = y(v);
      svg += '<line x1="' + P.l + '" x2="' + (W - P.r) + '" y1="' + yy + '" y2="' + yy + '" stroke="currentColor" opacity=".08"/>';
      svg += '<text x="' + (P.l - 6) + '" y="' + (yy + 3) + '" text-anchor="end">' + esc(opts.money ? "$" + v.toFixed(2) : fmt(v)) + "</text>";
    }
    let base = series.map(() => 0);
    keys.forEach((k, ki) => {
      const top = series.map((r, i) => base[i] + Number(r[k] || 0));
      let d = "M" + x(0) + "," + y(base[0]);
      top.forEach((v, i) => { d += " L" + x(i) + "," + y(v); });
      for (let i = n - 1; i >= 0; i--) d += " L" + x(i) + "," + y(base[i]);
      svg += '<path d="' + d + 'Z" fill="' + colors[ki] + '" fill-opacity=".28" stroke="none"/>';
      let line = ""; top.forEach((v, i) => { line += (i ? " L" : "M") + x(i) + "," + y(v); });
      svg += '<path d="' + line + '" fill="none" stroke="' + colors[ki] + '" stroke-width="2"/>';
      base = top;
    });
    const step = Math.max(1, Math.ceil(n / 8));
    series.forEach((r, i) => {
      if (i % step) return;
      const d = new Date(r.t);
      const lbl = opts.bucketMs >= 86400000 ? d.toLocaleDateString("de-DE", { day: "2-digit", month: "2-digit" }) : d.toLocaleTimeString("de-DE", { hour: "2-digit", minute: "2-digit" });
      svg += '<text x="' + x(i) + '" y="' + (H - 6) + '" text-anchor="middle">' + lbl + "</text>";
    });
    return svg + "</svg>";
  }

  function delta(cur, prev, invert) {
    if (!prev) return '<div class="d dim">–</div>';
    const ch = (cur - prev) / prev;
    const good = invert ? ch < 0 : ch > 0;
    return '<div class="d ' + (good ? "up" : "down") + '">' + (ch > 0 ? "▲ " : "▼ ") + Math.abs(ch * 100).toFixed(0) + " % vs. Vorperiode</div>";
  }

  /* ================= pages ================= */
  const PAGES = {};

  PAGES.overview = async function () {
    const [an, st, lim] = await Promise.all([api("/api/analytics?range=24h"), api("/api/status"), api("/api/limits")]);
    const t = an.totals, p = an.previous;
    const tok = t.inputTokens + t.outputTokens, ptok = p.inputTokens + p.outputTokens;
    const warn = lim.providers.filter((x) => x.exhausted || (x.local || []).some((l) => l.usedPercent >= 80));
    let html = head("Übersicht", "Letzte 24 Stunden · " + st.providers + " Provider · " + st.modelsEnabled + " von " + st.modelsTotal + " Modellen aktiv",
      '<button class="btn" id="ov-refresh">Aktualisieren</button>');
    if (!st.providers) {
      html += '<div class="card empty"><b>Willkommen bei Multi LLM</b>Verbinde deinen ersten Provider – per OAuth (GitHub Copilot, OpenRouter, Qwen) oder mit API-Key.<br><br><button class="btn primary" id="ov-add">Provider verbinden</button></div>';
      $("#main").innerHTML = html;
      $("#ov-add").onclick = () => { go("providers"); setTimeout(addProviderDialog, 50); };
      return;
    }
    html += '<div class="grid g4">' +
      stat("Anfragen", fmt(t.ok), delta(t.ok, p.ok)) +
      stat("Tokens", fmt(tok), delta(tok, ptok)) +
      stat("Kosten (geschätzt)", usd(t.costUsd), delta(t.costUsd, p.costUsd, true)) +
      stat("Fehlerquote", pct(t.errorRate), delta(t.errorRate, p.errorRate, true)) + "</div>";
    if (warn.length) {
      html += '<div class="card" style="margin-top:16px;border-color:rgba(245,158,11,.4)"><h3>Limits</h3>' + warn.map((w) =>
        '<div class="row"><span class="pill ' + (w.exhausted ? "bad" : "warn") + '">' + (w.exhausted ? "erschöpft" : "≥ 80 %") + "</span><b>" + esc(w.providerId) + "</b></div>").join("") + "</div>";
    }
    html += '<div class="grid g2" style="margin-top:16px"><div class="card"><div class="row"><h3 class="grow">Tokens</h3><div class="legend"><span><i style="background:#38d5c8"></i>Input</span><span><i style="background:#7c8cff"></i>Output</span></div></div>' +
      areaChart(an.series, ["inputTokens", "outputTokens"], ["#38d5c8", "#7c8cff"], { bucketMs: an.bucketMs }) + "</div>" +
      '<div class="card"><div class="row"><h3 class="grow">Anfragen</h3><div class="legend"><span><i style="background:#22c55e"></i>OK</span><span><i style="background:#ef4444"></i>Fehler</span></div></div>' +
      areaChart(an.series, ["ok", "failed"], ["#22c55e", "#ef4444"], { bucketMs: an.bucketMs }) + "</div></div>";
    html += '<div class="grid g2" style="margin-top:16px"><div class="card"><h3>Provider</h3>' + providerTable(an.providers) + "</div>" +
      '<div class="card"><h3>Top-Modelle</h3>' + modelTable(an.models.slice(0, 8), true) + "</div></div>";
    $("#main").innerHTML = html;
    $("#ov-refresh").onclick = () => go("overview");
    S.timers.push(setInterval(() => { if (S.page === "overview" && !$("#modal-root").firstChild) go("overview"); }, 30000));
  };

  function stat(l, v, d) { return '<div class="card stat"><div class="l">' + esc(l) + '</div><div class="v">' + v + "</div>" + (d || "") + "</div>"; }

  function providerTable(rows) {
    if (!rows.length) return '<div class="empty">Noch keine Anfragen.</div>';
    return "<table><tr><th>Provider</th><th class=num>Anfragen</th><th class=num>Tokens</th><th class=num>Fehler</th><th class=num>Kosten</th></tr>" +
      rows.map((r) => "<tr><td>" + (r.circuitOpen ? '<span class="dot bad"></span> ' : '<span class="dot ok"></span> ') + esc(r.providerId) +
        "</td><td class=num>" + fmt(r.ok) + "</td><td class=num>" + fmt(r.inputTokens + r.outputTokens) + "</td><td class=num>" + pct(r.errorRate) +
        "</td><td class=num>" + usd(r.costUsd) + "</td></tr>").join("") + "</table>";
  }

  function modelTable(rows, compact) {
    if (!rows.length) return '<div class="empty">Noch keine Anfragen.</div>';
    return "<table><tr><th>Modell</th><th class=num>Anfragen</th><th class=num>Tokens</th>" + (compact ? "" : "<th class=num>Input</th><th class=num>Output</th>") +
      "<th class=num>p50</th>" + (compact ? "" : "<th class=num>p95</th><th class=num>Fehler</th>") + "<th class=num>Kosten</th></tr>" +
      rows.map((r) => {
        const l = r.latencyMs || {};
        return "<tr><td><b>" + esc(r.modelId) + '</b><div class="dim">' + esc(r.providerId) + "</div></td><td class=num>" + fmt(r.ok) +
          "</td><td class=num>" + fmt(r.inputTokens + r.outputTokens) + "</td>" +
          (compact ? "" : "<td class=num>" + fmt(r.inputTokens) + "</td><td class=num>" + fmt(r.outputTokens) + "</td>") +
          "<td class=num>" + (l.p50 != null ? l.p50 + " ms" : "–") + "</td>" +
          (compact ? "" : "<td class=num>" + (l.p95 != null ? l.p95 + " ms" : "–") + "</td><td class=num>" + pct(r.errorRate) + "</td>") +
          "<td class=num>" + usd(r.costUsd) + "</td></tr>";
      }).join("") + "</table>";
  }

  /* ---------- Providers ---------- */
  PAGES.providers = async function () {
    const [full, health] = await Promise.all([api("/api/providers/full"), api("/api/health").catch(() => ({}))]);
    S.providers = full.providers;
    let html = head("Provider", "Verbinde Anbieter per OAuth oder API-Key. Schlüssel bleiben auf dem Server und sind pro Benutzer getrennt.",
      '<button class="btn" id="pv-check">Alle prüfen</button><button class="btn primary" id="pv-add">+ Provider</button>');
    if (!S.providers.length) {
      html += '<div class="card empty"><b>Noch keine Provider</b>Füge deinen ersten Anbieter hinzu.</div>';
    } else {
      const open = (health && health.circuit) || {};
      html += '<div class="pgrid">' + S.providers.map((p) => {
        const en = p.models.filter((m) => m.enabled).length;
        const stat = open[p.id] ? '<span class="pill bad">Pausiert</span>' : p.status === "ok" ? '<span class="pill ok">Online</span>' : p.status === "error" ? '<span class="pill bad">Fehler</span>' : '<span class="pill">Ungeprüft</span>';
        return '<div class="pcard"><div class="row">' + logoHtml(p.id, p.baseUrl) + '<div class="grow"><div class="ptitle">' + esc(p.id) + '</div><div class="psub">' + esc(p.baseUrl) +
          "</div></div>" + stat + '</div><div class="row">' + (p.oauth ? '<span class="pill acc">OAuth · ' + esc(p.oauth) + "</span>" : p.hasKey ? '<span class="pill">API-Key</span>' : '<span class="pill warn">Kein Key</span>') +
          '<span class="pill">' + en + "/" + p.models.length + " Modelle</span>" + (p.limits ? '<span class="pill">Limits</span>' : "") +
          '</div><div class="row"><button class="btn sm" data-edit="' + esc(p.id) + '">Bearbeiten</button>' +
          (open[p.id] ? '<button class="btn sm" data-reset="' + esc(p.id) + '">Reaktivieren</button>' : "") +
          '<span class="grow"></span><button class="btn sm danger" data-del="' + esc(p.id) + '">Entfernen</button></div></div>';
      }).join("") + "</div>";
    }
    $("#main").innerHTML = html;
    $("#pv-add").onclick = addProviderDialog;
    $("#pv-check").onclick = async () => { toast("Prüfe Provider …"); go("providers"); };
    $$("[data-edit]").forEach((b) => b.onclick = () => editProviderDialog(S.providers.find((p) => p.id === b.dataset.edit)));
    $$("[data-reset]").forEach((b) => b.onclick = async () => { await api("/api/health/reset", { body: { providerId: b.dataset.reset } }); toast("Reaktiviert"); go("providers"); });
    $$("[data-del]").forEach((b) => b.onclick = async () => {
      if (!(await confirmBox("Provider entfernen", "„" + b.dataset.del + "“ inklusive Schlüssel und OAuth-Verbindung entfernen?", "Entfernen", true))) return;
      await api("/api/providers/" + encodeURIComponent(b.dataset.del), { method: "DELETE" });
      toast("Entfernt"); go("providers");
    });
  };

  async function presets() {
    if (!S.presets) S.presets = await api("/api/presets");
    if (!S.info) S.info = await api("/api/server-info");
    return S.presets;
  }

  async function addProviderDialog() {
    const list = await presets();
    const oauth = (S.info && S.info.oauthProviders) || [];
    const body = '<p class="muted">Mit Konto verbinden (kein API-Key nötig):</p><div class="oauth-row">' +
      oauth.map((o) => '<button class="oauth-btn" data-oauth="' + esc(o.kind) + '">' + logoHtml(o.name) + "<span><b>" + esc(o.name) + "</b><small>" + esc(o.limits) + "</small></span></button>").join("") +
      '</div><div class="row"><p class="muted grow" style="margin:0">Oder Anbieter mit API-Key:</p><input class="search" id="ps-q" placeholder="Suchen …"></div><div style="height:10px"></div>' +
      '<div class="preset-grid" id="ps-grid"></div>';
    const ov = modal("Provider hinzufügen", body, '<button class="btn" data-close>Abbrechen</button><button class="btn" id="ps-custom">Eigener Endpunkt</button>', true);
    const grid = $("#ps-grid", ov);
    const render = (q) => {
      q = (q || "").toLowerCase();
      grid.innerHTML = list.filter((p) => !q || p.name.toLowerCase().includes(q) || p.url.toLowerCase().includes(q)).map((p, i) =>
        '<button class="preset" data-i="' + list.indexOf(p) + '">' + logoHtml(p.name, p.url) + "<span>" + esc(p.name) + "</span></button>").join("");
      $$(".preset", grid).forEach((b) => b.onclick = () => { const p = list[+b.dataset.i]; editProviderDialog(null, p); });
    };
    render("");
    $("#ps-q", ov).oninput = (e) => render(e.target.value);
    $("#ps-custom", ov).onclick = () => editProviderDialog(null, { name: "", url: "", format: "openai" });
    $$("[data-oauth]", ov).forEach((b) => b.onclick = () => oauthDialog(b.dataset.oauth));
  }

  async function oauthDialog(kind) {
    let r;
    try { r = await api("/api/oauth/start", { body: { kind } }); } catch (e) { toast(e.message, true); return; }
    const name = ((S.info.oauthProviders || []).find((o) => o.kind === kind) || {}).name || kind;
    let body;
    if (r.mode === "device") {
      body = '<p>1. Öffne <a href="' + esc(r.url) + '" target="_blank" rel="noopener">' + esc(r.url) + "</a></p><p>2. Gib diesen Code ein:</p>" +
        '<div class="device-code" id="oa-code">' + esc(r.userCode || "") + '</div><div class="row"><button class="btn sm" id="oa-copy">Code kopieren</button><span class="grow"></span><span class="dots" style="filter:invert(var(--inv,0))"><i style="background:var(--accent)"></i><i style="background:var(--accent)"></i><i style="background:var(--accent)"></i></span><span class="muted" id="oa-st">Warte auf Bestätigung …</span></div>';
    } else {
      body = '<p>Du wirst zu ' + esc(name) + ' weitergeleitet und kommst danach automatisch zurück.</p><p><a class="btn primary" href="' + esc(r.url) + '" target="_blank" rel="noopener">Bei ' + esc(name) + " anmelden</a></p>" +
        '<p class="muted">Klappt die Rückleitung nicht (z. B. anderer Hostname), kopiere die Adresse aus der Adressleiste hierher:</p><div class="copy"><input id="oa-paste" placeholder="http://…/oauth/callback/…?code=…"><button class="btn" id="oa-submit">Übernehmen</button></div>' +
        '<p class="muted" id="oa-st">Warte auf Anmeldung …</p>';
    }
    const ov = modal("Mit " + name + " verbinden", body);
    if (r.mode === "device") {
      $("#oa-copy", ov).onclick = () => copy(r.userCode);
      window.open(r.url, "_blank", "noopener");
    } else {
      $("#oa-submit", ov).onclick = async () => {
        try { await api("/api/oauth/complete", { body: { flowId: r.flowId, code: $("#oa-paste", ov).value } }); } catch (e) { toast(e.message, true); }
      };
    }
    const poll = setInterval(async () => {
      if (!document.body.contains(ov)) { clearInterval(poll); return; }
      try {
        const s = await api("/api/oauth/status/" + encodeURIComponent(r.flowId));
        if (s.status === "done") { clearInterval(poll); closeModal(); toast(s.message || "Verbunden"); go("providers"); }
        if (s.status === "error") { clearInterval(poll); const el = $("#oa-st", ov); if (el) el.textContent = "Fehler: " + (s.message || ""); }
      } catch (e) { clearInterval(poll); }
    }, 2000);
  }

  function slug(s) { return String(s || "").toLowerCase().replace(/\(.*?\)/g, "").trim().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 40) || "provider"; }

  function editProviderDialog(p, preset) {
    const isNew = !p;
    p = p || { id: slug(preset && preset.name), baseUrl: (preset && preset.url) || "", apiFormat: (preset && preset.format) || "openai", models: [], hasKey: false };
    let models = p.models.map((m) => Object.assign({}, m));
    const body = '<div class="grid" style="grid-template-columns:1fr 1fr;gap:10px"><div><label>ID</label><input id="pe-id" value="' + esc(p.id) + '"></div>' +
      '<div><label>API-Format</label><select id="pe-fmt"><option value="openai">OpenAI-kompatibel</option><option value="anthropic">Anthropic</option></select></div></div>' +
      '<label>Basis-URL</label><input id="pe-url" value="' + esc(p.baseUrl) + '" placeholder="https://api.example.com/v1"' + (p.oauth ? " disabled" : "") + ">" +
      (p.oauth ? '<p class="muted">Verbunden per OAuth (' + esc(p.oauth) + ")</p>" :
        '<label>API-Key ' + (p.hasKey ? '<span class="dim">(gespeichert – leer lassen, um ihn zu behalten)</span>' : "") + '</label><input id="pe-key" type="password" autocomplete="off" placeholder="sk-…">') +
      '<div class="row" style="margin-top:14px"><b class="grow">Modelle</b><button class="btn sm" id="pe-fetch">Vom Provider laden</button><button class="btn sm" id="pe-all">Alle an</button><button class="btn sm" id="pe-none">Alle aus</button></div>' +
      '<div class="copy" style="margin-top:8px"><input id="pe-mid" placeholder="Modell-ID manuell hinzufügen"><button class="btn" id="pe-madd">+</button></div>' +
      '<input id="pe-mq" placeholder="Modelle filtern …" style="margin-top:8px"><div id="pe-models" style="max-height:34vh;overflow:auto;margin-top:6px"></div>';
    const ov = modal(isNew ? "Provider hinzufügen" : "Provider bearbeiten", body, '<button class="btn" data-close>Abbrechen</button><button class="btn primary" id="pe-save">Speichern</button>', true);
    $("#pe-fmt", ov).value = p.apiFormat || "openai";
    const box = $("#pe-models", ov);
    const render = () => {
      const q = ($("#pe-mq", ov).value || "").toLowerCase();
      const vis = models.filter((m) => !q || m.id.toLowerCase().includes(q));
      box.innerHTML = vis.length ? "<table>" + vis.map((m) => '<tr><td style="width:30px"><input type="checkbox" data-m="' + esc(m.id) + '"' + (m.enabled ? " checked" : "") +
        "></td><td>" + esc(m.id) + "</td><td class=num dim>" + (m.contextLength ? fmt(m.contextLength) + " ctx" : "") + "</td></tr>").join("") + "</table>" : '<p class="dim">Keine Modelle – lade sie vom Provider oder füge sie manuell hinzu.</p>';
      $$("[data-m]", box).forEach((c) => c.onchange = () => { const m = models.find((x) => x.id === c.dataset.m); if (m) m.enabled = c.checked; });
    };
    render();
    $("#pe-mq", ov).oninput = render;
    $("#pe-all", ov).onclick = () => { models.forEach((m) => m.enabled = true); render(); };
    $("#pe-none", ov).onclick = () => { models.forEach((m) => m.enabled = false); render(); };
    $("#pe-madd", ov).onclick = () => { const id = $("#pe-mid", ov).value.trim(); if (id && !models.some((m) => m.id === id)) { models.push({ id, name: id, enabled: true }); render(); } $("#pe-mid", ov).value = ""; };
    $("#pe-fetch", ov).onclick = async (e) => {
      e.target.disabled = true; e.target.textContent = "Lädt …";
      try {
        const list = await api("/api/providers/fetch", { body: { baseUrl: $("#pe-url", ov).value, apiKey: ($("#pe-key", ov) || {}).value || null, providerId: isNew ? null : p.id, apiFormat: $("#pe-fmt", ov).value } });
        const old = new Map(models.map((m) => [m.id, m]));
        const few = list.length <= 40;
        models = list.map((m) => Object.assign({ enabled: old.has(m.id) ? old.get(m.id).enabled : few }, old.get(m.id) || {}, m, { enabled: old.has(m.id) ? old.get(m.id).enabled : few }));
        toast(list.length + " Modelle geladen");
        render();
      } catch (err) { toast(err.message, true); }
      e.target.disabled = false; e.target.textContent = "Vom Provider laden";
    };
    $("#pe-save", ov).onclick = async () => {
      const keyEl = $("#pe-key", ov);
      const payload = {
        originalId: isNew ? null : p.id, id: $("#pe-id", ov).value.trim().toLowerCase(), baseUrl: $("#pe-url", ov).value.trim(),
        apiKey: keyEl && keyEl.value ? keyEl.value : null, apiFormat: $("#pe-fmt", ov).value,
        models: models.map((m) => ({ id: m.id, name: m.name || m.id, enabled: !!m.enabled, contextLength: m.contextLength || null, starred: !!m.starred,
          inputPricePerMtok: m.inputPricePerMtok == null ? null : m.inputPricePerMtok, outputPricePerMtok: m.outputPricePerMtok == null ? null : m.outputPricePerMtok })),
      };
      try { await api("/api/providers", { body: payload }); closeModal(); toast("Gespeichert"); go("providers"); }
      catch (err) { toast(err.message, true); }
    };
  }

  /* ---------- Models ---------- */
  PAGES.models = async function () {
    const full = await api("/api/providers/full");
    const rows = [];
    full.providers.forEach((p) => p.models.forEach((m) => rows.push({ p, m })));
    let html = head("Modelle", "Aktive Modelle stehen in API, CLI (/model) und Routing zur Verfügung. Sterne bevorzugen ein Modell beim gewichteten Routing.",
      '<input class="search" id="md-q" placeholder="Suchen …"><div class="tabs" id="md-f"><button data-f="all" class="on">Alle</button><button data-f="on">Aktiv</button><button data-f="off">Inaktiv</button></div>');
    html += '<div class="card"><div id="md-list"></div></div>';
    $("#main").innerHTML = html;
    let filter = "all";
    const render = () => {
      const q = ($("#md-q").value || "").toLowerCase();
      const vis = rows.filter((r) => (filter === "all" || (filter === "on") === r.m.enabled) && (!q || (r.p.id + "/" + r.m.id).toLowerCase().includes(q)));
      $("#md-list").innerHTML = !vis.length ? '<div class="empty">Keine Modelle.</div>' : "<table><tr><th></th><th>Modell</th><th>Provider</th><th class=num>Kontext</th><th class=num>$/M in</th><th class=num>$/M out</th><th>Aktiv</th></tr>" +
        vis.slice(0, 500).map((r) => '<tr><td><button class="icon-btn" data-star="' + esc(r.p.id + "\u0000" + r.m.id) + '" title="Favorit">' + (r.m.starred ? "&#9733;" : "&#9734;") + "</button></td><td><b>" + esc(r.m.id) + "</b></td><td>" + esc(r.p.id) +
          "</td><td class=num>" + (r.m.contextLength ? fmt(r.m.contextLength) : "–") + "</td><td class=num>" + (r.m.inputPricePerMtok != null ? r.m.inputPricePerMtok.toFixed(2) : "–") +
          "</td><td class=num>" + (r.m.outputPricePerMtok != null ? r.m.outputPricePerMtok.toFixed(2) : "–") + '</td><td><input type="checkbox" data-en="' + esc(r.p.id + "\u0000" + r.m.id) + '"' + (r.m.enabled ? " checked" : "") + "></td></tr>").join("") + "</table>" +
        (vis.length > 500 ? '<p class="dim">' + (vis.length - 500) + " weitere – Suche verfeinern.</p>" : "");
      $$("[data-en]").forEach((c) => c.onchange = async () => {
        const [pid, mid] = c.dataset.en.split("\u0000");
        try { await api("/api/models/update", { body: { providerId: pid, modelId: mid, enabled: c.checked } }); rows.find((r) => r.p.id === pid && r.m.id === mid).m.enabled = c.checked; }
        catch (e) { toast(e.message, true); c.checked = !c.checked; }
      });
      $$("[data-star]").forEach((b) => b.onclick = async () => {
        const [pid, mid] = b.dataset.star.split("\u0000");
        const r = rows.find((x) => x.p.id === pid && x.m.id === mid);
        await api("/api/models/update", { body: { providerId: pid, modelId: mid, starred: !r.m.starred } });
        r.m.starred = !r.m.starred; render();
      });
    };
    $("#md-q").oninput = render;
    $$("#md-f button").forEach((b) => b.onclick = () => { filter = b.dataset.f; $$("#md-f button").forEach((x) => x.classList.toggle("on", x === b)); render(); });
    render();
  };

  /* ---------- Routing ---------- */
  const STRATEGIES = [
    ["auto", "Auto (ausgewogen)", "Bewertet Gesundheit, Latenz und Limit-Spielraum pro Anfrage."],
    ["weighted", "Gewichtet", "Zufällig, Favoriten (★) werden 4× so oft gewählt."],
    ["round_robin", "Round Robin", "Gleichmäßige Rotation über alle Modelle."],
    ["priority", "Qualität", "Geringste Fehlerquote (24 h), dann geringste Latenz."],
    ["latency", "Latenz", "Niedrigste gemessene Durchschnittslatenz zuerst."],
    ["fastest", "Schnellste", "Aktuelle Latenz (EMA), gewichtet mit Gesundheit."],
    ["cost", "Günstigste", "Niedrigster Preis pro Million Tokens zuerst."],
    ["least_used", "Kontingent schonen", "Provider mit den wenigsten Tokens der letzten 5 h zuerst."],
    ["sticky", "Sticky", "Eine Unterhaltung bleibt beim Modell, das sie begonnen hat."],
  ];
  PAGES.routing = async function () {
    const [set, virt, full] = await Promise.all([api("/api/settings"), api("/api/virtual"), api("/api/providers/full")]);
    const cur = (set.routing && set.routing.strategy) || "weighted";
    let html = head("Routing", "Das virtuelle Modell <code>multillm</code> und eigene Bündel verteilen Anfragen automatisch. Fällt ein Provider aus oder erreicht sein Limit, übernimmt der nächste.",
      '<button class="btn primary" id="vm-add">+ Virtuelles Modell</button>');
    html += '<div class="card"><h3>Globale Strategie</h3><div class="strategy-grid">' + STRATEGIES.map((s) =>
      '<button class="strategy' + (s[0] === cur ? " on" : "") + '" data-s="' + s[0] + '"><b>' + esc(s[1]) + "</b><small>" + esc(s[2]) + "</small></button>").join("") + "</div></div>";
    html += '<div class="card" style="margin-top:16px"><h3>Virtuelle Modelle</h3>' + (virt.virtualModels.length ? "<table><tr><th>Name</th><th>Modelle</th><th>Strategie</th><th></th></tr>" +
      virt.virtualModels.map((v) => "<tr><td><b>" + esc(v.id) + '</b></td><td><div class="chips">' + v.models.slice(0, 6).map((m) => '<span class="chip">' + esc(m) + "</span>").join("") +
        (v.models.length > 6 ? '<span class="chip">+' + (v.models.length - 6) + "</span>" : "") + "</div></td><td>" + esc((v.policy && v.policy.strategy) || "global") +
        '</td><td class=num><button class="btn sm" data-vedit="' + esc(v.id) + '">Bearbeiten</button> <button class="btn sm danger" data-vdel="' + esc(v.id) + '">Löschen</button></td></tr>').join("") + "</table>"
      : '<div class="empty">Noch keine Bündel. Ohne Bündel nutzt <code>multillm</code> alle aktiven Modelle.</div>') + "</div>";
    html += '<div class="card" style="margin-top:16px"><h3>Ausfallsicherheit</h3>' +
      '<div class="switch"><div>Circuit Breaker<small>Pausiert Provider nach ' + esc(set.circuitBreaker.threshold) + " Fehlern für " + esc(set.circuitBreaker.cooldownSecs) + ' s.</small></div><input type="checkbox" id="cb-on"' + (set.circuitBreaker.enabled ? " checked" : "") + "></div>" +
      '<div class="switch"><div>Max. Versuche pro Anfrage<small>0 = alle Kandidaten probieren.</small></div><input type="number" id="fb-max" style="width:90px" min="0" value="' + esc(set.failoverBudget.maxAttempts) + '"></div></div>';
    $("#main").innerHTML = html;
    const save = async (patch) => { try { await api("/api/settings", { body: patch }); toast("Gespeichert"); } catch (e) { toast(e.message, true); } };
    $$(".strategy").forEach((b) => b.onclick = async () => {
      $$(".strategy").forEach((x) => x.classList.toggle("on", x === b));
      await save({ routing: Object.assign({}, set.routing, { strategy: b.dataset.s }) });
    });
    $("#cb-on").onchange = (e) => save({ circuitBreaker: Object.assign({}, set.circuitBreaker, { enabled: e.target.checked }) });
    $("#fb-max").onchange = (e) => save({ failoverBudget: Object.assign({}, set.failoverBudget, { maxAttempts: Math.max(0, parseInt(e.target.value || "0", 10)) }) });
    const allModels = []; full.providers.forEach((p) => p.models.filter((m) => m.enabled).forEach((m) => allModels.push(p.id + "::" + m.id)));
    $("#vm-add").onclick = () => virtualDialog(null, allModels);
    $$("[data-vedit]").forEach((b) => b.onclick = () => virtualDialog(virt.virtualModels.find((v) => v.id === b.dataset.vedit), allModels));
    $$("[data-vdel]").forEach((b) => b.onclick = async () => {
      if (!(await confirmBox("Löschen", "Virtuelles Modell „" + b.dataset.vdel + "“ löschen?", "Löschen", true))) return;
      await api("/api/virtual/" + encodeURIComponent(b.dataset.vdel), { method: "DELETE" }); go("routing");
    });
  };

  function virtualDialog(v, allModels) {
    const sel = new Set(v ? v.models : []);
    const body = '<label>Name (so heißt das Modell in der API / CLI)</label><input id="vm-id" value="' + esc(v ? v.id : "") + '" placeholder="z. B. coding">' +
      '<label>Strategie</label><select id="vm-s"><option value="">Global</option>' + STRATEGIES.map((s) => '<option value="' + s[0] + '">' + esc(s[1]) + "</option>").join("") + "</select>" +
      '<label>Modelle</label><input id="vm-q" placeholder="Filtern …"><div id="vm-list" style="max-height:40vh;overflow:auto;margin-top:6px"></div>';
    const ov = modal(v ? "Virtuelles Modell bearbeiten" : "Virtuelles Modell", body, '<button class="btn" data-close>Abbrechen</button><button class="btn primary" id="vm-save">Speichern</button>', true);
    $("#vm-s", ov).value = (v && v.policy && v.policy.strategy) || "";
    const render = () => {
      const q = $("#vm-q", ov).value.toLowerCase();
      $("#vm-list", ov).innerHTML = allModels.filter((m) => !q || m.toLowerCase().includes(q)).map((m) =>
        '<label class="row" style="margin:0;padding:5px 0;color:var(--text)"><input type="checkbox" value="' + esc(m) + '"' + (sel.has(m) ? " checked" : "") + "> " + esc(m) + "</label>").join("") || '<p class="dim">Keine aktiven Modelle.</p>';
      $$("input[type=checkbox]", $("#vm-list", ov)).forEach((c) => c.onchange = () => { c.checked ? sel.add(c.value) : sel.delete(c.value); });
    };
    render(); $("#vm-q", ov).oninput = render;
    $("#vm-save", ov).onclick = async () => {
      try {
        await api("/api/virtual", { body: { originalId: v ? v.id : null, id: $("#vm-id", ov).value.trim(), models: Array.from(sel), strategyOverride: $("#vm-s", ov).value, tiers: (v && v.policy && v.policy.tiers) || [] } });
        closeModal(); toast("Gespeichert"); go("routing");
      } catch (e) { toast(e.message, true); }
    };
  }

  /* ---------- Usage & limits ---------- */
  PAGES.usage = async function (range) {
    range = range || S.usageRange || "24h"; S.usageRange = range;
    const [an, lim] = await Promise.all([api("/api/analytics?range=" + range), api("/api/limits")]);
    const tabs = '<div class="tabs" id="us-r">' + [["1h", "1 h"], ["24h", "24 h"], ["7d", "7 Tage"], ["30d", "30 Tage"]].map((r) =>
      '<button data-r="' + r[0] + '"' + (r[0] === range ? ' class="on"' : "") + ">" + r[1] + "</button>").join("") + '</div><button class="btn" data-dl="/api/export/usage" data-name="multillm-usage.json">Export</button>';
    let html = head("Nutzung & Limits", "Rollierende Fenster (5 h / Woche / Monat) werden beim Routing durchgesetzt. OAuth-Provider zeigen zusätzlich ihre echten Kontingente.", tabs);
    const t = an.totals;
    html += '<div class="grid g4">' + stat("Anfragen", fmt(t.ok)) + stat("Input-Tokens", fmt(t.inputTokens)) + stat("Output-Tokens", fmt(t.outputTokens)) +
      stat("Kosten", usd(t.costUsd), an.dailyBudgetUsd ? '<div class="d dim">Heute ' + usd(an.todayCostUsd) + " von " + usd(an.dailyBudgetUsd) + "</div>" : "") + "</div>";
    html += '<div class="card" style="margin-top:16px"><div class="row"><h3 class="grow">Kosten</h3></div>' + areaChart(an.series, ["costUsd"], ["#f59e0b"], { bucketMs: an.bucketMs, money: true }) + "</div>";
    html += '<div class="card" style="margin-top:16px"><h3>Limits pro Provider</h3>' + (lim.providers.length ? lim.providers.map(limitBlock).join('<hr style="border:none;border-top:1px solid var(--line);margin:14px 0">') : '<div class="empty">Keine Provider.</div>') + "</div>";
    html += '<div class="card" style="margin-top:16px"><h3>Modelle</h3>' + modelTable(an.models) + "</div>";
    $("#main").innerHTML = html;
    $$("#us-r button").forEach((b) => b.onclick = () => PAGES.usage(b.dataset.r));
    $$("[data-lim]").forEach((b) => b.onclick = () => limitDialog(lim.providers.find((p) => p.providerId === b.dataset.lim)));
  };

  function limitBar(l) {
    const p = l.usedPercent != null ? l.usedPercent : (l.limit ? (l.used / l.limit) * 100 : null);
    const cls = p == null ? "" : p >= 100 ? "bad" : p >= 80 ? "warn" : "";
    const unit = l.unit === "usd" ? (v) => usd(v) : (v) => fmt(v);
    const right = l.unlimited ? "unbegrenzt" : l.limit != null ? unit(l.used || 0) + " / " + unit(l.limit) : l.used != null ? unit(l.used) : "";
    const reset = l.resetsAtMs ? " · Reset " + until(l.resetsAtMs) : l.resetsAt ? " · Reset " + esc(String(l.resetsAt).slice(0, 10)) : "";
    return '<div class="limit"><div class="top"><span>' + esc(l.label) + '<span class="dim">' + reset + "</span></span><span>" + right + (p != null ? " · " + p.toFixed(0) + " %" : "") + "</span></div>" +
      (p != null ? '<div class="bar ' + cls + '"><span style="width:' + Math.min(100, p) + '%"></span></div>' : "") + "</div>";
  }

  function limitBlock(p) {
    const remote = Array.isArray(p.remote) ? p.remote.filter((r) => r.window !== "info") : [];
    const info = Array.isArray(p.remote) ? p.remote.filter((r) => r.window === "info") : [];
    const u = p.usage;
    return '<div class="row">' + logoHtml(p.providerId) + '<div class="grow"><b>' + esc(p.providerId) + "</b> " + (p.oauth ? '<span class="pill acc">' + esc(p.oauth) + "</span>" : "") +
      (p.exhausted ? ' <span class="pill bad">Limit erreicht</span>' : "") + '<div class="dim">5 h: ' + fmt(u.tokens5h) + " Tokens · Woche: " + fmt(u.tokensWeek) + " · Monat: " + fmt(u.tokensMonth) + "</div></div>" +
      '<button class="btn sm" data-lim="' + esc(p.providerId) + '">Limits festlegen</button></div>' +
      (remote.length ? '<div style="margin-top:8px"><div class="dim" style="font-size:12px">Vom Provider gemeldet</div>' + remote.map(limitBar).join("") + "</div>" : "") +
      info.map((i) => '<div class="dim">' + esc(i.label) + "</div>").join("") +
      (p.local.length ? '<div style="margin-top:8px"><div class="dim" style="font-size:12px">Eigene Limits</div>' + p.local.map(limitBar).join("") + "</div>" : "");
  }

  function limitDialog(p) {
    const l = p.limits || {};
    const f = (id, label, v) => '<div><label>' + label + '</label><input type="number" min="0" id="' + id + '" value="' + (v || "") + '" placeholder="unbegrenzt"></div>';
    const body = '<p class="muted">Wird ein Limit erreicht, überspringt das Routing diesen Provider, bis das Fenster wieder Platz hat.</p><div class="grid" style="grid-template-columns:1fr 1fr;gap:10px">' +
      f("l-5h", "Tokens pro 5 h", l.tokens5h) + f("l-r5h", "Anfragen pro 5 h", l.requests5h) + f("l-w", "Tokens pro Woche (7 Tage)", l.tokensWeek) +
      f("l-m", "Tokens pro Kalendermonat", l.tokensMonth) + f("l-rd", "Anfragen pro Tag", l.requestsDay) + "</div>";
    const ov = modal("Limits: " + p.providerId, body, '<button class="btn" data-close>Abbrechen</button><button class="btn primary" id="l-save">Speichern</button>');
    const n = (id) => { const v = parseInt($(id, ov).value, 10); return v > 0 ? v : null; };
    $("#l-save", ov).onclick = async () => {
      try {
        await api("/api/limits", { body: { providerId: p.providerId, tokens5h: n("#l-5h"), requests5h: n("#l-r5h"), tokensWeek: n("#l-w"), tokensMonth: n("#l-m"), requestsDay: n("#l-rd") } });
        closeModal(); toast("Gespeichert"); PAGES.usage();
      } catch (e) { toast(e.message, true); }
    };
  }

  /* ---------- API & CLI ---------- */
  PAGES.api = async function () {
    const [k, info] = await Promise.all([api("/api/api-key"), api("/api/server-info")]);
    const base = location.origin + "/v1";
    const repo = "https://raw.githubusercontent.com/RandomPixelStudios/Multi-LLM/HEAD";
    let html = head("API & CLI", "Jedes OpenAI-kompatible Tool funktioniert mit dieser Adresse und deinem persönlichen Schlüssel.");
    html += '<div class="grid g2"><div class="card"><h3>OpenAI-kompatible API</h3><label>Basis-URL</label><div class="copy"><input readonly value="' + esc(base) + '"><button class="btn" data-copy="' + esc(base) + '">Kopieren</button></div>' +
      '<label>Persönlicher API-Key</label><div class="copy"><input id="ak" readonly type="password" value="' + esc(k.key) + '"><button class="btn" id="ak-show">Zeigen</button><button class="btn" id="ak-copy">Kopieren</button></div>' +
      '<div class="row" style="margin-top:12px"><button class="btn danger sm" id="ak-rot">Neuen Schlüssel erzeugen</button><span class="dim">Der alte Schlüssel wird sofort ungültig.</span></div>' +
      '<label>Beispiel</label><pre class="code">curl ' + esc(base) + '/chat/completions \\\n  -H "Authorization: Bearer $MULTILLM_KEY" \\\n  -H "Content-Type: application/json" \\\n  -d \'{"model":"multillm","messages":[{"role":"user","content":"Hallo"}]}\'</pre></div>' +
      '<div class="card"><h3>multillm CLI</h3><p class="muted">Coding-Agent im Terminal (Linux & Windows). Anmeldung mit demselben Konto wie hier.</p>' +
      '<label>Linux / macOS</label><pre class="code">curl -fsSL ' + esc(repo) + '/install.sh | sh</pre>' +
      '<label>Windows (PowerShell)</label><pre class="code">irm ' + esc(repo) + '/install.ps1 | iex</pre>' +
      '<label>Starten</label><pre class="code">multillm\n# Host: ' + esc(location.host) + '</pre><p class="muted">Befehle: <code>/model</code> <code>/usage</code> <code>/connect</code> <code>/yolo</code> <code>/bypass-permissions</code> <code>/help</code></p>' +
      '<p class="muted">Websuche: ' + (info.search ? '<span class="pill ok">SearXNG aktiv</span>' : '<span class="pill warn">nicht konfiguriert</span>') + "</p></div></div>";
    $("#main").innerHTML = html;
    $$("[data-copy]").forEach((b) => b.onclick = () => copy(b.dataset.copy));
    $("#ak-show").onclick = () => { const i = $("#ak"); i.type = i.type === "password" ? "text" : "password"; };
    $("#ak-copy").onclick = () => copy($("#ak").value);
    $("#ak-rot").onclick = async () => {
      if (!(await confirmBox("Neuer Schlüssel", "Alle Programme mit dem alten Schlüssel verlieren sofort den Zugriff (die CLI holt sich automatisch den neuen).", "Erzeugen", true))) return;
      const r = await api("/api/api-key/rotate", { body: {} }); $("#ak").value = r.key; toast("Neuer Schlüssel erzeugt");
    };
  };

  /* ---------- Users (admin) ---------- */
  PAGES.users = async function () {
    if (!S.user.is_admin) { go("overview"); return; }
    const users = await api("/api/auth/users");
    let html = head("Benutzer", "Jeder Benutzer hat eigene Provider, Schlüssel und Statistiken.", '<button class="btn primary" id="u-add">+ Benutzer</button>');
    html += '<div class="card"><table><tr><th>Benutzer</th><th>Rolle</th><th>Erstellt</th><th>Letzte Anmeldung</th><th>Status</th><th></th></tr>' + users.map((u) =>
      '<tr><td><div class="row"><span class="avatar sm">' + initial(u.displayName || u.username) + "</span><div><b>" + esc(u.displayName || u.username) + '</b><div class="dim">' + esc(u.username) + "</div></div></div></td><td>" +
      (u.is_admin ? '<span class="pill acc">Administrator</span>' : '<span class="pill">Benutzer</span>') + "</td><td>" + esc(u.createdAt) + "</td><td>" + esc(u.lastLogin || "–") + "</td><td>" +
      (u.locked ? '<span class="pill bad">Gesperrt</span>' : u.mustChangePassword ? '<span class="pill warn">Kennwort ändern</span>' : '<span class="pill ok">Aktiv</span>') +
      '</td><td class=num><button class="btn sm" data-ued="' + esc(u.username) + '">Verwalten</button></td></tr>').join("") + "</table></div>";
    $("#main").innerHTML = html;
    $("#u-add").onclick = userCreateDialog;
    $$("[data-ued]").forEach((b) => b.onclick = () => userEditDialog(users.find((u) => u.username === b.dataset.ued)));
  };

  function userCreateDialog() {
    const body = '<label>Benutzername</label><input id="uc-n" autocomplete="off"><label>Anzeigename</label><input id="uc-d"><label>Kennwort (mind. 8 Zeichen)</label><input id="uc-p" type="password" autocomplete="new-password">' +
      '<div class="pw-meter" style="margin-top:6px;background:var(--line)"><span id="uc-m"></span></div><label class="row" style="color:var(--text)"><input type="checkbox" id="uc-a"> Administrator</label>';
    const ov = modal("Benutzer anlegen", body, '<button class="btn" data-close>Abbrechen</button><button class="btn primary" id="uc-s">Anlegen</button>');
    bindMeter($("#uc-p", ov), $("#uc-m", ov));
    $("#uc-s", ov).onclick = async () => {
      try { await api("/api/auth/users", { body: { username: $("#uc-n", ov).value, displayName: $("#uc-d", ov).value, password: $("#uc-p", ov).value, isAdmin: $("#uc-a", ov).checked } }); closeModal(); toast("Benutzer angelegt"); go("users"); }
      catch (e) { toast(e.message, true); }
    };
  }

  function userEditDialog(u) {
    const self = u.username.toLowerCase() === S.user.username.toLowerCase();
    const body = '<label>Anzeigename</label><input id="ue-d" value="' + esc(u.displayName || "") + '">' +
      '<label class="row" style="color:var(--text)"><input type="checkbox" id="ue-a"' + (u.is_admin ? " checked" : "") + (self ? " disabled" : "") + "> Administrator</label>" +
      '<label>Neues Kennwort setzen (optional)</label><input id="ue-p" type="password" autocomplete="new-password" placeholder="leer = unverändert">' +
      '<label class="row" style="color:var(--text)"><input type="checkbox" id="ue-mc" checked> Bei nächster Anmeldung ändern</label>' +
      (u.locked ? '<p><span class="pill bad">Gesperrt</span> nach zu vielen Fehlversuchen.</p>' : "");
    const ov = modal("Benutzer: " + u.username, body, (self ? "" : '<button class="btn danger" id="ue-del" style="margin-right:auto">Löschen</button>') +
      (u.locked ? '<button class="btn" id="ue-un">Entsperren</button>' : "") + '<button class="btn" data-close>Abbrechen</button><button class="btn primary" id="ue-s">Speichern</button>');
    const path = "/api/auth/users/" + encodeURIComponent(u.username);
    $("#ue-s", ov).onclick = async () => {
      const b = { displayName: $("#ue-d", ov).value };
      if (!self) b.isAdmin = $("#ue-a", ov).checked;
      if ($("#ue-p", ov).value) { b.password = $("#ue-p", ov).value; b.mustChange = $("#ue-mc", ov).checked; }
      try { await api(path, { body: b }); closeModal(); toast("Gespeichert"); go("users"); } catch (e) { toast(e.message, true); }
    };
    if ($("#ue-un", ov)) $("#ue-un", ov).onclick = async () => { await api(path, { body: { unlock: true } }); closeModal(); toast("Entsperrt"); go("users"); };
    if ($("#ue-del", ov)) $("#ue-del", ov).onclick = async () => {
      if (!(await confirmBox("Benutzer löschen", "„" + u.username + "“ und alle Daten (Provider, Schlüssel, Statistiken) unwiderruflich löschen?", "Löschen", true))) return;
      try { await api(path, { method: "DELETE", body: {} }); toast("Gelöscht"); go("users"); } catch (e) { toast(e.message, true); }
    };
  }

  /* ---------- Settings ---------- */
  PAGES.settings = async function () {
    const set = await api("/api/settings");
    let theme = "auto"; try { theme = localStorage.getItem("ml-theme") || "auto"; } catch (e) { /* ignore */ }
    let html = head("Einstellungen", "Gilt für dein Profil.");
    html += '<div class="grid g2"><div class="card"><h3>Darstellung</h3><div class="switch"><div>Design</div><select id="st-theme" style="width:160px"><option value="auto">System</option><option value="dark">Dunkel</option><option value="light">Hell</option></select></div></div>' +
      '<div class="card"><h3>Antwort-Cache</h3><div class="switch"><div>Identische Anfragen aus dem Cache beantworten<small>Spart Kosten bei wiederholten Prompts.</small></div><input type="checkbox" id="rc-on"' + (set.responseCache.enabled ? " checked" : "") + "></div>" +
      '<div class="switch"><div>Gültigkeit (Sekunden)</div><input type="number" id="rc-ttl" style="width:110px" value="' + esc(set.responseCache.ttlSecs) + '"></div></div>' +
      '<div class="card"><h3>Prompt-Komprimierung</h3><div class="switch"><div>Aktiv<small>Entfernt Füllwörter/Leerraum (Code bleibt unverändert).</small></div><input type="checkbox" id="cp-on"' + (set.compress.enabled ? " checked" : "") + "></div>" +
      '<div class="switch"><div>Stärke (1–10)</div><input type="number" min="1" max="10" id="cp-s" style="width:90px" value="' + esc(set.compress.strength) + '"></div></div>' +
      '<div class="card"><h3>Daten</h3><div class="row"><button class="btn" data-dl="/api/export/config" data-name="multillm-config.json">Konfiguration exportieren</button><label class="btn" style="margin:0;color:var(--text)">Importieren<input type="file" id="imp" accept=".json" class="hidden"></label></div><p class="dim">Exporte enthalten keine Schlüssel.</p></div></div>';
    $("#main").innerHTML = html;
    $("#st-theme").value = theme;
    $("#st-theme").onchange = (e) => { try { localStorage.setItem("ml-theme", e.target.value); } catch (x) { /* ignore */ } applyTheme(); };
    const save = async (patch) => { try { await api("/api/settings", { body: patch }); toast("Gespeichert"); } catch (e) { toast(e.message, true); } };
    $("#rc-on").onchange = (e) => save({ responseCache: Object.assign({}, set.responseCache, { enabled: e.target.checked }) });
    $("#rc-ttl").onchange = (e) => save({ responseCache: Object.assign({}, set.responseCache, { ttlSecs: parseInt(e.target.value, 10) || 300 }) });
    $("#cp-on").onchange = (e) => save({ compress: Object.assign({}, set.compress, { enabled: e.target.checked }) });
    $("#cp-s").onchange = (e) => save({ compress: Object.assign({}, set.compress, { strength: Math.min(10, Math.max(1, parseInt(e.target.value, 10) || 3)) }) });
    $("#imp").onchange = async (e) => {
      const f = e.target.files[0]; if (!f) return;
      try { await api("/api/import/config", { body: JSON.parse(await f.text()) }); toast("Importiert"); } catch (x) { toast(x.message, true); }
    };
  };

  /* ---------- Account dialog ---------- */
  async function accountDialog() {
    const me = await api("/api/auth/me");
    const sessions = (me.sessions || []).map((s) => "<tr><td>" + (s.kind === "cli" ? "CLI" : "Browser") + "</td><td>" + esc(s.ip) + "</td><td>" + ago(s.lastSeen) + "</td></tr>").join("");
    const body = '<div class="row"><span class="avatar md" style="background:linear-gradient(135deg,var(--accent),#9b6bff)">' + initial(me.user.displayName || me.user.username) + "</span><div><b>" + esc(me.user.displayName || me.user.username) +
      '</b><div class="dim">' + esc(me.user.username) + " · " + (me.user.is_admin ? "Administrator" : "Benutzer") + "</div></div></div>" +
      '<h3 style="margin:18px 0 6px;font-size:14px">Kennwort ändern</h3><input id="ac-old" type="password" placeholder="Aktuelles Kennwort" autocomplete="current-password"><div style="height:8px"></div>' +
      '<input id="ac-new" type="password" placeholder="Neues Kennwort" autocomplete="new-password"><div class="pw-meter" style="margin:6px 0;background:var(--line)"><span id="ac-m"></span></div>' +
      '<input id="ac-new2" type="password" placeholder="Bestätigen" autocomplete="new-password"><div style="height:8px"></div><button class="btn" id="ac-pw">Kennwort ändern</button>' +
      '<h3 style="margin:18px 0 6px;font-size:14px">Aktive Sitzungen</h3><table><tr><th>Typ</th><th>IP</th><th>Zuletzt</th></tr>' + sessions + "</table>";
    const ov = modal("Konto", body, '<button class="btn danger" id="ac-all" style="margin-right:auto">Überall abmelden</button><button class="btn" data-close>Schließen</button><button class="btn primary" id="ac-out">Abmelden</button>');
    bindMeter($("#ac-new", ov), $("#ac-m", ov));
    $("#ac-pw", ov).onclick = async () => {
      if ($("#ac-new", ov).value !== $("#ac-new2", ov).value) { toast("Die Kennwörter stimmen nicht überein.", true); return; }
      try { await api("/api/auth/password", { body: { current: $("#ac-old", ov).value, new: $("#ac-new", ov).value } }); toast("Kennwort geändert – andere Sitzungen wurden abgemeldet."); closeModal(); }
      catch (e) { toast(e.message, true); }
    };
    $("#ac-out", ov).onclick = async () => { await api("/api/auth/logout", { body: {} }).catch(() => {}); showLock(); };
    $("#ac-all", ov).onclick = async () => { await api("/api/auth/logout", { body: { all: true } }).catch(() => {}); showLock(); };
  }

  /* ================= boot ================= */
  function applyTheme() {
    let t = "auto"; try { t = localStorage.getItem("ml-theme") || "auto"; } catch (e) { /* ignore */ }
    if (t === "auto") document.documentElement.removeAttribute("data-theme"); else document.documentElement.setAttribute("data-theme", t);
    if (t === "auto" && window.matchMedia && window.matchMedia("(prefers-color-scheme: light)").matches) document.documentElement.setAttribute("data-theme", "light");
  }
  applyTheme();

  (async function boot() {
    try {
      const me = await api("/api/auth/me", { noRedirect: true });
      if (me.user.mustChangePassword) { showLock(); return; }
      enterApp(me.user);
    } catch (e) { showLock(); }
  })();
})();
