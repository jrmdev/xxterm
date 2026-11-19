import { createStore } from "solid-js/store";
import {
  listPortForwards as apiListPortForwards,
  createPortForward as apiCreatePortForward,
  updatePortForward as apiUpdatePortForward,
  deletePortForward as apiDeletePortForward,
  startPortForwards as apiStartPortForwards,
  stopPortForwards as apiStopPortForwards,
  autoStartPortForwards as apiAutoStartPortForwards,
  autoStopPortForwards as apiAutoStopPortForwards,
  startSinglePortForward as apiStartSinglePortForward,
  stopSinglePortForward as apiStopSinglePortForward,
  getPortForwardStatus as apiGetPortForwardStatus,
  type KeyPassphraseInput,
} from "../ipc/commands";
import { ensureMasterPasswordReady, prepareKeyPassphrases, sessionNeedsPasswordUnlock } from "./sessions";
import type { Session } from "./sessions";

export interface PortForward {
  id: number;
  session_id: number;
  forward_type: string;
  local_port: number;
  local_host: string;
  remote_host: string | null;
  remote_port: number | null;
  always_start: boolean;
}

export interface CreatePortForwardInput {
  session_id: number;
  forward_type: string;
  local_port: number;
  local_host: string;
  remote_host: string | null;
  remote_port: number | null;
  always_start: boolean;
}

export interface UpdatePortForwardInput {
  forward_type?: string;
  local_port?: number;
  local_host?: string;
  remote_host?: string | null;
  remote_port?: number | null;
  always_start?: boolean;
}

export interface ForwardStatus {
  id: number;
  forward_type: string;
  local_port: number;
  local_host: string;
  remote_host: string | null;
  remote_port: number | null;
  status: string;
  error: string | null;
}

export interface BoundPortForward extends PortForward {
  sessionName: string;
  status: string;
  error: string | null;
}

export const [portForwards, setPortForwards] = createStore<{
  sessionId: number | null;
  list: PortForward[];
  statuses: ForwardStatus[];
  showPanel: boolean;
  sidebarList: BoundPortForward[];
  sidebarLoading: boolean;
}>({
  sessionId: null,
  list: [],
  statuses: [],
  showPanel: false,
  sidebarList: [],
  sidebarLoading: false,
});

export function openPortForwardPanel(sessionId: number) {
  setPortForwards("sessionId", sessionId);
  setPortForwards("showPanel", true);
  loadPortForwards(sessionId);
}

export function closePortForwardPanel() {
  setPortForwards("showPanel", false);
  setPortForwards("sessionId", null);
  setPortForwards("list", []);
  setPortForwards("statuses", []);
}

export async function loadPortForwards(sessionId: number) {
  try {
    const [list, statuses] = await Promise.all([
      apiListPortForwards(sessionId),
      apiGetPortForwardStatus(sessionId),
    ]);
    setPortForwards("list", list);
    setPortForwards("statuses", statuses);
  } catch (e) {
    console.error("Failed to load port forwards:", e);
  }
}

export async function createPortForward(input: CreatePortForwardInput) {
  const result = await apiCreatePortForward(input);
  if (portForwards.sessionId === input.session_id) {
    await loadPortForwards(input.session_id);
  }
  return result;
}

export async function editPortForward(id: number, input: UpdatePortForwardInput) {
  const result = await apiUpdatePortForward(id, input);
  if (portForwards.sessionId) {
    await loadPortForwards(portForwards.sessionId);
  }
  return result;
}

export async function removePortForward(id: number) {
  await apiDeletePortForward(id);
  if (portForwards.sessionId) {
    await loadPortForwards(portForwards.sessionId);
  }
}

export async function startPortForwards(sessionId: number) {
  if (await sessionNeedsPasswordUnlock(sessionId)) {
    const ready = await ensureMasterPasswordReady();
    if (!ready) return;
  }
  const keyPassphrases = await prepareKeyPassphrases(sessionId);
  if (keyPassphrases === null) return;
  try {
    await apiStartPortForwards(sessionId, keyPassphrases);
    if (portForwards.sessionId === sessionId) {
      await loadPortForwards(sessionId);
    }
  } catch (e) {
    console.error("Failed to start port forwards:", e);
    throw e;
  }
}

export async function stopPortForwards(sessionId: number) {
  try {
    await apiStopPortForwards(sessionId);
    if (portForwards.sessionId === sessionId) {
      await loadPortForwards(sessionId);
    }
  } catch (e) {
    console.error("Failed to stop port forwards:", e);
  }
}

export async function autoStartPortForwards(sessionId: number, terminalId: string, preparedKeyPassphrases?: KeyPassphraseInput[]) {
  if (await sessionNeedsPasswordUnlock(sessionId)) {
    const ready = await ensureMasterPasswordReady();
    if (!ready) return;
  }
  const keyPassphrases = preparedKeyPassphrases ?? await prepareKeyPassphrases(sessionId);
  if (keyPassphrases === null) return;
  try {
    await apiAutoStartPortForwards(sessionId, terminalId, keyPassphrases);
    if (portForwards.sessionId === sessionId) {
      await loadPortForwards(sessionId);
    }
  } catch (e) {
    console.error("Failed to auto-start port forwards:", e);
  }
}

export async function autoStopPortForwards(sessionId: number, terminalId: string) {
  try {
    await apiAutoStopPortForwards(sessionId, terminalId);
    if (portForwards.sessionId === sessionId) {
      await loadPortForwards(sessionId);
    }
  } catch (e) {
    console.error("Failed to auto-stop port forwards:", e);
  }
}

export async function refreshPortForwardStatus(sessionId: number) {
  try {
    const statuses = await apiGetPortForwardStatus(sessionId);
    if (portForwards.sessionId === sessionId) {
      setPortForwards("statuses", statuses);
    }
  } catch (e) {
    console.error("Failed to refresh port forward status:", e);
  }
}

export async function startSinglePortForward(sessionId: number, forwardId: number) {
  if (await sessionNeedsPasswordUnlock(sessionId)) {
    const ready = await ensureMasterPasswordReady();
    if (!ready) return;
  }
  const keyPassphrases = await prepareKeyPassphrases(sessionId);
  if (keyPassphrases === null) return;
  try {
    await apiStartSinglePortForward(sessionId, forwardId, keyPassphrases);
    if (portForwards.sessionId === sessionId) {
      await loadPortForwards(sessionId);
    }
  } catch (e) {
    console.error("Failed to start port forward:", e);
    throw e;
  }
}

export async function stopSinglePortForward(sessionId: number, forwardId: number) {
  try {
    await apiStopSinglePortForward(sessionId, forwardId);
    if (portForwards.sessionId === sessionId) {
      await loadPortForwards(sessionId);
    }
  } catch (e) {
    console.error("Failed to stop port forward:", e);
  }
}

function mergeBoundForwards(session: Session, list: PortForward[], statuses: ForwardStatus[]): BoundPortForward[] {
  return list.map((forward) => {
    const status = statuses.find((item) => item.id === forward.id);
    return {
      ...forward,
      sessionName: session.name,
      status: status?.status ?? "stopped",
      error: status?.error ?? null,
    };
  });
}

export async function loadSidebarPortForwards(sessionList: Session[]) {
  setPortForwards("sidebarLoading", true);
  try {
    const results = await Promise.all(
      sessionList.map(async (session) => {
        const [list, statuses] = await Promise.all([
          apiListPortForwards(session.id),
          apiGetPortForwardStatus(session.id),
        ]);
        return mergeBoundForwards(session, list, statuses);
      })
    );
    setPortForwards("sidebarList", results.flat().sort((a, b) => a.sessionName.localeCompare(b.sessionName) || a.id - b.id));
  } catch (e) {
    console.error("Failed to load sidebar port forwards:", e);
  } finally {
    setPortForwards("sidebarLoading", false);
  }
}

export async function refreshSidebarPortForwardStatus(sessionList: Session[]) {
  try {
    const sessionIds = new Set(sessionList.map((session) => session.id));
    const statusEntries = await Promise.all(
      sessionList.map(async (session) => ({
        statuses: await apiGetPortForwardStatus(session.id),
      }))
    );
    const statusById = new Map<number, ForwardStatus>();
    for (const entry of statusEntries) {
      for (const status of entry.statuses) {
        statusById.set(status.id, status);
      }
    }
    setPortForwards("sidebarList", (current) =>
      current.map((forward) => {
        if (!sessionIds.has(forward.session_id)) return forward;
        const status = statusById.get(forward.id);
        return {
          ...forward,
          status: status?.status ?? "stopped",
          error: status?.error ?? null,
        };
      })
    );
  } catch (e) {
    console.error("Failed to refresh sidebar port forward status:", e);
  }
}
