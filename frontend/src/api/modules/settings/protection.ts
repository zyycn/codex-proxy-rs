export type ProtectionField = 'provider' | 'status' | 'code' | 'type' | 'message'
export type ProtectionOperator = 'equals' | 'contains'

export interface ProtectionCondition {
  field: ProtectionField
  operator: ProtectionOperator
  values: (string | null)[]
  ignoreCase: boolean
}

export interface ProtectionRule {
  id: string
  name: string
  conditions: ProtectionCondition[]
}

export interface AccountErrorProtection {
  enabled: boolean
  rules: ProtectionRule[]
  threshold: number
  windowSeconds: number
  durationSeconds: number
  probeEnabled: boolean
  probeModel: string | null
}
