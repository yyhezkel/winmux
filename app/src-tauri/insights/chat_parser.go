package main

// Phase 69.A — raw passthrough. handleClaudeLine forwards each stdout line
// verbatim so the spawn→WS pipeline is provable before the stream-json
// parser (69.B) replaces this with normalized events.

func (s *Session) handleClaudeLine(line []byte) {
	s.mgr.store.bumpActivity(s.id, 0)
	s.emit(jsonEvent(map[string]any{"type": "raw", "line": string(line)}))
}
