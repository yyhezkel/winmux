import { createSignal, For, Show, onCleanup } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import qrcode from "qrcode-generator";
import { t } from "./i18n";

// Phase 70.C — Mobile pairing tab inside the Monitor. Drives the nginx-proxy
// install + the daemon's pairing endpoints via the mobile_pairing_* commands.

interface PairStatus {
  domain: string;
  nginx_active: boolean;
  configured: boolean;
}
interface QrPayload {
  version: number;
  host: string;
  port: number;
  tls: boolean;
  device_id: string;
  token: string;
  expires_at: number;
}
interface PairedDevice {
  device_id: string;
  device_name: string;
  status: string;
  last_seen: number;
  last_ip: string;
}

function fmtWhen(unix: number): string {
  if (!unix) return "—";
  return new Date(unix * 1000).toLocaleString();
}

export function MobilePairing(p: { workspaceId?: string }) {
  const [domain, setDomain] = createSignal("");
  const [cfToken, setCfToken] = createSignal("");
  const [status, setStatus] = createSignal<PairStatus | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
  const [note, setNote] = createSignal<string | null>(null);

  const [devices, setDevices] = createSignal<PairedDevice[]>([]);
  const [pairName, setPairName] = createSignal("");
  const [qrSvg, setQrSvg] = createSignal<string | null>(null);
  const [countdown, setCountdown] = createSignal(0);
  let countdownTimer: ReturnType<typeof setInterval> | undefined;

  const ws = () => p.workspaceId ?? "";

  const refreshStatus = async () => {
    if (!ws()) return;
    try {
      const s = JSON.parse(await invoke<string>("mobile_pairing_status", { workspaceId: ws() })) as PairStatus;
      setStatus(s);
      if (s.domain && !domain()) setDomain(s.domain);
    } catch (e) {
      setErr(String(e));
    }
  };

  const refreshDevices = async () => {
    if (!ws()) return;
    try {
      const r = JSON.parse(await invoke<string>("mobile_pairing_list_devices", { workspaceId: ws() })) as {
        devices: PairedDevice[];
      };
      setDevices(r.devices ?? []);
    } catch {
      /* daemon may not be reachable yet — leave the list empty */
    }
  };

  // Initial load.
  void refreshStatus();
  void refreshDevices();

  const install = async () => {
    if (!ws() || !domain().trim() || !cfToken().trim()) return;
    setBusy(true);
    setErr(null);
    setNote(null);
    try {
      const r = JSON.parse(
        await invoke<string>("mobile_pairing_init", {
          workspaceId: ws(),
          domain: domain().trim(),
          cfToken: cfToken().trim(),
        }),
      ) as { ok: boolean; status: string };
      setCfToken(""); // clear the secret from the UI immediately
      setNote(r.status || t("mobile.installed"));
      await refreshStatus();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const clearCountdown = () => {
    if (countdownTimer) clearInterval(countdownTimer);
    countdownTimer = undefined;
  };
  onCleanup(clearCountdown);

  const generateQr = async () => {
    if (!ws()) return;
    setErr(null);
    setQrSvg(null);
    try {
      const payload = JSON.parse(
        await invoke<string>("mobile_pairing_generate_qr", {
          workspaceId: ws(),
          deviceName: pairName().trim() || "device",
        }),
      ) as QrPayload;
      // Render the QR locally from the payload JSON (typeNumber 0 = auto-size).
      const qr = qrcode(0, "M");
      qr.addData(JSON.stringify(payload));
      qr.make();
      setQrSvg(qr.createSvgTag({ cellSize: 5, margin: 4 }));
      // 5-min countdown from the token's expiry.
      const secs = Math.max(0, payload.expires_at - Math.floor(Date.now() / 1000));
      setCountdown(secs);
      clearCountdown();
      countdownTimer = setInterval(() => {
        setCountdown((c) => {
          if (c <= 1) {
            clearCountdown();
            setQrSvg(null);
            void refreshDevices();
            return 0;
          }
          return c - 1;
        });
      }, 1000);
      void refreshDevices();
    } catch (e) {
      setErr(String(e));
    }
  };

  const revoke = async (id: string) => {
    try {
      await invoke("mobile_pairing_revoke", { workspaceId: ws(), deviceId: id });
      await refreshDevices();
    } catch (e) {
      setErr(String(e));
    }
  };

  const rename = async (id: string, current: string) => {
    const name = window.prompt(t("mobile.rename_prompt"), current);
    if (name == null) return;
    try {
      await invoke("mobile_pairing_rename", { workspaceId: ws(), deviceId: id, name });
      await refreshDevices();
    } catch (e) {
      setErr(String(e));
    }
  };

  const mm = (s: number) => `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;

  return (
    <div class="mob-tab">
      <Show when={err()}>
        <div class="wizard-test-result err" style="margin:0 0 10px"><div class="wizard-test-line">✗ {err()}</div></div>
      </Show>

      {/* ── Setup ── */}
      <h4 class="ins-h4">{t("mobile.setup")}</h4>
      <div class="mob-setup">
        <label class="mob-field">
          <span>{t("mobile.domain")}</span>
          <input
            type="text"
            placeholder="winmux.example.com"
            value={domain()}
            onInput={(e) => setDomain(e.currentTarget.value)}
          />
        </label>
        <label class="mob-field">
          <span>{t("mobile.cf_token")}</span>
          <input
            type="password"
            placeholder="cloudflare API token"
            value={cfToken()}
            onInput={(e) => setCfToken(e.currentTarget.value)}
          />
        </label>
        <button class="primary" disabled={busy() || !domain().trim() || !cfToken().trim()} onClick={() => void install()}>
          {busy() ? t("mobile.installing") : t("mobile.install")}
        </button>
      </div>
      <div class="mob-status">
        <Show when={status()} fallback={<span class="settings-hint">{t("mobile.not_configured")}</span>}>
          <span class={status()!.nginx_active ? "mob-ok" : "mob-warn"}>
            {status()!.nginx_active ? "● nginx active" : "○ nginx inactive"}
          </span>
          <Show when={status()!.domain}><span class="settings-hint"> · {status()!.domain}</span></Show>
        </Show>
        <Show when={note()}><div class="settings-hint">{note()}</div></Show>
      </div>

      {/* ── Pairing ── */}
      <h4 class="ins-h4">{t("mobile.pairing")}</h4>
      <div class="mob-pair">
        <input
          type="text"
          placeholder={t("mobile.device_name")}
          value={pairName()}
          onInput={(e) => setPairName(e.currentTarget.value)}
        />
        <button class="primary" disabled={!status()?.configured} onClick={() => void generateQr()}>
          {t("mobile.pair_new")}
        </button>
      </div>
      <Show when={qrSvg()}>
        <div class="mob-qr">
          {/* eslint-disable-next-line solid/no-innerhtml -- our own generated SVG */}
          <div class="mob-qr-img" innerHTML={qrSvg()!} />
          <div class="mob-qr-meta">
            <div class="settings-hint">{t("mobile.scan_hint")}</div>
            <div class="mob-countdown">{t("mobile.expires_in")} {mm(countdown())}</div>
          </div>
        </div>
      </Show>

      {/* ── Devices / activity ── */}
      <h4 class="ins-h4">{t("mobile.devices")}</h4>
      <Show when={devices().length === 0}>
        <div class="settings-hint">{t("mobile.no_devices")}</div>
      </Show>
      <div class="mob-devices">
        <For each={devices()}>
          {(d) => (
            <div class="mob-dev">
              <span class="mob-dev-name">
                {d.status === "active" ? "●" : d.status === "pending" ? "◌" : "✕"} {d.device_name || d.device_id}
              </span>
              <span class="mob-dev-meta settings-hint">
                {d.status} · {fmtWhen(d.last_seen)}{d.last_ip ? ` · ${d.last_ip}` : ""}
              </span>
              <span class="mob-dev-actions">
                <button onClick={() => void rename(d.device_id, d.device_name)}>{t("common.rename")}</button>
                <button onClick={() => void revoke(d.device_id)}>{t("mobile.revoke")}</button>
              </span>
            </div>
          )}
        </For>
      </div>
    </div>
  );
}
