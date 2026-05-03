// Phase 8.F.4 — winmux-mcp: stdio MCP server bridging Claude Code / Cursor /
// any MCP-aware agent into the running winmux app's named-pipe RPC.
//
// Wire: agent ⇄ stdio JSON-RPC ⇄ winmux-mcp ⇄ \\.\pipe\winmux-<user> ⇄ app.
// Each `tools/call` opens a fresh pipe connection — server is stateless per
// call. The app must already be running; if the pipe is unreachable the
// tool result is an MCP error with that message.

use serde_json::{json, Value};
use std::io::{self};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::main]
async fn main() -> io::Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();

    // Newline-delimited JSON-RPC. Each request → one response line. Server
    // exits when stdin closes.
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let err = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("parse error: {e}") }
                });
                write_line(&mut stdout, &err).await?;
                continue;
            }
        };

        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let params = req.get("params").cloned().unwrap_or(json!({}));

        // Notifications (no id) — handle silently. MCP sends
        // `notifications/initialized` etc. after the handshake.
        if id.is_null() {
            // Nothing to send back. Continue.
            continue;
        }

        let resp = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {
                        "name": "winmux",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "capabilities": { "tools": {} }
                }
            }),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": tool_definitions() }
            }),
            "tools/call" => match handle_tool_call(params).await {
                Ok(v) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }]
                    }
                }),
                Err(e) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": e }],
                        "isError": true
                    }
                }),
            },
            other => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method not found: {other}") }
            }),
        };

        write_line(&mut stdout, &resp).await?;
    }
}

async fn write_line(stdout: &mut tokio::io::Stdout, v: &Value) -> io::Result<()> {
    let s = serde_json::to_string(v).unwrap_or_default();
    stdout.write_all(s.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await
}

// ─── Tool dispatch ──────────────────────────────────────────────────────────

async fn handle_tool_call(params: Value) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("tools/call: missing `name`")?
        .to_string();
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let (rpc_method, rpc_params) = match name.as_str() {
        // Discovery
        "list_workspaces" => ("list-workspaces", json!({})),
        "tree" => ("tree", args),
        // Browser navigation
        "browser_navigate" => ("pane.browser.navigate", args),
        "browser_url" => ("pane.browser.url", args),
        "browser_history" => ("pane.browser.history", args),
        "browser_go_back" => ("pane.browser.go-back", args),
        "browser_go_home" => ("pane.browser.go-home", args),
        // Browser automation (8.F.* iframe bridge)
        "browser_eval" => ("pane.browser.iframe.eval", args),
        "browser_click" => ("pane.browser.iframe.click", args),
        "browser_type" => ("pane.browser.iframe.type", args),
        "browser_find" => ("pane.browser.iframe.find", args),
        "browser_snapshot" => ("pane.browser.iframe.snapshot", args),
        "browser_wait_for" => ("pane.browser.iframe.wait-for", args),
        // Agent affordances
        "notify" => ("notify", args),
        "note_add" => ("note-add", args),
        other => return Err(format!("unknown tool: {other}")),
    };

    rpc_call(rpc_method, rpc_params).await
}

// ─── Named-pipe RPC client (Windows-only for v1) ────────────────────────────
// Mirrors the CLI's pipe path. Stateless per call: open pipe, write request,
// read one line back, close. The MCP server runs locally next to the user's
// agent; remote-agent use would target the CLI through the SSH tunnel.

#[cfg(windows)]
fn default_pipe_name() -> String {
    let user = std::env::var("USERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(whoami::username);
    format!(r"\\.\pipe\winmux-{}", user)
}

async fn rpc_call(method: &str, params: Value) -> Result<Value, String> {
    #[cfg(windows)]
    {
        let name = std::env::var("WINMUX_PIPE_PATH").unwrap_or_else(|_| default_pipe_name());
        let pipe = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&name)
            .map_err(|e| {
                format!(
                    "connect to {name}: {e} (is the winmux app running?)"
                )
            })?;
        rpc_via(pipe, method, params).await
    }
    #[cfg(not(windows))]
    {
        let _ = (method, params);
        Err("winmux-mcp: only Windows transport is implemented (named pipe)".into())
    }
}

async fn rpc_via<S>(stream: S, method: &str, params: Value) -> Result<Value, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (r, mut w) = tokio::io::split(stream);
    let mut reader = BufReader::new(r);
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let mut req_line = serde_json::to_string(&req).map_err(|e| format!("encode req: {e}"))?;
    req_line.push('\n');
    w.write_all(req_line.as_bytes())
        .await
        .map_err(|e| format!("write: {e}"))?;
    w.flush().await.ok();

    let mut buf = String::new();
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        reader.read_line(&mut buf),
    )
    .await
    .map_err(|_| "winmux RPC read timed out (60s)".to_string())?
    .map_err(|e| format!("read: {e}"))?;
    if r == 0 {
        return Err("winmux closed pipe before response".into());
    }
    let resp: Value = serde_json::from_str(buf.trim()).map_err(|e| format!("parse resp: {e}"))?;
    if let Some(err) = resp.get("error") {
        return Err(format!("winmux RPC error: {}", err));
    }
    Ok(resp.get("result").cloned().unwrap_or(Value::Null))
}

// ─── Tool definitions ──────────────────────────────────────────────────────

fn obj(props: &[(&str, Value)], required: &[&str]) -> Value {
    let mut p = serde_json::Map::new();
    for (k, v) in props {
        p.insert((*k).to_string(), v.clone());
    }
    json!({
        "type": "object",
        "properties": p,
        "required": required,
    })
}

fn s(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}
fn i(desc: &str) -> Value {
    json!({ "type": "integer", "description": desc })
}
fn b(desc: &str) -> Value {
    json!({ "type": "boolean", "description": desc })
}

fn tool_definitions() -> Vec<Value> {
    vec![
        // ── Discovery ────────────────────────────────────────────────────
        json!({
            "name": "list_workspaces",
            "description": "List winmux workspaces with their layouts. Returns the full WorkspacesFile JSON so the agent can find pane_ids.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "tree",
            "description": "Inspect a workspace's pane layout. Pass workspace_id to target a specific one; otherwise returns the active workspace's tree.",
            "inputSchema": obj(
                &[("workspace_id", s("workspace id (omit for the active workspace)"))],
                &[]
            )
        }),

        // ── Browser navigation (Phase 8.A/B/C) ───────────────────────────
        json!({
            "name": "browser_navigate",
            "description": "Navigate a browser pane to a URL. URLs targeting localhost on an SSH workspace are auto-forwarded via 8.B port forwarding.",
            "inputSchema": obj(
                &[
                    ("pane_id", s("browser pane id")),
                    ("url", s("absolute URL")),
                    ("raw", b("if true, skip any auto-resolution"))
                ],
                &["pane_id", "url"]
            )
        }),
        json!({
            "name": "browser_url",
            "description": "Read the persisted current URL of a browser pane.",
            "inputSchema": obj(&[("pane_id", s("browser pane id"))], &["pane_id"])
        }),
        json!({
            "name": "browser_history",
            "description": "Read the navigation history of a browser pane.",
            "inputSchema": obj(&[("pane_id", s("browser pane id"))], &["pane_id"])
        }),
        json!({
            "name": "browser_go_back",
            "description": "Pop the browser pane's history once.",
            "inputSchema": obj(&[("pane_id", s("browser pane id"))], &["pane_id"])
        }),
        json!({
            "name": "browser_go_home",
            "description": "Reset the browser pane to its home_url.",
            "inputSchema": obj(&[("pane_id", s("browser pane id"))], &["pane_id"])
        }),

        // ── Browser automation (Phase 8.F.* iframe bridge) ───────────────
        json!({
            "name": "browser_eval",
            "description": "Evaluate JavaScript inside the iframe via the winmux postMessage bridge. Works on cross-origin pages — the bridge runs in every frame at document creation time. Returns the JSON-serialized result of the expression.",
            "inputSchema": obj(
                &[
                    ("pane_id", s("browser pane id")),
                    ("expression", s("JS expression to evaluate")),
                    ("timeout_ms", i("timeout in ms (default 5000)"))
                ],
                &["pane_id", "expression"]
            )
        }),
        json!({
            "name": "browser_click",
            "description": "Click an element matched by a CSS selector inside the iframe. Pass button=\"right\" to issue a contextmenu event instead of a left click.",
            "inputSchema": obj(
                &[
                    ("pane_id", s("browser pane id")),
                    ("selector", s("CSS selector of the element to click")),
                    ("button", s("\"left\" (default) or \"right\"")),
                    ("timeout_ms", i("timeout in ms (default 5000)"))
                ],
                &["pane_id", "selector"]
            )
        }),
        json!({
            "name": "browser_type",
            "description": "Type text into an input/textarea matched by CSS selector. Sets focus, optionally clears, then appends and fires input + change events.",
            "inputSchema": obj(
                &[
                    ("pane_id", s("browser pane id")),
                    ("selector", s("CSS selector of the input/textarea")),
                    ("text", s("text to type")),
                    ("clear_first", b("clear the field before typing (default false)")),
                    ("timeout_ms", i("timeout in ms (default 5000)"))
                ],
                &["pane_id", "selector", "text"]
            )
        }),
        json!({
            "name": "browser_find",
            "description": "Semantic element search inside the iframe. AND-filters by role / text / label / placeholder / alt / title / testid / selector / visible_only. Returns matches with synthesized stable selectors usable for browser_click / browser_type. Text matching uses deepest-match (Playwright-style) so ancestor elements bubbled up by textContent don't pollute results.",
            "inputSchema": obj(
                &[
                    ("pane_id", s("browser pane id")),
                    ("role", s("ARIA role: button, link, textbox, checkbox, heading, listitem, ...")),
                    ("text", s("visible text content (case-insensitive contains)")),
                    ("label", s("aria-label or <label for>")),
                    ("placeholder", s("placeholder attribute")),
                    ("alt", s("img alt text")),
                    ("title", s("title attribute")),
                    ("testid", s("data-testid attribute (exact)")),
                    ("selector", s("optional CSS selector to narrow the search pool")),
                    ("visible_only", b("skip elements with display:none / visibility:hidden / zero area")),
                    ("first", b("return only the first match")),
                    ("limit", i("cap on number of matches")),
                    ("timeout_ms", i("timeout in ms (default 5000)"))
                ],
                &["pane_id"]
            )
        }),
        json!({
            "name": "browser_snapshot",
            "description": "Simplified accessibility-flavored DOM tree of the iframe. Implicit ARIA roles via tag table; collapses single-child wrappers without their own role+text into the child to keep the tree readable. Pass max_depth to cap depth.",
            "inputSchema": obj(
                &[
                    ("pane_id", s("browser pane id")),
                    ("max_depth", i("recursion cap (default 50)")),
                    ("text_only", b("strip non-essential attributes (level/url/src/alt/name)")),
                    ("timeout_ms", i("timeout in ms (default 10000)"))
                ],
                &["pane_id"]
            )
        }),
        json!({
            "name": "browser_wait_for",
            "description": "Poll the iframe until criteria are met or timeout. State semantics: visible (default — match exists AND visible), attached (match exists in DOM), hidden (match exists but not visible), detached (NO match — succeeds when element disappears). url_contains AND-s with element criteria.",
            "inputSchema": obj(
                &[
                    ("pane_id", s("browser pane id")),
                    ("selector", s("CSS selector to find")),
                    ("text", s("visible text (deepest-match)")),
                    ("role", s("ARIA role")),
                    ("label", s("accessible label")),
                    ("testid", s("data-testid")),
                    ("url_contains", s("substring of window.location.href")),
                    ("state", s("\"visible\" (default) | \"attached\" | \"hidden\" | \"detached\"")),
                    ("timeout_ms", i("timeout in ms (default 5000)"))
                ],
                &["pane_id"]
            )
        }),

        // ── Agent affordances ────────────────────────────────────────────
        json!({
            "name": "notify",
            "description": "Show a desktop toast in winmux. Use to ping the user when a long task finishes or a decision is needed.",
            "inputSchema": obj(
                &[
                    ("title", s("toast title")),
                    ("body", s("toast body")),
                    ("workspace_id", s("optional workspace id this notification belongs to"))
                ],
                &["title", "body"]
            )
        }),
        json!({
            "name": "note_add",
            "description": "Append a quick-capture note to winmux's notes. Tag is free-form; suggestions: idea, bug, todo. workspace_id / pane_id auto-attach context.",
            "inputSchema": obj(
                &[
                    ("text", s("note body")),
                    ("tag", s("optional tag (idea, bug, todo, …)")),
                    ("workspace_id", s("optional workspace id")),
                    ("pane_id", s("optional pane id"))
                ],
                &["text"]
            )
        }),
    ]
}
