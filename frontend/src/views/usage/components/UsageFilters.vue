<script setup lang="ts">
import type { UsageRecordFilterField } from '../composables/useUsageRecordsTable'
import { ZIconButton, ZInput, ZSelect } from '@codex-proxy/ui'

import { RefreshCw, Search } from '@lucide/vue'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

defineProps<{
  refreshing: boolean
  loading: boolean
}>()

const emit = defineEmits<{
  refresh: []
}>()

const { t } = useI18n({ useScope: 'global' })

const field = defineModel<UsageRecordFilterField>('field', { required: true })
const value = defineModel<string>('value', { required: true })
const fieldOptions = computed(() => [
  {
    value: 'key',
    label: t('usage.filters.keyName'),
  },
  {
    value: 'account',
    label: t('usage.filters.account'),
  },
  {
    value: 'model',
    label: t('usage.fields.model'),
  },
])
</script>

<template>
  <div class="flex w-full flex-wrap items-center gap-3" role="group" :aria-label="t('usage.filters.label')">
    <div class="grid w-full min-w-0 grid-cols-[8rem_minmax(0,1fr)] gap-2 sm:w-md sm:flex-none">
      <ZSelect v-model="field" :options="fieldOptions" :aria-label="t('usage.filters.fieldLabel')" />
      <ZInput
        v-model="value"
        :placeholder="t('usage.filters.enterFilterValue')"
        :aria-label="t('usage.filters.valueLabel')"
        class="w-full"
      >
        <template #prefix>
          <Search class="size-4.5 text-cp-text-tertiary" />
        </template>
      </ZInput>
    </div>

    <div class="ml-auto flex shrink-0 items-center justify-end gap-2">
      <slot name="actions" />
      <ZIconButton
        size="default"
        :aria-label="t('usage.filters.refreshUsageHistory')"
        :loading="refreshing"
        :disabled="loading || refreshing"
        @click="emit('refresh')"
      >
        <template #loading>
          <RefreshCw class="size-4.5 animate-spin motion-reduce:animate-none" />
        </template>
        <RefreshCw class="size-4.5" />
      </ZIconButton>
    </div>
  </div>
</template>
