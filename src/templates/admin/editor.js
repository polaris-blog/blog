/* Polaris admin — lightweight Markdown editor for textarea.content-editor.
   Zero dependencies: a formatting toolbar that edits the textarea selection
   in place, plus a server-rendered preview (POST /admin/preview, the same
   safe pipeline the site uses). Code blocks are highlighted with the
   theme's highlight.js (/static/js/highlight.js), loaded by the page. */
(function () {
  "use strict";

  document.addEventListener("DOMContentLoaded", function () {
    var ta = document.querySelector("textarea.content-editor");
    if (!ta || ta.dataset.editorReady) return;
    ta.dataset.editorReady = "1";

    var csrfMeta = document.querySelector('meta[name="csrf"]');
    var csrf = csrfMeta ? csrfMeta.getAttribute("content") : "";

    var wrap = document.createElement("div");
    wrap.className = "editor-wrap";

    var toolbar = document.createElement("div");
    toolbar.className = "editor-toolbar";
    toolbar.setAttribute("role", "toolbar");
    toolbar.setAttribute("aria-label", "Markdown formatting");

    var tabs = document.createElement("div");
    tabs.className = "editor-tabs";
    var btnWrite = document.createElement("button");
    btnWrite.type = "button";
    btnWrite.className = "active";
    btnWrite.textContent = "Write";
    var btnPreview = document.createElement("button");
    btnPreview.type = "button";
    btnPreview.textContent = "Preview";
    tabs.appendChild(btnWrite);
    tabs.appendChild(btnPreview);

    var preview = document.createElement("div");
    preview.className = "editor-preview post-body";
    preview.setAttribute("data-highlight", "");
    preview.hidden = true;
    preview.setAttribute("aria-live", "polite");

    var ACTIONS = [
      { label: "B", title: "Bold", wrap: ["**", "**"], placeholder: "bold text", style: "bold" },
      { label: "I", title: "Italic", wrap: ["*", "*"], placeholder: "italic text", style: "italic" },
      { label: "H2", title: "Heading", line: "## " },
      { label: "H3", title: "Sub-heading", line: "### " },
      { label: "🔗", title: "Link", wrap: ["[", "](https://)"], placeholder: "link text", selectAll: true },
      { label: "</>", title: "Inline code", wrap: ["`", "`"], placeholder: "code" },
      { label: "◧", title: "Code block", block: "```\n\n```", placeholder: "code", caret: 4 },
      { label: "❝", title: "Quote", line: "> " },
      { label: "•", title: "Bullet list", line: "- " },
      { label: "1.", title: "Numbered list", line: "1. " },
    ];

    function insert(prefix, suffix, placeholder, selectAll, block, caret) {
      var start = ta.selectionStart;
      var end = ta.selectionEnd;
      var value = ta.value;
      var selected = value.slice(start, end);
      if (block !== undefined) {
        var before = value.slice(0, start);
        var after = value.slice(end);
        if (before && !before.endsWith("\n")) before += "\n";
        var payload = block.replace("\n\n", "\n" + (selected || placeholder) + "\n");
        var pos = before.length + caret;
        ta.value = before + payload + (after.startsWith("\n") ? "" : "\n") + after;
        ta.focus();
        ta.setSelectionRange(pos, pos + (selected || placeholder).length);
        ta.dispatchEvent(new Event("input", { bubbles: true }));
        return;
      }
      var mid = selected || placeholder;
      var next = value.slice(0, start) + prefix + mid + suffix + value.slice(end);
      ta.value = next;
      ta.focus();
      if (selected) {
        ta.setSelectionRange(start + prefix.length, start + prefix.length + mid.length);
      } else {
        var p = start + prefix.length;
        ta.setSelectionRange(p, p + placeholder.length);
      }
      if (selectAll) ta.setSelectionRange(start + prefix.length, start + prefix.length + (selected || placeholder).length);
      ta.dispatchEvent(new Event("input", { bubbles: true }));
    }

    ACTIONS.forEach(function (a) {
      var b = document.createElement("button");
      b.type = "button";
      b.className = "editor-btn";
      if (a.style) b.classList.add("editor-btn-" + a.style);
      b.textContent = a.label;
      b.title = a.title;
      b.setAttribute("aria-label", a.title);
      b.addEventListener("click", function () {
        if (preview.hidden === false) switchTo("write");
        if (a.line) {
          var start = ta.value.lastIndexOf("\n", ta.selectionStart - 1) + 1;
          var end = ta.selectionEnd;
          var lines = ta.value.slice(start, end).split("\n").map(function (line) { return a.line + line; }).join("\n");
          ta.setRangeText(lines, start, end, "select");
          ta.focus();
          ta.dispatchEvent(new Event("input", { bubbles: true }));
          return;
        }
        insert(a.wrap ? a.wrap[0] : "", a.wrap ? a.wrap[1] : "", a.placeholder || "", a.selectAll, a.block, a.caret);
      });
      toolbar.appendChild(b);
    });

    var mode = "write";
    var request = null;
    function switchTo(next) {
      if (next === mode) return;
      if (request) request.abort();
      mode = next;
      btnWrite.classList.toggle("active", next === "write");
      btnPreview.classList.toggle("active", next === "preview");
      btnWrite.setAttribute("aria-pressed", String(next === "write"));
      btnPreview.setAttribute("aria-pressed", String(next === "preview"));
      if (next === "write") {
        preview.hidden = true;
        ta.hidden = false;
        return;
      }
      ta.hidden = true;
      preview.hidden = false;
      preview.innerHTML = '<p class="muted">Rendering…</p>';
      request = new AbortController();
      var current = request;
      fetch("/admin/preview", {
        signal: current.signal,
        method: "POST",
        headers: {
          "Content-Type": "application/x-www-form-urlencoded;charset=UTF-8",
        },
        credentials: "same-origin",
        body: "csrf=" + encodeURIComponent(csrf) + "&content=" + encodeURIComponent(ta.value),
      })
        .then(function (r) {
          if (!r.ok) throw new Error(String(r.status));
          return r.json();
        })
        .then(function (data) {
          if (current !== request || current.signal.aborted) return;
          preview.innerHTML = data.html || "<p class='muted'>Nothing to preview.</p>";
          if (window.PolarisHL) window.PolarisHL.run(preview);
        })
        .catch(function () {
          if (current !== request || current.signal.aborted) return;
          preview.innerHTML = '<p class="form-error">Preview failed — please try again.</p>';
        });
    }
    btnWrite.addEventListener("click", function () { switchTo("write"); });
    btnPreview.addEventListener("click", function () { switchTo("preview"); });

    // Ctrl/Cmd+B / I shortcuts.
    ta.addEventListener("keydown", function (e) {
      if (!(e.metaKey || e.ctrlKey)) return;
      if (e.key === "b" || e.key === "B") {
        e.preventDefault();
        insert("**", "**", "bold text");
      } else if (e.key === "i" || e.key === "I") {
        e.preventDefault();
        insert("*", "*", "italic text");
      }
    });

    ta.parentNode.insertBefore(wrap, ta);
    wrap.appendChild(toolbar);
    wrap.appendChild(tabs);
    wrap.appendChild(ta);
    wrap.appendChild(preview);
  });
})();
