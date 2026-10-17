import type { Component } from "vue";
import {
  createRouter,
  createWebHashHistory,
  type RouteRecordRaw,
  type RouteRecordSingleView,
  type Router,
  type RouterHistory,
} from "vue-router";
import { MonitorPlay, PackageSearch, RefreshCw, Settings } from "@lucide/vue";

/** 四个视图平级，路径/路由名照抄设计文档 §6.3 的路由表。 */
const viewRoutes: RouteRecordSingleView[] = [
  {
    path: "/version",
    name: "version",
    component: () => import("../views/VersionView.vue"),
    meta: { title: "版本与更新", icon: RefreshCw },
  },
  {
    path: "/origin",
    name: "origin",
    component: () => import("../views/OriginView.vue"),
    meta: { title: "安装来源", icon: PackageSearch },
  },
  {
    path: "/instance",
    name: "instance",
    component: () => import("../views/InstanceView.vue"),
    meta: { title: "运行实例", icon: MonitorPlay },
  },
  {
    path: "/settings",
    name: "settings",
    component: () => import("../views/SettingsView.vue"),
    meta: { title: "设置", icon: Settings },
  },
];

/** 导航项：路径、标题、图标三者同源于路由表（设计文档 §6.3「导航栏与路由同源」）。 */
export interface NavItem {
  path: string;
  title: string;
  icon: Component;
}

export const routes: RouteRecordRaw[] = [
  { path: "/", redirect: { name: "version" } },
  ...viewRoutes,
  { path: "/:pathMatch(.*)*", redirect: { name: "version" } },
];

/** 导航项由路由表生成，不另维护一份标签清单。 */
export const navItems: NavItem[] = viewRoutes.flatMap((route) => {
  const title = route.meta?.title;
  const icon = route.meta?.icon;
  if (typeof title !== "string" || icon === undefined) {
    return [];
  }
  return [{ path: route.path, title, icon: icon as Component }];
});

/**
 * 工厂函数：生产用哈希历史（桌面应用无服务端处理路径回退），测试注入 createMemoryHistory。
 * 本模块不在导入时创建 router，测试才能在无 DOM 的 Node 环境里导入 routes。
 */
export function createAppRouter(history: RouterHistory = createWebHashHistory()): Router {
  return createRouter({ history, routes });
}
