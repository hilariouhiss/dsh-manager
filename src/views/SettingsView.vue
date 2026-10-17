<script setup lang="ts">
import { computed, onMounted, ref, shallowRef } from "vue";
import { Download, RefreshCw, ScrollText, ShieldCheck } from "@lucide/vue";
import { save } from "@tauri-apps/plugin-dialog";

import ConfirmDialog from "../components/ConfirmDialog.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import {
  CHANNEL_OPTIONS,
  NUMERIC_LIMITS,
  SETTINGS_GROUPS,
  useSettingsStore,
  type NumericKey,
  type SettingItem,
} from "../stores/settings";
import { syncChecked } from "../utils/dom";
import { channelSwitchNeedsConfirm } from "../utils/settings-view";
import type { AutostartMode, Settings } from "../ipc/types";

const store = useSettingsStore();

/** 数值输入框的草稿（字符串），失焦时才解析并提交。 */
const drafts = ref<Record<string, string>>({});
/** 每个数值项当前的前端校验提示。 */
const draftHints = ref<Record<string, string>>({});
const oplogVisible = ref(false);
/** 操作记录一页 20 条；诊断组按页读取，不一次拉全量（NFR-5）。 */
const oplogPageSize = 20;
const oplogPage = ref(0);
const exportHint = ref<string | null>(null);
/** 待确认的通道切换：确认前不改写设置，取消时把下拉框拨回原值（FR-1.4）。 */
const pendingChannel = shallowRef<{ channel: string; select: HTMLSelectElement } | null>(null);

onMounted(() => {
  void store.load();
});

function currentValue(key: keyof Settings): unknown {
  if (store.settings === null) return undefined;
  return (store.settings as unknown as Record<string, unknown>)[key];
}

function setDraft(key: keyof Settings, value: string): void {
  drafts.value = { ...drafts.value, [key]: value };
}

function numberDraft(item: SettingItem): string {
  const raw = drafts.value[item.key];
  if (raw !== undefined) return raw;
  const value = currentValue(item.key);
  return typeof value === "number" ? String(value) : "";
}

/** 数值项允许的范围常显在输入框下方：与后端 §3.7 同一套区间，越界提示据此说明「允许多少」。 */
function numericRange(item: SettingItem): string {
  const key = item.key as NumericKey;
  const limit = NUMERIC_LIMITS[key];
  return limit === undefined ? "" : "允许 " + limit.min + "–" + limit.max + " " + limit.unit;
}

/** 刷新时丢掉本地草稿，界面一律以后端返回的实际生效值为准。 */
async function refresh(): Promise<void> {
  drafts.value = {};
  draftHints.value = {};
  await store.load();
}

/** 改动即持久化：一次只发被改字段的补丁（§3.4 命令 12）。 */
async function commitValue(
  item: SettingItem,
  value: boolean | string | string[] | null,
): Promise<void> {
  await store.setValue(item.key, value);
}

/**
 * 布尔开关：await 之后按 store 真值回写 DOM。
 * 写入被拒时 store 真值没变，Vue 会跳过值未变的 checked 补丁，开关会停在用户点出来的状态。
 */
async function commitBoolean(item: SettingItem, event: Event): Promise<void> {
  const el = event.target as HTMLInputElement;
  await commitValue(item, el.checked);
  syncChecked(el, Boolean(currentValue(item.key)));
}

/** 自启开关同理：以命令 14 返回的系统状态回写，失败时回到系统真相。 */
async function onAutostartToggle(event: Event): Promise<void> {
  const el = event.target as HTMLInputElement;
  await store.setAutostartEnabled(el.checked);
  syncChecked(el, store.autostart?.enabled ?? false);
}

async function commitNumber(item: SettingItem): Promise<void> {
  const key = item.key as NumericKey;
  if (NUMERIC_LIMITS[key] === undefined) return;
  const accepted = await store.setNumber(key, Number(numberDraft(item)));
  draftHints.value = {
    ...draftHints.value,
    [item.key]: accepted ? "" : (store.notSavedHint ?? ""),
  };
  if (accepted) {
    drafts.value = { ...drafts.value, [item.key]: String(currentValue(item.key)) };
  }
}

/** 启动参数一行一个（参数里可能带空格，不能用空格切分）。 */
function argsText(): string {
  const value = currentValue("launchArgs");
  return Array.isArray(value) ? value.join("\n") : "";
}

async function commitArgs(event: Event): Promise<void> {
  const target = event.target as HTMLTextAreaElement;
  const args = target.value
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
  await store.setValue("launchArgs", args);
}

/**
 * 通道下拉框的唯一写入口：走命令 2 原子写入 autoChannel + autoInstall。
 * 切换已开启自动安装的通道前先按 FR-1.4 弹普通确认；确认前不改写设置。
 */
async function commitChannel(event: Event): Promise<void> {
  const select = event.target as HTMLSelectElement;
  const next = select.value === "" ? null : select.value;
  if (store.settings !== null && channelSwitchNeedsConfirm(store.settings, next)) {
    pendingChannel.value = { channel: select.value, select };
    return;
  }
  await applyChannel(select, next);
}

async function applyChannel(select: HTMLSelectElement, channel: string | null): Promise<void> {
  await store.setAutoChannel(channel);
  // 写入失败时 store 真值没变；显式回写，别让下拉框停在用户点出来的值。
  select.value = String(store.settings?.autoChannel ?? "");
}

async function confirmChannelSwitch(): Promise<void> {
  const pending = pendingChannel.value;
  pendingChannel.value = null;
  if (pending !== null) await applyChannel(pending.select, pending.channel);
}

function cancelChannelSwitch(): void {
  const pending = pendingChannel.value;
  pendingChannel.value = null;
  if (pending !== null) pending.select.value = String(store.settings?.autoChannel ?? "");
}

async function onExportDiagnostics(): Promise<void> {
  exportHint.value = null;
  const target = await save({
    title: "导出诊断包",
    defaultPath: "dsh-manager-diagnostics.json",
    filters: [{ name: "JSON", extensions: ["json"] }],
  });
  if (target === null) return;
  const bytes = await store.exportDiagnostics(target);
  if (bytes !== null) {
    exportHint.value = "已写入 " + String(bytes) + " 字节：" + target;
  }
}

async function loadOplogPage(page: number): Promise<void> {
  oplogPage.value = page;
  await store.loadOplog(page * oplogPageSize, oplogPageSize);
}

async function onToggleOplog(): Promise<void> {
  oplogVisible.value = !oplogVisible.value;
  if (oplogVisible.value) {
    await loadOplogPage(0);
  }
}

const oplogFrom = computed(() =>
  store.oplogEntries.length === 0 ? 0 : oplogPage.value * oplogPageSize + 1,
);
const oplogTo = computed(() => oplogPage.value * oplogPageSize + store.oplogEntries.length);
const canOplogPrev = computed(() => oplogPage.value > 0);
const canOplogNext = computed(() => (oplogPage.value + 1) * oplogPageSize < store.oplogTotal);

/** 系统真实状态未知（还在读或读取失败）时，开关必须禁用，不能装成「已关闭」。 */
const autostartKnown = computed(() => store.autostart !== null && store.autostartError === null);

/** 自启开关下面那行必须说明「系统里现在到底是什么样」（FR-5.3）。 */
const autostartHint = computed(() => {
  if (store.autostartError !== null) {
    return "无法读取系统登录项状态，请重试：开关已禁用，不会用本地配置冒充系统状态。";
  }
  if (store.autostart === null) return "正在读取系统登录项状态……";
  return store.autostart.enabled
    ? "系统登录项已存在：" + (store.autostart.entryPath ?? "位置不可得")
    : "系统登录项不存在（当前为关闭状态）。";
});
</script>

<template>
  <section class="flex flex-col gap-4" data-testid="settings-page">
    <header class="flex items-center justify-between gap-4">
      <h1 class="text-xl font-bold">设置</h1>
      <button type="button" class="btn btn-sm" @click="refresh()">
        <RefreshCw class="size-4" aria-hidden="true" />
        刷新
      </button>
    </header>

    <div class="alert" data-testid="local-storage-note">
      <ShieldCheck class="size-5 shrink-0" aria-hidden="true" />
      <span>
        设置改动即时保存到本机 %APPDATA%\dsh-manager\settings.json，不会上传到任何服务器；
        本工具不读取、不上传任何凭据（NFR-3）。
      </span>
    </div>

    <ErrorAlert v-if="store.error !== null" :error="store.error" @retry="refresh()" />
    <p
      v-if="store.notSavedHint !== null"
      class="alert alert-warning"
      role="alert"
      data-testid="not-saved-hint"
    >
      {{ store.notSavedHint }}
    </p>

    <article
      v-for="group in SETTINGS_GROUPS"
      :key="group.id"
      class="card bg-base-100 shadow-sm"
      :data-testid="'settings-group-' + group.id"
    >
      <div class="card-body gap-3">
        <h2 class="card-title">{{ group.title }}</h2>

        <!-- 开机自启组：开关读的是系统真实状态，不是本地配置（FR-5.3、原则 P5） -->
        <div v-if="group.id === 'autostart'" class="flex flex-col gap-1">
          <label class="flex items-center justify-between gap-4">
            <span>
              随系统登录自动启动 DSH Manager
              <span class="block text-xs opacity-60">
                FR-5.1、FR-5.4、FR-5.5：只作用于本应用，不启动 DSH、不改 DSH 的任何设置；
                只支持「登录后启动」，不含计划任务、延迟启动或后台服务化。
              </span>
            </span>
            <input
              class="toggle toggle-primary"
              type="checkbox"
              data-testid="autostart-toggle"
              :checked="store.autostart?.enabled ?? false"
              :disabled="!autostartKnown"
              :indeterminate="!autostartKnown"
              @change="onAutostartToggle"
            />
          </label>
          <p class="text-sm opacity-70" data-testid="autostart-state-hint">{{ autostartHint }}</p>
          <div
            v-if="store.autostartError !== null"
            class="alert alert-error items-start"
            role="alert"
            data-testid="autostart-error"
          >
            <div class="flex flex-col gap-2">
              <p class="font-medium">无法读取系统登录项状态，请重试。</p>
              <p class="text-sm">原因：{{ store.autostartError.reason }}</p>
              <p class="text-sm">下一步：{{ store.autostartError.nextStep }}</p>
              <button
                type="button"
                class="btn btn-sm btn-outline w-fit"
                data-testid="autostart-retry"
                @click="store.refreshAutostart()"
              >
                重试读取
              </button>
            </div>
          </div>
        </div>

        <label
          v-for="item in group.items"
          :key="item.key"
          class="flex items-center justify-between gap-4"
          :data-testid="'setting-item-' + item.key"
        >
          <span>
            {{ item.label }}
            <span class="block text-xs opacity-60">影响：{{ item.requirement }}</span>
          </span>

          <input
            v-if="item.kind === 'boolean'"
            class="toggle"
            type="checkbox"
            :data-testid="'setting-' + item.key"
            :checked="Boolean(currentValue(item.key))"
            @change="commitBoolean(item, $event)"
          />

          <span v-else-if="item.kind === 'number'" class="flex flex-col items-end">
            <input
              class="input input-sm w-32"
              type="number"
              :data-testid="'setting-' + item.key"
              :value="numberDraft(item)"
              @input="setDraft(item.key, ($event.target as HTMLInputElement).value)"
              @blur="commitNumber(item)"
            />
            <span class="text-xs opacity-60">{{ numericRange(item) }}</span>
            <span
              v-if="draftHints[item.key]"
              class="text-xs text-error"
              :data-testid="'hint-' + item.key"
            >
              {{ draftHints[item.key] }}
            </span>
          </span>

          <input
            v-else-if="item.kind === 'text'"
            class="input input-sm w-80"
            type="text"
            :data-testid="'setting-' + item.key"
            :value="String(currentValue(item.key) ?? '')"
            @change="commitValue(item, ($event.target as HTMLInputElement).value)"
          />

          <textarea
            v-else-if="item.kind === 'lines'"
            class="textarea textarea-sm w-80"
            rows="3"
            :data-testid="'setting-' + item.key"
            :value="argsText()"
            @change="commitArgs"
          />

          <select
            v-else-if="item.kind === 'channel'"
            class="select select-sm"
            :data-testid="'setting-' + item.key"
            :value="String(currentValue(item.key) ?? '')"
            @change="commitChannel"
          >
            <option value="">不自动安装</option>
            <option v-for="channel in CHANNEL_OPTIONS" :key="channel.id" :value="channel.id">
              {{ channel.label }}
            </option>
          </select>

          <select
            v-else-if="item.kind === 'autostartMode'"
            class="select select-sm"
            :data-testid="'setting-' + item.key"
            :value="String(currentValue(item.key) ?? 'tray')"
            @change="commitValue(item, ($event.target as HTMLSelectElement).value as AutostartMode)"
          >
            <option value="tray">最小化到托盘（默认）</option>
            <option value="window">打开主窗口</option>
          </select>
        </label>
      </div>
    </article>

    <!-- 诊断组：不含设置项，只有动作 -->
    <article class="card bg-base-100 shadow-sm" data-testid="settings-group-diagnostics">
      <div class="card-body gap-3">
        <h2 class="card-title">诊断</h2>
        <div class="flex flex-wrap gap-2">
          <button
            type="button"
            class="btn btn-sm"
            data-testid="export-diagnostics"
            @click="onExportDiagnostics"
          >
            <Download class="size-4" aria-hidden="true" />
            导出诊断包
          </button>
          <button
            type="button"
            class="btn btn-sm"
            data-testid="toggle-oplog"
            @click="onToggleOplog"
          >
            <ScrollText class="size-4" aria-hidden="true" />
            {{ oplogVisible ? "收起操作记录" : "查看操作记录" }}
          </button>
        </div>
        <p v-if="exportHint !== null" class="text-sm text-success" data-testid="export-hint">
          {{ exportHint }}
        </p>

        <div v-if="oplogVisible" class="flex flex-col gap-2">
          <div class="flex flex-wrap items-center gap-2 text-sm opacity-70">
            <span data-testid="oplog-range">
              共 {{ store.oplogTotal }} 条，显示第 {{ oplogFrom }}–{{ oplogTo }} 条
            </span>
            <button
              type="button"
              class="btn btn-xs"
              data-testid="oplog-prev"
              :disabled="!canOplogPrev"
              @click="loadOplogPage(oplogPage - 1)"
            >
              上一页
            </button>
            <button
              type="button"
              class="btn btn-xs"
              data-testid="oplog-next"
              :disabled="!canOplogNext"
              @click="loadOplogPage(oplogPage + 1)"
            >
              下一页
            </button>
          </div>
          <div class="overflow-x-auto" data-testid="oplog-table">
            <table class="table table-sm">
              <thead>
                <tr>
                  <th>时间</th>
                  <th>动作</th>
                  <th>目标</th>
                  <th>结果</th>
                  <th>错误码</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="(entry, index) in store.oplogEntries" :key="index">
                  <td>{{ new Date(entry.timestampMs).toLocaleString() }}</td>
                  <td>{{ entry.action }}</td>
                  <td class="break-all font-mono text-xs">{{ entry.target }}</td>
                  <td>{{ entry.result }}</td>
                  <td>{{ entry.code ?? "—" }}</td>
                </tr>
                <tr v-if="store.oplogEntries.length === 0">
                  <td colspan="5" class="opacity-70">还没有操作记录。</td>
                </tr>
              </tbody>
            </table>
          </div>
        </div>

        <p class="text-xs opacity-70">
          凭据提示：本工具不读取、不显示、不上传任何凭据类信息——不会打开或解析 .dsh
          目录下的凭据文件、 令牌或密钥，导出的诊断包中也不含这些内容；对 DSH
          数据只读，系统代理设置只读（NFR-3）。
        </p>
      </div>
    </article>

    <ConfirmDialog
      :open="pendingChannel !== null"
      strength="normal"
      title="切换自动安装通道"
      :message="`切换后，之前开启自动安装的通道会被关闭。当前选择：${store.settings?.autoChannel ?? '无'}`"
      @confirm="confirmChannelSwitch()"
      @cancel="cancelChannelSwitch()"
    />
  </section>
</template>
