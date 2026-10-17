import type { ChannelStatus, CheckUpdatesReport, InstallPhase, VersionVerdict } from "../ipc/types";

/** 版本源固定的包名（FR-1.9），与 Rust 侧 m1::channels::PACKAGE_NAME 同值。 */
export const PACKAGE_NAME = "@deepseek-ai/dsh";

export interface ChannelCard {
  channel: string;
  stateText: string;
  isError: boolean;
  /** 该通道当前可安装的版本；未发布或从未成功过时为 null。 */
  version: string | null;
  /** 版本适用、已发布、已知版本号且当前没有安装在进行时才允许发起安装。 */
  canInstall: boolean;
  versionText: string;
  publishedText: string | null;
  conclusion: string | null;
  errorReason: string | null;
  errorNextStep: string | null;
  retryable: boolean;
  lastSuccessText: string;
}

const STATE_TEXT: Record<ChannelStatus["state"], string> = {
  published: "已发布",
  unpublished: "未发布",
  failed: "检查失败",
};

const DATE_FORMAT = new Intl.DateTimeFormat("zh-CN", {
  dateStyle: "medium",
  timeStyle: "short",
});

function formatIso(iso: string | null): string | null {
  if (iso === null) return null;
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return null;
  return DATE_FORMAT.format(d);
}

function formatMs(ms: number | null): string {
  if (ms === null) return "尚无成功记录";
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "尚无成功记录";
  return DATE_FORMAT.format(d);
}

/** 版本号标签：**版本号旁必须标注它属于哪个包**（FR-1.9）。 */
export function versionLabel(version: string | null): string {
  if (version === null) return "未在本机检测到该包";
  return version + "（" + PACKAGE_NAME + " 包）";
}

/** 后端算好的结论 → 界面文案。
 *
 *  **前端不做版本比较**：`report.verdicts` 由 Rust 侧的 semver（T-V2）算出，
 *  FR-1.3 明确要求按语义化版本规范判断；前端再写一份就是第二个真源，
 *  而且预发布段很容易退化成字符串比较（`alpha.10 < alpha.9`，审计重要-8）。
 */
const VERDICT_TEXT: Partial<Record<VersionVerdict, string>> = {
  newer: "有新版本",
  up_to_date: "已是最新",
};

/** 一条通道的对比结论；后端没给结论时返回 null（不猜）。 */
export function conclusionText(report: CheckUpdatesReport, status: ChannelStatus): string | null {
  if (!report.versionApplicable) return null;
  if (status.state !== "published") return null;
  const verdict = report.verdicts[status.channel];
  return verdict === undefined ? null : (VERDICT_TEXT[verdict] ?? null);
}

/** 不适用说明：标明原因并指向来源识别页（FR-1.9、设计文档 §4.6）。 */
export function notApplicableText(report: CheckUpdatesReport): string | null {
  if (report.versionApplicable) return null;
  const reason = report.notApplicableReason ?? "无法确认本机 DSH 的安装归属。";
  // 后端给出的原因本身已可能带「安装来源」指引，不重复追加。
  return reason.includes("安装来源") ? reason : reason + "请到「安装来源」页查看判定依据。";
}

function lastSuccessTextOf(status: ChannelStatus): string {
  if (status.state !== "failed") return "";
  if (status.version === null || status.lastSuccessAtMs === null) return "尚无成功记录";
  return "上次成功：" + versionLabel(status.version) + "，" + formatMs(status.lastSuccessAtMs);
}

export function channelCards(report: CheckUpdatesReport, installing = false): ChannelCard[] {
  return report.channels.map((status) => {
    return {
      channel: status.channel,
      stateText: STATE_TEXT[status.state],
      isError: status.state === "failed",
      version: status.version,
      // 产品裁定：不适用时（官方安装器/便携版/无法识别）不提供 npm 安装入口，
      // 否则会给本机再造一份 DSH；安装中也只保留一个入口。
      canInstall:
        report.versionApplicable &&
        status.state === "published" &&
        status.version !== null &&
        !installing,
      versionText: status.state === "unpublished" ? "尚无可安装版本" : versionLabel(status.version),
      publishedText: formatIso(status.publishedAtIso),
      conclusion: conclusionText(report, status),
      errorReason: status.lastError?.reason ?? null,
      errorNextStep: status.lastError?.nextStep ?? null,
      retryable: status.lastError?.retryable ?? false,
      lastSuccessText: lastSuccessTextOf(status),
    };
  });
}

/** 确认弹窗的三档强度（§3.3.2 冻结：与 `ConfirmDialog.vue` 的 `strength` prop 同值）。 */
export type ConfirmLevel = "normal" | "strong" | "force";

export type ConfirmAction =
  | "install"
  | "switch-channel"
  | "stop-managed"
  | "stop-external"
  | "forced-stop";

/** 危险操作的分级（NFR-1「危险操作须二次确认」）。 */
export function confirmLevel(action: ConfirmAction): ConfirmLevel {
  switch (action) {
    // SDD §8.3：安装新版本是**普通**二次确认（可被设置关闭），不是强化档；
    // 强化档留给「停止外部实例」这类本工具无法负责的破坏性动作（审计重要-7）。
    case "install":
    case "switch-channel":
    case "stop-managed":
      return "normal";
    case "stop-external":
      return "strong";
    case "forced-stop":
      return "force";
  }
}

/** 安装前是否还需要用户点一次确认（FR-1.4：该确认可被用户关闭）。 */
export function shouldShowInstallConfirm(confirmBeforeInstall: boolean): boolean {
  return confirmBeforeInstall;
}

/** 取消按钮在取消真正无效时的提示（窗口：taskId 未知、已进入安装阶段）。 */
export const CANCEL_UNAVAILABLE_TEXT = "当前阶段无法取消";

/** 一次进行中的安装（与 store 的 installing 同形；taskId 由首个进度事件补全）。 */
export interface InstallingState {
  taskId: string | null;
  percent: number;
  remainingMs: number | null;
  phase: InstallPhase;
}

/**
 * 取消只在真正有效的阶段提供：首个 task:progress 之前 taskId 未知；
 * 进入 installing 阶段后后端已注销任务（cancel_task 只会返回 accepted=false）。
 */
export function canCancelInstall(installing: InstallingState | null): boolean {
  return installing !== null && installing.taskId !== null && installing.phase !== "installing";
}
