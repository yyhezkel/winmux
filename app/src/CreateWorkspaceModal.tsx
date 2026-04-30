import { createSignal, Show } from "solid-js";
import type { Connection } from "./types";

interface Props {
  open: boolean;
  onClose: () => void;
  onCreate: (input: {
    name: string;
    connection: Connection;
    color?: string;
  }) => void;
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

  const reset = () => {
    setName("");
    setType("local");
    setShell("");
    setHost("");
    setUser("");
    setPort(22);
    setKeyPath("");
    setColor("#7aa2f7");
  };

  const submit = () => {
    if (!name().trim()) return;
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
    p.onCreate({ name: name().trim(), connection, color: color() });
    reset();
    p.onClose();
  };

  return (
    <Show when={p.open}>
      <div class="modal-backdrop" onClick={p.onClose}>
        <div class="modal" onClick={(e) => e.stopPropagation()}>
          <h3>New workspace</h3>

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
              />
            </label>
            <label>
              <span>Host</span>
              <input
                value={host()}
                onInput={(e) => setHost(e.currentTarget.value)}
                placeholder="example.com or 1.2.3.4"
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
              />
            </label>
            <label>
              <span>Key path</span>
              <input
                value={keyPath()}
                onInput={(e) => setKeyPath(e.currentTarget.value)}
                placeholder="(optional, falls back to ~/.ssh defaults)"
              />
            </label>
          </Show>

          <div class="modal-buttons">
            <button onClick={p.onClose}>Cancel</button>
            <button class="primary" onClick={submit}>
              Create
            </button>
          </div>
        </div>
      </div>
    </Show>
  );
}
