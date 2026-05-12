import { createEffect, createSignal, For, Show, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { t } from "./i18n";
import type { Connection, EnvVar, Workspace } from "./types";

// Phase 13.A wizard data shapes — mirror the Rust definitions in
// src-tauri/src/connect_wizard.rs.
interface ShellInfo {
  id: string;
  label: string;
  command: string;
  available: boolean;
}

interface RecentPathSuggestion {
  path: string;
  kind: "recent" | "default";
}

interface SshConfigHost {
  alias: string;
  hostname?: string | null;
  user?: string | null;
  port?: number | null;
  identity_file?: string | null;
  proxy_command?: string | null;
  proxy_jump?: string | null;
}

interface DetectedKey {
  path: string;
  filename: string;
  modified_iso: string | null;
  size_bytes: number;
  fingerprint: string | null;
  key_type: string | null;
  perms_ok: boolean;
  perms_error: string | null;
}

interface PermsResult {
  ok: boolean;
  error?: string | null;
}

interface TestResult {
  ok: boolean;
  stage: string;
  method?: string | null;
  server_version?: string | null;
  message?: string | null;
  hint?: string | null;
  elapsed_ms: number;
}

interface Props {
  open: boolean;
  // When set, the modal is in EDIT mode for that workspace; the connection-type
  // fields are read-only (deleting + recreating is required to change).
  editing: Workspace | null;
  onClose: () => void;
  onCreate: (input: {
    name: string;
    connection: Connection;
    color?: string;
    cwd?: string;
    setup_command?: string;
    teardown_command?: string;
    env?: EnvVar[];
  }) => void;
  onUpdate: (
    id: string,
    fields: {
      name?: string;
      color?: string;
      cwd?: string;
      setup_command?: string;
      teardown_command?: string;
      env?: EnvVar[];
    }
  ) => void;
}

export function CreateWorkspaceModal(p: Props) {
  const [name, setName] = createSignal("");
  const [type, setType] = createSignal<"local" | "ssh">("local");
  const [shell, setShell] = createSignal("");
  const [host, setHost] = createSignal("");
  const [user, setUser] = createSignal("");
  const [port, setPort] = createSignal(22);
  const [keyPath, setKeyPath] = createSignal("");
  const [color, setColor] = createSignal("#7aa2f7");
  const [setupCmd, setSetupCmd] = createSignal("");
  const [teardownCmd, setTeardownCmd] = createSignal("");
  const [envRows, setEnvRows] = createSignal<EnvVar[]>([]);

  // Phase 12.C local PTY mini-wizard state. `wizardMode` toggles between
  // "wizard" (shell dropdown + cwd combobox seeded from history) and
  // "custom" (a single free-text command field). The cwd signal is
  // separate from `shell` so the wizard preserves the user's directory
  // when they flip between shells.
  const [wizardMode, setWizardMode] = createSignal<"wizard" | "custom">("wizard");
  const [shellId, setShellId] = createSignal<string>("powershell");
  const [cwd, setCwd] = createSignal<string>("");
  const [detectedShells, setDetectedShells] = createSignal<ShellInfo[]>([]);
  const [recentPaths, setRecentPaths] = createSignal<RecentPathSuggestion[]>([]);

  // Phase 13.A wizard state.
  const [sshHosts, setSshHosts] = createSignal<SshConfigHost[]>([]);
  const [sshHostsLoaded, setSshHostsLoaded] = createSignal(false);
  const [showHostPicker, setShowHostPicker] = createSignal(false);
  const [detectedKeys, setDetectedKeys] = createSignal<DetectedKey[]>([]);
  const [keyMode, setKeyMode] = createSignal<"auto" | "detected" | "custom">("auto");
  const [keyPerms, setKeyPerms] = createSignal<PermsResult | null>(null);
  const [testPassword, setTestPassword] = createSignal("");
  const [testPassphrase, setTestPassphrase] = createSignal("");
  const [testing, setTesting] = createSignal(false);
  const [testResult, setTestResult] = createSignal<TestResult | null>(null);

  const isEdit = () => p.editing !== null;

  // Lazy-load the wizard inputs whenever the modal opens. Cheap calls and
  // they make the SSH section feel alive on first render.
  const loadWizardData = async () => {
    try {
      const hosts = await invoke<SshConfigHost[]>("parse_ssh_config");
      setSshHosts(hosts);
      setSshHostsLoaded(true);
    } catch (e) {
      console.warn("parse_ssh_config failed", e);
      setSshHostsLoaded(true);
    }
    try {
      const keys = await invoke<DetectedKey[]>("list_ssh_keys");
      setDetectedKeys(keys);
    } catch (e) {
      console.warn("list_ssh_keys failed", e);
    }
    // Phase 12.C: load the local PTY wizard inputs too. Both are cheap
    // (one PATH walk + one tiny JSON read) so we always pre-fetch even
    // if the user starts on the SSH tab.
    try {
      const shells = await invoke<ShellInfo[]>("detect_local_shells");
      setDetectedShells(shells);
    } catch (e) {
      console.warn("detect_local_shells failed", e);
    }
    try {
      const paths = await invoke<RecentPathSuggestion[]>("list_recent_paths");
      setRecentPaths(paths);
    } catch (e) {
      console.warn("list_recent_paths failed", e);
    }
  };

  onMount(() => {
    if (p.open) void loadWizardData();
  });

  // Re-check perms when keyPath changes via the detected dropdown.
  const refreshPermsFor = async (path: string) => {
    if (!path) {
      setKeyPerms(null);
      return;
    }
    try {
      const r = await invoke<PermsResult>("check_key_permissions", { path });
      setKeyPerms(r);
    } catch (e) {
      console.warn("check_key_permissions failed", e);
    }
  };

  const onPickDetectedKey = async (path: string) => {
    setKeyPath(path);
    setKeyMode(path ? "detected" : "auto");
    await refreshPermsFor(path);
  };

  const onFixPerms = async () => {
    const path = keyPath();
    if (!path) return;
    try {
      const r = await invoke<PermsResult>("fix_key_permissions", { path });
      setKeyPerms(r);
    } catch (e) {
      console.error("fix_key_permissions failed", e);
      setKeyPerms({ ok: false, error: String(e) });
    }
  };

  const onImportHost = (h: SshConfigHost) => {
    if (h.hostname) setHost(h.hostname);
    else if (h.alias) setHost(h.alias);
    if (h.user) setUser(h.user);
    if (h.port) setPort(h.port);
    if (h.identity_file) {
      setKeyPath(h.identity_file);
      setKeyMode("custom");
      void refreshPermsFor(h.identity_file);
    }
    if (!name().trim()) setName(h.alias);
    setShowHostPicker(false);
  };

  const onTestConnect = async () => {
    if (!host().trim() || !user().trim()) {
      setTestResult({
        ok: false,
        stage: "validation",
        message: "user + host required",
        elapsed_ms: 0,
      });
      return;
    }
    setTesting(true);
    setTestResult(null);
    try {
      const r = await invoke<TestResult>("test_ssh_connect", {
        host: host().trim(),
        user: user().trim(),
        port: port(),
        keyPath: keyPath() || null,
        keyPassphrase: testPassphrase() || null,
        password: testPassword() || null,
      });
      setTestResult(r);
    } catch (e) {
      setTestResult({
        ok: false,
        stage: "rpc",
        message: String(e),
        elapsed_ms: 0,
      });
    } finally {
      setTesting(false);
    }
  };

  // Whenever the modal becomes visible, refresh the wizard cache so the
  // dropdowns reflect newly-added keys / ssh_config entries without the
  // user having to reopen the app.
  createEffect(() => {
    if (p.open) void loadWizardData();
  });

  // Populate from `editing` whenever it (or open) changes.
  createEffect(() => {
    if (p.open) {
      if (p.editing) {
        const w = p.editing;
        setName(w.name);
        setColor(w.color || "#7aa2f7");
        setSetupCmd(w.setup_command || "");
        setTeardownCmd(w.teardown_command || "");
        setEnvRows(w.env ? [...w.env] : []);
        // Connection fields shown read-only.
        const c = w.layout?.kind === "pane" ? w.layout.connection : w.connection;
        if (c?.type === "local") {
          setType("local");
          setShell(c.shell || "");
        } else if (c?.type === "ssh") {
          setType("ssh");
          setHost(c.host);
          setUser(c.user);
          setPort(c.port);
          setKeyPath(c.key_path || "");
        }
      } else {
        // Fresh "new workspace" state.
        setName("");
        setType("local");
        setShell("");
        setHost("");
        setUser("");
        setPort(22);
        setKeyPath("");
        setColor("#7aa2f7");
        setSetupCmd("");
        setTeardownCmd("");
        setEnvRows([]);
        setWizardMode("wizard");
        setShellId("powershell");
        setCwd("");
      }
    }
  });

  const cleanedEnv = (): EnvVar[] =>
    envRows().filter((r) => r.key.trim() !== "");

  const submit = () => {
    if (!name().trim()) return;

    if (isEdit()) {
      p.onUpdate(p.editing!.id, {
        name: name().trim(),
        color: color(),
        setup_command: setupCmd(),
        teardown_command: teardownCmd(),
        env: cleanedEnv(),
      });
      p.onClose();
      return;
    }

    let connection: Connection;
    let workspaceCwd: string | undefined;
    if (type() === "local") {
      // Phase 12.C: pick the shell from the active mode. Wizard mode
      // maps shellId → the detected command string (or a sensible
      // default if detection failed). Custom mode passes the typed
      // `shell` straight through. The cwd lands at the workspace
      // level so any local pane in the workspace picks it up.
      let cmd: string | undefined;
      if (wizardMode() === "wizard") {
        const found = detectedShells().find((s) => s.id === shellId());
        cmd = found?.command;
      } else {
        cmd = shell().trim() || undefined;
      }
      connection = { type: "local", shell: cmd };
      workspaceCwd = cwd().trim() || undefined;
    } else {
      if (!host().trim() || !user().trim()) return;
      connection = {
        type: "ssh",
        host: host().trim(),
        user: user().trim(),
        port: port(),
        key_path: keyPath() || undefined,
      };
    }
    p.onCreate({
      name: name().trim(),
      connection,
      color: color(),
      cwd: workspaceCwd,
      setup_command: setupCmd() || undefined,
      teardown_command: teardownCmd() || undefined,
      env: cleanedEnv().length ? cleanedEnv() : undefined,
    });
    // Phase 12.C: bump the chosen cwd into the recent-paths history so
    // it shows up in the combobox next time. Best-effort — silently
    // ignore RPC failures.
    if (workspaceCwd) {
      invoke("record_recent_path", { path: workspaceCwd }).catch(() => {});
    }
    p.onClose();
  };

  return (
    <Show when={p.open}>
      <div class="modal-backdrop" onClick={p.onClose}>
        <div class="modal" onClick={(e) => e.stopPropagation()}>
          <h3>{isEdit() ? t("ws.create.title.edit") : t("ws.create.title.new")}</h3>

          <label>
            <span>{t("ws.create.field.name")}</span>
            <input
              autofocus
              value={name()}
              onInput={(e) => setName(e.currentTarget.value)}
              placeholder={t("ws.create.field.name.placeholder")}
            />
          </label>

          <label>
            <span>{t("ws.create.field.color")}</span>
            <input
              type="color"
              value={color()}
              onInput={(e) => setColor(e.currentTarget.value)}
            />
          </label>

          <label>
            <span>{t("ws.create.field.type")}</span>
            <select
              value={type()}
              onChange={(e) =>
                setType(e.currentTarget.value as "local" | "ssh")
              }
              disabled={isEdit()}
            >
              <option value="local">{t("ws.create.field.type.local")}</option>
              <option value="ssh">{t("ws.create.field.type.ssh")}</option>
            </select>
          </label>

          <Show when={type() === "local"}>
            {/* Phase 12.C: mode toggle. Wizard = shell dropdown +
                cwd combobox; Custom = single free-text command. */}
            <Show when={!isEdit()}>
              <div class="local-wizard-tabs">
                <button
                  type="button"
                  class={`local-wizard-tab ${wizardMode() === "wizard" ? "active" : ""}`}
                  onClick={() => setWizardMode("wizard")}
                >
                  {t("ws.create.mode.wizard")}
                </button>
                <button
                  type="button"
                  class={`local-wizard-tab ${wizardMode() === "custom" ? "active" : ""}`}
                  onClick={() => setWizardMode("custom")}
                >
                  {t("ws.create.mode.custom")}
                </button>
              </div>
            </Show>
            <Show when={wizardMode() === "wizard" && !isEdit()}>
              <label>
                <span>{t("ws.create.shell.label")}</span>
                <select
                  value={shellId()}
                  onChange={(e) => setShellId(e.currentTarget.value)}
                >
                  <For each={detectedShells()}>
                    {(s) => (
                      <option value={s.id} disabled={!s.available}>
                        {t(`ws.create.shell.${s.id}`, {}) || s.label}
                        {!s.available ? " " + t("ws.create.shell.not_installed") : ""}
                      </option>
                    )}
                  </For>
                </select>
              </label>
              <label>
                <span>{t("ws.create.cwd.label")}</span>
                <input
                  type="text"
                  list="winmux-recent-paths"
                  value={cwd()}
                  onInput={(e) => setCwd(e.currentTarget.value)}
                  placeholder={t("ws.create.cwd.placeholder")}
                />
              </label>
              <datalist id="winmux-recent-paths">
                <For each={recentPaths()}>
                  {(rp) => (
                    <option value={rp.path}>
                      {rp.kind === "recent"
                        ? t("ws.create.cwd.recent")
                        : t("ws.create.cwd.defaults")}
                    </option>
                  )}
                </For>
              </datalist>
            </Show>
            <Show when={wizardMode() === "custom" || isEdit()}>
              <label>
                <span>{t("ws.create.custom_cmd.label")}</span>
                <input
                  value={shell()}
                  onInput={(e) => setShell(e.currentTarget.value)}
                  placeholder={t("ws.create.custom_cmd.placeholder")}
                  disabled={isEdit()}
                />
              </label>
              <Show when={!isEdit()}>
                <label>
                  <span>{t("ws.create.cwd.label")}</span>
                  <input
                    type="text"
                    list="winmux-recent-paths"
                    value={cwd()}
                    onInput={(e) => setCwd(e.currentTarget.value)}
                    placeholder={t("ws.create.cwd.placeholder")}
                  />
                </label>
                <datalist id="winmux-recent-paths">
                  <For each={recentPaths()}>
                    {(rp) => <option value={rp.path} />}
                  </For>
                </datalist>
              </Show>
            </Show>
          </Show>

          <Show when={type() === "ssh"}>
            <Show when={!isEdit()}>
              <div class="wizard-row">
                <button
                  class="wizard-import"
                  type="button"
                  disabled={!sshHostsLoaded() || sshHosts().length === 0}
                  onClick={() => setShowHostPicker(true)}
                  title={
                    sshHosts().length === 0
                      ? t("wizard.import_no_hosts")
                      : t("wizard.import_hosts_tooltip", { n: sshHosts().length })
                  }
                >
                  {t("wizard.import_btn")}
                  <Show when={sshHosts().length > 0}>
                    <span class="wizard-pill">{sshHosts().length}</span>
                  </Show>
                </button>
              </div>
            </Show>
            <label>
              <span>{t("ws.create.field.user")}</span>
              <input
                value={user()}
                onInput={(e) => setUser(e.currentTarget.value)}
                placeholder="user"
                disabled={isEdit()}
              />
            </label>
            <label>
              <span>{t("ws.create.field.host")}</span>
              <input
                value={host()}
                onInput={(e) => setHost(e.currentTarget.value)}
                placeholder={t("ws.create.field.host.placeholder")}
                disabled={isEdit()}
              />
            </label>
            <label>
              <span>{t("ws.create.field.port")}</span>
              <input
                type="number"
                value={port()}
                onInput={(e) =>
                  setPort(parseInt(e.currentTarget.value) || 22)
                }
                disabled={isEdit()}
              />
            </label>
            <label>
              <span>{t("ws.create.field.key")}</span>
              <select
                value={keyMode()}
                disabled={isEdit()}
                onChange={(e) => {
                  const v = e.currentTarget.value as "auto" | "detected" | "custom";
                  setKeyMode(v);
                  if (v === "auto") {
                    setKeyPath("");
                    setKeyPerms(null);
                  } else if (v === "detected" && detectedKeys().length > 0) {
                    void onPickDetectedKey(detectedKeys()[0].path);
                  }
                }}
              >
                <option value="auto">{t("ws.create.key.mode.auto")}</option>
                <option value="detected" disabled={detectedKeys().length === 0}>
                  {t("ws.create.key.mode.detected", { n: detectedKeys().length })}
                </option>
                <option value="custom">{t("ws.create.key.mode.custom")}</option>
              </select>
            </label>
            <Show when={keyMode() === "detected"}>
              <label>
                <span></span>
                <select
                  value={keyPath()}
                  disabled={isEdit()}
                  onChange={(e) => void onPickDetectedKey(e.currentTarget.value)}
                >
                  <For each={detectedKeys()}>
                    {(k) => (
                      <option value={k.path}>
                        {k.filename}
                        {k.key_type ? ` (${k.key_type})` : ""}
                        {k.fingerprint ? ` · ${k.fingerprint.slice(0, 19)}…` : ""}
                      </option>
                    )}
                  </For>
                </select>
              </label>
            </Show>
            <Show when={keyMode() === "custom"}>
              <label>
                <span></span>
                <input
                  value={keyPath()}
                  onInput={(e) => {
                    setKeyPath(e.currentTarget.value);
                    if (e.currentTarget.value) void refreshPermsFor(e.currentTarget.value);
                    else setKeyPerms(null);
                  }}
                  placeholder={t("ws.create.field.key.placeholder")}
                  disabled={isEdit()}
                />
              </label>
            </Show>
            <Show when={keyPath() && keyPerms() && !keyPerms()!.ok && !isEdit()}>
              <div class="wizard-row wizard-warn">
                <span>{t("wizard.perms_warn", { err: keyPerms()!.error ?? "" })}</span>
                <button
                  type="button"
                  class="wizard-fix"
                  onClick={() => void onFixPerms()}
                >
                  {t("wizard.perms_fix")}
                </button>
              </div>
            </Show>
            <Show when={keyPath() && keyPerms()?.ok && !isEdit()}>
              <div class="wizard-row wizard-ok">{t("wizard.perms_ok")}</div>
            </Show>
            <Show when={!isEdit()}>
              <div class="wizard-test">
                <div class="wizard-test-fields">
                  <input
                    type="password"
                    placeholder={t("wizard.test_password.placeholder")}
                    value={testPassword()}
                    onInput={(e) => setTestPassword(e.currentTarget.value)}
                  />
                  <input
                    type="password"
                    placeholder={t("wizard.test_passphrase.placeholder")}
                    value={testPassphrase()}
                    onInput={(e) => setTestPassphrase(e.currentTarget.value)}
                  />
                </div>
                <button
                  type="button"
                  class="wizard-test-btn"
                  disabled={testing()}
                  onClick={() => void onTestConnect()}
                >
                  {testing() ? t("wizard.testing") : t("wizard.test_btn")}
                </button>
                <Show when={testResult()}>
                  <div
                    class={`wizard-test-result ${testResult()!.ok ? "ok" : "err"}`}
                  >
                    <div class="wizard-test-line">
                      {testResult()!.ok ? "✓" : "✗"}{" "}
                      {testResult()!.message ??
                        (testResult()!.ok ? t("wizard.test_connected") : t("wizard.test_failed"))}
                    </div>
                    <Show when={testResult()!.method}>
                      <div class="wizard-test-meta">
                        {t("wizard.test_meta", {
                          method: testResult()!.method ?? "",
                          stage: testResult()!.stage,
                          ms: testResult()!.elapsed_ms,
                        })}
                      </div>
                    </Show>
                    <Show when={!testResult()!.ok && testResult()!.hint}>
                      <div class="wizard-test-hint">{testResult()!.hint}</div>
                    </Show>
                  </div>
                </Show>
              </div>
            </Show>
          </Show>

          <hr class="modal-sep" />

          <label class="modal-textarea-label">
            <span>{t("ws.create.field.setup_cmd")}</span>
            <textarea
              rows="2"
              value={setupCmd()}
              onInput={(e) => setSetupCmd(e.currentTarget.value)}
              placeholder={t("ws.create.field.setup_cmd.placeholder")}
            />
          </label>

          <label class="modal-textarea-label">
            <span>{t("ws.create.field.teardown_cmd")}</span>
            <textarea
              rows="2"
              value={teardownCmd()}
              onInput={(e) => setTeardownCmd(e.currentTarget.value)}
              placeholder={t("ws.create.field.teardown_cmd.placeholder")}
            />
          </label>

          <div class="env-editor">
            <div class="env-editor-head">
              <span>{t("ws.create.field.env")}</span>
              <button
                class="env-add"
                onClick={() => setEnvRows([...envRows(), { key: "", value: "" }])}
              >
                {t("ws.create.btn.add_env")}
              </button>
            </div>
            <For each={envRows()}>
              {(row, i) => (
                <div class="env-row">
                  <input
                    placeholder="KEY"
                    value={row.key}
                    onInput={(e) => {
                      const next = [...envRows()];
                      next[i()] = { ...next[i()], key: e.currentTarget.value };
                      setEnvRows(next);
                    }}
                  />
                  <span class="env-eq">=</span>
                  <input
                    placeholder="value"
                    value={row.value}
                    onInput={(e) => {
                      const next = [...envRows()];
                      next[i()] = {
                        ...next[i()],
                        value: e.currentTarget.value,
                      };
                      setEnvRows(next);
                    }}
                  />
                  <button
                    class="env-remove"
                    title="remove"
                    onClick={() => {
                      const next = [...envRows()];
                      next.splice(i(), 1);
                      setEnvRows(next);
                    }}
                  >
                    ×
                  </button>
                </div>
              )}
            </For>
          </div>

          <div class="modal-buttons">
            <button onClick={p.onClose}>{t("common.cancel")}</button>
            <button class="primary" onClick={submit}>
              {isEdit() ? t("common.save") : t("ws.create.btn.create")}
            </button>
          </div>
        </div>
      </div>
      <Show when={showHostPicker()}>
        <div
          class="modal-backdrop"
          onClick={() => setShowHostPicker(false)}
          style={{ "z-index": 110 }}
        >
          <div
            class="modal wizard-host-picker"
            onClick={(e) => e.stopPropagation()}
          >
            <h3>{t("wizard.picker.title")}</h3>
            <Show
              when={sshHosts().length > 0}
              fallback={
                <p class="status-line">{t("wizard.picker.empty")}</p>
              }
            >
              <ul class="wizard-host-list">
                <For each={sshHosts()}>
                  {(h) => (
                    <li
                      class="wizard-host-row"
                      onClick={() => onImportHost(h)}
                    >
                      <div class="wizard-host-alias">{h.alias}</div>
                      <div class="wizard-host-meta">
                        {(h.user ? `${h.user}@` : "") +
                          (h.hostname ?? h.alias) +
                          (h.port && h.port !== 22 ? `:${h.port}` : "")}
                        <Show when={h.identity_file}>
                          {" · " + h.identity_file}
                        </Show>
                      </div>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
            <div class="modal-buttons">
              <button onClick={() => setShowHostPicker(false)}>{t("common.close")}</button>
            </div>
          </div>
        </div>
      </Show>
    </Show>
  );
}
