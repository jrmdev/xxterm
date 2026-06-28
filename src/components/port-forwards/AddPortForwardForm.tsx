import { Accessor, Show } from "solid-js";
import { Field } from "./shared";

export interface PortForwardFormData {
  forward_type: string;
  local_host: string;
  local_port: number;
  remote_host: string;
  remote_port: number;
  always_start: boolean;
}

interface Props {
  form: Accessor<PortForwardFormData>;
  saving: Accessor<boolean>;
  error: Accessor<string>;
  editingId: Accessor<number | null>;
  onSubmit: (event: Event) => void;
  onSet: (field: keyof PortForwardFormData, value: string | number | boolean) => void;
}

export default function AddPortForwardForm(props: Props) {
  const form = props.form;

  return (
    <div style={{ "border-top": "1px solid var(--app-border-muted)", "padding-top": "18px", "margin-top": "18px" }}>
      <h3 class="form-section__title">
        {props.editingId() ? "Update Forward" : "Add Forward"}
      </h3>
      <form onSubmit={props.onSubmit}>
        <Field label="Type">
          <select
            class="form-select"
            value={form().forward_type}
            onChange={(e) => props.onSet("forward_type", e.currentTarget.value)}
          >
            <option value="local">Local (-L)</option>
            <option value="remote">Remote (-R)</option>
            <option value="dynamic">Dynamic (-D / SOCKS5)</option>
          </select>
        </Field>

        <div class="form-row">
          <div style={{ width: "110px", "flex-shrink": 0 }}>
            <Field label={form().forward_type === "remote" ? "Remote Port" : "Local Port"}>
              <input
                class="form-input"
                type="number"
                value={form().local_port}
                onInput={(e) => props.onSet("local_port", parseInt(e.currentTarget.value) || 0)}
                min={1}
                max={65535}
                required
              />
            </Field>
          </div>
          <Show when={form().forward_type !== "dynamic"}>
            <div style={{ flex: 1, "min-width": 0 }}>
              <Field label={form().forward_type === "remote" ? "Local Host" : "Remote Host"}>
                <input
                  class="form-input"
                  type="text"
                  value={form().remote_host}
                  onInput={(e) => props.onSet("remote_host", e.currentTarget.value)}
                  placeholder={form().forward_type === "remote" ? "localhost" : "example.com"}
                />
              </Field>
            </div>
            <div style={{ width: "100px", "flex-shrink": 0 }}>
              <Field label={form().forward_type === "remote" ? "Local Port" : "Remote Port"}>
                <input
                  class="form-input"
                  type="number"
                  value={form().remote_port}
                  onInput={(e) => props.onSet("remote_port", parseInt(e.currentTarget.value) || 0)}
                  min={1}
                  max={65535}
                />
              </Field>
            </div>
          </Show>
        </div>

        <Field label={form().forward_type === "remote" ? "Remote Bind Address" : "Local Host (bind address)"}>
          <input
            class="form-input"
            type="text"
            value={form().local_host}
            onInput={(e) => props.onSet("local_host", e.currentTarget.value)}
            placeholder="127.0.0.1"
          />
        </Field>

        <div class="form-field">
          <label style={{ display: "flex", "align-items": "center", gap: "8px", color: "var(--form-muted)", "font-size": "12px", cursor: "pointer" }}>
            <input
              class="form-checkbox"
              type="checkbox"
              checked={form().always_start}
              onChange={(e) => props.onSet("always_start", e.currentTarget.checked)}
            />
            Auto-start on connect
          </label>
        </div>

        <Show when={props.error()}>
          <div class="form-error" style={{ "margin-bottom": "12px" }}>
            {props.error()}
          </div>
        </Show>

        <div style={{ display: "flex", gap: "8px", "justify-content": "flex-end" }}>
          <button
            type="submit"
            disabled={props.saving()}
            class="form-button form-button--primary"
          >
            {props.saving()
              ? props.editingId() ? "Updating..." : "Adding..."
              : props.editingId() ? "Update Forward" : "Add Forward"}
          </button>
        </div>
      </form>
    </div>
  );
}
