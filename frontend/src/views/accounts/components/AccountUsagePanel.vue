<script setup lang="ts">
import type { Account } from '@/api'

import { defineTableColumns, ZTable } from '@codex-proxy/ui'

import { Sigma } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import LastUsedAtCell from '@/components/LastUsedAtCell.vue'
import { modelSuccessRateTextClass, usageWindowLabels } from '../constants'

const props = defineProps<{
  account: Account
}>()

const { t } = useI18n({ useScope: 'global' })

type AccountModelUsage = Account['usage']['models'][number]

const totalBilling = computed(() => props.account.usage.costs.find(cost => cost.currency.toUpperCase() === 'USD'))
const totalBillingDisplay = computed(() => totalBilling.value?.estimatedAmountDisplay ?? '—')
const hasUsageSummary = computed(() => (props.account.usage.totalTokens ?? 0) > 0 || totalBilling.value !== undefined)

const modelUsageColumns = computed(() => (defineTableColumns<AccountModelUsage>([
  {
    key: 'model',
    label: t('accounts.concurrency.model'),
    kind: 'text',
    size: 'md',
  },
  {
    key: 'requestCountDisplay',
    label: t('accounts.usage.call'),
    kind: 'numeric',
    size: 'xs',
  },
  {
    key: 'successRateDisplay',
    label: t('accounts.usage.successRate'),
    kind: 'numeric',
    size: 'sm',
  },
  {
    key: 'inputTokensDisplay',
    label: t('accounts.usage.input'),
    kind: 'numeric',
    size: 'xs',
  },
  {
    key: 'outputTokensDisplay',
    label: t('accounts.usage.output'),
    kind: 'numeric',
    size: 'xs',
  },
  {
    key: 'cachedInputTokensDisplay',
    label: t('accounts.usage.cache'),
    kind: 'numeric',
    size: 'xs',
  },
  {
    key: 'totalTokensDisplay',
    label: t('accounts.usage.total'),
    kind: 'numeric',
    size: 'xs',
  },
  {
    key: 'billingAmountUsdDisplay',
    label: t('accounts.usage.cost'),
    kind: 'numeric',
    size: 'sm',
  },
  {
    key: 'lastUsedAt',
    label: t('accounts.fields.lastUsed'),
    kind: 'datetime',
    size: 'md',
    align: 'right',
  },
])))
</script>

<template>
  <section class="grid gap-4 rounded-lg bg-cp-bg-container p-4 shadow-cp-tertiary xl:min-h-0 xl:grid-cols-[0.52fr_1.48fr]">
    <div class="xl:flex xl:min-h-0 xl:flex-col">
      <div class="mb-3 flex shrink-0 items-center justify-between gap-3">
        <h3 class="m-0 text-cp-lg font-heavy text-cp-text">
          {{ t('accounts.usage.tokensStructure') }}
        </h3>
        <span class="text-cp-xs font-emphasis text-cp-text-quaternary">{{ usageWindowLabels[account.usage.windowKind] }}</span>
      </div>
      <div class="grid gap-2 xl:min-h-0 xl:flex-1 xl:grid-rows-5">
        <div class="flex items-center justify-between rounded-lg bg-cp-green-container px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-green-on-container">{{ t('accounts.usage.input') }}</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.inputTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-orange-container px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-orange-on-container">{{ t('accounts.usage.output') }}</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.outputTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-cyan-container px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-cyan-on-container">{{ t('accounts.usage.cacheRead') }}</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.cachedInputTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-blue-container px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-blue-on-container">{{ t('accounts.usage.reasoning') }}</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.reasoningOutputTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-blue-container px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-blue-on-container">{{ t('accounts.usage.cacheWrites') }}</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.cacheWriteInputTokensDisplay }}
          </strong>
        </div>
      </div>
    </div>

    <div class="min-w-0 pt-4 xl:flex xl:min-h-0 xl:flex-col xl:pt-0 xl:pl-4">
      <div class="mb-3 flex shrink-0 flex-wrap items-center gap-x-4 gap-y-2">
        <h3 class="m-0 shrink-0 text-cp-lg font-heavy text-cp-text">
          {{ t('accounts.usage.modelUsageRanking') }}
        </h3>

        <div class="ml-auto flex items-baseline gap-4">
          <div v-if="hasUsageSummary" class="flex items-baseline gap-1.5 whitespace-nowrap">
            <Sigma class="size-3.5 self-center text-cp-text-tertiary" :stroke-width="1.75" />
            <span :title="t('accounts.usage.totalTokens')">
              <span class="sr-only">{{ t('accounts.usage.totalTokens') }}</span>
              <span class="font-mono text-cp-sm font-emphasis tabular-nums text-cp-text">
                {{ account.usage.totalTokensDisplay }}
              </span>
            </span>
            <span class="mx-0.5 text-[10px] leading-none font-emphasis text-cp-text-quaternary"> / </span>
            <span :title="t('accounts.usage.totalCharge')">
              <span class="sr-only">{{ t('accounts.usage.totalCharges') }}</span>
              <span class="font-mono text-cp-sm font-heavy tabular-nums text-cp-green-text">
                {{ totalBillingDisplay }}
              </span>
            </span>
          </div>
          <span class="whitespace-nowrap text-cp-xs font-emphasis text-cp-text-quaternary">{{ usageWindowLabels[account.usage.windowKind] }}</span>
        </div>
      </div>

      <div class="h-56 min-w-0 xl:h-auto xl:min-h-0 xl:flex-1 xl:basis-0">
        <ZTable
          :columns="modelUsageColumns"
          :data="account.usage.models"
          row-key="model"
          size="small"
          :empty-text="t('accounts.usage.noModelUsageYet')"
        >
          <template #successRateDisplay="{ row }">
            <span :class="modelSuccessRateTextClass(row.successRate)">
              {{ row.successRateDisplay }}
            </span>
          </template>
          <template #lastUsedAt="{ row }">
            <LastUsedAtCell :value="row.lastUsedAt" :full-display="row.lastUsedAtFullDisplay" />
          </template>
        </ZTable>
      </div>
    </div>
  </section>
</template>
