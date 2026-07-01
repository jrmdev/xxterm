import { createSignal, onMount, onCleanup } from "solid-js";
import { sessions, closeMasterPasswordPrompt, handleMasterPasswordSubmit } from "../stores/sessions";


export default function MasterPasswordPrompt() {
  const [password, setPassword] = createSignal("");
  const [confirmPassword, setConfirmPassword] = createSignal("");
  const [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false);
  let passwordRef: HTMLInputElement | undefined;

  const mode = sessions.masterPasswordMode;
  const needsSetup = mode === "set";

  const handleSubmit = async (e: Event) => {
    e.preventDefault();
    setError("");
    setLoading(true);
    try {
      if (needsSetup) {
        if (password().length < 4) {
          setError("Master password must be at least 4 characters");
          setLoading(false);
          return;
        }
        if (password() !== confirmPassword()) {
          setError("Passwords do not match");
          setLoading(false);
          return;
        }
      }

      await handleMasterPasswordSubmit(password());
    } catch (e: any) {
      setError(e.toString());
    } finally {
      setLoading(false);
    }
  };

  const handleCancel = () => {
    closeMasterPasswordPrompt();
  };

  onMount(() => {
    passwordRef?.focus();
    const handler = (e: KeyboardEvent) => { if (e.key === "Escape") closeMasterPasswordPrompt(); };
    document.addEventListener("keydown", handler);
    onCleanup(() => document.removeEventListener("keydown", handler));
  });

  return (
    <div
      style={{
        position: "fixed",
        top: 0,
        left: 0,
        right: 0,
        bottom: 0,
        background: "var(--app-overlay)",
        display: "flex",
        "align-items": "center",
        "justify-content": "center",
        "z-index": "3000",
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
        handleCancel();
      }}
    >
      <div
        style={{
          padding: "16px",
          width: "min(calc(100vw - 32px), 420px)",
          "min-width": 0,
        }}
        class="form-panel"
        onContextMenu={(e) => {
          e.preventDefault();
          e.stopPropagation();
        }}
      >
        <div class="form-panel__header">
          <h2 class="form-panel__title">
              {needsSetup ? "Set Master Password" : "Enter Master Password"}
            </h2>
          <button class="form-icon-button" type="button" onClick={handleCancel} title="Close">x</button>
        </div>
        <p class="form-help" style={{ "margin-bottom": "16px" }}>
          {needsSetup
            ? "A master password is required to securely store session passwords and private key passphrases."
            : "Your master password is needed to access stored session passwords and private key passphrases."}
        </p>

        <form onSubmit={handleSubmit}>
          <div style={{ "margin-bottom": "12px" }}>
          <input
            class="form-input"
            type="password"
            value={password()}
            onInput={(e) => setPassword(e.currentTarget.value)}
            placeholder="Enter master password"
            required
            ref={passwordRef}
          />
          </div>

          {needsSetup && (
            <input
              class="form-input"
              type="password"
              value={confirmPassword()}
              onInput={(e) => setConfirmPassword(e.currentTarget.value)}
              placeholder="Confirm master password"
              required
            />
          )}

          {error() && (
            <div class="form-error" style={{ "margin-bottom": "12px" }}>
              {error()}
            </div>
          )}

          <div style={{ display: "flex", gap: "8px", "justify-content": "flex-end" , "margin-top": "20px"}}>
            <button
              type="button"
              onClick={handleCancel}
              class="form-button form-button--secondary"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={loading()}
              class="form-button form-button--primary"
            >
              {loading() ? "Processing..." : needsSetup ? "Set Password" : "Unlock"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
