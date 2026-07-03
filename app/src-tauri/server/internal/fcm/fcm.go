// Package fcm delivers out-of-band push notifications to paired mobile devices
// via the Firebase Cloud Messaging HTTP v1 API, so a phone gets a hook_request
// / assistant_text even when its WebSocket is closed (the app is backgrounded).
//
// OAuth2 is hand-rolled from the service-account JSON — an RS256-signed JWT
// exchanged for an access token, cached in-process — so this package adds ZERO
// third-party dependencies (matching the server's minimal-deps philosophy).
//
// Messages are DATA-ONLY (no notification payload): the mobile client decides
// how to display each type, and data messages wake the app to fetch full detail
// via the API. Sender implements core.NotificationSender structurally, so it
// needs no import of the core package (avoids an import cycle).
package fcm

import (
	"bytes"
	"crypto"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/json"
	"encoding/pem"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"sync"
	"time"
)

const (
	fcmScope        = "https://www.googleapis.com/auth/firebase.messaging"
	defaultTokenURI = "https://oauth2.googleapis.com/token"
)

// serviceAccount is the subset of a Google service-account JSON we use.
type serviceAccount struct {
	ClientEmail string `json:"client_email"`
	PrivateKey  string `json:"private_key"`
	TokenURI    string `json:"token_uri"`
	ProjectID   string `json:"project_id"`
}

// TokenResolver maps a paired-device ID to its registered FCM registration
// token. ok=false (or an empty token) means the device has no token, so the
// push is silently dropped.
type TokenResolver func(deviceID string) (fcmToken string, ok bool)

// Sender delivers data-only FCM messages. Construct with NewSender; a failure
// there means the caller should fall back to a NoopSender.
type Sender struct {
	projectID string
	sa        serviceAccount
	key       *rsa.PrivateKey
	resolve   TokenResolver
	client    *http.Client
	// tokenURI / fcmURL are fields (not constants) so tests can point them at
	// an httptest server.
	tokenURI string
	fcmURL   string

	mu       sync.Mutex
	token    string
	tokenExp time.Time
}

// NewSender loads the service-account JSON at saPath. projectID overrides the
// JSON's project_id when non-empty. Returns an error (→ caller uses NoopSender)
// if the file/key is missing or malformed.
func NewSender(saPath, projectID string, resolve TokenResolver) (*Sender, error) {
	raw, err := os.ReadFile(saPath)
	if err != nil {
		return nil, fmt.Errorf("read service account %q: %w", saPath, err)
	}
	var sa serviceAccount
	if err := json.Unmarshal(raw, &sa); err != nil {
		return nil, fmt.Errorf("parse service account: %w", err)
	}
	if sa.ClientEmail == "" || sa.PrivateKey == "" {
		return nil, fmt.Errorf("service account missing client_email/private_key")
	}
	key, err := parseRSAKey(sa.PrivateKey)
	if err != nil {
		return nil, err
	}
	pid := projectID
	if pid == "" {
		pid = sa.ProjectID
	}
	if pid == "" {
		return nil, fmt.Errorf("no project id (set FCM_PROJECT_ID or include project_id)")
	}
	if sa.TokenURI == "" {
		sa.TokenURI = defaultTokenURI
	}
	if resolve == nil {
		resolve = func(string) (string, bool) { return "", false }
	}
	return &Sender{
		projectID: pid,
		sa:        sa,
		key:       key,
		resolve:   resolve,
		client:    &http.Client{Timeout: 10 * time.Second},
		tokenURI:  sa.TokenURI,
		fcmURL:    fmt.Sprintf("https://fcm.googleapis.com/v1/projects/%s/messages:send", pid),
	}, nil
}

func parseRSAKey(pemStr string) (*rsa.PrivateKey, error) {
	block, _ := pem.Decode([]byte(pemStr))
	if block == nil {
		return nil, fmt.Errorf("service account private_key is not PEM")
	}
	if k, err := x509.ParsePKCS8PrivateKey(block.Bytes); err == nil {
		rk, ok := k.(*rsa.PrivateKey)
		if !ok {
			return nil, fmt.Errorf("service account key is not RSA")
		}
		return rk, nil
	}
	return x509.ParsePKCS1PrivateKey(block.Bytes)
}

// Notify sends a data-only message to the device's registration token. Returns
// nil (silent drop) when the device has no registered token — matching the
// NoopSender contract. Real send errors are returned so the caller can warn.
func (s *Sender) Notify(deviceID string, payload map[string]any) error {
	tok, ok := s.resolve(deviceID)
	if !ok || tok == "" {
		return nil // no registered token — drop silently
	}
	access, err := s.accessToken()
	if err != nil {
		return fmt.Errorf("fcm access token: %w", err)
	}
	// FCM data payloads are string→string only; JSON-encode non-string values.
	data := make(map[string]string, len(payload))
	for k, v := range payload {
		data[k] = stringify(v)
	}
	body, _ := json.Marshal(map[string]any{
		"message": map[string]any{
			"token":   tok,
			"data":    data,
			"android": map[string]any{"priority": "high"},
		},
	})
	req, _ := http.NewRequest(http.MethodPost, s.fcmURL, bytes.NewReader(body))
	req.Header.Set("Authorization", "Bearer "+access)
	req.Header.Set("Content-Type", "application/json")
	resp, err := s.client.Do(req)
	if err != nil {
		return fmt.Errorf("fcm send: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode >= 300 {
		b, _ := io.ReadAll(io.LimitReader(resp.Body, 2048))
		return fmt.Errorf("fcm send status %d: %s", resp.StatusCode, strings.TrimSpace(string(b)))
	}
	_, _ = io.Copy(io.Discard, resp.Body)
	return nil
}

func stringify(v any) string {
	switch x := v.(type) {
	case string:
		return x
	case nil:
		return ""
	default:
		b, _ := json.Marshal(x)
		return string(b)
	}
}

// accessToken returns a cached OAuth2 access token, refreshing when it's within
// 60s of expiry. Concurrency-safe.
func (s *Sender) accessToken() (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.token != "" && time.Until(s.tokenExp) > 60*time.Second {
		return s.token, nil
	}
	assertion, err := s.signJWT()
	if err != nil {
		return "", err
	}
	form := url.Values{
		"grant_type": {"urn:ietf:params:oauth:grant-type:jwt-bearer"},
		"assertion":  {assertion},
	}
	req, _ := http.NewRequest(http.MethodPost, s.tokenURI, strings.NewReader(form.Encode()))
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	resp, err := s.client.Do(req)
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()
	if resp.StatusCode >= 300 {
		b, _ := io.ReadAll(io.LimitReader(resp.Body, 1024))
		return "", fmt.Errorf("token endpoint status %d: %s", resp.StatusCode, strings.TrimSpace(string(b)))
	}
	var tr struct {
		AccessToken string `json:"access_token"`
		ExpiresIn   int    `json:"expires_in"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&tr); err != nil {
		return "", err
	}
	if tr.AccessToken == "" {
		return "", fmt.Errorf("token endpoint returned empty access_token")
	}
	s.token = tr.AccessToken
	s.tokenExp = time.Now().Add(time.Duration(tr.ExpiresIn) * time.Second)
	return s.token, nil
}

// signJWT builds the RS256-signed service-account assertion for the token
// exchange (iss=client_email, scope=FCM, aud=token_uri, 1h expiry).
func (s *Sender) signJWT() (string, error) {
	now := time.Now()
	header := b64url([]byte(`{"alg":"RS256","typ":"JWT"}`))
	claims, _ := json.Marshal(map[string]any{
		"iss":   s.sa.ClientEmail,
		"scope": fcmScope,
		"aud":   s.tokenURI,
		"iat":   now.Unix(),
		"exp":   now.Add(time.Hour).Unix(),
	})
	signingInput := header + "." + b64url(claims)
	h := sha256.Sum256([]byte(signingInput))
	sig, err := rsa.SignPKCS1v15(rand.Reader, s.key, crypto.SHA256, h[:])
	if err != nil {
		return "", err
	}
	return signingInput + "." + b64url(sig), nil
}

func b64url(b []byte) string { return base64.RawURLEncoding.EncodeToString(b) }
