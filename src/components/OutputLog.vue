<script setup lang="ts">
import { nextTick, ref, watch } from "vue";

import type { OutputLine } from "../ipc/types";
import { formatTimestamp } from "../utils/format";

const props = defineProps<{
  lines: OutputLine[];
  paused: boolean;
  autoScroll: boolean;
  canExport: boolean;
}>();

const emit = defineEmits<{
  (e: "toggle-pause"): void;
  (e: "toggle-auto-scroll", value: boolean): void;
  (e: "clear"): void;
  (e: "save-as"): void;
}>();

const box = ref<HTMLElement | null>(null);
const copyError = ref<string | null>(null);

// 只有「没暂停 且 开了自动滚动」时才跟随最新一行。
watch(
  () => props.lines.length,
  async () => {
    if (props.paused || !props.autoScroll) return;
    await nextTick();
    if (box.value) box.value.scrollTop = box.value.scrollHeight;
  },
);

/** 复制全部：走 WebView 的剪贴板 API；失败时给出原因 + 下一步，不静默吞掉（NFR-1）。 */
async function copyAll(): Promise<void> {
  copyError.value = null;
  const text = props.lines.map((l) => formatTimestamp(l.timestampMs) + " " + l.text).join("\n");
  try {
    if (!navigator.clipboard) {
      throw new Error("当前环境不提供剪贴板接口");
    }
    await navigator.clipboard.writeText(text);
  } catch (e) {
    copyError.value =
      "复制失败：" +
      (e instanceof Error ? e.message : String(e)) +
      "。可以改用「另存为」导出后再复制。";
  }
}
</script>

<template>
  <section class="flex flex-col gap-2">
    <header class="flex flex-wrap items-center gap-2">
      <span class="text-sm opacity-70">运行输出</span>
      <button type="button" class="btn btn-xs" @click="emit('toggle-pause')">
        {{ paused ? "继续" : "暂停" }}
      </button>
      <label class="flex cursor-pointer items-center gap-1 text-xs">
        <input
          type="checkbox"
          class="checkbox checkbox-xs"
          :checked="autoScroll"
          @change="emit('toggle-auto-scroll', ($event.target as HTMLInputElement).checked)"
        />
        自动滚动
      </label>
      <button type="button" class="btn btn-xs" @click="emit('clear')">清空</button>
      <button type="button" class="btn btn-xs" :disabled="lines.length === 0" @click="copyAll">
        复制全部
      </button>
      <button type="button" class="btn btn-xs" :disabled="!canExport" @click="emit('save-as')">
        另存为…
      </button>
      <span v-if="paused" class="badge badge-warning badge-sm">已暂停（仍在后台接收）</span>
    </header>

    <p v-if="copyError" class="alert alert-warning py-1 text-xs">{{ copyError }}</p>

    <div
      ref="box"
      class="h-80 overflow-auto rounded bg-base-300 p-2 font-mono text-xs leading-5"
      data-testid="output-log"
    >
      <p v-if="lines.length === 0" class="opacity-60">还没有输出。</p>
      <p v-for="l in lines" :key="l.seq" :class="l.isStderr ? 'text-error' : ''">
        <span class="opacity-50">{{ formatTimestamp(l.timestampMs) }}</span>
        <span class="ml-2">{{ l.text }}</span>
      </p>
    </div>
  </section>
</template>
