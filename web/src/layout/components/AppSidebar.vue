<script setup lang="ts">
import { gsap } from 'gsap'
import { nextTick, onMounted, ref, watch } from 'vue'
import {
  Box,
  ChartColumn,
  KeyRound,
  LayoutDashboard,
  PanelLeftClose,
  PanelLeftOpen,
  Radar,
  ScrollText,
  Settings,
  SquareTerminal,
  Users,
} from '@lucide/vue'

const navItems = [
  { label: '概览', icon: LayoutDashboard, active: true },
  { label: '账号池', icon: Users, active: false },
  { label: 'API Keys', icon: KeyRound, active: false },
  { label: '日志', icon: ScrollText, active: false },
  { label: '用量', icon: ChartColumn, active: false },
  { label: '模型', icon: Box, active: false },
  { label: '设置', icon: Settings, active: false },
  { label: '诊断', icon: Radar, active: false },
]

const props = withDefaults(defineProps<{
  collapsed?: boolean
}>(), {
  collapsed: false,
})

defineEmits<{
  toggle: []
}>()

const sidebarEl = ref<HTMLElement | null>(null)

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
  animateSidebarLabels(Boolean(props.collapsed))
})

watch(
  () => props.collapsed,
  async (collapsed) => {
    animateSidebarLabels(Boolean(collapsed))
    animateSidebarWidth(Boolean(collapsed))
    await nextTick()
    animateSidebarLabels(Boolean(collapsed))
  },
)
</script>

<template>
  <aside
    ref="sidebarEl"
    class="z-20 hidden h-screen shrink-0 flex-col overflow-hidden bg-white shadow-[var(--cp-shadow-sidebar)] min-[961px]:flex"
    :class="collapsed ? 'w-[88px] basis-[88px] items-center' : 'w-[256px] basis-[256px]'"
  >
    <div
      class="mt-[31px] grid h-[46px] grid-cols-[46px_minmax(0,1fr)] gap-2.5"
      :class="collapsed ? 'w-[46px]' : 'ml-6 w-[188px] self-start'"
    >
      <span class="inline-flex size-[46px] items-center justify-center rounded-[13px] bg-[#111827] text-white shadow-[0_8px_18px_-16px_#0E172614]">
        <SquareTerminal :size="24" />
      </span>
      <span class="sidebar-label grid min-w-[132px] content-center overflow-hidden transition-[opacity,transform] duration-200" :class="collapsed ? 'pointer-events-none w-0' : 'w-auto'">
        <strong class="text-[17px] leading-[1.1] font-[720] text-[#111827]">Codex</strong>
        <span class="mt-1 text-[12px] leading-[1.1] font-semibold text-[#64748B]">Proxy RS · v0.1.0</span>
      </span>
    </div>

    <nav
      class="mt-[35px] grid gap-3"
      :class="collapsed ? '' : 'ml-6 self-start'"
      aria-label="主导航"
    >
      <a
        v-for="item in navItems"
        :key="item.label"
        class="inline-flex h-[46px] items-center rounded-xl text-[14px] leading-[1.15] no-underline transition-[background-color,color,transform] duration-200 hover:-translate-y-px active:translate-y-0"
        :class="[
          collapsed ? 'w-[46px] justify-center' : 'w-[208px] gap-3 pl-[22px]',
          item.active
            ? 'bg-[#E9EEF5] font-bold text-[#111827]'
            : 'bg-transparent font-semibold text-[#64748B] hover:bg-[#F8FAFC]',
        ]"
        href="#"
      >
        <component :is="item.icon" class="shrink-0" :size="20" />
        <span class="sidebar-label overflow-hidden whitespace-nowrap transition-[opacity,transform] duration-200" :class="collapsed ? 'pointer-events-none w-0' : 'w-auto'">{{ item.label }}</span>
      </a>
    </nav>

    <button
      class="mt-auto mb-8 inline-flex items-center justify-center rounded-xl border-0 text-[#64748B] transition-[background-color,transform,color] duration-200 hover:-translate-y-px active:translate-y-0"
      :class="collapsed ? 'h-9 w-11 bg-transparent' : 'mr-6 size-9 self-end bg-[#F8FAFC]'"
      type="button"
      data-sidebar-toggle
      :aria-label="collapsed ? '展开侧边栏' : '收缩侧边栏'"
      @click="$emit('toggle')"
    >
      <PanelLeftOpen v-if="collapsed" :size="20" />
      <PanelLeftClose v-else :size="20" />
    </button>
  </aside>
</template>
