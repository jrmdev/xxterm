import { JSX } from "solid-js";

export function Field(props: { label: string; children: JSX.Element }) {
  return (
    <div class="form-field">
      <label class="form-label">
        {props.label}
      </label>
      {props.children}
    </div>
  );
}

export const smallBtnStyle: Record<string, string> = {
  background: "transparent",
  border: "1px solid var(--app-border-muted)",
  color: "var(--app-text)",
  cursor: "pointer",
  "font-size": "14px",
  padding: "4px 8px",
  "border-radius": "4px",
};

export function statusColor(status: string) {
  switch (status) {
    case "active": return "var(--app-success)";
    case "starting": return "var(--app-warning)";
    case "pending": return "var(--app-accent)";
    case "failed": return "var(--app-danger)";
    default: return "var(--app-muted)";
  }
}

export function statusIcon(status: string) {
  switch (status) {
    case "active": return "\u25CF";
    case "starting": return "\u25D4";
    case "pending": return "\u25D4";
    case "failed": return "\u2717";
    default: return "\u25CB";
  }
}
