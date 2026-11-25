<script setup lang="ts">
import type { TokenPrices } from '@/api'

import { ZFormItem, ZInput } from '@codex-proxy/ui'
import { useI18n } from 'vue-i18n'
import { effectivePrice, priceFields, validPrice } from './model'

const props = defineProps<{
  base?: TokenPrices
  custom?: TokenPrices
  multiplierBps?: number
  disabled: boolean
}>()

defineEmits<{ change: [field: keyof TokenPrices, value: string] }>()

const { t } = useI18n({ useScope: 'global' })

function preview(field: keyof TokenPrices) {
  const price = (props.custom ?? props.base)?.[field]
  return props.multiplierBps === undefined || price === undefined || !validPrice(price)
    ? '—'
    : effectivePrice(price, props.multiplierBps)
}
</script>

<template>
  <div class="grid grid-cols-2 gap-x-4 gap-y-5 sm:gap-x-6">
    <div v-for="field in priceFields" :key="field.key" class="min-w-0">
      <ZFormItem v-if="custom" :label="field.label">
        <ZInput
          :model-value="custom[field.key]"
          :aria-label="t('settings.pricing.unitPrice', { label: field.label })"
          class="font-mono"
          inputmode="decimal"
          placeholder="0.00"
          :disabled="disabled"
          @update:model-value="$emit('change', field.key, $event)"
        />
      </ZFormItem>
      <div v-else>
        <div class="mb-2 text-z leading-none font-medium text-z-text-secondary">
          {{ field.label }}
        </div>
        <div class="flex h-z-control items-center font-mono text-lg tabular-nums text-z-text">
          {{ effectivePrice(base?.[field.key]) }}
        </div>
      </div>
      <div v-if="(custom && base) || multiplierBps !== 10000" class="mt-2 grid gap-1 text-z-xs">
        <span v-if="custom && base" class="text-z-text-tertiary">
          {{ t('settings.pricing.source') }} <span class="break-all font-mono tabular-nums">{{ effectivePrice(base[field.key]) }}</span>
        </span>
        <span v-if="multiplierBps !== 10000" class="text-z-text-secondary">
          {{ t('settings.pricing.takeEffect') }} <span class="break-all font-mono tabular-nums text-z-primary-text">{{ preview(field.key) }}</span>
        </span>
      </div>
    </div>
  </div>
</template>
