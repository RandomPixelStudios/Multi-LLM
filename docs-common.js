/* Multi LLM documentation - shared shell.
 *
 * Every docs page includes this file. It owns the three things that must
 * behave identically everywhere: the navigation, the theme, and the consent
 * banner. Pages ship only their own article content.
 *
 * The page declares what it needs in data attributes on <body>:
 *   data-page    key into PAGES below, used to mark the current nav entry
 *   data-title   document title, gets the product name appended
 */
(function () {
  "use strict";

  var SITE = "https://github.com/RandomPixelStudios/Multi-LLM";
  var RELEASES = SITE + "/releases";
  var LEGAL = "legal.html";
  var REPO = SITE + "/blob/Windows/";

  /* One entry per docs page. href is relative, so the same array works from
   * the site root and from a subdirectory. */
  var PAGES = [
    { key: "home", href: "docs.html", title: "Documentation", blurb: "Everything about Multi LLM, from first install to a team setup." },
    { key: "start", href: "getting-started.html", title: "Getting started", blurb: "Install, run it, verify it, and connect your first tool." },
    { key: "connect", href: "connect.html", title: "Connect a tool", blurb: "Step-by-step for 21 editors, agents and chat front ends." },
    { key: "concepts", href: "concepts.html", title: "Concepts", blurb: "How a request flows, what tokens cost, and what the proxy does." },
    { key: "config", href: "configuration.html", title: "Configuration", blurb: "Every setting in the app, what it changes, and what it costs." },
    { key: "guides", href: "guides.html", title: "Guides", blurb: "Task-shaped walkthroughs, from first provider to bundle tiers." },
    { key: "api", href: "api.html", title: "API reference", blurb: "Endpoints, request shape, and copy-paste recipes." },
    { key: "docker", href: "docker.html", title: "Docker", blurb: "Container setup, multi-user mode, backup and hardening." },
    { key: "faq", href: "faq.html", title: "FAQ", blurb: "Short answers to the questions that come up most." }
  ];

  var THEME_KEY = "ml-prefs";
  var CONSENT_KEY = "ml-consent";

  function esc(s) {
    return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
  }

  function page() {
    return document.body.getAttribute("data-page") || "home";
  }

  /* ---------------- Theme ---------------- */

  function applyTheme(mode) {
    var eff = mode === "system" ? sysTheme() : mode;
    if (eff === "light") { document.documentElement.setAttribute("data-theme", "light"); }
    else { document.documentElement.removeAttribute("data-theme"); }
    var sw = document.getElementById("theme-foot");
    if (sw) { sw.setAttribute("aria-label", eff === "light" ? "Switch to dark theme" : "Switch to light theme"); }
  }

  function sysTheme() {
    try {
      return window.matchMedia && window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
    } catch (e) { return "dark"; }
  }

  function readTheme() {
    try {
      var p = JSON.parse(localStorage.getItem(THEME_KEY));
      if (p && (p.theme === "light" || p.theme === "dark" || p.theme === "system")) { return p.theme; }
    } catch (e) {}
    return "dark";
  }

  function setTheme(mode, persist) {
    applyTheme(mode);
    if (persist) {
      try {
        localStorage.setItem(THEME_KEY, JSON.stringify({ theme: mode, consent: true, ts: Date.now() }));
      } catch (e) {}
    }
  }

  /* ---------------- Consent for external content ----------------
   * Before consent nothing is loaded from a third party: no Google Fonts,
   * no YouTube player, only files that ship with this site. "Necessary
   * only" keeps it that way permanently; "accept all" loads the fonts and
   * swaps the video placeholder for the real player.
   *
   * GitHub Pages writes its server log before any page of ours can ask
   * anything. The banner says so, because no button here can prevent it. */

  var GOOGLE_FONTS =
    "https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700;800;900" +
    "&family=JetBrains+Mono:wght@400;500&display=swap";

  function readConsent() {
    try { return localStorage.getItem(CONSENT_KEY); } catch (e) { return null; }
  }

  function writeConsent(value) {
    try { localStorage.setItem(CONSENT_KEY, value); } catch (e) {}
  }

  function clearConsent() {
    try { localStorage.removeItem(CONSENT_KEY); } catch (e) {}
  }

  /* Injects the font stylesheet only after consent, and removes it again
   * when the visitor switches back to necessary-only. */
  function applyFonts(on) {
    var id = "ml-fonts";
    var el = document.getElementById(id);
    if (on) {
      if (el) { return; }
      var pre1 = document.createElement("link");
      pre1.rel = "preconnect";
      pre1.href = "https://fonts.googleapis.com";
      var pre2 = document.createElement("link");
      pre2.rel = "preconnect";
      pre2.href = "https://fonts.gstatic.com";
      pre2.crossOrigin = "anonymous";
      var css = document.createElement("link");
      css.id = id;
      css.rel = "stylesheet";
      css.href = GOOGLE_FONTS;
      document.head.appendChild(pre1);
      document.head.appendChild(pre2);
      document.head.appendChild(css);
    } else {
      if (el) { el.remove(); }
      var p1 = document.querySelector('link[href="https://fonts.googleapis.com"][rel="preconnect"]:not([id])');
      var p2 = document.querySelector('link[href="https://fonts.gstatic.com"][rel="preconnect"]:not([id])');
      if (p1) { p1.remove(); }
      if (p2) { p2.remove(); }
    }
  }

  /* Replaces the click-to-load placeholder with the real YouTube embed.
   * Without consent the placeholder stays, so nothing is fetched. */
  function applyVideo(on) {
    var slot = document.getElementById("video-slot");
    if (!slot) { return; }
    if (on) {
      if (slot.dataset.loaded === "1") { return; }
      slot.dataset.loaded = "1";
      slot.innerHTML =
        '<iframe class="video-frame" src="https://www.youtube-nocookie.com/embed/iSYeGJ6OnGU?rel=0"\n' +
        '        title="MultiLLM Launch Trailer" loading="lazy"\n' +
        '        referrerpolicy="strict-origin-when-cross-origin"\n' +
        '        allow="accelerometer; clipboard-write; encrypted-media; gyroscope; picture-in-picture; web-share"\n' +
        '        allowfullscreen></iframe>';
    } else if (slot.dataset.loaded === "1") {
      slot.dataset.loaded = "0";
      slot.innerHTML =
        '<button class="video-gate" type="button">' +
        '<span class="vg-play" aria-hidden="true">' +
        '<svg viewBox="0 0 24 24" width="30" height="30" fill="currentColor"><path d="M8 5.5v13l11-6.5z"/></svg>' +
        "</span>" +
        '<span class="vg-text">Trailer laden<small>Klicken erlaubt das Laden von YouTube</small></span>' +
        "</button>";
    }
  }

  function showConsent() {
    var b = document.getElementById("consent-banner");
    if (!b) { return; }
    b.classList.remove("hidden");
    document.body.classList.add("consent-open");
  }

  function hideConsent() {
    var b = document.getElementById("consent-banner");
    if (!b) { return; }
    b.classList.add("hidden");
    document.body.classList.remove("consent-open");
  }

  /* ---------------- Chrome ---------------- */

  /* The topbar carries only the two site-level links. The docs pages are a
   * second level and live in the sidebar, not in the bar. */
  function buildTopbar() {
    var cur = page();
    var home = '<a class="top-link' + (cur === "site" ? " active" : "") + '" href="index.html">Home</a>';
    var docs = '<a class="top-link' + (cur !== "site" ? " active" : "") + '" href="docs.html">Documentation</a>';
    var host = document.getElementById("topbar-slot");
    if (host) {
      host.innerHTML =
        '<div class="topbar-inner">' +
          '<a class="brand" href="index.html" aria-label="Multi LLM home">' +
            '<span class="brand-name caps">MULTILLM</span><span class="beta-badge">BETA</span>' +
          "</a>" +
          '<nav class="topbar-nav" aria-label="Main">' + home + docs + "</nav>" +
          '<div class="topbar-actions">' +
            '<a class="soc-btn" href="https://www.youtube.com/@RandomPixelStudios" target="_blank" rel="noopener" title="YouTube" aria-label="YouTube">' +
              '<svg viewBox="0 0 24 24" width="17" height="17" fill="currentColor" aria-hidden="true"><path d="M23.5 6.2a3 3 0 0 0-2.1-2.1C19.5 3.5 12 3.5 12 3.5s-7.5 0-9.4.6A3 3 0 0 0 .5 6.2 31.3 31.3 0 0 0 0 12a31.3 31.3 0 0 0 .5 5.8 3 3 0 0 0 2.1 2.1c1.9.6 9.4.6 9.4.6s7.5 0 9.4-.6a3 3 0 0 0 2.1-2.1A31.3 31.3 0 0 0 24 12a31.3 31.3 0 0 0-.5-5.8zM9.6 15.6V8.4L15.8 12z"/></svg>' +
            "</a>" +
            '<a class="soc-btn" href="https://discord.gg/CZ9Mr7epqj" target="_blank" rel="noopener" title="Discord" aria-label="Discord">' +
              '<svg viewBox="0 0 24 24" width="17" height="17" fill="currentColor" aria-hidden="true"><path d="M20.3 4.4A19.8 19.8 0 0 0 15.4 3c-.2.4-.5.9-.6 1.3a18.3 18.3 0 0 0-5.5 0C9.1 3.9 8.8 3.4 8.6 3a19.7 19.7 0 0 0-4.9 1.5C.8 9.1-.3 13.6.2 18.1a19.9 19.9 0 0 0 6 3c.5-.7.9-1.4 1.3-2.1-.7-.3-1.4-.6-2-1l.5-.4a14.2 14.2 0 0 0 12.1 0l.5.4c-.7.4-1.4.7-2.1 1 .4.7.8 1.4 1.3 2.1zM8 15.3c-1.2 0-2.1-1-2.1-2.3S6.8 10.7 8 10.7s2.2 1 2.1 2.3c0 1.2-.9 2.3-2.1 2.3zm8 0c-1.2 0-2.1-1-2.1-2.3s.9-2.3 2.1-2.3 2.1 1 2.1 2.3c0 1.2-.9 2.3-2.1 2.3z"/></svg>' +
            "</a>" +
            '<button class="soc-btn" id="theme-foot" title="Toggle theme" aria-label="Toggle theme">' +
              '<svg class="ic-moon" viewBox="0 0 24 24" width="17" height="17" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z"/></svg>' +
              '<svg class="ic-sun" viewBox="0 0 24 24" width="17" height="17" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4 1.4"/></svg>' +
            "</button>" +
          "</div>" +
        "</div>";
    }
  }

  /* The sidebar carries the page list. Every page has the same one, so it is
   * built here instead of being copied into eight files that would drift. */
  function buildSidebar() {
    var cur = page();
    var host = document.getElementById("page-list");
    if (!host) { return; }
    host.innerHTML = PAGES.filter(function (p) { return p.key !== "home"; })
      .map(function (p) {
        return '<a class="' + (p.key === cur ? "active" : "") + '" href="' + p.href + '">' + esc(p.title) + "</a>";
      }).join("");
  }

  function buildFooter() {
    var host = document.getElementById("footer-slot");
    if (!host) { return; }
    host.innerHTML =
      '<div class="foot-inner">' +
        '<div class="brand"><span class="brand-name caps sm">MULTILLM</span></div>' +
        '<a class="btn accent sm" href="https://ko-fi.com/randompixxelstudios" target="_blank" rel="noopener">' +
          '<svg viewBox="0 0 24 24" width="14" height="14" fill="currentColor" aria-hidden="true"><path d="M12 21C7 16.5 3 13 3 8.8 3 6 5.2 4 7.8 4c1.7 0 3.2.9 4.2 2.3C13 4.9 14.5 4 16.2 4 18.8 4 21 6 21 8.8c0 4.2-4 7.7-9 12.2z"/></svg>' +
          "Donate</a>" +
        '<nav class="foot-legal" aria-label="Legal">' +
          '<a href="' + LEGAL + '#imprint">Imprint</a>' +
          '<a href="' + LEGAL + '#privacy">Privacy</a>' +
          '<a href="' + LEGAL + '#terms">Terms</a>' +
        "</nav>" +
        '<button type="button" class="foot-consent" id="foot-consent">Privacy settings</button>' +
      "</div>";
  }

  function buildConsent() {
    var host = document.getElementById("consent-slot");
    if (!host) { return; }
    host.innerHTML =
      '<div class="consent-inner">' +
        "<div class=" + '"consent-text"' + ">" +
          "<p><b>Before you decide</b> this site loads nothing from a third party: " +
            "no Google Fonts, no YouTube, only files that ship with this site.</p>" +
          "<p><b>After &bdquo;Accept all&rdquo;</b> Google Fonts load and the YouTube " +
            "video is loaded. Until then neither is requested.</p>" +
          "<p class=" + '"consent-note"' + ">Visiting this site, your IP address is " +
            "processed by GitHub Pages and logged for security. That happens " +
            "independently of your choice and cannot be prevented by this banner.</p>" +
          '<p class="consent-links">Details in the ' +
            '<a href="' + LEGAL + '#privacy">privacy policy</a>. You can change ' +
            'this decision at any time via &ldquo;Privacy settings&rdquo; in the footer.</p>' +
        "</div>" +
        '<div class="consent-actions">' +
          '<button class="btn ghost" id="consent-decline">Necessary only</button>' +
          '<button class="btn accent" id="consent-accept">Accept all</button>' +
        "</div>" +
      "</div>";
  }

  /* ---------------- Scrollspy + search ---------------- */

  function initScrollspy() {
    var links = Array.prototype.slice.call(document.querySelectorAll(".toc a"));
    if (!links.length || !("IntersectionObserver" in window)) { return; }
    var map = {};
    links.forEach(function (a) {
      var id = a.getAttribute("href").slice(1);
      (map[id] = map[id] || []).push(a);
    });
    var spy = new IntersectionObserver(function (entries) {
      entries.forEach(function (e) {
        if (!e.isIntersecting) { return; }
        links.forEach(function (a) { a.classList.remove("active"); });
        (map[e.target.id] || []).forEach(function (a) { a.classList.add("active"); });
      });
    }, { rootMargin: "-15% 0px -70% 0px" });
    Object.keys(map).forEach(function (id) {
      var s = document.getElementById(id);
      if (s) { spy.observe(s); }
    });
  }

  /* Search spans every page, not just the current one. The index is a
   * generated JSON file (npm run search-index) that is committed next to the
   * pages, so it cannot drift from the content it describes. */
  var searchIndex = null;
  var searchLoading = false;
  var searchPending = null;

  function loadIndex() {
    if (searchIndex) { return Promise.resolve(searchIndex); }
    if (searchLoading) { return searchPending; }
    searchLoading = true;
    // searchLoading muss zurueckgesetzt werden, sonst liefert jeder
    // weitere Aufruf ein sofort aufgeloestes Promise mit [] zurueck und
    // die Suche bleibt dauerhaft leer.
    searchPending = fetch("search-index.json")
      .then(function (r) { return r.ok ? r.json() : []; })
      .then(function (data) { searchIndex = data || []; return searchIndex; })
      .catch(function () { searchIndex = []; return searchIndex; })
      .then(function (v) { searchLoading = false; return v; });
    return searchPending;
  }

  function initSearch() {
    var input = document.getElementById("doc-search");
    var out = document.getElementById("search-results");
    if (!input || !out) { return; }

    function pageHref(entry) {
      return (entry.page === page()) ? "#" + entry.id : entry.page + "#" + entry.id;
    }

    function run() {
      var q = input.value.trim().toLowerCase();
      if (q.length < 2) { out.innerHTML = ""; out.classList.add("hidden"); return; }
      var entries = searchIndex || [];
      var words = q.split(/\s+/);
      var hits = entries.filter(function (e) {
        var hay = (e.title + " " + e.text).toLowerCase();
        return words.every(function (w) { return hay.indexOf(w) >= 0; });
      }).sort(function (a, b) {
        // Title matches rank above body matches.
        var at = a.title.toLowerCase().indexOf(q) >= 0 ? 0 : 1;
        var bt = b.title.toLowerCase().indexOf(q) >= 0 ? 0 : 1;
        return at - bt;
      }).slice(0, 15);

      if (!hits.length) {
        out.innerHTML = '<p class="search-empty">Nothing found for &ldquo;' + esc(input.value) + "&rdquo;.</p>";
      } else {
        out.innerHTML = hits.map(function (e) {
          var where = e.page === page() ? "on this page" : e.page.replace(".html", "").replace(/-/g, " ");
          var snippet = e.text.length > 130 ? e.text.slice(0, 130) + "&hellip;" : e.text;
          return '<a class="search-hit" href="' + pageHref(e) + '">' +
            '<span class="sh-title">' + esc(e.title) + "</span>" +
            '<span class="sh-where">' + esc(where) + "</span>" +
            '<span class="sh-text">' + esc(snippet) + "</span></a>";
        }).join("");
      }
      out.classList.remove("hidden");
    }

    input.addEventListener("focus", function () {
      loadIndex().then(function () { if (input.value.trim().length >= 2) { run(); } });
    });
    input.addEventListener("input", function () {
      if (searchIndex) { run(); } else { loadIndex().then(run); }
    });
    document.addEventListener("keydown", function (e) {
      if (e.key === "Escape") { out.classList.add("hidden"); input.blur(); }
    });
  }

  /* ---------------- Boot ---------------- */

  buildConsent();
  buildTopbar();
  buildSidebar();
  buildFooter();
  applyTheme(readTheme());

  var tf = document.getElementById("theme-foot");
  if (tf) {
    tf.addEventListener("click", function () {
      setTheme(document.documentElement.getAttribute("data-theme") === "light" ? "dark" : "light", true);
    });
  }

  try {
    var mq = window.matchMedia("(prefers-color-scheme: light)");
    if (mq.addEventListener) {
      mq.addEventListener("change", function () { if (readTheme() === "system") { applyTheme("system"); } });
    }
  } catch (e) {}

  /* Consent: null = never decided, "all" = fonts + video, "necessary" =
     local files only. Decided once, then remembered. */
  var decision = readConsent();
  applyFonts(decision === "all");
  applyVideo(decision === "all");
  if (decision) { hideConsent(); } else { showConsent(); }

  var acc = document.getElementById("consent-accept");
  var dec = document.getElementById("consent-decline");
  if (acc) {
    acc.addEventListener("click", function () {
      writeConsent("all");
      applyFonts(true);
      applyVideo(true);
      hideConsent();
    });
  }
  if (dec) {
    dec.addEventListener("click", function () {
      writeConsent("necessary");
      applyFonts(false);
      applyVideo(false);
      hideConsent();
    });
  }
  /* Clicking the video placeholder without consent opens the banner
     instead of loading anything. */
  document.addEventListener("click", function (e) {
    if (e.target && e.target.id === "video-gate" && readConsent() !== "all") {
      showConsent();
    }
  });

  /* A stored consent has to stay withdrawable. The footer button clears the
     decision and reopens the banner, so switching back to "necessary only"
     is possible at any time - and, if it was "accept all", the fonts and the
     video go away again on the spot instead of after the next reload. */
  var reopen = document.getElementById("foot-consent");
  if (reopen) {
    reopen.addEventListener("click", function () {
      var wasAll = readConsent() === "all";
      clearConsent();
      if (wasAll) {
        applyFonts(false);
        applyVideo(false);
      }
      showConsent();
      var first = document.getElementById("consent-accept");
      if (first) { first.focus(); }
    });
  }

  initScrollspy();
  initSearch();

  /* Copy buttons for code blocks, injected so no page repeats the wiring. */
  document.querySelectorAll(".doc-code, .doc-pre").forEach(function (block) {
    var btn = document.createElement("button");
    btn.className = "copy-btn";
    btn.type = "button";
    btn.textContent = "Copy";
    btn.addEventListener("click", function () {
      var text = block.getAttribute("data-copy") || block.textContent;
      function done() {
        btn.textContent = "Copied";
        btn.classList.add("done");
        window.setTimeout(function () { btn.textContent = "Copy"; btn.classList.remove("done"); }, 1600);
      }
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(done, function () { btn.textContent = "Press Ctrl+C"; });
      } else {
        done();
      }
    });
    block.appendChild(btn);
  });

  /* Expose the page table for the landing page. */
  window.DOC_PAGES = PAGES;
  window.DOC_SITE = SITE;
  window.DOC_RELEASES = RELEASES;
})();
