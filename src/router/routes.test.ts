import { describe, expect, it } from "vite-plus/test";
import { createMemoryHistory, type RouteRecordRaw } from "vue-router";
import { createAppRouter, routes } from "./index";

const VIEW_PATHS = ["/version", "/origin", "/instance", "/settings"];
const VIEW_NAMES = ["version", "origin", "instance", "settings"];

/** 视图路由带上 component/meta 的可读视图（RouteRecordRaw 是联合类型，需要收窄）。 */
type ViewRoute = RouteRecordRaw & { component?: unknown; meta?: Record<string, unknown> };

function viewRoutes(): ViewRoute[] {
  return routes.filter((route) => VIEW_PATHS.includes(route.path)) as ViewRoute[];
}

describe("路由表（设计文档 §6.3）", () => {
  it("四条视图路由，路径与路由名一一对应", () => {
    expect(viewRoutes().map((route) => route.path)).toEqual(VIEW_PATHS);
    expect(viewRoutes().map((route) => route.name)).toEqual(VIEW_NAMES);
  });

  it("视图全部懒加载（动态导入，各自分包）", () => {
    for (const route of viewRoutes()) {
      expect(typeof route.component, route.path).toBe("function");
    }
  });

  it("每个视图的 meta 带中文标题与图标组件", () => {
    for (const route of viewRoutes()) {
      const title = route.meta?.title;
      expect(typeof title, route.path).toBe("string");
      expect((title as string).length).toBeGreaterThan(0);
      expect(route.meta?.icon, route.path).toBeTruthy();
    }
  });

  it("默认重定向：/ 落到 /version", async () => {
    const router = createAppRouter(createMemoryHistory());
    await router.push("/");
    expect(router.currentRoute.value.path).toBe("/version");
    expect(router.currentRoute.value.name).toBe("version");
  });

  it("未匹配路径回落到 /version（不建 404 页）", async () => {
    const router = createAppRouter(createMemoryHistory());
    await router.push("/not-a-real-page");
    expect(router.currentRoute.value.path).toBe("/version");
  });
});
