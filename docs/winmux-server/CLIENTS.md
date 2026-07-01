# winmux-server client SDKs

Generated + hand-written clients for the `winmux-server` API. Types come from the
server contract (`sdk-gen/`); the clients are thin and idiomatic. Both track the
server version.

- **REST** is described by OpenAPI (`/api/openapi.json`, generated from the huma
  handlers). Surface: `/healthz`, `/api/version`, `/api/v2/files/*`,
  `/api/v2/logs/*`. (The Insights metrics API is desktop-internal and not part
  of the SDK — see [PHASE-77-DESIGN §6](../PHASE-77-DESIGN.md).)
- **WebSocket frames** are described by AsyncAPI (`/api/asyncapi.json`) +
  `frames.schema.json`. One flat JSON object per frame, discriminated by `type`,
  `frame_version` negotiated in the `hello` frame.

## TypeScript (`sdk/typescript`, `@winmux/sdk`)

```ts
import { WinmuxClient, WorkspaceSocket } from "@winmux/sdk";

const client = new WinmuxClient({ baseUrl: "http://127.0.0.1:7879", token });
await client.version();                       // capability negotiation
await client.uploadFile("notes/a.txt", bytes);
const { bytes, truncated } = await client.readFile("notes/a.txt");
await client.listLogClients();

// Stream a workspace session (browser WebSocket, or `ws` in node):
new WorkspaceSocket({
  baseUrl: "http://127.0.0.1:7879", token,
  workspaceId: "ws_default", sessionId,
  makeSocket: (u) => new WebSocket(u),
  onFrame: (f) => { if (f.type === "hook_request") { /* … */ } },
});
```

`WorkspaceSocket` also exposes `sendUserInput`, `sendHookDecision(reqId, "allow"|"deny")`,
`interrupt`, `unsubscribe`. Frame types are the generated `WinmuxFrame` union —
narrow on `f.type`.

## Kotlin (`sdk/kotlin`, `dev.winmux.sdk`)

```kotlin
val client = WinmuxClient("http://127.0.0.1:7879", token)
client.version()
client.uploadFile("notes/a.txt", bytes)
val (bytes, truncated) = client.readFile("notes/a.txt")

WorkspaceSocket.subscribe(
    baseUrl = "http://127.0.0.1:7879", token = token,
    workspaceId = "ws_default", sessionId = sessionId,
    handler = object : FrameHandler {
        override fun onFrame(frame: WinmuxFrame) {
            when (frame) {
                is HookRequestFrame -> { /* … */ }
                else -> {}
            }
        }
    },
)
```

Frames deserialize into the sealed `WinmuxFrame` via `WinmuxJson.instance`
(`classDiscriminator = "type"`, unknown keys ignored for forward-compat).

## Regenerating

Types are generated — do not edit `*.gen.ts` or `Frames.kt`/`Models.kt` by hand.
After a server contract change: `cd sdk-gen && npm run gen`. CI runs
`node sdk-gen/ci-check.mjs` to fail on drift. See [`sdk-gen/README.md`](../../sdk-gen/README.md).
