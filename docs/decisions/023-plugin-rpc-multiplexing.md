# ADR-023：插件 RPC 多路复用

## 状态

已接受

## 背景

Lux 的后台元数据任务按条目使用有界并发 worker。旧的插件宿主在一个长期运行的插件进程上同时持有
stdin/stdout Mutex，并在一次调用中完成写入和读取，因此同一插件的不同媒体条目最终只能串行执行。
这抵消了 metadata worker 的并行度，也使 TMDb 插件自身的连接池和请求并发配额无法发挥作用。

## 决策

插件进程采用 request ID 驱动的多路复用：

- 宿主为每个请求登记 pending channel，只对 stdin 写入做短时互斥。
- 独立 stdout reader 解析响应，并按 response ID 分发到对应请求；响应可以乱序返回。
- 单个插件进程的 pending 请求数有界，默认上限为 16；等待并发许可计入调用超时。
- 插件进程退出、协议错误或超时时，所有 pending 请求都结束，并清理该进程。
- 保留现有 JSON-RPC 方法名和 request/response ID 字段，旧版串行插件仍可工作。
- TMDb 插件最多并发处理 16 个 RPC task，stdout 输出统一串行化，避免 JSON 行交错。
- TMDb 的单请求 bundle 语义保持不变；本 ADR 不改变候选、NFO 或图片写回语义。

## 未采用的方案

插件进程池实现简单，但会复制 TMDb client、HTTP 连接池、缓存和限流器，可能造成额外内存占用和多份
限流窗口。除非某类插件无法支持多路复用，否则不采用进程池作为默认方案。

## 故障语义

插件返回结构化 RPC error 时，只结束当前请求并保留进程。I/O、协议错误、超时或进程退出会结束所有
pending 请求并淘汰进程，下一次调用重新启动插件。

## 验证

- 宿主测试覆盖两个并发请求和乱序响应的 request ID 匹配。
- TMDb 插件测试覆盖 singleflight waiter 唤醒和 owner 取消清理。
- 完整验证仍需分别在 Lux 与 Lux-plugins 仓库执行 Rust 检查；本机只记录 ARM64 结果。
