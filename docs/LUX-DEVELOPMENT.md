# Lux 开发规格与分步实施计划

> 文档状态：待项目所有者审阅  
> 产品名称：Lux  
> 核心服务端语言：Rust  
> 目标部署：x86_64 飞牛 NAS（Debian）上的 Docker  
> 目标客户端：VidHub、SenPlayer、Infuse，以及 Lux 自带 Web 客户端  
> 目标媒体规模：至少 10,000 部电影、50,000 集剧集  
> 文档日期：2026-08-02

---

## 1. 文档用途

本文档既是 Lux 的产品规格，也是架构设计、验收标准和分步开发计划。后续使用 Codex 开发时，应把本文档放入代码仓库的 docs/LUX-DEVELOPMENT.md，并将其视为项目事实来源。

本文档刻意把工程拆成小步骤。每次只执行一个任务，完成测试与验收后再进入下一任务。任何需求变更必须先修改本文档，再修改代码。

### 1.1 Codex 执行原则

每次向 Codex 下达任务时使用以下模板：

~~~text
请先阅读 AGENTS.md、docs/LUX-DEVELOPMENT.md 中的“全局完成标准”以及任务 <任务编号>。
只执行任务 <任务编号>，不要提前实现后续任务。
先检查当前代码和测试，再给出本任务的短计划。
使用测试驱动方式实现；完成后运行任务要求的全部验证命令。
如果发现规格冲突、需要新增核心依赖、需要改变数据库公共模型，先停下并说明，不要自行扩大范围。
最后汇报：改动文件、验收结果、测试结果、剩余风险。
~~~

### 1.2 阶段门

每个阶段结束时必须：

1. 运行该阶段指定的测试、格式检查和静态分析。
2. 更新兼容性矩阵或性能记录。
3. 由项目所有者确认阶段结果。
4. 未通过阶段门时，不进入下一阶段。

### 1.3 当前假设

以下假设已在文档中显式使用：

- “使用 Rust”指 Lux 的核心服务端、索引、兼容 API、调度和文件传输使用 Rust；Web 前端暂按 React + TypeScript 设计，仍需在阶段 0 门确认。
- 三个第三方客户端通过服务器 URL 手动添加 Lux；局域网自动发现不是首版阻塞项。
- 飞牛 NAS 向 Docker 暴露普通 Linux 目录，媒体路径可 bind mount。
- 因为管理员要求回写 NFO 和图片，相关媒体目录将以读写方式挂载。
- SQLite 数据库位于 /config 的本机持久化卷，不位于 SMB/NFS。
- 兼容性只承诺实施时真实测试并记录版本的 VidHub、SenPlayer 和 Infuse；“对标 Emby”不等于实现 Emby 全部端点。
- 首版运行单个 Lux 实例，不做多节点、高可用或共享数据库。
- 弹幕首版只面向支持弹幕接口的第三方客户端；不把 Emby 标准字幕端点当作弹幕协议，也不为其他客户端增加 ASS 或转码兜底。

---

## 2. 产品目标

Lux 是一个从零实现的个人媒体服务端。它负责组织、索引、展示并直放 NAS 中的电影、电视剧和 .strm 媒体，同时提供与 Emby 客户端 API 足够兼容的接口，使 VidHub、SenPlayer 和 Infuse 能像添加 Emby 服务端一样添加 Lux。

Lux 的核心价值不是功能数量，而是：

- 在至少 60,000 个逻辑媒体条目的库中保持快速、稳定、可诊断。
- 文件变动时只增量处理受影响的目录和条目。
- 扫描、刮削、媒体探测不得阻塞浏览、搜索、登录或播放。
- 优先使用本地 NFO 和图片，保护用户已经整理的元数据。
- 以直接播放为主，不承担首版转码系统的复杂度。
- 使用独立、清晰的 Emby 兼容层，不让兼容 DTO 污染 Lux 内部领域模型。

### 2.1 主要用户

- 管理员：完成初始化、创建用户、管理权限、创建媒体库、配置扫描和刮削、纠正元数据匹配、查看任务与健康状态。
- 普通用户：通过 VidHub、SenPlayer、Infuse 或 Lux Web 客户端浏览和播放自己有权访问的媒体库。

### 2.2 成功定义

达到首个正式可用版本时，必须满足：

- 三个目标第三方客户端均可手动添加 Lux、登录、浏览多个媒体库、搜索、查看详情、直放、同步进度和收藏。
- Lux Web 客户端支持登录、继续观看、多个媒体库入口、搜索、筛选、详情、版本选择和浏览器原生直放。
- 10,000 部电影和 50,000 集剧集的测试库中，常用查询达到第 5 节定义的性能目标。
- 实时文件事件只触发局部增量扫描；定时全量校验在后台可暂停、可恢复，不锁住前台。
- 本地 NFO 和图片优先，所选刮削器仅补缺；低置信度匹配进入“待处理”。
- 管理员能重新匹配元数据条目并将结果原子地回写到 NFO 和同目录图片。
- Docker 容器重启后，任务、进度、用户、索引和扫描游标均保持一致。

---

## 3. 已确认的产品需求

### 3.1 媒体库

- 支持电影库、剧集库、混合库。
- 可创建多个逻辑媒体库。
- 管理员可以编辑已有媒体库的名称和类型。
- 单个媒体库可包含多个本地路径。
- 同一媒体库可同时包含真实媒体文件和 .strm 文件。
- `.strm` 默认只作为外部播放地址记录；普通扫描、PlaybackInfo 和播放请求不得主动读取其指向的视频源的容器信息、索引或媒体轨。下载请求按 LUX-091 的远程资源流式转发规则读取字节，不进行媒体信息探测。管理员显式创建 STRM 探测任务后，允许通过受监督的 `media_probe` 插件在后台读取远程媒体信息。
- `.strm` 若存在同名 `-mediainfo.json` 旁车，可在后台读取旁车填充已声明的媒体信息；没有旁车时保持媒体信息为空，不因缺少探测结果阻止播放。
- 每个媒体库可设置一个可选的自定义封面图；仅管理员可以上传或替换，普通用户只能在拥有该媒体库访问权限时读取。
- 媒体库封面图首版只接受 JPEG、PNG、WebP，大小上限为 5 MiB，并通过 Lux 的受保护图片接口提供。
- 每个媒体库默认实时监听文件系统；新增、修改、重命名和删除事件只触发受影响路径的局部增量扫描。实时监听不是可关闭的媒体库开关；旧版 `realtime_watch_enabled` 字段仅作兼容保留并始终按开启处理。全量校验和元数据任务可独立配置计划；局部增量扫描由实时事件触发，不作为管理员可配置的计划任务。
- 每个媒体库可选择一个已安装的元数据刮削器；未选择时仍读取本地 NFO 和图片，但不发起在线刮削请求。
- 管理员可在“全局策略”中设置媒体库的默认元数据、图像和字幕策略；媒体库可以继承全局默认值，也可以单独覆盖。
- 全局图像策略包括海报、艺术图、横幅图、徽标、缩略图、光盘封面、壁纸开关、每项最大背景图数量和最小下载宽度；媒体库可覆盖这些开关。
- 全局策略支持保守的存储预估，并明确应用范围：仅新内容、刷新选中内容或后台刷新全部内容；全局刮削可选择仅补全或完整刮削，批量刷新必须进入任务队列。
- 不在用户请求路径中扫描目录、读取 NFO、调用 ffprobe 或访问 TMDb。

### 3.2 媒体来源

- 本地媒体来自 NAS Docker 绑定挂载目录。
- .strm 文件的第一个非空文本内容被视为外部播放地址。
- .strm 地址在播放路径中直接交给客户端，不校验、不探测、不做代理、不访问 AList API；独立的管理员 STRM 探测任务可以在 URL 安全策略通过后将地址交给 `org.lux.strm-media-info` 插件。
- Lux 不负责保护 .strm URL 中可能包含的令牌；管理员应理解该 URL 会暴露给有播放权限的客户端。

### 3.3 播放

- 首版仅支持直接播放，不支持音视频转码、容器转换或字幕转换。
- 本地文件通过带鉴权的 HTTP GET/HEAD 和单区间 Range 请求传输。
- .strm 返回外部播放地址。
- 浏览器无法原生播放的编码直接显示不支持，不提供转码兜底。
- 暴露本地文件中的内嵌字幕轨以及同目录外挂字幕。
- 外挂字幕至少识别 srt、ass、ssa、vtt、sub、sup/pgs 等常见格式。
- 是否能够渲染某种字幕由客户端能力决定。

### 3.4 多版本

- 同一内容的 1080p、4K、Remux、Web-DL 等媒体源默认聚合为一个逻辑标题。
- 详情页允许用户选择媒体版本。
- 不同媒体源保留独立文件路径、媒体信息、播放地址和可用字幕。
- 已看、进度和收藏绑定逻辑标题，在普通清晰度版本之间共享。
- 导演剪辑版、加长版等内容不同的版本可作为独立逻辑条目。
- 自动聚合必须依赖可靠的 provider ID、显式版本标记或管理员操作，不得仅凭相似标题粗暴合并。

### 3.5 元数据优先级

字段级优先顺序：

1. 管理员手工编辑且锁定的本地字段。
2. 现有 NFO 与本地图片。
3. 已确认的 TMDb 数据。
4. 文件名、目录名和媒体探测得到的技术信息。

具体规则：

- 本地 .nfo 和已有海报、背景图优先。
- 常规自动处理和“仅补全”不覆盖本地已有标题、简介和图片；“完整刮削”只刷新未锁定的 NFO 字段并替换已有图片。
- 锁定的 NFO 字段在任何刮削模式下都不覆盖；在线没有返回的图片不删除本地图片。
- TMDb 插件提供可配置的首选语言，默认使用简体中文 `zh-CN`；可选语言按 `zh-CN`、`zh-SG`、`zh-HK`、`zh-TW`、其他 TMDb 主翻译语言的顺序展示。
- TMDb 语言回退开关默认关闭；开启后，电影、剧集、季度和单集元数据按管理员选择的语言顺序逐字段补全，默认预选 `zh-SG`、`zh-HK`、`zh-TW`。
- TMDb 插件提供默认关闭的替代 API 地址开关；开启后可选择默认官方地址 `https://api.themoviedb.org`、`https://api.tmdb.org` 或自定义 HTTP(S) 基础地址。自定义地址不得包含凭据、查询参数或片段，并持久化到 `/config/tmdb_settings.json`。
- 图片优先本地；在线图片按 zh-CN、无语言、英文的顺序选择。
- 电影、剧集、季度和单集 NFO 均应兼容常见 Emby/Kodi 旁挂形式。
- 至少识别 movie.nfo、tvshow.nfo、与视频同名的 .nfo、poster、fanart/backdrop、seasonXX-poster 等常见命名。
- 写回时使用稳定、公开记录的 Lux NFO 子集，同时尽量保留未知 XML 字段，避免破坏其他软件写入的信息。

### 3.6 元数据匹配和重新匹配

- 有明确 provider ID 时直接确认身份。
- 没有 provider ID 时，可用规范化标题、年份、媒体类型和季集号通过媒体库所选刮削器搜索。
- 匹配结果只保存当前媒体库所选刮削器对应的 provider ID；选择 TMDb 时保存 TMDb ID，选择其他刮削器时保存该刮削器返回的 ID。
- 自动匹配必须达到高置信度阈值；候选接近或信息不足时进入“待处理”。
- “待处理”条目保留原始文件名和可播放能力，不因缺少在线元数据从库中消失。
- 管理员 Web 控制台提供待处理列表。
- 管理员可搜索候选、查看差异、选择正确条目并确认。
- 元数据匹配错误时支持“重新匹配”。
- 重新匹配可选择仅补缺字段或刷新在线字段；无论哪种模式都不覆盖已锁定字段。
- 成功编辑或匹配后，将 NFO 和选中的图片回写到媒体目录。
- 新建媒体库首次添加可用根路径并完成扫描后，若媒体库配置了刮削器，自动按高置信度选择最佳候选，写回元数据并按该媒体库的图像策略下载所需图片；用户无需逐条进入管理后台确认。
- 管理员从媒体库入口手动执行“整库元数据匹配”时，使用与新库首次处理相同的自动选择、NFO 写回和图片下载流程；低置信度或候选接近的条目仍进入待处理队列。
- 回写使用临时文件、刷盘和原子重命名；失败时显示可重试状态，不谎报成功。

建议的首版自动匹配门槛：

- NFO 中存在合法 TMDb ID：确认。
- 规范化标题完全一致、媒体类型一致、年份相差不超过 1 年，且最佳候选明显高于第二候选：可自动确认。
- 其他情况：待处理。

具体分数只属于 Lux 内部实现，不作为 Emby 兼容 API 的公共契约。

### 3.7 弹幕

- 弹幕使用独立的 Lux 弹幕服务和 Emby 兼容弹幕路由，不伪装成普通字幕轨。
- 管理员可以配置一个 Dandanplay 兼容 API 基地址，也可以配置 `huangxd-/danmu_api` 的 API 基地址；地址可包含部署 token 路径。
- 后台匹配任务优先使用上游 `/api/v2/match`，不支持时回退到 Dandanplay 兼容的搜索、详情和弹幕接口。
- 匹配成功的 XML 弹幕写回视频同目录、同 basename 的 `.xml` 旁车；使用临时文件、刷盘和原子重命名。
- 只承诺支持弹幕接口的第三方客户端可以通过 Lux 的 Emby 接口读取；其他客户端是否识别 `.xml` 不属于 Lux 兼容承诺。
- 首版不实现 Web 播放器弹幕、ASS 写回、弹幕转换、实时发送、代理播放或非弹幕客户端适配。

### 3.8 图片

首版必须：

- 海报 poster。
- 背景图 backdrop/fanart。
- 本地图片发现、尺寸读取、缓存标签、HTTP 缓存和缩放接口兼容。
- 缺失时从所选刮削器下载并回写媒体目录；匹配选择时按所属媒体库启用的图片类型逐项取第一张可用图片，缺失类型跳过。

首版不阻塞但数据模型需预留：

- 透明 Logo。
- 横幅 banner。
- 人物图。
- 章节缩略图。

### 3.9 合集

- 支持电影合集。
- 读取 TMDb collection 信息自动建立电影系列。
- 合集是逻辑实体，不移动或复制媒体文件。
- 合集成员仍受媒体库 ACL 约束。
- 自定义合集不是首个可用版本的阻塞项。

### 3.10 用户、会话和权限

- 第一次启动进入初始化引导。
- 第一个完成初始化的账户为管理员。
- 不开放公开注册。
- 后续账户只能由管理员创建和管理。
- 支持大量普通用户。
- 每个用户的进度、已看状态和收藏独立。
- 用户权限至少包括：
  - 允许或拒绝访问指定媒体库。
  - 是否允许外网访问。
  - 是否允许使用下载功能。
  - 是否允许进入管理控制台。
- 内容分级和按标签控制属于后续阶段。
- 管理控制台的权限必须由服务端校验；隐藏前端菜单不等于授权。

### 3.11 首页、浏览和搜索

普通用户首页：

- 继续观看。
- 推荐轮播：服务端基于用户收藏、播放状态、播放活跃度和媒体入库新鲜度，对可访问的已入库电影与剧集进行可解释的加权排序；按用户和 UTC 日期生成每日批次，同一天保持稳定，跨天更换推荐内容；冷启动时优先最近入库内容。
- 用户有权访问的多个媒体库入口。
- 搜索入口。

媒体库浏览首版支持：

- 按媒体类型筛选。
- 按年份筛选。
- 按已看/未看筛选。
- 按收藏筛选。
- 按名称排序。
- 按最近添加排序。
- 按发行日期排序。
- 按评分排序。
- 所有列表分页，禁止无界返回。

后续能力：

- 演员、导演、制作公司等深度浏览。
- 全站排行榜和更复杂的内容相似度推荐。

### 3.12 播放进度

- 每个用户独立保存。
- 进度时间使用 Emby 兼容的 ticks 表示时，1 秒等于 10,000,000 ticks。
- 默认播放达到约 90% 标记为已看。
- 默认不足 2 分钟的进度不进入继续观看。
- 阈值由管理员在全局设置中调整。
- 收到播放开始、进度和停止事件时采用幂等更新。
- 客户端重复、乱序或延迟上报时，不允许进度无理由倒退；显式从头播放除外。

### 3.13 外网访问

- Lux 不实现公网穿透、UPnP 端口映射或自带证书签发。
- 外网通过 Tailscale、反向代理或用户域名接入。
- 网络代理设置是全局出站配置，支持 HTTP、HTTPS、SOCKS4、SOCKS4a、SOCKS5 和 SOCKS5h；可通过代理 URL 携带认证信息。
- 出站代理可使用 Lux 的统一配置或标准 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY`、`NO_PROXY` 环境变量；配置影响 Lux 发出的网络请求，包括 `.strm` 下载的上游请求，但不代理播放时由客户端直连的 `.strm` 地址和入站反向代理。
- 管理员可在网络代理设置中检测 TMDb、百度、Google 和 Cloudflare 的逐站延迟，并查看通过 Cloudflare trace 获取的网络出口 IP 与国家/地区代码。
- 远程访问权限由用户策略控制。
- 只有受信代理来源的 X-Forwarded-For、X-Forwarded-Proto 等请求头可以影响远近端判断。
- 反向代理场景必须使用 HTTPS；用户名和密码登录协议本身不能替代 TLS。

### 3.14 管理与可观测性

管理员控制台至少显示：

- 可编辑的服务器名称、Lux 服务端版本和 schema 版本。
- 每个媒体库的条目数、路径和状态。
- 当前正在播放的会话，包括账户、媒体、海报、播放进度、设备、客户端、媒体来源和音视频轨道摘要。
- 播放会话持久化并展示 Emby 风格的 `Client`、`DeviceName`、`DeviceId`、`DeviceType` 和 `ApplicationVersion`；播放事件显式字段优先，缺失字段从认证头回填。
- `PLAYING` 或 `PAUSED` 会话超过 90 秒没有收到任何播放事件时，服务端按失活会话处理，不再出现在当前播放列表、Emby `GET /Sessions` 或 Web 当前播放状态中；客户端显式上报 `STOPPED` 仍立即生效。
- 最近的账户活动，包括登录、开始播放、暂停和停止播放事件。
- 实时监听状态。
- 当前扫描进度、扫描游标和预计剩余项。
- 最近一次增量扫描与全量校验时间。
- 后台任务队列、运行中任务和失败重试。
- 待处理、元数据匹配失败、NFO 回写失败和图片下载失败数量。
- 服务端版本、运行时间、数据库状态、磁盘可写状态。
- 管理仪表盘显示 Lux 容器自身的 CPU 使用率、内存使用量/限制、`/media` 挂载点空间使用量/可用量；这些指标只能来自容器 cgroup 和容器内 `/media` 文件系统，不得回退为宿主机整体资源。
- 结构化日志查看和下载。

首版不提供内置备份与恢复。配置和数据库通过 Docker 持久化卷由 NAS 自己备份。

### 3.15 内置插件与刮削器

- Lux 提供安全的插件注册表；插件以标准 `.zip` 插件包放入 `/config/plugins`，服务重启时扫描并加载。插件代码运行在受监督的独立进程中，不直接注入 Lux Rust 主进程。
- 首个独立插件为 `org.lux.tmdb`。它提取 Emby `MovieDb.dll` 的 TMDb 行为，按 Lux 插件协议重写，并保留 Emby 风格的媒体类型、ProviderIds、ImageType、搜索结果和图片结果定义。
- SDK v1 同时支持 `media_probe` 插件类型。`org.lux.strm-media-info` 只接收 Lux 宿主按单个任务提交的已校验 URL，调用 `ffprobe` 并返回受限的 format/stream 结果；插件不能访问 Lux 数据库、媒体根目录或任务对象，宿主负责并发、取消、恢复、结果落库和可选旁车写回。
- 只有已安装、已启用且有可用凭据的插件才能被媒体库选择。插件可以声明自己的配置字段；没有配置项的插件不需要展开配置。TMDb 优先使用管理员填写的 API Key，其次使用运行时或历史 Read Access Token，最后使用服务端内置的默认 API Key；任何凭据都不返回 API 或写入日志。
- 媒体库的 `scraperId` 为空表示不进行在线刮削、只使用本地元数据；插件安装状态与媒体库选择均持久化，服务重启后保持不变。
- 插件列表 API 必须分页并设置服务端上限。插件安装和媒体库刮削器选择必须经过管理员鉴权与 CSRF 校验。
- 全局策略的服务器设置不得返回任何凭据；插件凭据仍只在插件管理页面配置。播放进度阈值继续属于服务器设置，不在媒体库策略页重复管理。

---

## 4. 明确不在首版范围

- 音频转码、视频转码、HLS 转码、容器转换。
- 在线字幕搜索、字幕下载、OCR 或字幕格式转换。
- 直播电视、DVR、DLNA、Chromecast 控制。
- 未经插件包格式、路径、manifest、文件哈希、权限声明和独立进程监督的任意外部代码执行。
- Emby Connect、Quick Connect 或官方云账户。
- 公网穿透、自动端口映射和自动证书申请。
- 音乐库、照片库、有声书库和游戏库。
- 完整复刻所有 Emby API。
- 绕过 Emby Premiere、客户端付费或授权机制。
- 使用 Emby 的商标、图标、网页资产或服务端源代码。
- 内置备份恢复。
- 复杂推荐算法。
- 内容分级与标签 ACL。
- 在线转码兜底的 Web 播放。

---

## 5. 非功能需求和性能目标

### 5.1 基准环境

正式性能报告必须记录真实硬件，不允许只写“很快”。初始参考环境：

- x86_64 飞牛 NAS。
- 4 核 CPU 或更高。
- 8 GB 内存。
- 媒体位于 NAS HDD。
- Lux 配置目录和 SQLite 数据库位于本机 SSD 或 NAS 本机文件系统，不放在 SMB/NFS 网络挂载上。
- 测试数据至少 10,000 部电影、50,000 集剧集，包含 NFO、图片、外挂字幕和一部分多版本。

### 5.2 API 服务级目标

在数据库已预热、单页 50 条、扫描任务同时运行的情况下：

| 场景 | 目标 |
|---|---:|
| 登录后首页聚合 | p95 小于 400 ms |
| 单媒体库首屏 | p95 小于 300 ms |
| 标题/别名搜索 | p95 小于 500 ms |
| 单条详情 | p95 小于 200 ms |
| 继续观看 | p95 小于 300 ms |
| 图片命中本地缓存 | p95 小于 150 ms，不含网络传输时间 |
| API 错误率 | 小于 0.1%，不含合法 4xx |
| 扫描期间前台 p95 | 不超过空闲时 2 倍，并保持小于 1 秒 |

这些目标不是用单个开发者电脑的偶然结果验收，必须使用可重复的基准脚本。

### 5.3 扫描目标

- 文件事件经防抖后 10 秒内进入队列。
- 排除 TMDb 网络等待，单个新增电影或剧集目录通常在 60 秒内出现在索引中。
- 未变化文件不得重复运行 ffprobe、解析 NFO 或下载图片。
- 全量校验可暂停、恢复和取消。
- 服务重启后从持久化游标继续。
- 全量校验期间前台 API 读取旧索引，并逐批看到原子更新。
- 临时挂载失效不得立刻删除整个媒体库；先标记根路径不可用并暂停删除判定。

### 5.4 资源目标

- 空闲常驻内存目标小于 300 MB。
- 默认扫描时常驻内存目标小于 750 MB。
- 所有后台队列有界；队列满时合并事件或施加背压，不无限增长。
- ffprobe 默认并发 2，可按媒体库或全局调小。
- TMDb 请求必须经过 `org.lux.tmdb` 插件；插件统一限制最多 16 个并发请求、每秒最多发起 32 次请求，并实现指数退避和抖动。
- SQLite 写事务短小，批次默认 100 至 500 项；禁止把整个库放入单个事务。

### 5.5 可靠性

- 进程异常退出后数据库保持可打开。
- 数据库迁移可重复运行并有版本记录。
- 扫描任务和元数据任务幂等。
- NFO 和图片回写失败不会破坏原文件。
- 单个坏 NFO、损坏媒体或 TMDb 错误只影响对应条目。
- 正常关机等待正在提交的小事务完成，并停止接收新任务。

---

## 6. 技术栈

### 6.1 核心服务端

- Rust stable，仓库提交 rust-toolchain.toml 固定工具链。
- Tokio：异步网络、定时器、进程和有界通道。
- Axum：HTTP 路由、中间件和请求提取。
- Tower / tower-http：追踪、压缩、超时、请求 ID、CORS 和静态文件。
- Serde / serde_json：Lux API 与 Emby 兼容 DTO。
- SQLx + SQLite：异步数据库访问、迁移和编译期查询检查。
- quick-xml：宽容读取和写入 NFO。
- notify：Linux inotify 实时监听；无法可靠监听时回退 PollWatcher 或定时校验。
- reqwest + rustls：TMDb HTTPS 客户端。
- tracing / tracing-subscriber：结构化日志。
- argon2：密码哈希，使用 Argon2id。
- uuid：内部 ID，优先 UUIDv7；Emby DTO 只暴露字符串。
- ffprobe：本地媒体由核心服务用于技术信息、时长、内嵌轨道和章节探测；`.strm` 远程媒体只能由管理员显式创建的后台任务通过受监督的 `media_probe` 插件探测，不得进入用户请求路径。

依赖版本不在本文档写死。项目初始化时选择当前稳定版本并提交 Cargo.lock；升级必须单独执行、单独验证。

### 6.2 Web

核心服务端全部使用 Rust。首版 Web 前端建议使用：

- React + TypeScript。
- Vite。
- TanStack Query 或等价的服务端状态管理。
- React Router。
- 原生 HTML video 元素。
- Playwright 端到端测试。

原因：Web 前端不处于媒体索引和传输性能热路径；TypeScript 浏览器生态对管理后台、可访问性和视频元素支持更成熟。若项目所有者要求“前端也必须 Rust”，需在实施前新增 ADR，评估 Leptos/Yew；不得在开发中途无记录切换。

### 6.3 数据库选择

首版使用单文件 SQLite，开启 WAL、外键、busy_timeout，并在后台执行受控 checkpoint。

选择原因：

- NAS 单机单实例部署。
- 运维成本低，不需要附带 PostgreSQL。
- 60,000 级媒体条目远低于 SQLite 的合理容量。
- WAL 允许读写并行，适合“前台高读、后台短批量写”。

限制：

- 数据库文件必须位于容器本机持久化卷，不得放在 SMB/NFS 上。
- 同一数据库只允许一个 Lux 实例写入。
- SQLite 同时只有一个写者，因此扫描器必须批量、短事务、限制写入竞争。

### 6.4 Docker

- 生产镜像为多阶段构建。
- 运行时包含 luxd、Web 静态资源、ffprobe 和必要 CA 证书。
- 非 root 用户运行。
- 支持 PUID/PGID 或文档化的 UID/GID 映射，使容器能读写媒体目录。
- /config 为可写持久化卷。
- 媒体目录必须按需求以读写方式挂载，因为 Lux 要回写 NFO 和图片。
- 默认容器端口建议 8097，避免与现有 Emby 的 8096 冲突；可通过环境变量修改。

---

## 7. 总体架构

Lux 首版采用模块化单体：一个 Rust 进程、一个 SQLite 数据库、一个 Web 静态前端和多个受控后台 worker。不要在首版拆微服务。

~~~text
VidHub / SenPlayer / Infuse             Browser
              |                           |
              | Emby-compatible API       | Lux /api/v1 + Web
              +-------------+-------------+
                            |
                      Axum HTTP Server
                            |
             +--------------+---------------+
             |                              |
       Emby Compatibility              Lux API / Web
          DTO + Routes                 DTO + Routes
             |                              |
             +--------------+---------------+
                            |
                    Application Services
         auth / catalog / playback / users / metadata
                            |
        +-------------------+--------------------+
        |                   |                    |
   SQLite Storage      Background Jobs       File Streaming
        |           scan / probe / TMDb /         |
        |             image / writeback            |
        +-------------------+--------------------+
                            |
                  NAS paths and .strm files
~~~

### 7.1 模块边界

- api/emby：只处理 Emby 路由、参数、头和 DTO 映射。
- api/lux：供 Web 与管理员使用的版本化 API。
- application：用例编排和权限校验。
- domain：媒体、用户、权限、进度、任务等核心类型与规则。
- storage：SQLx repository、事务和迁移。
- library：目录分类、扫描、指纹、实时事件与调和。
- metadata：NFO、刮削器、合并策略、匹配和写回。
- media：ffprobe、媒体源、字幕、版本分组。
- playback：播放信息、Range、进度和会话。
- jobs：持久任务、调度、重试、取消和资源配额。
- observability：日志、指标、健康检查和管理状态。
- config：环境变量、文件配置和初始化状态。

HTTP handler 不写 SQL，不执行文件扫描，不直接调用 TMDb。handler 只完成协议解析、边界验证、调用 application service 和 DTO 映射。

### 7.2 并发与背压

- HTTP 请求、文件扫描、ffprobe、TMDb、图片下载和 NFO 回写使用不同并发配额。
- 所有通道使用有界容量。
- 同一路径事件以路径为键合并。
- 同一媒体条目同一时刻最多有一个元数据匹配或写回任务。
- 前台读查询使用独立连接池配额。
- 数据库写入通过短事务和必要的写协调器减少 SQLITE_BUSY。
- 任何 CPU 或阻塞文件任务不得长时间占用 Tokio 核心 worker；使用 spawn_blocking 或专用线程池。

---

## 8. 项目结构

建议初始结构：

~~~text
lux/
├── AGENTS.md
├── Cargo.toml
├── Cargo.lock
├── rust-toolchain.toml
├── rustfmt.toml
├── clippy.toml
├── .env.example
├── Dockerfile
├── compose.yaml
├── scripts/
│   └── check-all.sh
├── README.md
├── docs/
│   ├── LUX-DEVELOPMENT.md
│   ├── COMPATIBILITY.md
│   ├── PERFORMANCE.md
│   ├── API.md
│   └── decisions/
│       ├── 001-modular-monolith.md
│       ├── 002-sqlite-wal.md
│       ├── 003-emby-compatibility-boundary.md
│       ├── 004-direct-play-only.md
│       ├── 005-local-metadata-authority.md
│       └── 006-react-web-client.md
├── migrations/
├── src/
│   ├── main.rs
│   ├── lib.rs
│   ├── config/
│   ├── domain/
│   ├── application/
│   ├── storage/
│   ├── api/
│   │   ├── emby/
│   │   └── lux/
│   ├── auth/
│   ├── library/
│   ├── metadata/
│   ├── media/
│   ├── playback/
│   ├── jobs/
│   └── observability/
├── tests/
│   ├── common/
│   ├── fixtures/
│   │   ├── nfo/
│   │   ├── media/
│   │   └── emby-contract/
│   ├── api/
│   ├── integration/
│   └── performance/
├── web/
│   ├── package.json
│   ├── pnpm-lock.yaml
│   ├── src/
│   ├── public/
│   └── tests/
└── tools/
    ├── catalog-fixture/
    └── compatibility-probe/
~~~

初期保持一个 Rust package，依靠模块边界而不是大量 crate 隔离。只有出现明确编译、发布或复用需求时才拆 workspace crate，并用 ADR 记录。

---

## 9. 开发命令

项目初始化后，以下命令必须真实可执行：

~~~bash
# Rust 构建
cargo build --locked

# Rust 全部测试
cargo test --locked --all-targets

# 格式检查
cargo fmt --all -- --check

# 静态分析
cargo clippy --locked --all-targets --all-features -- -D warnings

# 数据库迁移校验
cargo sqlx migrate run

# Web 安装
pnpm --dir web install --frozen-lockfile

# Web 单元测试
pnpm --dir web test

# Web 构建
pnpm --dir web build

# Web 端到端测试
pnpm --dir web exec playwright test

# 本地开发
cargo run --bin luxd
pnpm --dir web dev

# Docker
docker compose build
docker compose up

# 发布前总检查
./scripts/check-all.sh
~~~

scripts/check-all.sh 应只是上述命令的可移植封装，不隐藏错误、不自动修改文件。

---

## 10. 代码风格和工程边界

### 10.1 Rust 风格

- rustfmt 为唯一格式规范。
- clippy 警告视为错误。
- 生产代码禁止随意 unwrap、expect 和 panic。
- 错误在模块边界转换，并保留可诊断 cause。
- 领域 ID 使用新类型，避免 UserId、ItemId、LibraryId 混用。
- 公共函数和兼容 DTO 有文档。
- async 函数中不得直接进行长时间阻塞 I/O。
- SQL 只出现在 storage 模块。
- 文件路径永远使用 Path/PathBuf，不把未验证用户文本直接拼接成路径。

示例：

~~~rust
pub async fn get_item(
    service: &CatalogService,
    actor: &Actor,
    item_id: ItemId,
) -> Result<MediaItem, CatalogError> {
    let item = service.repository().find_item(item_id).await?
        .ok_or(CatalogError::NotFound(item_id))?;

    service.authorizer().ensure_can_view(actor, &item)?;
    Ok(item)
}
~~~

### 10.2 API 风格

- Lux 自有 API 使用 /api/v1。
- Lux API JSON 字段采用 camelCase。
- Lux API 错误统一为：

~~~json
{
  "error": {
    "code": "LIBRARY_PATH_NOT_WRITABLE",
    "message": "媒体目录不可写",
    "requestId": "..."
  }
}
~~~

- Emby 兼容 API 必须遵循 Emby 的路由、字段名、状态码和可观察行为，不强行套用 Lux 错误格式。
- 输入和输出 DTO 分离。
- 所有列表端点分页并设置上限。
- 添加字段优先，删除或改变类型必须写兼容性 ADR。

### 10.3 永远执行

- 修改行为前先写或更新测试。
- 每个任务运行格式、clippy 和相关测试。
- 所有外部输入在边界验证。
- TMDb 响应、NFO XML、ffprobe JSON 均视为不可信数据。
- 兼容性结论必须记录客户端版本、请求和实际结果。
- 数据库结构变化必须使用 migration。

### 10.4 必须先询问

- 增加大型依赖或替换框架。
- 改变数据库核心关系。
- 改变 NFO 写回格式。
- 改变 Emby API 已验证的响应字段或状态码。
- 加入转码、云服务、遥测或外部账户。
- 扩展首版范围。

### 10.5 永远禁止

- 提交密码、用户 TMDb token、真实 .strm URL 或用户数据；项目所有者明确批准的内置第三方 TMDb fallback Key 除外，但该 Key 仍不得由 API 返回或写入日志。
- 在日志中输出访问令牌、Cookie、完整查询令牌或 .strm 地址。
- 为了通过测试删除失败测试或降低断言。
- 在媒体扫描时加载整个库到内存。
- 在 API 请求中同步执行全库扫描。
- 复制 Emby 服务端代码、品牌资产或冒充官方 Emby Server。
- 实现绕过付费客户端或 Emby Premiere 的逻辑。

---

## 11. 核心数据模型

下表是逻辑模型，具体 SQL 在实现任务中确定。所有表包含必要的 created_at、updated_at，并使用 UTC。

### 11.1 身份和权限

#### users

- id
- username_normalized，唯一
- display_name
- password_hash
- is_disabled
- is_admin
- can_manage_server
- can_remote_access
- can_download
- created_at
- last_login_at

#### user_library_access

- user_id
- library_id
- can_view
- 唯一键 user_id + library_id

#### access_tokens

- id
- token_hash，只存哈希
- user_id
- device_id
- client_name
- device_name
- client_version
- created_at
- last_seen_at
- revoked_at

#### user_item_state

- user_id
- item_id，指向逻辑媒体条目
- position_ticks
- is_played
- is_favorite
- play_count
- last_played_at
- version，用于并发更新
- 唯一键 user_id + item_id

### 11.2 媒体库与路径

#### libraries

- id
- name
- kind：MOVIE、SERIES、MIXED
- cover_image_path，可空，指向配置目录下由服务端生成的封面文件名
- cover_image_content_type，可空
- cover_image_size，可空
- cover_image_tag，可空
- is_enabled
- realtime_watch_enabled
- incremental_schedule（兼容保留，始终为空，不参与调度）
- reconciliation_schedule
- metadata_schedule，首版可为空或手动
- scan_concurrency
- probe_concurrency
- last_scan_at

#### library_roots

- id
- library_id
- canonical_path
- display_path
- is_available
- is_writable
- last_checked_at
- unavailable_since
- scan_cursor

同一路径不得重复加入同一媒体库。跨库重复路径必须警告，因为会产生重复条目。

### 11.3 文件和逻辑媒体

#### filesystem_entries

- id
- library_root_id
- relative_path
- entry_kind
- size
- modified_at
- inode，可空且不作为唯一身份
- fingerprint
- last_seen_generation
- is_missing

#### media_items

- id
- library_id
- item_type：MOVIE、SERIES、SEASON、EPISODE、BOX_SET、FOLDER、UNRESOLVED
- parent_id
- series_id
- season_number
- episode_number
- absolute_number，可空
- title
- sort_title
- original_title
- overview
- production_year
- premiere_date
- runtime_ticks
- provider_ids_json
- metadata_provenance_json
- locked_fields_json
- identification_status：LOCAL_CONFIRMED、ONLINE_CONFIRMED、PENDING、FAILED
- added_at
- removed_at，可空

查询热字段必须是独立列，不得只存在 JSON 中。provider ID、别名和人物关系使用关联表或生成列支持索引。

#### media_sources

- id
- item_id
- source_kind：LOCAL_FILE、STRM_URL
- filesystem_entry_id
- edition_name
- quality_label
- container
- size
- bitrate
- duration_ticks
- external_url
- is_default
- probe_status

根据已确认需求，.strm URL 需要返回客户端。首版按明文保存，因为客户端最终必须拿到原 URL；必须保证数据库文件权限和日志脱敏。

#### media_streams

- id
- media_source_id
- stream_index
- stream_type：VIDEO、AUDIO、SUBTITLE
- codec
- language
- title
- is_default
- is_forced
- is_external
- external_path
- width、height、channels 等技术字段

#### danmaku_tracks

- id
- media_source_id
- relative_path：相对媒体库根路径的同名 `.xml` 旁车路径
- format：首版固定 XML
- provider
- provider_anime_id，可空
- provider_episode_id，可空
- fingerprint
- status：READY、MISSING、INVALID、FAILED
- last_checked_at
- created_at
- updated_at

#### danmaku_match_jobs

- id
- library_id
- overwrite
- concurrency
- status：PENDING、RUNNING、COMPLETED、FAILED、CANCELLED
- total_count
- processed_count
- success_count
- skipped_count
- failed_count
- error
- created_at、started_at、finished_at、updated_at

#### danmaku_match_job_items

- id
- job_id
- media_source_id
- status：PENDING、RUNNING、MATCHED、WRITTEN、SKIPPED、FAILED、CANCELLED
- provider_anime_id，可空
- provider_episode_id，可空
- error_code，可空
- error_message，可空且必须脱敏
- attempts
- updated_at

### 11.4 元数据和图片

#### item_aliases

- item_id
- alias
- language
- alias_normalized

#### item_images

- id
- item_id
- image_type
- index
- local_path
- width
- height
- file_size
- content_tag
- source
- language

#### collections / collection_items

- collection item 本身也可作为 media_items 的 BOX_SET。
- collection_items 保存合集与电影关系、排序和来源。
- 自动合集来源记录 TMDb collection ID。

#### metadata_candidates

- item_id
- provider
- provider_id
- candidate_json
- score
- status
- expires_at

### 11.5 任务与状态

#### jobs

- id
- job_type
- library_id，可空
- item_id，可空
- dedupe_key
- state：QUEUED、RUNNING、RETRY_WAIT、SUCCEEDED、FAILED、CANCELLED
- priority
- progress_current
- progress_total
- cursor_json
- attempt
- max_attempts
- next_run_at
- last_error_code
- last_error_summary
- created_at、started_at、finished_at

#### scheduled_task_configs

- owner_type：GLOBAL、LIBRARY
- owner_id
- task_type
- task_name、task_description
- source_type：SYSTEM、PLUGIN
- plugin_id（可空）
- cron_or_interval
- is_enabled
- resource_limit_json

#### job_events

- job_id
- level
- event_code
- message
- details_json，必须脱敏
- created_at

### 11.6 搜索

- SQLite FTS5 索引标题、排序标题、原始标题和别名。
- 中文首版可使用 unicode61 tokenizer；必须通过真实中文片名测试。
- 年份、类型、库、已看和收藏通过关系表/普通索引过滤，不塞进全文字符串。
- 搜索结果先做权限过滤，再返回。
- FTS 索引由数据库事务或可靠 outbox 同步，不能长期漂移。

---

## 12. 扫描与索引设计

### 12.1 两类独立任务

文件索引任务：

- 实时监听触发的局部增量扫描。
- 实时事件只读取和比较受影响文件；不得因为单文件事件遍历整个媒体库。
- 管理员手动扫描指定媒体库或目录。
- 每个库独立频率的全量调和，用于兜底实时事件丢失或索引与文件系统不一致。

在线元数据任务：

- 新条目缺失字段时触发。
- 管理员手动刷新缺失字段。
- 管理员重新匹配元数据条目。
- 与文件全量调和完全分离。

### 12.2 增量事件流程

~~~text
inotify/notify event
  -> 路径规范化
  -> 防抖和同路径合并
  -> 找到最近的媒体边界目录
  -> 建立带 dedupe_key 的持久任务
  -> 比较文件指纹
  -> 只解析变化文件
  -> 小批量事务更新索引
  -> 异步安排 ffprobe / NFO / 图片 / TMDb
~~~

媒体边界示例：

- 电影库：电影目录或单文件。
- 剧集库：剧集目录、季度目录或受影响的单集。
- 混合库：先判断存在 tvshow.nfo 或明确季集命名，再选择边界。

### 12.3 文件指纹

快速指纹至少包含：

- 规范化相对路径。
- 文件大小。
- 修改时间，使用足够精度。
- 可用时包含 inode/device，但不能依赖其稳定性。

对时间戳不可靠的文件系统，可选计算文件头尾小片段哈希。首版不对所有大文件做全文件哈希。

### 12.4 全量调和

全量调和仍需要遍历目录，这是无法消除的 O(n) 操作。Lux 的优化目标是：

- 只做 readdir/stat 和指纹比较。
- 未变化项不做 NFO 解析、ffprobe、TMDb 或图片处理。
- 每批保存扫描 generation 和游标。
- 可暂停和恢复。
- 低优先级运行。
- 根路径不可用时停止删除判定。
- 本轮完整看到根路径后，才将未出现条目标为 missing。
- 可设置宽限期后再从普通视图移除，避免临时磁盘故障清空媒体库。

### 12.5 混合库分类

优先顺序：

1. NFO 根元素和 provider 信息。
2. 目录中的 tvshow.nfo 或季度结构。
3. 明确的 SxxExx、季/集等命名模式。
4. 明确的电影目录和年份模式。
5. 无法确定时建立 UNRESOLVED 条目，进入待处理。

不允许一个不确定的混合库条目被静默误归类。

### 12.6 大库监听限制

Linux inotify 对 watch 数量有限制，且极大目录可能丢事件。因此：

- 启动时检查并记录 fs.inotify.max_user_watches 等限制。
- 监听失败在控制台明确显示。
- 支持 PollWatcher 或定时调和回退。
- 实时监听永远不是删除判断的唯一事实来源。

---

## 13. NFO、元数据与图片流水线

### 13.1 NFO 读取

- 宽容 XML 解析，未知字段不导致整个条目失败。
- 单个字段解析错误进入诊断，不丢弃其他字段。
- 读取并匹配 provider ID、标题、原标题、sort title、年份、日期、简介、类型、标签、流派、评分、季集号、演员等常用字段。
- 首版查询不需要的人物字段也可保留在 canonical metadata 中，以免写回丢失。
- 所有 XML 外部实体禁用，防止 XXE。

### 13.2 元数据字段合并

每个字段独立决策，不使用“一份来源覆盖整个对象”：

~~~text
locked local value
  > existing NFO/local image
  > confirmed scraper localized value
  > filename/probe fallback
~~~

空字符串不应覆盖有效值。TMDb 语言回退按选定语言顺序逐字段补全，而不是整条记录一次性切换语言；回退请求失败时保留首选语言已获得的字段。

### 13.3 刮削器客户端

- TMDb 客户端同时兼容 v3 API Key 和历史 v4 Read Access Token。管理员通过 TMDb 插件详情配置自己的 API Key。
- 服务端内置一个与 Emby 插件兼容的默认 TMDb API Key，因此首次引导不要求配置 TMDb；管理员填写的 API Key 优先于内置值。
- 自定义 API Key 和历史 token 只保存在 /config 中的受限配置或 secrets 文件，不返回普通用户、插件 API 或日志。
- TMDb 插件配置还包括首选语言、语言回退开关和有序回退语言列表；这些非敏感值保存在 `/config/tmdb_settings.json`，可通过管理员插件配置 API 返回，凭据仍不可返回。
- 主进程的元数据匹配、候选搜索、图片候选和合集请求统一通过媒体库所选刮削器协议；主进程不得直接访问第三方元数据 API。
- 插件内部使用统一 HTTP client、超时、16 并发配额、每秒 32 次请求限流、重试和 User-Agent。
- 404、429、5xx、网络超时分类处理。
- 搜索候选短期缓存，详情较长时间缓存。
- 响应 schema 验证后进入领域层。
- 自动匹配和手动重新匹配共用候选模型；候选的 provider ID 和 provider 名称必须与所选刮削器一致。
- 电影和剧集候选同时携带 0-10 的来源评分；确认候选后保存评分及其刮削器来源，Lux Web 目录和详情海报在右上角显示“来源 + 评分”。

### 13.4 NFO 和图片写回

写回必须：

1. 检查目标目录仍在允许的媒体库根路径内。
2. 检查目录可写。
3. 在同目录创建唯一临时文件。
4. 写入并刷盘。
5. 原子重命名替换目标。
6. 更新数据库指纹和任务状态。
7. 失败时保留原文件并记录可重试错误。

图片下载先写临时文件，并验证 MIME、文件签名和合理大小后再替换。

### 13.5 重新匹配

管理员流程：

1. 打开待处理或错误条目。
2. 输入标题、年份或所选刮削器的 provider ID。
3. 查看候选海报、标题、年份和简介。
4. 选择候选。
5. 选择“仅补缺”或“刷新未锁定在线字段”。
6. 预览将发生的字段变化。
7. 确认。
8. 写回 NFO/图片并重新索引该条目。

指定条目的批量重新识别仍使用持久化任务队列：管理员一次提交 1-100 个条目，服务端去重后以 `QUEUED` 创建任务并在后台逐条处理；每条记录 `PENDING/RUNNING/COMPLETED/FAILED`、候选数量和稳定错误代码，任务通过 `GET /api/v1/admin/metadata/reidentify/{jobId}` 查询。该指定条目接口只负责重新搜索并生成 pending 候选，供管理员处理；失败任务可通过 `POST /api/v1/admin/metadata/reidentify/{jobId}` 重新排队失败条目。

媒体库级“整库元数据匹配”使用同一持久化队列，但默认以 `FILL_MISSING` 自动处理：逐条使用所属媒体库的刮削器搜索候选，达到高置信度时自动选择最佳候选，按媒体库图像策略下载图片并原子写回 NFO/图片；低置信度条目只保留候选并进入待处理状态。新建媒体库首次扫描完成后也自动提交该队列。

全局元数据刷新使用同一持久化队列，模式为 `FILL_MISSING` 或 `FULL_REFRESH`。仅补全只写入缺失的未锁定 NFO 字段和图片；完整刮削刷新未锁定 NFO 字段并替换已有图片，但不覆盖锁定字段。未配置刮削器的条目跳过在线请求并保留本地结果。

管理员也可以从首页或媒体库入口对整个媒体库发起批量元数据匹配或元数据刷新；服务端为一次操作创建一个持久化任务并立即返回。任务内部最多 16 路异步 worker 并行处理条目，条目状态、失败重试和短事务仍逐条记录，前端不得等待匹配完成。

---

## 14. 播放与文件传输

### 14.1 本地文件直放

- 支持 GET、HEAD。
- 支持单 Range 请求和正确的 200、206、416。
- 返回 Accept-Ranges、Content-Length、Content-Range、Content-Type、ETag、Last-Modified。
- 令牌可通过 X-Emby-Token 或兼容 query 参数传入。
- 流式读取使用固定上限缓冲，不将文件装入内存。
- 客户端断开时及时取消读取。
- 不在日志中记录含令牌的完整 URL。
- 路径必须由数据库中的 source ID 解析，客户端不能提交任意磁盘路径。
- `.strm` 下载读取首个非空 URL，使用上游 GET/HEAD 和单 Range 流式转发；不转发入站 Authorization/Cookie，不自动跟随重定向。
- `.strm` 下载的 URL 仅允许 HTTP/HTTPS，拒绝凭据、fragment、localhost、元数据主机以及 DNS 解析到私网或保留地址的主机；连接和读取必须有超时。

多 Range 可在实际客户端证明确有需要时加入；不要首版预先实现复杂 multipart/byteranges。

### 14.2 .strm

- 读取文件的首个非空行并 trim BOM 与首尾空白。
- 播放路径不执行 URL 合法性检查或 HEAD 请求；下载路径按 LUX-091 使用独立的 URL 安全策略和上游流式转发，二者语义分离。
- PlaybackInfo 将其作为直接媒体源返回。
- 播放不通过 Lux 转发数据；下载不会把 `.strm` 文本直接交给客户端。

### 14.3 PlaybackInfo

只声明实际能力：

- SupportsDirectPlay = true。
- SupportsDirectStream/Transcoding 按首版实际实现返回 false。
- MediaSources 包含版本、容器、码率、大小、时长、流列表和直放 URL。
- `.strm` 的容器、时长和流列表可来自受限旁车或已完成的后台 STRM 探测；PlaybackInfo 请求本身不主动读取外部源，首次播放仍由客户端直接访问外部地址。
- 不伪造客户端能播放的编码。
- 选择默认版本使用稳定策略，并允许客户端显式选择 source ID。

### 14.4 字幕

- ffprobe 索引内嵌字幕。
- 扫描同目录外挂字幕并识别语言、forced、default 等文件名标记。
- API 列出内嵌和外挂字幕。
- 外挂字幕可由受鉴权端点直接读取。
- 内嵌字幕由客户端从媒体容器读取；若某目标客户端强制请求提取端点，再以兼容性探针决定是否用 ffmpeg 做无转换抽取。

### 14.5 弹幕兼容

- Lux 提供独立的 `/api/danmu/{itemId}` 和 `/api/danmu/{itemId}/raw` 读取端点，使用 Emby token 和媒体库 ACL。
- XML 来自已登记、已通过媒体根路径约束的同名旁车；请求不执行上游搜索、整库扫描或 XML 写回。
- `option=Refresh` 只刷新已登记旁车的索引；`option=GetJsonById` 作为已知 Emby 弹幕插件兼容别名，不承诺把 XML 转成通用 JSON。
- 支持弹幕接口的客户端以真实兼容性测试为准；不支持弹幕接口的客户端继续按自身能力处理或忽略该 XML。

### 14.6 Web 播放

- 使用原生 video 元素。
- 先根据容器/编码做能力提示，但最终以浏览器实际播放事件为准。
- 不支持时展示清晰错误和推荐使用第三方客户端。
- 记录开始、定时进度、暂停和停止。
- 页面关闭使用可靠的轻量上报机制。
- 不实现 DRM、转码和自定义解码器。

### 14.7 下载权限的限制

can_download 控制下载按钮和下载端点，但任何获准直放本地文件的用户理论上都能保存收到的字节。因此它是产品权限，不是 DRM 安全边界。文档和 UI 不得做虚假承诺。

---

## 15. Emby 兼容层

### 15.1 原则

- 兼容层采用 clean-room 的协议重实现方式。
- 只依据公开 API 文档、自己控制的 Emby 实例响应和目标客户端实际请求。
- 不复制 Emby 服务端源代码或品牌资源。
- Lux 对外品牌始终是 Lux；兼容字段中的版本号和产品名通过实际客户端测试确定，不能用来冒充官方产品。
- 同时接受带 /emby 前缀和不带前缀的常用 API 路径。
- HTTP header 名大小写不敏感。
- Emby DTO 与 Lux 领域模型完全分离。
- 未实现端点返回可诊断结果并记录客户端、版本、路径和脱敏参数。

### 15.2 兼容性验证方法

为每个目标客户端维护：

- 客户端名称、版本、平台版本和设备。
- 添加服务器结果。
- 登录结果。
- 首页请求序列。
- 浏览、搜索、详情、播放、进度、收藏、版本选择结果。
- 实际调用端点和所需响应字段。
- 已知差异与临时兼容行为。

COMPATIBILITY.md 是唯一兼容性事实来源。不能因为实现了官方 Swagger 中的端点就宣称客户端兼容。

### 15.3 首版端点优先级

#### P0：连接与登录

- GET /System/Info/Public
- GET/POST /System/Ping
- GET /System/Info
- GET /Users/Public
- POST /Users/AuthenticateByName
- POST /Sessions/Logout

#### P1：首页、库和详情

- GET /Users/{UserId}/Views
- GET /Users/{UserId}/Items
- GET /Users/{UserId}/Items/{Id}
- GET /Users/{UserId}/Items/Latest
- GET /Users/{UserId}/Items/Resume
- GET /Items
- GET /Items/Filters2，若目标客户端实际调用
- GET /Shows/{Id}/Seasons
- GET /Shows/{Id}/Episodes
- GET /Shows/NextUp
- GET /Search/Hints
- GET/HEAD /Items/{Id}/Images/{Type}
- GET/HEAD /Items/{Id}/Images/{Type}/{Index}
- GET/POST /Items/{Id}/PlaybackInfo

#### P1：播放、状态和收藏

- GET/HEAD /Videos/{Id}/stream
- GET/HEAD /Videos/{Id}/stream.{Container}
- GET /Items/{Id}/Download
- GET /Videos/{Id}/{MediaSourceId}/Subtitles/{Index}/Stream.{Format}
- POST /Sessions/Playing
- POST /Sessions/Playing/Progress
- POST /Sessions/Playing/Stopped
- POST/DELETE /Users/{UserId}/PlayedItems/{Id}
- POST/DELETE /Users/{UserId}/FavoriteItems/{Id}
- GET/POST /Sessions/Capabilities，按客户端请求实现

#### P2：体验完善

- DisplayPreferences 相关端点。
- Years、Genres、Tags 等筛选辅助端点。
- Collections 与合集成员。
- 多版本选择所需的 AlternateSources 等端点。
- Sessions WebSocket 或实时消息，仅在目标客户端确有依赖时实现。
- 图片变体、尺寸和索引端点。

#### 明确不实现

- LiveTv、Sync、Dlna、Packages、Plugins、Encoding、Connect 等首版无关端点。

### 15.4 必须正确的 Emby 查询语义

- UserId、ParentId、Ids。
- IncludeItemTypes、ExcludeItemTypes。
- Recursive。
- StartIndex、Limit。
- SortBy、SortOrder。
- Filters、IsPlayed、IsFavorite。
- Years。
- Fields。
- EnableImages、ImageTypeLimit。
- TotalRecordCount 与 Items 的一致性。

Limit 默认 50，服务端硬上限 500。客户端请求更大时按兼容性策略截断或拒绝，并记录测试。

### 15.5 核心 DTO

BaseItemDto 至少按场景提供：

- Id、ServerId、Name、SortName、OriginalTitle。
- Type、MediaType、IsFolder、ParentId、SeriesId、SeasonId。
- IndexNumber、ParentIndexNumber。
- Overview、ProductionYear、PremiereDate、RunTimeTicks。
- ProviderIds。
- ImageTags、BackdropImageTags。
- UserData：Played、PlaybackPositionTicks、IsFavorite、PlayCount。
- MediaSources、MediaStreams。

字段是否必填以实际目标客户端契约测试为准。不要返回内部数据库路径，除非特定兼容行为明确且经过安全评审。

### 15.6 鉴权兼容

- 接受 Emby Authorization header 中的 Client、Device、DeviceId、Version 和 UserId。
- 登录成功返回 AccessToken 和 User。
- 后续接受 X-Emby-Token。
- 为兼容媒体 URL，可接受 api_key 查询参数。
- 令牌为高熵随机值，数据库仅保存哈希。
- logout 撤销当前设备令牌。
- 401 表示令牌缺失、无效或撤销；403 表示用户已认证但无权限。

---

## 16. Lux 自有 API

Web 和管理控制台使用 /api/v1，不直接依赖 Emby DTO。

### 16.1 初始化和认证

- GET /api/v1/setup/status
- POST /api/v1/setup/complete
- POST /api/v1/auth/login
- POST /api/v1/auth/logout
- GET /api/v1/auth/me

Web 使用 HttpOnly、Secure（HTTPS 下）、SameSite Cookie。改变状态的 Cookie 请求需要 CSRF 防护。初始化完成后 setup/complete 永久关闭，除非管理员通过本地恢复流程重置。

### 16.2 媒体目录

- GET /api/v1/home
- GET /api/v1/libraries
- GET /api/v1/libraries/{id}/items
- GET /api/v1/items/{id}
- GET /api/v1/search
- GET /api/v1/items/{id}/playback
- POST /api/v1/items/{id}/progress
- PUT /api/v1/items/{id}/favorite

Lux 自有列表优先使用游标分页。游标包含稳定排序键和 ID，并进行签名或不可伪造编码。

### 16.3 管理

- GET/POST/PATCH/DELETE /api/v1/admin/libraries
- POST/DELETE /api/v1/admin/libraries/{id}/roots
- POST /api/v1/admin/libraries/{id}/scan
- POST /api/v1/admin/libraries/{id}/reconcile
- GET /api/v1/admin/jobs
- POST /api/v1/admin/jobs/{id}/cancel
- POST /api/v1/admin/jobs/{id}/retry
- GET/POST/PATCH/DELETE /api/v1/admin/users
- PATCH /api/v1/admin/users/{id}/policy
- GET /api/v1/admin/metadata/pending
- GET /api/v1/admin/items/{id}/identify/candidates
- POST /api/v1/admin/items/{id}/identify/candidates
- POST /api/v1/admin/items/{id}/identify/candidates/{candidateId}/select
- POST /api/v1/admin/metadata/reidentify
- GET /api/v1/admin/metadata/reidentify/{jobId}
- POST /api/v1/admin/metadata/reidentify/{jobId}
- POST /api/v1/admin/libraries/{libraryId}/metadata/refresh
- POST /api/v1/admin/libraries/{libraryId}/danmaku/match
- GET /api/v1/admin/danmaku/match-jobs
- GET /api/v1/admin/danmaku/match-jobs/{jobId}
- POST /api/v1/admin/danmaku/match-jobs/{jobId}/cancel
- POST /api/v1/admin/danmaku/match-jobs/{jobId}/retry
- PATCH /api/v1/admin/items/{id}/metadata
- POST /api/v1/admin/items/{id}/metadata/refresh
- DELETE /api/v1/admin/items/{id}
- GET/PATCH /api/v1/admin/settings
- GET /api/v1/admin/health
- GET /api/v1/admin/logs

`GET/PATCH /api/v1/admin/settings` 的 `danmaku` 配置只返回脱敏的地址和配置状态；地址中的 token、query secret 和完整外部 URL 不进入日志、审计事件或普通用户 API。

所有管理端点均在服务端检查 can_manage_server。敏感操作写审计事件。删除媒体源时，即使媒体文件已被外部删除，也会清理 Lux 中的媒体源记录；没有其他媒体源时同时标记逻辑条目移除。

---

## 17. Web 产品界面

### 17.1 初始化向导

1. 欢迎和语言。
2. 创建首个管理员用户名与密码。
3. 创建第一个媒体库，可跳过。
4. 显示 Docker 目录可读写检查。
5. 完成并进入登录页。

初始化未完成时只开放健康检查、静态资源和 setup API。部署指南要求在暴露到公网前完成初始化。

### 17.2 普通用户页面

- 登录。
- 首页：继续观看、媒体库入口、搜索。
- 媒体库列表：类型、年份、已看、收藏筛选；名称、最近添加、发行日期、评分排序。
- 搜索结果。
- 电影详情：海报、背景、简介、年份、时长、版本、字幕信息、播放、收藏。
- 电影和剧集详情显示所选刮削器匹配得到的主要演员；演员资料和头像缓存到 `/config/people`，无头像时显示姓名首字母占位。
- 剧集详情：季度、单集、下一集、进度。
- 合集详情。
- Web 播放页。
- 账户和当前设备会话。

### 17.3 管理页面

- 仪表盘。
- 媒体库列表和编辑。
- 全局策略：元数据、图像和字幕默认值，刮削模式，以及应用范围和存储预估。
- 路径选择/输入、读写检测。
- 扫描计划与元数据计划，明确分开。
- 扫描/任务页。
- 任务与日志页集中查看所有已注册的任务、运行记录和脱敏日志。任务不能由 Web 管理员凭空创建，只能由 Lux 系统或插件注册；管理员只能维护已注册任务的计划、启停和资源配置。
- 空库初始没有注册任务。创建媒体库时由系统原子注册“全量校验媒体库”和“元数据刮削”两个任务；任务默认未启用，管理员在任务页配置计划后才会运行。实时增量扫描由文件系统监听触发，不注册为计划任务。
- 待处理匹配页。
- 元数据编辑与锁定。
- 图片管理。
- 用户与权限。
- 服务端设置。
- 日志与健康。

普通用户访问管理 URL 时，服务端返回 403；前端同时隐藏入口。

### 17.4 可访问性和响应式

- 键盘可操作。
- 表单有 label 和错误关联。
- 图片有替代文本。
- 焦点状态清晰。
- 支持桌面、平板和手机。
- 大列表使用分页或虚拟滚动，不一次渲染数千节点。

---

## 18. 调度、日志与健康

### 18.1 任务类型

- INCREMENTAL_SCAN（内部实时事件任务，不注册为计划任务）
- RECONCILE_LIBRARY（扫描 job 类型；注册计划使用 `RECONCILIATION_SCAN`）
- PROBE_MEDIA
- PARSE_NFO
- DISCOVER_IMAGES
- FETCH_TMDB
- WRITE_NFO
- DOWNLOAD_IMAGE
- MATCH_DANMAKU
- WRITE_DANMAKU_XML
- REBUILD_SEARCH
- PURGE_MISSING

任务使用 dedupe_key，例如 library_id + normalized_path + job_type。重复事件合并。

### 18.2 重试

- 本地确定性错误，如 XML 格式错误：不无限重试，进入失败并等待文件变化或人工操作。
- 临时 I/O、TMDb 429/5xx：指数退避加随机抖动。
- 权限错误：立即失败并在控制台突出显示。
- 最多尝试次数按任务类型配置。

### 18.3 日志

- JSON 结构化日志为默认容器输出。
- 字段包含 timestamp、level、target、requestId、jobId、libraryId、itemId、errorCode、durationMs。
- 不记录密码、token、Cookie、完整外部 URL。
- 路径在管理员日志中可显示相对路径；对普通用户不显示磁盘路径。
- 登录失败以适合 Fail2Ban 或其他日志工具解析的稳定事件码记录。

### 18.4 健康

- /health/live：进程事件循环可响应。
- /health/ready：数据库迁移完成、配置可读、必要目录可访问。
- 管理健康页额外检查 SQLite WAL、任务延迟、根路径状态、ffprobe 可用性和 TMDb 配置。

---

## 19. 安全设计

- 密码使用 Argon2id，参数在真实 NAS 上基准后设置，并在哈希中保存参数。
- 登录、令牌和媒体端点有速率限制，但媒体字节传输不使用会显著拖慢直放的全局小限额。
- 访问令牌至少 256 bit 随机熵，只显示原值一次。
- 数据库仅保存 token 哈希。
- Web Cookie 和 Emby token 分离管理。
- 所有对象访问都执行用户与媒体库 ACL 检查，防止修改 ID 越权。
- 下载、图片、字幕、媒体流端点同样执行 ACL。
- 反向代理头只信任配置的代理网段。
- 路径解析后必须 canonicalize 并验证位于媒体库根内。
- 防止符号链接逃逸；策略需记录并测试。
- NFO 禁止外部实体。
- 图片验证大小和类型，防止超大文件或伪装内容。
- 管理编辑输出在 Web 中转义，防止 NFO/TMDb 文本造成 XSS。
- CORS 默认同源；第三方客户端不依赖浏览器 CORS。
- Docker 非 root，默认只暴露一个 HTTP 端口。
- 外部远程使用必须由 Tailscale 或 HTTPS 反向代理保护。

---

## 20. 测试策略

### 20.1 单元测试

重点模块：

- 文件和目录命名分类。
- 混合库判断。
- 文件指纹和事件合并。
- NFO 解析、字段合并、锁定与写回。
- 刮削器候选评分。
- 版本聚合。
- ACL 和远程访问判断。
- Range 解析。
- 进度阈值和乱序上报。
- Emby DTO 映射。

### 20.2 集成测试

- 每个测试使用临时 SQLite 数据库和临时媒体目录。
- migration 从空库运行。
- 创建库、扫描 fixture、查询、播放和写回完整路径。
- 模拟根路径临时不可用。
- 模拟 NFO 损坏、图片损坏、ffprobe 失败和 TMDb 超时。
- 验证服务重启后任务恢复。

### 20.3 协议契约测试

- 从自己控制的 Emby 测试实例获取脱敏响应样本。
- 只保存结构和非敏感 fixture。
- 对 P0/P1 端点做 golden/shape 测试。
- JSON 字段顺序不作为契约；字段存在、类型、值语义和状态码是契约。
- 每个目标客户端至少保留一组实际请求序列回归测试。

### 20.4 Web 测试

- 组件/逻辑单元测试。
- Playwright：初始化、管理员登录、创建用户、创建媒体库、普通用户首页、搜索、详情、播放错误提示。
- 测试普通用户无法访问管理 API 和页面。
- 测试大列表分页与筛选。

### 20.5 性能测试

提供可重复生成器：

- 10,000 部电影。
- 1,000 部剧集、50,000 集或等价规模。
- 多版本、NFO、图片、字幕、待处理和 .strm 的混合比例。

基准包括：

- 首页、库列表、搜索、详情、继续观看。
- 50 并发短 API 请求。
- 扫描同时运行。
- 4 个本地文件 Range 直放连接。
- 任务恢复和数据库 checkpoint。

每次性能优化都记录硬件、数据集、命令、提交和前后结果到 docs/PERFORMANCE.md。

### 20.6 覆盖率

- 核心领域规则目标行覆盖率不低于 80%。
- ACL、路径安全、NFO 合并、进度和 Range 必须覆盖成功与失败分支。
- 不能为了覆盖率写无断言测试。

---

## 21. Docker 与运维

建议 compose 基线：

~~~yaml
services:
  lux:
    image: lux:local
    container_name: lux
    ports:
      - "8097:8097"
    environment:
      LUX_HTTP_ADDR: "0.0.0.0:8097"
      LUX_CONFIG_DIR: "/config"
      RUST_LOG: "lux=info,tower_http=info"
      TZ: "Asia/Shanghai"
    volumes:
      - ./lux-config:/config
      - /vol1/movies:/media/movies:rw
      - /vol2/tv:/media/tv:rw
    restart: unless-stopped
~~~

要求：

- /config 与媒体路径分开。
- SQLite 文件位于 /config。
- 启动时验证 /config 可写。
- 运行数据库迁移后才 ready。
- 收到 SIGTERM 时优雅退出。
- 提供 amd64 镜像。
- 镜像版本不可只使用 latest；发布使用语义化版本和 immutable digest。

反向代理必须转发 Range、Content-Length、Content-Range，并关闭会破坏视频流的响应缓冲。部署文档分别给出 Tailscale 和常见反向代理的示例，但 Lux 自身不管理它们。

---

## 22. Emby 数据迁移

迁移是后续增强，不阻塞首版。

优先迁移：

- 一个或多个用户的已看状态。
- 播放位置。
- 收藏。

策略：

- 不直接读取或修改 Emby 内部数据库。
- 通过用户拥有权限的 Emby API 导出。
- 使用 TMDb ID、其他 provider ID，其次规范化标题+年份映射 Lux item。
- 不能唯一匹配的记录输出报告，不自动猜测。
- 管理员显式映射 Emby 用户到 Lux 用户。
- 导入幂等，可 dry-run。

本地 NFO 和图片通过扫描自然继承，不需要迁移工具复制。

---

## 23. 架构决策记录

项目初始化时把以下决定分别写入 docs/decisions。

### ADR-001：模块化单体

- 状态：建议接受。
- 决定：首版单进程、单数据库，通过 Rust 模块隔离。
- 原因：NAS 部署简单、事务清晰、Codex 分步开发更容易。
- 否决：微服务会增加部署、网络和一致性成本。

### ADR-002：SQLite WAL

- 状态：建议接受。
- 决定：首版 SQLite WAL，数据库必须位于本机卷。
- 原因：单机、高读低并发写、低运维。
- 风险：单写者；通过短事务、批量和写入配额缓解。

### ADR-003：独立 Emby 兼容边界

- 状态：必须接受。
- 决定：Emby 路由/DTO 与 Lux API/领域模型分离。
- 原因：兼容怪癖不能反向污染核心设计。

### ADR-004：首版只直放

- 状态：已由需求确认。
- 决定：不实现转码或 remux。
- 后果：部分浏览器文件无法播放，明确提示。

### ADR-005：本地元数据为默认来源

- 状态：已由需求确认。
- 决定：本地 NFO/图片始终读取；默认和“仅补全”只补缺失内容，显式“完整刮削”才刷新未锁定 NFO 字段并替换图片；锁定的 NFO 字段始终保留。
- 后果：媒体目录必须读写，写回可靠性成为核心功能。

### ADR-006：React Web 客户端

- 状态：待项目所有者确认。
- 决定：核心服务端 Rust；Web 使用 React/TypeScript。
- 原因：浏览器生态与开发效率。
- 替代：Leptos/Yew，全 Rust 但前端生态和调试成本更高。

---

## 24. 全局完成标准

任何任务只有同时满足以下条件才算完成：

- 规格对应的验收条件全部满足。
- 新行为有自动化测试。
- cargo fmt 检查通过。
- cargo clippy 零警告。
- 相关 Rust 测试通过。
- 涉及 Web 时，Web 单测和构建通过。
- 涉及用户流程时，相关 Playwright 测试通过。
- 没有新增未说明的 TODO、panic、unwrap、secret 或敏感日志。
- 数据库变化包含可从空库运行的 migration。
- 公共接口或架构变化更新文档。
- 兼容性行为在 COMPATIBILITY.md 记录。
- 本任务没有顺手实现后续阶段功能。

---

## 25. 分步实施计划

下面按依赖顺序实施。每个任务应控制在一次 Codex 专注会话内，通常修改不超过 5 个文件；超过时先拆分。

所有未单独标注的任务预计为 M：约 3 至 5 个文件。纯文档/配置任务通常为 S：1 至 2 个文件。开始任务前，Codex 必须根据当前仓库列出精确的“预计修改文件”，若超过 5 个则先把任务再拆小。各阶段的主要文件预算如下：

| 任务范围 | 主要文件或目录 |
|---|---|
| LUX-000 至 003 | Cargo.toml、README.md、AGENTS.md、docs/、scripts/ |
| LUX-010 至 013 | src/main.rs、src/config/、src/observability/、src/api/lux/、migrations/ |
| LUX-020 至 025 | src/auth/、src/api/emby/、src/api/lux/、src/storage/、tests/api/ |
| LUX-030 至 036 | src/domain/、src/library/、src/media/、src/api/、tests/fixtures/ |
| LUX-040 至 045 | src/library/、src/jobs/、src/storage/、tools/catalog-fixture/、tests/performance/ |
| LUX-050 至 056 | src/metadata/、src/jobs/、src/api/lux/、tests/fixtures/nfo/、tests/integration/ |
| LUX-057 | src/application/media_matching.rs、src/application/scanner.rs、src/application/candidates.rs、src/application/reidentify.rs、src/bin/lux-plugin-tmdb.rs、tests/ |
| LUX-060 至 064 | src/domain/、src/library/、src/metadata/、src/api/emby/、tests/fixtures/ |
| LUX-070 至 075 | src/playback/、src/api/emby/、src/application/、tests/api/、tests/integration/ |
| LUX-080 至 084 | src/storage/、src/application/、src/api/、migrations/、tests/performance/ |
| LUX-090 至 094 | src/auth/、src/application/、src/api/、src/storage/、tests/api/ |
| LUX-100 至 106 | web/src/、web/tests/、src/api/lux/；每个页面任务只改对应 feature 目录 |
| LUX-110 至 114 | web/src/features/、web/src/routes/、web/tests/；按单一用户流程切片 |
| LUX-120 至 123 | src/api/emby/、tests/fixtures/emby-contract/、tests/api/、docs/COMPATIBILITY.md |
| LUX-130 至 136 | migrations/、tests/performance/、Dockerfile、compose.yaml、docs/ |
| LUX-140 | src/application/plugins.rs、src/storage/、src/api/、migrations/、web/src/features/admin/、tests/ |
| LUX-142 | src/application/plugin_runtime.rs、src/application/plugin_protocol.rs、src/storage/、src/api/、migrations/、plugins/、docs/、tests/ |
| LUX-144 | src/application/settings.rs、src/application/plugin_protocol.rs、src/application/plugins.rs、src/api/mod.rs、src/bin/lux-plugin-tmdb.rs、web/src/features/admin/、web/src/lib/api/、tests/、docs/ |
| LUX-145 | src/application/thumbnails.rs、src/application/scanner.rs、src/storage/、src/api/mod.rs、tests/thumbnails.rs、docs/ |
| LUX-146 | src/application/plugin_protocol.rs、src/application/plugin_runtime.rs、src/application/plugins.rs、src/application/strm_probe.rs、src/application/strm_probe_policy.rs、src/application/probe.rs、src/storage/、src/api/mod.rs、src/bin/lux-plugin-strm-media-info.rs、src/bin/lux-plugin-pack.rs、migrations/、scripts/、tests/、docs/ |
| LUX-150 | src/application/danmaku.rs、src/storage/、src/api/mod.rs、migrations/、tests/、docs/ |
| LUX-151 | src/application/ip_location.rs、src/api/mod.rs、tests/、web/src/features/admin/、web/src/lib/api/、docs/ |
| LUX-153 | src/application/admin_events.rs、src/api/mod.rs、tests/admin_events.rs、web/src/features/admin/、web/tests/、docs/ |
| LUX-154 | src/application/scanner.rs、src/storage/mod.rs、migrations/、tests/scanning_jobs.rs、docs/LUX-DEVELOPMENT.md |

### 阶段 0：仓库和工程纪律

#### LUX-000：创建仓库骨架

描述：初始化 Rust package、Web 目录、docs、migrations、tests 和基础 README。

验收：

- cargo build 可运行空服务。
- README 列出开发命令和目录。
- 本文档复制到 docs/LUX-DEVELOPMENT.md。

验证：

- cargo build
- rg --files 检查结构

依赖：无。

#### LUX-001：建立 AGENTS.md

描述：把第 10 节边界、任务单步原则、测试命令和文档事实来源写入 AGENTS.md。

验收：

- 后续 Codex 会话只读 AGENTS.md 即可知道规则和验证命令。
- 明确禁止未经批准扩大范围。

验证：人工审阅。

依赖：LUX-000。

#### LUX-002：配置格式、clippy 和统一检查脚本

验收：

- cargo fmt --check、clippy、test 一键执行。
- 脚本错误时非零退出。
- 不自动修改源码。

验证：故意制造格式错误确认脚本失败，再恢复。

依赖：LUX-000。

#### LUX-003：建立 ADR 与兼容性文档

验收：

- 创建第 23 节的 6 个 ADR。
- COMPATIBILITY.md 有目标客户端矩阵模板。
- PERFORMANCE.md 有基准记录模板。

验证：人工审阅链接和状态。

依赖：LUX-000。

阶段门：

- 全部检查命令通过。
- 项目所有者确认 ADR-006，或用新 ADR 选择 Rust Web 框架。

### 阶段 1：服务骨架、配置和数据库

#### LUX-010：Axum 健康服务纵切片

描述：实现配置加载、Axum 启动、request ID、JSON 日志和 /health/live。

验收：

- 地址由环境变量配置。
- 每个请求有 requestId。
- SIGTERM 可优雅退出。

验证：

- 集成测试请求 /health/live 返回 200。
- 启动进程后发送 SIGTERM，进程正常退出。

依赖：阶段 0。

#### LUX-011：SQLite 连接和迁移框架

验收：

- 数据库路径位于 /config。
- 启动设置 foreign_keys、WAL、busy_timeout。
- migration 版本可查询。
- 数据库不可写时 ready 失败并给出明确错误。

验证：

- 空目录启动自动迁移。
- 只读目录集成测试。

依赖：LUX-010。

#### LUX-012：核心 ID、时间和错误类型

验收：

- UserId、ItemId、LibraryId、SourceId、JobId 不可混用。
- UTC 时间和 ticks 转换有边界测试。
- Lux API 错误包含稳定 error code。

验证：单元测试。

依赖：LUX-011。

#### LUX-013：就绪和版本信息

验收：

- /health/ready 检查迁移和配置。
- /api/v1/version 返回 Lux 版本、提交标识和 schema 版本。
- 不泄露文件系统敏感信息。

验证：集成测试。

依赖：LUX-011。

阶段门：

- 新容器从空 /config 启动。
- live/ready 行为正确。
- SQLite WAL 文件出现在本机卷并能正常 checkpoint。

### 阶段 2：初始化、认证和首个客户端连接

#### LUX-020：用户表和 Argon2id 密码服务

验收：

- 用户名规范化唯一。
- 密码只保存 Argon2id 哈希。
- 错误密码验证时间不产生明显用户枚举差异。

验证：单元和数据库集成测试。

依赖：阶段 1。

#### LUX-021：初始化状态 API

验收：

- 无用户时 setup/status 显示未完成。
- setup/complete 原子创建首个管理员。
- 初始化后重复调用永久拒绝。

验证：并发两次初始化只有一次成功。

依赖：LUX-020。

#### LUX-022：Lux Web 会话

验收：

- 登录创建 HttpOnly Cookie 会话。
- logout 撤销会话。
- /auth/me 返回当前用户和权限。
- 状态改变请求有 CSRF 保护。

验证：集成测试成功、失败、撤销和过期。

依赖：LUX-020。

#### LUX-023：Emby System/Ping 兼容端点

验收：

- 同时支持根路径和 /emby 前缀。
- 返回稳定 ServerId、Lux 名称、版本和启动状态。
- 公开信息不泄露内部路径。

验证：与官方字段模型的 shape fixture 对比。

依赖：LUX-013。

#### LUX-024：Emby 登录和设备令牌

验收：

- Users/Public、AuthenticateByName、Sessions/Logout 可用。
- 解析 Emby Authorization 设备字段。
- AccessToken 仅返回一次，数据库只存哈希。
- X-Emby-Token 和 api_key 兼容。

验证：协议集成测试覆盖登录、调用、logout 后 401。

依赖：LUX-020、LUX-023。

#### LUX-025：三客户端连接探针

描述：在 VidHub、SenPlayer、Infuse 中手动添加 Lux，只验证发现与登录，不实现媒体库。

验收：

- 记录每个客户端版本、请求序列和结果。
- 未实现路径被结构化记录且已脱敏。
- 至少一个客户端能成功登录；若不能，先修复 P0 契约。

验证：COMPATIBILITY.md 有实际证据。

依赖：LUX-024。

阶段门：

- 三个客户端全部能添加服务器并完成登录，或有项目所有者明确接受的阻塞记录。
- 未通过时不得进入大规模媒体库实现。

### 阶段 3：第一个电影端到端纵切片

#### LUX-030：媒体库和多根路径模型

验收：

- 管理员 API 可创建电影库。
- 可添加多个规范化根路径。
- 路径必须存在且在容器中可读；写权限单独报告。
- 重复和重叠路径给出明确错误/警告。

验证：临时目录集成测试。

依赖：阶段 2。

#### LUX-031：单电影目录发现

描述：只实现电影库中一个常见目录的扫描纵切片。

验收：

- 发现一个 MKV/MP4 文件。
- 从目录/文件名建立逻辑电影与媒体源。
- 扫描结果持久化，重启可查询。

验证：fixture 扫描测试。

依赖：LUX-030。

#### LUX-032：本地电影 NFO 和海报

验收：

- 读取 movie.nfo 或同名 NFO。
- 本地标题、年份、简介进入索引。
- 发现 poster 和 fanart。
- 坏 NFO 不阻塞电影入库。

验证：正常、部分、损坏 NFO fixtures。

依赖：LUX-031。

#### LUX-033：ffprobe 媒体信息

验收：

- 只对新增/变化文件运行。
- 保存容器、时长、视频/音频/字幕轨。
- 超时、退出码和损坏文件转成任务状态。

验证：小型合法/损坏 fixture；第二次扫描不重复 probe。

依赖：LUX-031。

#### LUX-034：电影查询纵切片

验收：

- Lux API 能列出和查看该电影。
- Emby Items/用户 Items/详情端点能返回兼容 DTO。
- 列表默认分页。

验证：API 集成测试和 DTO golden 测试。

依赖：LUX-032、LUX-033。

#### LUX-035：本地海报兼容端点

验收：

- Lux 和 Emby 图片端点读取同一图片记录。
- GET/HEAD、ETag 和 If-None-Match 正确。
- 不允许路径穿越。

验证：200、304、404、403 测试。

依赖：LUX-032。

#### LUX-036：基础媒体库 ACL

描述：在所有媒体查询进入 application service 时建立统一授权器，后续功能必须复用，不能等到发布前补权限。

验收：

- 管理员可为普通用户授予或拒绝媒体库访问。
- 列表、详情和图片端点均执行同一 ACL。
- 已知 item ID 不能绕过库权限。

验证：两个用户、两个媒体库的权限矩阵集成测试。

依赖：LUX-030、LUX-034、LUX-035。

阶段门：

- 三个客户端至少能看到一个电影的名称、详情和海报。
- 无权用户无法看到或按 ID 获取该电影和图片。
- 尚不要求播放。

### 阶段 4：高性能扫描引擎

#### LUX-040：文件指纹和扫描 generation

验收：

- 快速指纹稳定。
- 完整扫描能标记本轮 seen。
- 未变化文件跳过昂贵处理。

验证：同一树扫描两次，第二次 probe/NFO 任务为零。

依赖：阶段 3。

#### LUX-041：持久扫描任务和游标

验收：

- 扫描按批次提交。
- 进度和游标落库。
- 容器重启后恢复。
- 可取消。

验证：中途终止进程后恢复测试。

依赖：LUX-040。

#### LUX-042：实时监听、防抖和事件合并

验收：

- 新增、修改、重命名、删除进入局部任务。
- 同一路径短时间事件合并。
- 通道有界。
- 局部任务只处理事件路径，不执行整库目录遍历。

验证：临时目录事件集成测试。

依赖：LUX-041。

#### LUX-043：全量调和和根路径故障保护

验收：

- 全量调和只对变化项派生任务。
- 根路径不可用时不大规模删除。
- 完整 generation 后才标记 missing。

验证：模拟卸载、恢复和真实删除。

依赖：LUX-041。

#### LUX-044：每库扫描计划与资源配额

验收：

- 每个库独立实时开关、增量/调和频率和并发。
- 文件计划与元数据计划是独立模型。
- 修改计划无需重启。

验证：时间控制测试和管理 API 测试。

依赖：LUX-041。

#### LUX-045：60k 扫描 fixture 与基准

验收：

- 生成可重复大库 fixture。
- 记录首次扫描、无变化重扫、单目录增量结果。
- 前台 API 在扫描中达到性能目标或记录差距。

验证：固定命令输出 PERFORMANCE.md 记录。

依赖：LUX-044。

阶段门：

- 无变化全量校验不运行 NFO/ffprobe/TMDb。
- 扫描可恢复。
- 前台没有因扫描被长时间锁住。

### 阶段 5：元数据、刮削器和重新匹配

#### LUX-050：字段级来源和锁定规则

验收：

- 本地、TMDb、fallback 来源可追踪。
- locked 字段永不被自动刷新覆盖。
- 空在线字段不覆盖有效本地值。

验证：表驱动合并测试。

依赖：阶段 4。

#### LUX-051：TMDb 客户端边界

验收：

- token 配置、超时、16 并发/32 次每秒限流、退避和响应验证。
- 主进程所有 TMDb API 调用均经 `org.lux.tmdb` 插件协议，不存在绕过插件的直连路径。
- zh-CN 请求与英文回退可测试。
- 测试使用 stub，不调用真实 TMDb。

验证：模拟 200、404、429、5xx、超时。

依赖：LUX-050。

#### LUX-052：候选搜索和保守匹配

验收：

- provider ID 精确确认。
- 明确标题+年份可以高置信自动匹配所选刮削器条目。
- 候选接近时进入 PENDING。

验证：中文、英文、同名翻拍、缺年份 fixtures。

依赖：LUX-051。

#### LUX-053：待处理和候选管理 API

验收：

- 分页查看待处理。
- 搜索候选。
- 预览字段差异。
- 只有管理员可访问。

验证：API 和 ACL 测试。

依赖：LUX-052。

#### LUX-054：原子 NFO 写回

验收：

- 写回 common NFO 字段。
- 保留要求保留的未知字段。
- 临时文件+原子替换。
- 只读、磁盘满和并发修改不破坏原文件。

验证：故障注入测试。

依赖：LUX-050。

#### LUX-055：图片下载和原子写回

验收：

- poster/fanart 缺失时下载。
- 验证类型、大小、内容。
- 写回后图片索引更新。

验证：stub 图片服务和损坏响应。

依赖：LUX-051、LUX-054。

#### LUX-056：重新识别纵切片

验收：

- 管理员可选择候选。
- 可选择仅补缺或刷新未锁定在线字段。
- NFO/图片成功写回后条目变为确认状态。
- 失败可重试且不谎报成功。

验证：端到端集成测试。

依赖：LUX-053、LUX-054、LUX-055。

阶段门：

- 一个无 NFO 电影可通过 TMDb 补齐并写回。
- 一个同名歧义电影进入待处理。
- 一个错误条目可重新匹配所选刮削器条目。

### 阶段 6：剧集、混合库和字幕

#### LUX-060：剧集/季度/单集领域层级

验收：

- Series、Season、Episode 父子关系稳定。
- 季集号、特别篇和缺季目录有测试。
- 逻辑 ID 在重扫后稳定。

验证：剧集目录 fixtures。

依赖：阶段 5。

#### LUX-061：tvshow、season、episode NFO

验收：

- 读取 tvshow.nfo、季度图片、单集 NFO。
- 本地字段优先和写回规则与电影一致。

验证：多季剧集 fixture。

依赖：LUX-060。

#### LUX-057：统一媒体文件名解析与 Movie/TV 匹配

范围：参考 qmby 的 `ParseMediaName` 和刮削器候选策略，在 Lux 应用层提供统一的文件名/目录名解析与标题清洗。解析结果至少包含清洗后的标题、年份、季号、集号、版本和清晰度；支持 `SxxEyy`、`x` 格式、中文“第 N 季/第 M 集”和年份紧贴标题的常见命名。去除分辨率、编码、音频、字幕、来源、发布组等技术噪声，但保留可用于媒体源聚合的版本和清晰度字段。

元数据匹配和搜索必须使用媒体库所选刮削器，并按媒体类型分流；TMDb 刮削器的电影调用 `/search/movie`、剧集调用 `/search/tv`。带年份搜索无结果时允许回退无年份搜索，并对中文/英文标题候选逐项尝试。Lux 扫描、候选搜索、批量重新匹配和各刮削器插件使用同一解析语义；插件 RPC 公开字段保持兼容，不泄露凭据。

验收：

- [x] `暗夜与黎明2024` 清洗为标题“暗夜与黎明”、年份 2024；`暗夜与黎明 S01E01 H 265 AAC CHDWEB` 不把技术标签写入标题。
- [x] 统一解析器覆盖电影、剧集、季度、单集的年份/季集号和常见技术标签，并保留版本/清晰度信息。
- [x] MOVIE 候选和重新匹配请求只调用 `/search/movie`，SERIES 请求只调用 `/search/tv`；TV 搜索支持中文结果缺字段时的英文逐字段回退。
- [x] `lux-plugin-tmdb` 的 `metadata.search` 对相同输入产生相同清洗标题和类型分流，协议响应字段不变。
- [x] 解析和匹配错误只产生待处理/可重试结果，不在用户 HTTP 请求路径扫描文件或直接调用 TMDb。

验证：

- `cargo test --locked --test media_matching --test scanner --test series_scanner --test metadata_api --test tmdb --test tmdb_plugin`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`

依赖：LUX-052、LUX-060、LUX-061、LUX-142 的现有 TMDb 插件协议边界。

#### LUX-062：Emby Seasons/Episodes/NextUp

验收：

- 三端点按用户权限和进度返回。
- 单集 UserData 正确。
- 分页与排序稳定。

验证：协议集成测试。

依赖：LUX-061。

#### LUX-063：混合库分类

验收：

- 同一根目录可发现电影和剧集。
- 不确定内容进入 UNRESOLVED。
- 不因单个误分类破坏层级。

验证：混合 fixture。

依赖：LUX-060。

#### LUX-064：外挂与内嵌字幕索引

验收：

- 识别常见外挂扩展名和语言标记。
- ffprobe 流映射到 Emby MediaStreams。
- 字幕读取端点执行 ACL。

验证：多语言、多格式 fixture。

依赖：LUX-033、LUX-060。

阶段门：

- 三个客户端能浏览剧集、季度和单集。
- 可看到内嵌和外挂字幕轨信息。

### 阶段 7：播放、进度和收藏

#### LUX-070：Range 文件服务

验收：

- GET/HEAD、完整请求、单 Range、无效 Range 正确。
- 大文件不进入内存。
- ACL、取消和路径安全正确。

验证：RFC 边界单元测试和集成流测试。

依赖：阶段 6。

#### LUX-071：PlaybackInfo 和版本选择基础

验收：

- 只声明 DirectPlay。
- 返回稳定 source ID、媒体流和直放 URL。
- 默认 source 选择稳定。

验证：Emby DTO contract tests。

依赖：LUX-070。

#### LUX-072：.strm 直交

验收：

- 读取首个非空行并处理 BOM。
- 播放和 PlaybackInfo 不校验、不请求、不代理；下载端点的远程请求按 LUX-091 单独执行。
- URL 不进入日志。

验证：http、https、含查询令牌和空文件 fixtures。

依赖：LUX-071。

#### LUX-073：播放会话事件

验收：

- Playing、Progress、Stopped 幂等。
- 设备会话可查询。
- 会话保存并返回 `Client`、`DeviceName`、`DeviceId`、`DeviceType` 和 `ApplicationVersion`；事件体字段优先，缺失字段从 Emby 认证头回填。
- 播放会话记录接收请求的真实对端 IP；Emby `GET /Sessions` 按 `SessionInfo.RemoteEndPoint` 返回该 IP，无法获得时返回空值。
- 乱序进度不异常倒退。

验证：并发和乱序测试。

依赖：LUX-024、LUX-071。

#### LUX-074：继续观看和已看阈值

验收：

- 默认 90% 已看和 2 分钟继续观看门槛。
- 管理员可调整。
- 多用户完全隔离。

验证：边界值测试和 Resume API。

依赖：LUX-073。

#### LUX-075：收藏与已看 API

验收：

- Lux 与 Emby 端点操作同一用户状态。
- 重复 POST/DELETE 幂等。
- 无权条目返回 404 或兼容性要求的状态，避免信息泄露。

验证：多用户 API 测试。

依赖：LUX-074。

阶段门：

- 三个第三方客户端都能播放本地文件和 .strm。
- 进度、继续观看、已看和收藏在重启后正确。

### 阶段 8：搜索、筛选、合集和多版本

#### LUX-080：FTS5 搜索纵切片

验收：

- 标题、原标题和别名可搜索。
- 中文标题 fixture 可命中。
- 结果经过 ACL。
- 分页和稳定排序。

验证：查询集成和性能测试。

依赖：阶段 7。

#### LUX-081：媒体库筛选和排序

验收：

- 类型、年份、已看、收藏筛选。
- 名称、最近添加、发行日期、评分排序；评分为空的条目稳定排在有评分条目之后。
- Lux 和 Emby 查询语义映射。

验证：组合筛选测试。

依赖：LUX-080。

#### LUX-082：首页聚合

验收：

- 一次 Lux API 返回继续观看、可见库入口，以及每个可见媒体库按 `added_at` 从新到旧排列的最新资源横栏数据。
- Emby Latest/Resume/Views 分别正确。
- 无 N+1 查询。

验证：SQL 查询计数和性能测试。

依赖：LUX-081。

#### LUX-083：多版本聚合

验收：

- 可靠 provider ID/显式规则聚合。
- 不同剪辑版可独立。
- 进度绑定逻辑 item。
- 媒体源可选择。

验证：4K/1080p/edition fixtures。

依赖：LUX-071、LUX-052。

#### LUX-084：TMDb 自动合集

验收：

- TMDb collection 生成 BOX_SET。
- 成员按权限过滤。
- 重复刷新幂等。

验证：合集 stub 和 API 测试。

依赖：LUX-051、LUX-081。

阶段门：

- 60k 数据集中所有首页、搜索和库浏览性能达标。
- 多版本和合集在至少一个第三方客户端显示正确。

### 阶段 9：权限与远程访问

#### LUX-090：媒体库 ACL

验收：

- 审计 LUX-036 之后新增的全部资源端点。
- 所有列表、详情、图片、字幕、播放、下载和搜索一致执行 ACL。
- 默认策略明确，禁止通过已知 ID、source ID 或 image ID 绕过。

验证：跨用户矩阵测试。

依赖：阶段 8。

#### LUX-091：下载与管理权限

验收：

- can_download 控制下载 API/UI。
- can_manage_server 控制所有管理 API。
- 普通用户无管理数据泄露。
- 本地媒体源以单文件流响应；`.strm` 媒体源读取首个非空 URL 并流式转发远程资源，不返回 `.strm` 文本、不创建 ZIP。
- Lux/Emby 下载均支持 GET/HEAD、单 Range 和必要的上游响应头，并在远程请求前执行 URL/解析地址安全策略。

验证：权限矩阵集成测试。

依赖：LUX-090。

#### LUX-092：可信代理和远近端判断

验收：

- 默认不信任转发头。
- 只接受配置代理 CIDR 的转发头。
- can_remote_access 在所有认证入口和媒体请求生效。

验证：伪造头、可信代理、Tailscale 地址测试。

依赖：LUX-090。

#### LUX-093：认证限流和审计

验收：

- 登录失败限流。
- 审计记录用户管理、权限、媒体库和元数据重新匹配操作。
- 日志脱敏。

验证：限流时间测试和日志快照测试。

依赖：LUX-091。

#### LUX-094：用户管理 API

验收：

- 管理员可以创建、禁用、改密和查看用户。
- 可编辑媒体库 ACL、远程访问、下载和管理控制台权限。
- 不允许删除或禁用最后一个可管理服务器的账户。
- 普通用户不能调用任何用户管理端点。

验证：API 集成测试和最后管理员保护测试。

依赖：LUX-091、LUX-092。

阶段门：

- 自动化测试证明任意受保护资源无法跨库越权。
- 反向代理部署模型经过人工复核。

### 阶段 10：Web 初始化和管理控制台

#### LUX-100：Web 工程和 API 客户端

验收：

- TypeScript strict。
- 统一 API 错误和鉴权处理。
- 生产构建由 Rust 服务同源提供。

验证：Web 单测、构建、Rust 静态资源集成测试。

依赖：阶段 9。

#### LUX-101：初始化向导

验收：

- 创建首个管理员。
- 首次引导不要求设置 TMDb 凭据；自定义 API Key 在 TMDb 插件详情页配置。
- 可创建首个库或跳过。
- 初始化后不能再次访问。

验证：Playwright。

依赖：LUX-100、LUX-021。

#### LUX-102：管理仪表盘和健康

验收：

- 使用一个受保护的仪表盘接口显示可编辑的服务器名称、Lux 版本、库统计、运行任务、错误数和健康检查。
- 概览显示 Lux 进程运行时长，以及仅基于容器 cgroup 的 CPU、内存和 `/media` 挂载点存储指标；容器未暴露对应 cgroup 或挂载点不可用时明确显示不可用，不伪造宿主机数据。
- 显示当前正在播放会话；卡片包含账户、媒体标题/剧集信息、海报、进度、客户端/设备、客户端来源 IP、来源质量、视频轨和音频轨摘要。
- 播放卡片中，电影只显示电影标题；剧集以剧名为白色主标题，灰色副标题显示 `S01E02 · 单集标题`，并按用户、设备、客户端展示账户信息。
- 播放卡片明确显示客户端名称/版本、设备名称/类型和设备 ID（设备 ID 可折叠或以次要信息展示）。
- 显示最近登录、开始播放、暂停和停止播放的账户活动；活动记录由服务端统一写入并按时间倒序返回。
- 仪表盘数据有服务端数量上限，管理员 Web 端通过受保护的 SSE 接收变更通知并按作用域刷新查询；CPU、内存和存储等资源指标仍使用低频采样，不因页面打开产生过度轮询负载。

验证：API 集成测试、组件测试和 Playwright。

依赖：LUX-100。

#### LUX-103：媒体库和计划管理

验收：

- CRUD 库和多个根路径。
- 添加根路径时可通过按需加载的服务器目录树选择 Docker 容器内目录，同时保留手动输入；目录浏览仅限管理员、只返回目录并具有分页上限。
- 可编辑已有媒体库的名称和类型。
- 管理员可上传或替换媒体库封面图；封面图格式和大小经过服务端校验，并在服务重启后保持。
- 普通用户只能读取自己有权限访问的媒体库封面图。
- 文件扫描与元数据计划统一在任务与日志页配置，媒体库编辑页不再提供计划字段。
- 显示读写与监听状态。
- 首页和媒体库入口支持右键打开 Lux 自定义操作菜单，可对整个媒体库发起元数据匹配或扫描，并显示任务提交结果。

验证：媒体库 API 集成测试、Web 单测、Web 构建和 Playwright。

依赖：LUX-102。

#### LUX-104：用户和权限管理

验收：

- 创建、禁用、改密。
- 媒体库 ACL、远程、下载、管理权限。

验证：Playwright 和服务端权限回归。

依赖：LUX-094、LUX-102。

#### LUX-105：任务、日志和错误页

验收：

- 初始没有任何注册任务时，页面显示明确的空状态；任务由系统或插件注册后才出现。
- 创建媒体库后自动出现两个系统注册任务：全量校验媒体库、元数据刮削；注册项包含稳定类型、名称、说明、作用范围和注册来源。实时增量扫描由文件系统监听触发，不出现在计划任务列表中。
- 查看、取消、重试运行中的任务。
- 过滤失败类型。
- 日志脱敏。
- 已注册任务区分页查看任务，只能修改已注册项的计划、启停和资源配置；页面不提供任意新增任务类型或全局未注册任务的入口。
- 任务注册项缺少执行计划时明确显示“未配置”，不伪造调度状态。

验证：Playwright。

依赖：LUX-102。

#### LUX-106：待处理、重新匹配和图片管理

验收：

- 查看候选和 diff。
- 仅补缺/刷新未锁定选择。
- 写回成功/失败状态。
- poster/fanart 选择。

验证：Playwright 完整元数据重新匹配流程。

依赖：LUX-056、LUX-100。

阶段门：

- 管理员无需调用 API 即可完成初始化、用户、媒体库、扫描和纠错。
- 普通用户无法进入控制台。

### 阶段 11：普通用户 Web 客户端

#### LUX-110：登录和首页

验收：

- 登录、退出和会话恢复。
- 继续观看、媒体库入口和搜索。
- 无权库不显示。

验证：Playwright 多用户测试。

依赖：阶段 10。

#### LUX-111：媒体库列表与筛选

验收：

- 类型、年份、已看、收藏筛选。
- 名称、最近添加、发行日期、评分排序。
- 游标分页或虚拟滚动。
- 首页和媒体库中的剧集海报在右上角显示集数；剧集显示全部单集数，季度显示该季度单集数。

验证：大列表 Playwright。

依赖：LUX-110。

#### LUX-112：电影、剧集和合集详情

验收：

- 显示 poster、fanart、简介、季度/单集、合集和 UserData。
- 元数据匹配确认时通过所选刮削器抓取主要演员及角色名；详情页以圆形头像卡片展示演员，头像使用 `/config/people` 中的本地缓存。
- 详情页存在本地 logo/clearlogo 时显示在标题前；没有徽标时仅显示标题。
- 多版本选择。

验证：组件与 Playwright。

依赖：LUX-111。

#### LUX-113：Web 直放播放器

验收：

- 浏览器支持的源可播放。
- 使用与 Emby 兼容层相同的播放状态模型，上报开始、定时进度、暂停、停止和页面离开事件。
- 从服务端共享状态恢复播放位置；Web 与第三方播放器的进度和当前播放状态保持一致。
- 不支持的编码清晰提示。
- 不触发任何转码任务。

验证：可播放 MP4 和不可播放 fixture。

依赖：LUX-112、LUX-073。

#### LUX-114：响应式与可访问性

验收：

- 手机、平板、桌面布局。
- 键盘导航和表单错误可访问。
- 无明显横向溢出。

验证：Playwright 多 viewport 和自动 a11y 扫描。

依赖：LUX-113。

阶段门：

- 普通用户可只用浏览器完成登录、浏览、搜索、播放和续播。

### 阶段 12：三客户端完整兼容

每个客户端单独完成，不把三者放进一个大任务。

#### LUX-120：Infuse 完整流程

验收：

- 添加、登录、库、搜索、详情、本地直放、.strm、字幕、进度、收藏和版本选择。
- 所有差异记录到 COMPATIBILITY.md。
- 修复有自动协议回归测试。

依赖：阶段 11。

#### LUX-121：VidHub 完整流程

验收同 LUX-120。

依赖：LUX-120 的公共兼容修复完成。

#### LUX-122：SenPlayer 完整流程

验收同 LUX-120。

依赖：LUX-121 的公共兼容修复完成。

#### LUX-123：兼容回归套件

验收：

- 三客户端核心请求序列成为脱敏 fixture。
- CI 能验证 P0/P1 DTO 和状态码。
- 文档列明支持的最低实测客户端版本。

依赖：LUX-120 至 LUX-122。

阶段门：

- 三客户端矩阵核心项全部通过。
- 不以“官方 API 已实现”代替真实客户端测试。

### 阶段 13：性能、Docker 和发布候选

#### LUX-130：SQL 查询审计和索引

验收：

- 热查询有 EXPLAIN 记录。
- 消除 N+1。
- 按真实筛选增加最小必要索引。

验证：60k 基准。

依赖：阶段 12。

#### LUX-131：扫描与前台隔离调优

验收：

- 扫描期间 p95 达标。
- 写批次、连接池、checkpoint 和并发有记录。
- 资源上限可配置。

验证：组合压力测试。

依赖：LUX-130。

#### LUX-132：媒体 Range 压力测试

验收：

- 4 个并发直放连接稳定。
- 内存不随文件大小增长。
- 客户端断开释放资源。

验证：自动压力脚本。

依赖：LUX-070。

#### LUX-133：生产 Docker 镜像

验收：

- 多阶段 amd64 构建。
- 非 root。
- 包含 ffprobe、Web 静态资源、健康检查。
- 空卷初始化和升级迁移可用。

验证：全新 compose E2E。

依赖：LUX-131。

#### LUX-134：Tailscale/反代部署文档

验收：

- HTTPS、trusted proxy、Range、超时和流缓冲配置说明完整。
- 明确不公开初始化中的实例。

验证：至少一种真实反向代理手工验证。

依赖：LUX-133。

#### LUX-135：安全和故障恢复审查

验收：

- ACL、路径、令牌、NFO、XSS、代理头和日志审查。
- 模拟磁盘满、媒体挂载丢失、TMDb 失败、容器强制终止。
- 高风险问题全部关闭或有明确接受记录。

验证：安全测试和故障注入报告。

依赖：LUX-133。

#### LUX-136：发布候选

验收：

- 全局完成标准通过。
- 兼容矩阵通过。
- 性能目标通过或项目所有者明确接受偏差。
- README、部署、升级、已知限制完整。
- 生成带版本号的 Docker 镜像。

依赖：LUX-134、LUX-135。

最终阶段门：

- 在真实飞牛 NAS 上运行至少 7 天。
- 完成至少一次容器重启、媒体库增量更新和全量调和。
- 三个客户端和 Web 无阻塞级问题。

### 阶段 14：正式版后的可选增强

按价值单独立项，不提前混入：

- Emby 播放进度、已看和收藏导入。
- 自定义合集。
- banner、人物图和章节缩略图完善。
- 内容分级和标签 ACL。
- 局域网自动发现。
- 内嵌字幕按需无转换抽取。
- Web 浏览器兼容转码，需要全新规格和 ADR。

#### LUX-140：内置元数据插件与媒体库刮削器选择

范围：增加插件注册表和通用刮削器选择。管理员可以查看插件目录并安装刮削插件，通过已安装管理页启用或禁用插件；媒体库创建和编辑接口返回并持久化 `scraperId`，Web 管理页面提供可用刮削器选择。

验收：

- [ ] 空数据库迁移后，插件目录分页返回 TMDb，且未安装时不能被媒体库选择。
- [ ] 管理员安装任意合法刮削插件后，插件状态显示为已安装并可作为媒体库刮削器；TMDb 仍可在插件详情页填写自定义 API Key。
- [ ] 已安装管理页不把“已安装”作为静态状态展示，而是提供带有明确启用/禁用状态的开关；切换通过 `PATCH /api/v1/admin/plugins/{pluginId}/enabled` 持久化，刷新或重启后保持，禁用插件仍保留在已安装列表且不能作为新的媒体库刮削器。
- [ ] 创建和编辑媒体库可以选择或清空 `scraperId`，重启服务后选择保持；无效、未安装或未配置插件选择被拒绝。
- [ ] 非管理员不能查看或修改插件安装状态，也不能修改媒体库刮削器配置。
- [ ] Web 管理员可以完成安装 TMDb、创建媒体库并选择 TMDb、编辑已有媒体库并保存选择。

验证：

- `cargo test --locked --test plugins`
- `cargo test --locked --test libraries_api`
- `pnpm --dir web test`
- `pnpm --dir web build`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`

依赖：LUX-051、LUX-103。

明确不做：

- 不实现任意外部插件包下载、签名验证、动态加载或沙箱运行。
- 不在本任务增加新的 TMDb API 能力；TMDb 通过通用刮削器 RPC 适配现有 `TmdbClient` 能力。

#### LUX-141：内置插件配置与 TMDb 凭据

范围：扩展内置插件注册表的配置能力。插件目录返回非敏感配置 schema；管理员可以点开可配置插件，填写、保存或清除插件配置。TMDb 插件支持自定义 v3 API Key，并内置兼容 Emby 的默认 Key；首次引导不再出现 TMDb 配置。

验收：

- [ ] TMDb 插件目录返回 `configurable`、`configFields` 和不泄露明文凭据的配置状态；不可配置插件不提供展开配置。
- [ ] 管理员可以通过插件详情保存或清除 TMDb API Key；写操作需要管理员鉴权与 CSRF，配置目录文件权限为 0600。
- [ ] TMDb 请求优先使用自定义 API Key；清除后恢复内置 Key；历史 Read Access Token 仍可兼容使用。
- [ ] 首次引导的 React 页面、旧版静态页面和 setup API 均不再提供 TMDb 配置字段。
- [ ] 插件 API 响应、健康接口和日志不包含 API Key 或 Read Access Token。

验证：

- `cargo test --locked --test plugins`
- `cargo test --locked --test tmdb`
- `cargo test --locked --test setup`
- `pnpm --dir web test`
- `pnpm --dir web build`

依赖：LUX-140、LUX-051。

明确不做：

- 不在本任务增加新的 TMDb 上游能力；插件配置字段的通用持久化仍按各插件后续任务扩展。

#### LUX-142：动态插件包与独立 TMDb 插件

范围：将插件库从仅内置注册项升级为可发现的 `.zip` 插件包注册表。插件包必须包含 `manifest.json`、平台运行时和文件哈希；Lux 在服务重启时扫描 `/config/plugins`，验证后通过独立进程和稳定 RPC 协议调用插件。历史签名字段仅作兼容信息，新打包器始终生成普通包。将现有 TMDb 客户端和已反编译确认的 Emby MovieDb 行为重写为独立 `org.lux.tmdb` 插件，不直接加载原始 `MovieDb.dll`。

插件协议保留 Emby 风格的公开类型名称和字段语义，包括 `BaseItem`、`Movie`、`Series`、`Season`、`Episode`、`Person`、`BoxSet`、`MetadataResult`、`RemoteSearchResult`、`RemoteImageInfo`、`ProviderIds`、`ImageType` 及元数据/图片 Provider 能力。Lux 内部领域模型仍与 Emby DTO 分离，由适配层完成映射。

插件包采用跨平台 ZIP 格式，例如 `org.lux.tmdb-1.0.0.zip`。ZIP 根目录必须包含：

- `manifest.json`：包格式、插件 ID、版本、协议版本、运行时、能力、配置和权限声明。
- `binaries/`：按平台和架构组织的独立插件进程。
- `assets/`：图标等非执行资源。
- `signature.json`：历史包可带的签名算法、签发者和签名值；新包不生成。

插件进程通过 JSON-RPC over stdin/stdout 提供 `plugin.hello`、`plugin.health`、`metadata.search`、`metadata.get`、`metadata.images`、`metadata.credits`、`metadata.externalIds`、`metadata.trailers` 和 `plugin.shutdown`。所有刮削调用使用 provider-neutral 的 `itemType`、`providerId`、`ProviderIds` 与完整图片 URL；插件不能直接访问 Lux SQLite、媒体根目录或内部任务对象；元数据写回、图片下载和 Emby API 输出由 Lux 负责。

验收：

- [ ] 放入合法 `.zip` 插件包并重启 Lux 后，插件目录能发现、校验并展示插件；无 manifest、哈希错误、协议不兼容或平台不匹配的包不会运行；无 Lux 签名的包可以运行。
- [ ] 插件进程故障、超时或异常退出不会导致 Lux 主进程退出；状态和最后错误可由管理员查看。
- [ ] 管理员启用动态插件后，媒体库可以选择稳定的 `scraperId`，重启后选择保持。
- [ ] 独立 `org.lux.tmdb` 插件覆盖 MovieDb 的电影、剧集、季、集、人物、合集、图片、外部 ID、预告片、语言、缓存、限流和重试行为。
- [ ] TMDb 插件保留自定义 API Key、历史 Read Access Token 和内置 fallback 优先级；凭据不进入 RPC 响应、API 或日志。
- [ ] Emby 客户端登录、浏览、详情、ProviderIds 和图片展示不因插件拆分回归。

验证：

- `cargo test --locked --test plugin_protocol --test plugin_runtime`
- `cargo test --locked --test tmdb_plugin`
- `cargo test --locked --test plugins`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `pnpm --dir web test`
- `pnpm --dir web build`

依赖：LUX-140、LUX-141、LUX-120。

明确不做：

- 不在 Lux Rust 主进程中 `dlopen` 任意 native DLL。
- 不直接运行或模拟完整 Emby 服务端以兼容原始 `MovieDb.dll`；原始 DLL 只作为行为参考。
- 不自动从任意远程地址下载第三方插件包。

#### LUX-144：TMDb 多语言首选与回退配置

范围：为 `org.lux.tmdb` 插件增加首选语言、语言回退开关、有序回退语言列表和替代 API 地址配置。语言选项来自 TMDb 的主翻译语言列表，界面按简体中文、其他中文地区语言、其他语言排序；非敏感配置持久化到 `/config/tmdb_settings.json`。插件对电影、剧集、季度和单集详情按首选语言请求，并在回退开启时按选择顺序逐字段补全；替代 API 地址开启后使用管理员保存的地址。

验收：

- [ ] TMDb 插件配置返回语言下拉选项；首项为简体中文 `zh-CN`，其次为 `zh-SG`、`zh-HK`、`zh-TW`，之后为 TMDb 主翻译语言；默认首选为 `zh-CN`。
- [ ] 管理员可以保存语言回退开关和多个有序回退语言；默认预选 `zh-SG`、`zh-HK`、`zh-TW`，配置重启后保持，API 不返回任何凭据。
- [ ] 回退开启时，电影、剧集、季度、单集元数据只补全空字段，并严格遵循选择顺序；关闭时不发起回退请求。
- [ ] 替代 API 地址默认关闭并使用官方地址；开启后可选择 `https://api.tmdb.org` 或自定义 HTTP(S) 地址，插件请求实际经过所选地址。

验证：

- `cargo test --locked --test plugin_protocol --test plugins --test tmdb_plugin`
- `pnpm --dir web test`
- `pnpm --dir web build`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`

依赖：LUX-142。

明确不做：

- 不改变 TMDb Provider ID、Emby DTO 或插件 RPC 方法名称。
- 不把 TMDb 凭据放入非敏感配置、API 响应、日志或插件 RPC。

#### LUX-145：后台本地视频缩略图任务

范围：将外部 `ffmpegthumb` 的视频首帧缩略图行为重写为 Lux 内置后台任务。媒体库扫描成功后，任务为缺少缩略图的本地视频来源生成 JPEG 并登记到 `item_images`；只处理 `LOCAL_FILE`，不读取、不探测、不访问 `.strm` 指向的远程视频。

验收：

- [x] 本地视频在扫描完成后的后台阶段生成缩略图，默认截取 `00:03:01`，并通过 `THUMB` 图片记录提供给现有图片接口。
- [x] 同一逻辑媒体项优先使用默认本地来源；已有缩略图不被覆盖；缺少或失效的登记路径可以重建。
- [x] `STRM_URL` 不进入候选查询或 ffmpeg 参数；纯 `.strm` 条目不会生成缩略图。
- [x] ffmpeg 使用参数数组、路径根目录约束、原子输出和超时控制；单个文件失败不导致扫描任务失败。
- [x] 扫描任务事件记录缩略图阶段的完成/失败计数，容器重启后仍可在下一次扫描重试缺失项。

验证：

- `cargo test --locked --test thumbnails`
- `cargo test --locked --test scanning_jobs`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`

依赖：LUX-033、LUX-040、LUX-080。

明确不做：

- 不实现独立缩略图 HTTP API、Web 配置页、转码、音频 WAV 提取、字幕抽取或 `.strm` 远程处理。

---

#### LUX-146：STRM 远程媒体信息插件

范围：将 MediaInfoKeeper 的 `.strm` 远程媒体信息提取能力改写为 Lux Plugin SDK v1 的独立
`media_probe` 插件和 Lux 宿主后台任务。插件 ID 固定为 `org.lux.strm-media-info`，能力为
`media.probe`；插件只负责接收单个已校验 URL、调用 `ffprobe` 并返回受限的 format/stream
结果。媒体库选择、任务、并发、URL 安全、数据库写入和兼容旁车写回均由 Lux 宿主负责。

旧版本的 `org.lux.media-info` 作为迁移别名处理：已有插件配置会迁移到新的插件配置路径，
新的 API、manifest 和插件进程只使用 `org.lux.strm-media-info`。

插件 manifest 声明 `libraryIds`、`concurrency`、`existingInfoPolicy` 和 `writeSidecars` 配置项；
其中 `existingInfoPolicy` 的选项为 `SKIP`（跳过已有媒体信息）和 `OVERWRITE`
（覆盖已有媒体信息）。读取旧版本配置时，`includeReady: false` 迁移为 `SKIP`，
`includeReady: true` 迁移为 `OVERWRITE`。
Lux 管理页动态填充 `media-libraries` 选项并保存插件配置。管理员通过
`POST /api/v1/admin/plugins/org.lux.strm-media-info/run` 或兼容的
`POST /api/v1/admin/strm-probe-jobs` 按已保存配置启动任务，不从请求体接收宿主覆盖参数。服务为每个选定媒体库建立持久化任务，使用全局操作信号量
和媒体库 `probeConcurrency` 的较小值限制并发；任务支持分页列表、详情、取消、重试，并在服务
重启后恢复 PENDING/RUNNING 状态。探测结果保存到 `media_sources`/`media_streams`，旁车写回
使用同目录 `*-mediainfo.json` 的 MediaInfoKeeper 兼容子集和临时文件原子替换。

插件 manifest 必须声明 `type: "media_probe"`、`category: "MEDIA"` 和
`capabilities: ["media.probe"]`。插件进程不能访问 Lux SQLite、媒体根目录或内部任务对象；
插件错误、超时、异常退出和超限输出不能导致 Lux 主进程退出。RPC、任务事件、错误消息和旁车
不得包含完整 URL、认证信息或原始 `ffprobe` JSON。

当前 URL 策略只允许 HTTP/HTTPS，拒绝凭据、fragment、localhost、云实例元数据主机和字面量
回环/私网/链路本地/未指定/多播/共享地址。DNS 解析后的私网 rebinding、ffprobe 重定向逐跳
校验和管理员局域网 allowlist 属于后续生产化增强；未完成前不得声称支持任意 NAS/AList 内网地址。

验收：

- [ ] 管理员只能选择已有媒体库，未选媒体库不创建任务、不发起插件 RPC；空选择、无效 ID、并发超范围均被拒绝。
- [ ] 插件详情页展示并保存媒体库多选、并发数、已有媒体信息处理方式和旁车写回配置；配置文件原子保存且权限受限，插件列表回显非敏感值。
- [ ] 同一时间的有效探测数不超过任务全局并发和媒体库 `probeConcurrency`；单个 URL 失败只影响对应源，任务可继续。
- [ ] 服务重启可以恢复 PENDING/RUNNING 任务；取消不会领取新源，失败或取消任务可以重试。
- [ ] 成功结果写入媒体源和媒体流；`writeSidecars` 启用时写入兼容旁车，失败不会留下半个 JSON。
- [ ] 播放和 PlaybackInfo 请求不触发 STRM 远程探测，`.strm` 仍由客户端直连播放。
- [ ] 插件包、manifest、RPC 结果、URL 策略、超时、输出上限和无真实 URL 的 fake ffprobe 测试覆盖；插件异常不退出主进程。
- [ ] 从空数据库执行迁移成功，ARM64 本机验证记录 `uname -m`，并通过 Rust 格式化、测试和 Clippy 检查。

验证：

- `cargo test --locked --test plugin_protocol --test plugin_runtime --test plugin_package --test media_info_plugin --test media_info_config --test media_info_config_api --test strm_probe --test strm_probe_api`
- `pnpm --dir web test -- plugin-library.test.ts`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`

依赖：LUX-033、LUX-041、LUX-044、LUX-072、LUX-142。

明确不做：

- 不改变 `.strm` 播放直连语义，不做代理、转码、缓存或 AList API 访问。
- 不在普通扫描或用户请求路径中探测远程 `.strm`，不把插件权限扩展为媒体库/数据库访问。
- 第一版不接入计划任务；配置和手动启动通过插件管理页及管理员 API 提供。

---

---

#### LUX-150：Lux 弹幕兼容插件与后台匹配

范围：将 Emby 弹幕插件的能力重写为 Lux 内置弹幕服务。管理员配置 Dandanplay 兼容 API 基地址，或配置 `huangxd-/danmu_api` 的 API 基地址；Lux 在后台匹配已索引的本地视频，将有效 Bilibili 标准 XML 原子写回视频同目录、同 basename 的 `.xml` 旁车，并通过独立 Emby 兼容弹幕端点提供给支持弹幕接口的第三方客户端。

`danmu_api` 的 `POST /api/v2/match` 是可选的优先路径；不支持该接口时回退到 Dandanplay 兼容搜索、详情和评论接口。基地址可以包含部署 token 路径，必须保留路径但在配置响应、日志、审计和错误中脱敏。XML 旁车只登记相对路径，SQLite 保存索引和任务状态，不保存整份 XML。

管理员通过 `POST /api/v1/admin/libraries/{libraryId}/danmaku/match` 创建持久化任务，支持分页列表、详情、取消、失败重试、服务重启恢复、并发上限和默认不覆盖已有 XML。任务只领取已索引的本地视频源；`.strm`、用户请求中的整库扫描、弹幕实时发送和上游任意 URL 均不进入范围。

Emby 兼容层提供 `/api/danmu/{itemId}`、`/api/danmu/{itemId}/raw`，并保留 `option=Refresh` 和 `option=GetJsonById` 兼容别名。端点执行现有用户/媒体库 ACL；普通 Emby 字幕端点和不支持弹幕协议的客户端不属于本任务验收范围。

验收：

- [ ] 从空数据库执行迁移成功；扫描后的同名有效 XML 可以登记、读取，删除或损坏旁车会标记索引状态而不删除媒体。
- [ ] 管理员可以保存、清除和查看脱敏的弹幕地址；HTTP/HTTPS、token 路径、控制字符、凭据和 fragment 校验符合安全策略。
- [ ] `/api/v2/match` 成功响应可以得到 episode 并取得 XML；不支持 `match` 时搜索/详情回退可工作；无匹配、非 XML、超大响应、超时不会写旁车。
- [ ] 成功结果写入视频同名 `.xml`；默认不覆盖已有 XML；中断或权限失败不会留下半个目标文件。
- [ ] 后台任务支持分页、进度、取消、失败重试和重启恢复；取消不再领取新项，单项失败不终止任务。
- [ ] Emby 弹幕读取端点返回正确 Content-Type/XML，执行 ACL，并覆盖至少一个真实支持弹幕接口的客户端请求序列。
- [ ] 不实现 Web 播放器弹幕、ASS、转码、实时发送和其他非弹幕客户端适配；相关普通字幕能力不回归。
- [ ] 通过 Rust 格式化、测试、Clippy、空数据库迁移和 ARM 本机 `uname -m` 记录。

验证：

- `cargo test --locked --test danmaku --test danmaku_api --test emby_danmaku`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`

依赖：LUX-033、LUX-041、LUX-064、LUX-072、LUX-080、LUX-090。

明确不做：

- 不把弹幕 XML 当作普通字幕，不新增 Emby 标准字幕类型或强制客户端显示。
- 不执行用户输入的上游 URL，不做代理播放，不保存完整 XML 到 SQLite，不在 Web 播放器中渲染弹幕。
- 不生成 ASS、不做颜色/位置转换、不实现弹幕发送、实时推送或计划任务。

#### LUX-151：播放会话 IP 归属地

范围：参考 `IP-hiofd` 的请求签名和字段映射，在 Lux 内置一个受限的 Hiofd IP 归属地客户端。协议字段按参考项目内置为 `key11` 和 `pwd11`，不会返回 API、写入日志或持久化到数据库。管理员仪表盘的正在播放会话在已有 `remoteIp` 基础上异步显示国家、省、市、区、街道和运营商信息；解析结果只保存在进程内短期缓存，不写入 SQLite、不写入日志，也不提供普通用户查询接口。

首次展示时只返回已缓存结果，后台解析不会阻塞仪表盘请求；同一 IP 的并发解析合并，成功结果缓存 24 小时，失败结果缓存 5 分钟。回环、私网、链路本地、未指定和多播地址不发送到第三方服务。Hiofd 响应必须限制大小、验证 JSON、结果 IP 与查询 IP 一致，网络失败只显示未解析且不影响播放会话。

验收：

- [ ] 合法 IPv4/IPv6 可以按 Hiofd 协议生成请求并解析国家、省、市、区、街道和运营商；非法或非公网地址不发起查询。
- [ ] Hiofd 返回错误、超时、超大响应、非法 JSON 或结果 IP 不一致时，Lux 不泄露响应内容、不记录敏感信息，且仪表盘仍正常返回。
- [ ] 管理员仪表盘 API 返回可空的 `remoteIpLocation`，Web 在解析完成后显示归属地和运营商；非管理员不能访问仪表盘。
- [ ] 内存缓存有 TTL 和并发上限，不保存完整第三方响应，不新增数据库迁移。
- [ ] 通过 Rust/Web 测试、格式化、Clippy 和 Web 构建检查；ARM 本机记录 `uname -m`，不宣称 NAS/x86 性能。

验证：

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `pnpm --dir web test`
- `pnpm --dir web build`

依赖：LUX-073、LUX-092、LUX-102。

明确不做：

- 不把 IP 归属地作为登录、ACL、远近端判断或安全决策依据。
- 不持久化客户端 IP 归属地、不提供任意 IP 的公开查询、不接入第二个地理位置服务。
- 不在播放、搜索、媒体库扫描或普通用户请求路径中同步调用 Hiofd。

#### LUX-152：IP 归属地查询增强插件

范围：将 IP 归属地查询从 Lux 主进程内置的 Hiofd HTTP 客户端拆分为统一的动态插件能力。
插件通过现有 Plugin SDK v1 的独立进程和 JSON-RPC stdin/stdout 运行，Lux 只负责输入地址校验、
插件选择、结果校验、归一化展示和内存缓存。固定插件 ID 为 `org.lux.ip-hiofd` 和
`org.lux.qoo-ip138`。默认使用 ip138 插件；如果安装了其他 `ip_location` 插件，则停用
ip138，不再把它作为回退。Hiofd 插件显示名称为“IP归属地查询增强”，ip138 插件显示名称为
“ip138 IP归属地查询”。

统一 RPC 方法为 `ip.location`，请求为 `{ "ip": "8.8.8.8" }`，返回必须包含与查询地址一致的
`ip`，以及可选的 `country`、`province`、`city`、`district`、`street`、`isp`、`latitude` 和
`longitude` 字段。插件可以使用各自的第三方协议，但不得把第三方凭据、完整响应或上游 URL
返回给 Lux API 或写入日志。

宿主只向声明 `type: "ip_location"`、`category: "NETWORK"`、`capabilities: ["ip.location"]`
且已安装的插件发送查询；没有其他已安装归属地插件时使用 ip138；存在其他已安装归属地插件时
只尝试这些插件，不回退到 ip138。宿主拒绝非 IP、回环、
私网、链路本地、未指定和多播地址，并限制字段长度和插件响应大小。现有管理员仪表盘异步查询和
成功 24 小时/失败 5 分钟的进程内缓存保持不变，不新增数据库表或公开 IP 查询接口。

验收：

- [ ] Plugin SDK 能校验 IP 归属地 manifest 和 `ip.location` RPC 数据结构；未知插件类型或能力声明不能运行。
- [ ] 没有其他已安装归属地插件时 Lux 使用 ip138；安装 Hiofd 或其他 `ip_location` 插件后停用 ip138，校验返回 IP 与查询 IP 一致；单个插件失败不会影响播放会话。
- [ ] Hiofd 插件名称为“IP归属地查询增强”，ip138 插件名称为“ip138 IP归属地查询”；两者都提供 `plugin.hello`、`plugin.health`、`ip.location` 和 `plugin.shutdown`。
- [ ] 现有仪表盘仍只返回管理员可见的可空 `remoteIpLocation`；成功结果缓存 24 小时，失败结果缓存 5 分钟，同一 IP 不重复请求。
- [ ] 插件响应、错误和日志不包含 Hiofd 私有签名字段、凭据、完整第三方响应或完整上游 URL；第三方 HTML/JSON 经过大小和字段限制。
- [ ] 两个参考项目均提供可被 Lux Plugin SDK 直接启动的插件入口和 manifest，并有可重复的 Lux 插件包构建方式。
- [ ] 通过 Rust 格式化、测试、Clippy 和 ARM 本机 `uname -m` 记录；不宣称 NAS/x86 性能。

验证：

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `pnpm --dir web test`
- `pnpm --dir web build`

依赖：LUX-151、LUX-140、LUX-146。

明确不做：

- 不新增公开 IP 查询 API，不把归属地作为认证、ACL、近远端判断或其他安全决策依据。
- 不把 Hiofd 或 qoo-ip138 的供应商字段协议暴露为 Lux 公共协议，不持久化归属地数据。
- 不在 Lux 主进程中保留 Hiofd/qoo-ip138 的第三方 HTTP 解析实现；第三方请求只在对应插件进程中执行。

#### LUX-153：管理员控制台 SSE 实时更新

管理员控制台通过 `GET /api/v1/admin/events` 接收同源 SSE 变更通知。端点只允许已登录且
具有 `canManageServer` 的管理员 Web session，读取不要求 CSRF。服务端发送版本为 1 的
`ready` 首帧、带 `scope` 的 `invalidate` 事件和 15 秒注释心跳；广播缓冲区丢帧时发送
`all`，客户端重新读取所有活动管理员查询。作用域包括 `all`、`dashboard`、`jobs`、
`libraries`、`plugins`、`users`、`metadata` 和 `settings`。

前端只在 `AdminLayout` 建立一条 EventSource，连接恢复时补偿失效所有管理员查询，卸载时
关闭连接。扫描、元数据、插件、用户、媒体库、设置和播放/登录活动在对应服务端写入成功后
发布作用域通知；受影响的管理员审计日志和用户媒体库 ACL 查询也会失效。SSE 只传通知，不传
业务数据。页面移除页面级刷新按钮，但保留扫描、刮削、取消和重试等主动命令。资源指标继续
低频刷新，SSE 不替代资源采样。

验收：

- [ ] SSE 端点完成管理员鉴权、协议头、ready 帧、心跳和丢帧退化测试。
- [ ] 活动、后台任务和管理配置变更发布正确作用域，前端只失效受影响查询。
- [ ] 管理布局维持单连接、自动重连、重连补偿和卸载关闭行为；页面级刷新按钮全部移除。
- [ ] Rust/Web 测试、格式化、Clippy 和 Web 构建通过，并记录 ARM 本机 `uname -m`。

验证：

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `pnpm --dir web test`
- `pnpm --dir web build`

依赖：LUX-102、LUX-103、LUX-105、LUX-106。

明确不做：

- 不向普通用户或 Emby 兼容 API 提供 SSE，不传输业务数据或敏感信息。
- 不用 SSE 替代资源指标的低频采样，不增加页面级轮询。

#### LUX-154：全量调和单次发现与持久工作队列

全量调和任务不再以文件游标为依据在每个处理批次前重新遍历所有媒体库根路径。管理员 API
只持久化任务和根目录工作项并立即返回；后台 worker 先通过持久化目录队列完成一次有界目录
发现，把媒体文件保存为任务工作项，再按既有批次大小处理。目录展开与当前目录完成必须在同一
短事务中提交；服务重启后只允许重复尚未提交的当前目录或尚未提交的文件批次，已经提交的目录
不再遍历。任务取消、失败或完成时清理临时工作项，不让工作队列无限增长。

扫描 worker 使用进程内共享的容量为 1 的互斥锁。一个媒体库的全量扫描或实时增量扫描执行
期间，其他媒体库的扫描任务保持排队；锁覆盖整个扫描 worker 的生命周期，而不是只覆盖单个
批次。该机制是扫描互斥，不引入跨库 worker pool，也不改变任务的持久化、恢复和取消模型。

验收：

- [x] 创建全量任务不访问媒体文件系统；后台发现阶段对未中断任务中的每个目录只读取一次。
- [x] 发现的文件路径持久化后按批处理；处理批次不重新遍历媒体库，重启后从剩余目录或文件工作项继续。
- [x] 只有所有可用根路径完成发现后才执行 generation missing 判定；不可用或扫描中失效的根路径不批量标记缺失。
- [x] 任务进度在发现完成后具有稳定 `totalCount`，取消、失败和完成会清理临时工作项，现有增量扫描行为不变。
- [x] 自动化测试覆盖单次发现快照、分批恢复、发现期间取消、根路径不可用和工作项清理。

验证：

- `cargo test --locked --test scanning_jobs`
- `cargo build --locked`
- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `uname -m`

实施记录（2026-08-07）：`./scripts/check-all.sh` 全部通过；原生验证架构为 `arm64`。

依赖：LUX-041、LUX-043、LUX-045。

明确不做：

- 不实现 cron 解析、计划任务调度循环或跨库全局 worker pool；跨库串行化仅使用进程内扫描互斥锁。
- 不改变 Lux/Emby 公共 API，不增加核心依赖。
- 不在本任务拆分 ffprobe、NFO、缩略图或在线元数据后处理；这些资源队列另行实施和验证。

## 26. 风险与缓解

| 风险 | 影响 | 缓解 |
|---|---|---|
| Emby 客户端依赖未公开行为 | 高 | 早期 P0 探针、真实三客户端测试、独立兼容 DTO、请求序列回归 |
| 兼容范围无限膨胀 | 高 | 只承诺 VidHub、SenPlayer、Infuse 的已测试版本；端点按实际调用加入 |
| 大库全量遍历仍慢 | 高 | 实时局部事件、指纹跳过、持久游标、低优先级、前台读旧索引 |
| inotify 丢事件或 watch 上限 | 高 | 控制台健康检查、PollWatcher/定时调和回退，不以事件作为唯一事实 |
| SQLite 写竞争 | 中 | WAL、本机卷、短批量事务、有限写并发、后台 checkpoint |
| NAS 媒体目录只读 | 高 | 初始化和每库可写检查；写回失败显式展示 |
| TMDb 限流/不可用 | 中 | 本地优先、缓存、限流、退避、任务可重试 |
| 错误自动匹配污染大库 | 高 | 高置信门、候选差距、待处理、重新匹配、字段来源与锁定 |
| .strm URL 泄露令牌 | 中 | 明确产品行为、日志脱敏、只向有权限客户端返回 |
| 浏览器编码支持不足 | 已接受 | 明确不支持提示，推荐第三方客户端，不在首版转码 |
| 下载权限无法形成 DRM | 已接受 | 文档说明权限边界，不做虚假安全承诺 |
| 临时 NAS 卸载导致条目删除 | 高 | 根路径 availability、完整 generation、删除宽限期 |
| Web 与 Emby API 互相绑死 | 中 | Web 使用 Lux API，二者共享 application service |
| 侵权或品牌混淆 | 高 | clean-room、Lux 品牌、仅用公开资料和自有测试、不复制资产、不绕授权 |

---

## 27. 待确认的唯一架构假设

需求层面已足够开始。仍需项目所有者在阶段 0 门确认：

- Lux 核心服务端使用 Rust；Web 前端是否接受 React + TypeScript。本文档建议接受，因为“高效语言”目标针对服务端热路径，而浏览器 UI 使用 TypeScript 不影响索引和直放性能。

其余未特别指定的普通媒体服务行为以 Emby 的用户体验为参考，但只有本文档明确列出的能力才属于首版承诺。

---

## 28. 参考资料

实施时优先核对官方资料，不依赖博客复制协议：

- Emby REST API 总览：https://dev.emby.media/doc/restapi/index.html
- Emby 静态 API Browser：https://swagger.emby.media/?staticview=true
- Emby 用户认证：https://dev.emby.media/doc/restapi/User-Authentication.html
- Emby API Key 认证：https://dev.emby.media/doc/restapi/API-Key-Authentication.html
- Emby Identify：https://support.emby.media/support/articles/Identify.html
- Emby Metadata Manager：https://emby.media/support/articles/Metadata-manager.html
- Emby Library Setup：https://emby.media/support/articles/Library-Setup.html
- Emby Web Client 直放说明：https://emby.media/support/articles/Web-Client.html
- Tokio 官方教程：https://tokio.rs/tokio/tutorial
- Axum Router 文档：https://docs.rs/axum/latest/axum/struct.Router.html
- SQLx SQLite 文档：https://docs.rs/sqlx/latest/sqlx/sqlite/index.html
- notify 文档与大目录限制：https://docs.rs/notify/latest/notify/
- SQLite WAL：https://www.sqlite.org/wal.html
- SQLite FTS5：https://www.sqlite.org/fts5.html
- TMDb 开发文档：https://developer.themoviedb.org/docs/getting-started
- FFprobe 文档：https://ffmpeg.org/ffprobe.html
- React：https://react.dev/
- Vite：https://vite.dev/guide/
