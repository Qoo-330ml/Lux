import type { LibraryItemsOptions } from "../../lib/api/client";
import type { LibraryKind } from "../../lib/api/types";

/**
 * 媒体库页筛选；后端 `LuxPageQuery` 已支持 `itemType`、`year`、`is_played` 和
 * `is_favorite`，这里只负责把 URL 查询参数翻译成请求参数。
 *
 * URL 参数约定（刷新、分享都要保留）：
 * - `type`：`MOVIE` 或 `SERIES`，只在库本身包含多种类型时有意义；
 * - `year`：四位年份；
 * - `status`：`played`（已看）或 `unplayed`（未看）；
 * - `favorite`：`1` 表示只看收藏。
 */
export const LIBRARY_FILTER_PARAM_NAMES = ["type", "year", "status", "favorite"] as const;
export type LibraryFilterParamName = (typeof LIBRARY_FILTER_PARAM_NAMES)[number];

export const LIBRARY_TYPE_FILTER_VALUES = ["MOVIE", "SERIES"] as const;
export type LibraryTypeFilter = (typeof LIBRARY_TYPE_FILTER_VALUES)[number];

/** 与媒体识别页的年份输入保持一致，避免把明显不合理的输入发给后端。 */
export const LIBRARY_FILTER_YEAR_MIN = 1800;
export const LIBRARY_FILTER_YEAR_MAX = 2200;

export type LibraryFilterState = {
  itemType?: LibraryTypeFilter;
  year?: number;
  /** true = 已看，false = 未看，undefined = 不限。 */
  played?: boolean;
  favoriteOnly: boolean;
};

export const EMPTY_LIBRARY_FILTERS: LibraryFilterState = { favoriteOnly: false };

/** 媒体库可能有能力展示的类型；返回多项时页面才提供类型筛选。 */
export function libraryTypeFilterValues(kind?: LibraryKind): LibraryTypeFilter[] {
  if (kind === "MOVIE") return ["MOVIE"];
  if (kind === "SERIES") return ["SERIES"];
  if (kind === "MIXED") return ["MOVIE", "SERIES"];
  return [];
}

export function isLibraryFilterYear(value: string): boolean {
  if (!/^\d{4}$/.test(value)) return false;
  const year = Number(value);
  return year >= LIBRARY_FILTER_YEAR_MIN && year <= LIBRARY_FILTER_YEAR_MAX;
}

export function readLibraryFilters(params: URLSearchParams, kind?: LibraryKind): LibraryFilterState {
  const rawType = (params.get("type") ?? "").trim().toUpperCase();
  const structuralType = LIBRARY_TYPE_FILTER_VALUES.includes(rawType as LibraryTypeFilter)
    ? (rawType as LibraryTypeFilter)
    : undefined;
  // 库记录还没到达时先按结构接受类型，避免首屏请求被丢弃后再次改键。
  const itemType = structuralType
    && (kind === undefined || libraryTypeFilterValues(kind).includes(structuralType))
    ? structuralType
    : undefined;

  const rawYear = (params.get("year") ?? "").trim();
  const year = isLibraryFilterYear(rawYear) ? Number(rawYear) : undefined;

  const rawStatus = (params.get("status") ?? "").trim().toLowerCase();
  const played = rawStatus === "played" ? true : rawStatus === "unplayed" ? false : undefined;

  const rawFavorite = (params.get("favorite") ?? "").trim().toLowerCase();
  const favoriteOnly = rawFavorite === "1" || rawFavorite === "true";

  return { itemType, year, played, favoriteOnly };
}

export function libraryFiltersActive(filters: LibraryFilterState): boolean {
  return Boolean(filters.itemType) || filters.year != null || filters.played != null || filters.favoriteOnly;
}

/** 供 react-query 键使用：不同筛选组合各自缓存分页，切换时天然回到第 1 页。 */
export function libraryFilterQueryKey(filters: LibraryFilterState): string {
  if (!libraryFiltersActive(filters)) return "none";
  return [
    filters.itemType ?? "-",
    filters.year != null ? String(filters.year) : "-",
    filters.played === true ? "played" : filters.played === false ? "unplayed" : "-",
    filters.favoriteOnly ? "favorite" : "-",
  ].join("|");
}

/** 只下发真正生效的筛选，保持未筛选请求的参数与既有行为一致。 */
export function libraryFilterQueryOptions(
  filters: LibraryFilterState,
): Pick<LibraryItemsOptions, "year" | "isPlayed" | "isFavorite"> {
  return {
    ...(filters.year != null ? { year: filters.year } : {}),
    ...(filters.played != null ? { isPlayed: filters.played } : {}),
    ...(filters.favoriteOnly ? { isFavorite: true } : {}),
  };
}

export function setLibraryFilterParam(
  params: URLSearchParams,
  name: LibraryFilterParamName,
  value: string | undefined,
): URLSearchParams {
  const next = new URLSearchParams(params);
  if (value) next.set(name, value);
  else next.delete(name);
  return next;
}

export function clearLibraryFilterParams(params: URLSearchParams): URLSearchParams {
  const next = new URLSearchParams(params);
  for (const name of LIBRARY_FILTER_PARAM_NAMES) next.delete(name);
  return next;
}
