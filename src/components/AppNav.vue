<script setup lang="ts">
import { RouterLink } from "vue-router";
import { navItems } from "../router";
</script>

<template>
  <ul class="menu menu-horizontal gap-1 px-2">
    <li v-for="item in navItems" :key="item.path">
      <RouterLink :to="item.path" active-class="menu-active">
        <component :is="item.icon" class="size-4" aria-hidden="true" />
        {{ item.title }}
      </RouterLink>
    </li>
  </ul>
</template>
