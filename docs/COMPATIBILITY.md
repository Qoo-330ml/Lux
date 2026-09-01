# Lux 客户端兼容性矩阵

## LUX-195 元数据刮削器边界

元数据插件通过 `providerKey` 声明身份命名空间，`pluginId` 只表示安装/运行时实现。应用层统一调用
`metadata.search/get/bundle/images/credits/externalIds/trailers`，provider ID 按不透明字符串处理；因此
`tmdb:123`、`imdb:tt1234567`、`imdb:nm123` 和 `douban:douban-456` 不需要共享数字 ID 解析路径。

TMDb 和豆瓣的 typed endpoint、语言回退、合集、图片 URL 转换、凭据和配置解释全部由各自外置插件负责；
Lux 主程序统一走 `ScraperPluginClient`，不再编译 TMDb client/adapter 或按 TMDb 插件 ID 特判。metadata
插件只收到自己的 `LUX_PLUGIN_CONFIG_PATH`，不能继承整个 `LUX_CONFIG_DIR`。manifest 未声明所需 capability
时，宿主在发起 RPC 前返回稳定的 `scraper capability unavailable: <capability>` 错误，既不回退到 TMDb，
也不把缺失能力报告为 TMDb 故障。`tmdb`、`douban` 仍是兼容 namespace/alias；此变更不新增数据库 migration。

本文档是目标客户端兼容性的唯一事实来源。未填入实测版本和证据前，不得宣称兼容。

## 目标矩阵

| 客户端 | 版本 | 平台/设备 | 添加服务器 | 登录 | 浏览/详情 | 播放 | 进度/收藏 | 字幕/多版本 | 证据/备注 |
|---|---|---|---|---|---|---|---|---|---|
| Infuse | 未测试 | 未测试 | 未测试 | 未测试 | 未测试 | 未测试 | 未测试 | 未测试 | 待 LUX-025 |
| VidHub | 2.1.8 | macOS arm64 | 通过 | 通过 | 媒体库浏览、条目详情通过 | 通过 | 通过 | 未测试 | 2026-08-05 本机 ARM64 真实 UI 播放本地 MKV，Playing/Progress/Stopped 回传和 Resume 读回通过；收藏/已观看状态另有 2026-08-03 证据 |
| SenPlayer | 6.0.6 | macOS arm64 | 通过 | 通过 | 首页、电影列表通过 | 通过 | 未测试 | 未测试 | 2026-08-07 本机 ARM64 真实 UI 播放 `.strm` 电影通过；服务端兼容客户端生成的小写 `/emby/videos` 和路径内编码查询参数，并对远程源返回 307 直连重定向 |
| Harbor | 1.4.6 | macOS arm64 | 通过 | 通过 | 媒体库浏览、条目列表通过 | 未测试 | 未测试 | 未测试 | 2026-08-09 本机 Harbor 连接本机 Lux 后，媒体库详情请求 `/Users/:userId/Items/:libraryId` 从 404 修复为 200，并进入电影库显示条目 |
| Yamby | 2.0.5.5 | Android 17，23127PN0CC，arm64-v8a；宿主 macOS arm64 | 未单独验证（已有连接） | 未单独验证（已有会话） | 通过 | 通过 | 进度/继续播放/收藏通过 | 轨道枚举通过；字幕渲染和多版本未测 | 2026-08-28 adb/scrcpy 真机实测《飞驰人生》；详情显示 1080p H264、DTS-HD MA 5.1 和 PGSSUB，播放器实际出画面并读回进度 |
| CapyPlayer | 1.1.3 | Android 17，23127PN0CC，arm64-v8a；宿主 macOS arm64 | 通过（隔离测试服务器） | 通过 | 媒体库、条目详情通过 | 通过 | 进度通过；收藏/已观看另有实测 | 未测试 | 2026-08-28 使用独立临时媒体 `CapyPlayer Blue Compatibility` 进入 Exo 播放器并出画面；12 秒媒体退出后进度回传为约 12.091 秒 |
| Hills | 1.8.0 | Android 17，23127PN0CC，arm64-v8a；宿主 macOS arm64 | 未单独验证（已有连接） | 未单独验证（已有会话） | 通过 | 通过 | 未单独验证 | 未测试 | 2026-08-28 使用独立资源《7天》进入 Hills MPV 播放器并稳定出画面；此前其他资源已观察到中英文字幕显示；新增服务器流程受现有配置/Pro 限制未重测 |
| VidHub | 3.0.1 | Android 17，23127PN0CC，arm64-v8a；宿主 macOS arm64 | 通过（隔离测试服务器） | 通过 | 媒体库、条目详情通过 | 通过 | 进度/已观看通过；收藏另有实测 | 未测试 | 2026-08-28 使用独立临时媒体 `VidHub Green Compatibility` 进入 VideoPlayActivity；DirectPlay 会话播放完成，进度约 20 秒且 `Played=true` |
| 网易爆米花 | 2.12.5 | Android 17，23127PN0CC，arm64-v8a；宿主 macOS arm64 | 未单独验证（已有远程连接） | 未单独验证（已有会话） | 通过 | 通过（资源差异） | 进度通过；收藏未测试 | 未测试 | 2026-08-28 使用现有远程 Lux 服务器播放《黑色月光》第 1 集；画面稳定，进度由约 00:56 推进至 02:00，退出后详情页读回约 03:14，未出现网络错误提示。此前《风柜来的人》和另一资源曾出现网络提示，暂按资源相关风险记录 |
| Lux Web | Chrome 151 smoke | macOS arm64 | 通过 | 通过 | 基础浏览/详情/筛选/账户会话通过 | Direct、服务器 HLS、客户端 HEVC fallback 通过 | 进度/收藏接口与收藏浏览器 smoke 通过 | 多版本 source 切换、SRT 字幕、已登记 Bilibili XML 弹幕和可见性开关通过 | Chrome headless：普通用户无管理入口、Direct Range、HLS manifest/init/segment、HEVC Worker/WASM/MSE、390/768/1440 viewport、键盘/触摸 seek、覆盖层和会话清理通过；播放器联合 smoke 为 0 error / 0 warning、无外部请求；`scripts/browser-smoke.mjs`、`scripts/admin-smoke.mjs` 和 `scripts/player-danmaku-smoke.mjs` 已固化 |

## Android 真机实测（2026-08-28）

本次使用 Android 17（SDK 37）、设备 `23127PN0CC`、`arm64-v8a`，通过 adb/scrcpy 操作，宿主机
`uname -m=arm64`；记录基准为工作树提交 `12bd91ac20e5`。播放测试使用同一设备上的不同媒体资源，避免
客户端之间因复用播放缓存或进度状态造成误判；临时测试服务、媒体和 adb reverse 映射在测试后已清理。

| 客户端 | 最终播放资源 | 现场结果 |
|---|---|---|
| Yamby 2.0.5.5 | 《飞驰人生》 | 已有 Lux 连接下进入详情和播放器，1080p H264 画面正常；继续播放位置从约 05:02 推进到 05:35，详情页保留继续播放状态；收藏开关可用，测试后恢复原状态 |
| CapyPlayer 1.1.3 | `CapyPlayer Blue Compatibility` | 添加隔离测试服务器并登录后进入 Exo 播放器，画面正常；退出后服务端读到约 12.091 秒播放位置 |
| Hills 1.8.0 | 《7天》 | 已有 Lux 连接下进入详情，由 Hills 内置 MPV 播放器出画面；本次未单独验收进度和收藏 |
| VidHub 3.0.1 | `VidHub Green Compatibility` | 添加隔离测试服务器并登录后进入播放页；服务端会话为 `DirectPlay`，播放约 20 秒完成，进度和 `Played=true` 均回传 |
| 网易爆米花 2.12.5 | 《黑色月光》第 1 集 | 使用已有远程 Lux 服务器直接播放；播放器稳定显示画面，时间从约 00:56 推进到 02:00，退出后详情页读回约 03:14；本次未出现网络错误提示 |

本次实测确认：Yamby、CapyPlayer、Hills、VidHub 和网易爆米花均能完成至少一条本地/远程 Lux 媒体的
基础播放链路。网易爆米花此前在《风柜来的人》和另一条资源上出现过“网络异常，请确保网络正常后重试”提示，
但画面仍曾推进；本次《黑色月光》第 1 集未复现，因此当前将其记录为资源相关风险，而不是整体播放不兼容。

Yamby、Hills 和网易爆米花在本次测试中沿用应用已有的 Lux 服务器配置，没有清除数据重新添加服务器；因此
矩阵中的添加服务器/登录字段仅表示已有连接状态，不等同于全新配置流程验收。字幕、多版本和特定编码的支持
仍需按客户端分别补充实测。以上结果来自本机 ARM64 宿主，不外推为 NAS/x86_64 性能或所有 Android 设备兼容性。

## Lux Web 4K 媒体能力探针

LUX-184 已加入独立探针页面 `/media-capability-probe.html`，用于记录真实媒体样本在原生 `video`、
MediaCapabilities 和 WebCodecs 下的能力。探针能力声明和 LUX-185 客户端 fallback 性能记录分开维护；
fallback 的 4K 实时能力不因探针返回 `supported` 而自动宣称。

2026-08-17 的本机探针记录：Playwright HeadlessChrome 151、macOS `arm64` 对 4K HEVC Main、HEVC Main10/HDR10
和 H.264 的 3840×2160 配置均报告原生 `probably`、MediaCapabilities `supported/smooth/powerEfficient` 和
WebCodecs `supported`。这只是浏览器能力声明，不是实际 4K 文件播放证据。

同次使用公开 Sintel 片段生成的临时 12 秒 HEVC MP4 做解码链路烟测，浏览器实际识别为 854×480，metadata、
播放位置和 5 秒播放均正常；该文件不是 4K 样本，不能用于 4K 性能结论。

## Lux Web LUX-185 客户端 fallback 实测

2026-08-17，提交 `1233ec95`（包含性能状态提交 `fa39190a`），Playwright HeadlessChrome 151.0.0.0，macOS
`arm64`（`uname -m=arm64`）。
样本均为临时、本地、无个人数据文件，完整 URL 不写入记录：

| 样本 | SHA-256 | 结果 | 性能与质量 |
|---|---|---|---|
| 3840×2160 HEVC Main 8-bit + AAC、MP4、8 秒、9.4 MiB | `cbfad82624c6578ea9ce5f2a0f5e229d0230745d7cfa84eb9b5d457b57920ce1` | Worker/WASM 解码、H.264 编码、视频/音频缓冲、播放、暂停、seek、destroy 通过 | Worker 累计处理 21,558.7 ms，媒体时长 8,000 ms，`speedX=0.371`，低于实时；播放 2 秒窗口 50 帧/0 丢帧，漂移约 30 ms；seek 到 4 秒后漂移约 36 ms |
| 3840×2160 HEVC Main10 10-bit HDR10、MP4、约 4.13 秒、21 MiB，无音频 | `88b238b05eca4de87548f5d2b022ddf1daa2e60d4f0218e65ae04db770d1d2da` | WASM 解码、H.264 编码、播放、seek、destroy 通过 | Worker 累计处理 18,929.3 ms，媒体时长 4,086 ms，`speedX=0.216`，低于实时；播放窗口 24 帧/0 丢帧；seek 通过。原始样本音轨为 DTS，测试文件主动去除音频，不代表 AAC 兼容 |

`43a7b8e6` 的流式增量复测确认：`setSource()` 在完整转码完成前即可返回。HEVC Main 在 4,537 ms 返回首段并在
17,665 ms 完成全片，HEVC Main10 在 9,606 ms 返回首段并在 18,577 ms 完成全片；两者首段均收到首帧，seek
误差为 0 ms，`requestVideoFrameCallback` 的 `presentedFrames` 序列没有 gap，Main + AAC 的播放末段音画差约
44 ms。HeadlessChrome 的 `getVideoPlaybackQuality().droppedVideoFrames` 在该测试中与 presented-frame 序列不一致，
因此以 presented-frame gap 作为丢帧判断，并保留该 API 差异作为测试注意事项。

`79035ba7` 另以真实 `PlayerPage` 集成路径复测 4K Main：在新鲜 HeadlessChrome 151 中模拟原生 HEVC 不可用，
浏览器实际选择客户端 fallback，首帧约 11,493 ms，页面显示 `speedX≈0.37` 的低于实时提示；播放和暂停分别
上报 `PLAYING`/`PAUSED` 进度，浏览器控制台无播放相关错误。4K 选择器使用与 `@hevcjs/core` 一致的 H.264
High@5.1 (`avc1.640033`) 探测，避免 4K 错误使用 High@4.0 (`avc1.640028`) 而被 WebCodecs 拒绝。

因此当前这台 ARM64/HeadlessChrome 设备可以完成 4K HEVC 客户端 fallback，但 4K Main 和 Main10 均未通过实时
转码性能门。播放器会显示“客户端解码速度低于实时”的降级提示，并建议使用原生客户端或降低清晰度；不能把
本次结果外推到其他浏览器、硬件或目标 x86_64 NAS。

记录探针结果时必须包含浏览器版本、平台/设备、Lux 提交、`uname -m`、样本校验值、metadata 结果、实际
播放时长、VideoFrame 数量、丢帧和音画同步观察。不得写入完整媒体 URL、令牌、Cookie 或用户数据。

## Lux Web LUX-198 网页播放实测（2026-08-26）

本次验证使用本地临时媒体和隔离容器；没有把密码、Cookie、签名 URL 或用户数据写入记录。宿主机
`uname -m=arm64`，因此以下 ARM 结果不外推为 NAS/x86_64 性能结论。

| 路径 | 结果 | 证据 |
|---|---|---|
| Direct | 通过 | 本地有效媒体的签名 Direct 请求支持 Range 并返回 206；篡改签名、停止会话后的旧 URL 均被拒绝 |
| 服务端 HLS | 通过 | 本地媒体在服务端取得 `index.m3u8`、`init.mp4` 和多个 `.m4s`；首个分片生成前无需等待整部媒体完成，seek/暂停可用 |
| 会话事件 | 通过 | `PLAYING`、`PAUSED`、`STOPPED` 上报可用；重复 `eventId` 返回 `duplicate`，旧 `sequence` 返回 `stale`，播放位置不倒退 |
| 生命周期 | 通过 | 页面关闭、自然结束、心跳超时和服务重启后的孤儿目录均完成 HLS 进程/临时目录回收 |
| `.strm` | 通过 | 不支持 Direct 时响应为 `tier=0`、`UNSUPPORTED`、`STRM_REQUIRES_DIRECT_PLAY`；未创建 HLS 目录或 FFmpeg 进程，Lux 不代理媒体字节 |
| 浏览器引擎 | 通过 | Direct 原生 video、HLS.js/MSE 和 Safari 原生 HLS 路径均有单测；真实浏览器验证了 manifest、init、segment、seek、暂停和停止 |

运行时使用官方 [jellyfin-ffmpeg `v7.1.4-3`](https://github.com/jellyfin/jellyfin-ffmpeg/releases/tag/v7.1.4-3)
正式版。ARM64 与 AMD64 runtime/application 镜像均按固定 SHA-256 构建；容器内 `ffmpeg`、`ffprobe` 来自
`/usr/lib/jellyfin-ffmpeg`，未安装普通 Debian `ffmpeg`。该记录只证明构建与版本选择，不代表 AMD64/NAS 的转码性能。

## LuxPlayer LUX-203 至 LUX-208 Web 播放器阶段验证（2026-08-27）

本阶段的 LuxPlayer 使用 Lux 自己的 Controller、Engine contract、React UI 和播放会话接口；运行时和业务代码
不依赖 `artplayer` 包。ArtPlayer 仅作为可追溯的 MIT 实现参考，来源状态见
`docs/THIRD-PARTY-NOTICES.md`。本机宿主 `uname -m=arm64`；以下浏览器验证不能外推为飞牛 NAS 的
x86_64 性能或真实 iOS/Android 设备兼容性。

| 浏览器/平台 | 媒体样本 | 已验证能力 | 结果与边界 |
|---|---|---|---|
| Playwright Chromium 152.0.7977.8，Android 16 / Pixel 10 mobile emulation，宿主 macOS arm64 | 无个人数据的公开短 H.264/AAC MP4 夹具；播放会话 API 为本地 mock | Lux 播放会话创建、原生 video Direct 加载、Media Session metadata/playback state、播放/暂停 UI、进度事件与 heartbeat | 通过；真实视频请求返回两个 `206` Range 响应，最终 console 为 0 error / 0 warning，所有 Lux mock API 均为 2xx/204。离开播放页后 Media Session 变为 `none` 并清空 metadata；action handler 的 play/pause/seek 参数由组件与页面测试覆盖。未以系统锁屏/耳机按键实测。 |
| 同上 | 同一短夹具 | 触摸 pointer seek、safe-area/dvh 布局、320×700 / 768×1024 / 1440×900 viewport | 通过；从 0 秒水平触摸 seek 到约 2.53 秒（媒体时长 5.055 秒），三个 viewport 均无横向溢出且控制栏存在。真实刘海设备、横竖屏切换和浏览器全屏的物理 safe-area 仍待真机复测。 |
| 同上 | 不支持播放计划 mock | 可诊断失败状态 | 通过；显示“浏览器不支持此媒体”及 Lux 恢复建议，不回显后端计划原因、签名 URL 或引擎异常详情。资源过期、引擎失败和服务端计划失败的分类由单元/组件测试覆盖。 |

本阶段没有以本次短夹具宣称 4K、HDR、HEVC、服务端 HLS 或客户端 fallback 的实时播放能力；这些结论继续以各自
LUX-184、LUX-185 与 LUX-198 的样本和记录为准。页面可见性恢复、方向变化和 Media Session 不可用降级已有
Web 组件测试；仍需在至少一台真实 iOS 和一台真实 Android 设备上验证系统媒体控件、safe-area 与横竖屏行为。

## LuxPlayer LUX-209 控制层视觉回归（2026-08-27）

LUX-209 以 ArtPlayer 首页默认控制层和弹幕演示页的可见布局为参考，重新实现 Lux 自有 React 控制层；没有加入
`artplayer` 依赖，也没有复制其代码、DOM、CSS、图标或演示资源。验证使用无个人数据的本地 API fixture、公开短 MP4
媒体夹具和 Codex 内置 Chromium（宿主 `uname -m=arm64`）；结论不外推为 NAS/x86_64 性能或真实 iOS/Android 设备兼容性。

| viewport / 平台 | 已验证能力 | 结果与边界 |
|---|---|---|
| 1440×900，macOS arm64 | 46px 透明底部控制层、4px 基础时间轴/6px hover 时间轴、底部渐变、中心播放、版本选择、设置、画中画与原生全屏入口 | 通过；暂停和设置状态均可见，所有控件拥有可访问名称。独立 Lux 播放路由默认已占满视觉 viewport，因此没有无效的重复“网页全屏”按钮。 |
| 768×1024、390×844，macOS arm64 | safe-area、动态 viewport、中心播放、窄屏控制栏 | 通过；播放器自身没有横向溢出。窄屏将右侧非核心控制保留在可横向滚动的控制组中，键盘焦点可将目标带入视图；真实刘海设备仍待复测。 |
| 1440×900，Chromium DevTools 网络观察 | 弹幕显示开关 | 通过；`aria-pressed` 从 `true` 切换为 `false`，观察窗口内 `Network.requestWillBeSent` 为 0，console 为 0 error / 0 warning。没有输入框、发送动作、弹幕加载/渲染或热力图。 |
| Web 组件回归 | 本地 PNG 截图 | 通过；模拟已就绪视频帧后创建 canvas PNG 下载并清理 object URL，文件名剔除路径分隔符。跨域媒体不能导出帧时仅显示通用失败状态，不回显播放 URL。 |

## LuxPlayer LUX-212 至 LUX-214 弹幕阶段验证（2026-08-27）

本次使用单独的临时 SQLite 配置、无个人数据的短 H.264/AAC MP4 和已登记的 Bilibili XML 旁车；没有把
密码、Cookie、签名 URL 或完整播放地址写入记录。宿主机 `uname -m=arm64`，浏览器为
Chrome/Chromium `151.0.7922.174`；结论不外推为飞牛 NAS/x86_64 性能、真实 iOS/Android 设备或所有浏览器。

样本 SHA-256：MP4 `0cd83d944a6ca7822b4a8306cecc60a36e859b041f6702c6a1ad9ead78924451`（1,128,375 bytes，
时长 5.055 秒），XML `34a00dfa18be71bceec4c723290ba5403dda0a99a0b9558ab1992eb69de9306e`（104 bytes）。解析/调度
上限为 4 MiB、5,000 条、单条 200 字、时间 24 小时、字号 12-64px、同时渲染最多 80 条；支持滚动、顶部、
底部三种模式，文本使用 React 文本节点渲染。

| 浏览器/viewport | 已验证能力 | 结果与边界 |
|---|---|---|
| Chrome 151，macOS arm64，390×844 | Direct 播放、弹幕元数据/raw 读取、Worker 解析、seek、顶部/滚动弹幕安全区、控制栏 | 通过；元数据和 raw 均为 200，媒体 Range 为 206；seek 从 0 秒到约 60% 位置成功。弹幕层位于标题栏下方和控制栏上方，390 viewport 无横向溢出。 |
| Chrome 151，macOS arm64，768×1024 与 1440×900 | 控制栏、版本选择、截图、设置、画中画、全屏和弹幕开关的可访问名称 | 通过；三个 viewport 均无横向溢出，默认按钮均存在。 |
| Chrome 151，macOS arm64，390×844 | 关闭/开启弹幕及会话内生命周期 | 通过；`aria-pressed` 在 `true/false` 间切换，关闭时 overlay 被销毁且不创建新请求；开启后只读取 `/api/v1/items/{itemId}/danmaku`、`/danmaku/raw` 和同源 Worker。未请求 Emby `/api/danmu/*`，无外部请求、输入框、发送按钮、热力图或实时推送。 |
| Web 单测与 Rust 全量测试 | 恶意 XML/有界解析、lane 调度、source 生命周期、ACL/raw 合同 | 通过；Web 343 tests、Rust 273 passed/1 ignored，fmt、clippy、build 均通过。 |

2026-08-31 补充回归：Web 解析器现在接受 Lux 下载链路和 Rust 校验器均支持的标准
`<?xml version="1.0" encoding="UTF-8"?>` 声明，同时继续拒绝畸形声明、DOCTYPE/ENTITY、嵌套根节点与超限输入。
Chromium 隔离 smoke 已实际加载声明形式的 XML、启动同源 module Worker 并渲染文本，console 为 0 error / 0 warning；
Web 104 个静态规则测试和 433 个 Vitest 测试通过，生产构建通过。

2026-08-31 真实弹幕回归：生产站点的认证播放 smoke 观察到同源弹幕元数据/raw 请求均为 200，raw
响应为 2,937,001 bytes 的合法 Bilibili XML，包含 30,271 条 `<d>`；当时部署的 Worker 使用 5,000 条上限，
因此解析结果为空、DOM 中没有弹幕元素。修复将 Web 解析上限提高到 50,000 条，4 MiB 文件上限、单条 200 字、
24 小时时间上限和最多同时渲染 80 条保持不变；本次记录不包含账户、凭据、Cookie、签名 URL 或弹幕文本。

## LuxPlayer LUX-215 字幕/弹幕联合阶段门（2026-08-27）

本次在 Lux `4a7c3193` 上使用隔离 SQLite 配置、固定本地夹具和 Chrome `151.0.7922.174`。宿主为
macOS `arm64`（`uname -m=arm64`）。浏览器运行未记录密码、Cookie、签名 URL、完整播放地址或用户数据。

固定夹具及 SHA-256：

| 夹具 | SHA-256 | 大小/媒体信息 | 可见文本 |
|---|---|---|---|
| H.264 High/AAC | `0cd83d944a6ca7822b4a8306cecc60a36e859b041f6702c6a1ad9ead78924451` | 1,128,375 bytes；960×540；5.055 秒 | Direct 与服务器 HLS 媒体源 |
| H.264 SRT | `3ffa23c6797bddf9b2bc52a9daa77d92d58da5b469153c356d3865bf4340f686` | 55 bytes | `LuxPlayer 字幕夹具` |
| H.264 Bilibili XML | `34a00dfa18be71bceec4c723290ba5403dda0a99a0b9558ab1992eb69de9306e` | 104 bytes | `Stage 17 fixture`、`Top fixture` |
| HEVC Main/AAC | `7bb8cea1db72a27e39b7c4d0b574880b0cb9399865b32df4109acfdc960831a6` | 108,302 bytes；960×540；5.056 秒 | 客户端 fallback 媒体源 |
| HEVC SRT | `09975e9f8968df16f35fdd7d01e46eb9ef75978d524dc2f171ea73e97068b6d7` | 61 bytes | `LuxPlayer 切换字幕夹具` |
| HEVC Bilibili XML | `070a5865046cf3f704aa858ffa6288ddc5ff3f7cf475a27010fe217d435a28f8` | 117 bytes | `HEVC switched fixture`、`HEVC fixed fixture` |

解析/调度继续使用 LUX-212 至 LUX-214 记录的上限：字幕 1 MiB/5,000 cue，弹幕 4 MiB/5,000 条、
单条 200 字、最多同时渲染 80 条；本任务没有提高上限或新增解码引擎。

| 路径/场景 | 结果 | 证据 |
|---|---|---|
| Direct | 通过 | 最终播放计划为 `DIRECT`；媒体 Range、SRT 字幕、弹幕元数据/raw 和覆盖层渲染均成功。 |
| 服务器 HLS | 通过 | 浏览器从 Direct 降级到最终 `SERVER_HLS`，实际读取 `index.m3u8`、`init.mp4` 和 `.m4s` 分片；字幕与弹幕保持可见。 |
| 客户端 HEVC fallback/source 切换 | 通过 | 同页从 H.264 source 切换到 HEVC source；实际读取 `/hevc/transcode-worker.js`、`/hevc/hevc-decode.js`、`/hevc/hevc-decode.wasm`，最终 video 使用 `blob:` MSE。旧会话先停止，H.264 字幕/弹幕文本消失，HEVC 的不同文本出现，字幕和弹幕覆盖层各不超过一个。 |
| 390×844、768×1024、1440×900 | 通过 | 三条播放路径均检查字幕、弹幕、标题栏和控制栏几何关系；无相互重叠、无横向溢出。设置、截图、画中画、全屏具有可访问名称，时间轴可获得键盘焦点。 |
| 输入与生命周期 | 通过 | 键盘 seek 和 CDP 触摸 seek 成功；离开播放页后活动会话停止，字幕/弹幕 DOM 清空。source/engine 迟到错误、请求/解析失败和重复 destroy 另由 Web 回归覆盖。 |
| 网络与控制台 | 通过 | 三次运行只出现声明的同源 Lux 页面、静态资源、HEVC 资源、认证、条目、字幕、弹幕、播放会话、Direct/HLS 端点；无 Emby 弹幕请求、无外部请求，console 为 0 error / 0 warning，page error 为 0。 |

2026-08-28 质量门：`pnpm --dir web install --frozen-lockfile`、83 个 Node 检查、344 个 Vitest 测试、
TypeScript/Vite 生产构建、`cargo build --locked`、完整 `cargo test --locked --all-targets`、rustfmt 和
clippy `-D warnings` 全部成功。Rust 主库为 273 passed / 1 ignored；另有 3 个显式性能探针和 4 个需要本机
PostgreSQL 的集成测试按测试声明保持 ignored，不属于本阶段浏览器播放器验收。

上述证据只验证 Chrome 151、macOS arm64 和 960×540 固定夹具，不代表真实 iOS/Android 的刘海安全区、
横竖屏、系统锁屏/耳机按键，也不代表飞牛 NAS/x86_64、4K/HDR 或其他浏览器的性能。真机差异仍需单独记录；
本机结果不得外推。

## LuxPlayer LUX-217 循环、比例与镜像（2026-08-28）

本次在 Lux `8247db44` 的当前 Web 产物上使用隔离 Docker fixture、一次性测试账号和 Playwright Chrome
`151.0.7922.174`。宿主为 macOS `arm64`（`uname -m=arm64`）；测试密码、Cookie、签名 URL、完整播放地址和
用户数据未写入输出。Chrome DevTools MCP 未配置，因此使用仓库既有 Playwright smoke 所采用的隔离浏览器方式，
并在播放器媒体就绪后检查 DOM、实际盒子尺寸、网络计数、console 和 page error。

| 播放路径 | 结果 | 证据 |
|---|---|---|
| Direct | 通过 | 最终计划为 `DIRECT`；开启循环、`4:3`、水平镜像后 video 的实际盒子比例为 `1.3333333333`，`loop` 属性和 `scaleX(-1)` 生效。 |
| 服务器 HLS | 通过 | 计划由 `DIRECT` 降级为 `SERVER_HLS`；同一设置操作和几何断言通过，未产生外部请求或播放器错误。 |
| 客户端 HEVC fallback | 通过 | 强制原生 HEVC 不可用后出现 Lux 客户端解码准备态；同一循环、比例和镜像合同通过，默认/正常切换清除动态样式。 |
| 设置副作用 | 通过 | 三条路径在设置操作前后播放会话/事件请求计数不变；切换比例与镜像只更新当前 video 呈现，不创建会话、不上报进度。 |
| 可访问性与布局 | 通过 | `循环播放` 使用 `role=switch`/`aria-checked`，比例和镜像使用原生按钮/`aria-pressed`；390×844 截图中设置面板无横向溢出，三个路径均有截图留档。 |

页面媒体就绪前的未认证 `/api/v1/auth/me` 和无头像 fixture 的 `/api/v1/auth/avatar` 404 属于夹具启动噪声，未计入
播放器交互窗口；媒体就绪后的 console、page error 和外部请求均为 0。Web 质量门为 `pnpm --dir web test`
（83 个 Node 检查、347 个 Vitest 测试）和 `pnpm --dir web build`，均通过。该记录证明 LUX-217 行为，不代表
Safari AirPlay、真实 iOS/Android 或其他浏览器的性能兼容性。

## LuxPlayer LUX-222 阶段 18阶段门（2026-08-28）

本次阶段门使用当前提交 `19a785cb` 的 LuxPlayer、Chrome `151.0.7922.174`、macOS `arm64`（`uname -m=arm64`）和
隔离 Playwright Chromium。播放器页面和媒体响应使用本地 Vite 与一次性无个人数据夹具；没有连接真实账户，不记录
密码、Cookie、签名 URL 或用户数据。Rust Direct、服务器 HLS、客户端 HEVC fallback 的媒体/会话真实性证据沿用
LUX-215 的固定夹具和记录；本次只增加阶段 18的播放器交互断言，不改变播放计划或运行时行为。

| 场景 | 结果 | 证据与边界 |
|---|---|---|
| 390×844、768×1024、1440×900 | 通过 | 标题栏、字幕和控制层保持安全间距；页面无横向溢出；控制层盒子完整位于 viewport；390px 下右侧控件组使用受限横向滚动；设置、章节、播放进度、弹幕开关、截图、画中画、全屏和版本控件有可访问名称。 |
| 设置与章节 | 通过 | 循环、`4:3`、水平镜像和 `+1.2s` 字幕偏移应用到当前 video，设置操作前后 session 创建数不变；章节按钮可 focus、带标题和时间，点击 seek 到开场；`CREDITS_START` 可见；片头区间只显示“跳过片头”并 seek 到结束。 |
| 字幕、弹幕和隐藏态 mini progress | 通过 | SRT 文本覆盖层和 Bilibili XML 弹幕均可见且不进入标题/控制安全区；控制层自动隐藏后 mini progress 显示 played/buffered 比例，`aria-hidden=true`、`pointer-events:none`，重新活动后移除。没有弹幕输入/发送器或热力图。 |
| AirPlay 能力门 | 通过 | 隔离浏览器分别模拟 WebKit 播放目标 `available` 与缺失能力；可用时 AirPlay 控件出现并调用当前 video picker，不创建 session；不可用时控件不出现且无错误。该模拟不等价于真实 Safari/AirPlay 设备。 |
| source 生命周期 | 通过 | 从主 source 切到备用 source 后，旧章节、旧片头跳过按钮和旧字幕/弹幕不残留；新 source 章节出现；页面离开后会话清理。Direct/HLS/fallback 引擎销毁和会话事件继续由 LUX-215 与 Web 生命周期测试覆盖。 |
| 网络与控制台 | 通过 | 当前 fixture 运行的请求均为声明的同源 Lux/Vite/媒体/播放会话路径；无外部请求、无 Emby 弹幕路径；console error/warning、page error 均为 0。正式脚本通过 `LUX_E2E_STAGE18=1`、`LUX_E2E_AIRPLAY_MODE=available|unavailable` 和可选截图目录复现这些断言。 |

本次阶段门没有验证真实 Safari 的 AirPlay picker、iOS/Android 刘海安全区、系统锁屏/耳机按键，也没有把 Chrome 模拟结果
外推为飞牛 NAS/x86_64 性能。当前没有新增 ArtPlayer 运行时、依赖、代码复制或衍生 notice；ArtPlayer 仍仅按
`docs/THIRD-PARTY-NOTICES.md` 固定 commit 台账作为交互和生命周期参考。

## LUX-229 本地与远程 `.strm` 字幕兼容性阶段门（2026-08-29）

本阶段使用当前 Lux 工作树中的固定、无个人数据元数据夹具和 Rust/Web 回归；代码夹具提交为 `24730578`，宿主机
`uname -m=arm64`，本机可用 Chrome 基线为 `151.0.7922.175`。本次 LUX-229 定向证据实际使用 jsdom 和 Rust HTTP 集成，
没有使用真实一次性 UA/令牌网盘资源进行浏览器实测。哈希是对应测试对象经稳定 JSON 序列化后的 SHA-256，不是远程媒体
内容哈希；夹具不包含真实网盘地址、凭据、Cookie 或媒体字节。

| 固定夹具 | SHA-256 | 内容边界 |
|---|---|---|
| 本地 `LOCAL_FILE` MKV 字幕轨记录 | `f35df8cb0d5254e4756430d50f7b4653039c46df0da7289b7d78f65998c2decf` | 视频轨 `h264`；内嵌 `subrip`、`ass`、`ssa`、`hdmv_pgs_subtitle`、`sup` 五轨；SRT 默认；所有字幕轨 `isExternal=false` |
| URL 型 `.strm` 字幕轨记录 | `d3db0f84d9ed407c9405108f1f58235006867908343fc3c2620644823873ea42` | 远程文本轨元数据仅用于验证 native-track 映射；URL 目标为合成地址，未由测试访问 |
| Path 型 `.strm` 字幕轨记录 | `d2478122ca885f9ca87fdbe86dc28ef48a4aad249947f1f350df11fc47ac5110` | 与 URL 型相同的文本/PGS 元数据，目标为合成路径；未由 Lux 读取 |

| 路径/能力 | 结果 | 请求边界与证据 |
|---|---|---|
| 本地内嵌 SRT/ASS/SSA | 通过 | `playerCaptionOptions` 将三种文本轨标为可选；只有选择后才请求 source-scoped 字幕端点，交给现有 Worker/overlay；seek、source 切换、fallback 和离开页面清理旧 cue/Worker/AbortController |
| 本地 PGS/SUP | 明确不支持 | 两种轨道显示“当前不支持此字幕格式”，不生成字幕请求；视频播放路径不受影响，不实现烧录或图形字幕解析 |
| URL/Path `.strm` 无 native track | 通过 | 两种 `.strm` 均不创建 Lux 字幕 URL，不调用字幕 `fetch`；视频继续走既有 Direct Play/外部代理路径 |
| URL/Path `.strm` 有 native track | 通过 | 只有当前 `video.textTracks` 实际暴露的轨道可选；只切换 track mode，不改媒体 URL、播放会话、tier、进度或心跳 |
| 单次媒体读取实验关闭/失败 | 通过 | 默认关闭；失败回归只消费调用方提供的同一 `ReadableStream`，不在实验内 `fetch`、不重试第二条连接、不切换 HLS/媒体代理；视频保持原 Direct Play |
| Rust 播放边界 | 通过 | `cargo test --locked --test web_playback` 验证固定本地字幕轨从 Web item 合同读回；`tests/strm_resolver_playback.rs` 验证远程解析响应为 307 且响应体为空，未携带媒体字节 |

Web 定向证据为 LUX-229、字幕实验和字幕选择共 18 个 Vitest 测试通过；最终 Web 全量为 Node 88/88、Vitest 401/401，
TypeScript 检查和生产构建通过；Rust 定向播放测试 1 个通过，Rust all-targets 为 297 个主库测试通过、1 个按约定忽略，
所有集成目标通过。现有 Chrome 151
播放器阶段门已验证 Direct/HLS/fallback 的控制台和请求清洁度，但本次没有真实一次性 UA/令牌网盘资源可供安全复测，因此
没有把合成夹具升级为真实远程 `.strm` 兼容声明。Chrome/Playwright 真实浏览器的通用播放、字幕 overlay 生命周期和请求观察
结果继续以 LUX-215/LUX-222 记录为准；真实 Safari、Firefox、移动端浏览器，以及远程资源是否暴露 native Matroska 文本轨，
仍未验证。

`cargo fmt --all -- --check` 通过；在干净的 LUX-229 修订 `1ef52303` 工作树中，`cargo clippy --locked --all-targets
--all-features -- -D warnings` 通过。当前共享分支后续无关扫描批处理代码另有两处 clippy lint，未纳入本任务修复。

本阶段没有新增字幕专用 302/Redia 接口、远程媒体代理、ffmpeg/ffprobe 读取、PGS/SUP 支持或 ArtPlayer 运行时依赖。Rust
测试和本机 `arm64` 结果不外推为 NAS/x86_64 性能；发布前仍需项目所有者确认后关闭阶段。

## Lux Web Chrome 隐私浏览模式 CSRF 兼容性（2026-08-31）

本次回归针对经 NextEmby 代理访问 Lux、且 Chrome 隐私浏览模式无法读取或写入客户端 Cookie/Web Storage 的场景。
登录响应中的 CSRF nonce 现在同时保留在当前页面内存中；若浏览器允许，则继续写入同源 Cookie 和 Web Storage。当前页面
在客户端存储不可用时仍可为收藏、已看、播放会话和退出请求发送 `X-CSRF-Token`，会话认证仍只依赖 HttpOnly 的
`lux_session` Cookie。

| 场景 | 结果 | 证据与边界 |
|---|---|---|
| React Web API 客户端 | 通过 | Vitest 覆盖登录响应保存 nonce、客户端 Cookie/Storage 同时阻断，以及收藏、已看、播放会话和退出请求继续携带 CSRF 请求头 |
| 旧版静态 Web 入口 | 通过 | Node 测试覆盖 Cookie/Storage 同时阻断时的 nonce 读取、写请求请求头和退出清理 |
| 真实 Chrome 隐私窗口 | 未在本次本机回归中单独宣称 | 当前证据是浏览器存储阻断模拟；真实 Chrome、扩展和代理组合仍需在部署后的 8098 现场复测 |

该修复不保存会话令牌、不改变服务端 CSRF 校验或 NextEmby 行为；隐私模式关闭页面后，内存 nonce 会随页面销毁，用户需重新登录。

## 记录格式

每次探针或回归测试至少记录：客户端版本、平台版本、Lux 提交、请求路径序列、脱敏请求参数、状态码、关键响应字段、结果和已知差异。密码、token、Cookie、真实 `.strm` URL 和用户数据不得进入 fixture 或文档。

## 当前状态

### Emby 迁移插件（LUX-190 / LUX-191+）

`org.lux.emby-migration` 已实现为独立进程插件，Lux 宿主已实现后台任务、用户映射、媒体匹配、UserData
导入、首次登录密码验证、幂等记录和管理员报告接口。当前仍未在本机连接到受控 Emby 实例，因此真实
版本字段和完整事件端点尚未完成现场验证；不把本地 fixture 或聚合 UserData 当作真实事件证据。

用户状态迁移按 `PLAYED`、`FAVORITE` 和 `RESUMABLE` 三种 Emby 查询筛选读取，只对返回的有状态媒体执行
Lux 匹配；重叠结果按 Emby 条目 ID 去重。该优化不改变 `ITEM_STATE` 的字段范围，也不把聚合 UserData
升级为播放事件历史。

迁移宿主对旧版插件返回的 `PLUGIN_INVALID_RESPONSE` 做分页二分恢复：当单页中某个媒体条目导致整页
响应无法解析时，先拆分请求定位该条目，无法恢复的单条会以 `SKIPPED` 状态写入媒体匹配报告、计入
`failedCount`，然后继续后续页面。已成功解析的用户/媒体名称会把控制字符规范化为空格并裁剪首尾空白，
不修改 Emby 源数据。该兼容兜底不吞掉网络、认证、插件进程或数据库错误；这些错误仍会暂停任务并保留
失败阶段，便于修复后恢复。

2026-08-29 的迁移范围与吞吐优化将 Web 配置收敛为三个明确步骤：选择来源用户、选择迁移类别/目标 Lux
媒体库、确认启动。新流程默认不勾选任何类别；服务端也拒绝空范围，因而不会创建任务或保存 secret。未选择
的媒体状态、人物收藏不会发起对应 Emby RPC，也不会加载相应身份索引。涉及媒体状态或媒体库权限的新请求必须
选择目标库；历史请求没有该字段时继续按全部启用库处理。

目标库白名单以“禁止写入”为边界：白名单外条目仍保留匹配方法和 `TARGET_LIBRARY_EXCLUDED` 跳过报告，方便
管理员复核，但绝不写 `user_item_state`、导入记录或媒体库 ACL。来源用户没有访问权、或来源目录映射不唯一时也
只跳过，不能静默撤销 Lux 中已有的权限。为了保留这些可解释的跨库跳过报告，媒体状态迁移会在首个有效状态页
出现后按需加载只读的、分页的媒体身份索引；它不触发扫描、NFO、`ffprobe` 或写入，且只在选择媒体状态时加载。

支持 `supportsFilteredReads` 的新插件会进一步在来源端限制已选用户、来源虚拟媒体库和 UserData 字段；宿主在
`migration.list_users` 支持可选 `startIndex`、`limit`、`search` 及分页元数据；旧插件返回完整列表时宿主本地切页并
保留兼容行为。宿主转发已选用户 ID，在 `migration.user_state` 转发 `stateFilter`、`stateFields` 和
`sourceLibraryIds`。多个状态筛选同时启用时，`stateFields` 是已选字段并集，跨筛选按 Emby 条目 ID 去重仍能完整
保留已选状态；未选字段不会发送或写入。执行阶段还会在 `migration.list_users` 转发按范围计算的 `userFields`，只
读取用户资料或库权限所需字段；配置页用户预览只投影 ID、名称和状态字段，不读取虚拟媒体库目录。旧插件缺少该可选能力时不会收到新字段，可能仍读取较大范围，但宿主仍严格
禁止未选目标库的状态、导入记录和 ACL 写入。按页 Provider 候选查询使用 schema 107 的 `media_item_provider_ids` 索引，
不再加载全量媒体身份；异常页恢复最多 32 次来源 RPC，超出范围会记录 `sourceRangeLimit` 并推进游标。
同一迁移运行内跨用户重复的媒体候选页可复用固定容量的只读缓存，缓存键包含目标库白名单，不缓存用户状态或写入结果。

当前 `Lux-plugins` 的 `org.lux.emby-migration` 0.1.5 已实现该能力，并对多个已选用户使用固定上限 8 的并发读取；本仓库的
宿主和插件本地 HTTP fixture 均已验证过滤字段、空来源库短路、请求取消和并发上限。真实 Emby 版本对投影参数的实际执行仍需
受控实例复测，旧插件仍按兼容路径处理。

每 500 个来源条目在一个数据库事务中提交匹配报告、状态、导入记录、已处理条目与恢复游标；每类多行写入再按
100 条拆分，避免 SQLite 参数上限。相同状态或权限不会产生无意义更新；同一任务和不同迁移任务均通过单执行者
闸门串行，避免对 Emby 和 SQLite 施加无界并发。合成页面测量、硬件与不外推限制记录在
`docs/EMBY-MIGRATION-PERFORMANCE.md`；尚未连接受控 Emby 实例，因此这些数据不声明真实 Emby 版本或 NAS/x86
性能。

本轮按需优化还包括：新任务不查询空的已处理标记表，只有恢复中的用户才启用持久化去重；来源没有有效状态时不
加载完整媒体身份索引；来源没有有效人物收藏时不加载人物身份索引；用户资料字段没有变化时跳过用户更新事务；Web
只有在选择媒体状态或库权限后才加载目标库。

| 能力 | 当前状态 | 说明 |
|---|---|---|
| `ITEM_STATE` | 已实现，真实版本待验证 | 已看、播放位置、播放次数、最近播放时间和收藏 |
| `EVENT_HISTORY` | 未声明 | 当前插件不伪造事件；只有公开 API 返回真实事件时才允许升级 |
| 用户资料/权限 | 已实现，真实版本待验证 | 用户名、显示名、启用状态、远程访问、内容下载和按 Emby 虚拟媒体库 ID/名称/路径映射的媒体库访问权限；Emby 管理员不会自动成为 Lux 管理员 |
| 密码迁移 | 已实现，真实版本待验证 | 不读取密码哈希；首次 Lux 登录时向 Emby 验证一次原密码 |

Emby 官方源码中的 `SqliteUserDataRepository` 只持久化 `played`、`playCount`、`isFavorite`、
`playbackPositionTicks` 和 `lastPlayedDate` 等条目聚合状态，没有公开事件流字段；官方 Session API
描述的是当前会话列表，不是历史播放事件。因此当前能力保持 `ITEM_STATE`，直到受控实例证明存在可用的
公开原始事件端点：

- https://github.com/MediaBrowser/Emby/blob/master/Emby.Server.Implementations/Data/SqliteUserDataRepository.cs
- https://github.com/MediaBrowser/Emby/blob/master/MediaBrowser.Api/Session/SessionsService.cs

迁移插件不属于 Emby 客户端兼容承诺，不改变 Emby 兼容 DTO 或目标客户端矩阵。

### Lux 原生出站 Webhook

Lux 当前提供一个版本化的原生 Webhook 合同（`schemaVersion: 1`），用于发送媒体、扫描、元数据、后台任务
和播放事件。请求使用 `X-Lux-Event-Id`、时间戳和 HMAC-SHA256 签名，投递为至少一次语义，接收方应按
`eventId` 幂等。Webhook 目标可以选择 Lux 原生或 Emby 风格的有限 DTO payload；两者均经过字段白名单和脱敏
处理。该功能不是 Emby Webhooks 插件的完整兼容实现，不支持未列入测试合同的模板变量、插件事件或行为。

当前只提供 Webhook 渠道；Telegram、企业微信和 Email 尚未实现。

- 媒体库实时监听默认开启。复制到已配置根路径中的新视频会进入局部 `INCREMENTAL_SCAN`，只处理该事件路径，通常在几秒内进入索引；全量调和遇到待处理的局部任务会在当前批次提交后让出唯一扫描锁，优先完成局部索引；旧版 `realtimeWatchEnabled` 请求字段不会关闭监听。
- LUX-000 至 LUX-003：仅完成仓库工程检查，尚未连接任何真实客户端。
- LUX-023：已完成根路径/`/emby` 前缀的 System/Ping 本地协议 shape 测试；`GET/POST /System/Ping` 按 Emby OpenAPI 兼容为无需认证的空 200，并完成 VidHub/SenPlayer 真实登录前置探针。
- 2026-08-23 Infuse 8.5.5726（macOS arm64）连接探针发现：登录成功后请求 `/DisplayPreferences/usersettings?userId=:userId&client=:client`，此前因路由缺失落入 Web 首页 HTML，导致 JSON 解码失败；现已补齐带认证的 `GET /DisplayPreferences/{displayPreferencesId}`，根路径和 `/emby` 前缀均有自动协议回归。完整 Infuse 媒体库、播放和状态流程仍待部署后复测。
- LUX-024：已完成 Users/Public、AuthenticateByName、Sessions/Logout 的本地协议 shape 和 token 脱敏测试；VidHub 真实登录通过；SenPlayer 认证响应解析失败的历史缺口已补充更完整的 `User`/`SessionInfo` shape，并补齐认证后 `GET /Users/:userId` 用户详情路由；P0 真实 UI 复测已通过。
- 已补齐 Emby 用户写接口的基础兼容：`POST /Users/:userId`、`POST /Users/:userId/Policy`、`POST /Users/:userId/Password` 和 `POST/GET /Users/:userId/Images/Primary` 直接复用 Lux 的用户存储与头像存储，避免第三方管理端在改名、改密或同步头像时落到 405。
- Emby `GET /Items/Counts` 现已支持根路径和 `/emby` 前缀，执行 Emby token/API key 鉴权、用户媒体库 ACL，并支持 `UserId` 与 `IsFavorite` 过滤；`tests/emby_counts.rs` 覆盖协议响应。尚未以 Filmly 或 CapyPlayer 真实 UI 复测，不据此宣称客户端兼容。
- Emby 与 Lux 媒体库列表的 `SortBy=PremiereDate`/`sortBy=PremiereDate` 在条目没有完整发行日期时回退到 `ProductionYear`，并将两者都缺少的条目稳定放在最后；发行日期新旧两个方向均有服务端回归覆盖。
- 2026-08-23 AfuseKt `2.9.8.6-fix`（Android）HAR 显示其媒体库首页请求会发送空值 `IsFavorite=`；此前 `/Users/{userId}/Items` 和 `/Items/Latest` 因此返回 400，导致库条目与最新资源不显示。Emby 查询现在将空的可选布尔值按未提供处理，`tests/catalog.rs` 已加入 Items/Latest 回归；修复后的真实客户端复测待部署后进行。该客户端请求的 `/Genres` 仍属于规范明确未实现的端点。
- 2026-08-11 服务器名称兼容修复：第三方客户端添加服务器时可从 `GET /System/Info/Public` 的 `ServerName` 读取名称；认证后的 `GET /System/Info`、`Users/Public`、`AuthenticateByName` 返回的 `User.ServerName` 也统一读取管理员设置的 `serverName`。官方 Emby 文档将 `ServerName` 定义为服务器名称字段；本次加入 `tests/emby_system.rs` 协议回归，尚未在当前环境重新进行 VidHub UI 点选复测。
- SenPlayer 列表兼容修复：当请求的 `Fields` 未包含 `MediaSources` 或 `MediaStreams` 时，Emby 列表响应不再携带这些字段；详情和 `PlaybackInfo` 仍返回完整媒体源。自动化回归已覆盖，真实客户端需要清理缓存或重新进入库后复测。
- 2026-08-17 SenPlayer `.strm` 直放地址编码修复：HTTP(S) 目标包含 Unicode 路径或查询参数时，Emby 视频端点现在先将 URL 规范化为合法的百分号编码 `Location`，再返回原有 307 直连；数据库仍保留原始目标，不代理媒体字节。新增 API 单测和 `.strm` 集成回归，真实 SenPlayer UI 需重新部署后复测。
- LUX-196 有序媒体库刮削器：Lux 管理 API 和 Web 管理页支持按顺序配置多个 metadata 插件；首位固定为主刮削器，后续项可设为补充、备用或两者兼具。Emby 兼容 DTO 不变。自动化测试覆盖旧单值 `scraperId` 兼容、角色排序、不可用已选插件、实际命中来源记录和补充元数据保护；真实第三方客户端尚未因该管理配置变化重新实测。
- 2026-08-24 STRM 播放请求归属修复：HTTP(S) `.strm` 的 `DirectStreamUrl` 返回 Lux 的受保护播放入口；入口由 Lux 直连请求上游并转发 VidHub/SenPlayer 等实际播放器的 User-Agent，有限解析 302 后向播放器返回最终地址的 307。Lux 不代理媒体字节，也不经过全局出站代理。真实客户端播放需重新部署后复测。
- Emby `GET /Items` 对标准 ItemId 仍按逗号分隔的 `Ids` 严格过滤；不存在的 ItemId 或 UUID 返回空 `Items` 和 `TotalRecordCount: 0`。针对 Redia 的兼容兜底见下一条。
- Redia 兼容兜底：`GET /Items?Ids=<MediaSourceId>` 在没有同名 ItemId 时会解析到该媒体源所属条目；未知 ID 仍返回空结果，不会回退到媒体库第一条。`/Videos/{ItemId}/original.strm`（含 `/emby` 和大小写路径变体）复用 Emby 播放逻辑并对 STRM 返回 307 直连；其他未注册 `/Videos/...` 路径返回 404，不再落入 Web 前端 fallback 返回 HTML。标准客户端仍应使用 ItemId 和 `/Items/{ItemId}/PlaybackInfo`。
- 2026-08-28 Yamby 2.0.5.5 HAR 兼容修复：客户端请求 `PlaybackInfo` 时带有 Emby 鉴权头，但随后独立的媒体请求没有转发任何鉴权头，导致 Lux 视频入口返回 401。Emby `MediaSources[].AddApiKeyToDirectStreamUrl` 仍声明为 `true`，但新 HAR 证明该版本 Yamby 忽略此字段，因此 Yamby 专用 `DirectStreamUrl` 现在额外携带绑定用户、条目和媒体源的短期 HMAC 播放票据；服务端验证票据后允许无请求头直放，长期 Emby token 不进入 URL。`tests/playback.rs` 已覆盖无头播放和篡改票据拒绝。该 HAR 还显示 Yamby 进入季度后以季 ID 同时作为 `/Shows/{id}/Episodes` 的路径参数和 `SeasonId`，现已兼容解析到父系列并返回该季度的集；`tests/series_api.rs` 已加入回归。远端部署需更新后重新播放验证。
- 2026-08-28 通用 Emby 无头播放兼容：发现 HILLS 等 Android 播放器也可能在独立媒体请求中丢失 Emby 鉴权头。`PlaybackInfo` 现在对所有可播放媒体源生成绑定用户、条目和媒体源的短期 HMAC `DirectStreamUrl`，并将 `AddApiKeyToDirectStreamUrl` 设为 `false`，避免客户端再次把长期 token 追加到 URL；标准 `X-Emby-Token`、`X-Emby-Authorization` 和 `api_key` 仍继续接受。`.strm` 播放入口继续使用媒体请求中的播放器 User-Agent 解析有限重定向；自动化回归已覆盖无头本地播放、远程 307 和协议解析器路径。HILLS/Yamby 真机需在远端部署后重新复测。
- 2026-08-30 Lux Web 远程 HTTP(S) `.strm` 307 直放：播放器请求 Lux 的短期签名播放入口，由 Lux 使用入站播放器 User-Agent 解析上游有限重定向并返回 307；媒体字节仍由浏览器直连最终地址。远程 `.strm` 保持原生直放，不进入需要 CORS `fetch` 的客户端 HEVC/MKV fallback；复杂格式兼容性按浏览器原生能力和现有客户端单独判断。
- 2026-08-26 LUX-199 Redia 兼容：`MediaSource.DirectStreamUrl` 现在使用标准 Emby 的 `/Videos/{ItemId}/stream[.Container]?MediaSourceId={MediaSourceId}`，旧的媒体源路径入口继续保留；`GET /Items/{MediaSourceId}` 及 `/emby` 前缀可返回所属可见条目的 `MediaSources[].Path`。路径型 `.strm` 仍保持 `Protocol=File`、`IsRemote=false`，Lux 不访问或代理云盘媒体。自动化测试已覆盖标准查询播放、历史 URL、编码查询参数和媒体源 ID 详情；真实 Redia 联调需远端重新部署后复测。
- 2026-08-26 Redia 起播性能定位与优化：发现 Redia 在标准视频入口前会发起单个 `GET /Items?Ids=...` 查询，远端冷请求约 9–12 秒，而直接 `GET /Items/{id}` 约 0.3–0.4 秒。Lux 现对无冲突过滤条件的单个 `Ids` 查询直接按条目或媒体源 ID 查找，保留多 ID、筛选和分页请求的原有分页语义；之前的本机 VidHub 日志显示已预热源可在约 4 秒内开始回报进度，未预热源的等待仍属于 Redia/115 直链生成。补充实测：本机 VidHub 当前连接的远程 Luxx/Redia 实例，冷起播两次从 `MPVPlayerManager init` 到首帧相关的 `Has end credit time` 事件分别约 23.1 秒和 22.0 秒；工作树尚未部署到该远程实例，因此这不是本次代码优化后的结果。
- 2026-08-26 LUX-199 Web/Redia 补充：`/api/v1/playback/sessions` 对路径型 `.strm` 的 Direct Play 计划额外返回标准 `/Videos/{ItemId}/stream?MediaSourceId={MediaSourceId}` `proxyUrl`，Web 播放器优先使用该地址，因此 Redia 代理 Lux Web 页面时也能接管并映射到云盘直链；代理请求失败时回退到原有 Lux 短期签名 `url`，普通本地源和 URL 型 `.strm` 继续只使用签名地址。媒体源 ID 详情查询改为轻量路径，避免为 Redia 读取演员、NFO 和图片比例。自动化回归已覆盖，真实 Redia/Web 联调需部署后复测。
- `cargo` 验证是在本机 `arm64` 上完成，不代表目标 x86_64 飞牛 NAS 性能或客户端兼容性。
- Web 的“已实现”仅表示代码路径和服务端静态集成已完成；当前 Chrome smoke 覆盖登录、筛选、播放、收藏、账户会话和管理流程，不等同于所有浏览器/编码格式兼容。
- LUX-193 的演员收藏属于 Lux Web 自有 API 和人物详情能力，不扩展 Emby `Persons` DTO 或 Emby `FavoriteItems` 语义；演员收藏的目标客户端兼容性尚未单独实测。
- LUX-121 兼容补齐：Emby `Views` 返回媒体库类型、`ChildCount` 和标准 `ImageTags.Primary`；条目详情同时返回本地徽标的 `ImageTags.Logo`，并通过 `/Items/{itemId}/Images/Logo` 提供标准图片读取；媒体库封面支持 `/Items/{libraryId}/Images/Primary` 及带索引、HEAD、ETag 和 ACL。尚待 VidHub UI 重新实测确认。
- 2026-08-28 混合媒体库兼容修复：所有 Emby 客户端在混合库 DTO 中统一收到 `CollectionType: null`，并通过 `TypeOptions` 表示电影和剧集类型；这是 Emby 兼容的历史返回形状，避免 Yamby、VidHub 等客户端丢弃混合库。服务端不再按客户端名称分流；`tests/mixed_library_api.rs` 覆盖普通 Emby、VidHub 和 Yamby。
- Emby `GET /Library/VirtualFolders` 现返回接近官方 `VirtualFolderInfo` 的完整结构：`Id`、`Guid`、`ItemId` 使用同一个稳定媒体库 ID，`LibraryOptions` 包含 `PathInfos`、按电影/剧集类型拆分的 `TypeOptions`、Lux 当前图片策略、NFO 本地元数据策略、字幕语言和播放恢复阈值。Lux 没有等价 Emby 刮削器时，metadata/image fetcher 数组保持为空；尚未以目标第三方客户端真实 UI 复测该管理端点。
- Emby `GET /Persons` 已支持 `Recursive`、`Fields`、`SortBy=Name|DateCreated`、`SortOrder` 和任意正整数 `Limit`；返回去重后的演员 `Items`、`TotalRecordCount` 及 `DateCreated`，顶层结构与 Emby 一致且不额外返回 `StartIndex`。人物 DTO 使用 Emby 的 `Type: "Person"`，并补齐 `ServerId`、`ImageTags` 和 `BackdropImageTags`。支持根路径和 `/emby` 前缀、Emby token/API Key、媒体库 ACL，并在服务启动后台回填已有 `people.json` 关系到人物索引。`tests/people_api.rs` 已覆盖共享 API Key、跨条目聚合、字段投影、排序、超大 Limit、前缀和回填；尚未以目标第三方客户端真实 UI 复测。
- 人物关系快照恢复若无法唯一匹配当前媒体源，不再每次启动重复告警；快照会原子移动到 `/config/metadata/quarantine/people-relations/`，对应 `person_credits` 运行时索引会被清理。后续媒体源路径或指纹恢复后，人物索引重建会重新匹配并将快照回迁到活动 metadata 目录；多候选或仍无候选时继续保留在隔离目录，不会强行关联。
- Emby `GET /Persons/{personIdOrName}` 已与人物列表 DTO 对齐，优先按人物 ID、未匹配时按精确姓名查询，并支持 `/emby` 前缀、`Fields`、共享 API Key、用户媒体库 ACL 和人物图片标签；为兼容 MDC，`GET/HEAD /Items/{personId}` 也返回相同人物 DTO，`POST /Items/{personId}` 接收 MDC 提交的演员元数据并更新可访问媒体库中的人物关系后返回相同 DTO，`POST /Items/{personId}/Images/Primary` 接收 MDC 的 Base64 演员头像（也兼容 Emby 原始图片二进制，并按解码后的实际 JPEG/PNG/WebP 签名识别格式，即使 Content-Type 声明不准确）并返回 `204 No Content`，已鉴权的无 tag `Primary` 人物图片请求同样受支持。Lux Web 提供受 session/API Key 保护的 `GET /api/v1/people/{personId}` 人物详情，供人物详情页读取。人物详情不因缺少简介或头像而返回空响应；MDC 元数据和头像 POST 兼容已由 `tests/people_api.rs` 覆盖，尚未以目标第三方客户端真实演员刮削流程复测。
- Harbor 1.4.6 兼容修复：Emby 媒体库自身的 `/Users/{userId}/Items/{libraryId}` 详情现在返回 `CollectionFolder`，并复用媒体库启用状态和 ACL 校验；本机 Harbor 真实 UI 已验证可进入库并显示条目。
- 2026-08-10 Emby 目录兼容修复：`Items/Latest` 默认按 `GroupItems=true` 返回电影/剧集根条目，剧集与季度 DTO 补充 `ChildCount`/`RecursiveItemCount`；`ParentId` 现在支持媒体库、剧集和季度，并覆盖剧集单集查询。`tests/series_api.rs` 已加入协议回归覆盖；网易爆米花真实设备复测仍待完成。
- 2026-08-11 网易爆米花 2.15.3 DTO 兼容修复：已观察到客户端可登录并加载部分首页，但尚未进入播放会话。Emby 条目现补齐 `SortName`、`SeasonId`、`IndexNumber`、`PremiereDate` 和 `ProviderIds`，季/集层级的标准字段已有协议回归覆盖；完整首页、详情页和播放仍待重启服务后的真实设备复测，不据此宣称完全兼容。
- 2026-08-11 网易爆米花首页链路修复：补齐 Emby 用户虚拟根目录 `/Users/{userId}/Items/Root`、`/Items/Root?userId=...` 和 `CollectionFolder` 子项，修正媒体库范围 `Items/Latest` 将季/集误当成最新根条目的问题；协议回归已覆盖，真实设备首页复测仍待完成。
- 2026-08-14 网易爆米花 2.15.3 搜索兼容修复：客户端通过 Emby `/Users/{userId}/Items?SearchTerm=...` 搜索时，服务端此前忽略 `SearchTerm` 并返回未过滤目录，导致搜索页显示无关条目；现已接入标题、原始标题和别名搜索并执行 ACL。重启本机 ARM64 服务后，爆米花 macOS UI 搜索“鬼吹灯之南海归墟”返回匹配条目，不再返回未过滤目录。
- 2026-08-12 网易爆米花媒体库列表 DTO 对齐：针对其实际请求的 `SortBy=DateCreated,SortName`、`Fields=BasicSyncInfo,ChildCount,RunTimeTicks,CommunityRating,PremiereDate,ProductionYear,CanDownload`，列表响应现在省略未请求字段和空值，返回 `SupportsSync`、UTC ISO `PremiereDate`、无播放位置时不返回 `PlayedPercentage`，并批量返回 `Logo`、`Thumb`、`Banner`、`Disc` 与全部背景图标签；电影 `ParentId` 由扫描建立的物理目录条目提供。脱敏协议回归已覆盖，网易爆米花真实设备刷新复测仍待完成，不据此宣称完全兼容。
- 2026-08-14 Filmly 2.12.3 剧集详情集列表 DTO 对齐：针对真实请求 `Fields=BasicSyncInfo,Overview,ProviderIds,Path,Size,People,RuntimeTicks,Chapters,MediaSources,CanDownload`，剧集单集在没有独立海报时将本地 `-thumb` 映射为 `ImageTags.Primary`，补齐 `SupportsSync`、Overview、ProviderIds、SeasonName、ParentThumbItemId、Size/Container/Bitrate、People 空集合，并在 `MediaSources` 内返回 `MediaStreams`。`tests/series_api.rs` 已覆盖脱敏请求和关键字段；真实安卓设备复测仍待完成，不据此宣称完全兼容。
- 2026-08-14 Filmly Episode 媒体流 shape 进一步对齐参考 Emby：集列表在请求媒体源时保留 `PremiereDate`，媒体源补齐 `SupportsProbing`，媒体流补齐 `AttachmentSize`、`IsAnamorphic`、`Protocol` 和 `SupportsExternalStream`，并由剧集协议回归锁定；远端设备仍需重新部署后复测。
- 2026-08-14 Filmly 图片兼容修复：实测 Android/Filmly 图片加载不携带 Emby token，且部分集只有 `THUMB` 图片却在 DTO 中作为 `ImageTags.Primary` 使用，导致 `/Items/{id}/Images/Primary` 返回 401/404。Emby 图片端点现在在 Primary 无 Poster 时回退到 Thumb，覆盖带 tag、无 tag 和已认证请求；`tests/series_api.rs` 已加入三种路径回归，真实设备需部署后复测。
- STRM 兜底图片兼容修复：没有刮削器、刮削器无候选或候选没有主图时，启用 STRM 截图会生成同一文件并同时登记为 `POSTER` 与 `THUMB`，因此 Emby `ImageTags.Primary`、`Thumb` 和 Lux Web 海报入口都能读取；相关数据库迁移、无刮削器集成回归和共享文件删除保护已覆盖，目标第三方客户端需部署后复测。
- 2026-08-14 Filmly 详情状态请求补齐：`Shows/NextUp` 现在按请求的 `SeriesId` 过滤，并遵守 `EnableTotalRecordCount=false` 的分页 shape；季度列表请求 `Genres` 时补齐 `Genres`/`GenreItems` 空集合，避免客户端收到错误剧集状态或不完整季度 DTO。
- 2026-08-14 Filmly Android 详情页根因定位：部分媒体源的 `MediaStreams[].Language` 为 JSON `null` 时，爆米花详情页显示“尝试连接时发生错误”；该日期的临时方案曾仅将剧集分集接口对 Filmly User-Agent 的空语言规范化为 `"und"`。该方案已由 2026-08-28 的统一媒体流序列化修复取代。
- 2026-08-28 Filmly Android 详情页兼容修复：媒体探测结果中的空白/缺失流语言统一输出为 `"und"`，缺失/空白 `DisplayTitle` 按流类型输出 `Video`、`Audio`、`Subtitle` 或 `Unknown`；规则位于统一 Emby `MediaStream` 序列化入口，覆盖分集列表、详情和媒体源响应，不再依赖客户端 User-Agent 后处理。`tests/series_api.rs` 已覆盖空标题的 Filmly 与 VidHub 分集响应。
- 2026-08-11 Filmly 2.12.3 首页请求修复：`/Users/{userId}/Items` 现在支持 `ExcludeItemTypes`，未指定递归和类型时按 Emby 根层级返回电影/剧集，列表 DTO 补充用户 `CanDownload` 和请求的 `Chapters` 字段；已用真实请求参数加入剧集层级协议回归，真实设备刷新复测仍待完成。
- 播放兼容修复：本地源的 Emby `Container` 使用真实文件扩展名，播放 URL 由 `MediaSourceId` 定位文件并兼容复合容器旧后缀；`attached_pic` 不再暴露为视频轨。自动化播放/探测回归已覆盖 MKV 和 MP4 路径，VidHub 已实测本地 MKV 直放。
- 播放会话失活保护：若第三方客户端异常退出、网络中断或未发送 `Stopped`，`PLAYING`/`PAUSED` 会话在连续 90 秒没有事件后从 Emby `GET /Sessions`、管理员控制台和 Web 播放状态中隐藏；显式 `Stopped` 仍立即清理活动会话。
- LUX-091 下载回归已覆盖 Lux/Emby 的 GET/HEAD 单资源响应、Range/文件名响应，以及 `.strm` 远程资源流式转发；尚未完成第三方客户端的真实下载 UI 实测，因此不据此宣称 Infuse、VidHub 或 SenPlayer 下载兼容。
- LUX-160 通用 `.strm` 解析回归已覆盖路径目标的 `PlaybackInfo` Lux 入口、受监督解析器 RPC 和 307 转发；尚未完成目标客户端现场配置与播放实测，不据此宣称客户端兼容。
- LUX-161 允许本地绝对路径型 `.strm` 直接指向媒体库根目录之外的可读普通文件，无需额外配置；专项回归覆盖 Web/Emby 视频入口的 Range 响应和原始 `.strm` 内容保持不变。本机 ARM 架构验证为 `arm64`；部署后仍需用实际 Web、Emby 和第三方播放器复测，不能仅凭服务端测试宣称客户端兼容。
- LUX-151 IP 归属地只扩展 Lux 管理员 Web 仪表盘的 `nowPlaying` 数据，不改变 VidHub、SenPlayer 或 Infuse 的 Emby 兼容接口；Hiofd 出站可用性和归属地准确性尚未做目标 NAS 现场验证。
- LUX-165 图片资源布局回归：Rust 集成测试已验证新下载图片写入 `/config/metadata/library/<shard>/<item-id>/`，Lux/Emby 图片端点可读取该路径，媒体目录本地图片仍可读取，且删除仅允许两类受保护根目录；这属于服务端协议回归，不替代 VidHub、SenPlayer 或 Infuse 的真实客户端复测。
- LUX-202 元数据资源布局回归：新生成或回写的电影、剧集、季、集 NFO 和 Lux 管理图片默认写入媒体目录旁车；策略开启时同一份 NFO/图片额外原子镜像到 `/config/metadata/library/<shard>/<item-id>/`，图片数据库路径仍指向媒体目录，删除图片会清理镜像。全局和媒体库覆盖的策略 API 与 Web 管理开关已有回归测试；这属于服务端存储行为，不改变主刮削器、备用刮削器或补充刮削器的选择逻辑。
- 本地 NFO 派生缓存回归：详情接口只读取数据库快照；快照损坏或过大时会清理该派生行并继续返回基础条目，演员关系或人物头像损坏时保留演员文字信息并由 Web 使用人物占位图标。该行为已由 `tests/catalog.rs`、`tests/metadata.rs` 和人物单元测试覆盖，尚未替代第三方客户端现场复测。
- LUX-166 元数据对象路径回归：Rust 路径契约测试已验证 `collections`、`genres`、`studios`、`tags` 的展示名桶、provider/object ID 身份和越界拒绝；本任务不改变客户端 API 行为。
- LUX-167 元数据对象快照回归：合集刷新协议测试已验证数据库关系更新后生成 `collection.json`，快照写入失败映射为可重试的服务错误；genres、studios、tags 尚无在线对象数据源，因此仅验证共用存储能力。
- 2026-08-28 VidHub 长时间播放恢复兼容：Emby 直放入口的短期 HMAC 票据有效期延长至 12 小时，覆盖 VidHub 暂停、切后台或网络恢复后复用播放地址的场景；该期限只保护重新进入 Lux 播放入口的请求，不限制已建立的上游直连播放时长。`src/application/playback/session.rs` 增加了有效期回归测试，远端实例需重新部署后复测。

## LUX-025 本机探针进度（2026-08-02）

| 客户端 | 本机发现 | 已观察到的流程 | 当前结果 |
|---|---|---|---|
| VidHub 2.1.8 | 已安装并运行 | 已完成 Emby 添加服务器、登录并进入 Lux 空媒体库 | 添加服务器/登录通过；旧探针发生在 `Views/Resume` 实现前，当前服务端已有对应路径和自动化测试 |
| SenPlayer 6.0.6 | 已安装 | 修复后已完成服务器加载、认证、`Users/:userId`、Views、Resume 和 Items 请求；电影页已显示 16 个条目（服务端总数 22） | P0 连接/登录和电影列表浏览通过；详情、播放、进度、收藏、字幕和多版本尚未实测 |
| Infuse | 未发现已安装应用 | 无法开始本机 UI 探针 | 未测试，需安装后再测 |

本次 VidHub 探针使用临时本机 ARM 服务 `127.0.0.1:18099`，未记录密码、token、Cookie、用户 ID 或真实媒体数据。

## VidHub 最新 ARM64 实测（2026-08-03）

VidHub 2.1.8（macOS arm64）连接本机独立 ARM64 实例 `http://127.0.0.1:18612`，服务端镜像为 `lux:arm64-local`（revision `83b5977`），使用临时媒体库和有效 MP4 夹具。真实 UI 流程如下：

| 流程 | 结果 | 证据 |
|---|---|---|
| 添加服务器并登录 | 通过 | VidHub 显示 `Lux ARM64 Full Smoke Emby - http://127.0.0.1:18612` 并进入库首页 |
| 媒体库浏览 | 通过 | 显示 `VidHub Smoke Movies` 和 `VidHub Valid 2024` |
| 条目详情 | 通过 | 详情页显示标题、年份和播放入口 |
| 本地 MP4 直放 | 通过 | VidHub 播放器进入 `VidHub Valid` 播放页面；初始 10 字节伪 MKV 的失败提示属于无效测试夹具，换成有效 MP4 后播放成功 |
| 收藏/已观看 | 通过 | UI 开关操作后，Lux API 返回 `isFavorite=true`、`isPlayed=true`、`playCount=1` |
| 播放位置上报 | 未观察 | 30 秒 MP4 播放并退出后，服务端 `positionTicks` 仍为 0；不把服务端接口测试当作真实客户端进度证据 |

本次测试没有记录密码、token、Cookie 或真实媒体数据。字幕、多版本和 Infuse 仍未完成真实客户端实测。

VidHub 2.1.8 登录后请求序列（动态用户 ID 已脱敏；这是服务端实现 `Views/Resume` 前的历史探针）：

| 方法 | 路径 | 状态 | 结果 |
|---|---|---:|---|
| GET | `/emby/Users/:userId/Views` | 404 | 未实现的媒体库视图路径 |
| GET | `/emby/Users/:userId/Items/Resume` | 404 | 未实现的继续观看路径 |

这组 404 只代表当时运行的服务端版本，不代表当前源码状态。当前源码已提供这两条路径；`tests/acl.rs` 覆盖 `Views`，`tests/resume_favorites.rs` 覆盖 `Items/Resume`。上述最新 ARM64 实测已补充真实客户端浏览、详情、播放和用户状态证据。

## VidHub 播放进度回传实测（2026-08-05）

VidHub 2.1.8（macOS arm64）连接当前 Mac 地址 `http://192.168.50.108:8097`，使用包含回调字段兼容和 `PlaySessionId` 响应修复的工作树构建。此前保存的 `192.168.50.113:8097` 已失效，切换地址后客户端重新加载媒体库。

本次明确选择了二毛条目的本地 4K 标记 MKV 媒体源；Lux 收到的直放路径为脱敏后的 `/emby/Videos/:itemId/:mediaSourceId/stream.mkv`，没有请求 `.strm` 外部地址。客户端实际播放画面后，服务端结构化日志和 SQLite 均观察到：

| 流程 | 请求 | 状态/结果 |
|---|---|---|
| 建立播放 | `POST /emby/Sessions/Playing` | `204`，位置 0 |
| 播放进度 | 多次 `POST /emby/Sessions/Playing/Progress` | 均 `204`，位置从 `126000000` 增长至 `861670000` ticks |
| 停止播放 | `POST /emby/Sessions/Playing/Stopped` | `204`，最终状态 `STOPPED` |
| 客户端读回 | `GET /emby/Users/:userId/Items/Resume` | `200`；VidHub 详情页显示“继续播放” |

最终数据库记录绑定到该本地 MKV 的 `media_source_id`，`user_item_state.position_ticks=861670000`；播放会话的 `state=STOPPED`。该实测证明 VidHub 播放、退出停止和继续观看进度回传链路已打通。文件名中的 `2160p` 只属于媒体源标签，本机 ffprobe 对该夹具实际识别为 1920x1080 H.264，属于现有测试媒体内容差异。

SenPlayer 6.0.6 的历史实测结果：服务器已添加，但客户端重复请求 `POST /emby/Users/AuthenticateByName`，服务端均返回 `200`；客户端随后显示“未能读取数据，数据已丢失”，没有继续请求 `System/Info`。2026-08-06 真实 UI 重试捕获到认证后的 `GET /emby/Users/:userId`；该路由此前缺失，请求落入 Web 前端 fallback 并返回 HTML 200，正是客户端 JSON 解析失败的直接原因。补齐路由后，列表接口按请求的 `Fields` 省略未请求的 `MediaSources/MediaStreams`，并将服务监听到 SenPlayer 实际使用的 `192.168.50.108:8097`；真实 UI 已进入“我的媒体”，电影页显示 16 个条目，服务端总数为 22。

2026-08-07 SenPlayer 6.0.6 播放复测：客户端请求的脱敏路径为 `/emby/videos/:itemId/stream.mkv%3F...`，Lux 返回 `307` 并将 `.strm` 的外部地址放入 `Location`，不代理媒体字节；SenPlayer 播放器显示真实画面并以约 2.3 MB/s 读取，SQLite 播放会话记录为 `PLAYING`。未记录 token、Cookie、真实 `.strm` URL 或用户数据。

### 可重复的本地协议探针

`tools/compatibility-probe/probe.py` 可对本机 Lux 运行一次脱敏协议序列：

1. `System/Info/Public`
2. `Users/Public`
3. `Users/AuthenticateByName`
4. 带 token 的 `System/Info`、`System/Ping`
5. `Sessions/Logout`
6. logout 后再次访问 `System/Info`，应为 `401`

密码通过 `LUX_PROBE_PASSWORD` 注入，token 只在进程内使用；输出只包含路径、状态码和响应字段摘要。该工具用于协议回归，不等同于 VidHub、SenPlayer 或 Infuse 的真实客户端兼容性结论。
