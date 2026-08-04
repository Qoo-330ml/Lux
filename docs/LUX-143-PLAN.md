# LUX-143：本地元数据优先与全局刮削模式

## 目标

媒体库扫描始终读取本地 NFO 和图片；媒体库的刮削器只表示可选的在线补充来源。管理员从全局媒体策略发起批量刮削时，可以选择“仅补全”或“完整刮削”。

## 行为契约

- 未配置刮削器时仍正常读取本地 NFO、图片和技术信息，不发起在线刮削请求。
- “仅补全”只写入缺失的未锁定 NFO 字段和缺失图片。
- “完整刮削”刷新未锁定的 NFO 字段，并替换已有图片；在线没有返回的图片不删除已有本地图片。
- 已锁定的 NFO 字段在两种模式下都不覆盖。
- 全局刮削必须进入持久化后台任务队列，服务重启后继续处理。
- 低置信度或无法识别的条目保留本地内容并进入失败/待处理状态，不自动写入不确定候选。

## 接口

- 保留现有重新识别接口的“只搜索并生成候选”语义。
- 新增媒体库元数据刷新接口，接收 `{ "mode": "FILL_MISSING" | "FULL_REFRESH" }`，返回批量后台任务信息。
- 任务持久化模式，旧任务默认为 `REIDENTIFY`，不破坏已有任务恢复。

## 预计修改范围

- Rust：元数据任务应用服务、候选选择、任务存储与 migration、Lux 管理 API、相关集成测试。
- Web：全局策略模式选择、全局/媒体库刷新入口、API 类型与客户端、相关组件测试。
- 文档：`docs/API.md` 与 `docs/LUX-DEVELOPMENT.md` 的行为和验收说明。

## 验证

```bash
cargo test --locked --test reidentify --test metadata_selection --test libraries_api
pnpm --dir web test
pnpm --dir web build
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
```

## 边界

- 不新增依赖，不改变 Emby DTO，不在 HTTP 请求中执行扫描、NFO 解析、图片下载或在线请求。
- 图片替换仍使用现有的路径校验、大小限制和原子写回流程。
