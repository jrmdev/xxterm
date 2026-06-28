import { Accessor, For, Show } from "solid-js";
import {
  portForwards,
  removePortForward,
  PortForward,
} from "../../stores/sessions";
import { smallBtnStyle, statusColor, statusIcon } from "./shared";

interface Props {
  loadingForwards: Accessor<Set<number>>;
  onRefresh: () => void;
  onStartAll: () => void;
  onStopAll: () => void;
  onStartOne: (forward: PortForward) => void;
  onStopOne: (forward: PortForward) => void;
  onEdit: (forward: PortForward) => void;
}

export default function PortForwardList(props: Props) {
  return (
    <div style={{ "margin-bottom": "16px" }}>
      <div style={{ display: "flex", "justify-content": "space-between", "align-items": "center", "margin-bottom": "8px" }}>
        <span style={{ color: "var(--form-muted)", "font-size": "11px", "text-transform": "uppercase", "letter-spacing": "0.14em", "font-weight": "700" }}>
          Configured Forwards
        </span>
        <div style={{ display: "flex", gap: "4px" }}>
          <button onClick={props.onRefresh} style={smallBtnStyle} title="Refresh status">{"\u21BB"}</button>
          <button onClick={props.onStartAll} style={{ ...smallBtnStyle, color: "var(--app-success)" }} title="Start all">{"\u25B6"}</button>
          <button onClick={props.onStopAll} style={{ ...smallBtnStyle, color: "var(--app-danger)" }} title="Stop all">{"\u23F9"}</button>
        </div>
      </div>

      <For each={portForwards.list}>
        {(fwd: PortForward) => <PortForwardRow forward={fwd} {...props} />}
      </For>

      <Show when={portForwards.list.length === 0}>
        <div style={{ color: "var(--app-empty)", "font-size": "12px", "text-align": "center", "padding": "12px" }}>
          No port forwards configured
        </div>
      </Show>
    </div>
  );
}

function PortForwardRow(props: Props & { forward: PortForward }) {
  const fwd = props.forward;
  const status = () => portForwards.statuses.find((s) => s.id === fwd.id);
  const st = () => status()?.status || "stopped";
  const err = () => status()?.error;
  const isRunning = () => st() === "active";
  const isStarting = () => st() === "starting";
  const isPending = () => st() === "pending";
  const isLoading = () => props.loadingForwards().has(fwd.id);

  return (
    <div
      style={{
        display: "flex",
        "align-items": "center",
        gap: "8px",
        padding: "6px 8px",

        "font-size": "12px",
        height: "36px",
      }}
    >
      <span style={{ color: statusColor(st()), "font-size": "14px" }}>{statusIcon(st())}</span>
      <span style={{ color: "var(--app-text)", "min-width": "60px" }}>
        {fwd.forward_type === "dynamic" ? "SOCKS5" : fwd.forward_type}
      </span>
      <span style={{ color: "var(--app-muted)" }}>
        {fwd.forward_type === "local"
          ? `${fwd.local_host || "127.0.0.1"}:${fwd.local_port} \u2192 ${fwd.remote_host || "?"}:${fwd.remote_port || "?"}`
          : fwd.forward_type === "dynamic"
          ? `${fwd.local_host || "127.0.0.1"}:${fwd.local_port} (SOCKS5)`
          : `${fwd.remote_host || "?"}:${fwd.remote_port || "?"} \u2190 ${fwd.local_host || "127.0.0.1"}:${fwd.local_port}`}
      </span>
      <Show when={fwd.always_start}>
        <span style={{ color: "var(--app-muted)", "font-size": "10px", "font-style": "italic" }}>auto</span>
      </Show>
      <div style={{ flex: 1 }} />
      <Show when={err()}>
        <span style={{ color: "var(--app-danger)", "font-size": "10px", "max-width": "120px", overflow: "hidden", "text-overflow": "ellipsis" }}>
          {err()}
        </span>
      </Show>
      <Show when={!isRunning() && !isPending() && !isStarting() && !isLoading()}>
        <button
          onClick={() => props.onStartOne(fwd)}
          style={{ ...smallBtnStyle, color: "var(--app-success)", border: "none" }}
          title="Start"
        >
          {"\u25B6"}
        </button>
      </Show>
      <Show when={isStarting() || isLoading()}>
        <span
          style={{
            color: "var(--app-warning)",
            "font-size": "14px",
            display: "inline-block",
            animation: "pf-spin 1s linear infinite",
          }}
          title={isLoading() ? "Working..." : "Starting..."}
        >
          {"\u25D4"}
        </span>
      </Show>
      <Show when={!isStarting() && !isLoading() && (isRunning() || isPending())}>
        <button
          onClick={() => props.onStopOne(fwd)}
          style={{ ...smallBtnStyle, color: "var(--app-danger)", border: "none" }}
          title="Stop"
        >
          {"\u23F9"}
        </button>
      </Show>
      <button
        onClick={() => props.onEdit(fwd)}
        style={{
          background: "transparent",
          border: "none",
          color: "var(--app-accent)",
          cursor: "pointer",
          "font-size": "14px",
          padding: "0 2px",
        }}
        title="Edit forward"
      >
        {"\u270E"}
      </button>
      <button
        onClick={() => removePortForward(fwd.id)}
        style={{
          background: "transparent",
          border: "none",
          color: "var(--app-danger)",
          cursor: "pointer",
          "font-size": "14px",
          padding: "0 2px",
        }}
        title="Delete forward"
      >
        {"\u2715"}
      </button>
    </div>
  );
}
