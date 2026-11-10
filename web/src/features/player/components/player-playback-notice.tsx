import "./player-playback-notice.css";
import { AlertCircle, AlertTriangle, ArrowLeft, Info, RotateCcw, X } from "lucide-react";
import { useEffect, useState } from "react";
import type { PlaybackNotice } from "../playback-notices";

/**
 * One place for every playback state the viewer cannot resolve inside the video element.
 *
 * A blocking notice (`UNSUPPORTED` reason or a rejected session) replaces the video surface and
 * carries the retry/back actions; a transcode banner sits above the picture, stays out of the
 * control bar and can be dismissed for the current playback session.
 */

function NoticeIcon({ severity, size }: { severity: PlaybackNotice["severity"]; size: number }) {
  if (severity === "info") return <Info size={size} aria-hidden="true" />;
  if (severity === "warning") return <AlertTriangle size={size} aria-hidden="true" />;
  return <AlertCircle size={size} aria-hidden="true" />;
}

type PlayerPlaybackNoticeLayerProps = {
  notice: PlaybackNotice;
  onRetry: () => void;
  onBack: () => void;
};

export function PlayerPlaybackNoticeLayer({
  notice,
  onRetry,
  onBack,
}: PlayerPlaybackNoticeLayerProps) {
  return (
    <div className="lux-player-notice-layer" role="alert">
      <div className={`lux-player-notice-card lux-player-notice-card-${notice.severity}`}>
        <span className="lux-player-notice-icon">
          <NoticeIcon severity={notice.severity} size={36} />
        </span>
        <h2>{notice.title}</h2>
        <p className="lux-player-notice-message">{notice.message}</p>
        {notice.hint ? <p className="lux-player-notice-hint">{notice.hint}</p> : null}
        <div className="lux-player-notice-actions">
          <button
            className="lux-player-glass-btn"
            type="button"
            onClick={onRetry}
            aria-label="重试"
          >
            <RotateCcw size={16} aria-hidden="true" /> 重试
          </button>
          <button
            className="lux-player-glass-btn lux-player-glass-btn-primary"
            type="button"
            onClick={onBack}
            aria-label="返回上一页"
          >
            <ArrowLeft size={16} aria-hidden="true" /> 返回
          </button>
        </div>
      </div>
    </div>
  );
}

type PlayerPlaybackNoticeBannerProps = {
  notice: PlaybackNotice;
  /** Changes for every playback session so a dismissal does not leak into the next one. */
  lifecycleKey?: string;
  onDismiss?: () => void;
};

export function PlayerPlaybackNoticeBanner({
  notice,
  lifecycleKey = "",
  onDismiss,
}: PlayerPlaybackNoticeBannerProps) {
  const [dismissedKey, setDismissedKey] = useState<string | null>(null);

  useEffect(() => {
    setDismissedKey(null);
  }, [lifecycleKey, notice.key]);

  if (notice.blocking || dismissedKey === notice.key) return null;

  return (
    <div className="lux-player-notice-banner" role="status" aria-live="polite">
      <span className="lux-player-notice-banner-icon">
        <NoticeIcon severity={notice.severity} size={16} />
      </span>
      <div className="lux-player-notice-banner-body">
        <p className="lux-player-notice-banner-title">{notice.title}</p>
        <p className="lux-player-notice-banner-message">{notice.message}</p>
        {notice.hint ? <p className="lux-player-notice-banner-hint">{notice.hint}</p> : null}
      </div>
      <button
        className="lux-player-notice-banner-close"
        type="button"
        aria-label="关闭提示"
        onClick={() => {
          setDismissedKey(notice.key);
          onDismiss?.();
        }}
      >
        <X size={14} aria-hidden="true" />
      </button>
    </div>
  );
}
