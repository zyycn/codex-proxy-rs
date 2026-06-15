<script setup lang="ts">
import { gsap } from 'gsap'
import { ref } from 'vue'
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

const accountMenuOpen = ref(false)

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
      class="group inline-flex h-11 w-[170px] items-center gap-2.5 whitespace-nowrap rounded-xl border-0 bg-white px-3.5 text-[14px] leading-[1.15] font-[650] text-[var(--cp-text-primary)] shadow-[var(--cp-shadow-control)] transition-[box-shadow,transform] duration-200 hover:-translate-y-px active:translate-y-0"
      type="button"
    >
      <CalendarDays class="text-[var(--cp-info)]" :size="18" />
      <span class="whitespace-nowrap">最近 24 小时</span>
      <ChevronDown class="text-[var(--cp-text-secondary)] transition-transform duration-200 group-hover:rotate-180" :size="16" />
    </button>

    <button
      class="inline-flex size-11 items-center justify-center rounded-xl border-0 bg-white text-[var(--cp-normal)] shadow-[var(--cp-shadow-control)] transition-[box-shadow,transform] duration-200 hover:-translate-y-px active:translate-y-0"
      type="button"
      aria-label="刷新"
    >
      <RefreshCw :size="19" />
    </button>

    <div class="relative ml-0.5">
      <button
        class="group grid h-11 w-[216px] grid-cols-[28px_minmax(0,1fr)_16px] items-center rounded-xl border-0 bg-white px-3 text-[var(--cp-text-secondary)] shadow-[var(--cp-shadow-control)] transition-[box-shadow,transform] duration-200 hover:-translate-y-px active:translate-y-0"
        type="button"
        data-account-trigger
        @click="accountMenuOpen = !accountMenuOpen"
      >
        <span class="inline-flex size-7 items-center justify-center rounded-full bg-[var(--cp-normal)] text-[12px] leading-[1.15] font-bold text-white">A</span>
        <span class="grid gap-[5px] pl-3 text-left">
          <strong class="text-[13px] leading-[1.15] font-bold text-[var(--cp-text-primary)]">admin</strong>
          <span class="text-[11px] leading-[1.15] font-[650] text-[var(--cp-text-secondary)]">管理员</span>
        </span>
        <ChevronUp v-if="accountMenuOpen" class="text-[var(--cp-text-secondary)] transition-transform duration-200 group-hover:rotate-180" :size="16" />
        <ChevronDown v-else class="text-[var(--cp-text-secondary)] transition-transform duration-200 group-hover:rotate-180" :size="16" />
      </button>

      <Transition
        :css="false"
        @enter="animateAccountMenuEnter"
        @leave="animateAccountMenuLeave"
      >
        <div v-if="accountMenuOpen" data-account-menu class="absolute right-0 top-14 h-[304px] w-80 origin-top-right rounded-[18px] bg-white pt-[18px] shadow-[var(--cp-shadow-popover)]">
          <div class="mx-[18px] grid h-11 grid-cols-[44px_minmax(0,1fr)_64px] items-center gap-3">
          <span class="inline-flex size-11 items-center justify-center rounded-full bg-[var(--cp-normal)] text-[14px] leading-[1.15] font-bold text-white">A</span>
          <span class="grid gap-[5px]">
            <strong class="text-[15px] leading-[1.15] font-bold text-[var(--cp-text-primary)]">admin</strong>
            <span class="text-[12px] leading-[1.15] font-[650] text-[var(--cp-text-secondary)]">管理员</span>
          </span>
          <span class="inline-flex h-7 w-16 items-center gap-1.5 rounded-full bg-[var(--cp-success-bg)] px-3">
            <i class="size-1.5 rounded-full bg-[var(--cp-success)]" />
            <span class="text-[12px] leading-[1.15] font-[650] text-[var(--cp-success-text)]">在线</span>
          </span>
          </div>

          <div class="mx-4 mt-3.5 grid h-11 w-[288px] grid-cols-2 rounded-xl bg-[var(--cp-bg-subtle)] px-4 py-2">
          <span class="grid gap-[5px]">
            <small class="text-[11px] leading-[1.15] font-semibold text-[var(--cp-text-secondary)]">MFA</small>
            <strong class="text-[13px] leading-[1.15] font-bold text-[var(--cp-text-primary)]">已开启</strong>
          </span>
          <span class="grid gap-[5px] pl-[18px]">
            <small class="text-[11px] leading-[1.15] font-semibold text-[var(--cp-text-secondary)]">会话</small>
            <strong class="text-[13px] leading-[1.15] font-bold text-[var(--cp-text-primary)]">1 台</strong>
          </span>
          </div>

          <div class="mx-4 mt-3.5 grid w-[288px] gap-1.5">
          <button
            class="grid h-9 grid-cols-[20px_minmax(0,1fr)_16px] items-center gap-3 rounded-[11px] border-0 bg-[var(--cp-bg-subtle)] px-3 text-left text-[var(--cp-text-primary)] transition-[background-color,transform] duration-200 hover:-translate-y-px active:translate-y-0"
            type="button"
          >
            <User :size="20" />
            <span class="text-[13px] leading-[1.15] font-[650]">个人设置</span>
            <ChevronRight class="text-[var(--cp-text-muted)]" :size="16" />
          </button>

          <button
            class="grid h-9 grid-cols-[20px_minmax(0,1fr)_34px_16px] items-center gap-3 rounded-[11px] border-0 bg-transparent px-3 text-left text-[var(--cp-text-primary)] transition-[background-color,transform] duration-200 hover:-translate-y-px hover:bg-[var(--cp-bg-subtle)] active:translate-y-0"
            type="button"
          >
            <ShieldCheck :size="20" />
            <span class="text-[13px] leading-[1.15] font-[650]">安全与会话</span>
            <span class="text-[12px] leading-[1.15] font-[650] text-[var(--cp-text-secondary)]">MFA</span>
            <ChevronRight class="text-[var(--cp-text-muted)]" :size="16" />
          </button>

          <button
            class="grid h-9 grid-cols-[20px_minmax(0,1fr)_16px] items-center gap-3 rounded-[11px] border-0 bg-transparent px-3 text-left text-[var(--cp-text-primary)] transition-[background-color,transform] duration-200 hover:-translate-y-px hover:bg-[var(--cp-bg-subtle)] active:translate-y-0"
            type="button"
          >
            <ScrollText :size="20" />
            <span class="text-[13px] leading-[1.15] font-[650]">操作记录</span>
            <ChevronRight class="text-[var(--cp-text-muted)]" :size="16" />
          </button>

          <button
            class="grid h-9 grid-cols-[20px_minmax(0,1fr)] items-center gap-3 rounded-[11px] border-0 bg-transparent px-3 text-left text-[var(--cp-danger-text)] transition-[background-color,transform] duration-200 hover:-translate-y-px hover:bg-[var(--cp-danger-bg)] active:translate-y-0"
            type="button"
          >
            <LogOut class="text-[var(--cp-danger)]" :size="20" />
            <span class="text-[13px] leading-[1.15] font-[650]">退出登录</span>
          </button>
          </div>
        </div>
      </Transition>
    </div>
  </div>
</template>
