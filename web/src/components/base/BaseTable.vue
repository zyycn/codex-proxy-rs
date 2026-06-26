<script setup lang="ts">
import { ChevronLeft, ChevronRight } from '@lucide/vue'
import { computed, nextTick, onBeforeUnmount, onMounted, shallowRef, useTemplateRef } from 'vue'

import BaseEmpty from './BaseEmpty.vue'
import BaseScrollbar from './BaseScrollbar.vue'
import BaseSelect from './BaseSelect.vue'

export interface BaseTableColumn {
  key: string
  label?: string
  width?: number | string
  minWidth?: number | string
  maxWidth?: number | string
  flex?: number
  fixed?: 'left'
  align?: 'left' | 'right' | 'center'
  headerClass?: string
  cellClass?: string
}

export interface BaseTablePagination {
  page: number
  pageSize: number
  total: number
  pageSizes?: number[]
}

type TableRow = Record<string, any>
type RowKey = string | ((row: TableRow, index: number) => string | number)
type PagerItem = number | 'ellipsis'

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

const computedColumns = computed(() => {
  const fixedPercentTotal = props.columns.reduce((sum, column) => {
    return column.width === undefined ? sum : sum + numericPercentWidth(column.width)
  }, 0)
  const fixedPixelTotal = props.columns.reduce((sum, column) => {
    return column.width === undefined ? sum : sum + numericPixelWidth(column.width)
  }, 0)
  const minWidthPixels = props.minWidth === undefined ? 0 : numericPixelWidth(props.minWidth)
  const flexibleColumns = props.columns.filter((column) => column.width === undefined)
  const flexTotal = flexibleColumns.reduce((sum, column) => sum + (column.flex ?? 1), 0)
  const available = Math.max(100 - fixedPercentTotal, 0)
  const availablePixels = Math.max(minWidthPixels - fixedPixelTotal, 0)

  return props.columns.map((column) => {
    const flex = column.flex ?? 1
    const width =
      column.width === undefined
        ? minWidthPixels > 0 && fixedPercentTotal === 0
          ? `${availablePixels / Math.max(flexibleColumns.length, 1)}px`
          : flexTotal > 0
            ? `${(available * flex) / flexTotal}%`
            : `${available / Math.max(flexibleColumns.length, 1)}%`
        : normalizeWidth(column.width)

    return {
      ...column,
      resolvedWidth: width,
      resolvedMinWidth: column.minWidth === undefined ? undefined : normalizeWidth(column.minWidth),
      resolvedMaxWidth: column.maxWidth === undefined ? undefined : normalizeWidth(column.maxWidth),
    }
  })
})

const totalPages = computed(() => {
  if (!props.pagination || props.pagination.total <= 0) {
    return 0
  }

  return Math.max(1, Math.ceil(props.pagination.total / props.pagination.pageSize))
})

const currentPage = computed(() => {
  if (!props.pagination || totalPages.value === 0) {
    return 0
  }

  return Math.min(Math.max(props.pagination.page, 1), totalPages.value)
})

const pageSizeOptions = computed(() => {
  const pageSizes = props.pagination?.pageSizes?.length
    ? props.pagination.pageSizes
    : [10, 20, 50, 100]

  return pageSizes.map((pageSize) => ({
    label: `${pageSize} 条/页`,
    value: String(pageSize),
  }))
})

const pageSizeModel = computed({
  get: () => String(props.pagination?.pageSize ?? ''),
  set: (value: string) => {
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

let resizeObserver: ResizeObserver | undefined
let horizontalDragStartX = 0
let horizontalDragStartScrollLeft = 0

const canScrollX = computed(() => horizontalThumbWidth.value > 0)
const horizontalThumbStyle = computed(() => ({
  width: `${horizontalThumbWidth.value}px`,
  transform: `translateX(${horizontalThumbLeft.value}px)`,
}))

const pagerItems = computed<PagerItem[]>(() => {
  const total = totalPages.value
  const current = currentPage.value
  if (total <= 7) {
    return Array.from({ length: total }, (_, index) => index + 1)
  }

  const pages = new Set<number>([1, total, current, current - 1, current + 1])
  if (current <= 3) {
    pages.add(2)
    pages.add(3)
    pages.add(4)
  }
  if (current >= total - 2) {
    pages.add(total - 1)
    pages.add(total - 2)
    pages.add(total - 3)
  }

  const sorted = [...pages].filter((page) => page >= 1 && page <= total).sort((a, b) => a - b)
  const items: PagerItem[] = []
  for (const page of sorted) {
    const previous = items[items.length - 1]
    if (typeof previous === 'number' && page - previous > 1) {
      items.push('ellipsis')
    }
    items.push(page)
  }

  return items
})

function normalizeWidth(width: number | string) {
  return typeof width === 'number' ? `${width}px` : width
}

function numericPercentWidth(width: number | string) {
  if (typeof width === 'number') {
    return 0
  }

  if (width.endsWith('%')) {
    const parsed = Number.parseFloat(width)
    return Number.isFinite(parsed) ? parsed : 0
  }

  return 0
}

function numericPixelWidth(width: number | string) {
  if (typeof width === 'number') {
    return width
  }

  if (width.endsWith('px')) {
    const parsed = Number.parseFloat(width)
    return Number.isFinite(parsed) ? parsed : 0
  }

  return 0
}

function columnStyle(column: (typeof computedColumns.value)[number]) {
  return {
    width: column.resolvedWidth,
    minWidth: column.resolvedMinWidth,
    maxWidth: column.resolvedMaxWidth,
  }
}

function tableStyle() {
  if (props.minWidth === undefined) {
    return undefined
  }

  return { width: `max(100%, ${normalizeWidth(props.minWidth)})` }
}

function fixedHeaderClass(column: BaseTableColumn) {
  if (column.fixed !== 'left') {
    return undefined
  }

  return [
    'sticky left-0 z-30 bg-(--cp-bg-subtle)',
    horizontalScrolled.value ? 'shadow-[8px_0_14px_-14px_rgba(15,23,42,0.38)]' : undefined,
  ]
}

function fixedBodyClass(column: BaseTableColumn, row: TableRow, index: number) {
  if (column.fixed !== 'left') {
    return undefined
  }

  return [
    'sticky left-0 z-20',
    horizontalScrolled.value ? 'shadow-[8px_0_14px_-14px_rgba(15,23,42,0.38)]' : undefined,
    isRowSelected(row, index)
      ? 'bg-(--cp-bg-tertiary)'
      : props.stripe && index % 2 === 1
        ? 'bg-(--cp-bg-subtle)'
        : 'bg-(--cp-bg-surface)',
  ]
}

function cellValue(row: TableRow, key: string) {
  return row[key]
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

function alignClass(column: BaseTableColumn) {
  if (column.align === 'center') {
    return 'text-center'
  }

  if (column.align === 'right') {
    return 'text-right'
  }

  return 'text-left'
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
  return Math.max(wrap.clientWidth - 8, 0)
}

function maxScrollLeft(wrap: HTMLElement) {
  return Math.max(wrap.scrollWidth - wrap.clientWidth, 0)
}

function maxHorizontalThumbLeft(wrap: HTMLElement) {
  return Math.max(horizontalTrackWidth(wrap) - horizontalThumbWidth.value, 0)
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
    return
  }

  horizontalThumbWidth.value = Math.min(
    trackWidth,
    Math.max(trackWidth * (wrap.clientWidth / wrap.scrollWidth), 32),
  )
  horizontalThumbLeft.value = (wrap.scrollLeft / scrollRange) * maxHorizontalThumbLeft(wrap)
  horizontalScrolled.value = wrap.scrollLeft > 0
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
    thumbRange > 0
      ? (Math.max(0, Math.min(nextThumbLeft, thumbRange)) / thumbRange) * scrollRange
      : 0
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
  document.addEventListener('pointermove', handleHorizontalThumbPointerMove)
  document.addEventListener('pointerup', handleHorizontalThumbPointerUp, { once: true })
}

function handleHorizontalThumbPointerMove(event: PointerEvent) {
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
  horizontalDragging.value = false
  document.removeEventListener('pointermove', handleHorizontalThumbPointerMove)
}

onMounted(async () => {
  await nextTick()
  updateHorizontalScrollbar()
  resizeObserver = new ResizeObserver(updateHorizontalScrollbar)
  const wrap = scrollWrap()
  if (wrap) {
    resizeObserver.observe(wrap)
  }
  if (tableViewRef.value) {
    resizeObserver.observe(tableViewRef.value)
  }
})

onBeforeUnmount(() => {
  resizeObserver?.disconnect()
  document.removeEventListener('pointermove', handleHorizontalThumbPointerMove)
})

function isLastColumn(index: number) {
  return index === computedColumns.value.length - 1
}

function cellContentClass(column: BaseTableColumn) {
  if (column.key === 'selection') {
    return [
      'flex min-w-0 items-center overflow-visible leading-none',
      column.align === 'right'
        ? 'justify-end'
        : column.align === 'center'
          ? 'justify-center'
          : 'justify-start',
    ]
  }

  if (column.key === 'actions') {
    return 'min-w-0 overflow-visible'
  }

  return 'min-w-0 truncate'
}

function goToPage(page: number) {
  if (!props.pagination || page < 1 || page > totalPages.value || page === currentPage.value) {
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
    <div class="relative flex min-h-0 max-w-full flex-1 overflow-hidden pb-3">
      <div class="flex min-h-0 max-w-full flex-1 flex-col overflow-hidden">
        <div ref="headerWrap" class="max-w-full overflow-hidden">
          <table
            class="w-full shrink-0 table-fixed border-separate border-spacing-y-2 text-left"
            :style="tableStyle()"
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
            :style="tableStyle()"
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
                    <BaseEmpty :message="emptyText" plain class="w-full max-w-80" />
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
                    <div :class="cellContentClass(column)">
                      <slot
                        :name="column.key"
                        :row="row"
                        :value="cellValue(row, column.key)"
                        :index="index"
                      >
                        {{ cellValue(row, column.key) }}
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
      v-if="pagination && pagination.total > 0"
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
          size="pagination"
          class="w-28"
        />

        <div class="flex items-center gap-2">
          <button
            type="button"
            :class="paginationButtonClass(currentPage <= 1)"
            :disabled="currentPage <= 1"
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
              :disabled="item === currentPage"
              :aria-current="item === currentPage ? 'page' : undefined"
              @click="goToPage(item)"
            >
              {{ item }}
            </button>
          </template>

          <button
            type="button"
            :class="paginationButtonClass(currentPage >= totalPages)"
            :disabled="currentPage >= totalPages"
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
