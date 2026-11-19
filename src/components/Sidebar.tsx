import { createEffect, createSignal, lazy, onCleanup, Suspense } from "solid-js";
import SessionList from "./SessionList";
import { openSettingsPanel } from "../stores/settings";
import { fileBrowser, setSidebarPane, syncSidebarWithActiveTerminal } from "../stores/fileBrowser";
import { getActiveTerminal } from "../stores/terminals";

const FileBrowser = lazy(() => import("./FileBrowser"));
const SidebarForwards = lazy(() => import("./SidebarForwards"));

export default function Sidebar() {
  const [collapsed, setCollapsed] = createSignal(false);
  const [sidebarWidth, setSidebarWidth] = createSignal(240);
  let sidebarRef: HTMLDivElement | undefined;
  let stopDragging: (() => void) | undefined;
  onCleanup(() => stopDragging?.());

  createEffect(() => {
    const active = getActiveTerminal();
    void active?.id;
    void active?.connectionState;
    syncSidebarWithActiveTerminal();
  });

  const onMouseDown = (e: MouseEvent) => {
    if (e.button !== 0 || collapsed()) return;
    e.preventDefault();
    stopDragging?.();
    const startX = e.clientX;
    const startWidth = sidebarWidth();
    let pointerX = startX;
    let frame: number | undefined;

    const onMouseMove = (ev: MouseEvent) => {
      pointerX = ev.clientX;
      if (frame !== undefined) return;
      frame = requestAnimationFrame(() => {
        frame = undefined;
        setSidebarWidth(Math.max(100, Math.min(600, startWidth + pointerX - startX)));
      });
    };

    const onMouseUp = (ev: MouseEvent) => {
      if (ev.button !== 0) return;
      stopDragging?.();
      setSidebarWidth(Math.max(100, Math.min(600, startWidth + ev.clientX - startX)));
    };

    stopDragging = () => {
      if (frame !== undefined) cancelAnimationFrame(frame);
      document.removeEventListener("mousemove", onMouseMove);
      document.removeEventListener("mouseup", onMouseUp);
      stopDragging = undefined;
    };

    document.addEventListener("mousemove", onMouseMove);
    document.addEventListener("mouseup", onMouseUp);
  };

  const buttonStyle = {
    background: "transparent",
    border: "none",
    color: "var(--app-subtle)",
    cursor: "pointer",
    "font-size": "14px",
    padding: "2px 6px",
    width: "24px",
    height: "24px",
    display: "flex",
    "align-items": "center",
    "justify-content": "center",
    "flex-shrink": 0,
  } as const;

  return (
    <div
      ref={sidebarRef}
      style={{
        width: collapsed() ? "32px" : `${sidebarWidth()}px`,
        background: "var(--app-background)",
        "border-right": "1px solid var(--app-border)",
        display: "flex",
        "flex-direction": "column",
        "min-height": "100%",
        position: "relative",
      }}
    >
      <div
        style={{
          display: "flex",
          "justify-content": collapsed() ? "center" : "space-between",
          "align-items": "center",
          padding: "4px",
        }}
      >
        {!collapsed() && (
          <div style={{ display: "flex", gap: "4px" }}>
            <button
              onClick={openSettingsPanel}
              style={{ ...buttonStyle, color: "var(--app-text)" }}
              title="Settings"
            >
              {"\u2699"}
            </button>
            <button
              onClick={() => setSidebarPane("sessions")}
              style={{ ...buttonStyle, color: fileBrowser.activePane === "sessions" ? "var(--app-text)" : "var(--app-subtle)", background: fileBrowser.activePane === "sessions" ? "var(--app-surface-hover)" : "transparent", "border-radius": "4px" }}
              title="Sessions"
            >
              {"\u25A3"}
            </button>
            <button
              onClick={() => setSidebarPane("sftp")}
              style={{ ...buttonStyle, color: fileBrowser.activePane === "sftp" ? "var(--app-text)" : "var(--app-subtle)", background: fileBrowser.activePane === "sftp" ? "var(--app-surface-hover)" : "transparent", "border-radius": "4px" }}
              title="SFTP"
            >
              {"\u21C5"}
            </button>
            <button
              onClick={() => setSidebarPane("forwards")}
              style={{ ...buttonStyle, color: fileBrowser.activePane === "forwards" ? "var(--app-text)" : "var(--app-subtle)", background: fileBrowser.activePane === "forwards" ? "var(--app-surface-hover)" : "transparent", "border-radius": "4px" }}
              title="Forwards"
            >
              {"\u21C4"}
            </button>
          </div>
        )}
        <button
          onClick={() => setCollapsed(!collapsed())}
          style={buttonStyle}
          title={collapsed() ? "Expand sidebar" : "Collapse sidebar"}
        >
          {collapsed() ? "\u25B6" : "\u25C0"}
        </button>
      </div>
      {!collapsed() && (
        <div style={{ flex: 1, overflow: "auto" }}>
          <Suspense>
            {fileBrowser.activePane === "sessions"
              ? <SessionList />
              : fileBrowser.activePane === "sftp"
                ? <FileBrowser />
                : <SidebarForwards />}
          </Suspense>
        </div>
      )}
      <div
        onMouseDown={onMouseDown}
        style={{
          position: "absolute",
          top: 0,
          right: 0,
          bottom: 0,
          width: "4px",
          cursor: collapsed() ? "default" : "col-resize",
          "z-index": 10,
        }}
      />
    </div>
  );
}
