<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { Cpu, Gauge, RefreshCw, Save, Timer } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import { withMinimumDuration } from '@/utils/async'

import { getSettings, updateSettings } from '@/api'
import { toast } from '@/components/base/BaseToast'

const loading = ref(true)
const refreshing = ref(false)
const saving = ref(false)

const form = ref({
  defaultModel: '',
  maxConcurrentPerAccount: 3,
  requestIntervalMs: 0,
  rotationStrategy: 'least_used',
})

const rotationOptions = [
  { label: '最少使用优先', value: 'least_used' },
  { label: '轮询', value: 'round_robin' },
  { label: '会话粘性', value: 'sticky' },
]

function numericModel(key: 'maxConcurrentPerAccount' | 'requestIntervalMs') {
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
    const patch = {
      defaultModel: form.value.defaultModel,
      maxConcurrentPerAccount: form.value.maxConcurrentPerAccount,
      requestIntervalMs: form.value.requestIntervalMs,
      rotationStrategy: form.value.rotationStrategy,
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
    <header class="flex min-h-17 items-start justify-between gap-4">
      <div>
        <h1 class="mt-0 text-[34px] leading-[1.15] font-extrabold mb-0 text-(--cp-text-primary)">
          系统设置
        </h1>
        <p class="mt-2.5 text-[15px] leading-[1.15] font-semibold mb-0 text-(--cp-text-secondary)">
          配置默认模型和账号调度参数。
        </p>
      </div>

      <div class="mt-0.5 flex shrink-0 items-center gap-2">
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
          <template #icon>
            <Save class="size-4" />
          </template>
          {{ saving ? '保存中...' : '保存设置' }}
        </BaseButton>
      </div>
    </header>

    <BaseCard v-loading="loading" :padded="false" class="mt-5 max-w-5xl" body-class="px-5 py-5">
      <template #body>
        <div class="grid gap-5 md:grid-cols-2">
          <div class="grid gap-2">
            <span class="text-xs leading-[1.15] font-bold text-(--cp-text-secondary)"
              >默认模型</span
            >
            <BaseInput v-model="form.defaultModel">
              <template #prefix>
                <Cpu class="size-4 text-(--cp-text-muted)" />
              </template>
            </BaseInput>
          </div>

          <div class="grid gap-2">
            <span class="text-xs leading-[1.15] font-bold text-(--cp-text-secondary)">
              单账号最大并发
            </span>
            <BaseInput v-model="maxConcurrentPerAccountValue" type="number">
              <template #prefix>
                <Gauge class="size-4 text-(--cp-text-muted)" />
              </template>
            </BaseInput>
          </div>

          <div class="grid gap-2">
            <span class="text-xs leading-[1.15] font-bold text-(--cp-text-secondary)"
              >轮换策略</span
            >
            <BaseSelect v-model="form.rotationStrategy" :options="rotationOptions" />
          </div>

          <div class="grid gap-2">
            <span class="text-xs leading-[1.15] font-bold text-(--cp-text-secondary)">
              请求间隔 ms
            </span>
            <BaseInput v-model="requestIntervalMsValue" type="number">
              <template #prefix>
                <Timer class="size-4 text-(--cp-text-muted)" />
              </template>
            </BaseInput>
          </div>
        </div>
      </template>
    </BaseCard>
  </div>
</template>
