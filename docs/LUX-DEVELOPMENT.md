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
- 因为管理员要求回写 NFO 和图片，相关媒体目录将以读写方式挂载；媒体目录中的本地资源仍需可读，
  Lux 管理的新元数据资源默认写回媒体目录；管理员可以在全局或媒体库策略中选择是否额外保存到
  /config/metadata/library。
- 默认 SQLite 数据库位于 /config 的本机持久化卷，不位于 SMB/NFS；首次引导也可以选择管理员已准备好的外部 PostgreSQL。
- 兼容性只承诺实施时真实测试并记录版本的 VidHub、SenPlayer 和 Infuse；“对标 Emby”不等于实现 Emby 全部端点。
- 首版运行单个 Lux 实例，不做多节点或高可用；外部 PostgreSQL 只作为可选的共享存储后端，不代表 Lux 首版承诺多节点部署。
- LUX-150 的弹幕首版只面向支持弹幕接口的第三方客户端；LUX-214 才会为 Lux Web 添加已登记 XML 的本地渲染。
  两者都不把 Emby 标准字幕端点当作弹幕协议，也不为其他客户端增加 ASS 或服务端转码兜底。

---

## 2. 产品目标

Lux 是一个从零实现的个人媒体服务端。它负责组织、索引、展示并直放 NAS 中的电影、电视剧和 .strm 媒体，同时提供与 Emby 客户端 API 足够兼容的接口，使 VidHub、SenPlayer 和 Infuse 能像添加 Emby 服务端一样添加 Lux。

Lux 的核心价值不是功能数量，而是：

- 在至少 60,000 个逻辑媒体条目的库中保持快速、稳定、可诊断。
- 文件变动时只增量处理受影响的目录和条目。
- 扫描、刮削、媒体探测不得阻塞浏览、搜索、登录或播放。
- 优先使用本地 NFO 和图片，保护用户已经整理的元数据。
- 以直接播放为主；本地媒体允许受控的服务端 Remux/HLS 和转码档位，`.strm` 永远不进入服务端处理。
- 使用独立、清晰的 Emby 兼容层，不让兼容 DTO 污染 Lux 内部领域模型。

### 2.1 主要用户

- 管理员：完成初始化、创建用户、管理权限、创建媒体库、配置扫描和刮削、纠正元数据匹配、查看任务与健康状态。
- 普通用户：通过 VidHub、SenPlayer、Infuse 或 Lux Web 客户端浏览和播放自己有权访问的媒体库。

### 2.2 成功定义

达到首个正式可用版本时，必须满足：

- 三个目标第三方客户端均可手动添加 Lux、登录、浏览多个媒体库、搜索、查看详情、直放、同步进度和收藏。
- Lux Web 客户端支持登录、继续观看、多个媒体库入口、搜索、筛选、详情、版本选择和浏览器原生直放。
- 10,000 部电影和 50,000 集剧集的测试库中，常用查询达到第 5 节定义的性能目标。
- 实时文件事件只触发局部增量扫描；定时全量校验在后台可暂停、可恢复，不锁住前台；实时增量扫描可与全量校验共存，并在共享扫描资源可用时按批次优先执行。
- 本地 NFO 和图片优先，所选刮削器仅补缺；低置信度匹配进入“待处理”。
- 管理员能重新匹配元数据条目并将结果原子地回写到媒体目录旁车 NFO 和图片；可选的
  /config/metadata/library 镜像也必须原子写入。
- Docker 容器重启后，用户、索引和已提交进度保持一致；未完成的持久化后台作业不会自动继续，
  而是保留任务记录并标记为 `CANCELLED`，管理员可以按现有重试入口重新提交。

---

## 3. 已确认的产品需求

### 3.1 媒体库

- 支持电影库、剧集库、混合库。
- 可创建多个逻辑媒体库。
- 管理员可以编辑已有媒体库的名称和类型。
- 单个媒体库可包含多个本地路径。
- 同一媒体库可同时包含真实媒体文件和 .strm 文件。
- `.strm` 默认只记录首个非空播放目标；首版支持 HTTP/HTTPS、本地路径、SMB 和 FTP。普通扫描、PlaybackInfo 和播放请求不得主动读取 HTTP/HTTPS、SMB/FTP 目标指向的视频源的容器信息、索引或媒体轨。管理员显式创建 STRM 探测任务后，允许通过受监督的 `media_probe` 插件在后台读取已校验的支持目标；本地路径只有在 Lux 进程实际可读时才会被读取。其他协议保留原始文本但标记为不支持，不得伪造可播放地址。
- `.strm` 若存在同名 `-mediainfo.json` 旁车，可在后台读取旁车填充已声明的媒体信息；没有旁车时保持媒体信息为空，不因缺少探测结果阻止播放。
- 每个媒体库可设置一个可选的自定义封面图；仅管理员可以上传或替换，普通用户只能在拥有该媒体库访问权限时读取。
- 媒体库封面图首版只接受 JPEG、PNG、WebP，大小上限为 5 MiB，并通过 Lux 的受保护图片接口提供。
- 自动封面内置得意黑（Smiley Sans）字体用于媒体库名称和类型副标题；字体按其官方 SIL Open Font License 1.1 随项目分发，并可通过 `LUX_COVER_FONT_PATH` 指定替代字体。
- 媒体库没有自定义封面时，扫描建立或更新索引并完成本地图片登记后，若该库已有至少 9 个带 poster 的媒体条目，系统自动注册并执行一次 `AUTO_LIBRARY_COVER` 一次性任务；该检查独立于缩略图等其他扫描后处理，服务启动时也会对已有媒体库执行一次补偿检查。任务从库中随机选择 9 张 poster，按旋转堆叠布局生成封面，并将媒体库名称及类型英文副标题绘制在封面上（电影为 `Movies`，电视剧为 `Series`，混合媒体库为 `Mixed`）。已成功生成后的任务不会因后续扫描、海报数量变化或封面删除而自动再次运行，但管理员可以在“任务与日志”中手动执行它；手动执行只会重新生成自动封面，用户上传的封面始终优先。
- 自动封面生成前后，只要管理员上传了自定义封面，就始终以自定义封面为准，自动生成不得覆盖或替换它。
- 每个媒体库默认实时监听文件系统；管理员可以单独关闭 `realtime_watch_enabled`，关闭后该库根目录不创建实时文件监控，但手动扫描、计划调和及外部刷新接口仍可用。新增、修改、重命名和删除事件只触发受影响路径的局部增量扫描。媒体库另有独立的 `realtime_metadata_auto_match_enabled` 开关，默认开启；关闭后，局部增量扫描仍更新索引，并在后台仅探测本次新增或变化的本地媒体 source，不做在线元数据补全或全库探测。开启时，局部增量扫描完成并确认有可用媒体条目时，按 `FILL_MISSING` 提交受影响条目的在线元数据补全任务。`.strm` 媒体信息仍由现有定向插件探测流程处理。全量校验和元数据任务可独立配置计划；局部增量扫描由实时事件触发，不作为管理员可配置的计划任务。
- 每个媒体库可以配置一组已安装的元数据刮削器并按顺序排序。首位固定为 `PRIMARY` 主刮削器；后续每项可标记为 `SUPPLEMENT` 补充、`BACKUP` 备用或 `BOTH` 补充兼备用。未配置时仍读取本地 NFO 和图片，但不发起在线刮削请求。主刮削器先处理本轮请求的全部能力并直接作为可信结果；若某项能力返回空、无效、不支持或重试后失败，`BACKUP` 才按顺序接管该项，已成功的其他项不重复请求。主来源尚未确认身份时，备用来源才可以参与身份匹配。身份确认后，`SUPPLEMENT` 继续请求允许的能力：单值字段只填空，多值字段去重追加，单图类型不覆盖已有图片，背景图按 URL 去重后按索引追加。不同来源的单值结果不做冲突比较、不增加人工确认；后续来源不得覆盖本地 NFO、锁定字段、更高优先级来源或已确认身份。
- 剧集库和混合库可各自选择一个已安装、已启用且可用的片头片尾数据源；未选择表示不为该库生成或输出片头片尾标记。
  片头片尾数据源必须声明 chapters.detect 或 chapters.lookup；电影库不能选择该来源。混合库只对其中的剧集/分集参与检测。
- 管理员可在“全局策略”中设置媒体库的默认元数据、图像和字幕策略；媒体库可以继承全局默认值，也可以单独覆盖。
- 全局图像策略包括海报、艺术图、横幅图、徽标、缩略图、光盘封面、壁纸开关、每项最大背景图数量和最小下载宽度；媒体库可覆盖这些开关。
- 全局策略支持保守的存储预估，并明确应用范围：仅新内容、刷新选中内容或后台刷新全部内容；全局刮削可选择仅补全或完整刮削，批量刷新必须进入任务队列。
- 不在用户请求路径中扫描目录、读取 NFO、调用 ffprobe 或访问 TMDb。

### 3.2 媒体来源

- 本地媒体来自 NAS Docker 绑定挂载目录。
- `.strm` 文件的第一个非空文本内容被视为原始播放目标，Lux 只清理 BOM 和首尾空白，不改写目标内容。
- Lux 对目标做有限的词法分类：HTTP(S) URL、本地路径、SMB URI、FTP URI 和不支持的其他协议；分类不访问网络。相对路径在真正播放时相对于 `.strm` 文件所在目录解析，绝对路径按 Lux 进程实际可读性处理，不要求落在当前媒体库根目录内。扫描阶段不读取路径指向的媒体。数据库兼容字段仍保存为 `URL`、`PATH`、`OPAQUE` 或 `EMPTY`，其中 SMB、FTP 和不支持协议使用 `OPAQUE`，运行时再按原始目标区分。
- HTTP(S) 和本地路径型 `.strm` 都保留原始目标并通过 Emby 媒体源交给外部播放代理；HTTP(S) URL 型目标使用 `Protocol=Http`、`IsRemote=true`，本地路径型目标使用 `Protocol=File`、`IsRemote=false`。`PlaybackInfo` 对这两类目标保留原始 `Path`，但 `DirectStreamUrl` 必须使用当前 Emby 服务的标准视频入口，并由 Lux 添加短期播放票据。为兼容所有可能丢失独立媒体请求鉴权的第三方播放器，URL/路径型 `.strm` 统一将 `AddApiKeyToDirectStreamUrl` 设为 `true`，并在签名 URL 中携带本次标准 Emby token 的 `api_key`；请求中有设备 ID 时，也将其作为标准 `DeviceId` 提示带入 URL。本地文件和 SMB/FTP 解析源不携带长期 token。两种情况下 Lux 都仍要求绑定条目、媒体源和用户的短期 HMAC 票据，确保客户端通过公网代理域名回到 `/Videos/{数字ItemId}/stream`，而不是直接连接 `.strm` 中可能存在的内网 302 地址。外部代理从 `Path` 提取自己的映射或 302 信息；Emby 标准视频入口对 HTTP(S) URL 型目标直接返回 302 到原始目标，不在服务器端预探测；Lux 自有播放回退继续使用播放器 User-Agent 有限解析重定向并返回 307。扫描和 `PlaybackInfo` 不访问目标。SMB/FTP 目标交给已配置的协议解析器，解析结果必须是 HTTP(S) 地址。未配置挂载或解析器时不得伪造可播放 URL，也不得把 `.strm` 文件本身作为媒体返回；其他协议始终不支持。
- Lux 不负责保护目标中可能包含的令牌或路径信息；管理员应理解目标会暴露给有播放权限的客户端或已配置的解析器。

### 3.3 播放

- 本地媒体的 Web 播放使用 0～4 档服务端计划：档位 0 为 Direct Play，档位 1 为视频/音频 copy 的 Remux，档位 2 为视频 copy、音频转码，档位 3 为硬件转码，档位 4 为软件转码。决策始终优先选择较低档位。
- Lux Web 的档位 1～4 输出会话级 fMP4/CMAF HLS；Emby 第三方客户端按会话容器协商，默认使用 MPEG-TS，
  仅在客户端明确声明 `mp4`/`fmp4` 时使用 fMP4。两类 HLS 清单和分片都只存在于播放会话临时目录，不生成永久媒体副本。
- `.strm` 只能使用档位 0。直连或重定向失败时直接返回不支持，不允许 Remux、音频转码、视频转码、HLS、代理媒体字节或在用户请求中对远程目标运行 ffprobe/ffmpeg。
- 本地文件通过带鉴权的 HTTP GET/HEAD 和单区间 Range 请求传输。`.strm` 的本地目标可以位于媒体库根目录之外；目标必须是 Lux 进程实际可读取、canonicalize 后存在的普通文件，且不会把目录或另一个 `.strm` 当作视频返回。
- URL 和本地路径型 `.strm` 在 `Path` 保留原始目标；`PlaybackInfo` 对这两类目标的 `DirectStreamUrl` 使用标准 `/Videos/{数字ItemId}/stream[.Container]?MediaSourceId=...` 入口并附带短期 Lux 播放票据，可额外携带标准 `UserId` 与请求中的 `DeviceId` 供外部代理关联播放身份；这些提示字段不承担授权。为兼容所有可能丢失独立媒体请求鉴权的第三方播放器，URL/路径型 `.strm` 的 `AddApiKeyToDirectStreamUrl=true`，并将本次标准 Emby token 作为 `api_key` 写入签名 URL；本地文件和 SMB/FTP 解析源不携带长期 token。Lux 仍强制验证 HMAC 票据。外部播放代理从原始 `Path` 提取映射或 302 信息，客户端始终请求当前公网代理域名而不是 `.strm` 中的内网地址。Emby 标准入口对 HTTP(S) URL 型 `.strm` 返回 302 并原样交接目标；Lux 自有播放回退仍使用播放器 User-Agent 有限解析重定向并返回 307，不代理媒体字节。Lux Web 的 Direct Play 计划对 URL 和路径型 `.strm` 都同时提供代理入口和签名 Lux 入口，播放器优先使用代理入口，失败后回退到签名入口；未经过代理的 Lux Web 请求仍不会绕过权限。SMB/FTP 继续使用 Lux 的协议解析器和受保护播放入口；空目标和其他协议不可播放。
- 浏览器原生无法播放时，先尝试已有的客户端 HEVC/MKV fallback；本地文件仍不可播放时再按浏览器能力选择服务端档位 1～4。客户端 fallback 不计入服务端档位。
- 暴露本地文件中的内嵌字幕轨以及同目录外挂字幕。
- 外挂字幕至少识别 srt、ass、ssa、vtt、sub、sup/pgs 等常见格式。
- 是否能够渲染某种字幕由客户端能力决定。
- Lux Web 播放使用自有 `LuxPlayer`，不直接引入 ArtPlayer 作为运行时播放器。ArtPlayer MIT 源码可以按模块选择性复制和改造，
  但 Lux 必须拥有自己的状态、事件、UI、字幕、弹幕、手势和引擎接口；所有复制或改造来源记录在
  `docs/THIRD-PARTY-NOTICES.md`。
- LuxPlayer 的字幕、弹幕、移动端手势和浏览器解码能力必须与 Lux 的播放会话、媒体源、版本、ACL、进度、章节和错误降级结合，
  不能绕过 `/api/v1/playback/sessions` 或 `.strm` 播放边界。

### 3.4 多版本

- 同一内容的 1080p、4K、Remux、Web-DL 等媒体源默认聚合为一个逻辑标题。
- 详情页允许用户选择媒体版本。
- 不同媒体源保留独立文件路径、媒体信息、播放地址和可用字幕。
- 已看、进度和收藏绑定逻辑标题，在普通清晰度版本之间共享。
- 导演剪辑版、加长版等内容不同的版本可作为独立逻辑条目。
- 自动聚合必须依赖可靠的 provider ID、显式版本标记或管理员操作，不得仅凭相似标题粗暴合并。
- 电影文件名末尾的连字符后缀只有在同目录存在唯一、完全匹配的基础视频文件时，才作为版本标签并归入基础标题；无基础文件或存在多个可能基础名时不自动归并，后缀值不使用固定白名单。

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
- TMDb 插件提供可配置的首选语言，默认使用简体中文 `zh-CN`；界面按语言组展示，每个语言组只保留一个 canonical locale，简体中文合并 `zh-CN`/`zh-SG`，繁體中文合并 `zh-TW`/`zh-HK`，英语等地区变体同样合并并使用友好名称。
- TMDb 语言回退开关默认关闭；开启后，电影、剧集、季度和单集详情只请求一次并 append `translations`，再按管理员选择的语言组顺序逐字段补全，默认预选繁體中文 `zh-TW`。图片请求继续使用 `include_image_language`，并保留英文与无语言图片兜底。
- TMDb 插件提供默认关闭的替代 API 地址开关；开启后可选择默认官方地址 `https://api.themoviedb.org`、`https://api.tmdb.org` 或自定义 HTTP(S) 基础地址。自定义地址不得包含凭据、查询参数或片段，并由插件配置持久化到 `/config/plugin-config/org.lux.tmdb.json`。
- 图片优先本地；在线图片按首选语言组、无语言、英文的顺序选择。
- 电影、剧集、季度和单集 NFO 均应兼容常见 Emby/Kodi 旁挂形式。
- 至少识别 movie.nfo、tvshow.nfo、与视频同名的 .nfo、poster、fanart/backdrop、seasonXX-poster 等常见命名。
- 写回时使用稳定、公开记录的 Lux NFO 子集，同时尽量保留未知 XML 字段，避免破坏其他软件写入的信息。

### 3.6 元数据匹配和重新匹配

- 有明确 provider ID 时直接确认身份。
- 没有 provider ID 时，可用规范化标题、年份、媒体类型和季集号通过媒体库的主刮削器和按顺序启用的备用刮削器搜索；补充刮削器不得重新决定媒体身份。
- 匹配结果保存实际成功来源的 scraper ID，并合并各 provider namespace 下的 provider ID；选择 TMDb 时保存 TMDb ID，选择其他刮削器时保存该刮削器返回的 ID。字段、图片和可合并列表数据同时记录实际来源。
- 自动匹配必须达到高置信度阈值；信息不足或最高分未达到阈值时进入“待处理”。
- “待处理”条目保留原始文件名和可播放能力，不因缺少在线元数据从库中消失。
- 低置信度匹配保留为“待确认”状态，但不提供独立的元数据纠错控制台页面。
- 整库匹配任务完成后，任务结果显示自动确认、待确认、无候选和写回失败的数量；待确认数量链接到对应媒体库的“待确认”筛选。
- 媒体库列表支持“待确认”筛选，条目卡片显示待确认标记；管理员从媒体详情页搜索候选、查看差异、选择正确条目并确认。
- 管理员可在服务器设置中选择是否显示媒体库条目的“待确认”标记，默认显示；隐藏标记不改变待确认状态和筛选结果。
- 媒体库页面支持进入多选模式；选中的条目全部为待确认时显示“批量确认”，按各条目最高分待确认候选确认并保留已刮削元数据；混合选择时不显示批量确认，并保留普通媒体操作菜单。
- 媒体详情页在完成待确认匹配后提供“下一个待确认”入口，支持连续处理同一媒体库中的异常条目。
- 元数据匹配错误时支持“重新匹配”。
- 重新匹配可选择仅补缺字段或刷新在线字段；无论哪种模式都不覆盖已锁定字段。
- 成功编辑或匹配后，将 NFO 和 Lux 管理的图片回写到媒体目录旁车；当策略启用
  `writeToMetadata` 时，再将同一份 NFO 和图片额外写入
  /config/metadata/library/<shard>/<item-id>/。媒体目录已有 NFO 和图片仍保留且优先，历史
  metadata 资源不自动搬迁。
- 新建媒体库首次添加可用根路径并完成扫描后，若媒体库配置了刮削器，自动按主/备用角色和高置信度选择最佳候选，再按补充角色补齐缺失元数据，写回元数据并按该媒体库的图像策略下载所需图片；用户无需逐条进入管理后台确认。
- 手动“扫描媒体库文件”只做文件系统调和、媒体探测和本地 NFO/图片索引，不自动发起在线刮削；管理员可以单独执行“元数据匹配/刷新元数据”。全量扫描期间首页继续读取上一份稳定快照；所有可用根路径的 Manifest 索引及缺失确认提交后立即切换首页快照，不等待本地 NFO、probe、封面或缩略图等后处理完成。NFO/图片登记由独立有界后台 worker 并行补齐。
- 媒体详情页或媒体卡片上的“扫描所在文件夹”只扫描该媒体现有媒体源所在的文件夹；媒体库管理页上的“扫描媒体库文件”才扫描整个媒体库。两者都只做文件系统调和、媒体探测和本地 NFO/图片索引，不自动发起在线刮削。
- 管理员从媒体库入口手动执行“整库元数据匹配”时，使用与新库首次处理相同的自动选择、NFO 写回和图片下载流程；低置信度条目仍进入待处理队列。
- 回写使用临时文件、刷盘和原子重命名；失败时显示可重试状态，不谎报成功。

建议的首版自动匹配门槛：

- NFO 中存在合法 TMDb ID：确认。
- 规范化标题完全一致、媒体类型一致、年份相同或相差不超过 1 年，且最佳候选达到高置信度：可自动确认；多个候选按最高分选择，搜索结果顺序作为同分时的稳定 tie-breaker。只有最高分未达到阈值或信息不足时进入“待确认”。
- 其他情况：待处理。

具体分数只属于 Lux 内部实现，不作为 Emby 兼容 API 的公共契约。

### 3.7 弹幕

- 弹幕使用独立的 Lux 弹幕服务和 Emby 兼容弹幕路由，不伪装成普通字幕轨。
- 管理员可以配置一个 Dandanplay 兼容 API 基地址，也可以配置 `huangxd-/danmu_api` 的 API 基地址；地址可包含部署 token 路径。
- 管理员可以在弹幕插件配置中选择允许匹配的媒体库；未选择的媒体库不能创建弹幕匹配任务，空选择表示不匹配任何媒体库。
- 弹幕匹配策略支持使用原始文件名、尝试本地已登记的简体/繁体标题、尝试本地已登记的英文/原始标题；按原始文件名、简繁标题、英文/原始标题的顺序逐个回退，仅在前一个候选没有匹配时请求下一个候选。
- 弹幕文字的简繁转换由上游 `danmu_api` 的部署配置负责；Lux 不引入 OpenCC、不把简繁转换伪装成所有 Dandanplay 兼容服务都支持的请求参数，也不在首版筛选弹幕语言。
- 后台匹配任务优先使用上游 `/api/v2/match`，不支持时回退到 Dandanplay 兼容的搜索、详情和弹幕接口。
- 插件安装后宿主立即注册一个全局 `DANMAKU_MATCH` 任务，默认按 UTC 每天 `0 6 * * *` 执行；未配置有效 API 或未选择媒体库时任务保留但停用。配置有效且至少选择一个媒体库后，启用任务；每次执行按所选媒体库创建匹配作业，已运行的同类作业不重复创建。
- 管理员可以在“任务与日志”中立即执行或修改该任务的 Cron；修改会同步回弹幕插件配置。插件停用或卸载后保留注册记录但停用任务，服务重启后从持久化注册记录恢复调度。
- 匹配成功的 XML 弹幕写回视频同目录、同 basename 的 `.xml` 旁车；使用临时文件、刷盘和原子重命名。
- 只承诺支持弹幕接口的第三方客户端可以通过 Lux 的 Emby 接口读取；其他客户端是否识别 `.xml` 不属于 Lux 兼容承诺。
- LUX-150 本身不实现 Web 播放器弹幕、ASS 写回、Lux 侧弹幕文字转换、实时发送、代理播放或非弹幕客户端适配；
  后续 LUX-213/LUX-214 只能读取已登记的本地 XML 并在 Lux Web 内部渲染。

### 3.8 图片

首版必须：

- 海报 poster。
- 背景图 backdrop/fanart。
- 背景图写回采用 Emby 兼容命名：首张为 `backdrop.jpg`，后续为 `backdrop1.jpg`、`backdrop2.jpg`；读取继续兼容 `fanart.jpg`、`fanart-1.jpg` 等历史命名。
- 本地图片发现、尺寸读取、缓存标签、HTTP 缓存和缩放接口兼容。
- 缺失时从实际成功来源下载并写入媒体目录的标准旁车文件；当策略启用
  `writeToMetadata` 时，同时写入 /config/metadata/library/<shard>/<item-id>/。匹配选择时按所属
  媒体库启用的图片类型逐项处理：海报、徽标、缩略图等单图类型只在没有更高优先级本地图片时写入；背景图允许多张，主来源和补充来源的图片按 URL 去重并按优先级追加。扫描发现的媒体目录图片仍按本地优先
  规则登记和提供。
- 季条目没有自己的海报时，Lux Web 可以临时展示父剧集海报作为视觉回退；该回退不创建季的
  `item_images` 记录、不写回季目录，也不改变季自身图片的来源。季自身海报后来补全后，必须优先
  展示真实季海报。

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

> 待实施专项设计：[Lux 用户管理设计方案](USER-MANAGEMENT-DESIGN.md)（2026-10-19）。
> 覆盖账号生命周期、注册邀请、用户查询、权限预设、内容限制、播放控制、设备与活动七个模块。原模块八“外挂字幕权限”已于 2026-10-19 取消，不新增用户级字幕权限或字幕文件管理功能；现有字幕播放、媒体字幕属性编辑及扫描能力保持既有规则。
> 当前仅登记方案，不代表功能已上线；公开注册、显式媒体库权限模式和 IP 黑名单等条文在对应 `LUX-*` 任务实施前同步更新。

- 第一次启动进入初始化引导。
- 第一个完成初始化的账户为管理员。
- 默认采用指定邮箱邀请注册；邀请用户完成邮箱验证、设置用户名和密码后激活。
- 管理员仍可直接创建用户，也可选择开启公开邮箱注册；公开注册默认关闭。
- 公开注册用户验证邮箱后进入待审批状态，经管理员批准后激活。
- 待邮箱验证或待审批的账户不能登录或访问媒体；模块二的实现还需提供有界请求频率限制和一次性验证凭证。
- 支持大量普通用户。
- 每个用户的进度、已看状态和收藏独立。
- 用户可以上传或替换账户头像；头像由服务端校验并持久化到 `/config/user-avatars`，同一账户在不同浏览器登录后可读取相同头像。
- 用户权限至少包括：
  - 媒体库访问模式明确为 `ALL`、`SELECTED` 或 `NONE`。`ALL` 可访问所有已启用媒体库；`SELECTED` 只可访问正向授权的媒体库；`NONE` 不可访问任何媒体库。升级前没有正向授权记录的旧账号迁移为 `ALL`，已有正向记录的账号迁移为 `SELECTED`；迁移不得改变现有账号的可见媒体。
  - 是否允许外网访问。
  - 是否允许使用下载功能。
  - 是否允许进入管理控制台。
- 内容分级和按标签控制属于后续阶段。
- 管理控制台的权限必须由服务端校验；隐藏前端菜单不等于授权。

### 3.11 首页、浏览和搜索

普通用户首页：

- 继续观看。
- 推荐轮播：服务端基于用户收藏、播放状态、播放活跃度、评分和媒体入库新鲜度，对可访问的已入库电影与剧集进行可解释的加权排序；按用户和 UTC 每日 02:00 批次生成最多 7 个推荐，同一批次保持稳定，跨批次更换推荐内容；冷启动时优先最近入库内容。基础权重保留无用户状态 `+35`、已看 `-35` 和最近播放新鲜度最多 `+30`；入库新鲜度最多 `+7`，每天衰减 1 分，7 天后为 0。移除“有用户状态且未看完”和“有播放进度且未看完”两项。评分按 0–10 分映射为最多 `+50`，无评分时使用全部评分中位数；中位数持久化并固定 30 天。180 天内每个播放过该资源的不同用户 `+1`、最多 `+50`，且这类播放活跃度进入推荐的资源最多占 7 个结果中的 5 个；每个当前收藏用户 `+5`、最多 `+50`，不设时间衰减。播放和收藏统计在每日批次刷新时物化，避免首页请求全量聚合。
- 用户有权访问的多个媒体库入口。
- 搜索入口。
- 普通搜索按逻辑资源返回电影和整剧，不把季度、单集作为默认结果；剧集层级在进入剧集详情后浏览。Emby 客户端明确通过 `IncludeItemTypes=Season` 或 `Episode` 搜索时，保留对应层级结果。
- 首页分区标题、精选轮播纯文字标题，以及媒体详情页的标题、原文名、标语和分区标题，在桌面与移动端使用随 Web 包分发的 Libre Bodoni 与 Noto Serif SC（Didone/宋体风格）；首页媒体卡片片名中文保持无衬线，英文可用 Libre Bodoni；不依赖设备预装字体，图片 Logo 不受影响。

媒体库浏览首版支持：

- 按媒体类型筛选。
- 按年份筛选。
- 按已看/未看筛选。
- 按收藏筛选。
- 按名称排序。
- 按最近添加排序。
- 按发行日期排序。
- 按评分排序。
- 所有列表分页并设置服务端上限；Emby `/Persons` 为兼容现有客户端的明确例外，接受任意正整数 `Limit`，由调用方自行承担请求超大结果集的资源成本。

后续能力：

- 演员、导演、制作公司等深度浏览。
- 全站排行榜和更复杂的内容相似度推荐。

### 3.12 播放进度

- 每个用户独立保存。
- 进度时间使用 Emby 兼容的 ticks 表示时，1 秒等于 10,000,000 ticks。
- 默认播放达到 95% 自动标记为已看；实际阈值由每个用户在个人设置中单独调整。
- 默认不足 2 分钟的进度不进入继续观看。
- 继续观看的最短进度由管理员在全局设置中调整；自动标记已看的百分比由用户在个人设置中调整。
- Lux Web 首页和 Emby Resume 按剧集聚合继续观看进度：同一剧有多条符合继续观看条件的单集时，只展示季号优先、集号其次最大的单集进度；并列时使用最近播放时间决胜。电影保持逐条显示。
- Lux Web 首页和 Emby Resume 在某剧最近播放的已看单集之后没有更靠后的合格未完成进度时，显示季号、集号顺序中其后的第一集未看且可播放单集；该单集没有进度时从 0 开始。
- Lux Web 继续观看卡片的电影首行显示片名和剩余时长，第二行显示年份；剧集首行显示剧名和剩余时长，第二行显示季集号与单集标题。第三行以自然的短句显示当前续看条目的最近播放时间；未知时间不显示该行，不把当前单集的播放时间描述成上一集完播时间。
- 收到播放开始、进度和停止事件时采用幂等更新。
- 客户端重复、乱序或延迟上报时，不允许进度无理由倒退；显式从头播放除外。
- 电影和单集达到已看阈值后自动标记为已看；季度在其全部未删除且可播放的单集均已看后标记为已看，剧集在其全部季度的可播放单集均已看后标记为已看。
- 季度或剧集没有未删除且可播放的单集时不自动标记；单集被取消已看后，相关季度和剧集重新按单集状态计算。

### 3.13 外网访问

- Lux 不实现公网穿透、UPnP 端口映射或自带证书签发。
- 外网通过 Tailscale、反向代理或用户域名接入。
- 网络代理设置是全局出站配置，支持 HTTP、HTTPS、SOCKS4、SOCKS4a、SOCKS5 和 SOCKS5h；可通过代理 URL 携带认证信息。
- 出站代理可使用 Lux 的统一配置或标准 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY`、`NO_PROXY` 环境变量；配置影响 TMDb、插件、图片和下载等出站请求。URL 型 `.strm` 的解析请求由 Lux 直连目标并绕过全局出站代理，但不代理客户端最终读取的媒体字节和入站反向代理。
- 管理员可在网络代理设置中检测 TMDb、百度、Google 和 Cloudflare 的逐站延迟，并查看通过 Cloudflare trace 获取的网络出口 IP 与国家/地区代码。
- 远程访问权限由用户策略控制。
- Lux 展示的客户端 IP 按有效的 X-Forwarded-For、X-Real-IP、Forwarded 顺序读取；Forwarded 只采用首个代理元素的 for 参数，支持 IPv4、带方括号的 IPv6 和可选端口。转发 IP 缺失或无效时不把反代对端地址当作真实客户端 IP。HTTPS 判断仍读取 X-Forwarded-Proto；远程访问权限不再依据来源 IP 判断。
- 反向代理场景必须使用 HTTPS；用户名和密码登录协议本身不能替代 TLS。

### 3.14 管理与可观测性

管理员控制台至少显示：

- 可编辑的服务器名称、Lux 服务端版本和 schema 版本。
- 每个媒体库的条目数、路径和状态。
- 当前正在播放的会话，包括账户、媒体、海报、播放进度、设备、客户端、媒体来源和音视频轨道摘要。
- 播放会话持久化并展示 Emby 风格的 `Client`、`DeviceName`、`DeviceId`、`DeviceType` 和 `ApplicationVersion`；播放事件显式字段优先，缺失字段从认证头回填。
- `PLAYING` 或 `PAUSED` 会话超过 90 秒没有收到任何播放事件时，服务端按失活会话处理，不再出现在当前播放列表、Emby `GET /Sessions` 或 Web 当前播放状态中；客户端显式上报 `STOPPED` 仍立即生效。
- 最近的账户活动，包括登录、开始播放、暂停和停止播放事件；活动请求有可识别 IP 时记录并显示 IP，
  如果启用了 IP 归属地插件且已解析成功，则同时显示归属地和运营商信息。归属地沿用进程内短期缓存，
  不写入活动记录或日志。
- 实时监听状态。
- 当前扫描进度、扫描游标和预计剩余项。
- 最近一次增量扫描与全量校验时间。
- 后台任务队列、运行中任务和失败重试。
- 待处理、元数据匹配失败、NFO 回写失败和图片下载失败数量。
- 服务端版本、运行时间、数据库状态、磁盘可写状态。
- 管理仪表盘显示 Lux 容器自身的 CPU 使用率、内存使用量/限制、`/media` 挂载点空间使用量/可用量；这些指标只能来自容器 cgroup 和容器内 `/media` 文件系统，不得回退为宿主机整体资源。
- 结构化日志查看和下载。

首版不提供内置备份与恢复。配置和数据库通过 Docker 持久化卷由 NAS 自己备份。

### 3.15 外置插件与刮削器

- Lux 提供安全的插件注册表；插件以标准 `.zip` 插件包放入 `/config/plugins`，服务重启时扫描并加载。插件代码运行在受监督的独立进程中，不直接注入 Lux Rust 主进程。Lux 发布包和 Docker 镜像不包含任何现有插件实现、打包器或插件 ZIP。
- 插件商店使用可配置的 HTTPS 目录地址；目录返回插件元数据和包地址。默认目录源为 `https://github.com/Qoo-330ml/Lux-plugins`，Lux 将其解析为仓库 `main` 分支的 `index.json`；管理员可以在插件商店页面填写其他目录地址。
- 管理员从插件商店安装插件时，Lux 只下载目录声明的 `.zip` 包，限制大小、文件数量、路径、manifest、协议版本、平台入口和声明文件 SHA-256，并在校验成功后原子写入 `/config/plugins`；未经目录声明的地址不得作为下载目标。
- 首个独立插件为 `org.lux.tmdb`，由外部插件仓库发布。它提取 Emby `MovieDb.dll` 的 TMDb 行为，按 Lux 插件协议重写，并保留 Emby 风格的媒体类型、ProviderIds、ImageType、搜索结果和图片结果定义；上游 client、凭据和图片地址处理均只存在于插件进程。
- SDK v1 同时支持 `media_probe` 插件类型。`org.lux.strm-media-info` 只接收 Lux 宿主按单个任务提交的已校验 STRM 探测目标，按原始字符串调用 `ffprobe` 并返回受限的 format/stream 结果；插件不能访问 Lux 数据库、媒体根目录或任务对象，宿主负责并发、取消、恢复、结果落库和可选旁车写回。
- 只有已安装、已启用且可用的插件才能被媒体库选择或调度。插件可以声明自己的配置字段；没有配置项的插件不需要展开配置。TMDb 的 API Key、历史 Read Access Token、默认凭据和优先级由外置插件自己解释；Lux 只保存并传递该插件的专属配置文件，任何凭据都不返回 API 或写入日志。
- 媒体库的有序刮削器列表为空表示不进行在线刮削、只使用本地元数据；插件安装状态与媒体库选择、顺序和角色均持久化，服务重启后保持不变。对旧客户端继续返回首位 `scraperId`，新 Lux API 使用带有 `scraperId`、`position` 和 `role` 的有序列表。
- 片头片尾插件的 `libraryIds` 不再是媒体库归属配置；旧配置只用于一次性迁移到对应媒体库的
  `chapterSourceId`，迁移后调度只读取媒体库字段。
- 插件列表 API 必须分页并设置服务端上限。插件安装和媒体库刮削器选择必须经过管理员鉴权与 CSRF 校验。
- 全局策略的服务器设置不得返回任何凭据；插件凭据仍只在插件管理页面配置。播放进度阈值继续属于服务器设置，不在媒体库策略页重复管理。

### 3.16 章节与片头片尾

- 章节标记绑定具体 `media_source`，同一逻辑条目的不同版本分别保存。
- 当前唯一章节来源是片头片尾检测插件，只保存 Emby 兼容的隐藏标记 `IntroStart`、`IntroEnd` 和
  `CreditsStart`。不产生普通 `Chapter`，也不虚构 `CreditsEnd`；片尾区间延伸到媒体结束。
- Lux 不主动读取容器内嵌章节，现有本地媒体 `ffprobe` 不增加 `-show_chapters`。也不从 NFO 或 EDL
  导入普通章节；播放、详情和列表请求不得为章节打开媒体文件。
- 隐藏标记的权威运行时副本保存在数据库。Lux 不修改 MKV、MP4 或其他媒体容器，也不把检测结果默认写入
  NFO 或 EDL。
- Emby 条目 DTO 与 `PlaybackInfo.MediaSources` 按公开 `ChapterInfo` 形状返回章节：
  `StartPositionTicks`、可选 `Name`、可选 `ImageTag`、`MarkerType` 和 `ChapterIndex`。
- 自动片头片尾章节由独立 `chapter_detector` 插件提供。本地音频检测插件在后台对已校验的本地媒体运行
  ffmpeg/chromaprint；在线章节源插件只接收已保存的 provider ID、季号、集号和时长，从固定远程服务
  获取已标注结果。每个章节插件必须在 manifest 的 `supportedMediaSourceKinds` 中声明自己支持的
  `LOCAL_FILE`/`STRM_URL` 媒体源，宿主按声明筛选候选，不按插件 ID 推断。在线章节源可以声明两者，
  不读取媒体路径或 `.strm` 目标；指纹检测合同当前只能声明 `LOCAL_FILE`。两种插件都不能接收数据库
  或任务对象。
- 检测插件按季度批次比较至少两个可用分集，返回 `IntroStart`、`IntroEnd`、`CreditsStart` 候选。
  Lux 校验时间范围、顺序、数量和来源后原子替换 `provider_id` 等于该插件 ID 的隐藏标记；低置信度结果不落库。
- 媒体文件指纹变化时，旧检测标记失效；重新检测只在后台任务中发生。
- 检测标记不得改变媒体字节、直放 URL、运行时或用户播放进度。
- 章节来源状态按 `(media_source, plugin_id)` 持久化。新入库分集只有在本季达到门槛后才进入任务：本地 ffmpeg/chromaprint 来源至少 3 集，在线来源至少 1 集；本地新增单集可复用同季已保存的音频指纹上下文，不重复运行 ffmpeg。成功结果 30 天内不刷新；无结果 7 天后重试；失败 1 天后重试；媒体输入指纹或检测参数变化、管理员显式 `forceRefresh` 或任务重试会立即重新处理。
- 媒体库切换或清除 `chapterSourceId` 不删除历史来源标记；运行时输出只返回当前选择来源的标记，
  重新选择旧来源即可恢复其历史结果。混合库只对其中的 EPISODE 媒体条目输出片头片尾标记，电影条目不输出。

---

## 4. 明确不在当前范围

- `.strm` 的服务端 Remux、音频转码、视频转码、HLS 或媒体字节代理。
- 服务端字幕格式转换、字幕烧录、DRM 和多码率自适应 HLS。LUX-212 可以在浏览器中对已授权的本地文本字幕
  进行临时、无写回的 cue 归一化；它不是服务端转换能力。
- 在线字幕搜索、字幕下载、OCR 或服务器字幕格式转换。
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
- 无管理员资源策略、并发上限、临时目录配额和低磁盘保护的无限制在线转码。

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
- 服务重启时，遗留的未完成扫描作业标记为 `CANCELLED`；持久化游标只用于同一进程内的批次提交和
  管理员主动重试，不作为重启后的自动恢复依据。
- 全量校验期间首页继续读取上一份稳定快照；Manifest 差异按有界事务提交，只有全部可用根路径发现完成、差异安全应用和缺失确认完成后才原子替换首页快照。其他列表查询只读取已提交的数据库批次。
- 临时挂载失效不得立刻删除整个媒体库；先标记根路径不可用并暂停删除判定。

### 5.4 资源目标

- 空闲常驻内存目标小于 300 MB。
- 默认扫描时常驻内存目标小于 750 MB。
- 所有后台队列有界；队列满时合并事件或施加背压，不无限增长。
- 元数据补全使用独立的网络 I/O 并发策略：SQLite 和 PostgreSQL 的 worker 默认并发均为 4；`FILL_MISSING` 另有 4 路进程级上限，所有元数据 worker 的进程全局硬上限为 16。前台 p95、CPU 或内存压力升高时按 1/2、1/4 降档，默认值不是强制启动数。该限制独立于 TMDb 插件自身最多 16 路并发和每秒最多 32 次请求。
- ffprobe 自动进程预算为 `ceil(effective_cpu_cores × 4)`，范围 1 至 512；没有 cgroup 限额时使用标准库
  `available_parallelism`，Linux 可见的 v1/v2 quota 与 cpuset 共同取较小容量。新库将自动预算写入每库设置；
  已有每库配置保留，但运行时不能超过进程预算。`LUX_PROBE_CONCURRENCY` 数值覆盖自动进程预算。
  本地 ffprobe 的活动并发从约有效 CPU 核数起步，后续仅依据成功完成速率试探和回退，并结合近期前台延迟及内存保护；
  CPU 使用率不参与 probe 控制，进程全局硬上限为 512。ffprobe 只处理本轮 fingerprint 变化或新增的 source，未变化 source 不得重复探测。
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
- reqwest + rustls：Lux 自身需要的 HTTPS 请求；TMDb/豆瓣 HTTPS client 属于各自外置插件，不属于 Lux 核心依赖。
- tracing / tracing-subscriber：结构化日志。
- argon2：密码哈希，使用 Argon2id。
- uuid：内部 ID，优先 UUIDv7；Emby DTO 只暴露字符串。
- ffprobe：本地媒体由核心服务用于技术信息、时长和内嵌轨道；片头片尾指纹由独立章节检测后台任务提取，核心服务不读取容器内嵌章节。`.strm` 远程媒体只能由管理员显式创建的后台任务通过受监督的 `media_probe` 插件探测，不得进入用户请求路径。

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

首次安装引导允许管理员在以下两种后端中选择：

- 内置 SQLite：单文件 `/config/lux.db`，开启 WAL、外键、busy_timeout，并在后台执行受控 checkpoint。
- 外部 PostgreSQL：管理员提供已运行的 PostgreSQL 连接信息，Lux 只负责验证连接、运行迁移和使用该数据库，不负责启动或管理 PostgreSQL 服务。

默认仍推荐内置 SQLite，因为 Lux 首版是单实例、前台高读、后台短批量写入的 NAS 服务，60,000 级媒体条目在合理容量内。外部 PostgreSQL 面向需要更高并发写入、集中数据库管理或已有 PostgreSQL 基础设施的部署。

数据库选择只发生在首次初始化、创建第一个用户之前。选择 PostgreSQL 并测试成功后，Web 初始化页提供一次性的“重启 Lux”操作；Lux 在同一容器内优雅关闭并重新执行服务进程，启动时运行 PostgreSQL migrations，再继续管理员初始化。当前版本不支持已初始化实例在线切换后端，也不自动执行 SQLite 到 PostgreSQL 的数据迁移；后续如需迁移，必须提供显式导出、导入和回滚流程。

限制：

- SQLite 数据库文件必须位于容器本机持久化卷，不得放在 SMB/NFS 上。
- PostgreSQL 地址、用户名和密码属于敏感配置，不得进入日志、普通 API 响应或错误详情。
- PostgreSQL 连接失败时不得自动回退到 SQLite，避免形成两套数据。
- SQLite 和 PostgreSQL 必须各自从空数据库运行完整 migration；搜索实现可以使用后端专用索引，但不得改变 Lux API 语义。
- 数据库连接池默认上限为 SQLite 8、PostgreSQL 20；`LUX_DB_MAX_CONNECTIONS` 可在 1-100 范围内覆盖当前进程的后端连接池上限，未设置或为空时使用默认值，其他非法值必须在启动时报告配置错误。SQLite 增加连接不会改变单写者约束，PostgreSQL 部署还必须确保数据库实例和账号的连接配额足够。
- 本地文件索引并发默认 4 路；Docker 镜像和 Compose 默认注入 `LUX_SCAN_CONCURRENCY=4`、`LUX_FFMPEG_CONCURRENCY=2`。ffprobe 默认按有效 CPU 容量自动设置进程预算为 `ceil(CPU 核数 × 4)`，上限 512；Compose 未设置 `LUX_PROBE_CONCURRENCY` 时传空值启用自动预算。活动并发从约 CPU 核数起步，再按本地 ffprobe 成功完成速率自适应调整；首页延迟或 cgroup 内存压力升高时保护性降档，不依赖 CPU 使用率。管理员可通过 `LUX_PROBE_CONCURRENCY` 显式覆盖预算。`LUX_SCAN_CONCURRENCY` 只限制同一时刻活动的文件扫描任务/扫描工作项上限，不是 Tokio runtime 使用的 OS 线程总数；`LUX_PROBE_CONCURRENCY` 和 `LUX_FFMPEG_CONCURRENCY` 独立控制 ffprobe 和 ffmpeg 子进程。`LUX_SCAN_CONCURRENCY` 的范围为 1-1024，`LUX_PROBE_CONCURRENCY` 为 1-512，`LUX_FFMPEG_CONCURRENCY` 为 1-4；设置扫描环境变量后优先于媒体库保存的 `scanConcurrency`。非容器部署未设置扫描环境变量时，新建媒体库索引默认 4 路，SQLite 入库继续遵循单写者约束。

### 6.4 Docker

- 生产镜像为多阶段构建。
- 运行时包含 luxd、Web 静态资源、Jellyfin `jellyfin-ffmpeg7` v7.1.4-3 和必要 CA 证书；不安装普通 Debian `ffmpeg`。
- 以 root 用户运行，使 bind-mounted NAS 目录无需 PUID/PGID 交接或递归修改所有权即可读写。
- /config 为可写持久化卷。
- 媒体目录必须按需求以读写方式挂载，因为 Lux 要回写 NFO 和默认图片；媒体目录中的本地资源仍需可读。
  元数据策略可选择额外将 Lux 管理的 NFO 和图片写入 /config/metadata/library。
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
       Configured Storage   Background Jobs       File Streaming
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
- 元数据任务最多使用 16 路有界 worker；同一插件进程通过 request ID 多路复用 pending RPC，允许不同媒体条目的请求并行执行。
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

本地开发时由 Vite（`http://127.0.0.1:5173`）提供 React Web 页面；Rust 服务只从 `web/dist` 或显式配置的
`LUX_WEB_DIR` 提供静态文件。需要从 Rust 服务同源访问 Web 页面时，先运行 `pnpm --dir web build`。

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
- 所有列表端点分页并设置上限；Emby `/Persons` 按兼容合同接受任意正整数 `Limit`，不额外施加服务端上限。
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

- 提交密码、用户 TMDb/豆瓣 token、真实 .strm URL 或用户数据；第三方 provider 凭据只能通过受保护的插件配置或 secrets 注入，绝不写入 API、日志或版本库。
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
- device_type，可空；客户端平台（例如 `macOS`、`Windows`），旧令牌为空
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
- realtime_watch_enabled，默认开启；关闭后不创建该媒体库根目录的实时文件监控
- incremental_schedule（兼容保留，始终为空，不参与调度）
- reconciliation_schedule
- metadata_schedule，首版可为空或手动
- realtime_metadata_auto_match_enabled，默认开启；仅控制实时增量扫描完成后的受影响条目 `FILL_MISSING` 自动补全，不控制实时监听本身
- scan_concurrency
- probe_concurrency
- last_scan_at

#### library_scrapers

- library_id
- scraper_id
- position：从 0 开始，位置 0 必须是 `PRIMARY`
- role：`PRIMARY`、`SUPPLEMENT`、`BACKUP` 或 `BOTH`
- created_at、updated_at
- 唯一键 library_id + scraper_id 和 library_id + position

`libraries.scraper_id` 作为旧 API/旧任务配置的兼容镜像，始终等于 position 0 的 scraper ID；新代码以
`library_scrapers` 为事实来源。历史单值配置迁移为 position 0、`PRIMARY`。

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
- external_url：兼容字段；对 `.strm` 保存首个非空原始目标
- strm_target_kind：可空，URL、PATH、OPAQUE、EMPTY；旧数据为空时按原始目标词法回退
- is_default
- probe_status

根据已确认需求，.strm URL 需要保留并可用于播放。首版按明文保存，因为有权限的客户端仍需看到原始 Path；播放时 `DirectStreamUrl` 使用 Lux 入口，由 Lux 用播放器 User-Agent 解析有限重定向后返回 307，媒体字节不经过 Lux。必须保证数据库文件权限和日志脱敏。

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

#### media_chapters

- id
- media_source_id
- start_position_ticks
- name，可空；隐藏标记默认不设置名称
- marker_type：INTRO_START、INTRO_END、CREDITS_START
- chapter_index：同一媒体源内稳定、从 0 开始
- provider_id：检测插件 ID，非空
- confidence：范围 0 到 1，非空
- created_at、updated_at
- 唯一键 media_source_id + provider_id + marker_type

同一插件对同一媒体源最多保存三个章节标记。读取始终按 `start_position_ticks`、标记优先级和 ID
稳定排序；检测插件只能替换自己生成的隐藏标记。当前不保存容器章节、普通章节或手工章节。

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
- source_url，可空；在线图片写入时用于跨主/备用/补充来源按 URL 去重
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

#### scheduled_task_plans

- id
- task_type
- plan_name
- task_name、task_description
- source_type：SYSTEM、PLUGIN
- plugin_id，可空
- cron_or_interval，可空
- is_enabled
- resource_limit_json
- scope_type：GLOBAL、LIBRARY
- is_default
- created_at、updated_at

#### scheduled_task_plan_libraries

- plan_id
- library_id
- created_at

同一任务类型下，一个媒体库只能属于一个 `LIBRARY` 作用域的执行计划。`scheduled_task_configs.plan_id`
作为旧媒体库级配置到执行计划的镜像关系；旧 API 继续可读写，但执行计划更新必须同步镜像。

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
- 实时增量任务可以在全量调和运行期间持久化入队；文件扫描锁仍保持容量为 1，但全量任务在检测到待处理实时任务时于当前批次结束后让出锁，实时增量任务优先领取下一批。

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
  -> 小批量事务更新索引并登记受影响 source/item 目标
  -> 后台 ffprobe 只消费本次任务中新增/变化的本地 source；不探测全库或 `.strm`
  -> 按独立策略安排本地 NFO/图片、在线元数据和定向 `.strm` 插件探测
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

空字符串不应覆盖有效值。TMDb 语言回退按选定语言组顺序逐字段补全，而不是整条记录一次性切换语言；详情使用一次 `append_to_response=translations`，回退开关关闭时忽略翻译载荷，首选语言已获得的字段不会被覆盖。

TMDb 插件可选启用“原语言”模式。电影和剧集的标题优先使用 TMDb `original_title`/`original_name`，简介、tagline、网站和季/集文字从同一次详情响应的 `translations` 中选择 `original_language` 对应语言；对应翻译缺失时保留首选语言结果。启用时跳过中文标题别名替换。原语言图片按原语言、无语言、英语的顺序优先，已有详情图片在本地筛选，不因该选项重复请求详情。季/集的原语言继承父剧；插件可为冷缓存的父剧补一次详情请求，并在进程内缓存结果。

### 13.3 刮削器客户端

- TMDb 外置插件的客户端同时兼容 v3 API Key 和历史 v4 Read Access Token。管理员通过 TMDb 插件详情配置自己的 API Key。
- TMDb 插件自行决定默认凭据、管理员 API Key 和历史 token 的优先级；Lux 不内置、不解析这些凭据，也不在自身 API 或日志中返回它们。
- TMDb 插件配置包括首选语言组、语言回退开关、有序回退语言组列表和默认关闭的原语言开关，由宿主保存于 `/config/plugin-config/org.lux.tmdb.json` 并通过 `LUX_PLUGIN_CONFIG_PATH` 传给外置插件；宿主和插件都会将旧的地区 locale 归一化为 canonical 语言组，敏感字段仍不可返回。
- 主进程的元数据匹配、候选搜索、图片候选和合集请求统一通过媒体库有序刮削器协议；主进程不得直接访问第三方元数据 API。主刮削器先处理全部请求能力，备用刮削器按能力逐项接管主来源空、无效、不支持或重试失败的项目；补充刮削器只对已确认条目继续补全和合并内容，不重新决定媒体身份。
- 插件内部使用统一 HTTP client、超时、16 并发配额、每秒 32 次请求限流、重试和 User-Agent。
- 插件 stdin/stdout RPC 支持有界多路复用；响应按 request ID 分发并允许乱序返回，插件进程故障或超时会结束其全部 pending 请求。
- 404、429、5xx、网络超时分类处理。
- 搜索候选短期缓存，详情较长时间缓存。
- 响应 schema 验证后进入领域层。
- 自动匹配和手动重新匹配共用候选模型；候选的 provider ID、provider 名称和实际 scraper 来源必须一致。补充候选不得静默改变已确认的媒体身份。
- 管理员手动搜索候选时，按媒体库配置顺序查询 `PRIMARY`、`BACKUP` 和 `BOTH`，并按来源分组展示成功候选；单个来源失败不丢弃其他来源的成功结果。`SUPPLEMENT` 不参与身份候选。
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

重新匹配或元数据匹配写回电影、剧集 NFO 时，必须同时刷新该条目的本地 NFO 详情投影；详情页应在本次选择完成后直接展示新字段，不要求管理员再扫描媒体库或所在文件夹。投影由已确认的候选内容与仍有效的本地详情生成，并与候选选择结果在同一数据库事务中提交；不得在详情请求中解析 NFO。

指定条目的批量重新识别仍使用持久化任务队列：管理员一次提交 1-100 个条目，服务端去重后以 `QUEUED` 创建任务并在后台逐条处理；每条记录 `PENDING/RUNNING/COMPLETED/FAILED`、候选数量和稳定错误代码，任务通过 `GET /api/v1/admin/metadata/reidentify/{jobId}` 查询。条目级失败不会把整批伪装成基础设施失败，父任务以 `COMPLETED_WITH_ISSUES` 完成；只有任务无法收尾等基础设施故障才使用 `FAILED`。刮削器暂不可用的批次可以使用 `DEFERRED` 表示延后。该指定条目接口只负责重新搜索并生成 pending 候选，供管理员处理；失败、有问题或延后的任务可通过 `POST /api/v1/admin/metadata/reidentify/{jobId}` 重新排队未完成条目。

每个元数据任务持久化 `job_scope`（`ITEMS` 或 `LIBRARY`）和可选的 `library_id`；指定条目任务明确使用 `ITEMS`，同库条目仍记录其媒体库身份，整库任务明确使用 `LIBRARY` 和真实媒体库身份。历史任务默认按 `ITEMS` 处理，不根据条目数量推断范围。单进程内同一时刻只允许一个活动的整库元数据任务。服务重启标记为 `CANCELLED` / `SERVER_SHUTDOWN` 的元数据任务，在下次启动时恢复原任务 ID，只将未完成子项重新排队；已完成子项和管理员主动取消的任务保持原状。`FILL_MISSING` 仅在媒体库仍启用且仍选中刮削器时恢复；没有可用刮削器的库不得创建待运行任务。

自动 `FILL_MISSING` 因刮削器暂不可用而 `DEFERRED` 时，调度服务按持久化的 item 退避截止时间独立轮询并重排到期失败项，不依赖媒体库扫描或用户计划任务；只有自动来源、仍启用且选中刮削器的媒体库、未消费的带快照 `SCRAPER_UNAVAILABLE` 失败项可自动重试。每项仍按既有 5 分钟、30 分钟、最多 6 小时退避，成功后停止，持续不可用则继续延期。

自动识别结果必须区分“没有找到候选”和“找到候选但分数不足”：前者以 `NO_CANDIDATE` 和候选数 0 记录；后者以 `LOW_CONFIDENCE` 记录实际候选数，候选及媒体识别状态保留为 `PENDING` 供管理员复核。两种情况都让父任务显示 `COMPLETED_WITH_ISSUES`，条目本身仍为 `COMPLETED`。达到现有自动匹配门槛的结果仍自动确认；本任务不调整门槛或评分算法。

媒体库级“整库元数据匹配”使用同一持久化队列，但默认以 `FILL_MISSING` 自动处理：逐条先使用所属媒体库的主刮削器处理全部能力，某项能力为空、无效、不支持或重试失败时再由按顺序启用的备用刮削器接管该项；身份达到高置信度后自动选择最佳候选，再调用补充刮削器合并单值缺失项和去重后的多值项，按媒体库图像策略下载图片并原子写回 NFO/图片；低置信度条目只保留候选并进入待处理状态。新建媒体库首次扫描完成后也自动提交该队列。

实时增量扫描默认更新索引并为受影响条目提交 `FILL_MISSING` 元数据任务；媒体库关闭 `realtime_metadata_auto_match_enabled` 后才只更新索引，不再自动补全。不论开关状态，任务都只处理本次受影响且仍可用的媒体条目，不对整库重新刮削。NFO、图片和其他旁车文件的写回事件不得直接导致同一条目无限重复提交；已完整补全的条目由元数据任务跳过。

全局元数据刷新使用同一持久化队列，模式为 `FILL_MISSING` 或 `FULL_REFRESH`。仅补全只写入缺失的未锁定 NFO 字段和图片，并按主、备用、补充角色执行；完整刮削刷新主来源的未锁定在线字段，备用来源只接管主来源失败的能力，补充来源再合并去重后的多值字段和背景图，但不覆盖锁定字段、更高优先级来源或已确认身份。未配置刮削器的条目跳过在线请求并保留本地结果。

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

- 读取文件的首个非空行并 trim BOM 与首尾空白，保存为原始播放目标。
- 目标只做词法分类：HTTP(S) URL、路径、未知/其他目标；不在扫描或 PlaybackInfo 请求中访问目标。
- URL 和路径型目标都保留原始 `Path`；`PlaybackInfo` 对这两类目标使用标准带短期 Lux 播放票据的 `DirectStreamUrl`，并使用 `Protocol=File`、`IsRemote=false` 的代理兼容表示。为兼容所有可能忽略 `AddApiKeyToDirectStreamUrl` 的第三方播放器，URL/路径型目标统一将该字段设为 `true`，并从本次标准 Emby 鉴权提取用户 token 作为 `api_key` 写入签名 URL；本地文件和 SMB/FTP 解析源不携带长期 token。无论该提示取值如何，Lux 都要求绑定用户、条目和媒体源的短期 HMAC 票据，由外部代理从原始 `Path` 执行映射或 302 解析。直接请求 Lux 时，路径型目标按本地文件处理，URL 型目标仍可由 Lux 使用入站播放器 User-Agent 有限跟随重定向并返回 307；该回退不改变扫描和 `PlaybackInfo` 不访问目标的边界，含 token 的兼容 URL 不得进入公开日志。
- 下载路径按 LUX-091 使用独立的 URL 安全策略和上游流式转发，不能把路径型目标直接当作远程 URL 请求。

### 14.3 PlaybackInfo

只声明实际能力：

- SupportsDirectPlay = true。
- SupportsDirectStream 按首版实际播放入口实现返回 true；本地媒体在 Emby `PlaybackInfo`
  POST 声明 `EnableTranscoding=true` 且未启用直放、或同时声明直放/转码但由 `DeviceProfile` 确认直放
  profile 不匹配且存在 HLS 转码 profile、或携带 `forceTranscode=true` 时返回服务端转码能力；顶层布尔值
  全部省略时也按 `DeviceProfile` 协商。源容器或 codec 缺失/待探测时视为未知，不因此自动认定直放不兼容；
  URL/路径型 `.strm` 为兼容外部 Emby 播放代理返回 `SupportsTranscoding=true`，但 Lux 不为这两类源创建本地 HLS 转码会话或伪造 `TranscodingUrl`；本地媒体仍按实际 HLS 能力返回该字段。
- MediaSources 包含版本、容器、码率、大小、时长、流列表、章节和直放 URL。
- `PlaybackInfo` 响应顶层和每个 `MediaSources[]` 返回完整 `RunTimeTicks`；优先使用选中 source 的探测时长，
  缺失时回退到媒体项时长。Emby 兼容 HLS 清单按完整媒体时长生成 VOD 时间轴、完整分片列表和
  `ENDLIST`；`PlaybackInfo` 和已知总时长的完整清单只登记会话，首个 init 或媒体分片请求才替换同 source
  的旧会话、取得 FFmpeg 名额并按请求的逻辑位置启动。总时长未知时允许 `index.m3u8` 请求启动 FFmpeg 并
  回退到物理清单。各 generation 使用独立 init，init 与媒体分片并发请求跟随当前 generation；尚未生成的
  分片请求由服务端等待。Lux Web 的内部 HLS 清单仍保持动态追加和立即启动，不复用 Emby VOD 清单。
- 每个媒体版本的章节独立返回；条目级 `Chapters` 使用默认媒体源的章节。
  `IntroStart`、`IntroEnd`、`CreditsStart` 隐藏标记映射为 Emby `ChapterInfo`。
- `.strm` 的容器、时长和流列表可来自受限旁车或已完成的后台 STRM 探测；PlaybackInfo 请求本身不主动读取外部源，首次播放由 Lux 撷取上游响应头并返回 307，媒体内容仍由客户端直接访问最终地址。
- 不伪造客户端能播放的编码。
- 选择默认版本使用稳定策略，并允许客户端显式选择 source ID。

### 14.4 字幕

- ffprobe 索引内嵌字幕。
- 扫描同目录外挂字幕并识别语言、forced、default 等文件名标记。
- API 列出内嵌和外挂字幕。
- 外挂字幕可由受鉴权端点直接读取。
- Web 播放器首先尝试浏览器实际暴露的 in-band `TextTrack`；该路径不产生额外字幕请求，也不改变媒体 URL。
- 本地媒体的内嵌 SRT、ASS、SSA 在浏览器未暴露轨道时，可由 source-scoped 字幕端点按需做无转码抽取，再交给已有的
  Worker 文本解析器；不写回媒体、不烧录、不生成永久缓存。
- 远程 HTTP(S) `.strm` 的 Matroska/WebM 默认使用原生 `<video>`，保持字幕功能改动前的 Direct Play 行为。未选择字幕时不启动
  客户端 Matroska Worker、Range、MSE 或字幕专用连接。
- 用户明确选择远程内嵌 SRT/ASS/SSA 后，才由浏览器直接对媒体源 `externalUrl` 发起带 CORS 的有限 Range 读取，使用已有的客户端
  Matroska/WASM 管线解码音视频和字幕；远程音视频 fallback 的 MSE 输出、字幕 cue 和音频均由浏览器完成，Lux 不接收媒体字节。
  不会预先抽取、落盘或生成外挂文件。上游不支持 CORS/Range、索引或客户端 codec 时只报告能力不足，不回退到 Lux Relay。
- Range、索引、codec、MSE 或字幕解析失败时，只清除远程字幕并恢复同一播放计划的原生视频，不停止或重建播放会话，不切换服务端 HLS。
- PGS/SUP 图形字幕不属于本阶段承诺；完整 ASS/SSA 样式、字幕烧录和 HLS 字幕组另行处理。

### 14.5 弹幕兼容

- Lux 提供独立的 `/api/danmu/{itemId}` 和 `/api/danmu/{itemId}/raw` 读取端点，使用 Emby token 和媒体库 ACL。
- XML 来自已登记、已通过媒体根路径约束的同名旁车；请求不执行上游搜索、整库扫描或 XML 写回。
- `option=Refresh` 只刷新已登记旁车的索引；`option=GetJsonById` 作为已知 Emby 弹幕插件兼容别名，不承诺把 XML 转成通用 JSON。
- 支持弹幕接口的客户端以真实兼容性测试为准；不支持弹幕接口的客户端继续按自身能力处理或忽略该 XML。

### 14.6 Web 播放

- Web 播放通过独立的 `/api/v1/playback/sessions` 会话接口创建一次播放计划；Web API 与 Emby 播放接口、DTO 和领域类型分离。
- 会话计划使用 `tier: 0..4` 和 `plan.kind: DIRECT | SERVER_HLS | UNSUPPORTED` 的判别联合；普通 Direct Play 和 HLS 地址为短期签名 URL，不能要求 `<video>` 或 HLS 请求携带 Lux Cookie；路径型 `.strm` 的 `DIRECT` 计划继续额外返回标准 `/Videos/...` `proxyUrl`，Web 播放器优先使用它并在代理鉴权/映射失败时回退到签名 `url`。远程 HTTP(S) `.strm` 的 Web 媒体则直接使用 `MediaSources[].externalUrl`，忽略 `proxyUrl`、`rangeUrl` 和 Lux Direct。
- 档位 0 使用原生 Range 直放或现有客户端 fallback；档位 1～4 使用服务端 fMP4/CMAF HLS。Safari 使用原生 HLS，其他支持 MSE 的浏览器使用 Web HLS 播放器。
- 创建会话时固定媒体源、音频/字幕选择、起播位置和服务端计划；seek 必要时切换会话生成代次，不把客户端任意路径或外部 URL 交给服务端执行。
- `.strm` 只能返回档位 0；URL/路径型外部代理接管、URL 型 Lux 直连回退或本地安全读取失败时直接展示错误，不创建 ffmpeg 进程。
- 内嵌文本字幕是独立于媒体计划的能力：浏览器原生轨道或客户端 overlay 只能复用当前媒体资源，不能改变 `.strm` 的
  Direct 规则。远程 HTTP(S) STRM 的客户端读取必须直连 `externalUrl`，不使用 Lux 的 `rangeUrl`。
- 记录开始、定时进度、暂停、心跳和停止；事件带有幂等 `eventId` 与单调 `sequence`，服务端使用数据库媒体时长计算已看状态。
- 服务端 HLS 会话必须有界：独立进程组、stderr drain、临时目录配额、Remux/硬件/软件并发限制、心跳超时回收、孤儿目录清理和低磁盘拒绝策略。
- 不实现 DRM、服务器字幕转换/烧录、多码率自适应 HLS 或 `.strm` 服务端代理。LUX-212 的浏览器文本 cue
  归一化不生成或写回媒体文件，不能扩展为服务端转码能力。
- LUX-203 至 LUX-208 建立 LuxPlayer 的独立产品层；Web 弹幕、完整 ASS/SSA 渲染和更复杂的字幕/轨道能力只能在对应任务中实现。
- LUX-184 允许提供独立的浏览器媒体能力探针，用于实测原生 video、MediaCapabilities 和 WebCodecs；探针不接入
  正式播放路径，不读取或保存用户媒体数据。
- LUX-185 可为 MP4/fMP4 的 HEVC 媒体增加浏览器端 WASM 解码、H.264 客户端编码和 MSE 播放 fallback；重型工作
  必须在 Web Worker 中执行。本地媒体继续使用 Lux 的受保护 Range；远程 HTTP(S) `.strm` 的原始媒体字节由浏览器
  直接从 `MediaSources[].externalUrl` 读取，要求上游自行提供 CORS、Range 和稳定资源校验，Lux 不作为媒体代理。
- 客户端解码增强的目标包括具备相应硬件能力的 4K HEVC 8-bit、10-bit 和 HDR10；Dolby Vision 不属于当前承诺。
- 若后续新增 WebCodecs 或 WASM 播放引擎，必须单独修改本节、补充 ADR，并通过实际浏览器性能阶段门；不得把
  “浏览器报告支持”直接等同于 4K 实时播放能力。

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
- GET /Users/Query
- GET /Items/Counts
- GET /Items
- GET /Items/Filters2，若目标客户端实际调用
- GET /Shows/{Id}/Seasons
- GET /Shows/{Id}/Episodes
- GET /Shows/NextUp
- GET /Persons?ParentId={LibraryId}&Recursive=true&PersonTypes=Actor
- GET /Search/Hints
- GET/HEAD /Items/{Id}/Images/{Type}
- GET/HEAD /Items/{Id}/Images/{Type}/{Index}
- GET/POST /Items/{Id}/PlaybackInfo
- GET /Library/MediaFolders?LibraryId={LibraryId}&StartIndex=0&Limit=100
- POST /Items/{FolderId}/Refresh

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
- `GET /Items` 的 `Ids` 严格匹配条目 ID；为兼容使用媒体源 ID 查询路径的 Emby 代理，也可匹配 `MediaSourceId` 并返回其所属条目。完全未知的 ID 返回空列表，不得回退为未过滤目录页。
- `GET /Items/{Id}` 的 `Id` 正常匹配媒体条目；为兼容使用媒体源 ID 获取详情的 Emby 代理，也可将 `MediaSourceId` 解析到所属条目并返回该条目的 `MediaSources`。完全未知的 ID 返回 404，不得返回其他条目。
- IncludeItemTypes、ExcludeItemTypes。
- Recursive。
- StartIndex、Limit。
- Emby `GET /Items` 与 `GET /Users/{UserId}/Items` 在 `EnableTotalRecordCount=true` 时接受 `Limit=0` 作为只计数请求，返回空 `Items` 和完整 `TotalRecordCount`；未启用总数（未传标志或为 `false`）时，`Limit=0` 返回最多 1000 条的首个分页，以兼容将零值用作未分页请求的 Emby 客户端。其他 `Limit` 仍须为 1..=1000。
- SortBy、SortOrder。
- Filters、IsPlayed、IsFavorite。
- Years。
- Fields。
- EnableImages、ImageTypeLimit。
- `GET /Library/MediaFolders` 只返回已经建立的物理 `FOLDER` 条目，使用 `LibraryId` 或 `ParentId` 筛选并按 `StartIndex`/`Limit` 分页；`POST /Items/{FolderId}/Refresh` 将具体 FOLDER ID 映射到其媒体库根目录和相对路径，只创建局部 `INCREMENTAL_SCAN`。媒体库 ID 可作为未解析到具体 FOLDER 时的根目录级增量兜底，不得退化为同步整库扫描。
- `/Persons` 使用 `ParentId` 指定媒体库；`Recursive=true` 聚合媒体库所有后代媒体条目，`Recursive=false` 只聚合直接子条目，未传 `Recursive` 时按递归查询处理以兼容旧客户端；`PersonTypes` 包含 `Actor` 时返回去重后的演员。`SortBy=SortName` 与 `SortBy=Name` 都按人物姓名排序，另支持 `DateCreated`。人物 DTO 使用 `Type=Person`，并提供 `ServerId`、`ImageTags`、`BackdropImageTags`。响应必须保持 Emby 的 `Items`、`TotalRecordCount` 结构且不额外返回 `StartIndex`；接受任意正整数 `Limit`，不额外施加服务端上限；`Fields`、`SortBy`、`SortOrder` 必须在数据库分页前生效；`DateCreated` 使用演员首次出现在该媒体库媒体条目中的最早 `added_at`。人物关系由持久化索引提供，不能在请求中扫描 metadata 目录。
- TotalRecordCount 与 Items 的一致性。

人物详情兼容合同：

- `GET /Persons/{PersonIdOrName}` 与 `/emby/Persons/{PersonIdOrName}` 返回单个人物 DTO；路径参数优先按
  人物 ID 匹配，未匹配时按精确人物姓名匹配。两条路径使用与 `/Persons` 相同的 `Name`、`ServerId`、`Id`、
  `Type`、`ImageTags`、`BackdropImageTags` 结构，并按 `Fields` 投影 `Overview`、`Role`、`BirthDate`、
  `DeathDate`、`KnownForDepartment`、`PlaceOfBirth`、`DateCreated`。
- 人物详情只从持久化人物关系索引读取，不在请求中扫描 metadata 目录、解析 NFO 或调用 TMDb；人物没有
  图片时仍返回 JSON，图片标签为空，调用方可以使用占位图。
- 人物查询遵守当前 Emby 用户的媒体库 ACL；没有任何可访问媒体库中的出演关系时返回 `404`。

Limit 默认 50；Emby `/Persons` 接受任意正整数，不设置服务端硬上限，其他列表接口继续遵循各自的服务端上限。

### 15.5 核心 DTO

BaseItemDto 至少按场景提供：

- Id、ServerId、Name、SortName、OriginalTitle。
- Type、MediaType、IsFolder、ParentId、SeriesId、SeasonId。
- IndexNumber、ParentIndexNumber。
- Overview、ProductionYear、PremiereDate、RunTimeTicks。
- ProviderIds。
- ImageTags、BackdropImageTags。
- UserData：Played、PlaybackPositionTicks、IsFavorite、PlayCount；Series/Season 另提供按当前用户统计的 `UnplayedItemCount`。
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

Lux 自有 `/api/v1` 的媒体、搜索、首页、图片、播放和用户状态接口除 Web session 外，接受同一用户的
Emby AccessToken：推荐使用 `X-Lux-Token`，并兼容 `X-Emby-Token`、`X-MediaBrowser-Token` 和
`Authorization: Bearer`。令牌仍按用户执行媒体库 ACL；显式令牌请求不依赖 Cookie CSRF。`X-Lux-Api-Key`
和 `api_key` 查询参数保留给 LUX-182 共享管理员 API Key，不授予普通用户管理员权限。

---

## 16. Lux 自有 API

Web 和管理控制台使用 /api/v1，不直接依赖 Emby DTO；第三方 Lux 客户端可以使用用户级客户端令牌调用
同一份 Lux JSON 合同。

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
- GET /api/v1/libraries/{id}/items（支持 `metadataStatus=PENDING` 待确认筛选）
- GET /api/v1/items/{id}
- GET /api/v1/people
- GET /api/v1/people/{personId}
- GET /api/v1/people/{personId}/items
- GET /api/v1/search
- GET /api/v1/items/{id}/playback
- POST /api/v1/items/{id}/progress
- PUT /api/v1/items/{id}/favorite
- GET /api/v1/people/{personId}
- PUT /api/v1/people/{personId}/favorite

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
- GET /api/v1/admin/metadata/pending（兼容接口；Web 控制台通过媒体库待确认筛选处理）
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
`DELETE /api/v1/admin/items/{id}` 未指定 `sourceId` 时，若 `{id}` 是剧集，则删除该剧集及其季度、分集树下的全部本地/STRM 媒体源和同名旁车文件，并标记整棵层级移除；指定 `sourceId` 时仍只删除当前条目下的该媒体源。

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
- 账户设置可调整首页媒体库顺序；顺序按用户持久化到服务端，并同步用于 Web 与 Emby 兼容视图。
- 管理员可以在服务器设置中开启“媒体库顺序强制按照管理员排序”。普通用户的个人设置默认开启“按照管理员顺序排序”；开启时使用管理员账号的首页媒体库顺序，关闭后可以使用自己的顺序。服务器强制项开启后，普通用户的该项始终有效且不可取消，Web 与 Emby 兼容视图都必须使用管理员顺序。
- 媒体库列表：类型、年份、已看、收藏筛选；名称、最近添加、发行日期、评分排序。
- 搜索结果。
- 演员搜索结果可进入人物详情；人物详情显示当前用户有权限访问的全部参演电影和剧集，分
  页加载并按发行日期倒序。分集出演关系聚合为所属剧集，同一剧集只展示一次。
- 电影详情：海报、背景、简介、年份、时长、版本、字幕信息、播放、收藏。
- 电影和剧集详情显示本地 NFO 或所选刮削器提供的主要演员；演员姓名和角色不要求存在 provider ID，
  无头像时显示姓名首字母占位。已确认的人物身份和头像使用规范人物资源，可由 TMDb、IMDb、豆瓣等
  多个 provider 身份共同引用；已确认人物的可用简介、出生/去世日期、出生地和职业领域也保存到人物资料，
  未确认身份的演员只保存出演关系，不创建人物目录或发起人物图片请求。
- 剧集详情：季度、单集、下一集、进度。
- 合集详情。
- Web 播放页。
- 账户和当前设备会话。

### 17.3 管理页面

- 仪表盘。
- 媒体库列表和编辑；媒体库卡片封面可单击直接打开编辑弹窗。
- 全局策略：元数据、图像和字幕默认值，刮削模式，以及应用范围和存储预估。
- 路径选择/输入、读写检测。
- 扫描计划与元数据计划，明确分开。
- 扫描/任务页。
- 任务与日志页按“任务类型 → 执行计划”集中查看所有计划、运行记录和脱敏日志。任务类型只能由 Lux 系统或插件注册；管理员只能维护已注册类型下的执行计划，不能凭空创建任务类型。
- 空库初始没有计划。创建媒体库时由系统原子注册“全量校验媒体库”和“元数据刮削”任务类型，并将媒体库加入匹配的默认执行计划；插件安装或启用后注册插件提供的任务类型和默认计划。每个执行计划支持立即执行、独立 Cron、启停和资源配置；实时增量扫描由文件系统监听触发，不注册为计划任务。
- 同一任务类型下，一个媒体库只能属于一个执行计划。一个计划触发后按媒体库创建独立运行任务，运行记录仍可分别取消、重试和查看；共享全量扫描资源的计划按现有扫描锁串行执行。
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
- AUTO_LIBRARY_COVER（每个媒体库首次达到海报阈值时注册并自动执行一次；注册后与其他任务一样支持管理员手动执行和 Cron 计划重跑）
- DANMAKU_MATCH（全局插件注册任务；每次按所选媒体库创建弹幕匹配作业）
- WRITE_DANMAKU_XML
- REBUILD_SEARCH
- PURGE_MISSING

任务使用 dedupe_key，例如 library_id + normalized_path + job_type。重复事件合并。

管理员计划使用独立的计划 ID 作为调度游标键；计划到点只产生一次批次语义的派发请求，目标媒体库
仍各自进入对应运行队列。全量扫描索引默认最大并发为 1；一个库完成索引后即可释放共享索引资源，
其后处理可以与下一个库的索引重叠。实时增量扫描在批次边界优先领取资源；计划未完成时下一次触发
不得重复创建同一媒体库的活动运行任务。

### 18.2 重试

- 本地确定性错误，如 XML 格式错误：不无限重试，进入失败并等待文件变化或人工操作。
- 临时 I/O、TMDb 429/5xx：指数退避加随机抖动。
- 权限错误：立即失败并在控制台突出显示。
- 最多尝试次数按任务类型配置。

### 18.3 日志

- JSON 结构化日志为默认容器输出。
- Lux 同时将同一份 JSON 结构化日志按 UTC 日期写入配置目录的 `logs/lux.YYYY-MM-DD.log`；日志目录随 `/config` 持久化，容器重启后保留历史文件。管理员选择单日时下载原始 `.log` 文件，选择多日时下载包含每日文件的 ZIP。
- 活动 JSONL 段达到 50 MiB 或 UTC 日期切换后会压缩到 `/config/logs/archive/`；归档包最多保留 20 个，新包成功验证后才删除最旧包。管理员日志导出按日期合并活动文件和归档分段。
- 字段包含 timestamp、level、target、requestId、jobId、libraryId、itemId、errorCode、durationMs。
- 不记录密码、token、Cookie、完整外部 URL。
- 路径在管理员日志中可显示相对路径；对普通用户不显示磁盘路径。
- 登录失败以适合 Fail2Ban 或其他日志工具解析的稳定事件码记录。
- 管理员可以按 UTC 起止日期导出日志；单日返回原始 `.log` 文件，多日返回 ZIP；导出最多覆盖 31 天，只包含已存在的日文件，不提供普通用户访问。

### 18.4 健康

- /health/live：进程事件循环可响应。
- /health/ready：数据库迁移完成、配置可读、必要目录可访问。
- 管理健康页额外检查 SQLite WAL、任务延迟、根路径状态和 ffprobe 可用性；具体 metadata provider 的状态通过插件管理接口查看。

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
- 验证服务重启时未完成作业被取消且不会自动恢复；管理员主动重试仍可重新排队。

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
      LUX_SCAN_CONCURRENCY: "2"
      LUX_FFMPEG_CONCURRENCY: "2"
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

迁移是后续增强，不阻塞首版。该能力以独立插件 `org.lux.emby-migration`
提供，方向固定为 Emby → Lux，永远不实现 Lux → Emby。

优先迁移：

- 一个或多个用户的用户资料、启用/禁用状态和媒体库访问权限。
- 已看状态、播放位置、播放次数、最近播放时间和收藏。
- 用户级人物/演员收藏；通过 Emby Person 的 TMDb、IMDb、TVDb 或其他 Provider ID 匹配，缺少身份时按规范化姓名唯一匹配，冲突和无法匹配的条目进入迁移报告。
- 如果当前 Emby 版本通过公开 API 提供原始播放事件，则迁移按时间排序的播放历史事件；
  不得用条目聚合状态伪造历史事件。

策略：

- 不直接读取或修改 Emby 内部数据库。
- 通过管理员 API key 调用公开 Emby API；插件只运行在独立受监督进程中。
- Emby 基础地址、API key 和局域网访问许可在 `org.lux.emby-migration` 插件设置页面配置；连接测试、迁移选项、任务进度和报告也全部在该插件配置页面操作，不设置独立的 Emby 迁移控制台入口。API key
  作为敏感插件配置保存，不进入普通 API 响应、日志或插件包。测试连接或创建任务时，宿主读取并校验
  插件配置；创建任务会将经过校验的来源快照保存到该任务的受保护 secret，插件调用时临时接收。
- 用户不需要手动逐个创建 Lux 账户；插件按规范化用户名自动创建并绑定用户。
- Emby 密码不能从公开 API 读取。Lux 创建待迁移密码账户，用户首次登录时由插件向 Emby
  验证原密码，成功后只在 Lux 本地写入新的 Argon2 哈希；原密码不持久化。
- Emby 管理员不因迁移自动获得 Lux 管理员权限。
- 使用 TMDb ID、其他 provider ID，其次规范化标题+年份映射 Lux item。
- 不能唯一匹配的记录输出报告，不自动猜测。
- 媒体库、用户和条目映射不唯一时进入报告；可在预览阶段修正后再执行。
- 默认采用合并策略：播放次数取较大值，播放状态按较新的最近播放时间合并，收藏和已看状态合并；
  同时提供覆盖和跳过选项。
- 导入幂等，可 dry-run，可取消、重试和从检查点恢复。
- 迁移任务、用户映射、条目匹配、导入记录和（若可用）播放事件均必须持久化；历史播放事件
  不得塞入 `user_item_state` 聚合表。

本地 NFO 和图片通过扫描自然继承，不需要迁移工具复制。

公开 Emby API 的历史能力必须在 LUX-190 阶段用受控实例和脱敏 fixture 验证。插件和宿主必须
声明能力等级：`ITEM_STATE` 表示只能迁移条目状态，`EVENT_HISTORY` 表示返回真实原始事件。
若源端不支持 `EVENT_HISTORY`，迁移结果明确显示“历史时间线不可用”，但不阻塞其他状态导入。

迁移插件不得连接未经管理员确认的任意地址。Emby 基础地址只接受 HTTP(S)、禁止凭据/查询参数/片段；
宿主执行超时、响应大小、重定向、解析结果和出站网络策略校验。管理员显式允许局域网 Emby 时，
才允许访问私网地址。

---

## 23. 架构决策记录

项目初始化时把以下决定分别写入 docs/decisions。

### ADR-001：模块化单体

- 状态：建议接受。
- 决定：首版单进程、单数据库，通过 Rust 模块隔离。
- 原因：NAS 部署简单、事务清晰、Codex 分步开发更容易。
- 否决：微服务会增加部署、网络和一致性成本。

### ADR-002：SQLite WAL（默认后端）

- 状态：建议接受。
- 决定：内置数据库模式使用 SQLite WAL，数据库必须位于本机卷；它仍是默认后端，但不再是唯一允许的后端。
- 原因：单机、高读低并发写、低运维。
- 风险：单写者；通过短事务、批量和写入配额缓解。需要更高并发写入的部署可在首次引导选择外部 PostgreSQL。

### ADR-003：独立 Emby 兼容边界

- 状态：必须接受。
- 决定：Emby 路由/DTO 与 Lux API/领域模型分离。
- 原因：兼容怪癖不能反向污染核心设计。

### ADR-004：直放优先的 Web 播放

- 状态：已接受；服务端播放细节由 ADR-026 补充。
- 决定：Web 播放使用 0～4 档，始终先尝试档位 0 原始 Range 直放；本地媒体必要时按顺序使用档位 1 Remux、
  档位 2 音频转码、档位 3 硬件转码或档位 4 软件转码。服务端 HLS 只使用会话级临时资源，不生成永久副本。
- `.strm` 永远只允许档位 0；直连失败时返回明确错误，不进入服务端 Remux、转码、HLS 或媒体字节代理。
- 运行时统一使用 Jellyfin 官方 `jellyfin-ffmpeg` FFmpeg 7 正式版，普通 Debian `ffmpeg` 不安装。
- 后果：本地媒体覆盖更多浏览器格式，但服务端需要会话签名、进程组、并发、磁盘配额和生命周期回收治理。

### ADR-005：本地元数据为默认来源

- 状态：已由需求确认。
- 决定：本地 NFO/图片始终读取；默认和“仅补全”只补缺失内容，显式“完整刮削”才刷新未锁定 NFO 字段并替换图片；锁定的 NFO 字段始终保留。
- 后果：媒体目录必须读写，写回可靠性成为核心功能。

### ADR-006：React Web 客户端

- 状态：待项目所有者确认。
- 决定：核心服务端 Rust；Web 使用 React/TypeScript。
- 原因：浏览器生态与开发效率。
- 替代：Leptos/Yew，全 Rust 但前端生态和调试成本更高。

### ADR-014：统一元数据资源目录

- 状态：已由本任务接受。
- 决定：Lux 管理的图片、人物资料和后续对象资源统一放入 `/config/metadata`；数据库继续负责
  关系和查询，媒体目录中的 NFO/本地图片仍按 ADR-005 作为字段级来源。
- 后果：新布局必须支持旧 `/config/people` 只读兼容、原子写入、路径校验和可重建迁移。

### ADR-028：元数据 provider 与宿主实现彻底解耦

- 状态：已接受；由 LUX-201 实施。
- 决定：Lux 主程序只依赖 provider-neutral 的 metadata RPC 和插件目录契约；TMDb、豆瓣以及其他上游
  服务的 HTTP client、endpoint DTO、凭据读取、语言策略和图片 URL 转换全部属于各自的外置插件。
- 兼容：`tmdb`、`douban` 等 provider namespace 可以继续出现在 NFO、Emby DTO、历史 provider ID 和
  旧 `scraperId` 中，但只能由通用兼容层按字符串处理；它们不是主程序的 client、配置或网络探针依赖。
- 配置：宿主为每个插件生成并传递专属 `LUX_PLUGIN_CONFIG_PATH`，metadata 插件不得获得整个 Lux 配置根目录。
  旧共享配置在首次发现/启动时迁移到对应插件配置文件，迁移成功后不再由宿主读取上游专属字段。
- 版本：协议 v1 的 metadata 方法保持不变；TMDb 与豆瓣插件分别在解耦发布中增加一个 patch 版本。
- 原因：避免新增 provider 时修改核心依赖、数据库模型和应用服务，也避免插件读取无关凭据。

### ADR-032：内嵌文本字幕的浏览器优先与远程 STRM 隔离

- 状态：已接受；由 LUX-224 实施。
- 决定：Web 播放器先使用浏览器暴露的 in-band `TextTrack`。浏览器未暴露轨道时，本地媒体允许通过现有
  source-scoped 字幕端点按需抽取原始 SRT/ASS/SSA，复用 Lux 的 Worker 文本解析器；远程 `.strm` 不由 Lux
  拉取媒体或提供字幕代理接口。
- 远程 `.strm` 只有在浏览器本身支持内嵌字幕，或实验性的单次 `fetch`/MSE/Worker 媒体管线满足 CORS、Range、
  鉴权和生命周期条件时才尝试显示。实验失败必须回到原有 Direct Play，不得为了字幕切换到 Lux HLS、服务端
  代理或额外的远程媒体连接。
- PGS/SUP、服务器字幕烧录、HLS 字幕组、完整 ASS/SSA 样式和字幕写回不属于本决定。内嵌字幕选择不进入 Web
  播放会话创建请求，不影响 tier、媒体 URL、进度、心跳或停止接口。
- 原因：浏览器内部媒体管线可能拥有页面不可见的字幕数据；把远程 `.strm` 拉回 Lux 会破坏直连、User-Agent
  绑定和单连接语义，并扩大隐私、SSRF 和资源风险。浏览器能力必须以真实运行时暴露结果为准，不能从 ffprobe
  的轨道枚举推断浏览器一定可渲染。
- 后果：本地文本字幕可以安全提供可控的 Lux fallback；远程 `.strm` 字幕是能力型支持，不承诺所有浏览器，
  且不依赖 302/Redia 的字幕专用合同。兼容性记录必须分别记录视频请求和字幕数据来源。

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
| LUX-150 | src/application/danmaku.rs、src/application/plugin_protocol.rs、src/application/plugin_runtime.rs、src/application/plugins.rs、src/storage/、src/api/mod.rs、src/bin/lux-plugin-danmaku.rs、plugins/org.lux.danmaku/、migrations/、scripts/、tests/、docs/ |
| LUX-151 | src/application/ip_location.rs、src/api/mod.rs、tests/、web/src/features/admin/、web/src/lib/api/、docs/ |
| LUX-153 | src/application/admin_events.rs、src/api/mod.rs、tests/admin_events.rs、web/src/features/admin/、web/tests/、docs/ |
| LUX-154 | src/application/scanner.rs、src/storage/mod.rs、migrations/、tests/scanning_jobs.rs、docs/LUX-DEVELOPMENT.md |
| LUX-187 | src/application/admin_events.rs、src/application/scanner.rs、src/storage/mod.rs、src/api/mod.rs、migrations/、tests/、web/src/components/layout/、web/src/features/activity/、web/src/features/admin/、web/src/lib/api/、web/src/react.css、docs/ |
| LUX-156 | src/observability/、src/main.rs、src/api/mod.rs、Cargo.toml、Cargo.lock、tests/observability.rs、tests/log_export.rs、web/src/features/admin/、web/src/lib/api/、web/tests/、docs/ |
| LUX-158 | src/application/strm_target.rs、src/application/、tests/strm_target.rs、docs/ |
| LUX-160 | src/application/plugin_protocol.rs、src/application/plugins.rs、src/api/mod.rs、tests/、docs/ |
| LUX-161 | src/application/strm_target.rs、src/api/mod.rs、tests/、docs/ |
| LUX-162 | src/application/plugin_store.rs、src/application/plugin_runtime.rs、src/application/plugins.rs、src/api/mod.rs、web/src/features/admin/、web/src/lib/api/、tests/、docs/ |
| LUX-164 | src/application/metadata_paths.rs、src/application/people.rs、migrations/（后续对象关系）、tests/、docs/ |
| LUX-165 | src/application/images.rs、src/application/library_covers.rs、src/api/mod.rs、tests/、docs/ |
| LUX-166 | src/application/metadata_paths.rs、tests/metadata_paths.rs、docs/ |
| LUX-167 | src/application/metadata_objects.rs、src/application/collections.rs、src/api/mod.rs、tests/、docs/ |
| LUX-168 | src/application/metadata.rs、src/application/nfo.rs、src/application/scraper.rs、src/application/tmdb.rs、src/application/tmdb_plugin.rs、src/application/candidates.rs、src/bin/lux-plugin-tmdb.rs、tests/、docs/ |
| LUX-169 | plugins/org.lux.tmdb/manifest.json、src/application/plugins.rs、src/application/plugin_store.rs、scripts/package-tmdb-plugin.sh、Dockerfile、tests/、docs/ |
| LUX-170 | src/application/nfo.rs、src/application/metadata.rs、src/application/people.rs、src/application/scanner.rs、src/api/mod.rs、web/src/features/detail/、tests/、docs/ |
| LUX-171 | Cargo.toml、Dockerfile、docker-entrypoint.sh、src/application/plugins.rs、src/application/plugin_store.rs、src/bin/、plugins/、scripts/、tests/、web/、docs/ |
| LUX-172 | migrations/、migrations-postgres/、src/application/nfo.rs、src/application/metadata.rs、src/application/scanner.rs、src/storage/、src/api/mod.rs、web/src/features/detail/、web/src/lib/api/types.ts、tests/、docs/ |
| LUX-177 至 181 | migrations/、migrations-postgres/、src/library.rs、src/storage/、src/application/libraries.rs、src/application/plugins.rs、src/application/chapter_detector.rs、src/api/mod.rs、web/src/features/admin/、web/src/lib/api/、tests/、docs/ |
| LUX-182 | src/auth/、src/api/mod.rs、web/src/features/account/、web/src/lib/api/、tests/、docs/ |
| LUX-183 至 186 | src/application/webhooks.rs、src/storage/、src/api/mod.rs、migrations/、migrations-postgres/、tests/、docs/ |
| LUX-184 | web/public/media-capability-probe.html、web/public/media-capability-probe.js、web/tests/、docs/ |
| LUX-185 | web/src/features/player/、web/public/hevc/、web/tests/、web/package.json、web/pnpm-lock.yaml、web/vite.config.ts、docs/ |
| LUX-186 | src/application/plugins.rs、src/api/lux/mod.rs、src/api/mod.rs、tests/plugins.rs、web/src/features/admin/、web/src/lib/api/、web/tests/、docs/ |
| LUX-188 | migrations/、migrations-postgres/、src/storage/mod.rs、src/application/people.rs、src/api/mod.rs、tests/people_api.rs、docs/ |
| LUX-189 | src/application/watch.rs、src/application/reidentify.rs、src/application/images.rs、src/storage/mod.rs、migrations/、migrations-postgres/、web/src/features/admin/、web/src/react.css、tests/、web/tests/、docs/ |
| LUX-190 | docs/LUX-DEVELOPMENT.md、docs/LUX-190-PLAN.md、docs/decisions/022-emby-migration-plugin.md、docs/COMPATIBILITY.md |
| LUX-191+ | src/application/emby_migration*.rs、src/storage/emby_migration.rs、src/api/mod.rs、src/auth/users.rs、migrations/、migrations-postgres/、docs/LUX-191-PLAN.md |
| LUX-193 | migrations/、migrations-postgres/、src/storage/mod.rs、src/api/mod.rs、web/src/features/detail/、web/src/lib/api/、tests/people_api.rs、web/tests/、docs/ |
| LUX-194 | src/application/catalog.rs、src/application/people.rs、src/storage/mod.rs、src/api/mod.rs、web/src/features/search/、web/src/features/detail/、web/src/lib/api/、tests/、docs/ |
| LUX-195 | src/application/scraper.rs、src/application/tmdb_plugin.rs、src/application/plugin_protocol.rs、src/application/plugins.rs、src/application/candidates.rs、src/application/reidentify.rs、src/application/images.rs、src/application/collections.rs、tests/、docs/ |
| LUX-196 | migrations/、migrations-postgres/、src/library.rs、src/storage/mod.rs、src/application/libraries.rs、src/application/scraper.rs、src/application/candidates.rs、src/application/reidentify.rs、src/application/metadata.rs、src/api/mod.rs、web/src/features/admin/、tests/、web/tests/、docs/ |
| LUX-198 | runtime/Dockerfile、Dockerfile、docker-bake.hcl、src/application/playback/、src/api/lux/、src/storage/、migrations/、migrations-postgres/、web/src/features/player/、web/src/lib/api/、tests/、web/tests/、docs/ |
| LUX-199 | src/application/catalog.rs、src/storage/mod.rs、src/api/mod.rs、tests/、docs/ |
| LUX-202 | src/application/images.rs、src/application/nfo.rs、src/api/mod.rs、web/src/features/admin/、web/src/lib/api/、tests/、web/tests/、docs/ |
| LUX-203 | docs/LUX-DEVELOPMENT.md、docs/decisions/029-luxplayer.md、docs/THIRD-PARTY-NOTICES.md |
| LUX-204 | web/src/features/player/core/、web/tests/；定义 LuxPlayer 状态、命令和引擎契约 |
| LUX-205 | web/src/features/player/core/、web/src/features/player/PlayerPage.tsx、web/tests/；接入现有 Web 播放会话 |
| LUX-206 | web/src/features/player/、web/tests/；拆分 LuxPlayer UI 与播放页面 |
| LUX-207 | web/src/features/player/、web/tests/；实现来源可追溯的手势、自动隐藏和时间轴交互 |
| LUX-208 | web/src/features/player/、web/tests/、docs/COMPATIBILITY.md；Media Session、移动端安全区和兼容性收尾 |
| LUX-209 | web/src/features/player/、web/tests/、docs/；ArtPlayer 风格控制层与无数据管道的弹幕可见性 UI |
| LUX-578 | migrations/、migrations-postgres/、tests/storage.rs、docs/LUX-DEVELOPMENT.md；增加逐任务唯一媒体进度台账 |
| LUX-579 | src/storage/jobs/scan/manifest/discovery/presence.rs、src/storage/jobs/scan/lifecycle.rs、src/storage/repository.rs、tests/scanning_jobs.rs、docs/LUX-DEVELOPMENT.md |
| LUX-580 | src/storage/jobs/scan/lifecycle.rs、src/storage/repository.rs、src/application/scanner/tests.rs、src/storage/repository_tests.rs、docs/LUX-DEVELOPMENT.md |
| LUX-581 | src/application/admin_task_activity.rs、src/api/admin/admin_jobs.rs、src/api/admin/admin_scheduled_tasks.rs、tests/、docs/LUX-DEVELOPMENT.md |
| LUX-582 | web/src/lib/api/types.ts、web/src/features/admin/AdminOperationsPage.tsx、web/tests/admin-operations.test.tsx、docs/LUX-DEVELOPMENT.md |
| LUX-583 | web/src/features/activity/ScanActivityPopover.tsx、web/tests/scan-activity.test.tsx、docs/LUX-DEVELOPMENT.md |
| LUX-210 | docs/LUX-DEVELOPMENT.md；关闭 LuxPlayer 核心阶段并定义字幕、弹幕后续边界 |
| LUX-211 | src/api/mod.rs、tests/、web/src/features/player/、web/tests/、docs/；将字幕轨绑定到当前媒体源并实现原生 WebVTT 生命周期 |
| LUX-212 | web/src/features/player/、web/tests/、docs/THIRD-PARTY-NOTICES.md；Lux 自有的安全文本字幕解析与渲染 |
| LUX-213 | src/api/mod.rs、web/src/lib/api/、tests/、docs/；独立的 Lux Web 弹幕读取合同 |
| LUX-214 | web/src/features/player/、web/tests/、docs/THIRD-PARTY-NOTICES.md；Lux 自有弹幕解析、调度、渲染与控制层整合 |
| LUX-215 | web/src/features/player/、web/tests/、docs/COMPATIBILITY.md；字幕/弹幕跨引擎、性能与真实浏览器阶段门 |
| LUX-216 | docs/LUX-216-PLAN.md、docs/LUX-DEVELOPMENT.md；核验剩余 ArtPlayer 默认交互并定义阶段 18 |
| LUX-217 | web/src/features/player/、web/tests/、docs/THIRD-PARTY-NOTICES.md；循环、画面比例和镜像设置 |
| LUX-218 | web/src/features/player/、web/tests/、docs/THIRD-PARTY-NOTICES.md；原生 VTT 与 Lux 文本字幕偏移 |
| LUX-219 | web/src/features/player/、web/tests/、docs/THIRD-PARTY-NOTICES.md；AirPlay 能力门和 mini progress bar |
| LUX-220 | src/api/mod.rs、web/src/lib/api/types.ts、tests/chapters.rs、docs/；Lux source-scoped 章节合同 |
| LUX-221 | web/src/features/player/、web/tests/、docs/THIRD-PARTY-NOTICES.md；章节时间轴与片头跳过体验 |
| LUX-222 | scripts/player-danmaku-smoke.mjs、web/tests/、docs/COMPATIBILITY.md；阶段 18 真实浏览器和全量质量门 |
| LUX-223 | src/api/、src/storage/、src/application/people/、docs/；内部领域模块化重构，不改变公共协议或数据库模型 |
| LUX-224 | docs/LUX-DEVELOPMENT.md、docs/decisions/032-embedded-text-subtitles.md；内嵌文本字幕合同与远程 .strm 边界 |
| LUX-225 | src/storage/repository.rs、src/storage/jobs.rs、src/storage/mod.rs、tests/subtitles.rs；source-scoped 字幕流查询 |
| LUX-226 | src/application/embedded_subtitle.rs、src/application/mod.rs、src/api/media.rs、tests/subtitles.rs；本地内嵌文本字幕按需抽取 |
| LUX-227 | web/src/features/player/components/player-captions.ts、web/src/features/player/components/player-video-surface.tsx、web/src/features/player/PlayerPage.tsx、web/tests/player-captions.test.ts、web/tests/player-caption-surface.test.tsx；浏览器原生 in-band TextTrack 探测 |
| LUX-228 | web/src/features/player/、web/tests/；单次媒体读取的文本字幕解析实验，默认不改变 Direct Play |
| LUX-229 | tests/strm_resolver_playback.rs、tests/web_playback.rs、web/tests/、docs/COMPATIBILITY.md；本地与远程 .strm 字幕兼容性阶段门 |
| LUX-230 | src/application/scanner.rs、src/application/metadata.rs、src/storage/、src/api/media.rs、tests/、web/src/features/home/、web/src/lib/api/、web/src/react.css、docs/；全量扫描中的本地旁车流水线 |
| LUX-231 | web/src/features/player/PlayerPage.tsx、web/src/features/player/components/player-controls.tsx、web/src/react.css、web/tests/；LuxPlayer 剧集上一集/下一集导航 |
| LUX-232 | migrations/、migrations-postgres/、src/main.rs、src/storage/、src/application/scanner.rs、src/application/watch.rs、tests/、docs/API.md；数据库生命周期清理与写入膨胀控制 |
| LUX-234 | src/api/emby_catalog.rs、src/api/playback.rs、tests/strm.rs、tests/web_playback.rs、docs/；通用外部代理的 URL 型 `.strm` 交接 |
| LUX-235 | docs/LUX-DEVELOPMENT.md、docs/decisions/032-embedded-text-subtitles.md、docs/decisions/035-remote-matroska-client-pipeline.md、docs/decisions/037-remote-strm-native-caption-boundary.md；远程 Matroska 客户端字幕管线历史规格（当前不实施） |
| LUX-236 | web/src/features/player/matroska-demuxer.ts、web/src/features/player/matroska-range-index.ts、web/tests/matroska-demuxer.test.ts、web/tests/matroska-range-index.test.ts；SeekHead/Cues 和字幕解封装 |
| LUX-237 | web/src/features/player/matroska-subtitles.ts、web/src/features/player/caption-parser.ts、web/tests/player-caption-parser.test.ts；Matroska 文本字幕与安全 ASS/SSA 样式模型 |
| LUX-238 | web/src/features/player/components/player-caption-overlay.tsx、web/src/react.css、web/tests/player-caption-overlay.test.tsx；多 cue 和基础样式渲染 |
| LUX-239 | web/src/features/player/mkv-remux.ts、web/src/features/player/mkv-transcode.ts、web/src/features/player/mkv-transcode-worker.ts、web/tests/mkv-transcode.test.ts；多 codec fMP4 输出 |
| LUX-240 | web/src/features/player/matroska-range-reader.ts、web/src/features/player/mkv-playback-engine.ts、web/src/features/player/mkv-transcode-worker.ts、web/src/features/player/playback-engine.ts、web/tests/mkv-playback-engine.test.ts；Range、缓冲和 Cues seek |
| LUX-241 | web/src/features/player/components/player-captions.ts、web/src/features/player/components/player-settings-panel.tsx、web/tests/player-captions.test.ts、web/tests/player-components.test.tsx；字符串字幕 ID 和引擎字幕控制器 |
| LUX-242 | web/src/features/player/playback-selection.ts、web/src/features/player/PlayerPage.tsx、web/tests/player-playback.test.tsx、web/tests/player-fallback.test.tsx、web/tests/strm-caption-compatibility.test.tsx；远程管线接入和终止错误策略 |
| LUX-243 | docs/COMPATIBILITY.md、scripts/player-matroska-smoke.mjs、web/tests/；远程 Matroska 客户端管线历史阶段门（当前不实施） |
| LUX-245 | web/src/features/player/playback-selection.ts、web/src/features/player/PlayerPage.tsx、web/src/features/player/remote-mkv-caption-reader.ts、web/tests/；远程 STRM 浏览器直连与客户端解码 fallback |
| LUX-247 | docs/LUX-DEVELOPMENT.md、docs/COMPATIBILITY.md、src/discovery.rs、src/main.rs、compose.yaml、docs/DEPLOYMENT.md；Emby 兼容局域网发现 |
| LUX-248 | docs/LUX-DEVELOPMENT.md、docs/decisions/041-device-pairing.md、migrations/0119_device_pairings.sql、migrations-postgres/0119_device_pairings.sql、src/auth/device_pairings.rs、src/security.rs、src/storage/device_pairings.rs、src/storage/catalog.rs、src/storage/repository.rs、src/storage/users.rs、src/storage/mod.rs、src/auth/emby.rs、src/auth/mod.rs、src/api/legacy.rs、src/api/routes.rs、src/api/users.rs、tests/device_pairings.rs、tests/admin_health.rs、tests/danmaku.rs、tests/ready_version.rs、tests/scanner.rs、tests/storage.rs；Lux Prism 一次性设备配对 |
| LUX-249 | docs/LUX-DEVELOPMENT.md、docs/COMPATIBILITY.md、src/application/scraper.rs、src/application/images.rs、src/application/candidates.rs、src/storage/media.rs、src/storage/repository.rs、web/src/features/admin/AdminPluginsPage.tsx、web/tests/plugin-library.test.ts；TMDb 原语言文字与图片模式 |
| LUX-250 | docs/LUX-DEVELOPMENT.md、web/src/features/home/media.tsx、web/src/features/detail/MediaDetailPage.tsx、web/tests/home-media.test.tsx、web/tests/media-detail.test.tsx；季海报缺失时回退父剧海报 |
| LUX-251 | docs/LUX-DEVELOPMENT.md、docs/LUX-251-PLAN.md、migrations/0120_manual_item_merges.sql、migrations-postgres/0120_manual_item_merges.sql、src/storage/media_merge.rs、src/storage/repository.rs、src/storage/mod.rs、src/storage/catalog.rs、src/application/item_merge.rs、src/application/mod.rs、src/application/scanner.rs、src/api/legacy.rs、src/api/admin.rs、src/api/admin_handlers.rs、web/src/features/library/LibraryPage.tsx、web/src/lib/api/client.ts、web/src/lib/api/types.ts、tests/item_merge.rs、web/tests/library-page.test.ts；管理员手动合并媒体条目为多版本 |
| LUX-252 | docs/LUX-DEVELOPMENT.md、web/src/features/detail/MediaDetailPage.tsx、web/tests/media-detail.test.tsx；单季剧集详情直接展示单集列表 |
| LUX-253 | docs/LUX-DEVELOPMENT.md、web/src/features/detail/MediaDetailPage.tsx、web/src/react.css、web/tests/media-detail.test.tsx；单集图片播放与文字详情入口 |
| LUX-254 | docs/LUX-254-PLAN.md、src/application/playback/session.rs、src/api/playback.rs、src/api/emby.rs、src/api/legacy.rs、tests/playback.rs、docs/API.md、docs/COMPATIBILITY.md；Emby 客户端服务端转码 |
| LUX-255 | docs/LUX-DEVELOPMENT.md、docs/API.md、docs/COMPATIBILITY.md、src/api/users.rs、src/api/admin_handlers.rs、tests/lux_api_auth.rs、tests/admin_api_key.rs；Lux 用户级客户端令牌与第三方首页 API |
| LUX-256 | docs/LUX-DEVELOPMENT.md、src/application/thumbnail_policy.rs、src/application/thumbnails.rs、src/application/candidates.rs、src/application/strm_probe.rs、src/storage/、src/api/admin_handlers.rs、web/src/features/admin/AdminLibrariesPage.tsx、web/src/lib/api/types.ts、web/src/react.css、tests/、web/tests/；媒体库缩略图刮削模式与截图优先级 |
| LUX-258 | docs/LUX-DEVELOPMENT.md、src/storage/users.rs、src/api/admin_handlers.rs、src/api/users.rs、web/src/features/admin/AdminSettingsPage.tsx、web/src/features/auth/LoginPage.tsx、web/src/lib/api/、tests/、web/tests/；登录页背景来源选择（固定海报墙或媒体库最新添加） |
| LUX-259 | docs/PLUGIN-SDK.md、src/application/plugin_protocol.rs、src/application/plugins.rs、tests/plugins.rs、docs/；登录页背景插件类型与有界数据 RPC 合同 |
| LUX-260 | migrations/、migrations-postgres/、src/application/、src/storage/、src/api/users.rs、src/api/admin_handlers.rs、tests/、docs/；登录背景插件的后台刷新、缓存、来源选择与公开接口 |
| LUX-261 | web/src/features/auth/LoginPage.tsx、web/src/features/admin/AdminSettingsPage.tsx、web/src/app/、web/src/lib/api/、web/tests/、docs/；插件来源选择、瀑布流/大图布局和来源鸣谢 |
| LUX-262 | Lux-plugins/src/bin/lux-plugin-bing-daily-background.rs、manifests/org.lux.bing-daily-background.json、tests/、docs/；独立 Bing 每日图片插件 |
| LUX-263 | Lux-plugins/src/bin/lux-plugin-tmdb-trending-background.rs、manifests/org.lux.tmdb-trending-background.json、tests/、docs/；独立 TMDb 日榜电影+剧集横幅图插件 |
| LUX-317 | docs/LUX-317-PLAN.md、src/application/plugin_protocol.rs、src/application/plugins.rs、src/application/login_background_assets.rs、src/api/、web/src/features/admin/、Lux-plugins/；统一登录背景插件与单张自定义上传图 |
| LUX-318 | src/application/candidates.rs、src/application/nfo.rs、tests/metadata_selection.rs、tests/nfo_writer.rs、docs/；TMDb 电影完整详情候选与 NFO 写回 |
| LUX-319 | src/application/candidates.rs、src/application/nfo.rs、src/application/people/service.rs、tests/metadata_selection.rs、tests/nfo_writer.rs、docs/；电影 NFO 与详情 API 演员上限扩展到 100 并保持顺序 |
| LUX-320 | src/application/nfo.rs、src/application/probe.rs、tests/nfo_writer.rs、docs/；从本地探测结果生成 Emby/Kodi `fileinfo/streamdetails` |
| LUX-321 | src/application/nfo.rs、src/storage/media.rs、tests/nfo_writer.rs、docs/COMPATIBILITY.md、docs/；为电影 NFO 写入数据库时间/排序值并提供原子 probe-info 写回服务 |
| LUX-322 | src/application/probe.rs、src/api/legacy.rs、tests/probe.rs、docs/COMPATIBILITY.md、docs/；本地探测完成后调用 NFO 技术信息写回 |
| LUX-461 | src/application/admin_events.rs、src/application/scanner.rs、src/application/reidentify.rs、docs/LUX-DEVELOPMENT.md；合并高频 Admin Jobs 进度失效通知并保留关键事件即时刷新 |
| LUX-464 | src/application/reidentify.rs、tests/reidentify.rs、docs/LUX-DEVELOPMENT.md；为指定条目重识别的重复 ID 去重和首次出现顺序补充回归 |
| LUX-264 | docs/LUX-DEVELOPMENT.md、docs/decisions/043-full-scan-manifest.md；Manifest 与完成语义规格 |
| LUX-265 | migrations/0128_full_scan_manifest.sql、migrations-postgres/0128_full_scan_manifest.sql、src/storage/repository.rs、src/storage/mod.rs、src/storage/jobs.rs、tests/storage.rs、tests/postgres_database.rs；跨数据库 Manifest 存储合同 |
| LUX-266 | src/application/scanner.rs、src/storage/jobs.rs、src/storage/repository.rs、tests/scanning_jobs.rs、docs/PERFORMANCE.md；兼容持久 frontier 与新 Lite 目录发现 |
| LUX-267 | src/application/scanner.rs、src/storage/jobs.rs、src/storage/media.rs、src/storage/repository.rs、tests/scanning_jobs.rs；Manifest 差异与安全 apply |
| LUX-268 | src/application/scanner.rs、src/application/home.rs、src/storage/jobs.rs、tests/scanning_jobs.rs、tests/webhooks.rs；索引完成和首页快照时序 |
| LUX-269 | src/storage/repository.rs、src/storage/jobs.rs、src/storage/database_cleanup.rs、tests/scanning_jobs.rs、tests/storage.rs；升级、重试和有界清理 |
| LUX-270 | tests/postgres_database.rs、tests/storage.rs、docs/PERFORMANCE.md、docs/COMPATIBILITY.md；SQLite/PostgreSQL 兼容与性能阶段门 |
| LUX-271 | src/application/scanner.rs、tests/scanning_jobs.rs、tests/performance.rs、docs/PERFORMANCE.md；v3 资源感知并发与有界目录预读 |
| LUX-272 | src/application/scanner.rs、src/storage/jobs.rs、tests/performance.rs、docs/PERFORMANCE.md；全量扫描分阶段耗时与阻塞剖析 |
| LUX-273 | src/application/scanner.rs、tests/scanning_jobs.rs、tests/performance.rs、docs/PERFORMANCE.md；滚动式双 reader 有界预读与准备流水线 |
| LUX-274 | src/storage/jobs.rs、tests/storage.rs、tests/postgres_database.rs、tests/performance.rs、docs/PERFORMANCE.md；SQLite/PostgreSQL 共同写入路径降本 |
| LUX-275 | tests/performance.rs、docs/LUX-DEVELOPMENT.md、docs/PERFORMANCE.md、docs/COMPATIBILITY.md；全链路性能与阶段门 |
| LUX-276 | src/library.rs、src/application/libraries.rs、src/api/emby_catalog.rs、src/application/library_covers.rs、tests/library.rs；HOMEVIDEOS 媒体库类型与配置能力 |
| LUX-277 | src/storage/migration.rs、migrations-postgres/0150_homevideos_video_types.sql、tests/storage.rs、tests/postgres_database.rs；HOMEVIDEOS/VIDEO 双数据库迁移 |
| LUX-278 | src/application/scanner.rs、src/storage/media.rs、tests/scanning_jobs.rs、tests/storage.rs；其他视频扫描与目录层级 |
| LUX-279 | src/application/nfo.rs、src/storage/jobs.rs、tests/nfo_writer.rs、tests/scanning_jobs.rs；VIDEO 本地 NFO 与禁止自动匹配 |
| LUX-280 | src/api/media.rs、src/application/catalog.rs、tests/catalog.rs、tests/resume_favorites.rs；Lux VIDEO 搜索、目录过滤、统计与继续观看 |
| LUX-281 | src/api/emby_catalog.rs、src/application/catalog.rs、src/storage/catalog.rs、tests/mixed_library_api.rs、tests/resume_favorites.rs、docs/COMPATIBILITY.md；Emby homevideos/Video 契约 |
| LUX-282 | web/src/features/auth/AdminSetupForm.tsx、web/src/lib/api/types.ts、web/src/app.mjs、web/tests/setup-page.test.tsx；初始化媒体库类型选择 |
| LUX-283 | web/src/lib/api/types.ts、web/src/features/admin/AdminLibrariesPage.tsx、web/tests/admin-libraries.test.tsx；管理界面类型与刮削器配置 |
| LUX-284 | src/application/catalog.rs、src/storage/repository.rs、src/storage/repository_tests.rs；目录范围查询过滤 |
| LUX-285 | src/api/media.rs、src/storage/repository.rs、tests/catalog.rs；Lux API 分页列出根目录和 FOLDER 子项 |
| LUX-286 | web/src/features/library/LibraryPage.tsx、web/src/features/library/prefetchLibrary.ts、web/src/lib/api/client.ts、web/src/features/home/media.tsx、web/tests/library-page.test.ts、web/tests/api-client.test.ts、web/tests/search-and-filmography.test.tsx；其他视频目录浏览与搜索 |
| LUX-287 | web/src/features/detail/MediaDetailPage.tsx、web/src/features/media/MediaActionMenu.tsx、web/tests/media-detail.test.tsx、web/tests/media-action-menu.test.tsx、web/tests/home-media.test.tsx；视频详情、手动编辑与播放 |
| LUX-288 | docs/LUX-DEVELOPMENT.md、docs/decisions/046-progressive-scan-and-missing-metadata.md、docs/COMPATIBILITY.md、docs/PROGRESSIVE-SCAN-METADATA-PROPOSAL.md；渐进扫描与独立在线补缺规格 |
| LUX-289 | migrations/0151_progressive_scan_metadata.sql、migrations-postgres/0151_progressive_scan_metadata.sql、tests/storage.rs、docs/LUX-DEVELOPMENT.md；渐进扫描本地队列与完整性 schema |
| LUX-290 | src/storage/migration.rs、tests/storage.rs、tests/postgres_database.rs、docs/LUX-DEVELOPMENT.md；SQLite catalog 重建兼容与 PostgreSQL 升级合同 |
| LUX-291 | src/storage/jobs.rs、src/storage/repository.rs、src/storage/mod.rs、src/storage/repository_tests.rs、docs/LUX-DEVELOPMENT.md；渐进扫描本地 metadata outbox 操作 |
| LUX-292 | src/storage/metadata.rs、src/storage/repository.rs、src/storage/mod.rs、src/storage/repository_tests.rs、docs/LUX-DEVELOPMENT.md；能力级本地完整性存储 |
| LUX-293 | src/storage/metadata.rs、src/storage/jobs.rs、src/storage/mod.rs、src/storage/repository_tests.rs、docs/LUX-DEVELOPMENT.md；缺失结果与独立 FILL_MISSING 调度意向原子提交 |
| LUX-294 | migrations/0152_scan_manifest_workflow_three.sql、migrations-postgres/0152_scan_manifest_workflow_three.sql、src/application/scanner.rs、src/storage/jobs.rs、src/storage/repository.rs、tests/scanning_jobs.rs、tests/storage.rs、tests/postgres_database.rs、tests/admin_health.rs、tests/ready_version.rs、tests/scanner.rs、tests/danmaku.rs、docs/LUX-DEVELOPMENT.md；workflow 3 正向索引与本地 outbox 原子提交 |
| LUX-295 | migrations/0153_scan_local_metadata_image_stage.sql、migrations-postgres/0153_scan_local_metadata_image_stage.sql、src/application/metadata.rs、src/application/scanner.rs、src/storage/jobs.rs、src/storage/repository.rs、src/storage/repository_tests.rs、src/api/legacy.rs、src/main.rs、tests/scanned_metadata.rs、tests/scanned_series_metadata.rs、tests/scanning_jobs.rs、tests/storage.rs、tests/postgres_database.rs、docs/LUX-DEVELOPMENT.md；本地 outbox 后台消费与海报优先处理 |
| LUX-296 | migrations/0154_scan_local_metadata_backfill.sql、migrations-postgres/0154_scan_local_metadata_backfill.sql、src/storage/jobs.rs、src/storage/repository_tests.rs、docs/LUX-DEVELOPMENT.md；既有资源回填游标存储 |
| LUX-297 | src/application/scanner.rs、src/storage/mod.rs、src/storage/repository.rs、tests/scanned_metadata.rs、docs/LUX-DEVELOPMENT.md；既有资源本地海报/NFO 回填 worker |
| LUX-298 | src/application/scanner.rs、tests/scanning_jobs.rs、tests/scanned_metadata.rs、docs/LUX-DEVELOPMENT.md；渐进扫描首页刷新与事件合并 |
| LUX-299 | src/storage/metadata.rs、src/storage/mod.rs、src/storage/repository_tests.rs、docs/LUX-DEVELOPMENT.md；完整性检查批量领取 |
| LUX-300 | src/application/candidates.rs、docs/LUX-DEVELOPMENT.md；按请求计划计算本地元数据缺失 |
| LUX-301 | src/application/scanner.rs、src/api/legacy.rs、docs/LUX-DEVELOPMENT.md；扫描后持久化本地完整性状态 |
| LUX-302 | src/storage/metadata.rs、src/storage/repository_tests.rs、docs/LUX-DEVELOPMENT.md；缺失记录与补全任务原子提交 |
| LUX-303 | src/application/metadata.rs、src/application/scanner.rs、src/application/reidentify.rs、src/api/legacy.rs、docs/LUX-DEVELOPMENT.md；渐进扫描接入 FILL_MISSING |
| LUX-475 | src/storage/jobs.rs、src/application/metadata.rs、src/application/scanner.rs、docs/LUX-DEVELOPMENT.md；渐进扫描将分集所属的季度与剧集容器纳入 FILL_MISSING |
| LUX-304 | tests/performance.rs、docs/PERFORMANCE.md、docs/LUX-DEVELOPMENT.md；渐进扫描及海报处理性能测量 |
| LUX-305 | src/storage/catalog.rs、src/storage/repository.rs、src/storage/mod.rs、src/storage/repository_tests.rs、docs/LUX-DEVELOPMENT.md；本地图片批量写入合同 |
| LUX-306 | src/application/metadata.rs、tests/performance.rs、docs/PERFORMANCE.md、docs/LUX-DEVELOPMENT.md；海报 worker 批量写入及复测 |
| LUX-307 | docs/LUX-DEVELOPMENT.md、docs/API.md、src/api/media.rs、tests/catalog.rs；Lux 媒体条目响应公开入库时间 |
| LUX-308 | docs/LUX-DEVELOPMENT.md、web/src/lib/api/types.ts、web/src/features/detail/MediaDetailPage.tsx、web/tests/media-detail.test.tsx；资源详情显示添加时间 |
| LUX-576 | docs/LUX-DEVELOPMENT.md、docs/API.md、src/api/media.rs、tests/catalog.rs；Lux 媒体 userData 公开最近播放时间 |
| LUX-577 | docs/LUX-DEVELOPMENT.md、web/src/lib/api/types.ts、web/src/features/home/media.tsx、web/src/react.css、web/tests/home-media.test.tsx；继续观看卡片三行信息与最近观看文案 |

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
- AuthenticateByName 接受规范登录用户名，或唯一匹配的 Users/Public `Name` 显示名；规范用户名优先，重名显示名拒绝登录。
- `Users/{userId}/Authenticate` 接受 JSON `Pw`，只验证路径指定用户，并返回匹配的 `User.Id` 和 `AccessToken`；根路径与 `/emby` 前缀都可用。
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
- 用户媒体库权限显式区分 `ALL`、`SELECTED`、`NONE`：`ALL` 允许所有已启用媒体库，`SELECTED` 只允许指定库，`NONE` 不允许访问媒体库。兼容的逐库 PATCH 在没有正向选择时恢复 `ALL`；整组替换接口可明确设置 `NONE`。
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
- 容器重启时未完成扫描作业被取消；管理员主动重试后可从持久化状态重新排队。
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

范围：参考 qmby 的 `ParseMediaName` 和刮削器候选策略，在 Lux 应用层提供统一的文件名/目录名解析与标题清洗。解析结果至少包含清洗后的标题、年份、季号、集号、版本和清晰度；支持 `SxxEyy`、`x` 格式、中文“第 N 季/第 M 集”和年份紧贴标题的常见命名。去除分辨率、编码、音频、字幕、来源、发布组等技术噪声，但保留可用于媒体源聚合的版本和清晰度字段。兼容 Emby 常见的 `[tmdbid=123]`、`[tmdbid-123]`、`[tmdb=123]`、`[tmdb-123]` 及对应 `{...}` 标签；标签从标题中剥离并保存为 provider ID，TMDb 刮削器可直接用该 ID 获取详情。

元数据匹配和搜索必须使用媒体库所选刮削器，并按媒体类型分流；TMDb 刮削器的电影调用 `/search/movie`、剧集调用 `/search/tv`。带年份搜索无结果时允许回退无年份搜索，并对中文/英文标题候选逐项尝试。Lux 扫描、候选搜索、批量重新匹配和各刮削器插件使用同一解析语义；插件 RPC 公开字段保持兼容，不泄露凭据。

验收：

- [x] `暗夜与黎明2024` 清洗为标题“暗夜与黎明”、年份 2024；`暗夜与黎明 S01E01 H 265 AAC CHDWEB` 不把技术标签写入标题。
- [x] 统一解析器覆盖电影、剧集、季度、单集的年份/季集号和常见技术标签，并保留版本/清晰度信息。
- [x] 电影文件名末尾连字符后缀仅在同目录唯一匹配基础视频时聚合为版本；标签通用提取，无匹配或歧义时不自动合并，重扫能修复旧拆分条目且稳定条目不重复重探测。
- [x] MOVIE 候选和重新匹配请求只调用 `/search/movie`，SERIES 请求只调用 `/search/tv`；TV 搜索支持中文结果缺字段时的英文逐字段回退。
- [x] 电影和剧集目录/文件名中的 Emby 风格 TMDb ID 标签可被识别、持久化并在选择 TMDb 刮削器时直接请求对应详情；手动改用冲突标题或年份时不复用旧 ID。
- [x] `lux-plugin-tmdb` 的 `metadata.search` 对相同输入产生相同清洗标题和类型分流，协议响应字段不变。
- [x] 解析和匹配错误只产生待处理/可重试结果，不在用户 HTTP 请求路径扫描文件或直接调用 TMDb。

验证：

- `cargo test --locked --test media_matching --test scanner --test series_scanner --test metadata_api`
- `cargo test --locked --test scraper --test plugin_protocol --test plugin_runtime`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`

实施记录（2026-09-29）：新增同目录连字符后缀版本识别，只在恰好一个基础标题候选存在时聚合；扫描器覆盖初扫、旧拆分条目重扫修复、歧义拒绝和稳定重扫不重置 READY 探测状态。上述本地测试组 96 项、两项解析/扫描单测、Manifest 归并单测、`cargo build --locked` 和 `cargo fmt --all -- --check` 通过。`cargo test --locked --all-targets` 的 lib 阶段 591 项通过、1 项失败、7 项忽略；失败为既有 `embedded_subtitle::tests::rejects_excessive_extracted_output` 的超时/大小限制时序断言，单独重跑通过。Clippy 当前受日志改动阻塞：`src/observability/logs.rs::append_audit_event` 触发 `too_many_arguments`。仓库没有 `tmdb` 或 `tmdb_plugin` 测试目标；搜索分流由 `metadata_api` provider stub 覆盖，插件接口由 `plugin_protocol`/`plugin_runtime` 覆盖。

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
- URL 型 `PlaybackInfo` 不访问目标；直接访问 Lux 时，视频播放入口用入站客户端 User-Agent 直连目标、有限解析重定向并返回 307，确保 302 服务收到客户端 User-Agent。交给外部 Emby 代理时，LUX-234 规定 URL/路径型目标在 `MediaSources[].Path` 中保留原始目标，`DirectStreamUrl` 使用标准带短期票据的 `/Videos/{数字ItemId}/stream` 入口。下载端点的远程请求按 LUX-091 单独执行。
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

- 默认 95% 已看和 2 分钟继续观看门槛。
- 用户可在个人设置中调整自动标记已看的百分比；管理员仍可调整全局继续观看最短进度。
- 多用户完全隔离。
- 播放进度达到用户阈值或停止事件达到用户阈值时，电影/单集自动标记为已看。
- Emby 兼容播放回调成功保存进度后，通过普通用户 `home` 事件通知 Lux Web 重新读取继续观看；事件不携带媒体或用户数据，继续观看仍在每次首页请求时实时查询。
- 首页和 Emby Resume 对符合条件的同剧单集只显示季号、集号靠后的进度；总数按电影条目和剧集分组后的卡片数计算。
- 首页和 Emby Resume 在最近播放的已看单集之后没有更靠后的合格未完成进度时，继续观看显示其后的第一集未看且可播放单集；未开始播放时进度为 0。
- 季度和剧集按未删除且可播放的单集聚合已看状态；空容器不自动标记。

验证：边界值测试、播放事件集成测试、剧集聚合测试和 Resume API。

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

- Lux Web 的推荐轮播通过 `/api/v1/home/carousel` 单独读取；服务端与 Web 会话缓存只保存轮播推荐条目。
- Lux Web 的继续观看通过 `/api/v1/continue-watching` 读取，媒体库入口通过 `/api/v1/libraries` 读取，最新资源将所有当前可见媒体库 ID 传给 `/api/v1/home/libraries/latest` 一次批量读取，并按媒体库顺序分别显示 shelf；批量查询沿用有效入库时间，剧集按自身与最新可用分集加入时间的较大值排序。轮播、继续观看、媒体库入口和最新资源区块分别加载与刷新，单一区块失败不得阻塞其他区块。
- `home` SSE 事件会刷新轮播、继续观看、媒体库入口和已挂载的多库最新资源查询；封面刮削完成后，新的图像标签必须能随最新资源查询刷新。
- `GET /api/v1/home` 保留原完整响应供兼容调用；Lux Web 不再依赖该聚合接口。该接口的继续观看、可见库和最新资源均实时读取，只有推荐轮播使用缓存。
- Emby Latest/Resume/Views 分别正确。
- 每个单独的 Lux API 查询不产生 N+1；Lux Web 最新资源区块每次使用一次多库批量 API，不按媒体库数量增加请求。

验证：API 与 Web 回归测试；SQL 查询计数和性能测试。

依赖：LUX-081。

实施记录（2026-09-01）：PostgreSQL 生产扫描期间，首页每库最新资源查询的
`ROW_NUMBER()` 会对全部可见媒体排序后才取每库 20 条，并发刷新时产生大量临时写入。
PostgreSQL 路径改为一条 `LATERAL` 查询，每个媒体库先通过现有 `added_at` 索引限量，
再加载媒体详情；SQLite 路径保持不变，不增加 migration 或依赖。真实生产计划使用
`idx_media_items_library_added_visible`，20 条查询约 0.39 ms；两项扫描热修部署后，
30 秒处理 11,200 个文件，PostgreSQL 临时写入增量为 0。

#### LUX-083：多版本聚合

验收：

- 可靠 provider ID/显式规则聚合。
- 不同剪辑版可独立。
- 进度绑定逻辑 item。
- 媒体源可选择。

验证：4K/1080p/edition fixtures。

依赖：LUX-071、LUX-052。

#### LUX-251：管理员手动合并多版本

描述：在媒体库已有多选模式中，管理员可以选择同一媒体库内两个或更多同类型的电影或剧集，明确指定一个主条目，将其余条目并入主条目作为其他媒体版本。合并不删除媒体文件；被合并条目从目录隐藏，后续扫描仍归入主条目。

验收：

- 电影的媒体源、播放进度、收藏和已看状态合并到主条目；主条目原有默认媒体源优先，所有源仍可选择和播放。
- 剧集按季度号和集号合并匹配的层级；主剧集中不存在的季度或分集重新挂到主剧集，媒体源和状态不丢失。
- 只允许同一启用媒体库、同一 `MOVIE`/`SERIES` 根类型且未被合并的条目；操作事务化、管理员鉴权并受 CSRF 保护。
- 目录、首页、搜索和后续扫描不再显示或重新生成被合并的根条目；审计事件不包含路径、URL 或凭据。
- Web 多选工具栏提供合并入口，要求明确选择主条目并提供成功、失败和加载状态。

验证：电影/剧集合并 API 集成测试、扫描重扫回归测试、Web 多选流程测试。

依赖：LUX-083、LUX-106。

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

#### LUX-092：转发客户端 IP 和远程访问行为

验收：

- 无需配置代理 CIDR，始终优先使用有效的转发头。
- 远程访问只依赖账号认证和媒体库 ACL，不再依据来源 IP 或 can_remote_access 阻止请求。

验证：转发头解析、无转发头回退和反代 HTTPS Cookie 测试。

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
- 媒体库首次达到至少 9 个带 poster 媒体条目时，自动注册并执行一次带媒体库名称的旋转堆叠封面任务；后续扫描不重复触发，管理员上传封面优先。
- 普通用户只能读取自己有权限访问的媒体库封面图。
- 文件扫描与元数据计划统一在任务与日志页配置，媒体库编辑页不再提供计划字段。
- 显示读写与监听状态。
- 首页和媒体库入口支持右键打开 Lux 自定义操作菜单，可对整个媒体库发起元数据匹配或扫描，并显示任务提交结果。

验证：媒体库 API 集成测试、Web 单测、Web 构建和 Playwright。

依赖：LUX-102。

#### LUX-104：用户和权限管理

验收：

- 创建、禁用、改密。
- 上传、替换账户头像；仅接受 JPEG、PNG 和 WebP，单个文件不超过 5 MiB，保存后跨浏览器保持。
- 媒体库 ACL、远程、下载、管理权限。
- 管理页面中媒体库未勾选项表示未设置范围，即可访问全部已启用媒体库；勾选一个或多个后仅可访问已勾选媒体库；取消全部勾选后恢复全部访问。

验证：Playwright 和服务端权限回归。

依赖：LUX-094、LUX-102。

#### LUX-105：任务、日志和错误页

验收：

- 初始没有任何注册任务时，页面显示明确的空状态；任务由系统或插件注册后才出现。
- 创建媒体库后自动出现两个系统注册任务：全量校验媒体库、元数据刮削；注册项包含稳定类型、名称、说明、作用范围和注册来源。所有注册任务都提供立即执行和 Cron 计划；实时增量扫描由文件系统监听触发，不出现在计划任务列表中。
- 查看、取消、重试运行中的任务。
- 运行记录显示所属媒体库名称；名称无法解析时保留媒体库 ID，跨多个媒体库的批量任务不伪造单一名称。
- 过滤失败类型。
- 日志脱敏。
- 管理控制台导航最后提供“更新日志”页面，按版本倒序展示 `docs/CHANGELOG.md` 中的项目更新记录，并沿用 Lux 控制台的视觉样式。
- 已注册任务区分页查看任务，所有已注册项都支持立即执行、计划、启停和资源配置；页面不提供任意新增任务类型或全局未注册任务的入口。
- 任务注册项缺少执行计划时明确显示“未配置”，不伪造调度状态。

验证：Playwright。

依赖：LUX-102。

#### LUX-106：待处理、重新匹配和图片管理

验收：

- 不提供独立的元数据纠错控制台页面或导航入口。
- 整库匹配任务结果显示待确认数量，并能跳转到对应媒体库的待确认筛选。
- 媒体库列表支持服务端分页的待确认筛选，待确认条目保留可播放能力并显示状态标记。
- 媒体库列表支持多选；全为待确认条目时可批量确认，混合选择时继续显示普通媒体操作菜单。
- 从媒体详情页查看候选和 diff，选择仅补缺/刷新未锁定字段并处理写回成功/失败状态。
- 完成一项待确认匹配后可以继续打开下一项待确认媒体。
- poster/fanart 选择。

验证：Playwright 完整元数据重新匹配流程。

依赖：LUX-056、LUX-100。

阶段门：

- 管理员无需调用 API 即可完成初始化、用户、媒体库、扫描和低置信度匹配确认。
- 普通用户无法进入控制台。

### 阶段 11：普通用户 Web 客户端

#### LUX-110：登录和首页

验收：

- 登录、退出和会话恢复。
- 继续观看、媒体库入口和搜索。
- 无权库不显示。

验证：Playwright 多用户测试。

首页媒体库数据应写入 React Query 的 `libraries` 缓存；媒体库入口在 hover 或 keyboard focus
时预取默认排序的第一页，媒体库首屏等待期间显示稳定骨架屏，避免导航后的空白等待。

依赖：阶段 10。

#### LUX-111：媒体库列表与筛选

验收：

- 类型、年份、已看、收藏筛选。
- 名称、最近添加、发行日期、评分排序。
- 游标分页或虚拟滚动。
- 首页和媒体库中的剧集海报在右上角显示集数；剧集显示全部单集数，季度显示该季度单集数。

验证：大列表 Playwright。

媒体库首屏预取必须复用正式页面的 query key、分页边界和 ACL 语义，不得预取无权媒体库或
绕过服务端分页上限。

依赖：LUX-110。

#### LUX-112：电影、剧集和合集详情

验收：

- 显示 poster、fanart、简介、季度/单集、合集和 UserData。
- 元数据匹配确认时通过所选刮削器抓取主要演员及角色名；详情页以圆形头像卡片展示演员，头像使用
  `/config/metadata/people` 中的本地缓存。
- 详情页存在本地 logo/clearlogo 时显示在标题前；没有徽标时仅显示标题。
- 存在徽标时，徽标单独占一行；中文标题和原始语言标题显示在徽标下方的同一行。
- 剧集详情页中，播出季排在演职人员之后、剧照之前；剧照与“更多信息”之间保留留白。
- 详情页的演职人员、播出季、剧照、更多信息和媒体文件等区块标题使用一致的字号、字重与行高。
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
- 人物索引重建使用稳定的 keyset 游标，不再使用大库上的 `OFFSET + CASE ORDER BY`。
- 人物索引任务的游标、进度、取消标记和状态持久化；进程重启会把未完成任务标记为
  `CANCELLED`，取消或完成后不会重复领取已处理条目。
- `people.json` 内容指纹未变化时跳过关系表的 DELETE/INSERT；关系更新和指纹状态在同一事务中提交。
- 人物详情查询使用 `person_credits(person_id, item_id)` 和可见媒体条目索引，避免为每个请求重复扫描大表。

验证：60k 基准；专项测试覆盖 keyset 分页、重启取消、取消后续请求可继续、指纹跳过和查询索引。

实现记录（2026-08）：飞牛部署前必须在目标实例执行迁移并记录 `EXPLAIN`、`pg_stat_activity`、临时
字节增量和前台 p50/p95；本机 ARM 验证结果不得替代 NAS x86_64 性能结论。

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

- HTTPS、转发客户端 IP、Range、超时和流缓冲配置说明完整。
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
- Web 客户端媒体能力探针和客户端解码兼容性验证；首阶段只验证，不改变正式播放器。
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

插件进程通过支持 request ID 多路复用的 JSON-RPC over stdin/stdout 提供 `plugin.hello`、`plugin.health`、`metadata.search`、`metadata.get`、`metadata.bundle`、`metadata.images`、`metadata.credits`、`metadata.externalIds`、`metadata.trailers` 和 `plugin.shutdown`。响应允许乱序返回，宿主按 ID 分发并设置有界 pending 数量；插件不能直接访问 Lux SQLite、媒体根目录或内部任务对象；元数据写回、图片下载和 Emby API 输出由 Lux 负责。

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
- 不从任意未登记的远程地址下载第三方插件包；远程安装只允许使用当前插件商店目录声明的包地址。

#### LUX-144：TMDb 多语言首选与回退配置

范围：为外置 `org.lux.tmdb` 插件增加首选语言组、语言回退开关、有序回退语言组列表、标题别名替换和替代 API 地址配置。语言选项来自 TMDb 的主翻译语言列表并合并地区变体为 73 个 canonical 语言组，非敏感配置由宿主持久化到 `/config/plugin-config/org.lux.tmdb.json`。插件对电影、剧集、季度和单集详情按首选语言发起一次请求并 append `translations`，回退开启时按选择顺序在本地逐字段补全；标题别名替换开启且中文首选语言没有中文标题时，使用 TMDb `alternative_titles` 返回的第一个 `CN` 别名；替代 API 地址开启后使用管理员保存的地址。

验收：

- [x] TMDb 插件配置返回 73 个 canonical 语言组；首项为简体中文 `zh-CN`，其次为繁體中文 `zh-TW`、英语 `en-US`；默认首选为 `zh-CN`，旧地区 locale 会自动归一化。
- [x] 管理员可以保存语言回退开关和多个有序语言组；默认预选繁體中文 `zh-TW`，配置重启后保持，API 不返回任何凭据。
- [x] 回退开启时，电影、剧集、季度、单集详情只请求一次并从 `translations` 只补全空字段，严格遵循精确 locale、同语言组和选择顺序；关闭时忽略翻译回退。
- [x] 标题别名替换默认关闭；开启后电影和剧集在中文首选语言返回非中文标题时尝试使用第一个 `CN` 中文别名，已有中文标题和别名接口失败时保持原值。
- [x] 替代 API 地址默认关闭并使用官方地址；开启后可选择 `https://api.tmdb.org` 或自定义 HTTP(S) 地址，插件请求实际经过所选地址。

验证：

- `cargo test --locked --test plugins`
- 外置 `Lux-plugins`：`cargo test --locked --lib`、`cargo test --locked --bin lux-plugin-tmdb`、`python3 -m unittest discover -s tests`
- `pnpm --dir web test`
- `pnpm --dir web build`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`

依赖：LUX-142。

明确不做：

- 不改变 TMDb Provider ID、Emby DTO 或插件 RPC 方法名称。
- 不把 TMDb 凭据放入非敏感配置、API 响应、日志或插件 RPC。

#### LUX-145：后台本地视频封面与缩略图回退任务

范围：将外部 `ffmpegthumb` 的视频截图行为重写为 Lux 内置后台回退任务。媒体库扫描成功后，任务为缺少有效主图的本地视频来源从同一画面生成独立的竖版 `POSTER` 和横版 `THUMB` JPEG 并登记到 `item_images`；只处理 `LOCAL_FILE`，不读取、不探测、不访问 `.strm` 指向的远程视频。截图结果来源优先级低于本地图片和在线刮削器图片。

验收：

- [x] 本地视频在扫描完成后的后台阶段从默认 `00:03:01` 画面生成独立的 `POSTER`（2:3）和 `THUMB`（16:9），分别通过现有图片接口提供。
- [x] 同一逻辑媒体项优先使用默认本地来源；已有有效本地图片和刮削器图片不被截图覆盖；缺少或失效的登记路径可以重建。
- [x] 截图使用低优先级 `FFMPEG` 回退来源：没有选择刮削器、刮削器没有返回对应图片或截图先完成时，保留截图；刮削器后来获得对应图片时可以替换截图回退图；截图不能替换已经存在的非回退图片。
- [x] `POSTER` 和 `THUMB` 独立判断：刮削器只返回其中一种图片时，另一种可以单独由截图补全；两种图片不共享登记路径。
- [x] `STRM_URL` 不进入候选查询或 ffmpeg 参数；纯 `.strm` 条目不会生成缩略图。
- [x] ffmpeg 使用参数数组、路径根目录约束、原子输出和超时控制；单个文件失败不导致扫描任务失败。
- [x] 扫描任务事件记录缩略图阶段的完成/失败计数；容器重启取消未完成任务后，下一次扫描仍可重试缺失项。

验证：

- `cargo test --locked --test thumbnails`
- `cargo test --locked --test scanning_jobs`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`

依赖：LUX-033、LUX-040、LUX-080。

明确不做：

- 不实现独立缩略图 HTTP API、Web 配置页、转码、音频 WAV 提取、字幕抽取或 `.strm` 远程处理；STRM 截图仍由 LUX-146 单独负责。

---

#### LUX-146：STRM 远程媒体信息插件

范围：将 MediaInfoKeeper 的 `.strm` 远程媒体信息提取能力改写为 Lux Plugin SDK v1 的独立
`media_probe` 插件和 Lux 宿主后台任务。插件 ID 固定为 `org.lux.strm-media-info`，能力为
`media.probe`；插件只负责接收单个已校验的 STRM 探测目标，按请求分别调用 `ffprobe` 提取
媒体信息、调用 `ffmpeg` 截图，并返回受限的结果。探测目标按原始字符串传递，不解析其 URL、
IP 或网段类型。媒体库选择、任务、并发、目标输入校验、数据库写入和兼容旁车写回均由 Lux
宿主负责。

旧版本的 `org.lux.media-info` 作为迁移别名处理：已有插件配置会迁移到新的插件配置路径，
新的 API、manifest 和插件进程只使用 `org.lux.strm-media-info`。

插件 manifest 声明 `libraryIds`、`concurrency`、`mediaInfoEnabled`、`thumbnailEnabled`、
`thumbnailPositionPercent`、`existingInfoPolicy`、`writeSidecars` 和 `schedule` 配置项；`schedule` 使用标准五段式 cron
（分 时 日 月 周），按 UTC 解释，默认 `0 3 * * *`。`mediaInfoEnabled` 默认开启，
`thumbnailEnabled` 默认关闭，两个开关互相独立；`thumbnailPositionPercent` 默认 30，范围为 1-99，
表示按视频时长百分比选择截图位置。其中 `existingInfoPolicy` 的选项为 `SKIP`
（跳过已有媒体信息）和 `OVERWRITE`（覆盖已有媒体信息）。读取旧版本配置时，
`includeReady: false` 迁移为 `SKIP`，`includeReady: true` 迁移为 `OVERWRITE`。
Lux 管理页动态填充 `media-libraries` 选项并保存插件配置。管理员通过
`POST /api/v1/admin/plugins/org.lux.strm-media-info/run` 或兼容的
`POST /api/v1/admin/strm-probe-jobs` 按已保存配置启动任务，不从请求体接收宿主覆盖参数。服务为每个选定媒体库建立持久化任务，使用全局操作信号量
和媒体库 `probeConcurrency` 的较小值限制并发；任务支持分页列表、详情、取消、重试；服务重启时取消遗留的
PENDING/RUNNING 状态，管理员主动重试后继续使用持久化游标。探测结果保存到 `media_sources`/`media_streams`，旁车写回使用同目录
`*-mediainfo.json` 的 MediaInfoKeeper 兼容子集和临时文件原子替换。缩略图只针对 STRM，使用同目录
`*-thumbnail.jpg`；截图前先用 `ffprobe` 获取 duration，再调用 `ffmpeg` 在 `thumbnailPositionPercent` 指定的百分比位置输出一张受限尺寸的
JPEG，并将该文件同时登记为 `POSTER` 和 `THUMB`。媒体信息和缩略图是两步独立命令，不引入 FFmpeg 原生库；截图只补全缺少有效主图的 STRM，不
覆盖已有有效缩略图。历史 `*-thumb.jpg` 仅作读取兼容。只开启缩略图时不保存完整媒体信息，但仍会执行轻量 duration 探测。

STRM 截图采用“本地/在线主图优先、视频截图兜底”的顺序：数据库按媒体条目持久化
`poster_fallback_required` 标记。新增 STRM 没有本地 `POSTER` 或 `THUMB` 时设置该标记为 true；
媒体库未配置刮削器、所选刮削器没有候选、候选没有可用主图时，都保留该标记。发现本地
`POSTER`/`THUMB` 或刮削器成功写入任一主图时清除该标记。STRM 截图阶段只处理该标记为 true
且没有有效 `THUMB` 的 STRM 来源，不要求先找到在线条目。FFmpeg 截图成功后写入同目录
`*-thumbnail.jpg`，并用同一文件同时登记 `POSTER` 和 `THUMB`，来源为 `STRM_FFMPEG`；历史 `*-thumb.jpg` 仍可通过数据库登记路径读取。后续刮削器
获得真实海报或缩略图时可以按图片类型替换对应兜底记录；删除其中一个记录时不能删除仍被另一
记录引用的共享文件。

插件启用后，宿主自动登记一个全局 `STRM_MEDIA_INFO` 计划任务；任务读取同一份插件配置，首次
执行在后台完成，后续按 `schedule` cron 表达式重复执行。管理员可以在“任务与日志”中直接修改该任务的
执行时间，修改会同步回插件配置。插件禁用时任务保留但停用；服务重启后从已登记任务恢复调度。未完成
有效配置时只登记未启用的任务，不创建探测作业。实时监听触发的增量扫描完成后，如果本次受影响路径
包含新入库或发生变化的 `.strm` 媒体，且其媒体库已在插件配置中选中，宿主自动创建只覆盖本次受影响
STRM 来源的后台探测任务；这条事件驱动路径不替代全局计划任务，定时任务仍按同一份配置对所选媒体库
执行全库校验、补漏和按 `existingInfoPolicy` 处理。插件未安装、未启用、配置无效、媒体库未选中或本次
没有 STRM 来源时，不发起插件 RPC。

STRM 来源状态区分“尚未配置”和“已经排队”：未被插件选中的 STRM 来源，以及插件未启用时的来源，
状态为 `NOT_CONFIGURED`；只有已持久化 STRM 探测作业覆盖的来源才进入 `PENDING`。保存新的媒体库选择后，
宿主自动取消旧选择对应的活动作业，并为当前选中的媒体库排入全库处理；插件重新启用时也会按当前配置
排入处理。作业创建与来源转为 `PENDING` 在同一数据库事务中完成。作业完成时来源转为 `READY` 或实际
错误状态；作业取消时未处理来源恢复为 `NOT_CONFIGURED`。普通扫描不会把未配置来源留在探测队列，
并会将对应扫描探测阶段记为跳过。历史 STRM `PENDING` 状态由迁移 0181 改为 `NOT_CONFIGURED`，
随后由当前插件配置决定是否重新排队。

插件 manifest 必须声明 `type: "media_probe"`、`category: "MEDIA"` 和
`capabilities: ["media.probe"]`。插件进程不能访问 Lux SQLite、媒体根目录或内部任务对象；
插件错误、超时、异常退出和超限输出不能导致 Lux 主进程退出。RPC、任务事件、错误消息和旁车
不得包含完整 URL、认证信息或原始 `ffprobe` JSON。

当前 STRM 探测目标策略只校验非空和长度，不解析 STRM 内容，也不根据 HTTP/HTTPS、localhost、
云实例元数据主机、回环、私网、链路本地、未指定、多播、共享地址、域名或路径做拒绝，不要求
管理员指定 IP 或网段。STRM 探测目标只作为插件探测输入，不进入日志、任务事件、旁车或 API
响应；普通扫描、播放和 PlaybackInfo 仍不主动读取 STRM 指向的内容。

验收：

- [ ] 管理员只能选择已有媒体库，未选媒体库不创建任务、不发起插件 RPC；空选择、无效 ID、并发超范围均被拒绝。
- [ ] 未选中或未启用插件的 STRM 来源为 `NOT_CONFIGURED`，不占用普通 ffprobe 队列；保存新的媒体库选择或重新启用插件后，所选来源通过持久化后台作业重新处理，作业取消后未处理来源不遗留 `PENDING`。
- [ ] 插件详情页展示并保存媒体库多选、并发数、媒体信息开关、缩略图开关、缩略图位置百分比、已有媒体信息处理方式、旁车写回和五段式 cron 配置；配置文件原子保存且权限受限，插件列表回显非敏感值；任务与日志页可以修改同一份 STRM 计划。
- [ ] 同一时间的有效探测数不超过任务全局并发和媒体库 `probeConcurrency`；单个 URL 失败只影响对应源，任务可继续。
- [ ] 服务重启会取消 PENDING/RUNNING 任务且不自动领取新源；失败或取消任务可以重试。
- [ ] 成功结果写入媒体源和媒体流；`writeSidecars` 启用时写入兼容旁车，失败不会留下半个 JSON。
- [ ] `mediaInfoEnabled` 和 `thumbnailEnabled` 可以独立生效；缩略图缺失时先由 ffprobe 获取 duration，再由 ffmpeg 在 `thumbnailPositionPercent` 指定的位置生成同目录 `*-thumbnail.jpg`，默认位置为 30%，已有有效缩略图不会被覆盖，历史 `*-thumb.jpg` 仍可读取。
- [ ] STRM 截图遵循本地/在线主图优先顺序：没有刮削器、刮削器无候选或候选没有主图时持久化 `poster_fallback_required`；ffmpeg 不要求在线匹配成功，只消费该标记和缺失图条件；截图成功后将同一 `*-thumbnail.jpg` 文件登记为 `POSTER` 与 `THUMB` 并清除标记，后续刮削器获得图片时可按类型替换 `STRM_FFMPEG` 兜底图。
- [ ] 插件启用后自动出现全局 `STRM_MEDIA_INFO` 注册任务；任务按有效 `schedule` cron 表达式执行，禁用插件后不再领取新作业，重启服务后保留调度配置但取消遗留作业实例。
- [ ] 实时增量扫描完成后，所选媒体库中新入库或发生变化的 `.strm` 来源自动创建定向 STRM 探测任务；定向任务只处理本次增量扫描影响的来源，并支持取消和失败重试。
- [ ] 定向 STRM 探测与全局定时探测共用并发、插件配置和任务持久化边界；定时任务仍保留并继续负责全库补漏，两个任务不能并发占用同一媒体库。
- [ ] 播放和 PlaybackInfo 请求不触发 STRM 远程探测，`.strm` 仍由客户端直连播放。
- [ ] 插件包、manifest、RPC 结果、STRM 探测目标策略、ffprobe/ffmpeg 超时、输出上限和无真实目标的 fake ffprobe/fake ffmpeg 测试覆盖；插件异常不退出主进程。
- [ ] 从空数据库执行迁移成功，ARM64 本机验证记录 `uname -m`，并通过 Rust 格式化、测试和 Clippy 检查。

验证：

- `cargo test --locked --test plugin_protocol --test plugin_runtime --test plugin_package --test media_info_plugin --test media_info_config --test media_info_config_api --test strm_probe --test strm_probe_api`
- `pnpm --dir web test -- plugin-library.test.ts`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`

依赖：LUX-033、LUX-041、LUX-044、LUX-072、LUX-142。

明确不做：

- 不改变 `.strm` 播放直连语义，不做代理、转码、缓存或 AList API 访问。
- 不在普通全量扫描或用户请求路径中探测远程 `.strm`；仅允许文件系统实时事件完成增量索引后，通过宿主后台任务触发定向探测，不把插件权限扩展为媒体库/数据库访问。
- 不把计划任务执行放入普通扫描、播放或用户请求路径；计划任务只复用已有 STRM 后台作业服务。
- 不引入 `ffmpeg-next` 或 FFmpeg C API；插件通过现有系统 `ffprobe` 和 `ffmpeg` 命令完成两步处理。

---

---

#### LUX-150：独立弹幕插件与后台匹配

范围：将 Emby 弹幕插件的能力重写为 Lux Plugin SDK v1 的独立进程插件，固定插件 ID 为
`org.lux.danmaku`。插件负责弹幕配置、上游网络访问、`danmu_api` 的匹配/回退、搜索和评论
获取、Bilibili 标准 XML 的大小与格式校验，以及单项匹配结果和错误分类。主进程只负责插件
生命周期、已索引本地视频及 `.strm` 索引文件的安全分页、通用任务日志/进度/取消/重试、重启取消遗留作业、媒体库 ACL、
受限的旁车写入能力和 Emby 兼容弹幕端点。

插件声明 `type: "danmaku"`、`category: "MEDIA"`、`danmaku.match` 能力和统一 RPC 方法
`danmaku.match`。主进程向插件发送已经过路径和媒体库 ACL 校验的本地视频或 `.strm` 索引文件描述及任务选项，
插件返回结构化匹配状态和受大小限制的 XML；插件不得接收或执行用户直接提供的上游 URL，
不得访问主进程配置目录以外的凭据。管理员配置 Dandanplay 兼容 API 基地址，或配置
`huangxd-/danmu_api` 的 API 基地址，配置由插件 manifest 声明并通过插件配置界面保存。
基地址可以包含部署 token 路径，必须保留路径但在配置响应、日志、审计和错误中脱敏。插件配置还提供匹配并发数（0-64，默认 2；0 表示不设插件级限制，但仍受宿主资源上限约束）以及是否覆盖已有同名 XML（默认关闭）；计划任务使用保存的配置值，手动 API 任务可在请求中指定对应选项。
XML 旁车只登记相对路径，SQLite 保存索引和任务状态，不保存整份 XML。

匹配候选首选媒体源文件名的 basename，以兼容 Dandanplay 的文件名匹配语义；索引中的
`title`、`original_title` 及剧集的标题字段作为回退。对于已有结构化
`season_number`/`episode_number` 的分集，回退候选优先使用数据库中的季集号；只有季号或集号缺失时，
才从文件名补齐。`provider_ids_json` 不作为弹幕匹配键，也不发送给弹幕插件；`.strm` 仅使用本地索引文件名和
元数据，不读取或访问其远程目标。

`danmu_api` 的 `POST /api/v2/match` 是插件内部的可选优先路径；不支持该接口时由插件回退到
Dandanplay 兼容搜索、详情和评论接口。插件负责并发请求、超时、响应大小限制、XML 校验和
错误分类；主进程不得保留一份直接请求弹幕上游的实现。主进程对插件返回的 XML 仍执行最终
大小和路径安全校验，并通过临时文件加原子重命名写入旁车。

弹幕插件 manifest 通过 `scheduledTasks` 声明全局 `DANMAKU_MATCH` 任务，包括展示名称、描述、`scheduleConfigKey`、默认 Cron、启用所需的配置键和资源限制；Lux 使用通用 manifest 任务注册机制写入任务记录，不按弹幕插件 ID 特判。默认 Cron 为 UTC 每天 `0 6 * * *`；任务配置有效且选择媒体库后才启用。管理员可以通过“任务与日志”立即执行或修改 Cron，任务页的修改同步写回插件配置；一次全局执行按每个选定媒体库创建一个持久化弹幕匹配任务，已存在运行中任务的媒体库跳过。管理员也可以通过 `POST /api/v1/admin/libraries/{libraryId}/danmaku/match` 创建单库持久化任务，支持分页列表、详情、取消、失败重试、服务重启取消遗留作业、并发上限和默认不覆盖已有 XML。任务只领取已索引的本地源文件，包括本地视频文件和 `.strm` 索引文件；不读取或访问 `.strm` 指向的远程媒体，用户请求中的整库扫描、弹幕实时发送和上游任意 URL 均不进入范围。

Emby 兼容层提供 `/api/danmu/{itemId}`、`/api/danmu/{itemId}/raw`，并保留 `option=Refresh` 和 `option=GetJsonById` 兼容别名。端点执行现有用户/媒体库 ACL；普通 Emby 字幕端点和不支持弹幕协议的客户端不属于本任务验收范围。

验收：

- [ ] 从空数据库执行迁移成功；扫描后的同名有效 XML 可以登记、读取，删除或损坏旁车会标记索引状态而不删除媒体。
- [ ] Plugin SDK 能校验弹幕插件 manifest、`MEDIA` 分类和 `danmaku.match` 能力；插件提供 `plugin.hello`、`plugin.health`、`danmaku.match` 和 `plugin.shutdown`。
- [ ] 管理员可以通过插件配置界面保存、清除和查看脱敏的弹幕地址；HTTP/HTTPS、token 路径、控制字符、凭据和 fragment 校验符合安全策略，主进程不再保存弹幕专用配置模型。
- [ ] 插件的 `/api/v2/match` 成功路径可以得到 episode 并取得 XML；不支持 `match` 时插件内的搜索/详情回退可工作；无匹配、非 XML、超大响应、超时不会写旁车。
- [ ] 主进程不会直接访问弹幕上游；插件进程故障、超时或单项错误只标记当前任务项，不使主进程退出或终止整批任务。
- [ ] 成功结果写入视频或 `.strm` 索引文件同名的 `.xml`；默认不覆盖已有 XML；中断或权限失败不会留下半个目标文件。
- [ ] 后台任务支持分页、进度、取消、失败重试和重启取消；取消不再领取新项，单项失败不终止任务。
- [ ] Emby 弹幕读取端点返回正确 Content-Type/XML，执行 ACL，并覆盖至少一个真实支持弹幕接口的客户端请求序列。
- [ ] 不实现 Web 播放器弹幕、ASS、转码、实时发送和其他非弹幕客户端适配；相关普通字幕能力不回归。
- [ ] 通过 Rust 格式化、测试、Clippy、空数据库迁移和 ARM 本机 `uname -m` 记录。

验证：

- `cargo test --locked --test danmaku --test danmaku_api --test emby_danmaku`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`

依赖：LUX-033、LUX-041、LUX-064、LUX-072、LUX-080、LUX-090、LUX-142、LUX-146。

明确不做：

- 不把弹幕 XML 当作普通字幕，不新增 Emby 标准字幕类型或强制客户端显示。
- 插件不执行用户输入的上游 URL，不授予插件任意文件系统权限；不做代理播放，不保存完整 XML 到 SQLite，不在 Web 播放器中渲染弹幕。
- 不生成 ASS、不做颜色/位置转换、不实现弹幕发送、实时推送或用户请求中的即时上游匹配；后台 `DANMAKU_MATCH` 计划任务属于本任务范围。

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

元数据整库任务的条目进度通知按任务合并，每个任务最多每秒发布一次 `jobs` 失效事件，任务完成、
失败或取消时立即发布最终事件；单条结果不得同时通过 `jobs` 和 `metadata` 重复失效同一任务列表。
任务摘要先分页再聚合待确认条目，媒体库身份直接保存在任务记录中，不得按列表行重复扫描整张任务
明细表。整库任务全局只允许一个处于等待或运行状态，普通条目任务与整库任务共享最多 8 个 worker；
服务重启后遗留的运行中条目标记为 `CANCELLED`，同一任务进程内只允许一个 owner。

验收：

- [ ] SSE 端点完成管理员鉴权、协议头、ready 帧、心跳和丢帧退化测试。
- [ ] 活动、后台任务和管理配置变更发布正确作用域，前端只失效受影响查询。
- [ ] 管理布局维持单连接、自动重连、重连补偿和卸载关闭行为；页面级刷新按钮全部移除。
- [x] 仪表盘活跃状况中的剧集播放事件先显示剧名；剧集总季数大于一季时再显示季号，最后显示集名；电影及缺少剧集关联信息的事件保持原标题。
- [x] 元数据任务进度事件按任务节流且最终状态立即送达；任务摘要查询不做逐行关联扫描，整库任务和
      worker 总量有界，重启遗留条目标记取消后可由管理员重试。
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
短事务中提交；服务重启后遗留作业标记取消，管理员重试时只允许重复尚未提交的当前目录或尚未提交的文件批次，已经提交的目录
不再遍历。任务取消或完成时清理临时工作项；可恢复失败任务保留有界 checkpoint，避免工作队列
无限增长。

扫描 worker 使用进程内共享的容量为 1 的互斥锁。一个媒体库的文件系统发现、调和工作项处理
和索引写入期间，其他媒体库的文件扫描任务保持排队；文件系统阶段完成并提交后释放该锁。全量
任务默认持有该锁以保持全量文件扫描的跨库串行化；出现待处理的实时增量任务时，全量任务在当前
批次结束后让出锁，确保实时索引优先完成。
ffprobe、本地 NFO/图片、缩略图、自动封面和在线元数据调度属于后处理阶段，不持有扫描互斥锁，
由各自的有界资源配额控制。该机制仍不引入跨库 worker pool，也不改变任务的持久化、恢复和
取消模型。LUX-230 进一步规定本地 NFO/图片可以在全量文件批次提交后立即消费，不再等待整库
文件阶段结束。

验收：

- [x] 创建全量任务不访问媒体文件系统；后台发现阶段对未中断任务中的每个目录只读取一次。
- [x] 发现的文件路径持久化后按批处理；处理批次不重新遍历媒体库，重启取消后由管理员重试时从剩余目录或文件工作项继续。
- [x] 只有所有可用根路径完成发现后才执行 generation missing 判定；不可用或扫描中失效的根路径不批量标记缺失。
- [x] 任务进度在发现完成后具有稳定 `totalCount`；取消和完成会清理临时工作项，失败任务保留可恢复 checkpoint，现有增量扫描行为不变。
- [x] 文件系统阶段完成后释放扫描互斥锁，其他媒体库可以开始文件扫描；后处理继续受独立资源配额限制。
- [x] 自动化测试覆盖单次发现快照、分批恢复、发现期间取消、根路径不可用和工作项清理。
- [x] 自动化测试覆盖失败 checkpoint 重试和后处理阶段不持有扫描互斥锁。

验证：

- `cargo test --locked --test scanning_jobs`
- `cargo build --locked`
- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `uname -m`

实施记录（2026-08-07）：`./scripts/check-all.sh` 全部通过；原生验证架构为 `arm64`。

实施记录（2026-08-29）：`78ba26bd` 补齐未处理扫描错误的 FAILED 收尾并保留可重试 checkpoint；
`b9d9c7b0` 增加跨媒体库后处理期间释放扫描互斥锁的回归测试。相关扫描测试和默认并发测试均通过；
原生验证架构为 `arm64`。

依赖：LUX-041、LUX-043、LUX-045。

#### LUX-187：全局扫描活动与首页即时刷新

管理员 Web 页面右上角显示当前活动的扫描任务，摘要包括媒体库名称、扫描阶段、已处理/总数
和当前正在处理的相对条目显示名。摘要不得返回媒体库根路径、完整本地路径、`.strm` 原始
目标、token、查询参数或其他凭据。活动入口可以进入“任务与日志”并取消活动任务。
打开活动浮层后，点击浮层和活动入口以外的页面任意位置应关闭浮层；点击浮层内容本身不应关闭。

扫描任务持久化当前安全显示名和阶段；发现目录、索引文件、收尾、完成、失败和取消会通过
管理员 SSE 的 `jobs` 作用域刷新任务摘要。新增同源 `GET /api/v1/events`，只允许已登录的
Lux Web 用户，发送不携带业务数据的 `ready` 与 `invalidate` 事件。扫描索引提交后发布
`home` 作用域：扫描批次只标记首页 dirty；成功全量扫描在 Manifest 索引和缺失确认完成后、后处理开始前刷新快照并发送；失败或取消时刷新已提交的安全状态后发送；普通用户 Web
客户端收到后失效首页、媒体库列表和当前媒体库分页缓存；
断线时继续保留低频轮询兜底。该端点不向 Emby 兼容 API 或未认证请求开放。

实施说明（Manifest 扫描）：扫描批次只将首页标记为 dirty，并清理目录列表页缓存；不改变当前
首页 generation，也不同步重建用户级首页快照。成功全量扫描在所有可用根路径完成发现、差异应用和缺失确认后，先在后台强制刷新共享快照和已有用户级快照，再一次性发布 `home` 事件；该时点早于 NFO、probe、封面和缩略图后处理完成。失败或取消时只刷新已提交的安全状态，不对不完整根路径执行缺失删除。普通用户操作仍使用同步首页失效。用户级快照构建期间如果与扫描刷新并发，旧构建结果不得覆盖已发布的新快照。

验收：

- [ ] 任意普通 Lux Web 页面在管理员会话下显示全局活动扫描入口和实时进度。
- [ ] 当前条目摘要经过 basename/相对显示名清理，不包含完整路径、`.strm` URL、token 或 query string。
- [ ] 全量扫描批次期间普通用户继续读取旧首页快照；Manifest 索引和缺失确认完成后、后处理完成前，在新共享/用户级快照替换后发布一次 `home` 事件；失败或取消只反映已提交的安全状态，事件不携带业务数据。
- [ ] 现有 `ScanCompleted` webhook 与 `JOB_COMPLETED` 继续表示索引完成；后处理仍使用 `scan_phase=POSTPROCESSING`，结束后进入 `IDLE`，不新增公开 webhook 或改变 Emby 合同。
- [ ] 管理员 SSE、普通用户事件流分别完成鉴权、ready、刷新和断线退化测试。
- [ ] Rust/Web 测试、格式化、Clippy 和 Web 构建通过，并记录 ARM 本机 `uname -m`。

验证：

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `pnpm --dir web test`
- `pnpm --dir web build`

依赖：LUX-153、LUX-154、LUX-110、LUX-114。

明确不做：

- 不向 Emby 兼容 API 提供 Lux 活动浮层或普通用户 SSE。
- 不在用户请求路径中扫描文件、解析 NFO、调用 ffprobe 或访问 TMDb。
- 不把完整本地路径或 `.strm` 原始目标写入 API、日志或浏览器存储。

明确不做：

- 不把实时增量扫描纳入 cron 调度；全量校验、元数据和 STRM 任务使用持久化的五段式 cron，跨库串行化仅使用进程内扫描互斥锁。
- 不改变 Lux/Emby 公共 API，不增加核心依赖。
- 不在本任务拆分 ffprobe、NFO、缩略图或在线元数据后处理；这些资源队列另行实施和验证。

#### LUX-156：持久化日志与管理员导出

在保留 stdout JSON 容器日志的同时，将结构化日志写入配置目录下按 UTC 日期滚动的日文件，
并提供仅管理员可用的原始日志/ZIP 导出接口与控制台日期选择入口，便于收集其他实例的扫描、图片
和请求错误。日志文件不写入凭据、Cookie、token 或完整外部 URL；无法创建日志目录时必须保留
stdout 日志并在启动阶段报告降级原因。

验收：

- [x] 启动后在 `/config/logs` 生成 `lux.YYYY-MM-DD.log`，文件内容为 JSONL，stdout 日志行为不变。
- [x] UTC 日期变化后写入新日文件；文件日志使用独立后台 writer，不在 Tokio 核心 worker 上同步写文件。
- [x] `GET /api/v1/admin/logs/export` 只允许管理员；单日范围返回原始 `.log`，多日范围返回 ZIP；默认导出最近 7 个 UTC 日，显式日期范围最多 31 天。
- [x] 非法日期、超过范围、无日志文件和日志目录读失败均返回稳定错误，不返回绝对配置路径或内部堆栈。
- [x] 管理员“任务与日志”页可以选择起止日期并直接下载日志；移动端仍可操作，下载失败有可读错误提示。
- [x] 测试覆盖文件滚动命名、单日原始日志下载、多日 ZIP、导出范围限制、管理员权限和 Web 下载入口。

验证：

- `cargo test --locked --test observability --test log_export`
- `cargo build --locked`
- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `pnpm --dir web install --frozen-lockfile`
- `pnpm --dir web test`
- `pnpm --dir web build`
- `uname -m`

实施记录（2026-08-09）：LUX-156 专项 Rust 测试、构建、Clippy、Web 单测和 Web 构建均通过，
本机原生架构为 `arm64`。全量 Rust 测试目前被既有的 `collections` 测试阻塞：媒体可见性谓词
没有把 `collection_items` 成员关系计入合集可见性，导致测试请求返回 404；该问题不属于本任务，
未在本任务中修改。全局 rustfmt 检查还报告了工作区中其他未提交的数据库后端改动，未擅自格式化。

依赖：LUX-105、LUX-135、LUX-155。

明确不做：

- 不在本任务增加自动历史清理策略；管理员或部署系统负责根据配置卷容量管理历史日文件。
- 不提供普通用户日志读取，不修改现有 `/api/v1/admin/logs` 审计 JSON 合同，不改变 Emby API。

#### LUX-158：`.strm` 支持目标分类

验收：

- 读取并保存 `.strm` 首个非空目标，保留中文、空格、括号和路径分隔符。
- 纯词法分类覆盖 HTTP(S) URL、POSIX/UNC/相对路径、SMB、FTP 和不支持协议；分类不产生网络请求、不访问本地路径。
- URL 型目标保持现有兼容行为；本地路径、SMB/FTP 和不支持目标不会被误标记为 HTTP URL。

验证：`cargo test --locked --test strm`、目标分类单测、`cargo fmt --all -- --check` 和 `cargo clippy --locked --all-targets --all-features -- -D warnings`。

依赖：LUX-072。

明确不做：

- 不新增数据库字段或 migration。
- 不在本任务修改 Emby `MediaSource` 输出，不调用外部解析器，不请求路径，不代理媒体字节。
- 不把任何具体第三方工具写入 Lux 核心。

#### LUX-159：持久化 `.strm` 原始目标分类

范围：在不破坏现有 `STRM_URL`/`external_url` URL 兼容行为的前提下，使用已有的可空
`strm_target_kind` 持久化字段。扫描器在新增、重扫和文件内容变化时保存 `URL`、`PATH`、
`OPAQUE` 或 `EMPTY` 分类；旧记录分类为空时由播放表面按原始目标执行同一纯词法回退。

URL 型目标在 `PlaybackInfo` 中直接返回原始 URL；兼容视频入口仅把原始 HTTP(S) 目标以 307 返回给
客户端，不绑定具体 STRM 服务路径，也不代理媒体字节。本地路径型目标生成 Lux 受保护的视频入口
并读取根目录内的实际文件；SMB/FTP 和空目标、不支持目标不会被伪造为直链。STRM 后台探测仍将
原始目标交给受监督插件；普通扫描、`PlaybackInfo` 和 URL 型视频请求都不访问 HTTP/SMB/FTP 目标。

验收：

- [x] SQLite 和 PostgreSQL 空数据库迁移成功，旧数据库可增加可空 `strm_target_kind` 字段。
- [x] 电影、剧集和未解析 `.strm` 扫描均保存首个非空目标及其分类；重扫会更新分类和目标。
- [x] URL 型 `PlaybackInfo`/视频请求保持现有兼容行为并由客户端请求原始 URL，本地路径通过受保护的视频入口读取实际
      文件，SMB/FTP 仅在解析器成功后播放，其他目标不伪造直链，也不会把 `.strm` 文件当作媒体返回。
- [x] 后台 STRM 探测继续使用原始目标；仅 HTTP/HTTPS、本地路径、SMB 和 FTP 进入探测。扫描、
      `PlaybackInfo` 和 URL 型视频请求不因分类发起网络访问；客户端按原始 URL 直接播放。
- [x] 通过专项 Rust 测试、格式化、Clippy，并记录 ARM 本机 `uname -m`；本机为 `arm64`。

验证：`cargo test --locked --test strm --test strm_target`、`cargo fmt --all -- --check`、
`cargo clippy --locked --all-targets --all-features -- -D warnings`。

依赖：LUX-158、LUX-146。

明确不做：

- 不实现路径映射、外部解析器注册、媒体字节代理或转码；URL 型播放只解析响应和重定向地址。
- 不绑定任何具体云盘、网盘或第三方工具。

#### LUX-160：SMB/FTP `.strm` 目标解析与转发

范围：通过 `strm_resolver` 插件处理 SMB/FTP `.strm` 原始目标。插件只接收 Lux 保存的原始目标，
不访问 Lux 数据库和媒体根目录；Lux 不解释路径中的服务商、挂载名或映射规则。本地路径由 Lux
自己的根目录校验和文件读取流程处理。

插件 manifest 必须声明 `type: "strm_resolver"`、`category: "MEDIA"` 和
`strm.resolve` 能力。宿主通过 `strm.resolve` RPC 发送原始目标，插件返回 `RESOLVED` 加
HTTP(S) URL，或 `UNSUPPORTED`。宿主按插件 ID 稳定顺序尝试已安装、启用且配置有效的解析器，
第一个成功结果用于播放，因此可以接入多个互不相同的解析工具。

宿主对插件返回地址执行独立的 HTTP(S)、长度、凭据、fragment 和控制字符校验；校验失败、
插件失败或没有可用解析器时，不产生伪造直链。视频端点只在解析成功后临时重定向到结果地址，
不代理媒体字节、不缓存地址、不在日志记录原始目标或完整外部 URL。

验收：

- [x] 通用解析器 manifest 和 RPC 合同有协议测试，未知插件类型和缺少能力仍被拒绝。
- [x] 多个解析器按稳定顺序尝试；未安装、禁用或未配置的解析器不参与请求。
- [x] 仅 SMB/FTP 目标触发解析；HTTP(S) 目标保持既有直连合同，本地路径不经过解析器。
- [x] 解析器返回的非 HTTP(S)、带凭据、带 fragment、含控制字符或超长地址均被拒绝。
- [x] 解析成功时 `PlaybackInfo` 提供 Lux 受保护的视频入口，入口临时重定向到已校验地址；
      未解析时不伪造可播放 URL。
- [x] 通过专项 Rust 测试、格式化、Clippy，并记录 ARM 本机 `uname -m`。

验证：参见 `docs/LUX-160-PLAN.md`。

依赖：LUX-159、LUX-142。

明确不做：

- 不绑定任何具体云盘、网盘、代理或第三方工具。
- 不把 SMB/FTP 直接拼接为 URL，不实现媒体字节代理或转码。

#### LUX-161：`.strm` 本地路径直接播放

范围：修复本地绝对路径型 `.strm` 在媒体库根目录之外无法播放的问题。`.strm` 中的本地目标按 Lux
进程实际可读性直接处理，例如 `/CloudNAS/115-122/...` 无需配置额外允许根目录，也无需修改 `.strm`
内容。Web、Emby 和第三方播放器共用的 Lux 视频入口都使用该规则。

播放时 `.strm` 目标相对于 `.strm` 所在目录解析；绝对目标不再与媒体库根目录比较，但仍必须在文件系统中
canonicalize 成存在的普通文件。目录、失效路径和另一个 `.strm` 不作为视频返回；Lux 不主动访问远程
HTTP/SMB/FTP 目标。

验收：

- [x] 任意存在且可读的本地绝对 `.strm` 目标，即使位于媒体库根目录之外，Lux Web 和 Emby 视频入口均按
      本地文件返回 Range 响应；`.strm` 原始文本无需改写。
- [x] 相对目标仍相对于 `.strm` 所在目录解析；目录、失效路径和另一个 `.strm` 不作为视频返回。
- [x] 通过路径 canonicalize、普通文件检查和共享视频入口回归；不主动读取远程目标。
- [x] 通过专项 Rust/Web 测试、格式化、Clippy、Web 构建，并记录本机 ARM 架构（`uname -m`: `arm64`）。

验证：`cargo test --locked --test strm_target --test strm_allowed_roots`、相关 API 测试、
`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`、
`pnpm --dir web test`、`pnpm --dir web build` 和 `uname -m`。

依赖：LUX-159、LUX-160。

明确不做：

- 不读取或代理 HTTP/HTTPS、SMB、FTP 等远程目标；不实现路径映射、媒体字节代理、转码或改变第三方客户端的 Emby 路由合同。

#### LUX-162：可配置插件商店与远程插件包

范围：将插件商店目录从 Lux 进程内的静态插件发现结果扩展为可配置的远程目录。默认目录源为
`https://github.com/Qoo-330ml/Lux-plugins`，按该仓库 `main/index.json` 读取插件元数据；管理员可以在
Web 插件商店中填写其他 HTTPS 目录地址。目录项必须包含稳定插件 ID、manifest 元数据、相对或绝对
`.zip` 包地址和 SHA-256，安装只允许下载当前目录声明的包。

已安装插件不会被后台静默替换；管理员可在“已安装管理”中显式升级到目录声明的更高 SemVer 版本。
升级沿用安装的下载、大小、路径、manifest、平台入口和 SHA-256 校验，并以新包的原子替换完成；
版本不升、降级、下载或校验失败时保留旧包和安装状态。成功升级不改变启用状态或插件配置，停止旧
插件进程，后续请求使用新包。

安装流程先将包下载到 `/config/plugins` 外的临时文件，限制响应大小和超时，校验 ZIP 路径、manifest、
协议版本、当前平台入口、声明文件哈希和包内文件上限，再原子移动到 `/config/plugins` 并刷新进程内插件
目录；失败不得写入安装状态或留下可执行临时文件。远程目录不可用时，已发现的本地插件仍可在已安装
管理页使用，错误不得把远程地址或完整下载地址写入日志。

验收：

- [x] 空配置首次读取插件商店时返回内置默认仓库地址；管理员可保存合法 HTTPS 目录地址，拒绝凭据、
      fragment、控制字符和超长地址；刷新或重启后保持。
- [x] 默认 `Lux-plugins` 仓库的 `index.json` 可返回 TMDb、STRM 媒体信息和 IP 归属地插件目录项；
      列表仍分页并保留当前已安装状态。
- [x] 管理员安装目录中的插件后，包通过大小、路径、manifest、平台入口和 SHA-256 校验，写入
      `/config/plugins` 并立即可在媒体库刮削器/插件配置中使用；下载失败、哈希错误和不兼容包不改变安装状态。
- [x] 管理员可以在已安装管理页确认卸载插件；卸载会停止插件进程、移除插件包和安装状态，并清理该插件
      在媒体库中的选择，未确认前不得发起卸载请求。
- [x] 管理员可以在已安装管理页升级到目录中更高的 SemVer 版本；版本不升或降级被拒绝，失败时旧包、
      启用状态和配置保持不变，成功后停止旧进程并使用新包。
- [x] 非管理员不能读取或修改商店来源；插件包下载不记录凭据、完整外部 URL 或包内容。
- [x] 新增远程目录、包校验、配置持久化和 Web 商店地址表单测试；空数据库迁移链、ARM 本机
      `uname -m`、Rust 格式化、Clippy、Web 单测和构建均通过。

验证：

- `cargo test --locked --test plugins --test plugin_package --test plugin_store`
- `pnpm --dir web test -- plugin-library.test.ts -- api-client.test.ts`
- `pnpm --dir web build`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`

依赖：LUX-140、LUX-142。

明确不做：

- 不实现任意 URL 的插件包安装，不放宽现有独立进程和包校验边界。
- 不把插件仓库改造成代码执行平台；仓库只提供已打包插件和目录索引。

#### LUX-164：统一元数据资源目录与人物布局

范围：建立 `/config/metadata` 的统一资源路径合同。媒体条目资源使用
`library/<shard>/<item-id>/`。人物出演关系保存为媒体条目目录下的 `people.json`；已确认的人物资源
使用带永久 Lux 人物编号的可读规范人物目录，provider 身份只作为人物身份索引，不再天然决定人物资源目录。旧
`/config/metadata/people/<bucket>/<display-name>-<provider>-<provider-id>/` 和
`/config/people/items`、`/config/people/profiles` 只读兼容。媒体目录中的 NFO、海报和背景图不在本任务
迁移，继续遵守 ADR-005。

验收：

- [x] 人物头像、人物 NFO 和人物关系快照写入新目录。
- [x] 旧人物目录可读取，升级不会删除旧文件。
- [x] 路径清洗、稳定分片、符号链接拒绝和原子写入有自动化测试。
- [x] 关系查询不扫描整个 metadata 目录，外部图片完整 URL 不进入日志。
- [x] 新人物布局允许一个规范人物关联多个 provider 身份；无稳定身份的演员只写入出演关系。
- [x] 规范人物使用不可复用的永久 `lux-000001` 编号；目录格式为
      `people/person/<display-name-initial>/<display-name>-lux-<number>/`，同名人物使用不同 Lux 编号隔离。
- [x] 人物目录同时保存可迁移的 `person.nfo` 与版本化 `person.json`，其中包含 Lux 编号和全部已确认 provider 身份。

验证：参见 `docs/LUX-164-PLAN.md`。

依赖：LUX-050、LUX-051、LUX-056。

明确不做：

- 不新增人物数据库关系或详情公共 API。
- 不实现 genres、studios、tags、views、livetv 或音乐库对象。
- 不迁移或删除媒体目录中的 NFO、海报和背景图。

#### LUX-165：媒体图片进入统一 metadata/library

范围：将 Lux 通过刮削器下载并登记到 item_images 的新图片写入
/config/metadata/library/<shard>/<item-id>/。已有媒体目录图片继续被扫描、登记和优先提供；本任务
不迁移、删除或覆盖已有媒体目录图片。图片服务、Emby 兼容端点、删除逻辑和自动媒体库封面同时支持
媒体根目录与 metadata/library 两类受保护路径。

验收：

- [x] 新下载图片写入 metadata/library，item_images.local_path 指向新文件。
- [x] Lux/Emby 图片端点同时读取本地图片和 metadata/library 图片。
- [x] 删除逻辑只允许删除媒体根目录或 metadata/library 内的登记文件。
- [x] 缺失判断、符号链接、越界路径、损坏图片和原子写入有测试。
- [x] 自动媒体库封面可以从两类 poster 读取。

验证：参见 docs/LUX-165-PLAN.md。

依赖：LUX-055、LUX-145、LUX-164。

明确不做：

- 不迁移或删除已有媒体目录 NFO、海报、背景图。
- 不实现图片缩放、淘汰策略、合集/类型/工作室/标签对象。

#### LUX-166：辅助元数据对象目录合同

范围：为后续合集、类型、工作室和标签资源建立统一的安全路径规则：
`/config/metadata/<kind>/<bucket>/<display-name>-<provider>-<object-id>/`。本任务只提供路径工具和
契约测试，不创建数据库表、不修改现有合集关系、不增加 API，也不执行 TMDb 自动合集或对象索引。

验收：

- [x] `collections`、`genres`、`studios`、`tags` 使用独立的 metadata 子目录。
- [x] 路径包含可读展示名、provider 和受校验的 object ID。
- [x] 展示名清洗、首字符分桶和越界输入拒绝有自动化测试。

验证：参见 docs/LUX-166-PLAN.md。

依赖：LUX-164、LUX-165。

明确不做：

- 不增加合集、类型、工作室或标签的数据库关系和 API。
- 不实现 TMDb 自动合集、对象索引、对象图片下载或迁移。

#### LUX-167：元数据对象快照写入

范围：为四类辅助元数据对象提供共用的配置卷快照写入边界。对象目录内使用
`<kind-singular>.json` 保存可重建描述；已有合集刷新接入 `collection.json`，数据库继续作为关系和
查询事实来源。genres、studios、tags 在本任务只提供共用写入能力，不伪造对象数据源。

验收：

- [x] 快照保存 kind、展示名、provider、object ID，并可保存简介和成员数摘要。
- [x] 父级符号链接、越界路径、过大快照被拒绝，写入采用同步和原子替换。
- [x] 合集刷新成功后生成或更新 `metadata/collections/.../collection.json`。
- [x] 四类对象共用同一存储边界，不新增 genres/studios/tags 数据库关系或 API。

验证：参见 docs/LUX-167-PLAN.md。

依赖：LUX-166、现有合集刷新能力。

明确不做：

- 不改变合集数据库关系、成员 ACL 或客户端 API 合同。
- 不实现 genres、studios、tags 的抓取、索引、筛选或详情 API。

#### LUX-168：TMDb 电影丰富 NFO 写回

范围：在现有电影候选匹配链路中补充 TMDb 电影详情、演员与 crew、外部 ID、认证和预告片，
并将这些在线结果按稳定的 Lux 电影 NFO 子集写回媒体目录。首版只覆盖电影；剧集、季度和单集
继续使用现有字段。已有未知 XML 字段必须保留；Douban ID、入库时间和媒体技术信息不由 TMDb
伪造，分别留给其他数据源或本地服务。

首版写回字段：`rating`、`premiered`、`releasedate`、`mpaa`、重复的 `country`、`genre`、
`studio`、`tmdbid`/`imdbid`/`uniqueid`、`director`、`writer`、最多 30 个 `actor` 和 `trailer`。
Lux 内部现有评分、上映日期、原始语言和 provider ID 字段继续沿用；新增的重复字段与 crew
信息先作为候选和 NFO 数据处理，不增加 genres/studios 的数据库关系或筛选 API。

可选补充字段：`tagline`、`website`、`status`、`language`、`set`/`setid`、TMDb 海报和背景图
引用。TMDb 没有值时不写入空字段；预算、热度、Douban、入库时间和媒体流信息不映射到首版 NFO。

验收：

- [x] TMDb 电影详情候选包含类型、国家、制片公司和可用认证；认证缺失时不写入伪造值。
- [x] TMDb credits 的 cast 与 crew 能分别映射为演员、导演和编剧；坏 ID 或空姓名被丢弃。
- [x] 电影候选选择后，NFO 原子写回上述可用字段，并保留未知 XML。
- [x] 已有本地字段和锁定字段仍遵守 LUX-050/LUX-054 的优先级与保护规则。
- [x] 现有 TMDb stub、候选选择、NFO 写回和插件 RPC 测试覆盖新字段；不调用真实 TMDb。

明确不做：

- 不扩展剧集/季度/单集 NFO 字段。
- 不增加 Douban、dateadded、fileinfo 或 streamdetails 的假数据。
- 不增加 genres、studios、导演或编剧的数据库关系、筛选 API 或深度浏览 API。

依赖：LUX-050、LUX-051、LUX-054、LUX-055、LUX-056、LUX-142。

#### LUX-169：TMDb 插件版本与本地包更新

范围：将独立 `org.lux.tmdb` 插件从 `0.1.4` 升级到 `0.1.5`，同步内置插件目录、打包脚本、Docker
默认参数和本地 Lux 插件包。该任务只更新版本和包产物，不改变插件 RPC 方法名、协议版本或凭据行为。

验收：

- [x] 源码 manifest、内置目录、打包脚本和 Docker 默认值统一为 `0.1.5`。
- [x] 本地 `config/plugins` 使用包含当前 TMDb 刮削代码的 `org.lux.tmdb-0.1.5.zip`，旧包不再作为活动包。
- [x] 包 manifest、SHA-256、平台入口和插件 RPC 健康/hello 校验通过。
- [ ] Rust 构建、相关插件测试、格式和 Clippy 检查通过。

验证备注：Rust 构建、格式、Clippy 和相关插件测试已通过；全量测试唯一失败项读取了默认 GitHub
插件目录当前仍声明的 `0.1.4`，属于外部目录尚未同步到 `0.1.5`，不是本地包校验失败。

明确不做：

- 不升级 Lux 主程序 Cargo 版本。
- 不改变插件协议、API Key 优先级、TMDb 请求限流或元数据字段。

依赖：LUX-142、LUX-144、LUX-168。

#### LUX-170：本地电影 NFO 演员回退

范围：在后台本地元数据扫描阶段读取电影 NFO 的直接 `<actor>` 节点，将演员姓名、角色和排序始终
写入统一人物关系快照；可选的 TMDb、IMDb、豆瓣或其他 provider 身份用于人物资源关联，而不是演员
展示的前置条件。详情接口继续只读取人物缓存，不在用户请求中解析 NFO；已有规范人物资源或兼容旧
人物头像按身份映射复用，没有图片的演员仍保留在详情列表中并由 Web 使用人物图标占位。LUX-172 将同一套人物解析和关系复用扩展到
剧集、季度和单集 NFO。

验收：

- [x] Emby/Kodi 风格的 `<actor><name>/<role>/<order>` 节点能在后台解析；已知 provider ID 额外解析。
- [x] 没有在线匹配或刮削候选时，演员仍写入 `metadata/library/.../people.json` 并出现在详情页。
- [x] 已有规范人物资源、provider 身份目录或兼容旧人物头像能复用；没有图片时演员信息不丢失。
- [x] Web 详情页没有人物图时显示含人物含义的图标占位。
- [x] NFO 大小、XML 事件数和字段长度继续受现有安全上限保护，详情请求不读取或解析 NFO。
- [x] 演员关系写入与人物资源写入解耦；头像、`person.nfo` 或索引失败时仍保留演员关系，
  详情页使用占位图标，并在关系快照中记录可单独重试的 `pendingAssets`。

明确不做：

- 不为缺少稳定 provider ID 的 NFO 演员虚构任何 provider ID，也不在线补抓人物资料。
- 跨 provider 人物合并和共享图片由 LUX-178 负责；本任务只保存出演关系并复用已确认资源。
- LUX-170 本身不改变人物去重和 provider 规则；剧集层级的接入由 LUX-172 统一完成。

依赖：LUX-164、LUX-168。

#### LUX-171：外置插件包与商店安装

范围：将现有 TMDb、STRM 媒体探测和 IP 归属地插件完全移出 Lux 源码与部署镜像。插件实现和发布包由
`Qoo-330ml/Lux-plugins` 维护；Lux 只保留插件协议、包发现/校验、独立进程监督、商店目录和显式安装
接口。新部署的 Lux 不自动复制或自动启用插件，管理员从插件商店安装后才能使用。

验收：

- [x] 外部插件仓库先发布当前版本的 `linux-x86_64` 和 `linux-aarch64` 包；两种包分别由 AMD/x86 与 ARM runner 编译，文件名包含插件版本和架构，Release 资产与 `index.json` 中对应版本、架构、地址和 SHA-256 一致。
- [x] Lux 源码、Cargo targets、Dockerfile 和 entrypoint 不再包含现有插件进程实现、插件打包器、插件 manifest 或内置 ZIP。
- [x] 新建空 `/config` 启动 Lux 后，`/config/plugins` 不会出现任何自动复制的插件包，插件列表只显示商店中的可安装项。
- [x] 管理员从商店安装插件时，Lux 下载目录声明的包，完成大小、manifest、协议、平台入口和 SHA-256 校验后原子写入 `/config/plugins`，随后可以启用并调用插件。
- [x] 已存在于 `/config/plugins` 的插件在重启后仍可发现；安装状态、启用状态和媒体库 `scraperId` 持久化，不因移除内置包逻辑而自动变化。
- [x] Rust 和 Web 测试覆盖“无内置包”和“显式商店安装”路径；插件进程自身的 RPC/上游行为测试归外部插件仓库维护。

验证：

- `cargo build --locked`
- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `pnpm --dir web test`
- `pnpm --dir web build`

依赖：LUX-142、LUX-146、LUX-151、LUX-162、LUX-169。

#### LUX-172：本地 NFO 丰富字段展示

范围：在索引后的后台本地元数据阶段解析电影、剧集、季度和分集 NFO 的丰富字段，并将解析后的 JSON 原子写入
`media_items.nfo_metadata_json`。媒体详情接口从数据库读取该 JSON 并返回 `nfo` 对象；接口同时将 NFO
中的评分、播出日期、最后播出日期、状态、语言、运行时和 provider ID 回填到现有兼容字段，
使没有在线匹配的本地完整 NFO 也能直接展示。Web 详情页展示标语、类型、国家/地区、制片公司、认证、
合集、导演、编剧、投票数、官网、预告片、外部 ID，以及剧集层级的季/集和播出日期。

首版读取字段：`rating`、`votes`、`tagline`、`premiered`、`releasedate`、`aired`、`lastaired`、`runtime`、
`status`、`language`、`website`、`set`/`setid`、`mpaa`、重复的 `country`、`genre`、`studio`、
`tmdbid`/`imdbid`/`tvdbid`/`wikidataid`/`uniqueid`、`director`、`writer`/`credits`、`trailer`、
`season`/`seasonnumber` 和 `episode`/`episodenumber`。
不把 NFO 的网络图片引用当作本地图片；本地图片继续由图片索引和人物缓存负责复用，缺失时由前端使用占位。

验收：

- [x] 索引后台读取本地电影、剧集、季度和分集 NFO 并原子写入数据库 JSON；详情请求不打开或解析 XML。
- [x] 本地完整 NFO 在没有在线匹配时，详情接口返回丰富字段并回填兼容字段。
- [x] 详情页展示标语、标签、评分辅助信息、导演/编剧、外部 ID 和安全的 HTTP(S) 链接。
- [x] 所有本地 NFO 的大小、XML 事件数、字段长度、数组数量、评分/运行时/URL 范围继续受安全限制。
- [x] 丰富快照以 NFO 内容指纹判断新旧；仅文件时间戳变化不会清空未变化的丰富快照，内容变化才会重建。
- [x] 损坏或过大的派生 JSON 会在读取时自愈清除，详情接口仍返回 200 和基础信息，`nfo` 降级为空值。
- [x] 基础字段、丰富字段和演员关系由一次受限 XML 投影解析产生，避免同一 NFO 的多次解析不一致。
- [x] 不新增 genres、studios、导演或编剧数据库关系、筛选 API 或深度浏览 API。

明确不做：

- 不在用户请求路径读取、解析本地 NFO，也不因详情展示主动联网。
- 不把官网、预告片或 NFO 图片 URL 当作 Lux 代理目标；只作为受限外链展示。

依赖：LUX-164、LUX-168、LUX-170。

#### LUX-178：跨 provider 人物身份与共享图片

范围：将演员展示关系、provider 身份和规范人物资源解耦。演员姓名、角色和排序可以独立存在；TMDb、
IMDb、豆瓣等身份通过唯一的 provider-scoped identity 索引关联到永久 Lux 规范人物编号。规范人物目录保存
`person.nfo`、版本化 `person.json` 和 Emby 风格的 `folder.<ext>` 人物图片；同一规范人物的多个 provider
身份只共用该目录中的一份主头像。目录已有有效头像时，后续 provider 刮削只补缺，不静默替换管理员或本地头像；
人物 NFO 按字段来源补充，已有非空或锁定字段不被覆盖。

provider 切换由后台身份解析任务处理：优先使用已知 provider 身份和明确的跨 provider ID；同一媒体条目的演员
关系可在姓名规范化、角色/排序一一对应且无冲突时自动桥接；完整生日与唯一候选可作为辅助证据。仅姓名或部分生日
不得自动合并。高置信度结果自动关联，低置信度结果进入持久待处理队列；自动关联必须保留证据并支持撤销/拆分。

人物资源可以从配置卷恢复。`person.json` 保存 Lux 编号、provider 身份、别名、字段来源和版本；媒体条目的
`people.json` 保存 Lux 人物编号、角色/排序、旧 item ID、稳定媒体来源键以及用于迁移的 provider ID、规范化路径、
标题、年份和指纹。媒体人物关系快照不再作为数据库恢复源，服务启动和管理员索引重建均不得遍历
`/config/metadata/library` 或关系隔离区来回迁 `person_credits`；数据库清空后必须重新扫描媒体库或重新生成关系。

旧 provider 目录、旧 `people/assets` 文件和当前媒体条目中的旧关系快照继续兼容读取；升级不得删除旧文件，
但关系快照不再作为空数据库启动时的自动恢复源。新图片不得写入 `people/assets`。

验收：

- [x] 无 ID 的 NFO 演员在后台扫描后出现在详情页，并显示占位头像。
- [x] 同一出演关系携带 TMDb、IMDb、豆瓣身份时只保留一个规范人物目录和一份 `folder.<ext>` 图片。
- [x] 已有头像时后续 provider 图片写入被跳过；NFO 仅补充缺失字段，不覆盖已有非空字段。
- [x] 明确的跨 provider ID 可以自动合并；高置信度的同媒体关系/精确生日候选可以后台自动确认；
      仅姓名或不完整生日不得自动合并，未确认的同名人物保持隔离。
- [x] 每个规范人物分配不可复用的 `lux-000001` 编号；目录使用可读姓名加 Lux 编号，不暴露 provider ID。
- [x] provider 身份映射、自动匹配证据、撤销/拆分记录和字段锁定状态在配置卷快照中可恢复。
- [x] 删除数据库后重新扫描同一媒体库，可以恢复人物、provider 映射、头像、NFO 和媒体人物关系；仅凭配置卷快照不自动恢复关系；
      媒体移动、路径复用、损坏快照和多候选匹配不会静默关联错误条目。
- [x] 扫描和在线刮削并发时使用 generation/租约或等价 compare-and-swap，不能以旧快照覆盖新结果。
- [x] `people.json` 关系快照升级可读取版本 1，旧人物目录、旧图片索引和 Emby 兼容图片路由继续可用。
- [x] 演员关系写入、人物资源写入和图片下载彼此解耦；任一资源失败都不丢失演员展示关系。

验证：人物关系单测、NFO 扫描集成测试、跨 provider 自动合并/隔离/撤销测试、生日精度匹配测试、
图片内容去重和损坏恢复测试、旧布局兼容测试、数据库清空后重新扫描重建测试、详情 API 和 Web 占位图测试。

PostgreSQL 补充修复（2026-10-15）：SQLite 0076 已提供人物匹配决定的目标和原人物字段，
但 PostgreSQL 遗漏了对应结构。新增前向迁移 `0176_person_match_decisions.sql`，为
`person_match_candidates` 补齐可空的 `target_person_id`、`previous_person_id` 及目标人物索引，
保留既有迁移内容、校验和及候选记录。PostgreSQL schema version 升至 176，SQLite 保持 175。
`build.rs` 同时监听两套迁移目录。回归覆盖 PostgreSQL 空库、0175 旧库升级、人物确认、
快照重复恢复和撤销后还原原身份；FNOS 上线后的恢复结果需另行验证。

验证结果：本机 `uname -m=arm64`，隔离 PostgreSQL 16.15 的 `postgres_database` 17/17
通过；新增回归先复现缺列错误，再验证空库和 0175 升级、旧迁移校验和保留、快照恢复及撤销。
`storage` 59/59、`people_api` 10/10、`ready_version` 2/2、Rust build、fmt 和全目标全
feature Clippy 通过。全量 `cargo test --locked --all-targets` 的 lib 为 874 passed、23 ignored，
随后在 `scanned_metadata::failed_local_poster_insert_does_not_mark_image_stage_complete`
失败（本地海报数实际 2、期望 1）；未包含本修复的 `a42fa723` 隔离基线定向复跑得到相同失败。
完整 Rust gate 未通过，后续 integration targets 未由该全量命令执行；该 SQLite 扫描问题不在
本次 PostgreSQL 结构修复范围内。本次未部署 FNOS，也不代表生产快照恢复已验证。

依赖：LUX-164、LUX-170、LUX-172。

#### LUX-173：片头片尾章节标记存储

范围：新增按 `media_source` 归属的片头片尾章节标记表，为后续检测插件与 Emby 输出建立服务器 DB
事实来源。当前任务不产生章节记录，不读取容器章节，也不实现检测任务、API 映射、NFO/EDL 或媒体容器写回。

验收：

- [x] SQLite 与 PostgreSQL 从空数据库迁移成功，章节外键随媒体源删除级联清理。
- [x] schema 只接受 `INTRO_START`、`INTRO_END`、`CREDITS_START`，并约束非负时间、置信度和每插件每类型唯一性。
- [x] 现有本地与 STRM 媒体探测行为不变，不请求、解析或保存 ffprobe 容器章节。

验证：SQLite/PostgreSQL 迁移测试、约束与级联测试、现有探测回归测试和基线 Rust 检查。

依赖：LUX-033、LUX-064。

#### LUX-174：Emby 章节兼容输出

范围：从数据库批量加载片头片尾章节标记进入目录领域对象；条目 DTO 的 `Chapters` 使用默认媒体源，
`PlaybackInfo.MediaSources[].Chapters` 使用各自媒体源。映射公开的 `ChapterInfo` 字段和
`IntroStart`、`IntroEnd`、`CreditsStart` 枚举，不新增普通章节或 Emby 私有扩展。

验收：

- [x] 请求 `Fields=Chapters` 时条目返回默认媒体源章节，未请求时保持现有响应体积。
- [x] PlaybackInfo 为每个版本返回自己的章节，排序和 `ChapterIndex` 稳定。
- [x] 没有章节时返回空数组；权限、分页和播放能力行为不变。

验证：Emby 目录与 PlaybackInfo 集成测试、三客户端兼容探针记录和基线 Rust 检查。

依赖：LUX-173。

#### LUX-175：片头片尾检测插件宿主

范围：扩展 Plugin SDK v1，支持 `chapter_detector` 类型与 `chapters.detect` 能力。章节插件 manifest
必须声明 `supportedMediaSourceKinds`；该字段描述宿主可以为该插件提交的媒体源类型，不代表插件会收到
路径或 URL。Lux 在持久化后台
任务中按季度分页读取本地分集，使用现有 ffmpeg 的 chromaprint muxer 提取开头/结尾的有界原始指纹，
只把指纹、采样率、窗口相对时间和请求内临时键发送给插件。插件不接收路径、URL、媒体源 ID、凭据或任务对象。
宿主校验插件结果并把高置信度标记保存为插件来源特殊章节（`provider_id` 为插件 ID）。单季度超过
RPC 上限时批次保留一个分集的上下文重叠，但只对未处理分集落库，避免跨批次漏掉共同片头片尾。

验收：

- [x] manifest、RPC 请求和响应均有严格大小、枚举、数量、时间范围和置信度校验。
- [x] 管理员可按已保存插件配置启动、取消、重试和查看持久化检测任务；重启取消遗留的 PENDING/RUNNING 作业。
- [x] 插件失败、ffmpeg 缺少 chromaprint、超时或坏响应只影响对应分集/任务，不删除已有确认标记。
- [x] 成功重跑只原子替换同一插件生成的标记，不覆盖其他来源。

验证：假 ffmpeg、假插件进程、任务恢复、ACL/CSRF、故障注入测试和完整项目检查。

依赖：LUX-173、LUX-174、LUX-171。

#### LUX-176：外置片头片尾检测插件

范围：在独立 `Lux-plugins` 仓库实现 `org.lux.intro-outro-detector`。manifest 声明
`supportedMediaSourceKinds: ["LOCAL_FILE"]`。插件比较同季度至少两个分集的
Chromaprint 原始指纹，在配置的开头/结尾窗口内寻找满足最小时长和匹配阈值的公共序列，返回
`IntroStart`、`IntroEnd` 和可选 `CreditsStart`。插件不执行 ffmpeg、不读取媒体路径、不联网。

验收：

- [x] 合成指纹测试覆盖共同片头、共同片尾、不同片头、短匹配、静音、偏移和超长季度批次。
- [x] RPC 只接受宿主定义的受限指纹合同；畸形或超限输入返回稳定脱敏错误。
- [x] manifest、x86_64/aarch64 构建工作流和插件商店包生成脚本已接入；Lux 假宿主端到端测试得到特殊章节。
- [x] 未达到阈值时返回空标记，不猜测或写出低置信度结果。

验证：外部插件仓库 `cargo test --locked --all-targets`、fmt、clippy、双架构打包，以及 Lux 契约测试。

依赖：LUX-175。

#### LUX-177：TheIntroDB 在线章节源插件

范围：在独立 `Lux-plugins` 仓库实现 `org.lux.theintrodb-chapter-source`。manifest 声明
`supportedMediaSourceKinds: ["LOCAL_FILE", "STRM_URL"]`。插件通过新增的
`chapters.lookup` 合同，按 Lux 已保存的 TMDb/TVDb/IMDb ID、季号、集号和可选时长请求
TheIntroDB `/v3/media`，只映射片头和片尾为特殊章节。插件不接收媒体路径、`.strm` URL、音频指纹或
任务对象，不运行 ffmpeg/ffprobe；无数据响应不会清除已有章节。

验收：

- [x] TheIntroDB API 查询优先级、速率限制、有限重试、配置 API Key 和响应大小均受边界约束。
- [x] 片头/片尾时间转换、无结束片头、无开始片尾和无 provider ID 的情况有纯逻辑测试。
- [x] Lux 宿主可以在同一章节任务接口选择 `chapters.lookup` 插件，在线分支不调用 ffmpeg，且只把插件来源标记写入章节表。
- [x] 插件 manifest、独立仓库商店目录、aarch64/x86_64 发布工作流和使用说明已接入。

依赖：LUX-175、LUX-176。

#### LUX-182：Emby 风格共享管理员 API Key

范围：增加一个服务器级共享 API Key，行为与 Emby API Key 高度兼容。只有拥有
`can_manage_server` 的管理员可以查看、生成、轮换和撤销；所有管理员看到同一个当前 Key。
该 Key 同时用于 Lux `/api/v1` 和已实现的 Emby 兼容路由。认证后主体是独立的共享服务器主体，显式拥有服务器管理及远程访问权限；不得将它解析或记录为某个用户。媒体库授权按服务器管理员范围执行。请求若涉及具体用户的数据，只能使用路由中明确提供并验证的目标用户 ID；需要“当前登录用户”身份、个人设置或播放进度且请求未指定目标用户时，必须要求用户 session 或 Emby AccessToken。

验收：

- [x] 支持 `X-Emby-Token`、`X-Lux-Api-Key`、`Authorization: Bearer` 和兼容的 `api_key` 查询参数。
- [x] Lux API 与 Emby 兼容 API 都接受共享 Key；现有用户 Web session 和 Emby 登录 AccessToken 行为不变。
- [x] Key 使用至少 256 bit 随机熵，持久化到 `/config` 的受限文件，重启后保持不变；生成、轮换和撤销使用原子写入。
- [x] 非管理员不能读取或操作 Key；Key 不能调用自身的查看、轮换和撤销接口。
- [x] Key 请求跳过 Cookie CSRF 但仍执行管理员权限和远程访问策略；日志、审计事件、错误响应和普通 API 响应不包含明文 Key。
- [x] 轮换立即使旧 Key 失效；审计明确标记共享 API Key，不能伪装成某一位管理员。
- [x] 共享 Key 在路由中保持独立服务器主体，不生成或借用 `UserRecord`；服务器级媒体库权限与请求显式指定的目标用户身份分开处理。

验证：API Key 服务单测、SQLite 集成测试、Lux/Emby 路由鉴权测试、管理员管理接口测试、日志脱敏测试、Web 账户页测试，以及完整 Rust/Web 检查。

依赖：LUX-020、LUX-022、LUX-024。

#### LUX-183：通知器插件、Webhook 事件与持久化投递

范围：为 Lux 增加统一通知事件、持久化 outbox 和可插拔通知器宿主。通知通过持久化事件和投递记录由有界后台
worker 发送，不能阻塞扫描、播放或元数据请求。通知器使用独立进程插件协议；首个外置 provider 为
`org.lux.webhook`。旧版 `builtin.webhook` 目标保留兼容路径，新的通知配置应选择已安装的通知器插件。Lux 原生
`schemaVersion: 1` JSON 合同和 Emby 风格 payload 使用独立
adapter；不声称完整兼容 Emby Webhooks 插件的全部 payload/template 行为。Telegram、企业微信和 Email 的
具体插件实现不属于当前任务。

事件包括 `MEDIA_ADDED`、`MEDIA_REMOVED`、`SCAN_COMPLETED`、`SCAN_FAILED`、`METADATA_UPDATED`、
`JOB_FAILED`、`PLAYBACK_STARTED`、`PLAYBACK_PAUSED`、`PLAYBACK_PROGRESS`、`PLAYBACK_STOPPED`。Lux 核心在事件
进入投递队列前统一生成 `source`、`title`、`content`、`body` 和 ISO `timestamp`；所有内置或外置通知器都必须
转发这些字段，不得在插件中重新生成正文。事件不包含本地绝对路径、`.strm` 原始目标、令牌或完整外部 URL。
播放事件只携带有长度上限的用户显示名和展示所需媒体信息；远程 IP 仅在停止播放事件中携带，不包含用户 ID。
剧集播放通知的可读标题按“剧名 →（多季时）季号 → 集名”生成；单季剧集隐藏季号，特别篇显示“特别篇”。
结构化的 `itemTitle` 始终保留集名；缺少剧集关联信息时，可读标题回退到原标题。

验收：

- [x] 从空 SQLite 和 PostgreSQL 数据库运行 migration，建立通知目标、事件和投递状态表。
- [x] 管理员可以创建、查看、修改、删除、启停 Webhook 目标并执行测试发送；secret 只在创建/轮换时返回，
      普通列表和日志不返回明文。
- [x] Webhook 请求使用 `eventId`、时间戳和 HMAC-SHA256 签名；事件写入和匹配投递记录可恢复且按目标幂等。
- [x] 投递具备超时、固定并发、有限指数退避、`Retry-After`、429/5xx 重试、失败记录和服务重启恢复。
- [x] URL 校验阻止凭据、查询参数、重定向以及默认的 loopback、链路本地、私有和 metadata 地址；管理员显式
      允许私有网络时仍拒绝危险保留地址。
- [x] 媒体/任务服务接入基础事件；重复扫描不会重复发送同一媒体新增事件。
- [x] 播放边沿和节流进度事件接入；乱序回调不会造成位置倒退或通知风暴。
- [x] 剧集播放通知按剧名、（多季时）季号、集名生成可读标题，`itemTitle` 保持集名，缺少关联信息时回退原标题。
- [x] Lux/Emby payload adapter 按目标独立生成事件，旧目标升级后继续使用 Lux 合同。
- [x] 通知插件 manifest/RPC 合同、provider 目标绑定和宿主统一结果分类已实现；通知插件不继承完整配置目录。
- [x] API、存储、URL 安全、签名、重试、恢复、权限、CSRF、脱敏和本地接收器集成测试通过。

验证：参见 `docs/LUX-183-PLAN.md`；完成后更新 `docs/COMPATIBILITY.md`，明确 Lux 原生 Webhook、Emby 风格
payload 的实际支持范围，以及未实现的 Emby 插件行为。

依赖：LUX-020、LUX-022、LUX-041、LUX-073、LUX-093。

#### LUX-184：Web 4K 媒体能力探针

范围：为 Lux Web 提供独立的浏览器媒体能力探针，验证实际本地测试文件在原生 `video`、MediaCapabilities
和 WebCodecs 下的表现。探针只读取用户在页面中指定的媒体 URL，不上传、不持久化媒体内容，不接入正式播放器，
不改变服务端 DirectPlay、Range 或 `.strm` 行为。

目标测试范围包括 4K HEVC 8-bit、4K HEVC 10-bit HDR10、4K H.264 基准、MP4、MKV、24/30/60fps 和常见音频轨。
Dolby Vision、DRM 和服务端转码不属于本任务。

验收：

- [x] `/media-capability-probe.html` 能输入本地媒体 URL、MIME 类型、codec、分辨率、码率和帧率。
- [x] 页面分别报告 `HTMLVideoElement.canPlayType`、MediaCapabilities 和 WebCodecs 能力；结果不包含完整媒体
      URL，避免把令牌写入结果或日志。
- [x] 页面可对实际媒体执行 metadata、短时播放、VideoFrame 计数、丢帧和当前播放位置测量。
- [x] 预设包含 4K HEVC Main、HEVC Main10 HDR10 和 4K H.264 基准；不伪造 Dolby Vision 支持。
- [x] 测试说明要求使用不含个人数据的本地样本，并记录浏览器版本、平台、Lux 提交、样本校验值和结果。
- [x] 本任务不修改正式 `PlayerPage`、服务端播放接口、数据库、Emby DTO 或 WASM/FFmpeg 依赖。

验证：`node --test web/tests/media-capability-probe.test.mjs`、`pnpm --dir web test`、`pnpm --dir web build`，
以及在实际浏览器中打开 `/media-capability-probe.html` 完成媒体矩阵测试。记录 `uname -m`；未提供真实 4K
样本或未运行真实浏览器时，不得宣称 4K 播放兼容。

依赖：LUX-113、LUX-114。

#### LUX-185：Web 原生播放引擎与 HEVC 客户端兜底

范围：将 Web 播放页从直接依赖 HTML `video` 元素改为可替换的播放引擎。浏览器原生支持时继续使用原生
DirectPlay；浏览器无法原生解码 HEVC、但具备 WebAssembly、Web Worker、MSE 和 H.264 `VideoEncoder` 时，
使用客户端 WASM HEVC 解码并编码为 H.264 fMP4 后通过 MSE 播放。所有媒体字节仍来自 Lux 原始 Range 端点，
不触发服务端转码、Remux、代理或数据库任务。

首个客户端 fallback 依赖 MIT 许可的 `@hevcjs/core`（其运行时依赖 MP4Box，BSD-3-Clause），固定版本并记录
许可证；客户端 fallback 不处理 Dolby Vision、DRM 或无法由浏览器编码 H.264 的设备。

验收：

- [x] NativeVideoEngine 保持现有播放、恢复位置、进度、暂停、停止和页面离开事件语义。
- [x] 播放器按真实能力选择原生路径或客户端 fallback，不因 `canPlayType` 的静态结果误选路径。
- [x] 客户端 fallback 使用 Worker，动态加载 WASM/Worker 资产，支持 MP4/fMP4 HEVC + AAC 的播放和 seek。
- [x] fallback 失败时显示可诊断原因，并推荐原生客户端；不创建服务器端转码任务。
- [x] 4K HEVC 在能力探测允许且实际客户端吞吐足够时可以走同一 fallback；性能不足时有明确降级状态。
- [x] `.strm` 外部 URL 只有在浏览器具备 CORS/Range 能力时才尝试客户端读取；不新增服务端代理。
- [x] 不改变 Emby PlaybackInfo、Rust 播放接口、数据库和第三方客户端行为。

验证：Web 单测、Web 构建、真实浏览器 MP4/H.265 fixture 播放、seek、进度和 fallback 错误回归；记录浏览器、
平台、样本分辨率、媒体耗时、客户端转码速度和丢帧。未通过真实性能门时不得宣称该设备支持 4K 实时 fallback。

依赖：LUX-184、LUX-113、LUX-073。

#### LUX-186：插件商店更新检查与安全更新

范围：为管理员插件页面增加插件商店更新检查和已安装插件更新能力。Lux 使用当前已配置的插件商店目录
返回的版本与 SHA-256，比较已发现的本地插件 manifest 版本；页面展示 `latestVersion` 和
`updateAvailable`，管理员可以显式触发检查并更新单个插件。

更新必须复用现有插件包下载、大小、路径、manifest、平台入口和 SHA-256 校验。更新前停止该插件进程，
校验并原子写入新包，再刷新进程内目录；插件配置文件、`installed_plugins` 安装状态、启用状态和媒体库
选择均保持不变。无可用更新、未安装、未找到当前平台包或目录校验失败时不得删除旧包。

API：

- `GET /api/v1/admin/plugins` 返回可选 `latestVersion` 和 `updateAvailable` 字段；请求本身重新读取当前
  插件目录，因此也作为更新检查接口。
- `POST /api/v1/admin/plugins/{pluginId}/update` 只允许管理员并要求 CSRF；成功返回更新后的插件视图，
  无更新返回结构化 `PLUGIN_NO_UPDATE` 冲突错误。
- 更新包仍只允许当前插件商店目录声明的 HTTPS 地址，不接受请求体覆盖下载地址、版本或 SHA-256。

验收：

- [x] 已安装插件页面可以手动检查更新，并显示当前版本、最新版本和是否可更新。
- [x] 可更新插件显示“更新插件”；更新成功后插件仍保持原配置和启用状态，页面显示已是最新。
- [x] 更新下载失败、包校验失败、平台不支持或无更新时旧包仍可用，且不删除插件配置。
- [x] 更新过程中插件进程被停止，更新后通过正常 RPC 调用按需启动新版本；STRM 插件计划任务保持同步。
- [x] 非管理员不能检查或更新；更新接口不记录完整外部 URL、token 或包内容。

验证：

- `cargo test --locked --test plugins --test plugin_runtime`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `pnpm --dir web test`
- `pnpm --dir web build`
- 使用真实浏览器检查插件页面的更新状态、键盘操作、网络请求和无错误控制台。

依赖：LUX-162、LUX-171。

#### LUX-188：可恢复的人物索引重建任务

范围：将人物出演关系索引重建从一次性的启动扫描改为按媒体库持久化、可恢复、可取消的后台任务。
任务使用稳定 `media_items.id` 游标进行 keyset 分页，前台请求继续读取已有索引，不等待整库重建。
服务重启时遗留的未完成任务标记为 `CANCELLED`；同一媒体库同一时间只能有一个 worker 领取任务。
进程内重复触发会合并为 pending 标记，实际重建协调器保持单个运行器，避免重复整库索引扫描。

人物关系不支持从 `/config/metadata/library` 或 `quarantine/people-relations` 自动恢复。关系文件只在当前媒体条目被
扫描或已排队的索引任务处理时读取；`person_index_item_state` 为空不会触发配置卷关系快照的全量导入。

每个条目保存关系来源指纹和关系 schema 版本。只有当前指纹与已保存的非空指纹相同，且 schema 版本
一致时才跳过重建；没有指纹的条目必须重新读取关系文件。关系文件缺失时清理旧数据库关系，但不把
缺失文件标记为已处理，避免文件稍后恢复后永久跳过。

任务使用一次性 `runToken` 保护进度、完成和失败写入，防止旧 worker 在任务取消并重新排队后覆盖新一轮任务。
取消中的任务在当前批次结束后变为 `CANCELLED`；管理员重新执行时清除取消标记、游标和进度并重新排队。

API：

- `GET /api/v1/admin/people/index-rebuild?page=1&pageSize=20` 返回分页任务状态。
- `POST /api/v1/admin/people/index-rebuild/{libraryId}` 为指定启用媒体库排队或重新排队任务。
- `POST /api/v1/admin/people/index-rebuild/{libraryId}/cancel` 请求取消任务。
- 上述接口只允许管理员；GET 不要求 CSRF，POST 要求现有 CSRF/API Key 管理员鉴权。

索引只在 EXPLAIN 证明现有索引不足时增加；keyset 查询使用 `(library_id, id)` 可见条目索引，人物详情
查询使用 `(person_type, provider, person_id, item_id)` 组合索引。所有 worker batch 和事务保持有界。

验收：

- [x] 从空 SQLite 数据库执行迁移成功，任务表、条目状态表和必要索引存在。
- [ ] 从空 PostgreSQL 数据库执行迁移成功；本机 PostgreSQL daemon 不可用，尚未实测。
- [x] keyset 分页在条目增删时不重复、不跳过，且不使用 `OFFSET`。
- [x] `RUNNING` 任务重启后标记为 `CANCELLED`；并发领取只能成功一次。
- [x] 运行中和排队任务均可取消；取消后可重新排队，旧 worker 不能覆盖新任务状态。
- [x] 非空指纹未变化时跳过；指纹变化、缺失或关系 schema 变化时重建。
- [x] 缺失关系文件清理旧索引但不写入可跳过的空指纹状态。
- [x] 管理 API 分页、鉴权、CSRF、排队、取消和重试行为有集成测试。
- [x] Rust 专项测试、格式化、Clippy 和 ARM 本机 `uname -m` 通过；不得以本机 ARM 结果宣称 NAS/x86 性能。

验证：

- `cargo test --locked --test people_api --lib storage`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `uname -m`

依赖：LUX-164、LUX-172、LUX-187。

明确不做：

- 不改变人物资源目录合同、Emby 人物 DTO 或现有人物查询语义。
- 不在用户请求中执行整库扫描，不增加无限 worker，不读取整份 metadata 目录作为查询方案。

#### LUX-189：后台任务资源隔离与管理员任务体验

范围：吸收 PR #14 中与当前架构一致的后台任务和管理员体验改进。watcher 的同步注册工作必须在
有界的专用初始化线程中执行，不能因为 Tokio blocking pool 饱和而拖延启动，也不能直接阻塞 Tokio
核心 worker。整库 metadata 任务使用持久化的媒体库身份和任务范围摘要，进度 worker 有全局上限，
进程重启后遗留的条目标记为 `CANCELLED`；同一任务在单进程内只能有一个 owner。

管理员任务页的加载状态必须有可见反馈并暴露 `aria-busy`，错误状态不持续显示 spinner。metadata
进度事件按任务节流，完成、失败和取消立即发布；前端不能因为 `jobs` 与 `metadata` 两个作用域
同时失效同一查询。图片下载只对 429、5xx、连接错误和超时做有限退避重试。

运行记录在任务结束后显示本次运行的总耗时；总耗时使用任务真实的 `started_at` 与 `finished_at`
计算，时间字段不完整时不显示推算值，运行中的任务不显示完成耗时。

验收：

- [x] watcher 初始化不运行在 Tokio 核心 worker，初始化线程数量有界，现有根路径取消/重concile 行为不变。
- [x] metadata worker 总量有界，重复启动同一任务被拒绝，重启遗留 `RUNNING` 条目标记为 `CANCELLED`。
- [x] metadata 任务摘要不逐行扫描明细表；SQLite 空库迁移成功。
- [ ] PostgreSQL 空库迁移成功；本机 PostgreSQL daemon 不可用，尚未实测。
- [x] metadata 进度事件每个任务最多每秒发布一次，最终状态立即发布，前端不会重复失效同一查询。
- [x] 管理操作页加载态、错误态和无数据态均有 Web 测试；图片重试不重试永久错误。
- [x] 运行记录对所有后台任务返回真实开始/结束时间，并在已结束记录中显示总耗时；缺失时间字段时保持不显示。
- [x] Rust/Web 测试、格式化、Clippy 和 ARM 本机 `uname -m` 记录完成。

验证：参见 `docs/LUX-189-PLAN.md`。

依赖：LUX-153、LUX-162、LUX-172、LUX-187、LUX-188。

#### LUX-190：Emby 迁移插件边界与可行性验证

范围：冻结 `org.lux.emby-migration` 的单向 Emby → Lux 边界，确认公开 Emby API 可提供的用户、媒体
UserData、用户权限和历史播放事件字段。只做规格、协议草案、脱敏 fixture 和验证记录，不实现数据库、
运行时、后台任务、Web 页面或插件包。

验收：

- [ ] 规格明确只允许 Emby → Lux，不定义反向迁移合同。
- [ ] 规格明确 API key、用户密码和完整外部 URL 的存储、传输、日志规则。
- [ ] 保存至少一组脱敏的 Emby 用户、电影、剧集、分集和 UserData 响应 fixture。
- [ ] 用受控 Emby 实例验证并记录用户资料、禁用状态、媒体库权限、已看、播放位置、播放次数、
      最近播放时间和收藏的字段来源。
- [ ] 明确记录当前测试实例是否提供原始播放事件；不能提供时记录为 `ITEM_STATE`，不伪造事件。
- [ ] 定义 `ITEM_STATE` 与 `EVENT_HISTORY` 两级插件能力，以及源端不支持历史事件时的结果语义。
- [ ] ADR-022 与 `COMPATIBILITY.md` 记录协议边界和验证结果。

验证：参见 `docs/LUX-190-PLAN.md`。

依赖：LUX-189。

明确不做：

- 不读取 Emby 数据库、日志文件或未公开内部表。
- 不实现 Emby 密码哈希导入；密码迁移只保留首次登录验证方案。
- 不新增 migration、Rust 代码、Web 代码或插件包。

#### LUX-191+：Emby → Lux 迁移实现

正式实现作为 LUX-190 之后的连续任务，包含独立 `org.lux.emby-migration` 插件、Lux 宿主后台迁移任务、
用户/媒体映射、UserData 状态导入、首次登录密码验证、管理员报告和播放历史查询接口。当前实现只声明
`ITEM_STATE`；只有受控 Emby 实例证明存在公开原始播放事件端点后，才可增加 `EVENT_HISTORY`。

验证：参见 `docs/LUX-191-PLAN.md` 和 `docs/COMPATIBILITY.md`。

依赖：LUX-190。

#### LUX-193：演员收藏

范围：为 Lux Web 的演员/人物详情增加按用户隔离的收藏状态。演员收藏与媒体条目的
`user_item_state` 分开存储，不改变 Emby 人物 DTO 和 Emby 兼容收藏接口。

API：

- `GET /api/v1/people/{personId}` 在人物 DTO 中返回 `isFavorite`。
- `PUT /api/v1/people/{personId}/favorite` 接收 `{ "favorite": true|false }`，成功返回
  `204 No Content`。
- 修改接口需要登录和现有 CSRF 校验；人物不在当前用户可访问媒体库中时返回 `404`，避免越权探测。

验收：

- [x] 从空 SQLite 数据库执行迁移成功；PostgreSQL 集成测试因本机没有 PostgreSQL 实例而跳过。
- [x] 人物详情能读出当前用户的收藏状态。
- [x] 收藏、取消收藏、重复请求和不同用户隔离有 Rust 集成测试。
- [x] Web 人物详情提供可访问的收藏切换按钮，并在成功后刷新人物状态。
- [x] Web API 客户端和人物详情组件有自动化测试。
- [x] Rust/Web 基线检查通过。

明确不做：

- 不把演员收藏混入 Emby `FavoriteItems` 或媒体条目的 `user_item_state`。
- 不在本任务增加演员收藏列表页面；后续如需要，单独设计分页列表接口和页面。

#### LUX-194：演员搜索与人物参演作品

范围：扩展 Lux Web 搜索，使用户可以按演员姓名搜索人物；人物详情显示当前用户有权限访问的全部
参演电影和剧集。该能力只使用已持久化的 `person_credits` 关系，不调用 TMDb、不扫描 metadata
目录，也不改变 Emby `/Persons` 的 DTO 合同。

API：

- `GET /api/v1/people?q={query}&page={page}&pageSize={pageSize}` 返回分页演员摘要。
- `GET /api/v1/people/{personId}/items?page={page}&pageSize={pageSize}` 返回人物的分页参演作品。
- 作品只返回 `MOVIE` 和 `SERIES`；分集出演关系聚合到所属剧集，同一剧集只返回一次。
- 两个接口都严格执行当前用户的媒体库 ACL、启用状态、条目可用性和分页上限。
- 人物搜索结果和人物作品结果不暴露媒体路径、完整外部 URL 或内部文件信息。

Web 验收：

- 搜索页显示人物结果和现有媒体标题搜索结果；点击人物结果进入人物详情。
- 人物详情显示人物资料、头像和“参演作品”区域，作品使用现有媒体卡片和用户状态字段。
- 作品列表分页加载；无作品、无头像、加载失败和无权限状态均有明确界面反馈。

验证：

- Rust 集成测试覆盖中文/英文人物搜索、同一人物去重、电影/剧集/分集聚合、分页和 ACL。
- Web 单测覆盖搜索结果、人物详情作品加载和继续加载。
- Playwright 覆盖搜索演员、进入人物详情和查看参演作品。
- 运行 Rust/Web 基线检查，并记录 ARM64 验证。

依赖：LUX-080、LUX-164、LUX-178、LUX-193。

#### LUX-195：Provider-neutral 元数据刮削器边界

范围：将 TMDb、IMDb、豆瓣及后续元数据来源统一置于 provider-neutral 的应用层合同之后。TMDb
仍可保留自己的 endpoint façade、语言回退和数字 ID 适配，但候选匹配、重新识别、NFO/图片写回、
人物关联、合集和后台任务不得依赖 TMDb 类型、数字 ID 或固定 provider 名称。

每个 metadata 插件必须声明稳定的 `providerKey`，插件安装 ID 与元数据身份命名空间分离。内部
provider ID 统一按字符串和 provider namespace 处理，兼容 `tmdb:123`、`imdb:tt123`、
`imdb:nm123` 以及豆瓣等来源的非数字 ID。插件能力由 manifest 和通用 capability 读取，业务层
不得通过插件 ID 或字符串包含关系判断能力。

验收：

- [x] 通用 metadata RPC 不再通过 TMDb typed adapter 转换；TMDb endpoint façade 只属于 TMDb adapter。
- [x] provider ID 精确匹配、候选保存、NFO 写回、图片 source 和人物身份均使用当前所选 provider。
- [x] TMDb、IMDb 风格字母数字 ID、豆瓣风格任意字符串 ID 各有同一套 provider-neutral 单测和集成 fixture。
- [x] TMDb 现有搜索、语言回退、合集、图片、人物和重新识别行为保持不变；不新增数据库 migration。
- [x] 不支持某项 capability 的 provider 返回稳定的“不支持”结果，不伪造 TMDb 数据或把错误报告为 TMDb 故障。

验证：

- 先运行 provider、scraper、candidate、metadata selection 和 reidentify 相关 Rust 测试。
- 运行 `cargo fmt --all -- --check`、`cargo build --locked`、`cargo test --locked --all-targets` 和
  `cargo clippy --locked --all-targets --all-features -- -D warnings`。
- 更新 `docs/COMPATIBILITY.md` 和本任务 ADR，记录插件 ID、provider key、能力和 provider ID 规则。

依赖：LUX-142、LUX-168、LUX-178、LUX-194。

#### LUX-196：有序媒体库刮削器角色与补充策略

范围：将媒体库的单个 `scraperId` 扩展为可排序的刮削器列表。首位固定为 `PRIMARY`；后续刮削器可分别配置为 `SUPPLEMENT`、`BACKUP` 或 `BOTH`。主来源首先处理全部请求能力；备用来源按能力接管主来源失败的项目；补充来源在身份确认后合并单值缺失项、去重后的多值项和允许多张的背景图。

API 合同：

- 新 Lux API 返回 `scrapers` 数组，每项包含 `scraperId`、`position` 和 `role`。
- 旧版 `scraperId` 继续返回并表示 position 0 的主刮削器；旧版只提交 `scraperId` 时转换为单个 `PRIMARY` 项。
- 创建和 PATCH 媒体库时，`scrapers` 的顺序和角色作为一个原子配置更新；空数组清除在线刮削。
- 每个 scraper ID 只能出现一次；position 0 必须是 `PRIMARY`；所有选择都必须是已安装、已启用且可用的 metadata 插件。

执行合同：

- `PRIMARY` 首先处理本轮请求的全部能力；它只要能够确认身份就停止身份搜索，但每项能力的空结果、无效结果、不支持或重试失败都会单独标记为缺失。
- `BACKUP` 按 position 顺序只请求并接管仍缺失的能力；如果主来源未确认身份，`BACKUP` 才可以参与身份匹配。某个备用来源成功填充一项后，后续备用来源不再重复处理该项。
- `SUPPLEMENT` 和 `BOTH` 只在身份确认后进入补充阶段；单值字段只在当前为空时填充，多值字段按主来源、备用来源、补充来源的顺序去重追加，单图类型不覆盖已有图片，背景图允许按索引追加。
- `BOTH` 同时具备两种职责：主来源未确认身份时可参加身份/能力备用，身份确认后仍可参加补充合并。备用阶段已经填满的能力不会由后续备用来源重复处理；`BOTH` 进入补充阶段时仍可获取该来源的额外列表和背景图，用于真正的内容补充。
- 后续来源不得覆盖本地 NFO、锁定字段、已有更高优先级来源或已确认的媒体身份；每个字段和图片记录实际 scraper 来源。
- `FILL_MISSING` 只请求实际缺失的内容；`FULL_REFRESH` 允许刷新主来源的未锁定在线字段，再由补充来源补足仍缺失的内容。
- 所有来源失败时保留本地可播放条目，并按现有任务错误/待确认语义记录结果；日志只能记录脱敏的 scraper ID、角色和错误码。

验收：

- [x] SQLite 和 PostgreSQL 空库迁移成功，历史 `libraries.scraper_id` 自动迁移为 position 0 的 `PRIMARY`。
- [x] 管理员可以创建、编辑、排序和清除有序刮削器列表；旧 API 客户端仍能读取和提交单个 `scraperId`。
- [x] 主刮削器成功时，备用来源不被调用；主刮削器失败或某项能力缺失时，备用和 `BOTH` 按顺序接管仍缺失的能力。
- [x] 补充和 `BOTH` 来源只能填充缺失内容，不能覆盖本地、锁定或主来源字段；图片按类型补缺。
- [x] 第二来源成功后的 provider ID、字段来源和图片来源可在后续刷新中正确使用。
- [x] 非管理员不能查看或修改媒体库刮削器角色和顺序。

验证：

- Rust 单元/集成测试覆盖迁移、API 兼容、角色校验、备用接管、补充合并、来源追踪和图片补缺。
- Web 单测覆盖拖拽排序、角色选择、首位主刮削器约束、不可用已选插件和保存失败。
- 运行 Rust/Web 基线检查，并记录 ARM64 验证结果。

依赖：LUX-140、LUX-142、LUX-168、LUX-195。

#### LUX-197：全量扫描变化集与后处理资源隔离

范围：在现有 LUX-154 持久化目录发现和文件工作队列之上，补齐全量调和的变化集优化与后处理
资源隔离。文件工作项先通过 `stat` 和快速 fingerprint 分类为 unchanged、new 或 changed；
unchanged 只批量标记本轮 seen，不进入媒体索引、NFO、图片、缩略图或 ffprobe。new/changed
source 和受影响 item 持久化为扫描目标，后处理按目标集合执行；进程重启会取消遗留作业，管理员重试后再处理。NFO、图片
和其他旁车文件的变化必须能把对应 item 标记为 metadata target，不能因视频文件 fingerprint
未变化而永久跳过旁车更新。按媒体文件夹产生的扫描目标允许在全量扫描仍继续时被本地旁车 worker
消费，首页不必等整库文件阶段结束。

已有文件的 `stat`/fingerprint 检查使用最多 64 个在途 I/O 任务；新文件不创建 fingerprint
检查任务，结果按发现顺序回收，避免把扫描目录一次性展开为无限 Tokio 任务。

ffprobe 使用独立的有界资源配额：默认 256 路，单库有效上限 512，进程全局硬上限 512；配置值
保留 1 至 512 的输入范围，但实际并发受 CPU、内存、前台 p95 和全局 semaphore 限制。4 核 NAS
的正常 I/O 并行目标为 128，8 核可达到 256，16 核及以上可达到 512；压力升高时按四分之一或二分之一降档，恢复
后经过冷却期逐步翻倍升档。后处理
不持有文件扫描互斥锁，不能把无限数量的 ffprobe、NFO 或图片任务一次性提交到 Tokio worker。

验收：

- [x] 无变化全量重扫只执行目录读取、stat/fingerprint 和批量 seen 更新；不会调用媒体索引、NFO、图片、缩略图或 ffprobe。
- [x] 单个新增、变化、删除和旁车变化只派生对应 source/item target；同一 item 的多个 source 不重复处理 item 级任务。
- [x] 扫描目标、后处理阶段和失败状态持久化；进程重启会取消遗留作业，取消和重试不会重复完成已提交目标。
- [x] ffprobe 默认有效并发为 256，硬上限为 512；CPU、内存或前台 p95 恶化时能动态降档，恢复后带冷却地升档。
- [x] reconciliation 工作项的发现、seen、变化目标登记和完成清理使用有界批量事务，SQLite 不逐文件往返。
- [x] 128/256/384/512 路 ffprobe 和 60,000 文件首扫/无变化重扫均有可重复基准记录；扫描期间前台 p95 保持小于 1 秒或记录差距。

验证：

- `cargo test --locked --test scanner --test scanning_jobs --test probe --test thumbnails`
- `cargo build --locked`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `LUX_PERF_FILE_COUNT=60000 ./scripts/run-performance.sh`
- `uname -m`

依赖：LUX-040、LUX-043、LUX-044、LUX-045、LUX-145、LUX-154。

实施记录（2026-08-31）：PostgreSQL 生产全量校验在 385 万个剩余工作项下，30 秒处理 1,100 个文件却产生约
799 MiB 临时写入；活动查询显示 sidecar 目标登记为最多 100 个目录生成 `substr(...) OR ...`，反复扫描同一媒体根。
目标登记现改为一次短事务中按去重目录执行 `(library_root_id, relative_path)` B-tree 范围查询，不增加 schema 或索引；
回归测试覆盖相似目录前缀和中文路径，完整 `scanning_jobs` 目标通过。与首页排序热修一起部署后，同一生产任务
30 秒处理量从约 1,100 提高到 11,200，临时写入从约 799 MiB 降为 0；本机 ARM64 结果不外推 NAS/x86_64 性能。

#### LUX-198：Web 播放会话、服务端 HLS 与 Jellyfin FFmpeg 7

范围：在保留 Direct Play 优先级和现有客户端 HEVC/MKV fallback 的基础上，为本地媒体增加 Web 专用播放会话、
0～4 档服务端播放计划和会话级 fMP4/CMAF HLS。运行时固定使用 Jellyfin 官方 `jellyfin-ffmpeg` 项目
`v7.1.4-3` 正式版的 Debian Trixie ARM64/AMD64 包；普通 Debian `ffmpeg` 不再安装。

播放决策必须满足：

- 档位 0 为原始 Range 直放或客户端 fallback；档位 1 为视频/音频 copy 的 Remux；档位 2 为视频 copy、音频转码；
  档位 3 为硬件转码；档位 4 为软件转码。
- 本地媒体总是按 0 → 1 → 2 → 3 → 4 的顺序优先选择较低成本计划；浏览器能力、选择的音频/字幕轨和管理员资源策略参与决策。
- `.strm` 只允许档位 0；直连、有限重定向或本地安全读取失败时返回明确错误，不创建 ffmpeg 进程。
- 服务端 HLS 的清单、初始化片段和媒体片段只存于受配额限制的播放会话目录；会话结束、超时、服务重启清理孤儿目录。
- HLS 进程使用独立进程组，持续读取 stderr，按 Remux/硬件/软件类别分别限制并发，并在低磁盘水位拒绝新会话。

Web API：

- `POST /api/v1/playback/sessions` 创建播放计划并返回 `sessionId`、`tier` 和 `DIRECT`/`SERVER_HLS`/`UNSUPPORTED` 判别联合。
- `POST /api/v1/playback/sessions/{sessionId}/events` 接收带 `eventId`、`sequence`、状态和位置的幂等播放事件。
- `POST /api/v1/playback/sessions/{sessionId}/heartbeat` 延长会话生命周期；`DELETE` 停止会话并回收资源。
- Direct、HLS manifest 和 HLS segment 使用短期签名 URL；签名不能授权其他媒体源、路径或用户。
- 现有 Lux 播放进度接口继续兼容；Emby 路由/DTO 不复用 Web 播放 DTO。

字幕首阶段继续使用现有外挂字幕端点；不做字幕格式转换、烧录或 DRM。多码率自适应 HLS 不属于本任务。

验收：

- [x] 空 SQLite 和 PostgreSQL 均能运行新增迁移；播放会话、幂等事件和临时资源状态约束有效。
- [x] 本地 MP4/H.264/AAC 在档位 0 使用 Range 直放；MKV 等容器在需要时使用档位 1 HLS，视频和音频质量不改变。
- [x] 不兼容音频可选择档位 2；无可用硬件且策略允许时才选择档位 4；硬件能力不可用时不会伪造档位 3。
- [x] `.strm` 直放成功时只产生档位 0；直放失败时没有 ffmpeg 子进程、临时 HLS 目录或服务器代理流量。
- [x] HLS 播放可以取得 manifest、init segment 和媒体 segment，首次播放不需要等待整部媒体处理完成；seek、暂停、停止和断线回收正常。
- [x] 事件重复、乱序、页面关闭和心跳超时不会造成进度倒退或会话泄漏。
- [x] 无权限 source、过期签名、路径穿越、错误用户 session 和跨会话 segment 请求均被拒绝。
- [x] Web 播放器支持原生 Direct、Safari 原生 HLS、MSE/HLS.js 和现有客户端 fallback，并显示可诊断的失败原因。
- [x] `ffmpeg`、`ffprobe` 和所有现有媒体工具实际来自 `/usr/lib/jellyfin-ffmpeg`，版本为 `7.1.4-Jellyfin`。

验证：Rust 单元/集成测试、SQLite/PostgreSQL migration 测试、容器 ARM64/AMD64 smoke test、Web 单测和构建、
真实浏览器 manifest/segment/seek/停止测试；本机 `uname -m=arm64`，不以本机 ARM 结果宣称 NAS/x86 性能。验证记录见
`docs/LUX-198-PLAN.md` 和 `docs/COMPATIBILITY.md`。

依赖：LUX-184、LUX-185、LUX-189。

#### LUX-199：Emby 媒体源代理兼容

范围：补齐第三方 Emby 反向代理读取媒体源所依赖的标准请求形状，使路径型 `.strm` 可以由外部代理（例如
Redia）根据 `MediaSources[].Path` 执行自己的路径映射并返回云盘直链。Lux 只提供 Emby 元数据和受保护的视频入口，
不识别具体云盘、不执行路径映射、不请求 115，也不代理媒体字节。

兼容合同：

- `MediaSource.DirectStreamUrl` 使用标准 Emby 形状 `/Videos/{ItemId}/stream[.Container]?MediaSourceId={MediaSourceId}`；
  原有 `/Videos/{ItemId}/{MediaSourceId}/stream[.Container]` 入口继续接受，以免破坏已有客户端。
- `GET /Items/{MediaSourceId}` 和 `/emby/Items/{MediaSourceId}` 在媒体源属于当前用户可见条目时，返回该条目详情；未知
  媒体源仍返回 404。
- 路径型 `.strm` 的 `MediaSources[].Path` 保留原始路径；其 `Protocol=File`、`IsRemote=false` 仍表示 Lux 自身按本地文件
  语义处理，外部代理是否接管由外部代理决定，不通过伪造远程标志触发。
- Lux Web 对路径型 `.strm` 的 Direct Play 计划额外提供标准 `/Videos/{ItemId}/stream[.Container]?MediaSourceId=...` `proxyUrl`，允许外部
  Emby 代理接管；播放器优先使用该地址并保留签名 Lux `url` 作为回退，Lux Web 会话和播放进度仍由 Lux 的 Web 会话接口记录。
- Emby 查询参数和请求头中的 API token 继续兼容；播放 URL 可由客户端编码为路径中的 `%3F` 形式时，视频入口仍解析其中的
  `MediaSourceId` 和 token。

验收：

- [x] PlaybackInfo 和 Emby 条目详情给出可由代理关联的 ItemId、MediaSourceId 和原始 `MediaSources[].Path`。
- [x] 标准查询参数播放 URL 与历史媒体源路径 URL 都能播放本地文件或对 URL 型 `.strm` 返回有限重定向。
- [x] `GET /Items/{MediaSourceId}` 仅返回所属且可见条目；未知 ID、无权限条目不会泄露其他条目。
- [x] 路径型 `.strm` 不启动外部请求、不改变 `Protocol`/`IsRemote` 语义、不产生 Lux 侧代理媒体流量。
- [ ] 通过专项 Rust 测试、格式化、Clippy，并记录本机 ARM 架构；真实 Redia/VidHub 复测结果写入兼容性记录。

明确不做：

- 不实现 Redia 或其他第三方工具的路径映射、115 API、直链缓存或媒体字节代理。
- 不把路径型 `.strm` 改报为 `Protocol=Http` 或 `IsRemote=true`，不改变 Harbor 的本地直读行为。

依赖：LUX-159、LUX-161、LUX-198。

#### LUX-200：元数据补全请求扇出与图片资源隔离

范围：在现有持久化元数据队列、provider-neutral 刮削器和人物补全队列之上，修复元数据补全的无效
请求、失效图片重复请求和图片串行下载。该任务不改变 Lux/Emby 公共媒体元数据合同，不把在线请求
放回用户请求路径；只增加后台任务内部的按需计划、图片尝试状态和有界资源配额。

执行合同：

- `FILL_MISSING` 先根据当前未锁定字段、provider ID、本地图片和媒体库图像策略生成请求计划；只请求
  实际缺失的字段或图片类型。完整条目不得发起在线请求。
- 已支持 `metadata.bundle` 的插件优先使用一次 bundle；旧插件按请求计划调用独立 capability，不能为
  补全无条件请求 credits、external IDs 或 trailers。
- 自动候选只展开最佳候选的完整详情；其他候选保留搜索摘要，不请求详情、图片和人物详情。
- 图片下载使用独立全局 semaphore 和每条媒体的有界并发；同一 `(item_id, image_type, candidate_key)`
  同时只能有一个尝试，不能因为图片慢而无限占用元数据 worker。
- 图片尝试持久化 `AVAILABLE`、`UNAVAILABLE`、`FAILED`、尝试次数、最后错误分类和
  `next_retry_at`。上游明确无图片的结果不再重复请求；超时、连接失败、429 和 5xx 使用有限指数退避。
- 图片下载失败不得撤销已经成功写入的基础元数据；NFO 和图片仍使用现有临时文件、校验、刷盘和原子替换。
- 演员人物详情继续使用独立有界队列；元数据主任务完成时间不依赖可选的人物详情请求。
- 演员队列满时使用可取消背压，不丢弃已确认的补全请求；服务关闭先关闭发送端、取消并等待 worker，
  未领取任务被丢弃且其去重状态释放，关闭后的新请求明确失败。
- 元数据阶段记录脱敏的请求数量、缓存命中、重试数、各阶段耗时、图片字节数和队列等待时间；不得记录
  凭据、完整外部 URL 或原始 query string。

并发边界：元数据条目 worker 使用独立的网络 I/O 配额；SQLite 默认有效并发为 4，PostgreSQL 默认有效并发为 8，进程全局硬上限为 16。前台 p95、CPU、内存压力会使有效值降档。图片下载、图片写入、人物详情和元数据条目 worker 使用独立配额，但所有队列必须有界。

验收：

- [x] 完整 `FILL_MISSING` 条目上游请求数为 0；只缺一类字段时不会请求无关 capability。
- [x] 搜索摘要、详情、图片、credits、external IDs、trailers 和图片下载均有请求计数测试；缓存和
      singleflight 不产生重复请求。
- [x] 404/明确无图片在后续补全中不重复请求；临时失败只在 `next_retry_at` 到期后重试，成功后清零退避；
      永久 HTTP 状态不会被安排为临时重试。
- [x] 每条媒体图片并发不超过配置值，进程全局图片并发不超过 semaphore；并发测试证明 SQLite、文件写入
      和前台请求没有无界任务堆积。
- [x] SQLite 和 PostgreSQL 空库 migration 均可运行，已有图片、NFO 优先级、锁定字段和人物关系回归通过。
- [x] TMDb `0.1.8` bundle 目录、内置默认目录、包校验和相关插件测试一致。
- [x] 性能记录包含请求数、阶段耗时、吞吐、重试/不可用比例和本机 `uname -m`；不得将 ARM64 结果外推为
      NAS/x86_64 结论。

SQLite 空库 migration、NFO/图片优先级、锁定字段和人物关系回归已通过；PostgreSQL 使用
`postgres:16-alpine` 临时实例完成同一组元数据回归，实例已在验证后清理。release 元数据基准使用
32 条固定媒体夹具，记录了请求计数、阶段 p95、吞吐、图片重试/不可用比例、图片字节数和
`uname -m=arm64`；这些结果只代表本机 ARM64，不能外推 NAS/x86_64。

验证：参见 `docs/decisions/027-metadata-refresh-resource-pipeline.md` 和 `docs/PERFORMANCE.md`。

依赖：LUX-169、LUX-189、LUX-196。

#### LUX-201：TMDb/豆瓣与 Lux 主程序彻底解耦

范围：在不改变 metadata RPC v1、NFO/Emby provider namespace 和现有元数据性能优化结果的前提下，移除
Lux 主程序编译的 TMDb client/adapter、TMDb endpoint/凭据/图片 URL 逻辑和 TMDb 专用配置分支；TMDb 与
豆瓣的实现、配置读取和上游访问全部由 `Lux-plugins` 独立插件负责。

契约：

- metadata 插件必须通过 manifest 声明 `providerKey`；`pluginId` 只表示安装和运行时身份，aliases 只用于
  旧 `scraperId` 的通用解析。provider ID 在 Lux 业务层始终是不透明字符串。
- metadata RPC 继续使用 `metadata.search`、`metadata.get`、`metadata.bundle`、`metadata.images`、
  `metadata.credits`、`metadata.externalIds` 和 `metadata.trailers`，不增加 TMDb 专用方法。
- 宿主对 metadata 插件只传递其专属配置文件路径 `LUX_PLUGIN_CONFIG_PATH`，不传递 `LUX_CONFIG_DIR`；
  其他插件的配置隔离策略不因本任务改变。
- 旧 `/config/tmdb_*` 和其他历史 TMDb 设置只允许做一次性迁移，迁移结果写入 `plugin-config/org.lux.tmdb.json`；
  迁移过程不记录凭据，迁移后主程序不再解释这些字段。
- `tmdb` 和 `douban` 仅作为兼容 namespace/alias 保留，不能触发主程序的 provider 特判或外部网络请求。

验收：

- [x] Lux 主程序源码和二进制不包含 `TmdbClient`、`tmdb_plugin`、TMDb API endpoint、TMDb 运行时凭据解析或
      TMDb 图片 CDN 转换实现；旧配置读取仅存在于一次性兼容迁移路径。
- [x] TMDb `0.1.9` 和豆瓣 `0.1.4` 插件独立完成 metadata RPC v1，并只读取各自专属配置路径。
- [x] 旧 TMDb 配置、旧 `scraperId: "tmdb"`、NFO/Emby provider ID 和 TheIntroDB 所需外部 ID 均可兼容，
      且 provider ID 不丢失、不被强制转换为数字。
- [x] metadata 插件进程无法读取整个 Lux 配置目录；配置 API 不返回敏感值，日志不包含凭据和完整外部 URL。
- [x] 现有 LUX-200 元数据请求数、吞吐和 Rust/Web 质量门不退化；补充插件仓库构建、manifest、RPC、
      Linux x86_64/aarch64 包验证。

验证记录：详见 `docs/LUX-201-PLAN.md`；Lux 全量质量门和插件发布验证于 2026-08-27 完成，本机架构为
`uname -m=arm64`，性能结论不外推到 NAS/x86_64。

依赖：LUX-142、LUX-169、LUX-200。

验证：`docs/LUX-201-PLAN.md`。

### 阶段 16：LuxPlayer 原生 Web 播放系统

本阶段把现有 Web 播放能力收拢为 Lux 自有播放器系统。每次只执行一个 `LUX-*` 任务；ArtPlayer 只作为 MIT 许可下的
选择性衍生来源或实现参考，不作为 Lux 的运行时依赖。LUX-203 至 LUX-208 完成后必须经过阶段门，才能进入字幕、弹幕和
Rust/WASM 播放增强任务。

#### LUX-203：LuxPlayer 产品边界、衍生代码与许可证治理

范围：建立 LuxPlayer 的产品规格、架构边界、ArtPlayer MIT 衍生代码规则、第三方来源台账和后续第一阶段任务。此任务
只改文档，不复制 ArtPlayer 代码，不改变播放行为。

验收：

- [x] ADR 明确 LuxPlayer 与 ArtPlayer 的产品和运行时边界，并兼容 ADR-006、ADR-026。
- [x] `docs/THIRD-PARTY-NOTICES.md` 固定 ArtPlayer 上游仓库、MIT 许可、版权、参考 commit 和来源台账格式。
- [x] LUX-204 至 LUX-208 各自只有一个清晰目标、验收和验证方式，没有把字幕/弹幕/Rust codec 提前混入。

验证：`git diff --check`，人工审阅三份文档；文档-only 任务不需要新增代码测试。

依赖：LUX-201。

#### LUX-204：LuxPlayer 核心状态、命令和引擎契约

范围：在 `web/src/features/player/core/` 建立 Lux 自己的播放状态、命令、事件、快照、错误和 `PlaybackEngine` 契约，
将 Native/HLS/fallback 的生命周期约束写成 TypeScript 单测。此任务不改变页面视觉，不增加服务端 API。

契约最低要求：

- 状态至少区分 `IDLE`、`PREPARING`、`READY`、`PLAYING`、`PAUSED`、`BUFFERING`、`SEEKING`、`ENDED` 和 `FAILED`。
- 引擎必须声明 `setSource`、`play`、`pause`、`seek`、`snapshot` 和 `destroy`；`destroy` 幂等且不可向旧媒体继续派发事件。
- 状态转换拒绝不合法的旧事件；错误保留用户可诊断的原因和是否允许服务端回退。
- Controller 不知道 ArtPlayer 类型、DOM 结构或上游事件命名。

验收：单测覆盖初始加载、播放/暂停、缓冲、seek、结束、错误、重复销毁和旧引擎事件隔离；现有 Web 构建通过。

验证：`pnpm --dir web test`、`pnpm --dir web build`。

依赖：LUX-203。

#### LUX-205：LuxPlayer Controller 接入 Lux 播放会话

范围：把 LUX-204 的 Controller 接入现有 `/api/v1/playback/sessions`、事件、心跳和停止接口，保持当前 Direct、HLS、
客户端 fallback、版本选择、续播和错误回退语义。此任务不拆 UI。

验收：

- Controller 从服务端计划选择正确引擎，并在媒体源/会话变化时停止旧引擎和旧会话。
- 播放、暂停、定时进度、停止、页面离开、heartbeat 和单调 sequence 行为与现有测试一致。
- `.strm` 仍只走档位 0；Controller 不拼接任意 URL、不创建服务端未声明的计划。

验证：扩展 `web/tests/player-playback.test.tsx` 的会话生命周期断言，运行 `pnpm --dir web test`、
`pnpm --dir web build`，并运行相关 Rust Web 播放测试。

依赖：LUX-204、LUX-198。

#### LUX-206：LuxPlayer UI 与播放页面拆分

范围：将现有 `PlayerPage` 拆分为 LuxPlayer 容器、视频 surface、顶部信息、底部控制栏、设置面板和错误/加载状态；
保持已有外观、快捷键、倍速、音量、全屏、画中画、版本选择和可访问性行为，再为后续手势/字幕/弹幕预留明确插槽。

验收：

- 页面数据获取和播放器呈现职责分离；UI 不直接创建播放会话或操作 Rust API。
- 当前 Native、HLS、HEVC/MKV fallback 和错误提示回归不变。
- 所有交互控件有可访问名称、键盘路径和移动端可操作尺寸；不引入 ArtPlayer DOM/CSS。

验证：组件/页面单测、`pnpm --dir web test`、`pnpm --dir web build`，真实浏览器检查桌面和 320/768/1440 宽度。

依赖：LUX-205、LUX-113。

#### LUX-207：LuxPlayer 手势、自动隐藏和时间轴交互

范围：吸收并改造 ArtPlayer 中经过验证的手势、自动隐藏、时间轴和触摸交互思路，形成 Lux 自有实现。桌面保留
键盘快捷键；移动端增加双击快进/快退、水平滑动 seek、垂直滑动音量，并处理 pointer capture、滚动冲突和可访问性。

验收：

- 手势只作用于当前 LuxPlayer 实例，不泄漏到页面或旧播放会话。
- 单击、双击、拖动、悬停预览、缓冲显示和自动隐藏在鼠标、触摸和键盘输入下互不误触。
- seek 期间状态、时间显示、进度上报和 HLS/fallback 引擎保持一致。
- 来源台账记录实际复制/改造的 ArtPlayer 模块；没有复制的部分标为“仅参考”。

验证：纯逻辑与组件单测、Playwright 触摸/鼠标流程、`pnpm --dir web test`、`pnpm --dir web build`。

依赖：LUX-206。

#### LUX-208：Media Session、移动端安全区与播放器兼容性收尾

范围：将浏览器 Media Session、页面可见性、移动端 safe-area、方向/全屏策略和可诊断兼容性状态接入 LuxPlayer；不在
此任务中加入字幕、弹幕或 Rust/WASM codec。

验收：

- 支持的浏览器通过 Media Session 控制播放、暂停、前进、后退和 seek；不支持时安全降级。
- iOS/Android viewport、刘海安全区、横竖屏和全屏状态不遮挡核心控制；桌面键盘行为不回归。
- 播放失败能区分浏览器不支持、资源过期、引擎失败和服务端计划失败，并给出 Lux 建议。
- 兼容性记录包含浏览器、平台、媒体样本和已验证能力；不以单次探测宣称 4K 实时播放。

验证：Playwright 多 viewport、`pnpm --dir web test`、`pnpm --dir web build`、真实浏览器 console/network 检查，并更新
`docs/COMPATIBILITY.md`。

依赖：LUX-207、LUX-184、LUX-185。

#### LUX-209：LuxPlayer ArtPlayer 风格控制层与弹幕可见性开关

范围：在不改变 Lux 播放会话、媒体源、ACL、进度上报、解码引擎或服务端 API 的前提下，按 ArtPlayer 官方演示页已核验的
控件密度、底部渐变层、时间轴和桌面/移动布局重构 Lux 自有控制层。保留并重新放置 Lux 的版本选择、播放/暂停、音量、
时间、设置、画中画和浏览器全屏；新增本地截图动作与本地弹幕显示开关。独立的 Lux 播放路由已占满视觉 viewport，
等价于 ArtPlayer 嵌入式播放器的“网页全屏”状态；不得为此加入无效的重复全屏按钮。

明确不做：弹幕请求、匹配、解析、加载、渲染、发送、持久化或热力图；字幕、循环、镜像、画面比例和 AirPlay 也不因
本次视觉任务提前实现。弹幕开关只保存本次播放器实例的可访问 UI 状态，不发出网络请求。

验收：

- [x] 桌面控制栏按 ArtPlayer 已核验的 46px 控件节奏、透明控件层和底部渐变层呈现，并保留 Lux 标题、返回和版本语义。
- [x] 版本选择、截图、设置、画中画和全屏均有可访问名称；不可用的平台能力不显示或安全降级。
- [x] 弹幕显示开关具有 `aria-pressed` 状态，切换不创建网络请求、不显示输入框或发送按钮，也不渲染弹幕或热力图。
- [x] 现有 Direct/HLS/fallback、进度、手势、键盘、Media Session、来源切换及会话停止测试保持通过。
- [x] ArtPlayer 仅作视觉与交互参考；不复制其 DOM、CSS、图标、品牌、演示资产或运行时依赖，并在第三方台账留痕。

验证：组件/页面单测、`pnpm --dir web test`、`pnpm --dir web build`，真实浏览器在 390×844、768×1024、1440×900
检查视觉、可访问名称、console 和网络；更新 `docs/COMPATIBILITY.md`。

依赖：LUX-208。

阶段门：

- [x] LuxPlayer 已拥有独立的 Controller、Engine contract 和 UI 组件，业务代码不依赖 ArtPlayer 包。
- [x] Direct、服务器 HLS、客户端 fallback、版本选择、续播、播放进度和页面离开通过真实浏览器回归。
- [x] 桌面和移动 viewport 的播放控制、手势、Media Session 和错误提示通过验证。
- [x] ArtPlayer 衍生代码来源和 MIT notice 完整；无未记录的复制代码。
- [x] 运行本阶段 Web 检查，并记录 `uname -m`；不将本机 ARM 结果外推为 NAS/x86 性能。

#### LUX-210：LuxPlayer 后续范围与字幕/弹幕合同

范围：在 LUX-203 至 LUX-209 已验证的自有播放器基础上，关闭核心控制层阶段门，并定义下一个阶段的字幕与 Web
弹幕工作顺序、数据边界和验收。此任务只改文档；它不改变 Rust/TypeScript 行为、不新增路由或依赖，也不复制
ArtPlayer 源码。

后续阶段必须先完成字幕轨生命周期，再为 Web 创建独立于 Emby 的弹幕读取合同，最后实现 Lux 自有调度与渲染。ArtPlayer
的 `src/subtitle.js`、`packages/artplayer-plugin-danmuku/src/` 仅作为 MIT 许可下的行为、性能边界和交互参考；复制或
改造任何实现前必须先写入 `docs/THIRD-PARTY-NOTICES.md`。Lux 不得引入 `artplayer` 或其插件作为运行时依赖。

本阶段以已存在的 Lux 播放会话、媒体源流信息、受鉴权字幕端点和已登记弹幕旁车为唯一数据基础。不得将 Emby
`/api/danmu/*` 路由直接给 Lux Web 调用，不得在播放请求中做弹幕匹配、外部请求、整库扫描或旁车写入。

验收：

- [x] LUX-203 至 LUX-209 阶段门按 `docs/COMPATIBILITY.md`、自动化测试和本机 `arm64` 记录关闭；项目所有者已确认进入后续阶段。
- [x] LUX-211 至 LUX-215 各有单一目标、依赖、明确不做项和可执行验证；字幕格式处理、Web 弹幕协议和渲染没有混入同一任务。
- [x] 明确保持“不发送弹幕、无热力图、无远程弹幕上游访问、无服务器字幕转码/烧录、无 ArtPlayer 运行时依赖”的产品边界。

验证：`git diff --check`，人工审阅任务边界与第三方台账；文档任务不需要新增代码测试。

依赖：LUX-209。

### 阶段 17：LuxPlayer 字幕与本地弹幕体验

本阶段只把 Lux 已授权、已索引的本地文本字幕和已登记 Bilibili XML 弹幕带入 Lux Web 播放器。字幕与弹幕在切换媒体源、
停止会话、页面离开、Direct/HLS/fallback 切换时必须一起释放；它们不能影响播放计划、媒体 URL、ACL、进度、心跳或
Media Session。所有 UI 使用 Lux 自有类型、状态、DOM、CSS 和图标。

#### LUX-211：LuxPlayer 字幕轨选择与 WebVTT 生命周期

范围：从现有 `MediaSource.streams` 中识别可用字幕，为 LuxPlayer 提供关闭/选择状态和可访问的控制入口；已声明为
外挂 WebVTT 的轨道使用既有受鉴权 Lux 字幕端点和原生 `TextTrack`。为使字幕严格属于当前版本，现有 Lux 字幕端点
增加可选 `sourceId` 查询参数：省略时保持默认版本优先的既有行为，提供时只接受同时属于 `{itemId}` 的媒体源。
切换来源、退出页面或更换选择时销毁旧 track，不重新创建播放会话。

验收：

- [x] 仅显示当前媒体源的 `SUBTITLE` 流；语言、标题、default/forced 信息可读，且“关闭字幕”始终可选。
- [x] 选择外置 VTT 只请求 `/api/v1/items/{itemId}/subtitles/{streamIndex}?sourceId={mediaSourceId}`；`sourceId` 省略时保持既有默认版本回退，错误/跨条目 ID 返回既有安全失败。播放器不拼接文件路径、外部 URL 或 Emby 路由；无 VTT 或浏览器不支持时安全降级并说明原因。
- [x] 轨道选择在 source/engine/页面生命周期中不残留旧 cue、不改变播放会话或进度事件；键盘和触摸均可操作。
- [x] 不在本任务读取/转换 SRT、ASS/SSA、PGS/SUP 或内嵌字幕，不做样式编辑、数据库迁移或其他服务端行为改变。

验证：字幕 sourceId Rust API/ACL 测试、字幕选择单测、现有播放器会话回归、`pnpm --dir web test`、`pnpm --dir web build`，真实浏览器验证 track 网络请求与切换。

依赖：LUX-210、LUX-208。

#### LUX-212：LuxPlayer 安全文本字幕解析与渲染

范围：为已选的本地 SRT、ASS/SSA 与 VTT 外挂文本轨建立 Lux 自有的有界浏览器解析与覆盖层。解析器只接受由
LUX-211 从已授权字幕流导出的同源字节；它使用文本节点渲染，限制输入大小、cue 数、单条长度和时间范围，并在
Web Worker 中完成重型解析。SRT/ASS 到 cue 的客户端归一化不改变或写回源字幕，不创建服务器字幕转换、烧录或缓存。

验收：

- [x] SRT、ASS/SSA、VTT 的安全测试夹具可产生有序、受限的 Lux cue；格式错误、超限、负/倒置时间、控制字符和标记文本安全失败，不执行 HTML。
- [x] 覆盖层按播放时间显示/隐藏 cue，seek、暂停、倍速、source 变更和 destroy 不显示陈旧内容；渲染不依赖浏览器原生字幕样式。
- [x] ArtPlayer 仅作为 `subtitle.js` 生命周期和转换边界参考；Lux 代码、Worker 协议、DOM、CSS、错误文案和测试均为自有实现，并在台账记录来源状态。
- [x] 不支持 PGS/SUP 图形字幕、在线字幕搜索/下载、服务端转换/烧录或可编辑字幕样式。

验证：解析器/Worker/组件单测、恶意文本回归、`pnpm --dir web test`、`pnpm --dir web build`，真实浏览器检查 source 切换和 console。

依赖：LUX-211。

#### LUX-213：Lux Web 弹幕读取合同

范围：在 Rust Lux API 中增加与 Emby 弹幕路由分离的、ACL 保护的 Web 弹幕元数据和原始 XML 读取端点；只读取已登记的
本地同名 XML 旁车。合同使用可扩展 DTO 和统一 Lux API 错误，不泄露文件路径、上游地址、token 或插件配置。

验收：

- [x] 已授权用户只能看到所拥有条目的 `available`、固定 `BILIBILI_XML` 格式与同源 raw 读取地址；不存在、无权、未登记或故障情形遵循 Lux API 错误边界。
- [x] raw 端点执行现有 ACL、返回受限 XML 和 private no-cache，且不会触发匹配、插件 RPC、上游网络、扫描或旁车写入。
- [x] TypeScript API 类型/客户端是 Rust 合同的显式消费者；不复用或暴露 Emby `/api/danmu/*` DTO。
- [x] Rust API/ACL 测试覆盖授权、拒绝、缺失、无服务与 raw 内容；不实现发送、持久化、实时推送或热力图。

验证：相关 Rust API/ACL 测试、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`、Web API 单测与构建。

依赖：LUX-210、LUX-150、LUX-090。

#### LUX-214：LuxPlayer 弹幕解析、调度与渲染

范围：消费 LUX-213 合同，实现 Lux 自有 Bilibili XML 弹幕解析、时间调度、轨道分配、防重叠和 DOM 覆盖层。默认开关
继续是本实例内的 UI 状态；加载只在可见时发生，切换为不可见、来源/会话切换或 destroy 时取消/丢弃旧结果。

验收：

- [x] 解析器验证并限制 XML、条目数、文本长度、时间、模式和样式值；弹幕文字始终以文本节点渲染，不能执行标记或脚本。
- [x] 滚动、顶部和底部模式在 seek、暂停、倍速、窗口缩放和 source 切换中正确同步；轨道调度防止可见重叠并在高密度数据下有界。
- [x] `aria-pressed` 开关保持可访问，关闭时不请求或渲染；没有输入框、发送按钮、热力图、实时推送、上游匹配或 XML 持久化。
- [x] ArtPlayer 弹幕插件仅作为 lane、生命周期和性能问题的参考；Lux 不复制其 DOM、CSS、图标、网络调用或发送界面，实际来源状态写入台账。

验证：解析/调度单测、组件/会话隔离回归、Playwright 鼠标/触摸/seek 流程、`pnpm --dir web test`、`pnpm --dir web build`。

依赖：LUX-213、LUX-212。

#### LUX-215：LuxPlayer 字幕/弹幕兼容性与性能阶段门

范围：以真实浏览器和固定、无个人数据的媒体/字幕/弹幕夹具验证阶段 17。验证 Direct、服务器 HLS 和客户端 fallback，
并记录性能上限、可访问性、网络边界及真实设备差异；不新增新的解码引擎。现有 LUX-185 Worker/WASM fallback 仍是
浏览器解码增强的唯一承诺，新增 WebCodecs 或 WASM 引擎必须另立 ADR 和任务。

验收：

- [x] 390×844、768×1024、1440×900 下字幕、弹幕、控制栏、安全区、键盘焦点和触摸 seek 不重叠、不产生横向溢出。
- [x] Direct/HLS/fallback 的 source 切换、会话停止、页面离开和错误路径不会保留字幕/弹幕；console 为 0 error/0 warning，网络只包含声明的 Lux 端点。
- [x] 记录浏览器/平台/夹具哈希、解析/调度上限、已验证能力和未验证真机项；本机 `arm64` 结论不外推为 NAS/x86 或所有移动浏览器性能。
- [x] 全部 Rust/Web 质量门通过，第三方台账和 `docs/COMPATIBILITY.md` 更新；项目所有者确认阶段门后才可以再扩展播放器能力。

验证：相关 Rust 测试、`pnpm --dir web install --frozen-lockfile`、`pnpm --dir web test`、`pnpm --dir web build`、Playwright/真实浏览器检查、`cargo build --locked`、`cargo test --locked --all-targets`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`。

依赖：LUX-212、LUX-214。

阶段门：

- [x] 所有 Web 字幕与弹幕请求经过独立 Lux API、会话生命周期与媒体库 ACL；没有外部 URL、文件路径或 Emby DTO 泄露到 Lux Web。
- [x] 文本字幕与弹幕解析/渲染在 Direct、HLS 和客户端 fallback 下通过安全、功能与性能回归。
- [x] 桌面和移动 viewport 下的控制、字幕、弹幕和手势通过真实浏览器验证；真机差异明确记录。
- [x] ArtPlayer 任何复制/改造均有 MIT 追溯；未复制的逻辑标为仅参考，Lux 不依赖 ArtPlayer 包。
- [x] LUX-215 的 Rust/Web 全量检查、兼容性记录和 `uname -m` 完成，并由项目所有者确认。

### 阶段 18：LuxPlayer 默认交互与 Lux 章节整合

本阶段补齐 ArtPlayer 官方首页默认播放器中适合 Lux 的循环、画面比例、镜像、字幕偏移、AirPlay 和控制隐藏态细进度条，
并把 Lux 已有的 source-scoped 章节/片头片尾数据接入播放器。自动续播继续使用 Lux 服务端用户进度，清晰度继续使用
媒体源选择，独立播放路由已经是视觉 viewport 全屏，因此不复制 ArtPlayer localStorage 续播或重复网页全屏按钮。

阶段 18 不加入弹幕发送、热力图、演示页自定义按钮、Chromecast、外置音轨、完整样式 ASS/SSA、新解码器或新播放器依赖。
这些能力若要进入产品，必须另立 ADR 和任务，不得借本阶段修改播放计划或公共模型。

#### LUX-216：LuxPlayer 剩余能力核验与阶段 18 计划

范围：以 ArtPlayer 官方首页、固定源码快照、ADR-029 和当前 LuxPlayer 代码/真实截图为证据，区分“已由 Lux 等价实现”、
“本阶段缺失”和“不是 Lux 产品能力”，并把剩余工作拆成 LUX-217 至 LUX-222。此任务只改文档，不改变运行时行为。

验收：

- [x] 记录 ArtPlayer 首页默认选项、设置菜单、AirPlay 能力门、mini progress 和章节插件的固定源码路径。
- [x] 明确已有播放/音量/时间/倍速/版本/截图/画中画/全屏/续播能力不重复实现，并保留不发送弹幕、无热力图边界。
- [x] LUX-217 至 LUX-222 各有单一目标、依赖、预计文件和可执行验证；没有把新依赖、音轨合同或完整 ASS 渲染混入。

验证：`git diff --check`，人工核对 `docs/LUX-216-PLAN.md`、ADR-029、ArtPlayer 固定 commit 和当前 Web 组件。

依赖：LUX-215。

#### LUX-217：LuxPlayer 循环、画面比例与镜像设置

范围：在现有 Lux 设置面板中增加播放器实例内的循环、`default/4:3/16:9` 画面比例和
`normal/horizontal/vertical` 镜像。只改变当前 video 呈现和结束行为，不创建或替换播放会话，不写服务器设置。

验收：

- [x] 设置项显示当前值并可通过键盘、鼠标和触摸操作；循环使用可访问开关，比例和镜像选项有明确中文名称。
- [x] Direct、HLS 和客户端 fallback 的当前 video 都应用相同设置；source/engine 替换后重新应用，旧 DOM 不保留 transform/尺寸。
- [x] 切换设置不请求网络、不上报虚假进度；关闭循环仍执行原有 `ENDED/STOPPED` 生命周期。
- [x] ArtPlayer `aspectRatioMix.js`、`flipMix.js` 和设置模块只作为边界参考，实际来源状态写入第三方台账。

验证：设置纯逻辑/组件/页面测试、`pnpm --dir web test`、`pnpm --dir web build`，真实浏览器检查三种播放引擎。

依赖：LUX-216。

#### LUX-218：LuxPlayer 字幕偏移

范围：在设置面板增加 -10.0s 至 +10.0s、0.1s 步进的字幕偏移；同时支持原生 WebVTT track 和
Lux SRT/ASS/SSA/VTT 文本覆盖层。偏移只影响当前选中字幕的显示时间，不修改、缓存或写回字幕文件。

验收：

- [x] 原生 cue 和 Lux cue 都以不可累计的原始时间应用偏移，范围裁剪到媒体时长；反复调整不会漂移。
- [x] 关闭/切换字幕、source/engine 变更和 destroy 会恢复/释放旧 cue，不污染下一播放会话。
- [x] 无字幕或字幕尚未加载时设置安全可用并显示明确状态；控件具备 label、当前秒数和键盘路径。
- [x] ArtPlayer `subtitleOffset.js`/`subtitleOffsetMix.js` 仅作生命周期参考，Lux 保持自有解析器和 DOM。

验证：VTT/native track 与覆盖层单测、source 生命周期组件测试、`pnpm --dir web test`、`pnpm --dir web build`。

依赖：LUX-217、LUX-212。

#### LUX-219：LuxPlayer AirPlay 与隐藏态细进度条

范围：按平台能力显示 AirPlay 控件，并在常规控制层自动隐藏时保留无交互 mini progress bar。AirPlay 只调用当前
video 的 WebKit 播放目标选择器；不引入 Chromecast、远程 SDK 或新的媒体 URL。

验收：

- [x] 仅当 `webkitShowPlaybackTargetPicker` 和播放目标可用时显示有可访问名称的 AirPlay 控件；不可用时不显示且无错误。
- [x] AirPlay 调用当前引擎 video，不创建会话、不改写 URL；source/engine 变化后能力监听和引用一起更新/释放。
- [x] 控制层隐藏时显示当前播放/缓冲比例的细进度条，显示控制层、媒体未就绪或直播时按定义隐藏；不抢占 pointer/focus。
- [x] 参考 ArtPlayer `airplayMix.js`、`control/airplay.js`、`miniProgressBar.js` 的边界并更新第三方台账。

验证：平台能力/组件测试、响应式布局检查、`pnpm --dir web test`、`pnpm --dir web build`，Safari 真机差异记入兼容性记录。

依赖：LUX-217、LUX-208。

#### LUX-220：Lux Web source-scoped 章节合同

范围：将现有 `CatalogSource.chapters` 映射到 Lux item DTO 的每个媒体源，TypeScript 增加显式章节类型。
不新增数据库、检测、扫描、插件调用或独立章节端点；请求继续走现有 item ACL。

验收：

- [x] 每个媒体源只返回自己的有序、受限章节：`startPositionTicks`、可选 `name`、`markerType` 和 `chapterIndex`。
- [x] Lux DTO 使用 camelCase 且与 Emby ChapterInfo 分离；无章节返回空数组，不能泄露路径、插件配置或其他 source 数据。
- [x] Lux item ACL、默认/选中 source 和现有 Emby 章节输出回归通过；请求路径不运行检测或文件读取。

验证：`cargo test --locked --test chapters`、相关 API 单测、Web 类型/客户端测试、`cargo fmt --all -- --check`、
`cargo clippy --locked --all-targets --all-features -- -D warnings`、`pnpm --dir web build`。

依赖：LUX-216、现有章节持久化与 Emby 输出。

#### LUX-221：LuxPlayer 章节时间轴与片头跳过

范围：消费 LUX-220 合同，将当前 source 的普通章节、片头开始/结束和片尾开始标记带入时间轴；完整的片头区间显示
“跳过片头”操作。只执行当前引擎 seek，不修改章节或播放会话。

验收：

- [x] 章节按时间排序、去重和限制，时间轴分段/标记可 hover、focus 并显示标题；窄屏不产生横向溢出。
- [x] `INTRO_START/INTRO_END` 完整且当前时间位于区间时显示可访问的“跳过片头”，点击只 seek 到片头结束；
      缺失/倒置标记不猜测。`CREDITS_START` 可见但不伪造片尾结束。
- [x] source/engine/页面切换立即清理旧章节和跳过操作；Direct/HLS/fallback、进度、字幕和弹幕不回归。
- [x] ArtPlayer 章节插件只作为时间轴分段和标题定位参考，Lux 使用自己的章节 DTO、DOM、CSS 和 seek 命令。

验证：章节归一化单测、组件/页面 source 隔离测试、`pnpm --dir web test`、`pnpm --dir web build`，真实浏览器鼠标/键盘/触摸检查。

依赖：LUX-220、LUX-217。

#### LUX-222：LuxPlayer 默认交互与章节阶段门

范围：使用固定、无个人数据夹具验证 LUX-217 至 LUX-221；覆盖 Direct、服务器 HLS、客户端 fallback、source 切换、
三种 viewport、设置、章节、会话清理和网络边界，不新增行为。

验收：

- [x] 390×844、768×1024、1440×900 下设置、mini progress、章节、字幕、弹幕和控制栏不重叠且可访问。
- [x] Direct/HLS/fallback 下循环、比例、镜像、字幕偏移、章节 seek 和 source 切换生命周期通过；console/network 清洁。
- [x] AirPlay 的能力可用/不可用路径有自动化证据，真实 Safari/AirPlay 目标是否验证明确记录，不以 Chrome 结果冒充真机。
- [x] Rust/Web 全量质量门、第三方台账、兼容性记录和 `uname -m` 完成；项目所有者确认后关闭阶段 18。

验证：相关 Rust/Web 测试、`pnpm --dir web install --frozen-lockfile`、`pnpm --dir web test`、
`pnpm --dir web build`、真实浏览器检查、`cargo build --locked`、`cargo test --locked --all-targets`、
`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`。

依赖：LUX-217、LUX-218、LUX-219、LUX-220、LUX-221。

阶段门：

- [x] ArtPlayer 首页适合 Lux 的默认控制和设置已有实现或明确的 Lux 等价能力，没有重复产品功能。
- [x] 当前媒体源章节/片头片尾与 Lux 会话、source、字幕、弹幕和引擎生命周期一致。
- [x] 无 ArtPlayer 运行时、发送弹幕、热力图、外部播放器 SDK、新解码器或未登记衍生代码。
- [x] 真实浏览器、Rust/Web 全量门和兼容性限制记录完成。

阶段 18关闭记录（2026-08-28）：阶段门证据见 `docs/COMPATIBILITY.md` 的 LUX-222 小节。项目所有者已要求继续完成并关闭
本阶段；Safari/AirPlay 真机和 NAS/x86_64 性能保持为明确的后续验证边界。

### 阶段 19：服务端领域模块化维护

#### LUX-223：拆分超大 API、Storage 和 People 实现

范围：在不改变 HTTP 路由、DTO、领域模型、数据库 schema、SQL 语义和运行时行为的前提下，
将 `src/api/mod.rs`、`src/storage/mod.rs` 和 `src/application/people.rs` 的实现按领域移动到子模块。
facade 只保留模块声明、共享状态/类型、路由组合和稳定 re-export；领域模块负责自己的 handler、DTO 映射、
repository 方法或 People 用例。此任务不引入新依赖、不新增端点、不修改迁移，也不提前实现其他 LUX 任务。

验收：

- [x] API facade 不再承载完整领域 handler；Emby 路由/DTO、Lux API、管理员、用户、媒体和播放实现位于明确子模块。
- [x] Storage facade 不再承载完整 SQL repository；媒体、人物、会话、迁移和共享查询/模型边界清晰。
- [x] PeopleService 的关系/匹配、元数据、资源和索引任务实现分离，外部调用路径保持不变。
- [x] 现有 Rust/Web 行为测试不变且通过；模块移动没有改变公开 HTTP 合同、错误码或数据库行为。（最终 Rust/Web 全量质量门通过；此前曾观察到 `tests/users.rs::admin_can_manage_users_and_last_manager_is_protected` 的非确定性 503，见下方记录。）
- [x] 每个增量独立可编译、可回滚，并记录模块边界和未纳入本任务的后续拆分。

验证：每个增量运行对应的窄 Rust 测试和 `cargo check --locked`；任务完成时运行 `cargo build --locked`、
`cargo test --locked --all-targets`、`cargo fmt --all -- --check` 和 `cargo clippy --locked --all-targets --all-features -- -D warnings`。

依赖：LUX-222 阶段门已关闭。

阶段门：

- [x] 三个超大入口文件均降为 facade 或共享模型层，单个领域实现文件保持可审阅规模。
- [x] API、Storage 和 People 的模块边界已由 ADR-030 记录，未改变模块化单体部署边界。
- [ ] 全量 Rust 质量门通过，并由项目所有者确认后再进入下一阶段。（质量门已通过，待项目所有者确认。）

验证记录（2026-08-28）：`src/storage/repository.rs` 已降至约 2,660 行，Storage Repository 方法拆至
`catalog.rs`、`jobs.rs`、`library.rs`、`media.rs`、`metadata.rs`、`migration.rs`、`notifications.rs`、
`people.rs`、`sessions.rs` 和 `users.rs`，最大领域文件约 5,100 行；共享模型、数据库初始化、SQL 适配和错误仍由
`repository.rs` 持有。`uname -m` 为 `arm64`；`cargo build --locked`、`cargo fmt --all -- --check`、
`cargo clippy --locked --all-targets --all-features -- -D warnings`、Storage 定向测试以及 Web 安装/测试/构建
均通过。最终 `cargo test --locked --all-targets` 通过（库测试 285 passed、1 ignored，所有集成目标通过）；此前
一次全量运行和一次隔离重复运行曾收到用户管理测试 503，但最终全量复跑通过，说明该测试仍存在启动/后台任务
时序不稳定风险，未在本任务范围内修改测试行为。此 ARM64 结果不外推 NAS/x86_64 性能。

### 阶段 20：内嵌文本字幕与远程 STRM 能力边界

本阶段只处理文本字幕（SRT、ASS、SSA）的发现、按需抽取和浏览器侧显示，不处理 PGS/SUP 图形字幕。字幕是附着于
当前媒体源的独立展示能力：切换字幕不能重新创建播放会话，不能改变 Direct/HLS/fallback 计划、媒体 URL、ACL、进度、
心跳或停止语义。本阶段的远程字幕能力由 ADR-038 规定：远程 `.strm` 默认保持 Direct Play，只有用户明确选择 URL 型 HTTP(S)
Matroska/WebM 文本字幕后才启动客户端单管线；远程不做默认解封装，不做服务端抽取。

浏览器优先级固定为：首先使用实际运行时暴露的 `HTMLVideoElement.textTracks`；本地媒体未暴露内嵌轨时，再从 Lux 已授权的
source-scoped 字幕端点按需抽取文本字幕；远程 HTTP(S) Matroska 在用户选择文本轨后使用 ADR-038 单管线，其他远程 STRM 仍只尝试
原生轨道。ffprobe 的轨道列表只用于索引和能力提示，不能作为浏览器一定能读取内嵌轨的证明。

#### LUX-224：内嵌文本字幕规格与 ADR-032

范围：记录本阶段字幕合同、媒体源隔离、远程 `.strm` 边界、ArtPlayer 官方实现核验结论和任务依赖。只改规格和 ADR，
不改运行时行为、不新增路由、不新增依赖、不改数据库。

验收：

- [ ] 明确支持本地内嵌 SRT/ASS/SSA 的按需文本抽取；首版不支持 PGS/SUP、服务器烧录、HLS 字幕组和完整 ASS 样式。
- [ ] 明确浏览器原生 `TextTrack` 优先，以及远程 `.strm` 不拉取、不代理、不启动 ffmpeg、默认不启用实验管线。
- [ ] 明确字幕选择不影响播放会话、媒体 URL、tier、HLS、进度、心跳、停止和 ACL；source-scoped 合同不泄露路径或外部 URL。
- [ ] ADR 记录 ArtPlayer 核心字幕模块只加载外部 `subtitle.url`；JASSUB/Mediabunny 属于额外浏览器管线，不能推导普通
      `<video>` 能解封装远程 MKV 内嵌字幕。

验证：`git diff --check`，人工审阅规格和 ADR。

依赖：LUX-223 阶段门确认后进入。

#### LUX-225：source-scoped 字幕流查询合同

范围：为当前媒体源查询内嵌字幕流提供稳定的 Lux application/storage 合同。复用已有媒体流信息和 item ACL，不做数据库
迁移，不读取远程 `.strm` 目标，不在 HTTP handler 中执行 SQL 或媒体扫描。

验收：

- [ ] 查询只返回属于 `{itemId, sourceId}` 的字幕流，并区分 `embedded`、`external`、格式、语言、标题、default、forced
      和当前可用性；省略 `sourceId` 时保持既有默认源回退。
- [ ] 字幕流列表分页并有服务端上限；跨条目/跨源 ID、无权限和不存在资源使用既有安全错误边界，不暴露本地路径、原始
      `.strm` 文本、令牌或完整外部 URL。
- [ ] 对远程 `.strm` 不触发 HTTP/SMB/FTP 读取、ffprobe、ffmpeg、代理或字幕专用重定向；播放合同和既有外挂字幕端点不回归。

验证：`cargo test --locked --test subtitles`，相关 API/ACL 测试，`cargo fmt --all -- --check`。

依赖：LUX-224。

#### LUX-226：本地内嵌文本字幕按需抽取

范围：为本地、已授权、可读取的媒体提供 SRT/ASS/SSA 内嵌轨的按需无转码读取。抽取在 application/service 边界完成，
使用有界阻塞 worker，结果通过 source-scoped 字幕端点返回给 Web Worker；不写回媒体、不生成永久缓存、不处理 PGS/SUP。

验收：

- [ ] 只接受已索引且属于当前 item/source 的文本字幕流；路径 canonicalize 后仍必须位于已配置媒体根或既有允许的本地 `.strm`
      目标边界内，目录、另一个 `.strm`、远程 URL 和未知协议拒绝。
- [ ] 抽取有文件大小、读取时长、输出字节和并发上限；SRT/ASS/SSA 原始文本保持可解析，格式无效、超限、取消和读取失败
      返回可诊断但不泄露路径的错误。
- [ ] 读取只发生在用户请求的选定字幕轨，未选择字幕不触发抽取；本地视频 Direct/HLS/fallback 和外挂字幕行为不改变。

验证：`cargo test --locked --test subtitles`，`cargo build --locked`，相关取消/上限测试。

依赖：LUX-225。

#### LUX-227：浏览器原生 in-band TextTrack 探测

范围：在 LuxPlayer 当前视频 surface 中探测当前 video 实例真实暴露的 `textTracks`，把可用内嵌文本轨并入已有字幕选择器。
探测只读浏览器运行时状态，不猜测、不下载媒体、不创建额外字幕请求；track 随 source、engine、页面和 destroy 生命周期释放。

验收：

- [ ] 只接受当前 video 的实际 `TextTrack`，区分 native in-band 与 Lux 外挂 track，标签、语言、default、forced 和 mode 显示正确。
- [ ] 选择 native track 只切换 `mode`/当前显示状态，不调用播放会话 API，不改变媒体 URL、请求头、tier、进度或心跳。
- [ ] native 轨道不存在、浏览器不暴露、轨道格式无法渲染或 track 事件异常时，视频继续播放并显示字幕不可用原因；不影响
      SRT/ASS/SSA overlay fallback。

验证：`pnpm --dir web test`、`pnpm --dir web build`，组件生命周期测试和真实浏览器 console/network 检查。

依赖：LUX-225、LUX-226。

#### LUX-228：单次媒体读取字幕解析实验（由 ADR-038 收敛）

范围：历史实验不单独进入当前运行时；远程字幕读取由 ADR-038 的正式显式字幕管线承接。本任务保留为决策记录，不单独新增
第二条媒体读取或字幕专用连接。

验收（历史方案，不再单独执行）：

- [ ] 默认关闭；开启前必须满足 CORS、Range、媒体类型、读取上限、生命周期取消和可用解析器条件，任何条件不满足立即跳过。
- [ ] 解析失败、网络中断、资源一次性 UA/令牌绑定或浏览器不支持时，视频保持原有 Direct Play；不回退到 Lux HLS、媒体代理
      或 302/Redia 字幕接口，不重试第二条远程媒体连接。
- [ ] 实验结果与视频请求、字幕来源、字节上限和失败原因可诊断但不记录完整 URL、令牌、Cookie 或媒体内容；实验不处理 PGS/SUP。

验证：Web 单测覆盖开关、能力门、取消、失败回退和单连接约束；`pnpm --dir web test`、`pnpm --dir web build`。

依赖：LUX-227。

#### LUX-229：本地/远程 `.strm` 字幕兼容性阶段门（历史记录，已由 LUX-245 取代）

范围：阶段门使用固定、无个人数据的媒体夹具，分别验证本地媒体、URL 型远程 `.strm`、
路径型远程 `.strm`、浏览器原生轨道、按需
本地抽取和实验关闭/失败路径。只记录已验证能力，不扩大 `.strm` 播放合同。

验收：

- [ ] 本地内嵌文本字幕可按轨选择并与 Direct/HLS/fallback 生命周期一致；PGS/SUP 明确显示不支持且视频仍可播放。
- [ ] （历史方案）远程 URL/path `.strm` 未选择字幕时仍由播放器/外部代理按既有规则直连；旧方案仅允许 URL 型 HTTP(S) Matroska
      在明确选择文本轨后通过当前播放会话的有限 Range Relay 读取。该 Relay 约束已由 LUX-245 的浏览器 `externalUrl` 直连取代，
      当前实现不能据此验收；仍不允许调用 ffmpeg/ffprobe 或创建服务端媒体代理。
- [ ] source 切换、seek、停止、页面离开和失败回退不残留字幕；兼容性记录包含浏览器、平台、夹具哈希和请求边界。
- [ ] 阶段 Rust/Web 全量质量门通过，并记录 `uname -m`；本机 ARM64 结果不外推 NAS/x86_64 性能，项目所有者确认后关闭阶段。

验证：`pnpm --dir web install --frozen-lockfile`、`pnpm --dir web test`、`pnpm --dir web build`、`cargo build --locked`、
`cargo test --locked --all-targets`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`、
真实浏览器 console/network 检查，并更新 `docs/COMPATIBILITY.md`。

依赖：LUX-226、LUX-227、LUX-228。

阶段门：

- [ ] 本地文本字幕与浏览器 native track 均不改变播放会话和 `.strm` Direct Play 边界。
- [ ] （历史方案，已取代）远程 `.strm` 未选择字幕时没有 Lux 媒体字节流量；显式字幕仅允许同源 Range Relay。当前验收以 LUX-245
      为准：显式字幕和客户端解码均由浏览器直连 `externalUrl`，不增加服务端字幕抽取、ffmpeg 或 302/Redia 字幕专用合同。
- [ ] PGS/SUP、服务器烧录、HLS 字幕组和完整 ASS 样式未被隐式加入，所有未验证浏览器能力均已记录。
- [ ] Rust/Web 全量质量门、兼容性记录、本机架构记录和项目所有者确认均完成。

#### LUX-230：全量扫描中的本地旁车流水线

全量扫描按媒体文件夹持续建立可用视频源。单个文件夹的视频源和扫描目标在有界事务中提交；用户首页在扫描期间保持上一份稳定快照，完整 Manifest 索引和缺失确认完成后统一切换；
本地 NFO、海报、背景图和其他已存在的本地图片由独立、有界的旁车 worker 并行读取并写入索引，扫描 worker
立即继续下一个文件夹。旁车 worker 不进行在线匹配、不调用 TMDb、不下载缺失图片，也不持有文件扫描互斥锁。

旁车目标在数据库中复用现有扫描目标状态，首页只在目标仍待处理时返回 `localMetadataPending`。Web 卡片没有
图片且该状态为真时显示占位图和动态等待图标；旁车完成、没有可用图片或处理失败后停止等待，保留普通占位图。
本地旁车更新完成会发布首页失效事件，使已显示的条目及时获得本地元数据和图片。

验收：

- [ ] 全量扫描期间首页保持上一份稳定快照；所有可用根路径完成 Manifest 索引和缺失确认后、后处理完成前，首页原子切换到已提交目录结果。
- [x] 本地旁车读取与下一个文件夹的发现、索引并行执行；旁车慢或失败不阻塞扫描进度。
- [x] 已存在的本地 NFO、海报和图片只读取并登记，不发起在线匹配、TMDb 请求或缺失图片下载。
- [x] 首页在旁车处理期间返回 `localMetadataPending`，无图片时显示可访问的占位等待状态；旁车完成后首页刷新并显示本地图片、标题、年份和简介。
- [x] 没有本地海报或旁车处理失败时，等待状态结束且继续显示普通占位图。
- [x] 进程重启会取消遗留作业；取消、失败和管理员重试不会重复完成已提交的旁车目标。

验证：

- `cargo test --locked --test scanning_jobs --test scanned_metadata --test catalog`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `pnpm --dir web test`
- `pnpm --dir web build`
- `uname -m`（本机结果：`arm64`）

依赖：LUX-154、LUX-187、LUX-197、LUX-200。

明确不做：

- 不在本任务实现在线元数据匹配、缺失图片下载、刮削器请求或 TMDb 调用。
- 不把旁车读取放回用户请求路径，不因旁车处理而串行暂停全量扫描。

后续规格演进：本任务勾选项记录 LUX-230 当时的验收结果。新建扫描的前台可见时点、本地旁车工作启动时点和在线补缺边界由阶段 23 / LUX-288 另行规定；既有 workflow 1/2 的恢复语义不因新合同改变。

#### LUX-231：LuxPlayer 剧集集间导航

范围：为 Lux Web 播放器增加剧集单集的“上一集”和“下一集”控制。播放器只在当前条目是单集时，复用已有剧集单集查询合同读取同一季度的可播放单集并按服务端顺序定位相邻条目；电影和其他媒体类型不显示这两个控件。

验收：

- [x] 单集播放时左下角控制栏显示带可访问名称的“上一集”和“下一集”；首集/末集对应按钮置灰，查询失败或尚未完成时不导航。
- [x] 点击按钮进入相邻单集的 `/watch/{itemId}` 路由，旧播放会话按既有页面切换生命周期停止，新单集使用默认媒体源；不拼接媒体 URL、不改变播放会话 API 或进度合同。
- [x] 只把同一剧集、同一季度且存在媒体源的单集纳入导航；电影、剧集容器、季度和无权/不可播放条目不显示或不能导航。
- [x] 按钮具备键盘路径、焦点样式和明确的中文 `aria-label`/`title`，不造成桌面或窄屏控制栏横向溢出。

验证：播放器组件单测、剧集播放导航组件/页面测试、`pnpm --dir web test`、`pnpm --dir web build`，真实浏览器检查首集/中间集/末集和电影播放页。

依赖：现有 LUX-198 Web 播放会话、LUX-206 播放器 UI 与 LUX-220 的剧集单集查询合同。

明确不做：

- 不新增 Rust 路由、数据库字段、自动播放策略或跨季度/跨剧集的播放队列。
- 不改变账户设置中的“自动播放下一集”开关语义；本任务只提供显式按钮导航。

#### LUX-232：数据库生命周期清理与写入膨胀控制

数据库迁移完成后，Lux 在容器启动时后台自动执行一次幂等清理，并使用数据库标记记录完成状态；清理失败或进程中断时，下一次启动可以重试。清理不执行需要长时间独占数据库的全量压缩操作。

验收：

- [x] 升级迁移删除 `filesystem_entries` 上与唯一约束重复的显式索引，并为已有数据库写入一次性清理标记；空库和 SQLite/PostgreSQL 均可从迁移起点完成升级。
- [x] 启动清理删除已完成扫描任务的 `scan_job_paths`、`reconciliation_scan_entries`，只删除终态任务中不再需要重试的 `scan_job_targets`，并将终态任务游标压缩为轻量摘要；运行中、后处理和仍可恢复的失败任务数据必须保留。
- [x] `scan_job_events` 只保留 7 天内的 `WARN/ERROR`；扫描 INFO 过程事件不再持久化，事件保留清理在启动和新告警写入时执行。
- [x] `person_credits` 刷新使用去重、差量删除和带变化条件的 UPSERT，未变化的关系不重复删除/插入/更新；实时文件变更采用防抖合并，避免每个事件产生完整扫描任务。
- [x] 旧版本升级后的数据库清理由 Lux 容器自动触发，不依赖助手或管理员手工执行 SQL。

验证：

- `cargo build --locked`
- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `uname -m`，并明确本机 ARM64 结果不外推 NAS/x86_64 性能。

依赖：LUX-154、LUX-187、LUX-188、LUX-189。

明确不做：

- 不连接或直接修改用户线上 FNOS 数据库；本任务只交付迁移和容器启动逻辑。
- 不删除运行中或失败后仍有待重试目标的扫描数据，不执行 `VACUUM FULL` 或等价的长时间独占式压缩。

#### LUX-233：关闭或重启时取消未完成后台作业

关闭或重启 Lux 时，当前未完成的持久化后台作业视为被用户丢弃，不在下一次启动时自动继续。任务记录保留，
以便管理员查看关闭原因并主动重试；已提交的数据库状态和文件写回不回滚。该语义覆盖扫描、媒体探测、章节
检测、媒体库封面、弹幕匹配、元数据重新识别、Emby 导入和人物索引重建。计划任务配置、插件安装状态、用户
数据、媒体索引和 Webhook 投递 outbox 不属于被取消的作业实例。

服务启动并完成数据库迁移后，先在一个事务中将上一次异常退出遗留的活动作业，以及扫描中仍处于
`COMPLETED + POSTPROCESSING` 的作业，标记为 `CANCELLED`，并记录稳定错误码 `SERVER_SHUTDOWN`。优雅关闭时，
在关闭数据库连接前再次执行同一清理，覆盖关闭窗口内的活动作业。元数据识别任务按 LUX-572 恢复原任务中
未完成的子项；其他作业仍保留取消记录并等待既有主动重试或后续计划任务。

元数据识别恢复（LUX-572）：被取消的任务历史保留；启动后将 `SERVER_SHUTDOWN` 元数据任务中仍未完成的子项
原子地恢复到同一个任务 ID。`FILL_MISSING` 仅在媒体库仍启用且仍选中刮削器时恢复；已完成条目不重复刮削。
同时，元数据作业不得在仍有 `PENDING`
条目时记为完成（以 `FAILED` / `QUEUE_INCOMPLETE` 结束），无法派发的作业记为 `FAILED` / `DISPATCH_FAILED`
而不是长期停留在 `QUEUED`。

验收：

- [ ] 八类持久化作业全部在同一个数据库事务中标记为 `CANCELLED`，错误码为 `SERVER_SHUTDOWN`，任务历史保留。
- [ ] 启动清理上一次异常退出的活动作业；正常关闭在数据库关闭前再次清理，清理后活动作业查询为空。
- [ ] 服务启动不再自动领取 `PENDING`、`RUNNING` 或扫描后处理作业；管理员主动重试仍可重新排队。
- [ ] 扫描后处理、插件任务、人物索引和跨作业唯一约束不回归；Webhook outbox 继续使用独立投递重试语义。
- [ ] 覆盖数据库事务、异常重启、优雅关闭、任务重试和错误码的 Rust 测试，并通过格式化、Clippy 和完整项目检查。

验证：

- `cargo test --locked --test scanning_jobs --test reidentify`
- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `uname -m`，并明确本机 ARM64 结果不外推 NAS/x86_64 性能。

依赖：LUX-041、LUX-146、LUX-150、LUX-154、LUX-173、LUX-175、LUX-188、LUX-189、LUX-232。

明确不做：

- 不删除任务历史，不回滚已经提交的索引、元数据或旁车写回。
- 不取消播放会话，不改变登录会话或 Webhook 投递 outbox 的独立恢复策略。

#### LUX-234：通用外部代理的 URL 型 `.strm` 交接与 Emby 数字条目 ID

范围：修正 URL 型 `.strm` 与本地路径型 `.strm` 在第三方媒体代理场景下的 Emby 播放源合同。Emby 条目 `Path`
返回该条目已索引的 `.strm` 文件系统路径，`MediaSources[].Path` 保留 STRM 原始目标；在 `PlaybackInfo` 中使用标准带短期票据的 `DirectStreamUrl`；HTTP(S) URL 型目标使用
`Protocol=Http`、`IsRemote=true`，本地路径型目标使用 `Protocol=File`、`IsRemote=false`。为兼容所有可能丢失独立媒体请求鉴权的第三方播放器，URL/路径型目标的
`AddApiKeyToDirectStreamUrl=true`，并将本次标准 Emby 用户 token 作为 `api_key` 写入同一签名 URL；本地文件和 SMB/FTP
解析源不携带长期 token。无论提示取值如何，Lux 都要求短期票据，使具备自身映射或 302 能力的外部代理可以从原始 `Path`
提取信息并优先接管播放。这样客户端请求始终回到当前公网代理域名，不会直接访问 `.strm` 中的内网 302 地址。
无头媒体请求 URL 中的标准 `UserId` 仅作为外部代理的本地用户名提示，使用 Lux 登录用户名；Lux 内部 UUID
只保存在 `luxPlaybackUserId` 绑定的短期 HMAC 票据中，不能用该提示字段替代授权。Lux 不绑定具体代理品牌，
也不在扫描或 `PlaybackInfo` 请求中访问 `.strm` 目标。

Emby 兼容层对外统一使用由内部 UUID 无状态编码得到的稳定纯数字媒体条目 ID；已有数据库条目不需要迁移，
Lux 内部 UUID、数据库关系和 Lux 原生 `/api/v1` ID 保持不变。所有接收媒体条目 ID 的 Emby 详情、目录过滤、
剧集关系、PlaybackInfo、视频/字幕/图片/下载入口、进度回调和已看/收藏接口都必须把该数字 ID还原为内部 UUID，
并继续接受历史 UUID 请求。Emby DTO 中的 `Id`、`ItemId`、`ParentId`、`SeriesId`、`SeasonId`、媒体库条目 ID、
图片引用 ID 和标准视频 URL 使用数字表示；媒体源自身的 `MediaSourceId` 不在本次转换范围内。

Emby 标准视频入口的 URL 型 `.strm` 交接返回 302，并将原始 HTTP(S) 目标交给客户端；路径型目标由 Lux 按相对路径或绝对路径读取本地普通文件。
Lux 自有播放回退保持使用入站播放器 User-Agent 有限跟随重定向并返回 307；外部代理接管时不应请求该回退入口。SMB/FTP
解析器和其他不支持的目标不在本任务内改变。

验收：

- [x] URL 与路径型 `.strm` 的 Emby 条目 `Path` 返回已索引的 `.strm` 文件系统路径，`MediaSources[].Path` 保留原始目标；路径只通过已授权条目 DTO 暴露。`PlaybackInfo` 中代理交接所需的
      `Protocol`、`IsRemote`、标准带短期票据的 `DirectStreamUrl` 和权限行为一致；Emby URL 型 `.strm` 交接返回 302，签名直放 URL 带入已识别的 `DeviceId`；URL/路径型 `.strm` 对所有第三方播放器的
      `AddApiKeyToDirectStreamUrl=true`，并将本次标准 Emby 用户 token 作为 `api_key` 写入签名 URL；本地文件和 SMB/FTP
      解析源不携带长期 token。
- [x] URL 与路径型 `.strm` 的 Lux Web Direct Play 计划均提供标准 `proxyUrl`；播放器继续在代理失败时回退到签名 Lux URL。
- [x] Emby URL 型 `.strm` 入口返回 302；Lux 自有 URL 型 `.strm` 回退仍按播放器 User-Agent 有限解析并返回 307；路径型 `.strm` 仍提供本地 Range/HEAD 文件响应。
- [x] 扫描、`PlaybackInfo` 和外部代理交接测试不访问原始目标；不新增数据库字段、迁移、媒体字节代理、转码或具体代理适配。
- [x] Emby 兼容层对已有和新建媒体条目统一输出稳定纯数字 ID；输入边界兼容数字 ID 与历史 UUID，内部数据库和 Lux API 不变。
- [x] 数字 ID 兼容覆盖标准媒体详情、目录父子查询、PlaybackInfo、视频/字幕/图片/下载入口、进度回调以及已看/收藏操作；
      SMB/FTP 的目标解析和 URL/Path STRM 的 `MediaSources[].Path` 原始目标保持不变。

验证：

- `cargo test --locked --test strm --test web_playback --test strm_resolver_playback`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `pnpm --dir web install --frozen-lockfile`
- `pnpm --dir web test`
- `pnpm --dir web build`
- `uname -m`，并明确本机 ARM64 结果不外推 NAS/x86_64 性能。

依赖：LUX-159、LUX-161、LUX-198、LUX-199。

明确不做：

- 不删除 Lux 直接播放 URL 型 `.strm` 的现有 307 回退。
- 不实现任何第三方代理的路径映射、302 API、缓存、媒体字节代理或转码。

### 阶段 21：远程 STRM 浏览器直连与客户端解码 fallback

远程 HTTP(S) STRM 的 Web Direct Play 直接使用 `externalUrl`，不使用 Lux 的 `proxyUrl`、`rangeUrl` 或签名 Direct URL。原生 `<video>`
仍是第一路径；浏览器原生能力不足时，才由客户端 WASM/WebCodecs 管线从同一 `externalUrl` 读取。用户明确选择 URL 型
HTTP(S) Matroska/WebM 的 SRT/ASS/SSA 后，字幕旁路读取器也直接读取 `externalUrl`；所有远程媒体字节都不经过 Lux。
直连、CORS/Range、WASM/WebCodecs 或解析失败只显示能力错误，不切换到 Lux Relay/HLS。详细决定记录在 ADR-040，ADR-039
保留为已取代的 Relay 方案历史记录。

> 说明：下列 LUX-235 至 LUX-243 保留为此前的远程字幕 Relay 方案记录；其未完成的验收条件不再是当前实现目标，远程媒体边界以
> LUX-245 和 ADR-040 为准。

#### LUX-235：远程 Matroska 客户端管线规格与 ADR-035

范围：更新远程字幕产品合同，新增 ADR-035，标记 ADR-032 的远程部分被取代，并明确 Range、Cues、字幕旁路、字幕 cue、错误终止和安全上限。
远程音视频继续由原生 `<video>` 负责，不把 MSE codec 作为字幕可用性的前置条件。只改规格和 ADR，不改运行时。

验收：

- [ ] 规格明确 JS 负责读取/解封装字幕，`<video>` 负责原生音视频输出与渲染；不预抽取、不落盘、不生成外挂字幕；只允许受播放会话签名保护的有限
      Range Relay，不提供通用服务端媒体代理。
- [ ] 明确 HTTP(S) 范围、单逻辑读取器、顺序 Range、SeekHead/Cues 必须存在，以及失败直接判定不支持的策略。
- [ ] 明确支持的 Matroska TrackType、文本字幕 codec、音视频 codec、基础 ASS/SSA 样式、内存/元素上限和错误脱敏边界。
- [ ] ADR-032 保留本地字幕和 native TextTrack 决定；远程显式字幕接入和原生默认以 ADR-039 为准。

验证：`git diff --check`，人工审阅规格和 ADR。

依赖：LUX-234。

#### LUX-236：SeekHead/Cues 索引与字幕解封装

范围：扩展浏览器 Matroska 解封装器，支持 TrackUID、语言、default/forced、BlockGroup、BlockDuration、ReferenceBlock、
DiscardPadding、SeekHead 和 Cues；不顺序扫描远程文件。

验收：

- [ ] TrackType 17 作为 subtitle，支持 S_TEXT/UTF8、S_TEXT/ASS、S_TEXT/SSA，字幕时间使用 BlockDuration。
- [ ] Cues 可以按视频轨选择目标 Cluster；缺少有效 SeekHead/Cues、越界偏移、循环引用或资源变化会返回稳定错误。
- [ ] 分块写入与任意字节边界下结果一致；不安全 VINT、未知过大元素、加密/不支持 ContentEncoding 和字幕 lacing 被拒绝。

验证：`pnpm --dir web test -- matroska-demuxer matroska-range-index`，`pnpm --dir web build`。

依赖：LUX-235。

#### LUX-237：Matroska 文本字幕与安全 ASS/SSA 样式模型

范围：解析字幕样本和 CodecPrivate 全局头，扩展 LuxCaptionCue 为多 cue、layer、位置、对齐和安全文本 runs；SRT/VTT 既有
行为保持兼容。

验收：

- [ ] 支持 UTF-8、ASS/SSA ReadOrder/Style/Text；样式只允许颜色、粗体、斜体、对齐、margin 和 pos。
- [ ] 禁止动画、move、karaoke、clip、旋转、字体嵌入和 drawing 输出；React 不使用 `innerHTML`。
- [ ] 超长文本、控制字符、非法颜色/位置、非法 UTF-8 和超过 cue/内存上限的输入可诊断拒绝。

验证：字幕 parser 单测和 Web 构建。

依赖：LUX-236。

#### LUX-238：多 cue 和基础 ASS/SSA 覆盖层

范围：升级字幕 overlay，支持多个同时活动 cue、layer/read-order、对齐定位和校验后的 inline style；生命周期切换时释放旧
cue 和 Worker。

验收：

- [ ] SRT/VTT 旧 overlay 不回归；ASS/SSA 的颜色、粗体、斜体、对齐和位置可见。
- [ ] seek、切源、fallback/错误和页面离开不会残留 cue、状态或 AbortController。
- [ ] 字幕文本作为 React 文本节点渲染，不执行标签或 CSS。

验证：`pnpm --dir web test`、`pnpm --dir web build`。

依赖：LUX-237。

#### LUX-239：多 codec fMP4 remux

范围：扩展 Worker/Muxer 处理 H.264、HEVC、VP9、AV1 与 AAC、AC-3、E-AC-3、Opus，保留 HEVC WASM→H.264 fallback。

验收：

- [ ] 正确生成 AVC/HEVC/VP9/AV1 配置盒及 AAC/AC-3/E-AC-3/Opus 音频描述盒。
- [ ] 使用 Matroska decode order 构造单调 DTS，保留 PTS/DTS composition offset，不静默丢失音频轨。
- [ ] 完整 codec 组合通过 `MediaSource.isTypeSupported` 才创建 SourceBuffer；不支持组合返回终止错误。

验证：fMP4 盒结构、codec 字符串、时间戳和 HEVC fallback 单测，Web 构建。

依赖：LUX-236。

#### LUX-240：顺序 Range、缓冲和 Cues seek

范围：增加主线程 Range 协调器和 Worker range-request 协议；首段 1 MiB，单次 Range 最大 32 MiB，同代最多一个在途请求；
实现 10/30 秒前方缓冲、60 秒后方保留和 Cues seek。

验收：

- [ ] 206、Content-Range、总长度和可选 ETag 校验失败会停止播放；不发 HEAD，不顺序读取整部文件。
- [ ] seek 会取消旧请求、清理旧 MSE/cue generation，并从最近关键帧重新开始；旧 Worker 消息不能污染新代。
- [ ] 失败不创建第二个视频连接、不调用字幕端点或服务端 HLS；撤销远程字幕选择并恢复同一播放计划的原生 `proxyUrl`，必要时沿用既有签名 Lux Direct 回退。

验证：Range 协调器、取消、缓冲、seek 和资源变化测试；`pnpm --dir web test`、`pnpm --dir web build`。

依赖：LUX-239。

#### LUX-241：字幕字符串 ID 与引擎字幕控制器

范围：将 `PlayerCaptionOption` 主键从 streamIndex 改为字符串 ID，增加可选 `PlaybackEngine.captionController`，允许运行时
轨道发布、选择和 cue 订阅；本地 source-scoped 字幕 URL 保持兼容。远程原生音视频字幕由 ADR-039 的旁路读取器直接发布 cue，
不要求切换 `PlaybackEngine`。

验收：

- [ ] 远程 Matroska 头部轨道被映射为 `mkv:${TrackUID}` 或源内 TrackNumber fallback，灰色远程字幕项变为可选。
- [ ] 字幕切换不创建播放会话、不更改 URL/tier/进度/心跳/停止；默认轨与 forced/default 标记正确。
- [ ] source/engine/destroy 生命周期完整清理运行时轨道和 cue。

验证：字幕组件、设置面板和引擎控制器测试，Web 构建。

依赖：LUX-238、LUX-240。

#### LUX-242：远程 Matroska 播放接入与终止错误

范围：远程 HTTP(S) Matroska 始终保持原生音视频播放；用户显式选择字幕后才进入字幕旁路；将 Relay、Range、索引和字幕解封装失败映射为
“远程字幕不可用”，不切换 MSE、不销毁引擎、不恢复或重建媒体。

验收：

- [ ] 远程 Matroska 无字幕选择时保持 native `<video>`；选择远程文本字幕后只启动签名 Range 字幕旁路。
- [ ] 字幕旁路失败只清除字幕，不触发 HLS、字幕端点、MSE 或播放器引擎失败；错误消息不包含完整 URL、令牌、Cookie 或媒体内容。
- [ ] 远程字幕切换、暂停、seek、停止和页面离开均不重建播放会话。

验证：Web 播放、fallback、STRM 字幕兼容性测试和真实浏览器 network/console 检查。

依赖：LUX-241。

#### LUX-243：远程 Matroska 兼容性阶段门（阶段门待复测）

范围：使用固定、无个人数据的 H.264+AAC+SRT、HEVC+E-AC-3+ASS、VP9+Opus+SSA、AV1+AAC+ASS 夹具，记录浏览器、平台、
请求边界、夹具哈希、实际 codec 能力和性能；更新 `docs/COMPATIBILITY.md`。

验收：

- [ ] Chrome、Firefox、Safari 分别只记录真实可播放的 codec 组合，不把 `isTypeSupported` 单独当作成功。
- [ ] （历史方案，已由 LUX-245 取代）确认无字幕端点请求、无第二条原生媒体连接、无服务端抽取/ffmpeg/通用媒体代理流量；旧方案显式字幕只使用同源 Range Relay。
- [ ] `pnpm --dir web install --frozen-lockfile`、Web 全量测试/构建、Rust 全量质量门、`uname -m` 均通过；ARM64 结果不外推 NAS/x86。
- [ ] 项目所有者确认阶段门后才关闭本阶段。

依赖：LUX-242。

#### LUX-245：远程 STRM 浏览器直连与客户端解码 fallback

范围：修正 Lux Web 对远程 HTTP(S) `.strm` 的媒体边界。原生播放、客户端 Matroska/HEVC WASM fallback 和远程字幕读取器
均直接使用媒体源的 `externalUrl`；Lux 只创建/维护播放会话、记录进度并提供权限控制，不接收远程视频、音频或字幕媒体字节。
本地媒体和路径型 `.strm` 的现有 Lux 受保护播放/代理兼容行为保持不变。

验收：

- [ ] 远程 HTTP(S) `.strm` 的原生 `<video>`、`ClientMkvEngine`、`ClientHevcEngine` 和 `RemoteMkvCaptionReader` 输入均为
  原始 `externalUrl`；Web 不使用远程 `proxyUrl`、`rangeUrl`、Lux Direct、Lux HLS 或服务端 ffmpeg 传输媒体字节。
- [ ] 浏览器原生能力不足时，远程 MP4/fMP4 HEVC 和 Matroska fallback 可在上游支持 CORS/Range 时使用现有 WASM/WebCodecs
  Worker；不支持时给出可诊断失败，且不自动回退到 Lux Relay。
- [ ] 远程字幕旁路使用浏览器到 `externalUrl` 的有限 CORS/Range 请求；旁路失败只清除字幕，不停止/重建会话，不影响原生音视频。
- [ ] 路径型 `.strm`、本地媒体、Emby 兼容层、播放会话/进度接口和媒体字节不相关的 Lux 控制请求不回归。

验证：相关 Web 单测、`pnpm --dir web test`、`pnpm --dir web build`、远程 CORS/Range 浏览器 smoke test、`git diff --check`；
记录 `uname -m`，本机 ARM64 结果不外推 NAS/x86 性能。

依赖：LUX-185、LUX-198、LUX-234。

#### LUX-244：任务类型与执行计划聚合

将管理员任务配置从“每个媒体库一条注册项”调整为“任务类型 → 执行计划 → 媒体库级运行任务”。
同一任务类型允许多个执行计划，每个计划拥有独立 Cron、启停状态、资源限制和媒体库范围；一个媒体库
在同一任务类型下只能属于一个执行计划。执行计划到点或立即执行时，仍按媒体库创建独立运行任务，
共享现有扫描锁和资源队列，不因聚合配置同时启动所有媒体库。

验收：

- [x] 新增 `scheduled_task_plans` 和 `scheduled_task_plan_libraries`，`scheduled_task_configs.plan_id` 保存旧配置镜像关系。
- [x] 旧数据库迁移后，原有不同 Cron、启停状态、插件来源和资源限制保持不变；相同有效配置按任务类型自动分组。
- [x] 新建媒体库加入匹配的默认执行计划；自定义计划可以原子移动多个媒体库，服务端拒绝同一任务类型的重复归属。
- [x] 计划 API 支持分页列表、创建、更新、媒体库范围更新、删除和立即执行；删除自定义计划时媒体库回到匹配默认计划；默认及全局插件计划不可删除；旧 `/admin/scheduled-tasks` API 保持兼容。
- [x] 调度按计划触发、按媒体库运行；实时增量扫描优先，全量扫描默认串行，活动任务不重复排队。
- [x] Web 任务页按任务类型展示多个执行计划，支持搜索、多选媒体库、独立 Cron、计划级立即执行和自定义计划删除。
- [x] SQLite 空库/已有库迁移、Rust API/调度测试、Web 测试、格式、Clippy 和构建通过；记录 `uname -m`。

验证：参见 `docs/LUX-244-PLAN.md`。

依赖：LUX-105、LUX-154、LUX-189。

#### LUX-246：跨数据库扫描写入与索引维护优化

针对扫描期间 PostgreSQL 的写入、WAL 和索引维护压力，在不改变 Lux 现有扫描并发配置的前提下，优化扫描中间数据的批量写入、删除和重复状态写入，
并清理已经确认不被查询或唯一约束需要的冗余索引。所有改动必须同时适用于 SQLite 和 PostgreSQL，继续使用统一的 storage 抽象，
不得引入 PostgreSQL 专属 SQL 或把整个媒体库放入单个长事务。

本任务明确不调整 `LUX_SCAN_CONCURRENCY`、媒体库 `scanConcurrency`、数据库连接池上限或其他并发档位；并发控制仍由现有配置和资源调度逻辑负责。

验收：

- [ ] SQLite/PostgreSQL 迁移均删除经过查询与约束核对的冗余索引；空库初始化和已有数据库升级均可完成，不能误删唯一约束或仍被查询使用的索引。
- [ ] `scan_job_targets`、`reconciliation_scan_entries` 等扫描中间数据的批量 DML 使用有界且跨数据库安全的批次，SQLite 不超过参数限制，PostgreSQL 不产生不必要的大事务；扫描语义、取消、重试和幂等行为保持不变。
- [ ] 扫描状态和中间数据在值未变化时不重复执行可避免的写入、删除或索引维护；失败恢复仍能保留需要重试的数据。
- [ ] 增加覆盖迁移、批量边界、SQLite 参数安全和 PostgreSQL 兼容性的测试，并以代表性扫描数据记录优化前后的写入/WAL 或查询执行证据；不以单机 ARM64 结果外推 NAS/x86_64 性能。
- [ ] 不修改扫描并发环境变量语义，不改变 Lux API、数据库公共模型或 SQLite/PostgreSQL 的数据一致性语义。

验证：

- `cargo test --locked --test storage --test scanner --test scanning_jobs`
- `cargo test --locked --test postgres_database`（需要可用 PostgreSQL 测试环境）
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `uname -m`，并记录本机 ARM64 结果不外推 NAS/x86_64 性能。

依赖：LUX-232、LUX-244。

明确不做：

- 不降低或重写 `LUX_SCAN_CONCURRENCY`、媒体库 `scanConcurrency` 或数据库连接并发配置。
- 不执行 `VACUUM FULL`、在线重建全库索引或其他长时间独占数据库的操作。
- 不连接或直接修改用户线上 FNOS 数据库；优化通过 Lux 迁移和 storage 实现交付。

实现文件：`migrations/0117_redundant_child_indexes.sql`、`migrations-postgres/0117_redundant_child_indexes.sql`、
`migrations/0118_scan_index_compaction.sql`、`migrations-postgres/0118_scan_index_compaction.sql`、
`src/storage/jobs.rs`、`src/storage/repository.rs`、`src/storage/repository_tests.rs`、`tests/storage.rs`、
`tests/postgres_database.rs`、`tests/admin_health.rs`、`tests/danmaku.rs`、`tests/ready_version.rs` 和
`tests/scanner.rs`；性能记录见 `docs/PERFORMANCE.md`。

验证记录（2026-09-08，`uname -m=arm64`）：`cargo build --locked`、
`cargo test --locked --all-targets`（库测试 429 passed、4 ignored，所有集成目标通过）、
`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 和
`git diff --check` 均通过。扫描发现批量 DML 的 SQLite 回归基准由 1,025 条路径对应 11 条降至 6 条，
约减少 45.5%；该结果只代表本机 ARM64/SQLite，不外推 PostgreSQL WAL 或 NAS/x86_64。
PostgreSQL 集成测试目标已编译，但其 4 个运行测试因本机没有可用 PostgreSQL 实例而保持 ignored，
因此真实 PostgreSQL 迁移/WAL 证据仍待可用测试环境复测。

#### LUX-247：Emby 兼容局域网发现

为 Lux Prism 的局域网服务器发现增加独立 UDP 服务。服务监听 UDP `7359`，仅处理包含
`who is EmbyServer?` 的 UTF-8 或 UTF-16LE 请求，并返回 Emby 兼容的 JSON：

```json
{
  "Address": "http://192.168.1.20:8097",
  "Id": "server-id",
  "Name": "Lux Server"
}
```

`Address` 默认根据请求来源选择本机网络接口和 Lux HTTP 端口；容器、反向代理或多网卡场景可以用
`LUX_DISCOVERY_ADVERTISE_URL` 显式指定对客户端可达的 HTTP(S) 基地址。该地址只允许 HTTP/HTTPS，
拒绝 userinfo、query 和 fragment。监听地址可用 `LUX_DISCOVERY_BIND_ADDR` 覆盖，默认
`0.0.0.0:7359`，主要用于测试和受限网络部署。

Prism 必须校验发现 JSON 中的地址，并同时探测返回的 `Address` 与 UDP 响应包来源地址加 Lux HTTP
端口；以 `Id` 去重，不得把 UDP 返回的地址直接当作已验证的连接地址。发现服务不接触认证令牌，
不记录完整 UDP 数据包或地址中的凭据。

验收：

- [x] Lux 启动后监听 UDP `7359`，有效的大小写不敏感 `who is EmbyServer?` 请求返回 `Address`、`Id`、`Name` 三个字段。
- [x] UTF-8 和 UTF-16LE 请求都能得到同编码的 JSON 响应；无关、空包和来源端口为 0 的数据包不响应。
- [x] `LUX_DISCOVERY_ADVERTISE_URL` 通过 HTTP(S) 地址校验，拒绝 userinfo、query、fragment 和无效地址；未配置时使用请求对应的本机接口地址。
- [x] UDP 服务随 HTTP 服务收到 Ctrl-C/SIGTERM 后退出，不遗留任务；发现错误不会泄露请求内容、令牌或完整外部 URL。
- [x] Compose 暴露 `7359/udp`，部署文档说明 Docker、反向代理和多网卡场景的显式广播地址配置；不改变现有 HTTP、Emby 或数据库合同。

验证：`cargo test --locked --lib discovery`、`cargo fmt --all -- --check`、
`cargo clippy --locked --all-targets --all-features -- -D warnings`，并在 Docker 网络中用固定夹具验证
UDP 请求、响应和来源地址候选。记录 `uname -m`；本机 ARM64 结果不外推 NAS/x86_64 性能。

依赖：LUX-246。

明确不做：

- 不在本任务实现 Prism 客户端、服务器 ID 去重逻辑或二维码设备配对；后者属于 LUX-248。
- 不新增认证、广播加密、通用 UDP 代理或额外 Emby 端点。

验证记录（2026-09-09，`uname -m=arm64`）：`cargo test --locked --test discovery`（3 passed）、
`cargo test --locked --lib discovery`（4 passed）、`cargo build --locked`、
`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 和
`docker compose config --quiet` 均通过。`cargo test --locked --all-targets` 的发现测试及其他已运行目标通过，
但在既有 `tests/libraries_api.rs:admin_can_list_and_update_library_schedules_from_operations_page` 中，
`AUTO_LIBRARY_COVER` 调度更新返回 503；该测试单独重跑仍复现，且本任务未修改其覆盖的代码。真实 Docker
网络中的 UDP 广播验证仍待在目标部署环境执行。本机 ARM64 结果不外推 NAS/x86_64 性能。

补充记录（2026-09-08）：0118 迁移将 `reconciliation_scan_entries` 的主键列顺序调整为
`(job_id, entry_type, library_root_id, relative_path)`，删除与新主键重复的宽索引；将
`scan_job_targets` 的三个阶段索引限制为 `PENDING/FAILED`，并删除未发现独立查询路径的
`media_streams.external_path` 索引。`item_images`、`person_credits` 的冗余索引已由 0117
处理；`media_items` 未发现可安全删除的明确冗余索引，因此保持不变。新增测试会先运行 1–117
迁移、写入代表性旧数据，再单独运行 118，确认扫描条目、扫描目标和外键约束均被保留。
该验证覆盖 SQLite 的真实升级路径；PostgreSQL 仍需在可用实例上运行被忽略的集成测试。

#### LUX-248：Lux Prism 一次性设备配对

为 Lux Prism 提供仅限 Lux 的一次性设备配对合同。Web 登录会话通过
`POST /api/v1/auth/device-pairings` 创建一个有效期 5 分钟的票据；创建接口必须同时
验证 `lux_session` 会话和 `x-csrf-token`，不接受共享管理员 API Key。服务端只保存随机
`secret` 的 SHA-256 哈希，不保存或记录完整二维码 URI。Web 使用当前页面 origin 拼接
二维码 URI：

```text
lux-prism://pair?v=1&server=<percent-encoded-origin>&id=<pairingId>&secret=<secret>&expiresAt=<unix-seconds>
```

`server` 必须是二维码生成页面的当前 HTTP(S) origin；Prism 扫码后应展示服务器名称和
地址，用户确认后向该地址调用兑换接口。Emby 不生成此二维码。

创建响应为：

```json
{
  "pairingId": "019...",
  "secret": "url-safe-random-secret",
  "expiresAt": 1770000000
}
```

Prism 通过 `POST /api/v1/device-pairings/{pairingId}/redeem` 兑换，提交：

```json
{
  "secret": "url-safe-random-secret",
  "deviceId": "stable-prism-device-id",
  "deviceName": "Qoo's Mac",
  "platform": "macOS",
  "version": "0.1.0"
}
```

服务端在一个数据库事务中校验票据、原子标记已消费并创建已有 Emby
`access_tokens` 记录；新记录的 `client_name` 固定为 `Lux Prism`，`device_type` 保存
`platform`。兑换成功响应为：

```json
{
  "accessToken": "returned-once",
  "userId": "user-id",
  "serverId": "server-id"
}
```

`accessToken` 只在成功兑换响应中返回，不能写入日志、SQLite/PostgreSQL 或二维码缓存。
兑换不需要 Web session，但必须提交有效 secret；票据不存在、secret 错误、已过期、已取消
或已消费分别返回稳定的 `DEVICE_PAIRING_NOT_FOUND`、`DEVICE_PAIRING_INVALID_SECRET`、
`DEVICE_PAIRING_EXPIRED`、`DEVICE_PAIRING_CANCELLED` 和 `DEVICE_PAIRING_CONSUMED` 错误码。
取消使用 `DELETE /api/v1/auth/device-pairings/{pairingId}`，同样要求当前创建者的 Web
session + CSRF，且不能取消其他用户的票据。

创建和兑换分别按用户/来源地址限流；超限返回 `429 TOO MANY REQUESTS`、错误码
`RATE_LIMITED` 和不超过 60 秒的 `Retry-After`。所有设备字段在 API 边界限制长度并拒绝空值，
请求体限制为 16 KiB。取消是显式资源操作：不存在、已取消或已消费的票据不再产生新的 token。

验收：

- [x] 从空 SQLite 和已有 SQLite 数据库升级到 0119；PostgreSQL 迁移保持相同表、字段和约束。
- [x] 未登录、缺少/错误 CSRF 或仅使用共享 API Key 不能创建/取消票据。
- [x] 创建返回 5 分钟有效的票据和一次性 secret，数据库只保存 secret 哈希。
- [x] 错误 secret、过期、取消、已消费和不存在票据分别返回上面定义的错误码；设备字段越界被拒绝。
- [x] 两个并发兑换请求至多一个成功，成功者获得可调用 Emby API 的 AccessToken，另一个得到已消费错误。
- [x] 兑换事务失败时票据和 AccessToken 一起回滚；取消权限按创建用户隔离。
- [x] 创建和兑换限流可验证，限流响应不包含 secret、token 或完整 URI。
- [x] 运行 `cargo test --locked --test device_pairings`、`cargo test --locked --lib security`、
  `cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`，
  并在可用 PostgreSQL 环境运行对应迁移/并发测试。

验证记录（2026-09-09，`uname -m=arm64`）：`cargo test --locked --test device_pairings`
（8 passed）、`cargo test --locked --lib security`（3 passed）、`cargo build --locked`、
`cargo test --locked --all-targets`（所有目标通过；库测试 437 passed、4 ignored）、
`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`
和 `git diff --check` 均通过。测试期间发现观测子进程并行启动会争用 LUX-247 的默认 UDP
`7359`，已让观测测试使用 `127.0.0.1:0` 的临时发现端口；生产默认监听地址未改变。
PostgreSQL 集成测试目标已编译，但本机没有可用 PostgreSQL 实例，4 个测试保持 ignored，
因此真实 PostgreSQL 迁移/并发证据仍待可用环境复测。本机 ARM64 结果不外推 Windows、NAS
或 x86_64 性能。

依赖：LUX-247。

明确不做：

- 不支持 Emby 的二维码配对，不在 Prism 或服务器保存相机画面。
- 不改变现有 Web session 或普通 Emby 登录合同，不引入离线写队列。
- 不在本任务实现 Prism 客户端、二维码渲染组件、摄像头权限或系统凭据库存储。

#### LUX-249：TMDb 原语言文字与图片模式

范围：为外置 `org.lux.tmdb` 增加默认关闭的 `originalLanguageEnabled` 配置。启用后，电影和剧集标题使用 TMDb 原标题，其他文字字段优先使用 `original_language` 对应的翻译；季/集继承父剧原语言。电影、剧集详情响应中已经包含的图片按原语言、无语言、英语优先；独立图片请求仍只发一次上游请求，在需要时请求全语言并本地筛选。宿主通过可选的 `originalLanguage` 图片请求提示传递已持久化的原语言，不改变现有插件的默认行为，也不增加数据库迁移。

验收：

- [ ] TMDb manifest 和 Web 管理页暴露默认关闭的“原语言”开关，旧配置读取后保持关闭且可保存/恢复。
- [ ] 启用后电影、剧集的标题、简介等文字字段按原语言优先，缺失时回退首选语言；中文标题别名替换不覆盖原语言标题。
- [ ] 启用后详情图片和独立图片候选按原语言、无语言、英语排序；季/集文字和图片使用父剧原语言。
- [ ] 电影/剧集详情复用已有 `translations` 和 `images` 载荷；独立图片查询最多一次请求，季/集冷缓存最多补一次父剧详情请求。
- [ ] 未启用时现有语言、图片筛选、请求字段和插件 RPC 行为保持不变；不新增数据库迁移。

验证：

- 外置 `Lux-plugins`：`cargo test --locked --lib`、`cargo test --locked --bin lux-plugin-tmdb`
- Lux 主仓库：`cargo test --locked --test scraper`、`cargo test --locked --test image_api`、`cargo test --locked --test plugins`
- Web：`pnpm --dir web test`、`pnpm --dir web build`
- `cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`

依赖：LUX-144、LUX-195。

明确不做：

- 不将“原语言”伪装成新的 TMDb canonical locale，也不为搜索候选逐项追加详情请求。
- 不改变 TMDb Provider ID、metadata RPC 方法名称或数据库 schema。

#### LUX-250：季海报缺失时回退父剧海报

范围：当季条目没有自己的 `POSTER` 图片时，Lux Web 在已知父剧集上下文的页面中展示父剧集海报；
季条目自身的海报始终优先。回退仅属于展示层，不复制或登记图片，不改变图片编辑、元数据写回和
图片来源记录。后续元数据补全得到季海报后，页面刷新即可切换到真实季海报。

验收：

- [x] 剧集详情的季卡片没有季海报时显示父剧集海报。
- [x] 季详情没有季海报时显示父剧集海报；季详情已有自己的海报时继续显示季海报。
- [x] 回退不调用季图片端点、不创建或修改图片记录，且不影响电影、剧集和单集图片选择。
- [x] 前端单测覆盖季海报缺失、真实季海报优先和父剧上下文缺失三种情况。

验证：

- `pnpm --dir web test`
- `pnpm --dir web build`

依赖：LUX-060、LUX-061、LUX-100。

明确不做：

- 不把父剧海报复制到季目录或写入 `/config/metadata/library`。
- 不修改 TMDb 插件、数据库 schema、Emby 图片 DTO 或图片来源优先级。

#### LUX-252：单季剧集详情直接展示单集列表

范围：当剧集详情实际返回的季度数量为一时，剧集详情页直接展示该季度的单集列表，避免用户
先进入只有一个季度的季度详情页；多季剧集继续展示季度卡片，季度详情页和单集详情页保持现有行为。
该变化仅属于 Web 展示层，不修改 API、数据库或剧集/季度/单集领域关系。

验收：

- [x] 单季剧集详情不再显示季度卡片，而是在剧集详情下显示该季度的单集数量和单集列表。
- [x] 单季剧集的单集仍可进入原有单集详情页，播放入口和下一集选择逻辑不变。
- [x] 多季剧集继续显示季度卡片，不提前展开任一季度的单集列表。
- [x] 季度详情页仍显示原有单集列表，空季度仍显示原有空状态。

验证：

- `pnpm --dir web test -- media-detail.test.tsx`
- `pnpm --dir web build`
- `git diff --check`

验证记录（2026-09-15）：`pnpm --dir web test`（105 个 Node 测试、479 个 Vitest 测试通过）、
`pnpm --dir web build`、`git diff --check` 通过；浏览器运行时检查因本机 CUA 浏览器提供方无法加载
request-header policy 未执行。未修改 Rust 源码、API 或数据库。

依赖：LUX-060、LUX-061、LUX-100。

明确不做：

- 不修改 `/api/v1/items/{id}/children` 的请求或响应合同。
- 不将单集列表改为自动播放，也不改变多季剧集的浏览层级。

#### LUX-253：单集图片播放与文字详情入口

范围：在季详情页的单集列表以及单集详情页的同季分集卡片中，将图片区域与文字区域拆分为两个入口。
鼠标悬停在图片区域时显示居中的播放按钮；点击图片区域任意位置进入现有 `/watch/{itemId}` 播放页，
点击单集标题、简介或“查看详情”文字进入现有 `/items/{itemId}` 详情页。该变化仅属于 Web 展示层，
不修改播放会话、媒体源选择、API、数据库或单集层级关系。

验收：

- [x] 季详情单集列表的图片区域提供可访问的播放链接，并在桌面悬停时显示居中的播放图标。
- [x] 季详情单集列表的标题、简介和详情文字继续进入单集详情页。
- [x] 单集详情页的同季分集卡片保持相同的图片播放、文字详情语义。
- [x] 前端测试覆盖两类入口的目标路径和播放按钮的可访问名称。

验证：

- `pnpm --dir web test`
- `pnpm --dir web build`

验证记录（2026-09-15）：`pnpm --dir web test`（105 个 Node 测试、479 个 Vitest 测试通过）、
`pnpm --dir web build`、`git diff --check` 通过；浏览器运行时截图验证因本机 CUA 浏览器提供方无法加载
request-header policy 未执行。未修改 Rust 源码、API 或数据库。

依赖：LUX-100、LUX-231。

明确不做：

- 不改变 `/watch/{itemId}` 播放页及其播放会话初始化逻辑。
- 不将电影、剧集、季度或其他媒体卡片的图片点击行为一并改为播放。

#### LUX-254：Emby 客户端服务端转码

范围：把现有本地媒体服务端 HLS 能力接入 Emby 兼容 `PlaybackInfo` 协商，使第三方 Emby 客户端在
无法直放时可以使用标准 `TranscodingUrl` 播放。Emby 路由只负责解析协议请求、执行 ACL 和映射 DTO；
FFmpeg、临时目录、并发限制、签名资源和生命周期继续由现有播放 application service 负责。

兼容合同：

- `GET /Items/{itemId}/PlaybackInfo` 和空 body 的 `POST` 保持现有 Direct Play 行为；带有明确
  `EnableDirectPlay`、`EnableDirectStream`、`EnableTranscoding`、`AllowVideoStreamCopy` 和
  `AllowAudioStreamCopy` 的 `POST` 按客户端能力从 Direct、HLS Remux、音频转码、硬件转码和软件转码中
  选择最低成本可用档位；`EnableTranscoding=true` 且 `EnableDirectPlay` 未设置或为 `false` 时进入
  转码。客户端同时声明直放和转码时，按 Emby `DeviceProfile.DirectPlayProfiles` 匹配本地媒体源的容器和
  音视频 codec；只有源信息已知且直放 profile 确认不匹配、同时存在 HLS `TranscodingProfiles` 时进入转码。
  缺失或待探测的 source 元数据作为未知处理，不因未知值自动从 Direct 降级。没有顶层布尔值时也按此规则
  协商；HLS profile 限定 codec 时，只复制兼容的流，否则进入相应的音频或视频转码档位。
  `forceTranscode=true` 查询参数可覆盖 `EnableDirectPlay=true`。GET 和空 body 的 POST 保持
  Direct Play 行为。
- 本地媒体源在选择服务端转码时返回 `SupportsTranscoding=true`、`TranscodingUrl` 和
  `TranscodingSubProtocol=hls`。没有明确容器声明时使用 MPEG-TS，并返回 `TranscodingContainer=ts`、
  `TranscodingMimeType=video/mp2t`；客户端明确声明 `mp4`/`fmp4` 时使用 fMP4，并返回对应的
  `mp4`/`video/mp4`。URL 指向标准 Emby `master.m3u8` 入口，并带有 `DeviceId`、输出 codec、码率、轨道索引
  和 `SegmentContainer` 参数；实际转码 offer 的 `DirectStreamUrl` 与 `TranscodingUrl` 指向同一个签名 HLS
  清单，同时保持 `SupportsDirectPlay`/`SupportsDirectStream` 为 `false`。TS 清单不带 init；fMP4 清单中的
  每个逻辑 `segment_N.m4s` 使用对应的 `init_N.mp4` 和当前会话短期签名 URL，不能跨 generation 混用 fMP4
  初始化段，签名资源还必须绑定会话容器。
- 转码会话复用 `web_playback_sessions`，其 `PlaySessionId` 可被 Emby `Sessions/Playing`、`Progress` 和
  `Stopped` 回调关联；播放/暂停刷新 TTL，停止立即回收 FFmpeg 进程和临时目录。没有回调时仍由服务端
  过期清理回收。
- Emby `PlaybackInfo` 只登记惰性 HLS 会话，不占用 FFmpeg 并发名额，也不停止同一用户、条目和 source 的
  现有播放；首个逻辑 init、媒体分片或未知时长的物理清单请求才原子替换旧会话并启动。逻辑分片一旦绑定 generation，
  后续 seek 不得把它改映射到另一 generation。`StartTimeTicks` 只是 init 先到时的起点提示；总时长已知时，非正数以及等于或超过总时长的值都不用于启动。
- `.strm` 无论客户端是否声明转码能力，都不返回服务端转码 URL，不启动 FFmpeg，不生成 HLS 目录，也不
  代理媒体字节。
- 转码资源必须绑定当前用户、条目、媒体源、会话和签名有效期；错误用户、跨条目/媒体源、篡改或过期签名、
  路径穿越和无权限请求均拒绝。Emby token 不写入转码 URL 或日志。

验收：

- [x] 第三方 Emby `PlaybackInfo` POST 可以为本地媒体协商服务端转码，并实际取得 `master.m3u8` 和 media
      segment；fMP4 profile 额外取得 init segment，Direct Play 仍优先。
- [x] Emby 转码播放事件能够刷新会话并在 `Stopped` 后回收资源；无事件会被 TTL/孤儿清理回收。
- [x] `.strm`、无权限 source、错误用户、跨 source、过期/篡改签名和路径穿越均不会启动或泄露转码资源。
- [x] 现有 Web 播放、Emby 直放、ACL、Range、进度和媒体代理行为不回退。
- [ ] Rust 窄测试、全量质量门和本机架构记录通过；真实第三方客户端的首帧、seek、暂停、停止和断线行为
      由部署后专项兼容性测试记录，不以服务端测试替代。

验证：见 `docs/LUX-254-PLAN.md`；本机 `uname -m` 结果不外推 NAS/x86_64 性能或所有客户端兼容性。

验证记录（2026-09-16）：`cargo test --locked --test playback`（3 个通过）、
`cargo test --locked --lib playback`（52 个通过）和 `cargo fmt --all -- --check` 通过；转码集成测试使用
fake FFmpeg 实际读取 master manifest、init 和 m4s 片段，并验证回调刷新、停止清理、ACL、签名和 `.strm`
边界，以及 `forceTranscode` POST 查询、GET 直放、省略 `EnableDirectPlay`、标准 Enable 标志组合、
`DeviceProfile` 和不兼容 codec 不复制的兼容行为。`cargo test --locked --all-targets` 首次运行在并发运行时因既有
`tests/libraries_api.rs` 的 SQLite 服务不可用偶发失败，单独运行该目标通过；随后完整重跑通过。
播放目标的 `cargo clippy --locked --test playback --all-features -- -D warnings` 通过；全量
`cargo clippy --locked --all-targets --all-features -- -D warnings` 仍被既有
`tests/item_merge.rs:32` 的 `clippy::too_many_arguments` 阻塞。`uname -m` 为 `arm64`。真实 FFmpeg 和
VidHub、SenPlayer、Infuse 等第三方客户端的首帧、seek、暂停、停止及断线回收尚未在部署实例验证。

验证记录（2026-09-16 回归修复）：针对本地 source 元数据缺失/待探测时被 DeviceProfile 自动降级到服务端转码的情况，
新增“未知不等于不兼容”协商规则和回归覆盖；`cargo build --locked`、`cargo test --locked --all-targets`
（456 个库测试通过、4 个需 PostgreSQL 的库测试忽略，所有启用的集成目标通过）、
`cargo test --locked --test playback`（3 个通过）、`cargo test --locked --lib playback`（46 个通过）、
播放目标 Clippy、`cargo fmt --all -- --check` 和 `git diff --check` 通过。全量 Clippy 仍被既有
`tests/item_merge.rs:32` 的 `clippy::too_many_arguments` 阻塞；`uname -m` 为 `arm64`。FNOS 重部署及真实第三方
客户端播放尚未验证。

验证记录（2026-09-16 增量扫描探测修复）：新增扫描回归测试，确认仅探测本次变更的 `LOCAL_FILE` source，
未变化本地 source 与 `.strm` 均不由普通 ffprobe 处理，完成后不遗留扫描目标。`cargo build --locked`、
`cargo test --locked --all-targets`（457 个库测试通过、4 个忽略，所有启用的集成目标通过）、
`cargo test --locked --test scanning_jobs --test probe`（38+15 个通过）、目标 Clippy、格式检查和
`git diff --check` 通过。全量 Clippy 仍被未修改的 `tests/item_merge.rs:32` 参数过多 lint 阻塞；`uname -m` 为
`arm64`。FNOS 未部署本修复，现有线上 PENDING source 仍需在部署后重新触发针对性扫描验证。

验证记录（2026-09-16 Emby 转码 offer 修复）：新增只提交 `DeviceProfile` 的 POST 回归，确认省略播放开关按启用
处理、HLS profile 会公开 `SupportsTranscoding=true`，并确认实际返回 `TranscodingUrl` 时关闭 Direct Play/Direct Stream
能力。`cargo build --locked`、`cargo test --locked --all-targets`（459 个通过、4 个需本地 PostgreSQL 的测试忽略，
所有启用的集成目标通过）、`cargo test --locked --test playback`（3 个通过）、`cargo test --locked --lib emby_playback_tests`
（13 个通过）、播放目标 Clippy、格式检查和 `git diff --check` 通过。全量 Clippy 仍被未修改的
`tests/item_merge.rs:32` 参数过多 lint 阻塞；本机 `uname -m` 为 `arm64`。FNOS 新镜像及真实第三方客户端播放尚待部署后验证。

验证记录（2026-09-16 Emby 码率协商修复）：根据 FNOS 最新请求仍未进入转码、而本地源已探测为约 13.9 Mbps 的证据，
新增 `MaxStreamingBitrate` 顶层/`DeviceProfile` 解析及码率超限回归；码率超过客户端限制时不再复制视频流，改选硬件或软件
视频转码，并输出诊断字段 `source_bitrate`、`max_streaming_bitrate` 和 `source_bitrate_exceeds_limit`。`cargo test --locked --lib
emby_playback_tests`（14 个通过）、格式检查和 `git diff --check` 通过；FNOS 新镜像及真实第三方客户端仍需部署后验证。

验证记录（2026-09-16 Harbor 转码入口兼容修复）：FNOS 日志确认 Harbor 已拿到转码 offer，Lux 也已启动本地
H.264/AAC fMP4 HLS，但 Harbor 1.4.6 未请求 `TranscodingUrl`，而是在 `DirectStreamUrl` 为 `null` 时回退到直放路径，
因此没有产生 HLS manifest 请求。对照 Emby 的实际转码响应后，转码 offer 现让 `DirectStreamUrl` 与 `TranscodingUrl`
共同指向同一个签名 `master.m3u8`，并继续将两个直放能力位设为 `false`。`cargo build --locked`、
`cargo test --locked --test playback`（3 个通过）、`cargo test --locked --lib playback`（54 个通过）、格式检查和
`git diff --check` 通过；`cargo test --locked --all-targets` 的 462 个库测试通过，但其中两个内嵌字幕测试在该次
全量运行中超时；随后单独运行 `cargo test --locked --lib embedded_subtitle`（3 个通过）确认模块本身通过。全量
Clippy 仍被未修改的 `src/api/legacy.rs:44` 未使用导入阻塞。FNOS 新镜像及 Harbor 真机首帧验证尚待部署。

验证记录（2026-09-16 Harbor 进度时长兼容修复）：为避免 Harbor 将实时增长的 HLS 清单长度当作媒体总时长，
`PlaybackInfo` 现在在顶层和每个媒体源返回 `RunTimeTicks`，并在 source 时长缺失时回退到媒体项时长；详情 DTO 的
`RunTimeTicks` 与媒体源时长也使用同一回退规则。新增转码响应回归覆盖；`cargo test --locked --test playback`
（3 个通过）通过，其他全量质量门和 FNOS/Harbor 真机复测待完成。

验证记录（2026-09-19 Harbor 中段转码启动竞态修复）：FNOS 与 Harbor 日志确认，旧实现会在
`PlaybackInfo`/`master.m3u8` 阶段提前从第 0 段启动 FFmpeg，随后 Harbor 的首个中段分片请求立即取消该 generation，
使首个资源请求落入进程切换窗口。Emby HLS 现改为惰性启动：已知总时长的完整 VOD 清单不启动 FFmpeg，首个 init
或媒体分片请求才取得名额并启动，首个媒体分片直接决定 generation 0 的起点；不同 generation 使用独立 init，
并发 init waiter 会跟随当前 generation。新增回归覆盖中段首次启动、回到更早分片、init/seek 并发、新 offer 不提前
停止旧会话，以及越界 `StartTimeTicks`。`cargo build --locked`、`cargo test --locked --all-targets`
（513 个库测试通过、4 个忽略，所有启用的集成目标通过）、库与 playback 集成目标 Clippy、
`cargo fmt --all -- --check` 和 `git diff --check` 通过。全量 Clippy 仍只被未修改的
`tests/item_merge.rs:32` 参数过多 lint 阻塞；`uname -m` 为 `arm64`。FNOS 新镜像与 Harbor 真机的中段切换、回到开头、
首帧、声音和进度推进仍需部署后验证。

验证记录（2026-09-20 HLS init / generation 绑定修复）：FNOS Jellyfin FFmpeg 7.1.4 交叉探针确认，
仅添加 `-start_at_zero`、`use_editlist=0` 或输出偏移仍会把中段 fMP4 fragment 的 PTS 归零；因此 Emby VOD
清单改为每个逻辑 segment 声明对应的 `init_N.mp4`，不同 generation 的物理 segment 文件隔离命名，并将
逻辑 segment 持久绑定到首次生成它的 generation。窄 HLS 单元测试 24 个和 Emby PlaybackInfo/HLS 集成目标
已通过；FNOS 镜像重建、部署及 Harbor 真机首帧/seek/音画验证仍待完成。

验证记录（2026-09-20 Emby HLS 容器协商修复）：Emby 转码会话现在默认协商 MPEG-TS，只有请求或
`DeviceProfile.TranscodingProfiles` 明确声明 `mp4`/`fmp4` 时才选择 fMP4；`TranscodingContainer`、MIME、
FFmpeg segment type、manifest init 结构、分片扩展名和 Content-Type 保持一致。Emby HLS 资源签名绑定会话容器，
TS 也覆盖中段首次启动、回到第 0 段和容器篡改拒绝。`cargo test --locked --lib playback::hls`（27 个通过）、
`cargo test --locked --lib emby_playback_tests`（23 个通过）和 `cargo test --locked --test playback
emby_playback_info_negotiates_server_transcoding_and_cleans_hls`（1 个通过）已通过；完整质量门、FNOS
重部署以及 Harbor 真机首帧/seek/音画验证仍待完成。

依赖：LUX-198、LUX-199。

明确不做：

- 不实现字幕转换/烧录、DRM、多码率自适应 HLS、`.strm` 服务端转码或第三方客户端专属私有协议。
- 不改变现有 Web 播放 DTO、Emby 内部领域模型或数据库字段；没有数据库迁移需求。

#### LUX-255：Lux 用户级客户端令牌与第三方首页 API

范围：让 Lux 自有 `/api/v1` 的媒体、搜索、首页、图片、播放和用户状态接口接受用户级客户端令牌，
并使第三方客户端可以直接调用已有的 `GET /api/v1/home`。不新增令牌数据库表；复用现有 Emby
AccessToken 的生成、哈希存储、撤销和用户解析。

验收：

- [x] 新增推荐请求头 `X-Lux-Token: <accessToken>`，兼容 `X-Emby-Token`、`X-MediaBrowser-Token` 和
      `Authorization: Bearer <accessToken>`。
- [x] 无 Web Cookie 的有效用户令牌可调用 `/api/v1/home` 及 Lux 媒体查询；响应继续按当前用户执行
      媒体库 ACL。
- [x] 普通用户令牌不能调用管理员接口；LUX-182 共享管理员 API Key 的权限和 CSRF 豁免边界保持不变。
- [x] 更新 Lux API、首页合同和兼容性文档；不宣称 VidHub、SenPlayer、Infuse 等真实客户端已完成验证。

验证目标：`cargo test --locked --test lux_api_auth`、`cargo test --locked --all-targets`、`rustfmt --check`
和 `git diff --check`。本轮全量 Rust 测试为 443 passed、4 ignored、0 failed，`rustfmt --check` 与
`git diff --check` 也通过；该证据只覆盖 Lux 服务端协议，不代表第三方客户端已经完成真实客户端验证。

明确不做：

- 不新增普通用户 API Key 管理页面或细粒度 token scope。
- 不改变 Emby 路由/DTO，不把用户令牌写入 URL、日志、审计事件或普通响应。

#### LUX-256：媒体库缩略图刮削模式与截图优先级

范围：在全局媒体库策略和单个媒体库覆盖策略中新增 `images.thumbnailScrapingMode`，使用
`NONE`（不刮削）、`SCREENSHOT_FIRST`（截图优先）和 `SCRAPER_FIRST`（刮削器优先）三个值，默认
`SCRAPER_FIRST` 以保持已有媒体库行为。该策略同时约束 `POSTER` 与 `THUMB` 两类自动缩略图；
策略继续复用现有媒体策略 JSON，刮削器优先的重试时刻由持久化队列表记录。

验收：

- [ ] 管理页使用分段控制器展示三种模式；全局策略和自定义媒体库策略都可以读取、编辑并保存该值。
- [ ] 媒体库策略变更后，管理员重新执行“扫描媒体库文件”会重新评估该库已入库的本地视频；缺失的
      `POSTER`/`THUMB` 截图会按当前策略生成，不要求媒体文件指纹发生变化。
- [ ] `NONE` 不发起 `POSTER`/`THUMB` 在线刮削，也不生成本地视频或 STRM 视频截图；不删除已有登记图片，
      手工本地图片仍保持最高优先级。
- [ ] `SCREENSHOT_FIRST` 在资源入库后生成 `FFMPEG`/`STRM_FFMPEG` 截图；每种图片类型已有有效截图后，
      元数据刮削不得再请求或覆盖该类型的在线图片。截图生成失败或该类型仍无截图时，在线图片可以补位；
      `POSTER` 与 `THUMB` 独立判断。
- [ ] `SCRAPER_FIRST` 配置在线刮削器时先尝试在线图片，不立即生成截图；在线图片依次在首次处理、首次后
      6 小时、首次后 24 小时尝试，共三次。三次后仍缺少图片时才生成截图，并且只为缺失的类型生成；已有
      `POSTER` 和 `THUMB` 时不再截图。重试时刻持久化，进程重启和媒体库重扫不重置首次尝试时间。
- [ ] `SCRAPER_FIRST` 没有配置在线刮削器时，资源入库后仍可立即生成截图；三次在线尝试后截图作为回退图，
      后续在线刮削成功时仍可替换该回退图。
- [ ] STRM 信息提取插件的 `thumbnailEnabled` 仍受插件配置控制，但媒体库为 `NONE` 时宿主不得为该库
      请求或登记缩略图；媒体信息提取不受该缩略图策略影响。
- [ ] API 对未知模式返回校验错误；旧策略 JSON 缺少该字段时按 `SCRAPER_FIRST` 兼容解析。

验证：

- `cargo test --locked --test thumbnails --test strm_probe --test libraries_api`
- `cargo test --locked --lib storage::repository::repository_tests::thumbnail_scraper_retries_are_persisted_and_claimed_at_due_times`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `pnpm --dir web test`
- `pnpm --dir web build`

依赖：LUX-145、LUX-146、LUX-144。

明确不做：

- 不删除、迁移或重生成已有图片资产，不改变现有图片 API、Emby DTO 或插件 RPC 方法名称。
- 不把缩略图策略扩展为转码、代理或其他媒体库扫描策略；本任务只改变 `POSTER`/`THUMB` 自动来源选择。

#### LUX-257：管理员媒体库顺序策略

范围：在服务器设置中增加“媒体库顺序强制按照管理员排序”，在所有用户的个人设置“首页排版 → 媒体库顺序”中增加“按照管理员顺序排序”。普通用户默认开启个人选项；个人选项关闭时继续使用自己的服务端媒体库顺序。管理员开启强制项后，普通用户的个人选项显示为开启且不可取消，并且 Web 首页、媒体库入口和 Emby 兼容视图统一使用管理员账号保存的媒体库顺序。关闭强制项后恢复普通用户此前保存的个人选项。

验收：

- [ ] `GET/PATCH /api/v1/admin/settings` 读写强制排序设置，默认关闭；设置只允许管理员修改。
- [ ] `GET/PATCH /api/v1/auth/settings` 读写普通用户的“按照管理员顺序排序”偏好，普通用户默认开启；强制开启时返回有效开启状态和不可取消状态。
- [ ] 普通用户在有效开启个人选项或服务器强制项时，Lux Web 媒体库列表、首页和 Emby `OrderedViews` 使用管理员账号的媒体库顺序；关闭个人选项后使用个人顺序。
- [ ] 强制项开启时，服务端拒绝通过直接 API 请求绕过不可取消的个人设置；关闭强制项后不覆盖用户此前保存的个人选择。
- [ ] 新增 SQLite 与 PostgreSQL 迁移，并覆盖默认值、个人切换、强制切换和排序消费路径。

验证：

- `cargo test --locked --test library_order --test user_settings`
- `cargo fmt --all -- --check`
- `pnpm --dir web test -- --run web/tests/account-settings.test.tsx web/tests/admin-settings.test.tsx`
- `pnpm --dir web build`

明确不做：

- 不改变媒体库内部影片/剧集的浏览器本地排序；不改变媒体库 ACL、Emby DTO 结构或其他服务器设置。

#### LUX-258：登录页背景来源选择

范围：在服务器设置中增加登录页背景来源选择，支持现有固定海报墙和媒体库最新添加海报。选择最新添加时，
登录页通过公开的带图片标签的 Emby 图片地址展示最近加入媒体库的真实海报；没有可用海报、媒体库为空或接口
失败时回退固定海报墙。该设置默认保持固定海报墙，不引入 TMDb 榜单或新的插件能力。

验收：

- [ ] `GET/PATCH /api/v1/admin/settings` 读写 `loginBackgroundSource`，只允许 `STATIC` 和 `RECENTLY_ADDED`，默认 `STATIC`。
- [ ] `GET /api/v1/auth/login-background` 无需登录即可返回当前来源和有限数量的海报地址；响应不返回媒体标题、路径、库名或其他媒体元数据。
- [ ] `RECENTLY_ADDED` 只选择启用媒体库中按新增时间倒序的条目，并且只返回有登记海报标签的条目。
- [ ] 登录页加载服务器背景并渲染真实海报；接口失败或无海报时继续显示固定海报墙。
- [ ] 管理设置明确提示“最新添加”会让选中海报在未登录页面公开展示。

验证：

- `cargo test --locked --test login_background`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `pnpm --dir web test -- --run web/tests/login-page.test.tsx web/tests/admin-settings.test.tsx web/tests/api-client.test.ts`
- `pnpm --dir web build`

依赖：LUX-110、LUX-255。

明确不做：

- 不实现 TMDb 热门、TMDb 高分或 Top 250；这些需要后续外置插件榜单发现能力和后台缓存任务。
- 不改变现有用户媒体库 ACL；该设置明确开启后，登录页展示的海报属于管理员主动选择的公开展示资源。

注：本条只限定 LUX-258 当时的实现范围。后续登录背景插件能力由 LUX-259 至 LUX-263 单独定义和验收，不回溯改变 LUX-258 的验收结果。

#### LUX-259：登录页背景插件类型与数据 RPC 合同

范围：为 Plugin SDK 增加专用 `login_background` 插件类型和 `login_background.get` 能力，定义 provider-neutral、严格有界的数据响应。插件只能提供图片 URL 和必要的来源署名元数据；布局、CSS、HTML、脚本和登录页文案仍由 Lux 主程序决定。必应与 TMDb 插件必须是各自独立的插件包和配置，不扩展或复用 `org.lux.tmdb` 元数据插件的运行时配置。

验收：

- [ ] manifest 校验只允许合法的 `login_background` 类型/能力组合；普通元数据刮削器不能因此被当作登录背景提供者。
- [ ] `login_background.get` 响应只包含 `contentKind`（`POSTER_FEED`、`HERO_IMAGE`、`SINGLE_POSTER` 或 `SINGLE_IMAGE`）、有界 `items`，以及纯文本的来源/版权署名字段和可选署名/许可链接；不接受 HTML、CSS、脚本或任意组件定义。
- [ ] `POSTER_FEED` 最多 40 项；`HERO_IMAGE`、`SINGLE_POSTER` 和 `SINGLE_IMAGE` 恰好 1 项；每项图片必须是 HTTPS，限制长度并校验域名属于该插件声明的图片主机；署名/许可链接仅允许 HTTPS 且精确命中 manifest `network` 主机；拒绝凭据、localhost、私网/链路本地 IP 和非 HTTP(S) 地址。
- [ ] 合同校验拒绝畸形、超量、未知内容类型、未知字段和不安全图片 URL，并以不回显插件原始数据的错误类型报告拒绝原因。
- [ ] `docs/PLUGIN-SDK.md` 给出 manifest、RPC 请求/响应、图片 URL 及署名/许可链接安全规则的版本化示例。

验证：

- `cargo test --locked --test plugins`
- `cargo test --locked --test plugin_protocol`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --test plugins --all-features -- -D warnings`
- 外部插件 SDK fixture/manifest/RPC 合同测试。

依赖：LUX-142、LUX-258。

明确不做：

- 不在本任务新增管理员界面、公开背景 API、刷新调度、持久化缓存或任一供应商插件。
- 插件不允许注入登录页代码、改变 Lux 的视觉布局或直接读取 Lux 数据库/媒体库。

#### LUX-260：登录背景插件缓存与宿主接口

范围：由 Lux 主程序管理动态插件来源的选择、后台刷新和有限缓存。扩展现有 `loginBackgroundSource` 为 `STATIC`、`RECENTLY_ADDED` 或 `PLUGIN:<plugin-id>`；插件调用只由受限后台 worker 执行，未登录的 `GET /api/v1/auth/login-background` 只读已校验缓存，绝不在请求路径启动插件或访问第三方网络。不可用、未启用、无有效缓存或刷新失败时回退固定海报墙，并保留管理员所选来源以便恢复后生效。

验收：

- [ ] 已安装、已启用、可用且声明 `login_background` 的插件才可被选作来源；设置写入仍需管理员鉴权和 CSRF。
- [ ] 插件在首次启用/切换后由后台异步刷新，并至少每日刷新一次；失败采用有界退避，接口在刷新期间读取旧缓存，缓存超过 48 小时则回退 `STATIC`。
- [ ] SQLite 与 PostgreSQL 持久化有界、已校验的插件数据缓存；只缓存数据合同，不下载、重编码或复制图片二进制。
- [ ] 公开接口按宿主固定 DTO 返回来源、经校验的 `contentKind`、最多 40 个 HTTPS 图片 URL 和必要署名；`SINGLE_POSTER`/`SINGLE_IMAGE` 恰有一个 URL；署名/许可 URL 经过 manifest 主机 allowlist 校验；不透传插件原始 JSON、凭据、媒体标题、文件路径或库信息。
- [ ] 插件卸载/禁用、进程崩溃、超时、DNS/上游故障和空结果均回退静态墙；管理员端能看出所选插件不可用，而不是静默改写所选配置。
- [ ] 插件超时、进程崩溃、畸形/超量响应和未知内容类型均被隔离为可诊断的 provider 错误，不影响 Lux 主进程、其他插件或现有静态背景。
- [ ] 用户代理直接请求已声明的图片 CDN；系统不提供任意 URL 代理，不因登录请求造成 SSRF，也不把第三方 URL 作为认证或授权依据。
- [ ] 数据 TTL 与第三方许可一致；TMDb 数据保留远低于其六个月缓存上限。

验证：

- `cargo test --locked --test login_background --test plugins`
- 新增 SQLite/PostgreSQL cache migration 与空库迁移测试。
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`

依赖：LUX-259、LUX-258。

明确不做：

- 不在未登录 API 请求路径调用插件、TMDb 或必应，不创建无限增长的图片缓存。
- 不生成本地压缩版海报、不逐项转码媒体库新资源、不做通用第三方图片代理。

#### LUX-261：登录背景管理与两种宿主布局

范围：在服务器设置中列出当前已安装且可用的登录背景插件来源；登录页只按 Lux 固定模板渲染插件数据。海报流继续复用现有倾斜瀑布流；宽幅图片使用宿主内置的单幅大图布局。增加可访问的“关于/鸣谢”入口承载 TMDb 品牌和法定来源说明；TMDb 说明不放回登录按钮下方。必应当日图片的摄影者/版权信息只在背景区域以低干扰形式展示。

验收：

- [ ] 管理员可选固定海报墙、媒体库最新添加、Bing 每日图片或 TMDb 日榜横幅图；未安装/不可用插件不会被错误展示为可用选项，并清楚提示启用和公开访问的风险。
- [ ] `POSTER_FEED` 使用既有五列紧凑倾斜瀑布流和当前位置/留白，不改变尺寸、列距、倾斜角、交错规则；`HERO_IMAGE` 用 Lux 自带 CSS 呈现，不加载插件自定义 UI。
- [ ] `SINGLE_POSTER` 只展示一张完整原比例海报，不裁切、旋转、拼贴或叠加遮罩；由宿主固定布局在左侧视觉区呈现，不改变既有海报瀑布流。
- [ ] `SINGLE_IMAGE` 只展示一张完整原比例图片，不裁切、旋转、拼贴、压暗或叠加遮罩；作品署名与许可作为独立、可访问的外链呈现。
- [ ] 登录页 API 错误、空海报、图片 404、插件失效时保持完整可登录状态并显示静态背景。
- [ ] 登录卡片按钮下不新增“支持 Lux/Emby”或供应商免责声明；TMDb 规定的 logo 与文字放在可访问的“关于/鸣谢”区域，且 TMDb 标识不比 Lux 品牌更显著。
- [ ] Commons 每日图片逐项筛选许可；仅允许公共领域、CC0、CC BY 或 CC BY-SA 文件，并展示作者、作品页及许可证外链；遇到 NC、ND、未知许可或缺少必要署名时返回可恢复错误并回退静态墙。
- [ ] 登录页对读屏器、键盘和窄视口保持可用，背景图不抢焦点且不影响表单自动填充。

验证：

- `pnpm --dir web test -- --run web/tests/login-page.test.tsx web/tests/admin-settings.test.tsx web/tests/api-client.test.ts`
- `pnpm --dir web build`
- Playwright 桌面/窄屏检查四种来源、降级路径、图片加载失败、控制台和网络请求；不得把 mock 结果描述成已部署验证。

依赖：LUX-259、LUX-260、LUX-110。

明确不做：

- 不让插件注入任意前端代码或自定义主题；不改变既有登录表单和按钮下方留白。
- 不生成或上传媒体库图片衍生文件。

#### LUX-262：独立 Bing 每日图片插件

范围：在外部 `Lux-plugins` 仓库实现独立的 Bing 每日图片登录背景插件。复用项目所有者指定的 [`wefashe/bing-image`](https://github.com/wefashe/bing-image) 所记录的 `HPImageArchive.aspx` 数据格式，请求 `https://www.bing.com/HPImageArchive.aspx?format=js&idx=0&n=1&mkt=zh-CN`，仅取当日图片的直链、标题和版权说明。接口并非微软公开的第三方开发者 API，可能随时变化；插件不抓取 Bing 页面、不依赖第三方 API/容器，也不在服务端下载、缓存或重编码图片。图片 URL 由登录浏览器直接请求 Bing。

Bing 图片由原作者/权利人持有。上游项目将接口限于个人学习/研究，并将图片用途描述为个人壁纸；微软也说明每日图片是否可下载取决于具体图片的许可限制。因此插件必须明确提示管理员核对部署用途与适用许可，并设置启用确认；该确认不是版权授权或 Microsoft 背书。没有相应权利时不得商业使用、转载或对外再分发图片。

每日图片以 `HERO_IMAGE` 返回，由 Lux 固定 CSS 用 `object-fit: cover` 铺满登录页左侧视觉区；署名只在背景区域低干扰展示，不以居中的原比例 `SINGLE_IMAGE` 布局呈现。

验收：

- [x] 只请求 `www.bing.com` 的 `HPImageArchive.aspx`，固定 `idx=0`、`n=1`、`mkt=zh-CN`；请求限时、关闭重定向、限制 JSON 响应大小，畸形/空结果及上游错误返回可恢复插件错误。
- [x] 严格校验上游图片为 Bing HTTPS 直链，拒绝非 `www.bing.com` 主机、凭据、端口、片段、非 `/th` 路径及非 `_1920x1080.jpg` 当日大图。
- [x] 使用独立 ID、manifest、配置和版本；只输出一张 `HERO_IMAGE`，保留 Bing 给出的图片 URL，不自行代理、下载、存储、编辑或重编码图片。
- [x] 标题与版权说明仅按纯文本输出并限制长度；上游错误、恶意 URL、空结果和无法确认图片信息时回退固定海报墙。
- [x] manifest 明确声明网络/图片主机；配置要求管理员确认已核对适用许可并限定个人用途，且说明确认本身不授予图片版权。
- [x] Rust 单元与 mock HTTP 测试覆盖请求参数、直链与 `HERO_IMAGE` 输出、畸形响应、恶意图片 URL、大小限制、超时/重定向；测试不访问真实 Bing。
- [x] 完成 ARM64 与 x86_64 构建、SHA-256、ZIP/manifest 校验后登记正式插件目录；仓库 `main` 分支 release workflow 自动生成正式包和 `index.json`。

验证：Lux-plugins PR #11 已合并；v0.1.0 正式包已发布并登记至 `index.json`，aarch64 与 x86_64 均含 SHA-256。外部仓库 Rust 单测、插件 SDK/目录合同测试、mock HTTP、`cargo fmt --all -- --check`、双架构发布检查均通过。

依赖：LUX-259、LUX-260、LUX-261；管理员确认适用许可为启用门槛。

明确不做：

- 不将 Bing 当作微软承诺长期支持的公共 API；上游端点变化时安全失败并回退固定海报墙。
- 不把任何 Bing 图片文件打包进插件或 Lux，也不缓存/代理图片字节。

#### LUX-263：独立 TMDb 日榜电影+剧集横幅图插件

范围：在外部 `Lux-plugins` 仓库实现单独发布、单独配置的 TMDb 登录背景插件。优先复用仓库现有 `TmdbClient` 和其已批准的内嵌 fallback API key；该 key 编译进背景插件，但不读取 `org.lux.tmdb` 元数据插件配置、不放入 manifest、不由 Lux API/RPC 返回或写入日志。只请求 TMDb Trending All 日榜（`/3/trending/all/day`），按原排序过滤电影和剧集，并返回榜单中第一张有效 `backdrop_path` 横幅图；以 `HERO_IMAGE` 交给 Lux 固定布局用 `object-fit: cover` 铺满登录页左侧，不生成海报墙或其他衍生拼贴。

验收：

- [x] API 固定使用 `time_window=day` 和 Trending All 混合入口；只保留 `media_type=movie` 或 `tv` 且有有效 `backdrop_path` 的项目，过滤人物及无横幅项目，按原榜单顺序选中第一项；不将海报作为回退。
- [x] 只返回一张 `https://image.tmdb.org/t/p/w1280/…` 图片 CDN URL；只生成 URL，不下载或重新编码图片。宿主以 `HERO_IMAGE` 铺满登录页左侧视觉区，不旋转、遮罩或拼贴。
- [x] 该插件具有独立 package ID、独立 manifest 和独立插件配置；使用现有 `TmdbClient` 编译内嵌 fallback key，不再要求管理员提供另一把 API key。key 不读 `org.lux.tmdb` 插件配置、不由 Lux API/RPC 返回，也不写日志。
- [x] 记录复用仓库内 `TmdbClient` 的审查结果：Trending All/day 响应字段、Rust/Tokio/reqwest 兼容性及许可证；不复用元数据插件运行时配置或生命周期。
- [x] 仅缓存榜单结构所需的图片引用和刷新时间；支持超时、限流、空榜、缺失海报和 TMDb 故障，并通过宿主静态回退恢复登录页。
- [x] 在 Lux“关于/鸣谢”区域展示获准 TMDb Logo 及要求的非背书声明；页面不暗示 TMDb 赞助或认证 Lux。
- [x] 上线前确认实际部署用途符合 TMDb API 许可；商业使用必须先取得书面许可，未确认时不发布/启用该 provider。插件配置显式要求管理员确认已核对适用许可，确认开关不替代许可本身。
- [x] 测试覆盖 movie/tv/person、缺少 backdrop、海报不回退、恶意路径、榜单顺序和首个有效项；mock HTTP 验证只请求日榜且不依赖真实 TMDb 网络。
- [x] TMDb 背景插件 v0.1.1 登记到外部仓库正式目录；仓库自动完成 ARM64 与 x86_64 构建、SHA-256、ZIP/manifest 和目录校验后，Lux 商店目录提供铺满左侧的版本。

验证：外部仓库 Rust 单测、mock HTTP fixture、`cargo fmt --all -- --check`、插件 Clippy、双架构构建及 ZIP/manifest/hash 校验均通过；v0.1.1 aarch64 与 x86_64 包已在正式 `index.json` 登记。

依赖：LUX-259、LUX-260、LUX-261。

明确不做：

- 不提供周榜、热门榜、评分榜或 Top 250；不混入人物、季或集；不使用 poster 海报路径。
- 使用范围为非商业；如实际部署涉及商业用途，必须先取得 TMDb 书面许可。插件配置中的许可核对项不构成 TMDb 授权。
- 不把本插件合并进 `org.lux.tmdb`，不复用其设置或运行时进程。
- 不转码媒体库海报、不镜像 TMDb 图片二进制、不长期缓存 TMDb 响应。

### 阶段 21：全量扫描 Manifest 与索引/后处理完成语义

全量扫描使用 Manifest 表达目录发现、不可变根路径观察和根路径覆盖状态。新建扫描固定使用 `workflow_version=2`、`discovery_format_version=3`、`discovery_mode=LITE`：目录 frontier 在进程内按有界批次推进，子目录不写入 `scan_manifest_directories`；同一事务仍提交 CAS 保护的正向文件/媒体索引、`last_seen_generation`、紧凑 presence ledger、根状态和进度。旧 workflow 或显式 `PERSISTED` 任务继续使用持久目录 frontier 恢复。扫描不再为每个新增/变化文件持久化并二次应用正向 delta。正向索引使用 `last_seen_generation` 和 change kind 作为持久检查点，`scan_job_targets` 在索引完成后按根路径游标分批物化，且必须在 probe/NFO/缩略图 worker 启动前完成。只有根路径完整可用后才生成缺失候选，并在二次文件状态确认及基线 CAS 后删除。所有可用根路径完成索引与缺失确认后，成功扫描刷新首页稳定快照并发布 `home` 事件及 `ScanCompleted`；target 物化和其余后处理继续后台执行。升级前已存在的旧版 Manifest 任务由带版本号的旧执行器继续恢复。详见 `docs/decisions/044-compact-manifest-seen-paths.md` 与 `docs/decisions/045-manifest-lite-discovery.md`。

本段记录阶段 21 完成时的 workflow 2 合同，供已有任务兼容和历史验证使用。新建扫描的前向行为由阶段 23 / LUX-288 改为 workflow 3；后续修改不得改变已创建 workflow 2 任务的恢复语义。

本阶段不增加公开扫描状态或 webhook。现有 `ScanCompleted` 与 `JOB_COMPLETED` 表示索引完成；任务在 `POSTPROCESSING` 时仍可通过现有任务阶段字段观察后处理，完成后进入 `IDLE`。升级仅新增结构，不在 migration 中遍历文件系统或转换旧队列；启动时将没有 Manifest 的旧版活动全量任务安全取消，并保留任务诊断记录，管理员重试会创建新 Manifest 扫描。

Manifest observation 一经写入不可原地修改；新 Lite 只保留 root/directory identity observation，文件 stat/fingerprint 在 discovery 内存中用于二次安全校验，稳定文件通过 generation/seen-path 状态记录。应用新增或变化条目前进行二次 stat/fingerprint 校验，必要时追加 observation。差异应用对 `filesystem_entries` 的基线 ID/fingerprint 做 CAS，防止全量任务覆盖后完成的增量扫描。只有完整可用根路径允许生成缺失删除；删除前再次确认文件状态。SQLite 与 PostgreSQL 共用 SQL 行为和一致性语义，禁止在核心路径依赖 PostgreSQL 专属批量导入/更新语法或长事务。

#### LUX-264：确定 Manifest 与扫描完成语义

范围：消除产品说明、LUX-187 与 LUX-230 对首页可见时点的冲突，固定 Manifest 生命周期、数据库边界、完成事件语义、升级和重试合同，并记录 ADR-043。该任务只更新规格和架构决策，不改变运行时行为。

验收：

- [x] 首页在扫描期间继续读取旧稳定快照；成功全量扫描在 Manifest 索引和缺失确认完成后切换快照，不等待后处理。
- [x] `ScanCompleted` webhook 与 `JOB_COMPLETED` 表示索引完成，`POSTPROCESSING`/`IDLE` 延续现有任务阶段，不新增公开事件/API 或更改 Emby 合同。
- [x] 文档规定五类 Manifest 持久化数据、根覆盖安全条件、不可变 observation、delta CAS、SQLite/PostgreSQL 共用 SQL、旧版活动任务升级和清理策略。
- [x] ADR-043 与开发规格记录相同决定；`git diff --check` 通过。

验证：`git diff --check`。

依赖：LUX-154、LUX-187、LUX-230、LUX-246。

实现文件：`docs/LUX-DEVELOPMENT.md`、`docs/decisions/043-full-scan-manifest.md`。

后续规格演进：LUX-264 的勾选项记录 Manifest 阶段完成时的合同。阶段 23 仅为新建 workflow 3 定义渐进显示与早期本地旁车工作；workflow 1/2 以及已持久化任务继续沿用原首页、targets-ready 和完成事件语义。

#### LUX-265：Manifest schema 与跨数据库存储合同

范围：新增 `scan_manifests`、`scan_manifest_roots`、`scan_manifest_directories`、`scan_manifest_entries`、`scan_manifest_deltas` 五类持久化数据及 SQLite/PostgreSQL 一致的约束、索引、Rust 存储类型和有界写入接口。Manifest 保存工作流版本；旧版任务保持旧恢复语义，新建任务使用流式正向索引工作流。根路径维护事务内分配的 observation 序号，避免每条文件 observation 执行相关 `MAX(sequence)` 查询。Manifest 状态为 `DISCOVERING`、`READY_TO_DIFF`、`APPLYING`、`INDEXED`、`POSTPROCESSING`、`COMPLETED`、`FAILED`、`CANCELLED`；根路径状态为 `PENDING`、`SCANNING`、`COMPLETE`、`UNAVAILABLE`、`INCOMPLETE`。

验收：

- [x] 空库初始化与从当前 schema 升级均建立五类 Manifest 数据及追加式 workflow/sequence 字段，SQLite/PostgreSQL 结构语义一致；既有 Manifest 默认由旧执行器恢复。
- [x] observation 以 Manifest、root、relative path 和 observation 序号唯一标识；已有 observation 只能追加新版本，不能覆盖。
- [x] 旧版 delta 保持原合同；新版只持久化 destructive REMOVE 候选，保存基线 `filesystem_entries` ID 与 fingerprint，状态变化和扫描进度可幂等、有界地提交。
- [x] 测试覆盖外键/唯一约束、状态约束、分页索引和 SQLite 参数上限；核心 SQL 无 PostgreSQL 专属语法。

验证：`cargo test --locked --test storage`；PostgreSQL migration 集成测试；`cargo fmt --all -- --check`。

依赖：LUX-264。

实现文件：`migrations/0128_full_scan_manifest.sql`、`migrations-postgres/0128_full_scan_manifest.sql`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/jobs.rs`、`tests/storage.rs`、`tests/postgres_database.rs`。

#### LUX-266：Manifest 目录发现与观察

范围：旧 workflow 的目录 frontier 和文件 observation 由 Manifest 持久化；新 workflow 使用 ADR-045 的进程内 Lite frontier，并在发现事务中提交 root observation、正向索引、presence/generation、root 计数和任务进度。创建任务不访问文件系统，实时增量扫描优先级与现有扫描锁规则保持不变。

验收：

- [x] 旧 workflow 每个目录的发现结果按有界事务持久化；新 Lite workflow 按有界事务提交正向结果和 root 状态，进程关闭后从任务 root 快照重新发现。
- [x] 根路径分别记录完整、不可用或不完整；只有完整发现的根路径可进入后续缺失判定。
- [x] 同一路径再次观察会追加 observation 版本；发现失败、取消和重试不会把未提交工作标成完成。
- [x] 现有 `reconciliation_scan_entries` 路径不再承载新 Manifest 全量发现；旧记录保留给升级兼容和历史清理。新 Lite workflow 不把子目录 frontier 写入 `scan_manifest_directories`。

验证：`cargo test --locked --test scanning_jobs`；SQLite 批次/取消/恢复覆盖。

依赖：LUX-265。

实现文件：`src/application/scanner.rs`、`src/storage/jobs.rs`、`src/storage/repository.rs`、`tests/scanning_jobs.rs`、`docs/PERFORMANCE.md`。

#### LUX-267：Manifest 差异计算与安全应用

范围：新建 discovery format 3。成功新增/变化/重新出现的正向索引以 `filesystem_entries.last_seen_generation` 与 `last_seen_change_kind` 标记本次扫描；稳定 unchanged 文件以 observed fingerprint CAS 成功后标记同一 generation 并清空本轮 change kind，不再写 `scan_manifest_seen_paths`。该 ledger 只保留已观察但未能安全推进 generation 的路径，例如准备不稳定或 CAS 冲突路径。root 与目录身份仍保留完整 observation。发现批次中的文件 stat/fingerprint 保留在内存供二次校验和 CAS 使用，并与 `filesystem_entries`/媒体索引、必要的 presence ledger、目录 frontier 和进度原子提交。`scan_job_targets` 不在正向索引事务写入；索引完成后按持久化的每根路径游标分批物化，所有根路径的 target checkpoint 原子就绪前不得启动 probe/NFO/缩略图 worker。该阶段重试必须保持已完成 target 状态，并以数据库屏障阻止增量扫描改写尚未物化的全量 generation。REMOVE 候选按完整根路径下的已完成目录分批枚举，只比较直接 FILE 子项，且要求 generation 不匹配、不存在 seen-path 记录；删除前仍须确认路径缺失并按基线 ID/fingerprint CAS。既有 workflow 1/2 与 discovery format 2 继续使用原观察行和原执行器。

验收：

- [x] 正向差异仅以 `library_root_id + relative_path` 对照 `filesystem_entries`，不以 `media_items` 单独推断删除；未变化条目不重复写媒体/文件系统索引或 targets。
- [x] v3 新增/变化/重新出现的正向索引、稳定 unchanged generation 标记、残余 presence ledger、目录 observation、frontier 和进度同事务提交；失败回滚后无部分索引或 ledger，崩溃恢复可幂等重走已提交目录。
- [x] `scan_job_targets` 在索引完成后按每根路径游标分批物化；target 行与游标原子提交，旧 workflow/discovery format 默认跳过此阶段，新 v3 Manifest 明确从未就绪开始。
- [x] 所有根路径的 target 游标完成与 Manifest `targets_ready` 屏障在同一终结事务提交；probe/NFO/缩略图 worker 和并发增量扫描均等待此屏障，重试不重置已完成 target 状态。
- [x] target 物化覆盖不完整/不可用根路径上已安全提交的正向索引，但每个消费页都核对扫描根路径身份；根替换时不推进该根游标，等待恢复后继续。
- [x] 只有完整、仍匹配 device/inode 的根路径可生成 REMOVE 候选；删除前确认路径缺失，并以基线 ID/fingerprint CAS；根路径 unavailable/incomplete、取消或 I/O 错误不删除。
- [x] discovery format 3 的 REMOVE 候选按有界目录页枚举直接文件子项；相邻目录前缀不会互相匹配，历史异常路径不会阻塞终结检查。
- [x] 扫描中的安全正向提交可见于普通列表，但首页快照仍只在索引/缺失确认结束后切换；正向提交不得覆盖并发增量结果。
- [x] v3 REMOVE delta 与索引、targets 和进度原子提交；旧版未完成 Manifest 任务仍按旧 discovery format 与 delta PENDING/APPLIED 合同恢复。

验证：`cargo test --locked --test scanning_jobs --test scanner --test storage`；回滚与全量/增量竞争测试。

依赖：LUX-266。

实现文件：`src/application/scanner.rs`、`src/storage/jobs.rs`、`src/storage/media.rs`、`src/storage/repository.rs`、`tests/scanning_jobs.rs`。

#### LUX-268：索引完成、后处理与首页原子切换

范围：成功全量扫描在 Manifest 索引、缺失确认和索引事务完成后进入现有 `POSTPROCESSING`，先刷新共享/用户首页快照并发布 `home` 及 `ScanCompleted`/`JOB_COMPLETED`，再物化 v3 的 `scan_job_targets`，最后运行 probe、NFO、封面和缩略图后处理。后处理结束进入 `IDLE`；target 物化或后处理失败不撤销索引或重发索引完成通知，重试从 target 游标继续。失败/取消时只刷新已提交的安全状态。

验收：

- [x] 扫描批次期间继续返回旧首页快照；成功索引完成后，新快照全部替换成功才发布一次 `home` 事件。
- [x] `ScanCompleted` 与 `JOB_COMPLETED` 在后处理前触发；后处理失败可重试未完成 targets，不重复执行全量发现或重复发布完成 webhook。
- [x] 旧快照并发构建不能覆盖新 generation；失败/取消路径不对不完整 root 执行缺失删除。
- [x] v3 target 物化发生在索引完成与 worker 消费之间；断点续跑可恢复，增量扫描不能越过未就绪屏障，根路径更换时保持 target 工作待重试。
- [x] NFO/图片等后处理完成后仍可按现有局部失效行为刷新条目元数据；没有后处理工作时不产生无效首页 generation。

验证：`cargo test --locked --test scanning_jobs --test webhooks`，并运行相关 home/catalog 测试。

依赖：LUX-267。

实现文件：`src/application/scanner.rs`、`src/application/home.rs`、`src/storage/jobs.rs`、`tests/scanning_jobs.rs`、`tests/webhooks.rs`。

#### LUX-269：升级、重试与 Manifest 清理

范围：升级启动时识别没有 Manifest 的旧版活动全量任务，保留诊断记录并安全取消；管理员重试时创建新 Manifest。新 Manifest 的失败/取消任务按已提交 checkpoint 重试。完成后的 Manifest 路径内容按有界批次清理，仅保留不含媒体路径的统计摘要。migration 不访问文件系统、不回填完整媒体库、不删除旧表。

验收：

- [x] 旧版活动 `RECONCILE_LIBRARY` 任务没有关联 Manifest 时被标记 `CANCELLED`，事件代码明确说明需要新扫描，已完成媒体索引不变。
- [x] 重试旧任务创建新全量 Manifest；新 Manifest 任务重试遵循其 discovery/delta checkpoint 并保持幂等。
- [x] completed Manifest 的 entries/directories/deltas 被分批清理；可重试失败 checkpoint 按现有保留策略保留，清理循环有界且重复执行安全。
- [x] 从当前 SQLite/PostgreSQL schema 升级成功；旧 `reconciliation_scan_entries` 数据不被伪装成完整 Manifest，也不在迁移事务中转换。

验证：SQLite 空库/已有库迁移、旧活动任务启动恢复、管理员重试与分批清理集成测试。

依赖：LUX-265、LUX-268。

实现文件：`src/storage/repository.rs`、`src/storage/jobs.rs`、`src/storage/database_cleanup.rs`、`tests/scanning_jobs.rs`、`tests/storage.rs`。

#### LUX-270：SQLite/PostgreSQL 兼容与扫描性能阶段门

范围：以同一组语义测试验证两种后端的 migration、bounded DML、重试/删除安全和首页事件顺序；使用代表性媒体库对比当前扫描路径与 Manifest 路径的 SQL/DML 数量、扫描吞吐、批次耗时、SQLite 锁等待、PostgreSQL WAL/锁等待和前台 p95。仅在 PostgreSQL 测试环境实际运行后记录其运行时结果。

验收：

- [x] SQLite 空库、已有库升级和完整全量扫描覆盖通过；PostgreSQL 空库迁移、已有库升级和扫描存储集成测试通过。
- [x] 两种后端执行相同的根路径保护、delta/CAS、取消、重试、索引完成与后处理事件合同。
- [x] `docs/PERFORMANCE.md` 记录数据规模、命令、硬件、数据库后端及优化前后可比指标；不得以 SQLite ARM64 数值推断 PostgreSQL/NAS 性能。
- [x] `docs/COMPATIBILITY.md` 记录扫描完成 webhook、任务阶段和首页事件时序；不改变外部 API/Emby 合同。
- [x] 完成阶段 21 全部 Rust 检查、兼容性/性能记录和本机 `uname -m`，等待项目所有者确认后再进入后续阶段。

验证：`cargo build --locked`、相关 Rust 集成测试、`cargo test --locked --all-targets`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`、`uname -m`。

依赖：LUX-265 至 LUX-269。

实现文件：`src/application/scanner.rs`、`tests/postgres_database.rs`、`tests/performance.rs`、`tests/storage.rs`、`docs/PERFORMANCE.md`、`docs/COMPATIBILITY.md`。

### 阶段 22：v3 全量扫描 I/O 并发优化

阶段 21 已建立 format 3 正向索引、紧凑 presence ledger 和双数据库兼容；新扫描默认使用 ADR-045 的 Lite frontier，旧持久 frontier 仅用于兼容恢复。目标是缩短 60,000 文件从目录发现到索引入库的全链路时间，减少 per-file/批次 SQL 往返，并让扫描跨多线程重叠 I/O 与准备而不阻塞前台 API。按 LUX-272（分阶段测量）、LUX-273（滚动式有界读前与准备流水线）、LUX-274（共同写入路径）和 LUX-275（端到端门）执行。SQLite/PostgreSQL 仍共用 SQL 语义，每个 Manifest 只由一个事务 writer 提交；CAS、原子 checkpoint、完整根删除门槛和首页事件顺序不得改变。Lite 的首轮 A/B 已减少目录 frontier 写入和总体耗时，但当前数据仍未关闭 LUX-275 的严格双后端性能门。

#### LUX-271：v3 资源感知准备并发与目录读取评估

范围：discovery format 3 的正向索引准备使用全局 `LUX_SCAN_CONCURRENCY` 覆盖、媒体库 `scanConcurrency` 和当前资源反馈确定有效并发。曾测试成对目录读取但没有得到稳定收益；最后修复了取消、目录身份和预算合同，并暂留顺序枚举和跨目录有界提交。该提交是阶段基础，不代表端到端性能验收已完成；后续任务会重测并替换当前读入调度。

验收：

- [x] discovery format 3 新增/变化文件准备使用全局覆盖、库级设置和资源反馈确定的有效并发；全局覆盖优先，环境未设置时保留库级值，范围为 1–1024。
- [x] 当前实现顺序枚举；成对 reader 实验不留在正式路径。单批次观察条目不超过 8,001（8,000 条枚举预算加一个合成目录观察），低于 8,192；目录 frontier 与提交顺序可恢复。
- [x] 目录取消、替换或发生 I/O 错误时，不提交对应未完成 frontier 或部分正向索引；已提交页可幂等恢复，增量扫描优先级与 readiness barrier 保持不变。

LUX-271 的原 60k 性能验收由 LUX-275 统一执行，避免单独 reader 试验替代扫描入库端到端指标。

验证：`cargo test --locked --test scanning_jobs --test scanner --test storage`、`cargo test --locked --test postgres_database -- --ignored --nocapture --test-threads=1`、`scripts/run-performance.sh` 的 SQLite/PostgreSQL 三轮基准，以及完整 Rust 阶段门。

依赖：LUX-265 至 LUX-270。

实现文件：`src/application/scanner.rs`、`tests/performance.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。

#### LUX-272：全量扫描分阶段耗时与阻塞剖析

范围：给 60k `ScanJobService` 基准增加低基数阶段计时，分别记录目录打开、readdir/stat、基线查询、正向分类/文件准备/recheck、数据库事务校验/目录 frontier/known-path/observation/正向索引/presence ledger/checkpoint/commit、索引完成、target 物化、无变化重扫和扫描期间前台请求。报告累计阶段耗时与墙钟时间、阶段调用数、峰值活跃 reader/准备任务、批次数、SQL/DML 和 WAL；明确并发阶段累计耗时可重叠。测试路径/日志不得暴露媒体名、用户数据、完整路径或连接信息。仅增加诊断，不改扫描行为、数据库 schema 或 API。

验收：

- [x] 每一轮 60k SQLite/PostgreSQL 基准可以区分目录打开与枚举、基线读、文件准备、事务各写入段/commit 的累计时间与次数，并同时保留关键路径的墙钟时间。
- [x] 计时使用微秒、固定阶段名、测试/诊断低基数事件；阶段时长总和不得冒充墙钟时间，并明确表示并发阶段的累计耗时可能重叠。
- [x] 三轮结果记录硬件、fixture checksum、并发峰值、批次、SQL/DML、WAL、target、无变化重扫和前台 p95；不据单次异常值下结论。

验证：scoped `cargo test --locked --test performance query_counter_classifies_dml_statements_inside_common_table_expressions`、SQLite/PostgreSQL 各一次 60k 烟测、`cargo fmt --all -- --check`、Clippy。

依赖：LUX-271。

实现文件：`src/application/scanner.rs`、`src/storage/jobs.rs`、`tests/performance.rs`、`docs/PERFORMANCE.md`。

#### LUX-273：滚动式双 reader 有界预读评估

范围：基于 LUX-272 的分项证据，试验 format 3 discovery 的滚动双 reader 与有界 read/prepare/单 writer 流水线。只在相同 60k fixture 的 SQLite 和 PostgreSQL 三轮中位数都改善且满足前台/重扫门槛时保留；否则撤掉候选实现，保留已验证的顺序 reader，并把性能优化转向测得的写入热点。

验收：

- [x] 双 reader 候选的六轮基准实测到最多两个目录 reader，并记录了与文件准备/提交的重叠及全局/库级资源并发策略。
- [x] 候选流水线以统一的 8,192 在途预算运行；六轮观测峰值不超过预算，storage 仍使用单 writer 和稳定路径顺序。
- [x] 长/小目录混合、取消、目录替换、失败后重试、缺失保护与 readiness 等回归覆盖保留路径；LUX-273 专项目标通过。
- [x] 完成相同 fixture 的 SQLite/PostgreSQL 各三轮 A/B。SQLite 首扫中位数约快 5.2%，PostgreSQL 慢约 40.9%，因此撤回双 reader 生产实现，当前路径恢复为顺序 reader。

结果：LUX-273 作为性能候选评估关闭；没有把只对 SQLite 有利、却显著拖慢 PostgreSQL 的流水线留在正式扫描路径。阶段优化继续由 LUX-274 根据 `positive_index_apply` 等实测写入阶段推进。

验证：`cargo test --locked --lib application::scanner::tests`、`cargo test --locked --test scanning_jobs --test scanner --test storage`、SQLite 60k 三轮及 PostgreSQL 60k 烟测、fmt、Clippy。

依赖：LUX-272。

实现文件：`src/application/scanner.rs`、`tests/scanning_jobs.rs`、`tests/performance.rs`、`docs/PERFORMANCE.md`。

#### LUX-274：SQLite/PostgreSQL 共同写入路径降本

范围：用 LUX-272 的事务阶段时间和 SQL/DML/WAL 证据，优化 format 3 新增/变化项的共同 storage 写入路径，减少重复 bind/SQL 往返、触发器工作或事务内逐 chunk 查询。先采用 SQLx `Any` 可执行的 bounded batch/upsert 方案；不得引入 PostgreSQL 专属 `COPY`、并行同 Manifest 写事务或改变 SQLite/PostgreSQL 数据语义。若优化需要 schema/index 调整，先拆出独立子任务并验证空库与升级 migration。

验收：

- [x] 明确列出被优化的 `positive_index_apply` 热阶段及其 DML/SQL/WAL 基线，并将其拆分到 filesystem claim 与 movie materialization；只调整有 profile 支持的批次边界。
- [x] SQLite/PostgreSQL 使用相同 SQL 与数据合同；SQLite 保持 2,000 行批次，PostgreSQL 使用 5,000 行批次并低于 bind 上限。CAS、正向索引、frontier、进度、seen ledger 和 checkpoint 仍同事务提交；相关 SQLite 与 PostgreSQL 回滚/重试及恢复测试通过。
- [x] 对 60k fixture 完成 SQLite/PostgreSQL 各三轮，记录写入阶段、索引完成、WAL/SQL/DML、无变化重扫及前台 p95。PostgreSQL DML 从 209 降为 152、索引中位数快约 10.7%；SQLite 的 batch 与 DML 不变，索引中位数在基线 5% 内，target/重扫回退均低于 5%，前台 p95 改善。WAL 中位数增加约 2.7%，已记录。

结果：LUX-274 关闭为共用写入路径的有界批次优化。PostgreSQL 事务里的 `positive_index_apply` 本身仍约 6.44 秒，SQLite 本地首扫尚未稳定快于 LUX-270 参考；因此阶段 22 的严格性能门仍由 LUX-275 验收，本结果不能外推 NAS。

补充子阶段（2026-09-26，提交 `328d034b`）：PostgreSQL 的 `media_item_provider_ids` 派生索引原先按行触发器逐条重建；迁移 `0141_statement_provider_index_refresh.sql` 改为使用 transition table 的 statement-level trigger，在一次 `media_items` 语句内批量刷新 provider 行。该优化只改变 PostgreSQL 派生索引的刷新方式，不改变 SQLite 路径或公共数据语义；空库启动、旧库升级以及 provider 插入/更新语义回归均通过。独立三轮 PostgreSQL 结果仍需与同构旧版本三轮 A/B 后才能宣称稳定加速，不能据当前数据关闭 LUX-275。

补充子阶段（2026-09-26，工作树迁移 `0143_merge_provider_refresh_into_media_search.sql`）：将 provider statement-level 刷新合并到已有的 `media_items` 搜索刷新 trigger，标题未变化时跳过重复 `media_search` upsert，并删除实际 contains 搜索计划不使用的两个 PostgreSQL `media_search` B-tree。SQLite 路径不变；空库迁移、升级、provider/search/alias/availability 语义回归均通过。与同机干净 0142 worktree 的三轮 PostgreSQL A/B 相比，60k fixture 首扫中位数 15.242 s 降至 14.642 s（约 3.9%），`positive_index_apply` 4.441 s 降至 4.250 s（约 4.3%），WAL 中位数下降约 4.3%；无变化重扫和前台 p95 均未超过 5% 回退门槛。该结果仍高于 LUX-270 PostgreSQL 9.657 s 参考，不关闭 LUX-275。

后续窄修复（2026-09-26）：迁移 `0142_filter_available_source_promotions.sql` 让 PostgreSQL source INSERT 的 availability promotion 先按 `media_items.has_available_source = 0` 过滤，再检查 filesystem entry；已可用 item 不再重复做文件存在性探测。该修复保持 source 移动、缺失和重新出现路径的原有完整重算语义。

验证：相关 `storage`/`scanning_jobs` 测试、`postgres_database` ignored 集成目标、SQLite/PostgreSQL 60k 基准、fmt、build、Clippy。

依赖：LUX-272；如依赖 LUX-273 的数据形态，则在 LUX-273 完成后执行。

实现文件（按 LUX-274 子阶段剖析结果调整）：`src/storage/jobs.rs`、`src/storage/repository.rs`、`src/storage/media.rs`、`migrations-postgres/0141_statement_provider_index_refresh.sql`、`migrations-postgres/0142_filter_available_source_promotions.sql`、`migrations-postgres/0143_merge_provider_refresh_into_media_search.sql`、`tests/storage.rs`、`tests/postgres_database.rs`、`tests/performance.rs`、`docs/PERFORMANCE.md`。

#### LUX-275：全链路扫描性能与阶段门

范围：以最终 discovery、准备与 storage 路径运行 60,000 files / 600 directories 的 SQLite/PostgreSQL 各三轮 release 基准，作为阶段 22 完成判定。除索引完成外，必须评估 120,000 targets、无变化重扫、批次尾延迟、扫描期间 50 并发前台请求、SQL/DML、WAL 和锁等待。若首轮阶段门未通过，可依据已测子阶段做一轮有界 discovery work-unit 与受数据库 bind 上限约束的 storage 批次调整，然后完整重跑阶段门。`LUX_SCAN_CONCURRENCY` 与媒体库设置值均须证明有效多任务运行、有界、可受资源反馈降档；不得在 Tokio core 线程执行阻塞 I/O，不声称 NAS/x86 性能。

验收：

- [ ] SQLite 和 PostgreSQL 的全扫描索引中位数都相对同机 LUX-270 基线有稳定改善；无变化重扫和前台 p95 均不回退超过 5%，且 batch p95 无明显长尾恶化。
- [ ] 取消、root 替换、错误回滚/重试、CAS、target readiness、首页快照与事件时序的安全测试全部通过。
- [ ] 记录各阶段三轮中位数、分布、总墙钟、fixture、硬件、数据库配置和命令；运行 build、all-targets、fmt、Clippy 与 PostgreSQL integration gate。
- [ ] 通过性能门后更新 PERFORMANCE 与本规格，并等待项目所有者确认阶段 22；若外部兼容性行为发生变化，同时更新 COMPATIBILITY。未通过则保留阶段为开放，不进入下一阶段。

2026-09-27 补充的 Jellyfin 目录批处理实验见 `docs/PERFORMANCE.md`。该原型按父目录分别解析并提交；在 60k fixture 上没有优于 Lite，特别是 PostgreSQL 首扫中位数为 63.054 秒，对照 Lite 为 6.026 秒。根据该结果，Jellyfin 对照代码和 feature 已移除，性能文档保留数据作为已否决方案的历史记录。当前 LUX-045 直接扫描全流程在满核运行 5 分钟后被停止，未得到可用的新计时；文档中的旧版 2.105 秒不作为本轮等价性能门证据。LUX-275 严格阶段门仍开放。

2026-09-27 增加 target 物化 NEW-stage 快速路径，移除该阶段每个 item 的冗余 NEW-source `EXISTS` 检查，同时保留 CHANGED 阶段的优先级判定。同机三轮的 120k target 物化中位数：SQLite 0.753 → 0.704 秒，PostgreSQL 2.907 → 2.674 秒；首扫索引、无变化重扫及其他 LUX-275 门槛仍未关闭，完整结果见 `docs/PERFORMANCE.md`。

2026-09-27 继续将每个有界 source 页的 SOURCE/ITEM target 写入合并为单条 SQL，并将页从 8k 调至 16k。相对 8k 合并写入候选，target 物化中位数由 SQLite 0.684 → 0.629 秒、PostgreSQL 2.660 → 2.466 秒；INSERT 页数各从 8 降为 4，索引和无变化重扫未出现超过 5% 的回退。SQLite 索引中位数仍为 2.914 秒，高于 LUX-270 的 2.018 秒参考，因此 LUX-275 阶段门继续开放；各阶段结果见 `docs/PERFORMANCE.md`。

2026-09-27 移除 Lite 每个正向批次重复更新根目录队列行的无效果 SQL，把该状态更新留到内存 frontier 清空后的收尾事务。SQLite release 单轮扫描 DML 从 128 降至 119；首扫 2.894 秒与之前 2.914 秒三轮中位数接近，不作为稳定加速收益。PostgreSQL 一次 release 运行完成（首扫 6.709 秒），样本不足以作性能比较；target 语义集成测试通过，完整双后端阶段门仍开放。

2026-09-27 在 LUX-275 中评估有界的两目录首批预读：只并发目录打开与第一页读取，仍按原顺序交给单一数据库 writer，单路最多 4k、提交边界仍为 8k。与已提交顺序 reader 做同 fixture 交错三轮 A/B，SQLite 首扫中位数 2.246 → 2.217 秒，PostgreSQL 6.008 → 5.791 秒；DML 和提交批次数不变，target、无变化重扫、前台 p95 与 batch p95 中位数均未回退超过 5%。SQLite 差异落在样本波动范围内，考虑额外 reader 调度复杂度，不保留该候选，代码仍使用顺序 reader。完整样本及回退决定见 `docs/PERFORMANCE.md`；LUX-275 严格阶段门保持开放。

2026-09-27 将 postprocessing target page 上限从 16k 调到 32k，继续用单条 SQL 写 SOURCE/ITEM 两类 target，不增加逐行 bind 参数。对同 fixture 交错三轮 A/B 后，60k 文件对应的 target INSERT 从 4 条减到 2 条、target DML 从 14 条减到 8 条；target 物化中位数 SQLite 630 → 560 ms，PostgreSQL 2.549 → 2.523 s。重扫、前台 p95、目录列表 p95 和 batch p95 中位数均未回退超过 5%，PostgreSQL WAL 中位数增加约 1.4%，最大锁 waiter 为 0。索引计时先于 target 阶段，不能把观察到的索引时间差归因于此调整；SQLite 索引性能门仍开放。完整数据见 `docs/PERFORMANCE.md`。

2026-09-27 优化 SQLite `media_items_search_ai`：新媒体条目插入时不再逐条查询必为空的 `item_aliases`，alias 后续变化仍由 alias trigger 更新搜索索引。新 migration `0146_skip_empty_alias_lookup_on_media_item_insert.sql` 与启动时兼容表重建逻辑均使用空 alias。60k/600 同机交错三轮后，SQLite 首扫索引中位数从 2.236 降至 2.085 秒（快约 6.7%），无变化重扫和前台 p95 基本持平；PostgreSQL 路径未改变，首扫中位数从 5.855 到 5.887 秒。SQLite 仍比 LUX-270 的 2.018 秒参考慢约 3.3%，PostgreSQL WAL 差异尚未归因，因此 LUX-275 阶段门继续开放，详见 `docs/PERFORMANCE.md`。

同日评估 SQLite provider-ID INSERT trigger 对 `provider_ids_json IS NULL` 的短路。该候选将 `movie_item_insert` 子阶段中位数降低约 2.1%，但 60k/600 全链路首扫中位数反而从 2.254 增至 2.280 秒，故撤回候选及 migration，性能日志保留否决数据。LUX-275 继续只保留有稳定端到端收益的改动。

2026-09-27 增加 SQLite migration `0147_skip_redundant_sort_title_fts_tokens.sql`：当新条目的 `sort_title` 与 `title` 仅有 ASCII 大小写差异时，FTS trigger 不再索引重复的第二份 token；独立排序标题仍照常索引，现有 FTS 行不重建。固定二进制、同 fixture 交错三轮后，SQLite 首扫中位数 2.295 → 2.247 秒（约快 2.1%），无变化重扫慢约 1.5%，前台请求和 batch p95 未回退超过 5%；SQL/DML 未变。PostgreSQL 路径不变，SQLite 首扫仍高于 LUX-270 的 2.018 秒参考，LUX-275 阶段门保持开放，完整数据见 `docs/PERFORMANCE.md`。

2026-09-27 增加 SQLite migration `0148_fts_columnsize_zero.sql`：搜索保持 FTS5 默认完整 detail，只停用 Lux 未使用的 token-count `docsize` 存储，并从媒体条目与 aliases 重建索引。9 轮固定二进制 A/B 后，SQLite 首扫中位数从 2.200 降到 2.137 秒（约快 2.9%），无变化重扫与前台 p95 均回退低于 5%；PostgreSQL 未改变。索引完成仍高于 LUX-270 的 2.018 秒参考，LUX-275 阶段门继续开放，详见 `docs/PERFORMANCE.md`。

2026-09-27 评估 SQLite/PostgreSQL 两条层级索引前缀去重：移除 `(parent_id, removed_at)` 与 `(series_id, removed_at)`，保留含 `has_available_source` 的三列复合索引。SQLite schema/查询计划和 PostgreSQL 空库迁移测试通过，但同 fixture 交错三轮首扫中位数分别为 SQLite 2.277 → 2.330 秒、PostgreSQL 5.968 → 5.962 秒，没有形成稳定的双后端首扫收益；候选 migration、兼容重建调整和回归测试已撤回。target 阶段的轻微改善及 PG WAL 变化不能代替首扫门槛，LUX-275 继续开放，完整数据见 `docs/PERFORMANCE.md`。

2026-09-27 保留 PostgreSQL provider 索引 INSERT trigger 的快速路径：在 transition table 的非空 `provider_ids_json` 过滤后才执行 `json_each_text`，搜索条目仍全部写入，NULL 与 `{}` 的 provider 索引语义不变。60k/600 同机交错三轮，PG 首扫中位数 6.143 → 5.976 秒（快约 2.7%），三组配对均改善；`positive_index_apply` 快约 2.1%，重扫、前台 p95、batch p95 和 WAL 均未超过回退门。SQLite 路径未改，LUX-275 的双后端门仍开放；完整数据见 `docs/PERFORMANCE.md`。

2026-09-27 评估 SQLite 每连接 cache 扩容、WAL 自动 checkpoint 阈值、`temp_store=MEMORY` 和 16k discovery/storage 批次。候选的首扫收益很小或不稳定，同时出现无变化重扫、目录查询或 batch p95 超过回退门的情况；没有保留任何运行时 PRAGMA 或 16k discovery 批次，正式 discovery/file/entry 上限维持 80 / 8,000 / 8,192。基准专用 PRAGMA 开关和详细对照留在 `docs/PERFORMANCE.md`；LUX-275 阶段门仍开放。

2026-09-27 逐项评估目录 Provider ID 缓存、逐文件二次 stat、SQLite cache/temp/mmap PRAGMA、父电影目录 refresh 与 8k 粗粒度双缓冲。只保留每目录惰性解析 Provider ID 的 `OnceLock`：60k 首扫 SQLite 中位数 2.280 → 2.188 秒，PostgreSQL 6.075 → 6.111 秒（波动范围内），两个后端的正向准备累计耗时均下降；双缓冲三轮 A/B 没有稳定首扫收益，已撤回。二次 stat 继续保护文件变更竞态；不缓存跨事务“已验证目录”；PRAGMA 仅保留基准实验开关，不改变运行配置。复核测试、PRAGMA 数值及双缓冲分布见 `docs/PERFORMANCE.md`。本轮使用单个扫描作业的内部并发，不是多个扫描客户端并发；LUX-275 阶段门继续开放。

2026-09-27 将严格电影/剧集库的文件名分类解析移入有界准备任务，并复用解析结果构造索引记录，避免每个媒体文件在驱动循环和准备任务中各解析一次。60k/600 同机交错三轮，SQLite 首扫中位数 2.161 → 1.947 秒（快约 9.9%），PostgreSQL 5.839 → 5.767 秒（快约 1.2%）；SQLite 首扫加 120k target 合计快约 5.7%，PostgreSQL 合计快约 1.0%。无变化重扫、前台 p95 与 batch p95 未超过 5% 回退门，DML 与 8 个正向提交批次不变。最终互斥结果类型另做一组 release 配对复测；完整样本、target 单项波动和缓存较冷的离群轮见 `docs/PERFORMANCE.md`。LUX-275 完整阶段门仍开放。

同日评估 SQLite migration `0149_skip_redundant_original_title_fts_tokens.sql`，只跳过与 title ASCII 大小写等价的重复 original-title FTS tokens，独立原文标题搜索保持不变。与干净 `275fe6c9` 基线交错三轮后，SQLite 首扫中位数 1.955 → 1.941 秒（约快 0.7%），暖缓存配对差异在 ±0.5% 内，无法证明超过运行噪声；PostgreSQL 路径未变化。SQLite WAL 文件约减少 3.7%，但没有形成稳定端到端提速，target 阶段中位数还增加约 5.3%，因此撤回 migration 和兼容 trigger 变更。完整数据见 `docs/PERFORMANCE.md`；LUX-275 继续开放。

2026-09-27 对 JoinSet→500 文件分块 `spawn_blocking` 做同 fixture 交错三轮 A/B。SQLite 首扫中位数 1.932 → 1.849 秒（快约 4.3%），PostgreSQL 5.570 → 5.726 秒（慢约 2.8%）；每轮 SQLite 都改善、每轮 PostgreSQL 都回退。无变化重扫、前台 p95、SQL/DML 和 8 个正向提交批次未明显退化，但双后端首扫门未通过，因此撤回候选，正式扫描仍使用有界逐文件 JoinSet。已采纳的 `275fe6c9` 是文件名只解析一次，不是任务分块。LUX-275 阶段门仍开放，详细数据见 `docs/PERFORMANCE.md`。

同日继续优化首次扫描的 SQLite 文件系统 claim：全批插入成功时以 `rows_affected` 快速通过，不解码逐文件 `RETURNING`；部分冲突时 savepoint 回滚并使用原查询精确确定可 claim 路径。五轮同 fixture 后，SQLite 首扫中位数 1.996 → 1.915 秒（快约 4.1%），claim 阶段快约 26%；无变化重扫、前台 p95 和 DML 未明显回退。PostgreSQL 保持原 `RETURNING` 路径，五轮首扫中位数近乎持平，重扫和前台 p95 回退分别约 1.6% / 3.3%，仍在门槛内。该改动保留为 SQLite-only 快路径；LUX-275 整体门仍开放，详见 `docs/PERFORMANCE.md`。

2026-09-27 继续修复混合媒体库分类与准备阶段重复解析：分类结果现在携带已解析的电影/分集名，准备任务直接复用。60k/600 Mixed 库同 fixture 交错三轮，SQLite 首扫索引中位数 2.839 → 2.804 秒，PostgreSQL 6.768 → 6.644 秒；两个后端 `positive_file_prepare` 累计工作时间均约减半，target、无变化重扫、前台 p95 未超过 5% 回退门，DML 和提交批次不变。增益集中在 Mixed 库，严格电影/剧集路径不变；具体数据见 `docs/PERFORMANCE.md`。LUX-275 整体阶段门仍开放。

2026-09-27 评估 PostgreSQL 文件系统 claim 无 `RETURNING` 快路径：同 fixture 五组交错后，首扫中位数 5.675 → 5.665 秒（约快 0.2%），claim 子阶段约快 2.9%，但配对结果有快有慢，且每批额外增加 savepoint SQL，未形成稳定全链路收益；因此保留 PostgreSQL 原 `RETURNING` 路径，SQLite 已验证的快路径不变。目录 key 去重也暂不采纳：PG `movie_folder_refresh` / `movie_item_prefetch` 中位数约 77 / 101 ms，而 `movie_item_insert` 约 1.807 秒；计时还包含数据库工作，尚不能证明重复路径处理值得增加映射复杂度。A 项混合库重复文件名解析复用已采纳；JoinSet 分块和双缓冲已有先前 A/B 结果且均未通过双后端门。详细数据见 `docs/PERFORMANCE.md`。LUX-275 阶段门继续开放。

2026-09-27 评估四项数据库减负：新增 PG migration `0147_drop_media_search_item_fk.sql` 移除由 `media_items` statement trigger 冗余维护的 `media_search` 外键。60k fixture 三组交错 A/B，PostgreSQL 首扫中位数 5.820 → 5.493 秒（快约 5.6%），`movie_item_insert` 1.868 → 1.552 秒（快约 16.9%）；target、重扫、前台 p95 均在 5% 观察门内，DML 与提交批次不变，删除 trigger 清理回归通过。WAL 计数高约 9.1%，取自集群级 `pg_stat_wal`，尚不能归因于此 constraint migration，需继续观察。其余候选不采纳：跳过 generation lookup 的尝试改变 root checkpoint 与增量扫描竞态顺序，故保留重放检查并增加同 generation 计数测试；availability trigger 早退重复执行已有 `0142` 的父项过滤，三轮首扫慢约 5.7%、source insert 阶段慢约 36.6%；original-title 精确相等已被此前未保留的 0149 更宽条件 A/B 覆盖。完整数据见 `docs/PERFORMANCE.md`；LUX-275 阶段门仍开放。

同日评估空 baseline 查询、事务内重复状态查询、跨批次父目录 refresh 缓存及电影批次临时 map/set。新扫描 root 在 session 开始时确认为空时，跳过后续每批的大参数 baseline 查询；保留 ADD claim、generation CAS 与文件状态校验。SQLite/PostgreSQL 三轮顺序 A/B 中空 baseline 查询分别从约 34/99–103 ms 降为 0，SQL 数分别减少 24/14，DML 不变；首扫中位数变化为 1.959→1.903 秒、5.533→5.463 秒。合并同一事务中的重复 active-job JOIN 查询后，本 fixture 的 8 次读取降为 0，SQLite/PostgreSQL SQL 数再减少 6/8，且事务末尾有条件状态更新仍负责取消竞争保护。跨批次 folder cache 因整阶段计时无法隔离可省成本且会引入跨事务缓存正确性负担，暂缓；临时 map/set 虽令 SQLite `movie_item_insert` 少约 11 ms，却使全链路首扫慢约 2.4%、batch p95 恶化，PG 阶段无可辨收益，故撤回。性能过程、各指标和限制作见 `docs/PERFORMANCE.md`；LUX-275 阶段门继续开放。

2026-09-27 继续评估 PostgreSQL 扫描写入：把 `presence_ledger` 更新改成 PG 专用 `UPDATE ... FROM incoming`，使该阶段累计工作时间快约 12.4%、无变化重扫快约 4.5%；首扫中位数慢约 1.7%，因此只作为 PG 重扫优化保留，SQLite 继续用原 SQL。新增 PG migration `0148_drop_scan_job_targets_job_fk.sql` 后，target 物化三轮中位数 2.392→1.914 秒（快约 20%）；首扫、重扫、前台 p95 和 batch p95 均在 5% 观察门内。为补偿外键原有的级联清理，删除媒体库时在同一事务中先删其任务 target，并有 PostgreSQL 集成回归。强制物化 availability 候选没有改善 source insert；PG 未用的 `media_search.sort_title` 虽有 WAL 下降迹象，但没有稳定端到端收益，均撤回。PostgreSQL 不允许 `UPDATE OF` 与 transition table 同用，继续采用 statement trigger。阶段 22 / LUX-275 严格双后端性能门仍开放，详细 A/B 和限制见 `docs/PERFORMANCE.md`。

2026-09-27 将 PostgreSQL `unixepoch()` 从逐次 `clock_timestamp()` 改为向下取整的 `statement_timestamp()`，并标记 `PARALLEL SAFE`。同 fixture 三轮分组 A/B 首扫中位数 5.852→5.384 秒（快约 8.0%），120k target 快约 8.2%，无变化重扫持平，前台与 batch p95 未回退；SQLite 不变。没有采用 `CURRENT_TIMESTAMP`，避免长事务内更新时间退回事务启动时刻。行级可用性触发器候选在 60k 条目批量缺失变更上超过 120 秒，原 statement trigger 正反向都约 3.2 秒，故保留 set-based trigger。空 provider INSERT 的 `IF EXISTS` 候选首扫慢约 1.6%、无稳定收益，撤回。全量 Manifest target 主路径已使用合并 CTE；另一通用 path/reconciliation helper 保持不变，因为不在本轮 60k 首扫主路径。详细结果见 `docs/PERFORMANCE.md`；LUX-275 阶段门仍开放。

2026-09-28 PostgreSQL 扫描写事务启用事务局部 `SET LOCAL synchronous_commit = off`，普通 metadata 写事务与 SQLite 不变。60k fixture 六轮 A/B 的首扫索引汇总中位数从 5.599 降到 5.236 秒；末三轮反序交错子集为 5.390→5.188 秒，约快 3.7%。累计提交等待中位数约从 78.9 降到 2.35 ms；target、重扫和前台 p95 未观察到超过 5% 的回退。PostgreSQL 异常退出可能丢失近期已确认的整笔扫描事务，但不会造成数据库不一致；启动会取消未完成任务而不自动续跑，需要重新发起扫描。完整样本与限制见 `docs/PERFORMANCE.md`；本结果不关闭 LUX-275 双后端阶段门。

2026-10-18 将 PostgreSQL `media_items.merged_into_item_id` 索引改为只覆盖非空合并关系的部分索引。历史 60k/600 fixture A/B 首扫中位数从 5.788 降至 5.229 秒（约快 9.7%），`movie_item_insert` 中位数从 1.817 降至 1.538 秒；候选 WAL 中位数高约 2.8%，需继续观察。SQLite schema 不变。历史候选使用 0150 版本号；当前 PostgreSQL 0150 已用于 HOMEVIDEOS，因此以新 migration `0182_partial_merged_into_index.sql` 承载相同 schema 变更。完整样本见 `docs/PERFORMANCE.md`，LUX-275 双后端阶段门仍开放。

0.6.10 发布集成还适配了扫描和关机路径的有界批处理：嵌套剧集目录来源读取、未解析 Manifest 文件物化、Manifest 根目录初始化、已移除层级行恢复及旧版扫描任务回收。全量扫描用独立容量为 1 的队列约束索引阶段；索引终结后释放队列与共享扫描许可，使下一媒体库可在前一任务继续后处理时开始索引。对应固定 SQLite fixture 的历史 SQL 计数及验证边界见 `docs/PERFORMANCE.md`；本次发布按要求未运行测试、构建或 lint，LUX-275 的完整性能阶段门仍开放。

依赖：LUX-273、LUX-274。

实现文件（含首轮未过门后的有界 discovery/storage 批次跟进；Jellyfin 对照实现已移除）：`src/application/scanner.rs`、`src/storage/jobs.rs`、`src/storage/repository.rs`、`src/storage/media.rs`、`src/storage/migration.rs`、`migrations/0147_skip_redundant_sort_title_fts_tokens.sql`、`migrations/0148_fts_columnsize_zero.sql`、`migrations-postgres/0146_skip_empty_provider_index_expansion.sql`、`migrations-postgres/0147_drop_media_search_item_fk.sql`、`migrations-postgres/0148_drop_scan_job_targets_job_fk.sql`、`migrations-postgres/0149_statement_timestamp_unixepoch.sql`、`tests/storage.rs`、`tests/search.rs`、`tests/postgres_database.rs`、`tests/performance.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。

### 阶段 23：HOMEVIDEOS「其他视频」媒体库

阶段 22 的 LUX-275 严格性能门仍开放。本功能按项目所有者 2026-09-27 的明确指示在独立 `feature/homevideos` 分支并行实施；这项授权不代表 LUX-275 已通过，也不关闭阶段 22 门。HOMEVIDEOS 交付后仍须独立完成阶段 22 验收与确认。

目标：增加内部库类型 `HOMEVIDEOS`，Lux Web 显示为“其他视频”，Emby 映射为 `CollectionType: homevideos`。该库只包含普通可播放 `VIDEO` 条目和表示磁盘目录的 `FOLDER` 条目；目录层级按原路径保留。视频文件名不参与电影/剧集分类，即使名称类似 `Movie (2024)` 或 `S01E01` 也不改类型、不创建待确认条目、不触发在线匹配。视频仍可搜索、浏览详情、播放和保存播放进度。

边界：仅处理媒体库路径中的受支持视频文件（`.strm` 继续遵守现有安全和探测规则），不增加上传接口、照片、音乐或其他资源类型。视频的本地 NFO 可读；管理员可用现有元数据编辑器手动修改，写回媒体同名 NFO 并遵循当前 metadata 镜像策略。HOMEVIDEOS 不接受在线刮削器配置，不自动在线匹配。

任务顺序与验收：

#### LUX-276：HOMEVIDEOS 类型与库配置语义

- [x] `LibraryKind` 支持 `HOMEVIDEOS` 的序列化、反序列化、数据库字符串转换；旧类型字符串和 MIXED 行为保持不变。
- [x] HOMEVIDEOS 不支持刮削器或章节数据源；服务层创建/更新不会保存这些配置，已有库切换类型时也不会遗留刮削器。
- [x] 增加类型解析、序列化和库配置拒绝/清理行为的回归测试。

依赖：无。验证：`cargo test --locked --test library`、`cargo fmt --all -- --check`。

预计文件：`src/library.rs`、`src/application/libraries.rs`、`src/api/emby_catalog.rs`、`src/application/library_covers.rs`、`tests/library.rs`。Emby exhaustive matches 仅提供空兼容分支，实际协议映射留到 LUX-281。

结果（2026-09-27）：`cargo test --locked --test library` 13 项通过；库服务 scraper-settings 单测通过；`cargo clippy --locked --lib --all-features -- -D warnings`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。

#### LUX-277：SQLite 与 PostgreSQL 类型迁移

- [x] `libraries.kind` 接受 `HOMEVIDEOS`，`media_items.item_type` 接受 `VIDEO`；既有值与关联数据不变。
- [x] SQLite 空库启动和旧库升级都执行约束升级；PostgreSQL 空库与旧库迁移都可启动，并验证既有数据不变。
- [x] 回归验证 schema 约束、外键和已有库/媒体项读取。

依赖：LUX-276。验证：`cargo test --locked --test storage`、`cargo test --locked --test postgres_database`。

预计文件：`src/storage/migration.rs`、`migrations-postgres/0150_homevideos_video_types.sql`、`tests/storage.rs`、`tests/postgres_database.rs`。

结果（2026-09-27）：`cargo test --locked --test storage` 40 项通过；PostgreSQL 数据库目标中与新功能相关的空库启动和 149→150 升级测试使用 `--ignored --exact` 在本机 PostgreSQL 实际运行，2 项通过；未加 `--ignored` 的完整 PostgreSQL 目标显示 14 项因需本地 PostgreSQL 而忽略。回归测试也确认 SQLite 重建保留 5 个首页、目录和时间排序索引。迁移 SQL 支持检查、`cargo fmt --all -- --check`、`cargo clippy --locked --lib --all-features -- -D warnings` 和 `git diff --check` 通过。

#### LUX-278：其他视频扫描与目录层级

- [x] 全量、实时增量和重新调和扫描都为每个视频创建独立 `VIDEO`，并以 `FOLDER` 保留各级磁盘目录。
- [x] 文件名不运行电影/剧集解析；视频保持本地确认，不进入待确认队列。文件变更、移动、缺失、取消和重试沿用现有扫描安全语义。
- [x] `.strm` 使用已有目标校验与探测路径。

依赖：LUX-277。验证：`cargo test --locked --test scanning_jobs`、`cargo test --locked --test storage`。

预计文件：`src/application/scanner.rs`、`src/storage/jobs.rs`、`src/storage/media.rs`、`src/storage/repository.rs`、`tests/scanning_jobs.rs`。

结果（2026-09-28）：`cargo test --locked --test scanning_jobs` 78 项通过，覆盖普通全量扫描、旧版持久 Manifest 重调和、实时增量创建、删除后 VIDEO 移除及 `.strm` 目标校验；`cargo test --locked --test storage` 40 项通过。`cargo fmt --all -- --check`、`cargo clippy --locked --lib --all-features -- -D warnings` 和 `git diff --check` 通过。

#### LUX-279：VIDEO 本地 NFO 与手动元数据

- [x] 扫描可把同名 NFO 投影到 VIDEO；NFO 内容不会把视频重新分类成电影或剧集。
- [x] 现有元数据编辑器可修改 VIDEO，并将修改原子写回媒体同目录同名 NFO；metadata 镜像启用时继续写镜像。
- [x] VIDEO 不进入在线刮削、自动识别或候选确认任务。

依赖：LUX-278。验证：`cargo test --locked --test nfo_writer`、`cargo test --locked --test scanning_jobs`。

预计文件：`src/application/nfo.rs`、`src/application/metadata.rs`、`src/application/reidentify.rs`、`src/storage/catalog.rs`、`src/storage/jobs.rs`、`tests/nfo_writer.rs`、`tests/scanning_jobs.rs`。

结果（2026-09-28）：同名 NFO 全量/增量导入均保留 VIDEO 类型；即使 NFO 根节点为 `<movie>` 或 `<tvshow>`、文件名含年份，仍不会变成电影/剧集。编辑器原子写回视频同名 NFO，并按 metadata 策略写入镜像；视频自动匹配与显式补全任务均被排除。`cargo test --locked --test nfo_writer` 22 项、`--test scanning_jobs` 79 项、`--test reidentify` 12 项通过。`cargo build --locked`、`cargo test --locked --all-targets`（572 个单元测试通过、4 个忽略；集成目标零失败）、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 和 `git diff --check` 通过。PostgreSQL 集成用例因本地无 PostgreSQL 服务按约定忽略。

#### LUX-280：普通视频目录、搜索与播放状态

- [x] Lux API 可按库类型列出 VIDEO/FOLDER；全局搜索和统计将 VIDEO 视为普通可播放视频。
- [x] 播放进度、已看状态和继续观看对 VIDEO 生效，现有电影/剧集规则不变。
- [x] 覆盖分页、搜索、播放状态和继续观看查询。

依赖：LUX-278。验证：`cargo test --locked --test catalog`、`cargo test --locked --test resume_favorites`。

预计文件：`src/api/media.rs`、`src/application/catalog.rs`、`tests/catalog.rs`、`tests/resume_favorites.rs`。

结果（2026-09-28）：Lux 媒体库筛选支持 `VIDEO` 并保留 `FOLDER` 浏览，VIDEO 搜索限定在 Lux 全局搜索；VIDEO 计入普通 `itemCount`，不增加电影或剧集计数。Lux 播放进度和已看状态适用于 VIDEO，首页继续观看会显示未看完的视频，标记已看后会移除；Emby 默认搜索与 Resume 规则保持原样，留待 LUX-281 实现其协议映射。本机 `uname -m=arm64`。`cargo test --locked --test catalog --test resume_favorites` 6 项通过；`cargo build --locked`、`cargo test --locked --all-targets`（572 个单元测试通过、4 个忽略，集成目标通过；PostgreSQL 目标的 14 项因本机没有 PostgreSQL 服务而忽略）、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 和 `git diff --check` 均通过。

#### LUX-281：Emby homevideos 与 Video 契约

- [x] Emby 虚拟视图、根视图配置与库类型报告 `homevideos`；根计数只计算本库 VIDEO。
- [x] VIDEO 返回 `Type: Video`、`MediaType: Video`，支持 `IncludeItemTypes=Video`，并能按父目录返回 FOLDER/VIDEO 子项。
- [x] Emby 播放回调进度和 Resume 可见 VIDEO；更新兼容记录并保留现有 Emby DTO 边界。

依赖：LUX-280。验证：`cargo test --locked --test mixed_library_api`、`cargo test --locked --test resume_favorites`。

预计文件：`src/api/emby_catalog.rs`、`src/application/catalog.rs`、`src/storage/catalog.rs`、`tests/mixed_library_api.rs`、`tests/resume_favorites.rs`、`docs/COMPATIBILITY.md`。

结果（2026-09-28）：Emby Views/VirtualFolders 报告 `homevideos`，视图 ChildCount 只计 VIDEO；视频 DTO 映射为 `Type: Video`、`MediaType: Video`，支持按 Video 筛选和 HomeVideos 默认搜索。HOMEVIDEOS 根及其目录默认返回 FOLDER/VIDEO 子项；Emby 播放进度回调后，Resume 能读回 VIDEO。`cargo build --locked`、`cargo test --locked --all-targets`（572 个单元测试通过、4 个忽略，集成目标全部通过；14 个 PostgreSQL 用例因本机无 PostgreSQL 服务而忽略，4 个手动性能基准按设计忽略）、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 均通过；针对性 `mixed_library_api` 和 `resume_favorites` 各 3 项通过。本机 `uname -m=arm64`。尚未用第三方客户端真实 UI 单独复测 HomeVideos。阶段 22 / LUX-275 仍开放。

#### LUX-282：初始化时选择其他视频库

- [x] 初始化的可选首个媒体库类型加入“其他视频”，提交 `HOMEVIDEOS`。
- [x] 初始化页面的其余默认行为保持不变。

依赖：LUX-276。验证：`pnpm --dir web test -- setup-page`、`pnpm --dir web build`。

预计文件：`web/src/features/auth/AdminSetupForm.tsx`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/src/app.mjs`、`web/tests/setup-page.test.tsx`。

结果（2026-09-28）：React 初始化表单和旧版初始化表单都提供“其他视频”，默认类型仍为 `MIXED`；API 输入使用 `LibraryKind` 联合类型，并将选择值提交为 `HOMEVIDEOS`。`pnpm --dir web test -- setup-page` 与 `pnpm --dir web test` 均通过（75 个 Vitest 文件、516 项；Node 样式测试 107 项），`pnpm --dir web build` 通过。构建保留 Vite 对现有 HLS 产物超过 500 kB 的提示。

#### LUX-283：管理界面创建/编辑其他视频库

- [x] 创建和编辑媒体库类型选择器提供“其他视频”。
- [x] HOMEVIDEOS 隐藏刮削器配置并提交空配置；其他类型配置行为不变。

依赖：LUX-276。验证：`pnpm --dir web test -- admin-libraries`、`pnpm --dir web build`。

预计文件：`web/src/lib/api/types.ts`、`web/src/features/admin/AdminLibrariesPage.tsx`、`web/tests/admin-libraries.test.tsx`。

结果（2026-09-28）：`Library.kind` 使用 `LibraryKind` 联合类型；管理界面的创建/编辑选择器和媒体库卡片显示“其他视频”。HOMEVIDEOS 不显示刮削器列表和实时自动刮削开关；创建提交 `scrapers: []`、关闭实时自动刮削，编辑保存也清空刮削器并关闭该开关。电影、剧集和混合库沿用原配置行为。`pnpm --dir web test -- admin-libraries` 通过（75 个 Vitest 文件、519 项；Node 样式测试 107 项），`pnpm --dir web build` 通过；构建保留现有 HLS 大 chunk 提示。

#### LUX-284：媒体目录范围过滤

- [x] Catalog 查询支持“不限目录”“根目录”“指定父条目”三种范围，并在数据库分页前过滤。
- [x] 根目录查询包含 `parent_id IS NULL` 的根文件和 `parent_id = library_id` 的根文件夹。
- [x] 现有不指定目录范围的查询保持原行为。

依赖：LUX-280。验证：`cargo test --locked --lib catalog_filter_parent_scope`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`。

文件：`src/application/catalog.rs`、`src/storage/repository.rs`、`src/storage/repository_tests.rs`。

结果（2026-09-28）：Catalog 服务新增可选根目录/父条目范围，并把范围纳入分页缓存键；存储查询在分页前应用条件。测试覆盖根目录分页、嵌套目录结果和未指定范围时的既有全库结果。定向 Rust 测试、格式检查和全目标 Clippy 均通过。实现未修改 `src/storage/catalog.rs`，因为现有仓储查询路径已能承载条件。

#### LUX-285：Lux API 按目录分页浏览

- [x] 库条目 API 支持根目录范围，只返回根目录 FOLDER 和 VIDEO。
- [x] 传入 FOLDER 的 `parentId` 时只返回该目录下的 FOLDER/VIDEO 子项。
- [x] 目录浏览保留包含任意层级可播放 VIDEO 的父文件夹，空文件夹仍隐藏。
- [x] 保留服务端分页、排序、媒体库 ACL；跨库或无权父条目不泄漏子项。

依赖：LUX-281、LUX-284。验证：`cargo test --locked --test catalog`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`。

文件：`src/api/media.rs`、`src/storage/repository.rs`、`tests/catalog.rs`。

结果（2026-09-28）：`parentId=root` 和文件夹 ID 现在按目录范围分页，仅列 FOLDER/VIDEO。查询在媒体库 ACL 范围内验证父目录与子项属于同一库；因此错误或跨库父 ID 返回空页。目录范围查询递归保留有可用 VIDEO 后代的文件夹，并隐藏空文件夹；未带 `parentId` 的查询继续走原逻辑。集成用例覆盖根目录分页、两级嵌套浏览、电影/剧集样式命名的视频、空目录、跨库引用，以及旧的全库视频和文件夹查询。`cargo test --locked --test catalog` 的 3 项、`cargo fmt --all -- --check` 和全目标 Clippy 均通过。

#### LUX-286：Web 目录浏览与搜索

- [x] 其他视频库默认列出根目录条目；选择 FOLDER 进入下一层，并能返回父目录。
- [x] VIDEO 出现在库搜索结果中，文件夹不会作为可播放媒体显示。
- [x] 浏览列表保持现有分页、排序、空状态与权限行为。

依赖：LUX-281、LUX-283、LUX-285。验证：`pnpm --dir web test -- library-page`、`pnpm --dir web build`。

预计文件：`web/src/features/library/LibraryPage.tsx`、`web/src/features/library/prefetchLibrary.ts`、`web/src/lib/api/client.ts`、`web/src/features/home/media.tsx`、`web/tests/library-page.test.ts`、`web/tests/api-client.test.ts`、`web/tests/search-and-filmography.test.tsx`。

结果（2026-09-28）：HOMEVIDEOS 初始查询 `parentId=root`，目录链接通过 URL 保存目录路径，支持多层进入和逐级返回；父目录参与 TanStack Query 缓存键，避免目录间缓存串用。目录卡片使用文件夹图标且没有播放、编辑和待确认操作；VIDEO 继续链接到详情并在全局搜索标为“其他视频”。`pnpm --dir web install --frozen-lockfile`、`pnpm --dir web test -- library-page`（75 个 Vitest 文件、521 项；Node 样式测试 107 项）及 `pnpm --dir web build` 均通过。构建保留 Vite 对现有 HLS 大 chunk 的提示。

#### LUX-287：VIDEO 详情、编辑与播放

- [x] VIDEO 详情明确显示普通视频信息，保留现有详情、元数据编辑与 NFO 入口。
- [x] 从其他视频库可播放 VIDEO；进度条和继续观看卡片按普通视频呈现。
- [x] 文件夹与普通视频有清晰且正确的交互，文件夹不显示播放/编辑动作。

依赖：LUX-279、LUX-280、LUX-286。验证：`pnpm --dir web test -- media-detail`、`pnpm --dir web test -- media-action-menu`、`pnpm --dir web build`。

预计文件：`web/src/features/detail/MediaDetailPage.tsx`、`web/src/features/media/MediaActionMenu.tsx`、`web/tests/media-detail.test.tsx`、`web/tests/media-action-menu.test.tsx`、`web/tests/home-media.test.tsx`。

结果（2026-09-28）：VIDEO 详情显示“其他视频”类型并使用普通详情布局，现有播放、手动元数据编辑和 NFO 面板均可用。动作菜单保留普通编辑和文件操作，同时隐藏在线匹配/刷新；FOLDER 不渲染动作菜单。继续观看卡片覆盖 VIDEO 的普通播放器链接、类型标签和 40% 进度条。定向测试 44 项通过；`pnpm --dir web install --frozen-lockfile`、`pnpm --dir web test`（75 个 Vitest 文件、525 项；Node 样式测试 107 项）及 `pnpm --dir web build` 均通过。测试输出包含现有 jsdom `HTMLMediaElement.load/pause` 告警；构建保留 Vite 对现有 HLS 大 chunk 的提示。

### 阶段 23：渐进扫描展示、本地优先处理与独立在线补缺

本阶段为新建扫描启用 `workflow_version=3`，发现仍使用 `discovery_format_version=3` 与 Lite frontier。workflow 1/2、已持久化任务和升级恢复继续执行其创建时的完成语义；新 workflow 的变化通过版本号选择，不能在恢复旧任务时混用新规则。详见 ADR-046。

新 workflow 每个成功提交的正向索引批次立即可被分页目录查询读取；Lux Web 在扫描期间合并失效通知并刷新当前目录/首页投影。旧条目继续显示，只有根路径完整可用、缺失经过二次文件状态确认且基线 CAS 成功后才移除。扫描进度、删除安全门和 Manifest 索引完成语义保持可区分。

每个正向批次在提交媒体索引的同一事务中登记有界的本地 NFO/图片工作意向。独立本地 worker 可以在全库遍历和最终 target 物化完成前开始处理，扫描 worker 不等待它而继续后续路径。已有 `scan_job_targets`/targets-ready barrier 仍保护必须等待全量目标的 probe、缩略图和旧 workflow worker；新旁车意向有自己的持久状态、版本校验和恢复合同。图片与 NFO 能力分别记录 PENDING/RUNNING/READY/FAILED 等本地检查状态；海报可先登记和显示，但某项能力只有在本地检查成功后才能确认为缺失。I/O 错误、根路径不可用或任务仍在处理时只能重试，不能推断缺失。

本地检查不发起网络请求。对检查成功且确实缺失的能力，应用层复用 `MetadataRequestPlan`、字段锁定、本地图片、继承图片、provider 能力与冷却规则判断是否可请求；缺失事实与可请求状态分开保存。保存缺失结果和独立调度意向必须原子完成，由现有 FILL_MISSING worker 异步领取，领取时再次检查当前数据。在线刮削不计入扫描进度，也不阻塞索引完成或本地旁车状态；一项扫描的取消不得取消已独立提交的在线任务。

自动补缺入口策略如下：

| 入口或情形 | 本地检查 | 在线补缺 |
| --- | --- | --- |
| 新建全量扫描 | 正向批次提交后立即排队 | 只对本次正向新增/变化条目，成功确认缺失且统一开关允许时排入独立 FILL_MISSING |
| 手动全量/局部扫描 | 只处理对应范围 | 复用同一开关；不得扩大为整库刷新 |
| 实时增量扫描 | 只处理本次变化条目 | 复用同一开关 `realtime_metadata_auto_match_enabled` |
| 本地元数据回填/刷新 | 正常处理 | 只记录缺失，不排在线补缺；老资源由管理员手动刷新 |
| 升级后的既有媒体库 | 正常处理 | 统一开关沿用既有实时自动匹配值；明确关闭状态不得被 migration 打开 |
| 无可用 provider、策略关闭或 VIDEO/HOMEVIDEOS | 正常适用的本地规则 | 记录原因或保持禁止，不发起网络请求 |
| 本地检查失败或根不可用 | 记录可重试异常 | 不标记为已确认缺失，不提交在线请求 |

对新建媒体库，新增/变化资源自动刮削默认开启；管理员只需维护一个开关。全量扫描只对本次新增/变化条目使用该开关，未变化的老条目不会被自动重刮。历史 `scan_missing_metadata_auto_match_enabled` 仅保留兼容，不再作为运行时策略来源。缺失任务按现有最多 100 项的定向 FILL_MISSING 作业合并，同一条目/输入版本的活跃工作去重，并在执行时重新评估缺失，只补空值且不覆盖本地或锁定数据。升级不得在 migration 中扫描文件、访问 provider 或为历史全库一次性创建刮削任务。

`ScanCompleted` webhook 和 `JOB_COMPLETED` 继续表示 Manifest 索引及缺失确认完成；`POSTPROCESSING`/`IDLE` 继续描述本地扫描后处理阶段。在线补缺保持独立任务与自己的状态、尝试和取消入口。前端刷新按批次合并，图片使用更新后的 image tag，列表仍有分页与服务端上限，不等待浏览器逐项确认，也不为 10,000 条目逐条构造请求。

#### LUX-288：渐进扫描与独立在线补缺规格

范围：只更新正式产品/兼容性规格和 ADR，明确新扫描 workflow 3 的可见性、本地任务、缺失分类、独立 FILL_MISSING 调度、升级兼容及性能验收合同。本任务不改变运行时代码或数据库。

验收：

- [x] 新旧 workflow 合同清晰：新 workflow 3 的正向批次可见且早期本地旁车可运行；workflow 1/2 和已有任务保留原语义。
- [x] 本地检查未完成/失败与已确认缺失清晰分离；在线补缺仅由本地确认和策略触发，执行时重新检查且只补缺。
- [x] 全量、局部、实时增量、升级、关闭策略、无 provider 以及 VIDEO/HOMEVIDEOS 的入口行为一致；既有关闭配置不会被升级打开。
- [x] 删除安全、`ScanCompleted`/`JOB_COMPLETED`、任务进度和 Emby DTO 边界无冲突；目录分页与通知合并要求明确。
- [x] ADR、开发规格、兼容性记录与方案稿一致；`git diff --check` 通过。

依赖：LUX-230、LUX-264。LUX-275 的双后端性能门仍开放；阶段 23 必须提供新增并发下的同 fixture A/B，不能据本机 ARM 数据宣称 NAS/x86 性能。

文件：`docs/LUX-DEVELOPMENT.md`、`docs/decisions/046-progressive-scan-and-missing-metadata.md`、`docs/COMPATIBILITY.md`、`docs/PROGRESSIVE-SCAN-METADATA-PROPOSAL.md`。

结果（2026-09-28）：正式规格与 ADR-046 已确定 workflow 3 的渐进可见、本地旁车提前消费、能力级缺失确认和独立 FILL_MISSING 调度；旧 workflow 1/2、删除 CAS、ScanCompleted 及 Emby 边界保留。`git diff --check` 通过；本任务只改文档，未运行运行时代码测试。

#### LUX-289：渐进扫描本地队列与完整性 schema

范围：以 SQLite/PostgreSQL 同版本 additive migration 建立 `scan_local_metadata_batches` 本地处理 outbox、`item_metadata_completeness` 能力级本地检查/缺失记录，以及媒体库 `scan_missing_metadata_auto_match_enabled` 策略列。只改变 schema，不在本任务加入 Rust 领域/API 字段或运行时写入。

数据合同：本地批次最多含 256 个 source 引用，带 root、来源 workflow job、序号、状态、尝试/下次重试与诊断信息；`job_id` 作为来源标识保留但不设外键，使未完成 outbox 不随扫描任务历史清理丢失，library root 删除则级联清除其工作。完整性以 `item_id + capability` 唯一标识，记录本地状态、输入 fingerprint、检查时间、错误和 nullable `is_missing`；只有 `READY` 可写入已知 missing/available，FAILED/RUNNING 等不能表示缺失。

升级策略：新建媒体库列默认开启扫描触发的补缺；迁移对已有媒体库逐行复制 `realtime_metadata_auto_match_enabled`，保留管理员已关闭的配置。migration 不遍历文件系统或访问 provider。

验收：

- [x] SQLite 的 SQLx 空库和从 0148 升级都建立两张表、策略列、状态约束、唯一索引和领取索引。
- [x] SQLite 迁移升级验证既有开关从 realtime 设置回填、新行默认开启、批次不依赖 scan job 外键，以及 root/item 删除级联。
- [x] 无效状态、READY 与 nullable `is_missing` 不一致、空/超限批次、重复 item+capability 和重复 root batch sequence 均被拒绝。
- [x] migration 不改变既有媒体表数据及 workflow 1/2 行为。

依赖：LUX-288。验证：`cargo test --locked --test storage progressive_scan_metadata`；PostgreSQL runtime 验证见 LUX-290。

文件：`migrations/0151_progressive_scan_metadata.sql`、`migrations-postgres/0151_progressive_scan_metadata.sql`、`tests/storage.rs`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-28）：SQLite 0151 空库与 0148 升级回归覆盖开关默认/回填、outbox 与完整性表、超限/空批次、重复键、状态与缺失值一致性，以及 root/item 级联；`cargo test --locked --test storage` 42 项通过。PostgreSQL migration 的真实升级和 SQLite 启动后的 catalog rebuild 由 LUX-290 验证。

#### LUX-290：SQLite catalog 重建兼容与 PostgreSQL 升级合同

范围：SQLite `migrate_sqlite_catalog_constraints` 会在 SQLx migration 后重建 `libraries`；必须保留新策略列和值。补充真实 PostgreSQL 从 0150 升级到 0151 的回归，包括已有开关回填、默认值、表约束和级联关系。本任务不增加运行时 Rust 配置读写。

验收：

- [x] SQLite 新库启动后，catalog rebuild 前后均保留 `scan_missing_metadata_auto_match_enabled` 列与回填值；已有普通 catalog 列不丢失。
- [x] PostgreSQL 0150→0151 升级保留关闭/开启设置，新库行默认开启；批次与完整性约束、唯一索引和级联语义有效。
- [x] 已连接本机 PostgreSQL 服务并实际运行 bootstrap、0150→0151 upgrade 与 HomeVideos upgrade 用例；均非默认 ignored 结果。

依赖：LUX-289。验证：`cargo test --locked --test storage progressive_scan_metadata`；`cargo test --locked --test postgres_database` 与定向 ignored PostgreSQL migration 用例。

文件：`src/storage/migration.rs`、`tests/storage.rs`、`tests/postgres_database.rs`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-28）：SQLite 旧库经启动时 catalog 重建后仍保留扫描补缺策略及 0/1 配置；SQLite `storage` 43 项通过。PostgreSQL bootstrap、0150→0151 策略/队列迁移、既有 HomeVideos 迁移用例在本机 PostgreSQL 服务上各 1 项通过。`cargo fmt --all -- --check`、`cargo clippy --locked --test storage -- -D warnings`、`cargo clippy --locked --test postgres_database -- -D warnings` 通过。

#### LUX-291：渐进扫描本地 metadata outbox 操作

范围：为 `scan_local_metadata_batches` 增加应用层可复用的内部存储类型和有界操作：最多 256 个 source 的幂等入队、稳定游标分页、并发安全的原子领取、RUNNING 状态 CAS 完成/失败、按扫描 job 取消尚未完成批次，以及进程启动时恢复遗留 RUNNING 批次。不得在本任务连接 scanner，也不做本地文件检查或在线请求。

验收：

- [x] 空批次、超限批次和重复 source 被拒绝；同一 job/root/sequence 的相同输入幂等返回，不同输入报冲突。
- [x] 领取只选择到期 PENDING/FAILED 项，事务内 CAS 为 RUNNING 并递增 attempts；并发领取不会返回同一批次。
- [x] 只有 RUNNING 批次能完成或失败；重复终结不会覆盖已有状态。失败重试时间生效。
- [x] 取消按 job 原子终结所有未完成项（包括 RUNNING），遗留 RUNNING 项可在启动恢复时重新入队；分页大小有服务端上限且排序稳定。
- [x] 存储行为测试覆盖 SQLite；相同合同在真实 PostgreSQL 上由后续 P2 双后端门验证。

依赖：LUX-290。验证：`cargo test --locked --lib storage::repository::repository_tests::progressive_scan_metadata_batches`、`cargo fmt --all -- --check`、`cargo clippy --locked --lib -- -D warnings`。

预计文件：`src/storage/jobs.rs`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-28）：SQLite outbox 单测覆盖 256 来源边界、非法/重复输入、幂等冲突、稳定分页、并发领取、到期退避、终态 CAS、job 取消和重启恢复；`cargo test --locked --lib storage::repository::repository_tests::progressive_scan_metadata_batches` 1 项通过。LUX-292 的真实 PostgreSQL 同合同用例也验证了 outbox 领取/取消/恢复；`cargo fmt --all -- --check` 与 `cargo clippy --locked --lib -- -D warnings` 通过。本任务未接入 scanner。

#### LUX-292：能力级本地完整性状态存储

范围：以 item+capability+输入版本记录本地检查 PENDING/RUNNING/READY/FAILED/CANCELLED 与已确认 missing；提供输入指纹条件更新和缺失能力的有界分页读取。本任务不接入本地检查 worker 或扫描入口。

验收：

- [x] 非 READY 不能持久化 missing；fingerprint 变化时旧确认不能被当作当前版本结果。
- [x] item+capability 唯一记录支持有限状态转换；只有输入 fingerprint 仍匹配时才能接受 READY/missing 结果。
- [x] READY 缺失能力分页按稳定游标返回并有服务端上限；失败/未确认能力不会进入缺失列表。
- [x] 进程重启时可在 worker 启动前将 RUNNING 检查恢复为 PENDING，旧 worker 不能用旧 fingerprint 回写。
- [x] SQLite 与 PostgreSQL 使用同一合同通过自动化覆盖。

依赖：LUX-291。验证：`cargo test --locked --lib storage::repository::repository_tests::progressive_scan_metadata_completeness`、`cargo test --locked --lib storage::repository::repository_tests::postgres_progressive_scan_metadata_storage_contract -- --ignored`、`cargo fmt --all -- --check`、`cargo clippy --locked --lib -- -D warnings`。

预计文件：`src/storage/metadata.rs`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-28）：SQLite 用例覆盖指纹换代时清除旧缺失、RUNNING 重启恢复、拒绝旧 worker CAS、READY missing 双页读取、失败/可用/取消状态过滤；真实 PostgreSQL 用例覆盖 bytea 指纹换代与恢复、旧结果 CAS、缺失读取及 LUX-291 outbox 操作。两个定向用例各 1 项通过；`cargo fmt --all -- --check` 与 `cargo clippy --locked --lib -- -D warnings` 通过。应用 worker 尚未接入，按后续任务实施。

#### LUX-293：缺失结果与独立 FILL_MISSING 调度意向原子提交

范围：复用现有 metadata reidentify job，将已确认缺失结果和符合当前媒体库策略的 FILL_MISSING 调度意向放入一个事务；回滚时不遗留单边状态，不建立第二套在线队列。本任务不接入本地检查 worker 或扫描入口。

验收：

- [x] 确认结果和可调度的缺失请求同事务提交，回滚不留下单边状态；重复提交按 item/能力/输入版本去重。
- [x] 同一 item 已有可复用活跃 FILL_MISSING 作业时不创建重复在线工作；策略关闭或不可执行缺失仍保留结果但不排队。
- [x] SQLite 与 PostgreSQL 使用同一合同通过自动化覆盖。

依赖：LUX-292。验证：`cargo test --locked --lib storage::repository::repository_tests::progressive_scan_metadata_dispatch`，并运行真实 PostgreSQL 存储合同用例。

预计文件：`src/storage/metadata.rs`、`src/storage/jobs.rs`、`src/storage/mod.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-28）：完整性 READY/missing 更新与策略允许的 FILL_MISSING job 在同一事务提交；PostgreSQL 按库锁串行化自动调度、按媒体项行锁与手动 item job 创建协调，SQLite 使用写事务串行化。策略关闭、不可执行条目和现有活跃 job 不会产生重复在线请求；自动 job 每批最多 100 项。SQLite 用例验证策略关闭、手动活跃 job 去重、回滚后仍为 RUNNING 及成功调度；真实 PostgreSQL 用例验证注入 INSERT 错误后的回滚、成功调度和后续能力去重。两个定向用例各 1 项通过；`cargo fmt --all -- --check` 与 `cargo clippy --locked --lib -- -D warnings` 通过。尚未接入本地检查 worker 或扫描入口，按后续任务实施。

#### LUX-294：workflow 3 正向索引与本地 outbox 原子提交

范围：新建 Manifest 扫描切换为 `workflow_version=3`，沿用已验证的 Lite frontier、正向索引和删除安全合同；迁移放宽 SQLite/PostgreSQL 的 workflow 约束并保留旧任务数据。每个已提交的正向媒体/旁车引用在同一索引事务中写入不超过 256 项的持久本地 metadata outbox 批次。root 的单调序号在同一事务中为观察记录和 outbox 批次分别预留范围，生成稳定幂等批次身份。workflow 1/2 和已持久化任务仍按原语义恢复；最终 targets-ready barrier 继续保护其余后处理。本任务只发布本地工作，不读取 NFO/图片，也不接入本地 worker。

验收：

- [x] 新扫描创建 workflow 3；workflow 1/2 的发现、计数、恢复和后处理语义保持不变。
- [x] workflow 3 正向索引实际应用的媒体来源与旁车引用，在同一事务写入有界、稳定、幂等的 outbox；事务失败不留下索引或队列单边状态。
- [x] 首批 outbox 在扫描仍处于 DISCOVERING 时可领取；扫描结束/target 物化不重复发布已处理引用。
- [x] 大目录索引事务可拆成每批最多 256 个引用，批次序号稳定且不冲突；targets-ready barrier 与根覆盖/删除 CAS 不变。

依赖：LUX-293。验证：workflow 版本约束迁移的 SQLite 与 PostgreSQL 用例、`cargo test --locked --test scanning_jobs` 全目标，以及 fmt/clippy。

预计文件：`migrations/0152_scan_manifest_workflow_three.sql`、`migrations-postgres/0152_scan_manifest_workflow_three.sql`、`src/application/scanner.rs`、`src/storage/jobs.rs`、`src/storage/repository.rs`、`tests/scanning_jobs.rs`、`tests/storage.rs`、`tests/postgres_database.rs`、`tests/admin_health.rs`、`tests/ready_version.rs`、`tests/scanner.rs`、`tests/danmaku.rs`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-28）：新扫描 workflow 3 的正向索引事务按最多 256 个 filesystem source 引用原子写入本地 outbox；DISCOVERING 期间可见，最终 target 物化不重复写入。SQLite 注入 outbox 写错验证回滚；1,025 文件批次、`.strm` 与现有 poster sidecar、workflow 2 恢复语义均有回归。SQLite/PostgreSQL workflow 约束迁移各通过；`cargo test --locked --test scanning_jobs -- --test-threads=4` 81 项通过，`cargo test --locked --all-targets -- --test-threads=4` 全目标通过，定向真实 PostgreSQL migration 用例通过；`cargo build --locked`、`cargo fmt --all -- --check` 与 `cargo clippy --locked --all-targets --all-features -- -D warnings` 通过。当前工作只完成任务发布，尚未消费本地 NFO/图片，首张海报提速需要后续 worker 与页面更新任务。

#### LUX-295：本地 outbox 后台消费与海报优先处理

范围：实现 workflow 3 本地 outbox 消费者，服务启动时恢复遗留 RUNNING 批次并以有界单 worker 从持久队列领取工作；资源正向索引后即处理该资源目录中的本地图片与 NFO，图片登记先于可能较慢的 NFO 读取。扫描索引不等待 outbox 清空；worker 仅访问本地媒体根和本地存储，不调用 provider。复用现有媒体/剧集 NFO 与图片索引逻辑，并保留 workflow 1/2 原有后处理流程。workflow 3 的缩略图回退只等待本地图片阶段完成，不等待慢 NFO；这个等待位于索引完成之后，不能延迟索引完成事件。本任务不做对既有 unchanged 项目的全库回填、不确认缺失、不调度 FILL_MISSING，也不负责 Web 实时刷新；这些由后续任务完成。

验收：

- [x] 服务启动时恢复 RUNNING 本地批次，随后以单一有界 worker 领取、完成或退避重试；应用关闭/重启后已提交批次仍可恢复。
- [x] workflow 3 的 outbox 可在扫描索引未完成时消费；扫描完成时间不等待本地 NFO/图片队列清空；workflow 1/2 原流程不变。
- [x] 电影/剧集批次先发现并登记同目录本地图片，再读取/合并 NFO；已有多源和剧集父级去重逻辑继续生效。
- [x] 本地图片阶段完成后才运行 workflow 3 的视频缩略图回退，避免本地剧集缩略图被误判为缺失；慢 NFO 不阻塞此图片屏障或索引完成。
- [x] 本地处理不调用 scraper/provider；局部失败不能误标已完成，成功批次使主页投影失效以便后续刷新。
- [x] SQLite integration tests 覆盖提前领取、poster-before-NFO、队列重启恢复和无在线请求；真实 PostgreSQL 复用已有领取存储合同。

依赖：LUX-294。验证：`cargo test --locked --test scanned_metadata --test scanned_series_metadata --test scanning_jobs`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`；阶段完成再运行全目标门。

预计文件：`migrations/0153_scan_local_metadata_image_stage.sql`、`migrations-postgres/0153_scan_local_metadata_image_stage.sql`、`src/api/legacy.rs`、`src/application/metadata.rs`、`src/application/scanner.rs`、`src/main.rs`、`src/storage/jobs.rs`、`src/storage/mod.rs`、`src/storage/repository.rs`、`src/storage/repository_tests.rs`、`tests/admin_health.rs`、`tests/danmaku.rs`、`tests/library_cover_generation.rs`、`tests/postgres_database.rs`、`tests/ready_version.rs`、`tests/scanned_metadata.rs`、`tests/scanned_series_metadata.rs`、`tests/scanner.rs`、`tests/scanning_jobs.rs`、`tests/storage.rs`、`docs/LUX-DEVELOPMENT.md`。既有 `tests/thumbnails.rs::existing_series_episode_thumbnail_is_preserved_while_poster_is_generated` 用作图片屏障回归用例。

结果（2026-09-28）：服务启动时恢复中断批次并运行一个持久 outbox worker；图片阶段先于 NFO，发现提交后唤醒 worker，扫描索引不等待 NFO/图片队列。图片写库错误或批次图片失败不会设置图片完成标记，worker 保留失败批次并退避重试。SQLite 定向目标 `scanned_metadata`（9）、`scanned_series_metadata`（2）、`scanning_jobs`（81）、`storage`（43）、`thumbnails`（17）共 152 项通过；真实 PostgreSQL 迁移合同 1 项通过；`cargo build --locked`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 通过。在线缺失标记/补刮、既有 unchanged 项目回填和 Web 实时刷新按范围留给阶段 23 后续任务；阶段 23 总体验收尚未完成。

#### LUX-296：既有资源本地元数据回填游标存储

范围：为已有媒体根路径建立一次性的、持久化且可恢复的本地元数据回填游标。存储层按 filesystem entry ID 稳定分页，每页有硬上限；提交游标使用当前值 CAS，失败重试不得跳页，根路径删除时级联清理。服务启动和 workflow 3 扫描可幂等登记需要回填的根路径。本任务只实现 schema 与存储合同，不领取页面、不读 NFO/图片、不确认缺失、不调度在线任务，也不改变扫描索引路径。

验收：

- [x] SQLite/PostgreSQL migration 均可从空库升级；回填状态约束、根路径级联与 claim 索引一致。
- [x] 同一根路径重复登记幂等；分页稳定有界；只有当前游标和 attempt 匹配时才能推进，失败/重启保留当前页。
- [x] 没有可处理来源时可持久完成；已删除/缺失或无媒体 source 的条目不产生回填候选；空根完成后领取器继续查找后续根。
- [x] SQLite 与真实 PostgreSQL 存储合同覆盖登记、并发领取、推进、失败恢复、完成和根删除。

依赖：LUX-295。验证：`cargo test --locked --lib storage::repository::repository_tests::progressive_scan_metadata_backfill`、真实 PostgreSQL 对应 ignored 存储合同、`cargo fmt --all -- --check`、`cargo clippy --locked --lib -- -D warnings`。

预计文件：`migrations/0154_scan_local_metadata_backfill.sql`、`migrations-postgres/0154_scan_local_metadata_backfill.sql`、`src/storage/jobs.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-28）：新增根级 durable backfill 游标和按 filesystem entry ID 的最多 16 项候选页；候选只包含仍有有效媒体 source 的非 missing 文件。空/无效根在同一次领取中被完成并跳过；失败和 RUNNING 恢复保持当前游标，提交与失败操作同时比较 cursor 和 attempt。SQLite 定向存储测试 1 项、真实 PostgreSQL 合同 1 项通过，覆盖空根优先、多根继续领取、并发 claim 去重、失败重试、重启恢复、旧 attempt/旧 cursor 拒写及根删除级联；`cargo fmt --all -- --check` 和 `cargo clippy --locked --lib -- -D warnings` 通过。该项只提供存储能力，后台回填消费者与启动/扫描入口登记由后续任务接入。

#### LUX-297：既有资源本地海报/NFO 回填消费者

范围：将 LUX-296 的根级持久游标接入现有 `start_local_metadata_outbox_worker`。worker 启动时幂等登记已存在的媒体根路径并恢复中断回填；每轮优先领取 workflow 3 新增/变化 outbox，只有新队列暂时为空时才领取一个最多 16 个 filesystem entry 的低优先级回填页。回填页复用同一套本地图片登记与 NFO 读取逻辑，先处理图片、再异步处理 NFO；任一阶段失败或进程中断均保留当前游标并退避重试。成功图片登记立即使主页投影失效。回填不调用 provider，不等待扫描索引或缩略图屏障，也不混入 scan job 进度。本任务不做能力级缺失判定/FILL_MISSING，不实现浏览器 SSE 更新。

验收：

- [x] worker 启动时注册现存 roots、恢复 RUNNING 回填页；worker 重复启动不会重复创建消费者。
- [x] 全库本地回填最多一次领取有界页；workflow 3 新 outbox 始终优先，图片登记先于 NFO，后续索引不等待回填完成。
- [x] 已完成索引且未变化的旧媒体，在没有新扫描 outbox 的情况下也能登记本地 poster 与 NFO；任务不发网络请求。
- [x] 图片或 NFO 错误使当前页退避重试，不推进游标；worker 重启后从当前页恢复。
- [x] SQLite 集成测试覆盖旧资源 poster/NFO 登记、无在线任务、失败后游标不前进及重试恢复；worker 领取代码先查新 outbox，再查低优先级 backfill。

依赖：LUX-295、LUX-296。验证：`cargo test --locked --test scanned_metadata --test scanned_series_metadata`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`。

预计文件：`src/application/scanner.rs`、`src/storage/mod.rs`、`src/storage/repository.rs`、`tests/scanned_metadata.rs`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-28）：现有本地 metadata worker 启动时恢复中断回填并幂等登记数据库中的 roots；队列每次先检查 workflow 3 outbox，再取最多 16 个旧文件 entry。回填复用既有本地图片/NFO enricher，图片阶段完成即失效主页投影，NFO 在有界任务集合中继续；只有两阶段成功才提交游标。worker 不持有 scraper，也不把 backfill 加入扫描完成屏障。新增测试证明无新 outbox 时旧资源仍补出 poster 和 NFO、没有创建 FILL_MISSING job；NFO 写入失败后游标不前进，恢复后同页成功。`scanned_metadata` 11 项、`scanned_series_metadata` 2 项通过；`cargo build --locked`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 通过。全量 fixture 性能对比与浏览器实时刷新仍留在阶段 23 总体验收。

#### LUX-298：渐进扫描查询刷新与本地媒体合并通知

范围：复用现有 `HomeService::invalidate` 和 `UserEventHub` 的 `home` SSE scope/1 秒合并窗口。每次 workflow 3 正向索引事务成功提交后，同步使首页缓存代次失效，再发布首页/媒体库失效通知；本地 outbox 与旧资源回填完成图片登记、NFO 更新后也失效首页缓存并发布同类通知。失败/回滚的索引事务不发事件。图片部分成功后即使同页后续能力失败也要通知；多文件和扫描/worker 并发变化合并发送，不按条目发事件。workflow 1/2 继续保留扫描期间首页稳定快照语义。Lux Web 的 `useUserEvents` 已监听 `home` 并失效首页与媒体库查询，本任务不更改 SSE payload、Emby API 或前端协议。

验收：

- [x] 安全正向索引提交后，扫描仍处于 DISCOVERING 时能收到 `home` 事件；事务失败不发事件。
- [x] 收到事件后再次读取首页不会命中提交前缓存；workflow 1/2 的最终刷新合同不变。
- [x] 新 outbox 与既有资源 backfill 的 poster/NFO 持久化后触发同一 `home` 事件；图片部分成功且页面可见时不被后续 NFO 失败吞掉通知。
- [x] 通知沿用 `UserEventHub` 合并，批量扫描不会按每个媒体/图片生成独立 SSE 消息；连接重建仍依靠既有 open 事件重新取数。
- [x] SQLite 测试覆盖事件在扫描结束前到达和回填 poster 后到达；已有 Web SSE scope/query invalidation 测试保持通过。

依赖：LUX-294、LUX-295、LUX-297。验证：`cargo test --locked --test scanning_jobs --test scanned_metadata`、`pnpm --dir web test`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`。

预计文件：`src/application/scanner.rs`、`tests/scanning_jobs.rs`、`tests/scanned_metadata.rs`、`docs/LUX-DEVELOPMENT.md`。Web listener 和 SSE query invalidation 已存在，经现有 `web/tests/lux-shell.test.tsx` 验证；若源码检查发现协议不匹配，再将 Web 修正拆成独立任务。

结果（2026-09-28）：workflow 3 正向索引事务提交后先同步失效 HomeService 缓存，再发送合并的 `home` SSE；outbox/backfill 图片与 NFO更新也刷新缓存并复用同一事件，scan terminal 改为合并发布，workflow 1/2 继续使用旧扫描期稳定快照路径。测试证明事件在 manifest 仍 DISCOVERING 时到达、回填 poster 后到达，注入索引事务失败没有事件；既有 Web SSE listener 和 query invalidation 测试未改且通过。`scanning_jobs` 81 项、`scanned_metadata` 11 项、progressive Home 单测 1 项通过；Web 75 个 Vitest 文件/526 项和样式 Node 测试通过；`cargo build --locked`、fmt、all-target clippy 通过。migration 升级使一项扫描测试的预期 schema version 从 153 更新为 154。

#### LUX-299：本地完整性能力检查批量领取存储

范围：为已有 `item_metadata_completeness` 增加有界的批量 prepare-and-claim 操作，供本地检查 worker 在一次短事务中准备并领取 item+capability+input fingerprint。新增或版本变化、FAILED/CANCELLED 的能力重置为 PENDING 并原子进入 RUNNING；同 fingerprint 的 READY 结果保持不动，同 fingerprint 的 RUNNING 不重复领取。保留现有逐项接口和 `complete_local_metadata_and_enqueue_fill_missing` 的事务合同；本任务不计算字段/图片缺失，不调用 provider，也不连接扫描 worker。

验收：

- [x] 一批检查有硬上限、拒绝空/超限 fingerprint 与重复 item+capability；新/变化/可重试能力原子进入 RUNNING。
- [x] 同版本 READY 不重置，已经 RUNNING 的同版本能力不会被第二 worker 重领；旧 fingerprint 结果仍被完成 CAS 拒绝。
- [x] 返回本次实际领取的输入位置，使调用者只为领取成功的检查提交结果。
- [x] SQLite 与真实 PostgreSQL 合同覆盖批量 prepare/claim、版本替换、重复/并发 claim、失败恢复及完成 CAS。

依赖：LUX-292、LUX-293。验证：`cargo test --locked --lib storage::repository::repository_tests::progressive_scan_metadata_completeness`、真实 PostgreSQL 对应 ignored 存储合同、`cargo fmt --all -- --check`、`cargo clippy --locked --lib -- -D warnings`。

预计文件：`src/storage/metadata.rs`、`src/storage/mod.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-28）：新增最多 512 项的单事务 completeness prepare/claim 操作，按输入位置返回本 worker 实际领取的检查；相同 READY/RUNNING fingerprint 不重领，新版本和 FAILED/CANCELLED 可重置后领取。SQLite 定向存储用例与真实 PostgreSQL storage contract 各 1 项通过，覆盖并发双领取、同版本去重、版本换代、失败恢复和旧 fingerprint 拒写；`cargo fmt --all -- --check` 与 `cargo clippy --locked --lib -- -D warnings` 通过。能力判定与扫描 worker 接线继续由后续任务实现。

#### LUX-300：从 MetadataRequestPlan 计算能力级本地缺失

范围：在现有 `MetadataSelectionService` 请求计划基础上增加纯本地 completeness 视图，分别返回 METADATA、各个启用图片类型、CREDITS、EXTERNAL_IDS、TRAILERS 的实际 missing/available，以及输入 fingerprint 和“当前按计划可请求”的标记。缺失图像按类型记录，不把多个图像合并成一个布尔值。复用现有 FILL_MISSING 字段集合、NFO projection、图片策略、锁定字段与 attempt history；区分实际缺失与当前可请求状态。VIDEO/HOMEVIDEOS/FOLDER 等禁止在线识别的类型不产生自动补缺能力。本任务只提供计算合同，不写完整性表、不入队、不调用 provider，也不连接扫描 worker。

验收：

- [x] 无 provider 网络调用时能对支持类型返回每项实际缺失状态；关闭的图片能力不会生成可自动请求项，poster/fanart 等分别标记。
- [x] 实际缺失独立于 UNAVAILABLE/冷却记录；requestable 视图尊重这些记录，并复用手动 FILL_MISSING 请求规则。
- [x] 字段锁定、继承/回退图片与本地 NFO projection 沿用现有 selection 判断；不适用的媒体类型不伪造完整请求计划。
- [x] 输入 fingerprint 随当前元数据、有效图像能力与策略变化而变化；一致输入产生稳定指纹。
- [x] 单测覆盖缺少/已有/关闭图像类型、锁定字段、NFO、Unavailable 和不支持类型。

依赖：LUX-299。验证：`cargo test --locked --lib application::candidates::tests::<本地完整性计划用例>`、`cargo fmt --all -- --check`、`cargo clippy --locked --lib -- -D warnings`。

预计文件：`src/application/candidates.rs`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-28）：`MetadataRequestPlan` 额外保留每种图像类型的 missing mask；`MetadataSelectionService` 现在可将当前本地值拆成 METADATA、单图能力、CREDITS、EXTERNAL_IDS、TRAILERS，并分别给出实际缺失与按已有 attempt history 仍可请求的计划。实际与可请求计划共享一次本地字段/NFO/图片读取，fingerprint 由当前本地投影与两类计划的稳定摘要生成。纯计划用例覆盖单图策略、Unavailable、fingerprint 换代、锁定字段/NFO 现有逻辑和 VIDEO 排除；不发请求、不写数据库。定向 candidates 单测 1 项、`reidentify` integration 12 项、`cargo fmt --all -- --check` 与 `cargo clippy --locked --lib -- -D warnings` 通过。扫描 worker 尚未调用该计划，留给下一项接线任务。

#### LUX-301：扫描本地完成后保存能力级缺失

范围：将 `MetadataSelectionService` 注入现有 local metadata worker。仅在本地图片与 NFO 检查都成功后，为该批条目计算 LUX-300 的 completeness plan，通过 LUX-299 批量 claim，把每项能力 READY/missing 结果写入 `item_metadata_completeness`。重复输入 fingerprint 幂等；过期检查不得回写。全量 outbox 与既有资源 backfill 共用同一流程；本地读取失败不写 READY/missing。本任务暂不传入自动补缺 item IDs，也不创建 `FILL_MISSING` job，不访问 provider。

验收：

- [x] 生产 `ScanJobService` 获得现有 MetadataSelectionService；两类本地队列成功后均写能力状态，unsupported 类型安全跳过。
- [x] 图片/NFO 任一失败时不产生该页的 READY/missing 结果；完整输入版本变化时旧结果不能覆盖新检查。
- [x] poster 已有标为 available、仅 poster 缺失标为 missing；媒体字段按本地 NFO 与锁定规则判定，重复扫描不重置同版本 READY。
- [x] 本地缺失记录不会直接创建在线 job，worker 本身不调用 scraper。
- [x] SQLite 测试覆盖新 outbox 与 old-item backfill；PostgreSQL 存储合同复用 LUX-299 批量 claim/CAS。

依赖：LUX-295、LUX-297、LUX-299、LUX-300。验证：`cargo test --locked --lib application::scanner::tests::local_metadata_worker_persists_capability_missing`、`cargo test --locked --test scanned_metadata --test scanned_series_metadata`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`。

预计文件：`src/application/scanner.rs`、`src/api/legacy.rs`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-28）：生产 API startup 将现有 MetadataSelectionService 注入 local metadata worker；图片与 NFO 全部成功后，为每个支持条目批量 prepare/claim capability，再用 LUX-293 事务保存 READY/available 或 READY/missing，自动入队候选仍留空。真实旧资源 backfill 与 workflow 3 outbox 都有用例：poster 存在记为 available、METADATA 缺失写入 missing；NFO 故障期间没有 completeness READY，重试成功后才写入；未产生 FILL_MISSING job。`scanned_metadata` 11 项、`scanned_series_metadata` 2 项、scanner completeness 集成单测 1 项通过；`cargo build --locked`、fmt 与 all-target clippy 通过。P6 的独立自动调度由下一任务接线。

#### LUX-302：策略感知的缺失结果与 FILL_MISSING 存储事务

范围：扩展既有本地完整性与 FILL_MISSING 原子存储接口。调用者可以显式覆盖媒体库 `scan_missing_metadata_auto_match_enabled`（供实时增量任务传入创建时保存的策略），未传入时继续使用全量/backfill 策略。合格 item 的 eligible 列表即使没有新鲜 READY 结果，也可依据同事务中当前已确认 missing 的完整性行调度，解决策略开启或 scraper 安装后重放旧缺失标记的路径。仍复用既有任务表、active job 去重、支持类型过滤和每个 job 最多 100 项；本任务不连接扫描 worker、selection plan 或 provider。

验收：

- [x] READY/missing 新结果与对应 `FILL_MISSING` job 同事务提交；回滚不留下单边状态。
- [x] 默认策略取 `scan_missing_metadata_auto_match_enabled`；显式 true/false 覆盖只作用于当前调用。
- [x] 对没有新鲜完整性结果但已有 READY/missing 状态的 eligible item 可安全重放；活跃任务去重且每个 job 有界最多 100 项。
- [x] 自动调度只包含未删除的 MOVIE/SERIES/SEASON/EPISODE；HOMEVIDEOS/VIDEO/FOLDER 不进入在线 job。
- [x] SQLite 与真实 PostgreSQL 测试覆盖开关覆盖、已有 missing 重放、失败事务、active job 去重和任务分页合同。

依赖：LUX-293、LUX-299。验证：`cargo test --locked --lib storage::repository::repository_tests::progressive_scan_metadata_dispatch_is_atomic_and_deduplicated`、真实 PostgreSQL completeness/storage 合同、`cargo fmt --all -- --check`、`cargo clippy --locked --lib -- -D warnings`。

预计文件：`src/storage/metadata.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-29）：存储事务支持本次调用的策略覆盖，并可只凭现有 READY/missing 状态重新派发；事务失败不会留下单边完整性状态，活跃 job 按 item 去重。SQLite 和真实 PostgreSQL 合同均通过；103 项边界用例确认 FOLDER 与已删除资源不入队，101 个可调度 item 被分页为 100 + 1。SQLite 定向测试、PostgreSQL ignored 合同、`cargo fmt --all -- --check` 与 `cargo clippy --locked --lib -- -D warnings` 通过。LUX-303 扫描 worker 接线继续作为独立任务处理。

#### LUX-303：本地缺失计划的独立 FILL_MISSING 调度接线

范围：将 `MetadataReidentifyService` 接入本地 completeness worker。workflow 3 outbox/backfill 与实时增量扫描的 per-job 本地目标 worker 都在本地图片和 NFO 检查成功后计算 LUX-300 completeness plan、通过 LUX-299 领取检查，并将 READY/missing 与合格 `FILL_MISSING` job 交给 LUX-302 原子提交。只有实际缺失、requestable plan 有工作、当前入口策略允许且存在所选 scraper 时才派发；无 provider、关闭策略或只有 UNAVAILABLE/cooling 能力时仍保存真实缺失。全量/backfill 使用 scan missing 开关；INCREMENTAL_SCAN 使用 job 创建时持久化的 `auto_metadata_match`，不能被扫描期间的媒体库开关变化覆盖。实时增量已由本地 completeness worker 逐项派发时，扫描终点不再重复创建另一批 changed-item FILL_MISSING job。workflow 3 不创建扫描末尾整库 FILL_MISSING；完成本地页后立即 spawn 现有 reidentify worker，不等在线任务。进程重启沿用项目现有未完成任务取消/重试合同，不自动恢复 metadata jobs。workflow 1/2 原终点流程保持不变。

验收：

- [x] Workflow 3 outbox/backfill 和增量 target worker 在本地处理成功后都持久化能力级 READY/missing；本地失败时不写完成状态。
- [x] 全量/backfill 与增量分别服从库策略和 job 创建时的策略快照；策略关闭时只记录 missing，不排在线 job；打开时只派发本轮缺失条目。
- [x] 增量本地 worker 派发后，扫描终点不重复创建 changed-item FILL_MISSING job；workflow 3 不创建整库重复任务。
- [x] 无 provider、unsupported type、计划完整或 only unavailable/cooling 时不入队；provider 网络只由现有独立 job worker 发起。
- [x] 新 job 不阻塞本地 worker 或扫描完成；活动任务去重，执行前沿用现有 FILL_MISSING 二次检查和只补空值语义。
- [x] 测试覆盖配置 provider 后按需入队、job 与扫描并行、实时/全量策略隔离、无 provider 不请求和在线结果后页面通知。

依赖：LUX-293、LUX-295、LUX-297、LUX-298、LUX-300、LUX-301、LUX-302。验证：`cargo test --locked --lib application::scanner::tests::local_metadata_worker_dispatches_fill_missing_without_blocking_scan`、`cargo test --locked --test reidentify --test scanned_metadata --test scanned_series_metadata`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`。

预计文件：`src/application/metadata.rs`、`src/application/scanner.rs`、`src/application/reidentify.rs`、`src/api/legacy.rs`、`docs/LUX-DEVELOPMENT.md`。相关行为测试放在 scanner/reidentify 内部模块。

结果（2026-09-29）：workflow 3 outbox/backfill 与实时增量本地目标 worker 现在共用能力级完整性事务。增量 enricher 返回成功处理的 item IDs，本地图片/NFO完成后才写 READY/missing；入队使用 job 保存的自动补全策略，并检查当前 item 是否有选定 scraper。增量任务终点不再重复创建另一批 FILL_MISSING；provider job 独立运行，扫描先完成，任务结束后发出 home 更新事件。增量集成测试覆盖创建时开关快照、关闭时只保存缺失和不重复入队；原 outbox 测试覆盖慢 provider、扫描与刮削并行。

验证通过：scanner completeness/dispatch/incremental 三个定向单测；`reidentify` 12 项、`scanned_metadata` 11 项、`scanned_series_metadata` 2 项、`scanning_jobs` 81 项；`cargo build --locked`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`。

#### LUX-304：阶段 23 1k/10k 扫描与本地海报性能测量

范围：用 `lux_270_manifest_job_scan_benchmark` 对 1,000 与 10,000 文件 fixture 做 SQLite/PostgreSQL 基线 A/B，使用同一生成器、相同数据库参数和当前 ARM64 环境，基线为 LUX-295 前的 `6424ab12`，候选为本分支提交。每个大小/后端/版本至少交错运行三轮，记录首扫索引、无变化重扫、target 物化、用户媒体目录列表 p95、管理库列表 p95、事务/队列和 WAL/SQLite 锁指标。新增 ignored poster-worker 基准，在相同规模 fixture 为每个电影生成有效的本地海报文件，测量首条索引可查、首张本地 poster、扫描 job 完成、全量本地 poster 队列完成、扫描期间与 poster 队列完成后的媒体目录列表 p95；本地 worker 与刮削 job 的耗时分开记录。候选本地图片索引在每批 source 内缓存父目录路径快照，避免同目录电影逐项重复 `read_dir`。若发现索引或用户媒体目录 p95 稳定回退超过 5%，记录结果并建立阶段内的专门修复任务；在修复复测通过前，不关闭阶段 23。该本机 ARM64 / PostgreSQL 版本只作为同机 A/B，不外推为 NAS/x86_64 性能结论。

验收：

- [x] 1k/10k SQLite 与 PostgreSQL 基线/候选均有至少三轮交错记录，包含环境、提交、fixture、参数和 p50/p95。
- [x] poster-worker 基准证明条目索引后可读、首张本地 poster 在扫描 job 完成前写入；完整 poster 队列时间与扫描期间/队列完成后的媒体目录列表 p95 分开报告。
- [x] 用户媒体目录 p95、管理库列表 p95、索引耗时、target 物化和不变重扫分开记录；超过阈值的指标已拆为后续任务，不与在线刮削耗时混算。
- [x] `docs/PERFORMANCE.md` 记录本机架构、PostgreSQL 版本和受限结论；阶段 23 的通过门仍开放。

依赖：LUX-303。验证：`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --release --test performance lux_270_manifest_job_scan_benchmark -- --ignored --nocapture --test-threads=1`；`LUX-304` poster-worker ignored 基准；`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`。

预计文件：`src/application/metadata.rs`、`tests/performance.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-29）：Mac16,10 / 16 GiB / ARM64，PostgreSQL 16.15。`lux_270_manifest_job_scan_benchmark` 使用同版 fixture：1k SQLite/PostgreSQL 各 8 轮，10k SQLite 5 轮、PostgreSQL 6 轮；索引中位数分别为 SQLite 48.5→48 ms / 369→355 ms、PostgreSQL 166→170.5 ms / 879→892.5 ms，均未回退 5%；DML 计数保持 37（1k）/ 79–85（10k），target 中位数持平。poster-worker A/B：候选首 poster 由基线 134→111 ms（SQLite 1k）、1028→347 ms（SQLite 10k）、314→194 ms（PostgreSQL 1k）、1719→603 ms（PostgreSQL 10k）；候选 scan job 提前返回并让本地队列继续在后台完成。目录快照缓存将候选 10k 本地海报队列完成从 7.15→5.58 s（SQLite）、51.65→37.35 s（PostgreSQL）。扫描期间的 50 并发目录列表 p95 为 0.295–0.326 s，队列完成后的 p95 为 0.036–0.058 s；两毫秒与十毫秒的 scan-active 延迟实验没有稳定改善 p95 且延长队列，未保留。A/B 的后台 worker 并发 p95 仍高于旧流程，因此 LUX-305/306 继续处理 bounded image-write 批次；阶段 23 不在本任务中关闭。

#### LUX-305：本地图片存储批次接口与事务合同

范围：新增有界 `item_images` 多 item 批量写入事务，单批最多 16 个 item，语句行数遵循 SQLite/PostgreSQL 参数上限；图片行更新与 `poster_fallback_required` 清理在同一事务内提交。保持 `(item_id, image_type, image_index)` 幂等 upsert、路径/内容变化检测和 LOCAL source 语义。若一项写入失败，整批回滚，不得留下部分海报或已清除的 fallback 标记。本任务仅增加 storage contract，不接入扫描 worker，不改变图片发现和在线刮削策略。

验收：

- [x] SQLite 与真实 PostgreSQL 覆盖多 item 图片插入、重复幂等、路径变化更新、poster fallback 更新和事务失败回滚。
- [x] 空批次不打开写事务；单个 item 的多图 index 保序；每条 SQL 写入有界。
- [x] `cargo fmt --all -- --check`、定向 SQLite/PostgreSQL storage 合同通过；all-target clippy 将在 LUX-306 合并验证。

依赖：LUX-304。预计文件：`src/storage/catalog.rs`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-29）：新增 `ItemImageBatchInsert` 与有界 `insert_item_images_batch_at_indices`，一次事务最多接收 16 个 item，并按每条 SQL 最多 64 张图片分页插入；poster fallback 清理与图片 upsert 同一事务提交。SQLite 与真实 PostgreSQL 均验证多图 index、幂等重复、poster 路径变化、空页无 SQL、17 item 拒绝和触发器注入回滚时图片/fallback 均无部分提交。LUX-306 将把该事务接入 outbox movie image worker 并重新测量队列和 p95。

#### LUX-306：本地海报 worker 使用批量图片事务并复测 p95

范围：将 LUX-305 批次接口接入 workflow 3 本地 movie poster worker。先读取并准备最多 16 个 item 的图片，再原子写入该页；本地 NFO、metadata completeness、FILL_MISSING 派发和系列 artwork 次序保持不变。benchmark 重新运行同一 1k/10k SQLite/PostgreSQL poster-worker fixture，测量首 poster、scan job 完成、队列完成、扫描期间与队列完成后的媒体目录 p95。若 worker 写入仍使用户媒体目录 p95 稳定回退超过 5%，继续调整批次大小并交错重测；不得让扫描等待 online FILL_MISSING。

验收：

- [x] poster 首次可见时间在 20 ms 观察粒度内无回退，local queue 完成时间四组均缩短；扫描 job 先于剩余后台队列完成，网络刮削保持独立。
- [x] SQLite/PostgreSQL 完整性和失败重试合同通过；scanner 与 local metadata 行为测试通过。
- [x] 同 fixture 双后端 A/B 记录 p95 和 1k/10k 队列规模；活动扫描期间用户媒体目录 p95 回退门通过，结果写入 `docs/PERFORMANCE.md`。

依赖：LUX-303、LUX-304、LUX-305。预计文件：`src/application/metadata.rs`、`tests/performance.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-29）：workflow 3 本地 movie poster worker 首个 item 保留快速路径，其余 item 每 16 个组成一页，经 LUX-305 原子批量图片事务写入；系列 artwork 路径、NFO 顺序和网络刮削策略未改。失败重试回归用触发器令第二页写入失败，确认该页无部分 poster 落库、stage 不标记完成，重试后 3 个 poster 全部入库。相关 Rust 目标 `scanned_metadata`、`scanned_series_metadata`、`scanning_jobs` 共 94 项通过。性能结果见 `docs/PERFORMANCE.md`：四组 local poster queue 中位数缩短 17.1%–78.3%，活动扫描期间 p95 最大回退 3.0%；队列完成后的 10k PostgreSQL p95 有 +4 ms 变化，已保留说明。性能数据仅代表本机 ARM64 与本地 PostgreSQL 16.15，不能外推 NAS/x86_64。此前全目标验证中的 `metadata_selection::completed_scan_automatically_matches_and_writes_metadata` 实际是 workflow 2 旧自动匹配合同，却使用默认 workflow 3 manifest，导致读取不到旧 `metadata_reidentify_jobs` 记录并报 `RowNotFound`；现已明确设为 workflow 2 并改名。修正后 `cargo test --locked --test metadata_selection` 29 项通过。最终 `cargo test --locked --all-targets` 的 582 项库测试通过，但在无关的 `emby_auth` 目标因 3 个用户名大小写断言失败而停止（实际返回小写，断言期待首字母大写）；需作为独立问题处理。此前 build、fmt、clippy 门禁通过。

#### LUX-323：完整性结果按条目分片且只提交一次

范围：修正扫描完整性消费者在 FILL_MISSING 条目超过 100 项时，对每个调度分片重复提交整批完整性结果的情况。完整性结果按 item ID 与调度条目共同分片，每条新 claim 的结果只进入一个存储事务；不具备在线调度资格的已 claim 结果仍须保存。允许对已有 READY 且确认缺失、但本轮没有新 claim 的条目传入空结果集以触发补缺调度。保留每个存储事务内缺失结果与对应调度意向原子提交、每个调度分片最多 100 个 item，以及最多 256 个 source 的 outbox 合同。

验收：

- [x] 超过 100 个合格 item、每项含多个 capability、以及非合格但已 claim 的结果，分片后每条结果恰好提交一次；每个 eligible item 恰好进入一个调度分片。
- [x] 已 READY 的缺失 item 即使没有新 claim 结果，仍可排入 FILL_MISSING；在线策略关闭时已 claim 结果仍会持久化。
- [x] 测试证明 256-source / 最多 512 capability 结果批次的结果 UPDATE 尝试数从最多 1,536 降至最多 512；不据此推断墙钟耗时。
- [x] 保持当前 SQLite/PostgreSQL 事务和失败重试语义；定向 scanner 测试、格式检查与 Clippy 通过。

依赖：LUX-293、LUX-302、LUX-303。预计文件：`src/application/scanner.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。此任务仅调整结果分片，不改变存储公共模型、schema 或数据库事务实现。

结果（2026-10-02）：完整性结果先按最多 100 个 eligible item 分片，再依 item ID 分配已 claim 结果；不属于调度列表的结果落入首个事务，每条结果只提交一次。无新 claim 但有 READY/missing eligible item 时仍产生空结果提交批次；没有在线调度资格时仍提交 claim 结果。256 个 source、最多 512 个 capability 结果由最多 3 个事务各提交至多 100 个 eligible item；结果 UPDATE 尝试上界从 512×3=1,536 降至 512。SQLite/PostgreSQL 原子事务实现未修改。两个定向 scanner 测试、`cargo build --locked`、fmt 和 all-target Clippy 通过。`cargo test --locked --all-targets` 库测试 640 项通过、10 项忽略、2 项在并行全目标负载下遇到 2 秒子进程超时；隔离重跑 `application::embedded_subtitle::tests` 3 项通过。性能推导与限制见 `docs/PERFORMANCE.md`；没有据 SQL 次数变化推断墙钟耗时。

#### LUX-324：剧集合并批量读取季度分集

范围：手动合并剧集时，避免对每个源季度分别查询分集。每次合并源剧集与主剧集时，批量读取两侧所有有效季度的有效分集，再沿用现有季度号/集号、ID 的排序与匹配规则。只减少层级读取次数，不批量改写媒体源或用户状态，不改变合并顺序、事务边界、数据库模型或 schema。

验收：

- [x] 每次源剧集合并最多执行一次季度分集读取，与季度数无关；SQLite 与 PostgreSQL 使用兼容查询。
- [x] 12 个未匹配季度的查询计数相对当前逐季读取减少至少 11 条；该计数只代表 SQL 调用，不推断墙钟耗时。
- [x] 既有重复季度/分集合并、额外季度/分集迁移、媒体源和用户状态保留、后续扫描归并行为通过回归验证。
- [x] 不改变不同源剧集依次合并时，新挂载季度可被后续源识别的语义。

依赖：LUX-251。预计文件：`src/storage/media_merge.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。测试计划：新增存储级 SQL 计数用例，并运行 `item_merge` 与 `scanner` 相关集成目标、格式检查和 Clippy。

结果（2026-10-02）：`merge_series_in_transaction` 保留目标/源季度查询和全部逐条写操作，将两侧有效季度的分集改为单条 CTE 查询，并按 parent season 分组复用；每个源剧集仍顺序处理，所以前一个源新增的季度仍会被后一个源查询并匹配。新增用例先在旧实现测得 32 条存储查询，再在新实现测得 21 条（减少 11 条，约 34.4%）；这是 12 个空源季度的 SQL 调用计数，不是时延基准。SQLite 存储测试 2 项、`item_merge` 1 项、扫描重扫回归 1 项通过；`cargo build --locked`、`cargo test --locked --all-targets`（644 项通过、10 项忽略）、fmt 和 all-target Clippy 通过。查询形状使用 SQLite/PostgreSQL 通用 CTE 与固定 4 个 bind 参数，没有 PostgreSQL 实例运行本任务行为测试。

#### LUX-325：统一限制 STRM 扫描读取大小

范围：workflow 3 manifest 已将 `.strm` 文件内容限制为 1 MiB，但旧扫描/回退读取仍调用无界 `read_to_string`。统一所有扫描路径的读取上限为 1 MiB；最多读取上限加 1 字节以发现超限，超限时返回与 manifest 路径一致的无效数据错误。上限内仍按首个非空行分类，播放目标原文与协议语义不变。

验收：

- [x] legacy、manifest 回退与 manifest 读取都最多读取 1 MiB 加 1 字节用于超限判定，保留的 STRM 内容也有相同上界。
- [x] 超过上限的 STRM 明确报错；有效边界内的 BOM、空行、首个非空目标和 UTF-8 校验行为保持不变。
- [x] 增加非 manifest 读取的超限回归测试，既有 STRM 分类与扫描测试通过；fmt 和 Clippy 通过。

依赖：无。预计文件：`src/application/scanner.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。该任务只统一文件读取边界，不修改存储模型或扫描调度。

结果（2026-10-02）：统一 STRM 上限常量；普通扫描与 manifest 回退路径从无界 `read_to_string` 改为最多读取 1 MiB 加 1 字节，manifest 路径继续使用 root-relative 安全打开并执行同一上限检查。超限均返回 `InvalidData`。非 manifest 和 manifest 超限回归测试通过；既有首个非空行/BOM/目标分类合同不变。`cargo build --locked`、`cargo test --locked --all-targets`（645 项库测试通过、10 项忽略及全部集成目标通过）、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 通过。本任务确认了读取字节上界，没有新增 I/O 次数或耗时基准，也不推断端到端性能变化。

#### LUX-326：在 CI 中运行完整项目质量门

范围：当前 GitHub Actions 工作流构建 Docker 镜像，但未直接执行仓库的 `scripts/check-all.sh`。新增独立质量工作流，保留手动 `workflow_dispatch` 入口运行统一脚本，不对 Pull Request 或分支推送自动触发；设置只读仓库权限，并安装项目要求的 Rust、Node 与 pnpm 工具链。不得在质量工作流中发布镜像、读取仓库 secrets 或修改部署流程。

验收：

- [x] 工作流可通过 `workflow_dispatch` 手动运行，不因 PR 或 `main`/`test` 推送自动触发。
- [x] CI 使用受控的 Rust stable（含 rustfmt/clippy）、Node 22 和固定 pnpm 版本，并运行 `scripts/check-all.sh`。
- [x] 权限最小化为仓库只读；静态 YAML 校验和本地项目检查通过。

依赖：无。预计文件：`.github/workflows/quality.yml`、`docs/LUX-DEVELOPMENT.md`。此任务提供现有质量脚本的手动触发入口，不修改应用代码、Docker 发布或分支保护设置。

结果（2026-10-05）：独立只读 `Project quality` 工作流保留 `workflow_dispatch` 手动入口，不再监听 `main`/`test` 的 PR 或 push；使用 Rust stable、clippy/rustfmt、Node 22 和 pnpm 11.19.0，执行统一 `scripts/check-all.sh`。没有修改仓库分支保护规则。

#### LUX-327：拆分扫描器 Manifest 辅助模块

范围：`src/application/scanner.rs` 将 Manifest 专用数据类型、目录枚举、安全文件/目录 stat、观察校验、STRM 读取及 delta 准备辅助逻辑与顶层扫描编排放在同一文件。将这些 Manifest 辅助逻辑提取到 `src/application/scanner/manifest.rs`，扫描编排继续通过 scanner 内部接口调用。保持路径规范化、root 身份检查、`O_NOFOLLOW`/`O_NONBLOCK`、文件指纹 CAS、读取上界和错误行为不变；不借此修改扫描算法或扩大缓冲/并发。

验收：

- [x] Manifest 专用类型与文件访问/准备助手集中在独立子模块，`scanner.rs` 的 Manifest 流程保留编排和调用边界。
- [x] Manifest 安全打开、root 替换、文件变化、STRM 上限与扫描取消/恢复合同保持不变。
- [x] 扫描器单测、`scanner` 和 `scanning_jobs` 集成目标、格式检查与 all-target Clippy 通过；没有性能提升声明。

依赖：LUX-266。预计文件：`src/application/scanner.rs`、`src/application/scanner/manifest.rs`、`docs/LUX-DEVELOPMENT.md`。这是纯模块拆分，不修改用户可见行为、存储模型或性能策略。

结果（2026-10-02）：将约 1,800 行 Manifest 专用类型、目录发现/安全文件访问、观察验证、受限 STRM 读取和 delta 准备助手移至 `scanner/manifest.rs`；扫描流程仍留在 `scanner.rs`，仅添加内部可见性供父模块调用。`cargo fmt --all -- --check`、`cargo build --locked` 和 all-target Clippy 通过。扫描器单测 28 项、`scanner` 目标 17 项通过；`scanning_jobs` 首轮 80/81，其中一个带 750 ms 时限的等待用例超时，单项隔离复跑和随后完整目标重跑均通过（81/81）。没有改测试时限或扫描行为，不据模块拆分声称运行时性能提升。

#### LUX-328：复用电影版本后缀推断结果

范围：常规电影重扫的未变化检查会先推断同目录版本后缀，确认条目需要刷新身份；之后分组和实际扫描又会查询同一批候选兄弟文件。常规变更重扫与 reconciliation 也会在分组后再次推断，兼容性 reconciliation 会在预检查后再次推断。将已经得到的后缀随待扫描项传到文件扫描逻辑；这些连续调用链只推断一次，没有预计算结果的直接调用仍按原逻辑推断。保持候选文件判定、分组顺序、文件变化处理和电影身份语义不变。

验收：

- [x] 常规重扫的未变化检查、电影分组、reconciliation 分组和兼容性预检查向实际扫描传递已得到的后缀，不重复执行候选兄弟文件的 `symlink_metadata` 查询。
- [x] 电影后缀推断的单测记录代表性多连字符文件所需的 metadata 探测数；已有全量扫描与变体重扫回归保持不变。
- [x] `scanner` 目标、格式检查、build 与 all-target Clippy 通过；性能记录只报告文件系统查询次数，不把次数变化推断成耗时收益。

依赖：LUX-323、LUX-327。预计文件：`src/application/scanner.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。不修改数据库、扫描并发、Manifest 安全边界或目录读取策略。

结果（2026-10-02）：未变化变体快检得到的后缀现在沿重扫路径复用；普通变更重扫、reconciliation 分组以及兼容性预检查也把分组/预检查时计算的结果交给实际扫描。新增探测计数测试验证 `ADN-725-Alternate-Cut.mp4` 的代表性候选算法执行 13 次 metadata 探测，变体重扫回归验证 `Alternate-Cut` 身份仍被恢复。Toshiba 上 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target ./scripts/check-all.sh` 全部通过，包含 Rust build、all-target 测试、fmt、all-target Clippy、Python 检查和 Web 冻结安装/测试/生产构建；本机架构 `arm64`。性能记录仅量化候选查询次数，不声称系统调用或实际耗时收益。

#### LUX-329：批量恢复人物清单中的 provider 身份

范围：人物清单恢复当前在每个身份上分别查询归属，并逐条执行身份 INSERT；清单恢复会串行处理多个人物，单个人物也可以有多个 provider 身份。将同一个人物的归属预检查与 INSERT 改为有界批次，减少与身份数量成比例的 SQL 往返。身份归属冲突仍在写入人物前报告，使用原输入顺序选择首个冲突；人物/序列/身份写入仍处于同一事务，`ON CONFLICT DO NOTHING` 和清单校验语义保持不变。

验收：

- [x] 对 4 个无冲突身份的恢复查询计数从逐身份读写的 11 条降至 5 条；测试同时验证四个身份都保存。
- [x] 批次大小不超过 100 个身份；覆盖跨批次恢复和已有身份归属冲突，不部分写入其他身份或人物。
- [x] `storage` 定向测试、build、fmt 和 all-target Clippy 通过；性能记录只报告 SQL 查询计数，不据此推断墙钟耗时。

依赖：现有人物 Manifest 恢复合同。预计文件：`src/storage/people.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。不修改 schema、恢复格式、人物关系恢复策略或并发语义。

结果（2026-10-02）：人物身份归属预检查和 INSERT 都改为每批最多 100 个身份。storage 查询计数从 4 身份时 11→5、205 身份时 413→9；冲突回归确认冲突检查在人物写入前完成，既有冲突身份不被移动，新身份与人物均不落库。storage 定向测试 3 项通过；Rust build、all-target 测试（649 通过、10 忽略）、fmt、all-target Clippy、shell 语法、Python 检查（3/3 与 2/2）通过。本机架构 `arm64`。首次 `./scripts/check-all.sh` 在 Web 测试中有一个剧集加载期间播放导航用例失败；定向复跑、完整 Vitest 复跑（546/546）、随后 `pnpm --dir web test`（Node 108/108、Vitest 546/546）和 Web 生产构建均通过。失败未复现，未修改 Web 文件；完整脚本的首次退出码 1 如实保留。性能记录只报告 storage SQL 查询调用计数，不推断墙钟耗时。

#### LUX-330：避免人物重建启动时的无变化任务写入

范围：人物索引恢复启动时先读取启用库 ID，再逐库 upsert 重建任务；冲突分支无条件更新 `updated_at`，即使 schema 与任务状态均未变也会写行。改为从启用库集合执行一条批量 upsert，并且只在 schema 版本变化或 `RUNNING` 已超过 60 秒时更新已有任务。保持禁用库过滤、schema 重置、过期运行回收和最终任务列表语义。

验收：

- [x] 4 个启用库同步时 storage 查询计数由 6 条降为 2 条；单条批量 upsert 覆盖所有启用库。
- [x] 同步未变化任务不会触发 UPDATE；过期 `RUNNING` 与 schema 变更仍重置所需字段，活动任务仍保留 run token、游标、进度和取消状态。
- [x] `storage` 定向测试、build、fmt 和 all-target Clippy 通过；性能记录分别报告查询与写入计数，不据此推断墙钟耗时。

依赖：LUX-188 人物索引重建任务合同。预计文件：`src/storage/people.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。不修改人物索引算法、任务恢复期限、并发领取或 schema。

结果（2026-10-02）：启用库筛选、任务创建/恢复与最终列表查询合并为单条批量 upsert 加一次列表读取；4 个启用库的 SQL 调用计数为 6→2。SQLite UPDATE 触发器验证未变化同步写入 0 行，schema 变化更新 4 行；任务状态测试覆盖过期运行回收与活动运行的 token、游标、进度、取消标记保留。`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target ./scripts/check-all.sh` 最终通过：build、all-target 测试（650 个库测试通过、10 个忽略，所有集成目标通过）、fmt、Clippy、shell/Python 检查及 Web 冻结安装、测试和生产构建均通过。首次脚本运行仅因新增测试的 Clippy `type_complexity` 报错退出；为测试 SQL 行定义命名类型别名后，独立 Clippy 与完整脚本复跑通过。本机架构 `arm64`。性能记录只报告 SQL 与 UPDATE 行计数，不推断耗时收益。

#### LUX-331：批量读取人物清单恢复状态

范围：人物清单恢复逐项读取 `person_manifest_index_state` 来跳过校验和未变化的清单。改为每最多 100 份有效清单批量预读索引状态；完整 person ID、checksum 和 schema version 都匹配时跳过单项 storage 调用，不匹配时继续走既有单人物校验与事务恢复。批量预读失败时回退到既有逐项路径，保持恢复错误隔离和并发语义。

验收：

- [x] 205 份有效且校验状态未变的清单，storage 查询调用由 205 条降为 3 条；状态比较包含 person ID、checksum 与 schema version。
- [x] 查询批次最多 100 个 ID；修改过的清单仍恢复，provider 身份冲突仍按原逻辑跳过且不会部分写入。
- [x] 人物清单恢复定向测试、build、fmt 和 all-target Clippy 通过；性能记录只报告查询调用数，不据此推断墙钟耗时。

依赖：LUX-329、现有人物 Manifest 恢复合同。预计文件：`src/storage/people.rs`、`src/application/people/rebuild.rs`、`src/application/people/service.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。不修改 schema、人物关系恢复策略、Manifest 格式或事务边界。

结果（2026-10-02）：恢复流程每批最多 100 份有效清单，按 person ID、checksum、schema version 精确对照已存状态；未变化清单跳过单项查询，变化清单仍进入原有校验与原子恢复路径，批量状态读取失败会回退逐项检查。205 份未变化清单的 storage 查询调用由 205 降为 3。定向恢复测试和 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target ./scripts/check-all.sh` 通过：651 个库测试通过、10 个忽略，all-target 集成测试、fmt、Clippy、shell/Python 检查及 Web 测试和生产构建通过。本机架构 `arm64`；PostgreSQL 专用用例因本机无 PostgreSQL 实例而按配置忽略。生产构建仍报告 `hls.js` chunk 超过 500 kB（594.13 kB，gzip 185.60 kB）；播放器通过动态 import 延迟加载该 chunk，因此没有把它算作首页 bundle 回归，也未在缺少浏览器性能基线时改动打包策略。

#### LUX-332 使用 HLS.js light build 缩小服务器 HLS chunk

范围：Lux Web 的 `SERVER_HLS` 播放只加载 `hls.js/light`。本地服务器 HLS 使用 fMP4/CMAF，仅映射一个视频流和一个选中的音频流，不输出 HLS 字幕轨或备用音轨；保持原生 HLS 优先路径及现有 HLS.js manifest/error 生命周期。不修改 Emby HLS、播放计划、FFmpeg 输出或用户可见播放策略。

验收：

- [x] 原生 HLS 继续直接设置 video source；MSE 路径加载 light build、处理 manifest parsed/error 事件并在销毁时释放实例。
- [x] 生产构建中的 HLS chunk 与 gzip 字节数均下降；入口 chunk 不变，HLS chunk 不再触发 500 kB 警告。
- [x] `pnpm --dir web install --frozen-lockfile`、播放器定向测试、完整 Web 测试与生产构建通过。

预计文件：`web/src/features/player/hls-playback-engine.ts`、`web/src/types/hls-js-light.d.ts`、`web/tests/hls-playback-engine.test.ts`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。没有真实浏览器播放启动时间样本；本任务只对构建产物大小作性能结论。

结果（2026-10-02）：HLS.js light build 保留 Lux 自有服务端 fMP4 单视频/单音轨播放所需 API，并排除播放器未使用的 HLS 字幕、备用音轨及 DRM 等功能。播放器测试先验证非原生 HLS 分支，再改为 light build；原生 HLS 路径仍不加载 HLS.js。播放器定向测试 3 项通过，完整 Web 测试 76 个文件 / 548 项通过，冻结安装与生产构建通过。HLS chunk 从 594.13 kB / gzip 185.60 kB 降至 371.83 kB / gzip 117.93 kB；gzip 传输字节减少 67.67 kB（约 36.5%），入口 chunk 保持 113.77 kB / gzip 30.20 kB，构建不再产生大 chunk 警告。浏览器 LCP、MSE 实际首帧和播放启动时延未测量，不能据此声称这些时延已改善。

#### LUX-333 清理播放器中的未使用导入

范围：移除 TypeScript 未使用符号诊断确认的播放器死导入，不改字幕/弹幕解析、Matroska 转码或 HLS 播放逻辑。HLS 引擎保留 `PlayerPage` 中的动态导入，只删除未使用的静态导入。

验收：

- [x] 四个未使用导入从对应模块删除，运行时路径与公开行为不变。
- [x] 冻结依赖安装、完整 Web 测试与生产构建通过。
- [x] 单独的 TypeScript 未使用符号诊断确认这四处不再报告。

预计文件：`web/src/features/player/PlayerPage.tsx`、`web/src/features/player/components/player-caption-overlay.tsx`、`web/src/features/player/components/player-danmaku-overlay.tsx`、`web/src/features/player/mkv-transcode-worker.ts`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-10-02）：移除 4 个未使用导入；`PlayerPage` 的 HLS 引擎动态导入保持不变。TypeScript 未使用符号诊断从 7 处降至 3 处；剩余的是迁移报告组件的未使用 `jobId` 参数、HEVC 引擎未读取的 `streamTask` 字段和 API client 中未使用的 `Library` 类型导入，将分别审查，不纳入本任务。`pnpm --dir web install --frozen-lockfile`、完整 Web 测试（76 个文件 / 548 项）和生产构建通过；本任务没有性能收益声明。

#### LUX-334 清理未使用的报告参数和 HEVC 任务字段

范围：删除未被读取的迁移报告组件 `jobId` prop 及其唯一调用处的传参，删除 API client 未使用的 `Library` 类型导入，并删除 HEVC 播放引擎从未读取的 `streamTask` 字段。媒体流消费仍由 detached `consumeSource` 异步任务继续执行；取消仍由 AbortController 与 generation 检查处理。

验收：

- [x] 报告组件与唯一调用方移除未读取的 `jobId` prop，报告查询继续由 hook 的 job ID query key 隔离。
- [x] HEVC `consumeSource` 错误处理与取消/销毁行为保持，移除无读取者的 Promise 保留字段。
- [x] 未使用符号诊断不再报告这些声明；冻结依赖安装、完整 Web 测试与生产构建通过。

预计文件：`web/src/features/admin/EmbyMigrationReports.tsx`、`web/src/features/admin/EmbyMigrationPluginConfig.tsx`、`web/src/lib/api/client.ts`、`web/src/features/player/hevc-playback-engine.ts`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-10-02）：报告组件与唯一调用方删除未使用的 `jobId` prop；API client 删除未使用的 `Library` 类型；HEVC 播放引擎删除未被读取的 `streamTask` 字段，并以 `void` 明确保留后台 `consumeSource` 执行。取消、generation 隔离、错误上报及 MSE 结束逻辑保持不变。`pnpm --dir web exec tsc --noEmit --noUnusedLocals --noUnusedParameters`、冻结依赖安装、完整 Web 测试（76 个文件 / 548 项）和生产构建通过。构建输出中 HEVC chunk 为 197.74 kB / gzip 49.68 kB；不据微小 bundle 差异推断播放性能提升。

#### LUX-335 将 TypeScript 未使用符号检查加入 Web 构建

范围：在 Web TypeScript 项目配置中启用 `noUnusedLocals` 与 `noUnusedParameters`。检查范围沿用现有 `tsconfig.json` 的 `src` 和 `vite.config.ts`，不扩展到独立测试文件，也不改变运行时代码。

验收：

- [x] 当前生产源码与 Vite 配置通过两项未使用符号检查。
- [x] `pnpm --dir web build` 将自动执行检查；完整 Web 测试和构建通过。

预计文件：`web/tsconfig.json`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-10-02）：`noUnusedLocals` 与 `noUnusedParameters` 已在生产 TypeScript 项目中启用；冻结依赖安装、`pnpm --dir web build`、Node 样式检查（108 项）和 Vitest（76 个文件 / 548 项）通过。首次完整测试有 5 项首页异步轮播断言失败；隔离重跑 `home-page.test.tsx`（15 项）及随后完整测试均通过，未复现，未改动测试或运行时代码。构建产物中的 HLS chunk 为 371.83 kB / gzip 117.93 kB；本任务没有运行时性能变更，不据 bundle 清理推断实际播放性能提升。

#### LUX-336 批量读取首页媒体库最新资源

范围：新增 `GET /api/v1/home/libraries/latest`，以重复的 `libraryId` 查询参数一次读取多库首页最新资源。每个请求最多接受 100 个库 ID，每库仍最多返回 12 项；输入需为有效且当前用户可访问的媒体库，重复 ID 去重并保留首次出现顺序。返回 `{ "libraries": [{ "libraryId": "...", "items": [...] }] }`，空库也返回空 `items`。一次批量完成 catalog 查询和用户状态序列化；保留既有单库 `/api/v1/libraries/{libraryId}/latest` 合同。

验收：

- [x] 空、无效或超过 100 个 ID 返回 400；任一 ID 不可访问时返回 403，不泄露其他库资源。
- [x] 可访问的多个媒体库一次返回，各库顺序、最新 12 项、图片标记、用户状态和元数据待处理标记与单库接口一致；重复 ID 只返回一个分组。
- [x] API 与接口文档覆盖查询参数和响应结构；定向 `catalog` 集成测试通过。

预计文件：`src/api/lux_api.rs`、`src/api/media.rs`、`tests/catalog.rs`、`docs/API.md`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-10-02）：新增有界多库首页最新资源 API；100 个 ID 上限低于现有 SQLite/PostgreSQL 批量查询分块上限，catalog 读取不随库数逐项发 SQL。序列化时用户状态、候选待处理和本地待处理标记按 ID 批量读取。集成用例覆盖 query 参数边界、去重和首次顺序、空库、跨库拒绝访问，并断言批量返回的媒体对象与原单库接口逐项相等。`cargo test --locked --test catalog`（3 项）、`cargo build --locked`、全目标测试（651 个 library tests 通过、10 个忽略；PostgreSQL 专项因本机无 PostgreSQL 实例而忽略）、`cargo fmt --all -- --check`、全目标 Clippy、shell/Python 检查、冻结 Web 依赖安装、Web 测试（108 项 Node 检查、76 个文件 / 548 项 Vitest）和 Web 生产构建通过。首次总检查脚本仅因新增代码格式差异在 fmt 阶段停止；格式化后，fmt 与其余门禁逐项复跑通过。本机架构 `arm64`。本任务只新增后端批量入口，Web 仍调用旧的逐库接口，因此目前不声称首页请求或耗时已下降；Web 接入和固定多库 fixture 的请求数/延迟对比仍待后续任务。

#### LUX-337：增加首页多库最新资源的 Web API 客户端方法

范围：为 LUX-336 的 `GET /api/v1/home/libraries/latest` 定义 TypeScript 响应类型，并在 `LuxApiClient` 增加接受媒体库 ID 列表的方法。客户端用重复的 `libraryId` 查询参数保留输入顺序，并沿用首页请求超时和取消信号。保留既有 `homeLibraryLatest` 方法与路径；本任务不接入首页组件。

验收：

- [x] TypeScript 响应结构表达每库 ID 与资源数组。
- [x] API 客户端为每个 ID 添加一个重复 query 参数、正确解码响应，并传递取消信号。
- [x] 首页请求 15 秒超时测试覆盖新方法；Web API 客户端定向测试通过。

预计文件：`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-10-02）：新增 `HomeLatestLibrariesResponse` 与 `homeLibrariesLatest`，用 `URLSearchParams.append` 按输入顺序生成重复 `libraryId` 参数，并沿用首页 15 秒请求超时及调用方取消信号。新增测试覆盖批量响应解码、参数顺序、超时与主动取消；先确认旧客户端因方法不存在而失败，随后实现后定向 API client 测试 57 项通过。冻结依赖安装、Web 全量测试（Node 108 项、Vitest 76 个文件 / 550 项）和生产构建通过。本任务只增加客户端能力，首页尚未调用该方法，不据此声称运行时请求数或性能变化。

#### LUX-338：首页使用多库最新资源批量查询

范围：将首页每库一个 `homeLibraryLatest` 查询改为一个 `homeLibrariesLatest` 查询，传入当前可见媒体库 ID 并按输入顺序将结果映射到各媒体库 shelf。保持媒体库展示顺序、每库独立卡片、15 秒刷新与 `home` SSE 失效语义；空媒体库不发批量请求，最新资源错误不得阻塞轮播、媒体库入口或继续观看。查询 key 需包含有序媒体库 ID，以便库范围或顺序变化时刷新正确。兼容单库查询方法继续保留。

验收：

- [x] 有多个媒体库时每次只调用一次 `homeLibrariesLatest`，参数按媒体库顺序传入；不再逐库调用 `homeLibraryLatest`。
- [x] 返回的最新资源分别显示在正确 shelf；空库列表不发请求，库的显示顺序保持不变。
- [x] 首页事件刷新批量查询，15 秒轮询行为保留；最新资源请求失败不影响其他首页区块。
- [x] 首页与 LuxShell 事件刷新回归通过，Web 全量测试和构建通过。

预计文件：`web/src/features/home/HomePage.tsx`、`web/src/lib/api/query-keys.ts`、`web/tests/home-page.test.tsx`、`web/tests/lux-shell.test.tsx`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-10-02）：首页以一次有序多库批量查询替代逐库请求，缓存键包含媒体库 ID 顺序；空库不请求，15 秒刷新和 `home` SSE 刷新保留，失败状态只显示在最新资源区块。定向首页与 LuxShell 测试 32 项通过，先确认旧实现下批量调用断言失败。检查并修正了缓存轮播测试仍 mock 旧单库 API 的遗漏。冻结依赖安装、Node 测试 108 项、标准并行 Vitest 全量 76 个文件 / 551 项及 TypeScript 检查、生产构建通过。两次早期并行全量运行曾有 5 个轮播用例因 1 秒等待超时，修正过期 mock 后标准全量通过。测试量化的是每次首页最新资源 HTTP 请求由每库一次降为一次，不代表实际延迟或 p95 已测量。

#### LUX-339：批量写入增量扫描路径

范围：`enqueue_incremental_changes` 目前对每条路径分别执行 upsert、扫描完整 `scan_job_paths` 计数并更新任务行。将多路径入队改为每批最多 100 条的多行 upsert，并在每批后只刷新一次 `total_count`，避免按路径重复扫描队列表和写任务行。批次大小需兼容 SQLite 与 PostgreSQL 参数上限；重复的 `(root_id, relative_path)` 以输入中的最后一个变更类型为准。单路径调用保留现有语义，可复用批量存储实现。

验收：

- [x] 205 条唯一路径相对原逐条实现将存储 SQL 调用从 410 降至 6；该计数是查询调用数，不声称墙钟耗时收益。
- [x] 批量中的重复路径最后一个变更类型生效，已处理路径重新入队后清空 `processed_at`，`total_count` 等于队列中去重路径数。
- [x] 同一批次沿用 SQLite/PostgreSQL 共用查询和占位符适配；单路径入队和多根目录刷新继续使用既有存储路径。
- [x] 定向存储/扫描作业测试、全局 Rust 完成门通过；性能记录包含固定批量规模、查询次数和本机架构。

依赖：LUX-288。预计文件：`src/application/scanner.rs`、`src/storage/jobs.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。不修改扫描 job 的进度合同、表结构、路径规范化或消费顺序。

结果（2026-10-02）：增量变更和多根刷新改为调用有界批量入队，单路径 API 继续委托同一存储操作。每 100 条唯一路径执行一条多行 upsert，并刷新一次 `total_count`；205 条路径相对基线 410 次 SQL 调用降为 6 次（减少约 98.5%）。回归验证重复路径最后的变更类型胜出、已处理路径被重新打开、任务计数与去重后的队列行数一致。SQLite 存储计数测试、`tests/scanning_jobs.rs` 81 项、`cargo build --locked`、`cargo test --locked --all-targets`（652 个库测试通过、10 个 PostgreSQL 专项忽略，其余集成目标通过）、`cargo fmt --all -- --check` 和全目标 Clippy 通过。性能记录见 `docs/PERFORMANCE.md`；本机 `uname -m=arm64`。PostgreSQL 专项因测试环境没有 PostgreSQL 运行实例而未执行，本次只报告共享 SQL 形状与占位符适配，不声称 PostgreSQL 实测或墙钟耗时收益。

#### LUX-340：合并扫描文件状态与 inode 写入

范围：扫描器更新已有文件时，先写入大小、修改时间、指纹和扫描代次，再单独更新 inode；未变化剧集回退路径也先标记已扫描、再单独更新 inode。这些连续写入使用同一份文件系统 metadata，增加额外 UPDATE 和事务。将 inode 并入已有 filesystem entry 更新和 mark-seen 语句，并在电影、剧集和 sidecar 扫描调用中传入当前可用 inode。identity-repair 的纯 inode 更新没有前序状态写入，不在本任务合并。

验收：

- [x] 已有条目扫描更新仍原子写入文件大小、修改时间、指纹、扫描代次、缺失状态与 inode，并保留媒体条目恢复逻辑。
- [x] 扫描状态更新与 inode 不再拆成连续两次 filesystem entry UPDATE；存储查询计数证明每条相关路径至少减少一次 SQL 调用。
- [x] 单测验证更新后的字段值和查询数；`scanner` 及相关扫描作业回归、全局 Rust 完成门、格式检查和 Clippy 通过。
- [x] 性能记录只报告 SQL 调用计数，不据此推断墙钟、PostgreSQL 或 NAS 时延。

依赖：LUX-339。预计文件：`src/application/scanner.rs`、`src/storage/jobs.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。不修改 schema、扫描进度合同、媒体身份或恢复事务边界。

结果（2026-10-02）：已有电影、剧集和 sidecar filesystem entry 更新现在在同一条状态 UPDATE 中写入 inode；指纹未变化路径也在 mark-seen UPDATE 中更新 inode。保留恢复缺失媒体条目的事务逻辑，identity-repair 的单独 inode 更新因没有前序状态写入而维持原样。SQLite 存储回归将有变化与 mark-seen 两种路径都从 3 条 SQL 调用降为 2 条，并核对状态字段。`scanner` 17 项、`scanning_jobs` 81 项通过；`cargo test --locked --all-targets` 的库测试为 653 passed / 10 ignored，集成目标全部通过；`cargo build --locked`、`cargo fmt --all -- --check` 和全目标 Clippy 通过。本机 `uname -m=arm64`。PostgreSQL 实例不可用，因此 PostgreSQL 专项按要求忽略；性能记录只报告 SQL 调用数，不推断墙钟或 NAS 收益。

#### LUX-341：合并轻量 Manifest 根状态读取

范围：轻量 Manifest discovery session 初始化先列出根 ID，再逐根查询 Manifest 状态；对尚未完成或不可用的根又逐根查询是否存在 filesystem entry。将根 ID、Manifest 状态和是否已有 filesystem entry 合并到一次按 Manifest 有界读取中。保留根排序、跳过 `COMPLETE` / `UNAVAILABLE` 根及 `skip_baseline_queries` 判断；不读取或改变后续使用的根设备/inode 身份合同。

验收：

- [x] 多根初始化由 `1 + 根数 + 可处理根数` 条 SQL 调用降为 1 条，不随根数量增长。
- [x] 自动化测试通过真实轻量 Manifest session 初始化，验证 `COMPLETE` / `UNAVAILABLE` 根仍跳过，以及活跃根已有文件的 baseline 标志不变。
- [x] `scanner`、`scanning_jobs` 回归、全局 Rust 完成门、格式检查和 Clippy 通过。
- [x] 性能记录报告固定 fixture 与 SQL 调用数，不据此推断墙钟、PostgreSQL 或 NAS 时延。

依赖：LUX-340。预计文件：`src/application/scanner.rs`、`src/storage/jobs.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。不修改数据库 schema、持久化 workflow、根身份验证或扫描语义。

结果（2026-10-02）：轻量 Manifest session 现在通过一条有界查询读取有序根 ID、Manifest 状态和 filesystem entry baseline；仍跳过 `COMPLETE` / `UNAVAILABLE` 根，且不执行根设备/inode 身份读取。四根 SQLite fixture 含两个终态根、两个待处理根；旧路径为 1 次根列表查询、4 次状态读取和 2 次 baseline 查询，共 7 次 SQL 调用，新路径为 1 次。新测试通过真实服务初始化验证状态筛选、baseline 标志和查询数。scanner 模块 31 项、`tests/scanner.rs` 17 项、`tests/scanning_jobs.rs` 81 项及 `cargo test --locked --all-targets` 全部通过；PostgreSQL 专项因环境无实例而忽略。`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check` 和全目标 Clippy 通过；本机 `uname -m=arm64`。性能记录仅报告该固定 fixture 的 SQL 调用数，不推断墙钟、PostgreSQL 或 NAS 收益。

#### LUX-342：批量注册本地元数据回填根

范围：本地元数据 worker 启动时，先读取所有 library root，再逐根执行幂等 INSERT；多根库因此反复发 SQL 并多次获取 SQLite 写锁。将启动注册改为一条 `INSERT ... SELECT ... ON CONFLICT DO NOTHING`，只获取一次写锁，并用该语句的 affected-row 数返回新增根数。保留逐根注册 API、已注册根不变和新根注册语义。

验收：

- [x] 空库根列表、四根首次注册和重复注册均返回正确新增数；每次批量注册固定为 1 条 SQL 调用。
- [x] SQLite 自动化测试覆盖持久化队列行、幂等性和查询调用数；SQL 形状使用 SQLite 与 PostgreSQL 均支持的语法。
- [x] 存储定向回归、全局 Rust 完成门、格式检查和 Clippy 通过。
- [x] 性能记录报告固定根数与 SQL 调用数，不据此推断墙钟、PostgreSQL 或 NAS 时延。

依赖：LUX-341。预计文件：`src/storage/jobs.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。不修改 schema、回填资格、单根注册语义或队列消费顺序。

结果（2026-10-02）：4 根 SQLite fixture 的本地元数据回填根注册从原先 1 次根列表读取加 4 次逐根 INSERT，改为单条 `INSERT ... SELECT ... ON CONFLICT DO NOTHING`；新根返回 4，空列表和重复注册返回 0，三种情况下每次均为 1 次 SQL 调用，且 4 条队列记录实际持久化。定向 storage 测试通过；`cargo test --locked --all-targets` 全目标共 1,287 passed、0 failed、31 ignored；`cargo build --locked`、fmt、`git diff --check` 和全目标/全 feature Clippy 通过。本机 `uname -m=arm64`。性能记录见 `docs/PERFORMANCE.md`，只报告 SQL 调用数；PostgreSQL 未连接实测。

#### LUX-343：批量写入 Webhook 投递队列

范围：Webhook 事件入队目前对每个 destination 单独执行 delivery INSERT。将 delivery 行改为每批最多 100 个 destination 的多行 INSERT，降低 SQL 调用与语句执行开销，同时保留事件先入队、事务原子性、dedupe key、delivery 唯一键冲突忽略、投递 ID 唯一性和目标顺序。事件插入冲突时仍不创建 delivery；空目标列表仍只插入事件。

验收：

- [x] 205 个 destination 在 SQLite 中保留 205 条 PENDING delivery，入队 SQL 调用固定为 1 条事件 INSERT 加 3 条批量 delivery INSERT；重放同一 dedupe key 不产生重复记录。
- [x] 每批绑定参数不超过 SQLite 保守限制，SQL 使用 SQLite/PostgreSQL 均支持的多行 VALUES 与 `ON CONFLICT` 语法；不修改 schema 或 delivery 消费顺序。
- [x] 定向存储/Webhook 回归、全局 Rust 完成门、格式检查和 Clippy 通过。
- [x] 性能记录比较固定 fixture 的 SQL 调用数，不据此推断墙钟、PostgreSQL 或 NAS 时延。

依赖：LUX-342。预计文件：`src/storage/notifications.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-10-02）：delivery 行改为每批最多 100 条的多行 INSERT，每行绑定 3 个参数，最多 300 个绑定值。205 个目标从 206 次 SQL 调用降到 4 次（事件 1 次、delivery 3 批，减少约 98.1%），仍持久化 205 条 PENDING 行。SQLite 回归验证 dedupe 重放、空目标事件和第二批失败时整笔事务回滚；PostgreSQL 17 临时实例上的同一批量 fixture 验证 205 行、dedupe 和 4/1 次查询计数。定向 Webhook 与扫描集成回归通过；`cargo test --locked --all-targets` 为 1,288 passed、0 failed、32 ignored；`cargo build --locked`、fmt 和全目标/全 feature Clippy 通过。本机 `uname -m=arm64`；性能记录只报告 SQL 调用数，不推断墙钟、NAS 或 x86_64 时延。全量验收期间出现过一次无法复现的 library update 503，因此该测试现会在失败断言中包含响应体以便后续诊断。

#### LUX-344：移除播放器回调与调度器的双重类型断言

范围：HEVC 播放器把 MP4Box `onReady` 提供的 `Movie.tracks` 通过 `as unknown as` 转为局部轨道结构，忽略了库已有类型；时间线调度器把 `requestAnimationFrame` 的 number 与 `setTimeout` 的返回句柄强行统一为 number，并按全局 API 是否存在而非实际句柄来源取消。改为从 `createFile().onReady` 推导轨道类型，并用区分动画帧/定时器的句柄类型调用匹配的取消 API。保留时间线合并、节流、立即刷新和 HEVC 轨道处理逻辑。

验收：

- [x] MP4Box 轨道形状从 `createFile` 的回调类型推导，播放器实现不再双重断言 `onReady` 数据。
- [x] 时间线句柄保留创建来源，取消动画帧时使用 `cancelAnimationFrame`，取消计时器时使用 `clearTimeout`；节流和刷新语义不变。
- [x] 回归测试验证计时器回退会用 `clearTimeout` 清理，即使 `cancelAnimationFrame` 可用；播放器 helper 与时间线目标、TypeScript 构建通过。
- [x] Web 冻结依赖安装、全量测试和生产构建通过；不据类型清理声称运行时性能提升。

依赖：LUX-335。预计文件：`web/src/features/player/hevc-playback-engine.ts`、`web/src/features/player/player-timeline-scheduler.ts`、`web/tests/player-timeline-scheduler.test.ts`、`docs/LUX-DEVELOPMENT.md`。先增加定时器句柄清理回归并运行定向 Vitest，再修改类型与取消路径。

结果（2026-10-02）：先新增“无 requestAnimationFrame 但有 cancelAnimationFrame”回归，旧实现失败，因为 timeout handle 被交给错误的取消 API；实现改为带 `kind` 的 animation/timeout 句柄，分别走对应清理函数。HEVC 轨道类型现由 `createFile().onReady` 推导，移除其 `unknown` 双重断言；播放器源码中已无 `as unknown as`。时间线与 HEVC 定向测试 10/10、严格 TypeScript 检查通过；冻结安装、完整 Web 测试（Node 108/108，Vitest 76 个文件 / 553 项通过）和生产构建通过。本任务修正了取消 API 选择，不量化或声称播放时延提升。

#### LUX-345：清理 build 与 API handler 的 Clippy 条件告警

范围：目前全局 `clippy::collapsible_if = "allow"` 隐藏了 `build.rs` 以及部分 API handler 中可按现有 Rust let-chain 风格表达的嵌套条件。本任务处理 `build.rs`、全局媒体策略验证、用户配置复制、Emby DTO 序列化这 9 处告警；其余应用层告警和 Cargo 全局规则留给后续独立任务。仅重排等价条件，不改变请求验证顺序、错误响应、用户配置复制或 DTO 字段。

验收：

- [x] `build.rs`、`admin_handlers.rs`、`emby_handlers.rs`、`emby_catalog.rs` 中这 9 处 `collapsible_if` 告警消除，`.git` 信息读取、校验失败响应、用户配置复制和 Emby 序列化结果不变。
- [x] `libraries_api`、`emby_auth`、`catalog` 定向集成回归、build、fmt 通过；Clippy 诊断不再在本任务修改文件报告 `collapsible_if`。
- [x] 只改本任务列出的 Rust 文件与本节文档，不声称有运行时性能提升。

依赖：无。预计文件：`build.rs`、`src/api/admin_handlers.rs`、`src/api/emby_handlers.rs`、`src/api/emby_catalog.rs`、`docs/LUX-DEVELOPMENT.md`。本任务范围为 4 个代码文件与 1 个文档文件；其余 40 余项 Clippy 告警与 Cargo 级豁免分开处理。

结果（2026-10-02）：将 build 脚本、全局/媒体库策略校验、用户 Emby 配置复制、Emby 人员/播放状态/同步字段/主图比例序列化中的 9 处条件按 let-chain 等价合并。带 `-W clippy::collapsible_if` 的全目标诊断确认这 4 个源码文件及 `build.rs` 不再产生该告警；其他模块仍有 35 处待后续任务处理。`cargo build --locked`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 及 `libraries_api`、`emby_auth`、`catalog`、`emby_counts` 集成目标（25 项）通过。本任务为条件表达式清理，不声称运行时性能提升。

#### LUX-346：批量写入章节检测任务条目

范围：章节检测任务按最多 500 个候选源分页，但存储层仍对每个条目单独执行 INSERT。将每批最多 100 个条目合并为一条多行 INSERT，减少大剧集库创建任务时的 SQL 执行次数；保留现有事务原子性、条目字段、PENDING 状态、页面顺序和空输入行为。每行绑定 7 个参数，100 行最多 700 个绑定值。

验收：

- [x] SQLite 存储回归验证 205 个条目持久化完整、状态/指纹/context 标记正确，SQL 调用从 205 次降至 3 次。
- [x] 空输入不发 SQL；整页仍在单个事务内，任一批失败时整页回滚。
- [x] 多行 VALUES 使用 SQLite/PostgreSQL 通用语法；不改 schema、任务分页或消费顺序。
- [x] 性能记录仅报告固定条数的 SQL 调用数，不推断端到端时延、PostgreSQL 或 NAS 收益。

依赖：无。预计文件：`src/storage/catalog.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加 SQLite 查询计数和持久化回归，运行定向 storage 测试确认旧实现失败，再实现批量 INSERT。

结果（2026-10-02）：205 个候选条目从逐项 205 次 INSERT 改为最多 100 条一批的多行 INSERT，共 3 次 SQL 调用（约减少 98.5%）。回归在旧实现上先观察到 205 次并失败，再验证新实现的行数、PENDING 状态、source/input fingerprint 与 context 标记；空输入 0 次 SQL，第三批冲突会回滚整页。新语句每批最多 700 个绑定值。定向 storage 测试通过；`cargo build --locked`、`cargo test --locked --all-targets`、fmt、全目标/全 feature Clippy、脚本语法与 Python 工具测试通过；冻结 Web 安装、553 项 Web 测试和生产构建通过。本机 `uname -m=arm64`。性能记录只报告 SQLite 固定 fixture 的查询调用数；本任务未实测 PostgreSQL、墙钟时延或 NAS。

#### LUX-347：批量替换媒体探测音视频轨道

范围：每个媒体源的探测结果写入时，存储层先更新 media source、删除旧轨道，再逐条 INSERT 新轨道。将新轨道 INSERT 改为每批最多 75 条的多行语句；每行 12 个参数，最多 900 个绑定值。保留原子替换、输入顺序、轨道字段、旧轨道删除和空轨道行为，不更改探测调度或 schema。

验收：

- [x] SQLite 回归以 205 条轨道验证替换后字段、轨道数与顺序正确，SQL 调用从 207 次降至 5 次。
- [x] 空轨道仍删除旧轨道且不执行 INSERT；第三批重复索引导致整笔更新回滚，已有 source 与轨道记录不变。
- [x] SQL 使用 SQLite/PostgreSQL 通用多行 VALUES；不改探测状态机或流 DTO。
- [x] 性能记录报告固定 fixture 的 SQL 调用数，不推断端到端耗时、PostgreSQL 或 NAS 收益。

依赖：无。预计文件：`src/storage/catalog.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加查询计数、替换和回滚测试，确认逐条写入的基准，再实现有界批量 INSERT。

结果（2026-10-02）：205 条探测轨道从逐项 205 次 INSERT 改为最多 75 条一批的多行 INSERT，共 3 次批量 INSERT；连同 source UPDATE 和旧轨道 DELETE，SQL 调用从 207 次降至 5 次（约减少 97.6%）。回归验证了轨道字段、索引顺序、字幕外部路径、空轨道清理和第三批约束失败时的整笔回滚；新语句每批最多 900 个绑定值。`probe`、`strm_probe` 定向目标、全目标 Rust 测试、fmt、全目标/全 feature Clippy、脚本语法、Python 工具测试和 Web 553 项测试/生产构建通过。本机 `uname -m=arm64`。性能记录只报告 SQLite 固定 fixture 的查询调用数；本任务未实测 PostgreSQL、墙钟时延或 NAS。

#### LUX-348：章节检测复用任务级插件模式

范围：章节检测任务启动时已经读取一次插件 catalog，得到 `remote_lookup`；当前每个本地/远程分集 RPC 批次的 `process_season` 又重复读取 catalog snapshot。将任务级模式传入批次处理，消除重复读取，保留本地指纹检测、远程章节查询和媒体源筛选行为，不改变插件协议、任务状态机或数据库模型。

验收：

- [x] 每个任务只读取一次 `remote_lookup`，分集批次不再重复读取 catalog snapshot。
- [x] 本地检测与远程 lookup 的现有回归目标继续通过，分支选择和取消/重试语义不变。
- [x] 不新增数据库、插件协议或公共 API 变化。
- [x] 性能记录只报告可推导的重复读取上界，不冒充墙钟或数据库性能基准。

依赖：无。预计文件：`src/application/chapter_detector.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先复用现有任务级模式并运行章节检测相关目标，再记录固定批大小下的重复 snapshot 上界。

结果（2026-10-02）：`run_claimed` 计算出的任务级 `remote_lookup` 现在传入每个分集批次，`process_season` 不再重复读取 plugin catalog。章节检测 2 项、本地/远程媒体源筛选和章节检测 API 相关回归共 3 项通过；本机 `uname -m=arm64`。固定单季 10,000 集推导为本地批次最多从 157 次重复读取降至 0 次、远程批次最多从 417 次降至 0 次，任务级读取保持 1 次；该记录不代表墙钟或数据库性能收益。

#### LUX-349：批量变更 Emby 手动合集成员

范围：Emby 合集新增接口最多接受 1,000 个条目 ID，但存储层仍逐项执行成员 `INSERT ... SELECT`；删除接口也逐项执行成员 DELETE。将新增按最多 100 行合并，将删除按最多 500 个 ID 合并，保留媒体库归属校验、已移除条目过滤、输入顺序产生的 `sort_order`、重复成员幂等和事务边界，不改变 Emby API 合同。

验收：

- [x] 205 个成员新增从 208 次 SQL 调用降至 5 次或更少，成员数量和顺序正确。
- [x] 205 个成员删除从逐项调用降至 2 次或更少，删除后合集为空；空输入不执行成员写入。
- [x] 跨库/已移除/重复 ID 的现有过滤和冲突行为保持不变。
- [x] 性能记录只报告固定 fixture 的 SQL 调用数，不推断端到端时延或生产数据库收益。

依赖：无。预计文件：`src/storage/media.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加 205 ID 的 storage 查询计数回归，确认旧逐项路径的调用数，再实现有界批量语句。

结果（2026-10-02）：205 个有效成员新增由旧实现的 208 次调用（合集读取、collection id/max sort 两次预读和 205 次逐项 INSERT）降为 5 次（合集读取、合并后的 collection id/max sort 读取和 3 条 100/100/5 多行 INSERT）；成员数量与顺序保持不变。205 个成员删除由 206 次调用降为 2 次（合集读取和一条 205-ID DELETE）。新增仍过滤跨库/已移除条目，重复 ID 仍由唯一约束幂等处理；删除空输入不发成员 DELETE。storage 回归通过；本任务未实测 PostgreSQL 墙钟、NAS 或生产负载。

#### LUX-350：批量领取本地元数据完整性检查

范围：本地扫描每个完整性批次最多提交 512 个 item/capability 检查，但存储层当前对每条检查分别执行 upsert 和 claim UPDATE。将检查按最多 100 条合并为多行 upsert 和批量 claim，保留重复校验、输入指纹替换、失败重试、并发 worker 只能领取一次和返回原始索引的语义，不改变 schema 或扫描调度。

验收：

- [x] 205 条完整性检查从 410 次 SQL 调用降至 6 次，205 个原始索引全部返回。
- [x] 同指纹 READY/RUNNING 行不重复领取，失败/CANCELLED 和新指纹仍可重新领取。
- [x] 并发 worker 不会重复领取同一个检查，重复输入/空指纹/超限批次仍拒绝。
- [x] 性能记录只报告固定 fixture 的 SQL 调用数，不推断墙钟或生产数据库收益。

依赖：无。预计文件：`src/storage/metadata.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加 205 条 storage 查询计数回归，确认旧实现调用数，再实现有界多行语句。

结果（2026-10-02）：205 条完整性检查由旧实现每条一次 upsert 加一次 claim、共 410 次 SQL 调用，降为 3 条多行 upsert 与 3 条批量 claim、共 6 次（约减少 98.5%）。批量 claim 通过 `RETURNING item_id, capability` 映射回原始索引；现有版本替换、READY/RUNNING 重复领取、失败重试和并发互斥回归通过。每批最多 100 条、每条语句最多 300 个绑定值；本机 `uname -m=arm64`。性能记录只报告 SQLite SQL 调用数，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-351：批量提交本地元数据完整性结果

范围：本地元数据完整性结果在同一事务中逐条 UPDATE `item_metadata_completeness`，批次最多 512 条。将结果按最多 100 条合并为多行更新并通过 `RETURNING` 收集成功项，保留库归属校验、输入指纹匹配、RUNNING 状态门槛、缺失集合和补缺任务的原子边界，不改变 schema 或扫描调度。

验收：

- [x] 205 条结果从逐条 UPDATE 降至 3 次或更少，updated_count、缺失集合和 READY 字段正确。
- [x] 陈旧指纹、非 RUNNING 状态、已移除/跨库条目仍不会被错误更新。
- [x] 批量结果失败时完整性行和后续补缺任务仍整体回滚。
- [x] 性能记录只报告固定 fixture 的 SQL 调用数，不推断墙钟或生产数据库收益。

依赖：无。预计文件：`src/storage/metadata.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加 205 条结果的 storage 查询计数回归，确认旧逐条 UPDATE 调用数，再实现有界批量更新。

结果（2026-10-02）：205 条 RUNNING 完整性结果由旧实现的 205 次逐条 UPDATE 加 1 次库校验（206 次 SQL）降为 3 条批量 UPDATE 加 1 次库校验（4 次 SQL），`updated_count`、READY 状态和缺失数量保持正确。批量 `RETURNING` 只收集实际更新的 item；既有陈旧指纹、非 RUNNING、跨库/移除条目、事务回滚和补缺策略回归通过。每批最多 100 条、最多 501 个绑定值；本机 `uname -m=arm64`。性能记录只报告 SQLite SQL 调用数，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-352：批量替换用户媒体库排序

范围：用户媒体库排序接口最多接受 1,024 个库 ID，但存储层删除旧排序后仍逐条 INSERT 新位置。将新排序按最多 100 行合并写入，保留请求顺序、位置唯一约束、空排序和整笔事务边界，不改变用户/API 合同。

验收：

- [x] 205 个媒体库排序从 206 次 SQL 调用降至 4 次，读取顺序和 position 完整正确。
- [x] 空排序只删除旧行，不执行 INSERT；重复位置/数据库约束失败时整笔事务回滚。
- [x] 位置超过可表示范围仍返回原有序列化错误。
- [x] 性能记录只报告固定 fixture 的 SQL 调用数，不推断墙钟或生产数据库收益。

依赖：无。预计文件：`src/storage/users.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加 205 个库 ID 的 storage 查询计数回归，确认旧逐条路径调用数，再实现有界多行 INSERT。

结果（2026-10-02）：205 个媒体库排序由旧实现删除加 205 次逐条 INSERT、共 206 次 SQL 调用，降为删除加 100/100/5 三批多行 INSERT、共 4 次（约减少 98.1%）。回归验证读取顺序、position、空排序和第三批重复库 ID 约束失败时保留旧排序；本机 `uname -m=arm64`。性能记录只报告 SQLite SQL 调用数，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-353：批量写入计划媒体库关联

范围：计划迁移路径已经批量更新计划配置和删除旧关联，但向 `scheduled_task_plan_libraries` 写入当前媒体库集合时仍逐库 INSERT。将关联写入按最多 100 行合并，保留重复关联幂等、计划事务边界和媒体库顺序无关语义，不改变计划/API 合同。

验收：

- [x] 205 个媒体库关联写入从 207 次 SQL 调用降至 5 次。
- [x] 关联数量正确，重复关联仍幂等；任一批失败时计划配置和关联整体回滚。
- [x] 计划读取、调度和删除自定义计划的现有回归继续通过。
- [x] 性能记录只报告固定 fixture 的 SQL 调用数，不推断墙钟或生产数据库收益。

依赖：无。预计文件：`src/storage/library.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加计划关联批量写入计数回归，确认旧逐库路径调用数，再实现有界多行 INSERT。

结果（2026-10-02）：205 个媒体库关联由删除/更新配置加 205 次逐库 INSERT、共 207 次 SQL 调用，降为删除、更新配置和 100/100/5 三批多行 INSERT、共 5 次（约减少 97.6%）。回归验证关联数量、计划读取、计划调度、计划删除和已有计划镜像行为；本机 `uname -m=arm64`。性能记录只报告 SQLite SQL 调用数，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-354：批量读取计划媒体库任务配置

范围：创建或更新媒体库执行计划前，存储层当前按输入媒体库逐个读取 `scheduled_task_configs`，大计划会产生与媒体库数量成比例的重复查询。将任务配置校验改为每批最多 100 个媒体库 ID 的 `IN` 查询，再按输入顺序检查缺失配置和 source/plugin 是否一致，保持重复 ID、错误顺序、事务边界和计划/API 合同不变。

验收：

- [x] 205 个媒体库任务配置校验从 205 次读取降至 3 次有界批量读取。
- [x] 缺失配置、重复媒体库和混合插件来源仍返回原有错误，空媒体库列表仍被拒绝。
- [x] 创建、更新、计划迁移和现有计划镜像回归继续通过；不改变 schema 或公共 API。
- [x] 性能记录只报告固定 fixture 的 SQL 调用数，不推断墙钟或生产数据库收益。

依赖：无。预计文件：`src/storage/library.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加 205 个配置的查询计数回归，确认逐库读取基线，再实现有界批量读取。

结果（2026-10-02）：205 个媒体库任务配置读取由旧实现的 205 次逐库 SELECT 降为 100/100/5 三批查询，共 3 次（约减少 98.5%）。批量结果按输入顺序回填并保留缺失配置、重复 ID、source/plugin 不匹配和空输入的错误语义；相关计划存储回归通过。本机 `uname -m=arm64`。性能记录只报告 SQLite SQL 调用数，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-355：批量迁移计划移出的媒体库关联

范围：更新自定义媒体库计划时，移出当前计划的媒体库已经批量更新任务配置，但关联表仍对每个媒体库分别 DELETE 和 INSERT 到默认计划。将关联迁移改为每批最多 500 个 ID 的 `INSERT ... SELECT` 与 DELETE，保持默认计划选择、任务配置镜像、重复关联幂等、事务边界和计划/API 合同不变。

验收：

- [x] 705 个媒体库从自定义计划移回默认计划时，完整更新路径的 SQL 调用从 1,420 次降至 16 次。
- [x] 自定义计划保留 1 个关联，默认计划接收其余 704 个关联，任务配置镜像与关联数量一致。
- [x] 关联迁移使用 500 个 ID 的有界批次，不改变计划更新、默认计划保护和现有删除计划语义。
- [x] 性能记录只报告固定 fixture 的 SQLite SQL 调用数，不推断墙钟或生产数据库收益。

依赖：LUX-353。预计文件：`src/storage/library.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加 705 个媒体库迁移查询计数回归，确认旧逐库 DELETE/INSERT 基线，再实现有界关联迁移。

结果（2026-10-02）：705 个媒体库从自定义计划移出 704 个时，旧路径完整更新发出 1,420 次 storage SQL 调用；新路径以 500/204 两批复制并删除关联，共 16 次，减少 1,404 次（约 98.9%）。回归验证自定义计划保留 1 个关联、默认计划获得 704 个关联以及 704 条任务配置镜像；本机 `uname -m=arm64`。性能记录只报告 SQLite SQL 调用数，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-356：批量读取手动合并媒体条目根记录

范围：手动合并接口最多接受 100 个媒体条目，但存储层仍对每个根条目分别读取启用媒体库、条目类型和合并状态，电影合并也会对每个源条目分别写入媒体源、用户状态和隐藏标记。将根条目校验和电影合并写入改为有界批量 SQL，再按请求顺序恢复根记录，保持缺失条目错误、主条目选择、同库/同类型校验、媒体源默认优先级、用户状态合并和事务边界，不改变合并层级或公共 API。

验收：

- [x] 100 个电影根条目的根记录读取从 100 次降为 1 次；批量写入接入后完整合并从 596 次 SQL 调用降至 7 次。
- [x] 返回的合并条目顺序、主条目语义、启用库过滤、缺失条目错误和既有电影/剧集合并回归保持不变。
- [x] 根读取、电影媒体源迁移、用户状态迁移和隐藏标记更新均按有界 ID 集合执行，使用 SQLite/PostgreSQL 通用参数化查询，不改变 schema 或合并事务边界。
- [x] 性能记录只报告固定 SQLite fixture 的 SQL 调用数，不推断端到端时延、PostgreSQL 或 NAS 收益。

依赖：LUX-251。预计文件：`src/storage/media_merge.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加 100 个根条目的查询计数回归，确认逐条读取基线，再实现有界批量读取。

结果（2026-10-03）：100 个电影根条目的根读取由 100 次逐条 `SELECT` 降为 1 次有界 `IN` 查询；电影媒体源迁移、用户状态合并/清理和根条目标记分别改为批量 SQL，完整合并调用由 596 次降至 7 次，减少 589 次（约 98.8%）。根记录按请求顺序回填，主条目、媒体源默认优先级、用户状态最大值合并、合并顺序和启用媒体库过滤保持不变；剧集合并继续沿用逐层状态与标记路径，并跳过只供电影路径使用的主条目默认源读取，现有分集批量读取回归由 20 次降为 19 次。media merge 存储和 item_merge 集成回归通过，本任务未实测 PostgreSQL 墙钟、NAS 或生产负载。

#### LUX-357：批量读取 STRM 探测任务的媒体库计数

范围：创建 STRM 探测任务时，服务当前对每个选中的媒体库分别读取完整媒体库、刮削器列表和 STRM 来源计数，最多 64 个媒体库会产生与选择数成比例的重复读取。将媒体库存在性和 STRM 来源计数改为每批最多 100 个 ID 的聚合查询，保持输入去重、媒体库不存在错误、任务顺序和每库任务记录语义不变。

验收：

- [x] 64 个媒体库创建 STRM 探测任务时，前置读取和计数从 322 次 SQL 调用降至 131 次。
- [x] 64 个任务记录仍全部创建，顺序、选项和零 STRM 数量保持不变；重复媒体库仍只创建一个任务。
- [x] 不存在的媒体库仍返回 `LibraryNotFound`；查询按最多 100 个 ID 分批，不改变 schema 或公共 API。
- [x] 性能记录只报告固定 fixture 的 SQLite SQL 调用数，不推断墙钟或生产数据库收益。

依赖：无。预计文件：`src/application/strm_probe.rs`、`src/storage/catalog.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加 64 个媒体库的查询计数回归，确认逐库读取基线，再实现有界聚合读取。

结果（2026-10-02）：64 个媒体库的 STRM 探测任务创建由旧实现的 322 次 storage SQL 调用降为 131 次，减少 191 次（约 59.3%）。新增聚合读取按最多 100 个媒体库 ID 分批，并保留不存在媒体库、重复输入、任务顺序和零来源计数语义；本机 `uname -m=arm64`。性能记录只报告 SQLite SQL 调用数，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-358：批量读取扫描本地元数据完整性预检

范围：本地元数据完整性 worker 当前对每个 item 分别读取元数据和媒体库归属，即使两者来自同一条媒体关系。新增最多 500 个 item ID 一批的 active 元数据与媒体库联合读取，并按输入 item ID 回填；保留禁用/移除条目跳过、完整性计划、图片/NFO/人物读取、重试状态、claim 和补缺调度行为，不改变 schema 或扫描任务状态机。

验收：

- [x] 205 个 active item 的元数据/媒体库预检从 410 次 SQL 调用降至 1 次有界查询。
- [x] active item 的媒体库归属和元数据字段保持一致；禁用库或已移除 item 不进入完整性检查。
- [x] 后续完整性计划、claim、结果提交、在线补缺和增量策略快照回归保持不变。
- [x] 性能记录只报告预检读取的 SQLite SQL 调用数，不推断完整 worker 墙钟、PostgreSQL 或 NAS 收益。

依赖：LUX-293、LUX-302、LUX-303。预计文件：`src/storage/media.rs`、`src/application/scanner.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加 active item 的逐项读取基线，再实现有界联合预读。

结果（2026-10-03）：新增 active 元数据与 `library_id` 联合预读，205 个 item 的旧元数据/归属逐项读取为 410 次 SQL，新路径按最多 500 个 ID 一批为 1 次，减少 409 次（约 99.8%）。scanner 完整性流程复用预读结果，后续计划和调度代码未改变；storage 计数回归和 scanner 17 项回归通过。本任务未实测 PostgreSQL 墙钟、NAS 或完整 worker 端到端时延。

#### LUX-359：批量读取元数据任务创建前的条目校验

范围：创建最多 100 个条目的元数据重识别任务时，服务当前逐条读取 item 类型并再次读取完整元数据。改为按最多 500 个 ID 批量读取已有元数据，再按请求去重后的顺序校验缺失条目和 VIDEO 类型，保持错误语义、任务写入、去重和公共 API 不变。

验收：

- [x] 100 个有效 item 的创建前校验从 200 次逐项读取降至 1 次批量读取；完整创建路径从 205 次 SQL 调用降至 6 次。
- [x] 缺失 item 仍返回对应 `ItemNotFound`，VIDEO item 仍拒绝，输入去重和 1 到 100 条限制不变。
- [x] 元数据任务行、item 顺序和后续 worker 行为保持原有回归；不改变 schema 或任务状态机。
- [x] 性能记录只报告固定 SQLite fixture 的 SQL 调用数，不推断端到端墙钟、PostgreSQL 或 NAS 收益。

依赖：LUX-053、LUX-056。预计文件：`src/application/reidentify.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加 100 个 item 的创建校验计数回归，再复用现有批量元数据读取。

结果（2026-10-03）：元数据任务创建前校验复用一次批量元数据读取，100 个有效 item 的逐项类型/元数据读取由 200 次降为 1 次；包含任务写入和回读的完整路径由 205 次降为 6 次，减少 199 次（约 97.1%）。缺失 item、VIDEO 拒绝、去重和任务结果回归通过；本任务未实测 PostgreSQL 墙钟、NAS 或 worker 端到端时延。

#### LUX-360：批量写入章节检测 marker

范围：章节检测结果替换同一媒体源和 provider 的隐藏 marker 时，存储层先删除旧记录，再对最多三个 marker 逐条 INSERT。将 marker 写入改为有界多行 INSERT，保留 fingerprint 校验、删除旧结果、marker 顺序、空结果清理、事务边界和其他 provider 的 marker，不改变章节检测协议或读取 DTO。

验收：

- [x] 3 个 marker 的替换从 5 次 SQL 调用降至 3 次，marker 数量和字段保持正确。
- [x] fingerprint 不匹配仍回滚且不写入；空 marker 仍只删除当前 provider 结果；其他 provider 记录不受影响。
- [x] 多行 INSERT 使用 SQLite/PostgreSQL 通用参数化语句并保持有界，不改变章节检测任务状态机。
- [x] 性能记录只报告固定 SQLite fixture 的 SQL 调用数，不推断端到端墙钟、PostgreSQL 或 NAS 收益。

依赖：LUX-209。预计文件：`src/storage/catalog.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加 3 marker 的替换查询计数回归，再实现有界多行写入。

结果（2026-10-03）：同一媒体源的 3 个 marker 由逐条 INSERT 改为一条 3 行多值 INSERT；包含 fingerprint 读取和旧 marker DELETE 的 SQL 调用从 5 次降至 3 次，减少 2 次（40%）。回归验证 marker 数量、fingerprint 校验和章节检测 API/读取行为；本任务未实测 PostgreSQL 墙钟、NAS 或生产负载。

#### LUX-361：批量预读本地元数据完整性计划依赖

范围：本地元数据完整性 worker 已经批量读取 item 元数据，但计划计算仍对每个 item 单独读取媒体策略、图片索引和 metadata attempt 状态。新增有界批量预读并把已读图片索引传给计划计算，保持本地文件存在性检查、NFO 投影、人物关系文件、重试语义、补缺资格和完整性队列合同不变。本任务不迁移文件读取，也不改变后续刮削器资格查询。

验收：

- [x] 205 个 item 的策略、图片索引和 attempt 三类依赖读取从 615 次 SQL 调用降至 3 次有界查询。
- [x] scanner 使用批量计划结果，缺失能力、输入顺序、指纹、attempt 冷却和补缺资格保持不变。
- [x] 图片索引预读不会跳过现有本地图片路径、fallback、缩略图策略和路径安全检查。
- [x] 相关 storage、reidentify 和 scanner 回归通过；不改变 schema、公共 API 或数据库事务边界。
- [x] 性能记录只报告固定 SQLite fixture 的 SQL 调用数，不推断墙钟、PostgreSQL、NAS 或生产收益。

依赖：LUX-358。预计文件：`src/storage/media.rs`、`src/storage/catalog.rs`、`src/storage/metadata.rs`、`src/application/images.rs`、`src/application/candidates.rs`、`src/application/scanner.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加逐 item 与批量依赖读取计数回归，再接入 scanner。

结果（2026-10-03）：新增 205 item 的逐项与批量对照回归；媒体策略、图片索引和 attempt 状态由旧路径 615 次 SQL 调用降为 3 次，减少 612 次（约 99.5%）。scanner 改为一次批量计划预读后按 item 回填，保留本地图片/NFO/人物文件检查、fallback 和重试语义；schema、公共 API 与事务边界未改变。本机 `uname -m=arm64`，SQLite 计数不外推 PostgreSQL、NAS 或端到端墙钟。

#### LUX-362：合并有序与 legacy 刮削器配置读取

范围：刮削器 resolver 先查询 `library_scrapers`，没有可用有序配置时再次查询 legacy `libraries.scraper_id`。使用有界联合查询同时取得两种配置，保留有序配置优先、主/备用顺序、legacy fallback 和插件不可用错误语义；为后续批量资格检查提供存储入口。本任务只修改配置读取及现有单 item resolver，不提前接入 scanner。

验收：

- [x] 205 个 item 的配置批量读取为 1 次有界查询；空有序配置的现有单 item resolver 总配置读取由 410 次降为 205 次。
- [x] 有序配置、legacy fallback、未选择刮削器、无效 role、不可用插件、移除 item 与禁用库的现有解析语义保持不变。
- [x] 批量读取覆盖超过 500 个 ID、空输入、重复 ID、顺序和各 item 隔离；单次绑定不超过 500 个值。
- [x] storage 与 scraper 回归通过；不改变 schema、公共 API 或插件协议。
- [x] 性能记录只报告固定 SQLite fixture 的配置读取 SQL 调用数，不推断插件 RPC 墙钟或生产收益。

依赖：LUX-361。预计文件：`src/storage/media.rs`、`src/storage/repository_tests.rs`、`src/application/scraper.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加逐 item 与批量配置读取计数回归，再合并现有 resolver 的配置读取。

结果（2026-10-03）：有序配置读取在 205 item fixture 中由 205 次逐项查询降为 1 次有界批量查询；单 item resolver 统一复用同一配置读取并保留 legacy fallback。storage、scraper、reidentify 回归通过，本机 `uname -m=arm64`；未改变 schema、公共 API、插件协议或 scanner 的逐 item 资格检查。

#### LUX-363：批量判断本地完整性补缺刮削器资格

范围：scanner 在计划计算后仍对每个可补缺 item 单独调用 resolver 检查选中刮削器是否可用。收集有 requestable capability 的 item ID，一次批量加载配置并逐 item 复用现有客户端缓存完成可用性判断，再按 item 回填补缺资格。插件错误继续降级为不可自动补缺并保留告警，不改变手动元数据任务、插件 RPC 或队列状态机。

验收：

- [x] 205 个具备 requestable capability 的 item 只触发 1 次配置读取，插件客户端缓存与可用性判断仍逐 item 隔离。
- [x] 没有 requestable capability 或关闭自动匹配的 item 不进入资格批量查询；无 resolver 时沿用 provider key 判断。
- [x] 有序/legacy 配置、插件不可用、批量读取失败、输入重复和 item 顺序的补缺资格结果保持不变。
- [x] scraper、reidentify、scanner 回归通过；不改变 schema、公共 API、插件协议或任务写入语义。
- [x] 性能记录只报告固定 SQLite fixture 的配置读取 SQL 调用数，不推断插件 RPC 墙钟或生产收益。

依赖：LUX-362。预计文件：`src/application/scraper.rs`、`src/application/reidentify.rs`、`src/application/scanner.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加批量资格回归，再接入 scanner。

结果（2026-10-03）：scanner 只对具备 requestable capability 且未关闭自动匹配的 item 发起一次批量 resolver 配置读取，之后按 item 复用客户端缓存并隔离插件错误；无 resolver、legacy fallback 和失败降级语义保持不变。scraper、reidentify、scanner 回归及全目标 Rust 门通过，本机 `uname -m=arm64`；未改变 schema、公共 API、插件协议或任务写入语义。

#### LUX-364：批量预读本地完整性图片写回源上下文

范围：本地完整性计划已批量读取策略、图片索引和 attempt 状态，但图片本地检查对每个 item 仍分别读取媒体类型与可写回源路径。增加有界批量读取的写回上下文，并将已读上下文传入图片路径检查；保留电影/视频直接源、剧集/季度首集源选择，canonicalize、library root containment、legacy 图片路径和缺失源错误语义。本任务不处理 NFO projection 或人物关系文件读取。

验收：

- [x] 205 个 item 的媒体类型与写回源读取由逐 item 的 410 次 SQL 调用降为 1 次有界查询。
- [x] 直接媒体源、剧集/季度首集源、无源 item、输入重复和超过 500 个 ID 的批次边界保持原有结果语义。
- [x] 本地图片存在性、fallback、legacy episode fanart、路径 canonicalize 与 root containment 行为保持不变。
- [x] storage、metadata selection 回归通过；不改变 schema、公共 API、NFO projection 或人物文件读取边界。
- [x] 性能记录只报告固定 SQLite fixture 的 SQL 调用数，不推断 PostgreSQL、NAS 或生产墙钟收益。

依赖：LUX-363。预计文件：`src/storage/catalog.rs`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/repository_tests.rs`、`src/application/images.rs`、`src/application/candidates.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-10-03）：新增批量写回上下文读取，205 个 item 的类型与源预读由 410 次逐项查询降为 1 次；直接电影源和剧集首集源回归保持一致。完整性计划复用该上下文完成图片本地检查，未改变路径安全和缺失源语义。存储批量回归、metadata selection 图片/NFO 回归、本机 `uname -m=arm64` 与 library Clippy 通过；NFO projection、人物文件和端到端 worker 墙钟未纳入本任务性能数值。

#### LUX-365：批量读取插件安装状态

范围：插件管理列表、已安装列表和通知插件列表在生成视图时逐个读取 `installed_plugins` 状态。新增按插件 ID 有界批量读取并在列表服务中复用，保持未安装/已禁用/已启用三态、store-only 插件和分页排序语义；不改变插件配置文件读取、运行状态或插件 RPC。

验收：

- [x] 205 个插件 ID 的安装状态由逐项 205 次 SQL 调用降为 1 次有界查询。
- [x] 未安装、已禁用和已启用状态映射保持一致，重复 ID、空输入和超过 500 个 ID 的批次边界保持稳定。
- [x] 管理插件列表、已安装列表和通知插件列表复用批量状态，不改变动态配置校验、运行状态、分页和 store/catalog 合并。
- [x] storage、插件 API 与相关插件配置回归通过；不改变 schema、插件协议或配置文件边界。
- [x] 性能记录只报告固定 SQLite fixture 的 SQL 调用数，不推断 PostgreSQL、NAS 或生产墙钟收益。

依赖：LUX-364。预计文件：`src/storage/users.rs`、`src/storage/repository_tests.rs`、`src/application/plugins.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-10-03）：新增安装状态批量读取，插件管理列表和通知插件列表先按 ID 一次加载状态，再逐插件生成既有动态视图；205 个状态由 205 次读取降为 1 次。插件 API、danmaku、media-info 与 IP location 回归通过，本机 `uname -m=arm64`，library Clippy 通过；动态视图中的配置文件读取和运行时状态查询未纳入本任务 SQL 数值。

#### LUX-366：章节检测计划同步去除重复读取

范围：章节检测计划同步在每个已启用章节插件循环中重复读取全部媒体库，并在选中库写入任务时再次读取同一插件配置。复用 LUX-365 的批量安装状态、将媒体库列表延迟到首个有效插件后只读取一次，并缓存已验证的章节插件设置；保留无插件、禁用插件、无效配置、库类型过滤和任务写入语义。

验收：

- [x] 同一次同步最多读取一次媒体库列表，不再按已启用章节插件数量重复读取。
- [x] 每个有效章节插件的配置只解析一次，后续选中库直接复用已验证设置。
- [x] 未安装/禁用插件、无效配置、电影库和 chapter source 不匹配库仍按原规则跳过；任务 schedule、并发和窗口参数保持一致。
- [x] 章节检测 API 回归、library Clippy 和格式检查通过；不改变 schema、插件协议或任务存储合同。

依赖：LUX-365。预计文件：`src/application/plugins.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。

结果（2026-10-03）：章节同步先批量读取候选插件安装状态，在首个有效插件后缓存一次媒体库列表，并复用每个插件的已验证配置写入选中库任务。章节检测两项集成回归、本机 `uname -m=arm64`、library Clippy 与格式检查通过；未据静态调用上界推断墙钟或生产收益。

#### LUX-367：图片路径冲突修复去除逐候选数据库读取

范围：episode thumbnail 路径冲突修复在每个 `-thumbnail[-N]` 候选路径上查询一次数据库确认路径是否被其他图片占用，最多尝试 1,000 个候选。改为按冲突条目的 item ID 有界预读现有图片路径，在内存中筛选候选，并在实际写回前保留一次数据库复核；保留文件类型、内容一致性、路径命名、硬链接/原子写入和更新失败语义。

验收：

- [x] 每个修复批次按最多 500 个 item ID 预读图片索引，不再对每个候选路径执行数据库查询。
- [x] 当前待修复图片被排除，其他图片占用的目标路径仍跳过；写回前保留数据库复核以覆盖并发变更。
- [x] episode 冲突修复和系列元数据重扫回归通过；不改变 schema、图片路径合同或文件安全检查。
- [x] 性能记录只报告静态 SQL 调用上界变化，不推断墙钟、PostgreSQL、NAS 或生产收益。

依赖：LUX-366。预计文件：`src/application/image_repairs.rs`、`tests/image_writer.rs`、`tests/series_metadata.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-10-03）：修复器先按冲突 item 批量预读 `item_images`，候选循环改为内存路径判断；成功写回前仍执行一次数据库占用复核。既有图片修复和系列重扫回归、本机 `uname -m=arm64`、library Clippy 与格式检查通过；未据静态上界推断端到端时延。

#### LUX-368：插件状态批量读取覆盖剩余服务循环

范围：章节源列表、Manifest scheduled task 同步和 IP location provider 选择仍在循环中逐个读取插件安装状态。复用批量安装状态读取，保持动态视图、配置校验、插件优先级、任务禁用/注册和 IP138 互斥语义；不改变插件协议或任务存储合同。

验收：

- [x] 章节源列表、Manifest task 同步和 IP location provider 选择各自按候选插件 ID 批量读取安装状态。
- [x] 未安装、已禁用和已启用插件的既有过滤、排序和互斥行为保持不变。
- [x] 章节检测、插件管理和 IP location 回归通过；保留 IP location provider 的既有优先级，不改变 schema、配置文件或 RPC 边界。
- [x] 性能记录只报告静态循环调用上界，不推断墙钟、PostgreSQL、NAS 或生产收益。

依赖：LUX-367。预计文件：`src/application/plugins.rs`、`tests/ip_location_plugins.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。

结果（2026-10-03）：剩余三个插件服务循环统一复用批量安装状态，保留原过滤、动态视图、任务语义和 IP location provider 优先级。新增优先级回归与章节检测、插件管理、IP location 回归、本机 `uname -m=arm64`、library Clippy 和格式检查通过；未据静态调用上界推断端到端收益。

#### LUX-369：插件视图复用媒体库选项读取

范围：插件管理列表、已安装列表、通知插件列表和章节源列表生成动态视图时，带 `media-libraries` 配置字段的每个插件都会重复读取媒体库及其 scraper 关联。按一次列表请求建立有界媒体库选项快照，并传给同一请求内的动态视图；单插件配置接口仍按独立请求读取最新数据，章节检测插件继续隐藏 `libraryIds` 字段。

验收：

- [x] 两个带媒体库选项的插件列表请求，安装状态批量读取之外的媒体库/关联读取由每插件两次降为列表级一次，固定 SQLite fixture 的 SQL 调用由 5 次降至 3 次。
- [x] 媒体库启用过滤、章节插件电影库过滤、选项值/名称、插件排序和分页语义保持不变。
- [x] 通知插件、章节源和普通插件视图复用同一快照；没有媒体库选项的插件不额外触发媒体库读取。
- [x] 插件管理、媒体信息、弹幕配置和 IP location 回归通过；不改变 schema、插件协议、配置文件或 RPC。
- [x] 性能记录只报告固定 SQLite fixture 的 SQL 调用数，不推断插件视图墙钟、PostgreSQL、NAS 或生产收益。

依赖：LUX-368。预计文件：`src/application/plugins.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加两个媒体库选项插件的查询计数回归，再接入列表级快照。

结果（2026-10-03）：动态插件列表按请求懒加载一次 `list_libraries` 结果，并在普通、已安装、通知和章节源列表中复用；单插件路径保持独立读取。两个媒体库选项插件的固定 SQLite 查询由 5 次降至 3 次，选项过滤与插件配置回归通过；本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-370：Manifest 任务禁用镜像批量更新

范围：Manifest scheduled task 同步在同一插件声明多个任务时，对每个 task 分别开启事务并更新配置、计划两张表。将同一插件的 task 类型收集后在一个事务中用有界 `IN` 更新两张镜像表，保留未安装插件停用、计划镜像同步和后续 owner 注册语义；本任务不改变 owner 注册的任务字段或批量写入合同。

验收：

- [x] 同一插件两个 task 的禁用镜像更新由 4 次 SQL 调用、两次事务降至 2 次 SQL 调用、一次事务。
- [x] 配置表和计划表的启用状态、task 类型过滤、单 task 兼容路径和空 task 输入保持不变。
- [x] Manifest、弹幕配置和存储计划镜像回归通过；不改变 schema、插件协议、owner 注册字段或调度语义。
- [x] 性能记录只报告固定 SQLite fixture 的 SQL 调用数，不推断任务同步墙钟、PostgreSQL、NAS 或生产收益。

依赖：LUX-369。预计文件：`src/application/plugins.rs`、`src/storage/library.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加两个 task 的逐项禁用查询计数基线，再接入同插件有界批量禁用。

结果（2026-10-03）：Manifest 同步先按插件收集 task 类型，再一次事务更新 `scheduled_task_configs` 与 `scheduled_task_plans`；owner 注册循环保持原字段和顺序。两个 task 的固定 SQLite 镜像更新由 4 次降至 2 次，存储计划镜像、弹幕配置和 Manifest 注册回归通过；本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-371：NFO probe 写回复用媒体写回上下文

范围：`write_item_probe_details` 先分别读取媒体条目类型和写回源，随后调用通用 NFO target 解析再次读取同一类型和源。复用已有的 `list_media_item_writeback_contexts_by_ids` 单条上下文查询，并把电影 target 路径安全检查提取为纯路径阶段；保留 MOVIE/STRM/source ID 过滤、canonicalize、library root containment、既有 NFO 命名和写回后 fingerprint 处理。

验收：

- [x] 电影 probe 写回的类型/源预检由 2 次独立读取加 target 阶段重复 2 次，收敛为 1 次上下文查询；通用系列、季度、分集 NFO target 路径保持原读取合同。
- [x] 错误 source、STRM 源、无源条目、非电影条目、非标准 movie.nfo 和路径越界行为保持不变。
- [x] NFO writer、series metadata、metadata 回归通过；不改变数据库 schema、NFO/Emby 合同或 probe 状态。
- [x] 性能记录只报告固定路径上的 SQL 调用边界，不推断 NFO 写回墙钟、PostgreSQL、NAS 或生产收益。

依赖：LUX-370、LUX-364。预计文件：`src/application/nfo.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先覆盖 probe 写回的现有路径回归，再复用已存在的写回上下文查询。

结果（2026-10-03）：probe 写回直接使用一次有界写回上下文读取，电影 target 复用该 source 完成 canonicalize 和 root containment；普通 NFO 写入继续独立解析 item 类型。`nfo_writer` 25 项、`series_metadata` 3 项、`metadata` 20 项通过；本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-372：本地 NFO enrichment 复用元数据快照

范围：`MetadataEnricher::enrich_nfo_item` 在同一条 NFO 处理流程中先读取媒体元数据做身份冲突校验，完成 provider ID、NFO 缓存和人物关系同步后又读取完整元数据构造最终写回。复用本次 enrichment 开始时的元数据快照；中间步骤不修改媒体元数据列，保持锁定字段、provenance、身份冲突和 NFO fingerprint 写回语义。

验收：

- [x] 单条 NFO enrichment 的完整媒体元数据读取由 2 次降为 1 次；provider ID、人物关系和 NFO cache 仍按原顺序执行。
- [x] NFO 身份冲突、锁定字段、provenance、premiere/rating、坏 NFO 非阻塞和重复 enrichment 回归保持不变。
- [x] metadata、series metadata、NFO writer 回归通过；不改变 schema、NFO/Emby 合同或在线刮削器协议。
- [x] 性能记录只报告固定调用路径的 SQL 读取边界，不推断 worker 墙钟、PostgreSQL、NAS 或生产收益。

依赖：LUX-371、LUX-364。预计文件：`src/application/metadata.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先锁定重复 `find_media_item_metadata` 基线，再复用同一处理快照。

结果（2026-10-03）：NFO enrichment 保留一次初始 `find_media_item_metadata` 结果，在 provider ID、NFO cache 和 actor relation 处理后直接构造最终 `MediaMetadataUpdate`；metadata 20 项、series metadata 3 项和 NFO writer 25 项通过。本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-373：批量读取 STRM resolver 安装状态

范围：STRM resolver 可用性检查在遍历插件目录时逐个读取 `installed_plugins`。先收集当前 catalog 中声明 `strm.resolve` 的 resolver 插件 ID，再按有界批量查询一次安装状态，随后复用状态生成可用插件列表；保留插件目录顺序、未安装/禁用过滤、动态配置校验和 resolver RPC 顺序，不改变插件协议或数据库 schema。

验收：

- [x] 同一可用性请求包含多个 STRM resolver 时，安装状态读取由每插件一次降为一次批量查询。
- [x] 未安装、已禁用和已启用插件的过滤、动态配置可用性判断及返回顺序保持不变。
- [x] STRM resolver 播放与插件服务回归通过；不改变插件协议、配置文件或 RPC 边界。
- [x] 性能记录只报告固定 SQLite fixture 的 SQL 调用数，不推断 resolver RPC 墙钟、PostgreSQL、NAS 或生产收益。

依赖：LUX-368。预计文件：`src/application/plugins.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加多个 resolver 的安装状态查询计数回归，再复用已有批量安装状态读取。

结果（2026-10-03）：两个已安装 STRM resolver 的可用性检查由 2 次逐插件 `installed_plugins` 查询降为 1 次批量查询；动态视图与 resolver 返回顺序保持不变。`application::plugins::plugin_discovery_tests::strm_resolver_availability_reads_installation_statuses_once` 与 STRM resolver 集成回归通过，本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-374：批量迁移旧章节插件媒体库选择

范围：旧章节插件配置迁移对每个 `libraryIds` 分别读取完整媒体库（连同 scraper 关联），再逐库开启事务写入 `chapter_source_id`。将同一插件的库 ID 去重后按最多 100 个一批，在一个有界事务中只为存在、非电影且尚未分配章节源的库写入当前插件；保留插件优先级、无效/重复 ID、电影库和已有章节源的跳过语义，不改变章节任务或公共 API 合同。

验收：

- [x] 一个配置包含两个可分配库、一个电影库、一个已有章节源库和一个重复 ID 时，旧迁移路径的 10 次 storage SQL 调用降为 2 次（插件状态读取和一次批量条件更新）。
- [x] 不存在库、电影库、已有章节源、重复 ID 和多个插件的优先级语义保持不变。
- [x] 章节检测、计划任务、插件服务和存储回归通过；不改变 schema、插件协议或章节任务状态机。
- [x] 性能记录只报告固定 SQLite fixture 的 SQL 调用数，不推断迁移墙钟、PostgreSQL、NAS 或生产收益。

依赖：LUX-373。预计文件：`src/storage/library.rs`、`src/application/plugins.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加旧配置迁移的查询计数回归，再替换逐库读取和写入。

结果（2026-10-03）：旧迁移对包含两个可分配库、一个电影库、一个已有章节源库和一个重复 ID 的配置执行 10 次 SQL；新路径只执行一次插件状态读取和一次有界条件 UPDATE，共 2 次，减少 8 次（80%），且在同一插件内去重 ID、跨插件按既有排序保留先到先得。插件私有回归、章节检测/API、scheduled tasks、plugins、库级 Clippy 与格式检查通过，本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-375：合并元数据写回策略读取

范围：NFO 和图片写回共用的 `item_metadata_writeback_enabled` 先读取条目所属库 ID，再读取完整媒体库和 scraper 关联，最后读取全局媒体策略。改用已有的 item/library JOIN 读取一次本地策略与全局策略，保持启用库、未移除条目、库策略优先级和 JSON 容错语义，不改变写回目标或媒体元数据合同。

验收：

- [x] 单条写回策略判断由 4 次 storage SQL 调用降为 1 次。
- [x] 库策略优先于全局策略；禁用库、已移除条目和无策略仍返回原有结果。
- [x] NFO、图片和 metadata 回归通过；不改变数据库 schema、文件写回或公共 API。
- [x] 性能记录只报告固定 SQLite fixture 的 SQL 调用数，不推断写回墙钟、PostgreSQL、NAS 或生产收益。

依赖：LUX-364。预计文件：`src/application/metadata_writeback.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加策略判断查询计数回归，再复用已有的单条 JOIN 读取。

结果（2026-10-03）：策略判断从条目库 ID、完整库（含 scraper 关联）和全局设置三段读取收敛为一次 `find_item_media_strategy_settings`；固定 fixture 从 4 次降为 1 次（75%）。写回、NFO、图片和 metadata 回归通过，本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-376：批量注册 Manifest 媒体库任务 owner

范围：Manifest `LIBRARY` scheduled task 当前为每个 `ownerConfigKey` 值单独 upsert `scheduled_task_configs`。新增有界多行 upsert，按最多 100 个 owner 一批，在一次事务中写入；同一请求内重复 owner 去重。`GLOBAL` task 继续使用现有计划镜像注册路径，不改变 owner 类型、任务字段、启用状态或调度语义。

验收：

- [x] 205 个唯一媒体库 owner 加 1 个重复值由 206 次 SQL 写入降为 3 次有界批量 upsert。
- [x] owner 任务行数量、重复 owner 幂等、任务字段和启用状态保持不变。
- [x] Manifest、插件、计划任务和存储回归通过；不改变 schema、插件协议或 GLOBAL 计划镜像合同。
- [x] 性能记录只报告固定 SQLite fixture 的 SQL 调用数，不推断插件同步墙钟、PostgreSQL、NAS 或生产收益。

依赖：LUX-375。预计文件：`src/storage/library.rs`、`src/application/plugins.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加媒体库 owner 的逐条注册计数回归，再接入有界多行 upsert。

结果（2026-10-03）：`LIBRARY` owner 注册由逐条 upsert 改为 100/100/5 三批多行 upsert，并在输入内去重；205 个唯一 owner 加 1 个重复值由 206 次降为 3 次。GLOBAL task 的计划镜像路径保持原样；插件、Manifest、scheduled task、存储定向回归和库级 Clippy 通过，本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-377：复用弹幕计划的插件设置与可用性读取

范围：按配置为多个媒体库创建弹幕任务时，当前每个库都会重新解析插件配置，并重新读取插件安装状态、动态配置和媒体库选项。一次批量创建中复用已解析的 `DanmakuSettings`，并延迟一次可用性检查后复用结果；保留逐库存在性、活动任务、未选库错误优先级和每库独立任务写入语义，不改变插件协议或任务 schema。

验收：

- [x] 两个选中库的创建路径由 26 次 storage SQL 调用降为 19 次。
- [x] 每库任务数量、并发/覆盖选项、活动任务跳过和插件不可用错误语义保持不变。
- [x] 弹幕服务、配置、插件和 API 回归通过；不改变任务表、插件配置或 RPC 合同。
- [x] 性能记录只报告固定 SQLite fixture 的 SQL 调用数，不推断插件配置解析墙钟、PostgreSQL、NAS 或生产收益。

依赖：LUX-376。预计文件：`src/application/danmaku.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加多库配置任务的查询计数回归，再复用同一设置和可用性结果。

结果（2026-10-03）：两个媒体库的配置任务创建从每库重复读取设置/可用性改为设置读取一次、可用性检查一次；固定 fixture 从 26 次 SQL 降为 19 次，减少 7 次（约 26.9%）。弹幕、配置、配置 API 和插件回归通过，本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-378：Manifest 任务同步复用媒体库选项

范围：同一次 Manifest task 同步遍历多个带 `media-libraries` 配置字段的插件时，动态字段解析重复读取媒体库及 scraper 关联。把选项快照限制为当前同步调用内懒加载一次并复用；没有媒体库选项或未安装的插件不触发读取，单插件配置接口仍读取当前数据，不改变任务注册、启用状态或调度合同。

验收：

- [x] 两个带媒体库选项的 GLOBAL task 插件同步，由 15 次 storage SQL 调用降为 13 次。
- [x] 插件字段选项、过滤、配置校验、任务 schedule 和 GLOBAL 计划镜像保持不变。
- [x] 插件列表/Manifest 同步、弹幕配置和计划任务回归通过；不改变 schema 或插件协议。
- [x] 性能记录只报告固定 SQLite fixture 的 SQL 调用数，不推断同步墙钟、PostgreSQL、NAS 或生产收益。

依赖：LUX-376、LUX-369。预计文件：`src/application/plugins.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先扩展多插件选项 fixture 覆盖同步调用，再复用同步内快照。

结果（2026-10-03）：Manifest 同步在首次需要媒体库选项时加载一次媒体库/关联快照，随后各插件复用；两插件 GLOBAL task fixture 从 15 次降为 13 次，减少 2 次（约 13.3%）。插件、弹幕配置、scheduled tasks 和库级 Clippy 通过，本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-379：缩略图 scraper 重试复用首轮源读取

范围：缩略图 scraper-first 重试在同一轮中先判断策略是否适用，再判断 poster/thumbnail 是否缺失；两个判断分别读取同一条本地缩略图源。将首轮检查合并为一次源读取并复用其媒体库策略字段，保留无源、STRM、无 scraper、策略不适用和图片路径安全检查语义。元数据刷新完成后的最终图片检查必须重新读取源和图片索引，以反映本轮写回结果。

验收：

- [x] scraper-first 重试首轮检查对带 scraper 的本地媒体由 4 次 SQL 调用降为 3 次：本地源、全局策略和图片索引各读取一次。
- [x] 无源、STRM、无 scraper、非 `SCRAPER_FIRST` 策略和 poster/thumbnail 缺失、fallback、路径越界行为保持不变。
- [x] 元数据刷新后的最终图片重新检查仍读取最新源和图片索引；三次失败后的截图回退、重试时间和状态写回保持不变。
- [x] 缩略图、scheduled tasks 和相关 scanner/metadata 回归通过；不改变 schema、任务状态机、图片文件合同或插件协议。
- [x] 性能记录只报告固定 SQLite fixture 的 SQL 调用边界，不推断刷新墙钟、PostgreSQL、NAS 或生产收益。

依赖：LUX-378。预计文件：`src/application/thumbnails.rs`、`src/application/scheduled_tasks.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加首轮检查的查询计数回归，再复用同一源读取；刷新后的最终检查保留独立读取。

结果（2026-10-03）：scraper-first 重试首轮对带 scraper 的本地媒体由源/策略判断 2 次读取加图片缺失判断 2 次读取，共 4 次 SQL，收敛为一次状态读取中的本地源、全局策略和图片索引，共 3 次，减少 1 次（25%）。元数据刷新完成后的最终 `scraper_first_images_missing` 仍独立重新读取源和图片索引；缩略图回退、scanner/metadata 和计划任务回归通过。本机 `uname -m=arm64`，未实测刷新墙钟、PostgreSQL、NAS 或生产收益。

#### LUX-380：降低弹幕任务取消状态轮询

范围：弹幕匹配任务处理一页待处理条目时，当前在每个条目启动前都读取一次 `cancel_requested`，100 条页面会产生 100 次重复状态读取，末尾还会再检查一次。改为首项和每 8 个条目检查一次，并在页面 worker 排空后保留最终检查；保留取消后不再领取后续条目、取消 pending 项、任务状态写回和 worker 并发上限语义，最多增加 8 个条目的取消响应边界。

验收：

- [x] 100 条待处理页面的取消状态读取由 101 次降为 14 次（首项、每 8 条一次和页面结束最终检查）。
- [x] 取消请求在检查点后不会再领取超过 8 个条目；已领取 worker 仍按原有完成/失败写回，pending 项统一标记为 `CANCELLED`。
- [x] 无取消请求时任务成功/失败状态、进度计数、插件调用顺序和并发限制保持不变。
- [x] 弹幕、scheduled tasks 和相关 API 回归通过；不改变 schema、插件协议或任务状态机。
- [x] 性能记录只报告固定页面和检查间隔的 SQL 调用边界，不推断插件 RPC 墙钟、PostgreSQL、NAS 或生产收益。

依赖：LUX-379。预计文件：`src/application/danmaku.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先锁定取消检查间隔边界，再调整 `run_claimed` 的轮询位置。

结果（2026-10-03）：弹幕匹配每页最多 100 条时，取消状态读取从逐条检查加页面结束检查的 101 次静态上界，降为首项及每 8 条一次的 13 次间隔检查加 1 次最终检查，共 14 次，减少 87 次（约 86.1%）。检查点后最多再领取 8 条，已领取 worker、pending 取消、进度和插件调用语义保持不变；弹幕、配置、API 和 scheduled tasks 回归通过。本机 `uname -m=arm64`，未实测插件 RPC 墙钟、PostgreSQL、NAS 或生产收益。

#### LUX-381：批量写入 STRM 缩略图图片记录

范围：STRM 探测成功生成缩略图后，当前对同一个文件分别写入 `POSTER`、`THUMB` 两条 `item_images`，再单独清除媒体条目的 `poster_fallback_required`，形成 3 次写入和 3 个短事务。复用已有有界图片批量写入，将两条图片记录和 fallback 清除放入同一个事务；保留同一路径、尺寸、标签、来源、图片类型、失败状态和 fallback 语义。

验收：

- [x] STRM 缩略图成功登记由 2 次逐条图片 upsert 加 1 次 fallback UPDATE，降为 1 次批量图片 INSERT/UPSERT 加 1 次 fallback UPDATE。
- [x] `POSTER`、`THUMB` 两条记录的路径、尺寸、内容标签和 `STRM_FFMPEG` 来源保持一致；批量写入失败时不清除 fallback，任务仍按原失败语义结束。
- [x] 截图优先、已有图片跳过、NONE 策略、媒体信息和增量 STRM 探测回归保持不变；不改变 schema、插件协议或图片 API 合同。
- [x] STRM、storage、scheduled tasks 和相关 scanner 回归通过；性能记录只报告固定 fixture 的 SQL/事务边界，不推断插件 RPC 墙钟、PostgreSQL、NAS 或生产收益。

依赖：LUX-380。预计文件：`src/application/strm_probe.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加批量图片事务的查询计数回归，再接入 STRM 探测写回。

结果（2026-10-03）：STRM 截图登记从两次逐条 `item_images` upsert 加一次 fallback UPDATE，改为已有批量图片写入中的一条多行 UPSERT 加一次 fallback UPDATE，共 2 次 SQL、1 个事务；storage 回归锁定该边界。`POSTER`、`THUMB` 的路径、尺寸、标签、来源和 fallback 语义保持不变，STRM、scanner 和相关任务回归通过。本机 `uname -m=arm64`，未实测插件 RPC 墙钟、PostgreSQL、NAS 或生产收益。

#### LUX-382：批量删除媒体源与层级清理

范围：删除媒体条目或剧集时，应用层当前对每个媒体源分别查询存在性、开启事务删除并更新条目/父级/剧集层级。将源校验、删除和层级清理改为每批最多 250 个源的一组参数化 SQL；保留文件与旁车删除顺序、显式源删除、缺失源错误、item/parent/series 的移除条件、事务边界和每源 webhook 语义。

验收：

- [x] 两个同一电影条目的源从逐源 6 次 SQL 调用降为一次批量查询、删除和层级更新共 3 次。
- [x] 剧集删除按 item、parent、series 顺序批量更新，系列、季度和分集最终移除语义与原实现一致。
- [x] 源 ID 与 item ID 不匹配或批次中有缺失源时在写入前返回失败，不部分删除；显式单源和整条目删除 API 回归保持通过。
- [x] 每批最多 250 个源，层级更新最多绑定 750 个条目 ID，使用 SQLite/PostgreSQL 通用参数化查询，不改变 schema、文件删除或 webhook 合同。
- [x] 性能记录只报告固定 SQLite SQL 调用数，不推断端到端时延、PostgreSQL、NAS 或生产收益。

依赖：无。预计文件：`src/application/deletion.rs`、`src/storage/jobs.rs`、`docs/PERFORMANCE.md`。先增加多源删除的查询计数回归，确认逐源基线，再实现有界批量删除。

结果（2026-10-03）：两个同一电影条目的源由旧路径的 6 次逐源 SELECT/DELETE/UPDATE 降为 3 次批量 SQL；不匹配或缺失源只执行一次校验查询且不写入。剧集删除保留 item、parent、series 三层顺序更新，避免同一 UPDATE 中父级看不到刚删除的子级；删除媒体源和整剧集 API 回归通过。本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产负载。

#### LUX-383：计划任务媒体库配置批量读取

范围：执行包含多个媒体库的任务计划时，调度器当前对每个媒体库单独读取 `scheduled_task_configs`，然后再创建各自的运行任务。新增每批最多 500 个 owner ID 的配置读取，并在计划派发期间复用结果；保留每库独立运行、失败隔离、未注册任务错误、章节插件 ID 传递、全局任务路径和调度游标语义。

验收：

- [x] 两个媒体库的计划配置读取由两次逐库查询降为一次有界批量查询；两个独立运行任务仍各自创建。
- [x] 缺失配置的媒体库仍单独记录 `NotRegistered` 并继续派发其他媒体库；章节检测继续使用对应配置的 `plugin_id`。
- [x] 输入 owner ID 去重并按最多 500 个值分批，使用 SQLite/PostgreSQL 通用参数化查询，不改变任务表、计划镜像或公共 API。
- [x] scheduled tasks 4 项、计划镜像 storage 12 项、fmt 和 Clippy 回归通过；性能记录只报告 SQL 调用边界。

依赖：LUX-244。预计文件：`src/application/scheduled_tasks.rs`、`src/storage/library.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加多个 owner 的批量配置读取计数回归，再接入计划派发。

结果（2026-10-03）：计划派发先按最多 500 个媒体库 owner 批量读取任务配置，再按媒体库复用配置创建独立运行任务；两库配置读取固定为 1 次，原有每库独立运行与失败隔离合同保持不变。scheduled tasks 4 项、计划镜像 storage 12 项和相关格式/Clippy 定向验证通过；本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-384：复用无计划任务配置读取

范围：调度器分页读取没有执行计划的 `scheduled_task_configs` 后，当前仍调用通用 `run_task` 再次按 owner 查询同一配置。直接复用已分页读取的配置行，保留 owner/task 校验、插件 ID、未注册错误、独立运行任务和计划任务路径语义，不改变任务表或公共 API。

验收：

- [x] 无计划任务执行不再重复查询当前已加载的配置行；任务仍按原配置创建运行任务。
- [x] 非法 owner、unsupported task、缺失配置和章节插件 `plugin_id` 语义保持不变；执行计划任务仍使用 LUX-383 的批量读取路径。
- [x] 无计划任务调度回归、scheduled tasks、fmt 和 Clippy 通过；性能记录只报告重复读取边界。

依赖：LUX-383。预计文件：`src/application/scheduled_tasks.rs`、`tests/scheduled_tasks.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加无计划任务执行回归，再复用当前分页配置。

结果（2026-10-03）：无计划任务分页得到的 `StoredScheduledTaskConfig` 直接传入执行分发，移除了每个任务再次 `find_scheduled_task_config` 的重复读取；新增无计划扫描任务回归与 scheduled tasks 5 项回归通过。本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-385：批量写入服务器设置

范围：管理员保存服务器设置时，当前在同一个事务中对固定的五个 `server_settings` 键逐条执行 UPSERT。改为一次有界的五行参数化 UPSERT，保留键名、值转换、冲突更新、事务边界和管理设置 API 合同，不引入 schema 变化。

验收：

- [x] 一次服务器设置保存由五条逐键 UPSERT 降为一条多行 UPSERT；五个键的值和冲突更新语义保持不变。
- [x] 单条 SQL 只绑定固定五组值，兼容 SQLite/PostgreSQL，不把外部设置值拼入 SQL 文本。
- [x] storage 查询计数回归锁定写入调用数；管理设置、登录背景、首页/播放阈值相关回归保持通过。
- [x] 不改变事务边界、更新时间字段、服务器设置读取接口或公共 API。

依赖：无。预计文件：`src/storage/users.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加固定五键写入计数回归，再收敛为单条多行 UPSERT。

结果（2026-10-03）：服务器设置保存由五条逐键 UPSERT 收敛为一条固定五行参数化 UPSERT，事务和读取语义保持不变。storage 回归验证查询调用从 5 次降为 1 次，并校验五个键的最终值；本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-386：批量重排卸载插件后的媒体库刮削器

范围：卸载刮削器插件时，当前先删除插件关联，再对每个受影响媒体库单独读取剩余刮削器、删除并逐条重插、读取新的主刮削器和更新媒体库。改为按最多 100 个媒体库批量读取、清理、重插和更新主刮削器；保留位置顺序、首项变为 `PRIMARY`、原 `PRIMARY` 后移为 `BACKUP`、其他角色、章节源清理、安装记录删除和事务边界。

验收：

- [x] 两个受影响媒体库的刮削器重排由逐库/逐项 SQL 收敛为有界批量读取、删除、插入和主刮削器更新；当前固定 fixture 查询从 15 次降为 6 次。
- [x] 受影响库无剩余刮削器时 `libraries.scraper_id` 置空；备用和补充角色、位置和插件卸载后的章节源清理保持不变。
- [x] 每批最多 100 个媒体库，刮削器重插每批最多 100 行，所有 ID、角色和值均使用绑定参数，兼容 SQLite/PostgreSQL。
- [x] 插件卸载 API、媒体库管理回归、storage 查询计数回归、fmt、Clippy 和全目标 Rust 门禁通过；不改变插件文件清理或公共 API。

依赖：无。预计文件：`src/storage/users.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加多库卸载重排的查询计数回归，再实现有界批量重排。

结果（2026-10-05）：合并后的批量卸载实现对两个受影响媒体库执行 6 次 storage SQL 调用；固定 fixture 的旧实现为 15 次。角色重排、章节源清理、安装记录删除及插件/媒体库/storage 回归通过。本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-387：批量回收过期 Web 播放会话

范围：Web HLS 清理 worker 每轮最多领取 128 个过期或不活跃会话，但当前对每个会话单独执行条件 UPDATE。保留先分页读取、条件竞争保护、只返回实际成功停止的会话和后续临时目录清理语义，改为按会话 ID 一次批量 UPDATE，并用 `RETURNING` 过滤已被其他请求抢先更新的行。

验收：

- [x] 过期和不活跃会话的清理均由每轮最多 129 次 SQL 收敛为 1 次 SELECT 加 1 次批量 UPDATE；返回会话只包含实际从 `ACTIVE` 变为 `STOPPED` 的记录。
- [x] 过期时间、`SERVER_HLS` 计划、不活跃心跳条件和 `updated_at` 写入保持不变；空批次不发 UPDATE。
- [x] ID 批次受现有 128 条领取上限约束，所有 ID 和时间值使用绑定参数，兼容 SQLite/PostgreSQL。
- [x] storage 播放会话回归、Web 播放回归、fmt、Clippy 和全目标 Rust 门禁通过；不改变播放 API 或 HLS 文件清理合同。

依赖：无。预计文件：`src/storage/sessions.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加多会话清理查询计数回归，再接入批量条件 UPDATE。

结果（2026-10-03）：固定三个过期和三个不活跃会话的清理由每类旧路径 4 次 storage SQL 调用降为 2 次（SELECT 加批量 UPDATE）；`RETURNING` 保留并发条件下的实际成功集合，现有过期、不活跃和 Web 播放回归通过。本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-388：批量重挂载剧集合并的额外分集

范围：剧集合并时，源季度没有同号目标季度，或源季度存在未匹配集号时，当前对每个分集单独更新 `series_id` 或 `parent_id + series_id`。改为按最多 100 个分集 ID 批量更新；保留季度迁移、匹配分集的媒体源/用户状态/合并标记顺序，以及源季和目标季的层级语义。

验收：

- [x] 一个额外源季度的 20 个分集重挂载由 20 条逐分集 UPDATE 降为 1 条有界 UPDATE；固定完整合并查询从 28 次降为 9 次。
- [x] 无目标季度时只更新 `series_id`；有目标季度但集号未匹配时同时更新 `parent_id` 与 `series_id`；匹配分集仍沿用原有媒体源、用户状态和 `merged_into_item_id` 路径。
- [x] 每批最多 100 个分集 ID，使用 SQLite/PostgreSQL 通用参数化 SQL，不改变数据库模型、事务边界或合并顺序。
- [x] media merge storage、item merge、series scanner 回归、fmt、Clippy 和全目标 Rust 门禁通过；性能记录只报告固定 SQLite SQL 调用数。

依赖：LUX-324、LUX-356。预计文件：`src/storage/media_merge.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加额外季度分集的 SQL 计数回归，再批量化两类层级重挂载。

结果（2026-10-03）：额外季度 20 个分集的完整剧集合并从 28 次 storage SQL 调用降为 9 次，减少 19 次（约 67.9%）；新增目标季度未匹配集号的父级/系列断言，既有匹配分集、扫描和 item merge 回归通过。本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-389：批量合并剧集匹配分集的媒体源与状态

范围：剧集合并中，集号匹配的分集当前逐项执行媒体源重挂载、默认源归一化、用户播放状态合并/清理和 `merged_into_item_id` 更新。对目标集号唯一的映射改为有界映射批量 SQL；目标集号重复时保留逐项路径，以保持用户状态版本递增语义。季度层级、未匹配分集和不同源剧集的顺序语义保持不变。

验收：

- [x] 20 个唯一匹配分集的完整合并由旧路径 110 次 storage SQL 调用降为 15 次；媒体源、用户状态和合并标记均在批量映射路径完成。
- [x] 目标分集 ID 重复时回退逐项处理，保持原有用户状态版本和合并顺序语义；普通唯一映射使用批量路径。
- [x] 媒体源默认标记、播放进度/已看/收藏/播放次数、源条目标记结果保持原合同；批次最多 100 对映射，所有值使用绑定参数。
- [x] media merge storage、item merge、series scanner 回归、fmt、Clippy 和全目标 Rust 门禁通过；不改变数据库模型、事务边界或公共 API。

依赖：LUX-324、LUX-356、LUX-388。预计文件：`src/storage/media_merge.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加匹配分集 SQL 计数和状态回归，再接入映射批处理。

结果（2026-10-03）：20 个唯一匹配分集的完整合并由 110 次 storage SQL 调用降为 15 次，减少 95 次（约 86.4%）；重复目标集号保留逐项回退，item merge、series merge 和扫描回归通过。本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。

#### LUX-390：批量同步季度与剧集已看状态

范围：播放回调和手动已看状态更新调用 `sync_played_container_states`，一个分集最多关联季度和剧集两个父级。原实现读取父级后，对每个父级单独查询可播放分集状态并 UPSERT。合并父级状态读取和写入，保留用户隔离、空容器、不可用/已删除分集筛选、播放次数、时间戳和版本合同。不修改 HTTP、数据库模型或事务边界。

验收：

- [x] 两个父级的同步由 5 次 storage SQL 调用降为 3 次；最多两个父级、10 个 UPSERT 绑定值。
- [x] 完成、取消已看、重复同步与空容器行为保持原合同；收藏、进度和其他用户状态不被覆盖。
- [x] 定向 storage 回归和 `series_api`、`playback`、`web_playback`、`resume_favorites` 集成回归通过。
- [x] Rust build、全目标测试、fmt、全目标全 feature Clippy 和差异检查通过；记录 ARM64 与后端验证边界。

预计文件：`src/storage/sessions.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。

实施记录（2026-10-04）：旧实现查询计数回归先失败，实测为 5 次；新实现固定为 3 次，减少 2 次（约 40%）。父级状态、播放次数、版本、取消已看、重复同步和空容器回归通过。本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产收益。本任务为新一轮的单独增量，上一轮截至 LUX-389 的收口记录保持不变。

#### LUX-391：批量写入媒体库刮削器配置

范围：媒体库创建和编辑刮削器配置时，原实现对每个有序 scraper 单独执行 `library_scrapers` INSERT。改为复用一个有界多行 INSERT helper；保留最多 16 项校验、位置/角色顺序、主刮削器兼容字段、事务边界和单个 legacy `scraper_id` 更新语义。不改变读取 API 或数据库模型。

验收：

- [x] 5 个 scraper 行由 5 次 INSERT 降为 1 次；批次上限 100，超过上限仍分批写入。
- [x] 创建和编辑路径复用同一 helper；位置、角色、主刮削器字段和空列表行为保持不变。
- [x] storage 查询计数回归、library 集成回归和现有计划任务/插件 scraper 回归通过。
- [x] Rust build、全目标测试、fmt、全目标全 feature Clippy 和差异检查通过；记录 ARM64 与后端验证边界。

文件：`src/storage/library.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。测试与私有 helper 位于同一模块，因此未修改 `repository_tests.rs`。实现前 helper 回归因方法不存在而编译失败；旧创建/编辑循环每项执行一条 INSERT 属于源码计数，新 helper 的 SQL 调用数通过测试实测。补充空输入、205 行分批、位置/角色与后续批次失败回滚覆盖。

最终门禁（2026-10-05）：本机 `uname -m=arm64`，Toshiba target 已挂载。`cargo build --locked`、`cargo test --locked --all-targets`（711 passed、0 failed、11 ignored；其中 PostgreSQL 专项 15 项因无本地 PostgreSQL 而 ignored）、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 与 `git diff --check` 均通过。未进行 PostgreSQL、NAS 或生产环境性能验证。

#### LUX-392：复用 NFO 阶段的 source 快照

范围：扫描本地 metadata 的完整度阶段当前会在 NFO 阶段已查出 source 后，再按 filesystem entry 和目录重新展开完整 source 列表。让 NFO 阶段携带其实际处理的 `(item_id, source_id)` 快照；完整度阶段批量确认该 source 仍是 item 当前的首选有效 source，再读取 active item metadata。不要复用更早的图片阶段快照；source 已删除、item 已移除或首选 source 已改变时，不写该 item 的完整度结果。

验收：

- [x] 完整度阶段不再调用 `list_scan_local_metadata_sources`，只使用 NFO 阶段 source identity，并在有界批量查询中复核当前首选 source。
- [x] 回归覆盖 source 未变、source 被删除或标 missing、首选 source 切换；非重试失败排除项、增量扫描和 backfill 完整度语义保持不变。
- [x] 单 item、单目录固定 fixture 的完整度读取从 3 次 storage query-wrapper 调用降至 2 次；不据此推断墙钟、FNOS CPU、PostgreSQL 或 NAS 收益。
- [x] 相关本地 metadata / 扫描回归、build、fmt、全目标全 feature Clippy 与差异检查通过。
- [x] 全目标 Rust 测试通过。

文件：`src/application/metadata.rs`、`src/application/scanner.rs`、`src/storage/jobs.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。

结果（2026-10-05）：NFO source identity freshness 回归、`scanned_metadata`（15 项）、`scanned_series_metadata`（2 项）、串行 `scanning_jobs`（81 项）、build、fmt、Clippy 与差异检查通过。固定 SQLite fixture 的完整度阶段查询从 3 次降至 2 次。全目标串行 Rust 测试通过；此前在单独运行中失败的 `tests/emby_counts.rs::emby_item_counts_respects_auth_user_scope_and_favorites` 在本次完整串行运行中通过，未再复现。依赖本地 PostgreSQL 的测试和显式性能基准按测试配置忽略。没有据本地调用数推断 FNOS、PostgreSQL 或 NAS 收益。

#### LUX-393：校准插件卸载刮削器查询计数

范围：后续合并后的 `uninstall_plugin` 已将两个受影响媒体库的 fixture 压到 6 次 storage query-wrapper 调用，但旧回归仍断言 8 次，导致全目标测试失败。更新计数断言及 LUX-386 性能记录；不改变插件卸载行为。

验收：

- [x] 固定两个媒体库 fixture 断言 6 次调用，并继续验证刮削器位置、角色和 legacy 主刮削器结果。
- [x] LUX-386 文档记录当前实现为 15 次降至 6 次；性能比例与调用范围说明一致。
- [x] 定向 storage 回归通过；无运行时代码或 schema 变化。

文件：`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。

结果（2026-10-05）：插件卸载 storage 回归通过；双媒体库 fixture 固定为 6 次 query-wrapper 调用，插件卸载实现和 schema 均未变。

#### LUX-394：避免自动补缺仅因可选 provider 详情重复排队

范围：本地扫描完整度计划继续记录缺失的 `EXTERNAL_IDS` 和 `TRAILERS`，但二者单独缺失不启动自动 `FILL_MISSING`。核心 metadata、已启用图片或 credits 缺失仍可排队；这些必要能力触发任务时允许顺带补充外部 ID 和预告片。显式 metadata 任务保持通用 request plan，并且 optional-only 项不额外读取 capability attempt 状态来决定自动排队。

验收：

- [x] 仅缺 `EXTERNAL_IDS` / `TRAILERS` 时自动完整度计划不可排队；核心 metadata、图片或 credits 缺失仍可排队。
- [x] 显式 `FILL_MISSING` / `FULL_REFRESH` 计划仍包含其原有能力，不改变人工请求行为。
- [x] 定向 metadata selection、NFO writer 和 `cargo build --locked` 通过。
- [ ] 全目标 Rust 测试全绿：并行运行中的字幕/HLS 超时在逐项串行复跑时通过；串行全量运行复现无关的 `tests/emby_counts.rs:159` 失败，viewer 无剧集库权限时仍得到 `SeriesCount = 1`。
- [x] fmt、全目标全 feature Clippy 与差异检查通过。
- [x] 性能记录只描述计划资格与 attempt 状态读取边界，不推断 FNOS CPU 收益。

文件：`src/application/candidates.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。

结果（2026-10-06）：固定电影 metadata 测试对象先复现旧逻辑会把 optional-only 计划标记为可排队，再验证仅缺外部 ID/预告片不会启动自动补缺、credits 缺失仍会触发，且通用 metadata request plan 未改变。定向计划单测、`metadata_selection`（31 项）、`nfo_writer`（26 项）和 `cargo build --locked` 通过。并行全目标运行出现的 5 个字幕/HLS 超时逐项串行复跑通过；串行全目标运行则发现无关的 Emby 计数访问范围失败（`tests/emby_counts.rs:159`，实际 1、期望 0），因此全量 Rust 测试门未通过，本任务没有修改该独立行为。未据本地测试推断服务器性能收益。

#### LUX-395：按状态索引统计扫描任务

范围：管理健康数据每次请求都用 `SUM(CASE...)` 聚合整个 `scan_jobs` 历史表。FNOS 只读执行计划显示该表约 25.7 万行；单次并行顺序扫描约 155 ms、读取 8,722 个 shared buffers。改为分别统计活动扫描和失败扫描，让二者都能通过部分索引计数；保持健康 API 字段和统计含义不变。

验收：

- [x] SQLite 与 PostgreSQL 都有仅覆盖 `FAILED` 任务的计数索引；SQLite 从空库迁移成功。
- [x] 活动任务计数和失败任务计数使用各自匹配的部分索引；SQLite 回归检查查询计划且验证返回计数。
- [x] `admin_health` / dashboard 的 `scanRunning`、`scanFailed` 字段合同不变。
- [x] 定向 Rust 测试、build、fmt、Clippy 和差异检查通过；全目标 Rust 测试结果及已有独立失败如实记录。
- [x] 性能记录区分 FNOS 基线执行计划与本地 SQLite 查询计划，不把未部署变更表述为生产收益。

预计文件：`src/storage/jobs.rs`、`migrations/0163_scan_job_failed_count_index.sql`、`migrations-postgres/0163_scan_job_failed_count_index.sql`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。

结果（2026-10-06）：管理健康计数从一次历史表条件聚合改为两个可由状态部分索引覆盖的 `COUNT(*)` 子查询；新增 SQLite/PostgreSQL `FAILED` 部分索引，schema 版本为 162。SQLite 空库迁移、计数与 `EXPLAIN QUERY PLAN` 单测通过；`admin_health`、dashboard、ready/version、storage 目标通过，storage 47 项全过；scanner 17 项、danmaku 7 项、scanning_jobs 80 项通过。全量 `cargo test --locked --all-targets` 的库测试为 729 passed、11 ignored；随后在既有无关 `tests/emby_counts.rs:159` 失败（实际 1、期望 0）。`scanning_jobs` 全目标中另有一个用例并行运行时超时，单独串行复跑通过。Build、fmt、all-target Clippy 和 `git diff --check` 通过。本机没有 PostgreSQL 服务，Docker daemon 未启动，PostgreSQL 迁移未做运行时验证；FNOS 上仍是旧 revision，未部署、未测生产收益。

#### LUX-396：合并短周期管理健康探测

范围：管理员 dashboard 每 15 秒刷新；每次健康 payload 都会提交一次数据库探针写入、创建并 fsync 4 KB 临时文件，再启动 `ffprobe -version`。将这三项低频诊断结果按 AppState 缓存 30 秒，并合并并发刷新；`/health/ready` 继续执行实时数据库写探测，CPU、连接池、任务计数、媒体库等动态字段继续逐请求读取。

验收：

- [x] 同一状态缓存有效期内，多个健康/dashboard 请求只执行一次数据库、配置目录与 ffprobe 探测；缓存过期后重新探测。
- [x] admin health/dashboard JSON 字段与降级映射保持不变；`/health/ready` 不使用缓存。
- [x] 回归覆盖 TTL 命中、过期刷新和并发请求合并；管理健康、dashboard、ready/version 定向测试通过。
- [x] build、fmt、all-target Clippy 和差异检查通过；全量测试独立失败已如实记录。
- [x] 性能记录注明 dashboard 的 15 秒轮询与 30 秒探测缓存；不将调用次数变化推断成 FNOS CPU 或时延收益。

预计文件：`src/api/legacy.rs`、`src/api/admin_handlers.rs`、`tests/admin_health.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。缓存测试与 AppState 状态定义同文件。先写 TTL 命中/过期/并发合并测试，再接入健康数据构建。

结果（2026-10-06）：AppState 共享同一个 30 秒探测快照；对并发请求合并数据库写探针、配置目录读写检查与 `ffprobe -version`，TTL 从探测完成时起算；CPU、连接池、任务计数和库状态仍实时生成，`/health/ready` 继续实时检查写能力。缓存 TTL/过期合并/锁等待过期/探测耗时单测 3 项通过；`admin_health`、`admin_dashboard`、`ready_version` 共 4 项通过。`cargo build --locked`、`cargo fmt --all -- --check`、all-target/all-features Clippy 与 `git diff --check` 通过。`cargo test --locked --all-targets` 库测试 732 passed、11 ignored，随后在既有独立 `tests/emby_counts.rs:159` 失败（实际 1、期望 0）；此前 LUX-395 也观察到此失败，本任务未改动计数访问范围。性能记录仅按 dashboard 15 秒轮询/探测 30 秒 TTL 估算调用频率，未部署 FNOS，也未测 CPU、NAS 或 API 时延收益。

#### LUX-397：去重近期 provider-unavailable 的 FILL_MISSING 条目

范围：扫描完整度调度和通用 `create_or_merge_fill_missing_job` 都只将 QUEUED/RUNNING job 中 PENDING/RUNNING 的 item 当作活动去重项。provider 暂时不可用时，job 会进入 DEFERRED，失败 item 会落为 `FAILED/SCRAPER_UNAVAILABLE`，因此现有 1 小时 DEFERRED 窗口没有实际抑制重复调度。两处查询都应将 1 小时内 DEFERRED job 中明确标记 `SCRAPER_UNAVAILABLE` 的 item 纳入去重；其他错误仍可重试，超过窗口仍可重新排队。保留 QUEUED/RUNNING 行为、跨库语义和任务 API。

验收：

- [x] completeness 扫描调度和通用 FILL_MISSING 创建入口都抑制 1 小时内 DEFERRED 且 `FAILED/SCRAPER_UNAVAILABLE` 的同一 item。
- [x] 其他失败分类和超过 1 小时的 DEFERRED provider-unavailable item 仍可创建新任务。
- [x] QUEUED/RUNNING 的活动去重、queued job 合并及原子保存 completeness 和调度意向行为保持不变。
- [x] 相关 storage 回归、build、fmt、全目标全 feature Clippy 与差异检查通过；性能记录只说明去重状态覆盖，不外推 FNOS CPU 收益。

预计文件：`src/storage/jobs.rs`、`src/storage/metadata.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先扩展现有 SQLite storage 回归证明 FAILED provider item 当前会漏过去重，再更新两个查询条件。

结果（2026-10-06）：两处去重查询都将近 1 小时内 DEFERRED job 中 `FAILED/SCRAPER_UNAVAILABLE` item 作为已有工作；普通失败仍能立即重新排队，provider-unavailable 超过一小时后也可重新排队。SQLite storage 回归分别覆盖扫描 completeness 调度和通用 FILL_MISSING 创建入口，原子事务、queued job 合并与既有 active dedup 保持不变。两条定向测试、build、fmt、全目标全 feature Clippy 和差异检查通过。全目标测试的 lib 部分为 732 passed、11 ignored，随后在既有无关 `tests/emby_counts.rs:159` 失败（viewer 无剧集库访问权限时实际计数 1，预期 0）；本任务未改该行为。未部署 FNOS，也未测生产 CPU/队列创建率。

#### LUX-398：校准 Compose 扫描并发注释

范围：README 的 Compose 环境变量注释把索引默认值写成 8，但 Docker 镜像、Compose 与配置代码的默认值均为 2。将注释改为实际的 2/8/2；不改变运行配置、并发算法或环境变量。

验收：

- [x] README 中索引、ffprobe、ffmpeg 的注释值与 Dockerfile、Compose 和配置代码默认值一致。
- [x] 文档差异检查通过；本任务不涉及运行时行为，不运行 Cargo 测试。

文件：`README.md`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-10-06）：Compose 注释已改为索引、ffprobe、ffmpeg 默认并发 `2/8/2`，与 Dockerfile、Compose、Rust 配置常量及相邻说明一致。`git diff --check` 通过；仅改文档，未运行 Cargo 测试。

#### LUX-400：限制跨任务 FILL_MISSING worker 总并发

范围：单个 `FILL_MISSING` job 虽默认最多运行 2 个 worker，但进程级 metadata semaphore 容量为 16；多个媒体库的补缺 job 同时运行时仍可能累计到 16 个在线补缺 worker。增加独立的进程级补缺 semaphore，将所有 `FILL_MISSING` job 的 item worker 总数限制为 2，并继续使用既有 metadata 全局 semaphore。`REIDENTIFY` 和 `FULL_REFRESH` 不获取补缺专用 permit；不改变每库 job 排队、item claim、任务状态、重试或 scraper 请求合同。

验收：

- [x] 同一 Lux 进程中来自不同 job/service 的 `FILL_MISSING` item worker 总并发最多为 2。
- [x] 每个 worker 同时持有既有 metadata 全局 permit 和补缺 permit，worker 结束、job 退出或 future 取消时通过 RAII 释放。
- [x] 非 `FILL_MISSING` 模式不获取补缺 permit，既有 metadata 全局并发上限保持 16。
- [x] 双 job 并发集成回归直接统计两个 job 的 `RUNNING` item；取消等待 worker permit 的 job 会在占用 permit 的 job 释放前结束；相关 `reidentify` 定向目标和补缺 semaphore 单测通过。
- [x] 完成 Rust build、fmt、all-target Clippy 与全目标 Rust 测试并如实记录已有的独立失败。
- [x] 性能记录仅陈述代码并发上界与固定 stub 测试结果，不推断 FNOS CPU、墙钟、PostgreSQL 或 NAS 收益。

文件：`src/application/reidentify.rs`、`tests/reidentify.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先运行双 job 回归复现旧实现允许多个补缺 worker 同时超过 2，再添加进程级补缺 semaphore。

结果（2026-10-06）：新增独立容量为 2 的进程级 semaphore；每个补缺 item worker 在完整处理期间同时持有补缺 permit 与原有 metadata permit。固定 SQLite fixture 并行运行两个各含 4 个 item 的补缺 job，通过 job-item `RUNNING` 状态统计进程实际 claim 的并发 worker，最大值不超过 2。取消等待 permit 的第二个 job 后，它会在第一个 job 仍占用两个 permit 时结束；独立单测还验证通知被消费后，后续 permit 等待仍会看到锁存的取消状态。相关测试二进制中的 FILL_MISSING 执行用例用异步 mutex 串行，避免共享进程级 semaphore 造成测试相互干扰。`tests/reidentify.rs` 14 项通过，补缺 semaphore 与取消锁存单测通过；build、fmt 和全目标全 feature Clippy 通过。`cargo test --locked --all-targets` 的 library 测试为 734 passed、11 ignored，随后在无关的 `tests/emby_counts.rs:159` 失败（实际 1、期望 0）；本任务没有修改该访问范围行为。开发机架构为 `arm64`；未部署 FNOS，也未测量 CPU 或生产墙钟收益。

#### LUX-401：保留活动 FILL_MISSING 中变化后的补全意图

范围：自动扫描按 item 级去重活动和近期 `DEFERRED` 的 `FILL_MISSING`。当本地 completeness 输入指纹或当前 missing capability 集在旧 job 处理期间变化时，item 级去重可能吞掉新请求；近期 DEFERRED 也可能挡住实际上不同的新能力请求。为 job item 持久保存自动请求的 input fingerprint 与规范化 capability set，并在 claim 时保存处理快照。相同快照继续合并/去重；queued item 更新为最新快照；running item 在新快照到达后只复跑一次最新状态；近期 DEFERRED 仅抑制同快照。旧数据和人工创建的无快照 job 保持兼容，公共任务 DTO 不变。

验收：

- [x] SQLite/PostgreSQL 迁移为 job item 增加可空请求指纹、请求 capability set 与 claim 快照；SQLite 162→163 升级回归确认旧任务状态、计数和快照默认值保持正确。
- [x] 新的不同 fingerprint 或 capability set 不被近期 DEFERRED item 错误去重；相同快照仍去重，queued job 合并仍有界。
- [x] 请求在 item RUNNING 期间变化时，当前 worker 结束后该 item 回到 PENDING 并使用最新快照复跑；同一输入重复请求不触发复跑或并发重复 worker。
- [x] 取消、重试、worker 异常恢复和非 FILL_MISSING job 状态/计数语义保持正确，任务 API DTO 不变。
- [x] SQLite 状态机回归、migration-from-empty 与 build、fmt、全目标全 feature Clippy 通过；PostgreSQL SQL/迁移合同经可用的集成目标验证。

短计划与文件：先在 `src/storage/repository_tests.rs` 为 RUNNING item 收到新 fingerprint/capability 后仍需处理新意图写失败回归；新增 `migrations/0164_metadata_fill_request_snapshots.sql` 与 `migrations-postgres/0164_metadata_fill_request_snapshots.sql`；实现涉及 `src/storage/mod.rs`、`src/storage/metadata.rs`、`src/storage/jobs.rs`、`src/storage/repository_tests.rs`，以及 schema version 断言和迁移序列检查；最后更新本任务记录、`docs/PERFORMANCE.md` 与 `docs/LUX-FILL-MISSING-OPTIMIZATION-PLAN.md`。只处理自动 FILL_MISSING 请求快照，不扩展到其他 metadata 模式。

结果（2026-10-06）：相同快照重试对 job/item 表执行 0 次 INSERT、0 次 UPDATE；queued 输入变化更新既有 item 一次；RUNNING 期间出现变化的请求在 worker 收尾后重新进入 PENDING，取消不会重排，显式 retry 使用最新 fingerprint。迁移版本推进到 163，更新 SQLite/PostgreSQL schema-version 断言和迁移序列检查。定向状态机、旧任务 migration、claim/recovery fixture、`admin_health`、`ready_version`、`storage` 通过；build、fmt 和全目标全 feature Clippy 通过。全目标测试 `--no-fail-fast` 中 737 个库测试通过、11 个忽略；集成测试中 `emby_counts`（实际 1、期望 0）及 `strm`（401、期望 200）失败，前者已有独立基线记录，后者单独复跑仍失败但与本任务改动路径无关；`library_cover_generation` 全套时曾失败，独立复跑 5 项通过。PostgreSQL 测试端口 127.0.0.1:55432 未监听且 Docker 不可用，故未运行 PostgreSQL 集成迁移；本机 `arm64`，未部署 FNOS，也未测 CPU/生产墙钟收益。完整完成门仍未满足。

发布集成说明（2026-10-06，0.5.19）：保留 GitHub `test` 已发布的 `0162_filesystem_entry_directory_prefix_index.sql` 及全部历史迁移内容。LUX-395/LUX-401 尚未部署的迁移分别顺延为 0163/0164，同步双后端迁移列表、schema-version 断言和请求快照升级 fixture 的起始版本。上方测试结果对应功能分支原编号（162/163），不代表重新编号后的集成验证；本次按项目所有者要求不运行测试、构建或 lint，仅做 Git 差异、迁移编号/引用和合并保留检查。

#### LUX-402：provider 不可用时对自动 FILL_MISSING 进行逐 item 退避

范围：当前自动 `FILL_MISSING` 在 scraper/provider 不可用后将 job 置为 `DEFERRED`，并只依赖固定一小时去重窗口。对于持续不可用的 provider，周期扫描可能每小时再次创建相同 item 请求。为带自动请求快照的 job item 持久化退避次数、下次自动重试时间和到期记录是否已用于一次重试；同一 fingerprint/capability 快照在到期前去重，到期后由后续自动扫描重试一次，避免已完成的重试继续被旧失败记录触发。被新 fingerprint 或 capability 取代的旧失败快照也要消费，避免旧到期记录在新请求成功后再次解锁该 item。退避依次为 5 分钟、30 分钟、最多 6 小时。新请求快照不继承旧快照的退避；无快照的人工 job 不改变现有语义。管理员显式重试立即解除等待、清除已消费标记并保留自动失败次数；取消任务不增加次数。

验收：

- [x] SQLite/PostgreSQL migration 增加自动退避次数、到期时间和一次性消费标记；旧的自动 `DEFERRED/SCRAPER_UNAVAILABLE` 且带请求快照的 item 升级后有安全的首次到期时间，历史任务状态和计数不变。
- [x] 同一请求快照在到期前不创建新 job；到期后可创建新 job，并继承该快照此前的失败次数。
- [x] 到期重试派发后旧失败记录不再重复触发自动重试；管理员手动重试会重新启用该记录的自动退避。
- [x] 新 fingerprint/capability 替代旧请求后，旧 provider 失败记录被消费，不会在新请求成功后再次解锁同一 item。
- [x] 连续 provider 失败按 5 分钟、30 分钟、最多 6 小时退避；新 fingerprint/capability 不等待旧快照；人工 retry 可立即领取。
- [x] 仅自动且有请求快照的 `FILL_MISSING` item 使用退避；`REIDENTIFY`、`FULL_REFRESH`、人工无快照 job、取消与其他错误不受影响。
- [x] SQLite 状态机、migration-from-empty、旧 deferred-job 升级回归、build、fmt、全目标全 feature Clippy 通过；PostgreSQL SQL/迁移合同在 PostgreSQL 16 集成环境通过。

短计划与文件：实现涉及 `migrations/0165_metadata_fill_missing_retry_backoff.sql`、`migrations-postgres/0165_metadata_fill_missing_retry_backoff.sql`、`src/storage/jobs.rs`、`src/storage/metadata.rs`、`src/storage/mod.rs`、`src/storage/repository_tests.rs`、`tests/storage.rs`，以及公开 schema version 断言所在的集成测试文件。先添加红色状态机回归，再实现逐 item 退避、相同快照的跨 job 次数继承和到期去重，最后分别验证 SQLite 与 PostgreSQL 合同。本任务不改元数据 API DTO 或扫描并发策略。

结果（2026-10-07）：新增双后端 migration 0165，为 provider 失败后的自动快照 item 保存失败次数和截止时间；退避为 5 分钟、30 分钟、最高 6 小时，并在新 job 中继承相同 fingerprint/capability 的次数。migration 只为已有自动 deferred provider 失败且带请求快照的 item 安排首次 5 分钟冷却，保持任务状态和计数；无快照旧任务仍沿用一小时去重。相同快照冷却去重、新 fingerprint/capability、显式 retry、取消、其他错误及非 `FILL_MISSING` 模式均有状态机回归。去重检查保留单条有界 SQL 查询，不额外增加扫描调度往返。SQLite migration/state-machine 测试、`tests/storage.rs` 49 项、`tests/postgres_database.rs -- --include-ignored` 16 项及 PostgreSQL progressive-scan storage contract 1 项通过；后者直接验证 provider 失败后的冷却、到期重试和失败次数继承。`cargo build --locked`、fmt、全目标全 feature Clippy 通过。全目标 Rust 测试库部分为 746 passed、0 failed、11 ignored；仅 `emby_counts`（实际 1、期望 0）与 `strm`（401、期望 200）失败，两者均在干净 `origin/test=8412cc2a` 上独立复现。开发机 `arm64`；FNOS 尚未部署本分支，未据此宣称生产 CPU 或真实重试节奏改善。

补充结果（2026-10-07）：端到端扫描回归发现并修复 READY 且仍缺失的完整度记录不会在冷却到期后重新入队的问题。当前扫描只对本轮已确认仍缺失、且自动匹配可用的候选 ID 检查到期的 provider 失败；查询限制在每批最多 512 个扫描条目，并复用完整度 claim 事务。0165 不再更新历史 job item 表，而是在 `server_settings` 写入单个首次冷却截止时间；旧失败行继续保持原状态和计数，运行时按兼容规则视为已失败一次。调整旧测试夹具，使它同时把 item 级 retry deadline 设为已到期；精确扫描回归通过，`tests/storage.rs` 49/49、`tests/scanning_jobs.rs` 81/81 通过。最终 `cargo test --locked --all-targets --no-fail-fast` 的库测试为 746 passed、0 failed、11 ignored；集成目标仅 `emby_counts`（实际 1、期望 0）及 `strm`（401、期望 200）失败，两者此前均在干净 `origin/test=8412cc2a` 上独立复现。`cargo build --locked`、fmt、全目标全 feature Clippy 通过。此前 PostgreSQL 不可用；恢复 PostgreSQL 16.15 临时实例后，修正 PostgreSQL 合同夹具绕过 `Database` SQL 适配器、把 `?` 占位符原样发给 PostgreSQL 的问题，`postgres_database` 集成目标 16/16 和 progressive-scan storage contract 1/1 通过。开发机为 `arm64`；本分支未部署 FNOS，未据此宣称生产 CPU 或重试流量改善。

补充审查结果（2026-10-07）：SQLite 回归复现到期重试完成后，旧 `DEFERRED/SCRAPER_UNAVAILABLE` 记录仍会触发后续自动入队。未发布的 0165 增加默认关闭的一次性消费标记；派发到期重试时在同一事务内批量消费旧失败记录，完整度扫描与补缺任务去重忽略已消费记录。手动 retry 和新请求快照会重置标记。SQLite 状态机和迁移回归通过，PostgreSQL progressive-scan storage contract 覆盖相同的成功重试路径；FNOS 当前仍在 schema 164，尚未应用该 migration。

最终验证复核（2026-10-07）：加入旧快照被替代的回归后，`cargo test --locked --all-targets --no-fail-fast` 单测为 747 passed、0 failed、12 ignored。集成目标中 `emby_counts`（实际 1、期望 0）和 `strm`（401、期望 200）失败；两项此前均在干净 `origin/test=8412cc2a` 上独立复现。`libraries_api` 有一项在全套运行中短暂返回 `DATABASE_UNAVAILABLE`，随后该用例单独运行通过，整个 `libraries_api` 目标也 13/13 通过。`tests/storage.rs` 49/49、`tests/scanning_jobs.rs` 81/81、`cargo test --locked --lib fill_missing` 19/19、SQLite 旧快照回归 1/1 和独立 PostgreSQL 旧快照回归 1/1 通过；`cargo build --locked`、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。开发机为 `arm64`；FNOS 仍未部署此分支，不能据此声称生产 CPU 已下降。

补充代码审查（2026-10-07）：新快照不再被旧 deferred 失败阻止，但原实现只消费完全相同快照的到期记录。新增 SQLite 与 PostgreSQL 回归，覆盖旧失败已到期、fingerprint 改变、新请求成功后不得再次被旧记录解锁；自动请求查询按当前批次的 item ID 读取未消费自动失败快照，并在同一事务消费被替代的旧失败。定向状态机与 PostgreSQL progressive-scan contract 通过；FNOS 当前仍在 schema 164，尚未验证生产效果。

#### LUX-403：复用有空位的 queued FILL_MISSING job

范围：`enqueue_fill_missing_jobs_in_transaction` 当前只检查按创建时间排序的第一个 queued job。若该 job 已满，而后续 queued job 仍有容量，就会新建多余 job。分配请求时按创建顺序遍历有空位的 queued job，复用其容量；只在现有容量用尽后创建新 job。每次查询只读取最多与待分配请求数相同的有空位 job，保持分配有界；单 job 最多 100 项及跨库隔离不变。

验收：

- [x] 已有多个 queued job 且最早 job 已满时，新请求合并到最早的后续有空位 job，不新增 job。
- [x] 请求跨越多个 queued job 的剩余容量时，按创建顺序填充并只为剩余请求新建 job；job/item 计数一致且每个 job 不超过 100 项。
- [x] 并发/事务、取消、DEFERRED 去重和跨库行为保持不变；SQLite storage 回归、build、fmt、Clippy 及差异检查通过。
- [x] 性能记录只陈述固定 fixture 中避免的多余 job 创建，不外推 CPU 或生产时延收益。

预计文件：`src/storage/jobs.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先写含 3 个 queued job（100、100、50 项）和后续追加请求的失败回归，再实现有界多 job 容量分配。

结果（2026-10-07）：补缺分配只查询 `total_count < 100` 的 queued job，按创建顺序复用，并将候选上限限制为待分配 item 数；PostgreSQL 查询额外锁定候选 job 行，SQLite 继续依赖现有写事务。SQLite 回归先确认旧逻辑在 `100/100/50` 后追加 item 会多建 job，再验证后续有 1 项容量时保持 3 个 job、追加 80 项后分布为 `100/100/100/31`，331 个 item 均入队。`fill_missing_job_creation` 两项和 PostgreSQL progressive-scan storage contract 通过；后者验证第二个 item 复用已有 queued job。build、fmt、全目标全 feature Clippy 和差异检查通过。未测量 FNOS CPU 或生产 job 创建率。

#### LUX-404：显式表达扫描完整度补缺策略

范围：扫描完整度流程用 `Option<bool>` 表达是否覆盖媒体库策略，并用 `Option<&str>` 隐含区分扫描任务与后台回填。改为显式 trigger 与策略类型：全量扫描/无任务的后台回填沿用媒体库“新扫描缺失补全”设置；实时增量按任务的 `auto_metadata_match`；已删除或找不到的扫描任务禁用在线补缺。禁用策略不得探测刮削器。保持现有配置语义、API、数据库 schema 和任务状态不变。

验收：

- [x] 完整度 trigger 和解析后的补缺策略均使用具名 enum，不再通过可空任务 ID/布尔值组合隐式表达策略。
- [x] 回归覆盖全量/后台回填、增量开关开启/关闭、任务不存在及禁用时不探测刮削器；现有媒体库设置读写语义不变。
- [x] 定向 scanner/storage 测试、build、fmt、全目标全 feature Clippy 和差异检查通过；不新增数据库/API 变化，也不据此宣称 CPU 收益。

预计文件：`src/application/scanner.rs`、`src/storage/metadata.rs`、`src/storage/mod.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`。先检查现有策略边界测试，再引入类型并保留同一套状态断言。

结果（2026-10-07）：补缺调用现在显式区分 `ScanJob` 与 `LibrarySetting` trigger，并解析成 `UseLibrarySetting`、`Enabled` 或 `Disabled` 策略；存储入口同样接收具名策略类型。全量任务、后台回填和精确目录本地刷新继续读取库级新扫描设置，增量扫描继续服从任务开关，已删除任务禁用在线探测。策略单测、精确目录刷新回归和 PostgreSQL policy/storage contract 通过；build、fmt、全目标全 feature Clippy 通过。没有 schema/API 变化，也未据此推断 FNOS CPU 收益。

#### LUX-405：验证跨媒体库全量扫描串行队列

范围：应用共享的 `ScanJobService` 使用容量为 1 的 `full_scan_queue` 逐个执行全量扫描，但现有测试使用共享的 `scan_lock` 挂起所有任务、并为每个扫描服务分别创建队列，未实际验证这个串行队列。将回归改为两个媒体库共享同一个扫描服务，给共享扫描工作 semaphore 留出并发容量，并在 SQLite 中拒绝第二个全量任务进入 RUNNING，以证明扫描队列本身提供串行约束。增量扫描在全量扫描让出共享扫描工作资源时的优先行为保持不变。

验收：

- [x] 两个不同媒体库的全量 job 通过同一个扫描服务并发启动时，同一时刻最多一个 `RECONCILE_LIBRARY` 处于 RUNNING，最终两者都完成。
- [x] 测试中的共享 `scan_lock` 有足够 permit，不会替代 `full_scan_queue` 成为串行原因；已有增量优先回归继续通过。
- [x] `scanning_jobs` 定向目标、build、fmt、全目标全 feature Clippy 和差异检查通过；性能记录不外推 CPU 或 NAS 收益。

预计文件：`tests/scanning_jobs.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先将旧回归改为能在无 `full_scan_queue` 的实现中失败，再验证当前容量为 1 的生命周期队列。

结果（2026-10-07）：回归使用共享 `ScanJobService` 和 2 个扫描 semaphore permit，并由 SQLite trigger 拒绝并发全量任务进入 `RUNNING`；两个不同媒体库的 job 都完成。`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test scanning_jobs` 81/81 通过。全目标 Rust 测试库部分为 746 passed、0 failed、11 ignored；集成目标仅 `emby_counts` 与 `strm` 失败，两者在干净 `origin/test=8412cc2a` 上也复现，之前出现的插件查询计数波动和扫描超时本轮未复现。build、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。默认扫描并发为 2，全量扫描队列容量为 1。验证环境为 arm64；没有据此推断 FNOS CPU 或 NAS 性能收益。

#### LUX-406：扫描本地 NFO 阶段复用 source 与元数据读取

范围：扫描本地元数据 outbox、库级补回及精确目录刷新先读取 source 并处理图片，随后又按 filesystem entry ID 完整查询一次 source。NFO 阶段改为消费图片阶段返回的 source 快照，并用有界查询确认原 source 仍是当前首选且存在；保留 NFO 处理后的既有新鲜度复核。按批次预取实际存在 NFO 的 item metadata 与路径，避免每个 NFO 各自读取完整 metadata 行；没有 NFO 的 item 不触发 metadata 查询。缩小 image work 分组复制的数据，并保留家庭视频 NFO 路径 I/O 错误的失败语义。普通增量/库级元数据路径继续按需读取，不改变 NFO 字段、任务状态、在线刮削或数据库 schema。

验收：

- [x] 扫描 outbox、后台本地 metadata 补回和精确目录刷新复用图片阶段的 source 快照；NFO 处理前验证当前首选 source，完成后的既有 source 新鲜度复核不变。
- [x] 一个 NFO 批次仅对包含 NFO 的 item 做一次有界 metadata 批量读取；无 NFO 批次跳过 metadata 查询；陈旧 source 在 NFO 处理前被排除。
- [x] MOVIE、VIDEO、EPISODE、SERIES、SEASON 的 NFO 路径和层级去重语义保持；家庭视频 `try_exists` 错误仍记录为可重试失败；非批处理路径继续按需读取。
- [x] NFO 查询计数、陈旧 source 与路径错误单测，以及 `scanned_metadata`、`scanning_jobs`、build、fmt、Clippy 和 `git diff --check` 通过；全目标套件已执行，但 `emby_counts`、`strm` 各有一个已在干净 `origin/test` 复现的基线失败，不能记作全目标全绿；性能记录不把 SQL 调用数变化外推为 FNOS CPU 收益。

预计文件：`src/application/metadata.rs`、`src/application/scanner.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`、`docs/LUX-FILL-MISSING-OPTIMIZATION-PLAN.md`。先为家庭视频路径查询错误保留行为写回归，再复用 source snapshot、批量预取并验证 outbox、backfill、精确刷新三条入口。

结果（2026-10-07）：source 快照复用、preferred-source 校验、批量 metadata 查询及家庭视频路径错误回归通过；`scanned_metadata` 16/16、`scanning_jobs` 81/81 通过。最终 `cargo build --locked`、`cargo fmt --all -- --check`、全目标全 feature Clippy 和 `git diff --check` 通过。`cargo test --locked --all-targets --no-fail-fast` 完整执行，但 `tests/emby_counts.rs` 的 auth/favorites 计数测试和 `tests/strm.rs` 的 STRM 鉴权测试各失败一次；这两项此前已在干净 `origin/test=8412cc2a` 复现，作为基线失败记录，不归因于本任务。没有据本机 ARM64 验证推断 FNOS CPU 或 NAS 收益。

#### LUX-407：让 scraper 预检临时失败进入自动退避

范围：本地 completeness 流程会在提交 missing 状态前预检所选 scraper。当前预检对某个 item 返回错误时只记录日志并把 item 排除在自动入队之外；同一 fingerprint 的 completeness 随后变成 `READY + is_missing=1`，后续扫描不会重新领取，因此 scraper 恢复后该 item 可能永远不再补全。对“已配置 scraper 但预检临时失败”的 item，允许创建有界 `FILL_MISSING` 请求，由现有 worker 记录 `SCRAPER_UNAVAILABLE` 并应用 LUX-402 的逐 item 退避。明确没有所选 scraper（预检成功返回 false）、自动匹配关闭或没有可请求 capability 时仍不入队。公共 API、schema 和无 scraper 的配置语义不变。

验收：

- [x] 已配置 scraper 的预检错误允许新确认缺失项进入现有 `FILL_MISSING` worker，由 `SCRAPER_UNAVAILABLE` 退避；同一快照冷却期去重由 LUX-402 storage 回归覆盖。
- [x] 预检成功但没有 scraper、自动匹配关闭或没有可请求 capability 时不创建自动 job；决策单测和现有扫描策略回归通过。
- [x] 预检错误恢复路径的回归、scanner/scanning_jobs/storage 定向覆盖、fmt、Clippy 和差异检查通过；全目标失败项与独立复测结果如下。
- [x] 无数据库/API 变化；没有把本地恢复性修复描述为已验证的 FNOS CPU 降幅。

结果（2026-10-07）：预检失败与明确不可用状态现在分开处理。已配置 scraper 的 item 级或整批预检错误会进入现有补全 worker，错误由 worker 记录为 `SCRAPER_UNAVAILABLE` 并使用 LUX-402 退避；明确无 scraper 仍不入队。`cargo test --locked --all-targets --no-fail-fast` 中 library tests 为 750 passed、12 ignored，`scanning_jobs` 81/81、`scanned_metadata` 16/16、`storage` 49/49 通过；全目标的 `emby_counts` 与 `strm` 失败已在干净 `origin/test=8412cc2a` 基线复现，`libraries_api` 全目标并行运行时失败后独立复跑 13/13 通过。`cargo build --locked`、fmt、全目标全 feature Clippy 与 `git diff --check` 通过。该分支未部署到 FNOS，也没有生产 CPU 降幅证据。

补充结果（2026-10-09，阶段 23 无候选分类）：自动搜索所有标题/年份变体都成功返回空结果时，旧逻辑把它转换成 `SCRAPER_UNAVAILABLE`，导致合法的“没有匹配”进入 provider 退避；分集在找不到系列父候选时也会这样误分类。现在空搜索结果作为无候选返回，仍保留真实请求错误的 `SCRAPER_UNAVAILABLE` 分类。新增电影与分集 FILL_MISSING 集成回归，确认 job 不进入 `DEFERRED`、item 记录 `LOW_CONFIDENCE` 且 `candidate_count=0`、没有候选行，并且 title/provider IDs 不变。`cargo test --locked --test reidentify -- --test-threads=1` 在本机 ARM64/SQLite fixture 上 19/19 通过，包含既有 provider unavailable deferred 回归；没有运行 PostgreSQL 专项或性能基准，也未部署 FNOS。

短计划与文件：预计修改 `src/application/scanner.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/LUX-FILL-MISSING-OPTIMIZATION-PLAN.md`。先在 scanner 单测中固定“预检错误继续排队、明确无 scraper 不排队”的决策，再把预检错误送入已有 provider-unavailable job 流程，最后运行窄测试与相关完整检查。

#### LUX-408：合并扫描本地 metadata outbox 批量写入

范围：workflow 3 每 256 个 source 持久化一条 `scan_local_metadata_batches`，保留该任务粒度以限制 worker 的单次载荷和重试范围；但目前同一 Manifest 事务对每条 batch 分别执行 INSERT。60,000 个文件的固定扫描产生 240 条 outbox INSERT，令现有 LUX-275 基准中的 `<209 DML` 检查在干净 `origin/test` 上失败，尽管扫描索引本身仅执行 119 条 DML。将同一事务中多个 outbox batch 合并为有界多行 INSERT，只有遇到已存在的 batch 序号时才执行一次有界读取并校验原 ID、source 列表和计数。每条 outbox 仍最多含 256 个 source；不改变 worker、重试、序号、事务或公开接口。

验收：

- [x] 新 batch 在一个有界多行 INSERT 中写入；故障注入回归确认 Manifest 索引、outbox 和 manifest/job checkpoint 一同回滚。
- [x] 重放相同序号与完全相同 source 列表保持幂等；相同序号但 ID、source 列表或 source 数不同仍返回冲突；SQLite 与 PostgreSQL 均运行覆盖。
- [x] 固定 SQLite 回归对 513 个 source 生成 3 条独立 batch，首次写入只执行 1 条 INSERT；重放使用有界冲突校验，不逐行读取。
- [x] 每条 batch 最大仍为 256 个 source；最多 64 行、384 个绑定参数的多行 INSERT 在 SQLite 与 PostgreSQL 通过。
- [x] `lux_270_manifest_job_scan_benchmark` 的 60k SQLite/PostgreSQL 总 DML 均低于既有 209 阈值；索引、target、无变化重扫、前台 p95、SQL/DML 与 WAL 已记录，不把本机 ARM64 数据外推至 FNOS/x86_64。

短计划与文件：修改 `src/storage/jobs.rs`（批量序列化、INSERT、冲突校验及 SQLite/PostgreSQL query-count 回归）、`tests/scanning_jobs.rs`（Manifest 与 checkpoint 回滚）、`tests/performance.rs`（保持总 DML 门槛）、`docs/LUX-DEVELOPMENT.md` 和 `docs/PERFORMANCE.md`。先写 513-source 回归，再按最多 64 个 outbox row 构造一条多值 SQL、使用 `RETURNING` 识别新写序号，并仅在存在冲突时批量验证。已完成 storage/scanning 定向回归及 PostgreSQL 16 三轮基准；全量 Rust 质量门结果见下。

结果（2026-10-08）：定向 SQLite/PostgreSQL outbox 回归、扫描事务回滚、60k SQLite/PostgreSQL 性能基准、`cargo build --locked`、`cargo fmt --all -- --check` 和全目标全 feature Clippy 通过。`cargo test --locked --all-targets --no-fail-fast` 中 library 为 751 passed、13 ignored，`scanning_jobs` 81/81、`storage` 49/49 通过；全量集成测试仅 `emby_counts` 与 `strm` 两项失败，这两项已在干净 `origin/test` 的 LUX-407 质量门中复现，与本任务改动无关。`git diff --check` 通过。本机为 ARM64；此分支未部署 FNOS，固定 fixture 的语句数改善不代表 NAS CPU 或生产墙钟收益。

#### LUX-409：跳过未变化 NFO 写回的镜像与数据库检查

范围：`NfoWriteService::finish_item_write` 在 NFO 文件内容未改变时仍检查 metadata mirror、重新读取文件时间并访问数据库。未变化时复用原文件状态生成的既有 metadata fingerprint，直接返回，不检查镜像、不重新 stat，也不打开 metadata 写事务；文件发生变化时保留镜像、fingerprint 和状态同步的现有顺序及错误语义。该优化不改变 NFO 内容、文件原子写入、source freshness 或 mirror 的显式配置合同。

验收：

- [x] 首次 probe 写回可创建 metadata mirror 并同步 NFO 状态；重复相同写回不再查询 mirror 策略或写入数据库，NFO 与已有镜像内容保持一致。
- [x] 未变化路径返回的 metadata fingerprint 与此前 stat 路径的 fingerprint 算法和字节完全一致；变化路径仍刷新 fingerprint 并同步数据库。
- [x] `nfo` 定向单测、`cargo fmt --all -- --check`、相关 Clippy 与 `git diff --check` 通过；性能记录只报告固定 fixture 的 storage query-wrapper 调用数，不推断墙钟或生产 CPU。

短计划与文件：预计修改 `src/application/nfo.rs`（无变化回归及快速返回）、`src/application/metadata.rs`（从已读取文件状态构造一致的 fingerprint）、`docs/LUX-DEVELOPMENT.md` 和 `docs/PERFORMANCE.md`。先扩展 probe 写回回归，使其在重复调用时断言 mirror 配置读取和状态写入都被跳过，再实现 fingerprint 复用。

结果（2026-10-08）：重复 probe 写回的 SQLite fixture 从 3 次 storage query-wrapper 调用降为 2 次；首次有内容变化的写入仍创建 mirror 并同步 NFO 状态。NFO writer 单测 4/4、格式检查及 library 全 feature Clippy 通过。重复写回复用写入前已读取的 `FileStamp` 计算原有 metadata fingerprint，单测确认结果与 stat 算法逐字节一致；没有据 SQL 调用数推断墙钟、FNOS CPU、PostgreSQL 或 NAS 收益。

#### LUX-410：合并普通电影 metadata 页的图片登记

范围：`enrich_movie_sources` 目前在每部电影的 NFO 处理后立即单独写入本地图片；改为按最多 16 个电影收集图片记录后复用现有批量图片事务。保持 NFO 处理顺序、图片发现和 fallback 规则、每个条目的文件读取错误隔离；批量事务失败时退回逐条写入，以免一个条目的写入错误阻断同页其他电影。扫描 outbox 路径已有的首张海报时机和 16 项批处理不变。

- [x] 普通电影 metadata 页按最多 16 个 item 合并图片登记；超过上限时切页，且每条图片仍按原有路径、索引和 fallback 规则保存。
- [x] 单个条目的目录/图片错误与批量事务失败继续隔离；能成功的其他条目仍登记，报告的 `images_found` 与原单条路径一致。
- [x] 固定 SQLite 两电影 fixture 的图片写入 query-wrapper 次数从逐项 2 次降为单批 1 次；电影 NFO 和扫描 outbox 图片回归、格式、相关 Clippy 与 `git diff --check` 通过，不推断墙钟或生产 CPU。

短计划与文件：预计只改 `src/application/metadata.rs`、`docs/LUX-DEVELOPMENT.md` 和 `docs/PERFORMANCE.md`。先在 metadata 单测中用两部电影和本地 poster 锁定当前逐项登记 SQL 调用，再在 16 项页内收集共享 helper 的准备结果并批量提交，最后验证单项错误恢复。

结果（2026-10-08）：普通电影 enrichment 逐 16 项收集图片后复用现有有界图片事务，NFO 仍按 item 顺序处理；批次写失败后退回逐 item 事务并隔离错误。两电影本地 fanart fixture 中扫描 source 查询与图片登记共由 3 次 storage query-wrapper 调用降为 2 次；trigger 注入一项写失败时，另一项仍登记成功。新增单测通过，`scanned_metadata` 16/16、`series_metadata` 3/3、fmt、library all-features Clippy 与 `git diff --check` 通过。性能记录仅描述该 SQLite fixture 查询调用数，不外推墙钟或 FNOS CPU。

#### LUX-411：合并电影 NFO source 与辅助字段上下文读取

范围：电影 NFO/probe 写回分别读取 item 类型、preferred source、sort title 和 added_at。新增单 item NFO writeback context 查询，一次返回 target 所需 source/type 与 sort_title/added_at，供电影 NFO target、sorttitle/dateadded 和 probe source 校验复用。保持图片写回使用的通用批量 context 合同不变。保留 MOVIE 限制、首选 source/missing 检查、NFO 路径规则和 source ID freshness；不改变对外 API 或存储 schema。

验收：

- [x] probe 详情写回从上下文查询和辅助字段查询合为一次读；状态同步写入仍保持原事务边界，错误 source、非电影、STRM source 仍拒绝。
- [x] 电影 NFO 写回从 item kind、source、辅助字段多次读取合并为一次 context 读取；`sorttitle`、`dateadded` 内容一致，非电影和软删除条目不注入电影辅助字段。
- [x] NFO writer 与 candidate writeback regression、fmt、相关 Clippy 和 `git diff --check` 通过；性能记录只陈述固定 SQLite query-wrapper 调用数。

短计划与文件：预计改 `src/storage/catalog.rs`、`src/storage/media.rs`、`src/application/nfo.rs`、`docs/LUX-DEVELOPMENT.md` 和 `docs/PERFORMANCE.md`。先把 probe 的 storage 查询调用断言由 4 调至 3 并确认失败，再新增单 item 查询并移除不再使用的辅助字段查询，最后覆盖电影 NFO 的辅助 XML 字段和拒绝路径。

结果（2026-10-08）：电影 NFO 与 probe 写回复用单次 NFO writeback context 查询；条件投影保留旧辅助字段仅对未软删除 MOVIE 生效的语义，source 选择仍限制为原有媒体类型。固定 SQLite fixture 中 probe 写回由 4 次降为 3 次 storage query-wrapper 调用；变化中的电影 NFO 写回总计 3 次（context、mirror policy、状态同步）。定向 NFO 单测 1/1、`nfo_writer` 26/26、candidate 模块 22/22、候选写回失败回归 1/1、fmt、全目标全 feature Clippy 与 `git diff --check` 通过。查询计数不代表 PostgreSQL、墙钟或 FNOS CPU 收益。

#### LUX-412：批量登记普通剧集 metadata 图片

范围：普通剧集 enrichment 已按页读取 episode source，但 series、season、episode 图片仍通过 `index_images` 逐项读取已有记录并登记新记录。保留 NFO 按条目处理和系列目录读取顺序，将本页实际发现的本地图片按最多 16 个 item 聚合，批量读取已登记图片并复用现有批量写事务。保留 Thumb/Fanart 去重、poster fallback 清理、图片索引顺序和单 item 错误隔离；不增加 episode 并发，不改变扫描 worker 的 claim 与调度流程。

验收：

- [x] 两个 series fixture 的普通 enrichment 共用已有图片页查询与登记事务；固定 SQLite storage query-wrapper 调用数下降，图片归属、类型和报告计数正确。
- [x] 空图片、NFO-only 模式和批写失败回退保持原有语义；单条失败不阻止同页其他 item 登记。
- [x] series 图片与 metadata 回归、fmt、library all-features Clippy 和 `git diff --check` 通过；性能记录限定于固定 SQLite fixture。

短计划与文件：只改 `src/application/metadata.rs`、`docs/LUX-DEVELOPMENT.md` 和 `docs/PERFORMANCE.md`。先新增两 series/episode fixture 并锁定逐 item 查询调用数，再按 16 item 边界收集图片候选，复用批量既有图读取和图片登记；最后注入单 item 写失败确认错误隔离。

结果（2026-10-08）：普通剧集 enrichment 按最多 16 个 item 收集 series/season/episode 图片候选，既有图查询与登记共用页级 storage 操作；NFO 仍逐条处理。两 series、各一个 episode、每 series 一个 poster 的固定 SQLite fixture 从 11 次降为 4 次 query-wrapper 调用；其中单次批登记事务同时 upsert 两张 poster 并清除 poster fallback。trigger 注入首个 series 写失败后批事务回滚，逐项恢复仍登记第二个 series，并只将失败 series 记录为错误。metadata module 13/13、`scanned_metadata` 16/16、`series_metadata` 3/3、fmt、library all-features Clippy 与 `git diff --check` 通过。此数据只反映 fixture 调用数，不代表 PostgreSQL、墙钟或 FNOS CPU 收益。

#### LUX-413：避免已知缺失 credits 时读取人物关系文件

范围：本地 metadata completeness 计划对电影和系列总会检查 `people.json`，之后才判断 NFO projection 是否缺导演或编剧。若任一 crew 字段已缺失，credits 必然需要补全，人物关系是否存在不改变结果；先检查 crew completeness，仅在两类本地 crew 信息均齐全时读取 actor relation。保持自动补全能力与 NFO projection 语义不变，不新增跨请求缓存或并发。

验收：

- [x] 缺少 NFO projection、导演或编剧信息时仍标记 credits 缺失，且不进入 actor-relation 检查分支；两类 crew 信息齐全时继续检查 actor relation。
- [x] 候选计划回归、fmt、相关 Clippy 和 `git diff --check` 通过；不把分支跳过描述成已测得的总体 I/O 或 FNOS CPU 收益。

短计划与文件：只改 `src/application/candidates.rs`、`docs/LUX-DEVELOPMENT.md` 和 `docs/PERFORMANCE.md`。先增加 crew completeness 判定回归，再让计划构建仅在 crew 信息完整时读取 actor relation，最后运行候选模块测试、fmt、Clippy 和差异检查。

结果（2026-10-08）：credits 缺失判定先检查本地 NFO projection。缺少 projection、导演或编剧时直接标记缺失，不读取 `people.json`；两类 crew 信息齐全时仍检查人物关系。候选模块回归、fmt、Clippy 和 `git diff --check` 通过。本任务没有统计文件系统调用数、墙钟或生产 CPU；完整的页级 NFO projection 缓存与批量 relation 读取仍属于第 15 项剩余工作。

#### LUX-414：本地 metadata 图片完成时唤醒对应扫描等待者

范围：`wait_for_local_metadata_images` 当前按 1 秒 fallback 轮询数据库，即使本进程中的 outbox worker 已持久化对应 job 的图片阶段完成。为等待中的 job 注册独立 Notify；图片 worker 成功持久化 `images_completed_at` 后唤醒该 job 的等待者。保留周期 fallback，覆盖跨进程状态变化、通知注册竞态和 worker 恢复；失败重试不误报完成。

验收：

- [x] 图片阶段完成的持久化状态后发送对应 job 通知，等待者收到通知后重新读取数据库并退出等待；失败图片批次继续按原有重试策略等待。
- [x] 回归证明通知可让等待路径早于 1 秒 fallback 返回；fmt、相关 Clippy、`git diff --check` 通过。
- [x] 性能记录仅说明查询等待边界，不外推文件处理墙钟、PostgreSQL、FNOS 或 NAS 收益。

短计划与文件：只改 `src/application/scanner.rs`、`docs/LUX-DEVELOPMENT.md` 和 `docs/PERFORMANCE.md`。先添加包含未完成 image batch 的 scanner 回归并观察 1 秒轮询超时，再增加独立 job 通知注册和完成信号，最后验证扫描等待与批次重试行为。

结果（2026-10-08）：outbox 图片阶段成功写入 `images_completed_at` 后通知对应 job；等待方收到通知后重新查询数据库，周期 fallback 仍为 1 秒。通知单测先保持 batch 为 pending，再完成它并要求 250ms 内唤醒（不含开始时的 50ms 等待）；定向回归、fmt、全目标全 feature Clippy 与 `git diff --check` 通过。失败批次不发送完成通知，仍按原有延迟重试。本结果不测量 SQL 时延、扫描墙钟或 FNOS CPU。

#### LUX-415：本地 metadata worker 仅在 fallback 时刷新 scan job

范围：本地 metadata worker 每次收到 target 状态 Notify 后都会重新读取 scan job，即使 scan job 生命周期由同一个 worker 的 stop watch 控制，且 target 通知已由本地 scan service 发出。改为 Notify 后直接回到 pending-target 查询，仅 1 秒 fallback 重读 job 行，以发现外部状态变化或修复丢失通知；stop watch 仍立即退出。不改变 pending target claim、root 校验、批次大小、worker 状态机或数据库合同。

验收：

- [x] 固定 SQLite worker idle 回归确认一次 Notify 唤醒只执行 pending-target 查询，从原来的两次 query-wrapper 调用降为一次。
- [x] scan job fallback 刷新和 stop watch 行为保持；本地 metadata worker 定向回归、fmt、Clippy 和 `git diff --check` 通过。
- [x] 性能记录只说明 query-wrapper 调用数，不外推 PostgreSQL、墙钟或生产收益。

短计划与文件：只改 `src/application/scanner.rs`、`docs/LUX-DEVELOPMENT.md` 和 `docs/PERFORMANCE.md`。先让 idle worker 在通知回归中记录 query-wrapper 调用并观察现有 2 次读取，再仅在 fallback 分支刷新 job，最后验证 worker 通知、取消退出与现有扫描回归。

结果（2026-10-08）：idle worker 收到 target Notify 后不再查询 scan job；仍会回到 pending-target 查询，且只有 1 秒 fallback 会检查最新 scan job 状态。SQLite idle fixture 在一次 Notify 后的 query-wrapper 调用由 2 降为 1；stop watch、pending 状态及 scan job 持久化语义未变。候选 worker 与图片等待回归、fmt、全目标全 feature Clippy 和 `git diff --check` 通过；不推断 PostgreSQL、墙钟或 FNOS 收益。

#### LUX-416：用 NFO 内容指纹跳过无语义变化的重复解析

范围：数据库的 metadata fingerprint 仍使用路径、大小和 mtime 作为快速 stat 指纹。stat 指纹变化时，若 rich NFO cache 保存的原始内容 SHA-256 与当前文件字节一致、NFO defaults 已完整且 actor relation 不需重建，则跳过 XML 解析和完整 metadata 更新，只把新的 stat 指纹写回；内容哈希不同或任何前置条件不满足时保留原解析/修复路径。不尝试仅凭 mtime/大小抑制外部写入，不改变缓存 schema、字段优先级或 actor relation 恢复。

验收：

- [x] 同一 XML 内容位于新旁车路径时命中 rich cache，不解析、不重复更新完整 metadata，并持久化新 stat fingerprint。
- [x] 相同流程中随后修改 NFO 标题会重新解析并更新媒体标题；缓存不屏蔽内容变化。
- [x] NFO cache、metadata 模块定向回归、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。
- [x] 性能记录说明仍要读取并哈希旁车字节；本任务只减少 XML 解析和 metadata 更新，不外推墙钟或 FNOS 收益。

短计划与文件：修改 `src/storage/catalog.rs`（同查询读取 cache JSON 与 SHA-256）、`src/application/nfo.rs`（validated cache snapshot API）、`src/application/metadata.rs`（保守内容命中及测试）、`docs/LUX-DEVELOPMENT.md` 和 `docs/PERFORMANCE.md`。先添加 same-content/new-path 回归并确认旧逻辑仍加载 NFO，再在 rich cache 指纹匹配时跳过解析；最后同时验证内容变化路径。

结果（2026-10-08）：本轮已读取 metadata 快照后，在本地 NFO 文件字节与 rich cache 内容 SHA-256 一致且 defaults 完整、actor relation 正常时复用缓存并只同步新的路径/大小/mtime 指纹。旧实现下 new-path/same-content 回归的 `nfo_skipped` 为 0；按旧路径 6 次 query-wrapper 调用计算，新路径执行 3 次，且随后修改标题仍执行 NFO 加载。cache JSON 与 SHA-256 现在通过一条 query-wrapper 查询同时读取，原 unchanged stat 命中 fixture 也从 3 次降为 2 次。保守路径仍需读取全部 NFO 字节并计算 SHA-256，因此没有消除 NAS 文件读取；不外推 PostgreSQL、墙钟、FNOS 或 CPU 收益。

#### LUX-417：completeness 页复用 NFO projection 的 writeback context

范围：`local_metadata_completeness_plans` 已批量读取每页 item 的 writeback context，图片策略消费该 context，但 rich NFO cache 缺失时 projection fallback 仍逐 item 查询 item kind 和 source path。让 NFO projection 读取复用同页 context，并从当前页 metadata 快照取 SEASON 必需的 season number。继续 canonicalize source、media file、目标目录并验证都位于配置 root 内；context 缺少可写回 source 时保持 ItemNotFound 语义。单 item completeness 规划继续使用既有按需读取路径。

验收：

- [x] metadata 页中 NFO projection 文件读取复用已批量加载的 writeback context，不再对每个 item 重查 kind/source；单 item 入口仍正常读取。
- [x] MOVIE、EPISODE、SERIES、SEASON 的目标路径与根目录保护保持，SEASON 使用相同编号选择规则；缺失 source 语义不变。
- [x] query-wrapper 回归、NFO/candidate/scanner 定向测试、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。
- [x] 性能记录只报告 context SQL 调用数，不把 canonicalize 或 NAS 文件 I/O 推断为已消除。

短计划与文件：修改 `src/application/nfo.rs`（按上下文读取 projection）、`src/application/candidates.rs`（completion 页复用）和两份优化记录。先写 context/projection 查询计数回归，再连接页内消费，最后跑 NFO 与 candidate 相关检查。

结果（2026-10-08）：projection 查询计数回归确认，复用页级 writeback context 后不再执行逐 item kind/source SQL，projection 阶段由 2 次 query-wrapper 调用降为 0；NFO 文件读取及 canonicalize/root 校验保持。`nfo_writer` 26/26、metadata candidate 单测 22/22、`metadata_selection` 31/31、`scanning_jobs` 81/81、`cargo build --locked`、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。本机 `uname -m=arm64`。`cargo test --locked --all-targets` 的 library 测试 758 passed、13 ignored，随后在 `tests/emby_counts.rs:159` 遇到实际计数 1、期望 0；在未包含 LUX-417 改动的 `2b64ba65` 隔离工作树复跑同一目标得到相同失败，因此记为已有基线失败而非本任务回归。仅报告本地 SQLite 查询边界，不外推文件系统、PostgreSQL、NAS 或 FNOS 收益。

#### LUX-418：电影本地 NFO 有界并发

范围：普通电影 metadata enrichment 已按 16 个 item 分页并批量登记图片，但 NFO 仍逐条执行。对每页独立电影 NFO 使用最多 4 个并发任务，保持结果按来源顺序归并、item 错误隔离、report 计数以及现有图片批事务/单 item 失败回退。不扩展到 series/episode NFO、在线刮削、人物写入或调度器。

验收：

- [x] 有自动化回归证明任务并发数不超过 4、输入结果顺序稳定且所有 item 均被处理。
- [x] 每个电影 NFO 的错误仍只标记该 item；图片批登记和失败回退行为保持。
- [x] metadata/scanned_metadata 定向目标、fmt、全目标全 feature Clippy 与 `git diff --check` 通过。
- [x] 性能记录只描述本地 worker 并发上界，不把 SQL 并发或 fixture 推断为 FNOS CPU/墙钟收益。

短计划与文件：修改 `src/application/metadata.rs`，使用可测试的有界 task helper 驱动电影 NFO page；修改 `docs/LUX-DEVELOPMENT.md` 与 `docs/PERFORMANCE.md` 记录范围及验收。先写并发上限/顺序回归，再替换逐条 NFO loop，最后验证图片失败隔离与目标测试。

结果（2026-10-08）：普通电影每页最多 4 个 NFO task 并行，任务完成后再补充下一项，最终按输入顺序合并 report；坏 NFO 只使对应电影失败，健康电影仍更新，图片 page transaction 和逐 item 失败回退保持。metadata 单测 13/13、`scanned_metadata` 16/16、`cargo build --locked`、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。本轮 `cargo test --locked --all-targets` 的 library 测试 760 passed、13 ignored，随后 `tests/emby_counts.rs:159` 再次出现已在未改动基线 `2b64ba65` 复现的实际计数 1、期望 0。性能证据仅确认本地并发上限与结果顺序，没有墙钟、FNOS CPU、PostgreSQL 或 NAS A/B 数据。

#### LUX-419：剧集与分集本地 NFO 有界并发

范围：普通剧集 metadata 页按 series/season/episode 顺序遍历以保留层级图片语义，但各层 NFO enrichment 也在该循环中串行执行。收集当前 source page 中实际存在的 series、season、episode NFO request，再复用 LUX-418 的最多 4 路有界 task helper；保留 source 顺序归并、NFO snapshot metadata 不重复查询、Item 级错误隔离与 `tvshow.nfo`/season/episode 文件选择规则。目录/NFO 路径发现、图片扫描、人物 credits 跨 item 批量写入不属于本任务。

验收：

- [x] 当前 page 中层级 NFO request 最多并发 4 个，按 series/season/episode 原顺序归并统计；没有 NFO 的 item 不创建 task。
- [x] snapshot metadata 仍被消费一次且不触发逐 item metadata 查询；单 item OnDemand NFO 路径不变。
- [x] hierarchy、season/episode NFO 路径、NFO failure isolation、series/scanned-series metadata 定向测试、fmt、全目标全 feature Clippy 与 `git diff --check` 通过。
- [x] 性能记录仅报告 task 上界，不外推墙钟、NAS 或 FNOS 收益。

短计划与文件：修改 `src/application/metadata.rs` 收集拥有型 series NFO request 并以 LUX-418 有界 helper 执行；修改 `docs/LUX-DEVELOPMENT.md` 与 `docs/PERFORMANCE.md`。先覆盖 request/snapshot 消费语义，再连接当前 page 执行，最后运行 `series_metadata`、`scanned_series_metadata` 和候选测试。

结果（2026-10-08）：series、season、episode NFO 路径按层级遍历收集后使用与电影相同的 4-task 上限；snapshot metadata 随 request 一次取出，仍走无额外 metadata read 的 enrichment 入口，OnDemand 调用保留原入口。层级/image traversal 未移动，NFO requests 按其被发现的源顺序汇总 report。`series_metadata` 3/3、`scanned_series_metadata` 2/2、metadata task-order/concurrency 与 NFO isolation 回归、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。性能记录只陈述并发上限，没有墙钟、query 计数、FNOS 或 NAS 收益数据。

#### LUX-420：降低本地 metadata 完成等待 fallback 轮询

范围：扫描 job 等待本地 metadata target 完成时，每秒 fallback 会重新查询 pending target，即使同进程 worker 完成路径会发送 `Notify`。将此 completion waiter 的 fallback 独立调至 5 秒；通知到达仍立即检查数据库。worker 对 scan job 状态的 1 秒 fallback 保持不变，图片阶段等待也保持原有间隔。跨进程变更最迟通过 fallback 被观察到。

验收：

- [x] 同进程 metadata completion 通知能在 250ms 内唤醒等待者，不等待 5 秒 fallback。
- [x] fallback 常量仅应用于 completion waiter，worker 状态刷新与 image waiter 的容错间隔未改变。
- [x] scanner waiter 与 scanning_jobs 定向回归、fmt、全目标全 feature Clippy 与 `git diff --check` 通过。
- [x] 性能记录区分 waiter fallback 频率和 worker refresh 频率，不推断生产 CPU 收益。

结果（2026-10-08）：completion waiter pending-state 查询从每秒 fallback 降至每 5 秒一次；本进程 worker 的 `notify_waiters` 仍即时唤醒。新增 fixture 在 target 由 PENDING 变为 DONE 后触发 Notify，250ms 内返回；image waiter 原 250ms 通知回归也通过。`scanning_jobs` 81/81、全目标全 feature Clippy、fmt 和 `git diff --check` 通过。跨进程写入的检测延迟窗口扩大至最多 5 秒，性能记录不外推 CPU、PostgreSQL 或 FNOS 收益。

#### LUX-421：合并 completeness 完成后的缺失项筛选

范围：`complete_local_metadata_and_enqueue_fill_missing_with_policy` 在同一事务中完成 completeness 状态后，原先先查询 READY/missing 项，再查询仍有效的媒体项，最后将两组结果求交后交给请求构建器；请求构建器本身已按同样条件筛选 completeness 和媒体项。删除这两次冗余筛选，只把去重后的 eligible IDs 交给批量请求构建器。保留 completeness 更新、并发锁、事务提交、自动补全策略以及目标 eligibility 规则。

验收：

- [x] 固定 SQLite fixture 的应用 query-wrapper 调用从 9 次降为 7 次；对应回归精确断言候选路径调用数。
- [x] 请求构建器仍限定当前 library、未软删除、有效媒体类型、READY 且 missing 的 capability，并按输入 fingerprint 生成请求。
- [x] storage 定向测试、build、fmt、全目标全 feature Clippy 和 `git diff --check` 通过；不把 query-wrapper 数量解释为 SQL 执行时长或生产收益。

预计文件：`src/storage/metadata.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先测出旧路径 9 次查询，再去除重复读取并确认新路径为 7 次。

结果（2026-10-08）：旧版隔离基线与候选版各自运行同一 storage fixture，query-wrapper 调用为 9→7。请求构建器的事务内查询继续复核有效媒体项和 READY/missing 状态；本机 `arm64`。只证明固定 SQLite fixture 的应用层调用数变化，不外推 SQL 时长、PostgreSQL、FNOS 或 NAS 收益。

#### LUX-422：有界并行规划本地 metadata completeness 页

范围：完整性规划页已批量读取策略、图片索引、NFO writeback context 和 attempts，但每个 item 的本地图片发现、必要时的 NFO projection 与 actor relation 检查仍串行执行。对只读 item planning 使用最多 4 路并发，收齐后按输入顺序生成计划；保留 attempt state 批量读取、请求 eligibility、最先失败项语义和现有文件/数据库读取边界。不引入文件读取缓存、schema 或依赖。

验收：

- [x] 固定 12 个异步 planning task 的回归证明并发数大于 1 且不超过 4，返回结果按输入顺序稳定。
- [x] image discovery、NFO projection 或 actor relation 检查错误仍导致该页规划失败，并按输入顺序选择首个错误；这些操作保持只读。
- [x] 候选模块及相关扫描回归、build、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。
- [x] 性能记录只说明页内只读 task 并发上限；不声称减少文件读取次数或已验证墙钟/FNOS/NAS 收益。

预计文件：`src/application/candidates.rs`、`src/storage/repository.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先锁定有界 helper 的并发与稳定顺序，再将完整性页 per-item planning 移入 helper。

结果（2026-10-08）：每页 NFO projection、人物 relation 旁车和本地图片发现最多 4 个 item 并行，所有结果收齐后按原输入顺序处理；策略/图片/context/attempts 查询仍为页级数据库调用。候选模块测试 23/23 通过。该回归只证明任务上界和顺序，不测文件访问总数、墙钟或 FNOS/NAS 性能。

#### LUX-423：降低本地 metadata worker 的空闲 job 状态查询

范围：本地 metadata worker 没有 pending target 时通过 Notify 等待，同时每秒 fallback 重新读取 scan job。只将该 job-state refresh fallback 改为 5 秒；Notify 和 stop watch 仍即时唤醒。worker 启动加载失败重试、image-stage waiter 的 fallback 保持 1 秒，completion waiter 保持 LUX-420 的 5 秒。跨进程 scan job 状态变化的观察窗口扩大至最多 5 秒。

验收：

- [x] job-state refresh fallback 为 5 秒且没有改动其他 idle/retry/image fallback。
- [x] Notify 到达后仍无需等待 fallback，并保持现有单次通知 query 回归。
- [x] scanner waiter 定向测试、build、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。
- [x] 性能记录仅报告配置间隔 1 秒→5 秒；不推断实际运行时查询量、CPU 或 FNOS 收益。

预计文件：`src/application/scanner.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先锁定新 fallback 配置和即时 Notify 路径，再只替换 scan job 状态 refresh timer。

结果（2026-10-08）：worker 空闲时的 scan job 状态 refresh 从 1 秒 fallback 改为 5 秒；通知和 stop watch 仍即时唤醒，worker 启动读取错误及 image-stage waiter 的 1 秒 fallback 未改变。`local_metadata_worker_` 定向测试 4/4、`cargo build --locked`、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。只确认 fallback 配置和通知语义，未测总体 query/CPU 或 FNOS 收益；跨进程状态观察最迟延至 5 秒。

#### LUX-424：扫描生成的 FILL_MISSING job 使用按 library 复用的 dispatcher

范围：扫描后处理当前会为每个 `FILL_MISSING` job 建立独立 Tokio task。增加按 library 复用的持久 dispatcher，以有界队列接收 job ID，并由单个 runner 按序处理同一 library 的 job；队列满时对提交方施加背压，已持久化的 job 不得丢弃。将全量扫描、增量扫描和 completeness completion 生成的补全任务接入 dispatcher，保持数据库 job 生命周期、扫描事件、取消语义和现有全局最多两个 `FILL_MISSING` item worker 不变。dispatcher 关闭后拒绝新提交；服务关闭调用不等待队列执行完，已接收任务在 Tokio runtime 仍运行期间继续排空。

验收：

- [x] 同一 library 同时最多运行一个 `FILL_MISSING` job；另一 library 可有独立 dispatcher，item worker 总并发仍不超过 2。
- [x] dispatcher 队列有明确容量，队列满时提交受背压；重复 job ID 不重复排队，失败提交不会遗失数据库中的 job。
- [x] 全量扫描和增量扫描的自动补全入口均提交到 dispatcher，不再为每个 job 建立完整 runner task；扫描 job 事件内容保持一致。
- [x] 关闭后拒绝新提交；关闭唤醒队列容量等待者，已接收队列在 runtime 仍运行期间排空，关闭调用不等待长 job；取消中的 job 按原语义完成收尾。
- [x] 同 library 串行、不同 library 可调度、队列背压、重复提交、错误 library 路由、关闭唤醒和单个 job runner panic 后继续处理有自动化覆盖；定向 reidentify/scanning jobs、build、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。
- [x] 调度入口限于扫描和本地 completeness 自动补全；管理员直接请求和计划任务作为独立后续接线任务，不得绕过 dispatcher 扩大 runner 数量。
- [x] 性能记录只报告队列容量、runner 数和已有 item worker 上限，不推断运行时 CPU、数据库耗时或 FNOS/NAS 收益。

预计文件：`src/application/reidentify.rs`、`src/application/scanner.rs`、`tests/reidentify.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先添加同库串行调度回归，再接入有界 dispatcher 并验证关闭行为。

结果（2026-10-08）：扫描和本地 completeness 自动补全共用按 library 的容量 32 dispatcher；enqueue 先预留容量再在 shutdown 锁内去重，校验 job 的持久化 library/mode，关闭唤醒背压提交，单个 job runner panic 后将该 job 标记为 FAILED 并继续队列。`tests/reidentify.rs` 15/15、`tests/scanning_jobs.rs` 81/81、build、fmt、全目标全 feature Clippy、`git diff --check` 通过。全量测试的 library 部分 765 passed、13 ignored；随后在 `tests/emby_counts.rs:159` 遇到已于基线 `2b64ba65` 复现的无关计数失败（实际 1、预期 0）。本机 ARM64；没有 FNOS/NAS、PostgreSQL 或运行时 task 数/CPU A/B 证据。管理员和计划任务入口仍属后续任务。

#### LUX-425：管理员 FILL_MISSING 请求复用按 library dispatcher

范围：管理员的整库 reidentify、整库 metadata refresh 和单条目 metadata refresh 中，`FILL_MISSING` 仍直接为每个持久化 job spawn runner。统一通过 LUX-424 dispatcher 接收这三类管理员创建的 `FILL_MISSING` job；`FULL_REFRESH` 与 `REIDENTIFY` 继续沿用当前 runner。队列背压时管理员请求等待 dispatcher 容量；dispatcher 关闭或队列不可用时返回结构化服务不可用错误，持久化 job 保持可重试；成功响应、审计事件和 job DTO 不变。本任务不接计划任务、thumbnail retry 或调度窗口。

验收：

- [x] 三个管理员 FILL_MISSING 入口都进入对应 library dispatcher，不再直接 spawn job runner。
- [x] FULL_REFRESH、REIDENTIFY 的执行与响应合同不变；队列关闭时 FILL_MISSING 返回服务不可用，持久化 job 保持可重试。
- [x] 管理员 item/library FILL_MISSING HTTP 回归、reidentify dispatcher 定向回归、build、fmt、Clippy 和 diff check 通过。
- [x] 性能记录说明管理员路径复用每库一个 runner，不推断生产吞吐或 CPU 收益。

预计文件：`src/api/admin_handlers.rs`、`tests/reidentify.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先补管理员 refresh HTTP 回归，再接入 dispatcher。

结果（2026-10-08）：整库 reidentify、整库 metadata refresh 和 item metadata refresh 的 `FILL_MISSING` 均经由按 library dispatcher 入队；其他模式继续使用原 worker。dispatcher 关闭回归确认 API 返回 `503 DATABASE_UNAVAILABLE`，新建 job 保持 `QUEUED`。`tests/reidentify.rs` 15/15、`cargo build --locked`、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。本机 ARM64；不代表 FNOS/NAS、PostgreSQL 或运行时吞吐/CPU 收益。管理员显式 retry 入口和计划任务入口不属于本任务，仍需逐项核验。

#### LUX-426：管理员重试 FILL_MISSING job 复用 library dispatcher

范围：管理员 `POST /api/v1/admin/metadata/reidentify/{job_id}` retry 当前在将终态 job 重排为 `QUEUED` 后，直接为每个 job spawn runner。对 `FILL_MISSING` 重试复用 LUX-424 library dispatcher；`FULL_REFRESH` 与 `REIDENTIFY` 保持原 runner。dispatcher 关闭/不可用时返回结构化 `503 DATABASE_UNAVAILABLE`，重排后的 job 留在 `QUEUED` 供之后重试；成功响应和审计事件保持不变。不包含计划任务入口。

验收：

- [x] 管理员 FILL_MISSING retry 通过 job 持久化 library 进入 dispatcher；不得直接 spawn 完整 runner。
- [x] dispatcher 关闭时 retry 返回结构化服务不可用，持久化 job 保持可重试状态；其他模式的 retry 合同不变。
- [x] HTTP 回归、reidentify dispatcher 定向回归、build、fmt、Clippy 和 diff check 通过。
- [x] 性能记录说明 retry 复用每库 dispatcher，不推断生产收益。

预计文件：`src/api/admin_handlers.rs`、`tests/reidentify.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先添加 dispatcher 已关闭时的 FILL_MISSING retry HTTP 回归，再复用 LUX-425 调度 helper。

结果（2026-10-08）：管理员显式 retry 在 job 重排后复用 LUX-425 调度 helper；dispatcher 关闭时 HTTP 返回 `503 DATABASE_UNAVAILABLE`，job 留在 `QUEUED`。新回归在修复前观察到原 handler 错误返回 `202`，修复后通过。`tests/reidentify.rs` 15/15、build、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。本机 ARM64；没有 FNOS/NAS 或运行时收益证据。计划任务入口仍需独立接入与验证。

#### LUX-427：计划 metadata job 复用 library dispatcher

范围：`METADATA_PARSE` 计划任务当前为每个新建的 `FILL_MISSING` job 直接 spawn runner。改为通过 LUX-424 library dispatcher 入队，并将 dispatcher 失败作为计划任务启动错误返回，同时保留已创建的 `QUEUED` job。计划任务成功结果仍返回相同 job DTO。thumbnail scraper retry 的等待完成语义不属于此任务。

验收：

- [x] 计划 metadata job 进入对应 library dispatcher，不直接 spawn 完整 `FILL_MISSING` runner。
- [x] dispatcher 不可用时计划任务返回错误，job 保持 queued；可用时仍返回同一 metadata job 结果。
- [x] scheduled task 与 reidentify 定向回归、build、fmt、Clippy 和 diff check 通过。
- [x] 性能记录说明计划入口复用每库 runner，不推断生产收益。

预计文件：`src/application/scheduled_tasks.rs`、`src/api/admin_handlers.rs`、`tests/scheduled_tasks.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先添加 dispatcher 关闭时的计划任务回归，再接入 dispatcher 并保留既有返回值。

结果（2026-10-08）：`METADATA_PARSE` 计划任务通过 library dispatcher 入队；调度错误使用独立 `MetadataDispatch` 类型并映射成服务不可用。可用路径返回原 metadata job，关闭路径返回错误且 job 留在 `QUEUED`。修复前回归观察到关闭后仍返回成功，修复后通过。`tests/scheduled_tasks.rs` 6/6、`tests/reidentify.rs` 15/15、build、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。本机 ARM64；没有生产吞吐或 FNOS/NAS 性能证据。thumbnail scraper retry 仍需单独处理其等待语义。

#### LUX-428：thumbnail scraper retry 复用 dispatcher 并等待完成

范围：到期的 thumbnail scraper retry 会创建 `FILL_MISSING` job 后直接 `metadata.run()`，再检查 poster/thumb 是否仍缺失。改为提交到 LUX-424 library dispatcher；独占入队使用完成通知，job 被合并且没有新的通知接收端时使用低频状态 fallback 等到该 job 终态，再执行原图片检查。dispatcher 不可用时释放 thumbnail retry lease、保留原尝试次数并记录错误；不提前推进退避次数。

验收：

- [x] thumbnail retry 不再绕过 dispatcher；图片缺失检查只在对应 FILL_MISSING job 终态后执行。
- [x] dispatcher 入队失败释放 retry lease，保持可恢复状态且不增加 attempt count；失败隔离与原截图 fallback 不变。
- [x] thumbnail/scheduled task 定向回归、reidentify 回归、build、fmt、Clippy 和 diff check 通过。
- [x] 性能记录限定于 dispatcher 路由及等待语义，不推断生产收益。

预计文件：`src/application/scheduled_tasks.rs`、`tests/scheduled_tasks.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先增加 dispatcher 关闭时的到期 thumbnail retry 回归，再接入完成通知与 duplicate-job fallback。

结果（2026-10-08）：thumbnail retry 现经 library dispatcher 提交 FILL_MISSING；独占队列项等待 dispatcher 完成通知，合并项以 1 秒 fallback 读取 job 终态。关闭 dispatcher 时释放 retry lease、保留原 attempt count，job 保持可重试的 QUEUED。测试分别验证拒绝入队的恢复状态，以及 provider 被 gate 时不提前消费 attempt、job 完成后才继续图片缺失检查。旧实现的关闭回归曾观察到 3 秒后 retry 和 metadata job 仍 RUNNING。`tests/scheduled_tasks.rs` 8/8、`tests/reidentify.rs` 15/15、build、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。本机 ARM64；无生产吞吐或 FNOS/NAS 性能证据。

#### LUX-429：合并空闲窗口内到达的 FILL_MISSING 请求

范围：LUX-424 至 LUX-428 已将补全 job 接入按 library dispatcher，但每个 job 创建后立即被 runner claim，短时间内连续到达的小批次难以合并。dispatcher 从空闲状态收到首个 job 后增加 1 秒聚合窗口，让同库请求仍可并入 QUEUED job；已有积压时后续 job 立即按序执行，不逐项叠加窗口。RUNNING job 的输入保持不变，不改变 job 持久化合同、每库顺序或全局 item worker 上限。

验收：

- [x] dispatcher 空闲时首个 job 在 1 秒窗口中保持 QUEUED，同库后续请求能复用该 job，且不重复排入同一 job ID。
- [x] dispatcher 已有积压时，首个 job 完成后立即执行下一个 queued job，不再等待额外聚合窗口；队列为空后新一轮请求才开启窗口。
- [x] 自动化回归覆盖 storage 合并、claim 时序及积压执行；`tests/reidentify.rs`、build、fmt、Clippy 和 `git diff --check` 通过。
- [x] 性能记录说明窗口只降低空闲后短批请求创建新 job 的机会；不推断真实任务批次、SQL、CPU 或 FNOS/NAS 收益。

预计文件：`src/application/reidentify.rs`、`tests/reidentify.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先添加实际 storage 合并回归，再实现 dispatcher 空闲窗口，并用定向 runner 测试确认积压 job 不逐项延迟。

结果（2026-10-08）：library dispatcher 在空闲后 claim 首个 `FILL_MISSING` job 前等待 1 秒；期间新请求可并入同一个 QUEUED job，重复 ID 继续复用原 completion 通知。队列已积压时不逐 job 增加延迟，队列再次排空后新请求重新获得窗口。`tests/reidentify.rs` 16/16、对应 dispatcher library 单测通过；build、fmt、全目标全 feature Clippy 和差异检查通过。本机架构为 ARM64。`cargo test --locked --all-targets` 的 library 部分 766 passed、13 ignored，随后在既有 `tests/emby_counts.rs:159` 失败（实际 1、预期 0；此前 LUX-424 记录已在基线 `2b64ba65` 复现）。未部署 FNOS，也没有生产 job 聚合、吞吐、SQL 或 CPU 测量。

#### LUX-430：消除扫描 run 等待的短间隔轮询

范围：删除媒体库时等待进程内 active scan run 当前每 10ms 检查一次共享 registry；改为 guard 结束时通知，并在等待前注册通知后再复核活动状态，避免丢失唤醒。扫描 job 等待增量 job 或 manifest materialization 的数据库条件仍需跨进程观察，将固定 10ms query loop 改为 10ms 起步、最高 250ms 的有界指数退避；取得 scan semaphore 后的条件复核和重试保留。删除超时、library fence、全量/增量优先关系及跨进程最终可见语义不变。

验收：

- [x] Library deletion 等待 active run 使用 race-safe Notify；guard 释放后立即重新检查，仍保持 30 秒总超时及拒绝新 run 的 fence。
- [x] scan-lock 数据库条件等待按 10ms、20ms、40ms、80ms、160ms、250ms 退避并封顶；scan permit 获取后的状态复核不变。
- [x] 自动化覆盖 backoff 边界和删除等待/新 run fence；scanner/scanning jobs 定向回归、build、fmt、Clippy 与差异检查通过。
- [x] 性能记录只描述轮询/回退间隔与跨进程观察上限，不推断生产 CPU、FNOS 或 NAS 收益。

预计文件：`src/application/scanner.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先新增 backoff 边界回归并加强现有 deletion/active-run 测试，再接入 race-safe Notify 与有界数据库退避。

结果（2026-10-08）：active run 的最后一个 guard 移除 registry 项后通知等待者；删除等待先注册并启用通知，再复核活动项，避免通知早于 await 时丢失唤醒。原 30 秒超时和 deletion fence 保留。数据库条件等待采用 10、20、40、80、160、250ms 并封顶，拿到 scan semaphore 后仍重新读取优先级条件。退避边界单测、删除 active run/fence 单测、`tests/scanning_jobs.rs` 删除 worker 回归和 `tests/library_deletion.rs` 通过；build、fmt、Clippy 与 `git diff --check` 通过。本机架构 ARM64；记录的 250ms 是进程外数据库变化的退避观察窗口上限，不表示生产 CPU、FNOS 或 NAS 收益。

#### LUX-431：降低人物 manifest 文件锁争用轮询

范围：`acquire_exclusive_file_lock` 遇到已有锁时固定每 10ms 轮询，最多 100 次。改为单调时钟控制的 1 秒总等待期限，以及 10ms、20ms、40ms、80ms、160ms、250ms 后封顶的有界退避；stale lock 检查、跨进程 `create_new` 互斥语义、成功后的锁内容和错误类型保持不变。该文件锁允许其他进程持有，不能依赖进程内 Notify。

验收：

- [x] 自动化覆盖退避序列、封顶和仍被占用的锁在期限内返回 TimedOut；未占用锁仍可立即获取。
- [x] 人物 relation/manifest 写入及管理 API 定向回归通过；不改变 stale-lock 清理阈值或跨进程锁格式。
- [x] 性能记录仅报告最长等待与检查间隔，不外推文件系统墙钟、生产 CPU、FNOS 或 NAS 收益。

预计文件：`src/application/people/helpers.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先增加 backoff 序列回归，再替换固定 100 次循环并执行人物关系定向验证。

结果（2026-10-08）：人物 relation/manifest 文件锁等待改用 1 秒单调截止时间及 10、20、40、80、160、250ms 后封顶的退避；未占用锁仍立即通过 `create_new` 获取，stale lock 仍按 300 秒阈值处理。helper 边界和锁超时/立即获取单测、`tests/people_api.rs` 10/10、person relation 持久化与 NFO fingerprint 单测通过；build、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。本机架构 ARM64；没有文件系统基准、FNOS/NAS 或生产 CPU 测量。

#### LUX-432：校验人物 relation 与 credits 索引 revision 一致

范围：NFO actor 同步先原子写 `people.json`，再替换数据库 `person_credits` 与 `person_index_item_state`。如果文件写入成功而数据库事务失败，当前 `item_actor_relation_is_current` 仅比较旁车 fingerprint，会把不完整持久化误判为完成，后续扫描跳过 credits 恢复。只有旁车 fingerprint 与请求一致、且数据库 person-index item state 也记录相同 fingerprint/schema 时才返回 current；无数据库的 standalone `PeopleService` 保持原来的旁车检查语义。不改变文件格式或数据库 schema。

验收：

- [x] 回归覆盖 relation 文件已是当前 fingerprint、credits/index state 缺失时判为 stale；重试持久化后恢复 current。
- [x] 已有 NFO relation revision、people API 和 metadata/NFO 定向回归通过；无数据库服务语义保持。
- [x] 文档明确该检查提供失败后的自愈，不声称文件和数据库进入同一个原子事务，也不外推运行时收益。

预计文件：`src/application/people/service.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先增加模拟 relation 文件已提交但数据库索引 state 未提交的回归，再把数据库 revision 纳入 current 判断。

结果（2026-10-08）：有数据库的 PeopleService 现在只有在旁车 source fingerprint 与数据库 `person_index_item_state` fingerprint/schema revision 同时匹配时才跳过 NFO actor 同步；无数据库服务继续只检查旁车。回归曾在旧逻辑下观察到数据库 credits/index state 删除后仍返回 current，修复后返回 stale，重新写入后恢复 current。NFO relation 单测 3/3、`tests/people_api.rs` 10/10、build、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。本机 ARM64；该方案通过后续扫描自愈，不提供文件与数据库的跨持久化原子提交，也没有测量运行时收益。

#### LUX-433：按人物索引恢复页批量提交 credits

范围：人物索引重建已按最多 100 个 item 读取关系，并批量解析 canonical identities，但对每个 item 分别获取 metadata/credits 写锁并提交事务。新增批量 credits replacement，使每个最多 16 个 item 的有界写 chunk 共享一次写锁和 metadata transaction，再按现有每 item fingerprint、去重、删除过期 credit、条件 UPSERT 和 relation schema 状态写入。接入 index rebuild 页；保留页内 identity lookup、进度游标和 cancellation 检查边界，不改变 schema/API。

验收：

- [x] 批量存储回归验证多个 item 的 credits 与 fingerprint 一致，且同一写 chunk 内 SQL 错误回滚该 chunk，避免部分更新。
- [x] 人物索引重建服务与管理 API 定向回归通过，build、fmt、Clippy 与差异检查通过。
- [x] 代码路径确认每 100 个 item 至多获取 7 次 credits 写锁/metadata transaction；性能记录只陈述事务边界，不外推生产时延或数据库收益。

预计文件：`src/storage/people.rs`、`src/storage/repository_tests.rs`、`src/application/people/rebuild.rs`、两份开发/性能记录。第一步先添加批量 storage contract 回归，第二步实现单事务批量写入，第三步把重建页连接到新入口。

结果（2026-10-08）：人物索引重建的最多 100-item 页现在交给 credits batch replacement；storage 每 16 个 item 共享一次 metadata/credits 写锁和事务，因此每页最多 7 个事务，事务内保留逐 item 既有键读取、差异删除、条件 UPSERT 与 fingerprint 更新。两 item 回归验证第二个 item 的 SQL 失败会回滚整个当前 16-item chunk，且成功重试后两条索引状态与 credits 正确。storage unchanged-row 回归、`tests/people_api.rs` 10/10、build、fmt、全目标全 feature Clippy 和差异检查通过。本机 ARM64；没有统计 PostgreSQL query latency、生产事务耗时、FNOS/NAS 或 CPU 收益。

#### LUX-434：合并重复的 person manifest restore pending 写入

范围：person manifest 每次 checksum 变化都会标记单行 `person_manifest_restore_state` 为 `PENDING`。批量 actor/person 更新中，若该行已经是当前 schema 的 `PENDING`，避免重复 UPDATE 与 timestamp churn；仍须在状态为 `COMPLETED` 或 schema version 变化时写回。保留存储调用和跨进程可见状态，不增加内存缓存、事件或 schema。

验收：

- [x] storage 回归证明重复标记当前 `PENDING` 不触发 UPDATE；从 `COMPLETED` 恢复 pending 和 schema version 变化仍触发更新。
- [x] manifest restore/rebuild 与 people API 定向回归、build、fmt、Clippy 和差异检查通过。
- [x] 性能记录仅描述避免的数据库行 UPDATE，不声称消除了 storage 调用或测得生产收益。

预计文件：`src/storage/people.rs`、`src/storage/repository_tests.rs`、两份开发/性能记录。先增加 UPDATE trigger 计数回归，再为 UPSERT 增加 conflict update predicate。

结果（2026-10-08）：`mark_person_manifest_restore_pending` 的 UPSERT 仅当状态不是当前 schema 的 PENDING 时执行 conflict UPDATE。storage trigger 回归确认重复 pending 为 0 次 UPDATE，而 COMPLETED→PENDING 与 schema version 变化各执行一次更新。manifest checksum restore、legacy migration completion、`tests/people_api.rs` 10/10、build、fmt、全目标全 feature Clippy 和差异检查通过。本机 ARM64；确认的只是数据库 UPDATE 行数边界，不外推端到端 storage 调用数、墙钟或 FNOS/NAS 收益。

#### LUX-435：本地 NFO 页批量写入 actor credits

范围：本地 scan metadata NFO page 每个 item 完成 actor relation file 后，分别获取 credits lock 和 metadata transaction。增加仅供本地 NFO 页使用的 deferred credits 路径：保持每个 item 的 relation、person manifest、NFO 与 image 文件处理及错误隔离，收齐页内 actor credits 后按最多 16 个 item 一个事务批量提交。普通单 item API、在线刮削和 candidate refresh 保持即时持久化。批事务失败时记录失败 item IDs，relation fingerprint 与数据库 credits revision 不一致，使之后扫描可重试；不声称文件与数据库原子提交。

验收：

- [x] people service 回归验证 relation 文件在 credits flush 前已写入、DB revision 尚未 current，flush 后 revision current。
- [x] scanner local NFO page regression 验证多项 actor 更新走 batch flush，单项错误仍隔离；storage 16-item rollback 语义不变。
- [x] metadata/NFO、people API 定向回归、build、fmt、Clippy 与差异检查通过；记录每 16 item 至多一次 credits transaction，不推断生产数据库或 FNOS 收益。

预计文件：`src/application/people/relations.rs`、`src/application/people/service.rs`、`src/application/metadata.rs`、相关 people/scanning tests、两份开发/性能记录。先加 deferred persistence contract 回归，再实现 service batch，最后只连接 local scan NFO batch path。

结果（2026-10-08）：本地 scan NFO page 先完成每项 relation、manifest、NFO 和图片处理，再把 credits 按最多 16 项一组提交；每页 N 项至多执行 `ceil(N/16)` 次 credits replacement 事务。relation 文件早于 credits flush 写入；如果批事务失败，该 chunk 内 item 被报告为失败，数据库 revision 不 current，后续扫描可重试。普通人物 API、在线刮削及单条目 NFO enrichment 仍即时提交。deferred persistence、page flush/错误隔离、metadata NFO 单测、`tests/people_api.rs` 10/10、`tests/scanned_metadata.rs` 16/16、`tests/scanned_series_metadata.rs` 2/2、build、fmt、Clippy 和差异检查通过。本机 ARM64。全目标库测试 775 passed、13 ignored；集成目标中 `emby_counts`、`metadata`、`metadata_selection`、`strm` 四个失败已在父提交 `855d890a` 独立复现，`watch` 的 SQLite 锁冲突单独复跑通过。未据此推断生产 SQL 延迟、FNOS/NAS 或 CPU 收益。

#### LUX-436：在 PostgreSQL 诊断报告中提供 pg_stat_statements 摘要

范围：当前数据库诊断报告没有 SQL 聚合执行时间，扫描/metadata 优化只能依赖 query-wrapper 计数和本机 fixture。若连接的 PostgreSQL 已预加载并启用 `pg_stat_statements`，报告新增最多 20 条当前数据库的 statement 聚合指标：query ID、调用数、总/均值/最大执行时间、返回行数和 shared block hit/read。报告不返回 SQL 文本或参数，不尝试创建 extension、改变 PostgreSQL 配置或自动重启服务；extension 未预加载、未创建或当前角色无权读取时明确报告不可用。SQLite 标记为不适用。

验收：

- [x] PostgreSQL extension 可用时输出有界、脱敏的 query statistics；不改变现有 database diagnostics 字段和权限。
- [x] extension 缺失、未预加载或权限不足时诊断仍成功并标记不可用；SQLite 报告为不适用。
- [x] 自动化覆盖 available/unavailable JSON 合同；PostgreSQL 集成验证可用时执行，并说明未启用时的限制。
- [x] `docs/API.md` 记录管理员可选启用步骤、报告字段和无自动数据库变更的行为；性能记录不把诊断值作为改动后的基准。

预计文件：`src/storage/database_diagnostics.rs`、`docs/API.md`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先增加 SQLite/可用状态合同回归，再实现可选只读查询与 unavailable 降级。

结果（2026-10-08）：PostgreSQL 诊断现在只在服务端预加载且当前数据库已安装 `pg_stat_statements` 时读取当前库累计统计，最多返回 20 条 query ID、调用数、总/均值/最大执行时间、行数和 shared block hit/read；不返回 SQL 文本或参数。未预加载、extension 缺失、权限不足、不可用或超时均降级为显式状态，SQLite 标记 `NOT_APPLICABLE`。SQLite 合同、不可用状态单测和 `cargo test --locked --lib database_diagnostics -- --include-ignored` 通过；集成测试在本机 PostgreSQL 配置下通过可用/不可用合同。build、fmt、全目标全 feature Clippy 通过。最终 all-targets 库测试 777 passed、13 ignored；集成目标 `emby_counts`（实际 1、期望 0）、`metadata`（unchanged NFO 期望 nfo_loaded=1，实际 0）、`metadata_selection`（optional actor enrichment wait）及 `strm`（401/200）均在本任务父提交 `855d890a` 独立复现；`watch` 全目标 5/5 通过。本机 ARM64；API 文档提供 DBA 手动启用步骤。本任务没有测量 query latency 或优化收益，统计值只作为后续诊断信号。

#### LUX-437：本地 NFO 页合并人物 manifest restore 标记

范围：一个本地 NFO page 可能修改多个 Lux canonical person manifest；manifest 内容仍需按人物目录分别校验 checksum、持锁并原子写入，但每个人物目前都会重复调用相同数据库 restore-pending 标记。让 LUX-435 的 page context 在首个 checksum 确认变化的 manifest 写入前，最多提交一次当前 schema 的 restore-pending 状态。普通人物 API 和在线刮削仍保持即时标记；checksum 不变时不标记；文件锁与文件写入仍按人物独立，不声称跨文件原子提交。

验收：

- [x] 同一 deferred NFO page 中多个 manifest 均有变化时，只调用一次数据库 restore-pending 写入，且每个人物 manifest 都写入最新 checksum。
- [x] unchanged checksum 不触发 pending；写文件失败时数据库已标记 pending，之后恢复可重试。
- [x] 普通人物 API 的即时 restore-pending 语义不变；deferred credits 和 NFO page 回归继续通过。
- [x] people API、NFO metadata、build、fmt、Clippy、差异检查通过；性能记录仅报告每页状态写调用边界。

预计文件：`src/application/people/service.rs`、`src/application/people/relations.rs`、`src/application/metadata.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先直接测试两个 manifest 共用 deferred context 的 storage 调用数，再将 page context 接入 canonical manifest 写入。

结果（2026-10-08）：本地 scan NFO page 共享的 deferred context 现在也合并 canonical person manifest restore-pending 标记。并发写入两个不同人物 manifest 的回归确认数据库标记只调用一次，且两个文件都保存各自最新 identity 与 checksum；重复 checksum 不调用标记。标记成功后才原子写 manifest，因此写入失败会保留 PENDING 供后续恢复。非 NFO 人物资产路径仍即时标记。定向单测通过；`tests/people_api.rs` 10/10、`tests/scanned_metadata.rs` 16/16、`tests/scanned_series_metadata.rs` 2/2、build、fmt、全目标全 feature Clippy 和差异检查通过。本任务只记录每页一次标记调用上界，不推断时延、PostgreSQL、FNOS/NAS 或 CPU 收益。

#### LUX-438：记录本地 NFO 页运行指标

范围：管理员健康资源已提供固定低基数 `resources.metadata.counters` 和有界 `stageP95Ms`，但本地扫描 NFO page 尚未记录实际运行数据。增加固定 `local_nfo_page` 指标：每页调用数、输入 item 总数、最大 page size、含 item 错误的 page 数，以及该阶段最近 128 个样本的 p95 墙钟时间。耗时范围包含 source 校验、NFO 读取/解析、人物处理和 deferred credits flush。保留现有健康响应形状，不记录 item ID、路径或错误文本；PostgreSQL 单 SQL 延迟由 LUX-436 的可选 `pg_stat_statements` 提供。

验收：

- [x] 本地 NFO page 执行一次时，健康资源计数器准确增加一次并记录 page 输入条目数；p95 stage 有有界样本。
- [x] item 级 NFO 或 credits 错误将该 page 记为失败；未知指标名不进入输出，标签不含 item ID、路径或错误内容。
- [x] ScanJobService 的 outbox、job worker、手动 refresh 和 scan 后处理共用同一个 ResourceMetrics 实例。
- [x] metrics 与本地 NFO page 自动化回归、metadata/scan 定向测试、build、fmt、Clippy、差异检查通过，并记录指标口径及本机验证限制。

预计文件：`src/observability/resources.rs`、`src/application/metadata.rs`、`src/application/scanner.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先增加 metrics snapshot 和 NFO page 的失败回归，再实现固定名称批次计数与共享指标注入。

结果（2026-10-08）：本地 NFO page 记录固定 `local_nfo_page` 批次计数、输入条目累计数、最大 page 大小、成功/失败页数及最近 128 个耗时样本的 p95。失败页包含 item 级 NFO/credits 失败和批次级错误；计时覆盖 source 校验、NFO 读取/解析、人物处理及 deferred credits flush。ScanJobService 的 outbox、job worker、手动 refresh 与 scan 后处理都向 MetadataEnricher 传入同一个 ResourceMetrics。指标名称白名单阻止未知名字进入输出，不包含 item ID、路径或错误文本。metrics 单测、metadata 14/14、`tests/scanned_metadata.rs` 16/16、`tests/scanned_series_metadata.rs` 2/2、build、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。本机 ARM64；没有运行独立吞吐基准或 FNOS/NAS/PostgreSQL A/B，不据此声称 CPU、SQL 延迟或生产收益。

#### LUX-439：清理本地 metadata storage 的过期 dead-code 抑制

范围：早期本地 metadata storage API 曾先于 scanner 消费路径建立，代码仍保留“下一阶段 worker 使用”的 `#[allow(dead_code)]` 和过期注释。逐项核对 completeness、扫描 outbox/backfill、批量图片 storage 合同的生产调用点；已被消费的接口去掉无效抑制并更新注释。只在确认没有生产调用、且不是仍需维护的测试合同后，才删除无用接口；保留其他阶段的 manifest API、字幕 API 与无关 storage 范围。

验收：

- [x] 本地 metadata storage 中已接入的类型、字段和方法不再靠宽泛 `#[allow(dead_code)]` 隐藏编译器可发现的问题；注释反映当前消费者。
- [x] 未使用接口只有在生产调用、测试用途和后续任务依赖均核实后才清理；队列状态、schema、公共行为和存储事务语义不变。
- [x] storage 定向测试、build、fmt、Clippy 和差异检查通过；报告仍保留的非本地 metadata dead-code 抑制及原因。

预计文件：`src/storage/jobs.rs`、`src/storage/metadata.rs`、`src/storage/mod.rs`、`src/storage/repository.rs`、`docs/LUX-DEVELOPMENT.md`。先核对抑制范围内的类型/方法实际调用者与测试，再去掉过期抑制或将必要例外缩小到单个接口。

结果（2026-10-08）：移除了扫描 completeness、outbox/backfill 与图片 batch 合同上 7 处“下一阶段消费者”类型抑制和两个实现块级宽泛抑制。正常 build 先暴露 8 组确实只供 storage 合同回归使用的入口/字段；核对调用点后，将例外缩小到具体方法、类型或测试专用常量，并为每项注明它覆盖的持久化行为。没有删仍由回归覆盖的单项状态转换，也未改变 schema、队列状态、事务边界或运行期入口。`cargo test --locked --lib storage::repository::repository_tests` 119 passed、8 ignored；build、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。本轮仍保留的其他范围 dead-code 抑制属于 scan-manifest LUX-266/267 边界、字幕/Emby migration 接口和测试夹具；本任务没有扩大清理范围。

#### LUX-440：记录本地人物 credits 事务耗时与 item 写入量

范围：LUX-435 已将本地 NFO 页人物 credits 按最多 16 个 item 共用一个 storage transaction，但当前运行指标只记录整页耗时。每个 deferred credits storage transaction 增加固定低基数指标：事务数、transaction 输入 item replacement 数及最大批量、输入 actor credit 条目数、成功/失败数和最近 128 个事务耗时样本的 p95。复用 LUX-438 的 `ResourceMetrics`，不记录 item ID、路径、人物 ID 或错误文本；credit 条目数表示去重前的输入量，不伪称数据库实际变更行数。

验收：

- [x] deferred NFO credits 的每个 storage transaction 只记录一次指标；计数区分 transaction 数、输入 item replacement 数、最大每事务 item 数、credit 条目数和成功/失败。
- [x] 事务耗时覆盖 storage 锁、事务执行与 commit；最近 128 个样本计算 `stageP95Ms`，指标名固定且无 item/person 标签。
- [x] 本地 NFO page 回归验证 credits 失败会计为失败事务并与现有 page 错误指标一致；成功回归验证 item/credit 数准确。
- [x] people relation、metadata page 与 metrics 定向测试、build、fmt、Clippy、差异检查通过；文档明确计数口径和本机验证边界，不声称生产收益。

预计文件：`src/observability/resources.rs`、`src/application/people/relations.rs`、`src/application/metadata.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先增加资源快照及失败 credits page 的回归，再在每个 deferred storage transaction 周围记录固定名称指标并复用 MetadataEnricher 的 metrics。

结果（2026-10-08）：本地 NFO page 的每个 deferred credits storage 调用均记录固定低基数 transaction 指标。计时覆盖 replacement storage 调用，包括 storage lock、事务执行和 commit；credit entry 数按送入 storage 前的 relation 输入条目计，不冒充数据库变更行数。回归注入 credits 写入失败并重试，确认失败和成功 transaction 各计一次、输入 item/credit 数为 4、最大每事务 2 item，page 错误计数同步增加。资源指标的固定名称、输入/结果计数和最近 128 个样本 p95 单测通过；people deferred credits 与 NFO page 回归通过。`cargo build --locked`、fmt、全目标全 feature Clippy 和 `git diff --check` 通过；本机 ARM64。`cargo test --locked --all-targets --no-fail-fast` 的 library 部分 781 passed、13 ignored，集成测试中 4 项仍失败：`emby_counts`（实际 1、期望 0）、`metadata`（unchanged NFO 期望加载 1、实际 0）、`metadata_selection`（等待可选 actor enrichment 超时）和 `strm`（401、期望 200）。这些与父提交 `855d890a` 的既有验收记录相同；该父提交是当前 HEAD 的祖先，相关集成测试文件未改动，其余已执行目标通过。没有把本机测试推断为生产 PostgreSQL 延迟、FNOS/NAS 或 CPU 收益。

#### LUX-441：让 unchanged NFO revision 回归匹配语义 fingerprint

范围：LUX-416 已规定 NFO stat fingerprint 变化但文件内容 SHA-256 未变、cache defaults 完整且 actor relation current 时跳过 XML 解析并只同步 stat fingerprint。集成测试 `unchanged_nfo_content_keeps_the_rich_snapshot_after_file_revision_changes` 仍断言旧行为 `nfo_loaded = 1`，导致已优化的 `metadata` target 失败。修正测试为断言该次调用 `nfo_skipped = 1`、`nfo_loaded = 0`，并继续验证 rich NFO snapshot 不变；内容确实变化时重新解析的既有回归保持不变。不修改生产逻辑。

验收：

- [x] 相同内容、变化的 file revision 返回 skipped 且保留 rich snapshot。
- [x] `cargo test --locked --test metadata`、fmt 与差异检查通过；既有 changed-content 回归仍验证重新加载。

预计文件：`tests/metadata.rs`、`docs/LUX-DEVELOPMENT.md`。先隔离复现旧断言失败，再更新为 LUX-416 的语义 fingerprint 合同并运行 metadata 集成目标。

结果（2026-10-08）：隔离复跑确认旧断言稳定要求 `nfo_loaded = 1`，与当前语义 fingerprint 命中行为冲突；将回归改为要求 `nfo_loaded = 0`、`nfo_skipped = 1`，仍校验 snapshot 未变化。`cargo test --locked --test metadata` 20/20 通过，其中 changed-content 回归继续验证 NFO 会重新加载；fmt 与 `git diff --check` 通过。修正后 `cargo test --locked --all-targets --no-fail-fast` 的 library 测试为 781 passed、13 ignored；`metadata` 20/20 通过。全量门禁仍有 3 个已在父提交 `855d890a` 记录的独立失败：`emby_counts`（计数 1、期望 0）、`metadata_selection`（等待可选 actor enrichment 超时）、`strm`（401、期望 200）；本次没有修改这些测试文件或对应行为。只修正测试合同，没有改变生产代码。

#### LUX-442：原子提交本地 completeness claim、结果与补缺调度

范围：本地 metadata completeness worker 目前先以一个 storage transaction prepare/claim 一页 checks，再以一个或最多六个 transaction 写入 completeness 结果并创建或合并 `FILL_MISSING` job。由于本地检查计划已在进入 storage 前完成，claim 和结果提交之间没有耗时工作；把同一最多 512-check 批次的 prepare/claim、READY 结果写入、due provider retry 筛选及补缺调度放进同一个 metadata write transaction。保留输入 fingerprint、已 READY/RUNNING 去重、失败/取消重领、library/item 锁顺序、自动补缺策略和 job/item 去重语义。事务失败时整批回滚，不留下已提交的 RUNNING claim 或部分 job 状态；不改变 schema 或公共 API。

验收：

- [x] scanner 每个 completeness check batch 只调用一次合并 storage 操作；最多 512 个 check/result 与最多 512 个候选可在同一事务内处理，due retry 筛选与 FILL_MISSING SQL 均按最多 100 个 item 分片，不重复写入结果。
- [x] 同 fingerprint 的 READY/RUNNING 不重领；变化 fingerprint 与可重试 FAILED/CANCELLED 可完成一次检查；due provider-unavailable retry、不可用 scraper 退避和 library 自动策略与当前行为一致。
- [x] 注入 enqueue SQL 失败时，原子事务不遗留 RUNNING/READY 部分结果或 metadata job/item；在到期 retry 已进入消费流程后再注入失败，也会回滚 retry 消费和 job/item 变更；移除故障后同一输入可以成功重试且只创建有效 job。
- [x] 覆盖多个 capability、已 READY missing、due retry、自动策略关闭、独立数据库连接并发重复提交和 205 项分页；SQLite/PostgreSQL storage 合同、build、fmt、Clippy、`git diff --check` 通过。
- [x] 性能记录按事务路径说明每个批次由 2–7 个 metadata write transactions 降至 1 个；不将事务数推断为 SQL 时延、PostgreSQL/NAS/FNOS 吞吐或 CPU 收益。

预计文件：`src/storage/metadata.rs`、`src/application/scanner.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先增加故障注入回归，再合并 storage 事务并切换 scanner 调用，最后更新事务边界记录。

结果（2026-10-08）：扫描 worker 每页改为一次 combined storage 调用，在同一 metadata write transaction 内 claim、写 READY 结果、筛选 due retry 和调度补缺；due retry 候选也按 100 项分片。205 项 SQLite 回归确认查询包装器计数从未分片时的 11 次变为 13 次（due retry 查询为 3 片），明确记录这是大页的有界查询代价。故障注入覆盖新 job item 插入失败和 due retry 消费后插入失败，两者均整体回滚；并发回归使用独立 SQLite database handles。`metadata_completeness` 定向测试 8/8、原子提交测试、205 项分页测试和 PostgreSQL `postgres_progressive_scan_metadata_storage_contract` 通过；`cargo build --locked`、fmt、全目标全 feature Clippy 通过。`cargo test --locked --all-targets --no-fail-fast` 的 library 为 780 passed、13 ignored；全量集成仅有三个已在父提交 `855d890a` 记录的独立失败：`emby_counts`（实际 1、期望 0）、`metadata_selection`（可选 actor enrichment 等待超时）、`strm`（401、期望 200）；本轮 `watch` 5/5 通过。`git diff --check` 通过。本机 `arm64`；没有运行 FNOS/NAS 性能验证，也不推断 PostgreSQL 时延、吞吐或 CPU 收益。

#### LUX-443：合并本地 metadata worker 的 pending、page 与 manifest 状态读取

范围：本地 metadata worker 每个处理批次先查询 pending target，再读取 manifest 和 postprocessing roots，最后按电影、home video、episode 顺序最多查询三次目标页。将 pending 状态与按既有优先级选出的首个有界 target page 合并为一个 storage 查询；将 manifest workflow/discovery 状态与 root 身份记录合并为一个 storage 查询。worker 直接处理已读取的 page，不重新查询；仍在每个有 pending 的批次处理前 stat 正向 manifest roots，并保留身份不匹配时标记 pending target 失败、记录脱敏事件的行为。无可用 source 但仍有 pending target 时，继续进入原有 source unavailable 失败路径。无 schema、公共 API 或 target 顺序变化。

验收：

- [x] 一个有界 storage page 查询同时返回 `has_pending` 与最多请求上限的 target source；优先级仍为 MOVIE、VIDEO、EPISODE，每类内按 target ID 稳定排序。无 source 但有 pending 时能区分于无 pending。
- [x] manifest 与所有 postprocessing root 身份信息由单个 storage 查询加载；每个 pending 批次仍执行本地 root identity stat，变化时不处理媒体文件并保留原有失败/告警语义。
- [x] worker 不再单独调用 pending existence 查询，也不重复加载 metadata page；空闲通知回归仍为一次 query-wrapper 调用。
- [x] SQLite 回归覆盖三种 target 类型、顺序、无 source pending、无 pending 和 root metadata；PostgreSQL storage contract、相关 scanner/metadata 测试、build、fmt、Clippy、`git diff --check` 通过。
- [x] 性能记录只对比固定 fixture 的 query-wrapper 调用边界，不推断 SQL 墙钟、FNOS/NAS CPU 或 PostgreSQL 生产收益。

预计文件：`src/application/scanner.rs`、`src/application/metadata.rs`、`src/storage/jobs.rs`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先写 storage 查询计数与 source-order 回归，再实现 page/manifest 合并查询并让 worker 消费已加载 page。

结果（2026-10-08）：本地 metadata worker 一次读取 pending 状态与首个有界 source page，保持 MOVIE、VIDEO、EPISODE 优先级以及每类按 target ID 的稳定顺序；pending 条目无 source 时仍与无 pending 区分。manifest workflow/discovery 状态、全部 root identity 与 target cursor 信息合并为一次 storage query，仍在每个 pending 批次 stat 正向 roots；root identity 变化时目标被标记 FAILED、root 标为不可用，NFO 不会被应用。固定 SQLite query-wrapper 回归覆盖三类顺序和 source unavailable 状态，worker/manifest 回归覆盖 root mismatch。`metadata` 20/20、`scanned_metadata` 16/16、`scanning_jobs` 81/81、PostgreSQL `postgres_progressive_scan_metadata_storage_contract`、build、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。全量 all-targets 有三个失败目标（`emby_counts`、`metadata_selection`、`strm`），与父提交 `855d890a` 已记录的独立失败一致；相关集成测试文件未被本任务修改。开发机 `arm64`；未在 FNOS/NAS 上部署或测量运行性能，query-wrapper 计数不代表 SQL 墙钟。

#### LUX-444：本地 completeness 页按路径复用 NFO projection 与人物关系检查

范围：LUX-422 已把本地 completeness 的 item 检查限制为最多 4 路并发，metadata row 中可解析的 NFO projection 已直接复用；但多个 episode 共用 `episode.nfo` 时仍重复读取和解析 sidecar，legacy `people/items` fallback 也逐 item 查询目录。给一次 planning page 增加按 canonical NFO target path 键控的惰性缓存，让相同路径共用一次读取和解析；给 actor relation 检查增加惰性 page cache，在新式 per-item relation 不存在后只枚举一次 legacy relation 目录，并仅对命中 ID 读取旧文件。目录枚举最多读取 4096 个 entry；达到上限但目标 ID 尚未齐全，或目录枚举失败时，对未确定 item 保留原逐路径 fallback。缓存只活到本次 page planning 结束。每项仍独立做 root/source canonicalization 与目标安全检查。缓存错误按 item 传播，planning task 仍按原输入顺序选择首错误；数据库已有的 NFO projection 仍优先，LUX-413 的 crew 缺失短路不变。

预计文件：`src/application/nfo.rs`、`src/application/people/service.rs`、`src/application/candidates.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。

验收：

- [x] 同目录多个 episode 共用 `episode.nfo` 时，并发 page lookup 只有一个调用执行 sidecar read/parse initializer，所有 item 得到相同 projection；互不相同的 target 不合并。
- [x] 新 relation 缺失的多个 item 共用一次 legacy `people/items` 目录快照；命中旧 relation 仍解析文件，完整快照中的未命中 ID 不逐项 stat legacy 路径，4096-entry 上限或目录枚举失败时回退原路径检查。
- [x] NFO path/read/parse error 保持原类型并按既有输入顺序汇总；数据库 projection 快路径不触发旁车读取；storage item/context/attempt 批量查询不变。
- [x] page planning 总并发仍不超过 4；新增缓存回归、NFO/scanned metadata 定向目标、fmt、build、全目标全 feature Clippy 与差异检查完成。全量目标测试执行时命中已有且与本改动无关的集成测试失败，具体结果见下。
- [x] 性能记录只陈述同路径重复解析被去重及并发上界，不推断实际文件系统时延、PostgreSQL 或 FNOS/NAS CPU 收益。

短计划与文件：修改 `src/application/nfo.rs`、`src/application/people/service.rs`、`src/application/candidates.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先为同一 canonical sidecar 和 legacy directory snapshot 加并发回归，再将两个缓存限制在 planning page，并验证错误顺序、目录读失败 fallback、数据库 projection 快路径与既有并发上限。

结果（2026-10-08）：固定测试确认同一 page 内共享 sidecar 只解析一次、不同 target 不共享、下一页读取变更后的内容，错误类型保持且 database projection 可避开失效 source；relation cache 一页只枚举一次，读取命中的旧 JSON，不可读目录和超过 4096 项后的未确定 ID 均走原路径 fallback。候选 page planning 回归、NFO projection 回归、relation cache 3 项回归、`nfo_writer` 26/26、`scanned_metadata` 16/16、build、fmt 和全目标全 feature Clippy 通过。全量测试的库单测为 789 通过、13 忽略；集成阶段在 `emby_counts` 失败并停止（未收藏计数期望 0、实际 1）。单独运行 `metadata_selection` 为 30/31（可选 actor enrichment 等待超时），`strm` 的认证请求返回 401 而期望 200；这些失败此前已在父提交记录，且对应集成测试不在本任务改动中。开发机 `arm64`；未测文件系统耗时、PostgreSQL、FNOS/NAS 或生产 CPU 收益。

#### LUX-445：以 viewer 身份验证 Emby viewer 计数权限

范围：`tests/emby_counts.rs` 的 viewer 计数断言使用管理员 token，同时传入 viewer `UserId`，因此请求仍保留管理员的媒体库权限；该夹具不能证明 viewer 只能统计获授权的媒体库。用 viewer 自己登录得到的 token 发起计数请求，保持其仅获电影库权限的 fixture，并继续核验 viewer 收藏数。生产鉴权逻辑和 Emby 管理员代查语义不变。

验收：

- [x] viewer token 只统计 viewer 有权限的媒体库，并正确按 viewer 收藏状态筛选；管理员 token 的全库计数断言保持。
- [x] `cargo test --locked --test emby_counts`、fmt 与 `git diff --check` 通过。

预计文件：`tests/emby_counts.rs`、`docs/LUX-DEVELOPMENT.md`。先复现管理员 token 触发 viewer 权限断言失败，再改为 viewer token 并运行专用集成测试。

结果（2026-10-08）：原测试以管理员 token 传入 viewer `UserId`，管理员权限因此包含 Shows 库，稳定得到 `SeriesCount = 1`；用 viewer 登录 token 请求后，viewer 可见范围的影片计数与收藏计数均符合预期。`cargo test --locked --test emby_counts` 1/1 通过。

#### LUX-446：用可控门闩验证自动元数据任务不等待可选人物详情

范围：自动匹配测试用“整个 scan + metadata job 必须在 1 秒内完成”的墙钟断言验证可选 actor enrichment 不阻塞；该固定阈值受本机及 CI 负载影响，无法区分扫描本身耗时和人物详情网络等待。将 mock 人物详情响应改为测试控制的门闩：metadata job 进入终态后才放行人物详情响应，并继续确认人物简介最终写入。生产任务调度和 enrichment worker 保持不变。

验收：

- [x] 自动 metadata job 必须在被门闩暂停的人物详情响应前完成；之后响应释放，人物简介在有界等待内持久化。
- [x] `cargo test --locked --test metadata_selection`、fmt 与 `git diff --check` 通过。

预计文件：`tests/metadata_selection.rs`、`docs/LUX-DEVELOPMENT.md`。先复现固定 1 秒阈值因实际 1.15 秒工作流耗时而失败，再改为确定性请求门闩与终态断言。

结果（2026-10-08）：原失败重现为 1.15 秒，低于 mock 人物详情请求的 1.5 秒响应延迟，因此并非任务等待该响应；固定的 1 秒总时延限制误判了本机工作流耗时。新回归在 actor endpoint 收到请求后保持其未响应，确认 metadata job 已 `COMPLETED` 后才释放，并继续验证人物 NFO 得到补充。`cargo test --locked --test metadata_selection` 31/31 通过。

#### LUX-447：区分共享管理 API key 与个人 PlaybackInfo 身份

范围：STRM PlaybackInfo 集成用例只传共享管理 API key，却期待创建用户播放上下文成功。共享管理 key 代表服务器身份，不能替代个人会话；PlaybackInfo 需使用真实 Emby 用户 token。保留 key 单独请求应返回 401 的合同，并在提供真实用户 token 时验证 PlaybackInfo 成功且共享 key 不进入播放 URL。生产鉴权边界保持不变。

验收：

- [x] 只有共享管理 API key 的 PlaybackInfo 请求返回 401；用户会话 token 请求成功。
- [x] 同时带用户 token 与共享 key query 时，成功响应的播放 URL 不包含共享 key；STRM 重定向、用户播放和本地路径播放行为回归通过。
- [x] `cargo test --locked --test strm`、fmt 与 `git diff --check` 通过。

预计文件：`tests/strm.rs`、`docs/LUX-DEVELOPMENT.md`。先复现共享 key 单独请求返回 401，再明确区分服务器与用户身份，运行完整 STRM 集成目标。

结果（2026-10-08）：共享 key 单独请求稳定返回 401；带真实 `X-Emby-Token` 的 PlaybackInfo 返回 200，响应播放 URL 不含共享 key。完整 STRM 集成目标 1/1 通过，现有 Emby 302 handoff 与本地 path 播放断言保持。

#### LUX-448：跳过相同人物图片与 provider index 的原子替换

范围：人物 profile 图片和 provider index 的写入目前即使目标字节完全相同，也会创建临时文件、写入、`fsync` 并 `rename`。新增按现有安全 metadata 检查和有界内容读取判断是否相同的原子写入口；相同内容保留原文件，不同或缺失内容仍走原子替换。图片权限约束必须在跳过与写入两条路径都执行；不改变图片下载、provider identity 或 index rebuild 语义。

验收：

- [x] 重复上传相同图片后，人物图片和 provider index 文件不被替换；测试通过 inode 保持验证。
- [x] 不同图片字节仍原子替换图片及索引；缺失文件仍按原路径创建。
- [x] 非普通文件和符号链接仍被拒绝，目标权限仍为私有；不扩大可读取图片的最大输入范围。
- [x] people service 定向测试、fmt、build、Clippy 和 `git diff --check` 通过；性能记录只说明固定回归验证的文件替换边界，不推断墙钟、FNOS/NAS 或生产收益。

预计文件：`src/application/people/helpers.rs`、`src/application/people/assets.rs`、`src/application/people/service.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先扩展重复图片上传回归确认原实现会替换 inode，再增加安全的 unchanged-write 路径并复测。

结果（2026-10-09）：相同 profile 图片再次写入时，人物图片、provider index 和 legacy TMDb index 三个目标均保留原 inode；helper 回归确认缺失目标仍创建、不同内容走原子写、相同内容保持私有权限，符号链接和目录目标拒绝。旧实现回归先稳定观察到重复上传替换了人物图片 inode。People service 定向测试 39/39 通过；`cargo build --locked`、`cargo test --locked --all-targets --quiet`（791 library tests 通过、13 忽略，集成目标全部通过）、fmt、全 target/all features Clippy 与 `git diff --check` 通过。本机 `arm64`。固定测试只证明跳过相同内容的临时文件写入、`fsync` 和 `rename`，不测量总文件系统调用或墙钟，也不外推 FNOS/NAS/生产收益。

#### LUX-449：按页合并本地 NFO metadata 写事务

范围：本地 metadata worker 已在一次 item 更新中提交 NFO metadata、provider ID、premiere date 与 fingerprint，但一页中每个 item 仍独立获取 metadata 写锁并提交事务。将已成功解析的状态按最多 16 个 item 聚合，每个 chunk 只获取一次写锁并提交一个事务；标题或年份身份检查前先提交此前暂存项，保持同页 identity conflict 检查顺序。普通在线/单条 NFO 写回继续使用单条事务。若 chunk 事务失败，回退到逐 item 写入以保持既有错误隔离，并只将真正失败的 item 计为 NFO 失败。不改变字段优先级、provider ID 合并、NFO fingerprint 或失败重试语义。

验收：

- [x] 存储批量更新按最多 16 个 item 共用写锁和事务；注入后续 item 失败时，批次前面已执行的 UPDATE 也整体回滚。
- [x] 本地 NFO page 在解析完成后批量提交；标题或年份变更前提交前序状态并检查同页身份冲突。批次失败后逐 item 回退，成功 item 保留更新，失败 item 单独标记。
- [x] 定向 storage 与 metadata page 回归、build、fmt、Clippy、全目标测试和差异检查通过。
- [x] 性能记录区分事务/锁次数与 SQL UPDATE 次数；不把事务合并表述成 SQL 调用减少或 FNOS/NAS 收益。

预计文件：`src/application/metadata.rs`、`src/storage/catalog.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先添加跨 item 回滚回归，再实现 storage 批量提交和失败回退。

结果（2026-10-09）：本地 NFO metadata/provider ID/premiere date/fingerprint 更新按最多 16 个 item 共用一次 metadata 写锁和事务；标题或年份改变前先提交已暂存状态，再检查同页身份冲突。注入批次中途 UPDATE 失败后，存储回归确认整个 chunk 回滚；应用层逐项回退，成功项保留、失败项单独标记。`cargo test --locked --all-targets` 的 793 项库测试与所有集成目标通过（13 项忽略）；`cargo build --locked`、fmt、全目标全 feature Clippy、`git diff --check` 通过。本机 ARM64。SQL UPDATE 仍逐 item 执行；未测量事务墙钟或部署后 PostgreSQL/FNOS/NAS 收益。

#### LUX-450：合并本地 NFO 缺失默认字段修复事务

范围：unchanged NFO 的 semantic fingerprint、rich cache 和 actor relation 均已命中时，会跳过重新解析；若 provider ID 或 premiere date 仍缺失，当前 `repair_local_nfo_defaults` 对每个 item 单独获取 metadata 写锁并开启事务。将该修复接入 local NFO page 的 deferred 状态写入，按最多 16 个 item 共用一次 metadata 写锁和事务；只补填仍为空的 provider ID/premiere date，保留并发写入时已补齐的字段，并与 LUX-449 的 metadata 状态批次共享页级 flush。批次失败时逐 item 回退并保持失败隔离。记录固定低基数的 state transaction 次数、metadata/default-repair item 数、成功/失败与有界耗时 p95；普通单 item/在线 NFO enrichment 仍立即修复且不进入 page 指标。

验收：

- [x] 本地 NFO page 的已检查 NFO 缺失默认字段修复纳入页级批次；单 item/在线路径保留即时写入。
- [x] provider ID 仅合并新 key、premiere date 仅填空；并发或预先存在的非空值不被覆盖；无字段变化不执行 UPDATE。
- [x] 批次后续写失败会整体回滚，应用层逐 item 回退并仅报告实际失败项。
- [x] state transaction 指标覆盖页级批次与失败后的逐项回退，且不记录 item ID、路径或 SQL 文本。
- [x] 定向 storage 与 metadata page 回归、build、fmt、Clippy、全目标测试和差异检查通过；性能记录区分事务合并与逐 item SQL 语句数。

预计文件：`src/application/metadata.rs`、`src/storage/catalog.rs`、`src/storage/mod.rs`、`src/storage/repository_tests.rs`、`src/observability/resources.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先增加 storage 混合写批次回滚、metadata deferred repair 与 transaction metric 回归，再把 unchanged-NFO repair 接入既有页级 flush。

结果（2026-10-09）：unchanged NFO 的缺失 provider ID/premiere date repair 与 metadata 更新共用最多 16 项的页级 metadata 写事务；仅补缺失字段，并发已补齐值不会覆盖；无变化不会执行 UPDATE。混合写批次故障回归确认事务回滚，应用层逐 item fallback 保持错误隔离，指标覆盖批次及 fallback 的成功/失败与耗时。移除被新入口替代的无用 storage wrapper。定向测试通过；`cargo build --locked`、`cargo test --locked --all-targets`（库 796 passed、13 ignored，所有集成目标通过）、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。本机 `arm64`；PostgreSQL 专用测试/基准按标记未运行，未部署 FNOS/NAS，也没有生产 CPU 或墙钟收益测量。

#### LUX-451：为人物关系运行索引预留快照 checksum

范围：`person_index_item_state` 目前只记录 NFO source fingerprint 与 relation schema version。人物关系文件可以在 source fingerprint 不变时变化；如果文件原子替换成功而 credits transaction 失败，旧索引状态仍可能被误判为 current。新增 nullable `relation_checksum` 列以保存 `people.json` 完整快照 checksum；旧行迁移后 checksum 为空，后续消费路径必须将其视为 stale。文件快照继续作为配置卷恢复来源，数据库仍只是运行索引；本任务不声称文件系统与数据库可共享原子事务，也不改变 public API、人物关系模型或关系文件格式。

验收：

- [x] SQLite 与 PostgreSQL 均新增 nullable `person_index_item_state.relation_checksum`，历史 migration 文件和 checksum 不变。
- [x] 从空库运行全部 migration 后新列存在；从已有 0165 schema 升级保留 relation state 行和原 source fingerprint/schema version，checksum 初始为空。
- [x] migration 序号/双后端合同回归、`tests/storage.rs`、build、fmt、Clippy 与 `git diff --check` 通过。

预计文件：`migrations/0166_person_index_relation_checksum.sql`、`migrations-postgres/0166_person_index_relation_checksum.sql`、`tests/storage.rs`、`tests/scanning_jobs.rs`、`tests/danmaku.rs`、`tests/scanner.rs`、`tests/postgres_database.rs`、`tests/admin_health.rs`、`tests/ready_version.rs`、`docs/LUX-DEVELOPMENT.md`。新 migration 会提升当前 schema version；因此本任务也更新这些测试目标中的 latest-schema 断言。先添加既有 state 行升级回归并确认旧 schema 缺少该列，再增加双后端 additive migration 与空库/升级验证；随后由独立应用任务将 checksum 写入 credits transaction 并在跳过路径比较。

结果（2026-10-09）：SQLite/PostgreSQL 均新增 nullable `relation_checksum`，旧 migration 未修改。SQLite `tests/storage.rs` 51/51 通过，覆盖从空库迁移和从 0165 升级时保留 source fingerprint、relation schema version 与时间戳，旧 checksum 为 NULL；静态双后端合同确认 PostgreSQL 使用相同 additive nullable DDL。schema-version 相关 `scanning_jobs` 81/81、`scanner` 17/17、`danmaku` 7/7、`admin_health` 1/1、`ready_version` 2/2 通过；PostgreSQL 数据库目标 16 项因本机无 PostgreSQL 实例而 ignored。`cargo build --locked`、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。本次 `cargo test --locked --all-targets` 的 796 个库测试通过、13 项忽略；集成测试在 `watch` 中遇到 SQLite `database is locked`。该目标独立复跑 5/5 通过，其后 5 个 Web/webhook 目标也通过，因此记录为并行测试时的偶发锁冲突，不归因于 migration。开发机 `arm64`；未部署 PostgreSQL/FNOS，也没有生产性能验证。应用层 checksum 写入与比较留给后续独立任务。

#### LUX-452：在人物 credits 事务中保存关系快照 checksum

范围：LUX-451 已增加 nullable `person_index_item_state.relation_checksum`，本任务补齐 storage 写入边界：credits 批量替换与 source fingerprint、关系 checksum 和 schema version 在同一事务写入。关系 checksum 由调用方对实际持久化的 `people.json` 字节计算并以 SHA-256 小写十六进制传入。fingerprint-only 兼容 API 写入时清空 checksum，避免保留已过期快照值。文件 checksum 读取与全部应用层调用方接入在后续独立任务完成。

验收：

- [x] 单项与批量 credits 写入均能原子保存 relation checksum；批次失败时 credits 和 checksum state 一起回滚。
- [x] SQLite storage 回归覆盖 checksum 持久化、批次回滚与旧 API 清空 checksum；不改变 PostgreSQL/SQLite 通用 SQL 边界。
- [x] 相关 storage 测试、build、fmt、Clippy 与差异检查通过；PostgreSQL 未运行时如实记录。

预计文件：`src/storage/people.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`。先添加单项/批量写入与回滚回归，再增加 checksum-aware batch writer。此任务不修改应用层读写调用方。

结果（2026-10-09）：storage 的单项和批量 credits replacement 均在同一 metadata transaction 写 source fingerprint、relation schema version 和 relation checksum；fingerprint-only 兼容入口明确清空 checksum，避免沿用旧值。SQLite 回归覆盖单项/批量保存、旧 API 行为，以及第二个 item 写 state 失败时整页 credits/state 回滚。`cargo test --locked --all-targets` 通过（库 797 passed、13 ignored，所有未忽略集成目标通过）；`cargo build --locked`、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。本机 `arm64`；PostgreSQL 16 项按需实例的测试在本机 ignored，未运行 PostgreSQL/FNOS 部署或生产性能验证。测试与 build 均使用 `/Volumes/Toshiba/mywork/Lux/target`。

#### LUX-453：从人物关系文件快照贯通 checksum 写入

范围：人物关系持久化和 NFO deferred credits 页批次对即将原子写入的 `people.json` 字节计算 SHA-256 小写十六进制，并将同一份待写字节的 checksum 随 credits 一起交给 LUX-452 storage API。不从解析对象再次序列化计算，也不重复读取文件。人物 metadata 改写留给 LUX-454；读取快照、索引重建与 current 检查留给 LUX-455。文件系统与数据库不能共享原子事务；DB 写失败后关系文件仍可作为恢复来源。

验收：

- [x] checksum 对即将持久化的原始文件字节计算，不重序列化关系对象，也不额外读取同一文件。
- [x] 人物关系 immediate 持久化与 NFO deferred credits 批次均将正确 checksum 与 credits/fingerprint 一起提交。
- [x] `service.rs` 人物服务回归覆盖 immediate 与 deferred 写回均保存实际文件 checksum；相关 storage/people tests、build、fmt、Clippy 与差异检查通过。

预计文件：`src/application/people/helpers.rs`、`src/application/people/relations.rs`、`src/application/people/service.rs`、`docs/LUX-DEVELOPMENT.md`。先增加 raw snapshot checksum 读写回归，再贯通 immediate/deferred credits 路径。

结果（2026-10-09）：即将写入的关系 JSON 字节只序列化一次，并对同一份字节计算 SHA-256 小写十六进制；immediate relation persist 与 deferred NFO credits flush 在 credits transaction 中保存相同 checksum。服务回归核对数据库值等于实际文件字节 checksum。`application::people::service::tests` 39/39、`cargo build --locked`、fmt、全目标全 feature Clippy 与 `git diff --check` 通过。PostgreSQL/FNOS 未运行；跨文件系统与数据库的恢复校验留给 LUX-455。

#### LUX-454：人物 metadata 改写同步关系快照 checksum

范围：`update_item_actor_metadata` 和 `update_person_metadata` 会改写 `people.json` 并替换对应 credits；将待写文件原始字节的 SHA-256 checksum 与 credits、source fingerprint 一起提交，避免这些写回路径留下 checksum 为空或过期的索引状态。迁移完调用方后，将不再使用的 fingerprint-only 单项 wrapper 限制为测试使用。文件格式和 public API 不变，文件系统与数据库的持久化边界仍分别处理。

验收：

- [x] actor metadata 与 person metadata 更新后，数据库保存的 relation checksum 等于实际 `people.json` 字节的 SHA-256。
- [x] relation 文件先写入、credits/checksum 事务失败时保留文件作为恢复来源；不重复读盘或重序列化计算 checksum。
- [x] service 回归覆盖两种 metadata 更新路径；相关 people tests、build、fmt、Clippy 与差异检查通过。

预计文件：`src/application/people/metadata.rs`、`src/application/people/service.rs`、`src/storage/people.rs`、`docs/LUX-DEVELOPMENT.md`。先在现有 metadata 更新回归中加入实际文件 checksum 断言，再将 checksum 传入 credits transaction，并把已退出生产路径的 wrapper 限制为测试使用。

结果（2026-10-09）：actor metadata 与 person metadata 两种改写都对即将写入的 JSON 字节计算 checksum，并与对应 credits/source fingerprint 在同一数据库事务持久化；写文件仍先于数据库事务，失败时保留可供后续重建读取的文件。旧 fingerprint-only 单项入口只供测试使用。回归先在旧实现下失败（数据库 checksum 为 NULL），再以 39/39 people service tests 通过；`cargo build --locked`、全目标全 feature Clippy、fmt 与 `git diff --check` 通过。全目标测试在合并实现状态下通过（797 passed、13 ignored；其中 16 个 PostgreSQL 专项因无本地 PostgreSQL 而 ignored）。本机 `arm64`，未做 FNOS/生产性能验证。

#### LUX-455：人物关系 current 检查比较快照 checksum

范围：`item_actor_relation_is_current` 与 `nfo_relation_snapshot_is_current` 读取关系文件原始字节，并要求数据库中的 source fingerprint、relation checksum 和 schema version 三者匹配。checksum 缺失的旧行和文件变化但 source fingerprint 不变的记录必须判 stale；无数据库时保留当前文件内 source fingerprint 校验语义。索引重建的 checksum-aware skip/write 留给 LUX-456。

验收：

- [x] `item_actor_relation_is_current` 与 `nfo_relation_snapshot_is_current` 都比较关系 checksum。
- [x] 旧行 checksum 为 NULL、文件 checksum 改变、source fingerprint 改变或 schema version 变化都会触发重建；全部匹配才跳过。
- [x] service/storage 回归覆盖旧 NULL 行和相同 source fingerprint 下文件字节改变；storage predicate 仅在 checksum、source fingerprint 和 schema version 均匹配时返回 current。
- [x] 相关 people/storage tests、build、fmt、Clippy 与差异检查通过；PostgreSQL 未运行时如实记录。

预计文件：`src/storage/library.rs`、`src/application/people/service.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`。先锁定 checksum mismatch/legacy stale 回归，再替换 current 判定入口。

结果（2026-10-09）：service current 判断从关系文件一次读取的 raw bytes 计算 SHA-256，并要求数据库 source fingerprint、relation checksum、relation schema version 都匹配；NULL checksum、不同文件字节、不同 fingerprint 或 schema version 都判为 stale。SQLite service 与 storage 定向回归通过；rebuild 现已在 LUX-456 接入同一 checksum 判定并移除旧 current API。PostgreSQL/FNOS 未运行，未据此推断生产收益。

#### LUX-456：人物关系索引重建比较并保存快照 checksum

范围：索引重建读取新旧关系文件时保留原始字节，并只解析一次；skip 判定比较 source fingerprint、文件 checksum 和 relation schema version，重建时把该次读取的 checksum 与 credits/fingerprint 在同一事务保存。旧 checksum 为 NULL 或文件 checksum 已变化时进入现有批量重建。文件格式和 rebuild job 状态机不变。

验收：

- [x] rebuild skip 只有三维 state 与关系快照完全匹配才跳过；旧 checksum 为 NULL 或关系文件变化时重建。
- [x] rebuild credits batch 使用所读 raw bytes 的 checksum，不重读关系文件或重序列化关系对象计算。
- [x] service regression 覆盖相同 source fingerprint 下文件 bytes 变化，确认 rebuild 更新 credits 和 checksum；不改变 legacy relation 路径读取语义。
- [x] people tests、build、fmt、Clippy 与差异检查通过；PostgreSQL 未运行时如实记录。

预计文件：`src/storage/library.rs`、`src/storage/people.rs`、`src/application/people/rebuild.rs`、`src/application/people/service.rs`、`docs/LUX-DEVELOPMENT.md`。先增加 changed-bytes rebuild 回归，再贯通 raw snapshot checksum 到 batch commit，并删除不再被消费的 fingerprint-only batch wrapper。

结果（2026-10-09）：rebuild 同一次安全读取同时计算 raw-byte checksum 并解析 relation；skip 现在比较 source fingerprint、relation checksum、state schema version，batch credits commit 保存该 checksum。相同 source fingerprint 下改写 actor bytes 的回归先失败后通过，且验证 rebuild 保存值与实际文件匹配。删除无调用方 fingerprint-only batch wrapper。people service 40/40、storage checksums/current 回归通过；全量 `cargo test --locked --all-targets` 通过（798 passed、13 ignored；PostgreSQL 16 项因本机无实例而 ignored），`cargo build --locked`、fmt、全目标全 feature Clippy、`git diff --check` 通过。本机 `arm64`，未部署 FNOS/生产测量。

#### LUX-457：本地 metadata 页复用重复 episode NFO 路径检查

范围：本地 metadata 页在逐 episode 发现 NFO 时，先检查媒体同名 NFO，再回退到同目录 `episode.nfo`。同一目录的多个 episode 或多个媒体版本会重复检查相同 sidecar 路径。为一次 page 的 NFO 路径发现增加惰性存在性缓存，避免重复文件系统查询；保持同名 NFO 优先级、`episode.nfo` fallback、每个 item 的路径映射及现有错误传播语义。缓存仅在本次 page 内有效，不缓存解析内容，不改变 NFO 选择、安全校验和 metadata 写回。

验收：

- [x] 两个 episode 共用 `episode.nfo` 时都解析到相同目标，page 内重复 sidecar 目标只触发一次文件系统存在性检查。
- [x] 同名 NFO 仍优先于 `episode.nfo`；存在性检查错误仍按原逻辑终止该 item 的 NFO 查找。
- [x] NFO 路径发现和剧集 metadata 定向回归、fmt、全目标全 feature Clippy、全量 `cargo test --locked --all-targets` 与 `git diff --check` 通过；不推断 NAS/FNOS 墙钟或 CPU 收益。

短计划与文件：修改 `src/application/metadata.rs`，给本地 NFO page 增加仅 page 生命周期的 sidecar existence cache，并回归共享 fallback 与同名优先级；修改 `docs/LUX-DEVELOPMENT.md` 与 `docs/PERFORMANCE.md` 记录调用边界及限制。先加可观测 probe-count 回归，再接入缓存，最后运行库内 metadata 与剧集 NFO 目标。

结果（2026-10-09）：一次本地 scan NFO page 用惰性 `PathBuf` cache 复用 episode sidecar 的 `try_exists` 结果；三 episode 回归确认两项共享同一 `episode.nfo`，第三项仍优先同名 NFO，page 对四个唯一候选路径各检查一次。OnDemand 单 item 入口保留独立 cache，缓存不跨 page；文件系统错误仍被 `Option` 传播并停止该 item 的 fallback 查找。新增 metadata library regression 与 `series_metadata` 3/3 通过；第二轮全量 `cargo test --locked --all-targets` 通过，`cargo build --locked`、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。PostgreSQL 专项 16 项因本机无实例而 ignored。本机 ARM64，未测量 NAS/FNOS 文件系统墙钟或 CPU。

#### LUX-458：有界并发扫描本地 metadata NFO 路径

范围：LUX-419 已将 series/season/episode NFO enrichment 限制为最多 4 路并发，LUX-457 已按 page 复用重复 episode sidecar 检查，但 `scan_local_metadata_nfo_paths` 仍按 source 串行做路径发现。将一次 source page 的 NFO 路径发现改为最多 4 路并发，并按原 source 顺序归并路径与错误。多个任务检查同一 sidecar 路径时共用同一个异步结果，包括同目录 `episode.nfo`、重复 series/season 候选；缓存只活到该 page。保留每类原有候选优先级、hierarchy 仅由每个 series/season 的首个 source 决定、VIDEO 显式 I/O 错误、其他 NFO 查找错误通过 `Option` 终止 fallback 的行为。OnDemand 单项路径和后续 NFO enrichment/page metadata transaction 不变。

验收：

- [x] local scan NFO 路径发现的并发不超过 4，source-order 路径/error 结果稳定；测试能观察到并发大于 1。
- [x] 多个 episode 并发检查共享 `episode.nfo` 时，该 page 只执行一次底层 existence probe；同名 NFO 优先级、hierarchy 首 source 选择和 path errors 保持。
- [x] 不同 series/season ID 指向同一 hierarchy NFO 候选路径时，共享 page cache 只执行一次底层 existence probe。
- [x] NFO path discovery、`series_metadata`、`scanned_series_metadata` 定向回归、fmt、build、全目标全 feature Clippy、全量 `cargo test --locked --all-targets` 与 `git diff --check` 通过。
- [x] 性能记录只报告固定 page 的 probe 次数和并发上界，不外推 NAS/FNOS 墙钟、PostgreSQL 或 CPU 收益。

短计划与文件：修改 `src/application/metadata.rs`，用现有有界 task runner 执行拥有型 path-discovery request，以异步 per-path cell 合并同页重叠 probe，并按输入顺序合并结果；修改 `docs/LUX-DEVELOPMENT.md` 与 `docs/PERFORMANCE.md`。先增加 concurrency/probe/order 回归并确认旧串行实现使其失败，再接入 bounded path discovery，最后运行剧集 NFO 定向目标。

结果（2026-10-09）：新增 12-episode fixture 在旧串行实现下先失败（最大并发为 1）；有界执行后观察到并发大于 1 且不超过 4，12 个 item 共用的 `episode.nfo` 只检查一次，同名 NFO 优先级和 series/season 首个 source 选择保持。另一 fixture 先在旧实现下以 3 次 cache probe 对 5 次期望失败，随后确认两个 episode 以及重复 hierarchy ID 路径合计只检查 5 个唯一候选。路径发现单测 6/6、`series_metadata` 3/3、`scanned_series_metadata` 2/2 通过；全量 `cargo test --locked --all-targets` 通过，库测试 802 passed、13 ignored，PostgreSQL 专项 16 项因本机没有实例而 ignored。`cargo build --locked`、fmt、全目标全 feature Clippy 与 `git diff --check` 通过。本机 ARM64；未测量 NAS/FNOS 文件系统墙钟或 CPU，也未据本地 SQLite 测试外推 PostgreSQL 收益。

#### LUX-459：用完整 NFO 语义 fingerprint 跳过等价内容重处理

范围：现有 NFO cache 用原始字节 fingerprint 做无变化快路径；字节因 XML 注释、属性顺序或格式化空白变化时，仍会重新解析，并可能重复写 rich metadata 与人物关系。增加完整 XML 文档的语义 fingerprint，在保留原始字节快路径的同时，识别仅词法变化且 XML 语义未变的 NFO。语义 fingerprint 必须覆盖未知节点、未知属性与文本，不能复用仅比较已知 projection 的 NFO writer fingerprint；忽略 XML 声明、注释、属性顺序、空元素写法差异和纯 element-only 格式空白，将 CDATA 与相同文本按相同内容处理。继续遵守 NFO 字节数、事件数和禁止 DTD 的解析限制。旧 cache 中没有 semantic fingerprint 时安全地完整处理一次并写入新格式。只有 cache、默认字段和基于旧 raw fingerprint 的人物关系快照均可复用时，才更新当前 raw/stat fingerprint 并跳过 metadata 与 relation 写入；语义内容变化或快照过期仍走原 enrichment。

验收：

- [x] 相同 XML 内容在注释、属性顺序、CDATA/文本表示、空元素写法或纯格式化空白变化后得到相同 semantic fingerprint；metadata enrichment 跳过重复 metadata/person relation 写入并更新当前 revision state。
- [x] 已知字段变化、未知节点/属性/文本变化都会产生不同 fingerprint 并执行正常 enrichment；NFO 原始字节不被重写或规范化。
- [x] 原始字节完全相同的 fingerprint 快路径保持；没有 semantic fingerprint 的旧 cache 安全回退一次并可生成新 cache；资源限制和非法 XML 行为保持。
- [x] NFO fingerprint 单元回归与 `cargo test --locked --test metadata`、fmt、Clippy 和差异检查通过。性能记录明确区分文件读取、XML 解析和后续数据库/关系写入，不把本机正确性测试描述成 FNOS/NAS 性能收益。

预计文件：`src/application/nfo.rs`、`src/application/metadata.rs`、`src/application/candidates.rs`、`tests/metadata.rs`、`tests/reidentify.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先增加 lexical/unknown XML/metadata-state 回归，再实现有界、保留未知结构的 semantic fingerprint 与 cache 兼容，最后运行定向 NFO 与 metadata 验证。

进度（2026-10-09）：核心实现及 lexical、未知 XML、metadata state、旧 cache、`xml:space` 和属性规范化回归已补。cache-enabled projection 保留字段内部空白，projection-only 路径仍保持既有行为；属性规范化区分字面 TAB/换行与字符引用，并将 CRLF 与 LF 字面换行按相同值处理。cache-enabled 解析接受合法预定义/数字引用进入 metadata projection，并拒绝未声明 general entity。首次全量测试发现完整性规划仍按旧 JSON 结构解码 rich cache，导致完整本地 NFO 被误判缺少 credits/trailers 并发起候选搜索；现改为复用版本化 cache decoder，`fill_missing_skips_complete_movie_without_scraper_request` 回归通过，旧格式兼容不变。NFO 单测 8/8、metadata 单测 21/21、`tests/metadata.rs` 21/21 与 `tests/reidentify.rs` 16/16 通过。最终复跑 `cargo test --locked --all-targets` 成功：library 807 passed、13 ignored；所有 integration targets 通过。`cargo build --locked`、fmt、全目标全 feature Clippy 与 `git diff --check` 通过；PostgreSQL 专项因本机无默认实例而 ignored。
#### LUX-460：扫描本地 metadata page 普通电影 NFO 有界并发

范围：scan-local metadata NFO page 当前逐个 await 普通电影的 NFO enrichment。复用现有有界有序 task runner 和电影 NFO 并发上限（最多 4 路），让一个 page 内独立电影并发处理，并按原 source 顺序归并结果。单个 NFO 失败或 task 异常仍只影响对应 item。page 级延迟 metadata 更新继续在所有电影、home video 与剧集处理后统一 flush；actor credits 继续使用共享 deferred collector 并在 page 末统一 flush。只修改 scan-local page 的 MOVIE 路径；库级 `enrich_movie_sources`、home-video、series/season/episode 和 OnDemand 路径不扩大范围。

身份变更的电影在同一 page 共享的异步 guard 下检查数据库现存身份及 deferred collector 中尚未 flush 的同页身份 reservation，然后完成 NFO cache/actor relation 写入和 metadata update 入队；入队后释放 guard。pending reservation 的冲突候选仍由数据库按当前 parent、可用 source 和 active 状态条件复核。冲突检查不触发 page metadata flush；只有电影 title/year 变化的 item 等待该 guard。这样并发 item 使用旧 page snapshot 时，仍可保留 page-end batching 并避免同页重复身份入队。

验收：

- [x] 回归证明 scan-local 普通电影 NFO enrichment 的最大并发大于 1 且不超过 4，结果合并顺序与输入 source 顺序一致。
- [x] 一个电影 NFO 无效或 task 失败时，其余电影仍完成；单 item 错误隔离语义保持。
- [x] 两个并发电影将改为相同 title/year 时只允许一个身份更新入队，另一个作为不可重试冲突单独失败；同页普通电影仍成功，且冲突检查顺序由回归同步控制。
- [x] 冲突检查期间已有的普通 metadata update 不提前 flush；普通更新与唯一成功身份更新在 page 末同一事务 flush，actor credits 仍只在 page 末统一 flush；metadata batch transaction 失败后的逐 item fallback 回归通过。
- [x] 只改变 scan-local page 的 MOVIE 路径；series、home-video、OnDemand 和库级电影 enrichment 行为不变。
- [x] 相关 metadata 定向回归、格式、build、Clippy、全量 Rust 完成门及 `git diff --check` 通过。
- [x] 性能记录仅报告固定 task runner 的并发上界和行为边界，不推断墙钟、FNOS CPU、PostgreSQL 或 NAS 收益。

短计划与文件：先在 `src/application/metadata.rs` 为 scan-local movie page 增加有界并发、顺序、错误隔离、身份冲突串行化及 page-end flush 回归，再将该路径接入现有有界有序 runner，同时保留 page flush 边界；在 `src/storage/media.rs` 按既有 movie identity eligibility 复核同页 pending reservation；更新两份任务和性能记录。预计修改 `src/application/metadata.rs`、`src/storage/media.rs`、`docs/LUX-DEVELOPMENT.md` 与 `docs/PERFORMANCE.md`，共 4 个文件。

实现进度（2026-10-09）：scan-local MOVIE NFO 已接入最多 4 路有序 runner；并发、错误隔离、同页 identity reservation 与 page-end flush 回归均覆盖。身份 guard 跨越数据库与 page pending reservation 检查、cache/relation 写入和 metadata update 入队，并在入队后释放；storage 按 parent、available source 和 active 条件复核待检查 reservation。fallback 用例确认同页两项先参加一次失败 page batch、再逐项回退，故指标中的事务数为 3、item/update 尝试数为 4；与旧提前 flush 路径相比，page metadata 不会在 identity 检查中提前提交。定向 metadata 21/21、build、Clippy 与全量 Rust 测试通过；性能记录只报告固定 runner 行为，不外推生产性能。
#### LUX-461：合并高频 Admin Jobs 进度失效通知

范围：扫描活动项更新、Manifest `DISCOVERY_PROGRESS` 和 metadata reidentify 进度会反复使 Admin Jobs SSE 失效。为这些进度通知增加有界合并：首个通知立即发送，持续更新最多每秒补发一次尾随失效。任务创建、取消、错误和完成等关键状态变化继续立即发送；即时发送要取消旧进度窗口的待发尾随通知，确保结束状态刷新及时。事件 payload、SSE 协议和任务持久状态不变。

验收：

- [x] burst 内首个 Jobs 进度失效立即发送，后续事件合并；有持续更新时每秒最多发送一次尾随失效。
- [x] 即时 Jobs 状态事件在调用时发送并撤销已排队的进度尾随事件，最终完成状态不会被合并丢失或延迟。
- [x] 只合并扫描活动项、`DISCOVERY_PROGRESS` 与 reidentify progress；JOB_CREATED、取消、失败、阶段完成和任务完成仍即时发送。
- [x] AdminEventHub 单测覆盖 burst、尾随刷新和即时刷新。
- [x] 相关应用路径编译/测试、全量完成门通过；`git diff --check` 与 rustfmt 检查通过。

短计划与文件：先在 `src/application/admin_events.rs` 添加通知时序回归，再增加仅用于 Jobs 进度的 1 秒有界 coalescing 和可取消尾随刷新；在 `src/application/scanner.rs` 接入扫描活动项与发现进度，在 `src/application/reidentify.rs` 接入已节流的重识别进度。更新本文件记录边界和结果。预计修改文件共 4 个。

结果（2026-10-09）：Jobs 进度现在首个即时广播，窗口内后续变更置脏，由每秒一次的尾随广播合并刷新；连续压力最多每秒广播一次。任意即时 Jobs 发布会使旧窗口失效并清空待发尾随刷新。扫描活动项、`DISCOVERY_PROGRESS` 与 reidentify 进度接入该路径；其他扫描事件和任务创建/取消/完成继续即时刷新。AdminEventHub 单测 6/6、集成目标 `admin_events` 3/3 通过；build、全量 Rust 测试、fmt、全目标全 feature Clippy 与 `git diff --check` 通过。未在 FNOS 或生产负载上验证效果。

#### 本轮代码质量与性能优化收口

本轮修复范围截至已登记的 LUX-389；修复期间继续发现的候选不自动追加到本轮。后续优化应先记录调用频率、数据规模、预期收益与风险，再建立下一轮固定清单；剩余任务数和进度按各轮清单分别报告。

本轮的 SQL 调用数减少不等同于生产时延或硬件占用的改善。阶段 23 的扫描、海报可见性、双后端 A/B、前台 p95 与项目所有者确认仍按下方阶段门单独验收。
#### LUX-383：批量清理 Web HLS 播放会话

范围：Web HLS 会话清理当前先取最多 128 个过期或无心跳会话，再逐会话执行条件 UPDATE，单次清理最多产生 129 次 SQL。改为保留有界候选读取后，用一次参数化 `UPDATE ... RETURNING` 批量停止仍满足条件的会话；保留并发条件复核、按候选顺序返回成功停止的会话、最多 128 条上限和 HLS 目录清理语义。不改变播放会话表结构或 API 合同。

验收：

- [x] 130 个过期会话和 130 个无心跳会话各只停止前 128 个，剩余 2 个保持 active；两条路径的 storage SQL 调用均由 129 次降为 2 次。
- [x] 条件 UPDATE 只返回仍为 active 且满足过期/无心跳条件的会话；返回顺序保持候选读取顺序，清理任务继续只处理实际停止的 HLS 目录。
- [x] 播放会话、Web 播放和存储回归通过；不改变状态机、会话上限、并发竞态保护或 PostgreSQL/SQLite 通用 SQL 边界。
- [x] 性能记录只报告固定 SQLite fixture 的 SQL 调用数，不推断 HLS 文件清理墙钟、PostgreSQL、NAS 或生产收益。

依赖：无。预计文件：`src/storage/sessions.rs`、`src/storage/repository_tests.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先以 130 会话回归锁定逐会话 UPDATE 基线，再接入有界批量停止。

结果（2026-10-05）：过期与无心跳清理均由候选 SELECT 加逐会话 UPDATE 收敛为候选 SELECT 加一次 `UPDATE ... RETURNING id`，固定 130 会话 fixture 从 129 次降为 2 次 SQL；只返回仍满足条件的会话并按原候选顺序交给 HLS 目录清理。本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产负载。

#### LUX-384：批量重建插件卸载后的媒体库刮削器顺序

范围：卸载刮削器插件时，当前先删除插件行，再对每个受影响媒体库单独读取剩余行、删除全部配置、逐行重插、读取主刮削器并更新媒体库，媒体库数量和配置行数会放大 SQL 调用。改为一次读取受影响库的完整配置，在内存中完成角色/位置重建，再按有界批次删除、插入和更新；保留主刮削器兼容字段、PRIMARY 转 BACKUP 规则、空配置行为、章节源清理和插件删除事务边界。

验收：

- [x] 205 个媒体库、每库 3 个刮削器的卸载由 1,234 次 SQL 调用降为 12 次；插入和更新批次均有上限。
- [x] 删除后剩余刮削器从位置 0 重新编号，首项成为 PRIMARY，原 PRIMARY 被移除后的后续 PRIMARY 降为 BACKUP；`libraries.scraper_id` 与空配置行为保持一致。
- [x] 插件卸载 API、弹幕插件卸载、存储迁移和媒体库刮削器回归通过；不改变插件协议、schema 或章节源清理合同。
- [x] 性能记录只报告固定 SQLite fixture 的 SQL 调用数，不推断插件文件删除墙钟、PostgreSQL、NAS 或生产收益。

依赖：无。预计文件：`src/storage/users.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。先增加多库卸载查询计数回归，再接入内存重建和有界批量写入。

结果（2026-10-05）：受影响库配置由逐库读取/删除/重插/主项回读改为一次快照读取、一次批量删除、5 次批量插入和 3 次批量更新；固定 205 库 fixture 从 1,234 次降为 12 次 SQL。本机 `uname -m=arm64`，未实测 PostgreSQL 墙钟、NAS 或生产负载。

#### LUX-383：普通本地图片登记与 fallback 原子提交

范围：电影、剧集、季度和分集的普通本地图片索引复用 LUX-305 有界图片事务，把图片 upsert 和 poster fallback 清理合并为一个事务。保留图片命名、索引、legacy fanart 排除和处理顺序；不改变文件读取、schema 或在线任务合同。

验收：

- [x] 图片登记成功与 fallback 清理原子完成；注入 fallback 更新失败时图片不部分入库。
- [x] 重复登记幂等，既有 metadata、series metadata 图片路径回归通过。
- [x] 格式、相关 Clippy 和定向测试通过；性能记录只说明事务边界，不外推 FNOS CPU。

预计文件：`src/application/metadata.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。

#### LUX-384：通用 FILL_MISSING 创建入口去重与队列合并

范围：通用 `create_fill_missing_job` 入口此前直接创建新 job，可能绕过本地完整性路径的活动任务去重。按同一媒体库在事务内过滤 QUEUED/RUNNING/近期 DEFERRED 条目，并将新条目合并到现有 queued job；跨库请求保留原有独立 job 语义，不改变最多 100 项限制和执行前再次检查。

验收：

- [x] 同一库、同一条目重复创建只保留一份活动 job；新条目合并进已有 queued job。
- [x] 取消、运行中、近期 deferred 条目不会重新排队；跨库请求保持原有行为。
- [x] storage、metadata、reidentify、格式和 Clippy 回归通过；性能记录只说明任务创建边界，不外推 FNOS CPU。

预计文件：`src/application/reidentify.rs`、`src/storage/jobs.rs`、`src/storage/media.rs`、`src/storage/repository.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。

#### LUX-385：清理取消 metadata job 的残留 item 状态

范围：取消 metadata job 时，将仍为 `PENDING/RUNNING` 的 item 统一写成既有可重试终态 `FAILED`，并记录 `JOB_CANCELLED`；新增 migration 幂等清理历史取消 job 的残留行。显式 retry 仍将这些 item 置回 `PENDING`，不改变任务公共 API。

验收：

- [x] 新取消 job 不再留下 `PENDING/RUNNING` item；retry 后 item 恢复为 `PENDING`。
- [x] migration 可从空库运行，并能幂等修复历史取消 job，不触碰已完成 item。
- [x] metadata cancel、storage、build、格式和 Clippy 回归通过。

预计文件：`src/storage/jobs.rs`、`tests/metadata_cancel.rs`、`tests/storage.rs`、`migrations/0158_media_info_chapters.sql`、`migrations-postgres/0158_media_info_chapters.sql`、`migrations/0160_scan_local_metadata_backfill_non_retryable_items.sql`、`migrations-postgres/0160_scan_local_metadata_backfill_non_retryable_items.sql`、`migrations/0161_reconcile_cancelled_metadata_job_items.sql`、`migrations-postgres/0161_reconcile_cancelled_metadata_job_items.sql`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。

发布兼容性补正（2026-10-05）：部署库已使用 migration 0158 保存媒体章节、0160 保存本地 metadata backfill。保留这两条历史迁移及其 checksum；取消任务残留清理使用新版本 0161，避免升级时发生 SQLx 版本/校验和冲突。

#### LUX-386：跳过 unchanged NFO 的空默认值修复事务

范围：NFO fingerprint 未变化且 rich NFO/人物缓存可用时，复用本轮已读取的媒体元数据快照判断本地 provider IDs 和 premiere date 是否仍有缺失。两类默认值均已存在时不再进入存储事务；发现缺失时继续调用存储层并在事务内复核后修复。保留 NFO fingerprint、缓存恢复和人物关系同步语义。

验收：

- [x] 默认值完整的 unchanged NFO 路径省去空修复查询/事务；查询计数回归证明调用数下降。
- [x] 缺少 premiere date 或 provider ID 时仍按原逻辑补齐；已有值不覆盖。
- [x] metadata、NFO cache、格式、build 和 Clippy 回归通过；性能记录不外推 PostgreSQL/NAS/生产墙钟。

预计文件：`src/application/metadata.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先写 unchanged NFO 查询计数回归，再增加保守的快照判断。

结果（2026-10-05，`0f4dcdcc`）：unchanged NFO 且 rich cache 可用时，先用本轮已读取的 metadata 快照判断 provider IDs 与 premiere date；两者已有值就跳过修复存储调用，仍缺字段则走原子修复事务。回归覆盖两者都缺、仅 provider ID 缺、仅 premiere date 缺及已有值不得覆盖。定向 metadata/NFO cache 测试、全目标 Rust 测试、build、fmt 与 Clippy 通过。SQLite 单项测试查询调用从 4 次降到 3 次；该计数不是墙钟指标，也不外推 PostgreSQL、FNOS 或 x86 性能。详见 `docs/PERFORMANCE.md`。

#### 阶段 23 总体验收与阶段门

2026-10-10 复核：阶段 23 仍开放，六项证据、失败和关闭条件见[阶段 23 证据复核](STAGE-23-GATE-REVIEW-2026-10-09.md)。冻结 `32c158ec` 已完成公平请求计时修复、provider 空候选专项和常规 Rust 全门（1514 passed、0 failed、36 ignored）；LUX-469 的最终三个目标分别 22/22、3/3、81/81。原不公平 1k A/B 不作产品回退证据。

原始三轮矩阵有 3 对发现 Cargo 干扰；只补齐 LUX-270/10k SQLite，现存 44 行诊断样本，两组 SQLite poster 各少一轮。基线默认 scan 并发 16、冻结候选 2，最新 `test=0980dd36` 已改为 4；相同配置最终 A/B 未运行，不能判定实现回退或性能达标。逐请求 cohort、完整扫描窗口 null、本地/在线独立时段及 SQL/pool/RSS 数据见 `PERFORMANCE.md`，5% 门槛保留。

在线补缺 1k 两后端均真实处理 10 jobs/1000 items，在 provider gate 关闭时 scan/local 完成；10k SQLite 却被容量 32 dispatcher 背压卡住本地 worker，600 秒后超时。LUX-424 的提交方背压与本阶段/LUX-303 的不阻塞本地 worker 存在冲突；已按 AGENTS.md Source of truth 报告，在选择修订合同前不改变公共调度/存储边界。10k PostgreSQL 尚未运行。

另一会话已集成冻结提交链并推送到 `test=0980dd36`；本轮运行证据不覆盖其后续修改。基于该 revision 的 `18b88888` 的首页组件回归验证 mock 空列表→home 通知→电影标题/poster URL；Web Node134/Vitest570/build 通过。该测试未建模 scan lifecycle、未请求或解码 poster，不证明浏览器与真实 scan 的端到端行为，新的最终源码还须重跑 Rust/性能门。所有技术验收通过后才按 AGENTS.md Stage gates 请求项目所有者确认；当前不关闭、不进入后续阶段。

最终远端刷新又观察到 `origin/test=b4635ee3`（PR #43 的 PostgreSQL movie merge 修复），同样不在本轮 Rust/性能证据的覆盖范围内；记录工作树保持基于 `0980dd36`。

#### LUX-462：补全阶段 23 基准的 SQL 延迟、连接池与后台队列观测

范围：LUX-270 当前记录 SQL 调用数和 manifest 内部阶段，但没有 SQLx 执行时间分布、测量期间的连接池压力或目录 API 冷/热页面对照，无法把用户请求 p95 回退定位到 cache miss、数据库执行或连接池等待。LUX-304 的“本地海报队列完成”只依据 poster 行数；改为同时确认该 scan job 的全部本地图片阶段已有完成标记，并单独呈现扫描结束后但图片仍在排队的目录 API p95。只改性能测试 harness 与性能记录，不增加生产指标、不改变扫描或 API 行为。所有 SQL 汇总仅使用规范化 statement summary，不采集 bind 值或完整 SQL。

验收：

- [x] LUX-270 结果包含按规范化 statement summary 分组的 SQLx 调用数、累计时间、p50/p95/max；时间为空时明确报告不可用，不伪造零耗时。
- [x] 前台管理请求与目录请求各自测量期间记录连接池 `size/idle/in-use/saturated` 峰值或采样计数；目录请求增加预热缓存后的对照，以区分 cache miss 等待与热页 hydration/query 负载。
- [x] LUX-304 区分 scan-active、scan-finished/image-pending 和所有 scan job image batch 完成后的目录列表 p95；队列完成条件同时核验本地 poster 数与 `images_completed_at`，失败或残留批次不得被误记为 drain 完成。
- [x] 固定统计 helper 单测、LUX-270/LUX-304 小 fixture SQLite 定向运行、fmt、build、全目标 Clippy 和差异检查通过。
- [x] 性能文档明确记录 ARM64 SQLite 单轮观测、临时 PostgreSQL 历史样本及观测边界；当前 LUX-304 1k/10k 非 A/B 结果不用于关闭既有回退或性能阶段门，也不外推 FNOS/NAS。

预计文件：`tests/performance.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先为 SQL 延迟摘要统计添加确定性单测，再采集 query/连接池样本，接着为 LUX-304 增加严格 image-stage drain 和中间阶段测量；使用 1k SQLite 先确认输出完整后再决定是否运行双后端全矩阵。

进度记录（2026-10-09）：基准 harness 已接入规范化 SQLx 延迟摘要、5 ms 连接池采样、管理/目录请求各自的 pool 观测、目录页首次与热页对照，以及 LUX-304 scan 完成后 image-pending 采样和严格 image batch drain。SQL 延迟对象显式标注 phase window 和后台 SQL 可能混入；单次首次目录请求只记录单次延迟。image-pending 只接受请求窗口首尾都存在有效 pending/running batch 的样本；失败/取消不计作 pending，已完成但缺少 `images_completed_at` 会使 drain 立即失败。pool 无样本报告保持 unavailable，所有未观测数值为 null。helper 测试 10/10 通过。LUX-270 与 LUX-304 的 1k SQLite fixture 通过；另有 10k SQLite LUX-304 fixture，扫描中、image-pending、strict drain 后三个窗口均记录目录 p95，完整输出见 `docs/PERFORMANCE.md`。这些是单轮 ARM64 SQLite fixture 检查，不是 A/B 或性能收益结论；原 1k 回退和阶段 23 双后端 A/B 仍待修复/复测。`cargo build --locked`、fmt、全目标全 feature Clippy 和全量 Rust 测试完成门通过；本机无 PostgreSQL 实例的专项测试保持 ignored。上述数据不外推 FNOS/NAS。

#### LUX-463：将 scan-job lifecycle 状态并入本地 metadata page 读取

范围：LUX-443 已把 pending target 与有界 source page 合并读取，但本地 metadata worker 启动时仍单独读取一次 scan job；空闲 fallback 到期时又单独刷新 scan job，之后回到 page 查询。扩展现有 `load_scan_job_metadata_page` 单次查询，同时返回 job 是否存在、job type、job status、`auto_metadata_match`、pending 状态和首个有界 source page。worker 使用该 page lifecycle 判定不存在、FAILED、CANCELLED 的退出路径，并读取 `INCREMENTAL_SCAN` 类型及 completeness policy；删除独立的初始 `find_scan_job`、fallback refresh 和增量 completeness policy 的重复 job 查询。保留没有 page snapshot 的其他调用点原有策略解析、`Notify`、stop watch、5 秒跨进程状态 fallback、page 顺序、source unavailable 失败语义，以及每个 pending page 的 manifest root 身份复核。

验收：

- [x] page storage query 一次返回 lifecycle、`auto_metadata_match` 与既有 pending/source page；job 不存在可与空 job 区分，job status/type/auto-match 不引入第二次 query。
- [x] worker 启动和每次 Notify/fallback 后只通过 page 查询读取最新 lifecycle；missing、FAILED、CANCELLED 的退出行为与原实现一致，INCREMENTAL_SCAN completeness 从同一 page snapshot 取策略，不重复读取 job。
- [x] query-wrapper 回归证明 worker 启动与 Notify 分别只产生一次 lifecycle/page 查询，completeness policy 使用 page snapshot 时不增加查询；storage 回归覆盖 auto-match 开关、存在、终态和不存在 job，同时保留 target 优先级与无 source pending 行为。
- [x] root identity 校验、stop watch、Notify 即时唤醒与 5 秒跨进程 fallback 语义保持；相关 scanner/storage 测试、fmt、build、Clippy 和 `git diff --check` 通过。
- [x] 不把 query-wrapper 调用数变化描述成 SQL 墙钟、FNOS/NAS CPU 或 PostgreSQL 生产收益。

短计划与文件：先扩展 `StoredScanJobMetadataPage` 并在原有单条 page query 返回 job lifecycle 与 `auto_metadata_match`，添加 storage 回归；再将 scanner worker 的启动/刷新状态判断及增量 completeness policy 切到 page snapshot，并扩展 idle worker query-count 回归。预计修改 `src/storage/repository.rs`、`src/storage/jobs.rs`、`src/application/scanner.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md`，共 5 个文件。

进度记录（2026-10-09）：page lifecycle 合并 query 额外返回 `auto_metadata_match`，增量 metadata completeness 复用 page snapshot 解析策略，不再第二次读取 scan job；没有 page snapshot 的调用点保留既有策略读取路径。query counter 回归先复现了 1 次冗余 query，修复后策略解析增加 0 次 query；storage 覆盖 enabled flag、空 page、终态和不存在 job。相关定向测试、`cargo build --locked`、fmt、全目标全 feature Clippy、`git diff --check` 和 `cargo test --locked --all-targets` 通过；library 为 808 passed、13 ignored，需本地 PostgreSQL 的集成测试与手动性能基准仍按其条件 ignored。以上是调用次数与本机测试证据，不推断 SQL 墙钟、FNOS/NAS CPU 或 PostgreSQL 生产收益。

#### LUX-464：指定条目重识别的稳定去重回归

范围：为原性能审计清单第 13 项补齐回归保护。指定条目重识别收到重复 item ID 时，只保留每个 ID 的首次出现，稳定保留其余首次出现的输入顺序，并按唯一条目数创建任务。不得改变请求合同、数据库模型或 job item 的对外排序。

验收：

- [x] 单元回归证明去重后保留首次出现顺序，例如 `[b, a, b, c, a]` 得到 `[b, a, c]`。
- [x] 集成回归提交含重复 ID 的任务，确认任务总数等于唯一 ID 数，且持久化 job item 中每个唯一 ID 恰好出现一次。
- [x] `cargo test --locked --lib reidentify::tests::metadata_reidentify_deduplication_keeps_first_occurrence_order`、`cargo test --locked --test reidentify`、build、fmt、Clippy 和 `git diff --check` 通过。

短计划与文件：将现有保序 HashSet 逻辑抽成私有纯函数并先添加单元回归；再在 `tests/reidentify.rs` 通过真实 SQLite 数据库创建含重复 ID 的重识别任务，检查计数和持久化条目；最后记录测试结果。预计修改 `src/application/reidentify.rs`、`tests/reidentify.rs`、`docs/LUX-DEVELOPMENT.md`，共 3 个文件。

结果（2026-10-09）：保序去重 helper 的单元回归 1/1 通过；SQLite 集成用例确认 5 个输入 ID 去重为 3 个 job item、`total_count` 为 3，持久化 ID 与 3 个唯一 ID 一致；service 返回的 `job.items` 仍按既有 `item_id` 顺序排序。`tests/reidentify.rs` 17/17、`cargo build --locked`、全目标全 feature Clippy、fmt 与 `git diff --check` 通过。一次并行全量测试中 `libraries_api` 的库更新用例返回 `DATABASE_UNAVAILABLE`；随后隔离重跑该用例和 `tests/libraries_api.rs` 全目标（13/13）通过。该全量测试的串行重跑尚待最终阶段门执行。

#### LUX-465：合并本地 metadata 完成 waiter 状态查询

范围：完成等待器每次被通知或 5 秒 fallback 唤醒时，先查询是否还有 PENDING metadata target；有待处理 target 时又分别读取 scan job 是否存在和 cancel 状态。将三次读取合并为一个 SQLite/PostgreSQL 共用的 storage 查询，返回 pending、取消或缺失 job 状态。等待器在读取状态前启用 `Notify` future，避免 `notify_waiters` 恰好发生于状态读取期间而丢失；同进程完成和取消通知都即时唤醒，内存取消标记、5 秒跨进程 fallback 和既有 stop 行为保持不变。

验收：

- [x] 固定 SQLite storage fixture 将 pending 状态及 job 取消检查从 3 次 query-wrapper 调用收敛到 1 次；无 pending、已取消和不存在的 job 仍退出等待。
- [x] `Notify` 在状态查询前完成注册；同进程完成及取消通知都在 250ms 内唤醒，跨进程 fallback 仍为 5 秒。
- [x] 相关定向测试、build、fmt、全目标全 feature Clippy 和 `git diff --check` 通过。
- [x] 性能记录只描述固定 fixture 的 query-wrapper 数量，不外推 PostgreSQL 时延、FNOS/NAS 或生产 CPU 收益。

预计文件：`src/storage/jobs.rs`、`src/storage/repository_tests.rs`、`src/application/scanner.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先添加单 query 与状态语义回归，再合并 storage 查询并在 DB read 前注册通知 future。

结果（2026-10-09）：旧实现红灯回归观测到每次状态检查 3 次 query-wrapper 调用；合并后 storage 状态查询为 1 次。SQLite 回归覆盖 pending、complete、cancel requested 与 missing job；completion/cancellation waiter 通知 250ms 回归及固定 5 秒 fallback 测试通过。`cargo test --locked --lib local_metadata_completion`、`cargo build --locked`、fmt、全目标全 feature Clippy 与 `git diff --check` 通过。本机 ARM64；只验证 query-wrapper 计数和本地通知边界，没有 PostgreSQL/FNOS/NAS 时延或 CPU 收益证据。跨进程状态继续由最多 5 秒 fallback 发现。
#### LUX-469：补齐渐进扫描故障与竞态回归

范围：阶段 23 的故障清单需要区分已覆盖路径与真正未覆盖路径。为扫描本地图片 worker 增加首个 poster 读取阻塞、电影/剧集 poster 权限拒绝、outbox 发布后媒体根暂时不可用的 fixture；并检查已 claim 的 outbox 在 scan job 取消时是否会越过 READY/completeness 提交。I/O 读取错误或根不可用只能使图片批次可重试，不能将 image stage 标为完成或写入 `POSTER` 完整性结论；恢复后同一批次应成功。扫描索引应在本地图片读取阻塞时继续运行。复用现有慢 NFO、页后段图片事务失败、不可用根、取消/重试、全量/增量 CAS 与增量扫描图片回归，不重复造等价覆盖。fixture 只使用临时目录，不读取外部媒体库或 FNOS 数据。

验收：

- [x] 首个 poster FIFO 读取被阻塞时，workflow 3 仍完成扫描索引；释放 FIFO 后本地图片批次完成且两项 poster 可查。
- [x] 电影和剧集 poster 权限拒绝都使本地 outbox image stage 失败且可重试；恢复权限后同一批次完成，故障期间不写入 `POSTER` completeness 结论。
- [x] outbox 已发布后暂时移走 fixture 根路径会使图片批次失败且不确认缺失；恢复根路径后可重试成功。
- [x] 图片 FIFO read 已打开且 batch 为 RUNNING 时取消 scan job，会先取消该 job 的未完成 outbox；释放读闸门后不能提交 image READY/completeness。
- [x] 结果记录把慢 NFO、后段海报写事务失败、扫描取消/重试、不可用根恢复、全量/增量 CAS、扫描期间增量补图及未访问目录新增 poster 映射到现有测试；未覆盖的竞态明确保留为开放项。
- [x] 运行三个定向 Rust 集成目标（串行测试线程）、`cargo build --locked`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 与 `git diff --check`。

预计文件：`src/application/metadata.rs`、`src/application/scanner.rs`、`tests/scanned_metadata.rs`、`tests/scanned_series_metadata.rs`、`docs/LUX-DEVELOPMENT.md`。先分别写电影/剧集权限拒绝、首项图片阻塞、取消 outbox 与根离线回归，确认旧行为，再修复只记录日志却将 image stage 当成功的路径及生产取消未终结其 local outbox 的路径；最后完成定向验证。

进度记录（2026-10-09）：电影 `unreadable_local_poster_keeps_image_check_retryable` 与剧集 `unreadable_series_poster_keeps_local_image_batch_retryable` 先复现旧行为：chmod 000 的 poster 被跳过，outbox 错误地完成图片阶段；修复后两项定向回归各 1/1 通过，并确认故障时无 POSTER completeness、恢复权限后同一批次成功。`cancelling_scan_during_running_local_image_claim_cancels_before_completeness` 在确定 outbox 已发布且 worker 阻塞在 FIFO 后复现批次仍为 RUNNING；取消流程现在先取消该 job 的未完成 outbox，修复后定向回归 1/1 通过，释放 FIFO 后不会完成图片阶段或写 completeness。`blocked_first_local_poster_does_not_block_manifest_scan` 与 `unavailable_media_root_keeps_local_image_batch_retryable` 分别 1/1 通过；前者通过非阻塞 FIFO writer 成功打开确认 image reader 确实阻塞，再验证 workflow 3 扫描完成、释放后两张海报可查；后者验证 root 暂离线时 batch FAILED、未写 `images_completed_at`/POSTER completeness，恢复同一路径后同一批次成功。电影权限、剧集权限、取消竞态与两个 fixture 共五项新回归均已绿。完整目标 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test scanned_metadata --test scanned_series_metadata --test scanning_jobs -- --test-threads=1` 通过：`scanned_metadata` 20/20、`scanned_series_metadata` 3/3、`scanning_jobs` 81/81。默认并行测试先后两次让既有 `local_nfo_retry_excludes_non_retryable_conflict_item` 报 SQLx `RowNotFound`；该测试单独运行通过，另一次并行运行中 FIFO fixture 超过 60 秒。按顺序模式重跑三个目标全绿，归类为测试并行争用，不改变无关 NFO 生产逻辑。验证：`cargo build --locked` 通过（dev profile 41.87s；之后仅将测试权限字面值规范为 `0o000`，电影/剧集回归及全目标 Clippy均在该修改后重新通过）；`cargo fmt --all -- --check` 通过；`cargo clippy --locked --all-targets --all-features -- -D warnings` 通过（dev profile 22.34s）；`git diff --check` 通过。已有覆盖映射：慢 NFO=`manifest_scan_indexes_poster_while_local_nfo_is_blocked`；图片登记事务失败=`failed_local_poster_insert_does_not_mark_image_stage_complete`；扫描根不可用/恢复=`scan_job_marks_inaccessible_root_unavailable_and_recovers_after_restore`；Manifest 取消/恢复=`manifest_cancellation_preserves_committed_frontier_and_observations`、`cancelled_manifest_apply_resumes_pending_deltas_without_rediscovery`；全量/增量 CAS=`manifest_apply_does_not_overwrite_a_newer_incremental_filesystem_entry`、`manifest_add_conflicts_with_incremental_entry_claimed_after_diff`、`streamed_manifest_change_cas_does_not_overwrite_a_newer_incremental_entry`；增量图片=`incremental_movie_scan_indexes_local_images`、`incremental_sidecar_change_replaces_local_image`；扫描期间未访问目录新增 poster=`workflow3_running_discovery_indexes_poster_added_to_unvisited_directory`。本记录验证的是本机 SQLite 自动化 fixture，不外推 PostgreSQL/NAS/FNOS 性能。

补充进度（2026-10-09）：新增 `workflow3_running_discovery_indexes_poster_added_to_unvisited_directory`，验证同一 workflow 3 job 保持 `RUNNING`/`DISCOVERING` 时，本地 worker 能索引刚在已访问电影目录新增的 poster，且后续目录当时尚未访问。原测试按路径原始字符串查 poster 行导致误报；按目标媒体 source 的 `item_id` 查询并比较 poster 路径 canonical form 后，诊断版精确目标 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test scanned_metadata workflow3_running_discovery_indexes_poster_added_to_unvisited_directory -- --exact --test-threads=1` 通过（1/1）。随后将 SQL 查询收敛为只读取目标 item 的 POSTER 路径，canonical path 断言语义不变。最终整理版本的精确回归 1/1 通过；串行运行 `scanned_metadata` 21/21、`scanned_series_metadata` 3/3、`scanning_jobs` 81/81。没有修改生产代码。验证环境为本机 Mac ARM64、SQLite 临时 fixture；这不证明 PostgreSQL、NAS/FNOS 性能或部署运行行为。


- [ ] 1,000 与 10,000 项 fixture 证明首批已索引条目和本地海报在扫描结束前可查询/显示，且本地 worker 与后续索引并行。
- [ ] 人为阻塞首项图片、后段海报、慢 NFO、权限错误、不可用根、取消/重试、全量/增量竞态和扫描期间本地补图均有自动化覆盖。
- [ ] 缺失分类、自动补缺策略、队列去重、provider 无候选/失败冷却、执行前重新检查和禁止覆盖本地/锁定数据均有回归覆盖。
- [ ] SQLite 与 PostgreSQL 同 fixture A/B 分开报告索引完成耗时、首批可见、首张海报、local queue、在线 queue、前台 p95、事务/队列规模和内存；稳定索引或前台 p95 回退超过 5% 时先调度/并发并重测。
- [ ] 扫描索引耗时与本地/在线处理耗时分别呈现；不以任务仍有后台工作为由把索引时间混入扫描性能结论。
- [ ] 完成相关 Rust/Web 全量质量门、兼容性和性能记录、本机架构记录，并由项目所有者确认后结束阶段。

#### LUX-466：合并本地 NFO 页重复人物资产并跳过相同人物文件写入

范围：本地 NFO 页跨媒体条目重复引用同一人物时，使用该页共享的 deferred context 对人物资产请求做内容指纹去重；不同人物资产最多 4 路并发，单个人物失败不得阻断其他人物和 relation 保存。对人物 NFO、provider/canonical index 的写入仅在目标字节变化时原子替换。复用既有 person manifest checksum no-op、profile-image unchanged 写入和 person credits 页批处理；不把文件系统与数据库伪装为一个原子事务，不改变 identity、人物关系或 credits 数据模型。

验收：

- [x] 同一 page context 下重复引用的相同人物资产输入共用一个持久化结果；人物名、metadata 或 identity 输入变化时不错误复用旧结果。
- [x] 多人物处理保持每页最多 4 个不同人物资产并发；一人 provider index 写入失败时，该人物保留 pending 状态，其他人物资产与全部 relation 仍保存。
- [x] 相同人物 NFO、provider/canonical index 字节不替换文件；变化后的人物 metadata 仍写入新内容。
- [x] 定向人物资产、同页重复人物、scan-local NFO relation/credits 回归及 `cargo fmt --all -- --check`、`git diff --check` 通过。
- [ ] `cargo build --locked` 和全目标全 feature Clippy 待阶段 23 A/B 采样结束后运行，避免干扰同机测量。
- [x] 性能记录区分同页请求去重与 byte-identical write 的边界，不将调用上界推断为 FNOS/NAS CPU 或生产收益。

短计划与文件：先检查同页 actor relation/credits 生命周期和人物资产写入边界，再以 `src/application/people/relations.rs`、`src/application/people/service.rs` 补充有界去重与回归，最后更新本文件和 `docs/PERFORMANCE.md`。本任务不新增 storage schema 或依赖。

进度记录（2026-10-09）：同页共享缓存以人物 NFO/身份/metadata 写入输入的 SHA-256 标识一次资产结果，使用共享 semaphore 将跨 item 的不同人物资产任务限制为最多 4 路；person NFO 与 provider/canonical index 在目标字节未变时跳过临时文件、fsync 和 rename。person manifest 继续用其内容 checksum 避免无变化写入，profile image 沿用 LUX-448 的 unchanged-write，relation credits 沿用最多 16 item 的批量提交。覆盖的文件类型包括 `person.nfo`、人物 manifest、profile image、provider identity index 与 canonical person index；`people.json` relation 仍逐 item 持久化，credits 仍按现有 chunk 事务提交。测试专用闸门令 8 个不同人物请求竞争同一页 semaphore，确认 4 个 permit 全部占用且最大活动数为 4；symlink provider-index 故障 fixture 通过另一 provider 的可读 profile index 到达目标写回，确认只有失败人物标记 `personIndex` pending、其他人物 NFO 和全部 relation 仍保存。`cargo test --locked --lib local_nfo_person_assets_keep_page_concurrency_at_four`（1/1）、`local_nfo_person_asset_batch_isolates_failures_between_people`（1/1）、`local_nfo_person_`（3/3）、同页重复人物资产（1/1）、scan-local relation/credits（1/1）和 `cargo fmt --all -- --check` 通过，`git diff --check` 通过。`cargo build --locked` 和全目标全 feature Clippy 暂缓至阶段 23 A/B 采样结束。本机 `arm64`；测试没有测量系统调用墙钟、PostgreSQL/NAS 延迟、FNOS CPU 或生产收益，也不声称跨文件/数据库原子性。

补充回归（2026-10-09）：`local_nfo_lux_person_canonical_index_skips_unchanged_bytes`（1/1）直接覆盖 `lux-*` canonical index 写入路径；相同 index 字节保留 inode，metadata 改变仍更新 person NFO，且不替换未变化的 canonical index。`local_nfo_page_name_and_identity_changes_do_not_reuse_cached_person_assets`（1/1）覆盖同一 canonical person 的改名目录和新增 identity；分别临时移除 name、identity 指纹字段时测试均按预期失败，恢复实现后通过。上述项目只运行定向单测，build 与全目标 Clippy 仍待阶段门统一执行。

### 资源详情入库时间

#### LUX-307：Lux 媒体条目响应公开入库时间

范围：在 Lux API 的媒体条目 JSON 响应中暴露现有 `media_items.added_at`，字段名为 `addedAt`，值为 Unix epoch 秒。使用现有 `CatalogItem.added_at`，不新增或修改数据库字段，不改变 Emby DTO。

验收：

- [x] `GET /api/v1/items/{itemId}` 返回的 `addedAt` 与该条目的存储值一致。
- [x] `docs/API.md` 说明 `addedAt` 的含义、单位和来源；Emby 响应合同不变。
- [x] 无数据库 migration，Lux API 既有 ACL 和字段响应保持不变。

验证：`cargo test --locked --test catalog`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`。

文件：`src/api/media.rs`、`tests/catalog.rs`、`docs/API.md`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-28）：详情 API 返回数据库中的 `added_at` Unix 秒值，集成测试验证 JSON 与存储值一致。回归断言先因响应为 `null` 而失败，修复后 `cargo test --locked --test catalog` 的 3 项通过；`cargo fmt --all -- --check`、全目标 Clippy 和 `git diff --check` 通过。

#### LUX-308：资源详情显示添加时间

范围：Lux Web 具体资源详情的元信息行显示“添加于”及资源加入 Lux 媒体库的本地日期和时间，数据来自 LUX-307 的 `addedAt`。时间按浏览器本地时区显示到分钟；字段缺失或无效时隐藏该标签。详情页音频和字幕轨选择器在可用宽度足以容纳两个各 280px 的控件时横向排列，空间不足时上下排列；控件紧凑显示，长轨道名限制在自己的控件内并以省略号截断。本任务不改变列表排序和 Emby 客户端行为。

验收：

- [x] 有效 `addedAt` 在电影、剧集、季度、单集和 VIDEO 详情元信息行显示“添加于”及对应本地时间。
- [x] 页面以语义化 `<time>` 暴露 ISO 8601 `dateTime`；缺失或无效值不会显示 `Invalid Date` 或占位标签。
- [x] `MediaDetailPage` 自动化测试覆盖有效和缺失时间；现有详情内容及响应式元信息样式保持可用。
- [x] 音频和字幕选择器在可用宽度至少 572px 时各保留 280px 并横向排列；更窄时上下排列，不发生重叠。
- [x] 选择器标题、轨道数提示、已选轨道文字和下拉选项使用紧凑字号及控件高度；过长的已选轨道文字在选择框内省略。
- [x] 版本选择器与音频、字幕选择器使用相同的 280px 最大宽度、38px 控件高度、标题字号和间距；已选版本单行省略，展开选项保留容器或版本说明。

依赖：LUX-307。验证：`pnpm --dir web test -- media-detail`、`pnpm --dir web build`。

文件：`web/src/lib/api/types.ts`、`web/src/features/detail/MediaDetailPage.tsx`、`web/src/components/LuxSelect.tsx`、`web/src/react.css`、`web/tests/media-detail.test.tsx`、`web/tests/detail-layout.test.mjs`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-28）：有效时间在详情元信息行显示“添加于”及按浏览器本地时区格式化到分钟的日期，并提供 ISO 8601 `dateTime`；缺失和无效时间不显示标签。`pnpm --dir web install --frozen-lockfile` 通过，详情页测试 26 项通过，完整 Web 测试 75 个文件/526 项通过，`pnpm --dir web build` 通过。输出仍有既存 jsdom 媒体元素 `load/pause` 告警和 Vite 大 chunk 提示。

追加（2026-09-29）：轨道选择器按可用宽度自动换列，两个控件能各保留 320px 时并排，窄于 657px 时堆叠。`node --test web/tests/detail-layout.test.mjs` 27 项通过，完整 Web 测试 75 个文件/529 项通过，`pnpm --dir web install --frozen-lockfile` 与 `pnpm --dir web build` 通过。测试仍输出既存 jsdom `load/pause` 告警；构建保留既存大 chunk 提示。

追加（2026-09-30）：轨道选择器容器收窄为 572px，两列各 280px；每项明确限制宽度，长文本在框内省略，标签、已选文本和选项字号缩小，控件高 38px。Playwright 以 1400px、600px、320px 视口验证并排、堆叠和无页面横向溢出；`node --test web/tests/detail-layout.test.mjs` 27 项、`vitest run tests/media-detail.test.tsx` 26 项及完整 Web 测试 75 个文件/529 项通过，冻结锁文件安装和 Web 构建通过。测试仍输出 jsdom 媒体 `load/pause` 告警；构建有 Vite 大 chunk 提示。

追加（2026-10-10）：版本选择器复用轨道选择器的标题、宽度、间距、控件和菜单样式；折叠时只显示单行版本标签，容器或版本说明保留在展开选项中。Playwright 本地模拟 API 的尺寸断言在旧实现下失败、修复后通过，320px、390px、768px、1024px 和 1440px 视口下版本与音频控件尺寸和字号一致且无页面横向溢出；浅色、深色和长版本标签检查通过，键盘切换版本同步更新播放链接及对应轨道并恢复焦点。详情布局 Node 29/29、详情 Vitest 26/26、完整 Node 134/134、Vitest 569/569、冻结锁文件安装、生产构建、Cargo fmt、全目标全 feature Clippy 和 `git diff --check` 通过；Web 测试仍有既存 jsdom 媒体 `load/pause` 告警。本机 `arm64`，Cargo 使用 Toshiba 共享 target；仅修改 Web 样式及结构，未运行 Rust 测试，验证不代表部署状态。

#### LUX-309：配置目录日志分段归档与有界保留

统一程序结构化日志、任务事件和管理员审计事件的文件归档策略。日志主文件写入 `/config/logs/`，按 UTC 日期命名；单个未压缩 JSONL 段达到 50 MiB 时封存为 ZIP，日期切换时也封存当前段。归档包放在 `/config/logs/archive/`，全局最多保留 20 个。第 21 个归档只有在 ZIP 写入并校验成功后才删除最旧包；压缩失败时保留原始日志段。stdout JSON 日志继续保留。

验收：

- [x] 程序结构化日志通过独立 writer 写入配置目录 JSONL；写文件和压缩不阻塞 Tokio 核心 worker。
- [x] 单段在 50 MiB 边界和 UTC 日期切换时安全封存；重启后继续写入正确的当前日期文件。
- [x] 每个压缩包包含原始 JSONL 段；压缩包损坏或写入失败不会先删除原日志。
- [x] 归档目录最多 20 个包；成功完成第 21 个包后按创建顺序删最旧包。
- [x] 管理员按日期导出可读取当前文件和归档成员；单日仍返回 JSONL，多日仍返回 ZIP，日期权限和范围合同不变。
- [x] 自动化测试覆盖容量轮转、日期轮转、ZIP 内容、20/21 个归档边界、失败保留和导出读取。

结果（2026-09-29）：JSONL writer 在非阻塞日志线程写入配置目录，按 50 MiB/UTC 日切安全打包，验证归档后执行 20 包 FIFO 清理；日期导出从归档段和活动文件拼回原始 JSONL。`cargo test --locked --lib observability::logs::tests` 6 项、`cargo test --locked --test observability --test log_export` 4 项及 `cargo fmt --all -- --check` 通过。

验证：`cargo test --locked --test observability --test log_export`、`cargo fmt --all -- --check`。

预计文件：`src/observability/mod.rs`、`src/observability/logs.rs`、`tests/observability.rs`、`tests/log_export.rs`、`docs/LUX-DEVELOPMENT.md`。

依赖：LUX-156。明确不做：不改任务状态/恢复数据的数据库边界；不引入新的核心依赖。

#### LUX-310：扫描任务事件文件化

将扫描任务的生命周期事件（INFO、WARN、ERROR）及结构化详情写入 LUX-309 的 JSONL 文件层，并从文件归档读取管理员任务事件 API。`scan_jobs` 中用于队列、取消、重试、进度和恢复的状态、游标仍保留在数据库；`scan_job_events` 不再接收新日志。

验收：

- [x] 每个任务事件在文件中包含稳定事件 ID、UTC 时间、jobId、level、eventCode、message 和脱敏 details；INFO 过程事件也可查询。
- [x] `GET /api/v1/admin/jobs/{jobId}/events` 保持管理员权限、级别/事件码筛选、分页和 JSON DTO 合同，结果按时间倒序，并查询活动文件及压缩归档。
- [x] 新产生的任务事件不会写入 `scan_job_events`；任务运行、取消、重试和恢复仍由数据库状态驱动。旧事件由 LUX-315 启动迁出，LUX-316 完成后 API 不再回退查询该表。
- [x] 自动化测试覆盖文件事件、详情筛选、分页、不同级别、归档读取、敏感内容脱敏和数据库无新事件。

验证：`cargo test --locked --test job_events_api --test scanning_jobs`、`cargo fmt --all -- --check`。

文件：`src/observability/logs.rs`、`src/application/scanner.rs`、`src/api/legacy.rs`、`src/api/admin_handlers.rs`、`tests/job_events_api.rs`、`docs/API.md`。

结果（2026-09-29）：所有新扫描任务生命周期事件（含 INFO）写入共享 JSONL LogStore；管理员任务事件 API 从活动文件与归档读取，并保留旧数据库记录回退。`cargo test --locked --test job_events_api --test scanning_jobs` 的 API 测试 1 项和扫描任务测试 81 项通过；LogStore 归档事件单测 1 项通过；`cargo fmt --all -- --check` 通过。Rust 编译仍报告既有未使用函数 `prepare_manifest_filename` 警告。

依赖：LUX-309、LUX-232。明确不做：不迁出 `scan_jobs` 的执行状态、进度或恢复游标。

#### LUX-311：管理员操作审计文件化

管理员操作审计事件写入 LUX-309 的 JSONL 文件层；`/api/v1/admin/audit` 与兼容的 `/api/v1/admin/logs` 从活动日志及压缩归档读取。历史数据库事件由 LUX-316 迁出，之后两个端点只读文件。

验收：

- [x] 管理审计 JSONL 包含稳定事件 ID、时间、actor、eventType、target 和脱敏 metadata；共享 API Key 元数据继续脱敏。
- [x] 两个管理员审计读取端点保持现有分页、权限和响应字段，按时间与事件 ID 倒序读取文件记录；旧数据库历史由 LUX-316 迁出。
- [x] 新管理员操作审计不会写入 `audit_events`；登录/播放近期活动仍由 LUX-312 处理。
- [x] 自动化测试覆盖管理员操作记录、两个 API 路由、共享 API Key 脱敏和数据库中没有新管理员操作事件。

验证：`cargo test --locked --test users --test admin_api_key`、`cargo fmt --all -- --check`。

文件：`src/observability/logs.rs`、`src/api/admin_handlers.rs`、`tests/users.rs`、`docs/API.md`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-29）：管理员操作审计新写入 JSONL，`/admin/audit` 与兼容 `/admin/logs` 合并读取文件事件和数据库历史，按时间及 ID 排序；共享 API Key 只记录认证类型标记，不泄露 Key。`cargo test --locked --test users --test admin_api_key` 5 项及审计落盘/脱敏和归档单测通过；`cargo fmt --all -- --check` 与 `cargo clippy --locked --all-targets --all-features -- -D warnings` 通过。

依赖：LUX-309。明确不做：不改变登录/播放活动、播放进度和业务播放历史。

#### LUX-312：登录/播放活动文件化

登录及播放近期活动沿用 `audit_events` 的现有应用调用点，底层持久化改为 LUX-309 JSONL LogStore；仪表盘读取路径由 LUX-313 修改。用户播放进度、播放会话和业务播放历史继续留在业务表。

验收：

- [x] `AUTH_LOGIN`、`PLAYBACK_STARTED`、`PLAYBACK_PAUSED`、`PLAYBACK_STOPPED` 活动写入配置目录日志，不再新增到 `audit_events`。
- [x] 活动记录保存事件 ID、actor、eventType、target、脱敏 metadata 和时间；可从 LogStore 查询。
- [x] 用户播放进度、播放会话、业务播放事件和 Webhook 投递状态保持原存储合同。
- [x] 测试覆盖登录/播放活动文件记录、数据库无新活动审计行及 metadata 脱敏。

验证：`cargo test --locked --test web_playback`、`cargo fmt --all -- --check`。

预计文件：`src/storage/repository.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`、`tests/web_playback.rs`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-29）：登录和播放状态变化仍通过原调用点产生审计活动，但 `Database.insert_audit_event` 改为写共享 JSONL LogStore；播放会话及进度表不变。`cargo test --locked --test web_playback --test admin_health --test users` 3 项通过，播放回归确认 `audit_events` 没有新活动行且文件含登录/播放事件。

依赖：LUX-309、LUX-311。明确不做：不改变仪表盘读取路径；不迁移旧 `audit_events` 历史。

#### LUX-313：仪表盘近期活动文件读取

管理仪表盘近期登录与播放活动从文件审计记录生成，再关联数据库中仍存在的用户/媒体展示信息。LUX-316 迁出旧审计活动后，仪表盘不再查询 `audit_events`。

验收：

- [x] 仪表盘近期活动从文件读取，保持现有事件类型、最多 24 条、登录/播放分类配额和 DTO 字段；升级前历史由 LUX-316 迁入文件。
- [x] 活动按时间及 ID 倒序；用户或媒体已删除时 API 仍成功并安全返回可空名称/标题。
- [x] 测试覆盖登录与播放活动、分类配额、缺失用户/媒体、权限和响应合同。

验证：`cargo test --locked --test admin_dashboard --test web_playback`、`cargo fmt --all -- --check`。

文件：`src/observability/logs.rs`、`src/storage/users.rs`、`tests/admin_dashboard.rs`、`docs/API.md`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-29）：仪表盘将文件近期活动与历史数据库活动合并排序、去重后按登录/播放各 12 条限额与 24 条总限额返回；用当前用户/媒体信息填充名称，缺失关联返回空。`cargo test --locked --test admin_dashboard --test web_playback` 2 项、`cargo fmt --all -- --check` 和全目标 Clippy 通过；仪表盘测试覆盖文件与遗留数据库活动合并、分类配额、倒序与缺失用户/媒体。

依赖：LUX-312。明确不做：不迁移旧数据库审计历史，不改变业务播放状态。

#### LUX-314：日志迁移的批量持久写入

为历史任务/审计日志升级迁移提供有界批量 JSONL 写入。批次写入通过与程序日志相同的 LogStore 锁串行化；活动文件写完执行 `sync_all`，达到分段上限的归档仍须完整写入、校验并同步后才移除原始段。

验收：

- [x] 批量记录逐条保持合法 JSONL，不拆分记录；传入的稳定 ID、事件时间和脱敏字段原样保留。
- [x] 批次全部落盘并同步成功后才向调用方返回成功；任何写入/封存失败返回错误，调用方可以保留数据库源行并重试。
- [x] 同配置目录多个 LogStore 句柄共用同一 writer 锁；日志轮转、应用事件写入、导出和读取不会交错记录或读到一半归档。
- [x] 自动化测试覆盖多记录批写、失败后重试及记录 JSONL 完整。

验证：`cargo test --locked --lib observability::logs::tests`、`cargo fmt --all -- --check`。

文件：`src/observability/logs.rs`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-29）：新增迁移用批量 JSONL 持久写入，写入阶段通过共享 writer 锁串行化，批次完成后同步活动段；跨过分段门槛时归档会先校验并同步。故障注入后重试回归通过，确认失败会返回错误且后续批次完整落盘；`cargo fmt --all -- --check` 通过。

依赖：LUX-309。明确不做：不改变日志保留数、任务状态或数据库迁移顺序。

#### LUX-315：历史任务事件迁出数据库

升级时将既有 `scan_job_events` 安全导出到 `/config/logs/`，文件写入并确认可读后才删除对应数据库行。清理可重试且不触及 `scan_jobs` 状态、进度或恢复游标。

验收：

- [x] SQLite 与 PostgreSQL 历史任务事件保留原 ID、jobId、级别、事件代码、消息、详情和时间写入 JSONL。
- [x] 文件完整持久化并验证后才删除对应数据库行；中断重启可继续，重复执行不产生可见重复事件。
- [x] 迁移后任务事件 API 可读历史文件记录；不更改任务状态、进度、取消、重试或恢复语义。
- [x] 测试覆盖无历史、SQLite 历史、失败重试、幂等和任务状态不变；PostgreSQL 在可用集成环境验证。

验证：`cargo test --locked --test log_migration --test job_events_api`、`cargo fmt --all -- --check`。

预计文件：`src/main.rs`、`src/storage/database_cleanup.rs`、`src/observability/logs.rs`、`tests/log_migration.rs`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-29）：服务启动时按 UTC 日期和不超过单段 50 MiB 的字节量分批迁出 `scan_job_events`，每批 JSONL `sync_all` 后从活动文件和保留归档回读本批 ID，确认齐全才删除数据库行。重试只索引当前批次 ID，已落盘记录不会重复追加；失败批次可重试。日期/字节分批避免保留策略删除尚未校验的批内事件。SQLite 覆盖空历史、字段/扫描状态保持、失败重试、幂等及 22 日 FIFO 保留；PostgreSQL 隔离数据库合同测试通过。`cargo test --locked --test log_migration --test job_events_api` 4 项迁移测试和 1 项 API 测试通过；PostgreSQL 忽略测试单独启用后通过；LogStore 单测 12 项、格式检查通过。

依赖：LUX-310、LUX-313、LUX-314。明确不做：不清理旧管理员审计事件，不迁出扫描控制状态。

#### LUX-316：历史管理员审计迁出与数据库日志停写

升级时将既有 `audit_events` 导出至日志目录，确认文件持久且可读后才清理数据库记录。成功标记写入 `lux_meta`；后续启动不再查询日志表。任务事件与审计 API、近期活动读取只访问文件，日志表 schema 保留但不再有运行时读写或数据库保留清理。历史超过 20 个归档包容量的记录按 FIFO 过期；迁移必须先验证并删除当前批次数据库行，再应用归档淘汰。

验收：

- [x] SQLite 与 PostgreSQL 既有审计记录保留 ID、actor、目标、脱敏 metadata 和时间迁入 JSONL。
- [x] 写入校验后才删除对应历史行；迁移失败重试不丢记录、不重复展示；完成标记后后续启动不查日志表。
- [x] 两个日志表没有运行时写入/查询；任务控制状态、播放状态和业务关系保持原样。
- [x] 文件日志 API 与仪表盘能读迁移后的历史记录；归档继续遵守 20 包 FIFO 保留。
- [x] 单条或单批跨越轮转边界时，当前批次在读回验证并删除源行前不会被 FIFO 淘汰；超出保留容量的已提交旧包仍按 FIFO 过期。
- [x] 测试覆盖旧审计历史迁移、失败重试、幂等、日志表不可用时的文件读取和清理隔离；最终执行 build、all-targets、fmt、Clippy 与 `uname -m` 完成门。

验证：`cargo test --locked --test log_migration --test job_events_api --test admin_dashboard --test users --test web_playback`、`cargo test --locked --lib observability::logs::tests`、单独启用的 PostgreSQL 迁移合同测试、`cargo build --locked`、串行 `cargo test --locked --all-targets -- --test-threads=1`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`、`uname -m`。

预计文件：`src/main.rs`、`src/api/admin_handlers.rs`、`src/application/scanner.rs`、`src/observability/logs.rs`、`src/storage/database_cleanup.rs`、`src/storage/jobs.rs`、`src/storage/mod.rs`、`src/storage/repository.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`、`tests/admin_dashboard.rs`、`tests/job_events_api.rs`、`tests/log_migration.rs`、`tests/postgres_database.rs`、`tests/scanning_jobs.rs`、`tests/shutdown_jobs.rs`、`tests/thumbnails.rs`、`tests/users.rs`、`tests/web_playback.rs`、`docs/API.md`、`docs/LUX-DEVELOPMENT.md`。

结果（2026-09-30）：SQLite 与 PostgreSQL 的迁移均保留旧审计事件标识、操作者/目标、脱敏详情和时间；批次落盘、同步并回读验证后才删除源行，失败重试幂等。完成标记让后续启动在日志表不可用时仍跳过这些表；运行时任务事件、审计和近期活动均从 `/config/logs/` JSONL/ZIP 读取，任务状态与播放状态仍由业务存储负责。迁移/FIFO 回归覆盖 22 个日期批次，保留最多 20 个 ZIP，活动段仍可读，已验证的旧包按 FIFO 过期。JSONL 追加前会截去未完成尾行，避免进程内追加造成坏记录。迁移、任务事件 API、dashboard、用户与播放定向测试，14 项日志单测及 PostgreSQL 迁移合同测试通过。`cargo build --locked`、`cargo test --locked --all-targets -- --test-threads=1`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`、`git diff --check` 均通过；项目中已标记的专项测试仍保持 ignored，PostgreSQL 迁移专项已单独执行通过。扫描器测试 17 项、扫描任务 81 项通过；本机架构为 `arm64`。

依赖：LUX-311、LUX-312、LUX-313、LUX-315。明确不做：不删除 migration 历史，不改变任何业务事件和任务控制状态。

#### LUX-316：临时数据库体检报告与导出

范围：Lux 启动并开始监听 5 分钟后在后台生成一次只读数据库体检快照，目标在启动后 10 分钟内可导出，单次采集最多运行 15 分钟后超时。管理员也可通过手动接口立即发起首次采集或重新采集。报告覆盖 PostgreSQL 数据库总大小、用户表 heap/index/TOAST 大小、索引大小、估算行数、live/dead tuple、最近 analyze/autovacuum 时间和按 schema 汇总；SQLite 报告数据库文件/WAL/page/freelist 信息，并在 dbstat 可用时给出逐表/索引大小。无法读取或只能估算的字段必须明确标记，不执行全表 COUNT、ANALYZE、VACUUM 或任何写入。

报告包含后端与数据库版本、schema 版本、生成时间、大小排名和采样限制；不得包含媒体记录、媒体路径、用户内容、连接串、密码或 API Key。报告只在进程内存中保留，不新增诊断结果表或 migration。统计查询需设置有界超时和关系条目上限，不得阻塞 HTTP 启动或扫描任务；数据库未完成选择/配置时不启动体检。

提供管理员状态接口、手动启动/重新采集接口和 JSON 导出接口，均要求共享管理员 API Key 或管理员 session；API Key 通过请求头传递。采集中禁止并发重复启动；手动启动若先于启动延迟任务，则延迟任务跳过。重新采集期间及失败后保留上一份可用报告，直到新报告成功替换或 Lux 重启。此功能只从终端通过 HTTP API 调用，不在 Lux Web 设置页或其他页面展示。此功能是临时诊断能力，专用于收集下一版本用户数据库数据；在后续指定版本建立独立移除任务，届时一并删除启动调度和接口。本任务不实施数据库瘦身、历史记录清理或物理空间回收。

验收：

- [x] PostgreSQL 报告数据库总大小、每个用户关系的 heap/index/TOAST/总大小和每个索引大小；行数及 dead tuple 明确标为统计估算。
- [x] SQLite 报告文件、WAL、page size、page count、freelist；dbstat 不可用时优雅降级并说明逐对象大小不可用。
- [x] 启动延迟约 5 分钟后只读生成一次，单次统计最多运行 15 分钟；报告目标在启动 10 分钟内就绪，超时/失败提供安全状态。
- [x] 管理员可手动开始或重新采集；运行中拒绝重复启动，手动先启动时自动延迟任务不重复执行；重新采集期间上一份报告仍可导出。
- [x] 状态、启动和下载接口拒绝未授权请求，接受共享 Admin API Key 请求头；服务器设置页不展示体检状态或操作，管理页回归测试确保无体检入口。
- [x] 报告不暴露数据库路径、连接配置或任何行数据；生成过程中不修改任何数据库内容。
- [x] SQLite 与 PostgreSQL 覆盖存储查询；API 覆盖鉴权/未就绪/下载；Web 回归测试确认服务器设置页不展示体检入口。
- [x] 该临时功能无数据库 schema 变更，不引入新依赖。

验证：`cargo test --locked --lib database_diagnostics`、`cargo test --locked --lib postgres_database_diagnostics -- --ignored --nocapture`、`cargo test --locked --test admin_database_diagnostics`、`cargo build --locked`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`、`pnpm --dir web exec vitest run tests/admin-settings.test.tsx -t "keeps the temporary database diagnostics out of server settings"`、`pnpm --dir web build`。

预计文件：`src/storage/database_diagnostics.rs`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/repository_tests.rs`、`src/application/database_diagnostics.rs`、`src/application/mod.rs`、`src/api/legacy.rs`、`src/api/admin.rs`、`src/api/admin_handlers.rs`、`src/main.rs`、`tests/admin_database_diagnostics.rs`、`web/src/features/admin/AdminSettingsPage.tsx`、`web/tests/admin-settings.test.tsx`、`docs/API.md`、`docs/LUX-DEVELOPMENT.md`。

依赖：无。明确不做：数据库瘦身、定时清理、删除现存数据、VACUUM/REINDEX、修改持久化任务模型。未来移除版本待项目所有者指定。

结果（2026-09-29）：新增的进程内报告在 HTTP listener 就绪后等待 5 分钟运行；整体采集超时 15 分钟，SQLite `dbstat` 子查询超时 90 秒并可降级。管理员可通过 `POST /api/v1/admin/database-diagnostics/run` 立即开始/重新采集；重复运行时返回 409，重新采集期间保留上一份报告，直到新结果替换。SQLite 文件/WAL/页统计、PostgreSQL 16.15 数据库/关系/索引统计、API Key 鉴权和下载响应均已实现。报告通过 `GET /api/v1/admin/database-diagnostics/export` 下载，状态轮询不传输完整报告；没有新增 migration 或持久化报告数据。Web 设置面板曾短暂实现，后按下方 2026-09-30 更新移除，最终只保留终端/API 调用。

更新（2026-09-30）：将采集最长时间调整为 15 分钟，并新增手动开始/重新采集接口。重复触发在服务层原子拒绝；手动任务已开始后，5 分钟自动任务会跳过；重新采集及其失败期间继续保留上一份报告。依据产品要求，体检功能仅保留终端 HTTP API，已移除设置页面板、Web API client 方法和 Web 导出入口；设置页回归测试确保不显示该功能。截图显示 Web 页面调用的方法在当时载入的 API client 对象上不存在；具体是旧资源缓存还是前后端资源版本混用，需在用户部署环境才能确认。移除 Web 入口后，体检只通过终端请求接口。

复验（2026-09-30）：终端 API 和服务重采集定向 Rust 测试通过，真实 PostgreSQL 体检测试先前通过；Web 全套测试 76 个文件/535 项通过，设置页隐藏体检功能的回归测试、Web build、Rust build、fmt 和全目标 Clippy 通过。全目标 Rust 测试仍受无关 `tests/log_export.rs::admin_can_export_selected_daily_logs_but_viewer_cannot` 的 `FileNotFound` 阻断。
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
| 浏览器编码支持不足 | 高 | 先用 LUX-184 记录真实能力；4K 目标优先依赖原生/硬件 WebCodecs，不把 WASM 探测结果当作实时保证 |
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

#### LUX-317：统一登录背景插件与自定义上传图片（初始范围与验收）

本节保留 LUX-317 于 2026-09-30 确认实施时的原始产品范围与验收边界，不改写 LUX-259 至 LUX-263 的历史验收记录。当前阶段进展见后文实施记录；接口和阶段门详见 `docs/LUX-317-PLAN.md`。

范围：新建 `org.lux.login-background` 登录背景插件，以 `source` 配置选择 `BING_DAILY`、`TMDB_TRENDING` 或 `CUSTOM_IMAGE`。Bing 个人用途确认与 TMDb 非商业许可确认保留为两个互相独立的显式配置门槛，只在选择对应来源时生效；自定义上传另需确认管理员有权在未登录页面公开展示该图片。Bing 与 TMDb 仍按 LUX-262/263 已记录的请求、图片 URL、版权提示、缓存和静态回退规则执行。

自定义图片先只支持单张：管理员在该插件的配置界面上传 JPEG、PNG 或 WebP，最多 5 MiB；服务端校验真实格式与有界像素数，原样存储，不为媒体库图片生成衍生文件，也不压缩或重编码。上传新图以原子替换方式覆盖唯一现存的自定义背景文件。文件存放在 Lux 管理的数据目录，不交给插件进程读取，不接受客户端路径或任意图片 URL。管理员上传端点要求现有管理员鉴权与 CSRF；登录页使用固定同源资源路由读取该单张文件，该路由仅在统一插件已选为登录背景且其模式为 `CUSTOM_IMAGE` 时公开，提供受校验 MIME、`nosniff` 和可重新验证的缓存头。背景 URL 验证只允许该统一插件返回规定的固定资源路径，其他插件继续只能返回 manifest 主机 allowlist 内的 HTTPS URL；不增加通用 URL 代理。

验收：

- [ ] 正式目录只有一个统一登录背景插件 ID `org.lux.login-background`；它仍为独立的 `login_background` 包，不与 `org.lux.tmdb` 合并。
- [ ] 插件配置能选择 `BING_DAILY`、`TMDB_TRENDING`、`CUSTOM_IMAGE`；仅执行选中的来源。Bing 与 TMDb 的独立许可确认默认均为未确认，缺少相应确认时不请求对应上游；自定义图片须单独确认公开展示权。
- [ ] Bing 与 TMDb 行为保持原合同：Bing 仅请求既定每日大图接口；TMDb 仅取日榜混合电影/剧集中的首个有效 `backdrop_path`，不回退海报；二者继续输出 `HERO_IMAGE` 并由 Lux 左侧大图样式呈现。
- [ ] `image` 配置字段仅允许登录背景插件声明，UI 只提供单张上传/替换；服务器强制 5 MiB 上限、JPEG/PNG/WebP allowlist、文件签名与有界尺寸检查，拒绝伪造 MIME、SVG、动画 GIF、畸形或超大尺寸内容；替换保持原字节并原子提交，不留下历史上传文件。
- [ ] 上传只能由管理员经受 CSRF 保护的配置 API 完成；自定义图仅存于 Lux 配置数据目录，不写入媒体库、不通过插件文件权限暴露；插件 RPC 和配置响应均不返回服务端路径。
- [ ] 自定义 RPC 结果只能引用固定同源登录背景资源路由；Lux 仅在已选择统一插件且模式为自定义图片时公开资源。响应使用真实检测出的 `Content-Type`、`X-Content-Type-Options: nosniff` 与可重新验证缓存；未知路径、缺图、模式切换和读取失败安全回退到静态海报墙。
- [ ] 登录页 API 不把本地绝对路径、上传文件名或图片字节放入 JSON；不增加通用远程 URL 代理，不因公开图片 GET 触发插件、TMDb 或 Bing 请求。
- [ ] 正式目录自动发布新统一插件、双架构包及哈希后，从活动目录移除 `org.lux.bing-daily-background` 和 `org.lux.tmdb-trending-background`；随后按项目所有者授权删除二者现有 GitHub Release 及 release tag（Bing 0.1.0、TMDb 0.1.0/0.1.1）。不改写仓库 Git 历史，也不远程卸载任何现有 Lux 服务器中的插件文件。
- [ ] 发布说明给出手动迁移步骤：安装统一插件、选择旧来源对应的模式、重新确认对应许可、切换服务器背景来源，再由管理员自行卸载旧插件。不得把旧许可确认自动复制为新插件的确认。
- [ ] 覆盖恶意/超限上传、原子替换、未授权/CSRF、关闭/未选中时的公开资源访问、条件缓存、Bing/TMDb 来源选择与许可门槛、RPC URL allowlist、空图与故障回退；插件 mock 测试不访问真实上游。

验证：

- Lux：新增图片配置/RPC 合同和资源服务定向测试；`cargo fmt --all -- --check`、相关 `cargo test --locked --test ...`、`cargo clippy --locked --all-targets --all-features -- -D warnings`。
- Web：配置页/上传 API/LoginPage 定向 Vitest 与 `pnpm --dir web build`；Playwright 检查管理员上传、三种模式切换、资源替换/缓存、窄屏及未授权状态。
- Lux-plugins：统一插件单测、mock HTTP、插件目录与包合同测试，`cargo fmt --all -- --check`、Clippy；正式 workflow 必须同时通过 x86_64 与 aarch64 构建和 ZIP/manifest/hash 校验。
- 外部 cutover 后读取 `main/index.json` 确认只有新 ID，不再存在两个旧 ID；检查旧 release/tag 删除完成及统一插件双架构 Release 可下载。部署到既有 Lux 实例后的 UI/真实图片请求另行记录，不以 CI 代替部署验证。

建议增量与阶段门见 `docs/LUX-317-PLAN.md`。每个实现增量保持独立、先写失败测试再实现并原子提交；先完成宿主合同与本地图片托管，再接入 UI 和统一插件，最后发布切换并删除旧发布包。进入下一阶段前按项目阶段门运行检查并停下供项目所有者确认。

依赖：LUX-259 至 LUX-263、LUX-260、LUX-261、LUX-110。正式目录切换、旧 Release/tag 清理和手动迁移说明仍待阶段 C。

明确不做：自动卸载远端服务器已安装的旧插件；自动继承旧插件的许可确认；上传多图/轮播；读取媒体库路径；插件进程直接访问图片文件；图片 CDN/代理/转码服务；改动 TMDb 元数据插件或其配置。

#### LUX-318：TMDb 电影完整详情候选与 NFO 写回

范围：确保用户/任务下一次真正执行电影元数据刮削时，会请求完整电影详情并把 provider 返回的丰富字段写进 NFO。不得仅因已有 NFO 缺少标语、官网、认证、国家、类型或制片公司而触发 `FILL_MISSING`，也不为既有媒体增加专门回填任务。完整刮削、手动候选查询或其他本来需要详情的刮削继续遵守本地字段优先级与字段锁定。IMDb ID 可用时同时写入通用 `<id>`；官网可用时同时写入 `uniqueid type="official website"`。电影 NFO 的 `sorttitle` 和 `dateadded` 读取 Lux 数据库已有值，不由 TMDb 生成。

搜索摘要不得标记为完整详情。需要完整详情的刮削请求必须获取 TMDb 详情及本次计划要求的 credits、外部 ID 和预告片；若详情请求失败，候选搜索应失败并允许重试，不能把搜索摘要作为完整结果写入 NFO。TMDb 插件已经返回的字段继续由宿主统一合并与写回；不增加逐演员外部请求、数据库 schema 或公共 API。

验收：

- [ ] 电影条目的基础字段齐全但 rich NFO 字段缺失时，单纯 `FILL_MISSING` 计划不创建刮削请求；完整刮削和手动刮削仍请求电影详情。
- [ ] 完整刮削产生的每个可选电影候选都包含详情；旧搜索摘要/旧版本候选不能冒充完整详情。
- [ ] TMDb 有返回值的 rating、上映日期、MPAA、国家、类型、制片公司、合集、标语、官网、外部 ID、导演/编剧、演员和预告片能通过正常候选选择写入 NFO。
- [ ] IMDb ID 存在时写入 `<id>`，官网存在时写入相应 official-website uniqueid；值与原始 provider 值一致。
- [ ] 已有/锁定 NFO 字段不被覆盖；详情失败不能静默确认或写入搜索摘要候选，后续任务可重试。
- [ ] 回归覆盖“核心字段已完整、NFO rich 字段为空”时，单纯 `FILL_MISSING` 不补抓 metadata；完整刮削仍请求详情。

明确不做：以 TMDb 伪造 `dateadded`、`fileinfo`、`streamdetails`、Douban ID 或非 TMDb 人物身份；这些字段仅由实际本地来源提供。

验证：`cargo test --locked --test metadata_selection`、`cargo fmt --all -- --check`；任务完成时运行全 Rust 门禁。

依赖：LUX-168、LUX-195、LUX-196、LUX-300 至 LUX-303。

#### LUX-319：电影 NFO 演员上限扩展到 100

范围：将 TMDb 电影候选、Lux NFO 解析/写回和条目详情 API 的演员上限统一提高到 100，保留 provider 返回顺序、角色和排序。只写 provider 或已有关系明确提供的人物 ID；缺少 IMDb、TVDb 或 Douban ID 时不猜测，不为每个演员额外发起网络请求。

验收：

- [x] 100 位以内的演员按 TMDb 顺序完整进入 movie NFO，name、role、type、order 和可用 ID 保留。
- [x] 100 位以内的演员按来源顺序完整保留在详情 API，详情页不因显示上限丢失演员或角色。
- [x] 可选人物资料补充仍限制为每条目最多 12 次请求，不随完整演员表扩大到 100 次。
- [x] 超过上限时稳定截断至 100，解析器与写入器使用同一上限。
- [x] 已有本地演员关系与 NFO 的 Fill Missing 合并语义不变，未知 XML 字段继续保留。

明确不做：为取得 IMDb/TVDb/Douban 人物 ID 对演员逐条请求第三方 API；为缺失身份生成占位 ID。

验证：`cargo test --locked --test metadata_selection`、`cargo test --locked --test nfo_writer`、格式检查。

依赖：LUX-168、LUX-170、LUX-178、LUX-318。

#### LUX-320：NFO 本地媒体流信息序列化

范围：为电影 NFO 增加 `fileinfo/streamdetails` 序列化，将 Lux 有界本地探测结果映射为 Emby/Kodi 常用 video、audio、subtitle 字段，包括已观测到的 codec、bitrate、宽高、aspect、frame rate、language、channels、sampling rate、duration、default 和 forced。只写探测结果中存在且通过范围验证的值。

验收：

- [ ] video/audio/subtitle 轨分别写成对应节点；未知或无效值省略，布尔字段使用兼容的 `True`/`False`。
- [ ] duration ticks 转为 NFO 秒与分钟时使用明确单位，不能把 tick 当作秒。
- [ ] 更新 streamdetails 时保留 NFO 其他字段与未知 XML；没有可用探测结果时不制造空流信息。
- [ ] 解析与写入受既有 NFO 字节数、XML 事件数和字段长度上限保护。

明确不做：从 TMDb 推测媒体编码/分辨率/音轨；读取容器章节；改变播放媒体轨数据库模型。

验证：`cargo test --locked --test nfo_writer`、格式检查。

依赖：LUX-054、LUX-168、LUX-172。

#### LUX-321：电影 NFO 数据库字段与技术信息写回服务

范围：完整电影 NFO 写回时，将数据库中的真实 `sort_title` 和 `added_at` 写入缺失的 `<sorttitle>` 与 `<dateadded>`；已有本地值始终保留。提供 probe-info 写回服务：只接受当前默认本地电影 source，将本地探测结果原子合并为 `fileinfo/streamdetails`，刷新 NFO 指纹，并同步已启用的 `/config/metadata/library` 镜像。该服务本身不由扫描阶段自动调用，接线由 LUX-322 负责。

验收：

- [ ] 数据库 NFO 辅助字段只补空值；已有 `sorttitle`/`dateadded` 不被覆盖，新 NFO 使用 `media_items.sort_title` 与 `added_at`。
- [ ] probe-info 写回只作用于当前默认的本地电影 source，`.strm` 与其他媒体类型返回未处理，不生成本地技术信息。
- [ ] 替换旧 streamdetails 时保留 fileinfo 下其他 XML、TMDb rich 字段与其他未知 XML；写回原子且刷新 NFO 指纹/策略启用的镜像。
- [ ] probe-info writer 的定向测试覆盖嵌入轨道、替换、空数据和镜像行为。

明确不做：为 `.strm` 创建虚构的本地 streamdetails；修改 Emby API 的媒体轨输出；增加新的探测器或 Cargo 依赖；单独为已有缺失的 NFO 新建回填任务。

验证：`cargo test --locked --test nfo_writer`、格式检查。

依赖：LUX-168、LUX-198、LUX-320。

#### LUX-322：本地探测完成后更新 NFO 技术信息

范围：将 LUX-321 的 NFO probe-info 写回服务接入本地媒体探测成功路径。仅在 probe 结果成功并提交数据库后执行；NFO 写回失败通过脱敏错误码记录，不回滚已提交媒体探测数据，也不记录完整媒体路径。

验收：

- [ ] 本地电影 probe 成功后，同名/既有电影 NFO 和策略启用的配置卷镜像包含实际探测到的 streamdetails。
- [ ] 重复探测替换已有 streamdetails，不产生重复轨道；媒体其他元数据保持不变。
- [ ] `.strm` sidecar 的 probe 结果永不写入 `.strm` 相邻 NFO；非默认多版本 source 不污染默认版本 NFO。
- [ ] probe 成功但 NFO 写回失败时，媒体探测状态和轨道数据仍保持成功，可从脱敏日志定位失败类别。

明确不做：代理或读取 `.strm` URL 指向的远程媒体；为缺失的历史 NFO 执行后台回填；更改 Emby 媒体轨 API。

验证：`cargo test --locked --test probe`、`cargo fmt --all -- --check`；任务完成时运行全 Rust 门禁。

依赖：LUX-320、LUX-321。
#### LUX-317：统一登录背景插件与自定义上传图片（实施中）

本任务于 2026-09-30 经项目所有者确认实施，不改写 LUX-259 至 LUX-263 已完成任务的历史验收记录。一个新插件 `org.lux.login-background` 通过插件配置选择 Bing 每日图片、TMDb 日榜或自定义上传图；不合并进或复用 `org.lux.tmdb` 元数据插件。接口草案、阶段计划和验收详见 `docs/LUX-317-PLAN.md`。

统一插件配置字段 `source` 取 `BING_DAILY`、`TMDB_TRENDING` 或 `CUSTOM_IMAGE`。Bing 个人用途确认与 TMDb 非商业许可确认各自独立且默认关闭，仅对应来源启用时校验，不从旧插件配置迁移。Bing/TMDb 请求、URL、榜单选择、品牌署名和宿主 HERO_IMAGE 布局保持 LUX-262/263 的已验收行为。

自定义图片先只支持单张：JPEG/PNG/WebP，最多 5 MiB/20 MP，按原字节存储，不转码、压缩，不写入媒体库。配置值只保存 `sha256:<64 位小写十六进制>` 不透明资源 ID，不存路径/图像字节。仅管理员可上传/替换，且须确认拥有登录公开展示权。Lux 在配置目录安全保存、原子替换和提供固定同源资源路由；插件进程无文件系统权限。该路由仅当统一插件已安装/启用/可用、服务器背景选中它、`source=CUSTOM_IMAGE` 且宿主配置 `customImageRightsConfirmed=true` 时公开；由 Lux 自身检查该确认，不依赖插件 RPC，其他场景 404 并由登录页安全回退。只为统一插件接受精确资源路径，其他插件仍限 manifest HTTPS host allowlist；禁止通用图片代理。

验收：

- [ ] 官方目录只有一个新 ID `org.lux.login-background`；独立于 `org.lux.tmdb`。
- [x] 插件配置单选三种来源；按选择调用唯一 provider。Bing/TMDb 许可确认独立、默认 false；不确认时不访问相应上游。自定义图有单独公开展示许可确认。
- [x] Bing/TMDb 的既有行为和 HERO_IMAGE 左侧大图不变；TMDb 只返回混合日榜中首个电影/剧集的有效 backdrop，不回退 poster；自定义模式无外网调用。
- [x] manifest `image` 字段只用于 login_background，配置值只接受 SHA-256 opaque ID。上传限 5 MiB、JPEG/PNG/WebP、20 MP，并校验 magic bytes；拒绝伪 MIME、SVG、GIF、畸形/超大内容。替换成功后不保留旧自定义文件；失败回滚仍服务旧图。
- [x] 上传端点管理员鉴权+CSRF，服务端固定文件命名，不接受路径/文件名作目标；错误不会覆盖旧图。配置/API/RPC 不泄露文件路径/图片字节。
- [x] 精确固定同源图片路由以 MIME、nosniff、ETag 和 revalidation cache headers 提供；未选中、未启用、未确认 `customImageRightsConfirmed`、缺图、错误 hash 或无效状态时不可公开读取；该许可门由 Lux 宿主执行。登录公开 JSON 不包含路径或字节。
- [ ] 新插件双架构包和目录校验成功后，从目录移除旧 Bing/TMDb ID 并删除两者现存 GitHub Releases 与 release tags（Bing 0.1.0、TMDb 0.1.0/0.1.1 全部资产）；不改 Git 历史、不自动卸载任何用户服务器中的已安装文件。
- [ ] 提供手动迁移说明：安装新包、选择旧来源对应模式、重新单独确认许可、切换服务器背景来源、验证成功后管理员自行卸载旧包；不继承旧许可同意。
- [x] 覆盖来源选择、两项许可门、mock HTTP、恶意/超限上传、原子替换失败、CSRF/未授权、条件 GET/HEAD、未激活资源 404、fallback、双架构打包。

验证：Lux 定向协议/插件/背景资源测试、fmt、Clippy、Web 定向 Vitest/build/Playwright；Lux-plugins mock HTTP/目录测试、fmt/Clippy 与 x86_64/aarch64 release workflow。主索引和旧 Release/tag 清理需在 cutover 后实时核验；部署验证与 CI 分开记录。

阶段：A SDK/宿主安全托管及全目标质量门已于 2026-09-30 通过；原 shutdown 集成测试门限从 10 秒调整到 30 秒（本机冷启动实测约 12 秒），连续定向和完整 all-targets 验证通过。阶段 B（设置 UI 与统一插件）已于 2026-09-30 完成并通过：Lux Web 全量测试 540 项及构建通过，Lux-plugins GitHub Actions run `36738619827` 的 x86_64/aarch64 测试、Clippy、构建、ZIP/manifest/hash 校验全部成功。统一插件分支 `codex/unified-login-background` 已推送；活动 `plugins.json`、正式 Release 与旧包/tag 未改。阶段 B 结束后按阶段门等待项目所有者确认，再进入阶段 C（正式目录切换、旧 Release/tag 清理及手动迁移说明）。完整验证边界见 `docs/LUX-317-PLAN.md`。

明确不做：多图/轮播、任意 URL/路径、插件读宿主文件、图片服务端代理/CDN/转码、媒体库写入、许可同意自动迁移、远程卸载已装插件或重写 Git 历史。

#### LUX-467：让 deferred relation credits 失败后可重试

范围：本地 NFO page 先原子写入 `people.json`，再按最多 16 项提交 credits、source fingerprint 和 relation checksum。文件写入和数据库事务不共享 ACID 边界；数据库写入失败时保留 relation 文件作为恢复快照，并确保 index state/checksum mismatch 让该 revision 保持 stale。deferred credits flush 失败时将失败 chunk 保留在同一个 page collector 中，允许存储故障清除后重试；credits replacement 和 checksum state 继续在单个数据库事务内提交。扫描 batch 在 metadata 处理后统一失效 Home cache 并发送合并后的 Home event。文件、数据库和进程内事件不能组成一个跨系统事务；进程重启后的关系恢复依赖文件快照及既有 person index rebuild reconciliation。

验收：

- [x] 注入 credits/index-state 数据库失败后，relation 文件保留且 current 检查返回 stale；同一 deferred collector 在故障解除后可重试，并保存与文件字节匹配的 checksum。
- [x] 重试后 credits 只有一份；成功 flush 消费 pending 项，重复 flush 为 no-op；flush 串行化以避免同一 collector 的并发重复提交。
- [x] 现有扫描 page/job 边界仍统一失效 Home cache 并 coalesce Home event；文档不将它与文件或数据库提交描述为原子事务。
- [x] people service、metadata/scanner relation-retry 定向测试、build、fmt、Clippy 与 `git diff --check` 通过；验证边界如实记录。

预计文件：`src/application/people/relations.rs`、`src/application/people/service.rs`、`src/storage/repository.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/PERFORMANCE.md`。先添加 database failure / retry / idempotency regression，再保留失败 chunk 并串行化 flush。文件系统与数据库之间没有共同事务；durable relation snapshot 和 stale revision 检测用于恢复，Home invalidation/event 在 scan batch 完成边界合并。

结果（2026-10-09）：旧实现下新回归先失败，证明 DB index-state 写入失败后 relation 文件虽仍在且判 stale，同一个 deferred collector 已丢弃 pending，解除 SQL 故障后无法 flush。修复将 flush 串行化，失败 chunk 按原顺序放回队列头部，避免并发追加的较新快照被失败旧项覆盖；数据库缺失时不消费队列。SQLite trigger 回归确认 failure 后 stale、同 collector retry 后 checksum/current 恢复、重复 flush 无副作用且仅一条 credit。people service library tests 45/45、scanner Home-event worker regression 1/1、`tests/metadata.rs` 21/21 通过；`cargo build --locked`、`cargo fmt --all -- --check`、全目标全 feature Clippy 与 `git diff --check` 通过。使用 Toshiba 的共享 Cargo target，本机 `arm64`。没有运行 PostgreSQL、FNOS 或生产性能测量；relation file、DB transaction 与进程内 Home event 仍是分开的持久化/缓存边界，不声称跨系统 ACID。

补充重启恢复回归（2026-10-09）：`deferred_relation_credit_failure_recovers_from_snapshot_after_restart` 注入 credits/index-state 写入失败后丢弃 page-local collector、关闭并重新打开 SQLite 与 PeopleService；确认 `people.json` 的字节 checksum 保持不变、重启后的 current 检查仍判 stale，再由既有 person index rebuild 从文件快照恢复 credits/checksum 并转为 current。定向命令 `cargo test --locked --lib deferred_relation_credit_failure_recovers_from_snapshot_after_restart` 通过（1/1），使用 Toshiba 共享 Cargo target。本用例模拟持久化边界上的服务状态丢失，不等同于断电/文件系统崩溃测试，也不证明 PostgreSQL 或 FNOS 行为。

#### LUX-470：Logo 银灰默认强调色

范围：个人设置新增「银灰」强调色，并作为未保存颜色偏好时的默认值。沿用 Logo 的中性灰色：深色模式使用 `#CBD5E1`，浅色模式使用 `#3A3F4B`。强调色实心控件使用独立前景色，深色模式为 `#181B22`、浅色模式为白色；选中状态保持既有勾选和开关位置反馈。已保存的莓果、海蓝、琥珀、薄荷偏好继续有效。

验收：

- [x] 无本地偏好或无效强调色时默认银灰；已保存的五种颜色可读取、切换并按账户持久化。
- [x] 个人设置显示银灰选项，使用 `aria-pressed` 标记选择；切换主题保持所选色系并调整银灰明度。
- [x] 银灰文字、实心按钮文字、勾选和启用开关在深浅模式均清晰，相关文字对比度至少 4.5:1。
- [x] Web 全量测试与构建、定向浏览器检查、Rust 格式与静态检查通过；记录本机 ARM 验证边界。

实施切片：A 修改 `web/src/features/account/account-settings.ts`、`web/src/features/account/AccountPage.tsx`、`web/src/react.css`、`web/tests/account-settings.test.tsx` 和本文档；先用账户偏好回归验证默认值、旧颜色兼容和银灰切换。B 修改 `web/src/features/admin/plugin-library.css` 的启用开关前景色，并进行深浅模式控件对比度与 Web 全量验收。只调整 Web 外观和本地偏好，不修改服务端设置或数据库。

结果（2026-10-10）：新账户默认值、银灰偏好读取和银灰选项的回归先失败；实现后账户设置测试 21/21 通过。Web 全量 Node 测试 132/132、Vitest 567/567 和生产构建通过；本机通过 Corepack 调用 pnpm，安装使用 frozen lockfile。Playwright 在模拟 API 的真实账户页面验证默认银灰、海蓝与银灰刷新恢复、主题切换和 Tab/Space 选择；1440/1024/768/320px 下五个选项均无横向溢出，页面无控制台错误。实际 CSS 控件测量中，深色银灰按钮文字对比度 11.61:1、浅色石墨灰按钮文字 10.54:1，强调色对页面分别为 13.72:1 和 9.50:1；检查普通 checkbox、权限勾选、账户/策略/插件启用开关。播放器始终使用黑色背景，因此银灰播放器控件在浅色页面主题下也使用浅银灰与深色前景。`cargo fmt --all -- --check`、全目标全 feature Clippy 和 `git diff --check` 通过，Cargo 使用 Toshiba 共享 target，本机 `arm64`。本任务没有 Rust 逻辑变化，未运行 Rust 测试；浏览器验证使用模拟账号和 API，不证明部署状态、服务端持久化或 NAS 行为。

#### LUX-471：浅色媒体操作菜单与手机详情顶部遮罩

范围：修复媒体操作菜单和手机详情页顶部遮罩遗漏的浅色主题样式。其他浅色配色问题只报告，等待项目所有者选择后再修改。

验收：

- [x] 浅色媒体操作菜单使用浅色背景，标题、图标、悬停和键盘焦点清晰；挂载在 body 的菜单继续继承主题。
- [x] 手机详情页顶部使用浅色渐变遮罩，品牌、返回和菜单按钮同步使用深色前景；保留原有模糊、渐隐与安全区几何。
- [x] 深色模式菜单与顶部遮罩保留原配色；窄屏菜单不超出视口。
- [x] 两项回归在旧实现下失败，修复后定向测试、Web 全量测试和构建通过。

修改文件：`web/src/react.css`、`web/tests/light-theme-contrast.test.ts` 和本文档。仅新增这两处的浅色样式，不调整其他界面配色、服务端逻辑或数据库。

结果（2026-10-10）：定向 Vitest 18/18、全量 Node 134/134、Vitest 569/569 与生产构建通过。Playwright 使用本地模拟 API，在 390px 和 320px 视口检查浅色菜单、独立手机顶部遮罩与菜单视口边界，1440px 检查桌面菜单，并验证深色原配色。截图和 computed style 确认 body portal 正常应用浅色主题，菜单悬停使用深色文字。`cargo fmt --all -- --check`、全目标全 feature Clippy 和 `git diff --check` 通过；本机 `arm64`，Cargo 使用 Toshiba 共享 target。本任务没有 Rust 行为变化，未运行 Rust 测试；不证明部署状态或真实 iOS Safari 渲染。

#### LUX-472：浅色模式其余颜色问题

范围：根据 LUX-471 浅色模式检查报告，修复获准修改的七类问题：媒体编辑弹窗、手机详情操作、失败状态标题、提示和后台任务/日志状态颜色、加载骨架、缺失图片占位图，以及浅色背景上的键盘焦点轮廓。播放器控件处于深色背景，继续使用白色焦点轮廓。

验收：

- [x] 媒体编辑相关弹窗在浅色主题中使用浅色表面、深色标题和可读输入文字；深色配色不变。
- [x] 手机详情页收藏、已看和更多操作在浅色主题中没有深色图标底或白色悬停字；已看状态使用可读的绿色。
- [x] 普通页面与媒体库失败状态标题使用浅色主题文字颜色。
- [x] 通用错误/成功、账户通知、任务状态与日志级别使用浅色背景上满足 4.5:1 对比度的语义色。
- [x] 首页和媒体库加载骨架、媒体海报与后台媒体库封面占位图使用浅色渐变。
- [x] 浅色页面的通用和后台策略焦点轮廓清晰；播放器保留白色焦点轮廓。
- [x] 新增回归覆盖全部七类问题；Web 全量测试、生产构建和本地浏览器样式检查通过。

修改文件：`web/src/react.css`、`web/tests/light-theme-contrast.test.ts` 和本文档。只调整 Web 外观与回归覆盖，不修改服务端或数据库。

结果（2026-10-10）：旧样式下新增七组回归全部失败，修复后主题定向测试 18/18、Web 全量 Node 134/134、Vitest 576/576 通过，TypeScript 检查和 Vite 生产构建通过。Playwright 以本地样式夹具在 390px 与 320px 检查计算样式；320px 页面宽度未溢出，手机操作默认文字为深灰、悬停文字为深色，已看状态为深绿，焦点轮廓为深色；播放器键盘焦点仍为白色 3px。语义错误、成功、警告、活动色对浅色背景的自动对比度断言均达到 4.5:1。`cargo fmt --all -- --check`、Toshiba 共享 target 上的全目标全 feature Clippy 和 `git diff --check` 通过；本任务仅修改 Web，没有 Rust 行为变化，未运行 Rust 测试。这里只验证本地 Web 样式，不代表部署状态或真实 iOS Safari 渲染。

#### LUX-473：压缩移动端仪表盘媒体数量卡片并同步浏览器主题色

范围：Lux 管理仪表盘在手机宽度下让电影数量和剧集数量卡片并排显示，其余概览卡片继续占满一行。应用账户主题时同步更新 `meta[name="theme-color"]`，使 Safari 页面顶部浏览器区域匹配当前 Lux 深浅主题；保留现有桌面布局、卡片内容、主题背景和深色默认值。

验收：

- [x] 640px 及以下，电影数量与剧集数量卡片左右并排，服务器概况、用户、CPU、内存和存储卡片继续单列全宽；较宽视口布局不变。
- [x] 浅色主题将浏览器 `theme-color` 更新为浅色页面背景 `#f4f3f1`，深色主题更新为 `#050506`；主题切换后无需刷新即可更新。
- [x] 两项行为均有回归测试；Web 全量测试和生产构建通过。

预计文件：`docs/LUX-DEVELOPMENT.md`、`web/src/react.css`、`web/src/features/account/account-settings.ts`、`web/tests/admin-dashboard-layout.test.mjs`、`web/tests/account-settings.test.tsx`。

明确不做：其他仪表盘重排、浏览器 UI 自动读取 CSS 颜色、浅色主题之外的主题改动或真实 iOS 设备渲染声明。

结果（2026-10-10）：新布局和主题色回归在旧实现下均失败，修改后定向测试通过；Web 全量 Node 测试 135/135、Vitest 579/579 和生产构建通过。Playwright 的 CSS 预览在 390px 与 320px 检查到电影/剧集卡同排、用户卡全宽，页面滚动宽度分别等于视口宽度。主题色切换由账户主题单测验证。浏览器预览使用 Chromium，不代表真实 iOS Safari 渲染或部署状态。

#### LUX-474：加宽手机管理页主体内容

范围：在手机宽度下让管理页主体使用完整可用宽度，并将内容区左右留白缩至至少 12px，同时避开设备安全区。保留平板、桌面和普通媒体浏览页面的现有布局。

验收：

- [x] 560px 及以下，管理页外层占满视口，主体左右留白至少 12px，并避开更大的设备安全区。
- [x] 320px 与 390px 手机视口下，管理页主体变宽且页面没有横向溢出；561px 及以上布局不变。
- [x] 新布局有回归测试；Web 全量测试和生产构建通过。

预计文件：`docs/LUX-DEVELOPMENT.md`、`web/src/react.css`、`web/tests/admin-dashboard-layout.test.mjs`。

结果（2026-10-10）：手机断点将管理页扩至视口宽度，左右安全区感知内边距最少 12px；平板以上和普通媒体浏览页规则不变。定向布局测试、Web Node 测试 136/136、Vitest 579/579、生产构建、Rust fmt/build/Clippy 均通过。Playwright Chromium 样式预览在 390px 下主体可用宽度 366px，320px 下 296px；滚动宽度分别与视口相等。561px 下保留 92% 外层宽度与 20px 边距；1280px 桌面布局也保持原有比例。Rust 全目标测试的首次运行有 835 个单测通过、20 个忽略，随后在 `tests/scanned_metadata.rs::manifest_scan_indexes_poster_while_local_nfo_is_blocked` 遇到一次本地元数据批次失败；单独复跑该用例以及整个 `scanned_metadata` 目标（23/23）均通过。该前端改动未修改 Rust 源码，全目标套件未重新运行；浏览器检查使用本地 CSS 预览，不代表真实 iOS Safari 渲染或部署状态。

#### LUX-475：渐进扫描补全剧集与季度容器元数据

范围：workflow 3 本地 metadata completeness 和实时增量扫描在当前分集媒体源通过有效性检查、其本地图片/NFO 处理完成后，将该分集及其同库、未删除的父 SEASON 与 SERIES 一并纳入本地完整性计划。按现有扫描策略只派发确实缺失且可请求的能力；同一页多个分集共享的季/剧集去重。source identity 检查失败或层级关系无效时不得借旧分集身份调度容器。保持本地 NFO/图片优先级、字段锁定、INCREMENTAL_SCAN 策略快照、活动任务去重和独立异步 FILL_MISSING worker。

验收：

- [x] 开启自动补缺的增量扫描为分集及其季、剧集容器保存完整性状态，并将三个条目合并进有界 FILL_MISSING 工作。
- [x] 关闭自动补缺时仍保存这三个条目的本地缺失状态，但不创建在线任务。
- [x] source identity 检查失败、文件已缺失、条目已删除或容器跨库/类型不符时，不扩展到其父项。
- [x] 同页多个分集共享的季和剧集只参与一次计划/派发；现有去重、策略快照和分集任务行为不变。
- [x] 不新增 schema、公共 API 或扫描路径中的 provider 请求；provider 仍只由现有后台 worker 调用。

验证：定向 cargo test --locked --lib 覆盖 source 身份/父级扩展和增量扫描派发；cargo build --locked、cargo test --locked --all-targets、cargo fmt --all -- --check、cargo clippy --locked --all-targets --all-features -- -D warnings。Cargo 命令使用 Toshiba 原项目共享 target。

预计文件：src/storage/jobs.rs、src/application/metadata.rs、src/application/scanner.rs、docs/LUX-DEVELOPMENT.md。

结果（2026-10-11）：增量扫描的本地 metadata worker 在分集本地检查成功后，将所属季/剧集加入 completeness 计划，并在派发前去重；本地 outbox/backfill 路径则在首选 source 身份校验通过后扩展层级。失败的本地 NFO/图片项不会触发容器补缺。SQLite 回归覆盖首选 source 变化、文件缺失、条目删除和层级扩展；增量扫描回归用同季两集验证只产生一个季项、一个剧集项及两个分集项，并验证策略关闭时仍保存三类缺失状态。

验证：存储与增量扫描定向单测通过；scanning_jobs 81/81、scanned_metadata 23/23、scanned_series_metadata 3/3；cargo build --locked、cargo test --locked --all-targets（库测试 835 passed、20 ignored，所有集成目标通过）、cargo fmt --all -- --check、cargo clippy --locked --all-targets --all-features -- -D warnings 通过。Cargo 使用 Toshiba 原项目共享 target。本机 arm64；未部署 FNOS、未触发生产刮削。

#### LUX-476：补充元数据刮削逐条诊断日志

范围：为元数据刷新/补全任务增加结构化运行日志，帮助按 job ID 和 item ID 定位每条失败的阶段与原因。记录刮削器选择、插件不可用/超时/退出/无效响应、候选匹配未确认、元数据写回、图片下载/校验/路径写回及可用的图片上游 HTTP 状态码。主、备用或补充来源的单次尝试失败时，即使后续回退成功，也单独保留尝试记录；主来源返回低置信度候选、需要人工复核时记录 `NEEDS_REVIEW`，但保留原完成状态。没有选中刮削器而被跳过的条目单独记录 `NO_SCRAPER_SELECTED` 原因。保留现有任务状态、持久化错误码和 API DTO；LOW_CONFIDENCE 仍是完成状态下带错误码的条目，任务详情必须列出所有带错误码的条目。

运行日志事件名固定为 `metadata_reidentify_item_issue`、`metadata_reidentify_item_skipped` 和 `metadata_reidentify_scraper_attempt_issue`；逐尝试事件额外包含 `scraper_id`、`scraper_role` 和 `attempt_outcome`。字段使用 `job_id`、`item_id`、`mode`、`item_status`、`error_code`、`failure_stage`、`failure_reason`、`upstream_status` 和 `item_result_recorded` 等稳定值。禁止写入标题、路径、完整外部错误文本、图片 URL、查询参数或凭据。管理员可从“任务与日志”导出运行日志后按事件名、job ID 或 item ID 搜索。

验收：

- [x] 每条已记录的失败/低匹配结果产生带 job/item ID、模式、阶段和稳定原因码的 JSON 运行日志；图片上游 HTTP 状态存在时可查询。
- [x] 主、备用或补充刮削器尝试的失败即使后续来源成功也会记录，并标明来源 ID、角色和尝试结果；低置信度待复核结果记录为 `NEEDS_REVIEW`。
- [x] 未选刮削器导致补全项跳过时产生 `metadata_reidentify_item_skipped` 日志，但不改变该任务的既有状态语义。
- [x] 原始 provider 错误、URL、标题、路径和凭据不会写入诊断日志；图片状态码只在已知时记录。
- [x] 元数据任务详情在返回的条目页中包含所有 `error != null` 的条目，包括状态为 `COMPLETED` 的 `LOW_CONFIDENCE`；没有问题的条目保持隐藏。
- [x] 不新增 migration、公共 API 字段或日志查询端点；已有任务错误码和状态合同保持不变。

验证：Toshiba 原项目共享 target 下，刮削 service 单测、`reidentify` 集成测试、`cargo build --locked`、library-only Clippy、格式检查、管理任务页定向测试、Web 类型检查和生产构建通过。全量 Rust 测试与 all-targets Clippy 被未修改的 `tests/performance.rs` 中未定义变量/函数挡住；Web 全量测试有 5 个 `HomePage` 轮播测试失败，单独运行该文件可复现，其余 Node 测试和 576 个 Vitest 通过。本次改动没有部署到生产，也没有对报告用户的媒体库执行刮削。

预计文件：`src/application/reidentify.rs`、`tests/reidentify.rs`、`web/src/features/admin/AdminOperationsPage.tsx`、`web/tests/admin-operations.test.tsx`、本文档。

### 阶段 24：扫描应用服务模块化与流程简化

本阶段只整理 `application::scanner` 内部实现，不改变 HTTP/Emby 合同、数据库 schema/SQL 事务、扫描状态与错误码、批次大小、并发上限、工作流版本或运行时副作用顺序。`scanner` 根模块保留稳定的公开路径与薄 facade；子模块按文件处理、任务生命周期、Manifest、reconciliation、增量扫描、本地 metadata worker 和后处理划分。文件系统 descriptor/stat/STRM 安全访问继续由现有 `scanner/manifest.rs` 承担。

各任务最多预计修改 5 个文件，逐项独立验证和提交。实现中按增量移动代码，不引入通用 pipeline/dispatcher 抽象，不合并事务、不新增数据库状态读取；只有现有回归不能表达关键行为时才补充行为测试。扫描服务公开类型和 `crate::application::scanner::*` 调用路径由 facade re-export 保持。

#### LUX-477：拆分 LibraryScanner 文件处理实现

范围：把 `LibraryScanner` 的扫描/文件准备实现与文件名解析、混合库分类职责移入有界子模块。公开的 `LibraryScanner`、解析器和指纹 API 继续从 `application::scanner` 导出；内部跨模块方法使用最小可见性。纯移动，不改变扫描、归并、路径校验、探测或写入行为。

验收：

- [x] `scanner.rs` 不再承载完整的 `LibraryScanner` 实现和文件名/混合库分类实现。
- [x] `LibraryScanner`、`ParsedMovieFilename`、`ParsedEpisodeFilename`、`parse_movie_filename`、`parse_episode_filename` 和 `compute_file_fingerprint` 保持现有公开路径与签名。
- [x] 电影、剧集、混合库、文件移动、版本归并和 STRM 路径行为由既有测试覆盖且通过。

验证：`cargo test --locked --test scanner --test series_scanner --test mixed_scanner`、`cargo test --locked --lib scanner::tests`、fmt 检查和 build。

预计文件：`src/application/scanner.rs`、`src/application/scanner/library.rs`、`src/application/scanner/library/prepare.rs`、`src/application/scanner/names.rs`、本文档。

结果（2026-10-12）：`LibraryScanner` 扫描实现、文件准备、文件名解析与混合库分类移入 `scanner/library.rs`、`scanner/library/prepare.rs` 和 `scanner/names.rs`；文件遍历和受限 STRM 读取留在 library 模块。根模块通过 re-export 保持现有公开路径。scanner 根从约 15,151 行减至 11,701 行，拆出的文件处理模块分别约 2,070、900 和 511 行。scanner library tests 41 passed、1 ignored；`scanner`、`series_scanner`、`mixed_scanner` 集成目标 30 项通过；`cargo build --locked`、fmt 检查和 `git diff --check` 通过。该任务只移动实现并调整子树内可见性，不声称运行时性能变化。

#### LUX-478：隔离扫描任务 facade 与单元测试

范围：将 `ScanJobService` 共享状态、生命周期 guard 和稳定公开任务类型归入 `scanner/jobs`，把 scanner 内联单测移至 `scanner/tests.rs`。根模块只 re-export 公开合同并组合子模块，不复制锁、Notify、取消标记或状态注册表。

验收：

- [x] `ScanJobService`、`ScanJob`、`ScanJobError`、`ScanBatchReport`、`IncrementalScanChange` 与 `LocalMetadataRefreshAccepted` 的现有导出路径不变。
- [x] 活动任务 guard、删除 fencing、取消标志和 worker 通知仍共享同一组 `Arc` 状态。
- [x] 所有既有 scanner 单测仍在 `application::scanner::tests` 名称空间下运行。

验证：`cargo test --locked --lib scanner::tests`、`cargo test --locked --test shutdown_jobs --test scanning_jobs`、fmt 检查和 build。

预计文件：`src/application/scanner.rs`、`src/application/scanner/jobs/mod.rs`、`src/application/scanner/tests.rs`、本文档。

结果（2026-10-12）：`ScanJobService` 状态、运行/删除 guard、worker handle 与公开任务 DTO/error 移至 `scanner/jobs/mod.rs`，scanner 单测移至 `scanner/tests.rs`，根模块 re-export 保持类型路径及 `scanner::tests` 测试路径。取消与删除协调仍共享原 `Arc` 注册表、通知和标志。scanner library tests 41 passed、1 ignored；`scanning_jobs` 81/81、`shutdown_jobs` 1/1 通过；fmt 与 `git diff --check` 通过。剩余 runner 和各扫描阶段仍在 `scanner.rs`，后续任务继续拆分。

#### LUX-479：拆分 Manifest 扫描阶段

范围：分别移动 Manifest 目录发现与文件系统观察、差异计算/批次应用实现。保留 workflow 2/3、lite discovery、持久化 frontier、根身份确认、批次事务、取消与重试路径；文件描述符和受限 STRM 读取仍由既有 `scanner/manifest.rs` 实现。

验收：

- [x] runner 只根据已持久化阶段选择 discovery、diff、apply 或 indexed completion，不内嵌目录处理算法。
- [x] 根被替换/卸载、目录观察改变、并发增量索引、批次提交失败、取消/恢复与 workflow 2 兼容语义保持。
- [x] 所有 Manifest 定向回归及 scanner 单测通过。

验证：`cargo test --locked --test scanning_jobs --test scanner --test scanned_metadata`、scanner library tests、fmt 和 build。

预计文件：`src/application/scanner.rs`、`src/application/scanner/jobs/mod.rs`、`src/application/scanner/jobs/manifest_discovery.rs`、`src/application/scanner/jobs/manifest_index.rs`、本文档。

结果（2026-10-12）：Manifest 目录发现、受观察保护的批次提交与 lite discovery 移入 `jobs/manifest_discovery.rs`；差异计算、批次应用、postprocessing target materialization 和 indexed completion 移入 `jobs/manifest_index.rs`。runner 仍按持久化 Manifest 状态选择阶段，workflow 兼容判断、批次大小和调用顺序保持原样；底层 descriptor/stat/STRM 安全访问仍在 `scanner/manifest.rs`。定向集成测试 `scanned_metadata` 23 项、`scanner` 18 项、`scanning_jobs` 81 项通过；scanner library tests 41 项通过、1 项按要求忽略；`cargo build --locked`、fmt 与 `git diff --check` 通过。根模块从 8,756 行减至 6,353 行，两个 Manifest 阶段模块分别 1,436 行和 973 行。仅验证本地行为，无运行时性能结论。

#### LUX-480：拆分 reconciliation 与增量扫描

范围：将旧持久化 reconciliation 与实时/定向增量扫描编排分别移入所属任务子模块。保持发现快照、根不可用保护、删除确认、同路径更新、增量路径队列和并发协调行为。

验收：

- [x] reconciliation 和 incremental 算法从 runner 中分离，阶段状态由现有数据库记录驱动。
- [x] 根离线、扫描期间根替换、文件身份变化、reconciliation 重试和实时扫描抢占回归保持。
- [x] 不新增 whole-library request path 或无界后台工作。

验证：`cargo test --locked --test scanning_jobs --test watch --test scanner --test series_scanner --test mixed_scanner`、fmt 和 build。

预计文件：`src/application/scanner.rs`、`src/application/scanner/jobs/mod.rs`、`src/application/scanner/jobs/reconciliation.rs`、`src/application/scanner/jobs/incremental.rs`、本文档。

结果（2026-10-12）：持久化 reconciliation 的目录发现、文件批次、指纹与缺失确认移入 `jobs/reconciliation.rs`；实时/定向增量批次及媒体/sidecar 处理移入 `jobs/incremental.rs`。root runner 保留既有任务读取与阶段路由，未增加整库请求路径、后台队列或工作单元。定向集成目标 `mixed_scanner` 1 项、`scanner` 18 项、`scanning_jobs` 81 项、`series_scanner` 11 项、`watch` 5 项全部通过；`cargo build --locked`、fmt 和 `git diff --check` 通过。根模块从 6,353 行减至 4,355 行，两个新模块分别 1,470 行和 534 行；扫描和并发语义由原有回归覆盖，不据此推断性能变化。

#### LUX-481：抽离本地元数据 outbox worker

范围：把本地 metadata batch/backfill 辅助状态、outbox worker、每任务 worker、完成 waiter 与 completeness 协调放入独立模块。保持 source identity 快照、策略快照、批次顺序、重试和 Notify/fallback 语义。

验收：

- [x] 扫描编排只调用 worker 的明确入口，不持有 worker 循环和队列细节。
- [x] NFO/图片失败可重试、取消终结、源变化拒绝旧结果、父级 metadata 补缺和 worker 关闭行为保持；series 不可读海报用例通过。
- [x] 不将在线 provider 请求移入扫描请求路径；FILL_MISSING 仍由现有后台 worker 处理。

验证：`cargo test --locked --test scanned_metadata --test scanned_series_metadata --test scanning_jobs`、`cargo test --locked --lib scanner::tests`、fmt 和 build。

预计文件：`src/application/scanner.rs`、`src/application/scanner/jobs/mod.rs`、`src/application/scanner/jobs/incremental.rs`、`src/application/scanner/jobs/local_metadata.rs`、本文档。

结果（2026-10-12）：local metadata 的 batch/backfill/completeness helpers、单任务 worker、outbox worker、等待器、取消收尾及 refresh 实现移入 `jobs/local_metadata.rs`；outbox worker 从 incremental 模块抽出，scanner facade 保留稳定服务 API 与 worker 调用入口。`scanned_metadata` 23 项、`scanning_jobs` 81 项、scanner 单测 41 项通过（1 项按要求忽略）。海报失败用例原先用无序 `LIMIT 1` 读取批次；同一任务可同时包含已完成批次和图片失败批次，因此测试有时读到正常行。现改为按图片错误选取失败批次并用同一 batch ID 验证重试，重复运行通过；无需修改扫描逻辑。`cargo build --locked`、fmt 和 `git diff --check` 通过。根模块从 4,355 行减至 3,195 行，`jobs/local_metadata.rs` 为 1,415 行；未把在线 provider 请求移入扫描路径，也未增加后台队列。

#### LUX-482：拆分任务生命周期与扫描后处理

范围：将创建/取消/重试/运行 fencing 与扫描完成后的 probe、metadata、thumbnail、cover、事件发布拆入独立职责模块。索引完成与后处理仍保持两个阶段；后处理继续释放共享扫描锁并使用现有有界 worker。

验收：

- [x] lifecycle 负责任务状态转换与锁；postprocessing 负责已有后处理调用，不互相复制状态。
- [x] 增量与全量扫描的完成、取消、失败重试、锁释放、Home 刷新、Webhook/用户事件顺序保持。
- [x] 相关定向测试通过，文件职责与模块可见性记录清楚。

验证：`cargo test --locked --test scanning_jobs --test shutdown_jobs --test scanned_metadata --test watch`、fmt 和 build。

预计文件：`src/application/scanner.rs`、`src/application/scanner/jobs/mod.rs`、`src/application/scanner/jobs/lifecycle.rs`、`src/application/scanner/jobs/postprocessing.rs`、本文档。

结果（2026-10-12）：任务创建、取消/重试、run fencing、状态查询与失败收尾移入 `jobs/lifecycle.rs`；后处理执行体、扫描锁协调、probe、metadata、thumbnail、cover 与 webhook 发布移入 `jobs/postprocessing.rs`。`run_to_completion*` 公开入口仍与每任务 worker/run guard 协调放在 `jobs/local_metadata.rs`，并调用 postprocessing 执行体；`ScanJobService` 公开方法路径与签名保持不变。索引完成和后处理仍由既有 `scan_phase` 分离，后处理仍在现有有界 worker 中释放共享扫描锁。定向集成目标 `scanned_metadata` 23 项、`scanning_jobs` 81 项、`shutdown_jobs` 1 项、`watch` 5 项全部通过；`cargo build --locked`、fmt 与 `git diff --check` 通过。根模块从 3,195 行降至 826 行；lifecycle 与 postprocessing 模块分别 1,323 行和 1,078 行。未改状态、事件或副作用顺序，也未据此作性能结论。

#### LUX-483：收口 scanner facade 与 reconciliation helper

范围：把 scanner 根模块残留、同时供 `LibraryScanner` 与 reconciliation 使用的 batch/task helper 归入 `scanner/library.rs`，跨模块调用使用 scanner 内部最小可见性；根模块只保留 facade、稳定共享类型、错误/API 与 re-export。`lifecycle.rs` 已承载扫描批次 runner，本任务不再新增重复 runner 模块。只移动实现，不改行为。

验收：

- [x] `scanner.rs` 不超过 600 行，电影/剧集文件解析和 fingerprint 等公开路径保持不变。
- [x] library/reconciliation 共用的 task/result helper 位于 `library` 模块，未改变扫描批次、事务或错误传播。
- [x] scanner 单测、scanning_jobs 定向回归、fmt 和 build 通过。

验证：`cargo test --locked --lib scanner::tests`、`cargo test --locked --test scanning_jobs --test scanner`、fmt 和 build。

预计文件：`src/application/scanner.rs`、`src/application/scanner/library.rs`、`src/application/scanner/jobs/reconciliation.rs`、本文档。

结果（2026-10-12）：电影/剧集并发扫描与 reconciliation 共用的 task/result 类型和 JoinSet 收集 helper 从根模块移入 `scanner/library.rs`，reconciliation 通过 scanner 子树内受限可见性调用。根模块为 557 行，保留 facade、稳定共享 DTO/error、文件 fingerprint API 与 re-export；不新增与 `jobs/lifecycle.rs` 重叠的 runner。scanner 单测 41 项通过（1 项按要求忽略），`scanner` 18 项、`scanning_jobs` 81 项通过；build、fmt 和 `git diff --check` 通过。纯移动，无行为或性能结论。

#### LUX-484：简化 reconciliation 与 Manifest 编排

范围：整理 `run_reconciliation_file_batch`、`run_movie_reconciliation_file_batch`、Manifest diff/apply，将已有准备、根与文件身份校验、事务提交和事件发布步骤提取成有名操作。只重排现有控制流，不改变事务、取消检查、错误传播和副作用顺序。

验收：

- [x] 相关单个编排函数目标不超过 150 行；超出时在结果记录单一职责与保留理由。
- [x] 根离线/替换、同路径更新、并发增量索引、提交失败、删除确认与 workflow 2 兼容语义保持。
- [x] 相关定向测试通过。

验证：`cargo test --locked --test scanning_jobs --test scanner --test series_scanner --test mixed_scanner`、fmt 和 build。

预计文件：`src/application/scanner/jobs/reconciliation.rs`、`src/application/scanner/jobs/manifest_index.rs`、本文档。

结果（2026-10-12）：reconciliation 文件批次拆为 entry 分类、regular 扫描、并发文件准备和逐根提交；movie 路径另拆为 quick/sidecar、同目录分组扫描、准备和提交。Manifest diff/apply 拆为持久化 delta、根身份校验、准备、删除确认与事务提交步骤。相关方法均不超过 150 行，未改每根事务、root fencing、取消检查、增量冲突处理、删除确认或事件顺序。定向集成目标 `mixed_scanner` 1 项、`scanner` 18 项、`scanning_jobs` 81 项、`series_scanner` 11 项全部通过；build、fmt 与 `git diff --check` 通过。

#### LUX-485：简化扫描完成编排并关闭阶段 24

范围：整理增量批次与全量/增量完成、取消收尾和后处理编排，将已有步骤提取成明确的准备、校验、提交和发布操作。保持索引完成与后处理两个阶段；不改变事务、锁释放、取消、错误传播、worker 上限及事件顺序。

验收：

- [x] 相关单个编排函数目标不超过 150 行；超出时在结果记录单一职责与保留理由。
- [x] 增量/全量成功、取消、失败重试、共享扫描锁释放、Home 刷新与 webhook/用户事件顺序保持。
- [x] 全量 Rust 完成门通过：build、all-targets tests、fmt、all-targets/all-features Clippy；扫描定向目标通过。
- [x] 模块边界记录更新；进入下一阶段前仍须项目所有者确认。

验证：Toshiba 挂载检查后设置共享 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target`；运行扫描定向目标与 `cargo test --locked --all-targets`、`cargo build --locked`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`，记录 `uname -m`。不得以本机 ARM 结果推断 NAS/x86 性能。

预计文件：`src/application/scanner/jobs/incremental.rs`、`src/application/scanner/jobs/postprocessing.rs`、`src/application/scanner/jobs/local_metadata.rs`、本文档。

结果（2026-10-12）：incremental batch 与空队列完成拆为路径处理、终结状态更新；`run_to_completion*` 公共入口和 run guard 移到 postprocessing，主循环拆为扫描批次循环、Manifest target 准备、增量收尾和全量 probe/metadata/cover/thumbnail/completion 阶段。局部 worker 与 outbox loop 拆为单页/单批处理和等待入口；NFO 并发上限、批次优先级、通知与 fallback 顺序保持。相关 orchestration 方法均不超过 150 行。扫描定向集成 `mixed_scanner` 1 项、`scanned_metadata` 23 项、`scanned_series_metadata` 3 项、`scanner` 18 项、`scanning_jobs` 81 项、`series_scanner` 11 项、`shutdown_jobs` 1 项、`watch` 5 项通过（合计 143 项）；scanner 单测 41 项通过、1 项按要求忽略。海报失败用例的多批次选择误差已修正，重复 5 次及全目标测试通过。全目标完成门的原阻塞来自 `6d5b758c` 合并时未完整整合的性能请求探针：`tests/performance.rs` 引用了缺失变量、函数和批次初始化。现恢复到合并前可编译的基准 harness；原有基准测试仍保留并保持显式忽略，未运行性能矩阵，也不据此声称性能结果。`cargo test --locked --all-targets`、`cargo build --locked`、fmt、`cargo clippy --locked --all-targets --all-features -- -D warnings` 和 `git diff --check` 全部通过。机器为 `arm64`，不得外推 NAS/x86 性能。阶段24的代码与全量 Rust 完成门已通过；下一阶段仍须项目所有者确认。

依赖：无。明确不做：Storage/API 重构、schema/SQL 修改、新后台 worker、新扫描状态、新性能承诺或部署。

### 阶段 25：共享存储层 jobs 职责拆分与事务编排重构

本阶段以阶段 24 完成版本 `b08bdcf4` 为基线，整理 `src/storage/jobs.rs` 的共享存储职责。继续使用 `Database` 的现有方法作为内部调用面，保持方法签名、存储导出路径、SQL、批次大小、状态转换、错误和提交语义。先将根目录、字幕、文件系统条目、媒体源与关机处理迁入所属 storage 模块，再按 scan/Manifest、reconciliation、增量扫描、本地 metadata、metadata reidentify、STRM probe 分拆 jobs。最后整理 Manifest discovery/apply、diff 和 FILL_MISSING 大函数。每个 LUX 任务最多预计修改 5 个文件，逐个完成、验证并独立提交。

阶段验收：`jobs.rs` 只保留模块声明和需要稳定的导出，目标不超过 200 行；事务编排函数目标不超过 150 行，纯 SQL 阶段超出时须在结果中说明。跨步骤的子函数沿用调用方的 `&mut Transaction<'_, Any>`；不得在步骤内另开事务或改变锁顺序、SQL/绑定顺序、计数、批次边界及取消 fencing。保留 `storage::repository::jobs::tests` 测试名称空间。对文件和函数的移动须检查 repository 内调用点、application/storage 调用点与实际路由引用；`Database` 现有方法名和路由合同不变。

#### LUX-486：拆分媒体库根目录存储操作

范围：将 library root 的增删查、扫描游标和可用性操作从 `jobs.rs` 移入 `storage/library_roots.rs`。保留 `Database` 方法签名、SQL、删除清理范围及路径调用点。

验收：

- [x] 根目录存储实现不再位于 `jobs.rs`。
- [x] 既有 `Database` 调用点和路由调用无需改名，根目录删除与扫描游标行为保持。
- [x] library deletion、storage 和 jobs 单测通过；fmt/build 与 `git diff --check` 通过。

验证：确认 `/Volumes/Toshiba` 已挂载后使用共享 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target`；运行 `cargo test --locked --lib storage::repository::jobs::tests`、`cargo test --locked --test storage --test library_deletion`、`cargo fmt --all -- --check`、`cargo build --locked` 及 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/library_roots.rs`、`src/storage/repository.rs`、本文档。

结果（2026-10-12）：library root 的 13 个 `Database` 方法移入 `storage/library_roots.rs`，原方法名、签名和 repository 内调用路径保持。核对了 API、application、storage 和 tests 的所有同名调用。基线与迁移后 jobs 单测均为 14 passed、1 项 PostgreSQL 用例 ignored；`storage` 52 项与 `library_deletion` 1 项均通过；fmt 和 `git diff --check` 通过。

#### LUX-487：拆分字幕存储操作

范围：将外部字幕定位、字幕流查询与字幕记录更新从 `jobs.rs` 移入 `storage/subtitles.rs`。保留现有 Emby/Lux DTO 映射和 SQL 语义，不改变播放或字幕路由合同。

验收：

- [x] 字幕存储方法不再位于 `jobs.rs`，`Database` 方法签名保持。
- [x] Emby 与 Lux 字幕查询/更新调用点保持原方法名和响应映射。
- [x] 字幕集成测试、fmt/build 与 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --test subtitles`、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/subtitles.rs`、`src/storage/repository.rs`、本文档。

结果（2026-10-12）：字幕定位、轨道分页与外部字幕更新移入 `storage/subtitles.rs`，`Database` 方法名和返回映射保持。核对了 Emby/Lux media handlers、管理员 handler 与 storage tests 的原调用点；`subtitles` 1 项回归通过，fmt 和 `git diff --check` 通过。

#### LUX-488：拆分文件系统条目存储操作

范围：将 filesystem entry 查询、移动、批量 observed/restore 与 missing 状态更新移入 `storage/filesystem.rs`。文件系统状态变化仍使用现有的事务入口；reconciliation 对事务内恢复 helper 的调用保持同一事务。

验收：

- [x] 文件系统条目操作不再位于 `jobs.rs`，事务内 helper 可见性限于 storage 子树。
- [x] 路径、inode、seen/missing 状态和 reconciliation 批次确认语义保持。
- [x] storage、媒体库/媒体删除与 jobs 单测通过；fmt/build、`git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 jobs 单测、`cargo test --locked --test storage --test media_deletion --test library_deletion`、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/filesystem.rs`、`src/storage/repository.rs`、本文档。

结果（2026-10-12）：15 个 filesystem entry 查询/移动/观察/缺失状态方法移入 `storage/filesystem.rs`。reconciliation 调用的批量 restore 和 missing-path helper 仅开放到 repository 子树；媒体移除刷新仍复用 jobs 中的原事务 helper。核对了 API、application、storage 与 tests 调用点。jobs 单测 14 passed、1 项 PostgreSQL 用例 ignored；`storage` 52 项、`media_deletion` 1 项、`library_deletion` 1 项通过；fmt 和 `git diff --check` 通过。

#### LUX-489：拆分媒体源存储操作

范围：将媒体源查询、重分配、探测状态、原子删除及关联媒体条目移除/恢复移入 `storage/media_sources.rs`。删除、恢复及 removal refresh 继续接收并使用原事务；媒体源删除批量上限与其回归保持在所属模块。

验收：

- [x] 媒体源写入/删除和关联媒体状态维护不再位于 `jobs.rs`。
- [x] `Database` 方法名、批量边界、默认来源约束、级联 item 清理和回滚语义保持。
- [x] 媒体源原子删除单测、媒体删除与 storage 集成测试、fmt/build 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 jobs 单测、`cargo test --locked --test media_deletion --test storage`、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/media_sources.rs`、`src/storage/repository.rs`、本文档。

结果（2026-10-12）：媒体源查询、STRM/variant 更新、item 重分配、探测状态、恢复、删除与关联媒体移除操作移入 `storage/media_sources.rs`；删除批次上限随模块迁移，单测仍从 jobs 测试入口使用该合同。原子事务和批次常量保持。核对了应用层删除、扫描、catalog 与 `storage/media.rs` 的方法调用。jobs 单测 14 passed、1 项 PostgreSQL 用例 ignored；`media_deletion` 1 项及 `storage` 52 项通过；fmt 与 `git diff --check` 通过。

#### LUX-490：隔离 storage jobs 单元测试

范围：将 `jobs.rs` 末尾的内联单测迁入 `storage/jobs/tests.rs`，由根模块使用路径属性声明。测试名称空间和测试体保持，允许继续访问其父模块中的私有测试辅助函数。

验收：

- [x] 所有 jobs 内联单测位于独立测试文件中，路径仍为 `storage::repository::jobs::tests`。
- [x] 测试导入所用的媒体源删除批次上限继续指向 owning module。
- [x] 单测数量、忽略项和结果与迁移前一致；fmt、build 与 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；迁移前后运行 `cargo test --locked --lib storage::repository::jobs::tests`、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/jobs/tests.rs`、本文档。

结果（2026-10-12）：1,200 余行 jobs 单测从 `jobs.rs` 移入 `jobs/tests.rs`，以 `#[path]` 保持原测试名称空间及私有测试 helper 访问。迁移前后均为 14 passed、1 项 PostgreSQL 用例 ignored；fmt 和 `git diff --check` 通过。

#### LUX-491：拆分 STRM probe 与关机取消存储逻辑

范围：将 STRM probe job 的创建、领取、查询、进度、取消与终结操作移入 `jobs/strm_probe.rs`；将覆盖多类后台 job 的统一关机取消移入 `jobs/shutdown.rs`。关机取消事务与提交后 legacy scan event 顺序保持。

验收：

- [x] STRM probe persistence 与关机取消不再位于 `jobs.rs`。
- [x] `Database` 方法签名、取消 fencing、关机错误码、单事务更新及提交后事件写入顺序保持。
- [x] STRM、shutdown 与 jobs 单测通过，fmt/build 和 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；迁移前后运行 `cargo test --locked --test strm_probe --test strm_probe_api --test shutdown_jobs` 与 jobs 单测、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/jobs/strm_probe.rs`、`src/storage/jobs/shutdown.rs`、本文档。

结果（2026-10-12）：11 个 STRM job 方法移入 `jobs/strm_probe.rs`；原子关机取消方法和错误码移入 `jobs/shutdown.rs`。核对了 main shutdown、管理员列表与 application probe 的调用路径。jobs 单测 14 passed、1 项 PostgreSQL 用例 ignored；`shutdown_jobs` 1 项、`strm_probe` 2 项、`strm_probe_api` 1 项通过；build、fmt 与 `git diff --check` 通过。

#### LUX-492：拆分本地 metadata backfill 存储

范围：将 backfill page DTO、页大小限制与 root 注册、领取、完成、失败和中断恢复方法迁入 `jobs/local_metadata/backfill.rs`。`StoredScanLocalMetadataBackfillPage` 继续通过 repository 的现有 crate 内导出供 application 使用；本地 metadata item 排除序列化 helper 留在 local metadata 根模块供批次与 backfill 共用。

验收：

- [x] backfill 数据类型、限制与方法不再由 `jobs.rs` 实现。
- [x] repository 现有 DTO 导出路径、游标 fencing、页大小与重试/恢复状态保持。
- [x] backfill root 幂等和有界可恢复分页回归通过；fmt/build 与 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --lib storage::repository::tests::scan_local_metadata_backfill_roots_register_in_one_query`、`cargo test --locked --lib storage::repository::tests::progressive_scan_metadata_backfill_is_bounded_recoverable_and_root_scoped`、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/jobs/local_metadata.rs`、`src/storage/jobs/local_metadata/backfill.rs`、本文档。

结果（2026-10-12）：backfill DTO、page size、排除项校验 helper 和 8 个 root/分页/完成/失败/恢复方法移入 `jobs/local_metadata/backfill.rs`；本地 metadata 根复用排除项序列化，repository 继续通过 `jobs::StoredScanLocalMetadataBackfillPage` 导出原 DTO。backfill root 幂等与有界分页/重试/中断恢复各 1 项通过；build、fmt 与 `git diff --check` 通过。

#### LUX-493：拆分本地 metadata 批次与 outbox 存储

范围：将 local metadata batch 的创建、领取、完成、失败、取消、重试、分页和 Manifest outbox 写入移入 `jobs/local_metadata/batches.rs`。outbox transaction helper 仅对 repository 子树可见，Manifest discovery 继续传入并复用其原 transaction。

验收：

- [x] 本地 metadata batch/outbox 方法及批次限制常量不再位于 `jobs.rs`。
- [x] application、Manifest 和原 jobs 单测继续使用相同 `Database` 方法与事务入口。
- [x] outbox 有界批量和查询次数回归通过；fmt/build 与 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；迁移前后运行 `cargo test --locked --lib storage::repository::jobs::tests::manifest_local_metadata_outbox_inserts_three_batches_with_one_query`、jobs 其余单测、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/jobs/local_metadata.rs`、`src/storage/jobs/local_metadata/batches.rs`、本文档。

结果（2026-10-12）：15 个 local metadata batch/outbox 方法移入 `jobs/local_metadata/batches.rs`；写入批次限制归入 owning module，source 上限和错误长度限制留在 local metadata 根供其调用者共享。Manifest outbox transaction helper 限定 repository 子树可见，Manifest 调用仍复用原事务。jobs 单测 14 passed、1 项 PostgreSQL 用例 ignored；`scanned_metadata` 23 项、`scanned_series_metadata` 3 项、`scanning_jobs` 81 项全部通过；build、fmt 与 `git diff --check` 通过。

#### LUX-494：拆分本地 metadata 来源读取

范围：将扫描 metadata source 列表、当前来源 identity 检查及 SQL LIKE 转义 helper 移入 `jobs/local_metadata/sources.rs`。分页、来源身份批次大小和查询过滤保持。

验收：

- [x] source 查询与 identity 校验不再位于 `jobs.rs`。
- [x] scanner 与 metadata application 的 `Database` 调用签名及来源筛选结果保持。
- [x] 来源身份单测、metadata path 与扫描 metadata 回归通过；fmt/build 和 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --lib storage::repository::jobs::tests::local_metadata_completeness_reuses_current_source_identities_and_expands_episode_hierarchy`、`cargo test --locked --test metadata_paths --test scanned_metadata --test scanned_series_metadata`、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/jobs/local_metadata.rs`、`src/storage/jobs/local_metadata/sources.rs`、本文档。

结果（2026-10-12）：两项 local metadata source/identity 方法、来源身份批次上限与 LIKE 转义 helper 移入 `jobs/local_metadata/sources.rs`；原 `Database` 签名保持。因 LIKE 转义还用于 jobs 内另一个既有目录查询，由 local metadata 根按 repository 子树 re-export 同一 helper。核对了 scanner、metadata application 和 jobs tests 调用点。身份单测 1 项、`metadata_paths` 8 项、`scanned_metadata` 23 项、`scanned_series_metadata` 3 项通过；build、fmt 与 `git diff --check` 通过。

#### LUX-495：拆分 metadata reidentify 生命周期和 item 队列

范围：新增 `jobs/metadata_reidentify`，将 job 创建/查询/取消/重试/活动状态移入 `lifecycle.rs`，将目标查询、领取、item 完成与失败恢复移入 `items.rs`。现有 application、API 与 storage `Database` 调用保持。

验收：

- [x] metadata reidentify job 状态与 item 领取/结算实现不再位于 `jobs.rs`。
- [x] job 和 item 状态、取消竞态、retry/active 查询及 API 返回映射保持。
- [x] reidentify、metadata cancel 与 metadata API 回归通过；fmt/build 和 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；迁移前后运行 `cargo test --locked --test reidentify --test metadata_cancel --test metadata_api`、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/jobs/metadata_reidentify.rs`、`src/storage/jobs/metadata_reidentify/lifecycle.rs`、`src/storage/jobs/metadata_reidentify/items.rs`、本文档。

结果（2026-10-12）：15 个 reidentify job 创建/查询/状态方法移入 `lifecycle.rs`，8 个 item 查询、领取、完成及失败恢复方法移入 `items.rs`；优先级 SQL 片段放在新子树根，供 FILL_MISSING 后续复用。原 `Database` 签名保持。`metadata_api` 9 项、`metadata_cancel` 3 项、`reidentify` 20 项通过；build、fmt 与 `git diff --check` 通过。

#### LUX-496：拆分 FILL_MISSING 去重与请求合并

范围：将 FILL_MISSING job/item 去重、快照比较、队列合并与重试登记移入 `jobs/metadata_reidentify/fill_missing.rs`。把 `lock_media_items_for_update` 作为共享事务工具移入 repository。传入的 transaction、item 排序/锁定顺序、job 容量和请求快照规则保持。

验收：

- [x] FILL_MISSING 创建/合并实现及私有辅助状态类型不再位于 `jobs.rs`。
- [x] metadata 写入继续共享同一 sorted item lock helper，方法签名与 transaction 调用保持。
- [x] 去重、队列容量、请求快照、重试退避与 metadata 原子性回归通过；fmt/build 和 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 FILL_MISSING repository 单测、`cargo test --locked --test reidentify --test metadata_cancel`、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/jobs/metadata_reidentify.rs`、`src/storage/jobs/metadata_reidentify/fill_missing.rs`、`src/storage/repository.rs`、本文档。

结果（2026-10-12）：FILL_MISSING 请求快照、排队 job 合并、容量复用、item 插入与去重辅助状态移入 `metadata_reidentify/fill_missing.rs`。`lock_media_items_for_update` 移入 repository 事务工具，保留 ID 排序、去重、分批和 PostgreSQL `FOR UPDATE` 顺序；metadata application 与 fill_missing 继续使用同一方法。队列合并 2 项、请求快照退避 1 项、metadata 原子性 1 项、`reidentify` 20 项、`metadata_cancel` 3 项通过；build、fmt 与 `git diff --check` 通过。

#### LUX-497：拆分 scan job 生命周期

范围：将 scan job 创建、查找/列表、活动状态、领取/进度、取消、终结和 postprocessing 状态迁入 `jobs/scan/lifecycle.rs`。现有 application/API/repository 调用使用同一 `Database` 方法。

验收：

- [x] scan job lifecycle 实现不再位于 `jobs.rs`。
- [x] 状态筛选、取消 fencing、完成/后处理重试及管理端列表行为保持。
- [x] scanning/shutdown 集成测试、fmt/build 与 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --test scanning_jobs --test shutdown_jobs`、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/jobs/scan.rs`、`src/storage/jobs/scan/lifecycle.rs`、本文档。

结果（2026-10-12）：scan job 创建、自动匹配设置、查询/list/active、claim/progress、取消、完成、postprocessing 状态与重试等生命周期方法移入 `scan/lifecycle.rs`。外部 `Database` 方法签名及管理端/application 调用不变。`scanning_jobs` 81 项与 `shutdown_jobs` 1 项通过；build、fmt 与 `git diff --check` 通过。

#### LUX-498：拆分增量扫描路径队列

范围：将 incremental path 的单条/批量入队、计数刷新、待处理分页、已处理标记、item ID 读取及队列清理移入 `jobs/scan/incremental.rs`。保留路径去重、处理顺序与每批限制。

验收：

- [x] 增量路径队列与状态存储不再位于 `jobs.rs`。
- [x] `Database` 方法、队列容量/批次上限、删除及重试行为保持。
- [x] scanning/watch 与 incremental storage 回归通过；fmt/build 和 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --test scanning_jobs --test watch`、增量 storage 单测、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/jobs/scan.rs`、`src/storage/jobs/scan/incremental.rs`、本文档。

结果（2026-10-12）：7 个 incremental path 入队、分页、处理、ID 查询与清理方法及 100 路径批次上限移入 `scan/incremental.rs`。application/watch/storage 调用仍使用原 `Database` 方法。增量队列 storage 单测 1 项、`scanning_jobs` 81 项、`watch` 5 项通过；build、fmt 与 `git diff --check` 通过。

#### LUX-499：拆分 scan postprocessing target 存储

范围：将 scan target 和 sidecar target 登记、metadata/thumbnail 阶段分页、完成/失败/重试状态移入 `jobs/scan/targets.rs`。为 Manifest apply 与 reconciliation 共用的事务 helper 保持 repository 子树可见；侧车目录裁剪与 SQL 构造随目标模块迁移并保持现有查询边界。

验收：

- [x] scan job target 持久化和 worker page 查询不再位于 `jobs.rs`。
- [x] metadata/thumbnail 状态、removed target 优先级、sidecar 路径范围和事务调用保持。
- [x] query-count、sidecar 与扫描后处理回归通过；fmt/build 和 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 jobs 单测、`cargo test --locked --test scanning_jobs --test scanned_metadata`、相关 repository target/query-count 单测、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/jobs/scan.rs`、`src/storage/jobs/scan/targets.rs`、本文档。

结果（2026-10-12）：21 个 scan target 写入、分页、metadata/thumbnail 状态及查询方法和 sidecar helper 移入 `scan/targets.rs`；Application/API 的 `Database` 调用保持。target/outbox、reconciliation 与 Manifest apply 共用的事务 helper 暂限 repository 子树可见，后续调用方迁移后再收窄。jobs 单测 14 passed、1 项 PostgreSQL 用例 ignored；target 单查询与 sidecar 多目录查询各 1 项通过；`scanned_metadata` 串行 23 项、`scanning_jobs` 81 项通过；build、fmt 与 `git diff --check` 通过。两目标合并运行时 FIFO 海报用例超过 60 秒；单独运行 1 项和 `scanned_metadata -- --test-threads=1` 全部 23 项均通过。

#### LUX-500：拆分 reconciliation 存储

范围：将 reconciliation snapshot 读取、目录发现、文件批次事务、缺失确认、root page 完成与存在状态查询移入 `jobs/scan/reconciliation.rs`。共用 target/filesystem/media-source helper 沿用原 transaction；不改变根可用性 fencing 和重试 checkpoint。

验收：

- [x] reconciliation 存储方法不再位于 `jobs.rs`。
- [x] root 丢失保护、批次回滚、文件确认/缺失计数、checkpoint 与 target 登记仍在原事务中。
- [x] reconciliation 原子性单测和扫描集成回归通过；fmt/build 与 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --lib storage::repository::repository_tests::reconciliation_batch_commit_is_atomic_and_counts_confirmed_and_missing_entries`、`cargo test --locked --test scanning_jobs --test mixed_scanner --test watch`、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/jobs/scan.rs`、`src/storage/jobs/scan/reconciliation.rs`、本文档。

结果（2026-10-12）：15 个 reconciliation snapshot/discovery/批次提交/root page 方法移入 `scan/reconciliation.rs`。reconciliation 的 filesystem restore、media removal refresh 与 target 登记继续调用其原 transaction helper；外部 `Database` 方法名保持。reconciliation 原子性单测 1 项、`mixed_scanner` 1 项、`scanning_jobs` 81 项、`watch` 5 项通过；build、fmt 与 `git diff --check` 通过。

#### LUX-501：拆分 Manifest 创建与状态存储

范围：建立 `jobs/scan/manifest` 子树，将 Manifest 创建、读取、root 状态、diff-ready/apply/indexed 转换、结束与 retry fencing 移入 `manifest/state.rs`。原方法名、manifest workflow/state 值与 CAS 条件保持。

验收：

- [x] Manifest 创建/状态实现不再位于 `jobs.rs`。
- [x] workflow 2/3、root 状态、cancel fencing、retry 与 status transition 条件保持。
- [x] Manifest 创建原子性、恢复/重试扫描回归通过；fmt/build 和 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --lib storage::repository::jobs::tests::scan_manifest_creation_is_atomic_and_idempotent`、`cargo test --locked --test scanning_jobs`、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/jobs/scan.rs`、`src/storage/jobs/scan/manifest.rs`、`src/storage/jobs/scan/manifest/state.rs`、本文档。

结果（2026-10-12）：15 个 Manifest 创建、查询、root 状态、transition、finish 与 retry 方法，以及状态校验 helper 移入 `scan/manifest/state.rs`。原 Database 签名与 workflow 2/3、cancel/root fencing 保持。Manifest 原子创建单测 1 项和 `scanning_jobs` 81 项通过；build、fmt 与 `git diff --check` 通过。

#### LUX-502：拆分 Manifest 后处理目标分页

范围：将 Manifest postprocessing target page materialization、空页完成和事务内 target insert 移入 `scan/manifest/targets.rs`。保留 item target precedence、root identity checkpoint、cursor 和 target 写入的同事务边界。Manifest discovery 目录与 diff candidate 查询在其所属子模块迁移。

验收：

- [x] Manifest postprocessing target 物化与游标实现不再位于 `jobs.rs`。
- [x] 新旧 target 优先级、page limit、cursor CAS 和 root replacement fencing 保持。
- [x] target 回滚/续页与扫描集成回归通过；fmt/build 和 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --test scanning_jobs`、`cargo test --locked --lib storage::repository::jobs::tests`、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/jobs/scan/manifest.rs`、`src/storage/jobs/scan/manifest/targets.rs`、本文档。

结果（2026-10-12）：Manifest postprocessing target page、empty page 完成、事务内 target insert 及 page range DTO 移入 `scan/manifest/targets.rs`。cursor、root fencing、target precedence 和 transaction ownership 保持。jobs 单测 14 passed、1 项 PostgreSQL 用例 ignored；`scanning_jobs` 81 项通过；build、fmt 与 `git diff --check` 通过。

#### LUX-503：拆分 Manifest discovery 与正向索引

范围：将 discovery chunk 校验、frontier/observations、presence ledger、计数提交和正向媒体索引分别归入 `manifest/discovery.rs`、`manifest/indexes.rs`。两步继续共享本次 database transaction；本任务只移动步骤，不拆其内部大函数。

验收：

- [x] Manifest discovery 与正向索引实现不再位于 `jobs.rs`。
- [x] observations、filesystem/media indexes、local metadata outbox、presence ledger、manifest/job counters 仍在原事务按原顺序提交。
- [x] discovery 取消/去重、正向索引、outbox rollback 和 scanning integration 回归通过；fmt/build 和 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 jobs 单测、`cargo test --locked --test scanning_jobs`、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/jobs/scan/manifest.rs`、`src/storage/jobs/scan/manifest/discovery.rs`、`src/storage/jobs/scan/manifest/indexes.rs`、本文档。

结果（2026-10-12）：Manifest discovery commit/chunk 与正向媒体索引步骤移入 `manifest/discovery.rs`、`manifest/indexes.rs`；共享发现结果类型与 file identity helper 放在 Manifest 根。discovery、index、sidecar targets 与 local metadata outbox 继续复用同一 transaction，顺序及 progress/checkpoint 更新保持。jobs 单测 14 passed、1 项 PostgreSQL 用例 ignored；`scanning_jobs` 81 项通过；build、fmt 与 `git diff --check` 通过。

#### LUX-504：拆分 Manifest 差异计算和读取

范围：将 Manifest 目录/文件系统基线、removal candidates、差异状态计算与 delta 写入移入 `manifest/diff.rs`。差异顺序、root completeness 条件和 retryable state 保持。

验收：

- [x] Manifest diff/query 方法与差异批次常量不再位于 `jobs.rs`。
- [x] unchanged、ADD/CHANGE/REAPPEARED/REMOVE 分类、movie identity 检查和 root completeness 条件保持。
- [x] Manifest diff、root 离线/替换和扫描回归通过；fmt/build 和 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --test scanning_jobs`、jobs 单测、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/jobs/scan/manifest.rs`、`src/storage/jobs/scan/manifest/diff.rs`、本文档。

结果（2026-10-12）：10 个 Manifest directory/root baseline/removal candidate/diff/insert delta 方法与 diff batch limit 移入 `scan/manifest/diff.rs`。unchanged 与 ADD/CHANGE/REAPPEARED/REMOVE 判定、root completeness、movie variant identity 和原查询顺序保持。jobs 单测 14 passed、1 项 PostgreSQL 用例 ignored；`scanning_jobs` 81 项通过；build、fmt 与 `git diff --check` 通过。

#### LUX-505：拆分 Manifest delta apply

范围：将 pending delta 分页/删除计数与 delta batch apply 移入 `scan/manifest/apply.rs`，apply page limit 随方法归属迁移。job/manifest/root 检查、filesystem/media mutation、target 更新和进度仍在一个原 transaction 中。

验收：

- [x] Manifest apply 实现及分页上限不再位于 `jobs.rs`。
- [x] active job/cancel fencing、delta CAS、索引和 removal 原子提交、进度计数与返回结果保持。
- [x] delta rollback、并发增量冲突与批次上限回归通过；fmt/build 和 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --test scanning_jobs`、jobs 单测、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/jobs/scan/manifest.rs`、`src/storage/jobs/scan/manifest/apply.rs`、本文档。

结果（2026-10-12）：Manifest pending delta 读取、applied removal 计数和单批 delta apply 移入 `scan/manifest/apply.rs`；apply page limit 随模块迁移。active job/cancel fencing、delta CAS、filesystem/media mutations、target writes 与 progress checkpoint 仍在原 transaction。jobs 单测 14 passed、1 项 PostgreSQL 用例 ignored；`scanning_jobs` 81 项通过；build、fmt 与 `git diff --check` 通过。

#### LUX-506：抽离 Manifest discovery 输入校验

范围：把多 chunk 输入合并、跨 job/root 检查、路径/entry 校验、positive/unchanged/seen 路径唯一性和 baseline 校验移入 `manifest/discovery/input.rs`。结果带回已验证的借用数据和查找集合供原事务编排使用。

验收：

- [x] discovery coordinator 只接收已完成结构校验的输入，非法与空输入在打开事务前返回。
- [x] 错误类型/原因及检查顺序保持，输入顺序、去重规则和 collection 内容保持。
- [x] 既有 Manifest 输入/取消回归通过；fmt/build 和 `git diff --check` 通过。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 jobs 单测、`cargo test --locked --test scanning_jobs`、fmt、build 和 `git diff --check`。

预计文件：`src/storage/jobs.rs`、`src/storage/jobs/scan/manifest/discovery.rs`、`src/storage/jobs/scan/manifest/discovery/input.rs`、本文档。

结果（2026-10-12）：chunk 合并、path/entry 验证、positive/unchanged/seen 身份唯一性及 baseline 检查移入 `ManifestDiscoveryInput::collect`。invalid/empty 输入仍在开启 transaction 前返回，原错误原因和检查顺序保持。jobs 单测 14 passed、1 项 PostgreSQL 用例 ignored；`scanning_jobs` 81 项通过；build、fmt 与 `git diff --check` 通过。

#### LUX-507：提取 Manifest discovery 状态与 frontier 步骤

范围：把同一事务中的 Manifest/job state snapshot 与目录 frontier upsert 提取为具名 storage 步骤。主方法继续持有并传递原 `Transaction`，保留 stage diagnostics 的触发位置。

验收：

- [x] 单次状态读取仍返回同一 workflow/root/job/cancel snapshot。
- [x] 目录 frontier 的 chunk size、冲突处理、lite mode 跳过及插入计数语义保持。
- [x] frontier 取消/重试和 bounded directory batch 回归通过；fmt/build 与 `git diff --check` 通过。

预计文件：`src/storage/jobs/scan/manifest/discovery.rs`、`src/storage/jobs/scan/manifest/discovery/state.rs`、`src/storage/jobs/scan/manifest/discovery/frontier.rs`、本文档。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --lib storage::repository::jobs::tests::manifest_discovery_chunk_does_not_complete_directory_after_cancel_request`、`cargo test --locked --test scanning_jobs manifest_frontier_batches_handle_long_and_small_directories`、`cargo build --locked`、fmt 和 `git diff --check`。

结果（2026-10-12）：Manifest/job 状态读取改为单个 `ManifestDiscoveryStateSnapshot`，目录 frontier upsert 与完成更新分别迁入独立步骤；两者均复用 coordinator 持有的 transaction，SQL、bind 顺序、批次上限、冲突策略、lite mode 计数和诊断阶段位置保持。取消回归 1 passed，bounded directory batch 回归 1 passed；`cargo build --locked`、fmt 和 `git diff --check` 通过。`commit_scan_manifest_discovery_chunks` 及其应用层调用点未改名。

#### LUX-508：提取 Manifest root checkpoint 与 observation 写入

范围：将 root checkpoint 的目录/文件计数及 observation/local metadata sequence 预留，以及 observation 的分批写入迁入私有 discovery 子模块。继续由 coordinator 持有同一 transaction，并保留诊断计时边界。

验收：

- [x] workflow v2/v3 的 observation sequence 连续且与 root 高水位一致；legacy observation 的逐路径 sequence 语义保持。
- [x] local metadata batch sequence 预留与大目录 bounded batch 行为保持。
- [x] SQL/bind 顺序、批次上限、错误检查顺序和 diagnostics 计时边界保持；相关回归与 `git diff --check` 通过。

预计文件：`src/storage/jobs/scan/manifest/discovery.rs`、`src/storage/jobs/scan/manifest/discovery/checkpoint.rs`、本文档。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --test scanning_jobs full_scan_manifest_persists_discovery_and_reobservations_without_directory_work_queue`、`cargo test --locked --test scanning_jobs manifest_streams_large_directory_discovery_in_bounded_chunks`、fmt 和 `git diff --check`。

结果（2026-10-12）：root checkpoint 序号预留与 observation 批量写入移入 `discovery/checkpoint.rs`，原事务仍由 coordinator 创建、提交；SQL 和 bind 顺序、legacy/v2/v3 序号算法、批次大小和计时边界保持。目录计数转换仍在 frontier completion 前执行。observation 高水位回归 1 passed，大目录/local metadata sequence 回归 1 passed；fmt 与 `git diff --check` 通过。

#### LUX-509：提取 Manifest presence ledger 与 counters checkpoint

范围：将 format-3 generation presence CAS、seen-path ledger upsert 和 manifest/job counters checkpoint 拆为私有步骤。保持 filesystem row 的 ID/path/fingerprint CAS、Postgres/SQLite 更新路径、job cancel fencing 与同事务提交。

验收：

- [x] unchanged filesystem entry 仍按 generation 更新；不匹配的 ID/path/fingerprint 不计为 seen。
- [x] positive-index paths 与 seen generation paths 仍从 presence ledger 排除，其余路径按原批次 upsert。
- [x] manifest/job counters、取消 fencing、诊断计数与事务失败回滚语义保持；相关回归与 `git diff --check` 通过。

预计文件：`src/storage/jobs/scan/manifest/discovery.rs`、`src/storage/jobs/scan/manifest/discovery/presence.rs`、`src/storage/jobs/scan/manifest/discovery/counters.rs`、本文档。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --test scanning_jobs manifest_streams_large_directory_discovery_in_bounded_chunks`、`cargo test --locked --lib storage::repository::jobs::tests::manifest_discovery_chunk_does_not_complete_directory_after_cancel_request`、fmt 和 `git diff --check`。

结果（2026-10-12）：presence CAS/ledger 与 counters/cancel fence 分别迁入独立子模块；SQL、bind 顺序、backend 分支、failure rollback 和 stage diagnostics 统计保持。大目录两次扫描的 generation presence 回归 1 passed，取消回归 1 passed；fmt 与 `git diff --check` 通过。

#### LUX-510：收敛 Manifest discovery coordinator

范围：把 `commit_scan_manifest_discovery_chunks` 整理为短事务编排函数，已提取的阶段通过私有子步骤串接。保留同一 transaction 的创建/提交边界与现有 `Database` 调用面。

验收：

- [x] 事务协调函数不超过 150 行；transaction 仍只在 coordinator 开启和提交。
- [x] repository、application、API/main route 引用的 storage 方法名与签名不变。
- [x] 端到端 bounded scan、阶段 diagnostics 和 `git diff --check` 通过。

预计文件：`src/storage/jobs/scan/manifest/discovery.rs`、`src/storage/jobs/scan/manifest/discovery/coordinator.rs`、`src/storage/jobs/scan/manifest/discovery/paths.rs`、`src/storage/jobs/scan/manifest/discovery/indexing.rs`、本文档。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --test scanning_jobs manifest_streams_large_directory_discovery_in_bounded_chunks`、fmt、`git diff --check`，并搜索所有 `commit_scan_manifest_discovery_chunk(s)` repository/application 调用点。

结果（2026-10-12）：`commit_manifest_discovery_transaction` 为 149 行，transaction 仍由 `commit_scan_manifest_discovery_chunks` 单点创建和提交；路径查询、index/outbox 与 observation 子步骤独立。bounded full scan 回归 1 passed；格式和 diff 检查通过。搜索确认 application/scanner 与 storage tests 仍引用相同 `commit_scan_manifest_discovery_chunk(s)` 方法。

#### LUX-511：提取 Manifest apply guard 与 ADD preparation

范围：将 delta apply 的 active job/manifest/root 状态读取、ADD observation 验证、filesystem entry claim 与首次 movie/episode materialization 移入私有 preparation 步骤。保持校验顺序和共享 apply transaction。

验收：

- [x] active job、manifest state、root state 查询及冲突文案/次序保持。
- [x] ADD 只 claim 有完整 observation 且 root complete 的路径；未 claim 的媒体不 materialize。
- [x] requested bounded transaction batch 与 incremental ADD race 回归通过；storage 方法签名不变。

预计文件：`src/storage/jobs/scan/manifest/apply.rs`、`src/storage/jobs/scan/manifest/apply/prepare.rs`、本文档。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --test scanning_jobs manifest_add_conflicts_with_incremental_entry_claimed_after_diff`、`cargo test --locked --test scanning_jobs manifest_apply_uses_requested_bounded_transaction_batch`、fmt 和 `git diff --check`。

结果（2026-10-12）：apply preparation 提取事务内 active-state 检查、root 查询、四类路径映射、ADD observation 校验、filesystem claim 及首次 media insert；Delta 循环仍消费原 claim set。ADD race 1 passed，450 项 bounded apply 1 passed；fmt 与 diff 检查通过，`commit_scan_manifest_delta_batch` 对外方法名/签名保持。

#### LUX-512：提取 Manifest delta apply CAS 阶段

范围：将单条 ADD/CHANGE/REAPPEARED/REMOVE dispatch 与 filesystem CAS 操作从事务入口移入 `apply/deltas.rs`，返回 delta 状态和后续副作用所需路径集合。继续由原方法持有 transaction。

验收：

- [x] ADD claim、change/reappear baseline/fingerprint CAS、remove CAS 与冲突/unstable 分类保持。
- [x] delta 更新的输入顺序、计数和 batch 上限不变；helper 不另开/提交 transaction。
- [x] newer incremental write、bounded apply 和取消后恢复回归通过；storage 调用面不变。

预计文件：`src/storage/jobs/scan/manifest/apply.rs`、`src/storage/jobs/scan/manifest/apply/deltas.rs`、本文档。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --test scanning_jobs manifest_apply_does_not_overwrite_a_newer_incremental_filesystem_entry`、`cargo test --locked --test scanning_jobs manifest_apply_uses_requested_bounded_transaction_batch`、`cargo test --locked --test scanning_jobs cancelled_manifest_apply_resumes_pending_deltas_without_rediscovery`、fmt 和 `git diff --check`。

结果（2026-10-12）：delta dispatch/CAS 阶段移入事务内 helper，状态结果与 paths/update payload 显式返回给 coordinator；同一 batch transaction 继续负责后续 delta state、targets 与 job progress。incremental CAS、450 项 bounded apply、cancel/resume 三项均 1 passed；fmt 与 diff 检查通过，`commit_scan_manifest_delta_batch` 名称/签名不变。

#### LUX-513：提取 Manifest apply finalization

范围：将 delta state/error CASE 更新、metadata target/removal side effects、manifest/job progress checkpoint 移入 finalizer。transaction 仍由 `commit_scan_manifest_delta_batch` 创建并提交。

验收：

- [x] delta state/error CASE bind 顺序、affected-row guard、attempt count 与 batch 边界不变。
- [x] NEW/CHANGED/sidecar/removal targets、removed media refresh 与 applied/processed counters 保持。
- [x] cancel/resume 与 bounded apply 回归通过；只有入口持有 transaction 的 commit 权。

预计文件：`src/storage/jobs/scan/manifest/apply.rs`、`src/storage/jobs/scan/manifest/apply/finalize.rs`、本文档。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --test scanning_jobs manifest_apply_uses_requested_bounded_transaction_batch`、`cargo test --locked --test scanning_jobs cancelled_manifest_apply_resumes_pending_deltas_without_rediscovery`、fmt 和 `git diff --check`。

结果（2026-10-12）：delta persistence、target/removal side effects 与 progress checkpoint 移入 finalizer；CASE placeholders 和绑定顺序未改，取消检查仍在 job progress UPDATE，transaction commit 仍仅在入口。bounded apply 与 cancel/resume 均 1 passed；fmt 与 diff 检查通过。

#### LUX-514：拆分 Manifest diff completion 查询

范围：把 `finish_scan_manifest_diff` 中按 lite/v2/v3/legacy 分支统计 remaining changes，以及 legacy unchanged count 的查询提取到 `diff/finish.rs`。主方法保留 state transition、job progress/cancel fence 和 transaction commit。

验收：

- [x] 四种 workflow/version 路径及 SQL bind 顺序、remaining-change 条件不变。
- [x] 未完成变化时仍不推进 `APPLYING`；成功 completion 仍要求 `RUNNING AND cancel_requested = 0`。
- [x] legacy observation 与 v3/LITE 大目录回归通过；completion coordinator 不超过 150 行。

预计文件：`src/storage/jobs/scan/manifest/diff.rs`、`src/storage/jobs/scan/manifest/diff/finish.rs`、本文档。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --test scanning_jobs manifest_uses_latest_directory_observation_when_diffing_file_paths`、`cargo test --locked --test scanning_jobs manifest_streams_large_directory_discovery_in_bounded_chunks`、fmt 和 `git diff --check`。

结果（2026-10-12）：remaining-change 与 unchanged 统计查询移入 `diff/finish.rs`，`finish_scan_manifest_diff` 协调函数缩至 104 行；UPDATE 顺序、transaction guard、cancel fence 和 commit 位置保持。observation regression 与 v3/LITE 大目录两次扫描均 1 passed；fmt 与 diff 检查通过。

#### LUX-515：拆分 FILL_MISSING request reconciliation

范围：将 active/deferred request snapshot 查询、输入快照分类和 queued/deferred/job 更新分为状态读取、纯计划与持久化步骤。保留调用方 transaction 和现有存储方法签名。

验收：

- [x] Postgres row lock、100 项 query/update chunk、request fingerprint/capabilities 匹配语义不变。
- [x] 自动 retry/deferred suppression、obsolete snapshot consumption、queued completed-item reopen 与 processed count 下调保持。
- [x] 剩余请求仍经现有 100-item job coalescing/256-item insert 路径；retry 与 running request 回归通过。

预计文件：`src/storage/jobs/metadata_reidentify/fill_missing.rs`、`src/storage/jobs/metadata_reidentify/fill_missing/requests.rs`、本文档。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --lib storage::repository::repository_tests::automatic_fill_missing_provider_retry_backoff_is_capped_and_snapshot_scoped`、`cargo test --locked --lib storage::repository::repository_tests::changed_local_fill_request_during_running_item_is_not_lost`、fmt 和 `git diff --check`。

结果（2026-10-12）：request state load、request matching plan 和批量持久化分别成为私有步骤，`enqueue_or_update_fill_missing_requests_in_transaction` 收敛为短编排入口；锁子句、SQL/bind 顺序和 transaction ownership 保持。retry snapshot 与 running request 两项均 1 passed；fmt 与 diff 检查通过。

#### LUX-516：清理 jobs 根模块残留

范围：将 `restore_media_item` 移入媒体源 storage owner，将 legacy scan 重试错误码常量移入 shutdown owner。`jobs.rs` 只保留 jobs 模块 wiring、共享 imports/exports 与原测试入口。

验收：

- [x] `jobs.rs` 不再包含 `Database` 实现或业务常量，长度低于 200 行。
- [x] `restore_media_item`、legacy scan error code 和全部调用签名/引用保持。
- [x] shutdown 回归、crate build、fmt 与 `git diff --check` 通过。

预计文件：`src/storage/jobs.rs`、`src/storage/media_sources.rs`、`src/storage/jobs/shutdown.rs`、本文档。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --test shutdown_jobs`、`cargo build --locked`、fmt、`git diff --check`，并搜索 `restore_media_item` 与 legacy error code 的所有引用。

结果（2026-10-12）：`jobs.rs` 缩至模块 wiring 与必要共享 imports/exports；restore 方法迁入 `media_sources.rs`，legacy scan error code 迁入 `shutdown.rs`。调用方法名保持。shutdown、build、fmt 与 diff 检查通过。

#### LUX-517：收窄事务内存储 helper 可见范围

范围：把 local metadata outbox、LIKE escaping 和 scan target/removal transaction helper 限制在实际调用的 `jobs` 或 `jobs::scan` 子树。保持可供同层 repository tests 使用的 jobs 范围。

验收：

- [x] 不再有 jobs helper 使用整个 `storage::repository` 的事务内可见权限。
- [x] discovery/reconciliation/apply 与 jobs repository tests 的调用仍可解析。
- [x] build、jobs 单测、fmt 和 `git diff --check` 通过。

预计文件：`src/storage/jobs/local_metadata/sources.rs`、`src/storage/jobs/local_metadata/batches.rs`、`src/storage/jobs/scan/targets.rs`、本文档。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo build --locked`、`cargo test --locked --lib storage::repository::jobs::tests`、fmt 和 `git diff --check`，并搜索 repository 范围 helper 与所有调用点。

结果（2026-10-12）：LIKE escaping 和 Manifest outbox helper 缩至 `jobs`；scan target、sidecar、removal transaction helpers 缩至 `jobs::scan`。jobs repository tests 与 scanner/reconciliation/manifest 调用点仍在可见树内；build、jobs 单测、fmt 与 diff 检查通过。

#### LUX-518：缩短 reconciliation batch 事务协调函数

范围：将 pending path 过滤、movie/episode 索引、NEW/CHANGED/sidecar target 写入和 seen filesystem entry 恢复迁入事务内 batch helper。外层 coordinator 继续持有唯一 transaction。

验收：

- [x] reconciliation `Database` 公共方法名/签名不变；事务仍由 batch coordinator 开始并提交。
- [x] 文件索引、target 和 restore 更新保持原顺序，均复用调用方 transaction。
- [x] mixed reconciliation、失败回滚重试回归通过；事务协调函数不超过 150 行。

预计文件：`src/storage/jobs/scan/reconciliation.rs`、`src/storage/jobs/scan/reconciliation/batch.rs`、本文档。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --test scanning_jobs mixed_reconciliation_batches_known_media_and_keeps_unresolved`、`cargo test --locked --test scanning_jobs reconciliation_batch_rolls_back_index_and_retries_all_pending_files`、fmt 和 `git diff --check`。

结果（2026-10-12）：媒体索引/targets/restore 阶段移入 `reconciliation/batch.rs`；`commit_reconciliation_batch_in_transaction` 缩至 110 行，仍是唯一 transaction owner。mixed batch 与失败回滚重试回归各 1 passed；fmt 与 diff 检查通过。

#### LUX-519：拆分 Manifest 创建与 retry 生命周期事务

范围：将 full-scan Manifest root 初始化、legacy retry target 迁移和 DISCOVERING retry root reset 提取为事务内步骤。创建/重试 coordinator 继续独占 transaction begin/commit。

验收：

- [x] `create_full_scan_manifest_job` 与 `prepare_scan_manifest_retry` 方法名/签名及 validation/error 顺序保持。
- [x] root registration、legacy target copy、lite/persisted retry SQL 与 transaction 顺序保持。
- [x] 创建幂等与 DISCOVERING frontier resume 回归通过；事务协调函数不超过 150 行。

预计文件：`src/storage/jobs/scan/manifest/state.rs`、`src/storage/jobs/scan/manifest/state/transactions.rs`、本文档。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --test scanning_jobs full_scan_manifest_indexes_safe_positive_batches_during_discovery`、`cargo test --locked --test scanning_jobs shutdown_cancelled_manifest_resumes_the_persisted_discovery_frontier`、fmt 和 `git diff --check`。

结果（2026-10-12）：Manifest full-scan root registration/legacy target copy 与 DISCOVERING retry reset 移入 state transaction 子模块；外层方法保留 transaction owner。full scan/LITE 与 persisted frontier resume 回归各 1 passed；fmt 与 diff 检查通过。

#### LUX-520：拆分 Manifest delta 幂等批次写入

范围：将一组 delta 的 baseline conflict 检查、bounded row insert 和 kind 计数提取为 transaction 内 helper。外层 `insert_scan_manifest_deltas` 保留 payload 校验、每个 transaction batch 的 READY_TO_DIFF 锁、counter 更新和提交。

验收：

- [x] root/path 唯一性、delta payload 校验、retry baseline equality 与错误语义不变。
- [x] inner insert chunk、outer transaction batch 和 ADD/CHANGE/REMOVE/REAPPEARED 计数/绑定顺序不变。
- [x] 同输入幂等重试、冲突 baseline rollback 和 malformed payload 回归通过；外层 coordinator 不超过 150 行。

预计文件：`src/storage/jobs/scan/manifest/diff.rs`、`src/storage/jobs/scan/manifest/diff/insert.rs`、本文档。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --lib storage::repository::jobs::tests::scan_manifest_creation_is_atomic_and_idempotent`、fmt 和 `git diff --check`。

结果（2026-10-12）：delta row verification/insert 与分类计数迁入单批 helper；外层方法缩至 101 行并继续逐 outer batch 独立 commit。幂等 retry、baseline conflict、malformed ADD 回归通过；fmt 与 diff 检查通过。

#### LUX-521：Stage25 最终集成与调用合同审计

范围：运行项目完整检查门，并对 jobs 根长度、事务 coordinator 长度、storage `Database` 方法签名和 application/API/main 调用面做最终审计；把 Manifest discovery transaction coordinator 的内部 lifetime 声明收敛到不超过 150 行。

验收：

- [x] `jobs.rs` 小于 200 行；Stage25 事务 coordinators 不超过 150 行，或在结果中说明 SQL 阶段例外。
- [x] 基线中的 storage 方法名/签名无丢失或变化；`src/api`、`src/application`、`src/main.rs` 没有因本阶段产生的路由/调用改动。
- [x] `./scripts/check-all.sh` 全部通过，`git diff --check` 通过，本任务未修改原工作区。

预计文件：`src/storage/jobs/scan/manifest/discovery/coordinator.rs`、`web/tests/home-page.test.tsx`、本文档。

验证：先确认 Toshiba 挂载，然后以 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target` 运行 `./scripts/check-all.sh`；补充方法签名/调用点 diff、`uname -m` 和 git 状态检查。

结果（2026-10-12）：`jobs.rs` 28 行；`commit_manifest_discovery_transaction` 完整声明块 150 行（函数体 146 行）。此前收敛的 reconciliation、Manifest diff、delta insert、create/retry coordinator 也均不超过 150 行。基线 `b08bdcf4` 的 186 个 storage `Database` 方法在当前模块均存在且签名一致；`src/api`、`src/application`、`src/main.rs` 相对基线无改动；本机 `arm64`。首次最终门禁中的 Rust build、840 个单元测试（20 个按配置忽略）、全部 Rust integration、fmt、all-targets/all-features Clippy、shell 检查、两个 Python 检查和冻结锁文件安装通过；Web gate 在 Vitest `home-page.test.tsx` 的 5 个异步 carousel 断言失败（75/76 文件、576/581 tests）。该文件单独运行 17/17 通过，Web 与基线无差异，且此前一次完整 Web suite 通过 581/581、Web build 通过；首次门禁时第三项验收暂未勾选，后续修复和复验见补记。`git diff --check` 通过。

补记（2026-10-12）：统一将 `home-page.test.tsx` 的异步 UI 等待窗口设为 3 秒，并将轮播点击测试从固定 450ms 睡眠改为等待目标 dot 的 `aria-current` 状态，避免并行测试负载下默认 1 秒等待不足。定向 HomePage 测试 17/17 通过；随后 `./scripts/check-all.sh` 全部通过，包括 840 个 Rust 单元测试、全部 Rust integration、fmt、all-targets/all-features Clippy、shell 与 Python 检查、冻结锁文件安装、Web Node 136/136、Vitest 581/581 和 Web production build。本任务未修改原工作区文件；该工作区的 `test` HEAD 在本轮执行期间从 `bc9fd6ee` 前进到 `dcda7345`，现存未提交和未跟踪文件仍保留。

#### LUX-522：确保阻塞 metadata 测试总能释放 FIFO

范围：修正 scanned-metadata integration tests 的阻塞 NFO/poster FIFO 清理边界。先保存 NFO 解锁前的观测结果，再无论查询成功或失败都释放 FIFO；poster fixture 通过 guard 在提前返回时写入并移除 FIFO，避免 Tokio runtime shutdown 等待阻塞 reader。

验收：

- [x] snapshot 查询失败时也执行 FIFO release，Tokio runtime shutdown 不再遗留阻塞 reader。
- [x] scan 已在 NFO 解锁前结束时不重复 poll JoinHandle；未结束时仍在释放后等待 scan。
- [x] poster 阻塞与取消用例在正常和提前退出路径都释放 FIFO，失败断言不会卡住测试进程。
- [x] 独立阻塞 NFO 回归通过，原有 poster-before-NFO 与 scan-before-NFO 断言保留。

预计文件：`tests/scanned_metadata.rs`、本文档。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR`；运行 `cargo test --locked --test scanned_metadata manifest_scan_indexes_poster_while_local_nfo_is_blocked`、fmt 和 `git diff --check`。

结果（2026-10-12）：snapshot queries 改为先收集结果，随后总是释放 FIFO，再传播查询错误；通过 timeout 结果避免对已完成 JoinHandle 二次 poll。poster 阻塞/取消测试共用释放 guard，在 `Drop` 中打开读写 FIFO、写入 fixture 并移除路径。独立 NFO 用例通过；完整 `scanned_metadata` 23 passed、`scanning_jobs` 81 passed，fmt 与 diff 检查通过。

#### LUX-523：移动端首页媒体库单行滚动

范围：首页“我的媒体库”在桌面端继续使用现有自适应网格；移动端改为单行横向滚动，并复用“继续观看”的左右滚动按钮。

验收：

- [x] 桌面端网格布局和卡片尺寸保持不变。
- [x] 移动端媒体库卡片保持在同一行，可通过左右按钮分段滚动，也保留触控横向滚动。
- [x] 仅一个媒体库或滚动到边界时，不显示不可用方向的按钮。

预计文件：`web/src/react.css`、`web/tests/home-media-layout.test.mjs`、本文档。

验证：运行定向布局测试、完整 Web 测试和 Web 构建；执行 `git diff --check`。

结果（2026-10-13）：移动端媒体库沿用 `HorizontalScrollRail` 的左右按钮并保持 235px 卡片单行滚动；桌面自适应网格不变。定向布局测试 6/6、Web Node 测试 137/137、Vitest 583/583、Web 构建和 `git diff --check` 通过。

#### LUX-524：缩小移动端首页媒体库封面

范围：移动端首页“我的媒体库”沿用单行横向滚动，把媒体库卡片宽度缩小到最多 170px；桌面卡片尺寸和网格保持不变。

验收：

- [x] 移动端媒体库卡片宽度最多 170px，窄屏时随视口继续缩小。
- [x] 桌面媒体库卡片继续使用 235px 宽度和现有自适应网格。
- [x] 移动端继续使用现有左右滚动按钮与触控横向滚动。

预计文件：`web/src/react.css`、`web/tests/home-media-layout.test.mjs`、本文档。

验证：运行定向布局测试、完整 Web 测试和 Web 构建；执行 `git diff --check`。

结果（2026-10-13）：移动端卡片设置为 `min(170px, 52vw)`，窄屏更窄、桌面仍为 235px。定向布局测试 6/6，完整 Web 测试和构建通过。

#### LUX-525：移除旧 Web 源码目录 fallback

范围：Rust 静态服务默认只提供 `web/dist`，并继续支持显式 `LUX_WEB_DIR`；不再因构建产物缺失而把 `web/src` 当作旧版站点提供。将仍被历史测试导入的旧 JavaScript 实现隔离到 `web/tests/fixtures/legacy-web/`，更新测试引用；清理旧 HTML、CSS 与空入口文件。

验收：

- [x] 默认静态根目录固定为 `web/dist`；未设置 `LUX_WEB_DIR` 时不再读取或服务 `web/src`。
- [x] 旧 JavaScript 仅从测试 fixture 被显式导入，React 生产入口不依赖它。
- [x] 旧 HTML、CSS 和空 `index.mjs` 不再留在应用源码目录；浏览器标题检查只覆盖当前入口。

预计文件：`src/api/legacy.rs`、`web/src/app.mjs`、`web/src/request-options.mjs`、`web/src/admin-navigation.mjs`、`web/src/index.html`、`web/src/styles.css`、`web/src/index.mjs`、`web/tests/home-legacy.test.mjs`、`web/tests/setup-page.test.tsx`、`web/tests/request-options.test.mjs`、`web/tests/admin-navigation.test.mjs`、`web/tests/browser-title.test.mjs`、本文档。

验证：Toshiba 挂载检查后使用共享 `CARGO_TARGET_DIR` 运行 `cargo build --locked`；运行 Web 单测和构建；执行 `git diff --check`。

进度（2026-10-13）：静态根目录与 fixture 清理已完成。`cargo build --locked`、`cargo fmt --all -- --check`、all-targets/all-features Clippy、TypeScript 检查、Vite production build 和 `git diff --check` 通过。Web 自动化测试未运行；`pnpm build` 在尝试清理依赖目录时因无交互终端中止，随后直接运行现有 `tsc` 与 Vite 可执行文件完成构建检查。Web 测试门仍待执行。

### 阶段 26：管理员 API 领域化与 application 编排

#### LUX-526：拆分管理员接口并下沉业务编排

范围：以现有管理员 API 为合同基线，按用户、媒体库、媒体条目、元数据、人物、插件、Emby
迁移、扫描/后台任务、计划任务、设置、Webhook、日志/事件、健康、仪表盘和数据库诊断拆分
handler、DTO、映射及路由。`admin.rs` 保留显式领域路由组合和稳定 re-export，其他 Lux、用户、
媒体与 Emby 调用方继续从稳定 facade 引用共享 API helper。

管理员 handler 中的多服务业务编排、跨表/跨任务查询聚合、缓存失效、插件同步及后台 worker 启动
迁入所属 `application` 用例；单次调用现有 application service 的 handler 可保留为协议适配。
application 不接收 Axum 类型或 `AppState`，不直接生成 HTTP 响应。保留 SSE 协议循环在 API 层。

保持全部现有 HTTP 路径、方法、请求/响应 JSON、状态码、错误码、鉴权/CSRF 条件、请求大小限制、
查询上限、顺序和副作用。尤其保持计划任务旧接口、FILL_MISSING dispatcher、媒体库写入后的缓存及
插件同步、审计位置、健康探测缓存 TTL/并发行为、Emby 迁移 actor 要求与事件流格式。
不修改 schema、数据库关系、共享部署边界或依赖。

验收：

- [x] 管理员路由由领域子模块持有，并经 `admin.rs` 的显式稳定 re-export 组合；110 个基线路径及其方法（包括 Axum GET 隐含的 HEAD）均保留。
- [x] `admin_handlers.rs` 删除；API handler 不再直接编排多服务用例、数据库查询聚合或启动领域 worker。SSE writer/heartbeat 留在 API 协议适配。
- [x] application 用例使用领域输入/结果与显式依赖，不接收 HTTP 类型或 `AppState`；所有原调用方的 helper/DTO 兼容路径仍可编译。
- [x] 序列化、鉴权、错误响应、部分成功、审计、事件发布、任务调度、缓存失效与资源限制保持原行为。
- [x] 领域回归、全部 Rust 集成测试、完整项目检查通过；API 和 ADR 模块边界文档已更新。

计划文件：`src/api/admin.rs`、`src/api/admin_handlers.rs` 及按上述领域新建的 `src/api/admin/` 子模块；
`src/api/legacy.rs`、`users.rs`、`media.rs`、`playback.rs`、`emby_handlers.rs`、`emby_catalog.rs`、
`routes.rs` 中实际受 facade 引用影响的 helper；新增或扩展领域 `src/application/` 用例与导出；
路由合同和既有管理员领域 integration tests；本文档与 `docs/decisions/030-domain-module-boundaries.md`。

验证：每个增量先补/更新相关回归，再运行显式 Rust 集成 target 或 library test、`cargo build --locked`、
fmt 与 `git diff --check`，独立提交。完成时运行 `./scripts/check-all.sh`，以及 all-targets Rust、
Clippy、Web 测试和构建质量门；Cargo 命令使用 `/Volumes/Toshiba/mywork/Lux/target`。记录本机架构，
不从 ARM 检查推断 NAS/x86 性能。

实施结果（2026-10-13）：删除 9,956 行管理员 handler 单体，按用户、媒体库、媒体条目、元数据、人物、
插件、Emby 迁移、后台任务、计划任务、设置、Webhook、日志/事件、健康、仪表盘和数据库诊断拆成领域
子模块；`admin.rs` 显式组合子路由并重导出兼容 helper/DTO。跨服务编排、查询聚合、缓存失效、插件同步
和领域 worker 启动迁入 `application::admin_*`。与 `origin/test` 基线对照，110 个路径、128 个显式方法
绑定及包含隐式 HEAD 的 179 个路径/方法组合均一致。

验证结果：相关定向测试 30/30 通过；`./scripts/check-all.sh` 全部通过，其中 Rust 111 个测试目标共
1,536 passed、0 failed、42 ignored（library 840 passed、20 ignored），fmt、all-features Clippy、shell
语法、Python 3/3 与 2/2、冻结 lockfile 安装、Node 137/137、Vitest 583/583 及 Web production build
均通过。本机架构为 `arm64`，未据此推断 NAS/x86 性能。此前两次全量重跑分别遇到未修改区域的 NFO actor
credit 间歇失败和 watch SQLite 锁定；各自单独重跑通过，最终 all-targets 与 check-all 完整运行均通过。

### 阶段 27：自动刮削排队、吞吐基准与库级进度

本阶段先修正 workflow 3 自动补缺持久化后仍等待容量 32 dispatcher channel 的合同冲突，再测量并调整在线刮削并发，最后为管理员提供只读的库级入库进度。自动来源与管理员/计划任务显式来源必须在持久化状态中区分；自动 worker 不等待内存队列容量，显式入口保留已有背压、完成 receiver、取消与每库串行语义。不得使用无界队列或按 job 无界 spawn。

每个任务一次只处理一个 LUX 编号，先写失败回归，再做最小实现并定向验证。该阶段不改扫描索引、Manifest 删除/完成语义或 `ScanCompleted`/`JOB_COMPLETED`/Webhook/Emby 合同，不批量提交刮削写库，不放宽阶段 23 的 5% 性能门。所有本机性能记录标明 ARM64 和 SQLite/PostgreSQL 配置，不外推 FNOS/NAS/x86。

#### LUX-527：修订自动 FILL_MISSING dispatcher 合同

范围：记录自动本地补缺与管理员/计划任务显式提交的分离合同。持久化 `QUEUED` 自动 job 是自动调度来源；每库 dispatcher 通过有界唤醒与分页领取自动工作。自动本地 worker 不等待队列容量或 provider 完成；显式入口仍保留当前容量背压与完成等待。旧 job 升级后不推断来源。扫描任务状态、索引完成事件和 webhook 时序保持不变。

验收：

- [x] ADR-046、任务规格与阶段 23 阻塞记录明确同一调度策略如何同时满足 LUX-303 和 LUX-424。
- [x] 决策明确禁止无界内存队列、每 job task fan-out、自动工作并入显式 job，以及改变扫描完成语义。
- [x] 本决策增量只修改文档；阶段 23 性能门维持开放，后续 LUX-530 实现包含超过 32 个 job 的阻塞 provider 排空回归。

预计文件：`docs/LUX-DEVELOPMENT.md`、`docs/decisions/046-progressive-scan-and-missing-metadata.md`、`docs/PERFORMANCE.md`、`docs/STAGE-23-GATE-REVIEW-2026-10-09.md`。

验证：核对 LUX-303/LUX-424 原合同、现有失败快照与旧 job 升级边界；运行 `git diff --check`。本任务仅记录决策，不声称运行时修复或阶段门通过。

实施结果（2026-10-14）：ADR-046 明确自动补缺以持久化 QUEUED job 为工作队列，合并唤醒和有界分页领取满足 LUX-303 的本地 worker 不等待要求；容量 32 的显式 dispatcher 背压与完成等待仍满足 LUX-424，自动和显式来源不混合。LUX-528–530 随后完成来源字段、持久队列 drain 与超过 32 个自动 job 的回归；保留原 10k 失败快照，阶段 23 双后端 5% 性能门仍开放。

#### LUX-528：持久化补缺 job 来源字段

范围：追加 SQLite/PostgreSQL migration，为 FILL_MISSING job 保存内部调度来源。新旧/显式 job 默认显式来源，自动来源仅由后续任务设置；升级不得推断已有 job 的历史来源，公开 DTO/API 不增加来源字段。

验收：

- [x] SQLite migration 可从空库运行且能从当前 schema 升级；PostgreSQL migration 与其语义一致，已发布 migration checksum 不变。
- [x] 既有 job 获得显式来源默认值，来源字段有必要索引且不出现在 API DTO。
- [x] SQLite 空库与旧数据升级回归通过；PostgreSQL bootstrap/旧 schema regression 验证默认值和 partial index，并在本机服务可用时运行 ignored target（本机无服务，执行情况见结果）。

预计文件：`migrations/0169_metadata_reidentify_dispatch_source.sql`、`migrations-postgres/0169_metadata_reidentify_dispatch_source.sql`、`tests/storage.rs`、`tests/postgres_database.rs`、本文档。

验证：`tests/storage.rs` SQLite migration targets；本机 PostgreSQL 可用时运行 `tests/postgres_database.rs` bootstrap migration target；`cargo fmt --all -- --check`、`git diff --check`。Cargo 前确认 Toshiba 挂载并使用共享 `CARGO_TARGET_DIR`。

实施结果（2026-10-14）：追加 SQLite/PostgreSQL 0169 migration，`dispatch_source` 默认 `EXPLICIT` 并限制为 `EXPLICIT/AUTOMATIC`；部分索引只收录未取消的自动 FILL_MISSING QUEUED job。SQLite 旧 schema 记录保留原状态并读到显式默认值，空库能建立字段和索引；已发布 migration 未改。`cargo test --locked --test storage` 53/53 通过，格式检查与 diff 检查通过。PostgreSQL bootstrap 回归已补默认值和索引断言，但本机没有 Docker daemon 或 PostgreSQL server，`--include-ignored` 运行因 `PoolTimedOut` 未能验证数据库执行；该服务端运行限制在阶段 27 最终记录中保留。

#### LUX-529：自动与显式补缺事务分流

范围：本地 completeness 事务创建的 job 设置自动来源，只合并到同库、同来源自动 job；显式创建与管理员重试设置显式来源。保留活跃 item 去重、单 job 100 项边界、状态/计数和事务原子性。

验收：

- [x] 自动完整性事务不会扩展显式 job；显式 job 不会被自动合并逻辑重分类。
- [x] 显式 create/retry 保持显式来源；自动 job 去重/合并、item 状态、总数与回滚语义正确。
- [x] 覆盖两个来源的 SQLite 存储合同和 PostgreSQL 事务合同；来源字段不出现在 API DTO。

预计文件：`src/storage/jobs/metadata_reidentify/fill_missing.rs`、`src/storage/jobs/metadata_reidentify/fill_missing/requests.rs`、`src/storage/jobs/metadata_reidentify/lifecycle.rs`、`src/storage/repository_tests.rs`、`tests/admin_health.rs`、`tests/danmaku.rs`、`tests/scanning_jobs.rs`、`tests/scanner.rs`、`tests/ready_version.rs`、本文档。上述 health、danmaku、scan 与 version 测试同步 0169 migration 后的 schema version。

验证：存储 repository library tests、PostgreSQL FILL_MISSING transaction contract、fmt 与 diff 检查。Cargo 前确认 Toshiba 挂载并使用共享 target。

实施结果（2026-10-14）：自动补缺只更新/合并自动来源 job；显式 queued/running/deferred item 会阻止自动计划改写请求快照或消耗 provider retry，管理员 retry 将 job 转为显式来源。SQLite 来源隔离和扫描 dispatch 回归通过；PostgreSQL 合同回归已加入但执行因本机 `PoolTimedOut` 未能连接。`cargo build --locked`、all-features Clippy、fmt 与 diff 检查通过。`cargo test --all-targets` 被现有 HLS generation 用例的 `Superseded` unwrap 阻断；跳过该用例后，catalog 集成用例复现 HTTP 502，另一个 catalog 集成用例超过 60 秒未结束。0169 schema 版本相关集成断言已统一更新为 169。

#### LUX-530：自动补缺使用有界唤醒与持久队列 drain

范围：每库 dispatcher 保留单 runner 与显式 job 的有界 channel；自动来源通过合并唤醒通知触发持久队列 drain。dispatcher 只分页拉取并领取自动来源 `QUEUED` job，按创建顺序串行运行；自动提交即使存在 32 个以上待处理 job 也立即返回，不丢唤醒、不重复执行。显式提交仍等待既有 channel 容量并获得 completion receiver。

验收：

- [x] provider 被 gate 阻塞且自动队列超过 32 个 job 时，本地完整性 worker 与 scan 可完成；放行 provider 后自动 job 全部按每库单 runner 排空。
- [x] 显式提交在 channel 满时仍呈现原背压/completion receiver 语义；自动工作不提前领取显式 job。
- [x] 覆盖唤醒合并/竞态、自动 job 合并、取消、dispatcher shutdown、worker panic 恢复及重启后的 QUEUED job drain；不产生无界内存或 task fan-out。
- [x] SQLite 与 PostgreSQL 均有存储领取合同测试；保留 `ScanCompleted`/`JOB_COMPLETED` 与扫描进度时序。

预计文件：`src/application/reidentify.rs`、`src/storage/jobs/metadata_reidentify/lifecycle.rs`、`src/storage/repository_tests.rs`、本文档。

验证：先运行自动 dispatch/慢 provider regression 和 `tests/reidentify.rs`、扫描 metadata targets；阶段实现完成时运行 Rust 全目标与 all-features Clippy。Cargo 前确认 Toshiba 挂载并使用共享 `CARGO_TARGET_DIR`。

实施结果（2026-10-14）：自动来源改走每库容量为 1 的合并 mpsc 唤醒通道，显式来源继续使用容量 32 的背压队列；dispatcher 每次最多读取 100 条持久 QUEUED 自动 job，按 created_at/id 顺序串行处理，满页或失败后继续 drain/retry。自动 completion waiter 上限为每库 32。回归覆盖 provider gate 下 108 条及并发新增 job 的有序排空、显式队列已满时自动提交立即返回、显式与自动唤醒同时就绪、未 settled job 的 completion 保留、取消、panic、shutdown、重启恢复，以及本地 metadata worker 不阻塞 scan。

定向验证：`application::reidentify::tests::` 39/39，扫描 dispatch 回归 1/1，SQLite 来源/分页合同 1/1，串行 `tests/reidentify.rs` 21/21；`cargo build --locked`、fmt 与 all-features Clippy 通过。最终 `cargo test --locked --all-targets` 的单元部分 850 passed、21 ignored；integration 执行到 `libraries_api` 时有一项在默认并行下返回 `DATABASE_UNAVAILABLE`，该项单独通过且 `libraries_api -- --test-threads=1` 13/13。PostgreSQL 合同测试已编译但未执行：本机 Docker daemon 不可用、没有 PostgreSQL 监听端口或 `pg_isready`。`web_static` 依赖 Web 构建产物，在生成本地 `web/dist` 后通过；生成产物未纳入变更。

#### LUX-531：增加固定延迟的刮削并发基准

范围：为现有 metadata pipeline benchmark 增加可控的 stub provider 延迟和并发矩阵，至少测 4/8/12 个 worker、固定 item 数与固定每次 provider 调用延迟，记录 items/s、p50/p95 单项耗时、job 总耗时与配置。benchmark 使用真实 job worker 和当前 SQLite/PostgreSQL storage 路径；网络请求保持 stub，不访问 TMDb。

验收：

- [x] 同一 fixture/延迟/数据库参数可重复运行，测量结果包含有效性检查、版本/架构与逐项原始数据；SQLite 连跑 3 次并记录完整结果范围，PostgreSQL 参数路径已实现。
- [x] 并发 sweep 每档独立设置，记录峰值与 queue wait；SQLite 各档峰值精确达到 4/8/12 且不超限；成功吞吐过滤失败/取消项。
- [x] `scripts/run-metadata-performance.sh` 支持 `concurrency`/`legacy` 选择；性能记录明确区分固定延迟 stub 与真实上游时延。
- [x] 本任务仅增加测量和 feature-gated benchmark seam；生产默认并发仍为 4，没有宣称真实 TMDb 速率。

预计文件：`tests/performance.rs`、`scripts/run-metadata-performance.sh`、`docs/PERFORMANCE.md`、本文档。为使 8/12 路档位走真实 FILL_MISSING worker 而不提前开放运行时配置，允许增加仅由 benchmark Cargo feature 暴露的并发注入 seam（`Cargo.toml`、`src/application/reidentify.rs`）；该 seam 不进入默认构建。

验证：Toshiba 挂载检查后以共享 target 运行指定 ignored performance target；另运行 fmt 和 diff 检查。禁止将本机 ARM64 结果外推至 NAS/x86。

实施结果（2026-10-14）：新增 `performance-benchmarks` feature 仅供 ignored benchmark 注入 4/8/12 worker 和收集真实 `item_total` 样本，默认构建不包含 seam。新基准每档使用 96 项、每次 stub provider 调用固定 100ms、SQLite 真实 storage 与真实 FILL_MISSING job worker；脚本保留 `legacy` LUX-200 入口，并允许通过 `LUX_PERF_BACKEND=postgres` 选择 PostgreSQL。SQLite 连续 3 轮各档均 96/96 成功，0 失败/取消，192 次 provider 调用；4/8/12 路 items/s 中位数为 16.45/29.57/35.75，成功项 p95 中位数为 255.6/323.5/378.1ms，完整范围见 `docs/PERFORMANCE.md`。默认值未改。测试 target、legacy target、默认 build、fmt、all-features Clippy 与 diff 检查通过。PostgreSQL benchmark 本机未运行：未发现 `postgres`、`psql`、`pg_isready` 或可用服务；其连接配置路径已实现但后端耗时仍待实测。本机为 ARM64，结果不外推 NAS/x86。

#### LUX-532：可配置且有界的 FILL_MISSING 并发

范围：依据 LUX-531 的固定延迟矩阵为在线 FILL_MISSING 增加 `LUX_METADATA_CONCURRENCY` 启动期配置与绝对上限；默认值和上限写入性能/部署说明。保留全局 semaphore、每库串行 dispatcher、resource metrics 降档、取消响应和 DB 后端资源差异。无效/超范围值采用安全 clamp 并发出不含 secret 的配置告警。决策：LUX-531 只完成 SQLite 矩阵，PostgreSQL 尚无可用服务；因此默认仍为 4，不基于单一后端把默认提高到 8/12，绝对上限为 12，等待双后端实测后再决定是否提高默认。

验收：

- [x] env 未设置、有效边界、0、负数/非整数、超过上限均有解析回归；configured limit 在进程内由 OnceLock 初始化一次，并保存在 service 实例，避免 semaphore 与 worker limit 不一致。
- [x] 默认 4、绝对上限 12；PostgreSQL 未实测时保持默认。resource metrics 继续接收 configured limit，现有压力测试覆盖降档规则，worker loop 每轮重新读取并可恢复。
- [x] 两个并行 library job 共用 global semaphore；集成测试在 unset 默认 4 与 env=1 下通过；其它 metadata 模式仍遵循原后端建议值和进程级限制。
- [x] 更新性能/兼容文档并注明 PostgreSQL 基准待测；没有宣称未经上游验证的 TMDb 请求速率。

预计文件：`src/application/reidentify.rs`、`tests/reidentify.rs`、`docs/PERFORMANCE.md`、`docs/COMPATIBILITY.md`、本文档。

验证：Rust parser/worker 测试、`tests/reidentify.rs` 与相关 metadata 集成 target、build/fmt/all-features Clippy；Toshiba 挂载检查与共享 target 必须满足。

实施结果（2026-10-14）：`LUX_METADATA_CONCURRENCY` 采用缺省 4、支持 1–12、超过范围 clamp 到边界，非整数回退缺省；对非法/越界值发 warning，但不记录原始 env 内容。configured limit 在进程内 OnceLock 一次读取，并保存在每个 MetadataReidentifyService 实例；同一个 OnceLock 同时创建全局 FILL_MISSING semaphore，配置不会动态重读。资源 pressure 路径、每库 dispatcher 和取消不变。定向 reidentify 单测 42/42，通过；`LUX_METADATA_CONCURRENCY=1` 与 env unset 两种环境下，跨两个库的进程级 FILL_MISSING worker 集成各通过 1/1。默认 SQLite build、fmt、all-features Clippy 与 diff 检查通过。默认值依据 SQLite 4 路基线保持 4；PostgreSQL 没有可用服务，未测也未提高默认值。部署配置见 `docs/COMPATIBILITY.md`，性能依据见 `docs/PERFORMANCE.md`。

#### LUX-533：存储层聚合库级入库阶段

范围：只读聚合启用媒体库的扫描索引、Manifest/local metadata、probe、thumbnail、自动 FILL_MISSING 持久化状态，返回按库分页的内部快照和最近活动时间。只读现有表，不新建通用 pipeline 表，不改 worker 执行路径、单项状态或扫描完成语义。SQL 先限制到最多 50 个启用库，再在数据库内分组计数；不把 item 行载入应用层，不读取路径、媒体标题、来源凭据或上游错误文本。阶段运行状态只能依据已持久化的 job/target 状态与 scan job 当前阶段推导，缺少对应记录时返回无计划/空闲，不合成进度。各 stage 计数只在本 stage 内解释，底层分别按 scan job、local metadata batch/backfill/legacy target、probe target/STRM probe job、thumbnail target、自动 metadata job 计数，不跨 stage 求和，也不据此估算 ETA。扫描驱动的 NFO/probe/thumbnail 若没有逐项 RUNNING 状态，用 scan job 当前阶段计一个运行中的驱动任务；未完成的 target 仍留在 queued。

验收：

- [x] storage 返回每库各阶段的 queued/running/completed/failed/cancelled 计数、是否有计划和最近活动时间；索引阶段与后台阶段独立，在线 FILL_MISSING 不计入扫描索引。
- [x] 页大小拒绝或限制到 50；聚合只访问所选启用库，有界 SQL 完成计数，不读取 item 全表到应用层，也不触发 provider/filesystem。
- [x] SQLite fixture 覆盖无任务、Manifest 后处理、local metadata batches/backfill、自动刮削、混合阶段、失败/取消与库移除；PostgreSQL 合同测试使用代表性五阶段 fixture 执行共享聚合 SQL。
- [x] 类型只包含库标识/显示名、阶段状态计数和时间；不含媒体标题、路径、凭据、错误文本或 scan activity 的 current item。

预计文件（5 个）：`src/storage/repository.rs`、新建 `src/storage/pipeline_activity.rs`、`src/storage/mod.rs`、`src/storage/repository_tests.rs`、本文档。

验证：storage repository 定向回归与 PostgreSQL 合同；`cargo build --locked`、fmt、all-features Clippy 和 diff 检查。Cargo 前确认 Toshiba 挂载并使用共享 target。

实施结果（2026-10-14）：存储层以单条分页聚合查询返回五个阶段的状态计数和最近活动时间，只把启用库当前页（最多 50 个）的聚合结果带回应用；状态计数保留阶段本身的持久化单位，不合并成整体百分比。SQLite 定向回归 2/2 通过，覆盖分页、禁用/移除库、扫描后处理、metadata batch/backfill、probe/STRM、thumbnail、自动/显式 FILL_MISSING 与失败/取消；测试确认不回传媒体标题。build、all-target/all-features Clippy、fmt 与 diff 检查通过。增加了运行 PostgreSQL 共享查询的忽略测试并编译通过；尝试启动一次性 PostgreSQL 16 容器，但当前网络访问 Docker Hub 失败（registry 请求返回 EOF），因此该数据库运行证据仍待有 PostgreSQL 服务的环境补齐。

#### LUX-534：提供只读库级入库阶段 API

范围：增加管理员读取接口 `GET /api/v1/admin/library-pipeline-activity?page=1&pageSize=20`，将 LUX-533 storage 快照序列化为 `libraries` 与分页元数据。每页最多 50 个启用库；`pageSize` 缺省 20，页码从 1 开始。每库返回 `libraryId`、`libraryName`、各 stage 的 `planned`、五类状态计数及 `recentActivityAt`。只增加 Lux 管理 API，不改已有 task activity、Emby、ScanCompleted、Webhook DTO 或任务执行路径。

验收：

- [x] admin 鉴权、分页边界、空页和错误映射符合现有 API 约定，响应只包含 storage 阶段契约允许的字段。
- [x] 集成测试覆盖两个库并行、各阶段混合状态、取消/失败、禁用或移除库、无任务和超过 50 库分页；不会序列化媒体标题、路径、凭据或错误内容。
- [x] compatibility 文档记录 endpoint、分页和字段语义；复用 LUX-533 的共享 PostgreSQL storage query contract，不做第二次全表读取。

预计文件（5 个）：`src/application/admin_task_activity.rs`、`src/api/admin/admin_scheduled_tasks.rs`、`tests/libraries_api.rs`、`docs/COMPATIBILITY.md`、本文档。

验证：`tests/libraries_api.rs` 定向目标、build/fmt/all-features Clippy 与 diff 检查；Toshiba 挂载检查并使用共享 target。

实施结果（2026-10-14）：新增只读管理员 endpoint，默认页大小 20、最大 50、页码从 1 开始；沿用管理员鉴权，分页非法返回 400，数据库/服务不可用返回 503。应用层把 storage snapshot 映射到固定五阶段 API shape，不增加第二次读取。`tests/libraries_api.rs` 14/14 通过，包含未授权、51 库分页、空页、无计划阶段、混合 queued/running/completed/failed/cancelled、禁用/移除库排除和敏感字段不回传；build、all-features Clippy、fmt 与 diff 检查通过。API 字段和 Unix epoch 秒时间语义已记入 `docs/COMPATIBILITY.md`。PostgreSQL live contract 仍受 LUX-533 中记录的 Docker Hub 网络限制影响。

#### LUX-535：管理员活动视图展示库级入库阶段

范围：在现有管理员活动弹层中以媒体库为组展示 LUX-534 API 结果，清楚区分索引和后台 NFO、probe、thumbnail、刮削进度；支持无计划阶段、长任务与失败项。复用现有活动入口，不改变桌面/移动 shell 的触发方式。

验收：

- [x] 多库并行时各库独立显示阶段与计数；扫描索引完成后仍显示未结束的后台阶段。
- [x] 无任务、纯后台处理、部分失败、完成、分页/刷新与 API 错误均有 Web 回归。
- [x] 视图不把阶段总计伪装成精确 ETA；ARIA 状态与进度值准确，长库名和窄视口不造成横向溢出。

预计文件（5 个）：`web/src/features/activity/ScanActivityPopover.tsx`、`web/src/lib/api/client.ts`、`web/src/lib/api/types.ts`、`web/tests/scan-activity.test.tsx`、本文档。

验证：定向 Web 测试、完整 Web 测试与 production build。阶段 27 全部任务完成后，运行该阶段门、更新性能/兼容记录并停在项目所有者确认点；阶段 23 原有 5% A/B 性能门仍按其规格单独开放。

实施结果（2026-10-14）：管理员活动弹层按库显示五个阶段的计划状态、队列/运行/完成/失败/取消计数和最近活动时间；索引结束后仍保留后续后台阶段。弹层打开时每 5 秒刷新，支持手动刷新、每页 5 个媒体库、阶段展开、无计划/无任务状态和不暴露原始错误的重试。没有合成百分比或 ETA。`tests/scan-activity.test.tsx` 覆盖并行媒体库、纯后台处理、无计划、部分失败、全阶段完成、分页/刷新和 API 错误。浏览器用 mock API 在 375×812 验证长库名和阶段展开；视口和文档宽度均为 375px，弹层内容纵向滚动。

验证结果：目标测试 10/10、production build、定向浏览器检查和 `git diff --check` 通过。完整 `pnpm test` 的 Node 检查 137/137 通过；默认并行 Vitest 全量运行中，`tests/home-page.test.tsx` 的 5 个轮播用例失败，其余 583/588 通过。单 worker Vitest 全量运行中，另有 `tests/library-page.test.ts` 的 1 个排序恢复断言失败，其余 587/588 通过；这两个失败文件分别单独运行均为 17/17 通过，显示全量运行结果不稳定。阶段 23 的 A/B 性能门仍开放；LUX-533 PostgreSQL live query 受 Docker Hub EOF 阻塞，尚未取得 PostgreSQL runtime 证据。

产品调整（2026-10-15）：项目所有者决定从管理员活动弹层移除库级入库阶段状态列表。右上角活动入口仅在存在活动单项作业时显示；库级阶段状态不再作为活动入口的显示条件。LUX-533/LUX-534 的只读聚合与 API 保留，不改任务执行路径。

阶段 27 阶段门（2026-10-14）：在 Toshiba 挂载、`arm64` 环境下，通过 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target` 运行 `scripts/check-all.sh`。Cargo build、fmt、all-target/all-feature Clippy、shell 语法、两组 Python 工具测试均通过；`cargo test --locked --all-targets` 在 lib target 停止，854 通过、1 失败、22 ignored。失败 `application::playback::hls::tests::emby_init_and_segment_share_the_requested_generation` 在隔离重跑时仍因 `Superseded` 失败，位于本阶段未修改的 HLS 播放代码。Web frozen-lockfile install 和 production build 通过；完整 Web 测试波动详见 LUX-535 结果。PostgreSQL live query 未验证，阶段 23 的 5% A/B 门仍开放。本阶段按门停在项目所有者确认点，不将失败检查记为通过。

#### LUX-536：在媒体详情展示剧照

范围：电影详情与剧级电视剧详情读取现有条目图片中的 `FANART`、`THUMB`、`WALLPAPER`，作为“剧照”横向图片栏，放在“演职人员”之后、“更多信息”之前。季、集及其他条目类型不展示剧照；海报、LOGO 等其他图片类型不进入剧照栏。无符合类型的图片时隐藏该栏。

验收：

- [x] 电影与剧级电视剧详情展示该条目全部 FANART、THUMB、WALLPAPER 图片，按图片类型和索引稳定排序。
- [x] 季、集、普通视频及其他条目不展示剧照；其他图片类型不会混入；无匹配图片时不渲染空栏。
- [x] 桌面和移动布局中，“剧照”都位于演职人员之后、更多信息之前；图片有可访问的替代文本，并使用延迟加载。
- [x] 详情页回归覆盖支持的条目类型、过滤规则、空数据和分区顺序；Web 测试与生产构建通过。

预计文件（5 个）：`web/src/features/detail/MediaDetailPage.tsx`、`web/src/features/detail/MediaStills.tsx`、`web/src/react.css`、`web/tests/media-detail.test.tsx`、本文档。

验证：Toshiba 挂载后使用共享 `CARGO_TARGET_DIR` 运行 `cargo build --locked`、fmt 与 all-targets/all-features Clippy；Web 运行冻结锁文件安装、`web/tests/media-detail.test.tsx` 定向测试、完整 Web 测试和 production build；执行 `git diff --check`。本任务未修改 Rust 源码或 Rust API 合同，没有相关 Rust 行为测试。

实施结果（2026-10-14）：详情页复用现有 item-images 查询，不增加后端接口。电影和剧级电视剧通过“剧照”横向栏展示全部 FANART、THUMB、WALLPAPER 图，先按类型再按 imageIndex 排序；海报、LOGO 等类型被排除。季、集、VIDEO 和其他条目不渲染剧照栏；无符合类型的图片时隐藏空栏。桌面与移动布局均将剧照排在演职人员和更多信息之间，图片带描述性 alt 并延迟加载。

验证结果：冻结锁文件安装通过；`media-detail.test.tsx` 32/32、`detail-layout.test.mjs` 29/29、完整 Web Node 137/137 与 Vitest 593/593、production build 通过。Toshiba 已挂载，本机 `arm64`；共享 target 下 `cargo build --locked`、`cargo fmt --all -- --check`、all-targets/all-features Clippy 通过。`git diff --check` 通过。未运行 Rust 行为测试，因为本任务不改 Rust 代码或合同。

详情页视觉补充验收（2026-10-14）：

- [x] “更多信息”区域不使用外围面板，直接沿用详情页背景；内部评分及单项信息卡继续保留。
- [x] 类型行与五项信息卡之间、信息卡与制作来源之间不显示分割线；信息卡上下留白采用紧凑间距。
- [x] Tagline 字号增加 2px 并保留原衬线字体；官方网站链接上方不显示短分割线。
- [x] 浅色模式下评分和合集标题正确挂接深色文字样式，底层浅色渐变与信息面板透明度进一步降低，能看见背景图内容。
- [x] 浅色模式下媒体文件摘要框和视频、音频、字幕轨道卡片采用与“更多信息”一致的透明度和背景模糊。
- [x] 媒体摘要、折叠标题和展开后的地址共用一个外框；“媒体源地址与技术详情”作为无框按钮显示。
- [x] 展开时直接展示媒体源地址，并将复制按钮放在地址行最右侧；移除“媒体播放地址 (已安全收纳)”标签，折叠仍由原按钮控制。

补充实现与回归文件（5 个）：`web/src/features/detail/MediaInfoPanel.tsx`、`web/src/react.css`、`web/tests/media-detail.test.tsx`、`web/tests/detail-layout.test.mjs`、`web/tests/light-theme-contrast.test.ts`。本文档作为独立文档增量更新。

补充验证：冻结锁文件安装通过；媒体详情组件测试 32/32、详情布局检查 31/31、浅色主题测试 19/19；完整 Web 测试 Node 139/139、Vitest 593/593、production build 和 `git diff --check` 通过。浏览器本地预览确认剧照、半透明“更多信息”面板及媒体地址展开/收起和复制按钮位置。

后续样式更新（2026-10-15，经项目所有者确认）：上述 2026-10-14 浏览器预览记录的是首版半透明面板；当前验收以无外围面板、紧凑分区间距、无指定分割线及放大后的原字体 tagline 为准。

更新实现与验证（2026-10-15）：`MediaNfoPanel.tsx` 移除外围卡片并收紧分区留白；`react.css` 移除对应边框样式、将 tagline 调至 `1rem`，并去掉官方网站上方短线。新增的渲染回归和布局检查分别为 33/33、31/31；冻结锁文件安装和 Web production build 通过，`git diff --check` 通过。Web 全量检查仍有不相关失败：静态 Node 检查 137/139（首页卡片尺寸、触屏操作规则 2 项）；完整 Vitest 596/600（首页继续观看评分、首页媒体库焦点预取、媒体库卡片预取与合集链接 4 项）。

布局更新（2026-10-18）：宽屏媒体详情页将 tagline 放到主视觉右上方的留白区域；窄屏继续沿用详情内容流中的位置。

#### LUX-537：对齐 Emby 鉴权凭据优先级与重复 query 行为

范围：根据官方 Emby 4.10.1.0 原生 ARM64 临时实例实测，在 Emby 路由入口统一选择凭据，避免不同令牌 query 名称映射到同一 Serde 字段而返回 400。标准优先级为 HTTP `X-Emby-Token`、query `X-Emby-Token`、authorization 的 `Token`、HTTP `X-MediaBrowser-Token`、query `api_key`；非空无效高优先级凭据不回退。Lux 已有扩展仅在标准来源均为空时回退。保留现有 principal、管理员权限、媒体库 ACL 和签名播放票据规则；不改变原生 `/api/v1`。

验收：

- [x] 不同 query 来源同时存在时按优先级选择，与顺序及值是否相同无关；不再因 Serde alias 冲突返回 400。
- [x] 高优先级无效返回 401；普通用户 token 与管理员 API key 同时存在时不得提升权限。
- [x] 被选中的同名 query 出现多次时返回 401；更高优先级有效来源覆盖低优先级重复 query。
- [x] 根路径与 `/emby`、详情/列表/图片/PlaybackInfo/媒体入口使用相同提取规则；非鉴权参数保留原编码、顺序与既有重复参数语义。
- [x] Lux query `apiKey`/`X-MediaBrowser-Token` 与历史扩展 header 继续作为低优先级回退，原生 API 不变。
- [x] 自动化回归与兼容文档记录完成；不声称 NanShare/Hills 部署后的实机链路已验证。
- [ ] Rust 全量完成门通过；本次两轮全量失败与隔离重跑结果如实保留。

代码分三个增量：统一鉴权入口（`src/api/emby.rs`、`src/api/emby_credentials.rs`、`src/api/emby_handlers.rs`、`tests/emby_auth.rs`、`tests/strm.rs`）；播放 URL 选中凭据（`src/api/playback.rs`、`tests/strm.rs`）；公开发现/密码登录边界（`src/api/emby.rs`、`tests/emby_auth.rs`）。规格与兼容记录作为独立文档增量。

公开发现、Ping、公开用户与密码登录不消费标准 token，保留未使用 query token 的原行为。authorization 同时存在但前者不含 Token 的边界没有实测 Emby；Lux 保留查找后者非空 Token 的回退，明确不声称该边界已完成对照。签名播放票据保留独立授权路径。

实施结果（2026-10-14）：统一入口在受保护 Emby 路由内部按实测优先级选出凭据，原 query 不再把鉴权别名交给 DTO；保留非鉴权参数原编码和重复 MediaSourceId 的首值语义。内部凭据 header 防外部伪造，登出使用同一提取结果。公开发现/Ping/用户与密码登录保留原行为。PlaybackInfo 按选中的用户 token 生成代理播放 URL，低优先级 Lux key header 不再抑制它，共享管理员 key 仍不进入 URL。代码增量为 `c79e21ef`、`d53e8088`、`95e0d014`。

验证：先写失败回归，11 个相关 target 27/27、新解析单元测试 3/3 通过；收尾 7 个 target 17/17 通过。build、fmt、all-target/all-feature Clippy 和 diff 检查通过。本机 ARM64，Cargo target 保留 Toshiba。首轮 all-targets 1451 通过、1 失败、44 ignored，在 shutdown 因已有服务占用 UDP 7359 而启动失败；测试临时使用 `LUX_DISCOVERY_BIND_ADDR=127.0.0.1:0` 后 shutdown 通过，不改默认端口、不停止现有服务。第二轮全量在 lib 停止，858 通过、1 失败、22 ignored：此前已记录的 HLS generation 用例遇到 `Superseded`；该用例第一轮通过、第二轮失败、随后单跑通过。HLS 文件未修改，全量门仍未通过。详细边界见 `docs/COMPATIBILITY.md` 的 LUX-537。未推送、未部署，实际 NanShare/Hills 链路尚未复测。

#### LUX-538：首页移动端媒体卡片尺寸与菜单显隐

范围：微调首页“继续观看”和每库最新资源的移动端封面尺寸；继续观看不显示评分；缩小“我的媒体库”封面上的三点按钮并将其锚定在封面右下角。媒体卡片和媒体库的三点按钮在桌面端只于对应封面悬停或键盘聚焦时显示；触控设备沿手指移动经过的封面显示对应按钮，并保留该按钮供后续点击。

验收：

- [ ] 移动端“我的媒体库”封面最多 180px；继续观看封面最多 190px，略大一档；桌面尺寸保持 235px 和 280px。
- [ ] 移动端首页纵向最新资源卡片最多 160px；桌面卡片尺寸不变。
- [ ] 继续观看卡片不显示评分，播放进度、集数和播放入口不变。
- [ ] 媒体库菜单按钮视觉直径不超过 30px、保留至少 44px 触控区域，并位于封面右下角。
- [ ] 媒体卡片与媒体库菜单按钮默认隐藏；桌面封面悬停/键盘聚焦以及触控手指移动经过封面时按对应设备规则显示。
- [ ] 定向布局回归、Web 测试和 Web 构建通过。

预计文件：`web/src/app/App.tsx`、`web/src/features/home/media.tsx`、`web/src/react.css`、`web/tests/home-media-layout.test.mjs`、本文档。

#### LUX-538：为入库后的旁车变化建立独立文件队列

范围：增加扫描任务级、以 root 和相对文件路径为键的旁车变化队列，记录 ADD、CHANGE、REMOVE 与处理结果。该队列只表达某个文件待同步，不复用 `media_sources.probe_status`、条目 metadata 状态或缩略图状态。相同任务和路径的新观察合并为最新变化并重新排队；队列项按路径稳定分页，可持久恢复。

验收：

- [x] SQLite 空库 migration 建立文件变化队列表、状态约束、外键和分页索引；PostgreSQL 使用相同 migration 合同。
- [x] storage 可登记、稳定分页、领取、完成/失败/跳过和重排文件变化项；同一路径仅保留一项，运行期间出现新观察会递增 revision 并重新排队，旧 revision 不能完成新事件。
- [x] 操作文件变化队列不改动媒体源 probe 状态、媒体项 metadata 状态、既有 `scan_job_targets` 或其缩略图状态。
- [x] SQLite 回归覆盖 ADD/CHANGE/REMOVE、合并、分页、运行中更新、旧 revision 完成保护、中断重排和任务删除级联；PostgreSQL repository 合同覆盖写入及领取/完成语义。

预计文件（5 个）：`migrations/0174_scan_job_file_changes.sql`、`src/storage/jobs/scan/targets.rs`、`src/storage/repository.rs`、`src/storage/repository_tests.rs`、本文档。

验证：storage library 定向测试与 PostgreSQL 合同测试；Toshiba 挂载后使用共享 target 运行 build、fmt、all-features Clippy 与 diff 检查。Cargo 命令设置 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target`。

#### LUX-539：增量扫描发现外挂字幕和媒体信息文件

范围：将外挂字幕与 `*-mediainfo.json` 纳入增量文件发现，按具体路径登记 ADD、CHANGE、REMOVE。仅文件状态或 fingerprint 确实变化时排队；未知 JSON、未变化文件不排队。这是 LUX-538 文件队列的增量发现切片。现有 NFO/图片/XML/EDL 处理暂沿原消费者路径，后续 LUX-543 会将有定义的消费者迁到按文件处理并移除目录级重复调度。

验收：

- [x] watcher、定向增量扫描和目录批次都能登记字幕支持格式及 `*-mediainfo.json` 的新增、修改、删除；普通 JSON 不进入队列。
- [x] 新附件只追加文件事件，不改媒体源 probe、媒体项 identification/metadata 或 thumbnail 状态；相同 fingerprint 不产生新工作。
- [x] 新文件首次出现在旧媒体库时会进入队列，带有路径及 root 身份，处理路径仍受库根约束。

预计代码文件（5 个）：`src/application/scanner/library.rs`、`src/application/scanner/jobs/incremental.rs`、`src/storage/filesystem.rs`、`src/storage/repository.rs`、`tests/scanning_jobs.rs`。本文档单独记录范围。

验证：`tests/scanning_jobs.rs` 定向目标、build、fmt、all-features Clippy 与 diff 检查。

#### LUX-540：Manifest 全量扫描登记精确文件变化

范围：让 Manifest 全量扫描发现新增或变化的外挂字幕与 `*-mediainfo.json`，并与 Manifest 正向索引/delta 在同一事务中登记精确文件事件。新建修复后第一次全量扫描时，即使旧数据库没有这些附件的 filesystem entry，也能发现并入队。确认删除及持久 reconciliation 由 LUX-541 单独接入。

验收：

- [x] Manifest 对新增和变化的字幕、媒体信息文件按精确路径及 root 入队，change kind 与增量扫描一致；未变化路径不入队。
- [x] 文件状态与队列事件在同一 Manifest 正向索引或 delta 事务提交；root 不可用、观测冲突或取消时不产生越过 fencing 的事件。
- [x] 字幕和媒体信息事件不生成目录级 metadata/thumbnail target，也不改变未变化视频的探测、轨道或条目 metadata 状态。
- [x] 对已有媒体库加字幕或媒体信息文件后只需全量扫描即可补发现，不重建媒体库，也不重探测未变化视频。

预计代码文件（5 个）：`src/application/scanner/manifest.rs`、`src/application/scanner/jobs/manifest_discovery.rs`、`src/storage/jobs/scan/manifest/indexes.rs`、`src/storage/jobs/scan/manifest/apply/finalize.rs`、`tests/scanning_jobs.rs`。本文档单独记录范围。

验证：Manifest 定向扫描作业回归、build、fmt、all-features Clippy 与 diff 检查。

#### LUX-541：Manifest 确认删除登记精确文件变化

范围：让 Manifest 的确认删除为外挂字幕与 `*-mediainfo.json` 登记精确 REMOVE 事件，并在文件状态更新与队列提交时使用同一事务。Reconciliation 由 LUX-542 单独接入。

验收：

- [x] Manifest 只在 root 可用且缺失 fencing 确认成功后按精确路径及 root 登记 REMOVE；未确认缺失、不可用 root 不登记事件。
- [x] Manifest delta 状态与 REMOVE 队列事件在同一事务提交，取消、重试和恢复不会丢失已提交事件。
- [x] 被删除附件不会触发同目录其他媒体的 metadata/thumbnail target，也不改未变化视频状态。

预计代码文件（3 个）：`src/application/scanner/jobs/manifest_index.rs`、`src/storage/jobs/scan/manifest/apply/finalize.rs`、`tests/scanning_jobs.rs`。本文档单独记录范围。

验证：Manifest removal 定向扫描作业回归、build、fmt、all-features Clippy 与 diff 检查。

#### LUX-542：持久 reconciliation 登记精确文件变化

范围：让持久 reconciliation 发现外挂字幕与 `*-mediainfo.json`，并为新增、变化和确认删除登记精确文件事件。应用层将 ADD/CHANGE 类型随批次传入 storage；事件与 filesystem 状态在同一事务提交。删除事件只有在 root 可用且二次缺失确认成功后登记。

验收：

- [x] reconciliation 对新增、变化及确认删除的字幕和媒体信息文件按精确路径及 root 入队；未变化路径不入队。
- [x] 取消、重试和恢复不丢失已提交事件；不可用 root 或无法确认的缺失不登记 REMOVE。
- [x] 使用与增量扫描和 Manifest 相同的分类、change kind 语义，且不重新探测未变化视频。

预计代码文件（5 个）：`src/application/scanner/jobs/reconciliation.rs`、`src/storage/repository.rs`、`src/storage/jobs/scan/reconciliation.rs`、`src/storage/repository_tests.rs`、`tests/scanning_jobs.rs`。本文档单独记录范围。

验证：持久 reconciliation 定向扫描作业回归、storage repository 批次合同测试、build、fmt、all-features Clippy 与 diff 检查。

#### LUX-543：按文件变化同步依赖数据

范围：消费旁车文件队列并按类型更新依赖数据。外挂字幕只增删/更新对应外部轨道；媒体信息 JSON 只更新实际依赖它的 STRM/来源信息；NFO、图片、danmaku XML 只更新各自结果。视频本身未变时保留内嵌轨道和技术信息。失败保留上次有效结果，处理成功后完成精确文件队列项。

验收：

- [x] 新增/修改/删除字幕轨道的回归证明不会调用 ffprobe、不重写内嵌轨道、不改媒体源探测或 metadata 状态；其它外部轨道 ID 与 stream index 稳定。
- [x] 媒体信息 JSON、NFO、图片、XML/danmaku 变化只触发有定义的消费者；未变化输入不重新解析/索引。没有消费者的文件类型不得制造 metadata/thumbnail 任务。
- [x] 同目录多媒体、文件改名、相同路径内容变化、重复扫描、无效文件、文件删除和扫描取消/恢复都有覆盖；旧库可由全量扫描修复。
- [ ] worker 使用有界批处理和输入版本检查，旧任务不能覆盖更新文件结果；数据库变化兼容 SQLite 与 PostgreSQL。

预计文件：实施前按 LUX-538 至 LUX-542 的最终 worker/存储边界拆成不超过 5 个文件的可提交增量。

验证：字幕、扫描、媒体信息、NFO/图片与 danmaku 对应的最窄集成目标；完整 Rust 完成检查、兼容性记录和 diff 检查。不得以定向测试替代规格要求的完成门。

实施结果（2026-10-15）：新增独立的 scan-job 文件变化队列，增量扫描、Manifest 和持久 reconciliation 对精确路径登记 ADD/CHANGE/REMOVE；处理器按文件类型同步外挂字幕、STRM 媒体信息、NFO、图片和 danmaku XML，EDL 等无消费者文件以 no-op 完成。队列不改媒体项 metadata、媒体源 probe 或 thumbnail 状态；视频 fingerprint 未变时保留内嵌轨道和技术信息。PostgreSQL 0170 migration 与 revision/恢复合同测试已补齐；LUX-538 至 LUX-542 的队列和发现验收已覆盖。PostgreSQL 服务不可用，因此 LUX-543 最后一项留待运行时合同验证。

验证结果：`tests/scanning_jobs.rs` 87/87、`tests/storage.rs` 54/54、`tests/ready_version.rs` 2/2、migration 双后端合同测试 1/1、build、fmt 与 all-target/all-feature Clippy 通过。最终全量 `cargo test --locked --all-targets` 的 lib target 为 855 passed、2 failed、23 ignored：未修改的 `scan_local_nfo_page_flushes_actor_credits_and_isolates_invalid_nfo` 与 HLS `emby_init_and_segment_share_the_requested_generation` 失败；NFO 用例随后隔离重跑 1/1 通过，HLS 用例隔离运行有通过和 `Superseded` 失败两种结果。因 lib target 失败，这次 all-target 命令未运行后续 integration targets；相关扫描、storage 和 ready/version integration targets 已分别通过。PostgreSQL runtime 未验证；本机 ARM64/SQLite 验证不代表 PostgreSQL runtime 结果。

## 28. 自动电影合集阶段

目标：提供系统自动维护的“合集”虚拟媒体库。一个系列跨多个普通媒体库归并为一个合集；同一影片的不同库副本在合集内只展示一次，普通电影库仍逐条展示。仅根据已确认的来源 ID 自动归组，不以相似标题猜测关系。用户可见部数、作品、图片和来源都受其现有媒体库权限限制。Web 与 Emby 客户端均提供合集浏览、跨库版本选择和原条目播放进度。

工作条目是合集内的逻辑展示身份；各原电影、媒体源、收藏、已看和播放记录仍属于各自媒体库条目。默认版本优先使用用户上次选择的可访问副本，再按用户媒体库顺序选择。只有至少两部不同的有效电影构成同一系列时才显示合集；详情只列出用户可访问的已入库电影。自动合集只由系统维护，已有手动合集合同保留。

#### LUX-538：建立跨库合集存储结构

范围：为系统管理的无根路径合集库、跨库系列引用、影片逻辑身份及原电影副本映射建立 SQLite/PostgreSQL 迁移。合集库稳定存在于存储层，但只在有用户可见合集时出现在客户端。原电影、文件和媒体源继续归属普通媒体库；不迁移或复制媒体文件。旧的按库合集表及其手动成员关系保持可用。

验收：

- [x] 从空库应用全部迁移后，系统合集库和跨库关系表可用，系统库没有文件根路径。
- [x] SQLite 升级保留已有库、合集、用户权限和成员关系；PostgreSQL schema 与 SQLite 对齐。
- [x] 相同稳定 ID 可表示多个库中的副本；现有手动合集结构及约束保持有效。

预计文件（5 个）：`migrations/0170_global_movie_collections.sql`、`migrations-postgres/0170_global_movie_collections.sql`、`src/storage/repository.rs`、`src/storage/migration.rs`、`tests/storage.rs`。

验证：`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test storage` 与 `git diff --check`。

#### LUX-539：持久化电影的可靠系列身份

范围：从已确认的刮削器结果和带可识别命名空间的 NFO `setid` 记录电影 ID、合集 ID、来源命名空间、系列名称及顺序提示。身份确认、元数据刷新或 NFO 索引更新后原子更新该关系；没有可靠 ID 时不建立关系。

验收：

- [x] TMDb 与 NFO 的同一命名空间/系列 ID 产生相同引用；不同命名空间不会仅凭数值相同或标题相同合并。
- [x] 缺失、非法、待确认和重新识别的元数据不会留下旧系列关系。
- [x] 重复确认和刷新幂等，不改变现有影片元数据优先级或 NFO 内容。

预计代码文件（5 个）：`src/application/candidates.rs`、`src/application/metadata.rs`、`src/storage/mod.rs`、`src/storage/catalog.rs`、`src/storage/metadata.rs`；存储和 NFO 回归测试放在对应模块内。本文档作为独立文档增量更新。

验证：所有 Cargo 命令均设置 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target`；运行 `cargo test --locked --lib storage::tests`、`cargo test --locked --lib application::candidates::tests`、`cargo test --locked --lib application::metadata::tests -- --test-threads=1`、`cargo test --locked --test metadata_selection`、`cargo fmt --all -- --check` 与 `git diff --check`。

补充实施结果（2026-10-15）：迁移 `0173_nfo_collection_reference_key` 为已有电影记录添加 NFO `setid` 索引状态，后台元数据 worker 会对未记录或与当前合集引用不符的旧 NFO 幂等补建/修复关系；状态与现有元数据更新合并写入，并在 SQLite catalog 表重建时保留。回归覆盖从 v172 升级、旧元数据指纹保留、NFO 重复处理幂等和缺失引用修复。

#### LUX-540：自动生成并维护全站系列与作品

范围：依据 LUX-539 的已确认关系，在后台幂等建立合集和逻辑作品，将各媒体库的相同影片副本挂到同一作品下。同步由元数据入库/变化及条目移除触发，不在用户请求中调用刮削器或扫描媒体库。系列不足两部有效不同影片时对客户端隐藏；恢复后重用稳定 ID。

验收：

- [x] 两个媒体库中的同系列电影自动产生一个合集；重复、并发和重启后的同步不重复建合集/作品。
- [x] 副本只计作一部作品；新增副本、移除文件、禁用/删除库及改正系列 ID 后关系和阈值正确。
- [x] 单条失败不会损坏上一份完整关系；后台可重试。

实施拆为两个独立提交，每个提交最多改 5 个文件：

1. SQLite/PostgreSQL 迁移、storage 后台同步事务与回归测试；加入可重试 outbox，关系变更、可用性变化、媒体库禁用/删除均在同一事务入队。
2. 合集后台 worker、启动/关闭接线与 worker 回归测试；启动时恢复积压任务，失败后保留队列并周期重试，不扫描媒体库也不调用刮削器。

客户端可见成员的海报回退由 LUX-541 在权限过滤后的读取路径实现，不能由全局后台写入共享海报路径。

#### LUX-541：提供权限安全的合集库与 Web 数据接口

范围：在 Lux 库列表中按需公开系统合集库；复用有界分页提供系列卡片和详情。权限按各用户当前可访问的原媒体库成员计算，详情包括去重作品、可选副本和用户最近选择的可访问来源，并提供经 CSRF 校验的来源/版本偏好写入接口。

验收：

- [x] 管理员和普通用户均不需要创建合集库；无可见合集时隐藏入口。
- [x] 用户仅能看到至少一个自己可访问成员所属的系列；合集海报缺失时只从当前用户可见成员回退，计数、元数据、图片和副本来源不泄露其它库内容。
- [x] 结果稳定分页、顺序确定；原电影库仍逐条返回普通电影，不出现系统逻辑作品。

预计文件（不超过 5 个）：库/媒体 API、access/catalog 服务、合集测试及本文档。

#### LUX-542：接入 Lux Web 合集浏览与来源选择

范围：显示“合集”库入口、系列卡片及详情作品列表；同一作品有多个可访问副本时展示来源库和版本选择，链接回原媒体详情并播放。

验收：

- [x] 系列按默认上映顺序展示；合集海报缺失时安全回退；窄屏和空列表状态可用。
- [x] 来源选择只显示用户可访问副本，选择后进入原条目并保留其既有收藏、已看和播放进度。
- [x] 详情中可从电影回到所属合集；普通电影库布局和内容不变。

预计文件（不超过 5 个）：Web API client/types、库列表/合集页/详情组件、Web 测试及本文档。

补充实现（2026-10-18）：虚拟合集库的 Web 入口封面优先使用当前用户可访问合集自身的海报，再以成员电影海报补足；首页和媒体库列表一致，无可用海报时显示专属合集封面样式。封面仅在客户端组合，不写入共享库数据，避免跨用户 ACL 泄露。Web 生产构建通过；本次未运行自动化测试或浏览器视觉验收。

#### LUX-543：接入 Emby 合集库与系列浏览

范围：Emby 用户库列表提供 `CollectionType=boxsets` 的系统入口；合集显示为 `BoxSet`，成员显示为唯一逻辑 `Movie`。不同媒体库的同片副本不重复显示为多个系列成员。

验收：

- [x] `/Users/{id}/Views`、合集根列表、系列 children 与计数均使用同一权限过滤和稳定 ID。
- [x] 普通 Emby 电影库和既有手动合集合同不变；管理员不能通过系统合集 ID 修改自动关系。
- [x] 分页、排序和列表 DTO 有集成回归。

预计文件（不超过 5 个）：Emby catalog/handler、合集访问服务、Emby 合集测试及本文档。

#### LUX-544：支持 Emby 跨库版本播放与进度

范围：合集逻辑电影的 `PlaybackInfo` 展示所有有权访问的原副本为媒体版本。实际选中的原媒体 source 负责播放地址、字幕及会话；回调经用户/设备/会话和 source 校验后只更新该原电影的播放状态。媒体源缺省回调无法唯一匹配时拒绝更新。

验收：

- [x] 任一客户端选择的来源只能访问其所属启用库中的真实 source；撤权后播放及旧会话回调均被拒绝。
- [x] `ItemId`、`MediaSourceId` 缺省/转换组合、暂停、进度、停止、跨设备并发均不会把状态写给另一副本。
- [x] Emby DTO 保持标准媒体版本字段；兼容记录明确标注 VidHub、SenPlayer、Infuse 的合集浏览/版本选择/进度回写尚未实测，不将服务端测试冒充客户端通过。

文件（5 个）：`src/application/catalog.rs`、`src/api/playback.rs`、`tests/emby_global_collection_playback.rs`、`docs/COMPATIBILITY.md` 及本文档。复用了现有 playback source 授权查询，没有 schema 或 storage 授权代码变更。

实施结果（2026-10-15）：Catalog 将 ACL 过滤后的原电影与可用媒体 source 装配给 Emby `PlaybackInfo`，优先遵循已保存的副本/版本偏好，再按默认版本和库顺序排列。响应 DTO、直放/转码地址、字幕流和播放时长均使用 source 所属的原电影 ID；source 授权仍由原媒体入口重新校验。以合集逻辑 ItemId 上报时，回调先按当前 ACL 与 MediaSourceId 唯一解析原电影；无 source 的回调只在原件或已有会话可唯一定位时接受。已有合集会话绑定原件、source 和设备，撤权或会话错配会在写进度、改状态或停止转码前拒绝。

验证：`cargo test --locked --test emby_global_collection_playback --test playback --test subtitles --test emby_multi_part_playback`、`cargo fmt --all -- --check`、`git diff --check` 通过；Cargo 使用项目共享 target 目录。自动化覆盖两份跨库原件的 DTO/直放字节/字幕、source 选择、唯一和歧义回调、暂停/进度/停止、独立设备会话、source/device/session 错配拒绝及撤权后旧 URL/回调拒绝。真实 VidHub、SenPlayer、Infuse 客户端尚未做合集流程实测，阶段门由 LUX-545 如实记录。

#### LUX-545：完成合集阶段门与客户端兼容记录

验收：

- [x] SQLite 空库和升级迁移检查通过：`cargo test --locked --test storage` 为 56 passed。
- [x] PostgreSQL 16 临时实例迁移回归通过：`cargo test --locked --target aarch64-apple-darwin --test postgres_database -- --include-ignored` 为 16 passed；空库启动断言 v173 合集表、系统合集库无根路径及 NFO 引用列。
- [x] Rust 完整完成检查通过：ARM64 `cargo test --locked --target aarch64-apple-darwin --all-targets` 在 4 个 test threads 下完成，库测试 872 passed、22 ignored，所有可运行 integration targets 通过。
- [x] Web 冻结依赖安装、完整测试（Node 139/139、Vitest 600/600）和生产构建通过。
- [x] Playwright 浏览器流程通过：打开合集库、进入“飞驰人生”、选择 4K 来源并触发来源偏好写入；该流程使用 mock API，服务端合同由 Rust 集成测试验证。
- [ ] VidHub、SenPlayer、Infuse 的合集浏览、来源选择与进度回写实测尚未完成，未实测客户端不记为通过。
- [x] `docs/COMPATIBILITY.md` 已记录本机 arm64、检查结果、客户端状态和剩余风险；本阶段停在项目所有者确认点。

预计文件：兼容记录、本文档和阶段验收涉及的回归文件。

实施记录（2026-10-15）：本机为 macOS `arm64`，Cargo target 使用项目共享目录。设置 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target` 后，`cargo build --locked --target aarch64-apple-darwin`、`cargo fmt --all -- --check`、`cargo clippy --locked --target aarch64-apple-darwin --all-targets --all-features -- -D warnings`、`git diff --check` 及 `RUST_TEST_THREADS=4 cargo test --locked --target aarch64-apple-darwin --all-targets` 通过。测试时让 `shutdown` 子进程使用临时 UDP 发现端口；查询计数 fixture 使用不启动周期 worker 的专用构造器。全量门禁中的 PostgreSQL 专用用例按设计忽略；随后在临时 PostgreSQL 16 实例单独运行 `postgres_database` 目标，16 项通过。Web 检查和 mock API 的浏览器流程沿用兼容记录。VidHub、SenPlayer、Infuse 客户端实测仍未完成，阶段门停在项目所有者确认点。

2026-10-15 续查：VidHub 2.1.8、SenPlayer 6.0.6、Infuse 8.5.1 均已安装并启动，但 CUA 绑定原生窗口均返回 `-10005 cgWindowNotFound`，无法执行合集浏览、来源切换和进度回写。隔离浏览器访问本机服务停留在登录页；未改动现有本地用户或媒体数据。客户端验收继续保持未完成。

#### LUX-546：持续协调数据库中的合集身份

范围：后台持续检查数据库中已保存 NFO 合集身份、但尚未生成规范合集关系的电影；身份可靠时不受资源入库时间或元数据完整状态限制。引用写入后复用现有合集同步 outbox。不同来源命名空间仍独立，跨库副本仍归并成一个逻辑作品。任务不扫描媒体目录、不调用刮削器或 `ffprobe`。

验收：

- [x] 升级前已存在的电影只要数据库缓存含可靠合集 ID 和可匹配的影片来源 ID，就会被后台分页协调入合集；元数据仍待确认、合集名缺失时也可加入。
- [x] 缺少合集名时用稳定占位名建关系；后续名称到达后更新现有合集，不重复建合集或作品。
- [x] NFO 缓存变化会重新进入协调；任务按有界批次持续运行、可重启、幂等，失败记录可重试。
- [x] 不同合集命名空间不会合并；同片跨库副本只计作一部作品，资源删除或合集 ID 修正继续由现有关系/outbox 流程收敛。
- [x] SQLite/PostgreSQL 空库与升级迁移可用，待协调查询有索引；测试覆盖旧缓存、未完成元数据、缺失合集名、重复处理及名称补齐。

预计文件（15 个，分四个可独立验证代码增量和一个文档增量，每个提交不超过 5 个文件）：SQLite/PostgreSQL 迁移、合集存储和 worker、候选与 NFO 回归、迁移/健康/扫描/兼容性测试中的 schema 版本断言，以及本文档。

实施结果（2026-10-15）：全局合集 worker 每轮从带部分索引的查询读取至多 32 条电影缓存，解析数据库中已缓存的 NFO，不扫描媒体目录、不调用刮削器或 `ffprobe`。它按来源命名空间合并 NFO 与数据库影片 ID，允许 `PENDING` 资源及无合集名称的可靠 ID；合集引用和协调标记在同一事务写入，之后由现有 outbox 同步逻辑合集。缓存 JSON 变化会清空标记，让新快照重新进入协调；失败行不会阻断同批其他条目，仍可重试。缺名称时内部保存占位标记、对外显示稳定的 `合集 <来源>:<ID>`；后续名称到达会更新现有合集。待确认候选也会保存可靠合集引用。迁移 `0175` 为 SQLite 与 PostgreSQL 增加相同的局部待协调索引。

验证：macOS `arm64`，Cargo 输出位于项目共享 target。`cargo build --locked`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 通过；`cargo test --locked --test storage` 为 59 passed；PostgreSQL 16 临时实例下 `cargo test --locked --test postgres_database -- --include-ignored` 为 16 passed，覆盖空库启动、旧迁移升级和新索引存在；合集 worker 回归与名称补齐用例通过。全量 `cargo test --locked --all-targets --no-fail-fast --quiet` 的 874 个库测试通过、23 个按设计忽略。唯一稳定复现的失败是 `tests/scanned_metadata.rs::failed_local_poster_insert_does_not_mark_image_stage_complete`：实际 2 张、预期 1 张；在干净基线 `8f9defcf` 上相同失败，属于本任务开始前已有问题。另一个 `local_nfo_retry_excludes_non_retryable_conflict_item` 仅在全目标并行运行时出现过一次 `RowNotFound`，定向复跑通过。Rust 静态 Web 用例首次因隔离 worktree 尚无 `web/dist` 返回 404；生成 Web 构建产物后定向用例通过。未部署、未访问或修改 FNOS 数据。

#### LUX-547：索引媒体目录中的 extrafanart 图片

范围：电影和剧集的本地图片扫描除资源目录直系图片外，也读取同目录 `extrafanart` 子目录中的直系图片，并把支持格式登记为 `FANART`，供媒体详情剧照栏使用。继续支持资源目录中的 `fanart1.jpg`、`fanart2.jpg` 及 `fanart-1.jpg` 等编号命名；只读取 `extrafanart` 下一级普通图片文件，不递归其他目录、不跟随符号链接。增量新增、修改、删除同样只更新所属电影或剧集的图片记录。

验收：

- [x] 电影全量扫描登记根目录中的编号 fanart 和 `extrafanart` 中的 JPG/JPEG/PNG/WebP 图片，图片类型均为 FANART，序号稳定。
- [x] 剧集扫描将剧集目录 `extrafanart` 图片登记到剧集条目，不登记到季或分集；编号 fanart 继续有效。
- [x] `extrafanart` 图片的增量新增、修改和删除更新对应剧集/电影图片记录；同目录其他媒体和条目 metadata 状态不变。
- [x] 支持类型、文件层级、symlink 边界与详情页使用的本地 `item_images` 记录有自动化回归覆盖。

预计文件（4 个）：`src/application/metadata.rs`、`src/storage/jobs/local_metadata/sources.rs`、`tests/scanning_jobs.rs`、本文档。

实现边界：电影图片继续按资源目录归属，因此同一资源目录中的多个电影源共享该目录的图片，与现有直系图片行为一致。扫描时可见的 `extrafanart` 目录符号链接和其中的图片符号链接会被忽略；文件系统枚举沿用路径式 API，不承诺抵御另一进程在检查和读取之间替换目录的竞态。

验证：macOS `arm64`，Cargo 输出位于 Toshiba 共享 target。本任务新增的 `manifest_scan_indexes_extrafanart_image_add_change_and_remove` 定向回归通过；`cargo build --locked`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 与 `git diff --check` 通过。`cargo test --locked --all-targets` 在 lib 测试目标结束：862 passed、13 failed、23 ignored，因此后续目标未运行；失败集中于字幕/播放子进程和插件运行时超时或缺文件，涉及文件不在本任务改动范围内。单独运行 `tests/scanning_jobs.rs` 为 86 passed、2 failed：`completed_scan_runs_pending_ffprobe_before_worker_returns` 收到空 BIGINT，`incremental_scan_probes_only_changed_local_sources` 的 ffprobe 结果超时。未部署或访问 FNOS。Cargo 命令须设置 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target`，并先确认 Toshiba 已挂载。

#### LUX-548：兼容 iOS 爆米花 HideFromResume 请求

范围：基于 iOS 抓包实现 `POST /Users/{UserId}/Items/{Id}/HideFromResume` 及 `/emby` 前缀；接受标准 query `Hide=true/false` 和 JSON `{"Hide": false}`。按用户/条目持久化隐藏标记，继续观看在聚合、计数与分页前排除隐藏条目；不清空进度、不修改收藏或已看状态。隐藏状态由显式 Hide 请求切换。

验收：

- [x] JSON 与 query 均支持 true/false，返回标准 UserItemDataDto；缺失、非法、冲突的值返回 400。
- [x] root 与 `/emby` 路由、数字和 UUID 条目 ID 可用；匿名、跨用户、不可访问条目不能修改状态。
- [x] 隐藏后 Emby Resume 和 Lux 继续观看排除该条目，恢复后保留原进度；重复请求幂等且其他用户不受影响。
- [x] SQLite/PostgreSQL 空库和旧库升级保留原播放状态；相关定向测试与 Rust 完成检查有记录。
- [x] 兼容记录明确区分抓包证据、自动化测试与部署后 iOS 实测，未实测不得标记客户端通过。

预计文件：按会话计划拆分迁移与 schema 断言、状态 DTO、存储/应用逻辑、HTTP 接口与回归、文档五类增量，每个提交不超过 5 个文件。

实现与验证（2026-10-15）：新增 SQLite/PostgreSQL 0177 迁移和按用户/条目的持久化隐藏标记；
HTTP 接口兼容标准 query 与 iOS JSON，请求成功返回含 `HideFromResume` 的 UserItemDataDto。
继续观看在聚合、计数、分页前排除隐藏条目，显式恢复保留原进度、已看、收藏、播放次数和时间。
JSON 请求先复现原实现的 405，再通过集成回归；库测试验证按用户隔离、幂等和隐藏最新单集后的回退。
`resume_favorites` 4/4、SQLite `storage` 60/60、临时 PostgreSQL 18/18 通过；build/fmt/clippy
通过，本机为 arm64。为静态资源检查执行的冻结锁文件安装与 Web production build 通过。
完整检查受共享 target 并发覆盖影响后，改用仍位于指定 target 内的专用 Cargo profile：
112 个测试目标编译成功，库测试 875 passed、23 ignored；对并行失败目标和继续观看目标通过 Cargo
串行复跑为 179 passed、1 failed。唯一持续失败是未改动的 NFO 海报回滚用例，预期 1、实际 2，
修复前干净基线 b5cbab81 同样复现，因此 Rust 全量 gate 未全绿。详细边界与检查过程见
`docs/COMPATIBILITY.md` 的 LUX-548 记录。未部署，未将 iOS 客户端真实页面标记为兼容通过。

#### LUX-549：修复图像编辑器当前图片的陈旧缓存

范围：图像编辑器展示当前图片时，在图片 URL 加上由 `contentTag` 派生的 `tag` 参数；缺少内容标签时使用图片记录 ID。
这样图片内容更新后浏览器会请求新 URL，同时保持服务端缓存与详情页使用的版本键约定。不改变数据库、图片 API 或图片写入方式。

验收：

- [x] 图片编辑器当前图片 URL 含有当前 `contentTag`；无内容标签时使用当前图片记录 ID。
- [x] 自动化回归覆盖两个版本键来源；选中的新版本不会复用旧 URL 的浏览器缓存。
- [x] 图像编辑器定向 Web 测试与 production build 通过；全量 Web 测试已执行，失败项记录在实施结果中。

预计文件（3 个）：`web/src/features/media/MediaImageEditor.tsx`、`web/tests/media-editors.test.tsx`、本文档。

实施结果（2026-10-15）：图像编辑器当前图片 URL 追加 `?tag=<contentTag>`，内容标签缺失时使用图片行 ID。TDD 回归在修复前因缺少版本参数失败，修复后 `media-editors.test.tsx` 7/7 通过。冻结锁文件安装与 Web production build 通过。

全量 `pnpm test` 的静态检查为 141 passed、2 failed：失败来自未修改的 `home-media-layout.test.mjs` 和 `responsive-layout.test.mjs`。因脚本在静态检查失败后停止，另行运行完整 Vitest：74 个文件中 599 passed、4 failed，失败位于未修改的 HomePage、home-media 与 library-card 测试；`media-editors.test.tsx` 7/7 通过。`git diff --check` 通过。没有 Rust、API 或数据库变更，未部署 FNOS。

#### LUX-550：隔离媒体详情页查询与交互渲染

范围：落实项目所有者提供的前端重构方案第三项。路由入口仅订阅当前 item；Hero、操作区、版本/轨道选择、剧集、演员、剧照和 NFO 按关注点拆分。操作区独立持有 playback query、操作及编辑器状态，编辑器按需动态导入；剧集和剧照查询只更新相关子树。保留 15 秒刷新、API、布局与现有用户流程；SSE 刷新策略属于方案第五项，不在本任务引入。

验收：

- [x] playback、剧集、剧照查询变化不重渲染无关 Hero、演员和 NFO；真实数据变化仍展示最新值。
- [x] 保留电影/视频/剧集/季度/单集布局、季海报回退、单季展开、分集分页和下一集播放。
- [x] 保留版本/音轨/字幕切换、收藏/已看、元数据确认、下一个待确认、编辑与删除流程。
- [x] 编辑器仅在打开时动态加载；保持加载和失败反馈。
- [x] 针对性回归、完整 Web 检查及真实浏览器验证已执行，优化前后数据与既有失败均记录；完整 Web 门禁尚未全绿，见下方结果。

预计文件按四个代码增量与最终验证增量拆分，每次提交不超过五个文件：

1. 本文档、`web/src/features/detail/MediaDetailPage.tsx`、`MediaDetailSections.tsx`、`web/tests/media-detail-performance.test.tsx`：隔离演员/剧照/NFO。
2. `MediaDetailPage.tsx`、`MediaEpisodesSection.tsx`、`useMediaEpisodes.ts`、性能回归：隔离剧集查询与列表。
3. `MediaDetailPage.tsx`、`MediaDetailHero.tsx`、`MediaSourcePanel.tsx`、`media-source.ts`、`web/tests/detail-layout.test.mjs`：拆分 Hero 与版本轨道状态。
4. `MediaDetailPage.tsx`、`MediaDetailActions.tsx`、共享 `MediaEditorBoundary.tsx`、`web/tests/detail-logo-layout.test.mjs`、详情回归：操作区和编辑器延迟加载。
5. 本文档、`docs/COMPATIBILITY.md`、`docs/PERFORMANCE.md` 与两个浏览器验证脚本：验收与证据。

实施中另按至多五文件的独立提交提取 `home/media-display.tsx`，移除详情和编辑器对媒体卡片模块的静态依赖；卡片入口仅调整共享编辑器的动态导入和加载边界，未执行方案第一项的卡片结构或多选重构。父剧查询与剧集查询各由一个 provider 持有，避免拆分后增加轮询 observer；Hero 仅订阅图片查询中的 logo 值。

验证结果（2026-10-16，代码 `5c5661ab`，对照 `8c8d5c13`）：100 演员、五次 playback 更新时，演员/剧照/NFO 各自额外渲染从 5 次降为 0 次，实际收藏状态仍更新。详情入口从 1102 行降到 80 行。生产构建的入口与详情 JS 合计从 178.40 kB 降为 164.32 kB（不含公共 vendor；gzip 49.52→45.93 kB），五个编辑器各自形成动态 chunk。具体测量边界见 `docs/PERFORMANCE.md`。

- 冻结依赖安装、TypeScript、生产构建通过；详情/性能/编辑器边界 Vitest 43/43，相关 Node 静态回归 35/35 通过。
- 完整 `pnpm --dir web test` 的 Node 阶段 142 passed、2 failed，脚本因此未进入 Vitest；随后单独完整 Vitest 为 608 passed、4 failed。六项失败均在干净基线 `8c8d5c13` 复现：移动卡片尺寸和 touch selector 静态断言两项，继续观看评分、首页库卡片预取、库卡片预取及虚拟合集旧链接断言四项。完整门禁不能记为通过。
- Playwright Chrome `155.0.8059.41` 使用本分支 production bundle 和合成 API，验证 1440/390 px 无横向溢出、五编辑器按需下载与关闭卸载、版本/音轨、收藏/已看、元数据保存、指定版本删除失败反馈、单季展开、分页、下一集和季海报回退；额外模拟 chunk 延迟/失败，确认详情保留且错误可关闭。无非预期浏览器错误；删除 fixture 的一次 HTTP 409 属预期。
- 本机 macOS `arm64`；`cargo fmt --all -- --check` 通过，Cargo 输出目录指定 Toshiba 共享 target。无 Rust/API/数据库/依赖变更，未运行 Rust build/test/clippy，未部署或验证 FNOS/x86、真实服务器与客户端性能。

#### LUX-551：拆分媒体卡片并按需加载编辑器

范围：执行前端重构方案第一项。将 MediaCard 拆为 memo 外壳、纯展示层、操作触发层及条件加载的编辑器层；五种编辑器按打开动作动态导入。库页多选传递稳定回调和条目 ID，单张卡片选择不重绘其余卡片。保持首页、库页、搜索、收藏、文件夹和只读合集卡片的既有展示及操作语义。

验收：

- [x] 无关父状态更新不重绘 props 未变的卡片；选择单项只更新目标卡片，操作状态不重绘展示层。
- [x] 五种编辑器初次展示卡片时均不加载，仅打开对应功能时加载和挂载；关闭卸载，删除保留默认源选择及成功移除行为。
- [x] 标题、年份、图片版本、评分、集数、进度、待确认标记、导航、错误提示及只读/文件夹限制保留并有回归。
- [x] 冻结依赖安装、Web 测试、生产构建及相关 Playwright 流程验证完成；记录本机 ARM 性能与首屏 bundle 前后数据，不把方案预期比例当作实测结果。

预计增量：一（4 文件）：本文档、`web/src/features/home/media.tsx`、`media-card-editors.tsx`、`web/tests/media-card-loading.test.tsx`；二（至多 5 文件）：`media.tsx`、`media-card.tsx`、`media-card-visual.tsx`、`media-card-overlay.tsx`、`web/tests/media-card-performance.test.tsx`；三（至多 5 文件）：`web/src/features/library/LibraryPage.tsx`、库页性能回归、随组件搬迁调整的静态布局测试、本文档和性能记录。每个增量测试通过后独立提交。

实施结果（2026-10-16）：在 `codex/lux-551-media-card-performance` 分支完成三个独立增量。`MediaCard` 现在由 memo 外壳、`MediaCardVisual`、`MediaCardCopy`、`MediaCardOverlay` 和 `MediaCardEditors` 组成；五个编辑器改为动态导入，仅对应操作打开时请求。操作回调保持稳定，库页选择回调按条目 ID 复用，避免选择一个条目时为全部卡片创建新闭包。新增加载、显示状态、操作状态、文件夹/只读限制、库页选择和媒体卡片性能回归。

验证：Toshiba 已挂载，本机 `uname -m=arm64`。`CI=true corepack pnpm@10 --dir web install --frozen-lockfile`、`CI=true corepack pnpm@10 --dir web build` 通过；媒体卡片和库页选择定向 Vitest 5/5 通过。120 张卡片的 Playwright 基准中，12 次无关父级更新的展示读取从基线 1,440 次降为 0，React 提交耗时由每次约 5.4–18.1ms 降为 0–0.2ms；编辑器动态模块在卡片初次展示时未请求，打开元数据编辑器后只请求对应模块。桌面和 320px 移动视口截图确认编辑器布局可见且无重叠。

全量 `pnpm test` 的 Node 静态检查在基线 `c05d0eb0` 和本分支均为 141/143：两项旧断言分别期待已变更的首页媒体库卡片尺寸，以及旧的触控操作 CSS 选择器。Vitest 单独运行时，基线为 599/603，本分支为 605/609；四项相同的既有失败位于继续观看评分断言、首页库卡片预取，以及库卡片预取和虚拟合集旧选择器。此次新增/修改的媒体卡片与库页选择测试均通过。未部署或访问 FNOS。

#### LUX-552：媒体库网格虚拟化

范围：落实用户提供的前端重构方案第二项，使用 `@tanstack/react-virtual` 按行虚拟化 LibraryPage 已加载的媒体；上下各缓冲两行，沿用现有 CSS 响应式列宽和窗口滚动。分页查询与底部 IntersectionObserver 继续负责加载，不扩大到其他页面或其他重构项。

验收：

- [x] 500+ 已加载条目只挂载可视行与缓冲行，首尾条目均可滚动访问。
- [x] 响应式列数与真实行高同步，元数据徽章、窗口缩放和不满行不重叠、不裁切。
- [x] 排序、筛选、文件夹、合集、跨屏批量选择及原有分页/失败重试行为继续可用。
- [x] 增加自动化回归并完成本机浏览器前后对比、Web 单测与构建；记录失败和未验证边界。

预计文件分三个增量：依赖及登记（`web/package.json`、`web/pnpm-lock.yaml`、本文档）；实现及回归（`web/src/features/library/LibraryGrid.tsx`、`web/src/features/library/LibraryPage.tsx`、`web/src/react.css`、`web/tests/library-grid.test.tsx`，必要时扩展 `web/tests/library-page.test.ts`）；验收记录（本文档、`docs/COMPATIBILITY.md`）。每个增量不超过 5 个文件。

参考：[TanStack Virtualizer API](https://tanstack.com/virtual/latest/docs/api/virtualizer) 的 `scrollMargin`、`measureElement`、`gap` 与 `rangeExtractor`。

实现与验证（2026-10-15）：`LibraryGrid` 使用窗口虚拟化和真实行高测量，响应式列数直接读取现有 CSS 网格，包含上下两行缓冲；保留聚焦卡片所在行，窗口列数变化时保留其父节点身份和编辑草稿。行使用 `top` 定位，避免 transform 改变卡片内 fixed 对话框的包含块。列表提供位置/总数语义，库页继续持有跨屏选择。虚拟化依赖单独分包，只随库页加载（production gzip 约 8.42KB）。追加增量仅涉及 `web/vite.config.ts` 的分包规则，以及 `LibraryGrid.tsx`/回归测试中的聚焦卡片身份保留。

TDD：原实现加载 504 项时实际挂载 504 张卡片，节点上限回归先失败；列数变化导致编辑草稿丢失的回归同样先失败再修复。最终 `library-grid.test.tsx` 5/5、`library-page.test.ts` 19/19 通过。冻结锁文件安装、TypeScript 检查、Web production build、`cargo fmt --all -- --check` 和 `git diff --check` 通过。本任务没有 Rust 行为变化，未运行 Cargo 编译、Clippy 或 Rust 测试；Cargo fmt 使用 Toshiba 共享 target。

浏览器验证：本机 macOS arm64、Headless Chrome 155、Playwright。504 项模拟数据在 320/768/1024/1440px 宽度的首屏分别挂载 8/12/20/28 张卡片；首尾可达、无水平溢出和行间重叠。跨屏多选、从 24 项连续分页至 504 项（最后 42 张卡片）、打开实际卡片编辑器后从 1440px 缩至 320px 保留草稿均通过，无 pageerror。1440×900 下三轮交替 A/B 中，基线 `c05d0eb0` 的卡片/DOM 节点数为 504/10294，本实现为 28/648；初始 React Profiler 各次提交耗时总和的中位数从 138.2ms 降至 47.6ms（约 65.6%）。此数据来自开发构建、内存缓存、无真实海报的本机模拟数据，不代表真实网络、NAS 或移动设备性能，也不是 INP/LCP 指标。

全量检查边界：`pnpm test` 的静态测试 141 passed、2 failed，脚本在此停止；独立运行完整 Vitest 为 605 passed、4 failed（75 文件通过、3 文件失败）。失败为首页卡片紧凑宽度、触屏操作控件静态断言、继续观看评分、首页预取入口，以及 LibraryCard 预取和只读合集入口。六个失败均用原始基线的 `LibraryPage.tsx` 与 `react.css` 复现，相关其他源码和测试与基线一致，未扩展本任务处理。完整 Web gate 因既有失败未全绿。未推送、合并或部署；Safari/iOS 真实设备未验证。

#### LUX-553：优化横向滚动栏与前端动画

范围：落实前端重构方案第七项和第八项。横向滚动栏在滚动时不再逐事件同步读取元素几何信息；移动端导航、首页 Hero 轮播和登录卡片使用 CSS 动画，消除应用主入口对 `framer-motion` 的依赖。保留滚动按钮、导航开关、轮播切换、视觉布局和现有文案；动画遵从减少动态效果设置。

验收：

- [x] `HorizontalScrollRail` 通过 `IntersectionObserver` 更新首尾边界状态，滚动事件路径不调用 `getBoundingClientRect()`；内容或尺寸变化后箭头状态仍正确。
- [x] 不支持 `IntersectionObserver` 的环境有节流回退；滚动按钮仍按视口比例平滑滚动，到达边界后正确隐藏对应按钮。
- [x] 移动导航和 Hero 背景/文案在状态变化时平滑过渡；关闭导航后不可点击、聚焦或被辅助技术访问。
- [x] 登录、首页和移动导航动画使用 CSS，所有 `framer-motion` 调用点移除后依赖可删除；减少动态效果模式禁用新增动画。
- [x] 自动化回归覆盖滚动边界、导航开关、轮播切换和动画减少策略；真实浏览器流程通过。
- [ ] Web 全量检查通过。执行记录见下方；当前失败均在本任务未修改的测试中。
- [x] 记录 ARM64 环境优化前后主入口 JS/gzip 体积；不得将构建体积差异表述为真实设备帧率或 RUM 数据。

预计分五个增量，每次不超过五个文件：

1. 本文档、`web/src/components/layout/HorizontalScrollRail.tsx`、`web/tests/horizontal-scroll-rail.test.tsx`。
2. 本文档、`web/src/components/layout/LuxShell.tsx`、`web/src/react.css`、`web/tests/lux-shell.test.tsx`。
3. 本文档、`web/src/features/home/HomePage.tsx`、`web/src/react.css`、`web/tests/home-page.test.tsx`、`web/tests/home-hero-layout.test.mjs`。
4. 本文档、`web/src/features/auth/LoginPage.tsx`、`web/src/react.css`、`web/tests/login-page.test.tsx`。
5. 本文档、`web/package.json`、`web/pnpm-lock.yaml`、`web/vite.config.ts`。

实施结果（2026-10-16）：`HorizontalScrollRail` 用首尾卡片的 `IntersectionObserver` 可见边界更新滚动箭头，滚动路径不再同步读取 DOM 几何；内容替换时重新绑定端点，缺少 IntersectionObserver 时使用 requestAnimationFrame 合并的尺寸指标回退。移动导航、Hero 背景和文案、登录卡片改用 CSS 动画，并保留减少动态效果支持；删除 `framer-motion` 依赖及 Vite 分块规则。

验证：新增与相关回归通过：`horizontal-scroll-rail.test.tsx` 7/7、`lux-shell.test.tsx` 18/18、`home-hero-layout.test.mjs` 9/9、`login-page.test.tsx` 13/13。`home-page.test.tsx` 为 16/17，唯一失败是已有的媒体库分页预取夹具测试；它在本任务改动前的基线运行中也失败。冻结锁文件安装、生产构建和 `git diff --check` 通过。真实 Playwright 浏览器使用本地 API mock 复核了移动端 Hero、导航开关与继续观看滚动；导航关闭时带 `aria-hidden` 和 `inert`，浏览器控制台错误为 0。未访问生产数据或部署环境。

Web 全量检查未全绿：`pnpm test` 的静态层为 141 passed、2 failed，失败在未修改的 `home-media-layout.test.mjs` 与 `responsive-layout.test.mjs`；脚本因静态层失败而未继续，另行运行完整 Vitest 后为 74 个文件、601 passed、4 failed，失败分布在未修改的 HomePage、home-media 与 library-card 测试。相关失败与本任务修改文件无关，保持为未通过验收项，不为此扩大任务范围。

ARM64（`uname -m`：`arm64`）生产构建中，优化前入口 JS 为 124.09 kB / 33.47 kB gzip，另有静态依赖的 motion chunk 128.88 kB / 42.37 kB gzip；优化后入口 JS 为 124.49 kB / 33.70 kB gzip，motion chunk 已移除。按入口和该静态 motion chunk 合计，构建 JS 体积约减少 128.48 kB / 42.14 kB gzip；该数据只表示构建产物体积，不代表运行帧率或 RUM 表现。

#### LUX-554：路由切换时保留 Lux 导航

范围：普通懒加载页面的 Suspense fallback 放在 LuxShell 内容区，路由 chunk 加载时 header 和主导航保持可见；管理页子路由的 fallback 限定在 `.lux-admin-content`，管理侧栏保持可见。常用主导航和账户入口在指针进入或键盘聚焦时预加载目标页面 chunk。播放器路由继续使用全屏加载态。不改变页面数据、API 或路由权限。

验收：

- [x] 延迟加载媒体库页面 chunk 时，LuxShell header 和主导航保持可见，内容区显示轻量 skeleton；加载完成后页面正常替换。
- [x] 延迟加载管理页 chunk 时，主导航、管理侧栏保持可见，只有管理内容区显示 skeleton；加载完成后 dashboard 正常替换。
- [x] 桌面与移动导航，以及账户入口的焦点/指针预加载有回归；播放器路由仍位于全屏 Suspense fallback 下。
- [x] 冻结依赖安装、生产构建、相关 Web 回归和真实浏览器流程已运行；浏览器使用 2.5 秒本地 chunk 延迟及 mock API，未据此声称生产性能数值。
- [x] 已执行全量 Web 检查，并在干净 `test` 基线复现相同的 6 个既有失败；项目所有者于 2026-10-16 确认仅对 LUX-554 按基线例外关闭。全局 Web 完成门仍未通过。

预计文件：`web/src/app/App.tsx`、`web/src/features/admin/AdminLayout.tsx`、`web/src/components/layout/LuxShell.tsx`、`web/src/react.css`、相关 Web 测试及本文档。实现拆为路由边界与导航预加载两个独立提交。

实施结果（2026-10-16）：浏览器在 2.5 秒延迟媒体库模块时确认 header/nav 可见且没有全屏 loading；延迟管理 dashboard 模块时确认 LuxShell 与管理侧栏可见，内容 skeleton 在模块加载后消失。目标回归：`app-code-splitting.test.mjs` 5/5，`login-page.test.tsx` 14/14，管理事件/预加载 15/15，LuxShell 17/17、路由预加载 1/1；`pnpm install --frozen-lockfile` 与 `pnpm build` 通过。

全量 `pnpm test` 的静态检查为 143/145，失败在 `home-media-layout.test.mjs` 与 `responsive-layout.test.mjs`；单独 Vitest 为 601/605，4 个失败在 `home-page.test.tsx`、`home-media.test.tsx` 和 `library-card.test.tsx`。在干净 `test` 基线 `8c8d5c13` 重跑这 5 个失败目标，静态目标为 38/40、Vitest 目标为 28/32，完全重现同一组失败；相关测试与组件文件相对基线均未修改。本任务没有 Rust/API/数据库变更；全量 Web 完成门仍未全绿，未部署。

#### LUX-555：首页查询缓存与缓存媒体库并发

范围：落实前端重构方案第四项的短期方案及第五项的首页 staleTime。首页四个独立区块的查询保持 30 秒新鲜；回访期间复用 QueryClient 缓存。现有 `librariesQuery.data` 已同步提供缓存库 ID，不额外复制缓存或恢复慢聚合请求。冷启动无库 ID 时仍先加载媒体库，不声称消除冷启动 waterfall。

预计文件（3 个）：`web/src/features/home/HomePage.tsx`、`web/tests/home-query-policy.test.tsx`、本文档。

验收：

- [x] 30 秒内回访不重新请求四个首页区块；过期后正常刷新。
- [x] 有缓存库 ID 时最新资源与媒体库刷新并发；服务器返回不同 ID 后请求新的分组。
- [x] 首页定向回归、Web 全量检查和浏览器验证均已执行；全量基线失败在后续实施记录中单独列出。

#### LUX-556：共享用户 SSE 连接状态

范围：Shell 继续独占一条用户事件连接，以 React context 向内容页共享连接状态。未连接、不支持 EventSource 或 error 时为 disconnected；open 后 connected，重连仍复用同一 EventSource；卸载移除监听并关闭连接。

预计文件（4 个）：`web/src/features/activity/useUserEvents.ts`、`web/src/components/layout/LuxShell.tsx`、`web/tests/user-events.test.tsx`、本文档。

验收：连接、断连、重连、浏览器不支持与卸载关闭均有组件回归。后续任务接入条件轮询。

#### LUX-557：用户事件覆盖详情与收藏缓存

范围：接收 home/item/library/favorites 事件；兼容既有仅带 home 的服务器，并刷新详情、图像、播放信息、子项、收藏、人物与合集缓存。可选 itemId/libraryId 收窄相应缓存；不失效正在播放的 bootstrap/session。所有媒体查询保留在途请求与尾随刷新，首页首次失败仍保留手动重试，防止事件风暴反复启动失败请求。

预计文件（4 个）：`web/src/features/activity/useUserEvents.ts`、`web/tests/user-events.test.tsx`、`web/tests/lux-shell.test.tsx`、本文档。

验收：各作用域、可选 ID、无效 payload、会话缓存保留及已有首页在途请求保护均有回归。

#### LUX-558：为用户查询补齐服务端失效来源

范围：UserEventScope 增加 item/library/favorites 字符串，仍只发送无业务数据的 scope。应用状态将用户 hub 装配到管理员事件中心；元数据修改、库/访问权限/设置修改及任务关键状态向普通用户发送失效，高频 jobs progress 不转发。服务端广播丢帧仍回退 home，前端按全部媒体展示缓存处理。

预计文件（5 个）：`src/application/admin_events.rs`（含库测试）、`src/api/legacy.rs`、`tests/admin_events.rs`、`docs/API.md`、本文档。

验收：作用域 wire names、管理员修改桥接、进度不转发有库测试；执行最窄 `cargo test --locked --lib application::admin_events::tests`，再以 `--test admin_events` 验证鉴权与 SSE 帧。所有 Cargo 命令使用 Toshiba 统一 target。

#### LUX-559：媒体页在线停轮询与断线兜底

范围：首页四项查询和详情六项查询读取共享 SSE 状态；在线不做周期请求，断线/不支持 EventSource 时前台每 30 秒轮询。首页未有成功数据的失败请求仍由显式重试恢复。QueryClient 默认 60 秒 staleTime、单次重试及禁止焦点刷新保持既有配置；首页覆盖为 30 秒。

预计文件（5 个）：`web/src/features/home/HomePage.tsx`、`web/src/features/detail/MediaDetailPage.tsx`、`web/src/lib/api/query-keys.ts`、`web/tests/media-query-polling.test.tsx`、本文档。

验收：真实 QueryClient 与组件的虚拟时钟回归覆盖首页/详情一分钟在线零周期请求、断线 30 秒刷新与重连停轮询。定向 5 文件 / 65 项测试及 TypeScript、Web production build 通过；全量与浏览器检查后汇总。

#### LUX-564：选择并固定 SQLite Manifest 写事务预算

范围：保持 streamed reader 8,000 文件上限独立，为 SQLite 完整写事务固定经过 release A/B 验证的默认预算；比较 512、1,024、2,048、4,000、5,000、6,000、7,000、8,000，使用相同 60k/600 fixture、FULL synchronous 和固定池配置。扫描/DML 组关闭锁探针；独立竞争组从第二个 SQLite pool 周期性提交真实 scan-job progress update，分开记录 pool acquire、BEGIN IMMEDIATE admission、完整写入延迟和 outcome。PostgreSQL 默认路径不变。

实施文件：`src/application/scanner/jobs/commit_batches.rs`（SQLite 默认预算与回归）、`src/storage/repository.rs`（SQLite 专用 presence CAS 行批次上限）、`src/storage/jobs/scan/manifest/discovery/presence.rs`（应用上限）、`src/storage/jobs/tests.rs`（fingerprint CAS 回归）。A/B 复用 `tests/performance.rs` 现有报告入口，结果记录于 `docs/PERFORMANCE.md`。

验收：

- [x] reader 仍按 8,000 文件读取，事务预算单独设定；release 基准报告明确记录实际事务预算和进程 CPU 时间。
- [x] SQLite presence 更新单条语句最多分片 10,921 项（每项 3 个 bind + 3 个固定 bind，保持 SQLite 32,766 参数上限内），PostgreSQL 分片保持原值；fingerprint 匹配、旧 fingerprint 与 missing 行回归通过，CAS 与 seen-path ledger 语义不变。
- [x] 确认 4,000 为默认事务预算：存储批次优化后 4,000/8,000 无锁 A/B 交错五轮中位数分别为首扫 2,275/2,285 ms、unchanged 重扫 1,035/1,117 ms、目录 API p95 602/599 ms、foreground p95 370/377 ms、batch p95 321/328 ms、DML 189/133，4,000 的 WAL/RSS 更低。独立写者完整事务 p95 三轮中位数为 4,000 107.1 ms、5,000 126.4 ms、6,000 150.2 ms、8,000 223.1 ms；4,000 相对 8,000 改善 52.0%，且保持 `<209 DML` 门。7,000 的单轮 unchanged 重扫超过 5% 门；5,000/6,000 未达到 50% 写者 p95 改善目标。100 ms 是期望值，4,000 实测 107.1 ms；本机 ARM64 fixture 不外推 FNOS/NAS 或 x86_64。
- [ ] 完整 Rust all-targets gate 未通过。最终 `cargo build --locked`、`cargo fmt --all -- --check` 与全 targets / features Clippy 通过；`storage::repository::jobs` 为 16/16，串行 `scanning_jobs` 为 89/89。并行 `scanning_jobs` 首次有 1 项报 SQLite `database is locked`，该用例隔离重跑 1/1、串行全目标通过。
- [ ] 当前分支 `cargo test --locked --all-targets -- --test-threads=1` 的库测试为 884 passed / 1 failed / 23 ignored；失败为 `application::metadata::tests::scan_local_nfo_page_flushes_actor_credits_and_isolates_invalid_nfo`，其 actor-credit 并发失败项集合不符合断言，隔离重跑 1/1 通过。此前 all-targets 在 `1e7c2ea8` 上已复现 `scanned_metadata::failed_local_poster_insert_does_not_mark_image_stage_complete`；跳过它后的测试曾因 `web_static::same_origin_web_assets_are_served_by_rust` 固定资产断言停止，使用同一构建产物在基线也复现。当前完整 gate 不能报绿。
- [x] PostgreSQL runtime gate 通过：临时 PostgreSQL 16 Alpine 容器只绑定 localhost，数据库数据位于 tmpfs；`cargo test --locked --test postgres_database -- --include-ignored --test-threads=1` 为 18/18。测试后容器已停止并自动删除。

候选中间值的单轮筛选、逐轮 A/B、独立写者样本、此前基线测试复现及门禁限制见 `docs/PERFORMANCE.md`。
#### LUX-563：SQLite Manifest 已提交分片的恢复与幂等

范围：用实际文件 SQLite 验证 PERSISTED Manifest 在首片提交、后片失败、数据库连接关闭并重新打开后，可从 durable directory frontier 重试。重试必须保留此前完成的目录和正向索引，不重复 observed/processed 计数，也不重复 outbox source IDs 或 batch sequence。仅在发现存储合同缺陷时另拆最小 storage 修复任务。

预计文件（2 个）：`tests/scanning_jobs.rs`、本文档。分别验证 LUX-562 的 256 文件 streamed SQLite 分片重开恢复，以及 PERSISTED discovery 在 8,000 条 observation reader chunk 提交后、下一事务失败时的重开恢复；不改公共模型、schema 或 SQL 行为。

验收：

- [x] 重开 SQLite 后 manifest/job checkpoint 持久；完成目录不重新入队，未完成目录恢复为可处理 frontier。
- [x] 同一 job 重试完成后，manifest/root observed 与 completed 计数精确，generation 索引文件完整，outbox source IDs 与 batch sequences 全唯一。
- [x] 256 文件切片和 PERSISTED 8,000 条 observation reader-chunk 后续事务失败，均在关闭并重开 SQLite 后从 durable frontier 恢复；既有 PERSISTED shutdown-resume 与 LITE retry 回归仍通过。

定向验证：`LUX_PERF_DISCOVERY_TRANSACTION_FILES=256 cargo test --locked --features performance-benchmarks --test scanning_jobs manifest_discovery_keeps_committed_slices_when_a_later_slice_fails -- --exact --ignored`、`cargo test --locked --test scanning_jobs manifest_discovery_reopens_and_retries_persisted_frontier_idempotently -- --exact`、`cargo test --locked --test scanning_jobs shutdown_cancelled_manifest_resumes_the_persisted_discovery_frontier -- --exact`、`cargo test --locked --test scanning_jobs failed_manifest_discovery_retries_its_pending_frontier_safely -- --exact`。

验收结果：两个实际 SQLite 注入故障测试均通过关闭连接、重开数据库和同 job retry 验证；256 文件 LITE 分片最终索引/outbox 各 1,025 条唯一，PERSISTED 在首个 8,000 observation chunk 后保留 7,998 个正向索引并从持久 frontier 完成 8,001 个文件，计数与 outbox source IDs / batch sequences 无重复。既有 shutdown-resume 与失败 retry 各 1/1，`scanning_jobs` 串行完整目标 89/89 通过。首轮完整目标的两个 ffprobe 用例曾分别报 NULL 解码和 TIMEOUT，单独重跑均通过，随后完整串行目标也全绿；最终预算 A/B 与完整 Rust gate 留待 LUX-564。

#### LUX-562：SQLite Manifest discovery 按完整事务分片

范围：在 application 层、基线读取和正向索引准备之前，将 SQLite 的 Pending Manifest directory chunks 按有界文件数、总 observation entries、child frontier 和 completed-directory 数分成有序微批次。每片独立执行完整 storage 事务；DIRECTORY observation 与对应 child frontier 同片，跨片目录的 `completed_directory` 只落在最后一片。每片重新读取数据库基线并执行现有文件/root/目录复核。PostgreSQL 保留现有聚合事务路径。

预计文件（5 个）：`src/application/scanner/jobs/manifest_discovery.rs`、新建 `src/application/scanner/jobs/commit_batches.rs`、`src/application/scanner/tests.rs`、`tests/scanning_jobs.rs`、本文档。

初始 SQLite 文件预算保持 8,000，与当前最大 streamed file budget 一致；测试 feature 可注入较小值以验证实际事务边界，不增加用户配置或持久化 schema。此任务不选择最终生产预算，也不修改 `<209 DML` 门槛。

验收：

- [x] 分片器覆盖空/单条/边界/超限/多倍输入，混合文件、子目录和 completion；顺序保留，每个 observation/frontier 恰好一次，且每片所有维度均不越预算。
- [x] SQLite integration fixture 注入 256 文件预算后证明 1,025 个文件跨多个独立 root checkpoint 事务提交；父目录只在最后一片完成，索引、outbox source IDs 与计数无遗漏/重复。
- [x] SQLite 默认预算保持 8,000；PostgreSQL 保留单一聚合批次路径；注入第二片错误后后续片停止，首片索引/outbox 保留且 completion 未提前写入。
- [x] 分片在基线查询、positive preparation 和 identity recheck 之前；storage 的一次调用仍全成或全败。

定向验证：`cargo test --locked --lib commit_batches`、`LUX_PERF_DISCOVERY_TRANSACTION_FILES=256 cargo test --locked --features performance-benchmarks --test scanning_jobs manifest_streams_large_directory_discovery_in_bounded_chunks -- --exact`、`LUX_PERF_DISCOVERY_TRANSACTION_FILES=256 cargo test --locked --features performance-benchmarks --test scanning_jobs manifest_discovery_keeps_committed_slices_when_a_later_slice_fails -- --exact --ignored`，随后运行 `cargo test --locked --test scanning_jobs`。

验收结果：分片器单测 3/3；256 文件预算的 1,025 文件成功与第二片故障注入测试均通过；默认构建下完整 `scanning_jobs` 目标串行运行 88/88 通过，覆盖 8,000 文件默认预算。结构检查确认 SQLite 每片都重新读取基线并进入原有完整 storage 事务，PostgreSQL 直接保留原批次；格式与 diff 检查通过。最终预算选择和 A/B 性能门留待 LUX-564。

#### LUX-561：记录 Manifest 写事务体耗时与结果

范围：为 Manifest discovery 写事务单独记录事务体时间，排除 `BEGIN IMMEDIATE` 获取写者资格和 `COMMIT` 时间；记录事务体成功/失败计数，不记录错误文本或业务标识。只增加观测，不改变事务边界、批次大小、存储语义或性能门槛。

预计文件（4 个）：`src/storage/jobs/scan/manifest/discovery/coordinator.rs`、`tests/performance.rs`、`docs/PERFORMANCE.md`、本文档。

验收：

- [x] release Manifest 基准报告包含 `transaction_body` p50/p95 和事务体 outcome 计数，至少存在成功样本。
- [x] 事务体错误分支只记录耗时及 `error` 标签；计数器回归覆盖 success/error 事件，日志不记录错误内容或用户数据。
- [x] 事务计时能与 BEGIN 等待、COMMIT 分开；现有 `<209 DML` 门槛和事务行为不变。
- [x] 只记录 ARM64 本机观测，不推断 NAS/FNOS 性能收益；更新性能记录并保留干扰因素。

最窄目标：`cargo test --locked --test performance`；release 基准：`LUX_PERF_TEST_FILTER=lux_270_manifest_job_scan_benchmark ./scripts/run-performance.sh`。

实施结果（2026-10-16）：storage 现在分别记录 `transaction_body`、BEGIN 和 COMMIT，事务体事件只带 success/error 与条目计数。性能报告序列化并 reset outcome 计数；定向单测通过，60k SQLite release 基准 1/1 通过，两轮各观测到 9 个成功 body。两轮首扫 `transaction_body` p95 为 199,544 / 188,235 µs，DML 均为 127，低于原 `<209` 门槛。探针启用时的写锁等待 p95 为 232,899 / 223,689 µs；探针会主动争用写锁，所以数据只用于说明观测已接通和门槛未回归，不作为无探针性能基线。性能测试目标 18 passed、5 ignored；格式检查通过。

#### LUX-560：用户状态修改向其他会话发送失效

范围：Web/Emby 收藏、已看修改在状态持久化及容器状态同步成功后发布无 ID 的 favorites/item 用户事件；Web 两种播放事件路径、账户库顺序和排序策略更新发布合并 home 事件。人物收藏同样发 favorites。HTTP 响应和认证/CSRF/ACL 保持既有合同。

预计文件（5 个）：`src/api/playback.rs`、`src/api/users.rs`、`src/api/legacy.rs`、`tests/user_query_events.rs`、本文档。

验收：真实 HTTP/SSE 回归先复现收藏写入后收不到事件的超时，随后验证事件到达后数据库已反映收藏/已看状态，并覆盖播放进度和媒体库排序。最窄目标 `cargo test --locked --test user_query_events --test resume_favorites --test web_playback`。

实施结果（2026-10-15）：按用户选择保留首页独立区块；已有缓存库 ID 时并发请求最新资源，不恢复聚合请求。首页 30 秒 freshness、共享单条 SSE、媒体缓存失效及断线 30 秒前台兜底已实现。后端补齐管理员修改及用户状态修改通知，仅广播 scope，不传业务数据。

验证：定向 Web 5 文件 / 65 项、TypeScript 和 production build 通过；事件中心库测试 7/7，`admin_events` 3/3，`user_query_events`、`resume_favorites`、`web_playback` 共 6/6 通过。Web 全量静态检查 141 passed、2 failed，完整 Vitest 613 passed、4 failed；6 个失败在原始基线同样复现，涉及未修改的卡片尺寸、触控按钮可见性、继续观看评分及旧卡片链接选择器。未修改这些无关 UI 行为或测试。

真实 Chromium 使用固定本地 API/SSE fixture：基线 65 秒产生 12 次详情周期请求，新实现 83 秒为 0；首页进入详情后 30 秒内返回为 0 次首页请求。初始 SSE 握手不会重复已加载的 item/images/playback 请求；SSE 事件立即刷新标题/收藏状态；断线约 30 秒后详情、图片、播放信息各请求一次；原生 SSE 重连触发媒体缓存重新同步并保持单条连接，随后 69 秒为 0 次周期请求。此为本机 arm64 的请求策略验证，不是生产数据集、NAS CPU 或滚动帧率结论。

Rust 完成门禁（macOS `arm64`，Toshiba 已挂载，Cargo 输出使用 `/Volumes/Toshiba/mywork/Lux/target`）：`cargo build --locked`、`cargo fmt --all -- --check` 和 `cargo clippy --locked --all-targets --all-features -- -D warnings` 通过。目标回归 `application::admin_events::tests` 7/7、`admin_events` 3/3、`user_query_events` 1/1、`resume_favorites` 4/4、`web_playback` 1/1、`scanned_series_metadata` 3/3、`scanning_jobs` 88/88 通过。

完整 `cargo test --locked --all-targets` 在 `scanned_metadata::manifest_scan_indexes_poster_while_local_nfo_is_blocked` 停滞超过 23 分钟后中断；此前 1,311 项通过、0 项失败、47 项忽略。该目标按单线程复跑为 22/23：唯一失败 `failed_local_poster_insert_does_not_mark_image_stage_complete` 得到 2 张图片、预期 1 张，是此前已有的 NFO 海报回滚问题；按 4 线程复跑为 20/23，另有 `local_nfo_retry_excludes_non_retryable_conflict_item` 的 `RowNotFound` 和 `blocked_first_local_poster_does_not_block_manifest_scan` 的 FIFO 超时。相关阶段记录也报告 FIFO 用例在并行运行时可能超过 60 秒；本轮全量测试因此不能标为通过。串行 `scanned_series_metadata`、`scanning_jobs` 的定向回归通过。

Web 冻结安装、TypeScript 和 production build 通过。完整 `pnpm test` 静态部分为 141 passed、2 failed；Vitest 为 613 passed、4 failed。六项失败均在原始代码基线 `8c8d5c13` 复现，涉及既有卡片尺寸、触控操作显示、继续观看评分和卡片链接选择器。新增/相关 65 项 Web 回归通过。自动化测试、ARM 本机和固定浏览器 fixture 结果不代表 NAS 或生产性能。未推送、合并或部署。

#### LUX-551：Emby API 路径大小写不敏感

范围：Emby 兼容路由在根路径和 `/emby` 前缀下均按 ASCII 大小写不敏感匹配。只归一化已注册 Emby 路由的静态路径片段，保留动态路径参数、query、HTTP 方法和 Lux 原生 `/api/v1` 路由语义；未知路由继续走原有 fallback。

验收：

- [x] Emby 登录、公开发现和受保护接口对静态路径段的大小写变体返回相同的 JSON/状态语义，根路径及 `/emby` 前缀均覆盖。
- [x] 动态用户/条目 ID 与 query 内容保持原值；原生 Lux API 路径不参与大小写归一化。
- [x] 自动化回归先在现有实现下失败，修复后通过；兼容性记录仅陈述本机集成测试证据，不声称第三方客户端实测通过。

预计文件（5 个）：本文档、`src/api/emby.rs`、`src/api/routes.rs`、`tests/emby_system.rs`、`docs/COMPATIBILITY.md`。

实现结果：Emby 路由表记录注册路径模板；请求进入 Axum 路由器前，只对已注册 Emby 路由的静态片段恢复其规范大小写。根路径和 `/emby` 共用同一组路由模板，动态参数、query、方法和原生 API 路径保持不变。

验证：定向 Emby 集成测试、相关播放/字幕集成测试、2 个归一化单测、构建、格式检查和 Clippy 均通过。全量 `cargo test --locked --all-targets` 在未修改的 `tests/scanned_metadata.rs::failed_local_poster_insert_does_not_mark_image_stage_complete` 失败；单独重跑仍以图片数量实际 2、预期 1 失败，故全量门未通过。未验证 MoviePilot/第三方播放器实机，也未部署 FNOS。

#### LUX-561：扫描指纹与后台 CPU 热点优化

范围：结合 CPU 热点分析的扫描指纹短路和多管线后台并发预算。扫描指纹采用经 stat 快照验证的复用与版本化 XXH3-v2；后台计算、扫描准备、NFO/图片计算、ffprobe 和缩略图共用进程级预算。性能 A/B 按项目所有者 2026-10-16 的要求免除，未据此声称实际 CPU 或 NAS 收益。

设计记录： [A+C 扫描指纹方案](SCAN-FINGERPRINT-A-C-DESIGN.md)、[原 SHA 比较记录](SHA256-FINGERPRINT-OPTIMIZATION.md)及[多管线 CPU 预算设计](SCAN-CPU-BUDGET-DESIGN.md)。同一任务的两个实现分支已合并；下列完整门禁结果以本次合并后的验证为准。

##### 扫描指纹实现

可靠的完整 stat 快照命中时复用原指纹；新文件或确认变化时写入显式版本化的 XXH3-v2。旧 SHA-v1 行按需验证并预热快照，不因算法差异触发后处理。服务就绪后由一个全局串行 worker 低速逐库迁移旧扫描 SHA；迁移前后检查 stat、验证 v1 并通过 CAS 替换规范指纹。迁移不是媒体变化。旧任务、NFO、人物和章节引用保留兼容读取；inode 不能单独判等，缓存与规范行均使用路径和指纹锚定。

当前实现包括 SQLite/PostgreSQL 前向迁移、持久游标与延期记录、每库短租约、missing 指纹清理、无引用且过期墓碑的有界清理；迁移 worker 启动延迟 60 秒，全局最多每 50ms 处理一个条目。人物关系快照中的旧引用目前由兼容读取验证，不会随规范 filesystem entry 迁移而主动重写。worker 使用固定节流和忙库让步，尚未实现基于 CPU、IO 或前台 p95 的动态降速。

待验证的指纹验收：

- [ ] v1/v2 格式和稳定编码具备 golden vectors；所有入口双读兼容后才启用 v2 写入，旧 unchanged 数据不因迁移重处理。
- [ ] 可靠暖命中不计算摘要；未知字段、不可靠根和损坏快照回退；SQLite/PostgreSQL 迁移、批量 snapshot 读取与原子提交通过。
- [ ] 移动/硬链接、并发、取消、根替换、CAS 与旧任务恢复回归通过；NFO/人物/章节合同无意外变化，内容改写检测边界明确。
- [ ] 单个全局迁移 worker 低速逐库逐文件工作；限速、资源暂停、忙库让步、断点恢复和候选索引通过，完成条目清除旧扫描 SHA，延期/失败不伪报完成。
- [ ] missing FILE 指纹立即清空；仅对无媒体源引用且所属媒体库无活跃扫描的超期墓碑分批清理，并覆盖 SQLite/PostgreSQL。
- [ ] 纯算法迁移不产生媒体变化/后处理目标；章节派生 input 与人物关系引用具备等价性迁移及崩溃恢复；普通扫描不能回写 v1。
- [ ] 完成定向与全量 Rust 检查；分别记录冷升级与暖重扫的 CPU、墙钟、SQL/DML、体积和前台 p95。

##### 后台共享预算实现

通过进程级 singleton semaphore 共用后台工作预算，容量按 CPU/cgroup 推导并支持 `LUX_BACKGROUND_CPU_BUDGET` 覆盖。Discovery 与 Manifest preparation 保留有界 reader/JoinSet；指纹、NFO/图片、probe、thumbnail 的后台 CPU 片段接入同一预算，同时保留已有专用进程上限。调用者取消后，blocking closure 和受监督子进程仍持有预算，子进程确认回收后才释放名额。指标记录全局与分 stage 活跃/等待峰值、p50/p95/max、取消等待、运行中取消及子进程清理中计数；样本有界，日志不含路径或环境变量原值。

分支合并前的记录：预算核心定向单测 9/9；`scanner` 18/18、`scanning_jobs` 88/88、`probe` 20/20、`thumbnails` 18/18、`background_work_budget` 1/1、`metadata` 21/21、`scanned_series_metadata` 3/3。`scanned_metadata` 为 22/23；唯一失败 `failed_local_poster_insert_does_not_mark_image_stage_complete` 在干净基线 `1e7c2ea8` 也以实际 2、预期 1 复现，因此全量 Rust 测试门未通过。分支上的 `cargo build --locked`、fmt 与 clippy 通过；性能 A/B 按项目所有者要求免除，CPU 收益和自动容量仍无实测结论。

三分支合并后验证（2026-10-17）：`cargo build --locked`、`cargo fmt --all -- --check`、全 targets/all features Clippy、shell 语法、两组 Python 单测、Web 冻结依赖安装与 production build 通过。`cargo test --locked --all-targets` 在 `tests/scanned_metadata.rs` 以 exit 101 终止：lib 963 passed、24 ignored；该 target 22 passed、1 failed，唯一失败仍为 `failed_local_poster_insert_does_not_mark_image_stage_complete`（实际 2、预期 1，已在干净基线复现），后续 targets 未执行。Web Node 测试为 145/148（3 项失败），单独运行的 Vitest 为 644/650（6 项失败）；失败涉及的 Web 页面和测试文件与合并前 `origin/test` 相同，这三条后端功能分支没有 Web 文件改动。完整 Rust/Web 测试门未全部通过。

后台预算验收：

- [x] 比较全局 Semaphore、管线互斥/优先级调度、共享有界线程池，并以当前源码核实 probe CPU 采样及 Tokio blocking pool 的行为。
- [x] 定义共享范围、容量、进程单例、局部并发叠加、取消/进程回收、防嵌套死锁、前台与 DB 等待隔离及指标。
- [x] 项目所有者评审并批准设计（2026-10-16）；代码接入 Discovery/Manifest、指纹、NFO/图片、probe 和 thumbnail。
- [x] 取消后的 blocking 工作和受监督子进程在清理完成前继续占用预算；设置日志与有界指标。
- [ ] 完整 Rust 完成门全部通过；性能 A/B 按项目所有者明确指示免除，不把 CPU 收益或自动默认容量作为实测结论。
##### ffprobe CPU 容量与完成速率自适应

本地 ffprobe 的自动进程预算使用 ceil(effective_cpu_cores × 4) 并限制在 1 至 512；Linux 有可见的 cgroup v1/v2 quota 与 cpuset 时取保守交集，并缓存容量结果。首次活动并发从约 CPU 核数起步，之后只按本地 ffprobe 成功完成速率调整；首页延迟和可见 cgroup 内存压力只用于保护性降档，不读取 CPU 使用率。每库配置上限、LUX_PROBE_CONCURRENCY 环境变量覆盖、分页持久化和正常关停边界保留；远程 STRM 插件仍走独立调度。容器默认不再注入固定 probe 并发 8，共享后台 CPU 预算还会限制本地 ffprobe 实际活动量。

实现方案与分步记录见 [FFPROBE-CONCURRENCY-DESIGN.md](FFPROBE-CONCURRENCY-DESIGN.md) 和 [FFPROBE-CONCURRENCY-CODE-PLAN.md](FFPROBE-CONCURRENCY-CODE-PLAN.md)。子进程所有权由独立 runner 管理；自适应 probe permit 与共享后台 CPU permit 都在确认进程退出后释放。

ffprobe 分支的验证记录：定向探测器测试通过；cargo build --locked、fmt 和全 targets/all-features Clippy 通过。该分支全量 Cargo 测试在 lib 阶段 914 passed、1 failed、23 ignored 后停止，唯一失败为旧断言期待 1/8 步长下并发 6；更新断言后该定向用例通过。之后的集成 targets 未执行，Linux 交叉构建因缺少 x86_64-linux-gnu-gcc 未启动。ARM64 模拟负载发现忙碌时重置 sleep 导致采样 tick 不推进，修复为固定 tick；后续将上调步长改为 1/4，但真实 ffprobe A/B 尚未测量。具体数据和限制见 docs/PERFORMANCE.md；本机结果不外推为 NAS/x86 性能。

#### LUX-562：动态 SQL 构建与字符串分配基线

范围：为 CPU 热点分析第五点建立可重复的局部基线；只使用合成输入，不访问线上 FNOS 数据库，不改变生产查询行为。后续 SQLite SQL 转交、PostgreSQL 占位符改写、有限 SQL 模板复用和业务字符串借用仍须各自登记为后续任务。

设计记录：[动态 SQL 构建与字符串分配优化设计](DYNAMIC-SQL-OPTIMIZATION-PLAN.md)。基线源码为 `origin/test` 提交 `e7107bfe`。测试专用分配计数器只在 `performance-benchmarks` feature 下编译，测量窗口外关闭；普通生产构建不引入该 allocator。

验收：

- [x] ignored release harness 分别测量 SQLite 静态/owned SQL 适配、PostgreSQL 120 占位符改写、1/100/200/2,900/5,000 行 VALUES CTE 构造、200 条不同长度路径的 Any 参数绑定，以及 SQLite 单连接 statement-cache 冷执行/热重放/淘汰重放。
- [x] 在 Toshiba 共享 Cargo target 上完成五轮；逐项报告分配数/字节与耗时的中位数和范围，报告 120 种 SQL 形状相对 100 条缓存容量的驻留行为。
- [x] 记录本机架构、Rust 版本、基线提交和可复跑命令；明确结果仅是本机合成微基准，不外推 NAS/x86 性能，也不声称扫描整链路收益。
- [x] 定向 release 基准、格式、普通构建和 all-targets/all-features Clippy 通过；普通 `cargo build --locked` 不启用测试 allocator。
- [x] 执行并记录 `cargo test --locked --all-targets`。该命令在 `scanned_metadata` 停止：`failed_local_poster_insert_does_not_mark_image_stage_complete` 在干净 `e7107bfe` 基线也以实际 2、预期 1 复现；`manifest_scan_indexes_poster_while_local_nfo_is_blocked` 在并行全量运行中失败，但隔离重跑通过。不将全量 Rust 门禁报告为绿。

预计文件（4 个）：`src/storage/sql_query_benchmarks.rs`、`docs/PERFORMANCE.md`、本文档、`docs/DYNAMIC-SQL-OPTIMIZATION-PLAN.md`。基准模块挂接已在 LUX-562 工作分支的基线 harness checkpoint 中完成。

实施记录（2026-10-16）：Toshiba 已挂载，本机 `arm64`，Rust `1.97.1`。五轮 release 微基准分配数/字节稳定；静态 SQLite SQL adapter 的指针未保留、每次分配 81 字节；PostgreSQL 120 参数 adapter 为 121 次分配、1,615 字节；5,000 行 VALUES CTE 构造为 105,114 字节。五轮中位数、范围、缓存 100/120 形状驻留结果与复跑命令见 `docs/PERFORMANCE.md`。定向 ignored benchmark 通过；`cargo fmt --all -- --check`、普通 `cargo build --locked`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 通过。全量 `cargo test --locked --all-targets` 的 lib target 为 929 passed、0 failed、23 ignored；随后 `scanned_metadata` 为 21 passed、2 failed，后续 targets 未执行。已在干净 `e7107bfe` worktree 复现其中的既有 poster 计数失败，并隔离复跑通过慢 NFO 测试。`tests/performance.rs` 编译时有两条既有 dead-code warning；它们不影响 Clippy，未触及相关测试代码。本任务没有生产查询行为改动，完整 Rust 全量门禁仍非绿色。

#### LUX-563：SQLite SQL 直传与 PostgreSQL 占位符改写分配优化

范围：优化查询入口的 SQL 字符串适配；SQLite 直接转交 SQLx `SqlStr`，PostgreSQL 仅在确有未引用 `?` 时构造改写文本，并在同一个 `String` 中写出 `$N`，不为每个编号建立临时 `String`。同步更新迁移查询入口和 LUX-562 合成基准夹具。绑定值、SQL 引号解析规则、查询语义和数据库 schema 保持不变。不在本任务实现模板缓存或 IN 分桶。

预计文件（5 个）：`src/storage/repository.rs`、`src/storage/migration.rs`、`src/storage/repository_tests.rs`、`src/storage/sql_query_benchmarks.rs`、本文档。

验收：

- [x] SQLite 对静态 `&str`、owned `String` 与 `Arc<str>` 保持原 SQL 缓冲区地址；对应测试先失败再通过。
- [x] PostgreSQL 对无占位符 SQL 保留原地址；改写覆盖单双引号内问号、转义单引号和 `$10`–`$13` 多位编号。
- [x] repository 三种查询入口及迁移查询使用新的 `SqlStr` 返回值；adapter 定向单测与 `server_id_survives_database_reopen` 通过。
- [x] 重跑 LUX-562 release 基准，记录静态/owned SQLite adapter 与 120 参数 PostgreSQL adapter 的分配变化；其余基线项目不变。
- [x] `cargo fmt --all -- --check`、定向测试和任务完成阶段规定的全量门禁均有明确结果；既有基线失败单独报告，不称全量测试通过。

微基准记录（2026-10-16）：本机 `arm64`、Rust `1.97.1`，release harness 五轮，安静窗口内直接复跑编译后的测试二进制。静态 SQLite adapter 为 0 allocations / 0 B，时间中位数 2 ns（2–6）；owned SQLite adapter 为 0 / 0 B，中位数 10 ns（8–13）；PostgreSQL 120 参数改写为 1 allocation / 887 B / 0 reallocations，中位数 1,304 ns（1,301–1,367）。相对 LUX-562 基线，静态 SQLite 从 1 allocation / 81 B 降为零；PostgreSQL 从 121 allocations / 1,615 B / 1 realloc 降为 1 / 887 B / 0。VALUES CTE、Any 参数绑定及 statement-cache 行为不属于本步优化，五轮结果保持在基线波动范围内。本机合成微基准不外推为 NAS/x86 或扫描整链路收益。

完成门禁记录（2026-10-16）：`cargo build --locked`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 均通过。`cargo test --locked --all-targets` 的 lib target 为 931 passed、0 failed、23 ignored；随后在 `tests/scanned_metadata.rs` 为 21 passed、2 failed 并停止，后续 targets 未执行。`failed_local_poster_insert_does_not_mark_image_stage_complete` 隔离重跑仍为实际 2、预期 1，且已在干净 `e7107bfe` 基线复现（见 LUX-562）；并行失败的 `manifest_scan_indexes_poster_while_local_nfo_is_blocked` 隔离重跑通过。全量 Rust 测试门禁明确为非绿色。

#### LUX-564：有限 SQL 模板缓存基础

范围：建立 storage 内部、仅供受控查询族使用的完整 SQL 模板缓存。模板键固定包含有限查询族、后端、枚举版本与形状；缓存共享最终 SQL 的 `Arc<str>`，模板生成在单个 `String` 中完成并验证绑定预算。每个查询族/后端/版本保留满批模板和最多 4 个尾批模板；总缓存文本不超过 8 MiB，另有 256 条全局应急上限。构造发生在缓存锁外，锁毒化时退回未缓存构造。此任务只实现缓存/构造器基础，不添加尚无调用者的 repository 查询入口，不改扫描查询调用点，不引入任意 SQL 字符串键、数据库 schema 或新依赖。

预计文件（5 个）：`src/storage/sql_templates.rs`（新增并含单测）、`src/storage/mod.rs`、`src/storage/sql_query_benchmarks.rs`、本文档、`docs/DYNAMIC-SQL-OPTIMIZATION-PLAN.md`。首个专用 repository 查询入口在后续接入真实 filesystem 读取时一同实现。

验收：

- [x] 非法/越界模板形状在构造前拒绝；SQLite 与 PostgreSQL 完整 SQL 文本及绑定编号正确；实际绑定数不匹配时拒绝发布。
- [x] 相同有限键重复/并发请求最终复用同一 `Arc<str>`；SQL 构造不在缓存锁内，毒化锁退回未缓存构造。
- [x] 每个模板族仅保留满批与最多 4 个尾批；文本预算不超过 8 MiB、全局条目数不超过 256；超预算模板可用但不留存在缓存中。
- [x] 基准比较缓存命中和 SQL 构造成本；实际查询入口与 query counter 在后续工作包按调用点验收。
- [x] 定向单测、模板构造基准、格式、构建与 Clippy 有明确结果；本任务全量门禁按阶段记录失败，不称其为绿色。

微基准记录（2026-10-16）：本机 `arm64`、Rust `1.97.1`，release harness 五轮，直接复跑编译后的测试二进制。PostgreSQL 128 个路径参数（另含 root 参数）的 adapter 改写耗时中位数 1,488 ns（1,484–1,613），每次 1 allocation / 981 B / 0 reallocations；缓存命中为 16 ns（16–17）、0 allocations / 0 B；冷构造并发布为 1,192 ns（1,172–1,291）、4 allocations / 2,156 B / 0 reallocations。热命中消除了逐请求 SQL 生成和分配；冷构造仍比 adapter 快，但保留一次性构造成本。该构造微基准不包含数据库执行，不代表查询或扫描整链路收益，也不外推 NAS/x86。

完成门禁记录（2026-10-16）：`cargo test --locked --lib storage::sql_templates::tests -- --test-threads=1` 为 9 passed；`cargo build --locked`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 均通过。`cargo test --locked --all-targets` 的 lib target 为 940 passed、0 failed、23 ignored，之前的若干集成 target 通过；在 `tests/libraries_api.rs` 并行运行时，`admin_can_create_list_and_add_library_root_with_csrf` 一次收到 503 `DATABASE_UNAVAILABLE`（预期 200），测试 target 因此中止。该用例隔离运行及该 target 串行运行均通过；干净 `e7107bfe` 基线的并行 target 三次也通过，候选分支之后的并行复跑通过。当前证据不足以确认这次并行失败的根因，也不足以判定为代码回归；全量门禁仍明确为非绿色，后续完成门禁需继续观察并报告，不能把未运行的后续 target 算作通过。

#### LUX-565：filesystem 路径批量查询复用模板

范围：将 `list_filesystem_entries_for_paths` 从每批动态 `format!`、占位符 `Vec` 和 `join` 改为 S2 的有限 SQL 模板缓存。按路径数量选 power-of-two IN 桶（超过 256 时使用现有 500 项上限），不足桶容量的末尾参数绑定 typed NULL；每批仍最多 500 条、仍发起一次查询，保留 SQL 投影、绑定顺序、root 隔离和返回 map 语义。不修改其他扫描查询族，不调整批次大小、事务或公共接口。

预计文件（5 个）：`src/storage/filesystem.rs`、`src/storage/repository.rs`、`src/storage/sql_templates.rs`、`src/storage/repository_tests.rs`、本文档。

验收：

- [x] 1、3/4、256/257、500/501 路径数边界与缺失路径结果一致；相同相对路径在其他 root 的记录不泄漏。
- [x] 输入 0 条路径不执行查询；1–500 条仍一次查询，501 条仍两次查询，`query_count` 与旧实现一致。
- [x] 绑定先 root 后 paths，padding 使用 typed NULL；SQLite 实际查询通过，SQL 模板缓存命中且热路径不再拼 SQL 或分配占位符集合。
- [x] 定向 repository 测试先红后绿；格式、构建与 Clippy 结果明确，全量门禁的失败按证据报告。

微基准记录（2026-10-16）：本机 `arm64`、Rust `1.97.1`，release harness 五轮、每轮每种构造 50,000 次。对 127 个路径，旧式 exact SQL 构造中位数 327 ns（297–551）；查询 bucket 128 后的缓存命中（含 bucket 选择）为 18 ns（18–26）。这是 SQL 构造时间对照，不包含 SQLite/SQLx 执行，也不测数据库计划或整链路；typed NULL 对执行耗时的影响留待最终阶段验证。A/B 使用 `performance-benchmarks` feature 下默认忽略的 `filesystem_path_template_construction_benchmark`。

完成门禁记录（2026-10-16）：定向 `filesystem_path_reads_reuse_bucketed_templates_without_extra_queries` 为 1 passed，S2 模板单测为 10 passed；release A/B harness 为 1 passed。`cargo build --locked`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 均通过。`cargo test --locked --all-targets` 的 lib target 为 942 passed、0 failed、23 ignored；到 `tests/libraries_api.rs` 时，13 项通过，`admin_can_create_list_and_add_library_root_with_csrf` 再次在并行运行中收到 503 `DATABASE_UNAVAILABLE`（预期 200），后续 targets 未执行。该 target 串行运行 14/14 通过；同一个失败在 S2 全量门禁中已先出现，故不能归因于 LUX-565。随后在干净基线 `e7107bfe` 按“lib target 完成后立即并行运行 `libraries_api`”复核：lib 为 929 passed、0 failed、23 ignored，`libraries_api` 为 14/14 通过；因此该 503 未能在基线复现，根因仍未查明，完整 Rust 门禁仍为非绿色。PostgreSQL 模板编号有单测覆盖，本步没有独立 PostgreSQL 实例验证实际 NULL 绑定。

#### LUX-566：manifest 基线路径查询复用模板

范围：将 `list_scan_manifest_filesystem_baselines` 从每批动态占位符 `Vec`、`join` 和 `format!` 改为专用有限 SQL 模板。按后端路径数量选择不小于输入数的最小 power-of-two IN 桶，超过最后一个二次幂时使用该后端现有批次上限（SQLite 2,900、PostgreSQL 5,000）；不足桶容量的路径绑定 typed NULL。每批仍按原后端上限分块并执行一次查询，保持 root 过滤、投影、绑定顺序、空输入、HashMap 结果语义和查询计数。不改 manifest 批次、事务或其他查询族。

预计文件（5 个）：`src/storage/jobs/scan/manifest/diff.rs`、`src/storage/sql_templates.rs`、`src/storage/repository_tests.rs`、本文档、`docs/DYNAMIC-SQL-OPTIMIZATION-PLAN.md`。

验收：

- [x] 空输入不查询；单批、分桶两侧、SQLite 最大批次与跨批次结果和旧实现一致，缺失/重复路径不产生额外结果。
- [x] root 隔离、完整 baseline 字段、每个 chunk 一次查询及 query counter 保持；SQLite typed NULL 实际查询通过。
- [x] SQLite 与 PostgreSQL 模板固定参数、IN 容量、占位符编号和绑定顺序正确；容量不超过原后端批次上限。
- [x] repository 行为测试先红后绿；SQL 构造 A/B、格式、构建与 Clippy 有明确记录，全量门禁失败按证据报告。

微基准记录（2026-10-16）：本机 `arm64`、Rust `1.97.1`，release harness 五轮、每轮每种构造 50,000 次。257 个路径的旧式精确 SQL 构造中位数 600 ns（592–918）；计算桶容量并命中 512 项模板的中位数 18 ns（18–22）。结果只包括 SQL 构造，不包括 SQLite/SQLx 执行、绑定和分配计数；大容量 NULL 绑定对数据库执行的影响留待 S7 整链路复测。A/B 使用 `performance-benchmarks` feature 下默认忽略的 `manifest_baseline_template_construction_benchmark`。

完成门禁记录（2026-10-16）：`cargo test --locked --lib manifest_baseline -- --test-threads=1` 为 2 passed；`cargo test --locked --test scanning_jobs` 为 88 passed。`cargo build --locked`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 和 release A/B harness 均通过。`cargo test --locked --all-targets` 的 lib target 为 944 passed、0 failed、23 ignored；`tests/libraries_api.rs` 为 14/14。随后 `tests/probe.rs` 有 20 passed、1 failed：`probe_runner_terminates_when_stderr_exceeds_the_runtime_limit` 超过测试超时；该用例隔离重跑通过，probe target 串行运行 21/21。其余后续 targets 未执行，因此完整 Rust 门禁仍记为非绿色。失败发生在本任务未修改的 probe 代码，现有证据未能确认其根因或证明基线表现。默认 all-targets 未运行 PostgreSQL 实例；PostgreSQL 模板文本和绑定编号由单测验证，运行时 PG 验证留待 S7。

#### LUX-567：manifest 发现路径与 presence 模板

范围：缓存 manifest 发现的旧格式/V3 known-path 查询，以及 V3 presence 更新、部分成功回读、seen-path ledger 插入的精确行数 SQL 模板。模板按后端原有路径批次上限分块，不改查询次数、语义、绑定顺序或事务。全成功更新时借用输入路径；部分成功时只将回读结果持有为 owned 字符串。支持 PostgreSQL presence update 的 35,002 个绑定，并保留 SQLite 绑定上限。

预计文件（5 个）：`src/storage/jobs/scan/manifest/discovery/paths.rs`、`src/storage/jobs/scan/manifest/discovery/presence.rs`、`src/storage/sql_templates.rs`、`src/storage/repository_tests.rs`、本文档。

验收：

- [x] SQL 模板覆盖旧格式和 V3 known-path、V3 update/readback/ledger insert；后端占位符、参数顺序、边界绑定数正确，模板复用且缓存有界。
- [x] SQLite 行为测试覆盖 known path、更新全成功与部分成功回读、seen-path 写入、格式版本切换及计数结果；原扫描语义和 query counter 保持。
- [x] `successfully_seen_paths` 在全成功路径借用输入，部分成功回读使用 owned 数据；无生命周期扩大或公共 API 变化。
- [x] 定向测试、格式、构建、Clippy 与完整门禁按实际结果记录；PostgreSQL 运行时验证留至 S7。

完成门禁记录（2026-10-16）：manifest SQL 模板单测 4/4、presence repository 回归 1/1、`tests/scanning_jobs.rs` 88/88 通过；`cargo build --locked`、`cargo fmt --all -- --check` 和 `cargo clippy --locked --all-targets --all-features -- -D warnings` 通过。`cargo test --locked --all-targets` 中 lib target 为 948 passed、0 failed、23 ignored，`libraries_api` 为 14/14，`probe` 为 21/21；随后 `tests/scanned_metadata.rs` 有 22 passed、1 failed，失败为 `failed_local_poster_insert_does_not_mark_image_stage_complete`。在未包含 LUX-567 的基线提交 `3b4b7325` 上串行重跑该 target，同一用例以相同断言失败（22/23），确认该失败先于本任务存在。当前分支串行 target 另有一次 `local_nfo_retry_excludes_non_retryable_conflict_item` 的 `RowNotFound`，该用例隔离重跑通过，基线串行 target 也通过。完整 all-targets 门禁仍为非绿色；PostgreSQL 运行时验证留待 S7。

#### LUX-568：Manifest 目录 frontier SQL 模板

范围：缓存 `insert_manifest_directory_frontier` 的精确行数 `VALUES` 模板，以及 `complete_manifest_directory_frontier` 的精确行数 `IN` 更新模板。沿用 `SCAN_DML_CHUNK_SIZE`、lite mode 早退、冲突忽略、`state <> 'COMPLETE'`、固定参数和绑定顺序、原 transaction 与 rows-affected 计数；不补假行、不增加查询、不调整批次或事务。

预计文件（5 个）：`src/storage/jobs/scan/manifest/discovery/frontier.rs`、`src/storage/sql_templates.rs`、`src/storage/jobs/tests.rs`、`src/storage/sql_query_benchmarks.rs`、本文档。

验收：

- [x] SQLite/PostgreSQL 模板覆盖 1/满批/尾批行数，绑定编号和顺序正确，同键复用缓存且缓存保持有界；超过现有 200 行批次上限时拒绝模板形状。
- [x] SQLite 回归覆盖 201 个目录的跨批插入、冲突重放、完成状态及完成计数；查询次数与旧实现相同，lite mode 仍不写 frontier。
- [x] 记录满批与尾批 SQL 构造的动态基线/模板热命中 A/B；定向测试、格式、构建、Clippy 与完整门禁按实际结果记录。PostgreSQL 运行时验证留待 S7。

当前实现记录（2026-10-16）：定向模板测试 3/3、`tests/scanning_jobs.rs` 88/88 通过；`cargo build --locked`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 和 `git diff --check` 通过。满批 200 行构造：INSERT 动态基线中位数 844 ns、3 次分配/7,996 B，模板热命中 23 ns、0 次分配；COMPLETE 更新基线 474 ns、3 次分配/4,234 B，模板 23 ns、0 次分配。尾批 17 行：INSERT 基线 177 ns、3 次分配/1,042 B，模板 24 ns、0 次分配；COMPLETE 基线 109 ns、3 次分配/757 B，模板 24 ns、0 次分配。五轮本机 arm64 release 数据仅代表 SQL 文本构造，不含 SQLx/SQLite 执行、扫描整链路或 NAS 性能。PostgreSQL 运行时和一次完整 all-targets 门禁统一留至 S7；本工作包期间启动的重复 all-targets 测试按用户要求中断，未形成新的全量结果。

#### LUX-569：Manifest delta SQL 模板

范围：缓存 delta 基线冲突校验 CTE、delta INSERT/RETURNING、finalizer state/error CASE 更新三个精确行数模板。沿用 100 行 chunk、同一 transaction、校验与冲突分类、ON CONFLICT DO NOTHING、RETURNING 计数、PENDING CAS、affected-row guard、attempt_count、参数顺序和批次边界；CASE SQL 不再通过克隆整段 `String` 生成第二份。

预计文件（5 个）：`src/storage/jobs/scan/manifest/diff/insert.rs`、`src/storage/jobs/scan/manifest/apply/finalize.rs`、`src/storage/sql_templates.rs`、`src/storage/sql_query_benchmarks.rs`、本文档。

验收：

- [x] SQLite/PostgreSQL 模板覆盖首行、满批与尾批，绑定编号和顺序一致，缓存复用且尾部形状有界，超过 100 行拒绝模板。
- [x] 现有 SQLite 回归覆盖 250 行跨批 delta、同值重放、冲突重试、malformed ADD；apply 覆盖 bounded batch、增量写入 CAS、失败回滚和 cancel/resume，attempt_count 与计数不变。
- [x] 记录满批/尾批 SQL 构造 A/B 与定向测试、格式、构建、Clippy 的实际结果；完整门禁和 PostgreSQL 运行时验证留至 S7。

结果（2026-10-16）：verify/INSERT/finalize 使用精确行数模板，fallback、bind 顺序和事务边界保持。模板单测 2/2、250 行 delta 幂等/冲突回归 1/1、`tests/scanning_jobs.rs` 88/88 通过；`cargo build --locked`、`cargo fmt --all -- --check`、全 targets/all features Clippy 和 `git diff --check` 通过。单次 release A/B（arm64，full 100 行 2,000 次、tail 17 行 5,000 次）结果为平均单次构造时间：verify 满批动态 1,056 ns（3 alloc/6,364 B，另 1 realloc/4,932 B），模板命中 57 ns（0 alloc）；INSERT 满批 468 ns（3/4,822 B，另 2 realloc/8,409 B），模板 24 ns（0 alloc）；finalize 满批 878 ns（6/6,838 B，另 3 realloc/10,192 B），模板 24 ns（0 alloc）。尾批 verify 156 ns（3/3,127 B）→25 ns，INSERT 177 ns（3/1,336 B，另 1 realloc/1,248 B）→30 ns，finalize 565 ns（6/1,609 B，另 1 realloc/1,084 B）→45 ns，模板命中均 0 alloc。该 A/B 仅测 SQL 文本构造，不含数据库执行、扫描整链路或 NAS。完整 all-targets 与实际 PostgreSQL 运行时验证留至 S7。

#### LUX-570：Manifest 正向索引借用媒体文件记录

范围：已成功 claim 的 ADD 电影/剧集文件以 `&NewMovieFile` / `&NewEpisodeFile` 传入媒体物化与预取 helper，移除 `indexes.rs` 中整条文件记录的 `.clone()`。已有 owned-slice 调用方通过一次引用列表适配继续使用；不改变 claim 胜者、item/source 归并、层级关系、事务边界或公共 API。

预计文件（5 个）：`src/storage/jobs/scan/manifest/indexes.rs`、`src/storage/media.rs`、`src/storage/repository_tests.rs`、`src/storage/sql_query_benchmarks.rs`、本文档。

验收：

- [x] 正向索引只收集已 claim 的借用 Movie/Episode 记录；媒体 helper 内部消费引用列表，不复制文件记录中的 String 和 fingerprint Vec。
- [x] SQLite 回归覆盖 claim 竞态、重复电影来源/文件夹与 provider ID 合并、剧集/季度/分集层级及默认来源选择。
- [x] release 对比记录 clone 和引用列表的分配次数/字节；定向测试、格式、构建、Clippy 按实际结果记录。完整门禁与 PostgreSQL 运行时验证留至 S7。

结果（2026-10-17）：正向索引直接传入 Movie/Episode 借用记录，原有 owned-slice helper 通过一次 `Vec<&File>` 轻量适配，业务 SQL、排序和事务行为不变。借用 Movie helper 1/1、借用 Episode hierarchy helper 1/1 通过。`tests/scanning_jobs.rs` 目标为 87 passed、1 failed；claim race、重复电影来源/provider ID、streamed positive index 等相关用例通过，唯一失败 `manifest_scan_queues_attachment_removal_only_after_root_confirmation` 报 SQLite `database is locked`。按用户要求未隔离重跑。`cargo build --locked`、fmt、全 targets/all features Clippy 和 `git diff --check` 通过。单次 release A/B（arm64，256 records、100 次）中 Movie clone 为 73,408 ns、3,073 alloc/150,928 B；引用列表 70 ns、1 alloc/2,048 B。Episode clone 为 97,378 ns、3,841 alloc/204,660 B；引用列表 42 ns、1 alloc/2,048 B。测量只比较记录集合构造，不含 SQLite 或扫描整链路。完整 all-targets 与 PostgreSQL 运行时验证留至 S7。

#### LUX-571：动态 SQL 优化完成门禁与综合记录

范围：完成 S0–S6 后进行一次 Rust 全目标门禁，确认 PostgreSQL 测试环境，汇总 A+C 方案的局部性能证据与未覆盖边界。没有新增代码改动。

预计文件（3 个）：`docs/PERFORMANCE.md`、本文档、`docs/DYNAMIC-SQL-OPTIMIZATION-PLAN.md`。

验收：

- [x] 记录 A+C 结论、B 的取舍、已完成的分项 A/B 和本机/微基准测量边界。
- [x] 只运行一次最终 `cargo test --locked --all-targets`，如实记录失败 target 和未执行的后续 targets。
- [x] 检查并记录 PostgreSQL runtime 可用性；无本地测试实例时标记未验证，不将 ignored tests 视为通过。
- [ ] 完整 Rust all-targets 门禁全部通过。
- [x] 实际 PostgreSQL runtime 集成测试通过。

结果（2026-10-17）：最终 Rust 命令使用 `/Volumes/Toshiba/mywork/Lux/target`，运行一次后在 `tests/scanned_metadata.rs` 以 exit 101 结束。lib 为 955 passed、0 failed、23 ignored；该集成 target 为 21 passed、2 failed：`failed_local_poster_insert_does_not_mark_image_stage_complete`（实际 2、预期 1；此前已在基线 `3b4b7325` 重现）与 `local_nfo_retry_excludes_non_retryable_conflict_item`（`RowNotFound`；此次不重跑）。此前完成的 `libraries_api` 14/14、`probe` 21/21；其余后续 targets 未执行。S6 代码后的 `cargo build --locked`、`cargo fmt --all -- --check` 和全 targets/all features Clippy 已通过，文档收尾未重复运行。PostgreSQL 使用只绑定 `127.0.0.1:55432` 的临时 16 版容器运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test postgres_database -- --ignored --test-threads=1`，18/18 通过；随后容器已删除且端口不再监听。完整 Rust 门禁仍未通过；PostgreSQL runtime 项已完成。局部性能数据仅证明 SQL 文本/记录集合构造下降，不代表扫描整链路或生产 CPU 收益。

#### LUX-561：优化 Manifest 删除反连接与分页扫描

范围：针对 CPU 热点分析第六项，在不改变 Manifest、文件路径和删除安全合同的前提下，降低删除候选分页重复枚举，并将只用于判断完成与否的差异统计改为存在性检查。format 3/LITE、PERSISTED v3、workflow 2/3 与 legacy 路径均须保留各自语义。seen-path/delta 继续使用完整复合键反连接。先用 SQLx 双后端 fixture 建立可复现基线，再以固定 root、相对路径 keyset 查询复用 `(library_root_id, relative_path)` 现有索引；不默认增加索引。

验收：

- [x] EXISTS 完成检查与原差异集合一致；精确报告计数保持；root 完整性、取消/job fencing 与事务边界不变。
- [x] 各格式删除候选集合与基线逐项相同；跨 root 同名路径、多页、直接子项、通配符路径、父目录缺失、uncovered 路径、并发增量、取消恢复和 CAS 均有覆盖。
- [x] SQLite 实际运行计划与固定 A/B fixture 已验证；完整扫描及前台请求所有观测中位数均在 5% 回退门内。PostgreSQL、FNOS 无本次运行时证据，详见限制，不外推。
- [x] 实测未证明新增 generation 索引能带来净收益；继续使用现有索引，无 schema/migration 变更。
- [x] 定向 `storage`/`scanning_jobs` 测试、性能记录、fmt、build、全目标 Clippy 与全量 Rust 完成门结果均已记录；全量门有基线失败，未标为通过。

方案和增量文件计划：[`NOT-EXISTS-OPTIMIZATION-PLAN.md`](NOT-EXISTS-OPTIMIZATION-PLAN.md)。storage SQLx 基线及语义回归位于 `src/storage/jobs/tests.rs`，端到端分页回归在 `tests/scanning_jobs.rs`；查询实现位于 `src/storage/jobs/scan/manifest/diff.rs`、`src/storage/jobs/scan/manifest/diff/finish.rs` 和 `src/application/scanner/jobs/manifest_index.rs`。完整扫描 A/B 复用 `tests/performance.rs`。结果更新到本文、`docs/PERFORMANCE.md` 与 `docs/COMPATIBILITY.md`。若索引增量单独证明有必要，再按新增 migration 单独拆分并验证空库与旧库升级。

### 增量顺序

1. 在未修改查询上运行可复现的 SQLx 基线，记录实际后端版本、查询计划与代表性规模数据。
2. [x] 将四种差异完成检查改为 EXISTS，并补齐各格式语义回归。
3. [x] 将删除候选调度改为固定 root 的 keyset page；保留各版本目录覆盖和 generation/observation 条件，并以端到端场景验证差异集合和 finish retry。
4. [x] 交错测量基线与实现；只有整 root missing 更新被证明为独立热点时，才评估额外索引。
5. [x] 执行全量完成门并记录双后端、ARM64、FNOS/x86 的证据边界。

增量 A 进度（2026-10-16）：四种完成检查现以 `CASE WHEN EXISTS` 返回 0/1；workflow 1 的 observed 与 filesystem 两类候选保持独立短路判断，精确 `unchanged_count` 查询未改。LITE、PERSISTED v3、workflow 2 format 2 和 workflow 1 候选/delta 行为回归通过。SQLx SQLite 对照显示首个候选在文件顺序前部时 COUNT/EXISTS 中位数 86.1/0.91 ms，唯一候选在末尾时为 30.1/19.4 ms，无候选时为 34.2/34.2 ms；仅为本机 60k 合成库查询样本，不代表完整扫描收益。完整原始样本见 `docs/PERFORMANCE.md`。

增量 B 进度（2026-10-16）：候选现在先用有界 COMPLETE-root 页分组，再在固定 root 内按 `relative_path` 标量游标读取；PERSISTED v3 每次处理同 root 的最多 128 个 COMPLETE 目录，并为每个目录页单独推进候选游标。storage 回归覆盖两 root 同名路径、多页、seen/delta/current-generation 排除、目录直接子项与根目录、uncovered 路径、`A`/`AB`、`%`/`_`/反斜杠和中文。`cargo test --locked --test scanning_jobs` 88/88 通过。SQLx SQLite 的固定 root 计划使用已有 `(library_root_id, relative_path)` 唯一索引范围并取消临时排序；候选枚举和 delta 写入的 A/B 样本及波动见 `docs/PERFORMANCE.md`。未新增 migration 或 generation 索引。

验收结果（2026-10-16）：EXISTS 完成检查保留 workflow 1 两种候选来源的独立检查；精确 `unchanged_count` 未改。LITE、PERSISTED v3、workflow 2 format 2 与 workflow 1 的差异集合和 finish/retry 行为由 storage 与 `scanning_jobs` 回归覆盖。固定 root/path keyset 使用现有唯一索引，seen/delta 仍使用完整复合键反连接；无新增索引或迁移。完整 LUX-270 fixture 的 8 轮交错中位数：首扫 2,383.5/2,406.5 ms、无变化重扫 1,167.5/1,185.5 ms、前台 p95 338/339 ms、目录列表 p95 578/591.5 ms、热目录列表 p95 431.5/423.5 ms、batch p95 334.5/346 ms（基线/候选）；最大观测回退 3.44%，在 5% 门内。完整样本见 `docs/PERFORMANCE.md`。

代码验证：`cargo test --locked --lib manifest_removal_candidates_page_within_root_and_preserve_directory_scopes`、`cargo test --locked --test scanning_jobs`（88/88）、`cargo build --locked`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 均通过；以上 Cargo 命令均将 target 指向 `/Volumes/Toshiba/mywork/Lux/target`，本机 `arm64`。`./scripts/check-all.sh` 的 Rust 全量测试未通过：串行执行到未修改的 `tests/scanned_metadata.rs::failed_local_poster_insert_does_not_mark_image_stage_complete` 时，插入 2 张海报、断言预期 1 张；在干净基线 `1e7c2ea8` 上的同一精确测试得到相同失败。合并参数对象后的全目标重跑另在 `login_background_plugin_is_refreshed_in_worker_and_served_from_cache` 遇到 15 秒超时，单独精确重跑通过（1/1）。并行执行另曾出现 `libraries_api` 的临时 503，隔离及串行目标重跑通过（14/14）。因此全量 Rust 完成门如实记录为未通过，现有证据不将这些故障归因于 LUX-561。Web 冻结安装和 production build 通过；当前工作树 `pnpm test` 静态阶段为 145/148，Vitest 单独运行是 650/655；这些失败涉及未修改的 Web 页面/CSS/组件查询用例，未在本任务归因或修复。未验证 PostgreSQL runtime、FNOS/x86_64、生产 NAS 或部署行为。

#### LUX-572：重启后恢复未完成的元数据识别任务

范围：关闭或重启仍统一保留 `SERVER_SHUTDOWN` 任务历史，但下次启动恢复原元数据任务 ID，不为同一批未完成工作重复创建整库任务。原子地把遗留 `RUNNING/PENDING` 子项（以及迁移后标为 `FAILED/SERVER_SHUTDOWN` 的子项）重新排队；已完成子项不重跑，管理员主动取消和其他终态不恢复。`FILL_MISSING` 只有媒体库仍启用且仍配置主刮削器时恢复；指定条目任务沿用当前媒体库/刮削器选择处理，不可用条目在任务处理中完成为明确跳过结果。继续使用有界 dispatcher，不等待识别完成后才启动 HTTP 服务。

预计文件（6 个）：`docs/LUX-DEVELOPMENT.md`、`docs/decisions/034-cancel-jobs-on-shutdown.md`、`src/api/legacy.rs`、`src/application/reidentify.rs`、`src/storage/jobs/metadata_reidentify/lifecycle.rs`、`tests/reidentify.rs`。

验收：

- [x] 仅 `CANCELLED/SERVER_SHUTDOWN` 的元数据任务恢复原任务 ID；恢复事务同时更新父任务和未完成子项，重复调用幂等。
- [x] 已完成子项不重复请求；`PENDING/RUNNING` 和迁移后 `FAILED/SERVER_SHUTDOWN` 子项恢复；管理员主动取消或无可用刮削器时不创建待运行任务。
- [x] `ITEMS` 与库级 `FILL_MISSING` 按各自范围派发；整库任务仍串行，恢复筛选和状态更新有界、幂等。
- [x] 更新 ADR-034 与 §13.5 重启语义；无 schema 变化。定向测试、build、fmt 和 Clippy 结果见本任务记录。

验证：`cargo test --locked --test reidentify -- --test-threads=1`（24/24）、`cargo fmt --all -- --check`、`cargo build --locked`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 均通过；所有 Cargo build/test/clippy 命令将 `CARGO_TARGET_DIR` 设为 `/Volumes/Toshiba/mywork/Lux/target`。本机为 `arm64`；未验证 FNOS/x86_64 与生产部署，完整 `--all-targets` 测试门禁在这组关联修复完成后统一运行。

#### LUX-573：刮削器恢复后自动重试延期的补缺任务

范围：LUX-402 已为带请求快照的自动 `FILL_MISSING` 失败项保存 5 分钟、30 分钟、最多 6 小时的退避，但当前只有后续扫描会重新提交这些项。利用现有 15 秒后台调度轮询到期的自动延期项，从已保存的媒体完整度生成当前指纹与能力集合，复用请求快照去重事务创建自动重试任务，再经每媒体库有界 dispatcher 执行并消费旧失败记录。仅处理 `AUTOMATIC` 来源、未消费且带请求快照的失败；新尝试继承逐项失败次数，并确认媒体库仍启用且仍选中刮削器。不得触发整库扫描或人工/其他错误重试。

预计文件（7 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/reidentify.rs`、`src/application/scheduled_tasks.rs`、`src/storage/jobs/metadata_reidentify/fill_missing.rs`、`src/storage/jobs/metadata_reidentify/lifecycle.rs`、`src/storage/metadata.rs`、`tests/scheduled_tasks.rs`。

验收：

- [x] 调度轮询独立发现到期自动 provider 失败，即使没有新扫描或用户计划任务也会重新排队；未到期项不会提前重试。
- [x] 重试沿用当前完整度指纹和快照去重，经原有 dispatcher 创建一次自动尝试；保留 item 失败次数并在持续不可用时进入下一档退避，服务恢复后完成并停止重试。
- [x] 不重试人工来源、已消费快照、非 provider 错误、未启用/未选刮削器的媒体库；整个过程不发起扫描，查询和每次调度页有界。
- [x] 不增加 schema/API 变化；自动重试回归、build、fmt 和 Clippy 通过。调度目标存在一项在本任务基线中同样失败的 STRM cron 测试。

验证：`cargo test --locked --test scheduled_tasks due_automatic_metadata_retry_runs_without_a_scan_or_user_schedule -- --exact --nocapture`（1/1）、`cargo test --locked --test reidentify -- --test-threads=1`（24/24）通过。`scheduled_tasks` 目标 8/9 通过；`enabled_strm_task_runs_once_per_matching_cron_minute` 断言得到 0 而非 1，在干净基线 `3c7dd6be` 上精确重跑得到相同失败。`cargo build --locked`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 和 `git diff --check` 通过；Cargo build/test/clippy 使用 `/Volumes/Toshiba/mywork/Lux/target`，本机 `arm64`。未部署 FNOS/x86_64；全量 `--all-targets` 门禁在全部三项修复结束后运行。

#### LUX-574：区分无候选与低分候选的识别结果

范围：自动识别时将零候选记为 `NO_CANDIDATE`，将存在但未达到当前自动确认门槛的候选记为 `LOW_CONFIDENCE` 并保留候选待人工复核。任务条目保持已完成状态并保存候选数；两种结果都汇总为 `COMPLETED_WITH_ISSUES`。不改变自动阈值、评分算法、数据库 schema 或 API 结构。

预计文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/reidentify.rs`、`src/api/admin/admin_metadata.rs`、`src/application/scanner/tests.rs`、`tests/reidentify.rs`。

验收：

- [x] 搜索无候选时以 `NO_CANDIDATE`、候选数 0 记录并汇总为 `COMPLETED_WITH_ISSUES`；失败服务仍单独记录为 `SCRAPER_UNAVAILABLE`。
- [x] 低分候选以 `LOW_CONFIDENCE` 和候选数记录，媒体识别状态与候选保持 `PENDING`，可由管理员复核，父任务显示 `COMPLETED_WITH_ISSUES`。
- [x] 高分自动确认与指定条目候选搜索语义不变；新增及原有 Rust 回归通过，不部署到 FNOS。

验证：测试先按新断言复现旧行为失败，随后 `cargo test --locked --test reidentify -- --test-threads=1`（24/24）、`cargo test --locked --lib application::reidentify`（42/42）和扫描器空搜索回归（1/1）通过；`cargo build --locked`、`cargo fmt --all -- --check` 和 `cargo clippy --locked --all-targets --all-features -- -D warnings` 通过。全量 `cargo test --locked --all-targets` 的库测试 966 通过、24 忽略，集成测试在 `scanned_metadata` 的 `failed_local_poster_insert_does_not_mark_image_stage_complete`（22/23）停止；同一失败已在干净基线 `5548259d` 精确复现。Cargo build/test/clippy 使用 `/Volumes/Toshiba/mywork/Lux/target`，本机 `arm64`；未部署 FNOS/x86_64。

#### LUX-575：在管理任务详情中区分无候选与低分待复核

范围：Web 管理任务列表和详情把 `NO_CANDIDATE` 显示为“没有找到匹配候选”，把 `LOW_CONFIDENCE` 显示为低分候选已保留、等待人工复核；不改变任务 API 或页面布局。

预计文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminOperationsPage.tsx`。

验收：

- [x] 任务汇总错误标签和逐条问题详情均能区分无候选与低分待复核。
- [x] TypeScript 构建通过；Web 全套测试结果与干净基线对照并记录。

验证：`pnpm --dir web install --frozen-lockfile` 和 `pnpm --dir web build` 通过。`pnpm --dir web test` 有 3 项布局/代码拆分断言失败；在干净基线 `5548259d` 上同样 145 通过、3 失败（详情编辑器懒加载断言、移动媒体卡片宽度断言、触屏媒体卡片操作断言）。

#### LUX-576：Lux 媒体 userData 公开最近播放时间

范围：在 Lux 媒体条目的 `userData` 中增加当前用户最近播放该条目的 Unix 秒时间戳 `lastPlayedAt`，供继续观看卡片生成相对时间文案。未知或从未播放时返回 `null`；不改变 Emby DTO、播放状态写入和数据库 schema。

预计文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/api/media.rs`、`tests/catalog.rs`。

验收：

- [x] Lux 首页、继续观看、媒体目录和详情响应的 `userData.lastPlayedAt` 与已存储的 `last_played_at` 一致，单位为 Unix 秒；无值为 `null`。
- [x] Emby `LastPlayedDate`、播放时间存储和 ACL 行为保持不变。
- [x] Catalog API 回归通过；API 文档记录字段含义与空值行为。

依赖：LUX-074。先在现有 Catalog API 集成场景增加有值与空值响应断言，再添加 Lux DTO 字段。

结果：Lux `userData` 现在公开存储的 `lastPlayedAt` 秒级时间戳，未知值序列化为 `null`。Catalog 集成测试在首页、继续观看、目录及详情响应中覆盖有值和空值；Emby 响应保持不变。验证：`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test catalog`（3/3）；本机 `arm64`。未改数据库 schema 或播放状态写入。

#### LUX-577：继续观看卡片三行信息与最近观看文案

范围：调整 Lux Web 首页继续观看卡片的标题层级，并根据 `userData.lastPlayedAt` 显示简短、自然的最近观看提示。电影首行显示电影名与剩余时间，第二行显示年份；电视剧首行显示剧名与剩余时间，第二行显示 `SxxExx·单集标题`。第三行按最近播放时间使用“刚才看到这里”“半小时前看到这里”“今天上午看到这里”“昨天看到这里，接着来”等文案。不得仅凭当前集播放时间声称上一集已完播；缺少最近播放时间时隐藏第三行。

预计文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/features/home/media.tsx`、`web/src/react.css`、`web/tests/home-media.test.tsx`。

验收：

- [x] 电影与电视剧按指定顺序显示标题、剩余时间和上下文；季集格式为 `S01E05·用魔法打败魔法`。
- [x] 最近观看文案覆盖刚刚、分钟、小时、今天、昨天和较早记录；无时间值时不渲染第三行。
- [x] 保留播放入口、进度条、横版封面和桌面/移动端卡片尺寸规则。
- [x] Web 定向卡片测试和生产构建通过。
- [ ] 完整 Web 测试与布局检查全部通过。

依赖：LUX-576。先补充卡片顺序、年份、季集格式与相对时间回归，再修改组件和样式。

结果（2026-10-17）：卡片定向测试 13/13 通过，生产构建通过。全量 Vitest 为 648/652，另 4 项首页预取、媒体库卡片预取/合集链接和详情轮询用例失败；Node 布局检查为 5/6，失败断言仍要求媒体库封面宽度 `min(170px,52vw)`，当前样式为 `min(180px,56vw)`。`pnpm test` 在 Node 静态检查阶段失败于 3 项详情懒加载、首页卡片宽度和触屏卡片操作断言，因此未执行该脚本内的 Vitest 阶段。Web 生产构建通过；上述全量检查尚未全绿。

#### LUX-578：为全量扫描增加唯一媒体进度台账

范围：为全量扫描任务保存已处理媒体条目的去重集合和累计数量，供后续扫描进度接口使用。SQLite 与 PostgreSQL 均须支持；不预扫媒体库总数，不改变现有文件发现和文件计数语义。

预计文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0183_scan_job_processed_media.sql`、`migrations-postgres/0183_scan_job_processed_media.sql`、`tests/storage.rs`。

验收：

- [x] 空数据库迁移创建 `processed_media_count` 和逐任务唯一媒体台账；删除扫描任务时台账级联删除。
- [x] SQLite 与 PostgreSQL migration 一致，不需要预填充历史扫描数据。
- [x] 空库迁移回归通过。

结果（2026-10-18）：SQLite 空库迁移创建计数列和去重台账；回归验证唯一约束与扫描任务删除级联通过。PostgreSQL migration 已加入并由同一回归检查关键结构；尚未连接 PostgreSQL 实例执行 migration。

#### LUX-579：扫描期间累计已处理的唯一影片和整部剧

范围：全量扫描每个目录块完成文件登记后，在同一事务中按媒体 ID 去重累计进度。电影按电影条目计数；剧集库按 `series_id` 计整部剧，同一剧的多季、多集只计一次。附属文件、未识别文件不计入。原有文件数保留给内部诊断，不读取目录总量、不增加第二次文件遍历。

预计文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/jobs/scan/manifest/discovery/presence.rs`、`tests/scanning_jobs.rs`。

验收：

- [x] 同一电影的多个版本只计一部；同一剧的多集、多季只计一部剧；附属文件不计数。
- [x] 未变更的已索引媒体也能在全量扫描过程中计数；worker 重建后继续任务不重复计数。
- [x] 计数随扫描块提交到新字段；不改变旧文件计数。
- [x] 扫描任务目标测试通过。

依赖：LUX-578。先添加影片、剧集和附属文件混合场景的回归，再在已有 presence 事务中写入去重台账。

结果（2026-10-18）：Manifest presence 事务会从本批已登记的文件中聚合电影条目 ID，剧集文件映射到 `series_id`，并通过任务级唯一键累计；旧版任务的空计数不回填。集成回归覆盖同片多版本、跨季多集、附属文件、未变更文件、运行中计数以及重建 worker 后继续处理。`cargo test --locked --test scanning_jobs full_scan_counts_unique_movies_and_whole_series_during_discovery -- --exact` 通过。

#### LUX-580：从扫描任务存储模型读取媒体进度

范围：`StoredScanJob` 暴露独立的可空 `processed_media_count`。新任务从 0 开始；升级前创建的任务保持空值，因为无法可靠地回溯其扫描进度。原有文件计数字段不变。

预计文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/jobs/scan/lifecycle.rs`、`src/storage/repository.rs`、`src/application/scanner/tests.rs`、`src/storage/repository_tests.rs`。

验收：

- [x] 查找、分页列表和活动列表均能读取新计数，旧任务的计数保持空值。
- [x] 原有文件计数与扫描生命周期行为不变。
- [x] Rust 存储与扫描任务目标检查通过。

依赖：LUX-579。先补存储模型映射，再扩展查询列并读取计数。

结果（2026-10-18）：`StoredScanJob` 的查找、分页和活动查询都读取独立媒体计数；升级前的任务保持 `None`，新任务使用默认 0。干净验证 worktree 中 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib scan_job_media_progress_is_loaded_from_job_queries` 与 `cargo fmt --all -- --check` 通过。共享工作区未提交的 `src/application/metadata.rs` 当前有独立类型错误，因此没有把共享工作区的构建状态算作本任务验证结果。

#### LUX-581：在管理员扫描任务接口公开媒体进度

范围：管理员任务列表、任务详情和全局活动接口公开独立的 `processedMediaCount`。保留兼容的文件 `processedCount`/`totalCount` 字段；新字段只用于全量扫描媒体进度。

预计文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/admin_task_activity.rs`、`src/api/admin/admin_jobs.rs`、`src/api/admin/admin_scheduled_tasks.rs`、一个相关 Rust API 集成测试文件。

验收：

- [x] 管理任务列表、详情、活动接口均返回 `processedMediaCount`，且与存储计数一致。
- [x] 既有文件计数字段及其他任务的 JSON 行为不变。
- [x] Rust API 定向回归通过。

依赖：LUX-580。先补接口响应断言，再映射已存储计数。

结果（2026-10-18）：管理员扫描任务列表、详情、活动接口公开可空的 `processedMediaCount`；创建/重试响应从 0 开始。接口回归验证新字段与存储一致，且文件计数字段不变。干净验证 worktree 中 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test admin_events admin_scan_job_endpoints_expose_media_progress_without_changing_file_counts -- --exact` 通过。

#### LUX-582：管理运行记录显示媒体处理数量

范围：全量扫描的管理运行记录将右侧进度改为已处理电影/剧集数量，不显示文件数、总数或百分比。电影库显示“已处理 N 部电影”，剧集库按整部剧显示“已处理 N 部剧”；百分比进度改为不确定进度条。其他任务和增量扫描保持原展示。

预计文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/features/admin/AdminOperationsPage.tsx`、`web/tests/admin-operations.test.tsx`。

验收：

- [x] 全量电影库显示已处理电影数，全量剧集库显示去重后的整部剧数。
- [x] 全量扫描不显示文件分母或百分比；其他任务沿用现有行为。
- [x] 管理运行记录 Web 定向回归通过。

依赖：LUX-581。先补电影库、剧集库和非全量任务的渲染断言，再修改运行记录文案。

结果（2026-10-18）：运行记录按媒体库类型显示已处理电影数、整部剧数或通用媒体数，全量扫描使用不确定进度条并隐藏文件数；没有媒体计数的旧任务显示“媒体进度不可用”。增量扫描和其他任务保留原文件进度。`pnpm --dir web exec vitest run tests/admin-operations.test.tsx` 通过（23/23）。

#### LUX-583：全局活动浮层显示媒体处理数量

范围：全局后台活动浮层对全量扫描显示已处理的电影或整部剧数量，并使用不确定进度条；不显示发现到的文件总数或百分比。其他活动保持原行为。

预计文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/activity/ScanActivityPopover.tsx`、`web/src/lib/api/types.ts`、`web/tests/scan-activity.test.tsx`、`web/tests/lux-shell.test.tsx`。

验收：

- [x] 浮层在电影库显示电影数，在剧集库按整部剧显示；全量扫描进度条无百分比。
- [x] 增量扫描及其他后台任务进度不变。
- [x] 活动浮层 Web 定向回归通过。

依赖：LUX-581。先补活动浮层回归，再修改计数标签和进度显示。

结果（2026-10-18）：全局活动浮层对全量电影库显示电影数、剧集库显示整部剧数，并使用无百分比的不确定进度；增量扫描和其他后台任务继续按文件进度显示。旧任务没有可靠媒体计数时显示“媒体进度不可用”。活动浮层回归通过（6/6），Shell 集成回归通过（1/1），Web build 通过。

完成门禁（2026-10-18）：Rust 格式、build 和 Clippy 通过；扫描计数与管理员 API 定向测试通过。`cargo test --locked --all-targets` 在既有的 NFO actor-credit 回归失败，且该失败已在任务基线 `62c1d2ac` 上复现。完整 Web Vitest 为 651 通过、4 个无关失败；`pnpm --dir web test` 还在静态布局阶段因 3 条首页/触屏 CSS 断言失败而未启动 Vitest。

### 扫描后处理诊断与重试修复

本组修复保留 `COMPLETED`、`JOB_COMPLETED` 和 `ScanCompleted` 的索引完成语义。管理员任务接口通过附加的后处理问题标记和重试能力呈现未完成工作；旧 workflow 与 workflow 2/3 仍沿用各自的检查点恢复合同。

#### LUX-584：保留扫描后处理失败错误码

范围：后处理失败收尾仍进入 `COMPLETED/IDLE`，在现有 `error` 字段保存稳定的 `POSTPROCESSING_FAILED`。重试开始和后处理成功时清除错误；不变更数据库状态集合或完成通知时序。

预计文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/jobs/scan/lifecycle.rs`、`tests/scanning_jobs.rs`、`tests/postgres_database.rs`。

验收：

- [x] 失败目标和 workflow 3 目标物化失败均保留稳定错误码及可恢复状态。
- [x] 恢复后重试清除错误，已完成目标与游标不被重置。
- [x] SQLite 扫描回归及 PostgreSQL 对应合同通过。

验证：`cargo test --locked --test scanning_jobs` 中后处理失败/恢复相关用例；PostgreSQL 对应恢复用例。

后续增量依次补存储诊断、管理员 API 和 Web 流程，每个增量完成后再进入下一项。

结果（2026-10-18）：先确认旧实现无法保留错误码，再补最小存储修复。SQLite 后处理回归 8/8、根目录替换恢复 1/1，以及本地 PostgreSQL 16 workflow 3 恢复 1/1 通过；重试开始和最终成功均清除错误码。

#### LUX-585：批量读取后处理问题与恢复条件

范围：存储层按任务批量读取未完成阶段、未物化检查点、旧工作队列和同库同类型活跃任务；管理员问题筛选在分页之前执行。历史任务不依赖错误码或日志，终态目标不算未完成工作。

预计文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/jobs/scan/targets.rs`、`src/storage/repository_tests.rs`。

验收：

- [x] 旧任务的 PENDING/FAILED 和 workflow 2/3 未物化目标被识别；DONE/SKIPPED、运行中和增量任务不误报。
- [x] 检测旧队列及同库同类型冲突，分页查询没有逐任务 SQL。
- [x] 问题筛选在分页前执行，存储定向测试通过。

依赖：LUX-584。先补诊断矩阵和分页回归，再实现批量查询。

结果（2026-10-18）：未实现筛选时分页断言先失败；补上数据库筛选后，SQLite 与本地 PostgreSQL 16 的同一诊断矩阵均通过（2/2）。十个任务只查一次，超过批次上限按块查询，空集合不查库。

#### LUX-586：统一重试资格并保护后处理重试竞争

范围：读取能力和实际重试共用恢复判断；仅剩终态目标的任务不可重试。后处理重试在事务内复核活跃冲突及未完成工作，重复请求不回退成新建扫描。

预计文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/scanner/jobs/lifecycle.rs`、`src/storage/jobs/scan/lifecycle.rs`、`src/storage/jobs/scan/targets.rs`、`tests/scanning_jobs.rs`。

验收：

- [x] 正常完成和全部 DONE/SKIPPED 的任务拒绝重试。
- [x] 保留检查点的后处理重试沿用任务 ID，重复/冲突请求不产生新任务。
- [x] 旧 workflow 迁移恢复与既有后处理回归通过。

依赖：LUX-585。先补终态目标和并发重试回归。

结果（2026-10-18）：终态目标用例先在旧判断上失败。统一资格判断并补上事务内复核后，后处理回归 9/9、重试及旧 workflow 恢复回归 7/7 通过；待处理旧队列阻止后处理重试，已完成旧队列不阻止；同库冲突和并发重复请求均被拒绝。

#### LUX-587：管理员接口暴露后处理问题和重试能力

范围：管理员扫描列表和详情附加 `hasPostprocessingIssues`、`canRetry`，共享 LUX-586 资格判断；列表支持 `hasPostprocessingIssues=true/false` 筛选，保留原有状态白名单和分页上限；移除被批量诊断替代的旧查询。

预计文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/admin_scan_jobs.rs`、`src/api/admin/admin_jobs.rs`、`src/storage/jobs/scan/reconciliation.rs`、`tests/job_events_api.rs`。

验收：

- [x] 历史失败、未物化目标、正常完成和运行中任务返回正确能力。
- [x] 列表与详情字段一致，问题筛选先于分页，冲突重试返回 409。
- [x] 事件仍可读取 WARN/ERROR；管理员接口回归通过。

依赖：LUX-586。先补管理员 API 合同测试。

结果（2026-10-18）：新增用例先因缺少能力字段失败；实现后 `job_events_api` 2/2 通过，覆盖历史空错误码、v3 未物化检查点、分页、WARN 原因和 409/202 重试。既有用例的登录失败在干净基线 `8b019cef` 同样复现；本机开启系统代理，测试客户端显式直连回环地址后通过。

#### LUX-588：Web 显示后处理问题并提供重试

范围：扫描问题使用警告状态和“索引已完成，后处理未完成”提示，重试按钮使用 `canRetry`；按需读取 WARN/ERROR 原因并提供缺失日志回退。问题筛选使用扫描专属条件，重试后刷新任务及事件缓存。

预计文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/src/features/admin/AdminOperationsPage.tsx`、`web/tests/admin-operations.test.tsx`。

验收：

- [x] 历史问题显示警告、原因入口和允许的重试；正常完成、运行中和冲突任务不误显示重试。
- [x] 根目录 WARN 原因可见，日志缺失或读取失败不阻断重试。
- [x] 问题筛选包含扫描与元数据任务；重试后清除旧状态与原因缓存。
- [x] 管理运行记录定向测试、Web 构建及真实浏览器流程验证完成。

依赖：LUX-587。先补交互测试，再实现 UI。

结果（2026-10-18）：新增交互先复现 5 个失败；修复后管理运行记录 27/27、Web 构建通过。另补旧事件缓存断言，先确认只 invalidation 不会清除旧数据，再于重试成功后移除缓存。Playwright 在真实 Chromium、构建产物及受控 API fixture 下验证警告、WARN 原因、扫描/元数据混合问题筛选、重试后转为 POSTPROCESSING 且隐藏旧原因；390px 页面无横向溢出，控制台 0 error/0 warning。此项是浏览器 UI 证明，不代表 FNOS 部署或真实库恢复验证。

#### LUX-589：诊断查询使用既有阶段索引

范围：后处理诊断按阶段分别使用 EXISTS，复用现有 PENDING/FAILED 部分索引，避免列表读取已完成目标的全部行；不增加索引或迁移。

预计文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/jobs/scan/targets.rs`（含查询计划回归）。

验收：

- [x] SQLite 查询计划使用三个阶段部分索引，诊断及筛选含义不变。
- [x] SQLite/PostgreSQL 诊断矩阵通过。

依赖：LUX-588。先用实际数据库 schema 复现查询走目标主键范围扫描，再调整查询形状。

结果（2026-10-18）：查询计划回归先明确显示目标主键范围扫描并失败；按阶段拆开 EXISTS 后使用现有三个部分索引。查询计划、SQLite 诊断矩阵及本地 PostgreSQL 16 诊断矩阵 3/3 通过；未增加 schema 或迁移，不将此查询计划结果外推为 NAS 性能测量。

#### LUX-591：调整查询计划测试的模块位置

范围：将查询计划测试移到生产代码之后，满足 Clippy `items_after_test_module`；不改变查询或运行时行为。

预计修改文件：`src/storage/jobs/scan/targets.rs`、`docs/LUX-DEVELOPMENT.md`。

验收标准：

- [x] 查询计划和 SQLite/PostgreSQL 诊断矩阵继续通过。
- [x] `cargo clippy --locked --all-targets --all-features -- -D warnings` 通过。

依赖：LUX-589。Clippy 已复现测试模块位置错误，先调整位置，再复跑相关检查。

结果（2026-10-18）：只移动测试模块，查询计划和 SQLite/PostgreSQL 诊断矩阵 3/3 通过；完整 Clippy 与格式检查通过。完整测试中超时的 local metadata worker 用例在同一最终代码单独复跑通过，完整门禁仍保留原失败结果。

#### LUX-590：记录后处理重试修复的兼容与完成门禁

范围：记录 LUX-584 至 LUX-589 的 API、历史任务恢复、浏览器验证及完整检查结果；不增加运行时行为。

预计修改文件：`docs/LUX-DEVELOPMENT.md`、`docs/COMPATIBILITY.md`。

验收标准：

- [x] 记录索引完成语义、附加 API 字段、问题筛选和旧 workflow 恢复合同。
- [x] 记录 SQLite、PostgreSQL、接口、Web 和真实浏览器的证据范围。
- [x] 完整门禁失败逐项与干净基线对照，不以定向通过代替全量通过。

依赖：LUX-589、LUX-591。先完成最终检查与基线对照，再写入兼容记录。

结果（2026-10-18）：`docs/COMPATIBILITY.md` 记录合同及验证边界。相关后处理、诊断、PostgreSQL、接口、Web 回归与 Chromium 受控接口验证通过，build、fmt、Clippy 和 Shell/Python 工具检查通过。完整 Rust 库测试 972 通过、4 失败、25 ignored；三个存储失败在基线复现，worker 超时用例在最终代码和基线单独通过。完整扫描 target 为 89 通过、2 个基线复现失败；Web 静态 145 通过/3 失败，完整 Vitest 655 通过/4 失败，七项均在基线复现。完整门禁未通过，不扩展修复无关问题；未合并、推送或部署。

#### LUX-592：兼容 X-Real-IP 和 Forwarded 客户端 IP

范围：扩展播放会话和活动记录共用的客户端 IP 读取逻辑，按 X-Forwarded-For、X-Real-IP、Forwarded 的有效值顺序回退；保留现有账号认证、ACL、限流和 HTTPS 判断行为。

预计修改文件：`src/network.rs`、`src/api/legacy.rs`、`tests/remote.rs`、`docs/LUX-DEVELOPMENT.md`、`docs/COMPATIBILITY.md`。

验收标准：

- [x] 无需代理 CIDR 配置，支持 X-Real-IP 的单个 IPv4/IPv6 地址。
- [x] 支持 RFC 7239 Forwarded 首个元素中的 for，包含大小写不敏感参数名、引号、IPv6 和可选端口；unknown、混淆标识及异常值不作为 IP。
- [x] 有效 X-Forwarded-For 优先；无效高优先级头允许回退；不从后续代理元素猜测原始客户端 IP。
- [x] 真实 HTTP 集成测试覆盖根路径和 /emby 播放回调，验证保存的 IP 与 GET /Sessions 的 RemoteEndPoint。
- [x] 记录定向测试、Rust 完成门禁和本机架构；真实 Lucky/FNOS 部署验证单独说明。

验证：`cargo test --locked --test remote --test sessions --test playback --test admin_dashboard`；Rust build、all-targets test、fmt 和 Clippy 完成门禁。

依赖：LUX-092。

结果（2026-10-19）：新增回环 HTTP 测试先在旧逻辑上复现：只携带 `X-Real-IP` 的播放事件落库 IP 为 `None`。接入共用入口后，`remote` 8/8、`sessions` 2/2、`playback` 3/3、`admin_dashboard` 1/1 通过；其中新增播放回归覆盖根路径和 `/emby`、X-Real-IP/Forwarded 的 IPv4/IPv6，以及数据库与会话接口输出。解析回归覆盖优先级、无效值回退、引号/转义/端口和拒绝后续代理地址。原有 remote HTTP 用例首次受本机代理影响返回 502；为测试进程设置 `NO_PROXY`/`no_proxy=127.0.0.1,localhost` 后通过，生产出站代理配置未改动。

本机为 macOS `arm64`，全部 Cargo 命令使用 `/Volumes/Toshiba/mywork/Lux/target`。`cargo build --locked`、`cargo fmt --all -- --check` 和 `cargo clippy --locked --all-targets --all-features -- -D warnings` 通过。`cargo test --locked --all-targets` 在库测试阶段得到 973 passed、3 failed、25 ignored，后续集成 targets 未运行；相关集成 targets 已按上述定向命令独立通过。三个失败分别为 `automatic_and_explicit_fill_missing_jobs_keep_dispatch_sources_separate`（实际 0、预期 1）、`automatic_fill_missing_retry_backoff_excludes_manual_modes_and_cancelled_failures`（空集合越界）和 `changed_local_fill_request_during_running_item_is_not_lost`（实际 0、预期 1）。三项均在干净基线 `842f9870` 以各自完整测试名和 `--lib -- --exact` 单独复现相同失败，临时 worktree 已清理；完整 Rust 门禁仍未通过。

真实 Lucky、FNOS/x86_64 和第三方播放器尚未验证，本任务未推送或部署。请求头全部缺失时仍无法恢复客户端地址，兼容合同见 `docs/COMPATIBILITY.md`。

#### LUX-593：用户管理页面结构

范围：建立用户列表、用户详情和全局用户设置的导航结构。详情页预留账号资料、权限、偏好、内容限制、设备与登录、播放与转码和活动记录入口；全局设置预留注册邀请、邮件、新用户默认权限、角色预设、全站规则和保留策略。除现有用户列表入口外均显示未实施占位，不新增或暗示已生效的权限行为。原外挂字幕权限模块不属于此导航范围。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/app/App.tsx`、`web/src/features/admin/AdminUsersPage.tsx`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/src/features/admin/user-management.css`。

验收标准：

- [x] 管理员可以从用户列表进入全局设置和任意用户详情。
- [x] 详情页覆盖设计方案中规划的用户模块入口，页面结构适配小屏。
- [x] 未实现模块清楚显示占位提示，不提供无后端支撑的交互开关。
- [x] 当前创建、编辑、禁用和删除用户流程不因路由结构调整而改变。

依赖：用户管理设计已由 `docs/USER-MANAGEMENT-DESIGN.md` 登记。账号生命周期与用户查询在后续独立 `LUX-*` 任务实施；播放与转码权限须等待阶段门确认。

结果（2026-10-19）：新增用户列表视图占位、用户详情模块导航和全局用户设置导航。模块一、三、六及其他待实施模块均未新增业务行为。`pnpm --dir web build` 通过；未部署。

#### LUX-594：账号资料与回收站数据模型

范围：为用户补充可选邮箱、邮箱验证时间和软删除/清理时间字段；为邮箱建立不区分大小写的唯一索引，并在 SQLite 与 PostgreSQL 中同步迁移。现有用户邮箱为空且维持原有启用状态；本任务只建立持久化基础，不改变 API、登录或删除行为。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0184_user_account_profile.sql`、`migrations-postgres/0184_user_account_profile.sql`、`src/storage/repository.rs`、`src/storage/users.rs`。

验收标准：

- [x] 新迁移可从现有 SQLite/PostgreSQL 数据库应用，旧账号保持邮箱为空且状态不变。
- [x] 用户查询统一映射新增字段；邮箱为空允许重复，非空邮箱不区分大小写唯一。
- [x] 不改变当前登录、用户删除及公开 API 行为。

依赖：LUX-593。后续模块一任务逐步接通 API、用户界面和账号生命周期。

结果（2026-10-19）：SQLite/PostgreSQL 迁移、邮箱唯一索引和用户存储映射已实现。新增既有数据库升级回归：从 0183 迁移前账号开始应用 0184 后续迁移，确认旧账号邮箱/验证/删除字段为空且启用状态不变，并确认邮箱唯一索引忽略大小写。SQLite `storage` target 1/1、PostgreSQL `postgres_database` ignored target 1/1（PostgreSQL 16 临时实例）、Rust build、fmt check 与 diff check 通过。

#### LUX-595：管理员账号资料 API

范围：管理员创建用户时可选填写邮箱；详情接口返回单个用户的资料；独立资料更新接口可修改显示名称和邮箱。邮箱输入做基本格式校验并大小写归一化；修改地址清除原验证时间，不把未验证地址标记为已验证。权限更新仍沿用现有接口。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/users.rs`、`src/auth/users.rs`、`src/api/admin/admin_users.rs`。

验收标准：

- [x] 管理员可获取用户详情并创建/更新邮箱；同一邮箱大小写变体被唯一约束拒绝。
- [x] 修改邮箱后验证时间为空；旧账号为空邮箱时保持兼容。
- [x] 邮箱格式错误返回 400，不泄漏数据库或账户敏感信息。

依赖：LUX-594。

结果（2026-10-19）：新增管理员单用户详情、创建时设置邮箱和独立资料更新接口；邮箱统一小写、执行基本格式检查、重复地址映射为冲突，地址变更会清除验证时间。补充 `users` API 回归，覆盖旧账号空邮箱、创建与资料更新、CSRF、邮箱大小写重复、邮箱变更清除验证状态、等价地址保留验证状态及无效输入不泄漏存储错误。`users` target 4/4、Rust build、fmt check 和 diff check 通过；`docs/API.md` 已同步创建、详情与更新合同。

#### LUX-596：管理员账号资料详情页

范围：详情页直接按用户 ID 读取资料，并实现模块一中的显示名称、邮箱编辑和邮箱验证状态展示。保存成功后同步更新列表/详情缓存；其余模块继续保持占位。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/client.ts`、`web/src/lib/api/types.ts`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/src/features/admin/user-management.css`。

验收标准：

- [x] 详情页不依赖加载全量用户列表；不存在用户显示明确状态。
- [x] 管理员可保存显示名称与邮箱，显示邮箱未验证状态和服务端错误。
- [x] 模块六仍只有占位文案，不出现可操作的播放/转码开关。

依赖：LUX-595。

结果（2026-10-19）：详情页改为按用户 ID 请求资料；模块一资料表单可更新显示名称、邮箱并呈现验证状态。模块六保持禁用占位。`pnpm --dir web build` 通过；未部署。

#### LUX-597：账号软删除与恢复存储服务

范围：实现账号软删除、7 天清理期限、恢复和回收站彻底删除存储操作。禁用和软删除都撤销 Web/Emby 凭证并停止数据库中活跃的 Emby/Web 播放会话；软删除保留资料与权限、保存原禁用状态；恢复原禁用状态但不恢复已撤销凭证或播放会话。拒绝删除最后一个有效管理账户。仅提供存储与应用服务方法，不接通 HTTP 或页面操作。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 删除保留账号和关联数据 7 天，保存的恢复状态准确。
- [x] 禁用或删除撤销两类令牌并停止两类数据库播放会话；过期或重复删除不会破坏状态。
- [x] 恢复清除删除时间并还原此前启用/禁用状态，且不恢复旧凭证。
- [x] 最后一名有效管理员受保护，彻底删除只接受已进入回收站的账号。

依赖：LUX-596。

结果（2026-10-19）：存储层已有 7 天软删除、恢复和仅回收站账号彻底删除操作，本次补齐回归验证。新增三个存储用例：软删除与禁用都撤销 Web/Emby 凭证、停止 Emby/Web 播放记录并捕获服务端 HLS 会话；恢复分别保留此前启用和禁用状态且不恢复旧凭证；验证最后一名有效管理员保护、只能彻底删除回收站账号及到期批量清理边界。三个定向 lib 测试各 1/1，`cargo fmt --all -- --check` 与 `git diff --check` 通过。HTTP 接线由 LUX-599 覆盖。

#### LUX-598：禁用和删除时停止服务端 HLS 进程

范围：提供应用服务停止指定服务端 HLS 会话进程的方法。生命周期存储事务负责捕获目标用户的活跃 HLS 会话 ID，API 在事务提交后调用该方法；停止失败写入不含凭证的会话级日志并继续清理其他会话，不改变 Web 端播放协议。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/playback/session.rs`。

验收标准：

- [x] 禁用和软删除的数据库事务捕获目标用户当前活跃的服务端 HLS 会话。
- [x] 停止方法逐个释放 HLS 进程与临时目录，单项失败不阻断后续会话清理。
- [x] Cargo 构建与格式检查通过。

依赖：LUX-597。

结果（2026-10-19）：新增逐项停止指定 HLS 进程入口，活跃会话 ID 由 LUX-597/599 的生命周期事务捕获。`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked` 与格式检查通过；入口在 LUX-599 管理员生命周期 API 中接通。未运行测试。

#### LUX-599：管理员账号生命周期 API

范围：管理员删除用户改为软删除并返回回收站期限；新增恢复和彻底删除端点。禁用、软删除事务性撤销凭证、停止数据库播放会话并返回同时捕获的服务端 HLS 会话 ID，提交后交给播放服务终止。Emby 管理接口的禁用和删除沿用相同语义；移除不再需要的独立会话查询；彻底删除账号时清理头像文件。

预计修改文件（7 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/users.rs`、`src/auth/users.rs`、`src/api/admin/admin_users.rs`、`src/api/emby_handlers.rs`、`src/storage/sessions.rs`、`src/application/playback/session.rs`。

验收标准：

- [x] 管理员删除将账号放入 7 天回收站；恢复还原禁用状态；彻底删除只接受回收站账号。
- [x] Lux 与 Emby 管理入口禁用或删除后撤销 Web/Emby 凭证、停止数据库会话并终止捕获的服务端 HLS 进程。
- [x] 最后一名有效管理员保护和错误状态码保持正确；API 不返回原始凭证。
- [x] Cargo 构建与格式检查通过。

依赖：LUX-597、LUX-598。

结果（2026-10-19）：Lux 管理删除改为回收站，新增恢复和提前彻底删除；Emby 管理删除同步软删除。禁用/删除事务返回被停止的服务端 HLS ID，并在提交后终止进程；彻底删除会清理头像文件。`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked` 和格式检查通过，未运行测试。

#### LUX-600：用户个人资料编辑

范围：为登录用户提供受 CSRF 保护的资料读取和更新接口，并在账户页保存自己的显示名称和邮箱，展示邮箱验证状态。修改邮箱时沿用存储层清除验证时间的行为；邮件发送和邮箱验证流程留给注册邮件任务。

预计修改文件（6 个）：`docs/LUX-DEVELOPMENT.md`、`src/api/users.rs`、`web/src/lib/api/client.ts`、`web/src/lib/api/types.ts`、`web/src/lib/api/query-keys.ts`、`web/src/features/account/AccountPage.tsx`。

验收标准：

- [x] 读取和修改仅作用于当前登录账号；修改接口要求有效会话和 CSRF。
- [x] 用户可保存显示名称、邮箱，邮箱变更后显示未验证状态；非法或重复邮箱返回明确错误。
- [x] 页面同步更新当前会话显示名称，不影响头像和密码修改流程。
- [x] Web 构建通过。

依赖：LUX-594、LUX-595。

结果（2026-10-19）：新增当前登录用户的个人资料读取/更新 API，并在账户页保存显示名称和邮箱；邮箱更新由既有存储逻辑清除验证时间，当前显示未验证状态。`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、Rust 格式检查和 `pnpm --dir web build` 通过。邮件验证发送尚未接通，未运行测试。

#### LUX-601：管理员用户查询与分页 API

范围：管理员用户列表支持用户名/显示名称/邮箱搜索、账号状态筛选、创建时间/最近登录/最近活动排序及服务端分页。默认每页 20 条，上限 100 条；返回匹配总数、邮箱资料、近期活动时间与 Web/Emby 合并去重的当前播放数。默认列表排除回收站账号，`deleted` 状态专供回收站视图。

预计修改文件（6 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/users.rs`、`src/auth/users.rs`、`src/api/admin/admin_users.rs`。

验收标准：

- [x] 服务端先过滤和排序再分页，页大小默认 20 且不超过 100；返回准确匹配总数。
- [x] 搜索覆盖用户名、显示名称、邮箱；状态与排序仅接受定义选项。
- [x] 最近活跃合并登录/访问与播放活动；当前播放数覆盖 Web/Emby 且按会话去重、沿用 90 秒活跃窗口。
- [x] Cargo 构建与格式检查通过。

依赖：LUX-594、LUX-597、LUX-600。

结果（2026-10-19）：用户列表 API 支持分页、状态、搜索、排序及匹配总数；最近活动汇总 Web/Emby 凭证、Web 会话和播放事件，当前播放合并两种播放表并按播放会话去重。页大小默认 20、最大 100，默认排除回收站账号。`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked` 和格式检查通过；未运行测试。

#### LUX-602：用户列表与回收站界面

范围：用户管理页接入模块三分页查询，提供搜索、状态、排序和翻页控件；列表展示邮箱、最近登录/活动和当前播放数。回收站视图展示删除时间与到期清理时间，并接通恢复、提前彻底删除；普通列表删除提示改为进入 7 天回收站。待审批仍保持模块二占位。

预计修改文件（6 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/client.ts`、`web/src/lib/api/types.ts`、`web/src/lib/api/query-keys.ts`、`web/src/features/admin/AdminUsersPage.tsx`、`web/src/features/admin/user-management.css`。

验收标准：

- [x] 列表控件驱动服务端搜索、筛选、排序与分页，显示匹配总数和当前播放数。
- [x] 回收站显示清理时间，可恢复或经确认提前彻底删除；成功后刷新列表。
- [x] 普通列表删除明确告知进入回收站并保留期为 7 天；待审批仍不提供虚假操作。
- [x] Web 构建通过。

依赖：LUX-593、LUX-599、LUX-601。

结果（2026-10-19）：用户列表接入搜索、状态筛选、排序、分页和播放/活动信息；删除后进入回收站，提供恢复与提前彻底删除并明确影响范围。待审批维持占位。`pnpm --dir web build` 通过；未运行测试、未部署。

#### LUX-603：回收站到期自动清理

范围：添加有界后台清理 worker，每分钟至多清理 50 个已超过默认 7 天期限的回收站账号；数据库条件保证只清理仍处于回收站且期限已到的账号，恢复与自动清理并发时由数据库决定唯一成功操作。数据库删除后移除头像文件，单个头像删除失败只记录不含敏感数据的错误并继续下一项。用户详情、权限、偏好、设备、活动和播放数据按现有用户外键级联规则清理，不删除媒体文件。清理期限配置留在全局设置后续任务。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/users.rs`、`src/application/user_cleanup.rs`、`src/application/mod.rs`、`src/api/legacy.rs`。

验收标准：

- [x] worker 分钟级运行，单次处理数量有界；启动时不立即执行清理。
- [x] 只清理到期且仍在回收站的账号，恢复与清理竞态不误删已恢复账号。
- [x] 成功清理账号后移除头像；单项失败不会阻断其余清理。
- [x] Rust 构建与格式检查通过；未运行测试。

验证：`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check`。Cargo 前确认 `/Volumes/Toshiba` 已挂载。

依赖：LUX-597、LUX-599。

结果（2026-10-19）：后台 worker 每分钟最多原子清理 50 个已到期账号；恢复后的账号不匹配删除条件，头像清理逐项执行。Cargo build、fmt check 与 diff check 通过。Clippy 检查发现用户创建函数参数数量和排序枚举命名两项 lint，分别在 LUX-604、LUX-605 跟进；未运行测试或部署。

#### LUX-604：整理用户创建存储输入

范围：将用户创建存储方法的身份、凭证、初始权限和邮箱字段收敛为命名输入结构，保持 SQL、绑定顺序和账户创建行为不变，以消除参数数量 lint。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/users.rs`、`src/auth/users.rs`。

验收标准：

- [x] 新结构涵盖原调用的所有字段；SQLite/PostgreSQL 插入 SQL 和绑定顺序不变。
- [x] Rust 构建、格式检查通过；未运行测试。

验证：`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-595。

结果（2026-10-19）：`NewStoredUser` 命名输入结构取代七个独立字段参数，SQL 和绑定顺序未变。Cargo build、fmt check 与 diff check 通过；未运行测试。

#### LUX-605：整理用户列表排序字段命名

范围：缩短内部排序字段枚举变体名称，保留 API 的 `createdAt`、`lastLoginAt`、`lastActivityAt` 参数和对应 SQL 排序列不变。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/users.rs`、`src/api/admin/admin_users.rs`。

验收标准：

- [x] 内部枚举不再重复 `At` 后缀，API 参数与 SQL 映射不变。
- [x] Rust 构建、格式检查和 Clippy 全目标检查通过；未运行测试。

验证：`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`cargo fmt --all -- --check`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo clippy --locked --all-targets --all-features -- -D warnings`、`git diff --check`。

依赖：LUX-601、LUX-604。

结果（2026-10-19）：内部变体改为 `Created`、`LastLogin`、`LastActivity`，对外查询字段和数据库排序列保持原样。Cargo build、fmt check、全目标/all-features Clippy 与 diff check 通过；未运行测试。

#### LUX-606：管理员用户头像 API

范围：新增管理员读取与更新指定用户头像的 API，复用现有私有头像存储、内容校验和 5 MiB 上限。读取不要求 CSRF；更新要求管理员会话和 CSRF，或通过现有共享管理员 API key 鉴权。缺失头像返回 404；不返回用户凭证或文件系统路径。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`src/api/admin/admin_users.rs`。

验收标准：

- [x] 头像读取和更新只允许管理员访问；上传复用现有格式、大小和路径校验。
- [x] 无头像返回 404，响应为正确图片 MIME 类型且私有缓存需重新验证。
- [x] Rust 构建与格式检查通过；未运行测试。

验证：`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-594、LUX-595。

结果（2026-10-19）：新增受管理员鉴权的目标用户头像 GET/PUT；写入要求 CSRF 并复用已有 5 MiB、格式和私有目录校验，GET 返回实际 MIME、私有重验证缓存，缺失返回 404。Cargo build、fmt 和 diff check 通过；未运行测试。

#### LUX-607：管理用户头像页面

范围：列表和回收站显示用户头像，详情资料页显示并允许管理员更新头像；图片通过 LUX-606 管理员 API 获取和保存，上传后立即刷新图片缓存。列表展示预设来源；当前尚无预设模型的账号明确显示“未设置”。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/client.ts`、`web/src/features/admin/AdminUsersPage.tsx`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/src/features/admin/user-management.css`。

验收标准：

- [x] 普通列表与回收站显示头像；头像缺失或读取失败时显示通用图标。
- [x] 管理员可从详情页更新头像，上传成功后显示新图片并报告失败。
- [x] 列表显示当前预设来源状态，不虚构未配置的预设。
- [x] Web 构建通过；未运行测试。

验证：`pnpm --dir web build`、`git diff --check`。

依赖：LUX-593、LUX-601、LUX-606。

结果（2026-10-19）：列表和回收站改为懒加载管理员头像，缺失或失败时显示默认图标；详情资料页可更新头像并刷新缓存，失败会显示接口错误。用户列表明确显示预设来源（当前无预设时为“未设置”）。`pnpm --dir web build` 与 diff check 通过；未运行测试或部署。

#### LUX-608：用户详情账号状态操作

范围：在用户详情资料页显示并维护账号状态，支持启用、禁用、软删除、恢复和确认后彻底删除；已删除账号资料只读。回收站行提供进入详情的入口。复用既有管理员 API 与最后管理员保护。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/src/features/admin/AdminUsersPage.tsx`、`web/src/features/admin/user-management.css`、`web/tests/admin-user-profile-lifecycle.test.tsx`。

验收标准：

- [x] 用户详情显示正常、禁用或回收站状态，并展示对应可用操作。
- [x] 生命周期操作明确确认破坏性影响，成功后同步详情/列表缓存并导航到正确视图。
- [x] 已删除用户资料只读，仍可恢复或经确认彻底删除。
- [x] Web 构建通过；新增行为回归按用户管理测试目标执行。

验证：`pnpm --dir web build`、`pnpm --dir web exec vitest run tests/admin-user-profile-lifecycle.test.tsx`、`git diff --check`。完整 Web 门禁在阶段验收执行。

依赖：LUX-596、LUX-599、LUX-602。

结果（2026-10-19）：详情资料卡和页面标题统一呈现正常、禁用或回收站状态；生命周期面板支持启用、禁用、移入回收站、恢复和确认后彻底删除。成功后刷新详情/列表缓存并返回对应列表视图，回收站账号的资料与头像输入为只读。新增 4 项回归覆盖状态、确认、只读和恢复导航；定向 Vitest 4/4 通过，Web 构建在阶段门复跑。

#### LUX-609：对齐用户列表回收站回归和路由测试环境

范围：更新用户列表删除回归，使其验证移入回收站而非旧的永久删除行为；为使用 URL 状态和 React Router 链接的用户列表测试补充内存路由容器。不改变生产运行时行为。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`web/tests/admin-users-delete.test.tsx`、`web/tests/admin-users-access.test.tsx`。

验收标准：

- [x] 列表删除回归断言 7 天软删除确认文案、API 结果和列表刷新。
- [x] 用户列表查询和权限回归在 MemoryRouter 中渲染。
- [x] 用户列表、权限和详情生命周期定向 Vitest 目标通过。

验证：`pnpm --dir web exec vitest run tests/admin-users-delete.test.tsx tests/admin-users-access.test.tsx tests/admin-user-profile-lifecycle.test.tsx`、`git diff --check`。

结果（2026-10-19）：更新软删除断言并为用户列表测试补齐 MemoryRouter；三个相关文件组 13/13 用例通过。完整 Web 门禁在阶段验收执行。

#### LUX-610：读取可配置的用户删除宽限期

范围：软删除用户时从 `server_settings` 读取删除宽限天数，并将清理期限固化到该用户记录。默认值保持 7 天；已有回收站账号不因后续配置修改而改变其清理时间。不新增迁移或修改已经记录的期限。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 未配置时软删除仍保留 7 天。
- [x] 配置其他有效天数后，新删除账号的计划清理时间按该值计算。
- [x] 非法或超范围存储值不能导致立即清理。
- [x] SQLite 存储回归及 Rust 格式检查通过。

验证：`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib configured_user_deletion_grace_period`、`cargo fmt --all -- --check`、`git diff --check`。Cargo 前确认 Toshiba 已挂载。

依赖：LUX-597、LUX-599。

结果（2026-10-19）：软删除在事务中读取 `user_deletion_grace_days`，仅接受 1–3650 天，缺失或非法值安全回退 7 天；新期限在删除时固化，配置修改不重算回收站记录。新增存储回归覆盖默认 7 天、自定义 30 天和非法 0 值；定向 Cargo 测试 1/1、fmt 和 diff check 通过。

#### LUX-611：开放用户删除宽限期管理员设置接口

范围：在现有管理员设置 API 中读取和更新用户删除宽限天数，沿用管理员会话和 CSRF 边界。只接受 1–3650 天，默认 7 天；非法设置整体拒绝，不修改已进入回收站账号的期限。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/users.rs`、`src/application/settings.rs`、`src/api/admin/admin_settings.rs`、`tests/admin_dashboard.rs`。

验收标准：

- [x] 管理员 GET 返回当前删除宽限期，缺省为 7 天。
- [x] PATCH 保存有效范围并返回新值；范围外数值返回 400。
- [x] 设置只影响之后移入回收站的账号，已有到期时间保持不变。
- [x] `admin_dashboard` 集成目标和 Rust 格式检查通过。

验证：`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test admin_dashboard`、`cargo fmt --all -- --check`、`git diff --check`。Cargo 前确认 Toshiba 已挂载。

结果（2026-10-19）：管理员设置 GET/PATCH 增加 `userDeletionGraceDays`，默认 7 天，允许 1–3650 天；缺省保持原值，范围外返回明确 400，审计事件记录新值。接口回归覆盖默认、保存 30 天、拒绝 0/3651 且拒绝后值不变；`admin_dashboard` 1/1、fmt 和 diff check 通过。回环测试设置 `NO_PROXY/no_proxy=127.0.0.1,localhost`。

#### LUX-612：在用户全局设置页维护删除宽限期

范围：将“删除与活动保留”设置页中的用户删除宽限期接入既有管理员设置 API。保存后更新界面缓存；清楚说明新值只作用于后续移入回收站的账号，活动记录保留继续显示未实施状态。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/src/lib/api/types.ts`、`web/tests/admin-user-retention-settings.test.tsx`。

验收标准：

- [x] 默认或缺省值显示 7 天，管理员可配置 1–3650 天。
- [x] 无效值不能提交；保存成功后更新查询缓存并显示成功状态，失败时保留可重试界面。
- [x] 已在回收站的用户期限不因修改设置而改变，活动记录保留仍为占位。
- [x] 用户管理设置回归与 Web 构建通过。

验证：`pnpm --dir web exec vitest run tests/admin-user-retention-settings.test.tsx`、`pnpm --dir web build`、`git diff --check`。

结果（2026-10-19）：用户全局设置“删除与活动保留”页接入删除宽限天数表单，支持 1–3650 天；缺省显示 7 天，失败保留可重试表单并显示错误，成功同步管理员设置缓存。页面说明修改不重算现有账号期限，活动记录保留继续占位。新增 3 项回归通过，Web 构建通过。

#### LUX-613：隔离账户头像测试中的资料接口响应

范围：为账户头像上传页面测试显式模拟账号资料接口，避免该测试的头像上传 fetch 响应被错误复用为资料查询响应。不改变生产运行时行为。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`web/tests/account-settings.test.tsx`。

验收标准：

- [x] 头像上传回归覆盖既有账号资料查询并通过。
- [x] 账户设置 Vitest 目标通过。

验证：`pnpm --dir web exec vitest run tests/account-settings.test.tsx`、`git diff --check`。

结果（2026-10-19）：头像上传测试独立 mock `/api/v1/auth/profile` 查询返回，不再复用头像 PUT 的 `avatarUrl` 响应；账户设置 Vitest 23/23 通过。

#### LUX-614：为用户播放策略建立持久化字段

范围：为用户增加播放许可、同时播放上限、封装转换、音频转码和视频转码策略字段。已存在账号迁移后保持允许播放与转码、并发不限（`0`）；新账号默认 2 路及管理员读写界面由后续任务实现。本任务不改变播放准入行为。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0185_user_playback_policy.sql`、`migrations-postgres/0185_user_playback_policy.sql`、`src/storage/repository_tests.rs`、`tests/postgres_database.rs`。

验收标准：

- [x] SQLite 与 PostgreSQL migration 均增加 `can_play`、`max_simultaneous_playbacks`、`allow_remux`、`allow_audio_transcode`、`allow_video_transcode`，包含布尔值与非负并发上限约束。
- [x] 从空 SQLite 数据库启动仍可完成全部 migration。
- [x] 默认值保留既有账号的播放/转码行为，并将并发上限初始化为不限。
- [x] 新增 SQLite 与 PostgreSQL 引导回归覆盖所有字段默认值。

验证：`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib user_playback_policy_migration_defaults`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test postgres_database --no-run`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。Cargo 前确认 Toshiba 已挂载。

依赖：LUX-593、LUX-599、LUX-602、阶段门确认。

结果（2026-10-19）：SQLite 与 PostgreSQL 增加五项策略列；旧账号默认允许播放和转码、并发不限。SQLite 回归验证从空库启动及旧账号行经 migration 后的值，定向测试 1/1 通过。PostgreSQL bootstrap 回归已补充，但本机 `127.0.0.1:55432` 未监听，本次未执行该忽略测试；未部署。

#### LUX-615：存储层读取与更新播放策略

范围：为存储层增加读取和更新用户播放策略的方法；通过应用创建的普通用户和首个管理员默认设置为 2 路同时播放。迁移已有账号仍为 `0`（不限）。本任务不接管理员接口、不实施播放准入或转码限制。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/mod.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 新建普通账号和首个管理员通过应用存储方法创建时默认 2 路。
- [x] 可读取和更新播放、并发、封装转换、音频转码、视频转码五项策略。
- [x] 负并发上限被数据库约束拒绝；已删除或不存在的账号不能被更新。
- [x] 定向 SQLite 存储回归及 Rust 格式检查通过。

验证：`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib new_user_playback_policy_defaults_to_two_and_updates`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。Cargo 前确认 Toshiba 已挂载。

依赖：LUX-614。

结果（2026-10-19）：普通用户和首个管理员的应用创建路径显式设为 2 路；migration 对已有用户的 0（不限）保持不变。存储层增加五项策略读取/更新，负并发被 CHECK 拒绝，已软删除/不存在账号不接受更新。定向测试 1/1 通过，未部署。

#### LUX-616：开放管理员播放策略接口

范围：增加管理员读取及完整替换用户播放策略的 API。`maxSimultaneousPlaybacks=0` 表示不限；拒绝负值。修改要求管理员会话和 CSRF，并记录策略值审计事件；回收站账号只读。本任务不实施播放准入或转码限制。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`docs/COMPATIBILITY.md`、`src/auth/users.rs`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [x] 管理员可通过 `GET /api/v1/admin/users/{userId}/playback-policy` 读取五项策略。
- [x] CSRF 保护的 `PUT` 接收完整策略并返回保存结果；负上限返回 400，未知或回收站用户返回 404。
- [x] 普通用户不能访问；成功变更产生 `USER_PLAYBACK_POLICY_UPDATED` 审计事件，不记录凭证。
- [x] 兼容文档说明接口字段和 `0` 不限语义；定向 users 集成测试通过。

验证：`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test users admin_can_manage_users_and_last_manager_is_protected`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。Cargo 前确认 Toshiba 已挂载。

依赖：LUX-615。

结果（2026-10-19）：新增管理员专用 GET/PUT，PUT 通过 CSRF 校验并要求五字段完整提交；负并发上限返回 400，`0` 原样保存为不限，普通用户无权访问，回收站用户读写均返回 404。每次成功写入记录策略值审计事件，不含认证信息。`users` 定向集成测试 1/1 通过，接口及 `0` 语义已记入兼容文档；未部署。

#### LUX-617：在用户详情接入播放策略表单

范围：将管理员策略 API 接到“播放与转码”模块，提供允许播放、并发上限、封装转换、音频转码和视频转码设置。说明 `0` 表示不限及当前尚未由准入逻辑执行；回收站账号只读。不展示远程访问或码率伪开关。

预计修改文件（6 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/src/features/admin/user-management.css`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/admin-user-playback-policy.test.tsx`。

验收标准：

- [x] 播放与转码模块加载已保存策略并可完整保存五项设置。
- [x] 并发上限只接受非负整数，`0` 明确显示为不限；保存失败时保留用户输入。
- [x] 回收站账号不请求写入且呈现只读说明；页面标明播放准入仍待实施。
- [x] 定向 Vitest、Web 构建通过，桌面与窄屏模块布局无溢出。

验证：`pnpm --dir web exec vitest run tests/admin-user-playback-policy.test.tsx`、`pnpm --dir web build`、`git diff --check`。另用本地真实浏览器检查用户详情的播放策略流程。

依赖：LUX-616。

结果（2026-10-19）：用户详情的“播放与转码”读取并保存五项策略，非法并发数禁止提交且保存失败保留草稿；回收站账号不请求策略 API。页面说明 `0` 表示不限并标明准入逻辑待后续任务接入。定向 Vitest 3/3、Web 构建通过；Playwright 本地真实浏览器在接口 mock 下完成桌面和 390px 窄屏检查、保存交互及 PUT 载荷确认，无页面横向溢出或控制台错误。未部署。

#### LUX-618：为共享播放准入建立数据表

范围：为后续共享播放准入保存每位用户、播放会话的占位状态及准入/续租时间。SQLite 与 PostgreSQL 均迁移；本任务只建立 schema 与升级回归，不接入存储操作或播放路由。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0186_user_playback_admissions.sql`、`migrations-postgres/0186_user_playback_admissions.sql`、`src/storage/repository_tests.rs`、`tests/postgres_database.rs`。

验收标准：

- [x] 两种数据库都建立用户外键、`(user_id, play_session_id)` 主键、有效状态与时间约束，以及状态/续租时间索引。
- [x] SQLite 从空库应用迁移及从旧 schema 升级均通过；既有播放记录保留。
- [x] PostgreSQL bootstrap 回归加入占位表字段、约束和索引断言。
- [x] SQLite 定向迁移测试、PostgreSQL 集成目标编译和 Rust 格式检查通过。

验证：`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib user_playback_admission_migration`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test postgres_database`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。Cargo 前确认 Toshiba 已挂载。

依赖：LUX-617。

结果（2026-10-19）：SQLite 定向迁移回归 1/1 通过，覆盖新库 schema、复合主键、用户外键、状态与时间约束，以及旧播放记录保留。PostgreSQL 集成目标成功编译；19 个测试因需要本地 PostgreSQL 全部 ignored，因此 bootstrap 断言尚未运行时验证。Rust 格式检查与 `git diff --check` 通过。此任务只建立 schema，未实现准入逻辑。

#### LUX-619：原子执行共享播放准入

范围：在 storage 层增加播放会话准入、续租和释放操作。SQLite 用立即写事务串行准入，PostgreSQL 锁定用户行；计数合并活跃占位与 `playback_sessions` 中活跃/暂停会话，按用户和播放会话 ID 去重。沿用 90 秒活跃窗口；`0` 表示不限，禁止播放策略拒绝准入，同一现有会话在额度降低后可继续续租。此任务不接入 Web/Emby/STRM 路由。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/sessions.rs`、`src/storage/mod.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 并发争抢最后一个名额时，数据库只准入一个新会话；同一会话续租不重复占用名额。
- [x] 旧播放记录与新占位按会话去重；暂停会话计入额度，停止释放，超过 90 秒的未续租占位不再计数。
- [x] 并发上限降低时保留现存会话并拒绝新会话；`0` 不限，`can_play=false` 拒绝准入。
- [x] SQLite 定向存储回归、Rust 格式检查通过；PostgreSQL 分支编译验证通过。

验证：`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib playback_admission`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。Cargo 前确认 Toshiba 已挂载。

依赖：LUX-618、LUX-617。

结果（2026-10-19）：`playback_admission` 定向存储测试 3/3 通过，覆盖同用户并发争抢、续租复用、停止释放、旧播放记录去重、暂停计数、额度下调、不限额、禁播和 90 秒过期回收。PostgreSQL 集成目标编译通过，但 19 个测试均因需要本地 PostgreSQL 而 ignored；PostgreSQL 行锁逻辑尚未运行时验证。Rust 格式检查与 `git diff --check` 通过。播放入口接入留给后续任务。

#### LUX-620：Web 播放入口接入准入占位

范围：Web 创建播放会话/Bootstrap 时只检查当前策略与额度，不占用名额；Web 直放、STRM Range 与服务端 HLS 的实际 GET 请求原子准入。同一会话后续分片、Range、播放回调及已准入心跳复用名额；暂停保留名额，停止、删除、失败时释放。HEAD 和被动信息查询不占名额。拒绝响应需明确说明当前播放数与上限。本任务不接入 Emby PlaybackInfo/视频取流接口。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/sessions.rs`、`src/application/playback/session.rs`、`src/api/playback.rs`、`tests/web_playback.rs`。

验收标准：

- [x] Web 发链接只预检；只有实际媒体 GET 才创建准入记录，重复访问同一会话不重复占用。
- [x] 活跃心跳续租，暂停仍计数；停止、删除和失败会话释放名额，失效心跳不创建新名额。
- [x] 达到上限或禁止播放时返回包含当前播放数与策略上限的错误；竞争发生在发链接之后时，实际取流仍再次原子拒绝。
- [x] Web playback 定向集成测试及 Rust 格式检查通过。

验证：`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test web_playback`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。Cargo 前确认 Toshiba 已挂载。

依赖：LUX-619。

结果（2026-10-19）：Web playback 定向集成测试 1/1 通过，覆盖直放、STRM Range 与服务端 HLS 的发链接预检和实际取流准入；HEAD 不占位，重复请求复用会话名额。测试也验证了播放上限/禁播错误详情、暂停计数、活跃心跳续租、失效心跳不续占、STOPPED/删除/取流失败释放名额。`cargo fmt --all -- --check` 与 `git diff --check` 通过。集成测试访问 localhost 时设置 `NO_PROXY=127.0.0.1,localhost`，避免开发机代理截获本地请求。

#### LUX-621：将 Emby 直放票据绑定到播放会话

范围：Emby `PlaybackInfo` 签发的直放 URL 必须将其 `PlaySessionId` 纳入 HMAC 票据，并在流请求时验证同一 ID。防止客户端修改 URL 中的会话 ID、借此拆分并发计数；现有 URL 字段、外部代理交接和票据有效期保持兼容。此任务只建立可验证的会话身份，不实施 Emby 播放准入。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/playback/session.rs`、`src/api/emby_catalog.rs`、`src/api/media.rs`、`tests/playback.rs`。

验收标准：

- [x] 直放票据签名包含用户、条目、媒体源、播放会话 ID 和过期时间。
- [x] 票据缺少或篡改 `PlaySessionId` 时，Emby 取流返回未授权；原签名 URL 仍可播放。
- [x] Emby playback 定向集成测试及 Rust 格式检查通过。

验证：定向测试 `local_file_stream_supports_full_head_range_acl_and_path_safety` 通过（1/1）；`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 和 `git diff --check` 通过。Cargo 前确认 Toshiba 已挂载。

依赖：LUX-620。

#### LUX-622：接入 Emby 直放准入

范围：Emby `PlaybackInfo` 签发媒体链接前只读检查 `can_play` 与当前并发额度；额度不足时返回包含当前播放数和上限的错误，且不创建 HLS 会话。Emby 直放链接的实际 GET（含 Range）及 STRM 外部 URL 交接时，按 `PlaySessionId` 原子准入；HEAD 不占位，同一会话重复取流复用名额。Emby `PLAYING`/`PAUSED` 回调只续租已有且未过期的准入占位，不创建准入占位；`STOPPED` 回调停止并释放相同会话。兼容未带 Lux 签名票据的旧直放请求，并使用 Emby 回调共享的稳定会话身份，不接受客户端任意更换 ID 拆分并发计数。本任务不接入 Emby HLS 实际分片准入。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/playback/session.rs`、`src/api/playback.rs`、`src/api/media.rs`、`tests/playback.rs`。

验收标准：

- [x] `PlaybackInfo` 额度预检不占位；达到上限或禁止播放时返回当前播放数和上限，不创建 HLS 会话。
- [x] 直放/STRM 首次 GET 原子占位，HEAD 不占位；Range 与同一 `PlaySessionId` 的重复 GET 复用名额，超额新会话被拒绝且不影响已有会话。
- [x] 有效 `PLAYING`/`PAUSED` 回调续租，`STOPPED` 释放；没有有效占位的回调不新建准入记录。
- [x] Emby playback 定向集成测试、Rust 格式检查和 `git diff --check` 通过。

验证：`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test playback` 通过（4/4）；`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 和 `git diff --check` 通过。Cargo 前确认 Toshiba 已挂载。

依赖：LUX-619、LUX-621。

结果（2026-10-19）：PlaybackInfo 仅做准入预检，满额时拒绝 direct 与强制转码请求，未创建 HLS 会话。签名直放 GET 和 STRM 302 交接均按会话占位；HEAD 不占位，Range 复用占位，管理员并发上限从 1 到达后新会话被拒绝，原会话保留。PAUSED 续租、STOPPED 释放已验证；无播放 GET 的回调不创建占位。旧式直放使用条目/设备稳定会话 ID，STOPPED 无 PlaySessionId 时可释放。未部署。

#### LUX-623：接入 Emby HLS 实际准入

范围：Emby HLS manifest、初始化文件及媒体分片的实际 GET 在签名、会话、条目、源和资产均验证并可提供内容后，按对应 `lux-emby` `PlaySessionId` 原子准入；同一 HLS 会话的 manifest、分片、重复请求复用名额。HEAD 和 PlaybackInfo 预检不占位。满额或禁播时返回当前播放数与上限，并停止未准入的新 HLS 会话以回收其服务端资源；不得停止已有获准会话。Emby STOPPED 回调释放准入。本任务仅接入 Emby HLS，不改变 Web HLS 或 FFmpeg 服务器级并发策略。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/api/playback.rs`、`tests/playback.rs`。

验收标准：

- [x] PlaybackInfo 创建 HLS 会话后无准入记录；HEAD manifest 不占位，实际 GET manifest 才原子占位。
- [x] 后续 HLS 初始化文件/分片 GET 与重复请求复用同一名额；额度满时拒绝另一条已签发 HLS 会话并清理该会话，已有会话仍可继续读取。
- [x] STOPPED 回调释放对应 HLS 准入记录；拒绝或 HEAD 不创建准入记录。
- [x] Emby HLS 定向集成测试、Rust 格式检查和 `git diff --check` 通过。

验证：`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test playback` 通过（4/4）；`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 和 `git diff --check` 通过。Cargo 前确认 Toshiba 已挂载。

依赖：LUX-619、LUX-622。

结果（2026-10-19）：PlaybackInfo 创建 HLS 会话、HEAD manifest 均不占位；首次成功 manifest GET 占用一席，初始化文件/媒体分片共用并续租同一会话。并发上限 1 时，另一条已签发链接的 GET 返回 429，停止该 HLS 会话并删除其资源；已获准分片仍可读取。STOPPED 释放会话占位。未部署。

#### LUX-624：按用户策略筛选服务端播放档位

范围：Web 与 Emby 创建本地媒体 HLS 会话时，按当前用户 `allow_remux`、`allow_audio_transcode`、`allow_video_transcode` 与服务端可用能力共同选择最低允许档位。当前硬件/软件视频转码输出同时转码音频，因此必须同时允许音频与视频转码。直放不受转码权限影响；STRM 始终只允许外部直放。客户端请求了服务端播放但其可用档位全部被用户策略禁止时，拒绝请求且不创建 HLS 会话。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/playback/session.rs`、`src/api/playback.rs`、`tests/web_playback.rs`、`tests/playback.rs`。

验收标准：

- [x] 直放优先且不受转码权限影响；STRM 不创建 Remux、转码或 HLS 会话。
- [x] Remux、音频转码、硬件/软件视频转码分别遵守用户策略；禁止低档位时可选择其他允许且服务器可用的档位。
- [x] 所有兼容档位都被禁止时，Web 与 Emby 明确拒绝，不创建或遗留 HLS 会话。
- [x] Web 与 Emby 定向集成测试，以及 Rust 格式检查和 `git diff --check` 通过。

验证：`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test web_playback`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test playback`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。Cargo 前确认 Toshiba 已挂载。

依赖：LUX-614、LUX-615、LUX-617、LUX-620、LUX-623。

结果（2026-10-19）：Web 与 Emby 共用的会话服务按用户策略和服务器能力选择最低允许 HLS 档位；硬件/软件视频转码还要求音频转码许可，因为当前 FFmpeg 输出同时转码音频。所有可用转码档位被禁用时，在写入会话或启动 HLS 前返回 403。直放及 STRM 的外部直放决策保持原样。策略单元测试 3/3、`web_playback` 1/1、`playback` 4/4 通过；`cargo fmt --all -- --check` 与 `git diff --check` 通过。`cargo clippy --locked --all-targets --all-features -- -D warnings` 被两个既有 `clippy::too_many_arguments` 报错阻断：`src/api/media.rs::serve_emby_media_file` 与 `src/application/playback/session.rs::verify_emby_direct_stream`，本任务未修改这两个函数签名。未部署。

#### LUX-625：复核已签发 HLS 链接的转码权限

范围：每次授权 Web 或 Emby 的签名 HLS manifest、初始化文件、媒体分片前，重新读取该用户当前播放策略，并按会话已存档位判断是否仍允许。Remux 需要 `allow_remux`，音频转码需要 `allow_audio_transcode`，硬件/软件视频转码需要同时允许视频和音频转码（当前 HLS 输出会转码两者）。策略变更撤销了当前会话所需权限时，拒绝本次资源请求，停止 HLS、释放播放准入并删除临时资源。仅检查 HLS 会话，不改变直放与 STRM 外部交接合同。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`docs/COMPATIBILITY.md`、`src/application/playback/session.rs`、`tests/web_playback.rs`、`tests/playback.rs`。

验收标准：

- [x] 每次 HLS manifest/初始化文件/分片请求（包含 HEAD）使用当前策略校验会话档位；权限已撤销时不返回 HLS 资源。
- [x] 权限撤销时会话停止、准入名额释放、HLS 临时目录清理；原票据后续重用仍被拒绝。
- [x] Web 与 Emby 集成回归覆盖已签发链接被撤权后的取流行为；直放和 STRM 仍遵守现有边界。
- [x] 在兼容性文档记录 Emby 策略拒绝与撤权后签名 HLS 链接的响应合同。
- [x] 定向 `web_playback` 与 `playback` 测试、Rust 格式检查和 `git diff --check` 通过。

验证：`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test web_playback`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test playback`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。Cargo 前确认 Toshiba 已挂载。

依赖：LUX-624。

结果（2026-10-19）：共享签名资源授权在每次 Web/Emby HLS manifest、初始化文件或分片请求（HEAD 也包含）读取当前策略。撤销会话所需转码权限后，请求返回 403，HLS 会话停止、准入释放、临时目录清除；再次使用票据返回 410。Web 测试覆盖 tier 2 与 HEAD 拒绝，Emby 测试覆盖硬件/软件视频转码档撤权；定向 `web_playback` 1/1、`playback` 4/4 通过，`cargo fmt --all -- --check` 与 `git diff --check` 通过。Emby 策略拒绝及 HLS 票据失效响应合同已加入兼容性文档。未部署。

#### LUX-626：取消用户外挂字幕权限模块

范围：按项目所有者决定取消用户管理方案中的模块八。移除用户详情页外挂字幕占位和用户级字幕权限、上传/替换/删除字幕文件的规划；保留现有字幕播放、字幕元数据编辑、扫描与读取合同，不改变这些既有能力。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`docs/USER-MANAGEMENT-DESIGN.md`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/tests/admin-user-profile-lifecycle.test.tsx`。

验收标准：

- [x] 用户详情导航不再出现模块八或外挂字幕权限入口，模块一至七入口不变。
- [x] 用户管理设计将模块八明确标为取消，并从默认值、服务接口、数据能力、阶段门和验收清单中移除用户级字幕权限及文件管理要求。
- [x] 模块四的默认权限与复制范围不再包含用户级字幕权限。
- [x] 明确现有字幕播放、管理字幕元数据、索引与扫描功能保持既有合同。

短计划：先更新设计与任务规格，删去用户详情的已取消导航项，复查用户管理设计中的模块八引用；不修改字幕播放、元数据或扫描实现。

验证：`pnpm --dir web exec vitest run tests/admin-user-profile-lifecycle.test.tsx`、`pnpm --dir web build`、`git diff --check`。

依赖：LUX-593。

结果（2026-10-19）：新增导航回归先在原 8 项入口上失败（预期 7、实际 8），移除已取消的外挂字幕入口后，`admin-user-profile-lifecycle` 6/6 通过；Web production build 与 `git diff --check` 通过。用户管理方案和 LUX-593 页面结构同步移除用户级字幕权限；模块五仍约束已有字幕读取的媒体访问，现有字幕播放、元数据编辑与扫描能力未改。

#### LUX-627：为显式媒体库访问模式建立迁移

范围：为用户增加受约束的 `library_access_mode`（`ALL` / `SELECTED` / `NONE`）字段。旧账号按迁移前实际可见范围回填：存在 `can_view=1` 记录时为 `SELECTED`，否则为 `ALL`，从而保持旧行为。字段默认 `ALL` 仅作为兼容性数据库回退；新用户默认权限由后续模块四任务显式写入。本任务只改 schema/migration，不改读取 ACL、管理 API 或新用户创建行为。

预计修改文件（12 个）：`docs/LUX-DEVELOPMENT.md`、`docs/USER-MANAGEMENT-DESIGN.md`、`migrations/0187_user_library_access_mode.sql`、`migrations-postgres/0187_user_library_access_mode.sql`、`src/storage/repository_tests.rs`、`tests/storage.rs`、`tests/scanning_jobs.rs`、`tests/danmaku.rs`、`tests/postgres_database.rs`、`tests/admin_health.rs`、`tests/scanner.rs`、`tests/ready_version.rs`。

验收标准：

- [x] SQLite 与 PostgreSQL 都提供仅允许 `ALL`、`SELECTED`、`NONE` 的非空字段，空库完整启动可应用迁移。
- [x] 旧用户无正向授权记录（包括只有 `can_view=0` 的历史记录）迁移为 `ALL`；存在正向记录迁移为 `SELECTED`。
- [x] 新迁移保留既有媒体库授权表记录，且不改动任何读取行为。
- [x] SQLite 迁移回归、格式检查和 `git diff --check` 通过；受影响的 schema 版本接口/存储目标定向回归通过。

短计划：在主规格与方案中确认旧/新模式语义，先写旧 SQLite 授权数据迁移回归，再补 SQLite/PostgreSQL migration；更新空库启动、PostgreSQL 启动与既有最新 schema 版本断言；不改 ACL 查询和 API。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib user_library_access_mode_migration_preserves_legacy_semantics`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test storage user_library_access_mode_migration_runs_from_an_empty_database -- --exact`、受影响的 `admin_health` / `ready_version` / `scanner` / `scanning_jobs` / `danmaku` 定向用例、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test postgres_database --no-run`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test postgres_database postgres_bootstrap_runs_migrations_and_persists_core_state -- --ignored --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test postgres_database postgres_user_library_access_mode_migration_preserves_legacy_semantics -- --ignored --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-593、LUX-626。

结果（2026-10-19）：SQLite 对无授权/仅拒绝/正向授权账号分别回填 `ALL` / `ALL` / `SELECTED`，保留授权行并拒绝非法模式；PostgreSQL 16 运行时升级回归得到相同回填结果，并验证模式字段非空且拒绝非法值。PostgreSQL 空库完整启动至 schema 188，验证默认 `ALL`。两项 PostgreSQL 专项用例均 1/1；旧 SQLite 回归、受影响 schema 版本接口和存储目标、格式及 diff 检查通过。完整 `storage` 目标串行运行 63/63 通过（并行运行时旧 cooldown 时间窗口用例曾波动）。空库启动测试将初始管理员创建移至其他测试账号之前，避免测试本身提前完成初始化导致 `AlreadyCompleted`。未部署。

#### LUX-628：按显式模式执行媒体库访问检查

范围：让存储层 ACL、授权播放源和全局合集来源筛选按 `ALL` / `SELECTED` / `NONE` 判断；更新旧的单库及批量授权写入，使其事务内继续按旧界面语义在有正向授权时写入 `SELECTED`、无正向授权时写入 `ALL`。不开放 `NONE` 的管理界面，本任务不改管理员接口形状、新用户默认策略或内容分级规则。

预计修改文件（16 个）：`docs/LUX-DEVELOPMENT.md`、`docs/USER-MANAGEMENT-DESIGN.md`、`src/storage/users.rs`、`src/storage/catalog.rs`、`src/storage/repository_tests.rs`、`tests/catalog.rs`、`tests/collections.rs`、`tests/search.rs`、`tests/playback.rs`、`tests/resume_favorites.rs`、`tests/emby_counts.rs`、`tests/series_api.rs`、`tests/remote.rs`、`tests/emby_global_collection_playback.rs`、`tests/subtitles.rs`、`tests/people_api.rs`。

验收标准：

- [x] 用户媒体库检查和可访问库列表严格遵循三种模式；未知用户、`NONE` 不可读，`ALL` 含之后新增的启用库，`SELECTED` 只含正向授权库。
- [x] 播放源、合集卡片、合集成员、合集来源及来源偏好写入的 SQL 过滤均遵循同一模式判定。
- [x] 旧管理接口单条与批量授权写入事务性地同步模式，现有“清空所选后恢复全部”行为保持不变。
- [x] 直接插入旧授权记录的测试夹具显式设置 `SELECTED`；相关存储与媒体访问目标通过。

短计划：先新增 `ALL` / `SELECTED` / `NONE` 的 SQLite 存储回归并确认 `NONE` 在旧推断逻辑下失败；再改存储授权查询和全局合集 SQL；在旧接口写入中用同一事务更新模式，最后为依赖旧正向授权夹具的集成目标显式设置 `SELECTED`。

验证：检查 Toshiba 挂载状态后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib user_library_access_modes_are_enforced_by_storage_queries` 与受影响的 `catalog`、`collections`、`search`、`playback`、`resume_favorites`、`emby_counts`、`series_api`、`remote`、`emby_global_collection_playback`、`subtitles`、`people_api` 测试目标，再运行 `cargo fmt --all -- --check` 与 `git diff --check`。

依赖：LUX-627。

结果（2026-10-19）：存储授权和合集/播放 SQL 已按显式模式过滤；旧单条、批量写入在事务内同步为 `SELECTED` 或 `ALL`。`ALL` / `SELECTED` / `NONE`（包括 `NONE` 覆盖既有正向授权）、新启用媒体库、未知用户及清空选择回归通过；全局合集和直接播放 `NONE` 拒绝回归通过。相关 `catalog`、`collections`、`search`、`playback`、`resume_favorites`、`emby_counts`、`series_api`、`remote`、`emby_global_collection_playback`、`subtitles`、`people_api` 目标及存储批量写入回归均通过；`cargo fmt --all -- --check` 与 `git diff --check` 通过。PostgreSQL 运行时仍因本机服务不可用而未验证；未部署。

#### LUX-629：管理员显式设置用户媒体库访问模式

范围：为现有用户媒体库管理接口返回明确的 `ALL` / `SELECTED` / `NONE` 模式，并新增整组替换接口。`SELECTED` 接受启用的普通媒体库 ID；空选择规范化为 `NONE`。`ALL` 与 `NONE` 清除旧选择行并保存明确模式。既有逐库 PATCH 接口保持 LUX-628 的兼容行为。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/users.rs`、`src/application/admin_user_access.rs`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [x] 查询返回明确模式；不存在用户返回 404。
- [x] 整组更新原子替换模式与选择；`SELECTED` 空列表变成 `NONE`，库 ID 必须存在、启用且不能重复。
- [x] 旧逐库 PATCH 与已有权限行为兼容，输入无效时不产生部分更新。

短计划：先在用户 HTTP 集成测试覆盖明确模式查询、三种模式更新和无效输入；再增加存储整组替换事务，接入 application 校验与管理员 GET/PUT；最后运行 users 定向目标、相关存储回归、格式与 diff 检查。

验证：确认 Toshiba 挂载状态后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test users admin_can_manage_users_and_last_manager_is_protected -- --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib user_library_access_modes_are_enforced_by_storage_queries`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 与 `git diff --check`。

依赖：LUX-627、LUX-628。

结果（2026-10-19）：管理员媒体库权限 GET 返回显式模式和选择列表；新增 CSRF 保护的 PUT 整组替换，校验用户、模式、启用媒体库和重复 ID，并将 `SELECTED` 空选择规范化为 `NONE`。存储事务回归确认中途插入失败会回滚旧选择与模式；HTTP 集成回归覆盖三种模式、无效输入、CSRF 和旧 PATCH 路径。users 定向测试、存储回滚测试、格式与 diff 检查通过。一次首跑在后续既有账号删除断言收到 503，设置本地代理绕行后的复跑通过；该瞬态未能再次复现。未部署。

#### LUX-630：同步媒体库访问模式 API 文档

范围：同步现行媒体库 ACL 规格与管理员 API 文档，说明显式三模式、整组替换接口、校验规则，以及旧逐库 PATCH 的兼容行为。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`。

验收标准：

- [x] 主规格不再把“选择为空即全部”作为唯一的权限表达方式。
- [x] API 文档说明 GET 返回的模式和选择，PUT 可设置三种模式并校验媒体库 ID；逐库 PATCH 行为单独说明。

短计划：将 LUX-036 的当前权限合同更新为显式模式，再补管理员 GET/PUT/PATCH 路由及请求、响应和错误语义。

验证：检查主规格和代码路由一致，`git diff --check` 通过；本任务只改文档，不运行代码测试。

依赖：LUX-628、LUX-629。

结果（2026-10-19）：主规格与 `docs/API.md` 已同步 `ALL` / `SELECTED` / `NONE`，列明旧 PATCH 的兼容语义以及 PUT 对不存在、未启用和重复媒体库的处理。文档核对与 diff 检查通过。

#### LUX-631：Web API 客户端支持显式媒体库权限

范围：为 Web API 类型声明媒体库访问模式；管理员权限 GET 返回强类型模式与选择，客户端增加整组 PUT 更新方法；保留旧逐库 PATCH 方法供兼容调用。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] TypeScript 类型明确限定 `ALL` / `SELECTED` / `NONE`，GET/PUT 响应不再可省略模式。
- [x] PUT 发送正确路由、JSON 与 CSRF；旧 PATCH 请求合同保持不变。
- [x] API 客户端定向 Vitest 通过。

短计划：先新增 fetch mock 回归确认 GET 与 PUT 合同，再增加强类型与客户端方法；运行 API 客户端 Vitest、Web build 和 diff 检查。

验证：`pnpm --dir web exec vitest run tests/api-client.test.ts`、`pnpm --dir web build` 与 `git diff --check`。

依赖：LUX-629、LUX-630。

结果（2026-10-19）：新增 `AdminUserLibraryAccessMode` 与 GET/PUT 请求响应类型，客户端 PUT 方法发送显式模式和选择；旧逐库 PATCH 方法保持不变。新增 fetch mock 验证路由、请求体和 CSRF。`api-client.test.ts` 63/63 通过，`pnpm --dir web build` 通过。

#### LUX-632：在用户详情管理显式媒体库权限

范围：将用户媒体库权限编辑放入用户详情“权限”模块，允许管理员选择全部媒体库、指定媒体库或无媒体库权限；指定模式仅列出已启用媒体库，使用显式保存，不再把空复选框解释为“全部”。回收站账号只读。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUsersPage.tsx`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/src/features/admin/user-management.css`、`web/tests/admin-users-access.test.tsx`。

验收标准：

- [x] 用户详情权限页读取并显示服务端三态模式；选择 `SELECTED` 时仅展示启用媒体库。
- [x] 管理员可草拟模式/库选择并显式保存；空指定项按 `NONE` 显示，保存错误保留草稿并给出提示。
- [x] 用户列表不再存在空选择即全部的旧控件；回收站账号只读，加载失败不展示默认权限假象。
- [x] 定向 Vitest 与 Web build 通过。

短计划：先将现有 UI 回归改为详情页三态和保存行为；把 API 读写接入权限模块，再从列表移除含糊的逐库复选框；补充可访问的单选控件、指定库复选框、保存状态与错误提示。

验证：`pnpm --dir web exec vitest run tests/admin-users-access.test.tsx`、`pnpm --dir web build` 与 `git diff --check`。

依赖：LUX-593、LUX-631。

结果（2026-10-19）：详情页权限区改为显式三态和保存，指定范围只列已启用媒体库，空选择保存为 `NONE`；列表移除原先逐库权限编辑控件，回收站权限只读。定向 `admin-users-access`、用户资料生命周期和播放策略 Vitest 共 15/15 通过，`pnpm --dir web build` 与 `git diff --check` 通过。

#### LUX-633：存储新用户默认权限设置

范围：在管理员全局设置中增加新用户权限快照配置与持久化，包含显式媒体库模式/媒体库 ID、下载权限和模块六的五项播放策略。默认媒体库为指定空集合（即无权限），新用户默认允许播放、同时播放 2 路、允许当前服务器支持范围内的封装/音频/视频转换，并默认禁止下载。只增加设置读写，不改变用户创建、邀请或预设行为。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/users.rs`、`src/application/settings.rs`、`src/api/admin/admin_settings.rs`、`tests/admin_dashboard.rs`。

验收标准：

- [x] 管理员 GET 可读默认快照；首次读取返回明确、安全的设计默认值。
- [x] CSRF 保护的 PATCH 可保存完整默认快照；空 `SELECTED` 规范化为 `NONE`，`ALL/NONE` 不接受媒体库 ID。
- [x] 仅接受已启用普通媒体库、唯一 ID、非负播放上限；无效请求返回 400 且不改变已保存值。
- [x] 定向 `admin_dashboard` Rust 集成测试、格式检查和差异检查通过。

短计划：先扩展管理员设置集成回归，验证默认值、保存、空选择规范化和非法快照拒绝；再在 storage 用 `server_settings` 键持久化 JSON，在 application 层验证和归一化，最后扩展设置 DTO 与错误映射。不改用户创建路径。

验证：`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test admin_dashboard`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。Cargo 前确认 Toshiba 已挂载。

依赖：LUX-614–625、LUX-627–632。

结果（2026-10-19）：管理员设置新增默认权限快照，首次返回 `NONE` 媒体库范围、禁止下载、允许播放且并发上限 2；`server_settings` 以单个 JSON 项持久化，无需迁移。空指定项归一为 `NONE`，重复/停用库、非空 `ALL/NONE` 和负并发上限均拒绝，失败不覆盖已保存值。`admin_dashboard` 1/1、fmt 与 diff check 通过。公开 API 文档同步由 LUX-634 完成。

#### LUX-634：记录新用户默认权限设置 API

范围：在 `docs/API.md` 记录管理员设置响应/请求中的 `newUserPermissions` 形状、初始默认值、媒体库模式校验和 CSRF 边界。明确该接口当前只配置快照，实际应用至新账号由后续任务接入。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`。

验收标准：

- [x] API 文档包含全部媒体库、下载与五项播放策略字段及默认值。
- [x] 文档说明空指定集合归一为 `NONE`、`ALL/NONE` 禁止媒体库 ID、无效请求返回 400。
- [x] 文档明确管理员 Web session + CSRF 和新账号应用仍待后续实现。
- [x] `git diff --check` 通过。

短计划：核对已实现 DTO 和集成回归后，将该资源契约补充到管理员设置 API 条目；不改代码或接口行为。

验证：`git diff --check`。

依赖：LUX-633。

结果（2026-10-19）：`docs/API.md` 记录 `newUserPermissions` 全字段、默认值、完整快照更新、媒体库模式归一/拒绝规则、管理员 session + CSRF 和“尚未应用于新账号创建”的边界；`git diff --check` 通过。

#### LUX-635：在用户全局设置页编辑新用户默认权限

范围：将 `newUserPermissions` 接入“新用户默认权限”设置页，支持三态媒体库模式、启用媒体库选择、下载和五项播放/转码权限的完整快照编辑；保存成功更新管理员设置缓存，失败保留草稿。此任务只配置默认值，不改变用户创建行为。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/src/features/admin/user-management.css`、`web/src/lib/api/types.ts`、`web/tests/admin-user-default-permissions.test.tsx`。

验收标准：

- [x] 设置页读取服务端快照；媒体库选择只显示已启用普通库，并说明空指定保存为无权限。
- [x] 管理员可编辑媒体库模式、下载与播放/转码权限，显式保存完整对象。
- [x] 并发上限只接受非负整数；保存失败保留草稿并显示错误，成功更新缓存。
- [x] 定向 Vitest、Web build 与差异检查通过。

短计划：先为默认权限模块写三态加载、完整保存、非法并发上限和失败保留回归；再扩展 API 类型与设置页分支，使用现有表单控件样式增加默认权限布局。

验证：`pnpm --dir web exec vitest run tests/admin-user-default-permissions.test.tsx`、`pnpm --dir web build`、`git diff --check`。

依赖：LUX-633、LUX-634。

结果（2026-10-19）：全局“新用户默认权限”页接入三态媒体库模式、已启用媒体库选项、下载与播放/转码策略及非负并发上限；空指定集合提示为无媒体库权限，错误时草稿保留，成功刷新设置缓存。定向 Vitest 7/7、Web build 和 diff check 通过。Playwright 使用本机临时 SQLite 服务完成保存并刷新恢复；390px 视口文档宽度 375px、无页面横向溢出，控制台无错误。桌面与窄屏截图保存于 `output/playwright/user-default-permissions/.playwright-cli/`。默认值应用至新账号仍由后续任务实现。

#### LUX-636：原子创建带权限快照的账号

范围：为用户存储层增加带初始权限快照的创建路径，在同一数据库事务中写入账号字段和指定媒体库授权；保留现有创建方法行为。应用层后续负责读取并传入已验证的新用户默认权限。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/auth/users.rs`、`src/storage/mod.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 新的账号创建入口将下载、播放、并发、转码和媒体库模式写入账号记录。
- [x] 指定媒体库授权与账号插入处于同一事务，任一写入失败时不留下半创建账号。
- [x] 旧的账号插入调用保持原默认行为。
- [x] 定向 Rust 回归、fmt 和 diff check 通过。

短计划：先为指定媒体库快照与权限字段写存储回归，再增加存储快照类型、事务插入实现及 auth 创建入口；验证已有创建路径不变。

验证：`cargo test --locked --lib <新存储回归过滤器>`、`cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-627、LUX-628、LUX-620、LUX-633。

结果（2026-10-19）：存储新增初始用户权限快照，账号字段和指定媒体库授权在单个事务写入；正向保存和授权插入触发失败回滚的定向回归 2/2 通过，旧插入路径未改。`cargo build --locked`、fmt check 与 diff check 通过。全量 `cargo test --locked --all-targets` 为 985 passed、5 failed、25 ignored；失败为 `scan_local_nfo_page_flushes_actor_credits_and_isolates_invalid_nfo`、`stops_reading_a_chunked_response_as_soon_as_it_exceeds_the_limit`、`automatic_and_explicit_fill_missing_jobs_keep_dispatch_sources_separate`、`automatic_fill_missing_retry_backoff_excludes_manual_modes_and_cancelled_failures`、`changed_local_fill_request_during_running_item_is_not_lost`。全量 Clippy 被未修改的 `src/api/media.rs:2821` 与 `src/application/playback/session.rs:397` 两处 `too_many_arguments` 阻断。管理员直建由 LUX-637 接入默认快照，Emby 管理员建号由 LUX-638 接入。

#### LUX-637：管理员直建账号应用默认权限快照

范围：管理员通过 Lux API 创建普通账号时，读取已保存的新用户默认权限并传入原子创建路径；账号资料响应及后续权限查询反映实际快照。设置读取失败时不创建账号。本任务不改 Emby 创建和用户权限复制。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/settings.rs`、`src/auth/users.rs`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [x] 新建普通账号完整取得配置的媒体库、下载和播放/转码快照。
- [x] 创建响应中的下载权限与存储一致，播放策略和媒体库模式查询均返回配置值。
- [x] 默认设置读取失败时返回服务不可用，不留下账号。
- [x] 定向 `users` 集成回归、Rust build、fmt 和 diff check 通过。

短计划：先增加管理员设置保存后创建账号的失败回归；暴露只读默认值读取与快照映射，再让管理员创建 handler 调用权限快照创建方法。

验证：`cargo test --locked --test users`、`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-633、LUX-636。

结果（2026-10-19）：管理员直建读取服务端默认快照并使用原子创建；返回的 `canDownload`、播放策略及媒体库模式均与设置一致。定向 `users` 集成测试 2/2、Rust build、fmt check 与 diff check 通过。删除设置表模拟读取故障时 API 返回 503，账号未写入；Emby 创建和公开 API 文档同步由 LUX-638 完成。

#### LUX-638：Emby 管理员建号应用默认权限并同步 API 文档

范围：Emby `/Users/New` 管理员创建账号时应用同一份新用户默认权限快照，并继续执行现有用户策略/配置复制选项；默认设置读取失败时不创建账号。同步公开 API 文档中的快照应用范围和管理员建号行为。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/api/emby_handlers.rs`、`src/auth/users.rs`、`tests/emby_auth.rs`。

验收标准：

- [x] Emby 管理员创建账号取得默认的媒体库、下载和播放/转码策略。
- [x] 显式 `UserPolicy` / `UserConfiguration` 复制仍按现有顺序执行。
- [x] 默认设置读取失败时返回服务不可用，不留下账号。
- [x] Emby 创建回归、Rust build、fmt、diff check 通过；API 文档不再称默认值只保存未应用。

短计划：先修改 Emby 建号集成测试并确认默认策略断言失败；再接入 passwordless 权限快照创建，最后同步管理员设置与用户创建 API 文档。

验证：`cargo test --locked --test emby_auth`、`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-633、LUX-636、LUX-637。

结果（2026-10-19）：Emby `/Users/New` 管理员创建账号时读取同一份默认权限快照并原子保存，设置读取失败返回 503 且无账号残留；既有 `UserPolicy` / `UserConfiguration` 集成回归保持通过。Emby 定向测试 9/9、Rust build、fmt check、diff check 通过。同步 `docs/API.md`，说明默认快照在 Lux/Emby 管理员建号中的应用及复制选项的覆盖顺序。全量 Clippy 仍被未修改的 `src/api/media.rs:2821` 和 `src/application/playback/session.rs:397` 的 `too_many_arguments` 阻断。

#### LUX-639：实现权限预设的版本化存储与业务校验

范围：建立管理员、普通用户内置预设及自定义预设的数据模型，在现有 `server_settings` 中保存自定义权限快照；支持创建、更新、删除的业务操作、版本冲突检测和输入校验。本任务不暴露 HTTP 接口或管理页面，不把预设自动应用到已有用户。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/mod.rs`、`src/application/settings.rs`、`src/application/user_presets.rs`、`src/storage/users.rs`。

验收标准：

- [x] 列表包含管理员和普通用户内置预设；内置预设不能修改或删除。
- [x] 自定义预设支持创建、版本检查更新和删除，名称唯一且数量有上限。
- [x] 媒体库模式、下载、播放、并发与转码策略按新用户默认权限规则验证；空选择归一化为无权限。
- [x] 并发管理更新使用存储层比较并交换，冲突不会丢失另一份已保存预设。
- [x] 业务/存储定向回归、Rust build、fmt 和 diff check 通过。

短计划：先写内置预设、校验、版本冲突和存储并发回归；再实现自定义预设 JSON 持久化 CAS 与 application CRUD 服务。

验证：`cargo test --locked --lib user_presets`、`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-633、LUX-636。

结果（2026-10-19）：新增管理员/普通用户内置预设、自定义预设版本化 CRUD，权限字段复用新用户默认权限的结构校验；空的 `SELECTED` 归一化为 `NONE`，指定媒体库须仍为启用的普通媒体库。`server_settings` 使用单条 SQL 比较并交换，自定义名称不区分大小写且最多 100 项。定向 `user_presets` 5/5、`cargo build --locked`、fmt check、diff check 通过。`cargo test --locked --all-targets` 的库测试部分为 991 passed、4 failed、25 ignored；3 项补全队列基线失败重现，另有 `local_metadata_worker_dispatches_fill_missing_without_blocking_scan` 超时，因库测试失败后续集成 targets 未运行。全量 Clippy 仍被未修改的 `src/api/media.rs:2821` 和 `src/application/playback/session.rs:397` 两处 `too_many_arguments` 阻断。预设 HTTP API 和 UI 留待后续模块四任务。

#### LUX-640：暴露管理员权限预设管理 API

范围：为权限预设提供管理员列表、创建、版本化更新和删除接口；变更接口要求管理员 Web session 与 CSRF，预设版本冲突和名称冲突返回稳定错误。暂不实现 Web 管理表单或预设应用到用户的批量操作。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/api/admin/admin_users.rs`、`src/application/user_presets.rs`、`tests/users.rs`。

验收标准：

- [x] 管理员可以读取两个内置预设及自定义预设；非管理员不能读取或修改。
- [x] 创建、更新、删除均通过业务服务；写操作校验 CSRF，删除和更新要求预期版本。
- [x] 无效输入返回 400，重复名称、内置预设写入和版本冲突返回 409，不存在预设返回 404。
- [x] API 回归、`users` target、Rust build、fmt 和 diff check 通过。

短计划：先在 `users` 集成回归中覆盖列表、CSRF、CRUD 和错误映射并确认失败；再连接 LUX-639 服务，最后同步 API 合同。

验证：`cargo test --locked --test users`、`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-639。

结果（2026-10-19）：新增管理员预设列表与自定义预设创建/更新/删除 API，写操作通过 CSRF 校验并记录审计事件；更新和删除要求当前预设版本。`users` target 2/2、Rust build、fmt check 和 diff check 通过；集成回归覆盖非管理员读写拒绝、重复名称、无效快照、内置项保护、陈旧版本、缺失版本及不存在项。

#### LUX-641：在用户全局设置中管理权限预设

范围：为模块四“角色预设”占位页增加自定义预设列表、新建、编辑和删除表单，复用已完成的管理员预设 API；展示两个只读内置预设和每份快照的管理角色、媒体库、下载、播放与转码信息。不在本任务中将预设应用到已有账号。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/tests/admin-user-default-permissions.test.tsx`。

验收标准：

- [x] 全局设置“角色预设”页读取并区分内置和自定义预设，内置项只读。
- [x] 管理员可创建、编辑和删除自定义预设；保存快照覆盖媒体库、管理角色、下载、播放、并发和转码选项。
- [x] 表单校验非空名称、并发上限和空媒体库选择；服务器错误保留可继续编辑的草稿。
- [x] Web 定向回归、构建和 diff check 通过，桌面/窄屏浏览器验证可操作。

短计划：先扩展现有默认权限设置测试，覆盖预设页加载与保存；再添加 API 客户端方法、页面交互并验证真实浏览器。

验证：`pnpm --dir web test -- admin-user-default-permissions`、`pnpm --dir web build`、`git diff --check`。

依赖：LUX-640。

结果（2026-10-19）：页面接入管理员预设 API，支持自定义权限快照的创建、版本化更新与删除，并展示不可变内置项。定向 Vitest 6/6、冻结依赖安装、Web 构建、diff check 通过。Playwright 真实浏览器在 1280px 桌面和 390px 窄屏验证完整 CRUD，移动视口无页面横向溢出；控制台的 401 来自登录前匿名 `/auth/me` 探测，404 来自临时管理员无头像资源。

完整 Web 门禁未全绿：`pnpm --dir web test` 的 Node 布局断言失败（其中 hero 行高期望与工作区既有 `web/src/react.css` 修改不一致）；单独全量 Vitest 为 91 个文件、677 passed、4 failed。四项失败位于既有首页预取焦点、LibraryCard 预取/虚拟链接和媒体图片轮询测试，与本任务文件无关。该工作区 `web/src/react.css` 的改动属于已有用户修改，未触碰或纳入提交。

#### LUX-642：记录用户已应用的权限预设来源

范围：为用户已应用的权限快照建立来源记录，保存预设 ID、名称、版本和应用时间；来源快照不依赖自定义预设继续存在，删除用户时级联清理。本任务只建立 SQLite/PostgreSQL 持久化结构，不改变用户权限和 API 行为。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0188_user_permission_preset_sources.sql`、`migrations-postgres/0188_user_permission_preset_sources.sql`、`tests/storage.rs`。

验收标准：

- [x] 空库升级后出现来源表，外键以用户为主表并在用户删除时级联清理。
- [x] 来源记录保存预设 ID、名称、正版本号和应用时间；不外键关联可删除的自定义预设。
- [x] SQLite 迁移回归和 PostgreSQL 迁移定义检查、Rust build、fmt 与 diff check 通过。

短计划：先扩展空库迁移回归并确认失败；再添加双数据库迁移及来源记录约束。

验证：`cargo test --locked --test storage -- --test-threads=1`、`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-639、LUX-640。

结果（2026-10-19）：新增双数据库用户预设来源表，保存应用时的 ID、名称、版本和时间，并在账号彻底删除时级联清理。`storage` target 串行运行 64/64、Rust build、fmt check、diff check 通过；PostgreSQL 迁移通过结构定义断言，本机 PostgreSQL 运行时未验证。第一次并行跑完整 `storage` target 时，时间敏感的既有重试迁移断言失败，单线程复跑通过。

#### LUX-643：同步来源迁移后的其余 schema 版本断言

范围：将 `0188` 来源迁移后的当前 schema 版本断言更新为 188；仅修改现行数据库版本断言，历史迁移版本和迁移序列检查保持原值。本任务不改变运行时代码。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`tests/scanning_jobs.rs`、`tests/scanner.rs`、`tests/danmaku.rs`、`tests/postgres_database.rs`。

验收标准：

- [x] SQLite 与 PostgreSQL 测试中的当前 schema 版本断言均与最新迁移一致，历史迁移断言不变。
- [x] 对应迁移回归通过，PostgreSQL target 可编译；Rust build、fmt 与 diff check 通过。

短计划：先确认四个目标中的断言用途；更新当前版本值并运行对应迁移测试。

验证：`cargo test --locked --test scanning_jobs streamed_manifest_bulk_insert_avoids_redundant_availability_trigger_update`、`cargo test --locked --test scanner media_catalog_migration_creates_expected_tables`、`cargo test --locked --test danmaku database_migration_creates_danmaku_tables`、`cargo test --locked --test postgres_database --no-run`、`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-642。

结果（2026-10-19）：将 `scanning_jobs`、`scanner`、`danmaku` 和 PostgreSQL 集成测试中的现行 schema 版本同步为 188，历史迁移编号保持不变。三个 SQLite 定向回归各 1/1、`postgres_database --no-run`、Rust build、fmt check 和 diff check 通过；本机未运行 PostgreSQL 集成服务。

#### LUX-644：原子应用用户权限预设存储快照

范围：在存储层为现有用户原子应用角色、媒体库访问、下载、播放与转码快照，并写入 LUX-642 的来源记录。保护最后一个有效服务器管理员；数据库任一写入失败时回滚全部权限与来源变更。本任务不新增业务 API 或管理页面。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/mod.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 一次事务更新角色、下载、播放/转码、媒体库模式/选择和预设来源。
- [x] `ALL`/`NONE` 清除旧选择，`SELECTED` 完整替换旧选择；失败时旧快照保持不变。
- [x] 不存在或已删除用户不可应用；最后一个有效管理账户不能被降权。
- [x] 存储定向回归、Rust build、fmt 与 diff check 通过。

短计划：先添加存储层成功、降权保护与事务回滚回归并确认失败；再实现单事务预设应用。

验证：`cargo test --locked --lib apply_user_permission_preset`、`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-642、LUX-643。

结果（2026-10-19）：新增存储事务一次性应用角色、下载、播放/转码、媒体库访问与来源快照；`SELECTED` 仅接受启用的普通媒体库，`ALL`/`NONE` 清除旧选择。事务失败回滚全部变更，已删除/不存在用户拒绝操作，单管理员保护并防止并发降权共同移除最后管理员。`apply_user_permission_preset` 定向回归 3/3、Rust build、fmt check、diff check 通过；SQLite 并发回归通过，PostgreSQL 行锁逻辑已编译但未接运行时验证。

#### LUX-645：解析并校验用户权限预设应用服务

范围：在 application 层按预设 ID 读取内置或自定义预设，检查目标用户和当前有效的媒体库快照，再调用 LUX-644 原子存储操作。自定义预设之后可删除，用户已应用的权限与来源快照不受影响。本任务不暴露 HTTP 接口。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/user_presets.rs`。

验收标准：

- [x] 管理员与自定义预设都能按当前快照应用；未知预设、无效用户 ID 和已删除用户返回明确业务结果。
- [x] 媒体库已停用或权限快照失效时拒绝应用；最后管理员保护错误保留为稳定业务错误。
- [x] application 定向回归、Rust build、fmt 与 diff check 通过。

短计划：先增加服务成功、过期媒体库、缺失目标及最后管理员回归；再实现预设解析、快照校验和存储错误映射。

验证：`cargo test --locked --lib user_presets`、`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-639、LUX-640、LUX-644。

结果（2026-10-19）：application 服务可应用内置或自定义预设快照，校验用户 UUID、预设存在性和当前启用媒体库，并映射最后管理员与存储冲突错误。删除已应用的自定义预设不会改变用户权限或来源记录。`user_presets` 定向回归 7/7、Rust build、fmt check 和 diff check 通过；PostgreSQL 行锁仍未运行时验证。

#### LUX-646：开放管理员应用用户权限预设接口

范围：新增管理员将内置或自定义权限预设应用到指定用户的 API。操作要求管理员 Web session 与 CSRF，复用 application 服务和原子存储事务，并记录审计事件；用户/预设不存在、最后管理员保护和无效快照映射为稳定 HTTP 错误。本任务不接入预设来源查询或 Web 界面。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/api/admin/admin_users.rs`、`src/application/user_presets.rs`、`tests/users.rs`。

验收标准：

- [x] 管理员可应用内置或自定义预设，用户权限快照原子更新；自定义预设随后删除不回滚已应用权限。
- [x] 非管理员和缺少 CSRF 的请求拒绝；用户 ID、用户、预设及最后管理员错误映射稳定。
- [x] 应用操作记录目标用户、预设 ID 与版本，不记录敏感内容。
- [x] `users` 集成 target、Rust build、fmt 和 diff check 通过。

短计划：先添加端到端 API 回归并确认新路由失败；再接入 application 服务、错误映射、CSRF 和审计；最后同步公开 API 文档。

验证：`cargo test --locked --test users`、`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-640、LUX-645。

结果（2026-10-19）：新增管理员权限预设应用端点，复用事务存储与 application 校验，成功后写入以目标用户为对象、详情只含预设 ID 和版本的审计事件，并规范化 UUID 大小写。`users` target 3/3、Rust build、fmt check 与 diff check 通过；回归覆盖管理员/CSRF、权限快照、应用后删除预设、无效/不存在用户、未知预设和最后管理员保护。全量 Clippy 仍被既有 `src/api/media.rs:2821` 与 `src/application/playback/session.rs:397` 的 `too_many_arguments` 阻断。

#### LUX-647：批量读取用户权限预设来源

范围：为管理端用户查询提供单个用户的预设来源读取和一页用户 ID 的批量来源读取，返回预设 ID、应用时名称、版本和时间。查询只读来源快照，不联接现存预设配置，因此自定义预设删除后仍能读取。本任务不修改 API 投影或页面。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/mod.rs`、`src/storage/repository.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 单条与批量查询准确返回完整来源，缺少来源时返回空结果。
- [x] 空 ID 列表不执行查询；一页多个用户用单次批量查询完成。
- [x] SQLite 存储回归、Rust build、fmt 和 diff check 通过。

短计划：先添加单条/批量来源查询回归并确认存储接口尚不存在；再新增类型、双查询方法和模块导出。

验证：`cargo test --locked --lib user_permission_preset_source`、`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-642。

结果（2026-10-19）：新增用户预设来源类型、单用户读取及按最多 500 个 ID 分块的批量读取；空输入不执行 SQL。`user_permission_preset_source_reads_support_single_and_batch_queries` 1/1、Rust build、fmt check 和 diff check 通过。为让该中间存储合同保持零新增告警，标注由后续 LUX-648 API 投影接线；全量 Clippy 仍被既有 `src/api/media.rs:2821` 与 `src/application/playback/session.rs:397` 的 `too_many_arguments` 阻断。

#### LUX-648：在管理员用户列表与详情公开预设来源

范围：将 LUX-647 的预设来源快照投影到管理员用户分页列表和单用户详情 API。为现有 `presetSource` 提供应用时名称，并新增含预设 ID、名称、版本和应用时间的 `presetSourceDetails`；无来源时两个字段均为 `null`。分页列表须一次批量读取本页来源。本任务不改 Web 页面。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/auth/users.rs`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [x] 管理员列表和详情返回一致的预设来源字段；没有来源时返回 `null`。
- [x] 删除自定义预设后仍显示已保存的来源快照；列表不会逐用户查询来源。
- [x] `users` 集成 target、Rust build、fmt 和 diff check 通过。

短计划：先扩展 API 集成回归并确认来源字段缺失；再将来源接入 auth DTO 和列表批量投影，最后同步 API 文档。

验证：`cargo test --locked --test users`、`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-647。

结果（2026-10-19）：管理员用户列表和详情现在返回预设名称与完整来源快照，未应用的用户返回 `null`；列表通过一次批量来源查询，用户编辑资料后的详情响应也保留来源。`users` target 3/3、Rust build、fmt check 和 diff check 通过；回归覆盖删除自定义预设后来源仍可读。全量 Clippy 仍被既有 `src/api/media.rs:2821` 与 `src/application/playback/session.rs:397` 的 `too_many_arguments` 阻断。

#### LUX-649：为 Web 接入应用权限预设的 API 合同

范围：为 Web API 类型加入管理员用户预设来源详情，并新增将权限预设应用到单个用户的客户端方法。客户端向 LUX-646 端点发送 `presetId`，携带现有 CSRF token 并处理 204 空响应。本任务不添加页面按钮或批量应用。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] 用户类型包含 nullable 的预设来源详情字段。
- [x] 应用方法正确编码用户 ID、发送请求体和 CSRF，并把 204 转为 `undefined`。
- [x] API 客户端定向测试、Web build 和 diff check 通过。

短计划：先添加客户端契约回归并确认方法缺失；再补类型与 POST 方法，最后运行定向 Vitest 和 Web build。

验证：`pnpm --dir web exec vitest run tests/api-client.test.ts`、`pnpm --dir web build`、`git diff --check`。

依赖：LUX-646、LUX-648。

结果（2026-10-19）：为管理员用户类型增加可空预设来源详情，并新增应用预设 POST 客户端方法。`api-client.test.ts` 64/64、Web build 通过。`pnpm --dir web test -- api-client` 在运行 Node 布局断言阶段先失败（详情页 lazy import、Hero 标题行高、移动端媒体卡片宽度、触控卡片操作），未执行 Vitest；直接 Vitest target 已独立通过。Hero 行高断言也受工作区既有 `web/src/react.css` 修改影响，该文件未纳入任务提交。

#### LUX-650：在用户详情应用权限预设

范围：在用户详情的权限页展示用户当前已应用的预设来源，并允许管理员选择现有内置或自定义预设应用到该用户。应用前说明会替换当前权限快照并请求确认；应用成功后刷新用户详情和列表缓存。本任务不实现批量应用或权限复制。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/tests/admin-users-access.test.tsx`。

验收标准：

- [x] 权限页显示来源快照；自定义预设删除后仍显示已保存的来源名称、版本和时间。
- [x] 可选择现存预设；确认后调用应用 API，成功刷新详情、列表、媒体库范围和播放策略缓存并显示结果。
- [x] 取消确认不发送请求；预设加载或应用失败时给出可读反馈；回收站用户保持只读。
- [x] 页面定向回归、Web build、diff check 和真实浏览器交互验证通过。

短计划：先为预设来源、确认/取消、成功缓存刷新和错误反馈补页面回归并确认失败；再将预设选择与来源展示接入权限页，最后运行定向 Vitest、Web build 和 diff check。

验证：`pnpm --dir web exec vitest run tests/admin-users-access.test.tsx`、`pnpm --dir web build`、`git diff --check`。

依赖：LUX-649。

结果（2026-10-19）：权限页展示应用时保存的预设名称、版本和时间，即使来源自定义预设已经删除；可确认应用内置或自定义预设。成功后刷新用户详情、分页用户列表、媒体库访问和播放策略缓存；取消确认不提交，加载/应用错误可读且允许重试，回收站用户仍只读。`admin-users-access.test.tsx` 10/10、TypeScript 与 Web production build、diff check 通过。Playwright 使用模拟 API 验证 204 应用、详情来源重读及确认交互；1280px 和 390px viewport 均无横向溢出。浏览器验证不代表真实服务端或部署验证；本地模拟 EventSource 曾返回一条 MIME 错误，不影响预设请求链路。

#### LUX-651：建立服务端账户偏好存储结构

范围：新增用户账户偏好表，保存主题、强调色、首页分区开关、默认音轨/字幕语言和自动连播；保存版本号以供后续比较并交换，账号删除时级联清理。本任务只建立 SQLite/PostgreSQL schema 与 SQLite 空库迁移回归，不连接 API 或页面。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0189_user_account_preferences.sql`、`migrations-postgres/0189_user_account_preferences.sql`、`tests/storage.rs`。

验收标准：

- [x] 空库升级后创建带用户级联外键的账户偏好表，偏好列和版本列具有明确类型、默认值与合法值约束。
- [x] 新建偏好行使用与当前账户页一致的默认设置，版本为正整数。
- [x] SQLite 空库迁移回归通过；PostgreSQL schema 版本和迁移运行时由后续 LUX-652 覆盖。

短计划：先添加空库 schema 回归并确认失败；新增双数据库迁移，验证默认值、约束和用户删除级联。

验证：`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test storage user_account_preferences_migration_runs_from_an_empty_database -- --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。Cargo 前确认 Toshiba 已挂载。

结果（2026-10-19）：新增 SQLite/PostgreSQL 账户偏好表迁移，保存主题、强调色、首页区块、音轨/字幕语言、自动连播、版本和更新时间；约束限定可选主题/强调色、布尔值及正版本，用户删除级联清理。SQLite 空库回归先以 schema 版本 188/预期 189 失败，迁移后 1/1 通过，覆盖字段默认值、非法值拒绝和级联删除。PostgreSQL 运行时与当前 schema 断言由 LUX-652 接续。

#### LUX-652：验证账户偏好 PostgreSQL 迁移并同步 schema 版本

范围：将 SQLite/PostgreSQL 当前 schema 版本断言同步至 189，并补充 PostgreSQL 16 空库迁移后的账户偏好默认值、约束和级联删除运行时验证。旧库迁移仍保持其明确的历史版本起点。本任务不修改运行时代码或迁移。

预计修改文件（6 个）：`docs/LUX-DEVELOPMENT.md`、`tests/storage.rs`、`tests/scanner.rs`、`tests/scanning_jobs.rs`、`tests/danmaku.rs`、`tests/postgres_database.rs`。

验收标准：

- [x] 所有当前 schema 断言为 189，明确测试历史迁移的断言仍为其源版本。
- [x] PostgreSQL 空库创建偏好表，运行验证默认值、非法主题/强调色/版本拒绝及用户级联删除。
- [x] 受影响迁移 target、PostgreSQL 迁移回归、Rust 格式检查与 diff check 通过。

短计划：先搜索全部当前版本断言并区分历史升级起点；更新断言与 PostgreSQL bootstrap 运行时回归，再运行受影响 targets。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test storage`、相关 `scanner` / `scanning_jobs` / `danmaku` schema 迁移用例、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test postgres_database postgres_bootstrap_runs_migrations_and_persists_core_state -- --ignored --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test postgres_database postgres_user_library_access_mode_migration_preserves_legacy_semantics -- --ignored --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。

结果（2026-10-19）：所有当前 schema 断言同步到 189；旧库迁移仍从明确历史版本升级。PostgreSQL 16 空库 bootstrap 1/1，覆盖账户偏好默认值、无效主题/强调色/版本拒绝及用户删除级联；LUX-627 PostgreSQL 旧授权升级回归 1/1。SQLite `storage` target 串行运行 66/66，`scanner`、`scanning_jobs`、`danmaku` schema 定向用例各 1/1；PostgreSQL 格式与 diff 检查通过。`storage` 并行首跑中既有 cooldown 窗口测试 65/66，隔离复跑 1/1、串行复跑 66/66；不涉及本次迁移。

#### LUX-653：实现账户偏好版本化存储

范围：提供用户账户偏好的读取与版本比较并交换写入，首次写入要求版本 0 并创建版本 1；后续写入仅在客户端版本匹配时递增版本。偏好字段由存储约束校验，不允许修改已进入回收站的账号。本任务不接入 HTTP 或页面。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 无偏好记录返回空；有效用户首次 CAS 创建版本 1，后续匹配版本写入原子递增。
- [x] 过期版本返回冲突且不覆盖当前值；不存在或已删除用户不能创建或更新偏好。
- [x] 存储回归验证默认读取、CAS 冲突与无效字段拒绝；Rust 格式和 diff 检查通过。

短计划：先添加首次写入、版本递增、陈旧版本、删除用户和非法值存储回归；再新增类型及 SQLite/PostgreSQL 共用 CAS 操作。

验证：`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib account_preferences`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。Cargo 前确认 Toshiba 已挂载。

结果（2026-10-19）：新增账户偏好读取和单语句 CAS 写入。版本 0 创建版本 1，匹配版本更新时递增，陈旧版本返回冲突且保留当前值；数据库约束拒绝无效主题，回收站和不存在账号不可写。`account_preferences_use_compare_and_swap_versions` 1/1、`cargo build --locked`、fmt check 与 diff check 通过。API 接线留给后续任务。

#### LUX-654：实现账户偏好应用服务

范围：为账户设置 API 提供服务端默认偏好读取、完整偏好校验和版本比较并交换写入。无存储记录时返回与当前账户页一致的默认值及版本 0；有效写入委托 LUX-653 CAS 存储合同，明确映射版本冲突和不可用账号。本任务不接 HTTP 或 Web 页面。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/mod.rs`、`src/application/user_account_preferences.rs`（包含服务层回归）。

验收标准：

- [x] 未创建偏好记录的账号读取到当前账户页默认值、版本 0 和空更新时间。
- [x] 有效完整更新通过 CAS 返回递增版本与服务端更新时间；过期版本及不可用账号映射为明确错误。
- [x] 拒绝不支持的主题/强调色、负版本、超长语言值和控制字符。
- [x] 应用服务定向回归、Rust 格式检查与 diff check 通过。

短计划：先添加默认值、有效写入、版本冲突与输入校验回归并确认服务缺失；再实现默认值投影、输入校验和 CAS 错误映射。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib user_account_preferences`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-653。

结果（2026-10-19）：新增账户偏好 application 服务，缺少服务端记录时返回 dark/silver、首页区块开启、原始音轨/简体中文、自动连播、版本 0 和空更新时间；写入校验主题、强调色、非负版本和最长 64 字符且无控制字符的语言值，再调用版本 CAS。新增默认读取、成功写入、过期版本、无效字段及回收站用户回归，`user_account_preferences` 4/4 通过；`cargo build --locked`、fmt check 与 diff check 通过。全目标 Clippy 仍被未修改的 `src/api/media.rs:2821` 和 `src/application/playback/session.rs:397` 两处 `too_many_arguments` 阻断；以 `-A clippy::too_many_arguments` 排除这两项后，lib/all-features Clippy 通过。全目标测试为 1004 passed、5 failed、25 ignored；失败项 `stops_reading_a_chunked_response_as_soon_as_it_exceeds_the_limit`、`local_metadata_worker_dispatches_fill_missing_without_blocking_scan`、`automatic_and_explicit_fill_missing_jobs_keep_dispatch_sources_separate`、`automatic_fill_missing_retry_backoff_excludes_manual_modes_and_cancelled_failures`、`changed_local_fill_request_during_running_item_is_not_lost` 均在先前任务的全目标门禁中记录过，本次与账户偏好无关联。

#### LUX-655：开放当前用户账户偏好 API

范围：新增当前用户账户偏好的 GET/PATCH API，GET 返回服务层默认值或已保存快照，PATCH 接收完整偏好和 `expectedVersion`，要求用户会话与 CSRF，并将非法偏好、版本冲突、不可用账号和数据库故障映射为稳定 HTTP 错误。不修改现有播放阈值与媒体库排序设置 API，也不接 Web 客户端。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/api/users.rs`、`src/api/lux/mod.rs`、`tests/user_settings.rs`。

验收标准：

- [x] `GET /api/v1/auth/account-preferences` 返回当前用户完整偏好、版本和更新时间。
- [x] `PATCH /api/v1/auth/account-preferences` 要求 CSRF，版本 0 可首次写入，匹配版本更新成功；陈旧版本返回 `VERSION_CONFLICT`。
- [x] 非法字段返回 400、账号不可用返回 404、数据库错误返回 503；未认证与缺少 CSRF 请求被拒绝。
- [x] `user_settings` 集成 target、API 文档、Rust build、格式和 diff check 通过。

短计划：先添加默认读取、首次写入、冲突、非法值、CSRF 和认证回归并确认端点缺失；再接入 application 服务和稳定错误映射，最后同步 API 文档。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test user_settings`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-654。

结果（2026-10-19）：新增当前用户账户偏好 GET/PATCH 端点，GET 返回应用服务快照；PATCH 要求当前用户和 CSRF，通过完整字段及 `expectedVersion` 写入并在成功后失效首页缓存。错误映射为 400、404、503 或 409 `VERSION_CONFLICT`，新增稳定 API 错误码。`user_settings` 2/2、Rust build、fmt check 和 diff check 通过；测试覆盖未认证、默认值、缺少 CSRF、首次写入、陈旧版本、无效主题及数据库故障。全目标 Clippy 仍被未修改的两个 `too_many_arguments` 函数阻断；允许该既有 lint 后的 lib/all-features Clippy 通过。公开合同已加入 `docs/API.md`。

#### LUX-656：为 Web 接入账户偏好 API

范围：为 Web API 类型增加账户偏好响应和完整更新请求类型，并提供当前用户账户偏好 GET/PATCH 客户端方法。PATCH 复用客户端现有 CSRF 注入和错误处理；本任务不改账户设置页面或本地存储。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] Web 类型完整表达主题、强调色、首页区块、语言、自动连播、版本与更新时间。
- [x] API 客户端 GET/PATCH 使用正确 endpoint，PATCH 序列化完整版本化请求并发送 CSRF。
- [x] API client 定向 Vitest、Web build 与 diff check 通过。

短计划：先补客户端 endpoint、载荷与 CSRF 回归并确认方法缺失；再添加响应/请求类型和 GET/PATCH 方法，最后运行定向 Vitest、Web build 与 diff check。

验证：`pnpm --dir web exec vitest run tests/api-client.test.ts`、`pnpm --dir web build`、`git diff --check`。

依赖：LUX-655。

结果（2026-10-19）：Web API 增加账户偏好响应与完整更新类型，以及 GET/PATCH 客户端方法；写入复用公共请求层的 CSRF 注入。新增 API 客户端合同回归，`api-client.test.ts` 65/65、TypeScript/Web build 与 diff check 通过。

#### LUX-657：区分已保存的本地账户偏好与默认值

范围：为首次服务端迁移提供只读旧浏览器偏好读取函数。仅当指定用户键下存在可解析设置对象时返回经过现有规则归一化的设置；没有数据、无效 JSON 或浏览器存储不可用时返回 `null`，不读取或导入媒体库顺序以外的服务端状态。本任务不接账户 API 或页面。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/account/account-settings.ts`、`web/tests/account-settings.test.tsx`。

验收标准：

- [x] 无本地记录、JSON 损坏或 localStorage 不可用时返回 `null`，不会把代码默认值当成旧用户设置。
- [x] 有效旧记录保留已知设置并按既有规则补齐缺省/过滤非法字段。
- [x] `account-settings` 定向 Vitest 和 diff check 通过。

短计划：先添加“无记录/有效旧记录/坏数据/受限存储”回归并确认帮助函数缺失；再抽出现有解析与归一化逻辑，不改变普通读取行为。

验证：`pnpm --dir web exec vitest run tests/account-settings.test.tsx`、`git diff --check`。

依赖：LUX-656。

结果（2026-10-19）：新增 `readStoredAccountSettings`，只返回指定用户键下可解析的设置对象；无记录、JSON 损坏、非对象数据或存储读取抛错均返回 `null`。现有 `readAccountSettings` 继续在无有效旧值时返回相同默认值。`account-settings.test.tsx` 25/25、diff check 通过。

#### LUX-658：账户页使用服务端偏好并迁移旧本地值

范围：将主题、强调色、首页区块、默认音轨/字幕语言和自动连播接入当前用户账户偏好 API。服务端已有正版本时以服务端为准；服务端版本 0 且用户确有本地旧设置时只导入一次。后续更改自动版本化保存并更新本地缓存；多次快速编辑顺序提交，遇到跨设备版本冲突时停止自动覆盖并提示用户刷新。媒体库顺序和播放完成比例继续使用原有接口。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/account/AccountPage.tsx`、`web/src/lib/api/query-keys.ts`、`web/tests/account-settings.test.tsx`。

验收标准：

- [x] 账户页读取并显示服务端账户偏好；已保存的本地主题等值不会覆盖服务端正版本。
- [x] 服务端版本 0 时仅在有旧本地记录情况下首次导入，媒体库顺序不进入账户偏好 API。
- [x] 后续设置自动按最新版本串行保存；成功更新本地缓存和首页状态，冲突时停止写入并给出刷新提示。
- [x] 网络错误保留用户当前界面状态；加载失败时不把默认值写入服务端。
- [x] `account-settings` 定向 Vitest、TypeScript、Web build 与 diff check 通过。

短计划：先扩展账户设置组件回归覆盖服务端优先、一次性迁移、序列化保存、错误和冲突，再接入查询、迁移编排与有序 CAS 保存，最后运行定向 Vitest、Web build 和 diff check。

验证：`pnpm --dir web exec vitest run tests/account-settings.test.tsx`、`pnpm --dir web build`、`git diff --check`。

依赖：LUX-655–657。

结果（2026-10-19）：账户页已读取服务端偏好快照；正版本服务端设置覆盖旧浏览器值，版本 0 只迁移一次现存本地偏好且不提交媒体库顺序。更新通过 CAS 队列按返回版本串行保存，成功后更新本地缓存与首页查询；遇到版本冲突禁用偏好控件并提示刷新，普通网络错误保留当前选择且允许再次保存。初始读取失败时控件保持禁用并提供重试。账户设置回归 31/31、TypeScript 与 Web production build、diff check 通过。尚未部署。

#### LUX-659：为管理员会话管理提供分页存储查询

范围：为管理员读取单个用户有效 Web 会话提供服务端分页存储查询，返回有效会话总数及单页摘要，并标记当前 Web 会话。每页上限为 100；摘要只包含会话 ID、创建/更新时间、过期时间及最近活动时间，不包含凭证。HTTP 路由、撤销与页面接线留待后续任务。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/catalog.rs`（含存储回归）。

验收标准：

- [x] 查询只返回指定用户未撤销且未过期的 Web 会话，并提供稳定的时间/ID 排序及总数。
- [x] 分页偏移与大小受存储层约束，超大页请求不能超过 100 条；当前会话标记只比较 token 摘要。
- [x] 存储定向回归、Rust 格式检查和 diff check 通过。

短计划：先补多会话、分页边界、过期/撤销过滤和当前会话标记的存储回归；再新增有界 SQL 页面读取与总数查询。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib admin_web_session_page`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。

结果（2026-10-19）：增加只读有效 Web 会话页查询及总数读取，SQL 按更新时间和 ID 倒序分页，存储层将页大小限制在 1–100 并将负偏移归零；当前会话通过 token 摘要比较标记。回归覆盖 101 条会话的分页/硬上限，以及已撤销和已过期记录过滤，`admin_web_session_page` 1/1、fmt check 与 diff check 通过。管理员 HTTP 路由及撤销留给 LUX-660。

#### LUX-660：开放管理员分页读取用户 Web 会话 API

范围：新增 `GET /api/v1/admin/users/{user_id}/sessions`，返回有效 Web 会话摘要、页码、页大小和总数。要求管理员权限，页大小默认 50 且最大 100；当前会话只通过比较现有 token 摘要标记。接口不返回 Cookie、session token 或 CSRF token，不提供撤销操作。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/auth/sessions.rs`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [x] 管理员可读取指定用户的分页活动 Web 会话；未认证和非管理员不能访问。
- [x] 响应只公开会话 ID、时间字段与当前会话标记，不包含任何登录凭证；非法用户 ID 返回 400。
- [x] `users` 集成 target、API 文档、Rust 格式和 diff check 通过。

短计划：先补管理员权限、分页、当前会话标记和敏感字段排除的 API 回归并确认端点未开放；再复用 LUX-659 存储页接入服务与路由并记录公开合同。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test users admin_can_list_user_web_sessions_without_exposing_tokens -- --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。

结果（2026-10-19）：管理员分页会话接口接入 LUX-659 查询，返回摘要、当前页及总数；未认证/普通用户访问拒绝，非法用户 ID 返回 400，页大小上限 100，当前会话仅比较 token 摘要且响应不含 Cookie 或凭证。`admin_can_list_user_web_sessions_without_exposing_tokens` 1/1、fmt check 与 diff check 通过，API 合同已写入 `docs/API.md`。撤销和管理页面留待后续任务。

#### LUX-661：提供当前 Web 会话身份的精确存储检查

范围：增加按用户 ID、会话 ID 和 token 摘要精确检查有效 Web 会话身份的存储操作，供管理员撤销 API 防止撤销管理员自己的当前会话。只接受未撤销且未过期记录，不读取或返回原始凭证；本任务不新增 HTTP 行为。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/catalog.rs`（含存储回归）。

验收标准：

- [x] 只有三项身份均匹配且会话仍有效时返回 true；错用户、错会话、错摘要、撤销或过期记录均返回 false。
- [x] 存储定向回归、Rust 格式检查和 diff check 通过。

短计划：先添加匹配/错配/失效会话测试并确认检查方法缺失，再实现单条 EXISTS 查询。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib admin_current_web_session_guard`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。

结果（2026-10-19）：新增单条 EXISTS 存储检查，只有用户、会话 ID、token 摘要完全匹配且会话未撤销/未过期时返回 true。回归覆盖错用户、错会话、错摘要、过期和撤销，`admin_current_web_session_guard` 1/1、fmt check 与 diff check 通过。管理员撤销 API 将在 LUX-662 使用此检查。

#### LUX-662：开放管理员撤销用户 Web 会话 API

范围：新增 `DELETE /api/v1/admin/users/{user_id}/sessions/{session_id}`，撤销指定用户的一条活动 Web 会话。要求管理员权限与 CSRF；拒绝撤销当前管理员 Web 会话。目标会话必须属于路径用户且未过期/未撤销；成功返回 204，未知、过期、已撤销或用户不匹配返回 404。撤销不会影响该用户的其他 Web 会话。

预计修改文件（6 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/auth/sessions.rs`、`src/storage/catalog.rs`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [x] 缺少认证或 CSRF 请求被拒绝；错误用户/会话、过期和已撤销会话返回 404。
- [x] 当前管理员会话返回 400 不撤销；成功撤销返回 204，目标登录失效且其他目标会话仍可用。
- [x] `users` 集成 target、API 文档、Rust 格式和 diff check 通过。

短计划：先补 CSRF、当前会话保护、目标归属、过期会话、成功撤销及其他登录保留的集成回归；再连接 LUX-661 精确身份检查与仅撤销仍有效会话的原子存储操作，最后记录合同与审计事件。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test users admin_can_revoke_user_web_sessions -- --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。

结果（2026-10-19）：新增管理员单会话撤销 DELETE API，管理员 Web 登录需通过 CSRF；当前会话使用 user/session/token 摘要精确匹配保护。存储更新原子要求目标未撤销且未过期；错误归属、过期、重复撤销返回 404，撤销后只使目标 Cookie 失效并记录安全审计事件。撤销回归 1/1，串行 `users` target 6/6、Rust fmt check 与 diff check 通过。默认并发 `users` target 曾有一个既有用户管理测试收到 503；该测试单独及串行 target 重跑通过，暂未查明并发失败原因。

#### LUX-663：为管理员 Web 会话管理补齐 Web API 客户端合同

范围：为 LUX-660/LUX-662 管理员 Web 会话读取与撤销 API 增加有类型的 Web 客户端方法。列表支持页码和页大小参数；撤销使用 DELETE 和现有自动 CSRF 头处理。此任务不修改页面。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] 会话列表类型只含会话 ID、时间和当前会话标记；没有 token/cookie/凭证字段。
- [x] 客户端正确编码用户/会话 ID、构建分页查询，并为撤销请求附带 CSRF 头。
- [x] API 客户端定向 Vitest、Web 类型构建与 diff check 通过。

短计划：先添加列表/撤销 URL、参数与 CSRF 头的失败客户端回归；再新增安全摘要类型和两个 API 方法，运行定向 API 客户端测试与 Web build。

验证：`pnpm --dir web exec vitest run tests/api-client.test.ts -t "uses paginated admin user Web session"`、`pnpm --dir web build`、`git diff --check`。

依赖：LUX-660、LUX-662。

结果（2026-10-19）：新增 Web 会话分页摘要类型，以及管理员读取和撤销客户端方法；DELETE 复用统一请求层自动附加 CSRF。定向 API 客户端测试 1/1 通过，Web build 在临时恢复当前提交版 `AdminDashboardNowPlaying.tsx` 后通过，并确认还原了该文件原有未提交内容。直接在当前工作区运行 build 会被该用户改动中两个未使用 helper 阻断；`git diff --check` 通过。

#### LUX-664：在用户详情管理 Web 登录会话

范围：将用户详情的“设备与登录”页接入管理员 Web 会话 API，显示分页有效会话摘要与当前会话状态，并允许管理员撤销其他单个 Web 登录。当前会话没有撤销操作；撤销前确认，撤销后刷新列表并保留其他登录。该任务不展示或管理 Emby 凭证、设备黑名单和播放设备清单。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/src/features/admin/user-management.css`、`web/tests/admin-user-sessions.test.tsx`。

验收标准：

- [x] 详情页列出 Web 会话创建、最近活动、到期时间、当前会话状态及有效会话数量，不展示任何凭证。
- [x] 管理员可分页读取和确认撤销其他会话；当前会话不可撤销，成功后列表更新且其他会话仍保留。
- [x] 读取错误可重试，空结果清晰可见；组件回归通过。

短计划：先添加设备与登录页加载、会话摘要、分页、确认撤销、当前会话保护、错误和空状态回归；再连接 LUX-663 客户端方法与 React Query，最后检查组件回归和 Web build。

验证：`pnpm --dir web exec vitest run tests/admin-user-sessions.test.tsx`、`pnpm --dir web build`、`git diff --check`。

依赖：LUX-593、LUX-660、LUX-662、LUX-663。

结果（2026-10-19）：会话组件测试 3/3 通过；Web build 在临时恢复用户当前提交版 `AdminDashboardNowPlaying.tsx` 后通过，并逐字节还原了该文件的未提交用户改动。直接在当前工作区运行 build 仍被这份既有改动中两个未使用 helper 阻断。macOS 浏览器本地验证显示有效会话摘要、时间、状态、撤销操作和分页，浏览器控制台无警告或错误；`git diff --check` 通过。

#### LUX-665：批量应用用户权限预设 API

范围：新增管理员对明确用户 ID 集合批量应用权限预设的 API。每项结果独立返回成功状态和稳定失败码；各用户仍单独原子应用，单项失败不回滚其他用户。单批最多 100 个不同用户 ID。管理员 Web 会话、CSRF、最后一个有效服务器管理员保护及每个成功目标的审计事件均沿用现有单用户合同。本任务不增加列表多选或“全部筛选结果”界面。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/application/user_presets.rs`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [x] 仅管理员可通过 CSRF 保护端点批量应用预设；空集合、重复 ID 和超过 100 项请求整体拒绝。
- [x] 每个 ID 按输入顺序返回成功或稳定失败码；无效/不存在目标、最后管理员保护和单项存储错误不影响其他目标。
- [x] 每个成功目标写入原有预设来源与独立审计事件；请求和响应不包含登录凭证。
- [x] users 集成定向回归、API 文档、Rust 格式和 diff check 通过。

短计划：先添加认证/CSRF、输入边界、多目标部分成功和逐项失败回归；再复用预设快照校验并逐用户执行现有原子存储操作，最后记录 API 合同与审计行为。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test users admin_can_batch_apply_permission_presets_with_per_user_results -- --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-639、LUX-640、LUX-644–646、LUX-650。

结果（2026-10-19）：批量 API 对 1–100 个不同目标逐项执行同一份已校验预设快照，每项单独提交并按输入顺序返回稳定结果码；最后管理员、无效/不存在 ID 和注入的单项存储失败不影响其余目标，成功项各自写入脱敏审计事件。`admin_can_batch_apply_permission_presets_with_per_user_results` 1/1、Rust fmt check、定向 Clippy（仅放行仓库已知的 `too_many_arguments` lint）和 diff check 通过。未豁免的定向 Clippy 仍被未修改的 `src/api/media.rs:2821` 与 `src/application/playback/session.rs:397` 两处同一 lint 阻断。API 文档同步完成。

#### LUX-666：Web 批量应用权限预设客户端合同

范围：为 LUX-665 管理员批量应用预设 API 增加有类型的 Web 客户端方法，发送明确用户 ID 列表和预设 ID，并返回预设摘要与逐用户成功/失败码。POST 复用统一请求层的 CSRF 处理；不修改列表 UI。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] 类型覆盖批量响应和所有稳定失败码，不含任何会话凭证字段。
- [x] 客户端使用固定批量端点、正确 JSON body，并自动附加 CSRF 头。
- [x] 定向 API client Vitest、Web build 和 diff check 通过。

短计划：先添加批量 POST URL、body、逐项结果解码与 CSRF 的失败客户端回归；再新增类型及方法，最后运行定向测试和 Web build。

验证：`pnpm --dir web exec vitest run tests/api-client.test.ts -t "batch-applies admin permission preset"`、`pnpm --dir web build`、`git diff --check`。

依赖：LUX-665。

结果（2026-10-19）：新增批量响应与五种逐项错误码类型及 Web client POST 方法，统一请求层自动附加 CSRF。定向 API client 测试 1/1、Web build 与 diff check 通过；build 前临时使用提交版 `AdminDashboardNowPlaying.tsx`，构建后逐字节恢复用户当前未提交内容。当前工作区直接 build 仍被该文件的两个既有 unused helper 阻断。

#### LUX-667：用户列表批量应用权限预设界面

范围：在用户列表提供当前页用户的明确多选、全选当前页、权限预设选择、目标与变更确认及逐用户结果。换页或筛选变化时清空勾选；每次仅提交当前页选中的 1–100 个用户，不提供会把分页误当全量匹配结果的操作。本任务只完成“应用权限预设”批量动作。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUsersPage.tsx`、`web/src/features/admin/user-management.css`、`web/tests/admin-user-batch-actions.test.tsx`。

验收标准：

- [x] 选择范围和用户数量始终明确；当前页、筛选或视图变化会清空选择。
- [x] 应用前展示选中目标和预设并要求确认；取消不发请求。
- [x] 结果逐用户翻译稳定错误码并展示；网络错误可见，成功后刷新用户列表。
- [x] 定向组件回归、Web build 与 diff check 通过。

短计划：先为选择范围、取消确认、逐项结果和换页清选添加失败测试；再接入既有批量 API 和预设查询，最后验证组件及 Web 构建。

验证：`pnpm --dir web exec vitest run tests/admin-user-batch-actions.test.tsx`、`pnpm --dir web build`、`git diff --check`。

依赖：LUX-665、LUX-666。

结果（2026-10-19）：用户列表支持逐项选择或全选当前页、确认预设和完整目标、逐用户成功/错误结果；换页、筛选或切换视图清空选择，成功后刷新列表。批量功能按需加载预设，仅提交当前页明确选中的最多 100 个 ID。LUX-667 组件测试 4/4、用户管理组件回归 41/41、`pnpm --dir web install --frozen-lockfile` 和 Web build 通过。完整 `pnpm --dir web test` 为 699 passed、5 failed；失败在账号头像裁剪、首页媒体库预取、媒体库卡片预取/虚拟合集链接及媒体详情轮询四个未由本任务修改的测试文件，未据此声称为基线失败。`git diff --check` 通过。build 临时使用提交版 `AdminDashboardNowPlaying.tsx`，构建后逐字节还原用户未提交版本；由于本地服务已停止，没有使用真实账户数据做手工浏览器验证。

#### LUX-668：建立版本安全的账户偏好批量复制服务

范围：新增 application 服务，从一个有效来源账号向最多 100 个不同目标账号复制所选的外观、首页和播放偏好组。调用者提供来源与每个目标的预期版本；来源版本过期时整体拒绝，目标版本冲突或账号不可用时逐项报告，不覆盖未选择的偏好组。媒体库排序与播放完成比例因使用独立存储，本任务不复制它们。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/user_account_preferences.rs`。

验收标准：

- [x] 拒绝空组、空目标、重复目标、来源账号自我复制、超出 100 项及负版本输入。
- [x] 来源账号不存在、已删除或版本不匹配时不修改任何目标。
- [x] 成功目标只改所选偏好组并执行目标版本 CAS；冲突、删除、存储失败只影响对应目标。
- [x] application 定向测试、Rust fmt 和 diff check 通过。

短计划：先为分组选拷贝、目标版本冲突、来源过期和按目标部分失败添加失败测试；再复用现有 CAS 写入完成独立目标复制。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib user_account_preferences::tests`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-653、LUX-654。

结果（2026-10-19）：新增外观、首页和播放三类账户偏好组复制服务。来源账号版本过期或来源不可用时不开始写入；每个有效目标按目标版本 CAS 独立更新，缺失/删除、版本冲突和注入的单目标写入错误作为逐项结果返回。7 个 `user_account_preferences::tests` 通过，`cargo build --locked`、fmt、`git diff --check` 和放行仓库已知 `too_many_arguments` lint 的定向 Clippy 通过。服务尚未接 HTTP，临时 `dead_code` 许可用于 LUX-669 接线前的中间层。

#### LUX-669：开放管理员账户偏好复制预览与应用 API

范围：增加管理员偏好复制预览和应用端点。预览校验来源、目标和所选偏好组，返回所选组的来源值与来源/目标版本；应用提交预览版本并逐目标执行 LUX-668 CAS。应用需要管理员 Web 会话及 CSRF；预览只读但仍限管理员。成功目标逐项写审计，不记录偏好值、凭证或 Cookie。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/application/user_account_preferences.rs`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [x] 预览只返回所选偏好组；来源/目标不可用、空组、空/重复/超限目标及自我复制被拒绝。
- [x] 应用要求管理员和 CSRF；来源过期整体冲突，目标过期逐项返回 `VERSION_CONFLICT`，成功目标不受其他目标失败影响。
- [x] 响应不含未选择的偏好、凭证或敏感存储值；审计仅记来源、目标和组名。
- [x] users 集成回归、application 定向回归、Rust build/fmt 与 diff check 通过。

短计划：先补预览快照和版本合同的 application/API 失败测试；再实现管理员预览与 CAS 应用端点，更新 API 文档并跑定向集成验证。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib user_account_preferences::tests`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test users admin_can_preview_and_copy_account_preferences`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check`。

依赖：LUX-655、LUX-668。

结果（2026-10-19）：增加管理员偏好复制预览与应用 API。预览仅返回选择的外观/首页/播放快照、来源版本和目标版本；应用检查来源版本并为每个目标独立 CAS，逐项报告版本冲突，同时对成功目标写入仅包含来源 ID 和组标志的审计。覆盖非管理员拒绝、预览不需 CSRF、应用需 CSRF、空组拒绝、重复/无效 ID 拒绝、未选组不出现在响应、目标并发冲突、来源过期和审计脱敏。`user_account_preferences::tests` 8/8、`cargo test --locked --test users` 8/8 和 `cargo build --locked` 通过；`cargo clippy --locked --lib -- -D warnings -A clippy::too_many_arguments`、`cargo fmt --all -- --check` 与 `git diff --check` 通过。

#### LUX-670：为偏好复制预览与应用接入 Web API 客户端

范围：为 LUX-669 的管理员偏好复制预览与应用 API 增加有类型的客户端输入/输出和方法。客户端保留现有请求层的 CSRF 注入及错误处理；本任务不修改用户界面，也不扩展尚未版本化的媒体库顺序和播放完成比例。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] 输入和响应类型与 LUX-669 的 JSON camelCase 合同一致，只允许 Appearance/Home/Playback 三组。
- [x] 预览方法使用预览路径并返回源快照和目标版本；应用方法使用 CSRF 保护路径并返回逐目标结果。
- [x] API client 定向测试覆盖请求路径、请求体、类型化响应及 CSRF，Web production build 通过。

短计划：先写两个客户端方法的失败测试；再添加窄类型和方法实现，运行 API client 定向 Vitest 与 Web build。

验证：`pnpm --dir web exec vitest run tests/api-client.test.ts`、`pnpm --dir web build`、`git diff --check`。

依赖：LUX-669。

结果（2026-10-19）：新增偏好复制预览和应用的 Web 输入/响应类型及客户端方法，复用自动 CSRF 和统一错误处理。两项新测试验证精确路径、JSON 请求体、类型化响应及 CSRF；`pnpm --dir web exec vitest run tests/api-client.test.ts` 69/69 通过。Web build 在临时替换为提交版 `AdminDashboardNowPlaying.tsx` 后通过，退出前逐字节恢复工作区文件；该用户未提交文件本身含未使用 helper，仍未纳入本任务。

#### LUX-671：在用户列表预览并复制账户偏好

范围：用户列表的当前页显式选择增加偏好复制入口。管理员搜索一个未被选为目标的来源账号，选择 Appearance/Home/Playback 组，预览所选组快照和每个目标版本，再按预览版本应用。复制结果逐账号显示；成功后清空父列表选择。媒体库排序和播放完成比例由后续任务加入服务端版本合同后再扩展。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUsersPage.tsx`、`web/src/features/admin/AdminUserPreferenceCopyDialog.tsx`、`web/src/features/admin/user-management.css`、`web/tests/admin-user-preference-copy.test.tsx`。

验收标准：

- [x] 只复制当前页明确选中的 1–100 个目标；来源不得同时是目标，筛选、换页和视图变化仍清空选择。
- [x] 来源搜索、偏好组选择、预览值/目标版本与应用均可用；未预览或预览条件变更时不能应用。
- [x] 加载、无结果、错误、逐目标成功/失败及关闭状态清楚且可键盘操作；成功后清空列表选择并刷新数据。
- [x] 组件交互定向测试和 Web production build 通过。

短计划：先写目标选择、来源和预览、版本化应用、逐项目结果的失败交互测试；再实现独立对话框与用户列表入口，运行定向 Vitest、build 和 diff check。

验证：`pnpm --dir web exec vitest run tests/admin-user-preference-copy.test.tsx`、`pnpm --dir web build`、`git diff --check`。

依赖：LUX-667、LUX-670。

结果（2026-10-19）：新增用户列表偏好复制对话框，按当前页选中用户固定目标快照；来源查询排除目标账号，预览显示选中偏好和来源/目标版本，应用使用预览的版本条件并逐账号显示结果。新增交互测试覆盖选择、预览、版本冲突、清空选择、条件变更和失败后重预览。`pnpm --dir web exec vitest run tests/admin-user-preference-copy.test.tsx tests/admin-user-batch-actions.test.tsx tests/admin-users-delete.test.tsx` 12/12 通过；`pnpm --dir web build` 通过。构建期间临时使用已提交版 `AdminDashboardNowPlaying.tsx` 并在退出时逐字节恢复用户未提交内容；`git diff --check` 通过。模块四的媒体库顺序和播放完成比例仍由后续服务端版本合同任务补齐。

#### LUX-672：扩展版本化账户偏好存储合同

范围：把个人媒体库顺序和自动标记已看阈值纳入账户偏好快照。已有 `user_library_order` 与 `user_playback_settings` 仍为读取兼容数据源；CAS 写入可选择同步其中任一值，并与偏好版本更新处于同一事务。未提供的字段保持旧值，以便后续 API 分组更新只修改明确选择的配置。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`、`src/application/user_account_preferences.rs`。

验收标准：

- [x] 服务端偏好快照包含媒体库顺序和已看阈值；尚无版本化偏好行的账号仍能读到旧服务端值与版本 0。
- [x] 匹配版本的 CAS 可同步写入个人排序和已看阈值；省略某字段时不改动其旧值。
- [x] 过期版本、删除账号或任何同步存储失败均不留下部分写入。
- [x] 存储与服务层定向测试、构建、格式检查通过。

短计划：先增加覆盖旧值加载、同一事务写入、可选字段保留和冲突隔离的测试，再扩展偏好快照/CAS 的存储与 application 合同；不新增迁移，复用已存在的排序和播放设置表。

验证：确认 Toshiba 已挂载后，运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib user_account_preferences::tests`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib account_preferences_use_compare_and_swap_versions`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked` 和 `git diff --check`。

依赖：LUX-662、LUX-667、LUX-669。

结果（2026-10-19）：账户偏好服务快照现在包含旧服务端表中的媒体库顺序与 1–100 播放阈值；版本 0 读取旧值但不写入偏好行。CAS 使用单个数据库事务更新偏好版本和被显式提供的旧表值，字段省略时保留原设置；无效媒体库 ID 触发整笔回滚。`account_preferences_use_compare_and_swap_versions` 1/1、`user_account_preferences::tests` 9/9、`cargo build --locked`、`cargo fmt --all -- --check` 与 `git diff --check` 通过。此切片仅建立一致的服务端 CAS 写入合同，旧设置路由将在 LUX-673 接入合同。

#### LUX-673：账户设置 API 共用偏好版本

范围：账户偏好 API 读写媒体库顺序与播放阈值；原 `/settings` 和 `/library-order` 入口保留兼容，但对这两类数据也使用账户偏好 CAS 并返回版本。用户个人排序在验证可访问范围后规范化，API 响应不泄漏不可访问媒体库 ID。管理强制排序开关仍独立于可复制的个人排序。

预计修改文件（13 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/application/user_account_preferences.rs`、`src/application/libraries.rs`、`src/api/users.rs`、`src/storage/users.rs`、`tests/user_settings.rs`、`tests/library_order.rs`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/src/features/account/AccountPage.tsx`、`web/tests/account-settings.test.tsx`、`web/tests/api-client.test.ts`。

验收标准：

- [x] 账户偏好 GET/PATCH 含已看阈值和媒体库顺序；全量偏好保存与旧阈值/排序路由都会推进同一版本。
- [x] 旧路由省略版本时兼容地 CAS 最新读到的版本；带陈旧版本时返回 VERSION_CONFLICT，不能覆盖另一入口的更新。
- [x] 排序只接受目标账号可访问的媒体库，并继续尊重管理员强制排序；不泄漏被禁止访问的 ID。
- [x] Account 页面读取服务器全量快照，保存各偏好时携带当前版本；浏览器旧值仅在服务端版本为 0 时迁移。
- [x] API/Web 定向测试、Web build、Rust build、fmt 通过。

短计划：先扩展账户偏好与旧入口的 API 回归测试和 Web 客户端类型测试；再为排序验证增加只规范化不写入的服务方法，将两条兼容路由改为 CAS 更新，并让账户页提交/迁移完整偏好快照。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test user_settings`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test library_order`、Web 账户设置/API client 定向 Vitest、Web build、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、fmt check 和 `git diff --check`。

依赖：LUX-654、LUX-672。

结果（2026-10-19）：`user_settings` 2/2、`library_order` 1/1、`account_preferences_use_compare_and_swap_versions` 1/1、`user_account_preferences::tests` 9/9、Web 账户设置/API client 定向测试 101/101 通过；Rust build、fmt、Web build 与 `git diff --check` 通过。Web build 临时用 HEAD 版本覆盖用户未提交的 `AdminDashboardNowPlaying.tsx`，完成后已校验恢复原文件。Cargo 网络测试使用 `NO_PROXY=127.0.0.1,localhost` 避免本机请求经过代理。API 合同已同步到 `docs/API.md`。

#### LUX-674：完成账户偏好分组复制

范围：在管理员账户偏好复制中增加媒体库排序组；播放组同时复制已看阈值。预览按每个目标当前可访问媒体库过滤来源排序，应用时重新按目标当前权限过滤；复制只更新选中的组，并继续使用账户偏好版本 CAS，不改变管理员强制排序设置。

预计修改文件（10 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/application/user_account_preferences.rs`、`src/api/admin/admin_users.rs`、`tests/users.rs`、`web/src/lib/api/types.ts`、`web/src/features/admin/AdminUserPreferenceCopyDialog.tsx`、`web/src/features/admin/user-management.css`、`web/tests/admin-user-preference-copy.test.tsx`、`web/tests/api-client.test.ts`。

验收标准：

- [x] 预览/应用合同支持独立 `libraryOrder` 组；播放组选中时包含 `playedPercent`。
- [x] 预览为每个目标返回按其当前可访问范围过滤后的排序，应用时再次过滤；其他未选组保持原值。
- [x] 管理强制排序偏好不被复制或覆盖；逐目标 CAS 冲突和脱敏审计行为保持不变。
- [x] 管理员对话框可选择排序组并查看来源排序、各目标的过滤结果及已看阈值；过滤规则清楚可见。
- [x] application/API/Web 定向测试、Rust build/fmt、Web build 通过。

短计划：先扩展 application 复制与预览回归测试、管理员 API 响应合同和 Web 类型/组件测试；再扩展分组快照及逐目标权限过滤，并更新审计组标志和 API 文档。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib user_account_preferences::tests`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test users admin_can_preview_and_copy_account_preferences`、Web 偏好复制/API client 定向 Vitest、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、fmt、Web build 与 `git diff --check`。

依赖：LUX-671、LUX-672、LUX-673。

结果（2026-10-19）：管理员复制 API 增加 `libraryOrder` 选择组；预览返回来源个人顺序及逐目标过滤结果，应用时重新依据目标当前可访问媒体库过滤并以 CAS 保存。播放组现在包含 `playedPercent`。复制不修改目标的管理员强制排序设置，审计仅增加组标志、不写偏好值。`user_account_preferences::tests` 9/9、`users` 定向集成 1/1、Web 偏好复制/API client 定向测试 71/71、`cargo build --locked`、fmt、Web build 与 `git diff --check` 通过。Web build 临时以 HEAD 版覆盖用户未提交的 `AdminDashboardNowPlaying.tsx`，结束后已逐字节恢复；由于本会话未配置 Chrome DevTools MCP，没有真实浏览器截图验证。

#### LUX-675：建立用户内容限制策略表

范围：为模块五增加按用户版本化的内容限制策略表，以及允许/禁止标签子表。策略字段保存分级体系、最高分级和未分级处理；不迁移现有 numeric `rating`，不在请求内扫描 NFO，也不在此任务启用拦截。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0190_user_content_restrictions.sql`、`migrations-postgres/0190_user_content_restrictions.sql`、`tests/storage.rs`。

验收标准：

- [x] SQLite 与 PostgreSQL migration 均创建受用户生命周期级联清理的策略表和标签表。
- [x] 分级/未分级字段与标签类别有数据库约束；标签区分允许与禁止并按用户去重。
- [x] SQLite 空库迁移回归覆盖非法分级/布尔值、非法/重复标签和用户删除级联；存储测试的 schema 版本断言同步到 190。
- [x] Rust 存储定向测试、Rust build、fmt 和 diff check 通过。

短计划：先添加 SQLite 空库迁移回归并确认失败；再写 SQLite/PostgreSQL migration 及约束；最后运行存储定向检查和构建。其他集成目标的当前 schema 版本断言与 PostgreSQL 启动合同由 LUX-676 对齐。本切片只建立可持久化结构，存储 CAS、规则校验、索引与媒体访问执行留给后续任务。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test storage user_content_restrictions_migration_runs_from_an_empty_database`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、fmt check 与 `git diff --check`。PostgreSQL migration 的运行验证由 LUX-676 的 ignored PostgreSQL bootstrap 测试负责。

依赖：LUX-593、LUX-628。

结果（2026-10-19）：新增双数据库策略与标签表迁移，已有账号不回填策略行，因此尚未设置限制的用户保持既有访问行为；策略行随用户删除、标签行随策略删除。SQLite 空库迁移回归通过，检查非法空分级、越界未分级标志、非法类别、空/重复标签及级联删除。`cargo test --test storage -- --test-threads=1` 67/67、定向迁移测试、`cargo build --locked`、fmt 与 diff check 通过。并行执行完整存储目标时，既有重试冷却断言因执行耗时落在 299–300 秒窗口外失败；隔离复跑及串行完整目标通过。PostgreSQL migration 执行测试和其余目标的 schema 版本更新见 LUX-676。

#### LUX-676：同步内容限制 migration 的数据库测试基线

范围：将依赖当前数据库 schema 版本的 PostgreSQL、扫描器、扫描任务与弹幕测试更新到 LUX-675 的版本 190；PostgreSQL bootstrap 回归验证内容限制表、策略字段和标签表能从空库创建。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`tests/postgres_database.rs`、`tests/scanner.rs`、`tests/scanning_jobs.rs`、`tests/danmaku.rs`。

验收标准：

- [x] 相关目标的 schema 版本预期为 190，原功能断言不变。
- [x] PostgreSQL bootstrap 验证策略/标签表、字段约束、外键级联与非法值拒绝。
- [x] scanner、danmaku 与扫描任务的其他 90 项通过；单独记录并排除已在基线复现的扫描断言失败，Rust build、fmt 与 diff check 通过。

短计划：先更新相关测试目标的版本断言；再扩展 ignored PostgreSQL bootstrap 检查迁移后的结构和约束；运行 SQLite 定向目标并在可用时执行 PostgreSQL 测试。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test scanner`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test scanning_jobs -- --skip manifest_discovery_reopens_and_retries_persisted_frontier_idempotently`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test danmaku`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test postgres_database postgres_bootstrap_runs_migrations_and_persists_core_state -- --ignored --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、fmt check 与 `git diff --check`。

依赖：LUX-675。

结果（2026-10-19）：PostgreSQL 空库启动通过，策略字段与标签表可创建，非法布尔值/标签类别/空标签/重复标签均被约束拒绝，删除策略及用户可级联清理关联行。`scanner` 18/18、`danmaku` 7/7、扫描任务排除基线失败后的 90/90、PostgreSQL bootstrap 1/1、Rust build、fmt 与 diff check 通过。完整 `scanning_jobs` 目标中 `manifest_discovery_reopens_and_retries_persisted_frontier_idempotently` 在第 7311 行因检查点实数为 1、预期 2 而失败；该失败在未包含 LUX-675 的干净提交 `a3173c1d` 上以同名定向测试原样复现，未纳入本任务改动。

#### LUX-677：为用户内容限制提供版本化存储读写

范围：为用户内容限制策略提供读取与乐观锁写入。未配置的用户仍返回 `None`；写入策略和整组替换允许/禁止标签必须在同一数据库事务中完成。只校验存储边界和输入数量，不解释分级映射或执行媒体授权。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 在单条 SQL 快照中读回分级体系、最高等级、未分级规则和稳定排序的允许/禁止标签；缺少策略行返回 `None`。
- [x] 版本 0 创建、匹配版本更新并原子替换标签；陈旧版本、已删除/不存在账号、非法字段和重复规范化标签不产生部分写入。
- [x] SQLite 存储回归、构建、fmt 与 diff check 通过。

短计划：先加入缺省读取、CAS 版本冲突、规范化标签替换和事务失败回滚测试并确认失败；再定义存储合同和 SQLite/PostgreSQL 通用事务实现；最后运行定向测试与构建。本任务不接入 API/UI，也不改变访问判定。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib user_content_restrictions_storage`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、fmt check 与 `git diff --check`。

依赖：LUX-675、LUX-676。

结果（2026-10-19）：存储层增加用户策略读取和 CAS 写入，策略与标签通过一条 JOIN 读取，避免并发更新时读到不同版本；允许/禁止标签先 trim、统一小写并以 Rust 顺序排序，每类最多 100 个，空值、超长值和规范化后重复项拒绝。策略版本与标签整组替换位于同一事务，注入标签写失败后版本及原标签保持不变。缺少策略行返回 `None`。存储定向用例 1/1、build、fmt 与 diff check 通过；严格 Clippy 仍被未修改的 `serve_emby_media_file` 与 `verify_emby_direct_stream` 两个既有 `too_many_arguments` 阻挡，按现有基线豁免该 lint 后 Clippy 通过。

#### LUX-678：定义内容限制使用的年龄分级分类

范围：为模块五定义明确的可选年龄分级体系、规范化证书代码及比较顺序。仅解析 NFO 的认证文本，不把 TMDb/community 数值评分当作年龄等级；尚不创建条目索引、不改变媒体访问。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`docs/USER-MANAGEMENT-DESIGN.md`、`src/domain/content_restrictions.rs`、`src/domain/mod.rs`。

验收标准：

- [x] 明确支持 MPA 电影分级、TV Parental Guidelines 和 BBFC，策略接口使用稳定体系键与等级键。
- [x] 仅规范化已知分级文本；未知、缺失或不适用于所选体系的值保持未分级。
- [x] 分级比较顺序在代码和用户方案中一致；纯函数回归覆盖大小写/空白、已知等级、未知等级、体系不匹配和最大等级判定。
- [x] 认证字段与社区数值评分严格区分。

短计划：先加入域层纯函数回归，确认未实现时编译失败；再实现体系键、分级键、规范化和比较；最后同步方案与主规格，并运行定向 library 测试、fmt 与 diff check。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib content_restrictions`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 与 `git diff --check`。

依赖：LUX-677。

结果（2026-10-19）：域层定义稳定体系键 `MPA`、`TVPG`、`BBFC`，只识别各自正式等级代码并兼容 `Rated ` 前缀；未知文本、地域前缀和数值评分均保持未分级。应用内最高等级顺序与方案一致，其中 BBFC `12`/`12A` 同级；TV 指引官方列出六类而未定义数值序列，方案明确登记 Lux 自身比较顺序。纯分类回归 3/3 通过；过滤既有 `too_many_arguments` 基线后 Clippy、fmt 和 diff 检查通过。官方定义核对：[MPA](https://www.filmratings.com/ratings-guide/)、[TV Parental Guidelines](https://www.tvguidelines.org/aboutUs.html)、[BBFC](https://www.bbfc.co.uk/our-age-ratings)。

#### LUX-679：建立可失效的媒体内容分类索引

范围：新增分类等级目录及逐媒体条目的分级/标签索引。索引行存在表示该条目已被处理；缺行表示尚未就绪。只改数据库 schema 与 migration 回归，不接入索引 worker、NFO 失效触发器或用户授权查询。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0191_media_content_restriction_index.sql`、`migrations-postgres/0191_media_content_restriction_index.sql`、`tests/storage.rs`、`tests/postgres_database.rs`。

验收标准：

- [x] SQLite/PostgreSQL 都创建 MPA、TVPG、BBFC 等级目录，排序与 LUX-678 一致；逐媒体索引行、标签子表和查询索引使用外键/级联约束。
- [x] SQLite 从空库启动回归覆盖等级数据、非法等级/标签、索引就绪行及条目/索引级联删除；PostgreSQL bootstrap 回归覆盖相同 schema 合同。

短计划：先扩展 SQLite 和 PostgreSQL migration 回归并确认 schema/table 缺失时失败；再创建两份 0191 migration 及等级种子数据；最后运行 SQLite 定向用例、PostgreSQL migration 用例、Rust fmt 与 diff 检查。NFO 失效触发器因 SQLite 兼容表重建会移除新建触发器，留给后续独立任务处理。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test storage media_content_restriction_index_migration_runs_from_an_empty_database -- --exact`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test postgres_database postgres_media_content_restriction_index_migration -- --ignored --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 与 `git diff --check`。

依赖：LUX-678。

结果（2026-10-19）：SQLite 与 PostgreSQL schema 191 建立 18 个固定分级目录项、仅有索引行才表示就绪的媒体条目索引、标签子表及分级/标签查询索引；约束拒绝未知等级与空标签，媒体删除及索引删除均级联清理。SQLite 空库迁移和 PostgreSQL 本地 bootstrap 专项各 1/1 通过，build、fmt 和 diff check 通过。定向 Clippy 对新增代码在豁免既有 `too_many_arguments` 与测试目标 `type_complexity` 后通过。测试中发现 SQLite 的兼容表重建会丢弃 migration 新建的条目触发器，因此 0191 只建 schema；NFO 变更失效合同移至 LUX-680 单独处理，避免把未经修复的触发器描述为有效。

#### LUX-680：确保 SQLite 内容分类索引随 NFO 变更失效

范围：NFO 缓存文本变化时删除条目的分级/标签索引，并保证现有 SQLite `media_items` 兼容表重建不会丢失失效触发器。本任务只处理 SQLite；PostgreSQL 对应行为由 LUX-681 验证。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0192_media_content_restriction_index_invalidation.sql`、`src/storage/migration.rs`、`tests/storage.rs`。

验收标准：

- [x] NFO 缓存从旧值改为新值或清空时，SQLite 自动删除条目索引及其标签；无变化更新不影响已就绪索引。
- [x] 新库与会触发 catalog 兼容表重建的 SQLite 启动路径都保留此触发器。
- [x] 空库定向测试验证索引失效、级联清理和索引重新建立后的媒体删除路径。

短计划：先把已通过 migration 的回归扩展为 NFO 更新/清空的失效断言并确认失败；再新增 0192 触发器 migration，并在 SQLite catalog 兼容重建后幂等恢复触发器；最后重跑空库定向测试、build、fmt 与 diff 检查。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test storage media_content_restriction_index_migration_runs_from_an_empty_database -- --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 与 `git diff --check`。

依赖：LUX-679。

结果（2026-10-19）：新增 SQLite migration 0192；NFO 认证元数据实际变化或清空时，触发删除条目分类索引并级联删除标签。SQLite catalog 兼容表重建后会幂等恢复该触发器；无变化更新保留索引。空库专项测试 1/1、build、fmt、diff 检查通过；测试目标 Clippy 在豁免既有 `too_many_arguments` 与 `type_complexity` 后通过。

#### LUX-681：为 PostgreSQL 建立等价的内容索引失效触发器

范围：为 PostgreSQL 的 NFO 缓存变化建立与 LUX-680 相同行为的索引失效触发器，并通过本地 PostgreSQL 集成测试验证更新、清空、无变化更新和标签级联清理。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`migrations-postgres/0192_media_content_restriction_index_invalidation.sql`、`tests/postgres_database.rs`。

验收标准：

- [x] PostgreSQL 对 `nfo_metadata_json` 使用行级 `AFTER UPDATE` 触发器，只在旧值与新值不同时删除分类索引和标签。
- [x] PostgreSQL 空库测试验证无变化更新保留索引、内容变化或清空时失效，以及后续媒体/索引级联删除。
- [x] PostgreSQL 定向集成测试、build、fmt 与 diff 检查通过。

短计划：先扩展专用 PostgreSQL migration 测试并确认 NFO 变化不会使现有索引失效；再添加与 LUX-680 同版本的 PostgreSQL 函数/触发器 migration；最后在本地 PostgreSQL 上运行专项测试并执行格式与构建检查。

验证：确认 Toshiba 已挂载后运行 `NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test postgres_database postgres_media_content_restriction_index_migration -- --ignored --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 与 `git diff --check`。

依赖：LUX-679、LUX-680。

结果（2026-10-19）：PostgreSQL schema 192 使用 `AFTER UPDATE OF nfo_metadata_json` 行级触发器，并以 `OLD ... IS DISTINCT FROM NEW ...` 条件仅对真实变化失效索引；认证变化与清空均会级联删除标签，无变化更新保留索引。PostgreSQL migration/行为专项 1/1、build、fmt、diff 和过滤既有测试 lint 后的 Clippy 通过。实现依据 PostgreSQL 16 的 [CREATE TRIGGER](https://www.postgresql.org/docs/16/sql-createtrigger.html) 和 [PL/pgSQL Trigger Functions](https://www.postgresql.org/docs/16/plpgsql-trigger.html) 说明。

#### LUX-682：规范化 NFO 内容索引投影

范围：将已缓存 NFO 的认证文本和类型标签转换为有界内容索引投影。认证只接受 MPA、TVPG、BBFC 已知代码的并集；具体限制仍按用户选择的分级体系解释。类型标签去除首尾空白、折叠内部空白、统一小写、去重并排序。未知认证按未分级处理；标签超过索引合同边界时拒绝整个投影，不截断标签，以免漏掉禁止标签。此任务只增加域层纯函数，不读取文件或改变授权。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`src/domain/content_restrictions.rs`。

验收标准：

- [x] 已知认证代码可规范化；未知或数值评分保持未分级，认证不推断来源体系。
- [x] 标签以稳定顺序规范化、去重；空标签忽略，非法/超界标签数据拒绝整个投影。
- [x] 域层回归覆盖规范化、未知认证、多语言标签、非法标签和超过上限的标签组。
- [x] 定向 Rust 测试、fmt 与 diff check 通过。

短计划：先加入域层投影回归并运行 `content_restrictions` library 测试确认缺少实现；再实现认证并集规范化和有界标签投影；最后运行定向测试、fmt 与 diff 检查。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib content_restrictions`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 与 `git diff --check`。

依赖：LUX-678、LUX-679。

结果（2026-10-19）：域层现从认证代码并集规范化已知等级（后续仍由所选体系解释），将 NFO 类型标签折叠空白、统一小写、去重并稳定排序；未知/数值认证按未分级处理，空标签忽略，控制字符、超长标签或超过 100 个不同标签会拒绝整个投影，避免截断漏掉禁止标签。`cargo test --locked --lib content_restrictions` 7/7、fmt 与 diff check 通过。严格库 Clippy 仍被未修改的 `serve_emby_media_file` 与 `verify_emby_direct_stream` 两个 `too_many_arguments` 报错阻挡；豁免此既有 lint 后通过。无文件读取或授权行为变更。

#### LUX-683：存储内容分类索引候选并校验 NFO 快照

范围：为内容索引回填提供有界缺失候选查询和按 NFO 缓存快照原子写入。候选仅包含未删除且无索引行的媒体，按 ID 稳定排序并限制最多 100 项；查询只读取数据库中的 NFO JSON。写入前在 PostgreSQL 锁定媒体行，在 SQLite 使用立即写事务，且只有当前 NFO JSON 与读取快照完全相同才替换索引及标签；NFO 为空仍可写入已就绪的未分级行。此任务不解析 NFO、不启动 worker 或执行授权。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/mod.rs`、`src/storage/catalog.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 缺失候选查询有稳定顺序、服务端批次上限，排除已索引和已删除媒体，空 NFO 仍作为候选。
- [x] SQLite/PostgreSQL 写入只在 NFO 快照匹配且媒体未删除时成功；标签与索引行原子替换，陈旧快照不产生索引。
- [x] SQLite 与 PostgreSQL 存储回归覆盖上限、候选过滤、NFO 改变后的陈旧写入拒绝、空快照就绪状态。
- [x] 定向 Rust 测试、构建、fmt 与 diff check 通过。

短计划：先增加 SQLite/PostgreSQL 共用存储回归并确认候选/快照 API 缺失时失败；再实现有界查询、后端特定事务锁和索引原子替换；最后执行存储定向测试（含可用的本地 PostgreSQL ignored 用例）、build、fmt 与 diff 检查。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib storage::repository::repository_tests::media_content_restriction_index_storage_uses_bounded_snapshot_writes -- --exact`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib storage::repository::repository_tests::postgres_media_content_restriction_index_storage_uses_bounded_snapshot_writes -- --ignored --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 与 `git diff --check`。

依赖：LUX-679、LUX-681、LUX-682。

结果（2026-10-19）：存储层现在以 ID 顺序读取未删除且缺少索引的缓存条目，硬上限 100，空 NFO 作为可索引的未分级快照。SQLite 立即写事务与 PostgreSQL 媒体行锁保护快照比较；只有未删除条目的 NFO JSON 完全匹配才会事务性替换索引和标签。SQLite/PostgreSQL 共用回归各 1/1 通过，覆盖已就绪/删除过滤、批次上限、陈旧快照、空 NFO 与写标签约束失败回滚；`cargo build --locked`、fmt、diff check 和豁免既有 `too_many_arguments`/`type_complexity` 的全目标 Clippy 通过。后台 worker 尚未接入。

#### LUX-684：从缓存 NFO 分批建立内容分类索引

范围：增加 application 层批处理器，从数据库候选读取的缓存 NFO JSON 派生认证与类型标签投影，使用 LUX-683 的快照写入合同落索引。支持可选 ID 游标，坏缓存或变化中的条目不会挡住后续批次；空缓存登记为已处理的未分级项。此任务不从磁盘读取 NFO、不启动常驻 worker、不改变媒体访问。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/mod.rs`、`src/application/content_restriction_index.rs`、`src/storage/catalog.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 存储候选查询支持 ID 游标，边界批次稳定且不会重复查询游标之前的未处理条目。
- [x] application 批处理只解析 NFO 缓存、不触碰文件；坏 JSON/非法标签不标记就绪，并仍返回游标推进到后续条目。
- [x] 有效缓存中的认证与类型标签进入索引；无 NFO 缓存可以登记为未分级就绪；陈旧快照不写入。
- [x] Rust 定向回归、build、fmt 与 diff check 通过。

短计划：先增加存储游标及 application 批处理回归并确认接口/实现缺失时失败；再添加可选游标查询与缓存投影处理；最后运行定向 Rust 测试、build、fmt 与 diff 检查。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib storage::repository::repository_tests::media_content_restriction_index_storage_uses_bounded_snapshot_writes -- --exact`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib storage::repository::repository_tests::postgres_media_content_restriction_index_storage_uses_bounded_snapshot_writes -- --ignored --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib application::content_restriction_index::tests::index_batches_use_cached_nfo_and_advance_past_invalid_entries -- --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 与 `git diff --check`。

依赖：LUX-682、LUX-683。

结果（2026-10-19）：application 批处理器逐条消费缓存 NFO JSON，通过领域投影和 LUX-683 快照 CAS 写入；缓存不存在会登记为未分级就绪，坏 JSON/超界标签保留未就绪但游标仍向后推进。SQLite application 回归 1/1、SQLite/PostgreSQL 存储游标回归各 1/1、build、fmt、diff check 和豁免既有 `too_many_arguments`/`type_complexity` 的全目标 Clippy 通过。处理器还未由后台生命周期启动，准备在 LUX-685 接入。

#### LUX-685：启动内容分类索引后台 worker

范围：在 database selection 完成后启动内容索引后台 worker，并纳入 AppState 优雅关闭。worker 用 LUX-684 的 ID 游标分批处理现有及后续 NFO 变化，只访问缓存，不从媒体路径读取 NFO；每批不超过 100 项，错误重试，坏缓存/非法投影不就绪但不阻塞之后条目。日志只记录批次数量与错误，不写 NFO 文本。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/content_restriction_index.rs`、`src/application/mod.rs`、`src/api/legacy.rs`、`src/main.rs`。

验收标准：

- [x] worker 在数据库选择状态确认后启动，不在 database-selection 阻塞模式运行；重复启动不会生成重复 worker。
- [x] 现有缓存与后来失效的索引都会被有界回填；坏项不饿死后续批次，无候选时退避等待。
- [x] AppState shutdown 会停止并等待 worker；后台日志不含 NFO 内容或外部路径。
- [x] SQLite worker 启动/处理/关闭回归、build、fmt 与 diff check 通过。

短计划：先增加 worker 启动和处理的 SQLite 回归并确认 worker 不存在；再实现可退避的游标循环、AppState 持有与 shutdown、main 启动接线；最后运行定向测试、build、fmt 与 diff 检查。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib application::content_restriction_index::tests::background_worker_indexes_cached_metadata_and_shuts_down -- --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 与 `git diff --check`。

依赖：LUX-684。

结果（2026-10-19）：内容分类索引 worker 现在由主进程在数据库选择完成后启动，并由 AppState 持有、重复调用不重复创建，关闭时等待其退出。worker 每批最多 100 项，沿 ID 游标继续处理，坏 JSON/非法标签只记录汇总且不就绪，无候选时 10 秒退避，数据库错误 1 秒后重试。SQLite worker 处理及关闭回归 1/1、build、fmt、diff check 和豁免既有 `too_many_arguments`/`type_complexity` 的全目标 Clippy 通过；不读取磁盘 NFO。

#### LUX-686：提供内容分类索引就绪度管理员 API

范围：增加只读管理员 API，汇总未删除媒体条目总数、已建立分类索引数和未就绪数。统计口径与后台回填一致；仅管理员可读，不返回条目名称、路径、NFO 或用户策略。此任务不改变媒体访问，也不做前端展示。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/catalog.rs`、`src/storage/repository_tests.rs`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [x] 存储统计排除已移除媒体，并准确计算总数、已索引数与待处理数。
- [x] SQLite 与 PostgreSQL 存储合同覆盖空库、部分索引及移除条目。
- [x] 只读管理员端点拒绝匿名及普通账户，并返回 camelCase 汇总，不泄露媒体内容或路径。
- [x] 定向存储/API 测试、build、fmt 与 diff check 通过。

短计划：先在共享存储回归和管理员 HTTP 集成测试中加入就绪度断言并确认接口缺失时失败；再实现有界 SQL 汇总与管理员路由；最后运行目标测试、build、fmt 与 diff 检查。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib storage::repository::repository_tests::media_content_restriction_index_storage_uses_bounded_snapshot_writes -- --exact`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib storage::repository::repository_tests::postgres_media_content_restriction_index_storage_uses_bounded_snapshot_writes -- --ignored --exact`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test users admin_can_read_content_restriction_index_readiness -- --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、fmt、全目标 Clippy（放行两个既有 lint）与 `git diff --check`。

依赖：LUX-679、LUX-683、LUX-685。

结果（2026-10-19）：存储层用单条 LEFT JOIN 聚合活跃条目总数与已有索引数，已移除媒体不计入，待处理数由两者差值生成。只读端点 `/api/v1/admin/content-restriction-index` 要求管理员会话，响应仅含 `totalItems`、`indexedItems`、`pendingItems`。SQLite 与本地 PostgreSQL 存储回归各 1/1、管理员 HTTP 回归 1/1、`cargo build --locked`、fmt、diff check 和豁免既有 `too_many_arguments`/`type_complexity` 的全目标 Clippy 通过。UI 展示由后续独立任务接入。

#### LUX-687：为内容限制索引状态增加 Web API 客户端

范围：为 LUX-686 的只读管理员端点增加 TypeScript 状态类型和客户端方法，沿用现有请求层、camelCase 字段和错误处理。不在本任务修改页面。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] Web 类型准确描述活跃总数、已索引数与待就绪数。
- [x] 客户端使用管理员就绪度路径发起只读 GET，并解码响应。
- [x] API 客户端定向测试、Web typecheck/build 与 diff check 通过。

短计划：先加客户端合同测试并确认方法缺失时失败；再增加类型和 GET 方法；最后运行定向 Vitest、格式与 diff 检查。

验证：运行 `pnpm --dir web install --frozen-lockfile`、`pnpm --dir web exec vitest run tests/api-client.test.ts -t "reads content restriction index readiness"`、`pnpm --dir web build` 与 `git diff --check`。

依赖：LUX-686。

结果（2026-10-19）：新增 `AdminContentRestrictionIndexStatus` 类型和 `adminContentRestrictionIndexStatus()` GET 客户端方法；定向 API 客户端回归 1/1、冻结安装和 Web typecheck/build 通过。完整 `pnpm --dir web test` 为 706 passed、4 failed；失败分布于未改动的 `home-page.test.tsx`、`library-card.test.tsx`（2 项）和 `media-query-polling.test.tsx`，本任务未复现这些失败的基线状态。仓库未配置 Prettier 脚本或依赖，`git diff --check` 作为文本格式检查通过。页面展示留待 LUX-688。

#### LUX-688：在用户内容限制页展示索引进度

范围：将 LUX-686 管理员索引状态显示在用户详情的模块五页面，提供加载、错误重试、无媒体和部分/全部就绪状态。明确说明当前页仅展示索引回填状态，用户限制配置和访问拦截仍未接入。不修改索引 worker 或媒体访问行为。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/src/features/admin/user-management.css`、`web/tests/admin-user-content-restriction.test.tsx`。

验收标准：

- [x] 模块五页以服务端计数显示已索引、待就绪及整体进度。
- [x] 加载、API 错误和空媒体状态均有清晰、可访问的提示；错误可重试。
- [x] 页面文案不暗示策略配置或访问拦截已经生效。
- [x] 组件回归、Web build 与 diff check 通过，窄屏布局无溢出。

短计划：先添加模块五页面组件回归并确认旧占位缺少进度而失败；再以 React Query 接入 LUX-687 客户端并实现各状态与响应式样式；最后跑定向 Vitest、build、浏览器窄屏检查与 diff 检查。

验证：运行 `pnpm --dir web exec vitest run tests/admin-user-content-restriction.test.tsx`、`pnpm --dir web build`、Playwright 本地浏览器检查及 `git diff --check`。

依赖：LUX-686、LUX-687。

结果（2026-10-19）：用户详情模块五页接入服务端索引汇总，展示回填进度并在待处理期间每 5 秒刷新；加载、失败重试、无媒体、部分及全部就绪均有独立状态。页面明确说明用户限制配置和媒体访问拦截尚未接入。组件测试 4/4、Web build 和 diff check 通过。Playwright Chromium 在 390×844 视口显示 76/100、待就绪 24；文档宽度 390px、进度卡片宽 366px，无横向溢出，控制台无错误；预览截图保存在 `output/playwright/user-content-index-390.png`。完整 `pnpm --dir web test` 为 710 passed、4 failed，失败仍为未改动的 `home-page.test.tsx`、`library-card.test.tsx`（2 项）和 `media-query-polling.test.tsx`；该结果与 LUX-687 完整 Web 测试相同，未在本任务重现其基线原因。

#### LUX-689：开放管理员用户内容限制策略 API

范围：为管理员提供单用户内容限制策略读取和整组版本化保存 API。读取未配置策略时返回 `policy: null`，维持旧账号无限制行为；写入校验分级体系、最大等级与标签，并以存储 CAS 原子替换策略及允许/禁止标签。成功写入记录不含标签明文的审计摘要；不接 Web 客户端/UI，也不在本任务改变媒体访问。

预计修改文件（6 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/application/mod.rs`、`src/application/user_content_restrictions.rs`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [x] 只有管理员能读取或写入；写入要求管理员 Web session 和 CSRF。
- [x] 缺省策略以 `null` 返回；创建使用版本 0，更新需匹配版本，冲突返回 409，不存在/不可用账号返回 404。
- [x] 只接受 MPA、TVPG、BBFC 有效分级；等级须属于所选体系；标签去除首尾空白、规范化小写、拒绝重复/空/超长/控制字符及超过 100 项。
- [x] 策略及允许/禁止标签整组 CAS 写入，响应含稳定版本和更新时间；审计不包含标签值或凭证。
- [x] 用户 HTTP 定向回归、build、fmt 与 diff check 通过；API 合同同步。

短计划：先添加管理员/普通用户/匿名权限、缺省读取、校验、CSRF、CAS 冲突及规范化回读的 HTTP 回归并确认路由未开放；再实现应用层输入校验、API 接线和脱敏审计；最后运行 `users` 定向测试、build、fmt 与 diff 检查。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test users admin_can_manage_user_content_restrictions -- --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 与 `git diff --check`。

依赖：LUX-677、LUX-678、LUX-688。

结果（2026-10-19）：应用层提供策略读取/更新和规则校验；管理员 GET 对未配置策略返回 `null`，PUT 使用版本 CAS 原子替换分类字段和两组标签。仅管理员可读，写操作通过 CSRF；审计只写分级体系、是否有最大等级、标签数量及版本，不写标签值。分级体系与等级需匹配；标签规范化与媒体索引一致，并拒绝控制字符、空值、超长、重复及每组超过 100 项。HTTP 回归 1/1、应用层回归 2/2、`cargo build --locked`、fmt check 和 diff check 通过。策略配置尚未接 Web 客户端/UI，媒体读取路径也尚未应用限制。

#### LUX-690：为管理员用户内容限制 API 增加 Web 客户端合同

范围：新增单用户内容限制策略的 Web 类型、只读 GET 和版本化整组 PUT 客户端方法。客户端使用 LUX-689 的管理员路由与字段合同，写入沿用统一请求层的 CSRF 注入；不修改用户详情页面。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] 类型涵盖未配置策略的 `null`、全部策略字段及版本化更新输入。
- [x] GET/PUT 路径、序列化请求、响应解码和 CSRF 注入均有客户端回归。
- [x] 定向 Vitest、Web build 和 diff check 通过。

短计划：先加入未配置读取与版本化保存客户端合同并确认类型/方法不存在时失败；再添加策略类型和 GET/PUT 方法；最后运行 API 客户端定向测试、Web build 与 diff 检查。

验证：运行 `pnpm --dir web exec vitest run tests/api-client.test.ts -t "reads and updates admin user content restrictions"`、`pnpm --dir web build` 与 `git diff --check`。

依赖：LUX-689。

结果（2026-10-19）：增加内容限制策略/更新输入/响应类型，并实现按编码用户 ID 读取和整组 PUT 的客户端方法；PUT 沿用请求层 CSRF 注入。API 客户端回归 1/1、`pnpm --dir web build` 和 `git diff --check` 通过。页面配置留给 LUX-691。

#### LUX-691：在用户详情配置内容限制策略

范围：在模块五用户详情中读取、编辑并保存分级体系、最高等级、未分级规则、允许标签和禁止标签；未配置策略使用不限内容的表单默认值，使用已读取版本提交 CAS。显示读取/保存/冲突状态，并保留索引进度及“访问拦截尚未接入”提示。回收站账号只读；本任务不应用媒体访问过滤。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/src/features/admin/user-management.css`、`web/tests/admin-user-content-restriction.test.tsx`。

验收标准：

- [x] 管理员可配置三类分级体系、该体系的有效最高等级、未分级规则及两组标签；切换体系不会保留不兼容等级。
- [x] 未配置策略显示为不限内容，保存新策略提交版本 0；成功后展示服务端规范化结果和新版本。
- [x] 加载、错误重试、保存中、成功和版本冲突均清楚可访问；回收站不可编辑。
- [x] 页面继续明确说明媒体访问拦截尚未接入；定向组件测试、Web build、窄屏浏览器检查和 diff check 通过。

短计划：先扩展模块五组件测试覆盖默认、已保存策略、输入编辑、CAS 版本与错误状态并确认策略表单不存在；再接 LUX-690 客户端实现表单和响应式样式；最后运行组件测试、Web build、窄屏浏览器验证与 diff 检查。

验证：运行 `pnpm --dir web exec vitest run tests/admin-user-content-restriction.test.tsx`、`pnpm --dir web build`、Playwright Chromium 窄屏浏览器检查及 `git diff --check`。

依赖：LUX-690。

结果（2026-10-19）：模块五用户详情加入分级体系/最大等级、未分级开关和逐行允许/禁止标签表单；未配置策略以默认不限制状态呈现，创建提交版本 0，保存后用服务端规范化结果更新表单。切换体系会清除不兼容等级；CAS 冲突禁用再次提交并提供重新读取操作。错误可重试，回收站只读；页面说明保存的策略还未用于媒体访问过滤。模块五组件测试 9/9、播放策略回归 3/3、冻结依赖安装、Web build、diff check 通过。Playwright Chromium 390×844 检查显示文档宽 390px、策略卡宽 366px、表单单列且无横向溢出；mock API 后控制台错误 0，截图为 `output/playwright/user-content-policy-390.png`。完整 Web 套件 716 passed、4 failed；失败为既有 `home-page.test.tsx` 1 项、`library-card.test.tsx` 2 项、`media-query-polling.test.tsx` 1 项，本次未改这些文件，未验证它们的干净基线。

#### LUX-692：在库内容分页前应用用户内容限制

范围：让 Lux 与 Emby 共用的媒体库过滤列表在 SQL 分页前执行已保存的内容限制，匹配总数同步过滤。有效分级/标签策略下，条目或其剧集/季度上级缺少分类索引时拒绝展示；管理员及未配置或完全不收紧的策略维持原结果。限制生效版本进入库页缓存键，更新策略后不得复用旧权限页。本任务不覆盖搜索、首页推荐、详情、图片、字幕、下载或播放入口。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/catalog.rs`、`src/storage/repository.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 管理员、无策略和完全不收紧策略不改变列表结果；有效策略按分级/未分级、允许标签及优先的禁止标签过滤。
- [x] 媒体条目、剧集、季度和单集的列表遵守自身与对应上级限制；索引缺失时限制策略 fail closed。
- [x] SQL 在 `LIMIT/OFFSET` 前过滤；总数与可见条目一致，策略版本区分缓存项。
- [x] SQLite 存储回归验证规则组合、父级约束、缺少索引和过滤后分页；相关 library page/API 定向测试、Rust build、fmt 和 diff check 通过。

短计划：先在存储回归建立含分级、标签、父级和缺失索引的 fixture，并断言过滤后的计数/分页失败；再将策略谓词加入通用 catalog filter，将非管理员用户策略及版本带入缓存请求；最后运行存储与库列表定向回归、Rust build、fmt 和 diff check。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib content_restrictions_filter_catalog_rows_before_pagination`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib library_page_applies_user_policy_and_uses_policy_version_in_cache_key`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test catalog`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 与 `git diff --check`。

依赖：LUX-691。

结果（2026-10-19）：Lux 与 Emby 共用的分页查询现在从选中条目、父级季度和系列检查分级与标签策略；索引缺行按不允许访问处理，允许标签需至少命中一项，禁止标签优先。过滤条件进入 SQL 的计数和分页子查询，查询发生在 LIMIT/OFFSET 之前。策略版本加入 library page cache key；管理员不应用用户策略，无策略和完全不收紧策略与既有列表一致。存储回归 1/1、策略更新与缓存版本应用回归 1/1、`tests/catalog.rs` 3/3、`cargo build --locked`、fmt 与 diff check 通过。本任务只覆盖通用媒体库过滤列表；搜索、首页/推荐、详情、图片、字幕、下载和播放仍待实施。

#### LUX-693：定义单条媒体内容限制判定

范围：为用户内容限制提供可复用的领域判定，统一校验所选分级体系中的已知等级、最大等级、未分级规则和标签策略。未知/跨体系分级按未分级处理；无效策略失败关闭；允许标签要求至少一项命中，禁止标签优先。不在本任务连接具体 API 或媒体入口。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`src/domain/content_restrictions.rs`。

验收标准：

- [x] 三种分级体系按规格顺序比较，BBFC `12` 与 `12A` 同级；选用其他体系时无法映射的等级遵循未分级开关。
- [x] 标签匹配使用索引及用户策略的规范化值；禁止标签优先，允许标签列表非空时必须命中至少一项。
- [x] 未限制策略为允许；错误的持久化策略配置按拒绝处理。
- [x] 领域单测覆盖等级、未分级、标签交叉与无效策略；格式及 diff check 通过。

短计划：先为纯判定 API 添加三种分级体系、未知等级、标签交集及禁用优先回归；再实现无 I/O 的策略评估函数并运行领域定向测试。

验证：运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib content_restrictions::tests`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 与 `git diff --check`。

依赖：LUX-678。

结果（2026-10-19）：新增纯领域策略判定，验证 MPA/TVPG/BBFC 顺序、BBFC 12/12A 同级、跨体系与未知分级的未分级行为、允许标签至少命中一项、禁止标签优先和索引缺失 fail closed；损坏的等级体系、最高等级或标签记录失败关闭，完全不限制策略不要求索引。`content_restrictions::tests` 11/11、Rust fmt check 与 diff check 通过。

#### LUX-694：在单条媒体授权中执行内容限制

范围：将 LUX-693 的领域规则接入 `MediaAccessService::can_view_item`，使 Lux/Emby 单条详情及现有经该服务保护的图片、字幕、下载和播放入口共享用户内容限制。检查条目自身及系列/季度父级的索引；活跃策略遇到缺失索引或损坏配置时拒绝。管理员、无策略、无收紧项及媒体库 ACL 合同保持原行为。本任务不筛选父容器的子项分页或首页/搜索列表。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/access.rs`、`src/domain/content_restrictions.rs`、`src/storage/catalog.rs`、`src/storage/repository.rs`。

验收标准：

- [x] 单条访问先通过现有媒体库 ACL，再同时满足条目自身和关联系列/季度的内容限制；禁止项不得通过直接 ItemId 访问。
- [x] 缺失目标/上级索引、无效持久化策略和未知体系按拒绝处理；无策略与完全不收紧策略维持现有访问，管理员沿用既有绕过。
- [x] 原本经 `can_view_item` 保护的 Lux/Emby 详情、图片、字幕、下载与播放入口自动使用该判定；访问回归覆盖策略拒绝和父级继承。
- [x] application 定向测试、内容策略领域测试、Rust build、fmt 与 diff check 通过。

短计划：先在 `MediaAccessService` 测试创建用户、媒体库、等级/标签索引和父子层级并确认策略未拦截；再为 storage 增加有界条目+上级分类读取，将领域策略应用到所有候选；最后运行访问与内容策略定向回归、Rust build、fmt 和 diff check。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib content_policy_denies_direct_item_access_and_inherits_parent_restrictions`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib content_restrictions::tests`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 与 `git diff --check`。

依赖：LUX-693。

结果（2026-10-19）：`MediaAccessService::can_view_item` 在现有启用媒体库和 ACL 检查后读取用户策略，并按需取目标及系列/季度的分类索引；缺索引、无效策略或无可评估分类时拒绝。管理员和不存在/完全不收紧策略维持既有路径。详情、图片、字幕、下载、本地直放及 Emby 播放回调原本都使用该共享检查；ACL 1/1、playback 4/4、subtitles 1/1、download_api 1/1、image_api 1/1、直接访问/父级继承回归 1/1、领域策略 11/11、Rust build、fmt 和 diff check 通过。`cargo clippy --locked --lib -- -D warnings` 仍被未改动的 `src/api/media.rs::serve_emby_media_file` 与 `src/application/playback/session.rs::verify_emby_direct_stream` 两个 `too_many_arguments` 警告阻断。

#### LUX-695：在搜索候选分页前应用用户内容限制

范围：将同一内容限制谓词加入 Lux 与 Emby 共用的 FTS/LIKE 搜索候选查询，在排序、总数和分页前过滤等级、标签、上级条目及未就绪索引。管理员和无收紧策略保持原结果。搜索并发合并键包含策略版本，不能让不同版本的同用户请求共享旧结果。本任务不覆盖首页/推荐或媒体库子项列表。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/catalog.rs`、`src/storage/catalog.rs`。

验收标准：

- [x] Lux 与 Emby 搜索均在 SQL 的 `COUNT/LIMIT/OFFSET` 前执行统一内容限制；被过滤条目不增加总数或占用页位。
- [x] FTS、LIKE fallback、PostgreSQL LIKE 和 SQLite 深页 FTS+LIKE union 分支都传递策略与对应有序标签绑定。
- [x] 活跃策略下索引缺失 fail closed，父级限制继承；管理员及无策略、完全不收紧策略保持原结果。
- [x] 应用搜索回归验证等级过滤和准确总数；search/catalog、Rust build、fmt 与 diff check 通过。

短计划：先在 CatalogService 搜索回归创建可被同一搜索命中的允许/拒绝条目并确认尚未过滤；再让搜索并发键携带策略版本，并把版本化策略贯穿 SQLite/PostgreSQL 搜索绑定和所有分页分支；最后运行应用搜索、目录和内容策略回归及构建检查。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib library_search_applies_content_restrictions_before_pagination`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test catalog`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 与 `git diff --check`。

结果（2026-10-19）：搜索并发合并键加入策略版本；SQLite FTS、LIKE 回退和深页 FTS+LIKE union、PostgreSQL LIKE 查询都在排序/计数/分页前使用相同策略谓词，标签绑定按 blocked/allowed 顺序分别应用到各搜索分支。应用回归覆盖最高等级、允许和屏蔽标签、过滤后总数、LIKE 回退与深页 union。定向应用测试 1/1、`tests/catalog.rs` 3/3、`cargo build --locked`、fmt 和 diff check 通过；PostgreSQL 分支本任务未连接真实 PostgreSQL 实例验证。

依赖：LUX-694。

#### LUX-696：在继续观看候选分页前应用用户内容限制

范围：对 Emby 及独立继续观看 API 的已观看进度条目和“下一集”候选使用 LUX-693 的统一内容限制谓词，并在 SQL 排序、排名和分页前过滤，确保系列/季度父级及未就绪索引不能被直接继续观看列表绕过。管理员保持既有结果。缓存首页快照的策略失效、最近新增和推荐留给后续任务。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/catalog.rs`、`src/storage/catalog.rs`、`src/storage/repository.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 普通用户的继续观看进度项和下一集候选均满足自身及父级内容限制；缺失索引 fail closed。
- [x] 内容限制在 SQL 的总数、排名与分页前执行；过滤后总数及越界页计数正确。
- [x] 管理员和无策略用户继续沿用原行为；无标签限制时绑定列表不改变。
- [x] 存储与 CatalogService 回归覆盖等级过滤、父级、缺索引及下一集候选；相关测试、build、fmt 和 diff check 通过。

短计划：先在 CatalogService 继续观看回归建立带进度的允许/拒绝条目，并确认限制尚未应用；再把可选用户策略加入继续观看查询合同，在候选 CTE 和空页计数路径前执行统一策略谓词；最后跑应用与存储定向测试、Rust build、fmt 和 diff 检查。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib continue_watching_applies_content_restrictions_before_pagination`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib resume_items_apply_content_restrictions_before_ranking`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 与 `git diff --check`。

结果（2026-10-19）：Emby/独立继续观看服务及独立首页继续观看入口读取非管理员当前用户策略；存储在已保存进度候选和下一集候选过滤自身/系列/季度，并在计数/排名/分页之前应用相同谓词。缺失索引 fail closed，标签绑定按两个 SQL 候选分支重复且按序传入。存储回归 1/1、CatalogService 回归 1/1、原无策略 resume 页回归 1/1、`tests/resume_favorites.rs` 5/5、Rust build、fmt 和 diff check 通过。`cargo clippy --locked --lib -- -D warnings` 被 4 个参数数目 lint 阻断，其中 2 个是既有 `serve_emby_media_file` / `verify_emby_direct_stream`，另 2 个来自 LUX-695 新增的搜索方法；计划在后续小任务整理搜索查询参数合同。

依赖：LUX-695。

#### LUX-697：整理搜索策略查询参数合同

范围：将 LUX-695 搜索应用层与存储层传递的查询参数收敛为单个内部请求结构，消除新增的 Clippy 参数数目告警；保持 FTS/LIKE 分支、策略版本并发键、绑定顺序和查询结果不变。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/catalog.rs`、`src/storage/catalog.rs`、`src/storage/mod.rs`。

验收标准：

- [x] 搜索应用层与存储层均通过具名查询合同传递检索词、类型、媒体库、分页和用户策略。
- [x] SQLite FTS/LIKE/deep union、PostgreSQL LIKE 以及策略版本并发键行为保持不变。
- [x] 搜索定向测试、`tests/catalog.rs`、Rust build、fmt 和 diff check 通过；Clippy 不再报告这两个搜索函数参数数目告警。

短计划：先确认搜索测试与两个新告警的触发位置；再定义存储模块内部搜索参数结构并改为单参数传递；最后运行搜索、目录测试、build、fmt、diff check 与 Clippy。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib library_search_applies_content_restrictions_before_pagination`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test catalog`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check`、`git diff --check` 与 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo clippy --locked --lib -- -D warnings`。

结果（2026-10-19）：应用与存储共用具名 `CatalogSearchQuery` 传递搜索字段与策略，移除了 LUX-695 两个搜索方法的参数数量告警。策略版本并发键与 FTS、LIKE、深页 union 和 PostgreSQL LIKE 绑定保持不变。搜索回归 1/1、`tests/catalog.rs` 3/3、Rust build、fmt 和 diff check 通过。Clippy 仍被两处未改动的 `src/api/media.rs::serve_emby_media_file` 与 `src/application/playback/session.rs::verify_emby_direct_stream` 参数数量告警阻断。

依赖：LUX-695。

#### LUX-698：在主页继续观看与推荐缓存中执行内容限制

范围：主页继续观看列表通过统一分类策略过滤已保存进度和下一集候选。推荐候选在打分、候选池截取和每日批次写入前应用同一策略；用户策略版本区分主页缓存和每日推荐批次，策略更新后不返回旧推荐。本任务不覆盖最近新增、每库分组、合集或目录子项。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/catalog.rs`、`src/application/home.rs`、`src/storage/catalog.rs`。

验收标准：

- [x] `/api/v1/home` 的继续观看列表遵守自身/系列/季度限制，缺失索引 fail closed，并保持原分页总数行为。
- [x] 推荐 SQL 在打分候选进入 TOP/limit 前应用策略；限制版本变更后创建独立每日批次，不能重用旧策略 ID 列表。
- [x] 管理员和未配置策略保持既有结果；无收紧配置不阻断条目。
- [x] 推荐策略回归、主页策略版本缓存回归、相关目录/home 测试、build、fmt 与 diff check 通过。

短计划：先用主页/推荐回归确认未限制版本仍会缓存并返回高分级候选；再将策略传递到主页继续观看和推荐候选查询，并把策略版本纳入 HomeCacheKey 与每日批次 scope；最后运行应用、主页和存储推荐回归及构建检查。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib recommendations_apply_content_restrictions_before_candidate_pool`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib carousel_cache_isolated_by_content_restriction_version`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test catalog`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 与 `git diff --check`。

依赖：LUX-696。

结果（2026-10-19）：主页继续观看沿用统一内容限制谓词，继续观看计数与分页前候选一致。推荐查询在评分、TOP 候选及每日 ID 批次写入之前过滤；策略版本分别隔离主页缓存和持久化推荐批次。回归用 60 条受限条目和 1 条可见条目证明可见项不会被未过滤候选池挤掉，并覆盖管理员旁路、无策略和无收紧策略。

验证：`cargo test --locked --lib content_restriction` 21 passed、1 ignored（PostgreSQL 专用）；加强后的推荐回归 1/1；`carousel_cache_isolated_by_content_restriction_version` 1/1；继续观看策略回归 1/1；`tests/catalog.rs` 3/3；`tests/resume_favorites.rs` 中 `home_continue_watching_advances_to_next_unplayed_episode_after_completion` 1/1；`cargo build --locked`、fmt 检查和 `git diff --check` 通过。Clippy 仅被未改动基线中的 `src/api/media.rs::serve_emby_media_file` 与 `src/application/playback/session.rs::verify_emby_direct_stream` 两处 `too_many_arguments` 阻断。

#### LUX-699：首页最近新增列表应用用户内容限制

范围：`/api/v1/home` 的聚合最近新增列表及对应 CatalogService 查询，在总数计算和分页之前应用当前用户的统一内容限制谓词。过滤条目不得占用页位或计入总数；管理员、无策略和无收紧策略保持既有结果。本任务不处理按库分组的最新条目、合集卡片或目录子项。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/catalog.rs`、`src/application/home.rs`、`src/storage/catalog.rs`。

验收标准：

- [x] HomeService 将同一版本的用户策略传入最近新增列表；直接 CatalogService 入口按 principal 加载策略。
- [x] 自身、系列和季度限制及未就绪索引 fail closed；计数与分页使用相同谓词。
- [x] 管理员、无策略和完全不收紧策略不改变既有结果。
- [x] 回归证明较新的被限条目不会占据首个分页位置，过滤后总数准确；相关 catalog、build、fmt 和 diff check 通过。

短计划：先增加带允许、拒绝和缺失索引条目的应用回归，确认当前首页最近新增泄漏并污染分页总数；再将策略快照传入 storage 并在计数/分页 SQL 中使用统一谓词；最后跑内容策略、目录集成、build、fmt 和 diff 检查。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib home_recently_added_applies_content_restrictions_before_pagination`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib content_restriction`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test catalog`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 和 `git diff --check`。

依赖：LUX-698。

结果（2026-10-19）：HomeService 将首页统一读取的策略快照传入聚合最近新增；直接 CatalogService 入口按当前 principal 读取策略。存储层对可用与不可用父条目分支的 count/list SQL 都在分页前使用同一谓词，按 SQL 分支顺序重复绑定允许/屏蔽标签。应用回归覆盖较新的 R 内容、缺索引项、R 分级系列及其较低分级分集、较旧的允许内容及允许/屏蔽标签；首项和总数只返回允许内容。

验证：`home_recently_added_applies_content_restrictions_before_pagination` 1/1，覆盖管理员旁路、无策略和全允许策略仍返回原总数；`cargo test --locked --lib content_restriction` 22 passed、1 ignored（PostgreSQL 专用）；`tests/catalog.rs` 3/3；`cargo build --locked`、fmt 检查和 `git diff --check` 通过。Clippy 仅因未改动基线中的 `src/api/media.rs::serve_emby_media_file` 与 `src/application/playback/session.rs::verify_emby_direct_stream` 两处 `too_many_arguments` 阻断。

#### LUX-700：在按库最新条目候选限制前应用用户内容策略

范围：对 `/api/v1/home` 的每库最新分组，以及按库读取最近条目的两个 Lux API 入口，在候选排序和每库 Top-N 前执行当前用户内容策略。被拒条目和因自身、系列/季度或索引未就绪而拒绝的候选不能占据每库配额。全局合集卡片和合集成员列表留给后续任务。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/api/media.rs`、`src/application/catalog.rs`、`src/application/home.rs`、`src/storage/catalog.rs`。

验收标准：

- [x] HomeService 与两个按库 API 入口都使用请求用户的当前策略；管理员不受用户策略限制。
- [x] SQLite 与 PostgreSQL 的每库候选 SQL 都在 Top-N 之前过滤自身及父级限制，缺索引 fail closed。
- [x] SQL 标签绑定在重复的每库候选分支中保持正确顺序；隐藏候选不消耗可见条目的每库配额。
- [x] 无策略和无收紧策略保持既有结果；定向目录与主页回归、build、fmt 和 diff check 通过。

短计划：先扩展主页策略回归，确认当前每库最新分组会将最新受限条目放入配额；再让同一策略贯穿 HomeService、CatalogService 与 API 路由，并加入 SQLite/PostgreSQL 每库候选查询；最后运行内容策略、catalog、build、fmt 和 diff 检查。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib home_recently_added_applies_content_restrictions_before_pagination`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib content_restriction`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test catalog`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 和 `git diff --check`。

依赖：LUX-699。

结果（2026-10-19）：HomeService 和两个按库 API 路由都读取当前 principal 的内容策略；管理员旁路由策略服务返回 `None`。SQLite 与 PostgreSQL 在每库 Top-N 候选 SQL 中先应用共享限制谓词，覆盖条目自身及 series/season/episode 父级关系；无索引条目 fail closed。SQLite 两个候选分支按 WHERE、LIMIT 的顺序重复绑定标签参数。

验证：`home_recently_added_applies_content_restrictions_before_pagination` 1/1；`cargo test --locked --lib recent_catalog_rows` 3/3；`cargo test --locked --lib content_restriction` 22 passed、1 ignored（PostgreSQL 专用）；`tests/catalog.rs` 3/3；`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。`cargo clippy --locked --lib -- -D warnings` 仅被两处未改动基线 `too_many_arguments` 错误阻断：`src/api/media.rs::serve_emby_media_file` 与 `src/application/playback/session.rs::verify_emby_direct_stream`。

#### LUX-701：在虚拟合集中过滤受限内容

范围：虚拟合集卡片、最近更新卡片、成员列表、跨库副本/版本、默认海报回退、合集计数和来源偏好写入都按当前用户内容策略过滤；限制在卡片计数、成员分页及副本选择前执行。管理员、未配置/不收紧策略和媒体库 ACL 保持原合同。普通手动合集成员列表留给后续任务；本任务不改合集身份同步和数据库 schema。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/catalog.rs`、`src/storage/catalog.rs`、`src/storage/repository.rs`、`src/storage/mod.rs`。

验收标准：

- [x] 受限工作在虚拟合集卡片、成员列表与总数中不可见，不占分页位置。
- [x] 同一逻辑作品的受限副本不出现在来源/版本列表、播放来源及默认海报回退中；来源偏好不能指向受限副本。
- [x] 聚合库的虚拟合集数量与可见卡片一致；管理员、无策略和无收紧策略结果不变。
- [x] 统一分类谓词继续覆盖条目及缺失索引 fail closed；SQLite 定向回归、catalog、build、fmt 和 diff check 通过。

短计划：先扩展虚拟合集应用回归，加入允许副本、受限副本与仅受限逻辑作品，确认当前卡片/成员/来源会泄漏；再让限制谓词支持物理副本别名、将来源偏好写入收敛为具名请求，并贯穿合集存储查询、计数和偏好写入，在 SQL 计数与分页前过滤；最后运行合集、内容策略、catalog、build、fmt 和 diff 检查。

依赖：LUX-700。

结果（2026-10-19）：虚拟合集卡片、最新更新卡片、卡片成员数/海报回退、成员分页、跨库来源/版本、播放来源、来源偏好写入和聚合库数量统一按请求用户的内容策略过滤。共享谓词可指定物理副本别名，缺少索引的副本 fail closed；管理员策略旁路和无策略/全开放策略保持原结果。手动合集子项列表留待 LUX-702。

验证：`cargo test --locked --lib global_collection` 6/6；`cargo test --locked --lib content_restriction` 22 passed、1 ignored（PostgreSQL 专用）；`tests/catalog.rs` 3/3；`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。`cargo clippy --locked --lib -- -D warnings` 仅被两处未改动基线 `too_many_arguments` 错误阻断：`src/api/media.rs::serve_emby_media_file` 与 `src/application/playback/session.rs::verify_emby_direct_stream`。本任务未运行 PostgreSQL live-query 专项。

#### LUX-702：在手动合集子项分页前应用用户内容限制

范围：对普通媒体库中的手动合集子项列表，在 SQL 总数计算和 LIMIT/OFFSET 前应用当前用户内容策略，与媒体库 ACL 共用同一可见集合。覆盖 Lux 与 Emby 复用的 `CatalogService::list_collection_items`。合集容器自身没有媒体内容分级索引：详情仅在媒体库 ACL 允许且至少存在一个可见成员时返回，子项继续按内容策略过滤；不放宽图片等单条媒体资源的访问检查。本任务不修改合集成员关系、自动/虚拟合集或 schema。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/catalog.rs`、`src/storage/media.rs`。

验收标准：

- [x] 普通用户手动合集子项按自身、系列/季度父级分级和标签策略过滤；缺失索引 fail closed。
- [x] 过滤在总数、分页前执行；受限成员不占页位且不计入总数。
- [x] 受限用户能打开 ACL 允许且至少有一个可见成员的合集容器；单条媒体图片等访问校验不放宽。
- [x] 管理员、无策略和无收紧策略保持既有结果。
- [x] CatalogService 回归覆盖允许/拒绝、缺失索引、父级和分页计数；合集相关目标、build、fmt 与 diff check 通过。

短计划：先扩展 CatalogService 回归，在普通合集加入允许、受限、缺索引和父级受限成员，确认严格策略当前错误阻止合集容器访问；再让合集目录详情依赖过滤后的可见成员、列表入口对容器单独检查媒体库 ACL，并将统一策略谓词加入 count/list 查询；最后运行定向合集回归、相关内容限制测试、build、fmt 和 diff 检查。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib manual_collection_items_apply_content_restrictions_before_pagination`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib content_restriction`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 和 `git diff --check`。

结果（2026-10-19）：Lux 与 Emby 共用的普通合集成员查询在 SQL 计数和分页前应用当前用户策略、父级继承和媒体库 ACL。合集容器不做媒体分级判定；严格策略用户只有在库 ACL 允许且至少有一个可见子项时才能获取容器详情，直接图片等单条资源授权仍拒绝无索引容器。回归覆盖较早位置的 R/屏蔽标签成员、缺失索引成员、较低分级但属于 R 系列的分集及尾部允许成员，并验证管理员与无策略用户保持原结果。

验证：`manual_collection_items_apply_content_restrictions_before_pagination` 1/1；`content_restriction` 23 passed、1 ignored（PostgreSQL 专用）；`tests/collections.rs` 4/4；`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。`cargo clippy --locked --lib -- -D warnings` 仍仅被两处未改动基线 `too_many_arguments` 错误阻断：`src/api/media.rs::serve_emby_media_file` 与 `src/application/playback/session.rs::verify_emby_direct_stream`。未运行 PostgreSQL live-query 专项。

依赖：LUX-701。

#### LUX-703：在剧集与通用层级子项分页前应用用户内容限制

范围：为 CatalogService 的通用层级子项列表和剧集/季度集数列表，在 SQL 总数与 LIMIT/OFFSET 前执行当前用户内容策略。包含季条目自身、分集自身、季度与系列父级继承以及缺索引 fail closed；保留父级直接访问检查、媒体库 ACL 和现有排序。Lux 与 Emby 继续共享同一服务合同，不改 schema。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/catalog.rs`、`src/storage/catalog.rs`。

验收标准：

- [x] 通用子项、`/Shows/{id}/Episodes` 和季度兼容路径都在计数与分页前过滤内容策略。
- [x] 分集自身、季度/系列父级和未就绪索引规则一致；受限条目不占页位。
- [x] 管理员、无策略和无收紧策略保留结果与排序；父级 ACL/访问检查不变。
- [x] CatalogService 回归覆盖系列/季度继承、条目分级、缺索引和过滤后总数；定向测试、catalog、build、fmt 和 diff check 通过。

短计划：先增加 CatalogService 回归，放入允许分集、受限分集、缺索引分集以及受限季度下的允许分集，确认通用子项和剧集列表泄漏或污染分页；再将同一内容策略谓词加入两组 SQL 的 count/list 查询，并从应用层传入当前 principal 策略；最后跑定向策略和 catalog 测试、build、fmt 与 diff 检查。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib hierarchy_child_lists_apply_content_restrictions_before_pagination`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib content_restriction`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test catalog`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 和 `git diff --check`。

结果（2026-10-19）：通用层级子项与剧集/季度集数查询统一在计数和分页 SQL 中执行当前用户内容谓词。分集包含自身索引、季度及系列索引的继承判断；缺索引条目 fail closed。应用层保持父级 ACL 及直接访问检查，严格策略用户只有在选定系列/季度下有可见候选时获得相应结果。

验证：`hierarchy_child_lists_apply_content_restrictions_before_pagination` 1/1，覆盖受限季度、受限分集、缺索引分集及过滤后的总数/首位分页，并确认管理员和无策略结果；`content_restriction` 24 passed、1 ignored（PostgreSQL 专用）；`tests/catalog.rs` 3/3；`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。`cargo clippy --locked --lib -- -D warnings` 仍仅被两处未改动基线 `too_many_arguments` 错误阻断：`src/api/media.rs::serve_emby_media_file` 与 `src/application/playback/session.rs::verify_emby_direct_stream`。未运行 PostgreSQL live-query 专项。

依赖：LUX-702。

#### LUX-704：使媒体汇总与剧集数量遵守用户内容限制

范围：让 Emby `/Items/Counts` 的物理媒体汇总，以及目录条目携带的 series/season episode count，使用当前用户的可见媒体集合。内容策略在 SQL 聚合前过滤；管理员、无策略和无收紧策略保持既有数量。本任务不改变观看进度统计、管理员库诊断计数或 schema。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/catalog.rs`、`src/storage/catalog.rs`。

验收标准：

- [x] `/Items/Counts` 中影片、剧集、分集、合集和总数均排除受限条目，并保留收藏过滤语义。
- [x] 系列/季度 `episode_count` 在详情和各目录卡片中仅统计自身、季度/系列父级及索引都允许且存在有效来源的分集。
- [x] 无策略、全开放策略及管理员仍得到原计数；SQL 标签参数顺序正确。
- [x] 应用回归覆盖内容策略、父级、缺索引和汇总；定向统计、Emby counts、build、fmt 与 diff check 通过。

短计划：先在 hierarchy CatalogService 回归中为所有分集建立有效来源，并断言受限用户的 `/Items/Counts` 与可见季度分集数；确认当前汇总仍计入 R、父级受限和缺索引内容；再让物理统计与 episode count 共用内容谓词，并将当前请求策略传入详情/目录计数填充；最后跑定向回归、内容策略、Emby counts、build、fmt 和 diff 检查。

验证：确认 Toshiba 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib content_restricted_catalog_counts_and_episode_counts_match_visible_items`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib content_restriction`、`NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test emby_counts`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 和 `git diff --check`。

依赖：LUX-703。

结果（2026-10-19）：Emby `/Items/Counts` 的物理影片、剧集、分集、手动合集和总数在聚合前应用当前用户策略；严格策略下手动合集只有存在至少一个通过内容、父级、可用来源和媒体库访问检查的成员才计入。既有虚拟合集数量继续走 LUX-701 的可见成员谓词。系列/季度 episode count 统一按分集自身及季度/系列父级策略和有效来源统计，覆盖详情、媒体库页及层级子项卡片。回归覆盖 PG/R、禁止标签、父级继承、缺失索引、可见/全受限合集成员，以及管理员、无策略和全开放策略；SQL 中主项策略、合集成员库 ID、成员策略与收藏参数按占位顺序绑定。

验证：`content_restricted_catalog_counts_and_episode_counts_match_visible_items` 1/1；`content_restriction` 24 passed、1 ignored（PostgreSQL 专用）；`tests/emby_counts.rs` 1/1；`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。`cargo clippy --locked --lib -- -D warnings` 仍被两处未改动基线 `too_many_arguments` 错误阻断：`src/api/media.rs::serve_emby_media_file` 与 `src/application/playback/session.rs::verify_emby_direct_stream`。未运行 PostgreSQL live-query 专项。

#### LUX-705：提供批量启用与禁用用户 API

范围：新增管理员批量账号状态端点，接受明确勾选的 1–100 个用户 ID 和目标启用状态，拒绝重复目标；逐项复用单用户状态更新、最后管理员保护与禁用时的凭证撤销，并为每项目标返回稳定结果码。API 在禁用成功后停止对应服务端 HLS 会话并逐用户记审计事件。本任务不加入列表页面控件，也不支持“全部筛选结果”隐式扩展目标。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/admin_user_lifecycle.rs`、`src/application/mod.rs`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [x] 管理员端点要求 CSRF；空列表、超过 100 个 ID 和重复 ID 在执行前拒绝。
- [x] 每个有效目标逐项成功/失败；无效 ID、不存在用户、最后有效管理员及存储错误有稳定结果码，其他目标继续处理。
- [x] 禁用复用既有事务，撤销登录凭证并停止该用户服务端 HLS 会话；启用不恢复旧凭证。
- [x] API 集成回归覆盖未认证、缺 CSRF、输入边界、部分成功、最后管理员保护、禁用后拒绝登录及重新启用后的新登录；users、build、fmt 和 diff check 通过。

短计划：先在 `tests/users.rs` 为当前不存在的批量状态路由增加认证、部分成功和登录失效回归并确认失败；再添加逐用户应用服务，复用 `UserStore::update_user_with_hls_session_ids` 以及现有 HLS 停止路径，加入管理员端点和稳定 DTO；最后运行 `tests/users.rs`、build、fmt 与 diff 检查。

验证：确认 Toshiba 已挂载后运行 `NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test users`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 和 `git diff --check`。

结果（2026-10-19）：新增管理员批量账号状态应用服务和 `/api/v1/admin/users/batch/status`。请求必须是 1–100 个去重 ID；响应逐项返回 `INVALID_USER_ID`、`USER_NOT_FOUND`、`LAST_MANAGER` 或 `STORAGE_UNAVAILABLE`。每个状态变更单独复用既有事务和最后管理员保护；成功禁用后撤销登录凭证、停止返回的服务端 HLS 会话并写入 `USER_DISABLED` 审计事件，启用写入 `USER_ENABLED` 且不恢复旧登录。回归包含触发器注入的单用户存储失败，证明后续目标仍能成功；同时确认禁用使旧 Web 会话和密码登录失效，重新启用后可创建新会话。

验证：`tests/users.rs` 11/11、`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。`cargo clippy --locked --lib -- -D warnings` 仍仅被两处未改动基线 `too_many_arguments` 错误阻断：`src/api/media.rs::serve_emby_media_file` 与 `src/application/playback/session.rs::verify_emby_direct_stream`。本任务未运行全量 Rust 门禁。

#### LUX-706：增加批量账号状态 Web API 合同

范围：为 LUX-705 管理端点定义批量启用/禁用请求、逐项稳定错误码和响应类型，并在 Web API 客户端公开方法。请求继续使用统一客户端的 JSON、CSRF 与错误处理。不在本任务修改页面或执行全量 Web 套件。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/client.ts`、`web/src/lib/api/types.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] TypeScript 类型覆盖批量状态和无效 ID、用户不存在、最后管理员、存储失败四种逐项结果。
- [x] API 客户端 POST 到 `/api/v1/admin/users/batch/status`，发送精确用户 ID 列表及目标状态，并由统一请求层注入 CSRF。
- [x] 定向 API 客户端回归和 Web build、`git diff --check` 通过。

短计划：先在 API 客户端回归中断言批量禁用请求的 URL、JSON、方法和 CSRF，并确认目标方法不存在而失败；再添加类型和统一客户端方法；最后跑 `api-client.test.ts`、Web build 与 diff check。

验证：运行 `pnpm --dir web exec vitest run tests/api-client.test.ts`、`pnpm --dir web build` 和 `git diff --check`。

结果（2026-10-19）：增加批量账号状态逐项结果类型和 `updateAdminUserStatusBatch` 客户端方法，准确发送 `userIds` / `isDisabled` 并经统一请求层注入 CSRF。定向回归覆盖全部稳定结果码和请求合同。

验证：`tests/api-client.test.ts` 72/72、`pnpm --dir web build` 和 `git diff --check` 通过；未运行完整 Web 测试套件。

#### LUX-707：在用户列表批量启用或禁用账号

范围：在用户列表现有本页勾选操作区加入批量启用/禁用按钮。变更前确认所选人数、逐项用户名称及禁用/启用影响；提交 LUX-705 API 后逐项显示成功或错误原因。变更后清空勾选并刷新列表。本任务不选择当前筛选的其他分页用户。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUsersPage.tsx`、`web/tests/admin-user-batch-actions.test.tsx`。

验收标准：

- [x] 启用和禁用按钮只作用于当前页明确勾选的用户，并分别提供有目标名称与影响说明的确认。
- [x] 每项显示用户名称和成功/稳定失败原因；成功后清空勾选并刷新用户列表。
- [x] 换页、筛选或排序重置选择；请求中和偏好复制弹窗打开时禁用冲突操作。
- [x] 定向列表组件回归、Web build 与 `git diff --check` 通过。

短计划：先增加组件回归，确认选中用户后缺少批量状态按钮并失败；再接入 LUX-706 客户端方法，添加启用/禁用确认、逐项结果、错误映射和成功刷新；最后跑用户批量动作 Vitest、Web build 与 diff check。

验证：运行 `pnpm --dir web exec vitest run tests/admin-user-batch-actions.test.tsx`、`pnpm --dir web build` 和 `git diff --check`。

结果（2026-10-19）：用户列表新增批量启用/禁用操作，仅提交当前页勾选的 ID；确认框列出人数、名称并说明禁用时已交给播放器的 STRM 链接无法强制中断、重新启用需重新登录。服务端逐项结果展示本地用户名及错误原因，成功后清空勾选并刷新列表。批量请求和偏好复制期间禁用冲突操作；搜索、筛选、分页和排序变化清除旧选择与结果。

验证：`tests/admin-user-batch-actions.test.tsx` 6/6、`pnpm --dir web build` 和 `git diff --check` 通过。未运行完整 Web 测试套件。

#### LUX-708：为权限复制提供有界用户策略快照读取

范围：为模块四的“从来源账号复制权限到多个目标账号”建立存储读取合同，一次读取来源和最多 100 个目标的显式媒体库模式/库 ID、播放/下载/转码权限及内容限制快照与版本。已删除或不存在的账号不返回快照。该任务仅提供有界的一致读，不修改账号、预设或内容策略。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/mod.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 快照包含媒体库 `ALL` / `SELECTED` / `NONE` 与确定排序的库 ID、播放额度和三类转码开关、下载权限，以及可空的完整内容限制和版本。
- [x] 单次读取覆盖来源与至多 100 个目标；空集合不查询，目标缺失/删除可与空内容策略区分。
- [x] SQLite 回归验证完整值、无策略用户、缺失目标、排序稳定及上限；Rust build、fmt 和 diff check 通过。

短计划：先为新的批量权限快照读取补库测试并确认接口不存在而失败；再定义存储快照类型和单事务/有界批量查询；最后运行窄库测试、build、fmt 与 diff check。写入、预览 API 和 Web 界面留给后续 LUX 任务。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib user_permission_copy_snapshots`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 和 `git diff --check`。

依赖：LUX-627、LUX-628、LUX-614、LUX-675、LUX-677。

结果（2026-10-19）：新增按最多 101 个明确账号 ID（1 个来源 + 100 个目标）批量读取权限快照的存储合同。三条有界查询在单个一致性事务中读取用户权限、已选媒体库与完整内容限制；PostgreSQL 使用可重复读，SQLite 保持事务快照。无权限记录与无效目标明确区分，媒体库 ID 与限制标签稳定排序。定向存储回归 1/1、Rust build、fmt check 和 diff check 通过。

#### LUX-709：原子复制指定用户权限组

范围：在单目标事务中复制来源用户的媒体库、内容限制、播放/下载/转码三组权限，调用方提交预览得到的来源与目标快照用于变更检测。只更新明确选择的组；不复制服务器管理员/管理控制台角色、预设来源、账号资料、偏好或用户数据。来源指定媒体库失效时拒绝整个目标变更。批量逐项结果和 API 留待后续任务。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/mod.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] Library、Content、Playback 三组可独立选择并在一个目标事务中应用；未选择组保持原值，管理角色与预设来源不变。
- [x] 来源/目标所选组快照变化时返回冲突；缺失或已删除账号返回不可用，不留下部分写入。
- [x] 内容限制按目标版本单调递增复制或清除；库选择仅接受仍启用的普通媒体库。
- [x] SQLite 回归覆盖组隔离、CAS 冲突、复制/清除内容策略和失败回滚；Rust build、fmt、diff check 通过。

短计划：先补单目标复制存储回归并确认接口不存在而失败；再将 LUX-708 快照读取抽成事务内查询，增加 selected-groups CAS 写入和内容策略版本处理；最后跑定向存储测试、build、fmt 和 diff check。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib user_permission_copy`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 和 `git diff --check`。

依赖：LUX-708。

结果（2026-10-20）：新增单目标、单事务权限复制存储服务。Library、Content、Playback 三组分别比较预览快照，未选字段不变；目标内容策略版本保持单调递增，来源无策略时条件删除。所选库先批量校验仍为启用普通媒体库。PostgreSQL 按稳定 ID 顺序锁定来源与目标用户；现有媒体库授权与内容策略写入也获取相同用户锁，保证快照比较和复制串行化。管理员角色和预设来源从未进入复制字段。三项权限复制回归、既有媒体库事务与内容策略 CAS 回归通过；Rust build、fmt check 和 diff check 通过。

#### LUX-710：提供版本安全的权限复制预览与应用服务

范围：为 LUX-709 建立应用层预览/应用合同。预览返回选定组的来源快照以及来源和每个目标的不透明快照版本；应用先校验来源版本，再独立比较各目标版本并逐项执行存储事务。目标间允许部分成功，来源变化时整体拒绝。版本摘要只包含所选组，不暴露未选权限内容。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/mod.rs`、`src/application/user_permission_copy.rs`。

验收标准：

- [x] Library、Content、Playback 可任意组合；空组、自我复制、重复/无效 ID、空目标、超 100 目标及格式错误版本被拒绝。
- [x] 预览仅序列化所选来源组；来源与目标版本均由确定性策略快照摘要生成，不包含管理角色或未选内容。
- [x] 应用先检查来源版本；目标逐项返回用户不可用、版本冲突、无效策略或存储失败，目标冲突不阻断其他目标。
- [x] application 定向回归、Rust build、fmt 与 diff check 通过。

短计划：先为现有存储合同增加应用服务失败回归并确认模块不存在而失败；再增加严格输入、所选组快照摘要、预览 DTO 和逐目标应用服务；最后运行定向 application 测试、build、fmt 与 diff check。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib user_permission_copy`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 和 `git diff --check`。

依赖：LUX-708、LUX-709。

结果（2026-10-20）：增加权限复制预览与应用服务，校验所选权限组、来源及最多 100 个目标，并在任何读取前拒绝空组、自我/重复/无效 ID 和格式错误版本。来源及目标快照摘要包含规范账号 ID 和所选组；未选字段不输出。应用先校验来源版本，再独立比较每个目标版本并提交 LUX-709 单目标原子事务，返回部分成功结果；新增测试覆盖来源过期、目标冲突、显式空内容策略、无效请求和账号绑定版本。

验证：`cargo test --locked --lib user_permission_copy` 8/8、`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。Build 有 25 条该新应用模块尚未接入 API 前的 dead-code warning；后续 API 任务接线后将消除。

#### LUX-711：开放管理员用户权限组复制预览与应用 API

范围：为 LUX-710 应用服务增加管理员预览和应用端点。预览只要求管理员认证；应用要求管理员 Web session 和 CSRF。预览响应只包含选择的 Library、Content、Playback 来源快照、来源不透明版本和逐目标预期版本；应用按来源版本做整体冲突检查，再返回逐目标结果。仅对成功目标写脱敏审计，不记录权限值。本任务不加入 Web 客户端或页面。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [x] 预览和应用路由使用严格 JSON 输入、camelCase DTO、管理员授权；应用强制 CSRF，预览保持只读无 CSRF。
- [x] HTTP 回归覆盖未认证/非管理员、无 CSRF、非法请求、所选组隐私、来源版本整体冲突、目标部分成功和逐目标稳定错误码。
- [x] 每个成功目标有一条只含来源 ID 与组标志的审计事件；API 文档记录精确请求、响应和错误合同。
- [x] `tests/users` 定向回归、Rust build/fmt 与 diff check 通过。

短计划：先为权限复制预览与应用路由增加失败的 `users` 集成回归；再按偏好复制 API 的认证/CSRF 边界接入 LUX-710 服务、逐目标审计及错误映射；最后更新 API 文档并运行定向集成测试、build、fmt 和 diff check。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test users admin_can_preview_and_apply_user_permission_groups -- --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 和 `git diff --check`。

依赖：LUX-710。

结果（2026-10-20）：增加管理员权限复制预览和应用端点。预览仅需管理员认证且无 CSRF；应用使用管理员 Web session 与 CSRF。HTTP 回归覆盖匿名/非管理员拒绝、空权限组、未选择内容策略不出现在响应、来源过期返回 409 且不写目标、单目标冲突与另一目标成功、稳定错误码和成功目标审计脱敏。

验证：`tests/users::admin_can_preview_and_apply_user_permission_groups` 1/1、`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。

#### LUX-712：为用户权限复制增加 Web API 客户端合同

范围：为 LUX-711 权限组复制预览和应用 API 增加严格的 TypeScript 请求、响应及逐目标错误码类型，并提供客户端方法。两种 POST 继续使用统一 JSON、CSRF 和错误处理请求层；本任务不修改用户管理界面。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] 类型覆盖三组选择、Library/Content/Playback 快照、64 位版本令牌和所有稳定逐目标错误码。
- [x] 预览及应用方法使用精确路径和请求体，并复用统一请求层的 CSRF 与错误处理。
- [x] API client 定向 Vitest、Web build 和 diff check 通过。

短计划：先为两条新方法的路径、请求体、类型化响应及 CSRF 添加失败客户端回归；再增加类型和方法，运行定向 Vitest、Web build 与 diff check。

验证：`pnpm --dir web exec vitest run tests/api-client.test.ts -t "admin user permission copy"`、`pnpm --dir web build` 和 `git diff --check`。

依赖：LUX-711。

结果（2026-10-20）：新增权限复制的 TypeScript 输入、预览快照、应用结果及稳定逐目标错误码，并接入预览/应用客户端方法。两项 API client 合同测试验证精确 endpoint、JSON 请求、类型化响应与自动 CSRF。

验证：定向 `api-client.test.ts` 2/2、`pnpm --dir web install --frozen-lockfile` 和 `pnpm --dir web build` 通过。完整 `pnpm --dir web test` 为 721 passed、4 failed；失败仍为 `home-page.test.tsx`、`library-card.test.tsx` 两项及 `media-query-polling.test.tsx`，与既有记录的失败集合相同，本任务没有修改相关代码或测试。`git diff --check` 通过。

#### LUX-713：在用户列表实现权限组复制预览与应用界面

范围：在当前页已选择的用户列表操作区增加“预览并复制权限”流程，复用 LUX-712 客户端。管理员选择来源用户和 Library、Content、Playback 权限组后先预览；预览展示来源已选权限快照及每个目标的预览版本，确认后才应用，并逐用户显示结果。目标集合固定为打开对话框时本页明确勾选项；来源不可同时作为目标。来源或权限组改变后必须清除旧预览，应用仅能提交当前预览版本。成功后清除列表选择并刷新用户。账户偏好服务端持久化不在本任务范围内。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUsersPage.tsx`、`web/src/features/admin/AdminUserPermissionCopyDialog.tsx`、`web/src/features/admin/user-management.css`、`web/tests/admin-user-batch-actions.test.tsx`。

验收标准：

- [x] 列表只把当前页明确勾选用户传为目标；对话框展示固定目标、来源用户搜索/选择及三组权限复选项，来源不得是目标。
- [x] 预览只展示选中权限组的来源快照与目标版本；空组、来源/组变更、加载或请求错误不能遗留可应用的旧预览。
- [x] 仅在有效预览后可应用；逐用户呈现成功或稳定错误原因，成功后清空所选项并刷新列表。
- [x] 对话框具备加载/错误状态、Escape 关闭、焦点管理、窄屏可用布局；应用中不可关闭或重复提交。
- [x] 定向批量用户交互测试、Web build 和 diff check 通过；真实浏览器检查预览/应用关键交互与窄屏溢出。

短计划：先在现有批量用户组件测试中补上权限复制交互用例并确认当前缺少入口而失败；再新增对话框、连接已完成的 LUX-712 客户端并补齐样式；最后运行定向 Vitest、Web build、Playwright 桌面与窄屏验证及 diff check。

验证：`pnpm --dir web exec vitest run tests/admin-user-batch-actions.test.tsx`、`pnpm --dir web build`、Playwright 桌面与 390×844 检查、`git diff --check`。

依赖：LUX-712。

结果（2026-10-20）：用户列表新增独立的权限复制操作和预览/应用对话框。目标固定为当前页明确勾选账号，来源搜索排除目标；Library、Content、Playback 三组默认全选且可单独选择。仅在服务端预览成功后显示所选来源快照和逐目标版本，切换来源或组即清除预览；应用只提交预览返回的来源/目标版本，并对逐项目标结果映射稳定错误。成功后清空列表选择、刷新用户，同时在对话框保留结果。账户偏好服务端化仍按模块边界延后。

验证：TDD 初始回归在入口缺失时失败；`tests/admin-user-batch-actions.test.tsx` 8/8、冻结依赖安装、Web build 和 `git diff --check` 通过。完整 `pnpm --dir web test` 为 723 passed、4 failed；失败是 `home-page.test.tsx` 1 项、`library-card.test.tsx` 2 项、`media-query-polling.test.tsx` 1 项，与此前记录的失败集合相同，本任务没有改动相关代码。Playwright 使用模拟 API 在桌面 1280×900 和窄屏 390×844 检查预览/应用、逐项结果、权限组选项作废预览、Escape 关闭及焦点返回；窄屏文档宽 375px、滚动宽 375px，弹窗宽 359px、内容滚动宽 342px，无横向溢出，控制台错误 0。截图：`output/playwright/user-permission-copy-preview-desktop.png`、`output/playwright/user-permission-copy-preview-390.png`。首轮窄屏检查发现弹窗内溢出，经修复后复验通过。完整 Rust 门禁不属于本任务。

#### LUX-714：为管理员 Emby 登录凭证摘要提供分页存储查询

范围：基于现有 `access_tokens` 表，为指定用户分页读取未撤销的 Emby 登录凭证摘要，供模块七统一展示登录状态和设备信息。摘要包含凭证记录 ID、设备 ID/名称/类型、客户端名称/版本、创建/更新时间和最近访问时间；绝不返回 token、token hash 或其他凭证材料。本任务只增加有界只读存储合同，不修改登录验证、撤销行为、公开 API 或管理页面；无需新增 migration。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/mod.rs`、`src/storage/repository.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 只返回指定用户仍有效的 Emby 凭证摘要；已撤销凭证和其他用户记录不进入结果，也不读取/序列化 token hash。
- [x] 结果提供稳定的最近访问（缺失时回退更新时间）/记录 ID 顺序、匹配总数和分页；页大小在存储层限制为 1–100，负偏移归零。
- [x] SQLite 回归覆盖多设备/多 token、撤销过滤、用户隔离、稳定排序、分页上限和空结果；Rust 定向库测试、build、fmt 与 diff check 通过。

短计划：先在存储回归中增加有界 Emby 凭证摘要读取合同并确认新方法缺失而失败；再扩展存储摘要类型及查询；最后运行定向库测试、build、fmt 和 diff check。

结果（2026-10-20）：新增 `StoredEmbyLoginSessionSummary` 只含设备、客户端和时间字段；`list_user_emby_login_sessions_for_admin` 过滤已撤销/其他用户凭证，按 `COALESCE(last_seen_at, updated_at) DESC, id DESC` 排序，存储层限制页大小 1–100、负偏移归零，并返回总数。无 migration，也没有将 token hash 投影进查询或摘要结构。

验证：TDD RED 确认方法缺失时编译失败；最终 `admin_emby_login_session_page` 定向测试 1/1、`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。完整 `cargo test --locked --all-targets` 使用 `NO_PROXY=127.0.0.1,localhost` 后为 1048 passed、4 failed、26 ignored；三项 fill-missing 失败与先前记录的基线相同，另有 `local_metadata_worker_dispatches_fill_missing_without_blocking_scan` 在全量并发下超时、单独复跑通过，因此全量门禁未通过。第一次未设置 `NO_PROXY` 的全量运行中 `remote_body` 本地 HTTP 测试超时，补齐环境变量后的运行通过该测试。`cargo clippy --locked --all-targets --all-features -- -D warnings` 未通过，报告 8 条非本查询实现范围的 lint（`admin_users.rs`、`media.rs`、`playback/session.rs`、`user_permission_copy.rs`、`users.rs` 既有逻辑和一条既有测试断言）；新摘要查询没有新增 Clippy 报错。all-targets 因库测试失败未进入后续集成 targets。

依赖：LUX-593、LUX-660。

#### LUX-715：为 Emby 登录凭证管理提供用户范围撤销服务

范围：为模块七管理员 Emby 登录管理增加按凭证记录 ID 撤销单次登录和撤销该用户全部有效登录的存储操作，并由 `EmbyAuthService` 暴露有界摘要分页及撤销服务合同。撤销只能修改目标用户自己的有效凭证；原始 token 和 hash 不离开存储层。本任务不新增 HTTP API、管理页面或迁移。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/users.rs`、`src/auth/emby.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 单条撤销按用户 ID 与凭证记录 ID 联合限定；其他用户、未知 ID、已撤销项不改变数据，且重复撤销安全返回无变化。
- [x] 全部撤销只更新指定用户仍有效的凭证并返回影响行数；其他用户和既有撤销时间保持不变。
- [x] Emby 认证服务统一暴露摘要分页、单条撤销和全部撤销，接口不接收或返回 token/hash；SQLite 回归覆盖单条隔离、全量隔离与重复调用。
- [x] Rust 定向库测试、build、fmt 和 diff check 通过；不新增 migration。

短计划：先添加失败的凭证记录 ID 撤销 SQLite 回归；再实现按用户范围的单条/批量存储命令与 Emby 认证服务封装；最后运行定向库测试、build、fmt 和 diff check。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib admin_emby_login_session_revocation`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 和 `git diff --check`。

依赖：LUX-714。

结果（2026-10-20）：在 `access_tokens` 上新增按用户 ID + 凭证记录 ID 撤销单项的更新，以及撤销指定用户全部有效凭证并返回影响行数的更新；两者都只操作 `revoked_at IS NULL` 项，不读取 token/hash。`EmbyAuthService` 增加安全摘要分页、单项撤销和全部撤销方法，公开摘要仅有设备/客户端/时间元数据。

验证：TDD RED 确认服务方法缺失时测试编译失败；`admin_emby_login_session_revocation` 1/1、`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。不新增 migration；阶段完整 Rust 门禁仍按 LUX-714 所记未通过结果处理。

#### LUX-716：提供管理员 Emby 登录凭证管理 API

范围：新增管理员按用户分页读取 Emby 登录凭证摘要、撤销一条凭证和撤销该用户全部有效凭证的 HTTP API，复用 LUX-715 的 `EmbyAuthService`。分页响应只包含记录 ID、设备/客户端和时间字段；写操作要求管理员 Web session 与 CSRF，并写入不含凭证材料的审计事件。本任务不修改 Web 客户端或页面。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [x] 分页读取验证用户 ID 和账号存在性，页码至少为 1，页大小默认 50 且服务端限制 1–100；不返回 token/hash。
- [x] 单条及全部撤销仅管理员 Web session + CSRF 可执行；逐项未知/非目标凭证返回 404，成功逐项返回 204，全部撤销返回影响数。
- [x] 两种撤销写入脱敏审计；回归覆盖未认证、缺 CSRF、用户隔离、分页、凭证撤销生效及响应/审计不含 token/hash。
- [x] `users` 定向集成测试、Rust build、fmt 与 diff check 通过；同步 API 文档，不新增 migration。

短计划：先添加管理员 Emby 凭证列表及撤销流程的失败集成回归；再接入 LUX-715 服务、错误码与审计；最后同步 API 文档并运行定向集成测试、build、fmt 和 diff check。

验证：确认 `/Volumes/Toshiba` 已挂载后运行 `NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --test users admin_can_manage_emby_login_sessions -- --exact`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo build --locked`、`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo fmt --all -- --check` 和 `git diff --check`。

依赖：LUX-715、LUX-660、LUX-662。

结果（2026-10-20）：新增 `/api/v1/admin/users/{userId}/emby-sessions` 分页读取及全部撤销端点，以及 `{sessionId}` 单条撤销端点。分页固定返回安全摘要；单条和批量写入先执行管理员 Web session + CSRF 检查，目标用户及凭证范围由服务/存储双重限定。单条返回 204，全部返回 `revokedCount`；只有实际撤销时生成不含凭证材料的审计事件。API 文档已同步。

验证：TDD RED 在缺少路由时返回 404；最终 `admin_can_manage_emby_login_sessions` 1/1、`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。集成回归确认分页上限、单项/全部撤销、跨用户隔离、未认证与 CSRF 拒绝、token 失效及响应/审计不含原始 token。无 migration。

#### LUX-717：为管理员 Emby 登录会话增加 Web API 客户端合同

范围：为 LUX-716 的 Emby 登录会话分页、单项撤销和全部撤销接口增加 TypeScript DTO 与客户端方法。请求通过统一客户端处理，撤销沿用 JSON/CSRF 与错误处理边界；本任务不修改用户管理页面。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] DTO 只包含凭证记录 ID、设备/客户端与时间字段，不定义 token、hash 或密码字段。
- [x] 分页方法默认页码 1、页大小 50，使用 URL 编码的用户 ID 与 `page`/`pageSize` 查询参数。
- [x] 单项及全部撤销使用精确 DELETE 路径和统一 CSRF 请求层；全部撤销类型包含 `revokedCount`。
- [x] API client 定向 Vitest、Web build 和 diff check 通过。

短计划：先为分页、单项撤销和全部撤销增加精确路径、响应及 CSRF 合同测试并确认失败；再增加 DTO 与客户端方法；最后运行定向 Vitest、Web build 和 diff check。

验证：`pnpm --dir web exec vitest run tests/api-client.test.ts -t "admin user Emby session"`、`pnpm --dir web build` 和 `git diff --check`。

依赖：LUX-716。

结果（2026-10-20）：新增 Emby 登录摘要分页、单项撤销和全部撤销的 TypeScript DTO 与客户端方法，复用统一请求层处理 URL 编码、CSRF 与 API 错误。响应 DTO 仅暴露设备、客户端及时间元数据。

验证：TDD RED 确认新增客户端回归在方法缺失时失败；`tests/api-client.test.ts -t "admin user Emby session"` 1/1、`pnpm --dir web build` 和 `git diff --check` 通过。

#### LUX-718：在用户设备与登录页管理 Emby 登录凭证

范围：在现有用户“设备与登录”区域统一展示 Web 登录会话与 Emby 登录凭证。Emby 区域展示 API 提供的设备/客户端/时间安全摘要，支持分页、确认后撤销单项及撤销该用户全部有效凭证；提供独立加载、错误、重试、空结果和操作反馈。本任务不新增设备/IP/客户端黑名单、IP 来源推断、活动记录、真实 IP 或当前在线状态。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/src/features/admin/user-management.css`、`web/tests/admin-user-sessions.test.tsx`。

验收标准：

- [x] 页面同时显示 Web 与 Emby 登录摘要；Emby 列表只显示服务端返回的设备、客户端和时间元数据，不暴露凭证材料，也不虚构 IP/在线状态。
- [x] Emby 登录列表有界分页；页内最后一项被撤销时回退页码；撤销单项与全部撤销前均要求确认。
- [x] 列表读取失败可重试；撤销失败显示错误并允许再次尝试，成功展示反馈；批量撤销展示服务端实际撤销数量并刷新列表。
- [x] 定向 session 管理 Vitest、Web build、桌面与窄屏浏览器交互验证及 diff check 通过。

短计划：先在现有会话页面测试中覆盖 Emby 摘要、分页及单项/全部撤销并确认失败；再加独立 Emby 查询与撤销面板，复用当前会话表格风格；最后运行定向 Vitest、Web build、浏览器交互验证和 diff check。

验证：`pnpm --dir web exec vitest run tests/admin-user-sessions.test.tsx`、`pnpm --dir web build`、浏览器桌面及 390×844 视口验证、`git diff --check`。

依赖：LUX-717、LUX-716。

结果（2026-10-20）：在“设备与登录”页分别展示 Web 会话与 Emby 登录凭证。Emby 面板提供设备 ID/类型、客户端/版本、首次登录、最近活动和每用户 20 条分页；最后一项撤销后回到前页。单项和全部撤销均在确认后调用 API，成功后刷新会话列表并按服务端实际数量反馈；读取失败可重试，撤销失败保留凭证并显示错误。页面不呈现 IP、归属地或推断的在线状态。

验证：测试文件 admin-user-sessions.test.tsx 为 9/9，pnpm --dir web build 和 git diff --check 通过。Playwright 使用本地模拟 API 验证桌面 1200px 页面及全部撤销确认/DELETE/结果提示；390×844 CSS 视口下页面宽 375px，无页面级横向溢出，两张 760px 表格均在各自 310px 横向滚动区内。控制台 0 错误。截图：output/playwright/user-emby-sessions-desktop.png、output/playwright/user-emby-sessions-390.png。浏览器检查为本地模拟 API，不代表真实 FNOS 或生产环境验证。

#### LUX-719：建立用户活动有界索引表

范围：为模块七新增用户活动索引的 SQLite 与 PostgreSQL 迁移，保存活动所属用户、可选操作者、事件类型、目标引用、脱敏元数据 JSON 和发生时间。索引支持按用户/时间及用户/事件类型/时间倒序查询，并支持按发生时间清理过期索引。活动用户 ID 不加用户外键，避免账号彻底删除时绕过设计规定的活动保留期。本任务只建立 schema，不写入活动、不提供 API 或页面，也不回填旧归档。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0193_user_activity_events.sql`、`migrations-postgres/0193_user_activity_events.sql`、`tests/storage.rs`。

验收标准：

- [x] 空 SQLite 数据库迁移后存在用户活动索引表、用户/时间索引、用户/类型/时间索引和保留期清理索引。
- [x] SQLite 与 PostgreSQL schema 字段及约束语义一致，支持无操作者/目标的登录等事件。
- [x] 活动记录不依赖用户外键；新增迁移不改变现有表或历史记录。
- [x] storage 定向迁移测试与 diff check 通过。

短计划：先写空数据库迁移回归并确认索引表缺失而失败；再新增两种后端迁移及必要索引；最后运行指定 storage 回归和 diff check。

结果（2026-10-20）：新增 `user_activity_events` 索引表，保存活动用户、可选操作者、类型、目标引用、元数据 JSON 和 Unix 时间；以三组索引支持用户时间线倒序分页、按类型筛选和保留期清理。活动用户 ID 不设外键，允许账号清理后索引按保留策略独立过期。没有回填归档或接入写入路径。

验证：TDD RED 在空库中确认索引表缺失（0 行）；迁移后 `user_activity_events_migration_runs_from_an_empty_database` 1/1，通过字段/索引/外键/无操作者和目标插入检查。PostgreSQL 迁移做了结构合同检查，未连接真实 PostgreSQL 实例。`cargo fmt --all -- --check` 和 `git diff --check` 通过。

依赖：LUX-718。

#### LUX-720：写入脱敏的用户活动索引

范围：为用户活动索引增加存储写入合同，生成 UUIDv7 唯一记录 ID，并写入活动用户、可选操作者、事件类型、目标引用、元数据和发生时间。元数据必须在落库前复用日志敏感值脱敏规则；非法 JSON 以安全占位对象保存。允许活动记录在用户 ID 暂不可解析或账号已清理时独立保留。本任务不接入现有登录/播放事件写入点，不增加查询 API 或回填。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 存储方法生成并返回唯一活动 ID，保存用户、操作者、类型、目标和默认时间；不要求用户外键存在。
- [x] 嵌套敏感键/完整 URL 在落库前按现有规则脱敏；非法 JSON 不原样保存。
- [x] 存储回归读取数据库实值验证字段、脱敏、默认时间和不同调用的 ID 唯一。
- [x] 定向 storage 回归、build、fmt 和 diff check 通过。

短计划：先加存储回归并确认写入方法缺失时失败；再补存储 DTO、导出及脱敏写入；最后跑指定 repository 测试、build、fmt 和 diff check。

结果（2026-10-20）：新增内部 `NewUserActivityEvent` 与 `insert_user_activity_event`，使用 UUIDv7 作为记录 ID，数据库默认时间，并通过既有敏感值脱敏逻辑规范化元数据。非法 JSON 只保存 `{"invalid":true}`；用户/操作者 ID 不需要在 users 表中存在。接口暂由 LUX-721 接入活动写入流程。

验证：TDD RED 因存储类型/方法不存在而编译失败；`user_activity_index_insert_redacts_metadata_and_keeps_independent_subjects` 1/1，通过对数据库实值检查用户、操作者、事件目标、默认时间、UUID 唯一性、嵌套凭证/URL 脱敏及非法 JSON 替换。`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check` 通过；未使用警告由下一任务接入标注消除。

依赖：LUX-719。

#### LUX-721：接入登录与播放活动事件索引

范围：将现有 `record_activity_event` 记录的成功认证和播放状态变化写入用户活动索引，活动用户与操作者标识当前发起事件的账号。保留已有文件审计日志和 Dashboard 失效通知；索引写入失败不得丢弃已有审计日志，不记录错误正文、凭证或完整 URL。本任务不记录认证失败，不增加活动查询 API 或页面。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/api/admin/admin_logs_events.rs`、`tests/users.rs`、`tests/web_playback.rs`。

验收标准：

- [x] Web 成功登录及 Web 播放状态变化写入对应用户的 `AUTH_LOGIN`/`PLAYBACK_*` 索引事件，actor 与活动用户一致。
- [x] 索引写入复用已有脱敏存储合同；已有文件审计记录及 Dashboard 通知继续正常。
- [x] 定向认证与播放集成回归和原用户 API 回归、build、fmt、diff check 通过。

短计划：先为成功登录增加 API 回归并确认索引无记录；再在统一活动记录器写入索引并保留文件审计；最后跑用户/播放 API 定向回归、build、fmt 和 diff check。

结果（2026-10-20）：统一 `record_activity_event` 现在先将活动写入用户索引，再追加原文件审计记录；索引写入失败只发出不含用户/请求内容的事件类型告警，不阻断文件日志或 dashboard 通知。现有 Web/Emby 成功认证、Emby/Web/legacy 播放状态调用点因此共用此索引路径。

验证：TDD RED 的成功登录 API 已写入用户但索引数为 0；接线后 `successful_login_is_written_to_the_user_activity_index` 1/1。`web_playback_uses_signed_direct_urls_and_monotonic_events` 1/1 验证 `PLAYBACK_STARTED` 索引及原文件审计；`admin_can_manage_users_and_last_manager_is_protected` 1/1、`activity_events_publish_dashboard_invalidations` 1/1。`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check` 通过。登录失败仍未入索引，筛选/分页 API 与页面留待后续任务。

依赖：LUX-720。

#### LUX-722：提供用户活动索引的过滤分页存储查询

范围：为模块七活动时间线增加按固定用户范围读取用户活动索引的存储合同，支持可选事件类型和起止 Unix 时间筛选、稳定的时间/ID 倒序、匹配总数和有界分页。页大小强制限制为 1–100，负偏移归零；越界页仍返回正确总数。只读取索引表，不扫描文件审计归档，不加载未分页用户事件。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 用户隔离及事件类型/时间边界筛选在分页前执行，匹配总数准确。
- [x] 按 `occurred_at DESC, id DESC` 稳定排序；相同时间跨页不重复、不遗漏。
- [x] 页大小 1–100、负偏移归零；越界页返回空项和准确总数。
- [x] 定向 storage 回归、build、fmt 和 diff check 通过。

短计划：先增加过滤/排序/分页存储回归并确认查询合同缺失而失败；再补查询 DTO、存储方法和导出；最后跑定向 repository 测试、build、fmt 和 diff check。

结果（2026-10-20）：新增 `UserActivityEventQuery` 与按用户、事件类型、起止时间过滤的索引表查询。匹配总数独立计算，保证越界页仍返回准确总数；结果按 `occurred_at DESC, id DESC` 稳定排序，限制页大小 1–100 并将负偏移归零。不读取用户归档日志。

验证：TDD RED 因查询类型/方法不存在而编译失败；`user_activity_index_query_filters_and_pages_in_stable_order` 1/1，覆盖用户隔离、含边界时间过滤、事件类型、同时间跨页、稳定 ID 顺序、负偏移、最大页大小及越界页总数。`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。

依赖：LUX-721。

#### LUX-723：开放管理员用户活动分页 API

范围：新增只读 `GET /api/v1/admin/users/{userId}/activity`，从 LUX-722 的用户范围存储查询返回脱敏活动时间线。查询参数为 `page`、`pageSize`、`eventType`、`from`、`to`；页码缺省 1 且最小为 1，页大小缺省 20 并限制为 1–100，时间以包含端点的 Unix 秒表示。响应为 `{ events, page, pageSize, totalCount }`；活动项包含 `id`、`userId`、`actorUserId`、`eventType`、目标引用、已脱敏 `metadata` 和 `occurredAt`。仅管理员可读；无效用户 ID、负时间、反向时间范围、空/超长事件类型返回结构化 400；用户不存在返回 404。不得返回原始未脱敏 JSON。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [x] 接口认证管理员、校验用户 ID/存在性及查询边界；非管理员不能读取。
- [x] 响应与存储总数、稳定分页一致，元数据为已脱敏 JSON 对象。
- [x] 回归覆盖用户隔离、时间端点、类型筛选、跨页排序、非法参数、缺失用户和未授权请求。
- [x] 更新 API 文档；`users` 定向集成回归、build、fmt 和 diff check 通过。

短计划：先写 API 登录后读取及过滤分页集成回归并确认路由缺失而失败；再接入 LUX-722 存储查询和结构化错误映射；最后更新 API 文档并运行定向用户测试、build、fmt 和 diff check。

结果（2026-10-20）：新增管理员只读用户活动时间线 API，认证检查后校验用户 ID、账号存在状态、Unix 秒区间和事件类型；默认页为 1、每页 20，服务端限制页大小 1–100。响应提供活动/操作者、目标、发生时间及经过再次脱敏的 JSON 元数据；未知或损坏元数据不会以原文返回。已同步 `docs/API.md`。

验证：TDD RED 在登录后的有效管理员请求上确认新路由返回 404；最终 `admin_can_filter_and_page_user_activity_events` 1/1，覆盖管理员分页、用户隔离、含端点时间过滤、类型筛选、同时间跨页排序、普通用户 403、匿名 401、无效用户/时间范围/事件类型 400、缺失用户 404。`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。

依赖：LUX-722、LUX-660。

#### LUX-724：增加管理员用户活动时间线 Web API 客户端合同

范围：为 LUX-723 的用户活动分页 API 增加 TypeScript 活动项与分页响应类型，以及按用户读取活动的方法。客户端支持页码、页大小、事件类型和包含端点的起止 Unix 秒筛选，查询参数必须正确编码；读取沿用统一 GET 请求层。本任务不修改用户管理页面。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] 活动项类型覆盖 API 的活动 ID、用户/操作者、事件类型、目标引用、脱敏元数据和发生时间；不包含凭证或原始 JSON 字段。
- [x] 分页响应包含活动列表、页码、页大小和匹配总数；客户端默认第 1 页、每页 20 条。
- [x] 可选事件类型与时间范围经 `URLSearchParams` 编码，页码筛选准确进入 URL；读取使用 GET，不带写操作 CSRF。
- [x] 定向 API client Vitest、Web build 和 diff check 通过。

短计划：先为时间线响应、默认分页、事件类型/时间筛选、用户 ID 与查询值编码增加失败合同测试；再加入 DTO 和客户端方法；最后运行定向 Vitest、Web build 和 diff check。

依赖：LUX-723。

结果（2026-10-20）：增加 `AdminUserActivityEvent`、分页响应与筛选查询类型，以及 `adminUserActivity` GET 方法。方法默认第 1 页、每页 20 条，通过 `URLSearchParams` 编码事件类型和 Unix 时间边界，并复用用户 ID 路径编码。活动 DTO 不含任何凭证字段。

验证：先添加合同测试并确认因客户端方法缺失而失败；实现后 `pnpm --dir web exec vitest run tests/api-client.test.ts -t "admin user activity"` 1/1、`pnpm --dir web build` 和 `git diff --check` 通过。测试覆盖过滤值、用户 ID 编码、默认分页和 GET 请求方式。

#### LUX-725：在用户详情展示活动时间线

范围：将模块七活动记录页接入 LUX-723 API 和 LUX-724 客户端合同，显示按时间倒序返回的活动、事件类型与时间筛选以及有界分页。活动条目只展示脱敏元数据中的已知设备、客户端、状态和 IP 字段，不渲染原始 JSON。提供读取、错误重试、空结果和筛选校验状态；本任务不补活动事件生产或设备管理规则。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/src/features/admin/user-management.css`、`web/tests/admin-user-activity.test.tsx`。

验收标准：

- [x] 用户详情的“活动记录”页读取该用户活动，不读取其他用户或归档日志。
- [x] 支持按事件类型、起止本地时间筛选；反向时间范围在提交前提示；更改筛选回到第一页。
- [x] 展示活动类型、发生时间、可用目标和安全元数据；凭证/原始 JSON 不进入页面。
- [x] 具备加载、错误重试、空结果、总数和上一页/下一页；分页使用服务端总数。
- [x] 组件定向 Vitest、Web build 和 diff check 通过。

短计划：先为详情页加载活动、筛选分页及错误/空状态增加组件回归并确认现页仍是占位；再增加时间线查询与安全元数据展示、样式；最后运行定向 Vitest、Web build 和 diff check。

依赖：LUX-723、LUX-724。

结果（2026-10-20）：用户详情的活动页接入活动索引分页 API，提供事件类型、本地起止日期时间筛选、清除筛选、服务端总数和分页。结束时间将分钟边界扩至该分钟的第 59 秒，保持用户选择时间粒度下的包含端点语义；反向/无效范围提交前拒绝。列表白名单展示设备、客户端、版本、播放状态和 IP，不渲染原始 JSON 或未知元数据字段。

验证：TDD RED 的组件回归确认该页仍是占位；实现后 `pnpm --dir web exec vitest run tests/admin-user-activity.test.tsx tests/admin-user-sessions.test.tsx` 11/11、`pnpm --dir web build` 和 `git diff --check` 通过。Playwright 使用合成 API 数据验证活动读取、筛选查询、翻页、敏感字段不显示；390px 视口的活动面板/筛选区无水平溢出，浏览器控制台无错误。截图保存于 `output/playwright/admin-user-activity-20261020-mobile.png`。

#### LUX-726：增加失败认证活动安全记录器

范围：为已知用户提供失败认证活动写入入口，记录 `AUTH_LOGIN_FAILED` 用户活动并沿用既有审计日志和 Dashboard 失效通知。事件使用目标用户 ID，`actorUserId` 为空；调用者只传入固定协议标签和可选展示 IP，不传用户名、密码、token 或请求 URL。本任务只实现可安全调用的记录器，不接入 Web 或 Emby 认证分支。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`src/api/admin/admin_logs_events.rs`。

验收标准：

- [x] 记录器写入 `AUTH_LOGIN_FAILED` 用户活动和既有审计事件，活动用户/目标为指定用户且 actor 为空。
- [x] 元数据只含调用者提供的安全协议标签与可选 IP；没有用户名、密码、凭证或 URL。
- [x] 审计写入成功时发布 Dashboard 失效通知；索引失败不阻断既有审计流程。
- [x] 记录器定向回归、Rust build、fmt 和 diff check 通过。

短计划：先增加失败事件记录器的 SQLite 与审计/通知回归并确认记录器缺失；再实现 actor 为空的安全写入；最后运行定向库测试、build、fmt 和 diff check。

依赖：LUX-720。

结果（2026-10-20）：新增 `record_failed_authentication_event`，用户活动事件和既有审计事件都以 `AUTH_LOGIN_FAILED` 记录，actor 为空、目标指向被尝试账号；元数据只含固定协议标签和可选展示 IP。索引写入失败仅发出事件类型告警，不阻断审计写入；审计成功后发布 Dashboard 失效通知。

验证：TDD RED 确认记录器缺失时库测试编译失败；最终 `failed_authentication_activity_keeps_actor_empty_and_metadata_safe` 1/1，验证活动字段、actor、白名单元数据与 Dashboard 通知；`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。Web 登录路由接线留待 LUX-727。

#### LUX-727：索引已知账号的 Web 登录失败

范围：在未被限流的 Web 登录尝试认证失败后，使用与登录一致的用户名规范化规则查找已有账号；只对解析到的用户调用 LUX-726 记录器。事件不改变认证响应和频率限制行为；未知/非法用户名、数据库错误及被限流直接拒绝的请求不进入个人活动索引。本任务不记录 Emby 失败登录。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/auth/sessions.rs`、`src/api/admin.rs`、`src/api/users.rs`、`tests/users.rs`。

验收标准：

- [x] 已知账号密码错误或账号被禁用时记录 `AUTH_LOGIN_FAILED`，活动用户为目标账号且 actor 为空。
- [x] 未知/非法用户名、数据库错误和限流直接拒绝不写入用户活动索引；失败元数据不含用户名、密码、凭证或完整 URL。
- [x] 认证 HTTP 状态、频率限制与成功登录事件保持不变；失败审计记录仍产生。
- [x] `users` 定向集成回归、Rust build、fmt 和 diff check 通过。

短计划：先添加已知/未知账号失败尝试集成回归并确认个人活动缺失；再在 Web 认证服务提供安全用户名到用户 ID 查询，并仅在正确失败分支接入 LUX-726；最后运行定向 `users` 测试、build、fmt 和 diff check。

依赖：LUX-726、LUX-720。

结果（2026-10-20）：`WebAuthService` 增加沿用认证用户名规范化规则的账号 ID 查询；Web 登录在认证与可选迁移回退均结束为失败、且该请求未被限流时，仅为已有账号调用 LUX-726 安全记录器。禁用账号也会记录；未知/非法账号、认证数据库错误、迁移认证错误及入口限流不写个人失败活动。登录状态码、速率限制、成功登录记录维持现状。

验证：TDD RED 的集成回归先以 0 条活动失败（预期 6 条）；实现后 `cargo test --locked --test users` 16/16、`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。`cargo clippy --locked --test users --all-features -- -D warnings` 被现有 6 项未修改代码告警阻断，位置为 `src/api/admin/admin_users.rs`、`src/api/media.rs`、`src/application/playback/session.rs`、`src/application/user_permission_copy.rs` 和 `src/storage/users.rs`；本任务未改动这些文件。

#### LUX-728：为 Emby 失败登录解析已知账号

范围：为 Emby 按用户名认证提供与成功认证相同的目标账号解析，包括规范化用户名和唯一精确显示名称回退；使用有界查询，避免登录请求枚举全部用户。只公开内部用户 ID 查询，不改变认证响应或用户公开数据。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/auth/users.rs`、`src/auth/emby.rs`、`src/storage/users.rs`、`tests/users.rs`。

验收标准：

- [x] 规范化用户名和唯一精确显示名称解析到成功认证对应的用户；重复显示名称、未知及非法名称不解析账号。
- [x] 显示名称查询最多读取两个候选，不枚举用户列表；数据库错误以现有存储错误类型返回。
- [x] `users` 定向回归、Rust build、fmt 和 diff check 通过。

短计划：先增加服务层目标 ID 回归并确认方法缺失；再复用现有 Emby 用户名/显示名称合同，实现最多读取两个候选的内部解析；最后运行 `users` 集成目标、build、fmt 和 diff check。

依赖：LUX-727。

结果（2026-10-20）：Emby 登录目标解析统一使用规范化账号名及唯一精确显示名称回退。显示名称回退改为 storage 层 `LIMIT 2` 查询：无候选或有歧义均拒绝目标，不再在登录请求里枚举所有账号。内部认证服务可返回目标用户 ID，供失败活动索引复用。

验证：TDD RED 集成回归因 `find_failed_authentication_user_id` 不存在而失败；实现后 `cargo test --locked --test users emby_authentication_target_lookup_matches_login_name_rules` 1/1、`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。既有认证调用点已复用相同解析函数。

#### LUX-729：记录 Emby 已知账号登录失败

范围：在 Emby 按用户名和按用户 ID 登录均被允许尝试且认证失败后，使用 LUX-728 的目标账号解析调用 LUX-726 安全记录器。未知/非法账号、数据库或迁移错误以及入口限流直接拒绝不记录；保持 Emby 状态码、响应体和频率限制合同，不记录客户端凭证或请求内容。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/api/emby_handlers.rs`、`tests/users.rs`。

验收标准：

- [x] 已知用户名或用户 ID 的密码错误、禁用账号写入 `AUTH_LOGIN_FAILED`，actor 为空并关联目标用户。
- [x] 未知/非法目标、认证错误和限流直接拒绝不写用户活动；失败元数据不含用户名、密码、token 或完整 URL。
- [x] Emby 失败 HTTP 合同、速率限制和成功认证事件不变，既有审计记录仍产生。
- [x] `users` 定向回归、Rust build、fmt 和 diff check 通过。

短计划：先为 Emby 用户名/ID 认证失败及限流写集成回归并确认活动缺失；再仅在认证返回明确失败的分支查询已知目标并接入记录器；最后运行 `users` 集成目标、build、fmt 和 diff check。

依赖：LUX-726、LUX-728。

结果（2026-10-20）：Emby `AuthenticateByName` 和按用户 ID 认证在入口未被限流、认证明确返回失败后，分别用 LUX-728 的相同账号解析和安全 ID 查询定位现有账号并写 `AUTH_LOGIN_FAILED`。固定客户端标记为 `LUX_EMBY`，actor 为空；认证错误、未知/非法目标及入口限流不产生个人失败活动。HTTP 状态与响应体保持既有合同。

验证：TDD RED 集成回归先得到 0 条失败事件（预期 7 条）；实现后 `cargo test --locked --test users` 18/18、`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。回归覆盖用户名、唯一显示名称、已知用户 ID、禁用账号、未知/非法目标、速率限制，以及活动和审计元数据不包含账号名或凭证。

#### LUX-730：提供有界用户活动过期清理

范围：在 storage 层按发生时间删除早于明确截止时间的用户活动索引，单次最多处理一个固定上限批次，并按时间和 ID 稳定选择。保留截止时间及之后的事件，不触及文件审计、观看进度、已看状态或收藏；本任务只提供清理合同，不启动 worker 或更改保留设置。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 清理只删除 `occurred_at` 早于截止时间的用户活动记录，边界时间及较新事件保留。
- [x] 每次调用有界且安全处理空结果、非法/超大批次；现有进度、收藏和文件审计数据不变。
- [x] storage 库定向回归、Rust build、fmt 和 diff check 通过。

短计划：先写过期与边界时间的 storage 回归并确认清理方法缺失；再用时间/ID 索引执行最多 1,000 条的参数化批次删除；最后运行定向库测试、build、fmt 和 diff check。

依赖：LUX-719、LUX-720。

结果（2026-10-20）：storage 新增 `delete_user_activity_events_before`，使用 `occurred_at, id` 稳定选择最多 1,000 条过期事件并通过单条参数化删除；截止点采用严格早于语义，非正批次返回 0。数据存储层不修改 file audit、播放/已看/收藏数据；worker 接线由后续任务完成。

验证：TDD RED 库回归因清理方法不存在而失败；实现后 `cargo test --locked --lib user_activity_index_retention_deletes_old_events_in_bounded_batches` 1/1、`cargo build --locked`（无新增 warning）、`cargo fmt --all -- --check` 和 `git diff --check` 通过。

#### LUX-731：持久化用户活动保留天数

范围：在现有 `server_settings` 设置键中保存用户活动索引保留天数，默认 90 天；有效范围为 1–3650 天。损坏或旧配置读取为默认值，非法写入拒绝且不覆盖原值。本任务只提供 storage 读写，不增加管理 API、页面或清理 worker。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 空配置默认读取 90 天；有效值持久化并可重读，损坏/越界旧值安全回退至 90 天。
- [x] 新设置写入拒绝 0、负数和大于 3650 的值，拒绝时保留原设置；不新增 migration。
- [x] 定向 storage 库回归、Rust build、fmt 和 diff check 通过。

短计划：先增加默认、有效与非法值存储回归并确认接口缺失；再用 `server_settings` 新键实现有界读取与校验写入；最后运行定向库测试、build、fmt 和 diff check。

依赖：LUX-730。

结果（2026-10-20）：storage 通过 `server_settings` 的 `user_activity_retention_days` 键持久化天数，默认 90、范围 1–3650；非法旧配置回退默认值，非法新值在写入前拒绝。没有新增 migration。管理 API 接线由 LUX-732 完成。

验证：TDD RED 回归先因读写方法缺失而编译失败；实现后 `cargo test --locked --lib user_activity_retention_setting_defaults_validates_and_recovers_invalid_values` 1/1、`cargo build --locked`（无新增 warning）、`cargo fmt --all -- --check` 和 `git diff --check` 通过。

#### LUX-732：接入活动记录保留期管理 API

范围：在管理员全局设置读写合同中暴露用户活动索引保留天数，沿用 storage 默认值 90 和 1–3650 的校验；读取旧配置并返回 camelCase 字段，PATCH 可独立更新该值且拒绝非法输入。本任务只增加应用/API 合同，不增加 worker 或页面。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/settings.rs`、`src/api/admin/admin_settings.rs`、`tests/admin_dashboard.rs`。

验收标准：

- [x] 管理员设置 GET 返回 `userActivityRetentionDays`，缺省为 90。
- [x] PATCH 可单独保存有效保留天数；拒绝 0、负数和大于 3650 的输入，非法值不覆盖已有值。
- [x] 非管理员不能通过该合同更改设置；`admin_dashboard` 定向集成回归、Rust build、fmt 和 diff check 通过。

短计划：先扩展现有管理员设置集成回归并确认字段缺失；再贯通 settings snapshot/update/write 与 HTTP JSON；最后运行 `admin_dashboard` 集成目标、build、fmt 和 diff check。

依赖：LUX-731。

结果（2026-10-20）：管理员 settings snapshot、PATCH 更新和 HTTP JSON 已新增 `userActivityRetentionDays`，使用 LUX-731 storage 读写并保持未提交字段时原值不变。输入范围为 1–3650；非法设置返回 400，未认证请求仍返回 401。无 migration。

验证：TDD RED 集成回归先因 GET 缺少字段而失败；实现后 `NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost cargo test --locked --test admin_dashboard` 1/1、`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。

#### LUX-733：清理过期的用户活动索引

范围：将 LUX-730 的有界删除接入现有用户清理后台循环，每次循环读取可配置保留天数并清理最多 1000 条严格早于截止时间的用户活动索引。失败只记录不含用户隐私的错误并由后续循环重试；不删除文件审计、播放进度、已看状态或收藏。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/user_cleanup.rs`。

验收标准：

- [x] 后台循环按当前 1–3650 天配置计算截止时间，并逐次执行最多 1000 条的删除批次。
- [x] 截止时间边界及更新活动保留，活动清理错误不会终止用户清理 worker，后续循环可以继续。
- [x] worker 定向库回归、Rust build、fmt 和 diff check 通过；活动清理不触及其它用户数据。

短计划：先为清理批次写默认时间与配置边界的 worker 回归；再把一批过期活动清理接到现有后台循环；最后运行定向库测试、build、fmt 和 diff check。

依赖：LUX-730、LUX-731、LUX-732。

结果（2026-10-20）：现有每分钟用户清理循环读取当前活动保留设置，按 Unix 秒计算截止点并最多删除 1,000 条严格早于截止点的索引。失败只产生不带用户字段的错误日志，不影响 worker 后续循环；不会访问或删除 file audit、播放进度、已看状态及收藏。

验证：`cargo test --locked --lib configured_activity_retention_prunes_one_bounded_batch_and_keeps_boundary` 1/1，覆盖 1,001 条过期活动分批清理和截止边界保留；`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。

#### LUX-734：管理界面配置活动记录保留期

范围：将用户管理设置页“活动记录保留”占位改为真实表单，读取并保存管理员 settings API 的 `userActivityRetentionDays`。允许 1–3650 天，提供正在读取、保存、校验、成功与错误状态；保存只更新该保留字段。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/tests/admin-user-retention-settings.test.tsx`。

验收标准：

- [x] 页面展示当前活动记录保留期，默认 90 天并允许输入 1–3650 天。
- [x] 保存通过 `updateAdminSettings` 只提交 `userActivityRetentionDays`，更新缓存并展示成功/失败状态。
- [x] 无效输入或无变更时不能保存；定向 Vitest、Web build、锁文件安装和用户设置 Playwright 验证通过。
- [ ] `pnpm --dir web test` 全量通过；当前有 4 个范围外失败，见下方验证记录。

短计划：先扩展现有删除保留页面测试并确认活动保留占位缺失；再加入活动保留表单及 API 类型；最后运行定向 Vitest、Web build、Web tests 和浏览器流程验证。

依赖：LUX-732、LUX-733。

结果（2026-10-20）：活动保留占位替换为服务端设置表单。默认 90 天，范围 1–3650；无效值和无变更时禁用保存，成功后更新查询缓存并显示状态，错误显示可读提示。删除账号保留表单保持原行为。

验证：TDD RED 测试先因活动保留输入框不存在而失败；实现后 `pnpm --dir web exec vitest run tests/admin-user-retention-settings.test.tsx` 5/5、`pnpm --dir web install --frozen-lockfile`、`pnpm --dir web build` 通过。Playwright 使用临时 SQLite 管理员会话验证保存 30 天后刷新仍读回 30 天；390px 视口 `scrollWidth` 为 390，控件可访问名称完整。交互后无新增 console error；登录前的 `auth/me` 401 是预期未登录检查。

全量 `pnpm --dir web test` 的 148 个 Node 测试通过，Vitest 为 734 通过、4 失败。失败是 `home-page.test.tsx` 的首页预取 1 项、`library-card.test.tsx` 两项和 `media-query-polling.test.tsx` 1 项；单独重跑这三个目标仍复现，相关代码不在本任务修改范围内。

#### LUX-735：提供有界用户当前播放读取

范围：为模块七用户详情当前播放管理提供指定用户的有界存储查询。仅读取播放记录中最近 90 秒内且尚未停止的会话，支持服务端总数与稳定倒序分页；每页限制 1–100 条，负偏移归零。只增加 storage 合同，不接管理员 API、停止操作或 Web 页面，不新增 migration。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/sessions.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 查询只返回指定用户最近活动且未停止的播放会话，按最近活动倒序并以 ID 稳定排序；其他用户、过期和已停止会话不返回。
- [x] 返回的匹配总数不受分页影响；页大小强制限制为 1–100，偏移非负，查询结果有界。
- [x] storage 定向回归、Rust build、fmt 与 diff check 通过；无 migration。

短计划：先添加 SQLite storage 回归并确认分页方法缺失；再实现匹配总数和限量查询；最后运行定向库测试、build、fmt 与 diff check。API、HLS 停止和界面留给后续 LUX 任务。

#### LUX-735 验证记录

结果（2026-10-20）：新增指定用户当前播放 storage 查询，沿用后台播放卡片的 90 秒活跃窗口，排除已停止记录；结果按最近事件倒序、ID 正序分页，并在同一筛选条件下返回总数。页大小限制为 1–100，负偏移归零；未新增 migration。LUX-736 将接入管理员 API。

验证：TDD RED 回归先因分页查询方法缺失而编译失败；实现后 `user_current_playback_sessions_are_filtered_and_paginated` 1/1，覆盖用户隔离、90 秒边界、停止记录过滤、排序、总数、分页和 100 条上限。`cargo build --locked`、`cargo fmt --all -- --check` 与 `git diff --check` 通过。全目标 Clippy 门禁仍被 9 项未改动代码告警阻断，涉及 `src/api/admin/admin_users.rs:554`、`src/api/media.rs:2837`、`src/application/playback/session.rs:397`、`src/application/user_permission_copy.rs:382,657`、`src/storage/users.rs:3161,3164` 及 `src/storage/repository_tests.rs:15587,23380`。

#### LUX-736：开放管理员用户当前播放读取与 HLS 停止 API

范围：将 LUX-735 的有界分页查询开放给用户详情管理 API，并复用管理仪表盘的媒体摘要。管理员可按用户分页查看最近 90 秒的播放会话；服务端仅允许停止与当前用户播放记录关联的 `SERVER_HLS` 会话。直放和外部 `.strm` 播放不可由 Lux 保证立即中断，停止请求返回冲突且不改变其会话。本任务不增加管理页面、黑名单或 migration。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/application/admin_dashboard.rs`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [x] 管理员读取 API 返回有界分页、总数及媒体/设备/播放摘要；用户隔离、页大小默认 20 且最大 100，未认证和非管理员被拒绝。
- [x] 管理员停止 API 需要 Web session 与 CSRF，只停止目标用户仍活跃的服务端 HLS，并释放相关会话/准入；审计记录不含凭证。
- [x] 直放或外部播放停止返回稳定 `409 INVALID_REQUEST`，不会改变播放记录；用户不存在和其他用户会话不泄露信息。
- [x] `tests/users` 定向回归、Rust build、fmt 与 diff check 通过，API 文档记录路由和响应。

短计划：先在 `tests/users` 增加管理员权限、分页摘要、CSRF、HLS 停止和直放拒绝回归；再复用仪表盘摘要服务并接入两个管理 API；最后运行 `users` 定向目标、build、fmt 与 diff check。

#### LUX-736 验证记录

结果（2026-10-20）：新增用户当前播放分页读取和单会话停止 API。读取复用后台 Dashboard 的媒体/剧集摘要，返回进度、客户端、设备、来源 IP、播放计划及 `canStop`；仅活动服务端 HLS 标记可停止。停止接口要求管理员会话和 CSRF，验证播放记录归属、活动 HLS 关联后停止 HLS、更新播放记录并释放并发准入，最后写不含凭证的 `USER_PLAYBACK_STOPPED` 审计事件。直放和外部播放返回 409 `INVALID_REQUEST`，不改播放状态。`docs/API.md` 已登记两条路由；无 migration。

验证：TDD RED 集成回归因新路由返回 404（预期未认证 401）而失败；最终串行运行 `tests/users -- --test-threads=1` 为 19/19，`tests/admin_dashboard` 为 1/1。一次并行运行 `tests/users` 为 18/19，其中既有 `admin_can_manage_users_and_last_manager_is_protected` 在并行时返回 503 而非预期 200；该测试单独重跑通过，具体原因尚未确认。新增回归覆盖分页顺序/总数、媒体摘要、`canStop`、未认证/非管理员、CSRF、目标用户隔离、直放拒绝、HLS 停止、并发准入释放和审计事件。`cargo build --locked`、`cargo fmt --all -- --check` 与 `git diff --check` 通过。全目标 Clippy 仍被 9 项未改动代码错误阻断，位置与 LUX-735 记录相同；本任务涉及的 API 与应用文件未新增 Clippy 诊断。

#### LUX-737：增加用户当前播放 Web API 客户端合同

范围：为 LUX-736 的指定用户当前播放分页读取与单会话停止 API 增加安全 TypeScript DTO 和客户端方法。读取默认每页 20 条，路径参数须 URL 编码；停止复用统一请求层并携带 CSRF。本任务不修改用户详情 UI、设备清单或黑名单。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] DTO 包含用户当前播放 API 提供的媒体、播放、设备/IP 摘要和 `canStop`，不定义 token、Cookie 或凭证字段。
- [x] 分页方法默认页码 1、页大小 20，正确编码用户 ID 并发送 `page`/`pageSize`。
- [x] 停止方法使用精确 DELETE 路径与统一 CSRF 请求层。
- [x] API client 定向 Vitest、Web build 和 diff check 通过。

短计划：先在 `api-client.test.ts` 增加分页/默认参数、路径编码、停止请求和 CSRF 回归并确认方法缺失时失败；再增加 DTO 与 API client 方法；最后运行定向 Vitest、Web build 和 diff check。

结果（2026-10-20）：新增只含管理播放摘要与 `canStop` 的 `AdminUserPlaybackSession`/分页 DTO，及管理员当前播放分页读取和停止客户端方法。用户 ID、播放 ID 均按路径段编码；分页默认 1/20；停止通过统一请求层 DELETE 并自动携带 CSRF。未新增凭证字段。

验证：TDD RED 回归先因客户端方法缺失而失败；实现后 `tests/api-client.test.ts -t "admin user playback and CSRF-protected stop contracts"` 1/1、`pnpm --dir web install --frozen-lockfile`、`pnpm --dir web build`、`git diff --check` 通过。完整 `pnpm --dir web test` 为 736 passed、4 failed；失败仍为此前记录的 `home-page.test.tsx` 1 项、`library-card.test.tsx` 2 项及 `media-query-polling.test.tsx` 1 项，本任务没有修改这些实现/测试文件。无 Rust 或 migration 变化。

#### LUX-738：在用户详情展示当前播放与停止 HLS 会话

范围：在用户详情“设备与登录”页复用 LUX-737 客户端展示目标用户最近 90 秒活跃的播放会话，提供分页、媒体/进度、客户端/设备和 IP 摘要。只有 API 标记 `canStop` 的 Lux 服务端 HLS 才可请求停止；停止前确认，成功后刷新，失败可重试。页面必须说明直放和 STRM 外部播放的控制边界。本任务不增加设备清单、黑名单或 Emby 播放控制。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/src/features/admin/user-management.css`、`web/tests/admin-user-sessions.test.tsx`。

验收标准：

- [x] 活跃播放摘要按用户分页显示媒体标题、播放进度、状态、客户端、设备和来源 IP，提供加载、错误/重试、空结果及翻页反馈。
- [x] 只有 `canStop` 会话显示停止操作；操作需确认，成功后刷新当前列表，失败显示错误；其他会话保持不变。
- [x] 明确说明停止仅支持 Lux 服务端 HLS；直放与 STRM 外部播放无法保证立即中断，且不撤销用户登录。
- [x] 定向 session UI Vitest、Web build、桌面/窄屏浏览器验证和 diff check 通过。

短计划：先扩展 session UI 回归覆盖播放列表摘要、分页、停止权限/确认/刷新/失败及边界文案并确认失败；再接入播放查询面板和响应式样式；最后运行定向 Vitest、Web build、浏览器验证和 diff check。

结果（2026-10-20）：用户详情“设备与登录”增加当前播放面板，使用 LUX-737 API 显示分页媒体、进度/时长、播放方式、客户端/设备、来源 IP、状态和停止资格。仅 `canStop` 会话展示操作；停止前确认，成功刷新数据，失败保留会话并提示错误。直放和 STRM 的控制边界清楚说明，停止不会撤销登录。窄屏使用表格内部横向滚动，避免页面整体横溢出。

验证：TDD RED 回归在面板缺失时失败；最终 `tests/admin-user-sessions.test.tsx` 11/11、冻结依赖安装、`pnpm --dir web build` 和 `git diff --check` 通过。完整 `pnpm --dir web test` 为 738 passed、4 failed；仍是此前已记录且不涉及本任务文件的 `home-page.test.tsx` 1 项、`library-card.test.tsx` 2 项和 `media-query-polling.test.tsx` 1 项。Playwright 用本地模拟 API 验证 1280×900 桌面与 390×844 窄屏：桌面内容区 809px 时播放表格和停止操作完整可见；窄屏文档宽 375px、表格视口 310px，通过表格内横滑访问操作列，页面无横向溢出。确认对话框确认后发送 DELETE 204 并显示成功状态；`canStop=false` 的 STRM 行没有停止按钮；浏览器控制台错误 0。截图：`output/playwright/user-current-playback-desktop.png`、`output/playwright/user-current-playback-390.png`、`output/playwright/user-current-playback-390-actions.png`。此验证使用模拟 API，不代表真实 FNOS 或生产数据验证。

#### LUX-739：建立用户设备清单存储结构

范围：新增 SQLite/PostgreSQL 用户设备清单表，以 `(user_id, device_id)` 标识单个用户声明的设备，保存客户端名称/版本、设备名称/类型、首次出现、最近活动和可空的最近来源 IP。迁移从现存 Emby `access_tokens` 回填可识别的设备记录；已有令牌中的设备与客户端字段按声明信息处理，来源 IP 不推断或伪造。本任务只新增 schema 和可信旧数据回填，不接入登录/请求写入、查询 API、页面、黑名单或 IP ACL。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0194_user_devices.sql`、`migrations-postgres/0194_user_devices.sql`、`tests/storage.rs`、`tests/postgres_database.rs`。

验收标准：

- [x] SQLite 空库迁移建立用户设备清单、级联删除和按用户/最近活动读取索引；迁移不生成可空来源 IP。
- [x] SQLite 从 schema 193 升级时，以成功签发的历史 Emby 凭证为证据，按用户和设备合并记录，保留首次/最近活动及最新设备摘要。
- [x] PostgreSQL 提供等价迁移和空库运行回归；两种数据库 schema 和索引一致。
- [x] 定向 migration 测试、Rust build、fmt 与 diff check 通过；不改认证或访问控制行为。

短计划：先增加 SQLite 空库与 193→194 升级测试、PostgreSQL 迁移验证并确认失败；再新增两种数据库迁移与旧凭证回填；最后运行最窄 migration targets、build、fmt 与 diff check。

#### LUX-739 验证记录

结果（2026-10-20）：SQLite/PostgreSQL 新增 `user_devices`，以用户和客户端声明设备 ID 作为复合主键，外键级联清理，按用户及最近活动建立查询索引。迁移将已有 Emby 凭证按用户/设备合并；首次出现取最早凭证签发时间，最近活动取最大 `last_seen_at`（缺失时回退签发时间），摘要取最近活动凭证的客户端/设备字段。撤销时间不视为最近活动；空设备 ID 被跳过；旧记录没有可证实的 IP，故保持 NULL。未改认证、登录会话或访问控制行为。

验证：TDD RED 空库回归先因 schema 实际为 193（预期 194）而失败；实现后 `tests/storage` 设备清单 migration 回归 2/2、活动索引最新 schema 回归 1/1、PostgreSQL 16 空库 migration 回归 1/1 通过。`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check` 和允许已知 lint 类别的全目标 Clippy 通过。未放行的全量 Clippy 仍有 9 条既有诊断，位于 `src/api/admin/admin_users.rs:554`、`src/api/media.rs:2837`、`src/application/playback/session.rs:397, src/application/user_permission_copy.rs:382,657`、`src/storage/users.rs:3161,3164` 和 `src/storage/repository_tests.rs:15587,23380`；本任务未修改这些文件。旧凭证回填行为在 SQLite schema 193 升级回归覆盖；PostgreSQL 本地运行回归覆盖空库迁移和级联清理。

#### LUX-740：实现用户设备活动存储写入

范围：增加用户设备观察的原子 upsert 存储合同。首次写入建立设备记录；后续写入按用户和设备隔离，首次出现保留最早时间，最近活动与设备/客户端摘要不因乱序旧观察回退。新观察缺少 IP 或可选摘要时保留先前已知值。仅实现 storage 层，不接认证写入、HTTP API、黑名单或 ACL。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 新设备按用户/设备标识写入完整客户端声明摘要与可选来源 IP。
- [x] 重复及乱序观察原子合并；首次时间单调取最早，最近时间和摘要不回退。
- [x] 缺失的可选字段不清除已有值；相同设备标识在不同用户之间独立。
- [x] 定向 storage 回归、Rust build、fmt 与 diff check 通过；没有认证或访问控制接线。

短计划：先添加 storage upsert 行为回归并确认写入合同缺失；再增加观察 DTO 和兼容 SQLite/PostgreSQL 的单语句 upsert；最后运行最窄 storage 测试、build、fmt 与 diff check。

#### LUX-740 验证记录

结果（2026-10-20）：storage 增加 `NewUserDeviceObservation` 与单语句 upsert。相同用户/设备的首次出现时间只会向更早时间推进，最近活动时间取最大值；设备/客户端摘要和 IP 只由不早于当前记录的观察更新，空或缺失的可选值保留旧值。相同客户端设备 ID 在不同用户间保持隔离。调用接线留给后续任务。

验证：TDD RED 因观察类型及 `upsert_user_device` 缺失而编译失败；实现后 `cargo test --locked --lib user_device_observations_merge_monotonically_and_are_user_scoped` 1/1、`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check` 和仅允许 LUX-739 所记录 9 条既有 Clippy 诊断类别的全目标 Clippy 通过。完整 Clippy 的 9 条既有诊断仍未修复；本任务只新增 `repository.rs`、`mod.rs`、`users.rs` 和存储测试，不接认证或 ACL。

#### LUX-741：为 Web 请求提供稳定设备标识

范围：Web API 客户端生成并持久化随机、无个人资料含义的浏览器设备 ID，并在同源 API 请求中发送 `X-Lux-Device-Id`。优先使用浏览器本地存储；存储受限时以内存临时标识继续请求，不阻断登录。该标识仅供设备清单关联，服务端不得将其作为身份认证、授权或 ACL 凭据。本任务不接入服务端写入或展示设备记录。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] 请求携带格式受限的稳定设备 ID；同一浏览器连续请求复用该 ID。
- [x] 首次生成使用安全随机源并持久化；读取/写入本地存储失败时请求继续且有进程内稳定 ID。
- [x] 定向 API client 回归、冻结依赖安装、Web build 与 diff check 通过。

短计划：先添加共享请求头回归并确认现有客户端不发送设备 ID；再加入有界 UUID 生成、存储和请求头；最后运行 API client 定向 Vitest、Web build 和 diff check。

#### LUX-741 验证记录

结果（2026-10-20）：共享 Web API 客户端为请求附加 `X-Lux-Device-Id`。ID 使用 Web Crypto 生成 UUID v4，校验已存值格式后复用；无法读取或写入 localStorage 时以内存 ID 保持页面生命周期内稳定，并且不阻断请求。若存储随后恢复可用，会持久化已生成的内存 ID。设备 ID 仅用于服务端设备清单归并，不作为认证凭证。

验证：TDD RED 两项 API client 回归因请求缺少设备 ID 而失败；最终 `tests/api-client.test.ts` 79/79，冻结依赖安装和 `pnpm --dir web build` 通过。完整 `pnpm --dir web test` 为 740 passed、4 failed；失败仍集中于既有 `home-page.test.tsx` 1 项、`library-card.test.tsx` 2 项和 `media-query-polling.test.tsx` 1 项，本任务未改这些文件。构建通过 TypeScript 检查；diff check 通过。

#### LUX-742：记录 Web 用户成功登录设备

范围：Web 登录成功且通过远程访问策略后，校验 `X-Lux-Device-Id` 为 UUID v4，再将 Lux Web 客户端、WEB 类型和展示 IP 写入用户设备清单。客户端未发送或发送无效 ID 时仍按现有规则完成登录，但不创建设备行。设备登记失败不得撤销成功登录；只记录不含用户或设备字段的存储错误。本任务不更新后续请求活动、不增加查询 API、页面或 ACL。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/api/legacy.rs`、`src/api/users.rs`、`tests/users.rs`。

验收标准：

- [x] 使用有效 Web 设备 ID 登录后，当前用户设备清单记录客户端类型、首次/最近活动与可用展示 IP。
- [x] 缺失或无效设备 ID 不阻断登录，也不会创建未标识设备。
- [x] 设备观察发生在账号和远程访问检查之后；存储错误不泄露标识且不误报登录失败。
- [x] `users` 定向集成回归、Rust build、fmt 与 diff check 通过；不改 Emby 行为。

短计划：先扩展成功登录集成回归并确认设备行不存在；再在远程访问授权后校验 UUID v4 并调用 LUX-740 upsert；最后运行 `users` 目标、build、fmt 和 diff check。

#### LUX-742 验证记录

结果（2026-10-20）：Web 登录通过账号与远程访问检查后，才校验 `X-Lux-Device-Id` 为 UUID v4 并以 `Lux Web`/`WEB` 元数据写入设备清单；只在请求来源可用时保存展示 IP。缺少或格式无效的 ID 不影响正常登录，也不生成设备记录。设备 upsert 失败只写不带账号/设备字段的告警，不改变已创建的 Web 会话。Emby 登录与后续有效请求留给后续任务。

验证：`users` 目标串行运行 19/19；集成回归覆盖有效 ID 入库、无效 ID 登录仍成功且不新增设备、设备存储注入失败时登录仍返回 200。`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check` 和允许此前记录的 9 条 Clippy 诊断类别的全目标 Clippy 通过。未放行的全量 Clippy 仍受同一组未修改代码诊断阻断；没有变更 Emby 认证或 ACL。

#### LUX-743：记录 Emby 用户成功登录设备

范围：Emby 用户名登录和指定用户登录通过远程访问策略后，将有效的客户端声明设备 ID 写入 LUX-739 用户设备清单。Emby 设备 ID 保持客户端提供的任意字符串形式，不要求 UUID；空白或超过 256 字节的 ID 不登记，但不影响成功登录。记录 Emby 客户端名、设备名、版本及可用展示 IP；设备登记失败不得撤销成功登录，并且告警不得包含用户或设备字段。本任务不更新后续请求活动、不增加查询 API、页面或 ACL。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/api/emby_handlers.rs`、`tests/emby_auth.rs`。

验收标准：

- [x] 两种 Emby 密码登录共用成功接线；只有通过远程访问检查后才写入设备观察。
- [x] 合法设备 ID 记录客户端、设备名、版本、EMBY 类型及可用展示 IP；同一 ID 的后续登录更新同一用户设备记录。
- [x] 缺少、空白或过长设备 ID 不阻断认证且不产生设备记录；存储失败不改变成功认证结果。
- [x] `emby_auth` 定向集成回归、Rust build、fmt 与 diff check 通过；不改 Emby 令牌合同或 ACL。

短计划：先扩展 `tests/emby_auth` 覆盖正常、缺失/越界 ID 和设备存储失败，并确认设备行不存在；再在共用成功响应中、远程访问检查之后增加有界设备观察 upsert；最后运行 `emby_auth` 目标、build、fmt 和 diff check。

#### LUX-743 验证记录

结果（2026-10-20）：Emby 用户名和指定用户密码登录共用成功响应路径；通过既有远程访问检查后，有效的客户端设备 ID（非空、至多 256 字节）会 upsert 到用户设备清单，保存 `EMBY` 类型、客户端、设备名、版本、时间及可用展示 IP。Emby ID 不要求 UUID；无效 ID 跳过设备登记但保留登录成功。设备写入错误只记录存储错误，不返回用户或设备字段，也不撤销认证。

验证：TDD RED 集成回归先因设备行缺失而 `RowNotFound`；实现后 `cargo test --locked --test emby_auth -- --test-threads=1` 为 10/10，覆盖用户名登录、指定用户登录、同 ID 更新、展示 IP、缺失/空白/过长 ID 和注入设备存储失败时仍返回 200。测试使用项目真实 `ConnectInfo` 服务模式；本机 localhost 请求显式设 `NO_PROXY=127.0.0.1,localhost`。`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check` 和允许此前记录 9 类既有 Clippy 诊断类别的全目标 Clippy 通过。未修改 Emby token 合同、后续请求活动、设备查询 API/UI 或 ACL；完整严格 Clippy 仍有前序记录的 9 条未改代码诊断。

#### LUX-744：增加有界用户设备清单查询

范围：增加 storage 层按指定用户查询设备清单的稳定分页合同，返回设备标识、名称、类型、客户端、版本、首次/最近活动时间与最近展示 IP，并返回筛选总数。结果按最近活动倒序、设备 ID 正序稳定排序；页大小限制为 1–100，负偏移归零。仅增加查询模型、storage 方法和测试，不增加 HTTP API、Web 页面、活动接线或黑名单。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 查询仅返回目标用户设备，所有设备摘要字段与匹配总数正确。
- [x] 最近活动倒序并以设备 ID 稳定排序；页码偏移、空页与相邻页正确。
- [x] 页大小服务端限制为 1–100、负偏移归零，查询无全表扫描用户集合以外的数据。
- [x] storage 定向回归、Rust build、fmt 与 diff check 通过；无 migration 或 API 变化。

短计划：先添加 `repository_tests` 分页/隔离回归并确认查询合同不存在；再增加设备摘要 DTO 与有界 count/page 查询；最后运行最窄 storage 测试、build、fmt 与 diff check。

#### LUX-744 验证记录

结果（2026-10-20）：storage 提供指定用户设备清单的 count + page 查询，字段包括设备 ID/名称/类型、客户端/版本、首次/最近活动和展示 IP。排序为最近活动倒序、设备 ID 正序；页大小夹在 1–100，负偏移归零。无 migration，查询未暴露到 API。

验证：TDD RED 因 `UserDevicePageQuery` 与分页方法缺失而编译失败；实现后 `cargo test --locked --lib user_device_` 2/2，覆盖目标用户隔离、所有摘要字段、同时间稳定排序、总数、连续/空页、负偏移和 100 条上限。`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check` 和允许已知 9 类既有诊断类别的全目标 Clippy 通过。严格全量 Clippy 仍受前序记录的 9 条未修改代码诊断阻断；无 schema 或 API 改动。

#### LUX-745：开放管理员用户设备清单 API

范围：将 LUX-744 的有界设备分页查询开放为管理员只读接口。返回设备 ID、名称、类型、客户端、版本、首次/最近活动和展示 IP；默认页码 1、页大小 20，服务端页大小限制 1–100。仅需要管理员认证，不需要 CSRF；无效用户 ID 返回 400，不存在用户返回 404。本任务不增加 Web API client 或页面，不实现访问活动刷新、设备操作或黑名单。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [x] 管理员可读取目标用户设备分页和总数；排序继承 storage 的最近活动倒序及设备 ID 正序。
- [x] 未认证拒绝，非管理员禁止；GET 不要求 CSRF；无效/不存在目标 ID 返回文档规定状态。
- [x] 响应只包含设备摘要，不含会话 token、密码或其他认证凭证。
- [x] `users` 定向集成回归、Rust build、fmt 与 diff check 通过，API 文档已更新。

短计划：先在 `tests/users` 添加未认证/非管理员、分页摘要、排序和错误 ID 回归并确认新路由未注册；再接入 storage 查询和管理员用户存在性校验；最后运行 `users` 定向目标、build、fmt 与 diff check。

#### LUX-745 验证记录

结果（2026-10-20）：新增 `GET /api/v1/admin/users/{userId}/devices`，只允许管理员读取。页码默认 1 并钳制最小值，页大小默认 20 并限制 1–100；响应只含设备摘要，不包含 user ID 或登录凭证。用户 ID 格式错误返回 400、用户不存在返回 404；GET 只需管理员认证，无 CSRF 要求。

验证：TDD RED 回归先因新路由返回 404（预期未认证 401）；实现后 `cargo test --locked --test users -- --test-threads=1` 为 20/20，覆盖未认证/非管理员、设备字段、稳定分页、页码/页大小钳制、400/404 错误及无需 CSRF。`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check` 和允许前序记录 9 类既有诊断类别的全目标 Clippy 通过；严格全量 Clippy 仍有相同 9 条未改代码诊断。`docs/API.md` 已登记合同；不接入 Web client/UI、后续活动接线、黑名单或 ACL。

#### LUX-746：增加设备清单 Web API 合同

范围：为 LUX-745 管理员设备清单 GET API 增加安全 TypeScript 摘要/分页 DTO 和 client 方法。方法默认 page 1、pageSize 20，正确 URL 编码 user ID，并通过统一请求层读取。本任务不修改详情页 UI、服务端 API、认证或设备刷新行为。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] DTO 精确映射设备公开摘要字段，不定义用户 ID、token、Cookie 或凭证字段。
- [x] Client 分页默认参数正确，用户 ID 做路径编码并发送 `page`/`pageSize`。
- [x] API client 定向 Vitest、冻结依赖安装、Web build 与 diff check 通过。
- [x] 不改服务端行为或设备 UI。

短计划：先在 `api-client.test.ts` 增加分页参数、默认值、路径编码和响应 DTO 回归并确认 client 方法不存在；再增加 DTO 与 client 方法；最后运行定向 Vitest、冻结安装、Web build 和 diff check。

#### LUX-746 验证记录

结果（2026-10-20）：增加 `AdminUserDevice`/`AdminUserDevicePage` 安全 DTO 和 `adminUserDevices` client 方法。分页默认 1/20，user ID 路径编码，查询参数为 `page`/`pageSize`，读取通过统一请求层 GET，不定义 user ID、token 或 Cookie 字段。未改服务端或 UI。

验证：TDD RED 回归先因 `adminUserDevices` 方法不存在而失败；实现后定向 `tests/api-client.test.ts` 回归通过，冻结依赖安装和 `pnpm --dir web build` 通过。全量 `pnpm --dir web test` 为 741 passed、4 failed：`home-page.test.tsx` 首页库预取 1 项、`library-card.test.tsx` 两项、`media-query-polling.test.tsx` 1 项；与前序记录的四项基线失败相同，设备 API client 用例通过。`git diff --check` 通过；本任务不涉及 Playwright 页面变更。

#### LUX-747：在用户详情展示设备清单

范围：在用户详情“设备与登录”区域接入 LUX-746 设备清单 API，分页展示客户端、设备标识与名称、类型、版本、首次/最近活动及最近展示 IP。提供加载、空结果、读取错误重试和分页状态；所有设备值按普通文本安全展示。本任务只读，不实现设备/客户端封禁、访问活动刷新、登录状态关联或其他设备操作。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/src/features/admin/user-management.css`、`web/tests/admin-user-devices.test.tsx`、`web/tests/admin-user-sessions.test.tsx`（为共用详情页的既有会话测试隔离设备 API 请求）。

验收标准：

- [x] 用户详情设备与登录页通过 LUX-746 客户端按页读取设备，并展示公开摘要字段。
- [x] 加载、空结果、错误重试、总页数及上一页/下一页状态均可访问且正确。
- [x] 组件定向测试、冻结依赖安装、Web build、浏览器展示检查和 diff check 通过。
- [x] 不新增设备操作、黑名单、活动写入或登录凭证字段。

短计划：先新增设备字段、默认分页、空结果和失败重试的组件回归并确认当前详情页未调用设备 API；再接入 React Query 与响应式设备表格；最后运行定向 Vitest、冻结依赖安装、Web build、浏览器检查和 diff check。

#### LUX-747 验证记录

结果（2026-10-20）：用户详情“设备与登录”区新增只读设备清单，显示客户端、设备名与标识、类型、版本、首次/最近活动和最近展示 IP；列表默认每页 20 条，提供加载、空态、读取失败重试和分页。窄屏表格在自身容器内滚动，不扩展页面宽度。未新增封禁、设备操作、活动写入、登录状态关联或凭证字段。

验证：设备组件测试 4/4、现有用户会话组件测试 11/11；`pnpm --dir web install --frozen-lockfile` 与 `pnpm --dir web build` 通过。全量 Web 测试为 745 passed、4 failed，仍为 `home-page.test.tsx`、`library-card.test.tsx` 两项与 `media-query-polling.test.tsx` 的同四项既有基线失败。隔离 Playwright 固定假数据预览在 390×844 显示设备清单；视口和文档宽度均为 390 px，840 px 表格通过 350 px 内层容器滚动，浏览器 console 无错误/警告；截图 `output/playwright/admin-user-devices-390.png`。`git diff --check` 通过。

#### LUX-748：通过 Web 有效访问刷新设备活跃时间

范围：为携带有效 UUID v4 `X-Lux-Device-Id` 的已认证 Web 请求刷新该用户设备记录的最近活动时间及可用展示 IP。必须先通过会话、远程访问策略，再做 best-effort 存储；无效/缺失 ID 跳过，设备存储错误不得改变请求响应且日志不得记录用户或设备字段。设备 ID 仍不参与身份验证或授权。不接入 Emby 后续访问记录或设备黑名单。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/api/users.rs`、`tests/users.rs`。

验收标准：

- [x] 成功登录后，后续通过 Web 会话与远程访问检查的请求刷新同一设备的 `last_seen_at` 和可用展示 IP，不改变 `first_seen_at`。
- [x] 缺失或格式无效的设备 ID 不写入设备记录；设备存储写入失败不阻断已通过认证的请求。
- [x] 用户认证集成定向测试、Rust build、fmt、可用 Clippy 检查及 diff check 通过。

短计划：先在 `tests/users.rs` 将已登录设备的 `last_seen_at` 置为旧值，再用同会话访问受保护 API 并确认时间更新、首次时间不变；确认当前实现未刷新后，在共享 Web 用户身份解析的会话分支通过策略检查后加入 best-effort upsert；最后运行 `users` 精确回归、build、fmt、Clippy 与 diff check。

#### LUX-748 验证记录

结果（2026-10-20）：Web 登录沿用原设备登记入口；之后通过 Web 会话和远程访问策略检查的请求携带有效设备 ID 时，会 best-effort upsert 同一设备，刷新最近活动和可用展示 IP，保留首次出现时间。无效设备 ID 不新增或刷新记录。设备存储失败不会改变已认证请求响应，且告警不含用户/设备字段；设备 ID 仍只用于清单归并。

验证：TDD RED 在同一 authenticated request 带有效设备 ID、旧 `last_seen_at` 时失败；实现后 `tests/users` 串行 20/20，最后精确回归 1/1，另覆盖首次时间不变、无效设备 ID 跳过和设备存储失败时受保护请求仍成功。Rust build、fmt check、`git diff --check` 与放行既有 Clippy 诊断类别后的全目标 Clippy 通过。严格全目标 Clippy 仍受 10 条既有、位于未改文件的诊断阻断，本任务相关文件没有新增诊断。

#### LUX-749：通过 Emby 有效访问刷新设备活跃时间

范围：已签发的 Emby 用户凭证通过认证及远程访问策略后，使用凭证关联的客户端声明设备刷新用户设备清单最近活动，并更新可用展示 IP。首次出现时间不变；无可识别设备信息时跳过；设备存储失败不得阻断已通过认证的请求，日志不包含用户或设备字段。共享管理员 API Key 不代表普通用户设备，不写设备清单。不增加黑名单或 IP ACL。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/api/emby_handlers.rs`、`tests/emby_auth.rs`。

验收标准：

- [x] Emby 用户登录后使用同一凭证访问受保护端点，会刷新同一用户设备的 `last_seen_at` 与可用展示 IP，`first_seen_at` 不变。
- [x] 无设备 ID 的既有凭证不创建伪设备；设备清单写入失败不改变已认证请求响应。
- [x] Emby 认证集成目标、build、fmt、可用 Clippy 检查与 diff check 通过。

短计划：在 `tests/emby_auth.rs` 复用真实登录流程并把设备最近活动置旧，先验证后续受保护请求未更新；再在共享 Emby 用户凭证解析通过远程访问检查后，依据 token 的安全设备摘要 best-effort 更新设备记录；最后运行 Emby 认证集成目标、build、fmt 与 Clippy。

结果（2026-10-20）：Emby 用户凭证访问经过身份和远程访问检查后，按凭证关联的设备摘要 best-effort 刷新用户设备清单的最近活动时间及展示 IP；首次出现时间保持不变。无设备 ID 时跳过清单写入，设备查询或 upsert 错误只写不含标识的通用告警且不改变认证响应。共享管理员 API Key 不关联普通用户设备。

验证：TDD RED 的后续有效 Emby 请求未刷新旧设备时间；实现后 `cargo test --locked --test emby_auth -- --test-threads=1` 11/11 通过，覆盖活动时间/IP 更新、首次时间稳定、无设备 ID 跳过与设备更新失败仍返回 200。`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check` 通过。定向 Clippy 放行 7 类既有诊断后通过；严格 `cargo clippy --locked --test emby_auth -- -D warnings` 仍被未改文件的既有诊断阻断，本次修改文件无新增诊断。Cargo 输出目录在 Toshiba 挂载盘。

#### LUX-750：为 Web 登录会话预留设备关联字段

范围：为 `web_sessions` 增加可空 `device_id`，保存 Web 登录会话对应的客户端声明设备 ID。迁移必须从空库与现有库升级；既有会话保持 `NULL`，不回填猜测值。本任务仅更新 SQLite/PostgreSQL schema 和迁移验证，不在认证/API 写入或读取该字段，不改变 Web 会话行为。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0195_web_session_device_id.sql`、`migrations-postgres/0195_web_session_device_id.sql`、`tests/storage.rs`、`tests/postgres_database.rs`。

验收标准：

- [x] 新旧 SQLite/PostgreSQL 数据库迁移后，`web_sessions.device_id` 可空且旧会话值为 `NULL`。
- [x] PostgreSQL 与 SQLite schema 版本及 migration 对齐，空库迁移和既有 Web 会话升级回归通过。
- [x] `storage` 相关目标、build、fmt、可用 Clippy 与 diff check 通过。

短计划：先扩展 SQLite/PostgreSQL 设备清单迁移测试，断言新列存在且已有会话不回填；运行 SQLite migration 定向用例确认 RED；再添加版本 0195 双数据库 additive migration，最后运行存储与数据库迁移验证。

结果（2026-10-20）：SQLite 与 PostgreSQL 均新增 `web_sessions.device_id TEXT NULL` migration 0195；旧会话保持 NULL，不推断设备关联。未在该任务接入认证写入或读取。

验证：SQLite 空库迁移与既有 0194 会话升级分别通过；PostgreSQL 空库 migration 和旧会话升级两个真实本机测试均通过。`cargo build --locked`、`cargo fmt --all -- --check`、PostgreSQL 测试目标编译及 `git diff --check` 通过；`storage` 与 PostgreSQL 测试目标 Clippy 在放行 7 类既有诊断后通过。另一个宽泛 `storage migration_runs_from_an_empty_database` 过滤运行到 7 项，其中 5 项旧迁移测试仍因断言 schema 版本为 190/192 而失败；本任务新增与直接关联的迁移用例通过，这些历史版本断言尚未同步。

#### LUX-751：存储 Web 会话设备关联

范围：扩展 Web 会话存储，使调用者可为新会话写入可选的客户端声明设备 ID，并可从已验证会话安全读回该字段。原有无设备参数的内部调用继续写 NULL。只增加存储合同，不从 HTTP Header 绑定设备、不改变登录流程或设备访问策略。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/catalog.rs`、`src/storage/repository.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 新会话创建时可选择写入设备 ID，旧入口仍可创建 `device_id=NULL` 的会话。
- [x] 通过有效会话凭证读取时返回对应设备 ID；不存在、撤销或过期会话仍不可读取。
- [x] 存储会话相关单测、build、fmt、可用 Clippy 与 diff check 通过。

短计划：先扩展 session storage 测试，验证指定设备 ID 可写可读且默认空值保持；运行定向测试确认 RED；然后为 session insert 增加可选参数合同并在 `StoredWebSession` 返回字段；最后运行 session 存储回归和常规检查。

结果（2026-10-20）：`web_sessions` 存储合同增加带可选设备 ID 的创建入口，并由已验证会话查询返回该 nullable 字段；原创建入口保持兼容且写入 NULL。凭证查询仍过滤不存在、过期和撤销会话。

验证：TDD RED 先因创建方法和返回字段不存在而编译失败；实现后 `storage::repository::repository_tests::web_session_device_association_is_optional_and_only_read_from_active_sessions` 1/1 通过，覆盖有关联、旧入口空值、过期、撤销和未知 token。`cargo build --locked`、`cargo fmt --all -- --check`、定向 Clippy 放行 7 类既有诊断后及 `git diff --check` 通过。设备字段将在下一增量接入 Web 登录与活动检查。

#### LUX-752：登录时绑定 Web 会话设备 ID

范围：Web 登录时将有效 UUID v4 `X-Lux-Device-Id` 写入该次会话，后续有效会话访问以存储值刷新设备活动，不允许请求中变化的设备头替换已绑定身份。无效/缺失 ID 不阻断登录；历史 `device_id=NULL` 会话继续兼容现有请求头设备观察。不把设备 ID 用作身份认证或权限判定。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/api/users.rs`、`src/auth/sessions.rs`、`src/storage/repository.rs`、`tests/users.rs`。

验收标准：

- [x] 登录成功后，规范化的有效设备 ID 与新 Web 会话关联；无效/缺失 ID 保持 NULL 且登录成功。
- [x] 已绑定会话访问即使不带设备头或携带其他设备头，也只刷新登录时绑定的设备记录，不新建伪关联。
- [x] 既有 NULL 会话使用有效请求头时保持当前活动登记兼容；用户认证集成、build、fmt、可用 Clippy 与 diff check 通过。

短计划：先扩展 `tests/users.rs` 覆盖登录会话绑定和请求头替换，确认当前认证链失败；再增加带设备 ID 的 Web 登录服务入口，令普通登录入口保持兼容；身份读取后优先使用存储设备 ID，历史 NULL 会话才回退到已校验请求头；最后运行用户认证回归及常规检查。

结果（2026-10-20）：Web 登录将有效 UUID v4 规范化后写入新会话；会话身份读取优先采用已绑定 ID，旧会话才回退到请求头。无效或缺失设备头仍可登录且会话关联为空，设备记录失败仍不改变登录或后续授权响应。

验证：新增回归先因会话设备字段为空而失败；实现后精确用例通过，并验证绑定会话在请求头缺失、改变或无效时只刷新原设备，NULL 历史会话继续按有效头记录。`cargo test --locked --test users -- --test-threads=1` 最终 20/20 通过；首次整目标运行中管理员删除测试出现一次 503，单项复跑及完整目标重跑均通过。`cargo build --locked`、`cargo fmt --all -- --check`、Clippy 放行 7 类既有诊断后的全 targets 检查及 `git diff --check` 通过。

#### LUX-753：在设备清单展示有效登录状态

范围：扩展用户设备分页结果，指示同一用户、同一设备是否存在有效登录。未撤销且未过期的 Web 会话，或未撤销的 Emby 凭证，均计为有效登录；撤销/过期凭证不计。设备清单新增文字状态，不把有效登录状态表述为实时在线。本任务不改变会话有效期、撤销规则或认证策略。

预计修改文件（12 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`、`src/api/admin/admin_users.rs`、`tests/users.rs`、`tests/postgres_database.rs`、`web/src/lib/api/types.ts`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/src/features/admin/user-management.css`、`web/tests/admin-user-devices.test.tsx`、`web/tests/api-client.test.ts`。

验收标准：

- [x] 有效 Web 会话与 Emby 凭证能按用户和设备关联到设备清单；已撤销或已过期凭证不会显示为有效登录，SQLite 与 PostgreSQL 查询均通过。
- [x] 管理员设备 API 返回布尔登录状态，分页、用户隔离、凭证保密合同不变。
- [x] Web 设备表格用文字显示“有有效登录”或“无有效登录”，并覆盖客户端渲染与相关 Rust 查询测试。

短计划：先扩展 `user_device_listing_is_scoped_stable_and_bounded` 与设备 API/UI 测试，覆盖有效 Web、有效 Emby、撤销及过期记录；运行定向目标确认登录状态字段尚缺；再让有界设备页查询以 EXISTS 关联有效凭证，扩展 DTO 与表格；最后运行 SQLite storage/users、PostgreSQL 登录状态集成、对应 Web 用例、build、fmt、可用 Clippy 与 diff check。

结果（2026-10-20）：设备分页查询通过有界 correlated `EXISTS` 汇总未撤销且未过期的 Web 会话及未撤销 Emby 凭证；管理员 API 暴露 `hasActiveLogin`，用户详情设备表以文字显示有效登录状态。没有返回会话 Cookie 或 Emby token 字段。

验证：SQLite storage 回归 1/1、`tests/users` 20/20、真实 PostgreSQL 设备登录状态 API 集成 1/1、设备 UI/API client 用例 84/84、`pnpm --dir web build`、`cargo build --locked`、fmt 检查、放行已记录 Clippy 诊断后的全 targets Clippy 与 `git diff --check` 通过。完整 Web 测试仍有 4 个既有失败（首页库预取 1 项、媒体库卡片 2 项、详情轮询 1 项；745 项通过），不在本任务改动路径。窄屏表格横向滚动由设备列表容器承载，页面宽度保持不变。

#### LUX-754：建立用户设备与客户端黑名单规则表

范围：为模块七的全站与用户级设备、客户端、IP/CIDR 黑名单建立 SQLite/PostgreSQL schema。每条规则保存可选目标用户、规则类型、规范化匹配值、启用状态、创建者及时间；全站规则以空 `user_id` 表示，目标用户删除时清除其用户级规则，创建者删除时保留规则并清空创建者引用。本任务只增加迁移与 schema 验收，不开放 API、不解析 CIDR、不执行授权拒绝，也不改变现有认证行为。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0196_user_access_blacklist_rules.sql`、`migrations-postgres/0196_user_access_blacklist_rules.sql`、`tests/storage.rs`、`tests/postgres_database.rs`。

验收标准：

- [x] SQLite 与 PostgreSQL 迁移建立受约束的全站/用户级 `DEVICE`、`CLIENT`、`IP_CIDR` 规则表和有界匹配索引。
- [x] 空库启动与从 0195 升级成功；删除目标用户级联删除规则，删除创建者保留规则并清空创建者字段。
- [x] SQLite schema 约束回归与本机 PostgreSQL 空库迁移回归通过；本任务不引入规则执行行为。

短计划：先给 SQLite/PostgreSQL migration 回归增加版本、列、约束、唯一索引和外键断言并确认新迁移缺失；再加入双库迁移；最后运行 storage 定向测试、PostgreSQL 迁移测试、Rust build、fmt 与 diff check。

结果（2026-10-20）：SQLite 与 PostgreSQL 新增 0196 用户访问黑名单规则迁移，支持全站与用户范围的 `DEVICE`、`CLIENT`、`IP_CIDR` 类型，包含规范化值唯一约束、启用规则匹配索引、目标用户级联删除和创建者置空关系。迁移仅建立 schema，没有接入请求授权。

验证：SQLite 空库 schema/约束与从 0195 升级保留现有设备回归 1/1；PostgreSQL 空库迁移和从 0195 升级保留设备回归分别 1/1 通过。新增列、索引与外键删除动作通过检查；未新增依赖或修改认证行为。

#### LUX-755：提供有界黑名单规则存储合同

范围：为 LUX-754 的规则表提供按全站或指定用户范围分页读取、创建、启停和删除的 storage 合同。列表固定按创建时间倒序、规则 ID 正序稳定排序，页大小限制为 1–100；用户范围只能读写该用户规则，不能包含全站或其他用户规则。只增加内部存储模型与 SQLite 回归，不开放 API、解析匹配值或执行授权拒绝。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 能在全站与指定用户范围创建、分页查询、启停和删除规则，并返回匹配范围内总数。
- [x] 用户隔离、全站/用户重名规则分离、重复规则由唯一约束拒绝；分页稳定且限制有界。
- [x] SQLite storage 定向回归、Rust build、fmt 与 diff check 通过；不接认证行为。

短计划：先在 `repository_tests.rs` 写范围隔离、CRUD、稳定分页和重复规则回归，确认 storage 合同缺失；再加入最小 repository DTO 与 storage 方法；最后运行对应 storage 测试、build、fmt 和 diff check。

结果（2026-10-20）：storage 提供全站或精确用户范围的规则创建、分页读取、启停及删除；分页按创建时间倒序和 ID 正序稳定排序，limit 限制为 1–100。启停/删除按 rule ID 与作用范围双重限定，不会操作另一范围的规则。

验证：TDD RED 确认类型与 storage 方法缺失；定向回归 1/1 通过，覆盖全站/用户隔离、同匹配值跨范围、范围内重复拒绝、页数/排序/上限、错误范围更新删除和启停状态。Rust build、fmt 检查与 `git diff --check` 通过；没有接入 HTTP 或请求授权。

#### LUX-756：规范化黑名单设备、客户端和 IP 网段值

范围：增加黑名单规则领域值校验，支持设备标识精确匹配、客户端名去首尾空白/合并空白并按小写规范化、IPv4/IPv6 地址或 CIDR 网段规范化。IP 值清除主机位并以固定网络前缀形式保存；非法地址、前缀、空值、控制字符和超过 256 字节的输入拒绝。仅增加纯领域合同与单元测试，不在此任务解析代理头或连接认证策略。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/mod.rs`、`src/application/user_access_rules.rs`。

验收标准：

- [x] 三类规则均有明确输入长度、空值和控制字符校验；客户端名称大小写/重复空格规范化，设备 ID 保留精确匹配语义。
- [x] IPv4、IPv6 单地址、前缀边界、网络地址归一化、IPv4/IPv6 混合和非法 CIDR 均有回归覆盖。
- [x] 领域定向测试、Rust build、fmt 与 diff check 通过；不增加依赖、不改变请求行为。

短计划：先写覆盖三种规则输入边界与 CIDR 网络计算的失败单测；再实现无外部依赖的规范化函数和类型；最后运行定向领域测试、build、fmt 与 diff check。

结果（2026-10-20）：新增设备 ID 精确值、客户端名小写/空白规范化和 IPv4/IPv6 网络类型。IP 单地址被表示为 `/32` 或 `/128`；CIDR 清除主机位，匹配只接受同地址族并按网络前缀判断。输入空白、控制字符、超长值、非法地址及超范围前缀均拒绝；未增加 crate 或接入 HTTP/认证。

验证：领域测试 5/5、`cargo build --locked`、fmt 检查与 `git diff --check` 通过。

#### LUX-757：开放管理员黑名单规则管理 API

范围：为 LUX-754 至 LUX-756 的设备、客户端和 IP/CIDR 规则提供全站与精确用户范围的管理员 HTTP API，支持有界分页读取、规则创建、启停和删除。创建时执行既有领域规范化；用户范围校验目标账号存在；重复规则返回冲突；所有路径都强制作用域隔离。写请求要求管理员认证和 CSRF（共享管理员 API Key 按既有认证合同处理）。本任务只管理规则数据，不把规则接入登录拒绝、会话撤销或播放授权，也不表示 IP 规则已生效。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/api/admin/admin_users.rs`、`tests/users.rs`、`src/storage/users.rs`。

验收标准：

- [x] 全站与指定用户规则 API 支持读取、创建、启停、删除及 1–100 条有界分页；用户范围不能读取或修改全站及其他用户规则。
- [x] 规则类型与值经领域解析/规范化；无效输入返回 400、重复规则返回 409、无效用户 ID 返回 400、目标账号不存在返回 404。
- [x] 写请求检查管理员与 CSRF；响应仅返回规则安全摘要，不泄漏认证资料。
- [x] `tests/users` 的 API 定向回归、Rust build、fmt 与 diff check 通过。
- [x] 规则暂不参与认证或播放拒绝；IP 封禁仍等待可信代理来源解析任务。

短计划：先在 `tests/users.rs` 增加全站/用户范围 CRUD、分页、输入规范化、冲突、认证和 CSRF 回归并确认路由尚未注册；再接入既有 storage 与领域规范化合同；最后同步 API 文档并运行 users 定向目标、build、fmt 与 diff check。

结果（2026-10-20）：新增管理员全站及指定用户规则分页 API，支持设备、客户端和 IP/CIDR 规则创建、启停、删除。请求值经 LUX-756 规范化，重复规则返回 409，跨作用域 ID 按不存在处理；用户范围校验 ID 和目标账号。管理员 Web 写请求检查 CSRF；规则摘要记录 Web 管理员创建者，API Key 创建者按既有合同留空。此任务只提供规则管理 API，未改变认证或播放行为。

验证：TDD RED 先因路由未注册返回 404；实现后 `admin_can_manage_scoped_user_access_blacklist_rules` 通过，覆盖认证/CSRF、规则值校验与规范化、重复冲突、分页、作用域隔离及用户 ID 错误。`cargo test --locked --test users -- --test-threads=1` 在显式绕过本地回环代理后 21/21 通过；首次运行 13 个既有本地 HTTP 用例受代理影响返回 502。`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check` 通过。Cargo 使用 Toshiba target 目录。

#### LUX-758：查询设备与客户端黑名单命中

范围：为认证接线提供一个有界 storage 查询，按目标用户同时检查已启用的全站设备/客户端规则和该用户规则。设备标识精确匹配，客户端名使用调用方已规范化的值；禁用规则、其他用户规则和 IP/CIDR 规则不参与本任务。只增加存储合同及 SQLite 回归，不改变认证、会话或播放行为。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 同一查询能命中全站或精确用户范围内已启用的设备/客户端规则。
- [x] 禁用规则、其他用户范围、缺失的设备/客户端值和 IP 规则不产生误命中。
- [x] SQLite storage 定向回归、Rust build、fmt 与 diff check 通过；不改变认证行为。

短计划：先在 `repository_tests.rs` 覆盖全站/用户隔离、规则类型、启用状态与精确匹配并确认查询合同缺失；再添加匹配查询 DTO 和单条 `EXISTS` storage 查询；最后运行该 library 定向测试、build、fmt 与 diff check。

结果（2026-10-20）：增加单条 `EXISTS` storage 合同，同时检查目标用户的启用全站规则和精确用户规则。只比较设备 ID 与调用方已规范化的客户端名；其他用户、已停用规则、无输入以及 IP/CIDR 规则不产生命中，不改认证行为。

验证：TDD RED 因匹配查询 DTO 和 storage 方法缺失而编译失败；实现后 SQLite 定向回归 `user_access_blacklist_matching_is_scoped_enabled_and_type_specific` 1/1 通过。`cargo build --locked`、`cargo fmt --all -- --check` 与 `git diff --check` 通过，Cargo 使用 Toshiba target 目录。

#### LUX-759：在 Web 登录与会话访问中执行设备/客户端黑名单

范围：Web 登录创建会话前，以当前用户、已绑定设备 ID 和固定客户端 `Lux Web` 检查启用的全站及用户级 DEVICE/CLIENT 规则；匹配则拒绝登录。后续 Web 会话请求重复执行同一判断，命中后撤销该会话并拒绝请求。无法读取规则时 fail closed 返回服务不可用。客户端名按 LUX-756 规范化。只覆盖 Web 身份验证边界，不接 Emby、已签发播放链接或 IP/CIDR 规则。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/auth/sessions.rs`、`src/api/users.rs`、`tests/users.rs`。

验收标准：

- [x] 新登录命中设备或客户端规则时不签发会话；未命中设备仍可正常登录。
- [x] 已有 Web 会话首次命中规则时被撤销并拒绝请求；未命中规则的其他设备会话不受影响。
- [x] Web 登录与会话解析的集成回归、Rust build、fmt 与 diff check 通过。
- [x] 不接入 Emby、播放授权或 IP/CIDR 规则；这些行为留待各自任务。

短计划：先在 `tests/users.rs` 建立已登录设备后添加匹配规则的回归并确认会话仍可用；再于 Web 登录签发前与 Web 会话解析后复用 LUX-758 命中查询，并按命中/存储失败区分拒绝与 503；最后运行 users 定向目标、build、fmt 与 diff check。

结果（2026-10-20）：Web 登录在验证账号后、创建会话前检查规范化的 `Lux Web` 客户端名与当前设备 ID；匹配时返回 403 且不签发会话。后续 Web 会话请求重复检查；命中时先撤销当前会话再拒绝请求，未匹配设备仍可访问。规则存储查询失败时登录/访问 fail closed。

验证：TDD RED 时加入用户设备规则后的已有会话仍返回 200；实现后新增 Web 黑名单集成回归通过，覆盖现有会话撤销、新登录拒绝、另一设备保留访问及全局客户端拒绝。`cargo test --locked --test users -- --test-threads=1` 在设置 `NO_PROXY` 绕过本地代理后 22/22 通过；`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。Cargo 使用 Toshiba target 目录；Emby、播放链接与 IP/CIDR 规则留待后续任务。

#### LUX-760：在 Emby 登录与凭证访问中执行设备/客户端黑名单

范围：Emby 用户登录签发 token 前，以用户、客户端声明设备 ID 和规范化客户端名检查启用的全站及用户级 DEVICE/CLIENT 规则；命中时不签发凭证。已有凭证的每次认证解析重复检查；命中时立即撤销该凭证并按未认证处理。规则读取失败时 fail closed。共享管理员 API Key 不属于普通用户设备，不执行此规则。只覆盖 Emby 认证及凭证，不接播放链接或 IP/CIDR。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/auth/emby.rs`、`tests/emby_auth.rs`。

验收标准：

- [x] Emby 新登录命中设备或客户端规则时返回未认证且不创建 token；不同设备/客户端仍可登录。
- [x] 已有 Emby 凭证首次命中规则时被撤销，认证请求被拒绝；不匹配的凭证继续有效。
- [x] Emby 认证集成定向目标、Rust build、fmt 与 diff check 通过。
- [x] 不接入共享管理员 API Key、播放链接或 IP/CIDR 规则。

短计划：先在 `tests/emby_auth.rs` 覆盖凭证签发、加入设备规则后既有凭证访问和另一个设备登录，并确认当前访问仍有效；再在 `EmbyAuthService` 签发及 token 解析路径复用 LUX-758 查询，命中时撤销现有凭证；最后运行 `emby_auth` 定向目标、build、fmt 与 diff check。

结果（2026-10-20）：Emby token 签发前检查设备 ID 和按规则规范化的客户端名；匹配规则时以现有未认证响应拒绝，不写凭证。已签发 token 每次解析时读取设备摘要，命中后立即撤销并按未认证处理；规则存储失败时不继续授权。共享管理员 API Key 不走普通 Emby 用户凭证路径。

验证：TDD RED 先证明加入设备规则后旧 token 仍可访问；实现后 `emby_login_and_existing_tokens_enforce_device_client_blacklists` 通过，覆盖 token 撤销、同设备拒绝、其他设备继续登录及全局客户端规则规范化。`cargo test --locked --test emby_auth -- --test-threads=1` 绕过本地回环代理后 12/12 通过；`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。Cargo 使用 Toshiba target 目录；播放链接与 IP/CIDR 仍由后续任务处理。

#### LUX-761：解析显式可信代理后的客户端 IP

范围：增加独立领域解析器，接收真实连接 peer、完整 `X-Forwarded-For` 字段值及显式可信代理 CIDR。peer 不可信时完全忽略转发头并返回 peer；peer 可信时校验完整转发链，从靠近 Lux 的一端向左逐跳剥离可信代理，返回首个不可信地址。IPv4-mapped IPv6 统一为 IPv4。可信 peer 缺少头时返回 peer；提供了空值或无效链时返回错误，调用方不得静默退回展示 IP。本任务只提供纯解析逻辑及单元测试，不读取配置、不接 HTTP/认证、不改变访问行为。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/user_access_rules.rs`。

验收标准：

- [x] 不可信 peer 即使收到伪造或格式错误的转发头，解析结果仍为连接 peer。
- [x] 可信 peer 按完整多值链从右向左跳过可信代理；左侧伪造前缀不会覆盖首个非可信地址。
- [x] IPv4、IPv6、IPv4-mapped IPv6、缺失头、空值和无效地址均有单元覆盖。
- [x] 仅用户访问规则库定向测试、Rust build、fmt 与 diff check 通过；本任务不执行封禁。

短计划：先为可信 peer、非可信 peer、伪造左前缀、多字段及畸形输入添加领域单测并确认解析器缺失；再实现纯 CIDR 链解析；最后运行指定库测试、build、fmt 与 diff check。

结果（2026-10-20）：新增纯 trusted-proxy 解析器；将 IPv4-mapped IPv6 规范化为 IPv4，不可信 peer 忽略全部转发头，可信 peer 限制最多 64 跳/8 KiB 并从右侧跳过可信代理。缺失头返回 peer；空字段、非法地址、超长或过多 hop 返回错误。未接入请求、配置或授权。

验证：TDD RED 因解析函数及错误类型缺失而编译失败；实现后 `cargo test --locked --lib application::user_access_rules::tests` 9/9 通过。`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。Cargo 输出使用 `/Volumes/Toshiba/mywork/Lux/target`。

#### LUX-762：存储全局可信代理 CIDR 列表

范围：在现有 `server_settings` 键值表中增加可信代理 CIDR JSON 的读取与原子 upsert 合同，不新增 schema migration。存储层保留 JSON 原文并不解释地址；解析、规范化与上限校验由应用层负责。补充 SQLite 存储回归，不接设置 API、HTTP peer 或 ACL。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/users.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 空库读取返回未配置；写入后能准确读回相同 JSON；后续写入覆盖旧值。
- [x] 仍使用 server settings 参数化 upsert；错误通过既有 `StorageError` 返回。
- [x] SQLite 存储定向测试、Rust build、fmt 与 diff check 通过；请求与授权行为不变。

短计划：先增加 server-settings 回归并确认数据库接口缺失；再新增原文 JSON 的 getter/upsert；最后运行存储单测、build、fmt 与 diff check。

结果（2026-10-20）：`server_settings` 新增 `trusted_proxy_cidrs` 的原文 JSON 查询和参数化 upsert，不增加迁移；空值保持 `None`，后续设置覆盖原值。存储层不解释或信任 CIDR 内容。

验证：TDD RED 因读写方法缺失而编译失败；实现后 `trusted_proxy_cidrs_setting_reads_and_replaces_raw_json` 1/1 通过。`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check` 通过。Cargo 输出使用 `/Volumes/Toshiba/mywork/Lux/target`。

#### LUX-763：管理 API 配置可信代理网段

范围：在管理员服务器设置 GET/PATCH 中暴露 `trustedProxyCidrs`。读取默认 `[]`；PATCH 支持最多 100 个 IPv4/IPv6 地址或 CIDR，规范化网络位、拒绝非法值与规范化后的重复值；空数组清除信任设置，字段省略则保留。写入由管理员 Web session 与 CSRF 保护。本任务不把该配置用于 IP 封禁或改变请求处理。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/application/settings.rs`、`src/api/admin/admin_settings.rs`、`tests/admin_dashboard.rs`。

验收标准：

- [x] GET 总是返回 canonical `trustedProxyCidrs` 数组，未配置时为空。
- [x] PATCH 规范化 IPv4/IPv6/CIDR 并持久化；空数组能清除，省略字段不覆盖已有设置。
- [x] 非法地址、重复网段和超过 100 项均返回 400，失败时不改变已存设置。
- [x] 管理员认证/CSRF、设置 API 定向集成目标、build、fmt 与 diff check 通过；此任务不执行 IP ACL。

短计划：先扩展管理设置集成回归，确认 GET 缺少该字段且 PATCH 未保存；再增加应用层规范化和拒绝规则、API DTO 与映射，读取/写入 LUX-762 storage；最后更新 API 文档并运行定向设置目标、build、fmt 与 diff check。

结果（2026-10-20）：管理员设置 GET/PATCH 增加 `trustedProxyCidrs`，默认空列表；地址和网段经规范化，拒绝非法、规范化重复和超过 100 项，空数组清除，省略时保留。设置保存在 LUX-762 的键值记录中；认证与 CSRF 边界保持管理员 Web session。更新了 `docs/API.md`。本任务仍未将代理设置用于 ACL。

验证：TDD RED 先因 GET 字段缺失而失败；实现后 `admin_dashboard_returns_server_playback_and_activity_data` 1/1 通过，覆盖默认、存储回读、canonical IPv4/IPv6、清除、部分更新、非法输入、CSRF 和失败后保留原值。`cargo build --locked`、`cargo fmt --all -- --check` 与 `git diff --check` 通过。严格 `cargo clippy --locked --all-targets --all-features -- -D warnings` 仍报 13 项既有诊断（相关位置均不在本次新增代码；完整 Rust 门禁尚未通过）。Cargo 输出使用 `/Volumes/Toshiba/mywork/Lux/target`。

#### LUX-764：增加 Web 黑名单 API 客户端合同

范围：增加全站和用户级黑名单规则的 TypeScript DTO、分页读取、创建、启停、删除方法；管理员设置 DTO 增加 `trustedProxyCidrs`。所有 ID 路径参数 URL 编码，写请求复用统一 CSRF/错误处理层。本任务只提供客户端合同，不修改管理界面。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] 全站和用户范围的 CRUD URL、分页参数与请求体符合 LUX-757 API。
- [x] 写操作通过统一客户端附带 CSRF；类型不包含认证凭证或敏感内部字段。
- [x] `api-client.test.ts` 定向回归与 Web build 通过；不修改页面或样式。

短计划：先为全站/用户范围 API 方法增加 URL、分页、CSRF 与 payload 回归并确认方法缺失；再增加 DTO 和 client 方法；最后跑 API client 定向测试、Web build 和 diff check。

结果（2026-10-20）：新增全站及用户级黑名单规则分页 DTO、创建输入 DTO 和 CRUD 客户端方法，启停使用既有 `PATCH /{ruleId}` 合同；所有用户/规则 ID 均 URL 编码。`AdminSettings` 现包含 `trustedProxyCidrs`。未修改页面。

验证：TDD RED 先因客户端方法不存在而失败；实现后 `pnpm --dir web exec vitest run tests/api-client.test.ts` 81/81 通过，`pnpm --dir web build` 通过，`pnpm --dir web install --frozen-lockfile` 通过。Web 全套为 746/750，4 项失败与已有基线相同：首页预取 1 项、媒体库卡片 2 项、详情轮询 1 项；与本任务无关。`git diff --check` 通过。

#### LUX-765：读取并匹配有界 IP/CIDR 黑名单规则

范围：为后续认证接线提供目标用户的启用全站及用户级 `IP_CIDR` 规则值；查询最多返回 201 项，供应用层在超过每请求 200 项的执行上限时 fail closed。应用层只匹配规范 CIDR，存储值损坏或规则超过上限均返回错误。本任务只建立有界读取和纯匹配合同，不接 HTTP、认证、播放授权或管理界面。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/user_access_rules.rs`、`src/storage/users.rs`、`src/storage/repository.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 读取只包含启用的全站及目标用户 IP 规则，忽略其他用户、停用项和 DEVICE/CLIENT 规则。
- [x] Storage 查询限制最多 201 项；CIDR 匹配覆盖 IPv4、IPv6、未命中、损坏存储值和超过 200 项。
- [x] 相关定向测试、Rust build、fmt 与 diff check 通过；不改变 HTTP 或授权行为。

短计划：先添加 SQLite 规则范围/启用状态/分页上限回归和 CIDR 匹配边界单测并确认缺失接口；再添加最多返回 201 项的 storage 查询与 200 项 fail-closed 纯匹配；最后运行定向 Rust 测试、build、fmt 与 diff check。

结果（2026-10-20）：Storage 以稳定顺序最多读取 201 条启用的全站及目标用户 `IP_CIDR` 规则，应用层最多评估 200 条；损坏 CIDR 或超过评估上限返回错误，供后续认证层 fail closed。未接 HTTP、认证或播放授权。

验证：TDD RED 因缺少规则查询类型和方法而失败；实现后应用层规则/代理测试 11/11、Storage 作用域与启用状态回归 1/1 通过。`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过；严格全目标 Clippy 仍报 13 条已有问题，位于未修改的管理员 API、播放、权限复制及存储测试代码，本任务没有新增 Clippy 诊断。Cargo 命令使用 Toshiba target 目录。

#### LUX-766：从可信代理链解析请求访问 IP

范围：封装请求访问 IP 解析，使用 server middleware 注入的真实连接 peer 和完整 `X-Forwarded-For` 多值；可信代理 CIDR 从管理员设置读取并严格校验。没有真实 peer 时返回 `None` 供调用方 fail closed；代理配置损坏、peer 无效或可信 peer 的转发链无效均返回错误。本任务不接入登录、token 或播放授权检查。

预计修改文件（4 个）：`AGENTS.md`、`docs/LUX-DEVELOPMENT.md`、`src/application/settings.rs`、`src/api/legacy.rs`。

验收标准：

- [x] 未配置可信代理时忽略转发头，直接使用实际 peer；可信代理按完整重复头链从右向左剥离。
- [x] 损坏的可信代理设置、无效 peer 和可信 peer 的畸形转发链均返回错误；缺失 peer 不伪造客户端地址。
- [x] 更新 `AGENTS.md`，只允许访问 IP 解析器在上述可信边界内为 IP/CIDR 黑名单使用转发头；尚不改变授权行为。
- [x] 请求 IP 定向测试、Rust build、fmt 与 diff check 通过。

短计划：先添加 HTTP 层可信/不可信 peer、重复转发头、损坏配置和缺失 peer 回归并确认解析入口缺失；再让可信代理设置读取对损坏值严格报错，新增 request access-IP resolver 并修订 `AGENTS.md`；最后运行定向 lib 测试、build、fmt 与 diff check。

结果（2026-10-20）：`request_access_ip` 只以 middleware 注入的 peer 起算，读取并严格验证全局可信代理 CIDR，消费全部 `X-Forwarded-For` 字段值并复用 LUX-761 的从右向左链解析；无 peer 返回 `None`，无效 peer、可信配置或可信代理链返回错误。可信代理配置损坏不再被误当作空白配置。更新 `AGENTS.md`：只有 IP/CIDR 黑名单解析器可在该边界内参考 XFF，XFP 仍只用于传输元数据。本任务未将解析器接入授权检查。

验证：TDD RED 因请求解析器和错误类型缺失而失败；实现后 `cargo test --locked --lib access_ip_` 5/5、`cargo build --locked`、fmt 与 diff check 通过。严格全目标 Clippy 仍报 13 项既有诊断，均位于未修改的管理员 API、播放、权限复制及存储测试代码；本任务文件没有新增诊断。Cargo 命令使用 Toshiba target 目录。Lucky 官方 Web 服务文档确认提供“追加客户端 IP”设置，但没有说明同名 Header 的合并/覆盖行为；当前部署版本和真实请求链仍未现场核验。

#### LUX-767：在 Web 登录与会话检查中执行 IP/CIDR 黑名单

范围：Web 登录创建会话前和现有 Web 会话认证时，使用 LUX-766 请求访问 IP 与 LUX-765 有界规则匹配检查启用的全站/用户 `IP_CIDR` 规则。命中时拒绝新登录；已有会话立即撤销后拒绝。缺少 peer 且存在活动 IP 规则时 fail closed；规则存储/解析异常返回服务不可用。仅接 Web 登录与 Web session 路径，不覆盖 Emby、播放票据或管理界面专属事件流。

预计修改文件（6 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/mod.rs`、`src/application/user_access_rules.rs`、`src/auth/sessions.rs`、`src/api/users.rs`、`tests/users.rs`。

验收标准：

- [x] 命中用户或全站 IP/CIDR 时 Web 登录不签发会话；未命中地址可登录。
- [x] 活动 Web session 在命中时被撤销；其他地址仍可使用未命中的会话；缺少 peer 且存在 IP 规则时拒绝并撤销。
- [x] 规则超限、损坏值或 storage 错误不得绕过封禁，HTTP 返回服务不可用；直接客户端 IP 及可信代理来源均覆盖。
- [x] users 定向集成目标、Rust build、fmt 与 diff check 通过；不接 Emby 或 playback ticket。

短计划：先在 `tests/users.rs` 建立 Web 登录、现有会话命中、缺少 peer 和未命中地址回归并确认认证仍忽略 IP 规则；再在 `WebAuthService` 增加登录前及会话解析 IP 检查，API handler 注入 LUX-766 解析结果；最后运行 users 定向目标、build、fmt 与 diff check。

验证：`cargo test --locked --test users -- --test-threads=1` 23/23、`cargo build --locked`、fmt 与 diff check 通过。严格全目标 Clippy 仍有 13 项既有诊断，位于未修改的管理员 API、媒体播放、权限复制及存储测试代码；本任务没有新增诊断。Cargo 命令使用 Toshiba target 目录。直接、可信代理、缺少 peer、已有会话撤销及超限 fail-closed 均由 Web 集成用例覆盖。

#### LUX-768：在 Emby 登录与凭证解析中执行 IP/CIDR 黑名单

范围：Emby 按名称/用户 ID 登录签发凭证前，以及每次 Emby 凭证解析时，使用 LUX-766 的可信代理访问 IP 与 LUX-765 的有界规则匹配。命中时不签发新 token；已有 token 命中后撤销并按未认证处理。缺少 peer 且存在活动 IP 规则时 fail closed；损坏规则、规则超限或读取错误不得放行。共享管理员 API Key 不属于普通用户 Emby 凭证，不受本任务影响。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/auth/emby.rs`、`src/api/emby_handlers.rs`、`src/api/legacy.rs`、`tests/emby_auth.rs`。

验收标准：

- [x] 全部 Emby 密码登录入口以可信访问 IP 检查用户/全站 IP 规则；命中不创建凭证，未命中可登录。
- [x] Emby token 每次解析时执行同一规则；命中后凭证被撤销，缺少 peer 时 fail closed，其他地址仍可访问。
- [x] 坏规则、超限或 storage/可信代理解析错误返回服务不可用；管理员 API Key 行为不变。
- [x] Emby 认证集成定向目标、Rust build、fmt 与 diff check 通过；不接 Lux Web API 兼容认证或播放授权。

短计划：先在 `tests/emby_auth.rs` 覆盖可信/不可信来源的登录、已有 token 撤销、缺少 peer 和规则超限，并确认当前 IP 规则尚未限制 Emby；再在 `EmbyAuthService` 增加带 IP 的签发/解析入口并接入 Emby handler；最后运行 `emby_auth` 定向目标、build、fmt 与 diff check。

验证：`cargo test --locked --test emby_auth -- --test-threads=1` 13/13、`cargo build --locked`、fmt 与 diff check 通过。严格全目标 Clippy 仍报 13 项既有诊断，均在本任务未修改位置；本任务文件没有新增诊断。Cargo 命令使用 Toshiba target 目录。集成回归覆盖按名称和用户 ID 登录、可信/不可信代理、缺少 peer、已有 token 撤销、未命中地址、损坏与超限规则、可信代理配置损坏，以及共享管理员 API Key 不受影响。

#### LUX-769：在 Lux API 接受的 Emby 用户凭证中执行 IP/CIDR 黑名单

范围：Lux 原生 `/api/v1` 身份验证兼容 Emby 用户 token 的路径，使用 LUX-766 解析访问 IP，并调用 LUX-768 的逐 token IP 规则检查。命中时撤销 token 并拒绝请求；缺少 peer 且有活动规则时 fail closed。共享管理员 API Key 仍走独立主体解析。本任务不接入已签发的播放 URL/播放票据，也不改用户权限。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/api/users.rs`、`tests/users.rs`。

验收标准：

- [x] Lux API 上由 Emby 用户 token 建立的主体身份，每次解析均检查全站/用户 IP/CIDR 规则。
- [x] 命中时拒绝并撤销 Emby token；未命中继续工作；规则或访问 IP 解析错误 fail closed 返回服务不可用。
- [x] 共享管理员 API Key 不受规则路径影响；users 定向集成目标、build、fmt 和 diff check 通过。

短计划：先在 `tests/users.rs` 建立 Emby token 访问受保护 Lux API 的阻断/撤销与允许地址回归，并确认当前路径会绕过规则；再在 `require_web_principal` 的 Emby token 分支解析访问 IP 并映射服务不可用错误；最后运行 users 定向目标、build、fmt 与 diff check。

验证：TDD RED 时允许地址携带 Emby token 访问 `/api/v1/auth/me` 返回 401；接入 LUX-766 来源解析后新增回归通过。完整 `cargo test --locked --test users -- --test-threads=1` 24/24、`cargo build --locked`、fmt 与 diff check 通过。严格全目标 Clippy 仍有 13 项既有诊断，未在本任务改动文件中产生新诊断。Cargo 使用 Toshiba target 目录。

#### LUX-770：为 Web 播放票据持久化授权设备与客户端

范围：扩展 `web_playback_sessions`，保存会话签发时的可选设备 ID 和客户端名称，并提供按会话/用户写入及读取授权上下文的 storage 合同。为已有数据库增加可空 SQLite/PostgreSQL migration；本任务不改变播放授权或黑名单行为。

预计修改文件（7 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0197_web_playback_access_context.sql`、`migrations-postgres/0197_web_playback_access_context.sql`、`src/storage/repository.rs`、`src/storage/mod.rs`、`src/storage/sessions.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 空库可完成迁移；已有播放 session 的授权上下文保持空值，不破坏既有会话读写。
- [x] storage 可写入活动 session 的设备/客户端摘要，并能有界读取该 session 的用户与上下文；目标用户/状态不匹配时不更新。
- [x] SQLite 定向存储回归、Rust build、fmt 与 diff check 通过；不改变任何 API 响应。

短计划：先在 storage 回归中覆盖活动/错误用户/终态 session 的授权上下文写入和读取，并确认合同缺失；再添加双后端可空迁移、query DTO 及有界读写；最后运行指定 library 测试、build、fmt 与 diff check。

验证：TDD RED 因上下文存取合同缺失而编译失败；新增 `web_playback_access_context_is_scoped_to_active_session` 1/1 通过，并在空 SQLite 数据库上应用完整 migration 集。`cargo build --locked`、fmt 与 diff check 通过。PostgreSQL migration 为对应的可空文本列；本任务没有 PostgreSQL runtime 验证。Cargo 使用 Toshiba target 目录。

#### LUX-771：在已签发播放票据上执行访问黑名单

范围：Lux Web 创建播放会话时，持久化经认证 Web 会话绑定的设备 ID 与 `Lux Web` 客户端名；只有历史空设备关联会话才回退到本次经过 UUID v4 校验的请求设备 ID。通过 Emby 用户 token 访问时，从已认证 token 读取设备/客户端，不接受请求头替换。每次访问 Lux Web 签名直放、Range 或 HLS 票据时，按会话保存的设备/客户端上下文及当前可信代理解析出的来源 IP 检查启用的全站/用户黑名单。命中后停止播放会话、释放准入并使该票据失效；未知设备/客户端但对应规则已启用、缺少 peer 但有活动 IP 规则时 fail closed。损坏、超限规则以及规则或可信代理读取错误返回服务不可用，不得放行。Emby HLS/转码资源和无状态直放签名 URL 不在本任务范围。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/playback/session.rs`、`src/api/playback.rs`、`src/storage/users.rs`、`tests/web_playback.rs`。

验收标准：

- [x] Lux Web 新签发的直放/HLS 票据保存认证会话绑定的设备与客户端上下文；请求头不能替换已绑定设备，只有历史 NULL 会话允许使用有效请求设备头；上下文写入失败时停止新会话且不返回票据。
- [x] 每次 Lux Web 签名播放资源请求均检查会话签发时的设备/客户端规则和本次请求的可信来源 IP；命中撤销会话并返回已结束状态。
- [x] 缺失设备/客户端上下文但存在相应规则、缺少 peer 但存在 IP 规则、损坏/超限规则均 fail closed；规则存储或可信代理读取错误返回 503。
- [x] 覆盖设备、客户端、IP 命中、无 peer、坏规则与超限规则；`web_playback` 定向集成目标、Rust build、fmt 与 diff check 通过。

短计划：先在 `tests/web_playback.rs` 建立多个已签发 Lux Web 票据在新增设备/客户端/IP 规则后的命中、撤销与 fail-closed 回归，并确认当前签名链接仍可用；再为新 Lux Web 会话写入校验后的上下文、在票据授权中检查规则并处理可信请求 IP；最后运行 `web_playback` 定向目标、build、fmt 与 diff check。

验证：先确认已签发票据在新增设备规则后仍返回 200（TDD RED），并确认伪报设备回归在绑定设备被请求头替换时失败；修复后 `cargo test --locked --test web_playback` 1/1、`NO_PROXY=127.0.0.1,localhost cargo test --locked --test playback` 4/4、`cargo build --locked`、fmt 与 `git diff --check` 通过。完整 Rust all-targets 门禁为 1079 通过、4 失败、26 忽略：三项 fill-missing dispatch/retry 测试已在干净基线复现；`scan_local_nfo_page_flushes_actor_credits_and_isolates_invalid_nfo` 在未改动的 metadata 代码中单独复现。严格 Clippy 仍有 13 项既存诊断，均不在本任务新增代码。所有 Cargo 检查使用 Toshiba target 目录。

#### LUX-772：在 Emby HLS 票据上执行访问黑名单

范围：Emby 用户 token 请求创建服务端 HLS 会话时，从已认证凭证读取设备/客户端上下文并保存；只有缺少凭证设备字段时才使用本次 Emby 客户端声明补齐。每次访问已签发 Emby HLS manifest、初始化片段或分片时，检查会话设备/客户端规则和可信来源 IP；命中后停止会话并释放准入，规则变化立即使剩余票据失效。访问 IP、规则读取错误和坏/超限规则不得放行；共享管理员 API Key 的认证行为不变。无状态 Emby 直放票据留待单独任务处理。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/api/playback.rs`、`src/application/playback/session.rs`、`tests/playback.rs`。

验收标准：

- [x] 新签发 Emby HLS 会话持久化经过认证凭证绑定的设备与客户端上下文；上下文写入失败时停止会话且不返回链接。
- [x] 每个已签发 HLS manifest、初始化片段和分片请求均检查设备/客户端及可信来源 IP 规则；命中停止会话并使后续票据失效。
- [x] 缺少上下文但存在对应规则、缺少 peer 但存在 IP 规则、损坏/超限规则及代理/存储错误 fail closed；普通 Emby token 的未命中请求可继续播放。
- [x] `playback` 定向集成目标、Rust build、fmt 与 diff check 通过；共享管理员 API Key 行为保持不变。

短计划：先在 `tests/playback.rs` 添加“先签发 HLS 链接、再启用设备/客户端规则”的失败回归；再保存 token 绑定的授权上下文并让 Emby HLS 资源请求解析可信访问 IP、复用会话规则检查；最后运行 `playback` 定向目标、build、fmt 与 diff check。

验证：TDD RED 时启用全站客户端规则后，已签发 Emby HLS URL 仍返回 200；修复后 `NO_PROXY=127.0.0.1,localhost cargo test --locked --test playback --test web_playback` 中 `playback` 4/4、`web_playback` 1/1 通过。回归同时确认 HLS 会话绑定 token 中的 `transcode-test`/`PlaybackTest`，请求头伪报值不能替换已绑定值，命中后返回 410 并停止会话。`cargo build --locked`、fmt 与 `git diff --check` 通过。严格 Clippy 仍有 13 项既存诊断，没有来自本任务新增代码的诊断；完整 Rust all-targets 失败记录见 LUX-771。Cargo 使用 Toshiba target 目录。

#### LUX-773：建立 Emby 直放播放票据会话表

范围：为 Emby 无状态直放签名票据增加可撤销的会话上下文 schema，按用户和播放会话 ID 保存授权设备/客户端、过期时间及可空撤销时间。此任务只新增 SQLite/PostgreSQL migration 和空库 schema 回归，不改变现有 Emby 直放 URL 或授权行为。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0198_emby_direct_playback_tickets.sql`、`migrations-postgres/0198_emby_direct_playback_tickets.sql`、`src/storage/repository_tests.rs`。

验收标准：

- [x] SQLite 与 PostgreSQL 新建 `emby_direct_playback_tickets` 表，复合主键按用户/播放会话隔离，并保存可空设备/客户端与撤销时间、必需过期/创建时间。
- [x] 时间和字符串约束拒绝空播放会话 ID、负/倒置时间；用户删除级联清除票据上下文。
- [x] 从空库应用完整迁移集并验证列、主键、索引、约束和外键；不改变现有播放授权行为。

短计划：先添加空库 schema/约束回归并确认目标表缺失；再按现有 migration 序列添加 SQLite 和 PostgreSQL 表与索引；最后运行指定 Storage 测试、build、fmt 与 diff check。

验证：TDD RED 因表不存在而在 schema 断言失败；增加双后端 migration 后，`emby_direct_playback_ticket_migration_creates_revocable_context_table` 1/1 通过，覆盖列、有效索引、空/负/倒置值拒绝、复合主键冲突和用户删除级联。空 SQLite 数据库成功应用完整 migration 集；`cargo build --locked`、fmt 与 `git diff --check` 通过。PostgreSQL migration 已编写但未运行 PostgreSQL runtime 验证；未改变直放授权行为。Cargo 使用 Toshiba target 目录。

#### LUX-774：提供 Emby 直放票据上下文存储合同

范围：为 LUX-773 表提供新票据上下文创建、按用户/播放会话读取、只写一次兼容旧票据上下文、撤销及有界过期清理 storage 合同。新记录只保存设备/客户端摘要，不保存签名、原始 token 或完整 URL；本任务不接入 API 或播放授权。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/mod.rs`、`src/storage/repository.rs`、`src/storage/sessions.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 新票据按用户和播放会话复合键创建，支持精确用户范围读取，重复创建不得覆盖已有上下文。
- [x] 兼容旧票据的空上下文只在记录缺失时插入；撤销时间只写一次，查询能区分已撤销票据。
- [x] 过期记录清理接受硬上限 1–1000，不扫描或返回未过期票据；覆盖作用域、冲突、兼容插入、撤销与清理的 SQLite 存储回归通过。
- [x] Rust build、fmt 与 diff check 通过；storage 不接收原始签名或认证凭证。

短计划：先增加新建/读取、用户隔离、旧票据空上下文补写、不可覆盖、撤销一次性和有界清理回归并确认 storage 合同缺失；再新增类型与最小 SQL；最后运行指定库测试、build、fmt 与 diff check。

验证：TDD RED 因缺少 `NewEmbyDirectPlaybackTicket` 类型而编译失败；实现后 `emby_direct_playback_ticket_storage_scopes_and_revokes_context` 1/1 通过，覆盖用户隔离、旧记录兼容写入不覆盖、撤销一次性和 1000 行清理硬上限。`cargo build --locked`、fmt 与 `git diff --check` 通过。storage 接口只接收设备/客户端上下文、用户/播放会话 ID 与时间，不接收 token、签名或 URL。Cargo 使用 Toshiba target 目录。

#### LUX-775：版本化 Emby 直放票据签名

范围：为后续持久化授权上下文的 Emby 直放 URL 增加 v2 签名域与明确版本参数，版本参数必须由签名域保护；服务端识别并验证 v2，同时暂时兼容无版本参数的旧版签名票据直到其自身过期。URL 不新增认证 token、设备 ID 或客户端明文。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/playback/session.rs`、`src/api/emby_catalog.rs`、`src/api/media.rs`、`tests/playback.rs`。

验收标准：

- [x] 新 Emby 直放票据使用独立 v2 HMAC 域，并在 URL 加入受签名保护的版本参数。
- [x] v2 直放票据正常读取；删除/修改版本参数、修改会话或媒体源均拒绝，不能降级按旧 HMAC 验证。
- [x] 无版本的旧 HMAC 链接仍按原签名域验证至过期；签名 URL 不增加原始认证 token、设备标识或客户端名称。
- [x] `playback` 定向集成目标、Rust build、fmt 与 diff check 通过。

短计划：先增加新链接版本标记与去标记降级失败回归并确认当前链接仍被当作旧票据接受；再新增独立 v2 资源编码、URL 参数解析与双版本验证；最后运行 `playback` 定向目标、build、fmt 与 diff check。

验证：TDD RED 时新直放 URL 缺少版本标记，新增断言失败；实现后 `cargo test --locked --test playback -- --test-threads=1` 4/4 通过，覆盖 v2 读取、删改版本及篡改播放会话/媒体源拒绝；`cargo test --locked --lib application::playback::session::tests::emby_direct_stream_v2_signature_is_separate_from_legacy_domain -- --exact` 1/1 通过，确认旧签名与 v2 域不能互验，旧无版本请求保留原验证路径。`cargo build --locked`、`cargo fmt --all -- --check` 与 `git diff --check` 通过。Cargo 使用 Toshiba target 目录。

#### LUX-776：签发 Emby 直放票据时保存授权上下文

范围：为每个新 v2 Emby 直放播放会话，在返回 PlaybackInfo 前保存用户、播放会话 ID、过期时间及认证凭证绑定的设备/客户端摘要。多个媒体源共用同一过期时间和一条会话记录。只有凭证未记录的字段才回退至客户端声明；写入失败时不返回任何包含该票据的响应。GET PlaybackInfo 仅签发/持久化票据，不创建播放并发准入。

预计修改文件（6 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/playback/session.rs`、`src/api/emby_catalog.rs`、`src/api/playback.rs`、`src/storage/sessions.rs`、`tests/playback.rs`。

验收标准：

- [x] 新直放链接返回前，按用户与播放会话保存已认证的设备/客户端上下文及精确匹配的过期时间；查询/写入失败时不返回链接。
- [x] 已绑定 Emby 凭证中的设备/客户端值优先于本次请求头声明；仅缺失字段回退至声明值；不存储凭证、签名或 URL。
- [x] 同一 PlaybackInfo 中的直放媒体源共用播放会话及过期时间；被动协商不写入播放准入记录。
- [x] `playback` 定向集成目标、Rust build、fmt 与 diff check 通过。

短计划：先为 PlaybackInfo 新签发链接增加票据上下文、token 绑定设备/客户端优先级及零准入回归并确认当前表没有记录；再在一个共享过期时间下生成所有来源 URL、于响应前写入单条上下文并处理失败；最后运行 `playback` 定向目标、build、fmt 与 diff check。

验证：TDD RED 因 PlaybackInfo 没有创建 `emby_direct_playback_tickets` 上下文行而失败；实现后 `cargo test --locked --test playback -- --test-threads=1` 4/4 通过，其中回归覆盖认证 token 的 `playback-admin`/`PlaybackTest` 值优先于伪造请求头、所有媒体源 URL 共用数据库过期时间、没有并发准入记录，以及注入数据库写失败时返回 503 且不新增票据。`cargo build --locked`、fmt 与 `git diff --check` 通过。未存储 token、签名或 URL。Cargo 使用 Toshiba target 目录。

#### LUX-777：Emby 直放票据逐请求执行黑名单

范围：在验证新 v2 或兼容旧无版本 HMAC 后，要求播放会话票据记录存在、用户/会话/过期时间一致且未撤销。旧版有效链接首次访问时写入一次 NULL 设备/客户端上下文。每次媒体资源请求都检查签发时绑定的设备/客户端及当前可信来源 IP；命中永久撤销票据、停止已活动的播放记录并释放对应并发准入，解除规则后不得复活。上下文缺失但对应设备/客户端规则启用时 fail closed 并撤销。记录命中维度但不记录凭证、URL 或 IP 明文。

预计修改文件（7 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/playback/session.rs`、`src/api/media.rs`、`src/storage/sessions.rs`、`tests/playback.rs`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/tests/admin-user-activity.test.tsx`。

验收标准：

- [x] 每条直放资源请求先校验签名，再确认存储票据的用户、播放会话、过期时间和撤销状态；v2 缺行拒绝，旧无版本票据在有效期内按原 HMAC 兼容并只写一次空上下文。
- [x] 全站/用户级设备、客户端、IP/CIDR 命中立即撤销直放票据，释放该用户/播放会话的并发准入并停止活动播放记录；规则删除后同一票据仍不可用。
- [x] 缺少设备/客户端上下文且相应规则启用时拒绝并撤销；缺少来源 IP/坏规则/超限规则及存储或代理设置错误 fail closed。
- [x] 命中活动记录包含设备、客户端或 IP 维度，不含 token、签名、URL 或 IP 明文；播放请求定向回归、build、fmt 与 diff check 通过。

短计划：先为已签发直放链接添加客户端/设备/IP 新规则阻断、撤销持久化、解除规则不复活和命中原因回归，并确认当前票据仍然可用；再加入签名后票据状态校验、旧链接空上下文兼容、访问规则检查及撤销/准入清理；最后运行 `playback` 与授权逻辑单测、build、fmt 与 diff check。

验证（2026-10-20）：Rust 授权单测 2/2、`cargo test --locked --test playback -- --test-threads=1` 4/4、`cargo build --locked`、fmt 和 diff check 通过。Web 活动记录定向测试 2/2、`pnpm --dir web build` 通过；完整 Web 套件 746/750，4 项失败位于未修改的首页/媒体库卡片预取、虚拟集合链接和媒体详情离线轮询测试。活动页只显示白名单元数据。Cargo 使用 Toshiba target 目录。

#### LUX-778：同步主规格中的注册策略

范围：将 §3.10 与已批准的用户管理设计同步：邀请注册为默认方式，管理员仍可直接建号，可选公开邮箱注册默认关闭；邀请用户邮箱验证后激活，公开注册用户验证后等待管理员审批。明确待验证/待审批账户不能登录或访问媒体。仅更新规格，不改运行时行为、数据库模型或依赖。

预计修改文件（1 个）：`docs/LUX-DEVELOPMENT.md`。

验收标准：

- [x] §3.10 不再声称系统永久关闭公开注册或后续账户只能由管理员创建。
- [x] 主规格准确记录默认邀请、可选公开注册审批及待激活账户的访问边界。
- [x] 改动仅涉及主规格；模块八仍标记取消。

短计划：先核对设计方案的注册默认值及现有 §3.10 条文；再替换冲突的注册规则并记录本任务；最后检索主规格确认旧规则已消失、模块八状态未改变并运行 diff check。

验证（2026-10-20）：主规格已同步邀请注册默认值、公开邮箱注册开关、邮箱验证与管理员审批顺序，以及待激活账户不可登录/访问媒体的边界。检索确认 §3.10 已无旧的注册限制条文；模块八仍标记已取消。`git diff --check` 通过。未改变代码、数据库模型或依赖。

#### LUX-779：建立安全的 SMTP 邮件传输基础

范围：新增基于 Tokio 与 rustls 的 SMTP 客户端及邮件配置模型。SMTP 主机、端口、TLS 模式、发件人和站点 URL 存入设置文件；认证用户名与密码一起写入 config 目录内权限为 0600 的独立凭据文件，不进入数据库、API DTO、调试输出或日志。仅允许 SMTPS 和强制 STARTTLS，不提供明文 SMTP；邮件地址和配置值在构造传输前校验。本任务只建立传输与安全配置基础，不开放 API、不发送用户邮件、不创建邀请或注册行为。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`Cargo.toml`、`Cargo.lock`、`src/application/mail.rs`、`src/application/mod.rs`。

验收标准：

- [x] 使用 Lettre Tokio/rustls 异步传输；SMTP 模式只能是隐式 TLS 或强制 STARTTLS。
- [x] SMTP 凭据与非敏感配置分开持久化，Unix 凭据文件权限为 0600；删除凭据时清除文件。
- [x] 配置校验拒绝空/超长主机、非法端口、无效发件人或站点 URL，以及未支持的 TLS 模式。
- [x] 测试覆盖配置读写、SMTP 凭据隔离/删除、无效配置拒绝和 TLS 传输构造；不访问外部邮件服务器。
- [x] 不改变现有登录、用户邮箱验证、管理员设置 API 或注册行为。

依赖：LUX-778。SMTP API、队列、邀请和注册后续按独立任务实施。

短计划：先添加安全配置读写、字段校验和 TLS 模式回归并确认失败；再用 Lettre 0.11 的 Tokio/rustls 支持实现配置模型与传输构造；最后运行邮件模块定向库测试、build、fmt 和 diff check。

验证（2026-10-20）：邮件模块定向库测试 6/6、`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。`cargo clippy --locked --lib -- -D warnings` 仍被 11 条位于未修改 API/播放/用户存储文件的既有诊断阻断；本模块无新增 Clippy 诊断。仅构造 SMTP TLS 传输，未连接外部 SMTP 服务。依赖 Lettre 0.11.23，Cargo 使用 Toshiba target 目录。

#### LUX-780：提供管理员 SMTP 设置与连通性检查 API

范围：提供管理员读取/更新 SMTP 非敏感配置、单独保存/删除 SMTP 用户名与密码，以及检查已保存 SMTP 服务连接的 API。设置读取和响应只返回 `credentialsConfigured`，不回显凭据；设置更新不得覆盖凭据文件。连通性检查使用 SMTP `NOOP` 验证连接并要求 TLS，不发送测试邮件；连接失败返回固定脱敏错误，SMTP 错误详情不得进入响应或日志。不实现邮件队列、邀请、注册或 Web 设置 UI。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/application/mail.rs`、`src/api/admin/admin_settings.rs`、`tests/admin_mail_settings.rs`。

验收标准：

- [x] `GET/PUT /api/v1/admin/settings/mail` 读写 SMTP 主机、端口、TLS 模式、发件人和站点 URL；GET 附带配置状态但不返回用户名或密码。
- [x] `PUT/DELETE /api/v1/admin/settings/mail/credentials` 将用户名和密码一起保存到私有凭据文件或删除文件；所有写操作需要管理员权限与 CSRF。
- [x] `POST /api/v1/admin/settings/mail/test` 只对已保存配置执行有界超时的 TLS SMTP `NOOP` 连通性检查，不发送邮件；返回不包含供应商错误、用户名或密码的结果。
- [x] 集成测试覆盖鉴权/CSRF、配置往返与凭据隔离、校验失败、凭据删除和脱敏连接失败。
- [x] 更新 `docs/API.md`；定向集成目标、`cargo build --locked`、fmt 与 diff check 通过。

短计划：先新增管理员邮件设置集成测试并运行定向目标确认 RED；再补应用层连接检查与管理员路由；最后更新 API 文档，运行定向目标、build、fmt 和 diff check。

验证（2026-10-20）：TDD RED 时新接口返回 404；实现后 `cargo test --locked --test admin_mail_settings` 1/1、邮件模块定向库测试 6/6、`cargo build --locked`、`cargo fmt --all -- --check` 与 `git diff --check` 通过。集成回归用本机 SMTP 停滞连接确认 TLS 握手整体在 8 秒 Tokio 超时内退出；普通设置再次保存后凭据文件保持不变，且 API 响应无用户名/密码。`cargo clippy --locked --lib --test admin_mail_settings -- -D warnings` 被 11 项已有诊断阻断，均位于未修改的用户 API、Emby 播放和用户存储文件，本任务文件没有新增诊断。未连接外部 SMTP 服务。Cargo 使用 Toshiba target 目录。

#### LUX-781：接入管理员 SMTP 配置界面

范围：在用户管理全局设置的“邮件”页接入 LUX-780 SMTP API，管理员可保存 SMTP 主机、端口、TLS 模式、发件人和站点地址，单独更新或删除认证凭据，并执行连通性检查。页面只显示凭据是否已配置，不预填或缓存密码；发送测试只检查 SMTP 连接，不实际发送邮件。注册方式、邀请、邮件队列和公开注册流程仍由后续模块二任务实现。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/tests/admin-user-mail-settings.test.tsx`。

验收标准：

- [x] 邮件设置页显示并编辑完整 SMTP 配置；加载、保存失败和保存成功均有可访问状态提示。
- [x] 凭据以用户名/密码组合独立保存；页面不回显旧凭据，保存后清空密码输入，并显示已配置状态；删除凭据有明确操作反馈。凭据请求不进入 React Query mutation cache。
- [x] 连通性检查通过 LUX-780 API，展示成功或脱敏失败消息；操作中禁用重复提交。
- [x] Vitest 覆盖配置加载与保存、凭据不回显/保存/删除、连接检查及错误状态。
- [x] `pnpm --dir web build` 通过；本任务仅修改计划中的 SMTP 页面、API 类型/客户端和测试文件。
- [ ] `pnpm --dir web test` 全量通过。完成时重复结果为 751 passed、4 failed；失败位于既有首页预取、LibraryCard 预取/虚拟集合链接和媒体查询轮询用例，详见验证记录。

短计划：先增加邮件设置页组件测试并确认 RED；再补类型、API 客户端方法和全局设置页表单；最后运行指定 Web 测试与构建并记录结果。

验证（2026-10-20）：组件测试先因 SMTP 密码仍保留在 React Query mutation cache 而失败；改为直接执行凭据请求后，`tests/admin-user-mail-settings.test.tsx` 5/5 通过，并确认 mutation cache 不含密码。`pnpm --dir web build` 通过。完整 `pnpm --dir web test` 连续运行两次，最终均稳定为 751 passed、4 failed：`home-page.test.tsx` 首页媒体库预取 1 项、`library-card.test.tsx` 预取与虚拟集合链接 2 项、`media-query-polling.test.tsx` 图片查询调用次数 1 项；均为既有基线失败，与本任务文件无关。第一次全量运行另有 1 项头像裁切失败，单独重跑通过，第二次全量运行未复现。Playwright 在本机临时配置上确认邮件设置保存成功；390×844 移动视口页面宽度无横向溢出。未执行 SMTP NOOP 或联系外部邮件服务器。

#### LUX-782：建立持久化邮件队列存储合同

范围：新增 SQLite 与 PostgreSQL 邮件 outbox 表及内部存储接口，为之后 SMTP worker 提供持久化排队、原子领取、租约恢复、完成/失败状态和有界分页查询。本任务不启动投递 worker、不提供 API/UI，也不发送邮件。Outbox 仅保存收件地址、模板类型和邮件挑战记录 ID（挑战再关联邀请/注册业务记录）；不保存渲染正文、SMTP 凭据或一次性验证凭证。

预计修改文件（6 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0199_mail_outbox.sql`、`migrations-postgres/0199_mail_outbox.sql`、`src/storage/mod.rs`、`src/storage/repository.rs`（存储测试内置于 repository 模块）、`tests/postgres_database.rs`（更新空库迁移版本断言并检查 mail outbox schema）。

验收标准：

- [x] 新 migration 可从空 SQLite 与 PostgreSQL 数据库运行，定义有限状态、幂等键、尝试次数、下次可投递时间和领取租约，并建立适用索引。
- [x] 存储接口支持幂等入队、原子领取到期项、租约超时恢复、标记完成/失败、排定下一次尝试及有上限的分页查询。
- [x] 持久化行不包含正文、SMTP 凭据或明文一次性 token；错误字段只接受安全错误代码。
- [x] 自动化测试覆盖并发领取、重复入队、租约恢复、状态迁移、分页上限以及空库 migration。
- [x] 运行指定存储库测试、`cargo build --locked`、fmt 与 diff check；Cargo 命令使用 Toshiba target 目录。

短计划：先为 outbox 表和存储合同补回归测试并运行 RED；再新增两种数据库 migration 与存储实现；最后运行目标测试、build、fmt 和 diff check。

验证（2026-10-20）：`cargo test --locked --lib storage::repository::mail_outbox_tests` 5/5、忽略的 PostgreSQL 空库专项 `postgres_mail_outbox_migration_applies_to_an_empty_database` 1/1、`cargo build --locked`、fmt 与 `git diff --check` 通过；所有 Cargo 命令使用 Toshiba target 目录。PostgreSQL 测试确认新库迁移到 schema 199、outbox 有 13 列且不含正文/密码/token 字段。`cargo clippy --locked --lib -- -D warnings` 仍被 11 项既有诊断阻断，分布于未修改的 admin user API、Emby 播放、user permission copy 与 user storage；本任务文件无新增 Clippy 诊断。旧的 PostgreSQL 全 bootstrap 用例已更新 schema 断言，但继续执行时在未修改的 `src/storage/users.rs:1937` 因 `BOOLEAN` 被解码为 `i64` 而 panic；独立 outbox 空库 migration 用例通过。

#### LUX-783：建立注册邮件凭证存储

范围：新增与邮件模板引用关联的一次性注册凭证存储，覆盖邀请和邮箱验证。持久化只保存随机凭证的 32 字节校验摘要、收件邮箱、凭证类型、业务记录引用与有效/消费/撤销时间；不保存原始凭证、密码或邮件正文。本任务不创建邀请或注册业务记录、不发送邮件、不提供 API/UI。

预计修改文件（6 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0200_mail_challenges.sql`、`migrations-postgres/0200_mail_challenges.sql`、`src/storage/mod.rs`、`src/storage/repository.rs`（存储测试内置于 repository 模块）、`tests/postgres_database.rs`（验证空库 migration 和凭证表结构）。

验收标准：

- [x] SQLite 与 PostgreSQL migration 从空数据库建立凭证表；凭证类型有限，摘要必须为 32 字节，邮箱、业务引用和时间字段有界。
- [x] 存储接口支持创建凭证、按 ID 读取、只消费一次、撤销指定目标的凭证；过期、已消费、已撤销凭证不能再次消费。
- [x] 表中仅保存凭证摘要，不保存原始 token、密码或正文；创建与消费校验固定错误，不回显摘要。
- [x] 自动化测试覆盖两种凭证类型、一次性消费、过期/撤销拒绝、摘要长度约束以及空库 migration。
- [x] 运行指定存储库测试、PostgreSQL 空库凭证 migration 测试、`cargo build --locked`、fmt 与 diff check；Cargo 命令使用 Toshiba target 目录。

短计划：先在 SQLite 存储测试和 PostgreSQL 空库测试中写凭证表与消费合同并确认 RED；再新增 SQLite/PostgreSQL migration 和存储方法；最后运行两个定向目标、build、fmt 与 diff check。

验证（2026-10-20）：`cargo test --locked --lib storage::repository::mail_outbox_tests` 7/7、忽略的 PostgreSQL 空库专项 `postgres_mail_challenge_migration_applies_to_an_empty_database` 1/1、`cargo build --locked`、fmt 与 `git diff --check` 通过；所有 Cargo 命令使用 Toshiba target 目录。测试覆盖两类凭证、错误摘要、到期、撤销、单次消费、数据库列约束和空库迁移。`cargo clippy --locked --lib -- -D warnings` 被 11 项既有诊断阻断，均位于未修改的 admin user API、Emby 播放、user permission copy 与 user storage；本任务文件无新增 Clippy 诊断。未发送邮件。

#### LUX-784：建立可重建的注册邮件凭证

范围：为持久化 mail challenge 提供稳定的 HMAC 凭证派生器，使邮件 worker 在重试或重启后重建同一链接 token，同时数据库只保存摘要。派生密钥单独保存在 config 目录 0600 文件；拒绝符号链接、错误权限或非 32 字节密钥。本任务不接入 SMTP 投递、不创建挑战或业务记录、不开放 API。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/mail.rs`（定向测试内置于 mail 模块）。

验收标准：

- [x] 首次读取生成并私有保存 32 字节密钥；后续读取及并发初始化使用同一密钥，拒绝符号链接、错误权限和损坏密钥。
- [x] 凭证按挑战 ID、类型和过期时间进行 HMAC-SHA256 域隔离，URL-safe 且在进程重启后稳定；其他 ID/类型/过期时间得到不同凭证。
- [x] 数据库存储摘要由 SHA-256 计算；验证使用常量时间比较；原始 token 不写入文件或日志。
- [x] 自动化测试覆盖持久化、并发初始化、域隔离、摘要验证和密钥文件权限。
- [x] 运行 mail 模块定向测试、`cargo build --locked`、fmt 与 diff check；Cargo 命令使用 Toshiba target 目录。

短计划：先为稳定派生、密钥文件权限和并发初始化增加失败测试；再实现私有密钥的读写与 HMAC token 派生；最后运行邮件模块测试、build、fmt 和 diff check。

验证（2026-10-20）：`cargo test --locked --lib application::mail::tests` 9/9、`cargo build --locked`、`cargo fmt --all -- --check` 与 `git diff --check` 通过；所有 Cargo 命令使用 Toshiba target 目录。测试覆盖首次私有创建、密钥重载稳定性、并发初始化、权限/符号链接/长度拒绝、凭证域隔离、URL-safe 编码、摘要和验证。全目标全 feature Clippy 仍失败于 15 项既有诊断（11 项库代码、4 项测试代码），均位于未修改的管理员 API、Emby 播放、权限复制或用户存储文件；`src/application/mail.rs` 无 Clippy 诊断。

#### LUX-785：让邮件挑战存储接受预先分配的挑战 ID

范围：使注册邮件挑战的应用层可先用挑战 ID 派生 HMAC token 与摘要，再将该 ID 和摘要一起写入数据库。当前 storage 方法在插入时自行生成 ID，与 LUX-784 的 ID 绑定凭证合同不匹配。本任务只调整内部存储输入，不改变 schema、公开 API 或凭证算法。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`（定向存储测试内置于 repository 模块）。

验收标准：

- [x] 创建挑战的内部输入必须包含调用方预先分配的 ID；storage 校验 ID 并原样持久化，不再在插入后生成 ID。
- [x] 重复 ID 由数据库唯一约束拒绝；空白、控制字符和超长 ID 在进入 SQL 前拒绝。
- [x] 现有挑战摘要、消费、过期与撤销行为保持不变，并有定向回归覆盖。
- [x] 运行邮件挑战存储定向测试、`cargo build --locked`、fmt 与 diff check；Cargo 命令使用 Toshiba target 目录。

短计划：先增加预分配 ID 保留和边界拒绝测试并确认 RED；再把 ID 纳入挑战输入并在 storage 校验、持久化；最后运行定向存储测试、build、fmt 和 diff check。

验证（2026-10-20）：定向存储测试 `cargo test --locked --lib storage::repository::mail_outbox_tests` 8/8、`cargo build --locked`、`cargo fmt --all -- --check` 与 `git diff --check` 通过；所有 Cargo 命令使用 Toshiba target 目录。非法 ID 测试同时断言没有执行 SQL，重复 ID 由主键唯一约束拒绝。全目标全 feature Clippy 仍失败于 15 项既有诊断，均位于未修改的管理员 API、Emby 播放、权限复制或用户存储文件；本任务文件没有新增诊断。

#### LUX-786：投递持久化注册邮件队列

范围：启动有界邮件 worker，领取持久化 outbox 项，按挑战 ID 重建并验证一次性凭证，生成纯文本邀请/邮箱验证邮件并通过已保存的 TLS SMTP 配置发送。成功、可重试失败和不可重试记录分别更新队列状态；不在数据库、文件或日志中保存原始凭证。本任务不创建邀请或公开注册业务记录，也不开放管理队列 API。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/mail_worker.rs`、`src/application/mod.rs`、`src/main.rs`（worker 测试内置于 mail_worker 模块）。

验收标准：

- [x] worker 每次只领取一封邮件，使用租约恢复崩溃项；成功置为 SENT，临时错误按最多 5 次退避重试，过期/撤销/不匹配挑战立即置为 FAILED。
- [x] 仅当 outbox 模板、收件地址、挑战类型、状态和 SHA-256 摘要均匹配时才发送；邀请与验证链接使用 URL fragment 携带短期 token。
- [x] worker 使用已保存 SMTPS/STARTTLS 配置；SMTP 错误、收件地址、邮件正文、token、用户名和密码不进入日志；测试使用假的发送器，不连接外部邮件服务。
- [x] 自动化测试覆盖邮件正文/链接、成功状态、不可重试挑战、SMTP 失败重试、最多尝试次数及正常关闭。
- [x] 运行 mail worker 定向测试、`cargo build --locked`、fmt 与 diff check；Cargo 命令使用 Toshiba target 目录。

短计划：先写邮件链接与状态转移失败测试；再实现消息验证、纯文本渲染、重试和生命周期受控的后台 worker；最后运行 mail worker 定向测试、build、fmt 与 diff check。

验证（2026-10-20）：`cargo test --locked --lib application::mail_worker::tests` 5/5、`cargo build --locked`、`cargo fmt --all -- --check` 与 `git diff --check` 通过；所有 Cargo 命令使用 Toshiba target 目录。邮件正文仅包含纯文本链接；链接 token 位于 fragment。过期挑战不会交给发送器，SMTP 错误在 5 次尝试后转为 FAILED；worker 在关闭信号下及时退出。SMTP 凭据、收件地址、正文和错误详情均不写入日志。此前全目标全 feature Clippy 检查仍有 15 项既有诊断，涉及未修改的管理员 API、Emby 播放、权限复制及用户存储文件；本任务文件没有新增诊断。

#### LUX-787：持久化邀请权限快照与生命周期

范围：为邀请注册建立 SQLite/PostgreSQL 表，保存规范化收件邮箱、可选创建者、明确生命周期状态、7 天有效期、所选权限预设来源和创建时权限快照。支持单次接受、撤销和过期状态；本任务只修改 schema 并验证迁移，不提供 storage API、管理员接口或注册页面，不保存邮件凭证。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0201_user_invitations.sql`、`migrations-postgres/0201_user_invitations.sql`、`tests/storage.rs`、`tests/postgres_database.rs`。

验收标准：

- [x] 邀请行记录邮箱、创建者、状态、有效期、权限预设标识与版本、权限 JSON 快照及接受/撤销时间；创建者和已接受用户删除时保留邀请历史。
- [x] 数据库约束拒绝无效状态、时间和非对象快照；同一规范化邮箱最多有一条待处理邀请，终态记录可保留审计。
- [x] SQLite 与 PostgreSQL 空库迁移建立同等 schema、索引和外键规则；不加入 token、密码或邮件正文列。
- [x] 运行 SQLite 迁移定向测试、PostgreSQL schema 合同检查、Rust build、fmt 与 diff check；Cargo 命令使用 Toshiba target 目录。

短计划：先添加 SQLite 空库和 PostgreSQL 空库的邀请 schema 失败测试；再分别新增迁移及索引/约束；最后运行定向迁移测试、build、fmt 与 diff check。

验证（2026-10-20）：SQLite 空库定向测试 `cargo test --locked --test storage user_invitations_migration_runs_from_an_empty_database -- --exact` 1/1，PostgreSQL 16 空库迁移 `cargo test --locked --test postgres_database postgres_mail_and_invitation_migrations_apply_to_an_empty_database -- --ignored --exact` 1/1；`cargo build --locked`、fmt 与 diff check 通过。测试覆盖待处理邮箱唯一索引、无效状态/非对象快照拒绝、终态后可再次邀请，以及用户删除后邀请历史保留。所有 Cargo 命令使用 Toshiba target 目录，本机 `arm64`。

#### LUX-788：实现邀请存储与邮件入队事务

范围：提供创建、读取、过滤分页和撤销邀请的 storage 合同。新邀请、关联邀请挑战及 outbox 邮件必须在同一事务内写入；创建前把该邮箱已过期的待处理邀请转为终态。列表按服务端上限分页，并将已到期待处理项显示为 EXPIRED。本任务不实现邀请业务校验、公开 API 或 Web 界面。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/mod.rs`、`src/storage/repository.rs`、`src/storage/repository_tests.rs`（含 SQLite 与 PostgreSQL storage 回归）。

验收标准：

- [x] 邀请/挑战/outbox 关联不匹配或任何一项写入失败时整笔事务回滚；重复待处理邮箱返回稳定冲突结果。
- [x] 创建会把同邮箱已过期邀请标记为 EXPIRED；读取和过滤分页返回准确状态/总数，页大小受 100 条上限约束且排序稳定。
- [x] 撤销只作用于尚未过期的待处理邀请；已接受、已撤销、已过期或不存在的邀请不会被重复改变。
- [x] SQLite 与 PostgreSQL 均验证创建、队列原子性、过期替换、冲突、分页和撤销；定向测试、build、fmt 与 diff check 通过。

短计划：先为 storage 写入、分页、终态与原子回滚补充失败回归；再实现事务、硬上限分页及 CAS 撤销；最后运行 SQLite/PostgreSQL 定向目标、build、fmt 与 diff check。

验证（2026-10-20）：`cargo test --locked --lib user_invitation_storage_` 3/3；本机 PostgreSQL 16 定向用例 `cargo test --locked --lib postgres_user_invitation_storage_covers_lifecycle_and_atomicity -- --ignored` 1/1；`cargo build --locked`（最终验证无 warning）、`cargo fmt --all -- --check` 与 `git diff --check` 通过。所有 Cargo 命令使用 Toshiba target 目录，本机 `arm64`。

#### LUX-789：建立邀请注册 application service

范围：为管理员邀请建立 application service，按已选权限预设复制不可变快照，规范化并验证目标邮箱，创建 7 天有效的单次挑战凭证并将邮件原子入队；提供有界、可筛选的邀请分页和待处理邀请撤销。创建结果仅在当前操作返回可复制的相对邀请链接，原始 token 不写入持久化数据或日志。本任务不开放 HTTP API，不实现公开注册/账号激活或 Web 页面，不实际发送邮件。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/mod.rs`、`src/application/user_invitations.rs`（测试内置于新 service 模块）。

验收标准：

- [x] 邮箱在服务层校验并规范为小写；有效邀请必须绑定现存权限预设和创建时权限快照，默认期限为 7 天。
- [x] 创建同时写入邀请、HMAC 挑战摘要和 outbox 项；重复待处理邮箱得到稳定冲突；仅响应包含可复制链接，数据库只存摘要。
- [x] 邀请列表支持状态筛选、稳定排序和最多 100 条分页；撤销只作用于尚未到期的待处理邀请，并沿用 storage 的挑战/outbox 原子失效。
- [x] SQLite 测试覆盖创建快照、链接校验、重复邮箱、状态筛选分页、撤销和 token 不落库；build、fmt、diff check 通过。

短计划：先登记服务合同并添加 SQLite 回归；再实现邮箱/预设校验、7 天快照与邮件入队、分页和撤销；最后运行 `cargo test --locked --lib application::user_invitations::tests`、build、fmt 与 diff check。

验证（2026-10-20）：`cargo test --locked --lib application::user_invitations::tests` 3/3、`cargo build --locked`、`cargo fmt --all -- --check` 与 `git diff --check` 通过；本机 `arm64`，Cargo 使用 Toshiba target 目录。测试未连接 SMTP；服务返回的单次邀请 URL 使用片段携带 token，存储仅保留 HMAC 验证所需的摘要。

#### LUX-790：开放管理员邀请管理 API

范围：在 Lux 管理 API 提供邀请创建、状态筛选分页与撤销端点。创建和撤销要求管理员身份与适用的 CSRF 校验，记录不含邮箱、邀请链接或凭证的审计事件；DTO 仅暴露邀请状态与权限预设来源，不序列化权限快照或挑战摘要。创建响应一次性返回可复制邀请链接。本任务不做邮件重发端点、公开注册 API 或 Web 界面。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/api/admin.rs`、`src/api/admin/admin_user_invitations.rs`、`tests/admin_user_invitations.rs`。

验收标准：

- [x] 管理员可创建邀请、按状态分页查询和撤销；页大小默认 20、上限 100，服务端拒绝无效状态筛选。
- [x] 创建请求经过管理员与 CSRF 校验；重复待处理邮箱返回 409；创建和撤销写入不含 PII、凭证或链接的审计事件。
- [x] 列表 DTO 不包含权限快照、token 摘要或 token；只有创建响应含一次性链接，链接 token 位于 URL fragment。
- [x] HTTP 回归覆盖未认证/缺失 CSRF、创建、重复冲突、状态过滤分页、撤销和响应敏感字段；定向集成测试、build、fmt、diff check 通过。

短计划：先添加 HTTP 集成回归并确认路由返回 404；再添加隔离的 admin handler 模块和显式 DTO，连接 LUX-789 service；最后运行定向测试、build、fmt、Clippy 和 diff check。

验证（2026-10-20）：路由实现前，HTTP 回归因端点返回 404 失败；实现后 `cargo test --locked --test admin_user_invitations` 1/1、`cargo build --locked`、`cargo fmt --all -- --check` 与 `git diff --check` 通过。回归覆盖未认证、缺失 CSRF、邀请创建/重复冲突、默认与上限分页、状态筛选、撤销、DTO 字段缺席和审计脱敏。全目标全 feature Clippy 被 11 项既有库代码与 15 项既有测试代码诊断阻断，均在本任务文件之外。所有 Cargo 命令使用 Toshiba target 目录，本机 `arm64`。

#### LUX-791：原子重新入队有效邀请邮件

范围：为已有邀请提供存储层重发邮件操作：仅对仍待处理且未过期的邀请，找到邮箱、邀请 ID 和有效期匹配的未消费、未撤销邀请挑战，并原子地新增 outbox 项。重复使用同一幂等键不得重复入队；不同幂等键允许代表一次新的发送请求。重发复用原挑战和 token，不延长邀请有效期、不创建新挑战，也不写入正文或凭证。本任务不开放应用服务、HTTP API、速率限制或 Web 界面，不修改 schema。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 有效邀请的重发 outbox 项与邀请、邮箱和现有有效挑战一致，并在一个事务内创建。
- [x] 同一幂等键重复调用仅有一个 outbox 项；不同幂等键可产生独立重发项，但均引用同一挑战。
- [x] 不存在、已接受、已撤销、已过期邀请，以及缺失、已消费、已撤销或过期挑战均不入队；错误幂等键不会污染队列。
- [x] SQLite 与 PostgreSQL 回归覆盖有效重发、幂等、挑战复用和不可重发状态；定向存储测试、build、fmt 与 diff check 通过。

短计划：先在 SQLite 与 PostgreSQL 邀请存储回归中增加重发合同并确认 RED；再实现按邀请/挑战状态校验并原子入队；最后运行两个定向存储目标、build、fmt、Clippy 和 diff check。

验证（2026-10-20）：新增 SQLite 回归先因缺少重发存储方法而编译失败；实现后 `cargo test --locked --lib user_invitation_storage_` 5/5 通过，忽略的 PostgreSQL 生命周期目标 `cargo test --locked --lib postgres_user_invitation_storage_covers_lifecycle_and_atomicity -- --ignored` 1/1 通过。`cargo build --locked`、fmt 与 diff check 通过。全目标全 feature Clippy 仍被 11 项既有库代码与 15 项既有测试代码诊断阻断，均不在本任务新增代码中。所有 Cargo 命令使用 Toshiba target 目录，本机 `arm64`。

#### LUX-792：限制邀请邮件发送频率

范围：对同一规范化收件邮箱实施持久化滚动频率限制：每小时最多排入 5 封邀请邮件，涵盖新邀请和重发；达到上限后新请求不创建邀请、挑战或 outbox 项，并通过 Lux API 返回 429。额度从持久化 outbox 记录计算，发送失败和等待重试也占额度；重复使用同一重发幂等键只返回原结果，不占新额度。此任务不限制公开注册邮件，不开放重发路由，不修改 schema。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/repository.rs`、`src/storage/repository_tests.rs`、`src/application/user_invitations.rs`、`src/api/admin/admin_user_invitations.rs`。

验收标准：

- [x] 新邀请与重发共用每邮箱每小时 5 封上限；并发入队不能超过限额，重复幂等调用不增加邮件数。
- [x] 达到限额后新邀请写入事务回滚、重发不入队，并返回稳定的 RATE_LIMITED/429；窗口过期后可以再次发送。
- [x] SQLite 与 PostgreSQL 回归覆盖上限、窗口恢复、重发幂等和无副作用拒绝；相关存储目标、build、fmt 与 diff check 通过。

短计划：先为存储和应用层限额补充 SQLite/PostgreSQL 回归并确认 RED；再让邀请创建与重发在锁定事务内使用同一 outbox 计数规则，映射为 HTTP 429；最后运行定向存储、应用和 HTTP 回归，build、fmt、Clippy 和 diff check。

验证（2026-10-20）：新增回归先因缺少频率结果类型而编译失败；实现后 `cargo test --locked --lib user_invitation_` 7/7、应用层限额传播 1/1、HTTP 429 映射 1/1、管理员邀请 HTTP 集成 1/1、忽略的 PostgreSQL 生命周期/频率回归 1/1 通过。PostgreSQL 回归还覆盖并发重发下只允许一封进入队列、幂等重试不重复计数及一小时窗口恢复。`cargo build --locked`、fmt 与 diff check 通过。全目标全 feature Clippy 仍被 11 项既有库代码与 15 项既有测试代码诊断阻断，没有诊断指向本任务新增实现。所有 Cargo 命令使用 Toshiba target 目录，本机 `arm64`。

#### LUX-793：开放管理员邀请邮件重发 API

范围：为仍有效的待处理邀请开放管理员重发端点，将现有邀请挑战对应的邮件原子加入 outbox，不创建新邀请或挑战、不延长有效期，也不再次返回邀请 token。请求要求管理员身份、CSRF 和 `Idempotency-Key`；同一幂等键重试返回成功但不重复入队，不同键表示新的发送请求并继续受 LUX-792 的每邮箱频率限制。记录脱敏审计事件。本任务不增加邀请管理 Web 操作，也不实现受邀用户激活。

预计修改文件（6 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/storage/mod.rs`、`src/application/user_invitations.rs`、`src/api/admin/admin_user_invitations.rs`、`tests/admin_user_invitations.rs`。

验收标准：

- [x] `POST /api/v1/admin/user-invitations/{id}/resend` 仅接受管理员和有效 CSRF；缺少或格式错误的 `Idempotency-Key` 拒绝且不写入 outbox。
- [x] 有效待处理邀请排入引用原挑战的邮件并返回 202；重复幂等键不新增 outbox，邀请链接、凭证及有效期不返回或改变。
- [x] 不存在、已接受、已撤销或过期邀请返回稳定状态错误；超出 LUX-792 额度映射为脱敏 429，不产生新邮件。
- [x] 审计事件只包含邀请 ID 与固定事件类型，不记录邮箱、幂等键、邀请链接或凭证；集成回归覆盖权限、CSRF、入队、幂等、拒绝与脱敏。
- [x] 更新 `docs/API.md`；定向集成目标、build、fmt 与 diff check 通过。

短计划：先在管理员邀请 HTTP 集成目标添加重发路由回归并确认 RED；再增加 application service、管理员 handler 和 API 合同文档；最后运行定向集成测试、build、fmt、Clippy 与 diff check。

验证（2026-10-20）：RED 回归在新增路由前收到 405；实现后 `cargo test --locked --test admin_user_invitations` 1/1、`cargo build --locked`、`cargo fmt --all -- --check` 与 `git diff --check` 通过。集成覆盖未认证、缺失 CSRF/幂等键、空白幂等键、重复和冲突键、同邮箱 5 封上限、链接/挑战/期限不变、撤销/接受/过期/不存在邀请拒绝及审计脱敏。全目标全 feature Clippy 仍被 11 项既有库代码和 15 项既有测试代码诊断阻断，诊断均位于本任务文件之外。Cargo 使用 Toshiba target 目录，本机 `arm64`。

#### LUX-794：异步分批删除媒体库

范围：管理员删除媒体库时，在一个短事务中将媒体库置为 `DELETING` 并持久化删除任务，接口返回 `202 Accepted` 和任务 ID。媒体库立即从管理员可用库列表、普通用户及 Emby 的库访问中隐藏，并拒绝新的扫描和写入；后台 worker 取消并等待现有扫描退出，按有界批次提交媒体条目、文件索引和扫描任务数据的删除，全部完成后才删除媒体库行并清理插件配置。原媒体文件保留。管理员可以分页查看任务状态、阶段、已处理记录数和稳定错误码，并重试失败或重启后取消的任务；重试从已提交的数据状态继续。服务重启遵循全局标准，将运行任务标为 `CANCELLED`，不自动续跑。

HTTP 合同：`DELETE /api/v1/admin/libraries/{libraryId}` 返回 `202` 与 `{ job }`；`GET /api/v1/admin/library-deletion-jobs?page=&pageSize=&status=` 返回有界分页；`POST /api/v1/admin/library-deletion-jobs/{jobId}/retry` 返回 `202`。重复提交正在运行的删除请求返回现有任务。删除任务记录不对普通用户开放。

预计修改文件（20 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`migrations/0202_library_deletion_jobs.sql`、`migrations-postgres/0202_library_deletion_jobs.sql`、`src/domain/ids.rs`、`src/storage/mod.rs`、`src/storage/repository.rs`、`src/storage/library.rs`、`src/storage/users.rs`、`src/storage/catalog.rs`、`src/storage/jobs/shutdown.rs`、`src/storage/library_deletion.rs`、`src/application/mod.rs`、`src/application/library_deletion.rs`、`src/api/admin/admin_libraries.rs`、`web/src/lib/api/client.ts`、`web/src/lib/api/types.ts`、`web/src/lib/api/query-keys.ts`、`web/src/features/admin/AdminLibrariesPage.tsx`、`web/src/features/admin/AdminOperationsPage.tsx`。

验收标准：

- [ ] SQLite 与 PostgreSQL 从空库升级后均创建任务表、`ACTIVE/DELETING` 生命周期字段及删除外键索引。
- [ ] 删除请求不等待扫描退出或大级联删除；任务记录和 `DELETING` 状态原子写入，重复请求不会创建第二个活动任务。
- [ ] 删除中的库对管理员可用库列表、用户库列表、Emby 库枚举和条目访问不可见；新扫描被拒绝；已有扫描在后台取消并退出后才开始清理。
- [ ] 大型关系数据按有界批次和独立事务删除，批次结果与进度原子提交；最终事务删除媒体库并完成任务。任务失败或重启后媒体库保持隐藏，可从已提交数据重试。
- [ ] 管理员任务列表分页并显示阶段、已处理数、状态和稳定错误码；Web 接受 `202` 后立即从活动库列表移除该库，并在“任务与日志”显示后台进度及重试操作。
- [ ] 更新 `docs/API.md`；`cargo build --locked`、格式检查、Web 测试/构建及 diff 检查通过。所有 Cargo 命令使用 Toshiba 共享 target 目录。

短计划：先新增双后端迁移和存储任务状态/批次操作；再增加生命周期访问栅栏、后台 worker 与重启取消/重试；随后接入管理员 API 和任务审计；最后接入 Web 删除反馈、任务进度和重试，并运行相应检查。

#### LUX-795：建立管理员邀请 Web API 客户端合同

范围：为管理员邀请页面提供有类型的分页查询、邀请创建、重发和撤销客户端方法。创建响应中的一次性邀请链接仅作为调用结果返回；客户端不持久化链接或 token。重发请求必须传入调用方生成的稳定 `Idempotency-Key`。本任务不实现邀请页面或公开注册流程。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] 邀请状态、记录、分页响应和创建响应具有显式类型；列表过滤与分页参数符合管理员 API。
- [x] 客户端提供查询、创建、重发和撤销方法；写请求使用现有 same-origin/CSRF 行为，重发正确设置 `Idempotency-Key`。
- [x] 客户端测试覆盖路径、方法、查询参数、请求体、CSRF 与幂等请求头。
- [x] 定向 Vitest 与 `pnpm --dir web build` 通过。

短计划：先在 `api-client.test.ts` 添加邀请方法合同并确认 RED；再定义 DTO 类型和四个客户端方法；最后运行定向 Vitest、Web build 与 diff check。

验证（2026-10-20）：新增 API 客户端回归先因列表方法缺失而失败；实现后 `pnpm --dir web exec vitest run tests/api-client.test.ts` 82/82 通过，`pnpm --dir web build`（含 `tsc --noEmit`）与 `git diff --check` 通过。客户端请求字段与管理员邀请 API 的实际 handler/DTO 对齐。

#### LUX-796：实现管理员邀请管理界面

范围：在用户管理中增加注册邀请视图，支持管理员按状态分页查询邀请、指定邮箱并选择权限预设创建邀请、查看当前邮件入队结果、复制一次性邀请链接、重新发送和撤销待处理邀请。创建时沿用所选预设的媒体库权限快照，不额外选择或覆盖媒体库。复制链接时使用邮件设置中的 `siteUrl` 生成绝对地址并保留站点子路径；一次性链接只保留在当前组件内存，不进入 React Query 数据、浏览器持久化存储或日志；复制能力不可用时提供可手动选择的链接。重发失败后再次点击沿用原 `Idempotency-Key`，成功后新的发送请求生成新键。本任务不实现受邀用户激活、公开注册/审批或邮件队列管理页面。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUsersPage.tsx`、`web/src/features/admin/AdminUserInvitationsPanel.tsx`、`web/src/lib/api/query-keys.ts`、`web/tests/admin-user-invitations.test.tsx`。

验收标准：

- [x] 用户管理页提供邀请视图，按状态及服务端页码读取邀请，并显示邮箱、预设、状态和有效期。
- [x] 创建邀请要求有效邮箱和现存权限预设；表单明确展示预设的媒体库访问范围，创建成功显示邮件入队结果和一次性链接。
- [x] 一次性链接仅在组件内存中提供复制/手动选择，不进入查询缓存或浏览器持久化存储。
- [x] 复制链接按邮件设置中的 `siteUrl` 生成绝对地址，并保留反向代理部署的子路径。
- [x] 仅待处理邀请提供重发和撤销；重发幂等键在失败重试时保持一致，撤销需确认，成功操作刷新邀请列表。
- [x] 新增组件回归覆盖筛选、分页、创建/链接复制、重发幂等重试、撤销确认与刷新；定向 Vitest、Web build 和 diff check 通过。

短计划：先新增邀请面板回归并确认 RED；再接入用户管理导航和邀请 API，按状态与页码查询并在组件状态中处理一次性链接及重发幂等键；最后运行定向回归、Web build 与 diff check。

验证（2026-10-20）：邀请组件回归 `pnpm --dir web exec vitest run tests/admin-user-invitations.test.tsx` 4/4 通过，覆盖配置 `siteUrl` 子路径、链接复制及剪贴板不可用时手动选择；`pnpm --dir web build` 和 `git diff --check` 通过。此前依赖安装 `pnpm --dir web install --frozen-lockfile` 通过。隔离的本地浏览器预览确认邀请视图、预设媒体库范围、表格和操作可见，控制台无 error/warning。全量 `pnpm --dir web test` 中 Vitest 755/760 通过；5 项失败为 LUX-794 删除媒体库旧断言，以及未由本任务修改的首页/媒体卡片预取、虚拟合集链接和媒体详情轮询测试。此次未改动其余 LUX-794 文件。

#### LUX-797：原子接受邀请并创建已验证账户

范围：建立受邀用户激活的 storage 合同。凭证摘要仅解析有效的邀请挑战和对应待处理邀请；接受操作在单个事务内验证挑战/邀请状态，创建带已验证邮箱与邀请权限快照的活动用户、写入指定媒体库授权、标记邀请已接受并消费凭证。本任务不开放 HTTP API 或 Web 表单，不实现公开邮箱注册/审批。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/mod.rs`、`src/storage/repository.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 按凭证摘要读取时仅返回未消费、未撤销、未过期且与待处理邀请邮箱/期限匹配的邀请快照，不返回原始凭证。
- [x] SQLite 与 PostgreSQL 均在一个事务内创建已验证活动用户、复制角色/播放/媒体库权限、接受邀请并消费凭证。
- [x] 过期、撤销、重复消费、摘要不匹配或用户名冲突不会留下用户、权限关联、接受状态或已消费凭证等部分写入。
- [x] 定向 storage 回归、`cargo build --locked`、格式检查和 diff check 通过；Cargo 使用 Toshiba 共享 target 目录。

短计划：先在 storage 回归中覆盖成功接受、单次消费、错误凭证和冲突回滚并确认 RED；再增加查找与原子接受 storage 方法；最后运行定向 SQLite/PostgreSQL 回归、build、fmt 与 diff check。

结果（2026-10-20）：SQLite 接受流程在单事务内写入已验证活动账户、角色、下载/播放/转码策略和指定媒体库授权，并同时接受邀请、消费挑战；并发接受仅一个调用成功，重复消费返回不可接受。邮箱或期限不匹配、过期、撤销、错误摘要、用户名冲突和已注册邮箱均不会创建部分账户；授权 FK 写入失败会回滚账户、邀请和挑战状态。定向 SQLite storage 回归 8/8 通过；隔离 PostgreSQL 生命周期与接受回归 1/1 通过，验证 PostgreSQL 权限快照、并发单次消费、注册邮箱冲突及授权失败回滚。`cargo build --locked`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。最新 `cargo test --locked --all-targets` 为 1119 passed、5 failed、27 ignored，库测试失败后未运行后续集成 targets。失败为：三个已在干净基线记录的 fill-missing 测试；`scan_local_nfo_page_flushes_actor_credits_and_isolates_invalid_nfo`（单独复跑仍失败，先前任务已有记录）；`shutdown_cancels_legacy_scan_jobs_in_one_bounded_update`（并行 LUX-794 新增一条 shutdown 更新后实际查询数 10，旧断言期望 9）。全目标 Clippy 被 15 条既有告警阻断（管理 API、播放、权限复制、用户存储和旧测试），未报告本任务新增告警。

### 用户管理剩余实施任务（模块八除外）

按 `docs/USER-MANAGEMENT-DESIGN.md` 与当前实现记录审计后，模块四、模块五的访问执行、模块六以及模块七的服务端存储/认证执行已由此前任务覆盖。仍需补齐邀请接受公开流程、公开注册审批、已登录用户邮箱重验、访问黑名单管理界面与当前管理员自锁保护；并修正模块五索引页中已过期的“尚未拦截”说明。模块八继续废除。本清单不提前实现后续任务，每次仍只执行一个 `LUX-*`。

#### LUX-798：实现邀请接受 application service

范围：消费 LUX-797 的原子 storage 合同，按凭证摘要查找仍有效的邀请，验证 HMAC 挑战，规范化账户资料并使用现有 Argon2id 服务哈希密码，再原子创建已验证活动账户。错误凭证、重放、过期邀请、用户名冲突及已注册邮箱映射为稳定业务错误；不开放 HTTP API 或 Web 页面。

预计修改文件（2 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/user_invitations.rs`（定向测试内置于模块）。

验收标准：

- [x] 仅有效邀请 token 可创建账户；Token、密码和摘要不进入日志或错误响应。
- [x] 用户名、显示名称和密码有界校验，密码仅以 Argon2id 哈希交给 storage。
- [x] 过期、重放、无效签名、用户名冲突和已注册邮箱均返回稳定错误且不改变账户或邀请。
- [x] application 定向回归、`cargo build --locked`、fmt 与 diff check 通过；Cargo 使用 Toshiba target 目录。

短计划：先添加 service 成功/失败与重放回归并确认 RED；再连接 signer、密码哈希与原子 storage 接口；最后运行定向测试、build、fmt 和 diff check。

结果（2026-10-20）：邀请 service 通过挑战摘要查找有效待处理邀请，并再次验证 HMAC；之后规范化用户名/显示名称、限制输入长度，以 Argon2id 哈希密码并调用 LUX-797 的单事务接受接口。无效 token、重放、超长密码、用户名冲突和已注册邮箱不会部分创建账户；冲突时邀请保持待处理。没有在错误中返回 token、密码或摘要。

验证：TDD RED 因 application 输入类型与接受服务缺失而编译失败；实现后 `cargo test --locked --lib application::user_invitations::tests::invitation_acceptance_verifies_token_and_creates_argon2_user_once` 1/1、`cargo build --locked`、`cargo fmt --all -- --check` 与 `git diff --check` 通过。Cargo 使用 `/Volumes/Toshiba/mywork/Lux/target`，本机 `arm64`。`cargo clippy --locked --lib -- -D warnings` 仍被 11 项未修改文件诊断阻断（管理员 API、Emby 播放、权限复制和用户存储）；本任务文件无 Clippy 诊断。未连接 SMTP 或开放 HTTP/UI。

#### LUX-799：开放公开邀请接受 API

范围：新增不需登录的 `POST /api/v1/auth/invitations/accept`，接受 token、用户名、显示名称和密码，并调用 LUX-798。请求体设有上限；响应只报告成功或固定业务错误，不返回 token、邀请快照或其他账户信息。Token 只允许放在请求体，不放在 URL、query、审计或日志。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/api/users.rs`、`tests/users.rs`。

验收标准：

- [x] 未登录用户可完成有效邀请；错误 token、冲突、重复消费及数据库不可用映射为稳定脱敏响应。
- [x] 请求体大小、字段长度与 HTTP body 日志边界受限；成功创建账户后可用新凭证登录。
- [x] HTTP 回归覆盖成功、过期/撤销/错误 token、用户名或邮箱冲突、重复提交、请求体上限和不泄漏凭证。
- [x] 更新 API 文档；定向集成目标、build、fmt 与 diff check 通过。

短计划：先在 `tests/users.rs` 写公开路由合同并确认端点尚未注册（请求返回 405）；再连接 LUX-798 service 与显式 DTO；最后运行定向集成目标、build、fmt 和 diff check。

结果（2026-10-20）：公开端点只接受 JSON 中的 token、用户名、显示名称和密码，路由请求体上限为 16 KiB，DTO 拒绝未知字段。成功只返回 `{ "created": true }`；无效、过期、撤销和重放凭证使用固定错误，用户名或邮箱冲突使用统一脱敏文案，数据库故障映射为 503。请求跟踪不记录 query 或请求体；错误不会返回凭证或密码。

验证：TDD RED 的集成回归确认路由未注册（405）；实现后 `cargo test --locked --test users public_invitation_acceptance_api_creates_verified_login_once` 1/1 通过，覆盖有效接受并登录、错误/过期/撤销/重复 token、用户名和邮箱冲突、16 KiB 限制、数据库不可用及响应脱敏。`cargo build --locked`、`cargo fmt --all -- --check`、`git diff --check` 通过。Cargo 输出写入 `/Volumes/Toshiba/mywork/Lux/target`，Toshiba 已挂载；本机架构 `arm64`。

#### LUX-800：建立邀请接受 Web API 客户端合同

范围：为公开邀请接受端点提供安全 TypeScript 请求类型与 API 方法。Token、密码只作为直接请求参数传递，不进入 React Query mutation cache、浏览器持久化或日志；不实现表单。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [x] 客户端按 POST JSON 调用 LUX-799，错误码具备显式类型映射。
- [x] Token 与密码请求不进入 query cache 或持久化，测试覆盖方法、字段和失败响应。
- [x] 定向 Vitest、Web build 与 diff check 通过。

短计划：先添加客户端合同回归并确认方法缺失；再实现类型和请求；最后运行定向 Vitest、Web build 与 diff check。

结果（2026-10-20）：新增邀请接受输入/响应类型及 `acceptUserInvitation` 方法，通过直接 Promise 调用发送 POST JSON，不使用 React Query 或保存凭据。既有 `ApiError` 保留 HTTP 状态、固定错误码与请求 ID，供页面安全映射。

验证：新增 6 项合同回归先因方法缺失失败；实现后 `pnpm --dir web exec vitest run tests/api-client.test.ts` 88/88、`pnpm --dir web build` 与 `git diff --check` 通过。覆盖成功 JSON、无 URL 凭据、浏览器存储无凭据写入和 400/409/413/503/500 错误映射。

#### LUX-801：实现受邀用户账户创建页面

范围：实现 `/register/invitation` 页面，从 URL fragment 读取 token 后立即清理地址栏，仅保存在组件内存；用户设置用户名、显示名称和密码并调用 LUX-800。页面展示固定错误和成功状态，不暴露邀请是否存在于列表或有效期细节。

预计修改文件（6 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/app/App.tsx`、`web/src/features/auth/AcceptInvitationPage.tsx`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`、`web/tests/accept-invitation.test.tsx`。现有入口实际位于 `app/App.tsx`；公开页面需在 setup/session 查询前识别邀请路由，并向邀请客户端方法传递同源站点子路径。

验收标准：

- [x] URL fragment token 不发送到服务器日志、不存入本地/会话存储，完成或失败后不留在地址栏。
- [x] 表单具备可访问标签、提交中状态、固定错误反馈和创建成功后的登录入口。
- [x] 组件回归、Web build 与 diff check 通过；真实浏览器验证 token 清理和子路径路由。

短计划：先写页面加载、fragment 清理和提交回归；再注册路由与实现表单；最后运行 Vitest、Web build 和本地浏览器检查。

结果（2026-10-20）：公开邀请页在 setup/session 查询前接管根路径和反向代理子路径，StrictMode 下读取 fragment 后立即替换清理地址栏。Token 仅保存在页面内存，密码在成功/失败后清空；直接 API 请求避免 query/mutation cache 与持久化。页面有可访问标签、重复提交保护、固定错误、成功登录入口；子路径及任意尾斜杠均保留正确的 API 前缀。

验证：定向 Vitest（邀请页面、API client、登录页）110/110，Web build 与 diff check 通过。边界 RED 回归证明多个尾斜杠曾使 API 前缀包含注册路径，修正后通过。隔离浏览器验证 fragment 清理、子路径 POST 201、点击与键盘 Enter 提交成功；320/768/1024/1440 视口无横向溢出，成功状态截图位于 `output/playwright/user-invitation/.playwright-cli/`。此前有头浏览器曾一次进入登录页，隔离会话的两种提交方式均未复现，原因未确定；隔离邀请页控制台 0 error/0 warning。浏览器仅用本地生产预览和 mock 响应，服务端创建与登录由 LUX-799 集成回归覆盖；未部署。

#### LUX-802：持久化公开注册模式

范围：在现有 server settings 键值存储中增加 `INVITATION_ONLY`/`PUBLIC_EMAIL` 注册模式读取、更新与默认值，默认 `INVITATION_ONLY`。不开放 API 或 UI。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/settings.rs`、`src/storage/mod.rs`、`src/storage/repository.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [x] 空设置、损坏值和未知值安全回退为邀请制；有效更新可读回且版本并发写入不覆盖新值。
- [x] 无效模式拒绝，不影响邮件设置或其他 server settings。
- [x] 定向 storage/application 回归、build、fmt 与 diff check 通过；无需 schema migration。

短计划：先添加默认、合法/非法值及并发回归；再增加版本化读写合同；最后运行定向测试、build、fmt 和 diff check。

结果（2026-10-20）：注册模式使用现有 `server_settings.registration_mode` JSON 保存模式和单调版本；未配置、JSON 损坏、未知枚举和非正版本均回退为邀请制。写入只接受两个已定义模式，先检查客户端版本，再以原始快照执行单条 CAS；陈旧或并发写入仅一个成功，不覆盖其他设置键。无需 schema migration。

验证：TDD RED 确认模式类型和读写合同缺失；SQLite 与隔离 PostgreSQL 复用的默认/非法值/并发/损坏值修复回归 2/2 通过（`cargo test --locked --lib registration_mode -- --include-ignored`）。`cargo build --locked`、fmt 与 diff check 通过，Cargo 使用 Toshiba target，本机 arm64。库 Clippy 仍被此前已记录的 11 条管理 API、播放、权限复制和用户存储告警阻断；本任务文件没有新增诊断。未部署。

#### LUX-803：开放公开注册模式管理员 API

范围：通过受管理员认证与 CSRF 保护的设置 API 读取和修改 LUX-802 注册模式；响应仅返回模式枚举，不混入 SMTP 凭据。使用独立 `GET/PATCH /api/v1/admin/settings/registration`，响应为 `{ registrationMode, version }`；PATCH 带当前版本，省略模式为无修改，显式 null/未知字段/非法值返回 400，陈旧版本返回 409，请求体上限 4 KiB。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/api/admin/admin_settings.rs`、`tests/admin_settings.rs`。

验收标准：

- [x] 默认与已保存模式可读取；无效枚举拒绝，省略字段不改变当前值。
- [x] 未认证、非管理员及缺失 CSRF 的写请求拒绝；测试确认模式切换不触及 SMTP 凭据。
- [x] 更新 API 文档；定向集成目标、build、fmt 与 diff check 通过。

短计划：先增加管理员设置 HTTP 回归并确认字段缺失；再连接 LUX-802 storage/application 合同；最后运行定向目标、build、fmt 和 diff check。

结果（2026-10-20）：独立注册设置端点只返回 `registrationMode` 和版本；PATCH 调用 LUX-802 的版本化服务，空对象不修改配置，未知字段、null、非法模式/版本和缺少写入版本均固定返回 400，陈旧版本为 409 `VERSION_CONFLICT`。写请求复用管理员及 CSRF 校验，沿用共享管理员 API Key 的既有规则；请求体限制 4 KiB。成功修改写入只含模式/版本的审计事件，不读取或改写 SMTP 凭据。

验证：TDD RED 确认缺失端点返回 404；`cargo test --locked --test admin_settings --test admin_mail_settings` 2/2 通过，覆盖未认证、非管理员、缺失 CSRF、读取/开关/省略、非法输入、版本冲突、大小上限、审计脱敏及邮件凭据不变。`cargo build --locked`、fmt 与 diff check 通过；Cargo 使用 Toshiba target，本机 arm64。库 Clippy 仍被 LUX-802 已记录的 11 条既有告警阻断，本任务路由无新增诊断。未开放公开注册提交/验证接口，未部署。

#### LUX-804：在用户管理中配置公开注册模式

范围：在用户管理全局设置展示注册模式开关，默认邀请制；开启公开邮箱注册时说明申请须经邮箱验证和管理员批准。关闭后不影响仍有效的已发邀请。

预计修改文件（5 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/tests/admin-user-registration-settings.test.tsx`。

验收标准：

- [x] 设置读取/保存状态明确，模式切换成功后更新缓存，错误时保留草稿并可重试。
- [x] 页面清楚显示公开注册默认关闭和审批边界；权限只由管理员设置，不由访客控制。
- [x] 定向 Vitest、Web build 与 diff check 通过。

短计划：先新增设置组件回归；再连接 LUX-803 client 与表单；最后运行定向 Vitest、Web build 和 diff check。

结果（2026-10-20）：用户全局设置的“注册与邀请”页接入模式选择和版本化保存，默认邀请制；说明公开申请须先验证邮箱、等待管理员审批，权限由管理员分配，关闭公开注册不会撤销已发邀请。页面包含读取/保存中状态、无改动禁用、固定错误、草稿保留与重试；版本冲突时重新读取最新版本并保留选择。成功直接更新注册设置查询缓存。

验证：新增 client/组件回归先因方法缺失失败；定向 Vitest（注册设置、邮件设置、API client）103/103、Web build 与 diff check 通过。隔离浏览器以本地生产预览和模拟 API 验证选择、Tab/Enter 保存、保存反馈及刷新恢复；320/768/1024/1440 视口文档宽度均等于视口宽度，控制台 0 error/0 warning。截图位于 `output/playwright/user-registration-settings/.playwright-cli/`；真实 API 认证/CSRF/持久化由 LUX-803 集成测试覆盖。未部署。

#### LUX-805：建立 SQLite 公开注册申请表

范围：新增 SQLite `user_registration_requests` migration，保存邮箱、用户名/显示名称、密码哈希、权限默认快照、申请状态、创建/验证/审核时间与稳定错误状态；不保存原始凭证或密码。状态为 `PENDING_EMAIL`、`PENDING_APPROVAL`、`APPROVED`、`REJECTED`、`EXPIRED`；前两个状态使用邮箱部分唯一索引。申请保留到期时间，待处理状态必须有 Argon2id 哈希，终态清除哈希；审核人和已创建用户外键删除后置空。错误只允许 `EMAIL_CONFLICT`、`USERNAME_CONFLICT`、`MAIL_DELIVERY_FAILED`、`PERMISSIONS_UNAVAILABLE` 或空值。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`migrations/0203_user_registration_requests.sql`、`tests/storage.rs`。

验收标准：

- [x] migration 可从空库和当前 schema 升级；状态及唯一性约束防止重复待处理邮箱申请。
- [x] 表不含明文密码或 token 字段，拒绝非法状态与越界字段。
- [x] SQLite migration 回归、build、fmt 与 diff check 通过。

短计划：先扩展空库/升级 schema 回归并确认表缺失；再新增 migration；最后运行目标 migration 测试、build、fmt 和 diff check。

实施结果（2026-10-20）：新增 SQLite 申请表及状态/时间/字段约束，涵盖五种状态、Argon2id 密码哈希与终态清理、JSON 对象型权限快照（32 KiB 上限）、pending 邮箱唯一索引、到期查询索引和审核人/已创建用户 `ON DELETE SET NULL` 外键。migration 不保存原始 token 或密码。

验证：TDD RED 先确认空库缺表且缺少 migration；`cargo test --locked --test storage user_registration_requests_migration` 2/2 通过，覆盖空库与从当前 schema 升级、既有用户保留、重复 pending 邮箱、状态转移及字段约束、非法外键和删除置空。`cargo build --locked`、`cargo fmt --all -- --check` 与 `git diff --check` 通过；Cargo 使用 Toshiba target，本机 arm64。`cargo clippy --locked --test storage -- -D warnings` 因 lib 中 11 项既有诊断失败，诊断均位于本任务未修改文件，没有指向 migration 或本次新增测试。未实现注册业务接口。

#### LUX-806：建立 PostgreSQL 公开注册申请表

范围：为 LUX-805 增加等价 PostgreSQL migration 与空库/升级合同，不改变 SQLite 行为或注册业务。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`migrations-postgres/0203_user_registration_requests.sql`、`tests/postgres_database.rs`。

验收标准：

- [ ] PostgreSQL 空库与当前 schema 均可迁移；约束、索引和外键动作与 SQLite 等价。
- [ ] 本机 PostgreSQL 定向迁移回归、build、fmt 与 diff check 通过。

短计划：先增加 PostgreSQL schema 断言并确认 RED；再添加 migration；最后运行本机隔离数据库测试和常规检查。

#### LUX-807：建立注册申请原子存储合同

范围：为注册申请提供创建、验证邮箱、管理员分页查询、批准、拒绝和过期清理 storage 操作。批准事务从请求中取出经验证邮箱与密码哈希，按管理员确认的权限快照创建活动账户并终结申请；并发审批只能成功一次。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/mod.rs`、`src/storage/repository.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [ ] SQLite 与 PostgreSQL 的创建/验证/审批事务原子；并发批准仅一个用户成功。
- [ ] 重复邮箱、过期/撤销 challenge、重复审批、拒绝和权限 FK 错误均无部分账户或凭证消费。
- [ ] 分页上限 100，查询只返回申请摘要；定向 SQLite/PostgreSQL 回归、build、fmt 与 diff check 通过。

短计划：先为状态机与回滚写存储回归；再实现双后端查询和事务；最后运行 SQLite/PostgreSQL 定向目标、build、fmt 和 diff check。

#### LUX-808：实现公开注册与邮箱验证 application service

范围：仅在 LUX-802 模式为 `PUBLIC_EMAIL` 时接收邮箱、用户名、显示名称和密码；使用管理员新用户默认权限快照创建申请、生成 EMAIL_VERIFICATION challenge 并原子入队。返回不区分已存在邮箱的固定成功文案，避免账号枚举。验证凭证只保留 HMAC/摘要，邮件失败不激活账户。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/user_registration.rs`、`src/application/mod.rs`（测试内置于 service 模块）。

验收标准：

- [ ] 注册关闭、输入非法、重复邮箱和数据库错误均不泄漏账户存在状态；有效申请不创建可登录用户。
- [ ] 邮件频率上限按邮箱与连接 peer 执行；不信任未经验证的转发头作为 rate-limit 身份。
- [ ] 邮箱验证成功只将申请转为待审批；重复、过期、撤销 token 无副作用。
- [ ] application 定向回归、build、fmt 与 diff check 通过。

短计划：先补注册/验证及重复邮箱回归；再加入有界 service、token 与 outbox 原子流；最后运行定向测试和常规检查。

#### LUX-809：开放公开注册与邮箱验证 API

范围：新增公开注册提交、邮箱验证和验证邮件重发 API。三者均执行有界请求频率、请求体上限及固定脱敏响应；重发不更改原申请权限快照或过期时间。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/api/users.rs`、`tests/users.rs`。

验收标准：

- [ ] 邀请模式下拒绝公开注册；公开模式下提交返回固定响应且用户未验证/未审批前不能登录或访问媒体。
- [ ] 邮件验证、重复提交、重发限额、无效/过期 token 和邮件队列失败均有稳定响应与集成覆盖。
- [ ] Token 不在 URL/query/log 中；更新 API 文档，定向集成目标、build、fmt 与 diff check 通过。

短计划：先添加公开端点合同并确认 RED；再连接 LUX-808 service 与访问限额；最后运行用户认证集成目标、build、fmt 和 diff check。

#### LUX-810：建立公开注册 Web API 客户端

范围：为公开注册、邮箱验证和重发添加类型化 Web client；提交密码和 token 时绕过 query/mutation cache，Token 由调用方放在请求体。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/lib/api/client.ts`、`web/tests/api-client.test.ts`。

验收标准：

- [ ] 请求路径、字段、CSRF/无会话公开语义和错误码映射与 LUX-809 一致。
- [ ] 密码与 token 不进入 React Query cache；定向 Vitest、Web build 与 diff check 通过。

短计划：先写 client RED 回归，再实现显式 DTO/方法，最后运行定向 Vitest 和 Web build。

#### LUX-811：实现公开注册与邮箱验证页面

范围：增加公开注册申请及邮箱验证页面；仅在服务端允许公开注册时显示提交表单。邮箱链接 token 从 fragment 读取后清除地址栏，并只在验证请求期间存于内存。说明邮箱验证后仍须管理员批准。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/App.tsx`、`web/src/features/auth/RegisterPage.tsx`、`web/tests/register-page.test.tsx`。

验收标准：

- [ ] 邀请模式不暴露可提交表单；公开模式表单校验、等待/成功/错误状态可访问。
- [ ] 邮件验证成功显示“等待管理员审批”，token 被清理且不写入持久化。
- [ ] 定向 Vitest、Web build 与真实浏览器路由/fragment 验证通过。

短计划：先增加模式、提交及验证页面回归；再添加页面路由与状态；最后运行 Vitest、Web build 和浏览器检查。

#### LUX-812：开放注册申请审批管理员 API

范围：管理员分页查询已验证待审批申请，并通过单次事务批准或拒绝。批准请求携带管理员最终确认的权限快照；审批会创建活动用户并标记已批准，拒绝会撤销凭证/待投邮件并清除密码哈希。所有写操作需管理员认证与 CSRF，并产生脱敏审计记录。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/api/admin/admin_user_registrations.rs`、`tests/users.rs`。

验收标准：

- [ ] API 仅向管理员展示邮箱、申请时间、邮箱验证状态和拟分配权限，不返回 password hash/token。
- [ ] 未验证申请不可批准；批准并发仅创建一个用户；拒绝和权限错误无部分写入。
- [ ] CSRF、用户枚举、管理员保护、审计脱敏与分页上限均有回归；更新 API 文档并通过定向测试/build/fmt/diff check。

短计划：先加管理员 API 回归并确认路由缺失；再连接 LUX-807 storage/application；最后运行 `tests/users` 定向目标和常规检查。

#### LUX-813：在用户管理页审批注册申请

范围：在用户列表增加待审批入口，展示已验证申请并允许管理员审核拟分配的媒体库、播放、下载和转码权限，再批准或拒绝。仅显示服务端分页结果；每次操作需确认，完成后刷新申请和用户列表。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/client.ts`/`types.ts`、`web/src/features/admin/AdminUserRegistrationsPanel.tsx`、`web/tests/admin-user-registrations.test.tsx`。

验收标准：

- [ ] 待审批空态、分页、预设权限快照/修改、批准、拒绝、加载与错误重试均可访问。
- [ ] 成功审批后列表与用户缓存刷新；失败时保留申请并显示脱敏结果。
- [ ] 定向 Vitest、Web build 与 diff check 通过。

短计划：先写审批面板合同；再接 LUX-812 API 和用户列表入口；最后运行组件回归与 Web build。

#### LUX-814：为已登录用户建立邮箱重验存储事务

范围：对已登录用户邮箱变更创建 EMAIL_VERIFICATION challenge 时，暂不更改当前邮箱；消费有效 challenge 时以单个事务更新邮箱及验证时间、撤销同用户旧 challenge。新邮箱冲突、用户删除/禁用、token 重放或过期均不产生部分更新。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/storage/mod.rs`、`src/storage/repository.rs`、`src/storage/repository_tests.rs`。

验收标准：

- [ ] SQLite 与 PostgreSQL 均原子保留 pending email、验证后切换邮箱并设 `email_verified_at`。
- [ ] 原邮箱在验证前继续有效；新邮箱唯一冲突和无效 challenge 不消费凭证。
- [ ] 定向存储回归、build、fmt 与 diff check 通过。

短计划：先加 pending/consume/冲突/并发回归；再实现双后端事务；最后运行 SQLite/PostgreSQL 定向检查。

#### LUX-815：实现账户邮箱验证 application service

范围：当前登录用户提出邮箱变更时生成有界有效期验证邮件；应用层验证仅消费该用户对应的 challenge。发送限额按账号/邮箱执行；邮件发送失败不改变原邮箱。本任务不开放 HTTP endpoint。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/user_email_verification.rs`、`src/application/mod.rs`（service 回归内置于模块）。

验收标准：

- [ ] 变更邮箱后当前 API 仍返回原邮箱及未完成状态；验证成功后更新至新邮箱并显示已验证。
- [ ] 用户不能消费其他账号 challenge；过期/重放/冲突/速率限制失败稳定且无信息泄漏。
- [ ] service 使用 LUX-814 原子 storage 合同；定向 application 回归、build、fmt 与 diff check 通过。

短计划：先增加邮箱申请、签发与消费 service 回归；再连接 LUX-814 和邮件 outbox；最后运行定向测试及常规检查。

#### LUX-816：开放账户邮箱验证 API

范围：新增受会话认证和 CSRF 保护的邮箱验证邮件创建、重发及 token 消费 API。endpoint 仅接受当前用户对应的 challenge；邮件发送失败不改变当前邮箱。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`docs/API.md`、`src/api/users.rs`、`tests/users.rs`。

验收标准：

- [ ] 未认证/缺失 CSRF 请求拒绝；申请/验证结果不泄漏其他用户或邮箱状态。
- [ ] 仅当前用户的有效 challenge 可更新邮箱；成功、过期、重放、冲突与发送限额均有 HTTP 回归。
- [ ] 定向集成目标、build、fmt 与 diff check 通过，API 文档更新。

短计划：先添加账户资料/验证 HTTP 回归并确认路由缺失；再连接 LUX-815 service；最后运行 `tests/users` 定向目标和常规检查。

#### LUX-817：为账户页接入邮箱验证

范围：账户资料页显示邮箱未验证状态，提供发送/重发验证邮件并处理完成链接；保持原邮箱直到验证成功。Token 仅从 fragment 读取并在地址栏立即清理。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/client.ts`/`types.ts`、`web/src/features/account/AccountPage.tsx`、`web/tests/account-email-verification.test.tsx`。

验收标准：

- [ ] 邮箱变更显示待验证目标和发送结果；重载后由服务端状态恢复，页面不误报已验证。
- [ ] Token 不留在地址栏、cache 或持久化；加载、重发、成功及失败有组件回归。
- [ ] 定向 Vitest、Web build 和真实浏览器验证通过。

短计划：先添加账户邮箱流程回归；再连接 LUX-815 client/service；最后运行组件测试、Web build 和浏览器检查。

#### LUX-818：管理全站访问黑名单

范围：在用户管理全局设置增加全站黑名单视图，分页列出并创建、启停、删除设备、客户端和 IP/CIDR 规则。保存前给出命中会阻止登录、已有 session 与播放授权的明确说明。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUserGlobalAccessRulesPanel.tsx`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/tests/admin-user-access-rules.test.tsx`。

验收标准：

- [ ] 三种规则支持有界分页、创建、启停与确认删除；输入约束与服务端规范化一致。
- [ ] 请求/加载/错误/空态清晰，修改成功刷新；说明 IP 依据仅来自连接 peer 或已配置可信代理链。
- [ ] 定向 Vitest、Web build 与 diff check 通过。

短计划：先补全站规则组件合同；再接 LUX-757/764 已有 client；最后运行定向 Vitest 和 Web build。

#### LUX-819：管理单用户访问黑名单

范围：在用户详情“设备与登录”页增加指定用户黑名单面板，管理该用户的设备、客户端和 IP/CIDR 规则。该页不能读取或修改全站规则或其他用户规则。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUserAccessRulesPanel.tsx`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/tests/admin-user-access-rules.test.tsx`。

验收标准：

- [ ] 仅操作当前用户 scope；设备规则可由已记录设备选择，其他值由管理员明确输入。
- [ ] 创建、启停、删除和失败重试有反馈；规则生效边界明确。
- [ ] 定向 Vitest、Web build 与 diff check 通过。

短计划：先添加用户 scope 组件回归；再接既有用户规则 client；最后运行组件目标与 Web build。

#### LUX-820：防止黑名单规则封禁当前管理员入口

范围：在创建或启用全站及用户级规则前，比较当前管理请求的设备、客户端与可信访问 IP；若规则将阻断当前管理员会话则返回稳定冲突。IP 比较必须调用 LUX-766 可信代理解析结果，不能直接信任 XFF；无法安全判定时拒绝高风险 IP 规则启用。共享 API Key 的既有策略单独保持明确。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`src/application/user_access_rules.rs`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [ ] 设备/客户端/IP 自封禁规则被拒绝，未命中当前入口的规则仍可管理。
- [ ] 代理伪造、peer 缺失、可信代理多跳和 API Key 请求有回归；拒绝不新增或启用规则。
- [ ] 定向集成测试、build、fmt 与 diff check 通过。

短计划：先构造可信/伪造 peer 的规则 API 回归；再在写入前执行访问上下文匹配；最后运行 `tests/users` 目标与常规检查。

#### LUX-821：为用户设备摘要补充可用 IP 归属地

范围：用户设备列表在已配置 IP 位置插件且缓存命中时提供可选归属地摘要；未命中由既有 `IpLocationService` 异步安排查询。未安装插件、私有地址或查询失败保持为空，不阻塞设备查询，不新增外部服务依赖。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`src/api/admin/admin_users.rs`、`tests/users.rs`。

验收标准：

- [ ] Device DTO 仅增加 nullable safe location metadata；不返回原始外部 provider 响应。
- [ ] 缓存命中、未安装插件、非公网地址和 lookup 失败路径均有回归，列表仍有界。
- [ ] 定向 API 测试、build、fmt 与 diff check 通过。

短计划：先增加设备响应归属地映射测试；再连接已有位置缓存服务；最后运行定向用户测试、build 与 diff check。

#### LUX-822：在设备清单展示 IP 归属地

范围：在 LUX-820 设备行中显示可用的 IP 归属地；没有位置数据时显示“未知”，不把展示 IP 或归属地用作 ACL 判定。

预计修改文件（4 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/lib/api/types.ts`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/tests/admin-user-devices.test.tsx`。

验收标准：

- [ ] 设备表安全显示国家/地区/运营商摘要和未知状态；窄屏仍由表格容器滚动。
- [ ] 定向 Vitest、Web build 与 diff check 通过。

短计划：先为有/无归属地添加行级回归；再扩展 DTO 与展示；最后运行设备 Vitest 和 Web build。

#### LUX-823：修正内容限制索引页的过期说明

范围：核对 LUX-692 至 LUX-704 对列表、详情、图片、下载、字幕及播放的访问检查后，删除内容限制页中“媒体访问拦截尚未接入，策略目前不会过滤媒体”的过期文案，替换为准确说明分级/标签限制及索引就绪状态。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`web/src/features/admin/AdminUserManagementPage.tsx`、`web/tests/admin-user-content-restrictions.test.tsx`。

验收标准：

- [ ] 源码核验与回归证明受限媒体入口执行服务端策略；文案不声称未实现或过度承诺。
- [ ] 页面测试确认索引未就绪提示与已启用策略说明互不混淆。
- [ ] 定向 Vitest、Web build 与 diff check 通过。

短计划：先复核访问路径并添加文案断言；再修正面板说明；最后运行定向 Vitest 与 Web build。

#### LUX-824：核对并关闭用户管理模块一至七

范围：按 `docs/USER-MANAGEMENT-DESIGN.md` §5.2 的逐项验收清单，对照实现、自动化测试和 API/兼容性文档，标记模块一至七已完成项与明确限制。模块八维持已取消。执行用户管理相关 Rust/Web 全量门，并分别记录现有基线失败、当前工作区并行 LUX-794 影响、本机架构和未覆盖的真实浏览器/部署证据。

预计修改文件（3 个）：`docs/LUX-DEVELOPMENT.md`、`docs/USER-MANAGEMENT-DESIGN.md`、`docs/API.md`。

验收标准：

- [ ] 模块一至七的每项验收都有可定位的代码/测试证据或明确标记限制；模块八未实现。
- [ ] SQLite、PostgreSQL、用户/API、内容限制、播放/访问、Web 管理全链路和完整门禁记录可复现结果。
- [ ] 设计清单状态与实际证据一致；没有以本机 ARM 结果代替 FNOS/x86_64 或真实客户端验证。

短计划：先建立设计验收项到任务/测试的交叉表；再修正遗漏与状态；最后运行该目标规定的最终门禁并记录限制。

## 29. 参考资料

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
- Lettre async SMTP transport：https://docs.rs/lettre/latest/lettre/transport/smtp/struct.AsyncSmtpTransport.html
- SQLite FTS5：https://www.sqlite.org/fts5.html
- TMDb 开发文档：https://developer.themoviedb.org/docs/getting-started
- FFprobe 文档：https://ffmpeg.org/ffprobe.html
- React：https://react.dev/
- Vite：https://vite.dev/guide/
