<script setup lang="ts">
import { computed } from "vue";
import type { TaskProgressPayload } from "../ipc/types";

interface Props extends TaskProgressPayload {
  cancellable?: boolean;
}

const props = withDefaults(defineProps<Props>(), { cancellable: true });
const emit = defineEmits<{ cancel: [] }>();

const phaseText = computed(
  () =>
    ({
      checking: "正在检查磁盘空间",
      downloading: "正在下载安装包",
      verifying: "正在校验安装包完整性",
      installing: "正在安装",
    })[props.phase],
);

const remainingText = computed(() => {
  const ms = props.remainingMs;
  if (ms === null || ms <= 0) return "剩余时间：无法估算";
  const totalSeconds = Math.ceil(ms / 1000);
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return minutes > 0 ? `剩余时间：约 ${minutes} 分 ${seconds} 秒` : `剩余时间：约 ${seconds} 秒`;
});
</script>

<template>
  <div class="flex flex-col gap-2">
    <div class="flex items-center justify-between text-sm">
      <span>{{ phaseText }}</span>
      <span>{{ percent }}%</span>
    </div>
    <progress class="progress progress-primary w-full" :value="percent" max="100" />
    <div class="flex items-center justify-between text-xs opacity-70">
      <span>{{ remainingText }}</span>
      <button v-if="cancellable" class="btn btn-xs" @click="emit('cancel')">取消</button>
    </div>
  </div>
</template>
