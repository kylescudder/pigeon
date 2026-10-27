import { useEffect, useRef, type ReactNode } from "react";

export function Modal({
  open,
  onDismiss,
  title,
  children,
  className = "",
  dismissible = true,
}: {
  open: boolean;
  onDismiss: () => void;
  title: string;
  children: ReactNode;
  className?: string;
  dismissible?: boolean;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current;
    if (open && !dialog?.open) dialog?.showModal();
    else if (!open && dialog?.open) dialog.close();
  }, [open]);
  return (
    <dialog
      ref={ref}
      className={`modal ${className}`}
      aria-label={title}
      onCancel={(event) => {
        if (!dismissible) event.preventDefault();
        else onDismiss();
      }}
      onClick={(event) => {
        if (dismissible && event.target === event.currentTarget) onDismiss();
      }}
    >
      <div className="modal-content">{children}</div>
    </dialog>
  );
}
