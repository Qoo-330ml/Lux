# ADR-052：弹幕子系统重做（草案，待项目负责人逐项批准）

## 状态

**已批准（P1–P5、P7；P6 暂缓）。** 负责人已批准实现；P2 按“存量旁车规范化”修订。每项合同变更在实现前仍需把具体改动给负责人看。
批准后，被批准的条目才取代 ADR-011 的对应部分；未批准的条目不改变现状。

## 背景

ADR-011 与 LUX-150/213/214 建立了首版弹幕：XML 旁车、Dandanplay 兼容上游、`danmaku.match`
插件协议、Emby `/api/danmu/*` 与 Web `/api/v1/items/{id}/danmaku*` 两套读取端点。审阅现状发现：

1. **Web 渲染**（已在任务 A 重做，不涉及合同）：4 MiB/5 万条硬上限、倍速被忽略、React 逐帧重渲染、
   宽度估算、静态轨道分配、错误状态未接到界面。
2. **读取路径重复**：`read_sidecar_for_source` 与 `read_registered_sidecar_for_source`
   除一个仓储查询外完全相同；Emby 与 Lux 两个 handler 各自重复“鉴权 → ACL → 读 → 响应”。
3. **校验不一致**：后端 `validate_danmaku_xml` 只检查根元素和至少一个 `<d>`，不检查条目合法性；
   前端解析器按另一套规则丢弃条目。同一份文件两端的“合法”含义不同。
4. **`danmaku.rs` 1400+ 行**：路径安全、XML 校验、原子写入、任务编排、插件调用、候选标题构造混在一个文件。
5. **只能自动匹配、单来源**：`danmaku_tracks` 对 `media_source_id` 唯一，插件只有 `danmaku.match`；
   匹配错误或未命中时，用户和管理员都没有手动搜索、换集、重匹配的手段。
6. **每次读取整文件**：大 XML 每次请求整段读入内存并重新校验。

## 提案与变更清单

每项独立可批准、可拒绝。标注“合同”的条目会改动公共模型，需要你明确批准后才派实现任务。

### P1 后端内部整理（无合同变更）— 任务 C1

- 把 `src/application/danmaku.rs` 拆成 `danmaku/{path,xml,sidecar,job,matching}.rs`，对外 `DanmakuService` API 不变。
- 合并两个 `read_*sidecar*` 为一个，登记要求作为参数。
- Emby 与 Lux handler 共用一个“读取已授权弹幕”应用服务函数，handler 只做协议映射。
- 仓库内 `tests/plugins/danmaku*.rs` 行为测试保持通过；不改表、不改协议、不改响应字节。

### P2 校验统一与存量旁车规范化（**合同：旁车合法性定义**）— 任务 C2

负责人要求：已有弹幕功能下载过的旁车必须保留可用，不能因收紧校验而失效。因此改为“宽进、读时过滤、可选规范化”：

- `validate_danmaku_xml` 增加条目级统计（有效 / 被丢弃条目数）。**至少一条有效条目即视为有效**；
  含个别脏条目的文件不再失效，读取与解析时跳过脏条目。只有有效条目为 0 才判无效。
- 规则写入 `docs/LUX-DEVELOPMENT.md` §14.5，并与 Web `parse.ts` 共享同一份测试向量。
- 新增管理员后台作业 `DANMAKU_NORMALIZE`（持久化、可取消、可重启恢复，复用现有作业框架）：
  只处理 **Lux 自己写入的旁车**（`danmaku_tracks.provider` 非空，且路径在媒体根内），逐个原子重写为规范 XML：
  丢弃无效条目、字号夹到 12–64、文本裁到 200 字符、保留 mode 1–6 并把其余模式丢弃、去掉控制字符。
  **用户自带的旁车永不改写**，只在读取时过滤。
- 重写前后条目数写入作业项；重写失败不影响原文件。作业提供 dry-run 报告（会改多少、丢多少）。
- 不引入新表：作业项复用 `danmaku_match_job_items`（新增 status 值 `NORMALIZED`/`UNCHANGED` 需要迁移，
  属表约束变更，随 C2 批准）。

### P3 读取侧元数据与缓存（**合同：Lux API 响应新增字段，向后兼容**）— 任务 C3

- `GET /api/v1/items/{id}/danmaku` 响应新增可选字段：`entryCount`、`byteSize`、`provider`、`updatedAt`。
  现有字段 `available/format/rawUrl/sourceId` 不变，旧前端忽略新字段即可。
- `…/danmaku/raw` 增加 `ETag`（旁车 fingerprint）与 `If-None-Match` → 304；`Cache-Control` 从
  `private, no-cache` 改为 `private, no-cache` + 校验器（仍每次再验证，但可 304）。
- 需要在 `danmaku_tracks` 增加 `entry_count INTEGER`、`byte_size INTEGER` 两列（**合同：表结构**，
  可空，迁移在 SQLite 与 PostgreSQL 各一份，空库可跑；旧行在下次扫描/校验时回填）。
- 因 `ts-rs` 生成类型，需同步 `web/src/lib/api/generated/` 与 `docs/API-TYPES-SYNC.md`。

### P4 插件协议 v2：搜索与按集取弹幕（**合同：插件协议**）— 任务 D1

- `danmaku` 插件新增可选能力：
  - `danmaku.search`：入参 `{ "query": string, "limit": number }`，返回候选
    `[{ provider, animeId, animeTitle, episodes:[{ episodeId, title }] }]`，条数有上限。
  - `danmaku.fetch`：入参 `{ provider, episodeId }`，返回 `{ xmlBase64 }`（沿用现有大小限制）。
- 现有 `danmaku.match` 保持不变；声明了 `danmaku.search`/`fetch` 的插件才有手动匹配能力，
  旧插件继续工作，UI 对其隐藏手动匹配入口。
- 宿主仍不向插件传路径或上游地址；插件不能写文件。
- 需同步 `docs/PLUGIN-SDK.md`、`plugin_protocol.rs` 的 manifest 校验（能力集合允许 `search`/`fetch`），
  `plugins/org.lux.danmaku` 与 Lux-plugins 仓库中的插件本体需要对应升级（**插件本体不在本仓库**，
  需要你确认是否由我们一并修改该仓库）。

### P5 手动匹配 API（**合同：Lux API 新端点**）— 任务 D2，依赖 P4

- `GET  /api/v1/admin/items/{itemId}/danmaku/search?query=&sourceId=`：管理员搜索候选（经插件，不在用户请求里扫库，
  受速率与上限限制，结果分页上限 50）。
- `POST /api/v1/admin/items/{itemId}/danmaku/match`：`{ sourceId, provider, animeId, episodeId, overwrite }`，
  创建单条持久化后台作业（复用 `danmaku_match_jobs`，`total_count=1`），返回 202。
- `DELETE /api/v1/admin/items/{itemId}/danmaku?sourceId=`：删除已登记旁车与 track（需 CSRF；
  仅删除 Lux 写入且在媒体根内的 `.xml`，对用户自带旁车须二次确认字段 `confirmUserSidecar=true`）。
- 全部需要管理员 Web session + CSRF；错误信息脱敏，不回显上游 URL。
- `danmaku_match_job_items` 无需改表（已有 provider_*_id）。`danmaku_match_jobs.library_id` 对单条作业仍取条目所属库。

### P6 多来源合并（**合同：数据模型，高成本，建议暂缓**）

- 目前 `danmaku_tracks UNIQUE(media_source_id)`，一个媒体源一个旁车。多来源需要放宽唯一约束并定义合并/去重策略、
  文件命名（`name.xml` + `name.<provider>.xml`）、ACL 与 Emby 端点如何展示。
- 与 Emby 客户端兼容性冲突风险高（`/api/danmu/{id}/raw` 只能返回一份）。
- **建议本轮不做**，待 P1–P5 稳定、有真实多来源需求后另立 ADR。

### P7 Web 侧用户设置与手动匹配 UI（无后端合同变更，依赖 P3/P5 的字段）— 任务 B、D3

- 任务 B（不依赖后端）：透明度、字号倍率、速度倍率、显示区域、按类型屏蔽、关键词屏蔽、时间偏移，
  存于浏览器 localStorage，不上传服务器。
- 任务 D3：播放器内“弹幕匹配”抽屉（仅管理员可见）：搜索 → 选集 → 提交 → 轮询作业状态；
  轮询遵守 `docs/WEB-POLLING-AUDIT.md`。

## 顺序与依赖

```
A（Web 渲染，进行中）──► B（用户设置）
P1 (C1) ──► P2 (C2) ──► P3 (C3)
P4 (D1) ──► P5 (D2) ──► D3（UI）
最后：E（smoke 脚本、COMPATIBILITY/PERFORMANCE 记录、完整检查）
```

- C1 无合同变更，可立即派发；C2、C3、D1、D2 每项在你批准后才派。
- 同一时刻 C 系列与 D 系列都会改 `src/application/danmaku*` 与 `src/api/`，必须串行，避免冲突。

## 风险

- 旁车校验收紧（P2）会让个别历史旁车被判无效；需要提供“重新校验并列出无效旁车”的管理员只读报告（并入 C2）。
- 插件本体不在本仓库（P4）：协议升级与插件升级要有兼容窗口，宿主必须容忍旧插件。
- Emby 客户端对 `/api/danmu/*` 的真实兼容性仍无实测记录（ADR-011 的遗留项），本提案不改变该端点的字节行为。
- 手动匹配会让管理员触发上游请求；必须保持“不在普通用户请求里调用上游”，并限制频率。

## 需要负责人逐项决定

| 条目 | 建议 | 决定 |
|---|---|---|
| P1 后端内部整理 | 批准，立即派发 | 已批准 |
| P2 校验统一 + 存量旁车规范化 | 批准（已修订） | 已批准 |
| P3 响应新增字段 + 表新增两列 + ETag | 批准 | 已批准 |
| P4 插件协议新增 search/fetch | 批准；并确认是否同步修改 Lux-plugins 仓库 | 已批准，同步修改插件仓库（/Volumes/Toshiba/mywork/Lux-plugins） |
| P5 手动匹配/删除 API | 批准（依赖 P4） | 已批准 |
| P6 多来源合并 | 暂缓 | 暂缓（P1–P5 稳定后再议） |
| P7 用户设置（B）与匹配 UI（D3） | 批准 | 已批准 |
