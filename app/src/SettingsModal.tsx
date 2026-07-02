import { createSignal, For, Show, onMount, createMemo, createEffect, onCleanup } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
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
  loadSettings,
  DEFAULT_SHORTCUTS,
  DEFAULT_CLAUDE_SETTINGS,
} from "./settings";
import { applyI18nSettings, LANGUAGES, t } from "./i18n";
import { VersionManager } from "./VersionManager";
import { formatEvent } from "./shortcuts";
import { AddonsTab } from "./AddonsTab";

interface Props {
  open: boolean;
  settings: Settings;
  onClose: () => void;
  onChange: (next: Settings) => void;
  /** Phase 68.E: active workspace — add-ons are per-remote. */
  activeWorkspaceId?: string;
}

type Tab = "general" | "theme" | "font" | "terminal" | "shortcuts" | "claude" | "hooks" | "notifications" | "addons" | "updates" | "logs" | "language" | "stt";

export function SettingsModal(p: Props) {
  const [tab, setTab] = createSignal<Tab>("theme");
  const [presets, setPresets] = createSignal<PresetEntry[]>([]);
  const [fonts, setFonts] = createSignal<FontFamilies>({ ui: [], mono: [] });
  const [advanced, setAdvanced] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  const [lastSaved, setLastSaved] = createSignal<number>(0);
  const [updateInfo, setUpdateInfo] = createSignal<UpdateInfo | null>(null);
  const [checking, setChecking] = createSignal(false);
  // Phase 38/39: resolved debug.log path + "Copied" flash + live tail.
  const [logPath, setLogPath] = createSignal<string>("");
  const [logCopied, setLogCopied] = createSignal(false);
  const [logTail, setLogTail] = createSignal<string>("");
  const refreshLogTail = async () => {
    try {
      setLogTail(await invoke<string>("read_log_tail", { n: 200 }));
    } catch (e) {
      console.warn("read_log_tail failed", e);
    }
  };
  // Phase 75: clear the debug log now, then refresh the viewer.
  const clearLogs = async () => {
    try {
      await invoke("clear_debug_log_cmd");
      await refreshLogTail();
    } catch (e) {
      console.warn("clear_debug_log_cmd failed", e);
    }
  };
  // Phase 48-C: /doctor snapshot — paste-friendly JSON for bug reports.
  const [doctorJson, setDoctorJson] = createSignal<string>("");
  const runDoctor = async () => {
    try {
      const snapshot = await invoke<unknown>("doctor");
      setDoctorJson(JSON.stringify(snapshot, null, 2));
    } catch (e) {
      setDoctorJson(`error: ${String(e)}`);
    }
  };

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
    // Phase 38: resolve the debug.log path for the Logs section.
    try { setLogPath(await invoke<string>("log_dir_path")); } catch (e) { console.warn(e); }
  });

  // Phase 38: Logs section actions.
  const onOpenLogFolder = () => {
    if (!logPath()) return;
    void revealItemInDir(logPath()).catch((e) => console.warn("revealItemInDir failed", e));
  };
  const onCopyLogPath = async () => {
    if (!logPath()) return;
    try {
      await navigator.clipboard.writeText(logPath());
      setLogCopied(true);
      setTimeout(() => setLogCopied(false), 1500);
    } catch (e) {
      console.warn("clipboard write failed", e);
    }
  };

  // Phase 39: poll the log tail every 5s while the Logs tab is open;
  // stop when the user navigates away or closes the modal.
  createEffect(() => {
    if (!p.open || tab() !== "logs") return;
    void refreshLogTail();
    const id = setInterval(() => void refreshLogTail(), 5000);
    onCleanup(() => clearInterval(id));
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
      // v0.2.3: re-pull settings so the "Last check" line in this
      // modal reflects the timestamp the backend just wrote. Without
      // this, the modal shows the stale value it loaded on open.
      try {
        const fresh = await loadSettings();
        p.onChange(fresh);
      } catch (e) {
        console.warn("refresh settings after check failed", e);
      }
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
              <For each={["general", "theme", "font", "terminal", "shortcuts", "claude", "hooks", "notifications", "addons", "updates", "logs", "language", "stt"] as Tab[]}>
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
              {/* Phase 49.A: General tab — workspace-lifecycle settings
                  (auto-destroy of empty workspaces). Kept separate from
                  the Terminal tab since these are not terminal-specific. */}
              <Show when={tab() === "general"}>
                <section>
                  <h4>{t("settings.tab.general")}</h4>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.auto_connect_on_workspace_select !== false}
                      onChange={(e) => update("auto_connect_on_workspace_select", e.currentTarget.checked)}
                    />
                    <span>{t("settings.autoConnect.label")}</span>
                  </label>
                  <p class="settings-hint" style="margin-top:-4px;margin-inline-start:24px">
                    {t("settings.autoConnect.hint")}
                  </p>
                  {/* Unshipped-fivefer (#3): browser session persistence. */}
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.persist_browser_sessions !== false}
                      onChange={(e) => update("persist_browser_sessions", e.currentTarget.checked)}
                    />
                    <span>{t("settings.persistBrowser.label")}</span>
                  </label>
                  <p class="settings-hint" style="margin-top:-4px;margin-inline-start:24px">
                    {t("settings.persistBrowser.hint")}
                  </p>
                  <label>
                    <span>{t("settings.autoDestroy.label")}</span>
                    <input
                      type="number"
                      min="1"
                      max="90"
                      placeholder={t("settings.autoDestroy.disabled")}
                      value={p.settings.auto_destroy_empty_workspaces_days ?? ""}
                      onInput={(e) => {
                        const raw = e.currentTarget.value.trim();
                        const n = raw === "" ? null : Math.min(90, Math.max(1, parseInt(raw, 10) || 0)) || null;
                        update("auto_destroy_empty_workspaces_days", n ?? undefined);
                      }}
                    />
                  </label>
                  <p class="settings-hint" style="margin-top:-4px;margin-inline-start:24px">
                    {t("settings.autoDestroy.hint")}
                  </p>
                </section>
              </Show>

              <Show when={tab() === "theme"}>
                <section>
                  <h4>{t("settings.theme.preset")}</h4>
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
                  <h4>{t("settings.theme.base_colors")}</h4>
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
                  <h4>{t("settings.font.ui")}</h4>
                  <label>
                    <span>{t("settings.font.family")}</span>
                    <input
                      type="text"
                      list="winmux-ui-fonts"
                      placeholder={t("settings.font.ui.placeholder")}
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
                  <h4>{t("settings.font.terminal")}</h4>
                  <label>
                    <span>{t("settings.font.family")}</span>
                    <input
                      type="text"
                      list="winmux-mono-fonts"
                      placeholder={t("settings.font.terminal.placeholder")}
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
                  <h4>{t("settings.font.web.title")}</h4>
                  <label>
                    <span>{t("settings.font.web.url")}</span>
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
                    {t("settings.font.web.hint", { example: "Iosevka" })}
                  </p>
                </section>
              </Show>

              {/* ── Terminal ─────────────────────────────────────────── */}
              <Show when={tab() === "terminal"}>
                <section>
                  <h4>{t("settings.terminal.cursor")}</h4>
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
                  <h4>{t("settings.terminal.buffer.title")}</h4>
                  <label>
                    <span>{t("settings.terminal.scrollback")}</span>
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
                      checked={p.settings.terminal.allow_proposed_api}
                      onChange={(e) => update("terminal", { ...p.settings.terminal, allow_proposed_api: e.currentTarget.checked })}
                    />
                    <span>Allow xterm.js proposed API (needed for WebGL)</span>
                  </label>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.terminal.use_winmux_tmux_config ?? true}
                      onChange={(e) => update("terminal", { ...p.settings.terminal, use_winmux_tmux_config: e.currentTarget.checked })}
                    />
                    <span>{t("settings.terminal.use_winmux_tmux_config.label")}</span>
                  </label>
                  <p class="settings-hint" style="margin-top:-4px;margin-inline-start:24px">
                    {t("settings.terminal.use_winmux_tmux_config.hint")}
                  </p>
                </section>
                <section>
                  <h4>{t("settings.terminal.rtl.title")}</h4>
                  <p class="settings-hint" style="margin-top:0">
                    {t("settings.terminal.rtl.hint")}
                  </p>
                  <For each={[
                    ["auto_per_line", "settings.terminal.rtl.auto.label", "settings.terminal.rtl.auto.desc"],
                    ["bidi_reorder", "settings.terminal.rtl.bidi.label", "settings.terminal.rtl.bidi.desc"],
                    ["off", "settings.terminal.rtl.off.label", "settings.terminal.rtl.off.desc"],
                  ] as const}>
                    {([id, labelKey, descKey]) => (
                      <label class="settings-radio" style="grid-template-columns: none !important; display: flex !important; align-items: flex-start; gap: 8px;">
                        <input
                          type="radio"
                          name="rtl-mode"
                          value={id}
                          checked={(p.settings.terminal.rtl_mode ?? "auto_per_line") === id}
                          onChange={() => update("terminal", { ...p.settings.terminal, rtl_mode: id })}
                        />
                        <span style="flex:1">
                          <strong>{t(labelKey)}</strong>
                          <div style="color: var(--w-text-dim); font-size: var(--w-fs-sm); margin-top: 2px;">{t(descKey)}</div>
                        </span>
                      </label>
                    )}
                  </For>
                  <label class="settings-checkbox" style="margin-top:8px">
                    <input
                      type="checkbox"
                      checked={p.settings.terminal.mirror_arrows_rtl ?? true}
                      onChange={(e) => update("terminal", { ...p.settings.terminal, mirror_arrows_rtl: e.currentTarget.checked })}
                    />
                    <span>{t("settings.terminal.mirror_arrows_rtl.label")}</span>
                  </label>
                  <p class="settings-hint" style="margin-top:-4px;margin-inline-start:24px">
                    {t("settings.terminal.mirror_arrows_rtl.hint")}
                  </p>
                </section>
              </Show>

              {/* ── Shortcuts ────────────────────────────────────────── */}
              <Show when={tab() === "shortcuts"}>
                <section>
                  <h4>{t("settings.shortcuts.title")}</h4>
                  <p class="settings-hint" style="margin-top:0">
                    {t("settings.shortcuts.hint")}
                  </p>
                  <For each={[
                    ["copy", "settings.shortcuts.copy"],
                    ["paste", "settings.shortcuts.paste"],
                    ["select_all", "settings.shortcuts.select_all"],
                    ["find", "settings.shortcuts.find"],
                    ["new_workspace", "settings.shortcuts.new_workspace"],
                    ["toggle_notes", "settings.shortcuts.toggle_notes"],
                    ["toggle_settings", "settings.shortcuts.toggle_settings"],
                    ["summarize_claude", "settings.shortcuts.summarize_claude"],
                  ] as const}>
                    {([key, labelKey]) => (
                      <ShortcutRow
                        label={t(labelKey)}
                        value={(p.settings.shortcuts ?? DEFAULT_SHORTCUTS)[key]}
                        defaultValue={DEFAULT_SHORTCUTS[key]}
                        onChange={(v) =>
                          update("shortcuts", {
                            ...(p.settings.shortcuts ?? DEFAULT_SHORTCUTS),
                            [key]: v,
                          } as Settings["shortcuts"])
                        }
                      />
                    )}
                  </For>
                  <label class="settings-checkbox" style="margin-top: 12px;">
                    <input
                      type="checkbox"
                      checked={(p.settings.shortcuts ?? DEFAULT_SHORTCUTS).copy_on_select_with_ctrl_c}
                      onChange={(e) =>
                        update("shortcuts", {
                          ...(p.settings.shortcuts ?? DEFAULT_SHORTCUTS),
                          copy_on_select_with_ctrl_c: e.currentTarget.checked,
                        } as Settings["shortcuts"])
                      }
                    />
                    <span>{t("settings.shortcuts.ctrl_c_copy")}</span>
                  </label>
                </section>
              </Show>

              {/* ── Claude (Phase 17) ────────────────────────────────── */}
              <Show when={tab() === "claude"}>
                <section>
                  <h4>{t("settings.claude.title")}</h4>
                  <p class="settings-hint" style="margin-top:0">
                    {t("settings.claude.hint")}
                  </p>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={(p.settings.claude ?? DEFAULT_CLAUDE_SETTINGS).auto_summarize_on_stop}
                      onChange={(e) =>
                        update("claude", {
                          ...(p.settings.claude ?? DEFAULT_CLAUDE_SETTINGS),
                          auto_summarize_on_stop: e.currentTarget.checked,
                        } as Settings["claude"])
                      }
                    />
                    <span>{t("settings.claude.auto_on_stop")}</span>
                  </label>
                  <label>
                    <span>{t("settings.claude.history_count")}</span>
                    <input
                      type="number"
                      min="5"
                      max="50"
                      value={(p.settings.claude ?? DEFAULT_CLAUDE_SETTINGS).summary_history_count}
                      onChange={(e) =>
                        update("claude", {
                          ...(p.settings.claude ?? DEFAULT_CLAUDE_SETTINGS),
                          summary_history_count:
                            Math.max(5, Math.min(50, parseInt(e.currentTarget.value) || 10)),
                        } as Settings["claude"])
                      }
                    />
                  </label>
                  <label class="modal-textarea-label">
                    <span>{t("settings.claude.summary_prompt")}</span>
                    <textarea
                      rows="3"
                      value={(p.settings.claude ?? DEFAULT_CLAUDE_SETTINGS).summary_prompt}
                      onChange={(e) =>
                        update("claude", {
                          ...(p.settings.claude ?? DEFAULT_CLAUDE_SETTINGS),
                          summary_prompt: e.currentTarget.value,
                        } as Settings["claude"])
                      }
                    />
                  </label>
                  <p class="settings-hint">
                    {t("settings.claude.prompt_hint")}
                  </p>
                </section>
              </Show>

              {/* ── Hooks ────────────────────────────────────────────── */}
              <Show when={tab() === "hooks"}>
                <section>
                  <h4>{t("settings.hooks.title")}</h4>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.hooks.enabled}
                      onChange={(e) => update("hooks", { ...p.settings.hooks, enabled: e.currentTarget.checked })}
                    />
                    <span>{t("settings.hooks.enabled")}</span>
                  </label>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.hooks.policy_enabled ?? true}
                      onChange={(e) => update("hooks", { ...p.settings.hooks, policy_enabled: e.currentTarget.checked })}
                    />
                    <span>{t("settings.hooks.policy_enabled")}</span>
                  </label>
                  <p class="settings-hint" style="margin-top:-4px;margin-inline-start:24px">
                    {t("settings.hooks.policy_enabled.hint")}
                  </p>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.hooks.auto_install ?? true}
                      onChange={(e) => update("hooks", { ...p.settings.hooks, auto_install: e.currentTarget.checked })}
                    />
                    <span>{t("settings.hooks.auto_install")}</span>
                  </label>
                  <p class="settings-hint" style="margin-top:-4px;margin-inline-start:24px">
                    {t("settings.hooks.auto_install.hint")}
                  </p>
                  <label>
                    <span>{t("settings.hooks.policy_preset")}</span>
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
                  <h4>{t("settings.notifications.toasts.title")}</h4>
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
                    <span>{t("settings.notifications.sound_enabled")}</span>
                  </label>
                  {/* Phase 66 (KK): per-event toast toggles. */}
                  <h4 style="margin-top:14px">{t("settings.notifications.perEvent.title")}</h4>
                  <For each={[
                    ["toast_session_start", "settings.notifications.ev.session_start"],
                    ["toast_session_end", "settings.notifications.ev.session_end"],
                    ["toast_stop", "settings.notifications.ev.stop"],
                    ["toast_notification", "settings.notifications.ev.notification"],
                    ["toast_gate", "settings.notifications.ev.gate"],
                    ["toast_block", "settings.notifications.ev.block"],
                  ] as const}>
                    {([key, labelKey]) => (
                      <label class="settings-checkbox">
                        <input
                          type="checkbox"
                          disabled={!p.settings.notifications.toast_enabled}
                          checked={p.settings.notifications[key] ?? false}
                          onChange={(e) => update("notifications", { ...p.settings.notifications, [key]: e.currentTarget.checked })}
                        />
                        <span>{t(labelKey)}</span>
                      </label>
                    )}
                  </For>
                </section>
              </Show>

              {/* ── Updates ──────────────────────────────────────────── */}
              <Show when={tab() === "updates"}>
                <section>
                  <h4>{t("settings.updates.title")}</h4>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.updates.check_on_startup}
                      onChange={(e) => update("updates", { ...p.settings.updates, check_on_startup: e.currentTarget.checked })}
                    />
                    <span>{t("settings.updates.check_on_startup")}</span>
                  </label>
                  <label>
                    <span>{t("settings.updates.manifest_url")}</span>
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

                  {/* Phase 71: version history + install/downgrade + channel. */}
                  <hr class="modal-sep" />
                  <h4>{t("vm.history")}</h4>
                  <VersionManager
                    channel={p.settings.updates.channel}
                    onSetChannel={(c) => update("updates", { ...p.settings.updates, channel: c })}
                    skipped={p.settings.updates.skipped_versions}
                    onUnskip={(v) =>
                      update("updates", {
                        ...p.settings.updates,
                        skipped_versions: p.settings.updates.skipped_versions.filter((x) => x !== v),
                      })
                    }
                  />
                </section>
              </Show>

              {/* Phase 39: Logs tab — live tail viewer + path + open/copy. */}
              <Show when={tab() === "logs"}>
                <section>
                  <h4>{t("settings.logs.recent")}</h4>
                  <pre class="settings-logs-viewer">{logTail()}</pre>
                  <div class="settings-logs-actions">
                    <button onClick={() => void refreshLogTail()}>
                      {t("settings.logs.refresh")}
                    </button>
                  </div>
                  <hr class="modal-sep" />
                  <div class="settings-logs-row">
                    <span class="settings-logs-label">{t("settings.updates.logs.path")}</span>
                    <code class="settings-logs-path">{logPath()}</code>
                  </div>
                  <div class="settings-logs-actions">
                    <button onClick={onOpenLogFolder} disabled={!logPath()}>
                      {t("settings.updates.logs.openFolder")}
                    </button>
                    <button onClick={() => void onCopyLogPath()} disabled={!logPath()}>
                      {logCopied() ? t("settings.updates.logs.copied") : t("settings.updates.logs.copyPath")}
                    </button>
                  </div>
                  {/* Phase 75: retention + clear. */}
                  <hr class="modal-sep" />
                  <div class="settings-logs-row">
                    <span class="settings-logs-label">{t("settings.logs.retention")}</span>
                    <input
                      type="number"
                      min="0"
                      max="365"
                      class="settings-logs-retention"
                      value={p.settings.logs?.retention_days ?? 7}
                      onChange={(e) =>
                        update("logs", {
                          retention_days: Math.max(0, Math.min(365, parseInt(e.currentTarget.value || "0", 10) || 0)),
                        })
                      }
                    />
                  </div>
                  <div class="settings-hint">{t("settings.logs.retention_hint")}</div>
                  <div class="settings-logs-actions">
                    <button onClick={() => void clearLogs()}>{t("settings.logs.clear")}</button>
                  </div>
                  {/* Phase 48-C: /doctor diagnostic snapshot for bug reports. */}
                  <hr class="modal-sep" />
                  <div class="settings-logs-actions">
                    <button onClick={() => void runDoctor()}>Run Doctor</button>
                  </div>
                  <Show when={doctorJson()}>
                    <pre class="settings-logs-viewer">{doctorJson()}</pre>
                  </Show>
                </section>
              </Show>

              {/* ── Add-ons (Phase 68.E) ─────────────────────────────── */}
              <Show when={tab() === "addons"}>
                <AddonsTab workspaceId={p.activeWorkspaceId} />
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

              {/* Phase 58: voice input (speech-to-text). Opt-in;
                  hidden behind its own tab so the existing Settings
                  surface stays calm for users who don't care. */}
              <Show when={tab() === "stt"}>
                <section>
                  <h4>{t("settings.stt.title")}</h4>
                  <p class="settings-hint">{t("settings.stt.hint")}</p>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.stt?.enabled ?? false}
                      onChange={(e) =>
                        update("stt", {
                          ...(p.settings.stt ?? {
                            enabled: false,
                            backend: "webspeech",
                            local_endpoint: null,
                            language: "auto",
                            push_to_talk_hotkey: "Ctrl+Shift+M",
                          }),
                          enabled: e.currentTarget.checked,
                        })
                      }
                    />
                    <span>{t("settings.stt.enable")}</span>
                  </label>
                  <label>
                    <span>{t("settings.stt.backend.label")}</span>
                    <select
                      value={p.settings.stt?.backend ?? "webspeech"}
                      onChange={(e) =>
                        update("stt", {
                          ...(p.settings.stt ?? {
                            enabled: false,
                            backend: "webspeech",
                            local_endpoint: null,
                            language: "auto",
                            push_to_talk_hotkey: "Ctrl+Shift+M",
                          }),
                          backend: e.currentTarget.value as "webspeech" | "local",
                        })
                      }
                    >
                      <option value="webspeech">{t("settings.stt.backend.webspeech")}</option>
                      <option value="local">{t("settings.stt.backend.local")}</option>
                    </select>
                  </label>
                  <Show when={(p.settings.stt?.backend ?? "webspeech") === "local"}>
                    <label>
                      <span>{t("settings.stt.endpoint.label")}</span>
                      <input
                        type="text"
                        value={p.settings.stt?.local_endpoint ?? ""}
                        placeholder={t("settings.stt.endpoint.placeholder")}
                        onInput={(e) =>
                          update("stt", {
                            ...(p.settings.stt ?? {
                              enabled: false,
                              backend: "local",
                              local_endpoint: null,
                              language: "auto",
                              push_to_talk_hotkey: "Ctrl+Shift+M",
                            }),
                            local_endpoint:
                              e.currentTarget.value.trim() === ""
                                ? null
                                : e.currentTarget.value,
                          })
                        }
                      />
                    </label>
                  </Show>
                  <label>
                    <span>{t("settings.stt.language.label")}</span>
                    <select
                      value={p.settings.stt?.language ?? "auto"}
                      onChange={(e) =>
                        update("stt", {
                          ...(p.settings.stt ?? {
                            enabled: false,
                            backend: "webspeech",
                            local_endpoint: null,
                            language: "auto",
                            push_to_talk_hotkey: "Ctrl+Shift+M",
                          }),
                          language: e.currentTarget.value,
                        })
                      }
                    >
                      <option value="auto">{t("settings.stt.language.auto")}</option>
                      <option value="he-IL">עברית</option>
                      <option value="en-US">English (US)</option>
                      <option value="ar-SA">العربية</option>
                      <option value="ru-RU">Русский</option>
                    </select>
                  </label>
                  <label>
                    <span>{t("settings.stt.hotkey.label")}</span>
                    <input
                      type="text"
                      value={p.settings.stt?.push_to_talk_hotkey ?? "Ctrl+Shift+M"}
                      placeholder="Ctrl+Shift+M"
                      onInput={(e) =>
                        update("stt", {
                          ...(p.settings.stt ?? {
                            enabled: false,
                            backend: "webspeech",
                            local_endpoint: null,
                            language: "auto",
                            push_to_talk_hotkey: "Ctrl+Shift+M",
                          }),
                          push_to_talk_hotkey: e.currentTarget.value,
                        })
                      }
                    />
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

function ShortcutRow(p: {
  label: string;
  value: string;
  defaultValue: string;
  onChange: (v: string) => void;
}) {
  const [recording, setRecording] = createSignal(false);
  return (
    <div class="settings-shortcut-row">
      <span class="settings-shortcut-label">{p.label}</span>
      <input
        type="text"
        class="settings-shortcut-input"
        value={recording() ? t("settings.shortcuts.recording") : p.value}
        readOnly
        onFocus={() => setRecording(true)}
        onBlur={() => setRecording(false)}
        onKeyDown={(e) => {
          if (!recording()) return;
          // Esc cancels the recording without committing.
          if (e.key === "Escape") {
            e.preventDefault();
            (e.currentTarget as HTMLInputElement).blur();
            return;
          }
          const formatted = formatEvent(e);
          if (formatted) {
            e.preventDefault();
            p.onChange(formatted);
            (e.currentTarget as HTMLInputElement).blur();
          }
        }}
      />
      <button
        class="settings-shortcut-reset"
        type="button"
        title={t("common.reset")}
        onClick={() => p.onChange(p.defaultValue)}
      >
        ↺
      </button>
    </div>
  );
}
