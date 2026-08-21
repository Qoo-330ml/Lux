# LUX-190：Emby 迁移插件边界与可行性验证

## 范围

冻结 `org.lux.emby-migration` 的单向迁移边界，确认公开 Emby API 可提供的用户、媒体 UserData、
用户权限和历史播放事件字段。只做规格、协议草案、fixture 和验证记录，不实现数据库、运行时、
后台任务、Web 页面或插件包。

## 验收标准

- [ ] 规格明确只允许 Emby → Lux，不定义反向迁移合同。
- [ ] 规格明确 API key、用户密码和完整外部 URL 的存储、传输、日志规则。
- [ ] 保存至少一组脱敏的 Emby 用户、电影、剧集、分集和 UserData 响应 fixture。
- [ ] 用受控 Emby 实例验证并记录：用户资料、禁用状态、媒体库权限、已看、播放位置、播放次数、
      最近播放时间和收藏的字段来源。
- [ ] 明确记录当前测试实例是否提供原始播放事件；不能提供时记录为 `ITEM_STATE`，不伪造事件。
- [ ] 定义 `ITEM_STATE` 与 `EVENT_HISTORY` 两级插件能力，以及源端不支持历史事件时的结果语义。
- [ ] ADR-022 与 `COMPATIBILITY.md` 记录协议边界和验证结果。

## 明确不做

- 不读取 Emby 数据库、日志文件或未公开内部表。
- 不实现 Emby 密码哈希导入；密码迁移只保留首次登录验证方案。
- 不在本任务新增 migration、Rust 代码、Web 代码或插件包。

## 预计文件

- `docs/LUX-DEVELOPMENT.md`
- `docs/LUX-190-PLAN.md`
- `docs/decisions/022-emby-migration-plugin.md`
- `docs/COMPATIBILITY.md`

## 验证

- `git diff --check`
- 检查 fixture 不含密码、token、cookie、真实 URL 或用户数据。
- `uname -m`
