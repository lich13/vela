import { Suspense, lazy, useState } from "react";
import {
  Check,
  ChevronsUpDown,
  Monitor,
  Server,
  Settings2,
} from "lucide-react";
import { command } from "./bridge";
import { setInstanceState, useInstance } from "./instance";
import { errorOf } from "./types";
const InstanceManager = lazy(() => import("./InstanceManager"));

/** The store follows the list again after another window changed it. */
export const reloadInstances = async () =>
  setInstanceState(await command("get_instances"));

/**
 * `select` connects before the store names the new instance; choosing the
 * current one again retries a lost connection.
 */
function useInstanceSelect(confirm?: () => Promise<boolean>) {
  const { state, remote } = useInstance();
  const [pending, setPending] = useState<string | null>(null);
  const [failure, setFailure] = useState("");
  const offline = remote && state.connection.status === "offline";
  const select = async (id: string) => {
    if (pending || (id === state.active && !offline)) return;
    if (id !== state.active && confirm && !(await confirm())) return;
    setPending(id);
    setFailure("");
    try {
      setInstanceState(
        await command("select_instance", {
          id,
          expectedRevision: state.revision,
        }),
      );
    } catch (e) {
      setFailure(errorOf(e).message);
      if (errorOf(e).code === "CONFLICT")
        await reloadInstances().catch(() => {});
    } finally {
      setPending(null);
    }
  };
  const connecting =
    (pending !== null && pending !== "local") ||
    (remote && state.connection.status === "connecting");
  return {
    state,
    remote,
    pending,
    select,
    options: [
      { id: "local", name: "本机" },
      ...state.machines.map(({ id, name }) => ({ id, name })),
    ],
    status: connecting
      ? ("connecting" as const)
      : remote
        ? state.connection.status
        : ("local" as const),
    error: failure || (offline ? (state.connection.error ?? "") : ""),
  };
}
const statusLabel = {
  connecting: "连接中",
  connected: "已连接",
  offline: "离线",
  local: "",
};

/** Sidebar: which Vela the pages show, the connection to it and the machine list. */
export default function InstanceSwitch({
  confirm,
}: {
  /** Asked before another instance replaces the pages and their drafts. */
  confirm: () => Promise<boolean>;
}) {
  const { state, remote, pending, select, options, status, error } =
    useInstanceSelect(confirm);
  const [managing, setManaging] = useState(false);
  const current =
    options.find((o) => o.id === state.active)?.name ?? state.active;
  const closeMenu = (element: HTMLElement) =>
    element.closest("details")?.removeAttribute("open");
  return (
    <div className="instance-switch">
      <details className="account-menu instance-menu">
        <summary aria-label={`当前实例：${current}`}>
          {remote ? <Server size={15} /> : <Monitor size={15} />}
          <span>{current}</span>
          <ChevronsUpDown size={13} />
        </summary>
        <div className="menu-popover">
          {options.map(({ id, name }) => (
            <button
              key={id}
              type="button"
              aria-pressed={id === state.active}
              disabled={pending !== null}
              onClick={(e) => {
                closeMenu(e.currentTarget);
                void select(id);
              }}
            >
              <span>{name}</span>
              {id === state.active && <Check size={13} />}
            </button>
          ))}
          <button
            type="button"
            onClick={(e) => {
              closeMenu(e.currentTarget);
              setManaging(true);
            }}
          >
            <Settings2 size={14} />
            管理实例
          </button>
        </div>
      </details>
      {status !== "local" && (
        <div className="instance-status">
          <span
            className={`status-pill${status === "connected" ? " online" : ""}`}
            role="status"
          >
            <span className="dot" />
            {statusLabel[status]}
          </span>
          {status === "offline" && (
            <button
              type="button"
              className="text-button"
              onClick={() => void select(state.active)}
            >
              重试
            </button>
          )}
        </div>
      )}
      {error && (
        <p className="form-error" role="alert">
          {error}
        </p>
      )}
      {managing && (
        <Suspense fallback={null}>
          <InstanceManager close={() => setManaging(false)} />
        </Suspense>
      )}
    </div>
  );
}

/** Quick panel: the same choice as a row of segments, shown once a server exists. */
export function QuickInstances() {
  const { state, pending, select, options, status, error } =
    useInstanceSelect();
  if (!state.machines.length) return null;
  return (
    <div className="quick-instances">
      <nav className="segmented" aria-label="实例">
        {options.map(({ id, name }) => (
          <button
            key={id}
            type="button"
            disabled={pending !== null}
            aria-pressed={id === state.active}
            onClick={() => void select(id)}
          >
            {name}
          </button>
        ))}
      </nav>
      {status === "connecting" && (
        <span className="quick-instance-status" role="status">
          {statusLabel.connecting}…
        </span>
      )}
      {status === "offline" && (
        <button
          type="button"
          className="text-button"
          onClick={() => void select(state.active)}
        >
          重试
        </button>
      )}
      {(error || status === "offline") && (
        <div className="quick-feedback error" role="alert">
          {status === "offline"
            ? [statusLabel.offline, error].filter(Boolean).join("：")
            : error}
        </div>
      )}
    </div>
  );
}
