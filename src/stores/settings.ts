// 设置状态（U1）：全部设置项 + 自启状态。
// 设置项的键名与默认值来自主计划 §3.7（冻结）；命令名与入参来自 §3.4（冻结）。

import { defineStore } from "pinia";
import { ref } from "vue";

import { invokeCmd, isAppError } from "../ipc/client";
import type { AppError, AutostartMode, AutostartState, OplogEntry, Settings } from "../ipc/types";

/** 数值项的取值区间（§3.7 校验规则）。后端用同一套区间兜底，两侧必须一致。 */
export const NUMERIC_LIMITS = {
  outputBufferLines: { min: 1, max: 100000, unit: "行" },
  logMaxMiB: { min: 1, max: 1024, unit: "MiB" },
  logKeepFiles: { min: 1, max: 50, unit: "份" },
  oplogMaxMiB: { min: 1, max: 1024, unit: "MiB" },
  oplogKeepFiles: { min: 1, max: 50, unit: "份" },
} as const;

export type NumericKey = keyof typeof NUMERIC_LIMITS;

/** 通道下拉框的取值：通道标识与中文名照主计划 §3.8 D-1 的冻结表。 */
export const CHANNEL_OPTIONS: ReadonlyArray<{ id: string; label: string }> = [
  { id: "latest", label: "稳定通道" },
  { id: "next", label: "预览通道" },
  { id: "alpha", label: "先行通道" },
];

/** 设置项在界面上的呈现方式。 */
export type SettingKind = "boolean" | "number" | "text" | "lines" | "channel" | "autostartMode";

export interface SettingItem {
  key: keyof Settings;
  label: string;
  kind: SettingKind;
  /** 这一项影响哪条需求，界面上原样显示 */
  requirement: string;
}

export interface SettingGroup {
  id: string;
  title: string;
  items: SettingItem[];
}

/** 五个设置分组、§3.7 全表 14 项。「诊断」组不含设置项，由视图直接渲染。 */
export const SETTINGS_GROUPS: ReadonlyArray<SettingGroup> = [
  {
    id: "startup",
    title: "启动",
    items: [
      { key: "launchArgs", label: "启动参数", kind: "lines", requirement: "FR-3.1、FR-3.3" },
      { key: "workDir", label: "工作目录", kind: "text", requirement: "FR-3.1" },
      { key: "autoOpenBrowser", label: "自动打开浏览器", kind: "boolean", requirement: "FR-3.3" },
      {
        key: "stopInstanceOnExit",
        label: "退出时停止本工具启动的实例",
        kind: "boolean",
        requirement: "FR-4.4",
      },
    ],
  },
  {
    id: "display",
    title: "显示",
    items: [
      { key: "autoScroll", label: "输出自动滚动", kind: "boolean", requirement: "FR-3.2" },
      { key: "outputBufferLines", label: "输出缓冲行数", kind: "number", requirement: "NFR-5" },
    ],
  },
  {
    id: "logs",
    title: "日志",
    items: [
      { key: "logMaxMiB", label: "单个日志文件上限", kind: "number", requirement: "NFR-5" },
      { key: "logKeepFiles", label: "日志保留份数", kind: "number", requirement: "NFR-5" },
      { key: "oplogMaxMiB", label: "操作记录文件上限", kind: "number", requirement: "NFR-5" },
      { key: "oplogKeepFiles", label: "操作记录保留份数", kind: "number", requirement: "NFR-5" },
    ],
  },
  {
    id: "update",
    title: "更新",
    items: [
      { key: "autoChannel", label: "自动安装通道", kind: "channel", requirement: "FR-1.4" },
      { key: "autoInstall", label: "发现新版本时自动安装", kind: "boolean", requirement: "FR-1.4" },
      {
        key: "confirmBeforeInstall",
        label: "安装前二次确认",
        kind: "boolean",
        requirement: "FR-1.4",
      },
    ],
  },
  {
    id: "autostart",
    title: "开机自启",
    items: [
      { key: "autostartMode", label: "启动行为", kind: "autostartMode", requirement: "FR-5.2" },
    ],
  },
];

/** §3.7 的 14 个键，供一致性测试与界面自检使用。 */
export const SETTINGS_KEYS: ReadonlyArray<keyof Settings> = [
  "autoChannel",
  "autoInstall",
  "confirmBeforeInstall",
  "launchArgs",
  "workDir",
  "autoOpenBrowser",
  "autoScroll",
  "outputBufferLines",
  "logMaxMiB",
  "logKeepFiles",
  "oplogMaxMiB",
  "oplogKeepFiles",
  "stopInstanceOnExit",
  "autostartMode",
];

export function isNumericKey(key: keyof Settings): key is NumericKey {
  return Object.prototype.hasOwnProperty.call(NUMERIC_LIMITS, key);
}

/** 数值项校验：越界与非法输入都给出「原因 + 下一步」（NFR-1）。 */
export function validateNumeric(
  key: NumericKey,
  value: number,
): { ok: true } | { ok: false; message: string } {
  const limit = NUMERIC_LIMITS[key];
  const range = String(limit.min) + "–" + String(limit.max) + " " + limit.unit;
  if (!Number.isInteger(value)) {
    return {
      ok: false,
      message: "只能填整数（单位：" + limit.unit + "）：请填 " + range + " 之间的整数。",
    };
  }
  if (value < limit.min || value > limit.max) {
    return {
      ok: false,
      message: "超出允许范围（允许 " + range + "）：请改成范围内的值；后端会用同一区间兜底。",
    };
  }
  return { ok: true };
}

/**
 * 补丁里**没按请求生效**的字段。
 *
 * §3.7 规定越界与空白值由后端回落默认值，`update_settings` 返回的是**实际生效**的设置；
 * 把两者比一比，界面就能把「哪些项没按输入生效」如实说出来，而不是静默显示后端值（NFR-1）。
 */
export function rejectedKeys(patch: Partial<Settings>, effective: Settings): string[] {
  const requested = patch as Record<string, unknown>;
  const actual = effective as unknown as Record<string, unknown>;
  return Object.keys(requested).filter(
    (key) => JSON.stringify(requested[key]) !== JSON.stringify(actual[key]),
  );
}

/**
 * 把 catch 到的未知值收成 AppError。
 *
 * IPC 按契约只会抛 AppError；万一抛出别的东西，也要给出中文原因与下一步（NFR-1），
 * 不能像以前的 `err as AppError` 那样让界面显示 undefined。
 */
function asAppError(err: unknown): AppError {
  if (isAppError(err)) return err;
  return {
    code: "internal.unexpected",
    reason: "操作未能完成：发生了未预期的错误，界面可能不是最新状态。",
    nextStep: "请点「刷新」重试；若反复出现，请导出诊断包并附上其中的运行日志。",
    raw: err instanceof Error ? err.message : String(err),
    retryable: true,
  };
}

export const useSettingsStore = defineStore("settings", () => {
  const settings = ref<Settings | null>(null);
  const autostart = ref<AutostartState | null>(null);
  /** 自启状态的读取失败单独存放：它不得被后续成功的其它动作清掉（FR-5.3）。 */
  const autostartError = ref<AppError | null>(null);
  const oplogEntries = ref<OplogEntry[]>([]);
  const oplogTotal = ref(0);
  const error = ref<AppError | null>(null);
  /** 前端校验拦下的输入提示（不写文件、不发请求）。 */
  const notSavedHint = ref<string | null>(null);

  function remember(err: unknown): void {
    error.value = asAppError(err);
  }

  async function readSettings(): Promise<void> {
    try {
      settings.value = await invokeCmd<Settings>("get_settings", {});
    } catch (err) {
      remember(err);
    }
  }

  /** 设置与系统自启各自读、各自失败：一边读不到不影响另一边显示。 */
  async function load(): Promise<void> {
    error.value = null;
    notSavedHint.value = null;
    await Promise.all([readSettings(), refreshAutostart()]);
  }

  /**
   * 只读刷新系统自启状态（FR-5.3：系统里改了状态，界面跟上）。
   * 读取成功才清除失败状态；失败期间开关保持禁用，不显示成「已关闭」。
   */
  async function refreshAutostart(): Promise<void> {
    try {
      autostart.value = await invokeCmd<AutostartState>("get_autostart_state", {});
      autostartError.value = null;
    } catch (err) {
      autostartError.value = asAppError(err);
    }
  }

  /** 只把被改的字段放进补丁（§3.4 命令 12 的 patch 语义）。 */
  async function update(patch: Partial<Settings>): Promise<void> {
    error.value = null;
    notSavedHint.value = null;
    try {
      // 返回的是**实际生效**的设置：后端越界/空白时已回落默认值（§3.7）
      const effective = await invokeCmd<Settings>("update_settings", { patch });
      settings.value = effective;
      const rejected = rejectedKeys(patch, effective);
      if (rejected.length > 0) {
        // 不静默成功：把「哪些项没按输入生效」和下一步说清楚（NFR-1）
        notSavedHint.value =
          "以下设置未按输入生效，已回落为后端校验后的值：" +
          rejected.join("、") +
          "。请重新填写后保存；数值项允许的范围见输入框下方。";
      }
    } catch (err) {
      remember(err);
    }
  }

  /** 数值项：先按 §3.7 的区间校验，越界只提示、不发请求（后端另有同一套兜底）。 */
  async function setNumber(key: NumericKey, value: number): Promise<boolean> {
    const check = validateNumeric(key, value);
    if (!check.ok) {
      error.value = null;
      notSavedHint.value = check.message;
      return false;
    }
    await update({ [key]: value } as Partial<Settings>);
    return true;
  }

  async function setAutostartEnabled(enabled: boolean): Promise<void> {
    // 状态读取失败时沿用设置里的启动行为（设置是后端快照，仍可信），不静默改回托盘
    await writeAutostart(enabled, autostart.value?.mode ?? settings.value?.autostartMode ?? "tray");
  }

  /** 启动行为：把当前**系统状态**原样带上，改行为不得把自启打开或关掉。 */
  async function setAutostartMode(mode: AutostartMode): Promise<void> {
    if (autostart.value === null) {
      await refreshAutostart();
      if (autostart.value === null) {
        // 拿不到系统真实状态就不动系统（FR-5.3：不许猜）
        return;
      }
    }
    await writeAutostart(autostart.value.enabled, mode);
  }

  async function writeAutostart(enabled: boolean, mode: AutostartMode): Promise<void> {
    error.value = null;
    notSavedHint.value = null;
    try {
      // 命令 14 是「开机自启」这一组的唯一写入口：它先落盘启动行为，再写系统登录项
      const state = await invokeCmd<AutostartState>("set_autostart", { enabled, mode });
      autostart.value = state;
      // 命令 14 能返回状态，说明系统登录项这次读得到：清掉之前的读取失败状态。
      autostartError.value = null;
      // 命令 14 的出参形状由 §3.4 冻结，没有放「校验问题列表」的位置：
      // 用「请求值 vs 实际生效值」把回落显示出来，原始中文说明由后端记进运行日志（NFR-1）。
      const hints: string[] = [];
      if (state.mode !== mode) {
        hints.push(
          "启动行为未按所选生效，已回落为「" +
            (state.mode === "tray" ? "最小化到托盘" : "打开主窗口") +
            "」。请重新选择并保存；若反复回落，请导出诊断包并查看其中的运行日志。",
        );
      }
      if (state.enabled !== enabled) {
        hints.push(
          "系统未接受该更改：请求" +
            (enabled ? "开启" : "关闭") +
            "开机自启，但系统登录项仍为" +
            (state.enabled ? "开启" : "关闭") +
            "。请检查账户权限后重试；若反复出现请导出诊断包。",
        );
      }
      notSavedHint.value = hints.length > 0 ? hints.join(" ") : null;
      settings.value = await invokeCmd<Settings>("get_settings", {});
    } catch (err) {
      remember(err);
      // 写入系统失败后登录项可能已部分改变：重读系统真实状态（FR-5.3），
      // 不把失败当成「已关闭」。重读失败记进独立的读取失败状态，本次写入原因仍留在 error 里。
      await refreshAutostart();
      try {
        // 命令 14 先落盘启动行为再写系统：系统写入失败时 mode 可能已经生效
        settings.value = await invokeCmd<Settings>("get_settings", {});
      } catch {
        // 同上：设置重读失败不覆盖本次写入的失败原因
      }
    }
  }

  /** 视图的统一入口：v-for 出来的键值对走这里，autostartMode 被路由到命令 14。 */
  async function setValue(
    key: keyof Settings,
    value: boolean | number | string | string[] | AutostartMode | null,
  ): Promise<void> {
    if (key === "autostartMode") {
      await setAutostartMode(value as AutostartMode);
      return;
    }
    if (isNumericKey(key)) {
      await setNumber(key, Number(value));
      return;
    }
    await update({ [key]: value } as Partial<Settings>);
  }

  async function exportDiagnostics(targetPath: string): Promise<number | null> {
    error.value = null;
    try {
      const result = await invokeCmd<{ bytesWritten: number }>("export_diagnostics", {
        targetPath,
      });
      return result.bytesWritten;
    } catch (err) {
      remember(err);
      return null;
    }
  }

  async function loadOplog(offset = 0, limit = 50): Promise<void> {
    error.value = null;
    try {
      const page = await invokeCmd<{ entries: OplogEntry[]; total: number }>("list_oplog", {
        offset,
        limit,
      });
      oplogEntries.value = page.entries;
      oplogTotal.value = page.total;
    } catch (err) {
      remember(err);
    }
  }

  return {
    settings,
    autostart,
    autostartError,
    oplogEntries,
    oplogTotal,
    error,
    notSavedHint,
    load,
    refreshAutostart,
    update,
    setNumber,
    setValue,
    setAutostartEnabled,
    setAutostartMode,
    exportDiagnostics,
    loadOplog,
  };
});
