# LUX-056 重新识别纵切片

## 范围

把管理员候选选择与已有的元数据合并、NFO 写回、图片下载和持久化状态串起来。写文件或图片失败时不提交 `ONLINE_CONFIRMED`，候选保持可重试。

## 实现

- [x] 新增管理员 POST `/api/v1/admin/items/{itemId}/identify/candidates/{candidateId}/select`，要求 Web session、管理员权限和 CSRF。
- [x] 支持 `fillMissing`：只补全空字段和缺失图片，不覆盖本地已有字段/图片。
- [x] 支持 `refreshUnlocked`：刷新未锁定字段和图片，锁定字段始终保留。
- [x] 将候选 JSON 的 common metadata、provider ID 和 poster/fanart URL 转换为写回任务。
- [x] 所有 NFO/图片成功后，在短事务中更新 metadata、provider ID、`ONLINE_CONFIRMED`，选中候选并拒绝同条目其他 pending 候选。
- [x] 写回失败返回可重试错误，候选仍为 `PENDING`，不谎报确认成功。

## 验证

- 端到端 stub 测试覆盖本地字段保护、NFO/两类图片写回、图片索引更新和在线确认状态。
- 损坏图片故障测试确认响应失败、候选保持 pending；修复候选后重试成功。
- 元数据单元测试覆盖补缺、刷新和锁定字段保护。

## 明确不做

- 本阶段不从 HTTP handler 直接调用 TMDb 搜索；候选由已有识别任务写入，候选中的图片 URL 由后续任务提供。
- 本阶段不实现批量选择、任务队列化或剧集识别；后续阶段扩展。
