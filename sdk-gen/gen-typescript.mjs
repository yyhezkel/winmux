// gen-typescript.mjs — generate the TypeScript SDK's typed layer from the specs:
//   sdk/typescript/src/types.gen.ts  — REST paths/schemas (openapi-typescript)
//   sdk/typescript/src/frames.gen.ts — WS frame union (json-schema-to-typescript)
// The hand-written client (client.ts) + WS wrapper import these. Version-locked
// to the server via package.json below.
import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const specs = resolve(here, "specs");
const out = resolve(here, "..", "sdk", "typescript", "src");
mkdirSync(out, { recursive: true });
// Invoke the generators' JS entrypoints via node directly — the .bin/*.cmd
// shims aren't reliably execFile-able across Git Bash / cmd / Linux.
const node = process.execPath;
const oapiTs = resolve(here, "node_modules", "openapi-typescript", "bin", "cli.js");
const json2ts = resolve(here, "node_modules", "json-schema-to-typescript", "dist", "src", "cli.js");

const version = JSON.parse(readFileSync(resolve(specs, "openapi.json"), "utf8")).info.version;

// REST types.
execFileSync(node, [oapiTs, resolve(specs, "openapi.json"), "-o", resolve(out, "types.gen.ts")], { stdio: "inherit" });

// WS frame union.
execFileSync(
  node,
  [json2ts, "-i", resolve(specs, "frames.schema.json"), "-o", resolve(out, "frames.gen.ts"), "--additionalProperties", "false"],
  { stdio: "inherit" },
);

// Stamp the SDK version to match the server.
const pkgPath = resolve(here, "..", "sdk", "typescript", "package.json");
const pkg = JSON.parse(readFileSync(pkgPath, "utf8"));
pkg.version = version;
writeFileSync(pkgPath, JSON.stringify(pkg, null, 2) + "\n");
console.log(`TypeScript SDK types generated (v${version})`);
