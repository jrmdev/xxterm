import { createEffect, createSignal, onMount, onCleanup, Show } from "solid-js";
import { Terminal as XTermTerminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import type { WebglAddon } from "@xterm/addon-webgl";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { createPty, writePty, closePty, resizePty, connectSsh, readClipboard, type KeyPassphraseInput } from "../ipc/commands";
import { autoStartPortForwards, autoStopPortForwards } from "../stores/sessions";
import { removeTerminal, setTerminalConnectionState, setTerminalUnseenActivity, setTerminalExited } from "../stores/terminals";
import { clearSftpView, setSidebarPane, syncSidebarWithActiveTerminal } from "../stores/fileBrowser";
import { settings } from "../stores/settings";
import type { TerminalProfile } from "../stores/terminalProfiles";
import type { TerminalExitEvent } from "../types/terminal";
import { getColorScheme } from "../colorSchemes";
import "@xterm/xterm/css/xterm.css";

const terminalTextEncoder = new TextEncoder();
const clipboardTextDecoder = new TextDecoder();
const byteArray = Uint8Array as typeof Uint8Array & { fromBase64?: (value: string) => Uint8Array };

function toBase64(str: string): string {
  const bytes = terminalTextEncoder.encode(str) as Uint8Array & { toBase64?: () => string };
  if (bytes.toBase64) return bytes.toBase64();
  let binary = "";
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  return btoa(binary);
}

function binaryStringToBase64(str: string): string {
  return btoa(str);
}

function isZoomInShortcut(event: KeyboardEvent) {
  return (event.ctrlKey || event.metaKey) && (event.key === "+" || event.key === "=" || event.code === "NumpadAdd");
}

function isZoomOutShortcut(event: KeyboardEvent) {
  return (event.ctrlKey || event.metaKey) && (event.key === "-" || event.code === "NumpadSubtract");
}

function isWindowsPlatform() {
  return (navigator as any).userAgentData?.platform === "Windows" || navigator.userAgent.includes("Windows");
}

function base64ToBytes(b64: string): Uint8Array {
  if (byteArray.fromBase64) return byteArray.fromBase64(b64);
  const binaryStr = atob(b64);
  const bytes = new Uint8Array(binaryStr.length);
  for (let i = 0; i < binaryStr.length; i++) {
    bytes[i] = binaryStr.charCodeAt(i);
  }
  return bytes;
}

function decodeBase64Utf8(b64: string): string | null {
  try {
    return clipboardTextDecoder.decode(base64ToBytes(b64));
  } catch {
    return null;
  }
}

function brightenHex(hex: string, factor: number): string {
  if (!hex || hex.length < 7) return hex || "#4a4a4a";
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  const mixR = Math.min(255, Math.round(r + (255 - r) * factor));
  const mixG = Math.min(255, Math.round(g + (255 - g) * factor));
  const mixB = Math.min(255, Math.round(b + (255 - b) * factor));
  return `#${mixR.toString(16).padStart(2, "0")}${mixG.toString(16).padStart(2, "0")}${mixB.toString(16).padStart(2, "0")}`;
}

interface Props {
  id: string;
  label: string;
  type?: "local" | "ssh";
  sessionId?: number;
  profile?: TerminalProfile;
  keyPassphrases?: KeyPassphraseInput[];
  active: boolean;
}

export default function TerminalTab(props: Props) {
  let terminalRef: HTMLDivElement | undefined;
  let term: XTermTerminal | undefined;
  let fitAddon: FitAddon | undefined;
  let webglAddon: WebglAddon | undefined;
  let webglLoading = false;
  let webglUnavailable = false;
  let unlistenOutput: (() => void) | undefined;
  let unlistenExit: (() => void) | undefined;
  let resizeObserver: ResizeObserver | undefined;
  let layoutFrame: number | undefined;
  let focusRequested = false;
  let atlasRefreshRequested = false;
  let copySelectionFrame: number | undefined;
  let flashSelectionTimer: ReturnType<typeof setTimeout> | undefined;
  let selectionMouseDownHandler: ((event: MouseEvent) => void) | undefined;
  let selectionMouseUpHandler: (() => void) | undefined;
  let middleClickPasteGuard: ((event: MouseEvent) => void) | undefined;
  let nativePasteGuard: ((event: ClipboardEvent) => void) | undefined;
  let pendingSelection = "";
  let mouseSelectionStarted = false;
  let suppressNativeMousePasteUntil = 0;
  let middleClickPastedOnMouseDown = false;
  let suppressUnseenActivityUntil = 0;
  let sessionReady = false;
  let disposed = false;
  let exited = false;
  let restarting = false;
  let unseenActivity = false;
  const windowsPlatform = isWindowsPlatform();
  const selectionFlashDurationMs = 100;
  const resizeActivitySuppressionMs = 400;
  const [contextMenu, setContextMenu] = createSignal<{ x: number; y: number } | null>(null);

  const closeContextMenu = () => setContextMenu(null);

  const focusTerminal = () => {
    focusRequested = true;
    scheduleTerminalLayout();
  };

  const getTerminalSize = () => ({
    cols: Math.max(1, term?.cols || 80),
    rows: Math.max(1, term?.rows || 24),
  });

  const handleUnexpectedExit = (event?: TerminalExitEvent) => {
    exited = true;
    sessionReady = false;
    setTerminalExited(props.id, true);
    if (props.type === "ssh") {
      setTerminalConnectionState(props.id, "disconnected");
      syncSidebarWithActiveTerminal(true);
    }
    term?.write(`\r\n\x1b[31m${event?.message || "[Process exited]"}\x1b[0m\r\n`
      + "\r\n\x1b[31m      - Press \"r\" to restart\x1b[0m"
      + "\r\n\x1b[31m      - Press \"q\" to close\x1b[0m\r\n");
    if (!props.active) {
      unseenActivity = true;
      setTerminalUnseenActivity(props.id, true);
    }
  };

  createEffect(() => {
    if (contextMenu()) {
      const handler = (e: MouseEvent) => {
        const target = e.target as HTMLElement;
        if (!target.closest(".context-menu")) {
          closeContextMenu();
        }
      };
      document.addEventListener("click", handler);
      onCleanup(() => document.removeEventListener("click", handler));
    }
  });

  const copyToClipboard = (text: string, clearSelection = false) => {
    if (!text) return;
    navigator.clipboard
      .writeText(text)
      .then(() => {
        if (!clearSelection || !term) return;

        if (flashSelectionTimer !== undefined) {
          clearTimeout(flashSelectionTimer);
          flashSelectionTimer = undefined;
        }

        const originalSelectionBg = term.options.theme?.selectionBackground;
        const flashColor = brightenHex(originalSelectionBg || "#31343a", 0.5);
        const colorScheme = getColorScheme(settings.colorSchemeId);

        term.options.theme = {
          ...colorScheme.xterm,
          selectionBackground: flashColor,
        };

        flashSelectionTimer = setTimeout(() => {
          flashSelectionTimer = undefined;
          term!.options.theme = colorScheme.xterm;
          term!.clearSelection();
        }, selectionFlashDurationMs);
      })
      .catch(() => {});
  };

  const pasteFromClipboard = async () => {
    const currentTerm = term;
    if (!currentTerm) return;
    try {
      const text = await readClipboard();
      if (text && term === currentTerm) {
        currentTerm.focus();
        currentTerm.paste(text);
      }
    } catch {
    }
  };

  const handleOsc52 = (data: string): boolean => {
    const idx = data.indexOf(";");
    if (idx === -1) return false;

    const target = data.substring(0, idx);
    const payload = data.substring(idx + 1);
    if (!payload || payload === "?") return true;
    if (target && !target.includes("c")) return false;

    const text = decodeBase64Utf8(payload);
    if (text === null) return false;

    copyToClipboard(text);
    return true;
  };

  const handleOsc52Sequences = (data: string): boolean => {
    const osc52Prefix = "\x1b]52;";
    let handled = false;
    let start = 0;

    while ((start = data.indexOf(osc52Prefix, start)) !== -1) {
      const payloadStart = start + osc52Prefix.length;
      const bellEnd = data.indexOf("\x07", payloadStart);
      const stEnd = data.indexOf("\x1b\\", payloadStart);

      let payloadEnd = -1;
      if (bellEnd === -1) {
        payloadEnd = stEnd;
      } else if (stEnd === -1) {
        payloadEnd = bellEnd;
      } else {
        payloadEnd = Math.min(bellEnd, stEnd);
      }

      if (payloadEnd === -1) break;
      handled = handleOsc52(data.substring(payloadStart, payloadEnd)) || handled;
      start = payloadEnd + 1;
    }

    return handled;
  };

  const copyAndClearSelection = () => {
    if (copySelectionFrame !== undefined) {
      cancelAnimationFrame(copySelectionFrame);
    }

    copySelectionFrame = requestAnimationFrame(() => {
      copySelectionFrame = undefined;
      const selection = term?.getSelection() || pendingSelection;
      pendingSelection = "";
      if (selection) {
        copyToClipboard(selection, true);
      }
    });
  };

  const fitTerminal = () => {
    if (!props.active) return;
    try {
      fitAddon?.fit();
    } catch {
    }
  };

  const scheduleTerminalLayout = () => {
    if (!props.active || !term || layoutFrame !== undefined) return;
    layoutFrame = requestAnimationFrame(() => {
      layoutFrame = undefined;
      if (!props.active || !term) return;
      fitTerminal();
      if (atlasRefreshRequested) {
        atlasRefreshRequested = false;
        term.clearTextureAtlas();
      }
      if (focusRequested) {
        focusRequested = false;
        term.refresh(0, term.rows - 1);
        term.focus();
        void loadWebglRenderer();
      }
    });
  };

  const loadWebglRenderer = async () => {
    if (!props.active || !term || webglAddon || webglLoading || webglUnavailable) return;
    webglLoading = true;

    try {
      const { WebglAddon } = await import("@xterm/addon-webgl");
      if (!props.active || !term) return;
      const addon = new WebglAddon();
      addon.onContextLoss(() => {
        if (webglAddon !== addon) return;
        addon.dispose();
        webglAddon = undefined;
        // Keep hidden tabs on the fallback renderer until they are next shown.
        void loadWebglRenderer();
      });
      term.loadAddon(addon);
      webglAddon = addon;
      scheduleTerminalLayout();
    } catch (e) {
      webglUnavailable = true;
      console.warn("WebGL addon failed to load:", e);
    } finally {
      webglLoading = false;
    }
  };

  onMount(() => {
    if (!terminalRef) return;

    term = new XTermTerminal({
      cursorBlink: true,
      cursorStyle: "block",
      fontSize: settings.fontSize,
      fontFamily: settings.fontFamily,
      theme: getColorScheme(settings.colorSchemeId).xterm,
      scrollback: settings.scrollback,
      allowProposedApi: true,
      convertEol: false,
      customGlyphs: true,
      rescaleOverlappingGlyphs: true,
      linkHandler: {
        activate: (event: MouseEvent, text: string) => {
          const url = text.startsWith("http://") || text.startsWith("https://")
            ? text
            : null;
          if (url) {
            event.preventDefault();
            openUrl(url);
          }
        },
      },
    });

    fitAddon = new FitAddon();
    term.loadAddon(fitAddon);

    term.open(terminalRef);
    if (props.active) focusTerminal();

    term.attachCustomKeyEventHandler((event) => {
      if (event.type !== "keydown") return true;

      if (isZoomInShortcut(event)) {
        event.preventDefault();
        event.stopPropagation();
        zoomIn();
        return false;
      }

      if (isZoomOutShortcut(event)) {
        event.preventDefault();
        event.stopPropagation();
        zoomOut();
        return false;
      }

      if (
        (event.ctrlKey || event.metaKey) &&
        !event.altKey &&
        !event.shiftKey &&
        event.key.toLowerCase() === "v"
      ) {
        // Leave the browser paste event uncancelled so xterm's paste handler owns
        // Ctrl+V/Cmd+V. Returning false prevents xterm from sending raw ^V.
        return false;
      }

      if ((event.ctrlKey || event.metaKey) && !["Control", "Meta", "Alt", "Shift"].includes(event.key)) {
        event.preventDefault();
      }

      return true;
    });

    term.onSelectionChange(() => {
      pendingSelection = term?.getSelection() || "";
    });

    selectionMouseDownHandler = (event: MouseEvent) => {
      if (event.button === 0) {
        mouseSelectionStarted = true;
      }
    };
    selectionMouseUpHandler = () => {
      if (!mouseSelectionStarted) return;
      mouseSelectionStarted = false;
      copyAndClearSelection();
    };
    middleClickPasteGuard = (event: MouseEvent) => {
      if (event.button !== 1) return;
      const mouseMode = term?.modes?.mouseTrackingMode;
      if (mouseMode && mouseMode !== "none") {
        if (event.type === "mousedown") {
          middleClickPastedOnMouseDown = false;
        }
        // In mouse mode the terminal app receives button 2; suppress the browser's
        // paste path so tmux does not paste once from each path.
        suppressNativeMousePasteUntil = Date.now() + 750;
        if (event.type === "mousedown") return;
        event.preventDefault();
        event.stopImmediatePropagation();
        return;
      }

      if (!windowsPlatform) {
        middleClickPastedOnMouseDown = false;
        return;
      }

      suppressNativeMousePasteUntil = Date.now() + 750;
      event.preventDefault();
      event.stopImmediatePropagation();
      if (event.type === "mousedown") {
        middleClickPastedOnMouseDown = true;
        void pasteFromClipboard();
        return;
      }
      if (!middleClickPastedOnMouseDown) {
        void pasteFromClipboard();
      }
      middleClickPastedOnMouseDown = false;
    };
    nativePasteGuard = (event: ClipboardEvent) => {
      if (Date.now() > suppressNativeMousePasteUntil) return;
      event.preventDefault();
      event.stopImmediatePropagation();
    };
    terminalRef.addEventListener("mousedown", selectionMouseDownHandler);
    terminalRef.addEventListener("mousedown", middleClickPasteGuard, true);
    terminalRef.addEventListener("auxclick", middleClickPasteGuard, true);
    terminalRef.addEventListener("paste", nativePasteGuard, true);
    document.addEventListener("mouseup", selectionMouseUpHandler);

    term.parser.registerOscHandler(52, handleOsc52);

    term.parser.registerDcsHandler({ final: "t" }, (data: string) => {
      if (!data.startsWith("mux;")) return false;
      const passthrough = data.substring(4).replace(/\x1b\x1b/g, "\x1b");
      return handleOsc52Sequences(passthrough);
    });

    term.onResize(({ cols, rows }) => {
      if (!sessionReady) return;
      suppressUnseenActivityUntil = Date.now() + resizeActivitySuppressionMs;
      void resizePty(props.id, cols, rows);
    });

    term.onData((data) => {
      if (exited) {
        if (data === "r" || data === "R") {
          restart();
        } else if (data === "q" || data === "Q") {
          removeTerminal(props.id);
        }
        return;
      }
      writePty(props.id, toBase64(data));
    });

    term.onBinary((data) => {
      writePty(props.id, binaryStringToBase64(data));
    });

    const handleOutput = (event: { payload: string }) => {
      if (!term) return;
      try {
        term.write(base64ToBytes(event.payload));
      } catch {
      }
      if (!props.active && !unseenActivity && Date.now() > suppressUnseenActivityUntil) {
        unseenActivity = true;
        setTerminalUnseenActivity(props.id, true);
      }
    };

    const handleExitEvent = (event: { payload?: TerminalExitEvent }) => {
      if (event.payload?.initiatedByUser) return;
      handleUnexpectedExit(event.payload);
    };

    async function startSession(failureMessage: string): Promise<boolean> {
      try {
        fitTerminal();
        const { cols, rows } = getTerminalSize();
        if (props.type === "ssh" && props.sessionId) {
          setTerminalConnectionState(props.id, "connecting");
          setSidebarPane("sftp");
          await connectSsh(props.id, props.sessionId, cols, rows, props.keyPassphrases);
        } else {
          await createPty(props.id, cols, rows, props.profile);
        }
        if (disposed) {
          await closePty(props.id);
          return false;
        }
        if (exited) return false;

        sessionReady = true;
        const latest = getTerminalSize();
        if (latest.cols !== cols || latest.rows !== rows) {
          void resizePty(props.id, latest.cols, latest.rows);
        }
        if (props.type === "ssh" && props.sessionId) {
          setTerminalConnectionState(props.id, "connected");
          void autoStartPortForwards(props.sessionId, props.id, props.keyPassphrases);
        }
        return true;
      } catch (e) {
        if (disposed) return false;
        if (props.type === "ssh") {
          setTerminalConnectionState(props.id, "disconnected");
          syncSidebarWithActiveTerminal(true);
        }
        term?.write(`\r\n\x1b[31m${failureMessage}: ${e}\x1b[0m`);
        return false;
      }
    }

    async function restart() {
      if (restarting) return;
      restarting = true;
      try {
        unlistenExit?.();
        unlistenExit = undefined;
        await closePty(props.id);
        if (disposed) return;
        term?.write("\r\n\x1b[32mRestarting...\x1b[0m\r\n");
        await subscribeToExit();
        if (disposed) return;
        exited = false;
        if (await startSession("Failed to restart")) {
          setTerminalExited(props.id, false);
          term?.write("\x1b[2J\x1b[H");
        } else {
          exited = true;
        }
      } catch (error) {
        term?.write(`\r\n\x1b[31mFailed to restart: ${error}\x1b[0m`);
      } finally {
        restarting = false;
      }
    }

    const subscribeToExit = async () => {
      const unlisten = await listen<TerminalExitEvent>(`pty-exit-${props.id}`, handleExitEvent);
      if (disposed) unlisten();
      else unlistenExit = unlisten;
    };

    void Promise.all([
      listen<string>(`pty-output-${props.id}`, handleOutput).then((unlisten) => {
        if (disposed) unlisten();
        else unlistenOutput = unlisten;
      }),
      subscribeToExit(),
    ]).then(() => {
      if (!disposed) return startSession("Failed to start terminal");
    }).catch((error) => {
      term?.write(`\r\n\x1b[31mFailed to listen for terminal events: ${error}\x1b[0m`);
    });

    resizeObserver = new ResizeObserver(scheduleTerminalLayout);
    resizeObserver.observe(terminalRef);

    const handleWindowFocus = (event: Event) => {
      if (!props.active || document.hidden) return;
      if (event.type === "visibilitychange") atlasRefreshRequested = true;
      focusTerminal();
    };
    window.addEventListener("focus", handleWindowFocus);
    document.addEventListener("visibilitychange", handleWindowFocus);
    onCleanup(() => {
      window.removeEventListener("focus", handleWindowFocus);
      document.removeEventListener("visibilitychange", handleWindowFocus);
    });
  });

  createEffect(() => {
    const colorScheme = getColorScheme(settings.colorSchemeId);
    if (!term) return;
    term.options.theme = colorScheme.xterm;
  });

  createEffect(() => {
    const fontFamily = settings.fontFamily;
    const fontSize = settings.fontSize;
    if (!term) return;
    term.options.fontFamily = fontFamily;
    term.options.fontSize = fontSize;
    scheduleTerminalLayout();
  });

  createEffect(() => {
    if (props.active) {
      unseenActivity = false;
      focusTerminal();
    }
  });

  onCleanup(() => {
    disposed = true;
    resizeObserver?.disconnect();
    if (layoutFrame !== undefined) {
      cancelAnimationFrame(layoutFrame);
    }
    if (copySelectionFrame !== undefined) {
      cancelAnimationFrame(copySelectionFrame);
    }
    if (flashSelectionTimer !== undefined) {
      clearTimeout(flashSelectionTimer);
    }
    if (terminalRef && selectionMouseDownHandler) {
      terminalRef.removeEventListener("mousedown", selectionMouseDownHandler);
    }
    if (terminalRef && middleClickPasteGuard) {
      terminalRef.removeEventListener("mousedown", middleClickPasteGuard, true);
      terminalRef.removeEventListener("auxclick", middleClickPasteGuard, true);
    }
    if (terminalRef && nativePasteGuard) {
      terminalRef.removeEventListener("paste", nativePasteGuard, true);
    }
    if (selectionMouseUpHandler) {
      document.removeEventListener("mouseup", selectionMouseUpHandler);
    }
    unlistenOutput?.();
    unlistenExit?.();
    if (props.type === "ssh") {
      setTerminalConnectionState(props.id, "disconnected");
      clearSftpView(props.id);
    }
    closePty(props.id);
    if (props.type === "ssh" && props.sessionId) {
      void autoStopPortForwards(props.sessionId, props.id);
    }
    syncSidebarWithActiveTerminal(true);
    term?.dispose();
    term = undefined;
  });

  const zoomIn = () => {
    if (!term) return;
    term.options.fontSize = Math.min(40, (term.options.fontSize || 14) + 1);
    scheduleTerminalLayout();
    closeContextMenu();
  };

  const zoomOut = () => {
    if (!term) return;
    term.options.fontSize = Math.max(8, (term.options.fontSize || 14) - 1);
    scheduleTerminalLayout();
    closeContextMenu();
  };

  const closeTerminal = () => {
    closeContextMenu();
    removeTerminal(props.id);
  };

  return (
    <>
      <div
        ref={terminalRef}
        onContextMenu={(e) => {
          e.preventDefault();
          e.stopPropagation();
          if (!e.shiftKey) return;
          setContextMenu({ x: e.clientX, y: e.clientY });
        }}
        style={{ width: "100%", height: "100%", background: "var(--terminal-background)" }}
      />
      <Show when={contextMenu() !== null}>
        <div
          class="context-menu"
          style={{
            position: "fixed",
            left: `${contextMenu()!.x}px`,
            top: `${contextMenu()!.y}px`,
            background: "var(--app-surface-alt)",
            border: "1px solid var(--app-border)",
            "border-radius": "4px",
            padding: "4px 0",
            "z-index": "5000",
            "min-width": "140px",
            "box-shadow": "0 4px 12px rgba(0,0,0,0.3)",
          }}
          onClick={closeContextMenu}
        >
          <div
            onClick={zoomIn}
            style={contextMenuItemStyle}
            onMouseEnter={(e) => (e.currentTarget.style.background = "var(--app-surface-hover)")}
            onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
          >
            Zoom In
          </div>
          <div
            onClick={zoomOut}
            style={contextMenuItemStyle}
            onMouseEnter={(e) => (e.currentTarget.style.background = "var(--app-surface-hover)")}
            onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
          >
            Zoom Out
          </div>
          <div style={{ height: "1px", background: "var(--app-border)", margin: "4px 0" }} />
          <div
            onClick={closeTerminal}
            style={{ ...contextMenuItemStyle, color: "var(--app-danger)" }}
            onMouseEnter={(e) => (e.currentTarget.style.background = "var(--app-surface-hover)")}
            onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
          >
            Close
          </div>
        </div>
      </Show>
    </>
  );
}

const contextMenuItemStyle: Record<string, string> = {
  padding: "6px 16px",
  cursor: "pointer",
  "font-size": "12px",
  color: "var(--app-text)",
};
