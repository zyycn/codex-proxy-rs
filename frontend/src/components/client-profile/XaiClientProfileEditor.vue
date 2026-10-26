<script setup lang="ts">
import type { XaiClientProfilePreview, XaiClientProfileSelection } from '@/api/modules/settings/profiles'

import { ZButton, ZEmpty, ZFormItem, ZInput, ZSegmented, ZSelect } from '@codex-proxy/ui'
import { computed, onMounted, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { getXaiClientProfileOptions, previewXaiClientProfile } from '@/api/modules/settings/profiles'
import { errorMessage } from '@/utils/operation'
import ClientProfilePreviewPanel from './ClientProfilePreviewPanel.vue'

const props = withDefaults(defineProps<{ active?: boolean, disabled?: boolean, allowInherit?: boolean }>(), {
  active: true,
  disabled: false,
  allowInherit: false,
})

const { t } = useI18n({ useScope: 'global' })

const model = defineModel<XaiClientProfileSelection | null>({ required: true })
const globalConfiguration = shallowRef<XaiClientProfileSelection>()
const preview = shallowRef<XaiClientProfilePreview>()
const loadError = shallowRef('')
const previewError = shallowRef('')
const previewing = shallowRef(false)
const needsVersionInput = computed(() => model.value?.versionMode === 'fixed' && !model.value.clientVersion)
const fields = computed(() => ([
  {
    key: 'clientIdentifier',
    label: t('settings.clientProfile.clientId'),
  },
  {
    key: 'clientMode',
    label: t('settings.clientProfile.operatingMode'),
  },
  {
    key: 'targetOs',
    label: t('settings.clientProfile.operatingSystem'),
  },
  {
    key: 'targetArch',
    label: t('settings.clientProfile.cpuArchitecture'),
  },
] as const))

const source = computed({
  get: () => model.value === null
    ? 'global'
    : 'independent',
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
        versionMode: value === 'fixed'
          ? 'fixed'
          : 'latest',
        clientVersion: value === 'fixed'
          ? preview.value?.clientVersion ?? null
          : null,
      }
    }
  },
})

function updateField(key: keyof XaiClientProfileSelection, value: string) {
  if (model.value) {
    model.value = {
      ...model.value,
      [key]: key === 'clientVersion'
        ? value || null
        : value,
    }
  }
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

watch([
  model,
  () => props.active,
], ([configuration, active], _, onCleanup) => {
  if (!active)
    return
  let cancelled = false
  preview.value = undefined
  previewError.value = ''
  previewing.value = !needsVersionInput.value
  if (needsVersionInput.value)
    return
  const timer = setTimeout(async () => {
    try {
      const result = await previewXaiClientProfile({ configuration })
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
    <div v-if="allowInherit" class="flex flex-wrap items-center justify-between gap-3">
      <ZSegmented
        v-model="source"
        :aria-label="t('settings.clientProfile.xaiClientIdentitySource')"
        class="shrink-0"
        :options="[{ label: t('settings.clientProfile.globalConfiguration'), value: 'global' }, { label: t('settings.clientProfile.customConfiguration'), value: 'independent' }]"
        :disabled="disabled || !globalConfiguration"
      />
      <slot name="source-extra" />
    </div>
    <ZEmpty v-if="loadError" :title="t('settings.clientProfile.loadFailed')" :description="loadError" size="small" surface="none" role="alert">
      <template #action>
        <ZButton size="small" @click="load">
          {{ t('common.actions.retry') }}
        </ZButton>
      </template>
    </ZEmpty>
    <template v-if="model">
      <div class="grid gap-4 sm:grid-cols-2">
        <ZFormItem :label="t('settings.clientProfile.clientType')">
          <ZInput model-value="Grok CLI" readonly :disabled="disabled" />
        </ZFormItem>
        <ZFormItem :label="t('settings.clientProfile.versionStrategy')">
          <ZSelect v-model="versionMode" class="w-full" :disabled="disabled" :options="[{ label: t('settings.clientProfile.followLatestVersion'), value: 'latest' }, { label: t('settings.clientProfile.customVersion'), value: 'fixed' }]" />
        </ZFormItem>
      </div>
      <div v-if="model.versionMode === 'fixed'" class="grid gap-4 sm:grid-cols-2">
        <ZFormItem :label="t('settings.clientProfile.grokCliVersion')" required>
          <ZInput :model-value="model.clientVersion ?? ''" :disabled="disabled" :placeholder="t('settings.clientProfile.forExample')" @update:model-value="updateField('clientVersion', $event)" />
        </ZFormItem>
        <ZFormItem v-for="field in fields" :key="field.key" :label="field.label" required>
          <ZInput :model-value="model[field.key]" :disabled="disabled" @update:model-value="updateField(field.key, $event)" />
        </ZFormItem>
      </div>
    </template>
    <ClientProfilePreviewPanel v-if="!loadError" :preview="preview" :previewing="previewing" :needs-version-input="needsVersionInput" :error="previewError" />
  </div>
</template>
