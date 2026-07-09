import { createSignal, For, Show, createMemo } from "solid-js";
import { t } from "./i18n";
import { IconChevronDown, IconChevronRight, IconRefreshCcw, IconCheck, IconTrash } from "./icons";
import type { Note, Workspace } from "./types";

interface Props {
  open: boolean;
  notes: Note[];
  workspaces: Workspace[];
  activeWorkspaceId: string | null;
  onClose: () => void;
  onAdd: (text: string, tag: string | null, workspaceId: string | null) => void;
  onDone: (id: string) => void;
  onReopen: (id: string) => void;
  onDelete: (id: string) => void;
}

const TAGS = ["", "idea", "bug", "todo"];

export function NotesModal(p: Props) {
  const [text, setText] = createSignal("");
  const [tag, setTag] = createSignal<string>("idea");
  const [filterStatus, setFilterStatus] = createSignal<"open" | "done" | "all">("open");
  const [filterTag, setFilterTag] = createSignal<string>("");
  const [showDoneSection, setShowDoneSection] = createSignal(false);

  // Phase 39: new notes always attach to the active workspace (the
  // window is scoped to it). No active workspace → unassigned.
  const submit = () => {
    const t = text().trim();
    if (!t) return;
    p.onAdd(t, tag() || null, p.activeWorkspaceId);
    setText("");
  };

  const activeName = createMemo(
    () => p.workspaces.find((w) => w.id === p.activeWorkspaceId)?.name ?? null,
  );

  // Phase 39: scope to the active workspace. Legacy notes with no
  // workspace_id (pre-39 global notes) stay visible in every workspace
  // so nothing is lost.
  const visible = createMemo(() => {
    let arr = p.notes.filter(
      (n) => n.workspace_id == null || n.workspace_id === p.activeWorkspaceId,
    );
    if (filterTag()) arr = arr.filter((n) => n.tag === filterTag());
    arr.sort((a, b) => (a.updated_at < b.updated_at ? 1 : -1));
    return arr;
  });

  const openNotes = createMemo(() =>
    visible().filter((n) => n.status === "open")
  );
  const doneNotes = createMemo(() =>
    visible().filter((n) => n.status === "done")
  );

  const wsName = (id?: string | null) => {
    if (!id) return null;
    return p.workspaces.find((w) => w.id === id)?.name ?? id;
  };

  const fmtAge = (iso: string): string => {
    const t = Date.parse(iso);
    if (Number.isNaN(t)) return "";
    const sec = Math.max(1, Math.floor((Date.now() - t) / 1000));
    if (sec < 60) return `${sec}s`;
    const min = Math.floor(sec / 60);
    if (min < 60) return `${min}m`;
    const hr = Math.floor(min / 60);
    if (hr < 24) return `${hr}h`;
    return `${Math.floor(hr / 24)}d`;
  };

  return (
    <Show when={p.open}>
      <div class="modal-backdrop" onClick={p.onClose}>
        <div
          class="modal notes-modal"
          onClick={(e) => e.stopPropagation()}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <h3>{activeName() ? t("notes.window.title", { workspace: activeName()! }) : t("notes.title")}</h3>

          <div class="notes-add">
            <textarea
              class="notes-text"
              placeholder={t("notes.placeholder")}
              rows="3"
              value={text()}
              onInput={(e) => setText(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
                  e.preventDefault();
                  submit();
                } else if (e.key === "Escape") {
                  p.onClose();
                }
              }}
              autofocus
            />
            <div class="notes-add-row">
              <select value={tag()} onChange={(e) => setTag(e.currentTarget.value)}>
                <For each={TAGS}>
                  {(t) => <option value={t}>{t || "(no tag)"}</option>}
                </For>
              </select>
              <button class="primary" onClick={submit} disabled={!text().trim()}>
                Add (Ctrl+Enter)
              </button>
            </div>
          </div>

          <div class="notes-filters">
            <label>
              status:
              <select
                value={filterStatus()}
                onChange={(e) =>
                  setFilterStatus(e.currentTarget.value as "open" | "done" | "all")
                }
              >
                <option value="open">open</option>
                <option value="done">done</option>
                <option value="all">all</option>
              </select>
            </label>
            <label>
              tag:
              <select
                value={filterTag()}
                onChange={(e) => setFilterTag(e.currentTarget.value)}
              >
                <For each={TAGS}>
                  {(t) => <option value={t}>{t || "(any)"}</option>}
                </For>
              </select>
            </label>
            <span class="notes-count">
              {openNotes().length} open · {doneNotes().length} done
            </span>
          </div>

          <div class="notes-list">
            <Show
              when={filterStatus() !== "done"}
              fallback={null}
            >
              <For each={openNotes()}>
                {(n) => (
                  <NoteCard
                    note={n}
                    wsName={wsName(n.workspace_id)}
                    fmtAge={fmtAge}
                    onDone={p.onDone}
                    onReopen={p.onReopen}
                    onDelete={p.onDelete}
                  />
                )}
              </For>
            </Show>

            <Show
              when={filterStatus() === "done" || filterStatus() === "all"}
              fallback={
                <Show when={doneNotes().length > 0 && filterStatus() === "open"}>
                  <button
                    class="notes-show-done"
                    onClick={() => setShowDoneSection(!showDoneSection())}
                  >
                    {showDoneSection() ? <IconChevronDown size={14} /> : <IconChevronRight size={14} />} {doneNotes().length} done note
                    {doneNotes().length === 1 ? "" : "s"}
                  </button>
                  <Show when={showDoneSection()}>
                    <For each={doneNotes()}>
                      {(n) => (
                        <NoteCard
                          note={n}
                          wsName={wsName(n.workspace_id)}
                          fmtAge={fmtAge}
                          onDone={p.onDone}
                          onReopen={p.onReopen}
                          onDelete={p.onDelete}
                        />
                      )}
                    </For>
                  </Show>
                </Show>
              }
            >
              <For each={doneNotes()}>
                {(n) => (
                  <NoteCard
                    note={n}
                    wsName={wsName(n.workspace_id)}
                    fmtAge={fmtAge}
                    onDone={p.onDone}
                    onReopen={p.onReopen}
                    onDelete={p.onDelete}
                  />
                )}
              </For>
            </Show>

            <Show when={visible().length === 0}>
              <p class="notes-empty">{t("notes.empty")}</p>
            </Show>
          </div>

          <div class="modal-buttons">
            <button onClick={p.onClose}>{t("notes.btn.close")}</button>
          </div>
        </div>
      </div>
    </Show>
  );
}

function NoteCard(props: {
  note: Note;
  wsName: string | null;
  fmtAge: (iso: string) => string;
  onDone: (id: string) => void;
  onReopen: (id: string) => void;
  onDelete: (id: string) => void;
}) {
  return (
    <div class={`note-card ${props.note.status === "done" ? "done" : ""}`}>
      <div class="note-head">
        <Show when={props.note.tag}>
          <span class={`note-tag note-tag-${props.note.tag}`}>{props.note.tag}</span>
        </Show>
        <span class="note-age">{props.fmtAge(props.note.updated_at)} ago</span>
        <Show when={props.wsName}>
          <span class="note-context">· {props.wsName}</span>
        </Show>
        <span class="note-actions">
          <Show
            when={props.note.status === "open"}
            fallback={
              <button
                class="note-btn"
                title={t("notes.btn.reopen")}
                onClick={() => props.onReopen(props.note.id)}
              >
                <IconRefreshCcw size={14} />
              </button>
            }
          >
            <button
              class="note-btn"
              title={t("notes.btn.mark_done")}
              onClick={() => props.onDone(props.note.id)}
            >
              <IconCheck size={14} />
            </button>
          </Show>
          <button
            class="note-btn note-delete"
            title={t("notes.btn.delete")}
            onClick={() => {
              if (window.confirm("Delete this note?")) props.onDelete(props.note.id);
            }}
          >
            <IconTrash size={14} />
          </button>
        </span>
      </div>
      <div class="note-text">{props.note.text}</div>
    </div>
  );
}
