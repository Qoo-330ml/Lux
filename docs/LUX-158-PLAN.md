# LUX-158：路径型 `.strm` 目标分类

## 目标

将 `.strm` 内容从“必然是 URL”扩展为有限的支持目标，并建立稳定的词法分类基础。解析只负责识别格式；真正的本地路径访问、SMB/FTP 解析和媒体读取由后续播放/探测流程完成。

## 行为契约

- 读取首个非空行，去除 BOM 与首尾空白，保留目标正文。
- `http://` 和 `https://`（大小写不敏感）分类为 `URL`。
- 以 `/` 或 `//` 开头、Windows UNC/盘符形式以及不含 URI scheme 的相对路径分类为 `PATH`。
- `smb://` 分类为 SMB，`ftp://` 分类为 FTP；其他带 scheme 或无法判断的内容分类为不支持。
- 分类是纯函数，不解析 URL、不解析本地路径、不访问网络、不执行文件系统操作。
- 空文件返回 `EMPTY`，不能生成可播放地址。

## 范围

- 新增 `StrmTargetKind` 和目标分类函数。
- 保持现有 `STRM_URL` 数据库存储和 URL 型播放行为不变。
- 为本地路径、SMB、FTP 和不支持目标提供后续播放策略需要的内部类型基础。

## 明确不做

- 不新增数据库字段或 migration。
- 不在分类函数中调用外部解析器、不请求路径、不代理媒体字节。
- 不把任何具体第三方工具写入 Lux 核心。

## 验证

```bash
cargo test --locked --test strm
cargo test --locked --lib strm_target
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
```

## 预计文件

- `docs/LUX-DEVELOPMENT.md`
- `docs/LUX-072-PLAN.md`
- `docs/LUX-158-PLAN.md`
- `src/application/strm_target.rs`
- `src/application/mod.rs`
- `tests/strm_target.rs`
