import { useShallow } from "zustand/react/shallow";
import { useEffect, useRef, useState } from "react";
import { Import, Link2, Loader2, Search } from "lucide-react";
import { api, errorText, type SearchResult } from "../lib/api";
import { ImportPanel } from "./ImportPanel";
import { useStore } from "../store";
import { t } from "../lib/i18n";
import { scheduleAutoSync } from "../lib/sync";
import { SearchArt } from "./Illustrations";
import { Modal, SearchRow, looksLikeLink } from "./Chrome";
import { sameFeed } from "../lib/picks";

/** Dialog for adding a podcast, YouTube channel, Xiaoyuzhou show, or importing subscriptions. */
export function AddDialog() {
  const { set, reloadShows, go, openEpisode, notify, shows } = useStore(useShallow((s) => ({ set: s.set, reloadShows: s.reloadShows, go: s.go, openEpisode: s.openEpisode, notify: s.notify, shows: s.shows })));
  const [tab, setTab] = useState<"search" | "link" | "import">(useStore.getState().addTab ?? "search");
  const [q, setQ] = useState("");
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [searching, setSearching] = useState(false);
  const [results, setResults] = useState<SearchResult[] | null>(null);
  const [adding, setAdding] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const timer = useRef<number | undefined>(undefined);
  const close = () => set({ addOpen: false });
  // Split around the placeholder so "RSS" can stay bold wherever the translation puts it.
  const rss = t("任意播客 {rss} 地址", { rss: "\u0000" }).split("\u0000");
  const add = async (value: string) => {
    setError(null); setAdding(value); setBusy(true);
    try {
      const r = await api.addSubscription(value);
      close();
      if (r.show) { await reloadShows(); scheduleAutoSync(); notify(t("已订阅「{name}」", { name: r.show.title })); go({ kind: "show", id: r.show.id }); }
      else if (r.episode) openEpisode(r.episode);
    } catch (err) { setError(errorText(err)); } finally { setBusy(false); setAdding(null); }
  };
  // A link pasted into the search box is simply added.
  const pastedLink = looksLikeLink(q);
  useEffect(() => {
    window.clearTimeout(timer.current);
    if (!q.trim() || pastedLink) { setResults(null); return; }
    timer.current = window.setTimeout(async () => {
      setSearching(true);
      try { setResults((await api.searchPodcasts(q.trim(), /[\u4e00-\u9fff]/.test(q) ? "CN" : "US")).slice(0, 12)); } catch { setResults([]); } finally { setSearching(false); }
    }, 300);
  }, [q, pastedLink]);
  const head = (
    <div className="add-tabs" role="tablist">
      <button role="tab" aria-selected={tab === "search"} className={tab === "search" ? "on" : ""} onClick={() => { setTab("search"); setError(null); }}><Search size={15} /> {t("搜索节目")}</button>
      <button role="tab" aria-selected={tab === "link"} className={tab === "link" ? "on" : ""} onClick={() => { setTab("link"); setError(null); }}><Link2 size={15} /> {t("链接或 RSS")}</button>
      <button role="tab" aria-selected={tab === "import"} className={tab === "import" ? "on" : ""} onClick={() => { setTab("import"); setError(null); }}><Import size={15} /> {t("导入订阅")}</button>
    </div>
  );
  return (
    <Modal title={t("添加节目")} onClose={close} head={head}>
      {tab === "import" ? <ImportPanel onDone={close} /> : tab === "search" ? (
        <form className="add-form" onSubmit={(e) => { e.preventDefault(); if (pastedLink) void add(q.trim()); else if (results?.[0] && !shows.some((s) => s.feedUrl === results[0].feedUrl)) void add(results[0].feedUrl); }}>
          <div className="add-search">
            {pastedLink ? <Link2 size={16} /> : <Search size={16} />}
            <input autoFocus autoCapitalize="off" autoCorrect="off" spellCheck={false} value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("输入节目名，比如 Lex Fridman、张小珺")} />
            {(searching || busy) && <Loader2 size={14} className="spin muted" />}
          </div>
          {error && <p className="error-text">{error}</p>}
          <div className="add-results">
            {pastedLink ? (
              <div className="add-empty"><p className="muted small">{t("这是一个链接，回车直接添加")}</p><button type="submit" className="btn primary sm" disabled={busy}>{t("添加")}</button></div>
            ) : !q.trim() ? (
              <div className="add-empty"><p className="muted small">{t("搜索 YouTube 频道和 Apple Podcasts 上的节目，中英文名都可以。小宇宙请用「链接或 RSS」。")}</p></div>
            ) : results && !results.length ? (
              <div className="add-empty"><SearchArt /><p className="muted small">{t("没有找到，换个关键词，或者用「链接或 RSS」添加")}</p></div>
            ) : results?.map((r) => (
              <SearchRow key={r.feedUrl} r={r} done={shows.some((s) => sameFeed(s.feedUrl, r.feedUrl))} adding={adding} onOpen={() => { close(); go({ kind: "channel", input: r.feedUrl }); }} onSubscribe={() => void add(r.feedUrl)} />
            ))}
          </div>
        </form>
      ) : (
        <form onSubmit={(e) => { e.preventDefault(); if (input.trim()) void add(input.trim()); }} className="add-form">
          <input autoFocus autoCapitalize="off" autoCorrect="off" spellCheck={false} value={input} onChange={(e) => setInput(e.target.value)} placeholder={t("粘贴 YouTube / Apple Podcasts / 小宇宙 / RSS 链接")} />
          {error && <p className="error-text">{error}</p>}
          <ul className="hints muted small">
            <li><b>YouTube</b> {t("频道（@名字 / 频道链接 / 播放列表），或单个视频链接")}</li>
            <li><b>Apple Podcasts</b> {t("节目链接")}</li>
            <li><b>{t("小宇宙")}</b> {t("节目或单集链接")}</li>
            <li>{rss[0]}<b>RSS</b>{rss[1]}</li>
          </ul>
          <div className="modal-foot">
            <div className="spacer" />
            <button type="button" className="btn ghost" onClick={close}>{t("取消")}</button>
            <button type="submit" className="btn primary" disabled={busy || !input.trim()}>{busy ? <><Loader2 size={14} className="spin" /> {t("正在读取…")}</> : t("添加")}</button>
          </div>
        </form>
      )}
    </Modal>
  );
}
