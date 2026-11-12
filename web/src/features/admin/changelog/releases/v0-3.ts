import type { ChangelogRelease } from "../types";

export const changelogReleases03: ChangelogRelease[] = [
  {
    version: "0.3.12",
    date: "2026-09-09",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 媒体库卡片带数量 — 卡片上直接列出电影、剧集、分集、媒体源和文件夹各有多少。",
        "✨ feat: 局域网设备配对 — 新增 Lux Prism，能发现局域网里的设备并做一次性配对，受控设备连上服务快一些。",
        "✨ feat(scan): 旁车文件边扫边登记 — 全量和增量扫描会流式登记本地图片、NFO 这类旁车目标，文件索引一完成就能用上本地元数据和图片，不用等整轮扫完。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(web): 首页不再吐旧快照 — 缓存失效后的第一次请求会先刷新共享首页数据再生成用户快照，不会把旧的共享快照发出去。",
        "🐛 fix(scan): 旁车登记跟上扫描进度 — 扫描发现阶段结束前登记滞后的问题修好了，已索引的媒体能及时开始处理本地元数据。",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(web): 服务器概况重排 — 关键媒体统计改成 Bento 卡片，运行时长并到顶部 Hero 区域，重复的视觉层级少了。",
        "♻️ refactor(auth): 配对边界收紧了 — 设备配对限制了次数、有效期和发现端口，发现服务也加了边界处理，误配对少一些。",
      ] },
    ],
  },
  {
    version: "0.3.11",
    date: "2026-09-09",
    sections: [
      { kind: "added", items: [
        "✨ feat(metadata): TMDb 加了「原语言」模式 — 默认关闭，打开后电影和剧集优先用原语言的标题、文字和图片，缺对应翻译就回退到首选语言。",
        "✨ feat(playback): 继续观看能看出是哪部剧 — 分集卡片显示剧集名称，同名剧或只有集标题的记录好区分了。",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(image): 原语言图片按语言挑 — 请求图片时带上媒体的原语言提示，本地按原语言、无语言、英语的顺序筛详情图片和独立图片候选。",
        "♻️ refactor(media): 分集带上剧集名 — Lux 媒体目录接口给分集补了 seriesName 字段，Web 和其他客户端能显示所属剧集。",
      ] },
    ],
  },
  {
    version: "0.3.10",
    date: "2026-09-08",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 后台任务能看到在跑什么 — 任务活动显示当前处理的媒体，剧集任务还带剧集名、季度/集数、集标题，跑很久的任务好定位。",
        "✨ feat(metadata): TMDb 语言按语言组展示 — 地区变体合并到语言组里，旧的地区语言配置会被自动兼容并归一化。",
        "✨ feat(plugin): 自定义执行计划能删了 — 删掉后计划里的媒体库回到匹配的默认计划，默认计划和全局插件计划删不掉。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(build): 计划配置不再重复建计划 — 镜像更新时未变化的共享计划直接复用原记录，自定义计划的归属也保留。",
        "🐛 fix(scan): 任务列表标签准了 — 没有媒体库的默认计划、非扫描任务的阶段标签不再显示错。",
        "🐛 fix(web): 仪表盘去掉重复面板 — 重复的运行状态和管理入口面板清理掉了。",
        "🐛 fix(media): 剧集在「最新」里会挪位置 — 新增分集后按可见可用分集的最新入库时间排序，不再用剧集旧的入库时间。",
      ] },
      { kind: "changed", items: [
        "⚡ perf(scan): 扫描索引瘦身 — SQLite/PostgreSQL 的扫描索引做了压缩，待处理阶段的索引范围收窄、冗余索引去掉，写放大降低；新增迁移会保留已有扫描数据和外键约束。",
        "♻️ refactor(api): 任务活动接口统一带当前媒体 — 元数据、片头片尾和弹幕匹配任务都给出当前媒体标签，后台任务查询仍然有界。",
      ] },
    ],
  },
  {
    version: "0.3.9",
    date: "2026-09-08",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 执行计划按任务类型管 — 支持多个 Cron、绑定多个媒体库，管理台能编辑也能立刻执行。",
        "✨ feat(subtitle): MKV 播放和字幕补齐 — Matroska/MKV 的客户端 fallback、字幕轨道发现和手动选字幕都补上了，远程字幕由浏览器直连 externalUrl。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(subtitle): 字幕竞态修好了 — Matroska 字幕在 seek、切源和播放器销毁时的竞态不再出现，显式字幕失败会恢复原生播放。",
        "🐛 fix(subtitle): 解析加了边界和上限 — Matroska、字幕和 Range 解析都加了边界校验与资源上限，无效或超大输入不会拖垮播放。",
        "🐛 fix(plugin): 插件同步不再覆盖自定义计划 — 插件同步以前会盖掉媒体库的自定义执行计划，现在不会。",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(api): 任务的展示层级改了 — 按「任务类型—执行计划—媒体库运行任务」三级展示，旧计划 API 仍然兼容。",
        "♻️ refactor(scan): 实时扫描 — 增量扫描沿用锁和资源边界；远程 .strm 走 externalUrl 直放或客户端 fallback，不经 Lux Relay 和服务端 HLS。",
      ] },
    ],
  },
  {
    version: "0.3.8",
    date: "2026-09-04",
    sections: [
      { kind: "added", items: [
        "✨ feat(emby): Emby 兼容删除接口 — 新增媒体删除接口，管理员能删媒体源，也能看到 CanDelete 权限状态。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): 平铺电影目录的 NFO — NFO 冲突、首映日期和不可用媒体记录的处理都修了。",
        "🐛 fix(emby): Hills 等客户端的兼容 — 播放回调、媒体时长、复合容器路径、已看/收藏响应一起对齐了。",
        "🐛 fix(playback): .strm 播放鉴权 — URL/路径型 .strm 的代理播放鉴权和媒体源兼容修好了，SMB/FTP 继续走协议解析器。",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(playback): .strm 改用短期票据 — URL/路径型 .strm 保留原始目标，通过带短期票据的标准播放入口播放；本地文件和 SMB/FTP 不写长期 token。",
      ] },
    ],
  },
  {
    version: "0.3.7",
    date: "2026-09-02",
    sections: [
      { kind: "added", items: [
        "✨ feat(db): 管理员用户分页查询 — 用户列表支持分页；媒体库的实时文件监听也加了开关。",
        "✨ feat(emby): Emby 条目 ID 改数字 — 兼容层改用稳定的纯数字条目 ID，历史 UUID 请求照样能识别。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(scan): 扫描不再留过期记录 — 扫描生命周期里的过期记录会清掉，人物关系也少重复写入。",
        "🐛 fix(scan): 扫描与 .strm 修复 — 增量扫描误触发 STRM 探测、PostgreSQL 的 NFO 评分，以及 URL/路径型 .strm 的代理播放都修了。",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(web): 重启不自动接续任务 — 服务启动或关闭时取消没完成的后台任务、不再自动恢复，任务留着可以诊断也能手动重试。",
        "♻️ refactor(playback): .strm 入口统一了 — URL/路径型 .strm 的原始映射和标准代理入口合成一套，协议解析器的目标继续走受保护入口。",
      ] },
    ],
  },
  {
    version: "0.3.6",
    date: "2026-09-01",
    sections: [
      { kind: "added", items: [
        "✨ feat(emby): Emby 用户管理补齐 — 用户资料、策略、密码、头像、创建/删除和策略复制都兼容了。",
        "✨ feat(emby): 同季度上下集 — 播放器能在同季度里切上一集和下一集，Emby 会话还支持活动窗口和未看分集统计。",
        "✨ feat(auth): 用户配置会存下来 — 用户配置和活动信息持久化，NextEmby 的 Base64 头像上传也兼容。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(auth): 一批用户相关的问题 — 查不存在的用户、删掉头像还残留、SQLite 锁重试、PostgreSQL 密码状态兼容都修了。",
        "🐛 fix(metadata): 人物索引与条目拆分 — 人物索引恢复时的重复写入修好了，同标题不同来源的版本不再被拆成多个条目。",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(api): 服务器概况和 DTO 收敛 — 管理台服务器概况的响应式布局、活动扫描浮层调整过，用户 DTO 和播放会话的活动窗口也统一了。",
      ] },
    ],
  },
  {
    version: "0.3.5",
    date: "2026-08-31",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(api): 隐私浏览也能写请求 — 缺 Cookie/Storage 的隐私浏览模式下，CSRF 写请求不会再被拦。",
        "🐛 fix(emby): 弹幕与 Emby 字段兼容 — Bilibili 弹幕 XML 的声明和条目上限校验放宽了，Emby 播放会话字段的兼容也修了。",
        "🐛 fix(scan): 文件短暂缺失不再被删 — 扫描中文件短暂消失不会被当成移除，PostgreSQL migration 的版本冲突也修了。",
      ] },
      { kind: "changed", items: [
        "⚡ perf(danmaku): 弹幕匹配和扫库并发可调 — 弹幕匹配多了覆盖与并发配置，扫库并发用 LUX_SCAN_CONCURRENCY 调。",
        "♻️ refactor(scan): 扫描不容易被卡住 — 本地 NFO/图片旁车改成流式处理，首页查询和推荐统计清理也顺过，扫描阻塞少了。",
      ] },
    ],
  },
  {
    version: "0.3.4",
    date: "2026-08-31",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(playback): 远程 .strm 播放 — 远程 HTTP(S) .strm 误进客户端 fallback、CORS 配置不兼容、错误直连上游都修了。",
        "🐛 fix(metadata): NFO 评分和用户接口 — 本地 NFO 评分、剧集 tvshow.nfo 写回、推荐分页、Emby 用户/会话响应都修了。",
        "🐛 fix(api): HTML 响应和 CSRF — 未知 API 不再返回 HTML；反向代理或隐私模式下 CSRF Cookie 缺失导致 Web 写请求失败也修了。",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(metadata): 章节标记并进共享时间轴 — 章节标记并入共享时间轴，详情页操作和移动端布局跟着调了。",
        "⚡ perf(metadata): 批量预读与稳定推荐 — 目录、待确认元数据和人物数据改成批量预读，首页推荐按稳定批次生成。",
      ] },
    ],
  },
  {
    version: "0.3.3",
    date: "2026-08-30",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 移动端详情页布局 — 媒体详情在移动端重排过，剧集详情多了一张竖版海报。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(web): 移动端详情页与几处播放问题 — 移动端详情展示、元数据匹配并列、迁移任务恢复、实时扫描冲突、播放器时间轴都修好了。",
        "🐛 fix(playback): 远程 .strm 与容器识别 — 远程 .strm 的重定向能正常跟下去，Matroska 容器能正确识别，Docker runtime 的依赖漂移也修了。",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(image): 移动端海报和操作区 — 详情页的海报尺寸、层级关系和底部操作区布局都收紧了。",
      ] },
    ],
  },
  {
    version: "0.3.2",
    date: "2026-08-30",
    sections: [
      { kind: "added", items: [
        "✨ feat(emby): Emby 迁移能圈范围 — 迁移时可以只挑要搬的部分，还能按来源用户筛选，候选匹配会列出来，报告中断后能接着看。",
        "✨ feat(web): 首页和播放入口的调整 — 首页模块可见性能自己调，侧栏加了账户入口，移动端多了个直接开播的快捷入口；内嵌字幕轨道能用，.strm 也能匹配弹幕。",
        "✨ feat(scan): 扫描后处理有进度了 — 后处理阶段会显示进度和吞吐统计，任务状态也拆得更细。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(scan): 全量扫描期间的几处问题 — 实时任务不再一直排队，迁移回退、来源限流、首页加载和移动端播放器布局也一起修了。",
        "🐛 fix(subtitle): 一批兼容问题 — 字幕、截图、Provider ID、checkpoint 恢复和媒体库排序这几处都修了。",
      ] },
      { kind: "changed", items: [
        "⚡ perf(db): 迁移批处理更快 — 迁移的批处理、索引和状态合并都调过；管理台页面改成预加载，大库扫描时数据库往返也少了。",
        "♻️ refactor(db): 移动端控制与迁移向导 — 播放控制和迁移向导的布局收紧了；升级时数据库 schema 更新到 103。",
      ] },
    ],
  },
  {
    version: "0.3.1",
    date: "2026-08-28",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): LuxPlayer 播放器成型 — 播放器拆成状态、引擎、控制层和响应式界面，手势、字幕、弹幕、章节、循环、画面比例、镜像和 AirPlay 都能用。",
        "✨ feat(db): 剧集级删除和连接池 — 媒体删除可以按剧集来做，数据库连接池加了诊断信息，连接数也能配。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(playback): 一批播放与鉴权问题 — 并发切会话不再串到一起，字幕/弹幕的生命周期、移动端控件、媒体库删除和客户端鉴权兼容都修了。",
        "🐛 fix(playback): 混合媒体库与播放票据 — 混合媒体库、短期播放票据、媒体库封面缓存、扫描后处理和实时监听器的问题一起修了。",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(web): 播放页换成新播放器 — Web 播放页改用模块化的 LuxPlayer，播放协议、鉴权和会话接口保持兼容，现有客户端不用改。",
        "♻️ refactor(danmaku): 章节、字幕、弹幕跟着媒体源走 — 这三样都绑当前播放的媒体源，换源不会串；另外补了多客户端真机验证。",
      ] },
    ],
  },
  {
    version: "0.3.0",
    date: "2026-08-27",
    sections: [
      { kind: "added", items: [
        "✨ feat(metadata): 元数据能接别的供应商 — 加了与供应商无关的元数据插件边界，插件可以有自己的配置，支持批量查询，元数据资源怎么写回也定了规则。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): 迁移与刮削的一批问题 — Emby 迁移的重复遍历和权限遗漏、配置迁移、缓存防抖、多刮削器结果合并和 Web 图片编辑器都修了。",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(plugin): TMDb 和豆瓣搬进插件 — 两个的网络客户端和 DTO 移到独立插件里，元数据资源命名、图片写回和刮削器的执行顺序也跟着统一了。",
      ] },
    ],
  },
];
