package logs

// service.go — the Logs API service value + a mux-based RegisterRoutes shim.
// The HTTP surface is defined as typed huma operations in huma.go (Phase 77
// S4); this shim lets existing callers/tests mount onto a raw *http.ServeMux. In
// production the api package calls RegisterHuma on the server-wide huma API.

import (
	"net/http"

	"github.com/danielgtaylor/huma/v2"
	"github.com/danielgtaylor/huma/v2/adapters/humago"

	"winmux-server/internal/core"
)

// Service serves the Logs API over a Store.
type Service struct {
	store *Store
}

// NewService wires the HTTP layer to a Store.
func NewService(store *Store) *Service { return &Service{store: store} }

// RegisterRoutes mounts /api/v2/logs/* onto a raw mux by building a local huma
// API over it. The auth argument is accepted for signature compatibility only;
// production auth is enforced by the shared API's bearer middleware and tests
// pass a pass-through, so ignoring it preserves the exact behavior.
func (s *Service) RegisterRoutes(mux *http.ServeMux, _ func(http.HandlerFunc) http.HandlerFunc) {
	s.RegisterHuma(humago.New(mux, quietConfig()))
}

// quietConfig mirrors files.quietConfig: no auto docs/openapi/schemas endpoints
// and no $schema link transformer, so mounting adds only the operation routes.
func quietConfig() huma.Config {
	c := huma.DefaultConfig("winmux-server logs", core.Version)
	c.CreateHooks = nil
	c.OpenAPIPath = ""
	c.DocsPath = ""
	c.SchemasPath = ""
	return c
}
