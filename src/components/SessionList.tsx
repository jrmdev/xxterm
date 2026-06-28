import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { sessions, loadSessions, loadGroups, openCreatePanel, openEditPanel, removeSession, connectWithMasterPassword, openPortForwardPanel } from "../stores/sessions";
import type { Session } from "../stores/sessions";

const COLLAPSED_GROUPS_STORAGE_KEY = "session-group-collapsed-ids";

function loadCollapsedGroups(): Set<number> {
  try {
    const raw = localStorage.getItem(COLLAPSED_GROUPS_STORAGE_KEY);
    if (!raw) return new Set();

    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return new Set();

    return new Set(parsed.map((value) => Number(value)).filter((value) => Number.isFinite(value)));
  } catch {
    return new Set();
  }
}

function saveCollapsedGroups(collapsedGroups: Set<number>) {
  try {
    localStorage.setItem(COLLAPSED_GROUPS_STORAGE_KEY, JSON.stringify([...collapsedGroups]));
  } catch {
  }
}

export default function SessionList() {
  let [contextMenu, setContextMenu] = createSignal<{ x: number; y: number; session: Session } | null>(null);
  const [collapsedGroups, setCollapsedGroups] = createSignal<Set<number>>(loadCollapsedGroups());

  onMount(() => {
    loadSessions();
    loadGroups();
  });

  const handleContextMenu = (e: MouseEvent, session: Session) => {
    e.preventDefault();
    setContextMenu({ x: e.clientX, y: e.clientY, session });
  };

  const closeContextMenu = () => setContextMenu(null);

  createEffect(() => {
    saveCollapsedGroups(collapsedGroups());
  });

  createEffect(() => {
    if (contextMenu()) {
      const handler = (e: MouseEvent) => {
        const target = e.target as HTMLElement;
        if (!target.closest(".context-menu")) {
          closeContextMenu();
        }
      };
      document.addEventListener("click", handler);
      onCleanup(() => document.removeEventListener("click", handler));
    }
  });

  const toggleGroup = (groupId: number) => {
    setCollapsedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(groupId)) {
        next.delete(groupId);
      } else {
        next.add(groupId);
      }
      return next;
    });
  };

  const grouped = createMemo(() => {
    const groups = new Map<number, { name: string; sessions: typeof sessions.list }>();
    const ungrouped: typeof sessions.list = [];

    for (const session of sessions.list) {
      if (session.group_id != null) {
        if (!groups.has(session.group_id)) {
          groups.set(session.group_id, {
            name: session.group_name || `Group ${session.group_id}`,
            sessions: [],
          });
        }
        groups.get(session.group_id)!.sessions.push(session);
      } else {
        ungrouped.push(session);
      }
    }

    return { groups, ungrouped };
  });

  const renderSession = (session: Session, indent = false) => (
    <div
      onContextMenu={(e) => handleContextMenu(e, session)}
      onDblClick={() => connectWithMasterPassword(session.name, session.id)}
      style={{
        padding: "6px 8px",
        "padding-left": indent ? "20px" : "8px",
        cursor: "pointer",
        color: "var(--app-text)",
        "font-size": "12px",
        "border-radius": "4px",
        "margin-bottom": "2px",
        "user-select": "none",
      }}
      onMouseEnter={(e) => (e.currentTarget.style.background = "var(--app-surface-hover)")}
      onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
    >
      <div style={{ "font-weight": "600", "font-size": "13px" }}>{session.name}</div>
      <div style={{ color: "var(--app-muted)", "font-size": "10px", "font-family": "var(--form-mono)", "margin-top": "2px", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>
        {session.username}@{session.host}:{session.port}
      </div>
    </div>
  );

  return (
    <div style={{ padding: "8px" }}>
      <div
        style={{
          display: "flex",
          "justify-content": "space-between",
          "align-items": "center",
          "margin-bottom": "8px",
        }}
      >
        <span style={{ color: "var(--app-text)", "font-size": "11px", "text-transform": "uppercase", "letter-spacing": "1px" }}>
          Sessions
        </span>
        <button
          onClick={openCreatePanel}
          style={{
            background: "transparent",
            border: "none",
            color: "var(--app-muted)",
            cursor: "pointer",
            "font-size": "16px",
            padding: "0 4px",
          }}
          title="New session"
        >
          +
        </button>
      </div>

      <For each={grouped().ungrouped}>
        {(session) => renderSession(session)}
      </For>

      <For each={[...grouped().groups.entries()]}>
        {([groupId, group]) => {
          const isCollapsed = () => collapsedGroups().has(groupId);
          return (
            <div style={{ "margin-bottom": "2px" }}>
              <div
                onClick={() => toggleGroup(groupId)}
                style={{
                  padding: "4px 8px",
                  cursor: "pointer",
                  color: "var(--app-muted)",
                  "font-size": "11px",
                  "font-weight": "600",
                  "text-transform": "uppercase",
                  "letter-spacing": "0.5px",
                  "border-radius": "4px",
                  "user-select": "none",
                  display: "flex",
                  "align-items": "center",
                  gap: "4px",
                  "line-height": "16px",
                }}
                onMouseEnter={(e) => (e.currentTarget.style.background = "var(--app-surface-hover)")}
                onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
              >
                <span style={{ "font-size": "10px", width: "12px", "text-align": "center" }}>
                  {isCollapsed() ? "\u25B6" : "\u25BC"}
                </span>
                <span>{group.name}</span>
                <span style={{ "font-size": "9px", "margin-left": "4px" }}>
                  ({group.sessions.length})
                </span>
              </div>
              <Show when={!isCollapsed()}>
                <For each={group.sessions}>
                  {(session) => renderSession(session, true)}
                </For>
              </Show>
            </div>
          );
        }}
      </For>

      {sessions.list.length === 0 && (
        <div style={{ color: "var(--app-empty)", "font-size": "12px", "text-align": "center", "margin-top": "16px" }}>
          No sessions saved
        </div>
      )}

      {contextMenu() && (
        <div
          class="context-menu"
          style={{
            position: "fixed",
            left: `${contextMenu()!.x}px`,
            top: `${contextMenu()!.y}px`,
            background: "var(--app-surface-alt)",
            border: "1px solid var(--app-border)",
            "border-radius": "4px",
            padding: "4px 0",
            "z-index": "1000",
            "min-width": "140px",
            "box-shadow": "0 4px 12px rgba(0,0,0,0.3)",
          }}
        >
          <MenuItem label="Edit" onClick={() => { openEditPanel(contextMenu()!.session); closeContextMenu(); }} />
          <MenuItem label="Connect" onClick={() => { const s = contextMenu()!.session; connectWithMasterPassword(s.name, s.id); closeContextMenu(); }} />
          <MenuItem label="Port Forwards" onClick={() => { const s = contextMenu()!.session; openPortForwardPanel(s.id); closeContextMenu(); }} />
          <div style={{ height: "1px", background: "var(--app-border)", margin: "4px 0" }} />
          <MenuItem label="Delete" danger onClick={() => { removeSession(contextMenu()!.session.id); closeContextMenu(); }} />
        </div>
      )}
    </div>
  );
}

function MenuItem(props: { label: string; onClick: () => void; danger?: boolean }) {
  return (
    <div
      onClick={props.onClick}
      style={{
        padding: "6px 16px",
        cursor: "pointer",
        "font-size": "12px",
        color: props.danger ? "var(--app-danger)" : "var(--app-text)",
      }}
      onMouseEnter={(e) => (e.currentTarget.style.background = "var(--app-surface-hover)")}
      onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
    >
      {props.label}
    </div>
  );
}
