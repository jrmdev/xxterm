import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { open } from "@tauri-apps/plugin-dialog";
import { sep } from "@tauri-apps/api/path";
import {
  cancelActiveSftpQueue,
  clearSftpViewError,
  dismissSftpDelete,
  dismissSftpTransfer,
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

const modifiedDateFormatter = new Intl.DateTimeFormat(undefined, {
  year: "numeric", month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit", hour12: false,
});

function formatModified(value: number | null) {
  if (!value) return "";
  const date = new Date(value * 1000);
  return Number.isNaN(date.getTime()) ? date.toLocaleString() : modifiedDateFormatter.format(date);
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
  return path.split(sep() === "\\" ? /[\\/]/ : "/").filter(Boolean).pop() ?? path;
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

const listingRowHeight = 44;
const listingOverscan = 8;

export default function FileBrowser() {
  let retryTimer: number | undefined;
  let typeaheadResetTimer: number | undefined;
  let listingRef: HTMLDivElement | undefined;
  let scrollRef: HTMLDivElement | undefined;
  const [scrollTop, setScrollTop] = createSignal(0);
  const [viewportHeight, setViewportHeight] = createSignal(0);
  const [busyPath, setBusyPath] = createSignal<string | null>(null);
  const [contextMenu, setContextMenu] = createSignal<{ x: number; y: number; entry: RemoteFileEntry | null } | null>(null);
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
  const clampedScrollTop = createMemo(() => Math.max(0, Math.min(
    scrollTop(), visibleEntries().length * listingRowHeight - viewportHeight(),
  )));
  const firstRow = createMemo(() => Math.max(0, Math.floor(clampedScrollTop() / listingRowHeight) - listingOverscan));
  const endRow = createMemo(() => Math.min(
    visibleEntries().length, Math.ceil((clampedScrollTop() + viewportHeight()) / listingRowHeight) + listingOverscan,
  ));
  const renderedEntries = createMemo(() => visibleEntries().slice(firstRow(), endRow()));
  const selectedPathSet = createMemo(() => new Set(activeView().selectedPaths));
  const selectedEntries = () => {
    const selected = selectedPathSet();
    return activeView().entries.filter((entry) => selected.has(entry.path));
  };
  const contextMenuEntries = createMemo(() => {
    const menu = contextMenu();
    if (!menu) return [];
    const entries = selectedEntries();
    if (!menu.entry) return entries;
    if (entries.some((entry) => entry.path === menu.entry!.path)) {
      return entries;
    }
    return [menu.entry];
  });
  const contextMenuSingleEntry = createMemo(() => (contextMenuEntries().length === 1 ? contextMenuEntries()[0] : null));
  const [pathInput, setPathInput] = createSignal(activeView().currentPath);

  onMount(() => {
    const viewport = scrollRef;
    if (!viewport) return;
    const updateViewport = () => setViewportHeight(viewport.clientHeight);
    updateViewport();
    const observer = new ResizeObserver(updateViewport);
    observer.observe(viewport);
    onCleanup(() => observer.disconnect());
  });

  createEffect(on([terminalId, () => activeView().currentPath], () => {
    if (scrollRef) scrollRef.scrollTop = 0;
    setScrollTop(0);
  }));

  createEffect(on([terminalId, () => activeView().sessionId, () => fileBrowser.activePane], ([activeTerminalId, sessionId, pane]) => {
    if (pane !== "sftp" || !activeTerminalId || !sessionId) return;
    void loadRemoteDirectory(activeTerminalId);
  }));

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
    listingRef?.focus({ preventScroll: true });
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
    const selectedPath = activeView().selectedPaths[0];
    const startIndex = Math.max(0, entries.findIndex((entry) => entry.path === selectedPath));
    let matchIndex = -1;
    let fallbackIndex = -1;
    for (let offset = 0; offset < entries.length; offset++) {
      const index = (startIndex + offset) % entries.length;
      const name = entries[index].name.toLocaleLowerCase();
      if (name.startsWith(nextQuery)) {
        matchIndex = index;
        break;
      }
      if (fallbackIndex === -1 && name.startsWith(singleCharacterQuery)) fallbackIndex = index;
    }

    const index = matchIndex === -1 ? fallbackIndex : matchIndex;
    if (index === -1) {
      resetTypeahead();
      return;
    }

    setTypeaheadQuery(matchIndex === -1 ? singleCharacterQuery : nextQuery);
    queueTypeaheadReset();
    const path = entries[index].path;
    setSelectedPaths([path], path);
    if (scrollRef) {
      const top = index * listingRowHeight;
      const bottom = top + listingRowHeight;
      if (top < scrollRef.scrollTop) scrollRef.scrollTop = top;
      else if (bottom > scrollRef.scrollTop + scrollRef.clientHeight) scrollRef.scrollTop = bottom - scrollRef.clientHeight;
      setScrollTop(scrollRef.scrollTop);
    }
  };

  const handleListingKeyDown = (event: KeyboardEvent) => {
    if (event.ctrlKey || event.metaKey || event.altKey || event.isComposing) return;
    if (contextMenu() || renameDialog() || newFolderDialog() !== null) return;

    if (event.key === "Enter") {
      const entries = selectedEntries();
      const entry = entries[0];
      if (event.repeat || entries.length !== 1 || !entry?.isDir) return;

      event.preventDefault();
      resetTypeahead();
      void openEntry(entry);
      return;
    }

    if (event.key.length !== 1 || /\p{C}/u.test(event.key)) return;
    event.preventDefault();
    selectEntryByTyping(event.key);
  };

  const handleEntryMouseDown = (event: MouseEvent, entry: RemoteFileEntry) => {
    if (event.button !== 0) return;
    event.preventDefault();
    focusListing();
    resetTypeahead();

    const additive = event.ctrlKey || event.metaKey;
    if (!event.shiftKey && !additive) {
      setSelectedPaths([entry.path], entry.path);
      return;
    }

    const visiblePaths = visibleEntries().map((item) => item.path);
    if (event.shiftKey) {
      const anchorPath = activeView().selectionAnchorPath ?? entry.path;
      const nextSelection = new Set(additive ? activeView().selectedPaths : []);
      for (const path of selectionRange(visiblePaths, anchorPath, entry.path)) {
        nextSelection.add(path);
      }
      setSelectedPaths(orderedSelection(visiblePaths, nextSelection), anchorPath);
      return;
    }

    const currentSelection = new Set(activeView().selectedPaths);
    if (!currentSelection.delete(entry.path)) currentSelection.add(entry.path);
    setSelectedPaths(orderedSelection(visiblePaths, currentSelection), entry.path);
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
    const currentPath = activeView().currentPath;

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

    try {
      await uploadLocalFiles(activeTerminalId, paths.map((localPath) => ({
        remotePath: joinRemotePath(currentPath, localBaseName(localPath)),
        localPath,
      })));
    } catch (error) {
      if (isSftpQueueCancelledError(error)) return;
      setSftpViewError(activeTerminalId, formatErrorMessage(error, "Failed to upload file"));
    }
  };

  const activeTransfers = createMemo(() => getTransfersForTerminal(terminalId()));
  const deleteProgress = createMemo(() => getDeleteProgressForTerminal(terminalId()));
  const activityError = createMemo(() => {
    const error = activeView().error;
    if (activeTransfers().some((item) => item.error === error) || deleteProgress().some((item) => item.error === error)) return "";
    return error;
  });
  const dismissViewError = (error: string) => {
    const activeTerminalId = terminalId();
    if (activeTerminalId && activeView().error === error) clearSftpViewError(activeTerminalId);
  };
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

      <div
        ref={scrollRef}
        onScroll={(event) => setScrollTop(event.currentTarget.scrollTop)}
        style={{ flex: 1, "min-height": 0, overflow: "auto", "overflow-anchor": "none" }}
      >
        <div
          ref={listingRef}
          tabindex={0}
          style={{
            "min-height": "100%",
            "box-sizing": "border-box",
            "padding-top": `${firstRow() * listingRowHeight}px`,
            "padding-bottom": `${(visibleEntries().length - endRow()) * listingRowHeight}px`,
            outline: "none",
          }}
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
        <For each={renderedEntries()}>
          {(entry) => (
            <div
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
              style={{
                ...listingRowStyle,
                "box-sizing": "border-box",
                height: `${listingRowHeight}px`,
                "align-items": "center",
                "background-color": selectedPathSet().has(entry.path) ? "var(--app-selection)" : "transparent",
              }}
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

      <Show when={activeTransfers().length > 0 || deleteProgress().length > 0 || activityError()}>
        <div class="sftp-activity">
          <Show when={activityError()}>
            {(error) => <ActivityError message={error()} dismiss={() => dismissViewError(error())} />}
          </Show>
          <For each={activeTransfers()}>
            {(transfer) => {
              const action = transfer.direction === "upload" ? "Upload" : "Download";
              const isCancelled = () => isSftpQueueCancelledError(transfer.error);
              const isCancelling = () => !transfer.done && !!fileBrowser.cancellingQueues[transfer.queueId];
              const percent = () => transfer.total === null
                ? null
                : transfer.total === 0
                  ? 100
                  : Math.min(100, Math.round((transfer.transferred / transfer.total) * 100));
              const progressLabel = () => {
                if (transfer.done && !transfer.path) return "";
                const value = percent();
                if (value !== null) return value + "%";
                return transfer.transferred > 0 ? formatBytes(transfer.transferred) : "";
              };
              const speedLabel = () => {
                if (transfer.done || isCancelling()) return "";
                return formatTransferSpeed(getTransferSpeed(transfer.terminalId, transfer.queueId, transfer.transferId));
              };
              const title = () => {
                if (isCancelled()) return action + " cancelled";
                if (isCancelling()) return "Cancelling " + action.toLowerCase() + "…";
                if (transfer.error) return action + " failed · " + transfer.displayPath;
                if (transfer.done) return transfer.path ? action + " complete · " + transfer.displayPath : transfer.displayPath;
                return action + " " + transfer.displayPath;
              };
              return (
                <div>
                  <div class="sftp-activity__summary" classList={{ "sftp-activity__summary--error": !!transfer.error && !isCancelled() }}>
                    <span title={title()}>{title()}</span>
                    <span>{isCancelled() ? "" : [progressLabel(), speedLabel()].filter(Boolean).join("\u00A0\u2022\u00A0")}</span>
                  </div>
                  <ActivityProgress
                    title={title()} percent={percent()} done={transfer.done} error={!!transfer.error && !isCancelled()}
                    cancelled={isCancelled()} cancelling={isCancelling()}
                    cancelLabel="Cancel transfer queue" cancel={() => void cancelQueue(transfer.queueId)}
                  />
                  <Show when={!isCancelled() && transfer.error}>
                    {(error) => <ActivityError message={error()} dismiss={() => {
                      dismissViewError(error());
                      dismissSftpTransfer(transfer.terminalId, transfer.queueId, transfer.transferId);
                    }} />}
                  </Show>
                </div>
              );
            }}
          </For>
          <For each={deleteProgress()}>
            {(progress) => {
              const isCancelled = () => isSftpQueueCancelledError(progress.error);
              const isCancelling = () => !progress.done && !!fileBrowser.cancellingQueues[progress.queueId];
              const percent = () => progress.total === 0 ? 100 : Math.min(100, Math.round((progress.deleted / progress.total) * 100));
              const title = () => {
                if (isCancelling()) return "Cancelling after current item…";
                if (isCancelled()) return "Delete cancelled";
                if (progress.error) return "Delete failed";
                return progress.done ? "Deleted" : "Deleting…";
              };
              return (
                <div>
                  <div class="sftp-activity__summary" classList={{ "sftp-activity__summary--error": !!progress.error && !isCancelled() }}>
                    <span>{title()}</span>
                    <span>{progress.deleted}/{progress.total}</span>
                  </div>
                  <ActivityProgress
                    title={title()} percent={percent()} done={progress.done} error={!!progress.error && !isCancelled()}
                    cancelled={isCancelled()} cancelling={isCancelling()}
                    cancelLabel="Cancel delete queue" cancel={() => void cancelQueue(progress.queueId)}
                  />
                  <Show when={!isCancelled() && progress.error}>
                    {(error) => <ActivityError message={error()} dismiss={() => {
                      dismissViewError(error());
                      dismissSftpDelete(progress.queueId);
                    }} />}
                  </Show>
                </div>
              );
            }}
          </For>
        </div>
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

function ActivityProgress(props: {
  title: string;
  percent: number | null;
  done: boolean;
  error: boolean;
  cancelled: boolean;
  cancelling: boolean;
  cancelLabel: string;
  cancel: () => void;
}) {
  const indeterminate = () => props.percent === null && !props.done;
  const width = () => props.percent === null
    ? (props.cancelled ? "0%" : props.done && !props.error ? "100%" : "38%")
    : props.percent + "%";
  const color = () => props.error ? "var(--app-danger)" : props.cancelled ? "var(--app-muted)" : "var(--app-accent)";
  return (
    <div class="sftp-activity__progress">
      <div class="sftp-activity__track" role="progressbar" aria-label={props.title} aria-valuenow={props.percent ?? undefined}>
        <div
          class="sftp-progress-fill"
          classList={{ "sftp-progress-fill--indeterminate": indeterminate() }}
          style={{ width: width(), background: color() }}
        />
      </div>
      <Show when={!props.done}>
        <button
          class="sftp-transfer-cancel-btn"
          type="button"
          title={props.cancelLabel}
          aria-label={props.cancelLabel}
          disabled={props.cancelling}
          onClick={props.cancel}
        >
          x
        </button>
      </Show>
    </div>
  );
}

function ActivityError(props: { message: string; dismiss: () => void }) {
  return (
    <div class="sftp-activity-error">
      <span role="alert">{props.message}</span>
      <button type="button" class="sftp-transfer-cancel-btn" aria-label="Dismiss error" onClick={props.dismiss}>×</button>
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
