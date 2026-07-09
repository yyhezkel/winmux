import { createEffect, createSignal, For, Show, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { t } from "./i18n";
import { IconGitBranch, IconCheck, IconClose } from "./icons";
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
  // Phase 34: open the SSH-key-setup HelpPane as a side-by-side split.
  // Optional — when omitted (e.g. tests, embedded usage), the `?` icon
  // next to the key field still renders but the click is a no-op. The
  // parent (App.tsx) wires this to workspace_split with paneKind=help
  // on the currently-active workspace.
  onOpenSshHelp?: () => void;
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
      // Phase 37: editable connection (SSH workspaces). Absent = leave
      // the connection unchanged.
      connection?: Connection;
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
  // Phase 37: SSH auth mode. "key" → use key_path; "password" → save no
  // credential, prompt interactively at every connect (never persisted).
  const [authMode, setAuthMode] = createSignal<"key" | "password">("key");
  // Phase 36 (#2.2): auto port-forward toggle (edit mode only).
  const [autoPortForward, setAutoPortForward] = createSignal(true);
  // Phase 49-B: worktree creator (edit mode + local + cwd is git repo).
  // wtBusy guards against double-clicks while git is running; wtErr
  // surfaces the backend's git stderr verbatim so the user can react.
  const [wtBranch, setWtBranch] = createSignal("");
  const [wtBase, setWtBase] = createSignal("main");
  const [wtBusy, setWtBusy] = createSignal(false);
  const [wtErr, setWtErr] = createSignal<string | null>(null);
  const [color, setColor] = createSignal("#7aa2f7");
  const [emoji, setEmoji] = createSignal("");
  // Phase 30: editable hex value for the custom-color text input. Kept
  // separate from `color` so a half-typed value (e.g. "#fff") doesn't
  // clobber the live preview until blur/validation succeeds.
  const [customHex, setCustomHex] = createSignal("");
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

  // Phase 30: presets shown in the identity row. Eight named colors that
  // look distinct on both light and dark surfaces, and nine common
  // category-style glyphs. Both lists are deliberately short — too many
  // choices defeats the "instant recognition" goal.
  const COLOR_PRESETS = [
    "#1e40af", "#6d28d9", "#16a34a", "#ea580c",
    "#dc2626", "#ca8a04", "#0891b2", "#475569",
  ];
  const EMOJI_PRESETS = ["🟦", "🟣", "🟢", "🟠", "🔴", "🟡", "🔵", "⚪", "⬛"];
  const HEX_RE = /^#[0-9a-fA-F]{6}$/;

  // Phase 30: live-save helper. Used by the swatch / emoji buttons so a
  // click instantly persists + emits `workspaces:changed`; the Save
  // button is still required for non-identity fields (rename, env, …).
  const saveIdentity = async (nextColor: string | null, nextEmoji: string | null) => {
    if (!p.editing) return;
    try {
      const ws = await invoke<Workspace>("workspace_set_identity", {
        workspaceId: p.editing.id,
        color: nextColor,
        emoji: nextEmoji,
      });
      // Reflect the server-canonical state in local signals. `workspaces:changed`
      // will reload the workspace list in App.tsx — but the modal keeps a stale
      // p.editing snapshot, so we update local fields explicitly here too.
      setColor(ws.color || "#7aa2f7");
      setEmoji(ws.emoji || "");
      setCustomHex(ws.color || "");
    } catch (e) {
      console.error("workspace_set_identity failed", e);
    }
  };

  const pickColor = (hex: string) => {
    setColor(hex);
    setCustomHex(hex);
    void saveIdentity(hex, emoji() || null);
  };

  const pickEmoji = (g: string) => {
    setEmoji(g);
    void saveIdentity(color() || null, g);
  };

  const onCustomHexBlur = () => {
    const v = customHex().trim();
    if (v === "") {
      // Empty = revert to whatever color() currently shows; do not save.
      setCustomHex(color());
      return;
    }
    if (HEX_RE.test(v)) {
      setColor(v);
      void saveIdentity(v, emoji() || null);
    } else {
      // Invalid: revert input to last accepted color.
      setCustomHex(color());
    }
  };

  const onCustomEmojiInput = (v: string) => {
    // Limit emoji input to 4 grapheme-ish chars (cheap byte cap will keep us
    // well under the backend's 16-byte ceiling for any plausible glyph).
    const trimmed = v.slice(0, 8);
    setEmoji(trimmed);
  };

  const onCustomEmojiBlur = () => {
    void saveIdentity(color() || null, emoji() || null);
  };

  const resetIdentity = () => {
    setColor("#7aa2f7");
    setEmoji("");
    setCustomHex("");
    void saveIdentity(null, null);
  };

  // Phase 36 (#2.2): live-toggle auto port forwarding (edit mode).
  const onToggleAutoPortForward = (enabled: boolean) => {
    setAutoPortForward(enabled);
    if (!p.editing) return;
    void invoke("workspace_set_auto_port_forward", {
      workspaceId: p.editing.id,
      enabled,
    }).catch((e) => console.error("workspace_set_auto_port_forward failed", e));
  };

  // Phase 49-B: spawn `git worktree add` from the workspace's cwd. The
  // backend re-anchors the workspace's cwd to the new worktree path and
  // sets git_worktree, so new panes will spawn inside it.
  const onCreateWorktree = async () => {
    if (!p.editing) return;
    const branch = wtBranch().trim();
    const base = wtBase().trim();
    if (!branch || !base) return;
    setWtBusy(true);
    setWtErr(null);
    try {
      await invoke("workspace_create_worktree", {
        workspaceId: p.editing.id,
        branchName: branch,
        baseBranch: base,
      });
      setWtBranch("");
    } catch (e) {
      setWtErr(String(e));
    } finally {
      setWtBusy(false);
    }
  };

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
        setEmoji(w.emoji || "");
        setCustomHex(w.color || "");
        setSetupCmd(w.setup_command || "");
        setTeardownCmd(w.teardown_command || "");
        setEnvRows(w.env ? [...w.env] : []);
        setAutoPortForward(w.auto_port_forward ?? true);
        // Phase 37: connection fields are now editable (not read-only).
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
          // Phase 37: derive auth mode from whether a key is set. A
          // workspace saved in password mode has key_path = null →
          // hydrate into "password" mode + custom key-mode so a later
          // switch to "key" shows the path field.
          if (c.key_path) {
            setAuthMode("key");
            setKeyMode("custom");
          } else {
            setAuthMode("password");
          }
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
        setAuthMode("key");
        setColor("#7aa2f7");
        setEmoji("");
        setCustomHex("");
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

  // Phase 37: build the SSH Connection from the current form state.
  // In "password" auth mode key_path is null — the workspace saves no
  // credential and is prompted interactively at connect (never saved).
  const buildSshConnection = (): Connection | null => {
    if (!host().trim() || !user().trim()) return null;
    return {
      type: "ssh",
      host: host().trim(),
      user: user().trim(),
      port: port(),
      key_path: authMode() === "key" ? keyPath() || null : null,
    };
  };

  const submit = () => {
    if (!name().trim()) return;

    if (isEdit()) {
      // Phase 37: connection fields are now editable. For SSH
      // workspaces send the rebuilt connection so host/user/port/key/
      // auth-mode changes persist; local workspaces keep their existing
      // connection (their shell wizard state isn't reconstructed here).
      let editedConn: Connection | undefined;
      if (type() === "ssh") {
        const c = buildSshConnection();
        if (!c) return; // host/user required
        editedConn = c;
      }
      p.onUpdate(p.editing!.id, {
        name: name().trim(),
        color: color(),
        setup_command: setupCmd(),
        teardown_command: teardownCmd(),
        env: cleanedEnv(),
        connection: editedConn,
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
      // ts-rs binding renders Option<String> as `string | null`, so
      // these absent values are null, not undefined.
      connection = { type: "local", shell: cmd ?? null };
      workspaceCwd = cwd().trim() || undefined;
    } else {
      const c = buildSshConnection();
      if (!c) return;
      connection = c;
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
        <div class="modal ws-create-modal" onClick={(e) => e.stopPropagation()}>
          <h3>{isEdit() ? t("ws.create.title.edit") : t("ws.create.title.new")}</h3>
          {/* Phase 42: scrollable body — header (h3) above and footer
              (modal-buttons) below stay pinned via the flex column. */}
          <div class="ws-create-modal-body">

          <label>
            <span>{t("ws.create.field.name")}</span>
            <input
              autofocus
              value={name()}
              onInput={(e) => setName(e.currentTarget.value)}
              placeholder={t("ws.create.field.name.placeholder")}
            />
          </label>

          {/* Phase 30: rich identity picker shown in edit mode only.
              Each swatch / emoji is live-saved via workspace_set_identity. */}
          <Show when={isEdit()}>
            <div class="ws-identity-block">
              <div class="ws-identity-label">{t("ws.identity.color")}</div>
              <div class="ws-identity-row">
                <For each={COLOR_PRESETS}>
                  {(c) => (
                    <button
                      type="button"
                      class={`ws-identity-swatch ${color() === c ? "selected" : ""}`}
                      style={{ background: c }}
                      title={c}
                      onClick={() => pickColor(c)}
                    />
                  )}
                </For>
                <input
                  type="text"
                  class="ws-identity-hex"
                  value={customHex()}
                  placeholder={t("ws.identity.customColor")}
                  spellcheck={false}
                  onInput={(e) => setCustomHex(e.currentTarget.value)}
                  onBlur={onCustomHexBlur}
                />
              </div>
              <div class="ws-identity-label" style="margin-top: 8px">{t("ws.identity.emoji")}</div>
              <div class="ws-identity-row">
                <For each={EMOJI_PRESETS}>
                  {(g) => (
                    <button
                      type="button"
                      class={`ws-identity-emoji-btn ${emoji() === g ? "selected" : ""}`}
                      title={g}
                      onClick={() => pickEmoji(g)}
                    >
                      {g}
                    </button>
                  )}
                </For>
                <input
                  type="text"
                  class="ws-identity-emoji-custom"
                  value={emoji()}
                  placeholder={t("ws.identity.customEmoji")}
                  maxlength={8}
                  onInput={(e) => onCustomEmojiInput(e.currentTarget.value)}
                  onBlur={onCustomEmojiBlur}
                />
                <button
                  type="button"
                  class="ws-identity-reset"
                  onClick={resetIdentity}
                >
                  {t("ws.identity.reset")}
                </button>
              </div>
            </div>
          </Show>

          {/* Phase 36 (#2.2): auto port forwarding toggle. Edit-mode
              only — new workspaces default to ON (backend), toggle
              after creation here. */}
          <Show when={isEdit()}>
            <label class="ws-autoport">
              <input
                type="checkbox"
                checked={autoPortForward()}
                onChange={(e) => onToggleAutoPortForward(e.currentTarget.checked)}
              />
              <span>{t("ws.autoPortForward.label")}</span>
            </label>
            <p class="settings-hint ws-autoport-hint">{t("ws.autoPortForward.hint")}</p>
          </Show>

          {/* Phase 49-B: worktree creator. Shown only in edit mode for
              local workspaces. If a worktree already exists for this
              workspace, the path is shown instead. */}
          <Show when={isEdit() && type() === "local"}>
            <div class="ws-worktree-block">
              <Show
                when={!p.editing?.git_worktree}
                fallback={
                  <p class="settings-hint">
                    <IconGitBranch size={13} /> {t("ws.worktree.alreadyOn")}{" "}
                    <code>{p.editing?.git_worktree}</code>
                  </p>
                }
              >
                <label>
                  <span>{t("ws.worktree.branchName")}</span>
                  <input
                    type="text"
                    value={wtBranch()}
                    placeholder="feature/my-thing"
                    onInput={(e) => setWtBranch(e.currentTarget.value)}
                    disabled={wtBusy()}
                  />
                </label>
                <label>
                  <span>{t("ws.worktree.baseBranch")}</span>
                  <input
                    type="text"
                    value={wtBase()}
                    placeholder="main"
                    onInput={(e) => setWtBase(e.currentTarget.value)}
                    disabled={wtBusy()}
                  />
                </label>
                <button
                  type="button"
                  onClick={() => void onCreateWorktree()}
                  disabled={wtBusy() || !wtBranch().trim() || !wtBase().trim()}
                >
                  {wtBusy() ? "…" : t("ws.worktree.create")}
                </button>
                <Show when={wtErr()}>
                  <p class="settings-hint" style="color:#e88">
                    {t("ws.worktree.failed")}: {wtErr()}
                  </p>
                </Show>
                <p class="settings-hint">{t("ws.worktree.hint")}</p>
              </Show>
            </div>
          </Show>

          <Show when={!isEdit()}>
            <label>
              <span>{t("ws.create.field.color")}</span>
              <input
                type="color"
                value={color()}
                onInput={(e) => setColor(e.currentTarget.value)}
              />
            </label>
          </Show>

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
            {/* Phase 42: user / host / port sit in a 2-col grid so the
                modal can be wider without leaving acres of empty space. */}
            <div class="ws-form-grid">
              <label>
                <span>{t("ws.create.field.user")}</span>
                <input
                  value={user()}
                  onInput={(e) => setUser(e.currentTarget.value)}
                  placeholder={t("ws.create.field.user.placeholder")}
                />
              </label>
              <label>
                <span>{t("ws.create.field.host")}</span>
                <input
                  value={host()}
                  onInput={(e) => setHost(e.currentTarget.value)}
                  placeholder={t("ws.create.field.host.placeholder")}
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
                />
              </label>
            </div>
            {/* Phase 37: auth-mode chooser. "Password" mode saves no
                credential — the user is prompted interactively at every
                connect and the password is never persisted. */}
            <label>
              <span class="ws-key-label">
                {t("ws.create.auth.label")}
                {/* Phase 34: single contextual entry point to the
                    SSH-key-setup HelpPane. */}
                <button
                  type="button"
                  class="help-hint-btn"
                  title={t("help.sshKey.tooltip")}
                  onClick={(e) => {
                    e.preventDefault();
                    e.stopPropagation();
                    p.onOpenSshHelp?.();
                  }}
                >
                  ?
                </button>
              </span>
              <div class="ws-auth-seg">
                <label class="ws-auth-opt">
                  <input
                    type="radio"
                    name="ws-auth-mode"
                    checked={authMode() === "key"}
                    onChange={() => setAuthMode("key")}
                  />
                  <span>{t("ws.create.auth.key")}</span>
                </label>
                <label class="ws-auth-opt">
                  <input
                    type="radio"
                    name="ws-auth-mode"
                    checked={authMode() === "password"}
                    onChange={() => {
                      setAuthMode("password");
                      setKeyPath("");
                      setKeyPerms(null);
                    }}
                  />
                  <span>{t("ws.create.auth.password")}</span>
                </label>
              </div>
            </label>

            <Show when={authMode() === "password"}>
              <p class="settings-hint ws-auth-hint">{t("ws.create.auth.password.hint")}</p>
            </Show>

            <Show when={authMode() === "key"}>
              <label>
                <span>{t("ws.create.field.key")}</span>
                <select
                  value={keyMode()}
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
                  />
                </label>
              </Show>
              <Show when={keyPath() && keyPerms() && !keyPerms()!.ok}>
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
              <Show when={keyPath() && keyPerms()?.ok}>
                <div class="wizard-row wizard-ok">{t("wizard.perms_ok")}</div>
              </Show>
            </Show>

            <Show when={true}>
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
                      {testResult()!.ok ? <IconCheck size={14} /> : <IconClose size={14} />}{" "}
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
                    placeholder={t("ws.create.env.key.placeholder")}
                    value={row.key}
                    onInput={(e) => {
                      const next = [...envRows()];
                      next[i()] = { ...next[i()], key: e.currentTarget.value };
                      setEnvRows(next);
                    }}
                  />
                  <span class="env-eq">=</span>
                  <input
                    placeholder={t("ws.create.env.value.placeholder")}
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
                    title={t("ws.create.env.remove")}
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

          </div>
          <div class="modal-buttons ws-create-modal-footer">
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
