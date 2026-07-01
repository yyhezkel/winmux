// Package logs will serve per-client log storage (/api/v2/logs/*): list clients
// and read a client's tail (PHASE-77-DESIGN §4.2). Storage under
// ~/.winmux/server/logs/<client_id>.log, size-capped + age-pruned (reusing the
// Phase-75 janitor logic). Compile-only stub in Sprint 1; implemented in
// Sprint 2.
package logs

// TODO(S2): ListClients() + Read(clientID, tail) + a size/age janitor.
