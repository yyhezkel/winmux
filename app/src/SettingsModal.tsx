import { createSignal, For, Show, onMount, createMemo } from "solid-js";
import {
  Settings,
  PresetEntry,
  FontFamilies,
  UpdateInfo,
  applyTheme,
  getPresets,
  applyPreset,
  resetSettings,
  saveSettings,
  listSystemFonts,
  checkForUpdates,
} from "./settings";
import { applyI18nSettings, LANGUAGES, t } from "./i18n";

interface Props {
  open: boolean;
  settings: Settings;
  onClose: () => void;
  onChange: (next: Settings) => void;
}

type Tab = "theme" | "font" | "terminal" | "hooks" | "notifications" | "updates" | "language";

export function SettingsModal(p: Props) {
  const [tab, setTab] = createSignal<Tab>("theme");
  const [presets, setPresets] = createSignal<PresetEntry[]>([]);
  const [fonts, setFonts] = createSignal<FontFamilies>({ ui: [], mono: [] });
  const [advanced, setAdvanced] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  const [lastSaved, setLastSaved] = createSignal<number>(0);
  const [updateInfo, setUpdateInfo] = createSignal<UpdateInfo | null>(null);
  const [checking, setChecking] = createSignal(false);

  // Debounced save: live-preview every change locally, persist 500ms after
  // the last edit so a slider drag doesn't write 60 files/sec.
  let saveTimer: number | null = null;
  const queueSave = (next: Settings) => {
    p.onChange(next);
    applyTheme(next);
    applyI18nSettings(next.i18n);
    if (saveTimer) clearTimeout(saveTimer);
    setSaving(true);
    saveTimer = window.setTimeout(async () => {
      try {
        await saveSettings(next);
        setLastSaved(Date.now());
      } catch (e) {
        console.error("settings_save failed", e);
      } finally {
        setSaving(false);
      }
    }, 500);
  };

  const update = <K extends keyof Settings>(k: K, v: Settings[K]) =>
    queueSave({ ...p.settings, [k]: v });

  const setTheme = (patch: Partial<Settings["theme"]>) => {
    const next = { ...p.settings, theme: { ...p.settings.theme, ...patch, preset: "custom" } };
    queueSave(next);
  };

  const setAnsi = (patch: Partial<Settings["theme"]["ansi"]>) =>
    setTheme({ ansi: { ...p.settings.theme.ansi, ...patch } });

  onMount(async () => {
    try { setPresets(await getPresets()); } catch (e) { console.warn(e); }
    try { setFonts(await listSystemFonts()); } catch (e) { console.warn(e); }
  });

  const onPickPreset = async (id: string) => {
    try {
      const next = await applyPreset(id);
      p.onChange(next);
      applyTheme(next);
      setLastSaved(Date.now());
    } catch (e) {
      console.error("apply preset failed", e);
    }
  };

  const onResetAll = async () => {
    if (!window.confirm("Reset ALL settings to defaults?")) return;
    try {
      const next = await resetSettings();
      p.onChange(next);
      applyTheme(next);
      setLastSaved(Date.now());
    } catch (e) {
      console.error("reset failed", e);
    }
  };

  const onCheckUpdates = async () => {
    setChecking(true);
    try {
      const info = await checkForUpdates();
      setUpdateInfo(info);
    } catch (e) {
      console.error("check updates failed", e);
    } finally {
      setChecking(false);
    }
  };

  const fmtAge = (iso?: string | null) => {
    if (!iso) return "never";
    const t = Date.parse(iso);
    if (Number.isNaN(t)) return iso ?? "—";
    const sec = Math.max(1, Math.floor((Date.now() - t) / 1000));
    if (sec < 60) return `${sec}s ago`;
    if (sec < 3600) return `${Math.floor(sec / 60)}m ago`;
    if (sec < 86400) return `${Math.floor(sec / 3600)}h ago`;
    return `${Math.floor(sec / 86400)}d ago`;
  };

  const savedAge = createMemo(() => {
    if (saving()) return "saving…";
    if (!lastSaved()) return "";
    const sec = Math.floor((Date.now() - lastSaved()) / 1000);
    if (sec < 5) return "saved ✓";
    return "";
  });

  return (
    <Show when={p.open}>
      <div class="modal-backdrop" onClick={p.onClose}>
        <div
          class="modal settings-modal"
          onClick={(e) => e.stopPropagation()}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <div class="settings-head">
            <h3>{t("settings.title")}</h3>
            <span class="settings-saved-flag">{savedAge()}</span>
            <button class="feed-x" title={t("common.close")} onClick={p.onClose}>×</button>
          </div>

          <div class="settings-body">
            <nav class="settings-tabs">
              <For each={["theme", "font", "terminal", "hooks", "notifications", "updates", "language"] as Tab[]}>
                {(name) => (
                  <button
                    class={`settings-tab ${tab() === name ? "active" : ""}`}
                    onClick={() => setTab(name)}
                  >
                    {t(`settings.tab.${name}`)}
                  </button>
                )}
              </For>
              <div class="settings-tabs-spacer" />
              <button class="settings-tab danger" onClick={onResetAll}>
                {t("settings.reset_all")}
              </button>
            </nav>

            <div class="settings-pane">
              {/* ── Theme ────────────────────────────────────────────── */}
              <Show when={tab() === "theme"}>
                <section>
                  <h4>Preset</h4>
                  <div class="settings-preset-grid">
                    <For each={presets()}>
                      {(pr) => (
                        <button
                          class={`settings-preset-card ${p.settings.theme.preset === pr.id ? "active" : ""}`}
                          onClick={() => onPickPreset(pr.id)}
                          title={pr.label}
                        >
                          <div
                            class="settings-preset-swatches"
                            style={{ background: pr.theme.background }}
                          >
                            <span style={{ background: pr.theme.surface }} />
                            <span style={{ background: pr.theme.accent }} />
                            <span style={{ background: pr.theme.success }} />
                            <span style={{ background: pr.theme.warning }} />
                            <span style={{ background: pr.theme.error }} />
                          </div>
                          <span class="settings-preset-label">{pr.label}</span>
                        </button>
                      )}
                    </For>
                  </div>
                </section>
                <section>
                  <h4>Base colors</h4>
                  <div class="settings-color-grid">
                    <ColorRow label="Accent" value={p.settings.theme.accent} onInput={(v) => setTheme({ accent: v })} />
                    <ColorRow label="Background" value={p.settings.theme.background} onInput={(v) => setTheme({ background: v })} />
                    <ColorRow label="Surface" value={p.settings.theme.surface} onInput={(v) => setTheme({ surface: v })} />
                    <ColorRow label="Border" value={p.settings.theme.border} onInput={(v) => setTheme({ border: v })} />
                    <ColorRow label="Text primary" value={p.settings.theme.text_primary} onInput={(v) => setTheme({ text_primary: v })} />
                    <ColorRow label="Text secondary" value={p.settings.theme.text_secondary} onInput={(v) => setTheme({ text_secondary: v })} />
                    <ColorRow label="Success" value={p.settings.theme.success} onInput={(v) => setTheme({ success: v })} />
                    <ColorRow label="Warning" value={p.settings.theme.warning} onInput={(v) => setTheme({ warning: v })} />
                    <ColorRow label="Error" value={p.settings.theme.error} onInput={(v) => setTheme({ error: v })} />
                  </div>
                </section>
                <section>
                  <h4>
                    <button class="settings-disclose" onClick={() => setAdvanced(!advanced())}>
                      {advanced() ? "▾" : "▸"} ANSI palette (xterm 16)
                    </button>
                  </h4>
                  <Show when={advanced()}>
                    <div class="settings-color-grid">
                      <For each={Object.keys(p.settings.theme.ansi) as (keyof Settings["theme"]["ansi"])[]}>
                        {(k) => (
                          <ColorRow
                            label={k.replace(/_/g, " ")}
                            value={p.settings.theme.ansi[k]}
                            onInput={(v) => setAnsi({ [k]: v } as any)}
                          />
                        )}
                      </For>
                    </div>
                  </Show>
                </section>
              </Show>

              {/* ── Font ─────────────────────────────────────────────── */}
              {/* Phase font-bug-fix: family is now <input list=""> + <datalist>
                  so the user can pick from the detected list OR type any
                  custom name (CSS will fall back if it's not installed —
                  the "Web font URL" field below can fetch one at runtime). */}
              <Show when={tab() === "font"}>
                <section>
                  <h4>UI font</h4>
                  <label>
                    <span>Family</span>
                    <input
                      type="text"
                      list="winmux-ui-fonts"
                      placeholder="system-ui or any installed font"
                      value={p.settings.font.ui_family}
                      onChange={(e) => update("font", { ...p.settings.font, ui_family: e.currentTarget.value })}
                      onBlur={(e) => update("font", { ...p.settings.font, ui_family: e.currentTarget.value })}
                    />
                  </label>
                  <datalist id="winmux-ui-fonts">
                    <For each={fonts().ui}>{(f) => <option value={f} />}</For>
                  </datalist>
                  <label>
                    <span>Size (pt)</span>
                    <input
                      type="number"
                      min="8"
                      max="32"
                      value={p.settings.font.ui_size_pt}
                      onInput={(e) => {
                        const n = parseInt(e.currentTarget.value);
                        if (!Number.isNaN(n) && n >= 8 && n <= 32) {
                          update("font", { ...p.settings.font, ui_size_pt: n });
                        }
                      }}
                    />
                  </label>
                </section>
                <section>
                  <h4>Terminal font</h4>
                  <label>
                    <span>Family</span>
                    <input
                      type="text"
                      list="winmux-mono-fonts"
                      placeholder="Cascadia Mono / JetBrains Mono / …"
                      value={p.settings.font.terminal_family}
                      onChange={(e) => update("font", { ...p.settings.font, terminal_family: e.currentTarget.value })}
                      onBlur={(e) => update("font", { ...p.settings.font, terminal_family: e.currentTarget.value })}
                    />
                  </label>
                  <datalist id="winmux-mono-fonts">
                    <For each={fonts().mono}>{(f) => <option value={f} />}</For>
                  </datalist>
                  <label>
                    <span>Size (pt)</span>
                    <input
                      type="number"
                      min="8"
                      max="32"
                      value={p.settings.font.terminal_size_pt}
                      onInput={(e) => {
                        const n = parseInt(e.currentTarget.value);
                        if (!Number.isNaN(n) && n >= 8 && n <= 32) {
                          update("font", { ...p.settings.font, terminal_size_pt: n });
                        }
                      }}
                    />
                  </label>
                </section>
                <section>
                  <h4>Web font (optional)</h4>
                  <label>
                    <span>Stylesheet URL</span>
                    <input
                      type="text"
                      placeholder="https://fonts.googleapis.com/css2?family=Iosevka&display=swap"
                      value={p.settings.font.web_font_url ?? ""}
                      onChange={(e) =>
                        update("font", { ...p.settings.font, web_font_url: e.currentTarget.value || null })
                      }
                    />
                  </label>
                  <p class="settings-hint">
                    Pastes a CSS link tag at runtime. After it loads, type the
                    family name in the fields above (e.g. <code>Iosevka</code>).
                  </p>
                </section>
              </Show>

              {/* ── Terminal ─────────────────────────────────────────── */}
              <Show when={tab() === "terminal"}>
                <section>
                  <h4>Cursor</h4>
                  <div class="settings-radio-row">
                    <For each={["block", "bar", "underline"] as const}>
                      {(c) => (
                        <label class="settings-radio">
                          <input
                            type="radio"
                            name="cursor"
                            value={c}
                            checked={p.settings.terminal.cursor_style === c}
                            onChange={() => update("terminal", { ...p.settings.terminal, cursor_style: c })}
                          />
                          <span>{c}</span>
                        </label>
                      )}
                    </For>
                  </div>
                </section>
                <section>
                  <h4>Buffer</h4>
                  <label>
                    <span>Scrollback</span>
                    <input
                      type="number"
                      min="100"
                      max="100000"
                      step="500"
                      value={p.settings.terminal.scrollback_lines}
                      onChange={(e) => update("terminal", { ...p.settings.terminal, scrollback_lines: parseInt(e.currentTarget.value) || 10000 })}
                    />
                  </label>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.terminal.bidi_enabled}
                      onChange={(e) => update("terminal", { ...p.settings.terminal, bidi_enabled: e.currentTarget.checked })}
                    />
                    <span>BiDi (Hebrew/Arabic) reorder</span>
                  </label>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.terminal.allow_proposed_api}
                      onChange={(e) => update("terminal", { ...p.settings.terminal, allow_proposed_api: e.currentTarget.checked })}
                    />
                    <span>Allow xterm.js proposed API (needed for WebGL)</span>
                  </label>
                </section>
              </Show>

              {/* ── Hooks ────────────────────────────────────────────── */}
              <Show when={tab() === "hooks"}>
                <section>
                  <h4>Agent hooks</h4>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.hooks.enabled}
                      onChange={(e) => update("hooks", { ...p.settings.hooks, enabled: e.currentTarget.checked })}
                    />
                    <span>Pipe AI agent permission requests through winmux</span>
                  </label>
                  <label>
                    <span>Policy preset</span>
                    <select
                      value={p.settings.hooks.policy_preset}
                      onChange={(e) => update("hooks", { ...p.settings.hooks, policy_preset: e.currentTarget.value })}
                    >
                      <option value="paranoid">paranoid — every tool prompts</option>
                      <option value="default">default — risky tools only</option>
                      <option value="relaxed">relaxed — auto-allow trusted tools</option>
                      <option value="auto">auto — never prompt (deprecated)</option>
                    </select>
                  </label>
                  <p class="settings-hint">
                    To install/refresh the OS-level hook entries, run{" "}
                    <code>winmux setup-hooks --agent claude --force</code>{" "}
                    in any terminal.
                  </p>
                </section>
              </Show>

              {/* ── Notifications ────────────────────────────────────── */}
              <Show when={tab() === "notifications"}>
                <section>
                  <h4>Toasts</h4>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.notifications.toast_enabled}
                      onChange={(e) => update("notifications", { ...p.settings.notifications, toast_enabled: e.currentTarget.checked })}
                    />
                    <span>Show OS toast notifications (workspace events, updates)</span>
                  </label>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.notifications.sound_enabled}
                      onChange={(e) => update("notifications", { ...p.settings.notifications, sound_enabled: e.currentTarget.checked })}
                    />
                    <span>Play sound on permission-request cards</span>
                  </label>
                </section>
              </Show>

              {/* ── Updates ──────────────────────────────────────────── */}
              <Show when={tab() === "updates"}>
                <section>
                  <h4>Update check</h4>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.updates.check_on_startup}
                      onChange={(e) => update("updates", { ...p.settings.updates, check_on_startup: e.currentTarget.checked })}
                    />
                    <span>Check for updates on startup</span>
                  </label>
                  <label>
                    <span>Manifest URL</span>
                    <input
                      type="text"
                      value={p.settings.updates.manifest_url ?? ""}
                      onChange={(e) => update("updates", { ...p.settings.updates, manifest_url: e.currentTarget.value || null })}
                    />
                  </label>
                  <p class="settings-hint">
                    Last check: {fmtAge(p.settings.updates.last_check_iso)}
                    <Show when={p.settings.updates.last_seen_version}>
                      {" "}· latest seen: {p.settings.updates.last_seen_version}
                    </Show>
                  </p>
                  <button class="primary" disabled={checking()} onClick={onCheckUpdates}>
                    {checking() ? "Checking…" : "Check now"}
                  </button>
                  <Show when={updateInfo()}>
                    <div class="settings-update-result">
                      <p>
                        {t("settings.updates.current")} <code>{updateInfo()!.current_version}</code>
                        {" · "}{t("settings.updates.latest")} <code>{updateInfo()!.latest_version ?? "—"}</code>
                      </p>
                      <Show when={updateInfo()!.error}>
                        <p class="settings-update-err">{t("settings.updates.error", { msg: updateInfo()!.error ?? "" })}</p>
                      </Show>
                      <Show when={updateInfo()!.available}>
                        <p class="settings-update-ok">{t("settings.updates.available")}</p>
                      </Show>
                    </div>
                  </Show>
                </section>
              </Show>

              {/* ── Language ──────────────────────────────────────────── */}
              <Show when={tab() === "language"}>
                <section>
                  <h4>{t("settings.language.title")}</h4>
                  <label>
                    <span>{t("settings.language.label")}</span>
                    <select
                      value={p.settings.i18n.language}
                      onChange={(e) =>
                        update("i18n", { ...p.settings.i18n, language: e.currentTarget.value })
                      }
                    >
                      <For each={LANGUAGES}>
                        {(l) => <option value={l.id}>{l.label}</option>}
                      </For>
                    </select>
                  </label>
                  <label>
                    <span>{t("settings.language.direction")}</span>
                    <div class="settings-radio-row">
                      <For each={["auto", "ltr", "rtl"] as const}>
                        {(d) => (
                          <label class="settings-radio">
                            <input
                              type="radio"
                              name="dir"
                              value={d}
                              checked={p.settings.i18n.direction === d}
                              onChange={() =>
                                update("i18n", { ...p.settings.i18n, direction: d })
                              }
                            />
                            <span>{t(`settings.language.dir.${d}`)}</span>
                          </label>
                        )}
                      </For>
                    </div>
                  </label>
                </section>
              </Show>
            </div>
          </div>
        </div>
      </div>
    </Show>
  );
}

function ColorRow(p: { label: string; value: string; onInput: (v: string) => void }) {
  return (
    <div class="settings-color-row">
      <input
        type="color"
        value={p.value}
        onInput={(e) => p.onInput(e.currentTarget.value)}
      />
      <input
        type="text"
        class="settings-color-text"
        value={p.value}
        onInput={(e) => p.onInput(e.currentTarget.value)}
      />
      <span>{p.label}</span>
    </div>
  );
}
