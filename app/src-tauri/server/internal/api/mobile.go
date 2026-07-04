package api

// mobile.go — Phase 77 S6. The mobile-consumed endpoints that live in the chat
// (pairing) + workspace subsystems are surfaced here as typed huma ops so they
// land in the generated OpenAPI + the Kotlin/TS SDKs. They compose deps.Chat +
// deps.Workspace (raw duplicates of these paths were removed from those packages
// to avoid mux collisions). Auth: redeem is public (the one-shot token is the
// credential); the workspace ops require a bearer that tokenOK accepts (shared
// token or a paired device's long-term token).

import (
	"context"
	"errors"
	"net/http"
	"strings"

	"github.com/danielgtaylor/huma/v2"

	"winmux-server/internal/auth"
	"winmux-server/internal/workspace"
)

// ScopesBody carries a device's grant list (GET response + PUT request).
type ScopesBody struct {
	Scopes []string `json:"scopes"`
}

func scopeStrings(stored string) []string {
	sc := auth.ParseScopes(stored)
	out := make([]string, len(sc))
	for i, s := range sc {
		out[i] = string(s)
	}
	return out
}

var mobileSecured = []map[string][]string{{"bearerAuth": {}}}

// OKResponse is a minimal {"ok": true} body.
type OKResponse struct {
	OK bool `json:"ok"`
}

// HookDecisionRequest resolves a pending hook.
type HookDecisionRequest struct {
	Decision string `json:"decision"` // "allow" | "deny"
}

// HookResolved is the result of a hook resolution (won = this caller's decision
// was the winner-takes-all one that got broadcast).
type HookResolved struct {
	ReqID    string `json:"req_id"`
	Decision string `json:"decision"`
	Won      bool   `json:"won"`
}

// PairingRedeemResponse is the device credential handed to a freshly paired
// phone, plus the workspace it should connect to (saves a list round-trip).
type PairingRedeemResponse struct {
	DeviceID           string `json:"device_id"`
	LongTermToken      string `json:"long_term_token"`
	DefaultWorkspaceID string `json:"default_workspace_id"`
}

// Workspace is a workspace list item.
type Workspace struct {
	ID                 string `json:"id"`
	Name               string `json:"name"`
	CreatedAt          int64  `json:"created_at"`
	ActiveSessionCount int    `json:"active_session_count"`
}

// Session is a session's detail (get-session).
type Session struct {
	ID              string                     `json:"id"`
	Kind            string                     `json:"kind"`
	WorkspaceID     string                     `json:"workspace_id"`
	Subscribers     int                        `json:"subscribers"`
	PendingRequests []workspace.PendingRequest `json:"pending_requests"`
	EventCount      int64                      `json:"event_count"`
}

// CreateSessionRequest is the body for creating a session.
type CreateSessionRequest struct {
	Kind string `json:"kind"`
}

// SessionCreated is the response to a session create.
type SessionCreated struct {
	SessionID string `json:"session_id"`
	Kind      string `json:"kind"`
}

func (s *Server) registerMobileOps(api huma.API) {
	// POST /api/pairing/redeem — public; the one-shot token IS the credential.
	if s.deps.Chat != nil {
		huma.Register(api, huma.Operation{
			OperationID: "pairing-redeem", Method: http.MethodPost, Path: "/api/pairing/redeem",
			Summary: "Redeem a one-shot pairing token for a device credential",
			Tags:    []string{"pairing"},
		}, func(_ context.Context, in *struct {
			RealIP string `header:"X-Real-IP"`
			Body   struct {
				OneShotToken string `json:"one_shot_token"`
			}
		}) (*struct{ Body PairingRedeemResponse }, error) {
			id, longTerm, ok := s.deps.Chat.Redeem(in.Body.OneShotToken, in.RealIP)
			if !ok {
				return nil, huma.Error401Unauthorized("invalid or expired pairing token")
			}
			return &struct{ Body PairingRedeemResponse }{Body: PairingRedeemResponse{
				DeviceID: id, LongTermToken: longTerm, DefaultWorkspaceID: workspace.DefaultID,
			}}, nil
		})

		// GET /api/v2/devices/{id}/scopes — read a device's grants. A device may
		// read its OWN scopes (id = its device id, or the alias "me"); the
		// shared/admin token may read any device's.
		huma.Register(api, huma.Operation{
			OperationID: "device-get-scopes", Method: http.MethodGet,
			Path:    "/api/v2/devices/{id}/scopes",
			Summary: "Read a device's scope grants",
			Tags:    []string{"pairing"}, Security: mobileSecured,
		}, func(_ context.Context, in *struct {
			ID            string `path:"id"`
			Authorization string `header:"Authorization"`
		}) (*struct{ Body ScopesBody }, error) {
			caller, admin, ok := s.deps.Chat.ResolveToken(strings.TrimPrefix(in.Authorization, "Bearer "))
			if !ok {
				return nil, huma.Error401Unauthorized("unauthorized")
			}
			id := in.ID
			if id == "me" {
				id = caller
			}
			if !admin && id != caller {
				return nil, huma.Error403Forbidden("cannot read another device's scopes")
			}
			stored, found := s.deps.Chat.GetDeviceScopes(id)
			if !found {
				return nil, huma.Error404NotFound("device not found")
			}
			return &struct{ Body ScopesBody }{Body: ScopesBody{Scopes: scopeStrings(stored)}}, nil
		})

		// PUT /api/v2/devices/{id}/scopes — owner-only: set a device's grants.
		huma.Register(api, huma.Operation{
			OperationID: "device-set-scopes", Method: http.MethodPut,
			Path:    "/api/v2/devices/{id}/scopes",
			Summary: "Set a device's scope grants (owner only)",
			Tags:    []string{"pairing"}, Security: mobileSecured,
		}, func(_ context.Context, in *struct {
			ID            string `path:"id"`
			Authorization string `header:"Authorization"`
			Body          ScopesBody
		}) (*struct{ Body ScopesBody }, error) {
			_, admin, ok := s.deps.Chat.ResolveToken(strings.TrimPrefix(in.Authorization, "Bearer "))
			if !ok {
				return nil, huma.Error401Unauthorized("unauthorized")
			}
			if !admin {
				return nil, huma.Error403Forbidden("owner token required to set scopes")
			}
			stored := auth.NormalizeScopes(in.Body.Scopes)
			if !s.deps.Chat.SetDeviceScopes(in.ID, stored) {
				return nil, huma.Error404NotFound("device not found or not active")
			}
			return &struct{ Body ScopesBody }{Body: ScopesBody{Scopes: scopeStrings(stored)}}, nil
		})
	}

	if s.deps.Workspace == nil {
		return
	}
	mgr := s.deps.Workspace.Mgr()

	huma.Register(api, huma.Operation{
		OperationID: "workspace-list", Method: http.MethodGet, Path: "/api/v2/workspace/list",
		Summary: "List workspaces", Tags: []string{"workspace"}, Security: mobileSecured,
	}, func(context.Context, *struct{}) (*struct{ Body []Workspace }, error) {
		wss, err := mgr.ListWorkspaces()
		if err != nil {
			return nil, huma.Error500InternalServerError(err.Error())
		}
		out := make([]Workspace, 0, len(wss))
		for _, ws := range wss {
			sess, _ := mgr.ListSessions(ws.ID)
			out = append(out, Workspace{ID: ws.ID, Name: ws.Name, CreatedAt: ws.CreatedAt, ActiveSessionCount: len(sess)})
		}
		return &struct{ Body []Workspace }{Body: out}, nil
	})

	huma.Register(api, huma.Operation{
		OperationID: "workspace-create-session", Method: http.MethodPost, Path: "/api/v2/workspace/{id}/sessions",
		Summary: "Create a session in a workspace", Tags: []string{"workspace"}, Security: mobileSecured,
	}, func(_ context.Context, in *struct {
		ID   string `path:"id"`
		Body CreateSessionRequest
	}) (*struct{ Body SessionCreated }, error) {
		se, err := mgr.CreateSession(in.ID, in.Body.Kind)
		if err != nil {
			return nil, wsErr(err)
		}
		return &struct{ Body SessionCreated }{Body: SessionCreated{SessionID: se.ID, Kind: se.Kind}}, nil
	})

	huma.Register(api, huma.Operation{
		OperationID: "workspace-get-session", Method: http.MethodGet, Path: "/api/v2/workspace/{id}/session/{sid}",
		Summary: "Get a session's detail", Tags: []string{"workspace"}, Security: mobileSecured,
	}, func(_ context.Context, in *struct {
		ID  string `path:"id"`
		SID string `path:"sid"`
	}) (*struct{ Body Session }, error) {
		se, err := mgr.GetSession(in.SID)
		if err != nil {
			return nil, wsErr(err)
		}
		pending, _ := mgr.ListPending(se.ID)
		if pending == nil {
			pending = []workspace.PendingRequest{}
		}
		return &struct{ Body Session }{Body: Session{
			ID: se.ID, Kind: se.Kind, WorkspaceID: se.WorkspaceID,
			Subscribers: mgr.SubscriberCount(se.ID), PendingRequests: pending, EventCount: mgr.EventCount(se.ID),
		}}, nil
	})

	// PUT /api/v2/session/{sid}/hook/{req_id} — approve/deny a pending hook
	// over REST (the mobile alternative to the workspace-WS hook_decision
	// frame). Winner-takes-all: `won` reports whether this decision was the
	// first and got broadcast. resolved_by = the caller's device id.
	huma.Register(api, huma.Operation{
		OperationID: "session-resolve-hook", Method: http.MethodPut,
		Path:    "/api/v2/session/{sid}/hook/{req_id}",
		Summary: "Approve or deny a pending hook request",
		Tags:    []string{"workspace"}, Security: mobileSecured,
	}, func(_ context.Context, in *struct {
		SID           string `path:"sid"`
		ReqID         string `path:"req_id"`
		Authorization string `header:"Authorization"`
		Body          HookDecisionRequest
	}) (*struct{ Body HookResolved }, error) {
		decision := "deny"
		if in.Body.Decision == "allow" {
			decision = "allow"
		}
		clientID := "desktop" // shared/admin token
		if s.deps.Chat != nil {
			if id, _, ok := s.deps.Chat.ResolveToken(strings.TrimPrefix(in.Authorization, "Bearer ")); ok && id != "" {
				clientID = id
			}
		}
		won, err := mgr.ResolveHook(in.ReqID, clientID, decision)
		if err != nil {
			return nil, wsErr(err)
		}
		return &struct{ Body HookResolved }{Body: HookResolved{ReqID: in.ReqID, Decision: decision, Won: won}}, nil
	})
}

// wsErr maps a workspace error to the matching HTTP status.
func wsErr(err error) error {
	if errors.Is(err, workspace.ErrNotFound) {
		return huma.Error404NotFound(err.Error())
	}
	return huma.Error500InternalServerError(err.Error())
}
