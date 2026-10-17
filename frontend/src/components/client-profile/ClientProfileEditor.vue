<script setup lang="ts">
import type {
  ClientProfileOptions,
  ClientProfilePreview,
  ClientProfileSelection,
} from '@/api/modules/settings/profiles'
import { ZButton, ZSegmented, ZSwitch } from '@codex-proxy/ui'
import { computed, onMounted, shallowRef, watch } from 'vue'
import { getClientProfileOptions, previewClientProfile } from '@/api/modules/settings/profiles'
import { errorMessage } from '@/utils/operation'
import ClientProfilePresetFields from './ClientProfilePresetFields.vue'
import ClientProfilePreviewPanel from './ClientProfilePreviewPanel.vue'

const props = withDefaults(defineProps<{ active?: boolean, disabled?: boolean, allowInherit?: boolean }>(), {
  active: true,
  disabled: false,
  allowInherit: false,
})
const model = defineModel<ClientProfileSelection | null>({ required: true })
const options = shallowRef<ClientProfileOptions>()
const preview = shallowRef<ClientProfilePreview>()
const loading = shallowRef(true)
const loadError = shallowRef('')
const previewError = shallowRef('')
const previewing = shallowRef(false)
const effective = computed(() => model.value ?? options.value?.globalConfiguration)
const inherited = computed(() => props.allowInherit && model.value === null)
const preset = computed(() => effective.value && !effective.value.mode ? effective.value : undefined)
const custom = computed(() => effective.value?.mode === 'custom' || preset.value?.versionMode === 'fixed')
const needsInput = computed(() => effective.value?.mode === 'custom'
  ? !effective.value.userAgent.trim()
  : preset.value?.versionMode === 'fixed'
    && (!preset.value.codexVersion || (preset.value.client === 'desktop' && (!preset.value.desktopVersion || !preset.value.desktopBuild))))
const profileSource = computed({
  get: () => model.value === null ? 'global' : 'independent',
  set: (value: string) => {
    if (value === 'global') {
      model.value = null
    }
    else if (options.value?.globalConfiguration) {
      model.value = { ...options.value.globalConfiguration, perKeyInstallationIdEnabled: false }
    }
  },
})
const perKeyInstallationIdEnabled = computed({
  get: () => model.value?.perKeyInstallationIdEnabled ?? false,
  set: (value: boolean) => {
    if (model.value)
      model.value = { ...model.value, perKeyInstallationIdEnabled: value }
  },
})
async function load() {
  loading.value = true
  loadError.value = ''
  try {
    options.value = await getClientProfileOptions()
  }
  catch (error) {
    loadError.value = errorMessage(error)
  }
  finally {
    loading.value = false
  }
}

watch([model, () => props.active], ([configuration, active], _, onCleanup) => {
  if (!active)
    return
  let cancelled = false
  preview.value = undefined
  previewError.value = ''
  previewing.value = !needsInput.value
  if (needsInput.value)
    return
  const timer = setTimeout(async () => {
    try {
      const result = await previewClientProfile(configuration)
      if (!cancelled)
        preview.value = result
    }
    catch (error) {
      if (!cancelled)
        previewError.value = errorMessage(error).replaceAll('User-Agent', '用户代理')
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

onMounted(() => load())
</script>

<template>
  <div class="grid min-w-0 gap-4">
    <div v-if="allowInherit" class="flex flex-wrap items-center justify-between gap-3">
      <ZSegmented
        v-model="profileSource"
        aria-label="客户端身份来源"
        class="shrink-0"
        :options="[
          { label: '全局配置', value: 'global' },
          { label: '独立配置', value: 'independent' },
        ]"
        :disabled="disabled || loading || !options"
      />
      <slot name="source-extra" />
    </div>
    <div v-if="loadError" role="alert" class="flex flex-wrap items-center justify-between gap-3 text-cp text-cp-error">
      <span>预设加载失败：{{ loadError }}</span>
      <ZButton size="small" :disabled="disabled || loading" @click="load()">
        重试
      </ZButton>
    </div>
    <ClientProfilePresetFields
      v-if="!inherited"
      :model-value="effective ?? null"
      :presets="options?.presets ?? []"
      :preview="preview"
      :previewing="previewing"
      :error="previewError"
      :disabled="disabled || loading"
      :max-version-lag="options?.maxVersionLag"
      :aria-busy="loading || undefined"
      @update:model-value="model = $event"
    />
    <div v-if="allowInherit && !inherited" class="grid gap-1.5">
      <ZSwitch
        v-model="perKeyInstallationIdEnabled"
        label="独立 installation id"
        class="justify-self-start"
        :disabled="disabled || loading"
      />
      <p class="m-0 text-cp-sm text-cp-text-secondary">
        为此 Key 使用的每个账号分别保存 installation id
      </p>
    </div>
    <ClientProfilePreviewPanel
      v-if="inherited || !custom"
      :preview="preview"
      :previewing="previewing"
      :needs-version-input="!!needsInput"
      :error="previewError"
    />
  </div>
</template>
