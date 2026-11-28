// 管理订阅: every followed show and channel in one list, with a filter, a type switch and checkboxes, so
// someone following hundreds of channels can unsubscribe from many at once. Unsubscribing keeps generated
// subtitles, 精读 and progress, the same as the single unsubscribe on a show page.
import { useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { Check, Loader2, Pencil, Search, X } from "lucide-react";
import { TextField } from "./TextField";
import { api, type Show } from "../lib/api";
import { showAliasPatch } from "../lib/settings";
import { useStore } from "../store";
import { isChineseUi, t } from "../lib/i18n";
import { ask } from "../lib/confirm";
import { scheduleAutoSync } from "../lib/sync";
import { useNameMatch, useShowName } from "../lib/zh";
import { Modal } from "./Chrome";
import { showIcon } from "./Lists";

type Kind = "all" | "podcast" | "youtube";
const kindOf = (s: Show): Exclude<Kind, "all"> => (s.kind === "youtube" ? "youtube" : "podcast");
const kindLabel = (s: Show) => (s.kind === "youtube" ? t("YouTube 频道") : s.kind === "xiaoyuzhou" ? t("小宇宙") : s.kind === "qiaomu" ? t("乔木精读") : t("播客"));

export function ManageSubs() {
  const { shows, set, showAliases, translateNames, saveSettings } = useStore(useShallow((s) => ({ shows: s.shows, set: s.set, showAliases: s.settings.showAliases, translateNames: s.settings.translateShowNames, saveSettings: s.saveSettings })));
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState<Kind>("all");
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const matches = useNameMatch();
  const close = () => { if (!progress) set({ manageOpen: false }); };

  const mixed = shows.some((s) => kindOf(s) === "youtube") && shows.some((s) => kindOf(s) === "podcast");
  const shown = shows.filter((s) => (kind === "all" || kindOf(s) === kind) && matches(query, s.title, s.author, showAliases?.[s.id]));
  // Selecting acts on the visible rows; a pick hidden by the filter stays picked (and counted).
  const allShown = shown.length > 0 && shown.every((s) => picked.has(s.id));
  const filtered = !!query.trim() || kind !== "all";
  const toggleShown = () => {
    const n = new Set(picked);
    shown.forEach((s) => (allShown ? n.delete(s.id) : n.add(s.id)));
    setPicked(n);
  };
  const toggle = (id: string, on: boolean) => { const n = new Set(picked); on ? n.add(id) : n.delete(id); setPicked(n); };

  const remove = async () => {
    const ids = shows.filter((s) => picked.has(s.id)).map((s) => s.id);
    if (!ids.length) return;
    const one = ids.length === 1 ? shows.find((s) => s.id === ids[0]) : undefined;
    const ok = await ask({
      title: one ? t("取消订阅「{name}」？", { name: one.title }) : t("取消订阅这 {n} 个？", { n: ids.length }),
      message: t("已生成的字幕、精读和收听进度会保留。"),
      ok: t("取消订阅"),
      danger: true,
    });
    if (!ok) return;
    let done = 0, failed = 0;
    setProgress({ done, total: ids.length });
    for (const id of ids) {
      try { await api.removeShow(id); } catch { failed++; }
      done++; setProgress({ done, total: ids.length });
    }
    const st = useStore.getState();
    await st.reloadShows();
    if (done - failed > 0) scheduleAutoSync();
    if (st.view.kind === "show" && ids.includes(st.view.id)) st.go({ kind: "latest" });
    st.notify(failed ? t("已取消订阅 {n} 个，{f} 个没有成功", { n: done - failed, f: failed }) : t("已取消订阅 {n} 个", { n: done }));
    setProgress(null);
    setPicked(new Set());
  };

  return (
    <Modal title={t("管理订阅")} onClose={close}>
      <div className="manage">
        <div className="import-list-head">
          <p className="small muted">{t("共 {n} 个订阅", { n: shows.length })}</p>
          <span className="pick-side">
            {picked.size > 0 && <span className="small pick-count">{t("已选 {n} 个", { n: picked.size })}</span>}
            <button className="link-btn small" onClick={toggleShown} disabled={!shown.length || progress != null}>{allShown ? (filtered ? t("取消选择这些") : t("全不选")) : filtered ? t("选中这 {n} 个", { n: shown.length }) : t("全选")}</button>
          </span>
        </div>
        <div className="pick-tools">
          <TextField size="sm" value={query} onValue={setQuery} icon={<Search size={14} />} clearable autoFocus placeholder={t("按名字筛选")} aria-label={t("按名字筛选")} />
          {mixed && (
            <div className="seg-control" role="tablist" aria-label={t("类型")}>
              {([["all", t("全部")], ["podcast", t("播客")], ["youtube", "YouTube"]] as [Kind, string][]).map(([k, label]) => (
                <button key={k} role="tab" aria-selected={kind === k} className={kind === k ? "on" : ""} onClick={() => setKind(k)}>{label}</button>
              ))}
            </div>
          )}
        </div>
        <div className="import-list manage-list">
          {shown.map((s) => <ManageRow key={s.id} show={s} on={picked.has(s.id)} disabled={progress != null} onChange={(on) => toggle(s.id, on)} />)}
          {!shown.length && <p className="muted small list-empty">{shows.length ? t("没有名字里带「{q}」的", { q: query.trim() }) : t("还没有订阅")}</p>}
        </div>
        <div className="modal-foot">
          {progress
            ? <span className="small muted"><Loader2 size={13} className="spin" /> {t("正在取消订阅 {done} / {total}", { done: progress.done, total: progress.total })}</span>
            : isChineseUi() && <NameToggle on={!translateNames} onChange={(keep) => void saveSettings({ translateShowNames: !keep })} />}
          <div className="spacer" />
          <button className="btn ghost" onClick={() => { set({ manageOpen: false }); useStore.getState().openSettings("sync"); }} disabled={progress != null}>{t("跨设备同步…")}</button>
          <button className="btn ghost" onClick={close} disabled={progress != null}>{t("关闭")}</button>
          <button className="btn danger-btn" onClick={() => void remove()} disabled={!picked.size || progress != null}>{picked.size ? t("取消订阅 {n} 个", { n: picked.size }) : t("取消订阅")}</button>
        </div>
      </div>
    </Modal>
  );
}

function ManageRow({ show, on, disabled, onChange }: { show: Show; on: boolean; disabled: boolean; onChange: (on: boolean) => void }) {
  const { zh, orig, aliased } = useShowName(show.title, show.id);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const startEdit = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setDraft(aliased ? (zh ?? orig) : "");
    setEditing(true);
  };
  const commit = () => {
    const st = useStore.getState();
    void st.saveSettings(showAliasPatch(st.settings, show.id, draft));
    setEditing(false);
  };
  return (
    <label className="import-row manage-row">
      <input type="checkbox" checked={on} disabled={disabled} onChange={(e) => onChange(e.target.checked)} />
      <span className="manage-cover">{showIcon(show)}</span>
      {editing ? (
        <span className="manage-rename" onClick={(e) => e.preventDefault()}>
          <TextField
            size="sm"
            value={draft}
            onValue={setDraft}
            autoFocus
            placeholder={orig}
            aria-label={t("自定义节目名")}
            onEnter={commit}
            onKeyDown={(e) => { if (e.key === "Escape") { e.stopPropagation(); setEditing(false); } }}
          />
          <button type="button" className="icon-btn sm" title={t("保存")} aria-label={t("保存")} onClick={commit}><Check size={13} /></button>
          <button type="button" className="icon-btn sm" title={t("取消")} aria-label={t("取消")} onClick={() => setEditing(false)}><X size={13} /></button>
        </span>
      ) : (
        <>
          <span className="manage-names">
            <span className="clamp1">{zh ?? orig}</span>
            <span className="manage-meta clamp1">{kindLabel(show)}{zh ? ` · ${orig}` : show.author && show.author !== show.title ? ` · ${show.author}` : ""}</span>
          </span>
          <button type="button" className="icon-btn sm manage-edit" disabled={disabled} title={t("重命名")} aria-label={t("重命名")} onClick={startEdit}><Pencil size={13} /></button>
        </>
      )}
    </label>
  );
}

/** Same switch as Settings › 通用: foreign show names stay as they are instead of turning into Chinese. */
export function NameToggle({ on, onChange }: { on: boolean; onChange: (on: boolean) => void }) {
  return (
    <label className="check small" title={t("侧栏和订阅列表保留原名，如 Lenny's Podcast。自己改过的名字不受影响。")}>
      <input type="checkbox" checked={on} onChange={(e) => onChange(e.target.checked)} /> {t("外文节目名不翻译")}
    </label>
  );
}
