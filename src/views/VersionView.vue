<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { CircleAlert, CircleCheck, RefreshCw, TriangleAlert } from "@lucide/vue";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import TaskProgress from "../components/TaskProgress.vue";
import { isAppError } from "../ipc/client";
import type { AppError } from "../ipc/types";
import { useVersionStore } from "../stores/version";
import {
  CANCEL_UNAVAILABLE_TEXT,
  canCancelInstall,
  channelCards,
  confirmLevel,
  notApplicableText,
  shouldShowInstallConfirm,
  versionLabel,
  type ChannelCard,
} from "../utils/version-view";

const store = useVersionStore();
const reportError = ref<AppError | null>(null);
const refreshing = ref(false);
const pendingChannel = ref<string | null>(null);
const pendingInstall = ref<{ channel: string; version: string } | null>(null);
/** 最近一次安装目标：失败后「重试」按钮要用（自动安装失败时回落到 update:available）。 */
const lastInstall = ref<{ channel: string; version: string } | null>(null);
/** cancel_task 不接受时的提示（后端已注销任务的阶段）。 */
const cancelNotice = ref<string | null>(null);

const installing = computed(() => store.installing !== null);
const cards = computed(() =>
  store.report === null ? [] : channelCards(store.report, installing.value),
);
const notApplicable = computed(() =>
  store.report === null ? null : notApplicableText(store.report),
);
const localLabel = computed(() =>
  store.report === null ? "检查中……" : versionLabel(store.report.localVersion),
);

onMounted(() => {
  void initialize();
});

onUnmounted(() => {
  // §3.5 订阅约束：不注销会导致重复渲染
  store.stopListening();
});

// 自动安装提示：needsConfirm=true 时必须由用户点一次确认；false 由 store 直接开始安装。
watch(
  () => store.available,
  (payload) => {
    if (payload !== null && payload.needsConfirm) {
      pendingInstall.value = { channel: payload.channel, version: payload.version };
    }
  },
);

// 安装开始或结束时清掉上一轮的「无法取消」提示。
watch(installing, () => {
  cancelNotice.value = null;
});

async function initialize(): Promise<void> {
  try {
    // 订阅先于设置：设置读取失败不应连检查结果一起丢掉（NFR-7）。
    await store.startListening();
    await store.loadSettings();
  } catch (e) {
    if (isAppError(e)) reportError.value = e;
  }
  await loadReport();
}

async function loadReport(): Promise<void> {
  // 安装期间不重复拉检查结果：进度与结论以安装结束后的刷新为准。
  if (refreshing.value || installing.value) return;
  refreshing.value = true;
  try {
    await store.refresh();
    reportError.value = null;
  } catch (e) {
    if (isAppError(e)) reportError.value = e;
  } finally {
    refreshing.value = false;
  }
}

function isAutoChannel(channel: string): boolean {
  return store.settings.autoInstall && store.settings.autoChannel === channel;
}

async function onToggleChannel(channel: string): Promise<void> {
  const request = store.requestChannelToggle(channel);
  if (request.needsConfirm) {
    pendingChannel.value = channel;
    return;
  }
  await setAutoChannel(channel);
}

async function setAutoChannel(channel: string): Promise<void> {
  try {
    await store.confirmChannelToggle(channel);
    reportError.value = null;
  } catch (e) {
    if (isAppError(e)) reportError.value = e;
  }
}

async function confirmChannel(): Promise<void> {
  const channel = pendingChannel.value;
  pendingChannel.value = null;
  if (channel !== null) {
    await setAutoChannel(channel);
  }
}

function onInstall(card: ChannelCard): void {
  if (card.version === null) return;
  const target = { channel: card.channel, version: card.version };
  if (shouldShowInstallConfirm(store.settings.confirmBeforeInstall)) {
    pendingInstall.value = target;
    return;
  }
  void runInstall(target);
}

async function confirmInstall(): Promise<void> {
  const target = pendingInstall.value;
  pendingInstall.value = null;
  if (target !== null) {
    await runInstall(target);
  }
}

async function runInstall(target: { channel: string; version: string }): Promise<void> {
  lastInstall.value = target;
  try {
    const taskId = await store.startInstall(target.channel, target.version);
    if (taskId === null) {
      // 已有安装在跑：这次重复发起被忽略，不能用它顶掉正在展示的进度/取消目标
      lastInstall.value = null;
      return;
    }
    // 安装成功：清掉重试目标，再刷新一次检查结果（本机版本与结论可能已变化）
    lastInstall.value = null;
    await loadReport();
  } catch {
    // 失败已落进 store.installError（download.cancelled 不算失败），统一由页面展示
  }
}

function retryInstall(): void {
  const target =
    lastInstall.value ??
    (store.available === null
      ? null
      : { channel: store.available.channel, version: store.available.version });
  if (target !== null) {
    void runInstall(target);
  }
}

async function cancelInstall(): Promise<void> {
  cancelNotice.value = null;
  try {
    if (!(await store.cancelInstall())) {
      // taskId 未知或后端已注销任务：如实告诉用户这个阶段取消不了
      cancelNotice.value = CANCEL_UNAVAILABLE_TEXT;
    }
  } catch {
    // 失败已落进 store.installError，页面统一展示
  }
}
</script>

<template>
  <section class="flex flex-col gap-4">
    <header class="flex items-center justify-between">
      <h1 class="text-xl font-bold">版本与更新</h1>
      <button class="btn btn-sm" :disabled="refreshing || installing" @click="loadReport()">
        <RefreshCw class="size-4" aria-hidden="true" />
        {{ refreshing ? "正在检查……" : "检查更新" }}
      </button>
    </header>

    <div class="alert">
      <span>本机版本：{{ localLabel }}</span>
    </div>

    <div v-if="notApplicable !== null" class="alert alert-info">
      <CircleCheck class="size-5 shrink-0" aria-hidden="true" />
      <span>{{ notApplicable }}</span>
      <RouterLink class="link" to="/origin">去「安装来源」页查看判定依据</RouterLink>
    </div>

    <ErrorAlert v-if="reportError !== null" :error="reportError" @retry="loadReport()" />

    <p v-if="store.report === null && reportError === null" class="text-sm opacity-70">
      正在检查更新……
    </p>

    <article v-for="card in cards" :key="card.channel" class="card bg-base-200">
      <div class="card-body">
        <h2 class="card-title">
          {{ card.channel }}
          <span class="badge" :class="card.isError ? 'badge-error' : 'badge-ghost'">
            {{ card.stateText }}
          </span>
        </h2>
        <p>{{ card.versionText }}</p>
        <p v-if="card.publishedText !== null" class="text-sm opacity-70">
          发布日期：{{ card.publishedText }}
        </p>
        <p v-if="card.conclusion !== null" class="font-semibold">{{ card.conclusion }}</p>

        <div v-if="card.isError" class="alert alert-error items-start" role="alert">
          <CircleAlert class="size-5 shrink-0" aria-hidden="true" />
          <div>
            <p>原因：{{ card.errorReason }}</p>
            <p class="text-sm">下一步：{{ card.errorNextStep }}</p>
            <p v-if="card.lastSuccessText !== ''" class="text-sm">{{ card.lastSuccessText }}</p>
          </div>
        </div>

        <!-- 产品裁定：versionApplicable=false 时不渲染安装入口（含自动安装通道） -->
        <div v-if="store.report?.versionApplicable" class="card-actions items-center">
          <button
            class="btn btn-sm"
            :disabled="installing || isAutoChannel(card.channel)"
            @click="onToggleChannel(card.channel)"
          >
            {{ isAutoChannel(card.channel) ? "已开启自动安装" : "对该通道自动安装" }}
          </button>
          <button v-if="card.canInstall" class="btn btn-sm btn-primary" @click="onInstall(card)">
            安装这个版本
          </button>
        </div>
      </div>
    </article>

    <TaskProgress
      v-if="store.installing !== null"
      :task-id="store.installing.taskId ?? ''"
      :percent="store.installing.percent"
      :remaining-ms="store.installing.remainingMs"
      :phase="store.installing.phase"
      :cancellable="canCancelInstall(store.installing)"
      @cancel="cancelInstall()"
    />

    <p v-if="cancelNotice !== null" class="text-sm text-warning" role="status">
      {{ cancelNotice }}
    </p>

    <ErrorAlert
      v-if="store.installError !== null"
      :error="store.installError"
      @retry="retryInstall()"
    />

    <div class="alert">
      <TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
      <span>{{ store.coexistenceNotice }}</span>
    </div>

    <ConfirmDialog
      :open="pendingChannel !== null"
      :strength="confirmLevel('switch-channel')"
      title="切换自动安装通道"
      :message="`切换后，之前开启自动安装的通道会被关闭。当前选择：${store.settings.autoChannel ?? '无'}`"
      @confirm="confirmChannel()"
      @cancel="pendingChannel = null"
    />

    <ConfirmDialog
      :open="pendingInstall !== null"
      :strength="confirmLevel('install')"
      title="安装新版本"
      :message="`将安装 ${pendingInstall?.version ?? ''}（@deepseek-ai/dsh 包）。安装会替换本机已装的同名包，安装期间 DSH 不能运行。`"
      @confirm="confirmInstall()"
      @cancel="pendingInstall = null"
    />
  </section>
</template>
