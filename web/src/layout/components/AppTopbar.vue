<script setup lang="ts">
import { gsap } from 'gsap'
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import {
  CalendarDays,
  ChevronDown,
  ChevronRight,
  ChevronUp,
  LogOut,
  RefreshCw,
  ScrollText,
  ShieldCheck,
  User,
} from '@lucide/vue'

import { useAuthStore } from '@/stores/modules/auth'

const router = useRouter()
const authStore = useAuthStore()
const accountMenuOpen = ref(false)

async function handleLogout() {
  await authStore.logout()
  router.push('/login')
}

function prefersReducedMotion() {
  return window.matchMedia('(prefers-reduced-motion: reduce)').matches
}

function animateAccountMenuEnter(el: Element, done: () => void) {
  const target = el as HTMLElement

  if (prefersReducedMotion()) {
    gsap.set(target, { opacity: 1, y: 0, scale: 1 })
    done()
    return
  }

  gsap.fromTo(
    target,
    { opacity: 0, y: 6, scale: 0.98 },
    {
      opacity: 1,
      y: 0,
      scale: 1,
      duration: 0.22,
      ease: 'power3.out',
      onComplete: done,
    },
  )
}

function animateAccountMenuLeave(el: Element, done: () => void) {
  const target = el as HTMLElement

  if (prefersReducedMotion()) {
    gsap.set(target, { opacity: 0 })
    done()
    return
  }

  gsap.to(target, {
    opacity: 0,
    y: 4,
    scale: 0.985,
    duration: 0.14,
    ease: 'power2.in',
    onComplete: done,
  })
}
</script>

<template>
  <div class="relative z-30 flex h-11 items-start gap-3.5">
    <button
      class="group inline-flex h-11 w-42.5 items-center gap-2.5 whitespace-nowrap rounded-xl border-0 bg-white px-3.5 text-sm leading-[1.15] font-[650] text-(--cp-text-primary) shadow-(--cp-shadow-control) transition-shadow duration-200"
      type="button"
    >
      <CalendarDays class="text-(--cp-info)" :size="18" />
      <span class="whitespace-nowrap">最近 24 小时</span>
      <ChevronDown
        class="text-(--cp-text-secondary) transition-transform duration-200 group-hover:rotate-180"
        :size="16"
      />
    </button>

    <button
      class="inline-flex size-11 items-center justify-center rounded-xl border-0 bg-white text-(--cp-normal) shadow-(--cp-shadow-control) transition-shadow duration-200"
      type="button"
      aria-label="刷新"
    >
      <RefreshCw :size="19" />
    </button>

    <div class="relative ml-0.5">
      <button
        class="group grid h-11 w-54 grid-cols-[28px_minmax(0,1fr)_16px] items-center rounded-xl border-0 bg-white px-3 text-(--cp-text-secondary) shadow-(--cp-shadow-control) transition-shadow duration-200"
        type="button"
        data-account-trigger
        @click="accountMenuOpen = !accountMenuOpen"
      >
        <span
          class="inline-flex size-7 items-center justify-center rounded-full bg-(--cp-normal) text-xs leading-[1.15] font-bold text-white"
          >A</span
        >
        <span class="grid gap-1.25 pl-3 text-left">
          <strong class="text-[13px] leading-[1.15] font-bold text-(--cp-text-primary)"
            >admin</strong
          >
          <span class="text-[11px] leading-[1.15] font-[650] text-(--cp-text-secondary)"
            >管理员</span
          >
        </span>
        <ChevronUp
          v-if="accountMenuOpen"
          class="text-(--cp-text-secondary) transition-transform duration-200 group-hover:rotate-180"
          :size="16"
        />
        <ChevronDown
          v-else
          class="text-(--cp-text-secondary) transition-transform duration-200 group-hover:rotate-180"
          :size="16"
        />
      </button>

      <Transition :css="false" @enter="animateAccountMenuEnter" @leave="animateAccountMenuLeave">
        <div
          v-if="accountMenuOpen"
          data-account-menu
          class="absolute right-0 top-14 h-76 w-80 origin-top-right rounded-[18px] bg-white pt-4.5 shadow-(--cp-shadow-popover)"
        >
          <div class="mx-4.5 grid h-11 grid-cols-[44px_minmax(0,1fr)_64px] items-center gap-3">
            <span
              class="inline-flex size-11 items-center justify-center rounded-full bg-(--cp-normal) text-sm leading-[1.15] font-bold text-white"
              >A</span
            >
            <span class="grid gap-1.25">
              <strong class="text-[15px] leading-[1.15] font-bold text-(--cp-text-primary)"
                >admin</strong
              >
              <span class="text-xs leading-[1.15] font-[650] text-(--cp-text-secondary)"
                >管理员</span
              >
            </span>
            <span
              class="inline-flex h-7 w-16 items-center gap-1.5 rounded-full bg-(--cp-success-bg) px-3"
            >
              <i class="size-1.5 rounded-full bg-(--cp-success)" />
              <span class="text-xs leading-[1.15] font-[650] text-(--cp-success-text)">在线</span>
            </span>
          </div>

          <div
            class="mx-4 mt-3.5 grid h-11 w-72 grid-cols-2 rounded-xl bg-(--cp-bg-subtle) px-4 py-2"
          >
            <span class="grid gap-1.25">
              <small class="text-[11px] leading-[1.15] font-semibold text-(--cp-text-secondary)"
                >MFA</small
              >
              <strong class="text-[13px] leading-[1.15] font-bold text-(--cp-text-primary)"
                >已开启</strong
              >
            </span>
            <span class="grid gap-1.25 pl-4.5">
              <small class="text-[11px] leading-[1.15] font-semibold text-(--cp-text-secondary)"
                >会话</small
              >
              <strong class="text-[13px] leading-[1.15] font-bold text-(--cp-text-primary)"
                >1 台</strong
              >
            </span>
          </div>

          <div class="mx-4 mt-3.5 grid w-72 gap-1.5">
            <button
              class="grid h-9 grid-cols-[20px_minmax(0,1fr)_16px] items-center gap-3 rounded-[11px] border-0 bg-(--cp-bg-subtle) px-3 text-left text-(--cp-text-primary) transition-colors duration-200"
              type="button"
            >
              <User :size="20" />
              <span class="text-[13px] leading-[1.15] font-[650]">个人设置</span>
              <ChevronRight class="text-(--cp-text-muted)" :size="16" />
            </button>

            <button
              class="grid h-9 grid-cols-[20px_minmax(0,1fr)_34px_16px] items-center gap-3 rounded-[11px] border-0 bg-transparent px-3 text-left text-(--cp-text-primary) transition-colors duration-200 hover:bg-(--cp-bg-subtle)"
              type="button"
            >
              <ShieldCheck :size="20" />
              <span class="text-[13px] leading-[1.15] font-[650]">安全与会话</span>
              <span class="text-xs leading-[1.15] font-[650] text-(--cp-text-secondary)">MFA</span>
              <ChevronRight class="text-(--cp-text-muted)" :size="16" />
            </button>

            <button
              class="grid h-9 grid-cols-[20px_minmax(0,1fr)_16px] items-center gap-3 rounded-[11px] border-0 bg-transparent px-3 text-left text-(--cp-text-primary) transition-colors duration-200 hover:bg-(--cp-bg-subtle)"
              type="button"
            >
              <ScrollText :size="20" />
              <span class="text-[13px] leading-[1.15] font-[650]">操作记录</span>
              <ChevronRight class="text-(--cp-text-muted)" :size="16" />
            </button>

            <button
              class="grid h-9 grid-cols-[20px_minmax(0,1fr)] items-center gap-3 rounded-[11px] border-0 bg-transparent px-3 text-left text-(--cp-danger-text) transition-colors duration-200 hover:bg-(--cp-danger-bg)"
              type="button"
              @click="handleLogout"
            >
              <LogOut class="text-(--cp-danger)" :size="20" />
              <span class="text-[13px] leading-[1.15] font-[650]">退出登录</span>
            </button>
          </div>
        </div>
      </Transition>
    </div>
  </div>
</template>
