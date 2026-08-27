import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { PlayerErrorState, PlayerLoadingState } from "../src/features/player/components/player-state";
import { PlayerVideoSurface } from "../src/features/player/components/player-video-surface";
import { playerVideoPresentationStyle } from "../src/features/player/components/player-presentation";
import { PlayerControls } from "../src/features/player/components/player-controls";
import { PlayerSettingsPanel } from "../src/features/player/components/player-settings-panel";
import { PlayerTopBar } from "../src/features/player/components/player-top-bar";
import { LuxPlayer } from "../src/features/player/components/lux-player";
import { playerFailure } from "../src/features/player/components/player-diagnostics";

describe("LuxPlayer state components", () => {
  it("renders a busy loading state with its product message", () => {
    const markup = renderToStaticMarkup(<PlayerLoadingState message="正在准备播放会话…" />);

    expect(markup).toContain('class="lux-player-page lux-player-page-loading"');
    expect(markup).toContain('aria-busy="true"');
    expect(markup).toContain("正在准备播放会话…");
  });

  it("renders an actionable error state without owning playback APIs", () => {
    const onBack = vi.fn();
    const onRetry = vi.fn();
    const markup = renderToStaticMarkup(
      <PlayerErrorState
        title="播放会话创建失败"
        message="服务端暂时不可用"
        onBack={onBack}
        onRetry={onRetry}
      />,
    );

    expect(markup).toContain('role="alert"');
    expect(markup).toContain("播放会话创建失败");
    expect(markup).toContain("服务端暂时不可用");
    expect(markup).toContain('aria-label="重试"');
    expect(markup).toContain('aria-label="返回上一页"');
    expect(onBack).not.toHaveBeenCalled();
    expect(onRetry).not.toHaveBeenCalled();
  });

  it("keeps video events and playback status in the surface boundary", () => {
    const markup = renderToStaticMarkup(
      <PlayerVideoSurface
        streamUrl="/signed/movie-1"
        poster="/images/movie-1"
        title="示例电影"
        videoRef={() => undefined}
        onClick={() => undefined}
        onDoubleClick={() => undefined}
        onError={() => undefined}
        onLoadedMetadata={() => undefined}
        onPlay={() => undefined}
        onPause={() => undefined}
        onTimeUpdate={() => undefined}
        onEnded={() => undefined}
        centerSplash="play"
        fallbackLoading
        fallbackSpeedX={0.5}
        errorMessage="客户端解码失败"
        showError
        onRetry={() => undefined}
        onBack={() => undefined}
      />,
    );

    expect(markup).toContain('class="lux-player-frame"');
    expect(markup).toContain('src="/signed/movie-1"');
    expect(markup).toContain('poster="/images/movie-1"');
    expect(markup).toContain('aria-label="播放 示例电影"');
    expect(markup).toContain("正在准备客户端解码…");
    expect(markup).toContain("客户端解码速度低于实时");
    expect(markup).toContain("客户端解码失败");
  });

  it("applies loop, aspect ratio, and flip to the current video presentation", () => {
    expect(playerVideoPresentationStyle("default", "normal")).toBeUndefined();
    expect(playerVideoPresentationStyle("4:3", "horizontal")).toMatchObject({
      aspectRatio: "4 / 3",
      objectFit: "fill",
      transform: "translate(-50%, -50%) scaleX(-1)",
    });

    const markup = renderToStaticMarkup(
      <PlayerVideoSurface
        streamUrl="/signed/movie-presentation"
        title="画面设置测试"
        videoRef={() => undefined}
        onClick={() => undefined}
        onDoubleClick={() => undefined}
        presentation={{ loop: true, aspectRatio: "16:9", flip: "vertical" }}
        centerSplash={null}
        fallbackLoading={false}
        fallbackSpeedX={null}
        errorMessage={null}
        showError={false}
        onRetry={() => undefined}
        onBack={() => undefined}
      />,
    );

    expect(markup).toContain(" loop=\"\"");
    expect(markup).toContain("aspect-ratio:16 / 9");
    expect(markup).toContain("scaleY(-1)");
  });

  it("renders a classified Lux recovery path instead of an underlying engine error", () => {
    const markup = renderToStaticMarkup(
      <PlayerVideoSurface
        streamUrl="/signed/movie-1"
        title="示例电影"
        videoRef={() => undefined}
        onClick={() => undefined}
        onDoubleClick={() => undefined}
        centerSplash={null}
        fallbackLoading={false}
        fallbackSpeedX={null}
        errorMessage="decoder token https://example.test/private"
        failure={playerFailure("PLAYBACK_EXPIRED")}
        showError
        onRetry={() => undefined}
        onBack={() => undefined}
      />,
    );

    expect(markup).toContain("播放地址已过期");
    expect(markup).toContain("请重试以创建新的 Lux 播放会话");
    expect(markup).not.toContain("https://example.test/private");
  });

  it("renders top-level media context and source selection as presentation", () => {
    const markup = renderToStaticMarkup(
      <PlayerTopBar
        title="示例电影"
        badge="1080P • H264"
        subtitle="2026"
        onBack={() => undefined}
      />,
    );

    expect(markup).toContain("示例电影");
    expect(markup).toContain("1080P • H264");
    expect(markup).toContain('aria-label="返回"');
  });

  it("keeps settings actions in a focused, keyboard-addressable panel", () => {
    const markup = renderToStaticMarkup(
      <PlayerSettingsPanel
        playbackRates={[0.5, 1, 1.5]}
        playbackRate={1}
        onChangeRate={() => undefined}
        presentation={{
          loop: true,
          aspectRatio: "4:3",
          flip: "horizontal",
          onToggleLoop: () => undefined,
          onChangeAspectRatio: () => undefined,
          onChangeFlip: () => undefined,
        }}
        onClose={() => undefined}
      />,
    );

    expect(markup).toContain('role="dialog"');
    expect(markup).toContain('aria-label="播放设置"');
    expect(markup).toContain("标准");
    expect(markup).toContain("循环播放");
    expect(markup).toContain('role="switch"');
    expect(markup).toContain('aria-checked="true"');
    expect(markup).toContain("画面比例");
    expect(markup).toContain("4:3");
    expect(markup).toContain("画面翻转");
    expect(markup).toContain("水平镜像");
    expect(markup).toContain('aria-label="关闭播放设置"');
  });

  it("exposes a labeled keyboard timeline alongside the playback controls", () => {
    const markup = renderToStaticMarkup(
      <PlayerControls
        playing={false}
        currentTime={10}
        duration={100}
        bufferedEnd={25}
        volume={1}
        muted={false}
        fullscreen={false}
        pictureInPictureEnabled
        sources={[{ id: "source-1", label: "1080P", detail: "MP4" }]}
        selectedSourceId="source-1"
        danmuVisible
        settingsOpen={false}
        remainingTime={false}
        hoverTime={null}
        hoverPercent={null}
        progressBarRef={{ current: null }}
        onTimelinePointerDown={() => undefined}
        onTimelinePointerMove={() => undefined}
        onTimelinePointerUp={() => undefined}
        onTimelinePointerCancel={() => undefined}
        onTimelineMouseMove={() => undefined}
        onTimelineMouseLeave={() => undefined}
        onTimelineKeyDown={() => undefined}
        onTogglePlayPause={() => undefined}
        onSeekRelative={() => undefined}
        onToggleMute={() => undefined}
        onVolumeChange={() => undefined}
        onToggleRemainingTime={() => undefined}
        onSourceChange={() => undefined}
        onToggleDanmu={() => undefined}
        onTakeScreenshot={() => undefined}
        onToggleSettings={() => undefined}
        onTogglePictureInPicture={() => undefined}
        onToggleFullscreen={() => undefined}
      />,
    );

    expect(markup).toContain('role="slider"');
    expect(markup).toContain('aria-label="播放进度"');
    expect(markup).toContain('tabindex="0"');
    expect(markup).toContain('aria-valuenow="10"');
    expect(markup).toContain('aria-valuemax="100"');
    expect(markup).toContain('aria-label="播放"');
    expect(markup).toContain('aria-label="音量调节"');
    expect(markup).toContain('aria-label="隐藏弹幕"');
    expect(markup).toContain('aria-pressed="true"');
    expect(markup).toContain('aria-label="选择播放版本"');
    expect(markup).toContain('aria-label="截图"');
    expect(markup).toContain('aria-label="播放器设置"');
    expect(markup).not.toContain("发送弹幕");
    expect(markup).not.toContain("热力图");
  });

  it("provides named future caption and danmu slots at the LuxPlayer boundary", () => {
    const markup = renderToStaticMarkup(
      <LuxPlayer
        controlsVisible={false}
        containerRef={{ current: null }}
        onActivity={() => undefined}
        surface={<div data-slot="surface" />}
        topBar={<div data-slot="top-bar" />}
        settings={<div data-slot="settings" />}
        controls={<div data-slot="controls" />}
        captionSlot={<div data-slot="captions" />}
        danmuSlot={<div data-slot="danmu" />}
      />,
    );

    expect(markup).toContain('class="lux-player-page controls-hidden"');
    expect(markup).toContain('data-slot="surface"');
    expect(markup).toContain('data-slot="captions"');
    expect(markup).toContain('data-slot="danmu"');
  });
});
