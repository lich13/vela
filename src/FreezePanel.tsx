import { useState } from "react";
import { CalendarClock, Pause, Play, Snowflake, X } from "lucide-react";
import { useGatewayFreeze } from "./QuickFreeze";
import { clientName } from "./ClientSelection";
import type { ClientFreeze } from "./bridge-contracts";
import { errorOf } from "./types";
import "./styles/freeze.css";

const stages = [
  ["receiving", "接收中"],
  ["waiting", "等待发送"],
  ["headers", "等待响应"],
  ["streaming", "传输中"],
  ["finishing", "收尾中"],
  ["queued", "暂存"],
] as const;
function localTime(at: number) {
  const date = new Date(at);
  const part = (n: number) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${part(date.getMonth() + 1)}-${part(date.getDate())}T${part(date.getHours())}:${part(date.getMinutes())}`;
}
type Controller = ReturnType<typeof useGatewayFreeze>;
function Platform({
  state,
  control,
}: {
  state: ClientFreeze;
  control: Controller;
}) {
  const [editing, setEditing] = useState(false);
  const [time, setTime] = useState("");
  const [error, setError] = useState("");
  const draining = state.frozen && state.draining > 0;
  const ready = state.frozen && !draining;
  const label = !state.frozen ? "未冻结" : draining ? "等待收尾" : "已冻结";
  const run = async (action: () => Promise<void>) => {
    setError("");
    try {
      await action();
    } catch (e) {
      setError(errorOf(e).message);
    }
  };
  const openTime = () => {
    setTime(state.resumeAt ? localTime(state.resumeAt) : "");
    setEditing(true);
    setError("");
  };
  return (
    <section
      className={`freeze-platform freeze-platform-${ready ? "ice" : draining ? "yellow" : "green"}`}
      aria-label={clientName(state.clientId)}
    >
      <div className="freeze-platform-heading">
        <h2>{clientName(state.clientId)}</h2>
        <span className="freeze-status">
          <i aria-hidden="true" />
          {label}
        </span>
      </div>
      <div className="freeze-counts">
        {stages.map(([key, label]) => (
          <div key={key}>
            <span>{label}</span>
            <strong>{state[key]}</strong>
          </div>
        ))}
      </div>
      <div className="freeze-platform-actions">
        <button
          className="secondary"
          disabled={control.busy || draining}
          onClick={() =>
            void run(async () => {
              await control.setFrozen(!state.frozen, state.clientId);
              setEditing(false);
            })
          }
        >
          {state.frozen ? <Play size={16} /> : <Pause size={16} />}
          {state.frozen ? "解冻" : "冻结"}
        </button>
        <button
          className="secondary"
          disabled={control.busy || !ready}
          onClick={openTime}
        >
          <CalendarClock size={16} />
          {state.resumeAt ? "修改时间" : "定时恢复"}
        </button>
      </div>
      {state.resumeAt !== null && (
        <div className="freeze-schedule">
          <span>恢复时间</span>
          <time dateTime={new Date(state.resumeAt).toISOString()}>
            {localTime(state.resumeAt).replace("T", " ")}
          </time>
          <button
            className="icon-button"
            aria-label={`取消 ${clientName(state.clientId)} 定时恢复`}
            disabled={control.busy}
            onClick={() =>
              void run(async () => {
                await control.setResume(state.clientId, null);
                setEditing(false);
              })
            }
          >
            <X size={16} />
          </button>
        </div>
      )}
      {editing && ready && (
        <form
          className="freeze-time-form"
          onSubmit={(event) => {
            event.preventDefault();
            const at = new Date(time).getTime();
            if (!Number.isFinite(at) || at <= Date.now()) {
              setError("请选择晚于当前时间的恢复时间");
              return;
            }
            void run(async () => {
              await control.setResume(state.clientId, at);
              setEditing(false);
            });
          }}
        >
          <label htmlFor={`resume-${state.clientId}`}>恢复时间</label>
          <input
            id={`resume-${state.clientId}`}
            type="datetime-local"
            step="60"
            value={time}
            onChange={(e) => setTime(e.target.value)}
            required
            autoFocus
          />
          <div>
            <button
              type="button"
              className="secondary"
              disabled={control.busy}
              onClick={() => {
                setEditing(false);
                setError("");
              }}
            >
              取消
            </button>
            <button className="primary" disabled={control.busy}>
              保存
            </button>
          </div>
        </form>
      )}
      {error && (
        <div className="freeze-error" role="alert">
          {error}
        </div>
      )}
    </section>
  );
}
export default function FreezePanel() {
  const control = useGatewayFreeze("main");
  return (
    <section className="freeze-page">
      <div className="page-heading">
        <h1>冻结</h1>
        <Snowflake size={22} aria-hidden="true" />
      </div>
      {control.error && (
        <div className="freeze-error" role="alert">
          {control.error}
          <button className="secondary" onClick={control.retry}>
            重试
          </button>
        </div>
      )}
      <div className="freeze-platforms">
        {control.state?.clients.map((state) => (
          <Platform key={state.clientId} state={state} control={control} />
        ))}
      </div>
    </section>
  );
}
