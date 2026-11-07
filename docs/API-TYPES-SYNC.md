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

统计口径：`src/api/lux_api.rs` 共 **47 条 route 条目、49 个 handler**（`/items/{item_id}/metadata` 与 `/people/{person_id}` 各有两个方法；LUX-899 新增 `/items/{item_id}/similar`）。

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
| 32 | `/api/v1/playback/sessions` | POST | `lux_create_web_playback_session` | (b) | `create_web_playback_session_json`（`playback.rs:3645`）返回 `Value` | `WebPlaybackSession`（含 `bitrate` / `resolution`，见「手工镜像补充」） |
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
| 46 | `/api/v1/watch-history` | GET | `lux_list_watch_history` | (b) | `lux_catalog_page_json_for_user` | `PageResponse<MediaItem>` |
| 47 | `/api/v1/items/{item_id}/similar` | GET | `lux_get_similar` | (b) | `lux_catalog_page_json_for_user` | **无前端消费**（见「LUX-899 原生相似条目接口」） |

### 统计

| 类别 | route 条目 | 占比 |
| --- | --- | --- |
| (a) 全新具名结构体 | 1 | 2% |
| (a)+(b) 混合（元素具名、信封手拼） | 1 | 2% |
| (b) `json!` 手拼 / `Value` | 27 | 57% |
| (c) 非 JSON | 12 | 26% |
| 204 无响应体 | 6 | 13% |
| **合计** | **47** | 100% |

结论：Lux API 的 JSON 响应里 **只有 1 条路由（`/people/{id}`）已经是纯具名结构体**，(b) 占 57%。
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

- Lux API 47 条路由中，(b) `json!` 手拼占 57%，(a) 纯具名结构体只有 1 条；这正是 `types.ts` 需要手工镜像的根因。
- 全量改造需要 11~16 人日，必须先做 `MediaItem` / 播放会话两个大件；本次试点取 3 组小形状（约 1.2 人日）先打通「Rust 结构体 → 生成 TS → re-export」和防漂移检查。
- 清点过程中发现 4 处既有漂移，已记录为后续验收样本。

## 阶段 2 小结（试点实现）

### 依赖选择

`Cargo.toml`：

```toml
[features]
ts-bindings = ["dep:ts-rs"]

[dependencies]
ts-rs = { version = "12.0.1", optional = true, default-features = false, features = [
    "serde-compat",
] }
```

- **版本**：`12.0.1` 是 ts-rs 当前稳定版；其 MSRV 1.78 低于本项目的 1.94，与 serde 1.x 兼容。
  12.x 才具备本次需要的两个能力：`#[ts(optional = nullable)]`（生成 `?: T | null`，与既有手写类型同形）
  和 `#[ts(export)]`（为每个类型生成 `export_bindings_<type>` 测试，供防漂移检查复用）。
- **默认特性**：ts-rs 唯一的默认特性是 `serde-compat`，本项目**必须**保留（否则 `#[serde(rename_all)]`
  与 `Option` 语义不会传导到 TS）。这里仍写成 `default-features = false` + 显式 `serde-compat`，
  这样将来 ts-rs 若新增默认特性（例如会引入 dprint 的 `format`）不会悄悄进入构建。
- **`Cargo.lock`**：只新增 `ts-rs`、`ts-rs-macros`、`termcolor` 三个包，其余版本不变。
- **默认构建**：`cargo tree --locked -e normal` 不含 `ts-rs`；只有 `--features ts-bindings` 才出现。

### 试点结构体

| 类型 | 定义位置 | 覆盖路由 | 生成文件 |
| --- | --- | --- | --- |
| `ItemImage` | `src/api/media.rs` | `GET .../images`、`POST .../images/select` | `ItemImage.ts` |
| `ItemImagesResponse` | `src/api/media.rs` | `GET .../images` | `ItemImagesResponse.ts` |
| `ItemImageSelectResponse` | `src/api/media.rs` | `POST .../images/select` | `ItemImageSelectResponse.ts` |
| `ItemMetadata` | `src/api/media.rs` | `GET`/`PATCH .../metadata` | `ItemMetadata.ts` |
| `MetadataField` | `src/application/metadata.rs` | `ItemMetadata.lockedFields` | `MetadataField.ts` |
| `WebDanmakuInfo` | `src/api/media.rs` | `GET .../danmaku` | `WebDanmakuInfo.ts` |
| `DanmakuFormat` | `src/api/media.rs` | `WebDanmakuInfo.format` | `DanmakuFormat.ts` |

`item_image_json`（`src/api/admin/admin_metadata.rs`）与 `metadata_json`、`danmaku_info_json`（`src/api/media.rs`）
仍然叫原来的名字，只是返回类型从 `serde_json::Value` 变成了结构体；因此两个 handler 的 `Json(...)` 调用点几乎没有改动。

### 保持序列化字节一致的两个关键点

1. **字段顺序**：`serde_json` 未启用 `preserve_order`，`json!` 输出的对象键是**字母序**（`BTreeMap`），
   而 `#[derive(Serialize)]` 按**声明顺序**输出。所以结构体字段按序列化后的键名字母序声明，
   否则响应字节会变（内容不变，但字节不同）。这条约束在每个 DTO 上都有 `//` 注释说明。
2. **`Option` / `null`**：结构体没有 `skip_serializing_if`，`None` 和其他 `Option` 一样序列化成 `null`，
   与改前完全一致。`#[ts(optional = nullable)]` **只影响 TypeScript 输出**（生成 `field?: T | null`），
   不改变 JSON。之所以标成可选，是为了让生成的类型与 `types.ts` 里既有的手写类型同形，
   从而满足「`types.ts` 对外导出名不变、前端其余代码零改动」。

`src/api/media.rs` 末尾的 `lux_dto_tests` 用**改前**抓取的字节字面量固定了 6 个断言：

- 先写测试（改前通过）→ 再改实现（仍通过），确保改前改后 `serde_json::to_string` 输出逐字节相同；
  字节相等强于 `serde_json::Value` 结构相等，因此不再单独断言 `Value`。
- 覆盖了 `null`、`0` 与非 `0` 的 `imageIndex`（URL 后缀分支）、`lockedFields` 缺失、`sourceId` 缺失。

### 生成命令与产物

```bash
scripts/generate-api-types.sh
# 等价于：cargo test --locked --features ts-bindings --lib export_bindings
```

脚本固定了两个环境变量（ts-rs 在测试运行时读取）：

| 变量 | 值 | 原因 |
| --- | --- | --- |
| `TS_RS_EXPORT_DIR` | 仓库根（绝对路径） | 让 `#[ts(export_to = "web/src/lib/api/generated/")]` 与调用目录无关 |
| `TS_RS_LARGE_INT` | `number` | ts-rs 默认把 `i64` 渲染成 `bigint`，会改变所有时间戳/计数类型 |

产物提交在 `web/src/lib/api/generated/`（7 个文件，含文件头注释与 `import type`）。
`web/src/lib/api/types.ts` 用 re-export 保持导出名不变：

```ts
export type { MetadataField as MetadataFieldName } from "./generated/MetadataField";
export type { ItemMetadata } from "./generated/ItemMetadata";
export type { ItemImage } from "./generated/ItemImage";
export type { WebDanmakuInfo } from "./generated/WebDanmakuInfo";
```

生成的类型与手写类型的差异（都是放宽，读路径零影响）：

- `ItemImage.source`：`string`（原来是 `string | null` 可选）。服务端一直发送字符串，`json!` 里 `source` 不是 `Option`。
- `WebDanmakuInfo.available`：`boolean`（原来是字面量 `true`）——Rust 侧是 `bool`，ts-rs 不表达「恒为 true」；
  若要保留字面量可加 `#[ts(type = "true")]`，本次选择接受放宽。
- `WebDanmakuInfo.format`：`DanmakuFormat` = `"BILIBILI_XML"`（用单变体枚举保住了字面量，未退化为 `string`）。
- `WebDanmakuInfo.sourceId`：`sourceId?: string | null`（原来是必填）。JSON 始终发送该键。

### 门禁结果

| 命令 | 结果 |
| --- | --- |
| `cargo build --locked` | 通过 |
| `cargo build --locked --features ts-bindings` | 通过 |
| `cargo test --locked --lib lux_dto_tests` | 5 → 6 个断言通过（改前/改后各跑一次） |
| `cargo test --locked --features ts-bindings --lib lux_dto_tests` | 通过 |
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --locked --all-targets --all-features -- -D warnings` | 通过 |
| `pnpm --dir web build` | 通过（生成文件参与 `tsc --noEmit`） |
| `pnpm --dir web test` | 127 个文件 / 986 个用例通过 |

## 阶段 3 小结（防漂移检查）

`scripts/check-api-types.sh` **在真实工作区原地执行**（不是临时副本），顺序是：

1. 把当前的 `web/src/lib/api/generated/` 快照到临时目录；
2. **先删掉** `web/src/lib/api/generated/` 里的全部 `*.ts`，再运行 `scripts/generate-api-types.sh` 重新生成。
   先删后生成是关键：ts-rs 只会写文件、不会删文件，DTO 改名后残留的 `OldName.ts` 会让
   `diff -ru` 永远报「多了一个文件」，而那条差异没有内容可以提交，检查就会永远红；
3. 按「已消失 / 新出现 / 内容变化」三类分别报告：
   - 已消失（ts-rs 不再生成的绑定）：列出路径，Git 跟踪的给 `git rm <路径>`，未跟踪的说明无需暂存
     （文件在步骤 2 已从工作区删除，只需提交删除）；
   - 新出现：列出 `git add <路径>`；
   - 内容变化：按文件打印 `diff -u`，并提示提交重新生成的产物；
4. 生成失败时把快照还原回 `web/src/lib/api/generated/`，不会留下半空目录；
5. 额外检查仓库根目录没有游离的 `*.ts`：`#[ts(export)]` 若漏写 `export_to`，ts-rs 会把文件写到导出根目录
   （本次开发中就触发过一次），这类文件不会被 `tsc` 收录，属于静默漂移；命中时直接列出待删路径。

检查失败后重新生成的产物会留在工作区供评审；把删除/新增的文件提交（或按提示 `git rm`）后，
下一次运行即通过。

已加入 `scripts/check-all.sh`（在 `cargo clippy` 与 `bash -n scripts/*.sh` 之后、Python 检查之前）。

运行方式：

```bash
scripts/check-api-types.sh          # 只跑这一项
./scripts/check-all.sh              # 完整门禁
```

注意：这项检查会以 `--features ts-bindings` 编译一次测试目标，是本仓库里唯一会引入 `ts-rs` 的路径。

### 生成时的环境变量

`scripts/generate-api-types.sh` 会**先 `unset` 所有 `TS_RS_*` 变量**，再显式设置 `TS_RS_EXPORT_DIR`
与 `TS_RS_LARGE_INT=number`。原因是 ts-rs 12.0.1 的 `Config::from_env` 还会读
`TS_RS_IMPORT_EXTENSION`（会把 `import` 改写成 `./ItemImage.js`）等变量，开发者 shell 里残留的设置
会造成「本地生成结果与提交产物不一致」的假漂移。

已验证：`TS_RS_IMPORT_EXTENSION=.js TS_RS_LARGE_INT=bigint scripts/generate-api-types.sh` 生成的产物
与提交内容完全一致。

## 手工镜像补充：手动分辨率上限（`maxHeight`）

手动画质上限没有走 ts-rs 试点（`bitrate` / `resolution` 仍由 `web_playback_session_json` 的 `json!` 拼装），
前端按本文件约定继续手工镜像。播放页的「画质」已从码率上限改为分辨率上限，对应关系如下。

> **后端实现状态**：下表的 `max_height` 请求字段与 `resolution` 响应字段由并行后端任务
> `playback-max-height-backend` 提供；该任务合并前，本文档按契约记录字段，Rust 侧尚未落地，
> 不应把表中“Rust 位置/类型”当成已存在的代码。合并后如字段名或可空性有出入，以后端实际实现为准并同步本表。

| Rust（契约，由 `playback-max-height-backend` 提供） | 位置 | `types.ts` | 说明 |
| --- | --- | --- | --- |
| 请求体 `max_height: Option<i64>` | `WebPlaybackSessionRequest` / `WebPlaybackBootstrapRequest` | `WebPlaybackRequestSelection.maxHeight?: number` | 可选，像素，144–4320；`client.ts` 把 `selection` 原样展开进请求体，不做字段重命名 |
| 响应 `resolution.sourceHeight` | `web_playback_session_json` | `WebPlaybackResolution.sourceHeight: number \| null` | 源视频高度（ffprobe `Height`），未探测为 `null` |
| 响应 `resolution.maxHeight` | 同上 | `WebPlaybackResolution.maxHeight: number \| null` | 请求里的用户上限回显，未提供为 `null` |
| 响应 `resolution.targetHeight` | 同上 | `WebPlaybackResolution.targetHeight: number \| null` | 实际输出高度；无法确定时为 `null` |
| 响应 `resolution.limited` | 同上 | `WebPlaybackResolution.limited: boolean \| null` | 仅当源高度 > `maxHeight`、服务端真的转码缩放时为 `true`；无法确定时为 `null` |
| 响应 `resolution` 键本身 | 同上 | `WebPlaybackSession.resolution?: WebPlaybackResolution \| null` | **可选**：旧服务端不带该键或显式给 `null`，前端回落到中性文案「原画（自动）」 |
| 请求体 `max_bitrate: Option<i64>` | 同上 | `WebPlaybackRequestSelection.maxBitrate?: number` | **仅保留在 API 类型中**：UI 不再提供码率档位，播放页不再读取/写入 `maxBitrate` URL 参数 |
| 响应 `bitrate.*` / `bitrate` 键 | 同上 | `WebPlaybackBitrate` / `WebPlaybackSession.bitrate?` | 继续保留（其它读路径仍可能使用），但播放页的画质文案已改用 `resolution` |

约定：

- **`resolution` 与 `bitrate` 都保持可选**，与 `WebPlaybackPlan.candidates`、`MediaSource.bitrate` 的既有写法一致：
  这是读路径上的放宽，不会让旧响应的测试夹具全部失效；真正的必填化应等 G-Playback 改造成结构体时一次完成。
- **语义**：源高度 > `maxHeight` 时服务端转码并缩放到 `maxHeight`；否则不变（`limited: false`）。
  前端不自行断言服务端没有报告的结论：`resolution`/`limited` 缺失或 `targetHeight/sourceHeight` 均为 `null` 时只显示「原画（自动）」。
  `limited: false` 的状态文案优先使用 `targetHeight`（服务端真实输出高度），缺失才回落到 `sourceHeight`。
- **query key 槽位改为 `maxHeight`**（缓存按分辨率变体隔离，切换画质才能触发新会话）：
  `["playback-bootstrap" | "web-playback-session", itemId, sourceId, audioStreamIndex, subtitleStreamIndex, maxHeight, attempt]`。
  读取单个槽位的 helper 跟着各自的 feature 走：`audioParamFromQueryKey`（`audio-track-selection.ts`，下标 3）、
  `maxHeightParamFromQueryKey`（`playback-quality.ts`，下标 5）。后续若要改成具名对象键，两处 helper 与
  `usePlaybackReporting` 的 `queryKey[0]` 判断需要一起改。
- **URL 参数**：`maxHeight` 是唯一画质参数；旧的 `maxBitrate` 参数不再读取/写入（手写 URL 会被忽略），
  切换片源时保留规范化后的 `maxHeight`，音轨/字幕索引则随旧片源一起丢弃。
- 档位生成、URL 参数、状态文案都在 `web/src/features/player/playback-quality.ts`（纯函数，有单测），不参与类型生成。

## 如何扩展到更多路由

1. **先定形状，再改代码**：在 `docs/` 或 PR 描述里写出目标 JSON（键顺序无所谓，但 `null` / 缺省 / 数字类型要写清）。
2. **先写字节固定测试**：在 `#[cfg(test)] mod` 里对「当前实现」的响应体 `serde_json::to_string` 断言字面量，
   跑通后再改实现。改实现后该测试必须**原样通过**；如果需要改字面量，说明响应已经变了，那是一次 API 变更而不是重构。
3. **把 `json!` 换成结构体**：字段按序列化后的键名字母序声明（见上文「保持序列化字节一致的两个关键点」），
   并把 `json!({ ... })` 里每个键的 `Option` 语义原样保留（不要顺手加 `skip_serializing_if`）。
4. **加派生属性**：

   ```rust
   #[derive(Serialize)]
   #[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
   #[cfg_attr(feature = "ts-bindings", ts(export, export_to = "web/src/lib/api/generated/"))]
   #[serde(rename_all = "camelCase")]
   pub(super) struct FooResponse { /* … */ }
   ```

   `Option<T>` 字段若要生成 `?: T | null`，加 `#[cfg_attr(feature = "ts-bindings", ts(optional = nullable))]`。
5. **处理嵌套**：嵌套类型也要加同样的 `cfg_attr`，否则 `--features ts-bindings` 编译失败（这是有意的：
   宁可编译不过，也不要生成 `any`）。枚举建议用 `#[serde(rename_all = ...)]` + 派生，字面量字段用单变体枚举表达。
6. **信封单独建模**：`{items,page,pageSize,total}` 与 `{items,page,pageSize,hasNext}` 是两种信封，各写一个结构体，
   不要用 `Option` 合成一个（否则 TS 会把可能不存在的字段标成必填/可选，二者都不准确）。
7. **生成并提交**：`scripts/generate-api-types.sh`，然后 `scripts/check-api-types.sh` 必须通过。
8. **收窄 `types.ts`**：把手写类型换成 `export type { X } from "./generated/X"`，**保持导出名不变**；
   若生成类型与手写类型有差异，逐条确认是「放宽」（读路径安全）还是「收窄」（需要同步改前端代码）。
   最后跑 `pnpm --dir web build` 与 `pnpm --dir web test`。

推荐顺序（按阶段 1 的工作量估算，风险从低到高）：

1. `G-Images` 的 `image_candidate_json`（`ImageSearchResult`，8 个扁平字段）；
2. `G-Page` 的两种分页信封（先只建模信封，元素仍是现成结构体或后续再收）；
3. `G-Libraries`（`LibraryCard` / `LibrariesResponse` / `HomeLibraryCard`，注意 `/api/v1/home` 的卡片多 `latest`）；
4. `G-MediaItem`（最大的一步，`MediaItem` + `userData` + `imageTags` + 详情态 `mediaSources`/`actors`/`nfo`）；
5. `G-Playback`（`WebPlaybackSession` / `WebPlaybackBootstrap` / `WebPlaybackSessionStats` / `PlaybackHistoryEventView`），
   它的 JSON 由 `playback.rs` 里几个长函数拼装，改造前建议先把拼装逻辑拆成纯函数。

`/api/v1/continue-watching`、`/favorites`、`/search`、`/children` 等 8 条路由共用 `G-Page` + `G-MediaItem`，
所以第 3、4 步完成后一次就能覆盖它们。

## 管理员手动弹幕匹配 DTO（ADR-052 P5）

`GET /api/v1/admin/items/{item_id}/danmaku/search` 的响应在实现时就按具名结构体写，并直接接入
ts-rs 生成流程（`--features ts-bindings` → `scripts/generate-api-types.sh`），因此不再需要手工镜像：

| 类型 | 定义位置 | 覆盖路由 | 生成文件 |
| --- | --- | --- | --- |
| `ManualDanmakuSearchResponse` | `src/api/admin/admin_danmaku.rs` | `GET .../danmaku/search` | `ManualDanmakuSearchResponse.ts` |
| `ManualDanmakuCandidate` | 同上 | 响应项 | `ManualDanmakuCandidate.ts` |
| `ManualDanmakuEpisode` | 同上 | 候选内的分集 | `ManualDanmakuEpisode.ts` |

- `web/src/lib/api/types.ts` 用 `export type { ... } from "./generated/..."` 转出这三个类型，导出名与
  Rust 类型一致；`web/src/lib/api/client.ts` 的 `manualDanmakuSearch` / `startManualDanmakuMatch` /
  `deleteItemDanmaku` 使用它们（`AdminDanmakuMatchJob` 继续复用现有手写类型）。
- `provider` 在 Rust 是 `Option<String>`，因此生成 `provider: string | null`；服务端始终输出该键（不跳过
  `null`），与 `WebDanmakuInfo.sourceId` 的处理一致。
- **请求体不参与生成**（本文件范围约定）。`POST .../danmaku/match` 的请求形状
  `{ sourceId?, provider?, animeId?, episodeId, overwrite }` 与 `DELETE .../danmaku` 的
  `sourceId`/`confirmUserSidecar` 查询参数在 `client.ts` 里手写，字段名与 `docs/API.md` 一致。
- `DELETE` 成功为 `204`，因此生成类型不适用；`POST` 复用现有 match-jobs 作业 DTO（`{ job }` 信封）。
- 生成命令与防漂移检查沿用第 3 节：`scripts/generate-api-types.sh`、`scripts/check-api-types.sh`
  （后者已接入 `scripts/check-all.sh`）。

## LUX-899 原生相似条目接口

`GET /api/v1/items/{item_id}/similar?limit=`（`src/api/lux_api.rs` 的 `lux_get_similar`）是相似条目的
权威实现；Emby `GET /Items/{id}/Similar` 改为调用同一个 `CatalogService::list_similar_items`，只保留
Emby DTO/参数适配。

- 响应形状直接复用 `lux_catalog_page_json_for_user`：`{items,page,pageSize,hasNext,total}`。`items` 是
  `lux_catalog_items_json_for_user` 生成的 Lux 媒体条目（`MediaItem` 家族），**不是** Emby 的
  `Items`/`TotalRecordCount` DTO。
- 因此该路由按本文件口径属于 **(b)**：信封与条目都是 `json!` 手拼的 `Value`，没有可派生的具名结构体。
  `total` 是有界候选池（最多 10 个源 Genre × 每个 200 个候选，即 2000）内的去重总数；`page` 是请求的
  1 起页码，`pageSize` 是生效的 `limit`，`hasNext = offset + items.len() < total`（`offset =
  (page - 1) * limit`）。`offset` 超出候选池时返回 200 与空 `items`。
- 目前没有前端消费（Web 详情页的相似区按规格另立任务），所以 `web/src/lib/api/types.ts` 与
  `generated/` 都不新增类型，也不需要重新生成绑定。后续 Web 接入时按「如何扩展到更多路由」的流程，
  先定 `{items,page,pageSize,hasNext,total}` 信封（G-Page）再处理 `MediaItem`（G-MediaItem），不要为此
  单独发明一个 `Option` 合成信封。
- 请求参数是 `limit`（默认 50、1-1000，空值按未提供，`0`/负数/非整数/超过 1000 为 400）与 `page`
  （1 起、默认 1，空值按未提供，`0`/负数/非整数为 400），属于读路径查询参数，不在本文件的请求体范围内。
  `pageSize` 不是请求参数；Lux 与 Emby 的默认值/上限分开定义（`LUX_SIMILAR_*` 与
  `EMBY_SIMILAR_DEFAULT_LIMIT`/`EMBY_MAX_PAGE_LIMIT`），Emby 不接收 `page`。

## 已知限制

1. **`i64` → `number` 的精度**：JSON 数字在 JS 里是 `f64`，超过 2^53 会丢精度。本项目的 `i64` 目前都是
   时间戳（ms）、`ticks`、计数与自增 ID，量级安全。ts-rs 的默认渲染是 `bigint`，本项目用
   `TS_RS_LARGE_INT=number` 覆盖，理由是：`serde_json` 把 `i64` 编码成 JSON 数字，`bigint` 反而需要前端做一次转换。
   **风险**：若将来出现纳秒时间戳或 64 位雪花 ID，必须重新评估，届时更合适的做法是给该字段加 `#[ts(type = "string")]`。
2. **`serde_json::Value` 无法直接生成类型**：ts-rs 只有在启用 `serde-json-impl` feature 时才为 `Value` 实现 `TS`
   （生成 `JsonValue` 联合类型）。本试点**没有**启用该 feature，所以含 `Value` 字段的结构体在
   `--features ts-bindings` 下会编译失败。这正是当前 (b) 家族的现状（`lux_catalog_item_json` 及其副本都是 `Value`），
   扩展时必须先把 `Value` 拆成具名结构体，而不是打开 `serde-json-impl` 生成 `JsonValue`。
3. **`Option<Option<T>>`**：serde 的 `Option<Option<T>>` 在 JSON 里表现不出三态（缺省 / `null` / 有值），
   `json!` 与结构体都会把它压成 `null`。需要三态时用枚举（如 `#[serde(untagged)]` 的多变体）或独立的
   「字段存在性」标志，不要依赖 `Option<Option<T>>`。
4. **条件字段**：`lux_catalog_page_json_for_user` 只在 `total_included` 时才写 `total`。任何「有时出现」的键
   都必须拆成两个结构体（或两个响应类型），ts-rs 无法表达「条件出现」。
5. **`skip_serializing_if`**：ts-rs 只有在同时有 `#[serde(default)]` 时才把字段生成为可选。引入
   `skip_serializing_if` 会让 JSON 出现「键缺省」，前端读到的是 `undefined` 而不是 `null`；
   改造时要么保持「始终输出 `null`」，要么同步把 TS 字段改成可选并检查所有读点。
6. **字面量字段**：`bool` 会生成为 `boolean`（本试点把 `available: true` 放宽为 `boolean`）。需要保留字面量时
   用单变体枚举（本次的 `DanmakuFormat`）或 `#[ts(type = "...")]`。
7. **自定义 `Serialize` 实现**：ts-rs 只理解派生出来的形状；手写 `impl Serialize` 的类型必须用
   `#[ts(as = "...")]` / `#[ts(type = "...")]` 手工描述，否则生成结果与真实输出不一致。
8. **生成受环境变量影响**：`ts-rs` 在导出时读 `TS_RS_*` 系列变量（`TS_RS_EXPORT_DIR`、`TS_RS_LARGE_INT`、
   `TS_RS_IMPORT_EXTENSION` 等）。`scripts/generate-api-types.sh` 会先清空全部 `TS_RS_*`、再固定需要的两个，
   但直接运行 `cargo test --features ts-bindings export_bindings` 会把文件写到 `<repo>/bindings/`、用 `bigint`，
   并受调用者 shell 影响。因此**只能**通过脚本生成；`scripts/check-api-types.sh` 也用脚本，所以两者一致。
9. **生成的 `.ts` 参与 `tsc`**：`web/src/lib/api/generated/` 位于 `web/tsconfig.json` 的 `include` 之内，
   生成物语法错误会让 `pnpm --dir web build` 失败（这是期望行为）。生成文件带文件头注释，不要手改。
10. **Rustdoc 会进入 JS 文档注释**：`///` 会变成生成文件里的 `/** … */`，因此 DTO 上的实现细节注释用 `//` 而不是 `///`。
11. **范围**：只覆盖 Lux 自有 API 的**响应**。Emby 兼容层 DTO（必须保持 Emby 形状）与请求体类型不在其中。
12. **零运行时影响**：`ts-bindings` 是编译期功能，默认构建与 Dockerfile（`--locked`）都不会引入 `ts-rs`；
    生成的绑定在 `web` 侧只是类型，构建产物中不新增任何运行时代码。
