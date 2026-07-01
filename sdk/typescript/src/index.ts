// @winmux/sdk — TypeScript client for winmux-server (REST + WebSocket frames).
// Types are generated from the server's OpenAPI + frame schema (types.gen.ts,
// frames.gen.ts); the client + WS wrapper are hand-written. Do not edit the
// *.gen.ts files — regenerate via `sdk-gen` (npm run gen).
export * from "./client.js";
export * from "./ws.js";
export type * from "./frames.gen.js";
export type { paths, components, operations } from "./types.gen.js";
