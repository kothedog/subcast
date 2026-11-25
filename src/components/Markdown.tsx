import { Fragment, memo, type ReactNode } from "react";
import { Scissors } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { parseClock } from "../lib/format";
import { t, useLang } from "../lib/i18n";

// A small Markdown renderer for model answers: headings, code blocks, lists, bold, italic, links, inline code, and [mm:ss] timestamps that seek.
const INLINE = /(\*\*[^*]+\*\*|\*(?!\s)[^*]+(?<!\s)\*|`[^`]+`|\[[^\]]+\]\(https?:\/\/[^)\s]+\)|\[\d{1,2}:\d{2}(?::\d{2})?(?:\s*[-–~至]\s*\d{1,2}:\d{2}(?::\d{2})?)?\](?:\s*[-–~至]\s*\[\d{1,2}:\d{2}(?::\d{2})?\])?)/g;

function inline(text: string, onSeek: (t: number) => void, key: string, onClip?: (start: number, end?: number) => void): ReactNode[] {
  return text.split(INLINE).map((part, i) => {
    const k = `${key}-${i}`;
    if (part.startsWith("**") && part.endsWith("**")) return <strong key={k}>{part.slice(2, -2)}</strong>;
    if (part.startsWith("*") && part.endsWith("*") && part.length > 2) return <em key={k}>{part.slice(1, -1)}</em>;
    if (part.startsWith("`") && part.endsWith("`")) return <code key={k}>{part.slice(1, -1)}</code>;
    const linkMatch = part.match(/^\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)$/);
    if (linkMatch) {
      return <a key={k} href={linkMatch[2]} className="notes-link" onClick={(e) => { e.preventDefault(); void openUrl(linkMatch[2]); }}>{linkMatch[1]}</a>;
    }
    if (part.startsWith("[")) {
      const stamps = [...part.matchAll(/\b(\d{1,2}:\d{2}(?::\d{2})?)\b/g)];
      if (stamps.length >= 2) {
        const s1 = parseClock(stamps[0][1]);
        const s2 = parseClock(stamps[1][1]);
        if (s1 != null && s2 != null) {
          return (
            <span key={k} className="stamp-range">
              <button className="stamp" onClick={() => onSeek(s1)} title={t("跳到这里播放")}>{stamps[0][1]}</button>
              <span className="muted">–</span>
              <button className="stamp" onClick={() => onSeek(s2)} title={t("跳到这里播放")}>{stamps[1][1]}</button>
              {onClip && (
                <button type="button" className="stamp-clip" title={t("剪成双语视频")} aria-label={t("剪成双语视频")} onClick={() => onClip(s1, s2)}>
                  <Scissors size={11} />
                </button>
              )}
            </span>
          );
        }
      }
      if (stamps.length === 1) {
        const seconds = parseClock(stamps[0][1]);
        return seconds == null ? part : (
          <button key={k} className="stamp" onClick={() => onSeek(seconds)} title={t("跳到这里播放")}>{stamps[0][1]}</button>
        );
      }
    }
    return <Fragment key={k}>{part}</Fragment>;
  });
}

/** Memoised: a long conversation re-parses only the answer that changed. */
export const Markdown = memo(function Markdown({ text, onSeek, onClip }: { text: string; onSeek: (t: number) => void; onClip?: (start: number, end?: number) => void }) {
  useLang((s) => s.lang); // memoised: follow a language change itself
  const blocks: ReactNode[] = [];
  let list: ReactNode[] = [];
  let ordered = false;
  // A list whose every item starts with a timestamp is a timeline (chapters), drawn as one.
  let timeline = true;
  let rows: string[][] = [];
  let inCode = false;
  let codeLines: string[] = [];
  const flushCode = () => {
    if (!codeLines.length && !inCode) return;
    blocks.push(<pre key={`c${blocks.length}`} className="md-code"><code>{codeLines.join("\n")}</code></pre>);
    codeLines = [];
  };
  const flushTable = () => {
    if (!rows.length) return;
    const [head, ...body] = rows;
    const k = `t${blocks.length}`;
    blocks.push(
      <div key={k} className="md-table"><table>
        <thead><tr>{head.map((c, j) => <th key={j}>{inline(c, onSeek, `${k}h${j}`, onClip)}</th>)}</tr></thead>
        <tbody>{body.map((r, i) => <tr key={i}>{r.map((c, j) => <td key={j}>{inline(c, onSeek, `${k}${i}-${j}`, onClip)}</td>)}</tr>)}</tbody>
      </table></div>,
    );
    rows = [];
  };
  const flush = () => {
    flushTable();
    const cls = timeline ? "timeline" : undefined;
    if (list.length) blocks.push(ordered ? <ol key={`l${blocks.length}`} className={cls}>{list}</ol> : <ul key={`l${blocks.length}`} className={cls}>{list}</ul>);
    list = [];
    timeline = true;
  };
  text.split("\n").forEach((raw, i) => {
    const line = raw.trimEnd();
    if (line.trim().startsWith("```")) {
      if (inCode) { flushCode(); inCode = false; }
      else { flush(); inCode = true; }
      return;
    }
    if (inCode) { codeLines.push(raw); return; }
    const cells = line.trim().match(/^\|(.*)\|$/);
    if (cells) {
      if (list.length) flush();
      if (!/^[\s|:-]+$/.test(cells[1])) rows.push(cells[1].split("|").map((c) => c.trim()));
      return;
    }
    flushTable();
    const bullet = line.match(/^\s*(?:[-*•]|(\d+)[.)、])\s+(.*)$/);
    if (bullet) {
      const isOrdered = Boolean(bullet[1]);
      if (list.length && isOrdered !== ordered) flush();
      ordered = isOrdered;
      const stamped = /^\[\d{1,2}:\d{2}/.test(bullet[2]);
      timeline &&= stamped;
      list.push(<li key={i} className={stamped ? "ts-item" : undefined}>{inline(bullet[2], onSeek, `b${i}`, onClip)}</li>);
      return;
    }
    if (line.trim().startsWith(">")) { flush(); blocks.push(<blockquote key={i}>{inline(line.trim().replace(/^>\s*/, ""), onSeek, `q${i}`, onClip)}</blockquote>); return; }
    flush();
    const heading = line.match(/^(#{1,4})\s+(.*)$/);
    if (heading) blocks.push(heading[1].length <= 2 ? <h3 key={i}>{inline(heading[2], onSeek, `h${i}`, onClip)}</h3> : <h4 key={i}>{inline(heading[2], onSeek, `h${i}`, onClip)}</h4>);
    else if (line.trim() === "---") blocks.push(<hr key={i} />);
    else if (line.trim()) blocks.push(<p key={i}>{inline(line, onSeek, `p${i}`, onClip)}</p>);
  });
  if (inCode) flushCode();
  flush();
  return <div className="markdown">{blocks}</div>;
});
