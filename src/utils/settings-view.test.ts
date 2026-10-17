import { describe, expect, it } from "vite-plus/test";

import { channelSwitchNeedsConfirm } from "./settings-view";

function state(autoChannel: string | null, autoInstall: boolean) {
  return { autoChannel, autoInstall };
}

describe("channelSwitchNeedsConfirm（FR-1.4：切换自动安装通道先确认）", () => {
  it("已为 latest 开启自动安装时切到 next 需要确认", () => {
    expect(channelSwitchNeedsConfirm(state("latest", true), "next")).toBe(true);
  });

  it("重复选择当前通道不需要确认", () => {
    expect(channelSwitchNeedsConfirm(state("latest", true), "latest")).toBe(false);
  });

  it("首次选择通道（自动安装尚未开启）不需要确认", () => {
    expect(channelSwitchNeedsConfirm(state(null, false), "latest")).toBe(false);
  });

  it("选择「不自动安装」（关闭）不需要确认", () => {
    expect(channelSwitchNeedsConfirm(state("latest", true), null)).toBe(false);
  });

  it("自动安装关闭时改通道不需要确认", () => {
    expect(channelSwitchNeedsConfirm(state("latest", false), "next")).toBe(false);
  });
});
