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

	"winmux-server/internal/workspace"
)

var mobileSecured = []map[string][]string{{"bearerAuth": {}}}

// OKResponse is a minimal {"ok": true} body.
type OKResponse struct {
	OK bool `json:"ok"`
}

// FCMTokenRequest registers a device's Firebase push registration token.
type FCMTokenRequest struct {
	FCMToken string `json:"fcm_token"`
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

		// POST /api/v2/devices/{id}/fcm-token — a paired device registers its
		// Firebase push token so the server can wake it when its WS is closed.
		// A device may only set its OWN token; the shared/admin token may set any.
		huma.Register(api, huma.Operation{
			OperationID: "device-register-fcm-token", Method: http.MethodPost,
			Path:    "/api/v2/devices/{id}/fcm-token",
			Summary: "Register a device's FCM push token",
			Tags:    []string{"pairing"}, Security: mobileSecured,
		}, func(_ context.Context, in *struct {
			ID            string `path:"id"`
			Authorization string `header:"Authorization"`
			Body          FCMTokenRequest
		}) (*struct{ Body OKResponse }, error) {
			deviceID, admin, ok := s.deps.Chat.ResolveToken(strings.TrimPrefix(in.Authorization, "Bearer "))
			if !ok {
				return nil, huma.Error401Unauthorized("unauthorized")
			}
			if !admin && deviceID != in.ID {
				return nil, huma.Error403Forbidden("cannot register a token for another device")
			}
			if !s.deps.Chat.SetDeviceFCMToken(in.ID, strings.TrimSpace(in.Body.FCMToken)) {
				return nil, huma.Error404NotFound("device not found or not active")
			}
			return &struct{ Body OKResponse }{Body: OKResponse{OK: true}}, nil
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
}

// wsErr maps a workspace error to the matching HTTP status.
func wsErr(err error) error {
	if errors.Is(err, workspace.ErrNotFound) {
		return huma.Error404NotFound(err.Error())
	}
	return huma.Error500InternalServerError(err.Error())
}
