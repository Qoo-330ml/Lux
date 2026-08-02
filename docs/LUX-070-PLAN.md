# LUX-070 Range 文件服务

## 范围

为本地媒体源提供受鉴权的流式 GET/HEAD。媒体路径只能通过数据库中的条目和 source ID 解析，服务端不会接受客户端提交的任意磁盘路径。

## 实现

- [x] 支持完整 GET/HEAD 和单 `bytes` Range，返回 200、206、416。
- [x] 返回 `Accept-Ranges`、`Content-Length`、`Content-Range`、`Content-Type`、`ETag`、`Last-Modified`。
- [x] 使用 Tokio 文件流和固定范围读取，不把大文件读入内存；客户端断开时流被丢弃并停止读取。
- [x] 支持默认 source、显式 source ID、容器后缀和 `X-Emby-Token`/`api_key`。
- [x] 读取前执行条目 ACL、根目录 containment 和符号链接解析，拒绝越界路径。

## 验证

- Range 单元测试覆盖完整、闭区间、开放结尾、后缀、截断、多 Range、非法和越界场景。
- ARM64 集成测试覆盖 GET/HEAD、source 路由、单 Range、416、ACL、query token 和路径逃逸。

## 明确不做

- 首版不实现多 Range `multipart/byteranges`。
- `.strm` 外部 URL 直交和 PlaybackInfo 版本选择属于 LUX-071/LUX-072。
