import { clamp, sumBy } from 'es-toolkit'

export type TableRow = Record<string, unknown>

export interface BaseTableColumn<Row extends TableRow = TableRow> {
  key: string
  label?: string
  sortable?: boolean
  sortKey?: string
  format?: (value: unknown, row: Row) => unknown
  width?: number | string
  minWidth?: number | string
  maxWidth?: number | string
  flex?: number
  fixed?: 'left' | 'right' | false
  align?: 'left' | 'right' | 'center'
  ellipsis?: boolean
  emptyText?: string
  headerClass?: string
  cellClass?: string
}

type BaseTableSortDirection = 'asc' | 'desc'

export interface BaseTableSort {
  key: string
  direction: BaseTableSortDirection
}

export type ResolvedTableColumn<Row extends TableRow = TableRow> = Omit<
  BaseTableColumn<Row>,
  'fixed'
> & {
  fixed?: 'left' | 'right'
  resolvedWidth: string
  resolvedMinWidth?: string
  resolvedMaxWidth?: string
}

export function resolveColumns<Row extends TableRow>(
  columns: BaseTableColumn<Row>[],
  minWidth?: number | string,
): ResolvedTableColumn<Row>[] {
  const actionColumnIndex = columns.findIndex(column => column.key === 'actions')
  const fixedColumnIndex = actionColumnIndex >= 0 ? actionColumnIndex : 0
  const fixedPercentTotal = sumBy(columns, column =>
    column.width === undefined ? 0 : numericPercentWidth(column.width))
  const fixedPixelTotal = sumBy(columns, column =>
    column.width === undefined ? 0 : numericPixelWidth(column.width))
  const minWidthPixels = minWidth === undefined ? 0 : numericPixelWidth(minWidth)
  const flexibleColumns = columns.filter(column => column.width === undefined)
  const flexTotal = sumBy(flexibleColumns, column => column.flex ?? 1)
  const available = clamp(100 - fixedPercentTotal, 0, Number.POSITIVE_INFINITY)
  const availablePixels = clamp(minWidthPixels - fixedPixelTotal, 0, Number.POSITIVE_INFINITY)

  return columns.map((column, index) => {
    const flex = column.flex ?? 1
    const automaticallyFixed
      = index === fixedColumnIndex ? (actionColumnIndex >= 0 ? 'right' : 'left') : undefined
    const width
      = column.width === undefined
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
      fixed: column.fixed === false ? undefined : (column.fixed ?? automaticallyFixed),
      resolvedWidth: width,
      resolvedMinWidth: column.minWidth === undefined ? undefined : normalizeWidth(column.minWidth),
      resolvedMaxWidth: column.maxWidth === undefined ? undefined : normalizeWidth(column.maxWidth),
    }
  })
}

function normalizeWidth(width: number | string) {
  return typeof width === 'number' ? `${width}px` : width
}

export function columnStyle<Row extends TableRow>(column: ResolvedTableColumn<Row>) {
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

export function alignClass<Row extends TableRow>(column: BaseTableColumn<Row>) {
  if (column.align === 'center') {
    return 'text-center'
  }

  if (column.align === 'right') {
    return 'text-right'
  }

  return 'text-left'
}

export function cellValue(row: TableRow, key: string) {
  return row[key]
}

function isEmptyCellValue(value: unknown) {
  return value === undefined || value === null || value === ''
}

export function cellDisplayValue<Row extends TableRow>(column: BaseTableColumn<Row>, row: Row) {
  const rawValue = cellValue(row, column.key)
  const value = column.format ? column.format(rawValue, row) : rawValue
  return isEmptyCellValue(value) ? (column.emptyText ?? '—') : value
}

export function cellTitle<Row extends TableRow>(column: BaseTableColumn<Row>, row: Row) {
  if (column.ellipsis === false || column.key === 'selection' || column.key === 'actions') {
    return undefined
  }

  const value = cellDisplayValue(column, row)
  if (isEmptyCellValue(value)) {
    return undefined
  }

  return typeof value === 'string' || typeof value === 'number' ? String(value) : undefined
}

export function cellContentClass<Row extends TableRow>(column: BaseTableColumn<Row>) {
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
