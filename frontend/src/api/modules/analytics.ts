export interface AnalyticsMetadata {
  coverageStart: string | null
  coverageStartDisplay: string | null
  coverageEnd: string | null
  coverageEndDisplay: string | null
  generatedAt: string
  generatedAtDisplay: string
  lagSeconds: number
  state: 'ready' | 'building' | 'lagging' | 'stale'
  percentilesApproximate: boolean
  schemaVersion: number
  histogramVersion: number
}
