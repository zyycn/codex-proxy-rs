<script setup lang="ts">
interface TableColumn {
  key: string
  label: string
  width?: string
  align?: 'left' | 'right' | 'center'
  headerClass?: string
  cellClass?: string
}

withDefaults(
  defineProps<{
    columns: TableColumn[]
    rows: object[]
    tableClass?: string
    headerRowClass?: string
    bodyRowClass?: string
    headerCellClass?: string
    bodyCellClass?: string
    striped?: boolean
  }>(),
  {
    tableClass: 'min-w-155',
    headerRowClass:
      'h-10 rounded-xl bg-(--cp-bg-subtle) text-[11px] font-bold text-(--cp-text-muted)',
    bodyRowClass: 'h-13 rounded-[10px] transition-colors',
    headerCellClass:
      'min-w-0 overflow-hidden bg-(--cp-bg-subtle) px-3 text-ellipsis whitespace-nowrap first:rounded-l-xl last:rounded-r-xl',
    bodyCellClass:
      'min-w-0 overflow-hidden px-3 text-ellipsis whitespace-nowrap text-xs font-semibold text-(--cp-text-primary) first:rounded-l-[10px] last:rounded-r-[10px]',
    striped: true,
  },
)

function cellValue(row: object, key: string) {
  return (row as Record<string, unknown>)[key]
}

function rowKey(row: object, index: number) {
  const id = cellValue(row, 'id')
  return typeof id === 'string' || typeof id === 'number' ? id : index
}

function columnWidth(column: TableColumn) {
  return column.width ?? 'auto'
}

function alignClass(column: TableColumn) {
  if (column.align === 'center') {
    return 'text-center'
  }

  if (column.align === 'right') {
    return 'text-right'
  }

  return 'text-left'
}

function rowSurfaceClass(index: number, striped: boolean) {
  return [
    striped && index % 2 === 1 ? 'bg-(--cp-bg-subtle)' : 'bg-(--cp-bg-surface)',
    'group-hover:bg-(--cp-default-bg-hover)',
  ]
}
</script>

<template>
  <div class="cp-scrollbar max-w-full overflow-x-auto pb-1">
    <table
      class="table-fixed border-separate border-spacing-y-2 text-left"
      :class="tableClass"
      :style="{ width: '100%' }"
    >
      <colgroup>
        <col v-for="column in columns" :key="column.key" :style="{ width: columnWidth(column) }" />
      </colgroup>

      <thead>
        <tr :class="headerRowClass">
          <th
            v-for="column in columns"
            :key="column.key"
            :class="[headerCellClass, alignClass(column), column.headerClass]"
            scope="col"
          >
            {{ column.label }}
          </th>
        </tr>
      </thead>

      <tbody>
        <tr v-for="(row, index) in rows" :key="rowKey(row, index)" :class="['group', bodyRowClass]">
          <td
            v-for="column in columns"
            :key="column.key"
            :class="[
              bodyCellClass,
              rowSurfaceClass(index, striped),
              alignClass(column),
              column.cellClass,
            ]"
          >
            <slot :name="column.key" :row="row" :value="cellValue(row, column.key)">
              {{ cellValue(row, column.key) }}
            </slot>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>
