# Lux 客户端兼容性矩阵

本文档是目标客户端兼容性的唯一事实来源。未填入实测版本和证据前，不得宣称兼容。

## 目标矩阵

| 客户端 | 版本 | 平台/设备 | 添加服务器 | 登录 | 浏览/详情 | 播放 | 进度/收藏 | 字幕/多版本 | 证据/备注 |
|---|---|---|---|---|---|---|---|---|---|
| Infuse | 未测试 | 未测试 | 未测试 | 未测试 | 未测试 | 未测试 | 未测试 | 未测试 | 待 LUX-025 |
| VidHub | 2.1.8 | macOS arm64 | 通过 | 通过 | 媒体库浏览、条目详情通过 | 通过 | 部分通过 | 未测试 | 2026-08-03 本机 ARM64 独立实例真实 UI 流程通过；有效 MP4 播放、收藏/已观看状态通过，实际播放位置未观察到 |
| SenPlayer | 6.0.6 | macOS arm64 | 通过 | 已添加，客户端读取失败 | 未测试 | 未测试 | 未测试 | 未测试 | 认证 HTTP 200 后客户端报“未能读取数据，数据已丢失”；项目所有者接受该边界 |
| Lux Web | Chrome 150 smoke | macOS arm64 | 通过 | 通过 | 基础浏览/详情/筛选/账户会话通过 | MP4 直放通过 | 进度/收藏接口与收藏浏览器 smoke 通过 | 多版本代码已实现、字幕路径已有服务端测试 | Chrome headless：普通用户无管理入口、stream 206、readyState=4、390/768/1440 viewport 无横向溢出、控制台无错误；`scripts/browser-smoke.mjs` 和 `scripts/admin-smoke.mjs` 已固化 |

## 记录格式

每次探针或回归测试至少记录：客户端版本、平台版本、Lux 提交、请求路径序列、脱敏请求参数、状态码、关键响应字段、结果和已知差异。密码、token、Cookie、真实 `.strm` URL 和用户数据不得进入 fixture 或文档。

## 当前状态

- LUX-000 至 LUX-003：仅完成仓库工程检查，尚未连接任何真实客户端。
- LUX-023：已完成根路径/`/emby` 前缀的 System/Ping 本地协议 shape 测试，并完成 VidHub/SenPlayer 真实登录前置探针。
- LUX-024：已完成 Users/Public、AuthenticateByName、Sessions/Logout 的本地协议 shape 和 token 脱敏测试；VidHub 真实登录通过，SenPlayer 认证响应解析失败。
- `cargo` 验证是在本机 `arm64` 上完成，不代表目标 x86_64 飞牛 NAS 性能或客户端兼容性。
- Web 的“已实现”仅表示代码路径和服务端静态集成已完成；当前 Chrome smoke 覆盖登录、筛选、播放、收藏、账户会话和管理流程，不等同于所有浏览器/编码格式兼容。
- LUX-121 兼容补齐：Emby `Views` 返回媒体库类型、`ChildCount` 和标准 `ImageTags.Primary`；条目详情同时返回本地徽标的 `ImageTags.Logo`，并通过 `/Items/{itemId}/Images/Logo` 提供标准图片读取；媒体库封面支持 `/Items/{libraryId}/Images/Primary` 及带索引、HEAD、ETag 和 ACL。尚待 VidHub UI 重新实测确认。
- 播放兼容修复：本地源的 Emby `Container` 使用真实文件扩展名，播放 URL 由 `MediaSourceId` 定位文件并兼容复合容器旧后缀；`attached_pic` 不再暴露为视频轨。自动化播放/探测回归已覆盖 MKV 和 MP4 路径，尚待 VidHub 重新实测确认。
- LUX-091 下载回归已覆盖 Lux/Emby 的 GET/HEAD 单资源响应、Range/文件名响应，以及 `.strm` 远程资源流式转发；尚未完成第三方客户端的真实下载 UI 实测，因此不据此宣称 Infuse、VidHub 或 SenPlayer 下载兼容。

## LUX-025 本机探针进度（2026-08-02）

| 客户端 | 本机发现 | 已观察到的流程 | 当前结果 |
|---|---|---|---|
| VidHub 2.1.8 | 已安装并运行 | 已完成 Emby 添加服务器、登录并进入 Lux 空媒体库 | 添加服务器/登录通过；旧探针发生在 `Views/Resume` 实现前，当前服务端已有对应路径和自动化测试 |
| SenPlayer 6.0.6 | 已安装 | 已添加 Emby 服务器并触发登录；客户端未进入 System/Info | HTTP 200 后客户端提示数据读取失败 |
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

SenPlayer 6.0.6 的实际结果：服务器已添加，但客户端重复请求 `POST /emby/Users/AuthenticateByName`，服务端均返回 `200`；客户端随后显示“未能读取数据，数据已丢失”，没有继续请求 `System/Info`。项目所有者已接受暂不为该客户端补齐未实现的后续媒体接口，作为已知阻塞记录。

### 可重复的本地协议探针

`tools/compatibility-probe/probe.py` 可对本机 Lux 运行一次脱敏协议序列：

1. `System/Info/Public`
2. `Users/Public`
3. `Users/AuthenticateByName`
4. 带 token 的 `System/Info`、`System/Ping`
5. `Sessions/Logout`
6. logout 后再次访问 `System/Info`，应为 `401`

密码通过 `LUX_PROBE_PASSWORD` 注入，token 只在进程内使用；输出只包含路径、状态码和响应字段摘要。该工具用于协议回归，不等同于 VidHub、SenPlayer 或 Infuse 的真实客户端兼容性结论。
