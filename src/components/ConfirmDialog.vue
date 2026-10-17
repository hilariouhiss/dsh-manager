<script setup lang="ts">
import { computed } from "vue";
import { CircleAlert, CircleCheck, TriangleAlert } from "@lucide/vue";

// §3.3.2 冻结的共享 props：T-V7 落地，T-I4 / T-I6 / T-U1 复用。
// 名字与取值一字不改：body→message、level→strength、forced→force、无 confirmLabel。
const props = defineProps<{
  open: boolean;
  title: string;
  /** 必须写清「发生了什么、影响是什么」（NFR-1） */
  message: string;
  strength: "normal" | "strong" | "force";
}>();

const emit = defineEmits<{ (e: "confirm"): void; (e: "cancel"): void }>();

const icon = computed(() => {
  if (props.strength === "force") return CircleAlert;
  if (props.strength === "strong") return TriangleAlert;
  return CircleCheck;
});

const confirmClass = computed(() =>
  props.strength === "normal" ? "btn btn-primary" : "btn btn-error",
);
// 三档强度里只有 force 档不允许用「取消」离开——它本身就是「单独确认」的落点（§3.9 D-3）。
const cancellable = computed(() => props.strength !== "force");
const confirmLabel = computed(() => (props.strength === "force" ? "强制终止" : "确认"));
</script>

<template>
  <div v-if="open" class="modal modal-open" role="dialog" aria-modal="true">
    <div class="modal-box">
      <h3 class="flex items-center gap-2 text-lg font-bold">
        <component :is="icon" class="size-5" aria-hidden="true" />
        {{ title }}
      </h3>
      <p class="py-4 whitespace-pre-wrap">{{ message }}</p>
      <div class="modal-action">
        <button v-if="cancellable" class="btn" @click="emit('cancel')">取消</button>
        <button :class="confirmClass" @click="emit('confirm')">{{ confirmLabel }}</button>
      </div>
    </div>
  </div>
</template>
