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
  logging: {
    enabled: true,
    level: 'info',
  },
  quota: {
    warningThresholds: {
      requestsRemaining: 1000,
      tokensRemaining: 100000,
    },
  },
})

const requestsThreshold = ref('1000')
const tokensThreshold = ref('100000')

const logLevelOptions = [
  { label: 'Debug', value: 'debug' },
  { label: 'Info', value: 'info' },
  { label: 'Warning', value: 'warning' },
  { label: 'Error', value: 'error' },
]

async function loadSettings() {
  try {
    loading.value = true
    const data = await getSettings()
    form.value = data
    requestsThreshold.value = String(data.quota.warningThresholds.requestsRemaining)
    tokensThreshold.value = String(data.quota.warningThresholds.tokensRemaining)
  } catch (error: any) {
    toast.error(error.message || '加载失败')
  } finally {
    loading.value = false
  }
}

async function handleSave() {
  try {
    saving.value = true

    // 转换字符串为数字
    form.value.quota.warningThresholds.requestsRemaining = parseInt(requestsThreshold.value) || 0
    form.value.quota.warningThresholds.tokensRemaining = parseInt(tokensThreshold.value) || 0

    await updateSettings(form.value)
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
      <!-- 日志配置 -->
      <BaseCard>
        <div class="flex items-center justify-between mb-5">
          <div>
            <h2 class="m-0 text-[20px] font-bold text-(--cp-text-primary)">
              日志配置
            </h2>
            <p class="mt-1 m-0 text-[13px] text-(--cp-text-secondary)">
              控制系统日志记录行为
            </p>
          </div>
        </div>

        <div class="grid gap-5">
          <div class="flex items-center justify-between">
            <div>
              <label class="block text-[14px] font-medium text-(--cp-text-primary) mb-1">
                启用日志记录
              </label>
              <p class="m-0 text-[13px] text-(--cp-text-secondary)">
                记录系统运行事件和错误
              </p>
            </div>
            <label class="relative inline-flex items-center cursor-pointer">
              <input
                v-model="form.logging.enabled"
                type="checkbox"
                class="sr-only peer"
              >
              <div class="w-11 h-6 bg-gray-200 peer-focus:outline-none peer-focus:ring-4 peer-focus:ring-blue-300 rounded-full peer peer-checked:after:translate-x-full rtl:peer-checked:after:-translate-x-full peer-checked:after:border-white after:content-[''] after:absolute after:top-[2px] after:start-[2px] after:bg-white after:border-gray-300 after:border after:rounded-full after:h-5 after:w-5 after:transition-all peer-checked:bg-blue-600" />
            </label>
          </div>

          <div>
            <label class="block text-[14px] font-medium text-(--cp-text-primary) mb-2">
              日志级别
            </label>
            <BaseSelect
              v-model="form.logging.level"
              :options="logLevelOptions"
              class="w-48"
            />
          </div>
        </div>
      </BaseCard>

      <!-- 配额设置 -->
      <BaseCard>
        <div class="flex items-center justify-between mb-5">
          <div>
            <h2 class="m-0 text-[20px] font-bold text-(--cp-text-primary)">
              配额警告阈值
            </h2>
            <p class="mt-1 m-0 text-[13px] text-(--cp-text-secondary)">
              配置账号配额预警参数
            </p>
          </div>
        </div>

        <div class="grid gap-5">
          <div>
            <label class="block text-[14px] font-medium text-(--cp-text-primary) mb-2">
              剩余请求数阈值
            </label>
            <BaseInput
              v-model="requestsThreshold"
              type="number"
              class="w-64"
            />
            <p class="mt-1.5 m-0 text-[12px] text-(--cp-text-tertiary)">
              当账号剩余请求数低于此值时发出警告
            </p>
          </div>

          <div>
            <label class="block text-[14px] font-medium text-(--cp-text-primary) mb-2">
              剩余 Tokens 阈值
            </label>
            <BaseInput
              v-model="tokensThreshold"
              type="number"
              class="w-64"
            />
            <p class="mt-1.5 m-0 text-[12px] text-(--cp-text-tertiary)">
              当账号剩余 Tokens 低于此值时发出警告
            </p>
          </div>
        </div>
      </BaseCard>

      <!-- 保存按钮 -->
      <div class="flex items-center gap-3">
        <BaseButton
          variant="primary"
          size="md"
          :disabled="saving"
          @click="handleSave"
        >
          <Save class="size-4" />
          {{ saving ? '保存中...' : '保存设置' }}
        </BaseButton>

        <BaseButton
          variant="ghost"
          size="md"
          :disabled="loading"
          @click="loadSettings"
        >
          <RefreshCw class="size-4" />
          重置
        </BaseButton>
      </div>
    </div>
  </div>
</template>
