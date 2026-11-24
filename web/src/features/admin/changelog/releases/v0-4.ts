import type { ChangelogRelease } from "../types";

export const changelogReleases04: ChangelogRelease[] = [
  {
    version: "0.4.15",
    date: "2026-09-23",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(emby): Emby 单条媒体详情补上媒体源、播放时长、容器、文件大小和码率，请求只带有限的 Fields 也能拿到",
        "🐛 fix(web): 播放进度更新后首页会及时跟着刷新",
      ] },
    ],
  },
  {
    version: "0.4.14",
    date: "2026-09-23",
    sections: [
      { kind: "added", items: [
        "✨ feat(image): 登录页的动态背景最多用 40 张海报",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(image): 登录页海报墙的位置挪过",
        "♻️ refactor(auth): 登录表单换成 Lux 的白色强调色",
      ] },
    ],
  },
  {
    version: "0.4.13",
    date: "2026-09-22",
    sections: [
      { kind: "added", items: [
        "✨ feat(playback): 播放会话里能直接看到对应用户名称、当前播放的媒体标题和可用的剧集、集信息",
        "✨ feat(emby): Emby 会话补上简介、原标题、层级关系、季名称和 ProviderIds",
        "✨ feat(scan): 扫描任务活动接口多了发现阶段状态",
        "✨ feat(metadata): 补全元数据时跳过已经有完整本地元数据的条目",
        "✨ feat(emby): 补齐 Qmby 要的 Emby 接口",
        "✨ feat(emby): Emby 侧能创建合集、增删成员、上传媒体库封面",
        "✨ feat(emby): Emby 目录支持 IsPlayed/IsUnplayed、MinDateLastSaved、合集子项和最多 1000 条分页",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(emby): Emby 标准视频入口认证后用 302 把原始地址交给客户端，不再被 Lux 的预探测负载均衡绕一圈",
        "🐛 fix(db): 条目 Path 返回已索引的 .strm 路径，MediaSources[].Path 保留 STRM 目标",
        "🐛 fix(emby): Matroska 的几种写法统一报成 mkv",
        "🐛 fix(emby): PlaybackInfo 里 Matroska 别名的容器值都返回 mkv",
        "🐛 fix(auth): 签名过的 Lux DirectStreamUrl 除了代理用户提示和短期票据",
        "🐛 fix(deploy): 设备配置里的容器对不上时",
        "🐛 fix(playback): 直播放地址补上 PlaySessionId 和 static=true",
        "🐛 fix(emby): URL 和路径型 .strm 仍向外部 Emby 代理声明 SupportsTranscoding",
        "🐛 fix(playback): URL 和路径型 .strm 的 MediaSource.Name 不会再是空值",
        "🐛 fix(playback): 带设备配置的 URL 型 STRM 先改成走标准 /Videos 交接",
        "🐛 fix(api): 后来又改回返回 Lux 的 DirectStreamUrl",
        "🐛 fix(image): 登录页海报瀑布流的位置、缩放和错落布局重新对过",
        "🐛 fix(image): 海报墙固定成五列",
        "🐛 fix(image): 列间距收紧",
        "🐛 fix(image): 海报墙列数固定成五列",
        "🐛 fix(image): 列间距调成 12px",
        "🐛 fix(probe): TranscodingInfo 的码率字段不再为 null",
        "🐛 fix(emby): 没有 PlaySessionId 或 DeviceId 的 Emby 停止回调",
        "🐛 fix(web): ActiveWithinSeconds 不会突破服务端的 90 秒失活阈值",
        "🐛 fix(scan): 目录发现和扫描对账改成批量提交，缺失文件处理和元数据目标通知也顺手理顺",
      ] },
    ],
  },
  {
    version: "0.4.12",
    date: "2026-09-21",
    sections: [
      { kind: "added", items: [
        "✨ feat(api): 登录页改成媒体中心风格的响应式分栏",
        "✨ feat(web): 密码输入框多了显示和隐藏的切换按钮",
        "✨ feat(auth): 管理员可以选登录页背景的来源",
        "✨ feat(image): 登录页通过新接口动态取媒体库海报",
        "✨ feat(auth): 登录页加上 Lux 品牌标识",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(image): 登录页的动态背景只挑电影和剧集的海报",
        "🐛 fix(image): 动态登录海报改成三列倾斜瀑布流",
        "🐛 fix(emby): 部分 Emby 客户端把 MaxAudioChannels 当字符串发过来",
        "🐛 fix(scan): 扫描期间的刷新通知会合并",
      ] },
    ],
  },
  {
    version: "0.4.11",
    date: "2026-09-20",
    sections: [
      { kind: "added", items: [
        "✨ feat(emby): Emby 搜索和媒体列表能按 Provider ID 精确过滤",
        "✨ feat(emby): 有些 Emby 客户端拉分集只请求第一页、不继续翻页",
        "✨ feat(auth): 用户管理可以分别启用、禁用和永久删除账户",
        "✨ feat(emby): 新增 Emby 媒体库增量刷新和扫描状态的兼容接口",
        "✨ feat(web): 播放活动和通知里补上 Web 播放会话信息",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(media): 普通搜索和搜索提示默认只返回电影和整剧，不再默认塞季度和单集，结果不重复、层级也不乱",
        "🐛 fix(auth): Provider ID、类型筛选和 ACL 一起用时",
        "🐛 fix(api): VidHub 只请求第一个剧集页、不继续翻页",
        "🐛 fix(api): 剧集接口不传分页参数时按安全上限返回完整列表",
      ] },
    ],
  },
  {
    version: "0.4.10",
    date: "2026-09-20",
    sections: [
      { kind: "added", items: [
        "✨ feat(probe): 管理台「正在播放」能看到真实播放方式、服务端 HLS 档位、输出容器、视频和音频 codec 以及码率",
        "✨ feat(emby): Emby 会话详情补上服务端转码的输出信息",
        "✨ feat(auth): 新增服务器级的「媒体库顺序强制按照管理员排序」和用户级的「按照管理员顺序排序」开关",
        "✨ feat(subtitle): 媒体详情页可以在播放前选音轨和字幕轨道",
        "✨ feat(auth): 用户管理把「禁用账户」和「永久删除账户」分开了",
        "✨ feat(web): 首页轮播的指示器改成进度点",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(playback): 有活动的 HLS 会话时",
        "🐛 fix(auth): 登录表单补上标准的用户名和密码自动填充字段",
        "🐛 fix(auth): 管理员策略和用户个人设置之间来回切时",
        "🐛 fix(subtitle): 播放请求里选过音轨之后",
      ] },
    ],
  },
  {
    version: "0.4.9",
    date: "2026-09-19",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(emby): Harbor/Emby 切码率或续播时，HLS 会话以前会在第一个资源请求前启动又被立刻取消，视频播不出来",
        "🐛 fix(api): init 和 seek 分片并发请求时不会再读到不同 generation 的资源",
        "🐛 fix(playback): 无效或者超出媒体总时长的 StartTimeTicks 不会再让转码从片尾启动失败",
        "🐛 fix: 远距离 seek 或并发切 generation 时，init 和分片不会混用不同时间基准",
        "🐛 fix(api): seek 期间 init 还没写完或被替换时，Harbor 不会收到空资源",
        "🐛 fix(emby): Emby 客户端没声明 fMP4 也能稳稳切 HLS 播放流了",
        "🐛 fix(playback): HLS 的分片容器、文件扩展名、MIME 类型、清单里的 init 结构和签名资源对齐了",
      ] },
    ],
  },
  {
    version: "0.4.8",
    date: "2026-09-18",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(emby): 第三方 Emby 客户端读 Lux 的 HLS 时，不再只看到已转码片段的时长",
        "🐛 fix(playback): 续播时虚拟 VOD 时间线和实际分片编号对上了",
        "🐛 fix(playback): 客户端没发 StartTimeTicks 时按已保存的播放进度续播",
        "🐛 fix(playback): 续播 HLS 清单里的逻辑分片名能映射回 FFmpeg 的实际起始分片",
        "🐛 fix: 有些客户端续播时会先探时间线前段的分片，现在不会读取失败",
        "🐛 fix(emby): FFmpeg 输入 seek 以前会把媒体时间戳归零、Emby 的 VOD 时间线就断了",
        "🐛 fix(api): 远距离 seek 或者续播时，清单的逻辑时间线和 FFmpeg 输出一致了",
      ] },
    ],
  },
  {
    version: "0.4.7",
    date: "2026-09-18",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 管理台建通知改成弹窗流程",
        "✨ feat(scan): 通知模板补上媒体库、扫描进度、任务耗时、状态和错误上下文",
        "✨ feat(emby): 新增 Emby 的 GET /Library/MediaFolders 和 POST /Items/{FolderId}/Refresh，可按已登记目录单独异步刷新",
        "✨ feat(api): 新增 POST /api/v1/admin/libraries/{libraryId}/scan-path",
      ] },
      { kind: "changed", items: [
        "⚡ perf(scan): 目录刷新复用现有的增量扫描队列",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(media): 改完媒体库的缩略图策略后，已有媒体缺的缩略图能补回来了",
        "🐛 fix(playback): 没有 PlaySessionId 的过期 STOPPED 回调不会再顺手停掉当前的 HLS 转码会话",
        "🐛 fix(metadata): 本地 NFO 里没有演员身份信息时不会再覆盖已有的演员档案",
        "🐛 fix(scan): 首页推荐请求以前会顺手刷新全局推荐统计，迁移或者扫描期间就卡在数据库写事务上",
        "🐛 fix(emby): Emby HLS 转码切换或者续播时会认 StartTimeTicks",
      ] },
    ],
  },
  {
    version: "0.4.6",
    date: "2026-09-18",
    sections: [
      { kind: "added", items: [
        "✨ feat(plugin): 通知事件的 source、title、content、body 和 ISO timestamp 由 Lux 统一生成",
        "✨ feat(probe): 播放通知补上媒体标题、用户、容器、大小、码率、播放方式、设备和恢复播放这些信息，停止事件还带远程 IP 和简介",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(scan): 全量扫描会读已有的 URL 型 .strm 的 *-mediainfo.json 旁车",
      ] },
    ],
  },
  {
    version: "0.4.5",
    date: "2026-09-17",
    sections: [
      { kind: "added", items: [
        "✨ feat(db): 首次设置好 PostgreSQL 后",
        "✨ feat(metadata): 媒体库策略新增缩略图刮削方式",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(plugin): manifest 等待失败、播放停止或者客户端中断之后",
        "🐛 fix(scan): 扫描过程中不再重复更新媒体可用性状态",
      ] },
      { kind: "changed", items: [
        "⚡ perf(probe): 扫描、探测和缩略图/FFmpeg 的并发分开控制",
        "♻️ refactor(plugin): 通知器管理支持插件声明的 URL 模板和 Body 模板字段",
        "♻️ refactor(plugin): 通知器插件的目标地址放宽了，可以保留第三方服务需要的查询参数、认证信息和片段",
        "🔧 build(deploy): Docker 部署默认以 root 运行",
      ] },
    ],
  },
  {
    version: "0.4.4",
    date: "2026-09-16",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(emby): Emby 实时 HLS 转码的播放时长不再跟着片段长度走",
        "🐛 fix(playback): 播放回调带的是动态 HLS 当前清单的长度",
        "🐛 fix(plugin): HLS 清单以前错标成 VOD，FFmpeg 在转码结束前不出 manifest，第三方客户端只能干等",
        "🐛 fix(playback): 用户放弃播放或者发来停止事件后",
      ] },
    ],
  },
  {
    version: "0.4.3",
    date: "2026-09-16",
    sections: [
      { kind: "added", items: [
        "✨ feat(image): 本地视频缺有效海报或缩略图时",
        "✨ feat(emby): Lux 自有接口支持用户级 Emby AccessToken",
        "✨ feat(image): 管理员在媒体库里点一下封面就能打开媒体库编辑器",
        "✨ feat(metadata): 管理员手动搜元数据时",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): 刮削器失败时不再返回数据库不可用的错误码",
        "🐛 fix(plugin): 提交插件配置时只发清单里声明过的 UI 字段",
        "🐛 fix(emby): Emby 客户端只发 EnableTranscoding=true、省略 EnableDirectPlay 时",
        "🐛 fix(db): 播放开关放在 POST 查询参数、DeviceProfile 放在 body 时",
        "🐛 fix(playback): 真要走服务端转码时不再保留直放入口",
        "🐛 fix(emby): MaxStreamingBitrate 进了 PlaybackInfo 协商",
        "🐛 fix(playback): 同一媒体源重复协商、切换转码流时",
        "🐛 fix(playback): 转码 offer 里的 VideoBitrate 以前只写进 URL",
        "🐛 fix(playback): 实时 HLS 转码的进度条不再随已生成片段变长",
        "🐛 fix(emby): Emby 客户端可以在 DeviceProfile 里声明直放和 HLS 转码能力",
        "🐛 fix(probe): 转码 offer 补齐设备、codec、码率、轨道和分片参数",
        "🐛 fix(scan): Lux 内部生成或者写入图片后不会再触发实时扫描",
        "🐛 fix(probe): 媒体探测信息缺失时",
        "🐛 fix(probe): 实时增量扫描能及时探测到新增或者变化的本地媒体源",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(image): 本地图片、在线刮削图片和 FFmpeg 截图的优先级理清了",
        "♻️ refactor(api): 客户端令牌请求照样执行用户媒体库 ACL",
        "♻️ refactor(web): 首页的继续观看改成每次请求实时读取",
        "♻️ refactor(emby): PlaybackInfo 协商",
        "♻️ refactor(metadata): Web API 类型定义补齐了播放次数、排序标题、人物创建时间、首页最近添加总数和播放状态条目 ID 等字段",
        "♻️ refactor(emby): Emby HLS 转码协商和资源请求加了诊断日志",
        "♻️ refactor(scan): 实时增量扫描只处理这次任务登记的新增或变化本地 source",
      ] },
    ],
  },
  {
    version: "0.4.2",
    date: "2026-09-15",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 单季剧集的详情页直接列出分集",
        "✨ feat(image): 分集列表和同季分集卡片的图片区域多了播放入口",
        "✨ feat(emby): Emby 客户端能通过 PlaybackInfo 为本地媒体协商服务端 HLS 转码",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(playback): 分集浏览把播放和看详情分开了",
        "♻️ refactor(playback): 转码清单和分片用短期签名并绑定播放会话",
      ] },
    ],
  },
  {
    version: "0.4.1",
    date: "2026-09-10",
    sections: [
      { kind: "added", items: [
        "✨ feat(media): 管理员在媒体库多选模式里",
        "✨ feat(playback): 合并后媒体文件、媒体源、播放进度、收藏和已看状态都保留",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(media): 分集 FANART 和 FFmpeg 缩略图以前可能共用 *-thumb.jpg",
        "🐛 fix(image): 启动时会修掉已有的分集图片路径冲突",
        "🐛 fix(web): 详情页 Hero 的操作控件保持居中",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(web): 被合并的根条目从目录、首页和搜索结果里隐藏",
      ] },
    ],
  },
  {
    version: "0.4.0",
    date: "2026-09-09",
    sections: [
      { kind: "added", items: [
        "✨ feat(image): 季度没有独立海报时",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(image): 季度浏览时图片更连贯",
        "⚡ perf(web): 管理台首页的 Hero 卡片降低了高度",
      ] },
    ],
  },
];
