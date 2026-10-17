<script setup lang="ts">
import { computed, onMounted } from "vue";
import { PackageSearch, RefreshCw, ShieldCheck } from "@lucide/vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import { useOriginStore } from "../stores/origin";

const store = useOriginStore();

/** 未识别时要把「还缺什么」逐条说清楚（FR-2.3）。 */
const unidentified = computed(() => store.report?.verdict === "unidentified");

onMounted(() => {
  void store.load();
});
</script>

<template>
  <section class="flex flex-col gap-4">
    <header class="flex items-center justify-between gap-4">
      <h1 class="text-xl font-bold">安装来源</h1>
      <button type="button" class="btn btn-sm" :disabled="store.loading" @click="store.load()">
        <RefreshCw class="size-4" aria-hidden="true" />
        {{ store.loading ? "正在识别……" : "重新识别" }}
      </button>
    </header>

    <div class="alert" data-testid="origin-readonly-note">
      <ShieldCheck class="size-5 shrink-0" aria-hidden="true" />
      <span>
        本页为只读视图：只读取本机信息用于判断安装来源，不会修改任何设置、文件或环境变量。
      </span>
    </div>

    <ErrorAlert v-if="store.error !== null" :error="store.error" @retry="store.load()" />

    <p v-if="store.report === null && store.loading" class="text-sm opacity-70">
      正在识别安装来源……
    </p>

    <article class="card bg-base-100 shadow-sm">
      <div class="card-body gap-3">
        <div class="flex flex-wrap items-center gap-3">
          <span class="text-sm opacity-70">结论</span>
          <!-- 唯一结论徽章：包管理器名称来自后端结论对象，不写死 npm -->
          <span class="badge badge-lg badge-primary" data-testid="origin-verdict">
            {{ store.verdictText }}
          </span>
        </div>
        <p v-if="store.detailText !== ''" class="text-sm">{{ store.detailText }}</p>

        <div
          v-if="store.showMissing"
          class="alert alert-warning items-start"
          role="alert"
          data-testid="origin-missing"
        >
          <div class="flex flex-col gap-1">
            <p class="font-medium">
              {{ unidentified ? "未识别：依据不足，还缺以下信息：" : "还缺以下信息：" }}
            </p>
            <ul class="list-disc pl-5 text-sm">
              <li v-for="(item, index) in store.report?.missing ?? []" :key="index">
                {{ item }}
              </li>
            </ul>
            <p class="text-sm">缺少这些信息时，本工具不会猜测安装来源。</p>
          </div>
        </div>
      </div>
    </article>

    <article class="card bg-base-100 shadow-sm">
      <div class="card-body gap-3">
        <button
          type="button"
          class="btn btn-ghost justify-start px-0"
          :aria-expanded="store.expanded"
          @click="store.toggleExpanded()"
        >
          <PackageSearch class="size-4" aria-hidden="true" />
          {{ store.expanded ? "收起判定依据" : "展开判定依据" }}
          <span class="opacity-60">（{{ store.report?.evidences.length ?? 0 }} 条）</span>
        </button>

        <div v-if="store.expanded" class="overflow-x-auto" data-testid="origin-evidences">
          <table class="table table-sm">
            <thead>
              <tr>
                <th>来源</th>
                <th>路径</th>
                <th>观察到的内容</th>
                <th>是否支持结论</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="(evidence, index) in store.report?.evidences ?? []" :key="index">
                <td>{{ evidence.source }}</td>
                <td class="break-all font-mono text-xs">{{ evidence.path }}</td>
                <td>{{ evidence.observed }}</td>
                <td>
                  <span class="badge" :class="evidence.supports ? 'badge-success' : 'badge-ghost'">
                    {{ evidence.supports ? "支持" : "不支持" }}
                  </span>
                </td>
              </tr>
              <tr v-if="(store.report?.evidences.length ?? 0) === 0">
                <td colspan="4" class="opacity-70">本次识别没有取得任何判定依据。</td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </article>

    <p class="text-xs opacity-60">本工具不读取、不显示、不上传任何凭据类信息（NFR-3）。</p>
  </section>
</template>
