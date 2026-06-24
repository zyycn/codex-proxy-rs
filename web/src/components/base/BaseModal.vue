<script setup lang="ts">
import { AlertCircle, AlertTriangle, CheckCircle2, Info, X } from '@lucide/vue'
import { computed } from 'vue'

import BaseButton from './BaseButton.vue'
import BaseIconButton from './BaseIconButton.vue'

type ModalVariant = 'default' | 'info' | 'warning' | 'danger' | 'success'

const props = defineProps<{
  title: string
  description?: string
  width?: string | number
  variant?: ModalVariant
  closeDisabled?: boolean
}>()

const open = defineModel<boolean>({ default: false })

const variant = computed(() => props.variant ?? 'default')

const modalStyle = computed(() => ({
  width: typeof props.width === 'number' ? `${props.width}px` : (props.width ?? undefined),
  maxWidth: '100%',
}))

const iconMap = {
  default: Info,
  info: Info,
  warning: AlertTriangle,
  danger: AlertCircle,
  success: CheckCircle2,
}

const variantClasses: Record<ModalVariant, { iconBg: string; icon: string }> = {
  default: {
    iconBg: 'bg-(--cp-info-bg)',
    icon: 'text-(--cp-info)',
  },
  info: {
    iconBg: 'bg-(--cp-info-bg)',
    icon: 'text-(--cp-info)',
  },
  warning: {
    iconBg: 'bg-(--cp-warning-bg)',
    icon: 'text-(--cp-warning)',
  },
  danger: {
    iconBg: 'bg-(--cp-danger-bg)',
    icon: 'text-(--cp-danger)',
  },
  success: {
    iconBg: 'bg-(--cp-success-bg)',
    icon: 'text-(--cp-success)',
  },
}

function closeModal() {
  if (props.closeDisabled) return
  open.value = false
}
</script>

<template>
  <Teleport to="body">
    <div v-if="open" class="fixed inset-0 z-50 grid place-items-center p-6" role="presentation">
      <div class="absolute inset-0 bg-(--cp-overlay-scrim)" @click="closeModal" />
      <section
        class="[--cp-input-current-bg:var(--cp-input-soft-bg)] [--cp-input-current-bg-hover:var(--cp-input-soft-bg-hover)] relative w-[min(560px,100%)] rounded-[22px] bg-(--cp-bg-surface) shadow-(--cp-shadow-popover)"
        :style="modalStyle"
        role="dialog"
        aria-modal="true"
        :aria-label="title"
      >
        <header class="grid grid-cols-[auto_minmax(0,1fr)_28px] gap-4 p-7 pb-0">
          <span
            v-if="variant !== 'default'"
            class="inline-flex size-11 items-center justify-center rounded-[14px]"
            :class="variantClasses[variant].iconBg"
          >
            <component :is="iconMap[variant]" :size="18" :class="variantClasses[variant].icon" />
          </span>
          <div class="min-w-0" :class="variant === 'default' ? 'col-span-2' : ''">
            <h2 class="m-0 text-lg leading-[1.15] font-[760] text-(--cp-text-primary)">
              {{ title }}
            </h2>
            <p
              v-if="description"
              class="mt-2 mb-0 text-[13px] font-semibold leading-[1.45] text-(--cp-text-secondary)"
            >
              {{ description }}
            </p>
          </div>
          <BaseIconButton
            class="col-start-3"
            label="关闭"
            size="sm"
            variant="ghost"
            :disabled="closeDisabled"
            @click="closeModal"
          >
            <X :size="16" />
          </BaseIconButton>
        </header>
        <div class="p-7" :class="description || variant !== 'default' ? 'pt-5' : 'pt-6'">
          <slot />
        </div>
        <footer class="flex justify-end gap-3 px-7 pb-7">
          <slot name="footer">
            <BaseButton variant="default" @click="open = false">取消</BaseButton>
            <BaseButton variant="primary" @click="open = false">确认</BaseButton>
          </slot>
        </footer>
      </section>
    </div>
  </Teleport>
</template>
