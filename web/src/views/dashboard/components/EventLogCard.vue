<script setup lang="ts">
import BaseCard from '../../../components/base/BaseCard.vue'
import BaseTable from '../../../components/base/BaseTable.vue'
import type { EventLogItem, SemanticTone } from '../types'

defineProps<{
  rows: EventLogItem[]
}>()

const filters = ['全部', '警告', '错误']
const eventLogColumns = [
  { key: 'time', label: '时间', width: '11.5%' },
  { key: 'level', label: '级别', width: '7.5%' },
  { key: 'requestId', label: '请求 ID', width: '17%' },
  { key: 'route', label: '路由', width: '18.5%' },
  { key: 'model', label: '模型', width: '13%' },
  { key: 'statusCode', label: '状态码', width: '8.5%' },
  { key: 'latency', label: '延迟', width: '24%' },
]

const levelToneClasses: Record<SemanticTone, string> = {
  normal: 'bg-(--cp-normal-bg) text-(--cp-normal-text)',
  info: 'bg-(--cp-info-bg) text-(--cp-info-text)',
  success: 'bg-(--cp-success-bg) text-(--cp-success-text)',
  warning: 'bg-(--cp-warning-bg) text-(--cp-warning-text)',
  danger: 'bg-(--cp-danger-bg) text-(--cp-danger-text)',
}

const statusToneClasses: Record<SemanticTone, string> = {
  normal: 'text-(--cp-normal-text)',
  info: 'text-(--cp-success-text)',
  success: 'text-(--cp-success-text)',
  warning: 'text-(--cp-warning-text)',
  danger: 'text-(--cp-danger-text)',
}

function rowTone(row: object) {
  return (row as EventLogItem).tone
}
</script>

<template>
  <BaseCard as="article" :padded="false" class="h-[350px] w-full px-7 pt-6">
    <header class="flex items-start justify-between">
      <div>
        <h2 class="m-0 text-xl leading-[1.15] font-[760] text-(--cp-text-primary)">事件日志</h2>
        <p class="mt-1.75 mb-0 text-[13px] leading-[1.15] font-[650] text-(--cp-text-secondary)">最近 50 条事件</p>
      </div>

      <div class="grid h-9 w-[206px] grid-cols-[62px_58px_58px] gap-1 rounded-xl bg-(--cp-bg-muted) p-1">
        <button
          v-for="filter in filters"
          :key="filter"
          class="h-7 rounded-[9px] border-0 text-xs leading-[1.15] font-[650]"
          :class="filter === '全部' ? 'bg-white text-(--cp-text-primary) shadow-(--cp-shadow-control)' : 'bg-transparent text-(--cp-text-secondary)'"
          type="button"
        >
          {{ filter }}
        </button>
      </div>
    </header>

    <div class="mt-[17px] flex h-60 w-full justify-between overflow-hidden">
      <BaseTable
        class="min-w-0 flex-1"
        :columns="eventLogColumns"
        :rows="rows"
        table-class="min-w-full"
        header-row-class="h-10 rounded-xl bg-(--cp-bg-subtle) text-xs leading-[1.15] font-bold text-(--cp-text-secondary)"
        body-row-class="h-14 rounded-[10px] transition-colors duration-200 hover:bg-(--cp-bg-subtle)"
        header-cell-class="min-w-0 overflow-hidden bg-(--cp-bg-subtle) px-5 text-ellipsis whitespace-nowrap first:rounded-l-xl last:rounded-r-xl"
        body-cell-class="min-w-0 overflow-hidden px-5 text-ellipsis whitespace-nowrap text-xs leading-[1.15] font-[650] text-(--cp-text-primary) first:rounded-l-[10px] last:rounded-r-[10px]"
      >
        <template #time="{ value }">
          <span class="font-mono tabular-nums">{{ value }}</span>
        </template>

        <template #level="{ row, value }">
          <span class="inline-flex h-6 w-[58px] items-center justify-center rounded-full text-xs leading-[1.15] font-bold" :class="levelToneClasses[rowTone(row)]">{{ value }}</span>
        </template>

        <template #requestId="{ value }">
          <span class="font-mono tabular-nums">{{ value }}</span>
        </template>

        <template #statusCode="{ row, value }">
          <span class="font-mono font-bold tabular-nums" :class="statusToneClasses[rowTone(row)]">{{ value }}</span>
        </template>

        <template #latency="{ value }">
          <span class="font-mono tabular-nums">{{ value }}</span>
        </template>
      </BaseTable>
    </div>
  </BaseCard>
</template>
