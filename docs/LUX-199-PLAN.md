# LUX-199：Emby 媒体源代理兼容

## 目标

让使用标准 Emby API 的第三方反向代理能够从 Lux 的播放请求和媒体源详情中稳定取得
`ItemId`、`MediaSourceId` 以及 `.strm` 的原始 `Path`，由第三方代理自行完成路径映射和
云盘直链转换。Lux 不实现第三方代理的云盘逻辑。

## 实施切片

1. **播放 URL 合同**：将新生成的 `DirectStreamUrl` 对齐为
   `/Videos/{ItemId}/stream[.Container]?MediaSourceId={MediaSourceId}`，保留旧的媒体源路径入口。
2. **媒体源详情回退**：`GET /Items/{MediaSourceId}` 和 `/emby/Items/{MediaSourceId}` 在权限范围内解析到
   所属条目；未知或不可见媒体源不返回其他内容。
3. **回归验证**：覆盖标准查询 URL、历史路径 URL、编码到路径中的查询参数、媒体源 ID 详情和路径型 `.strm`
   的 `Protocol`/`IsRemote` 语义；更新兼容性记录并运行 Rust 基线检查。

## 明确边界

- Lux 不访问 `.strm` 路径背后的 115、CloudDrive 或其他远端网盘。
- Lux 不执行 filepath mapping，不缓存直链，不代理媒体字节。
- 路径型 `.strm` 继续输出 `Protocol=File`、`IsRemote=false`；是否由 Redia 接管由 Redia 的代理规则决定。

## 验证记录

实现完成后补充专项测试、`cargo build --locked`、`cargo test --locked --all-targets`、
`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`
以及 `uname -m` 结果。真实第三方 Redia/VidHub 播放验证需要用户远端实例和脱敏请求日志，
本地代码测试不宣称已经完成该外部联调。
