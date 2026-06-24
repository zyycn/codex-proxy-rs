export interface ApiEnvelope<T> {
  code: number
  message: string
  data: T
}

export interface ApiPageMeta {
  limit?: number
  page?: number
  pageSize?: number
  total?: number
  totalPages?: number
}

export interface ApiPageData<T> {
  items: T[]
  page: ApiPageMeta
}

export type ApiPageEnvelope<T> = ApiEnvelope<ApiPageData<T>>

export interface PaginatedResult<T> {
  items: T[]
  page: ApiPageMeta
}
