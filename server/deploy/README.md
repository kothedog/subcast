# 部署 SubCast 共享字幕 API（myvps）

目标：Docker Compose（Qiaomu 服务器的约定），容器只绑私有回环端口 127.0.0.1:8787，经 Nginx 暴露在 `https://subcast.qiaomu.ai/api/`。服务零依赖，直接用官方 `node:22-alpine` 镜像运行源码，不需要构建镜像。

## 1. 同步代码

在本地仓库根目录执行：

```bash
ssh myvps 'mkdir -p /opt/qiaomu-apps/subcast-api/data'
rsync -az --delete --exclude data --exclude node_modules --exclude test --exclude .env \
  server/ myvps:/opt/qiaomu-apps/subcast-api/
scp server/deploy/docker-compose.yml myvps:/opt/qiaomu-apps/subcast-api/docker-compose.yml
```

## 2. 环境文件（只在服务器上生成，不经过本机）

```bash
ssh myvps 'cd /opt/qiaomu-apps/subcast-api && [ -f .env ] || (umask 077; printf "ADMIN_TOKEN=%s\n" "$(openssl rand -hex 32)" > .env)'
```

## 3. 启动 / 更新

```bash
ssh myvps 'cd /opt/qiaomu-apps/subcast-api && docker compose up -d && docker compose ps'
```

代码只读挂载，更新代码后 `docker compose restart` 即可。日志：`ssh myvps 'docker logs -f qiaomu-subcast-api'`（每请求一行，无 IP）。数据库在宿主机 `data/`，备份：`ssh myvps 'docker exec qiaomu-subcast-api node --disable-warning=ExperimentalWarning src/backup.js'`。

（`deploy/subcast-api.service` 是早期的 systemd 方案，保留作参考，线上不用。）

## 5. Nginx

把 `deploy/nginx-location.conf` 的 `location /api/` 片段加入 `/www/server/panel/vhost/nginx/subcast.qiaomu.ai.conf` 的 HTTPS server 块（若改了端口，同步改 `proxy_pass`）。如果该站点还没有 vhost，按 qiaomu-vps-website-ops 的标准流程建 vhost + DNS-only A 记录（指向 76.13.103.27）+ Certbot 证书。

```bash
ssh myvps 'nginx -t && nginx -s reload'
```

## 6. 验证

```bash
curl -s https://subcast.qiaomu.ai/api/health
# {"ok":true,"version":"0.1.0","transcripts":0}

curl -s 'https://subcast.qiaomu.ai/api/v1/transcripts?key=yt:dQw4w9WgXcQ'
# {"error":"not_found"}
```

也可以从本机跑站点检查：

```bash
python3 ~/.agents/skills/qiaomu-vps-website-ops/scripts/qiaomu_vps_site_check.py \
  --host myvps --domain subcast.qiaomu.ai --runtime systemd --service subcast-api
```

## 7. 备份（可选，建议）

每天快照 SQLite（gzip，保留 30 份在 `data/backups/`）：

```bash
ssh myvps 'echo "17 3 * * * root docker exec qiaomu-subcast-api node --disable-warning=ExperimentalWarning src/backup.js >/dev/null 2>&1" > /etc/cron.d/subcast-api-backup'
```

## 8. 更新流程

重复第 2 步 rsync，然后 `ssh myvps 'systemctl restart subcast-api'`。数据库 schema 向后兼容（`user_version` 迁移），重启即生效。

---

## Deploy notes (English)

Docker Compose on myvps (`/opt/qiaomu-apps/subcast-api`, stock `node:22-alpine`, code mounted read-only, database in `data/` on the host), bound to 127.0.0.1:8787 and exposed as `https://subcast.qiaomu.ai/api/` through the `location /api/` block in the site's vhost. The `.env` (ADMIN_TOKEN) is generated on the server. Updates: rsync `server/` then `docker compose restart`. A daily cron runs `src/backup.js` inside the container (gzipped VACUUM INTO snapshots, 30 kept). `subcast-api.service` is the earlier systemd variant, kept for reference.

## yt-dlp mirror

The app downloads yt-dlp from `https://subcast.qiaomu.ai/tools/yt-dlp/` first (GitHub second) and checks it
against `SHA2-256SUMS`. `sync-ytdlp.sh` mirrors the latest release every six hours and verifies every file before
replacing it:

```bash
# /etc/cron.d/subcast-ytdlp
17 */6 * * * root /opt/qiaomu-apps/subcast-api/deploy/sync-ytdlp.sh >> /var/log/subcast-ytdlp.log 2>&1
```

The cron runs the copy that step 1's rsync puts in `deploy/`. Don't copy the script anywhere else under
`/opt/qiaomu-apps/subcast-api/`: `rsync --delete` removes files `server/` doesn't have, and the mirror then stops
updating without any visible error, leaving every client on an old yt-dlp that YouTube may already have broken.
Check it with `curl -s https://subcast.qiaomu.ai/tools/yt-dlp/VERSION` and `tail /var/log/subcast-ytdlp.log`.

Files live in `/www/wwwroot/subcast-tools/` (outside the website root, so website deploys never delete them),
served by `location ^~ /tools/ { alias /www/wwwroot/subcast-tools/; }` in the subcast.qiaomu.ai vhost.
