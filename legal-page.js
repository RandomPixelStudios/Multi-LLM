/* Language switcher for the legal page only.
   The theme toggle and the scroll spy live in docs-common.js, which this
   page also loads - one implementation each, shared with every other
   page. This file only adds what is specific here: the page title and
   meta description change with the language. */
(function () {
  "use strict";
  var LANG_KEY = "ml-lang";
  var LANG = "en";
  var FLAGS = {
    de: '<svg viewBox="0 0 24 18"><rect width="24" height="6" fill="#151515"/><rect y="6" width="24" height="6" fill="#DD0000"/><rect y="12" width="24" height="6" fill="#FFCE00"/></svg>',
    en: '<svg viewBox="0 0 24 18"><rect width="24" height="18" fill="#012169"/><path d="M0 0l24 18M24 0L0 18" stroke="#ffffff" stroke-width="3.4"/><path d="M0 0l24 18M24 0L0 18" stroke="#C8102E" stroke-width="2.2"/><path d="M12 0v18M0 9h24" stroke="#ffffff" stroke-width="5.6"/><path d="M12 0v18M0 9h24" stroke="#C8102E" stroke-width="3.2"/></svg>'
  };
  var TITLE = { en: "Legal \u2013 Multi LLM", de: "Rechtliches \u2013 Multi LLM" };
  var DESC = {
    en: "Multi LLM legal notices \u2013 imprint, privacy policy, terms of use and disclaimer (English and German).",
    de: "Multi-LLM-Rechtstexte \u2013 Impressum, Datenschutz, Bedingungen und Haftungsausschluss (Englisch und Deutsch)."
  };
  function setLang(lang, persist) {
    LANG = (lang === "de") ? "de" : "en";
    document.documentElement.lang = LANG;
    document.title = TITLE[LANG];
    var md = document.querySelector('meta[name="description"]');
    if (md) md.setAttribute("content", DESC[LANG]);
    var flag = document.getElementById("lang-flag"), code = document.getElementById("lang-code");
    if (flag) flag.innerHTML = FLAGS[LANG];
    if (code) code.textContent = LANG.toUpperCase();
    var opts = document.querySelectorAll(".lang-opt");
    for (var i = 0; i < opts.length; i++) opts[i].classList.toggle("selected", opts[i].getAttribute("data-lang") === LANG);
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
  setLang(readLang(), false);
  var btn = document.getElementById("lang-btn"), menu = document.getElementById("lang-menu");
  function closeMenu() { if (menu) menu.classList.add("hidden"); if (btn) btn.setAttribute("aria-expanded", "false"); }
  if (btn) btn.addEventListener("click", function (e) {
    e.stopPropagation();
    var open = menu.classList.toggle("hidden") === false;
    btn.setAttribute("aria-expanded", open ? "true" : "false");
  });
  document.addEventListener("click", function (e) {
    var box = document.getElementById("lang-select");
    if (box && !box.contains(e.target)) closeMenu();
  });
  document.addEventListener("keydown", function (e) { if (e.key === "Escape") closeMenu(); });
  var opts = document.querySelectorAll(".lang-opt");
  for (var j = 0; j < opts.length; j++) {
    opts[j].addEventListener("click", function () { setLang(this.getAttribute("data-lang"), true); closeMenu(); });
  }
})();
