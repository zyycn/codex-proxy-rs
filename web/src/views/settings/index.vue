<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import { Gauge, GitBranch, Plus, Save, Timer, Trash2, Zap } from '@lucide/vue'

import { getSettings, updateSettings } from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import { toast } from '@/components/base/BaseToast'

type RotationStrategy = 'least_used' | 'round_robin' | 'sticky'

interface AliasRow {
  alias: string
  target: string
}

interface SettingsForm {
  modelAliases: Record<string, string>
  refreshMarginSeconds: number
  refreshConcurrency: number
  maxConcurrentPerAccount: number
  requestIntervalMs: number
  rotationStrategy: RotationStrategy
}

const loading = ref(true)
const saving = ref(false)
const aliasError = ref('')

const form = reactive<SettingsForm>({
  modelAliases: {},
  refreshMarginSeconds: 300,
  refreshConcurrency: 2,
  maxConcurrentPerAccount: 3,
  requestIntervalMs: 50,
  rotationStrategy: 'least_used',
})

const aliasRows = ref<AliasRow[]>([])

const rotationOptions: Array<{
  label: string
  value: RotationStrategy
  description: string
}> = [
  {
    label: '智能分配',
    value: 'least_used',
    description: '优先使用压力更低的账号，适合长期稳定运行。',
  },
  {
    label: '轮询',
    value: 'round_robin',
    description: '按顺序分配请求，让账号池保持均匀节奏。',
  },
  {
    label: '粘性',
    value: 'sticky',
    description: '持续使用同一账号，直到遇到限速或不可用。',
  },
]

function numericModel(
  key: keyof Pick<
    SettingsForm,
    'refreshMarginSeconds' | 'refreshConcurrency' | 'maxConcurrentPerAccount' | 'requestIntervalMs'
  >,
) {
  return computed({
    get: () => String(form[key] ?? 0),
    set: (value: string) => {
      const parsed = Number(value)
      form[key] = Number.isFinite(parsed) ? parsed : 0
    },
  })
}

const refreshMarginSecondsValue = numericModel('refreshMarginSeconds')
const refreshConcurrencyValue = numericModel('refreshConcurrency')
const maxConcurrentPerAccountValue = numericModel('maxConcurrentPerAccount')
const requestIntervalMsValue = numericModel('requestIntervalMs')

function modelAliasesToRows(modelAliases: Record<string, string> = {}) {
  return Object.entries(modelAliases)
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([alias, target]) => ({ alias, target }))
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
  form.modelAliases = data.modelAliases || {}
  form.refreshMarginSeconds = Number(data.refreshMarginSeconds ?? 300)
  form.refreshConcurrency = Number(data.refreshConcurrency ?? 2)
  form.maxConcurrentPerAccount = Number(data.maxConcurrentPerAccount ?? 3)
  form.requestIntervalMs = Number(data.requestIntervalMs ?? 50)
  form.rotationStrategy = (data.rotationStrategy || 'least_used') as RotationStrategy
  aliasRows.value = modelAliasesToRows(form.modelAliases)
  aliasError.value = ''
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
    applySettings(await getSettings())
  } catch (error: any) {
    toast.error(error.message || '加载失败')
  } finally {
    loading.value = false
  }
}

async function handleSave() {
  if (saving.value || loading.value) return

  const aliasResult = rowsToAliases(aliasRows.value)
  if (aliasResult.error) {
    aliasError.value = aliasResult.error
    return
  }

  try {
    saving.value = true
    const data = await updateSettings({
      modelAliases: aliasResult.modelAliases,
      refreshMarginSeconds: form.refreshMarginSeconds,
      refreshConcurrency: form.refreshConcurrency,
      maxConcurrentPerAccount: form.maxConcurrentPerAccount,
      requestIntervalMs: form.requestIntervalMs,
      rotationStrategy: form.rotationStrategy,
    })
    applySettings(data)
    toast.success('设置已保存')
  } catch (error: any) {
    toast.error(error.message || '保存失败')
  } finally {
    saving.value = false
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
          让模型入口、账号选择和 Token 刷新保持可控。
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

    <div class="mt-5 grid max-w-6xl gap-5">
      <BaseCard
        :padded="false"
        title="运行参数"
        description="请求节奏、账号并发和 Token 刷新。"
        header-class="px-5 pt-4"
        body-class="px-5 py-5"
      >
        <div class="grid items-start gap-4 lg:grid-cols-2">
          <div class="grid gap-2">
            <span class="text-xs leading-[1.15] font-bold text-(--cp-text-secondary)">
              单账号最大并发
            </span>
            <BaseInput v-model="maxConcurrentPerAccountValue" type="number">
              <template #prefix>
                <Gauge class="size-4" />
              </template>
            </BaseInput>
          </div>

          <div class="grid gap-2">
            <span class="text-xs leading-[1.15] font-bold text-(--cp-text-secondary)">
              提前刷新秒数
            </span>
            <BaseInput v-model="refreshMarginSecondsValue" type="number">
              <template #prefix>
                <Timer class="size-4" />
              </template>
            </BaseInput>
            <span class="text-xs leading-[1.15] font-[650] text-(--cp-text-muted)">
              Token 过期前多少秒触发刷新。
            </span>
          </div>

          <div class="grid gap-2">
            <span class="text-xs leading-[1.15] font-bold text-(--cp-text-secondary)">
              刷新并发数
            </span>
            <BaseInput v-model="refreshConcurrencyValue" type="number">
              <template #prefix>
                <Zap class="size-4" />
              </template>
            </BaseInput>
            <span class="text-xs leading-[1.15] font-[650] text-(--cp-text-muted)">
              同时刷新 Token 的最大请求数，减小可避免限流。
            </span>
          </div>

          <div class="grid gap-2 lg:col-span-2">
            <span class="text-xs leading-[1.15] font-bold text-(--cp-text-secondary)">
              请求间隔 ms
            </span>
            <BaseInput v-model="requestIntervalMsValue" type="number">
              <template #prefix>
                <Timer class="size-4" />
              </template>
            </BaseInput>
          </div>
        </div>
      </BaseCard>

      <BaseCard
        :padded="false"
        title="模型映射"
        description="把客户端可见名称指向真实上游模型。"
        header-class="px-5 pt-4"
        body-class="px-5 py-5"
      >
        <div class="grid gap-3">
          <div
            class="hidden grid-cols-[minmax(0,1fr)_minmax(0,1fr)_2.5rem] gap-2 px-0.75 text-xs leading-none font-bold text-(--cp-text-secondary) sm:grid"
          >
            <span>别名</span>
            <span>目标模型</span>
            <span />
          </div>

          <div
            v-if="aliasRows.length === 0"
            class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-4 py-3 text-[13px] font-[650] text-(--cp-text-muted)"
          >
            还没有模型映射。
          </div>

          <div
            v-for="(row, index) in aliasRows"
            :key="index"
            class="grid items-center gap-2 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_2.5rem]"
          >
            <BaseInput
              :model-value="row.alias"
              placeholder="gpt-5.2"
              @update:model-value="updateAliasRow(index, 'alias', $event)"
            >
              <template #prefix>
                <GitBranch class="size-4" />
              </template>
            </BaseInput>
            <BaseInput
              :model-value="row.target"
              placeholder="gpt-5.5"
              @update:model-value="updateAliasRow(index, 'target', $event)"
            />
            <BaseButton
              variant="ghost"
              size="default"
              icon-only
              label="删除映射"
              :disabled="saving || loading"
              @click="removeAliasRow(index)"
            >
              <Trash2 class="size-4" />
            </BaseButton>
          </div>

          <div class="flex flex-wrap items-center gap-3 pt-1">
            <BaseButton variant="default" :disabled="saving || loading" @click="addAliasRow">
              <template #icon>
                <Plus class="size-4" />
              </template>
              添加映射
            </BaseButton>
            <span v-if="aliasError" class="text-xs font-[650] text-(--cp-danger-text)">
              {{ aliasError }}
            </span>
          </div>
        </div>
      </BaseCard>

      <BaseCard
        :padded="false"
        title="账号选择"
        description="决定每次请求如何使用账号池。"
        header-class="px-5 pt-4"
        body-class="px-5 py-5"
      >
        <div class="grid gap-3 lg:grid-cols-3">
          <button
            v-for="option in rotationOptions"
            :key="option.value"
            type="button"
            class="min-h-25 cursor-pointer rounded-(--cp-input-radius-base) border-0 px-4 py-3.5 text-left shadow-(--cp-shadow-input) outline-none transition-[background-color,box-shadow,color] duration-160 focus-visible:ring-2 focus-visible:ring-(--cp-info-border)"
            :class="
              form.rotationStrategy === option.value
                ? 'bg-(--cp-info-bg) text-(--cp-info-text) shadow-(--cp-shadow-control)'
                : 'bg-(--cp-input-current-bg,var(--cp-input-context-bg)) text-(--cp-text-primary) hover:bg-(--cp-input-current-bg-hover,var(--cp-input-context-bg-hover)) hover:shadow-(--cp-shadow-input-hover)'
            "
            :aria-pressed="form.rotationStrategy === option.value"
            @click="form.rotationStrategy = option.value"
          >
            <span class="flex items-center gap-2">
              <span
                class="inline-flex size-4 shrink-0 items-center justify-center rounded-full bg-(--cp-bg-surface) shadow-[inset_0_0_0_1px_var(--cp-default-border-hover)]"
              >
                <span
                  class="size-2 rounded-full transition-opacity duration-150"
                  :class="
                    form.rotationStrategy === option.value
                      ? 'bg-(--cp-info) opacity-100'
                      : 'opacity-0'
                  "
                />
              </span>
              <span class="text-[14px] leading-[1.15] font-[760]">{{ option.label }}</span>
            </span>
            <span
              class="mt-2 block text-[13px] leading-normal font-[650] text-(--cp-text-secondary)"
            >
              {{ option.description }}
            </span>
          </button>
        </div>
      </BaseCard>
    </div>
  </div>
</template>
