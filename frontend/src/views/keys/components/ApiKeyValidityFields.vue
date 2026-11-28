<script setup lang="ts">
import type { DatePickerValue } from '@codex-proxy/ui'
import type { ApiKeyValidity } from '@/api'
import { ZDatePicker, ZFormItem } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

defineProps<{ disabled: boolean }>()
const validity = defineModel<ApiKeyValidity>({ required: true })
const { t } = useI18n({ useScope: 'global' })

const range = computed(() => {
  const { startsAt, expiresAt } = validity.value
  return startsAt && expiresAt
    ? [
        new Date(startsAt),
        new Date(expiresAt),
      ]
    : null
})

function nextDays(days: number) {
  const start = new Date()
  start.setHours(0, 0, 0, 0)
  const end = new Date(start)
  end.setDate(start.getDate() + days)
  return [start, end]
}

const shortcuts = computed(() => [
  {
    text: t('keys.validity.week'),
    value: () => nextDays(7),
  },
  {
    text: t('keys.validity.month'),
    value: () => nextDays(30),
  },
])

function setRange(value: DatePickerValue) {
  const [start, end] = Array.isArray(value)
    ? value
    : []
  validity.value = {
    startsAt: start instanceof Date
      ? start.toISOString()
      : null,
    expiresAt: end instanceof Date
      ? end.toISOString()
      : null,
  }
}
</script>

<template>
  <ZFormItem prop="validity" :label="t('keys.validity.label')">
    <ZDatePicker
      type="datetimerange" single-panel class="w-full" :disabled="disabled"
      :model-value="range" :shortcuts="shortcuts" :aria-label="t('keys.validity.label')"
      :start-placeholder="t('keys.validity.startDate')" :end-placeholder="t('keys.validity.endDate')"
      @update:model-value="setRange"
    />
  </ZFormItem>
</template>
