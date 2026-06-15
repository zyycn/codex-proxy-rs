<script setup lang="ts">
interface TableColumn {
  key: string
  label: string
  width?: string
  align?: 'left' | 'right' | 'center'
}

defineProps<{
  columns: TableColumn[]
  rows: object[]
}>()

function cellValue(row: object, key: string) {
  return (row as Record<string, unknown>)[key]
}

function rowKey(row: object, index: number) {
  const id = cellValue(row, 'id')
  return typeof id === 'string' || typeof id === 'number' ? id : index
}

function gridColumns(columns: TableColumn[]) {
  return columns.map((column) => column.width ?? '1fr').join(' ')
}
</script>

<template>
  <div class="max-w-full overflow-x-auto pb-1">
    <div class="grid min-w-[620px] gap-2">
      <div
        class="grid min-h-10 items-center rounded-xl bg-[var(--cp-bg-subtle)] text-[11px] font-bold text-[var(--cp-text-muted)]"
        :style="{ gridTemplateColumns: gridColumns(columns) }"
      >
        <div
          v-for="column in columns"
          :key="column.key"
          class="min-w-0 overflow-hidden px-3 text-ellipsis whitespace-nowrap"
          :class="column.align === 'center' ? 'text-center' : column.align === 'right' ? 'text-right' : 'text-left'"
        >
          {{ column.label }}
        </div>
      </div>
      <div
        v-for="(row, index) in rows"
        :key="rowKey(row, index)"
        class="grid min-h-[52px] items-center rounded-[10px] bg-[var(--cp-bg-surface)] even:bg-[var(--cp-bg-subtle)]"
        :style="{ gridTemplateColumns: gridColumns(columns) }"
      >
        <div
          v-for="column in columns"
          :key="column.key"
          class="min-w-0 overflow-hidden px-3 text-ellipsis whitespace-nowrap text-xs font-semibold text-[var(--cp-text-primary)]"
          :class="column.align === 'center' ? 'text-center' : column.align === 'right' ? 'text-right' : 'text-left'"
        >
          <slot :name="column.key" :row="row" :value="cellValue(row, column.key)">
            {{ cellValue(row, column.key) }}
          </slot>
        </div>
      </div>
    </div>
  </div>
</template>
