<script setup lang="ts">
import { useEventListener, useResizeObserver } from '@vueuse/core'
import { ChevronLeft, ChevronRight } from '@lucide/vue'
import { clamp } from 'es-toolkit'
import { computed, nextTick, onMounted, shallowRef, useTemplateRef } from 'vue'

import BaseEmpty from '../BaseEmpty.vue'
import BaseScrollbar from '../BaseScrollbar.vue'
import BaseSelect from '../BaseSelect.vue'
import {
  alignClass,
  cellContentClass,
  cellDisplayValue,
  cellTitle,
  cellValue,
  columnStyle,
  resolveColumns,
  tableStyle as resolveTableStyle,
  type BaseTableColumn,
} from './columns'
import {
  getCurrentPage,
  getPageSizeOptions,
  getPagerItems,
  getTotalPages,
  type BaseTablePagination,
} from './pagination'

type TableRow = Record<string, any>
type RowKey = string | ((row: TableRow, index: number) => string | number)

const props = withDefaults(
  defineProps<{
    columns: BaseTableColumn[]
    rows: any[]
    rowKey?: RowKey
    selectedRowKeys?: Array<string | number>
    stripe?: boolean
    loading?: boolean
    emptyText?: string
    maxHeight?: string
    minWidth?: number | string
    pagination?: BaseTablePagination
    expandedRowKeys?: Array<string | number>
  }>(),
  {
    rowKey: 'id',
    selectedRowKeys: () => [],
    expandedRowKeys: () => [],
    stripe: true,
    loading: false,
    emptyText: '暂无数据',
    maxHeight: undefined,
    minWidth: undefined,
  },
)

const emit = defineEmits<{
  'page-change': [page: number]
  'page-size-change': [pageSize: number]
}>()

const headerRowClass = 'h-10 text-[11px] font-bold text-(--cp-text-muted)'
const bodyRowClass = 'h-13'
const headerCellClass =
  'min-w-0 bg-(--cp-bg-subtle) px-4 first:pl-3 shadow-[0_10px_16px_-18px_#0e172638]'
const bodyCellClass =
  'min-w-0 px-4 first:pl-3 text-[13px] text-(--cp-text-primary) first:rounded-l-lg'

const computedColumns = computed(() => resolveColumns(props.columns, props.minWidth))
const totalPages = computed(() => getTotalPages(props.pagination))
const currentPage = computed(() => getCurrentPage(props.pagination, totalPages.value))
const pageSizeOptions = computed(() => getPageSizeOptions(props.pagination))
const tableViewStyle = computed(() => resolveTableStyle(props.minWidth))

const pageSizeModel = computed({
  get: () => String(props.pagination?.pageSize ?? ''),
  set: (value: string) => {
    if (props.loading) {
      return
    }

    const pageSize = Number(value)
    if (Number.isFinite(pageSize) && pageSize > 0) {
      emit('page-size-change', pageSize)
    }
  },
})

const headerWrapRef = useTemplateRef<HTMLDivElement>('headerWrap')
const bodyScrollbarRef = useTemplateRef<InstanceType<typeof BaseScrollbar>>('bodyScrollbar')
const tableViewRef = useTemplateRef<HTMLTableElement>('tableView')
const horizontalThumbWidth = shallowRef(0)
const horizontalThumbLeft = shallowRef(0)
const horizontalHovering = shallowRef(false)
const horizontalDragging = shallowRef(false)
const horizontalScrolled = shallowRef(false)
const horizontalCanScrollRight = shallowRef(false)

let horizontalDragStartX = 0
let horizontalDragStartScrollLeft = 0

const canScrollX = computed(() => horizontalThumbWidth.value > 0)
const horizontalThumbStyle = computed(() => ({
  width: `${horizontalThumbWidth.value}px`,
  transform: `translateX(${horizontalThumbLeft.value}px)`,
}))

const pagerItems = computed(() => getPagerItems(totalPages.value, currentPage.value))

function fixedHeaderClass(column: BaseTableColumn) {
  if (!column.fixed) {
    return undefined
  }

  const showShadow =
    column.fixed === 'left' ? horizontalScrolled.value : horizontalCanScrollRight.value

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

function fixedBodyClass(column: BaseTableColumn, row: TableRow, index: number) {
  if (!column.fixed) {
    return undefined
  }

  const showShadow =
    column.fixed === 'left' ? horizontalScrolled.value : horizontalCanScrollRight.value

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

function getRowKey(row: TableRow, index: number) {
  if (typeof props.rowKey === 'function') {
    return props.rowKey(row, index)
  }

  const value = row[props.rowKey]
  return typeof value === 'string' || typeof value === 'number' ? value : index
}

function isRowSelected(row: TableRow, index: number) {
  return props.selectedRowKeys.includes(getRowKey(row, index))
}

function isRowExpanded(row: TableRow, index: number) {
  return props.expandedRowKeys.includes(getRowKey(row, index))
}

function rowClass(row: TableRow, index: number) {
  return [
    bodyRowClass,
    'hover:[&>td]:bg-(--cp-default-bg-hover)',
    props.stripe && index % 2 === 1 ? 'bg-(--cp-bg-subtle)' : undefined,
    isRowSelected(row, index) ? 'bg-(--cp-bg-tertiary)' : undefined,
  ]
}

function horizontalTrackWidth(wrap: HTMLElement) {
  return clamp(wrap.clientWidth - 8, 0, Number.POSITIVE_INFINITY)
}

function maxScrollLeft(wrap: HTMLElement) {
  return clamp(wrap.scrollWidth - wrap.clientWidth, 0, Number.POSITIVE_INFINITY)
}

function maxHorizontalThumbLeft(wrap: HTMLElement) {
  return clamp(horizontalTrackWidth(wrap) - horizontalThumbWidth.value, 0, Number.POSITIVE_INFINITY)
}

function scrollWrap() {
  return bodyScrollbarRef.value?.wrapRef ?? null
}

function updateHorizontalScrollbar() {
  const wrap = scrollWrap()
  if (!wrap) {
    return
  }

  const scrollRange = maxScrollLeft(wrap)
  const trackWidth = horizontalTrackWidth(wrap)
  if (scrollRange <= 0 || trackWidth <= 0) {
    horizontalThumbWidth.value = 0
    horizontalThumbLeft.value = 0
    horizontalScrolled.value = false
    horizontalCanScrollRight.value = false
    return
  }

  horizontalThumbWidth.value = clamp(
    trackWidth * (wrap.clientWidth / wrap.scrollWidth),
    32,
    trackWidth,
  )
  horizontalThumbLeft.value = (wrap.scrollLeft / scrollRange) * maxHorizontalThumbLeft(wrap)
  horizontalScrolled.value = wrap.scrollLeft > 0
  horizontalCanScrollRight.value = wrap.scrollLeft < scrollRange - 1
}

function handleTableScroll() {
  const wrap = scrollWrap()
  if (wrap && headerWrapRef.value) {
    headerWrapRef.value.scrollLeft = wrap.scrollLeft
  }
  updateHorizontalScrollbar()
}

function handleHorizontalTrackPointerDown(event: PointerEvent) {
  if (event.target !== event.currentTarget) {
    return
  }

  const wrap = scrollWrap()
  if (!wrap) {
    return
  }

  const rect = (event.currentTarget as HTMLElement).getBoundingClientRect()
  const nextThumbLeft = event.clientX - rect.left - horizontalThumbWidth.value / 2
  const scrollRange = maxScrollLeft(wrap)
  const thumbRange = maxHorizontalThumbLeft(wrap)
  wrap.scrollLeft =
    thumbRange > 0 ? (clamp(nextThumbLeft, 0, thumbRange) / thumbRange) * scrollRange : 0
}

function handleHorizontalThumbPointerDown(event: PointerEvent) {
  const wrap = scrollWrap()
  if (!wrap) {
    return
  }

  event.preventDefault()
  horizontalDragging.value = true
  horizontalDragStartX = event.clientX
  horizontalDragStartScrollLeft = wrap.scrollLeft
}

function handleHorizontalThumbPointerMove(event: PointerEvent) {
  if (!horizontalDragging.value) {
    return
  }

  const wrap = scrollWrap()
  if (!wrap) {
    return
  }

  const thumbRange = maxHorizontalThumbLeft(wrap)
  if (thumbRange <= 0) {
    return
  }

  const scrollRange = maxScrollLeft(wrap)
  wrap.scrollLeft =
    horizontalDragStartScrollLeft +
    ((event.clientX - horizontalDragStartX) / thumbRange) * scrollRange
}

function handleHorizontalThumbPointerUp() {
  if (!horizontalDragging.value) {
    return
  }

  horizontalDragging.value = false
}

onMounted(async () => {
  await nextTick()
  updateHorizontalScrollbar()
})

useResizeObserver(
  () =>
    [scrollWrap(), tableViewRef.value].filter(
      (element): element is HTMLDivElement | HTMLTableElement => Boolean(element),
    ),
  updateHorizontalScrollbar,
)
useEventListener(document, 'pointermove', handleHorizontalThumbPointerMove)
useEventListener(document, 'pointerup', handleHorizontalThumbPointerUp)

function isLastColumn(index: number) {
  return index === computedColumns.value.length - 1
}

function goToPage(page: number) {
  if (
    props.loading ||
    !props.pagination ||
    page < 1 ||
    page > totalPages.value ||
    page === currentPage.value
  ) {
    return
  }

  emit('page-change', page)
}

function paginationButtonClass(disabled: boolean) {
  return [
    'inline-flex size-8 items-center justify-center rounded-(--cp-input-radius-base) border-0 bg-(--cp-bg-subtle) text-(--cp-text-secondary) transition-colors duration-150 outline-none',
    disabled
      ? 'cursor-not-allowed opacity-45 shadow-none'
      : 'cursor-pointer hover:bg-(--cp-default-bg-hover) hover:text-(--cp-text-primary) focus-visible:ring-2 focus-visible:ring-(--cp-info-border) focus-visible:ring-offset-2 focus-visible:ring-offset-(--cp-bg-surface)',
  ]
}

function paginationPageClass(page: number) {
  return [
    'inline-flex size-8 items-center justify-center rounded-(--cp-input-radius-base) border-0 text-xs font-[720] leading-none transition-colors duration-150 outline-none',
    page === currentPage.value
      ? 'cursor-default bg-(--cp-info) text-(--cp-info-on)'
      : 'cursor-pointer bg-(--cp-bg-subtle) text-(--cp-text-primary) hover:bg-(--cp-default-bg-hover) focus-visible:ring-2 focus-visible:ring-(--cp-info-border) focus-visible:ring-offset-2 focus-visible:ring-offset-(--cp-bg-surface)',
  ]
}
</script>

<template>
  <div
    class="flex h-full min-h-0 w-full max-w-full flex-col overflow-hidden"
    @mouseenter="horizontalHovering = true"
    @mouseleave="horizontalHovering = false"
  >
    <div v-loading="loading" class="relative flex min-h-0 max-w-full flex-1 overflow-hidden pb-3">
      <div class="flex min-h-0 max-w-full flex-1 flex-col overflow-hidden">
        <div ref="headerWrap" class="max-w-full overflow-hidden">
          <table
            class="w-full shrink-0 table-fixed border-separate border-spacing-y-1 text-left"
            :style="tableViewStyle"
            role="table"
          >
            <colgroup>
              <col
                v-for="column in computedColumns"
                :key="column.key"
                :style="columnStyle(column)"
              />
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
                >
                  <div :class="cellContentClass(column)">
                    <slot :name="`header-${column.key}`" :column="column">
                      {{ column.label }}
                    </slot>
                  </div>
                </th>
              </tr>
            </thead>
          </table>
        </div>

        <BaseScrollbar
          ref="bodyScrollbar"
          class="min-h-0 flex-1"
          :force-visible="horizontalHovering"
          :max-height="maxHeight"
          @scroll="handleTableScroll"
        >
          <table
            ref="tableView"
            class="w-full table-fixed border-separate border-spacing-y-2 text-left"
            :style="tableViewStyle"
            role="table"
          >
            <colgroup>
              <col
                v-for="column in computedColumns"
                :key="column.key"
                :style="columnStyle(column)"
              />
            </colgroup>

            <tbody>
              <tr v-if="!loading && rows.length === 0" role="row">
                <td :colspan="computedColumns.length" class="h-72 p-0" role="cell">
                  <div class="grid h-full min-h-72 place-items-center">
                    <BaseEmpty :title="emptyText" plain class="w-full max-w-80" />
                  </div>
                </td>
              </tr>

              <template v-for="(row, index) in rows" :key="getRowKey(row, index)">
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
                    <div :class="cellContentClass(column)" :title="cellTitle(column, row)">
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
      </div>

      <div
        v-show="canScrollX"
        class="absolute right-1 bottom-1 left-1 z-30 h-1.5 rounded-full transition-opacity duration-200"
        :class="horizontalHovering || horizontalDragging ? 'opacity-100' : 'opacity-0'"
        @pointerdown="handleHorizontalTrackPointerDown"
      >
        <div
          class="h-full rounded-full bg-(--cp-scrollbar-thumb) transition-colors duration-200 hover:bg-(--cp-scrollbar-thumb-hover)"
          :class="horizontalDragging ? 'bg-(--cp-scrollbar-thumb-hover)' : ''"
          :style="horizontalThumbStyle"
          @pointerdown="handleHorizontalThumbPointerDown"
        />
      </div>
    </div>

    <footer
      v-if="pagination"
      class="mt-2 flex min-h-10 shrink-0 flex-wrap items-center justify-between gap-3 px-0 py-1"
    >
      <div
        class="flex min-w-0 items-center gap-2.5 text-[12px] font-[650] text-(--cp-text-secondary)"
      >
        <span class="whitespace-nowrap">共 {{ pagination.total }} 条</span>
      </div>

      <div class="flex items-center gap-2">
        <BaseSelect
          v-model="pageSizeModel"
          :options="pageSizeOptions"
          :disabled="loading"
          size="compact"
          class="w-28"
        />

        <div class="flex items-center gap-2">
          <button
            type="button"
            :class="paginationButtonClass(loading || currentPage <= 1)"
            :disabled="loading || currentPage <= 1"
            title="上一页"
            aria-label="上一页"
            @click="goToPage(currentPage - 1)"
          >
            <ChevronLeft class="size-4" />
          </button>

          <template v-for="(item, index) in pagerItems" :key="`${item}-${index}`">
            <span
              v-if="item === 'ellipsis'"
              class="inline-flex size-8 items-center justify-center text-xs font-[720] text-(--cp-text-muted)"
            >
              ...
            </span>
            <button
              v-else
              type="button"
              :class="paginationPageClass(item)"
              :disabled="loading || item === currentPage"
              :aria-current="item === currentPage ? 'page' : undefined"
              @click="goToPage(item)"
            >
              {{ item }}
            </button>
          </template>

          <button
            type="button"
            :class="paginationButtonClass(loading || currentPage >= totalPages)"
            :disabled="loading || currentPage >= totalPages"
            title="下一页"
            aria-label="下一页"
            @click="goToPage(currentPage + 1)"
          >
            <ChevronRight class="size-4" />
          </button>
        </div>
      </div>
    </footer>
  </div>
</template>
