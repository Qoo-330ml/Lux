import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { PlayerErrorState, PlayerLoadingState } from "../src/features/player/components/player-state";
import { PlayerVideoSurface } from "../src/features/player/components/player-video-surface";
import {
  playerVideoPresentationSize,
  playerVideoPresentationStyle,
} from "../src/features/player/components/player-presentation";
import { PlayerControls } from "../src/features/player/components/player-controls";
import { PlayerSettingsPanel } from "../src/features/player/components/player-settings-panel";
import { PlayerTopBar } from "../src/features/player/components/player-top-bar";
import { LuxPlayer } from "../src/features/player/components/lux-player";
import { playerFailure } from "../src/features/player/components/player-diagnostics";
import type { PlayerChapterSegment } from "../src/features/player/player-chapters";

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
    expect(markup).toContain('crossorigin="anonymous"');
    expect(markup).toContain('poster="/images/movie-1"');
    expect(markup).toContain('aria-label="播放 示例电影"');
    expect(markup).toContain("正在准备客户端解码…");
    expect(markup).toContain("客户端解码速度低于实时");
    expect(markup).toContain("客户端解码失败");
  });

  it("omits anonymous CORS mode for remote STRM playback", () => {
    const markup = renderToStaticMarkup(
      <PlayerVideoSurface
        streamUrl="/api/v1/playback/sessions/session-1/direct"
        corsEnabled={false}
        title="远程 STRM"
        videoRef={() => undefined}
        onClick={() => undefined}
        onDoubleClick={() => undefined}
        fallbackLoading={false}
        fallbackSpeedX={null}
        errorMessage={null}
        showError={false}
        onRetry={() => undefined}
        onBack={() => undefined}
      />,
    );

    expect(markup).not.toContain('crossorigin="anonymous"');
  });

  it("applies loop, aspect ratio, and flip to the current video presentation", () => {
    expect(playerVideoPresentationStyle("default", "normal")).toBeUndefined();
    expect(playerVideoPresentationStyle("4:3", "horizontal")).toMatchObject({
      aspectRatio: "4 / 3",
      objectFit: "fill",
      transform: "translate(-50%, -50%) scaleX(-1)",
    });
    expect(playerVideoPresentationSize("4:3", 1440, 900)).toEqual({ width: 1200, height: 900 });
    expect(playerVideoPresentationSize("16:9", 390, 844)).toEqual({ width: 390, height: 219.375 });

    const markup = renderToStaticMarkup(
      <PlayerVideoSurface
        streamUrl="/signed/movie-presentation"
        title="画面设置测试"
        videoRef={() => undefined}
        onClick={() => undefined}
        onDoubleClick={() => undefined}
        presentation={{ loop: true, aspectRatio: "16:9", flip: "vertical" }}
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

  it("points the generic surface fallback at the detail page version list", () => {
    const markup = renderToStaticMarkup(
      <PlayerVideoSurface
        streamUrl="/signed/movie-1"
        title="示例电影"
        videoRef={() => undefined}
        onClick={() => undefined}
        onDoubleClick={() => undefined}
        fallbackLoading={false}
        fallbackSpeedX={null}
        errorMessage={null}
        showError
        onRetry={() => undefined}
        onBack={() => undefined}
      />,
    );

    expect(markup).toContain("请返回详情页选择其他版本");
    expect(markup).not.toContain("请尝试其他版本");
  });

  it("renders top-level media context and source selection as presentation", () => {
    const markup = renderToStaticMarkup(
      <PlayerTopBar
        title="示例电影"
        subtitle="2026"
        onBack={() => undefined}
        airPlayAvailable
        pictureInPictureEnabled
        onAirPlay={() => undefined}
      />,
    );

    expect(markup).toContain("示例电影");
    expect(markup).not.toContain("1080P • H264");
    expect(markup).toContain('aria-label="返回"');
    expect(markup).toContain('class="lux-player-topbar-actions"');
    expect(markup).toContain('aria-label="AirPlay"');
    expect(markup).toContain('aria-label="画中画"');
  });

  const presentation = {
    loop: true,
    aspectRatio: "4:3" as const,
    flip: "horizontal" as const,
    onToggleLoop: () => undefined,
    onChangeAspectRatio: () => undefined,
    onChangeFlip: () => undefined,
  };

  it("renders the settings drawer with quality, aspect ratio and playback information tabs", () => {
    const markup = renderToStaticMarkup(
      <PlayerSettingsPanel presentation={presentation} onClose={() => undefined} />,
    );

    expect(markup).toContain('role="dialog"');
    expect(markup).toContain('aria-label="播放设置"');
    expect(markup).toContain('role="tablist"');
    for (const label of ["画质", "长宽比", "播放信息"]) expect(markup).toContain(`>${label}</button>`);
    expect(markup).not.toContain(">弹幕</button>");
    // The played version is chosen on the detail page; the panel must not repeat it.
    expect(markup).not.toContain("播放版本");
    expect(markup).not.toContain("播放速度");
    expect(markup).not.toContain("快捷键提示");
    expect(markup).toContain('aria-label="关闭播放设置"');
  });

  it("shows loop and auto-play switches on the quality tab", () => {
    const markup = renderToStaticMarkup(
      <PlayerSettingsPanel
        tab="quality"
        presentation={presentation}
        autoPlay={{ enabled: false, onToggle: () => undefined }}
        onClose={() => undefined}
      />,
    );
    expect(markup).toContain("循环播放");
    expect(markup).toContain("自动连播下一集");
    expect(markup).toContain('role="switch"');
    expect(markup).toContain('aria-checked="true"');
    expect(markup).not.toContain('id="lux-player-caption-offset"');
  });

  it("lists aspect ratios with fill and the flip choices on the aspect tab", () => {
    const markup = renderToStaticMarkup(
      <PlayerSettingsPanel tab="aspect" presentation={presentation} onClose={() => undefined} />,
    );
    expect(markup).toContain('aria-label="长宽比"');
    for (const label of ["默认", "16:9", "4:3", "填充"]) expect(markup).toContain(label);
    expect(markup).toContain("画面翻转");
    expect(markup).toContain("水平镜像");
    expect(markup).toMatch(/data-value="4:3"[^>]*>|aria-checked="true"[^>]*data-value="4:3"/);
  });

  it("shows playback information on its own tab", () => {
    const markup = renderToStaticMarkup(
      <PlayerSettingsPanel
        tab="info"
        info={{ groups: [{ title: "播放", rows: [{ label: "方式", value: "直连播放" }] }], copyText: "方式: 直连播放" }}
        onClose={() => undefined}
      />,
    );
    expect(markup).toContain('aria-label="复制播放信息"');
    expect(markup).toContain("直连播放");
  });

  it("exposes a labeled keyboard timeline alongside the playback controls", () => {
    const chapters: PlayerChapterSegment[] = [
      { id: "chapter-1", start: 0, end: 50, title: "开场", markerType: "CHAPTER", chapterIndex: 0 },
      { id: "chapter-2", start: 50, end: 100, title: "片尾开始", markerType: "CREDITS_START", chapterIndex: 1 },
    ];
    const markup = renderToStaticMarkup(
      <PlayerControls
        playing={false}
        currentTime={10}
        duration={100}
        bufferedEnd={25}
        volume={1}
        muted={false}
        fullscreen={false}
        danmuVisible
        chapters={chapters}
        introSkip={{ start: 0, end: 20 }}
        airPlayAvailable
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
        onToggleMute={() => undefined}
        onVolumeChange={() => undefined}
        onToggleRemainingTime={() => undefined}
        onToggleDanmu={() => undefined}
        onChapterSeek={() => undefined}
        onSkipIntro={() => undefined}
        onAirPlay={() => undefined}
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
    const timelineRowStart = markup.indexOf('<div class="lux-player-timeline-row">');
    const controlsStart = markup.indexOf('<div class="lux-player-controls">');
    const timeDisplayIndex = markup.indexOf('class="lux-player-timeline-times"');
    expect(timelineRowStart).toBeGreaterThanOrEqual(0);
    expect(timeDisplayIndex).toBeGreaterThan(timelineRowStart);
    expect(timeDisplayIndex).toBeLessThan(controlsStart);
    expect(markup).toMatch(/<div class="lux-player-timeline-row"><div class="lux-player-timeline-times"><span class="lux-player-time-current">/);
    expect(markup).not.toMatch(/<div class="lux-player-controls-left">[\s\S]*lux-player-timeline-times/);
    const timelineRailIndex = markup.indexOf('class="lux-player-timeline-rail"');
    const chapterRailIndex = markup.indexOf('class="lux-player-chapter-rail"');
    expect(timelineRailIndex).toBeGreaterThanOrEqual(0);
    expect(chapterRailIndex).toBeGreaterThan(timelineRailIndex);
    expect((markup.match(/class="lux-player-timeline-rail"/g) ?? []).length).toBe(1);
    expect(markup).toContain('aria-label="播放"');
    expect(markup).toContain('aria-label="音量调节"');
    expect(markup).toContain('style="--lux-volume-percent:100%"');
    expect(markup).toContain('aria-label="隐藏弹幕"');
    expect(markup).toContain('aria-pressed="true"');
    expect(markup).toMatch(/<div class="lux-player-controls-middle"><\/div><div class="lux-player-controls-right">[\s\S]*class="lux-player-action-btn lux-player-danmu-btn/);
    expect(markup).toContain('aria-label="AirPlay"');
    expect(markup).toContain('aria-label="章节：开场"');
    expect(markup).toContain('aria-label="章节：片尾开始"');
    expect(markup).toContain('data-marker-type="CREDITS_START"');
    expect(markup).toContain('aria-label="跳过片头"');
    expect(markup).not.toContain('aria-label="选择播放版本"');
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
