<script setup lang="ts">
import { gsap } from 'gsap'
import { nextTick, onMounted, ref, shallowRef, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import {
  KeyRound,
  LayoutDashboard,
  PanelLeftClose,
  PanelLeftOpen,
  ScrollText,
  Settings,
  SquareTerminal,
  Users,
} from '@lucide/vue'

const route = useRoute()
const router = useRouter()

const navItems = [
  { label: '概览', icon: LayoutDashboard, path: '/' },
  { label: '账号管理', icon: Users, path: '/accounts' },
  { label: 'API 密钥', icon: KeyRound, path: '/api-keys' },
  { label: '使用统计', icon: ScrollText, path: '/logs' },
  { label: '系统设置', icon: Settings, path: '/settings' },
]

const isActive = (path: string) => {
  if (path === '/') return route.path === '/'
  return route.path.startsWith(path)
}

function navigate(path: string) {
  router.push(path)
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
      class="mt-5 grid h-11.5 grid-cols-[46px_minmax(0,1fr)] gap-2.5"
      :class="collapsed ? 'w-11.5' : 'w-full'"
    >
      <span
        class="inline-flex size-11 items-center justify-center rounded-[13px] bg-gray-900 text-white shadow-[0_8px_18px_-16px_#0E172614]"
      >
        <SquareTerminal :size="22" />
      </span>
      <span
        v-show="brandLabelVisible"
        ref="brandLabelEl"
        class="grid min-w-33 content-center overflow-hidden"
      >
        <strong class="text-[17px] leading-[1.1] font-[720] text-gray-900">Codex</strong>
        <span class="mt-1 text-xs leading-[1.1] font-semibold text-slate-500"
          >Proxy RS · v0.1.0</span
        >
      </span>
    </div>

    <nav class="mt-6 grid gap-3" :class="collapsed ? '' : 'w-full'" aria-label="主导航">
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

    <button
      class="mt-auto mb-8 inline-flex items-center justify-center rounded-xl border-0 text-slate-500 transition-colors duration-200"
      :class="collapsed ? 'h-9 w-11 bg-transparent' : 'size-9 self-end bg-slate-50'"
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
