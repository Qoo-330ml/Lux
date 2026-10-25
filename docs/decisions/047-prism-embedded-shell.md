# ADR-047：Lux Prism 嵌入式桌面客户端

## 状态

已接受

## 日期

2026-10-25

## 背景

Lux 目前只有 Web 客户端（ADR-006），桌面端与原生播放能力是缺口。项目所有者确定 Prism 采用
「以 JellyPilot 为基底、前端完全复用 Lux Web」的路线，需要先固定三件事：前端复用边界、认证
入口、以及内嵌播放器的实现与许可证。

现有约束：

- Lux Web 是同源、cookie + CSRF 的前端（`web/src/lib/api/client.ts`），所有 API 路径为相对
  `/api/v1/...`，实时通知使用同源 `EventSource`。
- Lux 服务端是单进程（ADR-001），Prism 不得把第二个常驻服务或 Tauri 依赖带进 `luxd`。
- LuxPlayer 已按 UI / Controller / Engine / Rust API 分层（ADR-029），Engine 层是可扩展点。
- ADR-041 为 Prism 规划了一次性设备配对并签发 Emby AccessToken，其「未采用方案」明确拒绝让
  Prism 持有 Web session。
- 协议边界只有 Emby（ADR-003）；Jellyfin-only 客户端不在兼容承诺内。
- JellyPilot（MIT，`hewel/jellypilot`）把无显示逻辑的部分拆成独立 crate：
  `jellypilot-auth`、`jellypilot-media-server`（含 `media-server-api/emby`）、`jellypilot-mpv`、
  `jellypilot-session`；`jellypilot-ui`、`src-iced` 在另一个 workspace 中被显式排除。

## 决策

### 形态：独立壳，不打包前端

Prism 是独立仓库中的桌面壳（wry，必要时再加 Tauri 能力层），启动后加载用户填写的 Lux origin：

- 不分发 `web/dist`，不内嵌构建产物，前端只有服务器上那一份。
- 壳只负责：服务器地址簿、窗口与生命周期、mpv 进程/渲染层、IPC。
- Prism 不进入 `luxd` 的依赖图，保持单进程部署（ADR-001）。

### 认证：地址 + Lux 自己的登录页

- 原生 UI 只做「添加服务器地址」，接受 `http://` 与 `https://`。
- 账号密码在 Lux 原生登录页输入，会话 cookie 由服务器下发，Prism 不代为登录、不搬运 CSRF。
- `Secure` 属性跟随实际传输方案（见 `docs/API.md`「Lux 客户端令牌」前后两段），因此局域网明
  文地址可用；Prism 必须在地址为 `http://` 时提示「凭据明文传输」。
- Prism v1 不使用 `/api/v1/device-pairings/*`，也不持有 Emby AccessToken。ADR-041 的服务端能力
  与令牌模型保留，供后续「无密码配对」或其它客户端使用；本 ADR 不改动它。

### 播放：内嵌 libmpv，视频层在 WebView 之下

- 使用 **LGPL 模式构建的 libmpv**（`--enable-lgpl`），动态链接、允许替换库文件（LGPLv2.1 §6），
  不使用 GPL 版构建，避免与 `LUX-NCSAL` 冲突，也避免把 Prism 锁成 GPL。
- 组合方式：mpv 渲染层位于 WebView **之下**，WebView 在播放器路由上透明；控件、字幕设置、进度
  条仍由 LuxPlayer 的 DOM 绘制。这样播放器 UI 与 Web 完全一致。
- Lux Web 新增 `native` 播放引擎（`web/src/features/player/engines/`），默认关闭，仅当宿主注入
  `window.__LUX_PRISM__` 能力声明时启用。引擎通过 IPC 把播放地址、起始位置、轨道状态交给壳，壳
  把进度/轨道/停止事件回传，最终仍写入 Lux 的播放会话接口。
- 平台差异按各自最可靠的方式实现，不追求一套代码通吃：macOS 走 render API + 透明 WKWebView；
  Windows 用 WebView2 透明背景 + 子窗口/D3D 交换链；Linux 用 WebKitGTK 透明 + render API 或
  `wid` 嵌入（Wayland 另行验证）。

### 前端复用边界

允许的改动只有两处，其余一律不分叉：

1. 新增 `native` 引擎文件与其单测；
2. 播放器根节点在 `data-engine="native"` 时使用透明背景（视觉零差异）。

登录、首页、媒体库、详情、搜索、管理页、播放器控件结构保持逐像素一致。

### 上游依赖与台账

- 只依赖 JellyPilot 的四个无 UI crate，pin 具体 commit，不跟 HEAD；不引入 `jellypilot-ui`、
  `src-iced`、`jellypilot-launcher`、`android/`。
- 来源、commit、许可证与改动摘要登记在 `docs/THIRD-PARTY-NOTICES.md`，沿用 ADR-029 的上游台账
  规则；Prism 仓库自身同样维护一份。

## 未采用方案

### 用设备配对换取 AccessToken 驱动 Lux 前端

前端没有 `X-Lux-Token` 支持，而 `PATCH /api/v1/auth/password` 等接口明确只接受 Web session 与
CSRF；按此路线会出现「部分页面不可用」，直接破坏前端一致性。

### 把 `web/dist` 打包进客户端

本地 origin 与「Secure 跟随传输方案」的语义冲突；且会形成两份前端版本，升级必然漂移。若将来确实
需要离线安装包，需另行 ADR 解决本地 HTTPS 与证书信任。

### 外挂 mpv 进程（JellyPilot 在 Windows/macOS 的默认做法）

播放窗口脱离 WebView，播放器 UI 无法继续由 Lux 前端绘制，与「前端 1:1」目标冲突。仅作为 macOS
内嵌方案失败时的降级路径保留。

### 用 iced 重写 Lux UI

等于放弃前端 1:1，退化为两套 UI 并行维护，也浪费已完成的 Web 设计系统。

## 后果

- Prism 的登录态与服务端 Web session 生命周期绑定：Web 登出、session 过期（30 天）都会让 Prism
  回到登录页。这是「官方壳」形态的固有取舍，与 ADR-041 追求的「设备生命周期独立」不同。
- 内嵌 libmpv 意味着三套平台组合器实现与三份打包签名工作，其中 macOS 为首个验证平台（本机
  arm64）。
- 明文 HTTP 可用但不安全，Prism 必须做显式提示；面向公网仍应部署 HTTPS。
- `X-Forwarded-Proto` 当前只影响 cookie 的 `Secure` 属性与展示，且 `is_secure_request` 尚未校验
  对端是否受信代理：直连客户端可自称 https 并让自己收到 `Secure` cookie（自身登录失败，非提权）。
  这是一条独立的硬化项，不与本 ADR 捆绑实施。
- 待项目所有者分配任务号后，以下工作分别登记为独立任务：`docs/API.md` 纠错（本次已完成）、
  `X-Forwarded-Proto` 采信收敛、Lux Web 的 `native` 引擎、独立仓库 `lux-prism` 壳与打包。
