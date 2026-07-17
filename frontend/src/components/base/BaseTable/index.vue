<script setup lang="ts" generic="Row extends Record<string, unknown> = Record<string, unknown>">
import type { BaseTableColumn, BaseTableSort, ResolvedTableColumn } from './columns'
import type { BaseTablePagination as BaseTablePaginationConfig } from './pagination'

import { Triangle } from '@lucide/vue'
import { computed, shallowRef, useSlots, watch } from 'vue'
import BaseEmpty from '../BaseEmpty.vue'
import BaseScrollbar from '../BaseScrollbar.vue'
import BaseTablePagination from './BaseTablePagination.vue'
import {
  alignClass,

  cellContentClass,
  cellDisplayValue,
  cellTitle,
  cellValue,
  columnStyle,
  resolveColumns,

  tableStyle as resolveTableStyle,
} from './columns'
import { useHorizontalStickyShadow } from './useHorizontalStickyShadow'

type RowKey = string | ((row: Row, index: number) => string | number)

const props = withDefaults(
  defineProps<{
    columns: BaseTableColumn<Row>[]
    rows: Row[]
    rowKey?: RowKey
    selectedRowKeys?: Array<string | number>
    stripe?: boolean
    compact?: boolean
    loading?: boolean
    emptyText?: string
    maxHeight?: string
    minWidth?: number | string
    pagination?: BaseTablePaginationConfig
    expandedRowKeys?: Array<string | number>
    sort?: BaseTableSort
  }>(),
  {
    rowKey: 'id',
    selectedRowKeys: () => [],
    expandedRowKeys: () => [],
    stripe: true,
    compact: false,
    loading: false,
    emptyText: '暂无数据',
    maxHeight: undefined,
    minWidth: undefined,
  },
)

const emit = defineEmits<{
  'page-change': [page: number]
  'page-size-change': [pageSize: number]
  'sort-change': [sort: BaseTableSort | undefined]
}>()
const slots = useSlots()

const headerRowClass = computed(() => [
  props.compact ? 'h-8 text-[11px]' : 'h-10 text-[12px]',
  'font-bold text-(--cp-text-muted)',
])
const bodyRowClass = computed(() => (props.compact ? 'h-10' : 'h-13'))
const headerCellClass = computed(() => [
  'min-w-0 bg-(--cp-bg-subtle) first:pl-3 shadow-[0_10px_16px_-18px_#0e172638]',
  props.compact ? 'px-3' : 'px-4',
])
const bodyCellClass = computed(() => [
  'min-w-0 first:pl-3 text-(--cp-text-primary) first:rounded-l-lg',
  props.compact ? 'px-3 text-[12px]' : 'px-4 text-[13px]',
])

const computedColumns = computed(() => resolveColumns(props.columns, props.minWidth))
const tableViewStyle = computed(() => resolveTableStyle(props.minWidth))
const retainedRows = shallowRef<Row[]>([])
watch(
  [() => props.rows, () => props.loading],
  ([rows, loading]) => {
    if (rows.length > 0 || !loading) {
      retainedRows.value = rows
    }
  },
  { immediate: true },
)
const displayRows = computed(() =>
  props.loading && props.rows.length === 0 ? retainedRows.value : props.rows,
)
const hasRows = computed(() => displayRows.value.length > 0)
const { horizontalScrolled, horizontalCanScrollRight, handleTableScroll }
  = useHorizontalStickyShadow({
    hasRows,
    watchSources: () => [displayRows.value.length, props.minWidth],
  })

function fixedHeaderClass(column: ResolvedTableColumn<Row>) {
  if (!column.fixed) {
    return undefined
  }

  const showShadow
    = column.fixed === 'left' ? horizontalScrolled.value : horizontalCanScrollRight.value

  return [
    'sticky z-30 bg-(--cp-bg-subtle)',
    column.fixed === 'left' ? 'left-0' : 'right-0',
    showShadow
      ? column.fixed === 'left'
        ? 'shadow-[8px_0_14px_-14px_var(--cp-shadow-sticky)]'
        : 'shadow-[-8px_0_14px_-14px_var(--cp-shadow-sticky)]'
      : undefined,
  ]
}

function fixedBodyClass(column: ResolvedTableColumn<Row>, row: Row, index: number) {
  if (!column.fixed) {
    return undefined
  }

  const showShadow
    = column.fixed === 'left' ? horizontalScrolled.value : horizontalCanScrollRight.value

  return [
    'sticky z-20',
    column.fixed === 'left' ? 'left-0' : 'right-0',
    showShadow
      ? column.fixed === 'left'
        ? 'shadow-[8px_0_14px_-14px_var(--cp-shadow-sticky)]'
        : 'shadow-[-8px_0_14px_-14px_var(--cp-shadow-sticky)]'
      : undefined,
    isRowSelected(row, index)
      ? 'bg-(--cp-bg-tertiary)'
      : props.stripe && index % 2 === 1
        ? 'bg-(--cp-bg-subtle)'
        : 'bg-(--cp-bg-surface)',
  ]
}

function getRowKey(row: Row, index: number) {
  if (typeof props.rowKey === 'function') {
    return props.rowKey(row, index)
  }

  const value = row[props.rowKey]
  return typeof value === 'string' || typeof value === 'number' ? value : index
}

function isRowSelected(row: Row, index: number) {
  return props.selectedRowKeys.includes(getRowKey(row, index))
}

function isRowExpanded(row: Row, index: number) {
  return props.expandedRowKeys.includes(getRowKey(row, index))
}

function rowClass(row: Row, index: number) {
  return [
    bodyRowClass.value,
    'hover:[&>td]:bg-(--cp-default-bg-hover)',
    props.stripe && index % 2 === 1 ? 'bg-(--cp-bg-subtle)' : undefined,
    isRowSelected(row, index) ? 'bg-(--cp-bg-tertiary)' : undefined,
  ]
}

function isLastColumn(index: number) {
  return index === computedColumns.value.length - 1
}

function bodyCellTitle(column: BaseTableColumn<Row>, row: Row) {
  return slots[column.key] ? undefined : cellTitle(column, row)
}

function columnSortKey(column: BaseTableColumn<Row>) {
  return column.sortKey || column.key
}

function columnSortDirection(column: BaseTableColumn<Row>) {
  return props.sort?.key === columnSortKey(column) ? props.sort.direction : undefined
}

function toggleColumnSort(column: BaseTableColumn<Row>) {
  const key = columnSortKey(column)
  const direction = columnSortDirection(column)
  if (!direction) {
    emit('sort-change', { key, direction: 'asc' })
  }
  else if (direction === 'asc') {
    emit('sort-change', { key, direction: 'desc' })
  }
  else {
    emit('sort-change', undefined)
  }
}

function columnAriaSort(column: BaseTableColumn<Row>) {
  const direction = columnSortDirection(column)
  if (direction === 'asc')
    return 'ascending'
  if (direction === 'desc')
    return 'descending'
  return column.sortable ? 'none' : undefined
}

function sortButtonLabel(column: BaseTableColumn<Row>) {
  const direction = columnSortDirection(column)
  if (!direction)
    return `${column.label || column.key}：升序排列`
  if (direction === 'asc')
    return `${column.label || column.key}：降序排列`
  return `${column.label || column.key}：取消排序`
}
</script>

<template>
  <div class="flex h-full min-h-0 w-full max-w-full flex-col overflow-hidden">
    <div
      v-loading="{ loading, preserveContent: hasRows }"
      class="relative flex min-h-0 max-w-full flex-1 overflow-hidden pb-3"
    >
      <div class="flex min-h-0 max-w-full flex-1 flex-col overflow-hidden">
        <div ref="headerWrap" class="max-w-full overflow-hidden">
          <table
            class="w-full shrink-0 table-fixed border-separate text-left"
            :class="compact ? 'border-spacing-y-0.5' : 'border-spacing-y-1'"
            :style="tableViewStyle"
            role="table"
          >
            <colgroup>
              <col
                v-for="column in computedColumns"
                :key="column.key"
                :style="columnStyle(column)"
              >
            </colgroup>

            <thead>
              <tr :class="headerRowClass" role="row">
                <th
                  v-for="(column, index) in computedColumns"
                  :key="column.key"
                  class="bg-(--cp-bg-subtle)"
                  :class="[
                    headerCellClass,
                    alignClass(column),
                    isLastColumn(index) ? 'rounded-r-lg pr-3' : undefined,
                    fixedHeaderClass(column),
                    column.headerClass,
                  ]"
                  role="columnheader"
                  scope="col"
                  :aria-sort="columnAriaSort(column)"
                >
                  <div :class="cellContentClass(column)">
                    <button
                      v-if="column.sortable"
                      type="button"
                      class="inline-flex max-w-full cursor-pointer items-center gap-1 border-0 bg-transparent p-0 text-inherit outline-none transition-colors hover:text-(--cp-text-primary) focus-visible:text-(--cp-info)"
                      :aria-label="sortButtonLabel(column)"
                      :title="sortButtonLabel(column)"
                      @click="toggleColumnSort(column)"
                    >
                      <span class="truncate">
                        <slot :name="`header-${column.key}`" :column="column">
                          {{ column.label }}
                        </slot>
                      </span>
                      <span
                        class="inline-flex shrink-0 -translate-y-px flex-col gap-px"
                        aria-hidden="true"
                      >
                        <Triangle
                          class="size-1.25 fill-current"
                          :class="
                            columnSortDirection(column) === 'asc'
                              ? 'text-(--cp-info)'
                              : 'text-(--cp-text-tertiary)'
                          "
                          :stroke-width="0"
                        />
                        <Triangle
                          class="size-1.25 rotate-180 fill-current"
                          :class="
                            columnSortDirection(column) === 'desc'
                              ? 'text-(--cp-info)'
                              : 'text-(--cp-text-tertiary)'
                          "
                          :stroke-width="0"
                        />
                      </span>
                    </button>
                    <slot v-else :name="`header-${column.key}`" :column="column">
                      {{ column.label }}
                    </slot>
                  </div>
                </th>
              </tr>
            </thead>
          </table>
        </div>

        <BaseScrollbar
          v-if="hasRows"
          ref="bodyScrollbar"
          class="min-h-0 flex-1"
          horizontal
          :max-height="maxHeight"
          track-inset="none"
          @scroll="handleTableScroll"
        >
          <table
            ref="tableView"
            class="w-full table-fixed border-separate text-left"
            :class="compact ? 'border-spacing-y-1' : 'border-spacing-y-2'"
            :style="tableViewStyle"
            role="table"
          >
            <colgroup>
              <col
                v-for="column in computedColumns"
                :key="column.key"
                :style="columnStyle(column)"
              >
            </colgroup>

            <tbody>
              <template v-for="(row, index) in displayRows" :key="getRowKey(row, index)">
                <tr :class="rowClass(row, index)" role="row">
                  <td
                    v-for="(column, columnIndex) in computedColumns"
                    :key="column.key"
                    :class="[
                      bodyCellClass,
                      alignClass(column),
                      isLastColumn(columnIndex) ? 'rounded-r-lg pr-3' : undefined,
                      fixedBodyClass(column, row, index),
                      column.cellClass,
                    ]"
                    role="cell"
                  >
                    <div :class="cellContentClass(column)" :title="bodyCellTitle(column, row)">
                      <slot
                        :name="column.key"
                        :row="row"
                        :value="cellValue(row, column.key)"
                        :display-value="cellDisplayValue(column, row)"
                        :index="index"
                      >
                        {{ cellDisplayValue(column, row) }}
                      </slot>
                    </div>
                  </td>
                </tr>
                <tr v-if="isRowExpanded(row, index)" role="row">
                  <td
                    :colspan="computedColumns.length"
                    class="rounded-(--cp-input-radius-base) bg-(--cp-info-bg) p-0"
                    role="cell"
                  >
                    <slot name="expanded" :row="row" :index="index" />
                  </td>
                </tr>
              </template>
            </tbody>
          </table>
        </BaseScrollbar>

        <div v-else class="grid min-h-0 flex-1 place-items-center overflow-hidden px-4">
          <BaseEmpty v-if="!loading" :title="emptyText" plain class="w-full max-w-80" />
        </div>
      </div>
    </div>

    <BaseTablePagination
      v-if="pagination"
      :pagination="pagination"
      :loading="loading"
      @page-change="emit('page-change', $event)"
      @page-size-change="emit('page-size-change', $event)"
    />
  </div>
</template>
