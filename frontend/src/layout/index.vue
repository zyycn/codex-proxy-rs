<script setup lang="ts">
import { useModalFocus, ZScrollbar } from '@codex-proxy/ui'
import { useMediaQuery } from '@vueuse/core'

import { storeToRefs } from 'pinia'
import { nextTick, onBeforeUnmount, onMounted, ref, shallowRef, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'

import { RouterView, useRoute } from 'vue-router'
import { useSystemUpdateStore } from '@/stores/modules/system-update'
import { useUiStore } from '@/stores/modules/ui'

import AppSidebar from './components/AppSidebar.vue'
import FloatingSidebarToggle from './components/FloatingSidebarToggle.vue'
import SystemUpdateModal from './components/system-update-modal/index.vue'

const { t } = useI18n({ useScope: 'global' })

const uiStore = useUiStore()
const systemUpdateStore = useSystemUpdateStore()
const { sidebarCollapsed } = storeToRefs(uiStore)
const { toggleSidebar } = uiStore
const route = useRoute()
const pageScrollbarRef = ref<InstanceType<typeof ZScrollbar> | null>(null)
const mobileSidebarOpen = shallowRef(false)
const mobileSidebarPanel = useTemplateRef<HTMLElement>('mobileSidebarPanel')
const desktop = useMediaQuery('(min-width: 961px)')
const { rememberFocus, focusInitial, handleKeydown, restoreFocus } = useModalFocus(mobileSidebarPanel, {
  isOpen: () => mobileSidebarOpen.value,
  onEscape: closeMobileSidebar,
})
const systemUpdateOpen = shallowRef(false)

async function openMobileSidebar() {
  rememberFocus()
  mobileSidebarOpen.value = true
  await nextTick()
  focusInitial()
}

function closeMobileSidebar() {
  mobileSidebarOpen.value = false
}

watch(desktop, (isDesktop) => {
  if (isDesktop)
    closeMobileSidebar()
})

onMounted(() => {
  void systemUpdateStore.loadSidebarVersion().catch(() => undefined)
})

onBeforeUnmount(() => {
  systemUpdateStore.disconnectUpdateEvents()
})

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
  <div class="relative flex h-dvh overflow-hidden bg-z-bg-layout" :inert="mobileSidebarOpen">
    <AppSidebar
      :collapse="sidebarCollapsed"
      @toggle="toggleSidebar"

      @open-system-update="systemUpdateOpen = true"
    />
    <FloatingSidebarToggle v-show="!mobileSidebarOpen" @open="openMobileSidebar" />
    <main class="relative isolate h-dvh min-w-0 flex-1 overflow-hidden">
      <!-- 让主滚动区自身绘制不透明底色，浏览器可在合成线程滚动 -->
      <ZScrollbar ref="pageScrollbarRef" class="[&>.z-scrollbar-hidden]:bg-z-bg-layout">
        <div class="flex min-h-full min-w-0 flex-col p-4 min-[961px]:p-6">
          <RouterView v-slot="{ Component }">
            <component :is="Component" class="min-h-0 flex-1" />
          </RouterView>
        </div>
      </ZScrollbar>
    </main>

    <Teleport to="body">
      <Transition name="mobile-sidebar" @after-leave="restoreFocus">
        <div v-if="mobileSidebarOpen" :inert="!mobileSidebarOpen" role="presentation" class="fixed inset-0 z-50 min-[961px]:hidden" @keydown="handleKeydown">
          <button
            type="button"
            tabindex="-1"
            class="absolute inset-0 border-0 bg-black/32 backdrop-blur-[1px] cursor-default"
            :aria-label="t('navigation.sidebar.close')"
            @click="closeMobileSidebar"
          />
          <div
            ref="mobileSidebarPanel"
            class="mobile-sidebar-panel absolute inset-y-0 left-0 flex"
            role="dialog" aria-modal="true" :aria-label="t('navigation.mainLabel')" tabindex="-1"
          >
            <AppSidebar
              mobile
              @close="closeMobileSidebar"
              @navigate="closeMobileSidebar"

              @open-system-update="closeMobileSidebar(); systemUpdateOpen = true"
            />
          </div>
        </div>
      </Transition>
    </Teleport>

    <SystemUpdateModal v-model="systemUpdateOpen" />
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
