import { createEffect, createSignal, For, lazy, onCleanup, onMount, Show, Suspense } from "solid-js";
import { open } from "@tauri-apps/plugin-dialog";
import { terminals, addTerminal, removeTerminal, renameTerminal, reorderTerminals, setActiveTerminal, setTerminalUnseenActivity } from "../stores/terminals";
import type { Terminal } from "../stores/terminals";
import {
  addTerminalProfile,
  deleteTerminalProfile,
  detectedPlatform,
  getDefaultTerminalProfile,
  getTerminalProfiles,
  initPlatformProfiles,
  isDefaultTerminalProfile,
  setDefaultTerminalProfile,
  updateTerminalProfile,
  type TerminalProfile,
} from "../stores/terminalProfiles";
import { setSidebarPane, syncSidebarWithActiveTerminal } from "../stores/fileBrowser";
import { getStartupAdminTerminal, openAdminTerminal, type AdminShell, type AdminTerminal } from "../ipc/commands";

const TerminalTab = lazy(() => import("./TerminalTab"));

const windowsAdminProfiles: { shell: AdminShell; name: string }[] = [
  { shell: "cmd", name: "Command Prompt (Admin)" },
  { shell: "powershell", name: "PowerShell (Admin)" },
];

function addAdminTab({ shell, program }: AdminTerminal) {
  const profile = windowsAdminProfiles.find((profile) => profile.shell === shell);
  if (!profile) return;
  addTerminal(undefined, "local", undefined, {
    id: `windows:admin:${shell}`,
    name: profile.name,
    program,
    args: "",
    defaultWorkingDirectory: "",
    platform: "windows",
  });
}

export default function TerminalPane() {
  const [profileMenu, setProfileMenu] = createSignal<{ x: number; y: number } | null>(null);
  const [showProfileDialog, setShowProfileDialog] = createSignal(false);
  const [editingProfileId, setEditingProfileId] = createSignal<string | null>(null);
  const [profileName, setProfileName] = createSignal("");
  const [profileProgram, setProfileProgram] = createSignal("");
  const [profileArgs, setProfileArgs] = createSignal("");
  const [profileWorkingDirectory, setProfileWorkingDirectory] = createSignal("");
  const [profileError, setProfileError] = createSignal("");
  const [adminLaunchError, setAdminLaunchError] = createSignal("");
  const [tabContextMenu, setTabContextMenu] = createSignal<{ x: number; y: number; tabId: string; label: string } | null>(null);
  const [showRenameDialog, setShowRenameDialog] = createSignal(false);
  const [renameTabId, setRenameTabId] = createSignal<string | null>(null);
  const [renameLabel, setRenameLabel] = createSignal("");
  const [draggedTabId, setDraggedTabId] = createSignal<string | null>(null);
  let renameInputRef: HTMLInputElement | undefined;
  let tabBarRef: HTMLDivElement | undefined;
  let dragState: { tabId: string; pointerId: number; startX: number; dragging: boolean } | null = null;
  let suppressNextTabClick = false;

  onMount(async () => {
    await initPlatformProfiles();
    if (detectedPlatform !== "windows") return;
    try {
      const terminal = await getStartupAdminTerminal();
      if (terminal) addAdminTab(terminal);
    } catch (error) {
      setAdminLaunchError(String(error));
    }
  });

  createEffect(() => {
    if (!profileMenu()) return;
    const handler = (event: MouseEvent) => {
      const target = event.target as HTMLElement;
      if (!target.closest(".new-terminal-menu")) {
        setProfileMenu(null);
      }
    };
    document.addEventListener("click", handler);
    onCleanup(() => document.removeEventListener("click", handler));
  });

  createEffect(() => {
    if (!tabContextMenu()) return;
    const handler = (event: MouseEvent) => {
      const target = event.target as HTMLElement;
      if (!target.closest(".tab-context-menu")) {
        setTabContextMenu(null);
      }
    };
    document.addEventListener("click", handler);
    onCleanup(() => document.removeEventListener("click", handler));
  });

  const closeTab = (id: string) => {
    removeTerminal(id);
    syncSidebarWithActiveTerminal(true);
  };

  const activateTab = (tab: Terminal) => {
    setActiveTerminal(tab.id);
    setTerminalUnseenActivity(tab.id, false);
    if (tab.type === "ssh" && tab.sessionId) {
      setSidebarPane("sftp");
    } else {
      syncSidebarWithActiveTerminal();
    }
  };

  const activateTabById = (tabId: string) => {
    const tab = terminals.list.find((terminal) => terminal.id === tabId);
    if (tab) {
      activateTab(tab);
    }
  };

  const tabIndexFromPointer = (clientX: number, draggedTabId: string) => {
    if (!tabBarRef) return null;
    const tabs = Array.from(tabBarRef.querySelectorAll<HTMLElement>("[data-terminal-tab-id]"));
    if (tabs.length === 0) return null;

    let insertIndex = 0;
    for (const tabElement of tabs) {
      if (tabElement.dataset.terminalTabId === draggedTabId) continue;
      const rect = tabElement.getBoundingClientRect();
      if (clientX < rect.left + rect.width / 2) {
        return insertIndex;
      }
      insertIndex++;
    }

    return insertIndex;
  };

  const finishTabDrag = (event: PointerEvent, tabElement: HTMLDivElement) => {
    if (!dragState || dragState.pointerId !== event.pointerId) return null;

    const wasDragging = dragState.dragging;
    const tabId = dragState.tabId;
    try {
      tabElement.releasePointerCapture(event.pointerId);
    } catch {
    }
    dragState = null;
    setDraggedTabId(null);

    if (wasDragging) {
      event.preventDefault();
      event.stopPropagation();
      suppressNextTabClick = true;
      requestAnimationFrame(() => {
        suppressNextTabClick = false;
      });
    }

    return wasDragging ? tabId : null;
  };

  const openRenameDialog = (tabId: string, label: string) => {
    setTabContextMenu(null);
    setRenameTabId(tabId);
    setRenameLabel(label);
    setShowRenameDialog(true);
  };

  const duplicateTab = (tabId: string) => {
    const tab = terminals.list.find((terminal) => terminal.id === tabId);
    setTabContextMenu(null);
    if (!tab) return;

    addTerminal(
      tab.label,
      tab.type,
      tab.sessionId,
      tab.profile,
      tab.keyPassphrases?.map((input) => ({ ...input })),
    );
  };

  const closeTabFromContextMenu = (tabId: string) => {
    setTabContextMenu(null);
    closeTab(tabId);
  };

  const closeRenameDialog = () => {
    setShowRenameDialog(false);
    setRenameTabId(null);
    setRenameLabel("");
  };

  const doRename = () => {
    const id = renameTabId();
    const name = renameLabel().trim();
    if (id && name) {
      renameTerminal(id, name);
    }
    closeRenameDialog();
  };

  const openTerminalProfile = (profile: TerminalProfile) => {
    setProfileMenu(null);
    addTerminal(undefined, "local", undefined, profile);
  };

  const openAdminProfile = async (shell: AdminShell) => {
    setProfileMenu(null);
    setAdminLaunchError("");
    try {
      const terminal = await openAdminTerminal(shell);
      if (terminal) addAdminTab(terminal);
    } catch (error) {
      setAdminLaunchError(String(error));
    }
  };

  createEffect(() => {
    if (showRenameDialog() && renameInputRef) {
      renameInputRef.focus();
      renameInputRef.select();
    }
  });

  const resetProfileForm = () => {
    setEditingProfileId(null);
    setProfileName("");
    setProfileProgram("");
    setProfileArgs("");
    setProfileWorkingDirectory("");
    setProfileError("");
  };

  const closeProfileDialog = () => {
    setShowProfileDialog(false);
    resetProfileForm();
  };

  const editProfile = (profile?: TerminalProfile) => {
    if (!profile) {
      resetProfileForm();
      return;
    }
    setEditingProfileId(profile.id);
    setProfileName(profile.name);
    setProfileProgram(profile.program);
    setProfileArgs(profile.args);
    setProfileWorkingDirectory(profile.defaultWorkingDirectory);
    setProfileError("");
  };

  const openProfileDialog = () => {
    setProfileMenu(null);
    resetProfileForm();
    setShowProfileDialog(true);
  };

  const browseProgram = async () => {
    setProfileError("");
    try {
      const selected = await open({
        multiple: false,
        directory: false,
        title: "Select Terminal Program",
        defaultPath: profileProgram() || undefined,
      });
      if (typeof selected === "string" && selected.length > 0) {
        setProfileProgram(selected);
      }
    } catch (e: any) {
      setProfileError(e.toString());
    }
  };

  const saveProfile = () => {
    const name = profileName().trim();
    const program = profileProgram().trim();
    if (!name || !program) {
      setProfileError("Name and Program are required.");
      return;
    }

    const id = editingProfileId();
    if (id) {
      if (!updateTerminalProfile(id, {
        name,
        program,
        args: profileArgs(),
        defaultWorkingDirectory: profileWorkingDirectory(),
      })) {
        setProfileError("The default profile cannot be edited. Set another profile as default first.");
        return;
      }
    } else {
      addTerminalProfile({
        name,
        program,
        args: profileArgs(),
        defaultWorkingDirectory: profileWorkingDirectory(),
      });
    }
    closeProfileDialog();
  };

  const removeProfile = () => {
    const id = editingProfileId();
    if (!id) return;
    if (!deleteTerminalProfile(id)) {
      setProfileError("The default profile cannot be deleted.");
      return;
    }
    resetProfileForm();
  };

  const makeDefaultProfile = () => {
    const id = editingProfileId();
    if (!id) return;
    if (!setDefaultTerminalProfile(id)) {
      setProfileError("Could not set the default profile.");
      return;
    }
    editProfile(getDefaultTerminalProfile());
  };

  return (
    <div style={{
      display: "flex",
      "flex-direction": "column",
      height: "100%",
      width: "100%",
      background: "var(--terminal-background)",
    }}>
      {/* Tab bar */}
      <div ref={tabBarRef} style={{
        display: "flex",
        "align-items": "stretch",
        background: "var(--app-border-muted)",
        "min-height": "32px",
        overflow: "auto hidden",
      }}>
        <For each={terminals.list}>
          {(tab) => (
            <div
              data-terminal-tab-id={tab.id}
              onPointerDown={(e) => {
                if (e.button !== 0) return;
                if ((e.target as HTMLElement).closest("[data-tab-close]")) return;
                dragState = { tabId: tab.id, pointerId: e.pointerId, startX: e.clientX, dragging: false };
                e.currentTarget.setPointerCapture(e.pointerId);
              }}
              onPointerMove={(e) => {
                if (!dragState || dragState.pointerId !== e.pointerId) return;
                if (!dragState.dragging && Math.abs(e.clientX - dragState.startX) < 4) return;

                dragState.dragging = true;
                setDraggedTabId(dragState.tabId);
                suppressNextTabClick = true;

                const fromIndex = terminals.list.findIndex((terminal) => terminal.id === dragState!.tabId);
                const toIndex = tabIndexFromPointer(e.clientX, dragState.tabId);
                if (fromIndex !== -1 && toIndex !== null && fromIndex !== toIndex) {
                  reorderTerminals(fromIndex, toIndex);
                }
              }}
              onPointerUp={(e) => {
                const tabId = finishTabDrag(e, e.currentTarget);
                if (tabId) {
                  activateTabById(tabId);
                }
              }}
              onPointerCancel={(e) => {
                finishTabDrag(e, e.currentTarget);
              }}
              onMouseDown={(e) => {
                if (e.button !== 1) return;
                e.preventDefault();
                e.stopPropagation();
                closeTab(tab.id);
              }}
              onClick={(e) => {
                if (suppressNextTabClick) {
                  e.preventDefault();
                  e.stopPropagation();
                  suppressNextTabClick = false;
                  return;
                }
                activateTab(tab);
              }}
              onContextMenu={(e) => {
                e.preventDefault();
                e.stopPropagation();
                setTabContextMenu({ x: e.clientX, y: e.clientY, tabId: tab.id, label: tab.label });
              }}
              style={{
                display: "flex",
                "align-items": "center",
                gap: "6px",
                padding: "4px 12px",
                cursor: "grab",
                "font-size": "12px",
                "font-weight": tab.hasUnseenActivity ? "bold" : "normal",
                color: tab.hasExited ? "var(--app-danger)" : terminals.activeId === tab.id ? "var(--app-text)" : "var(--app-muted)",
                background: draggedTabId() === tab.id
                  ? "var(--app-selection)"
                  : terminals.activeId === tab.id ? "var(--app-surface-hover)" : "var(--app-surface-alt)",
                "border-right": "1px solid var(--app-border-muted)",
                "border-top-left-radius": "5px",
                "border-top-right-radius": "5px",
                "user-select": "none",
                "white-space": "nowrap",
              }}
            >
              <span style={{ "user-select": "none" }}>{tab.label}{tab.type === "ssh" ? " \uD83D\uDD12" : ""}</span>
              <span
                data-tab-close
                onPointerDown={(e) => e.stopPropagation()}
                onClick={(e) => {
                  e.stopPropagation();
                  closeTab(tab.id);
                }}
                style={{
                  cursor: "pointer",
                  opacity: 0.6,
                  "font-size": "14px",
                  "line-height": "14px",
                  "user-select": "none",
                }}
                onMouseEnter={(e) => (e.currentTarget.style.opacity = "1")}
                onMouseLeave={(e) => (e.currentTarget.style.opacity = "0.6")}
              >
                ×
              </span>
            </div>
          )}
        </For>
        {/* "+" button */}
        <div
          onClick={() => openTerminalProfile(getDefaultTerminalProfile())}
          onContextMenu={(e) => {
            e.preventDefault();
            e.stopPropagation();
            setProfileMenu({ x: e.clientX, y: e.clientY });
          }}
          style={{
            display: "flex",
            "align-items": "center",
            padding: "4px 10px",
            cursor: "pointer",
            background: "var(--app-surface-alt)",
            color: "var(--app-muted)",
            "font-size": "16px",
            "user-select": "none",
          }}
          title="New terminal"
        >
          +
        </div>
      </div>

      {/* Terminal area */}
      <Show when={adminLaunchError()}>
        <div class="form-error" role="alert" style={{ display: "flex", "align-items": "center", "justify-content": "space-between", gap: "8px" }}>
          <span>Could not open administrator terminal. {adminLaunchError()}</span>
          <button class="form-icon-button" onClick={() => setAdminLaunchError("")} aria-label="Dismiss administrator terminal error">×</button>
        </div>
      </Show>
      <div style={{
        flex: 1,
        position: "relative",
        background: "var(--terminal-background)",
      }}>
        <For each={terminals.list}>
          {(tab) => (
            <div
              style={{
                position: "absolute",
                top: 0,
                left: 0,
                right: 0,
                bottom: 0,
                background: "var(--terminal-background)",
                // xterm pauses rendering while its screen is out of layout.
                display: terminals.activeId === tab.id ? "block" : "none",
              }}
            >
              <Suspense>
                <TerminalTab id={tab.id} label={tab.label} type={tab.type} sessionId={tab.sessionId} profile={tab.profile} keyPassphrases={tab.keyPassphrases} active={terminals.activeId === tab.id} />
              </Suspense>
            </div>
          )}
        </For>
        {terminals.list.length === 0 && (
          <div style={{
            display: "flex",
            "align-items": "center",
            "justify-content": "center",
            height: "100%",
            color: "var(--app-empty)",
            "font-size": "14px",
          }}>
            Click + to open a new terminal
          </div>
        )}
      </div>
      <Show when={profileMenu() !== null}>
        <div
          class="new-terminal-menu"
          style={{
            position: "fixed",
            left: `${profileMenu()!.x}px`,
            top: `${profileMenu()!.y}px`,
            background: "var(--app-surface-alt)",
            border: "1px solid var(--app-border)",
            "border-radius": "4px",
            padding: "4px 0",
            "z-index": "5200",
            "min-width": "180px",
            "box-shadow": "0 4px 12px rgba(0,0,0,0.3)",
          }}
        >
          <For each={detectedPlatform === "windows" ? [...getTerminalProfiles(), ...windowsAdminProfiles] : getTerminalProfiles()}>
            {(profile) => (
              <div
                onClick={() => "shell" in profile ? openAdminProfile(profile.shell) : openTerminalProfile(profile)}
                style={menuItemStyle}
                onMouseEnter={(e) => (e.currentTarget.style.background = "var(--app-surface-hover)")}
                onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
              >
                {profile.name}
              </div>
            )}
          </For>
          <div style={{ height: "1px", background: "var(--app-border)", margin: "4px 0" }} />
          <div
            onClick={openProfileDialog}
            style={menuItemStyle}
            onMouseEnter={(e) => (e.currentTarget.style.background = "var(--app-surface-hover)")}
            onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
          >
            Edit...
          </div>
        </div>
      </Show>
      <Show when={showProfileDialog()}>
        <div
          style={{
            position: "fixed",
            inset: 0,
            background: "var(--app-overlay)",
            "z-index": "5300",
            display: "flex",
            "align-items": "center",
            "justify-content": "center",
            padding: "16px",
          }}
          onMouseDown={(e) => {
            if (e.target === e.currentTarget) closeProfileDialog();
          }}
          onContextMenu={(e) => {
            e.preventDefault();
            e.stopPropagation();
            closeProfileDialog();
          }}
        >
          <div class="form-panel" style={{ width: "min(860px, calc(100vw - 32px))", padding: "20px" }} onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); }}>
            <div class="form-panel__header">
              <h2 class="form-panel__title">Edit terminal profiles</h2>
              <button class="form-icon-button" title="Close" onClick={closeProfileDialog}>x</button>
            </div>
            <div style={{ display: "grid", gap: "24px", "grid-template-columns": "minmax(240px, 300px) minmax(0, 1fr)", "align-items": "start" }}>
              <div style={{ border: "1px solid var(--app-border)", "border-radius": "10px", overflow: "hidden", background: "var(--app-surface)" }}>
                <div style={{ padding: "10px 12px", "font-size": "11px", "letter-spacing": "0.08em", "text-transform": "uppercase", color: "var(--app-subtle)", background: "var(--app-surface-alt)", "border-bottom": "1px solid var(--app-border)" }}>
                  Profiles
                </div>
                <div style={{ display: "flex", "flex-direction": "column", "max-height": "420px", overflow: "auto" }}>
                  <For each={getTerminalProfiles()}>
                    {(profile) => {
                      const isEditing = editingProfileId() === profile.id;
                      const isDefault = isDefaultTerminalProfile(profile.id);
                      return (
                        <button
                          onClick={() => editProfile(profile)}
                          style={{
                            ...profileListItemStyle,
                            background: isEditing ? "var(--app-surface-hover)" : "transparent",
                            cursor: "pointer",
                          }}
                        >
                          <span>{profile.name}</span>
                          <span style={{ color: isDefault ? "var(--app-text)" : "var(--app-subtle)", "font-size": "11px" }}>
                            {isDefault ? "Default" : ""}
                          </span>
                        </button>
                      );
                    }}
                  </For>
                </div>
              </div>
              <div style={{ border: "1px solid var(--app-border)", "border-radius": "10px", background: "var(--app-surface)", padding: "18px" }}>
                <div style={{ display: "flex", "justify-content": "space-between", "align-items": "flex-start", gap: "16px", margin: "0 0 24px 0" }}>
                  <div>
                    <div style={{ "font-size": "16px", "font-weight": "600", color: "var(--app-text)" }}>
                      {editingProfileId() ? "Edit terminal profile" : "Add terminal profile"}
                    </div>
                    <div style={{ "font-size": "12px", color: "var(--app-subtle)", margin: "4px 0 0 0" }}>
                        "The selected program will run when opening a new terminal tab."
                    </div>
                  </div>
                  <button class="form-button form-button--secondary" onClick={() => resetProfileForm()}>Add New</button>
                </div>
                <div style={{ display: "flex", "flex-direction": "column", gap: "18px" }}>
                  <label class="form-field" style={profileFieldStyle}>
                    <span class="form-label" style={profileLabelStyle}>Name</span>
                    <input
                      class="form-input"
                      value={profileName()}
                      onInput={(e) => setProfileName(e.currentTarget.value)}
                      disabled={!!editingProfileId() && isDefaultTerminalProfile(editingProfileId()!)}
                    />
                  </label>
                  <label class="form-field" style={profileFieldStyle}>
                    <span class="form-label" style={profileLabelStyle}>Program</span>
                    <div class="form-row" style={{ "align-items": "center", gap: "10px" }}>
                      <input
                        class="form-input"
                        value={profileProgram()}
                        onInput={(e) => setProfileProgram(e.currentTarget.value)}
                        disabled={!!editingProfileId() && isDefaultTerminalProfile(editingProfileId()!)}
                      />
                      <button
                        class="form-button form-button--secondary"
                        onClick={browseProgram}
                        disabled={!!editingProfileId() && isDefaultTerminalProfile(editingProfileId()!)}
                      >
                        Browse
                      </button>
                    </div>
                  </label>
                  <label class="form-field" style={profileFieldStyle}>
                    <span class="form-label" style={profileLabelStyle}>Command-Line Arguments</span>
                    <input
                      class="form-input"
                      value={profileArgs()}
                      onInput={(e) => setProfileArgs(e.currentTarget.value)}
                      disabled={!!editingProfileId() && isDefaultTerminalProfile(editingProfileId()!)}
                    />
                  </label>
                  <label class="form-field" style={profileFieldStyle}>
                    <span class="form-label" style={profileLabelStyle}>Default Working Directory</span>
                    <input
                      class="form-input"
                      value={profileWorkingDirectory()}
                      onInput={(e) => setProfileWorkingDirectory(e.currentTarget.value)}
                      placeholder="Leave blank to use the system default"
                      disabled={!!editingProfileId() && isDefaultTerminalProfile(editingProfileId()!)}
                    />
                  </label>
                  <Show when={profileError()}>
                    <div class="form-error">{profileError()}</div>
                  </Show>
                </div>
                <div style={{ display: "flex", "justify-content": "space-between", gap: "12px", margin: "24px 0 0 0", padding: "16px 0 0 0", "border-top": "1px solid var(--app-border-muted)" }}>
                  <div style={{ display: "flex", "align-items": "center" }}>
                    <Show when={editingProfileId() && !isDefaultTerminalProfile(editingProfileId()!)}>
                      <button class="form-button form-button--danger" onClick={removeProfile}>Delete Profile</button>
                    </Show>
                  </div>
                  <div style={{ display: "flex", gap: "8px" }}>
                    <Show when={editingProfileId() && !isDefaultTerminalProfile(editingProfileId()!)}>
                      <button class="form-button form-button--secondary" onClick={makeDefaultProfile}>Set Default</button>
                    </Show>
                    <button class="form-button form-button--secondary" onClick={closeProfileDialog}>Close</button>
                    <Show when={!editingProfileId() || !isDefaultTerminalProfile(editingProfileId()!)}>
                      <button class="form-button form-button--primary" onClick={saveProfile}>
                        {editingProfileId() ? "Save Changes" : "Save"}
                      </button>
                    </Show>
                  </div>
                </div>
              </div>
            </div>
          </div>
        </div>
      </Show>
      <Show when={tabContextMenu() !== null}>
        <div
          class="tab-context-menu"
          style={{
            position: "fixed",
            left: `${tabContextMenu()!.x}px`,
            top: `${tabContextMenu()!.y}px`,
            background: "var(--app-surface-alt)",
            border: "1px solid var(--app-border)",
            "border-radius": "4px",
            padding: "4px 0",
            "z-index": "5200",
            "min-width": "140px",
            "box-shadow": "0 4px 12px rgba(0,0,0,0.3)",
          }}
        >
          <div
            onClick={() => {
              const ctx = tabContextMenu();
              if (ctx) openRenameDialog(ctx.tabId, ctx.label);
            }}
            style={menuItemStyle}
            onMouseEnter={(e) => (e.currentTarget.style.background = "var(--app-surface-hover)")}
            onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
          >
            Rename
          </div>
          <div
            onClick={() => {
              const ctx = tabContextMenu();
              if (ctx) duplicateTab(ctx.tabId);
            }}
            style={menuItemStyle}
            onMouseEnter={(e) => (e.currentTarget.style.background = "var(--app-surface-hover)")}
            onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
          >
            Duplicate
          </div>
          <div
            onClick={() => {
              const ctx = tabContextMenu();
              if (ctx) closeTabFromContextMenu(ctx.tabId);
            }}
            style={menuItemStyle}
            onMouseEnter={(e) => (e.currentTarget.style.background = "var(--app-danger-bg)")}
            onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
          >
            Close
          </div>
        </div>
      </Show>
      <Show when={showRenameDialog()}>
        <div
          style={{
            position: "fixed",
            inset: "0",
            background: "var(--app-overlay)",
            display: "flex",
            "align-items": "center",
            "justify-content": "center",
            "z-index": "5400",
          }}
          onMouseDown={() => closeRenameDialog()}
          onContextMenu={(e) => {
            e.preventDefault();
            e.stopPropagation();
            closeRenameDialog();
          }}
        >
          <div
            class="form-panel"
            style={{ width: "min(360px, calc(100vw - 32px))", padding: "16px", "box-shadow": "0 14px 48px rgba(0, 0, 0, 0.42)" }}
            onMouseDown={(event) => event.stopPropagation()}
            onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); }}
          >
            <div class="form-panel__header">
              <div>
                <div class="form-panel__eyebrow">TAB NAME</div>
              </div>
              <button class="form-icon-button" onClick={closeRenameDialog} aria-label="Close rename dialog">x</button>
            </div>
            <input
              ref={renameInputRef}
              class="form-input"
              value={renameLabel()}
              onInput={(event) => setRenameLabel(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  doRename();
                } else if (event.key === "Escape") {
                  closeRenameDialog();
                }
              }}
              style={{ "margin-bottom": "12px" }}
            />
            <div style={{ display: "flex", "justify-content": "flex-end", gap: "8px" }}>
              <button class="form-button form-button--secondary" onClick={closeRenameDialog}>
                Cancel
              </button>
              <button
                class="form-button form-button--primary"
                disabled={!renameLabel().trim()}
                onClick={doRename}
              >
                Rename
              </button>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}

const menuItemStyle: Record<string, string> = {
  padding: "6px 16px",
  cursor: "pointer",
  "font-size": "12px",
  color: "var(--app-text)",
  "white-space": "nowrap",
};

const profileFieldStyle: Record<string, string> = {
  margin: "0",
};

const profileListItemStyle: Record<string, string> = {
  display: "flex",
  "align-items": "center",
  "justify-content": "space-between",
  gap: "12px",
  width: "100%",
  padding: "10px 12px",
  border: "none",
  "border-bottom": "1px solid var(--app-border-muted)",
  color: "var(--app-text)",
  "font-size": "12px",
  "text-align": "left",
};

const profileLabelStyle: Record<string, string> = {
  "margin-bottom": "10px",
};
