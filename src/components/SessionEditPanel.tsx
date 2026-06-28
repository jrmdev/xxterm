import { createSignal, onMount, onCleanup, For, Show } from "solid-js";
import { open } from "@tauri-apps/plugin-dialog";
import { sessions, closeEditPanel, saveSession, editSession, loadGroups, createGroup } from "../stores/sessions";
import { Field } from "./settingsPanelParts";

interface FormData {
  name: string;
  host: string;
  port: number;
  username: string;
  auth_method: string;
  agent_forwarding: boolean;
  private_key_path: string;
  jump_host_session_id: string;
  startup_command: string;
  password: string;
  group_id: string;
}

export default function SessionEditPanel() {
  const [form, setForm] = createSignal<FormData>({
    name: "",
    host: "",
    port: 22,
    username: "",
    auth_method: "password",
    agent_forwarding: false,
    private_key_path: "",
    jump_host_session_id: "",
    startup_command: "",
    password: "",
    group_id: "",
  });
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal("");
  const [showAddGroup, setShowAddGroup] = createSignal(false);
  const [newGroupName, setNewGroupName] = createSignal("");
  const [groupError, setGroupError] = createSignal("");

  onMount(async () => {
    await loadGroups();
    const editing = sessions.editing;
    if (editing) {
      setForm({
        name: editing.name,
        host: editing.host,
        port: editing.port,
        username: editing.username,
        auth_method: editing.auth_method,
        agent_forwarding: editing.agent_forwarding,
        private_key_path: editing.private_key_path || "",
        jump_host_session_id: editing.jump_host_session_id ? String(editing.jump_host_session_id) : "",
        startup_command: editing.startup_command || "",
        password: "",
        group_id: editing.group_id ? String(editing.group_id) : "",
      });
    }

    const handler = (e: KeyboardEvent) => { if (e.key === "Escape") closeEditPanel(); };
    document.addEventListener("keydown", handler);
    onCleanup(() => document.removeEventListener("keydown", handler));
  });

  const handleSubmit = async (e: Event) => {
    e.preventDefault();
    setSaving(true);
    setError("");
    try {
      const f = form();
      const input = {
        name: f.name,
        host: f.host,
        port: f.port,
        username: f.username,
        auth_method: f.auth_method,
        agent_forwarding: f.auth_method === "agent" && f.agent_forwarding,
        private_key_path: f.private_key_path || null,
        jump_host_session_id: f.jump_host_session_id ? parseInt(f.jump_host_session_id, 10) : null,
        startup_command: f.startup_command || null,
        group_id: f.group_id ? parseInt(f.group_id, 10) : null,
      };
      if (sessions.editing) {
        await editSession(sessions.editing.id, input, f.password || undefined);
      } else {
        await saveSession(input, f.password || undefined);
      }
      closeEditPanel();
    } catch (e: any) {
      setError(e.toString());
    } finally {
      setSaving(false);
    }
  };

  const set = (field: keyof FormData, value: any) => {
    setForm((prev) => ({ ...prev, [field]: value }));
  };

  const handleBrowsePrivateKeyPath = async () => {
    setError("");
    try {
      const currentValue = form().private_key_path.trim();
      const defaultPath = /[\\/]/.test(currentValue) || /^[a-zA-Z]:/.test(currentValue) ? currentValue : undefined;
      const selected = await open({
        multiple: false,
        directory: false,
        title: "Select Private Key",
        defaultPath,
      });
      if (typeof selected === "string" && selected.length > 0) {
        set("private_key_path", selected);
      }
    } catch (e: any) {
      setError(e.toString());
    }
  };

  const isEditing = !!sessions.editing;
  const jumpHostOptions = () => sessions.list.filter((session) => session.id !== sessions.editing?.id);

  const handleAddGroup = async () => {
    const name = newGroupName().trim();
    if (!name) {
      setGroupError("Group name is required");
      return;
    }
    try {
      const group = await createGroup(name);
      set("group_id", String(group.id));
      setShowAddGroup(false);
      setNewGroupName("");
      setGroupError("");
    } catch (e: any) {
      setGroupError(e.toString());
    }
  };

  const handleGroupChange = (value: string) => {
    if (value === "__add_new__") {
      setShowAddGroup(true);
      setGroupError("");
      setNewGroupName("");
      return;
    }
    set("group_id", value);
    setShowAddGroup(false);
  };

  return (
    <div
      style={{
        position: "fixed",
        top: 0,
        left: 0,
        right: 0,
        bottom: 0,
        background: "var(--app-overlay)",
        display: "flex",
        "align-items": "center",
        "justify-content": "center",
        "z-index": "2000",
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
        closeEditPanel();
      }}
    >
      <div
        style={{
          padding: "16px",
          width: "min(calc(100vw - 32px), 500px)",
          "min-width": 0,
        }}
        class="form-panel form-panel--scrollable"
        onContextMenu={(e) => {
          e.preventDefault();
          e.stopPropagation();
        }}
      >
        <div class="form-panel__header">
          <h2 class="form-panel__title">{isEditing ? "Edit Session" : "New Session"}</h2>
          <button class="form-icon-button" type="button" onClick={closeEditPanel} title="Close">x</button>
        </div>

        <form onSubmit={handleSubmit}>
          <Field label="Name">
            <input class="form-input" type="text" value={form().name} onInput={(e) => set("name", e.currentTarget.value)} required />
          </Field>

          <div class="form-row">
            <div style={{ flex: 1 }}>
              <Field label="Host">
                <input class="form-input" type="text" value={form().host} onInput={(e) => set("host", e.currentTarget.value)} required />
              </Field>
            </div>
            <div style={{ width: "100px" }}>
              <Field label="Port">
                <input class="form-input" type="number" value={form().port} onInput={(e) => set("port", parseInt(e.currentTarget.value) || 22)} min={1} max={65535} />
              </Field>
            </div>
          </div>

          <Field label="Username">
            <input class="form-input" type="text" value={form().username} onInput={(e) => set("username", e.currentTarget.value)} required />
          </Field>

          <Field label="Auth Method">
            <select
              class="form-select"
              value={form().auth_method}
              onChange={(e) => {
                const authMethod = e.currentTarget.value;
                set("auth_method", authMethod);
                if (authMethod !== "agent") {
                  set("agent_forwarding", false);
                }
              }}
            >
              <option value="password">Password</option>
              <option value="key">Private Key</option>
              <option value="agent">SSH Agent</option>
            </select>
          </Field>

          {form().auth_method === "password" && (
            <Field label="Password">
              <input
                class="form-input"
                type="password"
                value={form().password}
                onInput={(e) => set("password", e.currentTarget.value)}
                placeholder={isEditing ? "Leave blank to keep existing" : ""}
              />
            </Field>
          )}

          {form().auth_method === "key" && (
            <Field label="Private Key Path">
              <div style={{ display: "flex", gap: "8px", "align-items": "stretch" }}>
                <input
                  class="form-input"
                  type="text"
                  value={form().private_key_path}
                  readOnly
                  placeholder="No private key selected"
                />
                <button
                  type="button"
                  onClick={handleBrowsePrivateKeyPath}
                  class="form-button form-button--secondary"
                  style={{ "flex-shrink": 0 }}
                >
                  Browse
                </button>
                <button
                  type="button"
                  onClick={() => set("private_key_path", "")}
                  disabled={!form().private_key_path.trim()}
                  class="form-button form-button--secondary"
                  style={{ "flex-shrink": 0 }}
                >
                  Clear
                </button>
              </div>
            </Field>
          )}

          {form().auth_method === "agent" && (
            <Field label="">
              <label class="form-checkbox-label">
                <input
                  class="form-checkbox"
                  type="checkbox"
                  checked={form().agent_forwarding}
                  onChange={(e) => set("agent_forwarding", e.currentTarget.checked)}
                />
                <span>Enable agent forwarding</span>
              </label>
            </Field>
          )}

          <Field label="Startup Command (optional)">
            <input
              class="form-input"
              type="text"
              value={form().startup_command}
              onInput={(e) => set("startup_command", e.currentTarget.value)}
              placeholder="e.g. tmux attach"
            />
          </Field>

          <Field label="Jump Host (optional)">
            <select class="form-select" value={form().jump_host_session_id} onChange={(e) => set("jump_host_session_id", e.currentTarget.value)}>
              <option value="">None</option>
              <For each={jumpHostOptions()}>
                {(session) => (
                  <option value={session.id}>
                    {session.name} ({session.username}@{session.host}:{session.port})
                  </option>
                )}
              </For>
            </select>
          </Field>

          <Field label="Group (optional)">
            <select class="form-select" value={showAddGroup() ? "__add_new__" : form().group_id} onChange={(e) => handleGroupChange(e.currentTarget.value)}>
              <option value="">None</option>
              <For each={sessions.groups}>
                {(group) => (
                  <option value={group.id}>{group.name}</option>
                )}
              </For>
              <option value="__add_new__" style={{ color: "var(--app-accent)" }}>+ Add new...</option>
            </select>
            <Show when={showAddGroup()}>
              <div style={{ display: "flex", gap: "8px", "align-items": "flex-start", "margin-top": "8px" }}>
                <input
                  class="form-input"
                  type="text"
                  value={newGroupName()}
                  onInput={(e) => { setNewGroupName(e.currentTarget.value); setGroupError(""); }}
                  placeholder="Enter group name"
                  style={{ flex: 1, "margin-bottom": 0 }}
                  onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); handleAddGroup(); } }}
                />
                <button
                  type="button"
                  onClick={handleAddGroup}
                  class="form-button form-button--primary"
                  style={{ "flex-shrink": 0 }}
                >
                  Save
                </button>
              </div>
              {groupError() && (
                <div class="form-error" style={{ "margin-top": "4px" }}>{groupError()}</div>
              )}
            </Show>
          </Field>

          {error() && (
            <div class="form-error" style={{ "margin-bottom": "12px" }}>
              {error()}
            </div>
          )}

          <div style={{ display: "flex", gap: "8px", "justify-content": "flex-end" }}>
            <button
              type="button"
              onClick={closeEditPanel}
              class="form-button form-button--secondary"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={saving()}
              class="form-button form-button--primary"
            >
              {saving() ? "Saving..." : isEditing ? "Update" : "Create"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}

