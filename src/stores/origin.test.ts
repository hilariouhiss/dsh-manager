import { beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { createPinia, setActivePinia } from "pinia";

vi.mock("../ipc/client", () => ({
  invokeCmd: vi.fn(),
  isAppError: (value: unknown) =>
    typeof value === "object" &&
    value !== null &&
    typeof (value as { code?: unknown }).code === "string",
}));

import { invokeCmd } from "../ipc/client";
import { missingHint, useOriginStore, verdictDetail, verdictLabel } from "./origin";
import type { AppError, OriginReport } from "../ipc/types";

const npmReport: OriginReport = {
  verdict: { packageManager: "npm" },
  evidences: [
    {
      source: "包管理器全局目录（npm）",
      path: "C:\\Users\\me\\AppData\\Roaming\\npm\\node_modules",
      observed: "该全局目录下有 @deepseek-ai/dsh 0.2.1-alpha.4（由 npm 11.6.2 管理）",
      supports: true,
    },
  ],
  missing: [],
};

const unidentifiedReport: OriginReport = {
  verdict: "unidentified",
  evidences: [],
  missing: ["PATH 中没有可解析的 dsh 启动器"],
};

describe("来源结论文案", () => {
  it("四值各有自己的中文文案", () => {
    expect(verdictLabel({ packageManager: "npm" })).toBe("包管理器安装（npm）");
    expect(verdictLabel({ packageManager: "pnpm" })).toBe("包管理器安装（pnpm）");
    expect(verdictLabel({ packageManager: "bun" })).toBe("包管理器安装（bun）");
    expect(verdictLabel("officialInstaller")).toBe("官方安装器安装");
    expect(verdictLabel("portable")).toBe("便携版");
    expect(verdictLabel("unidentified")).toBe("未识别");
  });

  it("未识别时明确说明不猜测、不产出结论", () => {
    expect(verdictDetail("unidentified")).toContain("依据不足");
    expect(verdictDetail("unidentified")).toContain("不会猜测");
  });

  it("非包管理器安装时说明版本管理不适用（FR-1.9）", () => {
    expect(verdictDetail("officialInstaller")).toContain("不适用");
    expect(verdictDetail("portable")).toContain("不适用");
    expect(verdictDetail({ packageManager: "npm" })).toContain("适用于这份安装");
  });

  it("缺失项为空与不为空给出不同提示", () => {
    expect(missingHint([])).toBe("所有取证来源都可用。");
    expect(missingHint(["A", "B"])).toBe("还缺以下信息：A；B");
  });
});

describe("origin store", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    vi.mocked(invokeCmd).mockReset();
  });

  it("初始状态没有结论", () => {
    const store = useOriginStore();
    expect(store.report).toBeNull();
    expect(store.verdictText).toBe("尚未识别");
  });

  it("load 成功后填充结论与依据", async () => {
    vi.mocked(invokeCmd).mockResolvedValue(npmReport);
    const store = useOriginStore();
    await store.load();

    expect(invokeCmd).toHaveBeenCalledWith("detect_origin", {});
    expect(store.verdictText).toBe("包管理器安装（npm）");
    expect(store.report?.evidences).toHaveLength(1);
    expect(store.showMissing).toBe(false);
    expect(store.loading).toBe(false);
  });

  it("load 失败时保留错误且不编造结论", async () => {
    const err: AppError = {
      code: "path.permission_denied",
      reason: "目录不可读",
      nextStep: "检查权限",
      raw: null,
      retryable: false,
    };
    vi.mocked(invokeCmd).mockRejectedValue(err);
    const store = useOriginStore();
    await store.load();

    expect(store.error).toEqual(err);
    expect(store.report).toBeNull();
    expect(store.verdictText).toBe("尚未识别");
    expect(store.loading).toBe(false);
  });

  it("缺失项非空时提示可见，未识别且零依据也如实保留", async () => {
    vi.mocked(invokeCmd).mockResolvedValue(unidentifiedReport);
    const store = useOriginStore();
    await store.load();

    expect(store.verdictText).toBe("未识别");
    expect(store.report?.evidences).toEqual([]);
    expect(store.showMissing).toBe(true);
    expect(store.missingText).toContain("PATH 中没有可解析的 dsh 启动器");
  });

  it("依据清单默认收起，可展开也可再收起", () => {
    const store = useOriginStore();
    expect(store.expanded).toBe(false);
    store.toggleExpanded();
    expect(store.expanded).toBe(true);
    store.toggleExpanded();
    expect(store.expanded).toBe(false);
  });
});
