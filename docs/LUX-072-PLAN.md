# LUX-072 `.strm` 直交

## 范围

扫描 `.strm` 文件并把首个非空文本行作为外部播放地址，交给有媒体库权限的客户端。Lux 不验证 URL、不发起 HEAD/GET、不调用 AList API，也不通过自身代理数据；扫描阶段也不读取外部源的容器信息、索引或媒体轨。

## 实现

- [x] 电影、剧集和混合库扫描器识别 `.strm`。
- [x] 读取首个非空行，清理 BOM 和首尾空白；空文件安全保存为无地址 source。
- [x] 保存 `STRM_URL` source kind 和明文 `external_url`，重扫时更新地址。
- [x] PlaybackInfo 返回外部 URL、Http 协议、远程标记和 DirectPlay 能力，不执行网络探测。
- [x] 本地文件流端点不会把 `.strm` 当作本地媒体转发。
- [ ] `.strm` 后台探测不调用 ffprobe；有同名 `-mediainfo.json` 时只读取旁车，没有旁车时保留空技术信息。

## 验证

- 含 BOM、多空行、查询令牌和尾随内容的 fixture 验证只读取首个非空行。
- 空 `.strm` fixture 验证不崩溃且不伪造播放地址。
- ARM64 集成测试验证 PlaybackInfo 直接返回 URL，测试过程无外部网络依赖。

## 明确不做

- 不校验 URL 合法性，不请求 URL，不代理、不下载、不保护 URL 中的令牌。
- 不在首次播放前读取外部媒体容器信息或索引；首次播放由客户端直接访问外部地址。
