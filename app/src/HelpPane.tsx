import { createEffect, createMemo, onMount } from "solid-js";
import { marked } from "marked";
import { currentLanguage, t, type Language } from "./i18n";

// Phase 33: in-app help pane. Renders one of a small set of bundled
// markdown documents (currently just ssh-key-setup) keyed by `topic`
// and the current UI language. Each fenced code block gets a Copy
// button at top-right so users can grab a command verbatim.
//
// Markdown files are imported via Vite's ?raw suffix so they ship in
// the bundle. Falls through to English if the active language doesn't
// have a localized version of the topic.

import sshEn from "./help/ssh-key-setup.en.md?raw";
import sshHe from "./help/ssh-key-setup.he.md?raw";
import sshAr from "./help/ssh-key-setup.ar.md?raw";
import sshRu from "./help/ssh-key-setup.ru.md?raw";

const DOCS: Record<string, Record<Language, string>> = {
  "ssh-key-setup": { en: sshEn, he: sshHe, ar: sshAr, ru: sshRu },
};

interface Props {
  topic: string;
}

export function HelpPane(p: Props) {
  const source = createMemo(() => {
    const doc = DOCS[p.topic];
    if (!doc) return `# ${p.topic}\n\n_No help document for this topic._`;
    return doc[currentLanguage()] ?? doc.en;
  });

  const html = createMemo(() => {
    // marked.parse returns string when no async options are set.
    return marked.parse(source(), { async: false }) as string;
  });

  let containerRef!: HTMLDivElement;

  onMount(() => {
    // Phase 33: wire Copy buttons. Walk every <pre> after render and
    // attach a positioned overlay button. We rebuild on language /
    // topic change via createEffect below.
    rewireCopyButtons();
  });

  // Re-wire whenever rendered HTML changes (language switch or topic
  // change). createEffect would also work; we attach via DOM since
  // marked renders to plain HTML.
  const rewireCopyButtons = () => {
    if (!containerRef) return;
    containerRef.innerHTML = html();
    const pres = containerRef.querySelectorAll("pre");
    pres.forEach((pre) => {
      const code = pre.querySelector("code");
      if (!code) return;
      const wrap = document.createElement("div");
      wrap.className = "help-pre-wrap";
      pre.parentNode?.insertBefore(wrap, pre);
      wrap.appendChild(pre);
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "help-copy-btn";
      btn.textContent = t("help.copyCode");
      btn.addEventListener("click", async () => {
        try {
          await navigator.clipboard.writeText(code.textContent ?? "");
          btn.textContent = t("help.copied");
          setTimeout(() => {
            btn.textContent = t("help.copyCode");
          }, 1500);
        } catch (e) {
          console.warn("clipboard write failed", e);
        }
      });
      wrap.appendChild(btn);
    });
  };

  // Re-render whenever language or topic changes. onMount handles
  // first paint; this effect catches subsequent updates.
  createEffect(() => {
    source(); // track
    queueMicrotask(() => rewireCopyButtons());
  });

  return (
    <div class="help-pane" dir="auto">
      <div ref={containerRef} class="help-pane-body" />
    </div>
  );
}
