/* Sets the theme before the first paint, so a light-mode visitor never
   sees a white flash on a dark site. Must run before the stylesheet is
   applied, which is why the tag carries blocking="render".
   Kept tiny on purpose: it is the first thing every page downloads. */
(function () {
  try {
    var p = JSON.parse(localStorage.getItem("ml-prefs"));
    if (p && p.theme === "light") {
      document.documentElement.setAttribute("data-theme", "light");
    }
  } catch (e) {
    /* No stored preference, or storage blocked. The default in the
       stylesheet is dark, so nothing has to happen here. */
  }
})();