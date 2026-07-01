package main

// Phase 76: server-side process hygiene. Detects the two leaks Yossi hit —
// duplicate `winmux port-watch` processes (should be exactly one per
// workspace) and orphaned long-running `claude` sessions with no terminal —
// and can reap the safe ones on request. The desktop Monitor surfaces this in
// a "Cleanup" tab. Uses gopsutil (already a dep) so it stays cross-platform
// for `go test` on the dev box.

import (
	"log"
	"sort"
	"strings"
	"time"

	"github.com/shirou/gopsutil/v4/process"
)

// PortWatcher is one `winmux port-watch --workspace X` process. Duplicate is
// true for every instance of a workspace EXCEPT the newest (smallest etime) —
// those are the safe-to-kill leaks.
type PortWatcher struct {
	PID        int32   `json:"pid"`
	Workspace  string  `json:"workspace"`
	EtimeSec   int64   `json:"etime_sec"`
	CPUTimeSec float64 `json:"cpu_time_sec"`
	Duplicate  bool    `json:"duplicate"`
}

// OrphanSession is a `claude` process that LOOKS abandoned: no controlling
// terminal, a real --session-id/--resume, and alive beyond the threshold.
// Reported for the user to decide — never auto-killed (it could be a
// deliberate long-running background workflow).
type OrphanSession struct {
	PID       int32   `json:"pid"`
	SessionID string  `json:"session_id"`
	Resume    string  `json:"resume"`
	EtimeSec  int64   `json:"etime_sec"`
	CPUPct    float64 `json:"cpu_pct"`
}

type Hygiene struct {
	PortWatchers   []PortWatcher   `json:"port_watchers"`
	DuplicateCount int             `json:"duplicate_count"`
	OrphanSessions []OrphanSession `json:"orphan_sessions"`
}

// orphanEtimeThreshold: a claude with no tty older than this is flagged.
const orphanEtimeThreshold int64 = 24 * 3600

// argAfter returns the token following `flag` in an argv slice ("" if absent).
func argAfter(args []string, flag string) string {
	for i := 0; i+1 < len(args); i++ {
		if args[i] == flag {
			return args[i+1]
		}
	}
	return ""
}

// markDuplicates flags every port-watcher for a workspace except the newest
// (smallest etime) and returns how many were flagged. Pure — unit-tested.
func markDuplicates(ws []PortWatcher) int {
	byWs := map[string][]int{}
	for i := range ws {
		byWs[ws[i].Workspace] = append(byWs[ws[i].Workspace], i)
	}
	dups := 0
	for _, idxs := range byWs {
		if len(idxs) <= 1 {
			continue
		}
		sort.Slice(idxs, func(a, b int) bool {
			return ws[idxs[a]].EtimeSec < ws[idxs[b]].EtimeSec // newest first
		})
		for _, k := range idxs[1:] {
			ws[k].Duplicate = true
			dups++
		}
	}
	return dups
}

// collectHygiene walks the process table once and classifies port-watchers +
// orphan claude sessions.
func collectHygiene() Hygiene {
	procs, err := process.Processes()
	if err != nil {
		return Hygiene{}
	}
	nowUnix := time.Now().Unix()
	var watchers []PortWatcher
	var orphans []OrphanSession
	for _, p := range procs {
		args, err := p.CmdlineSlice()
		if err != nil || len(args) == 0 {
			continue
		}
		joined := strings.Join(args, " ")
		etime := int64(0)
		if ct, err := p.CreateTime(); err == nil && ct > 0 {
			etime = nowUnix - ct/1000
		}
		switch {
		case strings.Contains(joined, "winmux port-watch"):
			cpu := 0.0
			if t, err := p.Times(); err == nil {
				cpu = t.User + t.System
			}
			watchers = append(watchers, PortWatcher{
				PID:        p.Pid,
				Workspace:  argAfter(args, "--workspace"),
				EtimeSec:   etime,
				CPUTimeSec: round1(cpu),
			})
		case strings.Contains(joined, "claude"):
			sid := argAfter(args, "--session-id")
			resume := argAfter(args, "--resume")
			tty, _ := p.Terminal()
			if (sid != "" || resume != "") && tty == "" && etime > orphanEtimeThreshold {
				pct, _ := p.CPUPercent()
				orphans = append(orphans, OrphanSession{
					PID:       p.Pid,
					SessionID: sid,
					Resume:    resume,
					EtimeSec:  etime,
					CPUPct:    round1(pct),
				})
			}
		}
	}
	dupCount := markDuplicates(watchers)
	return Hygiene{PortWatchers: watchers, DuplicateCount: dupCount, OrphanSessions: orphans}
}

// autoReapDuplicates SIGTERMs duplicate port-watchers automatically — exactly
// one per workspace is ever correct, so this is safe to do unattended. It
// NEVER touches claude sessions (those are report-only; the user may keep a
// long-running loop alive and kills them manually from the Cleanup tab).
// Returns how many were reaped. Kills by PID, never `pkill -f`.
func autoReapDuplicates() int {
	h := collectHygiene()
	var pids []int32
	for _, w := range h.PortWatchers {
		if w.Duplicate {
			pids = append(pids, w.PID)
		}
	}
	if len(pids) == 0 {
		return 0
	}
	killed := killPids(pids)
	if len(killed) > 0 {
		log.Printf("hygiene: auto-reaped %d duplicate port-watcher(s): %v", len(killed), killed)
	}
	return len(killed)
}

// portWatchReaper auto-reaps duplicate port-watchers every 5 minutes so the
// leak self-heals server-side even for a workspace the desktop hasn't
// reconnected. Only port-watchers — claude sessions are never auto-killed.
func portWatchReaper(stop <-chan struct{}) {
	autoReapDuplicates() // once at boot
	t := time.NewTicker(5 * time.Minute)
	defer t.Stop()
	for {
		select {
		case <-stop:
			return
		case <-t.C:
			autoReapDuplicates()
		}
	}
}

// killPids terminates (SIGTERM) only PIDs that are currently classified as a
// duplicate port-watcher or an orphan session — a caller can't ask us to kill
// an arbitrary process. Returns the PIDs actually signalled.
func killPids(requested []int32) []int32 {
	h := collectHygiene()
	allowed := map[int32]bool{}
	for _, w := range h.PortWatchers {
		if w.Duplicate {
			allowed[w.PID] = true
		}
	}
	for _, o := range h.OrphanSessions {
		allowed[o.PID] = true
	}
	killed := []int32{}
	for _, pid := range requested {
		if !allowed[pid] {
			continue
		}
		if p, err := process.NewProcess(pid); err == nil {
			if p.Terminate() == nil {
				killed = append(killed, pid)
			}
		}
	}
	return killed
}
