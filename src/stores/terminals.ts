import { createStore } from "solid-js/store";
import { getDefaultTerminalProfile, type TerminalProfile } from "./terminalProfiles";
import type { KeyPassphraseInput } from "../ipc/commands";

export interface Terminal {
  id: string;
  label: string;
  type: "local" | "ssh";
  sessionId?: number;
  profile?: TerminalProfile;
  keyPassphrases?: KeyPassphraseInput[];
  connectionState: "connecting" | "connected" | "disconnected";
  hasUnseenActivity: boolean;
  hasExited: boolean;
}

export const [terminals, setTerminals] = createStore<{ list: Terminal[]; activeId: string | null }>({
  list: [],
  activeId: null,
});

let nextId = 1;
const localProfileCounts = new Map<string, number>();

function nextProfileLabel(profile: TerminalProfile): string {
  const current = localProfileCounts.get(profile.name) ?? 0;
  const next = current + 1;
  localProfileCounts.set(profile.name, next);
  return `${profile.name} ${next}`;
}

export function addTerminal(label?: string, type?: "local" | "ssh", sessionId?: number, profile?: TerminalProfile, keyPassphrases?: KeyPassphraseInput[]): string {
  const id = `term-${nextId++}`;
  const terminalType = type || "local";
  const terminalProfile = terminalType === "local" ? profile ?? getDefaultTerminalProfile() : undefined;
  const term: Terminal = {
    id,
    label: label || (terminalProfile ? nextProfileLabel(terminalProfile) : `Terminal ${nextId - 1}`),
    type: terminalType,
    sessionId,
    profile: terminalProfile,
    keyPassphrases: terminalType === "ssh" ? keyPassphrases : undefined,
    connectionState: terminalType === "ssh" ? "connecting" : "connected",
    hasUnseenActivity: false,
    hasExited: false,
  };
  setTerminals("list", (prev) => [...prev, term]);
  setTerminals("activeId", id);
  return id;
}

export function removeTerminal(id: string) {
  const idx = terminals.list.findIndex((t) => t.id === id);
  if (idx === -1) return;
  setTerminals("list", (prev) => prev.filter((t) => t.id !== id));
  if (terminals.activeId === id) {
    const remaining = terminals.list.filter((t) => t.id !== id);
    setTerminals("activeId", remaining.length > 0 ? remaining[Math.min(idx, remaining.length - 1)].id : null);
  }
}

export function setActiveTerminal(id: string) {
  setTerminals("activeId", id);
}

export function setTerminalConnectionState(id: string, connectionState: Terminal["connectionState"]) {
  const idx = terminals.list.findIndex((terminal) => terminal.id === id);
  if (idx === -1) return;
  setTerminals("list", idx, "connectionState", connectionState);
}

export function setTerminalUnseenActivity(id: string, unseen: boolean) {
  const idx = terminals.list.findIndex((terminal) => terminal.id === id);
  if (idx === -1) return;
  setTerminals("list", idx, "hasUnseenActivity", unseen);
}

export function setTerminalExited(id: string, exited: boolean) {
  const idx = terminals.list.findIndex((terminal) => terminal.id === id);
  if (idx === -1) return;
  setTerminals("list", idx, "hasExited", exited);
}

export function getActiveTerminal() {
  return terminals.list.find((terminal) => terminal.id === terminals.activeId) ?? null;
}

export function renameTerminal(id: string, newLabel: string) {
  const idx = terminals.list.findIndex((terminal) => terminal.id === id);
  if (idx === -1) return;
  setTerminals("list", idx, "label", newLabel);
}

export function reorderTerminals(fromIndex: number, toIndex: number) {
  if (fromIndex === toIndex) return;
  const list = [...terminals.list];
  const [moved] = list.splice(fromIndex, 1);
  list.splice(toIndex, 0, moved);
  setTerminals("list", list);
}

export function getActiveSshSessionId() {
  const active = getActiveTerminal();
  return active?.type === "ssh" && active.connectionState !== "disconnected" ? active.sessionId ?? null : null;
}

export function getActiveSshTerminalId() {
  const active = getActiveTerminal();
  return active?.type === "ssh" && active.connectionState !== "disconnected" ? active.id : null;
}
