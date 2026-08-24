# ADR-025：有序媒体库刮削器与角色化执行

## 状态

Accepted

## 背景

Lux 原先每个媒体库只保存一个 `scraperId`。实际部署中，不同刮削器的覆盖范围不同：一个来源适合确定媒体身份，另一个来源可能拥有额外演员、图片或制作信息。单纯的失败回退无法补充主来源已经返回但仍缺失的字段；无规则地合并多个来源又会覆盖本地数据或改变媒体身份。

## 决策

1. 以 `library_scrapers(library_id, scraper_id, position, role)` 保存媒体库刮削器列表；`role` 为 `PRIMARY`、`SUPPLEMENT`、`BACKUP` 或 `BOTH`。
2. position 0 必须是唯一的 `PRIMARY`。后续项按 position 排序，不能重复选择同一插件。
3. 身份匹配阶段只使用 `PRIMARY`、`BACKUP` 和 `BOTH`；首个有效高置信候选确认身份后停止备用搜索。
4. 补充阶段使用 `SUPPLEMENT` 和 `BOTH`，只写入缺失的未锁定字段和图片类型，不覆盖更高优先级来源。
5. 每个最终字段、图片和 provider ID 记录实际来源；旧 `libraries.scraper_id` 保留为首位兼容镜像，旧 API 继续可读写单个主刮削器。

## 选择理由

- 将“备用”和“补充”分开，避免主来源成功后仍无意义地调用纯备用来源。
- `BOTH` 允许一个来源同时承担两种职责，不需要在列表中重复配置。
- 角色化列表比单独维护备用列表和补充列表更容易理解，也能保留用户可见的排序。
- 领域层按字段合并，符合本地 NFO 优先和锁定字段不覆盖的既有规则。

## 后果

- 任务执行可能访问多个外部来源，因此 `FILL_MISSING` 必须先计算缺失字段/图片并复用缓存。
- provider ID 仍使用 provider namespace，不得将插件 ID 当作 provider ID。
- 候选和来源状态需要在任务日志中脱敏记录，不能输出外部 URL 或凭据。
- 未来如需对不同字段配置更细的来源优先级，应新增策略模型，不改变本 ADR 的角色语义。

## 兼容性

- 数据库迁移把历史单值 `scraper_id` 转换为 position 0 的 `PRIMARY`。
- Lux API 新增有序 `scrapers` 字段，同时返回旧 `scraperId` 首项字段。
- Emby 兼容接口不暴露角色化配置，也不改变其既有 provider ID 响应合同。
