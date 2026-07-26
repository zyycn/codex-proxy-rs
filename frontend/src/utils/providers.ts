// 平台标识到展示名的唯一映射；未知平台返回 undefined，由调用方决定兜底文案。
export function providerDisplayName(value?: string | null): 'OpenAI' | 'xAI' | undefined {
  if (value === 'openai')
    return 'OpenAI'
  if (value === 'xai')
    return 'xAI'
  return undefined
}
