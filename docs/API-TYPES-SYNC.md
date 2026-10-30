# 前后端 DTO 类型同步（B3）

`web/src/lib/api/types.ts` 与 `web/src/lib/api/client.ts` 目前手工镜像 Rust 响应，字段漂移只能在运行时暴露。
本文件按三个阶段记录这次试点：

- 阶段 1（本文件主体）：清点 `src/api/lux_api.rs` 的全部路由，标注响应类别、Rust 类型、`types.ts` 对应类型，估算改造量并给出试点范围。
- 阶段 2：以可选 feature 引入 `ts-rs`，把试点响应改成具名结构体并生成 TypeScript 绑定（见「阶段 2 小结」）。
- 阶段 3：把「重新生成 → 与已提交产物逐字节比对」接入 `scripts/check-all.sh`（见「阶段 3 小结」）。

范围约定：

- 只覆盖 **Lux 自有 API**（`/api/v1`，不含 Emby 兼容层）。Emby 路由的响应必须保持 Emby 形状（`docs/decisions/003-emby-compatibility-boundary.md`），不参与类型生成。
- 只覆盖响应体；请求体（`Deserialize`）的 TS 类型不在本次试点内。

## 判定标准

对每条路由只按**成功路径（2xx）的响应体**分类：

| 标记 | 含义 |
| --- | --- |
| (a) | 返回已有具名 `Serialize` 结构体（或结构体的 `Vec`），类型可被 `ts-rs` 直接派生 |
| (b) | `json!` 手拼 / 返回 `serde_json::Value`，字段名只存在于宏调用里 |
| (c) | 非 JSON：图片字节流、媒体流、字幕文本、XML |
| 204 | 成功但无响应体 |

统计口径：`src/api/lux_api.rs` 共 **45 条 route 条目、47 个 handler**（`/items/{item_id}/metadata` 与 `/people/{person_id}` 各有两个方法）。

## 全量路由清点

| # | 路由 | 方法 | handler | 类别 | 响应形状 / Rust 类型 | `types.ts` 对应 |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | `/api/v1/libraries` | GET | `lux_list_libraries` | (b) | `{libraries:[{id,name,kind,coverImageUrl}], showMetadataPending}`（`media.rs:590`） | `LibrariesResponse` / `Library` |
| 2 | `/api/v1/libraries/{library_id}/cover` | GET, HEAD | `lux_library_cover` | (c) | 图片字节流 + `Content-Type` | — |
| 3 | `/api/v1/favorites` | GET | `lux_list_favorites` | (b) | `lux_catalog_page_json_for_user`（`media.rs:3936`） | `PageResponse<MediaItem>` |
| 4 | `/api/v1/search` | GET | `lux_search` | (b) | 同上 | `PageResponse<MediaItem>` |
| 5 | `/api/v1/home` | GET | `lux_home` | (b) | `{continueWatching,continueWatchingTotal,recentlyAdded,recentlyAddedTotal,recommended,libraries:[…+latest]}` | `HomeResponse` |
| 6 | `/api/v1/home/carousel` | GET | `lux_home_carousel` | (b) | `{recommended:[…]}` | `HomeCarouselResponse` |
| 7 | `/api/v1/home/libraries/latest` | GET | `lux_list_home_library_latest` | (b) | `{libraries:[{libraryId,items}]}` | `HomeLatestLibrariesResponse` |
| 8 | `/api/v1/continue-watching` | GET | `lux_list_continue_watching` | (b) | `lux_catalog_page_json_for_user` | `PageResponse<MediaItem>` |
| 9 | `/api/v1/libraries/{library_id}/items` | GET | `lux_list_library_items` | (b) | `{items,total,page,pageSize}`（合集卡片分支同形状） | `PageResponse<MediaItem>` |
| 10 | `/api/v1/libraries/{library_id}/latest` | GET | `lux_list_library_latest` | (b) | `{items}` | `PageResponse<MediaItem>`（**漂移**，见下） |
| 11 | `/api/v1/items/{item_id}` | GET | `lux_get_item` | (b) | 详情 `Value`（`load_lux_item_detail` → `detail.body`） | `MediaItem` |
| 12 | `/api/v1/people` | GET | `lux_search_people` | (a)+(b) | `{items:Vec<ActorView>,total,page,pageSize}`：`items` 是具名结构体，信封是 `json!` | `PageResponse<MediaActor>` |
| 13 | `/api/v1/people/{person_id}/items` | GET | `lux_get_person_items` | (b) | `lux_catalog_page_json_for_user` | `PageResponse<MediaItem>` |
| 14 | `/api/v1/people/{person_id}` | GET, PATCH | `lux_get_person` / `lux_update_person` | (a) | `ActorView`（`application/people/service.rs:583`） | `PersonDetail` / `MediaActor` |
| 15 | `/api/v1/people/{person_id}/favorite` | PUT | `lux_set_person_favorite` | 204 | — | `void` |
| 16 | `/api/v1/people/{person_id}/image` | GET | `lux_get_person_image` | (c) | 图片 / 302 | — |
| 17 | `/api/v1/people/{provider}/{person_id}/image` | GET | `lux_get_person_image_for_provider` | (c) | 图片 / 302 | — |
| 18 | `/api/v1/items/{item_id}/children` | GET | `lux_get_children` | (b) | `lux_catalog_page_json_for_user` | `PageResponse<MediaItem>` |
| 19 | `/api/v1/collections/{collection_id}` | GET | `lux_get_collection` | (b) | `{collection,items,page,pageSize,total}` | `CollectionDetailResponse` |
| 20 | `/api/v1/collections/{collection_id}/items/{work_item_id}/source` | PUT | `lux_set_collection_source` | 204 | — | `void` |
| 21 | `/api/v1/items/{item_id}/images/{image_type}` | GET, HEAD | `lux_image` | (c) | 图片字节流 | — |
| 22 | `/api/v1/items/{item_id}/images/{image_type}/{image_index}` | GET, HEAD | `lux_image_at_index` | (c) | 图片字节流 | — |
| 23 | `/api/v1/items/{item_id}/images` | GET | `lux_list_item_images` | (b) | `{images:[item_image_json…]}` | `{ images?: ItemImage[] }`（内联）/ `ItemImage` |
| 24 | `/api/v1/items/{item_id}/images/search` | POST | `lux_search_item_images` | (b) | `{images:[image_candidate_json…]}` | `{ images?: ImageSearchResult[] }` / `ImageSearchResult` |
| 25 | `/api/v1/items/{item_id}/images/select` | POST | `lux_select_item_image` | (b) | `{image:item_image_json}` | `{ image: ItemImage }` |
| 26 | `/api/v1/items/{item_id}/subtitles/{stream_index}` | GET, HEAD | `lux_subtitle` | (c) | 字幕文本（vtt/srt） | — |
| 27 | `/api/v1/items/{item_id}/danmaku` | GET | `lux_danmaku_info` | (b) | `{available,format,sourceId,rawUrl}` | `WebDanmakuInfo` |
| 28 | `/api/v1/items/{item_id}/danmaku/raw` | GET | `lux_danmaku_raw` | (c) | `application/xml` | — |
| 29 | `/api/v1/items/{item_id}/stream` | GET, HEAD | `lux_stream` | (c) | 媒体流（含 Range） | — |
| 30 | `/api/v1/items/{item_id}/playback` | GET | `lux_get_playback` | (b) | 播放状态 `json!` | `PlaybackState` |
| 31 | `/api/v1/items/{item_id}/progress` | POST | `lux_post_progress` | 204 | — | `void` |
| 32 | `/api/v1/playback/sessions` | POST | `lux_create_web_playback_session` | (b) | `create_web_playback_session_json`（`playback.rs:3645`）返回 `Value` | `WebPlaybackSession` |
| 33 | `/api/v1/playback/bootstrap` | POST | `lux_create_web_playback_bootstrap` | (b) | `{playback,session}` 等 `json!` | `WebPlaybackBootstrap` |
| 34 | `/api/v1/playback/sessions/{session_id}/events` | POST | `lux_web_playback_event` | (b) | `{accepted,duplicate,stale}` | 内联 `{accepted,duplicate,stale}` |
| 35 | `/api/v1/playback/sessions/{session_id}/heartbeat` | POST | `lux_web_playback_heartbeat` | (b) | `{sessionId,expiresAt}` | 内联 `{sessionId,expiresAt}` |
| 36 | `/api/v1/playback/sessions/{session_id}/stats` | GET | `lux_web_playback_stats` | (b) | `web_playback_stats_json`（`playback.rs:4571`） | `WebPlaybackSessionStats` |
| 37 | `/api/v1/playback/sessions/{session_id}/direct` | GET, HEAD | `lux_web_playback_direct` | (c) | 媒体流（signature 校验） | — |
| 38 | `/api/v1/playback/sessions/{session_id}/range` | GET | `lux_web_playback_range` | (c) | 媒体流（Range） | — |
| 39 | `/api/v1/playback/sessions/{session_id}/hls/{*asset}` | GET, HEAD | `lux_web_playback_hls` | (c) | m3u8 / fMP4 字节流 | — |
| 40 | `/api/v1/playback/sessions/{session_id}` | DELETE | `lux_delete_web_playback_session` | 204 | — | `void` |
| 41 | `/api/v1/items/{item_id}/favorite` | PUT | `lux_set_favorite` | 204 | — | `void` |
| 42 | `/api/v1/items/{item_id}/played` | PUT | `lux_set_played` | 204 | — | `void` |
| 43 | `/api/v1/playback-history` | GET | `lux_list_playback_history` | (b) | `{events:Vec<PlaybackHistoryEventView>,page,pageSize}` | **无前端消费** |
| 44 | `/api/v1/items/{item_id}/metadata` | GET, PATCH | `lux_get_metadata` / `lux_update_metadata` | (b) | `metadata_json`（`media.rs:1274`） | `ItemMetadata` / `MetadataFieldName` |
| 45 | `/api/v1/items/{item_id}/download` | GET, HEAD | `lux_download` | (c) | 文件下载流 | — |

### 统计

| 类别 | route 条目 | 占比 |
| --- | --- | --- |
| (a) 全新具名结构体 | 1 | 2% |
| (a)+(b) 混合（元素具名、信封手拼） | 1 | 2% |
| (b) `json!` 手拼 / `Value` | 25 | 56% |
| (c) 非 JSON | 12 | 27% |
| 204 无响应体 | 6 | 13% |
| **合计** | **45** | 100% |

结论：Lux API 的 JSON 响应里 **只有 1 条路由（`/people/{id}`）已经是纯具名结构体**，(b) 占 56%。
`types.ts` 中 221 个导出类型的绝大部分（含整个 `Admin*` 家族）对应的是 `src/api/admin/**` 与 `src/api/users.rs` 的路由，
不在本次清点范围内，但同样属于 (b)。

### 清点中发现的既有漂移（本次不修，先记录）

这些都是「手工镜像」已经对不上的实例，可作为后续任务的验收样本：

1. `GET /api/v1/libraries/{library_id}/latest` 只返回 `{items}`，而 `client.ts:646` 按 `PageResponse<MediaItem>` 解读（`page`/`pageSize`/`total`/`hasNext` 全为 `undefined`）。
2. `GET /api/v1/libraries` 的 `kind` 取值来自 `LibraryKind::as_str()`，`types.ts` 写死为 `"MOVIE" | "SERIES" | "MIXED" | "HOMEVIDEOS"`；但同一响应里还会推送常量 `GLOBAL_COLLECTION_LIBRARY_ID` 生成的「合集」卡片，其 `kind` 为 `"MIXED"`、`coverImageUrl` 恒为 `null`，属于接口层面的合成条目而非领域实体。
3. `GET /api/v1/people` 的响应没有 `hasNext`，但前端按 `PageResponse<MediaActor>` 使用（`hasNext` 恒 `undefined`）。
4. `GET /api/v1/playback-history` 在前端**完全没有调用点**（`grep -rn "playback-history" web/src` 为空），是 (b) 里唯一的「零消费者」路由。

## (b) → 具名结构体的工作量估算

按**响应形状家族**分组（同一家族的改动一次覆盖多条路由）：

| 组 | 涉及路由 | 需要新建的结构体 | 估算 | 难点 |
| --- | --- | --- | --- | --- |
| G-MediaItem（条目卡片/详情） | #3 #4 #5 #6 #7 #8 #9 #10 #11 #13 #18 #19 | `MediaItem` 家族：卡片 ~26 个扁平字段 + `imageTags` + `userData`；详情另加 `mediaSources`/`actors`/`nfo` | **大（5~8 天）** | 现在是 `json!` 里逐字段拼 + `object.insert("metadataPending")` 后置补字段；`MediaStream`/`MediaNfoDetails` 层级深；`Option` 与「键缺省」语义混用（`skip_serializing_if` 与显式 `null` 并存），不改字节就必须逐字段核对 |
| G-Page（分页信封） | #3 #4 #8 #13 #18 | `PageResponse<T>`，注意 `total` 是条件字段（`total_included`） | 中（1 天） | `{items,page,pageSize,hasNext}` 与 `{items,total,page,pageSize}` 是**两种**信封，必须分成两个结构体而不是一个带 `Option` 的 |
| G-Libraries（媒体库卡片） | #1 #5 #7 | `LibraryCard` / `LibrariesResponse` / `HomeLibraryCard` | 小（0.5 天） | #1 与 #5 的卡片字段集**不同**（#5 多 `latest`），不能共用一个结构体；`kind` 是 `as_str()` 出来的 `&str`，要改成枚举才能生成字面量联合类型 |
| G-Playback（播放会话） | #30 #32 #33 #34 #35 #36 #43 | `PlaybackState`、`WebPlaybackSession`、`WebPlaybackBootstrap`、`WebPlaybackSessionStats`、`PlaybackHistoryEventView`、2 个内联信封 | **大（3~5 天）** | `#32`/`#33` 的 JSON 由 `playback.rs` 里 3 个长函数拼装，中间还夹着 HLS/Direct 计划计算；字段里有签名 URL 与 `Option<i64>` 时间戳 |
| G-Images（图片） | #23 #24 #25 | `ItemImage`、`ImageCandidate`、2 个信封 | **小（0.5 天）** | `language` 字段在 `item_image_json` 里恒为 `Value::Null`（从未被真实赋值）；`imageType` 是 `String` 且大小写混用（`url` 里小写、`imageType` 保留原值） |
| G-Metadata（条目元数据） | #44（GET+PATCH） | `ItemMetadata` + `MetadataField` 枚举 | **小（0.5 天）** | `lockedFields` 目前是「先 `serde_json::to_string` 再 `from_str`」的往返，改造时可直接用 `Vec<MetadataField>` |
| G-Danmaku（弹幕信息） | #27 | `WebDanmakuInfo` | **很小（0.2 天）** | `available: true` 与 `format: "BILIBILI_XML"` 是字面量，`ts-rs` 默认生成 `boolean`/`string` |
| G-Collection（合集详情） | #19 | `CollectionDetailResponse` + `CollectionMember` | 中（1 天） | 依赖 G-MediaItem |

合计约 **11~16 人日**。其中 G-MediaItem 与 G-Playback 两点占了 70% 以上，且都依赖「先有一个可靠的
`MediaItem`/会话结构体」，不适合作为第一步试点。

### 序列化字节顺序的前提（影响所有 (b) 改造）

`serde_json` 在本项目**没有启用 `preserve_order`**（`Cargo.lock` 中 `serde_json` 依赖只有 `itoa`/`memchr`/`serde`/`serde_core`/`zmij`，无 `indexmap`），
所以 `serde_json::Map` 是 `BTreeMap`：`json!` 输出的对象键**按字母序**排列。
而 `#[derive(Serialize)]` 结构体按键**声明顺序**输出。要让响应字节级不变，结构体字段必须按字母序声明，
或接受「键顺序变化、内容不变」。试点按前者做（见阶段 2 小结），并同时断言 `Value` 相等与字节相等。

## 试点范围（本任务的阶段 2）

选 **3 组**，共同点是响应形状扁平、字段少、没有嵌套领域对象、前端只有读路径：

| 组 | 路由 | 新建结构体 | 对应 TS（`types.ts`） |
| --- | --- | --- | --- |
| **P1 条目图片** | `GET /api/v1/items/{item_id}/images`、`POST /api/v1/items/{item_id}/images/select` | `ItemImage` + `ItemImagesResponse` + `ItemImageSelectResponse` | `ItemImage` |
| **P2 条目元数据** | `GET` / `PATCH /api/v1/items/{item_id}/metadata` | `ItemMetadata` + `MetadataField` | `ItemMetadata`、`MetadataFieldName` |
| **P3 弹幕信息** | `GET /api/v1/items/{item_id}/danmaku` | `WebDanmakuInfo` | `WebDanmakuInfo` |

选择理由：

- **P1** 是 (b) 里最典型的「宏里写字段」形状（9 个扁平字段），有真实前端消费（`MediaImageEditor`、`MediaStills`），且 `ItemImage` 在两个响应信封里复用，能验证「同一结构体被多个信封引用」的生成效果。
- **P2** 覆盖**读写同一形状**（GET 与 PATCH 返回同构体）和**枚举**（`lockedFields` → `"title" | "originalTitle" | …`），验证 `#[serde(rename_all)]` 能否正确传导到 TS。
- **P3** 覆盖**字面量字段**（`available: true`）与**恒 `null` 的 `Option`**（`sourceId`），是最小的端到端样本，用于验证生成链路本身。
- 三者都不含 `MediaItem`，所以不依赖 G-MediaItem 的改造，可以独立落地。

**为什么试点不选 `/api/v1/libraries` 与 `/api/v1/continue-watching`**（任务说明中的示例）：

- `/libraries` 的信封（`LibrariesResponse`）本身很简单，但它的 `Library` 卡片在 `types.ts` 里同时服务 `/api/v1/home`（多一个 `latest: MediaItem[]` 字段），
  单独把卡片结构体收窄会让 `HomeResponse` 的用法编译失败；要正确生成就得先完成 G-MediaItem，工作量远超试点。
- `/continue-watching` 的类型是 `PageResponse<MediaItem>`，`MediaItem` 是全项目最大的 DTO（30+ 字段 + 3 层嵌套），正是上表里最贵的一组。
  把它放进试点等于「用最难的形状验证最不成熟的链路」。

两者都在「如何扩展到更多路由」一节给出具体的前置条件。

## 阶段 1 结论

- Lux API 45 条路由中，(b) `json!` 手拼占 56%，(a) 纯具名结构体只有 1 条；这正是 `types.ts` 需要手工镜像的根因。
- 全量改造需要 11~16 人日，必须先做 `MediaItem` / 播放会话两个大件；本次试点取 3 组小形状（约 1.2 人日）先打通「Rust 结构体 → 生成 TS → re-export」和防漂移检查。
- 清点过程中发现 4 处既有漂移，已记录为后续验收样本。
