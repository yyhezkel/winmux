// v0.4.4 (RTL Approach C) unit tests for detectDirection.
// Run: node --experimental-strip-types --test src/textDirection.test.ts
// (Excluded from the app tsconfig — it is a node test, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import { detectDirection } from "./textDirection.ts";

const LTR = "ltr";
const RTL = "rtl";

const cases: Array<[string, "ltr" | "rtl", string]> = [
  // Yossi's core five
  ["1. Hello world", LTR, "pure Latin list item"],
  ["1. שלום עולם", RTL, "pure Hebrew list item"],
  ["1. שלום world", RTL, "mixed → RTL"],
  ["/opt/wa/.shared.env", LTR, "pure ASCII path"],
  ["שרת רץ על port 4200", RTL, "mixed Hebrew + latin/digits → RTL"],
  // Edge cases from the brief
  ["", LTR, "empty → LTR default"],
  ["12345", LTR, "digits only → LTR"],
  ["→ ← ↑ ↓", LTR, "arrows/symbols only → LTR"],
  // More coverage
  ["   ", LTR, "whitespace only → LTR"],
  ["!@#$%^&*()", LTR, "punctuation only → LTR"],
  ["שלום", RTL, "single Hebrew word"],
  ["a", LTR, "single Latin char"],
  ["ש", RTL, "single Hebrew char"],
  ["مرحبا بالعالم", RTL, "pure Arabic"],
  ["run مرحبا now", RTL, "mixed Arabic + Latin → RTL"],
  ["4200", LTR, "port number"],
  ["$ ls -la /home", LTR, "shell prompt + command"],
  ["הפורט 4200 פתוח", RTL, "Hebrew wrapping a number"],
  ["ERROR: קובץ לא נמצא", RTL, "Latin word then Hebrew → RTL"],
  ["git commit -m 'תיקון'", RTL, "Latin command with Hebrew arg → RTL"],
  ["100% ✓ done", LTR, "digits + symbol + Latin → LTR"],
  ["שלום\tworld", RTL, "tab-separated mixed → RTL"],
  ["café", LTR, "Latin with accent → LTR"],
];

for (const [input, expected, label] of cases) {
  test(`detectDirection(${JSON.stringify(input)}) → ${expected} — ${label}`, () => {
    assert.equal(detectDirection(input), expected);
  });
}
