<script setup lang="ts">
import { ref } from "vue";
import { Copy, RotateCw, TriangleAlert } from "@lucide/vue";
import type { AppError } from "../ipc/types";

const props = defineProps<{ error: AppError }>();
const emit = defineEmits<{ retry: [] }>();

const copied = ref(false);

async function copyRaw(): Promise<void> {
  if (props.error.raw === null) {
    return;
  }
  await navigator.clipboard.writeText(props.error.raw);
  copied.value = true;
}
</script>

<template>
  <div class="alert alert-error items-start" role="alert">
    <TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
    <div class="flex flex-col gap-2">
      <p class="font-medium">原因：{{ props.error.reason }}</p>
      <p class="text-sm">下一步：{{ props.error.nextStep }}</p>

      <details v-if="props.error.raw !== null" class="text-xs">
        <summary class="cursor-pointer">原始错误</summary>
        <pre class="mt-1 whitespace-pre-wrap break-all">{{ props.error.raw }}</pre>
        <button type="button" class="btn btn-xs mt-1" @click="copyRaw">
          <Copy class="size-3" aria-hidden="true" />
          {{ copied ? "已复制" : "复制原始错误" }}
        </button>
      </details>

      <button
        v-if="props.error.retryable"
        type="button"
        class="btn btn-sm btn-outline w-fit"
        @click="emit('retry')"
      >
        <RotateCw class="size-4" aria-hidden="true" />
        重试
      </button>
    </div>
  </div>
</template>
