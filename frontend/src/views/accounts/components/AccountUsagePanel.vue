<script setup lang="ts">
import type { AccountRow } from '../constants'

import { Sigma } from '@lucide/vue'
import { computed } from 'vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { modelSuccessRateTextClass } from '../constants'

const props = defineProps<{
  account: AccountRow
}>()

type AccountModelUsage = AccountRow['usage']['models'][number]

const totalBilling = computed(() => props.account.usage.costs.find(cost => cost.currency.toUpperCase() === 'USD'))
const totalBillingDisplay = computed(() => totalBilling.value?.estimatedAmountDisplay ?? '—')
const hasUsageSummary = computed(() => (props.account.usage.totalTokens ?? 0) > 0 || totalBilling.value !== undefined)

const modelUsageColumns = defineTableColumns<AccountModelUsage>([
  { key: 'model', label: '模型', kind: 'text', size: 'lg' },
  { key: 'requestCountDisplay', label: '调用', kind: 'numeric', size: 'xs' },
  { key: 'successRateDisplay', label: '成功率', kind: 'numeric', size: 'sm' },
  { key: 'inputTokensDisplay', label: '输入', kind: 'numeric', size: 'xs' },
  { key: 'outputTokensDisplay', label: '输出', kind: 'numeric', size: 'xs' },
  { key: 'cachedTokensDisplay', label: '缓存', kind: 'numeric', size: 'xs' },
  { key: 'totalTokensDisplay', label: '总计', kind: 'numeric', size: 'xs' },
  { key: 'billingAmountUsdDisplay', label: '计费', kind: 'numeric', size: 'sm' },
  { key: 'lastUsedAtDisplay', label: '最近请求', kind: 'datetime', size: 'sm' },
])
</script>

<template>
  <section class="grid gap-4 rounded-lg bg-cp-bg-container p-4 shadow-cp-tertiary xl:min-h-0 xl:grid-cols-[0.52fr_1.48fr]">
    <div class="xl:flex xl:min-h-0 xl:flex-col">
      <div class="mb-3 flex shrink-0 items-baseline justify-between gap-3">
        <h3 class="m-0 text-cp-lg font-heavy text-cp-text">
          Tokens 结构
        </h3>
        <span class="text-cp-xs font-emphasis text-cp-text-quaternary">当前额度窗口</span>
      </div>
      <div class="grid gap-2 xl:min-h-0 xl:flex-1 xl:grid-rows-5">
        <div class="flex items-center justify-between rounded-lg bg-cp-green-bg px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-green-text-on-bg">输入</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.inputTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-orange-bg px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-orange-text-on-bg">输出</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.outputTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-cyan-bg px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-cyan-text-on-bg">缓存</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.cachedTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-blue-bg px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-blue-text-on-bg">推理</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.reasoningTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-blue-bg px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-blue-text-on-bg">读取</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.readTokensDisplay }}
          </strong>
        </div>
      </div>
    </div>

    <div class="min-w-0 pt-4 xl:flex xl:min-h-0 xl:flex-col xl:pt-0 xl:pl-4">
      <div class="mb-3 flex shrink-0 flex-wrap items-center gap-x-4 gap-y-2">
        <h3 class="m-0 shrink-0 text-cp-lg font-heavy text-cp-text">
          模型使用排行
        </h3>

        <div class="ml-auto flex items-center gap-4">
          <div v-if="hasUsageSummary" class="flex items-baseline gap-1.5 whitespace-nowrap">
            <Sigma class="size-3.5 self-center text-cp-text-tertiary" :stroke-width="1.75" />
            <span title="总 Token">
              <span class="sr-only">总 Token：</span>
              <span class="font-mono text-cp-sm font-emphasis tabular-nums text-cp-text">
                {{ account.usage.totalTokensDisplay }}
              </span>
            </span>
            <span class="mx-0.5 text-[10px] leading-none font-emphasis text-cp-text-quaternary"> / </span>
            <span title="总计费">
              <span class="sr-only">总计费：</span>
              <span class="font-mono text-cp-sm font-heavy tabular-nums text-cp-green-text">
                {{ totalBillingDisplay }}
              </span>
            </span>
          </div>
          <span class="whitespace-nowrap text-cp-xs font-emphasis text-cp-text-quaternary">当前额度窗口</span>
        </div>
      </div>

      <div class="h-56 min-w-0 xl:h-auto xl:min-h-0 xl:flex-1 xl:basis-0">
        <BaseTable
          :columns="modelUsageColumns"
          :rows="account.usage.models"
          row-key="model"
          density="compact"
          empty-text="暂无模型用量"
        >
          <template #successRateDisplay="{ row }">
            <span :class="modelSuccessRateTextClass(row.successRate)">
              {{ row.successRateDisplay }}
            </span>
          </template>
        </BaseTable>
      </div>
    </div>
  </section>
</template>
