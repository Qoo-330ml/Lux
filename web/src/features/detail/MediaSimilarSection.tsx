import { useQuery } from "@tanstack/react-query";
import { HorizontalScrollRail } from "../../components/layout/HorizontalScrollRail";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { MediaItem } from "../../lib/api/types";
import { MediaCard } from "../home/media";

const SIMILAR_ITEMS_LIMIT = 12;
const SIMILAR_ITEM_TYPES = new Set(["MOVIE", "SERIES"]);

/**
 * Similar recommendations for a movie or series detail page.
 *
 * Seasons, episodes, collections, videos and other item types never request the
 * endpoint. An empty result or a failed request (404/503/401 included) hides the
 * whole section instead of showing an error inside the detail page. While the
 * first page is loading no placeholder is rendered, matching the stills and
 * episodes sections on the same page.
 */
export function MediaSimilarSection({ item }: { item: Pick<MediaItem, "id" | "itemType"> }) {
  const supported = SIMILAR_ITEM_TYPES.has(item.itemType ?? "");
  const similar = useQuery({
    queryKey: queryKeys.similar(item.id, SIMILAR_ITEMS_LIMIT),
    queryFn: () => api.similarItems(item.id, { limit: SIMILAR_ITEMS_LIMIT }),
    enabled: supported,
    // A missing recommendation list is not an error the reader can act on; do not
    // retry it three times behind the rest of the detail page.
    retry: false,
  });

  if (!supported) return null;
  const items = (similar.data?.items ?? []).slice(0, SIMILAR_ITEMS_LIMIT);
  if (items.length === 0) return null;

  return (
    <section className="lux-detail-similar" aria-labelledby="media-similar-heading">
      <div className="lux-section-heading">
        <div className="lux-heading-lead">
          <h2 id="media-similar-heading">相似推荐</h2>
          <span className="lux-section-heading-sub">SIMILAR</span>
        </div>
        <span>{items.length} 项</span>
      </div>
      <HorizontalScrollRail className="lux-detail-similar-rail" ariaLabel="相似推荐列表">
        <div className="lux-media-rail">
          {items.map((similarItem) => <MediaCard item={similarItem} key={similarItem.id} />)}
        </div>
      </HorizontalScrollRail>
    </section>
  );
}
