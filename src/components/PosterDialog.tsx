import { useEffect, useRef, useState } from "react";
import { ChevronDown, Copy, Loader2, X } from "lucide-react";
import { api, type Episode } from "../lib/api";
import { CJK, POSTER_FONTS, POSTER_THEMES, drawPoster, loadPosterFont, type PosterInput } from "../lib/poster";
import { useStore } from "../store";
import { lang, t, tx } from "../lib/i18n";
import { Modal } from "./Chrome";
import logoUrl from "../assets/logo.png";

const DOWNLOADS_DIR = /^.*\/(Downloads|下载)\//; // i18n-ignore (macOS folder names)
const PREF = "subcast-poster";
const readPref = (): { theme?: string; font?: string } => { try { return JSON.parse(localStorage.getItem(PREF) ?? "{}"); } catch { return {}; } };
const toSeconds = (ts?: string) => (ts ? ts.split(":").reduce((n, p) => n * 60 + Number(p), 0) : 0);

/** Quote-sharing poster generator dialog, loaded lazily when requested from SelectionTools. */
export function PosterDialog({ text, ts, getEpisode, onClose }: { text: string; ts?: string; getEpisode: () => Promise<Episode | null>; onClose: () => void }) {
  const [url, setUrl] = useState<string | null>(null);
  const [name, setName] = useState(() => t("SubCast 摘句"));
  const [error, setError] = useState<string | null>(null);
  const [input, setInput] = useState<PosterInput | null>(null);
  const [themeId, setThemeId] = useState(() => readPref().theme ?? POSTER_THEMES[0].id);
  const [fontId, setFontId] = useState(() => readPref().font ?? POSTER_FONTS[0].id);
  const [fontBusy, setFontBusy] = useState(false);
  const [editOpen, setEditOpen] = useState(false);
  const logo = useRef<HTMLImageElement | null>(null);
  // Translate and gather the source once; the drawing is redone on every theme, font, or text change.
  useEffect(() => {
    (async () => {
      try {
        const ep = await getEpisode();
        const zhLocale = lang() === "zh-TW" ? "zh-TW" : "zh-CN";
        // A selection across a bilingual row comes with its timestamp and both lines: drop the time, split the languages.
        const lines = text.split(/\n+/).map((l) => l.replace(/^\s*\d{1,2}:\d{2}(?::\d{2})?\s*/, "").trim()).filter(Boolean)
          // "中文译文 English original" on one line: cut where the English sentence starts after the last Chinese character.
          .flatMap((l) => { const m = l.match(/^(.*[\u4e00-\u9fff][^A-Za-z]*?)\s*([A-Z"'“‘(][A-Za-z].{10,})$/); return m && !CJK.test(m[2]) ? [m[1].trim(), m[2].trim()] : [l]; });
        const zhPart = lines.filter((l) => CJK.test(l)).join("");
        const enPart = lines.filter((l) => !CJK.test(l)).join(" ");
        const cjk = !!zhPart && !/[a-z]{6}/i.test(zhPart);
        let zh = zhPart, en = enPart;
        if (!cjk) { en = lines.join(" "); try { zh = (await api.translateTexts([en], zhLocale))[0] ?? ""; } catch { zh = ""; } }
        const titleZh = ep ? (await api.translateTexts([ep.title], zhLocale).catch(() => [ep.title]))[0] || ep.title : "";
        const secs = toSeconds(ts);
        const link = ep?.videoId ? `https://www.youtube.com/watch?v=${ep.videoId}${secs ? `&t=${Math.floor(secs)}s` : ""}` : ep?.link;
        const img = new Image(); img.src = logoUrl; await img.decode().catch(() => {}); logo.current = img;
        setName(t("SubCast 摘句 {title}", { title: (titleZh || "").slice(0, 20) }));
        setInput({
          zh, en,
          title: titleZh,
          // The show keeps its own name (Acquired, not 收购): it is what readers will search for.
          show: ep?.showTitle ?? "",
          meta: [ep?.published ? new Date(ep.published).toLocaleDateString(zhLocale) : "", ts ? t("第 {ts}", { ts }) : ""].filter(Boolean).join(" · "),
          url: link,
          ts,
        });
      } catch (e) { setError(String((e as any)?.message ?? e)); }
    })();
  }, []);
  useEffect(() => {
    if (!input) return;
    let live = true;
    try { localStorage.setItem(PREF, JSON.stringify({ theme: themeId, font: fontId })); } catch { /* private mode */ }
    const timer = window.setTimeout(() => {
      void (async () => {
        const theme = POSTER_THEMES.find((x) => x.id === themeId) ?? POSTER_THEMES[0];
        let font = POSTER_FONTS.find((x) => x.id === fontId) ?? POSTER_FONTS[0];
        if (font.css) {
          setFontBusy(true);
          const ok = await loadPosterFont(font, [input.zh, input.title, input.show].join(""));
          if (!live) return;
          setFontBusy(false);
          if (!ok) { useStore.getState().notify(t("字体没有加载成功，先用系统字体")); font = POSTER_FONTS[0]; }
        }
        const c = await drawPoster(input, logo.current ?? undefined, theme, font);
        if (live) setUrl(c.toDataURL("image/png"));
      })();
    }, editOpen ? 180 : 0);
    return () => { live = false; window.clearTimeout(timer); };
  }, [input, themeId, fontId, editOpen]);
  const save = async () => { if (!url) return; try { const p = await api.saveImage(name, url); useStore.getState().notify(t("已保存到 {path}", { path: p.replace(DOWNLOADS_DIR, tx("folder", "下载") + "/") })); } catch (e) { useStore.getState().notify(String(e)); } };
  const copyImg = async () => {
    if (!url) return;
    try { const blob = await (await fetch(url)).blob(); await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]); useStore.getState().notify(t("图片已复制")); }
    catch { useStore.getState().notify(t("复制失败，请用「保存图片」")); }
  };
  return (
    <Modal title={t("分享海报")} onClose={onClose} wide>
      <div className="poster-layout">
        <div className="poster-pv">
          {url ? <img src={url} alt={t("分享海报")} /> : error ? <p className="error-text">{error}</p> : <div className="poster-wait"><Loader2 className="spin" size={18} /> {t("正在生成海报…")}</div>}
          {fontBusy && url && <span className="poster-busy"><Loader2 className="spin" size={13} /> {t("正在加载字体…")}</span>}
        </div>
        <div className="poster-side">
          <p className="poster-label">{t("主题")}</p>
          <div className="poster-themes">
            {POSTER_THEMES.map((th) => (
              <button key={th.id} type="button" className={`poster-theme ${themeId === th.id ? "on" : ""}`} aria-pressed={themeId === th.id} onClick={() => setThemeId(th.id)}>
                <span className="swatch" style={{ background: th.bg2 ? `linear-gradient(160deg, ${th.bg}, ${th.bg2})` : th.bg }}><i style={{ background: th.accent }} /><b style={{ color: th.ink }}>文</b></span>
                <span>{t(th.name)} · {t(th.style)}</span>
              </button>
            ))}
          </div>
          <p className="poster-label">{t("字体")}</p>
          <div className="poster-fonts">
            {POSTER_FONTS.map((f) => (
              <button key={f.id} type="button" className={`poster-font ${fontId === f.id ? "on" : ""}`} aria-pressed={fontId === f.id} onClick={() => setFontId(f.id)}>
                <span className="pf-name">{t(f.name)}</span>
                {f.css && <span className="pf-note">{t("开源")}</span>}
              </button>
            ))}
          </div>
          <p className="muted small">{t("开源字体第一次使用时联网下载，只取海报上用到的字。")}</p>
          {input && (
            <div className="poster-edit">
              <button type="button" className="poster-edit-toggle" aria-expanded={editOpen} onClick={() => setEditOpen((v) => !v)}>
                <span>{t("修改文字")}</span>
                <ChevronDown size={13} className={editOpen ? "flip" : ""} />
              </button>
              {editOpen && (
                <div className="poster-edit-fields">
                  <label>
                    <span className="muted small">{t("中文")}</span>
                    <textarea rows={3} value={input.zh} onChange={(e) => setInput((cur) => (cur ? { ...cur, zh: e.target.value } : cur))} />
                  </label>
                  <label>
                    <span className="muted small">{t("原文")}</span>
                    <textarea rows={2} value={input.en} onChange={(e) => setInput((cur) => (cur ? { ...cur, en: e.target.value } : cur))} />
                  </label>
                  <label>
                    <span className="muted small">{t("出处标题")}</span>
                    <input value={input.title} onChange={(e) => setInput((cur) => (cur ? { ...cur, title: e.target.value } : cur))} />
                  </label>
                </div>
              )}
            </div>
          )}
        </div>
      </div>
      <div className="modal-foot">
        <span className="spacer" />
        <button className="btn ghost" onClick={onClose}><X size={14} /> {t("关闭")}</button>
        <button className="btn" onClick={copyImg} disabled={!url}><Copy size={14} /> {t("复制图片")}</button>
        <button className="btn primary" onClick={save} disabled={!url}>{t("保存图片")}</button>
      </div>
    </Modal>
  );
}
