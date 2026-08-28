<script setup lang="ts">
import type { AccountProfileActivityInsights, AccountProfileInvocation } from '@/api'
import { Package } from '@lucide/vue'
import { computed } from 'vue'

import { formatInteger } from '@/utils/number'

const props = defineProps<{
  insights: AccountProfileActivityInsights
}>()

const insightRows = computed(() => [
  { label: '快速模式', value: formatPercent(props.insights.fastModePercent) },
  {
    label: '最常用的推理强度',
    value: formatReasoning(props.insights.reasoningEffort, props.insights.reasoningEffortPercent),
  },
  { label: '已探索的技能', value: formatNumber(props.insights.skillsExplored) },
  { label: '使用的技能总数', value: formatNumber(props.insights.totalSkillsUsed) },
  { label: '聊天总数', value: formatNumber(props.insights.totalThreads) },
])
const invocations = computed(() => (props.insights.invocations ?? [])
  .map(invocationView)
  .filter((invocation): invocation is NonNullable<typeof invocation> => invocation !== null))

function formatNumber(value: number | null) {
  return value === null ? '—' : formatInteger(value)
}

function formatPercent(value: number | null) {
  if (value === null)
    return '—'
  return `${new Intl.NumberFormat('zh-CN', { maximumFractionDigits: 1 }).format(value)}%`
}

function formatReasoning(effort: string | null, percent: number | null) {
  if (!effort)
    return '—'
  return percent === null ? effort : `${effort} · ${formatPercent(percent)}`
}

function invocationView(invocation: AccountProfileInvocation) {
  const isPlugin = invocation.type === 'plugin'
  const name = (isPlugin ? invocation.pluginName : invocation.skillName)?.trim()
  if (!name || invocation.usageCount === null)
    return null
  return {
    key: `${invocation.type}:${invocation.pluginId ?? invocation.skillId ?? name}`,
    name,
    count: invocation.usageCount,
  }
}
</script>

<template>
  <section class="grid gap-8 sm:grid-cols-2 sm:gap-12" aria-labelledby="profile-activity-insights-title">
    <div>
      <h3 id="profile-activity-insights-title" class="m-0 text-cp-lg font-heavy text-cp-text">
        活动洞察
      </h3>
      <dl class="mt-4 mb-0 grid gap-3.5">
        <div
          v-for="row in insightRows"
          :key="row.label"
          class="grid grid-cols-[minmax(0,1fr)_auto] items-baseline gap-4"
        >
          <dt class="min-w-0 text-cp font-semibold text-cp-text-secondary">
            {{ row.label }}
          </dt>
          <dd class="m-0 font-mono font-heavy tabular-nums text-cp-text">
            {{ row.value }}
          </dd>
        </div>
      </dl>
    </div>

    <div>
      <h3 class="m-0 text-cp-lg font-heavy text-cp-text">
        最常用的插件与技能
      </h3>
      <ol v-if="invocations.length > 0" class="mt-3 mb-0 grid list-none gap-1.5 p-0">
        <li
          v-for="invocation in invocations"
          :key="invocation.key"
          class="grid grid-cols-[24px_minmax(0,1fr)_auto] items-center gap-2.5 py-1.5"
        >
          <span
            class="grid size-6 place-items-center rounded-cp bg-cp-fill-quaternary text-cp-text-secondary"
            aria-hidden="true"
          >
            <Package class="size-3.5" :stroke-width="2" />
          </span>
          <span class="truncate text-cp font-bold text-cp-text">{{ invocation.name }}</span>
          <span class="font-mono text-cp-sm font-semibold tabular-nums text-cp-text-secondary">
            {{ formatInteger(invocation.count) }} 次运行
          </span>
        </li>
      </ol>
      <p v-else class="mt-4 mb-0 text-cp-sm font-semibold text-cp-text-quaternary">
        暂无插件或技能调用记录
      </p>
    </div>
  </section>
</template>
