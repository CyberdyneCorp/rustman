/* rustman docs: theme toggle and search. No dependencies, no network. */
(function () {
  "use strict";
  var THEME_KEY = "rustman-docs-theme";

  function prepare(entries) {
    return entries.map(function (e) {
      var hay = [e.t, e.m, e.p, e.k, e.d, (e.w || []).join(" ")].join(" ").toLowerCase();
      var words = (e.t + " " + e.p).toLowerCase().split(/[^a-z0-9]+/).filter(Boolean);
      return { e: e, hay: hay, words: words };
    });
  }

  function subsequence(term, text) {
    var at = 0;
    for (var i = 0; i < term.length; i++) {
      at = text.indexOf(term.charAt(i), at);
      if (at < 0) return false;
      at++;
    }
    return true;
  }

  /* A word of the name or path that starts like `term` and holds its
     letters in order: `lgn` finds `login`. */
  function fuzzy(term, words) {
    for (var i = 0; i < words.length; i++) {
      if (words[i].charAt(0) === term.charAt(0) && subsequence(term, words[i])) return true;
    }
    return false;
  }

  function termScore(term, item) {
    var at = item.hay.indexOf(term);
    if (at === 0 || (at > 0 && /[^a-z0-9]/.test(item.hay.charAt(at - 1)))) return 3;
    if (at > 0) return 2;
    if (term.length >= 3 && fuzzy(term, item.words)) return 1;
    return 0;
  }

  function search(prepared, query, limit) {
    var terms = query.toLowerCase().split(/\s+/).filter(Boolean);
    if (!terms.length) return [];
    var hits = [];
    for (var i = 0; i < prepared.length; i++) {
      var total = 0;
      for (var j = 0; j < terms.length; j++) {
        var s = termScore(terms[j], prepared[i]);
        if (!s) { total = 0; break; }
        total += s;
      }
      if (total) hits.push({ score: total, order: i, e: prepared[i].e });
    }
    hits.sort(function (a, b) { return b.score - a.score || a.order - b.order; });
    return hits.slice(0, limit || 20).map(function (h) { return h.e; });
  }

  function currentTheme() {
    var set = document.documentElement.getAttribute("data-theme");
    if (set) return set;
    var dark = window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
    return dark ? "dark" : "light";
  }

  function setupTheme() {
    var button = document.getElementById("theme-toggle");
    if (!button) return;
    button.hidden = false;
    button.addEventListener("click", function () {
      var next = currentTheme() === "dark" ? "light" : "dark";
      document.documentElement.setAttribute("data-theme", next);
      try { window.localStorage.setItem(THEME_KEY, next); } catch (e) { /* system preference stays */ }
    });
  }

  function render(list, results, root) {
    list.textContent = "";
    results.forEach(function (e) {
      var li = document.createElement("li");
      var a = document.createElement("a");
      a.href = root + e.u;
      a.textContent = (e.m ? e.m + " " : "") + e.t + (e.p ? " " + e.p : "");
      li.appendChild(a);
      if (e.x) {
        var badge = document.createElement("span");
        badge.className = "badge";
        badge.textContent = "deprecated";
        li.appendChild(document.createTextNode(" "));
        li.appendChild(badge);
      }
      list.appendChild(li);
    });
  }

  function setupSearch() {
    var box = document.getElementById("search");
    var index = window.RUSTMAN_SEARCH_INDEX;
    if (!box || !index) return;
    var input = document.getElementById("search-input");
    var list = document.getElementById("search-results");
    var root = document.documentElement.getAttribute("data-root") || "";
    var prepared = prepare(index.entries);
    box.hidden = false;
    var run = function () { render(list, search(prepared, input.value, 20), root); };
    input.addEventListener("input", run);
    var match = /[?&]q=([^&#]*)/.exec(window.location.search);
    if (match) {
      input.value = decodeURIComponent(match[1].replace(/\+/g, " "));
      run();
    }
  }

  if (typeof module !== "undefined" && module.exports) {
    module.exports = { prepare: prepare, search: search };
  }
  if (typeof document !== "undefined") {
    setupTheme();
    setupSearch();
  }
})();
