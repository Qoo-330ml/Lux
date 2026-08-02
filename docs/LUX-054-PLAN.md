# LUX-054：原子 NFO 写回实施计划

## 范围

在媒体库根路径约束内更新 common NFO 字段，保留未知 XML 事件；写入同目录唯一临时文件、刷盘并原子替换，失败或并发修改时不替换原文件；成功后刷新 metadata fingerprint。

## 实现

- [x] 增加流式 XML 重写，保留未知元素、属性、注释和 XML 声明。
- [x] 支持新建 movie NFO 和 title/originaltitle/year/plot common 字段。
- [x] 增加同目录唯一临时文件、`sync_all`、原子 rename 和临时文件清理。
- [x] 增加媒体根路径 canonicalize/越界/符号链接检查和数据库 fingerprint 更新。
- [x] 故障测试覆盖未知字段保留、损坏 NFO、只读目录、并发修改和临时文件清理。

## 明确不做

- 不在本阶段开放管理员写回 API；候选选择和 API 纵切片由 LUX-056 接入。
- 不下载图片；图片验证和写回属于 LUX-055。
