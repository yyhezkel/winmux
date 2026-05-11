import { createEffect, createSignal, For, Show, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import type { Connection, EnvVar, Workspace } from "./types";

// Phase 13.A wizard data shapes — mirror the Rust definitions in
// src-tauri/src/connect_wizard.rs.
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
    if (type() === "local") {
      connection = { type: "local", shell: shell() || undefined };
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
      setup_command: setupCmd() || undefined,
      teardown_command: teardownCmd() || undefined,
      env: cleanedEnv().length ? cleanedEnv() : undefined,
    });
    p.onClose();
  };

  return (
    <Show when={p.open}>
      <div class="modal-backdrop" onClick={p.onClose}>
        <div class="modal" onClick={(e) => e.stopPropagation()}>
          <h3>{isEdit() ? "Edit workspace" : "New workspace"}</h3>

          <label>
            <span>Name</span>
            <input
              autofocus
              value={name()}
              onInput={(e) => setName(e.currentTarget.value)}
              placeholder="e.g. Local PowerShell"
            />
          </label>

          <label>
            <span>Color</span>
            <input
              type="color"
              value={color()}
              onInput={(e) => setColor(e.currentTarget.value)}
            />
          </label>

          <label>
            <span>Type</span>
            <select
              value={type()}
              onChange={(e) =>
                setType(e.currentTarget.value as "local" | "ssh")
              }
              disabled={isEdit()}
            >
              <option value="local">Local PTY</option>
              <option value="ssh">SSH</option>
            </select>
          </label>

          <Show when={type() === "local"}>
            <label>
              <span>Shell</span>
              <input
                value={shell()}
                onInput={(e) => setShell(e.currentTarget.value)}
                placeholder="(auto: pwsh→powershell→cmd)"
                disabled={isEdit()}
              />
            </label>
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
                      ? "No ~/.ssh/config hosts found"
                      : `Pick from ${sshHosts().length} ssh_config host(s)`
                  }
                >
                  Import from SSH config
                  <Show when={sshHosts().length > 0}>
                    <span class="wizard-pill">{sshHosts().length}</span>
                  </Show>
                </button>
              </div>
            </Show>
            <label>
              <span>User</span>
              <input
                value={user()}
                onInput={(e) => setUser(e.currentTarget.value)}
                placeholder="user"
                disabled={isEdit()}
              />
            </label>
            <label>
              <span>Host</span>
              <input
                value={host()}
                onInput={(e) => setHost(e.currentTarget.value)}
                placeholder="example.com or 1.2.3.4"
                disabled={isEdit()}
              />
            </label>
            <label>
              <span>Port</span>
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
              <span>Key</span>
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
                <option value="auto">Auto (ssh-agent → ~/.ssh defaults)</option>
                <option value="detected" disabled={detectedKeys().length === 0}>
                  Detected key ({detectedKeys().length} found)
                </option>
                <option value="custom">Custom path…</option>
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
                  placeholder="C:\Users\you\.ssh\id_ed25519"
                  disabled={isEdit()}
                />
              </label>
            </Show>
            <Show when={keyPath() && keyPerms() && !keyPerms()!.ok && !isEdit()}>
              <div class="wizard-row wizard-warn">
                <span>⚠ Permissions too open: {keyPerms()!.error}</span>
                <button
                  type="button"
                  class="wizard-fix"
                  onClick={() => void onFixPerms()}
                >
                  Fix permissions
                </button>
              </div>
            </Show>
            <Show when={keyPath() && keyPerms()?.ok && !isEdit()}>
              <div class="wizard-row wizard-ok">✓ Permissions locked to current user</div>
            </Show>
            <Show when={!isEdit()}>
              <div class="wizard-test">
                <div class="wizard-test-fields">
                  <input
                    type="password"
                    placeholder="Password (only if needed)"
                    value={testPassword()}
                    onInput={(e) => setTestPassword(e.currentTarget.value)}
                  />
                  <input
                    type="password"
                    placeholder="Key passphrase (only if needed)"
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
                  {testing() ? "Testing…" : "Test connection"}
                </button>
                <Show when={testResult()}>
                  <div
                    class={`wizard-test-result ${testResult()!.ok ? "ok" : "err"}`}
                  >
                    <div class="wizard-test-line">
                      {testResult()!.ok ? "✓" : "✗"}{" "}
                      {testResult()!.message ??
                        (testResult()!.ok ? "Connected" : "Failed")}
                    </div>
                    <Show when={testResult()!.method}>
                      <div class="wizard-test-meta">
                        Method: {testResult()!.method} · Stage:{" "}
                        {testResult()!.stage} · {testResult()!.elapsed_ms}ms
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
            <span>Setup cmd</span>
            <textarea
              rows="2"
              value={setupCmd()}
              onInput={(e) => setSetupCmd(e.currentTarget.value)}
              placeholder="run after the shell prompt is ready, e.g. 'cd /repo && nvm use'"
            />
          </label>

          <label class="modal-textarea-label">
            <span>Teardown cmd</span>
            <textarea
              rows="2"
              value={teardownCmd()}
              onInput={(e) => setTeardownCmd(e.currentTarget.value)}
              placeholder="run before disconnect, e.g. 'make clean'"
            />
          </label>

          <div class="env-editor">
            <div class="env-editor-head">
              <span>Env vars</span>
              <button
                class="env-add"
                onClick={() => setEnvRows([...envRows(), { key: "", value: "" }])}
              >
                + add
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
            <button onClick={p.onClose}>Cancel</button>
            <button class="primary" onClick={submit}>
              {isEdit() ? "Save" : "Create"}
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
            <h3>Pick a host from ~/.ssh/config</h3>
            <Show
              when={sshHosts().length > 0}
              fallback={
                <p class="status-line">
                  No Host blocks found. Add some to ~/.ssh/config to use this.
                </p>
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
              <button onClick={() => setShowHostPicker(false)}>Close</button>
            </div>
          </div>
        </div>
      </Show>
    </Show>
  );
}
