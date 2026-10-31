# ADR-049：JSON 响应的 ETag、重新验证与压缩语义

## 状态

已接受（2026-10-28）。落实 `docs/LUX-DATA-PATH-PLAN.md` 的 W5「ETag/304 + gzip」。

## 日期

2026-10-28

## 背景

首页 JSON 92,674 B、搜索 24 条 99,614 B、库列表 50 条 99,464 B。客户端每次进入页面都要传输完整
响应，服务端也没有让客户端复用已获取内容的机制。方案同时需要避免把媒体流、图片与 SSE 拉进压缩
或缓冲路径。

## 决策

1. **压缩只作用于 JSON**：`tower-http` 的 `CompressionLayer` 配自定义谓词，只压缩成功的
   `application/json` 响应；媒体流、图片、SPA 回退与 `text/event-stream` 保持原样，Range 请求不受影响。
   依赖面：`tower-http` 增加 `compression-gzip`（引入纯 Rust 的
   `async-compression`/`compression-codecs`/`compression-core`），`http-body` 改为直接依赖以实现谓词。
   **该依赖变更属于计划 §11 需要所有者批准的事项，已记录在案。**
2. **强 ETag + 私有缓存**：中间件对“长度已知且 ≤4 MiB”的 JSON 响应计算未压缩 body 的 SHA-256 前 16 字节
   作为强 ETag，并下发 `Cache-Control: private, no-cache`；`If-None-Match` 命中返回 304 且无 body。
3. **不改写既有策略**：处理器自己声明的 `Cache-Control` 保留；`no-store` 响应既不打 ETag 也不被改成
   `private, no-cache`（管理端诊断/性能端点依赖这一点）。
4. **层序**：压缩层在 ETag 中间件之外，304 不经过压缩；ETag 只对 `GET`/`HEAD` 生效。
5. **SSE 与非 JSON 永不缓冲**：中间件在进入缓冲前用 body 的 `size_hint()` 判断长度；未知长度直接放行。

## 后果

- 66k 夹具实测：首页 92,674 → **8,308 B**（9.0%）、搜索 99,614 → 7,002 B（7.0%）、库列表 50 条
  99,464 → 6,993 B（7.0%）、继续观看 40,324 → 4,982 B（12.4%）。压缩成本约 0.12 ms/60 KB
  （zlib level 6），相对每次省下的 84 KB 可忽略。
- 二次访问命中 304 时 body 为空，省掉整个响应。
- ETag 计算要读一遍 body（SHA-256 90 KB 约 0.1–0.2 ms）；超过 4 MiB 或长度未知的响应放弃该能力。
- 客户端契约测试（`tests/response_compression.rs`）固定了“请求 gzip 才压缩、非 JSON 不动、
  `no-store` 保留、304 空 body”的行为，避免未来改动静默改变协议语义。
