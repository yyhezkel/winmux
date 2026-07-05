package files

// service.go — the Files API service. The HTTP surface itself is defined as
// typed huma operations in huma.go (Phase 77 S4); this file holds the Service
// value and a mux-based RegisterRoutes shim so existing callers/tests that mount
// onto a raw *http.ServeMux keep working. In production the api package calls
// RegisterHuma directly on the server-wide huma API.

import (
	"net/http"

	"github.com/danielgtaylor/huma/v2"
	"github.com/danielgtaylor/huma/v2/adapters/humago"

	"winmux-server/internal/core"
)

// Service serves the Files API over a core.FilesProvider.
type Service struct {
	fp core.FilesProvider
}

// NewService wires the HTTP layer to a provider (LocalFiles in production, a
// mock in tests).
func NewService(fp core.FilesProvider) *Service {
	return &Service{fp: fp}
}

// RegisterRoutes mounts /api/v2/files/* onto a raw mux by building a local huma
// API over it. The auth argument is accepted for signature compatibility with
// the other subsystems but not used here: production auth is enforced by the
// shared API's bearer middleware (see internal/api), and tests pass a
// pass-through, so ignoring it preserves the exact test behavior.
func (s *Service) RegisterRoutes(mux *http.ServeMux, _ func(http.HandlerFunc) http.HandlerFunc) {
	s.RegisterHuma(humago.New(mux, quietConfig()))
}

// quietConfig is a huma config with no auto-served docs/openapi/schemas
// endpoints and no $schema link transformer, so a locally-mounted API adds only
// the operation routes and nothing else to the mux or the response bodies.
func quietConfig() huma.Config {
	c := huma.DefaultConfig("winmux-server files", core.Version)
	c.CreateHooks = nil // drop the $schema/Link response transformer
	c.OpenAPIPath = ""
	c.DocsPath = ""
	c.SchemasPath = ""
	return c
}
