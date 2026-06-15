<script setup lang="ts">
import {
  Box,
  ChartColumn,
  KeyRound,
  LayoutDashboard,
  PanelLeftClose,
  Radar,
  ScrollText,
  Settings,
  SquareTerminal,
  Users,
} from '@lucide/vue'

const navItems = [
  { label: '概览', icon: LayoutDashboard, active: true, topClass: 'top-[112px]' },
  { label: '账号池', icon: Users, active: false, topClass: 'top-[170px]' },
  { label: 'API Keys', icon: KeyRound, active: false, topClass: 'top-[228px]' },
  { label: '日志', icon: ScrollText, active: false, topClass: 'top-[286px]' },
  { label: '用量', icon: ChartColumn, active: false, topClass: 'top-[344px]' },
  { label: '模型', icon: Box, active: false, topClass: 'top-[402px]' },
  { label: '设置', icon: Settings, active: false, topClass: 'top-[460px]' },
  { label: '诊断', icon: Radar, active: false, topClass: 'top-[518px]' },
]
</script>

<template>
  <aside
    class="sticky top-0 z-20 hidden h-screen min-h-screen w-[280px] shrink-0 basis-[280px] bg-white shadow-[var(--cp-shadow-sidebar)] min-[961px]:block"
  >
    <div class="absolute left-[28px] top-[31px] inline-flex size-[46px] items-center justify-center rounded-[13px] bg-[#111827] text-white shadow-[0_8px_18px_-16px_#0E172614]">
      <SquareTerminal :size="24" />
    </div>

    <div class="absolute left-[84px] top-[34px] text-[17px] font-[720] leading-[1.1] text-[#111827]">
      Codex
    </div>
    <div class="absolute left-[84px] top-[57px] flex h-4 w-[132px] items-center text-xs font-semibold leading-[1.1] text-[#64748B]">
      Proxy RS · v0.1.0
    </div>

    <nav aria-label="主导航">
      <a
        v-for="item in navItems"
        :key="item.label"
        class="absolute left-6 h-[46px] w-[232px] rounded-xl no-underline transition-colors"
        :class="[
          item.topClass,
          item.active
            ? 'bg-[#E9EEF5] text-[#111827]'
            : 'bg-transparent text-[#64748B] hover:bg-[#F8FAFC]',
        ]"
        href="#"
      >
        <component :is="item.icon" class="absolute left-[22px] top-[13px]" :size="20" />
        <span
          class="absolute left-[54px] top-[15px] text-sm leading-[1.15]"
          :class="item.active ? 'font-bold' : 'font-semibold'"
        >
          {{ item.label }}
        </span>
      </a>
    </nav>

    <button
      class="absolute bottom-8 left-[220px] inline-flex size-9 items-center justify-center rounded-xl border-0 bg-[#F8FAFC] text-[#64748B]"
      type="button"
      aria-label="收缩侧边栏"
    >
      <PanelLeftClose :size="20" />
    </button>
  </aside>
</template>
