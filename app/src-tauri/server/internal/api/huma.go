package api

// huma.go — the server-wide huma API. Phase 77 S4 moves the client-SDK HTTP
// surface (version/health negotiation + Files + Logs) onto typed huma
// operations so /api/openapi.json is generated from the handlers and can't
// drift. Insights stays on raw stdlib handlers (dynamic metric/docker/process
// maps, consumed by the desktop Monitor's Rust client, not by generated SDKs)
// and is intentionally out of the SDK spec — see DECISIONS.md (S4.1 scope).

import (
	"context"
	"net/http"
	"strings"

	"github.com/danielgtaylor/huma/v2"
	"github.com/danielgtaylor/huma/v2/adapters/humago"

	"winmux-server/internal/core"
)

// newHumaAPI builds the shared huma API on the given mux: security scheme,
// bearer middleware, and the version/health/files/logs operations. It returns
// the API so the caller can marshal OpenAPI() after registration.
func (s *Server) newHumaAPI(mux *http.ServeMux) huma.API {
	cfg := huma.DefaultConfig("winmux-server", core.Version)
	cfg.Info.Description = "winmux server daemon API — the client-SDK surface " +
		"(version negotiation, Files, Logs). Streaming WebSocket frames are " +
		"described in asyncapi.json (PHASE-77-DESIGN §4.4). Generated from the " +
		"huma handlers (S4); the Insights metrics API is desktop-internal and " +
		"documented separately."
	cfg.OpenAPI.Components.SecuritySchemes = map[string]*huma.SecurityScheme{
		"bearerAuth": {Type: "http", Scheme: "bearer"},
	}
	// We serve the spec ourselves (with CORS + caching) and don't want the
	// $schema link transformer mutating response bodies, so strip the auto
	// endpoints + create hooks.
	cfg.CreateHooks = nil
	cfg.OpenAPIPath = ""
	cfg.DocsPath = ""
	cfg.SchemasPath = ""

	api := humago.New(mux, cfg)
	api.UseMiddleware(bearerMiddleware(api, s.token))

	registerMetaOps(api)
	if s.deps.Files != nil {
		s.deps.Files.RegisterHuma(api)
	}
	if s.deps.Logs != nil {
		s.deps.Logs.RegisterHuma(api)
	}
	return api
}

// OpenAPISpec returns the generated OpenAPI document as JSON. It builds the
// huma API on a throwaway mux (registration only reflects types; the handlers
// are never invoked), so it works even with nil subsystem providers — the
// `winmux-server openapi` subcommand and the SDK pipeline use it to emit the
// spec without a running server.
func (s *Server) OpenAPISpec() ([]byte, error) {
	return s.newHumaAPI(http.NewServeMux()).OpenAPI().MarshalJSON()
}

// bearerMiddleware enforces the shared bearer token for every operation that
// declares a security requirement; operations with no Security (version,
// health) are public. Mirrors auth.Bearer's behavior (same 401 on missing or
// mismatched token) for the huma-hosted routes.
func bearerMiddleware(api huma.API, token string) func(huma.Context, func(huma.Context)) {
	return func(ctx huma.Context, next func(huma.Context)) {
		if len(ctx.Operation().Security) == 0 {
			next(ctx)
			return
		}
		got := strings.TrimPrefix(ctx.Header("Authorization"), "Bearer ")
		if got == "" || got != token {
			_ = huma.WriteErr(api, ctx, http.StatusUnauthorized, "unauthorized")
			return
		}
		next(ctx)
	}
}

// VersionBody is the capability-negotiation payload (§4, §4.4).
type VersionBody struct {
	Name         string `json:"name"`
	Version      string `json:"version"`
	APIVersions  []int  `json:"api_versions"`
	FrameVersion int    `json:"frame_version"`
}

// HealthBody is the liveness payload.
type HealthBody struct {
	OK      bool   `json:"ok"`
	Version string `json:"version"`
}

// registerMetaOps mounts the unauthenticated liveness + version endpoints.
func registerMetaOps(api huma.API) {
	huma.Register(api, huma.Operation{
		OperationID: "health", Method: http.MethodGet, Path: "/healthz",
		Summary: "Liveness probe (unauthenticated)", Tags: []string{"meta"},
	}, func(_ context.Context, _ *struct{}) (*struct{ Body HealthBody }, error) {
		return &struct{ Body HealthBody }{Body: HealthBody{OK: true, Version: core.Version}}, nil
	})

	huma.Register(api, huma.Operation{
		OperationID: "version", Method: http.MethodGet, Path: "/api/version",
		Summary: "Version + capability negotiation (unauthenticated)", Tags: []string{"meta"},
	}, func(_ context.Context, _ *struct{}) (*struct{ Body VersionBody }, error) {
		return &struct{ Body VersionBody }{Body: VersionBody{
			Name:         "winmux-server",
			Version:      core.Version,
			APIVersions:  []int{2},
			FrameVersion: core.FrameVersion,
		}}, nil
	})
}
