import { Link } from "react-router-dom";
import { luxButtonClass } from "../../../components/ui";

export function AdminUserManagementState({ label, error = false }: { label: string; error?: boolean }) {
  return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><h1>{error ? "无法打开用户详情" : "正在加载用户详情"}</h1><p>{label}</p><Link className={luxButtonClass({ tone: "secondary" })} to="/admin/users">返回用户列表</Link></section>;
}
