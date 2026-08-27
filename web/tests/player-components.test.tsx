import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { PlayerErrorState, PlayerLoadingState } from "../src/features/player/components/player-state";
import { PlayerVideoSurface } from "../src/features/player/components/player-video-surface";
import { PlayerControls } from "../src/features/player/components/player-controls";
import { PlayerSettingsPanel } from "../src/features/player/components/player-settings-panel";
import { PlayerTopBar } from "../src/features/player/components/player-top-bar";
import { LuxPlayer } from "../src/features/player/components/lux-player";

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

  it("renders top-level media context and source selection as presentation", () => {
    const markup = renderToStaticMarkup(
      <PlayerTopBar
        title="示例电影"
        badge="1080P • H264"
        subtitle="2026"
        sources={[
          { id: "source-1", label: "1080P", detail: "MP4" },
          { id: "source-2", label: "4K", detail: "MKV" },
        ]}
        selectedSourceId="source-1"
        settingsOpen
        fullscreen={false}
        onBack={() => undefined}
        onSourceChange={() => undefined}
        onToggleSettings={() => undefined}
        onToggleFullscreen={() => undefined}
      />,
    );

    expect(markup).toContain("示例电影");
    expect(markup).toContain("1080P • H264");
    expect(markup).toContain('aria-label="选择播放源"');
    expect(markup).toContain('aria-label="播放器设置"');
    expect(markup).toContain('aria-label="全屏"');
  });

  it("keeps settings actions in a focused, keyboard-addressable panel", () => {
    const markup = renderToStaticMarkup(
      <PlayerSettingsPanel
        playbackRates={[0.5, 1, 1.5]}
        playbackRate={1}
        onChangeRate={() => undefined}
        onClose={() => undefined}
      />,
    );

    expect(markup).toContain('role="dialog"');
    expect(markup).toContain('aria-label="播放设置"');
    expect(markup).toContain("标准");
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
        playbackRate={1}
        fullscreen={false}
        pictureInPictureEnabled={false}
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
        onCycleRate={() => undefined}
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
