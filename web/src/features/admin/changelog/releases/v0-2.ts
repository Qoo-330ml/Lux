import type { ChangelogRelease } from "../types";

export const changelogReleases02: ChangelogRelease[] = [
  {
    version: "0.2.9",
    date: "2026-08-27",
    sections: [
      { kind: "added", items: [
        "Web 本地播放：加了本地媒体的播放会话，Direct 走签名链接，播放计划分 0～4 档，服务端能出 HLS，带基本播放控制。",
        "Redia 媒体源与迁移报告：Redia 的媒体源能读了；元数据补全走一条资源管线；Emby 迁移会告诉用户跳过了哪些内容。",
      ] },
      { kind: "fixed", items: [
        "HLS 会话的起停和清理按预期走了；`.strm` 不再误入转码；Redia 媒体源 ID 和播放跳转修对了；同一份元数据不会反复向上游请求。",
      ] },
      { kind: "changed", items: [
        "播放、HLS、`.strm` 和元数据处理都改用短期签名，资源配额有上限，远程访问的边界也写明确了。",
      ] },
    ],
  },
  {
    version: "0.2.8",
    date: "2026-08-25",
    sections: [
      { kind: "added", items: [
        "弹幕匹配与 STRM 解析：弹幕按片匹配，`.strm` 会被解析出实际指向的目标；任务历史合成一份；加了人物索引，元数据任务能按调度跑；扫描变更集会落盘。",
        "Emby 迁移工作区：迁移有了自己的工作区，会把用户状态、人物收藏和 DisplayPreferences 一起带过来；人物能搜了，多个刮削器的先后顺序也能配。",
      ] },
      { kind: "fixed", items: [
        "`.strm` 的目标分类、扫描时的旁车处理、任务管理、人物索引、插件更新、迁移恢复和多刮削器优先级都修了。",
      ] },
      { kind: "changed", items: [
        "计划任务和一次性任务的记录合成一份；插件更新、迁移密钥和资源配额都收紧了；扫描拆成发现、调和、后处理三段。",
      ] },
    ],
  },
  {
    version: "0.2.7",
    date: "2026-08-20",
    sections: [
      { kind: "added", items: [
        "播放、人物与插件更新：Matroska/HEVC 在浏览器播不动时走客户端回退；控制栏补齐了；人物能查也有详情；待确认的条目能批量处理；插件在管理台里能更新。",
        "扫描活动有记录可查；人物身份统一了，匹配结果要人工审核；媒体库排序不再全局一份，每个用户各存自己的。",
      ] },
      { kind: "fixed", items: [
        "MKV 播放、扫描中断后从 checkpoint 接着跑、人物 API/图片/NFO、元数据状态和插件更新的问题都修了。",
      ] },
      { kind: "changed", items: [
        "媒体能力探针这一阶段的验收收尾了；快照缓存和扫描生命周期调过；人物资源目录、跨 provider 的身份匹配也一起改了。",
      ] },
    ],
  },
  {
    version: "0.2.6",
    date: "2026-08-17",
    sections: [
      { kind: "added", items: [
        "Emby 兼容、通知与能力探针：Emby 虚拟文件夹、共享 API Key、Webhook 进持久化队列、管理台里配通知、媒体能力探针，以及 HEVC 回退。",
        "Emby 人物列表带分页，权限和 API Key 的兼容也补上了。",
      ] },
      { kind: "fixed", items: [
        "Webhook 的重试和安全校验、人物身份回填、混合媒体库的目录，以及 Emby 请求解析的问题都修了。",
      ] },
      { kind: "changed", items: [
        "虚拟文件夹映射和人物索引补齐了；反向代理下的元数据、Provider ID 匹配过了一轮；4K 走 fallback 时会提示；Docker 发布标签也整理了。",
      ] },
    ],
  },
  {
    version: "0.2.5",
    date: "2026-08-16",
    sections: [
      { kind: "added", items: [
        "共享管理员 API Key：管理员可以共用一个服务器级的 API Key；一个媒体库能混不同类型的内容；人物资料接上了 TMDb。",
      ] },
      { kind: "fixed", items: [
        "任务列表顺序、老版本留下的 STRM 播放计划、经过反向代理时的客户端 IP，以及没有音频流的 Emby 条目缺字段，都修了。",
      ] },
      { kind: "changed", items: [
        "API Key 换成受限的文件存储，支持轮换和撤销，也能在一处集中管理。",
      ] },
    ],
  },
  {
    version: "0.2.4",
    date: "2026-08-15",
    sections: [
      { kind: "fixed", items: [
        "布尔值在 PostgreSQL 下的存储兼容问题修好了。",
      ] },
    ],
  },
  {
    version: "0.2.3",
    date: "2026-08-15",
    sections: [
      { kind: "added", items: [
        "管理员可以删用户：管理台里能删掉用户账号。",
      ] },
      { kind: "fixed", items: [
        "后台扫描的进度和取消响应修好了；PostgreSQL 下 migration 校验和对不上的问题也修了。",
      ] },
      { kind: "changed", items: [
        "扫描任务同步与取消流程：任务同步和取消都调顺了。",
      ] },
    ],
  },
  {
    version: "0.2.2",
    date: "2026-08-15",
    sections: [
      { kind: "added", items: [
        "媒体库多根路径：一个媒体库可以配多个根目录；登录状态能恢复，不必重新登录。",
      ] },
      { kind: "fixed", items: [
        "元数据任务不再把文件夹当条目处理；媒体的身份和路径不再漂移；migration 校验和的问题修了。",
      ] },
      { kind: "changed", items: [
        "入库时默认就匹配元数据，增量扫描能自动补上缺的那部分。",
      ] },
    ],
  },
  {
    version: "0.2.1",
    date: "2026-08-14",
    sections: [
      { kind: "added", items: [
        "更新日志在管理台里能直接看；匹配到的元数据要走一遍确认。",
      ] },
      { kind: "fixed", items: [
        "登录 session、扫描并发和 Emby 条目 ID 的兼容问题一起修掉。",
      ] },
      { kind: "changed", items: [
        "首页的加载性能调过了；元数据确认并进统一的管理流程。",
      ] },
    ],
  },
  {
    version: "0.2.0",
    date: "2026-08-14",
    sections: [
      { kind: "added", items: [
        "自动标记已看的阈值不再是全局一个值，每个用户可以自己调；收藏、播放进度和继续观看这条链路也走通了。",
        "本地 NFO 里的信息能把条目元数据补得更全，人物图片这类资源可以直接复用；插件能正常卸载，主题资源可用，首页会按屏幕宽度重排。",
      ] },
      { kind: "fixed", items: [
        "Emby/Filmly 和本地 NFO 里演员身份对不上、媒体流信息缺失、图片回退的问题修了。",
      ] },
      { kind: "changed", items: [
        "已看阈值改成用户设置，SQLite 和 PostgreSQL 两边的播放设置表结构也一起升级了。",
      ] },
    ],
  },
];
