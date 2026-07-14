// v0.4.4 (RTL Approach C) unit tests for detectDirection.
// v0.4.4-beta.3 (Approach C+) tests for classifyRow + detectRowDirections.
// Run: node --experimental-strip-types --test src/textDirection.test.ts
// (Excluded from the app tsconfig -- it is a node test, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  detectDirection,
  detectRowDirections,
  classifyRow,
} from "./textDirection.ts";

const LTR = "ltr";
const RTL = "rtl";

const cases: Array<[string, "ltr" | "rtl", string]> = [
  // Yossi's core five
  ["1. Hello world", LTR, "pure Latin list item"],
  ["1. שלום עולם", RTL, "pure Hebrew list item"],
  ["1. שלום world", RTL, "mixed -> RTL"],
  ["/opt/wa/.shared.env", LTR, "pure ASCII path"],
  ["שרת רץ על port 4200", RTL, "mixed Hebrew + latin/digits -> RTL"],
  // Edge cases from the brief
  ["", LTR, "empty -> LTR default"],
  ["12345", LTR, "digits only -> LTR"],
  ["-> <- up down", LTR, "arrows/symbols only -> LTR"],
  // More coverage
  ["   ", LTR, "whitespace only -> LTR"],
  ["!@#$%^&*()", LTR, "punctuation only -> LTR"],
  ["שלום", RTL, "single Hebrew word"],
  ["a", LTR, "single Latin char"],
  ["ש", RTL, "single Hebrew char"],
  ["مرحبا بالعالم", RTL, "pure Arabic"],
  ["run مرحبا now", RTL, "mixed Arabic + Latin -> RTL"],
  ["4200", LTR, "port number"],
  ["$ ls -la /home", LTR, "shell prompt + command"],
  ["הפורט 4200 פתוח", RTL, "Hebrew wrapping a number"],
  ["ERROR: קובץ לא נמצא", RTL, "Latin word then Hebrew -> RTL"],
  ["git commit -m 'תיקון'", RTL, "Latin command with Hebrew arg -> RTL"],
  ["100% done", LTR, "digits + symbol + Latin -> LTR"],
  ["שלום\tworld", RTL, "tab-separated mixed -> RTL"],
  ["cafe", LTR, "Latin -> LTR"],
];

for (const [input, expected, label] of cases) {
  test(`detectDirection(${JSON.stringify(input)}) -> ${expected} -- ${label}`, () => {
    assert.equal(detectDirection(input), expected);
  });
}

// -- Approach C+ (block-aware) tests -----------------------------------------

// classifyRow sanity checks
test("classifyRow: ASCII markdown separator is border", () => {
  assert.equal(classifyRow("|----|-----|"), "border");
  assert.equal(classifyRow("|------|-------|"), "border");
  assert.equal(classifyRow("+------+------+"), "border");
});

test("classifyRow: Unicode box-drawing line is border", () => {
  assert.equal(classifyRow("┌──────┐"), "border");
  assert.equal(classifyRow("│ code │"), "border");
  assert.equal(classifyRow("└──────┘"), "border");
});

test("classifyRow: fence opener/closer", () => {
  assert.equal(classifyRow("```"), "fence");
  assert.equal(classifyRow("```ts"), "fence");
  assert.equal(classifyRow("  ```javascript"), "fence");
});

test("classifyRow: table body row with letters is content, not border", () => {
  assert.equal(classifyRow("| Name | Value |"), "content");
  assert.equal(classifyRow("| שם | ערך |"), "content");
});

test("classifyRow: pure text is content", () => {
  assert.equal(classifyRow("שלום עולם"), "content");
  assert.equal(classifyRow("hello world"), "content");
});

// The 6 mandated block-aware cases from the brief.

test("Test 1: pure Hebrew table -> all rows RTL", () => {
  const rows = [
    "| שם | ערך |",
    "|----|-----|",
    "| א  | 1   |",
  ];
  assert.deepEqual(detectRowDirections(rows), ["rtl", "rtl", "rtl"]);
});

test("Test 2: pure ASCII table -> all rows LTR", () => {
  const rows = [
    "| Name | Value |",
    "|------|-------|",
    "| a    | 1     |",
  ];
  assert.deepEqual(detectRowDirections(rows), ["ltr", "ltr", "ltr"]);
});

test("Test 3: mixed table (Hebrew in one cell) -> all rows RTL", () => {
  const rows = [
    "| Name | ערך |",
    "|------|-----|",
    "| a    | 1   |",
  ];
  assert.deepEqual(detectRowDirections(rows), ["rtl", "rtl", "rtl"]);
});

test("Test 4: Hebrew paragraph -> ASCII box -> Hebrew paragraph, all RTL (box inherits)", () => {
  const rows = [
    "שלום זה טקסט",
    "┌──────┐",
    "│ code │",
    "└──────┘",
    "שלום עולם",
  ];
  assert.deepEqual(detectRowDirections(rows), ["rtl", "rtl", "rtl", "rtl", "rtl"]);
});

test("Test 5: pure ASCII code fence -> all rows LTR", () => {
  const rows = [
    "```",
    "const x = 5;",
    "```",
  ];
  assert.deepEqual(detectRowDirections(rows), ["ltr", "ltr", "ltr"]);
});

test("Test 6: mixed code fence with Hebrew comment -> all rows RTL", () => {
  const rows = [
    "```",
    "// הערה בעברית",
    "const x = 5;",
    "```",
  ];
  assert.deepEqual(detectRowDirections(rows), ["rtl", "rtl", "rtl", "rtl"]);
});

// Extra sanity: ASCII box between two LTR paragraphs stays LTR.
test("bonus: ASCII box between LTR paragraphs -> all LTR", () => {
  const rows = [
    "some english prose",
    "┌──────┐",
    "│ code │",
    "└──────┘",
    "more english prose",
  ];
  assert.deepEqual(detectRowDirections(rows), ["ltr", "ltr", "ltr", "ltr", "ltr"]);
});

// Extra sanity: empty input.
test("bonus: empty rows array", () => {
  assert.deepEqual(detectRowDirections([]), []);
});

// Extra sanity: single Hebrew standalone line still works.
test("bonus: single standalone Hebrew line -> RTL", () => {
  assert.deepEqual(detectRowDirections(["שלום עולם"]), ["rtl"]);
});
