import { beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { createPinia, setActivePinia } from "pinia";

/**
 * vi.mock 会被提升到 import 之前执行，因此 mock 工厂里只能引用 vi.hoisted 的值，
 * 否则会在模块求值阶段读到 TDZ 里的 invokeCmd（与 client.test.ts 的既有处置一致）。
 */
const mocks = vi.hoisted(() => {
  const handlers = new Map<string, (payload: unknown) => void>();
  const invokeCmd = vi.fn();
  const on = vi.fn((event: string, handler: (payload: unknown) => void) => {
    handlers.set(event, handler);
    return async () => {};
  });
  return { handlers, invokeCmd, on };
});

vi.mock("../ipc/client", () => ({
  invokeCmd: (name: string, args: Record<string, unknown>) => mocks.invokeCmd(name, args),
  on: (event: string, handler: (payload: unknown) => void) => mocks.on(event, handler),
}));

import { useVersionStore } from "./version";

const { handlers, invokeCmd, on } = mocks;

function settingsWith(channel: string | null) {
  return {
    autoChannel: channel,
    autoInstall: channel !== null,
    confirmBeforeInstall: true,
  };
}

describe("version store 的通道切换确认", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    invokeCmd.mockReset();
    on.mockClear();
  });

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
