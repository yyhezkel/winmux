import { createSignal, For, Show, onMount, createMemo } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { t } from "./i18n";

// Phase 15.B: dual-column file manager (local + remote SFTP).
//
// Lives inside a layout-pane just like Terminal / Browser. Local
// column always renders; remote column lights up only when the
// workspace has an active SSH session (the backend will return a
// friendly error otherwise — surfaced as a banner).

interface FileEntry {
  name: string;
  is_dir: boolean;
  is_link: boolean;
  size: number;
  modified: number;
  permissions: string;
}

interface Props {
  workspaceId: string;
  /** True if the workspace is an SSH workspace (i.e. the right column
   *  should be visible). When false we show only the local column.    */
  hasSsh: boolean;
  /** Phase 16: True iff a terminal pane in the workspace currently
   *  has an active SSH session. When false (SSH workspace, no
   *  terminal connected yet) the remote column shows a friendly
   *  "connect a terminal first" placeholder instead of an error. */
  hasActiveSession?: boolean;
}

type Side = "local" | "remote";

export function FileManagerPane(p: Props) {
  const [localPath, setLocalPath] = createSignal("");
  const [remotePath, setRemotePath] = createSignal("");
  const [localEntries, setLocalEntries] = createSignal<FileEntry[]>([]);
  const [remoteEntries, setRemoteEntries] = createSignal<FileEntry[]>([]);
  const [localSel, setLocalSel] = createSignal<string | null>(null);
  const [remoteSel, setRemoteSel] = createSignal<string | null>(null);
  const [showHidden, setShowHidden] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
  const [status, setStatus] = createSignal<string>("");
  // Phase 16: toolbar toggle for hiding the local column when the
  // user only cares about remote. Default on SSH workspaces is to
  // show both columns; users who want a wider remote pane flip
  // this off.
  const [showLocal, setShowLocal] = createSignal(true);

  const refreshLocal = async () => {
    try {
      const list = await invoke<FileEntry[]>("file_list_local", {
        path: localPath(),
        showHidden: showHidden(),
      });
      setLocalEntries(list);
    } catch (e) {
      setErr(`local list: ${String(e)}`);
    }
  };
  const refreshRemote = async () => {
    if (!p.hasSsh) return;
    try {
      const list = await invoke<FileEntry[]>("file_list_remote", {
        workspaceId: p.workspaceId,
        path: remotePath(),
        showHidden: showHidden(),
      });
      setRemoteEntries(list);
    } catch (e) {
      // Most common case: no active SSH session yet. Surface and try
      // again next refresh tick.
      setErr(`remote: ${String(e)}`);
      setRemoteEntries([]);
    }
  };

  onMount(async () => {
    try {
      const home = await invoke<string>("file_home_local");
      setLocalPath(home);
    } catch (e) {
      setLocalPath("C:\\");
    }
    if (p.hasSsh) {
      try {
        const home = await invoke<string>("file_home_remote", {
          workspaceId: p.workspaceId,
        });
        setRemotePath(home || "/");
      } catch {
        setRemotePath("/");
      }
    }
    await refreshLocal();
    await refreshRemote();
  });

  const fmtSize = (n: number) => {
    if (n < 1024) return `${n}B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)}K`;
    if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)}M`;
    return `${(n / 1024 / 1024 / 1024).toFixed(1)}G`;
  };
  const fmtTime = (ts: number) => {
    if (!ts) return "—";
    const d = new Date(ts * 1000);
    const now = Date.now();
    const sec = (now - d.getTime()) / 1000;
    if (sec < 86400) return d.toLocaleTimeString();
    return d.toLocaleDateString();
  };

  const parentOf = (path: string, sep: string): string => {
    const cleaned = path.replace(/[\\/]+$/, "");
    const idx = Math.max(cleaned.lastIndexOf("/"), cleaned.lastIndexOf("\\"));
    if (idx <= 0) return cleaned.length > 1 ? cleaned[0] + sep : cleaned;
    return cleaned.slice(0, idx) || sep;
  };

  const navIntoLocal = (e: FileEntry) => {
    if (!e.is_dir) return;
    const cur = localPath().replace(/[\\/]+$/, "");
    setLocalPath(`${cur}\\${e.name}`);
    void refreshLocal();
  };
  const navIntoRemote = (e: FileEntry) => {
    if (!e.is_dir) return;
    const cur = remotePath().replace(/\/+$/, "");
    setRemotePath(cur === "" ? `/${e.name}` : `${cur}/${e.name}`);
    void refreshRemote();
  };
  const goUp = (side: Side) => {
    if (side === "local") {
      setLocalPath(parentOf(localPath(), "\\"));
      void refreshLocal();
    } else {
      setRemotePath(parentOf(remotePath(), "/") || "/");
      void refreshRemote();
    }
  };

  const fullLocal = (name: string): string => {
    const cur = localPath().replace(/[\\/]+$/, "");
    return `${cur}\\${name}`;
  };
  const fullRemote = (name: string): string => {
    const cur = remotePath().replace(/\/+$/, "");
    return cur === "" ? `/${name}` : `${cur}/${name}`;
  };

  // Phase 17: "Open" handlers. For directories we keep the
  // existing navigation behavior (cd into); for files we ask the OS
  // to open with the default app. Remote files are downloaded to a
  // stable temp path first; the backend returns the temp location
  // which we surface in the status line so the user knows where the
  // copy lives.
  const openLocal = async (e: FileEntry) => {
    if (e.is_dir) {
      navIntoLocal(e);
      return;
    }
    const path = fullLocal(e.name);
    await wrap(`open ${e.name}`, async () => {
      await invoke("file_open_local", { path });
      setStatus(t("fm.toast.opened_local", { file: e.name }));
    });
  };
  const openRemote = async (e: FileEntry) => {
    if (e.is_dir) {
      navIntoRemote(e);
      return;
    }
    const path = fullRemote(e.name);
    await wrap(`open ${e.name}`, async () => {
      const tempPath = await invoke<string>("file_open_remote", {
        workspaceId: p.workspaceId,
        remotePath: path,
      });
      setStatus(t("fm.toast.opened_remote", { file: e.name, temp: tempPath }));
    });
  };

  const wrap = async <T,>(label: string, fn: () => Promise<T>): Promise<T | null> => {
    setBusy(true);
    setStatus(label);
    setErr(null);
    try {
      const r = await fn();
      setStatus(`${label} ✓`);
      return r;
    } catch (e) {
      setErr(`${label}: ${String(e)}`);
      setStatus("");
      return null;
    } finally {
      setBusy(false);
    }
  };

  const uploadSel = async () => {
    const name = localSel();
    if (!name) return;
    const local = fullLocal(name);
    const remote = fullRemote(name);
    const n = await wrap(`upload ${name}`, () =>
      invoke<number>("file_upload", {
        workspaceId: p.workspaceId,
        localPath: local,
        remotePath: remote,
      })
    );
    if (n != null) {
      setStatus(`uploaded ${name} (${fmtSize(n)}) ✓`);
      await refreshRemote();
    }
  };
  const downloadSel = async () => {
    const name = remoteSel();
    if (!name) return;
    const remote = fullRemote(name);
    const local = fullLocal(name);
    const n = await wrap(`download ${name}`, () =>
      invoke<number>("file_download", {
        workspaceId: p.workspaceId,
        remotePath: remote,
        localPath: local,
      })
    );
    if (n != null) {
      setStatus(`downloaded ${name} (${fmtSize(n)}) ✓`);
      await refreshLocal();
    }
  };
  const deleteSel = async (side: Side) => {
    const name = side === "local" ? localSel() : remoteSel();
    if (!name) return;
    if (!window.confirm(t("fm.action.confirm_delete", { name }))) return;
    if (side === "local") {
      const path = fullLocal(name);
      await wrap(`delete ${name}`, () => invoke("file_delete_local", { path }));
      await refreshLocal();
    } else {
      const path = fullRemote(name);
      await wrap(`delete ${name}`, () =>
        invoke("file_delete_remote", { workspaceId: p.workspaceId, path })
      );
      await refreshRemote();
    }
  };
  const renameSel = async (side: Side) => {
    const name = side === "local" ? localSel() : remoteSel();
    if (!name) return;
    const next = window.prompt(t("fm.action.rename_prompt", { name }), name);
    if (!next || next === name) return;
    if (side === "local") {
      await wrap(`rename ${name}`, () =>
        invoke("file_rename_local", {
          oldPath: fullLocal(name),
          newPath: fullLocal(next),
        })
      );
      await refreshLocal();
    } else {
      await wrap(`rename ${name}`, () =>
        invoke("file_rename_remote", {
          workspaceId: p.workspaceId,
          oldPath: fullRemote(name),
          newPath: fullRemote(next),
        })
      );
      await refreshRemote();
    }
  };
  const mkdirIn = async (side: Side) => {
    const name = window.prompt(t("fm.action.mkdir_prompt"));
    if (!name) return;
    if (side === "local") {
      await wrap(`mkdir ${name}`, () =>
        invoke("file_mkdir_local", { path: fullLocal(name) })
      );
      await refreshLocal();
    } else {
      await wrap(`mkdir ${name}`, () =>
        invoke("file_mkdir_remote", {
          workspaceId: p.workspaceId,
          path: fullRemote(name),
        })
      );
      await refreshRemote();
    }
  };

  const ColumnHeader = (props: { side: Side; path: () => string; setPath: (v: string) => void; refresh: () => void }) => (
    <div class="fm-col-head">
      <button class="fm-up" title={t("fm.btn.up")} onClick={() => goUp(props.side)}>↑</button>
      <input
        class="fm-path"
        value={props.path()}
        onChange={(e) => {
          props.setPath(e.currentTarget.value);
          props.refresh();
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            props.setPath((e.target as HTMLInputElement).value);
            props.refresh();
          }
        }}
        spellcheck={false}
      />
      <button class="fm-tool" title={t("fm.btn.refresh")} onClick={props.refresh}>⟳</button>
      <button class="fm-tool" title={t("fm.btn.new_folder")} onClick={() => mkdirIn(props.side)}>＋</button>
    </div>
  );

  const transferDir = createMemo(() => (p.hasSsh ? "Upload ↦ / Download ↤" : ""));
  void transferDir; // currently rendered inline in toolbar

  return (
    <div class="fm-pane">
      <div class="fm-toolbar">
        <label class="fm-checkbox">
          <input
            type="checkbox"
            checked={showHidden()}
            onChange={(e) => {
              setShowHidden(e.currentTarget.checked);
              void refreshLocal();
              void refreshRemote();
            }}
          />
          <span>{t("fm.checkbox.hidden")}</span>
        </label>
        <Show when={p.hasSsh}>
          <button class="fm-action" disabled={!localSel() || busy()} onClick={() => void uploadSel()}>{t("fm.btn.upload")}</button>
          <button class="fm-action" disabled={!remoteSel() || busy()} onClick={() => void downloadSel()}>{t("fm.btn.download")}</button>
          <label class="fm-checkbox">
            <input
              type="checkbox"
              checked={showLocal()}
              onChange={(e) => setShowLocal(e.currentTarget.checked)}
            />
            <span>{t("fm.checkbox.show_local")}</span>
          </label>
        </Show>
        <span class="fm-status">{busy() ? "…" : status()}</span>
        <Show when={err()}>
          <span class="fm-err" title={err()!}>⚠ {err()}</span>
        </Show>
      </div>
      <div class={`fm-grid ${p.hasSsh && showLocal() ? "fm-grid-dual" : "fm-grid-single"}`}>
        {/* Local column — hidden when the user untoggles "Show local"
            and we have an SSH workspace to focus on. */}
        <Show when={!p.hasSsh || showLocal()}>
          <div class="fm-col">
            <ColumnHeader side="local" path={localPath} setPath={setLocalPath} refresh={refreshLocal} />
            <div class="fm-list">
              <For each={localEntries()}>
                {(e) => (
                  <div
                    class={`fm-row ${localSel() === e.name ? "selected" : ""}`}
                    onClick={() => setLocalSel(e.name)}
                    onDblClick={() => void openLocal(e)}
                    onContextMenu={(ev) => {
                      ev.preventDefault();
                      setLocalSel(e.name);
                      const action = window.prompt(
                        t("fm.action.prompt_local", { name: e.name }),
                        "o"
                      );
                      if (action === "o") void openLocal(e);
                      else if (action === "u" && p.hasSsh) void uploadSel();
                      else if (action === "r") void renameSel("local");
                      else if (action === "d") void deleteSel("local");
                    }}
                  >
                    <span class="fm-icon">{e.is_dir ? "📁" : e.is_link ? "🔗" : "📄"}</span>
                    <span class="fm-name">{e.name}</span>
                    <span class="fm-size">{e.is_dir ? "" : fmtSize(e.size)}</span>
                    <span class="fm-time">{fmtTime(e.modified)}</span>
                  </div>
                )}
              </For>
            </div>
          </div>
        </Show>
        {/* Remote column (SSH workspaces only) */}
        <Show when={p.hasSsh}>
          <div class="fm-col">
            <ColumnHeader side="remote" path={remotePath} setPath={setRemotePath} refresh={refreshRemote} />
            <div class="fm-list">
              <Show
                when={remoteEntries().length > 0}
                fallback={
                  <div class="fm-empty">
                    {/* Phase 16: differentiate "SSH workspace, terminal not
                         connected yet" from a true error. The backend
                         returns `no active SSH session` precisely in this
                         shape — surface a friendlier message that points
                         the user at the fix. */}
                    {!p.hasActiveSession
                      ? t("fm.empty.connect_terminal_first")
                      : err()
                      ? t("fm.empty.no_ssh")
                      : t("fm.empty.empty")}
                  </div>
                }
              >
                <For each={remoteEntries()}>
                  {(e) => (
                    <div
                      class={`fm-row ${remoteSel() === e.name ? "selected" : ""}`}
                      onClick={() => setRemoteSel(e.name)}
                      onDblClick={() => void openRemote(e)}
                      onContextMenu={(ev) => {
                        ev.preventDefault();
                        setRemoteSel(e.name);
                        const action = window.prompt(
                          t("fm.action.prompt_remote", { name: e.name }),
                          "o"
                        );
                        if (action === "o") void openRemote(e);
                        else if (action === "d") void downloadSel();
                        else if (action === "r") void renameSel("remote");
                        else if (action === "x") void deleteSel("remote");
                      }}
                    >
                      <span class="fm-icon">{e.is_dir ? "📁" : e.is_link ? "🔗" : "📄"}</span>
                      <span class="fm-name">{e.name}</span>
                      <span class="fm-size">{e.is_dir ? "" : fmtSize(e.size)}</span>
                      <span class="fm-time">{fmtTime(e.modified)}</span>
                    </div>
                  )}
                </For>
              </Show>
            </div>
          </div>
        </Show>
      </div>
    </div>
  );
}
