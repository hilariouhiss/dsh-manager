import { describe, expect, it } from "vite-plus/test";
import { createSSRApp } from "vue";
import { renderToString } from "vue/server-renderer";
import TaskProgress from "./TaskProgress.vue";

function render(overrides: Record<string, unknown> = {}): Promise<string> {
  return renderToString(
    createSSRApp(TaskProgress, {
      taskId: "install-test",
      percent: 50,
      remainingMs: 90_000,
      phase: "downloading",
      ...overrides,
    }),
  );
}

describe("TaskProgress", () => {
  it("按阶段显示中文说明并展示百分比", async () => {
    const downloading = await render();
    expect(downloading).toContain("正在下载安装包");
    expect(downloading).toContain("50%");

    expect(await render({ phase: "checking" })).toContain("正在检查磁盘空间");
    expect(await render({ phase: "verifying" })).toContain("正在校验安装包完整性");
    expect(await render({ phase: "installing" })).toContain("正在安装");
  });

  it("剩余时间按毫秒估算，无法估算时如实说明", async () => {
    expect(await render({ remainingMs: 90_000 })).toContain("约 1 分 30 秒");
    expect(await render({ remainingMs: 5_000 })).toContain("约 5 秒");
    expect(await render({ remainingMs: null })).toContain("无法估算");
  });

  it("可取消时有取消按钮，cancellable 为假时没有", async () => {
    expect(await render()).toContain("取消");
    expect(await render({ cancellable: false })).not.toContain("取消");
  });
});
