<script setup lang="ts">
import type { TableColumn } from '@codex-proxy/ui'

import type { PricingRow } from './model'

import { ZCheckbox, ZIconButton, ZTable } from '@codex-proxy/ui'
import { Pencil, Trash2 } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { effectivePrice, multiplierText, sourceLabels } from './model'
import PricingUnit from './PricingUnit.vue'

defineProps<{ rows: PricingRow[], selected: string[], loading: boolean, disabled: boolean }>()

defineEmits<{ toggle: [model: string, checked: boolean], togglePage: [checked: boolean], edit: [row: PricingRow], delete: [row: PricingRow] }>()

const { t } = useI18n({ useScope: 'global' })

const columns = computed<TableColumn<PricingRow>[]>(() => ([
  { key: 'select', kind: 'selection' },
  {
    key: 'model',
    label: t('settings.pricing.model'),
    kind: 'identity',
  },
  {
    key: 'input',
    label: t('settings.pricing.input'),
    kind: 'numeric',
  },
  {
    key: 'output',
    label: t('settings.pricing.output'),
    kind: 'numeric',
  },
  {
    key: 'cacheRead',
    label: t('settings.pricing.cacheRead'),
    kind: 'numeric',
  },
  {
    key: 'multiplier',
    label: t('settings.pricing.multiplier'),
    kind: 'numeric',
    align: 'center',
  },
  {
    key: 'actions',
    label: t('common.fields.actions'),
    kind: 'actions',
  },
]))

function summaryBand(row: PricingRow) {
  return row.effective.bands.image ? 'image' : 'standard'
}
</script>

<template>
  <ZTable class="[--cp-table-row-height:50px]" :columns="columns" :data="rows" row-key="model" :loading="loading" :selected-row-keys="selected" :empty-text="t('settings.pricing.noMatchingModel')" show-header-when-empty>
    <template v-if="$slots.empty" #empty>
      <slot name="empty" />
    </template>
    <template #header-select>
      <ZCheckbox :aria-label="t('settings.pricing.selectModelOnThisPage')" :disabled="disabled || !rows.length" :model-value="!!rows.length && rows.every(row => selected.includes(row.model))" :indeterminate="rows.some(row => selected.includes(row.model)) && !rows.every(row => selected.includes(row.model))" @update:model-value="$emit('togglePage', $event)" />
    </template>
    <template #header-model>
      <div class="flex items-center gap-3">
        <span>{{ t('settings.pricing.model') }}</span>
        <PricingUnit />
      </div>
    </template>
    <template #select="{ row }">
      <ZCheckbox :aria-label="t('settings.pricing.select', { model: row.model })" :disabled="disabled" :model-value="selected.includes(row.model)" @update:model-value="$emit('toggle', row.model, $event)" />
    </template>
    <template #model="{ row }">
      <div class="min-w-0 py-1.5">
        <div class="flex min-w-0 flex-wrap items-baseline gap-x-3 gap-y-1">
          <span class="truncate font-mono font-semibold" :title="row.model">{{ row.model }}</span>
          <span class="shrink-0 text-cp-xs" :class="row.source === 'custom' ? 'text-cp-primary-text' : 'text-cp-text-tertiary'">
            {{ sourceLabels[row.source] }}<span v-if="row.effective.bands.image"> {{ t('settings.pricing.image') }}</span>
          </span>
        </div>
        <div class="mt-1 flex flex-wrap gap-x-3 gap-y-1 text-cp-xs text-cp-text-secondary sm:hidden">
          <span>{{ t('settings.pricing.input') }} <span class="font-mono">{{ effectivePrice(row.effective.bands[summaryBand(row)]?.input, row.effective.multiplierBps) }}</span></span>
          <span>{{ t('settings.pricing.output') }} <span class="font-mono">{{ effectivePrice(row.effective.bands[summaryBand(row)]?.output, row.effective.multiplierBps) }}</span></span>
        </div>
      </div>
    </template>
    <template v-for="field in (['input', 'output', 'cacheRead'] as const)" :key="field" #[field]="{ row }">
      <div class="py-1 font-mono tabular-nums">
        <div v-if="effectivePrice(row.base.bands[summaryBand(row)]?.[field]) !== effectivePrice(row.effective.bands[summaryBand(row)]?.[field], row.effective.multiplierBps)" class="text-cp-xs text-cp-text-tertiary line-through">
          {{ effectivePrice(row.base.bands[summaryBand(row)]?.[field]) }}
        </div>
        <div :class="row.source === 'custom' ? 'text-cp-primary-text' : 'text-cp-text'">
          {{ effectivePrice(row.effective.bands[summaryBand(row)]?.[field], row.effective.multiplierBps) }}
        </div>
      </div>
    </template>
    <template #multiplier="{ row }">
      <span class="font-mono tabular-nums">{{ multiplierText(row.effective.multiplierBps) }}</span>
    </template>
    <template #actions="{ row }">
      <div class="flex items-center gap-1">
        <ZIconButton size="small" :aria-label="t('settings.pricing.editPrice')" :disabled="disabled" @click="$emit('edit', row)">
          <Pencil class="size-3.5 text-cp-link" aria-hidden="true" />
        </ZIconButton>
        <ZIconButton v-if="row.canDelete" size="small" :aria-label="t('settings.pricing.deletePriceList')" :disabled="disabled" @click="$emit('delete', row)">
          <Trash2 class="size-3.5 text-cp-error" aria-hidden="true" />
        </ZIconButton>
      </div>
    </template>
  </ZTable>
</template>
