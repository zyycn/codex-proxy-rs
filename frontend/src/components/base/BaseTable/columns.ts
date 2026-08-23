export type TableRow = object

export type TableColumnKind
  = | 'text'
    | 'identity'
    | 'meta'
    | 'status'
    | 'numeric'
    | 'datetime'
    | 'mono'
    | 'index'
    | 'selection'
    | 'expander'
    | 'actions'
    | 'custom'

export type TableColumnSize = 'xs' | 'sm' | 'md' | 'lg' | 'xl' | '2xl' | '3xl' | '4xl'

export type TableColumnAlign = 'left' | 'right' | 'center'
type TableColumnSticky = 'left' | 'right'

export interface BaseTableColumn<Row extends TableRow = TableRow> {
  key: string
  label?: string
  kind?: TableColumnKind
  size?: TableColumnSize
  align?: TableColumnAlign
  sortable?: boolean | string
  format?: (value: unknown, row: Row) => unknown
  emptyText?: string
}

export interface BaseTableSort {
  key: string
  direction: 'asc' | 'desc'
}

export interface BaseTableProps<Row extends TableRow> {
  columns: BaseTableColumn<Row>[]
  rows: Row[]
  rowKey?: string | ((row: Row, index: number) => string | number)
  selectedRowKeys?: Array<string | number>
  expandedRowKeys?: Array<string | number>
  density?: 'compact' | 'default'
  loading?: boolean
  emptyText?: string
  sort?: BaseTableSort
}

interface ColumnRecipe {
  size: TableColumnSize
  basisWidth?: number
  align: TableColumnAlign
  truncate: boolean
  contentClass?: string
  paddingClass?: string
  sticky?: TableColumnSticky
}

const columnWidths: Record<TableColumnSize, number> = {
  'xs': 64,
  'sm': 88,
  'md': 112,
  'lg': 144,
  'xl': 184,
  '2xl': 240,
  '3xl': 288,
  '4xl': 352,
}

const columnRecipes: Record<TableColumnKind, ColumnRecipe> = {
  text: {
    size: '2xl',
    align: 'left',
    truncate: true,
  },
  identity: {
    size: '2xl',
    align: 'left',
    truncate: true,
  },
  meta: {
    size: 'lg',
    align: 'left',
    truncate: true,
    contentClass: 'text-cp-text-secondary',
  },
  status: {
    size: 'md',
    align: 'center',
    truncate: false,
  },
  numeric: {
    size: 'md',
    align: 'right',
    truncate: false,
    contentClass: 'font-mono tabular-nums text-cp-text-secondary',
  },
  datetime: {
    size: 'xl',
    align: 'left',
    truncate: false,
    contentClass: 'whitespace-nowrap font-mono text-cp-sm tabular-nums text-cp-text-secondary',
  },
  mono: {
    size: 'xl',
    align: 'left',
    truncate: true,
    contentClass: 'font-mono text-cp-sm font-emphasis',
  },
  index: {
    size: 'xs',
    align: 'center',
    truncate: false,
    contentClass: 'font-mono tabular-nums text-cp-text-secondary',
  },
  selection: {
    size: 'xs',
    basisWidth: 48,
    align: 'center',
    truncate: false,
    paddingClass: 'px-2',
    sticky: 'left',
  },
  expander: {
    size: 'xs',
    basisWidth: 40,
    align: 'center',
    truncate: false,
    paddingClass: 'px-2',
    sticky: 'left',
  },
  actions: {
    size: 'md',
    align: 'left',
    truncate: false,
    paddingClass: 'px-3',
    sticky: 'right',
  },
  custom: {
    size: 'lg',
    align: 'left',
    truncate: false,
  },
}

export interface ResolvedTableColumn<Row extends TableRow = TableRow>
  extends BaseTableColumn<Row> {
  kind: TableColumnKind
  basisWidth: number
  align: TableColumnAlign
  truncate: boolean
  contentClass?: string
  paddingClass?: string
  sticky?: TableColumnSticky
  stickyOffset?: number
}

export function defineTableColumns<Row extends TableRow>(columns: BaseTableColumn<Row>[]) {
  return columns
}

export function resolveColumns<Row extends TableRow>(
  columns: BaseTableColumn<Row>[],
): ResolvedTableColumn<Row>[] {
  const resolved = columns.map((column): ResolvedTableColumn<Row> => {
    const kind = column.kind ?? 'text'
    const recipe = columnRecipes[kind]
    const basisWidth = recipe.basisWidth ?? columnWidths[column.size ?? recipe.size]

    return {
      ...column,
      kind,
      basisWidth,
      align: column.align ?? recipe.align,
      truncate: recipe.truncate,
      contentClass: recipe.contentClass,
      paddingClass: recipe.paddingClass,
      sticky: recipe.sticky,
    }
  })

  let leftOffset = 0
  for (const column of resolved) {
    if (column.sticky !== 'left')
      continue
    column.stickyOffset = leftOffset
    leftOffset += column.basisWidth
  }

  let rightOffset = 0
  for (const column of [...resolved].reverse()) {
    if (column.sticky !== 'right')
      continue
    column.stickyOffset = rightOffset
    rightOffset += column.basisWidth
  }

  return resolved
}

export function minimumTableWidth<Row extends TableRow>(columns: ResolvedTableColumn<Row>[]) {
  return columns.reduce((total, column) => total + column.basisWidth, 0)
}

export function tableStyle<Row extends TableRow>(columns: ResolvedTableColumn<Row>[]) {
  return { width: `max(100%, ${minimumTableWidth(columns)}px)` }
}

export function columnStyle<Row extends TableRow>(
  column: ResolvedTableColumn<Row>,
  columns: ResolvedTableColumn<Row>[],
) {
  const tableWidth = minimumTableWidth(columns)
  const widthPercent = tableWidth > 0 ? (column.basisWidth / tableWidth) * 100 : 0

  return {
    width: `${widthPercent}%`,
    minWidth: `${column.basisWidth}px`,
  }
}

export function stickyStyle<Row extends TableRow>(column: ResolvedTableColumn<Row>) {
  if (!column.sticky)
    return undefined
  return { [column.sticky]: `${column.stickyOffset ?? 0}px` }
}

export function alignClass<Row extends TableRow>(column: ResolvedTableColumn<Row>) {
  if (column.align === 'center')
    return 'text-center'
  if (column.align === 'right')
    return 'text-right'
  return 'text-left'
}

export function cellValue(row: TableRow, key: string) {
  return (row as Record<string, unknown>)[key]
}

function isEmptyCellValue(value: unknown) {
  return value === undefined || value === null || value === ''
}

export function cellDisplayValue<Row extends TableRow>(column: BaseTableColumn<Row>, row: Row) {
  const rawValue = cellValue(row, column.key)
  const value = column.format ? column.format(rawValue, row) : rawValue
  return isEmptyCellValue(value) ? (column.emptyText ?? '—') : value
}

export function cellTitle<Row extends TableRow>(column: ResolvedTableColumn<Row>, row: Row) {
  if (!column.truncate)
    return undefined
  const value = cellDisplayValue(column, row)
  return typeof value === 'string' || typeof value === 'number' ? String(value) : undefined
}

export function cellContentClass<Row extends TableRow>(column: ResolvedTableColumn<Row>) {
  if (column.kind === 'selection' || column.kind === 'expander') {
    return [
      'flex min-w-0 items-center overflow-visible leading-none',
      column.align === 'right'
        ? 'justify-end'
        : column.align === 'center'
          ? 'justify-center'
          : 'justify-start',
    ]
  }
  if (column.kind === 'actions')
    return 'min-w-0 overflow-visible'
  return ['min-w-0', column.truncate ? 'truncate' : undefined]
}

export function columnSortKey<Row extends TableRow>(column: BaseTableColumn<Row>) {
  return typeof column.sortable === 'string' ? column.sortable : column.key
}
