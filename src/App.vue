<script setup lang="ts">
import { RouterView } from "vue-router";
import AppNav from "./components/AppNav.vue";
</script>

<template>
  <div class="min-h-screen bg-base-200">
    <header class="navbar bg-base-100 shadow-sm">
      <span class="px-3 text-lg font-semibold">DSH 管理器</span>
      <AppNav />
    </header>
    <main class="mx-auto w-full max-w-5xl p-4">
      <RouterView />
    </main>
  </div>
</template>
