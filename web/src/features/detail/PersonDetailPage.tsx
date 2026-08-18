import { ArrowLeft, Cake, MapPin, UserRound } from "lucide-react";
import { useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";

export function PersonDetailPage() {
  const { personId = "" } = useParams();
  const navigate = useNavigate();
  const person = useQuery({
    queryKey: queryKeys.person(personId),
    queryFn: () => api.person(personId),
    enabled: Boolean(personId),
  });
  const [imageFailed, setImageFailed] = useState(false);

  if (person.isPending) {
    return <section className="lux-page-state" aria-busy="true"><p>正在加载人物详情…</p></section>;
  }
  if (person.error) {
    return <section className="lux-page-state" role="alert"><h1>人物详情加载失败</h1><p>{person.error.message}</p><button className="lux-button lux-button-secondary" type="button" onClick={() => navigate(-1)}>返回</button></section>;
  }

  const detail = person.data;
  const image = detail.imageUrl && !imageFailed ? (
    <img className="lux-person-detail-photo" src={detail.imageUrl} alt={`${detail.name} 照片`} onError={() => setImageFailed(true)} />
  ) : (
    <div className="lux-person-detail-placeholder" role="img" aria-label={`${detail.name} 暂无照片`}><UserRound size={58} strokeWidth={1.4} /></div>
  );

  return (
    <article className="lux-person-detail-page">
      <button className="lux-person-back" type="button" onClick={() => navigate(-1)}><ArrowLeft size={17} /> 返回</button>
      <div className="lux-person-detail-grid">
        <div className="lux-person-detail-photo-frame">{image}</div>
        <div className="lux-person-detail-copy">
          <p className="lux-eyebrow">人物资料</p>
          <h1>{detail.name}</h1>
          {detail.character ? <p className="lux-person-detail-role">饰演：{detail.character}</p> : null}
          <dl className="lux-person-facts">
            {detail.birthday ? <div><dt><Cake size={15} />出生日期</dt><dd>{detail.birthday}</dd></div> : null}
            {detail.deathday ? <div><dt><Cake size={15} />去世日期</dt><dd>{detail.deathday}</dd></div> : null}
            {detail.knownForDepartment ? <div><dt>职业领域</dt><dd>{detail.knownForDepartment}</dd></div> : null}
            {detail.placeOfBirth ? <div><dt><MapPin size={15} />出生地</dt><dd>{detail.placeOfBirth}</dd></div> : null}
          </dl>
          <section className="lux-person-overview" aria-labelledby="person-overview-heading">
            <h2 id="person-overview-heading">简介</h2>
            <p>{detail.biography || "暂无简介。"}</p>
          </section>
        </div>
      </div>
    </article>
  );
}
