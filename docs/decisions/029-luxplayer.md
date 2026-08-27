# ADR-029：LuxPlayer 原生 Web 播放系统

## 状态

已接受

## 日期

2026-08-27

## 背景

Lux 已有独立的 Web 播放会话、Direct Play、服务端 HLS、客户端 HEVC/MKV fallback、字幕文件端点、章节数据和播放进度事件。现有播放器页面已经能播放媒体，但播放业务、引擎适配、控制栏和后续字幕/弹幕能力仍然集中在页面实现中，难以持续演进。

ArtPlayer 是成熟的 MIT 许可 HTML5 播放器，包含控制栏、手势、字幕、弹幕、时间轴和插件实现经验。Lux 希望大量借鉴其中经过验证的实现，但 Lux 与 ArtPlayer 是两个产品，不能让 ArtPlayer 的公共 API、DOM、插件模型或运行时依赖成为 Lux 的架构边界。

## 决策

### 独立的 LuxPlayer

Lux 实现名为 `LuxPlayer` 的自有播放器系统。ArtPlayer 源码可以在保留 MIT 版权与许可证声明的前提下，按模块选择性复制、改造和维护；也可以只借鉴算法和交互设计。Lux 运行时不直接依赖 `artplayer` 包，Lux 业务代码不引用 ArtPlayer 类型、事件名或插件接口。

吸收 ArtPlayer 代码时必须：

1. 固定上游仓库、版本和 commit。
2. 在 `docs/THIRD-PARTY-NOTICES.md` 的来源台账中记录原始路径、Lux 路径、变更摘要和许可证。
3. 在衍生文件保留清晰的来源声明；大段复制的模块保留适用的 MIT notice。
4. 不复制 ArtPlayer 品牌、文案、图标、演示站资产或与 Lux 无关的插件。
5. 任何复制模块都必须通过 Lux 自己的接口和测试，不能把上游内部结构向 Lux API 泄漏。

### 分层边界

```text
LuxPlayer UI（React + TypeScript）
        ↓
LuxPlayer Controller（状态机、命令、手势、字幕、弹幕）
        ↓
Lux Playback Engine（Native / HLS / Client fallback / Rust-WASM）
        ↓
Lux Rust Playback API（会话、签名资源、权限、进度、章节）
```

- Rust 服务端继续负责权限、媒体源固定、播放计划、会话生命周期、签名资源、HLS 资源和进度持久化。
- TypeScript 负责 Lux 播放器状态、UI、键盘/触摸交互、字幕显示、弹幕显示和浏览器能力适配。
- JavaScript 只作为浏览器运行时、Worker 和 WASM glue 的实现载体，不建立第二套播放器业务模型。
- Rust/WASM 只用于容器解析、索引、seek 辅助或经真实性能测试证明值得迁移的重型处理；浏览器原生解码仍是首选。
- Emby 播放路由和 DTO 继续与 LuxPlayer API 分离。

### 与现有 Lux 能力的结合

LuxPlayer 必须通过现有的 `POST /api/v1/playback/sessions` 创建播放计划，并使用返回的签名资源；不得自行拼接媒体路径、绕过 ACL、把 `.strm` 交给服务端处理或改变 0～4 档播放决策。播放器事件继续通过现有 Web 播放会话事件接口写入，因此 Home 继续观看、用户播放状态、管理员当前播放和 Emby 兼容视图保持一致。

版本、音轨、字幕、章节和弹幕都绑定到当前的 `itemId`、`mediaSourceId` 和播放会话。切换版本或需要改变媒体计划时，播放器先停止旧会话，再创建新会话；旧资源和旧事件不能污染新会话。

### 字幕与弹幕

字幕先复用 Lux 已有的受 ACL 保护字幕端点，在浏览器端进行格式解析和渲染。WebVTT/SRT 是第一批目标；ASS/SSA 需要单独的渲染器和真实性能测试，不能把服务端字幕转换或烧录偷偷加入播放路径。

Web 弹幕使用 Lux 自己的数据模型和渲染器。现有 Emby 弹幕 XML 兼容端点和后台匹配服务是数据来源之一，但不直接把 ArtPlayer 弹幕插件 API 暴露给 Web。弹幕读取、解析、密度、屏蔽、暂停和 seek 同步都必须有资源上限。

## 未采用方案

### 直接引入 ArtPlayer 作为 LuxPlayer

被拒绝。这样可以快速得到完整 UI，但会把 ArtPlayer 的生命周期、DOM、事件、插件和升级节奏变成 Lux 的隐性公共依赖，也会让 Lux 的播放会话、ACL、版本/轨道模型难以自然接入。

### 从零实现所有播放器代码

被拒绝。控制栏、手势、时间轴、字幕和弹幕有大量成熟边界问题；在不复制产品边界的前提下选择性吸收 MIT 代码，可以降低重复试错成本。

### 先实现 Rust 自有视频 codec

被拒绝。视频 codec、硬件加速、HDR、移动端功耗和浏览器集成的成本远高于当前收益。Rust/WASM 先处理可验证的容器/索引问题，codec 继续使用浏览器或已有 fallback。

## 后果

- Lux 需要维护来源台账、MIT notice 和与上游差异；这是一项长期维护成本。
- LuxPlayer 可以独立改变 UI、事件和扩展模型，不受 ArtPlayer API 兼容性约束。
- 现有播放后端和第三方 Emby 客户端不需要跟随 Web 播放器重构。
- Web 弹幕和完整 ASS/SSA 渲染成为明确的后续 Lux 任务，不再被误认为现有播放器已支持。
- 每个新播放引擎都必须通过统一的 Lux engine contract 和真实浏览器验证，不能只凭静态能力检测宣称兼容。

## 验证

- `rg` 检查 Lux 业务代码不直接导入 `artplayer` 或引用 ArtPlayer 类型。
- Web 单测覆盖状态机、命令和引擎生命周期；浏览器测试覆盖 Direct、HLS、fallback、seek、停止和页面离开。
- 衍生模块的来源、许可证、上游 commit 和变更摘要均在 `docs/THIRD-PARTY-NOTICES.md` 留痕。
- 每项 Rust/WASM 能力都要记录样本、浏览器、设备、吞吐、丢帧和回退结果，不能把探测成功等同于实时播放。
