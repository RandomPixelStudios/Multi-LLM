/* Multi LLM – Website V1: hero + faithful app demo (demo data only) */
(function () {
  "use strict";
  var $ = function (s, r) { return (r || document).querySelector(s); };
  var $$ = function (s, r) { return Array.prototype.slice.call((r || document).querySelectorAll(s)); };
  function esc(s) { return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;"); }
  function toast(msg) {
    var t = $("#toast");
    if (!t) return;
    t.textContent = msg;
    t.classList.add("show");
    clearTimeout(t._h);
    t._h = setTimeout(function () { t.classList.remove("show"); }, 2300);
  }
  function copyText(t, done) {
    function fb() {
      var ta = document.createElement("textarea");
      ta.value = t; ta.style.position = "fixed"; ta.style.opacity = "0";
      document.body.appendChild(ta); ta.select();
      try { document.execCommand("copy"); done(); } catch (e) { toast(T("copyFail")); }
      ta.remove();
    }
    if (navigator.clipboard && navigator.clipboard.writeText) navigator.clipboard.writeText(t).then(done, fb);
    else fb();
  }
  /* ================= Language (DE/EN) ================= */
  var LANG_KEY = "ml-lang";
  var LANG = "en";
  var FLAGS = {
    de: '<svg viewBox="0 0 24 18"><rect width="24" height="6" fill="#151515"/><rect y="6" width="24" height="6" fill="#DD0000"/><rect y="12" width="24" height="6" fill="#FFCE00"/></svg>',
    en: '<svg viewBox="0 0 24 18"><rect width="24" height="18" fill="#012169"/><path d="M0 0l24 18M24 0L0 18" stroke="#ffffff" stroke-width="3.4"/><path d="M0 0l24 18M24 0L0 18" stroke="#C8102E" stroke-width="2.2"/><path d="M12 0v18M0 9h24" stroke="#ffffff" stroke-width="5.6"/><path d="M12 0v18M0 9h24" stroke="#C8102E" stroke-width="3.2"/></svg>'
  };
  var STR = {
    en: { title: "Multi LLM – All LLM Providers. One API.", desc: "Multi LLM – all LLM providers behind one OpenAI-compatible local API. Try the faithful app demo right here.",
      copyFail: "Copy failed", prefsSaved: "Preferences saved",
      consentSaved: "Consent saved", consentDeclined: "No non-essential storage. Server logs are created by GitHub before this page loads.",
      provDeleted: "Provider deleted", needAppProv: "You need the app to add providers – install the app to use this.",
      installProv: "Install the app to configure providers.", vmDeleted: "Virtual model deleted", nameReq: "Name is required",
      pickMember: "Pick at least one member", vmCreated: "Virtual model created", copied: "Copied to clipboard",
      keyGen: "New API key generated", installUse: "Install the app to use this.", tabSaved: "Tab order saved",
      saved: "Saved", winDecor: "Demo window – controls are decorative here.", dcSoon: "Discord server coming soon.",
      testOk: "Success (Demo)", testFail: "Failed (Demo)", confirmAgain: "Click again to confirm",
      dlgClose: "Close", dlgSave: "Save", snippetsTitle: "Client config snippets",
      vvName: "Name", vvMembers: "Members", vvStrategy: "Strategy", vvCreate: "Create new Virtual Model",
      vmStrat: "Strategy:", sysBundle: "system bundle",
      starOn: " starred (picked 4× more often)", starOff: " unstarred",
      noMatch1: "No models match \"", noMatch2: "\"",
      exposeA: "Other devices can reach the API at: http://<your-pc-ip>:", exposeB: "/v1 (Windows Firewall may ask for permission once.)" },
    de: { title: "Multi LLM – Alle LLM-Provider. Eine API.", desc: "Multi LLM – alle LLM-Provider hinter einer OpenAI-kompatiblen lokalen API. Teste hier die originalgetreue App-Demo.",
      copyFail: "Kopieren fehlgeschlagen", prefsSaved: "Einstellungen gespeichert",
      consentSaved: "Zustimmung gespeichert", consentDeclined: "Keine nicht notwendige Speicherung. Server-Logs erstellt GitHub schon vor dem Laden dieser Seite.",
      provDeleted: "Provider gelöscht", needAppProv: "Du brauchst die App, um Provider hinzuzufügen – installiere die App.",
      installProv: "Installiere die App, um Provider zu konfigurieren.", vmDeleted: "Virtuelles Modell gelöscht", nameReq: "Name ist erforderlich",
      pickMember: "Wähle mindestens ein Mitglied", vmCreated: "Virtuelles Modell erstellt", copied: "In Zwischenablage kopiert",
      keyGen: "Neuer API-Key erstellt", installUse: "Installiere die App, um dies zu nutzen.", tabSaved: "Tab-Reihenfolge gespeichert",
      saved: "Gespeichert", winDecor: "Demo-Fenster – Steuerung ist hier Deko.", dcSoon: "Discord-Server kommt bald.",
      testOk: "Erfolg (Demo)", testFail: "Fehlgeschlagen (Demo)", confirmAgain: "Zum Bestätigen erneut klicken",
      dlgClose: "Schließen", dlgSave: "Speichern", snippetsTitle: "Client-Konfigurations-Snippets",
      vvName: "Name", vvMembers: "Mitglieder", vvStrategy: "Strategie", vvCreate: "Neues virtuelles Modell",
      vmStrat: "Strategie:", sysBundle: "System-Bundle",
      starOn: " markiert (wird 4× häufiger gewählt)", starOff: ": Markierung entfernt",
      noMatch1: "Keine Modelle für \"", noMatch2: "\"",
      exposeA: "Andere Geräte erreichen die API unter: http://<dein-pc-ip>:", exposeB: "/v1 (Windows-Firewall fragt einmal nach.)" }
  };
  function T(k) { return (STR[LANG] && STR[LANG][k] !== undefined) ? STR[LANG][k] : (STR.en[k] !== undefined ? STR.en[k] : k); }
  function setLang(lang, persist) {
    LANG = (lang === "de") ? "de" : "en";
    document.documentElement.lang = LANG;
    document.title = T("title");
    var md = document.querySelector('meta[name="description"]');
    if (md) md.setAttribute("content", T("desc"));
    var flag = $("#lang-flag"), code = $("#lang-code");
    if (flag) flag.innerHTML = FLAGS[LANG];
    if (code) code.textContent = LANG.toUpperCase();
    $$(".lang-opt").forEach(function (o) { o.classList.toggle("selected", o.getAttribute("data-lang") === LANG); });
    if (persist) { try { localStorage.setItem(LANG_KEY, LANG); } catch (e) {} }
  }
  function readLang() {
    try {
      var s = localStorage.getItem(LANG_KEY);
      if (s === "de" || s === "en") return s;
    } catch (e) {}
    try {
      var nav = (navigator.language || navigator.userLanguage || "en").toLowerCase();
      if (nav.indexOf("de") === 0) return "de";
    } catch (e2) {}
    return "en";
  }
  (function bootLang() {
    setLang(readLang(), false);
    var btn = $("#lang-btn"), menu = $("#lang-menu");
    function closeMenu() { if (menu) menu.classList.add("hidden"); if (btn) btn.setAttribute("aria-expanded", "false"); }
    if (btn) btn.addEventListener("click", function (e) {
      e.stopPropagation();
      var open = menu.classList.toggle("hidden") === false;
      btn.setAttribute("aria-expanded", open ? "true" : "false");
    });
    document.addEventListener("click", function (e) {
      var box = $("#lang-select");
      if (box && !box.contains(e.target)) closeMenu();
    });
    document.addEventListener("keydown", function (e) { if (e.key === "Escape") closeMenu(); });
    $$(".lang-opt").forEach(function (o) {
      o.addEventListener("click", function () { setLang(o.getAttribute("data-lang"), true); closeMenu(); });
    });
  })();
  /* ================= Theme =================
     Dark is the default and the page renders dark on first paint (see the
     inline script in the <head>). No first-visit dialog: on a phone with a
     light OS the page used to flash white before the visitor could choose.
     The footer toggle and Settings → Appearance still switch the theme. */
  var PREFS_KEY = "ml-prefs";
  var prefsMode = "dark"; // light | dark | system
  function sysTheme() {
    try { return window.matchMedia && window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark"; }
    catch (e) { return "dark"; }
  }
  function applyTheme(mode) {
    var eff = mode === "system" ? sysTheme() : mode;
    if (eff === "light") document.documentElement.setAttribute("data-theme", "light");
    else document.documentElement.removeAttribute("data-theme");
    var sw = $("#setting-theme");
    if (sw) sw.checked = (eff === "light");
  }
  function setMode(mode, persist) {
    prefsMode = mode;
    applyTheme(mode);
    if (persist) {
      try { localStorage.setItem(PREFS_KEY, JSON.stringify({ theme: mode, consent: true, ts: Date.now() })); } catch (e) {}
    }
  }
  function readPrefs() {
    try {
      var p = JSON.parse(localStorage.getItem(PREFS_KEY));
      if (p && (p.theme === "light" || p.theme === "dark" || p.theme === "system")) return p;
    } catch (e) {}
    try {
      // Migrate the previous toggle (ml-theme) so returning visitors keep their choice.
      var old = localStorage.getItem("ml-theme");
      if (old === "light" || old === "dark") return { theme: old, consent: true };
    } catch (e2) {}
    return null;
  }
  try {
    var _mq = window.matchMedia("(prefers-color-scheme: light)");
    var _onSys = function () { if (prefsMode === "system") applyTheme("system"); };
    if (_mq.addEventListener) _mq.addEventListener("change", _onSys);
  } catch (e) {}
  (function bootPrefs() {
    // No saved preference -> stay dark. No first-visit dialog: on a phone
    // with a light OS the page used to flash white before anyone could choose.
    var saved = readPrefs();
    if (saved) setMode(saved.theme || "dark", false);
    else setMode("dark", false);
  })();

  /* ================= Consent for server logs (GitHub Pages) =================
     A bottom banner, not a modal: the page stays usable and dark while the
     visitor decides. Nothing non-essential is loaded before a decision.
     The choice is stored in the same ml-prefs record as the theme. */
  var CONSENT_KEY = "ml-consent";
  function readConsent() {
    try { return localStorage.getItem(CONSENT_KEY); } catch (e) { return null; }
  }
  function writeConsent(v) {
    try { localStorage.setItem(CONSENT_KEY, v); } catch (e) {}
  }
  function showConsent() {
    var b = $("#consent-banner");
    if (!b) return;
    b.classList.remove("hidden");
    document.body.classList.add("consent-open");
  }
  function hideConsent() {
    var b = $("#consent-banner");
    if (!b) return;
    b.classList.add("hidden");
    document.body.classList.remove("consent-open");
  }
  (function bootConsent() {
    // Bind the footer "Cookie settings" button in every case, so a returning
    // visitor can always revise the decision.
    var acc = $("#consent-accept"), dec = $("#consent-decline"), re = $("#consent-open");
    if (acc) acc.addEventListener("click", function () { writeConsent("accepted"); hideConsent(); toast(T("consentSaved")); });
    // Declining cannot stop the server logs - GitHub records them before any
    // page loads. We record the refusal, stop storing anything else, and say so.
    if (dec) dec.addEventListener("click", function () { writeConsent("declined"); hideConsent(); toast(T("consentDeclined")); });
    if (re) re.addEventListener("click", showConsent);
    if (readConsent()) hideConsent();
    else showConsent();
  })();


  /* ================= Demo state ================= */
  var LOGOS = { "OpenAI": "openai.png", "Anthropic": "anthropic.png", "DeepSeek": "deepseek.png", "Ollama (Local)": "ollama.png", "Kilo Gateway": "kilo.ico", "OpenCode Zen": "opencode.png", "Groq": "groq.png", "Gemini": "gemini.png", "OpenRouter": "openrouter.png", "Mistral AI": "mistral.png" };
  var PRESETS = [
    { name: "OpenCode Zen", url: "https://opencode.ai/zen/v1" },
    { name: "Kilo Gateway", url: "https://api.kilo.ai/api/gateway/" },
    { name: "OpenAI", url: "https://api.openai.com/v1" },
    { name: "Anthropic", url: "https://api.anthropic.com", format: "anthropic" },
    { name: "DeepSeek", url: "https://api.deepseek.com" },
    { name: "xAI (Grok)", url: "https://api.x.ai/v1" },
    { name: "Groq", url: "https://api.groq.com/openai/v1" },
    { name: "Gemini", url: "https://generativelanguage.googleapis.com/v1beta/openai" },
    { name: "OpenRouter", url: "https://openrouter.ai/api/v1" },
    { name: "Mistral AI", url: "https://api.mistral.ai/v1" },
    { name: "Together AI", url: "https://api.together.ai/v1" },
    { name: "Ollama (Local)", url: "http://localhost:11434/v1" },
    { name: "LM Studio (Local)", url: "http://localhost:1234/v1" },
    { name: "vLLM (Local)", url: "http://localhost:8000/v1" }
  ];
  function M(id, o) {
    return { id: id, name: id, enabled: !!o.en, contextLength: o.ctx || 0, starred: !!o.star, lastTps: o.tps || 0 };
  }
  var state = {
    providers: [
      { id: "openai", baseUrl: "https://api.openai.com/v1", apiFormat: "openai", hasKey: true, status: "ok", models: [M("gpt-6-astra", { en: 1, ctx: 1048576, star: 1, tps: 118 }), M("gpt-5.6", { en: 1, ctx: 409600, tps: 96 })] },
      { id: "anthropic", baseUrl: "https://api.anthropic.com", apiFormat: "anthropic", hasKey: true, status: "ok", models: [M("opus-5", { en: 1, ctx: 512000, star: 1, tps: 84 }), M("fable-5.1", { en: 1, ctx: 204800, star: 1, tps: 102 }), M("fable-5", { en: 1, ctx: 204800, tps: 88 })] },
      { id: "deepseek", baseUrl: "https://api.deepseek.com", apiFormat: "openai", hasKey: true, status: "ok", models: [M("deepseek-v4.1-flash", { en: 1, ctx: 262144, tps: 141 })] },
      { id: "meta", baseUrl: "https://api.meta.ai", apiFormat: "openai", hasKey: false, status: "warn", models: [M("muse-spark-1.3", { en: 0, ctx: 131072, tps: 97 })] },
      { id: "ollama", baseUrl: "http://localhost:11434/v1", apiFormat: "openai", hasKey: false, status: "warn", models: [M("llama3.3", { en: 0, ctx: 131072 })] }
    ],
    virtual: [
      { id: "code-fast", models: ["openai/gpt-6-astra", "anthropic/fable-5.1", "deepseek/deepseek-v4.1-flash"], strategy: "fastest" }
    ],
    api: { port: 5000, enabled: true, exposeLan: false, exposeAllModels: true, key: "demo-mlm-9f2K-q7Xz-4Bd8" },
    strategy: "weighted",
    compress: { enabled: false, strength: 3 },
    usageRange: "all",
    usageSort: "most"
  };
  var now = Date.now();
  var usage = [
    { providerId: "openai", modelId: "gpt-6-astra", requestsOk: 48210, requestsFailed: 121, inputTokens: 14200000, outputTokens: 6800000, lastUsedMs: now - 4 * 60000, lastTps: 118 },
    { providerId: "anthropic", modelId: "fable-5.1", requestsOk: 30112, requestsFailed: 88, inputTokens: 8600000, outputTokens: 4100000, lastUsedMs: now - 11 * 60000, lastTps: 102 },
    { providerId: "anthropic", modelId: "opus-5", requestsOk: 21400, requestsFailed: 210, inputTokens: 6100000, outputTokens: 2900000, lastUsedMs: now - 42 * 60000, lastTps: 84 },
    { providerId: "deepseek", modelId: "deepseek-v4.1-flash", requestsOk: 15230, requestsFailed: 64, inputTokens: 3400000, outputTokens: 1600000, lastUsedMs: now - 3 * 3600000, lastTps: 141 },
    { providerId: "openai", modelId: "gpt-5.6", requestsOk: 9810, requestsFailed: 41, inputTokens: 2100000, outputTokens: 980000, lastUsedMs: now - 26 * 3600000, lastTps: 96 },
    { providerId: "anthropic", modelId: "fable-5", requestsOk: 5204, requestsFailed: 19, inputTokens: 1100000, outputTokens: 520000, lastUsedMs: now - 2 * 86400000, lastTps: 88 }
  ];
  var tabOrder = ["models", "providers", "virtual", "api", "usage", "settings"];
  var tabNames = { models: "Models", providers: "Providers", virtual: "Virtual Models", api: "API", usage: "Usage", settings: "Settings" };

  /* ================= Helpers ================= */
  function findProvider(id) {
    for (var i = 0; i < state.providers.length; i++) if (state.providers[i].id === id) return state.providers[i];
    return null;
  }
  function fmtTps(v) {
    if (!v || v <= 0) return "—";
    return (v >= 10 ? v.toFixed(0) : v.toFixed(1)) + " tok/s";
  }
  function fmtCtx(n) {
    if (!n || n <= 0) return "";
    return (n >= 1000 ? (Math.round(n / 100) / 10).toFixed(1).replace(/\.0$/, "") + "K" : String(n)) + " ctx";
  }
  function fmtNum(n) {
    if (n >= 1000000) return (Math.round(n / 100000) / 10).toFixed(1).replace(/\.0$/, "") + "M";
    if (n >= 1000) return (Math.round(n / 100) / 10).toFixed(1).replace(/\.0$/, "") + "K";
    return String(n);
  }
  function dotClass(s) { return s === "ok" ? "ok" : s === "error" ? "err" : s === "warn" ? "warn" : ""; }
  function logoHtml(name, cls) {
    var f = LOGOS[name];
    if (!f) return "";
    return '<span class="p-logowrap ' + cls + '"><img src="assets/logos/' + f + '" alt="" loading="lazy" onerror="this.parentNode.remove()"></span>';
  }
  function uniquePrefixes(provs) {
    if (!provs.length) return [];
    var max = Math.max.apply(null, provs.map(function (p) { return p.length; }).concat([1]));
    for (var len = 1; len <= max; len++) {
      var pre = provs.map(function (p) { return p.slice(0, len); });
      if (new Set(pre).size === pre.length) return pre;
    }
    return provs.slice();
  }
  function enabledByModel() {
    var byId = {};
    state.providers.forEach(function (p) {
      p.models.forEach(function (m) {
        if (!m.enabled) return;
        (byId[m.id] = byId[m.id] || []).push(p.id);
      });
    });
    return byId;
  }
  function modelAlias(providerId, modelId, byId) {
    var provs = byId[modelId];
    if (!provs || provs.length < 2) return null;
    var pre = uniquePrefixes(provs);
    var idx = provs.indexOf(providerId);
    return (idx >= 0 ? pre[idx] : providerId) + "-" + modelId;
  }
  function modelLabel(providerId, modelId) {
    var p = findProvider(providerId);
    if (!p) return modelId;
    for (var i = 0; i < p.models.length; i++) if (p.models[i].id === modelId) return p.models[i].name || modelId;
    return modelId;
  }

  /* ================= Sidebar tabs ================= */
  function showPanel(key) {
    $$("#demo-nav .nav-item[data-panel]").forEach(function (b) {
      var on = b.getAttribute("data-panel") === key;
      b.classList.toggle("active", on);
      b.setAttribute("aria-selected", on ? "true" : "false");
    });
    $$("#demo-main .panel").forEach(function (p) { p.classList.add("hidden"); });
    var p = $("#panel-" + key);
    if (p) p.classList.remove("hidden");
  }
  function renderSidebar() {
    var nav = $("#demo-nav");
    nav.innerHTML = "";
    tabOrder.forEach(function (key) {
      var b = document.createElement("button");
      b.className = "nav-item" + (!$("#panel-" + key) || !$("#panel-" + key).classList.contains("hidden") ? " active" : "");
      b.setAttribute("role", "tab");
      b.setAttribute("data-panel", key);
      b.innerHTML = "<span>" + tabNames[key] + "</span>";
      b.addEventListener("click", function () { showPanel(key); });
      nav.appendChild(b);
    });
    renderTabOrderList();
  }
  ["btn-goto-providers", "models-empty-goto"].forEach(function (id) {
    var b = document.getElementById(id);
    if (b) b.addEventListener("click", function () { showPanel("providers"); });
  });

  /* ================= Models ================= */
  function renderModels() {
    var grid = $("#model-grid"), emptyEl = $("#models-empty"), sub = $("#models-sub");
    var q = ($("#model-search").value || "").trim().toLowerCase();
    var sortMode = $("#model-sort").value;
    grid.innerHTML = "";
    var rows = [], total = 0;
    var byId = enabledByModel();
    state.providers.forEach(function (p) {
      p.models.forEach(function (m) {
        if (!m.enabled) return;
        total++;
        var alias = modelAlias(p.id, m.id, byId);
        if (q && (m.id + " " + (m.name || "") + " " + p.id + " " + (alias || "")).toLowerCase().indexOf(q) < 0) return;
        rows.push({ p: p, m: m, alias: alias });
      });
    });
    if (sortMode === "name") rows.sort(function (a, b) { return (a.m.name || a.m.id).localeCompare(b.m.name || b.m.id); });
    else if (sortMode === "context") rows.sort(function (a, b) { return (b.m.contextLength || 0) - (a.m.contextLength || 0); });
    else if (sortMode === "provider") rows.sort(function (a, b) { return a.p.id === b.p.id ? (a.m.name || a.m.id).localeCompare(b.m.name || b.m.id) : a.p.id.localeCompare(b.p.id); });
    else if (sortMode === "starred") rows.sort(function (a, b) { return ((b.m.starred ? 1 : 0) - (a.m.starred ? 1 : 0)) || (a.m.name || a.m.id).localeCompare(b.m.name || b.m.id); });
    rows.forEach(function (row) {
      var u = null;
      for (var i = 0; i < usage.length; i++) if (usage[i].providerId === row.p.id && usage[i].modelId === row.m.id) u = usage[i];
      var card = document.createElement("div");
      card.className = "mcard" + (row.m.starred ? " starred" : "");
      card.innerHTML = '<button class="star-btn' + (row.m.starred ? " on" : "") + '" title="' + (row.m.starred ? "Unstar" : "Star") + ' (starred models are picked more often by the random router)">' + (row.m.starred ? "★" : "☆") + "</button>" +
        '<div class="m-name"' + (row.alias ? ' title="Use this ID in API clients to target this provider\'s copy"' : "") + ">" + esc(row.alias || row.m.name || row.m.id) + "</div>" +
        '<div class="m-prov"><span class="pdot ' + dotClass(row.p.status) + '"></span>' + logoHtml(row.p.id === "openai" ? "OpenAI" : row.p.id === "anthropic" ? "Anthropic" : row.p.id === "deepseek" ? "DeepSeek" : row.p.id === "ollama" ? "Ollama (Local)" : "", "logo-model") + "<span>" + esc(row.p.id) + "</span></div>" +
        '<div class="m-foot"><span class="m-tps">' + fmtTps(u ? u.lastTps : row.m.lastTps) + "</span>" + (row.m.contextLength ? '<span class="m-ctx">' + esc(fmtCtx(row.m.contextLength)) + "</span>" : "") + "</div>";
      card.querySelector(".star-btn").addEventListener("click", function () {
        row.m.starred = !row.m.starred;
        renderModels();
        toast(row.m.id + (row.m.starred ? T("starOn") : T("starOff")));
      });
      grid.appendChild(card);
    });
    if (q && !rows.length && total > 0) {
      var d = document.createElement("div");
      d.className = "empty";
      d.style.gridColumn = "1/-1";
      d.textContent = T("noMatch1") + q + T("noMatch2");
      grid.appendChild(d);
    }
    emptyEl.classList.toggle("hidden", total > 0);
    sub.textContent = q ? rows.length + " of " + total + " models match" : (total === 1 ? "1 model enabled" : total + " models enabled");
  }
  $("#model-search").addEventListener("input", renderModels);
  $("#model-sort").addEventListener("change", renderModels);

  /* ================= Providers ================= */
  function healthOf(p) {
    if (p.status === "ok") return "ok";
    if (p.status === "error") return "err";
    return "warn";
  }
  function renderProviders() {
    var wrap = $("#provider-grid"), emptyEl = $("#providers-empty");
    wrap.innerHTML = "";
    emptyEl.classList.toggle("hidden", state.providers.length > 0);
    state.providers.forEach(function (p) {
      var en = p.models.filter(function (m) { return m.enabled; }).length;
      var card = document.createElement("div");
      card.className = "mcard";
      card.innerHTML = '<div class="m-name name-row">' + logoHtml(p.id === "openai" ? "OpenAI" : p.id === "anthropic" ? "Anthropic" : p.id === "deepseek" ? "DeepSeek" : p.id === "ollama" ? "Ollama (Local)" : "", "logo-prov") + '<span class="name-text">' + esc(p.id) + '</span></div>' +
        '<div class="m-prov"><span class="pdot ' + healthOf(p) + '" title="provider health"></span><span>' + en + " / " + p.models.length + " models enabled</span></div>" +
        '<div class="m-prov" title="' + esc(p.baseUrl) + '"><span>' + esc(p.baseUrl) + " · " + esc(p.apiFormat) + "</span></div>" +
        (p.hasKey ? "" : '<div style="margin-top:8px;"><span class="badge warn">no API key</span></div>') +
        '<div class="m-foot"><button class="btn sm provider-test-btn">Test</button><span class="pcard-actions">' +
        '<button class="ibtn act-edit" title="Edit provider">✎</button>' +
        '<button class="ibtn danger act-del" title="Delete provider (click twice)">🗑</button></span></div>';
      card.querySelector(".act-edit").addEventListener("click", function (ev) {
        ev.stopPropagation();
        toast(T("installProv"));
      });
      card.querySelector(".act-del").addEventListener("click", function (ev) {
        armThen(ev.currentTarget, function () {
          state.providers = state.providers.filter(function (x) { return x.id !== p.id; });
          renderProviders(); renderModels(); renderVirtual();
          toast(T("provDeleted"));
        });
      });
      card.querySelector(".provider-test-btn").addEventListener("click", function (ev) {
        ev.stopPropagation();
        card.classList.add("testing");
        setTimeout(function () {
          card.classList.remove("testing");
          showTestDialog("Test: " + p.id, true, "Connected – 2 models listed", 180 + Math.floor(Math.random() * 700));
        }, 700);
      });
      wrap.appendChild(card);
    });
  }
  function armThen(btn, action) {
    if (btn.dataset.armed === "1") {
      btn.dataset.armed = "";
      btn.classList.remove("armed");
      action();
      return;
    }
    btn.dataset.armed = "1";
    btn.classList.add("armed");
    btn.title = T("confirmAgain");
    setTimeout(function () { btn.dataset.armed = ""; btn.classList.remove("armed"); }, 2600);
  }

  /* ================= Dialogs ================= */
  function closeDialog() { var r = $("#dialog-root"); if (r) r.innerHTML = ""; }
  function overlay() {
    closeDialog();
    var root = $("#dialog-root");
    var ov = document.createElement("div");
    ov.className = "overlay";
    root.appendChild(ov);
    ov.addEventListener("mousedown", function (e) { if (e.target === ov) closeDialog(); });
    return ov;
  }
  function dialogShell(ov, title, wide) {
    var d = document.createElement("div");
    d.className = "dialog" + (wide ? " wide" : "");
    d.setAttribute("role", "dialog");
    d.innerHTML = '<div class="dialog-body"><div class="dialog-title">' + esc(title) + '</div><div class="dialog-content"></div></div>' +
      '<div class="dialog-footer"><button class="btn dlg-close">' + T("dlgClose") + '</button><button class="btn accent dlg-save">' + T("dlgSave") + "</button></div>";
    ov.appendChild(d);
    d.querySelector(".dlg-close").addEventListener("click", closeDialog);
    return d;
  }
  function showTestDialog(title, ok, msg, ms) {
    var ov = overlay();
    var d = document.createElement("div");
    d.className = "dialog";
    d.setAttribute("role", "dialog");
    d.innerHTML = '<div class="dialog-body"><div class="dialog-title">' + esc(title) + '</div>' +
      '<div class="test-result"><span class="tr-status ' + (ok ? "ok" : "err") + '">' + (ok ? T("testOk") : T("testFail")) + '</span>' +
      '<span class="tr-msg">' + esc(msg) + '</span><span class="tr-lat">' + ms + " ms</span></div></div>" +
      '<div class="dialog-footer"><button class="btn accent dlg-close">' + T("dlgClose") + "</button></div>";
    ov.appendChild(d);
    d.querySelector(".dlg-close").addEventListener("click", closeDialog);
  }

  /* ---- Adding providers needs the installed app ---- */
  $("#btn-add-provider").addEventListener("click", function () {
    toast(T("needAppProv"));
  });

  /* ================= Virtual models ================= */
  function allEnabledModels() {
    var out = [];
    state.providers.forEach(function (p) {
      p.models.forEach(function (m) { if (m.enabled) out.push({ key: p.id + "/" + m.id, label: m.name || m.id, prov: p.id }); });
    });
    return out;
  }
  function renderVirtual() {
    var wrap = $("#virtual-grid"), emptyEl = $("#virtual-empty");
    wrap.innerHTML = "";
    var enabled = allEnabledModels();
    var sysMembers = enabled.map(function (e) { return e.key; });
    function card(id, members, strategy, system, onDelete) {
      var el = document.createElement("div");
      el.className = "mcard";
      el.innerHTML = '<div class="m-name name-row"><span class="name-text">' + esc(id) + '</span><span class="vm-count">' + members.length + " models</span></div>" +
        '<div class="vm-members">' + members.slice(0, 6).map(function (k) { return '<span class="vm-chip">' + esc(modelLabel(k.split("/")[0], k.split("/")[1])) + "</span>"; }).join("") + (members.length > 6 ? '<span class="vm-chip">+' + (members.length - 6) + " more</span>" : "") + "</div>" +
        '<div class="vm-strat">' + T("vmStrat") + ' <b>' + esc(strategy) + "</b>" + (system ? " · " + T("sysBundle") : "") + "</div>" +
        '<div class="m-foot"><button class="btn sm v-test">Test</button><span class="pcard-actions">' + (onDelete ? '<button class="ibtn danger v-del" title="Delete (click twice)">🗑</button>' : "<span></span>") + "</span></div>";
      el.querySelector(".v-test").addEventListener("click", function (ev) {
        ev.stopPropagation();
        el.classList.add("testing");
        setTimeout(function () {
          el.classList.remove("testing");
          showTestDialog("Test: " + id, true, "Routed to " + modelLabel(members[0].split("/")[0], members[0].split("/")[1]) + " (" + strategy + ")", 200 + Math.floor(Math.random() * 600));
        }, 650);
      });
      if (onDelete) el.querySelector(".v-del").addEventListener("click", function (ev) { ev.stopPropagation(); armThen(ev.currentTarget, onDelete); });
      return el;
    }
    wrap.appendChild(card("multillm", sysMembers, state.strategy + (state.strategy === "weighted" ? " ★×4" : ""), true, null));
    state.virtual.forEach(function (v) {
      wrap.appendChild(card(v.id, v.models, v.strategy, false, function () {
        state.virtual = state.virtual.filter(function (x) { return x.id !== v.id; });
        renderVirtual();
        toast(T("vmDeleted"));
      }));
    });
    emptyEl.classList.add("hidden");
  }
  $("#btn-add-virtual").addEventListener("click", function () {
    var ov = overlay();
    var d = dialogShell(ov, T("vvCreate"), true);
    var c = d.querySelector(".dialog-content");
    var enabled = allEnabledModels();
    c.innerHTML = '<div class="field"><label>' + T("vvName") + '</label><input id="vv-id" type="text" value="code-fast" spellcheck="false" /></div>' +
      '<div class="dlg-sec-title">' + T("vvMembers") + '</div><div id="vv-models" class="edit-models-grid">' +
      enabled.map(function (e, i) { return '<label class="mrow"><input type="checkbox" data-k="' + esc(e.key) + '"' + (i < 3 ? " checked" : "") + ' /><span class="mrow-name">' + esc(e.label) + '</span><span class="hint-sm">' + esc(e.prov) + "</span></label>"; }).join("") + "</div>" +
      '<div class="field" style="margin-top:12px;"><label>' + T("vvStrategy") + '</label><select id="vv-strat"><option value="weighted">weighted</option><option value="round_robin">round_robin</option><option value="priority">priority</option><option value="latency">latency</option><option value="fastest" selected>fastest</option><option value="sticky">sticky</option></select></div>';
    d.querySelector(".dlg-save").addEventListener("click", function () {
      var id = (d.querySelector("#vv-id").value || "").trim();
      if (!id) { toast(T("nameReq")); return; }
      var members = $$("#vv-models input:checked").map(function (x) { return x.getAttribute("data-k"); });
      if (!members.length) { toast(T("pickMember")); return; }
      state.virtual.push({ id: id, models: members, strategy: d.querySelector("#vv-strat").value });
      closeDialog();
      renderVirtual();
      toast(T("vmCreated"));
    });
  });

  /* ================= API panel ================= */
  function renderApi() {
    var portEl = $("#api-port");
    if (document.activeElement !== portEl) portEl.value = String(state.api.port);
    $("#api-enabled").checked = state.api.enabled;
    $("#api-expose").checked = state.api.exposeLan;
    $("#api-expose-all").checked = state.api.exposeAllModels;
    $("#api-url-preview").textContent = "http://localhost:" + state.api.port + "/v1";
    $("#api-dot").className = "url-dot" + (state.api.enabled ? " on" : "");
    $("#api-key-input").value = state.api.key;
    updateExposeHint();
  }
  function updateExposeHint() {
    var hint = $("#expose-hint");
    if (!state.api.exposeLan) { hint.style.display = "none"; return; }
    hint.style.display = "block";
    hint.textContent = T("exposeA") + state.api.port + T("exposeB");
  }
  function flashSaved() {
    var s = $("#api-saved");
    s.classList.add("show");
    clearTimeout(s._h);
    s._h = setTimeout(function () { s.classList.remove("show"); }, 1800);
  }
  $("#api-port").addEventListener("input", function (e) {
    var v = parseInt(e.target.value, 10);
    if (v >= 1 && v <= 65535) state.api.port = v;
    $("#api-url-preview").textContent = "http://localhost:" + (isNaN(v) ? state.api.port : v) + "/v1";
    updateExposeHint();
    flashSaved();
  });
  $("#api-url-copy").addEventListener("click", function () {
    copyText($("#api-url-preview").textContent, function () { toast(T("copied")); });
  });
  $("#key-copy").addEventListener("click", function () {
    copyText(state.api.key, function () { toast(T("copied")); });
  });
  $("#key-show").addEventListener("click", function () {
    var inp = $("#api-key-input");
    inp.type = inp.type === "password" ? "text" : "password";
  });
  $("#key-reroll").addEventListener("click", function (e) { armThen(e.currentTarget, function () {
    state.api.key = "demo-mlm-" + Math.random().toString(36).slice(2, 6) + "-" + Math.random().toString(36).slice(2, 6) + "-4Bd8";
    renderApi();
    toast(T("keyGen"));
  }); });
  [["api-enabled"], ["api-expose"], ["api-expose-all"]].forEach(function (pair) {
    var box = $("#" + pair[0]);
    // Locked in the demo – enabling/disabling needs the installed app.
    box.addEventListener("click", function (e) {
      e.preventDefault();
      toast(T("installUse"));
    });
  });
  $("#btn-client-snippets").addEventListener("click", function () {
    var ov = overlay();
    var d = document.createElement("div");
    d.className = "dialog";
    d.setAttribute("role", "dialog");
    var url = "http://localhost:" + state.api.port + "/v1";
    d.innerHTML = '<div class="dialog-body"><div class="dialog-title">' + T("snippetsTitle") + "</div>" +
      '<div class="dlg-sec-title">cURL</div><div class="dlg-panel"><span class="mono" style="font-size:12px;">curl ' + esc(url) + '/chat/completions -H "Authorization: Bearer KEY" -d \'{"model":"multillm"}\'</span></div>' +
      '<div class="dlg-sec-title">Python</div><div class="dlg-panel"><span class="mono" style="font-size:12px;">OpenAI(base_url="' + esc(url) + '", api_key="KEY")</span></div>' +
      '<div class="dlg-sec-title">Cursor / VS Code</div><div class="dlg-panel"><span class="mono" style="font-size:12px;">Base URL: ' + esc(url) + ' · Model: multillm</span></div></div>' +
      '<div class="dialog-footer"><button class="btn accent dlg-close">' + T("dlgClose") + "</button></div>";
    ov.appendChild(d);
    d.querySelector(".dlg-close").addEventListener("click", closeDialog);
  });

  /* ================= Usage ================= */
  var RANGE_LABEL = { today: "Today", month: "Month", all: "All Time" };
  var SORT_LABEL = { most: "Most Usage", last: "Last Used", oldest: "Oldest Used", requests: "Most Requests", failed: "Most Failed" };
  var RANGE_FACTOR = { today: 0.08, month: 0.45, all: 1 };
  function renderUsage() {
    var q = ($("#usage-search").value || "").trim().toLowerCase();
    var f = RANGE_FACTOR[state.usageRange];
    var rows = usage.filter(function (u) {
      if (!q) return true;
      return (u.modelId + " " + u.providerId).toLowerCase().indexOf(q) >= 0;
    }).map(function (u) {
      return { u: u, total: Math.round((u.inputTokens + u.outputTokens) * f), ok: Math.round(u.requestsOk * f), fail: Math.round(u.requestsFailed * f), inp: Math.round(u.inputTokens * f), out: Math.round(u.outputTokens * f) };
    });
    var s = state.usageSort;
    rows.sort(function (a, b) {
      if (s === "most") return b.total - a.total;
      if (s === "last") return b.u.lastUsedMs - a.u.lastUsedMs;
      if (s === "oldest") return a.u.lastUsedMs - b.u.lastUsedMs;
      if (s === "requests") return b.ok - a.ok;
      return b.fail - a.fail;
    });
    var tOk = 0, tFail = 0, tIn = 0, tOut = 0;
    rows.forEach(function (r) { tOk += r.ok; tFail += r.fail; tIn += r.inp; tOut += r.out; });
    $("#stat-requests").textContent = tOk.toLocaleString("en-US");
    $("#stat-failed").textContent = tFail.toLocaleString("en-US");
    $("#stat-input").textContent = fmtNum(tIn);
    $("#stat-output").textContent = fmtNum(tOut);
    var body = $("#usage-body");
    body.innerHTML = "";
    var max = rows.length ? rows[0].total : 1;
    if (s === "last" || s === "oldest" || s === "requests" || s === "failed") {
      var m2 = {};
      usage.forEach(function (u) { m2[u.providerId + "/" + u.modelId] = u.inputTokens + u.outputTokens; });
      var top = 1;
      rows.forEach(function (r) { top = Math.max(top, m2[r.u.providerId + "/" + r.u.modelId] || 1); });
      max = top;
    }
    rows.forEach(function (r, i) {
      var pct = Math.max(2, Math.round((r.total / max) * 100));
      var row = document.createElement("div");
      row.className = "usage-row";
      row.innerHTML = '<span class="ur-rank">' + (i + 1) + '</span><span class="ur-model"><span class="ur-namewrap"><span class="ur-name">' + esc(r.u.modelId) + '</span><span class="ur-meta">' + esc(r.u.providerId) + " · " + r.ok.toLocaleString("en-US") + " req</span></span></span>" +
        '<span class="ur-track"><span class="ur-fill" style="width:' + pct + '%"></span></span>' +
        '<span class="ur-vals"><b>' + fmtNum(r.total) + "</b><i>tokens</i></span>";
      body.appendChild(row);
    });
    $("#usage-empty").classList.toggle("hidden", rows.length > 0);
    $("#usage-sort").textContent = "Sort: " + RANGE_LABEL[state.usageRange] + " · " + SORT_LABEL[state.usageSort];
    $$("#usage-sort-menu .sort-item").forEach(function (b) {
      var on = (b.dataset.range && b.dataset.range === state.usageRange) || (b.dataset.sort && b.dataset.sort === state.usageSort);
      b.classList.toggle("active", !!on);
    });
  }
  $("#usage-search").addEventListener("input", renderUsage);
  $("#usage-sort").addEventListener("click", function (e) {
    e.stopPropagation();
    $("#usage-sort-menu").classList.toggle("hidden");
  });
  document.addEventListener("click", function (e) {
    var w = $("#usage-sort-wrap");
    if (w && !w.contains(e.target)) $("#usage-sort-menu").classList.add("hidden");
  });
  $$("#usage-sort-menu .sort-item").forEach(function (b) {
    b.addEventListener("click", function () {
      if (b.dataset.range) state.usageRange = b.dataset.range;
      if (b.dataset.sort) state.usageSort = b.dataset.sort;
      $("#usage-sort-menu").classList.add("hidden");
      renderUsage();
    });
  });

  /* ================= Settings (Appearance + Token compression only) ================= */
  function renderTabOrderList() {
    var box = $("#tab-order-list");
    if (!box) return;
    box.innerHTML = "";
    tabOrder.forEach(function (key, i) {
      var row = document.createElement("div");
      row.className = "tab-order-row";
      row.innerHTML = '<span class="to-name">' + tabNames[key] + "</span>" +
        '<button class="ibtn to-up" title="Move up"' + (i === 0 ? " disabled" : "") + ">▲</button>" +
        '<button class="ibtn to-down" title="Move down"' + (i === tabOrder.length - 1 ? " disabled" : "") + ">▼</button>";
      row.querySelector(".to-up").addEventListener("click", function () {
        if (i === 0) return;
        tabOrder.splice(i - 1, 0, tabOrder.splice(i, 1)[0]);
        renderSidebar();
        toast(T("tabSaved"));
      });
      row.querySelector(".to-down").addEventListener("click", function () {
        if (i === tabOrder.length - 1) return;
        tabOrder.splice(i + 1, 0, tabOrder.splice(i, 1)[0]);
        renderSidebar();
        toast(T("tabSaved"));
      });
      box.appendChild(row);
    });
  }
  function initSettings() {
    $("#setting-theme").checked = document.documentElement.getAttribute("data-theme") === "light";
    $("#setting-theme").addEventListener("change", function (e) { setMode(e.target.checked ? "light" : "dark", true); });
    var comp = $("#setting-compress"), row = $("#row-compress-strength"), slider = $("#setting-compress-strength"), val = $("#setting-compress-strength-value");
    slider.value = String(state.compress.strength);
    val.textContent = String(state.compress.strength);
    function syncComp() {
      row.classList.toggle("dimmed", !comp.checked);
      comp.checked = state.compress.enabled;
    }
    comp.addEventListener("change", function () { state.compress.enabled = comp.checked; syncComp(); toast(T("saved")); });
    slider.addEventListener("input", function () {
      state.compress.strength = parseInt(slider.value, 10);
      val.textContent = slider.value;
    });
    slider.addEventListener("change", function () { toast(T("saved")); });
    syncComp();
    $("#settings-search").addEventListener("input", function (e) {
      var q = e.target.value.trim().toLowerCase();
      $$("#panel-settings .set-row[data-keys]").forEach(function (r) {
        var hit = !q || (r.getAttribute("data-keys") + " " + r.textContent).toLowerCase().indexOf(q) >= 0;
        r.style.display = hit ? "" : "none";
      });
      $$("#panel-settings .set-sec").forEach(function (sec) {
        var any = $$(".set-row", sec).some(function (r) { return r.style.display !== "none"; });
        sec.style.display = any || !q ? "" : "none";
      });
    });
  }

  /* ================= Titlebar (decorative in the browser demo) ================= */
  ["win-min", "win-max", "win-close"].forEach(function (id) {
    var b = document.getElementById(id);
    if (b) b.addEventListener("click", function () { toast(T("winDecor")); });
  });

  /* ================= Marquee: all configured providers ================= */
  (function renderMarquee() {
    var mq = $("#marquee-track");
    if (!mq) return;
    function isLocal(n) { return n.indexOf("(Local)") >= 0 || n === "LocalAI"; }
    var html = PRESETS.map(function (p) {
      return '<span class="mq' + (isLocal(p.name) ? " local" : "") + '">' + esc(p.name) + "</span>";
    }).join("") + '<span class="mq">+ Custom</span>';
    mq.innerHTML = html + html; // duplicated for a seamless loop
  })();

  /* ================= Footer theme toggle (moon/sun) ================= */
  (function bootThemeFoot() {
    var b = $("#theme-foot");
    if (!b) return;
    b.addEventListener("click", function () {
      var light = document.documentElement.getAttribute("data-theme") === "light";
      setMode(light ? "dark" : "light", true);
    });
  })();

  /* ================= Scroll-in animations ================= */
  (function bootAnims() {
    var reduce = false;
    try { reduce = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches; } catch (e) {}
    $$(".prov-section .section-title, .why-section .section-title, .demo-cap, .legal-section .legal-notice").forEach(function (el) { el.classList.add("rv"); });
    var targets = $$(".appwin, .why-grid, .rv");
    if (reduce || !("IntersectionObserver" in window)) {
      targets.forEach(function (el) { el.classList.add("in"); });
      return;
    }
    var io = new IntersectionObserver(function (es) {
      es.forEach(function (e) { if (e.isIntersecting) { e.target.classList.add("in"); io.unobserve(e.target); } });
    }, { threshold: 0.12 });
    targets.forEach(function (el) { io.observe(el); });
  })();

  /* ================= Boot ================= */
  renderSidebar();
  showPanel("models");
  renderModels();
  renderProviders();
  renderVirtual();
  renderApi();
  renderUsage();
  initSettings();
})();
