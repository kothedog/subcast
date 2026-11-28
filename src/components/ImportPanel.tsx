// 导入订阅: from YouTube (the browser you're signed in with), from Xiaoyuzhou and other podcast apps (an OPML
// file they export), or from backups (Google Takeout CSV, NewPipe, FreeTube). One list to review, then a single
// "订阅" for everything picked; shows already subscribed are skipped. Nothing starts checked: an account can
// follow hundreds of channels, so the list has a filter and the user picks what they want.
import { useEffect, useRef, useState } from "react";
import { Check, FileUp, Loader2, Search, Youtube } from "lucide-react";
import { TextField } from "./TextField";
import { invoke } from "@tauri-apps/api/core";
import { api, errorText, needsBrowserAccess, rawErrorText } from "../lib/api";
import { BrowserAccess, browserName } from "./BrowserAccess";
import { useStore } from "../store";
import { t, IS_MAC } from "../lib/i18n";
import { scheduleAutoSync } from "../lib/sync";
import { sameFeed } from "../lib/picks";
import { useNameMatch } from "../lib/zh";

export interface ImportItem { input: string; title: string; thumb?: string; source: "youtube" | "opml" | "csv" | "newpipe" | "freetube" }

const ytChannel = (id: string) => `https://www.youtube.com/channel/${id}`;

/** Every subscription a file holds, whatever app wrote it. */
export function parseImport(name: string, text: string): ImportItem[] {
  const out: ImportItem[] = [];
  const push = (x: ImportItem) => { if (x.input && !out.some((o) => o.input === x.input)) out.push(x); };
  const trimmed = text.trim();
  if (trimmed.startsWith("<")) {
    // OPML: Xiaoyuzhou, Pocket Casts, Overcast, Apple Podcasts (macOS), Invidious…
    const doc = new DOMParser().parseFromString(trimmed, "text/xml");
    doc.querySelectorAll("outline[xmlUrl], outline[xmlurl]").forEach((o) => {
      const url = o.getAttribute("xmlUrl") ?? o.getAttribute("xmlurl") ?? "";
      const id = url.match(/channel_id=(UC[\w-]{22})/)?.[1];
      push({ input: id ? ytChannel(id) : url, title: o.getAttribute("title") || o.getAttribute("text") || url, source: "opml" });
    });
    return out;
  }
  if (trimmed.startsWith("{") || trimmed.startsWith("[")) {
    try {
      const v = JSON.parse(trimmed);
      // NewPipe: { subscriptions: [{ service_id, url, name }] }
      for (const s of v?.subscriptions ?? []) if (typeof s?.url === "string") push({ input: s.url, title: s.name ?? s.url, source: "newpipe" });
      // FreeTube profiles: [{ subscriptions: [{ id, name, thumbnail }] }]; Takeout JSON: [{ snippet: { resourceId: { channelId }, title } }]
      for (const p of Array.isArray(v) ? v : v?.profiles ?? []) {
        for (const s of p?.subscriptions ?? []) if (typeof s?.id === "string") push({ input: ytChannel(s.id), title: s.name ?? s.id, thumb: s.thumbnail, source: "freetube" });
        const c = p?.snippet?.resourceId?.channelId;
        if (c) push({ input: ytChannel(c), title: p.snippet.title ?? c, source: "csv" });
      }
    } catch { /* not JSON after all */ }
    return out;
  }
  // FreeTube .db: one JSON profile per line.
  if (/\.db$/i.test(name)) {
    for (const line of trimmed.split("\n")) { try { for (const s of JSON.parse(line)?.subscriptions ?? []) if (s?.id) push({ input: ytChannel(s.id), title: s.name ?? s.id, source: "freetube" }); } catch { /* skip */ } }
    if (out.length) return out;
  }
  // Google Takeout subscriptions.csv: Channel Id,Channel Url,Channel Title
  for (const line of trimmed.split(/\r?\n/)) {
    const id = line.match(/^(UC[\w-]{22})\s*,/)?.[1];
    if (id) { const cols = line.split(","); push({ input: ytChannel(id), title: cols.slice(2).join(",").trim() || id, source: "csv" }); }
  }
  return out;
}

type Browser = "chrome" | "safari" | "edge" | "firefox" | "brave";
const BROWSERS: [Browser, string][] = IS_MAC
  ? [["chrome", "Chrome"], ["safari", "Safari"], ["edge", "Edge"], ["firefox", "Firefox"], ["brave", "Brave"]]
  : [["chrome", "Chrome"], ["edge", "Edge"], ["firefox", "Firefox"], ["brave", "Brave"]];

export function ImportPanel({ onDone }: { onDone?: () => void }) {
  const shows = useStore((s) => s.shows);
  const [items, setItems] = useState<ImportItem[] | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState<Browser | "file" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [denied, setDenied] = useState(false);
  const [progress, setProgress] = useState<{ done: number; total: number; failed: number } | null>(null);
  const [browser, setBrowser] = useState<Browser>("chrome");
  const [query, setQuery] = useState("");
  const matches = useNameMatch();
  const fileInput = useRef<HTMLInputElement>(null);
  const has = (input: string) => shows.some((s) => sameFeed(s.feedUrl, input) || (input.includes("/channel/") && s.feedUrl.includes(input.split("/channel/")[1].slice(2))));

  const offer = (list: ImportItem[]) => {
    setItems(list);
    setPicked(new Set());
    setQuery("");
    if (!list.length) setError(t("文件里没有找到订阅"));
  };
  const fromBrowser = async () => {
    setBusy(browser); setError(null); setDenied(false); setItems(null);
    try {
      const ch = await invoke<{ id: string; title: string; thumbnail: string }[]>("youtube_subscriptions", { browser });
      offer(ch.map((c) => ({ input: ytChannel(c.id), title: c.title, thumb: c.thumbnail, source: "youtube" })));
    } catch (e) { if (needsBrowserAccess(rawErrorText(e))) setDenied(true); else setError(errorText(e)); } finally { setBusy(null); }
  };
  const fromFile = async (f: File) => {
    setBusy("file"); setError(null); setItems(null);
    try { offer(parseImport(f.name, await f.text())); } catch (e) { setError(errorText(e)); } finally { setBusy(null); }
  };
  const go = async () => {
    const list = (items ?? []).filter((x) => picked.has(x.input));
    let done = 0, failed = 0;
    setProgress({ done, total: list.length, failed });
    const queue = [...list];
    await Promise.all(Array.from({ length: 3 }, async () => {
      for (let x = queue.shift(); x; x = queue.shift()) {
        try { await api.addSubscription(x.input); } catch { failed++; }
        done++; setProgress({ done, total: list.length, failed });
      }
    }));
    await useStore.getState().reloadShows();
    if (done - failed > 0) scheduleAutoSync();
    useStore.getState().notify(failed ? t("已导入 {n} 个，{f} 个没有成功", { n: done - failed, f: failed }) : t("已导入 {n} 个订阅", { n: done }));
    setProgress(null); setItems(null);
    onDone?.();
  };
  useEffect(() => { setError(null); setDenied(false); }, [browser]);

  if (items?.length) {
    const fresh = items.filter((x) => !has(x.input));
    const shown = items.filter((x) => matches(query, x.title));
    // 全选 / 全不选 act on the rows the filter leaves visible; picks hidden by the filter stay picked.
    const shownFresh = shown.filter((x) => !has(x.input));
    const allShown = shownFresh.length > 0 && shownFresh.every((x) => picked.has(x.input));
    const toggleShown = () => {
      const n = new Set(picked);
      shownFresh.forEach((x) => (allShown ? n.delete(x.input) : n.add(x.input)));
      setPicked(n);
    };
    return (
      <div className="import">
        <div className="import-list-head">
          <p className="small muted">{t("找到 {n} 个，其中 {m} 个还没订阅", { n: items.length, m: fresh.length })}</p>
          <span className="pick-side">
            {picked.size > 0 && <span className="small pick-count">{t("已选 {n} 个", { n: picked.size })}</span>}
            <button className="link-btn small" onClick={toggleShown} disabled={!shownFresh.length || progress != null}>{allShown ? (query ? t("取消选择这些") : t("全不选")) : query ? t("选中这 {n} 个", { n: shownFresh.length }) : t("全选")}</button>
          </span>
        </div>
        {items.length > 6 && (
          <div className="pick-tools">
            <TextField size="sm" value={query} onValue={setQuery} icon={<Search size={14} />} clearable autoFocus placeholder={t("按名字筛选")} aria-label={t("按名字筛选")} />
          </div>
        )}
        <div className="import-list">
          {shown.map((x) => {
            const done = has(x.input);
            return (
              <label key={x.input} className={`import-row ${done ? "done" : ""}`}>
                <input type="checkbox" disabled={done || progress != null} checked={done || picked.has(x.input)} onChange={(e) => { const n = new Set(picked); e.target.checked ? n.add(x.input) : n.delete(x.input); setPicked(n); }} />
                {x.thumb ? <img src={x.thumb} alt="" loading="lazy" /> : <span className="import-ph">{x.title.slice(0, 1)}</span>}
                <span className="clamp1">{x.title}</span>
                {done && <span className="sub-done"><Check size={12} /> {t("已订阅")}</span>}
              </label>
            );
          })}
          {!shown.length && <p className="muted small list-empty">{t("没有名字里带「{q}」的", { q: query.trim() })}</p>}
        </div>
        <div className="modal-foot">
          {progress ? <span className="small muted"><Loader2 size={13} className="spin" /> {t("正在订阅 {done} / {total}", { done: progress.done, total: progress.total })}</span>
            : !picked.size && <span className="small muted">{(items.every((x) => x.source === "youtube") ? t("勾选想订阅的频道") : t("勾选想订阅的节目"))}</span>}
          <div className="spacer" />
          <button className="btn ghost" onClick={() => setItems(null)} disabled={progress != null}>{t("返回")}</button>
          <button className="btn primary" onClick={() => void go()} disabled={!picked.size || progress != null}>{picked.size ? t("订阅 {n} 个", { n: picked.size }) : t("订阅")}</button>
        </div>
      </div>
    );
  }

  return (
    <div className="import">
      <div className="import-card">
        <div className="import-card-head"><span className="import-icon yt"><Youtube size={18} /></span><div><p className="import-title">{t("YouTube 订阅")}</p><p className="muted small">{t("读取你在浏览器里登录的 YouTube，一次导入所有订阅的频道。")}</p></div></div>
        <div className="import-row-actions">
          <select value={browser} onChange={(e) => setBrowser(e.target.value as Browser)} aria-label={t("浏览器")}>
            {BROWSERS.map(([k, label]) => <option key={k} value={k}>{label}</option>)}
          </select>
          <button className="btn primary sm" onClick={() => void fromBrowser()} disabled={busy != null}>{busy === browser ? <><Loader2 size={13} className="spin" /> {t("正在读取…")}</> : t("从浏览器导入")}</button>
        </div>
        <p className="muted small import-note">
          {browser === "safari" ? t("Safari 需要先在 系统设置 › 隐私与安全性 › 完全磁盘访问权限 里允许 SubCast。") : IS_MAC ? t("系统会问是否允许 SubCast 读取 {name} 的数据，选「允许」。", { name: browserName(browser) }) : ""}
          {t("登录信息只在本机读取，读完就删除，不会上传。")}
        </p>
        {denied && <BrowserAccess browser={browser} onRetry={() => void fromBrowser()} />}
      </div>
      <div className="import-card">
        <div className="import-card-head"><span className="import-icon"><FileUp size={18} /></span><div><p className="import-title">{t("小宇宙和其他播客 App")}</p><p className="muted small">{t("导入订阅文件：OPML（小宇宙、Pocket Casts、Overcast 等），或 Google Takeout、NewPipe、FreeTube 的导出。")}</p></div></div>
        <div className="import-row-actions">
          <button className="btn sm" onClick={() => fileInput.current?.click()} disabled={busy != null}>{busy === "file" ? <Loader2 size={13} className="spin" /> : null} {t("选择文件")}</button>
          <input ref={fileInput} type="file" accept=".opml,.xml,.csv,.json,.db,.txt" hidden onChange={(e) => { const f = e.target.files?.[0]; if (f) void fromFile(f); e.target.value = ""; }} />
        </div>
        <details className="import-how small">
          <summary>{t("小宇宙怎么导出？")}</summary>
          <ol><li>{t("打开小宇宙 App，进入「我的」→「订阅」")}</li><li>{t("点右上角的分享按钮，选「导出 OPML」")}</li><li>{t("把文件存到电脑上，在这里选择它")}</li></ol>
        </details>
      </div>
      {error && <p className="error-text">{error}</p>}
    </div>
  );
}
