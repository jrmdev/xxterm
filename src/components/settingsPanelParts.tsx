import { JSX } from "solid-js";

export function Section(props: { title: string; children: JSX.Element }) {
  return (
    <div class="form-section">
      <h3 class="form-section__title">
        {props.title}
      </h3>
      {props.children}
    </div>
  );
}

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

export function Divider() {
  return <div style={{ height: "1px", background: "var(--app-border-muted)", "margin": "18px 0" }} />;
}

export function buttonStyle(primary: boolean): Record<string, string> {
  return {
    minHeight: "32px",
    padding: "6px 14px",
    "border-radius": "4px",
    border: primary ? "1px solid transparent" : "1px solid var(--app-border-muted)",
    cursor: "pointer",
    "font-size": "12px",
    "font-weight": "600",
    background: primary ? "var(--app-accent)" : "transparent",
    color: primary ? "#fff" : "var(--app-text)",
  };
}

