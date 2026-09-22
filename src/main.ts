import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import PROVIDER_PRESETS_JSON from "./provider-presets.json";
import "./ui.css";

/* ================= Types ================= */

interface ModelEntry { id: string; name: string; enabled: boolean; contextLength?: number; starred?: boolean; inputModalities?: string[]; outputModalities?: string[]; inputPricePerMtok?: number | null; outputPricePerMtok?: number | null; }
interface Provider { id: string; baseUrl: string; apiFormat: string; hasKey: boolean; models: ModelEntry[]; status?: string; logo?: string; }
interface VirtualEntry { id: string; models: string[]; logo?: string; policy?: { strategy?: string; tiers?: string[][] }; }
interface ApiSettings { port: number; enabled: boolean; exposeLan: boolean; exposeAllModels: boolean; }
interface CircuitBreakerSettings { enabled: boolean; threshold: number; cooldownSecs: number; }
interface RoutingSettings { strategy: string; latencyWindow: number; }
interface HealthSettings { maxHistoryPerProvider: number; }
interface CompressSettings { enabled: boolean; strength: number; }
interface ResponseCacheSettings { enabled: boolean; ttlSecs: number; maxEntries: number; }
interface FailoverBudgetSettings { maxAttempts: number; firstTokenTimeoutSecs: number; }
interface PublicConfig { providers: Provider[]; api: ApiSettings; virtualModels?: VirtualEntry[]; localKey: string | null; circuitBreaker?: CircuitBreakerSettings; routing?: RoutingSettings; health?: HealthSettings; compress?: CompressSettings; responseCache?: ResponseCacheSettings; failoverBudget?: FailoverBudgetSettings; dailyBudgetUsd?: number | null; }
interface ProxyStatus { running: boolean; port: number | null; baseUrl: string; lanUrl: string; error?: string | null; }
interface SaveResult { config: PublicConfig; status: ProxyStatus; }
interface UsageEntry {
  providerId: string;
  modelId: string;
  requestsOk: number;
  requestsFailed: number;
  inputTokens: number;
  outputTokens: number;
  estimated: boolean;
  lastUsedMs: number;
  lastTps: number;
  costUsd?: number;
}
interface UsageDaily {
  day: string;
  requestsOk: number;
  requestsFailed: number;
  inputTokens: number;
  outputTokens: number;
}
interface TestResult { ok: boolean; message: string; latency_ms: number; }

/* ================= State ================= */

let cfg: PublicConfig = { providers: [], api: { port: 5000, enabled: true, exposeLan: false, exposeAllModels: true }, localKey: null, circuitBreaker: { enabled: false, threshold: 3, cooldownSecs: 60 }, routing: { strategy: "weighted", latencyWindow: 20 }, health: { maxHistoryPerProvider: 100 }, compress: { enabled: false, strength: 3 }, responseCache: { enabled: false, ttlSecs: 300, maxEntries: 50 }, failoverBudget: { maxAttempts: 0, firstTokenTimeoutSecs: 0 } };
let status: ProxyStatus = { running: false, port: null, baseUrl: "", lanUrl: "" };
// Toggle inputs the user flipped since the last successful API-settings save;
// renderApiPanel must not overwrite them (nor focused inputs).
const apiToggleDirty = new Set<string>();

interface ApiKeyInfo { key: string; isDefault: boolean; limitTokens?: number | null; allowedModels?: string[] | null; rpmLimit?: number | null; expiresAt?: string | null; isAdmin?: boolean; }
let apiSaveTimer: number | undefined;

// Batch rapid render calls so fast clicks can't flood the DOM.
let renderModelCardsScheduled = false;
function scheduleRenderModelCards(): void {
  if (renderModelCardsScheduled) { return; }
  renderModelCardsScheduled = true;
  window.requestAnimationFrame(function () {
    renderModelCardsScheduled = false;
    renderModelCards();
  });
}
let renderUsageScheduled = false;
function scheduleRenderUsage(): void {
  if (renderUsageScheduled) { return; }
  renderUsageScheduled = true;
  window.requestAnimationFrame(function () {
    renderUsageScheduled = false;
    renderUsage(lastUsageEntries);
  });
}
let lastUsageEntries: UsageEntry[] = [];
let lastCheckProviders = 0;

/* ================= Helpers ================= */

function $(sel: string): HTMLElement { return document.querySelector(sel) as HTMLElement; }

// Wire an optional element without letting a missing one abort startup.
function wireIf<T extends HTMLElement>(sel: string, wire: (node: T) => void): void {
  const node = document.querySelector(sel) as T | null;
  if (node) { wire(node); }
}

function icon(name: string): string {
  const map: Record<string, string> = {
    minimize: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="currentColor" d="M2 6.5h8v.5h-8z"/></svg>',
    maximize: '<svg viewBox="0 0 12 12" aria-hidden="true"><rect x="2" y="2" width="8" height="8" fill="none" stroke="currentColor" stroke-width="1.2"/></svg>',
    restore: '<svg viewBox="0 0 12 12" aria-hidden="true"><rect x="3" y="3" width="7" height="7" fill="none" stroke="currentColor" stroke-width="1.2"/><path fill="none" stroke="currentColor" stroke-width="1.2" d="M3 6v3h3"/></svg>',
    close: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M2.5 2.5l7 7M9.5 2.5l-7 7"/></svg>',
    add: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M6 2v8M2 6h8"/></svg>',
    edit: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M8.5 1.5l2 2-6 6H2.5v-2z"/></svg>',
    delete: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M2 3h8M4 3V2h4v1M3 3l.5 7h5L9 3"/></svg>',
    upload: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M6 2v5M3 5l3 3 3-3M2 9h8"/></svg>',
    refresh: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M10 6A4 4 0 1 1 6 2"/><path fill="none" stroke="currentColor" stroke-width="1.2" d="M10 2v3h-3"/></svg>',
    copy: '<svg viewBox="0 0 12 12" aria-hidden="true"><rect x="4" y="4" width="6" height="6" fill="none" stroke="currentColor" stroke-width="1.2"/><path fill="none" stroke="currentColor" stroke-width="1.2" d="M8 4V2H2a2 2 0 0 0 0 4h2"/></svg>',
    eye: '<svg viewBox="0 0 12 12" aria-hidden="true"><circle cx="6" cy="6" r="2.2" fill="none" stroke="currentColor" stroke-width="1.2"/><path fill="none" stroke="currentColor" stroke-width="1.2" d="M1 6s2-4 5-4 5 4 5 4-2 4-5 4-5-4-5-4z"/></svg>',
    eyeOff: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M1 6s2-4 5-4 5 4 5 4-2 4-5 4-5-4-5-4z"/><path fill="none" stroke="currentColor" stroke-width="1.2" d="M2 2l8 8"/></svg>',
    person: '<svg viewBox="0 0 12 12" aria-hidden="true"><circle cx="6" cy="4.5" r="2.2" fill="none" stroke="currentColor" stroke-width="1.2"/><path fill="none" stroke="currentColor" stroke-width="1.2" d="M1 11c0-2.8 2.2-5 5-5s5 2.2 5 5"/></svg>',
    list: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M2 3h8M2 6h8M2 9h8"/></svg>',
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
    starFilled: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="#ffc85a" d="M6 1.5l1.6 3.2 3.5.5-2.5 2.5.6 3.5L6 9l-3.2 1.7.6-3.5L1 5.2l3.5-.5z"/></svg>',
    star: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M6 1.5l1.6 3.2 3.5.5-2.5 2.5.6 3.5L6 9l-3.2 1.7.6-3.5L1 5.2l3.5-.5z"/></svg>',
    search: '<svg viewBox="0 0 12 12" aria-hidden="true"><circle cx="5" cy="5" r="3.5" fill="none" stroke="currentColor" stroke-width="1.2"/><path fill="none" stroke="currentColor" stroke-width="1.2" d="M7.5 7.5l2.5 2.5"/></svg>',
    logout: '<svg viewBox="0 0 12 12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.2" d="M7 2H3v8h4M5 6h6M9.5 4.5L11 6l-1.5 1.5"/></svg>',
  };
  return map[name] || "";
}

function el(html: string): HTMLElement {
  const t = document.createElement("template");
  t.innerHTML = html.trim();
  return t.content.firstElementChild as HTMLElement;
}

function esc(s: unknown): string {
  return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}

/* Human-readable text for unknown invoke() errors. */
function errText(e: unknown): string {
  if (e instanceof Error) { return e.message; }
  if (typeof e === "string") { return e; }
  if (e && typeof e === "object") {
    try { return JSON.stringify(e); } catch { /* fall through */ }
  }
  return String(e);
}

/* localStorage can throw (private mode / quota); never let that crash the app. */
function safeLocalStorageGet(key: string): string | null {
  try { return window.localStorage.getItem(key); } catch { return null; }
}
function safeLocalStorageSet(key: string, value: string): void {
  try { window.localStorage.setItem(key, value); } catch { /* ignore */ }
}

/* Only allow logo sources we control: bundled logos or data-URI PNGs. */
function safeLogoSrc(src: string | undefined | null): string {
  if (!src) { return ""; }
  if (src.startsWith("data:image/png")) { return src; }
  const clean = src.replace(/\\/g, "/");
  if (clean.startsWith("logos/") && clean.endsWith(".png") && clean.indexOf("..") === -1) { return clean; }
  return "";
}

/* Shared timing constants. */
const TOAST_MS = 3200;
const ARM_MS = 2600;
const API_SAVE_MS = 600;
const SAVED_NOTE_MS = 1800;
const POLL_MS = 3000;

interface ToastItem { msg: string; kind: "info" | "err"; }
const toastQueue: ToastItem[] = [];
let toastTimer: number | undefined;
let toastShowing = false;

function toast(msg: string, kind: "info" | "err" = "info"): void {
  toastQueue.push({ msg: msg, kind: kind });
  if (!toastShowing) { showNextToast(); }
}

function showNextToast(): void {
  const item = toastQueue.shift();
  if (!item) { toastShowing = false; return; }
  toastShowing = true;
  let t = document.getElementById("toast");
  if (!t) {
    t = el('<div id="toast" role="status" aria-live="polite"></div>');
    document.body.appendChild(t);
  }
  t.textContent = item.msg;
  t.classList.toggle("err", item.kind === "err");
  t.classList.add("show");
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(function () {
    t!.classList.remove("show");
    // Let the hide transition finish before showing the next toast.
    window.setTimeout(showNextToast, 200);
  }, TOAST_MS);
}

async function api<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    toast("Error: " + errText(e), "err");
    throw e;
  }
}

function copyText(text: string): void {
  const done = () => toast("Copied to clipboard");
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText(text).then(done).catch(() => fallbackCopy(text, done));
  } else {
    fallbackCopy(text, done);
  }
}

function fallbackCopy(text: string, done: () => void): void {
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  try { document.execCommand("copy"); done(); } catch { toast("Copy failed"); }
  ta.remove();
}

/* ================= Rendering ================= */

/* ---- Shared provider-health snapshot (probe history + open breakers).
   Loaded at most once a minute so the 3s poll can diff cheaply. ---- */
interface HealthData { history: Record<string, Array<[number, boolean]>>; circuit?: Record<string, number>; }
let healthData: HealthData = { history: {}, circuit: {} };
let healthFetchedAt = 0;
let lastHealthSig = "";
const HEALTH_TTL_MS = 60_000;
// A probe result younger than this counts as "fresh"; older greens fade to yellow.
const FRESH_PROBE_MS = 15 * 60_000;

async function refreshHealthData(force?: boolean): Promise<void> {
  if (!force && Date.now() - healthFetchedAt < HEALTH_TTL_MS) { return; }
  try {
    const h = await api<HealthData>("get_health_history");
    healthData = { history: h.history || {}, circuit: h.circuit || {} };
    healthFetchedAt = Date.now();
  } catch { /* keep the previous snapshot */ }
}

// Green = fresh OK probe, yellow = stale/unknown, red = failing or breaker open.
function healthDot(p: Provider): { cls: string; breakerOpen: boolean } {
  if (healthData.circuit && healthData.circuit[p.id]) { return { cls: "err", breakerOpen: true }; }
  const hist = healthData.history[p.id] || [];
  const last = hist.length ? hist[hist.length - 1] : null;
  if (!last) {
    // No probe seen yet: fall back to the persisted status field.
    if (p.status === "ok") { return { cls: "ok", breakerOpen: false }; }
    if (p.status === "error") { return { cls: "err", breakerOpen: false }; }
    return { cls: "warn", breakerOpen: false };
  }
  if (!last[1]) { return { cls: "err", breakerOpen: false }; }
  return { cls: Date.now() - last[0] < FRESH_PROBE_MS ? "ok" : "warn", breakerOpen: false };
}

let renderProvidersBusy = false;
async function renderProviders(): Promise<void> {
  // Reentrancy guard: rapid callers (poll + user actions) must not interleave.
  if (renderProvidersBusy) { return; }
  renderProvidersBusy = true;
  try {
    await renderProvidersInner();
  } finally {
    renderProvidersBusy = false;
  }
}

async function renderProvidersInner(): Promise<void> {
  const wrap = $("#provider-grid") as HTMLElement;
  const emptyEl = document.getElementById("providers-empty");
  if (!wrap) { return; }
  wrap.querySelectorAll("img").forEach(function (img) { img.src = ""; });
  wrap.innerHTML = "";
  if (emptyEl) { emptyEl.classList.toggle("hidden", cfg.providers.length > 0); }
  // TTL-gated: the 3s poll must not force a backend round-trip per render.
  await refreshHealthData();
  for (const p of cfg.providers) {
    const enabledCount = p.models.filter(function (m) { return m.enabled; }).length;
    const dot = healthDot(p);
    // Manual circuit-breaker reset, shown only while a breaker is actually open.
    const cbResetBtn = dot.breakerOpen
      ? '<button class="ibtn act-cbreset" title="Circuit breaker open &mdash; click to reset"><span class="glyph">' + icon("refresh") + '</span></button>'
      : '';
    const card = el(
      '<div class="mcard">' +
        '<div class="m-name name-row">' + logoHtmlForProvider(p, "prov") + '<span class="name-text">' + esc(p.id) + '</span></div>' +
        '<div class="m-prov"><span class="pdot ' + dot.cls + '" title="' + (dot.breakerOpen ? "circuit breaker open" : "provider health") + '"></span>' + cbResetBtn + '<span>' + enabledCount + " / " + p.models.length + " models enabled</span></div>" +
        '<div class="m-prov" title="' + esc(p.baseUrl) + '"><span>' + esc(p.baseUrl) + " &middot; " + esc(p.apiFormat) + '</span></div>' +
        (p.hasKey ? "" : '<div style="margin-top:8px;"><span class="badge warn">no API key</span></div>') +
        '<div class="m-foot">' +
          '<span></span>' +
            '<span class="pcard-actions">' +
              '<button class="ibtn act-edit" title="Edit provider"><span class="glyph">' + icon("edit") + '</span></button>' +
              '<button class="ibtn danger act-del" title="Delete provider (click twice)"><span class="glyph">' + icon("delete") + '</span></button>' +
            '</span>' +
        '</div>' +
      '</div>'
    );
    card.addEventListener("click", function (ev) {
      if ((ev.target as HTMLElement).closest("button")) { return; }
      openProviderEditor(p);
    });
    (card.querySelector(".act-edit") as HTMLElement).addEventListener("click", function () { openProviderEditor(p); });
    const cbBtn = card.querySelector(".act-cbreset") as HTMLButtonElement | null;
    if (cbBtn) {
      cbBtn.addEventListener("click", async function () {
        if (cbBtn.disabled) { return; }
        cbBtn.disabled = true;
        try {
          await api<void>("reset_circuit_breaker", { providerId: p.id });
          toast("Circuit breaker reset");
        } catch { /* error toast already shown by api() */ }
        cbBtn.disabled = false;
        await refreshHealthData(true);
        renderProviders();
        scheduleRenderModelCards();
      });
    }
    (card.querySelector(".act-del") as HTMLElement).addEventListener("click", function (ev) {
      const btn = ev.currentTarget as HTMLElement;
      armThen(btn, async function () {
        const res = await api<SaveResult>("delete_provider", { id: p.id });
        applyResult(res);
        toast("Provider deleted");
      });
    });
    wrap.appendChild(card);
  }
  wireLogoFallbacks(wrap);
}
function armThen(btn: HTMLElement, action: () => void | Promise<void>): void {
  const b = btn as HTMLButtonElement;
  if (b.dataset.busy === "1") { return; }
  if (b.dataset.armed === "1") {
    b.dataset.armed = "";
    b.classList.remove("armed");
    b.title = b.dataset.origTitle || b.title;
    b.dataset.busy = "1";
    b.disabled = true;
    const done = function (): void {
      b.dataset.busy = "";
      b.disabled = false;
    };
    Promise.resolve().then(action).then(done, done);
    return;
  }
  b.dataset.armed = "1";
  if (!b.dataset.origTitle) { b.dataset.origTitle = b.title; }
  b.classList.add("armed");
  b.title = "Click again to confirm";
  window.setTimeout(function () {
    b.dataset.armed = "";
    b.classList.remove("armed");
    b.title = b.dataset.origTitle || b.title;
  }, ARM_MS);
}

/* ---- Connectivity tests: spinner on the card, result in a dialog ---- */

function showTestResultDialog(title: string, r: TestResult): void {
  closeDialog();
  const root = $("#dialog-root");
  const overlay = el(
    '<div class="overlay">' +
      '<div class="dialog" role="dialog" aria-modal="true" style="width:440px;">' +
        '<div class="dialog-body">' +
          '<div class="dialog-title">' + esc(title) + '</div>' +
          '<div class="test-result">' +
            '<span class="tr-status ' + (r.ok ? "ok" : "err") + '">' + (r.ok ? "Success" : "Failed") + '</span>' +
            '<span class="tr-msg">' + esc(r.message) + '</span>' +
            '<span class="tr-lat">' + esc(String(r.latency_ms)) + ' ms</span>' +
          '</div>' +
        '</div>' +
        '<div class="dialog-footer">' +
          '<button class="btn accent dlg-close">Close</button>' +
        '</div>' +
      '</div>' +
    '</div>'
  );
  root.appendChild(overlay);
  (overlay.querySelector(".dlg-close") as HTMLElement).addEventListener("click", closeDialog);
  wireOverlayClose(overlay);
}

async function runProviderTest(providerId: string, card: HTMLElement | null): Promise<void> {
  if (card) { card.classList.add("testing"); }
  try {
    const r = await invoke<TestResult>("test_provider", { providerId: providerId });
    showTestResultDialog("Test: " + providerId, r);
  } catch (e) {
    showTestResultDialog("Test: " + providerId, { ok: false, message: errText(e), latency_ms: 0 });
  } finally {
    if (card) { card.classList.remove("testing"); }
  }
}

async function runModelTest(providerId: string, modelId: string, card: HTMLElement | null): Promise<void> {
  if (card) { card.classList.add("testing"); }
  try {
    const r = await invoke<TestResult>("test_model", { providerId: providerId, modelId: modelId });
    showTestResultDialog("Test: " + modelId, r);
  } catch (e) {
    showTestResultDialog("Test: " + modelId, { ok: false, message: errText(e), latency_ms: 0 });
  } finally {
    if (card) { card.classList.remove("testing"); }
  }
}

async function runVirtualModelTest(id: string, card: HTMLElement | null): Promise<void> {
  if (card) { card.classList.add("testing"); }
  try {
    // test_virtual_model serializes camelCase, unlike test_provider/test_model.
    const r = await invoke<{ ok: boolean; message: string; latencyMs: number }>("test_virtual_model", { id: id });
    showTestResultDialog("Test: " + id, { ok: r.ok, message: r.message, latency_ms: r.latencyMs });
  } catch (e) {
    showTestResultDialog("Test: " + id, { ok: false, message: errText(e), latency_ms: 0 });
  } finally {
    if (card) { card.classList.remove("testing"); }
  }
}

// Apply a stored checkbox state unless the user is interacting with the toggle
// (it has focus) or already changed it since the last save.
function syncApiCheckbox(id: string, value: boolean): void {
  const box = document.getElementById(id) as HTMLInputElement | null;
  if (!box) { return; }
  if (document.activeElement === box || apiToggleDirty.has(id)) { return; }
  box.checked = value;
}

function renderApiPanel(): void {
  const portEl = $("#api-port") as HTMLInputElement;
  if (document.activeElement !== portEl) { portEl.value = String(cfg.api.port); }
  syncApiCheckbox("api-enabled", !!cfg.api.enabled);
  syncApiCheckbox("api-expose", !!cfg.api.exposeLan);
  syncApiCheckbox("api-expose-all", !!cfg.api.exposeAllModels);
  updateUrlPreview();
  updateExposeHint();
  renderApiKeyField();
}

/* Single-key policy: exactly one API key exists. It renders masked under
 * the Base URL; rerolling needs a confirmed second click and invalidates
 * every previously issued key (the backend also clears extra keys). */
function renderApiKeyField(): void {
  const inp = document.getElementById("api-key-input") as HTMLInputElement | null;
  if (inp) { inp.value = cfg.localKey || ""; }
}

function updateUrlPreview(): void {
  const portEl = $("#api-port") as HTMLInputElement;
  const v = parseInt(portEl.value, 10);
  const port = isNaN(v) ? cfg.api.port : v;
  $("#api-url-preview").textContent = "http://127.0.0.1:" + port + "/v1";
  updateExposeHint();
}

function updateExposeHint(): void {
  const hint = document.getElementById("expose-hint") as HTMLElement;
  if (!hint) { return; }
  const on = (document.getElementById("api-expose") as HTMLInputElement).checked;
  if (!on) {
    hint.style.display = "none";
    hint.textContent = "";
    return;
  }
  hint.style.display = "block";
  const lanUrl = status.lanUrl || ("http://<your-pc-ip>:" + (cfg.api.port) + "/v1");
  hint.textContent = "Other devices can reach the API and web UI at: " + lanUrl + " (Windows Firewall may ask for permission once.)";
}

function applyResult(res: SaveResult): void {
  cfg = res.config;
  status = res.status;
  renderProviders();
  scheduleRenderModelCards();
  renderVirtual();
  renderApiPanel();
}

// Merge ONLY the providers' status fields of a late check_providers result
// into the live cfg (matched by id); replacing the whole object here could
// revert newer user mutations made while the check was in flight.
function mergeProviderStatus(res: SaveResult): void {
  if (!res.config || !res.config.providers) { return; }
  const statusById = new Map<string, string>();
  for (const p of res.config.providers) { statusById.set(p.id, p.status || ""); }
  let changed = false;
  for (const p of cfg.providers) {
    const next = statusById.get(p.id);
    if (next === undefined) { continue; }
    if ((p.status || "") !== next) { p.status = next; changed = true; }
  }
  if (changed) {
    renderProviders();
    scheduleRenderModelCards();
  }
}

/* ================= Model cards ================= */

let usageAll: UsageEntry[] = [];

const starInFlight = new Set<string>(); // provider::model toggles currently in flight

function findModelEntry(providerId: string, modelId: string): ModelEntry | null {
  for (const p of cfg.providers) {
    if (p.id !== providerId) { continue; }
    for (const m of p.models) {
      if (m.id === modelId) { return m; }
    }
  }
  return null;
}

function name_of(m: ModelEntry): string {
  return m.name && m.name.trim() ? m.name.trim() : m.id;
}

function fmtTps(v: number): string {
  if (!v || v <= 0) { return "\u2014"; }
  return (v >= 10 ? v.toFixed(0) : v.toFixed(1)) + " tok/s";
}

function dotClass(status?: string): string {
  if (status === "ok") { return "ok"; }
  if (status === "error") { return "err"; }
  return "";
}

function fmtCtx(n: number | undefined): string {
  if (!n || n <= 0) { return ""; }
  return (n >= 1000 ? (Math.round(n / 100) / 10).toFixed(1).replace(/\.0$/, "") + "K" : String(n)) + " ctx";
}

/* ---- Provider-prefixed aliases for duplicate model ids ---- */

// Shortest prefix that keeps every provider id in the group distinguishable
// ("openai"/"ollama" -> "op"/"ol"); falls back to full ids.
function uniquePrefixesOf(provs: string[]): string[] {
  if (!provs.length) { return []; }
  const maxLen = Math.max.apply(null, provs.map(function (p) { return p.length; }).concat([1]));
  for (let len = 1; len <= maxLen; len++) {
    const prefixes = provs.map(function (p) { return p.slice(0, len); });
    if (new Set(prefixes).size === prefixes.length) { return prefixes; }
  }
  return provs.slice();
}

function groupEnabledByModelId(): Map<string, string[]> {
  const byId = new Map<string, string[]>();
  for (const p of cfg.providers) {
    for (const m of p.models) {
      if (!m.enabled) { continue; }
      const arr = byId.get(m.id) || [];
      if (arr.indexOf(p.id) < 0) { arr.push(p.id); }
      byId.set(m.id, arr);
    }
  }
  return byId;
}

// Alias like "o-gpt-4o", or null when the id lives on a single provider.
function modelAlias(providerId: string, modelId: string, byId: Map<string, string[]>): string | null {
  const provs = byId.get(modelId);
  if (!provs || provs.length < 2) { return null; }
  const prefixes = uniquePrefixesOf(provs);
  const idx = provs.indexOf(providerId);
  const prefix = idx >= 0 && prefixes[idx] !== undefined ? prefixes[idx] : providerId;
  return prefix + "-" + modelId;
}

function renderModelCards(): void {
  const grid = document.getElementById("model-grid") as HTMLElement;
  const emptyEl = document.getElementById("models-empty");
  const sub = document.getElementById("models-sub");
  if (!grid) { return; }
  const qEl = document.getElementById("model-search") as HTMLInputElement | null;
  const query = qEl ? qEl.value.trim().toLowerCase() : "";
  const sortSel = document.getElementById("model-sort") as HTMLSelectElement | null;
  const sortMode = sortSel ? sortSel.value : "name";
  // Cancel pending image loads so the renderer can GC old logos before we
  // replace the whole grid - rapid clicks otherwise pile up image decoders.
  grid.querySelectorAll("img").forEach(function (img) { img.src = ""; });
  grid.innerHTML = "";
  type Row = { p: Provider; m: ModelEntry };
  let rows: Row[] = [];
  let totalEnabled = 0;
  // Built once up front so the search can match on aliases too.
  const byId = groupEnabledByModelId();
  for (const p of cfg.providers) {
    for (const m of p.models) {
      if (!m.enabled) { continue; }
      totalEnabled += 1;
      if (query) {
        const alias = modelAlias(p.id, m.id, byId);
        const hay = (name_of(m) + " " + p.id + " " + m.id + (alias ? " " + alias : "")).toLowerCase();
        if (hay.indexOf(query) < 0) { continue; }
      }
      rows.push({ p: p, m: m });
    }
  }
  if (sortMode === "name") {
    rows.sort(function (a, b) { return name_of(a.m).localeCompare(name_of(b.m)); });
  } else if (sortMode === "context") {
    rows.sort(function (a, b) { return (b.m.contextLength || 0) - (a.m.contextLength || 0); });
  } else if (sortMode === "provider") {
    rows.sort(function (a, b) { return a.p.id === b.p.id ? name_of(a.m).localeCompare(name_of(b.m)) : a.p.id.localeCompare(b.p.id); });
  } else if (sortMode === "starred") {
    rows.sort(function (a, b) { return (b.m.starred ? 1 : 0) - (a.m.starred ? 1 : 0) || name_of(a.m).localeCompare(name_of(b.m)); });
  }
  let count = 0;
  for (const row of rows) {
    count += 1;
    const p = row.p;
    const m = row.m;
    const u = usageAll.find(function (e) { return e.providerId === p.id && e.modelId === m.id; });
    const name = name_of(m);
    const ctx = fmtCtx(m.contextLength);
    const alias = modelAlias(p.id, m.id, byId);
    const starSvg = m.starred
      ? '<svg viewBox="0 0 24 24" aria-hidden="true"><path fill="#ffc85a" d="M12 2.2l2.95 5.98 6.6.96-4.78 4.65 1.13 6.58L12 17.24l-5.9 3.1 1.13-6.57L2.45 9.14l6.6-.96z"/></svg>'
      : '<svg viewBox="0 0 24 24" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.7" stroke-linejoin="round" d="M12 3.4l2.6 5.27 5.82.85-4.21 4.1.99 5.79L12 16.66l-5.2 2.74.99-5.79-4.21-4.1 5.82-.84z"/></svg>';
    const card = el(
      '<div class="mcard' + (m.starred ? " starred" : "") + '" data-provider="' + esc(p.id) + '" data-model="' + esc(m.id) + '">' +
        '<button class="star-btn' + (m.starred ? " on" : "") + '" title="' + (m.starred ? "Unstar" : "Star") + ' (starred models are picked more often by the random router)">' + starSvg + '</button>' +
        '<div class="m-name"' + (alias ? ' title="Use this ID in API clients to target this provider\'s copy"' : '') + '>' + esc(alias || name) + '</div>' +
        '<div class="m-prov"><span class="pdot ' + healthDot(p).cls + '" title="' + (healthDot(p).breakerOpen ? "circuit breaker open" : "provider health") + '"></span>' + logoHtmlForProvider(p, "model") + '<span>' + esc(p.id) + '</span></div>' +
        '<div class="m-foot">' +
          '<span class="m-tps">' + fmtTps(u ? u.lastTps : 0) + '</span>' +
          (ctx ? '<span class="m-ctx">' + esc(ctx) + '</span>' : '') +
        '</div>' +
      '</div>'
    );
    grid.appendChild(card);
  }
  wireLogoFallbacks(grid);
  // Onboarding empty state only when there are no enabled models at all;
  // a search with zero hits gets its own "no match" note instead.
  if (query && count === 0 && totalEnabled > 0) {
    grid.appendChild(el('<div class="chart-empty" style="grid-column:1/-1;padding:24px;">No models match &quot;' + esc(query) + '&quot;</div>'));
  }
  if (emptyEl) { emptyEl.classList.toggle("hidden", totalEnabled > 0); }
  if (sub) {
    sub.textContent = query
      ? count + " of " + totalEnabled + " models match"
      : (count === 1 ? "1 model enabled" : count + " models enabled");
  }
}

let lastUsageAllJson = "";

async function refreshUsageAll(): Promise<void> {
  try {
    usageAll = await invoke<UsageEntry[]>("get_usage", { range: "all" });
  } catch { return; /* app closing */ }
  // The 3s poll would rebuild the whole grid; skip when nothing changed so
  // hover states and click targets survive.
  const json = JSON.stringify(usageAll);
  if (json === lastUsageAllJson) { return; }
  lastUsageAllJson = json;
  scheduleRenderModelCards();
}

/* ================= Provider picker & editor ================= */

interface Preset { name: string; url: string; format?: string; logo?: string; }
// Colored brand icons fetched from public favicon services (see public/logos).
const PRESET_LOGOS: Record<string, string> = {
  "Kilo Gateway": "kilo.ico",
  "OpenAI": "openai.png",
  "Anthropic": "anthropic.png",
  "DeepSeek": "deepseek.png",
  "Kimi (Moonshot)": "kimi.png",
  "Kimi China (Moonshot)": "kimi.png",
  "Kimi Code (Subscription)": "kimi.png",
  "xAI (Grok)": "xai.png",
  "Groq": "groq.png",
  "Ollama (Local)": "ollama.png",
  "LM Studio (Local)": "lmstudio.png",
  "vLLM (Local)": "vllm.png",
  "Mistral AI": "mistral.png",
  "Z.AI (Zhipu)": "zai.png",
  "Z.AI GLM Coding (Anthropic)": "zai.png",
  "Zhipu BigModel (CN)": "bigmodel.ico",
  "Gemini": "gemini.png",
  "OpenRouter": "openrouter.png",
  "MiniMax": "minimax.png",
  "Qwen (Alibaba)": "qwen.png",
  "Together AI": "together.ico",
  "Fireworks AI": "fireworks.ico",
  "DeepInfra": "deepinfra.ico",
  "Perplexity (Sonar)": "perplexity.png",
  "Cohere": "cohere.ico",
  "Cerebras": "cerebras.ico",
  "SambaNova": "sambanova.ico",
  "Novita AI": "novita.png",
  "Hugging Face Router": "huggingface.png",
  "NVIDIA NIM": "nvidia.png",
  "ByteDance Volcano Ark": "volcark.png",
  "Tencent Hunyuan": "hunyuan.png",
  "StepFun": "stepfun.png",
  "Vercel AI Gateway": "vercel.png",
  "Requesty": "requesty.png",
  "Martian": "martian.png",
  "Reka AI": "reka.png",
  "Featherless AI": "featherless.png",
  "NanoGPT": "nanogpt.png",
  "Chutes": "chutes.png",
  "Venice AI": "venice.png",
  "iFlytek Spark": "iflytek.png",
  "Jan (Local)": "jan.png",
  "LocalAI": "localai.png",
  "Nebius Token Factory": "nebius.png",
  "Crusoe Cloud": "crusoe.png",
  "AI/ML API": "aimlapi.png",
  "Glama": "glama.png",
  "AiHubMix": "aihubmix.png",
  "CometAPI": "cometapi.png",
  "Portkey AI Gateway": "portkey.png",
  "LiteLLM Proxy (Local)": "litellm.png",
  "Azure OpenAI": "azure.png",
  "Amazon Bedrock": "aws.png",
  "Sarvam AI": "sarvam.png",
  "Krutrim (Ola)": "krutrim.png",
  "OVHcloud AI Endpoints": "ovh.png",
  "Scaleway": "scaleway.png",
  "Google Vertex AI": "gcp.png",
  "OpenCode Zen": "opencode.png",
  "AI21 (Jamba)": "ai21.png",
  "Hyperbolic": "hyperbolic.png",
  "GPT4All (Local)": "gpt4all.png",
  "text-generation-webui (Local)": "tgwebui.png",
  "KoboldCpp (Local)": "koboldcpp.png",
  "Akash ML": "akash.png",
  "302.AI": "302ai.png",
  "Tencent TokenHub": "tokenhub.png",
  "Ollama Cloud": "ollama.png",
  "llamafile (Local)": "llamafile.png",
  "IONOS AI Model Hub": "ionos.png",
  "Meituan LongCat": "longcat.png",
  "Xiaomi MiMo": "mimo.png",
  "SAKURA Internet": "sakura.png",
  "Upstage Solar": "upstage.png",
  "HyperClova X (Naver)": "naver.png",
  "Aleph Alpha": "alephalpha.png",
  "StackIT AI Model Serving": "stackit.png",
  "Maritaca AI": "maritaca.png",
  "SiliconFlow": "siliconflow.png",
  "PPIO": "ppio.png",
  "ModelScope": "modelscope.png",
  "01.AI (Yi)": "lingyi.png",
  "Baichuan AI": "baichuan.png",
  "YandexGPT": "yandex.png",
  "GigaChat (Sber)": "gigachat.png",
  "NVIDIA NIM (Local)": "nvidia.png",
  "Huawei PANGU (MaaS)": "huawei.png"
};

// Gemeinsame Quelle für Desktop UND eingebettetes Web-Frontend
// (generiert nach src-tauri/web/presets.js via npm run presets).
const PROVIDER_PRESETS: Preset[] = PROVIDER_PRESETS_JSON;

function presetLogo(name: string): string | undefined {
  const f = PRESET_LOGOS[name];
  return f;
}

/* ---- Logo lookup by provider base URL ---- */

// Presets pointing at loopback endpoints all share the "localhost" /
// "127.0.0.1" host, so those keys include the port to stay distinguishable.
const LOCAL_HOST_RE = /^(localhost|127\.0\.0\.1|\[::1\])$/;

function hostKeyOf(providerUrl: string): string | null {
  try {
    const u = new URL(providerUrl.trim());
    const host = u.hostname.toLowerCase();
    if (!host) { return null; }
    return LOCAL_HOST_RE.test(host) ? host + ":" + u.port : host;
  } catch { return null; }
}

// Small host -> logo-file map derived from PROVIDER_PRESETS + PRESET_LOGOS.
const PROVIDER_HOST_LOGOS: Map<string, string> = (function () {
  const map = new Map<string, string>();
  for (const pr of PROVIDER_PRESETS) {
    const file = PRESET_LOGOS[pr.name];
    if (!file) { continue; }
    const key = hostKeyOf(pr.url);
    if (key && !map.has(key)) { map.set(key, file); }
  }
  return map;
})();

// Returns the logo filename for a provider base URL, or null when unknown.
function logoForProvider(providerUrl: string): string | null {
  const key = providerUrl ? hostKeyOf(providerUrl) : null;
  if (!key) { return null; }
  const exact = PROVIDER_HOST_LOGOS.get(key);
  if (exact) { return exact; }
  if (key.indexOf(":") >= 0) { return null; } // loopback keys never fall back
  // Parent-domain fallback so custom subdomains still match
  // (e.g. my-resource.openai.azure.com -> openai.azure.com).
  const parts = key.split(".");
  for (let i = 1; i < parts.length - 1; i++) {
    const hit = PROVIDER_HOST_LOGOS.get(parts.slice(i).join("."));
    if (hit) { return hit; }
  }
  return null;
}

// Inline logo chip markup for cards; the delegated error listener removes the chip if the image fails to load.
function logoImgHtml(file: string | null, cls: string): string {
  const src = safeLogoSrc(file ? "logos/" + file : "");
  if (!src) { return ""; }
  return '<span class="p-logowrap logo-' + cls + '"><img src="' + esc(src) + '" alt="" draggable="false" loading="lazy"></span>';
}

// Prefer a user-uploaded custom icon (data URI) over the static preset logo.
function logoHtmlForProvider(p: Provider, cls: string): string {
  const custom = safeLogoSrc(p.logo);
  if (custom) {
    return '<span class="p-logowrap logo-' + cls + '"><img src="' + esc(custom) + '" alt="" draggable="false"></span>';
  }
  return logoImgHtml(logoForProvider(p.baseUrl), cls);
}

// One delegated capture-phase error listener covers every logo image (error
// events don't bubble, hence capture); safe to call after every render.
let logoFallbackWired = false;
function wireLogoFallbacks(_scope: ParentNode): void {
  if (logoFallbackWired) { return; }
  logoFallbackWired = true;
  document.addEventListener("error", function (ev) {
    const img = ev.target;
    if (!(img instanceof HTMLImageElement)) { return; }
    const wrap = img.parentElement;
    if (wrap && wrap.classList.contains("p-logowrap")) { wrap.remove(); }
  }, true);
}

/* ---- Custom PNG icon upload, shared by the provider & virtual editors ----
 * Expects an #dlg-icon button plus an #dlg-icon-prev preview span inside the
 * dialog header. Tri-state result: undefined = keep the stored icon,
 * "" = remove it, a data URI = set it. */

function iconPreviewHtml(logo: string): string {
  const src = safeLogoSrc(logo);
  return '<span class="p-logowrap logo-prov"><img src="' + esc(src) + '" alt="" draggable="false"></span>' +
    '<button class="ibtn danger" id="dlg-icon-del" title="Remove custom icon"><span class="glyph">' + icon("delete") + '</span></button>';
}

function wireIconUpload(
  overlay: HTMLElement,
  initial: string | undefined,
): { value: () => string | null } {
  let icon: string | null | undefined = initial;
  const prev = overlay.querySelector("#dlg-icon-prev") as HTMLElement;
  function refresh(): void {
    prev.innerHTML = icon ? iconPreviewHtml(icon) : "";
    const del = prev.querySelector("#dlg-icon-del");
    if (del) {
      (del as HTMLElement).addEventListener("click", function () {
        icon = "";
        refresh();
      });
    }
  }
  (overlay.querySelector("#dlg-icon") as HTMLElement).addEventListener("click", async function () {
    try {
      const uri = await api<string>("pick_and_store_icon");
      if (uri) { icon = uri; refresh(); toast("Icon added - save to apply"); }
    } catch { /* toast already shown */ }
  });
  refresh();
  return { value: function (): string | null { return icon === undefined ? null : icon; } };
}

function openProviderPicker(): void {
  closeDialog();
  const root = $("#dialog-root");
  const overlay = el(
    '<div class="overlay">' +
      '<div class="dialog wide" role="dialog" aria-modal="true">' +
        '<div class="dialog-body">' +
          '<div class="dialog-title">Select your provider</div>' +
          '<div class="mini-toolbar">' +
            '<input id="pk-search" type="search" placeholder="Search providers..." autocomplete="off" spellcheck="false" />' +
          '</div>' +
          '<div class="dlg-panel">' +
            '<div class="preset-list" id="pk-list"></div>' +
          '</div>' +
        '</div>' +
      '</div>' +
    '</div>'
  );
  root.appendChild(overlay);
  const list = overlay.querySelector("#pk-list") as HTMLElement;
  const search = overlay.querySelector("#pk-search") as HTMLInputElement;

  function buildItem(pr: Preset): HTMLButtonElement {
    const logoFile = presetLogo(pr.name);
    const iconHtml = logoFile
      ? '<span class="p-logowrap"><img src="' + esc(safeLogoSrc("logos/" + logoFile)) + '" alt="" draggable="false"></span>'
      : '<span class="p-icon"><span class="glyph">' + icon("person") + '</span></span>';
    const item = el(
      '<button class="preset-item">' +
        iconHtml +
        '<span style="min-width:0;"><span class="pi-name">' + esc(pr.name) + '</span><br/><span class="pi-url">' + esc(pr.url) + '</span></span>' +
      '</button>'
    ) as HTMLButtonElement;
    const pimg = item.querySelector(".p-logowrap img");
    if (pimg) {
      pimg.addEventListener("error", function () {
        const wrap = item.querySelector(".p-logowrap");
        if (wrap) { wrap.outerHTML = '<span class="p-icon"><span class="glyph">' + icon("person") + '</span></span>'; }
      });
    }
    item.addEventListener("click", function () { closeDialog(); openProviderEditor(null, pr); });
    return item;
  }

  function renderList(): void {
    const q = search.value.trim().toLowerCase();
    list.innerHTML = "";
    if (!q || "custom".indexOf(q) >= 0) {
      const c = el(
        '<button class="preset-item">' +
          '<span class="p-icon">' + icon("add") + '</span>' +
          '<span style="min-width:0;"><span class="pi-name">Custom</span><br/><span class="pi-url">Enter everything manually</span></span>' +
        '</button>'
      );
      c.addEventListener("click", function () { closeDialog(); openProviderEditor(null); });
      list.appendChild(c);
    }
    // Group presets into Cloud / Local so self-hosted options are easy to spot.
    // Alphabetical inside each group keeps the list predictable.
    const byPresetName = function (a: Preset, b: Preset): number { return a.name.localeCompare(b.name); };
    const cloudPresets = PROVIDER_PRESETS.filter(function (pr) { return pr.name.indexOf("(Local)") < 0; }).sort(byPresetName);
    const localPresets = PROVIDER_PRESETS.filter(function (pr) { return pr.name.indexOf("(Local)") >= 0; }).sort(byPresetName);
    let cloudHeader: HTMLElement | null = null;
    let localHeader: HTMLElement | null = null;
    for (const pr of cloudPresets.concat(localPresets)) {
      const hay = (pr.name + " " + pr.url).toLowerCase();
      if (q && hay.indexOf(q) < 0) { continue; }
      const isLocal = pr.name.indexOf("(Local)") >= 0;
      if (isLocal) {
        if (!localHeader) { localHeader = el('<div class="preset-group">Local</div>'); list.appendChild(localHeader); }
      } else if (!cloudHeader) {
        cloudHeader = el('<div class="preset-group">Cloud</div>');
        list.appendChild(cloudHeader);
      }
      list.appendChild(buildItem(pr));
    }
    if (!list.querySelector(".preset-item")) { list.appendChild(el('<div class="chart-empty" style="padding:10px;">No match</div>')); }
  }

  // Arrow-key navigation over the visible preset items.
  function focusItem(items: HTMLButtonElement[], idx: number): void {
    items.forEach(function (x, i) { x.classList.toggle("focused", i === idx); });
    if (items[idx]) { items[idx].scrollIntoView({ block: "nearest" }); }
  }
  search.addEventListener("keydown", function (e) {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp" && e.key !== "Enter") { return; }
    const items = Array.prototype.slice.call(list.querySelectorAll<HTMLButtonElement>(".preset-item"));
    if (!items.length) { return; }
    const cur = items.findIndex(function (x) { return x.classList.contains("focused"); });
    if (e.key === "Enter") {
      if (cur >= 0) { e.preventDefault(); items[cur].click(); }
      return;
    }
    e.preventDefault();
    const next = e.key === "ArrowDown" ? Math.min(cur + 1, items.length - 1) : Math.max(cur - 1, 0);
    focusItem(items, next < 0 ? 0 : next);
  });
  search.addEventListener("input", renderList);
  wireOverlayClose(overlay);
  renderList();
  window.setTimeout(function () { search.focus(); }, 30);
}

/* ---- Dialog lifecycle ----
 * Every dialog registers its document-level handlers (Escape) through an
 * AbortController. Opening a new dialog or closing the current one aborts the
 * previous controller, so handlers can never outlive their overlay - even
 * when a dialog opener wipes #dialog-root via innerHTML=''. */
let activeDialogAbort: AbortController | null = null;

function closeDialog(): void {
  if (activeDialogAbort) { activeDialogAbort.abort(); activeDialogAbort = null; }
  const root = document.getElementById("dialog-root");
  if (root) { root.innerHTML = ""; }
}

function wireOverlayClose(overlay: HTMLElement): void {
  const ac = new AbortController();
  activeDialogAbort = ac;
  overlay.addEventListener("mousedown", function (ev) {
    if (ev.target === overlay) { closeDialog(); }
  }, { signal: ac.signal });
  document.addEventListener("keydown", function (ev) {
    if (ev.key === "Escape") { closeDialog(); }
  }, { signal: ac.signal });
}

function baseUrlOf(existing: Provider | null, preset?: Preset): string {
  if (existing) { return existing.baseUrl; }
  return preset ? preset.url : "";
}

function apiFormatOf(existing: Provider | null, preset?: Preset): string {
  if (existing) { return existing.apiFormat === "anthropic" ? "anthropic" : "openai"; }
  return preset && preset.format === "anthropic" ? "anthropic" : "openai";
}

/* Suggest a valid, collision-free provider ID derived from the preset name. */
function slugifyProviderName(name: string): string {
  return name.toLowerCase().replace(/\(local\)/g, "").replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
}
function uniqueProviderSlug(base: string): string {
  const slug = slugifyProviderName(base) || "provider";
  if (!cfg.providers.some(function (p) { return p.id === slug; })) { return slug; }
  let n = 2;
  while (cfg.providers.some(function (p) { return p.id === slug + "-" + n; })) { n += 1; }
  return slug + "-" + n;
}

function openProviderEditor(existing: Provider | null, preset?: Preset): void {
  closeDialog();
  const root = $("#dialog-root");
  const isEdit = existing !== null;
  const pid0 = isEdit && existing ? existing.id : "";
  let models: ModelEntry[] = isEdit && existing
    ? existing.models.map(function (m) { return { id: m.id, name: m.name || m.id, enabled: m.enabled, contextLength: m.contextLength, starred: !!m.starred, inputModalities: m.inputModalities, outputModalities: m.outputModalities }; })
    : [];

  const overlay = el(
    '<div class="overlay">' +
      '<div class="dialog wide" role="dialog" aria-modal="true">' +
        '<div class="dialog-body">' +
          '<div class="dialog-title" style="display:flex;align-items:center;gap:8px;">' +
            '<span style="flex:1;min-width:0;">' + (isEdit ? "Edit " + esc(pid0) : "Add Provider") + '</span>' +
            '<span id="dlg-icon-prev" style="display:inline-flex;align-items:center;gap:4px;"></span>' +
            '<button class="ibtn" id="dlg-icon" title="Upload custom icon (PNG)"><span class="glyph">' + icon("upload") + '</span></button>' +
          '</div>' +
          '<div class="dlg-panel">' +
            (isEdit ? "" : '<div class="field"><label for="dlg-pid">Provider ID (lowercase)</label><input id="dlg-pid" type="text" spellcheck="false" placeholder="e.g. openai" /></div>') +
            '<div class="field-row two-col">' +
              '<div class="field"><label for="dlg-format">Endpoint Type</label>' +
                '<select id="dlg-format">' +
                  '<option value="openai">OpenAI-Compatible</option>' +
                  '<option value="anthropic">Anthropic-Compatible</option>' +
                '</select></div>' +
              '<div class="field"><label for="dlg-burl">Base URL</label>' +
                '<input id="dlg-burl" type="text" class="mono" spellcheck="false" /></div>' +
            '</div>' +
            '<div class="field"><label for="dlg-key">API Key</label>' +
              '<input id="dlg-key" type="password" class="mono" spellcheck="false" autocomplete="off" ' +
                (isEdit ? 'placeholder="Saved - leave blank to keep"' : 'placeholder="sk-... (optional)"') + ' /></div>' +
          '</div>' +
          '<div class="dlg-sec-title">Models</div>' +
          '<div class="mini-toolbar">' +
            '<input id="dlg-mid" type="text" class="mono" spellcheck="false" placeholder="Add model by ID..." />' +
            '<button class="ibtn" id="dlg-add-model" title="Add model"><span class="glyph">' + icon("add") + '</span></button>' +
            '<button class="btn" id="dlg-fetch"><span class="glyph">' + icon("refresh") + '</span><span>Fetch</span></button>' +
          '</div>' +
          '<div class="mini-toolbar">' +
            '<input id="dlg-msearch" type="text" spellcheck="false" placeholder="Search models..." />' +
            '<button class="btn sm" id="dlg-enall">Enable all</button>' +
            '<button class="btn sm" id="dlg-disall">Disable all</button>' +
          '</div>' +
          '<div class="err-text" id="dlg-err"></div>' +
          '<div class="edit-models-grid" id="dlg-mgrid"></div>' +
        '</div>' +
        '<div class="dialog-footer">' +
          '<button class="btn" id="dlg-cancel">Cancel</button>' +
          '<button class="btn accent" id="dlg-save">Save</button>' +
        '</div>' +
      '</div>' +
    '</div>'
  );
  root.appendChild(overlay);

  const burl = overlay.querySelector("#dlg-burl") as HTMLInputElement;
  const key = overlay.querySelector("#dlg-key") as HTMLInputElement;
  const fmt = overlay.querySelector("#dlg-format") as HTMLSelectElement;
  const mid = overlay.querySelector("#dlg-mid") as HTMLInputElement;
  const errLine = overlay.querySelector("#dlg-err") as HTMLElement;
  burl.value = baseUrlOf(existing, preset);
  fmt.value = apiFormatOf(existing, preset);
  if (!isEdit && preset) {
    const pidEl = overlay.querySelector("#dlg-pid") as HTMLInputElement | null;
    if (pidEl) { pidEl.value = uniqueProviderSlug(preset.name); }
  }
  const iconCtl = wireIconUpload(overlay, isEdit && existing ? existing.logo : undefined);

  function renderGrid(): void {
    const grid = overlay.querySelector("#dlg-mgrid") as HTMLElement;
    const sEl = overlay.querySelector("#dlg-msearch") as HTMLInputElement | null;
    const q = sEl ? sEl.value.trim().toLowerCase() : "";
    grid.innerHTML = "";
    let shown = 0;
    for (const m of models) {
      if (!q || (m.name + " " + m.id).toLowerCase().indexOf(q) >= 0) { shown += 1; }
    }
    if (q && !shown) {
      grid.appendChild(el('<div class="chart-empty" style="padding:8px;">No models match.</div>'));
      return;
    }
    if (!models.length) {
      grid.appendChild(el('<div class="chart-empty" style="padding:8px;">No models yet - fetch or add by ID.</div>'));
      return;
    }
    models.forEach(function (m, idx) {
      if (q && (m.name + " " + m.id).toLowerCase().indexOf(q) < 0) { return; }
      const ctx = fmtCtx(m.contextLength);
      const row = el(
        '<div class="mrow">' +
          '<input type="checkbox" class="switch" title="Enabled" />' +
          '<span class="mrow-name">' + esc(m.name || m.id) + '</span>' +
          '<span class="model-id">' + esc(m.id) + '</span>' +
          (ctx ? '<span class="model-id">' + esc(ctx) + '</span>' : '') +
          '<button class="ibtn danger m-del" title="Delete model"><span class="glyph">' + icon("delete") + '</span></button>' +
        '</div>'
      );
      const sw = row.querySelector("input.switch") as HTMLInputElement;
      sw.checked = m.enabled;
      sw.addEventListener("change", function () { models[idx].enabled = sw.checked; });
      (row.querySelector(".m-del") as HTMLElement).addEventListener("click", function () {
        models.splice(idx, 1);
        renderGrid();
      });
      grid.appendChild(row);
    });
  }
  const msearch = overlay.querySelector("#dlg-msearch") as HTMLInputElement;
  msearch.addEventListener("input", renderGrid);
  (overlay.querySelector("#dlg-enall") as HTMLElement).addEventListener("click", function () {
    models.forEach(function (m) { m.enabled = true; });
    renderGrid();
  });
  (overlay.querySelector("#dlg-disall") as HTMLElement).addEventListener("click", function () {
    models.forEach(function (m) { m.enabled = false; });
    renderGrid();
  });
  renderGrid();

  function addManual(): void {
    const id = mid.value.trim();
    if (!id) { errLine.textContent = "Enter a Model ID first."; return; }
    if (id.indexOf("::") >= 0) { errLine.textContent = "Model ID may not contain '::' (reserved separator)."; return; }
    errLine.textContent = "";
    if (!models.some(function (m) { return m.id === id; })) {
      models.push({ id: id, name: id, enabled: true, contextLength: undefined });
    }
    mid.value = "";
    mid.focus();
    renderGrid();
  }
  (overlay.querySelector("#dlg-add-model") as HTMLElement).addEventListener("click", addManual);
  mid.addEventListener("keydown", function (ev) { if (ev.key === "Enter") { addManual(); } });

  const fetchBtn = overlay.querySelector("#dlg-fetch") as HTMLButtonElement;
  fetchBtn.addEventListener("click", async function () {
    const base = burl.value.trim();
    if (!base) { errLine.textContent = "Enter the Base URL first."; return; }
    errLine.textContent = "";
    fetchBtn.disabled = true;
    try {
      const fetched = await api<{ id: string; name: string; contextLength?: number; inputModalities?: string[]; outputModalities?: string[] }[]>("fetch_provider_models", {
        baseUrl: base,
        apiKey: key.value.trim() || null,
        providerId: isEdit ? pid0 : null,
        apiFormat: fmt.value
      });
      let added = 0;
      for (const f of fetched) {
        const existingModel = models.find(function (m) { return m.id === f.id; });
        if (!existingModel) {
          models.push({ id: f.id, name: f.name || f.id, enabled: true, contextLength: f.contextLength, inputModalities: f.inputModalities, outputModalities: f.outputModalities });
          added += 1;
        } else if (!existingModel.inputModalities && f.inputModalities) {
          existingModel.inputModalities = f.inputModalities;
          existingModel.outputModalities = f.outputModalities;
        }
      }
      renderGrid();
      toast(added + " new model(s), " + fetched.length + " total");
    } catch (e) {
      errLine.textContent = String(e);
    } finally {
      fetchBtn.disabled = false;
    }
  });

  (overlay.querySelector("#dlg-cancel") as HTMLElement).addEventListener("click", function () { closeDialog(); });
  wireOverlayClose(overlay);

  (overlay.querySelector("#dlg-save") as HTMLElement).addEventListener("click", async function () {
    const saveBtn = overlay.querySelector("#dlg-save") as HTMLButtonElement;
    if (saveBtn.disabled) { return; }
    let id = pid0;
    if (!isEdit) {
      const pidEl = overlay.querySelector("#dlg-pid") as HTMLInputElement;
      id = pidEl.value.trim().toLowerCase();
      if (!/^[a-z0-9_-]+$/.test(id)) { errLine.textContent = "Provider ID may only contain a-z, 0-9, dashes and underscores."; return; }
    }
    const base = burl.value.trim();
    if (!/^https?:\/\//.test(base)) { errLine.textContent = "Base URL must start with http:// or https://"; return; }
    const seen = new Set<string>();
    const cleanModels = models.filter(function (m) {
      const mid2 = m.id.trim();
      if (!mid2 || seen.has(mid2)) { return false; }
      seen.add(mid2);
      return true;
    }).map(function (m) {
      return { id: m.id.trim(), name: (m.name.trim() || m.id.trim()), enabled: m.enabled, contextLength: (m.contextLength === undefined ? null : m.contextLength), starred: !!m.starred, inputModalities: (m.inputModalities && m.inputModalities.length ? m.inputModalities : null), outputModalities: (m.outputModalities && m.outputModalities.length ? m.outputModalities : null) };
    });
    saveBtn.disabled = true;
    try {
      const res = await api<SaveResult>("upsert_provider", {
        provider: {
          originalId: isEdit ? pid0 : null,
          id: id,
          baseUrl: base,
          apiKey: key.value.trim() || null,
          apiFormat: fmt.value,
          models: cleanModels,
          logo: iconCtl.value()
        }
      });
      applyResult(res);
      closeDialog();
      toast((isEdit ? "Provider updated" : "Provider added") + " - checking reachability...");
      const now = Date.now();
      if (now - lastCheckProviders > 1500) {
        lastCheckProviders = now;
        invoke<SaveResult>("check_providers", { ids: [id] }).then(function (r2) {
          mergeProviderStatus(r2);
        }).catch(function () { /* ignore */ });
      }
    } catch { /* toast already shown */ } finally {
      saveBtn.disabled = false;
    }
  });

  window.setTimeout(function () { burl.focus(); }, 30);
}

/* ================= Virtual models ================= */

function enabledModelEntries(): { key: string; label: string; providerId: string; baseUrl: string }[] {
  const out: { key: string; label: string; providerId: string; baseUrl: string }[] = [];
  for (const p of cfg.providers) {
    for (const m of p.models) {
      if (!m.enabled) { continue; }
      out.push({ key: p.id + "::" + m.id, label: name_of(m), providerId: p.id, baseUrl: p.baseUrl });
    }
  }
  return out;
}

function renderVirtual(): void {
  const grid = document.getElementById("virtual-grid") as HTMLElement | null;
  const emptyEl = document.getElementById("virtual-empty");
  if (!grid) { return; }
  grid.innerHTML = "";
  const list = cfg.virtualModels || [];
  if (emptyEl) { emptyEl.classList.toggle("hidden", list.length > 0); }
  const enabledKeys = new Set(enabledModelEntries().map(function (e) { return e.key; }));
  for (const v of list) {
    // Warn when bundled members are no longer enabled - the bundle would route nowhere.
    const missing = v.models.filter(function (k) { return !enabledKeys.has(k); }).length;
    const warnBadge = missing > 0
      ? '<span class="badge warn" style="margin-left:6px;" title="' + missing + ' bundled model(s) are not enabled">' + missing + " missing</span>"
      : "";
    const logo = safeLogoSrc(v.logo);
    const card = el(
        '<div class="mcard">' +
        '<div class="m-name name-row">' + (logo ? '<span class="p-logowrap logo-prov"><img src="' + esc(logo) + '" alt="" draggable="false"></span>' : '') + '<span class="name-text">' + esc(v.id) + '</span>' + warnBadge + '</div>' +
        '<div class="m-prov"><span class="pdot"></span><span>' + v.models.length + (v.models.length === 1 ? " model bundled" : " models bundled") + '</span></div>' +
        '<div class="m-foot">' +
          '<span></span>' +
          '<span class="pcard-actions">' +
            '<button class="ibtn act-edit" title="Edit virtual model"><span class="glyph">' + icon("edit") + '</span></button>' +
            '<button class="ibtn danger act-del" title="Delete virtual model (click twice)"><span class="glyph">' + icon("delete") + '</span></button>' +
          '</span>' +
        '</div>' +
      '</div>'
    );
    card.addEventListener("click", function (ev) {
      if ((ev.target as HTMLElement).closest("button")) { return; }
      openVirtualDialog(v);
    });
    (card.querySelector(".act-edit") as HTMLElement).addEventListener("click", function () { openVirtualDialog(v); });
    (card.querySelector(".act-del") as HTMLElement).addEventListener("click", function (ev) {
      const btn = ev.currentTarget as HTMLElement;
      armThen(btn, async function () {
        const res = await api<SaveResult>("delete_virtual_model", { id: v.id });
        applyResult(res);
        toast("Virtual model deleted");
      });
    });
    grid.appendChild(card);
  }
  wireLogoFallbacks(grid);
}

function openVirtualDialog(existing: VirtualEntry | null): void {
  closeDialog();
  const root = $("#dialog-root");
  const isEdit = existing !== null;
  const origId = isEdit && existing ? existing.id : "";
  let selected: string[] = isEdit && existing ? existing.models.slice() : [];

  const overlay = el(
    '<div class="overlay">' +
      '<div class="dialog wide" role="dialog" aria-modal="true">' +
        '<div class="dialog-body">' +
          '<div class="dialog-title" style="display:flex;align-items:center;gap:8px;">' +
            '<span style="flex:1;min-width:0;">' + (isEdit ? "Edit " + esc(origId) : "Create new Virtual Model") + '</span>' +
            '<span id="dlg-icon-prev" style="display:inline-flex;align-items:center;gap:4px;"></span>' +
            '<button class="ibtn" id="dlg-icon" title="Upload custom icon (PNG)"><span class="glyph">' + icon("upload") + '</span></button>' +
          '</div>' +
          '<div class="dlg-panel">' +
            '<div class="field"><label for="vm-id">Model ID (used in API requests)</label>' +
              '<input id="vm-id" type="text" class="mono" spellcheck="false" placeholder="e.g. my-bundle"' + (isEdit ? " value='" + esc(origId) + "'" : "") + ' /></div>' +
          '</div>' +
          '<div class="dlg-sec-title">Bundled models<span class="vm-count" id="vm-count" style="margin-left:6px;">0</span></div>' +
          '<div class="mini-toolbar">' +
            '<input id="vm-search" type="text" spellcheck="false" placeholder="Search models..." />' +
            '<button class="btn sm" id="vm-selall">Select all</button>' +
            '<button class="btn sm" id="vm-clear">Clear</button>' +
          '</div>' +
          '<div class="err-text" id="dlg-err"></div>' +
          '<div class="edit-models-grid" id="vm-grid"></div>' +
          '<div class="dlg-sec-title">Routing</div>' +
          '<div class="dlg-panel">' +
            '<div class="field half"><label for="vm-strategy">Routing strategy (optional override)</label>' +
              '<select id="vm-strategy" class="input">' +
                '<option value="">Global default</option>' +
                '<option value="weighted">Weighted random (starred ×4)</option>' +
                '<option value="round_robin">Round robin</option>' +
                '<option value="priority">Priority (provider order)</option>' +
                '<option value="latency">Lowest average latency</option>' +
                '<option value="fastest">Fastest (latency × health)</option>' +
                '<option value="sticky">Sticky sessions</option>' +
              '</select></div>' +
            '<div class="field"><label for="vm-tiers">Fallback tiers (optional JSON, e.g. [["p::m1"],["p::m2","p::m3"]])</label>' +
              '<textarea id="vm-tiers" class="mono" rows="3" spellcheck="false" placeholder=\'[["provider::model"]]\'></textarea></div>' +
          '</div>' +
        '</div>' +
        '<div class="dialog-footer">' +
          '<button class="btn" id="dlg-cancel">Cancel</button>' +
          '<button class="btn accent" id="dlg-save">Save</button>' +
        '</div>' +
      '</div>' +
    '</div>'
  );
  root.appendChild(overlay);

  const idEl = overlay.querySelector("#vm-id") as HTMLInputElement;
  const searchEl = overlay.querySelector("#vm-search") as HTMLInputElement;
  const grid = overlay.querySelector("#vm-grid") as HTMLElement;
  const errLine = overlay.querySelector("#dlg-err") as HTMLElement;
  const iconCtl = wireIconUpload(overlay, isEdit && existing ? existing.logo : undefined);
  const strategyEl = overlay.querySelector("#vm-strategy") as HTMLSelectElement;
  const tiersEl = overlay.querySelector("#vm-tiers") as HTMLTextAreaElement;
  if (isEdit && existing && existing.policy) {
    if (existing.policy.strategy) { strategyEl.value = existing.policy.strategy; }
    if (existing.policy.tiers && existing.policy.tiers.length) {
      tiersEl.value = JSON.stringify(existing.policy.tiers);
    }
  }

  function renderGrid(): void {
    const q = searchEl.value.trim().toLowerCase();
    grid.innerHTML = "";
    const countEl = overlay.querySelector("#vm-count") as HTMLElement | null;
    if (countEl) { countEl.textContent = String(selected.length); }
    const all = enabledModelEntries();
    if (!all.length) {
      grid.appendChild(el('<div class="chart-empty" style="padding:8px;">No enabled models - enable models on a provider first.</div>'));
      return;
    }
    let shown = 0;
    for (const e of all) {
      if (q && (e.label + " " + e.key).toLowerCase().indexOf(q) < 0) { continue; }
      shown += 1;
      const logo = logoImgHtml(logoForProvider(e.baseUrl), "model");
      const row = el(
        '<div class="mrow">' +
          '<input type="checkbox" class="switch" title="Included in bundle" />' +
          '<span class="mrow-name">' + esc(e.label) + '</span>' +
          '<span class="model-id vm-prov">' + logo + esc(e.providerId) + '</span>' +
        '</div>'
      );
      const sw = row.querySelector("input.switch") as HTMLInputElement;
      sw.checked = selected.indexOf(e.key) >= 0;
      sw.addEventListener("change", function () {
        if (sw.checked) {
          if (selected.indexOf(e.key) < 0) { selected.push(e.key); }
        } else {
          selected = selected.filter(function (k) { return k !== e.key; });
        }
        if (countEl) { countEl.textContent = String(selected.length); }
      });
      grid.appendChild(row);
    }
    if (!shown) {
      grid.appendChild(el('<div class="chart-empty" style="padding:8px;">No models match.</div>'));
    }
  }
  searchEl.addEventListener("input", renderGrid);
  (overlay.querySelector("#vm-selall") as HTMLElement).addEventListener("click", function () {
    const all = enabledModelEntries();
    for (const e of all) { if (selected.indexOf(e.key) < 0) { selected.push(e.key); } }
    renderGrid();
  });
  (overlay.querySelector("#vm-clear") as HTMLElement).addEventListener("click", function () {
    selected = [];
    renderGrid();
  });
  renderGrid();
  wireOverlayClose(overlay);

  (overlay.querySelector("#dlg-cancel") as HTMLElement).addEventListener("click", function () { closeDialog(); });
  (overlay.querySelector("#dlg-save") as HTMLElement).addEventListener("click", async function () {
    const saveBtn = overlay.querySelector("#dlg-save") as HTMLButtonElement;
    if (saveBtn.disabled) { return; }
    const id = idEl.value.trim().toLowerCase();
    if (!/^[a-z0-9_-]+$/.test(id)) { errLine.textContent = "ID may only contain a-z, 0-9, dashes and underscores."; return; }
    let tiers: string[][] | undefined;
    const tiersRaw = tiersEl.value.trim();
    if (tiersRaw) {
      let parsed: unknown;
      try {
        parsed = JSON.parse(tiersRaw);
      } catch {
        errLine.textContent = "Fallback tiers must be valid JSON, e.g. [[\"provider::model\"]]."; return;
      }
      const isMatrix = Array.isArray(parsed) && (parsed as unknown[]).every(function (t) {
        return Array.isArray(t) && (t as unknown[]).every(function (k) { return typeof k === "string" && k.trim().length > 0; });
      });
      if (!isMatrix || !(parsed as unknown[]).length) {
        errLine.textContent = "Fallback tiers must be a non-empty array of arrays of \"provider::model\" strings."; return;
      }
      tiers = parsed as string[][];
    }
    saveBtn.disabled = true;
    try {
      const res = await api<SaveResult>("upsert_virtual_model", {
        originalId: isEdit ? origId : null,
        id: id,
        models: selected,
        logo: iconCtl.value(),
        strategyOverride: strategyEl.value || null,
        tiers: tiers && tiers.length ? tiers : null,
      });
      applyResult(res);
      closeDialog();
      toast(isEdit ? "Virtual model updated" : "Virtual model created");
    } catch { /* toast already shown */ } finally {
      saveBtn.disabled = false;
    }
  });

  window.setTimeout(function () { (isEdit ? searchEl : idEl).focus(); }, 30);
}

/* ================= Settings ================= */

/* One-sentence explanations shown under the routing strategy select. */
const STRATEGY_NOTES: Record<string, string> = {
  weighted: "Random pick, but starred models and healthy providers get picked more often.",
  round_robin: "Cycles through the available models in order, one request at a time.",
  priority: "Always uses the first available provider; later ones only act as fallback.",
  latency: "Picks the model with the lowest average response time from recent history.",
  fastest: "Combines latency and health to pick the model most likely to answer quickly.",
  sticky: "Keeps sending the same conversation to the same model until it fails."
};

function updateStrategyNote(): void {
  const note = document.getElementById("strategy-note");
  const sel = document.getElementById("setting-strategy") as HTMLSelectElement | null;
  if (note && sel) { note.textContent = STRATEGY_NOTES[sel.value] || ""; }
}

function updateCompressDimming(): void {
  const sw = document.getElementById("setting-compress") as HTMLInputElement | null;
  const row = document.getElementById("row-compress-strength");
  if (sw && row) { row.classList.toggle("dimmed", !sw.checked); }
}

/* Brief inline "Saved" note on the row that triggered the save. */
function showSavedNote(row: HTMLElement | null): void {
  if (!row) { return; }
  let note = row.querySelector(".saved-note") as HTMLElement | null;
  if (!note) {
    note = el('<span class="saved-note">Saved</span>');
    row.appendChild(note);
  }
  // Force a reflow so repeated saves re-trigger the fade-in.
  note.classList.remove("show");
  void note.offsetWidth;
  note.classList.add("show");
  window.setTimeout(function () { note!.classList.remove("show"); }, SAVED_NOTE_MS);
}

function renderSettingsForm(): void {
  const c = cfg.compress || { enabled: false, strength: 3 };
  const themeSw = document.getElementById("setting-theme") as HTMLInputElement | null;
  if (themeSw) { themeSw.checked = currentTheme() === "light"; }
  const compressSw = document.getElementById("setting-compress") as HTMLInputElement | null;
  if (compressSw) { compressSw.checked = c.enabled; }
  const strengthEl = document.getElementById("setting-compress-strength") as HTMLInputElement | null;
  if (strengthEl) {
    const s = Math.min(10, Math.max(1, Math.round(c.strength)));
    strengthEl.value = String(s);
    const label = document.getElementById("setting-compress-strength-value");
    if (label) { label.textContent = String(s); }
  }
  const strategySel = document.getElementById("setting-strategy") as HTMLSelectElement | null;
  if (strategySel) {
    const s = cfg.routing ? cfg.routing.strategy : "weighted";
    strategySel.value = ["weighted", "round_robin", "priority", "latency", "fastest", "sticky"].indexOf(s) >= 0 ? s : "weighted";
  }
  updateStrategyNote();
  updateCompressDimming();
  refreshAutostartToggle();
  const autoUpdateSw = document.getElementById("setting-auto-update") as HTMLInputElement | null;
  if (autoUpdateSw) { autoUpdateSw.checked = safeLocalStorageGet("auto-update") !== "0"; }
  renderTabOrderList();
}

/* Autostart command may not exist in older backends - degrade gracefully. */
async function refreshAutostartToggle(): Promise<void> {
  const sw = document.getElementById("setting-autostart") as HTMLInputElement | null;
  if (!sw) { return; }
  try {
    const on = await invoke<boolean>("get_autostart");
    sw.disabled = false;
    sw.title = "";
    sw.checked = !!on;
  } catch {
    sw.checked = false;
    sw.disabled = true;
    sw.title = "Not supported by this build";
  }
}

async function checkForUpdates(silent?: boolean): Promise<void> {
  const out = document.getElementById("update-status");
  const btn = document.getElementById("setting-check-update") as HTMLButtonElement | null;
  if (!out) { return; }
  if (!silent) {
    if (btn) { btn.disabled = true; }
    out.className = "update-status";
    out.textContent = "Checking\u2026";
  }
  try {
    const msg = await invoke<string>("check_for_updates");
    const text = String(msg || "").trim();
    if (silent) {
      // Silent start-up check: only surface a genuinely available update.
      // "No update server configured." and up-to-date results stay quiet.
      if (/update available/i.test(text)) {
        const headline = text.split("\n")[0];
        out.className = "update-status available";
        out.textContent = headline;
        toast(headline);
      }
      return;
    }
    // Heuristic classification so the status line gets a meaningful color.
    if (/up to date|latest|no update|current/i.test(text)) {
      out.className = "update-status ok";
    } else if (/error|failed|could not|unable/i.test(text)) {
      out.className = "update-status err";
    } else {
      out.className = "update-status available";
    }
    out.textContent = text || "No details returned";
  } catch (e) {
    if (!silent) {
      out.className = "update-status err";
      out.textContent = "Update check failed: " + errText(e);
    }
  } finally {
    if (!silent && btn) { btn.disabled = false; }
  }
}

/* Filter settings rows; sections with no visible rows are hidden too. */
function applySettingsFilter(query: string): void {
  const q = query.trim().toLowerCase();
  document.querySelectorAll<HTMLElement>("#panel-settings .set-sec").forEach(function (sec) {
    const title = (sec.querySelector(".set-sec-title")?.textContent || "").toLowerCase();
    const titleHit = !!q && title.indexOf(q) >= 0;
    let anyVisible = false;
    sec.querySelectorAll<HTMLElement>(".set-row").forEach(function (row) {
      const text = (row.textContent || "").toLowerCase();
      const hit = !q || titleHit || text.indexOf(q) >= 0;
      row.style.display = hit ? "" : "none";
      if (hit) { anyVisible = true; }
    });
    // The tab-order list follows its own set-row; hide it together with that row.
    sec.querySelectorAll<HTMLElement>(".tab-order-list").forEach(function (tl) {
      const row = tl.previousElementSibling as HTMLElement | null;
      tl.style.display = row && row.style.display === "none" ? "none" : "";
    });
    sec.style.display = anyVisible ? "" : "none";
  });
}

async function saveSettings(triggerRow?: HTMLElement | null): Promise<void> {
  const compressEnabled = (document.getElementById("setting-compress") as HTMLInputElement | null)?.checked ?? false;
  const strength = parseInt((document.getElementById("setting-compress-strength") as HTMLInputElement | null)?.value || "3", 10);
  const prevRouting = cfg.routing || { strategy: "weighted", latencyWindow: 20 };
  const strategyVal = (document.getElementById("setting-strategy") as HTMLSelectElement | null)?.value;
  const routing = { strategy: strategyVal || prevRouting.strategy, latencyWindow: prevRouting.latencyWindow };
  try {
    const res = await api<SaveResult>("save_settings", {
      circuitBreaker: cfg.circuitBreaker,
      routing: routing,
      health: cfg.health,
      compress: { enabled: compressEnabled, strength },
      responseCache: cfg.responseCache,
      failoverBudget: cfg.failoverBudget,
    });
    cfg = res.config;
    showSavedNote(triggerRow ?? null);
  } catch { /* shown */ }
}

/* ================= Import / Export ================= */

function downloadJson(text: string, filename: string): void {
  const blob = new Blob([text], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}

async function exportUsage(): Promise<void> {
  try {
    const json = await api<string>("export_usage");
    downloadJson(json, "multillm-usage.json");
    toast("Usage exported to Downloads");
  } catch { /* shown */ }
}

async function exportConfig(): Promise<void> {
  let json: string;
  try {
    json = await api<string>("export_config");
  } catch { return; /* shown */ }
  closeDialog();
  const root = $("#dialog-root");
  const overlay = el(
    '<div class="overlay">' +
      '<div class="dialog" role="dialog" aria-modal="true" style="width:440px;">' +
        '<div class="dialog-body">' +
          '<div class="dialog-title">Export config</div>' +
          '<label class="export-opt"><input type="checkbox" id="export-keys" checked /> Include API keys</label>' +
          '<p class="hint-sm" style="margin:0;">Without keys the file is safe to share or commit.</p>' +
        '</div>' +
        '<div class="dialog-footer">' +
          '<button class="btn dlg-cancel">Cancel</button>' +
          '<button class="btn accent" id="export-go">Export</button>' +
        '</div>' +
      '</div>' +
    '</div>'
  );
  root.appendChild(overlay);
  (overlay.querySelector(".dlg-cancel") as HTMLElement).addEventListener("click", closeDialog);
  (overlay.querySelector("#export-go") as HTMLElement).addEventListener("click", function () {
    const withKeys = (overlay.querySelector("#export-keys") as HTMLInputElement).checked;
    let out = json;
    if (!withKeys) {
      // Strip secrets client-side so they never touch the downloaded file.
      try {
        const parsed = JSON.parse(json) as Record<string, unknown>;
        parsed.localKey = null;
        if (Array.isArray(parsed.providers)) {
          for (const p of parsed.providers as Array<Record<string, unknown>>) {
            if (p && typeof p === "object") { delete p.apiKey; }
          }
        }
        out = JSON.stringify(parsed, null, 2);
      } catch { /* keep the original payload */ }
    }
    downloadJson(out, "multillm-config.json");
    closeDialog();
    toast("Config exported to Downloads");
  });
  wireOverlayClose(overlay);
}

async function importConfig(): Promise<void> {
  const input = document.createElement("input");
  input.type = "file";
  input.accept = "application/json";
  input.onchange = async () => {
    const file = input.files?.[0];
    if (!file) { return; }
    const text = await file.text();
    // Parse locally first so we can show a summary before touching the backend.
    let obj: Record<string, unknown>;
    try {
      const parsed = JSON.parse(text);
      if (!parsed || typeof parsed !== "object") { throw new Error("not an object"); }
      obj = parsed as Record<string, unknown>;
    } catch {
      toast("Not a valid JSON config file", "err");
      return;
    }
    const provs = Array.isArray(obj.providers) ? (obj.providers as Array<Record<string, unknown>>) : [];
    const virt = Array.isArray(obj.virtualModels) ? (obj.virtualModels as unknown[]) : [];
    const hasKeys = !!obj.localKey || provs.some(function (p) { return !!(p && typeof p === "object" && p.apiKey); });
    closeDialog();
    const root = $("#dialog-root");
    const overlay = el(
      '<div class="overlay">' +
        '<div class="dialog" role="dialog" aria-modal="true" style="width:440px;">' +
          '<div class="dialog-body">' +
            '<div class="dialog-title">Import config</div>' +
            '<div class="import-summary">' +
              '<span><b>' + esc(String(provs.length)) + '</b> provider(s)</span>' +
              '<span><b>' + esc(String(virt.length)) + '</b> virtual model(s)</span>' +
              '<span>' + (hasKeys ? "Contains API keys - they will be imported." : "No API keys in this file.") + '</span>' +
            '</div>' +
            '<p class="hint-sm" style="margin:8px 0 0;">Existing providers with the same ID will be overwritten.</p>' +
          '</div>' +
          '<div class="dialog-footer">' +
            '<button class="btn dlg-cancel">Cancel</button>' +
            '<button class="btn accent" id="import-go">Import</button>' +
          '</div>' +
        '</div>' +
      '</div>'
    );
    root.appendChild(overlay);
    (overlay.querySelector(".dlg-cancel") as HTMLElement).addEventListener("click", closeDialog);
    (overlay.querySelector("#import-go") as HTMLElement).addEventListener("click", async function () {
      try {
        const res = await api<SaveResult>("import_config", { json: text });
        applyResult(res);
        closeDialog();
        toast("Config imported");
      } catch { /* shown */ }
    });
    wireOverlayClose(overlay);
  };
  input.click();
}

/* ================= Wiring ================= */

function wireTitlebar(): void {
  const win = getCurrentWindow();
  ($("#win-min") as HTMLElement).addEventListener("click", function () { win.minimize(); });
  ($("#win-max") as HTMLElement).addEventListener("click", async function () { await win.toggleMaximize(); });
  ($("#win-close") as HTMLElement).addEventListener("click", function () {
    // Schließen meldet immer ab (Backend tut das beim Close-Event ohnehin
    // auch) und schließt das Fenster danach.
    void (async function () {
      try { await invoke("logout_user"); } catch { /* ignore - backend handles it */ }
      currentUser = null;
      win.close();
    })();
  });
}

function showPanel(which: string): void {
  const items = Array.from(document.querySelectorAll<HTMLButtonElement>(".nav-item"));
  items.forEach(function (b) {
    const active = b.dataset.panel === which;
    b.classList.toggle("active", active);
    b.setAttribute("aria-selected", active ? "true" : "false");
  });
  ["models", "providers", "virtual", "api", "usage", "settings"].forEach(function (p) {
    const elx = document.getElementById("panel-" + p);
    if (elx) { elx.classList.toggle("hidden", p !== which); }
  });
  if (which === "usage") { loadUsage(); }
  if (which === "settings") { renderSettingsForm(); }
}

function wireTabs(): void {
  const items = Array.from(document.querySelectorAll<HTMLButtonElement>(".nav-item"));
  items.forEach(function (btn) {
    btn.addEventListener("click", function () {
      showPanel(btn.dataset.panel || "models");
    });
  });
}

/* ---- Custom sidebar tab order (persisted in localStorage) ---- */
const TAB_ORDER_KEY = "tab-order";

function readTabOrder(): string[] {
  const raw = safeLocalStorageGet(TAB_ORDER_KEY);
  if (!raw) { return []; }
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) { return []; }
    return parsed.filter(function (x): x is string { return typeof x === "string"; });
  } catch { return []; }
}

/* Re-append the .nav-item buttons in the saved order. Unknown or missing
   panel IDs are tolerated - leftover tabs keep their default order at the end. */
function applyTabOrder(): void {
  const nav = document.querySelector<HTMLElement>(".nav");
  if (!nav) { return; }
  const order = readTabOrder();
  if (!order.length) { return; }
  const items = Array.from(nav.querySelectorAll<HTMLButtonElement>(".nav-item"));
  const pending = new Map<string, HTMLButtonElement>();
  items.forEach(function (b) { pending.set(b.dataset.panel || "", b); });
  order.forEach(function (panel) {
    const b = pending.get(panel);
    if (b) { nav.appendChild(b); pending.delete(panel); }
  });
  items.forEach(function (b) {
    if (pending.has(b.dataset.panel || "")) { nav.appendChild(b); }
  });
}

/* Settings UI: one row per tab with move up/down buttons. */
function renderTabOrderList(): void {
  const box = document.getElementById("tab-order-list");
  if (!box) { return; }
  box.innerHTML = "";
  const items = Array.from(document.querySelectorAll<HTMLButtonElement>(".nav-item"));
  items.forEach(function (btn, idx) {
    const label = btn.querySelector("span")?.textContent || btn.dataset.panel || "";
    const row = el(
      '<div class="tab-order-row">' +
        '<span class="to-name">' + esc(label) + '</span>' +
        '<button class="ibtn to-up" title="Move up"><span class="glyph">' + icon("up") + '</span></button>' +
        '<button class="ibtn to-down" title="Move down"><span class="glyph">' + icon("down") + '</span></button>' +
      '</div>'
    );
    const up = row.querySelector(".to-up") as HTMLButtonElement;
    const down = row.querySelector(".to-down") as HTMLButtonElement;
    up.disabled = idx === 0;
    down.disabled = idx === items.length - 1;
    up.addEventListener("click", function () { moveTab(idx, idx - 1); });
    down.addEventListener("click", function () { moveTab(idx, idx + 1); });
    box.appendChild(row);
  });
}

function moveTab(from: number, to: number): void {
  const panels = Array.from(document.querySelectorAll<HTMLButtonElement>(".nav-item")).map(function (b) { return b.dataset.panel || ""; });
  if (to < 0 || to >= panels.length) { return; }
  const moved = panels.splice(from, 1)[0];
  panels.splice(to, 0, moved);
  safeLocalStorageSet(TAB_ORDER_KEY, JSON.stringify(panels));
  applyTabOrder();
  renderTabOrderList();
}

function apiPortsError(): string | null {
  const t = ($("#api-port") as HTMLInputElement).value.trim();
  if (!/^\d+$/.test(t) || !(Number(t) >= 1 && Number(t) <= 65535)) {
    return "Port must be a number between 1 and 65535";
  }
  return null;
}

function showApiError(msg: string | null): void {
  const el = document.getElementById("api-error");
  if (!el) { return; }
  if (msg) {
    el.textContent = msg;
    (el as HTMLElement).style.display = "block";
  } else {
    (el as HTMLElement).style.display = "none";
  }
}

function scheduleApiSave(): void {
  window.clearTimeout(apiSaveTimer);
  apiSaveTimer = window.setTimeout(async function () {
    const err = apiPortsError();
    if (err) { showApiError(err); return; }
    showApiError(null);
    try {
      const res = await api<SaveResult>("save_api_settings", {
        port: Number((($("#api-port") as HTMLInputElement).value.trim())),
        enabled: ($("#api-enabled") as HTMLInputElement).checked,
        exposeLan: (document.getElementById("api-expose") as HTMLInputElement).checked,
        exposeAllModels: (document.getElementById("api-expose-all") as HTMLInputElement).checked
      });
      apiToggleDirty.clear(); // stored values now match the toggles
      applyResult(res);
      const note = $("#api-saved");
      note.classList.add("show");
      window.setTimeout(function () { note.classList.remove("show"); }, SAVED_NOTE_MS);
    } catch { /* shown */ }
  }, API_SAVE_MS);
}

function wireApiPanel(): void {
  const portEl = $("#api-port") as HTMLInputElement;
  portEl.addEventListener("input", function () {
    updateUrlPreview();
    scheduleApiSave();
  });
  for (const tid of ["api-enabled", "api-expose", "api-expose-all"]) {
    const box = document.getElementById(tid) as HTMLInputElement | null;
    if (!box) { continue; }
    box.addEventListener("change", function () {
      apiToggleDirty.add(tid);
      updateExposeHint();
      scheduleApiSave();
    });
  }
  document.querySelectorAll<HTMLButtonElement>("#panel-api .url-copy").forEach(function (btn) {
    btn.addEventListener("click", function () {
      const src = document.getElementById(btn.dataset.src || "");
      if (src) { copyText((src as HTMLElement).textContent || ""); }
    });
  });
  const copyBtn = document.getElementById("key-copy") as HTMLElement | null;
  if (copyBtn) {
    copyBtn.addEventListener("click", function () {
      const inp = document.getElementById("api-key-input") as HTMLInputElement;
      copyText(inp.value);
    });
  }
  const showBtn = document.getElementById("key-show") as HTMLElement | null;
  if (showBtn) {
    showBtn.addEventListener("click", function () {
      const inp = document.getElementById("api-key-input") as HTMLInputElement;
      inp.type = inp.type === "password" ? "text" : "password";
    });
  }
  const rerollBtn = document.getElementById("key-reroll") as HTMLElement | null;
  if (rerollBtn) {
    rerollBtn.addEventListener("click", function (ev) {
      const btn = ev.currentTarget as HTMLElement;
      armThen(btn, async function () {
        try {
          cfg.localKey = await api<string>("regenerate_local_key");
          renderApiKeyField();
          toast("New API key generated - update your clients");
        } catch { /* shown */ }
      });
    });
  }
}

async function pollStatus(): Promise<void> {
  try {
    const s = await invoke<ProxyStatus>("proxy_status");
    status = s;
    // Surface backend-reported proxy errors inline. Never clear here, so a
    // local validation message (e.g. bad port) can't be wiped by the poll.
    if (s.error) { showApiError(String(s.error)); }
  } catch { /* backend unreachable - panels keep last state */ }
}

/* ================= Usage tab ================= */

function fmtInt(n: number): string {
  return n.toLocaleString("en-US");
}

/* Compact token format: 1640000000 -> "1.64B", 882200000 -> "882.2M", 12400 -> "12.4K". */
function fmtCompact(n: number): string {
  const abs = Math.abs(n);
  if (abs >= 1e9) { return fmtCompactFrac(n / 1e9) + "B"; }
  if (abs >= 1e6) { return fmtCompactFrac(n / 1e6) + "M"; }
  if (abs >= 1e3) { return fmtCompactFrac(n / 1e3) + "K"; }
  return fmtInt(n);
}

function fmtCompactFrac(v: number): string {
  return v.toFixed(2).replace(/\.?0+$/, "");
}

function timeAgo(ms: number): string {
  if (!ms) { return "\u2014"; }
  const diff = Math.max(0, Date.now() - ms);
  const s = Math.floor(diff / 1000);
  if (s < 45) { return "just now"; }
  const m = Math.floor(s / 60);
  if (m < 60) { return m + " min ago"; }
  const h = Math.floor(m / 60);
  if (h < 24) { return h + " h ago"; }
  const d = Math.floor(h / 24);
  return d + " d ago";
}

/* Usage view state: never persisted, defaults on every app start. */
let usageRange = "all";
let usageSort = "most";

const RANGE_LABELS: Record<string, string> = { today: "Today", month: "Month", all: "All Time" };
const SORT_LABELS: Record<string, string> = { most: "Most Usage", last: "Last Used", oldest: "Oldest Used", requests: "Most Requests", failed: "Most Failed" };

function updateSortButtonLabel(): void {
  const btn = $("#usage-sort");
  if (btn) { btn.textContent = "Sort: " + RANGE_LABELS[usageRange] + " \u00b7 " + SORT_LABELS[usageSort]; }
}

function refreshSortMenuChecks(): void {
  const menu = document.getElementById("usage-sort-menu");
  if (!menu) { return; }
  menu.querySelectorAll<HTMLButtonElement>(".sort-item[data-range]").forEach(function (b) {
    b.classList.toggle("active", (b.dataset.range || "") === usageRange);
  });
  menu.querySelectorAll<HTMLButtonElement>(".sort-item[data-sort]").forEach(function (b) {
    b.classList.toggle("active", (b.dataset.sort || "") === usageSort);
  });
}

async function loadUsage(): Promise<void> {
  const requested = usageRange;
  let entries: UsageEntry[] = [];
  try {
    entries = await invoke<UsageEntry[]>("get_usage", { range: requested });
  } catch { return; /* app closing */ }
  if (usageRange !== requested) { return; /* user switched ranges meanwhile */ }
  lastUsageEntries = entries;
  scheduleRenderUsage();
}

/* Accent colors cycled through the ranking bars. */
const RANK_COLORS = ["#ff5c7a", "#4a89f5", "#34c77b", "#9b59f5", "#45c4e8", "#f5923e", "#ffd23f", "#22d3a7"];

function renderUsage(entries: UsageEntry[]): void {
  // Optional toolbar filter narrows rows and the summary totals together.
  const fEl = document.getElementById("usage-search") as HTMLInputElement | null;
  const fq = fEl ? fEl.value.trim().toLowerCase() : "";
  if (fq) {
    entries = entries.filter(function (e) {
      return e.modelId.toLowerCase().indexOf(fq) >= 0 || e.providerId.toLowerCase().indexOf(fq) >= 0;
    });
  }
  let okTotal = 0;
  let failTotal = 0;
  let inTotal = 0;
  let outTotal = 0;
  for (const e of entries) {
    okTotal += e.requestsOk;
    failTotal += e.requestsFailed;
    inTotal += e.inputTokens;
    outTotal += e.outputTokens;
  }
  $("#stat-requests").textContent = fmtInt(okTotal);
  $("#stat-failed").textContent = fmtInt(failTotal);
  $("#stat-input").textContent = fmtInt(inTotal);
  $("#stat-output").textContent = fmtInt(outTotal);

  const list = document.getElementById("usage-body") as HTMLElement;
  const emptyEl = $("#usage-empty");
  list.querySelectorAll("img").forEach(function (img) { img.src = ""; });
  list.innerHTML = "";
  if (!entries.length) {
    list.classList.add("hidden");
    emptyEl.classList.remove("hidden");
    return;
  }
  list.classList.remove("hidden");
  emptyEl.classList.add("hidden");

  // Order rows by the current sort mode; tie-break by model name.
  const ranked = entries.map(function (e) {
    return { e: e, total: e.inputTokens + e.outputTokens };
  });
  const byName = function (a: { e: UsageEntry }, b: { e: UsageEntry }) {
    return (a.e.modelId || "").localeCompare(b.e.modelId || "");
  };
  if (usageSort === "last") {
    ranked.sort(function (a, b) { return (b.e.lastUsedMs - a.e.lastUsedMs) || byName(a, b); });
  } else if (usageSort === "oldest") {
    ranked.sort(function (a, b) { return (a.e.lastUsedMs - b.e.lastUsedMs) || byName(a, b); });
  } else if (usageSort === "requests") {
    ranked.sort(function (a, b) { return (b.e.requestsOk - a.e.requestsOk) || byName(a, b); });
  } else if (usageSort === "failed") {
    ranked.sort(function (a, b) { return (b.e.requestsFailed - a.e.requestsFailed) || byName(a, b); });
  } else {
    ranked.sort(function (a, b) { return (b.total - a.total) || byName(a, b); });
  }
  const maxTotal = ranked.length ? Math.max(ranked[0].total, 1) : 1;
  const allTotal = Math.max(inTotal + outTotal, 1);

  for (let i = 0; i < ranked.length; i++) {
    const r = ranked[i];
    const e = r.e;
    const estMark = e.estimated
      ? ' <span class="um-est" title="Some token counts were estimated from response size because the provider did not report usage.">*</span>'
      : "";
    const prov = cfg.providers.find(function (p) { return p.id === e.providerId; });
    const logo = prov ? logoImgHtml(logoForProvider(prov.baseUrl), "model") : "";
    const barPct = Math.max(0.75, (r.total / maxTotal) * 100);
    const sharePct = ((r.total / allTotal) * 100).toFixed(2);
    const color = RANK_COLORS[i % RANK_COLORS.length];
    const reqs = fmtInt(e.requestsOk) + " req" + (e.requestsFailed ? " \u00b7 " + fmtInt(e.requestsFailed) + " failed" : "");
    const row = el(
      '<div class="usage-row" data-provider="' + esc(e.providerId) + '" data-model="' + esc(e.modelId) + '">' +
        '<span class="ur-rank">' + (i + 1) + '</span>' +
        '<span class="ur-model">' + logo +
          '<span class="ur-namewrap">' +
            '<span class="ur-name">' + esc(e.modelId) + estMark + '</span>' +
            '<span class="ur-meta">' + esc(e.providerId) + ' &middot; ' + reqs + ' &middot; ' + timeAgo(e.lastUsedMs) + '</span>' +
          '</span>' +
        '</span>' +
        '<span class="ur-track"><span class="ur-fill" style="width:' + barPct.toFixed(2) + '%;background:' + color + ';"></span></span>' +
        '<span class="ur-vals"><b>' + fmtCompact(r.total) + '</b><i>' + sharePct + '%</i></span>' +
      '</div>'
    );
    list.appendChild(row);
  }
}

function hideSortMenu(): void {
  const menu = document.getElementById("usage-sort-menu");
  if (menu) { menu.classList.add("hidden"); }
}

function wireUsagePanel(): void {
  const wrap = document.getElementById("usage-sort-wrap");
  const btn = $("#usage-sort");
  const menu = document.getElementById("usage-sort-menu");
  if (!wrap || !btn || !menu) { return; }

  btn.addEventListener("click", function (e) {
    e.stopPropagation();
    const opening = menu.classList.contains("hidden");
    if (opening) { refreshSortMenuChecks(); }
    menu.classList.toggle("hidden");
  });

  // Close on outside click or Escape.
  window.addEventListener("click", function (e) {
    if (!(e.target as HTMLElement).closest("#usage-sort-wrap")) { hideSortMenu(); }
  });
  window.addEventListener("keydown", function (e) {
    if (e.key === "Escape") { hideSortMenu(); }
  });
  window.addEventListener("blur", hideSortMenu);

  menu.querySelectorAll<HTMLButtonElement>(".sort-item").forEach(function (b) {
    b.addEventListener("click", function () {
      const range = b.dataset.range;
      if (range && range !== usageRange) {
        usageRange = range;
        updateSortButtonLabel();
        refreshSortMenuChecks();
        hideSortMenu();
        loadUsage();
        return;
      }
      const sort = b.dataset.sort;
      if (sort && sort !== usageSort) {
        usageSort = sort;
        updateSortButtonLabel();
        refreshSortMenuChecks();
        hideSortMenu();
        renderUsage(lastUsageEntries); /* re-sort locally, no refetch */
      }
    });
  });
}

/* ================= Context menu ================= */

function hideContextMenu(): void {
  const m = document.querySelector(".ctx-menu");
  if (m) { m.remove(); }
}

interface CtxItem { label: string; glyph?: string; danger?: boolean; action: () => void; }

function showContextMenu(x: number, y: number, items: CtxItem[]): void {
  hideContextMenu();
  const menu = el('<div class="ctx-menu"></div>');
  for (const it of items) {
    const b = el(
      '<button class="ctx-item' + (it.danger ? " danger" : "") + '">' +
        (it.glyph ? '<span class="glyph">' + it.glyph + '</span>' : "") +
        '<span>' + esc(it.label) + '</span>' +
      '</button>'
    );
    b.addEventListener("click", function () { hideContextMenu(); it.action(); });
    menu.appendChild(b);
  }
  document.body.appendChild(menu);
  const rect = menu.getBoundingClientRect();
  const px = Math.min(x, window.innerWidth - rect.width - 8);
  const py = Math.min(y, window.innerHeight - rect.height - 8);
  menu.style.left = px + "px";
  menu.style.top = py + "px";
}

/* ================= Theme (light / dark) ================= */

type ThemePref = "light" | "dark";

function currentTheme(): ThemePref {
  return document.documentElement.getAttribute("data-theme") === "light" ? "light" : "dark";
}

function applyTheme(theme: ThemePref): void {
  if (theme === "light") {
    document.documentElement.setAttribute("data-theme", "light");
  } else {
    document.documentElement.removeAttribute("data-theme");
  }
  try {
    window.localStorage.setItem("theme", theme);
  } catch { /* storage unavailable - theme still applies for this session */ }
}

/* ================= Background polling =================
 * One shared tick for proxy status, config drift and health probes. The
 * timer is paused while the window is hidden and guarded against overlap. */

let pollTimer = 0;
let pollBusy = false;

async function pollTick(): Promise<void> {
  if (pollBusy) { return; } // previous tick still running - skip
  pollBusy = true;
  try {
    void pollStatus();
    refreshUsageAll();
    // Surface background connectivity checks in the provider list.
    try {
      const fresh = await invoke<PublicConfig>("load_settings");
      if (JSON.stringify(fresh.providers) !== JSON.stringify(cfg.providers) || JSON.stringify(fresh.virtualModels || []) !== JSON.stringify(cfg.virtualModels || [])) {
        cfg = fresh;
        await renderProviders();
        scheduleRenderModelCards();
        renderVirtual();
      }
    } catch { /* app closing */ }
    // Health dots: refresh the probe/breaker snapshot (TTL-gated) and
    // re-render only when something actually changed.
    try {
      await refreshHealthData();
      const sig = JSON.stringify(healthData);
      if (sig !== lastHealthSig) {
        lastHealthSig = sig;
        renderProviders();
        scheduleRenderModelCards();
      }
    } catch { /* ignore */ }
    const usagePanel = document.getElementById("panel-usage");
    const usageVisible = !!usagePanel && !usagePanel.classList.contains("hidden");
    if (usageVisible) { loadUsage(); }
    // usage render is debounced inside loadUsage/scheduleRenderUsage
  } finally {
    pollBusy = false;
  }
}

function startPolling(): void {
  if (pollTimer) { return; }
  pollTimer = window.setInterval(function () { void pollTick(); }, POLL_MS);
}

function stopPolling(): void {
  if (pollTimer) { window.clearInterval(pollTimer); pollTimer = 0; }
}

/* ================= Multi-user auth (local accounts) =================
 * - Erster Start (keine Benutzer): Onboarding-Wizard (Benutzername +
 *   Passwort, danach OAuth-Schritt mit "Ohne Anmeldung fortfahren").
 * - Danach: Login-Screen mit Benutzerliste. Die Session lebt nur im
 *   Speicher - App schließen meldet automatisch ab.
 * - Jedes Konto besitzt eigene Provider/Models/API-Keys plus einen eigenen
 *   Port (8123, 8124, ...), damit mehrere Benutzer die App parallel nutzen
 *   können (z. B. auf einem gemeinsamen Server). */

interface UserPublic {
  username: string;
  port: number;
  oauthProvider?: string | null;
  email?: string | null;
  createdAt: string;
}

let currentUser: UserPublic | null = null;
let authResolve: (() => void) | null = null;
let authBusy = false;

function authRootEl(): HTMLElement {
  let r = document.getElementById("auth-root");
  if (!r) {
    r = el('<div id="auth-root"></div>');
    document.body.appendChild(r);
  }
  return r;
}

function hideAuth(): void {
  authRootEl().innerHTML = "";
}

function avatarLetter(name: string): string {
  const t = (name || "?").trim();
  return (t.charAt(0) || "?").toUpperCase();
}

function oauthLabel(p?: string | null): string {
  if (p === "google") { return "Google"; }
  if (p === "microsoft") { return "Microsoft"; }
  if (p === "email") { return "Email"; }
  return "";
}

function renderUserChip(): void {
  const wrap = document.getElementById("tb-user");
  if (!wrap) { return; }
  wrap.classList.toggle("hidden", !currentUser);
  if (currentUser) {
    const av = document.getElementById("tb-avatar");
    const nm = document.getElementById("tb-username");
    const pt = document.getElementById("tb-port");
    if (av) { av.textContent = avatarLetter(currentUser.username); }
    if (nm) { nm.textContent = currentUser.username; }
    if (pt) { pt.textContent = ":" + currentUser.port; }
  }
}

function authShell(title: string, sub: string, stepsHtml: string, bodyHtml: string): HTMLElement {
  const root = authRootEl();
  root.innerHTML = "";
  const ov = el(
    '<div class="auth-overlay">' +
      '<div class="auth-card" role="dialog" aria-modal="true">' +
        '<div class="auth-logo">M</div>' +
        '<h1>' + esc(title) + '</h1>' +
        '<p class="sub">' + esc(sub) + '</p>' +
        stepsHtml +
        '<div class="auth-body"></div>' +
      '</div>' +
    '</div>'
  );
  (ov.querySelector(".auth-body") as HTMLElement).innerHTML = bodyHtml;
  root.appendChild(ov);
  return ov;
}

async function reloadAllData(): Promise<void> {
  try {
    cfg = await invoke<PublicConfig>("load_settings");
  } catch (e) {
    toast("Failed to load settings: " + errText(e), "err");
    return;
  }
  try {
    status = await invoke<ProxyStatus>("proxy_status");
  } catch { /* ignore */ }
  await renderProviders();
  scheduleRenderModelCards();
  renderVirtual();
  renderApiPanel();
  loadUsage();
  refreshUsageAll();
}

async function afterAuth(u: UserPublic, fresh: boolean): Promise<void> {
  currentUser = u;
  hideAuth();
  renderUserChip();
  if (authResolve) {
    // Noch im Start-Gate: init() lädt die Daten direkt danach selbst.
    const r = authResolve;
    authResolve = null;
    r();
  } else {
    await reloadAllData();
  }
  if (fresh) { toast("Logged in as " + u.username); }
}

/* ---- Onboarding wizard ---- */

interface WizardState { username: string; password: string; oauth: string | null; email: string; }

function showWizardStep1(prefill?: WizardState, backToLogin?: boolean): void {
  const w: WizardState = prefill || { username: "", password: "", oauth: null, email: "" };
  const ov = authShell(
    "Welcome to Multi LLM",
    "Create a user. Every user gets their own providers, models and API keys.",
    '<div class="auth-steps"><i class="now"></i><i></i></div>',
    '<div class="field"><label for="w-username">Username</label>' +
      '<input id="w-username" type="text" autocomplete="username" spellcheck="false" placeholder="e.g. anna" /></div>' +
      '<div class="field"><label for="w-password">Password (min. 10 chars)</label>' +
      '<input id="w-password" type="password" autocomplete="new-password" placeholder="Password" /></div>' +
      '<div class="field"><label for="w-password2">Repeat password</label>' +
      '<input id="w-password2" type="password" autocomplete="new-password" placeholder="Repeat password" /></div>' +
      '<div class="err-text" id="w-err"></div>' +
      '<div class="auth-actions">' +
      (backToLogin ? '<button class="btn" id="w-back">Back</button>' : '<span class="flex-spacer"></span>') +
      '<button class="btn accent" id="w-next"><span>Next</span></button></div>'
  );
  const nameEl = ov.querySelector("#w-username") as HTMLInputElement;
  const pwEl = ov.querySelector("#w-password") as HTMLInputElement;
  const pw2El = ov.querySelector("#w-password2") as HTMLInputElement;
  const errEl = ov.querySelector("#w-err") as HTMLElement;
  nameEl.value = w.username;
  if (backToLogin) {
    (ov.querySelector("#w-back") as HTMLElement).addEventListener("click", function () { void showLogin(); });
  }
  function next(): void {
    const name = nameEl.value.trim();
    if (name.length < 2) { errEl.textContent = "Username needs at least 2 characters."; nameEl.focus(); return; }
    if (!/^[A-Za-z0-9_-]+$/.test(name)) { errEl.textContent = "Nur Buchstaben, Ziffern, '_' und '-' sind erlaubt."; nameEl.focus(); return; }
    if (pwEl.value.length < 10) { errEl.textContent = "Password needs at least 10 characters."; pwEl.focus(); return; }
    if (pwEl.value !== pw2El.value) { errEl.textContent = "Passwords do not match."; pw2El.focus(); return; }
    errEl.textContent = "";
    w.username = name;
    w.password = pwEl.value;
    showWizardStep2(w);
  }
  (ov.querySelector("#w-next") as HTMLElement).addEventListener("click", next);
  for (const inp of [nameEl, pwEl, pw2El]) {
    inp.addEventListener("keydown", function (ev) { if (ev.key === "Enter") { next(); } });
  }
  window.setTimeout(function () { nameEl.focus(); }, 40);
}

function showWizardStep2(w: WizardState): void {
  const ov = authShell(
    "Link account",
    "Link \"" + w.username + "\" with Google, Microsoft or email. Verification comes later - for testing you can continue without login.",
    '<div class="auth-steps"><i class="done"></i><i class="now"></i></div>',
    '<div class="oauth-stack">' +
      '<button class="oauth-btn" id="oa-google"><span class="ob-mark g">G</span><span>Log in with Google</span><span class="ob-check hidden">✓</span></button>' +
      '<button class="oauth-btn" id="oa-ms"><span class="ob-mark m"><i></i><i></i><i></i><i></i></span><span>Log in with Microsoft</span><span class="ob-check hidden">✓</span></button>' +
      '<button class="oauth-btn" id="oa-email"><span class="ob-mark e">✉</span><span>Log in with email</span><span class="ob-check hidden">✓</span></button>' +
      '</div>' +
      '<div class="field hidden" id="oa-email-wrap" style="margin-top:10px;"><label for="oa-email-input">Email address</label>' +
      '<input id="oa-email-input" type="email" autocomplete="email" spellcheck="false" placeholder="name@beispiel.de" /></div>' +
      '<div class="auth-port-note"><span>🔑</span><span>Every new user automatically gets their own port, so multiple users can use the app at the same time.</span></div>' +
      '<div class="err-text" id="w-err"></div>' +
      '<div class="auth-actions">' +
      '<button class="btn" id="w-back">Back</button>' +
      '<span class="flex-spacer"></span>' +
      '<button class="btn accent" id="w-finish"><span>Create account</span></button></div>' +
      '<button class="auth-skip" id="w-skip">Continue without login</button>'
  );
  const errEl = ov.querySelector("#w-err") as HTMLElement;
  const emailWrap = ov.querySelector("#oa-email-wrap") as HTMLElement;
  const emailEl = ov.querySelector("#oa-email-input") as HTMLInputElement;
  emailEl.value = w.email;

  function markLinked(which: string | null): void {
    const map: Record<string, string> = { google: "#oa-google", microsoft: "#oa-ms", email: "#oa-email" };
    for (const key of Object.keys(map)) {
      const btn = ov.querySelector(map[key]) as HTMLElement;
      const check = btn.querySelector(".ob-check") as HTMLElement;
      const on = which === key;
      btn.classList.toggle("linked", on);
      check.classList.toggle("hidden", !on);
    }
  }
  (ov.querySelector("#oa-google") as HTMLElement).addEventListener("click", function () {
    w.oauth = "google";
    emailWrap.classList.add("hidden");
    markLinked("google");
    toast("Google link saved - verification comes later (test mode)");
  });
  (ov.querySelector("#oa-ms") as HTMLElement).addEventListener("click", function () {
    w.oauth = "microsoft";
    emailWrap.classList.add("hidden");
    markLinked("microsoft");
    toast("Microsoft link saved - verification comes later (test mode)");
  });
  (ov.querySelector("#oa-email") as HTMLElement).addEventListener("click", function () {
    w.oauth = "email";
    emailWrap.classList.remove("hidden");
    markLinked("email");
    window.setTimeout(function () { emailEl.focus(); }, 30);
  });
  (ov.querySelector("#w-back") as HTMLElement).addEventListener("click", function () {
    w.email = emailEl.value.trim();
    showWizardStep1(w, true);
  });

  async function finish(oauth: string | null): Promise<void> {
    const btn = ov.querySelector("#w-finish") as HTMLButtonElement;
    if (authBusy) { return; }
    if (oauth === "email") {
      w.email = emailEl.value.trim();
      if (!w.email || w.email.indexOf("@") < 0) {
        errEl.textContent = "Please enter a valid email - or continue without login.";
        emailEl.focus();
        return;
      }
    }
    errEl.textContent = "";
    authBusy = true;
    btn.disabled = true;
    try {
      await api<UserPublic>("create_user", {
        username: w.username,
        password: w.password,
        oauthProvider: oauth,
        email: oauth === "email" ? w.email : null
      });
      const u = await api<UserPublic>("login_user", { username: w.username, password: w.password });
      await afterAuth(u, true);
    } catch {
      btn.disabled = false; /* Fehler-Toast kommt aus api() */
    } finally {
      authBusy = false;
    }
  }
  (ov.querySelector("#w-finish") as HTMLElement).addEventListener("click", function () { void finish(w.oauth); });
  (ov.querySelector("#w-skip") as HTMLElement).addEventListener("click", function () { void finish(null); });
}

/* ---- Login screen (also the user manager) ---- */

async function showLogin(opts?: { allowClose?: boolean }): Promise<void> {
  let users: UserPublic[] = [];
  try {
    users = await invoke<UserPublic[]>("list_users");
  } catch { /* Fehler-Toast nicht nötig - leerer Stand reicht */ }
  if (!users.length) {
    showWizardStep1();
    return;
  }
  const canClose = !!opts?.allowClose && !!currentUser;
  let selected: string | null = currentUser ? currentUser.username : null;

  const ov = authShell(
    currentUser ? "Users" : "Welcome back",
    currentUser
      ? "Pick an account to switch, add new users, or log out."
      : "Pick your user and log in. Closing the app logs you out automatically.",
    "",
    '<div class="auth-user-list" id="auth-list"></div>' +
      '<div class="field"><label for="auth-password">Password</label>' +
      '<input id="auth-password" type="password" autocomplete="current-password" placeholder="Password" /></div>' +
      '<div class="err-text" id="auth-err"></div>' +
      '<div class="auth-actions">' +
      '<button class="btn" id="auth-add"><span>+ New user</span></button>' +
      '<span class="flex-spacer"></span>' +
      (canClose ? '<button class="btn" id="auth-cancel">Close</button>' : '') +
      '<button class="btn accent" id="auth-login"><span>Log in</span></button></div>'
  );
  const list = ov.querySelector("#auth-list") as HTMLElement;
  const pwdEl = ov.querySelector("#auth-password") as HTMLInputElement;
  const errEl = ov.querySelector("#auth-err") as HTMLElement;

  function paintSelection(): void {
    list.querySelectorAll<HTMLElement>(".auth-user").forEach(function (row) {
      row.classList.toggle("selected", row.dataset.user === selected);
    });
  }

  for (const u of users) {
    const isSelf = !!currentUser && currentUser.username.toLowerCase() === u.username.toLowerCase();
    const sub = "Port :" + u.port + (oauthLabel(u.oauthProvider) ? " · " + oauthLabel(u.oauthProvider) : "") + (isSelf ? " · logged in" : "");
    const row = el(
      '<div class="auth-user" data-user="' + esc(u.username) + '" role="button" tabindex="0">' +
        '<span class="tb-avatar">' + esc(avatarLetter(u.username)) + '</span>' +
        '<span class="au-name">' + esc(u.username) + '<span class="au-sub">' + esc(sub) + '</span></span>' +
        '<button class="ibtn danger au-del" title="Delete user"><span class="glyph">' + icon("delete") + '</span></button>' +
      '</div>'
    );
    if (isSelf) { row.classList.add("selected"); }
    row.addEventListener("click", function (ev) {
      if ((ev.target as HTMLElement).closest(".au-del")) { return; }
      selected = u.username;
      paintSelection();
      pwdEl.focus();
    });
    row.addEventListener("keydown", function (ev) {
      if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); selected = u.username; paintSelection(); pwdEl.focus(); }
    });
    (row.querySelector(".au-del") as HTMLElement).addEventListener("click", function (ev) {
      ev.stopPropagation();
      const btn = ev.currentTarget as HTMLElement;
      armThen(btn, async function () {
        try {
          await api<void>("delete_user", { username: u.username, password: pwdEl.value || null });
          if (currentUser && currentUser.username.toLowerCase() === u.username.toLowerCase()) {
            currentUser = null;
            renderUserChip();
          }
          toast("User \"" + u.username + "\" deleted");
          await showLogin({ allowClose: !!currentUser });
        } catch { /* Fehler-Toast kommt aus api() */ }
      });
    });
    list.appendChild(row);
  }
  paintSelection();

  async function doLogin(): Promise<void> {
    if (!selected) { errEl.textContent = "Please select a user first."; return; }
    if (!pwdEl.value) { errEl.textContent = "Please enter the password."; pwdEl.focus(); return; }
    const btn = ov.querySelector("#auth-login") as HTMLButtonElement;
    if (authBusy) { return; }
    errEl.textContent = "";
    authBusy = true;
    btn.disabled = true;
    try {
      const u = await api<UserPublic>("login_user", { username: selected, password: pwdEl.value });
      await afterAuth(u, true);
    } catch {
      btn.disabled = false; /* Fehler-Toast kommt aus api() */
    } finally {
      authBusy = false;
    }
  }
  (ov.querySelector("#auth-login") as HTMLElement).addEventListener("click", function () { void doLogin(); });
  pwdEl.addEventListener("keydown", function (ev) { if (ev.key === "Enter") { void doLogin(); } });
  (ov.querySelector("#auth-add") as HTMLElement).addEventListener("click", function () {
    showWizardStep1(undefined, true);
  });
  if (canClose) {
    (ov.querySelector("#auth-cancel") as HTMLElement).addEventListener("click", hideAuth);
  }
  window.setTimeout(function () { pwdEl.focus(); }, 40);
}

async function doLogout(): Promise<void> {
  try { await invoke("logout_user"); } catch { /* bereits abgemeldet */ }
  currentUser = null;
  renderUserChip();
  toast("Logged out - see you soon!");
  await showLogin();
}

function openUserManager(): void {
  if (currentUser) { void showLogin({ allowClose: true }); }
  else { void showLogin(); }
}

/* Backend-Session und Frontend-Stand abgleichen (z. B. nach Tray-Reopen:
 * das Schließen hat automatisch abgemeldet). */
async function reconcileSession(): Promise<void> {
  let backend: UserPublic | null = null;
  try {
    backend = await invoke<UserPublic | null>("current_user");
  } catch { return; }
  if (!backend && currentUser) {
    currentUser = null;
    renderUserChip();
    toast("Logged out automatically (app was closed)");
    await showLogin();
  } else if (backend && (!currentUser || currentUser.username !== backend.username)) {
    await afterAuth(backend, false);
  } else if (backend && currentUser) {
    currentUser = backend;
    renderUserChip();
  }
}

/* Start-Gate: Erststart -> Wizard, sonst Login. Wird erst aufgelöst, wenn
 * ein Benutzer angemeldet ist - danach lädt init() die Daten. */
async function gateAuth(): Promise<void> {
  currentUser = null;
  renderUserChip();
  let has = false;
  try { has = await invoke<boolean>("has_users"); } catch { has = false; }
  if (!has) {
    showWizardStep1();
  } else {
    void showLogin();
  }
  await new Promise<void>(function (res) { authResolve = res; });
}

function wireAuth(): void {
  wireIf<HTMLElement>("#btn-users", function (b) { b.addEventListener("click", openUserManager); });
  wireIf<HTMLElement>("#tb-user-chip", function (b) { b.addEventListener("click", openUserManager); });
  wireIf<HTMLElement>("#btn-logout", function (b) { b.addEventListener("click", function () { void doLogout(); }); });
  window.addEventListener("focus", function () { void reconcileSession(); });
  document.addEventListener("visibilitychange", function () {
    if (!document.hidden) { void reconcileSession(); }
  });
}

async function init(): Promise<void> {
  wireTitlebar();
  applyTabOrder();
  wireTabs();
  wireApiPanel();
  wireUsagePanel();
  updateSortButtonLabel();
  wireIf<HTMLButtonElement>("#btn-add-provider", function (b) { b.addEventListener("click", function () { openProviderPicker(); }); });
  wireIf<HTMLButtonElement>("#btn-goto-providers", function (b) { b.addEventListener("click", function () { showPanel("providers"); }); });
  wireIf<HTMLInputElement>("#setting-theme", function (sw) {
    sw.addEventListener("change", function () {
      applyTheme(sw.checked ? "light" : "dark");
    });
  });
  wireIf<HTMLInputElement>("#setting-compress", function (sw) {
    sw.addEventListener("change", function () {
      updateCompressDimming();
      void saveSettings(sw.closest<HTMLElement>(".set-row"));
    });
  });
  wireIf<HTMLInputElement>("#setting-compress-strength", function (inp) {
    inp.addEventListener("input", function () {
      const label = document.getElementById("setting-compress-strength-value");
      if (label) { label.textContent = inp.value; }
    });
    inp.addEventListener("change", function () {
      void saveSettings(inp.closest<HTMLElement>(".set-row"));
    });
  });
  wireIf<HTMLSelectElement>("#setting-strategy", function (sel) {
    sel.addEventListener("change", function () {
      updateStrategyNote();
      void saveSettings(sel.closest<HTMLElement>(".set-row"));
    });
  });
  wireIf<HTMLInputElement>("#settings-search", function (inp) {
    inp.addEventListener("input", function () { applySettingsFilter(inp.value); });
  });
  wireIf<HTMLInputElement>("#setting-autostart", function (sw) {
    sw.addEventListener("change", function () {
      const want = sw.checked;
      void invoke("set_autostart", { enabled: want }).catch(function (e) {
        sw.checked = !want; // command missing or failed - revert the toggle
        toast("Autostart: " + errText(e), "err");
      });
    });
  });
  wireIf<HTMLElement>("#setting-check-update", function (b) { b.addEventListener("click", function () { void checkForUpdates(); }); });
  wireIf<HTMLInputElement>("#setting-auto-update", function (sw) {
    sw.checked = safeLocalStorageGet("auto-update") !== "0";
    sw.addEventListener("change", function () {
      safeLocalStorageSet("auto-update", sw.checked ? "1" : "0");
    });
  });
  wireIf<HTMLElement>("#models-empty-goto", function (b) { b.addEventListener("click", function () { showPanel("providers"); }); });
  wireIf<HTMLElement>("#setting-export-usage", function (b) { b.addEventListener("click", exportUsage); });
  wireIf<HTMLElement>("#setting-export-config", function (b) { b.addEventListener("click", exportConfig); });
  wireIf<HTMLElement>("#setting-import-config", function (b) { b.addEventListener("click", importConfig); });
  wireIf<HTMLInputElement>("#usage-search", function (inp) { inp.addEventListener("input", scheduleRenderUsage); });
  const modelSearch = document.getElementById("model-search") as HTMLInputElement | null;
  if (modelSearch) { modelSearch.addEventListener("input", scheduleRenderModelCards); }
  const modelSort = document.getElementById("model-sort") as HTMLSelectElement | null;
  if (modelSort) {
    const savedSort = safeLocalStorageGet("model-sort");
    if (savedSort && ["name", "context", "provider", "starred"].indexOf(savedSort) >= 0) {
      modelSort.value = savedSort;
    }
    modelSort.addEventListener("change", function () {
      safeLocalStorageSet("model-sort", modelSort.value);
      scheduleRenderModelCards();
    });
  }
  const modelGrid = document.getElementById("model-grid") as HTMLElement | null;
  if (modelGrid) {
    modelGrid.addEventListener("click", function (e) {
      const star = (e.target as HTMLElement).closest(".star-btn");
      if (!star) { return; }
      const card = star.closest(".mcard") as HTMLElement | null;
      if (!card) { return; }
      const provider = card.dataset.provider || "";
      const model = card.dataset.model || "";
      const entry = findModelEntry(provider, model);
      if (!entry) { return; }
      const flightKey = provider + "::" + model;
      if (starInFlight.has(flightKey)) { return; }
      const nowStarred = !entry.starred;
      starInFlight.add(flightKey);
      void (async function () {
        try {
          applyResult(await api<SaveResult>("set_model_star", { providerId: provider, modelId: model, starred: nowStarred }));
          if (nowStarred) { toast("Starred - picked more often by the random router"); }
        } catch { /* shown */ } finally {
          starInFlight.delete(flightKey);
        }
      })();
    });
    modelGrid.addEventListener("contextmenu", function (e) {
      const card = (e.target as HTMLElement).closest(".mcard") as HTMLElement | null;
      if (!card) { return; }
      e.preventDefault();
      const provider = card.dataset.provider || "";
      const model = card.dataset.model || "";
      showContextMenu(e.clientX, e.clientY, [
        {
          label: "Test model",
          glyph: icon("star"),
          action: function () { void runModelTest(provider, model, card); }
        },
        {
          label: "Delete model",
          glyph: icon("delete"),
          danger: true,
          action: function () {
            void (async function () {
              try {
                applyResult(await api<SaveResult>("delete_provider_model", { providerId: provider, modelId: model }));
                toast("Model removed from provider");
              } catch { /* shown */ }
            })();
          }
        }
      ]);
    });
  }
  wireIf<HTMLElement>("#btn-add-virtual", function (b) { b.addEventListener("click", function () { openVirtualDialog(null); }); });
  window.addEventListener("contextmenu", function (e) { e.preventDefault(); });
  const virtualGrid = document.getElementById("virtual-grid") as HTMLElement | null;
  if (virtualGrid) {
    virtualGrid.addEventListener("contextmenu", function (e) {
      const card = (e.target as HTMLElement).closest(".mcard") as HTMLElement | null;
      if (!card) { return; }
      e.preventDefault();
      const vid = card.querySelector(".name-text")?.textContent || "";
      showContextMenu(e.clientX, e.clientY, [
        {
          label: "Test virtual model",
          glyph: icon("star"),
          action: function () { void runVirtualModelTest(vid, card); }
        },
        {
          label: "Edit virtual model",
          glyph: icon("edit"),
          action: function () {
            const v = (cfg.virtualModels || []).find(function (x) { return x.id === vid; });
            if (v) { openVirtualDialog(v); }
          }
        },
        {
          label: "Delete virtual model",
          glyph: icon("delete"),
          danger: true,
          action: function () {
            void (async function () {
              try {
                applyResult(await api<SaveResult>("delete_virtual_model", { id: vid }));
                toast("Virtual model deleted");
              } catch { /* shown */ }
            })();
          }
        }
      ]);
    });
  }
  const providerGrid = document.getElementById("provider-grid") as HTMLElement | null;
  if (providerGrid) {
    providerGrid.addEventListener("contextmenu", function (e) {
      const card = (e.target as HTMLElement).closest(".mcard") as HTMLElement | null;
      if (!card) { return; }
      e.preventDefault();
      const providerId = card.dataset.providerId || card.querySelector(".name-text")?.textContent || "";
      showContextMenu(e.clientX, e.clientY, [
        {
          label: "Test provider",
          glyph: icon("star"),
          action: function () { void runProviderTest(providerId, card); }
        },
        {
          label: "Delete provider",
          glyph: icon("delete"),
          danger: true,
          action: function () {
            void (async function () {
              try {
                applyResult(await api<SaveResult>("delete_provider", { id: providerId }));
                toast("Provider deleted");
              } catch { /* shown */ }
            })();
          }
        }
      ]);
    });
  }
  wireIf<HTMLElement>("#usage-body", function (body) {
    body.addEventListener("contextmenu", function (e) {
      const tr = (e.target as HTMLElement).closest(".usage-row") as HTMLElement | null;
      if (!tr) { return; }
      e.preventDefault();
      const provider = tr.dataset.provider || "";
      const model = tr.dataset.model || "";
      showContextMenu(e.clientX, e.clientY, [{
        label: "Delete data",
        glyph: icon("delete"),
        danger: true,
        action: function () {
          void (async function () {
            try {
              await api<void>("delete_usage_model", { providerId: provider, modelId: model });
              await loadUsage();
              toast("Usage data for " + model + " deleted");
            } catch { /* shown */ }
          })();
        }
      }]);
    });
  });
  window.addEventListener("click", hideContextMenu);
  window.addEventListener("blur", hideContextMenu);
  // Multi-user gate: Erststart -> Onboarding-Wizard, sonst Login. Die
  // Backend-Session lebt nur im Speicher, daher startet die App immer
  // abgemeldet. Erst danach werden Benutzerdaten geladen.
  wireAuth();
  renderUserChip();
  await gateAuth();
  try {
    const v = await invoke<string>("app_version");
    const sv = document.getElementById("side-version");
    if (sv) { sv.textContent = "Multi LLM v" + v; }
  } catch { /* ignore */ }
  try {
    cfg = await invoke<PublicConfig>("load_settings");
  } catch (e) {
    toast("Failed to load settings: " + errText(e), "err");
  }
  // Update model modalities (image/video/audio support) for existing models.
  void updateAllModelModalities();
  try {
    status = await invoke<ProxyStatus>("proxy_status");
  } catch { /* ignore */ }
  await renderProviders();
  scheduleRenderModelCards();
  renderVirtual();
  renderApiPanel();
  loadUsage();
  refreshUsageAll();
  // First launch: no providers yet - lead the user to the Providers tab.
  if (!cfg.providers.length) { showPanel("providers"); }
  startPolling();
  document.addEventListener("visibilitychange", function () {
    if (document.hidden) { stopPolling(); } else { startPolling(); }
  });
  // Start-up update check, unless the user turned it off in Settings.
  if (safeLocalStorageGet("auto-update") !== "0") { void checkForUpdates(true); }
  // Ctrl+1..7 switches the side-nav panels.
  window.addEventListener("keydown", function (e) {
    if (!e.ctrlKey || e.altKey || e.shiftKey || e.metaKey) { return; }
    const n = parseInt(e.key, 10);
    if (!(n >= 1 && n <= 6)) { return; }
    e.preventDefault();
    showPanel(["models", "providers", "virtual", "api", "usage", "settings"][n - 1]);
  });
}

async function updateAllModelModalities(): Promise<void> {
  // Base every decision on the freshest backend config so a stale local
  // snapshot can never be written back over mutations made meanwhile
  // (e.g. by the periodic poll or an open dialog).
  try { cfg = await invoke<PublicConfig>("load_settings"); } catch { return; }
  if (!cfg.providers.length) { return; }
  interface ProviderPayload {
    originalId: string | null;
    id: string;
    baseUrl: string;
    apiKey: string | null;
    apiFormat: string;
    models: { id: string; name: string; enabled: boolean; contextLength: number | null; starred: boolean; inputModalities: string[] | null; outputModalities: string[] | null }[];
  }
  const changed: ProviderPayload[] = [];
  for (const p of cfg.providers) {
    if (!p.baseUrl) { continue; }
    try {
      const fetched = await api<{ id: string; name: string; contextLength?: number; inputModalities?: string[]; outputModalities?: string[] }[]>("fetch_provider_models", {
        baseUrl: p.baseUrl,
        apiKey: null,
        providerId: p.id,
        apiFormat: p.apiFormat
      });
      if (!fetched.length) { continue; }
      const modelMap = new Map<string, ModelEntry>();
      for (const m of p.models) {
        modelMap.set(m.id, { ...m });
      }
      let needsSave = false;
      for (const f of fetched) {
        const existing = modelMap.get(f.id);
        if (existing) {
          // Metadata enrichment only. Modality refreshes alone never justify a
          // config persist + proxy restart.
          if (f.contextLength && !existing.contextLength) { existing.contextLength = f.contextLength; needsSave = true; }
          if (f.name && !existing.name) { existing.name = f.name; needsSave = true; }
          if (f.inputModalities && f.inputModalities.length) { existing.inputModalities = f.inputModalities; }
          if (f.outputModalities && f.outputModalities.length) { existing.outputModalities = f.outputModalities; }
        } else {
          // Newly discovered models stay disabled until the user enables them,
          // so a deleted model can never silently come back enabled.
          modelMap.set(f.id, { id: f.id, name: f.name || f.id, enabled: false, contextLength: f.contextLength, inputModalities: f.inputModalities, outputModalities: f.outputModalities });
          needsSave = true;
        }
      }
      if (!needsSave) { continue; }
      changed.push({
        originalId: p.id,
        id: p.id,
        baseUrl: p.baseUrl,
        apiKey: null,
        apiFormat: p.apiFormat,
        models: Array.from(modelMap.values()).map(function (m) {
          return { id: m.id.trim(), name: (m.name.trim() || m.id.trim()), enabled: m.enabled, contextLength: m.contextLength === undefined ? null : m.contextLength, starred: !!m.starred, inputModalities: (m.inputModalities && m.inputModalities.length ? m.inputModalities : null), outputModalities: (m.outputModalities && m.outputModalities.length ? m.outputModalities : null) };
        })
      });
    } catch { /* skip unreachable providers */ }
  }
  if (!changed.length) { return; }
  // One batched save pass at the end instead of interleaving saves with
  // fetches (the backend restarts the proxy per upsert, so only providers
  // with real changes are touched); keep the last authoritative snapshot.
  let lastRes: SaveResult | null = null;
  for (const u of changed) {
    try {
      lastRes = await api<SaveResult>("upsert_provider", { provider: u });
    } catch { /* skip failed saves */ }
  }
  if (lastRes) { applyResult(lastRes); }
}

init();

/* ================= Client config snippets (idea #10) =================
 * Self-contained read-only feature: builds ready-to-paste OpenAI-compatible
 * client configs from the live port / LAN URL. The API key is ALWAYS the
 * literal placeholder <your-local-key> - never a real key. */

const SNIPPETS_KEY = "<your-local-key>";

// Base URL exactly as shown by the URL preview chip (falls back to the
// current port input, then to stored settings).
function snippetBaseUrl(): string {
  const chip = document.getElementById("api-url-preview");
  const fromChip = chip ? (chip.textContent || "").trim() : "";
  if (fromChip && fromChip !== "\u2014") { return fromChip; }
  const portEl = document.getElementById("api-port") as HTMLInputElement | null;
  const v = portEl ? parseInt(portEl.value, 10) : NaN;
  return "http://127.0.0.1:" + (isNaN(v) ? cfg.api.port : v) + "/v1";
}

interface SnippetTarget { id: string; label: string; build: (base: string) => string; }

const SNIPPET_TARGETS: SnippetTarget[] = [
  {
    id: "env",
    label: "Env vars",
    build: function (base: string): string {
      return (
        "# PowerShell\n" +
        "$env:OPENAI_BASE_URL = \"" + base + "\"\n" +
        "$env:OPENAI_API_KEY = \"" + SNIPPETS_KEY + "\"\n" +
        "\n" +
        "# bash / zsh\n" +
        'export OPENAI_BASE_URL="' + base + '"\n' +
        'export OPENAI_API_KEY="' + SNIPPETS_KEY + '"'
      );
    }
  },
  {
    id: "curl",
    label: "curl",
    build: function (base: string): string {
      return (
        "curl " + base + "/chat/completions \\\n" +
        '  -H "Content-Type: application/json" \\\n' +
        '  -H "Authorization: Bearer ' + SNIPPETS_KEY + '" \\\n' +
        "  -d '{\n" +
        '    "model": "<model-id>",\n' +
        '    "messages": [{"role": "user", "content": "Hello!"}],\n' +
        '    "stream": true\n' +
        "  }'"
      );
    }
  },
  {
    id: "python",
    label: "Python",
    build: function (base: string): string {
      return (
        "# pip install openai\n" +
        "from openai import OpenAI\n" +
        "\n" +
        "client = OpenAI(\n" +
        '    base_url="' + base + '",\n' +
        '    api_key="' + SNIPPETS_KEY + '",\n' +
        ")\n" +
        "\n" +
        "stream = client.chat.completions.create(\n" +
        '    model="<model-id>",\n' +
        '    messages=[{"role": "user", "content": "Hello!"}],\n' +
        "    stream=True,\n" +
        ")\n" +
        "for chunk in stream:\n" +
        "    delta = chunk.choices[0].delta.content if chunk.choices else None\n" +
        '    if delta:\n' +
        '        print(delta, end="")'
      );
    }
  },
  {
    id: "ts",
    label: "TypeScript",
    build: function (base: string): string {
      return (
        "const res = await fetch(\"" + base + "/chat/completions\", {\n" +
        '  method: "POST",\n' +
        "  headers: {\n" +
        '    "Content-Type": "application/json",\n' +
        '    Authorization: "Bearer ' + SNIPPETS_KEY + '",\n' +
        "  },\n" +
        "  body: JSON.stringify({\n" +
        '    model: "<model-id>",\n' +
        '    messages: [{ role: "user", content: "Hello!" }],\n' +
        "  }),\n" +
        "});\n" +
        "const data = await res.json();\n" +
        "console.log(data.choices[0].message.content);"
      );
    }
  },
  {
    id: "generic",
    label: "Base URL only",
    build: function (base: string): string {
      return (
        "Base URL:     " + base + "\n" +
        "API key:      " + SNIPPETS_KEY + "\n" +
        "Auth header:  Authorization: Bearer " + SNIPPETS_KEY + "\n" +
        "\n" +
        "Point any OpenAI-compatible client at these values.\n" +
        "Endpoints: GET /v1/models - POST /v1/chat/completions"
      );
    }
  }
];

function openClientSnippetsDialog(): void {
  closeDialog();
  const root = $("#dialog-root");
  let tabsHtml = "";
  for (let i = 0; i < SNIPPET_TARGETS.length; i++) {
    const t = SNIPPET_TARGETS[i];
    tabsHtml += '<button class="rbtn' + (i === 0 ? " active" : "") + '" data-snippet="' + t.id + '">' + esc(t.label) + '</button>';
  }
  const lanOn = (document.getElementById("api-expose") as HTMLInputElement | null);
  const lanUrl = lanOn && lanOn.checked ? (status.lanUrl || "") : "";
  const overlay = el(
    '<div class="overlay">' +
      '<div class="dialog" style="width:640px;">' +
        '<div class="dialog-body">' +
          '<div class="dialog-title">Client config snippets</div>' +
          '<p class="hint-sm" style="margin:-6px 0 10px;">Replace <span class="mono">&lt;your-local-key&gt;</span> with one of your keys from the API Keys list below, and <span class="mono">&lt;model-id&gt;</span> with an enabled model.' +
            (lanUrl ? "<br/>LAN devices can use this instead: <span class=\"mono\">" + esc(lanUrl) + "</span>" : "") +
          '</p>' +
          '<div class="settings-tabs" role="tablist" style="margin-bottom:10px;">' + tabsHtml + '</div>' +
          '<div style="position:relative;">' +
            '<pre id="snippet-code" class="mono" style="margin:0;background:rgba(127,127,127,0.12);border-radius:8px;padding:14px;font-size:12px;line-height:1.5;overflow-x:auto;user-select:all;"></pre>' +
            '<button class="ibtn" id="snippet-copy" title="Copy snippet" style="position:absolute;top:8px;right:8px;background:var(--bg-elev,rgba(0,0,0,0.3));"><span class="glyph">' + icon("copy") + '</span></button>' +
          '</div>' +
        '</div>' +
        '<div class="dialog-footer">' +
          '<button class="btn accent" id="snippet-done">Done</button>' +
        '</div>' +
      '</div>' +
    '</div>'
  );
  root.appendChild(overlay);

  const pre = overlay.querySelector("#snippet-code") as HTMLElement;
  function show(id: string): void {
    const t = SNIPPET_TARGETS.find(function (x) { return x.id === id; }) || SNIPPET_TARGETS[0];
    // textContent (not innerHTML) so <your-local-key> renders literally.
    pre.textContent = t.build(snippetBaseUrl());
  }
  overlay.querySelectorAll<HTMLButtonElement>("[data-snippet]").forEach(function (b) {
    b.addEventListener("click", function () {
      overlay.querySelectorAll<HTMLButtonElement>("[data-snippet]").forEach(function (x) { x.classList.toggle("active", x === b); });
      show(b.dataset.snippet || "env");
    });
  });
  (overlay.querySelector("#snippet-copy") as HTMLElement).addEventListener("click", function () {
    copyText(pre.textContent || "");
  });
  (overlay.querySelector("#snippet-done") as HTMLElement).addEventListener("click", function () { closeDialog(); });
  wireOverlayClose(overlay);
  show("env");
}

wireIf<HTMLElement>("#btn-client-snippets", function (b) {
  b.addEventListener("click", function () { openClientSnippetsDialog(); });
});

/* Convert every [data-icon="..."] into its inline SVG. */
(function injectIcons(): void {
  document.querySelectorAll("[data-icon]").forEach(function (node) {
    const name = (node as HTMLElement).getAttribute("data-icon");
    if (name) { (node as HTMLElement).innerHTML = icon(name); }
  });
})();






