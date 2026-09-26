<script setup lang="ts">
import type { rotationOptions } from '../constants'
import type { SmartSchedulingConfig } from '@/api'
import { BaseCard, BaseIconButton } from '@codex-proxy/ui'
import { Settings2 } from '@lucide/vue'
import { isEqual } from 'es-toolkit'
import { computed, shallowRef } from 'vue'
import SmartSchedulingModal from './SmartSchedulingModal.vue'

type RotationOption = (typeof rotationOptions)[number]
type RotationStrategy = RotationOption['value']

const props = defineProps<{
  options: readonly RotationOption[]
  smartDefaults?: SmartSchedulingConfig
  disabled: boolean
}>()

const model = defineModel<RotationStrategy | ''>({ required: true })
const smartScheduling = defineModel<SmartSchedulingConfig | undefined>('smartScheduling', { required: true })
const settingsOpen = shallowRef(false)
const customized = computed(() => smartScheduling.value && props.smartDefaults && !isEqual(smartScheduling.value, props.smartDefaults))
</script>

<template>
  <BaseCard title="调度策略">
    <div class="grid max-w-6xl gap-3 sm:grid-cols-2 lg:grid-cols-4">
      <div
        v-for="option in options"
        :key="option.value"
        class="relative flex"
      >
        <button
          type="button"
          class="min-h-25 w-full cursor-pointer rounded-cp border-0 px-4 py-3.5 text-left shadow-cp-input outline-none transition-[background-color,box-shadow,color] duration-160 focus-visible:ring-2 focus-visible:ring-cp-control-outline"
          :class="
            model === option.value
              ? 'bg-cp-control-item-bg-active text-cp-primary-text shadow-cp-tertiary'
              : 'bg-(--cp-input-bg,var(--cp-input-bg)) text-cp-text hover:bg-(--cp-input-hover-bg,var(--cp-input-hover-bg)) hover:shadow-cp-input-hover'
          "
          :aria-pressed="model === option.value"
          :disabled="disabled"
          @click="model = option.value"
        >
          <span class="flex items-center gap-2" :class="option.value === 'smart' ? 'pr-7' : undefined">
            <span
              class="inline-flex size-4 shrink-0 items-center justify-center rounded-full bg-cp-bg-container shadow-[inset_0_0_0_1px_var(--cp-color-border)]"
            >
              <span
                class="size-2 rounded-full transition-opacity duration-150"
                :class="model === option.value ? 'bg-cp-primary opacity-100' : 'opacity-0'"
              />
            </span>
            <span class="inline-flex items-baseline gap-1 text-cp leading-snug font-emphasis">
              {{ option.label }}
              <span v-if="option.value === 'smart' && customized" class="shrink-0 text-cp-xs font-normal text-cp-text-secondary">自定义</span>
            </span>
          </span>
          <span class="mt-2 block text-cp leading-normal font-emphasis text-cp-text-secondary">
            {{ option.description }}
          </span>
        </button>
        <BaseIconButton
          v-if="option.value === 'smart'"
          class="absolute top-3 right-3 size-6! rounded-cp-sm p-0"
          label="智能调度设置"
          size="sm"
          :disabled="disabled || !smartScheduling || !smartDefaults"
          @click="settingsOpen = true"
        >
          <Settings2 class="size-3.5" />
        </BaseIconButton>
      </div>
    </div>
  </BaseCard>
  <SmartSchedulingModal
    v-if="smartScheduling && smartDefaults"
    v-model="settingsOpen"
    :config="smartScheduling"
    :defaults="smartDefaults"
    :active="model === 'smart'"
    @confirm="smartScheduling = $event"
  />
</template>
