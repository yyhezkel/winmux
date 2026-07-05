package insights

import (
	"strings"
	"testing"
)

func TestDockerHint(t *testing.T) {
	for _, reason := range []string{"permission", "not_installed", "no_socket", "api_error"} {
		if dockerHint(reason) == "" {
			t.Fatalf("expected a hint for reason %q", reason)
		}
	}
	if dockerHint("") != "" {
		t.Fatal("ok state should have no hint")
	}
	if !strings.Contains(dockerHint("permission"), "docker") {
		t.Fatal("permission hint should mention the docker group")
	}
}

func TestDockerCandidatesHonorsDockerHost(t *testing.T) {
	t.Setenv("DOCKER_HOST", "unix:///custom/docker.sock")
	c := dockerCandidates()
	if len(c) != 1 || c[0] != "/custom/docker.sock" {
		t.Fatalf("DOCKER_HOST not honored: %+v", c)
	}
}

func TestDockerCandidatesIncludesRootlessAndStandard(t *testing.T) {
	t.Setenv("DOCKER_HOST", "")
	t.Setenv("XDG_RUNTIME_DIR", "/run/user/1000")
	c := dockerCandidates()
	joined := strings.Join(c, "|")
	for _, want := range []string{"/run/user/1000/docker.sock", "/var/run/docker.sock", "/run/docker.sock"} {
		if !strings.Contains(joined, want) {
			t.Fatalf("candidate %q missing from %v", want, c)
		}
	}
}
