// Cross-device sync settings inside Settings › 通用 › 订阅: iCloud Drive and WebDAV.
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useEffect, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { Check, Cloud, FolderOpen, Globe, Loader2, RefreshCw } from "lucide-react";
import { api, errorText, type BackendSyncSummary } from "../lib/api";
import { IS_MAC, t, tr } from "../lib/i18n";
import { DEFAULT_SYNC, type SyncSettings as SyncCfg } from "../lib/settings";
import { useSyncState } from "../lib/sync";
import { useStore } from "../store";
import { KeyRow, NO_AUTO } from "./KeyField";

const WEBDAV_ACCOUNT = "sync.webdav";

const WEBDAV_PRESETS: { id: string; label: string; url: string }[] = [
  { id: "jianguoyun", label: "坚果云", url: "https://dav.jianguoyun.com/dav/" }, // i18n-ignore
  { id: "nextcloud", label: "Nextcloud", url: "https://your-server/remote.php/dav/files/username/" },
  { id: "infinicloud", label: "InfiniCLOUD", url: "https://your-host.teracloud.jp/dav/" },
];

function formatSyncTime(ms: number): string {
  const diff = Math.max(0, Date.now() - ms);
  if (diff < 60_000) return t("刚刚");
  if (diff < 3_600_000) return t("{n} 分钟前", { n: Math.floor(diff / 60_000) });
  if (diff < 86_400_000) return t("{n} 小时前", { n: Math.floor(diff / 3_600_000) });
  return new Date(ms).toLocaleDateString();
}

function SyncSummaryLine({ summary }: { summary?: BackendSyncSummary | null }) {
  if (!summary) return null;
  if (summary.error) {
    return <p className="sync-status-line error-text small">{t("上次同步失败：{err}", { err: tr(summary.error) })}</p>;
  }
  const parts: string[] = [];
  if (summary.addedShows > 0) parts.push(t("新增 {n} 个订阅", { n: summary.addedShows }));
  if (summary.removedShows > 0) parts.push(t("移除 {n} 个订阅", { n: summary.removedShows }));
  if (summary.updatedEpisodes > 0) parts.push(t("更新 {n} 集进度", { n: summary.updatedEpisodes }));
  if ((summary.updatedContent ?? 0) > 0) parts.push(t("更新 {n} 集导读/问答", { n: summary.updatedContent }));
  const detail = parts.length ? ` · ${parts.join(" · ")}` : summary.uploaded ? ` · ${t("已上传到云端")}` : "";
  return (
    <p className="sync-status-line muted small">
      <Check size={12} className="ok-text" /> {t("上次同步：{time}", { time: formatSyncTime(summary.syncedAt) })}{detail}
    </p>
  );
}

export function SyncSettings() {
  const { settings, saveSettings, notify } = useStore(useShallow((s) => ({
    settings: s.settings,
    saveSettings: s.saveSettings,
    notify: s.notify,
  })));
  const { busy, status, refreshStatus, run } = useSyncState(useShallow((s) => ({
    busy: s.busy,
    status: s.status,
    refreshStatus: s.refreshStatus,
    run: s.run,
  })));
  const sync: SyncCfg = { ...DEFAULT_SYNC, ...(settings.sync ?? {}) };
  const [tab, setTab] = useState<"icloud" | "webdav">(() =>
    sync.webdavEnabled && !sync.icloudEnabled ? "webdav" : IS_MAC ? "icloud" : "webdav",
  );

  useEffect(() => {
    void refreshStatus(sync.icloudDir);
  }, [sync.icloudDir, refreshStatus]);

  const patchSync = (patch: Partial<SyncCfg>, lazy = false) =>
    saveSettings({ sync: { ...sync, ...patch } }, lazy);

  const icloud = status?.icloud;
  const icloudBadge = !icloud
    ? ""
    : !icloud.available
      ? t("不可用")
      : sync.icloudEnabled
        ? t("已开启")
        : t("未开启");
  const webdavBadge = sync.webdavEnabled ? t("已开启") : t("未开启");

  return (
    <div id="sync-section" className="sync-box">
      <div className="sync-head">
        <p className="sync-sub-title">{t("跨设备同步（与移动版互通）")}</p>
        <p className="muted small">{t("在桌面版与移动版之间同步订阅列表、自定义节目名、播放进度以及导读、精读与问答。")}</p>
      </div>
      <div className="engine-switch sync-cards">
        <button
          type="button"
          className={`engine ${tab === "icloud" ? "on" : ""}`}
          onClick={() => setTab("icloud")}
        >
          <Cloud size={18} />
          <div>
            <p className="sync-card-title">
              <span>{t("iCloud 云盘")}</span>
              {icloudBadge && <span className={`sync-badge ${sync.icloudEnabled && icloud?.available ? "on" : ""}`}>{icloudBadge}</span>}
            </p>
            <p className="muted small">{t("适合 Mac 与 iPhone / iPad")}</p>
          </div>
        </button>
        <button
          type="button"
          className={`engine ${tab === "webdav" ? "on" : ""}`}
          onClick={() => setTab("webdav")}
        >
          <Globe size={18} />
          <div>
            <p className="sync-card-title">
              <span>WebDAV</span>
              <span className={`sync-badge ${sync.webdavEnabled ? "on" : ""}`}>{webdavBadge}</span>
            </p>
            <p className="muted small">{t("坚果云、Nextcloud 等，全平台互通")}</p>
          </div>
        </button>
      </div>

      {tab === "icloud" ? (
        <ICloudPanel
          sync={sync}
          icloud={icloud}
          lastSummary={status?.lastIcloud}
          busy={busy != null}
          onPatch={patchSync}
          onRefreshStatus={refreshStatus}
          onSync={() => void run("icloud")}
          onNotify={notify}
        />
      ) : (
        <WebdavPanel
          sync={sync}
          lastSummary={status?.lastWebdav}
          busy={busy != null}
          onPatch={patchSync}
          onSync={() => void run("webdav")}
        />
      )}

      <div className="sync-shared-opts">
        <label className="check">
          <input
            type="checkbox"
            checked={sync.syncProgress}
            onChange={(e) => void patchSync({ syncProgress: e.target.checked })}
          />
          {t("同时同步播放进度与已看状态")}
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={sync.syncContent}
            onChange={(e) => void patchSync({ syncContent: e.target.checked })}
          />
          {t("同时同步导读、精读与问答")}
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={sync.autoSync}
            onChange={(e) => void patchSync({ autoSync: e.target.checked })}
          />
          {t("启动、切回窗口或内容变更时自动同步")}
        </label>
      </div>
    </div>
  );
}

function ICloudPanel({
  sync,
  icloud,
  lastSummary,
  busy,
  onPatch,
  onRefreshStatus,
  onSync,
  onNotify,
}: {
  sync: SyncCfg;
  icloud?: { available: boolean; path: string; defaultPath: string; hasFile: boolean; note?: string | null };
  lastSummary?: BackendSyncSummary | null;
  busy: boolean;
  onPatch: (patch: Partial<SyncCfg>, lazy?: boolean) => Promise<void>;
  onRefreshStatus: (customDir?: string) => Promise<unknown>;
  onSync: () => void;
  onNotify: (msg: string) => void;
}) {
  const canEnable = !!icloud?.available;
  const activePath = sync.icloudDir.trim() || icloud?.path || "";
  const pickDir = async () => {
    const chosen = await openDialog({ directory: true, multiple: false, defaultPath: activePath || undefined });
    if (typeof chosen === "string" && chosen) {
      await onPatch({ icloudDir: chosen });
      await onRefreshStatus(chosen);
    }
  };
  const resetDir = async () => {
    await onPatch({ icloudDir: "" });
    await onRefreshStatus("");
  };
  return (
    <div className="provider-form">
      <label className="check">
        <input
          type="checkbox"
          checked={sync.icloudEnabled}
          disabled={!canEnable && !sync.icloudEnabled}
          onChange={(e) => void onPatch({ icloudEnabled: e.target.checked })}
        />
        {t("开启 iCloud 云盘同步")}
      </label>
      {icloud?.note && (
        <p className="muted small">{tr(icloud.note)}</p>
      )}
      <div className="sync-path-row">
        <div className="sync-path-text">
          <p className="small">{t("同步目录")}</p>
          <p className="muted small clamp1" title={activePath || undefined}>
            {sync.icloudDir ? sync.icloudDir : t("iCloud 专属容器（SubCast）")}
          </p>
        </div>
        <button type="button" className="btn sm" onClick={() => void pickDir()}>
          <FolderOpen size={13} /> {t("选择文件夹")}
        </button>
        {activePath && (
          <button
            type="button"
            className="btn ghost sm"
            onClick={() => void api.revealFile(activePath).catch((e) => onNotify(errorText(e)))}
          >
            {t("打开")}
          </button>
        )}
        {sync.icloudDir && (
          <button type="button" className="btn ghost sm" onClick={() => void resetDir()}>
            {t("恢复默认")}
          </button>
        )}
      </div>
      <p className="muted small">
        {t("同步文件保存在 iCloud 专属容器（SubCast）中，在移动版开启 iCloud 同步即可自动互通。")}
      </p>
      <SyncSummaryLine summary={lastSummary} />
      <div className="row-actions">
        <button
          type="button"
          className="btn sm primary"
          disabled={busy || !canEnable}
          onClick={onSync}
        >
          {busy ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />} {t("立即同步")}
        </button>
      </div>
    </div>
  );
}

function WebdavPanel({
  sync,
  lastSummary,
  busy,
  onPatch,
  onSync,
}: {
  sync: SyncCfg;
  lastSummary?: BackendSyncSummary | null;
  busy: boolean;
  onPatch: (patch: Partial<SyncCfg>, lazy?: boolean) => Promise<void>;
  onSync: () => void;
}) {
  const [password, setPassword] = useState("");
  const [hasPass, setHasPass] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testMsg, setTestMsg] = useState<{ ok: boolean; text: string } | null>(null);

  useEffect(() => {
    void api.hasApiKey(WEBDAV_ACCOUNT).then(setHasPass).catch(() => setHasPass(false));
  }, []);

  const savePass = async () => {
    await api.setApiKey(WEBDAV_ACCOUNT, password.trim());
    setHasPass(!!password.trim());
    setPassword("");
  };

  const testConn = async () => {
    setTesting(true);
    setTestMsg(null);
    try {
      if (password.trim()) await savePass();
      const msg = await api.syncWebdavTest(sync.webdavUrl, sync.webdavPath, sync.webdavUsername);
      setTestMsg({ ok: true, text: tr(msg) });
    } catch (e) {
      setTestMsg({ ok: false, text: errorText(e) });
    } finally {
      setTesting(false);
    }
  };

  const syncNow = async () => {
    setTestMsg(null);
    if (password.trim()) await savePass();
    onSync();
  };

  const isJianguoyun = sync.webdavUrl.toLowerCase().includes("jianguoyun.com");

  return (
    <div className="provider-form">
      <label className="check">
        <input
          type="checkbox"
          checked={sync.webdavEnabled}
          onChange={(e) => void onPatch({ webdavEnabled: e.target.checked })}
        />
        {t("开启 WebDAV 同步")}
      </label>
      <div className="model-chips">
        {WEBDAV_PRESETS.map((p) => {
          const active = sync.webdavUrl.trim() === p.url;
          return (
            <button
              key={p.id}
              type="button"
              className={`chip sm ${active ? "on" : ""}`}
              onClick={() => { setTestMsg(null); void onPatch({ webdavUrl: p.url }); }}
            >
              {p.id === "jianguoyun" ? t("坚果云") : p.label}
            </button>
          );
        })}
      </div>
      {isJianguoyun && (
        <p className="muted small">{t("坚果云请在网页端「账户信息 › 安全选项」生成第三方应用密码，不要填登录密码。")}</p>
      )}
      <label>
        <span>{t("服务器地址")}</span>
        <input
          {...NO_AUTO}
          value={sync.webdavUrl}
          onChange={(e) => void onPatch({ webdavUrl: e.target.value }, true)}
          placeholder="https://dav.jianguoyun.com/dav/"
        />
      </label>
      <label>
        <span>{t("同步目录")}</span>
        <input
          {...NO_AUTO}
          value={sync.webdavPath}
          onChange={(e) => void onPatch({ webdavPath: e.target.value }, true)}
          placeholder="/SubCast"
        />
      </label>
      <label>
        <span>{t("用户名")}</span>
        <input
          {...NO_AUTO}
          value={sync.webdavUsername}
          onChange={(e) => void onPatch({ webdavUsername: e.target.value }, true)}
          placeholder={t("邮箱或账号")}
        />
      </label>
      <label>
        <span>{t("密码")}</span>
        <KeyRow
          has={hasPass}
          value={password}
          onValue={setPassword}
          placeholder={t("输入密码或应用密码")}
          onSave={savePass}
          onDelete={async () => {
            await api.setApiKey(WEBDAV_ACCOUNT, "");
            setHasPass(false);
          }}
        />
      </label>
      <SyncSummaryLine summary={lastSummary} />
      <div className="row-actions">
        <button type="button" className="btn sm" disabled={testing || busy} onClick={() => void testConn()}>
          {testing && <Loader2 size={13} className="spin" />} {t("测试连接")}
        </button>
        <button type="button" className="btn sm primary" disabled={busy || testing} onClick={() => void syncNow()}>
          {busy ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />} {t("立即同步")}
        </button>
        {testMsg && <span className={testMsg.ok ? "ok-text small" : "error-text small"}>{testMsg.text}</span>}
      </div>
    </div>
  );
}
