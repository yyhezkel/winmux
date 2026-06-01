// Phase 50 (#2.4): live unified-diff pane.
//
// Spawns a background tokio task per Diff pane that polls `git diff`
// every POLL_INTERVAL_MS and emits a `diff-pane-updated` event with
// the new text when the output hash changes. Duplicate-suppression is
// done with a cheap u64 fnv-style hash rather than a full string
// compare so a large unchanged diff still costs ~O(n) read but
// zero allocs for the comparison.
//
// The watcher reads the workspace's cwd + DiffSource from
// state.workspaces under a short lock per tick — the cwd may change
// (Phase 49-B worktree re-anchor) and we want subsequent polls to see
// the new path. If the workspace is gone or its layout no longer
// contains this pane id, the task self-terminates.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::{dlog, AppState, DiffSource, LayoutNode};

const POLL_INTERVAL_MS: u64 = 800;

#[derive(Clone, Serialize)]
struct DiffPaneUpdatedEvent {
    pane_id: String,
    diff_text: String,
    is_git_repo: bool,
}

/// Run `git diff [args]` in `cwd` and return the unified-diff stdout.
/// Returns Err if `git` itself fails to spawn or exits non-zero with a
/// stderr we want to surface. A non-git directory returns Err so the
/// caller can fall through to the "not a git repo" UI state.
pub(crate) async fn fetch_diff(cwd: PathBuf, source: &DiffSource) -> Result<String, String> {
    if !cwd.join(".git").exists() {
        return Err(format!("not a git repository: {}", cwd.display()));
    }
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C")
        .arg(&cwd)
        .arg("--no-pager")
        .arg("diff")
        .arg("--no-color");
    match source {
        DiffSource::Working => {} // working tree vs index
        DiffSource::Head => {
            cmd.arg("HEAD");
        }
        DiffSource::Ref { git_ref } => {
            // The ref is a separate arg (no shell concat). git itself
            // rejects unsafe refs.
            cmd.arg(git_ref);
        }
    }
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("spawn git: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return Err(format!("git diff failed: {}", stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Look up the cwd for the workspace that owns `pane_id`. Returns the
/// pane's diff_source as well (None → Working) so the watcher tracks
/// in-flight source changes without re-walking on every tick.
fn lookup_pane_context(state: &AppState, pane_id: &str) -> Option<(PathBuf, DiffSource)> {
    let file = state.workspaces.lock().unwrap();
    for ws in &file.workspaces {
        let layout = ws.layout.as_ref()?;
        if let Some(source) = find_diff_pane(layout, pane_id) {
            let cwd = ws.cwd.clone()?;
            return Some((PathBuf::from(cwd), source));
        }
    }
    None
}

fn find_diff_pane(node: &LayoutNode, target: &str) -> Option<DiffSource> {
    match node {
        LayoutNode::Pane {
            pane_id,
            diff_source,
            ..
        } if pane_id == target => Some(diff_source.clone().unwrap_or_default()),
        LayoutNode::Pane { .. } => None,
        LayoutNode::Split { first, second, .. } => {
            find_diff_pane(first, target).or_else(|| find_diff_pane(second, target))
        }
    }
}

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Spawn (or restart) the watcher for `pane_id`. Aborts any existing
/// task on the same id first. Each tick: look up cwd+source, fetch
/// diff, hash output, emit `diff-pane-updated` only when the hash
/// changed (the very first emit also fires so the FE has something to
/// render). When `lookup_pane_context` returns None (pane gone), the
/// task self-terminates.
pub(crate) fn start_watcher(app: AppHandle, state: AppState, pane_id: String) {
    // Pre-empt any duplicate watcher for this pane.
    stop_watcher_inner(&state, &pane_id);

    let pane_id_for_task = pane_id.clone();
    let app2 = app.clone();
    let state_for_task = state.clone();
    let handle = tokio::spawn(async move {
        let state = state_for_task;
        let mut last_hash: Option<u64> = None;
        let mut last_error: Option<String> = None;
        loop {
            let Some((cwd, source)) = lookup_pane_context(&state, &pane_id_for_task) else {
                // Pane is gone — stop.
                dlog(&format!(
                    "[diff_pane] watcher exiting: pane {} no longer present",
                    pane_id_for_task
                ));
                break;
            };
            match fetch_diff(cwd, &source).await {
                Ok(text) => {
                    last_error = None;
                    let h = hash_str(&text);
                    if Some(h) != last_hash {
                        last_hash = Some(h);
                        let _ = app2.emit(
                            "diff-pane-updated",
                            DiffPaneUpdatedEvent {
                                pane_id: pane_id_for_task.clone(),
                                diff_text: text,
                                is_git_repo: true,
                            },
                        );
                    }
                }
                Err(e) => {
                    // Emit once per distinct error so the FE can show
                    // "not a git repository" without spamming when the
                    // condition is stable.
                    if last_error.as_deref() != Some(e.as_str()) {
                        last_error = Some(e.clone());
                        last_hash = None;
                        let _ = app2.emit(
                            "diff-pane-updated",
                            DiffPaneUpdatedEvent {
                                pane_id: pane_id_for_task.clone(),
                                diff_text: e,
                                is_git_repo: false,
                            },
                        );
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
        }
    });
    state
        .diff_pane_watchers
        .lock()
        .unwrap()
        .insert(pane_id, handle);
}

/// Abort the watcher for `pane_id` if one is running. Idempotent.
fn stop_watcher_inner(state: &AppState, pane_id: &str) {
    if let Some(h) = state.diff_pane_watchers.lock().unwrap().remove(pane_id) {
        h.abort();
    }
}

pub(crate) fn stop_watcher(state: &AppState, pane_id: &str) {
    stop_watcher_inner(state, pane_id)
}

// ─── tauri commands ──────────────────────────────────────────────────

/// Update the `diff_source` field on the Diff pane and restart its
/// watcher. The frontend calls this on mount (with the persisted
/// source) and on every dropdown change.
#[tauri::command]
pub(crate) async fn diff_pane_set_source(
    app: AppHandle,
    state: State<'_, AppState>,
    pane_id: String,
    source: DiffSource,
) -> Result<(), String> {
    // Persist the new source onto the pane in the layout tree.
    {
        let mut file = state.workspaces.lock().unwrap();
        let mut found = false;
        for ws in file.workspaces.iter_mut() {
            if let Some(layout) = ws.layout.as_mut() {
                if set_diff_source_in_layout(layout, &pane_id, source.clone()) {
                    found = true;
                    break;
                }
            }
        }
        if !found {
            return Err(format!("no Diff pane with id {pane_id}"));
        }
    }
    crate::persist(&state)?;
    // (Re)start the watcher with the fresh source.
    start_watcher(app, (*state).clone(), pane_id);
    Ok(())
}

fn set_diff_source_in_layout(node: &mut LayoutNode, target: &str, src: DiffSource) -> bool {
    match node {
        LayoutNode::Pane {
            pane_id,
            diff_source,
            ..
        } if pane_id == target => {
            *diff_source = Some(src);
            true
        }
        LayoutNode::Pane { .. } => false,
        LayoutNode::Split { first, second, .. } => {
            set_diff_source_in_layout(first, target, src.clone())
                || set_diff_source_in_layout(second, target, src)
        }
    }
}

/// One-shot fetch+emit. Used by the manual Refresh button so the user
/// doesn't wait for the next poll tick.
#[tauri::command]
pub(crate) async fn diff_pane_refresh(
    app: AppHandle,
    state: State<'_, AppState>,
    pane_id: String,
) -> Result<(), String> {
    let Some((cwd, source)) = lookup_pane_context(&state, &pane_id) else {
        return Err(format!("no Diff pane with id {pane_id}"));
    };
    match fetch_diff(cwd, &source).await {
        Ok(text) => {
            let _ = app.emit(
                "diff-pane-updated",
                DiffPaneUpdatedEvent {
                    pane_id,
                    diff_text: text,
                    is_git_repo: true,
                },
            );
            Ok(())
        }
        Err(e) => {
            let _ = app.emit(
                "diff-pane-updated",
                DiffPaneUpdatedEvent {
                    pane_id,
                    diff_text: e.clone(),
                    is_git_repo: false,
                },
            );
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn run_git(cwd: &std::path::Path, args: &[&str]) {
        let st = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t.test")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t.test")
            .status()
            .expect("git available for test");
        assert!(st.success(), "git {:?} failed", args);
    }

    #[tokio::test]
    async fn fetch_diff_sees_unstaged_change() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let dir = tmp.path();
        run_git(dir, &["init", "-q", "-b", "main"]);
        fs::write(dir.join("hello.txt"), "one\n").unwrap();
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-q", "-m", "init"]);
        // Modify after commit so `git diff` (working vs index) shows it.
        fs::write(dir.join("hello.txt"), "two\n").unwrap();

        let text = fetch_diff(dir.to_path_buf(), &DiffSource::Working)
            .await
            .expect("diff ok");
        assert!(text.contains("hello.txt"), "diff should mention file");
        assert!(text.contains("-one"), "diff should show old line");
        assert!(text.contains("+two"), "diff should show new line");
    }

    #[tokio::test]
    async fn fetch_diff_errors_for_non_repo() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let err = fetch_diff(tmp.path().to_path_buf(), &DiffSource::Working)
            .await
            .unwrap_err();
        assert!(err.contains("not a git repository"));
    }
}
