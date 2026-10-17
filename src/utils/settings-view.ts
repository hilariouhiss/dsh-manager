/** 设置视图的纯逻辑：自动安装通道切换的确认判定（FR-1.4）。 */

import type { Settings } from "../ipc/types";

/**
 * 通道下拉框改选时是否需要先弹确认。
 *
 * 只有「已经为某个通道开启自动安装，现在切到另一个通道」才需要确认；
 * 首次选择通道、重复选择、以及选「不自动安装」都不算切换（FR-1.4「切换时需确认」）。
 */
export function channelSwitchNeedsConfirm(
  current: Pick<Settings, "autoChannel" | "autoInstall">,
  nextChannel: string | null,
): boolean {
  return (
    current.autoInstall &&
    current.autoChannel !== null &&
    nextChannel !== null &&
    current.autoChannel !== nextChannel
  );
}
