export const listingRowStyle = {
  display: "grid",
  "grid-template-columns": "24px 1fr auto",
  gap: "8px",
  padding: "6px 8px",
  cursor: "pointer",
  "user-select": "none",
  "background-color": "transparent",
  "border-bottom": "1px solid var(--app-border-muted)",
} as const;

export const listingIconStyle = {
  "font-size": "15px",
  "line-height": "1",
  "align-self": "center",
  "text-align": "center",
} as const;

export const listingMetaStyle = {
  color: "var(--app-muted)",
  "font-size": "10px",
  "font-family": "var(--form-mono)",
  "margin-top": "2px",
  overflow: "hidden",
  "text-overflow": "ellipsis",
  "white-space": "nowrap",
  "min-height": "10px",
} as const;
