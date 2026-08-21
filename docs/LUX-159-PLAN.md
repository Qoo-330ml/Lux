# LUX-159：持久化 `.strm` 原始目标分类

## 目标

在不破坏现有 `STRM_URL`/`external_url` 兼容合同的前提下，让扫描器把 `.strm` 的首个非空内容作为原始目标保存，并持久化其词法分类。路径和其他目标在尚未配置处理策略时不能被伪造为 HTTP 直链，也不能被 Lux 当作本地媒体文件返回。

## 行为契约

- `strm_target_kind` 兼容字段取值为 `URL`、`PATH`、`OPAQUE` 或 `EMPTY`；SMB、FTP 和不支持协议保存为 `OPAQUE`，播放表面按原始目标重新区分。
- 新扫描的 `.strm` 使用 LUX-158 的纯函数分类；原始目标仍暂存于现有 `external_url` 字段，以保持已有读取合同。
- 旧数据库中分类字段为空时，读取播放表面按原始目标重新执行同一词法分类，不访问网络或文件系统。
- URL 型目标继续返回外部 URL、HTTP 协议和远程直播放行。
- 本地路径型目标生成受保护的 Lux 视频端点，并在请求时解析、校验和读取实际媒体文件；相对路径相对于 `.strm` 所在目录，绝对路径必须位于媒体库根目录内。
- SMB/FTP 目标不在普通请求中直接访问，交给 LUX-160 的解析器；没有解析器时不生成 `DirectStreamUrl`。空目标和其他协议不生成播放地址，也不会把 `.strm` 文件字节当作媒体返回。

## 范围

- 使用现有 SQLite/PostgreSQL `media_sources.strm_target_kind` 字段及其兼容迁移；本次不新增 schema 值，SMB/FTP 和不支持协议继续保存为 `OPAQUE`。
- 电影、剧集和未解析文件扫描在新增、重扫和内容变化时同步保存分类。
- Emby `MediaSource` 与视频端点使用分类结果保护 URL 直播放行。
- STRM 后台探测继续把原始目标交给受监督插件；只允许 HTTP/HTTPS、本地路径、SMB 和 FTP 进入探测，不在扫描或播放请求中解析目标。

## 明确不做

- 不实现路径映射、外部解析器注册、目标转发、媒体字节代理或转码。
- 不绑定任何具体云盘、网盘或第三方工具。
- 不修改现有 `STRM_URL`/`external_url` 对 URL 型 `.strm` 的数据库和客户端合同。

## 验证

```bash
cargo test --locked --test strm
cargo test --locked --test strm_target
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
```

## 预计文件

- `docs/LUX-DEVELOPMENT.md`
- `docs/LUX-159-PLAN.md`
- `src/application/scanner.rs`
- `src/application/strm_target.rs`
- `src/api/mod.rs`
- `src/application/downloads.rs`
- `src/application/strm_probe_policy.rs`
- `tests/strm.rs`
