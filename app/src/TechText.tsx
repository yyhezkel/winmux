import { For } from "solid-js";

// Phase 48-A (BiDi 33A): wrap technical tokens in `<code><bdi>…</bdi></code>`
// inside RTL contexts so mixed Hebrew/Arabic + Latin technical content
// like "open DEV branch" or "edit ~/.ssh/config" reads correctly. The
// terminal surface (xterm.js) is NOT touched here — that's the 33B
// PTY filter, deferred.
//
// Match heuristics (intentionally conservative — false negatives are
// fine, false positives are visually noisy):
//   • ALL_CAPS_IDENT  — DEV, MAIN, WINMUX_TUNNEL_TOKEN
//   • path-ish        — contains `/` or `\`, OR ends with `.ext`
//   • URLs            — http(s)://, ssh://, file://
//   • short SHA / hex — 7–40 hex chars
//   • dev branch words — main / master / dev / develop / staging / prod
//
// Any matched run becomes `<code class="tech-token"><bdi>…</bdi></code>`;
// the rest stays as plain text. `<bdi>` isolates the embedded LTR run
// from the surrounding RTL flow so neighboring punctuation doesn't
// reorder visually.

const TECH_PATTERN = new RegExp(
  [
    // Order matters — first match wins per index.
    "https?://[^\\s]+", // URLs
    "ssh://[^\\s]+",
    "file://[^\\s]+",
    "[A-Z][A-Z0-9_]{2,}", // ALL_CAPS_IDENT (≥3 chars to skip "OK" / "ID")
    "(?:[A-Za-z0-9_.~-]+[/\\\\])+[A-Za-z0-9_.~-]*", // path-like (slash or backslash)
    "[A-Za-z0-9_-]+\\.[A-Za-z]{1,8}\\b", // file.ext
    "\\b(?:main|master|dev|develop|staging|prod|production|hotfix|release)\\b",
    "\\b[0-9a-f]{7,40}\\b", // SHA-ish
  ].join("|"),
  "g",
);

interface Props {
  text: string;
}

interface Segment {
  text: string;
  tech: boolean;
}

function segment(input: string): Segment[] {
  if (!input) return [];
  const out: Segment[] = [];
  let last = 0;
  // Recreate the regex each call to avoid lastIndex state leaking between calls.
  const re = new RegExp(TECH_PATTERN.source, TECH_PATTERN.flags);
  let m: RegExpExecArray | null;
  while ((m = re.exec(input)) !== null) {
    if (m.index > last) {
      out.push({ text: input.slice(last, m.index), tech: false });
    }
    out.push({ text: m[0], tech: true });
    last = m.index + m[0].length;
    if (m[0].length === 0) re.lastIndex++; // safety against zero-width matches
  }
  if (last < input.length) {
    out.push({ text: input.slice(last), tech: false });
  }
  return out;
}

export function TechText(p: Props) {
  const segs = () => segment(p.text);
  return (
    <For each={segs()}>
      {(s) =>
        s.tech ? (
          <code class="tech-token">
            <bdi>{s.text}</bdi>
          </code>
        ) : (
          <>{s.text}</>
        )
      }
    </For>
  );
}
