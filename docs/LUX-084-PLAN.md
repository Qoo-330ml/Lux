# LUX-084：TMDb 自动合集实施记录

## 范围

- 由电影的 TMDb provider ID 查询电影详情，再读取 belongs_to_collection 和 collection 详情。
- 合集作为 media_items 中的 BOX_SET，并通过 collections/collection_items 保存来源、排序和成员关系。
- 刷新使用 library + provider + provider ID 唯一键，重复刷新更新同一 BOX_SET，不创建重复数据。
- 集合详情和成员列表复用媒体库 ACL；成员无权访问时从结果中隐藏。

## 已完成

- [x] TMDb client 增加电影所属 collection 和 collection 详情读取。
- [x] 自动刷新接口：POST /api/v1/admin/items/{itemId}/collection/refresh。
- [x] Lux 集合详情：GET /api/v1/collections/{collectionId}。
- [x] Emby 成员列表：GET /Items/{collectionId}/Children。
- [x] 本地 TMDb stub 覆盖幂等刷新和跨库 ACL 过滤。

## 安全和边界

- TMDb token 只由客户端 boundary 使用，不进入响应。
- 合集不复制或移动媒体文件。
- 当前按刷新来源电影所在媒体库建立合集；同一 TMDb collection 在不同媒体库中分别维护，避免跨库 ACL 泄漏。
- 无 TMDb provider ID 或不属于合集的电影不会创建空 BOX_SET。
