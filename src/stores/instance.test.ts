import { beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { createPinia, setActivePinia } from "pinia";

/**
 * vi.mock 会被提升到 import 之前执行，因此 mock 工厂里只能引用 vi.hoisted 的值
 * （与 version.test.ts / client.test.ts 的既有处置一致）。
 */
const mocks = vi.hoisted(() => {
  const handlers = new Map<string, (payload: unknown) => void>();
  const invokeCmd = vi.fn();
  const offs: Array<ReturnType<typeof vi.fn>> = [];
  const openUrl = vi.fn(async (_url: string) => {});
  const on = vi.fn((event: string, handler: (payload: unknown) => void) => {
    handlers.set(event, handler);
    const off = vi.fn();
    offs.push(off);
    return Promise.resolve(off);
  });
  return { handlers, invokeCmd, on, offs, openUrl };
});

vi.mock("../ipc/client", () => ({
  invokeCmd: (name: string, args: Record<string, unknown>) => mocks.invokeCmd(name, args),
  on: (event: string, handler: (payload: unknown) => void) => mocks.on(event, handler),
}));

vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: (url: string) => mocks.openUrl(url),
}));

import { ERROR_CODES, EVENTS } from "../ipc/types";
import type {
  AppError,
  InstanceInfo,
  InstanceOutputPayload,
  InstanceUrlPayload,
  OutputLine,
  Settings,
  StopResult,
} from "../ipc/types";
import { useInstanceStore } from "./instance";

const { handlers, invokeCmd, on, offs, openUrl } = mocks;

function line(seq: number, text: string, isStderr = false): OutputLine {
  return { seq, timestampMs: 1_700_000_000_000 + seq, text, isStderr };
}

function info(instanceId: string, managed: boolean, url = "http://127.0.0.1:3080"): InstanceInfo {
  return {
    instanceId,
    managed,
    pid: managed ? 5000 : 4242,
    startedAtMs: 1_700_000_000_000,
    uptimeMs: 1000,
    url,
    state: "running",
  };
}

function appError(code: string, retryable: boolean): AppError {
  return {
    code,
    reason: "原因",
    nextStep: "下一步",
    raw: null,
    retryable,
  };
}

function settings(overrides: Partial<Settings> = {}): Settings {
  return {
    autoChannel: null,
    autoInstall: false,
    confirmBeforeInstall: true,
    launchArgs: ["--no-open"],
    workDir: "C:\\Users\\me",
    autoOpenBrowser: true,
    autoScroll: true,
    outputBufferLines: 1000,
    logMaxMiB: 10,
    logKeepFiles: 3,
    oplogMaxMiB: 5,
    oplogKeepFiles: 3,
    stopInstanceOnExit: true,
    autostartMode: "tray",
    ...overrides,
  };
}

beforeEach(() => {
  setActivePinia(createPinia());
  invokeCmd.mockReset();
  on.mockClear();
  handlers.clear();
  offs.length = 0;
  openUrl.mockClear();
});

describe("instance store 的输出缓冲", () => {
  it("同帧到达的多行先攒起来，一帧只并入一次", () => {
    const store = useInstanceStore();
    store.appendLine(line(0, "a"));
    store.appendLine(line(1, "b"));
    store.appendLine(line(2, "c"));
    expect(store.lines).toHaveLength(0);
    store.flushNow();
    expect(store.lines.map((l) => l.text)).toEqual(["a", "b", "c"]);
  });

  it("暂停只冻结渲染，接收照旧", () => {
    const store = useInstanceStore();
    store.appendLine(line(0, "a"));
    store.flushNow();

    store.togglePause();
    store.appendLine(line(1, "b"));
    store.flushNow();

    expect(store.lines.map((l) => l.text)).toEqual(["a", "b"]);
    expect(store.renderedLines.map((l) => l.text)).toEqual(["a"]);
    expect(store.paused).toBe(true);

    store.togglePause();
    expect(store.renderedLines.map((l) => l.text)).toEqual(["a", "b"]);
  });

  it("清空同时清掉缓冲与渲染，并解除暂停", () => {
    const store = useInstanceStore();
    store.appendLine(line(0, "a"));
    store.flushNow();
    store.togglePause();
    store.clear();
    expect(store.lines).toHaveLength(0);
    expect(store.renderedLines).toHaveLength(0);
    expect(store.paused).toBe(false);
  });

  it("清空只清显示、不动回填游标；启动新实例前才归零", () => {
    const store = useInstanceStore();
    store.appendLine(line(4, "a"));
    store.flushNow();

    store.clear();
    expect(store.lines).toHaveLength(0);
    expect(store.lastSeq, "清空显示不等于丢掉回填游标").toBe(4);

    store.instances = [];
    store.clearForStart();
    expect(store.lastSeq, "新实例序号从 0 开始，游标必须归零").toBe(-1);
    expect(store.nextSeq).toBe(0);
  });

  it("启动前只在没有运行实例时清空（有实例在跑时清掉会毁掉它的实时输出）", () => {
    const store = useInstanceStore();
    store.appendLine(line(0, "a"));
    store.flushNow();

    store.instances = [info("inst-1", true)];
    store.clearForStart();
    expect(store.lines.map((l) => l.text)).toEqual(["a"]);

    store.instances = [];
    store.clearForStart();
    expect(store.lines).toHaveLength(0);
  });

  it("truncated 时整体替换缓冲而不是追加", async () => {
    invokeCmd.mockResolvedValue({ lines: [line(9, "x")], nextSeq: 10, truncated: true });
    const store = useInstanceStore();
    store.instances = [info("inst-1", true)];
    store.select("inst-1");
    store.appendLine(line(0, "old"));
    store.flushNow();

    await store.backfillOutput();

    expect(invokeCmd).toHaveBeenCalledWith("list_output", { instanceId: "inst-1", fromSeq: 1 });
    expect(store.lines.map((l) => l.seq)).toEqual([9]);
  });

  it("未 truncated 时把分页追加到已有行之后", async () => {
    invokeCmd.mockResolvedValue({ lines: [line(1, "b")], nextSeq: 2, truncated: false });
    const store = useInstanceStore();
    store.instances = [info("inst-1", true)];
    store.select("inst-1");
    store.appendLine(line(0, "a"));
    store.flushNow();

    await store.backfillOutput();

    expect(store.lines.map((l) => l.text)).toEqual(["a", "b"]);
  });
});

describe("instance store 的输出回填（视图重挂载后补齐离线期间的行）", () => {
  it("只并入未持有的序号：与实时行去重并保持 seq 顺序", async () => {
    const store = useInstanceStore();
    store.instances = [info("inst-1", true)];
    store.select("inst-1");
    store.appendLine(line(0, "live-0"));
    store.appendLine(line(2, "live-2"));
    store.flushNow();
    invokeCmd.mockResolvedValue({
      lines: [line(1, "back-1"), line(2, "live-2")],
      nextSeq: 3,
      truncated: false,
    });

    await store.backfillOutput();

    expect(invokeCmd).toHaveBeenCalledWith("list_output", {
      instanceId: "inst-1",
      fromSeq: 3,
    });
    expect(store.lines.map((l) => l.seq)).toEqual([0, 1, 2]);
    expect(store.lines.map((l) => l.text)).toEqual(["live-0", "back-1", "live-2"]);

    // 回填之后事件又重放了同一序号：不得重复并入。
    store.appendLine(line(2, "直播重放"));
    store.flushNow();
    expect(store.lines.map((l) => l.text)).toEqual(["live-0", "back-1", "live-2"]);
    expect(store.lastSeq).toBe(2);
    expect(store.nextSeq).toBe(3);
    expect(store.truncated).toBe(false);
  });

  it("分页从 lastSeq+1 继续；truncated 时按当前缓冲整体重建", async () => {
    const store = useInstanceStore();
    store.instances = [info("inst-1", true)];
    store.select("inst-1");
    invokeCmd.mockResolvedValueOnce({
      lines: [line(0, "a"), line(1, "b")],
      nextSeq: 2,
      truncated: false,
    });

    await store.backfillOutput();
    expect(store.lines.map((l) => l.seq)).toEqual([0, 1]);
    expect(store.nextSeq).toBe(2);

    invokeCmd.mockResolvedValueOnce({ lines: [], nextSeq: 2, truncated: false });
    await store.backfillOutput();
    expect(invokeCmd).toHaveBeenLastCalledWith("list_output", {
      instanceId: "inst-1",
      fromSeq: 2,
    });
    expect(store.lines.map((l) => l.seq)).toEqual([0, 1]);

    invokeCmd.mockResolvedValueOnce({
      lines: [line(9, "x"), line(10, "y")],
      nextSeq: 11,
      truncated: true,
    });
    await store.backfillOutput();
    expect(store.lines.map((l) => l.seq)).toEqual([9, 10]);
    expect(store.lastSeq).toBe(10);
    expect(store.nextSeq).toBe(11);
    expect(store.truncated).toBe(true);
  });

  it("外部实例不回填（无法获取输出）", async () => {
    const store = useInstanceStore();
    store.instances = [info("pid-4242", false)];
    store.select("pid-4242");

    await store.backfillOutput();

    expect(invokeCmd).not.toHaveBeenCalled();
    expect(store.lines).toHaveLength(0);
  });

  it("没有选中实例时不发命令", async () => {
    const store = useInstanceStore();
    await store.backfillOutput();
    expect(invokeCmd).not.toHaveBeenCalled();
  });

  it("选中的受管实例收到 instance:state 后刷新并回填", async () => {
    invokeCmd.mockImplementation((name: string) => {
      if (name === "list_instances") return Promise.resolve([info("inst-1", true)]);
      if (name === "list_output") {
        return Promise.resolve({ lines: [line(0, "hello")], nextSeq: 1, truncated: false });
      }
      return Promise.resolve([]);
    });
    const store = useInstanceStore();
    await store.refresh();
    store.select("inst-1");
    await store.subscribe();
    const handler = handlers.get(EVENTS.EVENT_INSTANCE_STATE) as ((p: unknown) => void) | undefined;

    await handler?.({ instanceId: "inst-1", state: "running", startedAtMs: 1, uptimeMs: 2 });

    expect(invokeCmd).toHaveBeenCalledWith("list_output", {
      instanceId: "inst-1",
      fromSeq: 0,
    });
    expect(store.lines.map((l) => l.text)).toEqual(["hello"]);
  });
});

describe("instance store 的实例列表", () => {
  it("refresh 调用 list_instances，空数组不报错", async () => {
    invokeCmd.mockResolvedValue([]);
    const store = useInstanceStore();
    await store.refresh();
    expect(invokeCmd).toHaveBeenCalledWith("list_instances", undefined);
    expect(store.instances).toHaveLength(0);
    expect(store.error).toBeNull();
  });

  it("选中外部实例时给出「无法获取其输出」的判据（设计文档 §2.4 边界 2）", async () => {
    invokeCmd.mockResolvedValue([info("pid-4242", false), info("inst-1", true)]);
    const store = useInstanceStore();
    await store.refresh();

    store.select("pid-4242");
    expect(store.externalSelected).toBe(true);
    expect(store.selected?.instanceId).toBe("pid-4242");

    store.select("inst-1");
    expect(store.externalSelected).toBe(false);
  });

  it("refresh 失败时保留错误，不伪造空列表", async () => {
    invokeCmd.mockRejectedValue(appError("path.permission_denied", false));
    const store = useInstanceStore();
    await store.refresh();
    expect(store.error).not.toBeNull();
  });
});

describe("instance store 的事件订阅", () => {
  it("注册三类事件，unlisten 时全部注销（防止输出重复渲染）", async () => {
    const store = useInstanceStore();
    const off = await store.subscribe();

    expect(on).toHaveBeenCalledTimes(3);
    const names = vi.mocked(on).mock.calls.map((c) => c[0]);
    expect(names).toEqual([
      EVENTS.EVENT_INSTANCE_OUTPUT,
      EVENTS.EVENT_INSTANCE_STATE,
      EVENTS.EVENT_INSTANCE_URL,
    ]);

    off();
    for (const u of offs) expect(u).toHaveBeenCalledTimes(1);
  });

  it("收到 instance:output 后进入缓冲", async () => {
    const store = useInstanceStore();
    await store.subscribe();
    const handler = handlers.get(EVENTS.EVENT_INSTANCE_OUTPUT) as
      | ((p: InstanceOutputPayload) => void)
      | undefined;
    handler?.({ instanceId: "inst-1", seq: 0, timestampMs: 1, text: "hi", isStderr: true });
    store.flushNow();

    expect(store.lines.map((l) => l.text)).toEqual(["hi"]);
    expect(store.lines[0].isStderr).toBe(true);
  });

  it("收到 instance:state 后刷新列表（停止后「未运行」由刷新驱动）", async () => {
    invokeCmd.mockResolvedValue([]);
    const store = useInstanceStore();
    await store.subscribe();
    const handler = handlers.get(EVENTS.EVENT_INSTANCE_STATE) as ((p: unknown) => void) | undefined;
    handler?.({ instanceId: "inst-1", state: "stopped", startedAtMs: 1, uptimeMs: 2 });
    await Promise.resolve();
    expect(invokeCmd).toHaveBeenCalledWith("list_instances", undefined);
  });

  it("地址事件带 ?token= 时保留完整地址（不裁掉 token）", async () => {
    invokeCmd.mockImplementation((name: string) =>
      name === "list_instances" ? Promise.resolve([info("inst-1", true)]) : Promise.resolve([]),
    );
    const store = useInstanceStore();
    await store.refresh();
    await store.subscribe();
    const handler = handlers.get(EVENTS.EVENT_INSTANCE_URL) as
      | ((p: InstanceUrlPayload) => void)
      | undefined;
    handler?.({ instanceId: "inst-1", url: "http://127.0.0.1:3080/?token=abc" });

    expect(store.urlFor(info("inst-1", true))).toBe("http://127.0.0.1:3080/?token=abc");
  });

  it("输出里的第三方链接不会被当成实例地址（真机实测噪声）", async () => {
    invokeCmd.mockImplementation((name: string) =>
      name === "list_instances" ? Promise.resolve([info("inst-1", true)]) : Promise.resolve([]),
    );
    const store = useInstanceStore();
    await store.refresh();
    await store.subscribe();
    const handler = handlers.get(EVENTS.EVENT_INSTANCE_URL) as
      | ((p: InstanceUrlPayload) => void)
      | undefined;
    handler?.({ instanceId: "inst-1", url: "https://api.deepseek.com/models" });

    expect(store.urlFor(info("inst-1", true))).toBe("http://127.0.0.1:3080");
    expect(openUrl).not.toHaveBeenCalled();
  });

  it("实例还没出现在列表里时不自动打开地址（不 fail-open）", async () => {
    invokeCmd.mockResolvedValue([]);
    const store = useInstanceStore();
    await store.subscribe();
    const handler = handlers.get(EVENTS.EVENT_INSTANCE_URL) as
      | ((p: InstanceUrlPayload) => void)
      | undefined;
    handler?.({ instanceId: "inst-late", url: "http://127.0.0.1:3099/?token=abc" });
    await Promise.resolve();

    expect(openUrl).not.toHaveBeenCalled();
    // 本机地址仍可留作显示：实例随后出现在列表里时用得到。
    expect(store.urls["inst-late"]).toBe("http://127.0.0.1:3099/?token=abc");
  });

  it("非本机地址与实例 origin 一致时只显示链接，不自动打开", async () => {
    invokeCmd.mockImplementation((name: string) =>
      name === "list_instances"
        ? Promise.resolve([info("inst-1", true, "http://10.0.0.7:8080")])
        : Promise.resolve([]),
    );
    const store = useInstanceStore();
    await store.refresh();
    await store.subscribe();
    const handler = handlers.get(EVENTS.EVENT_INSTANCE_URL) as
      | ((p: InstanceUrlPayload) => void)
      | undefined;
    handler?.({ instanceId: "inst-1", url: "http://10.0.0.7:8080/?token=abc" });
    await Promise.resolve();

    expect(store.urlFor(info("inst-1", true, "http://10.0.0.7:8080"))).toBe(
      "http://10.0.0.7:8080/?token=abc",
    );
    expect(openUrl).not.toHaveBeenCalled();
  });

  /// FR-3.3 与 §7.1 偏差 D-B 的落点：**是否自动打开浏览器必须是设置项**。
  it("autoOpenBrowser 为真才打开地址，为假时只接收不打开", async () => {
    invokeCmd.mockImplementation((name: string) => {
      if (name === "get_settings") return Promise.resolve(settings({ autoOpenBrowser: false }));
      if (name === "list_instances") {
        return Promise.resolve([info("inst-1", true, "http://127.0.0.1:3099")]);
      }
      return Promise.resolve([]);
    });

    const store = useInstanceStore();
    await store.loadSettings();
    await store.refresh();
    await store.subscribe();
    const handler = handlers.get(EVENTS.EVENT_INSTANCE_URL) as
      | ((p: InstanceUrlPayload) => void)
      | undefined;

    handler?.({ instanceId: "inst-1", url: "http://127.0.0.1:3099" });
    await Promise.resolve();
    expect(openUrl).not.toHaveBeenCalled();

    store.setAutoOpenBrowser(true);
    handler?.({ instanceId: "inst-1", url: "http://127.0.0.1:3099" });
    await Promise.resolve();
    expect(openUrl).toHaveBeenCalledWith("http://127.0.0.1:3099");
  });

  it("同一实例的地址只自动打开一次（后端重发也不重复开浏览器）", async () => {
    invokeCmd.mockResolvedValue([info("inst-1", true, "http://127.0.0.1:3099")]);
    const store = useInstanceStore();
    await store.refresh();
    await store.subscribe();
    const handler = handlers.get(EVENTS.EVENT_INSTANCE_URL) as
      | ((p: InstanceUrlPayload) => void)
      | undefined;

    handler?.({ instanceId: "inst-1", url: "http://127.0.0.1:3099/?token=abc" });
    handler?.({ instanceId: "inst-1", url: "http://127.0.0.1:3099/?token=abc" });
    await Promise.resolve();

    expect(openUrl).toHaveBeenCalledTimes(1);
    expect(openUrl).toHaveBeenCalledWith("http://127.0.0.1:3099/?token=abc");
  });

  it("链接点击走同一个打开入口，失败时留下可展示的错误", async () => {
    invokeCmd.mockResolvedValue([]);
    const store = useInstanceStore();
    openUrl.mockRejectedValueOnce("no browser");
    store.openUrl("http://127.0.0.1:3099");
    await Promise.resolve();
    expect(store.error?.reason).toContain("no browser");
  });
});

describe("instance store 的设置", () => {
  it("loadSettings 读 autoOpenBrowser、autoScroll 与工作目录默认值", async () => {
    invokeCmd.mockResolvedValue(settings({ autoOpenBrowser: false, autoScroll: false }));
    const store = useInstanceStore();
    await store.loadSettings();
    expect(store.autoOpenBrowser).toBe(false);
    expect(store.autoScroll).toBe(false);
    expect(store.defaultWorkDir).toBe("C:\\Users\\me");
  });

  it("setting 读取失败时保留当前值，只记错误", async () => {
    invokeCmd.mockRejectedValue(appError("path.permission_denied", false));
    const store = useInstanceStore();
    await store.loadSettings();
    expect(store.autoOpenBrowser).toBe(true);
    expect(store.error?.code).toBe(ERROR_CODES.PATH_PERMISSION_DENIED);
  });

  it("自动滚动开关写回设置（持久化），失败时保留本地选择并记错误", async () => {
    invokeCmd.mockResolvedValue(settings());
    const store = useInstanceStore();
    await store.setAutoScroll(false);
    expect(invokeCmd).toHaveBeenCalledWith("update_settings", { patch: { autoScroll: false } });
    expect(store.autoScroll).toBe(false);

    invokeCmd.mockRejectedValue(appError("path.permission_denied", false));
    await store.setAutoScroll(true);
    expect(store.autoScroll).toBe(true);
    expect(store.error?.code).toBe(ERROR_CODES.PATH_PERMISSION_DENIED);
  });
});

describe("instance store 的停止流程", () => {
  it("本工具实例用普通确认后的参数，成功后刷新列表", async () => {
    invokeCmd.mockImplementation((name: string) => {
      if (name === "stop_instance") {
        return Promise.resolve({
          stopped: true,
          residualPids: [],
          forced: false,
        } satisfies StopResult);
      }
      return Promise.resolve([]);
    });
    const store = useInstanceStore();
    const outcome = await store.stop(info("inst-1", true), false);

    expect(invokeCmd).toHaveBeenCalledWith("stop_instance", {
      instanceId: "inst-1",
      force: false,
      confirmExternal: false,
    });
    expect(invokeCmd).toHaveBeenCalledWith("list_instances", undefined);
    expect(outcome?.kind).toBe("stopped");
  });

  it("外部实例必须带 confirmExternal=true", async () => {
    invokeCmd.mockImplementation((name: string) => {
      if (name === "stop_instance") {
        return Promise.resolve({
          stopped: true,
          residualPids: [],
          forced: false,
        } satisfies StopResult);
      }
      return Promise.resolve([]);
    });
    const store = useInstanceStore();
    await store.stop(info("pid-4242", false), false);

    expect(invokeCmd).toHaveBeenCalledWith("stop_instance", {
      instanceId: "pid-4242",
      force: false,
      confirmExternal: true,
    });
  });

  it("停止期间置为 stopping 并拒绝重复提交", async () => {
    let resolveStop: (value: StopResult) => void = () => {};
    const pending = new Promise<StopResult>((resolve) => {
      resolveStop = resolve;
    });
    invokeCmd.mockImplementation((name: string) => {
      if (name === "stop_instance") return pending;
      return Promise.resolve([]);
    });
    const store = useInstanceStore();
    const instance = info("inst-1", true);

    const first = store.stop(instance, false);
    expect(store.stoppingId).toBe("inst-1");
    const second = await store.stop(instance, false);
    expect(second).toBeNull();

    resolveStop({ stopped: true, residualPids: [], forced: false });
    await first;
    expect(store.stoppingId).toBeNull();
    expect(invokeCmd.mock.calls.filter((call) => call[0] === "stop_instance")).toHaveLength(1);
  });

  it("instance.forced_stop_required 单独返回，交给强制确认对话框", async () => {
    const forced = appError(ERROR_CODES.INSTANCE_FORCED_STOP_REQUIRED, false);
    invokeCmd.mockImplementation((name: string) => {
      if (name === "stop_instance") return Promise.reject(forced);
      return Promise.resolve([]);
    });
    const store = useInstanceStore();
    const outcome = await store.stop(info("inst-1", true), false);

    expect(outcome).toEqual({ kind: "forceRequired", error: forced });
    expect(store.error).toBeNull();
  });

  it("stopped=false 时如实回报残留进程，并给可重试的错误", async () => {
    invokeCmd.mockImplementation((name: string) => {
      if (name === "stop_instance") {
        return Promise.resolve({
          stopped: false,
          residualPids: [11, 22],
          forced: true,
        } satisfies StopResult);
      }
      return Promise.resolve([]);
    });
    const store = useInstanceStore();
    const outcome = await store.stop(info("inst-1", true), true);

    expect(outcome?.kind).toBe("residual");
    if (outcome?.kind !== "residual") throw new Error("unreachable");
    expect(outcome.error.code).toBe(ERROR_CODES.INSTANCE_STOP_TIMEOUT);
    expect(outcome.error.reason).toContain("2 个进程");
    expect(outcome.error.nextStep).toContain("11、22");
    expect(outcome.error.retryable).toBe(true);
  });

  it("instance.stop_timeout 作为可重试错误返回", async () => {
    const timeout = appError(ERROR_CODES.INSTANCE_STOP_TIMEOUT, true);
    invokeCmd.mockImplementation((name: string) => {
      if (name === "stop_instance") return Promise.reject(timeout);
      return Promise.resolve([]);
    });
    const store = useInstanceStore();
    const outcome = await store.stop(info("inst-1", true), false);

    expect(outcome?.kind).toBe("error");
    if (outcome?.kind !== "error") throw new Error("unreachable");
    expect(outcome.error.retryable).toBe(true);
  });

  it("instance.not_managed 不当作可重试错误：提示刷新列表", async () => {
    const notManaged = appError(ERROR_CODES.INSTANCE_NOT_MANAGED, false);
    invokeCmd.mockImplementation((name: string) => {
      if (name === "stop_instance") return Promise.reject(notManaged);
      return Promise.resolve([]);
    });
    const store = useInstanceStore();
    const outcome = await store.stop(info("pid-4242", false), false);

    expect(outcome?.kind).toBe("error");
    if (outcome?.kind !== "error") throw new Error("unreachable");
    expect(outcome.error.retryable).toBe(false);
    expect(invokeCmd).toHaveBeenCalledWith("list_instances", undefined);
  });
});

describe("instance store 的日志导出", () => {
  it("exportAs 带上选中的实例与用户选择的路径，返回写入字节数", async () => {
    invokeCmd.mockImplementation((name: string) => {
      if (name === "export_output") return Promise.resolve({ bytesWritten: 42 });
      return Promise.resolve([]);
    });
    const store = useInstanceStore();
    store.select("inst-1");

    await expect(store.exportAs("C:\\tmp\\out.log")).resolves.toBe(42);
    expect(invokeCmd).toHaveBeenCalledWith("export_output", {
      instanceId: "inst-1",
      targetPath: "C:\\tmp\\out.log",
    });
  });

  it("没有选中实例时不发命令，直接报错", async () => {
    const store = useInstanceStore();
    await expect(store.exportAs("C:\\tmp\\out.log")).rejects.toThrow("没有选中的实例");
    expect(invokeCmd).not.toHaveBeenCalled();
  });
});
