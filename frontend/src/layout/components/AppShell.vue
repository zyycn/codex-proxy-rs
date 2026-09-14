<script setup lang="ts">
import type { Component } from 'vue'
import { storeToRefs } from 'pinia'
import { nextTick, ref, shallowRef, watch } from 'vue'
import { RouterView, useRoute } from 'vue-router'

import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import { useUiStore } from '@/stores/modules/ui'

import AppAboutModal from './AppAboutModal.vue'
import AppSidebar from './AppSidebar.vue'
import FloatingSidebarToggle from './FloatingSidebarToggle.vue'

const props = withDefaults(defineProps<{
  gitSha?: string
  hasUpdate?: boolean
  navItems: ReadonlyArray<{
    label: string
    icon: Component
    path: string
  }>
  systemUpdateEnabled?: boolean
  version?: string
}>(), {
  gitSha: '',
  hasUpdate: false,
  systemUpdateEnabled: false,
  version: '',
})

const emit = defineEmits<{
  openSystemUpdate: []
}>()

const uiStore = useUiStore()
const { sidebarCollapsed } = storeToRefs(uiStore)
const { toggleSidebar } = uiStore
const route = useRoute()
const pageScrollbarRef = ref<InstanceType<typeof BaseScrollbar> | null>(null)
const mobileSidebarOpen = shallowRef(false)
const aboutOpen = shallowRef(false)

function openMobileSidebar() {
  mobileSidebarOpen.value = true
}

function closeMobileSidebar() {
  mobileSidebarOpen.value = false
}

watch(
  () => route.fullPath,
  async () => {
    closeMobileSidebar()
    await nextTick()
    await pageScrollbarRef.value?.scrollToTop()
  },
  { flush: 'post' },
)
</script>

<template>
  <div class="relative flex h-dvh overflow-hidden bg-cp-bg-layout">
    <AppSidebar
      :collapsed="sidebarCollapsed"
      :nav-items="navItems"
      :version="version"
      :has-update="hasUpdate"
      :system-update-enabled="systemUpdateEnabled"
      @toggle="toggleSidebar"
      @open-about="aboutOpen = true"
      @open-system-update="emit('openSystemUpdate')"
    />
    <FloatingSidebarToggle v-if="!mobileSidebarOpen" @open="openMobileSidebar" />

    <main class="relative isolate h-dvh min-w-0 flex-1 overflow-hidden">
      <BaseScrollbar ref="pageScrollbarRef">
        <div class="flex min-h-full min-w-0 flex-col p-4 min-[961px]:p-6">
          <RouterView v-slot="{ Component: RouteComponent }">
            <component :is="RouteComponent" class="min-h-0 flex-1" />
          </RouterView>
        </div>
      </BaseScrollbar>
    </main>

    <Teleport to="body">
      <Transition name="mobile-sidebar">
        <div v-if="mobileSidebarOpen" class="fixed inset-0 z-50 min-[961px]:hidden">
          <button
            type="button"
            class="absolute inset-0 border-0 bg-black/32 backdrop-blur-[1px] cursor-default"
            aria-label="关闭侧边栏"
            @click="closeMobileSidebar"
          />
          <div class="mobile-sidebar-panel absolute inset-y-0 left-0 flex">
            <AppSidebar
              mobile
              :nav-items="navItems"
              :version="version"
              :has-update="hasUpdate"
              :system-update-enabled="systemUpdateEnabled"
              @close="closeMobileSidebar"
              @navigate="closeMobileSidebar"
              @open-about="aboutOpen = true"
              @open-system-update="emit('openSystemUpdate')"
            />
          </div>
        </div>
      </Transition>
    </Teleport>

    <AppAboutModal v-model="aboutOpen" :version="props.version" :git-sha="props.gitSha" />
    <slot name="overlay" />
  </div>
</template>

<style scoped>
.mobile-sidebar-enter-active,
.mobile-sidebar-leave-active {
  transition: opacity 180ms ease;
}

.mobile-sidebar-enter-active .mobile-sidebar-panel,
.mobile-sidebar-leave-active .mobile-sidebar-panel {
  transition: transform 220ms cubic-bezier(0.22, 1, 0.36, 1);
}

.mobile-sidebar-enter-from,
.mobile-sidebar-leave-to {
  opacity: 0;
}

.mobile-sidebar-enter-from .mobile-sidebar-panel,
.mobile-sidebar-leave-to .mobile-sidebar-panel {
  transform: translate3d(-100%, 0, 0);
}

@media (prefers-reduced-motion: reduce) {
  .mobile-sidebar-enter-active,
  .mobile-sidebar-leave-active,
  .mobile-sidebar-enter-active .mobile-sidebar-panel,
  .mobile-sidebar-leave-active .mobile-sidebar-panel {
    transition-duration: 1ms;
  }
}
</style>
