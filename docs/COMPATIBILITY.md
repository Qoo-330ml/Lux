# Lux 客户端兼容性矩阵

本文档是目标客户端兼容性的唯一事实来源。未填入实测版本和证据前，不得宣称兼容。

## 目标矩阵

| 客户端 | 版本 | 平台/设备 | 添加服务器 | 登录 | 浏览/详情 | 播放 | 进度/收藏 | 字幕/多版本 | 证据/备注 |
|---|---|---|---|---|---|---|---|---|---|
| Infuse | 未测试 | 未测试 | 未测试 | 未测试 | 未测试 | 未测试 | 未测试 | 未测试 | 待 LUX-025 |
| VidHub | 2.1.8 | macOS arm64 | 通过 | 通过 | 未实现（Views/Resume 返回 404） | 未测试 | 未测试 | 未测试 | 真实 UI 添加/登录成功；见下方请求序列 |
| SenPlayer | 6.0.6 | macOS arm64 | 通过 | 未通过（响应解析） | 未测试 | 未测试 | 未测试 | 未测试 | 服务器已保存；认证 HTTP 200 后客户端报“未能读取数据，数据已丢失” |
| Lux Web | Chrome 150 smoke | macOS arm64 | 通过 | 通过 | 基础浏览/详情/筛选通过 | MP4 直放通过 | 浏览器端进度/收藏未完整回归 | 多版本代码已实现、浏览器未覆盖 | Chrome headless：普通用户无管理入口、stream 206、readyState=4、390px 无横向溢出、控制台无错误；完整 Playwright 套件仍未固化 |

## 记录格式

每次探针或回归测试至少记录：客户端版本、平台版本、Lux 提交、请求路径序列、脱敏请求参数、状态码、关键响应字段、结果和已知差异。密码、token、Cookie、真实 `.strm` URL 和用户数据不得进入 fixture 或文档。

## 当前状态

- LUX-000 至 LUX-003：仅完成仓库工程检查，尚未连接任何真实客户端。
- LUX-023：已完成根路径/`/emby` 前缀的 System/Ping 本地协议 shape 测试，并完成 VidHub/SenPlayer 真实登录前置探针。
- LUX-024：已完成 Users/Public、AuthenticateByName、Sessions/Logout 的本地协议 shape 和 token 脱敏测试；VidHub 真实登录通过，SenPlayer 认证响应解析失败。
- `cargo` 验证是在本机 `arm64` 上完成，不代表目标 x86_64 飞牛 NAS 性能或客户端兼容性。
- Web 的“已实现”仅表示代码路径和服务端静态集成已完成；在真实浏览器中完成登录、筛选和播放前，不宣称浏览器端到端兼容。

## LUX-025 本机探针进度（2026-08-02）

| 客户端 | 本机发现 | 已观察到的流程 | 当前结果 |
|---|---|---|---|
| VidHub 2.1.8 | 已安装并运行 | 已完成 Emby 添加服务器、登录并进入 Lux 空媒体库 | 添加服务器/登录通过；媒体库路径尚未实现 |
| SenPlayer 6.0.6 | 已安装 | 已添加 Emby 服务器并触发登录；客户端未进入 System/Info | HTTP 200 后客户端提示数据读取失败 |
| Infuse | 未发现已安装应用 | 无法开始本机 UI 探针 | 未测试，需安装后再测 |

本次 VidHub 探针使用临时本机 ARM 服务 `127.0.0.1:18099`，未记录密码、token、Cookie、用户 ID 或真实媒体数据。

VidHub 2.1.8 登录后请求序列（动态用户 ID 已脱敏）：

| 方法 | 路径 | 状态 | 结果 |
|---|---|---:|---|
| GET | `/emby/Users/:userId/Views` | 404 | 未实现的媒体库视图路径 |
| GET | `/emby/Users/:userId/Items/Resume` | 404 | 未实现的继续观看路径 |

这些 404 是当前规格允许记录的未实现路径，不影响 LUX-025 的“添加服务器并完成登录”验收；LUX-030 以后才实现媒体库浏览。

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
