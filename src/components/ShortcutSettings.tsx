// Settings › 快捷键: every action with its keys. Click a key to record a new one (Esc cancels, ⌫ clears); a
// shortcut another action uses is offered as a swap; system shortcuts (⌘Q, ⌘W, copy/paste…) can't be taken.
import { useEffect, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { Plus, RotateCcw, X } from "lucide-react";
import { ACTIONS, checkCombo, comboOf, keymap, prettyCombo, useKeysHelp, type ActionId } from "../lib/shortcuts";
import { Modal } from "./Chrome";
import { useStore } from "../store";
import { t, useLang } from "../lib/i18n";

export function ShortcutSettings() {
  const { settings, saveSettings } = useStore(useShallow((s) => ({ settings: s.settings, saveSettings: s.saveSettings })));
  useLang((s) => s.lang);
  const map = keymap(settings.shortcuts);
  /** Which action and which of its keys (index; -1 = adding one) is being recorded. */
  const [rec, setRec] = useState<{ id: ActionId; index: number } | null>(null);
  const [note, setNote] = useState<{ id: ActionId; text: string; swap?: { combo: string; from: ActionId } } | null>(null);
  const save = (next: Partial<Record<ActionId, string[]>>) => void saveSettings({ shortcuts: { ...settings.shortcuts, ...next } });
  const setKeys = (id: ActionId, keys: string[]) => save({ [id]: keys });

  useEffect(() => {
    if (!rec) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault(); e.stopPropagation();
      if (e.key === "Escape") { setRec(null); return; }
      const keys = [...map[rec.id]];
      if (e.key === "Backspace" || e.key === "Delete") {
        if (rec.index >= 0) keys.splice(rec.index, 1);
        setKeys(rec.id, keys); setRec(null); return;
      }
      const combo = comboOf(e);
      if (!combo) return;
      const { reserved, takenBy } = checkCombo(combo, rec.id, settings.shortcuts);
      if (reserved) { setNote({ id: rec.id, text: t("{key} 是系统快捷键，不能用", { key: prettyCombo(combo) }) }); setRec(null); return; }
      if (takenBy) { setNote({ id: rec.id, text: t("{key} 已经用于「{action}」", { key: prettyCombo(combo), action: t(ACTIONS.find((a) => a.id === takenBy)!.label) }), swap: { combo, from: takenBy } }); setRec(null); return; }
      if (rec.index >= 0) keys[rec.index] = combo; else if (!keys.includes(combo)) keys.push(combo);
      setKeys(rec.id, keys); setNote(null); setRec(null);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [rec, settings.shortcuts]);

  const swap = (id: ActionId, combo: string, from: ActionId) => {
    save({ [from]: map[from].filter((k) => k !== combo), [id]: [...map[id].filter((k) => k !== combo), combo] });
    setNote(null);
  };
  const changed = (id: ActionId) => settings.shortcuts?.[id] != null && JSON.stringify(settings.shortcuts[id]) !== JSON.stringify(ACTIONS.find((a) => a.id === id)!.keys);
  const groups = [...new Set(ACTIONS.map((a) => a.group))];

  return (
    <div className="shortcuts">
      <div className="shortcuts-head">
        <p className="muted small">{t("点按键可以改成你习惯的组合，按 Esc 取消，按 ⌫ 删除。单个字母只在没有输入文字时生效。")}</p>
        {settings.shortcuts && Object.keys(settings.shortcuts).length > 0 && <button className="btn ghost sm" onClick={() => { void saveSettings({ shortcuts: {} }); setNote(null); }}><RotateCcw size={13} /> {t("全部恢复默认")}</button>}
      </div>
      {groups.map((g) => (
        <section key={g} className="shortcut-group">
          <p className="setup-title">{t(g)}</p>
          {ACTIONS.filter((a) => a.group === g).map((a) => (
            <div key={a.id} className="shortcut-row">
              <span className="shortcut-label">{a.id === "back" || a.id === "forward" ? <SkipLabel id={a.id} /> : t(a.label)}</span>
              <span className="shortcut-keys">
                {map[a.id].map((k, i) => (
                  <button key={k + i} className={`shortcut-recorder ${rec?.id === a.id && rec.index === i ? "recording" : ""}`} onClick={() => setRec({ id: a.id, index: i })} title={t("点击后按下新的组合")}>
                    {rec?.id === a.id && rec.index === i ? t("请按键…") : <kbd>{prettyCombo(k)}</kbd>}
                  </button>
                ))}
                {rec?.id === a.id && rec.index === -1
                  ? <button className="shortcut-recorder recording">{t("请按键…")}</button>
                  : map[a.id].length < 3 && <button className="icon-btn sm" title={t("再加一个")} aria-label={t("再加一个")} onClick={() => setRec({ id: a.id, index: -1 })}><Plus size={13} /></button>}
                {changed(a.id) && <button className="icon-btn sm" title={t("恢复默认")} aria-label={t("恢复默认")} onClick={() => { const next = { ...settings.shortcuts }; delete next[a.id]; void saveSettings({ shortcuts: next }); }}><RotateCcw size={12} /></button>}
              </span>
              {note?.id === a.id && (
                <p className="shortcut-note small">
                  {note.text}
                  {note.swap && <button className="link-btn" onClick={() => swap(a.id, note.swap!.combo, note.swap!.from)}>{t("改给这里用")}</button>}
                  <button className="icon-btn sm" aria-label={t("关闭")} onClick={() => setNote(null)}><X size={12} /></button>
                </p>
              )}
            </div>
          ))}
        </section>
      ))}
    </div>
  );
}

const SKIPS = { back: [5, 10, 15, 30], forward: [10, 15, 30, 60] } as const;

/** 后退 / 前进 with how far, chosen right in the shortcut row. */
function SkipLabel({ id }: { id: "back" | "forward" }) {
  const { settings, saveSettings } = useStore(useShallow((s) => ({ settings: s.settings, saveSettings: s.saveSettings })));
  const key = id === "back" ? "skipBack" : "skipForward";
  const value = settings[key] ?? (id === "back" ? 15 : 30);
  return (
    <span className="skip-label">
      {id === "back" ? t("后退") : t("前进")}
      <select value={value} aria-label={t("秒数")} onChange={(e) => void saveSettings({ [key]: Number(e.target.value) })}>
        {SKIPS[id].map((n) => <option key={n} value={n}>{t("{n} 秒", { n })}</option>)}
      </select>
    </span>
  );
}

/** ⌘/: every shortcut at a glance, read-only. */
export function KeysHelp() {
  const custom = useStore((s) => s.settings.shortcuts);
  const skip = useStore(useShallow((s) => [s.settings.skipBack ?? 15, s.settings.skipForward ?? 30] as const));
  useLang((s) => s.lang);
  const map = keymap(custom);
  const close = () => useKeysHelp.setState({ open: false });
  const label = (a: (typeof ACTIONS)[number]) => a.id === "back" ? t("后退 {n} 秒", { n: skip[0] }) : a.id === "forward" ? t("前进 {n} 秒", { n: skip[1] }) : t(a.label);
  return (
    <Modal title={t("快捷键")} onClose={close}>
      <div className="keys-help">
        {[...new Set(ACTIONS.map((a) => a.group))].map((g) => (
          <section key={g}>
            <p className="setup-title">{t(g)}</p>
            {ACTIONS.filter((a) => a.group === g && map[a.id].length).map((a) => (
              <div key={a.id} className="keys-help-row"><span>{label(a)}</span><span>{map[a.id].map((k) => <kbd key={k}>{prettyCombo(k)}</kbd>)}</span></div>
            ))}
          </section>
        ))}
      </div>
      <div className="row-actions"><span className="spacer" /><button className="btn ghost sm" onClick={() => { close(); useStore.getState().openSettings("shortcuts"); }}>{t("修改快捷键…")}</button></div>
    </Modal>
  );
}
