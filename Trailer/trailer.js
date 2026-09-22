/* ============================================================
   Multi LLM — Official Trailer
   1) hydrate icons exactly like the app
   2) build the app content 1:1
   3) run the cinematic timeline
   4) synthesise an emotional score with the Web Audio API
   ============================================================ */

"use strict";

/* ---------------------------------------------------------------
   0. Helpers
---------------------------------------------------------------- */
function $(s) { return document.querySelector(s); }
function $$(s) { return Array.prototype.slice.call(document.querySelectorAll(s)); }
function el(html) {
  var t = document.createElement("template");
  t.innerHTML = html.trim();
  return t.content.firstElementChild;
}
function esc(s) {
  return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}
function show(node) { if (node) node.classList.add("on"); }
function hide(node) { if (node) node.classList.remove("on"); }

/* ---------------------------------------------------------------
   1. Icons — same map as src/main.ts
---------------------------------------------------------------- */
var ICONS = {
  minimize: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="currentColor" d="M2 6.5h8v.5h-8z"/></svg>',
  maximize: '<svg viewBox="0 0 12 12" aria-hidden="true"><rect x="2" y="2" width="8" height="8" fill="none" stroke="currentColor" stroke-width="1.2"/></svg>',
  close: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M2.5 2.5l7 7M9.5 2.5l-7 7"/></svg>',
  add: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M6 2v8M2 6h8"/></svg>',
  edit: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M8.5 1.5l2 2-6 6H2.5v-2z"/></svg>',
  delete: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M2 3h8M4 3V2h4v1M3 3l.5 7h5L9 3"/></svg>',
  refresh: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M10 6A4 4 0 1 1 6 2"/><path fill="none" stroke="currentColor" stroke-width="1.2" d="M10 2v3h-3"/></svg>',
  copy: '<svg viewBox="0 0 12 12" aria-hidden="true"><rect x="4" y="4" width="6" height="6" fill="none" stroke="currentColor" stroke-width="1.2"/><path fill="none" stroke="currentColor" stroke-width="1.2" d="M8 4V2H2a2 2 0 0 0 0 4h2"/></svg>',
  eye: '<svg viewBox="0 0 12 12" aria-hidden="true"><circle cx="6" cy="6" r="2.2" fill="none" stroke="currentColor" stroke-width="1.2"/><path fill="none" stroke="currentColor" stroke-width="1.2" d="M1 6s2-4 5-4 5 4 5 4-2 4-5 4-5-4-5-4z"/></svg>',
  person: '<svg viewBox="0 0 12 12" aria-hidden="true"><circle cx="6" cy="4.5" r="2.2" fill="none" stroke="currentColor" stroke-width="1.2"/><path fill="none" stroke="currentColor" stroke-width="1.2" d="M1 11c0-2.8 2.2-5 5-5s5 2.2 5 5"/></svg>',
  chart: '<svg viewBox="0 0 12 12" aria-hidden="true"><rect x="1" y="6" width="2.5" height="5" fill="none" stroke="currentColor" stroke-width="1.2"/><rect x="4.5" y="3.5" width="2.5" height="7.5" fill="none" stroke="currentColor" stroke-width="1.2"/><rect x="8" y="1" width="2.5" height="10" fill="none" stroke="currentColor" stroke-width="1.2"/></svg>',
  palette: '<svg viewBox="0 0 12 12" aria-hidden="true"><circle cx="6" cy="6" r="4.2" fill="none" stroke="currentColor" stroke-width="1.2"/><circle cx="4" cy="5" r=".8" fill="currentColor"/><circle cx="7" cy="4.2" r=".8" fill="currentColor"/><circle cx="5.8" cy="7" r=".8" fill="currentColor"/></svg>',
  up: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M3 8l3-4 3 4"/></svg>',
  down: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M3 4l3 4 3-4"/></svg>',
  route: '<svg viewBox="0 0 12 12" aria-hidden="true"><circle cx="2" cy="2" r="1.4" fill="none" stroke="currentColor" stroke-width="1.2"/><circle cx="10" cy="10" r="1.4" fill="none" stroke="currentColor" stroke-width="1.2"/><path fill="none" stroke="currentColor" stroke-width="1.2" d="M3.2 3.2l5.6 5.6"/></svg>',
  compress: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M2 5h8M2 7h8"/></svg>',
  power: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M6 2v4M3 7a3 3 0 0 0 6 0"/></svg>',
  download: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M6 2v7M3 8l3 3 3-3M1 11h10"/></svg>',
  export: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M2 7v4h8V7M6 2v5M4 6l2-2 2 2"/></svg>',
  import: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M2 5v4h8V5M6 8V3M4 6l2 2 2-2"/></svg>',
  search: '<svg viewBox="0 0 12 12" aria-hidden="true"><circle cx="5" cy="5" r="3.5" fill="none" stroke="currentColor" stroke-width="1.2"/><path fill="none" stroke="currentColor" stroke-width="1.2" d="M7.5 7.5l2.5 2.5"/></svg>',
  logout: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M7 2H3v8h4M5 6h6M9.5 4.5L11 6l-1.5 1.5"/></svg>'
};
function hydrateIcons() {
  $$("[data-icon]").forEach(function (node) {
    var name = node.getAttribute("data-icon");
    if (ICONS[name]) node.innerHTML = ICONS[name];
  });
}

/* ---------------------------------------------------------------
   2. App content — mirrors what the real app renders
---------------------------------------------------------------- */
var LOGO = function (f) { return "../public/logos/" + f; };

var STAR_ON = '<svg viewBox="0 0 24 24" aria-hidden="true"><path fill="#ffc85a" d="M12 2.2l2.95 5.98 6.6.96-4.78 4.65 1.13 6.58L12 17.24l-5.9 3.1 1.13-6.57L2.45 9.14l6.6-.96z"/></svg>';
var STAR_OFF = '<svg viewBox="0 0 24 24" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.7" stroke-linejoin="round" d="M12 3.4l2.6 5.27 5.82.85-4.21 4.1.99 5.79L12 16.66l-5.2 2.74.99-5.79-4.21-4.1 5.82-.84z"/></svg>';

var MODELS = [
  { name: "claude-sonnet-4-5", prov: "anthropic", logo: "anthropic.png", ctx: "200K ctx", tps: "62 tok/s", star: true, dot: "ok" },
  { name: "gpt-5-codex", prov: "openai", logo: "openai.png", ctx: "400K ctx", tps: "48 tok/s", star: false, dot: "ok" },
  { name: "deepseek-chat", prov: "deepseek", logo: "deepseek.png", ctx: "128K ctx", tps: "91 tok/s", star: true, dot: "ok" },
  { name: "gemini-2.5-pro", prov: "gemini", logo: "gemini.png", ctx: "1M ctx", tps: "37 tok/s", star: false, dot: "ok" },
  { name: "kimi-k2-thinking", prov: "moonshot", logo: "kimi.png", ctx: "256K ctx", tps: "55 tok/s", star: false, dot: "" },
  { name: "llama-3.3-70b-versatile", prov: "groq", logo: "groq.png", ctx: "128K ctx", tps: "274 tok/s", star: false, dot: "ok" }
];

var PROVIDERS = [
  { id: "anthropic", logo: "anthropic.png", on: "14 / 16 models enabled", url: "https://api.anthropic.com", fmt: "anthropic", dot: "ok" },
  { id: "openai", logo: "openai.png", on: "22 / 24 models enabled", url: "https://api.openai.com/v1", fmt: "openai", dot: "ok" },
  { id: "deepseek", logo: "deepseek.png", on: "6 / 6 models enabled", url: "https://api.deepseek.com/v1", fmt: "openai", dot: "ok" },
  { id: "gemini", logo: "gemini.png", on: "9 / 11 models enabled", url: "https://generativelanguage.googleapis.com/v1beta", fmt: "google", dot: "ok" },
  { id: "groq", logo: "groq.png", on: "12 / 12 models enabled", url: "https://api.groq.com/openai/v1", fmt: "openai", dot: "ok" },
  { id: "ollama", logo: "ollama.png", on: "5 / 5 models enabled", url: "http://localhost:11434/v1", fmt: "openai", dot: "" }
];

var VIRTUALS = [
  { id: "multillm", n: "6 models bundled" },
  { id: "fast-coder", n: "3 models bundled" },
  { id: "long-context", n: "2 models bundled" }
];

var RANK_COLORS = ["#ff5c7a", "#4a89f5", "#34c77b", "#9b59f5", "#45c4e8", "#f5923e"];

var USAGE = [
  { rank: 1, model: "claude-sonnet-4-5", prov: "anthropic", logo: "anthropic.png", meta: "anthropic · 12,438 req · 2h ago", pct: 100, val: "412.9M", share: "38.20%" },
  { rank: 2, model: "gpt-5-codex", prov: "openai", logo: "openai.png", meta: "openai · 9,012 req · 5h ago", pct: 68.8, val: "284.1M", share: "26.35%" },
  { rank: 3, model: "deepseek-chat", prov: "deepseek", logo: "deepseek.png", meta: "deepseek · 21,770 req · 18m ago", pct: 42.8, val: "176.6M", share: "16.37%" },
  { rank: 4, model: "llama-3.3-70b-versatile", prov: "groq", logo: "groq.png", meta: "groq · 33,204 req · 4m ago", pct: 23.8, val: "98.2M", share: "9.10%" },
  { rank: 5, model: "gemini-2.5-pro", prov: "gemini", logo: "gemini.png", meta: "gemini · 4,188 req · 1d ago", pct: 11.5, val: "47.3M", share: "4.38%" },
  { rank: 6, model: "kimi-k2-thinking", prov: "moonshot", logo: "kimi.png", meta: "moonshot · 2,945 req · 3h ago", pct: 8.2, val: "33.7M", share: "3.12%" }
];

function buildModelCards() {
  var grid = $("#model-grid");
  if (!grid) return;
  grid.innerHTML = "";
  MODELS.forEach(function (m) {
    grid.appendChild(el(
      '<div class="mcard' + (m.star ? " starred" : "") + '">' +
        '<button class="star-btn' + (m.star ? " on" : "") + '">' + (m.star ? STAR_ON : STAR_OFF) + "</button>" +
        '<div class="m-name">' + esc(m.name) + "</div>" +
        '<div class="m-prov"><span class="pdot ' + m.dot + '"></span>' +
          '<span class="p-logowrap logo-model"><img src="' + LOGO(m.logo) + '" alt="" draggable="false"></span>' +
          "<span>" + esc(m.prov) + "</span></div>" +
        '<div class="m-foot"><span class="m-tps">' + esc(m.tps) + "</span>" +
          '<span class="m-ctx">' + esc(m.ctx) + "</span></div>" +
      "</div>"
    ));
  });
}

function buildProviderCards() {
  var grid = $("#provider-grid");
  if (!grid) return;
  grid.innerHTML = "";
  PROVIDERS.forEach(function (p) {
    grid.appendChild(el(
      '<div class="mcard">' +
        '<div class="m-name name-row"><span class="p-logowrap logo-prov"><img src="' + LOGO(p.logo) + '" alt="" draggable="false"></span>' +
          '<span class="name-text">' + esc(p.id) + "</span></div>" +
        '<div class="m-prov"><span class="pdot ' + p.dot + '"></span><span>' + esc(p.on) + "</span></div>" +
        '<div class="m-prov" title="' + esc(p.url) + '"><span>' + esc(p.url) + " &middot; " + esc(p.fmt) + "</span></div>" +
        '<div class="m-foot"><span></span><span class="pcard-actions">' +
          '<button class="ibtn"><span class="glyph">' + ICONS.edit + "</span></button>" +
          '<button class="ibtn danger"><span class="glyph">' + ICONS.delete + "</span></button>" +
        "</span></div>" +
      "</div>"
    ));
  });
}

function buildVirtualCards() {
  var grid = $("#virtual-grid");
  if (!grid) return;
  grid.innerHTML = "";
  VIRTUALS.forEach(function (v) {
    grid.appendChild(el(
      '<div class="mcard">' +
        '<div class="m-name name-row"><span class="name-text">' + esc(v.id) + "</span></div>" +
        '<div class="m-prov"><span class="pdot"></span><span>' + esc(v.n) + "</span></div>" +
        '<div class="m-foot"><span></span><span class="pcard-actions">' +
          '<button class="ibtn"><span class="glyph">' + ICONS.edit + "</span></button>" +
          '<button class="ibtn danger"><span class="glyph">' + ICONS.delete + "</span></button>" +
        "</span></div>" +
      "</div>"
    ));
  });
}

function buildUsage() {
  var body = $("#usage-body");
  if (!body) return;
  body.innerHTML = "";
  USAGE.forEach(function (u, i) {
    body.appendChild(el(
      '<div class="usage-row">' +
        '<span class="ur-rank">' + u.rank + "</span>" +
        '<span class="ur-model">' +
          '<span class="p-logowrap logo-model"><img src="' + LOGO(u.logo) + '" alt="" draggable="false"></span>' +
          '<span class="ur-namewrap"><span class="ur-name">' + esc(u.model) + "</span>" +
          '<span class="ur-meta">' + esc(u.meta) + "</span></span>" +
        "</span>" +
        '<span class="ur-track"><span class="ur-fill" style="width:' + u.pct + "%;background:" + RANK_COLORS[i % RANK_COLORS.length] + ';"></span></span>' +
        '<span class="ur-vals"><b>' + u.val + "</b><i>" + u.share + "</i></span>" +
      "</div>"
    ));
  });
}

function buildTabOrder() {
  var list = $("#tab-order-list");
  if (!list) return;
  list.innerHTML = "";
  ["Models", "Providers", "Virtual Models", "API", "Usage", "Settings"].forEach(function (n, i, arr) {
    list.appendChild(el(
      '<div class="tab-order-row"><span class="to-name">' + esc(n) + "</span>" +
        '<button class="ibtn"' + (i === 0 ? " disabled" : "") + '><span class="glyph">' + ICONS.up + "</span></button>" +
        '<button class="ibtn"' + (i === arr.length - 1 ? " disabled" : "") + '><span class="glyph">' + ICONS.down + "</span></button>" +
      "</div>"
    ));
  });
}

/* ---------------------------------------------------------------
   3. Scene engine
---------------------------------------------------------------- */
var TOTAL = 76;              // trailer length in seconds
var appFit = $("#app-fit");
var caption = $("#caption");
var ltKicker = $("#lt-kicker");
var ltLine = $("#lt-line");
var currentPanel = null;

function captionShow(kicker, line) {
  ltKicker.textContent = kicker;
  ltLine.textContent = line;
  show(caption);
}
function captionHide() { hide(caption); }

function cam(o) {
  appFit.style.setProperty("--px", (o.px || 0) + "px");
  appFit.style.setProperty("--py", (o.py || 0) + "px");
  appFit.style.setProperty("--sc", o.sc === undefined ? 1 : o.sc);
  appFit.style.setProperty("--ry", (o.ry || 0) + "deg");
  appFit.style.setProperty("--rx", (o.rx || 0) + "deg");
}

function popIn(scope) {
  scope.classList.add("scene-pop");
  var targets = scope.querySelectorAll(".mcard, .stat, .usage-row, .set-card, .api-tile, .api-hero-row");
  Array.prototype.forEach.call(targets, function (n, i) {
    n.style.animationDelay = (i * 70) + "ms";
  });
  window.setTimeout(function () { scope.classList.remove("scene-pop"); },
    400 + targets.length * 70 + 640);
}

function setPanel(name) {
  var id = "#panel-" + name;
  var next = $(id);
  if (!next || next === currentPanel) return;
  var prev = currentPanel;
  currentPanel = next;
  $$(".nav-item[data-panel]").forEach(function (b) {
    b.classList.toggle("active", b.getAttribute("data-panel") === name);
  });
  if (prev) {
    prev.classList.add("fading");
    window.setTimeout(function () {
      prev.classList.add("hidden");
      prev.classList.remove("fading");
    }, 380);
  }
  window.setTimeout(function () {
    next.classList.remove("hidden");
    next.classList.add("fading");
    // force reflow so the transition runs from the faded state
    void next.offsetWidth;
    next.classList.remove("fading");
    popIn(next);
  }, prev ? 360 : 0);
}

/* number counter */
function countTo(node, target, dur, fmt) {
  var t0 = performance.now();
  function step(now) {
    var k = Math.min(1, (now - t0) / dur);
    var e = 1 - Math.pow(1 - k, 3);
    node.textContent = fmt(Math.round(target * e));
    if (k < 1) requestAnimationFrame(step);
  }
  requestAnimationFrame(step);
}
var fmtInt = function (n) { return n.toLocaleString("en-US"); };

function animateUsage() {
  var list = $("#usage-body");
  if (!list) return;
  list.classList.add("rank-grow");
  void list.offsetWidth;
  list.classList.remove("rank-grow");
  countTo($("#stat-requests"), 128431, 1700, fmtInt);
  countTo($("#stat-failed"), 1204, 1700, fmtInt);
  countTo($("#stat-input"), 892418377, 1900, fmtInt);
  countTo($("#stat-output"), 214907331, 1900, fmtInt);
}

function setLight(on) {
  if (on) {
    document.documentElement.setAttribute("data-theme", "light");
    var t = $("#setting-theme"); if (t) t.checked = true;
  } else {
    document.documentElement.removeAttribute("data-theme");
    var t2 = $("#setting-theme"); if (t2) t2.checked = false;
  }
}

function toggleSwitch(id, on) {
  var n = $("#" + id);
  if (n) n.checked = on;
}

/* the timeline */
var TIMELINE = [];
function cue(t, fn) { TIMELINE.push({ t: t, fn: fn, done: false }); }

cue(0.0, function () { show($("#intro")); });
cue(4.2, function () { hide($("#intro")); show($("#tagline")); });
cue(8.0, function () {
  hide($("#tagline"));
  show(appFit);
  appFit.setAttribute("data-state", "enter");
  Music.whoosh();
  window.setTimeout(function () {
    appFit.setAttribute("data-state", "hold");
    cam({ px: 0, py: 0, sc: 1, ry: 0 });
  }, 1400);
  setPanel("models");
  captionShow("Models", "Every model you have. One grid.");
  popIn($("#panel-models"));
});
cue(10.6, function () { cam({ px: -14, py: 8, sc: 1.03, ry: 2.5, rx: -1 }); });
cue(15.0, function () {
  captionShow("Providers", "Bring your own keys. 60+ presets ready.");
  cam({ px: 16, py: -6, sc: 1.05, ry: -3, rx: 1 });
  setPanel("providers");
  Music.whoosh();
});
cue(21.5, function () {
  captionShow("Virtual Models", "Bundle models under one API name.");
  cam({ px: -10, py: -10, sc: 1.02, ry: 2, rx: 1.5 });
  setPanel("virtual");
  Music.whoosh();
});
cue(27.5, function () {
  captionShow("API", "One OpenAI-compatible endpoint. Any client.");
  cam({ px: 12, py: 6, sc: 1.06, ry: -2.5, rx: -1 });
  setPanel("api");
  Music.whoosh();
});
cue(29.0, function () { toggleSwitch("api-enabled", true); Music.ping(880); });
cue(30.0, function () { toggleSwitch("api-expose", true); Music.ping(1174); });
cue(31.0, function () { toggleSwitch("api-expose-all", true); Music.ping(1397);
  var s = $("#api-saved"); if (s) { s.classList.add("show"); window.setTimeout(function () { s.classList.remove("show"); }, 1500); }
});
cue(34.0, function () {
  captionShow("Usage", "Every token, accounted for.");
  cam({ px: -16, py: -4, sc: 1.04, ry: 3, rx: -1.5 });
  setPanel("usage");
  Music.whoosh();
  window.setTimeout(animateUsage, 520);
});
cue(41.0, function () {
  captionShow("Settings", "Yours to tune. Light mode included.");
  cam({ px: 14, py: 10, sc: 1.03, ry: -2, rx: 1 });
  setPanel("settings");
  Music.whoosh();
});
cue(43.2, function () { setLight(true); Music.ping(1318); });
cue(45.0, function () {
  var c = $("#setting-compress"); if (c) c.checked = true;
  var r = $("#setting-compress-strength");
  var v = $("#setting-compress-strength-value");
  if (r && v) {
    var n = 3;
    var iv = window.setInterval(function () {
      n++; r.value = String(n); v.textContent = String(n);
      if (n >= 7) window.clearInterval(iv);
    }, 160);
  }
});
cue(46.6, function () { setLight(false); Music.ping(988); });
cue(47.8, function () {
  captionShow("Docker · Multi-User", "One port. Everyone gets their own profile.");
  cam({ px: 0, py: 0, sc: 1.08, ry: 0, rx: 0 });
  show($("#auth"));
  var u = $("#tb-user"); if (u) u.classList.remove("hidden");
  Music.whoosh();
});
cue(53.5, function () {
  hide($("#auth"));
  var u = $("#tb-user"); if (u) u.classList.add("hidden");
  captionHide();
  appFit.setAttribute("data-state", "out");
  Music.whoosh();
  window.setTimeout(function () { show($("#platforms")); }, 520);
});
cue(60.0, function () {
  hide($("#platforms"));
  show($("#endcard"));
  Music.whoosh();
});

/* ---------------------------------------------------------------
   4. Music — emotional cinematic score, Web Audio API
---------------------------------------------------------------- */
var Music = (function () {
  var ctx = null, master = null, comp = null, conv = null, wet = null;
  var muted = false;
  var startTime = 0;
  var step = 0, nextTime = 0;
  var timer = null;
  var fired = {};
  var BPM = 78;
  var STEP = 60 / BPM / 4;          // 16th note
  var BAR = STEP * 16;

  // Dm — Bb — F — C  (i — VI — III — VII), warm and hopeful
  var CHORDS = [
    { pad: [50, 57, 62, 65], bass: 38, arp: [62, 65, 69, 72] },   // Dm
    { pad: [46, 53, 58, 62], bass: 34, arp: [58, 62, 65, 70] },   // Bb
    { pad: [48, 53, 57, 60], bass: 41, arp: [60, 65, 69, 72] },   // F/C
    { pad: [48, 55, 60, 64], bass: 36, arp: [64, 67, 72, 76] }    // C
  ];
  var ARP_UP = [0, 1, 2, 3, 2, 1, 2, 3];

  function mtof(m) { return 440 * Math.pow(2, (m - 69) / 12); }

  function makeIR() {
    var len = Math.floor(ctx.sampleRate * 2.9);
    var buf = ctx.createBuffer(2, len, ctx.sampleRate);
    for (var ch = 0; ch < 2; ch++) {
      var d = buf.getChannelData(ch);
      for (var i = 0; i < len; i++) {
        d[i] = (Math.random() * 2 - 1) * Math.pow(1 - i / len, 2.7);
      }
    }
    return buf;
  }

  function init() {
    var AC = window.AudioContext || window.webkitAudioContext;
    if (!AC) return false;
    ctx = new AC();
    master = ctx.createGain();
    master.gain.value = 0;
    comp = ctx.createDynamicsCompressor();
    comp.threshold.value = -16;
    comp.ratio.value = 3;
    conv = ctx.createConvolver();
    conv.buffer = makeIR();
    wet = ctx.createGain();
    wet.gain.value = 0.42;
    master.connect(comp);
    comp.connect(ctx.destination);
    master.connect(conv);
    conv.connect(wet);
    wet.connect(comp);
    master.gain.setTargetAtTime(0.9, ctx.currentTime, 1.4);
    return true;
  }

  function out(node, revAmt) {
    var g = ctx.createGain();
    g.gain.value = revAmt === undefined ? 0.3 : revAmt;
    node.connect(g);
    g.connect(master);
    var send = ctx.createGain();
    send.gain.value = 0.55;
    g.connect(send);
    send.connect(conv);
    return g;
  }

  /* --- instruments --- */
  function pad(freq, t, dur, gain) {
    var g = ctx.createGain();
    g.gain.setValueAtTime(0.0001, t);
    g.gain.exponentialRampToValueAtTime(gain, t + dur * 0.35);
    g.gain.exponentialRampToValueAtTime(0.0001, t + dur);
    var f = ctx.createBiquadFilter();
    f.type = "lowpass";
    f.frequency.setValueAtTime(500, t);
    f.frequency.linearRampToValueAtTime(1500, t + dur * 0.6);
    f.Q.value = 0.7;
    [-6, 6].forEach(function (cents) {
      var o = ctx.createOscillator();
      o.type = "sawtooth";
      o.frequency.value = freq;
      o.detune.value = cents;
      o.connect(f);
      o.start(t);
      o.stop(t + dur + 0.1);
    });
    f.connect(g);
    out(g, 0.5);
  }

  function piano(freq, t, gain, dur) {
    dur = dur || 2.6;
    var g = ctx.createGain();
    g.gain.setValueAtTime(0.0001, t);
    g.gain.exponentialRampToValueAtTime(gain, t + 0.008);
    g.gain.exponentialRampToValueAtTime(0.0001, t + dur);
    var o1 = ctx.createOscillator(); o1.type = "triangle"; o1.frequency.value = freq;
    var o2 = ctx.createOscillator(); o2.type = "sine"; o2.frequency.value = freq * 2;
    var g2 = ctx.createGain(); g2.gain.value = 0.28; o2.connect(g2);
    var f = ctx.createBiquadFilter(); f.type = "lowpass";
    f.frequency.setValueAtTime(4200, t);
    f.frequency.exponentialRampToValueAtTime(900, t + dur);
    o1.connect(f); g2.connect(f);
    f.connect(g);
    out(g, 0.6);
    o1.start(t); o2.start(t);
    o1.stop(t + dur + 0.05); o2.stop(t + dur + 0.05);
  }

  function bass(freq, t, dur) {
    var g = ctx.createGain();
    g.gain.setValueAtTime(0.0001, t);
    g.gain.exponentialRampToValueAtTime(0.34, t + 0.05);
    g.gain.exponentialRampToValueAtTime(0.0001, t + dur);
    var o = ctx.createOscillator(); o.type = "sine"; o.frequency.value = freq;
    var o2 = ctx.createOscillator(); o2.type = "triangle"; o2.frequency.value = freq;
    var g2 = ctx.createGain(); g2.gain.value = 0.1; o2.connect(g2);
    o.connect(g); g2.connect(g);
    out(g, 0.05);
    o.start(t); o2.start(t);
    o.stop(t + dur + 0.05); o2.stop(t + dur + 0.05);
  }

  function kick(t) {
    var o = ctx.createOscillator();
    var g = ctx.createGain();
    o.type = "sine";
    o.frequency.setValueAtTime(120, t);
    o.frequency.exponentialRampToValueAtTime(42, t + 0.16);
    g.gain.setValueAtTime(0.0001, t);
    g.gain.exponentialRampToValueAtTime(0.5, t + 0.012);
    g.gain.exponentialRampToValueAtTime(0.0001, t + 0.32);
    o.connect(g); out(g, 0.1);
    o.start(t); o.stop(t + 0.36);
  }

  function shaker(t) {
    var len = Math.floor(ctx.sampleRate * 0.06);
    var buf = ctx.createBuffer(1, len, ctx.sampleRate);
    var d = buf.getChannelData(0);
    for (var i = 0; i < len; i++) d[i] = (Math.random() * 2 - 1) * (1 - i / len);
    var src = ctx.createBufferSource(); src.buffer = buf;
    var f = ctx.createBiquadFilter(); f.type = "highpass"; f.frequency.value = 6500;
    var g = ctx.createGain(); g.gain.value = 0.16;
    src.connect(f); f.connect(g); out(g, 0.12);
    src.start(t);
  }

  function noiseBuf(dur) {
    var len = Math.floor(ctx.sampleRate * dur);
    var buf = ctx.createBuffer(1, len, ctx.sampleRate);
    var d = buf.getChannelData(0);
    for (var i = 0; i < len; i++) d[i] = Math.random() * 2 - 1;
    return buf;
  }

  function riser(t, dur) {
    var src = ctx.createBufferSource(); src.buffer = noiseBuf(dur);
    var f = ctx.createBiquadFilter(); f.type = "bandpass"; f.Q.value = 2.2;
    f.frequency.setValueAtTime(240, t);
    f.frequency.exponentialRampToValueAtTime(7200, t + dur);
    var g = ctx.createGain();
    g.gain.setValueAtTime(0.0001, t);
    g.gain.exponentialRampToValueAtTime(0.3, t + dur * 0.92);
    g.gain.exponentialRampToValueAtTime(0.0001, t + dur);
    src.connect(f); f.connect(g); out(g, 0.5);
    src.start(t); src.stop(t + dur + 0.05);
  }

  function whoosh() {
    if (!ctx) return;
    var t = ctx.currentTime + 0.02;
    var dur = 0.62;
    var src = ctx.createBufferSource(); src.buffer = noiseBuf(dur);
    var f = ctx.createBiquadFilter(); f.type = "bandpass"; f.Q.value = 1.1;
    f.frequency.setValueAtTime(3800, t);
    f.frequency.exponentialRampToValueAtTime(320, t + dur);
    var g = ctx.createGain();
    g.gain.setValueAtTime(0.0001, t);
    g.gain.exponentialRampToValueAtTime(0.16, t + 0.07);
    g.gain.exponentialRampToValueAtTime(0.0001, t + dur);
    src.connect(f); f.connect(g); out(g, 0.6);
    src.start(t); src.stop(t + dur + 0.05);
  }

  function ping(freq) {
    if (!ctx) return;
    piano(freq, ctx.currentTime + 0.02, 0.16, 1.8);
  }

  function impact(t) {
    var o = ctx.createOscillator(); var g = ctx.createGain();
    o.type = "sine";
    o.frequency.setValueAtTime(78, t);
    o.frequency.exponentialRampToValueAtTime(30, t + 1.4);
    g.gain.setValueAtTime(0.0001, t);
    g.gain.exponentialRampToValueAtTime(0.55, t + 0.02);
    g.gain.exponentialRampToValueAtTime(0.0001, t + 1.6);
    o.connect(g); out(g, 0.7);
    o.start(t); o.stop(t + 1.7);
    // bright shimmer on top of the hit
    [72, 77, 81].forEach(function (m, i) {
      piano(mtof(m + 12), t + 0.04 + i * 0.06, 0.14, 3.4);
    });
  }

  /* --- scheduler --- */
  function section(elapsed) {
    if (elapsed < 8) return "intro";
    if (elapsed < 27.5) return "build";
    if (elapsed < 56) return "lift";
    if (elapsed < 60.5) return "climax";
    return "resolve";
  }

  function scheduleStep(i, t) {
    var elapsed = t - startTime;
    if (elapsed > TOTAL + 2) return;
    var bar = Math.floor(i / 16);
    var pos = i % 16;
    var ch = CHORDS[bar % 4];
    var sec = section(elapsed);

    /* sustained pad — every bar */
    if (pos === 0) {
      var padGain = sec === "intro" ? 0.035 : sec === "resolve" ? 0.075 : 0.055;
      var padDur = BAR * 1.02;
      ch.pad.forEach(function (m) { pad(mtof(m), t, padDur, padGain); });
      bass(mtof(ch.bass), t, BAR * 0.92);
    }

    /* piano figure */
    if (sec === "intro") {
      if (pos === 0) piano(mtof(ch.arp[0]), t, 0.2, 3.4);
      if (pos === 6) piano(mtof(ch.arp[2]), t, 0.15, 3.0);
      if (pos === 10) piano(mtof(ch.arp[1]), t, 0.13, 2.6);
    }

    /* arpeggio */
    if (sec === "build" || sec === "lift" || sec === "climax") {
      var dense = (sec === "build") ? (pos % 2 === 0) : true;
      if (dense) {
        var idx = ARP_UP[(i >> 1) % ARP_UP.length];
        var oct = (sec === "lift" && pos % 4 === 0) ? 12 : 0;
        var g = sec === "climax" ? 0.115 : 0.085;
        piano(mtof(ch.arp[idx] + oct), t, g, sec === "build" ? 1.7 : 2.1);
      }
    }

    /* resolve — long hopeful melody + shimmer */
    if (sec === "resolve" && pos === 0) {
      piano(mtof(ch.arp[0] + 12), t, 0.2, 5.0);
    }
    if (sec === "resolve" && pos === 8) {
      piano(mtof(ch.arp[2] + 12), t, 0.16, 5.0);
    }

    /* pulse */
    if (sec === "lift" || sec === "climax") {
      if (pos === 0 || pos === 8) kick(t);
      if (pos % 4 === 2) shaker(t);
    }

    /* one-shot cues */
    if (!fired.riser && elapsed >= 56.0) { fired.riser = true; riser(ctx.currentTime, 4.4); }
    if (!fired.hit && elapsed >= 60.3) { fired.hit = true; impact(t); }
  }

  function tick() {
    if (!ctx) return;
    while (nextTime < ctx.currentTime + 0.25) {
      scheduleStep(step, nextTime);
      nextTime += STEP;
      step++;
    }
    // final fade
    var el = ctx.currentTime - startTime;
    if (el > TOTAL - 7) {
      master.gain.setTargetAtTime(0.0001, ctx.currentTime, 2.4);
    }
  }

  function start() {
    if (!init()) return;
    startTime = ctx.currentTime + 0.06;
    nextTime = startTime;
    step = 0;
    fired = {};
    timer = window.setInterval(tick, 40);
    tick();
  }

  function stop() {
    if (timer) window.clearInterval(timer);
    timer = null;
    if (ctx) { try { ctx.close(); } catch (e) { /* ignore */ } }
    ctx = null;
  }

  function toggleMute() {
    muted = !muted;
    if (ctx && master) {
      master.gain.setTargetAtTime(muted ? 0.0001 : 0.9, ctx.currentTime, 0.08);
    }
    return muted;
  }

  /* --- capture hook (used by the MP4 render pass; inert otherwise) --- */
  function attachCapture() {
    if (!ctx || !comp) return null;
    if (!capDest) {
      capDest = ctx.createMediaStreamDestination();
      comp.connect(capDest);
    }
    return capDest.stream;
  }
  var capDest = null;

  return { start: start, stop: stop, whoosh: whoosh, ping: ping, toggleMute: toggleMute,
           attachCapture: attachCapture,
           isMuted: function () { return muted; } };
})();

/* ---------------------------------------------------------------
   5. Playback control
---------------------------------------------------------------- */
var running = false;
var rafId = null;
var t0 = 0;

function fmtTime(s) {
  var m = Math.floor(s / 60);
  var r = Math.floor(s % 60);
  return m + ":" + (r < 10 ? "0" : "") + r;
}

function loop(now) {
  if (!running) return;
  var elapsed = (now - t0) / 1000;

  for (var i = 0; i < TIMELINE.length; i++) {
    var c = TIMELINE[i];
    if (!c.done && elapsed >= c.t) { c.done = true; c.fn(); }
  }

  var p = Math.min(1, elapsed / TOTAL);
  $("#progress-fill").style.width = (p * 100).toFixed(2) + "%";
  $("#ctl-time").textContent = fmtTime(Math.min(elapsed, TOTAL));

  if (elapsed >= TOTAL) { running = false; return; }
  rafId = requestAnimationFrame(loop);
}

function resetScenes() {
  TIMELINE.forEach(function (c) { c.done = false; });
  hide($("#intro")); hide($("#tagline")); hide($("#platforms")); hide($("#endcard"));
  hide($("#auth")); captionHide();
  var u = $("#tb-user"); if (u) u.classList.add("hidden");
  setLight(false);
  toggleSwitch("api-enabled", false);
  toggleSwitch("api-expose", false);
  toggleSwitch("api-expose-all", false);
  var r = $("#setting-compress"); if (r) r.checked = false;
  var sl = $("#setting-compress-strength"); var sv = $("#setting-compress-strength-value");
  if (sl && sv) { sl.value = "3"; sv.textContent = "3"; }
  ["stat-requests", "stat-failed", "stat-input", "stat-output"].forEach(function (id) {
    var n = $("#" + id); if (n) n.textContent = "0";
  });
  $("#progress-fill").style.width = "0%";
  appFit.classList.remove("on");
  appFit.setAttribute("data-state", "enter");
  cam({ px: 0, py: 0, sc: 1, ry: 0, rx: 0 });
}

function play() {
  cancelAnimationFrame(rafId);
  Music.stop();
  resetScenes();
  currentPanel = null;
  // reset panels to models
  $$(".panel").forEach(function (p, i) {
    p.classList.toggle("hidden", i !== 0);
    p.classList.remove("fading");
  });
  $$(".nav-item[data-panel]").forEach(function (b) {
    b.classList.toggle("active", b.getAttribute("data-panel") === "models");
  });
  currentPanel = $("#panel-models");
  hydrateIcons();

  running = true;
  t0 = performance.now();
  Music.start();
  $("#controls").classList.add("show");
  rafId = requestAnimationFrame(loop);
}

/* ---------------------------------------------------------------
   6. Wire up
---------------------------------------------------------------- */
function boot() {
  hydrateIcons();
  buildModelCards();
  buildProviderCards();
  buildVirtualCards();
  buildUsage();
  buildTabOrder();
  hydrateIcons();   // again for the injected glyphs
  currentPanel = $("#panel-models");

  // staggered letter animations for wordmark + end card
  $$("#intro .intro-word span").forEach(function (s, i) {
    s.style.animationDelay = (700 + i * 75) + "ms";
  });
  $$("#end-avail span").forEach(function (s, i) {
    s.style.animationDelay = (1400 + i * 70) + "ms";
  });

  $("#start-btn").addEventListener("click", function () {
    $("#start").classList.add("hide");
    play();
  });

  $("#btn-replay").addEventListener("click", function (e) { e.stopPropagation(); play(); });
  $("#btn-mute").addEventListener("click", function (e) {
    e.stopPropagation();
    var m = Music.toggleMute();
    $("#btn-mute").innerHTML = m ? "&#128263;" : "&#128266;";
  });

  document.addEventListener("keydown", function (e) {
    if (e.code === "Space") { e.preventDefault(); if ($("#start").classList.contains("hide")) play(); }
    if (e.key === "m" || e.key === "M") { $("#btn-mute").click(); }
    if (e.key === "r" || e.key === "R") { if ($("#start").classList.contains("hide")) play(); }
  });

  // keep controls visible while the pointer is near them
  var ctl = $("#controls");
  document.addEventListener("mousemove", function (e) {
    if (e.clientY > window.innerHeight - 90) ctl.classList.add("show");
  });
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", boot);
} else {
  boot();
}
