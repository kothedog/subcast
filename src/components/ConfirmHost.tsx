import { useEffect, useRef } from "react";
import { answer, useConfirm } from "../lib/confirm";
import { t } from "../lib/i18n";

/** Renders the pending ask() request, if any. Esc cancels, Enter confirms. */
export function ConfirmHost() {
  const req = useConfirm((s) => s.req);
  const ok = useRef<HTMLButtonElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!req) return;
    const prev = document.activeElement as HTMLElement | null;
    (req.danger ? cancel.current : ok.current)?.focus();
    const k = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.stopPropagation(); answer(false); return; }
      if (e.key === "Tab") {
        e.preventDefault();
        const next = document.activeElement === cancel.current ? ok.current : cancel.current;
        next?.focus();
      }
    };
    window.addEventListener("keydown", k, true);
    return () => {
      window.removeEventListener("keydown", k, true);
      prev?.focus?.();
    };
  }, [req]);
  if (!req) return null;
  return (
    <div className="modal-back confirm-back" onMouseDown={(e) => e.target === e.currentTarget && answer(false)}>
      <div className="modal confirm" role="alertdialog" aria-modal="true" aria-labelledby="confirm-title" aria-describedby={req.message ? "confirm-msg" : undefined}>
        <p id="confirm-title" className="confirm-title">{req.title}</p>
        {req.message && <p id="confirm-msg" className="muted confirm-msg">{req.message}</p>}
        <div className="modal-foot">
          <button ref={cancel} className="btn ghost" onClick={() => answer(false)}>{t("取消")}</button>
          <button ref={ok} className={`btn ${req.danger ? "danger-btn" : "primary"}`} onClick={() => answer(true)}>{req.ok}</button>
        </div>
      </div>
    </div>
  );
}
