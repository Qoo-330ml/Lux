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
- 因为管理员要求回写 NFO，相关媒体目录将以读写方式挂载；媒体目录中的本地图片仍需可读，Lux
  管理的新下载图片统一保存到 /config/metadata/library。
- 默认 SQLite 数据库位于 /config 的本机持久化卷，不位于 SMB/NFS；首次引导也可以选择管理员已准备好的外部 PostgreSQL。
- 兼容性只承诺实施时真实测试并记录版本的 VidHub、SenPlayer 和 Infuse；“对标 Emby”不等于实现 Emby 全部端点。
- 首版运行单个 Lux 实例，不做多节点或高可用；外部 PostgreSQL 只作为可选的共享存储后端，不代表 Lux 首版承诺多节点部署。
- 弹幕首版只面向支持弹幕接口的第三方客户端；不把 Emby 标准字幕端点当作弹幕协议，也不为其他客户端增加 ASS 或转码兜底。

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
- 实时文件事件只触发局部增量扫描；定时全量校验在后台可暂停、可恢复，不锁住前台。
- 本地 NFO 和图片优先，所选刮削器仅补缺；低置信度匹配进入“待处理”。
- 管理员能重新匹配元数据条目并将结果原子地回写到 NFO 和 /config/metadata/library 图片资源。
- Docker 容器重启后，任务、进度、用户、索引和扫描游标均保持一致。

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
- 媒体库没有自定义封面时，首次扫描完成后若该库已有至少 9 个带 poster 的媒体条目，系统自动注册并执行一次 `AUTO_LIBRARY_COVER` 一次性任务；任务从库中随机选择 9 张 poster，按旋转堆叠布局生成封面，并将媒体库名称及类型英文副标题绘制在封面上（电影为 `Movies`，电视剧为 `Series`，混合媒体库为 `Mixed`）。该任务不会因后续扫描、海报数量变化或封面删除而自动再次运行，但管理员可以在“任务与日志”中手动执行它；手动执行只会重新生成自动封面，用户上传的封面始终优先。
- 自动封面生成前后，只要管理员上传了自定义封面，就始终以自定义封面为准，自动生成不得覆盖或替换它。
- 每个媒体库默认实时监听文件系统；新增、修改、重命名和删除事件只触发受影响路径的局部增量扫描。实时监听不是可关闭的媒体库开关；旧版 `realtime_watch_enabled` 字段仅作兼容保留并始终按开启处理。媒体库另有独立的 `realtime_metadata_auto_match_enabled` 开关，默认开启；关闭后，局部增量扫描仍只更新索引。开启时，局部增量扫描完成并确认有可用媒体条目时，按 `FILL_MISSING` 提交受影响条目的在线元数据补全任务。全量校验和元数据任务可独立配置计划；局部增量扫描由实时事件触发，不作为管理员可配置的计划任务。
- 每个媒体库可以配置一组已安装的元数据刮削器并按顺序排序。首位固定为 `PRIMARY` 主刮削器；后续每项可标记为 `SUPPLEMENT` 补充、`BACKUP` 备用或 `BOTH` 补充兼备用。未配置时仍读取本地 NFO 和图片，但不发起在线刮削请求。`BACKUP` 只在此前的主匹配未确定媒体身份时按顺序尝试；`SUPPLEMENT` 在身份确定后只填充缺失的未锁定字段和缺失图片类型；后续来源不得覆盖本地 NFO、锁定字段或更高优先级来源。
- 剧集库和混合库可各自选择一个已安装、已启用且可用的片头片尾数据源；未选择表示不为该库生成或输出片头片尾标记。
  片头片尾数据源必须声明 chapters.detect 或 chapters.lookup；电影库不能选择该来源。混合库只对其中的剧集/分集参与检测。
- 管理员可在“全局策略”中设置媒体库的默认元数据、图像和字幕策略；媒体库可以继承全局默认值，也可以单独覆盖。
- 全局图像策略包括海报、艺术图、横幅图、徽标、缩略图、光盘封面、壁纸开关、每项最大背景图数量和最小下载宽度；媒体库可覆盖这些开关。
- 全局策略支持保守的存储预估，并明确应用范围：仅新内容、刷新选中内容或后台刷新全部内容；全局刮削可选择仅补全或完整刮削，批量刷新必须进入任务队列。
- 不在用户请求路径中扫描目录、读取 NFO、调用 ffprobe 或访问 TMDb。

### 3.2 媒体来源

- 本地媒体来自 NAS Docker 绑定挂载目录。
- `.strm` 文件的第一个非空文本内容被视为原始播放目标，Lux 只清理 BOM 和首尾空白，不改写目标内容。
- Lux 对目标做有限的词法分类：HTTP(S) URL、本地路径、SMB URI、FTP URI 和不支持的其他协议；分类不访问网络。相对路径在真正播放时相对于 `.strm` 文件所在目录解析，绝对路径必须落在配置的媒体库根目录或管理员配置的 STRM 额外允许根目录内；额外允许根目录必须是容器内的绝对目录，不能配置为 `/`。扫描阶段不读取路径指向的媒体。数据库兼容字段仍保存为 `URL`、`PATH`、`OPAQUE` 或 `EMPTY`，其中 SMB、FTP 和不支持协议使用 `OPAQUE`，运行时再按原始目标区分。
- HTTP(S) 目标沿用直接播放语义，但 `DirectStreamUrl` 指向 Lux 播放入口，由 Lux 用播放器 User-Agent 解析有限重定向后返回 307；媒体字节仍由播放器直连最终地址。本地路径在 Lux 进程可安全读取时按本地文件提供；SMB/FTP 目标交给已配置的协议解析器，解析结果必须是 HTTP(S) 地址。未配置挂载或解析器时不得伪造可播放 URL，也不得把 `.strm` 文件本身作为媒体返回；其他协议始终不支持。
- Lux 不负责保护目标中可能包含的令牌或路径信息；管理员应理解目标会暴露给有播放权限的客户端或已配置的解析器。

### 3.3 播放

- 本地媒体的 Web 播放使用 0～4 档服务端计划：档位 0 为 Direct Play，档位 1 为视频/音频 copy 的 Remux，档位 2 为视频 copy、音频转码，档位 3 为硬件转码，档位 4 为软件转码。决策始终优先选择较低档位。
- 档位 1～4 输出会话级 fMP4/CMAF HLS；HLS 清单和分片只存在于播放会话临时目录，不生成永久媒体副本。
- `.strm` 只能使用档位 0。直连或重定向失败时直接返回不支持，不允许 Remux、音频转码、视频转码、HLS、代理媒体字节或在用户请求中对远程目标运行 ffprobe/ffmpeg。
- 本地文件通过带鉴权的 HTTP GET/HEAD 和单区间 Range 请求传输。`.strm` 的本地目标可位于当前媒体库根目录或管理员配置的 STRM 额外允许根目录；目标必须是 canonicalize 后存在的普通文件，不能借助 `..` 或符号链接越界。
- URL 型 `.strm` 在 `Path` 保留原始播放地址，`DirectStreamUrl` 指向受保护的 Lux 视频端点；端点使用入站播放器 User-Agent 和单区间请求解析 HTTP(S) 目标的有限重定向，再返回最终地址的 307，不代理媒体字节。播放 URL 不写死任何 STRM 服务路径；本地路径型 `.strm` 生成受保护的 Lux 视频端点并提供本地文件语义；SMB/FTP 只有在解析器返回 HTTP(S) 地址后才生成该端点；空目标和其他协议不可播放。
- 浏览器原生无法播放时，先尝试已有的客户端 HEVC/MKV fallback；本地文件仍不可播放时再按浏览器能力选择服务端档位 1～4。客户端 fallback 不计入服务端档位。
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
- 成功编辑或匹配后，将 NFO 回写到媒体目录，将选中的 Lux 管理图片写入
  /config/metadata/library/<shard>/<item-id>/；媒体目录已有图片仍保留且优先。
- 新建媒体库首次添加可用根路径并完成扫描后，若媒体库配置了刮削器，自动按主/备用角色和高置信度选择最佳候选，再按补充角色补齐缺失元数据，写回元数据并按该媒体库的图像策略下载所需图片；用户无需逐条进入管理后台确认。
- 手动“扫描媒体库文件”只做文件系统调和、媒体探测和本地 NFO/图片索引，不自动发起在线刮削；管理员可以单独执行“元数据匹配/刷新元数据”。
- 媒体详情页或媒体卡片上的“扫描所在文件夹”只扫描该媒体现有媒体源所在的文件夹；媒体库管理页上的“扫描媒体库文件”才扫描整个媒体库。两者都只做文件系统调和、媒体探测和本地 NFO/图片索引，不自动发起在线刮削。
- 管理员从媒体库入口手动执行“整库元数据匹配”时，使用与新库首次处理相同的自动选择、NFO 写回和图片下载流程；低置信度条目仍进入待处理队列。
- 回写使用临时文件、刷盘和原子重命名；失败时显示可重试状态，不谎报成功。

建议的首版自动匹配门槛：

- NFO 中存在合法 TMDb ID：确认。
- 规范化标题完全一致、媒体类型一致、年份相同或相差不超过 1 年，且最佳候选达到高置信度：可自动确认；多个候选按最高分选择，但最高分与次高分差小于 5 分时进入“待确认”（包括完全同分），避免相近候选被静默选错。
- 其他情况：待处理。

具体分数只属于 Lux 内部实现，不作为 Emby 兼容 API 的公共契约。

### 3.7 弹幕

- 弹幕使用独立的 Lux 弹幕服务和 Emby 兼容弹幕路由，不伪装成普通字幕轨。
- 管理员可以配置一个 Dandanplay 兼容 API 基地址，也可以配置 `huangxd-/danmu_api` 的 API 基地址；地址可包含部署 token 路径。
- 管理员可以在弹幕插件配置中选择允许匹配的媒体库；未选择的媒体库不能创建弹幕匹配任务，空选择表示不匹配任何媒体库。
- 弹幕匹配策略支持使用原始文件名、尝试本地已登记的简体/繁体标题、尝试本地已登记的英文/原始标题；按原始文件名、简繁标题、英文/原始标题的顺序逐个回退，仅在前一个候选没有匹配时请求下一个候选。
- 弹幕文字的简繁转换由上游 `danmu_api` 的部署配置负责；Lux 不引入 OpenCC、不把简繁转换伪装成所有 Dandanplay 兼容服务都支持的请求参数，也不在首版筛选弹幕语言。
- 后台匹配任务优先使用上游 `/api/v2/match`，不支持时回退到 Dandanplay 兼容的搜索、详情和弹幕接口。
- 匹配成功的 XML 弹幕写回视频同目录、同 basename 的 `.xml` 旁车；使用临时文件、刷盘和原子重命名。
- 只承诺支持弹幕接口的第三方客户端可以通过 Lux 的 Emby 接口读取；其他客户端是否识别 `.xml` 不属于 Lux 兼容承诺。
- 首版不实现 Web 播放器弹幕、ASS 写回、Lux 侧弹幕文字转换、实时发送、代理播放或非弹幕客户端适配。

### 3.8 图片

首版必须：

- 海报 poster。
- 背景图 backdrop/fanart。
- 本地图片发现、尺寸读取、缓存标签、HTTP 缓存和缩放接口兼容。
- 缺失时从实际成功或补充刮削器下载并写入 /config/metadata/library/<shard>/<item-id>/；匹配选择时按所属
  媒体库启用的图片类型逐项取第一张可用图片，后续补充来源只填充缺失类型，缺失类型跳过。扫描发现的媒体目录图片仍按本地优先
  规则登记和提供。

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
- 用户可以上传或替换账户头像；头像由服务端校验并持久化到 `/config/user-avatars`，同一账户在不同浏览器登录后可读取相同头像。
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
- Lux 会优先使用 X-Forwarded-For、X-Forwarded-Proto 等反代请求头；远程访问权限不再依据来源 IP 判断。
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

### 3.15 外置插件与刮削器

- Lux 提供安全的插件注册表；插件以标准 `.zip` 插件包放入 `/config/plugins`，服务重启时扫描并加载。插件代码运行在受监督的独立进程中，不直接注入 Lux Rust 主进程。Lux 发布包和 Docker 镜像不包含任何现有插件实现、打包器或插件 ZIP。
- 插件商店使用可配置的 HTTPS 目录地址；目录返回插件元数据和包地址。默认目录源为 `https://github.com/Qoo-330ml/Lux-plugins`，Lux 将其解析为仓库 `main` 分支的 `index.json`；管理员可以在插件商店页面填写其他目录地址。
- 管理员从插件商店安装插件时，Lux 只下载目录声明的 `.zip` 包，限制大小、文件数量、路径、manifest、协议版本、平台入口和声明文件 SHA-256，并在校验成功后原子写入 `/config/plugins`；未经目录声明的地址不得作为下载目标。
- 首个独立插件为 `org.lux.tmdb`，由外部插件仓库发布。它提取 Emby `MovieDb.dll` 的 TMDb 行为，按 Lux 插件协议重写，并保留 Emby 风格的媒体类型、ProviderIds、ImageType、搜索结果和图片结果定义。
- SDK v1 同时支持 `media_probe` 插件类型。`org.lux.strm-media-info` 只接收 Lux 宿主按单个任务提交的已校验 STRM 探测目标，按原始字符串调用 `ffprobe` 并返回受限的 format/stream 结果；插件不能访问 Lux 数据库、媒体根目录或任务对象，宿主负责并发、取消、恢复、结果落库和可选旁车写回。
- 只有已安装、已启用且可用的插件才能被媒体库选择或调度。插件可以声明自己的配置字段；没有配置项的插件不需要展开配置。TMDb 优先使用管理员填写的 API Key，其次使用运行时或历史 Read Access Token，最后使用外置 TMDb 插件自己的默认凭据；任何凭据都不返回 API 或写入日志。
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
  获取已标注结果。两种插件都不能接收媒体路径、数据库或任务对象。
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
- 字幕格式转换、字幕烧录、DRM 和多码率自适应 HLS。
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
- 服务重启后从持久化游标继续。
- 全量校验期间前台 API 读取旧索引，并逐批看到原子更新。
- 临时挂载失效不得立刻删除整个媒体库；先标记根路径不可用并暂停删除判定。

### 5.4 资源目标

- 空闲常驻内存目标小于 300 MB。
- 默认扫描时常驻内存目标小于 750 MB。
- 所有后台队列有界；队列满时合并事件或施加背压，不无限增长。
- ffprobe 默认并发 256，可按媒体库配置 1 至 512；实际运行的单库有效上限为 512、进程全局硬上限为 512，
  并根据 CPU、内存和前台 p95 动态降档。4 核 NAS 的默认有效并发目标为 128，8 核目标为 256，16 核及以上目标为 512；ffprobe 只处理本轮
  fingerprint 变化或新增的 source，未变化 source 不得重复探测。
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

数据库选择只发生在首次初始化、创建第一个用户之前。当前版本不支持已初始化实例在线切换后端，也不自动执行 SQLite 到 PostgreSQL 的数据迁移；后续如需迁移，必须提供显式导出、导入和回滚流程。

限制：

- SQLite 数据库文件必须位于容器本机持久化卷，不得放在 SMB/NFS 上。
- PostgreSQL 地址、用户名和密码属于敏感配置，不得进入日志、普通 API 响应或错误详情。
- PostgreSQL 连接失败时不得自动回退到 SQLite，避免形成两套数据。
- SQLite 和 PostgreSQL 必须各自从空数据库运行完整 migration；搜索实现可以使用后端专用索引，但不得改变 Lux API 语义。

### 6.4 Docker

- 生产镜像为多阶段构建。
- 运行时包含 luxd、Web 静态资源、Jellyfin `jellyfin-ffmpeg7` v7.1.4-3 和必要 CA 证书；不安装普通 Debian `ffmpeg`。
- 非 root 用户运行。
- 支持 PUID/PGID 或文档化的 UID/GID 映射，使容器能读写媒体目录。
- /config 为可写持久化卷。
- 媒体目录必须按需求以读写方式挂载，因为 Lux 要回写 NFO；媒体目录中的本地图片仍需可读，
  Lux 管理的新图片写入 /config/metadata/library。
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
- 主进程的元数据匹配、候选搜索、图片候选和合集请求统一通过媒体库有序刮削器协议；主进程不得直接访问第三方元数据 API。主刮削器和备用刮削器负责身份匹配，补充刮削器只对已确认条目请求缺失字段或图片能力。
- 插件内部使用统一 HTTP client、超时、16 并发配额、每秒 32 次请求限流、重试和 User-Agent。
- 插件 stdin/stdout RPC 支持有界多路复用；响应按 request ID 分发并允许乱序返回，插件进程故障或超时会结束其全部 pending 请求。
- 404、429、5xx、网络超时分类处理。
- 搜索候选短期缓存，详情较长时间缓存。
- 响应 schema 验证后进入领域层。
- 自动匹配和手动重新匹配共用候选模型；候选的 provider ID、provider 名称和实际 scraper 来源必须一致。补充候选不得静默改变已确认的媒体身份。
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

指定条目的批量重新识别仍使用持久化任务队列：管理员一次提交 1-100 个条目，服务端去重后以 `QUEUED` 创建任务并在后台逐条处理；每条记录 `PENDING/RUNNING/COMPLETED/FAILED`、候选数量和稳定错误代码，任务通过 `GET /api/v1/admin/metadata/reidentify/{jobId}` 查询。条目级失败不会把整批伪装成基础设施失败，父任务以 `COMPLETED_WITH_ISSUES` 完成；只有任务无法收尾等基础设施故障才使用 `FAILED`。刮削器暂不可用的批次可以使用 `DEFERRED` 表示延后。该指定条目接口只负责重新搜索并生成 pending 候选，供管理员处理；失败、有问题或延后的任务可通过 `POST /api/v1/admin/metadata/reidentify/{jobId}` 重新排队未完成条目。

每个元数据任务持久化 `job_scope`（`ITEMS` 或 `LIBRARY`）和可选的 `library_id`；指定条目任务明确使用 `ITEMS`，同库条目仍记录其媒体库身份，整库任务明确使用 `LIBRARY` 和真实媒体库身份。历史任务默认按 `ITEMS` 处理，不根据条目数量推断范围。单进程内同一时刻只允许一个活动的整库元数据任务，服务重启后遗留的 `RUNNING` 条目重新进入 `PENDING`。

媒体库级“整库元数据匹配”使用同一持久化队列，但默认以 `FILL_MISSING` 自动处理：逐条使用所属媒体库的主刮削器和按顺序启用的备用刮削器搜索候选，达到高置信度时自动选择最佳候选，再调用补充刮削器填充缺失的未锁定字段和图片，按媒体库图像策略下载图片并原子写回 NFO/图片；低置信度条目只保留候选并进入待处理状态。新建媒体库首次扫描完成后也自动提交该队列。

实时增量扫描默认更新索引并为受影响条目提交 `FILL_MISSING` 元数据任务；媒体库关闭 `realtime_metadata_auto_match_enabled` 后才只更新索引，不再自动补全。不论开关状态，任务都只处理本次受影响且仍可用的媒体条目，不对整库重新刮削。NFO、图片和其他旁车文件的写回事件不得直接导致同一条目无限重复提交；已完整补全的条目由元数据任务跳过。

全局元数据刷新使用同一持久化队列，模式为 `FILL_MISSING` 或 `FULL_REFRESH`。仅补全只写入缺失的未锁定 NFO 字段和图片，并按补充刮削器角色补齐；完整刮削刷新主来源的未锁定在线字段，再由补充来源填充仍缺失的字段，但不覆盖锁定字段或更高优先级来源。未配置刮削器的条目跳过在线请求并保留本地结果。

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
- URL 型目标由 `PlaybackInfo` 返回 Lux 的受保护播放入口；播放入口使用入站播放器 User-Agent 请求原始目标并有限跟随重定向后返回 307，路径型和其他目标不得被标记为已解析的 HTTP URL，需由路径转发或解析器策略决定播放方式。
- 下载路径按 LUX-091 使用独立的 URL 安全策略和上游流式转发，不能把路径型目标直接当作远程 URL 请求。

### 14.3 PlaybackInfo

只声明实际能力：

- SupportsDirectPlay = true。
- SupportsDirectStream 按首版实际播放入口实现返回 true；Transcoding 返回 false。
- MediaSources 包含版本、容器、码率、大小、时长、流列表、章节和直放 URL。
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
- 内嵌字幕由客户端从媒体容器读取；若某目标客户端强制请求提取端点，再以兼容性探针决定是否用 ffmpeg 做无转换抽取。

### 14.5 弹幕兼容

- Lux 提供独立的 `/api/danmu/{itemId}` 和 `/api/danmu/{itemId}/raw` 读取端点，使用 Emby token 和媒体库 ACL。
- XML 来自已登记、已通过媒体根路径约束的同名旁车；请求不执行上游搜索、整库扫描或 XML 写回。
- `option=Refresh` 只刷新已登记旁车的索引；`option=GetJsonById` 作为已知 Emby 弹幕插件兼容别名，不承诺把 XML 转成通用 JSON。
- 支持弹幕接口的客户端以真实兼容性测试为准；不支持弹幕接口的客户端继续按自身能力处理或忽略该 XML。

### 14.6 Web 播放

- Web 播放通过独立的 `/api/v1/playback/sessions` 会话接口创建一次播放计划；Web API 与 Emby 播放接口、DTO 和领域类型分离。
- 会话计划使用 `tier: 0..4` 和 `plan.kind: DIRECT | SERVER_HLS | UNSUPPORTED` 的判别联合；播放地址为短期签名 URL，不能要求 `<video>` 或 HLS 请求携带 Lux Cookie。
- 档位 0 使用原生 Range 直放或现有客户端 fallback；档位 1～4 使用服务端 fMP4/CMAF HLS。Safari 使用原生 HLS，其他支持 MSE 的浏览器使用 Web HLS 播放器。
- 创建会话时固定媒体源、音频/字幕选择、起播位置和服务端计划；seek 必要时切换会话生成代次，不把客户端任意路径或外部 URL 交给服务端执行。
- `.strm` 只能返回档位 0；直连失败或浏览器不支持时直接展示错误，不创建 ffmpeg 进程。
- 记录开始、定时进度、暂停、心跳和停止；事件带有幂等 `eventId` 与单调 `sequence`，服务端使用数据库媒体时长计算已看状态。
- 服务端 HLS 会话必须有界：独立进程组、stderr drain、临时目录配额、Remux/硬件/软件并发限制、心跳超时回收、孤儿目录清理和低磁盘拒绝策略。
- 不实现 DRM、字幕转换/烧录、多码率自适应 HLS 或 `.strm` 服务端代理。
- LUX-184 允许提供独立的浏览器媒体能力探针，用于实测原生 video、MediaCapabilities 和 WebCodecs；探针不接入
  正式播放路径，不读取或保存用户媒体数据。
- LUX-185 可为 MP4/fMP4 的 HEVC 媒体增加浏览器端 WASM 解码、H.264 客户端编码和 MSE 播放 fallback；重型工作
  必须在 Web Worker 中执行，服务端只继续提供原始媒体 Range 数据。
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
- IncludeItemTypes、ExcludeItemTypes。
- Recursive。
- StartIndex、Limit。
- SortBy、SortOrder。
- Filters、IsPlayed、IsFavorite。
- Years。
- Fields。
- EnableImages、ImageTypeLimit。
- `/Persons` 使用 `ParentId` 指定媒体库；`Recursive=true` 聚合媒体库所有后代媒体条目，`Recursive=false` 只聚合直接子条目，未传 `Recursive` 时按递归查询处理以兼容旧客户端；`PersonTypes=Actor` 返回去重后的演员。人物 DTO 使用 `Type=Person`，并提供 `ServerId`、`ImageTags`、`BackdropImageTags`。响应必须保持 Emby 的 `Items`、`TotalRecordCount` 结构且不额外返回 `StartIndex`；接受任意正整数 `Limit`，不额外施加服务端上限；`Fields`、`SortBy`、`SortOrder` 必须在数据库分页前生效；`DateCreated` 使用演员首次出现在该媒体库媒体条目中的最早 `added_at`。人物关系由持久化索引提供，不能在请求中扫描 metadata 目录。
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

管理员设置中的 `strmAllowedRoots` 是服务器级 STRM 本地播放额外允许根目录列表，默认为空；媒体库根目录始终自动允许。列表中的值必须是非根的绝对目录，播放时仍会 canonicalize 配置目录和目标文件，并要求目标是普通文件。该配置不改变 `.strm` 原始内容，也不实现路径映射。

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
- 账户设置可调整首页媒体库顺序；顺序按用户持久化到服务端，并同步用于 Web 与 Emby 兼容视图。
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
- 媒体库列表和编辑。
- 全局策略：元数据、图像和字幕默认值，刮削模式，以及应用范围和存储预估。
- 路径选择/输入、读写检测。
- 扫描计划与元数据计划，明确分开。
- 扫描/任务页。
- 任务与日志页集中查看所有已注册的任务、运行记录和脱敏日志。任务不能由 Web 管理员凭空创建，只能由 Lux 系统或插件注册；管理员只能维护已注册任务的立即执行、计划、启停和资源配置。
- 空库初始没有注册任务。创建媒体库时由系统原子注册“全量校验媒体库”和“元数据刮削”两个任务；插件启用后注册插件提供的计划任务。所有已注册任务都由管理员在任务页支持立即执行和配置或清除 Cron 执行时间；实时增量扫描由文件系统监听触发，不注册为计划任务。
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
- Lux 同时将同一份 JSON 结构化日志按 UTC 日期写入配置目录的 `logs/lux.YYYY-MM-DD.log`；日志目录随 `/config` 持久化，容器重启后保留历史文件。管理员选择单日时下载原始 `.log` 文件，选择多日时下载包含每日文件的 ZIP。
- 字段包含 timestamp、level、target、requestId、jobId、libraryId、itemId、errorCode、durationMs。
- 不记录密码、token、Cookie、完整外部 URL。
- 路径在管理员日志中可显示相对路径；对普通用户不显示磁盘路径。
- 登录失败以适合 Fail2Ban 或其他日志工具解析的稳定事件码记录。
- 管理员可以按 UTC 起止日期导出日志；单日返回原始 `.log` 文件，多日返回 ZIP；导出最多覆盖 31 天，只包含已存在的日文件，不提供普通用户访问。

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
| LUX-161 | src/application/strm_target.rs、src/storage/mod.rs、src/api/mod.rs、web/src/features/admin/AdminSettingsPage.tsx、web/src/lib/api/types.ts、tests/、web/tests/、docs/ |
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

范围：参考 qmby 的 `ParseMediaName` 和刮削器候选策略，在 Lux 应用层提供统一的文件名/目录名解析与标题清洗。解析结果至少包含清洗后的标题、年份、季号、集号、版本和清晰度；支持 `SxxEyy`、`x` 格式、中文“第 N 季/第 M 集”和年份紧贴标题的常见命名。去除分辨率、编码、音频、字幕、来源、发布组等技术噪声，但保留可用于媒体源聚合的版本和清晰度字段。兼容 Emby 常见的 `[tmdbid=123]`、`[tmdbid-123]`、`[tmdb=123]`、`[tmdb-123]` 及对应 `{...}` 标签；标签从标题中剥离并保存为 provider ID，TMDb 刮削器可直接用该 ID 获取详情。

元数据匹配和搜索必须使用媒体库所选刮削器，并按媒体类型分流；TMDb 刮削器的电影调用 `/search/movie`、剧集调用 `/search/tv`。带年份搜索无结果时允许回退无年份搜索，并对中文/英文标题候选逐项尝试。Lux 扫描、候选搜索、批量重新匹配和各刮削器插件使用同一解析语义；插件 RPC 公开字段保持兼容，不泄露凭据。

验收：

- [x] `暗夜与黎明2024` 清洗为标题“暗夜与黎明”、年份 2024；`暗夜与黎明 S01E01 H 265 AAC CHDWEB` 不把技术标签写入标题。
- [x] 统一解析器覆盖电影、剧集、季度、单集的年份/季集号和常见技术标签，并保留版本/清晰度信息。
- [x] MOVIE 候选和重新匹配请求只调用 `/search/movie`，SERIES 请求只调用 `/search/tv`；TV 搜索支持中文结果缺字段时的英文逐字段回退。
- [x] 电影和剧集目录/文件名中的 Emby 风格 TMDb ID 标签可被识别、持久化并在选择 TMDb 刮削器时直接请求对应详情；手动改用冲突标题或年份时不复用旧 ID。
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
- URL 型 `PlaybackInfo` 不访问目标，`DirectStreamUrl` 返回 Lux 播放入口；视频播放入口用入站客户端 User-Agent 直连目标、有限解析重定向并返回 307，确保 302 服务收到客户端 User-Agent。下载端点的远程请求按 LUX-091 单独执行。
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
- 人物索引任务的游标、进度、取消标记和状态持久化；进程重启会把 `RUNNING` 任务恢复为
  `QUEUED`，取消或完成后不会重复领取已处理条目。
- `people.json` 内容指纹未变化时跳过关系表的 DELETE/INSERT；关系更新和指纹状态在同一事务中提交。
- 人物详情查询使用 `person_credits(person_id, item_id)` 和可见媒体条目索引，避免为每个请求重复扫描大表。

验证：60k 基准；专项测试覆盖 keyset 分页、重启恢复、取消后续请求可继续、指纹跳过和查询索引。

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

范围：为 `org.lux.tmdb` 插件增加首选语言、语言回退开关、有序回退语言列表、标题别名替换和替代 API 地址配置。语言选项来自 TMDb 的主翻译语言列表，界面按简体中文、其他中文地区语言、其他语言排序；非敏感配置持久化到 `/config/tmdb_settings.json`。插件对电影、剧集、季度和单集详情按首选语言请求，并在回退开启时按选择顺序逐字段补全；标题别名替换开启且中文首选语言没有中文标题时，使用 TMDb `alternative_titles` 返回的第一个 `CN` 别名；替代 API 地址开启后使用管理员保存的地址。

验收：

- [ ] TMDb 插件配置返回语言下拉选项；首项为简体中文 `zh-CN`，其次为 `zh-SG`、`zh-HK`、`zh-TW`，之后为 TMDb 主翻译语言；默认首选为 `zh-CN`。
- [ ] 管理员可以保存语言回退开关和多个有序回退语言；默认预选 `zh-SG`、`zh-HK`、`zh-TW`，配置重启后保持，API 不返回任何凭据。
- [ ] 回退开启时，电影、剧集、季度、单集元数据只补全空字段，并严格遵循选择顺序；关闭时不发起回退请求。
- [ ] 标题别名替换默认关闭；开启后电影和剧集在中文首选语言返回非中文标题时尝试使用第一个 `CN` 中文别名，已有中文标题和别名接口失败时保持原值。
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
和媒体库 `probeConcurrency` 的较小值限制并发；任务支持分页列表、详情、取消、重试，并在服务
重启后恢复 PENDING/RUNNING 状态。探测结果保存到 `media_sources`/`media_streams`，旁车写回使用同目录
`*-mediainfo.json` 的 MediaInfoKeeper 兼容子集和临时文件原子替换。缩略图只针对 STRM，使用同目录
`*-thumb.jpg`；截图前先用 `ffprobe` 获取 duration，再调用 `ffmpeg` 在 `thumbnailPositionPercent` 指定的百分比位置输出一张受限尺寸的
JPEG，并将该文件同时登记为 `POSTER` 和 `THUMB`。媒体信息和缩略图是两步独立命令，不引入 FFmpeg 原生库；截图只补全缺少有效主图的 STRM，不
覆盖已有有效缩略图。只开启缩略图时不保存完整媒体信息，但仍会执行轻量 duration 探测。

STRM 截图采用“本地/在线主图优先、视频截图兜底”的顺序：数据库按媒体条目持久化
`poster_fallback_required` 标记。新增 STRM 没有本地 `POSTER` 或 `THUMB` 时设置该标记为 true；
媒体库未配置刮削器、所选刮削器没有候选、候选没有可用主图时，都保留该标记。发现本地
`POSTER`/`THUMB` 或刮削器成功写入任一主图时清除该标记。STRM 截图阶段只处理该标记为 true
且没有有效 `THUMB` 的 STRM 来源，不要求先找到在线条目。FFmpeg 截图成功后写入同目录
`*-thumb.jpg`，并用同一文件同时登记 `POSTER` 和 `THUMB`，来源为 `STRM_FFMPEG`。后续刮削器
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
- [ ] 插件详情页展示并保存媒体库多选、并发数、媒体信息开关、缩略图开关、缩略图位置百分比、已有媒体信息处理方式、旁车写回和五段式 cron 配置；配置文件原子保存且权限受限，插件列表回显非敏感值；任务与日志页可以修改同一份 STRM 计划。
- [ ] 同一时间的有效探测数不超过任务全局并发和媒体库 `probeConcurrency`；单个 URL 失败只影响对应源，任务可继续。
- [ ] 服务重启可以恢复 PENDING/RUNNING 任务；取消不会领取新源，失败或取消任务可以重试。
- [ ] 成功结果写入媒体源和媒体流；`writeSidecars` 启用时写入兼容旁车，失败不会留下半个 JSON。
- [ ] `mediaInfoEnabled` 和 `thumbnailEnabled` 可以独立生效；缩略图缺失时先由 ffprobe 获取 duration，再由 ffmpeg 在 `thumbnailPositionPercent` 指定的位置生成同目录 `*-thumb.jpg`，默认位置为 30%，已有有效缩略图不会被覆盖。
- [ ] STRM 截图遵循本地/在线主图优先顺序：没有刮削器、刮削器无候选或候选没有主图时持久化 `poster_fallback_required`；ffmpeg 不要求在线匹配成功，只消费该标记和缺失图条件；截图成功后将同一文件登记为 `POSTER` 与 `THUMB` 并清除标记，后续刮削器获得图片时可按类型替换 `STRM_FFMPEG` 兜底图。
- [ ] 插件启用后自动出现全局 `STRM_MEDIA_INFO` 注册任务；任务按有效 `schedule` cron 表达式执行，禁用插件后不再领取新作业，重启服务后仍可恢复。
- [ ] 实时增量扫描完成后，所选媒体库中新入库或发生变化的 `.strm` 来源自动创建定向 STRM 探测任务；定向任务只处理本次增量扫描影响的来源，并支持重启恢复、取消和失败重试。
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
生命周期、已索引本地视频的安全分页、通用任务日志/进度/取消/重试/重启恢复、媒体库 ACL、
受限的旁车写入能力和 Emby 兼容弹幕端点。

插件声明 `type: "danmaku"`、`category: "MEDIA"`、`danmaku.match` 能力和统一 RPC 方法
`danmaku.match`。主进程向插件发送已经过路径和媒体库 ACL 校验的本地视频描述及任务选项，
插件返回结构化匹配状态和受大小限制的 XML；插件不得接收或执行用户直接提供的上游 URL，
不得访问主进程配置目录以外的凭据。管理员配置 Dandanplay 兼容 API 基地址，或配置
`huangxd-/danmu_api` 的 API 基地址，配置由插件 manifest 声明并通过插件配置界面保存。
基地址可以包含部署 token 路径，必须保留路径但在配置响应、日志、审计和错误中脱敏。
XML 旁车只登记相对路径，SQLite 保存索引和任务状态，不保存整份 XML。

`danmu_api` 的 `POST /api/v2/match` 是插件内部的可选优先路径；不支持该接口时由插件回退到
Dandanplay 兼容搜索、详情和评论接口。插件负责并发请求、超时、响应大小限制、XML 校验和
错误分类；主进程不得保留一份直接请求弹幕上游的实现。主进程对插件返回的 XML 仍执行最终
大小和路径安全校验，并通过临时文件加原子重命名写入旁车。

管理员通过 `POST /api/v1/admin/libraries/{libraryId}/danmaku/match` 创建持久化任务，支持分页列表、详情、取消、失败重试、服务重启恢复、并发上限和默认不覆盖已有 XML。任务只领取已索引的本地视频源；`.strm`、用户请求中的整库扫描、弹幕实时发送和上游任意 URL 均不进入范围。

Emby 兼容层提供 `/api/danmu/{itemId}`、`/api/danmu/{itemId}/raw`，并保留 `option=Refresh` 和 `option=GetJsonById` 兼容别名。端点执行现有用户/媒体库 ACL；普通 Emby 字幕端点和不支持弹幕协议的客户端不属于本任务验收范围。

验收：

- [ ] 从空数据库执行迁移成功；扫描后的同名有效 XML 可以登记、读取，删除或损坏旁车会标记索引状态而不删除媒体。
- [ ] Plugin SDK 能校验弹幕插件 manifest、`MEDIA` 分类和 `danmaku.match` 能力；插件提供 `plugin.hello`、`plugin.health`、`danmaku.match` 和 `plugin.shutdown`。
- [ ] 管理员可以通过插件配置界面保存、清除和查看脱敏的弹幕地址；HTTP/HTTPS、token 路径、控制字符、凭据和 fragment 校验符合安全策略，主进程不再保存弹幕专用配置模型。
- [ ] 插件的 `/api/v2/match` 成功路径可以得到 episode 并取得 XML；不支持 `match` 时插件内的搜索/详情回退可工作；无匹配、非 XML、超大响应、超时不会写旁车。
- [ ] 主进程不会直接访问弹幕上游；插件进程故障、超时或单项错误只标记当前任务项，不使主进程退出或终止整批任务。
- [ ] 成功结果写入视频同名 `.xml`；默认不覆盖已有 XML；中断或权限失败不会留下半个目标文件。
- [ ] 后台任务支持分页、进度、取消、失败重试和重启恢复；取消不再领取新项，单项失败不终止任务。
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

元数据整库任务的条目进度通知按任务合并，每个任务最多每秒发布一次 `jobs` 失效事件，任务完成、
失败或取消时立即发布最终事件；单条结果不得同时通过 `jobs` 和 `metadata` 重复失效同一任务列表。
任务摘要先分页再聚合待确认条目，媒体库身份直接保存在任务记录中，不得按列表行重复扫描整张任务
明细表。整库任务全局只允许一个处于等待或运行状态，普通条目任务与整库任务共享最多 8 个 worker；
服务重启后遗留的运行中条目重新排队，同一任务进程内只允许一个 owner。

验收：

- [ ] SSE 端点完成管理员鉴权、协议头、ready 帧、心跳和丢帧退化测试。
- [ ] 活动、后台任务和管理配置变更发布正确作用域，前端只失效受影响查询。
- [ ] 管理布局维持单连接、自动重连、重连补偿和卸载关闭行为；页面级刷新按钮全部移除。
- [x] 元数据任务进度事件按任务节流且最终状态立即送达；任务摘要查询不做逐行关联扫描，整库任务和
      worker 总量有界，重启遗留条目可恢复。
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
不再遍历。任务取消或完成时清理临时工作项；可恢复失败任务保留有界 checkpoint，避免工作队列
无限增长。

扫描 worker 使用进程内共享的容量为 1 的互斥锁。一个媒体库的文件系统发现、调和工作项处理
和索引写入期间，其他媒体库的文件扫描任务保持排队；文件系统阶段完成并提交后释放该锁。
ffprobe、本地 NFO/图片、缩略图、自动封面和在线元数据调度属于后处理阶段，不持有扫描互斥锁，
由各自的有界资源配额控制。该机制仍不引入跨库 worker pool，也不改变任务的持久化、恢复和
取消模型。

验收：

- [x] 创建全量任务不访问媒体文件系统；后台发现阶段对未中断任务中的每个目录只读取一次。
- [x] 发现的文件路径持久化后按批处理；处理批次不重新遍历媒体库，重启后从剩余目录或文件工作项继续。
- [x] 只有所有可用根路径完成发现后才执行 generation missing 判定；不可用或扫描中失效的根路径不批量标记缺失。
- [x] 任务进度在发现完成后具有稳定 `totalCount`；取消和完成会清理临时工作项，失败任务保留可恢复 checkpoint，现有增量扫描行为不变。
- [ ] 文件系统阶段完成后释放扫描互斥锁，其他媒体库可以开始文件扫描；后处理继续受独立资源配额限制。
- [x] 自动化测试覆盖单次发现快照、分批恢复、发现期间取消、根路径不可用和工作项清理。
- [ ] 自动化测试覆盖失败 checkpoint 重试和后处理阶段不持有扫描互斥锁。

验证：

- `cargo test --locked --test scanning_jobs`
- `cargo build --locked`
- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `uname -m`

实施记录（2026-08-07）：`./scripts/check-all.sh` 全部通过；原生验证架构为 `arm64`。

依赖：LUX-041、LUX-043、LUX-045。

#### LUX-187：全局扫描活动与首页即时刷新

管理员 Web 页面右上角显示当前活动的扫描任务，摘要包括媒体库名称、扫描阶段、已处理/总数
和当前正在处理的相对条目显示名。摘要不得返回媒体库根路径、完整本地路径、`.strm` 原始
目标、token、查询参数或其他凭据。活动入口可以进入“任务与日志”并取消活动任务。

扫描任务持久化当前安全显示名和阶段；发现目录、索引文件、收尾、完成、失败和取消会通过
管理员 SSE 的 `jobs` 作用域刷新任务摘要。新增同源 `GET /api/v1/events`，只允许已登录的
Lux Web 用户，发送不携带业务数据的 `ready` 与 `invalidate` 事件。扫描索引提交后发布
`home` 作用域，普通用户 Web 客户端收到后立即失效首页、媒体库列表和当前媒体库分页缓存；
断线时继续保留低频轮询兜底。该端点不向 Emby 兼容 API 或未认证请求开放。

验收：

- [ ] 任意普通 Lux Web 页面在管理员会话下显示全局活动扫描入口和实时进度。
- [ ] 当前条目摘要经过 basename/相对显示名清理，不包含完整路径、`.strm` URL、token 或 query string。
- [ ] 扫描写入后普通用户首页和媒体库列表立即刷新，事件不携带业务数据。
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

#### LUX-161：可配置的 STRM 本地播放额外允许根目录

范围：修复本地绝对路径型 `.strm` 在媒体库根目录之外无法播放的问题。管理员通过
`GET/PATCH /api/v1/admin/settings` 配置 `strmAllowedRoots`，每行/每项一个容器内绝对目录，
例如 `/CloudNAS/115-122`。配置为空时保持原有行为，媒体库根目录仍自动允许；配置后，Web、Emby
和第三方播放器共用的 Lux 视频入口可以读取额外根目录下的本地目标，无需修改 `.strm` 内容。

播放时 `.strm` 目标相对于 `.strm` 所在目录解析，随后对媒体库根目录、额外允许根目录和目标文件执行
canonicalize。只有位于任一允许根目录下且存在的普通文件可播放；`/`、空值、非绝对路径、`..` 越界、
符号链接越界、目录和另一个 `.strm` 仍拒绝。该任务不实现任意容器路径无条件开放、路径映射、远程 URL
代理、Remux、转码或 HLS。

验收：

- [ ] 管理员可以读取、保存和清除 `strmAllowedRoots`；旧数据库无该键时返回空列表，不需要 migration。
- [ ] 本地绝对 `.strm` 目标位于额外允许根目录时，Lux Web 和 Emby 视频入口均按本地文件返回 Range 响应；
      `.strm` 原始文本无需改写。
- [ ] 未配置的外部路径、`..` 越界、符号链接越界、根目录、目录和 `.strm` 目标继续被拒绝。
- [ ] 输入在管理 API 边界校验，播放时重新 canonicalize；不记录完整本地路径或 `.strm` 原始目标。
- [ ] 通过专项 Rust/Web 测试、格式化、Clippy、Web 构建，并记录本机 ARM 架构。

验证：`cargo test --locked --test strm_target --test strm_allowed_roots`、相关 API 测试、
`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`、
`pnpm --dir web test`、`pnpm --dir web build` 和 `uname -m`。

依赖：LUX-159、LUX-160。

明确不做：

- 不无条件开放容器内任意可读路径；不把 `/` 作为允许根目录。
- 不实现路径映射、媒体字节代理、转码或改变第三方客户端的 Emby 路由合同。

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

人物资源和媒体人物关系必须可从配置卷恢复。`person.json` 保存 Lux 编号、provider 身份、别名、字段来源和版本；
媒体条目的 `people.json` 保存 Lux 人物编号、角色/排序、旧 item ID、稳定媒体来源键以及用于迁移的 provider ID、
规范化路径、标题、年份和指纹。数据库清空后，后台恢复任务先重建媒体来源映射，再导入人物资源和 provider 索引，
最后恢复 `person_credits` 运行时索引。恢复过程必须可重试、幂等、可中断续跑，并隔离损坏或有歧义的快照。

旧 provider 目录、旧 `people/assets` 文件和旧关系快照继续兼容读取，升级不得删除旧文件；新图片不得写入
`people/assets`。

验收：

- [x] 无 ID 的 NFO 演员在后台扫描后出现在详情页，并显示占位头像。
- [x] 同一出演关系携带 TMDb、IMDb、豆瓣身份时只保留一个规范人物目录和一份 `folder.<ext>` 图片。
- [x] 已有头像时后续 provider 图片写入被跳过；NFO 仅补充缺失字段，不覆盖已有非空字段。
- [x] 明确的跨 provider ID 可以自动合并；高置信度的同媒体关系/精确生日候选可以后台自动确认；
      仅姓名或不完整生日不得自动合并，未确认的同名人物保持隔离。
- [x] 每个规范人物分配不可复用的 `lux-000001` 编号；目录使用可读姓名加 Lux 编号，不暴露 provider ID。
- [x] provider 身份映射、自动匹配证据、撤销/拆分记录和字段锁定状态在配置卷快照中可恢复。
- [x] 删除数据库后重新扫描同一媒体库，可以恢复人物、provider 映射、头像、NFO 和媒体人物关系；
      媒体移动、路径复用、损坏快照和多候选匹配不会静默关联错误条目。
- [x] 恢复和在线刮削并发时使用 generation/租约或等价 compare-and-swap，不能以旧快照覆盖新结果。
- [x] `people.json` 关系快照升级可读取版本 1，旧人物目录、旧图片索引和 Emby 兼容图片路由继续可用。
- [x] 演员关系写入、人物资源写入和图片下载彼此解耦；任一资源失败都不丢失演员展示关系。

验证：人物关系单测、NFO 扫描集成测试、跨 provider 自动合并/隔离/撤销测试、生日精度匹配测试、
图片内容去重和损坏恢复测试、旧布局兼容测试、数据库清空后快照恢复测试、详情 API 和 Web 占位图测试。

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

范围：扩展 Plugin SDK v1，支持 `chapter_detector` 类型与 `chapters.detect` 能力。Lux 在持久化后台
任务中按季度分页读取本地分集，使用现有 ffmpeg 的 chromaprint muxer 提取开头/结尾的有界原始指纹，
只把指纹、采样率、窗口相对时间和请求内临时键发送给插件。插件不接收路径、URL、媒体源 ID、凭据或任务对象。
宿主校验插件结果并把高置信度标记保存为插件来源特殊章节（`provider_id` 为插件 ID）。单季度超过
RPC 上限时批次保留一个分集的上下文重叠，但只对未处理分集落库，避免跨批次漏掉共同片头片尾。

验收：

- [x] manifest、RPC 请求和响应均有严格大小、枚举、数量、时间范围和置信度校验。
- [x] 管理员可按已保存插件配置启动、取消、重试和查看持久化检测任务；重启恢复 PENDING/RUNNING。
- [x] 插件失败、ffmpeg 缺少 chromaprint、超时或坏响应只影响对应分集/任务，不删除已有确认标记。
- [x] 成功重跑只原子替换同一插件生成的标记，不覆盖其他来源。

验证：假 ffmpeg、假插件进程、任务恢复、ACL/CSRF、故障注入测试和完整项目检查。

依赖：LUX-173、LUX-174、LUX-171。

#### LUX-176：外置片头片尾检测插件

范围：在独立 `Lux-plugins` 仓库实现 `org.lux.intro-outro-detector`。插件比较同季度至少两个分集的
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

范围：在独立 `Lux-plugins` 仓库实现 `org.lux.theintrodb-chapter-source`。插件通过新增的
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
该 Key 同时用于 Lux `/api/v1` 和已实现的 Emby 兼容路由，调用时按服务器管理员权限执行。

验收：

- [x] 支持 `X-Emby-Token`、`X-Lux-Api-Key`、`Authorization: Bearer` 和兼容的 `api_key` 查询参数。
- [x] Lux API 与 Emby 兼容 API 都接受共享 Key；现有用户 Web session 和 Emby 登录 AccessToken 行为不变。
- [x] Key 使用至少 256 bit 随机熵，持久化到 `/config` 的受限文件，重启后保持不变；生成、轮换和撤销使用原子写入。
- [x] 非管理员不能读取或操作 Key；Key 不能调用自身的查看、轮换和撤销接口。
- [x] Key 请求跳过 Cookie CSRF 但仍执行管理员权限和远程访问策略；日志、审计事件、错误响应和普通 API 响应不包含明文 Key。
- [x] 轮换立即使旧 Key 失效；审计明确标记共享 API Key，不能伪装成某一位管理员。

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
`JOB_FAILED`、`PLAYBACK_STARTED`、`PLAYBACK_PAUSED`、`PLAYBACK_PROGRESS`、`PLAYBACK_STOPPED`。事件不包含
本地绝对路径、`.strm` 原始目标、令牌、完整外部 URL 或不必要的用户隐私字段。

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
服务重启时遗留的 `RUNNING` 任务重新排队；同一媒体库同一时间只能有一个 worker 领取任务。
进程内重复触发会合并为 pending 标记，实际重建协调器保持单个运行器，避免重复整库恢复扫描。

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
- [x] `RUNNING` 任务重启后重新排队；并发领取只能成功一次。
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
进程重启后遗留的条目重新排队；同一任务在单进程内只能有一个 owner。

管理员任务页的加载状态必须有可见反馈并暴露 `aria-busy`，错误状态不持续显示 spinner。metadata
进度事件按任务节流，完成、失败和取消立即发布；前端不能因为 `jobs` 与 `metadata` 两个作用域
同时失效同一查询。图片下载只对 429、5xx、连接错误和超时做有限退避重试。

运行记录在任务结束后显示本次运行的总耗时；总耗时使用任务真实的 `started_at` 与 `finished_at`
计算，时间字段不完整时不显示推算值，运行中的任务不显示完成耗时。

验收：

- [x] watcher 初始化不运行在 Tokio 核心 worker，初始化线程数量有界，现有根路径取消/重concile 行为不变。
- [x] metadata worker 总量有界，重复启动同一任务被拒绝，重启遗留 `RUNNING` 条目重新进入 `PENDING`。
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

范围：将媒体库的单个 `scraperId` 扩展为可排序的刮削器列表。首位固定为 `PRIMARY`；后续刮削器可分别配置为 `SUPPLEMENT`、`BACKUP` 或 `BOTH`。主/备用来源负责确认媒体身份，补充来源只填充缺失的未锁定字段和缺失图片类型。

API 合同：

- 新 Lux API 返回 `scrapers` 数组，每项包含 `scraperId`、`position` 和 `role`。
- 旧版 `scraperId` 继续返回并表示 position 0 的主刮削器；旧版只提交 `scraperId` 时转换为单个 `PRIMARY` 项。
- 创建和 PATCH 媒体库时，`scrapers` 的顺序和角色作为一个原子配置更新；空数组清除在线刮削。
- 每个 scraper ID 只能出现一次；position 0 必须是 `PRIMARY`；所有选择都必须是已安装、已启用且可用的 metadata 插件。

执行合同：

- `PRIMARY` 和 `BACKUP` 按 position 顺序尝试，首个达到高置信度并拥有有效 provider ID 的来源确认媒体身份。
- `SUPPLEMENT` 和 `BOTH` 只在身份确认后运行；它们按顺序仅补充缺失的未锁定字段、图片类型和可安全按 provider ID 去重的列表数据。
- `BOTH` 在主来源失败时参加备用尝试，在主来源成功时参加补充尝试。
- 后续来源不得覆盖本地 NFO、锁定字段、已有更高优先级来源或已确认的媒体身份；每个字段和图片记录实际 scraper 来源。
- `FILL_MISSING` 只请求实际缺失的内容；`FULL_REFRESH` 允许刷新主来源的未锁定在线字段，再由补充来源补足仍缺失的内容。
- 所有来源失败时保留本地可播放条目，并按现有任务错误/待确认语义记录结果；日志只能记录脱敏的 scraper ID、角色和错误码。

验收：

- [ ] SQLite 和 PostgreSQL 空库迁移成功，历史 `libraries.scraper_id` 自动迁移为 position 0 的 `PRIMARY`。
- [ ] 管理员可以创建、编辑、排序和清除有序刮削器列表；旧 API 客户端仍能读取和提交单个 `scraperId`。
- [ ] 主刮削器成功时，备用来源不被调用；主刮削器失败时，备用和 `BOTH` 按顺序接管。
- [ ] 补充和 `BOTH` 来源只能填充缺失内容，不能覆盖本地、锁定或主来源字段；图片按类型补缺。
- [ ] 第二来源成功后的 provider ID、字段来源和图片来源可在后续刷新中正确使用。
- [ ] 非管理员不能查看或修改媒体库刮削器角色和顺序。

验证：

- Rust 单元/集成测试覆盖迁移、API 兼容、角色校验、备用接管、补充合并、来源追踪和图片补缺。
- Web 单测覆盖拖拽排序、角色选择、首位主刮削器约束、不可用已选插件和保存失败。
- 运行 Rust/Web 基线检查，并记录 ARM64 验证结果。

依赖：LUX-140、LUX-142、LUX-168、LUX-195。

#### LUX-197：全量扫描变化集与后处理资源隔离

范围：在现有 LUX-154 持久化目录发现和文件工作队列之上，补齐全量调和的变化集优化与后处理
资源隔离。文件工作项先通过 `stat` 和快速 fingerprint 分类为 unchanged、new 或 changed；
unchanged 只批量标记本轮 seen，不进入媒体索引、NFO、图片、缩略图或 ffprobe。new/changed
source 和受影响 item 持久化为扫描目标，后处理按目标集合执行并可在进程重启后恢复。NFO、图片
和其他旁车文件的变化必须能把对应 item 标记为 metadata target，不能因视频文件 fingerprint
未变化而永久跳过旁车更新。

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
- [x] 扫描目标、后处理阶段和失败状态持久化；进程重启、取消和重试不会重复完成目标，也不会丢失未完成目标。
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
