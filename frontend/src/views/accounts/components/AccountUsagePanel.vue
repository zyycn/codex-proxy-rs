<script setup lang="ts">
import type { AccountRow } from '../constants'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { modelSuccessRateTextClass } from '../constants'

defineProps<{
  account: AccountRow
}>()

type AccountModelUsage = AccountRow['usage']['models'][number]

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
    class="grid gap-4 rounded-lg bg-cp-surface p-4 shadow-cp-control xl:grid-cols-[0.52fr_1.48fr]"
  >
    <div>
      <div class="mb-3 flex items-baseline justify-between gap-3">
        <h3 class="m-0 text-[14px] font-heavy text-cp-primary">
          Token 结构
        </h3>
        <span class="text-[11px] font-emphasis text-cp-muted-text">当前额度窗口</span>
      </div>
      <div class="grid gap-2">
        <div class="flex items-center justify-between rounded-lg bg-cp-success-bg px-3 py-2">
          <span class="text-[12px] font-bold text-cp-success-text">输入 Tokens</span>
          <strong class="font-mono text-[13px] text-cp-primary">
            {{ account.usage.inputTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-warning-bg px-3 py-2">
          <span class="text-[12px] font-bold text-cp-warning-text">输出 Tokens</span>
          <strong class="font-mono text-[13px] text-cp-primary">
            {{ account.usage.outputTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-normal-bg px-3 py-2">
          <span class="text-[12px] font-bold text-cp-normal-text">缓存 Tokens</span>
          <strong class="font-mono text-[13px] text-cp-primary">
            {{ account.usage.cachedTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-info-bg px-3 py-2">
          <span class="text-[12px] font-bold text-cp-info-text">创建</span>
          <strong class="font-mono text-[13px] text-cp-primary">
            {{ account.usage.createdTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-info-bg px-3 py-2">
          <span class="text-[12px] font-bold text-cp-info-text">读取</span>
          <strong class="font-mono text-[13px] text-cp-primary">
            {{ account.usage.readTokensDisplay }}
          </strong>
        </div>
      </div>
    </div>

    <div
      class="min-w-0 pt-4 xl:pt-0 xl:pl-4"
    >
      <div class="mb-3 flex items-center justify-between">
        <h3 class="m-0 text-[14px] font-heavy text-cp-primary">
          模型使用排行
        </h3>
        <span class="text-[11px] font-emphasis text-cp-muted-text">当前额度窗口</span>
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
