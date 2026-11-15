import type { ChangelogRelease } from "../types";

export const changelogReleases02: ChangelogRelease[] = [
  {
    version: "0.2.9",
    date: "2026-08-27",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): Web 本地播放",
        "✨ feat(db): Redia 媒体源与迁移报告",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(playback): HLS 会话的起停和清理按预期走了",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(metadata): 播放、HLS、`.strm` 和元数据处理都改用短期签名",
      ] },
    ],
  },
  {
    version: "0.2.8",
    date: "2026-08-25",
    sections: [
      { kind: "added", items: [
        "✨ feat(danmaku): 弹幕匹配与 STRM 解析",
        "✨ feat(emby): Emby 迁移工作区",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(scan): `.strm` 的目标分类、扫描时的旁车处理、任务管理、人物索引、插件更新、迁移恢复和多刮削器优先级都修了",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(scan): 计划任务和一次性任务的记录合成一份",
      ] },
    ],
  },
  {
    version: "0.2.7",
    date: "2026-08-20",
    sections: [
      { kind: "added", items: [
        "✨ feat(plugin): 播放、人物与插件更新",
        "✨ feat(scan): 扫描活动有记录可查",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(scan): MKV 播放、扫描中断后从 checkpoint 接着跑、人物 API/图片/NFO、元数据状态和插件更新的问题都修了",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(scan): 媒体能力探针这一阶段的验收收尾了",
      ] },
    ],
  },
  {
    version: "0.2.6",
    date: "2026-08-17",
    sections: [
      { kind: "added", items: [
        "✨ feat(emby): Emby 兼容、通知与能力探针",
        "✨ feat(emby): Emby 人物列表带分页",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): Webhook 的重试和安全校验、人物身份回填、混合媒体库的目录",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(metadata): 虚拟文件夹映射和人物索引补齐了",
      ] },
    ],
  },
  {
    version: "0.2.5",
    date: "2026-08-16",
    sections: [
      { kind: "added", items: [
        "✨ feat(api): 共享管理员 API Key",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(playback): 任务列表顺序、老版本留下的 STRM 播放计划、经过反向代理时的客户端 IP",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(api): API Key 换成受限的文件存储",
      ] },
    ],
  },
  {
    version: "0.2.4",
    date: "2026-08-15",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(db): 布尔值在 PostgreSQL 下的存储兼容问题修好了",
      ] },
    ],
  },
  {
    version: "0.2.3",
    date: "2026-08-15",
    sections: [
      { kind: "added", items: [
        "✨ feat(auth): 管理员可以删用户",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(scan): 后台扫描的进度和取消响应修好了",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(scan): 扫描任务同步与取消流程",
      ] },
    ],
  },
  {
    version: "0.2.2",
    date: "2026-08-15",
    sections: [
      { kind: "added", items: [
        "✨ feat(media): 媒体库多根路径",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): 元数据任务不再把文件夹当条目处理",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(scan): 入库时默认就匹配元数据",
      ] },
    ],
  },
  {
    version: "0.2.1",
    date: "2026-08-14",
    sections: [
      { kind: "added", items: [
        "✨ feat(log): 更新日志在管理台里能直接看",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(emby): 登录 session、扫描并发和 Emby 条目 ID 的兼容问题一起修掉",
      ] },
      { kind: "changed", items: [
        "⚡ perf(web): 首页的加载性能调过了",
      ] },
    ],
  },
  {
    version: "0.2.0",
    date: "2026-08-14",
    sections: [
      { kind: "added", items: [
        "✨ feat(auth): 自动标记已看的阈值不再是全局一个值，每个用户可以自己调",
        "✨ feat(image): 本地 NFO 里的信息能把条目元数据补得更全，人物图片这类资源可以直接复用",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(emby): Emby/Filmly 和本地 NFO 里演员身份对不上、媒体流信息缺失、图片回退的问题修了",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(auth): 已看阈值改成用户设置",
      ] },
    ],
  },
];
