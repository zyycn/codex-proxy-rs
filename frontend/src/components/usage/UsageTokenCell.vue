<script setup lang="ts">
import type { UsageListRecord } from '@/api'

import { Archive, ArrowDown, ArrowUp } from '@lucide/vue'

import { computed } from 'vue'

import { useI18n } from 'vue-i18n'
import { usageTokenDetails } from '@/components/usage/shared/presenter'
import UsageDetailPopover from '@/components/usage/UsageDetailPopover.vue'

const props = defineProps<{
  record: Pick<UsageListRecord, 'tokenDetails'>
}>()

const { t } = useI18n({ useScope: 'global' })

const tokenDetails = computed(() => usageTokenDetails(props.record.tokenDetails))
const tokenItems = computed(() => [
  {
    label: t('usage.tokens.enterToken'),
    value: tokenDetails.value.inputTokensDisplay,
  },
  {
    label: t('usage.tokens.outputToken'),
    value: tokenDetails.value.outputTokensDisplay,
  },
  {
    label: t('usage.tokens.cacheReadTokens'),
    value: tokenDetails.value.cachedInputTokensDisplay,
  },
  {
    label: t('usage.tokens.cacheWriteTokens'),
    value: tokenDetails.value.cacheWriteInputTokensDisplay,
  },
  {
    label: t('usage.tokens.reasoningToken'),
    value: tokenDetails.value.reasoningOutputTokensDisplay,
  },
])
</script>

<template>
  <div class="flex items-center justify-end gap-1.5">
    <div
      class="grid grid-cols-[auto_auto] items-center justify-end gap-x-2 gap-y-1 font-mono text-cp-sm leading-none font-bold tabular-nums"
    >
      <span class="inline-flex items-center gap-1 text-cp-success-text">
        <ArrowDown class="size-3" />
        {{ tokenDetails.inputTokensDisplay }}
      </span>
      <span class="inline-flex items-center gap-1 text-cp-info-text">
        <ArrowUp class="size-3" />
        {{ tokenDetails.outputTokensDisplay }}
      </span>
      <span class="col-span-2 inline-flex items-center justify-end gap-1 text-cp-info-text">
        <Archive class="size-3" />
        {{ tokenDetails.cachedInputTokensDisplay }}
      </span>
    </div>

    <UsageDetailPopover :title="t('usage.tokens.tokenDetails')" :trigger-label="t('usage.tokens.viewTokenDetails')">
      <div class="grid gap-1.5 text-cp-text-secondary">
        <div v-for="item in tokenItems" :key="item.label" class="flex justify-between gap-4">
          <span class="whitespace-nowrap">{{ item.label }}</span>
          <span class="whitespace-nowrap font-mono font-heavy text-cp-text">
            {{ item.value }}
          </span>
        </div>
      </div>
      <div class="mt-1 flex justify-between border-t border-cp-split pt-2">
        <span class="whitespace-nowrap text-cp-text-secondary">{{ t('usage.tokens.totalTokens') }}</span>
        <span class="whitespace-nowrap font-mono font-heavy text-cp-info-text">
          {{ tokenDetails.totalTokensDisplay }}
        </span>
      </div>
    </UsageDetailPopover>
  </div>
</template>
