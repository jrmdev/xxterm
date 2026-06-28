import { createStore } from "solid-js/store";
import {
  listSessions,
  createSession as apiCreateSession,
  updateSession as apiUpdateSession,
  deleteSession as apiDeleteSession,
  getSession as apiGetSession,
  storeEncryptedPassword,
  deleteEncryptedPassword,
  importSshConfig as apiImportSshConfig,
  listSshConfigEntries as apiListSshConfigEntries,
  listGroups as apiListGroups,
  createGroup as apiCreateGroup,
  deleteGroup as apiDeleteGroup,
} from "../ipc/commands";
import { settings, refreshMasterPasswordStatus, unlockWithMasterPassword, setupMasterPassword } from "./settings";
import { addTerminal } from "./terminals";

export interface Group {
  id: number;
  name: string;
}

export interface Session {
  id: number;
  name: string;
  host: string;
  port: number;
  username: string;
  auth_method: string;
  agent_forwarding: boolean;
  private_key_path: string | null;
  jump_host_session_id: number | null;
  startup_command: string | null;
  group_id: number | null;
  group_name: string | null;
  created_at: string;
  updated_at: string;
}

export interface CreateSessionInput {
  name: string;
  host: string;
  port: number;
  username: string;
  auth_method: string;
  agent_forwarding: boolean;
  private_key_path: string | null;
  jump_host_session_id: number | null;
  startup_command: string | null;
  group_id: number | null;
}

export interface UpdateSessionInput {
  name?: string;
  host?: string;
  port?: number;
  username?: string;
  auth_method?: string;
  agent_forwarding?: boolean;
  private_key_path?: string | null;
  jump_host_session_id?: number | null;
  startup_command?: string | null;
  group_id?: number | null;
}

export interface SshConfigEntry {
  host: string;
  hostName: string | null;
  port: number | null;
  user: string | null;
  identityFile: string | null;
}

type PendingResolver = (value: boolean) => void;

export const [sessions, setSessions] = createStore<{
  list: Session[];
  groups: Group[];
  editing: Session | null;
  showEditPanel: boolean;
  masterPasswordPrompt: boolean;
  masterPasswordMode: "set" | "unlock" | null;
  pendingResolver: PendingResolver | null;
  pendingConnect: { name: string; sessionId: number } | null;
}>({
  list: [],
  groups: [],
  editing: null,
  showEditPanel: false,
  masterPasswordPrompt: false,
  masterPasswordMode: null,
  pendingResolver: null,
  pendingConnect: null,
});

export async function loadSessions() {
  try {
    const list = await listSessions();
    setSessions("list", list);
  } catch (e) {
    console.error("Failed to load sessions:", e);
  }
}

export async function loadGroups() {
  try {
    const groups = await apiListGroups();
    setSessions("groups", groups);
  } catch (e) {
    console.error("Failed to load groups:", e);
  }
}

export async function createGroup(name: string): Promise<Group> {
  const group = await apiCreateGroup(name);
  await loadGroups();
  return group;
}

export async function deleteGroup(id: number) {
  await apiDeleteGroup(id);
  await loadGroups();
  await loadSessions();
}

export function openCreatePanel() {
  setSessions("editing", null);
  setSessions("showEditPanel", true);
}

export function openEditPanel(session: Session) {
  setSessions("editing", session);
  setSessions("showEditPanel", true);
}

export function closeEditPanel() {
  setSessions("showEditPanel", false);
  setSessions("editing", null);
}

export function closeMasterPasswordPrompt() {
  const resolver = sessions.pendingResolver;
  setSessions("masterPasswordPrompt", false);
  setSessions("masterPasswordMode", null);
  setSessions("pendingResolver", null);
  setSessions("pendingConnect", null);
  if (resolver) resolver(false);
}

export async function handleMasterPasswordSubmit(masterPwd: string) {
  try {
    if (sessions.masterPasswordMode === "set") {
      await setupMasterPassword(masterPwd);
    } else {
      await unlockWithMasterPassword(masterPwd);
    }

    const resolver = sessions.pendingResolver;
    setSessions("masterPasswordPrompt", false);
    setSessions("masterPasswordMode", null);
    setSessions("pendingResolver", null);

    if (resolver) {
      resolver(true);
    } else {
      const pending = sessions.pendingConnect;
      setSessions("pendingConnect", null);
      if (pending) {
        addTerminal(pending.name, "ssh", pending.sessionId);
      }
    }
  } catch (e: any) {
    throw e;
  }
}

export async function ensureMasterPasswordReady(): Promise<boolean> {
  await refreshMasterPasswordStatus();

  if (!settings.hasMasterPassword) {
    return new Promise((resolve) => {
      setSessions("masterPasswordPrompt", true);
      setSessions("masterPasswordMode", "set");
      setSessions("pendingResolver", () => resolve as PendingResolver);
    });
  }

  if (!settings.unlocked) {
    return new Promise((resolve) => {
      setSessions("masterPasswordPrompt", true);
      setSessions("masterPasswordMode", "unlock");
      setSessions("pendingResolver", () => resolve as PendingResolver);
    });
  }

  return true;
}

export async function sessionNeedsPasswordUnlock(sessionId: number): Promise<boolean> {
  const seen = new Set<number>();
  let currentId: number | null = sessionId;

  while (currentId !== null) {
    if (seen.has(currentId)) {
      throw new Error("Jump host cycle detected in session configuration");
    }
    seen.add(currentId);

    const session: Session = sessions.list.find((item) => item.id === currentId) ?? await apiGetSession(currentId);
    if (session.auth_method === "password") {
      return true;
    }
    currentId = session.jump_host_session_id;
  }

  return false;
}

export async function connectWithMasterPassword(name: string, sessionId: number) {
  if (!await sessionNeedsPasswordUnlock(sessionId)) {
    addTerminal(name, "ssh", sessionId);
    return;
  }

  await refreshMasterPasswordStatus();

  if (!settings.hasMasterPassword) {
    setSessions("pendingConnect", { name, sessionId });
    setSessions("masterPasswordPrompt", true);
    setSessions("masterPasswordMode", "set");
    return;
  }

  if (!settings.unlocked) {
    setSessions("pendingConnect", { name, sessionId });
    setSessions("masterPasswordPrompt", true);
    setSessions("masterPasswordMode", "unlock");
    return;
  }

  addTerminal(name, "ssh", sessionId);
}

export async function saveSession(input: CreateSessionInput, password?: string) {
  const session = await apiCreateSession(input);

  if (password) {
    const ready = await ensureMasterPasswordReady();
    if (!ready) {
      await loadSessions();
      closeEditPanel();
      return session;
    }
    await storeEncryptedPassword(session.id, password);
  }

  await loadSessions();
  closeEditPanel();
  return session;
}

export async function editSession(id: number, input: UpdateSessionInput, password?: string) {
  const session = await apiUpdateSession(id, input);

  if (password !== undefined) {
    if (password) {
      const ready = await ensureMasterPasswordReady();
      if (!ready) {
        await loadSessions();
        closeEditPanel();
        return session;
      }
      await storeEncryptedPassword(session.id, password);
    } else {
      await deleteEncryptedPassword(session.id);
    }
  }

  await loadSessions();
  closeEditPanel();
  return session;
}

export async function removeSession(id: number) {
  await apiDeleteSession(id);
  await deleteEncryptedPassword(id);
  await loadSessions();
}

export async function importSshConfig() {
  const count = await apiImportSshConfig();
  await loadSessions();
  return count;
}

export async function loadSshConfigEntries(): Promise<SshConfigEntry[]> {
  return await apiListSshConfigEntries();
}

export * from "./portForwards";
