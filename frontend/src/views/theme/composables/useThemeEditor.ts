import type {
  ThemeColorId,
  ThemeColorPresetId,
  ThemeComponentOverrides,
  ThemeCustomization,
  ThemeMode,
  ThemeName,
  ThemeSeedOverrides,
  ThemeTokenName,
} from '@/theme'

import { storeToRefs } from 'pinia'
import { computed, ref, shallowRef, watch } from 'vue'

import { useThemeStore } from '@/stores/modules/theme'
import {
  DEFAULT_CUSTOM_THEME_COLOR,
  DEFAULT_THEME_COLOR,
  DEFAULT_THEME_MODE,
  isThemeColorId,
  isThemeMode,
  normalizeThemeCustomization,
  resolveTheme,
} from '@/theme'
import { normalizeHexColor } from '@/utils/color'

export type ThemeEditorScope = 'global' | 'component'
export type ThemeEditorGlobalCategory = 'color' | 'size' | 'style'
export type ThemeEditorPreview = 'page' | 'components'
export type ThemeEditorComponent
  = 'action'
    | 'form'
    | 'selection'
    | 'surface'
    | 'data'
    | 'feedback'
    | 'navigation'
    | 'layout'

export interface ThemeEditorDraft {
  mode: ThemeMode
  color: ThemeColorId
  customColor: string
  customization: ThemeCustomization
}

type NumericSeedKey = 'fontSize' | 'sizeUnit' | 'sizeStep' | 'controlHeight' | 'borderRadius' | 'shadowStrength'
type ComponentNumberKey = keyof ThemeComponentOverrides
type ColorSeedKey = Exclude<keyof ThemeSeedOverrides, NumericSeedKey>

export function useThemeEditor() {
  const themeStore = useThemeStore()
  const {
    themeMode,
    themeColor,
    customThemeColor,
    themeCustomization,
    effectiveTheme,
  } = storeToRefs(themeStore)

  const saved = ref(createStoreDraft())
  const draft = ref(cloneDraft(saved.value))
  const scope = shallowRef<ThemeEditorScope>('global')
  const globalCategory = shallowRef<ThemeEditorGlobalCategory>('color')
  const preview = shallowRef<ThemeEditorPreview>('page')
  const previewTheme = shallowRef<ThemeName>(effectiveTheme.value)
  const component = shallowRef<ThemeEditorComponent>('action')
  const query = shallowRef('')

  const resolvedPreview = computed(() => resolveTheme(
    previewTheme.value,
    draft.value.color,
    draft.value.customColor,
    draft.value.customization,
  ))
  const previewStyle = computed<Record<string, string>>(() => ({
    ...resolvedPreview.value.tokens,
    colorScheme: previewTheme.value,
  }))
  const modificationCount = computed(() => countDraftChanges(saved.value, draft.value))
  const dirty = computed(() => modificationCount.value > 0)

  watch(
    [themeMode, themeColor, customThemeColor, themeCustomization],
    () => {
      const hadUnsavedChanges = dirty.value
      saved.value = createStoreDraft()
      if (!hadUnsavedChanges) {
        draft.value = cloneDraft(saved.value)
        previewTheme.value = resolvedDraftTheme(draft.value)
      }
    },
    { deep: true },
  )

  watch(effectiveTheme, () => {
    if (draft.value.mode === 'system')
      previewTheme.value = effectiveTheme.value
  })

  function createStoreDraft(): ThemeEditorDraft {
    return {
      mode: themeMode.value,
      color: themeColor.value,
      customColor: customThemeColor.value,
      customization: normalizeThemeCustomization(themeCustomization.value),
    }
  }

  function selectPreset(color: ThemeColorPresetId) {
    draft.value = {
      ...draft.value,
      color,
    }
  }

  function setPrimaryColor(color: string) {
    const normalized = normalizeHexColor(color)
    if (!normalized)
      return
    draft.value = {
      ...draft.value,
      color: 'custom',
      customColor: normalized,
    }
  }

  function setMode(mode: ThemeMode) {
    draft.value = { ...draft.value, mode }
    previewTheme.value = mode === 'system' ? effectiveTheme.value : mode
  }

  function setSeedColor(key: ColorSeedKey, color: string) {
    const normalized = normalizeHexColor(color)
    if (!normalized)
      return
    updateSeed({ [key]: normalized })
  }

  function setSeedNumber(key: NumericSeedKey, value: number) {
    if (!Number.isFinite(value))
      return
    updateSeed({ [key]: value })
  }

  function resetSeed(key: keyof ThemeSeedOverrides) {
    const nextSeed = { ...(draft.value.customization.seed ?? {}) }
    delete nextSeed[key]
    setCustomization({
      ...draft.value.customization,
      seed: Object.keys(nextSeed).length > 0 ? nextSeed : undefined,
    })
  }

  function setComponentNumber(key: ComponentNumberKey, value: number) {
    if (!Number.isFinite(value))
      return
    setCustomization(normalizeThemeCustomization({
      ...draft.value.customization,
      component: {
        ...(draft.value.customization.component ?? {}),
        [key]: value,
      },
    }))
  }

  function resetComponentNumber(key: ComponentNumberKey) {
    const component = { ...(draft.value.customization.component ?? {}) }
    delete component[key]
    setCustomization({
      ...draft.value.customization,
      component: Object.keys(component).length > 0 ? component : undefined,
    })
  }

  function setTokenOverride(name: ThemeTokenName, value: string) {
    setCustomization(normalizeThemeCustomization({
      ...draft.value.customization,
      tokenOverrides: {
        ...(draft.value.customization.tokenOverrides ?? {}),
        [name]: value,
      },
    }))
  }

  function resetTokenOverride(name: ThemeTokenName) {
    const tokenOverrides = { ...(draft.value.customization.tokenOverrides ?? {}) }
    delete tokenOverrides[name]
    setCustomization({
      ...draft.value.customization,
      tokenOverrides: Object.keys(tokenOverrides).length > 0 ? tokenOverrides : undefined,
    })
  }

  function resetDraft() {
    draft.value = cloneDraft(saved.value)
    previewTheme.value = resolvedDraftTheme(draft.value)
  }

  function restoreDefaults() {
    draft.value = {
      mode: DEFAULT_THEME_MODE,
      color: DEFAULT_THEME_COLOR,
      customColor: DEFAULT_CUSTOM_THEME_COLOR,
      customization: {},
    }
    previewTheme.value = effectiveTheme.value
  }

  function save(event?: MouseEvent) {
    const normalizedDraft = normalizeDraft(draft.value)
    if (!normalizedDraft)
      return false
    const savedSuccessfully = themeStore.setThemeConfiguration(normalizedDraft, event)
    if (!savedSuccessfully)
      return false
    saved.value = cloneDraft(normalizedDraft)
    draft.value = cloneDraft(normalizedDraft)
    return true
  }

  function updateSeed(patch: Partial<ThemeSeedOverrides>) {
    setCustomization(normalizeThemeCustomization({
      ...draft.value.customization,
      seed: {
        ...(draft.value.customization.seed ?? {}),
        ...patch,
      },
    }))
  }

  function setCustomization(customization: ThemeCustomization) {
    draft.value = {
      ...draft.value,
      customization: normalizeThemeCustomization(customization),
    }
  }

  function resolvedDraftTheme(value: ThemeEditorDraft): ThemeName {
    return value.mode === 'system' ? effectiveTheme.value : value.mode
  }

  return {
    draft,
    scope,
    globalCategory,
    preview,
    previewTheme,
    component,
    query,
    resolvedPreview,
    previewStyle,
    modificationCount,
    dirty,
    selectPreset,
    setPrimaryColor,
    setMode,
    setSeedColor,
    setSeedNumber,
    resetSeed,
    setComponentNumber,
    resetComponentNumber,
    setTokenOverride,
    resetTokenOverride,
    resetDraft,
    restoreDefaults,
    save,
  }
}

function normalizeDraft(value: ThemeEditorDraft): ThemeEditorDraft | null {
  const customColor = normalizeHexColor(value.customColor)
  if (!isThemeMode(value.mode) || !isThemeColorId(value.color) || !customColor)
    return null
  return {
    mode: value.mode,
    color: value.color,
    customColor,
    customization: normalizeThemeCustomization(value.customization),
  }
}

function cloneDraft(value: ThemeEditorDraft): ThemeEditorDraft {
  return {
    ...value,
    customization: normalizeThemeCustomization({
      seed: { ...(value.customization.seed ?? {}) },
      component: { ...(value.customization.component ?? {}) },
      tokenOverrides: { ...(value.customization.tokenOverrides ?? {}) },
    }),
  }
}

function countDraftChanges(saved: ThemeEditorDraft, draft: ThemeEditorDraft) {
  let count = 0
  if (saved.mode !== draft.mode)
    count += 1
  if (saved.color !== draft.color || saved.customColor !== draft.customColor)
    count += 1

  const savedSeed = saved.customization.seed ?? {}
  const draftSeed = draft.customization.seed ?? {}
  for (const key of new Set([...Object.keys(savedSeed), ...Object.keys(draftSeed)])) {
    if (savedSeed[key as keyof ThemeSeedOverrides] !== draftSeed[key as keyof ThemeSeedOverrides])
      count += 1
  }

  const savedTokens = saved.customization.tokenOverrides ?? {}
  const draftTokens = draft.customization.tokenOverrides ?? {}
  for (const key of new Set([...Object.keys(savedTokens), ...Object.keys(draftTokens)])) {
    if (savedTokens[key as ThemeTokenName] !== draftTokens[key as ThemeTokenName])
      count += 1
  }

  const savedComponent = saved.customization.component ?? {}
  const draftComponent = draft.customization.component ?? {}
  for (const key of new Set([...Object.keys(savedComponent), ...Object.keys(draftComponent)])) {
    if (
      savedComponent[key as keyof ThemeComponentOverrides]
      !== draftComponent[key as keyof ThemeComponentOverrides]
    ) {
      count += 1
    }
  }
  return count
}
