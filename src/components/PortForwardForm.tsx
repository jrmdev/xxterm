import { createSignal, onCleanup, onMount } from "solid-js";
import {
  portForwards,
  closePortForwardPanel,
  createPortForward,
  editPortForward,
  startPortForwards,
  stopPortForwards,
  startSinglePortForward,
  stopSinglePortForward,
  refreshPortForwardStatus,
  PortForward,
  CreatePortForwardInput,
} from "../stores/sessions";
import AddPortForwardForm, { PortForwardFormData } from "./port-forwards/AddPortForwardForm";
import PortForwardList from "./port-forwards/PortForwardList";


const defaultForm = (): PortForwardFormData => ({
  forward_type: "local",
  local_host: "",
  local_port: 8080,
  remote_host: "",
  remote_port: 80,
  always_start: false,
});

export default function PortForwardForm() {
  const [form, setForm] = createSignal<PortForwardFormData>(defaultForm());
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal("");
  const [loadingForwards, setLoadingForwards] = createSignal(new Set<number>());
  const [editingId, setEditingId] = createSignal<number | null>(null);

  const sessionId = () => portForwards.sessionId;

  const handleClose = () => {
    closePortForwardPanel();
  };

  const handler = (e: KeyboardEvent) => {
    if (e.key === "Escape") handleClose();
  };
  document.addEventListener("keydown", handler);
  onCleanup(() => document.removeEventListener("keydown", handler));

  let pollTimer: ReturnType<typeof setInterval> | undefined;

  onMount(() => {
    pollTimer = setInterval(() => {
      const sid = portForwards.sessionId;
      if (sid) {
        refreshPortForwardStatus(sid);
      }
    }, 2000);
  });

  onCleanup(() => {
    if (pollTimer) clearInterval(pollTimer);
  });

  const handleEdit = (fwd: PortForward) => {
    setForm({
      forward_type: fwd.forward_type,
      local_host: fwd.local_host,
      local_port: fwd.local_port,
      remote_host: fwd.remote_host || "",
      remote_port: fwd.remote_port || 0,
      always_start: fwd.always_start,
    });
    setEditingId(fwd.id);
    setError("");
  };

  const handleSubmit = async (e: Event) => {
    e.preventDefault();
    if (!sessionId()) return;
    setSaving(true);
    setError("");
    try {
      const f = form();
      const input: CreatePortForwardInput = {
        session_id: sessionId()!,
        forward_type: f.forward_type,
        local_host: f.local_host,
        local_port: f.local_port,
        remote_host: f.remote_host || null,
        remote_port: f.remote_port || null,
        always_start: f.always_start,
      };
      if (editingId()) {
        await editPortForward(editingId()!, input);
      } else {
        await createPortForward(input);
      }
      setForm(defaultForm());
      setEditingId(null);
    } catch (e: any) {
      setError(e.toString());
    } finally {
      setSaving(false);
    }
  };

  const handleStartAll = async () => {
    if (!sessionId()) return;
    setError("");
    try {
      await startPortForwards(sessionId()!);
    } catch (e: any) {
      setError(e.toString());
    }
  };

  const handleStopAll = async () => {
    if (!sessionId()) return;
    try {
      await stopPortForwards(sessionId()!);
    } catch (e: any) {
      setError(e.toString());
    }
  };

  const handleRefresh = async () => {
    if (!sessionId()) return;
    await refreshPortForwardStatus(sessionId()!);
  };

  const withLoading = async (fwdId: number, fn: () => Promise<void>) => {
    setLoadingForwards(prev => new Set(prev).add(fwdId));
    try {
      await fn();
    } finally {
      setLoadingForwards(prev => {
        const next = new Set(prev);
        next.delete(fwdId);
        return next;
      });
    }
  };

  const handleStartOne = async (fwd: PortForward) => {
    if (!sessionId()) return;
    setError("");
    await withLoading(fwd.id, async () => {
      try {
        await startSinglePortForward(sessionId()!, fwd.id);
      } catch (e: any) {
        setError(e.toString());
      }
    });
  };

  const handleStopOne = async (fwd: PortForward) => {
    if (!sessionId()) return;
    await withLoading(fwd.id, async () => {
      try {
        await stopSinglePortForward(sessionId()!, fwd.id);
      } catch (e: any) {
        setError(e.toString());
      }
    });
  };

  const set = (field: keyof PortForwardFormData, value: string | number | boolean) => {
    setForm((prev) => ({ ...prev, [field]: value }));
  };

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
        "z-index": "2000",
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
        handleClose();
      }}
    >
      <style>{`
        @keyframes pf-spin {
          from { transform: rotate(0deg); }
          to { transform: rotate(360deg); }
        }
      `}</style>
      <div
        style={{
          padding: "16px",
          width: "min(calc(100vw - 32px), 560px)",
          "min-width": 0,
        }}
        class="form-panel form-panel--scrollable"
        onContextMenu={(e) => {
          e.preventDefault();
          e.stopPropagation();
        }}
      >
        <div class="form-panel__header">
          <h2 class="form-panel__title">Port Forwards</h2>
          <button class="form-icon-button" onClick={handleClose} title="Close">x</button>
        </div>

        <PortForwardList
          loadingForwards={loadingForwards}
          onRefresh={handleRefresh}
          onStartAll={handleStartAll}
          onStopAll={handleStopAll}
          onStartOne={handleStartOne}
          onStopOne={handleStopOne}
          onEdit={handleEdit}
        />
        <AddPortForwardForm
          form={form}
          saving={saving}
          error={error}
          editingId={editingId}
          onSubmit={handleSubmit}
          onSet={set}
        />
      </div>
    </div>
  );
}
