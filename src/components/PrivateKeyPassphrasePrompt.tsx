import { createSignal, onCleanup, onMount } from "solid-js";
import { sessions, closeKeyPassphrasePrompt, handleKeyPassphraseSubmit } from "../stores/sessions";

export default function PrivateKeyPassphrasePrompt() {
  const [passphrase, setPassphrase] = createSignal("");
  const [save, setSave] = createSignal(false);
  const [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false);
  let passphraseRef: HTMLInputElement | undefined;

  const handleSubmit = async (event: Event) => {
    event.preventDefault();
    setError("");
    setLoading(true);
    try {
      await handleKeyPassphraseSubmit(passphrase(), save());
    } catch (e: any) {
      setError(e.toString());
    } finally {
      setLoading(false);
    }
  };

  onMount(() => {
    passphraseRef?.focus();
    const handler = (event: KeyboardEvent) => {
      if (event.key === "Escape") closeKeyPassphrasePrompt();
    };
    document.addEventListener("keydown", handler);
    onCleanup(() => document.removeEventListener("keydown", handler));
  });

  return (
    <div
      style={{
        position: "fixed",
        inset: "0",
        background: "var(--app-overlay)",
        display: "flex",
        "align-items": "center",
        "justify-content": "center",
        "z-index": 2500,
      }}
      onMouseDown={() => closeKeyPassphrasePrompt()}
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
        closeKeyPassphrasePrompt();
      }}
    >
      <div
        class="form-panel"
        style={{ width: "min(420px, calc(100vw - 32px))", padding: "16px", "box-shadow": "0 14px 48px rgba(0, 0, 0, 0.42)" }}
        onMouseDown={(event) => event.stopPropagation()}
        onContextMenu={(e) => {
          e.preventDefault();
          e.stopPropagation();
        }}
      >
        <div class="form-panel__header">
          <div>
            <div class="form-panel__eyebrow">PROTECTED PRIVATE KEY</div>
          </div>
          <button class="form-icon-button" type="button" onClick={closeKeyPassphrasePrompt} aria-label="Close passphrase dialog">
            x
          </button>
        </div>
        <div style={{ color: "var(--form-text)", "font-size": "12px", "line-height": "1.5", "margin-bottom": "12px" }}>
          Private key is passphrase-protected:
          <br />
          <code style={{ "font-family": "var(--form-mono)" }}>{sessions.keyPassphrasePrompt?.privateKeyPath}</code>
        </div>
        <form onSubmit={handleSubmit}>
          <input
            ref={passphraseRef}
            class="form-input"
            type="password"
            value={passphrase()}
            onInput={(event) => setPassphrase(event.currentTarget.value)}
            placeholder="Private key passphrase"
            required
            style={{ "margin-bottom": "12px" }}
          />
          <label class="form-checkbox-label" style={{ "margin-bottom": "12px" }}>
            <input
              class="form-checkbox"
              type="checkbox"
              checked={save()}
              onChange={(event) => setSave(event.currentTarget.checked)}
            />
            <span>Save passphrase</span>
          </label>
          {error() && <div class="form-error" style={{ "margin-bottom": "12px" }}>{error()}</div>}
          <div style={{ display: "flex", "justify-content": "flex-end", gap: "8px" }}>
            <button type="button" class="form-button form-button--secondary" onClick={closeKeyPassphrasePrompt}>
              Cancel
            </button>
            <button type="submit" class="form-button form-button--primary" disabled={loading() || !passphrase()}>
              {loading() ? "Checking..." : "Connect"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
