import { defineStore } from "pinia";
import { invokeCmd, isAppError, on } from "../ipc/client";
import {
  ERROR_CODES,
  EVENTS,
  type AppError,
  type CheckUpdatesReport,
  type Settings,
  type TaskProgressPayload,
  type UpdateAvailablePayload,
} from "../ipc/types";
import type { InstallingState } from "../utils/version-view";

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
    /** 版本视图固定请求的三条通道（与 Rust 侧 m1::channels::CHANNELS 的 id 同值）。 */
    channelIds: ["latest", "next", "alpha"] as string[],
    /**
     * 进行中的安装。后端 start_install 在下载+安装全部结束后才返回 taskId，
     * 因此 taskId 先为 null，由首个 task:progress 事件或命令返回补上。
     */
    installing: null as null | InstallingState,
    /** 最近一次安装/取消失败：界面直接展示原因 + 下一步（NFR-1）。 */
    installError: null as AppError | null,
    /** 事件监听是否已注册：重复挂载不得叠加监听（§3.5 订阅约束）。 */
    listening: false,
    unlisteners: [] as Array<() => void>,
    /** FR-1.7 的提示文案：本工具不修改 DSH 自身的更新设置 */
    coexistenceNotice:
      "DSH 自身可能也具备更新能力，两者可能互相覆盖：DSH 更新后本工具记录的版本会随之变化，" +
      "本工具选择的通道也可能被 DSH 自己的更新改回。本工具不会修改 DSH 自身的更新设置。",
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

    /** 拉一次检查更新并落进 store（E2E-1：冷启动即出结论）。 */
    async refresh(): Promise<void> {
      this.report = await invokeCmd<CheckUpdatesReport>("check_updates", {
        channels: this.channelIds,
      });
    },

    /** 读取版本视图需要的三个设置项（其余设置项归「设置」页）。 */
    async loadSettings(): Promise<void> {
      const full = await invokeCmd<Settings>("get_settings", {});
      this.settings = {
        autoChannel: full.autoChannel,
        autoInstall: full.autoInstall,
        confirmBeforeInstall: full.confirmBeforeInstall,
      };
    },

    /** 发起安装；进度由 task:progress 事件推进，成功后由调用方刷新结论。
     *
     *  同一时间只允许一个安装：并发安装会覆盖进度、让取消打到错误的任务上，
     *  还会在全局范围重复装同一份包。已有安装时直接忽略并返回 null。
     */
    async startInstall(channel: string, version: string): Promise<string | null> {
      if (this.installing !== null) {
        return null;
      }
      this.installError = null;
      this.installing = { taskId: null, percent: 0, remainingMs: null, phase: "checking" };
      try {
        const r = await invokeCmd<{ taskId: string }>("start_install", {
          channel,
          version,
        });
        return r.taskId;
      } catch (e) {
        // 用户主动取消不是失败（NFR-2），不显示成红色错误。
        this.installError = isAppError(e) && e.code !== ERROR_CODES.DOWNLOAD_CANCELLED ? e : null;
        throw e;
      } finally {
        this.installing = null;
      }
    },

    /** 取消安装（NFR-2）。taskId 尚未知时无法定位任务，如实返回 false。 */
    async cancelInstall(): Promise<boolean> {
      const task = this.installing;
      if (task === null || task.taskId === null) {
        return false;
      }
      try {
        const r = await invokeCmd<{ accepted: boolean }>("cancel_task", {
          taskId: task.taskId,
        });
        if (r.accepted) {
          this.installing = null;
        }
        return r.accepted;
      } catch (e) {
        this.installError = isAppError(e) ? e : null;
        throw e;
      }
    },

    /** 注册 task:progress；返回注销函数（§3.5 订阅约束）。 */
    async listenForProgress(): Promise<() => void> {
      return on<TaskProgressPayload>(EVENTS.EVENT_TASK_PROGRESS, (p) => {
        const current = this.installing;
        if (current === null) {
          return;
        }
        // taskId 未知时接受首个事件并记下（取消按钮需要它）；已知时只认同一个任务。
        if (current.taskId !== null && current.taskId !== p.taskId) {
          return;
        }
        this.installing = {
          taskId: current.taskId ?? p.taskId,
          percent: p.percent,
          remainingMs: p.remainingMs,
          phase: p.phase,
        };
      });
    },

    /** 注册 update:available 监听；返回注销函数（§3.5 订阅约束）。 */
    async listenForUpdates(): Promise<() => void> {
      return on<UpdateAvailablePayload>(EVENTS.EVENT_UPDATE_AVAILABLE, (payload) => {
        this.available = payload;
        // needsConfirm=false（用户关闭了安装前确认）时直接进入安装（FR-1.4）。
        // 安装进行中不得再起第二个安装。
        if (payload.needsConfirm || this.installing !== null) {
          return;
        }
        void (async () => {
          try {
            await this.startInstall(payload.channel, payload.version);
          } catch {
            // 失败已落进 installError，自动安装没有可点的发起入口，界面统一展示它。
          }
          // 成功时本机版本已变、失败时结论也应保持最新，两条路径都刷新。
          await this.refresh().catch(() => {});
        })();
      });
    },

    /** 视图挂载时一次性注册全部事件；重复调用不会叠加监听。 */
    async startListening(): Promise<void> {
      if (this.listening) {
        return;
      }
      this.listening = true;
      this.unlisteners.push(await this.listenForUpdates());
      this.unlisteners.push(await this.listenForProgress());
    },

    /** 视图卸载时注销全部事件（§3.5：不注销会导致重复渲染）。 */
    stopListening(): void {
      for (const off of this.unlisteners) {
        off();
      }
      this.unlisteners = [];
      this.listening = false;
    },

    /** 该通道的更新是否直接进入安装（不需要再弹确认）。 */
    shouldAutoInstall(): boolean {
      return this.available !== null && this.available.needsConfirm === false;
    },
  },
});
