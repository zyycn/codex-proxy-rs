type PresetVisualTone = 'blue' | 'cyan' | 'green' | 'orange'

const presetVisualTones: readonly PresetVisualTone[] = ['blue', 'green', 'orange', 'cyan']
const presetVisualToneClasses = {
  blue: 'bg-cp-blue-container-strong text-cp-blue-on-container',
  cyan: 'bg-cp-cyan-container-strong text-cp-cyan-on-container',
  green: 'bg-cp-green-container-strong text-cp-green-on-container',
  orange: 'bg-cp-orange-container-strong text-cp-orange-on-container',
} as const satisfies Record<PresetVisualTone, string>

export function stableVisualIndex(value: unknown, length: number): number {
  if (length <= 0)
    return 0

  const text = String(value ?? '')
  let hash = 0
  for (const character of text)
    hash += character.codePointAt(0) ?? 0

  return hash % length
}

/** 为无状态含义的身份与分类生成稳定色调，不把颜色误解为成功或警告。 */
export function stablePresetVisualToneClass(value: unknown): string {
  const tone = presetVisualTones[stableVisualIndex(value, presetVisualTones.length)]!
  return presetVisualToneClasses[tone]
}
