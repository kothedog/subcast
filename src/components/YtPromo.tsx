// Promotion for the author's own podcast (词元之外 · Next Token) and its YouTube channel.
// The slots follow the "推荐节目的 YouTube 频道" setting, except the About card; the Discover banner can also be closed for good.
import { Check, Loader2, Plus, X, Youtube } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { t } from "../lib/i18n";
import { pickName } from "../lib/picks";
import { homeShow, showLinks, subscribeUrl } from "../lib/showLinks";
import { useStore } from "../store";

export const usePromo = () => useStore((s) => s.settings.youtubePromo);

/** The red YouTube subscribe button. YouTube's own red so it reads as YouTube at a glance. */
export function YtButton({ channel, label, small }: { channel: string; label?: string; small?: boolean }) {
  return (
    <button className={`btn yt-btn ${small ? "sm" : ""}`} onClick={(e) => { e.stopPropagation(); void openUrl(subscribeUrl(channel)); }}>
      <Youtube size={small ? 14 : 16} /> {label ?? t("订阅 YouTube 频道")}
    </button>
  );
}

/** The secondary places a show lives (Bilibili), kept quiet so YouTube stays the one obvious action. */
export function AlsoOn({ feedUrl }: { feedUrl?: string }) {
  const l = showLinks(feedUrl);
  if (!l.bilibili) return null;
  return <button className="link-btn also-on" onClick={(e) => { e.stopPropagation(); void openUrl(l.bilibili!); }}>{t("也在 B 站")}</button>;
}

/** Discover's top banner: our own show, with subscribing in the app and on YouTube side by side. */
export function HomeShowBanner({ subscribed, busy, onSubscribe, onOpen, onHide }: { subscribed: boolean; busy: boolean; onSubscribe: () => void; onOpen: () => void; onHide: () => void }) {
  const p = homeShow();
  const yt = showLinks(p?.input).youtube;
  if (!p || !yt) return null;
  return (
    <div className="home-show" role="button" tabIndex={0} onClick={onOpen} onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && (e.preventDefault(), e.stopPropagation(), onOpen())}>
      <button type="button" className="icon-btn sm home-show-close" title={t("不再显示")} aria-label={t("不再显示")} onClick={(e) => { e.stopPropagation(); onHide(); }}><X size={15} /></button>
      <img src={p.cover} alt="" />
      <div className="home-show-body">
        <p className="home-show-kicker">{t("主创的播客")}</p>
        <p className="home-show-name">{pickName(p)}</p>
        <p className="home-show-desc">{t("SubCast 作者乔木和朋友们的 AI 播客，和一线从业者长聊。每期都有视频版，在 YouTube 首发。")}</p>
        <div className="row-actions">
          <YtButton channel={yt} />
          {subscribed
            ? <span className="muted small home-show-done"><Check size={13} /> {t("已在 App 里订阅")}</span>
            : <button className="btn" onClick={(e) => { e.stopPropagation(); onSubscribe(); }} disabled={busy}>{busy ? <Loader2 size={14} className="spin" /> : <Plus size={15} />} {t("在 App 里订阅")}</button>}
          <AlsoOn feedUrl={p.input} />
        </div>
      </div>
    </div>
  );
}

/** Under the video of an episode whose show has a YouTube channel. */
export function YtVideoStrip({ channel, name, cover }: { channel: string; name: string; cover?: string }) {
  return (
    <div className="yt-strip">
      {cover ? <img src={cover} alt="" /> : <span className="yt-strip-icon"><Youtube size={16} /></span>}
      <div className="yt-strip-text">
        <p className="yt-strip-name clamp1">{name}</p>
        <p className="muted small clamp1">{t("视频版在 YouTube 首发，订阅后新一期第一时间看到")}</p>
      </div>
      <YtButton channel={channel} label={t("订阅")} small />
    </div>
  );
}

/** About page: a short card for our own show. */
export function HomeShowCard() {
  // Always shown on the About page: it is where the author introduces himself.
  const p = homeShow();
  const yt = showLinks(p?.input).youtube;
  if (!p || !yt) return null;
  return (
    <div className="home-card">
      <img src={p.cover} alt="" />
      <div className="home-card-text">
        <p className="qr-title">{t("听听我们的播客")}</p>
        <p className="muted small">{t("{name}：乔木和朋友们聊 AI", { name: pickName(p) })}</p>
      </div>
      <YtButton channel={yt} label={t("订阅")} small />
    </div>
  );
}
