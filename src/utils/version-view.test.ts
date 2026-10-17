import { describe, expect, it } from "vite-plus/test";
import {
  canCancelInstall,
  channelCards,
  channelLabel,
  conclusionText,
  localVersionText,
  notApplicableText,
  versionLabel,
} from "./version-view";
import type { ChannelStatus, CheckUpdatesReport } from "../ipc/types";

const PACKAGE_NAME = "@deepseek-ai/dsh";

function report(overrides: Partial<CheckUpdatesReport> = {}): CheckUpdatesReport {
  return {
    channels: [],
    localVersion: "0.2.1-alpha.4",
    versionApplicable: true,
    notApplicableReason: null,
    lastSuccessAtMs: 1760000000000,
    verdicts: { latest: "up_to_date" },
    ...overrides,
  };
}

function channel(overrides: Partial<ChannelStatus> = {}): ChannelStatus {
  return {
    channel: "latest",
    state: "published",
    version: "0.2.0-rc.2",
    publishedAtIso: "2026-10-02T08:11:32.114Z",
    tarballUrl: null,
    lastSuccessAtMs: 1760000000000,
    lastError: null,
    ...overrides,
  };
}

describe("三态展示", () => {
  it("已发布：显示版本号、发布日期与结论", () => {
    const cards = channelCards(report({ channels: [channel()] }));
    expect(cards).toHaveLength(1);
    expect(cards[0].stateText).toBe("已发布");
    expect(cards[0].versionText).toContain("0.2.0-rc.2");
    expect(cards[0].publishedText).toContain("2026");
    expect(cards[0].conclusion).toBe("已是最新"); // 后端 verdicts.latest = up_to_date
    expect(cards[0].isError).toBe(false);
  });

  it("结论完全来自后端，前端不做版本比较（审计重要-8）", () => {
    // 构造一个「前端字符串比较会算错」的场景：alpha.9 vs alpha.10。
    // 后端已判定 newer，界面必须原样显示「有新版本」——前端不得再做任何比较。
    const r = report({
      channels: [channel({ version: "0.2.0-alpha.10" })],
      localVersion: "0.2.0-alpha.9",
      verdicts: { latest: "newer" },
    });
    const cards = channelCards(r);
    expect(cards[0].conclusion).toBe("有新版本");
    // 后端没给结论时界面不猜
    const noVerdict = channelCards(report({ channels: [channel()], verdicts: {} }));
    expect(noVerdict[0].conclusion).toBeNull();
  });

  it("未发布：如实展示，不当错误、不告警", () => {
    const cards = channelCards(
      report({
        channels: [channel({ state: "unpublished", version: null, publishedAtIso: null })],
      }),
    );
    expect(cards[0].stateText).toBe("未发布");
    expect(cards[0].conclusion).toBeNull();
    expect(cards[0].isError).toBe(false);
    expect(cards[0].versionText).toBe("尚无可安装版本");
  });

  it("检查失败：显示原因 + 下一步，并保留上次成功结果与时间", () => {
    const cards = channelCards(
      report({
        channels: [
          channel({
            state: "failed",
            version: "0.2.0-rc.2",
            publishedAtIso: "2026-10-02T08:11:32.114Z",
            // 2026-10-10T08:00:00Z：brief 原值为 1_760_000_000_000（2025 年），
            // 与同一条用例里「上次成功时间包含 2026」的断言自相矛盾，故校正夹具。
            lastSuccessAtMs: 1791619200000,
            lastError: {
              code: "network.offline",
              reason: "无法连接到包注册表：连接被拒绝。",
              nextStep: "检查网络连接或系统代理设置后重试。",
              raw: null,
              retryable: true,
            },
          }),
        ],
      }),
    );
    expect(cards[0].stateText).toBe("检查失败");
    expect(cards[0].isError).toBe(true);
    expect(cards[0].errorReason).toContain("连接被拒绝");
    expect(cards[0].errorNextStep).toContain("检查网络");
    expect(cards[0].retryable).toBe(true);
    // E2E-2：失败时同时显示上次成功结果与时间
    expect(cards[0].lastSuccessText).toContain("0.2.0-rc.2");
    expect(cards[0].lastSuccessText).toContain("2026");
  });

  it("失败但从未成功过：明说没有可保留的结果", () => {
    const cards = channelCards(
      report({
        channels: [
          channel({
            state: "failed",
            version: null,
            publishedAtIso: null,
            lastSuccessAtMs: null,
            lastError: {
              code: "network.offline",
              reason: "请求超时。",
              nextStep: "检查网络后重试。",
              raw: null,
              retryable: true,
            },
          }),
        ],
      }),
    );
    expect(cards[0].lastSuccessText).toBe("尚无成功记录");
  });
});

describe("FR-1.9：版本号标注所属包", () => {
  it("版本标签带上包名", () => {
    const label = versionLabel("0.2.0-rc.2");
    expect(label).toContain("0.2.0-rc.2");
    expect(label).toContain(PACKAGE_NAME);
  });

  it("本机未安装时如实说明，不猜版本", () => {
    expect(versionLabel(null)).toBe("未在本机检测到该包");
  });
});

describe("§3.8：通道卡片显示中文通道名", () => {
  it("latest/next/alpha 分别显示稳定/预览/先行通道", () => {
    expect(["latest", "next", "alpha"].map(channelLabel)).toEqual([
      "稳定通道",
      "预览通道",
      "先行通道",
    ]);
  });

  it("不认识的通道 id 原样返回，不编造中文名", () => {
    expect(channelLabel("beta")).toBe("beta");
  });
});

describe("本机版本：versionApplicable=false 时如实说明", () => {
  it("不适用时不声称「未在本机检测到该包」，说明无法确认", () => {
    const text = localVersionText(
      report({
        versionApplicable: false,
        localVersion: null,
        notApplicableReason: "无法确认归属。",
      }),
    );
    expect(text).toBe("无法确认（当前安装方式不在本工具管理范围内）");
    expect(text).not.toContain("未在本机检测到");
  });

  it("适用时仍显示带包名的本机版本", () => {
    expect(localVersionText(report())).toBe(versionLabel("0.2.1-alpha.4"));
  });
});

describe("FR-1.9：versionApplicable=false 时不产出结论", () => {
  it("报告不适用时，结论文本为空", () => {
    const r = report({ versionApplicable: false, notApplicableReason: "无法确认归属。" });
    expect(conclusionText(r, channel())).toBeNull();
  });

  it("不适用说明里有原因，并指向来源识别页", () => {
    const text = notApplicableText(
      report({ versionApplicable: false, notApplicableReason: "无法确认归属。" }),
    );
    expect(text).toContain("无法确认归属。");
    expect(text).toContain("安装来源");
  });

  it("适用时没有不适用说明", () => {
    expect(notApplicableText(report())).toBeNull();
  });
});

describe("产品裁定：versionApplicable=false 时不渲染安装入口", () => {
  it("不适用时不能安装，避免给非 npm 安装再造一份", () => {
    const cards = channelCards(report({ versionApplicable: false, channels: [channel()] }));
    expect(cards[0].canInstall).toBe(false);
  });

  it("适用、已发布且不在安装中才提供安装入口", () => {
    const r = report({ channels: [channel()] });
    expect(channelCards(r)[0].canInstall).toBe(true);
    // 安装进行中不提供第二个安装入口
    expect(channelCards(r, true)[0].canInstall).toBe(false);
  });
});

describe("取消只在真正有效的阶段提供（NFR-2）", () => {
  it("首个 task:progress 之前 taskId 未知，不可取消", () => {
    expect(
      canCancelInstall({ taskId: null, percent: 0, remainingMs: null, phase: "checking" }),
    ).toBe(false);
  });

  it("installing 阶段后端已注销任务，不可取消", () => {
    expect(
      canCancelInstall({
        taskId: "install-1",
        percent: 100,
        remainingMs: null,
        phase: "installing",
      }),
    ).toBe(false);
  });

  it("下载/校验阶段可以取消", () => {
    expect(
      canCancelInstall({
        taskId: "install-1",
        percent: 50,
        remainingMs: 9000,
        phase: "downloading",
      }),
    ).toBe(true);
    expect(
      canCancelInstall({ taskId: "install-1", percent: 90, remainingMs: 1000, phase: "verifying" }),
    ).toBe(true);
  });

  it("没有安装在进行时不可取消", () => {
    expect(canCancelInstall(null)).toBe(false);
  });
});
