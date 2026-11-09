<script setup lang="ts">
import type { UsageListRecord } from '@/api'
import { CheckCheck } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { usageErrorCode, usageErrorOrigin, usageErrorTitle } from './shared/errors'

const props = defineProps<{ record: UsageListRecord }>()
const { t } = useI18n({ useScope: 'global' })
const message = computed(() => props.record.errorMessage?.replace(/^OpenAI upstream failure: /, ''))
const code = computed(() => usageErrorCode(props.record))
const origin = computed(() => usageErrorOrigin(props.record))
const description = computed(() => [code.value, message.value || t('usage.error.noDetails')].filter(Boolean).join(' · '))
const outcome = computed(() => usageErrorTitle(props.record))
</script>

<template>
  <div class="min-w-0 py-0.5">
    <div class="flex min-w-0 items-center gap-2">
      <span class="shrink-0 rounded-z-sm px-2 py-0.5 text-z-xs font-bold" :class="origin.className">
        {{ origin.label }}
      </span>
      <span class="truncate text-z-sm font-bold text-z-error-text" :title="outcome">{{ outcome }}</span>
      <span v-if="record.upstreamStatusCode != null" class="shrink-0 font-mono text-z-xs text-z-text-secondary" :title="t('usage.error.upstreamStatusCode')">
        {{ t('usage.error.upstreamHttpStatus', { status: record.upstreamStatusCode }) }}
      </span>
      <span v-if="record.clientStatusCode != null && record.clientStatusCode !== record.upstreamStatusCode" class="shrink-0 font-mono text-z-xs text-z-text-secondary" :title="t('usage.error.clientStatusCode')">
        {{ t('usage.error.clientHttpStatus', { status: record.clientStatusCode }) }}
      </span>
      <span v-if="record.recovered" class="inline-flex shrink-0 text-z-success-text" role="img" :aria-label="t('usage.error.recovered')" :title="t('usage.error.recovered')">
        <CheckCheck class="size-4" :stroke-width="2.25" aria-hidden="true" />
      </span>
    </div>
    <p class="mt-1 mb-0 truncate text-z-sm leading-[1.45] font-emphasis text-z-text-secondary" :title="description" translate="no">
      {{ description }}
    </p>
  </div>
</template>
