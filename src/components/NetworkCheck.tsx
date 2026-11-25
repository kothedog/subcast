// "网络诊断": what SubCast can reach through its proxy setting (checked by the app) next to what its web view can
// reach (where YouTube's embedded player runs), with a plain verdict and a report to copy into a feedback message.
import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { Check, Copy, Loader2, X } from "lucide-react";
import { api, type NetCheck } from "../lib/api";
import { useStore } from "../store";
import { IS_MAC, t, tk, tr } from "../lib/i18n";

interface Row { id: string; label: string; ok: boolean; ms?: number | null; detail: string }

/** Whether the web view reaches a URL: an opaque answer is enough, a network error or 10 s of nothing is not. */
async function viewReach(url: string): Promise<{ ok: boolean; ms: number; detail: string }> {
  const start = performance.now();
  const ctl = new AbortController();
  const timer = window.setTimeout(() => ctl.abort(), 10_000);
  try {
    await fetch(url, { mode: "no-cors", cache: "no-store", signal: ctl.signal });
    return { ok: true, ms: Math.round(performance.now() - start), detail: "" };
  } catch (e) {
    return { ok: false, ms: Math.round(performance.now() - start), detail: ctl.signal.aborted ? t("10 秒内没有响应") : String((e as Error)?.message ?? e) };
  } finally { window.clearTimeout(timer); }
}

const LABELS: Record<string, string> = {
  proxy: tk("SubCast 使用的代理"),
  youtube: tk("SubCast → YouTube"),
  video: tk("SubCast → YouTube 视频服务器"),
  ytdlpSource: tk("SubCast → 播放组件下载源"),
  ytdlp: tk("播放组件 yt-dlp"),
  viewYoutube: tk("播放器 → YouTube"),
  viewVideo: tk("播放器 → YouTube 视频服务器"),
};

export function NetworkCheck() {
  const [rows, setRows] = useState<Row[] | null>(null);
  const [viewProxy, setViewProxy] = useState("");
  const [busy, setBusy] = useState(false);
  // Opened from a video's error ("网络诊断"): run right away.
  const asked = useStore((s) => (s.settingsSection === "network" ? s.settingsNonce : null));

  const run = async () => {
    setBusy(true);
    setRows(null);
    const [report, vy, vv] = await Promise.all([
      api.networkCheck().catch((e) => ({ checks: [] as NetCheck[], webviewProxy: "", error: String(e) })),
      viewReach("https://www.youtube.com/generate_204"),
      viewReach("https://redirector.googlevideo.com/generate_204"),
    ]);
    const proxyText = (d: string) => d === "none" ? t("无（直接连接）") : d.replace(/^SubCast: /, `${t("SubCast 里设置的")} `).replace(/^system: /, `${t("系统代理")} `);
    const ytdlpText = (d: string) => d === "not installed" ? t("还没下载（第一次用时会自动下载）") : d === "doesn't run" ? t("无法运行") : d;
    setRows([
      ...report.checks.map((c) => ({ id: c.id, label: t(LABELS[c.id] ?? c.id), ok: c.ok, ms: c.ms, detail: c.id === "proxy" ? proxyText(c.detail) : c.id === "ytdlp" ? ytdlpText(c.detail) : tr(c.detail) })),
      { id: "viewYoutube", label: t(LABELS.viewYoutube), ...vy },
      { id: "viewVideo", label: t(LABELS.viewVideo), ...vv },
    ]);
    setViewProxy(report.webviewProxy);
    setBusy(false);
  };
  useEffect(() => { if (asked != null) void run(); }, [asked]);

  const ok = (id: string) => rows?.find((r) => r.id === id)?.ok;
  // One sentence on what to do, from the pattern of what failed.
  const verdict = !rows ? null
    : ok("viewYoutube") && ok("viewVideo") && ok("youtube") ? t("网络正常：播放器和 SubCast 都能连上 YouTube。")
    : !ok("youtube") && !ok("viewYoutube") ? t("SubCast 和播放器都连不上 YouTube。请确认代理软件在运行，并在上面填上它的地址（例如 127.0.0.1:7890）。")
    : ok("youtube") && !(ok("viewYoutube") && ok("viewVideo"))
      ? (viewProxy === "restart" ? t("SubCast 能连上 YouTube，播放器还不能：代理改过了，重启 SubCast 后播放器才会用上新代理。")
        : viewProxy === "unsupported-os" ? t("SubCast 能连上 YouTube，播放器不能：这个 macOS 版本的播放器只能用系统代理。请在系统设置里打开代理，或开启代理软件的「系统代理」。")
        : viewProxy === "unsupported-proxy" ? t("SubCast 能连上 YouTube，播放器不能：播放器只支持带端口的 HTTP 或 SOCKS5 代理地址。")
        : viewProxy === "system" ? t("SubCast 能连上 YouTube，播放器不能：在上面填上代理地址并重启 SubCast，播放器就会使用它。")
        : t("SubCast 能连上 YouTube，播放器不能。视频会自动改用 SubCast 的连接播放，只是开始得慢一些。"))
    : !ok("youtube") ? t("播放器能连上 YouTube，SubCast 不能：检查上面的代理地址是否正确。")
    : !ok("ytdlp") && !ok("ytdlpSource") ? t("播放组件还没下载，下载源也连不上：直连播放和下载视频暂时用不了。")
    : t("部分检查没有通过，看下面的详情。");

  const copy = async () => {
    if (!rows) return;
    const version = await getVersion().catch(() => "?");
    const text = [`SubCast ${version} · ${IS_MAC ? "macOS" : navigator.platform} · ${navigator.userAgent.match(/(Mac OS X|Windows NT|Linux)[^;)]*/)?.[0] ?? ""}`,
      `webview proxy: ${viewProxy}`,
      ...rows.map((r) => `${r.ok ? "✓" : "✗"} ${r.label}${r.ms != null ? ` (${r.ms} ms)` : ""}${r.detail ? ` — ${r.detail}` : ""}`)].join("\n");
    try { await navigator.clipboard.writeText(text); useStore.getState().notify(t("诊断结果已复制，可以粘贴到反馈里")); }
    catch { useStore.getState().notify(t("复制失败，请重试")); }
  };

  return (
    <div className="net-check" id="network">
      <div className="row-actions">
        <button className="btn sm" onClick={() => void run()} disabled={busy}>{busy ? <><Loader2 size={13} className="spin" /> {t("正在检查…")}</> : t("网络诊断")}</button>
        {rows && <button className="btn ghost sm" onClick={() => void copy()}><Copy size={13} /> {t("复制结果")}</button>}
        {!rows && !busy && <span className="muted small">{t("视频加载不出来时，查一下是哪里不通")}</span>}
      </div>
      {verdict && <p className="net-verdict small">{verdict}</p>}
      {rows && (
        <ul className="net-rows">
          {rows.map((r) => (
            <li key={r.id} className={r.ok ? "ok" : "bad"}>
              {r.ok ? <Check size={13} /> : <X size={13} />}
              <span className="net-label">{r.label}</span>
              <span className="muted small clamp1" title={r.detail}>{r.detail || (r.ms != null ? `${r.ms} ms` : "")}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
