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
    initCarousels();
    initReplies();
    initTypewriter();
    initPjax();
  });

  /* Typewriter headline: fills .type-target character by character with a
     blinking caret (opt-in via the theme's typewriter toggle; skipped for
     prefers-reduced-motion). Re-runs after pjax swaps on new headlines. */
  function initTypewriter() {
    var targets = document.querySelectorAll(
      '.hero-headline[data-typewriter="true"] .type-target'
    );
    var reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    Array.prototype.forEach.call(targets, function (el) {
      if (el.dataset.typed) return;
      el.dataset.typed = "1";
      var full = el.textContent;
      if (reduced || !full) {
        el.classList.add("type-ready", "type-done");
        return;
      }
      el.textContent = "";
      el.classList.add("type-ready");
      var i = 0;
      var timer = setInterval(function () {
        i += 1;
        el.textContent = full.slice(0, i);
        if (i >= full.length) {
          clearInterval(timer);
          el.classList.add("type-done");
        }
      }, 90);
    });
  }

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

  function initCarousels() {
    var carousels = document.querySelectorAll("[data-carousel]");
    Array.prototype.forEach.call(carousels, function (carousel) {
      if (carousel.getAttribute("data-ready") === "true") return;
      var slides = carousel.querySelectorAll("[data-carousel-slide]");
      var dots = carousel.querySelectorAll("[data-carousel-dot]");
      if (!slides.length) return;
      carousel.setAttribute("data-ready", "true");
      var index = 0;
      var timer = null;

      function show(next) {
        index = (next + slides.length) % slides.length;
        Array.prototype.forEach.call(slides, function (slide, i) {
          var active = i === index;
          slide.classList.toggle("is-active", active);
          if (active) slide.setAttribute("aria-current", "true");
          else slide.removeAttribute("aria-current");
        });
        Array.prototype.forEach.call(dots, function (dot, i) {
          var active = i === index;
          dot.classList.toggle("is-active", active);
          dot.setAttribute("aria-pressed", active ? "true" : "false");
        });
      }
      function stop() {
        if (timer !== null) { clearInterval(timer); timer = null; }
      }
      function start() {
        stop();
        if (
          slides.length < 2 ||
          carousel.getAttribute("data-autoplay") === "false" ||
          window.matchMedia("(prefers-reduced-motion: reduce)").matches
        ) return;
        var seconds = parseInt(carousel.getAttribute("data-interval"), 10) || 6;
        timer = setInterval(function () { show(index + 1); }, seconds * 1000);
      }
      var previous = carousel.querySelector("[data-carousel-prev]");
      var next = carousel.querySelector("[data-carousel-next]");
      if (previous) previous.addEventListener("click", function () { show(index - 1); start(); });
      if (next) next.addEventListener("click", function () { show(index + 1); start(); });
      Array.prototype.forEach.call(dots, function (dot) {
        dot.addEventListener("click", function () {
          show(parseInt(dot.getAttribute("data-carousel-dot"), 10) || 0);
          start();
        });
      });
      carousel.addEventListener("mouseenter", stop);
      carousel.addEventListener("mouseleave", start);
      carousel.addEventListener("focusin", stop);
      carousel.addEventListener("focusout", start);
      show(0);
      start();
    });
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
          initCarousels();
          initReplies();
          initTypewriter();
          // Re-highlight code blocks in the swapped-in page: the highlighter
          // only binds on DOMContentLoaded otherwise, so pjax-navigated posts
          // would render without syntax colors.
          if (window.PolarisHL) window.PolarisHL.run(main);
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
