# ADR-001：模块化单体

- 状态：已接受（规格建议）
- 日期：2026-08-02

## 决定

首版 Lux 使用一个 Rust 进程、一个 SQLite 数据库、一个 Web 静态资源入口和多个受控后台 worker。通过模块边界隔离 `api`、`application`、`domain`、`storage`、`library`、`metadata`、`media`、`playback` 和 `jobs`，暂不拆分微服务。

## 原因

目标部署是单实例 NAS。模块化单体能保留清晰的依赖和事务边界，同时降低容器、网络、升级和故障排查成本。

## 后果

模块边界必须由代码结构和测试守住；只有出现明确的编译、发布或复用需求时才拆分 crate 或服务，并新增 ADR。
