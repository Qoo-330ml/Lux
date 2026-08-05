import type { MediaActor } from "../../lib/api/types";
import { HorizontalScrollRail } from "../../components/layout/HorizontalScrollRail";

export function MediaCast({ actors }: { actors: MediaActor[] }) {
  if (actors.length === 0) return null;

  return (
    <section className="lux-media-cast" aria-labelledby="media-cast-heading">
      <div className="lux-media-cast-heading">
        <h2 id="media-cast-heading">演职人员</h2>
        <span>{actors.length} 位</span>
      </div>
      <HorizontalScrollRail className="lux-media-cast-rail" ariaLabel="演员列表">
        <ul className="lux-media-cast-list" role="list">
          {actors.map((actor) => (
            <li className="lux-media-cast-card" key={actor.id}>
              <div className="lux-media-cast-avatar">
                {actor.imageUrl ? (
                  <img src={actor.imageUrl} alt={`${actor.name} 头像`} loading="lazy" />
                ) : (
                  <span className="lux-media-cast-initial" aria-hidden="true">{actorInitial(actor.name)}</span>
                )}
              </div>
              <strong title={actor.name}>{actor.name}</strong>
              {actor.character ? <span title={actor.character}>饰 {actor.character}</span> : null}
            </li>
          ))}
        </ul>
      </HorizontalScrollRail>
    </section>
  );
}

function actorInitial(name: string) {
  return [...name.trim()][0] ?? "?";
}
