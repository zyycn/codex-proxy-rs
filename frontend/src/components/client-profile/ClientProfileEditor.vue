<script setup lang="ts">
import type { ClientProfilePreset, ClientProfilePreview, ClientProfileSelection } from '@/api/modules/client-profiles'
import { computed, onMounted, shallowRef, watch } from 'vue'
import { getClientProfileOptions, previewClientProfile } from '@/api/modules/client-profiles'
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
const model = defineModel<ClientProfileSelection | null>({ required: true })
const presets = shallowRef<ClientProfilePreset[]>([])
const globalConfiguration = shallowRef<ClientProfileSelection>()
const preview = shallowRef<ClientProfilePreview>()
const loading = shallowRef(true)
const loadError = shallowRef('')
const previewError = shallowRef('')
const previewing = shallowRef(false)
const platforms = { macos: 'MacOS', linux: 'Linux', windows: 'Windows' }
const presetOptions = computed(() => presets.value.map(({ configuration }) => ({
  value: `${configuration.platform}-${configuration.client}`,
  label: `${platforms[configuration.platform]} · ${configuration.client === 'desktop' ? 'Desktop' : 'CLI'}`,
})))
const currentPreset = computed(() => presets.value.find(({ configuration }) =>
  configuration.client === model.value?.client && configuration.platform === model.value?.platform,
))
const needsVersionInput = computed(() => model.value?.versionMode === 'fixed'
  && (!model.value.codexVersion || (model.value.client === 'desktop' && (!model.value.desktopVersion || !model.value.desktopBuild))))
const profileSource = computed({
  get: () => model.value === null ? 'global' : 'independent',
  set: (value: string) => {
    if (value === 'global')
      model.value = null
    else if (globalConfiguration.value)
      model.value = { ...globalConfiguration.value }
  },
})
const selectedPreset = computed({
  get: () => model.value ? `${model.value.platform}-${model.value.client}` : '',
  set: (value: string) => {
    const preset = presets.value.find(({ configuration }) => `${configuration.platform}-${configuration.client}` === value)
    if (preset)
      model.value = { ...preset.configuration, versionMode: preset.automaticAvailable ? 'latest' : 'fixed' }
  },
})
const versionMode = computed({
  get: () => model.value?.versionMode ?? 'latest',
  set: (value: string) => {
    if (!model.value)
      return
    const fixed = value === 'fixed'
    model.value = {
      ...model.value,
      versionMode: fixed ? 'fixed' : 'latest',
      codexVersion: fixed ? preview.value?.codexVersion ?? null : null,
      desktopVersion: fixed ? preview.value?.desktopVersion ?? null : null,
      desktopBuild: fixed ? preview.value?.desktopBuild ?? null : null,
    }
  },
})
const customFields = [
  { key: 'originator', label: '客户端标识' },
  { key: 'osVersion', label: '系统版本' },
  { key: 'arch', label: 'CPU 架构' },
  { key: 'terminal', label: '终端标记' },
] as const

function updateField(key: keyof ClientProfileSelection, value: string) {
  if (model.value)
    model.value = { ...model.value, [key]: value || null }
}

async function load() {
  loading.value = true
  loadError.value = ''
  try {
    const options = await getClientProfileOptions()
    presets.value = options.presets
    globalConfiguration.value = options.globalConfiguration
  }
  catch (error) {
    loadError.value = errorMessage(error)
  }
  finally {
    loading.value = false
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
      const result = await previewClientProfile(configuration)
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
  }, 300)
  onCleanup(() => {
    cancelled = true
    clearTimeout(timer)
  })
}, { immediate: true })

onMounted(load)
</script>

<template>
  <div class="grid min-w-0 gap-4">
    <div v-if="loadError" role="alert" class="flex items-center justify-between gap-3 text-cp text-cp-error">
      <span>预设加载失败：{{ loadError }}</span>
      <BaseButton size="sm" @click="load">
        重试
      </BaseButton>
    </div>
    <p v-else-if="loading" role="status" class="m-0 text-cp text-cp-text-secondary">
      正在加载客户端预设…
    </p>
    <template v-else>
      <BaseSegmented
        v-if="allowInherit"
        v-model="profileSource"
        label="客户端身份来源"
        class="justify-self-start"
        :options="[
          { label: '全局配置', value: 'global' },
          { label: '独立配置', value: 'independent' },
        ]"
        :disabled="disabled"
      />
      <template v-if="model">
        <div class="grid gap-4 sm:grid-cols-2">
          <BaseFormItem label="客户端预设">
            <BaseSelect v-model="selectedPreset" class="w-full" :options="presetOptions" :disabled="disabled" />
          </BaseFormItem>
          <BaseFormItem label="版本策略">
            <BaseSelect
              v-model="versionMode"
              class="w-full"
              :options="[
                { label: '跟随最新版本', value: 'latest', disabled: !currentPreset?.automaticAvailable },
                { label: '自定义版本', value: 'fixed' },
              ]"
              :disabled="disabled"
            />
          </BaseFormItem>
        </div>
        <p v-if="currentPreset?.reason" class="m-0 text-cp-sm text-cp-text-secondary">
          {{ currentPreset.reason }}
        </p>
        <div v-if="model.versionMode === 'fixed'" class="grid gap-4 sm:grid-cols-2">
          <BaseFormItem label="Codex Core 版本" required>
            <BaseInput :model-value="model.codexVersion ?? ''" :disabled="disabled" placeholder="例如 0.155.0" @update:model-value="updateField('codexVersion', $event)" />
          </BaseFormItem>
          <template v-if="model.client === 'desktop'">
            <BaseFormItem label="Desktop 版本" required>
              <BaseInput :model-value="model.desktopVersion ?? ''" :disabled="disabled" placeholder="填写该制品的应用版本" @update:model-value="updateField('desktopVersion', $event)" />
            </BaseFormItem>
            <BaseFormItem label="Desktop 构建号" required>
              <BaseInput :model-value="model.desktopBuild ?? ''" :disabled="disabled" placeholder="填写该制品的构建号" @update:model-value="updateField('desktopBuild', $event)" />
            </BaseFormItem>
          </template>
          <BaseFormItem v-for="field in customFields" :key="field.key" :label="field.label">
            <BaseInput
              :model-value="model[field.key] ?? ''"
              :placeholder="currentPreset?.defaults[field.key] ?? ''"
              :disabled="disabled"
              @update:model-value="updateField(field.key, $event)"
            />
          </BaseFormItem>
        </div>
      </template>
    </template>
    <ClientProfilePreviewPanel :preview="preview" :previewing="previewing" :needs-version-input="needsVersionInput" :error="previewError" />
  </div>
</template>
