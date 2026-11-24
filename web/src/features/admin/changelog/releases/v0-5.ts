import type { ChangelogRelease } from "../types";

export const changelogReleases05: ChangelogRelease[] = [
  {
    version: "0.5.25",
    date: "2026-10-13",
    sections: [
      { kind: "changed", items: [
        "♻️ refactor(api): 管理员 API 按业务领域拆开",
        "🔧 build(web): Web 静态服务只提供构建产物",
      ] },
    ],
  },
  {
    version: "0.5.24",
    date: "2026-10-13",
    sections: [
      { kind: "changed", items: [
        "♻️ refactor(web): 移动端首页「我的媒体库」卡片随屏幕尺寸收窄",
      ] },
    ],
  },
  {
    version: "0.5.23",
    date: "2026-10-13",
    sections: [
      { kind: "added", items: [
        "✨ feat(metadata): 元数据任务详情能逐条看问题",
        "✨ feat(image): 手动识别媒体可以按刮削器来源看候选、预览海报",
        "✨ feat(web): 移动端首页「我的媒体库」改成单行横向滚动，复用左右滚动按钮",
      ] },
      { kind: "changed", items: [
        "📝 docs(scan): 增量扫描查分集元数据时会连所属季度和剧集容器一起核对，共享父项只处理一次",
        "♻️ refactor(scan): 扫描、元数据回填和存储任务按职责拆成更小的子模块",
        "♻️ refactor(web): 电视剧详情页按页加载单集并显示总集数，可继续加载、失败重试",
        "♻️ refactor(image): 首页媒体操作菜单挪到海报角落",
        "📝 docs(image): 性能基准确认了验证目录和海报来自测试媒体，另加了可选的时序采样",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): 自动元数据搜索没有匹配项时按「无候选」处理",
        "🐛 fix(image): 本地海报读取失败会保留批次重试状态",
      ] },
    ],
  },
  {
    version: "0.5.22",
    date: "2026-10-10",
    sections: [
      { kind: "changed", items: [
        "♻️ refactor(probe): PostgreSQL 增量扫描的媒体源、缩略图和 STRM 探测查询改用目录前缀索引",
        "♻️ refactor(media): 软删除的媒体条目过保留期就清掉",
        "♻️ refactor(web): 手机管理页用满可用宽度并保留安全区留白",
        "♻️ refactor(web): 切换深浅主题时同步更新浏览器主题色（theme-color）",
        "⚡ perf(db): 感谢 @anlostyle 持续贡献数据库相关的修复和优化 PR",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(db): PostgreSQL 合并电影条目报错",
        "🐛 fix(media): 媒体源新增或归属调整后默认源会重复、缺失",
        "🐛 fix(emby): 多段影片不再把 cd1/cd2 列成独立版本",
        "🐛 fix(subtitle): 版本、音轨、字幕选择器统一成紧凑布局",
      ] },
    ],
  },
  {
    version: "0.5.21",
    date: "2026-10-10",
    sections: [
      { kind: "changed", items: [
        "⚡ perf(scan): 新媒体库扫描默认并发 4，SQLite/PostgreSQL 元数据任务默认并发也是 4，自动补缺跨任务最多 4 个 worker",
        "♻️ refactor(web): Web 默认强调色换成随明暗主题适配的银色",
        "♻️ refactor(image): 图片尺寸改成读文件头，不再整张解码",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): 自动补缺和重识别会区分空候选与刮削器不可用",
        "🐛 fix(media): 电影改过标题、年份后重扫会把版本文件拆成独立条目",
        "🐛 fix(metadata): Lux 自己写回 NFO 不再被实时监听当成外部改动重扫",
        "🐛 fix(web): 触控设备输入框字号保持至少 16px",
        "🐛 fix(web): 浅色主题下媒体操作菜单和移动端详情页顶部仍在用深色背景和文字",
      ] },
    ],
  },
  {
    version: "0.5.20",
    date: "2026-10-09",
    sections: [
      { kind: "changed", items: [
        "⚡ perf(image): 本地 NFO 和图片处理按页复用媒体源与图片快照",
        "♻️ refactor(media): 自动补全按媒体库进有界 dispatcher",
        "♻️ refactor(metadata): 本地 NFO 页和 deferred credits 批次加了耗时与批次指标",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(scan): 本地海报权限错误、扫描根暂时不可用、扫描被取消都会保留可重试状态",
        "🐛 fix(metadata): 人物关系索引用内容 checksum 判断快照过期",
        "🐛 fix(media): 指定条目重识别碰到重复 ID 时保留首次出现顺序",
      ] },
    ],
  },
  {
    version: "0.5.19",
    date: "2026-10-06",
    sections: [
      { kind: "changed", items: [
        "♻️ refactor(metadata): 自动元数据补全跨任务最多共享 2 个 worker，会存输入指纹和缺失能力快照",
        "♻️ refactor(scan): 健康页的扫描任务计数用状态部分索引",
        "♻️ refactor(metadata): PostgreSQL 上本地元数据批次的目录前缀查询加了索引",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(scan): 自动扫描不再因为缺预告片或额外外部 ID 就触发补全",
        "🐛 fix(emby): 扫描、STRM 探测、弹幕匹配和 Emby 导入取消后，晚到的完成结果不再覆盖状态",
        "🐛 fix(media): 媒体删除按条目串行",
        "🐛 fix(web): 首页刷新被取消时不再中断已启动的推荐计算",
        "🐛 fix(subtitle): Web 播放缓存区分字幕选择，停止播放后清理会话缓存",
        "🐛 fix(web): Web 请求优先读已更新的共享 CSRF 令牌",
      ] },
    ],
  },
  {
    version: "0.5.18",
    date: "2026-10-05",
    sections: [
      { kind: "changed", items: [
        "♻️ refactor(log): 共享管理员 API Key 改成独立服务器主体，响应和审计不再借用管理员账号",
        "⚡ perf(metadata): 完整度检查复用 NFO 阶段的媒体源快照",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(emby): Emby 的媒体库、搜索和会话管理保留共享 API Key 的服务器权限",
      ] },
    ],
  },
  {
    version: "0.5.17",
    date: "2026-10-04",
    sections: [
      { kind: "changed", items: [
        "♻️ refactor(emby): MediaTidy/StrmAssistant 导入时",
        "⚡ perf(scan): 自动补全只在新确认缺失项后排队，通用入口复用同库活动任务并合并排队",
        "♻️ refactor(metadata): 人物资源和关系文件改成按键串行写入，冲突和重复重试少了",
        "⚡ perf(metadata): 媒体库刮削器配置改批量写入",
        "♻️ refactor(metadata): 本地 NFO 和剧集元数据处理跳过没变化的写入和空默认值修复",
        "🔧 build: 项目质量检查工作流改成手动触发",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(probe): 字符串形式的码率、采样率和分数帧率不再被误拒",
        "🐛 fix(web): 首页轮播不再出现分集",
        "🐛 fix(subtitle): 修了 Linux 字幕测试夹具的可执行权限",
        "🐛 fix(db): 已发布迁移的版本号和校验和保持不变",
        "🐛 fix(api): 共享管理员 API Key 优先关联实际服务器管理员",
        "🐛 fix(web): 移动端任务活动弹窗和管理页面布局溢出",
      ] },
    ],
  },
  {
    version: "0.5.16",
    date: "2026-10-04",
    sections: [
      { kind: "added", items: [
        "✨ feat(api): 兼容路由可导入受限的 mediaInfoImport Bundle",
      ] },
      { kind: "changed", items: [
        "⚡ perf(web): Web 播放的容器状态同步改成批量读写剧集/季度父条目",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(auth): 导入 Bundle 加强了目标匹配、字段范围、路径和认证参数校验",
      ] },
    ],
  },
  {
    version: "0.5.15",
    date: "2026-10-04",
    sections: [
      { kind: "added", items: [
        "✨ feat(metadata): 本地元数据回填加了不可重试项记录和任务活动索引",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(metadata): NFO 写入靠内容指纹和稳定性保护判断",
        "⚡ perf(realtime): 管理台活动刷新改用轻量查询并合并失效通知",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): NFO 身份冲突不再无限重试",
      ] },
    ],
  },
  {
    version: "0.5.14",
    date: "2026-10-03",
    sections: [
      { kind: "added", items: [
        "✨ feat(auth): 账户设置里验证当前密码之后",
        "✨ feat(playback): STRM 兼容 MediaTidy/StrmAssistant",
      ] },
      { kind: "changed", items: [
        "⚡ perf(danmaku): 扫描、元数据、插件、计划任务、图片、弹幕、媒体合并、删源和 Web 播放会话清理都改成批量",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(emby): Emby 兼容请求加了认证参数过滤和媒体源归属校验",
      ] },
    ],
  },
  {
    version: "0.5.13",
    date: "2026-10-02",
    sections: [
      { kind: "added", items: [
        "✨ feat(playback): 播放活动和通知补上剧集的系列、季、集层级",
        "📝 docs(docs): 补了项目的非商业源码许可证说明",
      ] },
      { kind: "changed", items: [
        "⚡ perf(probe): 扫描、元数据、人物关系、章节探测、计划管理和通知队列的写入改成批量",
        "♻️ refactor(metadata): 媒体详情页的更多信息和 NFO 信息换成更紧凑的响应式布局",
      ] },
    ],
  },
  {
    version: "0.5.12",
    date: "2026-10-01",
    sections: [
      { kind: "changed", items: [
        "♻️ refactor(emby): Emby 目录查询支持 SortName 排序",
      ] },
    ],
  },
  {
    version: "0.5.11",
    date: "2026-10-01",
    sections: [
      { kind: "changed", items: [
        "♻️ refactor(auth): 普通用户的媒体库访问范围说清楚了",
      ] },
    ],
  },
  {
    version: "0.5.10",
    date: "2026-10-01",
    sections: [
      { kind: "changed", items: [
        "♻️ refactor(plugin): 管理台插件页改成响应式",
        "♻️ refactor: 补全需要详情时会给前两项候选都补上完整详情",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(plugin): 插件在 BelongsToCollection.Id 返回整数或字符串都认",
      ] },
    ],
  },
  {
    version: "0.5.9",
    date: "2026-10-01",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 首页改成分区独立加载",
        "✨ feat(plugin): 插件配置可统一登录背景来源并上传单张自定义图片",
        "✨ feat(probe): 电影 NFO 写入完整 TMDb 信息和本地探测的轨道信息，加排序标题与加入时间",
      ] },
      { kind: "changed", items: [
        "⚡ perf(metadata): 刮削少做无用功",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): 旧候选缓存不再被当成含完整 TMDb 详情，需要时重新拉取",
      ] },
    ],
  },
  {
    version: "0.5.8",
    date: "2026-09-30",
    sections: [
      { kind: "changed", items: [
        "♻️ refactor(emby): Emby 媒体库支持按 ProductionYear 升序或降序排序",
        "♻️ refactor(subtitle): 详情页音轨和字幕选择器布局更紧凑",
      ] },
    ],
  },
  {
    version: "0.5.7",
    date: "2026-09-30",
    sections: [
      { kind: "changed", items: [
        "♻️ refactor(image): 元数据候选、能力尝试和图片状态等写入改成有界批处理",
        "♻️ refactor(db): PostgreSQL 首页每库最新资源查询改成预聚合剧集最近添加时间，单次 SQL 从 3.556 秒降到 0.566 秒",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(plugin): 插件 JSON 响应读取时限制为 4 MiB",
        "🐛 fix(scan): 首页请求失败后被扫描变更事件反复触发重试、长期停在骨架屏，现在修了",
      ] },
    ],
  },
  {
    version: "0.5.6",
    date: "2026-09-30",
    sections: [
      { kind: "added", items: [
        "✨ feat(scan): 扫描能认出电影多版本",
        "✨ feat(db): 新增管理员临时只读的数据库体检 API",
        "✨ feat(image): 账户设置里能调自定义头像的裁剪、缩放和位置",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(scan): 结构化日志、扫描事件、管理审计和近期登录/播放活动改存配置目录里的 JSONL",
        "♻️ refactor(emby): Emby 兼容接口新增按用户 ID 验证密码的登录入口",
        "♻️ refactor(image): 调了元数据补全队列的索引、查询和图片任务调度",
        "♻️ refactor(db): 继续观看查询少了数据库往返",
        "♻️ refactor(subtitle): 详情页音轨和字幕选择器按可用宽度并排或堆叠显示",
      ] },
    ],
  },
  {
    version: "0.5.5",
    date: "2026-09-29",
    sections: [
      { kind: "added", items: [
        "✨ feat(scan): 全量扫描加了渐进式本地元数据处理",
        "✨ feat(web): 媒体详情页能看到条目加入媒体库的时间",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(web): 删媒体库改用自家的确认弹窗",
        "♻️ refactor(emby): Emby 目录查询开始遵循 SortBy 和 SortOrder",
        "♻️ refactor(emby): Emby AuthenticateByName 支持用唯一匹配的公开显示名登录",
        "♻️ refactor(image): 登录背景支持 Bing 每日图片和 TMDb 全幅横幅",
        "♻️ refactor(scan): PostgreSQL 扫描写事务改用局部异步提交，少等提交",
        "🔧 build(deploy): Docker Hub 的 main 分支发布完成后可发 Telegram 通知",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(scan): 删媒体库时会阻止新扫描进入、取消相关扫描、等活动 worker 退出再删",
        "🐛 fix(db): SQLite 和 PostgreSQL 里媒体库删除涉及的外键查找补了索引",
        "🐛 fix(scan): 管理台扫描任务失败时能读到并展示任务错误事件里的错误码",
      ] },
    ],
  },
  {
    version: "0.5.4",
    date: "2026-09-28",
    sections: [
      { kind: "added", items: [
        "✨ feat(scan): 新增「其他视频」媒体库，支持目录层级浏览、扫描、详情和播放",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(emby): 首页和 Emby Resume 的剧集续看按剧集收敛，只显示该剧集最近的未完成集",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(image): 登录页动态背景加载时会短暂闪出默认海报墙",
      ] },
    ],
  },
  {
    version: "0.5.3",
    date: "2026-09-27",
    sections: [
      { kind: "changed", items: [
        "⚡ perf(scan): 全量扫描新增轻量 Manifest 发现模式",
        "⚡ perf(db): PostgreSQL 的媒体搜索、可用性和 Provider 索引刷新改成批量",
        "⚡ perf(scan): Manifest 扫描继续调文件名解析复用和分批处理",
        "♻️ refactor(metadata): 配了在线刮削器的媒体库在 SCRAPER_FIRST 模式下",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(plugin): 管理台之前不显示登录背景插件的下拉配置",
      ] },
    ],
  },
  {
    version: "0.5.2",
    date: "2026-09-26",
    sections: [
      { kind: "added", items: [
        "✨ feat(plugin): 登录页背景插件支持单张海报或原比例图片，可展示作品来源和许可署名链接",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(web): 首页「继续观看」对同一剧集只显示季号、集号靠后的未完成进度",
        "♻️ refactor(scan): 全量扫描改用可恢复的 Manifest 检查点和分批索引",
        "♻️ refactor(emby): Emby 剧集媒体源名称显示文件名而不是媒体库路径",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): 本地人物资料的标识不再因为编辑或重命名而变",
      ] },
    ],
  },
  {
    version: "0.5.1",
    date: "2026-09-23",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(playback): 继续观看里 last_played_at 为空的旧数据会排在新进度前面",
      ] },
    ],
  },
  {
    version: "0.5.0",
    date: "2026-09-23",
    sections: [
      { kind: "changed", items: [
        "♻️ refactor(emby): Emby 音轨和字幕轨道详情会拼上可用的语言、编码、声道布局和原始标题",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(playback): 不同播放会话的进度会互相覆盖",
      ] },
    ],
  },
];
