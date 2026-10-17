import { describe, expect, it } from "vite-plus/test";
import { navItems, routes } from "../router";

describe("导航项由路由表生成", () => {
  it("导航项数量等于带 meta.title 的路由数，且为 4", () => {
    const withTitle = routes.filter((route) => typeof route.meta?.title === "string");
    expect(navItems.length).toBe(withTitle.length);
    expect(navItems.length).toBe(4);
  });

  it("顺序、路径与中文标题都来自路由表", () => {
    expect(navItems.map((item) => item.path)).toEqual([
      "/version",
      "/origin",
      "/instance",
      "/settings",
    ]);
    expect(navItems.map((item) => item.title)).toEqual([
      "版本与更新",
      "安装来源",
      "运行实例",
      "设置",
    ]);
  });

  it("每个导航项都带图标组件", () => {
    for (const item of navItems) {
      expect(item.icon, item.path).toBeTruthy();
    }
  });
});
