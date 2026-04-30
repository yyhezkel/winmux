//! Phase 7.B: notes / ideas capture.
//!
//! Persisted in `%APPDATA%\winmux\notes.json` next to `workspaces.json`. Same
//! atomic-write + load-poison-gate pattern as workspaces (so a corrupt or
//! mid-write file never silently loses everything). Mutations emit `notes:changed`
//! to the frontend and are exposed both as Tauri commands and over JSON-RPC so
//! the CLI on the remote pane can drop a note through the tunnel.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::{config_dir_pub, dlog, AppState};

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum NoteStatus {
    Open,
    Done,
}

impl Default for NoteStatus {
    fn default() -> Self {
        NoteStatus::Open
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Note {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tag: Option<String>,
    #[serde(default)]
    pub(crate) status: NoteStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pane_id: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub(crate) struct NotesFile {
    #[serde(default = "default_version")]
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) notes: Vec<Note>,
}

fn default_version() -> u32 {
    1
}

fn notes_path() -> Result<PathBuf, String> {
    Ok(config_dir_pub()?.join("notes.json"))
}

fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn next_note_id() -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    format!("n_{:x}_{:x}", t, n)
}

fn save_notes_to_disk(file: &NotesFile) -> Result<(), String> {
    use std::io::Write as _;

    let path = notes_path()?;
    let dir = path
        .parent()
        .ok_or_else(|| "no parent dir".to_string())?
        .to_path_buf();
    let tmp = dir.join(format!("notes.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| format!("open tmp {:?}: {e}", tmp))?;
        f.write_all(text.as_bytes())
            .map_err(|e| format!("write tmp: {e}"))?;
        f.sync_all().map_err(|e| format!("fsync: {e}"))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    dlog(&format!(
        "notes save: {} bytes ({} notes) -> {:?}",
        text.len(),
        file.notes.len(),
        path
    ));
    Ok(())
}

pub(crate) fn load_notes_from_disk() -> Result<NotesFile, String> {
    let path = notes_path()?;
    if !path.exists() {
        return Ok(NotesFile {
            version: 1,
            notes: Vec::new(),
        });
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read {:?}: {e}", path))?;
    let file: NotesFile = serde_json::from_str(text.trim_start_matches('\u{FEFF}'))
        .map_err(|e| format!("parse {:?}: {e}", path))?;
    Ok(file)
}

fn persist_notes(state: &AppState) -> Result<(), String> {
    use crate::LoadState;
    let load_state = *state.load_state.lock().unwrap();
    if load_state == Some(LoadState::Failed) {
        return Err("notes persist refused: workspaces load_state=Failed".into());
    }
    let file = state.notes.lock().unwrap().clone();
    save_notes_to_disk(&file)
}

/// Apply a function to the notes file in place; persist on success and
/// emit `notes:changed`. Returns the (cloned) updated file for the caller.
fn mutate_notes<F: FnOnce(&mut NotesFile) -> Result<(), String>>(
    state: &AppState,
    app: &AppHandle,
    f: F,
) -> Result<NotesFile, String> {
    {
        let mut nf = state.notes.lock().unwrap();
        f(&mut nf)?;
    }
    persist_notes(state)?;
    let _ = app.emit("notes:changed", ());
    Ok(state.notes.lock().unwrap().clone())
}

#[tauri::command]
pub(crate) fn notes_load(state: State<'_, AppState>) -> NotesFile {
    state.notes.lock().unwrap().clone()
}

#[tauri::command]
pub(crate) fn notes_add(
    state: State<'_, AppState>,
    app: AppHandle,
    text: String,
    tag: Option<String>,
    workspace_id: Option<String>,
    pane_id: Option<String>,
) -> Result<Note, String> {
    let now = iso_now();
    let note = Note {
        id: next_note_id(),
        created_at: now.clone(),
        updated_at: now,
        text,
        tag: tag.filter(|s| !s.is_empty()),
        status: NoteStatus::Open,
        workspace_id: workspace_id.filter(|s| !s.is_empty()),
        pane_id: pane_id.filter(|s| !s.is_empty()),
    };
    let added = note.clone();
    mutate_notes(&state, &app, |nf| {
        nf.notes.push(note);
        Ok(())
    })?;
    Ok(added)
}

#[tauri::command]
pub(crate) fn notes_update(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    text: Option<String>,
    tag: Option<Option<String>>,
    status: Option<NoteStatus>,
) -> Result<Note, String> {
    let updated = mutate_notes(&state, &app, |nf| {
        let n = nf
            .notes
            .iter_mut()
            .find(|n| n.id == id)
            .ok_or_else(|| format!("no note {id}"))?;
        if let Some(t) = text {
            n.text = t;
        }
        if let Some(tg) = tag {
            n.tag = tg.filter(|s| !s.is_empty());
        }
        if let Some(st) = status {
            n.status = st;
        }
        n.updated_at = iso_now();
        Ok(())
    })?;
    let n = updated
        .notes
        .into_iter()
        .find(|n| n.id == id)
        .ok_or_else(|| "race: note vanished after update".to_string())?;
    Ok(n)
}

#[tauri::command]
pub(crate) fn notes_delete(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
) -> Result<(), String> {
    mutate_notes(&state, &app, |nf| {
        let len_before = nf.notes.len();
        nf.notes.retain(|n| n.id != id);
        if nf.notes.len() == len_before {
            return Err(format!("no note {id}"));
        }
        Ok(())
    })?;
    Ok(())
}

// ─── Helpers exposed to the RPC dispatch ────────────────────────────────────

pub(crate) fn list_filtered(
    state: &AppState,
    tag: Option<&str>,
    status: Option<NoteStatus>,
    workspace_id: Option<&str>,
    limit: Option<usize>,
) -> Vec<Note> {
    let nf = state.notes.lock().unwrap();
    let mut out: Vec<Note> = nf
        .notes
        .iter()
        .filter(|n| tag.map_or(true, |t| n.tag.as_deref() == Some(t)))
        .filter(|n| status.as_ref().map_or(true, |s| &n.status == s))
        .filter(|n| {
            workspace_id.map_or(true, |w| n.workspace_id.as_deref() == Some(w))
        })
        .cloned()
        .collect();
    // Most-recently-updated first.
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    if let Some(lim) = limit {
        out.truncate(lim);
    }
    out
}

pub(crate) fn rpc_add(
    state: &AppState,
    app: &AppHandle,
    text: String,
    tag: Option<String>,
    workspace_id: Option<String>,
    pane_id: Option<String>,
) -> Result<Note, String> {
    let now = iso_now();
    let note = Note {
        id: next_note_id(),
        created_at: now.clone(),
        updated_at: now,
        text,
        tag: tag.filter(|s| !s.is_empty()),
        status: NoteStatus::Open,
        workspace_id: workspace_id.filter(|s| !s.is_empty()),
        pane_id: pane_id.filter(|s| !s.is_empty()),
    };
    let added = note.clone();
    mutate_notes(state, app, |nf| {
        nf.notes.push(note);
        Ok(())
    })?;
    Ok(added)
}

pub(crate) fn rpc_update(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    text: Option<String>,
    tag: Option<Option<String>>,
    status: Option<NoteStatus>,
) -> Result<Note, String> {
    let updated = mutate_notes(state, app, |nf| {
        let n = nf
            .notes
            .iter_mut()
            .find(|n| n.id == id)
            .ok_or_else(|| format!("no note {id}"))?;
        if let Some(t) = text {
            n.text = t;
        }
        if let Some(tg) = tag {
            n.tag = tg.filter(|s| !s.is_empty());
        }
        if let Some(st) = status {
            n.status = st;
        }
        n.updated_at = iso_now();
        Ok(())
    })?;
    updated
        .notes
        .into_iter()
        .find(|n| n.id == id)
        .ok_or_else(|| "post-update: note vanished".into())
}

pub(crate) fn rpc_delete(state: &AppState, app: &AppHandle, id: &str) -> Result<(), String> {
    mutate_notes(state, app, |nf| {
        let len_before = nf.notes.len();
        nf.notes.retain(|n| n.id != id);
        if nf.notes.len() == len_before {
            return Err(format!("no note {id}"));
        }
        Ok(())
    })?;
    Ok(())
}
