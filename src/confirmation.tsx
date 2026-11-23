import { createRoot } from "react-dom/client";
import Modal from "./Modal";

let pending = false;

// WKWebView does not reliably present window.confirm. Use the same accessible
// dialog in the native app and browser, and reject competing navigation events.
function choose<T>(
  message: string,
  dismissed: T,
  /** The last choice is the primary one. */
  choices: [label: string, value: T][],
): Promise<T> {
  if (pending) return Promise.resolve(dismissed);
  pending = true;
  return new Promise((resolve) => {
    const container = document.createElement("div");
    container.dataset.confirmation = "true";
    document.body.appendChild(container);
    const root = createRoot(container);
    let settled = false;
    const finish = (choice: T) => {
      if (settled) return;
      settled = true;
      queueMicrotask(() => {
        root.unmount();
        container.remove();
        pending = false;
        resolve(choice);
      });
    };
    root.render(
      <Modal title="确认操作" compact close={() => finish(dismissed)}>
        <p className="confirmation-message">{message}</p>
        <div className="modal-actions">
          <button type="button" onClick={() => finish(dismissed)}>
            取消
          </button>
          {choices.map(([label, value], i) => (
            <button
              key={label}
              className={i === choices.length - 1 ? "primary" : undefined}
              type="button"
              onClick={() => finish(value)}
            >
              {label}
            </button>
          ))}
        </div>
      </Modal>,
    );
  });
}

export function confirmAction(
  message: string,
  label = "放弃修改",
): Promise<boolean> {
  return choose(message, false, [[label, true]]);
}

/** Leaving unsaved edits: keep editing, drop them, or save them first. */
export function confirmSave(
  message: string,
): Promise<"save" | "discard" | "cancel"> {
  return choose<"save" | "discard" | "cancel">(message, "cancel", [
    ["放弃修改", "discard"],
    ["保存", "save"],
  ]);
}
