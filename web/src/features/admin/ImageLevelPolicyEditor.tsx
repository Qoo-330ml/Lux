import {
  countPresets,
  presetsWithCurrent,
  widthPresets,
} from "./image-policy-presets";
import type {
  MediaArtworkOverwrite,
  MediaArtworkPolicySettings,
  MediaArtworkSource,
  MediaArtworkTypeKey,
  MediaImageLevelKey,
  MediaImageLevelSettings,
  MediaImageStrategySettings,
  MediaScraperRole,
} from "../../lib/api/types";

const levels: Array<{ key: MediaImageLevelKey; label: string }> = [
  { key: "movie", label: "电影" },
  { key: "series", label: "剧集" },
  { key: "season", label: "季度" },
  { key: "episode", label: "单集" },
];

const artworkTypes: Array<{ key: MediaArtworkTypeKey; label: string }> = [
  { key: "poster", label: "海报" },
  { key: "backdrop", label: "背景图" },
  { key: "artwork", label: "艺术图 Art" },
  { key: "logo", label: "徽标" },
  { key: "thumbnail", label: "缩略图" },
  { key: "banner", label: "横幅图" },
  { key: "disc", label: "光盘封面" },
  { key: "wallpaper", label: "壁纸" },
];

const sourceOptions: Array<{ value: string; label: string }> = [
  { value: "", label: "继承" },
  { value: "SCRAPER,SCREENSHOT", label: "在线 + 截图" },
  { value: "SCRAPER", label: "仅在线" },
  { value: "SCREENSHOT", label: "仅截图" },
];

const overwriteOptions: Array<{ value: MediaArtworkOverwrite | ""; label: string }> = [
  { value: "", label: "继承" },
  { value: "KEEP", label: "保留已有" },
  { value: "FILL", label: "仅补缺" },
  { value: "OVERWRITE", label: "覆盖（完整刮削）" },
];

const roleOptions: Array<{ value: string; label: string }> = [
  { value: "", label: "继承" },
  { value: "PRIMARY,BACKUP,SUPPLEMENT", label: "全部角色" },
  { value: "PRIMARY,BACKUP", label: "主 + 备" },
  { value: "PRIMARY", label: "仅主来源" },
  { value: "BACKUP", label: "仅备用" },
  { value: "SUPPLEMENT", label: "仅补充" },
];

const enabledOptions: Array<{ value: string; label: string }> = [
  { value: "", label: "继承" },
  { value: "true", label: "抓取" },
  { value: "false", label: "不抓" },
];

const episodeImageOptions: Array<{ value: string; label: string }> = [
  { value: "", label: "继承缩略图方式" },
  { value: "SCRAPER_STILL", label: "在线剧照" },
  { value: "LOCAL_SCREENSHOT", label: "本地截图" },
  { value: "NONE", label: "都不要" },
];

function pruneLevels(
  levels: Partial<Record<MediaImageLevelKey, MediaImageLevelSettings>>,
): Partial<Record<MediaImageLevelKey, MediaImageLevelSettings>> | undefined {
  const next: Partial<Record<MediaImageLevelKey, MediaImageLevelSettings>> = {};
  for (const [levelKey, level] of Object.entries(levels) as Array<
    [MediaImageLevelKey, MediaImageLevelSettings]
  >) {
    const artworks = Object.entries(level.artworks ?? {}).filter(
      ([, policy]) => policy && Object.keys(policy).length > 0,
    );
    const cleaned: MediaImageLevelSettings = {};
    if (level.thumbnailScrapingMode) cleaned.thumbnailScrapingMode = level.thumbnailScrapingMode;
    if (level.episodeImagePolicy) cleaned.episodeImagePolicy = level.episodeImagePolicy;
    if (artworks.length > 0) {
      cleaned.artworks = Object.fromEntries(artworks) as MediaImageLevelSettings["artworks"];
    }
    if (Object.keys(cleaned).length > 0) next[levelKey] = cleaned;
  }
  return Object.keys(next).length > 0 ? next : undefined;
}

export function updateArtworkPolicy(
  images: MediaImageStrategySettings,
  levelKey: MediaImageLevelKey,
  typeKey: MediaArtworkTypeKey,
  patch: Partial<MediaArtworkPolicySettings>,
): MediaImageStrategySettings {
  const currentLevel = images.levels?.[levelKey] ?? {};
  const currentPolicy = currentLevel.artworks?.[typeKey] ?? {};
  const nextPolicy: MediaArtworkPolicySettings = { ...currentPolicy, ...patch };
  for (const [key, value] of Object.entries(nextPolicy) as Array<
    [keyof MediaArtworkPolicySettings, unknown]
  >) {
    if (value === undefined || value === null || value === "") delete nextPolicy[key];
  }
  const nextLevel: MediaImageLevelSettings = {
    ...currentLevel,
    artworks: { ...(currentLevel.artworks ?? {}), [typeKey]: nextPolicy },
  };
  const levels = pruneLevels({ ...(images.levels ?? {}), [levelKey]: nextLevel });
  const next: MediaImageStrategySettings = { ...images };
  if (levels) next.levels = levels;
  else delete next.levels;
  return next;
}

export function updateLevelEpisodePolicy(
  images: MediaImageStrategySettings,
  levelKey: MediaImageLevelKey,
  value: string,
): MediaImageStrategySettings {
  const currentLevel: MediaImageLevelSettings = { ...(images.levels?.[levelKey] ?? {}) };
  if (value) currentLevel.episodeImagePolicy = value as MediaImageLevelSettings["episodeImagePolicy"];
  else delete currentLevel.episodeImagePolicy;
  const levels = pruneLevels({ ...(images.levels ?? {}), [levelKey]: currentLevel });
  const next: MediaImageStrategySettings = { ...images };
  if (levels) next.levels = levels;
  else delete next.levels;
  return next;
}

function parseSources(value: string): MediaArtworkSource[] | undefined {
  if (!value) return undefined;
  return value.split(",") as MediaArtworkSource[];
}

function parseRoles(value: string): MediaScraperRole[] | undefined {
  if (!value) return undefined;
  return value.split(",") as MediaScraperRole[];
}

function levelOverrideCount(level?: MediaImageLevelSettings): number {
  if (!level) return 0;
  let count = Object.values(level.artworks ?? {}).filter(
    (policy) => policy && Object.keys(policy).length > 0,
  ).length;
  if (level.thumbnailScrapingMode) count += 1;
  if (level.episodeImagePolicy) count += 1;
  return count;
}

export function ImageLevelPolicyEditor({
  images,
  onChange,
  inheritLabel = "继承",
  emptySummary = "全部继承",
}: {
  images: MediaImageStrategySettings;
  onChange: (images: MediaImageStrategySettings) => void;
  /** Label for "no override set" — global settings has no parent layer, so it
   * uses 默认 instead of 继承. */
  inheritLabel?: string;
  emptySummary?: string;
}) {
  const optionLabel = (option: { value: string; label: string }) =>
    option.value === "" ? inheritLabel : option.label;

  return (
    <div className="lux-level-policy">
      <p className="lux-library-strategy-help">
        逐层逐类型覆盖上面的默认设置。未选择的项目显示“{inheritLabel}”，按上层取值；本地已有图片始终优先，且不会被静默覆盖。
      </p>
      {levels.map((level) => {
        const levelSettings = images.levels?.[level.key];
        const overrides = levelOverrideCount(levelSettings);
        return (
          <details className="lux-level-policy-group" key={level.key}>
            <summary>
              <span>{level.label}</span>
              <small>{overrides > 0 ? `${overrides} 项自定义` : emptySummary}</small>
            </summary>
            {level.key === "episode" ? (
              <label className="lux-level-policy-episode">
                单集图片
                <select
                  aria-label={`${level.label}图片来源`}
                  value={levelSettings?.episodeImagePolicy ?? ""}
                  onChange={(event) =>
                    onChange(updateLevelEpisodePolicy(images, level.key, event.target.value))
                  }
                >
                  {episodeImageOptions.map((option) => (
                    <option key={option.value} value={option.value}>
                      {option.value === "" ? inheritLabel : option.label}
                    </option>
                  ))}
                </select>
              </label>
            ) : null}
            <table className="lux-level-policy-table">
              <thead>
                <tr>
                  <th scope="col">图片类型</th>
                  <th scope="col">开关</th>
                  <th scope="col">来源</th>
                  <th scope="col">覆盖</th>
                  <th scope="col">张数</th>
                  <th scope="col">最小宽度</th>
                  <th scope="col">刮削器角色</th>
                </tr>
              </thead>
              <tbody>
                {artworkTypes.map((type) => {
                  const policy = levelSettings?.artworks?.[type.key] ?? {};
                  return (
                    <tr key={type.key}>
                      <th scope="row">{type.label}</th>
                      <td>
                        <select
                          aria-label={`${level.label}${type.label}开关`}
                          value={policy.enabled === undefined ? "" : String(policy.enabled)}
                          onChange={(event) =>
                            onChange(
                              updateArtworkPolicy(images, level.key, type.key, {
                                enabled:
                                  event.target.value === ""
                                    ? undefined
                                    : event.target.value === "true",
                              }),
                            )
                          }
                        >
                          {enabledOptions.map((option) => (
                            <option key={option.value} value={option.value}>
                              {optionLabel(option)}
                            </option>
                          ))}
                        </select>
                      </td>
                      <td>
                        <select
                          aria-label={`${level.label}${type.label}来源`}
                          value={policy.sources?.join(",") ?? ""}
                          onChange={(event) =>
                            onChange(
                              updateArtworkPolicy(images, level.key, type.key, {
                                sources: parseSources(event.target.value),
                              }),
                            )
                          }
                        >
                          {sourceOptions.map((option) => (
                            <option key={option.value} value={option.value}>
                              {optionLabel(option)}
                            </option>
                          ))}
                        </select>
                      </td>
                      <td>
                        <select
                          aria-label={`${level.label}${type.label}覆盖策略`}
                          value={policy.overwrite ?? ""}
                          onChange={(event) =>
                            onChange(
                              updateArtworkPolicy(images, level.key, type.key, {
                                overwrite: (event.target.value ||
                                  undefined) as MediaArtworkPolicySettings["overwrite"],
                              }),
                            )
                          }
                        >
                          {overwriteOptions.map((option) => (
                            <option key={option.value} value={option.value}>
                              {optionLabel(option)}
                            </option>
                          ))}
                        </select>
                      </td>
                      <td>
                        <select
                          aria-label={`${level.label}${type.label}张数`}
                          value={policy.maxCount ?? ""}
                          onChange={(event) =>
                            onChange(
                              updateArtworkPolicy(images, level.key, type.key, {
                                maxCount:
                                  event.target.value === ""
                                    ? undefined
                                    : Number(event.target.value),
                              }),
                            )
                          }
                        >
                          {presetsWithCurrent(countPresets, policy.maxCount).map((option) => (
                            <option key={option.value} value={option.value}>
                              {optionLabel(option)}
                            </option>
                          ))}
                        </select>
                      </td>
                      <td>
                        <select
                          aria-label={`${level.label}${type.label}最小宽度`}
                          value={policy.minWidth ?? ""}
                          onChange={(event) =>
                            onChange(
                              updateArtworkPolicy(images, level.key, type.key, {
                                minWidth:
                                  event.target.value === ""
                                    ? undefined
                                    : Number(event.target.value),
                              }),
                            )
                          }
                        >
                          {presetsWithCurrent(widthPresets, policy.minWidth).map((option) => (
                            <option key={option.value} value={option.value}>
                              {optionLabel(option)}
                            </option>
                          ))}
                        </select>
                      </td>
                      <td>
                        <select
                          aria-label={`${level.label}${type.label}刮削器角色`}
                          value={policy.scraperRoles?.join(",") ?? ""}
                          onChange={(event) =>
                            onChange(
                              updateArtworkPolicy(images, level.key, type.key, {
                                scraperRoles: parseRoles(event.target.value),
                              }),
                            )
                          }
                        >
                          {roleOptions.map((option) => (
                            <option key={option.value} value={option.value}>
                              {optionLabel(option)}
                            </option>
                          ))}
                        </select>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </details>
        );
      })}
    </div>
  );
}
