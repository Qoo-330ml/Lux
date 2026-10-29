import type { AdminUser, AdminUserContentRestrictionPolicy } from "../../../../lib/api/types";
import { LuxButton, LuxSelectField, LuxSwitch, LuxTextarea } from "../../../../components/ui";
import { contentRatingLevels, useContentRestrictionForm } from "./useContentRestrictionForm";

export function AdminUserContentRestrictionEditor({ user }: { user: AdminUser }) {
  const {
    policyQuery,
    currentPolicy,
    currentDraft,
    setDraft,
    hasChanges,
    notice,
    error,
    versionConflict,
    save,
    submit,
    reloadLatestPolicy,
  } = useContentRestrictionForm(user);

  if (user.deletedAt != null) {
    return (
      <section className="lux-admin-panel lux-admin-user-content-policy" aria-labelledby="lux-admin-user-content-policy-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-content-policy-title">用户内容限制</h2></div></div>
        <p className="lux-admin-muted" role="status">回收站中的账号内容限制为只读，恢复后可修改。</p>
      </section>
    );
  }

  if (policyQuery.isPending) {
    return (
      <section className="lux-admin-panel lux-admin-user-content-policy" aria-busy="true" aria-labelledby="lux-admin-user-content-policy-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-content-policy-title">用户内容限制</h2></div></div>
        <p className="lux-admin-muted" role="status">正在读取用户内容限制…</p>
      </section>
    );
  }

  if (policyQuery.error || !policyQuery.data) {
    return (
      <section className="lux-admin-panel lux-admin-user-content-policy" aria-labelledby="lux-admin-user-content-policy-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-content-policy-title">用户内容限制</h2></div></div>
        <p className="lux-error-copy" role="alert">读取用户内容限制失败：{policyQuery.error?.message || "没有收到策略数据"}</p>
        <LuxButton tone="secondary" type="button" onClick={() => void policyQuery.refetch()}>重试</LuxButton>
      </section>
    );
  }

  return (
    <section className="lux-admin-panel lux-admin-user-content-policy" aria-labelledby="lux-admin-user-content-policy-title">
      <div className="lux-admin-panel-heading">
        <div><h2 id="lux-admin-user-content-policy-title">用户内容限制</h2></div>
        <span className={`lux-user-badge ${currentPolicy ? "is-ok" : ""}`}>{currentPolicy ? "已配置" : "未配置"}</span>
      </div>
      {currentPolicy ? (
        <p className="lux-admin-muted lux-admin-user-content-policy-version">当前版本 {currentPolicy.version}</p>
      ) : (
        <p className="lux-admin-muted lux-admin-user-content-policy-default" role="status">当前没有已保存的限制；默认不限制分级和标签，并允许未分级内容。</p>
      )}
      <form className="lux-admin-form lux-admin-user-content-policy-form" onSubmit={submit}>
        <div className="lux-admin-user-content-policy-grid">
          <label>
            分级体系
            <LuxSelectField
              aria-label="分级体系"
              value={currentDraft.ratingSystem}
              disabled={save.isPending || versionConflict}
              onChange={(event) => setDraft({
                ...currentDraft,
                ratingSystem: event.currentTarget.value as AdminUserContentRestrictionPolicy["ratingSystem"],
                maximumRating: null,
              })}
            >
              <option value="MPA">MPA 电影分级</option>
              <option value="TVPG">TV 家长指引</option>
              <option value="BBFC">BBFC 英国分级</option>
            </LuxSelectField>
          </label>
          <label>
            最高允许等级
            <LuxSelectField
              aria-label="最高允许等级"
              value={currentDraft.maximumRating ?? ""}
              disabled={save.isPending || versionConflict}
              onChange={(event) => setDraft({ ...currentDraft, maximumRating: event.currentTarget.value || null })}
            >
              <option value="">不限制</option>
              {contentRatingLevels[currentDraft.ratingSystem].map((rating) => <option key={rating} value={rating}>{rating}</option>)}
            </LuxSelectField>
          </label>
        </div>
        <LuxSwitch className="lux-admin-user-content-policy-toggle" aria-label="允许未分级内容" checked={currentDraft.allowUnrated} disabled={save.isPending || versionConflict} onChange={(event) => setDraft({ ...currentDraft, allowUnrated: event.currentTarget.checked })} label="允许未分级内容" />
        <div className="lux-admin-user-content-policy-grid">
          <label>
            允许标签（每行一项）
            <LuxTextarea
              aria-label="允许标签"
              rows={4}
              value={currentDraft.allowedTagsText}
              disabled={save.isPending || versionConflict}
              onChange={(event) => setDraft({ ...currentDraft, allowedTagsText: event.currentTarget.value })}
            />
            <span className="lux-admin-muted">填写后，内容至少要命中一个允许标签。</span>
          </label>
          <label>
            禁止标签（每行一项）
            <LuxTextarea
              aria-label="禁止标签"
              rows={4}
              value={currentDraft.blockedTagsText}
              disabled={save.isPending || versionConflict}
              onChange={(event) => setDraft({ ...currentDraft, blockedTagsText: event.currentTarget.value })}
            />
            <span className="lux-admin-muted">禁止标签优先，命中后拒绝访问。</span>
          </label>
        </div>
        <p className="lux-admin-muted">标签按媒体分类索引匹配；每组最多 100 项。已保存的分级和标签限制由服务端用于列表筛选及详情、图片、下载、字幕和播放访问校验；缺少分类索引的条目在受限策略下按不可见处理。</p>
        <div className="lux-admin-user-profile-actions">
          <LuxButton tone="primary" type="submit" disabled={!hasChanges || save.isPending || versionConflict}>
            {save.isPending ? "保存中…" : "保存内容限制"}
          </LuxButton>
          {versionConflict ? <LuxButton tone="secondary" type="button" onClick={reloadLatestPolicy}>重新读取策略</LuxButton> : null}
          {error ? <span role="alert">{error}</span> : null}
          {notice ? <span role="status">{notice}</span> : null}
        </div>
      </form>
    </section>
  );
}
