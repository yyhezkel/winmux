package main

// Phase 69 — persistence for the mobile Claude-chat subsystem. Kept in its
// own SQLite file (chat.db) so the metrics retention sweep and the chat
// tables never contend. Pure-Go sqlite (modernc), same as the metrics store.

import (
	"database/sql"
	"log"
	"time"

	_ "modernc.org/sqlite"
)

// replayCap — max buffered events kept per session for WS-reconnect replay.
// Drop-oldest beyond this (a logged marker is emitted, never the content).
const replayCap = 500

type ChatStore struct {
	db *sql.DB
}

// DeviceRow is a registered mobile device (69.D). The bearer token is stored
// only as a sha256 hash (Rule #2 — no plaintext secrets at rest).
type DeviceRow struct {
	ID        string
	TokenHash string
	Label     string
	CreatedAt int64
	RevokedAt int64 // 0 = active
}

// SessionRow is the durable record of a Claude chat session.
type SessionRow struct {
	ID              string
	DeviceID        string
	ClaudeSessionID string
	Cwd             string
	Model           string
	Status          string
	Policy          string
	StartedAt       int64
	LastActivityAt  int64
	MessageCount    int
}

func openChatStore(path string) (*ChatStore, error) {
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, err
	}
	db.SetMaxOpenConns(1)
	for _, p := range []string{
		"PRAGMA journal_mode=WAL",
		"PRAGMA synchronous=NORMAL",
		"PRAGMA busy_timeout=3000",
	} {
		if _, err := db.Exec(p); err != nil {
			return nil, err
		}
	}
	schema := `
	CREATE TABLE IF NOT EXISTS devices (
	  id TEXT PRIMARY KEY,
	  token_hash TEXT NOT NULL,
	  label TEXT,
	  created_at INTEGER,
	  revoked_at INTEGER DEFAULT 0
	);
	CREATE TABLE IF NOT EXISTS sessions (
	  id TEXT PRIMARY KEY,
	  device_id TEXT,
	  claude_session_id TEXT,
	  cwd TEXT,
	  model TEXT,
	  status TEXT,
	  policy TEXT,
	  started_at INTEGER,
	  last_activity_at INTEGER,
	  message_count INTEGER DEFAULT 0
	);
	CREATE INDEX IF NOT EXISTS idx_sessions_device ON sessions(device_id, status);
	CREATE TABLE IF NOT EXISTS replay (
	  session_id TEXT,
	  seq INTEGER,
	  ts INTEGER,
	  event TEXT,
	  PRIMARY KEY (session_id, seq)
	);
	`
	if _, err := db.Exec(schema); err != nil {
		return nil, err
	}
	return &ChatStore{db: db}, nil
}

func (s *ChatStore) Close() {
	if s.db != nil {
		_ = s.db.Close()
	}
}

// ─── sessions ────────────────────────────────────────────────────────────

func (s *ChatStore) insertSession(r *SessionRow) error {
	_, err := s.db.Exec(
		`INSERT INTO sessions
		   (id, device_id, claude_session_id, cwd, model, status, policy,
		    started_at, last_activity_at, message_count)
		 VALUES (?,?,?,?,?,?,?,?,?,?)`,
		r.ID, r.DeviceID, r.ClaudeSessionID, r.Cwd, r.Model, r.Status, r.Policy,
		r.StartedAt, r.LastActivityAt, r.MessageCount)
	return err
}

func (s *ChatStore) updateSessionStatus(id, status string) {
	_, err := s.db.Exec(
		`UPDATE sessions SET status=?, last_activity_at=? WHERE id=?`,
		status, time.Now().Unix(), id)
	if err != nil {
		log.Printf("chat: update status %s: %v", id, err)
	}
}

func (s *ChatStore) setClaudeSessionID(id, claudeID string) {
	_, _ = s.db.Exec(`UPDATE sessions SET claude_session_id=? WHERE id=?`, claudeID, id)
}

func (s *ChatStore) bumpActivity(id string, msgDelta int) {
	_, _ = s.db.Exec(
		`UPDATE sessions SET last_activity_at=?, message_count=message_count+? WHERE id=?`,
		time.Now().Unix(), msgDelta, id)
}

func (s *ChatStore) getSession(id string) (*SessionRow, error) {
	r := &SessionRow{}
	err := s.db.QueryRow(
		`SELECT id, device_id, claude_session_id, cwd, model, status, policy,
		        started_at, last_activity_at, message_count
		   FROM sessions WHERE id=?`, id).
		Scan(&r.ID, &r.DeviceID, &r.ClaudeSessionID, &r.Cwd, &r.Model, &r.Status,
			&r.Policy, &r.StartedAt, &r.LastActivityAt, &r.MessageCount)
	if err != nil {
		return nil, err
	}
	return r, nil
}

func (s *ChatStore) listSessions() ([]SessionRow, error) {
	rows, err := s.db.Query(
		`SELECT id, device_id, claude_session_id, cwd, model, status, policy,
		        started_at, last_activity_at, message_count
		   FROM sessions ORDER BY last_activity_at DESC`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []SessionRow
	for rows.Next() {
		var r SessionRow
		if err := rows.Scan(&r.ID, &r.DeviceID, &r.ClaudeSessionID, &r.Cwd, &r.Model,
			&r.Status, &r.Policy, &r.StartedAt, &r.LastActivityAt, &r.MessageCount); err == nil {
			out = append(out, r)
		}
	}
	return out, rows.Err()
}

// activeSessionCountForDevice counts non-terminal sessions, for the per-device
// rate limit (69.D).
func (s *ChatStore) activeSessionCountForDevice(deviceID string) int {
	var n int
	_ = s.db.QueryRow(
		`SELECT COUNT(*) FROM sessions
		   WHERE device_id=? AND status NOT IN ('stopped','killed','error')`,
		deviceID).Scan(&n)
	return n
}

func (s *ChatStore) deleteSession(id string) {
	_, _ = s.db.Exec(`DELETE FROM replay WHERE session_id=?`, id)
	_, _ = s.db.Exec(`DELETE FROM sessions WHERE id=?`, id)
}

// ─── replay buffer ───────────────────────────────────────────────────────

func (s *ChatStore) appendReplay(sessionID string, seq int64, event []byte) {
	_, err := s.db.Exec(
		`INSERT OR REPLACE INTO replay (session_id, seq, ts, event) VALUES (?,?,?,?)`,
		sessionID, seq, time.Now().Unix(), string(event))
	if err != nil {
		return
	}
	// Drop-oldest beyond the cap so a long session can't grow unbounded.
	_, _ = s.db.Exec(
		`DELETE FROM replay WHERE session_id=? AND seq <= ?`,
		sessionID, seq-replayCap)
}

func (s *ChatStore) getReplay(sessionID string) [][]byte {
	rows, err := s.db.Query(
		`SELECT event FROM replay WHERE session_id=? ORDER BY seq ASC`, sessionID)
	if err != nil {
		return nil
	}
	defer rows.Close()
	var out [][]byte
	for rows.Next() {
		var e string
		if err := rows.Scan(&e); err == nil {
			out = append(out, []byte(e))
		}
	}
	return out
}

// ─── devices (69.D) ──────────────────────────────────────────────────────

func (s *ChatStore) insertDevice(d *DeviceRow) error {
	_, err := s.db.Exec(
		`INSERT INTO devices (id, token_hash, label, created_at, revoked_at)
		 VALUES (?,?,?,?,0)`,
		d.ID, d.TokenHash, d.Label, d.CreatedAt)
	return err
}

// deviceByTokenHash returns the active (non-revoked) device for a token hash.
func (s *ChatStore) deviceByTokenHash(hash string) (*DeviceRow, bool) {
	d := &DeviceRow{}
	err := s.db.QueryRow(
		`SELECT id, token_hash, label, created_at, revoked_at
		   FROM devices WHERE token_hash=? AND revoked_at=0`, hash).
		Scan(&d.ID, &d.TokenHash, &d.Label, &d.CreatedAt, &d.RevokedAt)
	if err != nil {
		return nil, false
	}
	return d, true
}

func (s *ChatStore) revokeDevice(id string) {
	_, _ = s.db.Exec(`UPDATE devices SET revoked_at=? WHERE id=?`, time.Now().Unix(), id)
}

func (s *ChatStore) listDevices() ([]DeviceRow, error) {
	rows, err := s.db.Query(
		`SELECT id, token_hash, label, created_at, revoked_at FROM devices ORDER BY created_at DESC`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []DeviceRow
	for rows.Next() {
		var d DeviceRow
		if err := rows.Scan(&d.ID, &d.TokenHash, &d.Label, &d.CreatedAt, &d.RevokedAt); err == nil {
			out = append(out, d)
		}
	}
	return out, rows.Err()
}
