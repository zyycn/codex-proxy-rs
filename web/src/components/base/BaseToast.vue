<script setup lang="ts">
import { CheckCircle2, AlertCircle, AlertTriangle, Info, X } from '@lucide/vue'

import { useToastStore } from '@/stores/modules/toast'

const toastStore = useToastStore()

const iconMap = {
  success: CheckCircle2,
  error: AlertCircle,
  warning: AlertTriangle,
  info: Info,
}

const titleMap = {
  success: '成功',
  error: '失败',
  warning: '警告',
  info: '信息',
}

const colorClasses = {
  success: {
    bg: 'bg-[#ECFDF5]',
    iconBg: 'bg-[#ECFDF5]',
    icon: 'text-[#12B981]',
  },
  error: {
    bg: 'bg-white',
    iconBg: 'bg-[#FEF2F2]',
    icon: 'text-[#EF4444]',
  },
  warning: {
    bg: 'bg-white',
    iconBg: 'bg-[#FFFBEB]',
    icon: 'text-[#F59E0B]',
  },
  info: {
    bg: 'bg-white',
    iconBg: 'bg-[#EEF6FF]',
    icon: 'text-[#2563EB]',
  },
}
</script>

<template>
  <Teleport to="body">
    <div class="fixed top-6 right-6 z-9999 flex flex-col gap-5 pointer-events-none">
      <TransitionGroup name="toast" tag="div" class="flex flex-col gap-5">
        <div
          v-for="toast in toastStore.toasts"
          :key="toast.id"
          class="pointer-events-auto flex items-start gap-3 w-90 py-4 px-4 rounded-[18px] shadow-[0_16px_34px_-18px_rgba(14,23,38,0.13)] backdrop-blur-sm transition-all"
          :class="colorClasses[toast.type].bg"
        >
          <div
            class="flex items-center justify-center shrink-0 w-8.5 h-8.5 rounded-[10px]"
            :class="colorClasses[toast.type].iconBg"
          >
            <component :is="iconMap[toast.type]" :size="18" :class="colorClasses[toast.type].icon" />
          </div>

          <div class="flex flex-col gap-1.25 flex-1 min-w-0 w-57.5">
            <p class="m-0 text-[13px] leading-[1.15] font-bold text-[#0E1726]">
              {{ titleMap[toast.type] }}
            </p>
            <p class="m-0 text-[12px] leading-[1.15] font-semibold text-[#64748B]">
              {{ toast.message }}
            </p>
          </div>

          <button
            type="button"
            class="flex items-center justify-center shrink-0 p-0 border-0 bg-transparent w-7 h-7 rounded-md cursor-pointer opacity-50 hover:opacity-100 transition-opacity"
            @click="toastStore.remove(toast.id)"
          >
            <X :size="16" class="text-[#94A3B8]" />
          </button>
        </div>
      </TransitionGroup>
    </div>
  </Teleport>
</template>

<style scoped>
.toast-enter-active {
  animation: toast-in 0.3s ease-out;
}

.toast-leave-active {
  animation: toast-out 0.2s ease-in;
}

@keyframes toast-in {
  from {
    opacity: 0;
    transform: translateX(100%) scale(0.95);
  }
  to {
    opacity: 1;
    transform: translateX(0) scale(1);
  }
}

@keyframes toast-out {
  from {
    opacity: 1;
    transform: translateX(0) scale(1);
  }
  to {
    opacity: 0;
    transform: translateX(100%) scale(0.95);
  }
}
</style>
