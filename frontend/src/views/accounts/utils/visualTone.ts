export type PresetVisualTone = 'blue' | 'cyan' | 'green' | 'orange'

const presetVisualTones: readonly PresetVisualTone[] = ['blue', 'green', 'orange', 'cyan']
const presetVisualToneClasses = {
  blue: 'bg-cp-blue-bg-strong text-cp-blue-text-on-bg',
  cyan: 'bg-cp-cyan-bg-strong text-cp-cyan-text-on-bg',
  green: 'bg-cp-green-bg-strong text-cp-green-text-on-bg',
  orange: 'bg-cp-orange-bg-strong text-cp-orange-text-on-bg',
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
export function stablePresetVisualTone(value: unknown): PresetVisualTone {
  return presetVisualTones[stableVisualIndex(value, presetVisualTones.length)]!
}

export function stablePresetVisualToneClass(value: unknown): string {
  return presetVisualToneClasses[stablePresetVisualTone(value)]
}
