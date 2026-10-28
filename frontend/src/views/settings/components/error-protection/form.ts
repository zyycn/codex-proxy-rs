import type { AccountErrorProtection, ProtectionCondition, ProtectionField, ProtectionOperator, ProtectionRule } from '@/api/modules/settings/protection'
import { computed } from 'vue'
import { t } from '@/i18n'

export type ProtectionDraft = Omit<AccountErrorProtection, 'threshold' | 'windowSeconds' | 'durationSeconds' | 'probeModel'> & {
  threshold: number | null
  windowSeconds: number | null
  durationSeconds: number | null
  probeModel: string
}

export const protectionLimits = {
  rules: 32,
  conditions: 8,
  values: 16,
  name: 128,
  textBytes: 64 * 1024,
}

export const protectionFieldLabels = computed<Record<ProtectionField, string>>(() => ({
  provider: t('settings.errorProtection.fields.provider'),
  status: t('settings.errorProtection.fields.status'),
  code: t('settings.errorProtection.fields.code'),
  type: t('settings.errorProtection.fields.type'),
  message: t('settings.errorProtection.fields.message'),
}))

export const protectionOperatorLabels = computed<Record<ProtectionOperator, string>>(() => ({
  equals: t('settings.errorProtection.operators.equals'),
  contains: t('settings.errorProtection.operators.contains'),
}))

function exceedsCharacters(value: string, max: number): boolean {
  let length = 0
  for (const _ of value) {
    if (++length > max)
      return true
  }
  return false
}

// 与服务端 char::is_whitespace 使用同一 Unicode 空白集合
export function trimProtectionText(value: string): string {
  return value.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, '')
}

export function createProtectionCondition(): ProtectionCondition {
  return {
    field: 'code',
    operator: 'equals',
    values: [''],
    ignoreCase: false,
  }
}

export function protectionConditionError(condition: ProtectionCondition): string {
  if (!condition.values.length || condition.values.length > protectionLimits.values)
    return t('settings.errorProtection.validation.values', { max: protectionLimits.values })
  if (condition.operator === 'contains' && (condition.field !== 'message' || condition.values.some(value => value === null || !trimProtectionText(value))))
    return t('settings.errorProtection.validation.contains')
  const max = condition.field === 'message'
    ? 1024
    : 256
  if (condition.values.some(value => value !== null && exceedsCharacters(value, max)))
    return t('settings.errorProtection.validation.valueLength', { max })
  if (condition.field === 'status' && condition.values.some(value => value !== null && !/^[1-5]\d{2}$/.test(value)))
    return t('settings.errorProtection.validation.status')
  return ''
}

export function protectionRuleStructureError(rule: ProtectionRule): string {
  if (!trimProtectionText(rule.name) || rule.name !== trimProtectionText(rule.name) || /\p{Cc}/u.test(rule.name) || exceedsCharacters(rule.name, protectionLimits.name))
    return t('settings.errorProtection.validation.name', { max: protectionLimits.name })
  if (!rule.conditions.length || rule.conditions.length > protectionLimits.conditions)
    return t('settings.errorProtection.validation.conditions', { max: protectionLimits.conditions })
  if (rule.conditions.every(condition => condition.field === 'provider'))
    return t('settings.errorProtection.validation.errorCondition')
  return ''
}

export function protectionRuleError(rule: ProtectionRule): string {
  const error = protectionRuleStructureError(rule)
  if (error)
    return error
  for (const condition of rule.conditions) {
    const conditionError = protectionConditionError(condition)
    if (conditionError)
      return conditionError
  }
  return ''
}

export function protectionRulesError(policy: Pick<AccountErrorProtection, 'enabled' | 'rules'>): string {
  if (policy.enabled && !policy.rules.length)
    return t('settings.errorProtection.validation.required')
  if (policy.rules.length > protectionLimits.rules)
    return t('settings.errorProtection.validation.rules', { max: protectionLimits.rules })
  for (const [index, rule] of policy.rules.entries()) {
    const error = protectionRuleError(rule)
    if (error)
      return t('settings.errorProtection.validation.rule', { name: trimProtectionText(rule.name) || String(index + 1), error })
  }
  return ''
}

export function protectionSettingsError(policy: ProtectionDraft): string {
  const { threshold, windowSeconds, durationSeconds } = policy
  if (threshold === null || windowSeconds === null || durationSeconds === null)
    return t('settings.form.protectionSettingsRequired')
  if (!Number.isInteger(threshold) || threshold < 2 || threshold > 1000
    || !Number.isInteger(windowSeconds) || windowSeconds < 60 || windowSeconds > 3600
    || !Number.isInteger(durationSeconds) || durationSeconds < 300 || durationSeconds > 604800) {
    return t('settings.form.invalidProtectionSettings')
  }
  if (exceedsCharacters(trimProtectionText(policy.probeModel), 128) || /\p{Cc}/u.test(policy.probeModel))
    return t('settings.form.invalidProbeModel')
  const rulesError = protectionRulesError(policy)
  if (rulesError)
    return rulesError
  const encoder = new TextEncoder()
  let bytes = encoder.encode(trimProtectionText(policy.probeModel)).byteLength
  for (const rule of policy.rules) {
    bytes += encoder.encode(rule.id).byteLength + encoder.encode(rule.name).byteLength
    for (const condition of rule.conditions) {
      for (const value of condition.values) {
        if (value !== null)
          bytes += encoder.encode(value).byteLength
        if (bytes > protectionLimits.textBytes)
          return t('settings.errorProtection.validation.textBudget')
      }
    }
  }
  return ''
}

export function protectionRuleSummary(rule: ProtectionRule): string {
  return rule.conditions.map((condition) => {
    const candidates = condition.values.slice(0, 2).map(value => value === null
      ? t('settings.errorProtection.missing')
      : trimProtectionText(value) === ''
        ? t('settings.errorProtection.emptyText')
        : `“${value.length > 60
          ? `${Array.from(value.slice(0, 120)).slice(0, 60).join('')}…`
          : value}”`).join(t('settings.errorProtection.orSeparator'))
    const values = condition.values.length > 2
      ? t('settings.errorProtection.moreValues', { values: candidates, count: condition.values.length - 2 })
      : candidates
    const summary = t('settings.errorProtection.conditionSummary', {
      field: protectionFieldLabels.value[condition.field],
      operator: protectionOperatorLabels.value[condition.operator],
      values: condition.values.length > 1
        ? `(${values})`
        : values,
    })
    return condition.ignoreCase
      ? t('settings.errorProtection.ignoreCaseSummary', { summary })
      : summary
  }).join(t('settings.errorProtection.andSeparator'))
}
