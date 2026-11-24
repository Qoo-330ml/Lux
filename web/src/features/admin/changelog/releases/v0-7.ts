import type { ChangelogRelease } from "../types";

export const changelogReleases07: ChangelogRelease[] = [
  {
    version: "0.7.26",
    date: "2026-11-24",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 通知页重做 — 目的地按启用状态筛选、可按名称搜索、带提供商图标，新建收进面板标题并默认收起，编辑走独立弹窗（换提供商会重新要凭据）；投递列表能按状态和目的地缩小范围、服务端分页，失败的投递可以直接逐条重试。",
        "✨ feat(web): 执行计划有了独立工作台 — 任务页里那些已注册计划从任务列表拆出来，可以新建、编辑、克隆、停用和删除，并显示 cron 对应的本地时间与 UTC 时间；页面和任务列表同一轮预取，/admin/jobs 一次请求就能画完。",
        "✨ feat(web): 人物页变成档案页 — 简介默认折叠可展开，也能打开详情弹窗；顶部按电影/剧集给出作品数，作品按年份倒序分组并可按类型筛选，切换人物时照片不再重闪。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(storage): PostgreSQL 下通知投递列表数不出来 — 计数用的还是裸 SQL，SQLite 的 ? 占位符原样进了 PostgreSQL，带状态或目的地筛选时直接报错。现在计数和列表都走数据库适配器，占位符按后端重写。",
        "🐛 fix(web): 执行计划工作台的边界情况 — 新建只列出已经有媒体库级计划的任务类型（没有就禁用）；某类型最后一份计划被移走后会提示并可删除空计划；搜索时统计仍按真实覆盖计算；cron 字段越界（分钟 60、小时 24）直接判非法。",
      ] },
    ],
  },
  {
    version: "0.7.25",
    date: "2026-11-23",
    sections: [
      { kind: "added", items: [
        "✨ feat(api): 客户端令牌能调管理接口了 — 管理员用用户级客户端令牌就能访问 /api/v1/admin/*、条目图片搜索/选中和元数据编辑，不用再带 Web Cookie 与 CSRF；/api/v1/auth/me 改成返回认证主体，管理入口按 canManageServer 判断。API Key 管理和注册审批这类高敏端点仍然只认 Web session。",
        "✨ feat(api): 新增头像和会话自助接口 — GET|PUT /api/v1/auth/avatar 读写当前用户头像（只收原始图片，上限 5 MiB，不裁切）；GET /api/v1/auth/sessions 与 DELETE /api/v1/auth/sessions/{id} 同时管 Web 会话和客户端令牌会话，正在用的那个不能自己撤。资料、邮箱验证、账户偏好和改密都支持令牌，改密后只保留当前这个登录。",
        "✨ feat(api): 新增 POST /api/v1/admin/libraries/scan — 一次给所有已启用媒体库排队扫描，已有任务会复用，返回 202 和任务列表。",
        "✨ feat(emby): POST /Items/{id} 能编辑电影/剧集元数据了 — 支持名称、原始标题、简介、年份和字段锁，没传的字段保持原值、传 null 清空可选字段，不认识的字段忽略而不是整条拒绝；写回 NFO 并记审计，成功 204。以前这条只认人物，Yamby/Hills 的「编辑元数据」会报错。",
        "✨ feat(emby): 条目图片能上传和按槽替换 — POST /Items/{id}/Images/{Type} 接受原始图片或 Base64（解码后上限 5 MiB），Backdrop 带索引时替换对应槽位而不是往后追加，单图类型的非零索引返回 404；DELETE 也接受不带 /Delete 的写法。",
        "✨ feat(emby): 补齐 /Users/Me、纯 ProviderIds 的远程搜索，以及 /Sessions 与停止播放的管理权限（改用 canManageServer，和 Lux 会话页一致）。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(emby): 图片上传与编辑的评审修正 — 带索引的上传不再因路径段数报错；标准 DTO 回显的 LockData: false、LockedFields: [] 不再清掉管理员设的字段锁（Emby 侧只增不减），未改动字段的来源不会被降级；没带 Type 的人物编辑不再撞上媒体编辑的 403/404。",
      ] },
    ],
  },
  {
    version: "0.7.24",
    date: "2026-11-23",
    sections: [
      { kind: "added", items: [
        "✨ feat(catalog): 「最近添加」按剧集的最新可播放分集排序 — 剧集新增分集后会回到媒体库列表最前，已移除或没有可用文件的分集不参与；电影、季和分集仍按自身入库时间，Lux 与 Emby 用同一语义。迁移 0252 增加排序列与索引并回填，升级后读模型会自动重建。",
        "✨ feat(home): 首页轮播条目带上 genres — /api/v1/home/carousel 的每个推荐条目新增 genres 数组，取自本地 NFO；没有 NFO 或读取失败时是空数组，其它字段不变。Web 类型同步，页面暂时还没用。",
      ] },
    ],
  },
  {
    version: "0.7.23",
    date: "2026-11-18",
    sections: [
      { kind: "added", items: [
        "✨ feat(emby): 扫描任务改名 RefreshLibrary，并支持从任务面板启动",
        "✨ feat(emby): /Items/{id}/Refresh 按客户端意图分流为元数据刷新与文件扫描",
        "✨ feat(emby): 新增 POST /Items/RemoteSearch/{Type} 与 Apply",
        "✨ feat(emby): 新增 RemoteImages 列表、下载与图片删除端点",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(emby): DELETE /Items/{id} 的 id 映射修正，并接受 /Delete 后缀",
      ] },
    ],
  },
  {
    version: "0.7.22",
    date: "2026-11-16",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(emby): camelCase 查询参数不再被静默丢弃",
        "🐛 fix(emby): 演员署名改成 Actor，并补上 ImageTags.Backdrop",
        "🐛 fix(home): 每日推荐与首页轮播的行带上外部 ID",
      ] },
    ],
  },
  {
    version: "0.7.21",
    date: "2026-11-15",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(metadata): 候选搜索失败时说清是哪种失败",
        "🐛 fix(metadata): 带版本标签的季目录也能定位剧集目录",
        "🐛 fix(images): 空的图片搜索不再和「还没搜」长得一样",
        "🐛 fix(images): 图片搜索会告诉你哪个来源回答的、返回了什么",
      ] },
    ],
  },
  {
    version: "0.7.20",
    date: "2026-11-15",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(probe): 网页从原始地址直连播放也会补媒体信息",
        "🐛 fix(probe): 播放补提取改用客户端自己的 User-Agent",
      ] },
    ],
  },
  {
    version: "0.7.19",
    date: "2026-11-14",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(probe): 播放补 STRM 媒体信息不再挑库、也不再排队",
        "🐛 fix(admin): 元数据写回失败按真实原因返回状态码",
        "🐛 fix(metadata): 同一份 NFO 的并发写改成排队",
      ] },
    ],
  },
  {
    version: "0.7.18",
    date: "2026-11-12",
    sections: [
      { kind: "added", items: [
        "✨ feat(api): 下载 .strm 不再由 Lux 中转",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(downloads): 删掉不再用的上游中继",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(web): 季度页的分集卡片改用 360 的图",
      ] },
    ],
  },
  {
    version: "0.7.17",
    date: "2026-11-11",
    sections: [
      { kind: "added", items: [
        "✨ feat(api): 条目详情会告诉你文件在哪",
        "✨ feat(probe): 片源现在能标出 HDR 了",
      ] },
      { kind: "changed", items: [
        "✨ feat(web): 首页轮播图会跟着容器形状换图",
      ] },
    ],
  },
  {
    version: "0.7.16",
    date: "2026-11-09",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 电影和剧集详情页多了「相似推荐」",
        "✨ feat(api): 相似推荐和标签现在都有 Lux 自己的接口了",
        "✨ feat(emby): Emby 客户端的筛选面板不再是空标签",
        "✨ feat(danmaku): 网页能直接刮弹幕了",
        "✨ feat(scan): 扫描器开始认 ts、m2ts、mov、webm、avi、asf、wmv、flv",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(plugin): 插件配置页不再为某个插件写死表单",
        "📝 docs(plugin): 给写插件的人补了文档",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(plugin): 只做评分的插件不会再被选成刮削器",
        "🐛 fix(metadata): 已经存下的、不受支持的刮削器不会再被悄悄清掉",
        "🐛 fix(metadata): 评分能力的清单校验收紧了",
      ] },
    ],
  },
  {
    version: "0.7.15",
    date: "2026-11-07",
    sections: [
      { kind: "added", items: [
        "✨ feat(realtime): 网页的实时推送换成了一条 WebSocket",
        "✨ feat(emby): Emby 详情页的「相似推荐」有数据了",
        "✨ feat(emby): Emby 的筛选面板不再是空的",
        "✨ feat(emby): Yamby 的「服务器状态」页不再疯狂重连",
        "✨ feat(plugin): Trakt 评分能按计划写回 NFO 了",
        "✨ feat(emby): STRM 条目给 Emby 的媒体源 id 换成 32 位十六进制",
        "✨ feat(metadata): 条目和媒体库菜单的「刷新元数据」会跟着刮削模式改名字",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(media): 复制出来的季目录不再多出一整套剧集",
        "🐛 fix(image): 已有的光盘图不会再被海报覆盖",
        "🐛 fix(image): 同一个条目的多张背景图不会再抢同一个文件",
        "🐛 fix(plugin): 改完插件配置，刮削马上用新配置",
        "🐛 fix(emby): Emby 查询不再因为空的可选数值参数返回 400",
        "🐛 fix(api): Filters=IsResumable 和 Filters=IsFavorite 真的生效了",
        "🐛 fix(media): 建合集、往合集里加条目不再偶发 503",
        "🐛 fix(probe): STRM 探测任务不会永远挂在「运行中」",
        "🐛 fix(api): 同源握手不再误拒「默认端口写成带端口」的请求",
      ] },
      { kind: "changed", items: [
        "💥 breaking(realtime): 两条 SSE 端点删掉了",
        "♻️ refactor(realtime): 反代要补 WebSocket 的配置",
      ] },
    ],
  },
  {
    version: "0.7.14",
    date: "2026-11-05",
    sections: [
      { kind: "added", items: [
        "✨ feat(media): 同季不同版本的目录会合并成一个剧集的多个版本",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(image): 主程序不再提供「最小下载宽度」",
      ] },
    ],
  },
  {
    version: "0.7.13",
    date: "2026-11-05",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(emby): Emby/Jellyfin 写法的 <ratings> 容器现在能读到评分",
        "🐛 fix(image): 刮削器只给了海报字段时不再丢封面",
        "🐛 fix(image): 上游给了海报地址的条目不再被永久判成「没有图」",
        "🐛 fix(db): 首次启动时选了 PostgreSQL 会卡在空数据库上起不来",
      ] },
    ],
  },
  {
    version: "0.7.12",
    date: "2026-11-05",
    sections: [
      { kind: "added", items: [
        "✨ feat(emby): Emby 图片按客户端要的尺寸走缓存了",
        "✨ feat(emby): Emby 图片请求有了脱敏诊断日志",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(image): 管理仪表盘的「近期活动」不再加载封面和章节",
        "♻️ refactor(db): 用户活动索引默认只留 7 天",
        "♻️ refactor(db): 管理仪表盘的「近期活动」改读活动索引",
        "♻️ refactor(log): 管理员审计和任务事件列表改成有界读取",
        "♻️ refactor(log): 运行日志经过脱敏",
        "♻️ refactor(web): 详情页的横向分隔线去掉了",
        "⚡ perf(web): 管理后台打开得更快了",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(log): 登录失败现在有稳定的结构化日志事件",
        "🐛 fix(log): 日志写入失败后不再重复输出已经写成功的行",
      ] },
    ],
  },
  {
    version: "0.7.11",
    date: "2026-11-04",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 播放器中间那颗按钮换成了 Lux 标记",
        "✨ feat(web): 播放器设置抽屉重做",
        "✨ feat(emby): Emby 字幕轨带上了投递信息",
        "✨ feat(web): 播放器能读远程 HTTPS 媒体了",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(subtitle): 字幕下载地址支持带格式后缀",
        "♻️ refactor(emby): Emby 视频的 DisplayTitle 先显示分辨率",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(web): 播放器设置面板点外面收不起来",
        "🐛 fix(web): 倍速按钮把文字挤在图标旁边",
        "🐛 fix(api): .strm 直连播放到第二个请求就 410",
      ] },
    ],
  },
  {
    version: "0.7.10",
    date: "2026-11-03",
    sections: [
      { kind: "added", items: [
        "✨ feat(danmaku): 弹幕可以手动指定某一集了",
        "✨ feat(danmaku): 弹幕的合法性规则前后端共用一份",
        "✨ feat(emby): Emby 兼容层多了类型、厂牌和年份",
        "✨ feat(media): 媒体库页能筛选了",
        "✨ feat(api): 观看历史页",
        "✨ feat(emby): Emby 的流信息更完整",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(danmaku): 弹幕模块拆开重写了",
        "♻️ refactor(danmaku): Web 弹幕解析和渲染重建",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(emby): 电视剧库列表空了",
        "🐛 fix(web): 首页/收藏/账户页样式丢了",
        "🐛 fix(danmaku): 弹幕设置连续改动合并成一次写入、默认值深拷贝",
        "🐛 fix(danmaku): 插件返回超大弹幕响应时按无效响应处理",
      ] },
    ],
  },
  {
    version: "0.7.9",
    date: "2026-11-02",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 播放器的控制条重排了",
        "✨ feat(metadata): 每日推荐改成按权重抽",
        "✨ feat(emby): 外部通知插件能直接拿宿主拼好的 Emby 对象",
        "✨ feat(web): 新通知的入口挪到目标面板里",
      ] },
      { kind: "changed", items: [
        "✨ feat(web): 首页轮播会自己停",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(web): 轮播暂停的几种别扭情况",
      ] },
    ],
  },
  {
    version: "0.7.8",
    date: "2026-11-02",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 后台通知能翻页和筛选了",
        "✨ feat(emby): 发给 Emby 的 webhook 和官方插件对齐了",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(probe): Linux 上换成 jemalloc",
        "♻️ refactor(probe): 第一次播放先把媒体信息补上",
      ] },
    ],
  },
  {
    version: "0.7.7",
    date: "2026-11-02",
    sections: [
      { kind: "added", items: [
        "✨ feat(plugin): 通知渠道可以做成插件",
        "✨ feat(playback): 播放兼容性对照工具（scripts/playback-matrix/）",
        "✨ feat(web): Web HLS 的 AAC 声道布局规整了",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(plugin): 插件搬到独立仓库",
        "♻️ refactor(playback): HLS 首个播放列表的等待放宽到 30 秒",
        "♻️ refactor(probe): 实时增量扫描也会探测新文件",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(web): 回退后还被当成已停止",
        "🐛 fix(danmaku): 弹幕接口 404 被当成报错",
        "🐛 fix: 非内置 webhook 的通知 provider 不再显示接收地址输入框",
      ] },
    ],
  },
  {
    version: "0.7.6",
    date: "2026-11-02",
    sections: [
      { kind: "added", items: [
        "✨ feat(metadata): 自动连播下一集",
        "✨ feat(subtitle): 播放页内切换音轨",
        "✨ feat(probe): 按分辨率选画质",
        "✨ feat(probe): 手动画质上限（码率）",
        "✨ feat(playback): 原生直放静默失败会自动回退",
        "✨ feat(playback): 播放不可用时说人话",
        "✨ feat(web): 快捷键帮助",
        "✨ feat(playback): 转码管理页重做",
        "✨ feat(web): 媒体库菜单里那个禁用的「复制」入口去掉了",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(probe): 浏览器端软解 HEVC 限制到 ≤1080p",
        "⚡ perf(probe): 奇数分辨率上限向下取偶",
        "📝 docs(web): README 和 agent 规则对齐「分层 Web 播放 + 受限转码在范围内」（ADR-004 / ADR-026）",
      ] },
      { kind: "fixed", items: [
        "🐛 fix: 4K HEVC 等 MKV 在浏览器里重新封装失败（MKV MSE SourceBuffer append failed）",
        "🐛 fix(playback): 客户端解码卡死不回退",
        "🐛 fix(api): 原生 MKV 直放的静默失败",
        "🐛 fix(probe): 10-bit H.264/VP9/AV1 改用真实位深编码串探测",
        "🐛 fix(playback): 播放页交互",
      ] },
    ],
  },
  {
    version: "0.7.5",
    date: "2026-11-01",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 手机端后台导航",
        "✨ feat(web): 看板合并成一张资源卡",
        "✨ feat(playback): 内存也显示百分比",
        "✨ feat(log): 播放活动日志能直接读",
      ] },
      { kind: "changed", items: [
        "⚡ perf(metadata): 空闲唤醒改成写事件驱动",
        "♻️ refactor(playback): 播放准入收敛到 30 秒一次",
        "♻️ refactor(metadata): 元数据和人物路径少开事务",
        "⚡ perf(db): 存储和查询减负",
        "♻️ refactor(db): 升级注意（0.7.4 → 0.7.5）",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(playback): 播放中撤权不生效",
        "🐛 fix(api): 库页缓存来回翻转",
        "🐛 fix(probe): 关停时收尾更完整",
        "🐛 fix(scan): 扫描恢复后消费者不醒",
        "🐛 fix(metadata): 手动跑完元数据任务不刷新",
      ] },
    ],
  },
  {
    version: "0.7.4",
    date: "2026-10-31",
    sections: [
      { kind: "added", items: [
        "✨ feat(db): SQLite 同步级别可配",
        "✨ feat(web): 详情页顶部横版背景",
        "✨ feat(playback): 删除进度挪到媒体库页",
        "✨ feat(probe): STRM 探测失败写清原因",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(playback): 播放进度写入合并",
        "♻️ refactor(db): 派生状态只在需要时刷新",
        "⚡ perf(auth): 鉴权和访问检查缓存",
        "♻️ refactor(auth): 事件推送按用户隔离",
        "⚡ perf(web): 首页缓存",
        "♻️ refactor(image): 读路径和写入路径",
        "♻️ refactor(db): 升级注意（0.7.3 → 0.7.4）",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(media): 队列无限重排撑满磁盘",
        "🐛 fix: 续播位置在阈值处不更新",
        "🐛 fix(playback): 进度回调的时长读取",
        "🐛 fix(db): PostgreSQL 起不来",
      ] },
    ],
  },
  {
    version: "0.7.3",
    date: "2026-10-31",
    sections: [
      { kind: "added", items: [
        "✨ feat(emby): 未初始化时只挂安装向导",
        "✨ feat(probe): 装完自动重启",
        "✨ feat(probe): 被打断的首扫会自己续跑",
        "✨ feat(log): 审计事件记录更多上下文",
        "✨ feat(log): 系统日志能读了",
        "✨ feat(web): 运营和通知后台重新排版",
        "✨ feat(playback): 播放计划给出有序的直放候选（LUX-872）",
        "✨ feat(emby): Emby 播放令牌只认一个条目",
        "✨ feat(api): API 类型绑定试点",
      ] },
      { kind: "changed", items: [
        "♻️ refactor: 启动修复按版本只跑一次",
        "⚡ perf(db): PostgreSQL 占位符改写按语句缓存",
        "⚡ perf(db): MediaFolders 的库根查询合并成一次批量",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(probe): 硬件编码器自检",
        "🐛 fix(probe): 阻塞调用移出 Tokio worker",
        "🐛 fix(web): 播放器的 keydown 监听每次挂载只绑定一次",
        "🐛 fix: 安装后的冒烟脚本会等安装重启后的恢复",
      ] },
    ],
  },
  {
    version: "0.7.2",
    date: "2026-10-30",
    sections: [
      { kind: "added", items: [
        "✨ feat(plugin): 插件商店支持多个目录",
        "✨ feat(emby): Emby 令牌收成播放专用",
        "✨ feat(web): Web 直放会挨个试候选",
        "✨ feat(db): 查询统计自动就绪",
        "✨ feat(playback): 继续观看的文案分场景",
        "✨ feat(probe): 只列出本机可用的硬件编码器",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(deploy): 容器里的访问 IP 认对了",
        "♻️ refactor(auth): 用户列表工具栏更扁",
        "♻️ refactor(web): 浅色主题改用语义状态色",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): 空闲时一直占 CPU",
        "🐛 fix(db): 推荐统计能刷新了",
        "🐛 fix(scan): 读模型从没真正就绪",
        "🐛 fix: 搜索表没变化就不重写",
        "🐛 fix(probe): 探测超时放宽到 130 秒",
        "🐛 fix(web): 后台「正在播放」排版",
      ] },
    ],
  },
  {
    version: "0.7.1",
    date: "2026-10-29",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 后台控件统一成一套",
        "✨ feat(web): 确认操作改用应用内弹窗",
        "✨ feat(auth): 用户详情编辑区默认收起",
        "✨ feat(scan): 扫描能经符号链接找到媒体",
        "🔧 build(playback): 链接转 .strm 脚本",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(scan): 读模型默认切成 on",
        "♻️ refactor(web): 窄屏用户列表改卡片",
        "♻️ refactor(auth): 用户和权限换成顶部标签",
        "♻️ refactor(auth): 用户列表更紧凑",
        "♻️ refactor(auth): 权限摘要挪到详情头部",
        "♻️ refactor(image): 头像和名字都能点进用户",
        "♻️ refactor(danmaku): 删链接只删链接本身",
        "♻️ refactor(db): 升级注意（0.7.0 → 0.7.1）",
        "♻️ refactor(web): 下拉选择补齐 title 和 name 透传",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(web): 确认文案的换行保留了",
        "🐛 fix(web): 进度条样式不再互相污染",
      ] },
    ],
  },
  {
    version: "0.7.0",
    date: "2026-10-29",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 读模型换成派生表",
        "✨ feat(db): 搜索索引改成入队重建",
        "✨ feat(emby): 搜索可以不算总数",
        "✨ feat(api): 响应压缩和 ETag",
        "✨ feat(metadata): 详情 NFO 解码结果缓存",
        "✨ feat(emby): 列表补齐媒体流字段",
        "✨ feat(web): 前台变慢时后台让路",
      ] },
      { kind: "changed", items: [
        "⚡ perf(scan): 扫描空闲退避",
        "♻️ refactor(db): 升级注意（0.6.20 → 0.7.0）",
        "♻️ refactor(db): SQLite 统计维护",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): 坏掉的 NFO 缓存会被重建",
        "🐛 fix(db): 库文件不见了就不启动",
      ] },
    ],
  },
];
