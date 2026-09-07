# ADR-041：字幕扩展不修改原生媒体响应头

## 状态

已接受；部分取代 ADR-040 第 3 项。

## 日期

2026-09-07

## 背景

字幕扩展的 Service Worker 已经通过 `host_permissions` 自己发起远程 HTTP(S) Range 请求，读取
Matroska 的 Tracks、Cues 和字幕 Cluster。原生 `<video>` 仍使用 Lux 的签名 URL/307 直连路径，
不需要页面 JavaScript 读取媒体响应头。继续安装 `declarativeNetRequest` 规则去修改 CDN 的
`Access-Control-Allow-Origin` 会扩大扩展权限，并把字幕功能无关的响应头变更施加到原生音视频请求。

## 决定

移除扩展的 `declarativeNetRequestWithHostAccess` 权限、动态响应头规则及其生命周期管理。扩展只
在 Service Worker 内用主机权限读取字幕所需的 Range；它不拦截、改写或代理 `<video>` 的请求。

## 后果

- 原生音视频请求保持修改字幕前的请求和响应路径，降低无声或解码回归风险。
- 扩展权限更小，页面关闭时不需要清理动态规则。
- 旧 ADR-040 中“为页面媒体补齐 CORS 响应头”的描述不再适用；字幕读取仍要求有效的 `206 + Content-Range`。
- 这不会为 Chrome 增加 E-AC-3/AC-3 解码能力；不支持的音频仍需 AAC/Opus 文件、支持该编码的浏览器，
  或另行设计的本地/服务端转码方案。
