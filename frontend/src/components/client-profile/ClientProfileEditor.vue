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
import { formatDateTime } from '@/utils/date'

withDefaults(defineProps<{ disabled?: boolean, allowInherit?: boolean }>(), {
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
const custom = shallowRef(false)
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
    if (preset) {
      model.value = { ...preset.configuration, versionMode: preset.automaticAvailable ? 'latest' : 'fixed' }
      custom.value = false
    }
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

watch(model, (configuration, _, onCleanup) => {
  let cancelled = false
  preview.value = undefined
  previewError.value = ''
  previewing.value = !needsVersionInput.value
  if (needsVersionInput.value)
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
        label="上游身份来源"
        class="justify-self-start"
        :options="[
          { label: '通用设置', value: 'global' },
          { label: '独立配置', value: 'independent' },
        ]"
        :disabled="disabled"
      />
      <template v-if="model">
        <div class="grid gap-4 sm:grid-cols-2">
          <BaseFormItem label="客户端预设">
            <BaseSelect v-model="selectedPreset" class="w-full" :options="presetOptions" :disabled="disabled" />
          </BaseFormItem>
          <BaseFormItem label="版本模式">
            <BaseSelect
              v-model="versionMode"
              class="w-full"
              :options="[
                { label: '自动最新', value: 'latest', disabled: !currentPreset?.automaticAvailable },
                { label: '固定版本', value: 'fixed' },
              ]"
              :disabled="disabled"
            />
          </BaseFormItem>
        </div>
        <p v-if="currentPreset?.reason" class="m-0 text-cp-sm text-cp-text-secondary">
          {{ currentPreset.reason }}
        </p>
        <div v-if="model.versionMode === 'fixed'" class="grid gap-4 sm:grid-cols-2">
          <BaseFormItem label="Core 版本" required>
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
        </div>
        <details :open="custom" @toggle="custom = ($event.target as HTMLDetailsElement).open">
          <summary class="cursor-pointer text-cp-sm text-cp-text-secondary">
            自定义身份参数
          </summary>
          <div class="mt-4 grid gap-4 sm:grid-cols-2">
            <BaseFormItem v-for="field in customFields" :key="field.key" :label="field.label">
              <BaseInput
                :model-value="model[field.key] ?? ''"
                :placeholder="currentPreset?.defaults[field.key] ?? ''"
                :disabled="disabled"
                @update:model-value="updateField(field.key, $event)"
              />
            </BaseFormItem>
          </div>
        </details>
      </template>
    </template>
    <div class="grid min-w-0 gap-2 rounded-cp bg-cp-fill-quaternary p-4" aria-live="polite">
      <p v-if="needsVersionInput" class="m-0 text-cp-sm text-cp-text-tertiary">
        填写版本后预览
      </p>
      <p v-else-if="previewing" class="m-0 text-cp-sm text-cp-text-tertiary">
        正在解析身份…
      </p>
      <p v-else-if="previewError" role="alert" class="m-0 text-cp-sm text-cp-error">
        {{ previewError }}
      </p>
      <template v-else-if="preview">
        <code class="break-all text-cp-sm text-cp-text">{{ preview.userAgent }}</code>
        <p class="m-0 text-cp-xs text-cp-text-tertiary">
          {{ preview.versionSource === 'custom' ? '固定版本' : '自动更新' }}
          <template v-if="preview.versionSource === 'official'">
            · {{ preview.checkedAt ? `检查于 ${formatDateTime(preview.checkedAt)}` : '待检查' }}
          </template>
        </p>
        <p v-if="preview.error && preview.versionSource === 'official'" :title="preview.error" class="m-0 text-cp-sm text-cp-warning">
          更新失败 · 沿用上次版本
        </p>
      </template>
    </div>
  </div>
</template>
