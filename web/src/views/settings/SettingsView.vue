<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { Cpu, Database, Gauge, RefreshCw, RotateCw, Save, ShieldCheck, Timer } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import { withMinimumDuration } from '@/utils/async'

import type { Settings } from '@/api'
import { getSettings, updateSettings } from '@/api'
import { toast } from '@/components/base/BaseToast'

const loading = ref(true)
const refreshing = ref(false)
const saving = ref(false)

const form = ref<Settings>({
  defaultModel: '',
  refreshEnabled: true,
  maxConcurrentPerAccount: 3,
  requestIntervalMs: 0,
  rotationStrategy: 'least_used',
  quotaWarningThresholds: { primary: [], secondary: [] },
  quotaSkipExhausted: false,
  logsEnabled: true,
  logsCapacity: 5000,
})

const rotationOptions = [
  { label: '最少使用优先', value: 'least_used' },
  { label: '轮询', value: 'round_robin' },
  { label: '随机', value: 'random' },
]

function numericModel(key: 'maxConcurrentPerAccount' | 'requestIntervalMs' | 'logsCapacity') {
  return computed({
    get: () => String(form.value[key] ?? 0),
    set: (value: string) => {
      const parsed = Number(value)
      form.value[key] = Number.isFinite(parsed) ? parsed : 0
    },
  })
}

const maxConcurrentPerAccountValue = numericModel('maxConcurrentPerAccount')
const requestIntervalMsValue = numericModel('requestIntervalMs')
const logsCapacityValue = numericModel('logsCapacity')

async function loadSettings() {
  try {
    loading.value = true
    const data = await getSettings()
    form.value = data
  } catch (error: any) {
    toast.error(error.message || '加载失败')
  } finally {
    loading.value = false
  }
}

async function refreshSettings() {
  if (refreshing.value || loading.value) return
  refreshing.value = true
  try {
    await withMinimumDuration(loadSettings)
  } finally {
    refreshing.value = false
  }
}

async function handleSave() {
  try {
    saving.value = true
    const patch: Partial<Settings> = {
      defaultModel: form.value.defaultModel,
      refreshEnabled: form.value.refreshEnabled,
      maxConcurrentPerAccount: form.value.maxConcurrentPerAccount,
      requestIntervalMs: form.value.requestIntervalMs,
      rotationStrategy: form.value.rotationStrategy,
      quotaWarningThresholds: form.value.quotaWarningThresholds,
      quotaSkipExhausted: form.value.quotaSkipExhausted,
      logsEnabled: form.value.logsEnabled,
      logsCapacity: form.value.logsCapacity,
    }
    await updateSettings(patch)
    toast.success('设置已保存')
  } catch (error: any) {
    toast.error(error.message || '保存失败')
  } finally {
    saving.value = false
  }
}

onMounted(() => {
  loadSettings()
})
</script>

<template>
  <div class="w-full">
    <header class="flex h-17 items-start justify-between">
      <div>
        <h1 class="mt-0 text-[34px] leading-[1.15] font-extrabold mb-0 text-(--cp-text-primary)">
          系统设置
        </h1>
        <p class="mt-2.5 text-[15px] leading-[1.15] font-semibold mb-0 text-(--cp-text-secondary)">
          配置系统运行参数
        </p>
      </div>
    </header>
    <section
      v-loading="loading"
      class="mt-6 overflow-hidden rounded-(--cp-card-radius) bg-(--cp-bg-surface) p-5 shadow-(--cp-shadow-card) md:p-6"
    >
      <header class="flex flex-wrap items-start justify-between gap-4">
        <div class="min-w-0">
          <h2 class="m-0 text-xl font-[760] leading-[1.15] text-(--cp-text-primary)">运行参数</h2>
          <p
            class="mt-1.75 mb-0 text-[13px] font-semibold leading-[1.15] text-(--cp-text-secondary)"
          >
            控制模型调度、刷新策略和事件日志保留。
          </p>
        </div>

        <div class="flex shrink-0 items-center gap-3">
          <BaseButton
            variant="ghost"
            :loading="refreshing"
            :disabled="loading"
            @click="refreshSettings"
          >
            <template #icon>
              <RefreshCw class="size-4" />
            </template>
            重置
          </BaseButton>
          <BaseButton variant="primary" :disabled="saving" @click="handleSave">
            <Save class="size-4" />
            {{ saving ? '保存中...' : '保存设置' }}
          </BaseButton>
        </div>
      </header>

      <div class="mt-5 grid gap-4 xl:grid-cols-12">
        <section
          class="min-w-0 rounded-(--cp-panel-radius) bg-(--cp-bg-subtle) p-4 md:p-5 xl:col-span-7"
        >
          <div class="mb-5 flex items-center gap-3">
            <span
              class="inline-flex size-9 shrink-0 items-center justify-center rounded-(--cp-icon-button-radius) bg-(--cp-info-bg) text-(--cp-info)"
            >
              <Cpu class="size-4.5" />
            </span>
            <div class="min-w-0">
              <h3 class="m-0 text-[15px] font-[760] leading-[1.15] text-(--cp-text-primary)">
                调度配置
              </h3>
              <p class="mt-1 mb-0 text-xs font-[650] leading-[1.15] text-(--cp-text-secondary)">
                模型、并发与账号轮换策略。
              </p>
            </div>
          </div>

          <div class="grid gap-4 md:grid-cols-2">
            <div class="grid gap-2 md:col-span-2">
              <span class="text-xs font-bold leading-[1.15] text-(--cp-text-secondary)">
                默认模型
              </span>
              <BaseInput v-model="form.defaultModel">
                <template #prefix>
                  <Cpu class="size-4 text-(--cp-text-muted)" />
                </template>
              </BaseInput>
            </div>

            <div class="grid gap-2">
              <span class="text-xs font-bold leading-[1.15] text-(--cp-text-secondary)">
                单账号最大并发
              </span>
              <BaseInput v-model="maxConcurrentPerAccountValue" type="number">
                <template #prefix>
                  <Gauge class="size-4 text-(--cp-text-muted)" />
                </template>
              </BaseInput>
            </div>

            <div class="grid gap-2">
              <span class="text-xs font-bold leading-[1.15] text-(--cp-text-secondary)">
                请求间隔 ms
              </span>
              <BaseInput v-model="requestIntervalMsValue" type="number">
                <template #prefix>
                  <Timer class="size-4 text-(--cp-text-muted)" />
                </template>
              </BaseInput>
            </div>

            <div class="grid gap-2 md:col-span-2">
              <span class="text-xs font-bold leading-[1.15] text-(--cp-text-secondary)">
                轮换策略
              </span>
              <BaseSelect v-model="form.rotationStrategy" :options="rotationOptions" />
            </div>
          </div>
        </section>

        <div class="grid min-w-0 content-start gap-4 xl:col-span-5">
          <section class="rounded-(--cp-panel-radius) bg-(--cp-bg-subtle) p-4 md:p-5">
            <div class="mb-4 flex items-center gap-3">
              <span
                class="inline-flex size-9 shrink-0 items-center justify-center rounded-(--cp-icon-button-radius) bg-(--cp-success-bg) text-(--cp-success-text)"
              >
                <RotateCw class="size-4.5" />
              </span>
              <div class="min-w-0">
                <h3 class="m-0 text-[15px] font-[760] leading-[1.15] text-(--cp-text-primary)">
                  运行策略
                </h3>
                <p class="mt-1 mb-0 text-xs font-[650] leading-[1.15] text-(--cp-text-secondary)">
                  影响调度健康度与账号可用性。
                </p>
              </div>
            </div>

            <div class="grid gap-3">
              <div
                class="flex min-h-14 items-center justify-between gap-4 rounded-(--cp-icon-button-radius) bg-(--cp-bg-surface) px-4 py-2 shadow-(--cp-shadow-control)"
              >
                <div class="min-w-0">
                  <p class="m-0 text-[13px] font-[720] leading-[1.15] text-(--cp-text-primary)">
                    自动刷新
                  </p>
                  <p class="mt-1 mb-0 text-xs font-[650] leading-[1.15] text-(--cp-text-secondary)">
                    定时刷新令牌和账号配额
                  </p>
                </div>
                <BaseSwitch v-model="form.refreshEnabled" label="启用自动刷新" />
              </div>

              <div
                class="flex min-h-14 items-center justify-between gap-4 rounded-(--cp-icon-button-radius) bg-(--cp-bg-surface) px-4 py-2 shadow-(--cp-shadow-control)"
              >
                <div class="min-w-0">
                  <p class="m-0 text-[13px] font-[720] leading-[1.15] text-(--cp-text-primary)">
                    跳过耗尽账号
                  </p>
                  <p class="mt-1 mb-0 text-xs font-[650] leading-[1.15] text-(--cp-text-secondary)">
                    调度时避开配额受限账号
                  </p>
                </div>
                <BaseSwitch v-model="form.quotaSkipExhausted" label="跳过配额耗尽账号" />
              </div>
            </div>
          </section>

          <section class="rounded-(--cp-panel-radius) bg-(--cp-bg-subtle) p-4 md:p-5">
            <div class="mb-4 flex items-center gap-3">
              <span
                class="inline-flex size-9 shrink-0 items-center justify-center rounded-(--cp-icon-button-radius) bg-(--cp-warning-bg) text-(--cp-warning-text)"
              >
                <ShieldCheck class="size-4.5" />
              </span>
              <div class="min-w-0">
                <h3 class="m-0 text-[15px] font-[760] leading-[1.15] text-(--cp-text-primary)">
                  日志策略
                </h3>
                <p class="mt-1 mb-0 text-xs font-[650] leading-[1.15] text-(--cp-text-secondary)">
                  控制事件日志采集与保留容量。
                </p>
              </div>
            </div>

            <div class="grid gap-3">
              <div
                class="flex min-h-14 items-center justify-between gap-4 rounded-(--cp-icon-button-radius) bg-(--cp-bg-surface) px-4 py-2 shadow-(--cp-shadow-control)"
              >
                <div class="min-w-0">
                  <p class="m-0 text-[13px] font-[720] leading-[1.15] text-(--cp-text-primary)">
                    启用事件日志
                  </p>
                  <p class="mt-1 mb-0 text-xs font-[650] leading-[1.15] text-(--cp-text-secondary)">
                    记录代理请求和上游状态
                  </p>
                </div>
                <BaseSwitch v-model="form.logsEnabled" label="启用事件日志" />
              </div>

              <div class="grid gap-2">
                <span class="text-xs font-bold leading-[1.15] text-(--cp-text-secondary)">
                  容量上限
                </span>
                <BaseInput v-model="logsCapacityValue" type="number">
                  <template #prefix>
                    <Database class="size-4 text-(--cp-text-muted)" />
                  </template>
                </BaseInput>
              </div>
            </div>
          </section>
        </div>
      </div>
    </section>
  </div>
</template>
