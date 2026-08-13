<script setup lang="ts">
import type { AccountRow } from '../constants'
import { modelSuccessRateTextClass } from '../constants'

defineProps<{
  account: AccountRow
}>()
</script>

<template>
  <section
    class="grid gap-4 rounded-lg bg-(--cp-bg-surface) p-4 shadow-(--cp-shadow-control) xl:grid-cols-[0.52fr_1.48fr]"
  >
    <div>
      <div class="mb-3 flex items-baseline justify-between gap-3">
        <h3 class="m-0 text-[14px] font-heavy text-(--cp-text-primary)">
          Token 结构
        </h3>
        <span class="text-[11px] font-emphasis text-(--cp-text-muted)">当前额度窗口</span>
      </div>
      <div class="grid gap-2">
        <div class="flex items-center justify-between rounded-lg bg-(--cp-success-bg) px-3 py-2">
          <span class="text-[12px] font-bold text-(--cp-success-text)">输入 Tokens</span>
          <strong class="font-mono text-[13px] text-(--cp-text-primary)">
            {{ account.usage.inputTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-(--cp-warning-bg) px-3 py-2">
          <span class="text-[12px] font-bold text-(--cp-warning-text)">输出 Tokens</span>
          <strong class="font-mono text-[13px] text-(--cp-text-primary)">
            {{ account.usage.outputTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-(--cp-normal-bg) px-3 py-2">
          <span class="text-[12px] font-bold text-(--cp-normal-text)">缓存 Tokens</span>
          <strong class="font-mono text-[13px] text-(--cp-text-primary)">
            {{ account.usage.cachedTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-(--cp-info-bg) px-3 py-2">
          <span class="text-[12px] font-bold text-(--cp-info-text)">创建</span>
          <strong class="font-mono text-[13px] text-(--cp-text-primary)">
            {{ account.usage.createdTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-(--cp-info-bg) px-3 py-2">
          <span class="text-[12px] font-bold text-(--cp-info-text)">读取</span>
          <strong class="font-mono text-[13px] text-(--cp-text-primary)">
            {{ account.usage.readTokensDisplay }}
          </strong>
        </div>
      </div>
    </div>

    <div
      class="min-w-0 pt-4 shadow-[inset_0_1px_0_var(--cp-divider-subtle)] xl:pt-0 xl:pl-4 xl:shadow-[inset_1px_0_0_var(--cp-divider-subtle)]"
    >
      <div class="mb-3 flex items-center justify-between">
        <h3 class="m-0 text-[14px] font-heavy text-(--cp-text-primary)">
          模型使用排行
        </h3>
        <span class="text-[11px] font-emphasis text-(--cp-text-muted)">当前额度窗口</span>
      </div>

      <div
        class="grid grid-cols-[1.5fr_0.7fr_0.8fr_1fr_1fr_1fr_1fr_1fr_1.2fr] gap-3 pb-2 text-[11px] font-heavy text-(--cp-text-secondary) shadow-[inset_0_-1px_0_var(--cp-divider-subtle)]"
      >
        <span>模型</span>
        <span>调用</span>
        <span>成功率</span>
        <span>输入</span>
        <span>输出</span>
        <span>缓存</span>
        <span>总计</span>
        <span>计费</span>
        <span>最近请求</span>
      </div>
      <div
        v-if="account.usage.models.length === 0"
        class="pt-3 text-[12px] font-emphasis text-(--cp-text-muted)"
      >
        -
      </div>
      <template v-else>
        <div
          v-for="model in account.usage.models"
          :key="model.model"
          class="grid grid-cols-[1.5fr_0.7fr_0.8fr_1fr_1fr_1fr_1fr_1fr_1.2fr] gap-3 pt-3 text-[12px] font-emphasis text-(--cp-text-primary)"
        >
          <span class="truncate">{{ model.model }}</span>
          <span>{{ model.requestCountDisplay }}</span>
          <span :class="modelSuccessRateTextClass(model.successRate)">
            {{ model.successRateDisplay }}
          </span>
          <span>{{ model.inputTokensDisplay }}</span>
          <span>{{ model.outputTokensDisplay }}</span>
          <span>{{ model.cachedTokensDisplay }}</span>
          <span>{{ model.totalTokensDisplay }}</span>
          <span>{{ model.billingAmountUsdDisplay }}</span>
          <span>{{ model.lastUsedAtDisplay }}</span>
        </div>
      </template>
    </div>
  </section>
</template>
