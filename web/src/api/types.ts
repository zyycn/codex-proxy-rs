export interface ApiEnvelope<T> {
  code: string
  message: string
  requestId: string
  data: T
}
