// contract.mjs — SDK ↔ real server contract test. Spawns winmux-server (go run)
// on a temp data dir + files root, then drives the whole client-SDK surface
// through the generated/typed SDK and asserts the wire contract holds: REST
// files round-trip + logs + version, and a WS 8a fan-out + hello frame.
//
// Run: node test/contract.mjs   (needs Go on PATH; installs `ws` for node WS)
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import assert from "node:assert/strict";

const imp = (p) => import(pathToFileURL(p).href);

// Import from source (no build step needed): ts is loaded via tsx? No — we ship
// compiled dist. Build first, then import dist.
const here = resolve(fileURLToPath(import.meta.url), "..");
const sdkRoot = resolve(here, "..");
const serverDir = resolve(sdkRoot, "..", "..", "app", "src-tauri", "server");

function sh(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { encoding: "utf8", ...opts });
  if (r.status !== 0) throw new Error(`${cmd} ${args.join(" ")} failed: ${r.stderr || r.stdout}`);
  return r;
}

// Build the SDK so we import the same dist consumers get.
sh("npm", ["run", "build"], { cwd: sdkRoot, shell: process.platform === "win32" });
const { WinmuxClient } = await imp(resolve(sdkRoot, "dist", "client.js"));
const { WorkspaceSocket } = await imp(resolve(sdkRoot, "dist", "ws.js"));
const WebSocket = (await imp(resolve(sdkRoot, "node_modules", "ws", "index.js"))).default;

const PORT = 7911;
const BASE = `http://127.0.0.1:${PORT}`;
const dir = mkdtempSync(join(tmpdir(), "winmux-contract-"));
const filesRoot = mkdtempSync(join(tmpdir(), "winmux-files-"));

// Build a real binary and spawn it directly — `go run` leaves an orphan child
// holding the port when killed, which breaks re-runs.
const exe = join(dir, process.platform === "win32" ? "winmux-server.exe" : "winmux-server");
sh("go", ["build", "-o", exe, "./cmd/winmux-server"], { cwd: serverDir });
const srv = spawn(exe, ["serve", "--port", String(PORT), "--dir", dir, "--files-root", filesRoot], {
  stdio: ["ignore", "inherit", "inherit"],
});

let failed = false;
try {
  // Wait for the token file + a live /healthz.
  const tokenPath = join(dir, "token");
  const deadline = Date.now() + 60_000;
  let token = "";
  for (;;) {
    if (existsSync(tokenPath)) token = readFileSync(tokenPath, "utf8").trim();
    if (token) {
      try {
        const r = await fetch(`${BASE}/healthz`);
        if (r.ok) break;
      } catch {
        /* not up yet */
      }
    }
    if (Date.now() > deadline) throw new Error("server did not become ready in 60s");
    await new Promise((r) => setTimeout(r, 400));
  }

  const client = new WinmuxClient({ baseUrl: BASE, token });

  // meta
  const v = await client.version();
  assert.equal(v.name, "winmux-server");
  assert.ok(Array.isArray(v.api_versions) && v.api_versions.includes(2), "api_versions includes 2");
  assert.ok(typeof v.frame_version === "number");

  // files round-trip
  const payload = new TextEncoder().encode("hello sdk");
  const up = await client.uploadFile("sub/note.txt", payload, "note.txt");
  assert.equal(up.size, 9, "upload size");
  assert.ok(up.sha256.length === 64, "sha256 present");

  const list = await client.listFiles("", 2);
  assert.ok(list.entries.some((e) => e.name.includes("note.txt")), "list sees uploaded file");

  const read = await client.readFile("sub/note.txt");
  assert.equal(new TextDecoder().decode(read.bytes), "hello sdk", "read matches");

  const dl = await client.downloadFile("sub/note.txt");
  assert.equal(new TextDecoder().decode(dl), "hello sdk", "download matches");

  const del = await client.deleteFile("sub/note.txt");
  assert.equal(del.ok, true, "delete ok");
  await assert.rejects(() => client.readFile("sub/note.txt"), /404/, "read after delete → 404");

  // logs
  const logs = await client.listLogClients();
  assert.ok(logs.clients.some((c) => c.client_id === "server"), "server pseudo-client present");

  // S6: pairing redeem + workspace list/create-session/get-session, and that a
  // redeemed device token authenticates the /api/v2 surface.
  const issue = await fetch(`${BASE}/api/pairing/issue`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
    body: JSON.stringify({ device_name: "contract-phone" }),
  });
  const { one_shot_token } = await issue.json();
  assert.ok(one_shot_token, "issued a one-shot pairing token");

  const redeemed = await client.pairing.redeem(one_shot_token);
  assert.ok(redeemed.device_id && redeemed.long_term_token, "redeem returns a device credential");
  assert.equal(redeemed.default_workspace_id, "ws_default", "redeem returns default_workspace_id");

  // the long-term device token must work on the bearer-gated workspace API
  const phone = new WinmuxClient({ baseUrl: BASE, token: redeemed.long_term_token });
  const wss = await phone.workspaces.list();
  assert.ok(wss.some((w) => w.id === "ws_default"), "device token lists workspaces");

  const created = await phone.workspaces.sessions("ws_default").create({ kind: "claude_chat" });
  assert.ok(created.session_id, "create session returns id");
  assert.equal(created.kind, "claude_chat", "session kind echoed");

  const detail = await phone.workspaces.getSession("ws_default", created.session_id);
  assert.equal(detail.workspace_id, "ws_default", "get-session workspace_id");
  assert.equal(detail.kind, "claude_chat", "get-session kind");

  // WS 8a: create a session under the default workspace, subscribe, receive hello.
  const sessRes = await fetch(`${BASE}/api/v2/workspace/ws_default/sessions`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
    body: JSON.stringify({ kind: "test" }),
  });
  const { session_id } = await sessRes.json();
  assert.ok(session_id, "session created");

  const frames = [];
  await new Promise((resolveP, rejectP) => {
    const to = setTimeout(() => rejectP(new Error("no hello frame in 10s")), 10_000);
    new WorkspaceSocket({
      baseUrl: BASE,
      token,
      workspaceId: "ws_default",
      sessionId: session_id,
      clientId: "sdk-test",
      makeSocket: (url) => new WebSocket(url),
      onFrame: (f) => {
        frames.push(f);
        if (f.type === "hello") {
          clearTimeout(to);
          resolveP();
        }
      },
      onError: (e) => {
        clearTimeout(to);
        rejectP(e);
      },
    });
  });
  const hello = frames.find((f) => f.type === "hello");
  assert.equal(hello.session_id, session_id, "hello carries session_id");
  assert.ok(typeof hello.frame_version === "number", "hello carries frame_version");

  console.log(
    `\n✓ contract OK — files/logs/version + pairing.redeem + device-token workspace list/create/get-session + WS hello (${frames.length} frame) verified`,
  );
} catch (e) {
  failed = true;
  console.error("\n✗ contract FAILED:", e?.message ?? e);
} finally {
  srv.kill("SIGKILL");
}
process.exit(failed ? 1 : 0);
