# LUX-050：字段级来源和锁定规则实施计划

## 范围

建立可复用的字段级 metadata state：每个字段分别记录来源，自动合并遵循锁定本地 > 本地 NFO > 已确认 TMDb 本地化值 > fallback；空字符串不产生覆盖。TMDb HTTP 客户端留到 LUX-051。

## 实现

- [x] 增加 `MetadataField`、`MetadataSource`、`MetadataCandidate` 和 `MetadataState`。
- [x] 支持字段独立合并、锁定字段保护、空字符串过滤和 JSON round-trip。
- [x] NFO enrichment 读取现有字段状态，写回字段级 `metadata_provenance_json` 与 `locked_fields_json`。
- [x] 增加表驱动优先级、锁定和空值测试。

## 验收结果

`cargo test --locked --test metadata` 通过 7 项测试；覆盖本地 NFO 优先于 TMDb、TMDb 优先于 fallback、锁定字段不被自动刷新、空在线字段不清除有效值，以及 NFO provenance 持久化。

## 明确不做

- 不在本任务调用真实 TMDb。
- 不实现候选搜索、匹配、管理 API 或 NFO 写回；分别属于 LUX-051 至 LUX-054。
