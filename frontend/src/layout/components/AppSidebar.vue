<script setup lang="ts">
import { usePreferredReducedMotion, useTimeoutFn } from '@vueuse/core'
import { gsap } from 'gsap'
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  shallowRef,
  useTemplateRef,
  watch,
} from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { storeToRefs } from 'pinia'
import {
  Cat,
  ChartNoAxesColumn,
  ArrowUpCircle,
  Info,
  KeyRound,
  LayoutDashboard,
  LogOut,
  Moon,
  PanelLeftClose,
  PanelLeftOpen,
  Settings,
  Sun,
  Users,
} from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseMotionIcon from '@/components/base/BaseMotionIcon.vue'
import { useSystemUpdate } from '@/composables/useSystemUpdate'
import { useAuthStore } from '@/stores/modules/auth'
import { useUiStore } from '@/stores/modules/ui'

import AppAboutModal from './AppAboutModal.vue'
import SystemUpdateModal from './SystemUpdateModal.vue'

const route = useRoute()
const router = useRouter()
const authStore = useAuthStore()
const uiStore = useUiStore()
const { effectiveTheme } = storeToRefs(uiStore)
const { toggleTheme } = uiStore
const preferredMotion = usePreferredReducedMotion()
const { version, hasUpdate, loadedOnce, loadVersion, loadSystem } = useSystemUpdate()

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
  toggle: []
}>()

const navItems = [
  { label: '概览', icon: LayoutDashboard, path: '/' },
  { label: '账号管理', icon: Users, path: '/accounts' },
  { label: 'API 密钥', icon: KeyRound, path: '/api-keys' },
  { label: '使用统计', icon: ChartNoAxesColumn, path: '/usage' },
  { label: '系统设置', icon: Settings, path: '/settings' },
]

const isActive = (path: string) => {
  if (path === '/') return route.path === '/'
  return route.path.startsWith(path)
}

const activeNavIndex = computed(() => {
  const index = navItems.findIndex((item) => isActive(item.path))
  return Math.max(0, index)
})
const activeNavIndicatorStyle = computed(() => ({
  transform: `translate3d(0, ${activeNavIndex.value * 58}px, 0)`,
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

const systemUpdateOpening = shallowRef(false)

async function openSystemUpdate() {
  if (systemUpdateOpen.value || systemUpdateOpening.value) return

  systemUpdateOpening.value = true
  try {
    if (!loadedOnce.value) {
      await loadSystem(false)
    }
  } catch {
    // 弹窗打开后由弹窗内的加载逻辑提示失败原因
  } finally {
    systemUpdateOpening.value = false
    systemUpdateOpen.value = true
  }
}

async function handleLogout() {
  await authStore.logout()
  await router.push('/login')
  emit('navigate')
}

const sidebarEl = ref<HTMLElement | null>(null)
const brandLabelEl = ref<HTMLElement | null>(null)
const navSignalEl = useTemplateRef<HTMLElement>('navSignal')
const systemUpdateOpen = shallowRef(false)
const aboutOpen = shallowRef(false)
const isCollapsed = computed(() => !props.mobile && Boolean(props.collapsed))
const sidebarWidth = computed(() => (isCollapsed.value ? 88 : 256))
const brandLabelVisible = shallowRef(!isCollapsed.value)
const themeToggleLabel = computed(() =>
  effectiveTheme.value === 'dark' ? '切换浅色模式' : '切换暗黑模式',
)
const versionText = computed(() => version.value?.version.trim() ?? '')
const hasVersionLabel = computed(() => versionText.value.length > 0)
const versionLabel = computed(() => `v${versionText.value}`)
const updateButtonLabel = computed(() =>
  hasUpdate.value ? '发现新版本，打开系统更新' : '打开系统更新',
)

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

  const targetWidth = collapsed ? 88 : 256
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
  const signal = navSignalEl.value
  if (!signal) return

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
  gsap.set(sidebarEl.value, {
    width: sidebarWidth.value,
    flexBasis: sidebarWidth.value,
  })
  if (isCollapsed.value) {
    hideBrandLabel()
  } else {
    gsap.set(brandLabelEl.value, {
      opacity: 1,
      x: 0,
    })
  }
  animateSidebarLabels(isCollapsed.value)
  gsap.set(navSignalEl.value, { opacity: 0, xPercent: -70 })
  void loadVersion().catch(() => undefined)
})

watch(
  () => isCollapsed.value,
  async (collapsed) => {
    if (collapsed) {
      hideBrandLabel()
    } else {
      brandLabelVisible.value = true
    }
    animateSidebarLabels(Boolean(collapsed))
    animateSidebarWidth(Boolean(collapsed))
    await nextTick()
    if (!collapsed) {
      animateBrandLabelEnter()
    }
    animateSidebarLabels(Boolean(collapsed))
  },
)

watch(
  () => route.path,
  async (path, previousPath) => {
    if (path === previousPath) return
    muteNavFeedbackDuringMove()
    await nextTick()
    animateNavSignal()
  },
  { flush: 'post' },
)

onBeforeUnmount(() => {
  stopNavFeedbackRestore()
  const targets = [sidebarEl.value, brandLabelEl.value, navSignalEl.value].filter(
    (target): target is HTMLElement => Boolean(target),
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
    class="z-20 h-dvh shrink-0 flex-col overflow-hidden bg-(--cp-bg-surface) px-4 shadow-(--cp-shadow-sidebar)"
    :class="[
      mobile ? 'flex' : 'hidden min-[961px]:flex',
      isCollapsed ? 'w-22 basis-22 items-center' : 'w-64 basis-64',
    ]"
  >
    <div
      class="mt-6 grid h-12 grid-cols-[44px_minmax(0,1fr)] items-center"
      :class="isCollapsed ? 'w-11 justify-start' : 'w-full gap-3'"
    >
      <BaseMotionIcon
        aria-hidden="true"
        variant="brand"
        class="inline-flex size-11 items-center justify-center relative -top-0.5 rounded-(--cp-icon-button-radius) bg-(--cp-bg-muted) text-(--cp-text-primary)"
      >
        <Cat :size="27" stroke-width="2" />
      </BaseMotionIcon>
      <span
        v-show="brandLabelVisible"
        ref="brandLabelEl"
        class="grid min-w-33 content-center overflow-hidden"
      >
        <strong class="text-base leading-[1.1] font-[760] text-(--cp-text-primary)">
          Codex Proxy
        </strong>
        <span class="mt-1.5 flex h-4.5 min-w-0 items-center gap-2">
          <span class="shrink-0 text-xs leading-none font-[650] text-(--cp-text-secondary)">
            Rust build
          </span>
          <button
            v-if="hasVersionLabel"
            type="button"
            class="inline-flex h-4.5 min-w-0 cursor-pointer items-center gap-1 rounded-(--cp-input-radius-small) border-0 px-1.5 font-mono text-[10px] leading-none font-[720] transition-colors outline-none focus-visible:ring-2 focus-visible:ring-(--cp-info-border) focus-visible:ring-offset-2 focus-visible:ring-offset-(--cp-bg-surface)"
            :class="[
              hasUpdate
                ? 'bg-(--cp-success-bg) text-(--cp-success-text) hover:bg-(--cp-success-bg-hover)'
                : 'bg-(--cp-bg-subtle) text-(--cp-text-muted) hover:bg-(--cp-bg-muted) hover:text-(--cp-text-secondary)',
              systemUpdateOpening ? 'cursor-wait opacity-70' : '',
            ]"
            :title="updateButtonLabel"
            :disabled="systemUpdateOpening"
            @click="openSystemUpdate"
          >
            <span>{{ versionLabel }}</span>
            <ArrowUpCircle v-if="hasUpdate" class="size-3 shrink-0 text-(--cp-success)" />
          </button>
        </span>
      </span>
    </div>

    <nav
      class="relative mt-7 grid gap-3"
      :class="isCollapsed ? 'w-11.5' : 'w-full'"
      aria-label="主导航"
    >
      <span
        aria-hidden="true"
        class="pointer-events-none absolute inset-x-0 top-0 h-11.5 overflow-hidden rounded-(--cp-icon-button-radius) bg-(--cp-bg-nav-active) transition-transform duration-260 ease-[cubic-bezier(0.22,1,0.36,1)] motion-reduce:transition-none"
        :style="activeNavIndicatorStyle"
      >
        <span ref="navSignal" class="sidebar-active-signal absolute inset-y-0 left-0 w-2/3" />
      </span>
      <button
        v-for="item in navItems"
        :key="item.label"
        type="button"
        class="relative z-10 inline-flex h-11.5 cursor-pointer items-center rounded-(--cp-icon-button-radius) border-0 text-sm leading-[1.15] outline-none focus-visible:ring-2 focus-visible:ring-(--cp-info-border) focus-visible:ring-offset-2 focus-visible:ring-offset-(--cp-bg-surface)"
        :class="[
          isCollapsed ? 'w-11.5 justify-center' : 'w-full gap-3 px-4',
          isActive(item.path)
            ? navFeedbackMuted
              ? 'bg-transparent font-bold text-(--cp-text-primary) transition-none'
              : 'bg-transparent font-bold text-(--cp-text-primary) transition-colors duration-200'
            : navFeedbackMuted
              ? 'bg-transparent font-semibold text-(--cp-text-secondary) transition-none'
              : 'bg-transparent font-semibold text-(--cp-text-secondary) transition-colors duration-200 hover:bg-(--cp-bg-subtle) hover:text-(--cp-text-primary)',
        ]"
        @click="navigate(item.path)"
      >
        <component :is="item.icon" class="shrink-0" :size="20" />
        <span
          class="sidebar-label overflow-hidden whitespace-nowrap transition-[opacity,transform] duration-200"
          :class="isCollapsed ? 'pointer-events-none w-0' : 'w-auto'"
          >{{ item.label }}</span
        >
      </button>
    </nav>

    <div class="mt-auto mb-6" :class="isCollapsed ? 'w-11' : 'w-full'">
      <div
        class="bg-(--cp-bg-subtle)"
        :class="
          isCollapsed
            ? 'grid gap-1 rounded-(--cp-icon-button-radius) p-1'
            : 'flex h-11 items-center justify-between rounded-(--cp-panel-radius) px-2'
        "
      >
        <span
          v-if="!isCollapsed"
          class="inline-flex whitespace-nowrap h-7 items-center gap-1.5 rounded-lg bg-(--cp-success-bg) px-2.5 text-xs leading-none font-[650] text-(--cp-success-text)"
        >
          <i class="size-1.5 rounded-full bg-(--cp-success)" />
          在线
        </span>

        <div class="flex items-center" :class="isCollapsed ? 'grid gap-1' : 'gap-1'">
          <BaseButton
            v-if="isCollapsed && hasUpdate"
            icon-only
            :variant="hasUpdate ? 'success' : 'ghost'"
            size="default"
            :label="updateButtonLabel"
            :loading="systemUpdateOpening"
            @click="openSystemUpdate"
          >
            <ArrowUpCircle :size="19" />
          </BaseButton>

          <BaseButton
            icon-only
            variant="ghost"
            :size="isCollapsed ? 'default' : 'sm'"
            label="退出登录"
            class="hover:bg-(--cp-danger-bg) hover:text-(--cp-danger)"
            @click="handleLogout"
          >
            <LogOut :size="isCollapsed ? 19 : 18" />
          </BaseButton>

          <BaseButton
            icon-only
            variant="ghost"
            :size="isCollapsed ? 'default' : 'sm'"
            :label="themeToggleLabel"
            @click="toggleTheme($event)"
          >
            <Sun v-if="effectiveTheme === 'dark'" :size="isCollapsed ? 19 : 18" />
            <Moon v-else :size="isCollapsed ? 19 : 18" />
          </BaseButton>

          <BaseButton
            icon-only
            variant="ghost"
            :size="isCollapsed ? 'default' : 'sm'"
            label="关于"
            @click="aboutOpen = true"
          >
            <Info :size="isCollapsed ? 19 : 18" />
          </BaseButton>

          <BaseButton
            v-if="mobile"
            icon-only
            variant="ghost"
            size="sm"
            label="关闭侧边栏"
            @click="emit('close')"
          >
            <PanelLeftClose :size="18" />
          </BaseButton>

          <BaseButton
            v-else
            icon-only
            variant="ghost"
            :size="isCollapsed ? 'default' : 'sm'"
            data-sidebar-toggle
            :label="isCollapsed ? '展开侧边栏' : '收缩侧边栏'"
            @click="emit('toggle')"
          >
            <PanelLeftOpen v-if="isCollapsed" :size="19" />
            <PanelLeftClose v-else :size="18" />
          </BaseButton>
        </div>
      </div>
    </div>
  </aside>

  <AppAboutModal v-model="aboutOpen" />
  <SystemUpdateModal v-model="systemUpdateOpen" />
</template>

<style scoped>
.sidebar-active-signal {
  background: linear-gradient(
    90deg,
    transparent,
    color-mix(in srgb, var(--cp-info) 9%, transparent),
    transparent
  );
}
</style>
