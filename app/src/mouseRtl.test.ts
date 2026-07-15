// v0.4.4-beta.4 (RTL mouse fix) unit tests for mouseRtl.
// Run: node --experimental-strip-types --test src/mouseRtl.test.ts
// (Excluded from the app tsconfig -- this is a node test, not browser code.)
import { test } from "node:test";
import assert from "node:assert/strict";
import { transformMouseX, findRow, type RowRect } from "./mouseRtl.ts";

const ltrRow: RowRect = { left: 100, right: 500, top: 0, bottom: 20, dir: "ltr" };
const rtlRow: RowRect = { left: 100, right: 500, top: 0, bottom: 20, dir: "rtl" };

// --- transformMouseX ------------------------------------------------------

test("transformMouseX: LTR row passes clientX through", () => {
  assert.equal(transformMouseX(150, ltrRow), 150);
  assert.equal(transformMouseX(300, ltrRow), 300);
  assert.equal(transformMouseX(499, ltrRow), 499);
});

test("transformMouseX: null row passes clientX through", () => {
  assert.equal(transformMouseX(150, null), 150);
  assert.equal(transformMouseX(0, null), 0);
});

test("transformMouseX: RTL row mirrors clientX around row midpoint", () => {
  // Row spans [100, 500]. Midpoint is 300. Mirror maps x -> 600 - x.
  assert.equal(transformMouseX(100, rtlRow), 500); // left edge -> right edge
  assert.equal(transformMouseX(500, rtlRow), 100); // right edge -> left edge
  assert.equal(transformMouseX(300, rtlRow), 300); // midpoint stays
  assert.equal(transformMouseX(200, rtlRow), 400); // 100px from left -> 100px from right
  assert.equal(transformMouseX(450, rtlRow), 150);
});

test("transformMouseX: RTL mirror is an involution (twice = identity)", () => {
  for (const x of [100, 150, 200, 275, 300, 350, 425, 500]) {
    const mirrored = transformMouseX(x, rtlRow);
    assert.equal(transformMouseX(mirrored, rtlRow), x);
  }
});

test("transformMouseX: RTL mirror handles fractional coords", () => {
  // Row [100, 500], sum = 600. Mirror maps x -> 600 - x.
  assert.equal(transformMouseX(123.5, rtlRow), 476.5);
});

// --- findRow --------------------------------------------------------------
//
// findRow reads getBoundingClientRect() off each row child. Under `node:test`
// we don't have jsdom, so we fake just enough of the Element / HTMLElement /
// HTMLCollection surface that findRow touches.

interface FakeRow {
  rect: { top: number; bottom: number; left: number; right: number };
  dir: string | null;
}

function makeHost(rows: FakeRow[]): Element {
  const children = rows.map((r) => ({
    getBoundingClientRect: () => ({
      top: r.rect.top,
      bottom: r.rect.bottom,
      left: r.rect.left,
      right: r.rect.right,
      // Height/width unused by findRow but included for realism.
      height: r.rect.bottom - r.rect.top,
      width: r.rect.right - r.rect.left,
      x: r.rect.left,
      y: r.rect.top,
      toJSON() {},
    }),
    getAttribute(name: string): string | null {
      return name === "dir" ? r.dir : null;
    },
  }));
  // findRow does `rowsHost.children[i]` and reads `.length`, so a plain
  // array-like object is enough.
  const host = {
    children: Object.assign(children, { length: children.length }),
  };
  return host as unknown as Element;
}

test("findRow: returns the row whose rect contains clientY", () => {
  const host = makeHost([
    { rect: { top: 0, bottom: 20, left: 0, right: 400 }, dir: "ltr" },
    { rect: { top: 20, bottom: 40, left: 0, right: 400 }, dir: "rtl" },
    { rect: { top: 40, bottom: 60, left: 0, right: 400 }, dir: "ltr" },
  ]);
  const r = findRow(host, 25);
  assert.ok(r);
  assert.equal(r.dir, "rtl");
  assert.equal(r.top, 20);
  assert.equal(r.bottom, 40);
});

test("findRow: returns null when clientY is outside every row", () => {
  const host = makeHost([
    { rect: { top: 0, bottom: 20, left: 0, right: 400 }, dir: "ltr" },
    { rect: { top: 20, bottom: 40, left: 0, right: 400 }, dir: "rtl" },
  ]);
  assert.equal(findRow(host, -5), null);
  assert.equal(findRow(host, 100), null);
});

test("findRow: no dir attr defaults to ltr", () => {
  const host = makeHost([
    { rect: { top: 0, bottom: 20, left: 0, right: 400 }, dir: null },
  ]);
  const r = findRow(host, 10);
  assert.ok(r);
  assert.equal(r.dir, "ltr");
});

test("findRow: dir='auto' or unknown counts as ltr (only 'rtl' triggers mirror)", () => {
  const host = makeHost([
    { rect: { top: 0, bottom: 20, left: 0, right: 400 }, dir: "auto" },
  ]);
  const r = findRow(host, 10);
  assert.ok(r);
  assert.equal(r.dir, "ltr");
});

// --- integration: findRow + transformMouseX ------------------------------

test("integration: RTL row found -> clientX mirrored; LTR row -> passthrough", () => {
  const host = makeHost([
    { rect: { top: 0, bottom: 20, left: 100, right: 500 }, dir: "ltr" },
    { rect: { top: 20, bottom: 40, left: 100, right: 500 }, dir: "rtl" },
  ]);
  // Pointer over the LTR row.
  const ltr = findRow(host, 10);
  assert.equal(transformMouseX(200, ltr), 200);
  // Pointer over the RTL row.
  const rtl = findRow(host, 30);
  assert.equal(transformMouseX(200, rtl), 400);
});

test("integration: pointer outside every row -> passthrough", () => {
  const host = makeHost([
    { rect: { top: 0, bottom: 20, left: 100, right: 500 }, dir: "rtl" },
  ]);
  const row = findRow(host, 999);
  assert.equal(row, null);
  assert.equal(transformMouseX(200, row), 200);
});
