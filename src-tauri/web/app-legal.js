/* Multi LLM Web-Verwaltung - Legal texts (imprint, privacy, terms).
   Renders the sections from the generated window.AppLegal, which comes from
   src/legal.ts - the same source the desktop app imports. */
(function () {
  "use strict";
  const A = window.App;

  function esc(s) {
    return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
  }

  function render() {
    const doc = window.AppLegal;
    const v = document.getElementById("view");
    if (!doc) {
      v.innerHTML = '<div class="card"><p class="sub">Legal texts unavailable.</p></div>';
      return;
    }
    const cards = doc.sections.map(function (s) {
      return '<div class="card">' +
        '<h3 style="margin:0 0 10px;font-size:16px;">' + esc(s.title) + "</h3>" +
        '<div class="legaltext">' + esc(s.body) + "</div>" +
      "</div>";
    }).join("");
    v.innerHTML = '<div class="toolbar"><div><h2>Legal</h2><p class="sub">' + esc(doc.intro) + "</p></div></div>" + cards;
  }

  window.UI_legal = render;
})();
