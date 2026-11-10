import { X } from "lucide-react";
import type { PlayerInfoGroup } from "../player-stats";
import { PlayerDanmakuSettings, type PlayerDanmakuSettingsProps } from "./player-danmaku-settings";
import { PlayerInfoSection } from "./player-info-section";
import { PlayerOptionList } from "./player-option-list";
import type {
  PlayerAspectRatio,
  PlayerFlip,
  PlayerVideoPresentation,
} from "./player-presentation";
import {
  PlayerQualitySelector,
  type PlayerQualitySection,
} from "./player-quality-selector";

export type PlayerSettingsTab = "quality" | "aspect" | "danmaku" | "info";

type PlayerPresentationSettings = PlayerVideoPresentation & {
  onToggleLoop: () => void;
  onChangeAspectRatio: (aspectRatio: PlayerAspectRatio) => void;
  onChangeFlip: (flip: PlayerFlip) => void;
};

type PlayerAutoPlaySettings = {
  enabled: boolean;
  onToggle: () => void;
};

type PlayerSettingsPanelProps = {
  tab?: PlayerSettingsTab;
  onTabChange?: (tab: PlayerSettingsTab) => void;
  /** Independent quality (resolution ceiling) slot; omitted for Direct-only sources. */
  quality?: PlayerQualitySection;
  presentation?: PlayerPresentationSettings;
  autoPlay?: PlayerAutoPlaySettings;
  /** Browser-local danmaku settings; the 弹幕 tab is omitted when the page has no danmaku track. */
  danmaku?: PlayerDanmakuSettingsProps;
  /** Playback information shown in the 播放信息 tab. */
  info?: { groups: PlayerInfoGroup[]; copyText: string };
  onClose: () => void;
};

const TABS: readonly { value: PlayerSettingsTab; label: string }[] = [
  { value: "quality", label: "画质" },
  { value: "aspect", label: "长宽比" },
  { value: "danmaku", label: "弹幕" },
  { value: "info", label: "播放信息" },
];

const ASPECT_RATIO_OPTIONS: readonly { value: PlayerAspectRatio; label: string; note?: string }[] = [
  { value: "default", label: "默认", note: "保持原始比例" },
  { value: "16:9", label: "16:9" },
  { value: "4:3", label: "4:3" },
  { value: "fill", label: "填充", note: "裁切铺满" },
];

const FLIP_OPTIONS: readonly { value: PlayerFlip; label: string }[] = [
  { value: "normal", label: "正常" },
  { value: "horizontal", label: "水平镜像" },
  { value: "vertical", label: "垂直镜像" },
];

/**
 * Right-hand settings drawer with three tabs: quality, aspect ratio and playback information.
 * Subtitles, audio tracks and speed live in the control bar instead.
 */
export function PlayerSettingsPanel({
  tab = "quality",
  onTabChange = () => undefined,
  quality,
  presentation,
  autoPlay,
  danmaku,
  info,
  onClose,
}: PlayerSettingsPanelProps) {
  const tabs = TABS.filter((item) => item.value !== "danmaku" || danmaku);
  return (
    <aside className="lux-player-settings-drawer" role="dialog" aria-label="播放设置">
      <div className="lux-player-settings-header">
        <span>播放设置</span>
        <button
          type="button"
          className="lux-player-settings-close"
          aria-label="关闭播放设置"
          title="关闭"
          onClick={onClose}
        >
          关闭
          <X size={14} aria-hidden="true" />
        </button>
      </div>

      <div className="lux-player-settings-tabs" role="tablist" aria-label="设置分类">
        {tabs.map((item) => (
          <button
            key={item.value}
            type="button"
            role="tab"
            aria-selected={tab === item.value}
            className={`lux-player-settings-tab${tab === item.value ? " is-active" : ""}`}
            onClick={() => onTabChange(item.value)}
          >
            {item.label}
          </button>
        ))}
      </div>

      <div className="lux-player-settings-body" role="tabpanel">
        {tab === "quality" ? (
          <>
            {quality ? <PlayerQualitySelector {...quality} /> : (
              <p className="lux-player-caption-status">当前版本没有可选画质。</p>
            )}
            {autoPlay ? (
              <div className="lux-player-settings-section">
                <h4 className="lux-player-settings-label" id="lux-player-auto-play-label">自动连播下一集</h4>
                <button
                  type="button"
                  className={`lux-player-option${autoPlay.enabled ? " is-selected" : ""}`}
                  role="switch"
                  aria-label="自动连播下一集"
                  aria-checked={autoPlay.enabled}
                  onClick={autoPlay.onToggle}
                >
                  <span className="lux-player-option-label">{autoPlay.enabled ? "已开启" : "已关闭"}</span>
                </button>
              </div>
            ) : null}
            {presentation ? (
              <div className="lux-player-settings-section">
                <h4 className="lux-player-settings-label" id="lux-player-loop-label">循环播放</h4>
                <button
                  type="button"
                  className={`lux-player-option${presentation.loop ? " is-selected" : ""}`}
                  role="switch"
                  aria-labelledby="lux-player-loop-label"
                  aria-checked={presentation.loop}
                  onClick={presentation.onToggleLoop}
                >
                  <span className="lux-player-option-label">{presentation.loop ? "已开启" : "已关闭"}</span>
                </button>
              </div>
            ) : null}
          </>
        ) : null}

        {tab === "aspect" ? (
          presentation ? (
            <>
              <div className="lux-player-settings-section">
                <h4 className="lux-player-settings-label">长宽比</h4>
                <PlayerOptionList
                  label="长宽比"
                  value={presentation.aspectRatio}
                  options={ASPECT_RATIO_OPTIONS}
                  onSelect={(value) => presentation.onChangeAspectRatio(value as PlayerAspectRatio)}
                />
              </div>
              <div className="lux-player-settings-section">
                <h4 className="lux-player-settings-label">画面翻转</h4>
                <PlayerOptionList
                  label="画面翻转"
                  value={presentation.flip}
                  options={FLIP_OPTIONS}
                  onSelect={(value) => presentation.onChangeFlip(value as PlayerFlip)}
                />
              </div>
            </>
          ) : null
        ) : null}

        {tab === "danmaku" ? (danmaku ? <PlayerDanmakuSettings {...danmaku} /> : null) : null}

        {tab === "info" ? (
          info ? <PlayerInfoSection groups={info.groups} copyText={info.copyText} /> : null
        ) : null}
      </div>
    </aside>
  );
}
