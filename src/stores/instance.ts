import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { openUrl as openWithSystem } from "@tauri-apps/plugin-opener";

import { invokeCmd, on } from "../ipc/client";
import { ERROR_CODES, EVENTS } from "../ipc/types";
import type {
  AppError,
  InstanceInfo,
  InstanceOutputPayload,
  InstanceStatePayload,
  InstanceUrlPayload,
  ListOutputResult,
  OutputLine,
  Settings,
  StopResult,
} from "../ipc/types";
import { asAppError, isInstanceAddress, isLocalAddress } from "../utils/instance-view";

/** 同帧到达的行先攒这么久再一次性并入——长输出不至于把界面拖死。 */
export const FLUSH_DELAY_MS = 16;

/** 停止结果：视图据此决定弹哪个对话框、显示哪种错误（§3.9 D-3）。 */
export type StopOutcome =
  | { kind: "stopped" }
  | { kind: "forceRequired"; error: AppError }
  | { kind: "residual"; error: AppError }
  | { kind: "error"; error: AppError };

export const useInstanceStore = defineStore("instance", () => {
  const instances = ref<InstanceInfo[]>([]);
  const lines = ref<OutputLine[]>([]);
  const paused = ref(false);
  const autoScroll = ref(true);
  const autoOpenBrowser = ref(true);
  /** 设置里的默认工作目录：启动表单据此预填（后端在留空时也会回落）。 */
  const defaultWorkDir = ref("");
  const selectedId = ref<string | null>(null);
  const error = ref<AppError | null>(null);
  /** 正在停止的实例：最多等 5 秒，期间禁止重复提交。 */
  const stoppingId = ref<string | null>(null);
  /** instance:url 带来的完整地址（可能带 ?token=），优先于列表里由命令行拼出的地址。 */
  const urls = ref<Record<string, string>>({});
  /** 暂停时的可见长度；null 表示没暂停。 */
  const pausedAt = ref<number | null>(null);
  /** 已并入缓冲的最大序号；-1 表示还没有任何行。回填从 lastSeq+1 继续。 */
  const lastSeq = ref(-1);
  /** 下一个期望的序号：回填页的 nextSeq，实时行到达后同步推进。 */
  const nextSeq = ref(0);
  /** 最近一次回填是否因起点被环形缓冲挤出而整体重建。 */
  const truncated = ref(false);

  const pending: OutputLine[] = [];
  let flushTimer: ReturnType<typeof setTimeout> | null = null;
  const openedUrls = new Set<string>();

  /** 暂停只冻结**渲染**，缓冲照旧增长（§5 T-I2 要点）。 */
  const renderedLines = computed(() =>
    pausedAt.value === null ? lines.value : lines.value.slice(0, pausedAt.value),
  );
  const selected = computed(
    () => instances.value.find((i) => i.instanceId === selectedId.value) ?? null,
  );
  /** 外部实例拿不到输出，界面据此标注（设计文档 §2.4 边界 2）。 */
  const externalSelected = computed(() => selected.value !== null && !selected.value.managed);

  function urlFor(instance: InstanceInfo): string | null {
    return urls.value[instance.instanceId] ?? instance.url;
  }

  function isStopping(instanceId: string): boolean {
    return stoppingId.value === instanceId;
  }

  function flush(): void {
    flushTimer = null;
    if (pending.length === 0) return;
    lines.value.push(...pending.splice(0, pending.length));
  }

  /** 测试用：立刻并入，不等计时器。 */
  function flushNow(): void {
    if (flushTimer !== null) {
      clearTimeout(flushTimer);
      flushTimer = null;
    }
    flush();
  }

  function appendLine(line: OutputLine): void {
    // 事件与回填可能重放已持有的序号：序号单调，已并入的行不再进缓冲。
    if (line.seq <= lastSeq.value) return;
    lastSeq.value = line.seq;
    nextSeq.value = Math.max(nextSeq.value, line.seq + 1);
    pending.push(line);
    if (flushTimer === null) {
      flushTimer = setTimeout(flush, FLUSH_DELAY_MS);
    }
  }

  /**
   * 首个未持有的序号。视图必须在订阅/任何 await **之前**取它：晚一步，
   * 订阅期间到达的实时行会把 lastSeq 推到卸载期间缺失的那一段之后。
   */
  function firstMissingSeq(): number {
    return lastSeq.value + 1;
  }

  function togglePause(): void {
    pausedAt.value = pausedAt.value === null ? lines.value.length : null;
    paused.value = pausedAt.value !== null;
  }

  function setAutoOpenBrowser(value: boolean): void {
    autoOpenBrowser.value = value;
  }

  /** 自动滚动开关是持久设置（FR-1.4）：写回失败时保留本地选择并显示错误，不静默吞掉。 */
  async function setAutoScroll(value: boolean): Promise<void> {
    autoScroll.value = value;
    try {
      await invokeCmd<Settings>("update_settings", { patch: { autoScroll: value } });
      error.value = null;
    } catch (e) {
      error.value = asAppError(e);
    }
  }

  /** 清空只清显示：回填游标保留，切走再回来不会把用户主动清掉的行重新拉回来。 */
  function clear(): void {
    pending.length = 0;
    lines.value = [];
    pausedAt.value = null;
    paused.value = false;
  }

  /** 清空并归零回填游标：新一次运行的输出序号从 0 重新开始。 */
  function resetOutput(): void {
    clear();
    lastSeq.value = -1;
    nextSeq.value = 0;
    truncated.value = false;
  }

  /**
   * 启动前清掉上一轮的行。有实例在跑时**不清**：后端会以 instance.already_running
   * 拒绝启动，清掉只会把在跑实例的实时输出抹掉，且没有回填。
   * 列表可能过期，所以启动成功后还必须再 resetOutput() 一次（见 InstanceView.start）。
   */
  function clearForStart(): void {
    if (instances.value.length === 0) resetOutput();
  }

  function select(instanceId: string | null): void {
    selectedId.value = instanceId;
  }

  async function refresh(): Promise<void> {
    try {
      instances.value = await invokeCmd<InstanceInfo[]>("list_instances");
      error.value = null;
    } catch (e) {
      error.value = asAppError(e);
    }
  }

  /**
   * 把一页后端缓冲并入本地行：按 seq 去重并排序，截断时只丢弃快照之外的行。
   * 合并前先并入等待中的实时行，否则它们会在 flush 后与回填行重复。
   */
  function mergePage(page: ListOutputResult): void {
    flushNow();
    // 截断时后端给的是「当前缓冲全部内容」；快照之后到达的实时行（seq >= nextSeq）必须留下。
    const kept = page.truncated ? lines.value.filter((l) => l.seq >= page.nextSeq) : lines.value;
    const bySeq = new Map<number, OutputLine>();
    for (const line of [...kept, ...page.lines]) bySeq.set(line.seq, line);
    lines.value = [...bySeq.values()].sort((a, b) => a.seq - b.seq);
    lastSeq.value = lines.value.length === 0 ? -1 : lines.value[lines.value.length - 1].seq;
    // 快照会落后于实时事件：nextSeq 只增不减，否则会被旧快照拨回去。
    nextSeq.value = Math.max(nextSeq.value, page.nextSeq);
    truncated.value = page.truncated;
  }

  /**
   * 回填选中实例的输出（命令 9）：视图重挂载或实例状态变化时补齐离线期间的行。
   * `fromSeq` 默认取首个缺失序号，但重挂载路径必须传挂载前算好的值——订阅期间
   * 到达的实时行会推进 lastSeq，事后再算就会把卸载期间缺的那一段跳过去（FR-3.2）。
   * 逐页取到快照末尾或没有更多行为止，mergePage 按 seq 去重并保持顺序。
   * 外部实例后端拿不到输出，直接跳过，不用空内容冒充「没有输出」（设计文档 §2.4 边界 2）。
   */
  async function backfillOutput(fromSeq = firstMissingSeq()): Promise<void> {
    const instance = selected.value;
    if (instance === null || !instance.managed) return;
    let cursor = fromSeq;
    try {
      for (;;) {
        const page = await invokeCmd<ListOutputResult>("list_output", {
          instanceId: instance.instanceId,
          fromSeq: cursor,
        });
        mergePage(page);
        const next = lastSeq.value + 1;
        // 并到快照末尾、没有更多行，或游标不再前进（防死循环）即完成。
        if (page.lines.length === 0 || next >= page.nextSeq || next <= cursor) break;
        cursor = next;
      }
      // 成功不回写 error：它可能来自先前的 refresh 失败，回填成功不证明那次已恢复。
    } catch (e) {
      error.value = asAppError(e);
    }
  }

  async function exportAs(targetPath: string): Promise<number> {
    const instanceId = selectedId.value;
    if (instanceId === null) throw new Error("没有选中的实例");
    const result = await invokeCmd<{ bytesWritten: number }>("export_output", {
      instanceId,
      targetPath,
    });
    return result.bytesWritten;
  }

  /** 打开地址：自动打开与点击链接共用同一个入口（Tauri opener 插件）。 */
  function openUrl(url: string): void {
    void openWithSystem(url).catch((e) => {
      error.value = asAppError(e);
    });
  }

  /**
   * 停止实例（命令 8）。三档确认由调用方在弹窗里完成：
   * force=true 只会在强制确认之后传入，confirmExternal 由归属推导，外部实例不会被静默停止。
   */
  async function stop(instance: InstanceInfo, force: boolean): Promise<StopOutcome | null> {
    if (stoppingId.value !== null) return null;
    stoppingId.value = instance.instanceId;
    try {
      const result = await invokeCmd<StopResult>("stop_instance", {
        instanceId: instance.instanceId,
        force,
        confirmExternal: !instance.managed,
      });
      // 「未运行」由列表刷新 + instance:state 事件共同驱动（不靠前端猜）。
      await refresh();
      if (!result.stopped) {
        return {
          kind: "residual",
          error: {
            code: ERROR_CODES.INSTANCE_STOP_TIMEOUT,
            reason:
              "实例 " +
              instance.instanceId +
              " 已收到终止命令，但仍有 " +
              result.residualPids.length +
              " 个进程没有退出",
            nextStep:
              "残留进程：" +
              result.residualPids.join("、") +
              "。可在任务管理器中结束它们，或稍后重试。",
            raw: null,
            retryable: true,
          },
        };
      }
      return { kind: "stopped" };
    } catch (e) {
      const err = asAppError(e);
      // 失败后也刷新：可能已退出、或状态仍是 running，界面以列表为准。
      await refresh();
      if (err.code === ERROR_CODES.INSTANCE_FORCED_STOP_REQUIRED) {
        return { kind: "forceRequired", error: err };
      }
      return { kind: "error", error: err };
    } finally {
      stoppingId.value = null;
    }
  }

  /** 读一次设置：autoOpenBrowser 决定是否自动开浏览器，autoScroll 是面板开关的初值。 */
  async function loadSettings(): Promise<void> {
    try {
      const settings = await invokeCmd<Settings>("get_settings");
      autoOpenBrowser.value = settings.autoOpenBrowser;
      autoScroll.value = settings.autoScroll;
      defaultWorkDir.value = settings.workDir;
      error.value = null;
    } catch (e) {
      error.value = asAppError(e);
    }
  }

  /** 视图挂载时调用；返回的函数必须在卸载时调用，否则输出会重复渲染（设计文档 §6.2）。 */
  async function subscribe(): Promise<() => void> {
    // 事件名一律走 EVENTS 镜像，不写字符串字面量（主计划 §3.5）。
    const offOutput = await on<InstanceOutputPayload>(EVENTS.EVENT_INSTANCE_OUTPUT, (p) => {
      appendLine({
        seq: p.seq,
        timestampMs: p.timestampMs,
        text: p.text,
        isStderr: p.isStderr,
      });
    });
    const offState = await on<InstanceStatePayload>(EVENTS.EVENT_INSTANCE_STATE, async (p) => {
      await refresh();
      // 受管实例状态变化（例如 running）后补一次：期间可能已有输出落在缓冲里。
      const instance = selected.value;
      if (instance !== null && instance.instanceId === p.instanceId && instance.managed) {
        await backfillOutput();
      }
    });
    const offUrl = await on<InstanceUrlPayload>(EVENTS.EVENT_INSTANCE_URL, (p) => {
      // T-I3 的提取器还会认出输出里的第三方链接（例如 API 地址），
      // 只有属于这个实例的地址才显示/打开；否则会把日志里的链接当成访问入口。
      const instance = instances.value.find((i) => i.instanceId === p.instanceId);
      // 实例还没出现在列表里时没有可核对的地址：只收本机回环地址，且不自动打开。
      const belongs = isInstanceAddress(instance?.url ?? null, p.url);
      if (belongs) {
        // 地址里可能带 ?token=：原样显示、原样打开，不裁剪（Lead 裁定）。
        urls.value = { ...urls.value, [p.instanceId]: p.url };
      }
      // FR-3.3 与偏差 D-B 的落点：本工具是**唯一**的浏览器打开方。
      // 实例未知或主机不是本机时都不自动打开（--host 局域网地址只显示链接）。
      if (
        belongs &&
        instance !== undefined &&
        isLocalAddress(p.url) &&
        autoOpenBrowser.value &&
        !openedUrls.has(p.instanceId)
      ) {
        openedUrls.add(p.instanceId);
        openUrl(p.url);
      }
      void refresh();
    });
    return () => {
      offOutput();
      offState();
      offUrl();
    };
  }

  return {
    instances,
    lines,
    renderedLines,
    paused,
    autoScroll,
    autoOpenBrowser,
    defaultWorkDir,
    selectedId,
    selected,
    externalSelected,
    error,
    stoppingId,
    urls,
    lastSeq,
    nextSeq,
    truncated,
    flushNow,
    appendLine,
    togglePause,
    setAutoScroll,
    setAutoOpenBrowser,
    clear,
    clearForStart,
    resetOutput,
    firstMissingSeq,
    select,
    refresh,
    loadSettings,
    backfillOutput,
    exportAs,
    openUrl,
    stop,
    urlFor,
    isStopping,
    subscribe,
  };
});
