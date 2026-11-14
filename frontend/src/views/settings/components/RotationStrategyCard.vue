<script setup lang="ts">
import type { rotationOptions } from '../constants'

import type { ResetWindowWeights, SmartSchedulingConfig } from '@/api'
import { ZCard, ZIconButton } from '@codex-proxy/ui'
import { Settings2 } from '@lucide/vue'
import { isEqual } from 'es-toolkit'
import { computed, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import QuotaResetPriorityModal from './QuotaResetPriorityModal.vue'
import SmartSchedulingModal from './SmartSchedulingModal.vue'

const props = defineProps<{
  options: readonly RotationOption[]
  smartDefaults?: SmartSchedulingConfig
  quotaResetDefaults?: ResetWindowWeights
  disabled: boolean
}>()

const { t } = useI18n({ useScope: 'global' })

type RotationOption = (typeof rotationOptions.value)[number]
type RotationStrategy = RotationOption['value']

const model = defineModel<RotationStrategy | ''>({ required: true })
const smartScheduling = defineModel<SmartSchedulingConfig | undefined>('smartScheduling', { required: true })
const quotaResetWindowWeights = defineModel<ResetWindowWeights | undefined>('quotaResetWindowWeights', { required: true })
const settingsOpen = shallowRef<RotationStrategy>()
const strategySettings = computed<Partial<Record<RotationStrategy, { label: string, ready: boolean, customized: boolean }>>>(() => ({
  smart: {
    label: t('settings.rotation.smartSchedulingSettings'),
    ready: Boolean(smartScheduling.value && props.smartDefaults),
    customized: Boolean(smartScheduling.value && props.smartDefaults && !isEqual(smartScheduling.value, props.smartDefaults)),
  },
  quota_reset_priority: {
    label: t('settings.rotation.quotaResetPrioritySettings'),
    ready: Boolean(quotaResetWindowWeights.value && props.quotaResetDefaults),
    customized: Boolean(quotaResetWindowWeights.value && props.quotaResetDefaults && !isEqual(quotaResetWindowWeights.value, props.quotaResetDefaults)),
  },
}))
</script>

<template>
  <ZCard :title="t('settings.rotation.title')" class="@container">
    <div class="grid max-w-6xl gap-3 @md:grid-cols-2 @5xl:grid-cols-4">
      <div
        v-for="option in options"
        :key="option.value"
        class="relative flex"
      >
        <button
          type="button"
          class="flex min-h-25 w-full cursor-pointer flex-col rounded-z border-0 px-4 py-3.5 text-left shadow-z-input outline-none transition-[background-color,box-shadow,color] duration-160 focus-visible:ring-2 focus-visible:ring-z-control-outline"
          :class="
            model === option.value
              ? 'bg-z-control-item-bg-active text-z-primary-text shadow-z-tertiary'
              : 'bg-(--z-input-bg,var(--z-input-bg)) text-z-text hover:bg-(--z-input-hover-bg,var(--z-input-hover-bg)) hover:shadow-z-input-hover'
          "
          :aria-pressed="model === option.value"
          :disabled="disabled"
          @click="model = option.value"
        >
          <span class="flex items-center gap-2" :class="strategySettings[option.value] ? 'pr-7' : undefined">
            <span
              class="inline-flex size-4 shrink-0 items-center justify-center rounded-full bg-z-bg-container shadow-[inset_0_0_0_1px_var(--z-color-border)]"
            >
              <span
                class="size-2 rounded-full transition-opacity duration-150"
                :class="model === option.value ? 'bg-z-primary opacity-100' : 'opacity-0'"
              />
            </span>
            <span class="inline-flex items-baseline gap-1 text-z leading-snug font-emphasis">
              {{ option.label }}
              <span v-if="strategySettings[option.value]?.customized" class="shrink-0 text-z-xs font-normal text-z-text-secondary">{{ t('settings.rotation.customized') }}</span>
            </span>
          </span>
          <span class="mt-2 block text-z leading-normal font-emphasis text-z-text-secondary">
            {{ option.description }}
          </span>
        </button>
        <ZIconButton
          v-if="strategySettings[option.value]"
          class="absolute top-3 right-3 size-6! rounded-z-sm p-0"
          :aria-label="strategySettings[option.value]?.label"
          size="small"
          :disabled="disabled || !strategySettings[option.value]?.ready"
          @click="settingsOpen = option.value"
        >
          <Settings2 class="size-3.5" />
        </ZIconButton>
      </div>
    </div>
  </ZCard>
  <SmartSchedulingModal
    v-if="smartScheduling && smartDefaults"
    :model-value="settingsOpen === 'smart'"
    :config="smartScheduling"
    :defaults="smartDefaults"
    @update:model-value="settingsOpen = $event ? 'smart' : undefined"
    @confirm="smartScheduling = $event"
  />
  <QuotaResetPriorityModal
    v-if="quotaResetWindowWeights && quotaResetDefaults"
    :model-value="settingsOpen === 'quota_reset_priority'"
    :config="quotaResetWindowWeights"
    :defaults="quotaResetDefaults"
    @update:model-value="settingsOpen = $event ? 'quota_reset_priority' : undefined"
    @confirm="quotaResetWindowWeights = $event"
  />
</template>
