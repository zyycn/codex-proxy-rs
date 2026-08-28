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

const totalBilling = computed(() =>
  props.account.usage.costs.find(cost => cost.currency.toUpperCase() === 'USD'),
)
const totalBillingDisplay = computed(() =>
  totalBilling.value?.estimatedAmountDisplay ?? '—',
)
const hasUsageSummary = computed(() =>
  (props.account.usage.totalTokens ?? 0) > 0 || totalBilling.value !== undefined,
)

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
  <section
    class="grid gap-4 rounded-lg bg-cp-bg-container p-4 shadow-cp-tertiary xl:grid-cols-[0.52fr_1.48fr]"
  >
    <div>
      <div class="mb-3 flex items-baseline justify-between gap-3">
        <h3 class="m-0 text-cp-lg font-heavy text-cp-text">
          Token 结构
        </h3>
        <span class="text-cp-xs font-emphasis text-cp-text-quaternary">当前额度窗口</span>
      </div>
      <div class="grid gap-2">
        <div class="flex items-center justify-between rounded-lg bg-cp-success-bg px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-success-text">输入 Tokens</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.inputTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-warning-bg px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-warning-text">输出 Tokens</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.outputTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-status-normal-bg px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-status-normal-text">缓存 Tokens</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.cachedTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-info-bg px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-info-text">创建</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.createdTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-info-bg px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-info-text">读取</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.readTokensDisplay }}
          </strong>
        </div>
      </div>
    </div>

    <div
      class="min-w-0 pt-4 xl:pt-0 xl:pl-4"
    >
      <div class="mb-3 flex flex-wrap items-center gap-x-4 gap-y-2">
        <h3 class="m-0 shrink-0 text-cp-lg font-heavy text-cp-text">
          模型使用排行
        </h3>

        <div class="ml-auto flex items-center gap-4">
          <div v-if="hasUsageSummary" class="flex items-baseline gap-1.5 whitespace-nowrap">
            <Sigma
              class="size-3.5 self-center text-cp-text-tertiary"
              :stroke-width="1.75"
              aria-hidden="true"
            />
            <span title="总 Token">
              <span class="sr-only">总 Token：</span>
              <span class="font-mono text-cp-sm font-emphasis tabular-nums text-cp-text">
                {{ account.usage.totalTokensDisplay }}
              </span>
            </span>
            <span
              class="mx-0.5 text-[10px] leading-none font-emphasis text-cp-text-quaternary"
              aria-hidden="true"
            >
              /
            </span>
            <span title="总计费">
              <span class="sr-only">总计费：</span>
              <span class="font-mono text-cp-sm font-heavy tabular-nums text-cp-success-text">
                {{ totalBillingDisplay }}
              </span>
            </span>
          </div>
          <span class="whitespace-nowrap text-cp-xs font-emphasis text-cp-text-quaternary">当前额度窗口</span>
        </div>
      </div>

      <div class="h-52 min-w-0">
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
