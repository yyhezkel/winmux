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
      default:
        throw new Error("unknown winmux cmd: " + cmd);
    }
  }
})();
