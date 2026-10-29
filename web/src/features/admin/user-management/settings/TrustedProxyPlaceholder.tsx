import { LuxButton } from "../../../../components/ui";

export function TrustedProxyPlaceholder() {
  return (
    <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-user-proxy-title">
      <div className="lux-admin-panel-heading">
        <div><h2 id="lux-admin-user-proxy-title">可信代理</h2></div>
        <span className="lux-user-badge is-warn">尚未开放</span>
      </div>
      <p className="lux-admin-muted">只有来自可信代理的连接才会采用 X-Forwarded-For 判断来源 IP。Docker 部署会自动信任容器运行时的默认网关，无需手工填写；此处用于补充额外的前置反向代理地址或网段，界面开放前可经管理员设置接口配置。</p>
      <div className="lux-admin-user-profile-actions">
        <LuxButton tone="secondary" type="button" disabled>保存可信代理</LuxButton>
      </div>
    </section>
  );
}
