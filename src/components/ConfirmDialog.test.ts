import { describe, expect, it } from "vite-plus/test";
import { createSSRApp } from "vue";
import { renderToString } from "vue/server-renderer";
import ConfirmDialog from "./ConfirmDialog.vue";

function render(overrides: Record<string, unknown> = {}): Promise<string> {
  return renderToString(
    createSSRApp(ConfirmDialog, {
      open: true,
      title: "安装新版本",
      message: "将安装 0.2.0-rc.2（@deepseek-ai/dsh 包）。",
      strength: "normal",
      ...overrides,
    }),
  );
}

describe("ConfirmDialog 的冻结契约（§3.3.2）", () => {
  it("open=false 时不渲染对话框", async () => {
    const html = await render({ open: false });
    expect(html).not.toContain("安装新版本");
    expect(html).not.toContain("dialog");
  });

  it("普通档：标题、消息、取消与确认按钮都在", async () => {
    const html = await render();
    expect(html).toContain("安装新版本");
    expect(html).toContain("将安装 0.2.0-rc.2");
    expect(html).toContain("取消");
    expect(html).toContain("确认");
    expect(html).toContain('role="dialog"');
  });

  it("强制档：只能单独确认，不提供「取消」离开", async () => {
    const html = await render({ strength: "force" });
    expect(html).toContain("强制终止");
    expect(html).not.toContain("取消");
  });
});
