// @ts-ignore - bidi-js has no type defs
import bidiFactory from "bidi-js";

const bidi: any = bidiFactory();

const RTL_RE = /[֐-ࣿיִ-ﻼ]/;
const ANSI_RE = /\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~]|\][^\x07\x1B]*(?:\x07|\x1B\\))/g;

function reorderLine(line: string): string {
  if (!RTL_RE.test(line)) return line;
  const levels = bidi.getEmbeddingLevels(line, "ltr");
  const flips = bidi.getReorderSegments(line, levels);
  const mirrors = bidi.getMirroredCharactersMap(line, levels);

  const arr = line.split("");
  if (mirrors && mirrors.size) {
    mirrors.forEach((repl: string, idx: number) => {
      arr[idx] = repl;
    });
  }
  for (const [start, end] of flips) {
    const sub = arr.slice(start, end + 1).reverse();
    for (let i = 0; i < sub.length; i++) arr[start + i] = sub[i];
  }
  return arr.join("");
}

function reorderTextSegment(seg: string): string {
  if (!RTL_RE.test(seg)) return seg;
  const parts = seg.split(/(\r\n|\r|\n)/);
  return parts
    .map((p) => (/^(?:\r\n|\r|\n)$/.test(p) ? p : reorderLine(p)))
    .join("");
}

export function reorderRtlForDisplay(chunk: string): string {
  if (!RTL_RE.test(chunk)) return chunk;
  const out: string[] = [];
  let last = 0;
  for (const m of chunk.matchAll(ANSI_RE)) {
    const idx = m.index!;
    if (idx > last) out.push(reorderTextSegment(chunk.slice(last, idx)));
    out.push(m[0]);
    last = idx + m[0].length;
  }
  if (last < chunk.length) out.push(reorderTextSegment(chunk.slice(last)));
  return out.join("");
}
