import { useEffect, useRef, type ComponentProps, type ReactNode } from "react";
import { X } from "lucide-react";

/**
 * A modal dialog docked to the right edge. Escape or a click on the backdrop
 * closes it, and focus goes back to the opener once it is gone.
 */
export function DrawerShell({
  onClose,
  ...rest
}: { onClose: () => void } & ComponentProps<"dialog">) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const focus = document.activeElement as HTMLElement | null;
    const dialog = ref.current;
    dialog?.showModal();
    return () => {
      dialog?.close();
      queueMicrotask(() => {
        if (!dialog?.isConnected && focus?.isConnected)
          focus.focus({ preventScroll: true });
      });
    };
  }, []);
  return (
    <dialog
      {...rest}
      ref={ref}
      onCancel={(e) => {
        e.preventDefault();
        onClose();
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget) {
          const r = e.currentTarget.getBoundingClientRect();
          if (e.clientX < r.left) onClose();
        }
      }}
    />
  );
}

export function Drawer({
  title,
  onClose,
  children,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
}) {
  return (
    <DrawerShell className="usage-drawer" aria-label={title} onClose={onClose}>
      <header>
        <h2>{title}</h2>
        <button className="icon-button" aria-label="关闭详情" onClick={onClose}>
          <X size={18} />
        </button>
      </header>
      <div className="usage-drawer-body">{children}</div>
    </DrawerShell>
  );
}
