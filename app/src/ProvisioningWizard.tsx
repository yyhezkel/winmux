import { createSignal, For, Show, onMount, onCleanup, createMemo } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

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

interface Props {
  open: boolean;
  onClose: () => void;
  /** workspace id to associate the provisioning run with (for secret store). */
  workspaceId?: string;
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

  // Execute-step state.
  const [runId, setRunId] = createSignal<string | null>(null);
  // logLines is kept for future "full transcript" view; not currently
  // rendered (we surface per-step output via stepStates).
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  const [_logLines, setLogLines] = createSignal<StepProgress[]>([]);
  const [stepStates, setStepStates] = createSignal<Record<number, StepProgress>>({});

  let unlisten: UnlistenFn | null = null;
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
  });
  onCleanup(() => {
    if (unlisten) unlisten();
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
            <h3>Provision new server</h3>
            <span class="provisioning-step-indicator">
              {wizStep() === "connect" && "1 / 4 · Connect"}
              {wizStep() === "configure" && "2 / 4 · Configure"}
              {wizStep() === "execute" && "3 / 4 · Execute"}
              {wizStep() === "done" && "4 / 4 · Done"}
            </span>
            <button class="feed-x" onClick={p.onClose}>×</button>
          </div>

          <div class="provisioning-body">
            {/* Step 1: connect + inspect */}
            <Show when={wizStep() === "connect"}>
              <p class="settings-hint">
                Enter the initial credentials your hosting provider gave you
                (commonly <code>root</code> + password). winmux will inspect
                the server before any mutation.
              </p>
              <label>
                <span>Host</span>
                <input
                  value={host()}
                  onInput={(e) => setHost(e.currentTarget.value)}
                  placeholder="1.2.3.4"
                />
              </label>
              <label>
                <span>Port</span>
                <input
                  type="number"
                  value={port()}
                  onInput={(e) => setPort(parseInt(e.currentTarget.value) || 22)}
                />
              </label>
              <label>
                <span>Initial user</span>
                <input
                  value={user()}
                  onInput={(e) => setUser(e.currentTarget.value)}
                />
              </label>
              <label>
                <span>Password</span>
                <input
                  type="password"
                  value={password()}
                  onInput={(e) => setPassword(e.currentTarget.value)}
                  placeholder="(if password-only)"
                />
              </label>
              <label>
                <span>Key path</span>
                <input
                  value={keyPath()}
                  onInput={(e) => setKeyPath(e.currentTarget.value)}
                  placeholder="(if provider gave you a key)"
                />
              </label>
              <Show when={keyPath()}>
                <label>
                  <span>Key passphrase</span>
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
                      ✓ {inspect()!.os_pretty_name ?? "OS detected"}
                    </div>
                    <div class="wizard-test-meta">
                      package_manager: {inspect()!.package_manager} ·
                      whoami: {inspect()!.whoami?.trim()}
                    </div>
                    <Show when={inspect()!.df_h}>
                      <div class="wizard-test-meta">disk: {inspect()!.df_h?.trim()}</div>
                    </Show>
                  </Show>
                  <Show when={!inspect()!.ok}>
                    <div class="wizard-test-line">✗ {inspect()!.message}</div>
                  </Show>
                </div>
              </Show>

              <div class="modal-buttons">
                <button onClick={p.onClose}>Cancel</button>
                <button
                  class="primary"
                  disabled={inspecting() || !host() || !user()}
                  onClick={() => void runInspect()}
                >
                  {inspecting() ? "Inspecting…" : "Connect & inspect"}
                </button>
              </div>
            </Show>

            {/* Step 2: configure */}
            <Show when={wizStep() === "configure"}>
              <p class="settings-hint">
                Target: <code>{host()}</code> ·{" "}
                <code>{inspect()?.os_pretty_name ?? "OS"}</code> · pm:{" "}
                <code>{inspect()?.package_manager}</code>
              </p>
              <label>
                <span>Profile</span>
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
                <span>New user name</span>
                <input
                  value={newUser()}
                  onInput={(e) => setNewUser(e.currentTarget.value)}
                />
              </label>
              <h4 class="provisioning-h4">Steps (uncheck to skip)</h4>
              <div class="provisioning-steps">
                <For each={stepCatalog()}>
                  {([id, label]) => (
                    <label class="provisioning-step-row">
                      <input
                        type="checkbox"
                        checked={effectiveSteps().includes(id)}
                        onChange={() => toggleStep(id)}
                      />
                      <span>{label}</span>
                    </label>
                  )}
                </For>
              </div>
              <div class="modal-buttons">
                <button onClick={() => setWizStep("connect")}>Back</button>
                <button class="primary" onClick={() => void startRun()}>
                  Execute
                </button>
              </div>
            </Show>

            {/* Step 3: execute */}
            <Show when={wizStep() === "execute"}>
              <p class="settings-hint">
                Run: <code>{runId() ?? "(starting…)"}</code> ·{" "}
                target <code>{host()}</code>
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
                <button onClick={() => setWizStep("done")}>Mark done</button>
              </div>
            </Show>

            <Show when={wizStep() === "done"}>
              <p>Provisioning finished. You can close this dialog.</p>
              <div class="modal-buttons">
                <button class="primary" onClick={p.onClose}>Close</button>
              </div>
            </Show>
          </div>
        </div>
      </div>
    </Show>
  );
}
