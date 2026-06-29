package main

import (
	"database/sql"
	"log"
	"time"

	_ "modernc.org/sqlite"
)

// retentionDays — rolling window kept in SQLite.
const retentionDays = 7

type Store struct {
	db *sql.DB
}

func openStore(path string) (*Store, error) {
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, err
	}
	// One writer; keep the pool tiny (resource budget).
	db.SetMaxOpenConns(1)
	pragmas := []string{
		"PRAGMA journal_mode=WAL",
		"PRAGMA synchronous=NORMAL",
		"PRAGMA busy_timeout=3000",
	}
	for _, p := range pragmas {
		if _, err := db.Exec(p); err != nil {
			return nil, err
		}
	}
	schema := `
	CREATE TABLE IF NOT EXISTS samples (
	  ts INTEGER NOT NULL,
	  cpu_pct REAL, load1 REAL,
	  mem_used INTEGER, mem_total INTEGER, swap_used INTEGER,
	  net_rx_bps INTEGER, net_tx_bps INTEGER
	);
	CREATE INDEX IF NOT EXISTS idx_samples_ts ON samples(ts);
	CREATE TABLE IF NOT EXISTS disk_samples (
	  ts INTEGER, mount TEXT, used INTEGER, total INTEGER
	);
	CREATE INDEX IF NOT EXISTS idx_disk_ts ON disk_samples(ts);
	CREATE TABLE IF NOT EXISTS docker_samples (
	  ts INTEGER, cid TEXT, name TEXT, cpu_pct REAL, mem_used INTEGER, state TEXT
	);
	CREATE INDEX IF NOT EXISTS idx_docker_ts ON docker_samples(ts);
	`
	if _, err := db.Exec(schema); err != nil {
		return nil, err
	}
	return &Store{db: db}, nil
}

func (s *Store) Close() { _ = s.db.Close() }

// insert writes one sample tick (core metrics + per-disk + per-container)
// in a single transaction.
func (s *Store) insert(snap *Snapshot) {
	tx, err := s.db.Begin()
	if err != nil {
		log.Printf("store: begin: %v", err)
		return
	}
	defer func() { _ = tx.Commit() }()
	ts := snap.TS
	_, _ = tx.Exec(
		`INSERT INTO samples (ts,cpu_pct,load1,mem_used,mem_total,swap_used,net_rx_bps,net_tx_bps)
		 VALUES (?,?,?,?,?,?,?,?)`,
		ts, snap.CPU.Pct, load1(snap), snap.Mem.Used, snap.Mem.Total, snap.Mem.SwapUsed,
		snap.NetRxBps, snap.NetTxBps,
	)
	for _, d := range snap.Disks {
		_, _ = tx.Exec(`INSERT INTO disk_samples (ts,mount,used,total) VALUES (?,?,?,?)`,
			ts, d.Mount, d.Used, d.Total)
	}
	for _, c := range snap.Docker {
		_, _ = tx.Exec(
			`INSERT INTO docker_samples (ts,cid,name,cpu_pct,mem_used,state) VALUES (?,?,?,?,?,?)`,
			ts, c.ID, c.Name, c.CPUPct, c.MemUsed, c.State)
	}
}

func load1(snap *Snapshot) float64 {
	if len(snap.CPU.Load) > 0 {
		return snap.CPU.Load[0]
	}
	return 0
}

// HistoryPoint is one (t, value) for a metric series.
type HistoryPoint struct {
	T int64   `json:"t"`
	V float64 `json:"v"`
}

// history returns a metric series since a unix-seconds timestamp. `metric`
// is one of cpu / mem / swap / net_rx / net_tx / load.
func (s *Store) history(metric string, since int64, limit int) ([]HistoryPoint, error) {
	col := map[string]string{
		"cpu":    "cpu_pct",
		"load":   "load1",
		"mem":    "mem_used",
		"swap":   "swap_used",
		"net_rx": "net_rx_bps",
		"net_tx": "net_tx_bps",
	}[metric]
	if col == "" {
		col = "cpu_pct"
	}
	if limit <= 0 || limit > 5000 {
		limit = 2000
	}
	rows, err := s.db.Query(
		`SELECT ts, `+col+` FROM samples WHERE ts >= ? ORDER BY ts ASC LIMIT ?`,
		since, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []HistoryPoint
	for rows.Next() {
		var p HistoryPoint
		var v sql.NullFloat64
		if err := rows.Scan(&p.T, &v); err == nil {
			p.V = v.Float64
			out = append(out, p)
		}
	}
	return out, rows.Err()
}

// sweep deletes rows older than the retention window. Cheap; run on boot
// and hourly.
func (s *Store) sweep() {
	cut := time.Now().Add(-retentionDays * 24 * time.Hour).Unix()
	for _, t := range []string{"samples", "disk_samples", "docker_samples"} {
		if _, err := s.db.Exec(`DELETE FROM `+t+` WHERE ts < ?`, cut); err != nil {
			log.Printf("store: sweep %s: %v", t, err)
		}
	}
	_, _ = s.db.Exec(`PRAGMA wal_checkpoint(TRUNCATE)`)
}
