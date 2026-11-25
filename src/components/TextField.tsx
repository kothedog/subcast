import { forwardRef, useRef, type InputHTMLAttributes, type ReactNode } from "react";
import { X } from "lucide-react";
import { t } from "../lib/i18n";

type Native = Omit<InputHTMLAttributes<HTMLInputElement>, "value" | "onChange" | "size" | "prefix">;

/** The app's text field. The frame (border, fill, focus ring) belongs to the wrapper, so an icon or clear
 *  button sits inside one box and the bare input never draws a second ring of its own. Use this instead of a
 *  plain `<input>` for text, search and filter fields. */
export const TextField = forwardRef<HTMLInputElement, Native & {
  value: string;
  onValue: (v: string) => void;
  /** Leading icon, e.g. `<Search size={14} />`. */
  icon?: ReactNode;
  /** A clear button while there is text; Escape clears too (and stops there instead of closing the dialog). */
  clearable?: boolean;
  /** Something after the text, inside the box: a unit, a count, a small button. */
  suffix?: ReactNode;
  size?: "sm" | "md";
  /** Enter, without Shift. */
  onEnter?: () => void;
  className?: string;
}>(function TextField({ value, onValue, icon, clearable, suffix, size = "md", onEnter, className, onKeyDown, disabled, ...rest }, ref) {
  const own = useRef<HTMLInputElement | null>(null);
  const setRef = (el: HTMLInputElement | null) => {
    own.current = el;
    if (typeof ref === "function") ref(el);
    else if (ref) ref.current = el;
  };
  const clear = () => { onValue(""); own.current?.focus(); };
  return (
    <label className={`tf tf-${size}${value ? " filled" : ""}${disabled ? " disabled" : ""}${className ? ` ${className}` : ""}`}>
      {icon && <span className="tf-icon" aria-hidden>{icon}</span>}
      <input
        ref={setRef}
        className="tf-input"
        value={value}
        disabled={disabled}
        spellCheck={false}
        autoCapitalize="off"
        autoCorrect="off"
        {...rest}
        onChange={(e) => onValue(e.target.value)}
        onKeyDown={(e) => {
          if (clearable && e.key === "Escape" && value) { e.preventDefault(); e.stopPropagation(); onValue(""); return; }
          if (onEnter && e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); onEnter(); return; }
          onKeyDown?.(e);
        }}
      />
      {clearable && value && !disabled && (
        <button type="button" className="tf-clear" onClick={clear} aria-label={t("清除")} tabIndex={-1}><X size={11} strokeWidth={2.5} /></button>
      )}
      {suffix && <span className="tf-suffix">{suffix}</span>}
    </label>
  );
});
