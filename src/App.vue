<script setup lang="ts">
import { ref } from "vue";
import { RouterView } from "vue-router";
import ErrorAlert from "./components/ErrorAlert.vue";
import { invokeCmd, isAppError } from "./ipc/client";
import type { AppError } from "./ipc/types";

const result = ref<string | null>(null);
const failure = ref<AppError | null>(null);

async function probe(fail: boolean): Promise<void> {
  result.value = null;
  failure.value = null;
  try {
    result.value = await invokeCmd<string>("ping", { fail });
  } catch (error) {
    failure.value = isAppError(error)
      ? error
      : {
          code: "unknown",
          reason: "收到了无法识别的错误对象",
          nextStep: "请导出诊断并反馈。",
          raw: String(error),
          retryable: false,
        };
  }
}
</script>

<template>
  <details class="m-4 rounded border border-warning p-3 text-sm">
    <summary class="cursor-pointer">临时联通验证（设置功能落地后删除）</summary>
    <div class="mt-2 flex gap-2">
      <button type="button" class="btn btn-sm" @click="probe(false)">检查联通</button>
      <button type="button" class="btn btn-sm" @click="probe(true)">模拟失败</button>
    </div>
    <p v-if="result !== null" class="mt-2">{{ result }}</p>
    <ErrorAlert v-if="failure !== null" class="mt-2" :error="failure" @retry="probe(true)" />
  </details>

  <RouterView />
</template>
