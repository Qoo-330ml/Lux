import { defaultRangeExtractor, useWindowVirtualizer } from "@tanstack/react-virtual";
import { useCallback, useLayoutEffect, useRef, useState, type Key, type ReactNode } from "react";
import type { MediaItem } from "../../lib/api/types";

type LibraryGridProps = {
  items: MediaItem[];
  renderItem: (item: MediaItem) => ReactNode;
};

export function LibraryGrid({ items, renderItem }: LibraryGridProps) {
  const gridRef = useRef<HTMLDivElement>(null);
  const [layout, setLayout] = useState({ columns: 2, width: 0, gap: 34, margin: 0 });
  const [focusedCard, setFocusedCard] = useState<{ id: string; rowKey: Key }>();
  const focusedIndex = focusedCard ? items.findIndex((item) => item.id === focusedCard.id) : -1;
  const focusedRow = focusedIndex >= 0 ? Math.floor(focusedIndex / layout.columns) : undefined;
  const estimatedHeight = layout.width ? layout.width * 1.5 + 90 : 360;
  const getItemKey = useCallback((index: number) => {
    // Preserve the active card's parent when a resize moves it into a new row.
    if (index === focusedRow && focusedCard) return focusedCard.rowKey;
    const key = items[index * layout.columns]?.id ?? index;
    return key === focusedCard?.rowKey ? `unfocused:${key}` : key;
  }, [items, layout.columns, focusedRow, focusedCard]);
  const virtualizer = useWindowVirtualizer<HTMLDivElement>({
    count: Math.ceil(items.length / layout.columns),
    estimateSize: () => estimatedHeight,
    getItemKey,
    overscan: 2,
    gap: layout.gap,
    scrollMargin: layout.margin,
    initialRect: { width: 0, height: typeof window === "undefined" ? 768 : window.innerHeight },
    // Keep keyboard focus and an open card editor mounted while scrolling.
    rangeExtractor: (range) => {
      const rows = defaultRangeExtractor(range);
      if (focusedRow != null && focusedRow < range.count && !rows.includes(focusedRow)) {
        rows.push(focusedRow);
        rows.sort((a, b) => a - b);
      }
      return rows;
    },
    measureElement: (element) => element.getBoundingClientRect().height || estimatedHeight,
  });

  useLayoutEffect(() => {
    const grid = gridRef.current;
    if (!grid) return;
    const update = () => {
      const style = getComputedStyle(grid);
      const tracks = style.gridTemplateColumns.split(/\s+/).filter((track) => /^\d/.test(track));
      const columns = tracks.length || 2;
      const rect = grid.getBoundingClientRect();
      const columnGap = parseFloat(style.columnGap) || 0;
      const next = {
        columns,
        width: Math.max(0, (rect.width - columnGap * (columns - 1)) / columns),
        gap: parseFloat(style.rowGap) || 34,
        margin: rect.top + window.scrollY,
      };
      setLayout((current) => Object.keys(next).every((key) => current[key as keyof typeof next] === next[key as keyof typeof next]) ? current : next);
    };
    update();
    const observer = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(update);
    observer?.observe(grid);
    if (grid.parentElement) observer?.observe(grid.parentElement);
    window.addEventListener("resize", update);
    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", update);
    };
  }, []);

  useLayoutEffect(() => {
    virtualizer.measure();
  }, [virtualizer, layout.columns, layout.width]);

  return (
    <div
      ref={gridRef}
      className="lux-poster-grid lux-library-virtual-grid"
      role="list"
      aria-label="媒体库内容"
      style={{ height: virtualizer.getTotalSize() }}
      onBlurCapture={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setFocusedCard(undefined);
      }}
    >
      {virtualizer.getVirtualItems().map((row) => (
        <div
          key={row.key}
          ref={virtualizer.measureElement}
          data-index={row.index}
          className="lux-library-virtual-row"
          role="presentation"
          // top preserves viewport positioning of the cards' fixed dialogs.
          style={{ top: row.start - layout.margin }}
        >
          {items.slice(row.index * layout.columns, (row.index + 1) * layout.columns).map((item, column) => (
            <div
              key={item.id}
              role="listitem"
              aria-posinset={row.index * layout.columns + column + 1}
              aria-setsize={items.length}
              onFocusCapture={() => setFocusedCard((current) => current?.id === item.id && current.rowKey === row.key ? current : { id: item.id, rowKey: row.key })}
            >
              {renderItem(item)}
            </div>
          ))}
        </div>
      ))}
    </div>
  );
}
