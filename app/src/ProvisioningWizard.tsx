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

interface StepProgress {
  run_id: string;
  step_index: number;
  step_kind: string;
  state: "running" | "done" | "failed" | "skipped";
  log_chunk: string;
  message?: string | null;
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

  const startRun = async () => {
    setWizStep("execute");
    setLogLines([]);
    setStepStates({});
    setResult(null);
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
            {/* Step 1: connect + inspect */}
            <Show when={wizStep() === "connect"}>
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

              <div class="modal-buttons">
                <button onClick={p.onClose}>{t("common.cancel")}</button>
                <button
                  class="primary"
                  disabled={inspecting() || !host() || !user()}
                  onClick={() => void runInspect()}
                >
                  {inspecting() ? t("provisioning.inspecting") : t("provisioning.btn.connect_inspect")}
                </button>
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
                        <Show when={s()?.message || s()?.log_chunk}>
                          <pre class="provisioning-step-log">
                            {s()?.message ? `${s()?.message}\n` : ""}
                            {s()?.log_chunk ?? ""}
                          </pre>
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
