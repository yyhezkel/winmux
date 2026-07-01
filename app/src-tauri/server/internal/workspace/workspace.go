// Package workspace will hold the cross-client shared state (PHASE-77-DESIGN
// §4.2 + Q3): active sessions + subscribers-per-session + pending requests,
// backing use-cases 8a (multi-client attach — several clients on the same
// Claude session see the same chat/tool progression/hook requests) and 8b
// (notification broadcast — a client answers approve/deny/message, it reaches
// Claude, and every other subscriber sees it). workspace_id is a
// server-authoritative UUID (Q5). Compile-only stub in Sprint 1; implemented in
// Sprint 3 atop core.EventBus.
package workspace

// TODO(S3): Manager (create → UUID), Subscribe/Publish via core.EventBus,
// pending-request registry shared with chat's hook flow.
