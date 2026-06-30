package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"os"
	"strings"
	"time"
)

// logDockerUnavailable writes a single consistent diagnostic line. Used by
// both the /docker endpoint and the sampler so the cause is unambiguous in
// insights.log (this patch's whole point).
func logDockerUnavailable(socket, reason, detail string) {
	log.Printf("docker: unavailable reason=%s socket=%s uid=%d detail=%q",
		reason, socket, os.Getuid(), detail)
}

// dockerCandidates lists the socket paths we probe, in priority order. The
// daemon runs as the SSH user (systemd --user / nohup), so the standard root
// socket at /var/run/docker.sock is often unreadable; rootless Docker exposes
// a per-user socket under $XDG_RUNTIME_DIR / /run/user/<uid> instead.
func dockerCandidates() []string {
	if h := os.Getenv("DOCKER_HOST"); strings.HasPrefix(h, "unix://") {
		return []string{strings.TrimPrefix(h, "unix://")}
	}
	candidates := []string{}
	if xdg := os.Getenv("XDG_RUNTIME_DIR"); xdg != "" {
		candidates = append(candidates, xdg+"/docker.sock")
	}
	return append(candidates,
		fmt.Sprintf("/run/user/%d/docker.sock", os.Getuid()),
		"/var/run/docker.sock",
		"/run/docker.sock",
	)
}

// dockerSockPath returns the first candidate that exists on disk, falling back
// to the standard path so error messages stay meaningful.
func dockerSockPath() string {
	for _, c := range dockerCandidates() {
		if _, err := os.Stat(c); err == nil {
			return c
		}
	}
	return "/var/run/docker.sock"
}

// dockerResolve picks the socket and classifies reachability in one pass,
// returning a machine reason + a human detail. Centralises the logic so both
// the /docker endpoint and the sampler log a consistent, actionable line —
// the whole point of this patch: a failing server must explain itself.
//
// reason: "" (ok) | "not_installed" | "permission" | "no_socket".
func dockerResolve() (socket, reason, detail string) {
	socket = dockerSockPath()
	info, err := os.Stat(socket)
	if err != nil {
		if os.IsNotExist(err) {
			return socket, "not_installed", "no docker socket at any known path (" +
				strings.Join(dockerCandidates(), ", ") + ")"
		}
		return socket, "no_socket", err.Error()
	}
	if info.Mode()&os.ModeSocket == 0 {
		return socket, "no_socket", "path exists but is not a unix socket"
	}
	conn, err := net.DialTimeout("unix", socket, 2*time.Second)
	if err != nil {
		if os.IsPermission(err) || strings.Contains(err.Error(), "permission denied") {
			return socket, "permission", err.Error()
		}
		return socket, "no_socket", err.Error()
	}
	_ = conn.Close()
	return socket, "", ""
}

// dockerHint is an English, actionable one-liner per reason. Sent in the API
// response so even an old desktop UI shows guidance; the new UI localises by
// the machine `reason`.
func dockerHint(reason string) string {
	switch reason {
	case "permission":
		return "the daemon user can't access the Docker socket — add it to the 'docker' group " +
			"(sudo usermod -aG docker $USER) and fully reconnect the workspace so the daemon restarts " +
			"with the new group, or run rootless Docker."
	case "not_installed":
		return "no Docker socket found — is Docker installed and running on this server?"
	case "no_socket":
		return "the Docker socket exists but isn't responding — is the Docker daemon running?"
	case "api_error":
		return "reached the Docker socket but the API call failed — check the daemon log."
	}
	return ""
}

// DockerContainer is the slim view the Monitor UI renders.
type DockerContainer struct {
	ID      string  `json:"id"`
	Name    string  `json:"name"`
	Image   string  `json:"image"`
	State   string  `json:"state"`
	Status  string  `json:"status"`
	CPUPct  float64 `json:"cpu_pct"`
	MemUsed uint64  `json:"mem_used"`
	MemPct  float64 `json:"mem_pct"`
}

// dockerHTTP returns an http.Client that talks to the Docker unix socket
// (no heavy SDK dependency — keeps the daemon tiny).
func dockerHTTP() *http.Client {
	sock := dockerSockPath()
	return &http.Client{
		Timeout: 4 * time.Second,
		Transport: &http.Transport{
			DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
				var d net.Dialer
				return d.DialContext(ctx, "unix", sock)
			},
		},
	}
}

func dockerList() ([]DockerContainer, error) {
	resp, err := dockerHTTP().Get("http://d/containers/json?all=1")
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != 200 {
		return nil, fmt.Errorf("docker list: %d", resp.StatusCode)
	}
	var raw []struct {
		Id     string   `json:"Id"`
		Names  []string `json:"Names"`
		Image  string   `json:"Image"`
		State  string   `json:"State"`
		Status string   `json:"Status"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&raw); err != nil {
		return nil, err
	}
	out := make([]DockerContainer, 0, len(raw))
	for _, r := range raw {
		name := ""
		if len(r.Names) > 0 {
			name = strings.TrimPrefix(r.Names[0], "/")
		}
		dc := DockerContainer{
			ID: shortID(r.Id), Name: name, Image: r.Image,
			State: r.State, Status: r.Status,
		}
		if r.State == "running" {
			if cpu, mu, mp, ok := dockerStats(r.Id); ok {
				dc.CPUPct, dc.MemUsed, dc.MemPct = cpu, mu, mp
			}
		}
		out = append(out, dc)
	}
	return out, nil
}

// dockerStats fetches a one-shot (non-streaming) stats sample and computes
// CPU% the same way `docker stats` does.
func dockerStats(id string) (cpuPct float64, memUsed uint64, memPct float64, ok bool) {
	resp, err := dockerHTTP().Get("http://d/containers/" + id + "/stats?stream=false")
	if err != nil {
		return 0, 0, 0, false
	}
	defer resp.Body.Close()
	var s struct {
		CPUStats struct {
			CPUUsage    struct{ TotalUsage uint64 `json:"total_usage"` } `json:"cpu_usage"`
			SystemUsage uint64 `json:"system_cpu_usage"`
			OnlineCPUs  uint64 `json:"online_cpus"`
		} `json:"cpu_stats"`
		PreCPUStats struct {
			CPUUsage    struct{ TotalUsage uint64 `json:"total_usage"` } `json:"cpu_usage"`
			SystemUsage uint64 `json:"system_cpu_usage"`
		} `json:"precpu_stats"`
		MemoryStats struct {
			Usage uint64 `json:"usage"`
			Limit uint64 `json:"limit"`
		} `json:"memory_stats"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&s); err != nil {
		return 0, 0, 0, false
	}
	cpuDelta := float64(s.CPUStats.CPUUsage.TotalUsage) - float64(s.PreCPUStats.CPUUsage.TotalUsage)
	sysDelta := float64(s.CPUStats.SystemUsage) - float64(s.PreCPUStats.SystemUsage)
	cpus := float64(s.CPUStats.OnlineCPUs)
	if cpus == 0 {
		cpus = 1
	}
	if sysDelta > 0 && cpuDelta > 0 {
		cpuPct = round1(cpuDelta / sysDelta * cpus * 100)
	}
	memUsed = s.MemoryStats.Usage
	if s.MemoryStats.Limit > 0 {
		memPct = round1(float64(memUsed) / float64(s.MemoryStats.Limit) * 100)
	}
	return cpuPct, memUsed, memPct, true
}

func dockerAction(id, cmd string) error {
	switch cmd {
	case "start", "stop", "restart", "kill":
	default:
		return fmt.Errorf("bad cmd %q (want start|stop|restart|kill)", cmd)
	}
	resp, err := dockerHTTP().Post("http://d/containers/"+id+"/"+cmd, "application/json", nil)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	_, _ = io.Copy(io.Discard, resp.Body)
	if resp.StatusCode >= 300 {
		return fmt.Errorf("docker %s: HTTP %d", cmd, resp.StatusCode)
	}
	return nil
}

func shortID(id string) string {
	if len(id) > 12 {
		return id[:12]
	}
	return id
}
