<script setup lang="ts">
import { RouterView } from 'vue-router'

import AppSidebar from './components/AppSidebar.vue'
</script>

<template>
  <div class="flex min-h-screen bg-[var(--cp-bg-page)]">
    <AppSidebar />
    <main class="min-w-0 flex-1 overflow-x-auto">
      <RouterView />
    </main>
  </div>
</template>
