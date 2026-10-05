/* Polaris — tiny dependency-free syntax highlighter.
   Highlights `pre code` inside any `[data-highlight]` container.
   Usage: <div class="post-body" data-highlight> … <pre><code class="language-rust">
   Also exposes window.PolarisHL.run(root) for dynamically injected content
   (e.g. the admin markdown preview, pjax page swaps). Every fragment is
   HTML-escaped before a span is wrapped around it, so highlighting can never
   inject markup.
   Each block is also decorated with a language badge and a copy-to-clipboard
   button. Button labels come from the nearest [data-copy-label] ancestor
   (the <body> carries the i18n'd strings); they fall back to English. */
(function () {
  "use strict";

  var KEYWORDS = {
    rust: "as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self static struct super trait true type unsafe use where while",
    javascript: "async await break case catch class const continue debugger default delete do else export extends finally for from function get if import in instanceof let new of return set static super switch this throw try typeof var void while with yield true false null undefined",
    typescript: "abstract any as async await boolean break case catch class const constructor continue declare default do else enum export extends finally for from function get if implements import in infer instanceof interface is keyof let new never number of private protected public readonly return set static string super switch this throw try type typeof undefined unknown var void while yield true false null",
    python: "and as assert async await break class continue def del elif else except False finally for from global if import in is lambda None nonlocal not or pass raise return True try while with yield",
    sql: "select from where insert into values update set delete create table drop alter add join left right inner outer full on group by order having limit offset as and or not null primary key foreign references distinct union all exists between like in is asc desc",
    bash: "if then else elif fi for while until do done case esac function in select time coproc echo export local return read exit set unset shift source alias",
    go: "break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var nil true false",
    java: "abstract assert boolean break byte case catch char class const continue default do double else enum extends final finally float for goto if implements import instanceof int interface long native new package private protected public return short static strictfp super switch synchronized this throw throws transient try void volatile while true false null",
    c: "auto break case char const continue default do double else enum extern float for goto if inline int long register restrict return short signed sizeof static struct switch typedef union unsigned void volatile while true false NULL",
    toml: "true false",
    json: "true false null",
    ruby: "def end class module if elsif else unless while until for in do then begin rescue ensure raise return yield self nil true false and or not require puts attr_accessor",
    php: "abstract and array as break callable case catch class clone const continue declare default do echo else elseif empty enddeclare endfor endforeach endif endswitch endwhile extends final finally fn for foreach function global goto if implements include include_once instanceof insteadof interface isset list match namespace new or print private protected public readonly require require_once return static switch throw trait try unset use var while xor yield true false null",
  };
  var ALIAS = {
    js: "javascript", ts: "typescript", py: "python", sh: "bash", shell: "bash",
    zsh: "bash", yml: "yaml", rb: "ruby", golang: "go", "c++": "cpp",
  };
  // Line-comment prefixes per language family.
  var HASH_COMMENTS = ["python", "bash", "yaml", "toml", "ruby", "ini", "conf", "dockerfile"];
  var DASH_COMMENTS = ["sql"];

  function langOf(el) {
    var cls = el.className || "";
    var m = /language-([\w+#-]+)/.exec(cls);
    if (!m) return "";
    var l = m[1].toLowerCase();
    return ALIAS[l] || l;
  }

  function esc(s) {
    return s
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;");
  }

  function highlight(src, lang) {
    var isHash = HASH_COMMENTS.indexOf(lang) !== -1;
    var isDash = DASH_COMMENTS.indexOf(lang) !== -1;
    var kws = {};
    String(KEYWORDS[lang] || "true false null").split(" ").forEach(function (k) {
      kws[k.toLowerCase()] = true;
    });

    // One combined pass: block comments, strings, line comments, numbers,
    // identifiers. Everything between matches stays plain (escaped).
    var re = new RegExp(
      "(\\/\\*[\\s\\S]*?(?:\\*\\/|$))" +            // 1 block comment
      "|(<!--[\\s\\S]*?-->)" +                      // 2 html comment
      "|(\"(?:[^\"\\\\\\n]|\\\\.)*\"?|'(?:[^'\\\\\\n]|\\\\.)*'?|`(?:[^`\\\\]|\\\\.)*`?)" + // 3 string
      "|((?:\\/\\/|#|--)[^\\n]*)" +                 // 4 line comment (filtered below)
      "|\\b(\\d+(?:\\.\\d+)?)\\b" +                 // 5 number
      "|([A-Za-z_]\\w*)"                            // 6 identifier
    , "g");

    var out = "";
    var last = 0;
    var m;
    while ((m = re.exec(src)) !== null) {
      if (m.index > last) out += esc(src.slice(last, m.index));
      var s = m[0];
      if (m[1] || m[2]) {
        out += '<span class="hl-com">' + esc(s) + "</span>";
      } else if (m[3]) {
        out += '<span class="hl-str">' + esc(s) + "</span>";
      } else if (m[4]) {
        var prefix = s.slice(0, 2) === "//" ? "//" : s.slice(0, 1);
        var ok =
          (prefix === "//" && !isHash && !isDash) ||
          (prefix === "#" && isHash) ||
          (prefix === "-" && isDash) ||
          (prefix === "#" && lang === "json"); // JSON has no comments; skip
        if (ok) {
          out += '<span class="hl-com">' + esc(s) + "</span>";
        } else {
          out += esc(s);
        }
      } else if (m[5]) {
        out += '<span class="hl-num">' + esc(s) + "</span>";
      } else {
        var word = s;
        var next = src.charAt(re.lastIndex);
        if (kws[word.toLowerCase()]) {
          out += '<span class="hl-kw">' + esc(word) + "</span>";
        } else if (next === "(") {
          out += '<span class="hl-fn">' + esc(word) + "</span>";
        } else {
          out += esc(word);
        }
      }
      last = re.lastIndex;
    }
    if (last < src.length) out += esc(src.slice(last));
    return out;
  }

  /* Label lookup: the nearest ancestor carrying the data attribute wins, so
     the admin preview inside <body data-copy-label="…"> resolves too. */
  function label(el, attr, fallback) {
    var node = el;
    while (node) {
      if (node.getAttribute && node.getAttribute(attr)) {
        return node.getAttribute(attr);
      }
      node = node.parentNode;
    }
    return fallback;
  }

  function decorate(pre, lang) {
    if (pre.dataset.wrapped) return;
    pre.dataset.wrapped = "1";

    var wrap = document.createElement("div");
    wrap.className = "codeblock";
    pre.parentNode.insertBefore(wrap, pre);
    wrap.appendChild(pre);

    if (lang) {
      var badge = document.createElement("span");
      badge.className = "code-lang";
      badge.textContent = lang;
      badge.setAttribute("aria-hidden", "true");
      wrap.appendChild(badge);
    }

    var btn = document.createElement("button");
    btn.type = "button";
    btn.className = "code-copy";
    btn.textContent = label(pre, "data-copy-label", "Copy");
    btn.setAttribute("aria-label", btn.textContent);
    btn.addEventListener("click", function () {
      var text = pre.innerText || pre.textContent || "";
      var done = function () {
        btn.classList.add("copied");
        btn.textContent = label(pre, "data-copied-label", "Copied");
        setTimeout(function () {
          btn.classList.remove("copied");
          btn.textContent = label(pre, "data-copy-label", "Copy");
        }, 1600);
      };
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(done, function () {});
      } else {
        // Fallback for non-secure contexts.
        var box = document.createElement("textarea");
        box.value = text;
        box.setAttribute("readonly", "");
        box.style.position = "fixed";
        box.style.opacity = "0";
        document.body.appendChild(box);
        box.select();
        try { document.execCommand("copy"); done(); } catch (e) {}
        document.body.removeChild(box);
      }
    });
    wrap.appendChild(btn);
  }

  function run(root) {
    var scope = root || document;
    var blocks = scope.querySelectorAll("pre code");
    Array.prototype.forEach.call(blocks, function (el) {
      var pre = el.parentElement;
      var lang = langOf(el);
      if (!el.dataset.hlDone) {
        el.dataset.hlDone = "1";
        el.innerHTML = highlight(el.textContent || "", lang);
      }
      // Decoration is idempotent and independent of highlighting so pjax
      // swaps and admin previews can re-run safely on new fragments.
      if (pre) decorate(pre, lang);
    });
  }

  window.PolarisHL = { run: run };
  document.addEventListener("DOMContentLoaded", function () {
    var boxes = document.querySelectorAll("[data-highlight]");
    Array.prototype.forEach.call(boxes, run);
  });
})();
