import { useEffect, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { Check, ChevronDown, Cloud, Download, ExternalLink, Laptop, Lock, Square, Trash2, Volume2 } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, type SystemVoice } from "../lib/api";
import { TTS_CLOUD_MAIN, TTS_SERVICES, type TtsService } from "../lib/settings";
import { currentTtsService, listWebSystemVoices, previewVoice, stopSpeaking, useTtsPreview } from "../lib/tts";
import { useStore } from "../store";
import { t, tk, tr } from "../lib/i18n";
import { bytes } from "../lib/offline";
import { ask } from "../lib/confirm";
import {
  cancelLocalTts, deleteLocalTts, installLocalTts, localInstalled, refreshLocalTts, useLocalTts, type LocalModelId,
} from "../lib/localTts";
import { KeyField, NO_AUTO } from "./KeyField";
import { RateControl } from "./RatePicker";


/** One preview button for the active engine + voice. */
export function PreviewButton({ engine, voice }: { engine: string; voice: string }) {
  const active = useTtsPreview((s) => s.active);
  const notify = useStore((s) => s.notify);
  const key = `${engine}:${voice}`;
  const playing = active === key;
  return (
    <button
      className="btn ghost sm"
      onClick={() => void previewVoice(engine, voice, (m) => notify(tr(m)))}
    >
      {playing ? <Square size={11} /> : <Volume2 size={13} />}
      {playing ? t("停止试听") : t("试听")}
    </button>
  );
}

/** Shared hook to load installed OS voices (preferring Web Speech so macOS Premium / Enhanced / Siri voices appear). */
export function useSystemVoices(enabled: boolean): SystemVoice[] {
  const [voices, setVoices] = useState<SystemVoice[]>([]);
  useEffect(() => {
    if (!enabled) return;
    let alive = true;
    const load = () => {
      const web = listWebSystemVoices();
      if (web.length) {
        if (alive) setVoices(web);
        return true;
      }
      return false;
    };
    if (!load()) {
      void api.ttsSystemVoices().then((v) => { if (alive && !listWebSystemVoices().length) setVoices(v); }).catch(() => {});
    }
    const synth = typeof window !== "undefined" ? window.speechSynthesis : undefined;
    const onChange = () => { load(); };
    synth?.addEventListener?.("voiceschanged", onChange);
    return () => {
      alive = false;
      synth?.removeEventListener?.("voiceschanged", onChange);
    };
  }, [enabled]);
  return voices;
}

/** The AI-model key each service falls back to when it has none of its own (mirrors speech.rs resolve_key). */
export const SHARED_KEY: Record<string, { account: string; name: string }> = {
  deepseek: { account: "deepseek", name: "DeepSeek" },
  mimo: { account: "mimo", name: tk("小米 MiMo") },
  qwen: { account: "qwen-cn", name: tk("通义千问（百炼）") },
  openai: { account: "openai", name: "OpenAI" },
};

function CloudTtsEditor({
  svc,
  onChange,
}: {
  svc: TtsService;
  onChange: (next: TtsService, quiet?: boolean) => void;
}) {
  const [customVoice, setCustomVoice] = useState(
    svc.voices.length === 0 || !svc.voices.some((v) => v.id === svc.voice),
  );
  const [advanced, setAdvanced] = useState(svc.id === "custom");

  return (
    <div className="provider-form">
      <p className="muted small">
        {t("{note}。朗读时文字会发送到 {name}。", { note: t(svc.note), name: t(svc.name) })}
      </p>

      <div className="setup-head">
        <p className="setup-title">{t("音色")}</p>
        <div className="row-actions">
          {svc.voices.length > 0 && (
            <button className="link-btn small" onClick={() => setCustomVoice(!customVoice)}>
              {t("自定义音色")}
            </button>
          )}
          <PreviewButton engine={svc.id} voice={svc.voice} />
        </div>
      </div>

      {svc.voices.length > 0 && (
        <div className="model-chips">
          {svc.voices.map((v) => (
            <button
              key={v.id}
              className={`chip sm ${svc.voice === v.id ? "on" : ""}`}
              onClick={() => {
                setCustomVoice(false);
                onChange({
                  ...svc,
                  voice: v.id,
                  ...(svc.protocol === "doubao" ? { resource: "seed-tts-2.0" } : {}),
                });
              }}
            >
              {t(v.name)}
            </button>
          ))}
        </div>
      )}

      {customVoice && (
        <>
          <input
            {...NO_AUTO}
            value={svc.voice}
            onChange={(e) => onChange({ ...svc, voice: e.target.value.trim() }, true)}
            placeholder={svc.protocol === "doubao" ? t("复刻音色或其他音色的 ID") : t("输入音色 ID")}
          />
          {svc.protocol === "doubao" && (
            <label className="check">
              <input
                type="checkbox"
                checked={svc.resource === "seed-icl-2.0"}
                onChange={(e) => onChange({ ...svc, resource: e.target.checked ? "seed-icl-2.0" : "seed-tts-2.0" })}
              />{" "}
              {t("这是我复刻的声音")}
            </label>
          )}
        </>
      )}

      {(svc.needsKey || svc.id === "custom") && (
        <div className="setup-block">
          <div className="setup-head">
            <p className="setup-title">API Key</p>
            {svc.keyUrl && (
              <button className="link-btn small" onClick={() => openUrl(svc.keyUrl!)}>
                <ExternalLink size={11} /> {t("获取 Key")}
              </button>
            )}
          </div>
          <KeyField
            account={`tts.${svc.id}`}
            shared={SHARED_KEY[svc.id]}
            placeholder={
              svc.sharedKeyHint
                ? t(svc.sharedKeyHint)
                : svc.needsKey
                  ? undefined
                  : t("可选，本地服务可留空")
            }
          />
        </div>
      )}

      {svc.protocol !== "edge" && (
        <>
          <div className="row-actions">
            <span className="spacer" />
            <button className="adv-toggle" onClick={() => setAdvanced(!advanced)}>
              <ChevronDown size={12} className={advanced ? "flip" : ""} /> {t("高级设置")}
            </button>
          </div>

          {advanced && (
            <div className="adv-body">
              <label>
                <span>{t("接口地址")}</span>
                <input
                  {...NO_AUTO}
                  value={svc.baseUrl}
                  onChange={(e) => onChange({ ...svc, baseUrl: e.target.value }, true)}
                  placeholder="https://…/v1"
                />
              </label>
              {svc.protocol !== "doubao" && svc.protocol !== "deepseek" && (
                <>
                  <label>
                    <span>{t("模型")}</span>
                    <input
                      {...NO_AUTO}
                      value={svc.model}
                      list={`tts-model-${svc.id}`}
                      onChange={(e) => onChange({ ...svc, model: e.target.value }, true)}
                      placeholder={t("模型名")}
                    />
                    <datalist id={`tts-model-${svc.id}`}>
                      {svc.models.map((m) => <option key={m} value={m} />)}
                    </datalist>
                  </label>
                  {svc.models.length > 1 && (
                    <div className="model-chips">
                      {svc.models.map((m) => (
                        <button
                          key={m}
                          className={`chip sm ${svc.model === m ? "on" : ""}`}
                          onClick={() => onChange({ ...svc, model: m })}
                        >
                          {m}
                        </button>
                      ))}
                    </div>
                  )}
                </>
              )}
            </div>
          )}
        </>
      )}
    </div>
  );
}

/** The offline choices: the OS voice, plus Melo and Kokoro (downloaded on first use). */
const LOCAL_MODELS: { id: LocalModelId; name: string; note: string }[] = [
  { id: "melo", name: "Melo", note: tk("中英混读，一个女声，最快") },
  { id: "kokoro", name: "Kokoro", note: tk("8 种中文男女声，英文也能读") },
];

/** Whether this engine id is one of the offline voices shown under 「本地语音」. */
export const isLocalEngine = (id: string) => id === "local" || id === "system";

/** One selectable model card. A div (not a button) so download / delete buttons can sit inside it. */
function VoiceCard({
  on, name, note, onSelect, children,
}: { on: boolean; name: string; note: string; onSelect: () => void; children: React.ReactNode }) {
  return (
    <div
      role="radio"
      aria-checked={on}
      tabIndex={0}
      className={`voice-card ${on ? "on" : ""}`}
      onClick={onSelect}
      onKeyDown={(e) => { if (e.target === e.currentTarget && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); onSelect(); } }}
    >
      <p className="voice-card-name">{name}</p>
      <p className="voice-card-note">{note}</p>
      <div className="voice-card-status">{children}</div>
    </div>
  );
}

function LocalTtsEditor({ onPick }: { onPick: (engine: "local" | "system", model?: LocalModelId) => void }) {
  const { settings, saveSettings } = useStore(
    useShallow((s) => ({ settings: s.settings, saveSettings: s.saveSettings })),
  );
  const { status, downloading } = useLocalTts(useShallow((s) => ({ status: s.status, downloading: s.downloading })));
  const notify = useStore((s) => s.notify);
  const [moreSysVoices, setMoreSysVoices] = useState(false);
  const active = currentTtsService(settings);
  const local = settings.ttsServices.local;
  const sysVoices = useSystemVoices(true);
  const onSystem = active.id === "system";
  const model = (local.model === "kokoro" ? "kokoro" : "melo") as LocalModelId;
  const supported = status?.supported ?? true;

  useEffect(() => { void refreshLocalTts(); }, []);

  const setLocal = (patch: Partial<typeof local>) =>
    void saveSettings({ ttsServices: { ...settings.ttsServices, local: { ...local, ...patch } } });
  const setSystemVoice = (voice: string) =>
    void saveSettings({ ttsServices: { ...settings.ttsServices, system: { ...settings.ttsServices.system, voice } } });

  const zhSysVoices = sysVoices.filter((v) => v.lang.toLowerCase().startsWith("zh"));
  const otherSysVoices = sysVoices.filter((v) => !v.lang.toLowerCase().startsWith("zh"));
  const shownSysVoices = moreSysVoices || zhSysVoices.length === 0 ? sysVoices : zhSysVoices;

  const remove = async (id: LocalModelId) => {
    if (!(await ask({ title: t("删除这个本地语音模型？"), message: t("以后再用需要重新下载。"), ok: t("删除"), danger: true }))) return;
    await deleteLocalTts(id);
  };

  const modelStatus = (id: LocalModelId) => {
    const st = status?.models.find((m) => m.id === id);
    const dl = downloading?.id === id ? downloading : null;
    if (dl) {
      const pct = dl.total ? Math.round((dl.received / dl.total) * 100) : 0;
      return (
        <>
          <div className="voice-card-bar"><div style={{ width: `${pct}%` }} /></div>
          <span className="num">{pct}%</span>
          <button className="link-btn small" onClick={(e) => { e.stopPropagation(); void cancelLocalTts(id); }}>{t("取消")}</button>
        </>
      );
    }
    if (localInstalled(status, id)) {
      return (
        <>
          <span className="voice-card-ok"><Check size={12} /> {t("已下载")}{st ? ` · ${bytes(st.bytes)}` : ""}</span>
          <button
            className="icon-btn sm"
            title={t("删除")}
            aria-label={t("删除")}
            onClick={(e) => { e.stopPropagation(); void remove(id); }}
          >
            <Trash2 size={13} />
          </button>
        </>
      );
    }
    return (
      <>
        <span>{st ? t("约 {size}", { size: bytes(st.bytes + (status?.runtime ? 0 : 22 * 1024 * 1024)) }) : t("首次使用需要下载")}</span>
        <button
          className="btn sm"
          disabled={downloading != null}
          onClick={(e) => {
            e.stopPropagation();
            onPick("local", id);
            if (useLocalTts.getState().downloading?.id !== id) void installLocalTts(id).then((ok) => ok && notify(t("本地语音已就绪")));
          }}
        >
          <Download size={12} /> {t("下载")}
        </button>
      </>
    );
  };

  const voiceChips = onSystem || (active.id === "local" && model === "kokoro");

  return (
    <div className="provider-form">
      <div className="setup-head">
        <p className="setup-title">{t("模型")}</p>
        <PreviewButton engine={active.id} voice={active.voice} />
      </div>
      <div className="voice-cards" role="radiogroup" aria-label={t("模型")}>
        <VoiceCard
          on={onSystem}
          name={t("系统增强语音")}
          note={t("系统自带的增强音色，开箱即用")}
          onSelect={() => onPick("system")}
        >
          <span className="voice-card-ok"><Check size={12} /> {t("系统自带，无需下载")}</span>
        </VoiceCard>
        {supported && LOCAL_MODELS.map((m) => (
          <VoiceCard
            key={m.id}
            on={active.id === "local" && model === m.id}
            name={m.name}
            note={t(m.note)}
            onSelect={() => onPick("local", m.id)}
          >
            {modelStatus(m.id)}
          </VoiceCard>
        ))}
      </div>

      {voiceChips && (
        <div className="voice-pick">
          <p className="setup-title">{t("音色")}</p>
          {onSystem ? (
            <>
              <div className="model-chips">
                <button className={`chip sm ${!active.voice ? "on" : ""}`} onClick={() => setSystemVoice("")}>
                  {t("系统默认")}
                </button>
                {shownSysVoices.map((v) => (
                  <button
                    key={v.id}
                    className={`chip sm ${active.voice === v.id ? "on" : ""}`}
                    onClick={() => setSystemVoice(v.id)}
                  >
                    {v.name} <span className="muted">({v.lang})</span>
                  </button>
                ))}
              </div>
              {otherSysVoices.length > 0 && zhSysVoices.length > 0 && (
                <button className="adv-toggle" onClick={() => setMoreSysVoices(!moreSysVoices)}>
                  <ChevronDown size={12} className={moreSysVoices ? "flip" : ""} />{" "}
                  {moreSysVoices ? t("收起其他语言音色") : t("其他语言音色（{n}）", { n: otherSysVoices.length })}
                </button>
              )}
            </>
          ) : (
            <div className="model-chips">
              {local.voices.map((v) => (
                <button
                  key={v.id}
                  className={`chip sm ${local.voice === v.id ? "on" : ""}`}
                  onClick={() => setLocal({ voice: v.id })}
                >
                  {t(v.name)}
                </button>
              ))}
            </div>
          )}
        </div>
      )}

      <p className="privacy-note"><Lock size={11} /> {t("完全在本机离线合成，文字不会离开你的电脑。")}</p>
    </div>
  );
}

export function TtsSettings() {
  const { settings, saveSettings } = useStore(
    useShallow((s) => ({ settings: s.settings, saveSettings: s.saveSettings })),
  );
  const [moreCloud, setMoreCloud] = useState(false);
  const active = currentTtsService(settings);
  const isCloud = active.group === "cloud";
  const lastCloudId = isCloud ? active.id : "doubao";

  useEffect(() => () => stopSpeaking(), []);

  const updateService = (next: TtsService, quiet = false) => {
    const patch: Partial<typeof settings> = {
      ttsServices: { ...settings.ttsServices, [next.id]: next },
    };
    if (next.id === "doubao") {
      patch.ttsVoice = next.voice;
      patch.ttsResource = next.resource ?? "seed-tts-2.0";
    }
    void saveSettings(patch, quiet);
  };

  /** Select Melo / Kokoro to inspect voices or use once downloaded; download stays on the explicit button. */
  const pickLocal = (engine: "local" | "system", model?: LocalModelId) => {
    if (engine === "local" && model) {
      const local = settings.ttsServices.local;
      void saveSettings({ ttsEngine: "local", ttsServices: { ...settings.ttsServices, local: { ...local, model } } });
    } else {
      void saveSettings({ ttsEngine: engine });
    }
  };
  const localOn = isLocalEngine(active.id);
  const lastLocal = settings.ttsEngine === "system" ? "system" : "local";

  const cloudServices = TTS_SERVICES.filter((s) => s.group === "cloud");

  return (
    <>
      <section className="setup-block">
        <p className="setup-title">{t("朗读引擎")}</p>
        <p className="muted small">{t("用于精读和字幕中文翻译的朗读。")}</p>
        <div className="engine-switch">
          <button
            className={`engine ${localOn ? "on" : ""}`}
            onClick={() => void saveSettings({ ttsEngine: localOn ? active.id : lastLocal })}
          >
            <Laptop size={18} />
            <div>
              <p>{t("本地语音")}</p>
              <p className="muted small">{t("优先系统增强，也支持 Melo / Kokoro")}</p>
            </div>
          </button>
          <button
            className={`engine ${isCloud ? "on" : ""}`}
            onClick={() => void saveSettings({ ttsEngine: lastCloudId })}
          >
            <Cloud size={18} />
            <div>
              <p>{t("云端服务")}</p>
              <p className="muted small">{t("豆包、DeepSeek、MiMo、Edge（免 Key）等")}</p>
            </div>
          </button>
        </div>

        {localOn && <LocalTtsEditor onPick={pickLocal} />}

        {isCloud && (
          <>
            <div className="provider-tabs">
              {cloudServices
                .filter((s) => moreCloud || TTS_CLOUD_MAIN.includes(s.id) || active.id === s.id)
                .map((s) => (
                  <button
                    key={s.id}
                    className={`chip ${active.id === s.id ? "on" : ""}`}
                    onClick={() => void saveSettings({ ttsEngine: s.id })}
                  >
                    {t(s.name)}
                  </button>
                ))}
              {!moreCloud && (
                <button className="chip" onClick={() => setMoreCloud(true)}>
                  {t("更多")}
                </button>
              )}
            </div>
            <CloudTtsEditor key={active.id} svc={active} onChange={updateService} />
          </>
        )}
      </section>

      <section className="setup-block">
        <p className="setup-title">{t("朗读语速")}</p>
        <RateControl />
        <p className="muted small">{t("朗读时也可以在朗读按钮旁随时调整。")}</p>
      </section>
    </>
  );
}
