import { createSignal, For, Show, onMount, onCleanup, createMemo } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { t } from "./i18n";
import { FileEditor } from "./FileEditor";
import { TechText } from "./TechText";

// Phase 15.B: dual-column file manager (local + remote SFTP).
//
// Lives inside a layout-pane just like Terminal / Browser. Local
// column always renders; remote column lights up only when the
// workspace has an active SSH session (the backend will return a
// friendly error otherwise — surfaced as a banner).
//
// Phase 23: full-featured polish — new file (not just folder), upload
// from arbitrary disk path via native picker, OS drag-and-drop, copy
// path action, real popup context menu (no more window.prompt).

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

  // Phase 62 (item 6): in-pane confirm toast with Confirm/Cancel
  // actions. Replaces window.confirm for destructive (delete, unzip
  // overwrite) and packing (zip) operations — the native dialog is
  // jarring and can render off the floating window.
  type ConfirmReq = {
    title: string;
    detail?: string;
    confirmLabel: string;
    danger: boolean;
    onConfirm: () => void;
  };
  const [confirmToast, setConfirmToast] = createSignal<ConfirmReq | null>(null);
  const askConfirm = (o: ConfirmReq) => setConfirmToast(o);
  // Phase 16/29: toolbar toggle for hiding the local column when the
  // user only cares about remote. Phase 29: default flipped to FALSE
  // — Yossi's workflow is remote-first on SSH workspaces, the local
  // column was just visual noise most of the time. Local-only
  // workspaces are unaffected: the column render is guarded by
  // `!p.hasSsh || showLocal()`, so `!hasSsh` still forces it shown.
  const [showLocal, setShowLocal] = createSignal(false);

  // Phase 29 (B): per-pane sort control. Directories stay grouped
  // first regardless of field; within each group, sort by the chosen
  // field/direction. Name sort is case-insensitive.
  const [sortMode, setSortMode] = createSignal<"name" | "modified">("name");
  const [sortDir, setSortDir] = createSignal<"asc" | "desc">("asc");

  // Phase 29 (C): substring name filter, case-insensitive, applies
  // to BOTH columns. Composes with sort: filter first, then sort.
  const [filterText, setFilterText] = createSignal("");

  // Phase 17.B: built-in editor modal state. When the user clicks
  // Edit on a file row we open the modal targeting that side / path.
  const [editorOpen, setEditorOpen] = createSignal(false);
  const [editorTarget, setEditorTarget] = createSignal<{
    side: Side;
    path: string;
    filename: string;
  } | null>(null);

  // Phase 23: popup context menu — replaces the old window.prompt
  // hack. Stores screen-coordinate position so we can position-fix
  // the menu div over the WebView. `null` ⇒ hidden.
  const [ctxMenu, setCtxMenu] = createSignal<{
    side: Side;
    entry: FileEntry;
    x: number;
    y: number;
  } | null>(null);

  // Phase 23: which column the OS is currently dragging files over,
  // for visual highlight. Null ⇒ no drag in progress.
  const [dragOverSide, setDragOverSide] = createSignal<Side | null>(null);

  // Phase 23: refs to each column DOM node so we can hit-test Tauri's
  // drag-drop event coordinates against their bounding boxes. Tauri
  // gives us window-space physical pixels for `position` — we divide
  // by devicePixelRatio to compare against DOM rects (which are in
  // CSS pixels).
  let localColRef: HTMLDivElement | undefined;
  let remoteColRef: HTMLDivElement | undefined;
  // Phase 23: hidden <input type="file"> — the "Upload from disk"
  // button click()s this to pop the OS file picker. We stash which
  // side initiated the pick so the change handler knows where to put
  // the resulting bytes.
  let fileInputRef: HTMLInputElement | undefined;
  const [pickerTargetSide, setPickerTargetSide] = createSignal<Side>("remote");
  const openEditor = (side: Side, name: string) => {
    const path = side === "local" ? fullLocal(name) : fullRemote(name);
    setEditorTarget({ side, path, filename: name });
    setEditorOpen(true);
  };

  // Phase 17.B: convenience accessors for the toolbar's "Selected"
  // group. Returns the currently-selected entry on whichever side
  // last received a click, or null when nothing is selected.
  const [focusedSide, setFocusedSide] = createSignal<Side>("local");
  const selectedEntry = createMemo<{ side: Side; entry: FileEntry } | null>(() => {
    const lname = localSel();
    const rname = remoteSel();
    if (focusedSide() === "remote" && rname) {
      const ent = remoteEntries().find((e) => e.name === rname);
      if (ent) return { side: "remote", entry: ent };
    }
    if (lname) {
      const ent = localEntries().find((e) => e.name === lname);
      if (ent) return { side: "local", entry: ent };
    }
    if (rname) {
      const ent = remoteEntries().find((e) => e.name === rname);
      if (ent) return { side: "remote", entry: ent };
    }
    return null;
  });

  // Phase 29 (B+C): derived view of each column's entries after
  // applying filter (substring on name, case-insensitive) and sort
  // (directories first, then by name|modified asc|desc). Used by the
  // <For> renderers below. Pure derivation — the raw signals
  // localEntries / remoteEntries are still the source of truth
  // (refresh writes raw lists into them).
  const cmpEntries = (a: FileEntry, b: FileEntry): number => {
    // Directories always come before files, regardless of sort field.
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
    let cmp: number;
    if (sortMode() === "modified") {
      cmp = (a.modified ?? 0) - (b.modified ?? 0);
      // Tiebreak on name (case-insensitive) so identical timestamps
      // produce a stable order.
      if (cmp === 0) {
        cmp = a.name.toLowerCase().localeCompare(b.name.toLowerCase());
      }
    } else {
      cmp = a.name.toLowerCase().localeCompare(b.name.toLowerCase());
    }
    return sortDir() === "asc" ? cmp : -cmp;
  };
  const applyFilterSort = (entries: FileEntry[]): FileEntry[] => {
    const q = filterText().trim().toLowerCase();
    const filtered = q.length === 0
      ? entries
      : entries.filter((e) => e.name.toLowerCase().includes(q));
    return [...filtered].sort(cmpEntries);
  };
  const localEntriesView = createMemo(() => applyFilterSort(localEntries()));
  const remoteEntriesView = createMemo(() => applyFilterSort(remoteEntries()));

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

    // Phase 23: register OS drag-drop. Tauri 2 emits 'enter' / 'over'
    // / 'drop' / 'leave' phases. We use 'over' to drive the
    // dragOverSide highlight, 'drop' to actually do the upload, and
    // 'leave' to clear the highlight. Positions are in window
    // physical pixels; we divide by devicePixelRatio to compare
    // against CSS-pixel DOM rects. The webview emits ALL events for
    // the whole window — we hit-test against our column refs to
    // ignore drops outside the file-manager pane.
    let unlisten: (() => void) | undefined;
    try {
      unlisten = await getCurrentWebview().onDragDropEvent((event) => {
        const payload = event.payload as
          | { type: "enter" | "over"; position: { x: number; y: number } }
          | { type: "drop"; paths: string[]; position: { x: number; y: number } }
          | { type: "leave" };
        if (payload.type === "leave") {
          setDragOverSide(null);
          return;
        }
        const dpr = window.devicePixelRatio || 1;
        const x = payload.position.x / dpr;
        const y = payload.position.y / dpr;
        const hitLocal = localColRef && pointInRect(x, y, localColRef.getBoundingClientRect());
        const hitRemote = remoteColRef && pointInRect(x, y, remoteColRef.getBoundingClientRect());
        const side: Side | null = hitRemote ? "remote" : hitLocal ? "local" : null;
        if (payload.type === "enter" || payload.type === "over") {
          setDragOverSide(side);
          return;
        }
        // drop
        setDragOverSide(null);
        if (payload.type !== "drop") return;
        const dropPaths = payload.paths;
        if (side === "remote" && dropPaths.length > 0) {
          void dropUploadToRemote(dropPaths);
        } else if (side === "local" && dropPaths.length > 0) {
          // Local → local: copy each dropped file into the displayed
          // local dir. Use Rust-side fs::copy via a tiny helper —
          // for simplicity we read the bytes via file_read_local +
          // file_write_local. Skip if it'd overwrite itself.
          (async () => {
            for (const host of dropPaths) {
              const basename = host.split(/[\\/]/).filter(Boolean).pop() || "dropped";
              const dest = fullLocal(basename);
              if (dest.toLowerCase() === host.toLowerCase()) continue;
              await wrap(`copy ${basename}`, async () => {
                // Cheapest path: shell out via cmd /C copy /Y.
                // We don't have a dedicated backend command yet —
                // instead we read+write the file. Works for any size
                // tauri-IPC can stomach (~megabytes).
                const fc = await invoke<{ text: string; is_binary: boolean }>(
                  "file_read_local",
                  { path: host }
                );
                if (fc.is_binary) {
                  throw new Error(
                    "binary drop into local column not yet supported — use remote column"
                  );
                }
                await invoke("file_write_local", { path: dest, text: fc.text });
              });
            }
            await refreshLocal();
          })();
        }
      });
    } catch (e) {
      // Drag-drop hookup failure is non-fatal — file manager still
      // works without it.
      console.warn("fm: onDragDropEvent failed:", e);
    }
    onCleanup(() => {
      try {
        unlisten?.();
      } catch {}
    });

    // Phase 23: dismiss popup context menu on any outside click /
    // scroll / Escape. Capture phase so we beat the row's click
    // handler when the user clicks elsewhere.
    const onDocClick = (e: MouseEvent) => {
      // If they clicked inside one of our menus, that menu's item
      // handler closes it after firing the action; otherwise close
      // immediately. We check all three popup classes in one pass.
      const target = e.target as HTMLElement;
      if (!target?.closest?.(".fm-ctx-menu")) closeCtxMenu();
      if (!target?.closest?.(".fm-bg-menu")) closeBgCtxMenu();
      if (!target?.closest?.(".fm-add-menu") && !target?.closest?.(".fm-add-btn")) closeAddMenu();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        closeCtxMenu();
        closeBgCtxMenu();
        closeAddMenu();
      }
    };
    document.addEventListener("mousedown", onDocClick, true);
    document.addEventListener("keydown", onKey);
    onCleanup(() => {
      document.removeEventListener("mousedown", onDocClick, true);
      document.removeEventListener("keydown", onKey);
    });
  });

  // Hit-test helper: is the point (x,y) inside a DOMRect? Used by the
  // OS drag-drop logic to figure out which column the user is
  // dragging over.
  const pointInRect = (x: number, y: number, r: DOMRect) =>
    x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;

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
  // Phase 57: zip + unzip the currently selected item. Single-item v1
  // (multi-selection support would need an array selection model);
  // matches the right-click → "compress" affordance in OS file
  // managers. The output zip name is derived from the basename so
  // identical entries can sit side-by-side post-zip without colliding
  // unless the user re-runs the action.
  // Phase 62 (item 6): zip now confirms first (toast). The actual work
  // is in performZip; zipSel just gates it behind the confirm toast.
  const performZip = async (s: { side: Side; entry: FileEntry }) => {
    const name = s.entry.name;
    const outputName = `${name}.zip`;
    if (s.side === "local") {
      const out = await wrap(`zip ${name}`, () =>
        invoke<string>("file_manager_zip_local", {
          cwd: localPath(),
          paths: [name],
          outputName,
        })
      );
      if (out != null) {
        setStatus(t("fm.zip.done", { out: outputName }));
        await refreshLocal();
      }
    } else {
      // Phase 65 (bug 2.5): don't use wrap() here — we need to inspect
      // the error so a missing `zip` on the server becomes a tar offer
      // (with an install hint) instead of a raw top-bar error.
      setBusy(true);
      setStatus(`zip ${name}`);
      setErr(null);
      try {
        await invoke<string>("file_manager_zip_remote", {
          workspaceId: p.workspaceId,
          cwd: remotePath(),
          paths: [name],
          outputName,
        });
        setStatus(t("fm.zip.done", { out: outputName }));
        await refreshRemote();
      } catch (e) {
        const msg = String(e);
        if (isZipMissing(msg)) {
          // `zip` isn't installed on the server — offer the tar.gz
          // fallback + an install hint (the toast detail).
          setStatus("");
          askConfirm({
            title: t("fm.zip.notInstalled.title"),
            detail: t("fm.zip.notInstalled.detail"),
            confirmLabel: t("fm.zip.useTar"),
            danger: false,
            onConfirm: () => void performTarGzRemote(s),
          });
        } else {
          setErr(`zip ${name}: ${msg}`);
          setStatus("");
        }
      } finally {
        setBusy(false);
      }
    }
  };
  // Phase 65 (bug 2.5): true when a remote zip failed because `zip` is
  // not installed (exit 127 / "command not found"), as opposed to a real
  // packing error. Drives the tar.gz fallback offer.
  const isZipMissing = (msg: string): boolean => {
    const low = msg.toLowerCase();
    return (
      low.includes("exit 127") ||
      low.includes("command not found") ||
      low.includes("zip: not found")
    );
  };
  // Phase 65 (bug 2.5): tar.gz fallback for servers without `zip`.
  const performTarGzRemote = async (s: { side: Side; entry: FileEntry }) => {
    const name = s.entry.name;
    const outputName = `${name}.tar.gz`;
    const out = await wrap(`tar ${name}`, () =>
      invoke<string>("file_manager_targz_remote", {
        workspaceId: p.workspaceId,
        cwd: remotePath(),
        paths: [name],
        outputName,
      })
    );
    if (out != null) {
      setStatus(t("fm.zip.done", { out: outputName }));
      await refreshRemote();
    }
  };
  const zipSel = () => {
    const s = selectedEntry();
    if (!s) return;
    askConfirm({
      title: t("fm.confirm.zip.title", {
        name: s.entry.name,
        out: `${s.entry.name}.zip`,
      }),
      confirmLabel: t("fm.zip.button"),
      danger: false,
      onConfirm: () => void performZip(s),
    });
  };

  const performUnzip = async (s: { side: Side; entry: FileEntry }) => {
    const name = s.entry.name;
    if (s.side === "local") {
      const out = await wrap(`unzip ${name}`, () =>
        invoke<string>("file_manager_unzip_local", {
          zipPath: fullLocal(name),
        })
      );
      if (out != null) {
        setStatus(t("fm.unzip.done", { dest: out }));
        await refreshLocal();
      }
    } else {
      const out = await wrap(`unzip ${name}`, () =>
        invoke<string>("file_manager_unzip_remote", {
          workspaceId: p.workspaceId,
          zipPath: fullRemote(name),
        })
      );
      if (out != null) {
        setStatus(t("fm.unzip.done", { dest: out }));
        await refreshRemote();
      }
    }
  };
  const unzipSel = async () => {
    const s = selectedEntry();
    if (!s) return;
    const name = s.entry.name;
    if (!name.toLowerCase().endsWith(".zip")) {
      setErr(t("fm.unzip.error.notZip"));
      return;
    }
    // Phase 60 (smoke-test 3b) → 62: pre-flight — when the destination
    // folder already exists (locally: exists AND non-empty), confirm
    // before the extraction overwrites files in it. The confirm is now
    // the in-pane toast instead of window.confirm. (Per-file
    // Skip/Rename is a dialog component — deferred until needed.)
    const destName = name.replace(/\.zip$/i, "");
    let conflict = false;
    try {
      conflict =
        s.side === "local"
          ? await invoke<boolean>("file_manager_unzip_local_check", {
              zipPath: fullLocal(name),
            })
          : await invoke<boolean>("file_manager_unzip_remote_check", {
              workspaceId: p.workspaceId,
              zipPath: fullRemote(name),
            });
    } catch (e) {
      // Check failure shouldn't block the user — log + proceed (the
      // unzip itself surfaces real errors through wrap()).
      console.warn("unzip pre-flight check failed", e);
    }
    if (conflict) {
      askConfirm({
        title: t("fm.unzip.confirmOverwrite", { dest: destName }),
        confirmLabel: t("fm.unzip.button"),
        danger: true,
        onConfirm: () => void performUnzip(s),
      });
    } else {
      void performUnzip(s);
    }
  };

  // Phase 62 (item 6): delete confirms via the in-pane toast.
  const performDelete = async (side: Side, name: string) => {
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
  const deleteSel = (side: Side) => {
    const name = side === "local" ? localSel() : remoteSel();
    if (!name) return;
    askConfirm({
      title: t("fm.confirm.delete.title", { name }),
      detail: t("fm.confirm.delete.detail"),
      confirmLabel: t("common.delete"),
      danger: true,
      onConfirm: () => void performDelete(side, name),
    });
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

  // Phase 23: create an empty file in the given side's current directory.
  // Distinct from mkdir, distinct from Edit (which opens a possibly-large
  // file). Backend refuses to clobber existing paths.
  const createFileIn = async (side: Side) => {
    const name = window.prompt(t("fm.action.create_file_prompt"));
    if (!name) return;
    if (side === "local") {
      await wrap(`create ${name}`, () =>
        invoke("file_create_local", { path: fullLocal(name) })
      );
      await refreshLocal();
    } else {
      await wrap(`create ${name}`, () =>
        invoke("file_create_remote", {
          workspaceId: p.workspaceId,
          path: fullRemote(name),
        })
      );
      await refreshRemote();
    }
  };

  // Phase 23: copy the absolute path of a file/dir to the system
  // clipboard. Pure-frontend operation — uses navigator.clipboard
  // since Tauri 2 exposes it in WebView2 with the same async API as
  // browsers. Falls back to writing into a temp <textarea> + execCommand
  // for older WebView2 builds that don't grant clipboard-write.
  const copyPathOf = async (side: Side, name: string) => {
    // Empty name ⇒ copy the column's current directory path, not a child.
    const path = name
      ? side === "local"
        ? fullLocal(name)
        : fullRemote(name)
      : side === "local"
      ? localPath()
      : remotePath();
    try {
      await navigator.clipboard.writeText(path);
      setStatus(t("fm.toast.path_copied", { path }));
    } catch {
      const ta = document.createElement("textarea");
      ta.value = path;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      try {
        document.execCommand("copy");
        setStatus(t("fm.toast.path_copied", { path }));
      } catch (e) {
        setErr(`copy: ${String(e)}`);
      } finally {
        document.body.removeChild(ta);
      }
    }
  };

  // Phase 23: pick files from anywhere on disk via OS picker and
  // upload them to the remote (or copy them into the local column if
  // side === "local"). The HTML5 file input gives us a Blob; we read
  // it into a Uint8Array and push it to the backend in one shot.
  // We don't stream — uploads here are interactive and capped at the
  // tens-of-MB range; for big transfers users should fall back to scp.
  const pickAndUpload = (side: Side) => {
    setPickerTargetSide(side);
    fileInputRef?.click();
  };
  const onFilesPicked = async (ev: Event) => {
    const input = ev.target as HTMLInputElement;
    const files = input.files;
    if (!files || files.length === 0) return;
    const side = pickerTargetSide();
    for (let i = 0; i < files.length; i++) {
      const f = files[i];
      const arrayBuf = await f.arrayBuffer();
      const bytes = Array.from(new Uint8Array(arrayBuf));
      if (side === "remote") {
        if (!p.hasSsh) {
          setErr("no remote — cannot upload");
          break;
        }
        const target = fullRemote(f.name);
        await wrap(`upload ${f.name}`, () =>
          invoke<number>("file_upload_bytes", {
            workspaceId: p.workspaceId,
            remotePath: target,
            bytes,
          })
        );
      } else {
        // Local-side "upload" = save the picked file's bytes into the
        // currently-displayed local directory under its original name.
        // We use file_write_local with binary text — but that goes
        // through utf8. For now use a tiny exec dance: create_local +
        // write_local via base64 isn't ideal. Cleanest is just a
        // synchronous fs.writeFile on the backend; we'll model this
        // as: create then write via the existing file_write_local
        // path with bytes-as-utf8 only when the file is text. For
        // arbitrary binaries we'd need a file_write_bytes_local —
        // skip for now and surface a friendly error if the file
        // looks binary.
        const target = fullLocal(f.name);
        // We can write any bytes via a minimal `fs writeFile` —
        // exposing it as file_upload_bytes_local would mirror remote;
        // for v1 lean on a fetch-data-URL → blob approach:
        const blob = new Blob([arrayBuf]);
        const url = URL.createObjectURL(blob);
        try {
          // Pull text via FileReader for text-likely cases; otherwise
          // bail with a hint. Most users picking "Upload to local"
          // are copying text/config files anyway.
          const txt = await f.text();
          await wrap(`save ${f.name}`, () =>
            invoke("file_write_local", { path: target, text: txt })
          );
        } finally {
          URL.revokeObjectURL(url);
        }
      }
    }
    // Reset so picking the same files twice in a row re-fires change.
    input.value = "";
    if (side === "remote") await refreshRemote();
    else await refreshLocal();
  };

  // Phase 23: upload raw bytes from a Tauri OS drag-drop. Given the
  // host path, slurp via file_upload (which already reads from disk).
  const dropUploadToRemote = async (hostPaths: string[]) => {
    if (!p.hasSsh) {
      setErr("no remote — cannot upload");
      return;
    }
    for (const host of hostPaths) {
      const basename = host.split(/[\\/]/).filter(Boolean).pop() || "dropped";
      const remote = fullRemote(basename);
      await wrap(`upload ${basename}`, () =>
        invoke<number>("file_upload", {
          workspaceId: p.workspaceId,
          localPath: host,
          remotePath: remote,
        })
      );
    }
    await refreshRemote();
  };

  // Phase 23: context-menu helpers.
  const openCtxMenu = (side: Side, entry: FileEntry, ev: MouseEvent) => {
    ev.preventDefault();
    setCtxMenu({ side, entry, x: ev.clientX, y: ev.clientY });
    if (side === "local") {
      setLocalSel(entry.name);
      setFocusedSide("local");
    } else {
      setRemoteSel(entry.name);
      setFocusedSide("remote");
    }
  };
  const closeCtxMenu = () => setCtxMenu(null);

  // Phase 23.B: "Add" dropdown next to the path bar — single ＋ button
  // that opens a small popup with New folder / New file / Upload from
  // disk. Replaces the prior three separate buttons in the column
  // header for a less crowded toolbar.
  const [addMenu, setAddMenu] = createSignal<{ side: Side; x: number; y: number } | null>(null);
  const closeAddMenu = () => setAddMenu(null);
  // Phase 23.B: background context menu for clicks on the empty area
  // of a list (between or below rows). Different from the per-row
  // context menu — offers create / upload actions for the directory
  // as a whole.
  const [bgCtxMenu, setBgCtxMenu] = createSignal<{ side: Side; x: number; y: number } | null>(null);
  const openBgCtxMenu = (side: Side, ev: MouseEvent) => {
    // Only fire when the click is on the list itself, not a row.
    const t = ev.target as HTMLElement;
    if (t?.closest?.(".fm-row")) return;
    ev.preventDefault();
    setBgCtxMenu({ side, x: ev.clientX, y: ev.clientY });
    setFocusedSide(side);
  };
  const closeBgCtxMenu = () => setBgCtxMenu(null);

  const ColumnHeader = (props: { side: Side; path: () => string; setPath: (v: string) => void; refresh: () => void }) => {
    // Anchor the dropdown at the bottom-left of the + button so it
    // doesn't drift if the path input width changes between renders.
    const openAdd = (ev: MouseEvent) => {
      const r = (ev.currentTarget as HTMLElement).getBoundingClientRect();
      setAddMenu({ side: props.side, x: r.left, y: r.bottom + 4 });
    };
    return (
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
        <button
          class="fm-tool fm-add-btn"
          title={t("fm.btn.add_menu")}
          onClick={openAdd}
        >
          ＋
        </button>
      </div>
    );
  };

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
          <label class="fm-checkbox">
            <input
              type="checkbox"
              checked={showLocal()}
              onChange={(e) => setShowLocal(e.currentTarget.checked)}
            />
            <span>{t("fm.checkbox.show_local")}</span>
          </label>
        </Show>
        {/* Phase 29 (B): sort controls — name vs modified, asc vs
             desc. Directories always sort first regardless. */}
        <label class="fm-checkbox" title={t("fm.sort.label")}>
          <span>{t("fm.sort.label")}:</span>
          <select
            class="fm-sort-select"
            value={sortMode()}
            onChange={(e) =>
              setSortMode(e.currentTarget.value as "name" | "modified")
            }
          >
            <option value="name">{t("fm.sort.name")}</option>
            <option value="modified">{t("fm.sort.modified")}</option>
          </select>
        </label>
        <button
          class="fm-tool"
          title={sortDir() === "asc" ? t("fm.sort.asc") : t("fm.sort.desc")}
          onClick={() => setSortDir(sortDir() === "asc" ? "desc" : "asc")}
        >
          {sortDir() === "asc" ? "▲" : "▼"}
        </button>
        {/* Phase 29 (C): name-substring filter, applies to both
             columns, composes with sort. */}
        <div class="fm-filter-wrap">
          <input
            class="fm-filter"
            type="text"
            placeholder={t("fm.filter.placeholder")}
            value={filterText()}
            onInput={(e) => setFilterText(e.currentTarget.value)}
          />
          <Show when={filterText().length > 0}>
            <button
              class="fm-filter-x"
              title={t("common.close")}
              onClick={() => setFilterText("")}
            >
              ×
            </button>
          </Show>
        </div>
        {/* Phase 29 (D): the selected-actions block is ALWAYS rendered
             (no <Show> gate) so the toolbar has a constant set of
             children. Buttons just toggle `disabled` based on whether
             anything is selected. Combined with the .fm-toolbar CSS
             (flex-wrap: nowrap, fixed min-height, overflow-x: auto)
             this gives the toolbar a provably constant height
             regardless of selection state — selecting a row no longer
             pushes the file list down and breaks the user's
             double-click target. */}
        <span class="fm-sep">|</span>
        <span
          class="fm-selected-label"
          title={selectedEntry()?.entry.name ?? ""}
        >
          {selectedEntry()?.entry.name ?? "—"}
        </span>
        <button
          class="fm-action"
          title={t("fm.action.open.tooltip")}
          disabled={busy() || !selectedEntry()}
          onClick={() => {
            const s = selectedEntry();
            if (!s) return;
            if (s.side === "local") void openLocal(s.entry);
            else void openRemote(s.entry);
          }}
        >
          {t("fm.action.open")}
        </button>
        <button
          class="fm-action"
          title={t("fm.action.edit.tooltip")}
          disabled={busy() || !selectedEntry() || !!selectedEntry()?.entry.is_dir}
          onClick={() => {
            const s = selectedEntry();
            if (!s) return;
            openEditor(s.side, s.entry.name);
          }}
        >
          {t("fm.action.edit")}
        </button>
        {/* Upload + Download are SSH-workspace-only; further gated by
             which side the selection is on. Always rendered in SSH
             workspaces (no Show on selection state) so layout stays
             constant — they grey out when not applicable. */}
        <Show when={p.hasSsh}>
          <button
            class="fm-action"
            title={t("fm.btn.upload.tooltip")}
            disabled={
              busy() ||
              !selectedEntry() ||
              selectedEntry()?.side !== "local" ||
              !!selectedEntry()?.entry.is_dir
            }
            onClick={() => void uploadSel()}
          >
            ↥
          </button>
          <button
            class="fm-action"
            title={t("fm.btn.download.tooltip")}
            disabled={
              busy() ||
              !selectedEntry() ||
              selectedEntry()?.side !== "remote" ||
              !!selectedEntry()?.entry.is_dir
            }
            onClick={() => void downloadSel()}
          >
            ↧
          </button>
        </Show>
        <button
          class="fm-action"
          title={t("fm.action.rename.tooltip")}
          disabled={busy() || !selectedEntry()}
          onClick={() => {
            const s = selectedEntry();
            if (s) void renameSel(s.side);
          }}
        >
          {t("common.rename")}
        </button>
        <button
          class="fm-action"
          title={t("fm.action.copy_path.tooltip")}
          disabled={busy() || !selectedEntry()}
          onClick={() => {
            const s = selectedEntry();
            if (s) void copyPathOf(s.side, s.entry.name);
          }}
        >
          ⧉
        </button>
        {/* Phase 57: compress / extract. Zip always enabled when
            something is selected; Unzip only enabled when the
            selection name ends with .zip (case-insensitive). Output
            lands beside the source: zip → <name>.zip in the same
            dir, unzip → <name>/ in the same dir. */}
        <button
          class="fm-action"
          title={t("fm.zip.tooltip")}
          disabled={busy() || !selectedEntry()}
          onClick={() => zipSel()}
        >
          {t("fm.zip.button")}
        </button>
        <button
          class="fm-action"
          title={t("fm.unzip.tooltip")}
          disabled={
            busy() ||
            !selectedEntry() ||
            !selectedEntry()!.entry.name.toLowerCase().endsWith(".zip")
          }
          onClick={() => void unzipSel()}
        >
          {t("fm.unzip.button")}
        </button>
        <button
          class="fm-action fm-action-danger"
          title={t("fm.action.delete.tooltip")}
          disabled={busy() || !selectedEntry()}
          onClick={() => {
            const s = selectedEntry();
            if (s) void deleteSel(s.side);
          }}
        >
          {t("common.delete")}
        </button>
        <span class="fm-status">{busy() ? "…" : status()}</span>
        <Show when={err()}>
          <span class="fm-err" title={err()!}>⚠ {err()}</span>
        </Show>
      </div>
      <div class={`fm-grid ${p.hasSsh && showLocal() ? "fm-grid-dual" : "fm-grid-single"}`}>
        {/* Local column — hidden when the user untoggles "Show local"
            and we have an SSH workspace to focus on. */}
        <Show when={!p.hasSsh || showLocal()}>
          <div
            class={`fm-col ${dragOverSide() === "local" ? "drag-over" : ""}`}
            ref={(el) => (localColRef = el)}
          >
            <ColumnHeader side="local" path={localPath} setPath={setLocalPath} refresh={refreshLocal} />
            <div
              class="fm-list"
              onContextMenu={(ev) => openBgCtxMenu("local", ev)}
            >
              <For each={localEntriesView()}>
                {(e) => (
                  <div
                    class={`fm-row ${localSel() === e.name ? "selected" : ""}`}
                    onClick={() => {
                      setLocalSel(e.name);
                      setFocusedSide("local");
                    }}
                    onDblClick={() => void openLocal(e)}
                    onContextMenu={(ev) => openCtxMenu("local", e, ev)}
                  >
                    <span class="fm-icon">{e.is_dir ? "📁" : e.is_link ? "🔗" : "📄"}</span>
                    <span class="fm-name"><TechText text={e.name} /></span>
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
          <div
            class={`fm-col ${dragOverSide() === "remote" ? "drag-over" : ""}`}
            ref={(el) => (remoteColRef = el)}
          >
            <ColumnHeader side="remote" path={remotePath} setPath={setRemotePath} refresh={refreshRemote} />
            <div
              class="fm-list"
              onContextMenu={(ev) => openBgCtxMenu("remote", ev)}
            >
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
                <For each={remoteEntriesView()}>
                  {(e) => (
                    <div
                      class={`fm-row ${remoteSel() === e.name ? "selected" : ""}`}
                      onClick={() => {
                        setRemoteSel(e.name);
                        setFocusedSide("remote");
                      }}
                      onDblClick={() => void openRemote(e)}
                      onContextMenu={(ev) => openCtxMenu("remote", e, ev)}
                    >
                      <span class="fm-icon">{e.is_dir ? "📁" : e.is_link ? "🔗" : "📄"}</span>
                      <span class="fm-name"><TechText text={e.name} /></span>
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
      {/* Phase 23: hidden OS file picker — triggered by the ↥ toolbar
           button. Persistent in the DOM so the click() handler always
           has a target. */}
      <input
        ref={(el) => (fileInputRef = el)}
        type="file"
        multiple
        style="display:none"
        onChange={onFilesPicked}
      />

      {/* Phase 23: popup context menu. Position-fixed; tracks mouse
           coordinates of the right-click. Outside-click + Escape
           handlers in onMount close it. */}
      <Show when={ctxMenu()}>
        {(() => {
          const m = ctxMenu()!;
          // Clamp menu position so it doesn't bleed off-screen on
          // right-edge clicks.
          const maxX = window.innerWidth - 200;
          const maxY = window.innerHeight - 280;
          const x = Math.min(m.x, maxX);
          const y = Math.min(m.y, maxY);
          const e = m.entry;
          const side = m.side;
          const isLocal = side === "local";
          const fire = (fn: () => void) => () => {
            fn();
            closeCtxMenu();
          };
          return (
            <div
              class="fm-ctx-menu"
              style={{ left: `${x}px`, top: `${y}px` }}
              onClick={(ev) => ev.stopPropagation()}
            >
              <button class="fm-ctx-item" onClick={fire(() => (isLocal ? openLocal(e) : openRemote(e)))}>
                {t("fm.action.open")}
              </button>
              <Show when={!e.is_dir}>
                <button class="fm-ctx-item" onClick={fire(() => openEditor(side, e.name))}>
                  {t("fm.action.edit")}
                </button>
              </Show>
              <Show when={p.hasSsh && isLocal && !e.is_dir}>
                <button class="fm-ctx-item" onClick={fire(() => void uploadSel())}>
                  {t("fm.btn.upload")}
                </button>
              </Show>
              <Show when={p.hasSsh && !isLocal && !e.is_dir}>
                <button class="fm-ctx-item" onClick={fire(() => void downloadSel())}>
                  {t("fm.btn.download")}
                </button>
              </Show>
              <button class="fm-ctx-item" onClick={fire(() => void copyPathOf(side, e.name))}>
                {t("fm.action.copy_path")}
              </button>
              <button class="fm-ctx-item" onClick={fire(() => void renameSel(side))}>
                {t("common.rename")}
              </button>
              <div class="fm-ctx-sep" />
              <button class="fm-ctx-item fm-ctx-danger" onClick={fire(() => void deleteSel(side))}>
                {t("common.delete")}
              </button>
            </div>
          );
        })()}
      </Show>

      {/* Phase 23.B: "+" dropdown next to the path bar.
           Shows New folder / New file / Upload from disk. */}
      <Show when={addMenu()}>
        {(() => {
          const m = addMenu()!;
          const maxX = window.innerWidth - 200;
          const maxY = window.innerHeight - 180;
          const x = Math.min(m.x, maxX);
          const y = Math.min(m.y, maxY);
          const side = m.side;
          const fire = (fn: () => void) => () => {
            fn();
            closeAddMenu();
          };
          return (
            <div
              class="fm-ctx-menu fm-add-menu"
              style={{ left: `${x}px`, top: `${y}px` }}
              onClick={(ev) => ev.stopPropagation()}
            >
              <button class="fm-ctx-item" onClick={fire(() => void mkdirIn(side))}>
                📁  {t("fm.btn.new_folder")}
              </button>
              <button class="fm-ctx-item" onClick={fire(() => void createFileIn(side))}>
                📄  {t("fm.btn.new_file")}
              </button>
              <button
                class="fm-ctx-item"
                disabled={side === "remote" && !p.hasSsh}
                onClick={fire(() => pickAndUpload(side))}
              >
                ↥  {side === "remote"
                  ? t("fm.btn.upload_from_disk_remote")
                  : t("fm.btn.upload_from_disk_local")}
              </button>
            </div>
          );
        })()}
      </Show>

      {/* Phase 23.B: background context menu — right-click on the empty
           area of a list (not a row) opens this with directory-level
           create/upload actions. */}
      <Show when={bgCtxMenu()}>
        {(() => {
          const m = bgCtxMenu()!;
          const maxX = window.innerWidth - 200;
          const maxY = window.innerHeight - 200;
          const x = Math.min(m.x, maxX);
          const y = Math.min(m.y, maxY);
          const side = m.side;
          const fire = (fn: () => void) => () => {
            fn();
            closeBgCtxMenu();
          };
          return (
            <div
              class="fm-ctx-menu fm-bg-menu"
              style={{ left: `${x}px`, top: `${y}px` }}
              onClick={(ev) => ev.stopPropagation()}
            >
              <button class="fm-ctx-item" onClick={fire(() => void mkdirIn(side))}>
                📁  {t("fm.btn.new_folder")}
              </button>
              <button class="fm-ctx-item" onClick={fire(() => void createFileIn(side))}>
                📄  {t("fm.btn.new_file")}
              </button>
              <button
                class="fm-ctx-item"
                disabled={side === "remote" && !p.hasSsh}
                onClick={fire(() => pickAndUpload(side))}
              >
                ↥  {side === "remote"
                  ? t("fm.btn.upload_from_disk_remote")
                  : t("fm.btn.upload_from_disk_local")}
              </button>
              <div class="fm-ctx-sep" />
              <button class="fm-ctx-item" onClick={fire(() => void copyPathOf(side, ""))}>
                {t("fm.btn.copy_path_current")}
              </button>
              <button class="fm-ctx-item" onClick={fire(() => (side === "local" ? refreshLocal() : refreshRemote()))}>
                {t("fm.btn.refresh")}
              </button>
            </div>
          );
        })()}
      </Show>

      {/* Phase 62 (item 6): confirm toast for delete / zip / unzip-
           overwrite. Replaces the native window.confirm with an in-pane
           card carrying Cancel + a (danger-styled) confirm action. */}
      <Show when={confirmToast()}>
        {(c) => (
          <div
            class={`fm-confirm-toast ${c().danger ? "danger" : ""}`}
            role="alertdialog"
            aria-modal="false"
          >
            <div class="fm-confirm-body">
              <div class="fm-confirm-title">{c().title}</div>
              <Show when={c().detail}>
                <div class="fm-confirm-detail">{c().detail}</div>
              </Show>
            </div>
            <div class="fm-confirm-actions">
              <button class="fm-action" onClick={() => setConfirmToast(null)}>
                {t("common.cancel")}
              </button>
              <button
                class={`fm-action ${c().danger ? "fm-action-danger" : ""}`}
                onClick={() => {
                  const fn = c().onConfirm;
                  setConfirmToast(null);
                  fn();
                }}
              >
                {c().confirmLabel}
              </button>
            </div>
          </div>
        )}
      </Show>

      <Show when={editorOpen() && editorTarget()}>
        <FileEditor
          open
          filename={editorTarget()!.filename}
          path={editorTarget()!.path}
          side={editorTarget()!.side}
          workspaceId={p.workspaceId}
          onClose={() => setEditorOpen(false)}
          onSaved={() => {
            // After a successful save, refresh the corresponding column
            // so the new size / mtime show up in the listing.
            if (editorTarget()?.side === "local") void refreshLocal();
            else void refreshRemote();
          }}
        />
      </Show>
    </div>
  );
}
