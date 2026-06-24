export interface ApiEnvelope<T> {
  code: number
  message: string
  requestId: string
  data: T
}
