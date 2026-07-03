package fcm

import (
	"crypto/rand"
	"crypto/rsa"
	"crypto/x509"
	"encoding/json"
	"encoding/pem"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// writeTestServiceAccount generates an RSA key and writes a minimal
// service-account JSON to a temp file, returning its path.
func writeTestServiceAccount(t *testing.T) string {
	t.Helper()
	key, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		t.Fatalf("gen key: %v", err)
	}
	der, err := x509.MarshalPKCS8PrivateKey(key)
	if err != nil {
		t.Fatalf("marshal key: %v", err)
	}
	pemKey := pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: der})
	sa := map[string]string{
		"client_email": "test@winmux.iam.gserviceaccount.com",
		"private_key":  string(pemKey),
		"project_id":   "winmux-test",
		// token_uri is overridden to the httptest server in the test.
	}
	b, _ := json.Marshal(sa)
	path := filepath.Join(t.TempDir(), "sa.json")
	if err := os.WriteFile(path, b, 0o600); err != nil {
		t.Fatalf("write sa: %v", err)
	}
	return path
}

func TestSenderNotifySendsDataMessage(t *testing.T) {
	var gotAuth, gotToken string
	var gotData map[string]string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case strings.HasSuffix(r.URL.Path, "/token"):
			// Assert it's a jwt-bearer grant, hand back an access token.
			_ = r.ParseForm()
			if r.FormValue("grant_type") != "urn:ietf:params:oauth:grant-type:jwt-bearer" {
				t.Errorf("wrong grant_type: %s", r.FormValue("grant_type"))
			}
			if r.FormValue("assertion") == "" {
				t.Error("missing signed assertion")
			}
			_ = json.NewEncoder(w).Encode(map[string]any{"access_token": "ya29.test", "expires_in": 3600})
		case strings.HasSuffix(r.URL.Path, "/send"):
			gotAuth = r.Header.Get("Authorization")
			var body struct {
				Message struct {
					Token string            `json:"token"`
					Data  map[string]string `json:"data"`
				} `json:"message"`
			}
			_ = json.NewDecoder(r.Body).Decode(&body)
			gotToken = body.Message.Token
			gotData = body.Message.Data
			_ = json.NewEncoder(w).Encode(map[string]string{"name": "projects/winmux-test/messages/1"})
		default:
			http.Error(w, "unexpected path "+r.URL.Path, http.StatusNotFound)
		}
	}))
	defer srv.Close()

	s, err := NewSender(writeTestServiceAccount(t), "", func(id string) (string, bool) {
		if id == "dev_1" {
			return "fcm-reg-token-abc", true
		}
		return "", false
	})
	if err != nil {
		t.Fatalf("NewSender: %v", err)
	}
	s.tokenURI = srv.URL + "/token"
	s.fcmURL = srv.URL + "/send"

	err = s.Notify("dev_1", map[string]any{
		"type":       "hook_request",
		"session_id": "sess_9",
		"seq":        int64(42),
		"tool_input": map[string]any{"cmd": "ls"},
	})
	if err != nil {
		t.Fatalf("Notify: %v", err)
	}
	if !strings.HasPrefix(gotAuth, "Bearer ya29.test") {
		t.Errorf("missing/wrong bearer: %q", gotAuth)
	}
	if gotToken != "fcm-reg-token-abc" {
		t.Errorf("wrong device token: %q", gotToken)
	}
	if gotData["type"] != "hook_request" || gotData["session_id"] != "sess_9" {
		t.Errorf("wrong data: %v", gotData)
	}
	// Non-string values must be JSON-stringified for FCM's string-only data.
	if gotData["seq"] != "42" {
		t.Errorf("seq not stringified: %q", gotData["seq"])
	}
	if gotData["tool_input"] != `{"cmd":"ls"}` {
		t.Errorf("tool_input not JSON-stringified: %q", gotData["tool_input"])
	}
}

func TestSenderNotifyDropsWhenNoToken(t *testing.T) {
	called := false
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		called = true
	}))
	defer srv.Close()

	s, err := NewSender(writeTestServiceAccount(t), "", func(string) (string, bool) { return "", false })
	if err != nil {
		t.Fatalf("NewSender: %v", err)
	}
	s.tokenURI = srv.URL + "/token"
	s.fcmURL = srv.URL + "/send"

	if err := s.Notify("dev_unknown", map[string]any{"type": "assistant_text"}); err != nil {
		t.Fatalf("Notify should silently drop, got: %v", err)
	}
	if called {
		t.Error("no HTTP call expected when device has no token")
	}
}

func TestNewSenderRejectsBadFile(t *testing.T) {
	if _, err := NewSender(filepath.Join(t.TempDir(), "missing.json"), "", nil); err == nil {
		t.Error("expected error for missing service account file")
	}
}
