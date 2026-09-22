/* Multi LLM Web - gemeinsame Helfer, vor allen anderen Skripten geladen.
   Die eingebetteten Seiten (Index, Login, Dashboard) haben esc() bislang
   jeweils selbst kopiert; die Definition lebt jetzt nur noch hier. */
(function () {
  "use strict";

  // HTML-Escaping für alle per innerHTML eingefügten Werte.
  function esc(s) {
    return String(s == null ? "" : s).replace(/&/g, "&amp;").replace(/</g, "&lt;")
      .replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");
  }

  window.esc = esc;
  window.MLLM = Object.assign(window.MLLM || {}, { esc: esc });
})();
