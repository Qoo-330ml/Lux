# ADR-006：React Web 客户端

- 状态：待项目所有者确认
- 日期：2026-08-02

## 决定

核心服务端使用 Rust；首版 Web 客户端建议使用 React + TypeScript、Vite、服务端状态管理库、React Router 和原生 HTML `video` 元素。

## 原因

Web UI 不在媒体索引和传输热路径上。React/TypeScript 生态更适合管理后台、响应式界面、可访问性和浏览器视频能力；不影响 Rust 服务端性能目标。

## 后果

Web 阶段开始前由项目所有者确认本 ADR。若改为全 Rust Web，必须先新增 ADR 评估 Leptos/Yew，并调整 Web 工程、测试和发布计划；不能在实现中途无记录切换。
