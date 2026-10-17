import { defineComponent, h } from "vue";
import {
  createRouter,
  createWebHashHistory,
  type RouteRecordRaw,
  type Router,
  type RouterHistory,
} from "vue-router";

/** 骨架占位视图：T-S4 用四个真实视图替换它。 */
const ShellPlaceholder = defineComponent({
  name: "ShellPlaceholder",
  render: () => h("p", { class: "p-6" }, "DSH Manager 骨架已就绪"),
});

export const routes: RouteRecordRaw[] = [{ path: "/", name: "shell", component: ShellPlaceholder }];

/**
 * 工厂函数：生产用哈希历史，测试注入 createMemoryHistory（设计文档 §6.3）。
 * 本模块不在导入时创建 router，测试才能在无 DOM 的 Node 环境里导入 routes。
 */
export function createAppRouter(history: RouterHistory = createWebHashHistory()): Router {
  return createRouter({ history, routes });
}
