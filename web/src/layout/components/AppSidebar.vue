<script setup lang="ts">
import { gsap } from 'gsap'
import { nextTick, onMounted, ref, shallowRef, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import {
  Cat,
  KeyRound,
  LayoutDashboard,
  List,
  LogOut,
  PanelLeftClose,
  PanelLeftOpen,
  Settings,
  Users,
} from '@lucide/vue'

import { useAuthStore } from '@/stores/modules/auth'

const route = useRoute()
const router = useRouter()
const authStore = useAuthStore()

const navItems = [
  { label: '概览', icon: LayoutDashboard, path: '/' },
  { label: '账号管理', icon: Users, path: '/accounts' },
  { label: 'API 密钥', icon: KeyRound, path: '/api-keys' },
  { label: '事件日志', icon: List, path: '/logs' },
  { label: '系统设置', icon: Settings, path: '/settings' },
]

const isActive = (path: string) => {
  if (path === '/') return route.path === '/'
  return route.path.startsWith(path)
}

function navigate(path: string) {
  router.push(path)
}

async function handleLogout() {
  await authStore.logout()
  router.push('/login')
}

const props = withDefaults(
  defineProps<{
    collapsed?: boolean
  }>(),
  {
    collapsed: false,
  },
)

defineEmits<{
  toggle: []
}>()

const sidebarEl = ref<HTMLElement | null>(null)
const brandLabelEl = ref<HTMLElement | null>(null)
const brandLabelVisible = shallowRef(!props.collapsed)

function prefersReducedMotion() {
  return window.matchMedia('(prefers-reduced-motion: reduce)').matches
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

onMounted(() => {
  gsap.set(sidebarEl.value, {
    width: props.collapsed ? 88 : 256,
    flexBasis: props.collapsed ? 88 : 256,
  })
  if (props.collapsed) {
    hideBrandLabel()
  } else {
    gsap.set(brandLabelEl.value, {
      opacity: 1,
      x: 0,
    })
  }
  animateSidebarLabels(Boolean(props.collapsed))
})

watch(
  () => props.collapsed,
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
</script>

<template>
  <aside
    ref="sidebarEl"
    class="z-20 hidden h-screen shrink-0 flex-col overflow-hidden bg-white px-4 shadow-(--cp-shadow-sidebar) min-[961px]:flex"
    :class="collapsed ? 'w-22 basis-22 items-center' : 'w-64 basis-64'"
  >
    <div
      class="mt-5 grid h-12 grid-cols-[44px_minmax(0,1fr)] items-center"
      :class="collapsed ? 'w-12 justify-start' : 'w-full gap-3'"
    >
      <span
        class="inline-flex size-11 items-center justify-center rounded-xl bg-slate-100 text-slate-700"
      >
        <Cat :size="27" stroke-width="2.05" />
      </span>
      <span
        v-show="brandLabelVisible"
        ref="brandLabelEl"
        class="grid min-w-33 content-center overflow-hidden"
      >
        <strong class="text-base leading-[1.1] font-[760] text-gray-900">Codex</strong>
        <span class="mt-1 text-xs leading-[1.1] font-semibold text-slate-500"
          >Proxy RS · v0.1.0</span
        >
      </span>
    </div>

    <nav class="mt-7 grid gap-3" :class="collapsed ? '' : 'w-full'" aria-label="主导航">
      <button
        v-for="item in navItems"
        :key="item.label"
        type="button"
        class="inline-flex h-11.5 items-center rounded-xl text-sm leading-[1.15] border-0 cursor-pointer transition-colors duration-200"
        :class="[
          collapsed ? 'w-11.5 justify-center' : 'w-full gap-3 px-4',
          isActive(item.path)
            ? 'bg-[#E9EEF5] font-bold text-gray-900'
            : 'bg-transparent font-semibold text-slate-500 hover:bg-slate-50',
        ]"
        @click="navigate(item.path)"
      >
        <component :is="item.icon" class="shrink-0" :size="20" />
        <span
          class="sidebar-label overflow-hidden whitespace-nowrap transition-[opacity,transform] duration-200"
          :class="collapsed ? 'pointer-events-none w-0' : 'w-auto'"
          >{{ item.label }}</span
        >
      </button>
    </nav>

    <div class="mt-auto mb-8" :class="collapsed ? 'w-11' : 'w-full'">
      <div
        class="bg-slate-50/80"
        :class="
          collapsed
            ? 'grid gap-1 rounded-xl p-1'
            : 'flex h-11 items-center justify-between rounded-2xl px-2'
        "
      >
        <span
          v-if="!collapsed"
          class="inline-flex h-7 items-center gap-1.5 rounded-lg bg-(--cp-success-bg) px-2.5 text-xs leading-none font-[650] text-(--cp-success-text)"
        >
          <i class="size-1.5 rounded-full bg-(--cp-success)" />
          在线
        </span>

        <div class="flex items-center" :class="collapsed ? 'grid gap-1' : 'gap-1'">
          <button
            class="inline-flex items-center justify-center rounded-xl border-0 bg-transparent text-slate-500 transition-colors duration-200 hover:bg-slate-100 hover:text-slate-700"
            :class="collapsed ? 'size-9' : 'size-8'"
            type="button"
            data-sidebar-toggle
            :aria-label="collapsed ? '展开侧边栏' : '收缩侧边栏'"
            :title="collapsed ? '展开侧边栏' : '收缩侧边栏'"
            @click="$emit('toggle')"
          >
            <PanelLeftOpen v-if="collapsed" :size="19" />
            <PanelLeftClose v-else :size="18" />
          </button>

          <button
            class="inline-flex items-center justify-center rounded-xl border-0 bg-transparent text-slate-500 transition-colors duration-200 hover:bg-(--cp-danger-bg) hover:text-(--cp-danger)"
            :class="collapsed ? 'size-9' : 'size-8'"
            type="button"
            aria-label="退出登录"
            title="退出登录"
            @click="handleLogout"
          >
            <LogOut :size="collapsed ? 19 : 18" />
          </button>
        </div>
      </div>
    </div>
  </aside>
</template>
