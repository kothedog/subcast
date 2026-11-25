// Connecting the AI agents on this computer (Claude Code, Codex, Antigravity…) to SubCast's MCP server, so a person
// can say "总结 Lex Fridman 最新一期" there. Two ways in: a list in Settings › 通用, and a one-time offer a little after
// launch. Nothing is written to an agent's config without a click.
import { useEffect, useState } from "react";
import { Check, Loader2, Plug, RefreshCw, X } from "lucide-react";
import { api, errorText, type McpTarget } from "../lib/api";
import { t } from "../lib/i18n";
import { useStore } from "../store";
import { Mark } from "./ProviderMark";

const EXAMPLE = () => t("「总结一下 Lex Fridman 最新一期」「找出这期的高光片段」");

/** The agents on this computer; null while loading or when this build has no CLI to connect (development builds). */
function useTargets() {
  const [targets, setTargets] = useState<McpTarget[] | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const load = () => { void api.mcpTargets().then((x) => { setTargets(x); setUnavailable(false); }).catch(() => setUnavailable(true)); };
  useEffect(load, []);
  const put = (n: McpTarget) => setTargets((xs) => xs?.map((x) => (x.id === n.id ? n : x)) ?? xs);
  return { targets, unavailable, load, put };
}

/** Connects each agent in turn; returns the names that worked and the first error. */
async function connectAll(list: McpTarget[], put: (t: McpTarget) => void, onBusy: (id: string | null) => void) {
  const done: string[] = [];
  let failed: string | null = null;
  for (const x of list) {
    onBusy(x.id);
    try { const n = await api.mcpConnect(x.id); put(n); if (n.connected) done.push(n.name); }
    catch (e) { failed ??= `${t(x.name)}：${errorText(e)}`; }
  }
  onBusy(null);
  return { done, failed };
}

/** Settings › 通用: every agent found, with its state and a button. */
export function AgentConnectSettings() {
  const { targets, unavailable, load, put } = useTargets();
  const notify = useStore((s) => s.notify);
  const [busy, setBusy] = useState<string | null>(null);
  const markOffered = () => { if (!useStore.getState().settings.agentsOffered) void useStore.getState().saveSettings({ agentsOffered: true }); };

  if (unavailable) return <p className="muted small">{t("发布版才能接入（开发版没有附带命令行程序）。")}</p>;
  if (!targets) return <p className="muted small"><Loader2 size={13} className="spin" /> {t("正在查找本机的 AI 助手…")}</p>;
  if (!targets.length) return <p className="muted small">{t("没有找到支持的 AI 助手。装好 Claude Code、Codex、Antigravity CLI 等之后，回来点「重新查找」。")} <button className="link-btn small" onClick={load}>{t("重新查找")}</button></p>;

  const pending = targets.filter((x) => !x.connected || x.outdated);
  const toggle = async (x: McpTarget) => {
    setBusy(x.id);
    try {
      const n = x.connected && !x.outdated ? await api.mcpDisconnect(x.id) : await api.mcpConnect(x.id);
      put(n);
      markOffered();
      notify(n.connected ? t("已接入 {name}，重新打开它就能用", { name: t(n.name) }) : t("已从 {name} 移除", { name: t(n.name) }));
    } catch (e) { notify(errorText(e)); } finally { setBusy(null); }
  };
  const all = async () => {
    const { done, failed } = await connectAll(pending, put, setBusy);
    markOffered();
    notify(failed ? failed : t("已接入 {n} 个 AI 助手，重新打开它们就能用", { n: done.length }));
  };

  return (
    <div className="agent-connect">
      <div className="agent-connect-head">
        <p className="muted small">{t("接入后，可以直接对它们说：{example}", { example: EXAMPLE() })}</p>
        {pending.length > 1 && <button className="btn primary sm" disabled={busy != null} onClick={() => void all()}><Plug size={13} /> {t("全部接入")}</button>}
      </div>
      <ul className="agent-list">
        {targets.map((x) => (
          <li key={x.id} className="agent-row">
            <Mark id={x.icon} name={x.name} size={28} />
            <div className="agent-row-text">
              <p>{t(x.name)}</p>
              <p className="muted small">
                {x.outdated ? <span className="warn-text">{t("指向旧位置的 SubCast，点「更新」修好")}</span>
                  : x.connected ? <span className="ok-text"><Check size={12} /> {t("已接入")}</span>
                  : t("未接入")}
              </p>
            </div>
            <button className={`btn sm ${x.connected && !x.outdated ? "ghost" : ""}`} disabled={busy != null} onClick={() => void toggle(x)} title={x.config}>
              {busy === x.id ? <Loader2 size={13} className="spin" /> : x.outdated ? <RefreshCw size={13} /> : null}
              {x.outdated ? t("更新") : x.connected ? t("移除") : t("接入")}
            </button>
          </li>
        ))}
      </ul>
      <p className="muted small">{t("SubCast 只在对方的配置里加一条 subcast，原有设置不动；随时可以移除。")}</p>
    </div>
  );
}

/** The one-time offer, a few seconds after launch, when agents are found that don't have SubCast yet. */
export function AgentConnectOffer() {
  const offered = useStore((s) => s.settings.agentsOffered);
  const [list, setList] = useState<McpTarget[] | null>(null);
  const [state, setState] = useState<"ask" | "busy" | "done">("ask");
  const [result, setResult] = useState("");
  const [allFailed, setAllFailed] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);

  useEffect(() => {
    if (offered) return;
    const timer = window.setTimeout(() => {
      void api.mcpTargets().then((xs) => { const todo = xs.filter((x) => !x.connected); if (todo.length) setList(todo); }).catch(() => {});
    }, 6000);
    return () => window.clearTimeout(timer);
  }, [offered]);

  if (offered && state !== "done") return null;
  if (!list) return null;
  const close = () => { setList(null); if (!useStore.getState().settings.agentsOffered) void useStore.getState().saveSettings({ agentsOffered: true }); };
  const names = list.map((x) => t(x.name));
  const shown = names.length > 3 ? t("{names} 等 {n} 个", { names: names.slice(0, 3).join("、"), n: names.length }) : names.join("、");

  const connect = async () => {
    setState("busy");
    const { done, failed } = await connectAll(list, () => {}, setBusyId);
    void useStore.getState().saveSettings({ agentsOffered: true });
    setAllFailed(!!failed && !done.length);
    setResult(failed && !done.length ? failed : failed ? t("已接入 {n} 个，有一个没成功：{error}", { n: done.length, error: failed }) : t("已接入 {n} 个。重新打开它们，就能让它们帮你总结、找高光、剪视频。", { n: done.length }));
    setState("done");
    window.setTimeout(() => setList(null), failed ? 12000 : 7000);
  };
  const choose = () => { close(); useStore.getState().openSettings("agents"); };

  return (
    <div className="update-card agent-offer" role="status" aria-label={t("让 AI 助手调用 SubCast")}>
      <div className="agent-offer-marks" aria-hidden>
        {list.slice(0, 3).map((x) => <span key={x.id} className={busyId === x.id ? "busy" : ""}><Mark id={x.icon} name={x.name} size={26} /></span>)}
      </div>
      <div className="update-text">
        {state === "done" ? (
          <p className={`update-title${allFailed ? " error-text" : ""}`}>{!allFailed && <Check size={13} className="ok-text" />} {result}</p>
        ) : (
          <>
            <p className="update-title">{t("让 {names} 也能用 SubCast？", { names: shown })}</p>
            <p className="muted small">{t("接入后，可以直接对它们说：{example}", { example: EXAMPLE() })}</p>
            <div className="agent-offer-actions">
              <button className="btn primary sm" disabled={state === "busy"} onClick={() => void connect()}>
                {state === "busy" ? <Loader2 size={13} className="spin" /> : <Plug size={13} />} {state === "busy" ? t("正在接入…") : t("一键接入")}
              </button>
              <button className="btn ghost sm" disabled={state === "busy"} onClick={choose}>{t("挑选")}</button>
            </div>
          </>
        )}
      </div>
      {state !== "busy" && <button className="icon-btn" title={t("不用了")} aria-label={t("不用了")} onClick={close}><X size={14} /></button>}
    </div>
  );
}
