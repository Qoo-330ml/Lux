# LUX-160：通用 `.strm` 目标解析与转发

## 目标

让 SMB/FTP `.strm` 目标可以通过 Lux Plugin SDK 的协议解析器得到可播放地址。Lux 只负责传递
原始目标、校验插件结果并在播放请求中转发，不认识任何具体存储服务、路径映射规则或第三方工具。

## 行为契约

- 插件 manifest 使用 `type: "strm_resolver"`、`category: "MEDIA"` 和
  `capabilities: ["strm.resolve"]`。
- Lux 通过 `strm.resolve` RPC 发送 `{ "target": "<原始目标>" }`。
- 插件返回 `RESOLVED` 加 HTTP(S) `url`，或返回没有 URL 的 `UNSUPPORTED`；其他结果均视为无效。
- Lux 只把 SMB/FTP 目标交给解析器；本地路径由 Lux 的安全本地文件读取流程处理，HTTP(S) 目标继续使用原来的直连行为，其他协议不会交给该解析器。
- 可同时安装多个解析器。Lux 按插件 ID 稳定顺序尝试已安装、启用且配置有效的解析器，首个
  `RESOLVED` 结果生效；插件自行决定是否支持目标。
- 插件结果必须是 HTTP(S) URL，长度、凭据、fragment 和控制字符校验失败时不得下发给客户端。
- 解析失败或没有解析器时不伪造地址；播放端点返回稳定的“不支持”结果。
- Lux 不代理媒体字节，不把原始目标写入日志，也不绑定具体外部工具。

## 增量

1. 在 Plugin SDK 中增加 `strm_resolver` manifest 类型和 `strm.resolve` 请求/结果合同。
2. 在应用服务中发现可用解析器，调用受监督进程并校验结果。
3. 在 Emby `PlaybackInfo` 和视频端点接入解析器转发。

## 验证

```bash
cargo test --locked --test plugin_protocol --test strm
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
```
