import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { fileBrowser } from "../stores/fileBrowser";
import {
  loadSidebarPortForwards,
  portForwards,
  refreshSidebarPortForwardStatus,
  startSinglePortForward,
  stopSinglePortForward,
  type BoundPortForward,
} from "../stores/portForwards";
import { loadSessions, sessions } from "../stores/sessions";
import { listingIconStyle, listingMetaStyle, listingRowStyle } from "./sidebarListStyles";

function forwardLabel(forward: BoundPortForward) {
  return forward.forward_type === "dynamic" ? "dynamic" : forward.forward_type;
}

function forwardDefinition(forward: BoundPortForward) {
  if (forward.forward_type === "local") {
    return `${forward.local_host || "127.0.0.1"}:${forward.local_port} -> ${forward.remote_host || "?"}:${forward.remote_port || "?"}`;
  }
  if (forward.forward_type === "dynamic") {
    return `${forward.local_host || "127.0.0.1"}:${forward.local_port} (SOCKS5)`;
  }
  return `${forward.remote_host || "?"}:${forward.remote_port || "?"} <- ${forward.local_host || "127.0.0.1"}:${forward.local_port}`;
}

export default function SidebarForwards() {
  const [loadingIds, setLoadingIds] = createSignal(new Set<number>());
  const sessionList = createMemo(() => sessions.list);
  const forwards = createMemo(() => portForwards.sidebarList);

  const reload = async () => {
    if (sessions.list.length === 0) {
      await loadSessions();
    }
    await loadSidebarPortForwards(sessions.list);
  };

  const refreshStatuses = async () => {
    if (sessions.list.length === 0) return;
    await refreshSidebarPortForwardStatus(sessions.list);
  };

  const withLoading = async (id: number, fn: () => Promise<void>) => {
    setLoadingIds((prev) => new Set(prev).add(id));
    try {
      await fn();
    } finally {
      setLoadingIds((prev) => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      });
    }
  };

  const handleStart = async (forward: BoundPortForward) => {
    await withLoading(forward.id, async () => {
      try {
        await startSinglePortForward(forward.session_id, forward.id);
      } catch (error) {
        console.error("Failed to start port forward:", error);
      } finally {
        await refreshStatuses();
      }
    });
  };

  const handleStop = async (forward: BoundPortForward) => {
    await withLoading(forward.id, async () => {
      try {
        await stopSinglePortForward(forward.session_id, forward.id);
      } catch (error) {
        console.error("Failed to stop port forward:", error);
      } finally {
        await refreshStatuses();
      }
    });
  };

  onMount(() => {
    void reload();
  });

  createEffect(() => {
    if (fileBrowser.activePane !== "forwards") return;
    void sessionList().length;
    if (!portForwards.sidebarLoading && portForwards.sidebarList.length === 0) {
      void reload();
    }
  });

  let pollTimer: ReturnType<typeof setInterval> | undefined;
  createEffect(() => {
    if (fileBrowser.activePane !== "forwards") {
      if (pollTimer) {
        clearInterval(pollTimer);
        pollTimer = undefined;
      }
      return;
    }
    if (pollTimer) return;
    pollTimer = setInterval(() => {
      void refreshStatuses();
    }, 2000);
  });

  onCleanup(() => {
    if (pollTimer) clearInterval(pollTimer);
  });

  return (
    <div style={{ display: "flex", "flex-direction": "column", height: "100%", color: "var(--app-text)" }}>
      <div style={{ padding: "8px", "border-bottom": "1px solid var(--app-border)", display: "flex", "justify-content": "space-between", "align-items": "center" }}>
        <span style={{ color: "var(--app-text)", "font-size": "11px", "text-transform": "uppercase", "letter-spacing": "1px" }}>Forwards</span>
        <button style={toolbarButtonStyle} onClick={() => void reload()} title="Refresh forwards">{"\u21BB"}</button>
      </div>

      <div style={{ flex: 1, overflow: "auto" }}>
        <Show when={!portForwards.sidebarLoading && forwards().length === 0}>
          <div style={{ color: "var(--app-empty)", "font-size": "12px", padding: "16px", "text-align": "center" }}>
            No port forwards configured.
          </div>
        </Show>
        <For each={forwards()}>
          {(forward) => {
            const active = () => forward.status === "active";
            const pending = () => forward.status === "pending" || forward.status === "starting";
            const loading = () => loadingIds().has(forward.id);
            return (
              <div style={listingRowStyle}>
                <span style={{ ...listingIconStyle, color: active() ? "var(--app-success)" : "var(--app-muted)" }}>
                  {active() ? "\u25CF" : "\u25CB"}
                </span>
                <div style={{ overflow: "hidden" }}>
                  <div style={{ display: "flex", gap: "6px", "align-items": "baseline", overflow: "hidden" }}>
                    <span style={{ "font-size": "13px", "text-transform": "capitalize", color: "var(--app-text)" }}>{forwardLabel(forward)}</span>
                    <span style={{ color: "var(--app-subtle)", "font-size": "12px", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>
                      {forward.sessionName}
                    </span>
                  </div>
                  <div style={listingMetaStyle}>{forwardDefinition(forward)}</div>
                  <Show when={forward.error}>
                    <div style={{ color: "var(--app-danger)", "font-size": "10px", "margin-top": "2px", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>
                      {forward.error}
                    </div>
                  </Show>
                </div>
                <div style={{ display: "flex", gap: "6px", "align-items": "center", "align-self": "center" }}>
                  <button
                    style={{
                      ...actionButtonStyle,
                      color: active() || pending() || loading() ? "var(--app-muted)" : "var(--app-success)",
                      opacity: active() || pending() || loading() ? "0.55" : "1",
                    }}
                    disabled={active() || pending() || loading()}
                    onClick={() => void handleStart(forward)}
                    title="Start"
                    aria-label="Start"
                  >
                    {"\u25B6"}
                  </button>
                  <button
                    style={{
                      ...actionButtonStyle,
                      color: (!active() && !pending()) || loading() ? "var(--app-muted)" : "var(--app-danger)",
                      opacity: (!active() && !pending()) || loading() ? "0.55" : "1",
                    }}
                    disabled={(!active() && !pending()) || loading()}
                    onClick={() => void handleStop(forward)}
                    title="Stop"
                    aria-label="Stop"
                  >
                    {"\u25A0"}
                  </button>
                </div>
              </div>
            );
          }}
        </For>
      </div>
    </div>
  );
}

const toolbarButtonStyle = {
  background: "var(--app-input)",
  border: "1px solid var(--app-input-border)",
  color: "var(--app-text)",
  cursor: "pointer",
  "font-size": "12px",
  "border-radius": "4px",
  width: "26px",
  height: "26px",
} as const;

const actionButtonStyle = {
  background: "var(--app-input)",
  border: "1px solid var(--app-input-border)",
  color: "var(--app-text)",
  cursor: "pointer",
  "font-size": "11px",
  "border-radius": "4px",
  padding: "0",
  width: "24px",
  height: "24px",
} as const;
