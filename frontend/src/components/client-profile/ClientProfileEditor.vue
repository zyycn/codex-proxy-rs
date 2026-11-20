<script setup lang="ts">
import type {
  ClientProfileOptions,
  ClientProfilePreview,
  ClientProfileSelection,
} from '@/api/modules/settings/profiles'

import { ZSegmented } from '@codex-proxy/ui'
import { computed, onMounted, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { getClientProfileOptions, previewClientProfile } from '@/api/modules/settings/profiles'
import { notification as ZNotification } from '@/utils/notification'
import { errorMessage } from '@/utils/operation'
import ClientProfilePresetFields from './ClientProfilePresetFields.vue'
import ClientProfilePreviewPanel from './ClientProfilePreviewPanel.vue'

const props = withDefaults(defineProps<{ active?: boolean, disabled?: boolean, allowInherit?: boolean, allowPreserve?: boolean }>(), {
  active: true,
  disabled: false,
  allowInherit: false,
  allowPreserve: false,
})

const { t } = useI18n({ useScope: 'global' })

const model = defineModel<ClientProfileSelection | null | undefined>({ required: true })
const options = shallowRef<ClientProfileOptions>()
const preview = shallowRef<ClientProfilePreview>()
const loading = shallowRef(true)
const loadError = shallowRef('')
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
  get: () => model.value === undefined
    ? 'keep'
    : model.value === null
      ? 'global'
      : 'independent',
  set: (value: string) => {
    if (value === 'keep') {
      model.value = undefined
      return
    }
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
  if (!active || configuration === undefined)
    return
  let cancelled = false
  preview.value = undefined
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
      if (!cancelled) {
        ZNotification.error({
          message: errorMessage(error).replaceAll('User-Agent', t('settings.clientProfile.userAgent')),
          deduplicate: true,
        })
      }
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
          ...(allowPreserve ? [{ label: t('accounts.modelAccess.reserve'), value: 'keep' }] : []),
          { label: t('settings.clientProfile.globalConfiguration'), value: 'global' },
          { label: t('settings.clientProfile.customConfiguration'), value: 'independent' },
        ]"
        :disabled="disabled || loading || !options"
      />
      <slot name="source-extra" />
    </div>
    <ClientProfilePresetFields
      v-if="model !== undefined && !loadError && !inherited"
      :model-value="effective ?? null"
      :presets="options?.presets ?? []"
      :preview="preview"
      :previewing="previewing"
      :disabled="disabled || loading"
      :max-version-lag="options?.maxVersionLag"
      :aria-busy="loading || undefined"
      @update:model-value="model = $event"
    />
    <ClientProfilePreviewPanel
      v-if="model !== undefined && !loadError && (inherited || !custom)"
      :preview="preview"
      :previewing="previewing"
      :needs-version-input="!!needsInput"
    />
  </div>
</template>
