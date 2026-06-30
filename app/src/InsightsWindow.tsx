import { createSignal, For, Show, createEffect, onCleanup } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { t } from "./i18n";
import {
  clampToViewport,
  makeWindowControls,
  ResizeHandles,
  type Geometry,
} from "./floatingWindow";

// Phase 68.D: Server Insights monitor. Pull-based — fetches the live
// snapshot from the remote `winmux-insights` daemon (via the insights_fetch
// Tauri command, which curls 127.0.0.1:7879 over the workspace SSH session).
// No mock data: if the daemon isn't installed/running the panel says so and
// points at Settings → Add-ons.

interface Snapshot {
  ts: number;
  cpu: { pct: number; per_core: number[]; load: number[] };
  mem: { total: number; used: number; cached: number; swap_used: number };
  disks: { mount: string; total: number; used: number; pct: number }[];
  net: { iface: string; rx_bps: number; tx_bps: number }[];
  docker_running: number;
  docker_total: number;
  top: { pid: number; name: string; cpu: number; rss: number }[];
}
interface DockerContainer {
  id: string;
  name: string;
  image: string;
  state: string;
  status: string;
  cpu_pct: number;
  mem_used: number;
  mem_pct: number;
}

interface Props {
  open: boolean;
  workspaceId?: string;
  workspaceName?: string;
  onClose: () => void;
  /** Phase 68 (UX): open the Add-ons window to install the daemon. */
  onInstall?: () => void;
}

const DEFAULT_GEOMETRY: Geometry = { x: 180, y: 90, w: 820, h: 620 };
const MIN_W = 460;
const MIN_H = 360;

function fmtBytes(n: number): string {
  if (!n) return "0";
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v < 10 && i > 0 ? 1 : 0)} ${u[i]}`;
}
const fmtBps = (n: number) => `${fmtBytes(n)}/s`;

export function InsightsWindow(p: Props) {
  const [geom, setGeom] = createSignal<Geometry>(
    clampToViewport(DEFAULT_GEOMETRY, MIN_W, MIN_H),
  );
  const { onDragStart, onResizeStart } = makeWindowControls({
    geom,
    setGeom,
    minW: MIN_W,
    minH: MIN_H,
    closeGuardSelector: ".insights-x",
  });

  const [snap, setSnap] = createSignal<Snapshot | null>(null);
  const [docker, setDocker] = createSignal<DockerContainer[]>([]);
  const [dockerOk, setDockerOk] = createSignal(false);
  const [dockerReason, setDockerReason] = createSignal<string | null>(null);
  // Diagnostics surfaced by the daemon (Phase 68 docker patch): the resolved
  // socket, the daemon version (so we can tell if the server is on an old
  // build), and the daemon's own English hint.
  const [dockerInfo, setDockerInfo] = createSignal<{
    socket: string;
    version: string;
    hint: string;
    detail: string;
  } | null>(null);
  const [err, setErr] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [auto, setAuto] = createSignal(false);

  const refresh = async () => {
    if (!p.workspaceId) return;
    setLoading(true);
    setErr(null);
    try {
      const cur = await invoke<string>("insights_fetch", {
        workspaceId: p.workspaceId,
        path: "/current",
      });
      if (!cur.trim()) throw new Error(t("insights.unreachable"));
      setSnap(JSON.parse(cur) as Snapshot);
      try {
        const d = await invoke<string>("insights_fetch", {
          workspaceId: p.workspaceId,
          path: "/docker",
        });
        const parsed = JSON.parse(d) as {
          available: boolean;
          reason?: string;
          detail?: string;
          socket?: string;
          hint?: string;
          daemon_version?: string;
          containers: DockerContainer[];
        };
        setDockerOk(parsed.available);
        setDockerReason(parsed.available ? null : parsed.reason ?? null);
        setDocker(parsed.containers ?? []);
        setDockerInfo({
          socket: parsed.socket ?? "",
          version: parsed.daemon_version ?? "",
          hint: parsed.hint ?? "",
          detail: parsed.detail ?? "",
        });
      } catch {
        // Empty/failed /docker fetch — likely an old daemon that predates the
        // endpoint, or the daemon is down. Flag it so the panel can advise.
        setDockerOk(false);
        setDockerReason("api_error");
        setDocker([]);
        setDockerInfo(null);
      }
    } catch (e) {
      setErr(String(e instanceof Error ? e.message : e));
      setSnap(null);
    } finally {
      setLoading(false);
    }
  };

  // Refresh on open; optional 5s auto-refresh while focused.
  createEffect(() => {
    if (!p.open || !p.workspaceId) return;
    void refresh();
    if (!auto()) return;
    const id = setInterval(() => void refresh(), 5000);
    onCleanup(() => clearInterval(id));
  });

  const dockerAction = async (id: string, action: string) => {
    if (!p.workspaceId) return;
    try {
      await invoke("insights_docker_action", {
        workspaceId: p.workspaceId,
        containerId: id,
        action,
      });
      await refresh();
    } catch (e) {
      setErr(String(e));
    }
  };

  const bar = (pct: number, danger = false) => (
    <div class="ins-bar">
      <div
        class={`ins-bar-fill ${danger && pct > 80 ? "danger" : ""}`}
        style={{ width: `${Math.min(100, Math.max(0, pct))}%` }}
      />
    </div>
  );

  return (
    <Show when={p.open}>
      <div
        class="fm-window insights-window"
        style={{
          left: `${geom().x}px`,
          top: `${geom().y}px`,
          width: `${geom().w}px`,
          height: `${geom().h}px`,
        }}
      >
        <div class="fm-window-header" onMouseDown={onDragStart}>
          <span class="fm-window-title">
            📊 {t("insights.title")}
            {p.workspaceName ? ` — ${p.workspaceName}` : ""}
          </span>
          <label class="ins-auto">
            <input type="checkbox" checked={auto()} onChange={(e) => setAuto(e.currentTarget.checked)} />
            <span>{t("insights.auto")}</span>
          </label>
          <button class="ins-refresh" onClick={() => void refresh()} title={t("insights.refresh")}>
            {loading() ? "…" : "⟳"}
          </button>
          <button class="fm-window-x insights-x" onClick={p.onClose} title={t("common.close")}>
            ×
          </button>
        </div>
        <div class="fm-window-body insights-body">
          <Show when={err()}>
            <div class="wizard-test-result err" style="margin:10px">
              <div class="wizard-test-line">✗ {err()}</div>
              <div class="wizard-test-meta">{t("insights.install_hint")}</div>
              <Show when={p.onInstall}>
                <button
                  class="primary"
                  style="margin-top:8px"
                  onClick={() => {
                    p.onClose();
                    p.onInstall!();
                  }}
                >
                  {t("insights.install_btn")}
                </button>
              </Show>
            </div>
          </Show>
          <Show when={snap()}>
            {(s) => (
              <>
                <div class="ins-metrics">
                  <div class="ins-metric">
                    <div class="ins-metric-head"><span>CPU</span><b>{s().cpu.pct}%</b></div>
                    {bar(s().cpu.pct, true)}
                    <div class="ins-metric-sub">load {s().cpu.load.map((l) => l.toFixed(2)).join(" ")}</div>
                  </div>
                  <div class="ins-metric">
                    <div class="ins-metric-head">
                      <span>RAM</span>
                      <b>{fmtBytes(s().mem.used)} / {fmtBytes(s().mem.total)}</b>
                    </div>
                    {bar(s().mem.total ? (s().mem.used / s().mem.total) * 100 : 0, true)}
                    <div class="ins-metric-sub">swap {fmtBytes(s().mem.swap_used)}</div>
                  </div>
                  <For each={s().disks}>
                    {(d) => (
                      <div class="ins-metric">
                        <div class="ins-metric-head"><span>{d.mount}</span><b>{d.pct}%</b></div>
                        {bar(d.pct, true)}
                        <div class="ins-metric-sub">{fmtBytes(d.used)} / {fmtBytes(d.total)}</div>
                      </div>
                    )}
                  </For>
                  <div class="ins-metric">
                    <div class="ins-metric-head"><span>NET</span><b /></div>
                    <For each={s().net}>
                      {(n) => (
                        <div class="ins-metric-sub">{n.iface}: ↓{fmtBps(n.rx_bps)} ↑{fmtBps(n.tx_bps)}</div>
                      )}
                    </For>
                  </div>
                </div>

                <h4 class="ins-h4">🐳 Docker ({s().docker_running}/{s().docker_total})</h4>
                <Show when={!dockerOk()}>
                  <div class="ins-docker-err">
                    <div class="ins-docker-err-msg">
                      ⚠️ {t(
                        dockerReason() === "permission" ? "insights.dk_reason.permission"
                        : dockerReason() === "not_installed" ? "insights.dk_reason.not_installed"
                        : dockerReason() === "no_socket" ? "insights.dk_reason.no_socket"
                        : dockerReason() === "api_error" ? "insights.dk_reason.api_error"
                        : "insights.no_docker"
                      )}
                    </div>
                    <Show when={dockerInfo()?.hint}>
                      <div class="ins-docker-err-hint">{dockerInfo()?.hint}</div>
                    </Show>
                    <div class="ins-docker-err-diag">
                      <Show when={dockerInfo()?.socket}>
                        <span>socket: <code>{dockerInfo()?.socket}</code></span>
                      </Show>
                      <span>
                        {t("insights.daemon_version")}:{" "}
                        <code>{dockerInfo()?.version || t("insights.daemon_unknown")}</code>
                      </span>
                    </div>
                    {/* No version field at all → the server runs a pre-patch daemon. */}
                    <Show when={p.onInstall && !dockerInfo()?.version}>
                      <div class="ins-docker-err-hint">{t("insights.daemon_outdated")}</div>
                    </Show>
                    <Show when={p.onInstall}>
                      <button class="primary" style="margin-top:8px" onClick={() => p.onInstall?.()}>
                        {t("insights.dk_reinstall")}
                      </button>
                    </Show>
                  </div>
                </Show>
                <Show when={dockerOk() && docker().length === 0}>
                  <div class="settings-hint">{t("insights.dk_no_containers")}</div>
                </Show>
                <div class="ins-docker">
                  <For each={docker()}>
                    {(c) => (
                      <div class={`ins-cont ${(c.cpu_pct > 80 || c.mem_pct > 80) ? "alert" : ""}`}>
                        <span class="ins-cont-name">{c.state === "running" ? "●" : "○"} {c.name}</span>
                        <span class="ins-cont-stat">
                          {c.state === "running"
                            ? `${c.cpu_pct}% cpu · ${fmtBytes(c.mem_used)} (${c.mem_pct}%)`
                            : c.status}
                        </span>
                        <span class="ins-cont-actions">
                          <Show when={c.state === "running"}>
                            <button onClick={() => void dockerAction(c.id, "stop")}>{t("insights.dk.stop")}</button>
                            <button onClick={() => void dockerAction(c.id, "restart")}>{t("insights.dk.restart")}</button>
                          </Show>
                          <Show when={c.state !== "running"}>
                            <button onClick={() => void dockerAction(c.id, "start")}>{t("insights.dk.start")}</button>
                          </Show>
                        </span>
                      </div>
                    )}
                  </For>
                </div>

                <h4 class="ins-h4">{t("insights.top")}</h4>
                <div class="ins-procs">
                  <For each={s().top.slice(0, 8)}>
                    {(pr) => (
                      <span class="ins-proc">{pr.name} <b>{pr.cpu}%</b> {fmtBytes(pr.rss)}</span>
                    )}
                  </For>
                </div>
              </>
            )}
          </Show>
        </div>
        <ResizeHandles onStart={onResizeStart} />
      </div>
    </Show>
  );
}
