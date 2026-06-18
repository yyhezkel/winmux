import { createSignal, For, Show, onMount, onCleanup, createMemo } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { t } from "./i18n";

// Phase 14.A type mirrors — see src-tauri/src/provisioning.rs.
interface InspectResult {
  ok: boolean;
  message?: string | null;
  uname?: string | null;
  os_pretty_name?: string | null;
  os_id?: string | null;
  os_version?: string | null;
  package_manager?: string | null;
  whoami?: string | null;
  df_h?: string | null;
}

interface ProvisioningProfile {
  id: string;
  label: string;
  steps: string[];
}

interface ProfilesFile {
  version: number;
  profiles: ProvisioningProfile[];
}

// Phase 32.A: structured error variant carried alongside `message`.
// When present, drives a dedicated UI (sudo-required modal, step
// stderr block) instead of the generic message line.
type ProvisioningError =
  | { kind: "SudoRequired"; details: { user: string; raw_stderr: string } }
  | {
      kind: "StepFailed";
      details: { step: string; exit_code: number; stderr: string };
    }
  | { kind: "Generic"; details: string };

interface StepProgress {
  run_id: string;
  step_index: number;
  step_kind: string;
  state: "running" | "done" | "failed" | "skipped";
  log_chunk: string;
  message?: string | null;
  error?: ProvisioningError | null;
  timestamp_iso: string;
}

interface RunHandle {
  run_id: string;
}

interface ProvisionResult {
  run_id: string;
  workspace_id?: string | null;
  workspace_name?: string | null;
  claude_installed: boolean;
  host: string;
}

interface Props {
  open: boolean;
  onClose: () => void;
  /** workspace id to associate the provisioning run with (for secret store). */
  workspaceId?: string;
  /** Phase 14.A.2: called when the wizard reaches Done with a created
   *  workspace and the user clicks "Open it now". `claude` = open in
   *  Claude Code mode (Smart Connect's claude option). */
  onOpenWorkspace?: (workspaceId: string, mode: "default" | "claude") => void;
}

export function ProvisioningWizard(p: Props) {
  type WizardStep = "connect" | "configure" | "execute" | "done";
  const [wizStep, setWizStep] = createSignal<WizardStep>("connect");

  // Phase 56-B: mode picker at step 1.
  //   "new"      → full provisioning flow (existing 4-step wizard).
  //   "existing" → minimal flow for an already-set-up server: install
  //                an SSH key + create a workspace, no user creation,
  //                no sudo, no agent install. One backend command
  //                (provision_existing_install_key) does steps 2-5.
  type WizardMode = "new" | "existing";
  const [mode, setMode] = createSignal<WizardMode>("new");
  const [existingBusy, setExistingBusy] = createSignal(false);
  const [existingError, setExistingError] = createSignal<string | null>(null);

  // Connect-step inputs.
  const [host, setHost] = createSignal("");
  const [port, setPort] = createSignal(22);
  const [user, setUser] = createSignal("root");
  const [password, setPassword] = createSignal("");
  const [keyPath, setKeyPath] = createSignal("");
  const [keyPass, setKeyPass] = createSignal("");
  const [inspecting, setInspecting] = createSignal(false);
  const [inspect, setInspect] = createSignal<InspectResult | null>(null);

  // Configure-step inputs.
  const [profiles, setProfiles] = createSignal<ProvisioningProfile[]>([]);
  const [profileId, setProfileId] = createSignal("default");
  const [stepCatalog, setStepCatalog] = createSignal<[string, string][]>([]);
  const [newUser, setNewUser] = createSignal("runner");
  const [customSteps, setCustomSteps] = createSignal<string[]>([]);
  // Phase 14.A.2: editable workspace name. Defaults to a host-derived
  // label like "myserver" from "myserver.com" — recomputed whenever
  // the host changes (only if the user hasn't manually typed
  // something).
  const [workspaceName, setWorkspaceName] = createSignal("");
  const [workspaceNameTouched, setWorkspaceNameTouched] = createSignal(false);
  const deriveWorkspaceName = (h: string): string => {
    const trimmed = h.trim();
    if (!trimmed) return "";
    const head = trimmed.split(".")[0];
    if (/^[0-9.]+$/.test(trimmed)) return trimmed;
    return head;
  };
  // Auto-fill when the host changes (Connect step input) — only if the
  // user hasn't typed their own name yet.
  createMemo(() => {
    if (!workspaceNameTouched()) {
      setWorkspaceName(deriveWorkspaceName(host()));
    }
  });

  // Execute-step state.
  const [runId, setRunId] = createSignal<string | null>(null);
  // logLines is kept for future "full transcript" view; not currently
  // rendered (we surface per-step output via stepStates).
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  const [_logLines, setLogLines] = createSignal<StepProgress[]>([]);
  const [stepStates, setStepStates] = createSignal<Record<number, StepProgress>>({});
  // Phase 32.A: when the preflight failed with SudoRequired, capture
  // it so we can render the dedicated modal block above the step list
  // instead of silently failing at the first step.
  const [preflightError, setPreflightError] = createSignal<ProvisioningError | null>(null);
  // Phase 14.A.2 — completion payload from the backend's
  // `provisioning:complete` event. When this carries a workspace_id
  // the Done step renders the "Open it now" buttons.
  const [result, setResult] = createSignal<ProvisionResult | null>(null);

  let unlisten: UnlistenFn | null = null;
  let unlistenComplete: UnlistenFn | null = null;
  onMount(async () => {
    try {
      const pf = await invoke<ProfilesFile>("provisioning_profiles_list");
      setProfiles(pf.profiles);
    } catch (e) {
      console.warn("provisioning_profiles_list failed", e);
    }
    try {
      const cat = await invoke<[string, string][]>("provisioning_step_catalog");
      setStepCatalog(cat);
    } catch (e) {
      console.warn("provisioning_step_catalog failed", e);
    }
    unlisten = await listen<StepProgress>("provisioning:progress", (e) => {
      setLogLines((prev) => [...prev, e.payload]);
      setStepStates((prev) => ({ ...prev, [e.payload.step_index]: e.payload }));
      // Phase 32.A: the preflight failure rides on the same event
      // channel with step_kind === "Preflight". Hoist it to a
      // dedicated banner so the user gets the actionable
      // /etc/sudoers hint immediately, not buried in step 0.
      if (
        e.payload.state === "failed" &&
        e.payload.error?.kind === "SudoRequired"
      ) {
        setPreflightError(e.payload.error);
      }
    });
    // Phase 14.A.2: the backend fires this once after the per-step
    // loop finishes. We snapshot the payload and auto-advance to the
    // Done step so the user immediately sees the CTAs.
    unlistenComplete = await listen<ProvisionResult>("provisioning:complete", (e) => {
      setResult(e.payload);
      setWizStep("done");
    });
  });
  onCleanup(() => {
    if (unlisten) unlisten();
    if (unlistenComplete) unlistenComplete();
  });

  const activeProfile = createMemo<ProvisioningProfile | null>(() =>
    profiles().find((p) => p.id === profileId()) ?? null
  );
  const effectiveSteps = createMemo<string[]>(() =>
    customSteps().length > 0 ? customSteps() : activeProfile()?.steps ?? []
  );

  const runInspect = async () => {
    setInspecting(true);
    setInspect(null);
    try {
      const r = await invoke<InspectResult>("provisioning_inspect", {
        host: host().trim(),
        port: port(),
        user: user().trim(),
        password: password() || null,
        keyPath: keyPath() || null,
        keyPassphrase: keyPass() || null,
      });
      setInspect(r);
      if (r.ok) {
        // Pre-load custom steps from the active profile so the
        // configure-step checkboxes already show the right set.
        setCustomSteps(activeProfile()?.steps ?? []);
        setWizStep("configure");
      }
    } catch (e) {
      setInspect({ ok: false, message: String(e) });
    } finally {
      setInspecting(false);
    }
  };

  // Phase 56-B: existing-server mode. One backend call does cred-test
  // + keygen + remote install + verify-with-key + persist-workspace.
  // On success, we synthesize a `result()` payload that drives the
  // existing Done step UI (with the "Open it now" button).
  const runExistingInstallKey = async () => {
    setExistingBusy(true);
    setExistingError(null);
    try {
      const newWorkspaceId = await invoke<string>(
        "provision_existing_install_key",
        {
          host: host().trim(),
          port: port(),
          sshUser: user().trim(),
          password: password(),
          workspaceName: workspaceName().trim(),
        }
      );
      setResult({
        run_id: "existing-install",
        workspace_id: newWorkspaceId,
        workspace_name: workspaceName().trim() || host().trim(),
        claude_installed: false,
        host: host().trim(),
      });
      setWizStep("done");
    } catch (e) {
      setExistingError(String(e));
    } finally {
      setExistingBusy(false);
    }
  };

  const startRun = async () => {
    setWizStep("execute");
    setLogLines([]);
    setStepStates({});
    setResult(null);
    setPreflightError(null);
    try {
      const handle = await invoke<RunHandle>("provisioning_start", {
        input: {
          workspace_id: p.workspaceId ?? `prov-${Date.now()}`,
          host: host().trim(),
          port: port(),
          initial_user: user().trim(),
          initial_password: password() || null,
          initial_key_path: keyPath() || null,
          initial_key_passphrase: keyPass() || null,
          new_user: newUser().trim() || "runner",
          local_key_path: null,
          profile_id: profileId(),
          workspace_name: workspaceName().trim() || null,
          // When the wizard was launched against an existing workspace
          // (right-click → Run provisioning, future), the caller
          // passes its id through `p.workspaceId`. Backend will rewrite
          // that workspace's connection rather than creating a new one.
          existing_workspace_id: p.workspaceId ?? null,
        },
      });
      setRunId(handle.run_id);
    } catch (e) {
      setLogLines([
        {
          run_id: "",
          step_index: 0,
          step_kind: "spawn",
          state: "failed",
          log_chunk: "",
          message: String(e),
          timestamp_iso: new Date().toISOString(),
        },
      ]);
    }
  };

  const toggleStep = (s: string) => {
    const cur = customSteps().length > 0 ? customSteps() : activeProfile()?.steps ?? [];
    if (cur.includes(s)) setCustomSteps(cur.filter((x) => x !== s));
    else setCustomSteps([...cur, s]);
  };

  const stateBadge = (s?: StepProgress) => {
    if (!s) return { cls: "pending", icon: "○" };
    if (s.state === "done") return { cls: "ok", icon: "✓" };
    if (s.state === "failed") return { cls: "err", icon: "✗" };
    if (s.state === "running") return { cls: "running", icon: "…" };
    return { cls: "pending", icon: "○" };
  };

  return (
    <Show when={p.open}>
      <div class="modal-backdrop" onClick={p.onClose}>
        <div
          class="modal provisioning-modal"
          onClick={(e) => e.stopPropagation()}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <div class="provisioning-head">
            <h3>{t("provisioning.title")}</h3>
            <span class="provisioning-step-indicator">
              {wizStep() === "connect" && t("provisioning.step.connect")}
              {wizStep() === "configure" && t("provisioning.step.configure")}
              {wizStep() === "execute" && t("provisioning.step.execute")}
              {wizStep() === "done" && t("provisioning.step.done")}
            </span>
            <button class="feed-x" title={t("common.close")} onClick={p.onClose}>×</button>
          </div>

          <div class="provisioning-body">
            {/* Step 1: connect + inspect (new mode) OR install key
                + create workspace (existing mode). The fields up to
                "password" are shared; key_path + configure/execute
                steps are new-mode only. */}
            <Show when={wizStep() === "connect"}>
              {/* Phase 56-B: mode selector. Default is "new" so the
                  legacy provisioning flow keeps the same first-click
                  behaviour. */}
              <div class="provisioning-mode">
                <p class="settings-hint">{t("provisioning.mode.label")}</p>
                <label class="provisioning-mode-row">
                  <input
                    type="radio"
                    name="provisioning-mode"
                    value="new"
                    checked={mode() === "new"}
                    onChange={() => setMode("new")}
                  />
                  <span>
                    <strong>{t("provisioning.mode.new")}</strong>
                    <span class="provisioning-mode-hint">{t("provisioning.mode.new.hint")}</span>
                  </span>
                </label>
                <label class="provisioning-mode-row">
                  <input
                    type="radio"
                    name="provisioning-mode"
                    value="existing"
                    checked={mode() === "existing"}
                    onChange={() => setMode("existing")}
                  />
                  <span>
                    <strong>{t("provisioning.mode.existing")}</strong>
                    <span class="provisioning-mode-hint">{t("provisioning.mode.existing.hint")}</span>
                  </span>
                </label>
              </div>
              <p class="settings-hint">{t("provisioning.hint.connect")}</p>
              <label>
                <span>{t("provisioning.field.host")}</span>
                <input
                  value={host()}
                  onInput={(e) => setHost(e.currentTarget.value)}
                  placeholder="1.2.3.4"
                />
              </label>
              <label>
                <span>{t("provisioning.field.port")}</span>
                <input
                  type="number"
                  value={port()}
                  onInput={(e) => setPort(parseInt(e.currentTarget.value) || 22)}
                />
              </label>
              <label>
                <span>{t("provisioning.field.initial_user")}</span>
                <input
                  value={user()}
                  onInput={(e) => setUser(e.currentTarget.value)}
                />
              </label>
              <label>
                <span>{t("provisioning.field.password")}</span>
                <input
                  type="password"
                  value={password()}
                  onInput={(e) => setPassword(e.currentTarget.value)}
                  placeholder={t("provisioning.field.password.placeholder")}
                />
              </label>
              {/* Phase 56-B: key_path / key_passphrase inputs are
                  new-mode only. Existing mode generates a fresh
                  ed25519 keypair itself — accepting a user-provided
                  one in that flow would conflict with the install
                  step's "write a brand-new pubkey to authorized_keys"
                  guarantee. */}
              <Show when={mode() === "new"}>
                <label>
                  <span>{t("provisioning.field.key_path")}</span>
                  <input
                    value={keyPath()}
                    onInput={(e) => setKeyPath(e.currentTarget.value)}
                    placeholder={t("provisioning.field.key_path.placeholder")}
                  />
                </label>
                <Show when={keyPath()}>
                  <label>
                    <span>{t("provisioning.field.key_passphrase")}</span>
                    <input
                      type="password"
                      value={keyPass()}
                      onInput={(e) => setKeyPass(e.currentTarget.value)}
                    />
                  </label>
                </Show>
              </Show>

              {/* Phase 56-B: workspace name lives on step 1 in existing
                  mode (no configure step to host it). Auto-derived
                  from host() via the same memo as new-mode. */}
              <Show when={mode() === "existing"}>
                <label>
                  <span>{t("provisioning.field.workspace_name")}</span>
                  <input
                    value={workspaceName()}
                    placeholder={t("provisioning.field.workspace_name.placeholder")}
                    onInput={(e) => {
                      setWorkspaceName(e.currentTarget.value);
                      setWorkspaceNameTouched(true);
                    }}
                  />
                </label>
              </Show>

              <Show when={inspect()}>
                <div
                  class={`wizard-test-result ${inspect()!.ok ? "ok" : "err"}`}
                >
                  <Show when={inspect()!.ok}>
                    <div class="wizard-test-line">
                      ✓ {inspect()!.os_pretty_name ?? t("provisioning.inspect.os_detected")}
                    </div>
                    <div class="wizard-test-meta">
                      {t("provisioning.inspect.meta", {
                        pm: inspect()!.package_manager ?? "?",
                        who: inspect()!.whoami?.trim() ?? "?",
                      })}
                    </div>
                    <Show when={inspect()!.df_h}>
                      <div class="wizard-test-meta">
                        {t("provisioning.inspect.disk", { df: inspect()!.df_h!.trim() })}
                      </div>
                    </Show>
                  </Show>
                  <Show when={!inspect()!.ok}>
                    <div class="wizard-test-line">✗ {inspect()!.message}</div>
                  </Show>
                </div>
              </Show>

              {/* Phase 56-B: existing-mode error banner. Surfaces
                  ssh-keygen / connect / install / verify failures
                  inline; the next click retries with the same form. */}
              <Show when={mode() === "existing" && existingError()}>
                <div class="wizard-test-result err">
                  <div class="wizard-test-line">✗ {existingError()}</div>
                </div>
              </Show>

              <div class="modal-buttons">
                <button onClick={p.onClose}>{t("common.cancel")}</button>
                <Show
                  when={mode() === "new"}
                  fallback={
                    <button
                      class="primary"
                      disabled={
                        existingBusy() ||
                        !host() ||
                        !user() ||
                        !password()
                      }
                      onClick={() => void runExistingInstallKey()}
                    >
                      {existingBusy()
                        ? t("provisioning.existing.step.install")
                        : t("provisioning.btn.existing_install_key")}
                    </button>
                  }
                >
                  <button
                    class="primary"
                    disabled={inspecting() || !host() || !user()}
                    onClick={() => void runInspect()}
                  >
                    {inspecting()
                      ? t("provisioning.inspecting")
                      : t("provisioning.btn.connect_inspect")}
                  </button>
                </Show>
              </div>
            </Show>

            {/* Step 2: configure */}
            <Show when={wizStep() === "configure"}>
              <p class="settings-hint">
                {t("provisioning.target", {
                  host: host(),
                  os: inspect()?.os_pretty_name ?? "OS",
                  pm: inspect()?.package_manager ?? "?",
                })}
              </p>
              <label>
                <span>{t("provisioning.field.profile")}</span>
                <select
                  value={profileId()}
                  onChange={(e) => {
                    setProfileId(e.currentTarget.value);
                    setCustomSteps(
                      profiles().find((p) => p.id === e.currentTarget.value)?.steps ?? []
                    );
                  }}
                >
                  <For each={profiles()}>
                    {(pf) => <option value={pf.id}>{pf.label}</option>}
                  </For>
                </select>
              </label>
              <label>
                <span>{t("provisioning.field.new_user")}</span>
                <input
                  value={newUser()}
                  onInput={(e) => setNewUser(e.currentTarget.value)}
                />
              </label>
              <label>
                <span>{t("provisioning.field.workspace_name")}</span>
                <input
                  value={workspaceName()}
                  placeholder={t("provisioning.field.workspace_name.placeholder")}
                  onInput={(e) => {
                    setWorkspaceName(e.currentTarget.value);
                    setWorkspaceNameTouched(true);
                  }}
                />
              </label>
              <h4 class="provisioning-h4">{t("provisioning.steps.title")}</h4>
              <div class="provisioning-steps">
                <For each={stepCatalog()}>
                  {([id, label], idx) => {
                    // Phase 14.A.2: visually separate AI-agent install
                    // steps from the rest, since they're an opt-in
                    // group that usually moves together. The dividing
                    // header appears right before the first
                    // InstallClaudeCode entry.
                    const isFirstAgent = id === "InstallClaudeCode" &&
                      !stepCatalog().slice(0, idx()).some(([s]) =>
                        s === "InstallClaudeCode"
                      );
                    // Use i18n labels where we have one; fall back to
                    // the backend's English label (which itself is the
                    // source of truth for unknown step ids).
                    const i18nKey =
                      id === "InstallClaudeCode" ? "provisioning.step.install_claude.label"
                      : id === "InstallCodex" ? "provisioning.step.install_codex.label"
                      : id === "InstallGemini" ? "provisioning.step.install_gemini.label"
                      : id === "AddWinmuxToPath" ? "provisioning.step.add_winmux_to_path.label"
                      : null;
                    const display = i18nKey ? t(i18nKey) : label;
                    return (
                      <>
                        {isFirstAgent && (
                          <div class="provisioning-section-header">
                            <strong>{t("provisioning.agents.section_title")}</strong>
                            <span>{t("provisioning.agents.help")}</span>
                          </div>
                        )}
                        <label class="provisioning-step-row">
                          <input
                            type="checkbox"
                            checked={effectiveSteps().includes(id)}
                            onChange={() => toggleStep(id)}
                          />
                          <span>{display}</span>
                        </label>
                      </>
                    );
                  }}
                </For>
              </div>
              <div class="modal-buttons">
                <button onClick={() => setWizStep("connect")}>{t("provisioning.btn.back")}</button>
                <button class="primary" onClick={() => void startRun()}>
                  {t("provisioning.btn.execute")}
                </button>
              </div>
            </Show>

            {/* Step 3: execute */}
            <Show when={wizStep() === "execute"}>
              <p class="settings-hint">
                {t("provisioning.run.label", {
                  id: runId() ?? t("provisioning.run.starting"),
                  host: host(),
                })}
              </p>
              {/* Phase 32.A: SudoRequired banner. Renders BEFORE the
                  step list so the user sees the actionable hint
                  immediately. The /etc/sudoers line is copy-pasteable
                  so they don't have to type it. */}
              <Show when={preflightError()?.kind === "SudoRequired"}>
                {(() => {
                  const err = preflightError() as Extract<
                    ProvisioningError,
                    { kind: "SudoRequired" }
                  >;
                  const line = `${err.details.user} ALL=(ALL) NOPASSWD: ALL`;
                  return (
                    <div class="prov-error-card prov-error-sudo">
                      <div class="prov-error-title">{t("prov.error.sudoRequired.title")}</div>
                      <p class="prov-error-body">{t("prov.error.sudoRequired.body")}</p>
                      <p class="prov-error-hint">{t("prov.error.sudoRequired.hint")}</p>
                      <code class="prov-error-line">{line}</code>
                      <div class="prov-error-actions">
                        <button
                          onClick={async () => {
                            try {
                              await navigator.clipboard.writeText(line);
                            } catch (e) {
                              console.warn("clipboard write failed", e);
                            }
                          }}
                        >
                          {t("prov.error.copy")}
                        </button>
                        <button
                          class="primary"
                          onClick={() => {
                            setPreflightError(null);
                            setStepStates({});
                            void startRun();
                          }}
                        >
                          {t("prov.error.retry")}
                        </button>
                      </div>
                      <Show when={err.details.raw_stderr}>
                        <details class="prov-error-raw">
                          <summary>{t("prov.error.rawStderr")}</summary>
                          <pre>{err.details.raw_stderr}</pre>
                        </details>
                      </Show>
                    </div>
                  );
                })()}
              </Show>
              <div class="provisioning-step-list">
                <For each={effectiveSteps()}>
                  {(stepId, idx) => {
                    const s = createMemo(() => stepStates()[idx()]);
                    const b = createMemo(() => stateBadge(s()));
                    const labelFor = stepCatalog().find(([id]) => id === stepId)?.[1] ?? stepId;
                    return (
                      <div class={`provisioning-step-card state-${b().cls}`}>
                        <div class="provisioning-step-head">
                          <span class={`provisioning-step-icon ${b().cls}`}>{b().icon}</span>
                          <span class="provisioning-step-label">{labelFor}</span>
                        </div>
                        {/* Phase 32.A: StepFailed gets a dedicated
                            block with the step name, exit code, and
                            stderr expanded (not behind "show details"). */}
                        <Show
                          when={
                            s()?.error?.kind === "StepFailed"
                          }
                          fallback={
                            <Show when={s()?.message || s()?.log_chunk}>
                              <pre class="provisioning-step-log">
                                {s()?.message ? `${s()?.message}\n` : ""}
                                {s()?.log_chunk ?? ""}
                              </pre>
                            </Show>
                          }
                        >
                          {(() => {
                            const sf = s()!.error as Extract<
                              ProvisioningError,
                              { kind: "StepFailed" }
                            >;
                            return (
                              <div class="prov-step-failed">
                                <div class="prov-step-failed-head">
                                  {t("prov.error.stepFailed.title", { step: sf.details.step })}
                                  <span class="prov-step-exit">exit {sf.details.exit_code}</span>
                                </div>
                                <pre class="prov-step-stderr">{sf.details.stderr}</pre>
                              </div>
                            );
                          })()}
                        </Show>
                      </div>
                    );
                  }}
                </For>
              </div>
              <div class="modal-buttons">
                <button onClick={() => setWizStep("done")}>{t("provisioning.btn.mark_done")}</button>
              </div>
            </Show>

            <Show when={wizStep() === "done"}>
              <p>{t("provisioning.done.message")}</p>
              <Show
                when={result() && result()!.workspace_id}
                fallback={
                  <Show when={result() && !result()!.workspace_id}>
                    <div class="wizard-test-result err">
                      <div class="wizard-test-line">
                        {t("provisioning.done.workspace_skipped")}
                      </div>
                    </div>
                  </Show>
                }
              >
                <div class="wizard-test-result ok">
                  <div class="wizard-test-line">
                    {t("provisioning.done.workspace_created", {
                      name: result()!.workspace_name ?? "",
                    })}
                  </div>
                </div>
              </Show>
              <div class="modal-buttons">
                <Show when={result()?.workspace_id}>
                  <Show when={result()?.claude_installed}>
                    <button
                      onClick={() => {
                        p.onOpenWorkspace?.(result()!.workspace_id!, "claude");
                        p.onClose();
                      }}
                    >
                      {t("provisioning.done.btn.open_with_claude")}
                    </button>
                  </Show>
                  <button
                    class="primary"
                    onClick={() => {
                      p.onOpenWorkspace?.(result()!.workspace_id!, "default");
                      p.onClose();
                    }}
                  >
                    {t("provisioning.done.btn.open_now")}
                  </button>
                </Show>
                <Show when={!result()?.workspace_id}>
                  <button class="primary" onClick={p.onClose}>{t("common.close")}</button>
                </Show>
              </div>
            </Show>
          </div>
        </div>
      </div>
    </Show>
  );
}
