<script setup lang="ts">
import { AlertCircle, AlertTriangle, CheckCircle2, Info, X } from '@lucide/vue'

import BaseIconButton from '../BaseIconButton.vue'
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
    iconBg: 'bg-cp-success-bg',
    icon: 'text-cp-success',
  },
  error: {
    iconBg: 'bg-cp-error-bg',
    icon: 'text-cp-error',
  },
  warning: {
    iconBg: 'bg-cp-warning-bg',
    icon: 'text-cp-warning',
  },
  info: {
    iconBg: 'bg-cp-info-bg',
    icon: 'text-cp-info',
  },
}
</script>

<template>
  <Teleport to="body">
    <div class="pointer-events-none fixed inset-x-3 top-3 z-9999 flex flex-col items-end gap-3 sm:inset-x-auto sm:top-6 sm:right-6">
      <TransitionGroup name="toast" tag="div" class="flex flex-col gap-3">
        <article
          v-for="message in toast.messages"
          :key="message.id"
          class="pointer-events-auto flex min-h-16 w-90 max-w-[calc(100vw-1.5rem)] items-center gap-3 rounded-cp-card bg-cp-bg-container px-3.5 py-3 shadow-cp"
          :role="message.type === 'error' ? 'alert' : 'status'"
          aria-atomic="true"
        >
          <div
            class="flex size-8.5 shrink-0 items-center justify-center rounded-cp"
            :class="colorClasses[message.type].iconBg"
          >
            <component
              :is="iconMap[message.type]"
              :size="18"
              :class="colorClasses[message.type].icon"
            />
          </div>

          <div class="flex min-w-0 flex-1 flex-col gap-1">
            <p class="m-0 truncate text-cp leading-[1.15] font-bold text-cp-text">
              {{ message.title ?? titleMap[message.type] }}
            </p>
            <p
              class="m-0 max-h-8 overflow-hidden text-xs leading-tight font-semibold text-cp-text-secondary"
            >
              {{ message.message }}
            </p>
          </div>

          <BaseIconButton
            size="sm"
            variant="ghost"
            :label="`关闭${message.title ?? titleMap[message.type]}通知`"
            @click="toast.remove(message.id)"
          >
            <X :size="16" class="text-cp-text-quaternary" />
          </BaseIconButton>
        </article>
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

@media (prefers-reduced-motion: reduce) {
  .toast-enter-active,
  .toast-leave-active {
    animation: none;
  }
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
