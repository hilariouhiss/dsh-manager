import { describe, expect, it } from "vite-plus/test";
import { confirmLevel, shouldShowInstallConfirm } from "./version-view";

describe("ConfirmDialog 的三档强度", () => {
  it("安装新版本是普通二次确认（SDD §8.3）", () => {
    expect(confirmLevel("install")).toBe("normal");
  });

  it("切换自动安装通道用普通档", () => {
    expect(confirmLevel("switch-channel")).toBe("normal");
  });

  it("停止外部实例用强化档", () => {
    expect(confirmLevel("stop-external")).toBe("strong");
  });

  it("强制终止用「强制」档（必须单独确认，取值 force）", () => {
    expect(confirmLevel("forced-stop")).toBe("force");
  });
});

describe("安装前二次确认可被设置关闭", () => {
  it("默认需要确认", () => {
    expect(shouldShowInstallConfirm(true)).toBe(true);
  });

  it("用户在设置里关掉后不再确认", () => {
    expect(shouldShowInstallConfirm(false)).toBe(false);
  });
});
