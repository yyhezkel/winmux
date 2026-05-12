import { createSignal, Show, onMount, onCleanup } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { t } from "./i18n";

// Phase 17.B: minimal built-in editor. A modal with a monospace
// <textarea>, Save / Cancel / Reload buttons, and an unsaved-changes
// confirmation on close. Syntax highlighting is intentionally out of
// scope — we want this to be a "view the file, fix a typo, save"
// affordance, not a code editor.

interface FileContents {
  text: string;
  encoding: string;
  is_binary: boolean;
  size: number;
  truncated: boolean;
}

interface Props {
  open: boolean;
  /** Display name shown in the header. */
  filename: string;
  /** Full path shown under the filename + used by the backend. */
  path: string;
  /** "local" → reads/writes via fs. "remote" → SFTP via the
   *  workspace. */
  side: "local" | "remote";
  /** Required when side === "remote". */
  workspaceId?: string;
  onClose: () => void;
  /** Fires after a successful save so the parent (file manager) can
   *  refresh the row's size/mtime in the listing. */
  onSaved?: () => void;
}

export function FileEditor(p: Props) {
  const [contents, setContents] = createSignal<string>("");
  const [original, setOriginal] = createSignal<string>("");
  const [meta, setMeta] = createSignal<FileContents | null>(null);
  const [loading, setLoading] = createSignal(true);
  const [saving, setSaving] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
  const [largeWarn, setLargeWarn] = createSignal(false);

  const dirty = () => contents() !== original();

  const load = async () => {
    setLoading(true);
    setErr(null);
    try {
      const fc: FileContents =
        p.side === "local"
          ? await invoke("file_read_local", { path: p.path })
          : await invoke("file_read_remote", {
              workspaceId: p.workspaceId,
              path: p.path,
            });
      setMeta(fc);
      setContents(fc.text);
      setOriginal(fc.text);
      // Phase 17.B threshold check happens against the actual byte
      // size returned by the backend so we don't second-guess what
      // counts as "large".
      const threshold = await invoke<number>("file_large_threshold");
      setLargeWarn(fc.size > threshold && !fc.is_binary);
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  };

  const save = async () => {
    if (meta()?.is_binary) return;
    setSaving(true);
    setErr(null);
    try {
      if (p.side === "local") {
        await invoke("file_write_local", { path: p.path, text: contents() });
      } else {
        await invoke("file_write_remote", {
          workspaceId: p.workspaceId,
          path: p.path,
          text: contents(),
        });
      }
      setOriginal(contents());
      p.onSaved?.();
    } catch (e) {
      setErr(String(e));
    } finally {
      setSaving(false);
    }
  };

  const tryClose = () => {
    if (dirty()) {
      if (!window.confirm(t("editor.unsaved_confirm"))) return;
    }
    p.onClose();
  };

  // Ctrl+S → save. Scoped to the modal — we attach to document while
  // open and detach on close.
  const keydown = (e: KeyboardEvent) => {
    if (!p.open) return;
    if (e.ctrlKey && !e.shiftKey && !e.altKey && (e.key === "s" || e.key === "S")) {
      e.preventDefault();
      void save();
    }
    if (e.key === "Escape") {
      e.preventDefault();
      tryClose();
    }
  };

  onMount(() => {
    if (p.open) void load();
    window.addEventListener("keydown", keydown);
  });
  onCleanup(() => window.removeEventListener("keydown", keydown));

  const fmtSize = (n: number) => {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / 1024 / 1024).toFixed(1)} MB`;
  };

  return (
    <Show when={p.open}>
      <div class="modal-backdrop" onClick={tryClose}>
        <div
          class="modal editor-modal"
          onClick={(e) => e.stopPropagation()}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <div class="editor-head">
            <h3>{t("editor.title", { filename: p.filename })}</h3>
            <span class="editor-path" title={p.path}>{p.path}</span>
            <button class="feed-x" title={t("common.close")} onClick={tryClose}>
              ×
            </button>
          </div>

          <Show when={loading()}>
            <div class="editor-status">{t("common.loading")}</div>
          </Show>

          <Show when={err()}>
            <div class="editor-status err">⚠ {err()}</div>
          </Show>

          <Show when={!loading() && meta()?.is_binary}>
            <div class="editor-status err">
              {t("editor.binary_warning")}
            </div>
          </Show>

          <Show when={!loading() && largeWarn()}>
            <div class="editor-status warn">
              {t("editor.large_file_warning", {
                size: meta() ? fmtSize(meta()!.size) : "?",
              })}
            </div>
          </Show>

          <Show when={!loading() && !meta()?.is_binary}>
            <textarea
              class="editor-textarea"
              spellcheck={false}
              value={contents()}
              onInput={(e) => setContents(e.currentTarget.value)}
              autofocus
            />
            <div class="editor-meta">
              {meta()?.encoding ?? "—"} ·{" "}
              {meta() ? fmtSize(meta()!.size) : "0 B"}
              <Show when={dirty()}>
                <span class="editor-dirty"> · {t("editor.dirty")}</span>
              </Show>
            </div>
          </Show>

          <div class="modal-buttons">
            <button onClick={() => void load()} disabled={loading() || saving()}>
              {t("editor.btn.reload")}
            </button>
            <button onClick={tryClose}>{t("editor.btn.cancel")}</button>
            <button
              class="primary"
              disabled={loading() || saving() || !dirty() || meta()?.is_binary}
              onClick={() => void save()}
            >
              {saving() ? t("common.saving") : t("editor.btn.save")}
            </button>
          </div>
        </div>
      </div>
    </Show>
  );
}
