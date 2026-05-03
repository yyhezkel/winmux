// Phase 8.F.1 — winmux iframe bridge.
// Injected into EVERY frame (parent + iframes) via Tauri's
// `WebviewWindowBuilder::initialization_script_for_all_frames`. Runs at
// document-creation time, before any page script.
//
// Protocol: messages have shape `{ winmux: true, role: "command"|"response",
// request_id, ... }`. Backend → parent → iframe (command). Iframe → parent
// → backend (response).
//
// In an iframe (cross-origin to parent): receives `command`, executes against
// local DOM, posts `response` back to parent via window.parent.postMessage.
//
// In the parent: receives `response`, forwards to backend via the global
// `window.__TAURI__.core.invoke('pane_browser_iframe_response', ...)` (the
// global is exposed by `withGlobalTauri: true` in tauri.conf.json).
(function () {
  if (window.__winmux_bridge_installed) return;
  window.__winmux_bridge_installed = true;
  var isTop = window === window.top;

  window.addEventListener("message", async function (e) {
    var m = e.data;
    if (!m || m.winmux !== true || !m.request_id) return;

    // PARENT: forward iframe response to the backend.
    if (isTop && m.role === "response") {
      try {
        var t =
          window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke;
        if (!t) {
          console.error("winmux bridge: __TAURI__.core.invoke unavailable");
          return;
        }
        await t("pane_browser_iframe_response", {
          requestId: m.request_id,
          ok: !!m.ok,
          result: m.result == null ? null : m.result,
          error: m.error == null ? null : m.error,
        });
      } catch (err) {
        console.error("winmux bridge: forward iframe-response failed:", err);
      }
      return;
    }

    // IFRAME: execute the command against the local DOM and respond.
    if (!isTop && m.role === "command" && m.cmd) {
      var payload;
      try {
        var result = await runCommand(m.cmd, m.args || {});
        payload = {
          winmux: true,
          role: "response",
          request_id: m.request_id,
          ok: true,
          result: result,
        };
      } catch (err) {
        payload = {
          winmux: true,
          role: "response",
          request_id: m.request_id,
          ok: false,
          error: String((err && err.message) || err),
        };
      }
      try {
        window.parent.postMessage(payload, "*");
      } catch (err) {
        console.error("winmux bridge: post-response to parent failed:", err);
      }
    }
  });

  async function runCommand(cmd, args) {
    switch (cmd) {
      case "click": {
        var el = document.querySelector(args.selector);
        if (!el) throw new Error("no element matching " + args.selector);
        // Honor `button` arg: "right" issues a contextmenu event; default left click.
        if (args.button === "right") {
          var r = el.getBoundingClientRect();
          el.dispatchEvent(
            new MouseEvent("contextmenu", {
              bubbles: true,
              cancelable: true,
              view: window,
              button: 2,
              clientX: r.left + r.width / 2,
              clientY: r.top + r.height / 2,
            })
          );
        } else {
          el.click();
        }
        return { ok: true, tag: el.tagName };
      }
      case "type": {
        var el2 = document.querySelector(args.selector);
        if (!el2) throw new Error("no element matching " + args.selector);
        el2.focus();
        if (args.clear_first) {
          el2.value = "";
          el2.dispatchEvent(new Event("input", { bubbles: true }));
        }
        var current = el2.value == null ? "" : String(el2.value);
        var added = args.text == null ? "" : String(args.text);
        el2.value = current + added;
        el2.dispatchEvent(new Event("input", { bubbles: true }));
        el2.dispatchEvent(new Event("change", { bubbles: true }));
        return { ok: true, value: el2.value };
      }
      case "eval": {
        // Indirect eval — runs in the global scope of THIS frame.
        var v = (0, eval)(args.expression);
        if (v && typeof v.then === "function") v = await v;
        var serialized;
        try {
          serialized = JSON.parse(JSON.stringify(v));
        } catch (_) {
          serialized = String(v);
        }
        return { value: serialized };
      }
      case "find":
        return findElements(args || {});
      case "snapshot":
        return { tree: snapshot(args || {}) };
      default:
        throw new Error("unknown winmux cmd: " + cmd);
    }
  }

  // ─── Phase 8.F.2: semantic queries + accessibility snapshot ────────────

  function cssEscape(s) {
    if (window.CSS && CSS.escape) return CSS.escape(s);
    return String(s).replace(
      /([!"#$%&'()*+,.\/:;<=>?@[\\\]^`{|}~])/g,
      "\\$1"
    );
  }

  // Map of HTML tag → implicit ARIA role. Covers the common 80%; explicit
  // role attribute always wins.
  var IMPLICIT_ROLE = {
    a: "link",
    button: "button",
    nav: "navigation",
    main: "main",
    header: "banner",
    footer: "contentinfo",
    article: "article",
    section: "region",
    form: "form",
    table: "table",
    img: "img",
    h1: "heading",
    h2: "heading",
    h3: "heading",
    h4: "heading",
    h5: "heading",
    h6: "heading",
    ul: "list",
    ol: "list",
    li: "listitem",
    textarea: "textbox",
    select: "combobox",
    option: "option",
    dialog: "dialog",
    summary: "button",
  };

  function getRole(el) {
    var explicit = el.getAttribute && el.getAttribute("role");
    if (explicit) return explicit;
    var tag = el.tagName ? el.tagName.toLowerCase() : null;
    if (!tag) return null;
    if (tag === "input") {
      var t = (el.getAttribute("type") || "text").toLowerCase();
      if (t === "submit" || t === "button" || t === "reset") return "button";
      if (t === "checkbox") return "checkbox";
      if (t === "radio") return "radio";
      if (t === "range") return "slider";
      return "textbox";
    }
    return IMPLICIT_ROLE[tag] || null;
  }

  // Resolve the accessible label for a form control.
  function labelFor(el) {
    var aria = el.getAttribute && el.getAttribute("aria-label");
    if (aria) return aria;
    if (el.id) {
      try {
        var lbl = document.querySelector('label[for="' + cssEscape(el.id) + '"]');
        if (lbl) return (lbl.textContent || "").trim();
      } catch (_) {}
    }
    if (el.closest) {
      var p = el.closest("label");
      if (p) return (p.textContent || "").trim();
    }
    return null;
  }

  function getName(el) {
    return (
      labelFor(el) ||
      (el.textContent || "").trim() ||
      el.getAttribute("alt") ||
      el.getAttribute("title") ||
      ""
    );
  }

  function isVisible(el) {
    if (!el || !el.isConnected) return false;
    if (!el.getBoundingClientRect) return false;
    var r = el.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) return false;
    var cs = window.getComputedStyle ? window.getComputedStyle(el) : null;
    if (cs) {
      if (cs.display === "none") return false;
      if (cs.visibility === "hidden") return false;
      if (parseFloat(cs.opacity || "1") === 0) return false;
    }
    return true;
  }

  // Build a stable-ish CSS selector. Prefers id, then data-testid, then
  // tag.classes:nth-of-type(n) chained up to 4 levels.
  function synthesizeSelector(el) {
    if (el.id) return "#" + cssEscape(el.id);
    var testid = el.getAttribute && el.getAttribute("data-testid");
    if (testid) return '[data-testid="' + cssEscape(testid) + '"]';
    var parts = [];
    var cur = el;
    var depth = 0;
    while (
      cur &&
      cur.tagName &&
      cur.tagName.toLowerCase() !== "html" &&
      depth < 4
    ) {
      var part = cur.tagName.toLowerCase();
      if (cur.id) {
        parts.unshift("#" + cssEscape(cur.id));
        break;
      }
      if (cur.classList && cur.classList.length) {
        // Drop classes containing odd characters; they'd need escaping that
        // browsers handle inconsistently. Keep simple identifier-style ones.
        var ok = [];
        for (var i = 0; i < cur.classList.length && ok.length < 3; i++) {
          if (/^[A-Za-z_][\w-]*$/.test(cur.classList[i])) ok.push(cur.classList[i]);
        }
        if (ok.length) part += "." + ok.join(".");
      }
      var parent = cur.parentElement;
      if (parent) {
        var tag = cur.tagName;
        var sib = [];
        for (var j = 0; j < parent.children.length; j++) {
          if (parent.children[j].tagName === tag) sib.push(parent.children[j]);
        }
        if (sib.length > 1) {
          part += ":nth-of-type(" + (sib.indexOf(cur) + 1) + ")";
        }
      }
      parts.unshift(part);
      cur = cur.parentElement;
      depth++;
    }
    return parts.join(" > ");
  }

  function serializeMatch(el) {
    var name = getName(el);
    return {
      selector: synthesizeSelector(el),
      tag: el.tagName ? el.tagName.toLowerCase() : null,
      role: getRole(el),
      text: name ? name.slice(0, 200) : "",
      label: el.getAttribute ? el.getAttribute("aria-label") || labelFor(el) : null,
      visible: isVisible(el),
      href: el.getAttribute ? el.getAttribute("href") || undefined : undefined,
      src: el.getAttribute ? el.getAttribute("src") || undefined : undefined,
    };
  }

  function findElements(q) {
    var rootSelector = q.selector || "*";
    var pool;
    try {
      pool = Array.prototype.slice.call(document.querySelectorAll(rootSelector));
    } catch (e) {
      throw new Error("invalid selector " + rootSelector + ": " + e.message);
    }
    var out = [];
    var lower = function (s) {
      return s == null ? "" : String(s).toLowerCase();
    };
    var qText = q.text ? lower(q.text) : null;
    var qLabel = q.label ? lower(q.label) : null;
    var qPlaceholder = q.placeholder ? lower(q.placeholder) : null;
    var qAlt = q.alt ? lower(q.alt) : null;
    var qTitle = q.title ? lower(q.title) : null;
    var qRole = q.role || null;
    var qTestid = q.testid || null;
    for (var i = 0; i < pool.length; i++) {
      var el = pool[i];
      if (qRole && getRole(el) !== qRole) continue;
      if (qText && lower((el.textContent || "").trim()).indexOf(qText) === -1) continue;
      if (qLabel) {
        var lab = labelFor(el);
        if (!lab || lower(lab).indexOf(qLabel) === -1) continue;
      }
      if (qPlaceholder) {
        var ph = el.getAttribute && el.getAttribute("placeholder");
        if (!ph || lower(ph).indexOf(qPlaceholder) === -1) continue;
      }
      if (qAlt) {
        var alt = el.getAttribute && el.getAttribute("alt");
        if (!alt || lower(alt).indexOf(qAlt) === -1) continue;
      }
      if (qTitle) {
        var tt = el.getAttribute && el.getAttribute("title");
        if (!tt || lower(tt).indexOf(qTitle) === -1) continue;
      }
      if (qTestid) {
        var tid = el.getAttribute && el.getAttribute("data-testid");
        if (tid !== qTestid) continue;
      }
      if (q.visibleOnly && !isVisible(el)) continue;
      out.push(serializeMatch(el));
      if (q.first) break;
      if (q.limit && out.length >= q.limit) break;
    }
    return { matches: out };
  }

  // Walk the DOM into a simplified accessibility-flavored tree. Skips
  // purely presentational wrappers (div/span with no role and no direct
  // text). `maxDepth` caps recursion. `textOnly` strips attributes other
  // than role/name/level/url so the tree stays small.
  function snapshot(opts) {
    var maxDepth = opts.maxDepth || 50;
    var textOnly = !!opts.textOnly;

    function ownText(el) {
      // Concatenate direct text-node children only — children's text shows
      // up via recursion.
      var t = "";
      for (var i = 0; i < el.childNodes.length; i++) {
        var n = el.childNodes[i];
        if (n.nodeType === 3) t += n.textContent;
      }
      return t.trim();
    }

    function walk(el, depth) {
      if (depth >= maxDepth) return null;
      if (!el || !el.tagName) return null;
      var tag = el.tagName.toLowerCase();
      if (tag === "script" || tag === "style" || tag === "noscript") return null;
      var role = getRole(el);
      var text = ownText(el).slice(0, 200);
      var childTrees = [];
      for (var i = 0; i < el.children.length; i++) {
        var c = walk(el.children[i], depth + 1);
        if (c) childTrees.push(c);
      }
      // Skip purely presentational nodes that contribute nothing.
      var meaningful =
        role || text || childTrees.length > 0 || tag === "body" || tag === "html";
      if (!meaningful) return null;
      // Collapse pass-through wrappers: div/span with no role + exactly one
      // child trees and no own text → return the child directly.
      if (
        !role &&
        !text &&
        (tag === "div" || tag === "span") &&
        childTrees.length === 1
      ) {
        return childTrees[0];
      }
      var node = {};
      if (role) node.role = role;
      else node.role = tag; // fallback so consumers can see the tag
      if (text) node.text = text;
      if (!textOnly) {
        if (tag === "h1" || tag === "h2" || tag === "h3" ||
            tag === "h4" || tag === "h5" || tag === "h6") {
          node.level = parseInt(tag.slice(1), 10);
        }
        if (tag === "a") {
          var href = el.getAttribute("href");
          if (href) node.url = href;
        }
        if (tag === "img") {
          var src = el.getAttribute("src");
          if (src) node.src = src;
          var alt = el.getAttribute("alt");
          if (alt) node.alt = alt;
        }
        var aria = el.getAttribute("aria-label");
        if (aria && !text) node.name = aria;
      }
      if (childTrees.length) node.children = childTrees;
      return node;
    }

    return walk(document.body || document.documentElement, 0);
  }
})();
