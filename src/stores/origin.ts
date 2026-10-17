import { computed, ref } from "vue";
import { defineStore } from "pinia";

import { invokeCmd, isAppError } from "../ipc/client";
import type { AppError, OriginReport, OriginVerdict } from "../ipc/types";

/** 四值结论的中文文案（FR-2.1）。包管理器名称取自后端的结论对象，不写死 npm。 */
export function verdictLabel(verdict: OriginVerdict): string {
  if (typeof verdict !== "string") {
    return "包管理器安装（" + verdict.packageManager + "）";
  }
  switch (verdict) {
    case "officialInstaller":
      return "官方安装器安装";
    case "portable":
      return "便携版";
    case "unidentified":
      return "未识别";
  }
}

/** 结论下面那句解释：未识别时说清「不猜」，非包管理器安装时说清「版本管理不适用」。 */
export function verdictDetail(verdict: OriginVerdict): string {
  if (typeof verdict !== "string") {
    return "本机 DSH 由 " + verdict.packageManager + " 全局安装，本工具的版本管理适用于这份安装。";
  }
  switch (verdict) {
    case "officialInstaller":
      return "本机 DSH 由官方安装器安装，不属于 NPM 包 @deepseek-ai/dsh 的包管理器安装；本工具的版本管理对该安装方式不适用。";
    case "portable":
      return "本机 DSH 不在任何包管理器全局目录下，看起来是便携版；本工具的版本管理对该安装方式不适用。";
    case "unidentified":
      return "依据不足，无法确认本机 DSH 的安装方式。本工具不会猜测结论。";
  }
}

/** 缺什么信息（FR-2.3）：缺失项为空时也要明确说明取证来源都可用。 */
export function missingHint(missing: string[]): string {
  return missing.length === 0 ? "所有取证来源都可用。" : "还缺以下信息：" + missing.join("；");
}

export const useOriginStore = defineStore("origin", () => {
  const report = ref<OriginReport | null>(null);
  const loading = ref(false);
  const error = ref<AppError | null>(null);
  /** 依据清单默认收起，用户点击后展开（E2E-3）。 */
  const expanded = ref(false);

  const verdictText = computed(() =>
    report.value === null ? "尚未识别" : verdictLabel(report.value.verdict),
  );
  const detailText = computed(() =>
    report.value === null ? "" : verdictDetail(report.value.verdict),
  );
  const missingText = computed(() =>
    report.value === null ? "" : missingHint(report.value.missing),
  );
  const showMissing = computed(() => (report.value?.missing.length ?? 0) > 0);

  /** 失败时不编造结论：只记错误，report 保持原样（原则 P4）。 */
  async function load(): Promise<void> {
    loading.value = true;
    error.value = null;
    try {
      report.value = await invokeCmd<OriginReport>("detect_origin", {});
    } catch (e) {
      error.value = isAppError(e) ? e : null;
    } finally {
      loading.value = false;
    }
  }

  function toggleExpanded(): void {
    expanded.value = !expanded.value;
  }

  return {
    report,
    loading,
    error,
    expanded,
    verdictText,
    detailText,
    missingText,
    showMissing,
    load,
    toggleExpanded,
  };
});
