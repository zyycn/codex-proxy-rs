<script setup lang="ts">
import type {
  ClientProfileOptions,
  ClientProfilePreview,
  ClientProfileSelection,
} from '@/api/modules/settings/profiles'

import { ZButton, ZEmpty, ZSegmented } from '@codex-proxy/ui'
import { computed, onMounted, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { getClientProfileOptions, previewClientProfile } from '@/api/modules/settings/profiles'
import { errorMessage } from '@/utils/operation'
import ClientProfilePresetFields from './ClientProfilePresetFields.vue'
import ClientProfilePreviewPanel from './ClientProfilePreviewPanel.vue'

const props = withDefaults(defineProps<{ active?: boolean, disabled?: boolean, allowInherit?: boolean }>(), {
  active: true,
  disabled: false,
  allowInherit: false,
})

const { t } = useI18n({ useScope: 'global' })

const model = defineModel<ClientProfileSelection | null>({ required: true })
const options = shallowRef<ClientProfileOptions>()
const preview = shallowRef<ClientProfilePreview>()
const loading = shallowRef(true)
const loadError = shallowRef('')
const previewError = shallowRef('')
const previewing = shallowRef(false)
const independentDraft = shallowRef<ClientProfileSelection>()
const effective = computed(() => model.value ?? options.value?.globalConfiguration)
const inherited = computed(() => props.allowInherit && model.value === null)
const preset = computed(() => effective.value && !effective.value.mode
  ? effective.value
  : undefined)
const custom = computed(() => effective.value?.mode === 'custom' || preset.value?.versionMode === 'fixed')
const needsInput = computed(() => effective.value?.mode === 'custom'
  ? !effective.value.userAgent.trim()
  : preset.value?.versionMode === 'fixed'
    && (!preset.value.codexVersion || (preset.value.client === 'desktop' && (!preset.value.desktopVersion || !preset.value.desktopBuild))))
const profileSource = computed({
  get: () => model.value === null
    ? 'global'
    : 'independent',
  set: (value: string) => {
    if (value === 'global') {
      if (model.value)
        independentDraft.value = { ...model.value }
      model.value = null
    }
    else if (independentDraft.value ?? options.value?.globalConfiguration) {
      model.value = {
        ...(independentDraft.value ?? options.value!.globalConfiguration),
      }
    }
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

watch([
  model,
  () => props.active,
], ([configuration, active], _, onCleanup) => {
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
      const result = await previewClientProfile({ configuration })
      if (!cancelled)
        preview.value = result
    }
    catch (error) {
      if (!cancelled)
        previewError.value = errorMessage(error).replaceAll('User-Agent', t('settings.clientProfile.userAgent'))
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
        :aria-label="t('settings.clientProfile.clientIdentitySource')"
        class="shrink-0"
        :options="[
          { label: t('settings.clientProfile.globalConfiguration'), value: 'global' },
          { label: t('settings.clientProfile.customConfiguration'), value: 'independent' },
        ]"
        :disabled="disabled || loading || !options"
      />
      <slot name="source-extra" />
    </div>
    <ZEmpty v-if="loadError" :title="t('settings.clientProfile.presetLoadingFailed')" :description="loadError" size="small" surface="none" role="alert">
      <template #action>
        <ZButton size="small" :disabled="disabled || loading" @click="load()">
          {{ t('common.actions.retry') }}
        </ZButton>
      </template>
    </ZEmpty>
    <ClientProfilePresetFields
      v-if="!loadError && !inherited"
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
    <ClientProfilePreviewPanel
      v-if="!loadError && (inherited || !custom)"
      :preview="preview"
      :previewing="previewing"
      :needs-version-input="!!needsInput"
      :error="previewError"
    />
  </div>
</template>
