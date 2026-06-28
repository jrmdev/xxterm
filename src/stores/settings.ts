import { batch } from "solid-js";
import { createStore } from "solid-js/store";
import { getAppBootstrapState, getMasterPasswordStatus, setSetting, setMasterPassword, unlockMasterPassword, lockMasterPassword, changeMasterPassword } from "../ipc/commands";
import { DEFAULT_COLOR_SCHEME_ID, isColorSchemeId, type ColorSchemeId } from "../colorSchemes";

const DEFAULT_SCROLLBACK = 50000;

export const [settings, setSettings] = createStore<{
  showSettingsPanel: boolean;
  fontFamily: string;
  fontSize: number;
  colorSchemeId: ColorSchemeId;
  codeEditorCommand: string;
  scrollback: number;
  hasMasterPassword: boolean;
  unlocked: boolean;
}>({
  showSettingsPanel: false,
  fontFamily: "'Cascadia Code', 'Fira Code', 'JetBrains Mono', 'Consolas', monospace",
  fontSize: 14,
  colorSchemeId: DEFAULT_COLOR_SCHEME_ID,
  codeEditorCommand: "",
  scrollback: DEFAULT_SCROLLBACK,
  hasMasterPassword: false,
  unlocked: false,
});

export function openSettingsPanel() {
  setSettings("showSettingsPanel", true);
}

export function closeSettingsPanel() {
  setSettings("showSettingsPanel", false);
}

export function previewColorScheme(colorSchemeId: ColorSchemeId) {
  setSettings("colorSchemeId", colorSchemeId);
}

export function previewFontSettings(fontFamily: string, fontSize: number) {
  setSettings("fontFamily", fontFamily);
  setSettings("fontSize", fontSize);
}

export async function loadSettings() {
  try {
    const state = await getAppBootstrapState();
    batch(() => {
      if (state.fontFamily) setSettings("fontFamily", state.fontFamily);
      if (state.fontSize) setSettings("fontSize", parseInt(state.fontSize, 10) || 14);
      if (state.colorScheme === "default") {
        setSettings("colorSchemeId", DEFAULT_COLOR_SCHEME_ID);
      } else if (isColorSchemeId(state.colorScheme)) {
        setSettings("colorSchemeId", state.colorScheme);
      }
      if (state.codeEditorCommand !== null) setSettings("codeEditorCommand", state.codeEditorCommand);
      if (state.scrollback !== null) setSettings("scrollback", parseInt(state.scrollback, 10) || DEFAULT_SCROLLBACK);
      setSettings("hasMasterPassword", state.hasMasterPassword);
      setSettings("unlocked", state.unlocked);
    });
  } catch (e) {
    console.error("Failed to load settings:", e);
  }
}

export async function saveFontSettings(fontFamily: string, fontSize: number) {
  try {
    await setSetting("font_family", fontFamily);
    await setSetting("font_size", fontSize.toString());
    setSettings("fontFamily", fontFamily);
    setSettings("fontSize", fontSize);
  } catch (e) {
    console.error("Failed to save font settings:", e);
    throw e;
  }
}

export async function saveColorScheme(colorSchemeId: ColorSchemeId) {
  try {
    await setSetting("color_scheme", colorSchemeId);
    setSettings("colorSchemeId", colorSchemeId);
  } catch (e) {
    console.error("Failed to save color scheme:", e);
    throw e;
  }
}

export async function saveCodeEditorCommand(codeEditorCommand: string) {
  try {
    await setSetting("code_editor_command", codeEditorCommand);
    setSettings("codeEditorCommand", codeEditorCommand);
  } catch (e) {
    console.error("Failed to save code editor command:", e);
    throw e;
  }
}

export async function saveScrollback(scrollback: number) {
  try {
    await setSetting("scrollback", scrollback.toString());
    setSettings("scrollback", scrollback);
  } catch (e) {
    console.error("Failed to save scrollback:", e);
    throw e;
  }
}

export async function setupMasterPassword(password: string) {
  await setMasterPassword(password);
  setSettings("hasMasterPassword", true);
  setSettings("unlocked", true);
}

export async function unlockWithMasterPassword(password: string) {
  await unlockMasterPassword(password);
  setSettings("unlocked", true);
}

export async function lockMasterPwd() {
  await lockMasterPassword();
  setSettings("unlocked", false);
}

export async function changeMasterPwd(oldPassword: string, newPassword: string) {
  await changeMasterPassword(oldPassword, newPassword);
}

export async function refreshMasterPasswordStatus() {
  try {
    const status = await getMasterPasswordStatus();
    setSettings("hasMasterPassword", status.hasMasterPassword);
    setSettings("unlocked", status.unlocked);
  } catch (e) {
    console.error("Failed to refresh master password status:", e);
  }
}
