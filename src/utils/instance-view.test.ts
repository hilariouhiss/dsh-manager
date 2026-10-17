import { describe, expect, it } from "vite-plus/test";

import { asAppError, isInstanceAddress, isLocalAddress, stopConfirmSpec } from "./instance-view";
import type { InstanceInfo } from "../ipc/types";

function info(overrides: Partial<InstanceInfo> = {}): InstanceInfo {
  return {
    instanceId: "inst-1",
    managed: true,
    pid: 5000,
    startedAtMs: new Date(2026, 9, 16, 13, 36).getTime(),
    uptimeMs: 1000,
    url: "http://127.0.0.1:3099",
    state: "running",
    ...overrides,
  };
}

describe("stopConfirmSpec（§3.9 D-3 三档确认）", () => {
  it("本工具实例走普通档，写明实例标识与启动时间", () => {
    const spec = stopConfirmSpec(info(), false);
    expect(spec.strength).toBe("normal");
    expect(spec.title).toBe("停止实例");
    expect(spec.message).toContain("inst-1");
    expect(spec.message).toContain("5000");
    expect(spec.message).toContain("2026-10-16 13:36");
  });

  it("外部实例走强化档：明说不是本工具启动、可能正在执行任务", () => {
    const spec = stopConfirmSpec(
      info({ instanceId: "pid-4242", managed: false, pid: 4242 }),
      false,
    );
    expect(spec.strength).toBe("strong");
    expect(spec.title).toBe("停止外部实例");
    expect(spec.message).toContain("pid-4242");
    expect(spec.message).toContain("不是本工具启动");
    expect(spec.message).toContain("可能正在执行任务");
    expect(spec.message).not.toMatch(/pid-4242[^）]*pid 4242/);
  });

  it("强制终止是单独档，写明实例、启动时间与可能丢失任务", () => {
    const spec = stopConfirmSpec(info(), true);
    expect(spec.strength).toBe("force");
    expect(spec.title).toBe("强制终止实例");
    expect(spec.message).toContain("inst-1");
    expect(spec.message).toContain("2026-10-16 13:36");
    expect(spec.message).toContain("可能丢失它正在进行的任务");
  });

  it("读不到启动时间时如实写「无法取得」，不拿 1970 冒充", () => {
    const spec = stopConfirmSpec(info({ startedAtMs: null }), false);
    expect(spec.message).toContain("启动时间无法取得");
    expect(spec.message).not.toContain("1970");
  });
});

describe("isInstanceAddress（过滤输出里混入的第三方链接）", () => {
  it("同源且带 token 的本地地址属于本实例", () => {
    expect(isInstanceAddress("http://127.0.0.1:3099", "http://127.0.0.1:3099/?token=abc")).toBe(
      true,
    );
  });

  it("端口相同、主机写法不同也算（例如监听 0.0.0.0 而输出给 127.0.0.1）", () => {
    expect(isInstanceAddress("http://0.0.0.0:3099", "http://127.0.0.1:3099")).toBe(true);
  });

  it("输出里的第三方链接不属于本实例", () => {
    expect(isInstanceAddress("http://127.0.0.1:3099", "https://api.deepseek.com/models")).toBe(
      false,
    );
  });

  it("未知实例（known 为 null）只收本机回环地址，不把第三方链接当访问入口", () => {
    expect(isInstanceAddress(null, "http://127.0.0.1:43210")).toBe(true);
    expect(isInstanceAddress(null, "https://api.deepseek.com/models")).toBe(false);
    expect(isInstanceAddress(null, "http://10.0.0.7:3099")).toBe(false);
  });

  it("userinfo 伪装（:3099@evil.com）被拒绝", () => {
    expect(isInstanceAddress("http://127.0.0.1:3099", "http://127.0.0.1:3099@evil.com/")).toBe(
      false,
    );
  });

  it("端口后缀伪装（:30990、:3099.evil.com）被拒绝", () => {
    expect(isInstanceAddress("http://127.0.0.1:3099", "http://127.0.0.1:30990")).toBe(false);
    expect(isInstanceAddress("http://127.0.0.1:3099", "http://127.0.0.1:3099.evil.com/")).toBe(
      false,
    );
  });

  it("非 http(s) 地址被拒绝", () => {
    expect(isInstanceAddress("http://127.0.0.1:3099", "javascript:alert(1)")).toBe(false);
    expect(isInstanceAddress("http://127.0.0.1:3099", "file:///C:/secret.txt")).toBe(false);
  });

  it("非本机地址只有在与实例自己的 origin 完全一致时才接受（--host 局域网 / --tls-cert）", () => {
    expect(isInstanceAddress("http://10.0.0.7:8080", "http://10.0.0.7:8080/?token=abc")).toBe(true);
    expect(isInstanceAddress("http://10.0.0.7:8080", "https://10.0.0.7:8080/?token=abc")).toBe(
      false,
    );
    expect(isInstanceAddress("http://127.0.0.1:3099", "http://10.0.0.7:3099")).toBe(false);
  });
});

describe("isLocalAddress（自动打开浏览器前的本机校验）", () => {
  it("localhost / 127.0.0.1 / ::1 是本机地址", () => {
    expect(isLocalAddress("http://localhost:3099")).toBe(true);
    expect(isLocalAddress("http://127.0.0.1:3099/?token=abc")).toBe(true);
    expect(isLocalAddress("http://[::1]:3099/")).toBe(true);
  });

  it("第三方主机、userinfo 伪装与非 http(s) 都不是本机地址", () => {
    expect(isLocalAddress("https://api.deepseek.com/models")).toBe(false);
    expect(isLocalAddress("http://127.0.0.1:3099@evil.com/")).toBe(false);
    expect(isLocalAddress("javascript:alert(1)")).toBe(false);
    expect(isLocalAddress("not a url")).toBe(false);
  });
});

describe("asAppError", () => {
  it("后端返回的 AppError 五字段原样保留", () => {
    const backend = {
      code: "instance.stop_timeout",
      reason: "复核进程表失败",
      nextStep: "重新点击「停止」再试一次。",
      raw: "access denied",
      retryable: true,
    };
    expect(asAppError(backend)).toEqual(backend);
  });

  it("字符串与 Error 都包成可展示的原因 + 下一步，不静默吞掉", () => {
    const text = asAppError("boom");
    expect(text.reason).toContain("boom");
    expect(text.nextStep).not.toBe("");
    expect(text.retryable).toBe(true);

    const err = asAppError(new Error("WebView 出错"));
    expect(err.reason).toContain("WebView 出错");
    expect(err.nextStep).not.toBe("");
  });
});
