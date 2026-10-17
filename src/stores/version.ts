import { defineStore } from "pinia";
import { invokeCmd, on } from "../ipc/client";
import {
  EVENTS,
  type CheckUpdatesReport,
  type Settings,
  type UpdateAvailablePayload,
} from "../ipc/types";

export interface ChannelToggleRequest {
  needsConfirm: boolean;
  channel: string;
}

export const useVersionStore = defineStore("version", {
  state: () => ({
    report: null as CheckUpdatesReport | null,
    settings: {
      autoChannel: null as string | null,
      autoInstall: false,
      confirmBeforeInstall: true,
    },
    /** 后端推来的 update:available，界面据此决定是直接安装还是先确认 */
    available: null as UpdateAvailablePayload | null,
    /** 本次会话已经确认过的通道，避免重复弹窗 */
    confirmedChannels: [] as string[],
  }),

  actions: {
    /** 切换通道：需要确认时只返回请求，**不调用后端**（FR-1.4）。 */
    requestChannelToggle(channel: string): ChannelToggleRequest {
      const needsConfirm = this.settings.autoInstall && this.settings.autoChannel !== channel;
      return { needsConfirm, channel };
    },

    /** 用户确认后写设置；写回的值以后端返回为准。 */
    async confirmChannelToggle(channel: string): Promise<void> {
      const updated = await invokeCmd<Settings>("set_auto_channel", {
        channel,
        enabled: true,
      });
      this.settings = {
        autoChannel: updated.autoChannel,
        autoInstall: updated.autoInstall,
        confirmBeforeInstall: updated.confirmBeforeInstall,
      };
      if (!this.confirmedChannels.includes(channel)) {
        this.confirmedChannels.push(channel);
      }
    },

    /** 注册 update:available 监听；返回注销函数（§3.5 订阅约束）。 */
    async listenForUpdates(): Promise<() => void> {
      return on<UpdateAvailablePayload>(EVENTS.EVENT_UPDATE_AVAILABLE, (payload) => {
        this.available = payload;
      });
    },

    /** 该通道的更新是否直接进入安装（不需要再弹确认）。 */
    shouldAutoInstall(): boolean {
      return this.available !== null && this.available.needsConfirm === false;
    },
  },
});
