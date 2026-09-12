<script setup lang="ts">
import type { AccountQuotaForecast } from '@/api'
import { ArrowRight, Info, TriangleAlert } from '@lucide/vue'
import { computed } from 'vue'

const props = defineProps<{ forecast: AccountQuotaForecast }>()
const source = computed(() => props.forecast.source)
const metrics = computed(() => [
  {
    label: 'Token 容量',
    observed: source.value?.tokensDisplay ?? '—',
    estimated: props.forecast.estimatedTokensDisplay,
    tone: 'text-cp-cyan-text',
  },
  {
    label: '等价费用 · USD',
    observed: source.value?.usdDisplay ?? '—',
    estimated: props.forecast.estimatedUsdDisplay,
    tone: 'text-cp-green-text',
  },
])
</script>

<template>
  <section v-if="source" aria-label="容量预测" class="flex flex-col gap-5 rounded-cp-card bg-cp-fill-tertiary/70 p-4 md:justify-between [html[data-theme=light]_&]:bg-cp-fill-quaternary/70">
    <div>
      <div class="flex flex-wrap items-center justify-between gap-2">
        <span class="text-cp-xs text-cp-text-secondary">{{ source.label }}已用</span>
        <span
          class="inline-flex items-center gap-1 rounded-cp-sm px-2 py-1 text-cp-xs font-emphasis"
          :class="forecast.lowSample ? 'bg-cp-warning-container text-cp-warning-on-container' : 'bg-cp-info-container text-cp-info-on-container'"
        >
          <TriangleAlert v-if="forecast.lowSample" class="size-3" />
          <Info v-else class="size-3" />
          {{ forecast.lowSample ? '低样本 · 误差较大' : '估算值 · 仅供参考' }}
        </span>
      </div>
      <div class="mt-2 flex items-end justify-between gap-4">
        <strong class="font-mono text-3xl font-heavy tabular-nums text-cp-text [html[data-theme=light]_&]:font-emphasis">{{ source.usedPercentDisplay }}</strong>
        <span class="pb-1 text-cp-xs text-cp-text-secondary">完整源窗口 <span class="font-mono">100%</span></span>
      </div>

      <!-- 实心为已观测比例，虚线为待推算容量，不伪造历史趋势或置信区间。 -->
      <div
        class="relative mt-3 h-3 overflow-hidden rounded-full bg-cp-bg-container"
        role="img"
        :aria-label="`源窗口已用 ${source.usedPercentDisplay}，其余为推算区域`"
      >
        <span class="absolute inset-x-0 top-1/2 border-t-2 border-dashed border-cp-primary/50" />
        <span class="absolute inset-y-0 left-0 rounded-full bg-cp-primary" :style="{ width: `${source.usedPercent ?? 0}%` }" />
      </div>
      <div class="mt-2 flex justify-between font-mono text-[10px] text-cp-text-tertiary" aria-hidden="true">
        <span>0</span><span>25</span><span>50</span><span>75</span><span>100%</span>
      </div>
    </div>

    <div class="grid gap-2">
      <div class="flex justify-between px-3 text-cp-xs text-cp-text-secondary">
        <span>采样区间已记录</span>
        <span>完整{{ forecast.period === 'weekly' ? '周' : '月' }}预测</span>
      </div>
      <div class="grid gap-2">
        <div v-for="metric in metrics" :key="metric.label" class="rounded-cp bg-cp-bg-container px-3 py-3">
          <p class="m-0 text-cp-xs text-cp-text-secondary">
            {{ metric.label }}
          </p>
          <div class="mt-2 grid grid-cols-[1fr_20px_1.2fr] items-center gap-2">
            <span class="font-mono text-cp-sm font-emphasis tabular-nums text-cp-text [html[data-theme=light]_&]:font-medium">{{ metric.observed }}</span>
            <ArrowRight class="size-3.5 text-cp-text-tertiary" />
            <strong class="text-right font-mono text-xl font-heavy tabular-nums" :class="metric.tone">
              <span v-if="metric.estimated !== '—'" class="mr-1 text-cp-sm font-normal text-cp-text-tertiary">≈</span>{{ metric.estimated }}
            </strong>
          </div>
        </div>
      </div>
    </div>

    <div class="grid gap-3">
      <div class="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-2 text-cp-xs">
        <span class="text-cp-text-secondary">快照时源窗口剩余</span>
        <span class="font-mono font-emphasis text-cp-text [html[data-theme=light]_&]:font-medium">
          {{ forecast.remainingTokensDisplay }} Tokens
          <span class="mx-1 text-cp-text-tertiary">/</span>
          {{ forecast.remainingUsdDisplay }}
        </span>
      </div>
      <p v-if="forecast.lowSample" class="m-0 text-cp-xs leading-relaxed text-cp-warning-text">
        有效额度进度或分段样本较少，估算容易波动。
      </p>
      <p v-if="forecast.incompleteCost" class="m-0 text-cp-xs leading-relaxed text-cp-warning-text">
        美元费用记录不完整，暂不预测等价费用。
      </p>
      <p v-if="forecast.incompleteTokens" class="m-0 text-cp-xs leading-relaxed text-cp-warning-text">
        Token 记录不完整，暂不预测 Token 容量；未知用量不按零消耗估算。
      </p>
    </div>
  </section>
</template>
