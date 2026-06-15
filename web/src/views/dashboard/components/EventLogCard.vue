<script setup lang="ts">
import type { EventLogItem, SemanticTone } from '../types'

defineProps<{
  rows: EventLogItem[]
}>()

const filters = ['全部', '警告', '错误']
const columns = [
  { label: '时间', className: '' },
  { label: '级别', className: 'col-start-3' },
  { label: '请求 ID', className: 'col-start-5' },
  { label: '路由', className: 'col-start-7' },
  { label: '模型', className: 'col-start-9' },
  { label: '状态码', className: 'col-start-11' },
  { label: '延迟', className: '[grid-column-start:13]' },
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
</script>

<template>
  <article class="h-[350px] w-[1584px] rounded-[18px] bg-white px-7 pt-6 shadow-[var(--cp-shadow-card)]">
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

    <div class="mt-[26px] h-10 w-[1528px] grid grid-cols-[150px_40px_90px_20px_230px_22px_250px_24px_170px_24px_96px_32px_100px] items-center rounded-xl bg-[var(--cp-bg-subtle)] px-5">
      <span
        v-for="column in columns"
        :key="column.label"
        class="text-[12px] leading-[1.15] font-bold text-[var(--cp-text-secondary)]"
        :class="column.className"
      >
        {{ column.label }}
      </span>
    </div>

    <div class="mt-4 flex h-[184px] w-[1528px] justify-between overflow-hidden">
      <div class="grid w-[1476px] gap-2">
        <div
          v-for="(row, index) in rows"
          :key="row.id"
          class="grid h-14 w-[1476px] grid-cols-[150px_40px_90px_20px_230px_22px_250px_24px_170px_24px_96px_32px_100px] items-center rounded-[10px] px-5"
          :class="index % 2 === 0 ? 'bg-white' : 'bg-[var(--cp-bg-subtle)]'"
        >
          <span class="font-mono text-[12px] leading-[1.15] font-[650] tabular-nums text-[var(--cp-text-primary)]">{{ row.time }}</span>
          <span class="col-start-3 inline-flex h-6 w-[58px] items-center justify-center rounded-full text-[12px] leading-[1.15] font-bold" :class="levelToneClasses[row.tone]">{{ row.level }}</span>
          <span class="col-start-5 font-mono text-[12px] leading-[1.15] font-[650] tabular-nums text-[var(--cp-text-primary)]">{{ row.requestId }}</span>
          <span class="col-start-7 text-[12px] leading-[1.15] font-[650] text-[var(--cp-text-primary)]">{{ row.route }}</span>
          <span class="col-start-9 text-[12px] leading-[1.15] font-[650] text-[var(--cp-text-primary)]">{{ row.model }}</span>
          <span class="col-start-11 font-mono text-[12px] leading-[1.15] font-bold tabular-nums" :class="statusToneClasses[row.tone]">{{ row.statusCode }}</span>
          <span class="[grid-column-start:13] font-mono text-[12px] leading-[1.15] font-[650] tabular-nums text-[var(--cp-text-primary)]">{{ row.latency }}</span>
        </div>
      </div>

      <div class="mt-1.5 h-[172px] w-[3px] overflow-hidden rounded-full bg-[var(--cp-bg-muted)]">
        <i class="mt-2 block h-16 w-[3px] rounded-full bg-[var(--cp-text-muted)]" />
      </div>
    </div>
  </article>
</template>
