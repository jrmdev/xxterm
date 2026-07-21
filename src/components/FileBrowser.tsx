import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { open } from "@tauri-apps/plugin-dialog";
import {
  cancelActiveSftpQueue,
  clearSftpViewError,
  fileBrowser,
  getActiveSftpView,
  getDeleteProgressForTerminal,
  getTransferSpeed,
  getTransfersForTerminal,
  isSftpQueueCancelledError,
  loadRemoteDirectory,
  setShowHiddenFiles,
  setSelectedPaths,
  createRemoteDirectory,
  deleteRemoteEntries,
  renameRemoteEntry,
  setSftpViewError,
  uploadLocalFiles,
  type RemoteFileEntry,
} from "../stores/fileBrowser";
import { getActiveSshTerminalId } from "../stores/terminals";
import { settings } from "../stores/settings";
import { openLocalPath, sftpDownload, sftpDownloadSelection, sftpEdit } from "../ipc/commands";
import { listingIconStyle, listingMetaStyle, listingRowStyle } from "./sidebarListStyles";

function parentPath(path: string) {
  const normalized = path.replace(/\/+$/, "");
  if (normalized === "" || normalized === "/") return "/";
  const idx = normalized.lastIndexOf("/");
  return idx <= 0 ? "/" : normalized.slice(0, idx);
}

function joinRemotePath(dir: string, name: string) {
  if (dir === ".") return `./${name}`;
  if (dir === "/") return `/${name}`;
  return `${dir.replace(/\/+$/, "")}/${name}`;
}

function formatBytes(value: number | null) {
  if (value === null) return "";
  if (value < 1024) return `${value}\u00A0B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)}\u00A0KB`;
  if (value < 1024 * 1024 * 1024) return `${(value / (1024 * 1024)).toFixed(1)}\u00A0MB`;
  return `${(value / (1024 * 1024 * 1024)).toFixed(1)}\u00A0GB`;
}

function formatTransferSpeed(value: number | null) {
  if (value === null) return "";
  if (value < 1000) return `${value}\u00A0B/s`;
  if (value < 1000 * 1000) return `${(value / 1000).toFixed(1)}\u00A0KB/s`;
  if (value < 1000 * 1000 * 1000) return `${(value / (1000 * 1000)).toFixed(1)}\u00A0MB/s`;
  return `${(value / (1000 * 1000 * 1000)).toFixed(1)}\u00A0GB/s`;
}

function formatModified(value: number | null) {
  if (!value) return "";
  return new Date(value * 1000).toLocaleString(undefined, { year: "numeric", month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit", hour12: false });
}

function isHiddenEntry(entry: RemoteFileEntry) {
  return entry.name.startsWith(".");
}

const archiveExtensions = new Set(["7z", "bz2", "gz", "rar", "tar", "tgz", "txz", "xz", "zip", "zst"]);
const archiveSuffixes = [".tar.bz2", ".tar.gz", ".tar.xz", ".tar.zst"];
const codeExtensions = new Set([
  "c", "cc", "clj", "cljs", "cpp", "cs", "css", "dart", "ex", "exs", "fs", "go", "h", "hpp",
  "html", "java", "js", "jsx", "kt", "kts", "lua", "m", "mm", "php", "pl", "py", "rb", "rs",
  "scala", "scss", "sh", "sql", "svelte", "swift", "ts", "tsx", "vue", "zig",
]);
const codeFilenames = new Set(["dockerfile", "makefile", "rakefile", "gemfile", "justfile"]);
const configExtensions = new Set(["cfg", "conf", "env", "ini", "json", "lock", "toml", "yaml", "yml", "xml"]);
const dataExtensions = new Set(["csv", "jsonl", "ndjson", "parquet", "tsv", "xls", "xlsx"]);
const databaseExtensions = new Set(["db", "sqlite", "sqlite3"]);
const documentExtensions = new Set(["doc", "docx", "odt", "pages", "pdf", "ppt", "pptx"]);
const imageExtensions = new Set(["avif", "bmp", "gif", "heic", "ico", "jpg", "jpeg", "png", "svg", "tif", "tiff", "webp"]);
const mediaExtensions = new Set(["aac", "flac", "m4a", "mid", "midi", "mp3", "ogg", "opus", "wav", "webm"]);
const videoExtensions = new Set(["avi", "m4v", "mkv", "mov", "mp4", "mpeg", "mpg", "webm", "wmv"]);
const keyExtensions = new Set(["asc", "cer", "crt", "key", "pem", "pub"]);
const textExtensions = new Set(["log", "md", "rst", "text", "txt"]);

function fileExtension(name: string) {
  const index = name.lastIndexOf(".");
  return index > 0 && index < name.length - 1 ? name.slice(index + 1).toLowerCase() : "";
}

function entryIcon(entry: RemoteFileEntry) {
  if (entry.isSymlink) return "\uD83D\uDD17";
  if (entry.isDir) return "\uD83D\uDCC1";

  const name = entry.name.toLowerCase();
  const ext = fileExtension(name);
  if (archiveExtensions.has(ext) || archiveSuffixes.some((suffix) => name.endsWith(suffix))) return "\uD83D\uDCE6";
  if (codeExtensions.has(ext) || codeFilenames.has(name)) return "</>";
  if (configExtensions.has(ext)) return "\u2699\uFE0F";
  if (dataExtensions.has(ext)) return "\uD83D\uDCCA";
  if (databaseExtensions.has(ext)) return "\uD83D\uDDC4\uFE0F";
  if (documentExtensions.has(ext)) return ext === "pdf" ? "\uD83D\uDCD5" : "\uD83D\uDCC3";
  if (imageExtensions.has(ext)) return "\uD83D\uDDBC\uFE0F";
  if (videoExtensions.has(ext)) return "\uD83C\uDF9E\uFE0F";
  if (mediaExtensions.has(ext)) return "\uD83C\uDFB5";
  if (keyExtensions.has(ext)) return "\uD83D\uDD11";
  if (textExtensions.has(ext)) return "\uD83D\uDCDD";
  return "\uD83D\uDCC4";
}

function localBaseName(path: string) {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

function formatErrorMessage(error: unknown, fallback: string) {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error.trim()) return error;
  if (error && typeof error === "object" && "toString" in error) {
    const message = error.toString();
    if (message && message !== "[object Object]") return message;
  }
  return fallback;
}

function selectionRange(paths: string[], anchorPath: string, targetPath: string) {
  const anchorIndex = paths.indexOf(anchorPath);
  const targetIndex = paths.indexOf(targetPath);
  if (anchorIndex === -1 || targetIndex === -1) return [targetPath];
  const start = Math.min(anchorIndex, targetIndex);
  const end = Math.max(anchorIndex, targetIndex);
  return paths.slice(start, end + 1);
}

function orderedSelection(paths: string[], selected: Set<string>) {
  return paths.filter((path) => selected.has(path));
}

export default function FileBrowser() {
  let retryTimer: number | undefined;
  let errorOverlayTimer: number | undefined;
  let typeaheadResetTimer: number | undefined;
  let listingRef: HTMLDivElement | undefined;
  const entryRefs = new Map<string, HTMLDivElement>();
  const [busyPath, setBusyPath] = createSignal<string | null>(null);
  const [contextMenu, setContextMenu] = createSignal<{ x: number; y: number; entry: RemoteFileEntry | null } | null>(null);
  const [visibleError, setVisibleError] = createSignal("");
  const [renameDialog, setRenameDialog] = createSignal<{ entry: RemoteFileEntry; name: string } | null>(null);
  let renameInputRef: HTMLInputElement | undefined;
  const [newFolderDialog, setNewFolderDialog] = createSignal<string | null>(null);
  let newFolderInputRef: HTMLInputElement | undefined;
  const [typeaheadQuery, setTypeaheadQuery] = createSignal("");
  const terminalId = createMemo(() => getActiveSshTerminalId());
  const activeView = createMemo(() => getActiveSftpView());
  const visibleEntries = createMemo(() => {
    const entries = activeView().entries;
    return fileBrowser.showHiddenFiles ? entries : entries.filter((entry) => !isHiddenEntry(entry));
  });
  const selectedPathSet = createMemo(() => new Set(activeView().selectedPaths));
  const selectedEntries = createMemo(() => {
    const selected = selectedPathSet();
    return activeView().entries.filter((entry) => selected.has(entry.path));
  });
  const contextMenuEntries = createMemo(() => {
    const menu = contextMenu();
    const entries = selectedEntries();
    if (!menu?.entry) return entries;
    if (entries.some((entry) => entry.path === menu.entry!.path)) {
      return entries;
    }
    return [menu.entry];
  });
  const contextMenuSingleEntry = createMemo(() => (contextMenuEntries().length === 1 ? contextMenuEntries()[0] : null));
  const [pathInput, setPathInput] = createSignal(activeView().currentPath);

  createEffect(() => {
    const activeTerminalId = terminalId();
    const sessionId = activeView().sessionId;
    if (fileBrowser.activePane !== "sftp" || !activeTerminalId || !sessionId) return;
    void loadRemoteDirectory(activeTerminalId);
  });

  createEffect(() => {
    if (retryTimer !== undefined) {
      window.clearTimeout(retryTimer);
      retryTimer = undefined;
    }
    const activeTerminalId = terminalId();
    const view = activeView();
    if (fileBrowser.activePane !== "sftp" || !activeTerminalId) return;
    if (view.sftpStatus !== "initializing" || view.loading) return;
    retryTimer = window.setTimeout(() => {
      const nextTerminalId = terminalId();
      if (!nextTerminalId) return;
      void loadRemoteDirectory(nextTerminalId);
    }, 250);
  });

  createEffect(() => {
    setPathInput(activeView().currentPath);
  });

  createEffect(() => {
    activeView().currentPath;
    visibleEntries().length;
    resetTypeahead();
  });

  createEffect(() => {
    const error = activeView().error;
    if (errorOverlayTimer !== undefined) {
      window.clearTimeout(errorOverlayTimer);
      errorOverlayTimer = undefined;
    }

    setVisibleError(error);
    if (error) {
      errorOverlayTimer = window.setTimeout(() => {
        setVisibleError("");
        errorOverlayTimer = undefined;
      }, 2000);
    }
  });

  createEffect(() => {
    if (contextMenu()) {
      const handler = (e: MouseEvent) => {
        const target = e.target as HTMLElement;
        if (!target.closest(".context-menu")) {
          setContextMenu(null);
        }
      };
      document.addEventListener("click", handler);
      onCleanup(() => document.removeEventListener("click", handler));
    }
  });

  createEffect(() => {
    if (renameDialog() && renameInputRef) {
      renameInputRef.focus();
      renameInputRef.select();
    }
  });

  createEffect(() => {
    if (newFolderDialog() && newFolderInputRef) {
      newFolderInputRef.focus();
    }
  });

  onCleanup(() => {
    if (retryTimer !== undefined) {
      window.clearTimeout(retryTimer);
    }
    if (errorOverlayTimer !== undefined) {
      window.clearTimeout(errorOverlayTimer);
    }
    if (typeaheadResetTimer !== undefined) {
      window.clearTimeout(typeaheadResetTimer);
    }
  });

  const refresh = async () => {
    const activeTerminalId = terminalId();
    if (!activeTerminalId) return;
    await loadRemoteDirectory(activeTerminalId);
  };

  const browseToInputPath = async () => {
    const activeTerminalId = terminalId();
    if (!activeTerminalId) return;
    const nextPath = pathInput().trim();
    if (!nextPath) {
      setPathInput(activeView().currentPath);
      return;
    }
    await loadRemoteDirectory(activeTerminalId, nextPath);
  };

  const toggleHiddenFiles = () => {
    const showHidden = !fileBrowser.showHiddenFiles;
    if (!showHidden) {
      const visiblePaths = activeView().entries
        .filter((entry) => !isHiddenEntry(entry))
        .map((entry) => entry.path);
      const visiblePathSet = new Set(visiblePaths);
      const selectedPaths = activeView().selectedPaths.filter((path) => visiblePathSet.has(path));
      const anchorPath = activeView().selectionAnchorPath;
      setSelectedPaths(
        selectedPaths,
        anchorPath && visiblePathSet.has(anchorPath) ? anchorPath : selectedPaths[selectedPaths.length - 1] ?? null,
      );
    }
    setShowHiddenFiles(showHidden);
    setContextMenu(null);
  };

  const focusListing = () => {
    listingRef?.focus();
  };

  const resetTypeahead = () => {
    if (typeaheadResetTimer !== undefined) {
      window.clearTimeout(typeaheadResetTimer);
      typeaheadResetTimer = undefined;
    }
    setTypeaheadQuery("");
  };

  const queueTypeaheadReset = () => {
    if (typeaheadResetTimer !== undefined) {
      window.clearTimeout(typeaheadResetTimer);
    }
    typeaheadResetTimer = window.setTimeout(() => {
      typeaheadResetTimer = undefined;
      setTypeaheadQuery("");
    }, 3000);
  };

  const selectEntryByTyping = (typedCharacter: string) => {
    const entries = visibleEntries();
    if (entries.length === 0) return;

    const singleCharacterQuery = typedCharacter.toLocaleLowerCase();
    const nextQuery = `${typeaheadQuery()}${typedCharacter}`.toLocaleLowerCase();
    const startIndex = Math.max(0, entries.findIndex((entry) => entry.path === activeView().selectedPaths[0]));
    const rotatedEntries = [...entries.slice(startIndex), ...entries.slice(0, startIndex)];
    const match = rotatedEntries.find((entry) => entry.name.toLocaleLowerCase().startsWith(nextQuery))
      ?? entries.find((entry) => entry.name.toLocaleLowerCase().startsWith(nextQuery));
    const fallbackMatch = rotatedEntries.find((entry) => entry.name.toLocaleLowerCase().startsWith(singleCharacterQuery))
      ?? entries.find((entry) => entry.name.toLocaleLowerCase().startsWith(singleCharacterQuery));

    const resolvedMatch = match ?? fallbackMatch;
    if (!resolvedMatch) {
      resetTypeahead();
      return;
    }

    setTypeaheadQuery(match ? nextQuery : singleCharacterQuery);
    queueTypeaheadReset();
    setSelectedPaths([resolvedMatch.path], resolvedMatch.path);
    entryRefs.get(resolvedMatch.path)?.scrollIntoView({ block: "nearest" });
  };

  const handleListingKeyDown = (event: KeyboardEvent) => {
    if (event.ctrlKey || event.metaKey || event.altKey || event.isComposing) return;
    if (event.key.length !== 1 || /\p{C}/u.test(event.key)) return;
    if (contextMenu() || renameDialog() || newFolderDialog() !== null) return;

    event.preventDefault();
    selectEntryByTyping(event.key);
  };

  const handleEntryMouseDown = (event: MouseEvent, entry: RemoteFileEntry) => {
    if (event.button !== 0) return;
    event.preventDefault();
    focusListing();
    resetTypeahead();

    const visiblePaths = visibleEntries().map((item) => item.path);
    const currentSelection = new Set(activeView().selectedPaths);

    const additive = event.ctrlKey || event.metaKey;
    if (event.shiftKey) {
      const anchorPath = activeView().selectionAnchorPath ?? entry.path;
      const nextSelection = additive ? new Set(currentSelection) : new Set<string>();
      for (const path of selectionRange(visiblePaths, anchorPath, entry.path)) {
        nextSelection.add(path);
      }
      setSelectedPaths(orderedSelection(visiblePaths, nextSelection), anchorPath);
      return;
    }

    if (additive) {
      if (currentSelection.has(entry.path)) {
        currentSelection.delete(entry.path);
      } else {
        currentSelection.add(entry.path);
      }
      setSelectedPaths(orderedSelection(visiblePaths, currentSelection), entry.path);
      return;
    }

    setSelectedPaths([entry.path], entry.path);
  };

  const openEntry = async (entry: RemoteFileEntry) => {
    if (entry.isDir) {
      const activeTerminalId = terminalId();
      if (!activeTerminalId) return;
      await loadRemoteDirectory(activeTerminalId, entry.path);
      return;
    }
    await openWithDefault(entry);
  };

  const openWithDefault = async (entry: RemoteFileEntry) => {
    const activeTerminalId = terminalId();
    if (!activeTerminalId) return;
    clearSftpViewError(activeTerminalId);
    setBusyPath(entry.path);
    try {
      const localPath = await sftpDownload(activeTerminalId, entry.path);
      await openLocalPath(localPath);
    } catch (error) {
      if (isSftpQueueCancelledError(error)) return;
      setSftpViewError(activeTerminalId, formatErrorMessage(error, `Failed to open ${entry.name}`));
    } finally {
      setBusyPath(null);
    }
  };

  const downloadEntries = async (entries: RemoteFileEntry[]) => {
    const activeTerminalId = terminalId();
    if (!activeTerminalId || entries.length === 0) return;

    clearSftpViewError(activeTerminalId);

    let destinationDir: string;
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: "Download to",
      });
      if (!selected) return;
      destinationDir = Array.isArray(selected) ? selected[0] : selected;
    } catch (error) {
      setSftpViewError(activeTerminalId, formatErrorMessage(error, "Failed to open directory picker"));
      return;
    }

    if (!destinationDir) return;

    try {
      await sftpDownloadSelection(activeTerminalId, entries.map((entry) => entry.path), destinationDir);
    } catch (error) {
      if (isSftpQueueCancelledError(error)) return;
      const fallback = entries.length === 1 ? `Failed to download ${entries[0].name}` : "Failed to download selection";
      setSftpViewError(activeTerminalId, formatErrorMessage(error, fallback));
    }
  };

  const editEntry = async (entry: RemoteFileEntry) => {
    const activeTerminalId = terminalId();
    if (!activeTerminalId) return;
    clearSftpViewError(activeTerminalId);
    setBusyPath(entry.path);
    try {
      await sftpEdit(activeTerminalId, entry.path, settings.codeEditorCommand || undefined);
    } catch (error) {
      if (isSftpQueueCancelledError(error)) return;
      setSftpViewError(activeTerminalId, formatErrorMessage(error, `Failed to edit ${entry.name}`));
    } finally {
      setBusyPath(null);
    }
  };

  const doRename = async (entry: RemoteFileEntry, nextName: string) => {
    const activeTerminalId = terminalId();
    if (!activeTerminalId) return;
    const nextPath = joinRemotePath(parentPath(entry.path), nextName);
    clearSftpViewError(activeTerminalId);
    try {
      await renameRemoteEntry(activeTerminalId, entry.path, nextPath);
    } catch (error) {
      setSftpViewError(activeTerminalId, formatErrorMessage(error, `Failed to rename ${entry.name}`));
    }
  };

  const handleUpload = async () => {
    const activeTerminalId = terminalId();
    if (!activeTerminalId) return;

    clearSftpViewError(activeTerminalId);
    let paths: string[];
    try {
      const selected = await open({
        multiple: true,
        directory: false,
        title: "Select files to upload",
      });
      if (!selected) return;
      paths = Array.isArray(selected) ? selected : [selected];
    } catch (error) {
      setSftpViewError(activeTerminalId, formatErrorMessage(error, "Failed to open file picker"));
      return;
    }

    if (paths.length === 0) return;

    let uploadError = "";

    try {
      const currentPath = activeView().currentPath;
      await uploadLocalFiles(activeTerminalId, paths.map((localPath) => ({
        remotePath: joinRemotePath(currentPath, localBaseName(localPath)),
        localPath,
      })));
    } catch (error) {
      if (isSftpQueueCancelledError(error)) return;
      uploadError = formatErrorMessage(error, "Failed to upload file");
      setSftpViewError(activeTerminalId, uploadError);
    } finally {
      if (uploadError) {
        setSftpViewError(activeTerminalId, uploadError);
      }
    }
  };

  const activeTransfers = createMemo(() => getTransfersForTerminal(terminalId()));
  const deleteProgress = createMemo(() => getDeleteProgressForTerminal(terminalId()));
  const cancelQueue = async (queueId: string) => {
    const activeTerminalId = terminalId();
    if (!activeTerminalId) return;
    try {
      await cancelActiveSftpQueue(activeTerminalId, queueId);
    } catch (error) {
      if (isSftpQueueCancelledError(error) || /no longer active/i.test(formatErrorMessage(error, ""))) return;
      setSftpViewError(activeTerminalId, formatErrorMessage(error, "Failed to cancel transfer"));
    }
  };
  const activityLabel = createMemo(() => {
    const view = activeView();
    if (view.sftpStatus === "initializing" && view.entries.length === 0) return "SFTP initializing...";
    if (view.loading && (view.sftpStatus !== "initializing" || view.entries.length > 0)) return "Loading...";
    return "";
  });

  return (
    <div style={{ display: "flex", "flex-direction": "column", height: "100%", color: "var(--app-text)" }}>
      <div style={{ padding: "8px", "border-bottom": "1px solid var(--app-border)", display: "flex", "flex-direction": "column", gap: "8px" }}>
        <div style={{ display: "flex", "justify-content": "space-between", "align-items": "center" }}>
          <span style={{ color: "var(--app-text)", "font-size": "11px", "text-transform": "uppercase", "letter-spacing": "1px" }}>SFTP</span>
          <div style={{ display: "flex", "align-items": "center", gap: "6px" }}>
            <Show when={activityLabel()}>
              <span class="sftp-spinner" title={activityLabel()}>{"\u27F3"}</span>
            </Show>
            <button style={toolbarButtonStyle} onClick={() => void handleUpload()} disabled={!terminalId()} title="Upload files">{"\u2B06"}</button>
            <button style={toolbarButtonStyle} onClick={refresh} disabled={!terminalId()} title="Refresh">{"\u21BB"}</button>
          </div>
        </div>
        <Show when={terminalId()} fallback={<div style={{ color: "var(--app-subtle)", "font-size": "12px" }}>Connect an SSH session to browse remote files.</div>}>
          <div style={{ display: "flex", gap: "6px" }}>
            <button
              style={toolbarButtonStyle}
              onClick={async () => {
                const activeTerminalId = terminalId();
                if (!activeTerminalId) return;
                await loadRemoteDirectory(activeTerminalId, parentPath(activeView().currentPath));
              }}
              title="Up one level"
            >
              {"\u21B0"}
            </button>
            <input
              value={pathInput()}
              onInput={(event) => setPathInput(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  void browseToInputPath();
                } else if (event.key === "Escape") {
                  setPathInput(activeView().currentPath);
                }
              }}
              onBlur={() => setPathInput(activeView().currentPath)}
              style={{
                flex: 1,
                height: "26px",
                background: "var(--app-input)",
                border: "1px solid var(--app-input-border)",
                "border-radius": "4px",
                color: "var(--app-text)",
                "font-size": "12px",
                padding: "0 8px",
                outline: "none",
              }}
              title="Remote path"
            />
            <button
              style={toolbarButtonStyle}
              onClick={() => {
                setNewFolderDialog("");
              }}
              title="New folder"
            >
              +
            </button>
          </div>
        </Show>
      </div>

      <div style={{ flex: 1, overflow: "auto", position: "relative" }}>
        <Show when={visibleError()}>
          <div style={{
            position: "absolute",
            top: "8px",
            left: "8px",
            right: "8px",
            color: "var(--app-danger)",
            "font-size": "12px",
            padding: "8px",
            background: "var(--app-surface)",
            border: "1px solid var(--app-danger)",
            "border-radius": "4px",
            "z-index": 2,
            "box-shadow": "0 6px 18px rgba(0, 0, 0, 0.25)",
          }}>
            {visibleError()}
          </div>
        </Show>
        <div
          ref={listingRef}
          tabindex={0}
          style={{ height: "100%", outline: "none" }}
          onKeyDown={handleListingKeyDown}
          onBlur={() => resetTypeahead()}
          onMouseDown={(event) => {
            if (event.button !== 0 || event.target !== event.currentTarget) return;
            focusListing();
            resetTypeahead();
            setSelectedPaths([], null);
            setContextMenu(null);
          }}
          onContextMenu={(event) => {
            if (event.target !== event.currentTarget) return;
            event.preventDefault();
            focusListing();
            resetTypeahead();
            setSelectedPaths([], null);
            setContextMenu({ x: event.clientX, y: event.clientY, entry: null });
          }}
        >
        <For each={visibleEntries()}>
          {(entry) => (
            <div
              ref={(element) => entryRefs.set(entry.path, element)}
              onMouseDown={(event) => handleEntryMouseDown(event, entry)}
              onDblClick={() => void openEntry(entry)}
              onContextMenu={(event) => {
                event.preventDefault();
                event.stopPropagation();
                focusListing();
                resetTypeahead();
                if (!selectedPathSet().has(entry.path)) {
                  setSelectedPaths([entry.path], entry.path);
                }
                setContextMenu({ x: event.clientX, y: event.clientY, entry });
              }}
              style={{ ...listingRowStyle, "background-color": selectedPathSet().has(entry.path) ? "var(--app-selection)" : "transparent" }}
            >
              <span style={listingIconStyle}>{entryIcon(entry)}</span>
              <div style={{ overflow: "hidden" }}>
                <div style={{ "font-size": "13px", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>
                  {entry.name}
                  <Show when={busyPath() === entry.path}>
                    <span style={{ color: "var(--app-subtle)" }}> {"\u2022"} working</span>
                  </Show>
                </div>
                <div style={listingMetaStyle}>
                  {formatModified(entry.modified)}
                </div>
              </div>
              <div style={{ color: "var(--app-muted)", "font-size": "11px", "font-family": "var(--form-mono)", "align-self": "center" }}>{entry.isDir ? "" : formatBytes(entry.size)}</div>
            </div>
          )}
        </For>
        <Show when={!activeView().loading && activeView().sftpStatus !== "initializing" && visibleEntries().length === 0 && !activeView().error}>
          <div style={{ color: "var(--app-empty)", "font-size": "12px", padding: "16px", "text-align": "center" }}>This folder is empty.</div>
        </Show>
        </div>
      </div>

      <Show when={activeTransfers().length > 0}>
        <div style={{ "border-top": "1px solid var(--app-border)", background: "var(--app-surface)", padding: "8px", display: "flex", "flex-direction": "column", gap: "6px" }}>
          <For each={activeTransfers()}>
            {(transfer) => {
              const isCancelled = () => isSftpQueueCancelledError(transfer.error);
              const percent = () => transfer.total === null
                ? null
                : transfer.total === 0
                  ? 100
                  : Math.min(100, Math.round((transfer.transferred / transfer.total) * 100));
              const progressLabel = () => {
                const p = percent();
                return p === null
                  ? transfer.transferred > 0
                    ? formatBytes(transfer.transferred)
                    : ""
                  : `${p}%`;
              };
              const speedLabel = () => {
                if (transfer.done || transfer.error || isCancelled()) return "";
                return formatTransferSpeed(getTransferSpeed(transfer.terminalId, transfer.queueId, transfer.transferId));
              };
              const transferTitle = () => {
                if (isCancelled()) {
                  return `${transfer.direction === "upload" ? "Upload" : "Download"} cancelled`;
                }
                return `${transfer.direction === "upload" ? "Upload" : "Download"} ${transfer.displayPath}`;
              };
              return (
                <div>
                  <div style={{ display: "flex", "justify-content": "space-between", gap: "8px", "font-size": "11px", color: transfer.error ? "var(--app-danger)" : "var(--app-text)" }}>
                    <span style={{ overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>
                      {transferTitle()}
                    </span>
                    <span style={{ "white-space": "nowrap" }}>{isCancelled() ? "" : [progressLabel(), speedLabel()].filter(Boolean).join("\u00A0\u2022\u00A0")}</span>
                  </div>
                  <div style={{ display: "flex", "align-items": "center", gap: "8px", margin: "4px 0 0" }}>
                    <div style={{ flex: 1, height: "4px", background: "var(--app-progress-track)", "border-radius": "999px", overflow: "hidden" }}>
                      <div
                        class={percent() === null ? "sftp-progress-fill sftp-progress-fill--indeterminate" : "sftp-progress-fill"}
                        style={{
                          width: percent() === null ? "38%" : `${percent()}%`,
                          background: transfer.error ? "var(--app-danger)" : "var(--app-accent)",
                        }}
                      />
                    </div>
                    <Show when={!transfer.done && !transfer.error}>
                      <button
                        class="sftp-transfer-cancel-btn"
                        type="button"
                        title="Cancel transfer queue"
                        aria-label="Cancel transfer queue"
                        onClick={() => void cancelQueue(transfer.queueId)}
                      >
                        x
                      </button>
                    </Show>
                  </div>
                </div>
              );
            }}
          </For>
        </div>
      </Show>

      <Show when={deleteProgress()}>
        {(progress) => {
          const isCancelled = () => isSftpQueueCancelledError(progress().error);
          const percent = () => progress().total === 0 ? 100 : Math.min(100, Math.round((progress().deleted / progress().total) * 100));
          return (
            <div style={{ "border-top": "1px solid var(--app-border)", background: "var(--app-surface)", padding: "8px" }}>
              <div style={{ display: "flex", "justify-content": "space-between", gap: "8px", "font-size": "11px", color: progress().error ? "var(--app-danger)" : "var(--app-text)" }}>
                <span>{isCancelled() ? "Delete cancelled" : progress().error ? "Delete failed" : progress().done ? "Deleted" : "Deleting..."}</span>
                <span>{isCancelled() ? "" : `${progress().deleted}/${progress().total}`}</span>
              </div>
              <div style={{ display: "flex", "align-items": "center", gap: "8px", margin: "4px 0 0" }}>
                <div style={{ flex: 1, height: "4px", background: "var(--app-progress-track)", "border-radius": "999px", overflow: "hidden" }}>
                  <div style={{ width: `${percent()}%`, height: "100%", background: progress().error ? "var(--app-danger)" : "var(--app-accent)", transition: "width 0.15s ease" }} />
                </div>
                <Show when={!progress().done && !progress().error}>
                  <button
                    class="sftp-transfer-cancel-btn"
                    type="button"
                    title="Cancel delete queue"
                    aria-label="Cancel delete queue"
                    onClick={() => void cancelQueue(progress().queueId)}
                  >
                    x
                  </button>
                </Show>
              </div>
            </div>
          );
        }}
      </Show>

      <Show when={contextMenu()}>
        <div
          class="context-menu"

          style={{
            position: "fixed",
            left: `${Math.min(contextMenu()!.x, window.innerWidth - 190)}px`,
            top: `${(() => {
              const y = contextMenu()!.y;
              const selectedCount = contextMenuEntries().length;
              const singleEntry = contextMenuSingleEntry();
              let itemCount = 1;
              if (selectedCount > 0) {
                itemCount += 2;
              }
              if (singleEntry) {
                itemCount += singleEntry.isDir ? 2 : 3;
              }
              const h = itemCount * 30 + 8;
              return y + h <= window.innerHeight ? y : Math.max(0, y - h);
            })()}px`,
            background: "var(--app-surface-alt)",
            border: "1px solid var(--app-border)",
            "border-radius": "4px",
            padding: "4px 0",
            "z-index": "1500",
            "min-width": "180px",
            "box-shadow": "0 4px 12px rgba(0,0,0,0.3)",
          }}
        >
          <MenuItem
            label={fileBrowser.showHiddenFiles ? "Hide Hidden Files" : "Show Hidden Files"}
            onClick={() => { void toggleHiddenFiles(); }}
          />
          <Show when={contextMenuEntries().length > 0}>
            <Show when={contextMenuSingleEntry()}>
              <MenuItem label={contextMenuSingleEntry()!.isDir ? "Open Folder" : "Open"} onClick={() => { void openEntry(contextMenuSingleEntry()!); setContextMenu(null); }} />
              <Show when={!contextMenuSingleEntry()!.isDir}>
                <MenuItem label="Edit" onClick={() => { void editEntry(contextMenuSingleEntry()!); setContextMenu(null); }} />
              </Show>
            </Show>
            <MenuItem label="Download" onClick={() => { void downloadEntries(contextMenuEntries()); setContextMenu(null); }} />
            <Show when={contextMenuSingleEntry()}>
            <MenuItem label="Rename" onClick={() => {
              const entry = contextMenuSingleEntry()!;
              setContextMenu(null);
              setRenameDialog({ entry, name: entry.name });
            }} />
            </Show>
            <MenuItem label={contextMenuEntries().length > 1 ? `Delete (${contextMenuEntries().length} items)` : "Delete"} danger onClick={() => {
              const activeTerminalId = terminalId();
              const entries = contextMenuEntries();
              const confirmed = entries.length > 1
                ? window.confirm(`Delete ${entries.length} items?`)
                : entries.length === 1
                  ? entries[0].isDir
                    ? window.confirm(`Delete folder ${entries[0].name} and all contents?`)
                    : window.confirm(`Delete ${entries[0].name}?`)
                  : false;
              if (activeTerminalId && confirmed && entries.length > 0) {
                clearSftpViewError(activeTerminalId);
                void (async () => {
                  try {
                    await deleteRemoteEntries(activeTerminalId, entries);
                  } catch (error) {
                    if (isSftpQueueCancelledError(error)) return;
                    const fallback = entries.length === 1 ? `Failed to delete ${entries[0].name}` : `Failed to delete ${entries.length} items`;
                    setSftpViewError(activeTerminalId, formatErrorMessage(error, fallback));
                  }
                })();
              }
              setContextMenu(null);
            }} />
          </Show>
        </div>
      </Show>

      <Show when={newFolderDialog() !== null}>
        <div
          style={{
            position: "fixed",
            inset: "0",
            background: "var(--app-overlay)",
            display: "flex",
            "align-items": "center",
            "justify-content": "center",
            "z-index": 5000,
          }}
          onMouseDown={() => setNewFolderDialog(null)}
          onContextMenu={(e) => {
            e.preventDefault();
            e.stopPropagation();
            setNewFolderDialog(null);
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
                <div class="form-panel__eyebrow">NEW FOLDER</div>
              </div>
              <button class="form-icon-button" onClick={() => setNewFolderDialog(null)} aria-label="Close new folder dialog">x</button>
            </div>
            <input
              ref={newFolderInputRef}
              class="form-input"
              value={newFolderDialog()!}
              onInput={(event) => setNewFolderDialog(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  const name = newFolderDialog()?.trim();
                  if (name) {
                    setNewFolderDialog(null);
                    const activeTerminalId = terminalId();
                    if (activeTerminalId) {
                      clearSftpViewError(activeTerminalId);
                      void createRemoteDirectory(activeTerminalId, joinRemotePath(activeView().currentPath, name)).catch((error) => {
                        setSftpViewError(activeTerminalId, formatErrorMessage(error, `Failed to create ${name}`));
                      });
                    }
                  }
                } else if (event.key === "Escape") {
                  setNewFolderDialog(null);
                }
              }}
              style={{ "margin-bottom": "12px" }}
            />
            <div style={{ display: "flex", "justify-content": "flex-end", gap: "8px" }}>
              <button class="form-button form-button--secondary" onClick={() => setNewFolderDialog(null)}>
                Cancel
              </button>
              <button
                class="form-button form-button--primary"
                disabled={!newFolderDialog()?.trim()}
                onClick={() => {
                  const name = newFolderDialog()?.trim();
                  if (name) {
                    setNewFolderDialog(null);
                    const activeTerminalId = terminalId();
                    if (activeTerminalId) {
                      clearSftpViewError(activeTerminalId);
                      void createRemoteDirectory(activeTerminalId, joinRemotePath(activeView().currentPath, name)).catch((error) => {
                        setSftpViewError(activeTerminalId, formatErrorMessage(error, `Failed to create ${name}`));
                      });
                    }
                  }
                }}
              >
                Create
              </button>
            </div>
          </div>
        </div>
      </Show>

      <Show when={renameDialog()}>
        <div
          style={{
            position: "fixed",
            inset: "0",
            background: "var(--app-overlay)",
            display: "flex",
            "align-items": "center",
            "justify-content": "center",
            "z-index": 5000,
          }}
          onMouseDown={() => setRenameDialog(null)}
          onContextMenu={(e) => {
            e.preventDefault();
            e.stopPropagation();
            setRenameDialog(null);
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
                <div class="form-panel__eyebrow">NEW NAME</div>
              </div>
              <button class="form-icon-button" onClick={() => setRenameDialog(null)} aria-label="Close rename dialog">x</button>
            </div>
            <input
              ref={renameInputRef}
              class="form-input"
              value={renameDialog()!.name}
              onInput={(event) => setRenameDialog({ entry: renameDialog()!.entry, name: event.currentTarget.value })}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  const dialog = renameDialog();
                  if (dialog && dialog.name.trim()) {
                    setRenameDialog(null);
                    void doRename(dialog.entry, dialog.name.trim());
                  }
                } else if (event.key === "Escape") {
                  setRenameDialog(null);
                }
              }}
              style={{ "margin-bottom": "12px" }}
            />
            <div style={{ display: "flex", "justify-content": "flex-end", gap: "8px" }}>
              <button class="form-button form-button--secondary" onClick={() => setRenameDialog(null)}>
                Cancel
              </button>
              <button
                class="form-button form-button--primary"
                disabled={!renameDialog()!.name.trim()}
                onClick={() => {
                  const dialog = renameDialog();
                  if (dialog && dialog.name.trim()) {
                    setRenameDialog(null);
                    void doRename(dialog.entry, dialog.name.trim());
                  }
                }}
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

function MenuItem(props: { label: string; onClick: () => void; danger?: boolean }) {
  return (
    <div
      onClick={props.onClick}
      style={{
        padding: "6px 16px",
        cursor: "pointer",
        "font-size": "12px",
        color: props.danger ? "var(--app-danger)" : "var(--app-text)",
      }}
      onMouseEnter={(e) => (e.currentTarget.style.background = "var(--app-surface-hover)")}
      onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
    >
      {props.label}
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
