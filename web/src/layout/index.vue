<script setup lang="ts">
import { storeToRefs } from 'pinia'
import { RouterView } from 'vue-router'

import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import { useUiStore } from '@/stores/modules/ui'

import AppSidebar from './components/AppSidebar.vue'

const uiStore = useUiStore()
const { sidebarCollapsed } = storeToRefs(uiStore)
const { toggleSidebar } = uiStore
</script>

<template>
  <div class="flex h-screen overflow-hidden bg-(--cp-bg-page)">
    <AppSidebar :collapsed="sidebarCollapsed" @toggle="toggleSidebar" />
    <main class="h-screen min-w-0 flex-1 overflow-hidden">
      <BaseScrollbar view-class="flex h-full min-w-0 flex-col p-6">
        <RouterView />
      </BaseScrollbar>
    </main>
  </div>
</template>
