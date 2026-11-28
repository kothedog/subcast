import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { sizeMb } from "../lib/format";
import { ask } from "../lib/confirm";
import { useShallow } from "zustand/react/shallow";
import { useEffect, useState } from "react";
import { BookOpen, Check, ChevronDown, Cloud, Cpu, ExternalLink, FolderOpen, Github, Loader2, MessageCircle, RefreshCw, Terminal, Trash2, Volume2, X, Captions, HardDrive, Info, Keyboard, SlidersHorizontal, Sparkles, GraduationCap, type LucideIcon } from "lucide-react";
import { getVersion } from "@tauri-apps/api/app";
import { ASR_SERVICES, CLOUD_ASR_MAIN } from "../lib/settings";
import { stopSpeaking } from "../lib/tts";
import logoUrl from "../assets/logo.png";
import wechatQr from "../assets/about-wechat-qr.jpg";
import rewardQr from "../assets/about-reward-qr.png";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import { api, errorText, llmChat, type CliStatus, type CloudAsr, type ModelInfo, type Provider } from "../lib/api";
import { useStore } from "../store";
import { useClipTools } from "../lib/clipTools";
import { AgentConnectSettings } from "./AgentConnect";
import { NetworkCheck } from "./NetworkCheck";
import { bytes, clearAudioCache, clearDownloads, useOffline } from "../lib/offline";
import { HomeShowCard } from "./YtPromo";
import { YouTubeAccount } from "./YouTubeAccount";
import { useModelDownload } from "./TranscriptPanel";
import { LANGS, systemLang, t, tk, tr, type LangSetting, IS_MAC, kbd, isChineseUi } from "../lib/i18n";
import { ModelSettings } from "./ModelSettings";
import { ShortcutSettings } from "./ShortcutSettings";
import { ReadingSettings } from "./ReadingSettings";
import { LearnSettings } from "./LearnSettings";
import { TtsSettings } from "./TtsSettings";
import { SyncSettings } from "./SyncSettings";
import { DOWNLOAD_PAGE, checkForUpdate, hasUpdate, installUpdate, useUpdate } from "../lib/updater";
import { DefaultButton, KeyField, KeyRow, NO_AUTO } from "./KeyField";

/** One DeepSeek key covers Q&A, translation, transcription and read-aloud; this switches them all over at once. */
function DeepSeekEverywhere() {
  const { settings, saveSettings } = useStore(useShallow((s) => ({ settings: s.settings, saveSettings: s.saveSettings })));
  const uses = [
    { name: tk("问答"), on: settings.chatProvider === "deepseek" },
    { name: tk("字幕翻译"), on: settings.translateProvider === "deepseek" && settings.translateMode !== "google" },
    { name: tk("转写"), on: settings.asrEngine === "deepseek" },
    { name: tk("朗读"), on: settings.ttsEngine === "deepseek" },
  ];
  const all = uses.every((u) => u.on);
  return (
    <div className="ds-all">
      <div className="ds-all-text">
        <p className="setup-title">{t("一个 Key，全部用 DeepSeek")}</p>
        <p className="muted small">{uses.map((u) => <span key={u.name} className={`ds-use ${u.on ? "on" : ""}`}>{u.on && <Check size={11} />}{t(u.name)}</span>)}</p>
        <p className="muted small">{t("转写只返回文字，字幕时间是按句子估算的；需要精准时间轴时用豆包或本地模型。")}</p>
      </div>
      <button
        className={`btn sm ${all ? "" : "primary"}`}
        disabled={all}
        onClick={() => void saveSettings({ chatProvider: "deepseek", chatModel: "", translateProvider: "deepseek", translateMode: "auto", modelChosen: true, asrEngine: "deepseek", ttsEngine: "deepseek" })}
      >
        {all ? t("已全部使用") : t("全部改用 DeepSeek")}
      </button>
    </div>
  );
}

function ProviderEditor({ p, onChange, onKeyChange }: { p: Provider; onChange: (p: Provider) => void; onKeyChange?: () => void }) {
  const settings = useStore((s) => s.settings);
  const [key, setKey] = useState("");
  const [hasKey, setHasKey] = useState(false);
  const [testing, setTesting] = useState<string | null>(null);
  const [testOk, setTestOk] = useState(false);
  const [live, setLive] = useState<string[] | null>(null);
  const [fetching, setFetching] = useState(false);
  const [advanced, setAdvanced] = useState(false);
  useEffect(() => { setKey(""); setLive(null); setAdvanced(!p.needsKey); api.hasApiKey(p.id).then(setHasKey); }, [p.id]);
  const fetchModels = async () => {
    setFetching(true); setTesting(null); setTestOk(false);
    try { if (key.trim()) await saveKey(); const ids = await api.listModels(p); setLive(ids); if (!ids.length) setTesting(t("这个服务没有返回模型列表，请手动填写")); }
    catch (e) { setTesting(errorText(e)); } finally { setFetching(false); }
  };
  const saveKey = async () => { await api.setApiKey(p.id, key); setHasKey(!!key.trim()); setKey(""); onKeyChange?.(); };
  const test = async () => {
    setTesting("…"); setTestOk(false);
    try {
      if (key.trim()) await saveKey();
      const r = await llmChat({ provider: p, system: "Reply with one short word.", messages: [{ role: "user", content: "ping" }], maxTokens: 1500 });
      setTesting(t("可用 ✓ {reply}", { reply: r.slice(0, 20) })); setTestOk(true);
    } catch (e) { setTesting(errorText(e)); }
  };
  const forTranslate = settings.translateProvider === p.id && settings.translateMode !== "google";
  return (
    <div className="provider-form">
      {p.needsKey && (
        <div className="setup-block">
          <div className="setup-head">
            <p className="setup-title">API Key</p>
            {p.keyUrl && <button className="link-btn" onClick={() => openUrl(p.keyUrl!)}>{t("去官网申请")} <ExternalLink size={11} /></button>}
          </div>
          <KeyRow key={p.id} has={hasKey} value={key} onValue={setKey} onSave={saveKey} onDelete={async () => { await api.setApiKey(p.id, ""); setHasKey(false); onKeyChange?.(); }} />
        </div>
      )}
      <div className="setup-block">
        <DefaultButton id={p.id} isDefault={forTranslate} />
      </div>
      {p.id === "deepseek" && hasKey && <DeepSeekEverywhere />}
      <div className="row-actions">
        <button className="btn sm" onClick={test} disabled={testing === "…"}>{testing === "…" && <Loader2 size={13} className="spin" />} {t("测试连接")}</button>
        {testing && testing !== "…" && <span className={testOk ? "ok-text small" : "error-text small"}>{testing}</span>}
        <span className="spacer" />
        <button className="adv-toggle" onClick={() => setAdvanced(!advanced)}><ChevronDown size={12} className={advanced ? "flip" : ""} /> {t("高级设置")}</button>
      </div>
      {advanced && (
        <div className="adv-body">
          <label><span>{t("接口类型")}</span>
            <select value={p.protocol} onChange={(e) => onChange({ ...p, protocol: e.target.value as Provider["protocol"] })}>
              <option value="openai">{t("OpenAI 兼容")}</option><option value="anthropic">Anthropic</option>
            </select>
          </label>
          <label><span>{t("接口地址")}</span><input {...NO_AUTO} value={p.baseUrl} onChange={(e) => onChange({ ...p, baseUrl: e.target.value })} placeholder="https://…/v1" /></label>
          <label><span>{t("模型")}</span>
            <input {...NO_AUTO} value={p.model} list={`models-${p.id}`} onChange={(e) => onChange({ ...p, model: e.target.value })} placeholder={t("模型名")} />
            <datalist id={`models-${p.id}`}>{[...new Set([...(live ?? []), ...(p.models ?? [])])].map((m) => <option key={m} value={m} />)}</datalist>
          </label>
          {(p.models?.length || live?.length) ? (
            <div className="model-chips">
              {[...new Set([...(p.models ?? []), ...(live ?? []).slice(0, 40)])].map((m) => <button key={m} className={`chip sm ${p.model === m ? "on" : ""}`} onClick={() => onChange({ ...p, model: m })}>{m}</button>)}
            </div>
          ) : null}
          <div className="row-actions">
            <button className="btn ghost sm" onClick={fetchModels} disabled={fetching}>{fetching ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />} {t("获取模型列表")}</button>
          </div>
        </div>
      )}
    </div>
  );
}

/** Settings, a page of its own (it was a dialog): room for five sections, and the player stays usable. */
export function SettingsView() {
  const { settings, saveSettings, set, notify, settingsSection, settingsNonce } = useStore(useShallow((s) => ({ settings: s.settings, saveSettings: s.saveSettings, set: s.set, notify: s.notify, settingsSection: s.settingsSection, settingsNonce: s.settingsNonce })));
  type Section = "subtitles" | "ai" | "tts" | "general" | "learn" | "about";
  // Old section names (links from elsewhere in the app) land in their new home.
  const LEGACY: Record<string, Section> = { overview: "subtitles", reading: "subtitles", asr: "subtitles", model: "ai", tts: "tts", shortcuts: "general", storage: "general", agents: "general", network: "general", sync: "general", general: "general", about: "about", subtitles: "subtitles", ai: "ai", learn: "learn" };
  const [section, setSection] = useState<Section>(LEGACY[settingsSection ?? "subtitles"] ?? "subtitles");
  // Asked for a section while the page is already open (a "去设置" link): go there.
  useEffect(() => {
    if (settingsSection) setSection(LEGACY[settingsSection] ?? "subtitles");
    if (settingsSection === "agents") window.setTimeout(() => document.getElementById("agent-connect")?.scrollIntoView({ behavior: "smooth", block: "start" }), 80);
    if (settingsSection === "network") window.setTimeout(() => document.getElementById("network")?.scrollIntoView({ behavior: "smooth", block: "center" }), 80);
    if (settingsSection === "sync") window.setTimeout(() => document.getElementById("sync-section")?.scrollIntoView({ behavior: "smooth", block: "start" }), 80);
    if (settingsSection === "shortcuts") {
      setShowKeys(true);
      window.setTimeout(() => document.getElementById("shortcuts-section")?.scrollIntoView({ behavior: "smooth", block: "start" }), 80);
    }
    if (settingsSection === "storage") {
      setShowStorage(true);
      window.setTimeout(() => document.getElementById("storage-section")?.scrollIntoView({ behavior: "smooth", block: "start" }), 80);
    }
  }, [settingsNonce]);
  useEffect(() => () => stopSpeaking(), []);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [proxy, setProxy] = useState(settings.proxy);
  const [storageRev, setStorageRev] = useState(0);
  useEffect(() => { setProxy(settings.proxy); }, [settings.proxy]);
  const [elsewhere, setElsewhere] = useState<{ app: string; format: string }[]>([]);
  const refresh = () => api.asrModels().then((m) => { setModels(m); void api.asrElsewhere().then((x) => setElsewhere(x ?? [])).catch(() => {}); });
  const { downloading, start, cancel } = useModelDownload(refresh);
  useEffect(() => { void refresh(); }, []);
  const updateWaiting = useUpdate(hasUpdate);
  const [moreLocal, setMoreLocal] = useState(false);
  const [moreCloud, setMoreCloud] = useState(false);
  const [showKeys, setShowKeys] = useState(false);
  const [showStorage, setShowStorage] = useState(false);
  const [showCli, setShowCli] = useState(false);
  const NAV: [Section, string, LucideIcon][] = [["subtitles", tk("字幕"), Captions], ["ai", tk("AI"), Sparkles], ["tts", tk("朗读"), Volume2], ["general", tk("通用"), SlidersHorizontal], ["learn", tk("学英语"), GraduationCap], ["about", tk("关于"), Info]];
  const modelRow = (m: ModelInfo) => (
    <div key={m.id} className={`model-item ${settings.asrEngine === "local" && settings.asrModel === m.id ? "on" : ""}`}>
      <div role="button" tabIndex={0} className="model-pick" onClick={() => saveSettings({ asrEngine: "local", asrModel: m.id })} onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && (e.preventDefault(), saveSettings({ asrEngine: "local", asrModel: m.id }))}>
        <span className={`radio ${settings.asrEngine === "local" && settings.asrModel === m.id ? "on" : ""}`} />
        <div><p>{tr(m.name)}</p><p className="muted small">{m.id.startsWith("file:") ? sizeMb(m.sizeMb) : [tr(m.note), sizeMb(m.sizeMb)].filter(Boolean).join(" · ")}</p></div>
      </div>
      {downloading?.id === m.id ? (
        <div className="dl"><span className="small">{downloading.total ? Math.round((downloading.received / downloading.total) * 100) : 0}%</span><button className="btn ghost sm" onClick={() => cancel(m.id)}>{t("取消")}</button></div>
      ) : m.installed && m.from ? (
        <div className="dl"><span className="from-tag small">{t("来自 {app}", { app: tr(m.from) })}</span><button className="icon-btn" title={t("不再使用（原文件保留）")} onClick={async () => { await api.deleteModel(m.id); void refresh(); }}><X size={14} /></button></div>
      ) : m.installed ? (
        <div className="dl"><span className="ok-text small"><Check size={13} /> {t("已下载")}</span><button className="icon-btn" title={t("删除模型")} onClick={async () => {
          if (!(await ask({ title: t("删除模型") + `「${tr(m.name)}」？`, message: sizeMb(m.sizeMb), ok: t("删除"), danger: true }))) return;
          await api.deleteModel(m.id); setStorageRev((n) => n + 1); void refresh();
        }}><Trash2 size={14} /></button></div>
      ) : (
        <button className="btn sm" onClick={() => start(m.id)} disabled={downloading != null}>{t("下载")}</button>
      )}
    </div>
  );

  return (
    <div className="page settings-page">
      <h1 className="page-title">{t("设置")}</h1>
      <div className="settings">
        <nav className="settings-nav">
          {NAV.map(([k, label, Icon]) => <button key={k} className={section === k ? "on" : ""} onClick={() => setSection(k)}><Icon size={15} aria-hidden />{t(label)}{k === "about" && updateWaiting && <span className="update-dot" aria-label={t("有新版本")} />}</button>)}
        </nav>
        <div className="settings-body">
          {section === "subtitles" && (
            <>
              <ReadingSettings />
              <section className="setup-block">
                <p className="setup-title">{t("如果没有字幕时")}</p>
                <p className="muted small">{t("先查节目自带的和别人分享的字幕，都没有时才听写。")}</p>
                <div className="engine-switch">
                  <button className={`engine ${settings.asrEngine === "local" ? "on" : ""}`} onClick={() => saveSettings({ asrEngine: "local" })}>
                    <Cpu size={18} /><div><p>{t("本机模型")}</p><p className="muted small">{t("免费，音频不出本机")}</p></div>
                  </button>
                  <button className={`engine ${settings.asrEngine !== "local" ? "on" : ""}`} onClick={() => saveSettings({ asrEngine: settings.asrEngine !== "local" ? settings.asrEngine : "doubao" })}>
                    <Cloud size={18} /><div><p>{t("云端服务")}</p><p className="muted small">{t("更快，需要 Key，按量付费")}</p></div>
                  </button>
                </div>
                {settings.asrEngine === "local" ? (
                  <>
                    <div className="model-list">{models.filter((m) => m.id === "qwen3-asr").map(modelRow)}</div>
                    {elsewhere.length > 0 && !models.find((m) => m.id === "qwen3-asr")?.installed && (
                      <p className="model-elsewhere small">{t("在 {where} 里找到了 Qwen3-ASR，但它是 {format} 格式，SubCast 的识别程序只能用 GGUF 格式，没法直接共用。下载上面的 Qwen3-ASR（约 960 MB）只需一次。", { where: [...new Set(elsewhere.map((e) => tr(e.app)))].join("、"), format: [...new Set(elsewhere.map((e) => tr(e.format)))].join(" / ") })}</p>
                    )}
                    <button className="adv-toggle" aria-expanded={moreLocal} onClick={() => setMoreLocal(!moreLocal)}><ChevronDown size={12} className={moreLocal ? "flip" : ""} /> {t("其他模型")}</button>
                    {moreLocal && (
                      <>
                        <div className="model-list">{models.filter((m) => m.id !== "qwen3-asr").map(modelRow)}</div>
                        <button className="link-btn small" onClick={async () => { await api.rescanModels(); void refresh(); notify(t("已重新查找本机的识别模型")); }}><RefreshCw size={12} /> {t("重新查找本机已有的模型")}</button>
                      </>
                    )}
                  </>
                ) : (
                  <>
                    <div className="provider-tabs">
                      {ASR_SERVICES.filter((a) => moreCloud || CLOUD_ASR_MAIN.includes(a.id) || settings.asrEngine === a.id).map((a) => <button key={a.id} className={`chip ${settings.asrEngine === a.id ? "on" : ""}`} onClick={() => saveSettings({ asrEngine: a.id })}>{t(a.name)}</button>)}
                      {!moreCloud && <button className="chip" onClick={() => setMoreCloud(true)}>{t("更多")}</button>}
                    </div>
                    {settings.cloudAsr[settings.asrEngine] && <CloudAsrEditor key={settings.asrEngine} a={settings.cloudAsr[settings.asrEngine]} onChange={(na) => saveSettings({ cloudAsr: { ...settings.cloudAsr, [na.id]: na } }, true)} />}
                  </>
                )}
              </section>
            </>
          )}
          {section === "ai" && <ModelSettings ProviderEditor={ProviderEditor} />}
          {section === "tts" && <TtsSettings />}
          {section === "general" && (
            <>
              <section className="setup-block">
                <p className="setup-title">{t("界面")}</p>
                <label><span>{t("界面语言")}</span>
                  <select value={settings.uiLang} onChange={(e) => void saveSettings({ uiLang: e.target.value as LangSetting })}>
                    <option value="auto">{t("跟随系统（{name}）", { name: LANGS.find((l) => l.id === systemLang())?.name ?? "" })}</option>
                    {LANGS.map((l) => <option key={l.id} value={l.id}>{l.name}</option>)}
                  </select>
                </label>
                <label className="check"><input type="checkbox" checked={settings.youtubePromo} onChange={(e) => saveSettings({ youtubePromo: e.target.checked })} /> {t("显示节目的 YouTube 订阅按钮")}</label>
                {settings.youtubePromo && <label className="check"><input type="checkbox" checked={!settings.homeBannerHidden} onChange={(e) => saveSettings({ homeBannerHidden: !e.target.checked })} /> {t("在发现页顶部推荐「词元之外」")}</label>}
                <ClipToolsToggle />
                <label className="check"><input type="checkbox" checked={settings.sideCurated} onChange={(e) => saveSettings({ sideCurated: e.target.checked })} /> {t("在侧栏显示「乔木精读」推荐")}</label>
                {isChineseUi() && (
                  <>
                    <label className="check"><input type="checkbox" checked={!settings.translateShowNames} onChange={(e) => saveSettings({ translateShowNames: !e.target.checked })} /> {t("外文节目名不翻译")}</label>
                    <p className="muted small check-note">{t("侧栏和订阅列表保留原名，如 Lenny's Podcast。自己改过的名字不受影响。")}</p>
                  </>
                )}
              </section>
              <section className="setup-block">
                <p className="setup-title">{t("订阅")}</p>
                <div className="row-actions">
                  <button className="btn sm" onClick={() => { set({ addOpen: true, addTab: "import" }); }}>{t("导入订阅…")}</button>
                  <button className="btn sm" onClick={async () => { try { const path = await api.exportOpml(); notify(t("已导出到「下载」：{name}", { name: path.split(/[\\/]/).pop() ?? "" })); void revealItemInDir(path).catch(() => {}); } catch (e) { notify(errorText(e)); } }}>{t("导出 OPML")}</button>
                </div>
                <SyncSettings />
              </section>
              <section className="setup-block">
                <p className="setup-title">{t("网络")}</p>
                <label><span>{t("代理")}</span>
                  <div className="key-row">
                    <input {...NO_AUTO} value={proxy} onChange={(e) => setProxy(e.target.value)} placeholder={t("留空就用系统代理")} />
                    <button className="btn sm" onClick={async () => { try { const changed = proxy.trim() !== (settings.proxy ?? "").trim(); await saveSettings({ proxy }); notify(changed ? t("代理已保存。视频播放器重启 SubCast 后使用新代理") : t("代理已保存")); } catch (e) { notify(errorText(e)); } }}>{t("保存")}</button>
                  </div>
                </label>
                <NetworkCheck />
                <YouTubeAccount />
                <label><span>{t("YouTube 要求验证时")}</span>
                  <select value={settings.ytCookiesBrowser ?? ""} onChange={(e) => void saveSettings({ ytCookiesBrowser: e.target.value })}>
                    <option value="">{t("用 SubCast 里登录的账号，没登录就问我")}</option>
                    {(IS_MAC ? ["chrome", "safari", "edge", "firefox", "brave"] : ["chrome", "edge", "firefox", "brave"]).map((b) => <option key={b} value={b}>{t("借用 {name} 的 YouTube 登录", { name: b[0].toUpperCase() + b.slice(1) })}</option>)}
                  </select>
                </label>
              </section>
              <section className="setup-block" id="agent-connect">
                <p className="setup-title">{t("让 AI 助手调用 SubCast")}</p>
                <AgentConnectSettings />
              </section>
              <section className="setup-block">
                <p className="setup-title">{t("共享")}</p>
                <label className="check"><input type="checkbox" checked={settings.shareTranscripts} onChange={(e) => saveSettings({ shareTranscripts: e.target.checked })} /> {t("匿名分享我生成的字幕、译文和导读")}</label>
                <p className="muted small">{t("只分享文字，帮别人省去等待。关掉后照样能用别人分享的。")}</p>
              </section>
              <section className="setup-block">
                <button id="shortcuts-section" className="setting-row" aria-expanded={showKeys} onClick={() => setShowKeys(!showKeys)}><Keyboard size={15} /> <span>{t("快捷键")}</span><span className="muted small">{t("{key} 随时查看", { key: kbd("⌘/") })}</span><ChevronDown size={14} className={showKeys ? "flip" : ""} /></button>
                {showKeys && <ShortcutSettings />}
                <button id="storage-section" className="setting-row" aria-expanded={showStorage} onClick={() => setShowStorage(!showStorage)}><HardDrive size={15} /> <span>{t("存储")}</span><StorageTotal rev={storageRev} /><ChevronDown size={14} className={showStorage ? "flip" : ""} /></button>
                {showStorage && <Storage onAsr={() => setSection("subtitles")} onTts={() => setSection("tts")} onChanged={() => setStorageRev((n) => n + 1)} />}
                <button className="setting-row" aria-expanded={showCli} onClick={() => setShowCli(!showCli)}><Terminal size={15} /> <span>{t("命令行工具（CLI）")}</span><span className="muted small">subcast</span><ChevronDown size={14} className={showCli ? "flip" : ""} /></button>
                {showCli && <CliSettings />}
              </section>
            </>
          )}
          {section === "learn" && <LearnSettings />}
          {section === "about" && <About />}
        </div>
      </div>
    </div>
  );
}

function CloudAsrEditor({ a, onChange }: { a: CloudAsr; onChange: (a: CloudAsr) => void }) {
  return (
    <div className="provider-form">
      <p className="muted small">{t("{note}。识别时音频会发送到 {name}。", { note: t(a.note), name: t(a.name) })}</p>
      <label><span>{t("接口地址")}</span><input {...NO_AUTO} value={a.baseUrl} onChange={(e) => onChange({ ...a, baseUrl: e.target.value })} /></label>
      <label><span>{t("模型")}</span>
        <input {...NO_AUTO} value={a.model} list={`asr-${a.id}`} onChange={(e) => onChange({ ...a, model: e.target.value })} />
        <datalist id={`asr-${a.id}`}>{a.models.map((m) => <option key={m} value={m} />)}</datalist>
      </label>
      {a.models.length > 1 && <div className="model-chips">{a.models.map((m) => <button key={m} className={`chip sm ${a.model === m ? "on" : ""}`} onClick={() => onChange({ ...a, model: m })}>{m}</button>)}</div>}
      {a.needsKey && <label><span>API Key</span><KeyField account={`asr.${a.id}`} shared={a.protocol === "deepseek" ? { account: "deepseek", name: "DeepSeek" } : undefined} /></label>}
      <div className="row-actions">
        <span className="spacer" />
        {a.keyUrl && <button className="btn ghost sm" onClick={() => openUrl(a.keyUrl!)}><ExternalLink size={13} /> {t("获取 Key")}</button>}
      </div>
    </div>
  );
}

function UpdateButton() {
  const { phase } = useUpdate();
  if (phase === "available") return <button className="btn primary sm" onClick={() => void installUpdate()}><RefreshCw size={13} /> {t("更新并重启")}</button>;
  if (phase === "ready") return <button className="btn primary sm" onClick={() => void installUpdate()}><RefreshCw size={13} /> {t("立即重启以更新")}</button>;
  const busy = phase === "checking" || phase === "downloading" || phase === "installing";
  return <button className="btn sm" disabled={busy} onClick={() => void checkForUpdate(true)}>{busy ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />} {t("检查更新")}</button>;
}

function UpdateStatus() {
  const { phase, version, progress, error } = useUpdate();
  const { settings, saveSettings } = useStore(useShallow((s) => ({ settings: s.settings, saveSettings: s.saveSettings })));
  const line = phase === "latest" ? <p className="ok-text small">{t("已经是最新版本")}</p>
    : phase === "available" ? <p className="small">{t("SubCast {version} 可以更新了", { version: version ?? "" })}</p>
    : phase === "ready" ? <p className="small">{t("SubCast {version} 已下载好，重启即可完成更新", { version: version ?? "" })}</p>
    : phase === "downloading" ? <p className="muted small">{progress == null ? t("正在下载新版本…") : t("正在下载新版本 {pct}%", { pct: Math.round(progress * 100) })}</p>
    : phase === "error" ? <p className="error-text small">{t("检查更新失败：{error}", { error: error ?? "" })} <button className="link-btn" onClick={() => void openUrl(DOWNLOAD_PAGE)}>{t("去官网下载")}</button></p>
    : null;
  return (
    <>
      {line}
      <label className="check small-check"><input type="checkbox" checked={settings.autoUpdate} onChange={(e) => saveSettings({ autoUpdate: e.target.checked })} /> {t("自动下载新版本，重启后生效")}</label>
    </>
  );
}

const REPO = "https://github.com/joeseesun/subcast";
/** "占用 1.9 GB" for the 通用 page's storage row. */
/** Highlights and clip-making: on for people who make clips, out of the way for the rest. */
function ClipToolsToggle() {
  const on = useClipTools();
  const saveSettings = useStore((s) => s.saveSettings);
  return (
    <>
      <label className="check"><input type="checkbox" checked={on} onChange={(e) => void saveSettings({ clipTools: e.target.checked })} /> {t("显示高光与剪视频功能")}</label>
      <p className="muted small check-note">{t("字幕旁的「高光」、进度条上的高光标记、选中文字和 AI 回答里的「剪视频」。")}</p>
    </>
  );
}

/** Where highlight clips are saved: the Downloads folder unless the user picks another. */
function ClipExportRow() {
  const { dir, saveSettings } = useStore(useShallow((s) => ({ dir: s.settings.clipExportDir ?? "", saveSettings: s.saveSettings })));
  const pick = async () => {
    const chosen = await openDialog({ directory: true, multiple: false, defaultPath: dir || undefined });
    if (typeof chosen === "string" && chosen) void saveSettings({ clipExportDir: chosen });
  };
  return (
    <div className="storage-row">
      <div className="storage-row-text">
        <p className="storage-row-name">{t("高光剪辑导出到")}</p>
        <p className="muted small clamp1" title={dir || undefined}>{dir || t("「下载」文件夹")}</p>
      </div>
      <button className="btn sm" onClick={() => void pick()}><FolderOpen size={13} /> {t("选择文件夹")}</button>
      {dir && <button className="btn ghost sm" onClick={() => void api.revealFile(dir).catch(() => {})}>{t("打开")}</button>}
      {dir && <button className="btn ghost sm" onClick={() => void saveSettings({ clipExportDir: "" })}>{t("恢复默认")}</button>}
    </div>
  );
}

function StorageTotal({ rev }: { rev: number }) {
  const saved = useOffline((s) => s.saved);
  const [total, setTotal] = useState<number | null>(null);
  useEffect(() => { void api.storageInfo().then((i) => setTotal(i.downloads + i.models + i.library + i.temp + (i.audioCache ?? 0) + (i.ttsCache ?? 0) + (i.ttsModels ?? 0))).catch(() => {}); }, [rev, saved]);
  return <span className="muted small">{total == null ? "" : t("占用 {size}", { size: bytes(total) })}</span>;
}

function Storage({ onAsr, onTts, onChanged }: { onAsr: () => void; onTts: () => void; onChanged: () => void }) {
  const clipTools = useClipTools();
  const go = useStore((s) => s.go);
  const notify = useStore((s) => s.notify);
  const saved = useOffline((s) => s.saved);
  const [info, setInfo] = useState<{ downloads: number; models: number; library: number; temp: number; audioCache: number; ttsCache?: number; ttsModels?: number } | null>(null);
  const load = () => { void api.storageInfo().then(setInfo).catch(() => {}); onChanged(); };
  useEffect(() => { void api.storageInfo().then(setInfo).catch(() => {}); }, [saved]);
  const n = Object.keys(saved).length;
  const size = (b?: number) => (info ? (b ? bytes(b) : "0 MB") : "…");
  const clearAll = async () => {
    if (!(await ask({ title: t("删除全部已下载的音频？"), message: t("字幕、精读和收听进度都会保留。"), ok: t("全部删除"), danger: true }))) return;
    await clearDownloads(); load(); notify(t("已清理"));
  };
  return (
    <>
      <div className="storage-list">
        <div className="storage-row">
          <div className="storage-row-text">
            <p className="storage-row-name">{t("离线下载")}</p>
            <p className="muted small">{n ? t("{n} 集音频，没有网络也能听", { n }) : t("在单集页点下载按钮，就能离线收听")}</p>
          </div>
          <span className="storage-size">{size(info?.downloads)}</span>
          <button className="btn sm" onClick={() => void api.openDataFolder("downloads")}><FolderOpen size={13} /> {t("打开文件夹")}</button>
          {n > 0 && <button className="btn ghost sm" onClick={() => go({ kind: "downloads" })}>{t("查看")}</button>}
          {n > 0 && <button className="icon-btn" title={t("全部删除")} onClick={clearAll}><Trash2 size={14} /></button>}
        </div>
        {clipTools && <ClipExportRow />}
        <div className="storage-row">
          <div className="storage-row-text">
            <p className="storage-row-name">{t("语音识别模型")}</p>
            <p className="muted small">{t("本机识别用的模型，在「语音识别」里下载或删除")}</p>
          </div>
          <span className="storage-size">{size(info?.models)}</span>
          <button className="btn sm" onClick={() => void api.openDataFolder("models")}><FolderOpen size={13} /> {t("打开文件夹")}</button>
          <button className="btn ghost sm" onClick={onAsr}>{t("管理")}</button>
        </div>
        <div className="storage-row">
          <div className="storage-row-text">
            <p className="storage-row-name">{t("节目库")}</p>
            <p className="muted small">{t("订阅、字幕、翻译、精读和收听进度")}</p>
          </div>
          <span className="storage-size">{size(info?.library)}</span>
          <button className="btn ghost sm" onClick={() => void api.openDataFolder("data")}><FolderOpen size={13} /> {t("打开文件夹")}</button>
        </div>
        {!!info?.audioCache && (
          <div className="storage-row">
            <div className="storage-row-text">
              <p className="storage-row-name">{t("字幕对应的音频")}</p>
              <p className="muted small">{t("识别字幕或对齐字幕时留下的音频，播放时优先用它，字幕和声音更准；最多占 2 GB，久不听的先删")}</p>
            </div>
            <span className="storage-size">{size(info.audioCache)}</span>
            <button className="btn sm" onClick={() => void clearAudioCache().then(() => { load(); notify(t("已清理")); })}>{t("清理")}</button>
          </div>
        )}
        {!!info?.ttsModels && (
          <div className="storage-row">
            <div className="storage-row-text">
              <p className="storage-row-name">{t("本地语音模型")}</p>
              <p className="muted small">{t("Melo、Kokoro 离线朗读用的模型，在「朗读」里下载或删除")}</p>
            </div>
            <span className="storage-size">{size(info.ttsModels)}</span>
            <button className="btn sm" onClick={() => void api.openDataFolder("tts-models")}><FolderOpen size={13} /> {t("打开文件夹")}</button>
            <button className="btn ghost sm" onClick={onTts}>{t("管理")}</button>
          </div>
        )}
        {!!info?.ttsCache && (
          <div className="storage-row">
            <div className="storage-row-text">
              <p className="storage-row-name">{t("朗读缓存")}</p>
              <p className="muted small">{t("朗读精读和字幕中文翻译生成的语音缓存；最多占 500 MB，久不用的先删")}</p>
            </div>
            <span className="storage-size">{size(info.ttsCache)}</span>
            <button className="btn ghost sm" onClick={() => void api.openDataFolder("tts-cache")}><FolderOpen size={13} /> {t("打开文件夹")}</button>
            <button className="btn sm" onClick={() => void api.clearTtsCache().then(() => { load(); notify(t("已清理")); })}>{t("清理")}</button>
          </div>
        )}
        {!!info?.temp && (
          <div className="storage-row">
            <div className="storage-row-text">
              <p className="storage-row-name">{t("临时文件")}</p>
              <p className="muted small">{t("中断的识别留下的音频，可以放心清理")}</p>
            </div>
            <span className="storage-size">{size(info.temp)}</span>
            <button className="btn sm" onClick={() => void api.clearTemp().then(() => { load(); notify(t("已清理")); })}>{t("清理")}</button>
          </div>
        )}
      </div>
    </>
  );
}

function CliSettings() {
  const notify = useStore((s) => s.notify);
  const [status, setStatus] = useState<CliStatus | null>(null);
  const [busy, setBusy] = useState(false);

  const load = () => { void api.cliStatus().then(setStatus).catch(() => {}); };
  useEffect(load, []);

  const install = async () => {
    setBusy(true);
    try {
      const next = await api.cliInstall();
      setStatus(next);
      notify(next.pathAdded ? t("已安装 subcast 命令，并加入了 PATH（{file}）。新开一个终端窗口就能用", { file: next.pathAdded }) : t("已安装 subcast 命令"));
    } catch (e) {
      notify(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  if (!status) return <p className="muted small">{t("正在检查…")}</p>;
  const badge = !status.bundled
    ? t("仅发布版附带")
    : status.installed && !status.needsUpdate
      ? t("已安装")
      : status.needsUpdate
        ? t("需要更新链接")
        : t("未安装");

  return (
    <div className="provider-form">
      <p className="muted small">{t("在终端或 AI Agent 中通过 subcast 命令管理订阅、转写、翻译、导读、切片与生词本。")}</p>
      <p className="small"><span>{t("状态")}：{badge}</span>{status.linkPath ? ` · ${status.linkPath}` : ""}</p>
      {status.installed && !status.inPath && status.linkPath && (
        <p className="muted small">
          {t("新开的终端还找不到 subcast 命令（{dir} 不在 PATH 里）。", { dir: status.linkPath.replace(/[/\\][^/\\]+$/, "") })}{" "}
          <button className="link-btn" disabled={busy} onClick={install}>{t("自动加入 PATH")}</button>
        </p>
      )}
      {status.bundled && (
        <div className="row-actions">
          <button className="btn sm" disabled={busy || (status.installed && !status.needsUpdate)} onClick={install}>
            {busy ? <Loader2 size={13} className="spin" /> : <Terminal size={13} />}
            {status.needsUpdate ? t("更新链接") : status.installed ? t("已就绪") : t("安装 subcast 命令")}
          </button>
        </div>
      )}
    </div>
  );
}

function About() {
  const [version, setVersion] = useState("0.1.0");
  const uiBuild = useStore((s) => s.uiBuild);
  useEffect(() => { getVersion().then(setVersion).catch(() => {}); }, []);
  const links: [string, string][] = [["qiaomu.ai", "https://qiaomu.ai"], [t("博客"), "https://blog.qiaomu.ai"], [t("乔木推荐"), "https://tuijian.qiaomu.ai"], ["X", "https://x.com/vista8"], ["GitHub", "https://github.com/joeseesun"]];
  return (
    <div className="about">
      <div className="about-top">
        <img src={logoUrl} alt="" />
        <div className="about-meta">
          <p className="about-name">SubCast <span className="about-ver">{t("版本 {version}", { version })}{uiBuild ? ` · ${t("界面更新 {n}", { n: uiBuild })}` : ""}</span></p>
          <p className="muted small">{t("海外好播客，配上中文字幕、精读与 AI 问答。")}</p>
          <UpdateStatus />
        </div>
      </div>
      <div className="row-actions">
        <UpdateButton />
        <button className="btn sm" onClick={() => openUrl(`${REPO}/issues/new`)}><MessageCircle size={13} /> {t("反馈问题")}</button>
        <button className="btn sm" onClick={() => openUrl(`${REPO}#readme`)}><BookOpen size={13} /> {t("使用说明")}</button>
        <button className="btn sm" onClick={() => openUrl(REPO)}><Github size={13} /> {t("源代码")}</button>
      </div>
      <HomeShowCard />
      <div className="about-qr">
        <div className="qr-card"><img src={wechatQr} alt={t("公众号二维码")} /><div><p className="qr-title">{t("关注公众号")}</p><p className="muted small">{t("微信搜索「向阳乔木推荐看」")}</p></div></div>
        <div className="qr-card"><img src={rewardQr} alt={t("赞赏码")} /><div><p className="qr-title">{t("请我喝杯咖啡")}</p><p className="muted small">{t("微信扫码赞赏")}</p></div></div>
      </div>
      <div className="about-foot">
        {links.map(([label, u]) => <button key={u} className="link-btn" onClick={() => openUrl(u)}>{label}</button>)}
        <span className="spacer" />
        <span className="muted small">{t("© {year} 向阳乔木 · AGPL-3.0", { year: new Date().getFullYear() })}</span>
      </div>
    </div>
  );
}
