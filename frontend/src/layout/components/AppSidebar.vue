<script setup lang="ts">
import type { PluginManagementView } from '@/api'

import { BaseIconButton, BaseMotionIcon, BaseScrollbar } from '@codex-proxy/ui'
import {
  ArrowUpCircle,
  Blocks,
  ChartNoAxesColumn,
  ChevronDown,
  FolderTree,
  Info,
  KeyRound,
  LayoutDashboard,
  LogOut,
  Moon,
  Network,
  Palette,
  PanelLeftClose,
  PanelLeftOpen,
  PanelsTopLeft,
  Puzzle,
  Settings,
  Sun,
  Users,
} from '@lucide/vue'
import { usePreferredReducedMotion, useTimeoutFn } from '@vueuse/core'
import { gsap } from 'gsap'

import { storeToRefs } from 'pinia'
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, useId, useTemplateRef, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import AppBrandMark from '@/components/AppBrandMark.vue'
import { useAuthStore } from '@/stores/modules/auth'
import { usePluginViewsStore } from '@/stores/modules/plugin-views'
import { useSystemUpdateStore } from '@/stores/modules/system-update'
import { useThemeStore } from '@/stores/modules/theme'
import { pluginPageLocation, shortPluginInstanceId } from '@/utils/plugin'

const props = withDefaults(
  defineProps<{
    collapsed?: boolean
    mobile?: boolean
  }>(),
  {
    collapsed: false,
    mobile: false,
  },
)
const emit = defineEmits<{
  close: []
  navigate: []
  openAbout: []
  openSystemUpdate: []
  toggle: []
}>()
const route = useRoute()
const router = useRouter()
const authStore = useAuthStore()
const pluginViewsStore = usePluginViewsStore()
const systemUpdateStore = useSystemUpdateStore()
const themeStore = useThemeStore()
const { pageViews: pluginPageViews } = storeToRefs(pluginViewsStore)
const { version, hasUpdate } = storeToRefs(systemUpdateStore)
const { effectiveTheme } = storeToRefs(themeStore)
const { toggleTheme } = themeStore
const preferredMotion = usePreferredReducedMotion()
const isCollapsed = computed(() => !props.mobile && Boolean(props.collapsed))
const pluginMenuId = useId()
const pluginGroupExpanded = shallowRef(route.path.startsWith('/plugins'))

const navItems = [
  { label: '概览', icon: LayoutDashboard, path: '/' },
  { label: '账号管理', icon: Users, path: '/accounts' },
  { label: '代理管理', icon: Network, path: '/proxies' },
  { label: '分组管理', icon: FolderTree, path: '/groups' },
  { label: 'API 密钥', icon: KeyRound, path: '/keys' },
  { label: '使用统计', icon: ChartNoAxesColumn, path: '/usage' },
  { label: '插件', icon: Puzzle, path: '/plugins' },
  { label: '主题设置', icon: Palette, path: '/theme' },
  { label: '系统设置', icon: Settings, path: '/settings' },
]
const pluginNavIndex = navItems.findIndex(item => item.path === '/plugins')
const pluginMenuVisible = computed(() => pluginGroupExpanded.value && !isCollapsed.value)
const pluginViewNameCounts = computed(() => pluginPageViews.value.reduce((counts, view) => {
  const title = view.pages[0]?.title ?? view.name
  counts.set(title, (counts.get(title) ?? 0) + 1)
  return counts
}, new Map<string, number>()))
const pluginChildrenHeight = computed(() => pluginMenuVisible.value
  ? (pluginPageViews.value.length + 1) * 44
  : 0)

function isActive(path: string) {
  if (path === '/')
    return route.path === '/'
  return route.path.startsWith(path)
}

const activeNavIndex = computed(() => {
  const index = navItems.findIndex(item => isActive(item.path))
  return Math.max(0, index)
})
const activeNavIndicatorStyle = computed(() => ({
  transform: `translate3d(0, ${activeNavIndex.value * 58 + (activeNavIndex.value > pluginNavIndex ? pluginChildrenHeight.value : 0)}px, 0)`,
  opacity: isActive('/plugins') && pluginMenuVisible.value ? 0 : 1,
}))
const activePluginIndex = computed(() => route.name === 'plugins'
  ? 0
  : pluginPageViews.value.findIndex(isPluginViewActive) + 1)
const activePluginIndicatorStyle = computed(() => ({
  transform: `translate3d(0, ${activePluginIndex.value * 44}px, 0)`,
  opacity: isActive('/plugins') && pluginMenuVisible.value ? 1 : 0,
}))
const navFeedbackMuted = shallowRef(false)
const { start: restoreNavFeedback, stop: stopNavFeedbackRestore } = useTimeoutFn(
  () => {
    navFeedbackMuted.value = false
  },
  300,
  { immediate: false },
)

function muteNavFeedbackDuringMove() {
  navFeedbackMuted.value = true
  stopNavFeedbackRestore()
  restoreNavFeedback()
}

function navigate(path: string) {
  muteNavFeedbackDuringMove()
  void router.push(path)
  emit('navigate')
}

function togglePluginGroup() {
  if (isCollapsed.value) {
    navigate('/plugins')
    return
  }
  pluginGroupExpanded.value = !pluginGroupExpanded.value
}

function navigatePluginView(view: PluginManagementView) {
  const page = view.pages[0]
  if (page)
    navigate(router.resolve(pluginPageLocation(view, page)).fullPath)
}

function isPluginViewActive(view: PluginManagementView) {
  const value = route.params.instanceId
  const routeInstanceId = Array.isArray(value) ? value[0] : value
  return route.name === 'plugin-page' && routeInstanceId === view.target.instanceId
}

function pluginViewLabel(view: PluginManagementView) {
  const title = view.pages[0]?.title ?? view.name
  if (pluginViewNameCounts.value.get(title) === 1)
    return title
  const duplicateName = pluginPageViews.value.some(item => item !== view && item.name === view.name && item.pages[0]?.title === title)
  return `${title} · ${duplicateName ? shortPluginInstanceId(view.target.instanceId) : view.name}`
}

function openSystemUpdate() {
  emit('openSystemUpdate')
}

async function handleLogout() {
  if (!await authStore.logout())
    return
  await router.push('/login')
  emit('navigate')
}

const sidebarEl = ref<HTMLElement | null>(null)
const brandLabelEl = ref<HTMLElement | null>(null)
const navSignalEl = useTemplateRef<HTMLElement>('navSignal')
const pluginNavSignalEls = useTemplateRef<HTMLElement[]>('pluginNavSignal')
const collapsedSidebarWidth = 88
const expandedSidebarWidth = 251
const sidebarWidth = computed(() => (isCollapsed.value ? collapsedSidebarWidth : expandedSidebarWidth))
const brandLabelVisible = shallowRef(!isCollapsed.value)
const themeToggleLabel = computed(() => (effectiveTheme.value === 'dark' ? '切换浅色模式' : '切换暗黑模式'))
const versionText = computed(() => version.value?.version.trim() ?? '')
const hasVersionLabel = computed(() => versionText.value.length > 0)
const versionLabel = computed(() => `v${versionText.value}`)
const updateButtonLabel = computed(() => (hasUpdate.value ? '发现新版本，打开系统更新' : '打开系统更新'))

function prefersReducedMotion() {
  return preferredMotion.value === 'reduce'
}

function animateSidebarLabels(collapsed: boolean) {
  const labels = sidebarEl.value?.querySelectorAll<HTMLElement>('.sidebar-label')

  if (!labels?.length) {
    return
  }

  if (prefersReducedMotion()) {
    gsap.set(labels, {
      opacity: collapsed ? 0 : 1,
      x: collapsed ? -6 : 0,
    })
    return
  }

  gsap.to(labels, {
    opacity: collapsed ? 0 : 1,
    x: collapsed ? -6 : 0,
    duration: collapsed ? 0.16 : 0.2,
    ease: collapsed ? 'power2.in' : 'power3.out',
    stagger: collapsed ? 0 : 0.018,
    overwrite: true,
  })
}

function hideBrandLabel() {
  const label = brandLabelEl.value

  if (label) {
    gsap.killTweensOf(label)
    gsap.set(label, {
      opacity: 0,
      x: -6,
    })
  }

  brandLabelVisible.value = false
}

function animateBrandLabelEnter() {
  const label = brandLabelEl.value

  if (!label) {
    return
  }

  if (prefersReducedMotion()) {
    gsap.set(label, {
      opacity: 1,
      x: 0,
    })
    return
  }

  gsap.fromTo(
    label,
    {
      opacity: 0,
      x: -6,
    },
    {
      opacity: 1,
      x: 0,
      duration: 0.2,
      ease: 'power3.out',
      overwrite: true,
    },
  )
}

function animateSidebarWidth(collapsed: boolean) {
  if (!sidebarEl.value) {
    return
  }

  const targetWidth = collapsed ? collapsedSidebarWidth : expandedSidebarWidth
  const currentWidth = sidebarEl.value.getBoundingClientRect().width

  if (prefersReducedMotion()) {
    gsap.set(sidebarEl.value, {
      width: targetWidth,
      flexBasis: targetWidth,
    })
    return
  }

  gsap.set(sidebarEl.value, {
    width: currentWidth,
    flexBasis: currentWidth,
  })

  gsap.to(sidebarEl.value, {
    width: targetWidth,
    flexBasis: targetWidth,
    duration: 0.34,
    ease: 'power3.out',
    overwrite: true,
  })
}

function animateNavSignal() {
  const signal = isActive('/plugins') && pluginMenuVisible.value ? pluginNavSignalEls.value?.[0] : navSignalEl.value
  if (!signal)
    return

  gsap.killTweensOf(signal)
  if (prefersReducedMotion()) {
    gsap.set(signal, { opacity: 0, xPercent: -70 })
    return
  }

  gsap.fromTo(
    signal,
    { opacity: 0.48, xPercent: -70 },
    {
      opacity: 0,
      xPercent: 120,
      duration: 0.52,
      ease: 'power2.out',
      overwrite: true,
    },
  )
}

onMounted(() => {
  void pluginViewsStore.ensureLoaded().catch(() => undefined)
  gsap.set(sidebarEl.value, {
    width: sidebarWidth.value,
    flexBasis: sidebarWidth.value,
  })
  if (isCollapsed.value) {
    hideBrandLabel()
  }
  else {
    gsap.set(brandLabelEl.value, {
      opacity: 1,
      x: 0,
    })
  }
  animateSidebarLabels(isCollapsed.value)
  gsap.set(navSignalEl.value, { opacity: 0, xPercent: -70 })
})

watch(
  () => isCollapsed.value,
  async (collapsed) => {
    if (collapsed) {
      hideBrandLabel()
    }
    else {
      brandLabelVisible.value = true
    }
    animateSidebarLabels(Boolean(collapsed))
    animateSidebarWidth(Boolean(collapsed))
    await nextTick()
    if (!collapsed) {
      animateBrandLabelEnter()
    }
  },
)

watch(
  () => route.path,
  async (path, previousPath) => {
    if (path === previousPath)
      return
    if (path.startsWith('/plugins'))
      pluginGroupExpanded.value = true
    muteNavFeedbackDuringMove()
    await nextTick()
    animateNavSignal()
  },
  { flush: 'post' },
)

onBeforeUnmount(() => {
  stopNavFeedbackRestore()
  const targets = [sidebarEl.value, brandLabelEl.value, navSignalEl.value, ...(pluginNavSignalEls.value ?? [])].filter((target): target is HTMLElement =>
    Boolean(target),
  )
  gsap.killTweensOf(targets)

  const labels = sidebarEl.value?.querySelectorAll<HTMLElement>('.sidebar-label')
  if (labels?.length) {
    gsap.killTweensOf(labels)
  }
})
</script>

<template>
  <aside
    ref="sidebarEl"
    class="z-20 h-dvh shrink-0 flex-col overflow-hidden bg-(--cp-layout-sider-bg) shadow-cp-layout-sider"
    :class="[
      mobile ? 'flex' : 'hidden min-[961px]:flex',
      isCollapsed ? 'w-22 basis-22 items-center' : 'w-62.75 basis-62.75',
    ]"
  >
    <div
      class="mx-4 mt-6 grid h-12 shrink-0 grid-cols-[44px_minmax(0,1fr)] items-center"
      :class="isCollapsed ? 'w-11 justify-start' : 'self-stretch gap-3'"
    >
      <BaseMotionIcon
        variant="brand"
        class="inline-flex size-11 items-center justify-center relative -top-0.5 rounded-cp"
      >
        <AppBrandMark class="block size-11 select-none" />
      </BaseMotionIcon>
      <span v-show="brandLabelVisible" ref="brandLabelEl" class="grid min-w-33 content-center overflow-hidden">
        <strong class="text-base leading-[1.1] font-heavy text-cp-text"> Codex Proxy </strong>
        <span class="mt-1.5 flex h-4.5 min-w-0 items-center gap-2">
          <span class="shrink-0 text-xs leading-none font-emphasis text-cp-text-secondary"> Rust build </span>
          <button
            v-if="hasVersionLabel"
            type="button"
            class="inline-flex h-4.5 min-w-0 cursor-pointer items-center gap-1 rounded-cp-sm border-0 px-1.5 font-mono text-[10px] leading-none font-bold transition-colors outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline focus-visible:ring-offset-2 focus-visible:ring-offset-cp-bg-container"
            :class="[
              hasUpdate
                ? 'bg-cp-success-container text-cp-success-on-container hover:bg-cp-success-container-hover'
                : 'bg-cp-fill-quaternary text-cp-text-quaternary hover:bg-cp-fill-tertiary hover:text-cp-text-secondary',
            ]"
            :title="updateButtonLabel"
            @click="openSystemUpdate"
          >
            <span>{{ versionLabel }}</span>
            <ArrowUpCircle v-if="hasUpdate" class="size-3 shrink-0 text-cp-success" />
          </button>
        </span>
      </span>
    </div>

    <BaseScrollbar class="my-6 w-full flex-1">
      <div class="px-4">
        <nav class="relative grid gap-3" :class="isCollapsed ? 'mx-auto w-11.5' : 'w-full'" aria-label="主导航">
          <span
            class="pointer-events-none absolute inset-x-0 top-0 h-11.5 overflow-hidden rounded-cp bg-cp-menu-item-selected-bg transition-[transform,opacity] duration-260 ease-[cubic-bezier(0.22,1,0.36,1)] motion-reduce:transition-none"
            :style="activeNavIndicatorStyle"
          >
            <span
              ref="navSignal"
              class="absolute inset-y-0 left-0 w-2/3 [background:linear-gradient(90deg,transparent,color-mix(in_srgb,var(--cp-color-info)_9%,transparent),transparent)]"
            />
          </span>
          <template v-for="item in navItems" :key="item.label">
            <div v-if="item.path === '/plugins'" class="relative z-10 grid min-w-0">
              <button
                type="button"
                class="inline-flex h-11.5 cursor-pointer items-center rounded-cp border-0 bg-transparent text-sm leading-[1.15] outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline focus-visible:ring-offset-2 focus-visible:ring-offset-cp-bg-container"
                :class="[
                  isCollapsed ? 'w-11.5 justify-center' : 'w-full gap-3 px-4',
                  isActive(item.path)
                    ? navFeedbackMuted
                      ? 'font-bold text-cp-text transition-none'
                      : 'font-bold text-cp-text transition-colors duration-200'
                    : navFeedbackMuted
                      ? 'font-semibold text-cp-text-secondary transition-none'
                      : 'font-semibold text-cp-text-secondary transition-colors duration-200 hover:bg-cp-fill-quaternary hover:text-cp-text',
                ]"
                :aria-expanded="isCollapsed ? undefined : pluginGroupExpanded"
                :aria-controls="isCollapsed ? undefined : pluginMenuId"
                :aria-label="isCollapsed ? item.label : undefined"
                :title="isCollapsed ? item.label : undefined"
                @click="togglePluginGroup"
              >
                <component :is="item.icon" class="shrink-0" :size="20" />
                <span
                  class="sidebar-label min-w-0 flex-1 overflow-hidden whitespace-nowrap text-left transition-[opacity,transform] duration-200"
                  :class="isCollapsed ? 'hidden' : 'w-auto'"
                >
                  {{ item.label }}
                </span>
                <span
                  v-if="!isCollapsed"
                  class="sidebar-label inline-flex size-4 shrink-0"
                  aria-hidden="true"
                >
                  <ChevronDown
                    class="size-4 transition-transform duration-200 motion-reduce:transition-none"
                    :class="pluginGroupExpanded ? undefined : '-rotate-90'"
                  />
                </span>
              </button>

              <div
                :id="pluginMenuId"
                class="grid transition-[grid-template-rows] duration-260 ease-[cubic-bezier(0.22,1,0.36,1)] motion-reduce:transition-none"
                :class="pluginMenuVisible ? 'grid-rows-[1fr]' : 'grid-rows-[0fr]'"
                :inert="!pluginMenuVisible"
                aria-label="插件导航"
              >
                <div class="min-h-0 overflow-hidden">
                  <div class="relative ml-3 grid gap-1 pt-1 pl-3">
                    <span
                      aria-hidden="true"
                      class="pointer-events-none absolute top-1 right-0 left-3 h-10 overflow-hidden rounded-cp bg-cp-menu-item-selected-bg transition-[transform,opacity] duration-260 ease-[cubic-bezier(0.22,1,0.36,1)] motion-reduce:transition-none"
                      :style="activePluginIndicatorStyle"
                    >
                      <span ref="pluginNavSignal" class="absolute inset-y-0 left-0 w-2/3 opacity-0 [background:linear-gradient(90deg,transparent,color-mix(in_srgb,var(--cp-color-info)_9%,transparent),transparent)]" />
                    </span>
                    <button
                      type="button"
                      class="relative inline-flex h-10 min-w-0 cursor-pointer items-center gap-2 rounded-cp border-0 bg-transparent px-3 text-left text-cp-sm outline-none transition-colors focus-visible:ring-2 focus-visible:ring-cp-control-outline motion-reduce:transition-none"
                      :class="route.name === 'plugins'
                        ? 'font-emphasis text-cp-text'
                        : navFeedbackMuted ? 'font-semibold text-cp-text-secondary' : 'font-semibold text-cp-text-secondary hover:bg-cp-fill-quaternary hover:text-cp-text'"
                      :aria-current="route.name === 'plugins' ? 'page' : undefined"
                      @click="navigate('/plugins')"
                    >
                      <PanelsTopLeft class="block size-4 shrink-0" />
                      <span class="sidebar-label truncate leading-none">插件管理</span>
                    </button>
                    <button
                      v-for="view in pluginPageViews"
                      :key="view.target.instanceId"
                      type="button"
                      class="relative inline-flex h-10 min-w-0 cursor-pointer items-center gap-2 rounded-cp border-0 bg-transparent px-3 text-left text-cp-sm outline-none transition-colors focus-visible:ring-2 focus-visible:ring-cp-control-outline motion-reduce:transition-none"
                      :class="isPluginViewActive(view)
                        ? 'font-emphasis text-cp-text'
                        : navFeedbackMuted ? 'font-semibold text-cp-text-secondary' : 'font-semibold text-cp-text-secondary hover:bg-cp-fill-quaternary hover:text-cp-text'"
                      :aria-current="isPluginViewActive(view) ? 'page' : undefined"
                      @click="navigatePluginView(view)"
                    >
                      <Blocks class="block size-4 shrink-0" />
                      <span class="sidebar-label truncate leading-none">{{ pluginViewLabel(view) }}</span>
                    </button>
                  </div>
                </div>
              </div>
            </div>

            <button
              v-else
              type="button"
              class="relative z-10 inline-flex h-11.5 cursor-pointer items-center rounded-cp border-0 text-sm leading-[1.15] outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline focus-visible:ring-offset-2 focus-visible:ring-offset-cp-bg-container"
              :class="[
                isCollapsed ? 'w-11.5 justify-center' : 'w-full gap-3 px-4',
                isActive(item.path)
                  ? navFeedbackMuted
                    ? 'bg-transparent font-bold text-cp-text transition-none'
                    : 'bg-transparent font-bold text-cp-text transition-colors duration-200'
                  : navFeedbackMuted
                    ? 'bg-transparent font-semibold text-cp-text-secondary transition-none'
                    : 'bg-transparent font-semibold text-cp-text-secondary transition-colors duration-200 hover:bg-cp-fill-quaternary hover:text-cp-text',
              ]"
              @click="navigate(item.path)"
            >
              <component :is="item.icon" class="shrink-0" :size="20" />
              <span
                class="sidebar-label overflow-hidden whitespace-nowrap transition-[opacity,transform] duration-200"
                :class="isCollapsed ? 'pointer-events-none w-0' : 'w-auto'"
              >
                {{ item.label }}
              </span>
            </button>
          </template>
        </nav>
      </div>
    </BaseScrollbar>

    <div class="mx-4 mb-6 shrink-0" :class="isCollapsed ? 'w-11' : 'self-stretch'">
      <div
        class="bg-cp-fill-quaternary"
        :class="isCollapsed ? 'grid gap-1 rounded-cp p-1' : 'flex h-11 items-center justify-between rounded-cp-lg px-2'"
      >
        <span
          v-if="!isCollapsed"
          class="inline-flex whitespace-nowrap h-7 items-center gap-1.5 rounded-lg bg-cp-success-container px-2.5 text-xs leading-none font-emphasis text-cp-success-on-container"
        >
          <i class="size-1.5 rounded-full bg-cp-success" />
          在线
        </span>

        <div class="flex items-center" :class="isCollapsed ? 'grid gap-1' : 'gap-1'">
          <BaseIconButton
            v-if="isCollapsed && hasUpdate"
            variant="success"
            size="md"
            :label="updateButtonLabel"
            @click="openSystemUpdate"
          >
            <ArrowUpCircle :size="19" />
          </BaseIconButton>

          <BaseIconButton
            :size="isCollapsed ? 'md' : 'sm'"
            label="退出登录"
            variant="destructive"
            @click="handleLogout"
          >
            <LogOut :size="isCollapsed ? 19 : 18" />
          </BaseIconButton>

          <BaseIconButton
            variant="ghost"
            :size="isCollapsed ? 'md' : 'sm'"
            :label="themeToggleLabel"
            @click="toggleTheme($event)"
          >
            <Sun v-if="effectiveTheme === 'dark'" :size="isCollapsed ? 19 : 18" />
            <Moon v-else :size="isCollapsed ? 19 : 18" />
          </BaseIconButton>

          <BaseIconButton variant="ghost" :size="isCollapsed ? 'md' : 'sm'" label="关于" @click="emit('openAbout')">
            <Info :size="isCollapsed ? 19 : 18" />
          </BaseIconButton>

          <BaseIconButton v-if="mobile" variant="ghost" size="sm" label="关闭侧边栏" @click="emit('close')">
            <PanelLeftClose :size="18" />
          </BaseIconButton>

          <BaseIconButton
            v-else
            variant="ghost"
            :size="isCollapsed ? 'md' : 'sm'"
            data-sidebar-toggle
            :label="isCollapsed ? '展开侧边栏' : '收缩侧边栏'"
            @click="emit('toggle')"
          >
            <PanelLeftOpen v-if="isCollapsed" :size="19" />
            <PanelLeftClose v-else :size="18" />
          </BaseIconButton>
        </div>
      </div>
    </div>
  </aside>
</template>
