import { createEffect, createSignal, For, Show } from "solid-js";
import type { Connection, EnvVar, Workspace } from "./types";

interface Props {
  open: boolean;
  // When set, the modal is in EDIT mode for that workspace; the connection-type
  // fields are read-only (deleting + recreating is required to change).
  editing: Workspace | null;
  onClose: () => void;
  onCreate: (input: {
    name: string;
    connection: Connection;
    color?: string;
    setup_command?: string;
    teardown_command?: string;
    env?: EnvVar[];
  }) => void;
  onUpdate: (
    id: string,
    fields: {
      name?: string;
      color?: string;
      cwd?: string;
      setup_command?: string;
      teardown_command?: string;
      env?: EnvVar[];
    }
  ) => void;
}

export function CreateWorkspaceModal(p: Props) {
  const [name, setName] = createSignal("");
  const [type, setType] = createSignal<"local" | "ssh">("local");
  const [shell, setShell] = createSignal("");
  const [host, setHost] = createSignal("");
  const [user, setUser] = createSignal("");
  const [port, setPort] = createSignal(22);
  const [keyPath, setKeyPath] = createSignal("");
  const [color, setColor] = createSignal("#7aa2f7");
  const [setupCmd, setSetupCmd] = createSignal("");
  const [teardownCmd, setTeardownCmd] = createSignal("");
  const [envRows, setEnvRows] = createSignal<EnvVar[]>([]);

  const isEdit = () => p.editing !== null;

  // Populate from `editing` whenever it (or open) changes.
  createEffect(() => {
    if (p.open) {
      if (p.editing) {
        const w = p.editing;
        setName(w.name);
        setColor(w.color || "#7aa2f7");
        setSetupCmd(w.setup_command || "");
        setTeardownCmd(w.teardown_command || "");
        setEnvRows(w.env ? [...w.env] : []);
        // Connection fields shown read-only.
        const c = w.layout?.kind === "pane" ? w.layout.connection : w.connection;
        if (c?.type === "local") {
          setType("local");
          setShell(c.shell || "");
        } else if (c?.type === "ssh") {
          setType("ssh");
          setHost(c.host);
          setUser(c.user);
          setPort(c.port);
          setKeyPath(c.key_path || "");
        }
      } else {
        // Fresh "new workspace" state.
        setName("");
        setType("local");
        setShell("");
        setHost("");
        setUser("");
        setPort(22);
        setKeyPath("");
        setColor("#7aa2f7");
        setSetupCmd("");
        setTeardownCmd("");
        setEnvRows([]);
      }
    }
  });

  const cleanedEnv = (): EnvVar[] =>
    envRows().filter((r) => r.key.trim() !== "");

  const submit = () => {
    if (!name().trim()) return;

    if (isEdit()) {
      p.onUpdate(p.editing!.id, {
        name: name().trim(),
        color: color(),
        setup_command: setupCmd(),
        teardown_command: teardownCmd(),
        env: cleanedEnv(),
      });
      p.onClose();
      return;
    }

    let connection: Connection;
    if (type() === "local") {
      connection = { type: "local", shell: shell() || undefined };
    } else {
      if (!host().trim() || !user().trim()) return;
      connection = {
        type: "ssh",
        host: host().trim(),
        user: user().trim(),
        port: port(),
        key_path: keyPath() || undefined,
      };
    }
    p.onCreate({
      name: name().trim(),
      connection,
      color: color(),
      setup_command: setupCmd() || undefined,
      teardown_command: teardownCmd() || undefined,
      env: cleanedEnv().length ? cleanedEnv() : undefined,
    });
    p.onClose();
  };

  return (
    <Show when={p.open}>
      <div class="modal-backdrop" onClick={p.onClose}>
        <div class="modal" onClick={(e) => e.stopPropagation()}>
          <h3>{isEdit() ? "Edit workspace" : "New workspace"}</h3>

          <label>
            <span>Name</span>
            <input
              autofocus
              value={name()}
              onInput={(e) => setName(e.currentTarget.value)}
              placeholder="e.g. Local PowerShell"
            />
          </label>

          <label>
            <span>Color</span>
            <input
              type="color"
              value={color()}
              onInput={(e) => setColor(e.currentTarget.value)}
            />
          </label>

          <label>
            <span>Type</span>
            <select
              value={type()}
              onChange={(e) =>
                setType(e.currentTarget.value as "local" | "ssh")
              }
              disabled={isEdit()}
            >
              <option value="local">Local PTY</option>
              <option value="ssh">SSH</option>
            </select>
          </label>

          <Show when={type() === "local"}>
            <label>
              <span>Shell</span>
              <input
                value={shell()}
                onInput={(e) => setShell(e.currentTarget.value)}
                placeholder="(auto: pwsh→powershell→cmd)"
                disabled={isEdit()}
              />
            </label>
          </Show>

          <Show when={type() === "ssh"}>
            <label>
              <span>User</span>
              <input
                value={user()}
                onInput={(e) => setUser(e.currentTarget.value)}
                placeholder="user"
                disabled={isEdit()}
              />
            </label>
            <label>
              <span>Host</span>
              <input
                value={host()}
                onInput={(e) => setHost(e.currentTarget.value)}
                placeholder="example.com or 1.2.3.4"
                disabled={isEdit()}
              />
            </label>
            <label>
              <span>Port</span>
              <input
                type="number"
                value={port()}
                onInput={(e) =>
                  setPort(parseInt(e.currentTarget.value) || 22)
                }
                disabled={isEdit()}
              />
            </label>
            <label>
              <span>Key path</span>
              <input
                value={keyPath()}
                onInput={(e) => setKeyPath(e.currentTarget.value)}
                placeholder="(optional, falls back to ~/.ssh defaults)"
                disabled={isEdit()}
              />
            </label>
          </Show>

          <hr class="modal-sep" />

          <label class="modal-textarea-label">
            <span>Setup cmd</span>
            <textarea
              rows="2"
              value={setupCmd()}
              onInput={(e) => setSetupCmd(e.currentTarget.value)}
              placeholder="run after the shell prompt is ready, e.g. 'cd /repo && nvm use'"
            />
          </label>

          <label class="modal-textarea-label">
            <span>Teardown cmd</span>
            <textarea
              rows="2"
              value={teardownCmd()}
              onInput={(e) => setTeardownCmd(e.currentTarget.value)}
              placeholder="run before disconnect, e.g. 'make clean'"
            />
          </label>

          <div class="env-editor">
            <div class="env-editor-head">
              <span>Env vars</span>
              <button
                class="env-add"
                onClick={() => setEnvRows([...envRows(), { key: "", value: "" }])}
              >
                + add
              </button>
            </div>
            <For each={envRows()}>
              {(row, i) => (
                <div class="env-row">
                  <input
                    placeholder="KEY"
                    value={row.key}
                    onInput={(e) => {
                      const next = [...envRows()];
                      next[i()] = { ...next[i()], key: e.currentTarget.value };
                      setEnvRows(next);
                    }}
                  />
                  <span class="env-eq">=</span>
                  <input
                    placeholder="value"
                    value={row.value}
                    onInput={(e) => {
                      const next = [...envRows()];
                      next[i()] = {
                        ...next[i()],
                        value: e.currentTarget.value,
                      };
                      setEnvRows(next);
                    }}
                  />
                  <button
                    class="env-remove"
                    title="remove"
                    onClick={() => {
                      const next = [...envRows()];
                      next.splice(i(), 1);
                      setEnvRows(next);
                    }}
                  >
                    ×
                  </button>
                </div>
              )}
            </For>
          </div>

          <div class="modal-buttons">
            <button onClick={p.onClose}>Cancel</button>
            <button class="primary" onClick={submit}>
              {isEdit() ? "Save" : "Create"}
            </button>
          </div>
        </div>
      </div>
    </Show>
  );
}
