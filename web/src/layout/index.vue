<script setup lang="ts">
import { storeToRefs } from 'pinia'
import { nextTick, ref, watch } from 'vue'
import { RouterView, useRoute } from 'vue-router'

import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import { useUiStore } from '@/stores/modules/ui'

import AppSidebar from './components/AppSidebar.vue'

const uiStore = useUiStore()
const { sidebarCollapsed } = storeToRefs(uiStore)
const { toggleSidebar } = uiStore
const route = useRoute()
const pageScrollbarRef = ref<InstanceType<typeof BaseScrollbar> | null>(null)

watch(
  () => route.fullPath,
  async () => {
    await nextTick()
    await pageScrollbarRef.value?.scrollToTop()
  },
  { flush: 'post' },
)
</script>

<template>
  <div class="flex h-screen overflow-hidden bg-(--cp-bg-page)">
    <AppSidebar :collapsed="sidebarCollapsed" @toggle="toggleSidebar" />
    <main class="h-screen min-w-0 flex-1 overflow-hidden">
      <BaseScrollbar ref="pageScrollbarRef" view-class="flex h-full min-w-0 flex-col p-6">
        <RouterView />
      </BaseScrollbar>
    </main>
  </div>
</template>
