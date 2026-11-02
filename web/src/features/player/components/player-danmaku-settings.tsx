import { Check, RotateCcw, X } from "lucide-react";
import { useState } from "react";
import {
  DANMAKU_SETTINGS_LIMITS,
  normalizeDanmakuKeyword,
  type DanmakuArea,
  type DanmakuSettings,
} from "../danmaku/settings";
import type { DanmakuMode } from "../danmaku/parse";

export type PlayerDanmakuSettingsProps = {
  settings: DanmakuSettings;
  /** Receives a partial patch; the player normalizes and persists it. */
  onChange: (patch: Partial<DanmakuSettings>) => void;
  /** Restores every danmaku setting to its default. */
  onReset: () => void;
};

const AREA_OPTIONS: readonly { value: DanmakuArea; label: string }[] = [
  { value: "full", label: "全屏" },
  { value: "half", label: "上 1/2" },
  { value: "quarter", label: "上 1/4" },
];

const MODE_OPTIONS: readonly { mode: DanmakuMode; label: string; ariaLabel: string }[] = [
  { mode: "scroll", label: "滚动", ariaLabel: "屏蔽滚动弹幕" },
  { mode: "top", label: "顶部", ariaLabel: "屏蔽顶部弹幕" },
  { mode: "bottom", label: "底部", ariaLabel: "屏蔽底部弹幕" },
];

function formatScale(value: number) {
  return `${value.toFixed(1)}×`;
}

function formatOffset(value: number) {
  return `${value > 0 ? "+" : ""}${value.toFixed(1)}s`;
}

/**
 * The “弹幕” section of the player settings dialog.
 *
 * It owns only the draft keyword input; every persisted value comes from `settings` and every change
 * is reported through `onChange`, so the page keeps a single source of truth (the local settings
 * store) and the overlay updates live. Controls are native inputs/buttons with Chinese accessible
 * names, so keyboard and screen-reader use matches the rest of the settings panel.
 */
export function PlayerDanmakuSettings({ settings, onChange, onReset }: PlayerDanmakuSettingsProps) {
  const [keywordDraft, setKeywordDraft] = useState("");
  const opacityPercent = Math.round(settings.opacity * 100);
  const keywordsAtLimit = settings.keywords.length >= DANMAKU_SETTINGS_LIMITS.maxKeywords;
  const pendingKeyword = normalizeDanmakuKeyword(keywordDraft);
  const duplicateKeyword = pendingKeyword !== null
    && settings.keywords.some((keyword) => keyword.toLowerCase() === pendingKeyword.toLowerCase());
  const canAddKeyword = pendingKeyword !== null && !duplicateKeyword && !keywordsAtLimit;

  const toggleMode = (mode: DanmakuMode) => {
    onChange({
      blockedModes: settings.blockedModes.includes(mode)
        ? settings.blockedModes.filter((blocked) => blocked !== mode)
        : [...settings.blockedModes, mode],
    });
  };

  const addKeyword = () => {
    if (!canAddKeyword || pendingKeyword === null) return;
    onChange({ keywords: [...settings.keywords, pendingKeyword] });
    setKeywordDraft("");
  };

  return (
    <div
      className="lux-player-settings-section lux-player-danmaku-settings"
      data-settings-section="danmaku"
      role="group"
      aria-label="弹幕设置"
    >
      <span className="lux-player-settings-label">弹幕</span>

      <div className="lux-player-danmaku-row">
        <label className="lux-player-settings-label" htmlFor="lux-player-danmaku-opacity">
          不透明度
        </label>
        <output htmlFor="lux-player-danmaku-opacity" className="lux-player-danmaku-value">
          {opacityPercent}%
        </output>
      </div>
      <input
        id="lux-player-danmaku-opacity"
        className="lux-player-caption-offset"
        type="range"
        min={Math.round(DANMAKU_SETTINGS_LIMITS.opacityMin * 100)}
        max={Math.round(DANMAKU_SETTINGS_LIMITS.opacityMax * 100)}
        step={Math.round(DANMAKU_SETTINGS_LIMITS.opacityStep * 100)}
        value={opacityPercent}
        aria-label="弹幕不透明度"
        aria-valuetext={`${opacityPercent}%`}
        onChange={(event) => onChange({ opacity: Number(event.target.value) / 100 })}
      />

      <div className="lux-player-danmaku-row">
        <label className="lux-player-settings-label" htmlFor="lux-player-danmaku-font-scale">
          字号
        </label>
        <output htmlFor="lux-player-danmaku-font-scale" className="lux-player-danmaku-value">
          {formatScale(settings.fontScale)}
        </output>
      </div>
      <input
        id="lux-player-danmaku-font-scale"
        className="lux-player-caption-offset"
        type="range"
        min={DANMAKU_SETTINGS_LIMITS.fontScaleMin}
        max={DANMAKU_SETTINGS_LIMITS.fontScaleMax}
        step={DANMAKU_SETTINGS_LIMITS.fontScaleStep}
        value={settings.fontScale}
        aria-label="弹幕字号"
        aria-valuetext={formatScale(settings.fontScale)}
        onChange={(event) => onChange({ fontScale: Number(event.target.value) })}
      />

      <div className="lux-player-danmaku-row">
        <label className="lux-player-settings-label" htmlFor="lux-player-danmaku-speed">
          速度
        </label>
        <output htmlFor="lux-player-danmaku-speed" className="lux-player-danmaku-value">
          {formatScale(settings.speed)}
        </output>
      </div>
      <input
        id="lux-player-danmaku-speed"
        className="lux-player-caption-offset"
        type="range"
        min={DANMAKU_SETTINGS_LIMITS.speedMin}
        max={DANMAKU_SETTINGS_LIMITS.speedMax}
        step={DANMAKU_SETTINGS_LIMITS.speedStep}
        value={settings.speed}
        aria-label="弹幕速度"
        aria-valuetext={formatScale(settings.speed)}
        onChange={(event) => onChange({ speed: Number(event.target.value) })}
      />

      <div className="lux-player-danmaku-row">
        <label className="lux-player-settings-label" htmlFor="lux-player-danmaku-offset">
          时间偏移
        </label>
        <output htmlFor="lux-player-danmaku-offset" className="lux-player-danmaku-value">
          {formatOffset(settings.offsetSeconds)}
        </output>
      </div>
      <input
        id="lux-player-danmaku-offset"
        className="lux-player-caption-offset"
        type="range"
        min={DANMAKU_SETTINGS_LIMITS.offsetMin}
        max={DANMAKU_SETTINGS_LIMITS.offsetMax}
        step={DANMAKU_SETTINGS_LIMITS.offsetStep}
        value={settings.offsetSeconds}
        aria-label="弹幕时间偏移"
        aria-valuetext={formatOffset(settings.offsetSeconds)}
        onChange={(event) => onChange({ offsetSeconds: Number(event.target.value) })}
      />
      <p className="lux-player-danmaku-hint">正值提前，负值延后。</p>

      <span className="lux-player-settings-label">显示区域</span>
      <div className="lux-player-speed-grid" role="group" aria-label="弹幕显示区域">
        {AREA_OPTIONS.map((option) => (
          <button
            key={option.value}
            type="button"
            className={`lux-player-speed-pill ${settings.area === option.value ? "is-active" : ""}`}
            aria-label={option.label}
            aria-pressed={settings.area === option.value}
            onClick={() => onChange({ area: option.value })}
          >
            {option.label}
          </button>
        ))}
      </div>

      <span className="lux-player-settings-label">屏蔽类型</span>
      <div className="lux-player-speed-grid" role="group" aria-label="屏蔽弹幕类型">
        {MODE_OPTIONS.map((option) => {
          const blocked = settings.blockedModes.includes(option.mode);
          return (
            <button
              key={option.mode}
              type="button"
              className={`lux-player-speed-pill ${blocked ? "is-active" : ""}`}
              role="switch"
              aria-checked={blocked}
              aria-label={option.ariaLabel}
              onClick={() => toggleMode(option.mode)}
            >
              {option.label}
              {blocked ? <Check size={14} aria-hidden="true" /> : null}
            </button>
          );
        })}
      </div>

      <div className="lux-player-danmaku-row">
        <span className="lux-player-settings-label" id="lux-player-danmaku-keywords-label">关键词屏蔽</span>
        <span className="lux-player-danmaku-value" aria-hidden="true">
          {settings.keywords.length}/{DANMAKU_SETTINGS_LIMITS.maxKeywords}
        </span>
      </div>
      <form
        className="lux-player-danmaku-keyword-form"
        onSubmit={(event) => {
          event.preventDefault();
          addKeyword();
        }}
      >
        <input
          className="lux-player-danmaku-keyword-input"
          type="text"
          value={keywordDraft}
          maxLength={DANMAKU_SETTINGS_LIMITS.maxKeywordLength}
          placeholder="输入要屏蔽的文字"
          aria-label="屏蔽关键词"
          onChange={(event) => setKeywordDraft(event.target.value)}
        />
        <button
          type="submit"
          className="lux-player-danmaku-keyword-add"
          aria-label="添加屏蔽关键词"
          disabled={!canAddKeyword}
        >
          添加
        </button>
      </form>
      {settings.keywords.length > 0 ? (
        <ul className="lux-player-danmaku-keywords">
          {settings.keywords.map((keyword) => (
            <li key={keyword.toLowerCase()} className="lux-player-danmaku-keyword">
              <span>{keyword}</span>
              <button
                type="button"
                className="lux-player-danmaku-keyword-remove"
                aria-label={`删除屏蔽词 ${keyword}`}
                onClick={() => onChange({
                  keywords: settings.keywords.filter((existing) => existing !== keyword),
                })}
              >
                <X size={12} aria-hidden="true" />
              </button>
            </li>
          ))}
        </ul>
      ) : null}
      <p className="lux-player-danmaku-hint">
        普通文本、不区分大小写的包含匹配，最多 {DANMAKU_SETTINGS_LIMITS.maxKeywords} 条，每条不超过{" "}
        {DANMAKU_SETTINGS_LIMITS.maxKeywordLength} 个字符。
      </p>

      <button
        type="button"
        className="lux-player-settings-info-row"
        aria-label="恢复默认弹幕设置"
        onClick={onReset}
      >
        <RotateCcw size={16} aria-hidden="true" />
        恢复默认
      </button>
    </div>
  );
}
