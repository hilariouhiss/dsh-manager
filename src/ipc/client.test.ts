import { describe, expect, it, vi } from "vite-plus/test";
import type { AppError } from "./types";

/**
 * 在导入被测模块之前把 window.__TAURI_INTERNALS__ 装好：@tauri-apps/api 的 invoke
 * 从这里取实现，而 Node 环境下没有 window。vi.hoisted 的回调先于 import 执行。
 */
const internals = vi.hoisted(() => {
  const state = {
    invoke: async (..._args: unknown[]): Promise<unknown> => undefined,
  };
  (globalThis as unknown as { window: unknown }).window = {
    __TAURI_INTERNALS__: state,
  };
  return state;
});

const { invokeCmd, isAppError } = await import("./client");

const sampleError: AppError = {
  code: "network.offline",
  reason: "网络请求未能完成",
  nextStep: "检查网络连接与系统代理设置后重试。",
  raw: "dns error",
  retryable: true,
};

describe("invokeCmd", () => {
  it("原样透传命令名与参数，不做包装", async () => {
    const invoke = vi.spyOn(internals, "invoke").mockResolvedValue("pong");
    await expect(invokeCmd<string>("ping", { fail: false })).resolves.toBe("pong");
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke.mock.calls[0][0]).toBe("ping");
    expect(invoke.mock.calls[0][1]).toEqual({ fail: false });
    vi.restoreAllMocks();
  });

  it("失败时把 AppError 原样抛出（不吞错、不改写）", async () => {
    vi.spyOn(internals, "invoke").mockRejectedValue(sampleError);
    await expect(invokeCmd("ping", { fail: true })).rejects.toBe(sampleError);
    vi.restoreAllMocks();
  });
});

describe("isAppError", () => {
  it("五个字段齐全时判为真", () => {
    expect(isAppError(sampleError)).toBe(true);
  });

  it("字符串或其他对象判为假", () => {
    expect(isAppError("boom")).toBe(false);
    expect(isAppError({ code: "x", reason: "y" })).toBe(false);
    expect(isAppError(null)).toBe(false);
  });
});
