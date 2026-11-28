# macOS 独立开发与上游合并

本仓库为 https://github.com/kothedog/subcast ，上游为 https://github.com/joeseesun/subcast 。仅开发和验收 macOS，优先 Apple 芯片；Intel 兼容性另行验证。保留跨平台源码以便合并上游，不安排 Windows/Linux 发包。

## 仓库与提交方向

- origin 是自己的 fork，所有开发分支、推送和 PR 都进入 kothedog/subcast。
- upstream 只用于获取 joeseesun/subcast 的更新。不向上游推送，不向上游提 PR；只有用户明确改变这一要求时才调整。
- 独立开发走短分支 → 自己 fork 内的 PR → merge commit，不直接推送 main。
- 合并上游时保留双方历史及本 fork 定制，通过 Git merge 处理冲突。fork 已有独立改动后，禁止用上游整棵文件树覆盖 fork。
- 这是公开 fork，原作者仍可查看改动；它不会自动接收这些改动。保留原作者署名与 AGPL-3.0 许可。

本机已将默认推送目标设为 origin，并禁用 upstream 推送地址。新克隆需设置一次：

    git remote add upstream https://github.com/joeseesun/subcast.git
    git remote set-url --push upstream DISABLED
    git config remote.pushDefault origin

已存在 upstream 时跳过第一行。插件操作也必须显式选择 owner=kothedog、repo=subcast，不能依赖本机 Git 设置代替目标核对。

## 合并上游更新

在干净的独立工作区中执行，以已更新的 origin/main 建同步分支：

    git fetch origin main
    git fetch upstream main
    git switch -c chore/sync-upstream-YYYYMMDD origin/main
    git merge upstream/main

逐项处理冲突，保留 fork 的 macOS 配置、开发入口和本说明。运行下方检查后，仅推送同步分支到 origin，创建目标为 kothedog/subcast:main 的 PR，合并时保留 merge commit。通过 GitHub 插件完成写入时也要保留双方父提交及实际合并结果，不能直接取上游文件树作为结果。

## 需求与验收范围

以 docs/PRD.md、docs/ARCHITECTURE.md 和当前实现为依据；docs/HANDOFF.md 是历史记录，须结合当前代码核实。主要路径是订阅 → 音视频播放 → 双语字幕与对齐 → 翻译与 AI 问答，以及代理、Keychain、离线下载、本地模型和 macOS 快捷键。云端功能仅在用户主动配置后验收。

PRD 的字幕出现时间和本地转写速度仍为待实测目标。现有代码已包含精读、海报、生词学习、CLI/MCP 等能力；准备阶段不重写它们。本轮合并上游 0.3.12，保留其可选跨设备数据同步实现，不自动启用或配置 WebDAV。

## 工具链与启动

需要 Node.js 24 LTS（ts-fsrs 6 要求 Node >=24）、Rust stable、CMake、Xcode 及 Metal Toolchain。scripts/macos-dev.sh 优先使用 .dev-tools 下的项目内工具，也支持 PATH 中已有工具；不修改全局 shell 配置。.dev-tools、模型、缓存及密钥不提交。

    bash scripts/macos-dev.sh doctor
    bash scripts/macos-dev.sh install
    bash scripts/macos-dev.sh check
    bash scripts/macos-dev.sh dev
    bash scripts/macos-dev.sh build

check 包含 TypeScript、严格 i18n、前端生产构建、字幕规则及 Rust workspace lib 测试。首次编译需联网下载依赖，部分测试需要本地 HTTP 端口；真实服务测试按原配置忽略，需单独安排。模型单测仅使用临时数据，避免扫描真实用户目录。

开发入口使用 src-tauri/tauri.macos-dev.conf.json，标识为 io.github.kothedog.subcast.dev，数据库和模型位于 ~/Library/Application Support/io.github.kothedog.subcast.dev/，与原版数据库分开。Keychain 服务名仍沿用上游，不能据此声称密钥完全隔离。

build 生成当前架构的 debug app：src-tauri/target/debug/bundle/macos/SubCast.app。它不代表已签名、公证、包含可用 CLI 的发行安装包；完整 CLI 打包另行验证。上游 scripts/dev-app.sh 默认使用作者 Developer ID，不是本 fork 的通用构建入口。

## 独立发行前的配置

开发配置清空应用更新源并关闭 updater 产物生成；上游字幕、翻译、界面更新、远程配置等网络地址仍存在，开发版不是独立后端或纯离线产品。正式发行前需要配置自己的正式应用标识、Keychain 命名、应用与界面更新源、签名公钥、Developer ID 和公证资料，并明确共享字幕的数据去向。

遵循 docs/RELEASING.md 区分界面更新、应用发版和服务部署；不要直接运行含作者证书、官网和服务器目标的 release.sh 发布到原作者设施。当前 GitHub workflow 仍含上游 Windows/Linux 发版流程，本轮不触发。
