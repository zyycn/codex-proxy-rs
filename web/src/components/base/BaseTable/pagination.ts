import { clamp, range } from 'es-toolkit'

export interface BaseTablePagination {
  page: number
  pageSize: number
  total: number
  pageSizes?: number[]
}

export type PagerItem = number | 'ellipsis'

const DEFAULT_PAGE_SIZES = [10, 20, 50, 100]

export function getTotalPages(pagination?: BaseTablePagination) {
  if (!pagination || pagination.total <= 0) {
    return 0
  }

  return clamp(Math.ceil(pagination.total / pagination.pageSize), 1, Number.POSITIVE_INFINITY)
}

export function getCurrentPage(pagination: BaseTablePagination | undefined, totalPages: number) {
  if (!pagination || totalPages === 0) {
    return 0
  }

  return clamp(pagination.page, 1, totalPages)
}

export function getPageSizeOptions(pagination?: BaseTablePagination) {
  const pageSizes = pagination?.pageSizes?.length ? pagination.pageSizes : DEFAULT_PAGE_SIZES

  return pageSizes.map((pageSize) => ({
    label: `${pageSize} 条/页`,
    value: String(pageSize),
  }))
}

export function getPagerItems(total: number, current: number): PagerItem[] {
  if (total <= 7) {
    return range(1, total + 1)
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
}
