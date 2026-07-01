package logs

// huma.go — the Logs API (/api/v2/logs/*) as typed huma operations (Phase 77
// S4): list clients + read a tail are plain JSON; stream is a Server-Sent
// Events tail. Reflected into the server-wide OpenAPI so the spec tracks the
// handlers. Wire contract matches the S2 stdlib handlers: same params, the same
// {lines,truncated} / {clients} JSON, and 400 on a bad id.

import (
	"bufio"
	"context"
	"errors"
	"io"
	"net/http"
	"os"
	"time"

	"github.com/danielgtaylor/huma/v2"
	"github.com/danielgtaylor/huma/v2/sse"
)

var secured = []map[string][]string{{"bearerAuth": {}}}

// ClientsBody mirrors the S2 {clients:[…]} response.
type ClientsBody struct {
	Clients []ClientInfo `json:"clients"`
}

// ReadBody mirrors the S2 {lines,truncated} response.
type ReadBody struct {
	Lines     []string `json:"lines"`
	Truncated bool     `json:"truncated"`
}

// lineEvent is the SSE payload for one appended log line (event: line).
type lineEvent struct {
	Line string `json:"line"`
}

// errEvent reports a stream that could not start (bad id); SSE conventionally
// carries errors as events since the 200 stream has already opened.
type errEvent struct {
	Error string `json:"error"`
}

// RegisterHuma mounts the Logs operations onto a shared huma API.
func (s *Service) RegisterHuma(api huma.API) {
	huma.Register(api, huma.Operation{
		OperationID: "logs-list", Method: http.MethodGet, Path: "/api/v2/logs/list",
		Summary: "List log clients (per-device + the server pseudo-client)",
		Tags:    []string{"logs"}, Security: secured,
	}, func(_ context.Context, _ *struct{}) (*struct{ Body ClientsBody }, error) {
		clients := s.store.ListClients()
		if clients == nil {
			clients = []ClientInfo{}
		}
		return &struct{ Body ClientsBody }{Body: ClientsBody{Clients: clients}}, nil
	})

	huma.Register(api, huma.Operation{
		OperationID: "logs-read", Method: http.MethodGet, Path: "/api/v2/logs/read",
		Summary: "Read the tail of a client's log",
		Tags:    []string{"logs"}, Security: secured,
	}, func(_ context.Context, in *struct {
		ClientID string `query:"client_id"`
		File     string `query:"file"`
		Tail     int    `query:"tail"`
	}) (*struct{ Body ReadBody }, error) {
		tail := in.Tail
		if tail == 0 {
			tail = 200
		}
		lines, err := s.store.Read(in.ClientID, in.File, tail)
		if err != nil {
			if errors.Is(err, ErrBadID) {
				return nil, huma.Error400BadRequest(err.Error())
			}
			return nil, huma.Error500InternalServerError(err.Error())
		}
		if lines == nil {
			lines = []string{}
		}
		return &struct{ Body ReadBody }{Body: ReadBody{Lines: lines, Truncated: len(lines) >= 5000}}, nil
	})

	sse.Register(api, huma.Operation{
		OperationID: "logs-stream", Method: http.MethodGet, Path: "/api/v2/logs/stream",
		Summary: "Server-Sent Events tail of a client's log (event: line)",
		Tags:    []string{"logs"}, Security: secured,
	}, map[string]any{
		"line":  lineEvent{},
		"error": errEvent{},
	}, func(ctx context.Context, in *struct {
		ClientID string `query:"client_id"`
		File     string `query:"file"`
	}, send sse.Sender,
	) {
		p, ok := s.store.Path(in.ClientID, in.File)
		if !ok {
			_ = send(sse.Message{Data: errEvent{Error: ErrBadID.Error()}})
			return
		}
		f, err := os.Open(p)
		if err != nil {
			_ = send(sse.Message{Data: errEvent{Error: "open log"}})
			return
		}
		defer f.Close()
		_, _ = f.Seek(0, io.SeekEnd) // new lines only

		reader := bufio.NewReader(f)
		ticker := time.NewTicker(500 * time.Millisecond)
		defer ticker.Stop()
		for {
			for {
				line, rerr := reader.ReadString('\n')
				if len(line) > 0 {
					_ = send(sse.Message{Data: lineEvent{Line: trimNewline(line)}})
				}
				if rerr != nil {
					break // EOF or partial line — wait for more
				}
			}
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
			}
		}
	})
}

func trimNewline(s string) string {
	if len(s) > 0 && s[len(s)-1] == '\n' {
		return s[:len(s)-1]
	}
	return s
}
