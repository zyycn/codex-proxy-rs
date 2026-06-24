<script setup lang="ts">
import { CheckCircle2, AlertCircle, AlertTriangle, Info, X } from '@lucide/vue'

import { toast } from './toast'

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
    iconBg: 'bg-(--cp-success-bg)',
    icon: 'text-(--cp-success)',
  },
  error: {
    iconBg: 'bg-(--cp-danger-bg)',
    icon: 'text-(--cp-danger)',
  },
  warning: {
    iconBg: 'bg-(--cp-warning-bg)',
    icon: 'text-(--cp-warning)',
  },
  info: {
    iconBg: 'bg-(--cp-info-bg)',
    icon: 'text-(--cp-info)',
  },
}
</script>

<template>
  <Teleport to="body">
    <div class="fixed top-6 right-6 z-9999 flex flex-col gap-3 pointer-events-none">
      <TransitionGroup name="toast" tag="div" class="flex flex-col gap-3">
        <div
          v-for="message in toast.messages"
          :key="message.id"
          class="pointer-events-auto flex min-h-16 w-90 items-center gap-3 rounded-[18px] bg-(--cp-bg-surface) px-3.5 py-3 shadow-(--cp-shadow-popover) transition-all"
        >
          <div
            class="flex items-center justify-center shrink-0 w-8.5 h-8.5 rounded-[10px]"
            :class="colorClasses[message.type].iconBg"
          >
            <component
              :is="iconMap[message.type]"
              :size="18"
              :class="colorClasses[message.type].icon"
            />
          </div>

          <div class="flex min-w-0 flex-1 flex-col gap-1">
            <p class="m-0 truncate text-[13px] leading-[1.15] font-bold text-(--cp-text-primary)">
              {{ message.title ?? titleMap[message.type] }}
            </p>
            <p
              class="m-0 max-h-8 overflow-hidden text-xs leading-tight font-semibold text-(--cp-text-secondary)"
            >
              {{ message.message }}
            </p>
          </div>

          <button
            type="button"
            class="flex items-center justify-center shrink-0 p-0 border-0 bg-transparent w-7 h-7 rounded-(--cp-button-radius-base) cursor-pointer opacity-60 transition-opacity hover:bg-(--cp-bg-subtle) hover:opacity-100"
            :aria-label="`关闭${message.title ?? titleMap[message.type]}通知`"
            @click="toast.remove(message.id)"
          >
            <X :size="16" class="text-(--cp-text-muted)" />
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
