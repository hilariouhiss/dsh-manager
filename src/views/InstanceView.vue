<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { save } from "@tauri-apps/plugin-dialog";

import ConfirmDialog from "../components/ConfirmDialog.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import OutputLog from "../components/OutputLog.vue";
import { invokeCmd } from "../ipc/client";
import type { AppError, InstanceInfo } from "../ipc/types";
import { useInstanceStore, type StopOutcome } from "../stores/instance";
import { formatDate, formatDuration } from "../utils/format";
import { asAppError, stopConfirmSpec } from "../utils/instance-view";

const store = useInstanceStore();

const argsText = ref("--port 3099");
const workDir = ref("");
const busy = ref(false);
const startError = ref<AppError | null>(null);
const stopError = ref<AppError | null>(null);
const savedHint = ref<string | null>(null);

interface ConfirmState {
  open: boolean;
  strength: "normal" | "strong" | "force";
  title: string;
  message: string;
}

const confirm = ref<ConfirmState>({
  open: false,
  strength: "normal",
  title: "",
  message: "",
});
let pendingStop: { instance: InstanceInfo; force: boolean } | null = null;
let lastStop: { instance: InstanceInfo; force: boolean } | null = null;

let off: (() => void) | null = null;

onMounted(async () => {
  off = await store.subscribe();
  // autoOpenBrowser 是设置项，进页面先读一次（FR-3.3）。
  await store.loadSettings();
  // 工作目录预填设置里的值；留空时后端也会回落（FR-3.1）。
  workDir.value = store.defaultWorkDir;
  await store.refresh();
});

onUnmounted(() => {
  // §3.5 订阅约束：不注销会导致输出重复渲染。
  off?.();
  off = null;
});

const canStart = computed(() => !busy.value);

async function start(): Promise<void> {
  if (busy.value) return;
  busy.value = true;
  startError.value = null;
  stopError.value = null;
  savedHint.value = null;
  try {
    const args = argsText.value.split(/\s+/).filter((s) => s.length > 0);
    // 新的一次运行：先清掉上一轮的行，避免与旧实例的输出混在一起。
    store.clear();
    const started = await invokeCmd<{ instanceId: string }>("start_instance", {
      args,
      workDir: workDir.value,
    });
    store.select(started.instanceId);
    await store.refresh();
  } catch (e) {
    startError.value = asAppError(e);
    // 启动失败也可能留下状态事件，列表以事实为准（不静默另起）。
    await store.refresh();
  } finally {
    busy.value = false;
  }
}

/** 停止前先按 D-3 定强度：外部实例必须走强化确认，强制终止必须单独确认。 */
function askStop(instance: InstanceInfo, force = false): void {
  pendingStop = { instance, force };
  confirm.value = { open: true, ...stopConfirmSpec(instance, force) };
}

function cancelConfirm(): void {
  pendingStop = null;
  confirm.value = { ...confirm.value, open: false };
}

async function doStop(): Promise<void> {
  const target = pendingStop;
  pendingStop = null;
  confirm.value = { ...confirm.value, open: false };
  if (target === null) return;
  await runStop(target.instance, target.force);
}

async function runStop(instance: InstanceInfo, force: boolean): Promise<void> {
  lastStop = { instance, force };
  stopError.value = null;
  savedHint.value = null;
  busy.value = true;
  try {
    const outcome = await store.stop(instance, force);
    // null = 已有一次停止正在进行，重复提交被挡住（防双击）。
    if (outcome !== null) handleStopOutcome(instance, outcome);
  } finally {
    busy.value = false;
  }
}

function handleStopOutcome(instance: InstanceInfo, outcome: StopOutcome): void {
  switch (outcome.kind) {
    case "stopped":
      stopError.value = null;
      return;
    case "forceRequired":
      // forced_stop_required 不是普通错误：必须弹单独的强制确认（§3.9 D-3）。
      askStop(instance, true);
      return;
    case "residual":
    case "error":
      stopError.value = outcome.error;
      return;
  }
}

/** 可重试的停止错误（instance.stop_timeout）：重新走一次确认，不自作主张直接强杀。 */
function retryStop(): void {
  if (lastStop === null) return;
  askStop(lastStop.instance, lastStop.force);
}

async function saveAs(): Promise<void> {
  savedHint.value = null;
  stopError.value = null;
  try {
    const target = await save({
      defaultPath: "dsh-instance-" + (store.selectedId ?? "output") + ".log",
      filters: [{ name: "日志文件", extensions: ["log", "txt"] }],
    });
    if (target === null) return;
    const bytes = await store.exportAs(target);
    savedHint.value = "已写入 " + bytes + " 字节：" + target;
  } catch (e) {
    stopError.value = asAppError(e);
  }
}

function startedText(instance: InstanceInfo): string {
  return instance.startedAtMs === null
    ? "无法取得"
    : formatDate(new Date(instance.startedAtMs).toISOString());
}

function urlOf(instance: InstanceInfo): string | null {
  return store.urlFor(instance);
}

function openUrlOf(instance: InstanceInfo): void {
  const url = urlOf(instance);
  if (url !== null) store.openUrl(url);
}
</script>

<template>
  <section class="flex flex-col gap-4">
    <header class="flex items-center justify-between gap-4">
      <h1 class="text-xl font-bold">运行实例</h1>
      <button type="button" class="btn btn-sm" :disabled="busy" @click="store.refresh()">
        刷新
      </button>
    </header>

    <ErrorAlert v-if="startError" :error="startError" @retry="start()" />
    <ErrorAlert v-if="stopError" :error="stopError" @retry="retryStop" />
    <ErrorAlert v-if="store.error" :error="store.error" @retry="store.refresh()" />
    <p v-if="savedHint" class="alert alert-success py-1 text-sm">{{ savedHint }}</p>

    <article class="card bg-base-100 shadow-sm">
      <div class="card-body gap-3">
        <h2 class="card-title text-base">启动</h2>
        <label class="form-control">
          <span class="label-text">启动参数（空格分隔）</span>
          <input v-model="argsText" class="input input-bordered input-sm" :disabled="busy" />
        </label>
        <label class="form-control">
          <span class="label-text">工作目录（留空用设置里的值）</span>
          <input v-model="workDir" class="input input-bordered input-sm" :disabled="busy" />
        </label>
        <button
          type="button"
          class="btn btn-primary btn-sm w-fit"
          :disabled="!canStart"
          @click="start"
        >
          {{ busy ? "处理中…" : "启动实例" }}
        </button>
        <p class="text-xs opacity-60">
          设置里的默认启动参数会先于这里填写的参数；外部实例永远不会被自动停止。
        </p>
      </div>
    </article>

    <article class="card bg-base-100 shadow-sm">
      <div class="card-body gap-3">
        <h2 class="card-title text-base">实例列表</h2>

        <p v-if="store.instances.length === 0" class="opacity-70" data-testid="no-instance">
          未运行
        </p>

        <div v-else class="overflow-x-auto">
          <table class="table table-sm">
            <thead>
              <tr>
                <th>实例</th>
                <th>归属</th>
                <th>pid</th>
                <th>启动时间</th>
                <th>运行时长</th>
                <th>访问地址</th>
                <th>操作</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="i in store.instances"
                :key="i.instanceId"
                :class="i.instanceId === store.selectedId ? 'bg-base-200' : ''"
                @click="store.select(i.instanceId)"
              >
                <td class="font-mono text-xs whitespace-nowrap">{{ i.instanceId }}</td>
                <td>
                  <span
                    :class="
                      i.managed
                        ? 'badge badge-primary badge-sm whitespace-nowrap'
                        : 'badge badge-warning badge-sm whitespace-nowrap'
                    "
                  >
                    {{ i.managed ? "本工具启动" : "外部实例" }}
                  </span>
                </td>
                <td class="whitespace-nowrap">{{ i.pid }}</td>
                <td class="whitespace-nowrap">{{ startedText(i) }}</td>
                <td class="whitespace-nowrap">{{ formatDuration(i.uptimeMs) }}</td>
                <td>
                  <a
                    v-if="urlOf(i)"
                    class="link link-primary font-mono text-xs"
                    href="#"
                    @click.prevent="openUrlOf(i)"
                    >{{ urlOf(i) }}</a
                  >
                  <span v-else class="opacity-60">无法取得</span>
                </td>
                <td>
                  <button
                    type="button"
                    class="btn btn-xs btn-error"
                    :disabled="busy || store.stoppingId !== null"
                    @click.stop="askStop(i)"
                  >
                    {{ store.isStopping(i.instanceId) ? "停止中…" : "停止" }}
                  </button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </article>

    <article v-if="store.selected" class="card bg-base-100 shadow-sm">
      <div class="card-body gap-3">
        <div v-if="store.externalSelected" class="alert alert-warning" data-testid="external-note">
          <span> 这是外部实例，本工具无法获取它的输出。如需查看输出，请用本工具启动实例。 </span>
        </div>

        <OutputLog
          v-else
          :lines="store.renderedLines"
          :paused="store.paused"
          :auto-scroll="store.autoScroll"
          :can-export="store.selected?.managed === true"
          @toggle-pause="store.togglePause()"
          @toggle-auto-scroll="store.setAutoScroll($event)"
          @clear="store.clear()"
          @save-as="saveAs"
        />
      </div>
    </article>

    <ConfirmDialog
      :open="confirm.open"
      :title="confirm.title"
      :message="confirm.message"
      :strength="confirm.strength"
      @confirm="doStop"
      @cancel="cancelConfirm"
    />
  </section>
</template>
