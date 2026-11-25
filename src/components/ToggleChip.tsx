import type { ReactNode } from "react";
import { Check } from "lucide-react";

/** An on/off option shown as a pill: a tint and a check when on. For short rows of independent switches
 *  ("show the title", "show the source") where native checkboxes look like a form. */
export function ToggleChip({ on, onChange, children, title }: { on: boolean; onChange: (on: boolean) => void; children: ReactNode; title?: string }) {
  return (
    <button type="button" role="switch" aria-checked={on} title={title} className={`toggle-chip${on ? " on" : ""}`} onClick={() => onChange(!on)}>
      <span className="toggle-chip-mark" aria-hidden>{on && <Check size={11} strokeWidth={3} />}</span>
      {children}
    </button>
  );
}
