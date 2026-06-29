package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"strings"
	"time"
)

const dockerSock = "/var/run/docker.sock"

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
	return &http.Client{
		Timeout: 4 * time.Second,
		Transport: &http.Transport{
			DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
				var d net.Dialer
				return d.DialContext(ctx, "unix", dockerSock)
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
