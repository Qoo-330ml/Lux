# ADR-047：Lux Prism 桌面客户端边界

## 状态

已接受。本文于 2026-10-25 修订，取代同日初稿（初稿假设 Prism 尚未实现，并按「裸壳加载 Lux Web」
设计）。Prism 自身的实现决策以其仓库 ADR 为准，本文只记录跨仓库边界。

## 日期

2026-10-25

## 背景

Lux Prism 桌面客户端已在独立仓库 `../lux-prism` 实现，形态为 Tauri 2 + 自带 React 前端 +
Rust 侧网络/凭据/播放 + 内嵌 libmpv（`libmpv2-sys`，`native-mpv` feature）。它的产品规格、任务
列表和验收标准以 `../lux-prism/docs/PRISM-DEVELOPMENT.md` 为准，Prism 侧已有 ADR-012、ADR-017、
ADR-018 记录自己的决策。

Prism 侧已经固定的安全边界与本文一致：WebView 不得接收 AccessToken、配对 secret 或完整播放 URL；
播放必须使用内嵌 libmpv 的 client/render API，禁止启动外部 mpv 进程，也不把 `--wid` 当正式架构。

项目所有者决定：Prism 前端**只做视觉与交互对齐，不共享 Lux 前端代码，也不在运行时加载 Lux 部署
产物**。因此 Lux 仓库需要记录的只有跨仓库边界，不记录 Prism 的内部实现。

## 决策

### 两条独立前端

- Prism 是自有 React 应用，不引用 Lux `web/src`，不在运行时加载 Lux 的 Web 产物。
- 「一比一」的范围限定为视觉与交互：配色、排版、页面结构、状态与反馈以 Lux Web 为参照，由
  Prism 各自实现与测试。
- Lux 仓库不引入任何 Prism 代码、依赖或构建步骤，继续保持单进程（ADR-001）。

### 协议面

- Prism 通过 Emby 兼容合同访问 Lux：`Users/AuthenticateByName`、`Items`、`PlaybackInfo`、
  `Sessions/Playing*`、图片与字幕入口等。具体返回形状以本仓库实现与 `tests/` 为准，不以通用
  Emby 文档替代。
- Prism 在 Rust 侧持有用户令牌并使用客户端令牌或 Emby 鉴权头；它不使用浏览器 cookie 会话，因此
  Lux Web 的 cookie/CSRF 语义与 Prism 无关。
- 桌面端 v1 只支持「手动 HTTP(S) 地址 + 账号密码」，不提供扫码/二维码入口。ADR-041 的服务端
  配对能力保留，但不由桌面端使用。

### 传输与地址

- 允许明文 HTTP，Prism 必须持续提示安全风险；HTTPS 下不得关闭 TLS 校验。
- Lux 侧 cookie 的 `Secure` 属性跟随传输方案（见 `docs/API.md`），只影响浏览器语义的 Web 客户端。

### 播放

- 内嵌 libmpv 走 client/render API，视频层由客户端自己渲染。
- libmpv 使用 **LGPL 模式构建**、动态链接、允许替换库文件（LGPLv2.1 §6）；不使用 GPL 构建，避免
  与 `LUX-NCSAL` 冲突，也避免把 Prism 锁成 GPL。构建产物与来源台账由 Prism 仓库维护。
- 同一时刻只允许一个播放会话，按 Prism 规格执行。

### 上游参考

- JellyPilot 仅作为参考实现阅读，不引入其 crate 依赖（Prism 已有等价的 Rust 协议层与 mpv 层）。
  如需引入具体 crate，须单独评审并在上游台账登记 commit、许可证与改动摘要。

## 未采用方案

### 共享 Lux 前端源码

需要把 Lux `web/src` 的视图层与数据层解耦成可注入适配器，且 LuxPlayer 的浏览器解码链
（hls.js、@hevcjs、mp4box）与内嵌 libmpv 会形成两套播放器。所有者选择各自实现。

### 运行时加载 Lux 部署产物

WebView 指向 Lux origin 会持有浏览器会话、接触完整播放 URL，与 Prism 的凭据边界冲突。

### 设备配对作为桌面 v1 入口

桌面端不做摄像头扫码；配对能力留给其它形态或后续版本。

### 外部 mpv 进程或 `--wid` 嵌入

播放窗口脱离客户端渲染，UI、字幕与生命周期不可控。

### 以 JellyPilot 作为实现基底

已评估并否决：Prism 现有的 `native_mpv`、协议与缓存模块已覆盖同等职责，引入其 crate 会同时存在
两套播放器与协议实现；保留 JellyPilot 作为参考资料，不进入依赖图。

## 后果

- 两套前端并存：Lux Web 改版后 Prism 需要人工跟进，视觉漂移是持续成本。这是「不共享代码」的代价。
- Lux 侧不承担 Prism 的发版、打包与 libmpv 许可合规，也不因此新增运行时依赖。
- Prism 桌面 v1 无扫码配对，用户需手输地址，这是可用性上的已知取舍。
- 待编号事项：Prism 视觉对齐验收清单、libmpv LGPL 构建产物与台账、Prism 跟进 Lux 新接口的节奏。
