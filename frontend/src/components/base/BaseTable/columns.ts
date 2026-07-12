import { clamp, sumBy } from 'es-toolkit'

export interface BaseTableColumn {
  key: string
  label?: string
  sortable?: boolean
  sortKey?: string
  format?: (value: any, row: Record<string, any>) => any
  width?: number | string
  minWidth?: number | string
  maxWidth?: number | string
  flex?: number
  align?: 'left' | 'right' | 'center'
  ellipsis?: boolean
  emptyText?: string
  headerClass?: string
  cellClass?: string
}

export type BaseTableSortDirection = 'asc' | 'desc'

export interface BaseTableSort {
  key: string
  direction: BaseTableSortDirection
}

export type ResolvedTableColumn = BaseTableColumn & {
  fixed?: 'left' | 'right'
  resolvedWidth: string
  resolvedMinWidth?: string
  resolvedMaxWidth?: string
}

export function resolveColumns(
  columns: BaseTableColumn[],
  minWidth?: number | string,
): ResolvedTableColumn[] {
  const actionColumnIndex = columns.findIndex((column) => column.key === 'actions')
  const fixedColumnIndex = actionColumnIndex >= 0 ? actionColumnIndex : 0
  const fixedPercentTotal = sumBy(columns, (column) =>
    column.width === undefined ? 0 : numericPercentWidth(column.width),
  )
  const fixedPixelTotal = sumBy(columns, (column) =>
    column.width === undefined ? 0 : numericPixelWidth(column.width),
  )
  const minWidthPixels = minWidth === undefined ? 0 : numericPixelWidth(minWidth)
  const flexibleColumns = columns.filter((column) => column.width === undefined)
  const flexTotal = sumBy(flexibleColumns, (column) => column.flex ?? 1)
  const available = clamp(100 - fixedPercentTotal, 0, Number.POSITIVE_INFINITY)
  const availablePixels = clamp(minWidthPixels - fixedPixelTotal, 0, Number.POSITIVE_INFINITY)

  return columns.map((column, index) => {
    const flex = column.flex ?? 1
    const width =
      column.width === undefined
        ? minWidthPixels > 0 && fixedPercentTotal === 0
          ? flexTotal > 0
            ? `${(availablePixels * flex) / flexTotal}px`
            : `${availablePixels / clamp(flexibleColumns.length, 1, Number.POSITIVE_INFINITY)}px`
          : flexTotal > 0
            ? `${(available * flex) / flexTotal}%`
            : `${available / clamp(flexibleColumns.length, 1, Number.POSITIVE_INFINITY)}%`
        : normalizeWidth(column.width)

    return {
      ...column,
      fixed: index === fixedColumnIndex ? (actionColumnIndex >= 0 ? 'right' : 'left') : undefined,
      resolvedWidth: width,
      resolvedMinWidth: column.minWidth === undefined ? undefined : normalizeWidth(column.minWidth),
      resolvedMaxWidth: column.maxWidth === undefined ? undefined : normalizeWidth(column.maxWidth),
    }
  })
}

export function normalizeWidth(width: number | string) {
  return typeof width === 'number' ? `${width}px` : width
}

export function columnStyle(column: ResolvedTableColumn) {
  return {
    width: column.resolvedWidth,
    minWidth: column.resolvedMinWidth,
    maxWidth: column.resolvedMaxWidth,
  }
}

export function tableStyle(minWidth?: number | string) {
  if (minWidth === undefined) {
    return undefined
  }

  return { width: `max(100%, ${normalizeWidth(minWidth)})` }
}

export function alignClass(column: BaseTableColumn) {
  if (column.align === 'center') {
    return 'text-center'
  }

  if (column.align === 'right') {
    return 'text-right'
  }

  return 'text-left'
}

export function cellValue(row: Record<string, any>, key: string) {
  return row[key]
}

export function isEmptyCellValue(value: unknown) {
  return value === undefined || value === null || value === ''
}

export function cellDisplayValue(column: BaseTableColumn, row: Record<string, any>) {
  const rawValue = cellValue(row, column.key)
  const value = column.format ? column.format(rawValue, row) : rawValue
  return isEmptyCellValue(value) ? (column.emptyText ?? '—') : value
}

export function cellTitle(column: BaseTableColumn, row: Record<string, any>) {
  if (column.ellipsis === false || column.key === 'selection' || column.key === 'actions') {
    return undefined
  }

  const value = cellDisplayValue(column, row)
  if (isEmptyCellValue(value)) {
    return undefined
  }

  return typeof value === 'string' || typeof value === 'number' ? String(value) : undefined
}

export function cellContentClass(column: BaseTableColumn) {
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

  return ['min-w-0', column.ellipsis === false ? undefined : 'truncate']
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
