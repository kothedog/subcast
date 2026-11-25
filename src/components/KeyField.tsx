import { useEffect, useState } from "react";
import { Check, Trash2 } from "lucide-react";
import { api } from "../lib/api";
import { t, tr } from "../lib/i18n";
import { useStore } from "../store";

export const NO_AUTO = { autoCapitalize: "off", autoCorrect: "off", spellCheck: false } as const;

/** One default model for translation and the guide; Q&A starts with it and can switch in the chat box. */
export function DefaultButton({ id, isDefault }: { id: string; isDefault: boolean }) {
  const saveSettings = useStore((s) => s.saveSettings);
  const chatProvider = useStore((s) => s.settings.chatProvider);
  const translateProvider = useStore((s) => s.settings.translateProvider);
  const mode = useStore((s) => s.settings.translateMode);
  const split = mode === "google" || chatProvider !== translateProvider;
  const isChat = chatProvider === id;
  const isTrans = translateProvider === id && mode !== "google";
  const isBoth = isChat && isTrans;
  const setAll = () => void saveSettings({ translateProvider: id, chatProvider: id, chatModel: "", modelChosen: true, ...(mode === "google" ? { translateMode: "auto" } : {}) });
  if (isBoth || (!split && isDefault)) {
    return <p className="default-on small"><Check size={13} /> {t("当前默认模型")}</p>;
  }
  if (split && (isChat || isTrans)) {
    return (
      <div className="default-split">
        <p className="default-on small"><Check size={13} /> {isChat ? t("当前用于问答") : t("当前用于翻译")}</p>
        <button className="btn ghost sm" onClick={setAll} title={t("将问答、导读和字幕翻译都切换为这个模型")}>{t("设为全部默认")}</button>
      </div>
    );
  }
  return (
    <button className="btn primary sm" onClick={setAll} title={split ? t("将问答、导读和字幕翻译都切换为这个模型") : undefined}>
      {t("设为默认模型")}
    </button>
  );
}

/** A key field. With `shared` (the AI-model key this service falls back to), a missing own key shows as
 *  already covered by that one instead of as an empty field. */
export function KeyField({ account, placeholder, shared }: { account: string; placeholder?: string; shared?: { account: string; name: string } }) {
  const [key, setKey] = useState("");
  const [has, setHas] = useState(false);
  const [hasShared, setHasShared] = useState(false);
  const [own, setOwn] = useState(false);
  useEffect(() => {
    setKey(""); setOwn(false); setHasShared(false);
    void api.hasApiKey(account).then(setHas);
    if (shared) void api.hasApiKey(shared.account).then(setHasShared);
  }, [account, shared?.account]);
  if (shared && hasShared && !has && !own) {
    return (
      <div className="key-row key-saved">
        <span className="key-state"><Check size={14} /><span>{t("使用「AI 模型」里的 {name} Key", { name: tr(shared.name) })}</span></span>
        <button className="btn sm" onClick={() => setOwn(true)}>{t("单独设置")}</button>
      </div>
    );
  }
  return <KeyRow key={account} has={has} value={key} onValue={setKey} placeholder={placeholder}
    onCancel={own ? () => { setKey(""); setOwn(false); } : undefined}
    onSave={async () => { await api.setApiKey(account, key); setHas(!!key.trim()); setKey(""); }}
    onDelete={async () => { await api.setApiKey(account, ""); setHas(false); }} />;
}

/** A saved key reads as saved (a tinted row with a check and masked dots), not as an empty field;
 *  「更换」 opens the field again. */
export function KeyRow({ has, value, onValue, onSave, onDelete, onCancel, placeholder }: { has: boolean; value: string; onValue: (v: string) => void; onSave: () => Promise<void>; onDelete: () => Promise<void>; onCancel?: () => void; placeholder?: string }) {
  const [editing, setEditing] = useState(false);
  if (has && !editing) {
    return (
      <div className="key-row key-saved" title={t("存于系统钥匙串")}>
        <span className="key-state"><Check size={14} /><span>{t("已保存")}</span><span className="key-dots" aria-hidden>••••••••</span></span>
        <button className="btn sm" onClick={() => setEditing(true)}>{t("更换")}</button>
        <button className="icon-btn" title={t("删除 Key")} aria-label={t("删除 Key")} onClick={() => void onDelete()}><Trash2 size={14} /></button>
      </div>
    );
  }
  return (
    <div className="key-row">
      <input {...NO_AUTO} type="password" autoFocus={editing} value={value} onChange={(e) => onValue(e.target.value)} placeholder={placeholder ?? t("粘贴 API Key")} />
      <button className="btn sm" onClick={async () => { await onSave(); setEditing(false); }} disabled={!value.trim()}>{t("保存")}</button>
      {(has || onCancel) && <button className="btn ghost sm" onClick={() => { onValue(""); setEditing(false); onCancel?.(); }}>{t("取消")}</button>}
    </div>
  );
}
