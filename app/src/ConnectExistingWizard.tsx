import { createSignal, createMemo, For, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { t } from "./i18n";
import type { ServerDiscovery } from "./bindings/ServerDiscovery";
import type { ConnectExistingInput } from "./bindings/ConnectExistingInput";
import type { ConnectExistingResult } from "./bindings/ConnectExistingResult";

// Phase 65.C: "Connect to existing server" multi-machine SSH onboarding
// wizard. The Rust backend (connect_existing_discover /
// connect_existing_execute) does the heavy lifting; this component is the
// auth → discover → choose → execute → result flow over those two
// commands.

interface Props {
  open: boolean;
  onClose: () => void;
  /**
   * When set, the wizard runs in "add this machine to an existing
   * workspace" mode: existing_workspace_id is passed through to execute
   * instead of creating a new workspace.
   */
  existingWorkspaceId?: string;
  /** Optional prefill for the auth step (e.g. a known host/user). */
  prefill?: { host?: string; port?: number; user?: string };
  /** Called on success so the parent can activate the new workspace. */
  onCreated: (workspaceId: string) => void;
}

export function ConnectExistingWizard(p: Props) {
  type WizardStep = "auth" | "choose" | "result";
  const [step, setStep] = createSignal<WizardStep>("auth");

  // Auth-step inputs. The password is sensitive (Rule #2): it lives only
  // in this signal, is never logged, and is reused for the execute call.
  const [host, setHost] = createSignal(p.prefill?.host ?? "");
  const [port, setPort] = createSignal(p.prefill?.port ?? 22);
  const [user, setUser] = createSignal(p.prefill?.user ?? "");
  const [password, setPassword] = createSignal("");

  const [discovering, setDiscovering] = createSignal(false);
  const [authError, setAuthError] = createSignal<string | null>(null);
  const [discovery, setDiscovery] = createSignal<ServerDiscovery | null>(null);

  // Choose-step state.
  type Mode = "existing" | "new";
  const [mode, setMode] = createSignal<Mode>("existing");
  const [existingUser, setExistingUser] = createSignal("");
  const [newUser, setNewUser] = createSignal("winmux-user");
  const [grantSudo, setGrantSudo] = createSignal(true);
  const [workspaceName, setWorkspaceName] = createSignal("");

  // Execute-step state.
  const [executing, setExecuting] = createSignal(false);
  const [execError, setExecError] = createSignal<string | null>(null);
  const [result, setResult] = createSignal<ConnectExistingResult | null>(null);

  const isAddMode = (): boolean => p.existingWorkspaceId !== undefined;

  // Friendly error mapping: the backend surfaces raw SSH errors; turn the
  // common "auth failed" case into actionable guidance.
  const friendlyError = (raw: string): string => {
    const low = raw.toLowerCase();
    if (
      low.includes("authentication") ||
      low.includes("auth failed") ||
      low.includes("permission denied") ||
      low.includes("password")
    ) {
      return t("connectExisting.error.authFailed");
    }
    return raw;
  };

  const runDiscover = async () => {
    setDiscovering(true);
    setAuthError(null);
    setDiscovery(null);
    try {
      const d = await invoke<ServerDiscovery>("connect_existing_discover", {
        host: host().trim(),
        port: port(),
        user: user().trim(),
        password: password(),
      });
      setDiscovery(d);
      // Seed the choose-step defaults from what we found.
      setExistingUser(d.users[0] ?? user().trim());
      if (!d.can_sudo) setMode("existing");
      setStep("choose");
    } catch (e) {
      setAuthError(friendlyError(String(e)));
    } finally {
      setDiscovering(false);
    }
  };

  // The user account the run targets, derived from the chosen mode.
  const targetUser = createMemo<string>(() =>
    mode() === "new" ? newUser().trim() : existingUser().trim()
  );

  const canExecute = createMemo<boolean>(() => {
    if (executing()) return false;
    if (mode() === "new") {
      return newUser().trim().length > 0 && (discovery()?.can_sudo ?? false);
    }
    return existingUser().trim().length > 0;
  });

  const runExecute = async () => {
    const d = discovery();
    if (!d) return;
    setExecuting(true);
    setExecError(null);
    try {
      const input: ConnectExistingInput = {
        host: host().trim(),
        port: port(),
        auth_user: user().trim(),
        password: password(),
        target_user: targetUser(),
        create_new_user: mode() === "new",
        grant_sudo: mode() === "new" ? grantSudo() : false,
        sudo_group: d.sudo_group,
        workspace_name: isAddMode() ? null : workspaceName().trim() || null,
        existing_workspace_id: p.existingWorkspaceId ?? null,
      };
      const r = await invoke<ConnectExistingResult>("connect_existing_execute", {
        input,
      });
      setResult(r);
      setStep("result");
    } catch (e) {
      setExecError(friendlyError(String(e)));
    } finally {
      setExecuting(false);
    }
  };

  const finish = () => {
    const r = result();
    if (r) p.onCreated(r.workspace_id);
    p.onClose();
  };

  const stepLabel = createMemo<string>(() => {
    switch (step()) {
      case "auth":
        return t("connectExisting.step.auth");
      case "choose":
        return t("connectExisting.step.choose");
      case "result":
        return t("connectExisting.step.result");
    }
  });

  return (
    <Show when={p.open}>
      <div class="modal-backdrop" onClick={p.onClose}>
        <div
          class="modal provisioning-modal"
          onClick={(e) => e.stopPropagation()}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <div class="provisioning-head">
            <h3>{t("connectExisting.title")}</h3>
            <span class="provisioning-step-indicator">{stepLabel()}</span>
            <button class="feed-x" title={t("common.close")} onClick={p.onClose}>
              ×
            </button>
          </div>

          <div class="provisioning-body">
            {/* Step 1: auth + discover */}
            <Show when={step() === "auth"}>
              <p class="settings-hint">{t("connectExisting.auth.hint")}</p>
              <label>
                <span>{t("connectExisting.field.host")}</span>
                <input
                  value={host()}
                  onInput={(e) => setHost(e.currentTarget.value)}
                  placeholder="1.2.3.4"
                />
              </label>
              <label>
                <span>{t("connectExisting.field.port")}</span>
                <input
                  type="number"
                  value={port()}
                  onInput={(e) => setPort(parseInt(e.currentTarget.value) || 22)}
                />
              </label>
              <label>
                <span>{t("connectExisting.field.user")}</span>
                <input
                  value={user()}
                  onInput={(e) => setUser(e.currentTarget.value)}
                  placeholder="root"
                />
              </label>
              <label>
                <span>{t("connectExisting.field.password")}</span>
                <input
                  type="password"
                  value={password()}
                  onInput={(e) => setPassword(e.currentTarget.value)}
                  placeholder={t("connectExisting.field.password.placeholder")}
                />
              </label>

              <Show when={authError()}>
                <div class="wizard-test-result err">
                  <div class="wizard-test-line">✗ {authError()}</div>
                </div>
              </Show>

              <div class="modal-buttons">
                <button onClick={p.onClose}>{t("common.cancel")}</button>
                <button
                  class="primary"
                  disabled={discovering() || !host() || !user() || !password()}
                  onClick={() => void runDiscover()}
                >
                  {discovering()
                    ? t("connectExisting.discovering")
                    : t("connectExisting.btn.discover")}
                </button>
              </div>
            </Show>

            {/* Step 2: choose target user + execute */}
            <Show when={step() === "choose" && discovery()}>
              {(() => {
                const d = discovery()!;
                return (
                  <>
                    <div class="wizard-test-result ok">
                      <div class="wizard-test-line">
                        ✓{" "}
                        {d.is_root
                          ? t("connectExisting.status.root")
                          : d.can_sudo
                            ? t("connectExisting.status.sudo")
                            : t("connectExisting.status.plain")}
                      </div>
                    </div>

                    <label class="provisioning-mode-row">
                      <input
                        type="radio"
                        name="connect-existing-mode"
                        value="existing"
                        checked={mode() === "existing"}
                        onChange={() => setMode("existing")}
                      />
                      <span>
                        <strong>{t("connectExisting.mode.existing")}</strong>
                        <span class="provisioning-mode-hint">
                          {t("connectExisting.mode.existing.hint")}
                        </span>
                      </span>
                    </label>
                    <Show when={mode() === "existing"}>
                      <label>
                        <span>{t("connectExisting.field.existingUser")}</span>
                        <select
                          value={existingUser()}
                          onChange={(e) => setExistingUser(e.currentTarget.value)}
                        >
                          <For each={d.users}>
                            {(u) => <option value={u}>{u}</option>}
                          </For>
                        </select>
                      </label>
                    </Show>

                    <label class="provisioning-mode-row">
                      <input
                        type="radio"
                        name="connect-existing-mode"
                        value="new"
                        checked={mode() === "new"}
                        disabled={!d.can_sudo}
                        onChange={() => setMode("new")}
                      />
                      <span>
                        <strong>{t("connectExisting.mode.new")}</strong>
                        <span class="provisioning-mode-hint">
                          {t("connectExisting.mode.new.hint")}
                        </span>
                      </span>
                    </label>
                    <Show when={!d.can_sudo}>
                      <p class="settings-hint">
                        {t("connectExisting.mode.new.needsSudo")}
                      </p>
                    </Show>
                    <Show when={mode() === "new" && d.can_sudo}>
                      <label>
                        <span>{t("connectExisting.field.newUser")}</span>
                        <input
                          value={newUser()}
                          onInput={(e) => setNewUser(e.currentTarget.value)}
                          placeholder="winmux-user"
                        />
                      </label>
                      <label class="provisioning-mode-row">
                        <input
                          type="checkbox"
                          checked={grantSudo()}
                          onChange={(e) => setGrantSudo(e.currentTarget.checked)}
                        />
                        <span>
                          {t("connectExisting.field.grantSudo", {
                            group: d.sudo_group,
                          })}
                        </span>
                      </label>
                    </Show>

                    {/* New-workspace name — only when not adding to an
                        existing workspace. */}
                    <Show when={!isAddMode()}>
                      <label>
                        <span>{t("connectExisting.field.workspaceName")}</span>
                        <input
                          value={workspaceName()}
                          onInput={(e) => setWorkspaceName(e.currentTarget.value)}
                          placeholder={t(
                            "connectExisting.field.workspaceName.placeholder"
                          )}
                        />
                      </label>
                    </Show>

                    {/* Confirm summary of what the Connect button will do. */}
                    <p class="settings-hint">
                      {mode() === "new"
                        ? grantSudo()
                          ? t("connectExisting.confirm.newUserSudo", {
                              user: targetUser(),
                            })
                          : t("connectExisting.confirm.newUser", {
                              user: targetUser(),
                            })
                        : t("connectExisting.confirm.existingUser", {
                            user: targetUser(),
                          })}
                    </p>

                    <Show when={execError()}>
                      <div class="wizard-test-result err">
                        <div class="wizard-test-line">✗ {execError()}</div>
                      </div>
                    </Show>

                    <div class="modal-buttons">
                      <button onClick={() => setStep("auth")}>
                        {t("connectExisting.btn.back")}
                      </button>
                      <button
                        class="primary"
                        disabled={!canExecute()}
                        onClick={() => void runExecute()}
                      >
                        {executing()
                          ? t("connectExisting.connecting")
                          : t("connectExisting.btn.connect")}
                      </button>
                    </div>
                  </>
                );
              })()}
            </Show>

            {/* Step 3: result */}
            <Show when={step() === "result" && result()}>
              <div class="wizard-test-result ok">
                <div class="wizard-test-line">
                  ✓{" "}
                  {t("connectExisting.result.ready", {
                    name: result()!.workspace_name,
                  })}
                </div>
                <div class="wizard-test-meta">
                  {t("connectExisting.result.keyPath", {
                    path: result()!.key_path,
                  })}
                </div>
              </div>
              <div class="modal-buttons">
                <button onClick={p.onClose}>{t("common.close")}</button>
                <button class="primary" onClick={finish}>
                  {t("connectExisting.btn.createPane")}
                </button>
              </div>
            </Show>
          </div>
        </div>
      </div>
    </Show>
  );
}
