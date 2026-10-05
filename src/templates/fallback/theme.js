/* Polaris theme script: dark/light toggle + pjax-style navigation.
   Page switches replace <main> in place and cross-fade opacity only
   (no progress bar). Loaded synchronously from <head>; CSP-safe. */
(function () {
  "use strict";
  var root = document.documentElement;

  // Apply the stored theme (or the system preference) before first paint.
  try {
    var stored = localStorage.getItem("polaris-theme");
    var dark = stored
      ? stored === "dark"
      : window.matchMedia("(prefers-color-scheme: dark)").matches;
    root.classList.add(dark ? "dark" : "light");
  } catch (e) {}

  document.addEventListener("DOMContentLoaded", function () {
    var btn = document.getElementById("theme-toggle");
    if (btn) {
      btn.addEventListener("click", function () {
        var next = root.classList.contains("dark") ? "light" : "dark";
        root.classList.remove("dark", "light");
        root.classList.add(next);
        try { localStorage.setItem("polaris-theme", next); } catch (e) {}
      });
    }
    initReplies();
    initPjax();
  });

  /* Nested comment replies: "Reply" fills the parent id, shows who is being
     replied to and scrolls to the form; "Cancel" resets it. */
  function initReplies() {
    var form = document.getElementById("comment-form");
    var parentInput = document.getElementById("comment-parent");
    if (!form || !parentInput || parentInput.getAttribute("data-ready") === "true") return;
    parentInput.setAttribute("data-ready", "true");
    var replying = document.getElementById("replying-to");
    var replyingName = document.getElementById("replying-to-name");
    var cancel = document.getElementById("cancel-reply");
    Array.prototype.forEach.call(document.querySelectorAll("[data-reply]"), function (link) {
      link.addEventListener("click", function (e) {
        e.preventDefault();
        parentInput.value = link.getAttribute("data-reply") || "";
        if (replyingName) replyingName.textContent = link.getAttribute("data-author") || "";
        if (replying) replying.hidden = false;
        form.scrollIntoView({ behavior: "smooth" });
        var box = form.querySelector("textarea");
        if (box) box.focus();
      });
    });
    if (cancel) {
      cancel.addEventListener("click", function () {
        parentInput.value = "";
        if (replying) replying.hidden = true;
      });
    }
  }

  function initPjax() {
    if (!window.fetch || !window.DOMParser || !window.history || !window.history.pushState) return;
    var main = document.querySelector("main");
    if (!main) return;

    var busy = false;

    function wait(ms) {
      return new Promise(function (res) { setTimeout(res, ms); });
    }

    function load(href, push) {
      if (busy) return;
      busy = true;
      main.style.transition = "opacity .15s ease";
      main.style.opacity = "0";
      var fetched = fetch(href, {
        headers: { "X-Requested-With": "polaris-pjax" },
        credentials: "same-origin",
      }).then(function (r) {
        if (!r.ok) throw new Error(String(r.status));
        return r.text();
      });
      // Hold the swap until the fade-out finished: an instantly resolved
      // local fetch would otherwise pull opacity back up mid-animation and
      // the fade-out would never be visible.
      Promise.all([fetched, wait(160)])
        .then(function (out) {
          var doc = new DOMParser().parseFromString(out[0], "text/html");
          var fresh = doc.querySelector("main");
          if (!fresh) throw new Error("no <main> in response");
          main.replaceChildren.apply(
            main,
            Array.prototype.slice.call(fresh.childNodes)
          );
          initReplies();
          if (doc.title) document.title = doc.title;
          if (push) history.pushState({}, "", href);
          window.scrollTo(0, 0);
          main.style.transition = "opacity .18s ease";
          requestAnimationFrame(function () {
            requestAnimationFrame(function () {
              main.style.opacity = "1";
              busy = false;
            });
          });
        })
        .catch(function () {
          // Any failure — network error, non-200, or a body without <main>
          // (plugin pages, feeds) — falls back to a normal navigation.
          // The busy lock and visibility MUST be released here, otherwise
          // one bad link freezes the whole page.
          busy = false;
          main.style.opacity = "";
          main.style.transition = "";
          location.href = href;
        });
    }

    function internal(url) {
      return (
        url.origin === location.origin &&
        url.pathname.indexOf("/admin") !== 0 &&
        url.pathname.indexOf("/static") !== 0 &&
        // Feeds (rss.xml, atom.xml, sitemap.xml) are documents, not HTML
        // pages — let the browser navigate to them natively.
        url.pathname.slice(-4) !== ".xml"
      );
    }

    document.addEventListener("click", function (e) {
      if (e.defaultPrevented || e.button !== 0) return;
      if (e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
      var a = e.target && e.target.closest ? e.target.closest("a") : null;
      if (!a || a.hasAttribute("download")) return;
      var target = a.getAttribute("target");
      if (target && target !== "_self") return;
      var href = a.getAttribute("href");
      if (
        !href ||
        href.charAt(0) === "#" ||
        href.indexOf("mailto:") === 0 ||
        href.indexOf("javascript:") === 0
      ) {
        return;
      }
      var url;
      try {
        url = new URL(a.href, location.href);
      } catch (err) {
        return;
      }
      if (!internal(url)) return;
      e.preventDefault();
      load(url.href, true);
    });

    // GET forms (search) navigate the same way.
    document.addEventListener("submit", function (e) {
      if (e.defaultPrevented) return;
      var form = e.target;
      if (!form || !form.tagName || form.tagName !== "FORM") return;
      var method = (form.getAttribute("method") || "get").toLowerCase();
      if (method !== "get") return;
      var url;
      try {
        url = new URL(form.getAttribute("action") || location.href, location.href);
      } catch (err) {
        return;
      }
      if (!internal(url)) return;
      e.preventDefault();
      url.search = new URLSearchParams(new FormData(form)).toString();
      load(url.href, true);
    });

    window.addEventListener("popstate", function () {
      load(location.href, false);
    });
  }
})();
