// v0.4.4 (RTL Approach C): per-line text direction for the terminal.
//
// xterm.js's DOM renderer, given `dir="auto"` on a row, uses the Unicode
// "first strong directional character wins" rule. That mis-renders a mixed
// Hebrew+Latin line that HAPPENS to start with Latin — e.g. a numbered list
// item "2. /opt/wa/.shared.env — הערה" got laid out LTR because the first
// strong char is Latin, even though the line is mostly Hebrew.
//
// Yossi's rule instead:
//   - line contains ANY Hebrew/Arabic char  -> RTL   (mixed OR pure RTL)
//   - line is pure Latin                     -> LTR
//   - digits / symbols / whitespace only     -> LTR   (safe default)
//
// Embedded Latin runs inside an RTL line (paths, "port 4200") still get their
// natural LTR ordering from the browser's BiDi algorithm once the row's
// paragraph direction is RTL — which is exactly what we want.
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
