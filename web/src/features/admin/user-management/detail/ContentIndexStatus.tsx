import { useQuery } from "@tanstack/react-query";
import { api } from "../../../../lib/api/client";
import { LuxButton, LuxProgress } from "../../../../components/ui";

export function AdminUserContentIndexStatus() {
  const readiness = useQuery({
    queryKey: ["admin", "content-restriction-index"],
    queryFn: () => api.adminContentRestrictionIndexStatus(),
    refetchInterval: (query) => query.state.data?.pendingItems ? 5_000 : false,
  });

  if (readiness.isPending) {
    return (
      <section className="lux-admin-panel lux-admin-user-content-index" aria-busy="true" aria-labelledby="lux-admin-user-content-index-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-content-index-title">内容分类索引</h2></div></div>
        <p className="lux-admin-muted" role="status">正在读取内容索引进度…</p>
      </section>
    );
  }

  if (readiness.error) {
    return (
      <section className="lux-admin-panel lux-admin-user-content-index" aria-labelledby="lux-admin-user-content-index-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-content-index-title">内容分类索引</h2></div></div>
        <p className="lux-error-copy" role="alert">读取索引进度失败：{readiness.error.message}</p>
        <LuxButton tone="secondary" type="button" onClick={() => void readiness.refetch()}>重试</LuxButton>
      </section>
    );
  }

  const { totalItems, indexedItems, pendingItems } = readiness.data;
  const hasNoItems = totalItems === 0;
  const isReady = !hasNoItems && pendingItems === 0;
  const statusLabel = hasNoItems ? "暂无媒体" : isReady ? "索引已就绪" : "正在回填";

  return (
    <section className="lux-admin-panel lux-admin-user-content-index" aria-labelledby="lux-admin-user-content-index-title">
      <div className="lux-admin-panel-heading">
        <div><h2 id="lux-admin-user-content-index-title">内容分类索引</h2></div>
        <span className={`lux-user-badge ${isReady ? "is-ok" : hasNoItems ? "" : "is-warn"}`}>{statusLabel}</span>
      </div>
      {hasNoItems ? (
        <p className="lux-admin-muted" role="status">目前没有可统计的媒体条目。</p>
      ) : (
        <div className="lux-admin-user-content-index-progress" aria-live="polite">
          <div><span>已就绪</span><strong>{indexedItems.toLocaleString()} / {totalItems.toLocaleString()}</strong></div>
          <LuxProgress label="已索引条目" max={totalItems} value={Math.min(indexedItems, totalItems)} />
          <p className="lux-admin-muted">待就绪 {pendingItems.toLocaleString()} 条。索引会在后台分批更新。</p>
        </div>
      )}
      <p className="lux-admin-muted lux-admin-user-content-index-note">此面板只展示媒体分类索引回填进度；尚无分类索引的条目会在受限策略下被服务端按不可见处理。</p>
    </section>
  );
}
