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
  normal: 'bg-[var(--cp-normal-bg)] text-[var(--cp-normal-text)]',
  info: 'bg-[var(--cp-info-bg)] text-[var(--cp-info-text)]',
  success: 'bg-[var(--cp-success-bg)] text-[var(--cp-success-text)]',
  warning: 'bg-[var(--cp-warning-bg)] text-[var(--cp-warning-text)]',
  danger: 'bg-[var(--cp-danger-bg)] text-[var(--cp-danger-text)]',
}

const statusToneClasses: Record<SemanticTone, string> = {
  normal: 'text-[var(--cp-normal-text)]',
  info: 'text-[var(--cp-success-text)]',
  success: 'text-[var(--cp-success-text)]',
  warning: 'text-[var(--cp-warning-text)]',
  danger: 'text-[var(--cp-danger-text)]',
}

function rowTone(row: object) {
  return (row as EventLogItem).tone
}
</script>

<template>
  <BaseCard as="article" :padded="false" class="h-[350px] w-full px-7 pt-6">
    <header class="flex items-start justify-between">
      <div>
        <h2 class="m-0 text-[20px] leading-[1.15] font-[760] text-[var(--cp-text-primary)]">事件日志</h2>
        <p class="mt-[7px] mb-0 text-[13px] leading-[1.15] font-[650] text-[var(--cp-text-secondary)]">最近 50 条事件</p>
      </div>

      <div class="grid h-9 w-[206px] grid-cols-[62px_58px_58px] gap-1 rounded-xl bg-[var(--cp-bg-muted)] p-1">
        <button
          v-for="filter in filters"
          :key="filter"
          class="h-7 rounded-[9px] border-0 text-[12px] leading-[1.15] font-[650]"
          :class="filter === '全部' ? 'bg-white text-[var(--cp-text-primary)] shadow-[var(--cp-shadow-control)]' : 'bg-transparent text-[var(--cp-text-secondary)]'"
          type="button"
        >
          {{ filter }}
        </button>
      </div>
    </header>

    <div class="mt-[17px] flex h-[240px] w-full justify-between overflow-hidden">
      <BaseTable
        class="min-w-0 flex-1"
        :columns="eventLogColumns"
        :rows="rows"
        table-class="min-w-full"
        header-row-class="h-10 rounded-xl bg-[var(--cp-bg-subtle)] text-[12px] leading-[1.15] font-bold text-[var(--cp-text-secondary)]"
        body-row-class="h-14 rounded-[10px] transition-[background-color,transform] duration-200 hover:-translate-y-px hover:bg-[var(--cp-bg-subtle)] active:translate-y-0"
        header-cell-class="min-w-0 overflow-hidden bg-[var(--cp-bg-subtle)] px-5 text-ellipsis whitespace-nowrap first:rounded-l-xl last:rounded-r-xl"
        body-cell-class="min-w-0 overflow-hidden px-5 text-ellipsis whitespace-nowrap text-[12px] leading-[1.15] font-[650] text-[var(--cp-text-primary)] first:rounded-l-[10px] last:rounded-r-[10px]"
      >
        <template #time="{ value }">
          <span class="font-mono tabular-nums">{{ value }}</span>
        </template>

        <template #level="{ row, value }">
          <span class="inline-flex h-6 w-[58px] items-center justify-center rounded-full text-[12px] leading-[1.15] font-bold" :class="levelToneClasses[rowTone(row)]">{{ value }}</span>
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
