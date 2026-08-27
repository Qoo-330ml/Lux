import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { PlayerErrorState, PlayerLoadingState } from "../src/features/player/components/player-state";
import { PlayerVideoSurface } from "../src/features/player/components/player-video-surface";

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
});
