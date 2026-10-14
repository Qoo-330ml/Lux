import type { ItemImage } from "../../lib/api/types";
import { HorizontalScrollRail } from "../../components/layout/HorizontalScrollRail";

const stillImageTypeOrder: Record<string, number> = {
  FANART: 0,
  THUMB: 1,
  WALLPAPER: 2,
};

export function MediaStills({ images, title }: { images: ItemImage[]; title: string }) {
  const stills = images
    .filter((image) => Object.hasOwn(stillImageTypeOrder, image.imageType.trim().toUpperCase()))
    .sort((left, right) => {
      const typeOrder = stillImageTypeOrder[left.imageType.trim().toUpperCase()]
        - stillImageTypeOrder[right.imageType.trim().toUpperCase()];
      if (typeOrder !== 0) return typeOrder;
      if (left.imageIndex !== right.imageIndex) return left.imageIndex - right.imageIndex;
      return left.id === right.id ? 0 : left.id < right.id ? -1 : 1;
    });

  if (stills.length === 0) return null;

  return (
    <section className="lux-detail-stills" aria-labelledby="media-stills-heading">
      <div className="lux-media-stills-heading">
        <h2 id="media-stills-heading">剧照</h2>
        <span>{stills.length} 张</span>
      </div>
      <HorizontalScrollRail className="lux-media-stills-rail" ariaLabel="剧照列表">
        <ul className="lux-media-stills-list" role="list">
          {stills.map((image, index) => (
            <li className="lux-media-stills-card" key={image.id}>
              <img
                src={image.url}
                alt={`${title} 剧照 ${index + 1}`}
                loading="lazy"
                decoding="async"
              />
            </li>
          ))}
        </ul>
      </HorizontalScrollRail>
    </section>
  );
}
