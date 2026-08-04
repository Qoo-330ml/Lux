# LUX-082 首页聚合

## 范围

提供一次 Lux 首页请求所需的继续观看、媒体库入口和每个媒体库的最新资源横栏，并补齐 Emby Latest；继续观看和最新资源使用现有用户状态和媒体库 ACL。

## 实现

- [x] 新增 `GET /api/v1/home`，返回继续观看摘要和可见媒体库入口。
- [x] 首页每个可见媒体库入口返回最多 12 条按 `added_at` 倒序的 `latest` 资源，避免前端逐库请求。
- [x] 新增 `GET /Users/{userId}/Items/Latest`，复用 ACL、筛选和稳定排序路径。
- [x] Latest 使用 DateCreated/UUID v7 倒序作为当前稳定的最近添加排序。

## 验证

- 既有 NextUp、Resume、目录和 ACL 集成测试保持通过。
- 全量 ARM64 回归覆盖首页相关路径。

## 明确不做

- 本阶段不引入首页缓存；不改变第三方客户端真实请求前的 DTO 兼容边界。
