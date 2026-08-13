export interface AccountSchedulingValues {
  concurrencyLimit: number | null
  weight: number
}

const MAX_ACCOUNT_CONCURRENCY = 4_294_967_295

type AccountSchedulingParseResult
  = | { valid: true, values: AccountSchedulingValues }
    | { valid: false, message: string }

export function parseAccountSchedulingForm(
  concurrencyLimit: string,
  weight: string,
): AccountSchedulingParseResult {
  const limitText = concurrencyLimit.trim()
  const parsedLimit = limitText === '' ? null : Number(limitText)
  if (
    parsedLimit !== null
    && (!Number.isSafeInteger(parsedLimit)
      || parsedLimit < 1
      || parsedLimit > MAX_ACCOUNT_CONCURRENCY)
  ) {
    return { valid: false, message: `并发限制必须留空或填写 1 到 ${MAX_ACCOUNT_CONCURRENCY} 的整数` }
  }

  const parsedWeight = Number(weight.trim())
  if (!Number.isSafeInteger(parsedWeight) || parsedWeight < 1 || parsedWeight > 100) {
    return { valid: false, message: '权重必须是 1 到 100 的整数' }
  }

  return {
    valid: true,
    values: {
      concurrencyLimit: parsedLimit,
      weight: parsedWeight,
    },
  }
}

export function concurrencyLimitInput(value: number | null) {
  return value === null ? '' : String(value)
}
