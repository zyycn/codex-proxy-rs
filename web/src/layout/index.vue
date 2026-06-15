<script setup lang="ts">
import { ref } from 'vue'
import { RouterView } from 'vue-router'

import AppSidebar from './components/AppSidebar.vue'

const sidebarCollapsed = ref(false)
</script>

<template>
  <div class="flex h-screen overflow-hidden bg-[var(--cp-bg-page)]">
    <AppSidebar :collapsed="sidebarCollapsed" @toggle="sidebarCollapsed = !sidebarCollapsed" />
    <main class="cp-scrollbar h-screen min-w-0 flex-1 overflow-auto">
      <RouterView />
    </main>
  </div>
</template>
