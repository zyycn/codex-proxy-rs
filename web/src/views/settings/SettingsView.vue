<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { Save, RefreshCw } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseSpinner from '@/components/base/BaseSpinner.vue'
import AppTopbar from '@/layout/components/AppTopbar.vue'

import type { Settings } from '@/api'
import { getSettings, updateSettings } from '@/api'
import { useToastStore } from '@/stores/modules/toast'

const toast = useToastStore()

const loading = ref(true)
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
  <div class="w-full min-w-295 p-7">
    <header class="flex h-17 items-start justify-between">
      <div>
        <h1 class="mt-0 text-[34px] leading-[1.15] font-extrabold mb-0 text-(--cp-text-primary)">
          系统设置
        </h1>
        <p class="mt-2.5 text-[15px] leading-[1.15] font-semibold mb-0 text-(--cp-text-secondary)">
          配置系统运行参数
        </p>
      </div>

      <AppTopbar class="mt-0.5" />
    </header>

    <BaseSpinner v-if="loading" class="mt-20" />

    <div v-else class="mt-6 grid gap-6 max-w-4xl">
      <!-- 模型配置 -->
      <BaseCard>
        <h2 class="m-0 text-[20px] font-bold text-(--cp-text-primary) mb-5">模型配置</h2>
        <div>
          <label class="block text-[14px] font-medium text-(--cp-text-primary) mb-2">默认模型</label>
          <BaseInput v-model="form.defaultModel" class="w-64" />
        </div>
      </BaseCard>

      <!-- 调度配置 -->
      <BaseCard>
        <h2 class="m-0 text-[20px] font-bold text-(--cp-text-primary) mb-5">调度配置</h2>
        <div class="grid gap-5">
          <div>
            <label class="block text-[14px] font-medium text-(--cp-text-primary) mb-2">单账号最大并发</label>
            <BaseInput v-model.number="form.maxConcurrentPerAccount" type="number" class="w-48" />
          </div>
          <div>
            <label class="block text-[14px] font-medium text-(--cp-text-primary) mb-2">请求间隔 (ms)</label>
            <BaseInput v-model.number="form.requestIntervalMs" type="number" class="w-48" />
          </div>
          <div>
            <label class="block text-[14px] font-medium text-(--cp-text-primary) mb-2">轮换策略</label>
            <BaseSelect v-model="form.rotationStrategy" :options="rotationOptions" class="w-48" />
          </div>
          <div class="flex items-center justify-between">
            <div>
              <label class="block text-[14px] font-medium text-(--cp-text-primary) mb-1">跳过配额耗尽账号</label>
              <p class="m-0 text-[13px] text-(--cp-text-secondary)">调度时自动跳过已触发配额的账号</p>
            </div>
            <label class="relative inline-flex items-center cursor-pointer">
              <input v-model="form.quotaSkipExhausted" type="checkbox" class="sr-only peer">
              <div class="w-11 h-6 bg-gray-200 peer-focus:outline-none peer-focus:ring-4 peer-focus:ring-blue-300 rounded-full peer peer-checked:after:translate-x-full rtl:peer-checked:after:-translate-x-full peer-checked:after:border-white after:content-[''] after:absolute after:top-[2px] after:start-[2px] after:bg-white after:border-gray-300 after:border after:rounded-full after:h-5 after:w-5 after:transition-all peer-checked:bg-blue-600" />
            </label>
          </div>
        </div>
      </BaseCard>

      <!-- 日志配置 -->
      <BaseCard>
        <h2 class="m-0 text-[20px] font-bold text-(--cp-text-primary) mb-5">日志配置</h2>
        <div class="grid gap-5">
          <div class="flex items-center justify-between">
            <div>
              <label class="block text-[14px] font-medium text-(--cp-text-primary) mb-1">启用日志记录</label>
              <p class="m-0 text-[13px] text-(--cp-text-secondary)">记录系统运行事件和错误</p>
            </div>
            <label class="relative inline-flex items-center cursor-pointer">
              <input v-model="form.logsEnabled" type="checkbox" class="sr-only peer">
              <div class="w-11 h-6 bg-gray-200 peer-focus:outline-none peer-focus:ring-4 peer-focus:ring-blue-300 rounded-full peer peer-checked:after:translate-x-full rtl:peer-checked:after:-translate-x-full peer-checked:after:border-white after:content-[''] after:absolute after:top-[2px] after:start-[2px] after:bg-white after:border-gray-300 after:border after:rounded-full after:h-5 after:w-5 after:transition-all peer-checked:bg-blue-600" />
            </label>
          </div>
          <div>
            <label class="block text-[14px] font-medium text-(--cp-text-primary) mb-2">日志容量上限</label>
            <BaseInput v-model.number="form.logsCapacity" type="number" class="w-48" />
          </div>
        </div>
      </BaseCard>

      <!-- 刷新配置 -->
      <BaseCard>
        <h2 class="m-0 text-[20px] font-bold text-(--cp-text-primary) mb-5">自动刷新</h2>
        <div class="flex items-center justify-between">
          <div>
            <label class="block text-[14px] font-medium text-(--cp-text-primary) mb-1">启用自动刷新</label>
            <p class="m-0 text-[13px] text-(--cp-text-secondary)">定时刷新账号令牌和配额信息</p>
          </div>
          <label class="relative inline-flex items-center cursor-pointer">
            <input v-model="form.refreshEnabled" type="checkbox" class="sr-only peer">
            <div class="w-11 h-6 bg-gray-200 peer-focus:outline-none peer-focus:ring-4 peer-focus:ring-blue-300 rounded-full peer peer-checked:after:translate-x-full rtl:peer-checked:after:-translate-x-full peer-checked:after:border-white after:content-[''] after:absolute after:top-[2px] after:start-[2px] after:bg-white after:border-gray-300 after:border after:rounded-full after:h-5 after:w-5 after:transition-all peer-checked:bg-blue-600" />
          </label>
        </div>
      </BaseCard>

      <!-- 保存按钮 -->
      <div class="flex items-center gap-3">
        <BaseButton variant="primary" size="md" :disabled="saving" @click="handleSave">
          <Save class="size-4" />
          {{ saving ? '保存中...' : '保存设置' }}
        </BaseButton>
        <BaseButton variant="ghost" size="md" :disabled="loading" @click="loadSettings">
          <RefreshCw class="size-4" />
          重置
        </BaseButton>
      </div>
    </div>
  </div>
</template>
