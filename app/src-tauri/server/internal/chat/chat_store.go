package chat

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

// PairedDevice is a mobile device enrolled via the Phase 70 pairing flow.
// Supersedes 69.D's `devices`. Tokens are stored only as sha256 hashes
// (Rule #2). A device is created `pending` with a one-shot token (ots_hash +
// expires_at); redeeming it issues the long-term token (token_hash) and flips
// it to `active`.
type PairedDevice struct {
	ID         string
	Name       string
	TokenHash  string // long-term bearer (after redeem)
	OtsHash    string // one-shot (pending only; cleared on redeem)
	Scopes     string // JSON; "all" for now (decision #4)
	Status     string // pending | active | revoked
	CreatedAt  int64
	ExpiresAt  int64 // one-shot expiry (pending only)
	LastSeen   int64
	LastIP     string
}

// PendingEvent is a queued push envelope awaiting delivery to an offline device
// (native push §4). push_seq is the device's monotonic cursor.
type PendingEvent struct {
	PushSeq int64
	Ts      int64
	Event   string // JSON of the §4.4 event object
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

func OpenChatStore(path string) (*ChatStore, error) {
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
	CREATE TABLE IF NOT EXISTS paired_devices (
	  device_id TEXT PRIMARY KEY,
	  device_name TEXT,
	  token_hash TEXT,
	  ots_hash TEXT,
	  scopes TEXT,
	  status TEXT,
	  created_at INTEGER,
	  expires_at INTEGER,
	  last_seen INTEGER,
	  last_ip TEXT
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
	CREATE TABLE IF NOT EXISTS pending_events (
	  device_id TEXT,
	  push_seq INTEGER,
	  ts INTEGER,
	  event TEXT,
	  PRIMARY KEY (device_id, push_seq)
	);
	`
	if _, err := db.Exec(schema); err != nil {
		return nil, err
	}
	// Migrations (idempotent; a "duplicate column name" error means it's
	// already there). push_seq is the per-device monotonic push cursor for the
	// native push queue (never decreases, even after the queue drains).
	_, _ = db.Exec(`ALTER TABLE paired_devices ADD COLUMN push_seq INTEGER DEFAULT 0`)
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

func (s *ChatStore) listSessionsForDevice(deviceID string) ([]SessionRow, error) {
	rows, err := s.db.Query(
		`SELECT id, device_id, claude_session_id, cwd, model, status, policy,
		        started_at, last_activity_at, message_count
		   FROM sessions WHERE device_id=? ORDER BY last_activity_at DESC`, deviceID)
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

// ─── paired devices (Phase 70) ───────────────────────────────────────────

// issueDevice inserts a pending device holding a one-shot token hash + expiry.
func (s *ChatStore) issueDevice(d *PairedDevice) error {
	_, err := s.db.Exec(
		`INSERT INTO paired_devices
		   (device_id, device_name, token_hash, ots_hash, scopes, status,
		    created_at, expires_at, last_seen, last_ip)
		 VALUES (?,?,'',?,?,'pending',?,?,0,'')`,
		d.ID, d.Name, d.OtsHash, d.Scopes, d.CreatedAt, d.ExpiresAt)
	return err
}

// redeemDevice exchanges a valid one-shot (pending, unexpired) for a long-term
// token: stores the long-term hash, clears the one-shot, flips to active.
// Returns the device id, or ok=false if the one-shot is unknown/expired/used.
func (s *ChatStore) redeemDevice(otsHash, longTermHash string, now int64) (string, bool) {
	d := &PairedDevice{}
	err := s.db.QueryRow(
		`SELECT device_id, expires_at FROM paired_devices
		   WHERE ots_hash=? AND status='pending'`, otsHash).
		Scan(&d.ID, &d.ExpiresAt)
	if err != nil || d.ExpiresAt < now {
		return "", false
	}
	res, err := s.db.Exec(
		`UPDATE paired_devices
		    SET token_hash=?, ots_hash='', status='active', last_seen=?
		  WHERE device_id=? AND status='pending'`,
		longTermHash, now, d.ID)
	if err != nil {
		return "", false
	}
	if n, _ := res.RowsAffected(); n == 0 {
		return "", false // raced with another redeem
	}
	return d.ID, true
}

// deviceByTokenHash returns the active device for a long-term token hash.
func (s *ChatStore) deviceByTokenHash(hash string) (*PairedDevice, bool) {
	if hash == "" {
		return nil, false
	}
	d := &PairedDevice{}
	err := s.db.QueryRow(
		`SELECT device_id, device_name, scopes, status
		   FROM paired_devices WHERE token_hash=? AND status='active'`, hash).
		Scan(&d.ID, &d.Name, &d.Scopes, &d.Status)
	if err != nil {
		return nil, false
	}
	return d, true
}

// deviceByID returns any device (any status) by id — for owner scope reads.
func (s *ChatStore) deviceByID(id string) (*PairedDevice, bool) {
	d := &PairedDevice{}
	err := s.db.QueryRow(
		`SELECT device_id, device_name, scopes, status
		   FROM paired_devices WHERE device_id=?`, id).
		Scan(&d.ID, &d.Name, &d.Scopes, &d.Status)
	if err != nil {
		return nil, false
	}
	return d, true
}

// activeDeviceIDs lists every active paired device — the native-push fan-out
// targets (delivery is per-device: live socket now, or queued for reconnect).
func (s *ChatStore) activeDeviceIDs() []string {
	rows, err := s.db.Query(`SELECT device_id FROM paired_devices WHERE status='active'`)
	if err != nil {
		return nil
	}
	defer rows.Close()
	var out []string
	for rows.Next() {
		var id string
		if rows.Scan(&id) == nil {
			out = append(out, id)
		}
	}
	return out
}

// ─── native push queue (§4) ──────────────────────────────────────────────

// enqueuePending assigns the device its next monotonic push_seq, appends the
// event to its queue, caps the queue (drop-oldest beyond capN), and returns the
// seq. Atomic under the single-conn store.
func (s *ChatStore) enqueuePending(deviceID, eventJSON string, capN int) (int64, error) {
	tx, err := s.db.Begin()
	if err != nil {
		return 0, err
	}
	defer func() { _ = tx.Rollback() }()
	if _, err := tx.Exec(`UPDATE paired_devices SET push_seq = push_seq + 1 WHERE device_id=?`, deviceID); err != nil {
		return 0, err
	}
	var seq int64
	if err := tx.QueryRow(`SELECT push_seq FROM paired_devices WHERE device_id=?`, deviceID).Scan(&seq); err != nil {
		return 0, err
	}
	if _, err := tx.Exec(
		`INSERT INTO pending_events(device_id, push_seq, ts, event) VALUES(?,?,?,?)`,
		deviceID, seq, time.Now().Unix(), eventJSON); err != nil {
		return 0, err
	}
	if capN > 0 {
		_, _ = tx.Exec(`DELETE FROM pending_events WHERE device_id=? AND push_seq <= ?`, deviceID, seq-int64(capN))
	}
	if err := tx.Commit(); err != nil {
		return 0, err
	}
	return seq, nil
}

// pendingAfter returns a device's queued events with push_seq > cursor, oldest
// first — the reconnect replay.
func (s *ChatStore) pendingAfter(deviceID string, cursor int64) []PendingEvent {
	rows, err := s.db.Query(
		`SELECT push_seq, ts, event FROM pending_events
		   WHERE device_id=? AND push_seq > ? ORDER BY push_seq ASC`, deviceID, cursor)
	if err != nil {
		return nil
	}
	defer rows.Close()
	var out []PendingEvent
	for rows.Next() {
		var e PendingEvent
		if rows.Scan(&e.PushSeq, &e.Ts, &e.Event) == nil {
			out = append(out, e)
		}
	}
	return out
}

// ackPending drops a device's queued events with push_seq <= upto.
func (s *ChatStore) ackPending(deviceID string, upto int64) {
	_, _ = s.db.Exec(`DELETE FROM pending_events WHERE device_id=? AND push_seq <= ?`, deviceID, upto)
}

// prunePendingOlderThan drops queued events older than cutoff (TTL sweep).
func (s *ChatStore) prunePendingOlderThan(cutoff int64) {
	_, _ = s.db.Exec(`DELETE FROM pending_events WHERE ts < ?`, cutoff)
}

func (s *ChatStore) touchDevice(id, ip string) {
	_, _ = s.db.Exec(
		`UPDATE paired_devices SET last_seen=?, last_ip=? WHERE device_id=?`,
		time.Now().Unix(), ip, id)
}

func (s *ChatStore) revokeDevice(id string) {
	_, _ = s.db.Exec(`UPDATE paired_devices SET status='revoked' WHERE device_id=?`, id)
}

func (s *ChatStore) renameDevice(id, name string) {
	_, _ = s.db.Exec(`UPDATE paired_devices SET device_name=? WHERE device_id=?`, name, id)
}

func (s *ChatStore) listDevices() ([]PairedDevice, error) {
	rows, err := s.db.Query(
		`SELECT device_id, device_name, scopes, status, created_at, expires_at, last_seen, last_ip
		   FROM paired_devices ORDER BY created_at DESC`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []PairedDevice
	for rows.Next() {
		var d PairedDevice
		if err := rows.Scan(&d.ID, &d.Name, &d.Scopes, &d.Status,
			&d.CreatedAt, &d.ExpiresAt, &d.LastSeen, &d.LastIP); err == nil {
			out = append(out, d)
		}
	}
	return out, rows.Err()
}
