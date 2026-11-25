<script setup lang="ts">
import type { ProfileOptions, ProfilePreview, ProfileSelection } from './profile'
import type { ProfileEditorProps } from '@/components/request-profile/useProfileEditor'
import { ZFormItem, ZInput, ZSelect } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import ProfileEditorFrame from '@/components/request-profile/ProfileEditorFrame.vue'
import { useProfileEditor } from '@/components/request-profile/useProfileEditor'

const props = defineProps<ProfileEditorProps<ProfileSelection>>()
const model = defineModel<ProfileSelection | null | undefined>({ required: true })
const { t } = useI18n({ useScope: 'global' })
const { effective, preview, source, frame, loading } = useProfileEditor<ProfileSelection, ProfileOptions, ProfilePreview>(
  'xai',
  model,
  props,
  selection => selection.versionMode === 'fixed' && !selection.clientVersion,
)
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

const versionMode = computed({
  get: () => effective.value?.versionMode ?? 'latest',
  set: (value: string) => {
    if (effective.value) {
      model.value = {
        ...effective.value,
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

function updateField(key: keyof ProfileSelection, value: string) {
  if (effective.value) {
    model.value = {
      ...effective.value,
      [key]: key === 'clientVersion'
        ? value || null
        : value,
    }
  }
}
</script>

<template>
  <ProfileEditorFrame v-model="source" v-bind="frame" :source-label="t('settings.clientProfile.xaiClientIdentitySource')">
    <template #source-extra>
      <slot name="source-extra" />
    </template>
    <div class="grid gap-4 sm:grid-cols-2">
      <ZFormItem :label="t('settings.clientProfile.clientType')">
        <ZInput model-value="Grok CLI" readonly :disabled="disabled || loading" />
      </ZFormItem>
      <ZFormItem :label="t('settings.clientProfile.versionStrategy')">
        <ZSelect v-model="versionMode" class="w-full" :disabled="disabled || loading" :options="[{ label: t('settings.clientProfile.followLatestVersion'), value: 'latest' }, { label: t('settings.clientProfile.customVersion'), value: 'fixed' }]" />
      </ZFormItem>
    </div>
    <div v-if="effective?.versionMode === 'fixed'" class="grid gap-4 sm:grid-cols-2">
      <ZFormItem :label="t('settings.clientProfile.grokCliVersion')" required>
        <ZInput :model-value="effective?.clientVersion ?? ''" :disabled="disabled || loading" :placeholder="t('settings.clientProfile.forExample')" @update:model-value="updateField('clientVersion', $event)" />
      </ZFormItem>
      <ZFormItem v-for="field in fields" :key="field.key" :label="field.label" required>
        <ZInput :model-value="effective?.[field.key]" :disabled="disabled || loading" @update:model-value="updateField(field.key, $event)" />
      </ZFormItem>
    </div>
  </ProfileEditorFrame>
</template>
