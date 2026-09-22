import { ArrowRightLeft, BookOpen, GitBranch, Sparkles, Wrench } from "lucide-react";

type ChangelogSectionKind = "added" | "fixed" | "changed";

type ChangelogSection = {
  kind: ChangelogSectionKind;
  items: string[];
};

type ChangelogRelease = {
  version: string;
  date: string;
  sections: ChangelogSection[];
};

export const changelogReleases: ChangelogRelease[] = [
  {
    version: "0.4.13",
    date: "2026-09-22",
    sections: [
      { kind: "added", items: [
        "Emby 播放会话补充对应用户名称、当前播放媒体标题及可用剧集/集信息，管理员查看多用户会话时按实际用户正确展示。",
        "Emby 播放会话进一步补齐简介、原标题、层级关系、季名称、图片标签和 ProviderIds，并将 LastActivityDate 统一为 UTC RFC3339；缺失字段使用稳定的空值形状。",
        "扫描任务活动接口新增发现阶段状态，管理台和后台活动面板会在目录发现完成前显示“发现中”，不再展示尚未确定的临时百分比。",
        "补全元数据任务会跳过已具备完整本地元数据的条目，并复用已有 Provider ID 获取缺失图片，减少不必要的标题搜索和远端请求。",
      ] },
      { kind: "fixed", items: [
        "修复 Emby STRM 外部直链播放的交接流程；认证后直接将原始地址重定向给客户端，避免 Lux 预探测负载均衡重定向并保留客户端的请求上下文。",
        "修复 Emby STRM 媒体详情暴露外部目标地址的问题；详情中的 Path 保持安全的 .strm 形态，同时 URL 型 STRM 在 PlaybackInfo 中正确标记为远程可播放媒体。",
        "修复 Emby 对 Matroska 容器的兼容性；将 matroska,webm 规范化为 mkv，并生成带 .mkv 后缀的播放入口，避免第三方代理降级到无后缀流地址。",
        "修复带设备配置的 URL 型 STRM 播放协商；继续返回包含代理用户提示和短期票据的 Lux DirectStreamUrl，避免 FileBar 自行构造播放地址时丢失 NextEmby 用户上下文。",
        "修复 URL 型 STRM 在设备配置容器不匹配时被误判为需要服务器转码的问题；兼容 mkv、matroska 和 matroska,webm 别名，继续交给外部代理直放。",
        "修复 Emby 直播放地址缺少 PlaySessionId 和 static=true 参数的问题；第三方代理现在可以正确识别并切换对应的播放流。",
        "修正登录页动态海报瀑布流的位置、缩放和错落布局，使海报墙与登录面板的视觉对齐更稳定。",
        "优化登录页动态海报瀑布流的响应式布局，根据可用宽度自适应列数并独立调整海报卡片尺寸，改善不同窗口宽度下的展示效果。",
        "收紧登录页海报瀑布流的列间距并将卡片靠右对齐，减少宽屏下的空白并保持紧凑的海报墙效果。",
        "修复 Emby 会话 TranscodingInfo 的码率字段可能返回空值的问题；直播放、未知探针结果和服务端 HLS 会话现在统一返回数值，未知值为 0。",
        "兼容缺少 PlaySessionId 或 DeviceId 的 Emby 停止回调；仅在用户、媒体和近期会话候选唯一时停止对应会话，避免误停多个活动会话。",
        "限制 ActiveWithinSeconds 不得突破服务端 90 秒失活阈值，并保证缺失生产年份时仍返回数值 0，避免第三方会话卡片比较空值失败。",
        "优化扫描发现与 reconciliation 的批量提交、缺失文件处理和元数据目标通知；未变化文件不再重复写入状态，扫描终态会在首页快照刷新完成后再通知客户端。",
      ] },
    ],
  },
  {
    version: "0.4.12",
    date: "2026-09-21",
    sections: [
      { kind: "added", items: [
        "登录页升级为媒体中心风格的响应式分栏界面，新增海报墙、品牌资源、欢迎文案和更清晰的登录表单布局。",
        "密码输入框新增显示/隐藏切换按钮，并保留用户名和密码自动填充能力。",
        "管理员可配置登录页背景来源，支持固定海报墙或启用媒体库中的最新添加内容。",
        "新增登录背景接口，登录页可动态展示媒体库海报；接口不可用或没有可用海报时自动回退到固定海报墙。",
        "补充登录页 Lux 品牌标识以及配套 Logo、favicon 资源。",
      ] },
      { kind: "fixed", items: [
        "修复登录页动态背景可能选取季、集等非目标层级媒体的问题；现在仅使用电影和剧集海报。",
        "调整动态登录海报为三列倾斜瀑布流布局，使其与固定海报墙的视觉效果保持一致。",
        "兼容部分 Emby 客户端以字符串传递 MaxAudioChannels 的播放请求，避免播放协商失败。",
        "优化扫描期间首页和媒体库的刷新通知：连续扫描批次在 1 秒窗口内合并，扫描完成、失败或取消时立即刷新，减少刷新风暴并保持最终结果及时可见。",
      ] },
    ],
  },
  {
    version: "0.4.11",
    date: "2026-09-20",
    sections: [
      { kind: "added", items: [
        "Emby 搜索和媒体列表支持按 Provider ID 精确过滤，并保留客户端明确请求季度或单集时的层级结果。",
        "增加部分 Emby 客户端的剧集最新单集兼容处理；请求剧集单集列表时可在服务端合并完整结果，避免客户端因不继续分页而遗漏分集。",
        "管理员用户管理支持分别启用、禁用和永久删除账户；禁用保留账户数据，永久删除清理账户及关联数据。",
        "增加 Emby 媒体库增量刷新和扫描状态兼容接口，支持客户端读取 RefreshMediaLibrary 的排队/运行状态。",
        "播放活动和通知补充 Web 播放会话信息，并保留 STRM 播放方式，便于管理台和通知接收方识别实际播放链路。",
      ] },
      { kind: "fixed", items: [
        "修复普通搜索和搜索提示默认返回季度、单集，导致客户端结果重复或层级混乱的问题；默认只返回电影和整剧，显式类型请求仍按协议返回对应层级。",
        "修复 Emby 媒体列表在 Provider ID、类型筛选和 ACL 组合使用时的结果范围与分页问题。",
        "修复 VidHub 季列表只请求首个剧集页且不继续分页导致分集不完整的问题；现在仅对该客户端的首个请求返回有上限的完整剧集列表，其他客户端继续遵循标准分页。",
        "修复剧集接口省略分页参数时的返回语义；现在按安全上限返回完整列表，显式传入 Limit 时仍遵循标准分页，避免客户端遗漏分集。",
      ] },
    ],
  },
  {
    version: "0.4.10",
    date: "2026-09-20",
    sections: [
      { kind: "added", items: [
        "管理台“正在播放”补充真实播放方式、服务端 HLS 档位、输出容器、视频/音频 codec 和码率等信息，便于确认当前实际播放链路。",
        "Emby 会话详情补充服务端转码输出信息，并持久化播放会话的输出容器、视频/音频 codec 与码率。",
        "新增服务器级“媒体库顺序强制按照管理员排序”设置和用户级“按照管理员顺序排序”开关；管理员可统一控制普通用户在首页、媒体库入口和 Emby 视图中的媒体库顺序。",
        "媒体详情页支持在播放前选择音轨和字幕轨道，并将选择传递到播放会话。",
        "管理员用户管理区分“禁用账户”和“永久删除账户”：禁用保留数据，删除会清理账户及关联数据。",
        "首页轮播控制改为进度点，减少遮挡并更直观地表示当前轮播位置。",
      ] },
      { kind: "fixed", items: [
        "修复 Emby 播放会话仍显示为直放、无法反映实际转码输出的问题；现在会根据活动 HLS 会话返回准确的 Transcode 与 TranscodingInfo。",
        "登录表单补充标准用户名和密码自动填充字段，改善浏览器和密码管理器的识别。",
        "修复媒体库排序在管理员策略和普通用户个人设置之间切换时的有效顺序、缓存失效及 Emby 兼容视图同步问题。",
        "修复播放请求选择音轨后服务端 HLS 会话未稳定保留轨道选择的问题。",
      ] },
    ],
  },
  {
    version: "0.4.9",
    date: "2026-09-19",
    sections: [
      { kind: "fixed", items: [
        "修复 Harbor/Emby 切换到中段码率或续播时，HLS 会话在首个资源请求前提前启动并被立即取消，导致视频无法播放的问题；现在首个 init 或媒体分片请求才启动转码，并由首个媒体分片确定实际起点。",
        "修复 HLS init 与 seek 分片并发请求时可能读取不同 generation 资源的问题；切换转码 generation 后，等待中的请求会跟随当前 generation，避免返回失效的 init 或分片。",
        "修复无效或超出媒体总时长的 StartTimeTicks 导致转码从 EOF 启动失败的问题，并避免新建但尚未使用的播放 offer 提前停止当前播放会话。",
        "修复 Emby VOD 在远距离 seek 或并发切换 generation 时混用不同时间基准的 init 与媒体分片的问题；现在每个逻辑分片绑定对应的 init 和 generation 资源，旧 generation 的请求不会被新 seek 改写。",
        "修复 seek 期间 init 文件尚未落盘或被替换导致 Harbor 收到空资源的问题；generation 切换会保护已绑定的 init，并让并发 init/分片请求使用同一 generation。",
        "修复 Emby 客户端未声明 fMP4 时无法稳定切换 HLS 播放流的问题；现在默认协商 MPEG-TS，只有明确声明 mp4/fmp4 时才使用 fMP4。",
        "修复 HLS 分片容器、文件扩展名、MIME 类型、清单 init 结构和签名资源不一致的问题；TS 与 fMP4 会话现在严格隔离，避免 Harbor 将不同容器的资源混用。",
      ] },
    ],
  },
  {
    version: "0.4.8",
    date: "2026-09-18",
    sections: [
      { kind: "fixed", items: [
        "修复第三方 Emby 客户端读取 Lux HLS 时只能看到已转码片段时长的问题；Emby HLS 现在按完整媒体时长返回 VOD 清单，并等待尚未生成的分片。",
        "修复 Emby HLS 续播时虚拟 VOD 时间线与实际分片编号不一致的问题；转码输出按四秒时间线对齐关键帧和起始分片编号，避免续播分片无法读取。",
        "修复 Emby 客户端未发送 StartTimeTicks 时无法从已保存播放进度续播的问题；显式起始位置仍优先，并避免已播放或已到结尾的条目误跳转。",
        "修复 Emby 续播 HLS 清单的逻辑分片名无法映射到 FFmpeg 实际起始分片的问题，恢复播放时分片现在可以正常读取。",
        "修复部分 Emby 客户端续播时先探测时间线前段分片导致读取失败的问题；兼容早期分片探测，同时保持续播位置之后的原始分片编号。",
        "修复 HLS 续播时 FFmpeg 输入 seek 将媒体时间戳重置为 0、导致 Emby VOD 时间线不连续的问题；现在保留原始媒体时间戳。",
        "修复 Emby HLS 远距离 seek 或续播时清单逻辑时间线与 FFmpeg 输出不一致的问题；现在按逻辑分片对应的媒体位置重启转码，保留源时间戳并安全处理并发请求。",
      ] },
    ],
  },
  {
    version: "0.4.7",
    date: "2026-09-18",
    sections: [
      { kind: "added", items: [
        "管理台通知创建改为弹窗流程，通知投递历史现在展示通知标题和正文，便于快速确认实际发送内容。",
        "通知模板补充媒体库、扫描进度、任务耗时、状态和错误上下文，并将媒体新增、移除、元数据更新、扫描和任务失败等事件转换为更易读的标题与正文。",
        "新增 Emby GET /Library/MediaFolders 和 POST /Items/{FolderId}/Refresh，支持管理员按已登记物理目录触发局部异步刷新。",
        "新增原生 POST /api/v1/admin/libraries/{libraryId}/scan-path，支持校验后的媒体库相对路径增量扫描，不再因目录刷新触发整库调和。",
      ] },
      { kind: "changed", items: [
        "目录刷新复用现有增量扫描队列，保留权限校验、路径安全、去重、取消、后处理和 Webhook 生命周期。",
      ] },
      { kind: "fixed", items: [
        "修复修改媒体库缩略图策略后，已有媒体不会重新生成缺失缩略图的问题；后续全量扫描会补登记现有本地媒体的缩略图目标。",
        "修复缺少 PlaySessionId 的过期 Emby STOPPED 回调误停止当前 HLS 转码会话的问题。",
        "修复本地 NFO 缺少演员身份信息时覆盖已有演员档案的问题；同步后继续保留已登记的身份和头像。",
        "修复首页推荐请求同步刷新全局推荐统计导致迁移或扫描期间等待数据库写事务的问题；推荐列表现在保持读优先。",
        "修复 Emby HLS 转码切换或续播时未应用 StartTimeTicks 的问题，转码会从客户端请求的播放位置开始。",
      ] },
    ],
  },
  {
    version: "0.4.6",
    date: "2026-09-18",
    sections: [
      { kind: "added", items: [
        "Lux 核心统一生成通知事件的 source、title、content、body 和 ISO timestamp 字段，内置 Webhook 与通知器插件使用一致的可读内容。",
        "播放通知补充媒体标题、用户、容器、大小、码率、播放方式、设备和恢复播放等展示信息，并在停止事件中提供远程 IP 与简介，同时继续过滤本地路径和外部地址。",
      ] },
      { kind: "fixed", items: [
        "修复全量扫描无法为已有 URL .strm 媒体读取 *-mediainfo.json 旁车文件的问题；现在可直接导入容器、时长、码率和媒体流信息，无需调用 ffprobe。",
      ] },
    ],
  },
  {
    version: "0.4.5",
    date: "2026-09-17",
    sections: [
      { kind: "added", items: [
        "首次设置 PostgreSQL 后，管理员可直接在引导页面点击“重启 Lux”；服务会优雅重启并在数据库迁移完成后继续管理员初始化。",
        "媒体库策略新增缩略图刮削方式，可选择不刮削、截图优先或刮削器优先，并兼容已有媒体库配置。",
      ] },
      { kind: "fixed", items: [
        "修复失败或孤立的 HLS 转码会话未及时清理的问题；manifest 等待失败、播放停止或客户端中断后会回收对应 FFmpeg 资源。",
        "修复扫描过程中重复更新媒体可用性状态的问题，并增强 reconciliation 工作项的批量提交、状态持久化和失败恢复，减少重试时的重复处理。",
      ] },
      { kind: "changed", items: [
        "将扫描、媒体技术信息探测和缩略图/FFmpeg 阶段拆分为独立并发控制，Docker 默认提供 LUX_SCAN_CONCURRENCY、LUX_PROBE_CONCURRENCY 和 LUX_FFMPEG_CONCURRENCY 配置。",
        "通知器管理支持插件声明的 URL 模板和 Body 模板字段，管理员可在管理台直接编辑多行请求模板。",
        "放宽通知器插件目标地址的兼容性，允许保留第三方服务所需的查询参数、认证信息和片段；核心 Webhook 地址仍执行严格的安全校验。",
        "Docker 部署默认以 root 运行，挂载 NAS 目录时无需额外的 PUID/PGID 交接或递归修改所有权。",
      ] },
    ],
  },
  {
    version: "0.4.4",
    date: "2026-09-16",
    sections: [
      { kind: "fixed", items: [
        "修复 Emby 实时 HLS 转码的播放时长被动态清单片段长度误导的问题；PlaybackInfo 顶层和媒体源现在返回完整 RunTimeTicks，并在源时长缺失时回退到媒体项时长。",
        "修复 Emby 播放回调携带动态 HLS 当前清单时长时覆盖服务端完整媒体时长的问题。",
        "修复 HLS 转码清单错误声明 VOD 类型导致 FFmpeg 在转码完成前不输出 manifest、第三方客户端无法开始播放的问题；恢复可边转边播的增量清单。",
        "修复用户放弃播放或发送停止事件后旧 Emby 转码会话未及时释放的问题，及时停止 FFmpeg 并清理相关播放资源。",
      ] },
    ],
  },
  {
    version: "0.4.3",
    date: "2026-09-16",
    sections: [
      { kind: "added", items: [
        "本地视频在缺少有效海报或缩略图时，可从同一画面生成独立的竖版 POSTER 和横版 THUMB 回退图片。",
        "Lux 自有 API 支持用户级 Emby AccessToken；第三方客户端可通过 X-Lux-Token、兼容令牌请求头或 Bearer 调用首页、媒体、搜索、图片、播放和用户状态接口。",
        "管理员可直接点击媒体库封面打开媒体库编辑器。",
        "管理员手动搜索元数据候选时，支持按媒体库配置的刮削器顺序在主刮削器无结果或失败后尝试备用刮削器。",
      ] },
      { kind: "fixed", items: [
        "修复元数据刮削器失败时错误返回数据库不可用错误码的问题，改为正确报告插件不可用。",
        "修复插件配置提交清单未声明的 UI 字段，避免旧版或能力不同的插件收到不支持的配置项。",
        "修复 Emby 客户端仅发送 EnableTranscoding=true、省略 EnableDirectPlay 时未进入服务端转码的问题。",
        "修复 Emby 客户端将 PlaybackInfo 播放开关放在 POST 查询参数、而将 DeviceProfile 放在 body 时未触发服务端转码的问题。",
        "修复 Emby 转码 offer 已准备但仍保留直放入口的问题；实际选择服务端转码时关闭直放能力并返回可用的转码 URL，同时保留可转码能力供客户端重试。",
        "修复 Emby MaxStreamingBitrate 限制未参与 PlaybackInfo 协商的问题；已知源码率超限时选择服务端转码，未知码率不会因此误触发转码。",
        "兼容 Emby 客户端通过 DeviceProfile 声明直放和 HLS 转码能力，并在直放 profile 不匹配时协商服务端转码的 PlaybackInfo 请求。",
        "对齐 Emby 服务端转码 offer：补齐标准设备、codec、码率、轨道和 HLS 分片参数，实际转码 offer 不再暴露竞争性的 DirectStreamUrl。",
        "修复 Lux 内部生成或写入图片后触发实时扫描的问题；外部图片更新仍可使内部写入抑制标记失效。",
        "媒体探测信息缺失时，不再将未知的容器或编解码信息误判为直放不兼容，避免意外协商服务端转码。",
        "修复实时增量扫描未及时探测新增或变化的本地媒体源的问题；未变化的媒体源和 .strm 不会交给普通 ffprobe。",
      ] },
      { kind: "changed", items: [
        "调整本地图片、在线刮削图片和 FFmpeg 截图回退的优先级；已有本地或刮削器图片不会被截图覆盖，缺失的图片类型可独立补全。",
        "客户端令牌请求继续执行用户媒体库 ACL，普通用户令牌不能调用管理员接口。",
        "首页继续观看内容改为每次请求实时读取，其他稳定首页内容继续使用缓存，播放进度变化可以更快反映在首页。",
        "扩展 Emby PlaybackInfo 协商兼容性：POST 的 forceTranscode=true 可覆盖直放请求，GET 不会因该参数创建转码会话；本地媒体正确声明服务端转码能力，.strm 仍保持直放限制。",
        "对齐 Lux Web API 类型定义，补充播放次数、排序标题、人物创建时间、首页最近添加总数及播放状态条目 ID 等字段。",
        "补充 Emby HLS 转码协商和资源请求诊断日志，以有限的资源类别、状态码和耗时帮助排查播放问题。",
        "优化实时增量扫描流程，仅在后台处理本次任务登记的新增或变化本地 source，并保持 .strm 的定向插件探测边界。",
      ] },
    ],
  },
  {
    version: "0.4.2",
    date: "2026-09-15",
    sections: [
      { kind: "added", items: [
        "单季剧集详情页现在直接展示分集列表，减少进入季度层级的操作步骤。",
        "分集列表和同季分集卡片的图片区域新增直接播放入口，并显示可访问的播放按钮。",
        "Emby 客户端可通过 PlaybackInfo 为本地媒体协商服务端 HLS 转码，支持 Remux、音频转码、硬件转码和软件转码。",
      ] },
      { kind: "changed", items: [
        "分集浏览交互明确区分播放与查看详情：点击图片播放，点击标题或详情入口进入分集详情页。",
        "转码清单和分片使用短期签名并绑定播放会话；播放回调可刷新会话，停止或过期后会回收 FFmpeg 资源，.strm 媒体不启用服务端转码。",
      ] },
    ],
  },
  {
    version: "0.4.1",
    date: "2026-09-10",
    sections: [
      { kind: "added", items: [
        "管理员可在媒体库多选模式中将同一媒体库内的电影或剧集合并为多版本，并明确选择主条目。",
        "合并电影或剧集时保留媒体文件、媒体源、播放进度、收藏和已看状态；剧集的季度与分集会按编号归并，未匹配项会挂接到主剧集。",
      ] },
      { kind: "fixed", items: [
        "修复分集 FANART 与 FFmpeg 缩略图可能共用 *-thumb.jpg 路径的问题；新 FANART 和缩略图使用独立路径，历史文件仍可读取。",
        "启动时修复已有的分集图片路径冲突，并压缩稀疏 FANART 索引，确保默认图片入口稳定指向首张剧照。",
        "修复详情页 Hero 操作区被高海报或 Logo 撑高的问题；操作控件保持居中并隐藏重复的装饰图片。",
      ] },
      { kind: "changed", items: [
        "被合并的根条目从目录、首页和搜索结果隐藏，后续扫描会继续将新增媒体源归入主条目。",
      ] },
    ],
  },
  {
    version: "0.4.0",
    date: "2026-09-09",
    sections: [
      { kind: "added", items: [
        "新增季度海报回退：季度没有独立海报时，详情页和剧集季度卡片会使用所属剧集海报。",
      ] },
      { kind: "changed", items: [
        "改善季度浏览时的图片连续性和内容识别度。",
        "优化管理台 Dashboard Hero 卡片高度，减少首屏空间占用。",
      ] },
    ],
  },
  {
    version: "0.3.12",
    date: "2026-09-09",
    sections: [
      { kind: "added", items: [
        "新增媒体库卡片统计展示，管理员可以直接查看媒体库中的电影、剧集、分集、媒体源和文件夹数量。",
        "新增 Lux Prism 局域网设备发现与一次性设备配对能力，便于受控设备快速连接服务。",
        "全量和增量扫描支持流式登记本地图片、NFO 等旁车目标，媒体在文件索引完成后即可更早获得本地元数据和图片。",
      ] },
      { kind: "fixed", items: [
        "修复首页缓存失效后仍可能返回旧共享快照的问题；失效后的首次请求会先刷新共享首页数据，再生成用户快照。",
        "修复扫描发现阶段结束前旁车目标登记滞后的问题，确保已索引媒体的本地元数据处理可以及时开始。",
      ] },
      { kind: "changed", items: [
        "管理台服务器概况将关键媒体统计提升为可读的 Bento 卡片，并将运行时长整合到顶部 Hero 区域，减少重复视觉层级。",
        "加强设备配对生命周期与发现服务的边界处理，限制配对次数、有效期和发现端口，降低误配对风险。",
      ] },
    ],
  },
  {
    version: "0.3.11",
    date: "2026-09-09",
    sections: [
      { kind: "added", items: [
        "TMDb 插件新增默认关闭的“原语言”模式；启用后电影和剧集优先使用原语言标题、文字和图片，并在缺少对应翻译时回退到首选语言。",
        "继续观看中的剧集分集卡片现在显示剧集名称，便于区分同名或仅显示集标题的播放记录。",
      ] },
      { kind: "changed", items: [
        "原语言图片请求携带媒体原语言提示，在本地按原语言、无语言和英语顺序筛选详情图片及独立图片候选。",
        "Lux 媒体目录接口为分集补充 seriesName 字段，供 Web 和其他客户端展示所属剧集。",
      ] },
    ],
  },
  {
    version: "0.3.10",
    date: "2026-09-08",
    sections: [
      { kind: "added", items: [
        "管理台后台任务活动现在显示当前正在处理的媒体；剧集任务同时显示剧集、季度/集数和集标题，便于定位长时间运行的任务。",
        "TMDb 语言设置按语言组展示并合并地区变体，宿主会自动兼容并归一化旧的地区语言配置。",
        "管理台支持删除自定义媒体库执行计划；删除后计划内媒体库会回到匹配的默认计划，默认计划和全局插件计划不可删除。",
      ] },
      { kind: "fixed", items: [
        "修复计划配置镜像更新时可能重复创建执行计划的问题；未变化的共享计划会复用原记录，并保留自定义计划归属。",
        "修复任务列表显示无媒体库默认计划和非扫描任务阶段标签不准确的问题。",
        "清理管理台仪表盘中重复的运行状态和管理入口面板，减少重复信息。",
        "修复剧集新增分集后首页“最新”排序仍使用剧集旧入库时间的问题；现在按可见可用分集的最新入库时间更新剧集位置。",
      ] },
      { kind: "changed", items: [
        "压缩 SQLite/PostgreSQL 扫描相关索引，限制待处理阶段索引范围并移除冗余索引；新增迁移会保留已有扫描数据和外键约束，降低扫描数据库写放大。",
        "任务活动接口统一为元数据、片头片尾和弹幕匹配任务提供当前媒体标签，并保持后台任务的有界查询。",
      ] },
    ],
  },
  {
    version: "0.3.9",
    date: "2026-09-08",
    sections: [
      { kind: "added", items: [
        "按任务类型管理执行计划，支持多 Cron、多媒体库绑定、管理台编辑和立即执行。",
        "完善本地与远程 Matroska/MKV 客户端 fallback、字幕轨道发现和手动字幕选择；远程字幕按需由浏览器直连 externalUrl。",
      ] },
      { kind: "fixed", items: [
        "修复 Matroska 字幕在 seek、切源和播放器销毁时的竞态，并在显式字幕失败时恢复原生播放。",
        "为 Matroska、字幕和 Range 解析增加边界校验与资源上限，避免无效或超大输入影响播放。",
        "修复插件同步覆盖媒体库自定义执行计划的问题。",
      ] },
      { kind: "changed", items: [
        "任务按“任务类型—执行计划—媒体库运行任务”展示，同时保留旧计划 API 兼容。",
        "实时增量扫描优先并沿用锁和资源边界；远程 `.strm` 继续使用 externalUrl 直放或客户端 fallback，不经 Lux Relay 或服务端 HLS。",
      ] },
    ],
  },
  {
    version: "0.3.8",
    date: "2026-09-04",
    sections: [
      { kind: "added", items: [
        "新增 Emby 兼容媒体删除接口，管理员可删除媒体源并查看 `CanDelete` 权限状态。",
      ] },
      { kind: "fixed", items: [
        "修复平铺电影目录的 NFO 冲突、首映日期和不可用媒体记录处理问题。",
        "修复 Hills 等客户端的播放回调、媒体时长、复合容器路径以及已看/收藏响应兼容问题。",
        "修复 URL/路径型 `.strm` 的代理播放鉴权和媒体源兼容问题，SMB/FTP 继续使用协议解析器。",
      ] },
      { kind: "changed", items: [
        "URL/路径型 `.strm` 保留原始目标并提供带短期票据的标准播放入口；本地文件和 SMB/FTP 不写入长期 token。",
      ] },
    ],
  },
  {
    version: "0.3.7",
    date: "2026-09-02",
    sections: [
      { kind: "added", items: [
        "新增管理员用户分页查询和媒体库实时文件监听开关。",
        "Emby 兼容层改用稳定纯数字条目 ID，并兼容历史 UUID 请求。",
      ] },
      { kind: "fixed", items: [
        "清理扫描生命周期中的过期记录，减少人物关系重复写入。",
        "修复增量扫描触发 STRM 探测、PostgreSQL NFO 评分及 URL/路径型 `.strm` 代理播放的问题。",
      ] },
      { kind: "changed", items: [
        "服务启动或关闭时取消未完成后台任务，不再自动恢复；任务保留为可诊断、可手动重试状态。",
        "统一 URL/路径型 `.strm` 的原始映射和标准代理入口，协议解析器目标继续走受保护入口。",
      ] },
    ],
  },
  {
    version: "0.3.6",
    date: "2026-09-01",
    sections: [
      { kind: "added", items: [
        "增加 Emby 用户资料、策略、密码、头像、创建/删除和策略复制兼容能力。",
        "播放器支持同季度上一集/下一集导航；Emby 会话支持活动窗口和未看分集统计。",
        "持久化用户配置与活动信息，并兼容 NextEmby 的 Base64 头像上传。",
      ] },
      { kind: "fixed", items: [
        "修复不存在用户、删除用户头像残留、SQLite 锁重试和 PostgreSQL 密码状态兼容问题。",
        "修复人物索引恢复重复写入，以及相同标题不同来源版本被拆成多个条目的问题。",
      ] },
      { kind: "changed", items: [
        "优化管理台服务器概况的响应式布局和活动扫描浮层，收敛用户 DTO 与播放会话活动窗口。",
      ] },
    ],
  },
  {
    version: "0.3.5",
    date: "2026-08-31",
    sections: [
      { kind: "fixed", items: [
        "兼容隐私浏览模式下缺少 Cookie/Storage 的 CSRF 写请求。",
        "放宽 Bilibili 弹幕 XML 声明和条目上限校验，并修复 Emby 播放会话字段兼容问题。",
        "修复扫描中文件短暂缺失被错误移除，以及 PostgreSQL migration 版本冲突问题。",
      ] },
      { kind: "changed", items: [
        "增加弹幕匹配覆盖与并发配置，并支持通过 `LUX_SCAN_CONCURRENCY` 调整扫库并发。",
        "优化本地 NFO/图片旁车的流式处理、首页查询和推荐统计清理，减少扫描阻塞。",
      ] },
    ],
  },
  {
    version: "0.3.4",
    date: "2026-08-31",
    sections: [
      { kind: "fixed", items: [
        "修复远程 HTTP(S) `.strm` 误进入客户端 fallback、CORS 配置不兼容和错误直连上游的问题。",
        "修复本地 NFO 评分、剧集 `tvshow.nfo` 写回、推荐分页及 Emby 用户/会话响应问题。",
        "修复未知 API 返回 HTML，以及反向代理或隐私模式下 CSRF Cookie 缺失导致 Web 写请求失败的问题。",
      ] },
      { kind: "changed", items: [
        "将章节标记并入共享时间轴，优化详情页操作和移动端布局。",
        "批量预读取目录、待确认元数据和人物数据，并按稳定批次生成首页推荐。",
      ] },
    ],
  },
  {
    version: "0.3.3",
    date: "2026-08-30",
    sections: [
      { kind: "added", items: [
        "优化移动端媒体详情布局，并为剧集详情补充移动端竖版海报。",
      ] },
      { kind: "fixed", items: [
        "修复移动端详情展示、元数据匹配并列、迁移任务恢复、实时扫描冲突和播放器时间轴问题。",
        "修复远程 `.strm` 重定向、Matroska 容器识别及 Docker runtime 依赖漂移问题。",
      ] },
      { kind: "changed", items: [
        "收紧移动端详情页海报、层级和操作区布局。",
      ] },
    ],
  },
  {
    version: "0.3.2",
    date: "2026-08-30",
    sections: [
      { kind: "added", items: [
        "增加 Emby 迁移范围、来源用户筛选、候选匹配和可恢复报告。",
        "增加首页模块可见性、账户侧栏、移动端播放快捷入口、内嵌字幕轨道和 `.strm` 弹幕匹配。",
        "增加扫描后处理进度、吞吐统计和更细的任务状态展示。",
      ] },
      { kind: "fixed", items: [
        "修复全量扫描期间实时任务排队、迁移回退、来源限流、首页加载和移动端播放器布局问题。",
        "修复字幕、截图、Provider ID、checkpoint 恢复及媒体库排序等兼容问题。",
      ] },
      { kind: "changed", items: [
        "优化迁移批处理、索引和状态合并；预加载管理台页面并降低大库扫描的数据库往返。",
        "收紧移动端播放控制和迁移向导布局，数据库 schema 更新至 103。",
      ] },
    ],
  },
  {
    version: "0.3.1",
    date: "2026-08-28",
    sections: [
      { kind: "added", items: [
        "建立 LuxPlayer 的状态、引擎、控制层和响应式 UI，支持手势、字幕、弹幕、章节、循环、画面比例、镜像与 AirPlay。",
        "增加剧集级媒体删除、数据库连接池诊断和可配置连接数。",
      ] },
      { kind: "fixed", items: [
        "修复播放会话并发切换、字幕/弹幕生命周期、移动端控件、媒体库删除和客户端鉴权兼容问题。",
        "修复混合媒体库、短期播放票据、媒体库封面缓存、扫描后处理和实时监听器问题。",
      ] },
      { kind: "changed", items: [
        "Web 播放页改用模块化 LuxPlayer，保持既有播放协议、鉴权和会话接口兼容。",
        "章节、字幕和弹幕绑定当前媒体源，并补充多客户端真机兼容性验证。",
      ] },
    ],
  },
  {
    version: "0.3.0",
    date: "2026-08-27",
    sections: [
      { kind: "added", items: [
        "增加供应商无关的元数据插件边界、专属配置、批量查询和资源写回策略。",
      ] },
      { kind: "fixed", items: [
        "修复 Emby 迁移重复遍历、权限遗漏、配置迁移、缓存防抖、多刮削器合并和 Web 图片编辑器问题。",
      ] },
      { kind: "changed", items: [
        "将 TMDb/豆瓣网络客户端和 DTO 移入独立插件，统一元数据资源命名、图片写回和有序刮削器策略。",
      ] },
    ],
  },
  {
    version: "0.2.9",
    date: "2026-08-27",
    sections: [
      { kind: "added", items: [
        "增加 Web 本地媒体播放会话、签名 Direct、0～4 档播放计划、服务端 HLS 和播放控制。",
        "增加 Redia 媒体源兼容、元数据补全资源管线和 Emby 迁移跳过报告。",
      ] },
      { kind: "fixed", items: [
        "修复 HLS 生命周期、`.strm` 越界进入转码、Redia 媒体源 ID、播放跳转和元数据重复请求问题。",
      ] },
      { kind: "changed", items: [
        "播放、HLS、`.strm` 和元数据处理均采用短期签名、有界资源配额和明确的远程访问边界。",
      ] },
    ],
  },
  {
    version: "0.2.8",
    date: "2026-08-25",
    sections: [
      { kind: "added", items: [
        "增加弹幕匹配、STRM 解析、统一任务历史、人物索引、元数据任务调度和持久化扫描变更集。",
        "增加 Emby 迁移工作区、用户状态/人物收藏导入、DisplayPreferences、人物搜索和有序刮削器配置。",
      ] },
      { kind: "fixed", items: [
        "修复 STRM 目标分类、任务管理、人物索引、插件更新、迁移恢复、多刮削器优先级和扫描旁车处理问题。",
      ] },
      { kind: "changed", items: [
        "统一计划与一次性任务记录，收紧插件更新、迁移密钥和资源配额；扫描拆分为发现、调和与后处理阶段。",
      ] },
    ],
  },
  {
    version: "0.2.7",
    date: "2026-08-20",
    sections: [
      { kind: "added", items: [
        "增加 Matroska/HEVC 客户端播放回退、完整播放器控制栏、人物查询与详情、待确认批量操作和插件更新。",
        "增加扫描活动、规范人物身份、人物匹配审核和按用户保存的媒体库排序。",
      ] },
      { kind: "fixed", items: [
        "修复 Matroska 播放、扫描 checkpoint、人物 API/图片/NFO、元数据状态和插件更新问题。",
      ] },
      { kind: "changed", items: [
        "完成媒体能力探针阶段验收，优化快照缓存、扫描生命周期、人物资源目录和跨 provider 身份匹配。",
      ] },
    ],
  },
  {
    version: "0.2.6",
    date: "2026-08-17",
    sections: [
      { kind: "added", items: [
        "增加 Emby 虚拟文件夹、共享 API Key、持久化 Webhook/outbox、通知管理、媒体能力探针和 HEVC fallback。",
        "增加 Emby 人物列表及其分页、权限和 API Key 兼容。",
      ] },
      { kind: "fixed", items: [
        "修复 Webhook 重试和安全校验、人物身份回填、混合媒体库目录及 Emby 请求解析问题。",
      ] },
      { kind: "changed", items: [
        "完善虚拟文件夹映射、人物索引、反向代理元数据、Provider ID 匹配、4K fallback 提示和 Docker 发布标签。",
      ] },
    ],
  },
  {
    version: "0.2.5",
    date: "2026-08-16",
    sections: [
      { kind: "added", items: [
        "增加服务器级共享管理员 API Key、混合媒体库类型和 TMDb 人物资料。",
      ] },
      { kind: "fixed", items: [
        "修复任务排序、旧版 STRM 计划、转发客户端 IP 和无音频流的 Emby 字段问题。",
      ] },
      { kind: "changed", items: [
        "将 API Key 改为受限文件存储，支持轮换、撤销和集中管理。",
      ] },
    ],
  },
  {
    version: "0.2.4",
    date: "2026-08-15",
    sections: [
      { kind: "fixed", items: [
        "修复 PostgreSQL 布尔值存储兼容性问题。",
      ] },
    ],
  },
  {
    version: "0.2.3",
    date: "2026-08-15",
    sections: [
      { kind: "added", items: [
        "增加管理员删除用户功能。",
      ] },
      { kind: "fixed", items: [
        "修复后台扫描进度/取消响应和 PostgreSQL migration 校验和问题。",
      ] },
      { kind: "changed", items: [
        "优化扫描任务同步、取消流程及用户管理回归覆盖。",
      ] },
    ],
  },
  {
    version: "0.2.2",
    date: "2026-08-15",
    sections: [
      { kind: "added", items: [
        "支持媒体库多根路径和登录 session 恢复。",
      ] },
      { kind: "fixed", items: [
        "修复元数据任务误处理文件夹、媒体身份/路径不稳定和 migration 校验和问题。",
      ] },
      { kind: "changed", items: [
        "默认开启实时元数据匹配，增量扫描可自动补全缺失元数据。",
      ] },
    ],
  },
  {
    version: "0.2.1",
    date: "2026-08-14",
    sections: [
      { kind: "added", items: [
        "增加管理台更新日志、元数据确认流程和共享表单控件。",
      ] },
      { kind: "fixed", items: [
        "修复登录 session、扫描并发和 Emby 条目 ID 兼容问题。",
      ] },
      { kind: "changed", items: [
        "优化首页性能，并将元数据确认统一纳入管理流程。",
      ] },
    ],
  },
  {
    version: "0.2.0",
    date: "2026-08-14",
    sections: [
      { kind: "added", items: [
        "支持每个用户独立调整自动标记已看的播放阈值，并完善收藏、进度和继续观看链路。",
        "增加本地 NFO 丰富元数据、人物资源复用、插件卸载、主题资源和响应式首页。",
      ] },
      { kind: "fixed", items: [
        "修复 Emby/Filmly 和本地 NFO 演员身份、媒体流及图片回退问题。",
      ] },
      { kind: "changed", items: [
        "将自动已看阈值改为用户设置，并同步升级 SQLite/PostgreSQL 播放设置结构。",
      ] },
    ],
  },
  {
    version: "0.1.18",
    date: "2026-08-14",
    sections: [
      { kind: "added", items: [
        "通过 Emby 输出本地 NFO 丰富元数据，并增强 Filmly 媒体流和图片字段。",
      ] },
      { kind: "fixed", items: [
        "修复 Filmly 分集、next-up、season DTO、媒体流和主图回退问题。",
      ] },
      { kind: "changed", items: [
        "将 Filmly 播放流配置限制在对应客户端，并补充兼容性记录。",
      ] },
    ],
  },
  {
    version: "0.1.17",
    date: "2026-08-13",
    sections: [
      { kind: "added", items: [
        "增强 Emby/Filmly 详情、媒体流、人物头像和图片尺寸信息。",
      ] },
      { kind: "fixed", items: [
        "兼容多种 Emby token 写法，修复媒体策略校验和媒体流类型问题。",
      ] },
      { kind: "changed", items: [
        "扩展目录详情与恢复播放回归覆盖，并更新 Logo 资源。",
      ] },
    ],
  },
  {
    version: "0.1.16",
    date: "2026-08-13",
    sections: [
      { kind: "added", items: [
        "增加章节持久化、片头片尾检测插件、TheIntroDB 在线章节源和按媒体库调度。",
        "增加章节来源配置、检测任务管理、Emby 章节输出及管理台支持。",
      ] },
      { kind: "changed", items: [
        "章节仅处理剧集/分集，结果按来源隔离，并更新插件 SDK 和兼容性边界。",
      ] },
    ],
  },
  {
    version: "0.1.15",
    date: "2026-08-12",
    sections: [
      { kind: "added", items: [
        "增加本地 NFO 丰富元数据、演员关系快照、人物头像和条目统计。",
      ] },
      { kind: "fixed", items: [
        "修复 NFO 快照失效、派生数据降级和演员信息丢失问题。",
      ] },
      { kind: "changed", items: [
        "合并 NFO 与演员信息投影，并优化详情页降级处理。",
      ] },
    ],
  },
  {
    version: "0.1.14",
    date: "2026-08-12",
    sections: [
      { kind: "added", items: [
        "增加元数据对象/图片目录、NFO 演职员读取和按架构安装插件包。",
      ] },
      { kind: "fixed", items: [
        "修复元数据写回重试和 STRM 探测任务外键迁移问题。",
      ] },
      { kind: "changed", items: [
        "升级 TMDb 插件并调整插件打包、Docker 启动和商店来源设置。",
      ] },
    ],
  },
  {
    version: "0.1.13",
    date: "2026-08-11",
    sections: [
      { kind: "added", items: [
        "增加 STRM 解析器/缩略图链路、持久化用户头像、STRM media-info 插件和远程插件商店。",
      ] },
      { kind: "fixed", items: [
        "修复 Emby/Filmly 层级、首页布局和 STRM 缩略图回退问题。",
      ] },
      { kind: "changed", items: [
        "实时增量扫描为变化的 STRM 来源创建定向探测任务，并优化首页加载。",
      ] },
    ],
  },
  {
    version: "0.1.12",
    date: "2026-08-11",
    sections: [
      { kind: "fixed", items: [
        "修复 STRM 探测缺少媒体信息及目标被过早过滤的问题。",
      ] },
    ],
  },
  {
    version: "0.1.11",
    date: "2026-08-10",
    sections: [
      { kind: "added", items: [
        "增加 STRM 定时探测任务配置、执行管理和实时元数据匹配开关。",
      ] },
    ],
  },
  {
    version: "0.1.10",
    date: "2026-08-10",
    sections: [
      { kind: "fixed", items: [
        "修复 Docker 构建占位文件路径，并将 STRM media-info 插件纳入镜像。",
      ] },
      { kind: "changed", items: [
        "对齐插件版本，优化插件、Rust 依赖和 Web 资源的构建缓存。",
      ] },
    ],
  },
  {
    version: "0.1.9",
    date: "2026-08-10",
    sections: [
      { kind: "added", items: [
        "任务记录显示所属媒体库。",
      ] },
      { kind: "fixed", items: [
        "修复媒体库排序偏好和首页排序应用问题。",
      ] },
      { kind: "changed", items: [
        "为扫描、探测和后台任务增加并发/队列边界，并为大库查询补充分页和批处理。",
      ] },
    ],
  },
  {
    version: "0.1.8",
    date: "2026-08-10",
    sections: [
      { kind: "fixed", items: [
        "修复电影图片命名、媒体探测释放和 Emby 文件夹/最新内容兼容问题。",
      ] },
      { kind: "changed", items: [
        "扫描、目录和 STRM 查询改为有界批量、分页和内存保护。",
      ] },
    ],
  },
  {
    version: "0.1.7",
    date: "2026-08-09",
    sections: [
      { kind: "fixed", items: [
        "修复元数据候选失败分类、实时 watcher 调和和横向滚动箭头问题。",
      ] },
      { kind: "changed", items: [
        "调整元数据后台处理，并完成扫描性能验证记录。",
      ] },
    ],
  },
  {
    version: "0.1.6",
    date: "2026-08-09",
    sections: [
      { kind: "fixed", items: [
        "修复横向媒体栏箭头与实际滚动位置不同步。",
      ] },
      { kind: "changed", items: [
        "补充后台扫描性能验证记录。",
      ] },
    ],
  },
  {
    version: "0.1.5",
    date: "2026-08-09",
    sections: [
      { kind: "changed", items: [
        "完成版本号维护更新；该版本没有额外的独立用户可见变化。",
      ] },
    ],
  },
  {
    version: "0.1.4",
    date: "2026-08-09",
    sections: [
      { kind: "fixed", items: [
        "增加 Redia Emby 兼容回退。",
      ] },
    ],
  },
  {
    version: "0.1.3",
    date: "2026-08-09",
    sections: [
      { kind: "added", items: [
        "增加可选 PostgreSQL Compose 服务和 migration 构建支持。",
      ] },
      { kind: "fixed", items: [
        "修复 PostgreSQL 路径，以及媒体库对话框、混合库图片、播放诊断和 Emby ID 过滤问题。",
      ] },
    ],
  },
  {
    version: "0.1.2",
    date: "2026-08-09",
    sections: [
      { kind: "added", items: [
        "支持首次引导选择 SQLite/PostgreSQL，增加跨后端存储和结构化日志归档。",
      ] },
      { kind: "fixed", items: [
        "修复 PostgreSQL 搜索/健康诊断、bootstrap、合集可见性和日志导出限制问题。",
      ] },
    ],
  },
  {
    version: "0.1.1",
    date: "2026-08-08",
    sections: [
      { kind: "fixed", items: [
        "隔离本地元数据刷新失败，并修复 Emby 媒体库详情兼容问题。",
      ] },
    ],
  },
  {
    version: "0.1.0",
    date: "2026-08-02",
    sections: [
      { kind: "added", items: [
    "建立 Lux Rust 模块化单体服务，涵盖初始化认证、SQLite migration、统一错误和可观测性基础。",
        "支持电影、电视剧和混合媒体库、增量扫描、访问控制、NFO/图片/ffprobe 索引及元数据写回。",
        "支持剧集层级、合集、字幕、本地直放、`.strm`、多版本媒体源、进度、收藏、搜索和下载权限。",
        "建立 Lux Web 与管理台，覆盖登录、浏览、搜索、详情、继续观看、用户权限、元数据审核和任务诊断。",
        "增加 Docker/Compose 部署、ARM64 验证、浏览器 smoke、性能基线和恢复演练。",
      ] },
    ],
  },
];

const sectionMeta: Record<ChangelogSectionKind, { label: string; icon: typeof Sparkles }> = {
  added: { label: "新增", icon: Sparkles },
  fixed: { label: "修复", icon: Wrench },
  changed: { label: "变更", icon: ArrowRightLeft },
};

export function AdminChangelogPage() {
  const latestRelease = changelogReleases[0];

  return (
    <main className="lux-admin-page lux-changelog-page">
      <header className="lux-changelog-header">
        <div className="lux-changelog-header-mark" aria-hidden="true"><BookOpen size={21} /></div>
        <div>
          <span className="lux-eyebrow">项目历史</span>
          <h1>更新日志</h1>
          <p>记录 Lux 每一次让媒体管理更清晰、更可靠的更新。</p>
        </div>
        <div className="lux-changelog-current" aria-label={`当前版本 ${latestRelease.version}`}>
          <span>当前版本</span>
          <strong>v{latestRelease.version}</strong>
          <small>{latestRelease.date}</small>
        </div>
      </header>

      <div className="lux-changelog-intro">
        <div className="lux-changelog-intro-line" aria-hidden="true"><GitBranch size={16} /></div>
        <p>从 <strong>0.1.0</strong> 开始，按版本倒序整理项目主要的用户可见变化。</p>
        <span>{changelogReleases.length} 个版本</span>
      </div>

      <div className="lux-changelog-timeline">
        {changelogReleases.map((release) => (
          <article className="lux-changelog-release" key={release.version}>
            <div className="lux-changelog-release-rail" aria-hidden="true"><span /></div>
            <div className="lux-changelog-release-body">
              <header className="lux-changelog-release-header">
                <div>
                  <span className="lux-changelog-release-kicker">RELEASE</span>
                  <h2>v{release.version}</h2>
                </div>
                <time dateTime={release.date}>{release.date}</time>
              </header>
              <div className="lux-changelog-sections">
                {release.sections.map((section) => {
                  const { icon: Icon, label } = sectionMeta[section.kind];
                  return (
                    <section className={`lux-changelog-section is-${section.kind}`} key={section.kind}>
                      <h3><Icon size={14} aria-hidden="true" />{label}</h3>
                      <ul>
                        {section.items.map((item) => <li key={item}>{item}</li>)}
                      </ul>
                    </section>
                  );
                })}
              </div>
            </div>
          </article>
        ))}
      </div>

      <p className="lux-changelog-note">日志根据仓库源码版本记录整理，日期不等同于各渠道的正式发布日期。</p>
    </main>
  );
}
