import { beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { createPinia, setActivePinia } from "pinia";

const { invokeCmd } = vi.hoisted(() => ({ invokeCmd: vi.fn() }));
vi.mock("../ipc/client", () => ({ invokeCmd }));

import {
  CHANNEL_OPTIONS,
  SETTINGS_GROUPS,
  SETTINGS_KEYS,
  useSettingsStore,
  validateNumeric,
} from "./settings";
import type { Settings } from "../ipc/types";

describe("设置分组（§3.7 全表，一个都不能漏）", () => {
  it("覆盖 14 个设置键，不重不漏", () => {
    const fromGroups = SETTINGS_GROUPS.flatMap((group) =>
      group.items.map((item) => item.key),
    ).sort();
    expect(fromGroups).toEqual([...SETTINGS_KEYS].sort());
    expect(fromGroups).toHaveLength(14);
  });

  it("分组顺序固定为 启动 / 显示 / 日志 / 更新 / 开机自启", () => {
    expect(SETTINGS_GROUPS.map((group) => group.id)).toEqual([
      "startup",
      "display",
      "logs",
      "update",
      "autostart",
    ]);
  });

  it("每个设置项都标注了它影响的需求编号", () => {
    for (const group of SETTINGS_GROUPS) {
      for (const item of group.items) {
        expect(item.requirement.length, item.key + " 缺少需求标注").toBeGreaterThan(0);
      }
    }
  });

  it("通道下拉框与 §3.8 D-1 的冻结表一致", () => {
    expect(CHANNEL_OPTIONS.map((channel) => channel.id)).toEqual(["latest", "next", "alpha"]);
    expect(CHANNEL_OPTIONS.map((channel) => channel.label)).toEqual([
      "稳定通道",
      "预览通道",
      "先行通道",
    ]);
  });
});

describe("数值项校验（与后端 §3.7 同一套区间）", () => {
  it("输出缓冲行数：1 与 100000 合法，0 与 100001 越界", () => {
    expect(validateNumeric("outputBufferLines", 1).ok).toBe(true);
    expect(validateNumeric("outputBufferLines", 100000).ok).toBe(true);
    expect(validateNumeric("outputBufferLines", 0).ok).toBe(false);
    expect(validateNumeric("outputBufferLines", 100001).ok).toBe(false);
  });

  it("MiB 项：1 与 1024 合法，0 与 1025 越界", () => {
    expect(validateNumeric("logMaxMiB", 1).ok).toBe(true);
    expect(validateNumeric("logMaxMiB", 1024).ok).toBe(true);
    expect(validateNumeric("logMaxMiB", 0).ok).toBe(false);
    expect(validateNumeric("logMaxMiB", 1025).ok).toBe(false);
    expect(validateNumeric("oplogMaxMiB", 1025).ok).toBe(false);
  });

  it("保留份数：1 与 50 合法，0 与 51 越界", () => {
    expect(validateNumeric("logKeepFiles", 1).ok).toBe(true);
    expect(validateNumeric("logKeepFiles", 50).ok).toBe(true);
    expect(validateNumeric("logKeepFiles", 0).ok).toBe(false);
    expect(validateNumeric("logKeepFiles", 51).ok).toBe(false);
    expect(validateNumeric("oplogKeepFiles", 51).ok).toBe(false);
  });

  it("非整数被拒绝", () => {
    expect(validateNumeric("logKeepFiles", 2.5).ok).toBe(false);
    expect(validateNumeric("outputBufferLines", Number.NaN).ok).toBe(false);
  });

  it("越界提示是「原因 + 下一步」", () => {
    const result = validateNumeric("logKeepFiles", 51);
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.message).toContain("1–50 份");
      expect(result.message).toContain("请改成");
    }
  });
});
/** §3.7 的默认值。字段顺序与 SETTINGS_KEYS 一致，值必须逐字一致。 */
const DEFAULT_SETTINGS: Settings = {
  autoChannel: null,
  autoInstall: false,
  confirmBeforeInstall: true,
  launchArgs: ["--no-open"],
  workDir: "C:/Users/example",
  autoOpenBrowser: true,
  autoScroll: true,
  outputBufferLines: 5000,
  logMaxMiB: 5,
  logKeepFiles: 3,
  oplogMaxMiB: 2,
  oplogKeepFiles: 3,
  stopInstanceOnExit: true,
  autostartMode: "tray",
};

describe("settings store", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    invokeCmd.mockReset();
  });

  it("load 同时取设置与系统自启状态", async () => {
    invokeCmd.mockImplementation((name: string) => {
      if (name === "get_settings") return Promise.resolve({ ...DEFAULT_SETTINGS });
      if (name === "get_autostart_state") {
        return Promise.resolve({ enabled: false, mode: "tray", entryPath: null });
      }
      return Promise.reject(new Error("未预期的命令：" + name));
    });
    const store = useSettingsStore();
    await store.load();
    expect(invokeCmd).toHaveBeenCalledWith("get_settings", {});
    expect(invokeCmd).toHaveBeenCalledWith("get_autostart_state", {});
    expect(store.settings?.outputBufferLines).toBe(5000);
    expect(store.autostart?.enabled).toBe(false);
  });

  it("自启开关读的是系统状态，不是设置里的 autostartMode", async () => {
    invokeCmd.mockImplementation((name: string) => {
      if (name === "get_settings")
        return Promise.resolve({ ...DEFAULT_SETTINGS, autostartMode: "window" });
      if (name === "get_autostart_state") {
        return Promise.resolve({ enabled: false, mode: "window", entryPath: null });
      }
      return Promise.reject(new Error("未预期的命令：" + name));
    });
    const store = useSettingsStore();
    await store.load();
    expect(store.autostart?.enabled, "设置里选了 window 不代表系统里已开启").toBe(false);
    expect(store.autostart?.entryPath).toBeNull();
  });

  it("补丁只含被改字段", async () => {
    const store = useSettingsStore();
    store.settings = { ...DEFAULT_SETTINGS };
    invokeCmd.mockResolvedValue({ ...DEFAULT_SETTINGS, autoScroll: false });
    await store.update({ autoScroll: false });
    expect(invokeCmd).toHaveBeenCalledTimes(1);
    expect(invokeCmd).toHaveBeenCalledWith("update_settings", { patch: { autoScroll: false } });
    expect(store.settings?.autoScroll).toBe(false);
  });

  it("数值越界只提示、不发请求，界面仍显示后端值", async () => {
    const store = useSettingsStore();
    store.settings = { ...DEFAULT_SETTINGS };
    const accepted = await store.setNumber("logKeepFiles", 51);
    expect(accepted).toBe(false);
    expect(invokeCmd).not.toHaveBeenCalled();
    expect(store.notSavedHint).toContain("1–50 份");
    expect(store.settings?.logKeepFiles).toBe(3);
  });

  it("数值合法时发一个只含该字段的补丁", async () => {
    const store = useSettingsStore();
    store.settings = { ...DEFAULT_SETTINGS };
    invokeCmd.mockResolvedValue({ ...DEFAULT_SETTINGS, outputBufferLines: 8000 });
    const accepted = await store.setNumber("outputBufferLines", 8000);
    expect(accepted).toBe(true);
    expect(invokeCmd).toHaveBeenCalledWith("update_settings", {
      patch: { outputBufferLines: 8000 },
    });
  });

  it("自启开关走命令 14，并用返回的系统状态刷新界面", async () => {
    const store = useSettingsStore();
    store.settings = { ...DEFAULT_SETTINGS };
    store.autostart = { enabled: false, mode: "tray", entryPath: null };
    invokeCmd.mockImplementation((name: string) => {
      if (name === "set_autostart") {
        return Promise.resolve({ enabled: true, mode: "tray", entryPath: "HKCU/.../dsh-manager" });
      }
      if (name === "get_settings") return Promise.resolve({ ...DEFAULT_SETTINGS });
      return Promise.reject(new Error("未预期的命令：" + name));
    });
    await store.setAutostartEnabled(true);
    expect(invokeCmd).toHaveBeenCalledWith("set_autostart", { enabled: true, mode: "tray" });
    expect(store.autostart?.enabled).toBe(true);
  });

  it("切启动行为不会把自启打开或关掉", async () => {
    const store = useSettingsStore();
    store.settings = { ...DEFAULT_SETTINGS };
    store.autostart = { enabled: false, mode: "tray", entryPath: null };
    invokeCmd.mockImplementation((name: string) => {
      if (name === "set_autostart") {
        return Promise.resolve({ enabled: false, mode: "window", entryPath: null });
      }
      if (name === "get_settings") {
        return Promise.resolve({ ...DEFAULT_SETTINGS, autostartMode: "window" });
      }
      return Promise.reject(new Error("未预期的命令：" + name));
    });
    await store.setAutostartMode("window");
    expect(invokeCmd).toHaveBeenCalledWith("set_autostart", { enabled: false, mode: "window" });
    expect(store.autostart?.enabled).toBe(false);
  });

  it("setValue 把 autostartMode 路由到命令 14，而不是 update_settings", async () => {
    const store = useSettingsStore();
    store.settings = { ...DEFAULT_SETTINGS };
    store.autostart = { enabled: true, mode: "tray", entryPath: "HKCU/.../dsh-manager" };
    invokeCmd.mockImplementation((name: string) => {
      if (name === "set_autostart") {
        return Promise.resolve({
          enabled: true,
          mode: "window",
          entryPath: "HKCU/.../dsh-manager",
        });
      }
      if (name === "get_settings") {
        return Promise.resolve({ ...DEFAULT_SETTINGS, autostartMode: "window" });
      }
      return Promise.reject(new Error("未预期的命令：" + name));
    });
    await store.setValue("autostartMode", "window");
    expect(invokeCmd).toHaveBeenCalledWith("set_autostart", { enabled: true, mode: "window" });
    expect(invokeCmd).not.toHaveBeenCalledWith("update_settings", expect.anything());
  });

  it("导出诊断调用 export_diagnostics 并回传字节数", async () => {
    const store = useSettingsStore();
    const target = "C:/Users/example/Desktop/dsh-manager-diagnostics.json";
    invokeCmd.mockResolvedValue({ bytesWritten: 2048 });
    const bytes = await store.exportDiagnostics(target);
    expect(invokeCmd).toHaveBeenCalledWith("export_diagnostics", { targetPath: target });
    expect(bytes).toBe(2048);
  });

  it("操作记录分页调用 list_oplog", async () => {
    const store = useSettingsStore();
    invokeCmd.mockResolvedValue({
      entries: [
        {
          timestampMs: 1760000000000,
          action: "settings.update",
          target: "autoScroll",
          result: "ok",
          code: null,
          raw: null,
        },
      ],
      total: 1,
    });
    await store.loadOplog(0, 50);
    expect(invokeCmd).toHaveBeenCalledWith("list_oplog", { offset: 0, limit: 50 });
    expect(store.oplogTotal).toBe(1);
    expect(store.oplogEntries).toHaveLength(1);
  });

  it("命令失败时保留 AppError，不把失败当成成功", async () => {
    const store = useSettingsStore();
    store.settings = { ...DEFAULT_SETTINGS };
    invokeCmd.mockRejectedValue({
      code: "settings.corrupt",
      reason: "设置文件无法解析，已回落到默认值。",
      nextStep: "请在设置页重新确认各项后保存。",
      raw: "expected value at line 1 column 1",
      retryable: false,
    });
    await store.update({ autoScroll: false });
    expect(store.error?.code).toBe("settings.corrupt");
    expect(store.settings?.autoScroll, "失败时不得改写本地状态").toBe(true);
  });

  it("后端回落时不静默：提示哪些项没按输入生效", async () => {
    const store = useSettingsStore();
    store.settings = { ...DEFAULT_SETTINGS };
    // 前端放行（在区间内），后端仍回落成 5000：界面必须显示生效值并说明原因
    invokeCmd.mockResolvedValue({ ...DEFAULT_SETTINGS, outputBufferLines: 5000 });
    await store.update({ outputBufferLines: 8000 });
    expect(store.settings?.outputBufferLines).toBe(5000);
    expect(store.notSavedHint).toContain("outputBufferLines");
    expect(store.notSavedHint).toContain("重新填写");
  });

  it("补丁全部生效时不提示", async () => {
    const store = useSettingsStore();
    store.settings = { ...DEFAULT_SETTINGS };
    invokeCmd.mockResolvedValue({ ...DEFAULT_SETTINGS, autoScroll: false });
    await store.update({ autoScroll: false });
    expect(store.notSavedHint).toBeNull();
  });

  it("启动行为未按所选生效时给出提示（命令 14 的出参里没有 issues 字段）", async () => {
    const store = useSettingsStore();
    store.settings = { ...DEFAULT_SETTINGS };
    store.autostart = { enabled: false, mode: "tray", entryPath: null };
    invokeCmd.mockImplementation((name: string) => {
      if (name === "set_autostart") {
        return Promise.resolve({ enabled: true, mode: "tray", entryPath: "HKCU/.../dsh-manager" });
      }
      if (name === "get_settings") return Promise.resolve({ ...DEFAULT_SETTINGS });
      return Promise.reject(new Error("未预期的命令：" + name));
    });
    await store.setAutostartMode("window");
    expect(store.autostart?.mode).toBe("tray");
    expect(store.notSavedHint).toContain("最小化到托盘");
  });

  it("set_autostart 失败后重读系统真实状态，不把失败当成已关闭", async () => {
    const store = useSettingsStore();
    store.settings = { ...DEFAULT_SETTINGS };
    store.autostart = { enabled: true, mode: "tray", entryPath: "HKCU/.../dsh-manager" };
    invokeCmd.mockImplementation((name: string) => {
      if (name === "set_autostart") {
        return Promise.reject({
          code: "autostart.denied",
          reason: "写入登录项被系统拒绝。",
          nextStep: "确认当前账户可以写入 HKCU 的 Run 项，然后重试。",
          raw: "access denied",
          retryable: false,
        });
      }
      if (name === "get_autostart_state") {
        return Promise.resolve({ enabled: true, mode: "tray", entryPath: "HKCU/.../dsh-manager" });
      }
      return Promise.reject(new Error("未预期的命令：" + name));
    });
    await store.setAutostartEnabled(false);
    expect(store.error?.code).toBe("autostart.denied");
    expect(invokeCmd).toHaveBeenCalledWith("get_autostart_state", {});
    expect(store.autostart?.enabled, "系统里仍是开启，不得显示成关闭").toBe(true);
  });

  it("系统自启状态读不到时，开关沿用设置里的启动行为，不回落到托盘", async () => {
    const store = useSettingsStore();
    store.settings = { ...DEFAULT_SETTINGS, autostartMode: "window" };
    store.autostart = null;
    invokeCmd.mockImplementation((name: string) => {
      if (name === "set_autostart") {
        return Promise.resolve({
          enabled: true,
          mode: "window",
          entryPath: "HKCU/.../dsh-manager",
        });
      }
      if (name === "get_settings") {
        return Promise.resolve({ ...DEFAULT_SETTINGS, autostartMode: "window" });
      }
      return Promise.reject(new Error("未预期的命令：" + name));
    });
    await store.setAutostartEnabled(true);
    expect(invokeCmd).toHaveBeenCalledWith("set_autostart", { enabled: true, mode: "window" });
  });
});
