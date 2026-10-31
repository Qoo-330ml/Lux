import { useCallback, useEffect, useRef, useState } from "react";
import type { MediaItem } from "../../lib/api/types";
import { mediaTitle } from "../home/media";
import { screenshotFileName } from "./player-page-helpers";

type VideoRef = { current: HTMLVideoElement | null };

export type PlayerScreenshot = {
  screenshotStatus: string | null;
  takeScreenshot: () => void;
};

type PlayerScreenshotOptions = {
  videoRef: VideoRef;
  media: MediaItem | undefined;
};

/**
 * Captures the current video frame to a local PNG download. Nothing is uploaded:
 * the blob URL is created and revoked in the page.
 */
export function usePlayerScreenshot({ videoRef, media }: PlayerScreenshotOptions): PlayerScreenshot {
  const [screenshotStatus, setScreenshotStatus] = useState<string | null>(null);
  const screenshotStatusTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => () => {
    if (screenshotStatusTimeoutRef.current) clearTimeout(screenshotStatusTimeoutRef.current);
  }, []);

  const showScreenshotStatus = useCallback((message: string) => {
    setScreenshotStatus(message);
    if (screenshotStatusTimeoutRef.current) clearTimeout(screenshotStatusTimeoutRef.current);
    screenshotStatusTimeoutRef.current = setTimeout(() => setScreenshotStatus(null), 2_500);
  }, []);

  const takeScreenshot = useCallback(() => {
    const video = videoRef.current;
    if (!video || video.videoWidth <= 0 || video.videoHeight <= 0) {
      showScreenshotStatus("当前视频帧尚未就绪");
      return;
    }

    const canvas = document.createElement("canvas");
    canvas.width = video.videoWidth;
    canvas.height = video.videoHeight;
    const context = canvas.getContext("2d");
    if (!context) {
      showScreenshotStatus("当前浏览器无法生成截图");
      return;
    }

    try {
      context.drawImage(video, 0, 0, canvas.width, canvas.height);
      canvas.toBlob((blob) => {
        if (!blob) {
          showScreenshotStatus("当前媒体不允许截图");
          return;
        }
        try {
          const objectUrl = URL.createObjectURL(blob);
          const anchor = document.createElement("a");
          anchor.href = objectUrl;
          anchor.download = screenshotFileName(media ? mediaTitle(media) : "lux-screenshot");
          anchor.click();
          window.setTimeout(() => URL.revokeObjectURL(objectUrl), 0);
          showScreenshotStatus("截图已保存");
        } catch {
          showScreenshotStatus("当前浏览器无法保存截图");
        }
      }, "image/png");
    } catch {
      showScreenshotStatus("当前媒体不允许截图");
    }
  }, [media, showScreenshotStatus, videoRef]);

  return { screenshotStatus, takeScreenshot };
}
