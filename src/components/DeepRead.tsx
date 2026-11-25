import { useEffect, useMemo, useRef, useState } from "react";
import { BookOpen, ExternalLink } from "lucide-react";
import { SkeletonArticle } from "./Skeleton";
import { ReadAlongControls, useReadAlongPage } from "./ReadAlong";
import { markSentences } from "../lib/sentences";
import { useReadAlong } from "../lib/readAlong";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, type Episode } from "../lib/api";
import { seekSoon, useStore } from "../store";
import { t } from "../lib/i18n";

const KEEP = new Set(["P", "H2", "H3", "H4", "BLOCKQUOTE", "UL", "OL", "LI", "STRONG", "EM", "B", "I", "A", "BR", "HR", "SPAN", "ASIDE", "FIGURE", "FIGCAPTION", "CODE", "PRE"]);

/** Keeps the deep-read's structure and nothing executable: unknown elements are unwrapped, attributes dropped
 *  except link targets and the site's timestamp markers (data-t="0:47:55"). */
function sanitize(html: string): string {
  const doc = new DOMParser().parseFromString(`<div>${html}</div>`, "text/html");
  const walk = (el: Element) => {
    for (const c of Array.from(el.children)) {
      walk(c);
      if (["SCRIPT", "STYLE", "IFRAME", "BUTTON", "FORM", "INPUT", "SVG", "IMG", "VIDEO", "AUDIO"].includes(c.tagName)) { c.remove(); continue; }
      if (!KEEP.has(c.tagName)) { c.replaceWith(...Array.from(c.childNodes)); continue; }
      const href = c.getAttribute("href");
      const t = c.getAttribute("data-t");
      for (const a of Array.from(c.attributes)) c.removeAttribute(a.name);
      if (c.tagName === "A" && href && /^https?:/.test(href)) c.setAttribute("href", href);
      if (t && /^[\d:]+$/.test(t)) { c.setAttribute("data-t", t); c.setAttribute("class", "ts"); }
    }
  };
  walk(doc.body.firstElementChild!);
  return doc.body.firstElementChild!.innerHTML;
}

const seconds = (t: string) => t.split(":").reduce((n, p) => n * 60 + Number(p), 0);

/** The 乔木精读 article for an episode, cached after the first fetch. */
/** `onMissing`: the feed promised a deep-read but there is none to fetch. */
export function DeepRead({ ep, children, onMissing }: { ep: Episode; children?: React.ReactNode; onMissing?: () => void }) {
  const [html, setHtml] = useState<string | null | undefined>(undefined);
  useEffect(() => {
    let live = true;
    setHtml(undefined);
    api.getBlob(ep.id, "deep-read").then(async (cached) => {
      if (cached) { if (live) setHtml(cached); return; }
      try { const d = await api.qiaomuDetail(ep.id); if (live) setHtml(d.deepRead ?? null); } catch { if (live) setHtml(null); }
    });
    return () => { live = false; };
  }, [ep.id]);
  useEffect(() => { if (html === null) onMissing?.(); }, [html]);
  // Each sentence marked, so 朗读 can highlight it and a click can read from it.
  const marked = useMemo(() => (html ? markSentences(sanitize(html)) : { html: "", sentences: [] }), [html]);
  const owner = `deep:${ep.id}`;
  const article = useRef<HTMLElement>(null);
  const reading = useReadAlongPage(owner, article);
  // While reading, the badge shrinks to its icon so the player fits on the header's one line.
  const readingHere = useReadAlong((s) => s.owner === owner);
  if (html === undefined) return <SkeletonArticle />;
  if (!html) return <>{children}</>;
  return (
    <div className="deep-read">
      <div className={`deep-head ${readingHere ? "ra-on" : ""}`}>
        <span className="deep-badge" title={t("AI 基于完整访谈改写的中文长文，不是逐字翻译")}><BookOpen size={13} /><span className="deep-badge-text">{t("乔木精读")}</span></span>
        <span className="spacer" />
        <ReadAlongControls owner={owner} sentences={marked.sentences} />
        {ep.link && <button className="icon-btn" title={t("在网页中打开")} onClick={() => openUrl(ep.link!)}><ExternalLink size={14} /></button>}
      </div>
      <article
        ref={article}
        className="deep-body"
        dangerouslySetInnerHTML={{ __html: marked.html }}
        onClick={(e) => {
          if (reading.onClick(e)) return;
          const el = e.target as HTMLElement;
          const ts = el.closest<HTMLElement>("[data-t]");
          if (ts) {
            const sec = seconds(ts.dataset.t!);
            const s = useStore.getState();
            if (s.current?.id === ep.id) s.player?.seek(sec);
            else { s.load({ ...ep, progress: sec }); seekSoon(sec); }
            return;
          }
          const a = el.closest("a");
          if (a?.href) { e.preventDefault(); void openUrl(a.href); }
        }}
      />
      {reading.backToReading}
    </div>
  );
}
