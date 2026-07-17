import type { rotationOptions } from '../options'
import { sortBy } from 'es-toolkit'

import { computed, onMounted, reactive, ref, shallowRef } from 'vue'
import { getSettings, updateSettings } from '@/api'
import { toast } from '@/components/base/BaseToast'

import { errorMessage } from '@/utils/async'

type RotationStrategy = (typeof rotationOptions)[number]['value']

export function useSettingsForm() {
  const loading = shallowRef(true)
  const saving = shallowRef(false)
  const aliasError = shallowRef('')
  const form = reactive({
    modelAliases: {} as Record<string, string>,
    refreshMarginSeconds: null as number | null,
    refreshConcurrency: null as number | null,
    maxConcurrentPerAccount: null as number | null,
    requestIntervalMs: null as number | null,
    rotationStrategy: '' as RotationStrategy | '',
  })
  const aliasRows = ref<ReturnType<typeof modelAliasesToRows>>([])

  function numericModel(
    key: keyof Pick<
      typeof form,
      | 'refreshMarginSeconds'
      | 'refreshConcurrency'
      | 'maxConcurrentPerAccount'
      | 'requestIntervalMs'
    >,
  ) {
    return computed({
      get: () => (form[key] === null ? '' : String(form[key])),
      set: (value: string) => {
        if (!value.trim()) {
          form[key] = null
          return
        }
        const parsed = Number(value)
        form[key] = Number.isFinite(parsed) ? parsed : null
      },
    })
  }

  const refreshMarginSecondsValue = numericModel('refreshMarginSeconds')
  const refreshConcurrencyValue = numericModel('refreshConcurrency')
  const maxConcurrentPerAccountValue = numericModel('maxConcurrentPerAccount')
  const requestIntervalMsValue = numericModel('requestIntervalMs')

  function applySettings(data: Awaited<ReturnType<typeof getSettings>>) {
    form.modelAliases = data.modelAliases
    form.refreshMarginSeconds = data.refreshMarginSeconds
    form.refreshConcurrency = data.refreshConcurrency
    form.maxConcurrentPerAccount = data.maxConcurrentPerAccount
    form.requestIntervalMs = data.requestIntervalMs
    form.rotationStrategy = data.rotationStrategy
    aliasRows.value = modelAliasesToRows(form.modelAliases)
    aliasError.value = ''
  }

  function addAliasRow() {
    aliasRows.value = [...aliasRows.value, { alias: '', target: '' }]
    aliasError.value = ''
  }

  function updateAliasRow(
    index: number,
    key: keyof (typeof aliasRows.value)[number],
    value: string,
  ) {
    aliasRows.value = aliasRows.value.map((row, rowIndex) =>
      rowIndex === index ? { ...row, [key]: value } : row,
    )
    aliasError.value = ''
  }

  function removeAliasRow(index: number) {
    aliasRows.value = aliasRows.value.filter((_row, rowIndex) => rowIndex !== index)
    aliasError.value = ''
  }

  async function loadSettings() {
    try {
      loading.value = true
      applySettings(await getSettings())
    }
    catch (error: unknown) {
      toast.error(errorMessage(error, '设置加载失败'))
    }
    finally {
      loading.value = false
    }
  }

  async function saveSettings() {
    if (saving.value || loading.value)
      return

    const aliasResult = rowsToAliases(aliasRows.value)
    if (aliasResult.error) {
      aliasError.value = aliasResult.error
      return
    }

    const {
      refreshMarginSeconds,
      refreshConcurrency,
      maxConcurrentPerAccount,
      requestIntervalMs,
      rotationStrategy,
    } = form
    if (
      refreshMarginSeconds === null
      || refreshConcurrency === null
      || maxConcurrentPerAccount === null
      || requestIntervalMs === null
      || !rotationStrategy
    ) {
      toast.warning('请完整填写运行参数和调度策略')
      return
    }

    try {
      saving.value = true
      applySettings(
        await updateSettings({
          modelAliases: aliasResult.modelAliases,
          refreshMarginSeconds,
          refreshConcurrency,
          maxConcurrentPerAccount,
          requestIntervalMs,
          rotationStrategy,
        }),
      )
      toast.success('设置已保存')
    }
    catch (error: unknown) {
      toast.error(errorMessage(error, '保存失败'))
    }
    finally {
      saving.value = false
    }
  }

  onMounted(() => {
    void loadSettings()
  })

  return {
    loading,
    saving,
    aliasError,
    form,
    aliasRows,
    refreshMarginSecondsValue,
    refreshConcurrencyValue,
    maxConcurrentPerAccountValue,
    requestIntervalMsValue,
    addAliasRow,
    updateAliasRow,
    removeAliasRow,
    saveSettings,
  }
}

function modelAliasesToRows(modelAliases: Record<string, string>) {
  return sortBy(Object.entries(modelAliases), [([alias]) => alias]).map(([alias, target]) => ({
    alias,
    target,
  }))
}

function rowsToAliases(rows: ReturnType<typeof modelAliasesToRows>) {
  const modelAliases: Record<string, string> = {}
  for (const row of rows) {
    const alias = row.alias.trim()
    const target = row.target.trim()
    if (!alias && !target)
      continue
    if (!alias || !target)
      return { modelAliases: {}, error: '别名和目标模型需要同时填写' }
    if (alias === target)
      return { modelAliases: {}, error: '别名不能指向自身' }
    if (modelAliases[alias] !== undefined) {
      return { modelAliases: {}, error: `别名重复：${alias}` }
    }
    modelAliases[alias] = target
  }
  return { modelAliases, error: '' }
}
