<script setup lang="ts">
import type { XaiClientProfilePreview, XaiClientProfileSelection } from '@/api/modules/client-profiles'
import { computed, onMounted, shallowRef, watch } from 'vue'
import { getXaiClientProfileOptions, previewXaiClientProfile } from '@/api/modules/client-profiles'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import { errorMessage } from '@/utils/async'
import ClientProfilePreviewPanel from './ClientProfilePreviewPanel.vue'

const props = withDefaults(defineProps<{ active?: boolean, disabled?: boolean, allowInherit?: boolean }>(), {
  active: true,
  disabled: false,
  allowInherit: false,
})
const model = defineModel<XaiClientProfileSelection | null>({ required: true })
const globalConfiguration = shallowRef<XaiClientProfileSelection>()
const preview = shallowRef<XaiClientProfilePreview>()
const loadError = shallowRef('')
const previewError = shallowRef('')
const previewing = shallowRef(false)
const needsVersionInput = computed(() => model.value?.versionMode === 'fixed' && !model.value.clientVersion)
const fields = [
  { key: 'clientIdentifier', label: '客户端标识' },
  { key: 'clientMode', label: '运行模式' },
  { key: 'targetOs', label: '操作系统' },
  { key: 'targetArch', label: 'CPU 架构' },
] as const

const source = computed({
  get: () => model.value === null ? 'global' : 'independent',
  set: (value: string) => {
    if (value === 'global')
      model.value = null
    else if (globalConfiguration.value)
      model.value = { ...globalConfiguration.value }
  },
})
const versionMode = computed({
  get: () => model.value?.versionMode ?? 'latest',
  set: (value: string) => {
    if (model.value) {
      model.value = {
        ...model.value,
        versionMode: value === 'fixed' ? 'fixed' : 'latest',
        clientVersion: value === 'fixed' ? preview.value?.clientVersion ?? null : null,
      }
    }
  },
})

function updateField(key: keyof XaiClientProfileSelection, value: string) {
  if (model.value)
    model.value = { ...model.value, [key]: key === 'clientVersion' ? value || null : value }
}

async function load() {
  loadError.value = ''
  try {
    globalConfiguration.value = (await getXaiClientProfileOptions()).globalConfiguration
  }
  catch (error) {
    loadError.value = errorMessage(error)
  }
}

watch([model, () => props.active], ([configuration, active], _, onCleanup) => {
  let cancelled = false
  preview.value = undefined
  previewError.value = ''
  previewing.value = active && !needsVersionInput.value
  if (!active || needsVersionInput.value)
    return
  const timer = setTimeout(async () => {
    try {
      const result = await previewXaiClientProfile(configuration)
      if (!cancelled)
        preview.value = result
    }
    catch (error) {
      if (!cancelled)
        previewError.value = errorMessage(error)
    }
    finally {
      if (!cancelled)
        previewing.value = false
    }
  }, 250)
  onCleanup(() => {
    cancelled = true
    clearTimeout(timer)
  })
}, { immediate: true })

onMounted(() => props.allowInherit && void load())
</script>

<template>
  <div class="grid min-w-0 gap-4">
    <div v-if="loadError" role="alert" class="flex items-center justify-between gap-3 text-cp text-cp-error">
      <span>全局配置加载失败：{{ loadError }}</span>
      <BaseButton size="sm" @click="load">
        重试
      </BaseButton>
    </div>
    <BaseSegmented
      v-if="allowInherit"
      v-model="source"
      label="xAI 客户端身份来源"
      class="justify-self-start"
      :options="[{ label: '全局配置', value: 'global' }, { label: '独立配置', value: 'independent' }]"
      :disabled="disabled || !globalConfiguration"
    />
    <template v-if="model">
      <div class="grid gap-4 sm:grid-cols-2">
        <BaseFormItem label="客户端类型">
          <BaseInput model-value="Grok CLI" readonly :disabled="disabled" />
        </BaseFormItem>
        <BaseFormItem label="版本策略">
          <BaseSelect v-model="versionMode" class="w-full" :disabled="disabled" :options="[{ label: '跟随最新版本', value: 'latest' }, { label: '自定义版本', value: 'fixed' }]" />
        </BaseFormItem>
      </div>
      <div v-if="model.versionMode === 'fixed'" class="grid gap-4 sm:grid-cols-2">
        <BaseFormItem label="Grok CLI 版本" required>
          <BaseInput :model-value="model.clientVersion ?? ''" :disabled="disabled" placeholder="例如：1.0.13" @update:model-value="updateField('clientVersion', $event)" />
        </BaseFormItem>
        <BaseFormItem v-for="field in fields" :key="field.key" :label="field.label" required>
          <BaseInput :model-value="model[field.key]" :disabled="disabled" @update:model-value="updateField(field.key, $event)" />
        </BaseFormItem>
      </div>
    </template>
    <ClientProfilePreviewPanel :preview="preview" :previewing="previewing" :needs-version-input="needsVersionInput" :error="previewError" />
  </div>
</template>
