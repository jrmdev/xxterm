import { createStore } from "solid-js/store";
import { listen } from "@tauri-apps/api/event";
import { batch } from "solid-js";
import { cancelSftpQueue, sftpCreateDir, sftpDelete, sftpDeleteSelection, sftpListDir, sftpRename, sftpUpload, sftpUploadBytes, sftpUploadSelection } from "../ipc/commands";
import { getActiveSshSessionId, getActiveSshTerminalId } from "./terminals";

export interface RemoteFileEntry {
  name: string;
  path: string;
  isDir: boolean;
  isSymlink: boolean;
  size: number | null;
  modified: number | null;
}

export interface TransferState {
  terminalId: string;
  sessionId: number | null;
  queueId: string;
  transferId: string;
  direction: "upload" | "download";
  path: string;
  displayPath: string;
  transferred: number;
  total: number | null;
  done: boolean;
  error: string | null;
}

interface TransferRateState {
  latestTransferred: number;
  tickTransferred: number;
  lastTickAt: number;
  bytesPerSecond: number | null;
}

export interface EditUploadErrorState {
  terminalId: string;
  sessionId: number | null;
  remotePath: string;
  localPath: string;
  error: string;
}

export interface DeleteProgressState {
  terminalId: string;
  queueId: string;
  deleted: number;
  total: number;
  done: boolean;
  error: string | null;
}

export interface SftpViewState {
  sessionId: number | null;
  currentPath: string;
  entries: RemoteFileEntry[];
  selectedPaths: string[];
  selectionAnchorPath: string | null;
  loading: boolean;
  error: string;
  sftpStatus: "idle" | "initializing" | "ready";
}

function defaultView(sessionId: number | null = null): SftpViewState {
  return {
    sessionId,
    currentPath: ".",
    entries: [],
    selectedPaths: [],
    selectionAnchorPath: null,
    loading: false,
    error: "",
    sftpStatus: sessionId ? "initializing" : "idle",
  };
}

export const [fileBrowser, setFileBrowser] = createStore<{
  activePane: "sessions" | "sftp" | "forwards";
  activeTerminalId: string | null;
  showHiddenFiles: boolean;
  views: Record<string, SftpViewState>;
  transfers: TransferState[];
  transferRates: Record<string, TransferRateState>;
  deleteProgress: Record<string, DeleteProgressState>;
  cancellingQueues: Record<string, string>;
  editUploadError: EditUploadErrorState | null;
}>({
  activePane: "sessions",
  activeTerminalId: null,
  showHiddenFiles: false,
  views: {},
  transfers: [],
  transferRates: {},
  deleteProgress: {},
  cancellingQueues: {},
  editUploadError: null,
});

let sftpEventListenersBound = false;
let sftpEventListenersPromise: Promise<void> | null = null;
const directoryLoads = new Map<string, { token: number; sessionId: number; path: string }>();
let directoryLoadToken = 0;
let transferRateIntervalId: number | null = null;

function transferRateKey(terminalId: string, queueId: string, transferId: string) {
  return `${terminalId}:${queueId}:${transferId}`;
}

const STALE_RATE_MS = 2500;
const EMA_ALPHA = 0.3;

function reconcileTransferRates(now = Date.now()) {
  const activeKeys = new Set<string>();

  batch(() => {
    for (const transfer of fileBrowser.transfers) {
      const key = transferRateKey(transfer.terminalId, transfer.queueId, transfer.transferId);
      activeKeys.add(key);

      if (transfer.done || transfer.error) {
        setFileBrowser("transferRates", key, undefined!);
        continue;
      }

      const existing = fileBrowser.transferRates[key];
      if (!existing) {
        setFileBrowser("transferRates", key, {
          latestTransferred: transfer.transferred,
          tickTransferred: transfer.transferred,
          lastTickAt: now,
          bytesPerSecond: null,
        });
        continue;
      }

      const elapsed = now - existing.lastTickAt;
      if (elapsed < 1000) {
        continue;
      }

      const latestTransferred = Math.max(existing.latestTransferred, transfer.transferred);
      const delta = Math.max(0, latestTransferred - existing.tickTransferred);
      const rawSpeed = (delta * 1000) / Math.max(1, elapsed);
      const isStale = elapsed >= STALE_RATE_MS && delta === 0;
      let bytesPerSecond: number;
      if (isStale) {
        bytesPerSecond = 0;
      } else if (existing.bytesPerSecond === null || existing.bytesPerSecond === 0) {
        bytesPerSecond = Math.round(rawSpeed);
      } else {
        bytesPerSecond = Math.round(EMA_ALPHA * rawSpeed + (1 - EMA_ALPHA) * existing.bytesPerSecond);
      }
      setFileBrowser("transferRates", key, {
        latestTransferred,
        tickTransferred: latestTransferred,
        lastTickAt: now,
        bytesPerSecond,
      });
    }

    for (const key of Object.keys(fileBrowser.transferRates)) {
      if (!activeKeys.has(key)) {
        setFileBrowser("transferRates", key, undefined!);
      }
    }
  });
}

function ensureTransferRateTicker() {
  if (transferRateIntervalId !== null) return;
  transferRateIntervalId = window.setInterval(() => {
    reconcileTransferRates();
  }, 1000);
}

function updateTransferRateSample(incoming: TransferState, now = Date.now()) {
  const key = transferRateKey(incoming.terminalId, incoming.queueId, incoming.transferId);
  const existing = fileBrowser.transferRates[key];
  if (incoming.done || incoming.error) {
    setFileBrowser("transferRates", key, undefined!);
    return;
  }

  if (!existing) {
    setFileBrowser("transferRates", key, {
      latestTransferred: incoming.transferred,
      tickTransferred: incoming.transferred,
      lastTickAt: now,
      bytesPerSecond: null,
    });
    return;
  }

  if (incoming.transferred < existing.tickTransferred) {
    setFileBrowser("transferRates", key, {
      latestTransferred: incoming.transferred,
      tickTransferred: incoming.transferred,
      lastTickAt: now,
      bytesPerSecond: null,
    });
    return;
  }

  setFileBrowser("transferRates", key, "latestTransferred", incoming.transferred);
}

function upsertTransfer(incoming: TransferState) {
  const index = fileBrowser.transfers.findIndex((item) =>
    item.terminalId === incoming.terminalId
    && item.queueId === incoming.queueId
    && item.transferId === incoming.transferId
  );
  if (index >= 0) {
    if (fileBrowser.transfers[index].path !== incoming.path) {
      setFileBrowser("transferRates", transferRateKey(incoming.terminalId, incoming.queueId, incoming.transferId), undefined!);
    }
    setFileBrowser("transfers", index, incoming);
    return;
  }

  setFileBrowser("transfers", (current) => {
    const next = [...current, incoming];
    while (next.length > 6) {
      const completedIndex = next.findIndex((item) =>
        item.done && (!item.error || isSftpQueueCancelledError(item.error)),
      );
      if (completedIndex === -1) break;
      next.splice(completedIndex, 1);
    }
    return next;
  });
}

export function isSftpQueueCancelledError(error: unknown) {
  const message = error instanceof Error ? error.message : typeof error === "string" ? error : "";
  return /^(transfer|upload|download|delete) cancel(?:ed|led)$/i.test(message.trim());
}

function sftpProgressDismissDelay(error: string | null) {
  if (!error) return 1800;
  if (isSftpQueueCancelledError(error)) return 1000;
  return null;
}

export function dismissSftpTransfer(terminalId: string, queueId: string, transferId: string) {
  setFileBrowser("transfers", (current) => current.filter((item) => !(
    item.terminalId === terminalId && item.queueId === queueId && item.transferId === transferId
  )));
  setFileBrowser("transferRates", transferRateKey(terminalId, queueId, transferId), undefined!);
}

export function dismissSftpDelete(queueId: string) {
  setFileBrowser("deleteProgress", queueId, undefined!);
}

export function setSftpViewError(terminalId: string, error: string) {
  if (!fileBrowser.views[terminalId]) return;
  setFileBrowser("views", terminalId, "error", error);
}

export function clearSftpViewError(terminalId: string) {
  setSftpViewError(terminalId, "");
}

export function dismissEditUploadError() {
  setFileBrowser("editUploadError", null);
}

export async function ensureSftpEventListeners() {
  if (sftpEventListenersBound) return;
  if (sftpEventListenersPromise) return sftpEventListenersPromise;

  sftpEventListenersPromise = Promise.all([
    listen<TransferState>("sftp-transfer-progress", (event) => {
      const incoming = event.payload;
      if (!fileBrowser.views[incoming.terminalId]) return;
      upsertTransfer(incoming);
      updateTransferRateSample(incoming);

      if (incoming.done) {
        setFileBrowser("cancellingQueues", incoming.queueId, undefined!);
        const delay = sftpProgressDismissDelay(incoming.error);
        if (delay !== null) {
          window.setTimeout(() => dismissSftpTransfer(incoming.terminalId, incoming.queueId, incoming.transferId), delay);
        }
      }
    }),
    listen<DeleteProgressState>("sftp-delete-progress", (event) => {
      const incoming = event.payload;
      if (!fileBrowser.views[incoming.terminalId]) return;
      setFileBrowser("deleteProgress", incoming.queueId, incoming);
      if (incoming.done) {
        setFileBrowser("cancellingQueues", incoming.queueId, undefined!);
        const delay = sftpProgressDismissDelay(incoming.error);
        if (delay !== null) {
          window.setTimeout(() => dismissSftpDelete(incoming.queueId), delay);
        }
      }
    }),
    listen<EditUploadErrorState>("sftp-edit-upload-error", (event) => {
      const incoming = event.payload;
      setFileBrowser("editUploadError", incoming);
    }),
  ])
    .then(() => {
      sftpEventListenersBound = true;
      ensureTransferRateTicker();
    })
    .catch((error) => {
      sftpEventListenersPromise = null;
      throw error;
    });

  return sftpEventListenersPromise;
}

function showSftpForActiveTerminal() {
  const activeTerminalId = getActiveSshTerminalId();
  const activeSessionId = getActiveSshSessionId();
  if (activeTerminalId && activeSessionId) {
    openSftpForTerminal(activeTerminalId, activeSessionId);
    return true;
  }
  return false;
}

export function setSidebarPane(pane: "sessions" | "sftp" | "forwards") {
  if (pane === "sessions") {
    setFileBrowser("activePane", "sessions");
    setFileBrowser("activeTerminalId", null);
    return;
  }

  if (pane === "forwards") {
    setFileBrowser("activePane", "forwards");
    setFileBrowser("activeTerminalId", null);
    return;
  }

  if (showSftpForActiveTerminal()) {
    return;
  }

  setFileBrowser("activePane", "sftp");
  setFileBrowser("activeTerminalId", null);
}

export function setSelectedPaths(paths: string[], anchorPath: string | null = paths.length > 0 ? paths[paths.length - 1] : null) {
  const terminalId = fileBrowser.activeTerminalId;
  if (!terminalId) return;
  batch(() => {
    setFileBrowser("views", terminalId, "selectedPaths", paths);
    setFileBrowser("views", terminalId, "selectionAnchorPath", anchorPath);
  });
}

export function setShowHiddenFiles(show: boolean) {
  setFileBrowser("showHiddenFiles", show);
}

export function openSftpForTerminal(terminalId: string, sessionId: number) {
  const existing = fileBrowser.views[terminalId];
  const sessionChanged = existing?.sessionId !== sessionId;
  batch(() => {
    setFileBrowser("activePane", "sftp");
    setFileBrowser("activeTerminalId", terminalId);
  });

  if (!existing || sessionChanged) {
    setFileBrowser("views", terminalId, defaultView(sessionId));
    return;
  }

  batch(() => {
    setFileBrowser("views", terminalId, "sessionId", sessionId);
    setFileBrowser("views", terminalId, "sftpStatus", "initializing");
    setFileBrowser("views", terminalId, "error", "");
  });
}

export function clearSftpView(terminalId: string) {
  directoryLoads.delete(terminalId);
  batch(() => {
    setFileBrowser("views", terminalId, undefined!);
    setFileBrowser("transfers", (current) => current.filter((item) => item.terminalId !== terminalId));
    for (const key of Object.keys(fileBrowser.transferRates)) {
      if (key.startsWith(`${terminalId}:`)) {
        setFileBrowser("transferRates", key, undefined!);
      }
    }
    for (const progress of Object.values(fileBrowser.deleteProgress)) {
      if (progress.terminalId === terminalId) dismissSftpDelete(progress.queueId);
    }
    for (const [queueId, owner] of Object.entries(fileBrowser.cancellingQueues)) {
      if (owner === terminalId) setFileBrowser("cancellingQueues", queueId, undefined!);
    }
  });
  if (fileBrowser.activeTerminalId === terminalId) {
    setFileBrowser("activeTerminalId", null);
    if (fileBrowser.activePane === "sftp") {
      if (!showSftpForActiveTerminal()) {
        setFileBrowser("activePane", "sessions");
      }
    }
  }
}

export function syncSidebarWithActiveTerminal(preferSftp = false) {
  if (preferSftp) {
    if (showSftpForActiveTerminal()) {
      return;
    }
    if (fileBrowser.activePane === "sftp") {
      setSidebarPane("sessions");
    }
    return;
  }

  if (fileBrowser.activePane !== "sftp") return;
  if (showSftpForActiveTerminal()) {
    return;
  }
  setSidebarPane("sessions");
}

export function getActiveSftpView(): SftpViewState {
  const terminalId = fileBrowser.activeTerminalId;
  return terminalId ? (fileBrowser.views[terminalId] ?? defaultView()) : defaultView();
}

export function getTransfersForTerminal(terminalId: string | null): TransferState[] {
  return terminalId ? fileBrowser.transfers.filter((transfer) => transfer.terminalId === terminalId) : [];
}

export function getTransferSpeed(terminalId: string, queueId: string, transferId: string): number | null {
  return fileBrowser.transferRates[transferRateKey(terminalId, queueId, transferId)]?.bytesPerSecond ?? null;
}

export async function loadRemoteDirectory(terminalId: string, path?: string) {
  const view = fileBrowser.views[terminalId] ?? defaultView();
  const sessionId = view.sessionId;
  if (!sessionId) return;

  const pending = directoryLoads.get(terminalId);
  const requestedPath = path ?? (pending?.sessionId === sessionId ? pending.path : view.currentPath);
  const token = ++directoryLoadToken;
  directoryLoads.set(terminalId, { token, sessionId, path: requestedPath });
  const isStale = () => fileBrowser.views[terminalId]?.sessionId !== sessionId || directoryLoads.get(terminalId)?.token !== token;
  batch(() => {
    setFileBrowser("views", terminalId, "sftpStatus", "initializing");
    setFileBrowser("views", terminalId, "loading", true);
    setFileBrowser("views", terminalId, "error", "");
  });
  try {
    const listing = await sftpListDir(terminalId, requestedPath);
    if (isStale()) return;
    batch(() => {
      setFileBrowser("views", terminalId, "currentPath", listing.path);
      setFileBrowser("views", terminalId, "entries", listing.entries);
      setFileBrowser("views", terminalId, "selectedPaths", []);
      setFileBrowser("views", terminalId, "selectionAnchorPath", null);
      setFileBrowser("views", terminalId, "sftpStatus", "ready");
    });
  } catch (e: any) {
    if (isStale()) return;
    const message = e?.toString?.() ?? "Failed to load remote directory";
    if (message.includes("SFTP is only available while an SSH session is connected") && getActiveSshTerminalId() === terminalId) {
      batch(() => {
        setFileBrowser("views", terminalId, "sftpStatus", "initializing");
        setFileBrowser("views", terminalId, "error", "");
      });
    } else {
      batch(() => {
        setFileBrowser("views", terminalId, "sftpStatus", "idle");
        setFileBrowser("views", terminalId, "error", message);
      });
    }
  } finally {
    if (!isStale()) {
      directoryLoads.delete(terminalId);
      setFileBrowser("views", terminalId, "loading", false);
    }
  }
}

export async function deleteRemoteEntry(terminalId: string, entry: RemoteFileEntry) {
  await sftpDelete(terminalId, entry.path, entry.isDir);
  await loadRemoteDirectory(terminalId);
}

export async function deleteRemoteEntries(terminalId: string, entries: RemoteFileEntry[]) {
  try {
    await sftpDeleteSelection(terminalId, entries.map((entry) => ({ path: entry.path, isDir: entry.isDir })));
  } finally {
    await loadRemoteDirectory(terminalId).catch(() => undefined);
  }
}

export function getDeleteProgressForTerminal(terminalId: string | null): DeleteProgressState[] {
  return terminalId ? Object.values(fileBrowser.deleteProgress).filter((progress) => progress.terminalId === terminalId) : [];
}

export async function cancelActiveSftpQueue(terminalId: string, queueId: string) {
  if (fileBrowser.cancellingQueues[queueId]) return;
  setFileBrowser("cancellingQueues", queueId, terminalId);
  try {
    await cancelSftpQueue(terminalId, queueId);
  } catch (error) {
    setFileBrowser("cancellingQueues", queueId, undefined!);
    throw error;
  }
}

export async function renameRemoteEntry(terminalId: string, oldPath: string, newPath: string) {
  await sftpRename(terminalId, oldPath, newPath);
  await loadRemoteDirectory(terminalId);
}

export async function createRemoteDirectory(terminalId: string, path: string) {
  await sftpCreateDir(terminalId, path);
  await loadRemoteDirectory(terminalId);
}

export async function uploadLocalFile(terminalId: string, remotePath: string, localPath: string, refresh = true) {
  try {
    await sftpUpload(terminalId, remotePath, localPath);
  } catch (error) {
    if (refresh) {
      await loadRemoteDirectory(terminalId).catch(() => undefined);
    }
    throw error;
  }
  if (refresh) {
    await loadRemoteDirectory(terminalId);
  }
}

export async function uploadLocalFiles(terminalId: string, entries: { remotePath: string; localPath: string }[], refresh = true) {
  try {
    await sftpUploadSelection(terminalId, entries);
  } catch (error) {
    if (refresh) {
      await loadRemoteDirectory(terminalId).catch(() => undefined);
    }
    throw error;
  }
  if (refresh) {
    await loadRemoteDirectory(terminalId);
  }
}

export async function uploadFileBytes(terminalId: string, remotePath: string, data: Uint8Array, refresh = true) {
  try {
    await sftpUploadBytes(terminalId, remotePath, data);
  } catch (error) {
    if (refresh) {
      await loadRemoteDirectory(terminalId).catch(() => undefined);
    }
    throw error;
  }
  if (refresh) {
    await loadRemoteDirectory(terminalId);
  }
}
