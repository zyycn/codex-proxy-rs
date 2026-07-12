<script setup lang="ts">
import { computed, onMounted, reactive, ref, shallowRef } from 'vue'
import { useClipboard } from '@vueuse/core'
import { sortBy } from 'es-toolkit'
import { Save } from '@lucide/vue'

import {
  deleteAdminApiKey,
  getAdminApiKeyStatus,
  getSettings,
  regenerateAdminApiKey,
  updateSettings,
} from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import { toast } from '@/components/base/BaseToast'

import AdminApiKeyCard from './components/AdminApiKeyCard.vue'
import ModelAliasesCard from './components/ModelAliasesCard.vue'
import RotationStrategyCard from './components/RotationStrategyCard.vue'
import RuntimeSettingsCard from './components/RuntimeSettingsCard.vue'

type RotationStrategy = 'smart' | 'quota_reset_priority' | 'round_robin' | 'sticky'

interface AliasRow {
  alias: string
  target: string
}

interface SettingsForm {
  modelAliases: Record<string, string>
  refreshMarginSeconds: number | null
  refreshConcurrency: number | null
  maxConcurrentPerAccount: number | null
  requestIntervalMs: number | null
  rotationStrategy: RotationStrategy | ''
}

interface RotationOption {
  label: string
  value: RotationStrategy
  description: string
}

interface AdminApiKeyStatus {
  exists: boolean
}

const loading = shallowRef(true)
const saving = shallowRef(false)
const aliasError = shallowRef('')
const adminKeyLoading = shallowRef(true)
const adminKeyRegenerating = shallowRef(false)
const adminKeyDeleting = shallowRef(false)
const showDeleteAdminKeyModal = shallowRef(false)
const generatedAdminApiKey = shallowRef('')
const { copy } = useClipboard()

const form = reactive<SettingsForm>({
  modelAliases: {},
  refreshMarginSeconds: null,
  refreshConcurrency: null,
  maxConcurrentPerAccount: null,
  requestIntervalMs: null,
  rotationStrategy: '',
})

const aliasRows = ref<AliasRow[]>([])
const adminApiKeyStatus = reactive<AdminApiKeyStatus>({
  exists: false,
})

const rotationOptions: RotationOption[] = [
  {
    label: '智能调度（推荐）',
    value: 'smart',
    description: '按负载、窗口用量、请求数和健康反馈评分，优先选择更空闲的账号',
  },
  {
    label: '额度重置优先',
    value: 'quota_reset_priority',
    description: '优先选择额度窗口更快重置的账号，适合在重置前消耗剩余额度',
  },
  {
    label: '轮询',
    value: 'round_robin',
    description: '在可用候选账号间按顺序轮转，分配结果最可预测',
  },
  {
    label: '粘滞',
    value: 'sticky',
    description: '优先复用最近使用的账号，直到不可用后再切换',
  },
]

function numericModel(
  key: keyof Pick<
    SettingsForm,
    'refreshMarginSeconds' | 'refreshConcurrency' | 'maxConcurrentPerAccount' | 'requestIntervalMs'
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

function modelAliasesToRows(modelAliases: Record<string, string> = {}) {
  return sortBy(Object.entries(modelAliases), [([alias]) => alias]).map(([alias, target]) => ({
    alias,
    target,
  }))
}

function rowsToAliases(rows: AliasRow[]) {
  const modelAliases: Record<string, string> = {}
  for (const row of rows) {
    const alias = row.alias.trim()
    const target = row.target.trim()
    if (!alias && !target) continue
    if (!alias || !target) {
      return { modelAliases: {}, error: '别名和目标模型需要同时填写' }
    }
    if (alias === target) {
      return { modelAliases: {}, error: '别名不能指向自身' }
    }
    if (modelAliases[alias] !== undefined) {
      return { modelAliases: {}, error: `别名重复：${alias}` }
    }
    modelAliases[alias] = target
  }

  return { modelAliases, error: '' }
}

function applySettings(data: any) {
  form.modelAliases = isRecord(data?.modelAliases) ? data.modelAliases : {}
  form.refreshMarginSeconds = optionalNumber(data?.refreshMarginSeconds)
  form.refreshConcurrency = optionalNumber(data?.refreshConcurrency)
  form.maxConcurrentPerAccount = optionalNumber(data?.maxConcurrentPerAccount)
  form.requestIntervalMs = optionalNumber(data?.requestIntervalMs)
  form.rotationStrategy = isRotationStrategy(data?.rotationStrategy) ? data.rotationStrategy : ''
  aliasRows.value = modelAliasesToRows(form.modelAliases)
  aliasError.value = ''
}

function optionalNumber(value: unknown) {
  if (value === null || value === undefined || value === '') return null
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : null
}

function isRecord(value: unknown): value is Record<string, string> {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value)
}

function isRotationStrategy(value: unknown): value is RotationStrategy {
  return rotationOptions.some((option) => option.value === value)
}

function applyAdminApiKeyStatus(data: any) {
  adminApiKeyStatus.exists = Boolean(data?.exists)
}

function addAliasRow() {
  aliasRows.value = [...aliasRows.value, { alias: '', target: '' }]
  aliasError.value = ''
}

function updateAliasRow(index: number, key: keyof AliasRow, value: string) {
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
    adminKeyLoading.value = true
    const [settings, adminKeyStatus] = await Promise.all([getSettings(), getAdminApiKeyStatus()])
    applySettings(settings)
    applyAdminApiKeyStatus(adminKeyStatus)
  } catch (error: any) {
    toast.error(error.message || '加载失败')
  } finally {
    loading.value = false
    adminKeyLoading.value = false
  }
}

async function handleSave() {
  if (saving.value || loading.value) return

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
    refreshMarginSeconds === null ||
    refreshConcurrency === null ||
    maxConcurrentPerAccount === null ||
    requestIntervalMs === null ||
    !rotationStrategy
  ) {
    toast.warning('请完整填写运行参数和调度策略')
    return
  }

  try {
    saving.value = true
    const data = await updateSettings({
      modelAliases: aliasResult.modelAliases,
      refreshMarginSeconds,
      refreshConcurrency,
      maxConcurrentPerAccount,
      requestIntervalMs,
      rotationStrategy,
    })
    applySettings(data)
    toast.success('设置已保存')
  } catch (error: any) {
    toast.error(error.message || '保存失败')
  } finally {
    saving.value = false
  }
}

async function handleRegenerateAdminApiKey() {
  if (adminKeyRegenerating.value || adminKeyDeleting.value) return

  try {
    adminKeyRegenerating.value = true
    const wasEnabled = adminApiKeyStatus.exists
    const data = await regenerateAdminApiKey()
    generatedAdminApiKey.value = data.key
    applyAdminApiKeyStatus({ exists: true })
    toast.success(wasEnabled ? '管理员 API Key 已更新' : '管理员 API Key 已生成')
  } catch (error: any) {
    toast.error(error.message || '生成失败')
  } finally {
    adminKeyRegenerating.value = false
  }
}

async function handleDeleteAdminApiKey() {
  if (adminKeyDeleting.value || adminKeyRegenerating.value) return

  try {
    adminKeyDeleting.value = true
    await deleteAdminApiKey()
    applyAdminApiKeyStatus({ exists: false })
    generatedAdminApiKey.value = ''
    showDeleteAdminKeyModal.value = false
    toast.success('管理员 API Key 已删除')
  } catch (error: any) {
    toast.error(error.message || '删除失败')
  } finally {
    adminKeyDeleting.value = false
  }
}

async function copyAdminApiKey() {
  if (!generatedAdminApiKey.value) return

  try {
    await copy(generatedAdminApiKey.value)
    toast.success('已复制')
  } catch (error: any) {
    toast.error(error.message || '复制失败')
  }
}

onMounted(loadSettings)
</script>

<template>
  <div class="w-full">
    <header class="flex min-h-17 items-start justify-between gap-4">
      <div>
        <h1 class="m-0 text-[34px] leading-[1.15] font-extrabold text-(--cp-text-primary)">
          系统设置
        </h1>
        <p class="mt-2.5 mb-0 text-[15px] leading-[1.15] font-semibold text-(--cp-text-secondary)">
          管理运行参数、调度策略、模型映射与外部访问配置
        </p>
      </div>

      <div class="mt-0.5 flex shrink-0 items-center gap-2">
        <BaseButton variant="primary" :loading="saving" :disabled="loading" @click="handleSave">
          <template #icon>
            <Save class="size-4" />
          </template>
          {{ saving ? '保存中...' : '保存' }}
        </BaseButton>
      </div>
    </header>

    <div class="mt-5 grid w-full gap-5">
      <AdminApiKeyCard
        :status="adminApiKeyStatus"
        :loading="adminKeyLoading"
        :regenerating="adminKeyRegenerating"
        :deleting="adminKeyDeleting"
        :generated-key="generatedAdminApiKey"
        @regenerate="handleRegenerateAdminApiKey"
        @request-delete="showDeleteAdminKeyModal = true"
        @copy="copyAdminApiKey"
      />

      <RuntimeSettingsCard
        v-model:max-concurrent-per-account="maxConcurrentPerAccountValue"
        v-model:refresh-margin-seconds="refreshMarginSecondsValue"
        v-model:refresh-concurrency="refreshConcurrencyValue"
        v-model:request-interval-ms="requestIntervalMsValue"
      />

      <ModelAliasesCard
        :rows="aliasRows"
        :error="aliasError"
        :disabled="saving || loading"
        @add="addAliasRow"
        @update="updateAliasRow"
        @remove="removeAliasRow"
      />

      <RotationStrategyCard v-model="form.rotationStrategy" :options="rotationOptions" />

      <BaseConfirmModal
        v-model="showDeleteAdminKeyModal"
        title="删除管理员 API Key"
        description="删除后外部系统将无法继续使用该 Key 调用管理接口"
        variant="danger"
        confirm-text="确认删除"
        :loading="adminKeyDeleting"
        width="480px"
        @confirm="handleDeleteAdminApiKey"
      >
        <p class="m-0">确定要删除当前管理员 API Key 吗？此操作会立即生效</p>
      </BaseConfirmModal>
    </div>
  </div>
</template>
