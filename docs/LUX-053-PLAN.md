# LUX-053：待处理和候选管理 API 实施计划

## 范围

持久化低置信识别候选，提供管理员专用的 pending 分页、条目候选搜索和字段差异预览。候选数据来自脱敏 stub、TMDb 管理员搜索或后续识别任务；本阶段不自动选择、不写回 NFO。

## 实现

- [x] 新增 `metadata_candidates` schema、状态和索引。
- [x] 新增 candidate application service，校验 JSON 并生成当前值/候选值/provenance 差异。
- [x] 新增 pending 分页和条目候选搜索 API，页大小限制 1-100。
- [x] 所有端点使用管理员 session 权限；普通用户返回 403。
- [x] 集成测试覆盖分页、搜索、字段预览、非法分页和不存在条目。

## 明确不做

- 不在 HTTP handler 中调用 TMDb、执行匹配或写回。
- 不实现候选选择和 metadata PATCH；后续 LUX-054/LUX-056 处理。
