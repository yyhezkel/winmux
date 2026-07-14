// v0.4.4 (RTL Approach C): per-line text direction for the terminal.
//
// xterm.js's DOM renderer, given `dir="auto"` on a row, uses the Unicode
// "first strong directional character wins" rule. That mis-renders a mixed
// Hebrew+Latin line that HAPPENS to start with Latin -- e.g. a numbered list
// item "2. /opt/wa/.shared.env - הערה" got laid out LTR because the first
// strong char is Latin, even though the line is mostly Hebrew.
//
// Yossi's rule instead (per-line, Approach C):
//   - line contains ANY Hebrew/Arabic char  -> RTL   (mixed OR pure RTL)
//   - line is pure Latin                     -> LTR
//   - digits / symbols / whitespace only     -> LTR   (safe default)
//
// v0.4.4-beta.3 (Approach C+): pure per-line detection breaks TABLES and BOXES.
// Claude Code drawing a Hebrew table produces:
//     | שם | ערך |     <- Hebrew content, per-line rule -> RTL
//     |----|-----|      <- pure ASCII border, per-line rule -> LTR
//     | א  | 1   |     <- Hebrew content, per-line rule -> RTL
// The middle row's LTR direction visually mirrors the border columns
// relative to the RTL data rows, so the table looks column-flipped.
//
// Fix: group consecutive rows into "blocks" (tables, code fences, boxed text)
// and give the WHOLE block a single direction. Any Hebrew/Arabic in a block
// -> RTL; pure-ASCII blocks inherit from the previous content row's direction
// (default LTR). See `detectRowDirections`.
//
// Embedded Latin runs inside an RTL line (paths, "port 4200") still get their
// natural LTR ordering from the browser's BiDi algorithm once the row's
// paragraph direction is RTL -- which is exactly what we want.
//
// This module is intentionally dependency-free and pure so it can be unit
// tested under `node --test` without a bundler or the DOM.

const HEBREW = /[֐-׿]/; // Hebrew block
const ARABIC = /[؀-ۿݐ-ݿ]/; // Arabic + Arabic Supplement
const LATIN = /[A-Za-z]/;

export function detectDirection(text: string): "ltr" | "rtl" {
  if (HEBREW.test(text) || ARABIC.test(text)) return "rtl"; // mixed or pure RTL
  if (LATIN.test(text)) return "ltr"; // pure Latin
  return "ltr"; // digits / symbols / whitespace / empty -> safe LTR default
}

// -- Approach C+ (block-aware direction) -------------------------------------

export type BlockRole = "content" | "border" | "fence";

// Unicode box-drawing glyphs used by TUIs and Claude Code for tables/frames.
// Two-or-more of these in a row => it's a border row.
const BOX_CHARS = /[─-╿]/g;
// ASCII "markdown/ASCII table" border characters. Only treated as a border
// row when the row has NO letters -- so "|-------|" is border but
// "|-- Value |" (which contains letters) stays content.
const ASCII_BORDER_CHARS = /[\-|+=]/g;
// Any letter (Latin / Hebrew / Arabic). If present, a row is not a pure
// ASCII-border separator.
const HAS_LETTER = /[A-Za-z֐-׿؀-ۿݐ-ݿ]/;
// A code-fence opener/closer, possibly with a language tag: ``` or ```ts
const FENCE_START = /^\s*```/;
// A content row that "belongs" to an adjacent table block -- it has a column
// separator (Unicode U+2502 or ASCII |).
const HAS_TABLE_BAR = /[│|]/;

/**
 * Classify a single row by shape (not by direction). See detectRowDirections
 * for how these roles are grouped into blocks.
 */
export function classifyRow(text: string): BlockRole {
  if (FENCE_START.test(text)) return "fence";
  const boxCount = (text.match(BOX_CHARS) || []).length;
  if (boxCount >= 2) return "border";
  // ASCII "markdown/table" separator row: no letters, plenty of |/-/+/=.
  if (!HAS_LETTER.test(text)) {
    const asciiBorderCount = (text.match(ASCII_BORDER_CHARS) || []).length;
    if (asciiBorderCount >= 3) return "border";
  }
  return "content";
}

/**
 * Approach C+: compute a direction PER ROW using block-aware grouping.
 *
 * Blocks:
 *   - Fence block: from a ```-opener row to the next ```-closer row (inclusive).
 *   - Table/box block: a run of "border" rows, extended on either side to
 *     include adjacent "content" rows that carry a column separator.
 *   - Standalone rows: everything else, resolved per-row via detectDirection.
 *
 * Direction for a block:
 *   - Any Hebrew/Arabic char anywhere in the block   -> whole block RTL
 *   - Otherwise inherit direction from the nearest resolved row BEFORE the block
 *   - Otherwise LTR (safe default)
 *
 * The block heuristic keeps tables coherent: a table with a single Hebrew cell
 * renders RTL end-to-end (columns preserved from the user's perspective); a
 * pure-ASCII code fence stays LTR even if it happens to sit between Hebrew
 * paragraphs; a pure-ASCII BOX between Hebrew paragraphs flips RTL to match.
 */
export function detectRowDirections(rows: string[]): ("ltr" | "rtl")[] {
  const n = rows.length;
  if (n === 0) return [];
  const result: (("ltr" | "rtl") | null)[] = new Array(n).fill(null);

  const roles: BlockRole[] = rows.map(classifyRow);
  const hasRtl: boolean[] = rows.map((t) => HEBREW.test(t) || ARABIC.test(t));

  // 1. Identify block ranges.
  const blocks: Array<{ start: number; end: number }> = [];
  const inBlock = new Array<boolean>(n).fill(false);
  let i = 0;
  while (i < n) {
    if (inBlock[i]) { i++; continue; }
    if (roles[i] === "fence") {
      // Fence: opener .. next fence row (inclusive), or clamp to EOF.
      let j = i + 1;
      while (j < n && roles[j] !== "fence") j++;
      const end = j < n ? j : n - 1; // unterminated fence: run to end
      blocks.push({ start: i, end });
      for (let k = i; k <= end; k++) inBlock[k] = true;
      i = end + 1;
      continue;
    }
    if (roles[i] === "border") {
      // Table/box: extend backward through table-like content, and forward
      // through consecutive border rows or table-like content rows.
      let start = i;
      while (
        start > 0 &&
        !inBlock[start - 1] &&
        roles[start - 1] === "content" &&
        HAS_TABLE_BAR.test(rows[start - 1])
      ) {
        start--;
      }
      let end = i;
      while (end + 1 < n) {
        const nextRole = roles[end + 1];
        if (nextRole === "border") { end++; continue; }
        if (nextRole === "content" && HAS_TABLE_BAR.test(rows[end + 1])) {
          end++;
          continue;
        }
        break;
      }
      blocks.push({ start, end });
      for (let k = start; k <= end; k++) inBlock[k] = true;
      i = end + 1;
      continue;
    }
    i++;
  }

  // 2. Blocks that contain ANY Hebrew/Arabic -> whole block RTL. Blocks with
  //    zero RTL content are "unresolved" until step 4 (they need to see the
  //    surrounding standalone rows first).
  const unresolved: Array<{ start: number; end: number }> = [];
  for (const b of blocks) {
    let anyRtl = false;
    for (let k = b.start; k <= b.end; k++) {
      if (hasRtl[k]) { anyRtl = true; break; }
    }
    if (anyRtl) {
      for (let k = b.start; k <= b.end; k++) result[k] = "rtl";
    } else {
      unresolved.push(b);
    }
  }

  // 3. Standalone (non-block) rows: classic per-row detection.
  for (let k = 0; k < n; k++) {
    if (!inBlock[k] && result[k] === null) {
      result[k] = detectDirection(rows[k]);
    }
  }

  // 4. Pure-ASCII blocks: inherit from the nearest already-resolved row that
  //    precedes the block. Default LTR if none.
  for (const b of unresolved) {
    let dir: "ltr" | "rtl" = "ltr";
    for (let k = b.start - 1; k >= 0; k--) {
      if (result[k] !== null) {
        dir = result[k] as "ltr" | "rtl";
        break;
      }
    }
    for (let k = b.start; k <= b.end; k++) result[k] = dir;
  }

  return result as ("ltr" | "rtl")[];
}
