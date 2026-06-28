import { createSignal, For, onMount, onCleanup, Show } from "solid-js";
import { open } from "@tauri-apps/plugin-dialog";
import { COLOR_SCHEMES, type ColorSchemeId } from "../colorSchemes";
import { listMonospaceFonts } from "../ipc/commands";
import { settings, closeSettingsPanel, previewColorScheme, previewFontSettings, saveFontSettings, saveColorScheme, saveCodeEditorCommand, saveScrollback, setupMasterPassword, changeMasterPwd, lockMasterPwd, refreshMasterPasswordStatus } from "../stores/settings";
import { Field, Divider } from "./settingsPanelParts";
import { setSessions } from "../stores/sessions";

const SORTED_COLOR_SCHEMES = [...COLOR_SCHEMES].sort((a, b) => a.label.localeCompare(b.label));

function getPrimaryFontFamily(fontFamily: string) {
  const match = fontFamily.match(/['"]([^'"]+)['"]/);
  if (match) return match[1];
  return fontFamily.split(",")[0]?.trim() || "monospace";
}

function toFontFamilyCss(fontFamily: string) {
  return `"${fontFamily.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}", monospace`;
}

export default function SettingsPanel() {
  const initialFontFamily = settings.fontFamily;
  const initialFontSize = settings.fontSize;
  const initialColorSchemeId = settings.colorSchemeId;
  const [fontFamily, setFontFamily] = createSignal(getPrimaryFontFamily(settings.fontFamily));
  const [fontSize, setFontSize] = createSignal(settings.fontSize);
  const [colorSchemeId, setColorSchemeId] = createSignal<ColorSchemeId>(settings.colorSchemeId);
  const [codeEditorCommand, setCodeEditorCommand] = createSignal(settings.codeEditorCommand);
  const [scrollback, setScrollback] = createSignal(settings.scrollback);
  const [fontOptions, setFontOptions] = createSignal<string[]>([getPrimaryFontFamily(settings.fontFamily)]);
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal("");
  const [didSave, setDidSave] = createSignal(false);

  const [pwdNew, setPwdNew] = createSignal("");
  const [pwdConfirm, setPwdConfirm] = createSignal("");
  const [pwdOld, setPwdOld] = createSignal("");
  const [pwdError, setPwdError] = createSignal("");
  const [pwdSuccess, setPwdSuccess] = createSignal("");
  const [pwdLoading, setPwdLoading] = createSignal(false);
  const [showChangePwd, setShowChangePwd] = createSignal(false);

  onMount(() => {
    const handler = (e: KeyboardEvent) => { if (e.key === "Escape") handleClose(); };
    document.addEventListener("keydown", handler);
    onCleanup(() => document.removeEventListener("keydown", handler));
    void refreshMasterPasswordStatus();
    void loadFontOptions();
  });

  onCleanup(() => {
    if (!didSave()) {
      if (settings.colorSchemeId !== initialColorSchemeId) {
        previewColorScheme(initialColorSchemeId);
      }
      if (settings.fontFamily !== initialFontFamily || settings.fontSize !== initialFontSize) {
        previewFontSettings(initialFontFamily, initialFontSize);
      }
    }
  });

  const loadFontOptions = async () => {
    try {
      const installedFonts = await listMonospaceFonts();
      const currentFont = getPrimaryFontFamily(settings.fontFamily);
      const mergedFonts = Array.from(new Set([currentFont, ...installedFonts]));
      setFontOptions(mergedFonts);
    } catch (e) {
      console.error("Failed to load monospace fonts:", e);
    }
  };

  const handleClose = () => {
    if (!didSave()) {
      if (settings.colorSchemeId !== initialColorSchemeId) {
        previewColorScheme(initialColorSchemeId);
      }
      if (settings.fontFamily !== initialFontFamily || settings.fontSize !== initialFontSize) {
        previewFontSettings(initialFontFamily, initialFontSize);
      }
    }
    closeSettingsPanel();
  };

  const handleSaveAll = async () => {
    setSaving(true);
    setError("");
    try {
      await saveFontSettings(toFontFamilyCss(fontFamily()), fontSize());
      await saveColorScheme(colorSchemeId());
      await saveCodeEditorCommand(codeEditorCommand().trim());
      await saveScrollback(scrollback());
      setDidSave(true);
      closeSettingsPanel();
    } catch (e: any) {
      setError(e.toString());
    } finally {
      setSaving(false);
    }
  };

  const handleBrowseCodeEditor = async () => {
    setError("");
    try {
      const currentValue = codeEditorCommand().trim();
      const defaultPath = /[\\/]/.test(currentValue) || /^[a-zA-Z]:/.test(currentValue) ? currentValue : undefined;
      const selected = await open({
        multiple: false,
        directory: false,
        title: "Select Default Code Editor",
        defaultPath,
      });
      if (typeof selected === "string" && selected.length > 0) {
        setCodeEditorCommand(selected);
      }
    } catch (e: any) {
      setError(e.toString());
    }
  };

  const handleSetMasterPassword = async () => {
    setPwdError("");
    setPwdSuccess("");
    if (pwdNew().length < 4) {
      setPwdError("Master password must be at least 4 characters");
      return;
    }
    if (pwdNew() !== pwdConfirm()) {
      setPwdError("Passwords do not match");
      return;
    }
    setPwdLoading(true);
    try {
      await setupMasterPassword(pwdNew());
      setPwdSuccess("Master password set successfully");
      setPwdNew("");
      setPwdConfirm("");
    } catch (e: any) {
      setPwdError(e.toString());
    } finally {
      setPwdLoading(false);
    }
  };

  const handleChangeMasterPassword = async () => {
    setPwdError("");
    setPwdSuccess("");
    if (pwdNew().length < 4) {
      setPwdError("New password must be at least 4 characters");
      return;
    }
    if (pwdNew() !== pwdConfirm()) {
      setPwdError("Passwords do not match");
      return;
    }
    setPwdLoading(true);
    try {
      await changeMasterPwd(pwdOld(), pwdNew());
      setPwdSuccess("Master password changed successfully");
      setPwdOld("");
      setPwdNew("");
      setPwdConfirm("");
      setShowChangePwd(false);
    } catch (e: any) {
      setPwdError(e.toString());
    } finally {
      setPwdLoading(false);
    }
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
        handleClose();
      }}
    >
      <div
        style={{
          padding: "16px",
          width: "min(calc(100vw - 32px), 520px)",
          "min-width": 0,
        }}
        class="form-panel form-panel--scrollable"
        onContextMenu={(e) => {
          e.preventDefault();
          e.stopPropagation();
        }}
      >
        <div class="form-panel__header">
          <h2 class="form-panel__title">Global Settings</h2>
          <button class="form-icon-button" onClick={handleClose} title="Close">x</button>
        </div>

        <form onSubmit={(e) => e.preventDefault()}>
          {!settings.hasMasterPassword ? (
            <>
              <p class="form-help" style={{ "margin-bottom": "12px" }}>
                A master password is required to securely store SSH session passwords.
              </p>
              <Field label="New Master Password">
                <input
                  class="form-input"
                  type="password"
                  value={pwdNew()}
                  onInput={(e) => setPwdNew(e.currentTarget.value)}
                  placeholder="Enter master password"
                />
              </Field>
              <Field label="Confirm Master Password">
                <input
                  class="form-input"
                  type="password"
                  value={pwdConfirm()}
                  onInput={(e) => setPwdConfirm(e.currentTarget.value)}
                  placeholder="Confirm master password"
                />
              </Field>
              {pwdError() && <div class="form-error" style={{ "margin-bottom": "8px" }}>{pwdError()}</div>}
              {pwdSuccess() && <div class="form-success" style={{ "margin-bottom": "8px" }}>{pwdSuccess()}</div>}
              <button
                type="button"
                onClick={handleSetMasterPassword}
                disabled={pwdLoading()}
                class="form-button form-button--primary"
              >
                {pwdLoading() ? "Setting..." : "Set Master Password"}
              </button>
            </>
          ) : (
            <div style={{ display: "flex", "flex-direction": "column", gap: "12px" }}>
              <div style={{ display: "flex", "align-items": "center", gap: "8px", "flex-wrap": "wrap" }}>
                <span style={{
                  display: "inline-block",
                  width: "8px",
                  height: "8px",
                  "border-radius": "50%",
                  background: settings.unlocked ? "var(--app-success)" : "var(--app-danger)",
                }} />
                <span style={{ color: "var(--form-text)", "font-size": "12px" }}>
                  Master password: {settings.unlocked ? "Unlocked" : "Locked"}
                </span>
                {settings.unlocked ? (
                  <button
                    type="button"
                    onClick={async () => { await lockMasterPwd(); }}
                    class="form-button form-button--secondary"
                    style={{ padding: "7px 12px", "font-size": "11px", "min-height": "unset" }}
                  >
                    Lock
                  </button>
                ) : (
                  <button
                    type="button"
                    onClick={() => {
                      setSessions("masterPasswordPrompt", true);
                      setSessions("masterPasswordMode", "unlock");
                    }}
                    class="form-button form-button--secondary"
                    style={{ padding: "7px 12px", "font-size": "11px", "min-height": "unset" }}
                  >
                    Unlock
                  </button>
                )}
                <button
                  type="button"
                  onClick={() => { setShowChangePwd(v => !v); setPwdError(""); setPwdSuccess(""); }}
                  class="form-button form-button--secondary"
                  style={{ padding: "7px 12px", "font-size": "11px", "min-height": "unset" }}
                >
                  Change...
                </button>
              </div>

              <Show when={showChangePwd()}>
                <div>
                  <Field label="Current Master Password">
                    <input
                      class="form-input"
                      type="password"
                      value={pwdOld()}
                      onInput={(e) => setPwdOld(e.currentTarget.value)}
                      placeholder="Current password"
                    />
                  </Field>
                  <Field label="New Master Password">
                    <input
                      class="form-input"
                      type="password"
                      value={pwdNew()}
                      onInput={(e) => setPwdNew(e.currentTarget.value)}
                      placeholder="New password"
                    />
                  </Field>
                  <Field label="Confirm New Master Password">
                    <input
                      class="form-input"
                      type="password"
                      value={pwdConfirm()}
                      onInput={(e) => setPwdConfirm(e.currentTarget.value)}
                      placeholder="Confirm new password"
                    />
                  </Field>
                  {pwdError() && <div class="form-error" style={{ "margin-bottom": "8px" }}>{pwdError()}</div>}
                  {pwdSuccess() && <div class="form-success" style={{ "margin-bottom": "8px" }}>{pwdSuccess()}</div>}
                  <div style={{ display: "flex", gap: "8px" }}>
                    <button
                      type="button"
                      onClick={() => { setShowChangePwd(false); setPwdError(""); setPwdSuccess(""); }}
                      class="form-button form-button--secondary"
                    >
                      Cancel
                    </button>
                    <button
                      type="button"
                      onClick={handleChangeMasterPassword}
                      disabled={pwdLoading()}
                      class="form-button form-button--primary"
                    >
                      {pwdLoading() ? "Changing..." : "Change Password"}
                    </button>
                  </div>
                </div>
              </Show>
            </div>
          )}
        <Divider />

        <div class="form-section">
          <Field label="Terminal font">
            <select
              class="form-select"
              value={fontFamily()}
              onChange={(e) => {
                const nextFontFamily = e.currentTarget.value;
                setFontFamily(nextFontFamily);
                previewFontSettings(toFontFamilyCss(nextFontFamily), fontSize());
              }}
            >
              <For each={fontOptions()}>
                {(opt) => (
                  <option value={opt}>
                    {opt}
                  </option>
                )}
              </For>
            </select>
          </Field>
          <Field label="Font Size">
            <div style={{ display: "flex", "align-items": "center", gap: "8px" }}>
              <input
                type="range"
                min={10}
                max={24}
                value={fontSize()}
                onInput={(e) => {
                  const nextFontSize = parseInt(e.currentTarget.value) || 14;
                  setFontSize(nextFontSize);
                  previewFontSettings(toFontFamilyCss(fontFamily()), nextFontSize);
                }}
                style={{ flex: 1, "accent-color": "var(--form-accent)" }}
              />
              <span style={{ color: "var(--form-text)", "font-size": "12px", "min-width": "24px" }}>
                {fontSize()}
              </span>
            </div>
          </Field>
          <Field label="Color Scheme">
            <select
              class="form-select"
              value={colorSchemeId()}
              onChange={(e) => {
                const nextScheme = e.currentTarget.value as ColorSchemeId;
                setColorSchemeId(nextScheme);
                previewColorScheme(nextScheme);
              }}
            >
              <For each={SORTED_COLOR_SCHEMES}>
                {(scheme) => (
                  <option value={scheme.id}>
                    {scheme.label}
                  </option>
                )}
              </For>
            </select>
          </Field>
          <Field label="Scrollback Lines">
            <input
              class="form-input"
              type="number"
              min={1000}
              max={4294967295}
              step={1000}
              value={scrollback()}
              onInput={(e) => setScrollback(parseInt(e.currentTarget.value) || 50000)}
            />
            <p class="form-help" style={{ "margin-top": "4px" }}>
              Takes effect for new terminal panes. Higher values use more memory.
            </p>
          </Field>
        </div>

        <Divider />

          <Field label="Default Code Editor">
            <div style={{ display: "flex", gap: "8px", "align-items": "stretch" }}>
              <input
                class="form-input"
                type="text"
                value={codeEditorCommand()}
                readOnly
                placeholder="No editor selected"
              />
              <button
                type="button"
                onClick={handleBrowseCodeEditor}
                class="form-button form-button--secondary"
                style={{ "flex-shrink": 0 }}
              >
                Browse
              </button>
              <button
                type="button"
                onClick={() => setCodeEditorCommand("")}
                disabled={!codeEditorCommand().trim()}
                class="form-button form-button--secondary"
                style={{ "flex-shrink": 0 }}
              >
                Clear
              </button>
            </div>
          </Field>
          <p class="form-help" style={{ "margin-bottom": "12px" }}>
            Used by the SFTP browser when you choose "Edit". Leave blank to use the OS default app.
          </p>

        {error() && <div class="form-error" style={{ "margin-bottom": "12px" }}>{error()}</div>}

        <div style={{ display: "flex", gap: "8px", "justify-content": "flex-end" }}>
          <button
            type="button"
            onClick={handleSaveAll}
            disabled={saving()}
            class="form-button form-button--primary"
          >
            {saving() ? "Saving..." : "Save"}
          </button>
        </div>
        </form>
      </div>
    </div>
  );
}
