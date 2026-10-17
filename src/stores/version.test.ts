import { beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { createPinia, setActivePinia } from "pinia";

/**
 * vi.mock 会被提升到 import 之前执行，因此 mock 工厂里只能引用 vi.hoisted 的值，
 * 否则会在模块求值阶段读到 TDZ 里的 invokeCmd（与 client.test.ts 的既有处置一致）。
 */
const mocks = vi.hoisted(() => {
  const handlers = new Map<string, (payload: unknown) => void>();
  const invokeCmd = vi.fn();
  const offs: Array<ReturnType<typeof vi.fn>> = [];
  const on = vi.fn((event: string, handler: (payload: unknown) => void) => {
    handlers.set(event, handler);
    const off = vi.fn();
    offs.push(off);
    return Promise.resolve(off);
  });
  return { handlers, invokeCmd, on, offs };
});

vi.mock("../ipc/client", () => ({
  invokeCmd: (name: string, args: Record<string, unknown>) => mocks.invokeCmd(name, args),
  on: (event: string, handler: (payload: unknown) => void) => mocks.on(event, handler),
  isAppError: (value: unknown) =>
    typeof value === "object" &&
    value !== null &&
    typeof (value as { code?: unknown }).code === "string",
}));

import { useVersionStore } from "./version";
import { ERROR_CODES } from "../ipc/types";

const { handlers, invokeCmd, on, offs } = mocks;

function settingsWith(channel: string | null) {
  return {
    autoChannel: channel,
    autoInstall: channel !== null,
    confirmBeforeInstall: true,
  };
}

beforeEach(() => {
  setActivePinia(createPinia());
  invokeCmd.mockReset();
  on.mockClear();
  offs.length = 0;
});

describe("version store 的通道切换确认", () => {
  it("切换到另一个通道时先弹确认，不直接调用后端", () => {
    const store = useVersionStore();
    store.settings = settingsWith("latest");
    const pending = store.requestChannelToggle("alpha");
    expect(pending).toEqual({ needsConfirm: true, channel: "alpha" });
    expect(invokeCmd).not.toHaveBeenCalled();
  });

  it("用户确认后才写设置，且写入的是新通道", async () => {
    const store = useVersionStore();
    store.settings = settingsWith("latest");
    invokeCmd.mockResolvedValue({
      autoChannel: "alpha",
      autoInstall: true,
      confirmBeforeInstall: true,
    });
    store.requestChannelToggle("alpha");
    await store.confirmChannelToggle("alpha");
    expect(invokeCmd).toHaveBeenCalledWith("set_auto_channel", {
      channel: "alpha",
      enabled: true,
    });
    expect(store.settings.autoChannel).toBe("alpha");
    expect(store.settings.autoInstall).toBe(true);
  });

  it("同一通道重复开启不弹确认", () => {
    const store = useVersionStore();
    store.settings = settingsWith("alpha");
    expect(store.requestChannelToggle("alpha")).toEqual({
      needsConfirm: false,
      channel: "alpha",
    });
  });

  it("订阅 update:available 并把载荷落进 store", async () => {
    const store = useVersionStore();
    const off = await store.listenForUpdates();
    expect(on).toHaveBeenCalledWith("update:available", expect.any(Function));
    const handler = handlers.get("update:available");
    expect(handler).toBeTypeOf("function");
    handler?.({ channel: "alpha", version: "0.2.1-alpha.4", needsConfirm: true });
    expect(store.available).toEqual({
      channel: "alpha",
      version: "0.2.1-alpha.4",
      needsConfirm: true,
    });
    off();
  });

  it("needsConfirm=false 时直接进入安装，true 时仍要确认", async () => {
    const store = useVersionStore();
    expect(store.shouldAutoInstall()).toBe(false);
    store.available = { channel: "alpha", version: "0.2.1-alpha.4", needsConfirm: true };
    expect(store.shouldAutoInstall()).toBe(false);
    store.available = { channel: "alpha", version: "0.2.1-alpha.4", needsConfirm: false };
    expect(store.shouldAutoInstall()).toBe(true);
  });
});

describe("version store 的刷新、安装与事件订阅", () => {
  it("refresh 用三条通道调用 check_updates 并落进 report", async () => {
    const store = useVersionStore();
    const report = {
      channels: [],
      localVersion: null,
      versionApplicable: false,
      notApplicableReason: null,
      lastSuccessAtMs: null,
      verdicts: {},
    };
    invokeCmd.mockResolvedValue(report);
    await store.refresh();
    expect(invokeCmd).toHaveBeenCalledWith("check_updates", {
      channels: ["latest", "next", "alpha"],
    });
    expect(store.report).toEqual(report);
  });

  it("loadSettings 只取版本视图需要的三项", async () => {
    const store = useVersionStore();
    invokeCmd.mockResolvedValue({
      autoChannel: "alpha",
      autoInstall: true,
      confirmBeforeInstall: false,
      launchArgs: [],
      workDir: "",
      autoOpenBrowser: true,
      autoScroll: true,
      outputBufferLines: 1000,
      logMaxMiB: 10,
      logKeepFiles: 3,
      oplogMaxMiB: 10,
      oplogKeepFiles: 3,
      stopInstanceOnExit: true,
      autostartMode: "tray",
    });
    await store.loadSettings();
    expect(invokeCmd).toHaveBeenCalledWith("get_settings", {});
    expect(store.settings).toEqual({
      autoChannel: "alpha",
      autoInstall: true,
      confirmBeforeInstall: false,
    });
  });

  it("startInstall 在命令返回前就进入安装态，返回后清空", async () => {
    const store = useVersionStore();
    let resolveInstall: ((value: { taskId: string }) => void) | undefined;
    invokeCmd.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveInstall = resolve;
        }),
    );
    const pending = store.startInstall("latest", "0.2.0-rc.2");
    // 后端 start_install 在安装全部结束后才返回 taskId，因此进度必须先进安装态。
    expect(store.installing).toEqual({
      taskId: null,
      percent: 0,
      remainingMs: null,
      phase: "checking",
    });
    expect(invokeCmd).toHaveBeenCalledWith("start_install", {
      channel: "latest",
      version: "0.2.0-rc.2",
    });
    resolveInstall?.({ taskId: "install-1" });
    await expect(pending).resolves.toBe("install-1");
    expect(store.installing).toBeNull();
  });

  it("task:progress 推进安装进度，并忽略别的 taskId", async () => {
    const store = useVersionStore();
    await store.listenForProgress();
    const progress = handlers.get("task:progress");
    expect(progress).toBeTypeOf("function");

    // 未处于安装态：忽略
    progress?.({ taskId: "install-1", percent: 10, remainingMs: 9000, phase: "downloading" });
    expect(store.installing).toBeNull();

    // 安装中且 taskId 尚未知：首个事件同时记下 taskId（之后 cancel_task 才有目标）
    store.installing = { taskId: null, percent: 0, remainingMs: null, phase: "checking" };
    progress?.({ taskId: "install-1", percent: 10, remainingMs: 9000, phase: "downloading" });
    expect(store.installing).toEqual({
      taskId: "install-1",
      percent: 10,
      remainingMs: 9000,
      phase: "downloading",
    });

    // 其他任务的事件不得改写当前进度
    progress?.({ taskId: "install-2", percent: 99, remainingMs: 0, phase: "installing" });
    expect(store.installing?.percent).toBe(10);
  });

  it("取消安装调用 cancel_task，accepted 时清空安装态", async () => {
    const store = useVersionStore();
    store.installing = {
      taskId: "install-1",
      percent: 50,
      remainingMs: 1000,
      phase: "downloading",
    };
    invokeCmd.mockResolvedValue({ accepted: true });
    await expect(store.cancelInstall()).resolves.toBe(true);
    expect(invokeCmd).toHaveBeenCalledWith("cancel_task", { taskId: "install-1" });
    expect(store.installing).toBeNull();
  });

  it("taskId 尚未知时取消返回 false 且不调用后端", async () => {
    const store = useVersionStore();
    store.installing = { taskId: null, percent: 0, remainingMs: null, phase: "checking" };
    await expect(store.cancelInstall()).resolves.toBe(false);
    expect(invokeCmd).not.toHaveBeenCalled();
    expect(store.installing).not.toBeNull();
  });

  it("startListening 幂等注册；stopListening 注销后可重新注册", async () => {
    const store = useVersionStore();
    await store.startListening();
    await store.startListening();
    // 重复挂载不得叠加监听：每条事件只注册一次
    expect(on).toHaveBeenCalledTimes(2);
    expect(offs).toHaveLength(2);

    const first = offs.slice(0, 2);
    store.stopListening();
    for (const off of first) expect(off).toHaveBeenCalledTimes(1);

    await store.startListening();
    expect(on).toHaveBeenCalledTimes(4);
    expect(offs).toHaveLength(4);
    store.stopListening();
    for (const off of offs.slice(2)) expect(off).toHaveBeenCalledTimes(1);
  });

  it("update:available 且 needsConfirm=false 时直接开始安装", async () => {
    const store = useVersionStore();
    invokeCmd.mockResolvedValue({ taskId: "install-1" });
    await store.listenForUpdates();
    handlers.get("update:available")?.({
      channel: "alpha",
      version: "0.2.1-alpha.4",
      needsConfirm: false,
    });
    await vi.waitFor(() =>
      expect(invokeCmd).toHaveBeenCalledWith("start_install", {
        channel: "alpha",
        version: "0.2.1-alpha.4",
      }),
    );
  });

  it("update:available 且 needsConfirm=true 时只落状态，等界面确认", async () => {
    const store = useVersionStore();
    await store.listenForUpdates();
    handlers.get("update:available")?.({
      channel: "alpha",
      version: "0.2.1-alpha.4",
      needsConfirm: true,
    });
    await Promise.resolve();
    expect(invokeCmd).not.toHaveBeenCalled();
    expect(store.available).toEqual({
      channel: "alpha",
      version: "0.2.1-alpha.4",
      needsConfirm: true,
    });
  });

  it("自动安装结束后刷新检查结果（成功路径）", async () => {
    const store = useVersionStore();
    const report = {
      channels: [],
      localVersion: "0.2.1-alpha.4",
      versionApplicable: true,
      notApplicableReason: null,
      lastSuccessAtMs: null,
      verdicts: {},
    };
    invokeCmd.mockImplementation((name: string) => {
      if (name === "start_install") return Promise.resolve({ taskId: "install-1" });
      if (name === "check_updates") return Promise.resolve(report);
      return Promise.resolve({});
    });
    await store.listenForUpdates();
    handlers.get("update:available")?.({
      channel: "alpha",
      version: "0.2.1-alpha.4",
      needsConfirm: false,
    });
    await vi.waitFor(() => expect(store.report).toEqual(report));
    // 先安装、后刷新：自动安装结束后本机版本可能已变，页面不能停留在旧结论。
    expect(invokeCmd.mock.calls.map((call) => call[0])).toEqual(["start_install", "check_updates"]);
    expect(store.installing).toBeNull();
  });

  it("自动安装失败时不刷新检查结果，失败原因保留给界面（避免自触发安装循环）", async () => {
    const store = useVersionStore();
    const report = {
      channels: [],
      localVersion: null,
      versionApplicable: true,
      notApplicableReason: null,
      lastSuccessAtMs: null,
      verdicts: {},
    };
    const failure = {
      code: ERROR_CODES.INSTALL_FAILED,
      reason: "安装失败。",
      nextStep: "重试。",
      raw: null,
      retryable: true,
    };
    invokeCmd.mockImplementation((name: string) => {
      if (name === "start_install") return Promise.reject(failure);
      if (name === "check_updates") return Promise.resolve(report);
      return Promise.resolve({});
    });
    await store.listenForUpdates();
    handlers.get("update:available")?.({
      channel: "alpha",
      version: "0.2.1-alpha.4",
      needsConfirm: false,
    });
    await vi.waitFor(() => expect(store.installError).toEqual(failure));
    // 失败/取消时本机版本未变：刷新会立刻再次收到 update:available，形成自动安装循环。
    expect(invokeCmd.mock.calls.map((call) => call[0])).toEqual(["start_install"]);
    expect(store.report).toBeNull();
    expect(store.installing).toBeNull();
  });

  it("安装进行中再次 startInstall 直接忽略，不调用后端也不覆盖进度", async () => {
    const store = useVersionStore();
    let resolveInstall: ((value: { taskId: string }) => void) | undefined;
    invokeCmd.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveInstall = resolve;
        }),
    );
    const first = store.startInstall("latest", "0.2.0");
    store.installing = {
      taskId: "install-1",
      percent: 50,
      remainingMs: 1000,
      phase: "downloading",
    };
    await expect(store.startInstall("next", "0.2.1")).resolves.toBeNull();
    expect(invokeCmd).toHaveBeenCalledTimes(1);
    expect(store.installing).toEqual({
      taskId: "install-1",
      percent: 50,
      remainingMs: 1000,
      phase: "downloading",
    });
    resolveInstall?.({ taskId: "install-1" });
    await first;
  });

  it("安装进行中收到 update:available 不落状态、不发起第二个安装（不会弹出空确认框）", async () => {
    const store = useVersionStore();
    await store.listenForUpdates();
    store.installing = {
      taskId: "install-1",
      percent: 50,
      remainingMs: 1000,
      phase: "downloading",
    };
    handlers.get("update:available")?.({
      channel: "alpha",
      version: "0.2.1-alpha.4",
      needsConfirm: true,
    });
    await Promise.resolve();
    // 视图 watch(store.available) 才会弹确认框：安装中不落状态，空操作的确认框不会出现。
    expect(store.available).toBeNull();
    expect(invokeCmd).not.toHaveBeenCalled();
    expect(store.installing?.taskId).toBe("install-1");
  });

  it("cancel_task 不接受时返回 false 并保留安装态，界面据此提示无法取消", async () => {
    const store = useVersionStore();
    store.installing = {
      taskId: "install-1",
      percent: 100,
      remainingMs: null,
      phase: "installing",
    };
    invokeCmd.mockResolvedValue({ accepted: false });
    await expect(store.cancelInstall()).resolves.toBe(false);
    expect(invokeCmd).toHaveBeenCalledWith("cancel_task", { taskId: "install-1" });
    expect(store.installing).not.toBeNull();
  });

  it("安装失败落进 installError 供界面展示；取消不算失败", async () => {
    const store = useVersionStore();
    const failure = {
      code: ERROR_CODES.INSTALL_FAILED,
      reason: "安装子进程以非零状态退出。",
      nextStep: "按提示手工安装后重试。",
      raw: "npm ERR! code EACCES",
      retryable: true,
    };
    invokeCmd.mockRejectedValueOnce(failure);
    await expect(store.startInstall("latest", "0.2.0")).rejects.toBe(failure);
    expect(store.installError).toEqual(failure);
    expect(store.installing).toBeNull();

    const cancelled = {
      code: ERROR_CODES.DOWNLOAD_CANCELLED,
      reason: "安装已取消。",
      nextStep: "需要时可重新发起安装。",
      raw: null,
      retryable: true,
    };
    invokeCmd.mockRejectedValueOnce(cancelled);
    await expect(store.startInstall("latest", "0.2.0")).rejects.toBe(cancelled);
    expect(store.installError).toBeNull();
  });
});
