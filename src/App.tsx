import { createEffect, lazy, onCleanup, onMount, Show, Suspense } from "solid-js";
import { sessions, portForwards } from "./stores/sessions";
import { settings } from "./stores/settings";
import { dismissEditUploadError, ensureSftpEventListeners, fileBrowser } from "./stores/fileBrowser";
import TerminalPane from "./components/TerminalPane";
import Sidebar from "./components/Sidebar";
import { loadSettings } from "./stores/settings";
import { applyColorScheme } from "./colorSchemes";
import "./App.css";

const SessionEditPanel = lazy(() => import("./components/SessionEditPanel"));
const SettingsPanel = lazy(() => import("./components/SettingsPanel"));
const MasterPasswordPrompt = lazy(() => import("./components/MasterPasswordPrompt"));
const PrivateKeyPassphrasePrompt = lazy(() => import("./components/PrivateKeyPassphrasePrompt"));
const PortForwardForm = lazy(() => import("./components/PortForwardForm"));

function isEditableTarget(target: EventTarget | null) {
  if (!(target instanceof HTMLElement)) return false;
  const tagName = target.tagName.toLowerCase();
  return target.isContentEditable || tagName === "input" || tagName === "textarea" || tagName === "select";
}

function preventWebviewShortcutDefaults(event: KeyboardEvent) {
  if (!(event.ctrlKey || event.metaKey) || ["Control", "Meta", "Alt", "Shift"].includes(event.key)) return;
  const key = event.key.toLowerCase();
  if (isEditableTarget(event.target) && ["a", "c", "v", "x", "y", "z"].includes(key)) return;
  event.preventDefault();
}

function App() {
  createEffect(() => {
    applyColorScheme(settings.colorSchemeId);
  });

  onMount(() => {
    void Promise.all([ensureSftpEventListeners(), loadSettings()]);
    document.addEventListener("keydown", preventWebviewShortcutDefaults);
    onCleanup(() => document.removeEventListener("keydown", preventWebviewShortcutDefaults));
  });

  return (
    <>
      <div
        onContextMenu={(e) => e.preventDefault()}
        style={{
        display: "flex",
        width: "100vw",
        height: "100vh",
        overflow: "hidden",
        background: "var(--app-background)",
        color: "var(--app-text)",
      }}>
        <Sidebar />
        <div style={{ flex: 1, overflow: "hidden" }}>
          <TerminalPane />
        </div>
      </div>
      <Suspense>
        <Show when={sessions.showEditPanel}>
          <SessionEditPanel />
        </Show>
        <Show when={settings.showSettingsPanel}>
          <SettingsPanel />
        </Show>
        <Show when={sessions.masterPasswordPrompt}>
          <MasterPasswordPrompt />
        </Show>
        <Show when={sessions.keyPassphrasePrompt}>
          <PrivateKeyPassphrasePrompt />
        </Show>
        <Show when={portForwards.showPanel}>
          <PortForwardForm />
        </Show>
      </Suspense>
      <Show when={fileBrowser.editUploadError}>
        <div
          style={{
            position: "fixed",
            inset: "0",
            background: "var(--app-overlay)",
            display: "flex",
            "align-items": "center",
            "justify-content": "center",
            "z-index": 6000,
          }}
          onClick={() => dismissEditUploadError()}
          onContextMenu={(e) => {
            e.preventDefault();
            e.stopPropagation();
            dismissEditUploadError();
          }}
        >
        <div
          class="form-panel"
          style={{ width: "min(460px, calc(100vw - 32px))", padding: "16px", "box-shadow": "0 14px 48px rgba(0, 0, 0, 0.42)" }}
          onClick={(event) => event.stopPropagation()}
          onContextMenu={(e) => {
            e.preventDefault();
            e.stopPropagation();
          }}
        >
            <div class="form-panel__header">
              <div>
                <div class="form-panel__eyebrow">SFTP</div>
                <h2 class="form-panel__title">Upload failed</h2>
              </div>
              <button class="form-icon-button" onClick={() => dismissEditUploadError()} aria-label="Close upload error dialog">
                x
              </button>
            </div>
            <div style={{ color: "var(--form-text)", "font-size": "12px", "line-height": "1.5", "margin-bottom": "8px" }}>
              Could not re-upload <code style={{ "font-family": "var(--form-mono)" }}>{fileBrowser.editUploadError?.remotePath}</code>.
            </div>
            <div class="form-error" style={{ "margin-bottom": "16px" }}>{fileBrowser.editUploadError?.error}</div>
            <div style={{ display: "flex", "justify-content": "flex-end" }}>
              <button class="form-button form-button--primary" onClick={() => dismissEditUploadError()}>
                Close
              </button>
            </div>
          </div>
        </div>
      </Show>
    </>
  );
}

export default App;
