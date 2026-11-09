# Lux 插件开发指南（第三方开发者）

本文档面向想为 Lux 编写插件的第三方开发者，说明如何从零做出一个可安装、可运行的插件包，
Lux 向插件提供哪些调用、插件可以调用哪些宿主能力，以及打包、测试和发布时必须遵守的规则。

字段级的完整合同以 [PLUGIN-SDK.md](PLUGIN-SDK.md) 为准；本文是它的开发者导读。两者冲突时，
以 SDK 文档和代码（`src/application/plugin_protocol.rs`、`src/application/plugin_runtime.rs`）为准，
并请提交 issue 或修订文档。

---

## 1. 先决条件与核心模型

Lux 插件是**独立进程**：

- 插件以可执行文件的形式运行，由 Lux 启动并通过 stdin/stdout 与之通信。插件进程不加载进 Lux 主进程。
- 插件**不能**直接访问 Lux 的 SQLite 数据库、媒体根目录、任务对象或内部服务。所有数据都由 Lux 通过
  RPC 请求传入，插件的结果也由 Lux 校验后再写入。
- Lux 是**宿主**，它决定插件类型、能力、持久化、任务调度和界面。插件只实现宿主定义的某一类合同。
- 插件的 stdout 只能输出协议消息，日志一律写 stderr。

因此插件开发的基本工作是：

1. 写一个可执行入口，读取 stdin 的 JSON 请求，写 stdout 的 JSON 响应。
2. 写一份 `manifest.json`，声明插件类型、分类、能力和配置项。
3. 把入口与 manifest 打成 ZIP 包，供 Lux 商店或管理员手动安装。

语言不限。任何能读写标准输入输出、解析 JSON 的程序都可以。下面的示例使用 Python 标准库。

---

## 2. 快速开始：一个最小的 IP 归属地插件

这个例子展示完整的一次调用链，不需要任何第三方依赖。它的 manifest 类型是 `ip_location`，
能力是 `ip.location`。

目录结构（开发时的源码目录）：

```text
org.example.ip-demo/
├── manifest.json
└── bin/
    └── plugin.py
```

`manifest.json`：

```json
{
  "formatVersion": 1,
  "id": "org.example.ip-demo",
  "name": "示例 IP 归属地",
  "description": "演示用插件，只返回固定结果",
  "version": "0.1.0",
  "apiVersion": 1,
  "runtime": {
    "kind": "process",
    "entrypoint": "bin/plugin.py"
  },
  "type": "ip_location",
  "category": "NETWORK",
  "capabilities": ["ip.location"],
  "permissions": {
    "network": [],
    "filesystem": []
  },
  "files": []
}
```

`bin/plugin.py`：

```python
#!/usr/bin/env python3
import json
import sys


def handle(request):
    method = request.get("method")
    params = request.get("params") or {}
    if method == "ip.location":
        ip = params.get("ip")
        # 真实插件应在这里调用上游服务并解析结果。
        return {
            "ip": ip,
            "country": "示例",
            "province": None,
            "city": None,
            "district": None,
            "street": None,
            "isp": None,
            "latitude": None,
            "longitude": None,
        }
    raise ValueError("PLUGIN_INVALID_REQUEST")


def main():
    # 每行一个 JSON 请求；每个响应也只写一行 JSON 到 stdout。
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        request_id = None
        try:
            request = json.loads(line)
            request_id = request.get("id")
            result = handle(request)
            response = {"id": request_id, "result": result}
        except ValueError as error:
            code = str(error) if str(error).startswith("PLUGIN_") else "PLUGIN_INVALID_REQUEST"
            response = {"id": request_id, "error": {"code": code, "message": "请求无效"}}
        except Exception:
            # 诊断信息写 stderr，不能写 stdout。
            print("internal error", file=sys.stderr)
            response = {"id": request_id, "error": {"code": "PLUGIN_INTERNAL_ERROR", "message": "插件内部错误"}}
        sys.stdout.write(json.dumps(response, ensure_ascii=False) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
```

注意入口文件需要可执行权限（`chmod +x bin/plugin.py`），并且 Python 解释器必须在目标机器上可用。
对于正式发布，建议把入口编译为原生二进制，见第 8 节。

本地手动测试（不经过 Lux）：

```bash
echo '{"id":"1","method":"ip.location","params":{"ip":"8.8.8.8"}}' | python3 bin/plugin.py
# 期望输出一行：{"id": "1", "result": {"ip": "8.8.8.8", ...}}
```

---

## 3. 包格式与安装

### 3.1 ZIP 包结构

```text
org.example.ip-demo-0.1.0.zip
├── manifest.json
├── bin/linux-x86_64/plugin        # 按平台的入口，路径由 manifest 的 entrypoint 决定
└── assets/icon.png                # 可选
```

- ZIP 根目录必须直接包含 `manifest.json`，不能再嵌套一层目录。
- `entrypoint` 必须是包内相对路径，不能是绝对路径，不能含 `..`。
- `entrypoint` 中可以使用 `${platform}` 与 `${arch}` 占位符，例如
  `binaries/${platform}-${arch}/lux-plugin-demo`。平台取值为 `linux`，架构取值为 `x86_64` 或 `aarch64`。
- 生产包的入口文件必须存在，且必须与 `files` 中声明的 SHA-256 一致（若声明）。

### 3.2 打包限制

Lux 对插件包有硬性上限，超出即拒绝安装（来源：`src/application/plugin_runtime.rs`）：

| 项目 | 上限 |
|---|---|
| ZIP 文件大小 | 128 MiB |
| 解压后总大小 | 256 MiB |
| 文件数量 | 512 |
| `manifest.json` 大小 | 1 MiB |

请保持入口二进制精简，不要把模型文件、测试数据或缓存打进包内。

### 3.3 商店发布与手动安装

- **商店发布**：Lux 默认商店目录是 `https://github.com/Qoo-330ml/Lux-plugins` 的 `main/index.json`。
  管理员也可以在插件商店中添加最多 10 个其他 HTTPS 目录地址。目录项的 `packages` 按平台列出 ZIP 地址和
  SHA-256；Lux 只下载目录声明的 ZIP，并在安装前校验大小、路径、manifest、平台入口和哈希。
- **手动安装**：开发时可把包内容解压到 `/config/plugins/<插件 ID>/` 目录便于调试；正式分发使用 ZIP 包。Lux 启动时扫描插件目录，具体路径以部署文档为准。
  具体放置方式以管理页面和 `docs/DEPLOYMENT.md` 为准。
- **生效时机**：SDK 文档约定插件目录在 **Lux 启动时扫描**，升级包后需要重启 Lux。商店安装路径另有即时生效的
  说明，请以实际部署为准。

### 3.4 插件 ID 与版本

- `id` 只允许 ASCII 字母、数字、`.`、`-`、`_`，最长 128 个字符。推荐使用 `org.<你的组织>.<名称>` 格式，避免与
  官方插件（`org.lux.*`）冲突。
- `version` 是插件自身版本，建议遵循 SemVer。
- `formatVersion` 是包格式版本，目前只能为 `1`。
- `apiVersion` 是 RPC 合同版本，目前只能为 `1`。
- 三者不能混用：修改插件版本不需要改 `apiVersion`；只有升级 RPC 合同时才改它。

---

## 4. Manifest 完整参考

### 4.1 顶层字段

| 字段 | 必填 | 说明 |
|---|---|---|
| `formatVersion` | 是 | 固定为 `1` |
| `id` | 是 | 插件 ID，见上节 |
| `name` | 是 | 界面显示名称 |
| `description` | 否 | 简短描述 |
| `version` | 是 | 插件版本 |
| `apiVersion` | 是 | 固定为 `1` |
| `runtime` | 是 | `{"kind": "process", "entrypoint": "..."}`，目前只支持 `process` |
| `type` | 是 | 插件类型，见 4.2 |
| `category` | 是 | 分类，必须与类型匹配，见 4.2 |
| `capabilities` | 是 | 能力名数组，必须包含该类型要求的能力 |
| `supportedItemTypes` | 否 | 元数据插件可支持的条目类型，如 `Movie`、`Series` |
| `configFields` | 否 | 管理员可配置的字段，见 4.3 |
| `permissions` | 否 | 网络和文件权限声明，见 4.4 |
| `embyRoutes` | 否 | 仅 `emby.route` 能力使用，见第 6.11 节 |
| `providerKey` / `aliases` | 否 | 仅 metadata 插件，见 4.5 |
| `files` | 否 | 包内文件及 SHA-256 声明 |

### 4.2 类型、分类与必需能力

下表与 `src/application/plugin_protocol.rs` 中的校验一致。选错分类会直接导致清单被拒绝。

| `type` | `category` | 必需能力 | 用途 |
|---|---|---|---|
| `metadata` | `SCRAPER` | `metadata.search` 等 `metadata.*` 能力 | 电影、剧集的元数据刮削，见第 6.1 节 |
| `media_probe` | `MEDIA` | `media.probe` | 对单个媒体地址做 ffprobe 类探测，见第 6.3 节 |
| `ip_location` | `NETWORK` | `ip.location` | IP 归属地查询，见第 6.4 节 |
| `strm_resolver` | `MEDIA` | `strm.resolve` | 把 SMB/FTP 等原始目标解析为可播放的 HTTP(S) 地址 |
| `chapter_detector` | `MEDIA` | `chapters.detect` 或 `chapters.lookup`，且只能声明其中一个 | 片头片尾章节，见第 6.6 节 |
| `notification` | `NOTIFICATION` | `notification.send` | 通知推送，见第 6.8 节 |
| `danmaku` | `MEDIA` | `danmaku.match`；可选 `danmaku.search` 与 `danmaku.fetch` 必须同时声明 | 弹幕匹配，见第 6.7 节 |
| `data_migration` | `MIGRATION` | `migration.emby` | 从 Emby 等系统迁移数据，见第 6.10 节 |
| `login_background` | `UTILITY` | 只能是 `login_background.get` | 登录页背景，见第 6.9 节 |

约束：

- `login_background` 插件必须且只能声明 `login_background.get`，并且必须声明 `permissions.imageHosts`。
- 只有 `login_background` 插件可以声明 `imageHosts`。
- `emby.route` 能力可附加在其他类型上，用于把 Emby 兼容路由转发给插件。
- `ratings.*` 能力只允许 `metadata` 类插件声明，且不能只声明 `ratings.*` 而不声明 `metadata.search`（见 4.5）。
- 未知能力名或未知类型会被拒绝。

### 4.3 `configFields`：管理员可配置的字段

`configFields` 由 Lux 管理页面根据声明**通用渲染**，插件不需要自己做界面。

```json
{
  "configFields": [
    {"key": "apiKey", "label": "API Key", "type": "password", "required": true, "sensitive": true},
    {"key": "concurrency", "label": "并发数", "type": "number", "defaultValue": 2, "minimum": 1, "maximum": 64},
    {"key": "enabledFeature", "label": "启用功能", "type": "toggle", "defaultValue": true},
    {"key": "mode", "label": "模式", "type": "select", "options": [
      {"value": "fast", "label": "快速"},
      {"value": "accurate", "label": "精确"}
    ]}
  ]
}
```

支持的 `type`：

| `type` | 说明 |
|---|---|
| `text` | 文本 |
| `password` | 密码，必须同时 `sensitive: true`，不会回显 |
| `select` | 下拉选择，`options` 为 `{value, label}` 数组；`multiple: true` 可多选 |
| `toggle` | 开关 |
| `number` | 数字，可声明 `minimum`、`maximum`、`defaultValue` |
| `image` | 仅 `org.lux.login-background` 使用，由 Lux 托管上传 |

其他规则：

- `select` 可设置 `optionsSource: "media-libraries"`，由 Lux 根据当前媒体库动态填充选项。插件不应把媒体库 ID 或路径写死在包内。
- 敏感字段（`sensitive: true` 或 `password`）只保存在 Lux 的配置中，**不会**出现在管理 API 的 `configValues` 中，也不会写入日志。
- 通知插件的 `secret` 字段是唯一的凭据字段，规则见 SDK 文档“通知器调用约定”。
- 插件无法声明对其他插件或 Lux 全局设置的读写权限；配置只属于本插件。

### 4.4 `permissions`：权限声明

```json
{
  "permissions": {
    "network": ["api.example.com"],
    "filesystem": [],
    "imageHosts": []
  }
}
```

- `network`：插件预计会访问的主机名列表（精确域名）。**这是能力声明，不替代 Lux 的出站 URL 安全策略**。
  插件仍必须自行做 SSRF 防护：拒绝 localhost、私网地址、保留地址和 IP 字面量（除非协议明确允许）。
- `filesystem`：目前仅用于声明插件需要的本地缓存。插件不能访问 Lux 的媒体目录。
- `imageHosts`：仅 `login_background` 使用，声明浏览器将直接请求的 HTTPS 图片域名。

### 4.5 metadata 插件的 `providerKey` 与 `aliases`

metadata 插件应声明稳定的 `providerKey`，用于在媒体库配置中稳定引用 provider，并可声明 `aliases` 兼容旧配置：

```json
{
  "id": "org.example.metadata",
  "type": "metadata",
  "category": "SCRAPER",
  "providerKey": "example",
  "aliases": ["example-legacy"]
}
```

`providerKey` 是不透明字符串，最长 64 个字符。`id` 只表示安装和运行时身份，不能替代 provider 命名空间。

### 4.6 `ratings.*` 能力的特殊规则

评分查询（`ratings.lookup`、`ratings.batch`、`ratings.search`）是只读能力，只能由 `metadata` 类插件提供，
但**该插件不能被选为媒体库刮削器**，因为它不实现 `metadata.search`。完整合同见 SDK 文档“评分查询调用约定”。
除非你要做评分插件，否则不要声明 `ratings.*`。

---

## 5. 通信协议

### 5.1 帧格式

- 每个请求是 stdin 上的**一行** JSON，以换行 `\n` 结尾。
- 每个响应是 stdout 上的**一行** JSON，以换行 `\n` 结尾。
- stdout 只能输出协议响应。插件内部的日志、调试信息、崩溃栈都必须写 stderr。
- 插件可以乱序返回响应；宿主按 `id` 分发。插件也可以顺序处理，这是最简单可靠的做法。
- 宿主同时最多有 16 个未完成请求（`MAX_PLUGIN_INFLIGHT`），单个响应体上限为 4 MiB（`MAX_PLUGIN_RESPONSE_BYTES`）。

### 5.2 请求信封

```json
{"id": "0196b2c4-...", "method": "metadata.search", "params": { ... }}
```

- `id` 是宿主生成的字符串，插件必须原样回显在响应中。
- `method` 是方法名，见第 6 节。
- `params` 是 JSON 对象。

### 5.3 响应信封

成功：

```json
{"id": "0196b2c4-...", "result": { ... }}
```

失败：

```json
{"id": "0196b2c4-...", "error": {"code": "PLUGIN_INVALID_REQUEST", "message": "简短说明"}}
```

- `result` 与 `error` 二选一。
- `error.code` 必须使用下面的稳定错误码之一，或该类型合同中定义的专用错误码（如 `MEDIA_PROBE_TIMEOUT`、`TRAKT_*` 等）。
- `error.message` 面向诊断，**不能包含完整 URL、凭据、API Key 或 stderr 内容**。

### 5.4 稳定错误码

```text
PLUGIN_INVALID_REQUEST     请求参数不合法
PLUGIN_TIMEOUT             上游超时
PLUGIN_UNAVAILABLE         上游不可用或插件暂不可服务
PLUGIN_RATE_LIMITED        上游限流
PLUGIN_AUTH_FAILED         上游鉴权失败（不得回显凭据）
PLUGIN_PROVIDER_NOT_FOUND  未找到条目，或不支持该媒体类型
PLUGIN_INVALID_RESPONSE    插件无法返回合法结构（宿主侧使用）
PLUGIN_INTERNAL_ERROR      插件内部错误
```

宿主会对插件的所有输出做校验。输出超限、字段不合法、枚举越界都会被当作插件失败。

### 5.5 超时与生命周期

- 每次调用默认超时为 30 秒（`DEFAULT_PLUGIN_CALL_TIMEOUT`）。超时后宿主会终止插件进程并在下次调用时重新启动。
- 插件进程由宿主按需启动，不需要保持常驻；但在两次调用之间保持进程存活可以减少启动开销。
- 宿主停止插件时直接终止进程，**不会发送 `plugin.shutdown` 请求**（见第 9 节）。插件应该可以被安全地杀死，
  不能依赖退出前的清理动作保证数据一致。

---

## 6. 各类型的 RPC 方法

下面按插件类型列出宿主会调用的方法。请求和响应的字段级定义以 SDK 文档对应小节为准，这里给出开发者最需要的要点。

### 6.1 `metadata`：元数据刮削

主要方法：

- `metadata.search`：搜索候选条目，返回 `items`（`RemoteSearchResult` 列表）。
- `metadata.get`：按 `providerId` 获取详情，返回 `metadata`（`MetadataResult`）。
- `metadata.bundle`：一次性返回详情与图片、演员等。
- `metadata.images`：返回 `images`（`RemoteImageInfo` 列表）。
- `metadata.credits`：返回 `cast`。
- `metadata.externalIds`：返回 `ProviderIds`。
- `metadata.trailers`：返回预告片候选。

要点：

- 请求字段是 provider-neutral 的：`itemType`（`Movie`、`Series`、`Season`、`Episode`、`Person`、`BoxSet`）、`name`、`year`、
  `language`、`providerId`。
- `ProviderIds` 的值必须是字符串，键由你定义（例如 `Tmdb`、`Douban`）。Lux 把它当作不透明 ID 保存。
- 图片必须返回完整 HTTPS `Url`，并声明 `Type`、语言和尺寸（如有）。
- 缺少某种媒体类型或能力时，返回 `PLUGIN_PROVIDER_NOT_FOUND`，不要伪造空数据。
- 当 `language` 为空字符串时，表示“不限语言”，插件不得套用管理员的语言偏好。

### 6.2 `ratings`：评分查询

由 metadata 插件附加声明。方法包括 `ratings.lookup`、`ratings.batch` 和 `ratings.search`。宿主只读取 SDK 文档中列出的
字段（如 `traktId`、`writeback.confident`、`writeback.nfo.ratings`），其他字段会被忽略。

### 6.3 `media_probe`：媒体探测

- 方法：`media.probe`。
- 请求包含一个已由宿主校验的 `url`。插件不解析 `.strm` 内容，也不会收到路径或媒体库信息。
- 返回的 `container`、`streams`、`durationTicks`、`bitrate` 等字段只能是 Lux 允许写入 `media_sources` 与 `media_streams` 的那几项。
- 可以在 `includeThumbnail` 为真时返回受限的 `thumbnailJpegBase64`。
- 错误码使用 `MEDIA_PROBE_INVALID_URL`、`MEDIA_PROBE_TIMEOUT`、`MEDIA_PROBE_PROCESS_FAILED`、`MEDIA_PROBE_INVALID_OUTPUT`。
- 错误消息不得包含完整 URL 或 stderr。

### 6.4 `ip_location`：IP 归属地

- 方法：`ip.location`。
- 请求 `{"ip": "8.8.8.8"}`，响应中的 `ip` 必须与请求相同。
- 字段：`country`、`province`、`city`、`district`、`street`、`isp`、`latitude`、`longitude`，缺失的字段填 `null`。
- Lux 会缓存成功结果 24 小时，失败结果 5 分钟；插件不需要自己缓存这些结果。

### 6.5 `strm_resolver`：`.strm` 目标解析

- 方法：`strm.resolve`。
- 请求 `{"target": "smb://..."}`。
- 成功返回 `{"status": "RESOLVED", "url": "https://..."}`；不适用时返回 `{"status": "UNSUPPORTED"}`。
- 返回的 `url` 必须是无凭据、无片段、无控制字符的 HTTP(S) 地址，宿主会再次校验。

### 6.6 `chapter_detector`：章节检测

- 方法：`chapters.detect`（基于音频指纹）或 `chapters.lookup`（基于外部 ID 查询），两者只能声明其一。
- `chapters.detect` 接收同一季至少两个分集的 Chromaprint 指纹序列，返回 `IntroStart`、`IntroEnd` 和 `CreditsStart`，时间必须落在窗口内。
- `chapters.lookup` 接收 TMDb/TVDb/IMDb ID、季号、集号等，只能访问 manifest 声明的固定主机。无数据时返回空标记。
- 插件不能接收文件路径、URL、媒体源 ID 或任务对象。

### 6.7 `danmaku`：弹幕

- 必需方法：`danmaku.match`。接收不含路径和上游地址的文件名，返回 `MATCHED` 或 `NO_MATCH`，匹配时返回受大小限制的 `xmlBase64`。
- 可选方法：`danmaku.search` 和 `danmaku.fetch`，用于管理员手动搜索和按集获取。**两者必须同时声明**，只声明一个的清单无效。
- 字段大小限制见 SDK 文档“弹幕手动匹配合同”。

### 6.8 `notification`：通知推送

- 方法：`notification.send`。
- 宿主提供 `event`（白名单字段）、`target`（目标信息，含 `allowPrivateNetwork`）、`config`（非秘密设置）和 `secret`（凭据）。
- 插件必须自行执行私网与保留地址检查，不得放宽 `allowPrivateNetwork` 策略。
- 凭据只能通过 `secret` 字段声明为唯一的 `password` 类字段，不能进入 `config`。

### 6.9 `login_background`：登录页背景

- 方法：`login_background.get`，参数为空对象 `{}`。
- 返回 `contentKind`（`POSTER_FEED`、`HERO_IMAGE`、`SINGLE_POSTER` 或 `SINGLE_IMAGE`）、`sourceName` 和 `items`。
- 响应 JSON 最大 256 KiB，最多 40 项，每个图片 URL 最多 2048 字节。
- 图片 URL 必须是 HTTPS，主机必须与 `permissions.imageHosts` 精确匹配。
- 插件只提供数据，不提供图片字节，也不负责界面布局。

### 6.10 `data_migration`：Emby 迁移

- 必需能力 `migration.emby`，并需要实现 `migration.test`、`migration.list_users`、`migration.list_items`、
  `migration.user_state`、`migration.authenticate_user` 等方法。
- 宿主负责迁移任务、映射、导入、幂等、恢复和历史事件。插件只负责与 Emby 通信，并返回有界的数据。
- 插件不得返回或持久化 Emby access token、密码或完整认证响应。
- 具体字段和过滤规则见 SDK 文档迁移相关小节。

### 6.11 `emby.route`：Emby 兼容路由（能力附加）

- 在 manifest 中用 `embyRoutes` 声明精确的 HTTP 方法和路径，宿主只把匹配的请求转发给已安装、已启用且声明了该路由的插件。
- 插件收到方法、路径、原始 query、已脱敏的非认证头和有界 Base64 请求体，返回状态码、受限头和有界 Base64 响应体。
- 认证头、Cookie、完整 URL 和 Lux 内部身份**不会**传给插件。

### 6.12 其他方法

- `plugin.hello`、`plugin.health`、`plugin.shutdown`：SDK 文档中有描述，但**当前宿主不会发送它们**（见第 9 节）。
  插件可以实现它们以便将来兼容，但不能依赖它们的调用。

---

## 7. 宿主向插件提供的环境变量与配置文件

启动插件时，宿主会设置以下环境变量（来源：`src/application/plugin_runtime.rs`）：

| 变量 | 说明 |
|---|---|
| `LUX_PLUGIN_ID` | 插件 ID |
| `LUX_PLUGIN_PROTOCOL_VERSION` | 清单中的 `apiVersion` |
| `LUX_PLUGIN_CONFIG_PATH` | 本插件专属的配置文件绝对路径（metadata 与 `login_background` 插件只拿到这个） |
| `LUX_CONFIG_DIR` | 旧兼容：共享配置目录，仅适用于非 metadata、非 `login_background` 插件，**新插件不要依赖** |
| `LUX_PROXY_URL` 与 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY` | 管理员配置了网络代理时注入；被绕过的插件不注入 |

插件的工作目录是插件包的根目录。

### 7.1 配置文件

- `LUX_PLUGIN_CONFIG_PATH` 指向的文件是 JSON 对象，键为 `configFields` 中声明的 `key`。
- 文件可能不存在（尚未保存配置）。插件应使用自己的默认值，不能因此崩溃。
- 敏感字段也会写入该文件，因此插件不能把它们打印到 stderr 或写到其他位置。
- 配置由管理员通过 Lux 管理页面修改。插件不应自行修改该文件。

### 7.2 宿主不提供的东西

- 不提供 Lux 数据库或 HTTP API 的访问地址或令牌。插件不能调用 Lux 的 `/api/v1/...` 接口。
- 不提供媒体库的文件列表、路径或扫描结果。插件只能处理宿主传入的单个对象。
- 不提供用户数据、播放记录或会话信息（除非是 `data_migration` 中明确定义的迁移字段）。

---

## 8. 构建、打包与测试

### 8.1 构建

- 推荐把入口编译为独立的静态或少依赖二进制。Rust、Go 等语言都适合；Python 或 Node 插件需要保证目标机器有对应运行时，
  且最好用 `${platform}-${arch}` 目录分别打包。
- 同时为 `linux-x86_64` 和 `linux-aarch64` 构建。Lux 按当前平台选择资产，不会跨架构运行。

### 8.2 打包

```bash
cd org.example.ip-demo
zip -r ../org.example.ip-demo-0.1.0.zip manifest.json bin/
```

确认 ZIP 根目录直接包含 `manifest.json`，并检查 `unzip -l` 输出是否满足第 3.2 节的上限。

### 8.3 本地测试

- 直接对入口发送 JSON 请求（见第 2 节）。
- 用错误输入测试：非 JSON、缺少 `id`、未知 `method`、超长字段、空字符串，确认返回的是 `error` 而不是崩溃。
- 用超长上游响应测试 4 MiB 上限。
- 检查 stdout 是否只输出 JSON 行，日志是否都在 stderr。

### 8.4 Lux 侧的验证

- 把 ZIP 放入测试 Lux 实例的插件目录，启动后检查插件是否被识别、清单是否被接受。
- 清单被拒绝时，Lux 日志会给出校验错误信息，请对照第 4 节修正。
- 修改配置后，确认受影响的调用会使用新配置。

---

## 9. 已知差异与注意事项

以下内容是开发时最容易踩的坑，也标注了文档与代码不一致的地方：

1. **`plugin.hello`、`plugin.health`、`plugin.shutdown` 当前不会被宿主调用。** SDK 文档中仍描述它们，但运行时只发送业务方法，
   停止插件时直接终止进程。不要依赖这些方法做初始化或优雅退出。
2. **插件目录的生效时机**：SDK 文档约定插件目录只在 Lux 启动时扫描，而商店安装路径另有即时生效的说明。升级插件后，
   请以部署环境的实际行为为准，并在测试时重启 Lux。
3. **官方插件 ID 有特殊行为，但这些行为不适用于第三方插件。** 例如默认的 IP 归属地插件、片头片尾章节源的代理绕过、
   都是 Lux 对官方 ID 写死的逻辑。第三方插件不应依赖它们，也不应使用 `org.lux.*` 作为自己的 ID。
4. **不要把凭据写进 manifest、日志或 `error.message`。** 凭据只能来自配置文件中的敏感字段。
5. **不要信任宿主传入的数据以外的信息。** 例如不要假设 `url` 一定是可访问的，也不要假设 `providerId` 是数字。
6. **不要在 stdout 上打印任何非协议内容**，哪怕是一行调试信息，也会导致整个响应被判为无效。
7. **不要在回复中包含完整 URL 查询参数。** 错误消息和日志都会被限制，完整 URL 不得出现。

---

## 10. 开发检查清单

发布前逐项确认：

- [ ] `manifest.json` 的 `formatVersion` 与 `apiVersion` 均为 `1`。
- [ ] `id` 不使用 `org.lux.*`，只含允许的字符，长度不超过 128。
- [ ] `type`、`category` 与 `capabilities` 的组合符合第 4.2 节的表格。
- [ ] `configFields` 中的敏感字段标记为 `sensitive: true` 或 `password`。
- [ ] `permissions.network` 只列出确实需要的域名，插件自身仍做 SSRF 防护。
- [ ] `entrypoint` 是包内相对路径，不含 `..`，且存在于 ZIP 中。
- [ ] 入口对所有输入都返回 JSON 响应，不因异常退出。
- [ ] stdout 只输出协议行，日志全部在 stderr。
- [ ] 错误码使用稳定错误码之一，错误消息不含凭据或完整 URL。
- [ ] ZIP 大小、文件数量和解压大小在上限之内。
- [ ] 在 `linux-x86_64` 与 `linux-aarch64` 上分别测试通过。

---

## 11. 相关文档

- [PLUGIN-SDK.md](PLUGIN-SDK.md)：字段级合同、错误码、每种类型的完整请求与响应。
- [decisions/010-dynamic-plugin-packages.md](decisions/010-dynamic-plugin-packages.md)：动态插件包的设计决策。
- [decisions/023-plugin-rpc-multiplexing.md](decisions/023-plugin-rpc-multiplexing.md)：RPC 多路复用设计。
- [decisions/011-notification-provider-plugins.md](decisions/011-notification-provider-plugins.md)：通知插件设计。
- [decisions/022-emby-migration-plugin.md](decisions/022-emby-migration-plugin.md)：Emby 迁移插件设计。
