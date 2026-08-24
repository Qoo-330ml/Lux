# LUX-191+：Emby → Lux 迁移实现

## 已实现范围

- 独立进程插件 `org.lux.emby-migration`，只允许 Emby → Lux。
- Emby API Key 使用受保护 secret 文件保存，不写入日志和 API 响应。
- 后台任务支持 dry-run、分页、进度、取消、恢复和幂等导入。
- 自动创建 Lux 用户；首次登录时向 Emby 验证原密码，再在 Lux 本地生成 Argon2 哈希。
- 用户资料、禁用状态、已看、播放位置、播放次数、最近播放时间和收藏迁移。
- Provider ID、剧集/季/集键和标题/年份匹配；冲突或找不到的媒体跳过并报告。
- 迁移分页遇到插件无效响应时，宿主自动拆分分页定位异常条目；单条仍无法解析时计入失败数并继续后续迁移，避免整项任务中止。
- 默认合并，也支持覆盖和跳过。
- `ITEM_STATE` 与 `EVENT_HISTORY` 能力分离；当前插件只声明 `ITEM_STATE`。

## 验收

- Rust 宿主和插件检查通过。
- 空数据库可运行全部迁移至当前 schema version。
- API Key、密码、token 和完整外部 URL 不进入日志或响应敏感字段。
- 没有真实 Emby 实例时，不宣称真实版本兼容或完整事件时间线可用。
- 真实事件能力只有通过受控实例和脱敏 fixture 后才能增加 `EVENT_HISTORY`。
