import type { FormRule } from '@codex-proxy/ui'

export function schedulingWeightRule(message: string): FormRule {
  return {
    validator: (_rule, input: string) => {
      const value = Number(input)
      return input.trim() !== '' && Number.isFinite(value) && value >= 0 && value <= 10 && Math.round(value * 10) / 10 === value
    },
    message,
  }
}
